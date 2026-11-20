//! Filesystem leases shared by lifecycle maintenance and requested provider effects.
use crate::{path, require_absolute};
use aac_core::{Cause, Failure};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
#[cfg(windows)]
use std::os::windows::fs::OpenOptionsExt;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

fn refused() -> Failure {
    Failure::new(Cause::Refused, "invalid lifecycle queue lease")
}

fn filename(root: &Path, scope: &str) -> Result<PathBuf, Failure> {
    require_absolute(root)?;
    let name = if scope == "claude-purge" {
        scope.to_owned()
    } else {
        let provider = scope.strip_prefix("key:").ok_or_else(refused)?;
        if provider.is_empty()
            || provider.len() > 32
            || !provider
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        {
            return Err(refused());
        }
        format!("key-{provider}")
    };
    let run = root.join("run");
    let volume_root = run.ancestors().last().ok_or_else(refused)?;
    if path::linked_within(volume_root, &run) {
        return Err(refused());
    }
    match crate::private_builder().create(&run) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(&run)?;
    if !meta.is_dir() {
        return Err(refused());
    }
    #[cfg(unix)]
    if meta.mode() & 0o777 != 0o700 || meta.uid() != path::uid() {
        return Err(refused());
    }
    Ok(run.join(format!("lifecycle-{name}.lock")))
}

/// A native lifecycle owner opens the same 0600 marker as the compatibility
/// front door. Keep the descriptor until the whole effect and rollback settle.
pub fn open(root: &Path, scope: &str) -> Result<File, Failure> {
    let path = filename(root, scope)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    options
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    #[cfg(windows)]
    options.custom_flags(0x0020_0000); // FILE_FLAG_OPEN_REPARSE_POINT
    let file = options.open(&path)?;
    verify(&path, &file)?;
    Ok(file)
}

fn verify(path: &Path, file: &File) -> Result<(), Failure> {
    let expected = fs::symlink_metadata(path)?;
    let actual = file.metadata()?;
    if !expected.is_file() || !actual.is_file() || actual.len() != 0 {
        return Err(refused());
    }
    #[cfg(unix)]
    if actual.uid() != path::uid()
        || actual.mode() & 0o777 != 0o600
        || actual.nlink() != 1
        || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
    {
        return Err(refused());
    }
    Ok(())
}

/// Lock the caller's open file description. Its retained descriptor keeps the
/// workflow serialized after this process exits.
#[cfg(unix)]
pub fn lock_inherited(root: &Path, scope: &str, file: &File) -> Result<(), Failure> {
    verify(&filename(root, scope)?, file)?;
    file.lock()?;
    Ok(())
}
