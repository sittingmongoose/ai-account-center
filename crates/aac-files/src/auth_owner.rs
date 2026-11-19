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
        // A legacy TS listener uses this directory to serialize takeover. Fresh
        // claims still use no-clobber publication without taking this gate.
        let lock = parent.join("sessions.owner.lock");
        fs::create_dir(&lock).map_err(|_| busy())?;
        let _takeover = SessionTakeover(lock);
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

impl Drop for SessionTakeover {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.0);
    }
}

fn busy() -> Failure {
    Failure::new(Cause::Busy, "dashboard auth owner occupied or unverified")
}
