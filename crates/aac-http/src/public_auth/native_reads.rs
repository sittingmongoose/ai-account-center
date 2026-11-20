//! The native sampler sends writes to the same private generation that serves its reads.
use super::owner_host::OwnerHost;
use serde_json::json;

/// The front door's service principal word (`VerifiedPrincipal::valid`).
const SERVICE: &str = r#"{"kind":"session","id":"front-door","role":"service"}"#;

/// Forward a completed native generation to the existing store owner. The G14
/// sampler consumes its regular feed and keeps its history import order.
pub(super) async fn publish_usage(
    host: &OwnerHost,
    cache: &crate::account_usage::AccountUsageCache,
    now: i64,
) {
    let Some(snapshot) = crate::analytics_route::snapshot(cache, now) else {
        return;
    };
    let body = json!({
        "generation": now.to_string(),
        "data": snapshot,
        "facts": {},
    });
    let _ = host
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
pub(super) async fn usage_sample(host: &OwnerHost, payload: serde_json::Value) -> bool {
    host.request(
        "POST",
        "/internal/usage-snapshot/sample",
        SERVICE,
        &[("content-type", "application/json")],
        Some(payload.to_string().as_bytes()),
    )
    .await
    .is_some_and(|reply| (200..300).contains(&reply.status))
}
