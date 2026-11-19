//! The cut's authenticated access to the remaining store host.
use super::{Reply, storage};
use aac_files::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use axum::{body::Body, extract::Request};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::json;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::UnixStream;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionEndpoint {
    ccs_dir: PathBuf,
    socket_path: PathBuf,
    secret: String,
    boot_id: String,
}

impl CollectionEndpoint {
    fn load(root: &Path) -> Option<Self> {
        let root = root.canonicalize().ok()?;
        let policy = ReadPolicy::capped(8192)
            .ancestors(Ancestors::NoLinkBelow(&root))
            .mode(Mode::Private)
            .owner(Owner::Mine);
        let Read::Bytes(bytes, _) = aac_files::read::read(&root.join("run/aac-http.json"), &policy)
        else {
            return None;
        };
        let endpoint: Self = serde_json::from_slice(&bytes).ok()?;
        let parent = endpoint.socket_path.parent()?;
        let parent_mode = std::fs::symlink_metadata(parent).ok()?;
        let metadata = std::fs::symlink_metadata(&endpoint.socket_path).ok()?;
        let uid = std::fs::metadata(&root).ok()?.uid();
        (endpoint.ccs_dir == root
            && endpoint.socket_path.is_absolute()
            && endpoint.socket_path.file_name()?.to_str()? == "http.sock"
            && parent.file_name()?.to_str()? == format!("aac-http-{}", endpoint.boot_id)
            && storage::is_hex(&endpoint.secret, 64)
            && storage::is_hex(&endpoint.boot_id, 24)
            && parent_mode.is_dir()
            && parent_mode.uid() == uid
            && parent_mode.mode() & 0o777 == 0o700
            && metadata.file_type().is_socket()
            && metadata.uid() == uid
            && metadata.mode() & 0o777 == 0o600)
            .then_some(endpoint)
    }
}

/// Forward a completed native generation to the existing store owner. The G14
/// sampler consumes its regular feed and keeps its history import order.
pub(super) async fn publish_usage(
    root: PathBuf,
    cache: &crate::account_usage::AccountUsageCache,
    now: i64,
    analytics_writers: bool,
) {
    let Some(timestamp) = storage::iso(now) else {
        return;
    };
    let endpoint = tokio::task::spawn_blocking(move || CollectionEndpoint::load(&root))
        .await
        .ok()
        .flatten();
    let Some(endpoint) = endpoint else {
        return;
    };
    if analytics_writers {
        let Some(generation) = cache.generation(crate::account_usage::Desktop::Mac, false) else {
            return;
        };
        let dashboards = cache.hub_dashboards(&timestamp, now);
        let body = json!({
            "generation":generation,
            "data": { "dashboards":dashboards },
            "facts": {},
        });
        endpoint
            .send("PUT", "/internal/feed/analytics-dashboard", body)
            .await;
    } else if let Ok(observations) = cache.store_observations(&timestamp, now, i64::MIN)
        && !observations.is_empty()
    {
        endpoint
            .send(
                "POST",
                "/internal/store/record",
                json!({"observations":observations}),
            )
            .await;
    }
}

impl CollectionEndpoint {
    async fn send(&self, method: &str, uri: &str, value: serde_json::Value) -> bool {
        let Ok(request) = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "localhost")
            .header("x-aac-boot-secret", &self.secret)
            .header(
                "x-aac-principal",
                r#"{"kind":"session","id":"front-door","role":"service"}"#,
            )
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
        else {
            return false;
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            let stream = UnixStream::connect(&self.socket_path).await.ok()?;
            let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.ok()?;
            let reply = tokio::select! {
                biased;
                reply = sender.send_request(request) => reply.ok()?,
                _ = connection => return None,
            };
            Some(
                reply.headers().get("x-aac-service-boot")?.to_str().ok()? == self.boot_id
                    && reply.status().is_success(),
            )
        })
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
    }
}
