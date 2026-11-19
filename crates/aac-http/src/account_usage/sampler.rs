//! Native sampling loop for the shared dashboard cache.
use super::{AccountUsageCache, Desktop};
use aac_core::UtcMs;
use aac_transport::CancellationToken;
use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio::sync::watch;

/// One completed selector's normalized public dashboard and the Home presentation facts.
pub struct DashboardSample {
    /// Desktop whose quotas and launchers this answer represents.
    pub desktop: Desktop,
    /// Whether the dashboard may advertise secure sign-in actions.
    pub secure: bool,
    /// The dashboard before its request-time reset pass.
    pub dashboard: Value,
    /// The shared `order` and `epochs` data Home uses with it.
    pub facts: Value,
}

/// Start with one collection immediately. After every pass and its downstream publication,
/// wait the configured refresh interval. A setting change replaces the pending wait.
/// The source closure owns the providers' per-account holds and can omit a selector it cannot
/// prove. Omitted or declined selectors keep their preceding generation.
pub async fn run<C, Collect, P, Publish>(
    cache: AccountUsageCache,
    mut interval_ms: watch::Receiver<u64>,
    cancel: CancellationToken,
    mut collect: C,
    mut publish: P,
) where
    C: FnMut(bool) -> Collect,
    Collect: Future<Output = Vec<DashboardSample>>,
    P: FnMut(AccountUsageCache) -> Publish,
    Publish: Future<Output = ()>,
{
    let mut generation = 0_u64;
    let mut forced = false;
    loop {
        if let Ok(now) = UtcMs::now() {
            cache
                .last_refresh
                .fetch_max(now.get(), std::sync::atomic::Ordering::AcqRel);
        }
        let samples = tokio::select! {
            biased;
            () = cancel.cancelled() => return,
            samples = collect(forced) => samples,
        };
        let Some(now) = UtcMs::now().ok().map(UtcMs::get) else {
            return;
        };
        generation = generation.saturating_add(1).max(now.saturating_mul(1000));
        let mut changed = false;
        for sample in samples.into_iter().take(4) {
            changed |= cache
                .publish(
                    sample.desktop,
                    sample.secure,
                    generation,
                    sample.dashboard,
                    sample.facts,
                )
                .unwrap_or(false);
        }
        if changed {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                () = publish(cache.clone()) => {}
            }
        }
        let Some(next) = wait(&mut interval_ms, &cancel, &cache.refresh).await else {
            return;
        };
        forced = next;
    }
}

async fn wait(
    interval_ms: &mut watch::Receiver<u64>,
    cancel: &CancellationToken,
    refresh: &tokio::sync::Notify,
) -> Option<bool> {
    loop {
        let duration =
            Duration::from_millis((*interval_ms.borrow_and_update()).clamp(30_000, 3_600_000));
        tokio::select! {
            biased;
            () = cancel.cancelled() => return None,
            () = refresh.notified() => return Some(true),
            changed = interval_ms.changed() => {
                if changed.is_err() { return None }
            }
            () = tokio::time::sleep(duration) => return Some(false),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use tokio::sync::mpsc;

    #[tokio::test(start_paused = true)]
    async fn starts_immediately_reschedules_and_publishes_a_complete_cache() {
        let (settings, interval) = watch::channel(60_000);
        let cancel = CancellationToken::new();
        let cache = AccountUsageCache::default();
        let (sent, mut reports) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run(
            cache.clone(),
            interval,
            cancel.clone(),
            |_| async {
                vec![DashboardSample {
                    desktop: Desktop::Mac,
                    secure: false,
                    dashboard: json!({"schemaVersion":1, "updatedAt":"", "accounts":[]}),
                    facts: json!({"order":{}, "epochs":{}}),
                }]
            },
            move |cache| {
                let sent = sent.clone();
                async move {
                    sent.send(cache.hub_dashboards("fixture", 0))
                        .expect("report");
                }
            },
        ));
        assert_eq!(reports.recv().await.expect("first sample").len(), 1);
        tokio::time::advance(Duration::from_millis(40_000)).await;
        assert!(reports.try_recv().is_err());
        settings.send(30_000).expect("settings");
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(29_999)).await;
        assert!(reports.try_recv().is_err());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(reports.recv().await.expect("second sample").len(), 1);
        cancel.cancel();
        worker.await.expect("worker stopped");
    }
}
