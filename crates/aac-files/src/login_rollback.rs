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

/// The identity a saved Codex copy proved before it was moved: the agent inventory's identity key
/// (a digest of the workspace and principal binding, never a token) and the login's email.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexPin {
    /// 64 lowercase hex digits (`codex-principal-v1` identity key).
    pub identity: String,
    /// The login's email, for the row while no T3 login answers.
    pub email: String,
}

/// The step 3 switch. Absent: everything reads and renews as before step 3.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Switch {
    /// Always 1.
    pub version: u32,
    /// When the quarantine ran (UTC, seconds).
    pub since: String,
    /// Set: Codex accounts are read only through a T3 instance's own login with the pinned
    /// identity, never through a saved copy. Profiles not named here are shown unavailable.
    #[serde(default)]
    pub codex: Option<BTreeMap<String, CodexPin>>,
    /// Set: the Claude renewal runner renews only this computer's sign-ins, one per account.
    #[serde(default)]
    pub claude_renewal_host: Option<String>,
}

/// A profile or computer name: 1-64 of letters, digits, `.`, `_`, `-`, starting with a letter or
/// digit (the renewal runner's and the Codex registry's names both fit).
pub fn valid_name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn valid_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_email(value: &str) -> bool {
    (3..=254).contains(&value.len())
        && value.contains('@')
        && value.bytes().all(|b| b.is_ascii_graphic())
}

fn valid_stamp(value: &str) -> bool {
    value.len() == 20
        && value.ends_with('Z')
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || b"-:TZ".contains(&b))
}

impl Switch {
    /// Every field's shape.
    pub fn valid(&self) -> bool {
        self.version == 1
            && valid_stamp(&self.since)
            && self.codex.as_ref().is_none_or(|pins| {
                pins.len() <= 64
                    && pins.iter().all(|(name, pin)| {
                        valid_name(name) && valid_identity(&pin.identity) && valid_email(&pin.email)
                    })
            })
            && self.claude_renewal_host.as_deref().is_none_or(valid_name)
    }

    /// Parse and check the switch's bytes.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice::<Self>(bytes)
            .ok()
            .filter(Self::valid)
    }
}

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
    /// A move failed; every earlier move was put back.
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

/// Move every planned file that exists into the quarantine folder, record it, then write the
/// switch (when given). All or nothing: any unsafe file, or a failed move, leaves every file where
/// it was and writes no record and no switch.
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
    let undo = |done: &[(Moved, PathBuf)]| {
        for (m, from) in done.iter().rev() {
            let _ = std::fs::rename(moved_path(places, &m.id), from);
        }
        let _ = manifest_store.remove();
    };
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
        if there {
            out.kept.push((m.clone(), Kept::NewerThere));
            continue;
        }
        let made = from.parent().is_some_and(|parent| {
            parent.is_dir()
                || std::fs::create_dir_all(parent)
                    .and_then(|()| {
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                parent,
                                std::fs::Permissions::from_mode(0o700),
                            )?;
                        }
                        Ok(())
                    })
                    .is_ok()
        });
        if !made || std::fs::rename(&source, &from).is_err() {
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
        let text = std::fs::read_to_string(&record).unwrap().replace(
            &p.codex_copy("work").unwrap().to_string_lossy().into_owned(),
            &elsewhere.to_string_lossy(),
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
}
