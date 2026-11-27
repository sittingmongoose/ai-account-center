//! Host agent with bounded, deadline-controlled NDJSON requests.
pub mod allocator;
mod antigravity;
mod apps;
mod claude_host;
mod claude_usage;
/// In-memory Codex update process runtime.
pub mod codex_update;
mod cursor_open;
/// Standalone desktop usage collectors and shared helpers (`desktop_helpers.py`, `desktop_usage.py`, `cursor_usage.py`).
pub mod desktop_usage;
mod failover;
/// Own-host additional-provider failover discovery and collection.
pub mod failover_additional;
/// Own-host special-provider failover discovery and collection.
pub mod failover_special;
mod files;
mod history;
mod inventory;
mod key_store;
mod launch;
mod logins;
mod muse_login;
mod pins;
mod proof;
mod quota;
mod rollback;
mod scan;
mod signin;
/// Interactive sign-in session state machine, PTY driver, journal and verb.
pub mod signin_session;
pub mod web_session;
use aac_core::{Cause, Failure, UtcMs};
use aac_protocol::{
    Control, Event, Handshake, MAX_FRAME_BYTES, MAX_HISTORY_FRAME_BYTES, MAX_SCAN_FRAME_BYTES,
    PROTOCOL_VERSION, Request, frame_cap, max_deadline_ms,
};
use serde::Deserialize;
use serde_json::value::RawValue;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

/// The private Claude reset host, one call per process: `{"request": {...}}` on stdin (the
/// reset transaction's helper payload), one JSON answer on stdout. Exit 0 unless the input or
/// output itself failed; every refusal is an answer, never provider text.
pub async fn claude_reset_stdio() -> i32 {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut bytes = vec![];
    if tokio::io::stdin()
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .is_err()
        || bytes.len() > 2 * 1024 * 1024
    {
        return 1;
    }
    let Ok(payload) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return 1;
    };
    let answer = claude_usage::host::reset_host(&payload["request"]).await;
    let Ok(mut out) = serde_json::to_vec(&answer) else {
        return 1;
    };
    out.push(b'\n');
    let mut stdout = tokio::io::stdout();
    if stdout.write_all(&out).await.is_err() || stdout.flush().await.is_err() {
        return 1;
    }
    0
}

/// The macOS GUI-session child of [`claude_reset_stdio`] (launchd `gui/<uid>`).
pub async fn claude_reset_gui(reply: &std::path::Path) -> i32 {
    claude_usage::keychain_bridge::child(reply).await
}

/// `aac-agent --terminal-child ...`: the app-update relaunch child (see `apps::updater::terminal`).
pub fn terminal_child(args: &[std::ffi::OsString]) -> i32 {
    apps::updater::terminal::terminal_child(args)
}

/// `aac-agent --console-probe ...`: the Windows console-window probe (see `apps::updater::terminal`).
pub fn console_probe(args: &[std::ffi::OsString]) -> i32 {
    apps::updater::terminal::console_probe(args)
}

/// `aac-agent --t3-restart ...` / `--t3-issuer ...`: T3 helper subcommands
/// (see `apps::updater::t3`); `None` when `flag` is neither.
pub fn app_update_subcommand(flag: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> Option<i32> {
    apps::updater::subcommand(flag.to_str()?, args)
}

/// `aac-agent --app-update-task`: the Windows interactive-task child (see `apps::updater::windows_task`).
pub async fn app_update_task() -> i32 {
    apps::updater::windows_task::child().await
}

/// The Central Logins verbs (`login-inventory`, `login-put`, `login-remove`, `login-copy`) run
/// in-process, as the dispatcher runs them: the dashboard's delivery tests drive real agent
/// homes through this.
pub fn login_verb(verb: &str, args: &serde_json::Value) -> Result<serde_json::Value, Failure> {
    logins::run(verb, args)
}

/// `aac-agent --logins-quarantine ...`, `--logins-restore`, `--logins-status`: Central Logins
/// step 3 on this computer (see `rollback`); `None` when `flag` is none of them.
pub fn logins_subcommand(flag: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> Option<i32> {
    rollback::subcommand(flag.to_str()?, args)
}

/// Return the exact build identity used by shipping's manifest handshake.
pub fn handshake() -> Handshake {
    Handshake {
        name: "aac-agent".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        protocol: PROTOCOL_VERSION,
        target: env!("AAC_AGENT_TARGET").into(),
    }
}

/// Read one newline-delimited frame while allocating at most `cap` bytes; EOF is distinct.
pub async fn read_line(
    reader: &mut (impl AsyncBufRead + Unpin),
    cap: usize,
) -> Result<Option<Vec<u8>>, Failure> {
    let mut bytes = Vec::new();
    loop {
        let buffer = reader.fill_buf().await?;
        if buffer.is_empty() {
            return if bytes.is_empty() {
                Ok(None)
            } else {
                Err(Failure::new(Cause::Damaged, "unterminated protocol frame"))
            };
        }
        let end = buffer
            .iter()
            .position(|c| *c == b'\n')
            .map(|n| n + 1)
            .unwrap_or(buffer.len());
        if end > cap.saturating_sub(bytes.len()) {
            return Err(Failure::new(
                Cause::OutputTooLarge,
                "protocol frame cap exceeded",
            ));
        }
        bytes.extend_from_slice(&buffer[..end]);
        reader.consume(end);
        if bytes.ends_with(b"\n") {
            bytes.pop();
            return Ok(Some(bytes));
        }
    }
}

/// Dispatch one process request and emit exactly one terminal NDJSON event. Exit 0 means a
/// successful result, 1 a classified failure. The initial request has a fixed five-second read
/// deadline; its `deadlineMs` starts after receipt, and stdin remains open for cancel/input.
pub async fn run(
    mut reader: impl AsyncBufRead + Unpin,
    mut writer: impl AsyncWrite + Unpin,
) -> Result<i32, Failure> {
    let frame = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        read_line(&mut reader, MAX_HISTORY_FRAME_BYTES),
    )
    .await;
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct RawEnvelope<'a> {
        protocol: u32,
        verb: String,
        id: String,
        deadline_ms: u64,
        #[serde(borrow)]
        args: &'a RawValue,
    }

    enum ParsedRequest {
        ScanUsage {
            id: String,
            deadline_ms: u64,
            frame: Vec<u8>,
            validation: Result<(), Failure>,
        },
        Other(Request),
    }

    let request = match frame {
        Ok(Ok(Some(bytes))) => {
            let parsed = serde_json::from_slice::<RawEnvelope<'_>>(&bytes)
                .map_err(|_| Failure::new(Cause::Damaged, "invalid agent request JSON"))
                .and_then(|envelope| {
                    if bytes.len() >= frame_cap(&envelope.verb) {
                        return Err(Failure::new(
                            Cause::OutputTooLarge,
                            "request frame cap exceeded",
                        ));
                    }
                    if envelope.verb == "scan-usage" {
                        let validation = validate_scan_envelope(
                            envelope.protocol,
                            &envelope.id,
                            &envelope.verb,
                            envelope.deadline_ms,
                            envelope.args,
                        );
                        return Ok((Some((envelope.id, envelope.deadline_ms, validation)), None));
                    }
                    let unproven_failover =
                        envelope.verb == "failover-inventory" && !failover::request_proven(&bytes);
                    let args: serde_json::Value = serde_json::from_str(envelope.args.get())
                        .map_err(|_| Failure::new(Cause::Damaged, "invalid agent request JSON"))?;
                    // Judge raw spellings/caps before serde can erase their distinction. Only the
                    // action survives, so the refusal answers in that action's shape.
                    let args = if unproven_failover {
                        failover::unproven_args(&args)
                    } else {
                        args
                    };
                    Ok((
                        None,
                        Some(Request {
                            protocol: envelope.protocol,
                            verb: envelope.verb,
                            id: envelope.id,
                            deadline_ms: envelope.deadline_ms,
                            args,
                        }),
                    ))
                });
            match parsed {
                Ok((Some((id, deadline_ms, validation)), None)) => Ok(ParsedRequest::ScanUsage {
                    id,
                    deadline_ms,
                    frame: bytes,
                    validation,
                }),
                Ok((None, Some(req))) => {
                    drop(bytes);
                    Ok(ParsedRequest::Other(req))
                }
                Err(failure) => {
                    drop(bytes);
                    Err(failure)
                }
                _ => unreachable!(),
            }
        }
        Ok(Ok(None)) => Err(Failure::new(Cause::Damaged, "missing agent request")),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(Failure::new(
            Cause::Timeout,
            "request read deadline exceeded",
        )),
    };
    match request {
        Ok(ParsedRequest::ScanUsage {
            id,
            deadline_ms,
            frame,
            validation,
        }) => {
            let cap = MAX_SCAN_FRAME_BYTES;
            let outcome = match validation {
                Err(failure) => {
                    drop(frame);
                    Err(failure)
                }
                Ok(()) => run_scan_request(deadline_ms, frame, &mut reader).await,
            };
            match outcome {
                Ok(raw_value) => {
                    write_raw_result(&mut writer, &id, raw_value, cap).await?;
                    Ok(0)
                }
                Err(failure) => {
                    let event = Event::Failure {
                        protocol: PROTOCOL_VERSION,
                        id: safe_id(&id),
                        failure,
                    };
                    write_event(&mut writer, &event, cap).await?;
                    Ok(1)
                }
            }
        }
        other => {
            let (id, cap, result) = match other {
                Err(failure) => (String::new(), MAX_FRAME_BYTES, Err(failure)),
                Ok(ParsedRequest::Other(request)) => {
                    let id = request.id.clone();
                    let cap = frame_cap(&request.verb);
                    let result = match request.validate() {
                        Err(failure) => Err(failure),
                        Ok(()) => {
                            if request.verb == "signin-session" {
                                // The interactive session owns the control stream and emits its
                                // own progress events; one terminal frame still ends the process.
                                return signin_session::verb::interactive(
                                    request,
                                    &mut reader,
                                    &mut writer,
                                )
                                .await;
                            }
                            run_request(request, &mut reader, &mut writer).await
                        }
                    };
                    (id, cap, result)
                }
                Ok(ParsedRequest::ScanUsage { .. }) => unreachable!(),
            };
            let (event, code) = match result {
                Ok(value) => (
                    Event::Result {
                        protocol: PROTOCOL_VERSION,
                        id,
                        value,
                    },
                    0,
                ),
                Err(failure) => (
                    Event::Failure {
                        protocol: PROTOCOL_VERSION,
                        id: safe_id(&id),
                        failure,
                    },
                    1,
                ),
            };
            write_event(&mut writer, &event, cap).await?;
            Ok(code)
        }
    }
}

fn validate_scan_envelope(
    protocol: u32,
    id: &str,
    verb: &str,
    deadline_ms: u64,
    args: &RawValue,
) -> Result<(), Failure> {
    if protocol != PROTOCOL_VERSION {
        return Err(Failure::new(
            Cause::AgentMismatch,
            "agent protocol mismatch",
        ));
    }
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
        || deadline_ms == 0
        || deadline_ms > max_deadline_ms(verb)
        || !args.get().trim_start().starts_with('{')
    {
        return Err(Failure::new(Cause::Refused, "invalid request envelope"));
    }
    Ok(())
}

async fn run_scan_request(
    deadline_ms: u64,
    frame: Vec<u8>,
    reader: &mut (impl AsyncBufRead + Unpin),
) -> Result<Box<RawValue>, Failure> {
    use std::time::Duration;
    use tokio::time::Instant;

    let deadline = Instant::now() + Duration::from_millis(deadline_ms);
    let control = async {
        loop {
            let Some(line) = read_line(reader, MAX_FRAME_BYTES).await? else {
                return std::future::pending::<Result<Box<RawValue>, Failure>>().await;
            };
            match serde_json::from_slice::<Control>(&line) {
                Ok(Control::Cancel { cancel: true }) => {
                    return Err(Failure::new(Cause::Cancelled, "agent request cancelled"));
                }
                Ok(_) => {}
                Err(_) => {
                    return Err(Failure::new(Cause::Refused, "invalid agent control"));
                }
            }
        }
    };
    let work = async {
        tokio::task::yield_now().await;
        scan::run_frame(frame).await
    };
    tokio::select! {
        biased;
        result = control => result,
        _ = tokio::time::sleep_until(deadline) => Err(Failure::new(Cause::Timeout, "agent deadline exceeded")),
        result = work => result,
    }
}

async fn write_raw_result(
    writer: &mut (impl AsyncWrite + Unpin),
    id: &str,
    value: Box<RawValue>,
    cap: usize,
) -> Result<(), Failure> {
    // The result event `{"type":"result","protocol":1,"id":..,"value":..}` is written in three
    // parts around the answer's own text, so a scan answer is never copied into a second
    // buffer (R4). The bytes are exactly those of the serialized event.
    let id =
        serde_json::to_string(id).map_err(|_| Failure::new(Cause::Bug, "event encoding failed"))?;
    let head =
        format!("{{\"type\":\"result\",\"protocol\":{PROTOCOL_VERSION},\"id\":{id},\"value\":");
    if head.len() + value.get().len() + 1 >= cap {
        return Err(Failure::new(Cause::OutputTooLarge, "event cap exceeded"));
    }
    writer.write_all(head.as_bytes()).await?;
    writer.write_all(value.get().as_bytes()).await?;
    drop(value);
    writer.write_all(b"}\n").await?;
    writer.flush().await?;
    Ok(())
}

async fn run_request(
    mut request: Request,
    reader: &mut (impl AsyncBufRead + Unpin),
    writer: &mut (impl AsyncWrite + Unpin),
) -> Result<serde_json::Value, Failure> {
    use aac_protocol::{APP_UPDATE_ADMISSION, APP_UPDATE_ADMIT};
    use std::time::Duration;
    use tokio::time::Instant;

    let deadline = Instant::now() + Duration::from_millis(request.deadline_ms);
    let prepared = match (request.verb.as_str(), request.args["action"].as_str()) {
        ("apps", Some("prepare-update")) => Some("update"),
        ("apps", Some("prepare-acp-follow-up")) => Some("acp-follow-up"),
        _ => None,
    };
    if let Some(action) = prepared {
        // No journal, installer or host lease is opened until the dashboard grants this
        // connected request. An older agent refuses the preparation action.
        let ready = Event::Progress {
            protocol: PROTOCOL_VERSION,
            id: request.id.clone(),
            data: serde_json::json!({"event": APP_UPDATE_ADMISSION}),
        };
        let admission = async {
            write_event(writer, &ready, frame_cap(&request.verb)).await?;
            read_line(reader, 64).await
        };
        let until = deadline.min(Instant::now() + Duration::from_secs(5));
        let grant = tokio::time::timeout_at(until, admission)
            .await
            .map_err(|_| {
                Failure::new(Cause::Timeout, "app update admission deadline exceeded")
            })??;
        if grant.as_deref() != Some(APP_UPDATE_ADMIT.as_bytes()) {
            return Err(Failure::new(
                Cause::Cancelled,
                "app update was not admitted",
            ));
        }
        request.args["action"] = serde_json::json!(action);
    }
    if request.verb == "apps" && request.args["action"] == "acp-follow-up" && prepared.is_none() {
        return Err(Failure::new(Cause::Refused, "app update was not prepared"));
    }
    let control = async {
        loop {
            let Some(line) = read_line(reader, MAX_FRAME_BYTES).await? else {
                return std::future::pending::<Result<serde_json::Value, Failure>>().await;
            };
            match serde_json::from_slice::<Control>(&line) {
                Ok(Control::Cancel { cancel: true }) => {
                    return Err(Failure::new(Cause::Cancelled, "agent request cancelled"));
                }
                Ok(_) => {}
                Err(_) => {
                    return Err(Failure::new(Cause::Refused, "invalid agent control"));
                }
            }
        }
    };
    let work = async {
        if request.verb == "apps"
            && matches!(
                request.args["action"].as_str(),
                Some("update" | "cancel" | "acp-follow-up")
            )
        {
            return apps::update::run(&request, writer).await;
        }
        dispatch(&request).await
    };
    tokio::select! {
        biased;
        result = control => result,
        _ = tokio::time::sleep_until(deadline) => Err(Failure::new(Cause::Timeout, "agent deadline exceeded")),
        result = work => result,
    }
}

pub(crate) async fn write_event(
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

fn safe_id(id: &str) -> String {
    if id.len() <= 128
        && id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
    {
        id.into()
    } else {
        String::new()
    }
}

/// Run blocking file, Keychain or process work on a worker so the request's deadline and cancel
/// branches stay live; the whole verb (its proof scope and fixture state are thread-local) runs on
/// that one worker. Main exits after the terminal event, so a stuck read cannot hold the reply.
pub(crate) async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, Failure> + Send + 'static,
) -> Result<T, Failure> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Failure::new(Cause::Bug, "agent worker failed"))?
}

async fn dispatch(request: &Request) -> Result<serde_json::Value, Failure> {
    tokio::task::yield_now().await;
    if matches!(
        request.verb.as_str(),
        "key-store"
            | "t3-pins"
            | "claude-history"
            | "failover-inventory"
            | "signin-expiry"
            | "launch"
            | "claude-host"
            | "muse-login-identity"
            | "login-inventory"
            | "login-put"
            | "login-remove"
            | "login-copy"
    ) {
        let args = request.args.clone();
        let verb = request.verb.clone();
        return blocking(move || match verb.as_str() {
            "key-store" => key_store::run(&args),
            "launch" => launch::run(&args),
            "claude-host" => claude_host::run(&args),
            "t3-pins" => pins::run(&args),
            "failover-inventory" => failover::run(&args),
            "signin-expiry" => signin::run(&args),
            "muse-login-identity" => muse_login::run(&args),
            "login-inventory" | "login-put" | "login-remove" | "login-copy" => {
                logins::run(&verb, &args)
            }
            _ => history::run(&args),
        })
        .await;
    }
    if request.verb == "cursor-open" {
        return cursor_open::run(&request.args).await;
    }
    if request.verb == "quota-read" {
        return quota::run(&request.args).await;
    }
    if request.verb == "web-session" {
        return web_session::run_verb(&request.args).await;
    }
    if request.verb == "apps" {
        return apps::run_inventory(&request.args).await;
    }
    if request.verb == "antigravity" {
        return antigravity::run(&request.args).await;
    }
    if request.verb == "inventory" {
        return inventory::run(&request.args).await;
    }
    if request.verb != "health" {
        return Err(Failure::new(Cause::Refused, "unsupported agent verb"));
    }
    if request.args.as_object().is_none_or(|args| !args.is_empty()) {
        return Err(Failure::new(Cause::Refused, "health takes no arguments"));
    }
    let identity = handshake();
    Ok(serde_json::json!({ "status": "ok", "observedAt": UtcMs::now()?, "agent": identity }))
}

#[cfg(test)]
mod raw_result_tests {
    use super::*;
    use serde::Serialize;

    /// R4: the three-part write is byte for byte the event the agent serialized before, and the
    /// event cap still counts the whole line without its newline.
    #[tokio::test]
    async fn a_raw_result_is_written_exactly_as_the_serialized_event() {
        #[derive(Serialize)]
        struct RawResultEvent<'a> {
            #[serde(rename = "type")]
            kind: &'static str,
            protocol: u32,
            id: &'a str,
            value: Box<RawValue>,
        }
        for (id, value) in [
            (
                "r4-scan",
                r#"{"kinds":{"claude":{"state":"ok"}},"rows":[]}"#,
            ),
            ("", "{}"),
            (
                "a.b:c_d-1",
                r#"{"text":"caf\u00e9 \"quoted\" \n","n":[1,2.5,-3e9]}"#,
            ),
        ] {
            let raw = RawValue::from_string(value.to_string()).expect("raw value");
            let mut expected = serde_json::to_vec(&RawResultEvent {
                kind: "result",
                protocol: PROTOCOL_VERSION,
                id,
                value: raw.clone(),
            })
            .expect("written");
            let exact = expected.len();
            expected.push(b'\n');
            let mut written = Vec::new();
            write_raw_result(&mut written, id, raw.clone(), exact + 1)
                .await
                .expect("written");
            assert_eq!(written, expected);
            let mut refused = Vec::new();
            let failure = write_raw_result(&mut refused, id, raw, exact)
                .await
                .expect_err("refused at the cap");
            assert_eq!(failure.cause, Cause::OutputTooLarge);
            assert!(refused.is_empty());
        }
    }
}
