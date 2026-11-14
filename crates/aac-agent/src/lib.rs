//! Health-only host agent. Reads one bounded request followed by optional controls; no usage
//! scan, provider refresh, credential access or external host action is implemented here.
use aac_core::{Cause, Failure, UtcMs};
use aac_protocol::{Control, Event, Handshake, MAX_FRAME_BYTES, PROTOCOL_VERSION, Request};
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
        read_line(&mut reader, MAX_FRAME_BYTES),
    )
    .await;
    let request = match frame {
        Ok(Ok(Some(bytes))) => serde_json::from_slice::<Request>(&bytes)
            .map_err(|_| Failure::new(Cause::Damaged, "invalid agent request JSON")),
        Ok(Ok(None)) => Err(Failure::new(Cause::Damaged, "missing agent request")),
        Ok(Err(failure)) => Err(failure),
        Err(_) => Err(Failure::new(
            Cause::Timeout,
            "request read deadline exceeded",
        )),
    };
    let (id, result) = match request {
        Err(failure) => (String::new(), Err(failure)),
        Ok(request) => {
            let id = request.id.clone();
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
            (id, result)
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
    if bytes.len() >= MAX_FRAME_BYTES {
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

async fn dispatch(request: &Request) -> Result<serde_json::Value, Failure> {
    tokio::task::yield_now().await;
    if request.verb != "health" {
        return Err(Failure::new(Cause::Refused, "unsupported agent verb"));
    }
    if request.args.as_object().is_none_or(|args| !args.is_empty()) {
        return Err(Failure::new(Cause::Refused, "health takes no arguments"));
    }
    let identity = handshake();
    Ok(serde_json::json!({ "status": "ok", "observedAt": UtcMs::now()?, "agent": identity }))
}
