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
        };
        let router = Router::new().route("/dashboard", get(dashboard));
        #[cfg(windows)]
        let router = router.route("/sample", get(sample));
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
