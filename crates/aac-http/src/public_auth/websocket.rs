use super::{AuthRuntime, DashboardEvent, PublicPeer, now, storage::SessionWatch};
use axum::{
    extract::ws::{CloseFrame, Message, WebSocket},
    http::HeaderMap,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::sync::{broadcast, watch};

#[derive(Clone)]
pub(super) struct Client {
    pub(super) session: Option<SessionWatch>,
    pub(super) peer: PublicPeer,
    pub(super) headers: HeaderMap,
    pub(super) secure_at_upgrade: bool,
    pub(super) changes: watch::Receiver<u64>,
}

fn client_state(runtime: &mut AuthRuntime, client: &Client) -> Option<bool> {
    let config = runtime.load_config().ok()?;
    let trusted = runtime.settle_credential(&config);
    let current =
        config
            .network
            .connection(&client.headers, client.peer.address.ip(), client.peer.tls);
    if let Some(admitted) = &client.session {
        let epoch = runtime.files.epoch().ok()?;
        if !config.enabled
            || !trusted
            || epoch != admitted.epoch
            || !runtime
                .sessions
                .read_record(&admitted.key, &current, now())
                .is_some_and(|session| session.current(Ok(epoch)))
        {
            return None;
        }
    } else if config.enabled {
        return None;
    }
    Some(client.secure_at_upgrade && current.secure)
}

async fn invalidated(session: &mut Option<SessionWatch>) {
    if let Some(session) = session {
        let _ = session.invalidated.changed().await;
    } else {
        std::future::pending().await
    }
}

async fn send(socket: &mut WebSocket, message: Message) -> bool {
    tokio::time::timeout(Duration::from_secs(2), socket.send(message))
        .await
        .is_ok_and(|sent| sent.is_ok())
}

pub(super) async fn serve(
    mut socket: WebSocket,
    mut events: broadcast::Receiver<DashboardEvent>,
    mut client: Client,
    runtime: Arc<Mutex<AuthRuntime>>,
) {
    let mut periodic = tokio::time::interval(Duration::from_secs(30));
    loop {
        let event = tokio::select! {
            biased;
            _ = invalidated(&mut client.session) => break,
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
        // Revalidate the live record and network policy at each tick and before delivery.
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
            break;
        };
        if let Some(value) =
            event.and_then(|value| value.for_client(client.session.is_some(), secure))
            && let Ok(text) = serde_json::to_string(&value)
        {
            tokio::select! {
                biased;
                // A pending write may already have queued bytes. Drop the socket
                // on invalidation so a close frame cannot flush those bytes.
                _ = invalidated(&mut client.session) => return,
                sent = send(&mut socket, Message::Text(text.into())) => {
                    if !sent { return; }
                }
            }
        }
    }
    let _ = send(
        &mut socket,
        Message::Close(Some(CloseFrame {
            code: 4001,
            reason: "Signed out".into(),
        })),
    )
    .await;
}
