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

/// Python's `failover_core.SAFE_NAME` (`^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`, ASCII): the folder
/// names its sibling discovery keeps as saved or T3 homes.
pub(crate) fn safe_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(c))
}

/// True when the file is not the caller's own: Python's readers (`failover_core._read_present`)
/// refuse a file whose owner is not `os.getuid()`. Windows has no such check.
pub(crate) fn foreign(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.uid() != crate::key_store::uid()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// True when a component strictly between `base` and `path` is a link, or cannot be checked.
/// Readers that must not be redirected below a home (plan credentials) decline those paths.
pub(crate) fn linked_below(base: &Path, path: &Path) -> bool {
    linked(base, path, false)
}

/// [`linked_below`], and `path` itself too. Python's `failover_core._local_path` follows a link
/// below the home only while each written target stays inside it, hop by hop; no port proves that
/// chain, so readers that Python sends through it (T3 settings, sibling homes) decline any link.
pub(crate) fn linked_within(base: &Path, path: &Path) -> bool {
    linked(base, path, true)
}

/// Where Python's `failover_core._local_path(scope, link)` leads for one link whose own folders
/// below `scope` hold no link: its written target, made absolute as text (`os.path.abspath`).
pub(crate) enum Hop {
    /// Inside the scope with no further link: whether that path is a folder (Python's
    /// `S_ISDIR(os.lstat(validated))`).
    Inside(bool),
    /// Refused without a counted failure: the target leaves the scope, or is spelled "//".
    Outside,
    /// The path the target names is not there: Python counts a failed read.
    Missing,
    /// A second link, a target spelling no port proves (backslashes, not UTF-8) or an error.
    Unproved,
}

/// One hop of Python's `_local_path`; a chain longer than one link is [`Hop::Unproved`].
pub(crate) fn hop(scope: &Path, link: &Path) -> Hop {
    let Ok(target) = fs::read_link(link) else {
        return Hop::Unproved;
    };
    let Some(text) = target.to_str() else {
        return Hop::Unproved;
    };
    if text.starts_with("//") {
        return Hop::Outside;
    }
    let Some(parent) = link
        .parent()
        .filter(|_| !text.is_empty() && !text.contains('\\'))
    else {
        return Hop::Unproved;
    };
    let next = lexical(&parent.join(text));
    if !next.starts_with(scope) {
        return Hop::Outside;
    }
    if linked_within(scope, &next) {
        return Hop::Unproved;
    }
    match fs::symlink_metadata(&next) {
        Ok(m) => Hop::Inside(m.is_dir()),
        Err(e) if missing(&e) => Hop::Missing,
        Err(_) => Hop::Unproved,
    }
}

fn linked(base: &Path, path: &Path, leaf: bool) -> bool {
    let Ok(rest) = path.strip_prefix(base) else {
        return true;
    };
    let mut current = base.to_path_buf();
    let parts: Vec<_> = rest.components().collect();
    let checked = if leaf {
        parts.len()
    } else {
        parts.len().saturating_sub(1)
    };
    for part in parts.iter().take(checked) {
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
    fn sibling_names_are_pythons_safe_names() {
        for name in ["a", "A9", "work-1_b", &"y".repeat(64)] {
            assert!(safe_name(name), "{name:?}");
        }
        for name in [
            "",
            "_under",
            "-dash",
            ".hidden",
            "a.b",
            "a b",
            "caf\u{e9}",
            &"y".repeat(65),
        ] {
            assert!(!safe_name(name), "{name:?}");
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
            // The leaf counts too: a final link, a link above it, or a dangling link.
            assert!(!linked_below(dir.path(), &link));
            assert!(linked_within(dir.path(), &link));
            assert!(linked_within(dir.path(), &alias));
            assert!(linked_within(dir.path(), &alias.join("x.json")));
            assert!(!linked_within(dir.path(), &sub.join("x.json")));
            assert!(!linked_within(dir.path(), &dir.path().join("gone/x.json")));
            std::os::unix::fs::symlink(dir.path().join("none"), dir.path().join("dead"))?;
            assert!(linked_within(dir.path(), &dir.path().join("dead")));
            assert!(linked_within(dir.path(), &dir.path().join("dead/x.json")));
            assert!(!foreign(&fs::symlink_metadata(&file)?));
            // One hop of `_local_path`: in the scope, out of it, missing, or a second link.
            let scope = dir.path();
            let cases: [(&str, String); 7] = [
                ("in-dir", "sub".into()),
                ("in-file", scope.join("a.json").to_string_lossy().into()),
                ("out", "../..".into()),
                ("network", format!("/{}", scope.join("sub").display())),
                ("gone", "nothing".into()),
                ("second", "alias".into()),
                ("slash", "sub\\x".into()),
            ];
            for (name, target) in &cases {
                std::os::unix::fs::symlink(target, scope.join(name))?;
            }
            let hop = |name: &str| hop(scope, &scope.join(name));
            assert!(matches!(hop("in-dir"), Hop::Inside(true)));
            assert!(matches!(hop("in-file"), Hop::Inside(false)));
            assert!(matches!(hop("out"), Hop::Outside));
            assert!(matches!(hop("network"), Hop::Outside));
            assert!(matches!(hop("gone"), Hop::Missing));
            assert!(matches!(hop("second"), Hop::Unproved));
            assert!(matches!(hop("slash"), Hop::Unproved));
            assert!(matches!(hop("a.json"), Hop::Unproved));
        }
        Ok(())
    }
}
