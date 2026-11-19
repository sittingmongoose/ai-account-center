//! The Codex job owns its unique staging name from prepare through cleanup.
use super::{path, profile_writes::owned_directory, require_absolute, sync_directory};
use std::{
    fs, io,
    os::unix::fs::{DirBuilderExt, PermissionsExt, symlink},
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

fn stage_name(name: &str) -> bool {
    name.strip_prefix(".staging-job_").is_some_and(|id| {
        id.len() == 16
            && id
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

/// The unique staging folder is private before the CLI is allowed to start.
pub fn prepare(root: &Path, name: &str, shared: &Path) -> io::Result<()> {
    owned_directory(root)?;
    if !stage_name(name) || require_absolute(shared).is_err() {
        return Err(io::Error::other("invalid staging layout"));
    }
    let parent = root.join("codex-instances");
    match fs::DirBuilder::new().mode(0o700).create(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    owned_directory(&parent)?;
    let stage = parent.join(name);
    fs::DirBuilder::new().mode(0o700).create(&stage)?;
    fs::set_permissions(&stage, fs::Permissions::from_mode(0o700))?;
    // The unchanged optional config link is owned by this job.
    if fs::metadata(shared).is_ok_and(|value| value.is_file()) {
        let _ = symlink(shared, stage.join("config.toml"));
    }
    let _ = sync_directory(&parent);
    Ok(())
}

fn remove(parent: &Path, name: &str) -> io::Result<bool> {
    if !stage_name(name) {
        return Err(io::Error::other("invalid staging name"));
    }
    let target = parent.join(name);
    match fs::symlink_metadata(&target) {
        Ok(meta) if meta.is_dir() && !path::foreign(&meta) => {
            // remove_dir_all does not follow symlinks inside the owned job directory.
            fs::remove_dir_all(target)?;
            Ok(true)
        }
        Ok(meta) if meta.file_type().is_symlink() => {
            fs::remove_file(target)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
        Ok(_) => Err(io::Error::other("unsafe staging entry")),
    }
}

/// Job cleanup is idempotent after a successful promotion.
pub fn cleanup(root: &Path, name: &str) -> io::Result<()> {
    let parent = root.join("codex-instances");
    owned_directory(&parent)?;
    remove(&parent, name).map(|_| ())
}

/// Sweep only expired staging directories. Live profiles and symlinks stay in place.
pub fn sweep(root: &Path, now_ms: u64) -> io::Result<usize> {
    let parent = root.join("codex-instances");
    match owned_directory(&parent) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
        Ok(()) => {}
    }
    let cutoff = Duration::from_millis(now_ms).saturating_sub(Duration::from_secs(3600));
    let mut removed = 0;
    for entry in fs::read_dir(&parent)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| stage_name(name)) else {
            continue;
        };
        let metadata = fs::symlink_metadata(entry.path());
        if !metadata.is_ok_and(|metadata| {
            metadata.is_dir()
                && !path::foreign(&metadata)
                && metadata
                    .modified()
                    .ok()
                    .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                    .is_some_and(|age| age <= cutoff)
        }) {
            continue;
        }
        if remove(&parent, name).unwrap_or(false) {
            removed += 1;
        }
    }
    Ok(removed)
}
