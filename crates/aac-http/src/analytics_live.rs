//! The Analytics collector's live state: whether a scan pass is in flight right now. The writers'
//! job raises it around each activity pass; the read reports it as the answer's `refreshing`,
//! exactly the flag TS read from its activity service's pending pass. The source grid itself comes
//! from the store's own reads (never a second copy of the collector's table).
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// The two flags the writers' job and the read share.
#[derive(Default)]
pub(crate) struct Live {
    scanning: AtomicBool,
    refresh: AtomicBool,
}

impl Live {
    /// A pass is running (the answer is a cached projection while it finishes).
    pub(crate) fn scanning(&self) -> bool {
        self.scanning.load(Ordering::Acquire)
    }

    /// `refresh=true`: the next tick collects activity at once (the page's Refresh).
    pub(crate) fn request_refresh(&self) {
        self.refresh.store(true, Ordering::Release);
    }

    /// Take a pending refresh request; the job answers it on its next tick.
    pub(crate) fn take_refresh(&self) -> bool {
        self.refresh.swap(false, Ordering::AcqRel)
    }

    /// Mark a pass running for the guard's lifetime.
    pub(crate) fn pass(self: &Arc<Self>) -> Pass {
        self.scanning.store(true, Ordering::Release);
        Pass(Arc::clone(self))
    }
}

/// One activity pass's live bit.
pub(crate) struct Pass(Arc<Live>);

impl Drop for Pass {
    fn drop(&mut self) {
        self.0.scanning.store(false, Ordering::Release);
    }
}
