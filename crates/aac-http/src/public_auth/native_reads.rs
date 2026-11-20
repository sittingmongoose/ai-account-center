//! The cut's authenticated access to the remaining store host.
use super::{service_link::ServiceLink, storage};
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
    analytics_writers: bool,
) {
    let Some(timestamp) = storage::iso(now) else {
        return;
    };
    let link = tokio::task::spawn_blocking(move || ServiceLink::load(&root))
        .await
        .ok()
        .flatten();
    let Some(link) = link else {
        return;
    };
    if analytics_writers {
        // The writers' own switch: with the retained worker this repeats its feed; once the
        // worker is gone this is the only producer, so sampling still starts.
        let enable = json!({
            "generation": "native",
            "data": { "enabled": true },
            "facts": {},
        });
        let _ = link
            .request(
                "PUT",
                "/internal/feed/analytics-writers",
                SERVICE,
                &[("content-type", "application/json")],
                Some(enable.to_string().as_bytes()),
            )
            .await;
        let Some(generation) = cache.generation(crate::account_usage::Desktop::Mac, false) else {
            return;
        };
        // Named by platform: the Analytics read needs the requested platform's own dashboard.
        let dashboards = cache.labeled_dashboards(&timestamp, now);
        let body = json!({
            "generation":generation,
            "data": { "dashboards":dashboards },
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
    } else if let Ok(observations) = cache.store_observations(&timestamp, now, i64::MIN)
        && !observations.is_empty()
    {
        let body = json!({"observations": observations});
        let _ = link
            .request(
                "POST",
                "/internal/store/record",
                SERVICE,
                &[("content-type", "application/json")],
                Some(body.to_string().as_bytes()),
            )
            .await;
    }
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
