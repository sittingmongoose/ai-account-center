use crate::{path, require_absolute};
use aac_core::{Cause, Failure};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
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
    let meta = fs::symlink_metadata(&run)?;
    if path::linked_within(Path::new("/"), &run)
        || !meta.is_dir()
        || meta.mode() & 0o777 != 0o700
        || meta.uid() != unsafe { libc::geteuid() }
    {
        return Err(refused());
    }
    Ok(run.join(format!("lifecycle-{name}.lock")))
}

/// A native lifecycle owner opens the same 0600 marker as the compatibility
/// front door. Keep the descriptor until the whole effect and rollback settle.
pub fn open(root: &Path, scope: &str) -> Result<File, Failure> {
    let path = filename(root, scope)?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(&path)?;
    verify(&path, &file)?;
    Ok(file)
}

fn verify(path: &Path, file: &File) -> Result<(), Failure> {
    let expected = fs::symlink_metadata(path)?;
    let actual = file.metadata()?;
    if !expected.is_file()
        || !actual.is_file()
        || actual.uid() != unsafe { libc::geteuid() }
        || actual.mode() & 0o777 != 0o600
        || actual.nlink() != 1
        || actual.len() != 0
        || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
    {
        return Err(refused());
    }
    Ok(())
}

/// Lock the caller's open file description. Its retained descriptor keeps the
/// workflow serialized after this process exits.
pub fn lock_inherited(root: &Path, scope: &str, file: &File) -> Result<(), Failure> {
    verify(&filename(root, scope)?, file)?;
    file.lock()?;
    Ok(())
}
