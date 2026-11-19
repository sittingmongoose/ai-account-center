//! The Codex job owns its unique staging name from prepare through cleanup.
use super::{
    path,
    profile_writes::{file_key, open_leaf, owned_directory},
    require_absolute, sync_directory,
};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink as symlink_file};
#[cfg(windows)]
use std::os::windows::fs::symlink_file;
use std::{
    fs,
    io::{self, Write},
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
    match super::private_builder().create(&parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    owned_directory(&parent)?;
    let stage = parent.join(name);
    private_folder(&stage)?;
    // The unchanged optional config link is owned by this job.
    if fs::metadata(shared).is_ok_and(|value| value.is_file()) {
        let _ = symlink_file(shared, stage.join("config.toml"));
    }
    let _ = sync_directory(&parent);
    Ok(())
}

fn remove(parent: &Path, name: &str, valid: fn(&str) -> bool) -> io::Result<bool> {
    if !valid(name) {
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
    remove(&parent, name, stage_name).map(|_| ())
}

/// Sweep only expired staging directories. Live profiles and symlinks stay in place.
pub fn sweep(root: &Path, now_ms: u64) -> io::Result<usize> {
    sweep_expired(&root.join("codex-instances"), now_ms, stage_name)
}

fn sweep_expired(parent: &Path, now_ms: u64, valid: fn(&str) -> bool) -> io::Result<usize> {
    match owned_directory(parent) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(error),
        Ok(()) => {}
    }
    let cutoff = Duration::from_millis(now_ms).saturating_sub(Duration::from_secs(3600));
    let mut removed = 0;
    for entry in fs::read_dir(parent)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str().filter(|name| valid(name)) else {
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
        if remove(parent, name, valid).unwrap_or(false) {
            removed += 1;
        }
    }
    Ok(removed)
}

fn antigravity_name(name: &str) -> bool {
    name.strip_prefix(".staging-").is_some_and(|id| {
        id.len() == 16
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn private_folder(target: &Path) -> io::Result<()> {
    super::private_builder().create(target)?;
    #[cfg(unix)]
    fs::set_permissions(target, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

/// Prepare the common terminal and dashboard sign-in directory.
pub fn antigravity_root(root: &Path) -> io::Result<()> {
    owned_directory(root)?;
    let parent = root.join("antigravity-signin");
    match private_folder(&parent) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => owned_directory(&parent)?,
        other => other?,
    }
    #[cfg(unix)]
    if fs::symlink_metadata(parent)?.mode() & 0o077 != 0 {
        return Err(io::Error::other("sign-in directory is not private"));
    }
    Ok(())
}

fn create_file(path: &Path, content: &[u8], mode: u32) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(mode);
    #[cfg(windows)]
    let _ = mode;
    let mut file = options.open(path)?;
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(content)?;
    file.sync_all()
}

/// Create the isolated Antigravity home and fixed browser stubs, never a login file.
pub fn prepare_antigravity(root: &Path, name: &str, bus: &[u8]) -> io::Result<()> {
    let parent = root.join("antigravity-signin");
    owned_directory(&parent)?;
    if !antigravity_name(name) || bus.is_empty() || bus.len() > 4096 {
        return Err(io::Error::other("invalid staging layout"));
    }
    let stage = parent.join(name);
    private_folder(&stage)?;
    let home = stage.join("home");
    let mask = home.join(".gemini");
    let cli = mask.join("antigravity-cli");
    let stubs = stage.join("browser-stubs");
    let create = || -> io::Result<()> {
        for path in [
            &home,
            &mask,
            &cli,
            &cli.join("log"),
            &mask.join("AAC-signin"),
            &stubs,
        ] {
            private_folder(path)?;
        }
        for name in [
            "xdg-open",
            "open",
            "sensible-browser",
            "x-www-browser",
            "www-browser",
        ] {
            create_file(&stubs.join(name), b"#!/bin/sh\\nexit 0\\n", 0o700)?;
        }
        create_file(&stage.join("session-bus.conf"), bus, 0o600)
    };
    if let Err(error) = create() {
        let _ = remove(&parent, name, antigravity_name);
        return Err(error);
    }
    Ok(())
}

/// Remove only the Antigravity staging folder named by this job.
pub fn cleanup_antigravity(root: &Path, name: &str) -> io::Result<()> {
    let parent = root.join("antigravity-signin");
    owned_directory(&parent)?;
    remove(&parent, name, antigravity_name).map(|_| ())
}

/// Expire only abandoned Antigravity staging folders.
pub fn sweep_antigravity(root: &Path, now_ms: u64) -> io::Result<usize> {
    sweep_expired(&root.join("antigravity-signin"), now_ms, antigravity_name)
}

fn marker_name(name: &str) -> bool {
    name.strip_suffix(".running").is_some_and(|profile| {
        profile
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_lowercase)
            && profile.len() <= 48
            && profile.bytes().all(|byte| {
                byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte)
            })
    })
}

fn marker_temp(name: &str) -> bool {
    name.strip_prefix(".marker-").is_some_and(|id| {
        id.len() == 16
            && id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

/// Install a complete marker by exclusive hard link, refusing a competing claim.
pub fn claim_antigravity_marker(
    root: &Path,
    name: &str,
    temp: &str,
    holder: &[u8],
) -> io::Result<bool> {
    let parent = root.join("antigravity-signin");
    owned_directory(&parent)?;
    if !marker_name(name) || !marker_temp(temp) || holder.is_empty() || holder.len() > 512 {
        return Err(io::Error::other("invalid sign-in marker"));
    }
    let path = parent.join(temp);
    let mut options = fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&path)?;
    let result = (|| {
        super::set_private(&file).map_err(|_| io::Error::other("sign-in marker is not private"))?;
        file.write_all(holder)?;
        file.sync_all()?;
        match fs::hard_link(&path, parent.join(name)) {
            Ok(()) => Ok(true),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
            Err(error) => Err(error),
        }
    })();
    let _ = fs::remove_file(path);
    let _ = sync_directory(&parent);
    result
}

/// Remove only the previously judged marker inode, never an intervening claim.
pub fn remove_antigravity_marker(root: &Path, name: &str, dev: u64, ino: u64) -> io::Result<bool> {
    let parent = root.join("antigravity-signin");
    owned_directory(&parent)?;
    if !marker_name(name) && !marker_temp(name) {
        return Err(io::Error::other("invalid sign-in marker"));
    }
    let file = parent.join(name);
    match marker_identity(&file) {
        Ok(key) if key == (dev, ino) => {
            fs::remove_file(file)?;
            let _ = sync_directory(&parent);
            Ok(true)
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
        _ => Ok(false),
    }
}

/// The current marker identity, without following links.
pub fn marker_identity(path: &Path) -> io::Result<(u64, u64)> {
    let (device, inode, _) = file_key(&open_leaf(path, false)?)?;
    Ok((device, inode))
}

/// Validate and read the complete staged login without accepting linked credentials.
pub fn antigravity_token(root: &Path, staging_name: &str) -> Option<Vec<u8>> {
    if !antigravity_name(staging_name) {
        return None;
    }
    let path = root
        .join("antigravity-signin")
        .join(staging_name)
        .join("home/.gemini/antigravity-cli/antigravity-oauth-token");
    let policy = super::read::ReadPolicy::capped(16_384)
        .mode(super::read::Mode::Private)
        .owner(super::read::Owner::Mine)
        .ancestors(super::read::Ancestors::NoLinkBelow(root));
    let held = open_leaf(&path, false).ok()?;
    let files = file_key(&held).ok()?;
    if files.2 != 1 {
        return None;
    }
    let super::read::Read::Bytes(bytes, _metadata) = super::read::read(&path, &policy) else {
        return None;
    };
    if bytes.is_empty() || file_key(&open_leaf(&path, false).ok()?).ok()? != files {
        return None;
    }
    #[cfg(unix)]
    if _metadata.dev() != files.0 || _metadata.ino() != files.1 || _metadata.mode() & 0o777 != 0o600
    {
        return None;
    }
    Some(bytes)
}

/// Sweep expired Antigravity marker drafts and markers whose holder is gone or
/// silent. The caller supplies the provider's holder-record decoder.
pub fn sweep_antigravity_markers(
    root: &Path,
    now_ms: u64,
    mut holder_stale: impl FnMut(&[u8], u64) -> Option<bool>,
) -> io::Result<usize> {
    let parent = root.join("antigravity-signin");
    owned_directory(&parent)?;
    let mut count = 0;
    for entry in fs::read_dir(&parent)?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        let Ok(meta) = fs::symlink_metadata(entry.path()) else {
            continue;
        };
        let Some(modified) = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|time| u64::try_from(time.as_millis()).ok())
        else {
            continue;
        };
        let age = now_ms.saturating_sub(modified);
        let stale = if marker_name(name) {
            super::read_bounded(&entry.path(), 512)
                .ok()
                .and_then(|bytes| holder_stale(&bytes, modified))
                .unwrap_or(age > 30 * 60_000)
        } else {
            marker_temp(name) && meta.is_file() && age > 60 * 60_000
        };
        if stale
            && let Ok((device, inode)) = marker_identity(&entry.path())
            && remove_antigravity_marker(root, name, device, inode).unwrap_or(false)
        {
            count += 1;
        }
    }
    Ok(count)
}
