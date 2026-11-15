//! Host agent with bounded, deadline-controlled NDJSON requests.
mod failover;
mod files;
mod history;
mod inventory;
mod key_store;
mod pins;
mod proof;
mod quota;
mod scan;
mod signin;
use aac_core::{Cause, Failure, UtcMs};
use aac_protocol::{
    Control, Event, Handshake, MAX_FRAME_BYTES, MAX_SCAN_FRAME_BYTES, PROTOCOL_VERSION, Request,
    frame_cap,
};
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
    let request = match frame {
        Ok(Ok(Some(bytes))) => serde_json::from_slice::<Request>(&bytes)
            .map_err(|_| Failure::new(Cause::Damaged, "invalid agent request JSON"))
            .and_then(|mut request| {
                if request.verb == "failover-inventory" && !failover::request_proven(&bytes) {
                    // Delegate raw spellings/caps before serde can erase their distinction.
                    request.args = serde_json::json!({"unprovenRawRequest": true});
                }
                if bytes.len() >= frame_cap(&request.verb) {
                    Err(Failure::new(
                        Cause::OutputTooLarge,
                        "request frame cap exceeded",
                    ))
                } else {
                    Ok(request)
                }
            }),
        Ok(Ok(None)) => Err(Failure::new(Cause::Damaged, "missing agent request")),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(Failure::new(
            Cause::Timeout,
            "request read deadline exceeded",
        )),
    };
    let (id, cap, result) = match request {
        Err(failure) => (String::new(), MAX_FRAME_BYTES, Err(failure)),
        Ok(request) => {
            let id = request.id.clone();
            let cap = frame_cap(&request.verb);
            let result = match request.validate() {
                Err(failure) => Err(failure),
                Ok(()) => {
                    let control = async {
                        loop {
                            let Some(line) = read_line(&mut reader, MAX_FRAME_BYTES).await? else {
                                return std::future::pending::<Result<serde_json::Value, Failure>>(
                                )
                                .await;
                            };
                            match serde_json::from_slice::<Control>(&line) {
                                Ok(Control::Cancel { cancel: true }) => {
                                    return Err(Failure::new(
                                        Cause::Cancelled,
                                        "agent request cancelled",
                                    ));
                                }
                                Ok(_) => {}
                                Err(_) => {
                                    return Err(Failure::new(
                                        Cause::Refused,
                                        "invalid agent control",
                                    ));
                                }
                            }
                        }
                    };
                    tokio::select! {
                        biased;
                        result = control => result,
                        _ = tokio::time::sleep(std::time::Duration::from_millis(request.deadline_ms)) => Err(Failure::new(Cause::Timeout, "agent deadline exceeded")),
                        result = dispatch(&request) => result,
                    }
                }
            };
            (id, cap, result)
        }
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
    let mut bytes = serde_json::to_vec(&event)
        .map_err(|_| Failure::new(Cause::Bug, "event encoding failed"))?;
    if bytes.len() >= cap {
        return Err(Failure::new(Cause::OutputTooLarge, "event cap exceeded"));
    }
    bytes.push(b'\n');
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(code)
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
        "key-store" | "t3-pins" | "claude-history" | "failover-inventory" | "signin-expiry"
    ) {
        let args = request.args.clone();
        let verb = request.verb.clone();
        return blocking(move || match verb.as_str() {
            "key-store" => key_store::run(&args),
            "t3-pins" => pins::run(&args),
            "failover-inventory" => failover::run(&args),
            "signin-expiry" => signin::run(&args),
            _ => history::run(&args),
        })
        .await;
    }
    if request.verb == "quota-read" {
        return quota::run(&request.args).await;
    }
    if request.verb == "scan-usage" {
        return scan::run(&request.args).await;
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
