use super::{AuthRuntime, PublicPeer};
use axum::{
    extract::ws::{CloseFrame, Message, WebSocket},
    http::HeaderMap,
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{broadcast, watch};

/// Push hints for dashboard clients. The recipients and redaction are picked per connection.
#[derive(Clone)]
pub enum DashboardEvent {
    /// An account changed and the dashboard should re-read its snapshot.
    AccountsChanged,
    /// A Status source changed. Sent to signed-in browser sessions only.
    StatusChanged,
    /// Reset claims changed. Sent to signed-in browser sessions only.
    ResetClaimsChanged,
    /// The public sign-in job DTO from the job owner.
    /// The verification and result email fields are removed on plain transports.
    SignInJob(Value),
}

#[derive(Clone)]
pub(super) enum WireEvent {
    Accounts,
    Status,
    Reset(String),
    SignIn(Value),
}

/// Publishes dashboard events to this front door.
#[derive(Clone)]
pub struct DashboardEvents(pub(super) broadcast::Sender<WireEvent>);

impl DashboardEvents {
    /// Enqueues an event for connected clients. Returns the number of queued subscribers.
    pub fn publish(&self, event: DashboardEvent) -> usize {
        let wire = match event {
            DashboardEvent::AccountsChanged => WireEvent::Accounts,
            DashboardEvent::StatusChanged => WireEvent::Status,
            DashboardEvent::ResetClaimsChanged => {
                let Ok(id) = super::storage::random::<16>() else {
                    return 0;
                };
                WireEvent::Reset(super::storage::hex(&id))
            }
            DashboardEvent::SignInJob(job) if job.is_object() => WireEvent::SignIn(job),
            DashboardEvent::SignInJob(_) => return 0,
        };
        self.0.send(wire).unwrap_or(0)
    }
}

#[derive(Clone)]
pub(super) struct Client {
    pub(super) epoch: Option<i64>,
    pub(super) peer: PublicPeer,
    pub(super) headers: HeaderMap,
    pub(super) secure_at_upgrade: bool,
    pub(super) changes: watch::Receiver<u64>,
}

fn event_message(event: WireEvent, session: bool, secure: bool) -> Option<Value> {
    match event {
        WireEvent::Accounts => Some(json!({"type":"accounts-changed"})),
        WireEvent::Status if session => Some(json!({"type":"status-changed"})),
        WireEvent::Reset(id) if session => Some(json!({"type":"reset-claims-changed","id":id})),
        WireEvent::SignIn(mut job) if session => {
            if !secure {
                job["verification"] = Value::Null;
                if job["result"].is_object() {
                    job["result"]["email"] = Value::Null;
                }
            }
            Some(json!({"type":"signin-job","job":job}))
        }
        _ => None,
    }
}

fn client_state(runtime: &mut AuthRuntime, client: &Client) -> Option<bool> {
    let config = runtime.load_config().ok()?;
    let trusted = runtime.settle_credential(&config);
    if let Some(epoch) = client.epoch {
        if !trusted || runtime.files.epoch().ok() != Some(epoch) {
            return None;
        }
    } else if config.enabled {
        return None;
    }
    let current =
        config
            .network
            .connection(&client.headers, client.peer.address.ip(), client.peer.tls);
    Some(client.secure_at_upgrade && current.secure)
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    tokio::time::timeout(Duration::from_secs(2), socket.send(message))
        .await
        .is_ok_and(|sent| sent.is_ok())
}

pub(super) async fn serve(
    mut socket: WebSocket,
    mut events: broadcast::Receiver<WireEvent>,
    mut client: Client,
    runtime: Arc<Mutex<AuthRuntime>>,
) {
    let mut periodic = tokio::time::interval(Duration::from_secs(30));
    loop {
        let event = tokio::select! {
            biased;
            changed = client.changes.changed() => {
                if changed.is_err() { return; }
                None
            }
            _ = periodic.tick() => None,
            event = events.recv() => match event {
                Ok(value) => Some(value),
                Err(broadcast::error::RecvError::Lagged(_)) => None,
                Err(broadcast::error::RecvError::Closed) => return,
            },
            message = socket.recv() => match message {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
                Some(Ok(_)) => continue,
            },
        };
        // File-backed revocations and live network trust are checked again before emitting data.
        let state = {
            let runtime = runtime.clone();
            let facts = client.clone();
            tokio::task::spawn_blocking(move || {
                runtime
                    .lock()
                    .ok()
                    .and_then(|mut runtime| client_state(&mut runtime, &facts))
            })
            .await
            .ok()
            .flatten()
        };
        let Some(secure) = state else {
            let _ = send(
                &mut socket,
                Message::Close(Some(CloseFrame {
                    code: 4001,
                    reason: "Signed out".into(),
                })),
            )
            .await;
            return;
        };
        if let Some(value) =
            event.and_then(|value| event_message(value, client.epoch.is_some(), secure))
            && !send(&mut socket, Message::Text(value.to_string().into())).await
        {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn browser_only_events_and_plain_transport_redaction() {
        let job = json!({"state":"waiting","verification":{"code":"fixture"},"result":{"email":"user@example.test","ok":true}});
        assert!(event_message(WireEvent::SignIn(job.clone()), false, true).is_none());
        assert!(event_message(WireEvent::Status, false, false).is_none());
        assert_eq!(
            event_message(WireEvent::Accounts, false, false),
            Some(json!({"type":"accounts-changed"}))
        );
        assert_eq!(
            event_message(WireEvent::SignIn(job.clone()), true, false),
            Some(json!({
                "type":"signin-job", "job":{"state":"waiting","verification":null,"result":{"email":null,"ok":true}}
            }))
        );
        assert_eq!(
            event_message(WireEvent::SignIn(job.clone()), true, true),
            Some(json!({"type":"signin-job","job":job}))
        );
    }
}
