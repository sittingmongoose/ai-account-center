//! The dashboard session ownership marker shared with the legacy TypeScript listener.

use crate::process;
use crate::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use aac_core::{Cause, Failure};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime},
};

const SESSION_OWNER: &str = "auth/sessions.owner";
const HEARTBEAT: Duration = Duration::from_secs(15);
const LEGACY_STALE: Duration = Duration::from_secs(3600);

/// The shared lifetime auth claim. The public TS listener also takes it before
/// loading or writing persisted auth state.
pub struct SessionOwner {
    claim: Arc<Claim>,
    stop: mpsc::Sender<()>,
    heartbeat: Option<JoinHandle<()>>,
}

struct Claim {
    root: PathBuf,
    path: PathBuf,
    bytes: Vec<u8>,
    lost: AtomicBool,
}

impl SessionOwner {
    /// Publish a complete claim atomically, reclaiming a verified dead owner.
    /// The caller has already established the private auth directory.
    pub fn take(root: &Path) -> Result<Self, Failure> {
        let path = root.join(SESSION_OWNER);
        let parent = path.parent().ok_or_else(busy)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = fs::symlink_metadata(parent)?;
            if !metadata.is_dir() || metadata.mode() & 0o022 != 0 {
                return Err(Failure::new(
                    Cause::Refused,
                    "unsafe dashboard auth directory",
                ));
            }
        }
        let mut pending = tempfile::Builder::new()
            .prefix(".sessions-owner-")
            .tempfile_in(parent)?;
        // The pre-cut predicate always reclaims a claim with missing identity.
        let identity = process::host_boot()
            .filter(|(_, boot)| *boot > 0)
            .ok_or_else(busy)?;
        let (host, boot) = &identity;
        let claim = serde_json::to_vec(&json!({
            "pid": std::process::id(),
            "host": host,
            "boot": boot,
            "writer": "rust",
            "token": pending.path().file_name().ok_or_else(busy)?.to_string_lossy(),
        }))
        .map_err(|_| busy())?;
        pending.write_all(&claim)?;
        pending.as_file().sync_all()?;
        let held = match Self::read(root, &path)? {
            None => {
                let file = pending.persist_noclobber(&path).map_err(|_| busy())?;
                return Self::start(root, path, claim, file);
            }
            Some(held) => held,
        };
        if !Self::proven_dead(&held, &identity) {
            return Err(busy());
        }
        // New reclaimers serialize on the kernel lease. Keep the old directory
        // protocol during replacement so pre-cut reclaimers still see it.
        let lease = crate::open_lock(&parent.join("sessions.owner.takeover.lock"))?;
        fs2::FileExt::try_lock_exclusive(&lease).map_err(|_| busy())?;
        let _compatibility_gate = SessionTakeover::take(parent.join("sessions.owner.lock"))?;
        let again = Self::read(root, &path)?.ok_or_else(busy)?;
        if again != held || !Self::proven_dead(&again, &identity) {
            return Err(busy());
        }
        let file = pending.persist(&path).map_err(|_| busy())?;
        Self::start(root, path, claim, file)
    }

    fn start(root: &Path, path: PathBuf, bytes: Vec<u8>, file: File) -> Result<Self, Failure> {
        let claim = Arc::new(Claim {
            root: root.to_owned(),
            path,
            bytes,
            lost: AtomicBool::new(false),
        });
        let (stop, stopped) = mpsc::channel();
        let beat = Arc::clone(&claim);
        let heartbeat = thread::Builder::new()
            .name("aac-session-owner".into())
            .spawn(move || {
                while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(HEARTBEAT) {
                    if beat.refresh(&file).is_err() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            claim,
            stop,
            heartbeat: Some(heartbeat),
        })
    }

    /// Confirm that the listener still owns a fresh claim before admitting or writing a session.
    pub fn verify(&self) -> Result<(), Failure> {
        self.claim.verify()
    }

    fn read(root: &Path, path: &Path) -> Result<Option<Vec<u8>>, Failure> {
        Ok(Self::read_claim(root, path)?.map(|(bytes, _)| bytes))
    }

    fn read_claim(
        root: &Path,
        path: &Path,
    ) -> Result<Option<(Vec<u8>, crate::read::Meta)>, Failure> {
        let policy = ReadPolicy::capped(4096)
            .ancestors(Ancestors::NoLinkBelow(root))
            .mode(Mode::Private)
            .owner(Owner::Mine);
        match crate::read::read(path, &policy) {
            Read::Bytes(bytes, metadata) => Ok(Some((bytes, *metadata))),
            Read::Absent => Ok(None),
            _ => Err(busy()),
        }
    }

    fn proven_dead(held: &[u8], (host, boot): &(String, u64)) -> bool {
        #[derive(Deserialize)]
        struct LegacyOwner {
            pid: u32,
            host: String,
            boot: u64,
        }
        let Ok(owner) = serde_json::from_slice::<LegacyOwner>(held) else {
            return false;
        };
        if owner.host != *host
            || owner.pid == 0
            || i32::try_from(owner.pid).is_err()
            || owner.boot == 0
        {
            return false;
        }
        owner.boot.saturating_add(2) < *boot
            || (owner.boot.abs_diff(*boot) <= 2 && process::is_dead(owner.pid))
    }
}

impl Claim {
    fn verify(&self) -> Result<(), Failure> {
        if self.lost.load(Ordering::Acquire) {
            return Err(busy());
        }
        let ours = SessionOwner::read_claim(&self.root, &self.path).is_ok_and(|entry| {
            entry.is_some_and(|(bytes, metadata)| {
                bytes == self.bytes
                    && metadata.modified().is_ok_and(|modified| {
                        SystemTime::now()
                            .duration_since(modified)
                            .unwrap_or_default()
                            < LEGACY_STALE - HEARTBEAT
                    })
            })
        });
        if !ours {
            self.lost.store(true, Ordering::Release);
            return Err(busy());
        }
        Ok(())
    }

    fn refresh(&self, file: &File) -> Result<(), Failure> {
        self.verify()?;
        // Touch the original descriptor. A replacement at this path never gets our heartbeat.
        if file.set_modified(SystemTime::now()).is_err() {
            self.lost.store(true, Ordering::Release);
            return Err(busy());
        }
        self.verify()
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.verify().is_ok() {
            let _ = fs::remove_file(&self.path);
        }
    }
}

impl Drop for SessionOwner {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(heartbeat) = self.heartbeat.take() {
            let _ = heartbeat.join();
        }
    }
}

struct SessionTakeover(PathBuf);

impl SessionTakeover {
    fn take(path: PathBuf) -> Result<Self, Failure> {
        match Self::create(&path) {
            Ok(()) => return Ok(Self(path)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }

        // Pre-cut listeners made an empty directory without an owner record.
        // Keep their one-minute grace period and only remove the observed empty directory.
        let held = fs::symlink_metadata(&path)?;
        if !Self::reclaimable(&held) {
            return Err(busy());
        }
        let again = fs::symlink_metadata(&path)?;
        if !Self::reclaimable(&again)
            || !crate::same_file(&held, &again)
            || held.modified()? != again.modified()?
        {
            return Err(busy());
        }
        fs::remove_dir(&path).map_err(|_| busy())?;
        Self::create(&path).map_err(|_| busy())?;
        Ok(Self(path))
    }

    fn create(path: &Path) -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::DirBuilderExt;
            // The sticky bit distinguishes a gate created under the kernel lease
            // from an old mkdir-only gate. It carries no state of its own.
            fs::DirBuilder::new().mode(0o1700).create(path)
        }
        #[cfg(not(target_os = "linux"))]
        fs::create_dir(path)
    }

    fn reclaimable(metadata: &fs::Metadata) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            if metadata.uid() != crate::path::uid() {
                return false;
            }
            #[cfg(target_os = "linux")]
            if metadata.mode() & 0o1000 != 0 {
                return metadata.is_dir();
            }
        }
        #[cfg(windows)]
        if crate::windows_reparse_point(metadata) {
            return false;
        }
        metadata.is_dir()
            && metadata
                .modified()
                .and_then(|modified| modified.elapsed().map_err(std::io::Error::other))
                .is_ok_and(|age| age > Duration::from_secs(60))
    }
}

impl Drop for SessionTakeover {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn busy() -> Failure {
    Failure::new(Cause::Busy, "dashboard auth owner occupied or unverified")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static BOOT_READS: AtomicU32 = AtomicU32::new(0);

    /// The claim's boot minute at acquisition, then the next minute: the drift a fresh
    /// estimate takes when uptime's whole seconds cross a rounding boundary.
    fn drifting_boot() -> Option<(String, u64)> {
        let read = BOOT_READS.fetch_add(1, Ordering::SeqCst);
        Some(("owner-boot-test".into(), 1000 + u64::from(read > 0)))
    }

    struct RestoreSource;

    impl Drop for RestoreSource {
        fn drop(&mut self) {
            process::set_test_boot_source(None);
        }
    }

    fn fixture_root() -> tempfile::TempDir {
        let root = tempfile::tempdir().expect("fixture root");
        let auth = root.path().join("auth");
        fs::create_dir_all(&auth).expect("auth folder");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // A private group umask must not make the fixture's auth folder look unsafe.
            fs::set_permissions(&auth, fs::Permissions::from_mode(0o700)).expect("auth mode");
        }
        root
    }

    #[test]
    fn samples_the_boot_identity_once_so_a_later_minute_never_makes_the_owner_stale() {
        let _restore = RestoreSource;
        BOOT_READS.store(0, Ordering::SeqCst);
        process::set_test_boot_source(Some(drifting_boot));
        let root = fixture_root();
        let marker = root.path().join(SESSION_OWNER);
        let owner = SessionOwner::take(root.path()).expect("claim taken");
        let claim = fs::read(&marker).expect("claim written");
        let written: serde_json::Value = serde_json::from_slice(&claim).expect("claim json");
        assert_eq!(
            written.get("boot").and_then(serde_json::Value::as_u64),
            Some(1000)
        );
        assert_eq!(
            BOOT_READS.load(Ordering::SeqCst),
            1,
            "the identity is sampled once, at acquisition"
        );
        // A fresh estimate now rounds to the next minute. The reference store took its own
        // claim for another process's and dropped every sign-in; the stored bytes must stand.
        for _ in 0..3 {
            owner.verify().expect("the claim is still ours");
        }
        assert_eq!(
            BOOT_READS.load(Ordering::SeqCst),
            1,
            "verification never re-measures the boot identity"
        );
        assert_eq!(fs::read(&marker).expect("claim intact"), claim);
        drop(owner);
        assert!(!marker.exists(), "the owner removes its own claim");
    }
}
