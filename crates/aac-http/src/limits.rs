//! L1: bounded blocking work. Every route's blocking work (file reads, compositions, the store)
//! runs on a semaphore-bounded pool: 8 permits for reads, 4 for the store, and 4 for fed Status.
//! A full read or store pool is a `busy` decline at once, never a queue. Fed Status waits a few
//! milliseconds for its own slot, and declines `busy` if none frees; the request's deadline still
//! cancels that wait. The permit travels into the blocking task, so a task that outlives its
//! route's deadline still counts until it ends: at most 8, 4 and 4 blocking tasks ever exist,
//! however many requests time out. A request whose future is dropped (its deadline, or the front
//! door closing the connection) cancels its task if it has not started.
use crate::table::Pool;
use aac_views::decline::Decline;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::Duration;
use tokio::sync::Semaphore;

/// Blocking permits for read routes.
pub(crate) const READ_PERMITS: usize = 8;
/// Blocking permits for the store (R3 moves the store host here; today its status read).
pub(crate) const STORE_PERMITS: usize = 4;
/// Blocking permits for fed Status. Its own set, so a full read pool does not turn a cheap Status
/// read into `busy`.
pub(crate) const STATUS_PERMITS: usize = 4;
/// How long a fed Status read waits for one of its slots. A few milliseconds, and never past the
/// request's own deadline: that deadline drops this future, and the wait is inside it.
pub(crate) const STATUS_WAIT: Duration = Duration::from_millis(8);

/// The service's blocking pools.
#[derive(Clone)]
pub(crate) struct Limits {
    reads: Arc<Semaphore>,
    store: Arc<Semaphore>,
    status: Arc<Semaphore>,
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
            status: Arc::new(Semaphore::new(STATUS_PERMITS)),
        }
    }

    /// Run `work` on the blocking pool, or decline `busy` when every permit is taken.
    pub(crate) async fn blocking<T, F>(&self, pool: Pool, work: F) -> Result<T, Decline>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, Decline> + Send + 'static,
    {
        self.blocking_for(pool, Duration::ZERO, work).await
    }

    /// A fed Status read: its own slots, waiting [`STATUS_WAIT`] for one.
    pub(crate) async fn blocking_status<T, F>(&self, work: F) -> Result<T, Decline>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, Decline> + Send + 'static,
    {
        self.blocking_for(Pool::Status, STATUS_WAIT, work).await
    }

    /// Run `work` on `pool`. `wait` of zero declines `busy` at once; a longer wait declines `busy`
    /// when no permit arrives in time. Dropping this future (the request's deadline) cancels the
    /// wait and, if the task has not started, the work.
    pub(crate) async fn blocking_for<T, F>(
        &self,
        pool: Pool,
        wait: Duration,
        work: F,
    ) -> Result<T, Decline>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, Decline> + Send + 'static,
    {
        let semaphore = match pool {
            Pool::Reads => &self.reads,
            Pool::Store => &self.store,
            Pool::Status => &self.status,
        };
        let permit = if wait.is_zero() {
            semaphore
                .clone()
                .try_acquire_owned()
                .map_err(|_| Decline::Busy)?
        } else {
            match tokio::time::timeout(wait, semaphore.clone().acquire_owned()).await {
                Ok(Ok(permit)) => permit,
                _ => return Err(Decline::Busy),
            }
        };
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

    async fn held(limits: &Limits, pool: Pool, n: usize, running: &Arc<AtomicUsize>) {
        for _ in 0..n {
            let (limits, running) = (limits.clone(), running.clone());
            let _join = tokio::spawn(async move {
                let _ = limits
                    .blocking(pool, move || {
                        running.fetch_add(1, Ordering::SeqCst);
                        std::thread::sleep(Duration::from_millis(300));
                        running.fetch_sub(1, Ordering::SeqCst);
                        Ok(())
                    })
                    .await;
            });
        }
        let until = std::time::Instant::now() + Duration::from_secs(2);
        while running.load(Ordering::SeqCst) < n {
            assert!(
                std::time::Instant::now() < until,
                "the permits were not taken"
            );
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_full_read_pool_does_not_make_a_status_read_busy() {
        let limits = Limits::new();
        let running = Arc::new(AtomicUsize::new(0));
        held(&limits, Pool::Reads, READ_PERMITS, &running).await;
        let started = std::time::Instant::now();
        assert_eq!(limits.blocking_status(|| Ok(1)).await, Ok(1));
        assert!(started.elapsed() < Duration::from_millis(200));
        assert!(matches!(
            limits.blocking(Pool::Reads, || Ok(())).await,
            Err(Decline::Busy)
        ));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_status_read_waits_a_few_milliseconds_then_declines_busy() {
        let limits = Limits::new();
        let running = Arc::new(AtomicUsize::new(0));
        held(&limits, Pool::Status, STATUS_PERMITS, &running).await;
        let started = std::time::Instant::now();
        assert_eq!(limits.blocking_status(|| Ok(())).await, Err(Decline::Busy));
        let elapsed = started.elapsed();
        assert!(
            elapsed < Duration::from_millis(150),
            "the wait is a few ms, not the hold: {elapsed:?}"
        );
        // A slot that frees inside the wait is taken: saturation does not have to fall back.
        assert_eq!(
            limits
                .blocking_for(Pool::Status, Duration::from_millis(500), || Ok(7))
                .await,
            Ok(7)
        );
    }
}
