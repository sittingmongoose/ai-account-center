//! The cut's authenticated access to the remaining store host.
use super::service_link::ServiceLink;
use serde_json::json;
use std::path::PathBuf;

/// The front door's service principal word (`VerifiedPrincipal::valid`).
const SERVICE: &str = r#"{"kind":"session","id":"front-door","role":"service"}"#;

/// Forward a completed native generation to the existing store owner. The G14
/// sampler consumes its regular feed and keeps its history import order.
pub(super) async fn publish_usage(
    root: PathBuf,
    cache: &crate::account_usage::AccountUsageCache,
    now: i64,
) {
    let link = tokio::task::spawn_blocking(move || ServiceLink::load(&root))
        .await
        .ok()
        .flatten();
    let Some(link) = link else {
        return;
    };
    let Some(snapshot) = crate::analytics_route::snapshot(cache, now) else {
        return;
    };
    let body = json!({
        "generation": now.to_string(),
        "data": snapshot,
        "facts": {},
    });
    let _ = link
        .request(
            "PUT",
            "/internal/feed/analytics-dashboard",
            SERVICE,
            &[("content-type", "application/json")],
            Some(body.to_string().as_bytes()),
        )
        .await;
}
/// One pushed usage sample: the service's share writer takes it on its own blocking task.
pub(super) async fn usage_sample(root: PathBuf, payload: serde_json::Value) -> bool {
    let link = tokio::task::spawn_blocking(move || ServiceLink::load(&root))
        .await
        .ok()
        .flatten();
    let Some(link) = link else {
        return false;
    };
    link.request(
        "POST",
        "/internal/usage-snapshot/sample",
        SERVICE,
        &[("content-type", "application/json")],
        Some(payload.to_string().as_bytes()),
    )
    .await
    .is_some_and(|reply| (200..300).contains(&reply.status))
}
