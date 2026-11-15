//! Path admission: every spelling and link rule a reader applies before it opens a file, each
//! named for the reference implementation whose semantics it reproduces. Readers decide what an
//! answer means for their own reference (absent, unseen or a fallback); these functions only
//! answer the question they name. (L2: moved here from `aac-agent/src/files.rs`.)
use std::{
    fs,
    path::{Component, Path, PathBuf},
};

/// Python's `pathlib` and `os.path` spelling rules.
pub mod python {
    use super::*;

    /// Python keeps exactly two leading slashes as a root of their own ("//x" is not "/x") and
    /// reads three or more as "/". No port proves either rule, so a spelling that starts with
    /// "//" is unproved wherever it comes from. True means unproved.
    pub fn double_root(raw: &str) -> bool {
        raw.starts_with("//")
    }

    /// A home spelled the way Python's `Path.home()` and `pathlib.Path` already write it:
    /// absolute, printable ASCII, no empty, "." or ".." component and no trailing separator.
    /// Python normalizes other spellings before they reach locators, hashes and repair commands,
    /// so a reader that uses the raw text declines them instead of guessing that normalization.
    pub fn canonical_home(raw: &str) -> bool {
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
            !double_root(raw)
                && (raw == "/" || raw.strip_prefix('/').is_some_and(|r| parts(r, '/')))
        };
        raw.len() <= 4096
            && raw.bytes().all(|c| (0x20..=0x7e).contains(&c))
            && rooted
            && Path::new(raw).is_absolute()
    }

    /// `os.path.abspath` of an absolute spelling that [`double_root`] admitted: "." and empty
    /// components drop, ".." removes its parent (never past the root).
    pub fn normpath(path: &Path) -> PathBuf {
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
}

/// Node's `path` spelling rules.
pub mod node {
    use super::*;
    use aac_core::{Cause, Failure};

    /// `path.resolve(base, selected)`: relative paths use the supplied absolute base, dot
    /// segments collapse, and no filesystem symlink is followed. The result may name a folder
    /// that does not exist yet.
    pub fn resolve(base: &Path, selected: &Path) -> Result<PathBuf, Failure> {
        let path = if selected.is_absolute() {
            selected.to_path_buf()
        } else {
            if !base.is_absolute() {
                return Err(Failure::new(
                    Cause::Refused,
                    "relative root requires an absolute base",
                ));
            }
            base.join(selected)
        };
        absolute(&path)
    }

    /// `path.resolve` of an absolute path.
    pub(crate) fn absolute(path: &Path) -> Result<PathBuf, Failure> {
        if !path.is_absolute() {
            return Err(Failure::new(Cause::Refused, "store root must be absolute"));
        }
        let mut result = PathBuf::new();
        for part in path.components() {
            match part {
                Component::CurDir => {}
                Component::ParentDir => {
                    if result.file_name().is_some() {
                        result.pop();
                    }
                }
                _ => result.push(part.as_os_str()),
            }
        }
        Ok(result)
    }
}

/// Not there: Python's `(FileNotFoundError, NotADirectoryError)`.
pub(crate) fn missing(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

/// What a walk from `base` to `path` found on the folders between them.
pub(crate) enum Walk {
    /// No link; the walk may have stopped at a missing component.
    Clear,
    /// A component is a link.
    Link,
    /// `path` is not below `base`.
    Outside,
    /// A component could not be checked.
    Failed(std::io::ErrorKind),
}

/// Check each component strictly between `base` and `path` (and `path` itself when `leaf`).
pub(crate) fn walk(base: &Path, path: &Path, leaf: bool) -> Walk {
    let Ok(rest) = path.strip_prefix(base) else {
        return Walk::Outside;
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
            Ok(m) if m.file_type().is_symlink() => return Walk::Link,
            Ok(_) => {}
            Err(e) if missing(&e) => return Walk::Clear,
            Err(e) => return Walk::Failed(e.kind()),
        }
    }
    Walk::Clear
}

/// True when a component strictly between `base` and `path` is a link, or cannot be checked.
/// Readers that must not be redirected below a home (plan credentials) decline those paths.
pub fn linked_below(base: &Path, path: &Path) -> bool {
    !matches!(walk(base, path, false), Walk::Clear)
}

/// [`linked_below`], and `path` itself too. Python's `failover_core._local_path` follows a link
/// below the home only while each written target stays inside it, hop by hop; no port proves
/// that chain, so readers that Python sends through it decline any link.
pub fn linked_within(base: &Path, path: &Path) -> bool {
    !matches!(walk(base, path, true), Walk::Clear)
}

/// Where Python's `failover_core._local_path(scope, link)` leads for one link whose own folders
/// below `scope` hold no link: its written target, made absolute as text (`os.path.abspath`).
#[derive(Debug, PartialEq, Eq)]
pub enum Hop {
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
pub fn hop(scope: &Path, link: &Path) -> Hop {
    let Ok(target) = fs::read_link(link) else {
        return Hop::Unproved;
    };
    let Some(text) = target.to_str() else {
        return Hop::Unproved;
    };
    if python::double_root(text) {
        return Hop::Outside;
    }
    let Some(parent) = link
        .parent()
        .filter(|_| !text.is_empty() && !text.contains('\\'))
    else {
        return Hop::Unproved;
    };
    let next = python::normpath(&parent.join(text));
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

/// Python's `failover_core.SAFE_NAME` (`^[A-Za-z0-9][A-Za-z0-9_-]{0,63}$`, ASCII): the folder
/// names its sibling discovery keeps as saved or T3 homes.
pub fn safe_name(name: &str) -> bool {
    let b = name.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|c| c.is_ascii_alphanumeric() || b"_-".contains(c))
}

/// This process's user id (`os.getuid()`).
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn uid() -> u32 {
    // SAFETY: getuid has no arguments, memory access or failure state.
    unsafe { libc::getuid() }
}

/// True when the file is not the caller's own: Python's readers (`failover_core._read_present`)
/// refuse a file whose owner is not `os.getuid()`. Windows has no such check.
pub fn foreign(meta: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        meta.uid() != uid()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_spellings_python_normalizes_are_refused() {
        for raw in ["/home/a", "/", "/tmp/fixture-1"] {
            assert!(python::canonical_home(raw), "{raw:?}");
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
            assert!(!python::canonical_home(raw), "{raw:?}");
        }
        assert!(python::double_root("//x") && !python::double_root("/x"));
        assert_eq!(
            python::normpath(Path::new("/a/./b/../c")),
            PathBuf::from("/a/c")
        );
        assert_eq!(python::normpath(Path::new("/..")), PathBuf::from("/"));
    }

    #[test]
    fn node_resolve_is_lexical() -> Result<(), aac_core::Failure> {
        assert_eq!(
            node::resolve(Path::new("/base"), Path::new("x/../y"))?,
            PathBuf::from("/base/y")
        );
        assert_eq!(
            node::resolve(Path::new("/base"), Path::new("/a/./b"))?,
            PathBuf::from("/a/b")
        );
        assert!(node::resolve(Path::new("base"), Path::new("x")).is_err());
        Ok(())
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

    #[cfg(unix)]
    #[test]
    fn links_below_a_base_and_one_hop() -> std::io::Result<()> {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"{}")?;
        let link = dir.path().join("link.json");
        symlink(&file, &link)?;
        let sub = dir.path().join("sub");
        fs::create_dir(&sub)?;
        fs::write(sub.join("x.json"), b"{}")?;
        let alias = dir.path().join("alias");
        symlink(&sub, &alias)?;
        assert!(linked_below(dir.path(), &alias.join("x.json")));
        assert!(!linked_below(dir.path(), &sub.join("x.json")));
        assert!(!linked_below(dir.path(), &dir.path().join("gone/x.json")));
        assert!(linked_below(dir.path(), Path::new("/elsewhere/x")));
        // The leaf counts too: a final link, a link above it, or a dangling link.
        assert!(!linked_below(dir.path(), &link));
        assert!(linked_within(dir.path(), &link));
        assert!(linked_within(dir.path(), &alias));
        assert!(linked_within(dir.path(), &alias.join("x.json")));
        assert!(!linked_within(dir.path(), &sub.join("x.json")));
        assert!(!linked_within(dir.path(), &dir.path().join("gone/x.json")));
        symlink(dir.path().join("none"), dir.path().join("dead"))?;
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
            symlink(target, scope.join(name))?;
        }
        let hop = |name: &str| hop(scope, &scope.join(name));
        assert_eq!(hop("in-dir"), Hop::Inside(true));
        assert_eq!(hop("in-file"), Hop::Inside(false));
        assert_eq!(hop("out"), Hop::Outside);
        assert_eq!(hop("network"), Hop::Outside);
        assert_eq!(hop("gone"), Hop::Missing);
        assert_eq!(hop("second"), Hop::Unproved);
        assert_eq!(hop("slash"), Hop::Unproved);
        assert_eq!(hop("a.json"), Hop::Unproved);
        Ok(())
    }
}
