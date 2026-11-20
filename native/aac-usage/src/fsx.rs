//! Bounded folder walks and link rules, as the Python helper applies them; and the guards that keep a
//! scan answering (FIX-ROUND-BIG 9): network and FUSE mounts are probed once on a thread before any
//! walk touches them, a mount that does not answer is never stat'ed again, and only regular files
//! are opened.

use crate::collector::Collector;
use crate::util::{self, abspath, normpath, path_text, realpath};
use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

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

/// Folders a walk from home never enters (FIX-ROUND-BIG 15, Fable L12): cloud-synced folders, whose
/// files may be placeholders that a read downloads, and the personal folders a Mac guards with a
/// consent prompt. Tool homes are never kept there by default; a configured root there is still read.
pub fn personal_or_cloud(name: &str) -> bool {
    const NAMES: [&str; 14] = [
        "OneDrive",
        "Dropbox",
        "iCloudDrive",
        "iCloud Drive",
        "Google Drive",
        "Library",
        "AppData",
        "Desktop",
        "Documents",
        "Downloads",
        "Pictures",
        "Music",
        "Movies",
        "Videos",
    ];
    NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
        || name.starts_with("OneDrive - ")
        || name.starts_with("OneDrive-")
        || name.starts_with("GoogleDrive-")
}

/// Mounts that did not answer their probe this scan: nothing under them is stat'ed, listed or opened.
static AVOID: Mutex<Vec<String>> = Mutex::new(Vec::new());

fn fold(text: &str) -> String {
    if cfg!(windows) {
        text.replace('/', "\\").to_lowercase()
    } else {
        text.to_string()
    }
}

fn under(text: &str, mount: &str) -> bool {
    let trimmed = mount.trim_end_matches(['\\', '/']);
    text == mount || text.starts_with(&format!("{trimmed}{}", std::path::MAIN_SEPARATOR))
}

fn avoided_text(text: &str) -> bool {
    let list = AVOID.lock().unwrap_or_else(|e| e.into_inner());
    if list.is_empty() {
        return false;
    }
    let folded = fold(text);
    list.iter().any(|mount| under(&folded, mount))
}

/// Record a mount (or folder) that did not answer: never touched again this scan.
pub fn avoid(path: &Path) {
    let text = fold(&path_text(&abspath(path)));
    let mut list = AVOID.lock().unwrap_or_else(|e| e.into_inner());
    if !list.contains(&text) {
        list.push(text);
    }
}

/// How many avoided mounts lie under `base` (a walk from home would have looked there).
pub fn avoided_under(base: &Path) -> usize {
    let base = fold(&path_text(&abspath(base)));
    AVOID
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .filter(|mount| under(mount, &base))
        .count()
}

/// Tests only: forget the mounts recorded by `avoid`.
pub fn reset_avoided() {
    AVOID.lock().unwrap_or_else(|e| e.into_inner()).clear();
}

/// True when `path`, or what its links point at (followed by their written text, never by a stat
/// past an avoided mount), lies under a mount that did not answer.
pub fn avoided(path: &Path) -> bool {
    if AVOID.lock().unwrap_or_else(|e| e.into_inner()).is_empty() {
        return false;
    }
    let mut current = abspath(path);
    for _ in 0..8 {
        if avoided_text(&path_text(&current)) {
            return true;
        }
        let Ok(meta) = fs::symlink_metadata(&current) else {
            return false;
        };
        if !meta.file_type().is_symlink() {
            return false;
        }
        let Ok(target) = fs::read_link(&current) else {
            return false;
        };
        current = normpath(&current.parent().unwrap_or(Path::new("")).join(target));
    }
    false
}

/// A symlink or (Windows) junction: Rust's `is_symlink` covers name-surrogate reparse points only,
/// so a cloud placeholder (OneDrive) or an app-execution alias is not a link (FIX-ROUND-BIG 15).
pub fn is_link(path: &Path) -> bool {
    !avoided_text(&path_text(&abspath(path)))
        && fs::symlink_metadata(path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false)
}

pub fn is_dir(path: &Path) -> bool {
    !avoided(path) && fs::metadata(path).map(|m| m.is_dir()).unwrap_or(false)
}

pub fn is_file(path: &Path) -> bool {
    !avoided(path) && fs::metadata(path).map(|m| m.is_file()).unwrap_or(false)
}

/// Discovery must distinguish absence from a failed stat, including an avoided mount.
pub fn metadata(path: &Path) -> io::Result<fs::Metadata> {
    if avoided(path) {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "did not answer"));
    }
    fs::metadata(path)
}

/// A failed discovery filter makes the kind partial and names the unvisited path. Only NotFound
/// is absence; retaining unvisited usage and fingerprints is then the server's existing rule.
pub fn discovery_metadata(collector: &mut Collector, kind: &str, path: &Path) -> Option<fs::Metadata> {
    match metadata(path) {
        Ok(meta) => Some(meta),
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => None,
        Err(_) => {
            // A failed stat does not establish a folder. In particular, a busy regular file,
            // SQLite sidecar or broken file link must never inflate unreadableFolders.
            // unreadable_folder checks for an avoided mount before its own stat, so a mount
            // that did not answer is never stat'ed again here (REVIEW-cheap M3).
            collector.unreadable_folder(kind, path);
            None
        }
    }
}

/// A cloud-only placeholder (Windows: recall on open or on data access, or offline): reading it
/// would download it, so a walk never opens one (FIX-ROUND-BIG 15).
pub fn placeholder(meta: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const OFFLINE: u32 = 0x1000;
        const RECALL_ON_OPEN: u32 = 0x40000;
        const RECALL_ON_DATA_ACCESS: u32 = 0x400000;
        meta.file_attributes() & (OFFLINE | RECALL_ON_OPEN | RECALL_ON_DATA_ACCESS) != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

/// Open a file only when it is a regular file (FIX-ROUND-BIG 9): a FIFO, socket or device would
/// block or never end. On Unix the open itself cannot block (O_NONBLOCK, checked on the handle).
pub fn open_regular(path: &Path) -> io::Result<fs::File> {
    if avoided(path) {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "did not answer"));
    }
    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?
    };
    #[cfg(not(unix))]
    let file = fs::File::open(path)?;
    let meta = file.metadata()?;
    if !meta.is_file() || placeholder(&meta) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"));
    }
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        // SAFETY: an open descriptor; only the non-blocking flag is cleared again.
        unsafe {
            let fd = file.as_raw_fd();
            let flags = libc::fcntl(fd, libc::F_GETFL);
            if flags >= 0 {
                libc::fcntl(fd, libc::F_SETFL, flags & !libc::O_NONBLOCK);
            }
        }
    }
    Ok(file)
}

/// A whole regular file of at most `limit` bytes; None when larger, not regular or unreadable.
pub fn read_regular(path: &Path, limit: u64) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = open_regular(path).ok()?;
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes).ok()?;
    (bytes.len() as u64 <= limit).then_some(bytes)
}

/// A folder's entry names, sorted (Python's `sorted(os.listdir(...))`), or why it cannot be
/// listed. An entry that fails mid-listing fails the listing: a short list passed off as whole
/// would silently lose the logs behind it (RECHECK-BIG-1 M3).
pub fn list_dir(directory: &Path) -> io::Result<Vec<String>> {
    if avoided(directory) {
        return Err(io::Error::new(io::ErrorKind::TimedOut, "did not answer"));
    }
    let mut names: Vec<String> = Vec::new();
    for entry in fs::read_dir(directory)? {
        names.push(entry?.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    Ok(names)
}

/// Sorted entry names of a folder (Python's `sorted(os.listdir(...))`), None when unreadable.
pub fn sorted_names(directory: &Path) -> Option<Vec<String>> {
    list_dir(directory).ok()
}

/// A folder inside a walk that exists but cannot be listed (FIX-ROUND-BIG 4): named, never silent.
fn listing(collector: &mut Collector, kind: &str, directory: &Path) -> Option<Vec<String>> {
    match list_dir(directory) {
        Ok(names) => Some(names),
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => None,
        Err(_) => {
            collector.unreadable_folder(kind, directory);
            None
        }
    }
}

/// True when a folder past the depth cap holds an entry a walk would have looked at. Python drops
/// such folders silently; this port says partial instead, but only when the cap leaves something
/// unread: an empty folder (or one holding only skipped names) is a complete read (big review
/// Wonder L7). An unreadable listing is unread.
fn leaves_unread(directory: &Path) -> bool {
    match fs::read_dir(directory) {
        Ok(entries) => entries
            .into_iter()
            .any(|entry| entry.map_or(true, |e| !skipped(&e.file_name().to_string_lossy()))),
        Err(_) => true,
    }
}

/// Python's `_iter_jsonl_files`: depth-first (a stack), names sorted, links never descended, caps
/// flag truncation. `skip_dir` names folders that never hold logs.
pub fn walk_files(
    kind: &str,
    root: &Path,
    accept: &dyn Fn(&str) -> bool,
    collector: &mut Collector,
    skip_dir: Option<&dyn Fn(&str) -> bool>,
) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending: Vec<(PathBuf, usize)> = vec![(root.to_path_buf(), 0)];
    let counted_root = util::realpath(root);
    let mut visited_dirs = 0usize;
    let mut visited_entries = 0usize;
    while let Some((directory, depth)) = pending.pop() {
        if visited_dirs >= WALK_MAX_DIRS || collector.expired() {
            collector.truncated = true;
            return found;
        }
        visited_dirs += 1;
        let Some(names) = listing(collector, kind, &directory) else {
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
            if accept(&name) && collector.skip_settled(kind, &counted_root, &path) {
                continue;
            }
            // Never follow a directory link just to classify it (FIX-ROUND-BIG 9).
            let link = is_link(&path);
            if link && !accept(&name) {
                continue;
            }
            let Some(meta) = discovery_metadata(collector, kind, &path) else {
                continue;
            };
            if !link && meta.is_dir() {
                if skip_dir.is_some_and(|skip| skip(&name)) {
                    continue;
                }
                if depth < 64 {
                    pending.push((path, depth + 1));
                } else if leaves_unread(&path) {
                    // The depth cap leaves this folder unread: the tool is partial, never a
                    // complete read that found nothing (search-bound REVIEW-cheap M1).
                    collector.usage_partial.insert(kind.to_string());
                }
            } else if accept(&name) && meta.is_file() && !placeholder(&meta) {
                found.push(path);
            }
        }
    }
    found
}

/// A regular file that is no cloud placeholder (links followed, unless onto an avoided mount).
pub fn regular(path: &Path) -> bool {
    !avoided(path) && fs::metadata(path).is_ok_and(|m| m.is_file() && !placeholder(&m))
}

/// Sub-folders of base (a link to a folder counts), by name, at most T3_MAX_HOMES.
pub fn child_dirs(base: &Path) -> Vec<PathBuf> {
    child_dirs_checked(base).0
}

/// `child_dirs`, with whether the base itself could not be listed for a reason other than not
/// being there: an unreadable account base is named, not silently empty (RECHECK-BIG-1 M3).
pub fn child_dirs_checked(base: &Path) -> (Vec<PathBuf>, bool) {
    let names = match list_dir(base) {
        Ok(names) => names,
        Err(error) => {
            return (
                Vec::new(),
                !matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory),
            );
        }
    };
    let mut found = Vec::new();
    let mut failed = false;
    for name in names {
        if found.len() >= T3_MAX_HOMES {
            break;
        }
        let path = base.join(&name);
        match metadata(&path) {
            Ok(meta) if meta.is_dir() => found.push(path),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(_) => failed = true,
        }
    }
    (found, failed)
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
    if !same_or_inside(&target, &[start.to_path_buf()]) || avoided(&target) {
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
    if avoided(path) {
        return None;
    }
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

/// An existing stat supplies the inode on Unix. Windows uses an attribute-only handle for the
/// file index, which ordinary Rust metadata doesn't expose.
pub fn metadata_identity(_path: &Path, _metadata: &fs::Metadata) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((_metadata.dev(), _metadata.ino()))
    }
    #[cfg(windows)]
    {
        windows_identity(_path)
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
    kind: &str,
    root: &Path,
    accept: &dyn Fn(&str) -> bool,
    collector: &mut Collector,
    exclude: &[PathBuf],
) -> Vec<(PathBuf, (u64, u64))> {
    let mut found = Vec::new();
    let start = realpath(root);
    if same_or_inside(&start, exclude) || !discovery_metadata(collector, kind, &start).is_some_and(|meta| meta.is_dir())
    {
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
            None => {
                if !matches!(metadata(&directory), Err(error) if error.kind() == io::ErrorKind::NotFound) {
                    collector.unreadable_folder(kind, &directory);
                }
                continue;
            }
        }
        let Some(names) = listing(collector, kind, &directory) else {
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
            if accept(&name) && !same_or_inside(&path, exclude) && collector.skip_settled(kind, &start, &path) {
                continue;
            }
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
            if real != path && accept(&name) && collector.skip_settled(kind, &start, &real) {
                continue;
            }
            let Some(meta) = discovery_metadata(collector, kind, &real) else {
                continue;
            };
            if meta.is_dir() {
                if depth < 64 {
                    pending.push((real, depth + 1));
                } else if leaves_unread(&real) {
                    collector.usage_partial.insert(kind.to_string());
                }
            } else if accept(&name) && meta.is_file() && !placeholder(&meta) {
                match identity(&real) {
                    Some(id) => found.push((real, id)),
                    None => {
                        if !matches!(metadata(&real), Err(error) if error.kind() == io::ErrorKind::NotFound) {
                            collector.read_failure(kind, util::filekey(kind, &real));
                        }
                    }
                }
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
    !avoided_text(&path_text(&abspath(path))) && fs::symlink_metadata(path).is_ok()
}

pub fn abs(path: &Path) -> PathBuf {
    abspath(path)
}

/// A log folder's (or database's) probe (DESIGN 16.4, FIX-ROUND-BIG 4 and 9): its real path when it
/// answers and can be read, or why not.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Probe {
    Readable(PathBuf),
    /// Not there at all (and not a link): an ordinary absent folder, said nothing about.
    Absent,
    /// There, but it cannot be read: `state` is the root's state for the report contract
    /// (`unreadable`, `link_unreadable` or `link_unreachable`) and `cause` says why
    /// (`permission_denied`, `io_error`, `target_missing` or `did_not_answer`).
    Failed {
        state: &'static str,
        cause: &'static str,
    },
}

pub fn error_cause(error: &io::Error) -> &'static str {
    match error.kind() {
        io::ErrorKind::PermissionDenied => "permission_denied",
        io::ErrorKind::TimedOut => "did_not_answer",
        _ => "io_error",
    }
}

fn failed(linked: bool, error: &io::Error) -> Probe {
    let cause = error_cause(error);
    Probe::Failed {
        state: if !linked {
            "unreadable"
        } else if cause == "did_not_answer" {
            "link_unreachable"
        } else {
            "link_unreadable"
        },
        cause,
    }
}

/// A link whose target is not there (a dangling symlink or Windows junction): its written target,
/// resolved against the link's folder, does not exist.
fn dangling(path: &Path) -> bool {
    let Ok(target) = fs::read_link(path) else {
        return false;
    };
    let target = path.parent().unwrap_or(Path::new("")).join(target);
    matches!(fs::symlink_metadata(&target), Err(e) if e.kind() == io::ErrorKind::NotFound)
}

fn probe_one(path: &Path, folder: bool, linked: bool) -> Probe {
    if avoided(path) {
        return failed(linked, &io::Error::new(io::ErrorKind::TimedOut, "did not answer"));
    }
    let real = match fs::canonicalize(path) {
        Ok(real) => real,
        Err(error) if linked && (error.kind() == io::ErrorKind::NotFound || dangling(path)) => {
            return Probe::Failed {
                state: "link_unreachable",
                cause: "target_missing",
            };
        }
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => {
            return Probe::Absent;
        }
        Err(error) => return failed(linked, &error),
    };
    let readable = if folder {
        fs::read_dir(&real).map(|_| ())
    } else {
        fs::metadata(&real).and_then(|meta| {
            if meta.is_file() {
                Ok(())
            } else {
                Err(io::Error::new(io::ErrorKind::InvalidInput, "not a regular file"))
            }
        })
    };
    match readable {
        Ok(()) => Probe::Readable(real),
        Err(error) if matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory) => Probe::Absent,
        Err(error) => failed(linked, &error),
    }
}

/// Probe each folder on its own thread, all within `wait` together: a stat of a dead network mount
/// can block for minutes, and a thread left waiting on one never holds up the scan. Folders are
/// returned in order, each with its probe; one that did not answer is avoided from then on.
pub fn probe_dirs(dirs: &[PathBuf], wait: Duration) -> Vec<(PathBuf, Probe)> {
    probe_paths(dirs, wait, true)
}

/// `probe_dirs` for files (databases): readable means a regular file.
pub fn probe_files(files: &[PathBuf], wait: Duration) -> Vec<(PathBuf, Probe)> {
    probe_paths(files, wait, false)
}

enum Heard {
    /// The path's own lstat answered: whether it is a link.
    Linked(usize, bool),
    Done(usize, Probe),
}

fn probe_paths(paths: &[PathBuf], wait: Duration, folder: bool) -> Vec<(PathBuf, Probe)> {
    let deadline = std::time::Instant::now() + wait;
    let (sender, receiver) = std::sync::mpsc::channel();
    for (index, path) in paths.iter().enumerate() {
        let sender = sender.clone();
        let path = path.clone();
        std::thread::spawn(move || {
            let linked = is_link(&path);
            let _ = sender.send(Heard::Linked(index, linked));
            let _ = sender.send(Heard::Done(index, probe_one(&path, folder, linked)));
        });
    }
    drop(sender);
    let mut probes: Vec<Option<Probe>> = vec![None; paths.len()];
    let mut links: Vec<Option<bool>> = vec![None; paths.len()];
    let mut left = paths.len();
    while left > 0 {
        let now = std::time::Instant::now();
        if now >= deadline {
            break;
        }
        match receiver.recv_timeout(deadline - now) {
            Ok(Heard::Linked(index, linked)) => links[index] = Some(linked),
            Ok(Heard::Done(index, probe)) => {
                probes[index] = Some(probe);
                left -= 1;
            }
            Err(_) => break,
        }
    }
    paths
        .iter()
        .cloned()
        .zip(probes.into_iter().zip(links))
        .map(|(path, (probe, linked))| {
            let probe = probe.unwrap_or_else(|| {
                // Not answered: never stat'ed again this scan. A link whose own lstat answered is
                // named as one, and its written target (read without touching it) is avoided too.
                let linked = linked == Some(true);
                if linked && let Ok(target) = fs::read_link(&path) {
                    avoid(&normpath(&path.parent().unwrap_or(Path::new("")).join(target)));
                }
                avoid(&path);
                failed(linked, &io::Error::new(io::ErrorKind::TimedOut, "did not answer"))
            });
            (path, probe)
        })
        .collect()
}

/// Network and FUSE mounts from the mount table (read without touching any mount): where a stat
/// can hang. Linux: /proc/self/mountinfo; macOS: getfsstat without waiting. None on Windows.
pub fn network_mounts() -> Vec<PathBuf> {
    const TYPES: [&str; 14] = [
        "nfs",
        "nfs4",
        "cifs",
        "smb3",
        "smbfs",
        "sshfs",
        "9p",
        "davfs",
        "webdav",
        "afpfs",
        "ncpfs",
        "glusterfs",
        "ceph",
        "autofs",
    ];
    let network = |fstype: &str| TYPES.contains(&fstype) || fstype.starts_with("fuse") || fstype.contains("fuse");
    #[cfg(target_os = "linux")]
    {
        let Ok(text) = fs::read_to_string("/proc/self/mountinfo") else {
            return Vec::new();
        };
        let unescape = |field: &str| {
            let bytes = field.as_bytes();
            let mut out = Vec::with_capacity(bytes.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'\\'
                    && i + 3 < bytes.len()
                    && bytes[i + 1..i + 4].iter().all(|b| (b'0'..=b'7').contains(b))
                {
                    out.push((bytes[i + 1] - b'0') * 64 + (bytes[i + 2] - b'0') * 8 + (bytes[i + 3] - b'0'));
                    i += 4;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            String::from_utf8_lossy(&out).into_owned()
        };
        text.lines()
            .filter_map(|line| {
                let (left, right) = line.split_once(" - ")?;
                let point = left.split(' ').nth(4)?;
                let fstype = right.split(' ').next()?;
                network(fstype).then(|| PathBuf::from(unescape(point)))
            })
            .collect()
    }
    #[cfg(target_os = "macos")]
    {
        // SAFETY: getfsstat fills at most `count` statfs records; MNT_NOWAIT never asks a mount.
        unsafe {
            let count = libc::getfsstat(std::ptr::null_mut(), 0, libc::MNT_NOWAIT);
            if count <= 0 {
                return Vec::new();
            }
            let mut buffer: Vec<libc::statfs> = Vec::with_capacity(count as usize + 8);
            let size = (buffer.capacity() * std::mem::size_of::<libc::statfs>()) as libc::c_int;
            let got = libc::getfsstat(buffer.as_mut_ptr(), size, libc::MNT_NOWAIT);
            if got <= 0 {
                return Vec::new();
            }
            buffer.set_len(got as usize);
            buffer
                .iter()
                .filter_map(|record| {
                    let text = |chars: &[libc::c_char]| {
                        let bytes: Vec<u8> = chars.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
                        String::from_utf8_lossy(&bytes).into_owned()
                    };
                    network(&text(&record.f_fstypename)).then(|| PathBuf::from(text(&record.f_mntonname)))
                })
                .collect()
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = network;
        Vec::new()
    }
}

/// Probe this computer's network and FUSE mounts once, together, before any walk: one that does not
/// answer in `wait` is avoided for the rest of the scan. Returns how many did not answer.
pub fn guard_mounts(wait: Duration) -> usize {
    let mounts = network_mounts();
    probe_dirs(&mounts, wait)
        .into_iter()
        .filter(|(_, probe)| {
            matches!(
                probe,
                Probe::Failed {
                    cause: "did_not_answer",
                    ..
                }
            )
        })
        .count()
}
