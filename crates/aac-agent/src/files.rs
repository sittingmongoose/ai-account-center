//! The agent's one hardened file read and one path-spelling rule. Every verb that reads a local
//! store (signin-expiry, failover-inventory, quota-read, claude-history) opens files and admits
//! path spellings through here, so a fix to either rule reaches every reader at once. Each verb
//! still decides what an outcome means for its own reference (absent, unseen or a fallback).
use std::{
    fs,
    io::Read,
    path::{Component, Path, PathBuf},
};

/// Python's pathlib and os.path keep exactly two leading slashes as a root of their own
/// ("//x" is not "/x") and read three or more as "/". No port proves either rule, so a spelling
/// that starts with "//" is unproved wherever it comes from: a request scope, a settings home, an
/// environment selector or the default home. This records the decline and answers true.
pub(crate) fn double_root(raw: &str) -> bool {
    let unproved = raw.starts_with("//");
    if unproved {
        crate::proof::reject();
    }
    unproved
}

/// A home spelled the way Python's `Path.home()` and `pathlib.Path` already write it: absolute,
/// printable ASCII, no empty, "." or ".." component and no trailing separator. Python normalizes
/// other spellings before they reach locators, hashes and repair commands; a verb that uses the
/// raw text declines them here instead of guessing that normalization.
pub(crate) fn canonical_home(raw: &str) -> bool {
    let parts = |rest: &str, separator: char| {
        rest.split(separator)
            .all(|p| !p.is_empty() && p != "." && p != "..")
    };
    // Windows: `WindowsPath` writes a drive root and backslashes ("C:\Users\x"); UNC and
    // forward-slash spellings are rewritten, so they are not proved.
    let rooted = if cfg!(windows) {
        let b = raw.as_bytes();
        b.len() >= 3
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && b[2] == b'\\'
            && !raw.contains('/')
            && (b.len() == 3 || parts(&raw[3..], '\\'))
    } else {
        !double_root(raw) && (raw == "/" || raw.strip_prefix('/').is_some_and(|r| parts(r, '/')))
    };
    let canonical = raw.len() <= 4096
        && raw.bytes().all(|c| (0x20..=0x7e).contains(&c))
        && rooted
        && Path::new(raw).is_absolute();
    if !canonical {
        crate::proof::reject();
    }
    canonical
}

/// `os.path.abspath` of an absolute spelling that [`double_root`] admitted: "." and empty
/// components drop, ".." removes its parent (never past the root).
pub(crate) fn lexical(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            part => result.push(part.as_os_str()),
        }
    }
    result
}

fn missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// What one bounded read of one ordinary file found.
pub(crate) enum Regular {
    /// Nothing is there (or an ancestor is not a directory).
    Missing,
    /// Present, but a link, directory, FIFO, socket or device: never opened for reading.
    Unusual,
    /// Larger than the cap, by its metadata or by the bytes read.
    TooLarge,
    /// A metadata, open or read error, or the file was replaced between its check and its open.
    Failed,
    /// The bytes (at most the cap) and the metadata of the descriptor they came from.
    Read(Vec<u8>, fs::Metadata),
}

/// Why [`open_regular`] opened nothing; the same outcomes as [`Regular`]'s other variants.
pub(crate) enum Unopened {
    Missing,
    Unusual,
    TooLarge,
    Failed,
}

impl From<Unopened> for Regular {
    fn from(outcome: Unopened) -> Self {
        match outcome {
            Unopened::Missing => Regular::Missing,
            Unopened::Unusual => Regular::Unusual,
            Unopened::TooLarge => Regular::TooLarge,
            Unopened::Failed => Regular::Failed,
        }
    }
}

/// Open one ordinary file without following a final link or blocking on a FIFO, and prove the
/// descriptor is the file the path named when it was checked. Readers that need only a header
/// (an app archive) read from the returned file themselves; everyone else uses [`read_regular`].
pub(crate) fn open_regular(path: &Path, cap: u64) -> Result<(fs::File, fs::Metadata), Unopened> {
    let before = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if missing(&e) => return Err(Unopened::Missing),
        Err(_) => return Err(Unopened::Failed),
    };
    if before.len() > cap {
        return Err(Unopened::TooLarge);
    }
    if !before.is_file() {
        return Err(Unopened::Unusual);
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options.open(path).map_err(|_| Unopened::Failed)?;
    let opened = file.metadata().map_err(|_| Unopened::Failed)?;
    if !opened.is_file() {
        return Err(Unopened::Unusual);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (before.dev(), before.ino()) != (opened.dev(), opened.ino()) {
            return Err(Unopened::Failed);
        }
    }
    Ok((file, opened))
}

/// [`open_regular`], then read at most `cap` bytes from that one descriptor.
pub(crate) fn read_regular(path: &Path, cap: usize) -> Regular {
    let (file, meta) = match open_regular(path, cap as u64) {
        Ok(opened) => opened,
        Err(outcome) => return outcome.into(),
    };
    let mut bytes = Vec::new();
    if file.take(cap as u64 + 1).read_to_end(&mut bytes).is_err() {
        return Regular::Failed;
    }
    if bytes.len() > cap {
        return Regular::TooLarge;
    }
    Regular::Read(bytes, meta)
}

/// True when a component strictly between `base` and `path` is a link, or cannot be checked.
/// Readers that must not be redirected below a home (plan credentials) decline those paths.
pub(crate) fn linked_below(base: &Path, path: &Path) -> bool {
    let Ok(rest) = path.strip_prefix(base) else {
        return true;
    };
    let mut current = base.to_path_buf();
    let parts: Vec<_> = rest.components().collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        current.push(part.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(m) if m.file_type().is_symlink() => return true,
            Ok(_) => {}
            Err(e) if missing(&e) => return false,
            Err(_) => return true,
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_spellings_python_normalizes_are_declined() {
        for raw in ["/home/a", "/", "/tmp/fixture-1"] {
            assert!(crate::proof::scope(|| serde_json::json!(canonical_home(raw))) == true);
        }
        for raw in [
            "//home/a",
            "///home/a",
            "/tmp//fixture",
            "/tmp/fixture/.",
            "/tmp/fixture/",
            "/tmp/./fixture",
            "/tmp/a/../b",
            "relative",
            "",
            "/tmp/caf\u{e9}",
        ] {
            assert_eq!(
                crate::proof::scope(|| serde_json::json!(canonical_home(raw))),
                crate::proof::fallback(),
                "{raw:?}"
            );
        }
    }

    #[test]
    #[allow(unsafe_code)]
    fn only_one_ordinary_file_is_read() -> std::io::Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"{}")?;
        assert!(matches!(read_regular(&file, 2), Regular::Read(ref b, _) if b == b"{}"));
        assert!(matches!(read_regular(&file, 1), Regular::TooLarge));
        assert!(matches!(
            read_regular(&dir.path().join("none"), 9),
            Regular::Missing
        ));
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
            // The size check comes first (signin's order): a link's own size is its target text.
            assert!(matches!(read_regular(&link, 1 << 20), Regular::Unusual));
            let fifo = dir.path().join("fifo");
            let name = std::ffi::CString::new(fifo.to_string_lossy().as_bytes())?;
            // SAFETY: a valid NUL-terminated path in a private temporary directory.
            assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
            assert!(matches!(read_regular(&fifo, 1 << 20), Regular::Unusual));
            let sub = dir.path().join("sub");
            fs::create_dir(&sub)?;
            fs::write(sub.join("x.json"), b"{}")?;
            let alias = dir.path().join("alias");
            std::os::unix::fs::symlink(&sub, &alias)?;
            assert!(linked_below(dir.path(), &alias.join("x.json")));
            assert!(!linked_below(dir.path(), &sub.join("x.json")));
            assert!(!linked_below(dir.path(), &dir.path().join("gone/x.json")));
        }
        Ok(())
    }
}
