//! The interactive `signin-session` verb (M4c): one agent process runs one sign-in CLI
//! session, emits every job state change as a progress event, takes the authorization code
//! on `{"input":...}` controls and stops on `{"cancel":true}`. One-shot actions read and
//! settle the journal, so a second call can verify a cancel and a TypeScript restart can
//! still read the journaled job states.
//!
//! The install commit stays with the TypeScript front door until M5: on CLI exit 0 the
//! session reports `verifying` with `installPending`, the front door runs the unchanged
//! `complete` under its identity guards and registry lock, then settles the journal.

use crate::signin_session::jobs::{self, ErrorCode, Kind, SessionEvent, Spec, State, valid_job_id};
use crate::signin_session::journal::{Entry, Journal};
use crate::signin_session::process::{self, Command};
use aac_core::{Cause, Failure, UtcMs};
use aac_protocol::{Control, Event, PROTOCOL_VERSION, Request};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufRead, AsyncWrite, AsyncWriteExt};

/// The longest sign-in window: the CLI's own 15 minutes (`ANTIGRAVITY_SIGNIN_TIMEOUT_MS`).
pub const MAX_SESSION_TIMEOUT_MS: u64 = 900_000;

/// One parsed verb action.
pub enum Action {
    /// Run one interactive session.
    Start {
        /// The flow metadata and prepared command.
        spec: Box<Spec>,
        /// The CCS root whose journal holds the job.
        ccs: PathBuf,
        /// The front door's reserved id, when it mirrors the job synchronously.
        job_id: Option<String>,
        /// The front-door process that will install the login (its pid).
        front_door_pid: Option<u32>,
    },
    /// Read one journaled job.
    Get {
        /// The job id.
        job_id: String,
        /// The CCS root.
        ccs: PathBuf,
    },
    /// List the running jobs.
    List {
        /// The CCS root.
        ccs: PathBuf,
    },
    /// Cancel a running job from a second call and kill its CLI.
    Cancel {
        /// The job id.
        job_id: String,
        /// The CCS root.
        ccs: PathBuf,
    },
    /// Record the front door's install outcome.
    Settle {
        /// The job id.
        job_id: String,
        /// The CCS root.
        ccs: PathBuf,
        /// The terminal state.
        state: State,
        /// The installed identity, on success.
        result: Option<Value>,
        /// The classified error, on failure.
        error: Option<ErrorCode>,
    },
}

fn refused(reason: &'static str) -> Failure {
    Failure::new(Cause::Refused, reason)
}

/// ASCII printable text, the verbs' shared boundary before semantic parsing.
fn ascii_text(value: &Value, max: usize) -> Option<String> {
    let text = value.as_str()?;
    if text.is_empty() || text.len() > max {
        return None;
    }
    if !text.bytes().all(|b| (0x20..=0x7e).contains(&b)) {
        return None;
    }
    Some(text.to_string())
}

fn word(value: &Value, allowed: &[&str]) -> Option<String> {
    let text = ascii_text(value, 80)?;
    allowed.contains(&text.as_str()).then_some(text)
}

/// The providers the session vocabulary admits (the TS runner's plus Claude's terminal flow):
/// the provider table's `signInSessions`.
fn sign_in_provider(value: &Value) -> Option<String> {
    let text = ascii_text(value, 80)?;
    aac_providers::sign_in_session_providers()
        .any(|row| row.id.as_str() == text)
        .then_some(text)
}

fn optional_id(value: &Value) -> Result<Option<String>, Failure> {
    if value.is_null() {
        return Ok(None);
    }
    let text = ascii_text(value, 128).ok_or_else(|| refused("invalid sign-in identity"))?;
    if !text
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
    {
        return Err(refused("invalid sign-in identity"));
    }
    Ok(Some(text))
}

fn ccs_root(args: &Value) -> Result<PathBuf, Failure> {
    let text = ascii_text(&args["ccs"], 1024).ok_or_else(|| refused("invalid sign-in root"))?;
    let path = PathBuf::from(&text);
    if !path.is_absolute() {
        return Err(refused("invalid sign-in root"));
    }
    Ok(path)
}

fn parse_antigravity_driver(
    value: Option<&Value>,
) -> Result<Option<crate::signin_session::antigravity_driver::DriverSpec>, Failure> {
    let Some(value) = value.filter(|v| !v.is_null()) else {
        return Ok(None);
    };
    let object = value
        .as_object()
        .ok_or_else(|| refused("invalid sign-in driver"))?;
    if object.len() != 2
        || object
            .keys()
            .any(|key| !["tokenPath", "origin"].contains(&key.as_str()))
    {
        return Err(refused("invalid sign-in driver"));
    }
    let token_path = ascii_text(&value["tokenPath"], 1024)
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| refused("invalid sign-in driver"))?;
    let origin = ascii_text(&value["origin"], 256)
        .filter(|o| {
            o.strip_prefix("https://")
                .is_some_and(|h| !h.is_empty() && !h.contains('/'))
        })
        .ok_or_else(|| refused("invalid sign-in driver"))?;
    Ok(Some(
        crate::signin_session::antigravity_driver::DriverSpec { token_path, origin },
    ))
}

fn parse_command(value: &Value) -> Result<Command, Failure> {
    let object = value
        .as_object()
        .ok_or_else(|| refused("invalid sign-in command"))?;
    if object
        .keys()
        .any(|key| !["file", "args", "env", "pty", "antigravityDriver"].contains(&key.as_str()))
    {
        return Err(refused("invalid sign-in command"));
    }
    let file = ascii_text(&value["file"], 1024).ok_or_else(|| refused("invalid sign-in tool"))?;
    if !PathBuf::from(&file).is_absolute() {
        return Err(refused("invalid sign-in tool"));
    }
    let args = value["args"]
        .as_array()
        .ok_or_else(|| refused("invalid sign-in argv"))?;
    if args.len() > 64 {
        return Err(refused("invalid sign-in argv"));
    }
    let mut argv = Vec::new();
    for arg in args {
        argv.push(ascii_text(arg, 4096).ok_or_else(|| refused("invalid sign-in argv"))?);
    }
    let env = value["env"]
        .as_object()
        .ok_or_else(|| refused("invalid sign-in environment"))?;
    if env.len() > 64 {
        return Err(refused("invalid sign-in environment"));
    }
    let mut environment = BTreeMap::new();
    for (key, value) in env {
        let key = ascii_text(&Value::String(key.clone()), 128)
            .ok_or_else(|| refused("invalid sign-in environment"))?;
        let value =
            ascii_text(value, 8192).ok_or_else(|| refused("invalid sign-in environment"))?;
        environment.insert(key, value);
    }
    let pty = value["pty"]
        .as_bool()
        .ok_or_else(|| refused("invalid sign-in pty"))?;
    let antigravity_driver = parse_antigravity_driver(object.get("antigravityDriver"))?;
    Ok(Command {
        file,
        args: argv,
        env: environment,
        pty,
        antigravity_driver,
    })
}

fn parse_start(args: &Value) -> Result<Action, Failure> {
    let provider =
        sign_in_provider(&args["provider"]).ok_or_else(|| refused("invalid sign-in provider"))?;
    let kind = match word(&args["kind"], &["device-code", "supervised-cli"])
        .ok_or_else(|| refused("invalid sign-in kind"))?
        .as_str()
    {
        "device-code" => Kind::DeviceCode,
        _ => Kind::SupervisedCli,
    };
    let mode = word(&args["mode"], &["add", "signin-again"])
        .ok_or_else(|| refused("invalid sign-in mode"))?;
    let platform = word(&args["platform"], &["ubuntu", "mac"])
        .ok_or_else(|| refused("invalid sign-in platform"))?;
    let account_id = optional_id(&args["accountId"])?;
    let profile_name = optional_id(&args["profileName"])?;
    let origins = args["allowedOrigins"]
        .as_array()
        .ok_or_else(|| refused("invalid sign-in origins"))?;
    if origins.is_empty() || origins.len() > 8 {
        return Err(refused("invalid sign-in origins"));
    }
    let mut allowed_origins = Vec::new();
    for origin in origins {
        let text = ascii_text(origin, 256).ok_or_else(|| refused("invalid sign-in origins"))?;
        if !text.starts_with("https://") || text[8..].contains('/') {
            return Err(refused("invalid sign-in origins"));
        }
        allowed_origins.push(text);
    }
    let timeout_ms = args["timeoutMs"]
        .as_u64()
        .filter(|ms| (100..=MAX_SESSION_TIMEOUT_MS).contains(ms))
        .ok_or_else(|| refused("invalid sign-in timeout"))?;
    let first_output_timeout_ms = match &args["firstOutputTimeoutMs"] {
        Value::Null => jobs::FIRST_OUTPUT_TIMEOUT_MS,
        value => value
            .as_u64()
            .filter(|ms| (100..=300_000).contains(ms))
            .ok_or_else(|| refused("invalid sign-in timeout"))?,
    };
    let command = parse_command(&args["command"])?;
    let job_id = match &args["jobId"] {
        Value::Null => None,
        value => Some(
            ascii_text(value, 20)
                .filter(|id| valid_job_id(id))
                .ok_or_else(|| refused("invalid sign-in job id"))?,
        ),
    };
    let front_door_pid = match &args["frontDoorPid"] {
        Value::Null => None,
        value => Some(
            value
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok())
                .filter(|pid| *pid > 0)
                .ok_or_else(|| refused("invalid sign-in front door"))?,
        ),
    };
    Ok(Action::Start {
        spec: Box::new(Spec {
            provider,
            kind,
            mode,
            account_id,
            profile_name,
            platform,
            allowed_origins,
            timeout_ms,
            first_output_timeout_ms,
            command,
        }),
        ccs: ccs_root(args)?,
        job_id,
        front_door_pid,
    })
}

fn parse_state(value: &Value) -> Option<State> {
    match value.as_str()? {
        "succeeded" => Some(State::Succeeded),
        "failed" => Some(State::Failed),
        "expired" => Some(State::Expired),
        "cancelled" => Some(State::Cancelled),
        _ => None,
    }
}

fn parse_error(value: &Value) -> Result<Option<ErrorCode>, Failure> {
    if value.is_null() {
        return Ok(None);
    }
    value
        .as_str()
        .and_then(ErrorCode::from_word)
        .map(Some)
        .ok_or_else(|| refused("invalid sign-in error"))
}

fn parse_result(value: &Value) -> Result<Option<Value>, Failure> {
    if value.is_null() {
        return Ok(None);
    }
    let object = value
        .as_object()
        .ok_or_else(|| refused("invalid sign-in result"))?;
    if object
        .keys()
        .any(|key| !["accountId", "email", "plan"].contains(&key.as_str()))
    {
        return Err(refused("invalid sign-in result"));
    }
    let account_id =
        ascii_text(&value["accountId"], 128).ok_or_else(|| refused("invalid sign-in result"))?;
    let email = optional_id(&value["email"])?;
    let plan = optional_id(&value["plan"])?;
    Ok(Some(
        json!({"accountId": account_id, "email": email, "plan": plan}),
    ))
}

/// Parse and validate the verb arguments.
pub fn parse(args: &Value) -> Result<Action, Failure> {
    let object = args
        .as_object()
        .ok_or_else(|| refused("invalid sign-in args"))?;
    let action = word(
        &args["action"],
        &["start", "get", "list", "cancel", "settle"],
    )
    .ok_or_else(|| refused("invalid sign-in action"))?;
    let known = [
        "action",
        "ccs",
        "provider",
        "kind",
        "mode",
        "accountId",
        "profileName",
        "platform",
        "allowedOrigins",
        "timeoutMs",
        "firstOutputTimeoutMs",
        "command",
        "jobId",
        "frontDoorPid",
        "state",
        "result",
        "error",
    ];
    if object.keys().any(|key| !known.contains(&key.as_str())) {
        return Err(refused("invalid sign-in args"));
    }
    match action.as_str() {
        "start" => parse_start(args),
        "get" | "cancel" => {
            let job_id = ascii_text(&args["jobId"], 20)
                .filter(|id| valid_job_id(id))
                .ok_or_else(|| refused("invalid sign-in job id"))?;
            let ccs = ccs_root(args)?;
            if action == "get" {
                Ok(Action::Get { job_id, ccs })
            } else {
                Ok(Action::Cancel { job_id, ccs })
            }
        }
        "list" => Ok(Action::List {
            ccs: ccs_root(args)?,
        }),
        _ => {
            let job_id = ascii_text(&args["jobId"], 20)
                .filter(|id| valid_job_id(id))
                .ok_or_else(|| refused("invalid sign-in job id"))?;
            let state =
                parse_state(&args["state"]).ok_or_else(|| refused("invalid sign-in state"))?;
            Ok(Action::Settle {
                job_id,
                ccs: ccs_root(args)?,
                state,
                result: parse_result(&args["result"])?,
                error: parse_error(&args["error"])?,
            })
        }
    }
}

/// A fresh job id: `job_` plus 16 lowercase hex characters (`randomBytes(8)`).
fn new_job_id() -> String {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};
    let now = UtcMs::now().map(|t| t.get()).unwrap_or(0);
    let pid = std::process::id();
    let mut bytes = Vec::with_capacity(16);
    for seed in 0..2u64 {
        let state = RandomState::new();
        let mut hasher = state.build_hasher();
        hasher.write_u64(now);
        hasher.write_u64(u64::from(pid));
        hasher.write_u64(seed);
        bytes.extend_from_slice(&hasher.finish().to_le_bytes());
    }
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    format!("job_{hex}")
}

fn now_ms() -> i64 {
    UtcMs::now().map(|t| t.get() as i64).unwrap_or(0)
}

/// The one-shot actions: journal reads, the second call's cancel, and the install settle.
pub fn one_shot(action: Action) -> Result<Value, Failure> {
    match action {
        Action::Get { job_id, ccs } => {
            let journal = Journal::open(&ccs)?;
            Ok(journal.get(&job_id, now_ms()))
        }
        Action::List { ccs } => {
            let journal = Journal::open(&ccs)?;
            Ok(journal.list(now_ms()))
        }
        Action::Cancel { job_id, ccs } => {
            let journal = Journal::open(&ccs)?;
            cancel(&journal, &job_id)
        }
        Action::Settle {
            job_id,
            ccs,
            state,
            result,
            error,
        } => {
            let journal = Journal::open(&ccs)?;
            journal.settle(&job_id, state, result, error, now_ms())
        }
        Action::Start { .. } => Err(refused("start is interactive")),
    }
}

/// The second call's cancel: flag the journal, kill the CLI's group, verify it is gone.
fn cancel(journal: &Journal, job_id: &str) -> Result<Value, Failure> {
    let Some(entry) = journal.request_cancel(job_id)? else {
        return Ok(json!({"cancelled": false, "job": Value::Null}));
    };
    if entry.finished() {
        return Ok(json!({"cancelled": false, "job": entry.job}));
    }
    #[cfg(unix)]
    {
        kill_and_verify(journal, &entry);
    }
    Ok(json!({"cancelled": true, "job": entry.job}))
}

/// SIGTERM to the relay group, SIGKILL after the relay's own grace, then verify the CLI.
#[cfg(unix)]
fn kill_and_verify(journal: &Journal, entry: &Entry) {
    use crate::signin_session::process::{group_alive, process_alive, signal_group};
    // The owner journals the child pids on its first flush; wait briefly for them.
    let mut current = entry.clone();
    let wait_until = std::time::Instant::now() + std::time::Duration::from_millis(1_000);
    while current.relay_pid.is_none() && std::time::Instant::now() < wait_until {
        std::thread::sleep(std::time::Duration::from_millis(50));
        if let Some(fresh) = entry.job["id"].as_str().and_then(|id| journal.read(id)) {
            current = fresh;
        }
    }
    let Some(relay) = current.relay_pid else {
        return;
    };
    // Discover the CLI's pid before the relay dies (Linux lists its children).
    let mut cli_pid = current.cli_pid;
    if cli_pid.is_none() {
        cli_pid = linux_children(relay).into_iter().next();
    }
    if group_alive(relay) {
        signal_group(relay, nix::sys::signal::Signal::SIGTERM);
        // The relay SIGKILLs the CLI one second after its SIGTERM; wait past that.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1_500);
        while group_alive(relay) && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if group_alive(relay) {
            signal_group(relay, nix::sys::signal::Signal::SIGKILL);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1_000);
            while group_alive(relay) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    if let Some(pid) = cli_pid {
        if process_alive(pid) {
            signal_group(pid, nix::sys::signal::Signal::SIGKILL);
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
            while process_alive(pid) && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        // Record the verified pid so a later get can name the removed child.
        let mut updated = current.clone();
        updated.cli_pid = Some(pid);
        let _ = journal.record(&updated);
    }
}

/// The relay's child pids on Linux (`/proc/<pid>/task/<pid>/children`), best effort.
#[cfg(target_os = "linux")]
fn linux_children(pid: u32) -> Vec<u32> {
    let path = format!("/proc/{pid}/task/{pid}/children");
    let Ok(text) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    text.split_whitespace()
        .filter_map(|word| word.parse::<u32>().ok())
        .collect()
}

/// No child listing on this platform; the relay's group kill still covers the CLI.
#[cfg(not(target_os = "linux"))]
fn linux_children(_pid: u32) -> Vec<u32> {
    Vec::new()
}

/// Run one interactive session, journaling every state change and forwarding each job
/// snapshot on `progress`. Returns the terminal value and whether a cancel/transport stop
/// ended it (the protocol then emits `cancelled`).
pub async fn run_session(
    spec: Spec,
    ccs: PathBuf,
    job_id: Option<String>,
    front_door_pid: Option<u32>,
    controls: &mut tokio::sync::mpsc::UnboundedReceiver<Control>,
    progress: tokio::sync::mpsc::UnboundedSender<Value>,
) -> Result<(Value, bool), Failure> {
    let journal = Journal::open(&ccs)?;
    let now = now_ms();
    journal.recover(now);
    journal.prune(now);

    // The runner's limits: one running job per provider, three in all.
    let running = journal.running(now);
    if let Some(same) = running
        .iter()
        .find(|entry| entry.job["provider"].as_str() == Some(spec.provider.as_str()))
    {
        return Ok((
            json!({"conflict": {"code": "job_running", "jobId": same.job["id"]}}),
            false,
        ));
    }
    if running.len() >= 3 {
        return Ok((
            json!({"conflict": {"code": "too_many_jobs", "jobId": Value::Null}}),
            false,
        ));
    }

    // The CLI session is the only process tree this verb owns; reap orphaned grandchildren.
    #[cfg(target_os = "linux")]
    let _ = nix::sys::prctl::set_child_subreaper(true);

    let id = job_id.unwrap_or_else(new_job_id);
    journal.issue(&id);
    let pid_slot: Arc<Mutex<Option<u32>>> = Arc::new(Mutex::new(None));
    let spawner: jobs::Spawner = {
        let slot = pid_slot.clone();
        Arc::new(move |command: &Command| {
            let handle = process::spawn(command);
            if let Ok(mut slot) = slot.lock() {
                *slot = handle.pid();
            }
            handle
        })
    };
    let stop_journal = Journal::open(&ccs)?;
    let stop_id = id.clone();
    let external_stop: jobs::StopRequest = Arc::new(move || {
        let entry = stop_journal.read(&stop_id)?;
        if entry.cancel_requested && !entry.finished() {
            Some((State::Cancelled, None))
        } else {
            None
        }
    });

    let mut entry = Entry {
        job: json!({
            "id": id,
            "provider": spec.provider,
            "kind": spec.kind.word(),
            "mode": spec.mode,
            "accountId": spec.account_id,
            "profileName": spec.profile_name,
            "platform": spec.platform,
            "state": "starting",
            "verification": Value::Null,
            "result": Value::Null,
            "error": Value::Null,
            "startedAt": jobs::iso(now),
            "updatedAt": jobs::iso(now),
            "expiresAt": jobs::iso(now + spec.timeout_ms as i64),
        }),
        relay_pid: None,
        cli_pid: None,
        owner_pid: std::process::id(),
        front_door_pid,
        install_pending: false,
        cancel_requested: false,
        finished_at: None,
    };
    let _ = journal.record(&entry);

    let pty = spec.command.pty;
    let (handle, mut events) = jobs::start(
        id.clone(),
        spec,
        jobs::system_clock(),
        spawner,
        Some(external_stop),
    );
    let _ = journal.record(&entry);

    let mut last_job = entry.job.clone();
    let mut stopped = false;
    let mut terminal: Option<Value> = None;
    loop {
        tokio::select! {
            control = controls.recv() => {
                match control {
                    Some(Control::Cancel { cancel: true }) => {
                        handle.request_stop(State::Cancelled, None);
                    }
                    Some(Control::Input { input }) => {
                        handle.submit_code(&input);
                    }
                    Some(Control::Cancel { cancel: false }) => {}
                    None => {
                        // The front door's stdin closed: shutdown, nothing resumes.
                        handle.request_stop(State::Failed, Some(ErrorCode::ServerRestarted));
                    }
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                // The spawner runs on the session task: pick up the child pids here and
                // journal them, so a second call or recovery can name and kill the CLI.
                let mut changed = false;
                if entry.relay_pid.is_none()
                    && let Ok(slot) = pid_slot.lock()
                    && slot.is_some()
                {
                    entry.relay_pid = *slot;
                    changed = true;
                }
                if entry.cli_pid.is_none() && entry.relay_pid.is_some() {
                    // A plain-pipe child is the CLI itself; the PTY relay's child is the
                    // CLI, listed by the kernel (Linux; elsewhere the group kill covers it).
                    let cli = if pty {
                        entry.relay_pid.and_then(|relay| linux_children(relay).into_iter().next())
                    } else {
                        entry.relay_pid
                    };
                    if cli.is_some() {
                        entry.cli_pid = cli;
                        changed = true;
                    }
                }
                if changed {
                    let _ = journal.record(&entry);
                }
            }
            event = events.recv() => {
                let Some(event) = event else { break };
                match event {
                    SessionEvent::Job(job) => {
                        entry.job = job.clone();
                        last_job = job.clone();
                        let _ = journal.record(&entry);
                        let _ = progress.send(job);
                    }
                    SessionEvent::Result(value) => {
                        let install_pending = value["installPending"].as_bool().unwrap_or(false);
                        entry.install_pending = install_pending;
                        if !install_pending {
                            entry.finished_at = Some(now_ms());
                        }
                        entry.job = value["job"].clone();
                        last_job = value["job"].clone();
                        let _ = journal.record(&entry);
                        terminal = Some(value);
                        break;
                    }
                    SessionEvent::Stopped => {
                        stopped = true;
                        entry.finished_at = Some(now_ms());
                        entry.job = last_job.clone();
                        let _ = journal.record(&entry);
                        break;
                    }
                }
            }
        }
    }
    // The session task owns the CLI handle; dropping the control side ends any leftover work.
    drop(handle);
    match terminal {
        Some(value) => Ok((value, stopped)),
        None => Ok((json!({"job": last_job}), true)),
    }
}

/// Write one NDJSON event with the protocol's cap.
async fn write_event(
    writer: &mut (impl AsyncWrite + Unpin),
    event: &Event,
    cap: usize,
) -> Result<(), Failure> {
    let mut bytes =
        serde_json::to_vec(event).map_err(|_| Failure::new(Cause::Bug, "event encoding failed"))?;
    if bytes.len() >= cap {
        return Err(Failure::new(Cause::OutputTooLarge, "event cap exceeded"));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// The interactive entry point: one request, progress events, controls, one terminal event.
pub async fn interactive(
    request: Request,
    reader: &mut (impl AsyncBufRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<i32, Failure> {
    let cap = aac_protocol::frame_cap(&request.verb);
    let action = match parse(&request.args) {
        Ok(action) => action,
        Err(failure) => {
            let event = Event::Failure {
                protocol: PROTOCOL_VERSION,
                id: request.id.clone(),
                failure,
            };
            write_event(writer, &event, cap).await?;
            return Ok(1);
        }
    };
    if !matches!(action, Action::Start { .. }) {
        let work = crate::blocking(move || one_shot(action)).await;
        let (event, code) = match work {
            Ok(value) => (
                Event::Result {
                    protocol: PROTOCOL_VERSION,
                    id: request.id.clone(),
                    value,
                },
                0,
            ),
            Err(failure) => (
                Event::Failure {
                    protocol: PROTOCOL_VERSION,
                    id: request.id.clone(),
                    failure,
                },
                1,
            ),
        };
        write_event(writer, &event, cap).await?;
        return Ok(code);
    }
    let Action::Start {
        spec,
        ccs,
        job_id,
        front_door_pid,
    } = action
    else {
        return Err(refused("start is interactive"));
    };

    let (control_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let mut control_tx = Some(control_tx);
    let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
    let session = run_session(
        *spec,
        ccs,
        job_id,
        front_door_pid,
        &mut controls,
        progress_tx,
    );
    tokio::pin!(session);
    let deadline = tokio::time::sleep(std::time::Duration::from_millis(request.deadline_ms));
    tokio::pin!(deadline);
    let mut eof = false;
    let mut outcome: Result<(Value, bool), Failure> =
        Err(Failure::new(Cause::Bug, "session ended without a result"));
    let mut deadline_hit = false;

    loop {
        tokio::select! {
            biased;
            result = &mut session => {
                outcome = result;
                break;
            }
            _ = &mut deadline => {
                deadline_hit = true;
                break;
            }
            job = progress_rx.recv() => {
                if let Some(job) = job {
                    let event = Event::Progress {
                        protocol: PROTOCOL_VERSION,
                        id: request.id.clone(),
                        data: json!({"job": job}),
                    };
                    write_event(writer, &event, cap).await?;
                }
            }
            line = crate::read_line(reader, aac_protocol::MAX_FRAME_BYTES), if !eof => {
                match line {
                    Ok(Some(bytes)) => match serde_json::from_slice::<Control>(&bytes) {
                        Ok(control) => {
                            if let Some(tx) = &control_tx {
                                let _ = tx.send(control);
                            }
                        }
                        Err(_) => {
                            let event = Event::Failure {
                                protocol: PROTOCOL_VERSION,
                                id: request.id.clone(),
                                failure: refused("invalid agent control"),
                            };
                            write_event(writer, &event, cap).await?;
                            return Ok(1);
                        }
                    },
                    Ok(None) => {
                        // EOF: closing the control side is the shutdown signal.
                        eof = true;
                        control_tx = None;
                    }
                    Err(failure) => {
                        let event = Event::Failure {
                            protocol: PROTOCOL_VERSION,
                            id: request.id.clone(),
                            failure,
                        };
                        write_event(writer, &event, cap).await?;
                        return Ok(1);
                    }
                }
            }
        }
    }

    // Completion can win select while earlier progress is still queued; drain it in order
    // before the terminal frame, the way the service's own agent transport does.
    while let Ok(job) = progress_rx.try_recv() {
        let event = Event::Progress {
            protocol: PROTOCOL_VERSION,
            id: request.id.clone(),
            data: json!({"job": job}),
        };
        write_event(writer, &event, cap).await?;
    }

    if deadline_hit {
        // The transport budget passed: stop the session, journal the stop, report timeout.
        // The adapter sizes the deadline past the job's own timeout, so this is the backstop.
        drop(control_tx);
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), &mut session).await;
        let event = Event::Failure {
            protocol: PROTOCOL_VERSION,
            id: request.id.clone(),
            failure: Failure::new(Cause::Timeout, "agent deadline exceeded"),
        };
        write_event(writer, &event, cap).await?;
        return Ok(1);
    }

    match outcome {
        Ok((value, stopped)) => {
            if stopped {
                // The final job was the last progress event; the terminal frame is the
                // cancelled failure the transport acknowledges.
                let event = Event::Failure {
                    protocol: PROTOCOL_VERSION,
                    id: request.id.clone(),
                    failure: Failure::new(Cause::Cancelled, "agent request cancelled"),
                };
                write_event(writer, &event, cap).await?;
                Ok(1)
            } else {
                let event = Event::Result {
                    protocol: PROTOCOL_VERSION,
                    id: request.id.clone(),
                    value,
                };
                write_event(writer, &event, cap).await?;
                Ok(0)
            }
        }
        Err(failure) => {
            let event = Event::Failure {
                protocol: PROTOCOL_VERSION,
                id: request.id.clone(),
                failure,
            };
            write_event(writer, &event, cap).await?;
            Ok(1)
        }
    }
}
