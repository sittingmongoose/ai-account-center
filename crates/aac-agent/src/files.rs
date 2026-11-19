//! The agent's view of the one file policy (L2): `aac-files` opens and admits every path, and
//! this module only maps its outcomes to what the agent's references mean and records a decline
//! where a shared rule refuses a spelling. signin-expiry, failover-inventory, quota-read and
//! claude-history all read through here, so each still decides what an outcome means for its
//! own reference (absent, unseen or a fallback).
use aac_files::read::{self, Read, ReadPolicy};
use std::{fs, path::Path};

pub(crate) use aac_files::path::{Hop, hop, linked_within, python::normpath as lexical, safe_name};

/// [`aac_files::path::python::double_root`], recording the decline: a spelling that starts
/// with "//" is unproved wherever it comes from (a request scope, a settings home, an
/// environment selector or the default home).
pub(crate) fn double_root(raw: &str) -> bool {
    let unproved = aac_files::path::python::double_root(raw);
    if unproved {
        crate::proof::reject();
    }
    unproved
}

/// [`aac_files::path::python::canonical_home`], recording the decline for any other spelling.
pub(crate) fn canonical_home(raw: &str) -> bool {
    let canonical = aac_files::path::python::canonical_home(raw);
    if !canonical {
        crate::proof::reject();
    }
    canonical
}

/// What one bounded read of one ordinary file found, as the agent's references group it.
pub(crate) enum Regular {
    /// Nothing is there (or a folder on the way is not a folder).
    Missing,
    /// Present, but a link, folder, FIFO, socket or device: never opened for reading.
    Unusual,
    /// Larger than the cap, by its metadata or by the bytes read.
    TooLarge,
    /// A metadata, open or read error, or the file was replaced between its check and its open.
    Failed,
    /// Refused by the owner rule of the reader's policy.
    Foreign,
    /// Refused by the mode rule of the reader's policy.
    Loose,
    /// The bytes (at most the cap) and the metadata of the descriptor they came from. Only the Unix
    /// readers compare inode and mode, so the metadata is unread on Windows by design.
    Read(
        Vec<u8>,
        #[cfg_attr(not(unix), allow(dead_code))] fs::Metadata,
    ),
}

impl From<Read> for Regular {
    fn from(outcome: Read) -> Self {
        match outcome {
            Read::Bytes(bytes, meta) => Regular::Read(bytes, *meta),
            Read::Absent | Read::Unreadable(std::io::ErrorKind::NotADirectory) => Regular::Missing,
            Read::Link | Read::NotRegular => Regular::Unusual,
            Read::TooLarge => Regular::TooLarge,
            Read::Foreign => Regular::Foreign,
            Read::BadMode => Regular::Loose,
            Read::Unreadable(_) | Read::ChainUnreadable(_) | Read::Unproved => Regular::Failed,
        }
    }
}

/// Read one file under `policy` and group the outcome.
pub(crate) fn read_with(path: &Path, policy: &ReadPolicy) -> Regular {
    read::read(path, policy).into()
}

/// One ordinary file of at most `cap` bytes; a final link is never followed.
pub(crate) fn read_regular(path: &Path, cap: usize) -> Regular {
    read_with(path, &ReadPolicy::capped(cap as u64))
}

/// Open one ordinary file for a reader that needs only a header (an app archive); the
/// descriptor's metadata comes back with it. Every refusal is `None` to this reader.
/// Only the Unix readers (the Claude history archive) need a descriptor and its metadata.
#[cfg(unix)]
pub(crate) fn open_regular(path: &Path, cap: u64) -> Option<(fs::File, fs::Metadata)> {
    read::open(path, &ReadPolicy::capped(cap)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declines_are_recorded_where_a_shared_rule_refuses() {
        // The canonical spelling is the host's: a single root on Unix, a drive root with
        // backslashes on Windows (a forward-slash or UNC spelling is rewritten there).
        #[cfg(unix)]
        for raw in ["/home/a", "/", "/tmp/fixture-1"] {
            assert!(crate::proof::scope(|| serde_json::json!(canonical_home(raw))) == true);
        }
        #[cfg(windows)]
        for raw in [r"C:\Users\a", r"C:\", r"C:\tmp\fixture-1"] {
            assert!(crate::proof::scope(|| serde_json::json!(canonical_home(raw))) == true);
        }
        for raw in [
            "//home/a",
            "/tmp//fixture",
            "/tmp/fixture/",
            "relative",
            "",
            // The other host's canonical shape is never proved here either.
            r"C:\Users\\a",
            r"C:\Users\a\",
            "C:relative",
        ] {
            assert_eq!(
                crate::proof::scope(|| serde_json::json!(canonical_home(raw))),
                crate::proof::fallback(),
                "{raw:?}"
            );
        }
        assert_eq!(
            crate::proof::scope(|| serde_json::json!(double_root("//x"))),
            crate::proof::fallback()
        );
        assert!(crate::proof::scope(|| serde_json::json!(double_root("/x"))) == false);
    }

    #[test]
    #[allow(unsafe_code)]
    fn outcomes_keep_the_agents_grouping() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"{}")?;
        assert!(matches!(read_regular(&file, 2), Regular::Read(ref b, _) if b == b"{}"));
        assert!(matches!(read_regular(&file, 1), Regular::TooLarge));
        assert!(matches!(
            read_regular(&dir.path().join("none"), 9),
            Regular::Missing
        ));
        // A folder on the way that is a file reads as missing, as Python's NotADirectoryError.
        assert!(matches!(
            read_regular(&file.join("below"), 9),
            Regular::Missing
        ));
        assert!(matches!(
            read_regular(dir.path(), 1 << 20),
            Regular::Unusual
        ));
        #[cfg(unix)]
        {
            let link = dir.path().join("link.json");
            std::os::unix::fs::symlink(&file, &link)?;
            assert!(matches!(read_regular(&link, 1 << 20), Regular::Unusual));
            let fifo = dir.path().join("fifo");
            let name = std::ffi::CString::new(fifo.to_string_lossy().as_bytes())?;
            // SAFETY: a valid NUL-terminated path in a private temporary directory.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(matches!(read_regular(&fifo, 1 << 20), Regular::Unusual));
            assert!(open_regular(&fifo, 9).is_none());
        }
        Ok(())
    }
}
