//! Central Logins step 3: quarantine, never delete.
//!
//! AAC's saved Codex copies (`<ccs>/codex-instances/<name>/auth.json`) and a computer's Claude
//! sign-in files (`~/.claude/.credentials.json`, `~/.claude-t3/<profile>/.credentials.json`) are
//! **moved** into `<ccs>/disabled-rollback/` (0700) under neutral names (`0001.moved`, ...), so no
//! reader or refresher that looks for `auth.json` or `.credentials.json` in a login folder can
//! reach them. `manifest.json` beside them records where each came from, its size and a short
//! digest; [`restore`] moves every one back (never over a newer file) and removes the step 3
//! switch, so one command puts the computer back as it was.
//!
//! The **switch** (`<ccs>/logins-step3.json`) is what the dashboard and the Claude renewal runner
//! read: the Codex accounts by name with the identity each saved copy proved before it was moved
//! (the dashboard then reads each account only through a T3 instance's own login with that
//! identity), and the one computer whose Claude sign-ins are still renewed. No secret is in either
//! file, and nothing here ever deletes a login: a purge is a separate, later action.
use crate::read::{self, Ancestors, Mode, Owner, Read, ReadPolicy};
use crate::registry;
use aac_core::{Cause, Failure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// The switch's registry row (`logins-step3.json`).
pub const SWITCH_ROW: &str = "logins-switch";
/// The manifest's registry row (`disabled-rollback/manifest.json`).
pub const MANIFEST_ROW: &str = "logins-rollback";
/// The quarantine folder under the AAC private folder.
pub const FOLDER: &str = "disabled-rollback";
const MAX_LOGIN_FILE: u64 = 1024 * 1024;
const MAX_MOVES: usize = 256;

use crate::step3_switch::valid_stamp;
pub use crate::step3_switch::{CodexPin, Switch, valid_name};

/// What kind of login a moved file was.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// AAC's saved Codex copy, `<ccs>/codex-instances/<name>/auth.json`.
    CodexSaved,
    /// A Claude sign-in on this computer, `~/.claude/.credentials.json` or
    /// `~/.claude-t3/<profile>/.credentials.json`.
    ClaudeSignin,
}

impl Kind {
    /// The manifest spelling.
    pub fn word(self) -> &'static str {
        match self {
            Kind::CodexSaved => "codex-saved",
            Kind::ClaudeSignin => "claude-signin",
        }
    }
}

/// One moved file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Moved {
    /// `0001` ...: the file is `disabled-rollback/<id>.moved`.
    pub id: String,
    /// What it was.
    pub kind: Kind,
    /// The profile it belonged to (`default` for `~/.claude`).
    pub profile: String,
    /// Where it was (absolute).
    pub from: String,
    /// Its size in bytes.
    pub size: u64,
    /// 16 hex digits of its SHA-256: tells a restore that the file is the one that was moved.
    pub digest: String,
}

/// The quarantine record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Manifest {
    /// Always 1.
    pub version: u32,
    /// When the quarantine ran (UTC, seconds).
    pub at: String,
    /// The one command that puts everything back on this computer.
    pub restore: String,
    /// Every moved file.
    pub moved: Vec<Moved>,
}

impl Manifest {
    fn parse(bytes: &[u8]) -> Option<Self> {
        let manifest: Self = serde_json::from_slice(bytes).ok()?;
        (manifest.version == 1
            && valid_stamp(&manifest.at)
            && manifest.moved.len() <= MAX_MOVES
            && manifest.restore.len() <= 4096
            && manifest.moved.iter().all(|m| {
                m.id.len() == 4
                    && m.id.bytes().all(|b| b.is_ascii_digit())
                    && valid_name(&m.profile)
                    && m.digest.len() == 16
                    && m.digest.bytes().all(|b| b.is_ascii_hexdigit())
            }))
        .then_some(manifest)
    }
}

/// The computer's home and AAC private folder: every place this module may move from.
#[derive(Clone, Debug)]
pub struct Places {
    /// The user's home folder.
    pub home: PathBuf,
    /// The AAC private folder (`~/.ccs` or `CCS_HOME`).
    pub ccs: PathBuf,
}

impl Places {
    /// The quarantine folder.
    pub fn folder(&self) -> PathBuf {
        self.ccs.join(FOLDER)
    }

    /// AAC's saved Codex copy of profile `name`.
    pub fn codex_copy(&self, name: &str) -> Option<PathBuf> {
        valid_name(name).then(|| {
            self.ccs
                .join("codex-instances")
                .join(name)
                .join("auth.json")
        })
    }

    /// The Claude sign-in file of renewal profile `profile` on this computer.
    pub fn claude_signin(&self, profile: &str) -> Option<PathBuf> {
        if !valid_name(profile) {
            return None;
        }
        Some(if profile == "default" {
            self.home.join(".claude").join(".credentials.json")
        } else {
            self.home
                .join(".claude-t3")
                .join(profile)
                .join(".credentials.json")
        })
    }

    fn place(&self, kind: Kind, profile: &str) -> Option<PathBuf> {
        match kind {
            Kind::CodexSaved => self.codex_copy(profile),
            Kind::ClaudeSignin => self.claude_signin(profile),
        }
    }

    /// The root a place's folders must not link below.
    fn root(&self, kind: Kind) -> &Path {
        match kind {
            Kind::CodexSaved => &self.ccs,
            Kind::ClaudeSignin => &self.home,
        }
    }
}

fn refused(detail: &'static str) -> Failure {
    Failure::new(Cause::Refused, detail)
}

fn damaged(detail: &'static str) -> Failure {
    Failure::new(Cause::Damaged, detail)
}

fn private_policy(root: &Path, cap: u64) -> ReadPolicy<'_> {
    ReadPolicy::capped(cap)
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(root))
}

fn single_link(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.nlink() == 1)
    }
    #[cfg(not(unix))]
    {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.is_file())
    }
}

fn digest(bytes: &[u8]) -> String {
    aac_core::sha256_hex(bytes)[..16].to_owned()
}

fn read_row(row_id: &str, ccs: &Path) -> Result<Option<Vec<u8>>, Failure> {
    let row = registry::row(row_id)?;
    let path = ccs.join(&row.path);
    match read::read(&path, &private_policy(ccs, row.max_bytes)) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, _) if single_link(&path) => Ok(Some(bytes)),
        _ => Err(refused("step 3 file is not private")),
    }
}

/// The switch on this computer: `None` when step 3 is not switched on. A file that is not
/// private or not valid is an error, never "off": callers show it as unavailable.
pub fn read_switch(ccs: &Path) -> Result<Option<Switch>, Failure> {
    match read_row(SWITCH_ROW, ccs)? {
        None => Ok(None),
        Some(bytes) => Switch::parse(&bytes)
            .map(Some)
            .ok_or_else(|| damaged("step 3 switch is not valid")),
    }
}

/// The quarantine record on this computer, when one is in place.
pub fn read_manifest(ccs: &Path) -> Result<Option<Manifest>, Failure> {
    match read_row(MANIFEST_ROW, ccs)? {
        None => Ok(None),
        Some(bytes) => Manifest::parse(&bytes)
            .map(Some)
            .ok_or_else(|| damaged("quarantine record is not valid")),
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Failure> {
    let mut bytes =
        serde_json::to_vec_pretty(value).map_err(|_| Failure::new(Cause::Bug, "step 3 encode"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn private_folder(path: &Path) -> Result<(), Failure> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::symlink_metadata(path)?;
        if !meta.is_dir() {
            return Err(refused("quarantine folder is not a folder"));
        }
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    {
        if !std::fs::symlink_metadata(path)?.is_dir() {
            return Err(refused("quarantine folder is not a folder"));
        }
    }
    Ok(())
}

fn sync_folder(path: &Path) {
    #[cfg(unix)]
    if let Ok(folder) = std::fs::File::open(path) {
        let _ = folder.sync_all();
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// One file to move: its kind and the profile it belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Planned {
    /// What it is.
    pub kind: Kind,
    /// Its profile (`default` for `~/.claude`).
    pub profile: String,
}

/// What a quarantine did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Quarantined {
    /// Moved into the quarantine folder.
    pub moved: Vec<Moved>,
    /// Asked for but not there: (kind, profile).
    pub absent: Vec<(Kind, String)>,
    /// Whether the switch was written.
    pub switched: bool,
}

/// Why a quarantine refused (nothing was moved).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Refused {
    /// A quarantine is already in place: restore it first.
    AlreadyInPlace,
    /// A name is not a valid profile name.
    BadName(String),
    /// This file is a link, has a second hard link, belongs to another user or is too large.
    Unsafe(Kind, String),
    /// A move failed. Inverse moves were attempted; the recovery record keeps any entry not
    /// proved restored.
    MoveFailed(Kind, String),
    /// The folder, record or switch could not be written.
    Failure(Failure),
}

impl From<Failure> for Refused {
    fn from(value: Failure) -> Self {
        Refused::Failure(value)
    }
}

fn moved_path(places: &Places, id: &str) -> PathBuf {
    places.folder().join(format!("{id}.moved"))
}

/// Undo the moves that succeeded. The recovery record is removed only when every inverse move
/// succeeded; otherwise it is rewritten to the entries still quarantined, so a failed recovery
/// never erases the only map back. Returns false when any entry could not be proved restored.
fn undo_moves(
    places: &Places,
    manifest_store: &crate::OwnedStore,
    manifest: &Manifest,
    done: &[(Moved, PathBuf)],
) -> bool {
    let mut still = Vec::new();
    for (m, from) in done.iter().rev() {
        if std::fs::rename(moved_path(places, &m.id), from).is_err() {
            still.push(m.clone());
        }
    }
    if still.is_empty() {
        return manifest_store.remove().is_ok();
    }
    let left = Manifest {
        moved: still,
        ..manifest.clone()
    };
    let _ = encode(&left).and_then(|bytes| manifest_store.write(&bytes));
    false
}

/// Move every planned file that exists into the quarantine folder, record it, then write the
/// switch (when given). An unsafe file leaves every file where it was and writes no record and no
/// switch. A failed move reverses every move it can; any entry whose inverse move fails stays
/// quarantined with its recovery record, never silently unrecorded.
pub fn quarantine(
    places: &Places,
    planned: &[Planned],
    switch: Option<&Switch>,
    at: &str,
    restore_command: &str,
) -> Result<Quarantined, Refused> {
    if !valid_stamp(at) || switch.is_some_and(|s| !s.valid()) {
        return Err(Refused::Failure(refused("invalid quarantine request")));
    }
    let manifest_store = registry::claim_as_agent(MANIFEST_ROW, &places.ccs, None)?;
    private_folder(&places.folder())?;
    if read_manifest(&places.ccs)
        .map_err(Refused::Failure)?
        .is_some()
    {
        return Err(Refused::AlreadyInPlace);
    }
    // Preflight: each file is absent, or a single-link regular file of this user.
    let mut out = Quarantined::default();
    let mut moves: Vec<(Moved, PathBuf)> = Vec::new();
    for item in planned {
        let Some(from) = places.place(item.kind, &item.profile) else {
            return Err(Refused::BadName(item.profile.clone()));
        };
        if moves.iter().any(|(_, seen)| seen == &from) {
            continue;
        }
        let policy = ReadPolicy::capped(MAX_LOGIN_FILE)
            .owner(Owner::Mine)
            .ancestors(Ancestors::NoLinkBelow(places.root(item.kind)));
        match read::read(&from, &policy) {
            Read::Absent => out.absent.push((item.kind, item.profile.clone())),
            Read::Bytes(bytes, _) if single_link(&from) => {
                if moves.len() >= MAX_MOVES {
                    return Err(Refused::Failure(refused("too many files")));
                }
                let id = format!("{:04}", moves.len() + 1);
                moves.push((
                    Moved {
                        id,
                        kind: item.kind,
                        profile: item.profile.clone(),
                        from: from.to_string_lossy().into_owned(),
                        size: bytes.len() as u64,
                        digest: digest(&bytes),
                    },
                    from,
                ));
            }
            _ => return Err(Refused::Unsafe(item.kind, item.profile.clone())),
        }
    }
    if moves.is_empty() && switch.is_none() {
        return Ok(out);
    }
    let manifest = Manifest {
        version: 1,
        at: at.to_owned(),
        restore: restore_command.to_owned(),
        moved: moves.iter().map(|(m, _)| m.clone()).collect(),
    };
    manifest_store.write(&encode(&manifest)?)?;
    let undo = |done: &[(Moved, PathBuf)]| undo_moves(places, &manifest_store, &manifest, done);
    for (index, (m, from)) in moves.iter().enumerate() {
        let target = moved_path(places, &m.id);
        // Never over an existing file: a leftover from an earlier run stays and this run stops.
        if std::fs::symlink_metadata(&target).is_ok() || std::fs::rename(from, &target).is_err() {
            undo(&moves[..index]);
            return Err(Refused::MoveFailed(m.kind, m.profile.clone()));
        }
        if let Some(parent) = from.parent() {
            sync_folder(parent);
        }
    }
    sync_folder(&places.folder());
    if let Some(switch) = switch {
        let written = registry::claim_as_agent(SWITCH_ROW, &places.ccs, None)
            .and_then(|store| encode(switch).and_then(|bytes| store.write(&bytes)));
        if let Err(failure) = written {
            undo(&moves);
            return Err(Refused::Failure(failure));
        }
        out.switched = true;
    }
    out.moved = moves.into_iter().map(|(m, _)| m).collect();
    Ok(out)
}

/// Why one moved file stayed in the quarantine folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kept {
    /// A file is at the original place now (a newer sign-in); the old copy is not put over it.
    NewerThere,
    /// The quarantined file is missing.
    Missing,
    /// The quarantined file is not the one that was moved (size or digest differ), or not safe.
    Changed,
    /// The record names a place no login lives in; it is never written to.
    NotALoginPlace,
    /// The destination folder chain contains a link/reparse point or cannot be proved safe.
    UnsafeDestination,
    /// The move back failed.
    MoveFailed,
}

impl Kept {
    /// A plain reason.
    pub fn reason(self) -> &'static str {
        match self {
            Kept::NewerThere => "a newer file is at that place now; the old copy stays",
            Kept::Missing => "the quarantined copy is missing",
            Kept::Changed => "the quarantined copy is not the file that was moved",
            Kept::NotALoginPlace => "the record names a place that is not a login place",
            Kept::UnsafeDestination => {
                "the destination folder chain contains a link or is not safe"
            }
            Kept::MoveFailed => "the move back failed",
        }
    }
}

/// What a restore did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Restored {
    /// Put back (or already back).
    pub back: Vec<Moved>,
    /// Left in the quarantine folder, with why. The record keeps only these.
    pub kept: Vec<(Moved, Kept)>,
    /// Whether a switch was removed.
    pub switch_removed: bool,
}

fn reparse_point(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = meta;
        false
    }
}

/// Whether every existing folder in `parent` below `root` is a real folder, never a link or
/// reparse point. The first missing component ends the check: creation below it is done by the
/// platform-specific mover, never by following a link that was already there.
fn destination_parent_clear(root: &Path, parent: &Path) -> bool {
    let Ok(rest) = parent.strip_prefix(root) else {
        return false;
    };
    let mut current = root.to_path_buf();
    for part in rest.components() {
        current.push(part.as_os_str());
        match std::fs::symlink_metadata(&current) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || reparse_point(&meta) || !meta.is_dir() {
                    return false;
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return true,
            Err(_) => return false,
        }
    }
    true
}

/// Move `source` to `dest` without replacement. On Linux the destination parent chain is pinned
/// below `root` with `openat`/`renameat`, and missing folders are made relative to those pinned
/// directories; a link swapped into the chain cannot redirect the move. Other platforms use the
/// checked path after the same chain refusal (the pre-existing-link case this restores against).
#[cfg(target_os = "linux")]
fn move_back_below(root: &Path, parent: &Path, source: &Path, dest: &Path) -> std::io::Result<()> {
    use rustix::fs::{self, Mode, OFlags, RenameFlags};
    let flags = OFlags::RDONLY
        .union(OFlags::DIRECTORY)
        .union(OFlags::NOFOLLOW)
        .union(OFlags::CLOEXEC);
    // The trusted root itself is caller-admitted; every component below it is pinned no-follow.
    let mut dir = fs::open(
        root,
        OFlags::RDONLY
            .union(OFlags::DIRECTORY)
            .union(OFlags::CLOEXEC),
        Mode::empty(),
    )?;
    let rest = parent
        .strip_prefix(root)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "outside root"))?;
    for part in rest.components() {
        let name = part.as_os_str();
        dir = match fs::openat(&dir, name, flags, Mode::empty()) {
            Ok(next) => next,
            Err(rustix::io::Errno::NOENT) => {
                fs::mkdirat(&dir, name, Mode::from_raw_mode(0o700))?;
                fs::openat(&dir, name, flags, Mode::empty())?
            }
            Err(error) => return Err(error.into()),
        };
        // A race that made the newly opened entry a link or non-folder is caught by fstat.
        let stat = fs::fstat(&dir)?;
        if rustix::fs::FileType::from_raw_mode(stat.st_mode) != rustix::fs::FileType::Directory {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotADirectory,
                "destination is not a folder",
            ));
        }
    }
    let leaf = dest.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "destination has no leaf")
    })?;
    fs::renameat_with(rustix::fs::CWD, source, &dir, leaf, RenameFlags::NOREPLACE)?;
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn move_back_below(root: &Path, parent: &Path, source: &Path, dest: &Path) -> std::io::Result<()> {
    let _ = root;
    if !parent.is_dir() {
        std::fs::create_dir_all(parent)?;
        owner_only_folder(parent)?;
    }
    // Never replace a file that appeared after the caller's check where the OS offers no
    // no-replace rename through std; the checked path is still confined by the caller.
    std::fs::rename(source, dest)
}

/// Put every quarantined file back where it was (never over a file that is there now), then
/// remove the switch, so the computer reads and renews as before step 3. A record that cannot be
/// read changes nothing.
pub fn restore(places: &Places) -> Result<Restored, Failure> {
    let manifest_store = registry::claim_as_agent(MANIFEST_ROW, &places.ccs, None)?;
    let manifest = read_manifest(&places.ccs)?;
    let mut out = Restored::default();
    for m in manifest.iter().flat_map(|manifest| manifest.moved.iter()) {
        let from = PathBuf::from(&m.from);
        if places.place(m.kind, &m.profile).as_deref() != Some(from.as_path()) {
            out.kept.push((m.clone(), Kept::NotALoginPlace));
            continue;
        }
        let source = moved_path(places, &m.id);
        let there = std::fs::symlink_metadata(&from).is_ok();
        let policy = private_policy(&places.ccs, MAX_LOGIN_FILE).mode(Mode::Any);
        let held = match read::read(&source, &policy) {
            Read::Absent => None,
            Read::Bytes(bytes, _) if single_link(&source) => Some(bytes),
            _ => {
                out.kept.push((m.clone(), Kept::Changed));
                continue;
            }
        };
        let Some(bytes) = held else {
            // Already back from an interrupted restore, or lost.
            if there {
                out.back.push(m.clone());
            } else {
                out.kept.push((m.clone(), Kept::Missing));
            }
            continue;
        };
        if bytes.len() as u64 != m.size || digest(&bytes) != m.digest {
            out.kept.push((m.clone(), Kept::Changed));
            continue;
        }
        let Some(parent) = from.parent() else {
            out.kept.push((m.clone(), Kept::UnsafeDestination));
            continue;
        };
        if !destination_parent_clear(places.root(m.kind), parent) {
            out.kept.push((m.clone(), Kept::UnsafeDestination));
            continue;
        }
        if there {
            out.kept.push((m.clone(), Kept::NewerThere));
            continue;
        }
        if move_back_below(places.root(m.kind), parent, &source, &from).is_err() {
            out.kept.push((m.clone(), Kept::MoveFailed));
            continue;
        }
        if let Some(parent) = from.parent() {
            sync_folder(parent);
        }
        out.back.push(m.clone());
    }
    sync_folder(&places.folder());
    // The switch goes off whatever it holds (even damaged): restore means "as before step 3".
    if std::fs::symlink_metadata(places.ccs.join(&registry::row(SWITCH_ROW)?.path)).is_ok() {
        registry::claim_as_agent(SWITCH_ROW, &places.ccs, None)?.remove()?;
        out.switch_removed = true;
    }
    match &manifest {
        Some(manifest) if !out.kept.is_empty() => {
            let left = Manifest {
                moved: out.kept.iter().map(|(m, _)| m.clone()).collect(),
                ..manifest.clone()
            };
            manifest_store.write(&encode(&left)?)?;
        }
        _ => {
            manifest_store.remove()?;
            drop(manifest_store);
            let folder = places.folder();
            let _ = std::fs::remove_file(folder.join("manifest.json.lock"));
            let _ = std::fs::remove_dir(&folder);
            let _ = std::fs::remove_file(places.ccs.join("logins-step3.json.lock"));
        }
    }
    Ok(out)
}

/// A folder restore had to make again: owner-only on Unix (Windows folders inherit their ACL).
#[cfg(not(target_os = "linux"))]
fn owner_only_folder(folder: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(not(unix))]
    let _ = folder;
    Ok(())
}

/// Select the AAC private root with the dashboard's rules: an explicit root (the agent's
/// `--data-root`, like the dashboard CLI's `config_dir`) over `CCS_DIR` over legacy `CCS_HOME`
/// (`.ccs` appended) over the home's `.ccs`; empty values are skipped and relative selections
/// resolve lexically against the process current directory, following no symlink.
pub fn selected_ccs_root(
    home: &Path,
    environment: &BTreeMap<String, String>,
    explicit: Option<&str>,
) -> Result<PathBuf, Failure> {
    let direct = explicit
        .filter(|value| !value.is_empty())
        .or_else(|| {
            environment
                .get("CCS_DIR")
                .filter(|value| !value.is_empty())
                .map(String::as_str)
        })
        .map(PathBuf::from);
    let selected = direct.unwrap_or_else(|| {
        environment
            .get("CCS_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| home.to_path_buf())
            .join(".ccs")
    });
    let base = if selected.is_absolute() {
        PathBuf::new()
    } else {
        std::env::current_dir()?
    };
    crate::path::node::resolve(&base, &selected)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const AT: &str = "2026-11-27T18:00:00Z";
    const IDENTITY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    fn places(dir: &Path) -> Places {
        let home = dir.join("home");
        let ccs = home.join(".ccs");
        std::fs::create_dir_all(&ccs).unwrap();
        Places { home, ccs }
    }

    fn private_file(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
    }

    fn switch() -> Switch {
        Switch {
            version: 1,
            since: AT.into(),
            codex: Some(BTreeMap::from([(
                "work".to_owned(),
                CodexPin {
                    identity: IDENTITY.into(),
                    email: "work@example.test".into(),
                },
            )])),
            claude_renewal_host: Some("ubuntu".into()),
        }
    }

    fn plan() -> Vec<Planned> {
        vec![
            Planned {
                kind: Kind::CodexSaved,
                profile: "work".into(),
            },
            Planned {
                kind: Kind::ClaudeSignin,
                profile: "default".into(),
            },
            Planned {
                kind: Kind::ClaudeSignin,
                profile: "party".into(),
            },
            Planned {
                kind: Kind::ClaudeSignin,
                profile: "gone".into(),
            },
        ]
    }

    /// Every file the record lists left its login place for the 0700 folder under a neutral
    /// name, the switch is on, and one restore puts each file back byte for byte and turns the
    /// switch off.
    #[test]
    fn quarantine_moves_never_deletes_and_one_restore_puts_everything_back() {
        let dir = tempfile::tempdir().unwrap();
        let p = places(dir.path());
        let codex = p.codex_copy("work").unwrap();
        let claude = p.claude_signin("default").unwrap();
        let t3 = p.claude_signin("party").unwrap();
        private_file(&codex, b"{\"tokens\":\"TESTONLY-codex\"}");
        private_file(&claude, b"{\"claudeAiOauth\":\"TESTONLY-default\"}");
        private_file(&t3, b"{\"claudeAiOauth\":\"TESTONLY-party\"}");

        let done = quarantine(
            &p,
            &plan(),
            Some(&switch()),
            AT,
            "aac-agent --logins-restore",
        )
        .unwrap();
        assert_eq!(done.moved.len(), 3);
        assert_eq!(done.absent, vec![(Kind::ClaudeSignin, "gone".to_owned())]);
        assert!(done.switched);
        for path in [&codex, &claude, &t3] {
            assert!(!path.exists(), "{} still in place", path.display());
        }
        let folder = p.folder();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&folder).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o700);
        }
        let mut names: Vec<_> = std::fs::read_dir(&folder)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| !n.ends_with(".lock"))
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["0001.moved", "0002.moved", "0003.moved", "manifest.json"]
        );
        let record = std::fs::read_to_string(folder.join("manifest.json")).unwrap();
        assert!(!record.contains("TESTONLY"), "no secret in the record");
        assert_eq!(read_switch(&p.ccs).unwrap(), Some(switch()));
        assert_eq!(read_manifest(&p.ccs).unwrap().unwrap().moved.len(), 3);
        // A second quarantine refuses while this one is in place.
        assert_eq!(
            quarantine(&p, &plan(), None, AT, "x"),
            Err(Refused::AlreadyInPlace)
        );

        let back = restore(&p).unwrap();
        assert_eq!(back.back.len(), 3);
        assert!(back.kept.is_empty());
        assert!(back.switch_removed);
        assert_eq!(
            std::fs::read(&codex).unwrap(),
            b"{\"tokens\":\"TESTONLY-codex\"}"
        );
        assert_eq!(
            std::fs::read(&claude).unwrap(),
            b"{\"claudeAiOauth\":\"TESTONLY-default\"}"
        );
        assert_eq!(
            std::fs::read(&t3).unwrap(),
            b"{\"claudeAiOauth\":\"TESTONLY-party\"}"
        );
        assert_eq!(read_switch(&p.ccs).unwrap(), None);
        assert_eq!(read_manifest(&p.ccs).unwrap(), None);
        assert!(!folder.exists(), "the empty quarantine folder is gone");
        // Restoring again is a no-op.
        assert_eq!(restore(&p).unwrap(), Restored::default());
    }

    /// A newer sign-in made after the quarantine is never overwritten: the old copy stays in the
    /// folder and the record keeps only it.
    #[test]
    fn restore_never_writes_over_a_newer_file() {
        let dir = tempfile::tempdir().unwrap();
        let p = places(dir.path());
        let codex = p.codex_copy("work").unwrap();
        let claude = p.claude_signin("party").unwrap();
        private_file(&codex, b"old-codex");
        private_file(&claude, b"old-claude");
        quarantine(&p, &plan(), Some(&switch()), AT, "restore").unwrap();
        private_file(&claude, b"newer-claude");
        let back = restore(&p).unwrap();
        assert_eq!(back.back.len(), 1);
        assert_eq!(back.kept.len(), 1);
        assert_eq!(back.kept[0].1, Kept::NewerThere);
        assert_eq!(std::fs::read(&claude).unwrap(), b"newer-claude");
        assert_eq!(std::fs::read(&codex).unwrap(), b"old-codex");
        assert_eq!(read_manifest(&p.ccs).unwrap().unwrap().moved.len(), 1);
        assert_eq!(read_switch(&p.ccs).unwrap(), None, "the switch goes off");
    }

    /// A link, or a file with a second hard link, is refused and nothing at all is moved.
    #[cfg(unix)]
    #[test]
    fn an_unsafe_file_moves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let p = places(dir.path());
        let codex = p.codex_copy("work").unwrap();
        private_file(&codex, b"codex");
        let claude = p.claude_signin("party").unwrap();
        std::fs::create_dir_all(claude.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&codex, &claude).unwrap();
        assert_eq!(
            quarantine(&p, &plan(), Some(&switch()), AT, "restore"),
            Err(Refused::Unsafe(Kind::ClaudeSignin, "party".into()))
        );
        assert!(codex.exists());
        assert_eq!(read_manifest(&p.ccs).unwrap(), None);
        assert_eq!(read_switch(&p.ccs).unwrap(), None);
        std::fs::remove_file(&claude).unwrap();
        std::fs::hard_link(&codex, &claude).unwrap();
        assert_eq!(
            quarantine(&p, &plan(), None, AT, "restore"),
            Err(Refused::Unsafe(Kind::CodexSaved, "work".into()))
        );
        assert!(codex.exists() && claude.exists());
    }

    /// A tampered record that names another place is never written to.
    #[test]
    fn restore_writes_only_to_login_places() {
        let dir = tempfile::tempdir().unwrap();
        let p = places(dir.path());
        private_file(&p.codex_copy("work").unwrap(), b"codex");
        quarantine(&p, &plan()[..1], None, AT, "restore").unwrap();
        let record = p.folder().join("manifest.json");
        let elsewhere = dir.path().join("elsewhere");
        // The record holds paths as JSON strings (a Windows path's backslashes are escaped).
        let json_text = |path: &Path| {
            let quoted = serde_json::to_string(&path.to_string_lossy()).unwrap();
            quoted.trim_matches('"').to_owned()
        };
        let text = std::fs::read_to_string(&record).unwrap().replace(
            &json_text(&p.codex_copy("work").unwrap()),
            &json_text(&elsewhere),
        );
        private_file(&record, text.as_bytes());
        let back = restore(&p).unwrap();
        assert_eq!(back.kept[0].1, Kept::NotALoginPlace);
        assert!(!elsewhere.exists());
    }

    #[test]
    fn a_bad_name_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let p = places(dir.path());
        for bad in ["../x", ".hidden", "a/b", ""] {
            let plan = [Planned {
                kind: Kind::ClaudeSignin,
                profile: bad.into(),
            }];
            assert_eq!(
                quarantine(&p, &plan, None, AT, "restore"),
                Err(Refused::BadName(bad.into()))
            );
        }
    }

    #[test]
    fn a_damaged_switch_is_an_error_never_off() {
        let dir = tempfile::tempdir().unwrap();
        let p = places(dir.path());
        private_file(&p.ccs.join("logins-step3.json"), b"{\"version\":2}");
        assert!(read_switch(&p.ccs).is_err());
        let mut bad = switch();
        bad.codex
            .as_mut()
            .unwrap()
            .get_mut("work")
            .unwrap()
            .identity = "short".into();
        assert!(!bad.valid());
    }
    #[test]
    fn a_linked_destination_folder_is_never_used() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let places = Places {
            home: home.clone(),
            ccs: home.join(".ccs"),
        };
        let alpha = places.ccs.join("codex-instances/alpha");
        let beta = places.ccs.join("codex-instances/beta");
        std::fs::create_dir_all(&alpha).unwrap();
        std::fs::create_dir_all(&beta).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&places.home, &places.ccs] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        std::fs::write(alpha.join("auth.json"), b"alpha-login").unwrap();
        std::fs::write(beta.join("auth.json"), b"beta-login").unwrap();
        let planned = vec![
            Planned {
                kind: Kind::CodexSaved,
                profile: "alpha".into(),
            },
            Planned {
                kind: Kind::CodexSaved,
                profile: "beta".into(),
            },
        ];
        let done = quarantine(&places, &planned, None, "2026-11-27T18:00:00Z", "restore").unwrap();
        assert_eq!(done.moved.len(), 2);
        std::fs::remove_dir(&alpha).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&beta, &alpha).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&beta, &alpha).unwrap();

        let back = restore(&places).unwrap();
        let alpha_kept = back
            .kept
            .iter()
            .find(|(m, _)| m.profile == "alpha")
            .map(|(_, kept)| *kept);
        assert_eq!(alpha_kept, Some(Kept::UnsafeDestination));
        assert_eq!(
            std::fs::read(beta.join("auth.json")).unwrap(),
            b"beta-login"
        );
        assert!(
            places.folder().join("manifest.json").exists(),
            "the unsafe entry stays quarantined and recorded"
        );
        assert!(
            std::fs::symlink_metadata(&alpha)
                .unwrap()
                .file_type()
                .is_symlink(),
            "restore must not replace or follow the link"
        );
    }

    #[test]
    fn a_failed_undo_keeps_a_recovery_record_for_what_stays_quarantined() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let places = Places {
            ccs: home.join(".ccs"),
            home,
        };
        std::fs::create_dir_all(&places.ccs).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in [&places.home, &places.ccs] {
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
            }
        }
        std::fs::create_dir_all(places.folder()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(places.folder(), std::fs::Permissions::from_mode(0o700))
                .unwrap();
        }
        let moved = Moved {
            id: "0001".into(),
            kind: Kind::CodexSaved,
            profile: "gone".into(),
            from: "codex-instances/gone/auth.json".into(),
            size: 4,
            digest: digest(b"data"),
        };
        let manifest = Manifest {
            version: 1,
            at: "2026-11-27T18:00:00Z".into(),
            restore: "restore".into(),
            moved: vec![moved.clone()],
        };
        std::fs::write(moved_path(&places, &moved.id), b"data").unwrap();
        let store = crate::registry::claim_as_agent(MANIFEST_ROW, &places.ccs, None).unwrap();
        store.write(&encode(&manifest).unwrap()).unwrap();
        let missing = places.ccs.join("codex-instances/gone/auth.json");
        assert!(!undo_moves(&places, &store, &manifest, &[(moved, missing)]));
        let kept = read_manifest(&places.ccs).unwrap().expect("record stays");
        assert_eq!(kept.moved.len(), 1);
        assert_eq!(kept.moved[0].profile, "gone");
    }

    #[test]
    fn the_agent_root_selector_matches_the_dashboard_rules() {
        let home = PathBuf::from(if cfg!(windows) {
            "C:/home"
        } else {
            "/home/test"
        });
        let mut env = BTreeMap::new();
        assert_eq!(
            selected_ccs_root(&home, &env, None).unwrap(),
            home.join(".ccs")
        );
        env.insert(
            "CCS_HOME".into(),
            if cfg!(windows) {
                "C:/legacy".into()
            } else {
                "/legacy".into()
            },
        );
        assert_eq!(
            selected_ccs_root(&home, &env, None).unwrap(),
            PathBuf::from(env["CCS_HOME"].clone()).join(".ccs")
        );
        env.insert(
            "CCS_DIR".into(),
            if cfg!(windows) {
                "D:/direct".into()
            } else {
                "/direct".into()
            },
        );
        assert_eq!(
            selected_ccs_root(&home, &env, None).unwrap(),
            PathBuf::from(env["CCS_DIR"].clone())
        );
        assert_eq!(
            selected_ccs_root(
                &home,
                &env,
                Some(if cfg!(windows) { "E:/cli" } else { "/cli" })
            )
            .unwrap(),
            PathBuf::from(if cfg!(windows) { "E:/cli" } else { "/cli" })
        );
    }
}
