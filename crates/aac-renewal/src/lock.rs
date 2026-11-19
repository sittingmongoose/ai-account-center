//! Use the shared provider activation lease, without delaying an interactive transaction.

use std::path::Path;

pub use aac_files::profile_writes::CodexLease as ActivationLock;

/// The lock was held or could not be taken. Renewal skips without waiting.
#[derive(Debug)]
pub struct LockBusy;

/// Take the same activation lock used by the dashboard and Codex CLI.
pub fn try_acquire_codex_activation_lock(codex_home: &Path) -> Result<ActivationLock, LockBusy> {
    ActivationLock::try_activation(codex_home).map_err(|_| LockBusy)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

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
