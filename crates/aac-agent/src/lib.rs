//! Host agent with bounded, deadline-controlled NDJSON requests.
pub mod allocator;
mod apps;
mod claude_host;
mod cursor_open;
mod failover;
mod files;
mod history;
mod inventory;
mod key_store;
mod launch;
mod muse_login;
mod pins;
mod proof;
mod quota;
mod scan;
mod signin;
mod signin_session;
use aac_core::{Cause, Failure, UtcMs};
use aac_protocol::{
    Control, Event, Handshake, MAX_FRAME_BYTES, MAX_SCAN_FRAME_BYTES, PROTOCOL_VERSION, Request,
    frame_cap, max_deadline_ms,
};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

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
        read_line(&mut reader, MAX_SCAN_FRAME_BYTES),
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
                    let args = if unproven_failover {
                        serde_json::json!({"unprovenRawRequest": true})
                    } else {
                        serde_json::from_str(envelope.args.get()).map_err(|_| {
                            Failure::new(Cause::Damaged, "invalid agent request JSON")
                        })?
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
    #[derive(Serialize)]
    struct RawResultEvent<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        protocol: u32,
        id: &'a str,
        value: Box<RawValue>,
    }
    let event = RawResultEvent {
        kind: "result",
        protocol: PROTOCOL_VERSION,
        id,
        value,
    };
    let mut bytes = serde_json::to_vec(&event)
        .map_err(|_| Failure::new(Cause::Bug, "event encoding failed"))?;
    drop(event);
    if bytes.len() >= cap {
        return Err(Failure::new(Cause::OutputTooLarge, "event cap exceeded"));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
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

    if request.verb == "apps" {
        return apps::run_inventory(&request.args).await;
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
