//! A boot-scoped loopback feed for the compatibility worker's collection-dependent views.
use crate::account_usage::{AccountUsageCache, Desktop};
use axum::{
    Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use std::{
    io,
    net::SocketAddr,
    sync::{Arc, OnceLock},
    time::Duration,
};
use subtle::ConstantTimeEq;

#[derive(Clone)]
struct Source {
    proof: Arc<str>,
    local: Arc<OnceLock<AccountUsageCache>>,
    #[cfg(unix)]
    adopted: Arc<OnceLock<super::service_link::ServiceLink>>,
    #[cfg(windows)]
    store: Arc<OnceLock<std::sync::Weak<super::owner_host::OwnerHost>>>,
}

pub(super) struct CollectionFeed {
    source: Source,
    address: SocketAddr,
    task: tokio::task::JoinHandle<()>,
}

impl CollectionFeed {
    pub(super) async fn start(proof: &str) -> io::Result<Self> {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let source = Source {
            proof: Arc::from(proof),
            local: Arc::default(),
            #[cfg(unix)]
            adopted: Arc::default(),
            #[cfg(windows)]
            store: Arc::default(),
        };
        let router = Router::new().route("/dashboard", get(dashboard));
        #[cfg(windows)]
        let router = router
            .route("/sample", get(sample))
            .route("/store/{method}", axum::routing::post(store_call));
        let router = router.with_state(source.clone());
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        Ok(Self {
            source,
            address,
            task,
        })
    }

    pub(super) fn address(&self) -> String {
        format!("http://{}", self.address)
    }

    pub(super) fn set_local(&self, cache: AccountUsageCache) {
        let _ = self.source.local.set(cache);
    }

    #[cfg(unix)]
    pub(super) fn set_adopted(&self, link: super::service_link::ServiceLink) {
        let _ = self.source.adopted.set(link);
    }

    #[cfg(windows)]
    pub(super) fn set_store(&self, host: &Arc<super::owner_host::OwnerHost>) {
        let _ = self.source.store.set(Arc::downgrade(host));
    }
}

impl Drop for CollectionFeed {
    fn drop(&mut self) {
        self.task.abort();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    platform: String,
    secure: bool,
    refresh: bool,
}

fn authorized(source: &Source, headers: &HeaderMap) -> bool {
    headers
        .get("x-aac-collection-proof")
        .is_some_and(|proof| bool::from(source.proof.as_bytes().ct_eq(proof.as_bytes())))
}

/// The remaining Analytics and Status consumers use the public boot's store on Windows.
/// The public router never mounts this private endpoint.
#[cfg(windows)]
async fn store_call(
    State(source): State<Source>,
    axum::extract::Path(method): axum::extract::Path<String>,
    request: axum::extract::Request,
) -> Response {
    if !authorized(&source, request.headers()) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let error = |message: &str| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(serde_json::json!({ "error": message })),
        )
            .into_response()
    };
    if method.is_empty()
        || method.len() > 32
        || !method.bytes().all(|byte| byte.is_ascii_alphanumeric())
        || matches!(
            method.as_str(),
            "open" | "close" | "restore" | "maintain" | "backup"
        )
    {
        return error("invalid: unknown worker store method");
    }
    let Ok(bytes) =
        axum::body::to_bytes(request.into_body(), crate::store_host::STORE_INPUT_CAP).await
    else {
        return error("invalid: store request too large");
    };
    let Ok(args) = String::from_utf8(bytes.to_vec()) else {
        return error("invalid: store request is not text");
    };
    let Some(host) = source.store.get().and_then(std::sync::Weak::upgrade) else {
        return error("service_down: the store host is starting");
    };
    let Some(owners) = host.local().await else {
        return error("service_down: the store host is unavailable");
    };
    let Some(store) = &owners.store else {
        return error("disabled: the store host is disabled");
    };
    match store.call(method, args).await {
        Ok(text) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            text,
        )
            .into_response(),
        Err(reason) => error(&reason),
    }
}

async fn dashboard(
    State(source): State<Source>,
    headers: HeaderMap,
    Query(selection): Query<Selection>,
) -> Response {
    if !authorized(&source, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let desktop = match selection.platform.as_str() {
        "mac" => Desktop::Mac,
        "windows" => Desktop::Windows,
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let target = format!(
        "/api/accounts/dashboard?platform={}&refresh={}",
        selection.platform, selection.refresh
    );
    #[cfg(unix)]
    if let Some(link) = source.adopted.get() {
        let principal = r#"{"kind":"session","id":"collection-worker","role":"admin"}"#;
        let secure = if selection.secure { "1" } else { "0" };
        return match link
            .request(
                "GET",
                &target,
                principal,
                &[(crate::dashboard_fed::SECURE_HEADER, secure)],
                None,
            )
            .await
        {
            Some(reply) if reply.status == 200 => (
                [(axum::http::header::CONTENT_TYPE, "application/json")],
                reply.body,
            )
                .into_response(),
            _ => StatusCode::SERVICE_UNAVAILABLE.into_response(),
        };
    }
    let Some(cache) = source.local.get() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(uri) = target.parse::<Uri>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let empty = HeaderMap::new();
    if let Ok(reply) =
        crate::dashboard_fed::latest_for_access(cache, &uri, &empty, selection.secure)
    {
        return reply;
    }
    if cache
        .await_generation(desktop, selection.secure, Duration::from_secs(12))
        .await
        && let Ok(reply) =
            crate::dashboard_fed::latest_for_access(cache, &uri, &empty, selection.secure)
    {
        return reply;
    }
    StatusCode::SERVICE_UNAVAILABLE.into_response()
}

#[cfg(windows)]
async fn sample(State(source): State<Source>, headers: HeaderMap) -> Response {
    if !authorized(&source, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match source
        .local
        .get()
        .and_then(AccountUsageCache::completed_sample)
    {
        Some(sample) => (
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            sample,
        )
            .into_response(),
        None => StatusCode::SERVICE_UNAVAILABLE.into_response(),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn worker_store_channel_refuses_engine_lifecycle_methods() {
        let proof = "fixture-collection-proof";
        let feed = CollectionFeed::start(proof).await.unwrap();
        let client = reqwest::Client::new();
        let send = |path: &str, supplied: &str| {
            client
                .post(format!("{}{path}", feed.address()))
                .header("x-aac-collection-proof", supplied)
                .body("{}")
                .send()
        };
        assert_eq!(send("/store/status", "wrong").await.unwrap().status(), 401);
        assert_eq!(
            send("/internal/store/status", proof)
                .await
                .unwrap()
                .status(),
            404
        );
        for action in ["open", "close", "restore", "maintain", "backup"] {
            let reply = send(&format!("/store/{action}"), proof).await.unwrap();
            assert_eq!(reply.status(), 500, "{action}");
            assert_eq!(
                reply.text().await.unwrap(),
                r#"{"error":"invalid: unknown worker store method"}"#,
                "{action}"
            );
        }
    }
}
