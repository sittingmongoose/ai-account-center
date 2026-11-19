use std::{
    collections::HashMap,
    fs::TryLockError,
    future::Future,
    path::PathBuf,
    sync::{Arc, Mutex, Weak},
    time::Duration,
};

/// Per-boot FIFO for the workflows that touch shared key or Claude trash
/// state. The lease also excludes an older front door during handoff.
pub struct LifecycleQueues {
    root: PathBuf,
    active: Mutex<HashMap<String, Weak<tokio::sync::Mutex<()>>>>,
}

impl LifecycleQueues {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            active: Mutex::default(),
        }
    }

    pub async fn run<T>(
        &self,
        scope: &str,
        effect: impl Future<Output = T>,
    ) -> Result<T, aac_core::Failure> {
        let file = aac_files::lifecycle_queue::open(&self.root, scope)?;
        let queue = {
            let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
            active.retain(|_, queue| queue.strong_count() > 0);
            if let Some(queue) = active.get(scope).and_then(Weak::upgrade) {
                queue
            } else {
                let queue = Arc::new(tokio::sync::Mutex::new(()));
                active.insert(scope.to_owned(), Arc::downgrade(&queue));
                queue
            }
        };
        let _turn = queue.lock().await;
        loop {
            match file.try_lock() {
                Ok(()) => break,
                Err(TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(TryLockError::Error(error)) => return Err(error.into()),
            }
        }
        let result = effect.await;
        drop(file);
        Ok(result)
    }
}
