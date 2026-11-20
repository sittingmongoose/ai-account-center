//! R4 (DESIGN-M3-COMPLETE 1.2/1.4): the owned share readers. The agent reports, the quota
//! report, the pm-mail registry, the status documents and the Scoreboard catalog, scanned
//! under the same rules the TypeScript readers kept: list a folder before reading what it
//! names, stat first and read only what changed, keep the last good copy with a named
//! problem, ignore a lower `seq`, keep a listed file through one transient share error and
//! name it on the second, and count a file absent from its listing as removed only after the
//! grace period. Every refusal is the TypeScript reader's fixed sentence.
//!
//! The filesystem is a trait ([`ShareFs`]) so the parity fixtures drive the same scan
//! sequences the TypeScript tests inject; [`NodeFs`] is the real share, read through
//! `aac-files` (the one file policy). The scanners are synchronous: the service runs them on
//! its bounded blocking pool, single-flight per source.

pub mod catalog;
pub mod check;
pub mod quota;
pub mod registry;
pub mod report;
pub mod reports;
pub mod seed;
pub mod status_docs;

use aac_files::read::{Read, ReadPolicy, Stat};
use serde_json::Value as Json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

/// A file absent from its folder's listing is kept (last good copy) this long before it counts
/// as removed: a Mac rename-over through SMB is a delete then a rename, and NFS caches the gap.
pub const MISSING_GRACE_MS: u64 = 120_000;

/// A lower `seq` than the one already accepted: the older copy is ignored and said out loud.
pub const SEQ_WENT_BACK: &str = "its seq is lower than one already read, so this copy is ignored";

/// A listed file whose ENOENT or ESTALE came back on the next scan too.
pub const PERSISTENT_SHARE_ERROR: &str = "could not be read on two scans in a row (the share says it is gone or stale)";

/// The same rule for a listed folder whose own listing fails twice in a row.
pub const PERSISTENT_LISTING_ERROR: &str =
    "folder could not be listed on two scans in a row (the share says it is gone or stale)";

/// The problem on a row whose folder could not be listed: the row is its last good reading.
pub fn folder_row_problem(folder_reason: &str) -> String {
    format!("its {folder_reason}")
}

/// One named problem: a file (relative to `<reports>/v1`) or a limit, its fixed reason, and
/// when it was first seen (epoch ms).
#[derive(Clone, Debug, PartialEq)]
pub struct Problem {
    pub file: String,
    pub reason: String,
    pub since: u64,
}

/// One listing entry, as Node's `Dirent`: the type the folder listing itself reports, never a
/// stat of the name (a symlink is neither a file nor a folder here).
#[derive(Clone, Debug)]
pub struct Dirent {
    pub name: String,
    pub is_dir: bool,
    pub is_file: bool,
}

/// A read that went past the cap after the stat: the caller refuses it by name, as the
/// TypeScript reader refuses the over-cap bytes `readRegularFile` hands back.
#[derive(Debug)]
pub enum ReadError {
    OverCap,
    Io(io::Error),
}

/// The share operations one scan uses. Errors keep their OS code: the scanners tell ENOENT
/// and ESTALE apart from a lasting failure exactly as the TypeScript readers do.
pub trait ShareFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<Dirent>>;
    fn stat(&self, file: &Path) -> io::Result<Stat>;
    fn read(&self, file: &Path, cap: usize) -> Result<Vec<u8>, ReadError>;
}

/// A listing's safety bound: past it the folder reads as a named listing failure. The
/// TypeScript reader lists unbounded; the share's own limits (writers, cards, runs) apply
/// first in every real scan.
const LIST_CAP: usize = 1_000_000;

/// The real share through the one file policy: a listing with a count cap, a stat that
/// follows links as `fs.stat` does, and a hardened read (a final link refused, a FIFO never
/// opened blocking, at most the cap plus one byte held).
pub struct NodeFs;

impl ShareFs for NodeFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<Dirent>> {
        let entries = aac_files::read::entries(dir, LIST_CAP)?;
        let mut out = Vec::new();
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(aac_files::read::Listed::TooMany) => {
                    return Err(io::Error::other("folder listing past the count cap"));
                }
                Err(aac_files::read::Listed::Entry(error)) => return Err(error),
            };
            let file_type = entry.file_type()?;
            let name = entry.file_name().to_string_lossy().into_owned();
            out.push(Dirent {
                name,
                is_dir: file_type.is_dir(),
                is_file: file_type.is_file(),
            });
        }
        Ok(out)
    }

    fn stat(&self, file: &Path) -> io::Result<Stat> {
        aac_files::read::stat(file)
    }

    fn read(&self, file: &Path, cap: usize) -> Result<Vec<u8>, ReadError> {
        // `readRegularFile` holds at most cap + 1 bytes: a file that grew past its cap after
        // the stat comes back over the cap, and the caller's own check refuses it by name.
        match aac_files::read::read(file, &ReadPolicy::capped(cap as u64 + 1)) {
            Read::Bytes(bytes, _) => Ok(bytes),
            Read::TooLarge => Err(ReadError::OverCap),
            // The TypeScript reader's open is O_NOFOLLOW | O_NONBLOCK with a regular-file
            // check on the handle: a link, a FIFO or a folder where a report belongs fails
            // the read (never a transient share error).
            Read::Link | Read::Directory | Read::NotRegular => {
                Err(ReadError::Io(io::Error::other("not a regular file")))
            }
            Read::Absent => Err(ReadError::Io(io::Error::from_raw_os_error(2))),
            Read::Unreadable(kind) | Read::ChainUnreadable(kind) => {
                Err(ReadError::Io(io::Error::new(kind, "share read failed")))
            }
            Read::Foreign | Read::BadMode | Read::Unproved => Err(ReadError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "share read refused",
            ))),
        }
    }
}

/// ENOENT or ENOTDIR: the name is not there (the readers' `isMissing`).
pub fn is_missing(error: &io::Error) -> bool {
    matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) || error.raw_os_error() == Some(20) // ENOTDIR where the kind does not say it
}

/// ENOENT or ESTALE: the share is between a delete and a rename, or holds a stale handle.
/// Node reports Linux ESTALE as "Unknown system error -116" on every platform it reads the
/// share from, so the raw code (116 or -116) is the fact, not the host's own errno table;
/// macOS also spells ESTALE 70.
pub fn is_transient(error: &io::Error) -> bool {
    if error.kind() == io::ErrorKind::NotFound {
        return true;
    }
    let raw = error.raw_os_error();
    raw == Some(116) || raw == Some(-116) || (cfg!(target_os = "macos") && raw == Some(70))
}

/// Consecutive transient errors per listed file: the first failure's time when the file
/// failed on the scan before too (a persistent problem), else None (the race, said nothing).
#[derive(Default)]
pub struct TransientMisses {
    first: HashMap<String, u64>,
}

impl TransientMisses {
    pub fn miss(&mut self, file: &str, now: u64) -> Option<u64> {
        match self.first.get(file) {
            Some(since) => Some(*since),
            None => {
                self.first.insert(file.to_string(), now);
                None
            }
        }
    }

    pub fn clear(&mut self, file: &str) {
        self.first.remove(file);
    }

    /// After a scan: a file not listed this scan starts over.
    pub fn keep_only(&mut self, seen: &std::collections::HashSet<String>) {
        self.first.retain(|file, _| seen.contains(file));
    }
}

/// A name safe to show: printable ASCII without separators, at most 100 characters.
pub fn showable(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && name
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && b != b'/' && b != b'\\')
}

/// `localeCompare` order on the proven printable-ASCII domain; a name outside it sorts by
/// bytes (the share's writer, card and registry names are ASCII in practice).
pub fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    aac_num::intl::ascii_compare(a, b).unwrap_or_else(|| a.as_bytes().cmp(b.as_bytes()))
}

/// The stat key the readers skip unchanged files by: size, mtime, inode, ctime, the numbers
/// as JavaScript writes them.
pub fn stat_key(stat: &Stat) -> String {
    format!(
        "{}:{}:{}:{}",
        stat.size,
        aac_num::ecma::to_string(stat.mtime_ms),
        stat.inode,
        aac_num::ecma::to_string(stat.ctime_ms)
    )
}

/// Hex SHA-256 of the bytes as read (the good copies and the readers check quote it).
pub fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|byte| format!("{byte:02x}")).collect()
}

/// `JSON.stringify`'s text for a parsed value, for the change keys the TypeScript readers
/// compare: numbers as JavaScript writes them (`1.0` is `1`, `-0` is `0`), objects in their
/// own order. Both sides of a comparison go through this one function.
pub fn js_text(value: &Json) -> String {
    match value {
        Json::Number(number) => aac_num::ecma::json(number.as_f64().unwrap_or(f64::NAN)),
        Json::Array(items) => {
            let parts: Vec<String> = items.iter().map(js_text).collect();
            format!("[{}]", parts.join(","))
        }
        Json::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .map(|(key, value)| format!("{}:{}", Json::from(key.clone()), js_text(value)))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        other => other.to_string(),
    }
}

/// The reports root: `AAC_AGENT_REPORTS_DIR`, else the shared folder (the TS reader's rule).
pub fn agent_reports_root(env_root: Option<&str>) -> PathBuf {
    match env_root.map(str::trim).filter(|root| !root.is_empty()) {
        Some(root) => PathBuf::from(root),
        None => machine_share_root().join("agent-reports"),
    }
}

/// The pm-mail registry folder: `AAC_PM_MAIL_REGISTRY_DIR`, else the shared inbox registry.
pub fn pm_mail_registry_dir(env_dir: Option<&str>) -> PathBuf {
    match env_dir.map(str::trim).filter(|dir| !dir.is_empty()) {
        Some(dir) => PathBuf::from(dir),
        None => machine_share_root().join("Inbox/registry"),
    }
}

/// A path a writer gave (VM, Mac or Windows spelling of the shared folder) -> where it is on
/// this computer, or None. Only paths inside the share that holds the reports folder are
/// looked at (`shareLocalPath` in agent-reports-reader.ts).
pub fn share_local_path(given: &str, root: &Path) -> Option<PathBuf> {
    let share_root = root.parent()?.to_path_buf();
    let lower = given.to_ascii_lowercase();
    let candidate = if lower.starts_with("/mnt/cursor/") {
        share_root.join(given["/mnt/cursor/".len()..].replace('\\', "/"))
    } else if lower.starts_with("/volumes/cursor/") {
        share_root.join(given["/volumes/cursor/".len()..].replace('\\', "/"))
    } else if lower.starts_with("\\\\truenas\\cursor\\") {
        share_root.join(given["\\\\truenas\\cursor\\".len()..].replace('\\', "/"))
    } else if path_is_absolute(given) {
        PathBuf::from(given)
    } else {
        return None;
    };
    // path.resolve on an absolute candidate is the candidate itself (normalized); the TS
    // check is a prefix of the share root plus a separator. Compare with one separator
    // spelling so a POSIX root reads the same on every host the readers run on.
    let normalized = normalize_absolute(&candidate)?;
    let text = normalized.to_string_lossy().replace('\\', "/");
    let prefix = format!("{}/", share_root.to_string_lossy().replace('\\', "/"));
    if text.starts_with(&prefix) {
        Some(normalized)
    } else {
        None
    }
}

/// `path.isAbsolute` on the platform the reader runs on: POSIX on Unix; drive letters, UNC
/// and rooted paths on Windows (the dashboard host is the VM; the rule stays the reference's).
fn path_is_absolute(path: &str) -> bool {
    #[cfg(unix)]
    {
        path.starts_with('/')
    }
    #[cfg(not(unix))]
    {
        let bytes = path.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'\\' || bytes[2] == b'/'))
            || path.starts_with("\\\\")
            || path.starts_with('/')
            || path.starts_with('\\')
    }
}

/// Lexical normalization of an absolute path (`path.resolve`'s effect on one): `.` and `..`
/// segments resolved, separators kept as the platform's.
fn normalize_absolute(path: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::RootDir => out.push(Component::RootDir.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

/// The mounted Cursor share on this platform.
#[must_use]
pub fn machine_share_root() -> &'static Path {
    Path::new(if cfg!(windows) {
        r"\\TRUENAS\Cursor"
    } else if cfg!(target_os = "macos") {
        "/Volumes/Cursor"
    } else {
        "/mnt/Cursor"
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_are_enoent_and_estale_only() {
        assert!(is_transient(&io::Error::from_raw_os_error(2)));
        assert!(!is_transient(&io::Error::from_raw_os_error(13)));
        assert!(is_missing(&io::Error::from_raw_os_error(2)));
        assert!(is_missing(&io::Error::from_raw_os_error(20)));
        assert!(!is_missing(&io::Error::from_raw_os_error(116)));
        // ESTALE is the share's stale handle, as Node reports it (-116) on every platform.
        assert!(is_transient(&io::Error::from_raw_os_error(116)));
        assert!(is_transient(&io::Error::from_raw_os_error(-116)));
    }

    #[test]
    fn misses_name_the_second_scan_only() {
        let mut misses = TransientMisses::default();
        assert_eq!(misses.miss("a", 10), None);
        assert_eq!(misses.miss("a", 20), Some(10));
        misses.clear("a");
        assert_eq!(misses.miss("a", 30), None);
        let mut seen = std::collections::HashSet::new();
        seen.insert("b".to_string());
        misses.miss("a", 40);
        misses.keep_only(&seen);
        assert_eq!(misses.miss("a", 50), None);
    }

    #[test]
    fn showable_names_are_printable_ascii_without_separators() {
        assert!(showable("vm.lead"));
        assert!(showable(&"a".repeat(100)));
        assert!(!showable(&"a".repeat(101)));
        assert!(!showable("a/b"));
        assert!(!showable("a\\b"));
        assert!(!showable(""));
        assert!(!showable("a b"));
        assert!(!showable("a\u{7f}"));
    }

    #[cfg(unix)]
    #[test]
    fn share_paths_stay_inside_the_share() {
        let root = Path::new("/mnt/Cursor/agent-reports");
        assert_eq!(
            share_local_path("/mnt/Cursor/share/board.md", root),
            Some(PathBuf::from("/mnt/Cursor/share/board.md"))
        );
        assert_eq!(
            share_local_path("\\\\TRUENAS\\Cursor\\share\\board.md", root),
            Some(PathBuf::from("/mnt/Cursor/share/board.md"))
        );
        assert_eq!(share_local_path("/etc/passwd", root), None);
        assert_eq!(share_local_path("/mnt/Cursor/../etc/passwd", root), None);
        assert_eq!(share_local_path("relative.md", root), None);
    }
}
