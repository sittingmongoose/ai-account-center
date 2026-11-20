//! The Analytics collector's live state: whether a scan pass is in flight right now. The writers'
//! job raises it around each activity pass; the read reports it as the answer's `refreshing`,
//! exactly the flag TS read from its activity service's pending pass. The source grid itself comes
//! from the store's own reads (never a second copy of the collector's table).
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// The two flags the writers' job and the read share.
#[derive(Default)]
pub(crate) struct Live {
    scanning: AtomicBool,
    refresh_gen: AtomicU64,
    answered_gen: AtomicU64,
}

impl Live {
    /// A pass is running (the answer is a cached projection while it finishes).
    pub(crate) fn scanning(&self) -> bool {
        self.scanning.load(Ordering::Acquire)
    }

    /// Queued Refresh or a pass that has not finished: later tabs keep polling.
    pub(crate) fn refreshing(&self) -> bool {
        self.scanning() || self.pending_refresh()
    }

    fn pending_refresh(&self) -> bool {
        self.refresh_gen.load(Ordering::Acquire) > self.answered_gen.load(Ordering::Acquire)
    }

    /// `refresh=true`: the next tick collects activity at once (the page's Refresh).
    pub(crate) fn request_refresh(&self) {
        self.refresh_gen.fetch_add(1, Ordering::AcqRel);
    }

    /// A pending refresh request; the job answers it on its next tick without hiding it.
    pub(crate) fn take_refresh(&self) -> bool {
        self.pending_refresh()
    }

    /// Mark a pass running for the guard's lifetime. Dropping it completes a queued Refresh.
    pub(crate) fn pass(self: &Arc<Self>) -> Pass {
        self.scanning.store(true, Ordering::Release);
        Pass {
            live: Arc::clone(self),
            answered: self.refresh_gen.load(Ordering::Acquire),
        }
    }
}

/// One activity pass's live bit.
pub(crate) struct Pass {
    live: Arc<Live>,
    answered: u64,
}

impl Drop for Pass {
    fn drop(&mut self) {
        self.live
            .answered_gen
            .store(self.answered, Ordering::Release);
        self.live.scanning.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pending_refresh_stays_queued_until_the_pass_completes() {
        let live = Arc::new(Live::default());
        assert!(!live.refreshing());
        live.request_refresh();
        assert!(live.refreshing());
        assert!(live.take_refresh());
        assert!(
            live.refreshing(),
            "queued work stays visible after the writer takes it"
        );
        let pass = live.pass();
        assert!(live.refreshing());
        drop(pass);
        assert!(!live.refreshing());
        assert!(!live.take_refresh());
    }

    #[test]
    fn a_refresh_requested_during_a_pass_stays_queued() {
        let live = Arc::new(Live::default());
        live.request_refresh();
        let pass = live.pass();
        live.request_refresh();
        drop(pass);
        assert!(live.refreshing());
        drop(live.pass());
        assert!(!live.refreshing());
    }
}
