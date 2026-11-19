//! The file rules the Claude Open store shares, split by platform.
//!
//! On Unix the rules are the core's POSIX ones: real, private, owned, exact mode, one link,
//! and a no-follow descriptor. Windows has no POSIX owner or mode bits and Node reports
//! synthetic ones, so the reference accepts the profile ACL there (see
//! `private-json-store.ts`'s `isPrivate`/`checkStoreFolder`); the ownership-shaped checks
//! degrade to "regular file reached through the checked folder" and the marker protocol's
//! `ownedPrivate` stays false, as the core's does on a non-POSIX host.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

/// The core's `process.getuid()`: read once from `/proc/self/status` (no unsafe call).
/// Non-Unix hosts have no uid; callers use [`owner_ok`] instead of comparing one.
fn process_uid() -> u32 {
    #[cfg(unix)]
    {
        static UID: std::sync::LazyLock<u32> = std::sync::LazyLock::new(|| {
            fs::read_to_string("/proc/self/status")
                .ok()
                .and_then(|status| {
                    status.lines().find_map(|line| {
                        line.strip_prefix("Uid:")
                            .and_then(|rest| rest.split_whitespace().next())
                            .and_then(|real| real.parse().ok())
                    })
                })
                .unwrap_or(u32::MAX)
        });
        *UID
    }
    #[cfg(not(unix))]
    {
        u32::MAX
    }
}

/// Whether the file belongs to this process's account. Windows enforces this with the ACL.
pub(super) fn owner_ok(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        meta.uid() == process_uid()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

/// `mode & 0o077 == 0` on Unix; Windows has no POSIX permission bits.
pub(super) fn private_mode(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        meta.permissions().mode() & 0o077 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

/// The core's `ownedPrivate`: real, private, owned, exact mode (0600 file, 0700 directory),
/// one link for a file. Non-POSIX hosts answer false, which is the core's own behaviour there.
pub(super) fn owned_private(meta: &fs::Metadata, directory: bool) -> bool {
    #[cfg(unix)]
    {
        let file_type = meta.file_type();
        let right_kind = if directory {
            file_type.is_dir()
        } else {
            file_type.is_file()
        };
        right_kind
            && !file_type.is_symlink()
            && meta.uid() == process_uid()
            && meta.permissions().mode() & 0o777 == if directory { 0o700 } else { 0o600 }
            && (directory || meta.nlink() == 1)
    }
    #[cfg(not(unix))]
    {
        let _ = (meta, directory);
        false
    }
}

/// The file identity Node's `stat` reports: device and inode on Unix. Windows has no stable
/// fields in the standard library, so the pair is (0, 0) and [`same_file`] compares the
/// available facts instead.
pub(super) fn file_identity(meta: &fs::Metadata) -> (u64, u64) {
    #[cfg(unix)]
    {
        (meta.dev(), meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        (0, 0)
    }
}

/// Whether two descriptors or lstat results name the same file. Unix compares device and
/// inode; Windows compares length and the last-write time.
pub(super) fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        left.len() == right.len() && left.modified().ok() == right.modified().ok()
    }
}

/// Node's `stat.mtimeMs` float milliseconds.
pub(super) fn mtime_ms(meta: &fs::Metadata) -> f64 {
    #[cfg(unix)]
    {
        meta.mtime() as f64 * 1000.0 + meta.mtime_nsec() as f64 / 1_000_000.0
    }
    #[cfg(not(unix))]
    {
        epoch_ms(meta.modified().ok())
    }
}

/// Node's `stat.ctimeMs`. Windows has no change time in the standard library; the creation
/// time (or the last write when that is unavailable) is the closest stable fact.
pub(super) fn ctime_ms(meta: &fs::Metadata) -> f64 {
    #[cfg(unix)]
    {
        meta.ctime() as f64 * 1000.0 + meta.ctime_nsec() as f64 / 1_000_000.0
    }
    #[cfg(not(unix))]
    {
        epoch_ms(meta.created().ok().or_else(|| meta.modified().ok()))
    }
}

#[cfg(not(unix))]
fn epoch_ms(at: Option<std::time::SystemTime>) -> f64 {
    at.and_then(|at| at.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0.0, |elapsed| elapsed.as_millis() as f64)
}

/// The reference's no-follow read descriptor (Unix); a plain read elsewhere.
pub(super) fn open_read_no_follow(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path)
}

/// A create-only private file (0600 on Unix).
pub(super) fn open_create_new(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        options.custom_flags(libc::O_NOFOLLOW).mode(0o600);
    }
    options.open(path)
}

/// `fsync` a directory inode; a no-op where the platform has no directory descriptor.
pub(super) fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
            .open(directory)?;
        file.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

/// Make a folder private (0700). Windows inherits the profile ACL.
pub(super) fn set_private_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}
