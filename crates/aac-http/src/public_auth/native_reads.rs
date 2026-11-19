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
        let Some(generation) = cache.generation(crate::account_usage::Desktop::Mac, false) else {
            return;
        };
        let dashboards = cache.hub_dashboards(&timestamp, now);
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
