//! Capped reads of the existing private files Rust answers from (REVIEW-M3-part3-cheap L1).
//!
//! The name is opened once, without following a final symlink and without blocking. Every
//! decision after that is made on the opened descriptor, never on a second path lookup, so a
//! replacement or a FIFO swapped in between cannot change what is read. At most the cap plus
//! one byte is read. Whatever the descriptor cannot prove is a refusal (`None`), and the bridge
//! serves the TS reader. The callers' files are fixed basenames under the canonical boot root,
//! so an `ELOOP` from `O_NOFOLLOW` can only mean that the final name is a symlink.
use std::{
    fs::OpenOptions,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::Path,
};

/// What one capped open found.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Capped {
    /// No such name at the open (`ENOENT`).
    Absent,
    /// The final name is a symlink (`ELOOP` under `O_NOFOLLOW`); nothing was opened.
    Symlink,
    /// A regular file whose descriptor reports more than the cap; nothing was read.
    TooLarge,
    /// The whole regular file, at most the cap, with the descriptor's mode bits.
    Bytes { bytes: Vec<u8>, mode: u32 },
}

/// Open `path` no-follow and nonblocking, then decide on the descriptor. `None` refuses: an
/// unexpected open error, anything but a regular file, or a file that grew past the cap while
/// it was read.
pub(crate) fn read_capped(path: &Path, cap: u64) -> Option<Capped> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY)
        .open(path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Some(Capped::Absent),
        Err(error) if error.raw_os_error() == Some(libc::ELOOP) => return Some(Capped::Symlink),
        Err(_) => return None,
    };
    let meta = file.metadata().ok()?;
    if !meta.file_type().is_file() {
        return None;
    }
    if meta.len() > cap {
        return Some(Capped::TooLarge);
    }
    let mut bytes = Vec::new();
    file.take(cap.checked_add(1)?)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() as u64 > cap {
        return None;
    }
    Some(Capped::Bytes {
        bytes,
        mode: meta.mode(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs, os::unix::fs::symlink};

    #[test]
    fn decides_on_the_opened_descriptor() {
        let dir = tempfile::tempdir().expect("fixture folder");
        let root = dir.path();
        assert_eq!(read_capped(&root.join("absent"), 8), Some(Capped::Absent));

        fs::write(root.join("small"), b"12345678").expect("fixture file");
        let Some(Capped::Bytes { bytes, .. }) = read_capped(&root.join("small"), 8) else {
            panic!("an exactly-capped file is read");
        };
        assert_eq!(bytes, b"12345678");
        assert_eq!(read_capped(&root.join("small"), 7), Some(Capped::TooLarge));

        symlink(root.join("small"), root.join("link")).expect("fixture link");
        assert_eq!(read_capped(&root.join("link"), 8), Some(Capped::Symlink));
        symlink(root.join("absent"), root.join("dangling")).expect("fixture link");
        assert_eq!(
            read_capped(&root.join("dangling"), 8),
            Some(Capped::Symlink)
        );

        // A directory and a FIFO are never read; the FIFO open does not block.
        fs::create_dir(root.join("folder")).expect("fixture folder");
        assert_eq!(read_capped(&root.join("folder"), 8), None);
        let fifo = root.join("fifo");
        let made = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("mkfifo");
        assert!(made.success(), "mkfifo");
        assert_eq!(read_capped(&fifo, 8), None);
    }
}
