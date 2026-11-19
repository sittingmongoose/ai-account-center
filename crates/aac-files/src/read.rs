//! The one hardened read (L2). Every Rust reader of a private or reference file opens it here.
//! With [`Ancestors::NoLinkBelow`] on Unix the folders below the base are opened as a descriptor
//! chain (`openat`, each `O_NOFOLLOW | O_DIRECTORY`) while they are checked, and the leaf is
//! lstat'd and opened relative to the pinned parent: a folder swapped for a link at any point
//! cannot redirect the read (Wonder L12). The leaf itself: one `lstat`, one open with
//! `O_NOFOLLOW | O_NONBLOCK | O_NOCTTY | O_CLOEXEC`, then every decision on that descriptor's
//! `fstat` plus a (dev, ino) identity check against the `lstat`, so a link, FIFO or other file
//! swapped in at the final name cannot change what is read either. With [`Ancestors::Any`] the
//! caller admitted the folders (or the reference follows them) and the leaf is opened by path;
//! the identity check still holds for the final name. On Windows the open is the reference's
//! plain open (no `O_NOFOLLOW`; owner, mode and identity pass trivially); native qualification
//! is an M4 gate item.
//!
//! The policy names the rules; [`Read`] names what was found. Each caller maps a [`Read`] to its
//! own reference's meaning (absent, unseen, incomplete or a fallback): this module never decides
//! that a refused file reads as empty.
use crate::path;
#[cfg(not(unix))]
use crate::path::Walk;
use std::{
    fmt,
    fs::{self, File},
    io::{self, Read as _},
    path::{Path, PathBuf},
};

/// The descriptor's own metadata (`fstat`), never the path's.
pub type Meta = fs::Metadata;

/// The facts every admission check reads, from an `lstat`, an `fstatat` or an `fstat`.
///
/// Owner, mode and identity are Unix facts: Windows has no owner check, no POSIX bits and no
/// `(dev, ino)` pair to compare, so those checks are not compiled there.
trait Facts {
    fn link(&self) -> bool;
    fn regular(&self) -> bool;
    fn size(&self) -> u64;
    #[cfg(unix)]
    fn owner(&self) -> u32;
    #[cfg(unix)]
    fn bits(&self) -> u32;
    #[cfg(unix)]
    fn device(&self) -> u64;
    #[cfg(unix)]
    fn inode(&self) -> u64;
}

impl Facts for fs::Metadata {
    fn link(&self) -> bool {
        self.file_type().is_symlink()
    }
    fn regular(&self) -> bool {
        self.is_file()
    }
    fn size(&self) -> u64 {
        self.len()
    }
    #[cfg(unix)]
    fn owner(&self) -> u32 {
        use std::os::unix::fs::MetadataExt;
        self.uid()
    }
    #[cfg(unix)]
    fn bits(&self) -> u32 {
        use std::os::unix::fs::MetadataExt;
        self.mode()
    }
    #[cfg(unix)]
    fn device(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        self.dev()
    }
    #[cfg(unix)]
    fn inode(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        self.ino()
    }
}

// Share the conversion with a cfg-independent test at both mode_t widths: Darwin uses u16,
// Linux uses u32. Keep the cast even where it is an identity conversion.
#[cfg(any(unix, test))]
macro_rules! mode_bits {
    ($mode:expr) => {{
        #[allow(clippy::unnecessary_cast)]
        {
            $mode as u32
        }
    }};
}

/// An `fstatat` result: the leaf's facts before it is opened, relative to the pinned parent.
#[cfg(unix)]
struct RawStat(libc::stat);

#[cfg(unix)]
impl Facts for RawStat {
    fn link(&self) -> bool {
        self.0.st_mode & libc::S_IFMT == libc::S_IFLNK
    }
    fn regular(&self) -> bool {
        self.0.st_mode & libc::S_IFMT == libc::S_IFREG
    }
    fn size(&self) -> u64 {
        u64::try_from(self.0.st_size).unwrap_or(0)
    }
    fn owner(&self) -> u32 {
        self.0.st_uid
    }
    fn bits(&self) -> u32 {
        mode_bits!(self.0.st_mode)
    }
    // dev_t and ino_t are platform-sized (already u64 on Linux, signed on macOS).
    #[allow(clippy::unnecessary_cast)]
    fn device(&self) -> u64 {
        self.0.st_dev as u64
    }
    #[allow(clippy::unnecessary_cast)]
    fn inode(&self) -> u64 {
        self.0.st_ino as u64
    }
}

/// What a link at the final name means.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Leaf {
    /// [`Read::Link`]: never followed, never opened.
    Refuse,
    /// [`Read::Absent`], for readers whose reference treats a linked file as missing.
    Absent,
}

/// Which folders on the way to the file must not be links.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ancestors<'a> {
    /// Not checked (the caller admitted the folders, or the reference follows them).
    Any,
    /// No folder strictly between this base and the file is a link ([`Read::Link`]); a path not
    /// below the base is [`Read::Unproved`].
    NoLinkBelow(&'a Path),
}

/// Who must own the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    /// Anyone.
    Any,
    /// This process's user ([`Read::Foreign`] otherwise; Windows has no such check).
    Mine,
}

/// Which permission bits the file may carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// Any.
    Any,
    /// No group or other bit (`mode & 0o077 == 0`, TS `isPrivate`); [`Read::BadMode`] otherwise.
    /// Windows reports no POSIX bits, so every file passes there.
    Private,
}

/// One reader's rules. Build it with [`ReadPolicy::capped`] and the setters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadPolicy<'a> {
    /// At most this many bytes; `cap + 1` are read, so growth during the read is caught.
    /// `u64::MAX` is no cap (header readers that apply their own size rules after the open).
    pub cap: u64,
    /// A link at the final name.
    pub leaf: Leaf,
    /// Links on the folders above it.
    pub ancestors: Ancestors<'a>,
    /// The owner rule.
    pub owner: Owner,
    /// The mode rule.
    pub mode: Mode,
}

impl<'a> ReadPolicy<'a> {
    /// A regular file of at most `cap` bytes, a final link refused, nothing else checked.
    pub const fn capped(cap: u64) -> Self {
        Self {
            cap,
            leaf: Leaf::Refuse,
            ancestors: Ancestors::Any,
            owner: Owner::Any,
            mode: Mode::Any,
        }
    }
    /// The same policy with another leaf rule.
    pub const fn leaf(mut self, leaf: Leaf) -> Self {
        self.leaf = leaf;
        self
    }
    /// The same policy with another ancestor rule.
    pub const fn ancestors(mut self, ancestors: Ancestors<'a>) -> Self {
        self.ancestors = ancestors;
        self
    }
    /// The same policy with another owner rule.
    pub const fn owner(mut self, owner: Owner) -> Self {
        self.owner = owner;
        self
    }
    /// The same policy with another mode rule.
    pub const fn mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }
}

/// What one read found. Only `Bytes` carries content.
pub enum Read {
    /// The whole file (at most the cap) and the metadata of the descriptor it came from.
    Bytes(Vec<u8>, Box<Meta>),
    /// No such name (`ENOENT`). A folder on the way that is not a folder is
    /// `Unreadable(NotADirectory)`: readers whose reference counts that as missing say so.
    Absent,
    /// The final name, or (with [`Ancestors::NoLinkBelow`]) a folder above it, is a link.
    Link,
    /// Not this user's file ([`Owner::Mine`]).
    Foreign,
    /// A group or other permission bit ([`Mode::Private`]).
    BadMode,
    /// A folder, FIFO, socket or device, by its `lstat` or by the opened descriptor.
    NotRegular,
    /// Over the cap, by its metadata or by the bytes read.
    TooLarge,
    /// An `lstat`, open, `fstat` or read error (never `ENOENT` at the first `lstat`).
    Unreadable(io::ErrorKind),
    /// A step of the pinned ancestor chain failed before the leaf was opened — the base or a
    /// folder below it, or the leaf's own `lstat` through its pinned parent. A denied chain
    /// cannot prove the reference's named read would fail on platforms without a search-capable
    /// descriptor, so callers that must match the reference decline rather than answer.
    ChainUnreadable(io::ErrorKind),
    /// The descriptor is not the file the path named when it was checked (replaced, removed or
    /// linked in between), or the path is not below the policy's base.
    Unproved,
}

impl fmt::Debug for Read {
    // Never prints file contents: private files hold credentials.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Read::Bytes(bytes, _) => write!(f, "Bytes({} bytes)", bytes.len()),
            Read::Absent => f.write_str("Absent"),
            Read::Link => f.write_str("Link"),
            Read::Foreign => f.write_str("Foreign"),
            Read::BadMode => f.write_str("BadMode"),
            Read::NotRegular => f.write_str("NotRegular"),
            Read::TooLarge => f.write_str("TooLarge"),
            Read::Unreadable(kind) => write!(f, "Unreadable({kind:?})"),
            Read::ChainUnreadable(kind) => write!(f, "ChainUnreadable({kind:?})"),
            Read::Unproved => f.write_str("Unproved"),
        }
    }
}

/// Read one file under `policy`.
pub fn read(path: &Path, policy: &ReadPolicy) -> Read {
    read_with(path, policy, &mut |_| {})
}

/// Open one file under `policy` for a reader that needs only part of it (an archive header, a
/// byte range). The cap applies to the metadata; the caller bounds what it reads.
pub fn open(path: &Path, policy: &ReadPolicy) -> Result<(File, Meta), Read> {
    open_with(path, policy, &mut |_| {})
}

/// The path the kernel holds for an open descriptor (`/proc/self/fd/N`), for readers whose
/// reference confines the resolved path; `None` where the platform cannot say.
pub fn opened_path(file: &File) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::fd::AsRawFd;
        fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd())).ok()
    }
    #[cfg(not(unix))]
    {
        let _ = file;
        None
    }
}

/// Where the test seam runs: after the ancestor phase, before the leaf's `lstat` (`Walk`), after
/// every check on the path, before the open (`Open`), and after every check on the descriptor,
/// before the bytes are read (`Read`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    Walk,
    Open,
    Read,
}

fn read_with(path: &Path, policy: &ReadPolicy, between: &mut dyn FnMut(Stage)) -> Read {
    let (file, meta) = match open_with(path, policy, between) {
        Ok(opened) => opened,
        Err(outcome) => return outcome,
    };
    between(Stage::Read);
    let mut bytes = Vec::new();
    if let Err(error) = (&file)
        .take(policy.cap.saturating_add(1))
        .read_to_end(&mut bytes)
    {
        return Read::Unreadable(error.kind());
    }
    if bytes.len() as u64 > policy.cap {
        return Read::TooLarge;
    }
    Read::Bytes(bytes, Box::new(meta))
}

fn open_with(
    path: &Path,
    policy: &ReadPolicy,
    between: &mut dyn FnMut(Stage),
) -> Result<(File, Meta), Read> {
    if let Ancestors::NoLinkBelow(base) = policy.ancestors {
        #[cfg(unix)]
        return open_below(base, path, policy, between);
        #[cfg(not(unix))]
        match path::walk(base, path, false) {
            Walk::Clear => {}
            Walk::Link => return Err(Read::Link),
            Walk::Outside => return Err(Read::Unproved),
            Walk::Failed(kind) => return Err(Read::Unreadable(kind)),
        }
    }
    between(Stage::Walk);
    open_named(path, policy, between)
}

/// The leaf by its full path: one `lstat`, the path-only admission, then the descriptor checks.
fn open_named(
    path: &Path,
    policy: &ReadPolicy,
    between: &mut dyn FnMut(Stage),
) -> Result<(File, Meta), Read> {
    let before = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Err(Read::Absent),
        Err(error) => return Err(Read::Unreadable(error.kind())),
    };
    if before.link() {
        return Err(match policy.leaf {
            Leaf::Refuse => Read::Link,
            Leaf::Absent => Read::Absent,
        });
    }
    // Decide what the path alone can: an unusual, oversized, foreign or loose file is never opened.
    admit(&before, policy)?;
    between(Stage::Open);
    let file = open_descriptor(path).map_err(|error| {
        if replaced(&error) {
            Read::Unproved
        } else {
            Read::Unreadable(error.kind())
        }
    })?;
    opened_checks(file, &before, policy)
}

/// The leaf relative to a pinned chain of the folders below `base` (Wonder L12). Each folder is
/// opened `O_NOFOLLOW | O_DIRECTORY` as it is checked, so a later swap of any of them cannot
/// redirect the leaf's `lstat` or open. `base` itself may be a link: the caller admitted it (the
/// walk never checked it). Search-capable descriptors preserve access through search-only
/// folders; other platforms retain the read-only open. A chain step's failure (the leaf's
/// `lstat` included) is `ChainUnreadable`, so callers can decline a denied chain while a
/// denied leaf itself stays `Unreadable` (qwen lane 2 recheck L1).
#[cfg(unix)]
fn open_below(
    base: &Path,
    path: &Path,
    policy: &ReadPolicy,
    between: &mut dyn FnMut(Stage),
) -> Result<(File, Meta), Read> {
    let Ok(rest) = path.strip_prefix(base) else {
        return Err(Read::Unproved);
    };
    let parts: Vec<_> = rest.components().collect();
    let mut dir = open_at(None, base.as_os_str(), directory_flags()).map_err(chain_error)?;
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        // Check the name, then pin it: a link is refused before the open, and a swap between
        // the two is caught by the open's own `O_NOFOLLOW | O_DIRECTORY`.
        let stat = stat_at(&dir, part.as_os_str()).map_err(chain_error)?;
        if stat.link() {
            return Err(Read::Link);
        }
        dir = open_at(
            Some(&dir),
            part.as_os_str(),
            directory_flags() | libc::O_NOFOLLOW,
        )
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ELOOP) => Read::Link,
            // O_DIRECTORY reports a link as ENOTDIR: say which the name became.
            Some(libc::ENOTDIR) => match stat_at(&dir, part.as_os_str()) {
                Ok(now) if now.link() => Read::Link,
                _ => Read::Unreadable(io::ErrorKind::NotADirectory),
            },
            _ => chain_error(error),
        })?;
    }
    between(Stage::Walk);
    let leaf = parts
        .last()
        .map_or(std::ffi::OsStr::new("."), |part| part.as_os_str());
    // The leaf's stat goes through the pinned parent: its denial is the chain's, not the
    // leaf's — the file itself was never opened (qwen lane 2 recheck L1).
    let before = stat_at(&dir, leaf).map_err(chain_error)?;
    if before.link() {
        return Err(match policy.leaf {
            Leaf::Refuse => Read::Link,
            Leaf::Absent => Read::Absent,
        });
    }
    // Decide what the pinned name alone can: an unusual, oversized, foreign or loose file is
    // never opened.
    admit(&before, policy)?;
    between(Stage::Open);
    let file = open_at(
        Some(&dir),
        leaf,
        libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY,
    )
    .map_err(|error| {
        if replaced(&error) {
            Read::Unproved
        } else {
            Read::Unreadable(error.kind())
        }
    })?;
    opened_checks(file, &before, policy)
}

/// Pin a directory without requiring listing permission where the platform supports it.
#[cfg(unix)]
fn directory_flags() -> libc::c_int {
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let access = libc::O_PATH;
    #[cfg(any(
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos"
    ))]
    let access = libc::O_SEARCH;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos"
    )))]
    let access = libc::O_RDONLY;
    access | libc::O_DIRECTORY
}

/// Whether this platform pins ancestors with a search-capable descriptor (`O_PATH` or
/// `O_SEARCH`), so a denied chain is told apart from a denied leaf and search-only folders
/// still read. Callers that must match a named-read reference decline a `ChainUnreadable`
/// everywhere, and a leaf `PermissionDenied` only where this is false.
pub const fn search_capable() -> bool {
    cfg!(any(
        target_os = "linux",
        target_os = "android",
        target_vendor = "apple",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "solaris",
        target_os = "illumos"
    ))
}

/// The descriptor's own admission: type, identity against the earlier stat, then owner/mode/size.
fn opened_checks<B: Facts>(
    file: File,
    before: &B,
    policy: &ReadPolicy,
) -> Result<(File, Meta), Read> {
    let opened = file
        .metadata()
        .map_err(|error| Read::Unreadable(error.kind()))?;
    if !opened.is_file() {
        return Err(Read::NotRegular);
    }
    if !same_file(before, &opened) {
        return Err(Read::Unproved);
    }
    // The descriptor is authoritative: the same inode may have grown or changed hands.
    admit(&opened, policy)?;
    Ok((file, opened))
}

/// A chain step's error: the leaf was never opened. Missing is absent and a non-folder the
/// named unreadable; any other failure names the chain, so a caller can tell a denied
/// ancestor from a denied leaf (qwen lane 2 recheck L1).
#[cfg(unix)]
fn chain_error(error: io::Error) -> Read {
    match error.kind() {
        io::ErrorKind::NotFound => Read::Absent,
        io::ErrorKind::NotADirectory => Read::Unreadable(io::ErrorKind::NotADirectory),
        kind => Read::ChainUnreadable(kind),
    }
}

/// The leaf's `lstat` relative to the pinned parent (`AT_SYMLINK_NOFOLLOW`).
#[cfg(unix)]
#[allow(unsafe_code)]
fn stat_at(dir: &File, name: &std::ffi::OsStr) -> io::Result<RawStat> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interior NUL in path"))?;
    let mut stat: libc::stat = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            name.as_ptr(),
            &mut stat,
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if rc == 0 {
        Ok(RawStat(stat))
    } else {
        Err(io::Error::last_os_error())
    }
}

/// One open of the chain: relative to the pinned parent when there is one, `O_CLOEXEC` always.
#[cfg(unix)]
#[allow(unsafe_code)]
fn open_at(dir: Option<&File>, name: &std::ffi::OsStr, flags: libc::c_int) -> io::Result<File> {
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::OsStrExt;
    let name = std::ffi::CString::new(name.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "interior NUL in path"))?;
    let fd = unsafe {
        match dir {
            Some(dir) => libc::openat(dir.as_raw_fd(), name.as_ptr(), flags | libc::O_CLOEXEC, 0),
            None => libc::open(name.as_ptr(), flags | libc::O_CLOEXEC, 0),
        }
    };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { File::from_raw_fd(fd) })
    }
}

fn admit<S: Facts>(meta: &S, policy: &ReadPolicy) -> Result<(), Read> {
    if !meta.regular() {
        return Err(Read::NotRegular);
    }
    if meta.size() > policy.cap {
        return Err(Read::TooLarge);
    }
    if policy.owner == Owner::Mine && foreign_owner(meta) {
        return Err(Read::Foreign);
    }
    if policy.mode == Mode::Private && loose(meta) {
        return Err(Read::BadMode);
    }
    Ok(())
}

/// Not this process's user; Windows has no such check.
fn foreign_owner<S: Facts>(meta: &S) -> bool {
    #[cfg(unix)]
    {
        meta.owner() != path::uid()
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

fn loose<S: Facts>(meta: &S) -> bool {
    #[cfg(unix)]
    {
        meta.bits() & 0o077 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        false
    }
}

/// An open error that means the name changed after its `lstat`: a link (`ELOOP` under
/// `O_NOFOLLOW`) or nothing (`ENOENT`) where a regular file was.
fn replaced(error: &io::Error) -> bool {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::ELOOP) {
        return true;
    }
    error.kind() == io::ErrorKind::NotFound
}

fn open_descriptor(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_NOCTTY | libc::O_CLOEXEC);
    }
    options.open(path)
}

fn same_file<B: Facts, O: Facts>(before: &B, opened: &O) -> bool {
    #[cfg(unix)]
    {
        (before.device(), before.inode()) == (opened.device(), opened.inode())
    }
    #[cfg(not(unix))]
    {
        let _ = (before, opened);
        true
    }
}

/// Why a bounded listing stopped early.
#[derive(Debug)]
pub enum Listed {
    /// More entries than the cap.
    TooMany,
    /// One entry could not be read.
    Entry(io::Error),
}

/// A folder listing that yields at most `cap` entries, then [`Listed::TooMany`] once if there
/// are more. Entry errors are passed through in place, so each caller keeps its reference's
/// order of checks (a count bound on the raw listing, before any filtering).
pub struct Entries {
    inner: fs::ReadDir,
    left: usize,
    done: bool,
}

/// List `dir` with a count cap (the caller has already admitted the folder itself).
pub fn entries(dir: &Path, cap: usize) -> io::Result<Entries> {
    Ok(Entries {
        inner: fs::read_dir(dir)?,
        left: cap,
        done: false,
    })
}

impl Iterator for Entries {
    type Item = Result<fs::DirEntry, Listed>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.done {
            return None;
        }
        let Some(item) = self.inner.next() else {
            self.done = true;
            return None;
        };
        if self.left == 0 {
            self.done = true;
            return Some(Err(Listed::TooMany));
        }
        self.left -= 1;
        Some(item.map_err(Listed::Entry))
    }
}

#[cfg(test)]
mod tests {
    //! One test per rule. Each was also run against a copy of this module with that rule
    //! removed, and failed there (README-L2-files.md, "Fails before").
    use super::*;

    fn plain(cap: u64) -> ReadPolicy<'static> {
        ReadPolicy::capped(cap)
    }

    #[test]
    fn stat_mode_bits_widen_both_narrow_and_wide_platform_types() {
        // Removing the shared cast fails to compile this u16 case even on Linux.
        let narrow: u32 = mode_bits!(0o100600_u16);
        let wide: u32 = mode_bits!(0o100600_u32);
        assert_eq!(narrow, wide);
        assert_eq!(narrow, 0o100600);
    }

    #[test]
    fn an_ordinary_file_is_read_whole() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"{}")?;
        assert!(matches!(read(&file, &plain(2)), Read::Bytes(ref b, _) if b == b"{}"));
        assert!(matches!(
            read(&dir.path().join("none"), &plain(9)),
            Read::Absent
        ));
        // A file below a file: not "absent", so each reader states its reference's meaning.
        assert!(matches!(
            read(&file.join("below"), &plain(9)),
            Read::Unreadable(io::ErrorKind::NotADirectory)
        ));
        Ok(())
    }

    #[test]
    fn oversize_by_metadata_and_by_bytes_read() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"12345678")?;
        assert!(matches!(read(&file, &plain(8)), Read::Bytes(..)));
        assert!(matches!(read(&file, &plain(7)), Read::TooLarge));
        // Growth of the same inode after the path check: the descriptor's size decides; after the
        // descriptor check: the cap + 1 read catches it.
        for stage in [Stage::Open, Stage::Read] {
            fs::write(&file, b"12345678")?;
            let mut grow = |now| {
                if now == stage {
                    let _ = fs::OpenOptions::new()
                        .append(true)
                        .open(&file)
                        .and_then(|mut f| io::Write::write_all(&mut f, b"9"));
                }
            };
            assert!(
                matches!(read_with(&file, &plain(8), &mut grow), Read::TooLarge),
                "{stage:?}"
            );
        }
        // `u64::MAX` reads without a cap.
        assert!(matches!(read(&file, &plain(u64::MAX)), Read::Bytes(ref b, _) if b.len() == 9));
        // Oversized by its metadata: never opened (this one could not be).
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&file, fs::Permissions::from_mode(0o000))?;
            let unopenable = fs::File::open(&file).is_err();
            assert!(matches!(read(&file, &plain(8)), Read::TooLarge));
            if unopenable {
                assert!(matches!(
                    read(&file, &plain(9)),
                    Read::Unreadable(io::ErrorKind::PermissionDenied)
                ));
            }
            fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_final_link_is_never_followed() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"{}")?;
        let link = dir.path().join("link.json");
        symlink(&file, &link)?;
        assert!(matches!(read(&link, &plain(1 << 20)), Read::Link));
        assert!(matches!(
            read(&link, &plain(1 << 20).leaf(Leaf::Absent)),
            Read::Absent
        ));
        let dangling = dir.path().join("dangling");
        symlink(dir.path().join("none"), &dangling)?;
        assert!(matches!(read(&dangling, &plain(9)), Read::Link));
        assert!(matches!(open(&link, &plain(9)), Err(Read::Link)));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_linked_folder_below_the_base_is_refused() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir()?;
        let sub = dir.path().join("sub");
        fs::create_dir(&sub)?;
        fs::write(sub.join("x.json"), b"{}")?;
        symlink(&sub, dir.path().join("alias"))?;
        let below = |base| plain(9).ancestors(Ancestors::NoLinkBelow(base));
        let through = dir.path().join("alias/x.json");
        assert!(matches!(read(&through, &plain(9)), Read::Bytes(..)));
        assert!(matches!(read(&through, &below(dir.path())), Read::Link));
        assert!(matches!(
            read(&sub.join("x.json"), &below(dir.path())),
            Read::Bytes(..)
        ));
        assert!(matches!(
            read(&dir.path().join("gone/x.json"), &below(dir.path())),
            Read::Absent
        ));
        assert!(matches!(
            read(&sub.join("x.json"), &below(Path::new("/elsewhere"))),
            Read::Unproved
        ));
        Ok(())
    }

    /// qwen lane 2 recheck L1: a denied leaf is the reference's read_failed(), but a denied
    /// chain cannot prove the named read would fail — the two must be told apart.
    #[cfg(unix)]
    #[test]
    fn a_denied_leaf_is_unreadable_but_a_denied_chain_is_named() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        if crate::path::uid() == 0 {
            return Ok(()); // Root bypasses the fixture's permission control.
        }
        let home = tempfile::tempdir()?;
        let userdata = home.path().join(".t3/userdata");
        fs::create_dir_all(&userdata)?;
        let file = userdata.join("settings.json");
        fs::write(&file, b"{}")?;
        let policy = ReadPolicy::capped(65536).ancestors(Ancestors::NoLinkBelow(home.path()));
        // The leaf itself denied: the chain reads, the file does not open.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o000))?;
        let leaf = read(&file, &policy);
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        assert!(
            matches!(leaf, Read::Unreadable(io::ErrorKind::PermissionDenied)),
            "{leaf:?}"
        );
        // An ancestor denied: the leaf was never opened, and the chain's denial is named.
        fs::set_permissions(&userdata, fs::Permissions::from_mode(0o000))?;
        let chain = read(&file, &policy);
        fs::set_permissions(&userdata, fs::Permissions::from_mode(0o700))?;
        assert!(
            matches!(
                chain,
                Read::ChainUnreadable(io::ErrorKind::PermissionDenied)
            ),
            "{chain:?}"
        );
        // Search-only ancestors still read where the platform pins them search-capable.
        for mode in [0o300, 0o100] {
            fs::set_permissions(&userdata, fs::Permissions::from_mode(mode))?;
            let outcome = read(&file, &policy);
            fs::set_permissions(&userdata, fs::Permissions::from_mode(0o700))?;
            if search_capable() {
                assert!(
                    matches!(outcome, Read::Bytes(..)),
                    "mode {mode:o}: {outcome:?}"
                );
            }
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_folder_or_device_is_never_opened() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let fifo = dir.path().join("fifo");
        let made = std::process::Command::new("mkfifo").arg(&fifo).status()?;
        assert!(made.success(), "mkfifo");
        // A blocking open of a FIFO with no writer would hang this test.
        assert!(matches!(read(&fifo, &plain(1 << 20)), Read::NotRegular));
        assert!(matches!(
            read(dir.path(), &plain(1 << 20)),
            Read::NotRegular
        ));
        assert!(matches!(
            read(Path::new("/dev/null"), &plain(9)),
            Read::NotRegular
        ));
        // A FIFO swapped in after the check: the nonblocking open returns at once and the
        // descriptor's own type refuses it.
        let file = dir.path().join("a.json");
        fs::write(&file, b"{}")?;
        let mut swap = |stage| {
            if stage == Stage::Open {
                let _ = fs::remove_file(&file);
                let _ = fs::rename(&fifo, &file);
            }
        };
        assert!(matches!(
            read_with(&file, &plain(9), &mut swap),
            Read::NotRegular
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_foreign_owner_is_refused() -> io::Result<()> {
        use std::os::unix::fs::MetadataExt;
        // A system file owned by root: foreign to every test user except root.
        let system = Path::new("/etc/passwd");
        let meta = fs::symlink_metadata(system)?;
        if meta.uid() == path::uid() {
            eprintln!("skipped: the test runs as the owner of /etc/passwd");
            return Ok(());
        }
        assert!(matches!(
            read(system, &plain(1 << 20).owner(Owner::Mine)),
            Read::Foreign
        ));
        assert!(matches!(read(system, &plain(1 << 20)), Read::Bytes(..)));
        let dir = tempfile::tempdir()?;
        let own = dir.path().join("own.json");
        fs::write(&own, b"{}")?;
        assert!(matches!(
            read(&own, &plain(9).owner(Owner::Mine)),
            Read::Bytes(..)
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn group_or_other_bits_are_refused_when_private() -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"{}")?;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        assert!(matches!(
            read(&file, &plain(9).mode(Mode::Private)),
            Read::Bytes(..)
        ));
        for loose in [0o640, 0o604, 0o660] {
            fs::set_permissions(&file, fs::Permissions::from_mode(loose))?;
            assert!(matches!(
                read(&file, &plain(9).mode(Mode::Private)),
                Read::BadMode
            ));
        }
        assert!(matches!(read(&file, &plain(9)), Read::Bytes(..)));
        // Loosened after the check: the descriptor's own mode decides.
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
        let mut loosen = |stage| {
            if stage == Stage::Open {
                let _ = fs::set_permissions(&file, fs::Permissions::from_mode(0o644));
            }
        };
        assert!(matches!(
            read_with(&file, &plain(9).mode(Mode::Private), &mut loosen),
            Read::BadMode
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_file_replaced_between_check_and_open_is_unproved() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("a.json");
        fs::write(&file, b"checked")?;
        let other = dir.path().join("other.json");
        // Another ordinary file renamed over the checked one: a second inode.
        let mut replace = |stage| {
            if stage == Stage::Open {
                let _ = fs::write(&other, b"swapped");
                let _ = fs::rename(&other, &file);
            }
        };
        assert!(matches!(
            read_with(&file, &plain(64), &mut replace),
            Read::Unproved
        ));
        assert!(matches!(read(&file, &plain(64)), Read::Bytes(ref b, _) if b == b"swapped"));
        // A link swapped in: O_NOFOLLOW refuses it at the open.
        let target = dir.path().join("target.json");
        fs::write(&target, b"target")?;
        let mut relink = |stage| {
            if stage == Stage::Open {
                let _ = fs::remove_file(&file);
                let _ = symlink(&target, &file);
            }
        };
        assert!(matches!(
            read_with(&file, &plain(64), &mut relink),
            Read::Unproved
        ));
        // A link to the very inode that was checked: still a link, never followed.
        fs::remove_file(&file)?;
        fs::write(&file, b"same inode")?;
        let moved = dir.path().join("moved.json");
        let mut alias = |stage| {
            if stage == Stage::Open {
                let _ = fs::rename(&file, &moved);
                let _ = symlink(&moved, &file);
            }
        };
        assert!(matches!(
            read_with(&file, &plain(64), &mut alias),
            Read::Unproved
        ));
        // Removed in between.
        fs::remove_file(&file)?;
        fs::write(&file, b"again")?;
        let mut remove = |stage| {
            if stage == Stage::Open {
                let _ = fs::remove_file(&file);
            }
        };
        assert!(matches!(
            read_with(&file, &plain(64), &mut remove),
            Read::Unproved
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn an_ancestor_swapped_after_the_walk_cannot_redirect_the_read() -> io::Result<()> {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir()?;
        let base = dir.path();
        let sub = base.join("sub");
        fs::create_dir(&sub)?;
        let file = sub.join("x.json");
        fs::write(&file, b"original")?;
        let attacker = base.join("attacker");
        fs::create_dir(&attacker)?;
        fs::write(attacker.join("x.json"), b"attacker")?;
        let moved = base.join("sub.moved");
        // The finding's window: the folder becomes a link after the ancestor walk and before the
        // leaf's lstat, so path-only checks follow it consistently (Wonder L12).
        let mut swap = |stage| {
            if stage == Stage::Walk {
                let _ = fs::rename(&sub, &moved);
                let _ = symlink(&attacker, &sub);
            }
        };
        let policy = plain(64).ancestors(Ancestors::NoLinkBelow(base));
        let outcome = read_with(&file, &policy, &mut swap);
        assert!(
            matches!(&outcome, Read::Bytes(b, _) if b == b"original"),
            "the swapped ancestor must not redirect the read: {outcome:?}"
        );
        // A link already there at the walk is still refused.
        assert!(matches!(read(&file, &policy), Read::Link));
        Ok(())
    }

    #[test]
    fn listings_stop_at_their_count_cap() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        for i in 0..3 {
            fs::write(dir.path().join(format!("{i}")), b"")?;
        }
        let all: Vec<_> = entries(dir.path(), 3)?.collect();
        assert_eq!(all.len(), 3);
        assert!(all.iter().all(Result::is_ok));
        let capped: Vec<_> = entries(dir.path(), 2)?.collect();
        assert_eq!(capped.len(), 3);
        assert!(matches!(capped[2], Err(Listed::TooMany)));
        assert!(entries(dir.path(), 0)?.next().is_some_and(|e| e.is_err()));
        assert!(entries(&dir.path().join("none"), 1).is_err());
        Ok(())
    }

    #[test]
    fn debug_never_prints_contents() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let file = dir.path().join("secret.json");
        fs::write(&file, b"fixture-secret-value")?;
        let text = format!("{:?}", read(&file, &plain(64)));
        assert_eq!(text, "Bytes(20 bytes)");
        Ok(())
    }
}
