//! Bounded folder walks and link rules, as the Python helper applies them.

use crate::collector::Collector;
use crate::util::{self, abspath, normpath, path_text, realpath};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const WALK_MAX_DIRS: usize = 10_000;
pub const WALK_MAX_ENTRIES: usize = 100_000;
pub const T3_MAX_HOMES: usize = 64;
pub const SCAN_SKIP_DIRS: [&str; 10] = [
    "node_modules",
    ".git",
    "tests",
    "test",
    "fixtures",
    "__fixtures__",
    "target",
    "dist",
    "coverage",
    "test-results",
];

pub fn skipped(name: &str) -> bool {
    SCAN_SKIP_DIRS.contains(&name)
}

/// A symlink or (Windows) junction: Rust's `is_symlink` covers both name-surrogate reparse points.
pub fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

pub fn is_dir(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

pub fn is_file(path: &Path) -> bool {
    fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Sorted entry names of a folder (Python's `sorted(os.listdir(...))`), None when unreadable.
pub fn sorted_names(directory: &Path) -> Option<Vec<String>> {
    let mut names: Vec<String> = fs::read_dir(directory)
        .ok()?
        .filter_map(|entry| entry.ok().map(|e| e.file_name().to_string_lossy().into_owned()))
        .collect();
    names.sort();
    Some(names)
}

/// Python's `_iter_jsonl_files`: depth-first (a stack), names sorted, links never descended, caps
/// flag truncation. `skip_dir` names folders that never hold logs.
pub fn walk_files(
    root: &Path,
    accept: &dyn Fn(&str) -> bool,
    collector: &mut Collector,
    skip_dir: Option<&dyn Fn(&str) -> bool>,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let mut visited_dirs = 0usize;
    let mut visited_entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if visited_dirs >= WALK_MAX_DIRS || collector.expired() {
            collector.truncated = true;
            return found;
        }
        visited_dirs += 1;
        let Some(names) = sorted_names(&directory) else {
            continue;
        };
        for name in names {
            if visited_entries >= WALK_MAX_ENTRIES {
                collector.truncated = true;
                return found;
            }
            visited_entries += 1;
            if skipped(&name) {
                continue;
            }
            let path = directory.join(&name);
            if is_dir(&path) && !is_link(&path) {
                if skip_dir.is_some_and(|skip| skip(&name)) {
                    continue;
                }
                if depth < 64 {
                    pending.push((path, depth + 1));
                }
            } else if is_file(&path) && accept(&name) {
                found.push(path);
            }
        }
    }
    found
}

/// Sub-folders of base (a link to a folder counts), by name, at most T3_MAX_HOMES.
pub fn child_dirs(base: &Path) -> Vec<PathBuf> {
    let Some(names) = sorted_names(base) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for name in names {
        if found.len() >= T3_MAX_HOMES {
            break;
        }
        let path = base.join(&name);
        if is_dir(&path) {
            found.push(path);
        }
    }
    found
}

/// Python's `_same_or_inside` (case-insensitive on Windows, like `os.path.normcase`).
pub fn same_or_inside(child: &Path, roots: &[PathBuf]) -> bool {
    let fold = |p: &Path| {
        let text = path_text(p);
        if cfg!(windows) {
            text.replace('/', "\\").to_lowercase()
        } else {
            text
        }
    };
    let child = fold(child);
    let sep = std::path::MAIN_SEPARATOR;
    roots.iter().any(|root| {
        let root = fold(root);
        let trimmed = root.trim_end_matches(['\\', '/']);
        child == root || child.starts_with(&format!("{trimmed}{sep}"))
    })
}

/// The real path of the link at `path`, or None when its target leaves `start`. The written target
/// is checked before any stat, so a link to a network mount is never touched.
pub fn link_inside(path: &Path, start: &Path) -> Option<PathBuf> {
    let written = fs::read_link(path).ok()?;
    let mut text = path_text(&written);
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        text = format!(r"\\{rest}");
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        text = rest.to_string();
    }
    let target = normpath(&path.parent().unwrap_or(Path::new("")).join(text));
    if !same_or_inside(&target, &[start.to_path_buf()]) {
        return None;
    }
    let real = realpath(path);
    if same_or_inside(&real, &[start.to_path_buf()]) {
        Some(real)
    } else {
        None
    }
}

/// Python's `_dedup_dirs`: canonical roots, widest first, an overlap read once.
pub fn dedup_dirs(candidates: &[PathBuf]) -> Vec<PathBuf> {
    let mut reals: Vec<PathBuf> = candidates.iter().map(|c| realpath(c)).collect();
    reals.sort_by(|a, b| {
        let (ta, tb) = (path_text(a), path_text(b));
        ta.chars().count().cmp(&tb.chars().count()).then(ta.cmp(&tb))
    });
    reals.dedup();
    let mut dirs: Vec<PathBuf> = Vec::new();
    for real in reals {
        if !same_or_inside(&real, &dirs) {
            dirs.push(real);
        }
    }
    dirs
}

/// File identity (device, inode) for "each real file once". On Windows, as Python's `os.stat`:
/// the volume serial number and the 64-bit file index.
pub fn identity(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path).ok()?;
        Some((meta.dev(), meta.ino()))
    }
    #[cfg(windows)]
    {
        windows_identity(path)
    }
}

#[cfg(windows)]
fn windows_identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS, FILE_SHARE_DELETE, FILE_SHARE_READ,
        FILE_SHARE_WRITE, GetFileInformationByHandle, OPEN_EXISTING,
    };
    // Past 260 characters CreateFileW needs the verbatim prefix (Rust's std adds it for its own
    // calls; this one is direct). Claude Code writes project folders longer than that.
    let text = path_text(path);
    let verbatim = if text.starts_with(r"\\?\") {
        text
    } else if let Some(rest) = text.strip_prefix(r"\\") {
        format!(r"\\?\UNC\{rest}")
    } else if text.len() > 2 && text.as_bytes()[1] == b':' {
        format!(r"\\?\{}", text.replace('/', "\\"))
    } else {
        text
    };
    let wide: Vec<u16> = std::ffi::OsStr::new(&verbatim)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    // SAFETY: a NUL-terminated path; zero access rights only query attributes; the handle is closed.
    unsafe {
        let handle = CreateFileW(
            wide.as_ptr(),
            0,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            std::ptr::null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS,
            std::ptr::null_mut(),
        );
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let mut info: BY_HANDLE_FILE_INFORMATION = std::mem::zeroed();
        let ok = GetFileInformationByHandle(handle, &mut info);
        CloseHandle(handle);
        if ok == 0 {
            return None;
        }
        Some((
            info.dwVolumeSerialNumber as u64,
            ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
        ))
    }
}

/// Python's `_iter_t3_files`: wanted files under one non-default root. Links are followed, but only
/// to a target inside the root's own real path; a root or folder inside `exclude` (the real default
/// roots) is not read; each folder is entered once by identity.
pub fn walk_t3_files(
    root: &Path,
    accept: &dyn Fn(&str) -> bool,
    collector: &mut Collector,
    exclude: &[PathBuf],
) -> Vec<(PathBuf, (u64, u64))> {
    let mut found = Vec::new();
    let start = realpath(root);
    if !is_dir(&start) || same_or_inside(&start, exclude) {
        return found;
    }
    let mut pending: Vec<(PathBuf, usize)> = vec![(start.clone(), 0)];
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    let mut entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if seen.len() >= WALK_MAX_DIRS || collector.expired() {
            collector.truncated = true;
            return found;
        }
        match identity(&directory) {
            Some(id) => {
                if !seen.insert(id) {
                    continue;
                }
            }
            None => continue,
        }
        let Some(names) = sorted_names(&directory) else {
            continue;
        };
        for name in names {
            entries += 1;
            if entries > WALK_MAX_ENTRIES {
                collector.truncated = true;
                return found;
            }
            if skipped(&name) {
                continue;
            }
            let path = directory.join(&name);
            let real = if is_link(&path) {
                link_inside(&path, &start)
            } else {
                Some(path.clone())
            };
            let Some(real) = real else {
                continue;
            };
            if same_or_inside(&real, exclude) {
                continue;
            }
            if is_dir(&real) {
                if depth < 64 {
                    pending.push((real, depth + 1));
                }
            } else if is_file(&real)
                && accept(&name)
                && let Some(id) = identity(&real)
            {
                found.push((real, id));
            }
        }
    }
    found
}

/// Python's `os.path.isabs` for a configured path, accepting a Windows drive path on any host.
pub fn absolute_text(text: &str) -> bool {
    Path::new(text).is_absolute()
        || (text.len() > 2 && text.as_bytes()[1] == b':' && matches!(text.as_bytes()[2], b'\\' | b'/'))
}

/// A home-relative path from configuration (`~`, `~/x`, `~\x`) to an absolute one; None otherwise.
pub fn home_path(value: Option<&serde_json::Value>, home: &Path) -> Option<PathBuf> {
    let text = util::py_strip(value?.as_str()?);
    if text.is_empty() || text.chars().count() > 1024 || text.contains('\0') {
        return None;
    }
    let expanded = if text == "~" {
        home.to_path_buf()
    } else if let Some(rest) = text.strip_prefix("~/").or_else(|| text.strip_prefix("~\\")) {
        home.join(rest)
    } else {
        PathBuf::from(text)
    };
    if expanded.is_absolute() {
        Some(normpath(&expanded))
    } else {
        None
    }
}

pub fn exists(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok()
}

pub fn abs(path: &Path) -> PathBuf {
    abspath(path)
}
