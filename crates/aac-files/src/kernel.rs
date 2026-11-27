//! Kernel pseudo-files and process-runtime helper files for the Codex update runtime (P2).
//!
//! `/proc` entries are not AAC private files: they have no stable size, owner proof or
//! registry row, so they get plain bounded reads here instead of [`crate::read`]. The lock and
//! log helpers create private (0600) files under the caller\x27s own runtime folder.
use std::{
    fs::File,
    io::{self, Read},
    path::Path,
};

/// Read a kernel text file (`/proc/...`) of at most `cap` bytes; anything longer is an error,
/// never a silently cut text.
pub fn read_text(path: &Path, cap: usize) -> io::Result<String> {
    let mut raw = String::new();
    File::open(path)?
        .take(cap as u64 + 1)
        .read_to_string(&mut raw)?;
    if raw.len() > cap {
        return Err(io::Error::other("kernel file over its cap"));
    }
    Ok(raw)
}

/// Open the file a process holds, through its `/proc/<pid>/fd/<n>` link, for reading.
pub fn open_held(link: &Path) -> io::Result<File> {
    File::open(link)
}

/// Open (creating it 0600 if missing, never truncating) a lock file the caller then locks.
#[cfg(unix)]
pub fn open_lock(path: &Path) -> io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(path)
}

/// Make sure a private (0600) append log exists, moving it to `<name>.log.1` first once it is
/// larger than `rotate_at` bytes. Rotation is best effort; creating the log is not.
#[cfg(unix)]
pub fn prepare_log(path: &Path, rotate_at: u64) -> io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| io::Error::other("log has no folder"))?,
    )?;
    if std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > rotate_at) {
        let _ = std::fs::rename(path, path.with_extension("log.1"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn read_text_refuses_a_file_over_its_cap_and_reads_one_at_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("stat");
        std::fs::write(&path, "12345").unwrap();
        assert_eq!(read_text(&path, 5).unwrap(), "12345");
        assert!(read_text(&path, 4).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn prepare_log_creates_a_private_log_and_rotates_a_large_one() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/runtime.log");
        prepare_log(&path, 4).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        std::fs::write(&path, "too long").unwrap();
        prepare_log(&path, 4).unwrap();
        assert_eq!(
            std::fs::read_to_string(path.with_extension("log.1")).unwrap(),
            "too long"
        );
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn open_lock_keeps_existing_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("x.lock");
        std::fs::write(&path, "held").unwrap();
        drop(open_lock(&path).unwrap());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "held");
    }
}
