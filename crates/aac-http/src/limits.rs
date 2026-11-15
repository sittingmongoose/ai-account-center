//! L1: bounded blocking work. Every route's blocking work (file reads, compositions, the store)
//! runs on one of two semaphore-bounded pools: 8 permits for reads and 4 for the store. A full
//! pool is a `busy` decline at once, never a queue. The permit travels into the blocking task, so
//! a task that outlives its route's deadline still counts until it ends: at most 8 (or 4) blocking
//! tasks ever exist, however many requests time out. A request whose future is dropped (its
//! deadline, or the front door closing the connection) cancels its task if it has not started.
use crate::table::Pool;
use aac_views::decline::Decline;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::Semaphore;

/// Blocking permits for read routes.
pub(crate) const READ_PERMITS: usize = 8;
/// Blocking permits for the store (R3 moves the store host here; today its status read).
pub(crate) const STORE_PERMITS: usize = 4;

/// The service's two blocking pools.
#[derive(Clone)]
pub(crate) struct Limits {
    reads: Arc<Semaphore>,
    store: Arc<Semaphore>,
}

/// Marks the task cancelled when the request's future is dropped before it ends.
struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

impl Limits {
    pub(crate) fn new() -> Self {
        Self {
            reads: Arc::new(Semaphore::new(READ_PERMITS)),
            store: Arc::new(Semaphore::new(STORE_PERMITS)),
        }
    }

    /// Run `work` on the blocking pool, or decline `busy` when every permit is taken.
    pub(crate) async fn blocking<T, F>(&self, pool: Pool, work: F) -> Result<T, Decline>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, Decline> + Send + 'static,
    {
        let semaphore = match pool {
            Pool::Reads => &self.reads,
            Pool::Store => &self.store,
        };
        let permit = semaphore
            .clone()
            .try_acquire_owned()
            .map_err(|_| Decline::Busy)?;
        let cancelled = Arc::new(AtomicBool::new(false));
        let flag = cancelled.clone();
        let _cancel = CancelOnDrop(cancelled);
        let task = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if flag.load(Ordering::Acquire) {
                return Err(Decline::Deadline);
            }
            work()
        });
        task.await.unwrap_or(Err(Decline::Internal))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::atomic::AtomicUsize, time::Duration};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_pool_declines_busy_and_never_runs_more_than_its_permits() {
        let limits = Limits::new();
        let running = Arc::new(AtomicUsize::new(0));
        let peak = Arc::new(AtomicUsize::new(0));
        let mut calls = Vec::new();
        for _ in 0..20 {
            let (limits, running, peak) = (limits.clone(), running.clone(), peak.clone());
            calls.push(tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_millis(100),
                    limits.blocking(Pool::Reads, move || {
                        let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                        peak.fetch_max(now, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(400));
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    }),
                )
                .await
            }));
        }
        let (mut busy, mut deadline, mut other) = (0, 0, 0);
        for call in calls {
            match call.await {
                Ok(Ok(Err(Decline::Busy))) => busy += 1,
                Ok(Err(_elapsed)) => deadline += 1,
                _ => other += 1,
            }
        }
        assert_eq!(busy, 20 - READ_PERMITS, "the rest decline at once");
        assert_eq!(deadline, READ_PERMITS, "the admitted ones hit the deadline");
        assert_eq!(other, 0);
        // The timed-out tasks still hold their permits: the pool stays full until they end.
        assert!(matches!(
            limits.blocking(Pool::Reads, || Ok(())).await,
            Err(Decline::Busy)
        ));
        tokio::time::sleep(Duration::from_millis(600)).await;
        assert!(limits.blocking(Pool::Reads, || Ok(())).await.is_ok());
        assert_eq!(peak.load(Ordering::SeqCst), READ_PERMITS);
    }

    #[test]
    fn a_request_dropped_before_its_task_starts_cancels_the_work() -> std::io::Result<()> {
        // One blocking thread, held busy, so the request's task waits in the queue.
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .max_blocking_threads(1)
            .enable_all()
            .build()?;
        let ran = Arc::new(AtomicUsize::new(0));
        let counter = ran.clone();
        runtime.block_on(async move {
            let limits = Limits::new();
            let holder = tokio::task::spawn_blocking(|| {
                std::thread::sleep(Duration::from_millis(300));
            });
            tokio::time::sleep(Duration::from_millis(20)).await;
            let request = limits.blocking(Pool::Store, move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(())
            });
            // The deadline drops the request while its task is still queued.
            assert!(
                tokio::time::timeout(Duration::from_millis(50), request)
                    .await
                    .is_err()
            );
            let _ = holder.await;
            tokio::time::sleep(Duration::from_millis(50)).await;
            // Its permit came back once the queued task saw the cancel and ended.
            for _ in 0..STORE_PERMITS {
                assert!(limits.blocking(Pool::Store, || Ok(())).await.is_ok());
            }
        });
        assert_eq!(ran.load(Ordering::SeqCst), 0, "cancelled work never ran");
        Ok(())
    }
}
