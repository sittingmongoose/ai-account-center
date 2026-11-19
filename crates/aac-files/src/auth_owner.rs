//! The dashboard session ownership marker shared with the legacy TypeScript listener.

use crate::process;
use crate::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use aac_core::{Cause, Failure};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const SESSION_OWNER: &str = "auth/sessions.owner";

/// The shared lifetime auth claim. The public TS listener also takes it before
/// loading or writing persisted auth state.
pub struct SessionOwner {
    path: PathBuf,
    claim: Vec<u8>,
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
        let identity = process::host_boot();
        let host = identity.as_ref().map(|(host, _)| host);
        let boot = identity.as_ref().map(|(_, boot)| boot);
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
                pending.persist_noclobber(&path).map_err(|_| busy())?;
                return Ok(Self { path, claim });
            }
            Some(held) => held,
        };
        if !Self::proven_dead(&held, identity.as_ref()) {
            return Err(busy());
        }
        // A legacy TS listener uses this directory to serialize takeover. Fresh
        // claims still use no-clobber publication without taking this gate.
        let lock = parent.join("sessions.owner.lock");
        fs::create_dir(&lock).map_err(|_| busy())?;
        let _takeover = SessionTakeover(lock);
        let again = Self::read(root, &path)?.ok_or_else(busy)?;
        if again != held || !Self::proven_dead(&again, identity.as_ref()) {
            return Err(busy());
        }
        pending.persist(&path).map_err(|_| busy())?;
        Ok(Self { path, claim })
    }
    fn read(root: &Path, path: &Path) -> Result<Option<Vec<u8>>, Failure> {
        let policy = ReadPolicy::capped(4096)
            .ancestors(Ancestors::NoLinkBelow(root))
            .mode(Mode::Private)
            .owner(Owner::Mine);
        match crate::read::read(path, &policy) {
            Read::Bytes(bytes, _) => Ok(Some(bytes)),
            Read::Absent => Ok(None),
            _ => Err(busy()),
        }
    }

    fn proven_dead(held: &[u8], identity: Option<&(String, u64)>) -> bool {
        #[derive(Deserialize)]
        struct LegacyOwner {
            pid: u32,
            host: String,
            boot: u64,
        }
        let (Some((host, boot)), Ok(owner)) =
            (identity, serde_json::from_slice::<LegacyOwner>(held))
        else {
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

impl Drop for SessionOwner {
    fn drop(&mut self) {
        if crate::read_bounded(&self.path, 4096).is_ok_and(|bytes| bytes == self.claim) {
            let _ = fs::remove_file(&self.path);
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
