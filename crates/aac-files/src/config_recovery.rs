//! Recovery for the old writer's exclusive-open-before-write crash window.

use aac_core::{Cause, Failure};
use std::{
    fs::{self, File, Metadata},
    path::Path,
    time::Duration,
};

pub(crate) fn guard_empty(path: &Path, before: &Metadata) -> Result<File, Failure> {
    if !old_empty(before) {
        return Err(busy());
    }
    let guard = exclude_writers(path)?;
    let opened = guard.metadata()?;
    let current = fs::symlink_metadata(path)?;
    if !old_empty(&opened)
        || !old_empty(&current)
        || !crate::same_file(before, &opened)
        || !crate::same_file(&opened, &current)
        || before.modified()? != current.modified()?
    {
        return Err(busy());
    }
    Ok(guard)
}

fn old_empty(metadata: &Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if !crate::private_cache_metadata(metadata, false, metadata.nlink() == 1) {
            return false;
        }
    }
    #[cfg(windows)]
    if crate::windows_reparse_point(metadata) {
        return false;
    }
    metadata.is_file()
        && metadata.len() == 0
        && metadata.modified().is_ok_and(|modified| {
            // Match the grace period of the pre-cut TS config lock.
            modified
                .elapsed()
                .is_ok_and(|age| age > Duration::from_secs(5))
        })
}

#[cfg(target_os = "linux")]
#[allow(unsafe_code)]
fn exclude_writers(path: &Path) -> Result<File, Failure> {
    use std::os::{fd::AsRawFd, unix::fs::OpenOptionsExt};

    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    if !old_empty(&file.metadata()?) {
        return Err(busy());
    }
    let fd = file.as_raw_fd();
    // Linux fcntl.h. SIGURG is ignored by default, unlike the lease's usual SIGIO.
    const F_SETSIG: libc::c_int = 10;
    if unsafe { libc::fcntl(fd, F_SETSIG, libc::SIGURG) } == -1
        || unsafe { libc::fcntl(fd, libc::F_SETLEASE, libc::F_RDLCK) } == -1
    {
        return Err(busy());
    }
    // A read lease refuses any already-open writer, including the legacy TS
    // process stopped immediately after exclusive open. Closing releases it.
    Ok(file)
}

#[cfg(windows)]
fn exclude_writers(path: &Path) -> Result<File, Failure> {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_FLAG_OPEN_REPARSE_POINT, FILE_SHARE_DELETE, FILE_SHARE_READ,
    };

    fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_DELETE)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
        .open(path)
        .map_err(|_| busy())
}

#[cfg(not(any(target_os = "linux", windows)))]
fn exclude_writers(_path: &Path) -> Result<File, Failure> {
    Err(busy())
}

fn busy() -> Failure {
    Failure::new(
        Cause::Busy,
        "empty config writer marker is recent or still open for writing",
    )
}
