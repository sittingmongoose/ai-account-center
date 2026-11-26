//! The sign-in session state machine, ported from `signin-jobs.ts` (CONTRACT-registry-lifecycle
//! 6.6). One session per agent process; the multi-job map, the limits and the install commit
//! stay with the TypeScript runner until M5.
//!
//! The port covers every transition up to the install boundary: starting, waiting,
//! awaiting_code, verifying, and the terminal failed/expired/cancelled states. Once the CLI
//! exits 0 the session reports `verifying` with `installPending`: the front door runs the
//! unchanged `complete` (identity guards, registry lock) and settles the journal.
//!
//! Output is parsed in memory and discarded; it is never logged or persisted. A first-output
//! timeout (30 s) or an unrecognized answer fails the job with `unexpected_output`; the overall
//! timeout ends it as `expired`. Cancel, timeout and shutdown kill the CLI's whole process group.

use crate::signin_session::parser::{self, Parser, State as ParserState};
use crate::signin_session::process::{Command, Handle, ProcessEvent, SpawnFailure};
use serde_json::{Value, json};
use std::sync::Arc;
use tokio::sync::mpsc;

/// The first-output budget (`firstOutputTimeoutMs` default).
pub const FIRST_OUTPUT_TIMEOUT_MS: u64 = 30_000;
/// How often the session checks the front door's external stop request.
const STOP_POLL_MS: u64 = 200;

/// The flow kind (`SignInJobKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Device-code: the URL and the user code, no input.
    DeviceCode,
    /// Supervised CLI: the URL, then one authorization code on input.
    SupervisedCli,
}

impl Kind {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Kind::DeviceCode => "device-code",
            Kind::SupervisedCli => "supervised-cli",
        }
    }
}

/// The job states (`SignInJobState`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    /// Reserved, the CLI is starting.
    Starting,
    /// Device-code: the verification facts are shown.
    Waiting,
    /// Supervised: the URL is shown, the code is expected.
    AwaitingCode,
    /// The code was submitted or the CLI exited 0; the login is being installed.
    Verifying,
    /// Installed (settled by the front door).
    Succeeded,
    /// A classified failure.
    Failed,
    /// The overall timeout passed.
    Expired,
    /// A cancel stopped it.
    Cancelled,
}

impl State {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            State::Starting => "starting",
            State::Waiting => "waiting",
            State::AwaitingCode => "awaiting_code",
            State::Verifying => "verifying",
            State::Succeeded => "succeeded",
            State::Failed => "failed",
            State::Expired => "expired",
            State::Cancelled => "cancelled",
        }
    }

    /// Whether the state is terminal (`FINISHED`).
    pub fn finished(self) -> bool {
        matches!(
            self,
            State::Succeeded | State::Failed | State::Expired | State::Cancelled
        )
    }
}

pub use aac_core::signin::{ErrorCode, valid_code, valid_job_id};

/// One session's spec: the flow metadata plus the prepared command (`SignInFlowSpec` minus
/// the callbacks, which stay in TypeScript).
#[derive(Clone)]
pub struct Spec {
    /// The provider word.
    pub provider: String,
    /// Device-code or supervised.
    pub kind: Kind,
    /// Add or sign in again.
    pub mode: String,
    /// The bound account, when signing in again.
    pub account_id: Option<String>,
    /// The profile name.
    pub profile_name: Option<String>,
    /// The computer platform word.
    pub platform: String,
    /// The parser's origin allowlist.
    pub allowed_origins: Vec<String>,
    /// The overall budget.
    pub timeout_ms: u64,
    /// The first-output budget (the runner default unless the caller overrides it).
    pub first_output_timeout_ms: u64,
    /// The prepared CLI command.
    pub command: Command,
}

/// What the session emits: every job snapshot, then one end.
#[derive(Clone, Debug)]
pub enum SessionEvent {
    /// A state change (`onChange`): the complete job DTO.
    Job(Value),
    /// The session ended normally: the final job and whether the install is pending.
    Result(Value),
    /// A cancel or transport stop ended it; the final job was the last [`SessionEvent::Job`].
    Stopped,
}

/// An external stop (the journal's cancel flag, read by the front door's wrapper).
pub type StopRequest = Arc<dyn Fn() -> Option<(State, Option<ErrorCode>)> + Send + Sync>;

/// The session's control side.
#[derive(Clone)]
pub struct SessionHandle {
    input: mpsc::UnboundedSender<String>,
    stop: mpsc::UnboundedSender<(State, Option<ErrorCode>)>,
}

impl SessionHandle {
    /// Submit the authorization code (supervised flows only).
    pub fn submit_code(&self, code: &str) {
        let _ = self.input.send(code.to_string());
    }

    /// Ask the session to end: cancel, or the transport's shutdown.
    pub fn request_stop(&self, state: State, code: Option<ErrorCode>) {
        let _ = self.stop.send((state, code));
    }
}

/// The job DTO under construction; field order is the reference's literal order.
struct Job {
    id: String,
    provider: String,
    kind: Kind,
    mode: String,
    account_id: Option<String>,
    profile_name: Option<String>,
    platform: String,
    state: State,
    verification: Option<Value>,
    error: Option<Value>,
    started_at: String,
    updated_at: String,
    expires_at: String,
}

impl Job {
    fn value(&self) -> Value {
        json!({
            "id": self.id,
            "provider": self.provider,
            "kind": self.kind.word(),
            "mode": self.mode,
            "accountId": self.account_id,
            "profileName": self.profile_name,
            "platform": self.platform,
            "state": self.state.word(),
            "verification": self.verification,
            "result": Value::Null,
            "error": self.error,
            "startedAt": self.started_at,
            "updatedAt": self.updated_at,
            "expiresAt": self.expires_at,
        })
    }
}

/// The clock the session reads (injectable for deterministic tests).
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// The spawner the session uses (injectable for fake-process tests).
pub type Spawner = Arc<dyn Fn(&Command) -> Handle + Send + Sync>;

/// The live clock: UTC milliseconds.
pub fn system_clock() -> Clock {
    Arc::new(|| aac_core::UtcMs::now().map(|t| t.get() as i64).unwrap_or(0))
}

/// Start one session. The returned receiver carries every job snapshot and the end;
/// dropping it stops the session (and kills the CLI through the handle's drop guard).
pub fn start(
    id: String,
    spec: Spec,
    clock: Clock,
    spawner: Spawner,
    external_stop: Option<StopRequest>,
) -> (SessionHandle, mpsc::UnboundedReceiver<SessionEvent>) {
    let (event_tx, events) = mpsc::unbounded_channel();
    let (input_tx, input_rx) = mpsc::unbounded_channel();
    let (stop_tx, stop_rx) = mpsc::unbounded_channel();
    let handle = SessionHandle {
        input: input_tx,
        stop: stop_tx,
    };
    tokio::spawn(run(
        id,
        spec,
        clock,
        spawner,
        external_stop,
        event_tx,
        input_rx,
        stop_rx,
    ));
    (handle, events)
}

#[allow(clippy::too_many_arguments)]
async fn run(
    id: String,
    spec: Spec,
    clock: Clock,
    spawner: Spawner,
    external_stop: Option<StopRequest>,
    events: mpsc::UnboundedSender<SessionEvent>,
    mut input: mpsc::UnboundedReceiver<String>,
    mut stop: mpsc::UnboundedReceiver<(State, Option<ErrorCode>)>,
) {
    let now = clock();
    let started_at = iso(now);
    let expires_at = iso(now + spec.timeout_ms as i64);
    let mut job = Job {
        id,
        provider: spec.provider.clone(),
        kind: spec.kind,
        mode: spec.mode.clone(),
        account_id: spec.account_id.clone(),
        profile_name: spec.profile_name.clone(),
        platform: spec.platform.clone(),
        state: State::Starting,
        verification: None,
        error: None,
        started_at: started_at.clone(),
        updated_at: started_at,
        expires_at: expires_at.clone(),
    };
    let mut parser = Parser::new(parser::Options {
        allowed_origins: spec.allowed_origins.clone(),
        expects_user_code: spec.kind == Kind::DeviceCode,
        keep_query: spec.kind == Kind::SupervisedCli,
    });
    let mut process_handle = Some(spawner(&spec.command));
    let mut code_sent = false;
    let mut stopped = false;

    let mut overall = Box::pin(tokio::time::sleep(std::time::Duration::from_millis(
        spec.timeout_ms,
    )));
    let mut first_output = Box::pin(tokio::time::sleep(std::time::Duration::from_millis(
        spec.first_output_timeout_ms,
    )));
    let mut first_output_fired = false;
    let mut poll = tokio::time::interval(std::time::Duration::from_millis(STOP_POLL_MS));
    poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let emit = |job: &Job, events: &mpsc::UnboundedSender<SessionEvent>| {
        let _ = events.send(SessionEvent::Job(job.value()));
    };
    emit(&job, &events);

    macro_rules! finish {
        ($state:expr, $code:expr) => {{
            if let Some(handle) = process_handle.as_ref() {
                handle.kill();
            }
            job.verification = None;
            job.error = $code.map(|code: ErrorCode| {
                json!({"code": code.word(), "message": code.message()})
            });
            job.state = $state;
            job.updated_at = iso(clock());
            emit(&job, &events);
            let value = json!({"job": job.value(), "installPending": false});
            let _ = events.send(if stopped {
                SessionEvent::Stopped
            } else {
                SessionEvent::Result(value)
            });
            return;
        }};
    }

    loop {
        tokio::select! {
            _ = &mut overall => {
                finish!(State::Expired, Some(ErrorCode::Timeout));
            }
            _ = &mut first_output, if !first_output_fired => {
                first_output_fired = true;
                if job.state == State::Starting {
                    finish!(State::Failed, Some(ErrorCode::UnexpectedOutput));
                }
            }
            _ = poll.tick() => {
                if let Some(request) = external_stop.as_ref().and_then(|f| f()) {
                    stopped = request.0 == State::Cancelled;
                    finish!(request.0, request.1);
                }
            }
            request = stop.recv() => {
                match request {
                    Some((state, code)) => {
                        stopped = state == State::Cancelled;
                        finish!(state, code);
                    }
                    None => {
                        // The control side dropped: the front door is gone.
                        stopped = true;
                        finish!(State::Failed, Some(ErrorCode::ServerRestarted));
                    }
                }
            }
            code = input.recv() => {
                if let Some(code) = code {
                    if spec.kind != Kind::SupervisedCli
                        || job.state != State::AwaitingCode
                        || code_sent
                        || !valid_code(&code)
                    {
                        continue;
                    }
                    code_sent = true;
                    let written = process_handle
                        .as_ref()
                        .map(|handle| handle.write(&format!("{code}\n")))
                        .unwrap_or(false);
                    if !written {
                        finish!(State::Failed, Some(ErrorCode::WriteFailed));
                    }
                    job.state = State::Verifying;
                    job.updated_at = iso(clock());
                    emit(&job, &events);
                }
            }
            event = async {
                match process_handle.as_mut() {
                    Some(handle) => handle.events.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                let Some(event) = event else { continue };
                match event {
                    ProcessEvent::Data(chunk) => {
                        if job.state != State::Starting {
                            continue;
                        }
                        match parser.push(&chunk) {
                            ParserState::Rejected => {
                                finish!(State::Failed, Some(ErrorCode::UnexpectedOutput));
                            }
                            ParserState::Ready => {
                                reached_verification(&mut job, &parser, &expires_at, &clock, &events);
                            }
                            ParserState::Pending => {}
                        }
                    }
                    ProcessEvent::Exit(code, failure) => {
                        // An external cancel killed the CLI: end cancelled, not denied.
                        if let Some(request) = external_stop.as_ref().and_then(|f| f()) {
                            stopped = request.0 == State::Cancelled;
                            finish!(request.0, request.1);
                        }
                        if let Some(failure) = failure {
                            let code = match failure {
                                SpawnFailure::Missing => ErrorCode::ToolMissing,
                                SpawnFailure::Failed => ErrorCode::WriteFailed,
                            };
                            finish!(State::Failed, Some(code));
                        }
                        if job.state == State::Starting {
                            if parser.end() == ParserState::Ready {
                                reached_verification(&mut job, &parser, &expires_at, &clock, &events);
                            } else {
                                let code = if code == Some(127) {
                                    ErrorCode::ToolMissing
                                } else {
                                    ErrorCode::UnexpectedOutput
                                };
                                finish!(State::Failed, Some(code));
                            }
                        }
                        if code != Some(0) {
                            finish!(State::Failed, Some(ErrorCode::ProviderDenied));
                        }
                        // Exit 0: the install boundary. Timers off; the front door commits.
                        job.state = State::Verifying;
                        job.updated_at = iso(clock());
                        emit(&job, &events);
                        let value = json!({"job": job.value(), "installPending": true});
                        let _ = events.send(SessionEvent::Result(value));
                        return;
                    }
                }
            }
        }
    }
}

fn reached_verification(
    job: &mut Job,
    parser: &Parser,
    expires_at: &str,
    clock: &Clock,
    events: &mpsc::UnboundedSender<SessionEvent>,
) {
    let Some(verification) = parser.verification() else {
        return;
    };
    job.verification = Some(json!({
        "url": verification.url,
        "userCode": verification.user_code,
        "expiresAt": expires_at,
    }));
    job.state = if job.kind == Kind::SupervisedCli {
        State::AwaitingCode
    } else {
        State::Waiting
    };
    job.updated_at = iso(clock());
    let _ = events.send(SessionEvent::Job(job.value()));
}

/// `new Date(ms).toISOString()` on the proven domain; the clock stays in 1970..=9999.
pub(crate) fn iso(ms: i64) -> String {
    aac_num::ecma::to_iso_string(ms).unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_string())
}

/// The restarted placeholder (`RestartedSignInJob`): an id this boot never issued.
pub fn restarted_placeholder(id: &str, now_ms: i64) -> Value {
    aac_core::signin::restarted_placeholder(id, Some(iso(now_ms)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;
    use crate::signin_session::process;
    use crate::signin_session::process::CountingKiller;
    use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

    const OUTPUT: &str = "Open this link\n   https://auth.openai.com/codex/device?state=PRIVATE\nEnter this code\n   ABCD-12345\n";

    fn spec(kind: Kind) -> Spec {
        Spec {
            provider: "codex".into(),
            kind,
            mode: "add".into(),
            account_id: None,
            profile_name: Some("work".into()),
            platform: "ubuntu".into(),
            allowed_origins: vec!["https://auth.openai.com".into()],
            timeout_ms: 900_000,
            first_output_timeout_ms: FIRST_OUTPUT_TIMEOUT_MS,
            command: Command {
                file: "/fake/cli".into(),
                args: vec![],
                env: Default::default(),
                pty: true,
                antigravity_driver: None,
            },
        }
    }

    struct Fake {
        tx: mpsc::UnboundedSender<ProcessEvent>,
        input: mpsc::UnboundedReceiver<String>,
        kills: Arc<AtomicUsize>,
    }

    fn fake_spawner() -> (Spawner, Arc<tokio::sync::Mutex<Fake>>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let kills = Arc::new(AtomicUsize::new(0));
        let killer = CountingKiller {
            kills: kills.clone(),
        };
        let handle = process::test_handle(rx, input_tx, Arc::new(killer));
        let fake = Arc::new(tokio::sync::Mutex::new(Fake {
            tx,
            input: input_rx,
            kills,
        }));
        let shared = Arc::new(std::sync::Mutex::new(Some(handle)));
        let spawner: Spawner = Arc::new(move |_| shared.lock().unwrap().take().unwrap());
        (spawner, fake)
    }

    fn clock_at(ms: i64) -> (Clock, Arc<AtomicI64>) {
        let now = Arc::new(AtomicI64::new(ms));
        let copy = now.clone();
        (Arc::new(move || copy.load(Ordering::SeqCst)), now)
    }

    async fn collect(
        mut events: mpsc::UnboundedReceiver<SessionEvent>,
    ) -> (Vec<Value>, Option<Value>, bool) {
        let mut jobs = Vec::new();
        let mut result = None;
        let mut stopped = false;
        while let Some(event) = events.recv().await {
            match event {
                SessionEvent::Job(job) => jobs.push(job),
                SessionEvent::Result(value) => {
                    result = Some(value);
                    break;
                }
                SessionEvent::Stopped => {
                    stopped = true;
                    break;
                }
            }
        }
        (jobs, result, stopped)
    }

    #[tokio::test]
    async fn device_code_success_sequence() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (handle, events) = start(
            "job_0123456789abcdef".into(),
            spec(Kind::DeviceCode),
            clock,
            spawner,
            None,
        );
        let fake = fake.lock().await;
        fake.tx.send(ProcessEvent::Data(OUTPUT.into())).unwrap();
        fake.tx.send(ProcessEvent::Exit(Some(0), None)).unwrap();
        drop(fake);
        let (jobs, result, stopped) = collect(events).await;
        assert!(!stopped);
        let states: Vec<&str> = jobs.iter().map(|j| j["state"].as_str().unwrap()).collect();
        assert_eq!(states, ["starting", "waiting", "verifying"]);
        assert_eq!(
            jobs[1]["verification"]["url"],
            "https://auth.openai.com/codex/device"
        );
        assert_eq!(jobs[1]["verification"]["userCode"], "ABCD-12345");
        let result = result.unwrap();
        assert_eq!(result["installPending"], true);
        assert_eq!(result["job"]["state"], "verifying");
        // The install boundary keeps the verification facts; TS clears them at finish.
        assert_eq!(
            result["job"]["verification"]["url"],
            "https://auth.openai.com/codex/device"
        );
        let _ = handle;
    }

    async fn wait_state(
        events: &mut mpsc::UnboundedReceiver<SessionEvent>,
        state: &str,
    ) -> Vec<Value> {
        let mut jobs = Vec::new();
        loop {
            let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
                .await
                .expect("state event timeout")
                .expect("event channel");
            match event {
                SessionEvent::Job(job) => {
                    let reached = job["state"].as_str() == Some(state);
                    jobs.push(job);
                    if reached {
                        return jobs;
                    }
                }
                other => panic!("unexpected end before {state}: {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn supervised_code_sequence() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (handle, mut events) = start(
            "job_0123456789abcdef".into(),
            {
                let mut s = spec(Kind::SupervisedCli);
                s.allowed_origins = vec!["https://accounts.google.com".into()];
                s
            },
            clock,
            spawner,
            None,
        );
        let mut fake = fake.lock().await;
        fake.tx
            .send(ProcessEvent::Data(
                "https://accounts.google.com/o/oauth2/auth?code=4/0Abc\n".into(),
            ))
            .unwrap();
        // The URL alone reaches awaiting_code for a supervised flow.
        let mut jobs = wait_state(&mut events, "awaiting_code").await;
        handle.submit_code("4/0AbcXYZ");
        let written = fake.input.recv().await.unwrap();
        assert_eq!(written, "4/0AbcXYZ\n");
        fake.tx.send(ProcessEvent::Exit(Some(0), None)).unwrap();
        drop(fake);
        let (rest, result, _) = collect(events).await;
        jobs.extend(rest);
        let states: Vec<&str> = jobs.iter().map(|j| j["state"].as_str().unwrap()).collect();
        assert_eq!(
            states,
            ["starting", "awaiting_code", "verifying", "verifying"]
        );
        assert_eq!(result.unwrap()["installPending"], true);
    }

    #[tokio::test]
    async fn cancel_kills_and_clears_verification() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (handle, events) = start(
            "job_0123456789abcdef".into(),
            spec(Kind::DeviceCode),
            clock,
            spawner,
            None,
        );
        let fake = fake.lock().await;
        fake.tx.send(ProcessEvent::Data(OUTPUT.into())).unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        handle.request_stop(State::Cancelled, None);
        let (jobs, result, stopped) = collect(events).await;
        assert!(stopped);
        assert!(result.is_none());
        let last = jobs.last().unwrap();
        assert_eq!(last["state"], "cancelled");
        assert_eq!(last["verification"], Value::Null);
        assert_eq!(last["error"], Value::Null);
        assert_eq!(fake.kills.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn nonzero_exit_is_provider_denied() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (_handle, events) = start(
            "job_0123456789abcdef".into(),
            spec(Kind::DeviceCode),
            clock,
            spawner,
            None,
        );
        let fake = fake.lock().await;
        fake.tx.send(ProcessEvent::Data(OUTPUT.into())).unwrap();
        fake.tx.send(ProcessEvent::Exit(Some(1), None)).unwrap();
        drop(fake);
        let (jobs, result, _) = collect(events).await;
        assert_eq!(jobs.last().unwrap()["state"], "failed");
        assert_eq!(jobs.last().unwrap()["error"]["code"], "provider_denied");
        assert_eq!(result.unwrap()["installPending"], false);
    }

    #[tokio::test]
    async fn unexpected_output_fails() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (_handle, events) = start(
            "job_0123456789abcdef".into(),
            spec(Kind::DeviceCode),
            clock,
            spawner,
            None,
        );
        let fake = fake.lock().await;
        fake.tx
            .send(ProcessEvent::Data("https://evil.example/x\n".into()))
            .unwrap();
        drop(fake);
        let (jobs, _, _) = collect(events).await;
        assert_eq!(jobs.last().unwrap()["state"], "failed");
        assert_eq!(jobs.last().unwrap()["error"]["code"], "unexpected_output");
    }

    #[tokio::test(start_paused = true)]
    async fn first_output_timeout_fails_starting() {
        let (spawner, _fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (_handle, events) = start(
            "job_0123456789abcdef".into(),
            spec(Kind::DeviceCode),
            clock,
            spawner,
            None,
        );
        let (jobs, _, _) = collect(events).await;
        assert_eq!(jobs.last().unwrap()["state"], "failed");
        assert_eq!(jobs.last().unwrap()["error"]["code"], "unexpected_output");
    }

    #[tokio::test(start_paused = true)]
    async fn overall_timeout_expires() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let mut s = spec(Kind::DeviceCode);
        s.timeout_ms = 1_000;
        let (_handle, events) = start("job_0123456789abcdef".into(), s, clock, spawner, None);
        let fake = fake.lock().await;
        fake.tx.send(ProcessEvent::Data(OUTPUT.into())).unwrap();
        drop(fake);
        let (jobs, _, _) = collect(events).await;
        assert_eq!(jobs.last().unwrap()["state"], "expired");
        assert_eq!(jobs.last().unwrap()["error"]["code"], "timeout");
    }

    #[tokio::test]
    async fn missing_tool_fails() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (_handle, events) = start(
            "job_0123456789abcdef".into(),
            spec(Kind::DeviceCode),
            clock,
            spawner,
            None,
        );
        let fake = fake.lock().await;
        fake.tx
            .send(ProcessEvent::Exit(None, Some(SpawnFailure::Missing)))
            .unwrap();
        drop(fake);
        let (jobs, _, _) = collect(events).await;
        assert_eq!(jobs.last().unwrap()["error"]["code"], "tool_missing");
    }

    #[tokio::test]
    async fn exit_127_while_starting_is_tool_missing() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (_handle, events) = start(
            "job_0123456789abcdef".into(),
            spec(Kind::DeviceCode),
            clock,
            spawner,
            None,
        );
        let fake = fake.lock().await;
        fake.tx.send(ProcessEvent::Exit(Some(127), None)).unwrap();
        drop(fake);
        let (jobs, _, _) = collect(events).await;
        assert_eq!(jobs.last().unwrap()["error"]["code"], "tool_missing");
    }

    #[tokio::test]
    async fn code_not_expected_outside_awaiting() {
        let (spawner, fake) = fake_spawner();
        let (clock, _) = clock_at(1_700_000_000_000);
        let (handle, mut events) = start(
            "job_0123456789abcdef".into(),
            {
                let mut s = spec(Kind::SupervisedCli);
                s.allowed_origins = vec!["https://accounts.google.com".into()];
                s
            },
            clock,
            spawner,
            None,
        );
        let mut fake = fake.lock().await;
        handle.submit_code("4/0Abc");
        // No URL yet: the code is ignored, no write happens.
        let _ = wait_state(&mut events, "starting").await;
        assert!(fake.input.try_recv().is_err());
        fake.tx
            .send(ProcessEvent::Data("https://accounts.google.com/x\n".into()))
            .unwrap();
        let _ = wait_state(&mut events, "awaiting_code").await;
        handle.submit_code("4/0Abc");
        assert_eq!(fake.input.recv().await.unwrap(), "4/0Abc\n");
        // A second code is not expected once the first was sent.
        handle.submit_code("4/0Abc");
        let _ = wait_state(&mut events, "verifying").await;
        assert!(fake.input.try_recv().is_err());
        handle.request_stop(State::Cancelled, None);
        let (_, _, stopped) = collect(events).await;
        assert!(stopped);
    }

    #[test]
    fn code_and_id_grammars() {
        assert!(valid_code("4/0AbcDEF._~-"));
        assert!(!valid_code(""));
        assert!(!valid_code("has space"));
        assert!(!valid_code(&"x".repeat(2049)));
        assert!(valid_job_id("job_0123456789abcdef"));
        assert!(!valid_job_id("job_0123456789ABCDEF"));
        assert!(!valid_job_id("job_short"));
    }
}
