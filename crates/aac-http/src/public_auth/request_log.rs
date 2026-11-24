//! Request correlation and the dashboard's persistent completion log.
use super::{Dashboard, Reply, header, peer, set_header, static_ui::AssetResponse};
use aac_log::{Extra, Level, Log, RequestLog};
use axum::{
    extract::{Request, State},
    http::HeaderValue,
    middleware::Next,
};
use serde_json::{Map, json};
use std::{sync::Arc, time::Instant};

#[derive(Clone)]
pub(super) struct RequestId(pub(super) HeaderValue);

#[derive(Clone, Copy)]
pub(super) struct RequestFailure(pub(super) &'static str);

#[derive(Clone)]
pub(super) struct Audit {
    pub(super) dashboard: Dashboard,
    pub(super) log: Arc<Log>,
}

pub(super) async fn record(State(audit): State<Audit>, mut request: Request, next: Next) -> Reply {
    let start = Instant::now();
    let id = aac_log::request_id();
    if let Ok(value) = HeaderValue::from_str(&id) {
        request.extensions_mut().insert(RequestId(value));
    }
    let method = request.method().clone();
    let url = request
        .uri()
        .path_and_query()
        .map_or("/", |path| path.as_str())
        .to_owned();
    let remote = peer(request.extensions());
    let headers = request.headers().clone();
    let mut reply = next.run(request).await;
    if reply.extensions().get::<AssetResponse>().is_some() {
        return reply;
    }

    set_header(&mut reply, "x-ccs-request-id", &id);
    if url.starts_with("/api/logs") {
        return reply;
    }
    let status = reply.status().as_u16();
    let failure = reply.extensions().get::<RequestFailure>().copied();
    let duration_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    // Fire-and-forget log append off the HTTP reply critical path (never block response flush on flock).
    tokio::task::spawn_blocking(move || {
        if let Some(RequestFailure(kind)) = failure {
            let context = Map::from_iter([
                ("status".into(), json!(status)),
                ("kind".into(), json!(kind)),
                ("headersSent".into(), json!(false)),
            ]);
            audit.log.write(
                "web-server",
                Level::Error,
                "web.request.failed",
                "Dashboard request failed",
                &context,
                &Extra {
                    request_id: Some(id.clone()),
                    ..Extra::default()
                },
            );
        }
        let connection = remote.and_then(|remote| {
            let runtime = audit.dashboard.runtime.lock().ok()?;
            let config = runtime.load_config().ok()?;
            Some(
                config
                    .network
                    .connection(&headers, remote.address.ip(), remote.tls),
            )
        });
        let peer_address = remote.map(|remote| remote.address.ip().to_string());
        let client = connection
            .as_ref()
            .and_then(|connection| connection.client_address());
        let via = connection
            .as_ref()
            .filter(|connection| connection.via_proxy)
            .map(|_| peer_address.as_deref());
        audit.log.request_completed(&RequestLog {
            request_id: &id,
            method: method.as_str(),
            url: &url,
            status,
            duration_ms,
            remote_address: client.as_deref().or(peer_address.as_deref()),
            via,
            user_agent: header(&headers, "user-agent"),
        });
    });
    reply
}
