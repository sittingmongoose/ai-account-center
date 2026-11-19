//! The Codex activation lock, without waiting
//! (`src/codex-auth/codex-activation-lock.ts` over proper-lockfile): renewal
//! takes `.ccs-activation.lock` under the Codex home and skips its turn while
//! another holder has it, instead of delaying an activation, sign-in or
//! removal. The on-disk form is proper-lockfile's: a directory whose mtime the
//! holder refreshes every 5 s and that reads as stale after 120 s.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

const STALE_MS: u128 = 120_000;
#[cfg(unix)]
const UPDATE_MS: u64 = 5_000;

/// The lock was held (or could not be taken); renewal skips without waiting.
#[derive(Debug)]
pub struct LockBusy;

/// A held activation lock. Dropping it releases the lock directory.
pub struct ActivationLock {
    lock_path: PathBuf,
    stop: Arc<AtomicBool>,
    heartbeat: Option<std::thread::JoinHandle<()>>,
}

impl Drop for ActivationLock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(handle) = self.heartbeat.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_dir_all(&self.lock_path);
    }
}

fn lock_age_ms(lock_path: &Path) -> Option<u128> {
    let meta = std::fs::metadata(lock_path).ok()?;
    let modified = meta.modified().ok()?;
    Some(SystemTime::now().duration_since(modified).ok()?.as_millis())
}

/// `tryAcquireCodexActivationLock`: mkdir the lock directory; a fresh existing
/// lock means busy; a stale one (mtime older than 120 s) is reclaimed once.
pub fn try_acquire_codex_activation_lock(codex_home: &Path) -> Result<ActivationLock, LockBusy> {
    let lock_path = codex_home.join(".ccs-activation.lock");
    match std::fs::create_dir(&lock_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let stale = lock_age_ms(&lock_path).is_some_and(|age| age > STALE_MS);
            if !stale {
                return Err(LockBusy);
            }
            let _ = std::fs::remove_dir_all(&lock_path);
            std::fs::create_dir(&lock_path).map_err(|_| LockBusy)?;
        }
        Err(_) => return Err(LockBusy),
    }
    let stop = Arc::new(AtomicBool::new(false));
    let heartbeat = spawn_heartbeat(lock_path.clone(), Arc::clone(&stop));
    Ok(ActivationLock {
        lock_path,
        stop,
        heartbeat,
    })
}

/// The mtime heartbeat proper-lockfile runs while it holds the lock. Unix
/// opens the directory and sets its times; Windows cannot open a folder, and
/// the service runs on Linux — short Windows test holds need no heartbeat.
fn spawn_heartbeat(
    lock_path: PathBuf,
    stop: Arc<AtomicBool>,
) -> Option<std::thread::JoinHandle<()>> {
    #[cfg(unix)]
    {
        std::thread::Builder::new()
            .name("aac-renewal-lock".into())
            .spawn(move || {
                let mut last = SystemTime::now();
                while !stop.load(Ordering::Acquire) {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if stop.load(Ordering::Acquire) {
                        break;
                    }
                    if last.elapsed().unwrap_or_default()
                        >= std::time::Duration::from_millis(UPDATE_MS)
                    {
                        if let Ok(dir) = std::fs::File::open(&lock_path) {
                            let now = std::fs::FileTimes::new().set_modified(SystemTime::now());
                            let _ = dir.set_times(now);
                        }
                        last = SystemTime::now();
                    }
                }
            })
            .ok()
    }
    #[cfg(not(unix))]
    {
        let _ = (lock_path, stop);
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fresh_lock_is_busy_and_release_frees_it() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock = try_acquire_codex_activation_lock(dir.path()).expect("first");
        assert!(try_acquire_codex_activation_lock(dir.path()).is_err());
        drop(lock);
        assert!(!dir.path().join(".ccs-activation.lock").exists());
        let _second = try_acquire_codex_activation_lock(dir.path()).expect("second");
    }

    #[test]
    fn a_stale_lock_is_reclaimed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let lock_path = dir.path().join(".ccs-activation.lock");
        std::fs::create_dir(&lock_path).expect("plant");
        let old = SystemTime::now() - std::time::Duration::from_secs(130);
        #[cfg(unix)]
        {
            let file = std::fs::File::open(&lock_path).expect("open dir");
            file.set_times(std::fs::FileTimes::new().set_modified(old))
                .expect("age");
        }
        #[cfg(not(unix))]
        {
            // Without a way to age a folder on Windows, a planted fresh lock
            // stays busy; the stale path is exercised on the Linux gate.
            let _ = old;
            assert!(try_acquire_codex_activation_lock(dir.path()).is_err());
        }
        #[cfg(unix)]
        {
            let _lock = try_acquire_codex_activation_lock(dir.path()).expect("reclaim");
        }
    }
}
