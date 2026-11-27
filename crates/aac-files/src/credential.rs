//! The `credential` file class (Central Logins step 2): one owner-only file per static login a
//! computer holds, under `logins/` in the AAC private folder (registry row `login-credentials`).
//!
//! **Static only.** The class knows the login kinds by name: a kind that never rotates (a Claude
//! `setup-token` token, a provider API key) may be written; every kind whose tool refreshes it (a
//! Claude or Codex sign-in, an Antigravity, Muse or Cursor sign-in, a pasted web session) is
//! refused by its type, before any byte is read or written. A secret whose shape is a rotating
//! credential (a refresh token, a JWT, a whole JSON sign-in file) is refused the same way.
//!
//! **Writes.** Every write holds the file's kernel lease from the read to the rename: the caller
//! sees the current bytes and decides (compare before replace), the old file is first copied to
//! `logins/previous/<name>` (one generation, 0600), then the new bytes are staged with `O_EXCL`
//! beside the target at 0600, fsynced, renamed and the folder fsynced ([`OwnedStore::write`]).
//! A file that is a link, has a second hard link, belongs to another user or carries a group or
//! other bit is refused, never repaired.
//!
//! **No secret leaves through a summary.** [`Summary`] has no field for the secret, and
//! [`Record`]'s `Debug` prints none. On Windows the secret is sealed with DPAPI (`CurrentUser`).
use crate::read::{self, Ancestors, Mode, Owner, Read, ReadPolicy};
use crate::{OwnedStore, registry, single_link};
use aac_core::{Cause, Failure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
use std::path::{Path, PathBuf};

/// The registry row of the class.
pub const ROW_ID: &str = "login-credentials";
/// The folder under the private root.
pub const FOLDER: &str = "logins";
/// The one-generation backup folder under [`FOLDER`].
pub const PREVIOUS: &str = "previous";
/// A record's byte cap (the row's `maxBytes`).
pub const MAX_BYTES: usize = 8192;
/// The most files one listing reads.
pub const MAX_FILES: usize = 256;

/// The kind of a login, by name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Kind {
    /// Claude Code's one-year `claude setup-token` token (`CLAUDE_CODE_OAUTH_TOKEN`); it never
    /// rotates, so it may sit on every computer.
    ClaudeToken,
    /// A provider API key (Z.ai, Kimi Code, OpenCode Go, ...): static until revoked.
    ApiKey,
}

/// Why a kind name is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    /// A sign-in its tool refreshes: copying it makes a second holder.
    Rotating,
    /// Not a kind this class knows.
    Unknown,
}

/// Kinds whose tool refreshes the sign-in. They are named so the refusal says why.
pub const ROTATING_KINDS: [&str; 7] = [
    "claude-signin",
    "codex-signin",
    "antigravity-signin",
    "muse-signin",
    "cursor-signin",
    "qwen-signin",
    "web-session",
];

impl Kind {
    /// Admit a kind by name: static kinds only, every rotating one refused by its type.
    pub fn admit(word: &str) -> Result<Self, Refusal> {
        match word {
            "claude-token" => Ok(Self::ClaudeToken),
            "api-key" => Ok(Self::ApiKey),
            word if ROTATING_KINDS.contains(&word) => Err(Refusal::Rotating),
            _ => Err(Refusal::Unknown),
        }
    }

    /// The wire spelling.
    pub fn word(self) -> &'static str {
        match self {
            Self::ClaudeToken => "claude-token",
            Self::ApiKey => "api-key",
        }
    }
}

/// Admit a secret for a kind: one line of printable ASCII, 8 to 512 bytes, and not the shape of
/// a rotating credential (a refresh token, a JWT access token, a Google token, a JSON sign-in
/// file). A Claude token must have the `setup-token` prefix.
pub fn admit_secret(kind: Kind, secret: &str) -> Result<(), Refusal> {
    let printable =
        (8..=512).contains(&secret.len()) && secret.bytes().all(|b| (0x21..=0x7e).contains(&b));
    if !printable {
        return Err(Refusal::Unknown);
    }
    let lower = secret.to_ascii_lowercase();
    let jwt = secret.starts_with("eyJ") && secret.matches('.').count() == 2;
    let rotating = secret.starts_with("sk-ant-ort")
        || secret.starts_with('{')
        || secret.starts_with('[')
        || secret.starts_with("ya29.")
        || secret.starts_with("1//")
        || lower.contains("refresh")
        || jwt;
    if rotating {
        return Err(Refusal::Rotating);
    }
    match kind {
        Kind::ClaudeToken if !secret.starts_with("sk-ant-oat01-") => Err(Refusal::Unknown),
        Kind::ApiKey if secret.starts_with("sk-ant-oat") => Err(Refusal::Rotating),
        _ => Ok(()),
    }
}

/// The public fingerprint of a secret: `sha256:` and the first 16 hex digits of its SHA-256 (the
/// account-key receipt's rule). It identifies a secret without revealing it.
pub fn fingerprint(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let hex: String = digest[..8].iter().map(|b| format!("{b:02x}")).collect();
    format!("sha256:{hex}")
}

/// A fingerprint's shape.
pub fn valid_fingerprint(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    })
}

/// A login id: 8 lowercase hex digits (the dashboard's registry assigns it).
pub fn valid_login_id(value: &str) -> bool {
    value.len() == 8
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A provider id: a lowercase word.
pub fn valid_provider(value: &str) -> bool {
    (1..=32).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
}

/// The account a login belongs to: the dashboard's opaque account key, printable ASCII without
/// spaces or quotes, at most 128 bytes.
pub fn valid_account(value: &str) -> bool {
    (1..=128).contains(&value.len())
        && value
            .bytes()
            .all(|c| (0x21..=0x7e).contains(&c) && !b"\"'\\`".contains(&c))
}

/// An ISO UTC second stamp `YYYY-MM-DDTHH:MM:SSZ`.
pub fn valid_stamp(value: &str) -> bool {
    let b = value.as_bytes();
    b.len() == 20
        && b.iter().enumerate().all(|(i, c)| match i {
            4 | 7 => *c == b'-',
            10 => *c == b'T',
            13 | 16 => *c == b':',
            19 => *c == b'Z',
            _ => c.is_ascii_digit(),
        })
}

/// The file name of one login: `<provider>-<loginId>.json`.
pub fn file_name(provider: &str, login_id: &str) -> Option<String> {
    (valid_provider(provider) && valid_login_id(login_id))
        .then(|| format!("{provider}-{login_id}.json"))
}

/// A login's facts without its secret: what an inventory or a receipt may carry.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Summary {
    /// The dashboard's login id.
    pub login_id: String,
    /// The kind.
    pub kind: Kind,
    /// The provider id.
    pub provider: String,
    /// The account key.
    pub account: String,
    /// The secret's fingerprint.
    pub fingerprint: String,
    /// When the secret ends, if known (ISO second stamp).
    pub expires_at: Option<String>,
    /// When this computer received it (ISO second stamp).
    pub written_at: String,
}

impl Summary {
    fn valid(&self) -> bool {
        valid_login_id(&self.login_id)
            && valid_provider(&self.provider)
            && valid_account(&self.account)
            && valid_fingerprint(&self.fingerprint)
            && self.expires_at.as_deref().is_none_or(valid_stamp)
            && valid_stamp(&self.written_at)
            && (self.kind != Kind::ClaudeToken || self.provider == "claude")
    }

    /// The same login: id, kind, provider and account all equal.
    pub fn same_identity(&self, other: &Summary) -> bool {
        self.login_id == other.login_id
            && self.kind == other.kind
            && self.provider == other.provider
            && self.account == other.account
    }
}

/// One stored login: its facts and its secret. `Debug` never prints the secret.
#[derive(Clone, PartialEq, Eq)]
pub struct Record {
    /// The facts.
    pub summary: Summary,
    secret: String,
}

impl fmt::Debug for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Record")
            .field("summary", &self.summary)
            .field("secret", &"<redacted>")
            .finish()
    }
}

impl Record {
    /// A new record; refuses a rotating kind's shape, a mismatched fingerprint or bad facts.
    pub fn new(summary: Summary, secret: String) -> Result<Self, Refusal> {
        admit_secret(summary.kind, &secret)?;
        if !summary.valid() || summary.fingerprint != fingerprint(&secret) {
            return Err(Refusal::Unknown);
        }
        Ok(Self { summary, secret })
    }

    /// The same login with the same secret and end; only the arrival time may differ.
    pub fn same_content(&self, other: &Record) -> bool {
        self.summary.same_identity(&other.summary)
            && self.summary.fingerprint == other.summary.fingerprint
            && self.summary.expires_at == other.summary.expires_at
            && self.secret == other.secret
    }

    /// The secret, for the one caller that hands it to another computer's agent.
    pub fn secret(&self) -> &str {
        &self.secret
    }

    /// The bytes on disk.
    pub fn encode(&self) -> Result<Vec<u8>, Failure> {
        let refused = || Failure::new(Cause::Refused, "login could not be stored safely");
        #[cfg(windows)]
        let (encoding, secret, ciphertext) = {
            use base64::{Engine, engine::general_purpose::STANDARD};
            let sealed = crate::web_session::dpapi::protect_with(ENTROPY, self.secret.as_bytes())
                .ok_or_else(refused)?;
            ("dpapi-v1", None, Some(STANDARD.encode(sealed)))
        };
        #[cfg(not(windows))]
        let (encoding, secret, ciphertext) = ("plain", Some(self.secret.clone()), None::<String>);
        let disk = Disk {
            version: 1,
            summary: self.summary.clone(),
            encoding: encoding.into(),
            secret,
            ciphertext,
        };
        let mut bytes = serde_json::to_vec(&disk).map_err(|_| refused())?;
        bytes.push(b'\n');
        if bytes.len() > MAX_BYTES {
            return Err(refused());
        }
        Ok(bytes)
    }

    /// Read the bytes on disk back, checking every fact again.
    pub fn decode(bytes: &[u8]) -> Result<Self, Failure> {
        let damaged = || Failure::new(Cause::Damaged, "stored login is not valid");
        let disk: Disk = serde_json::from_slice(bytes).map_err(|_| damaged())?;
        if disk.version != 1 {
            return Err(damaged());
        }
        let secret = match (disk.encoding.as_str(), disk.secret, disk.ciphertext) {
            ("plain", Some(secret), None) => secret,
            #[cfg(windows)]
            ("dpapi-v1", None, Some(sealed)) => {
                use base64::{Engine, engine::general_purpose::STANDARD};
                let blob = STANDARD.decode(sealed).map_err(|_| damaged())?;
                let plain = crate::web_session::dpapi::unprotect_with(ENTROPY, &blob)
                    .ok_or_else(damaged)?;
                String::from_utf8(plain).map_err(|_| damaged())?
            }
            _ => return Err(damaged()),
        };
        Self::new(disk.summary, secret).map_err(|_| damaged())
    }
}

#[cfg(windows)]
const ENTROPY: &[u8] = b"AAC/login/v1";

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Disk {
    version: u32,
    #[serde(flatten)]
    summary: Summary,
    encoding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ciphertext: Option<String>,
}

/// What a write did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Written {
    /// A new file.
    Created,
    /// An existing file was backed up and replaced.
    Replaced,
    /// The file already held exactly these bytes.
    Unchanged,
}

/// One login file held under its writer lease.
#[derive(Debug)]
pub struct CredentialFile {
    store: OwnedStore,
    root: PathBuf,
    name: String,
}

fn policy(root: &Path) -> ReadPolicy<'_> {
    ReadPolicy::capped(MAX_BYTES as u64)
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(root))
}

fn unsafe_file() -> Failure {
    Failure::new(Cause::Refused, "login file is not private")
}

/// Read one file by the class's rules: absent, or a single-link 0600 file of this user that is
/// no link and no larger than the cap. Anything else is refused.
fn read_private(root: &Path, file: &Path) -> Result<Option<Vec<u8>>, Failure> {
    match read::read(file, &policy(root)) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, _) if single_link(file) => Ok(Some(bytes)),
        _ => Err(unsafe_file()),
    }
}

impl CredentialFile {
    /// The agent's claim on one login file under `root` (the computer's AAC private folder).
    pub fn claim_as_agent(root: &Path, provider: &str, login_id: &str) -> Result<Self, Failure> {
        let name = file_name(provider, login_id)
            .ok_or_else(|| Failure::new(Cause::Refused, "invalid login name"))?;
        let store = registry::claim_as_agent(ROW_ID, root, Some(&name))?;
        Ok(Self {
            store,
            root: root.to_path_buf(),
            name,
        })
    }

    /// The current record, if any (refused when the file is not private or not valid).
    pub fn read(&self) -> Result<Option<Record>, Failure> {
        read_private(&self.root, self.store.path())?
            .map(|bytes| Record::decode(&bytes))
            .transpose()
    }

    /// Replace the record. `decide` sees the current record (read under this lease) and refuses
    /// by returning an error, so nothing changes; the old file is backed up before the rename.
    pub fn replace(
        &self,
        record: &Record,
        decide: impl FnOnce(Option<&Record>) -> Result<(), Failure>,
    ) -> Result<Written, Failure> {
        let current_bytes = read_private(&self.root, self.store.path())?;
        let current = current_bytes.as_deref().map(Record::decode).transpose()?;
        decide(current.as_ref())?;
        // A repeat of what is already there (same login, secret and end) changes nothing: the file,
        // its arrival time and the previous generation all stay as they are.
        if current.as_ref().is_some_and(|c| c.same_content(record)) {
            return Ok(Written::Unchanged);
        }
        let bytes = record.encode()?;
        let written = match current_bytes {
            Some(old) => {
                self.backup(&old)?;
                Written::Replaced
            }
            None => Written::Created,
        };
        self.store.write(&bytes)?;
        Ok(written)
    }

    /// Remove the record after `decide` accepts it; the old file is backed up first. An absent
    /// file answers `false` without calling `decide`.
    pub fn remove(
        &self,
        decide: impl FnOnce(&Record) -> Result<(), Failure>,
    ) -> Result<bool, Failure> {
        let Some(old) = read_private(&self.root, self.store.path())? else {
            return Ok(false);
        };
        decide(&Record::decode(&old)?)?;
        self.backup(&old)?;
        self.store.remove()?;
        Ok(true)
    }

    /// The one-generation copy of what a write replaces: `logins/previous/<name>`, 0600, staged,
    /// fsynced and renamed under this file's own lease (the same open file description).
    fn backup(&self, bytes: &[u8]) -> Result<(), Failure> {
        let parent = self
            .store
            .path()
            .parent()
            .ok_or_else(|| Failure::new(Cause::Refused, "missing login folder"))?;
        let path = parent.join(PREVIOUS).join(&self.name);
        let lock = match &self.store._lock {
            crate::StoreLock::Kernel { _file } => _file.try_clone()?,
            crate::StoreLock::Config { .. } => return Err(unsafe_file()),
        };
        if read_private(&self.root, &path).is_err() {
            return Err(unsafe_file());
        }
        let backup = OwnedStore {
            path,
            max_size: MAX_BYTES,
            parents: crate::Parents::Private,
            mode: 0o600,
            _lock: crate::StoreLock::Kernel { _file: lock },
        };
        backup.write(bytes)
    }
}

/// One listed file: its name and its facts, or why it could not be read safely.
pub type Listed = (String, Result<Summary, Failure>);

/// Every login file on this computer, by name, each read by the class's rules: a file that
/// cannot be read safely is listed as an error, never skipped silently or repaired.
pub fn list(root: &Path) -> Result<Vec<Listed>, Failure> {
    let folder = root.join(FOLDER);
    let entries = match read::entries(&folder, MAX_FILES + 64) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let row = registry::row(ROW_ID)?;
    let mut out = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| Failure::new(Cause::OutputTooLarge, "too many logins"))?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        // Lock files, staging files and the backup folder are not logins.
        if row.file(Some(&name)).is_err() {
            continue;
        }
        if out.len() == MAX_FILES {
            return Err(Failure::new(Cause::OutputTooLarge, "too many logins"));
        }
        let read = read_private(root, &folder.join(&name)).and_then(|bytes| {
            let bytes = bytes.ok_or_else(|| Failure::new(Cause::Damaged, "login vanished"))?;
            let record = Record::decode(&bytes)?;
            if file_name(&record.summary.provider, &record.summary.login_id).as_deref()
                != Some(name.as_str())
            {
                return Err(Failure::new(Cause::Damaged, "login file name differs"));
            }
            Ok(record.summary)
        });
        out.push((name, read));
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// The registry row of this computer's login identity (`login-computer.json`).
pub const COMPUTER_ROW_ID: &str = "login-computer";

/// This computer's identity for login delivery: a random key made once by its agent, plus its
/// host name. The dashboard pins both when the owner checks the computers, and every write names
/// the pinned key, so a request routed to another computer is refused before a file is touched.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ComputerId {
    /// 32 lowercase hex digits.
    pub key: String,
    /// The computer's host name (1-64 of letters, digits, `.`, `_`, `-`).
    pub host: String,
}

/// A computer key's shape.
pub fn valid_computer_key(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// A host name's shape.
pub fn valid_host(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"._-".contains(&c))
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ComputerDisk {
    version: u32,
    key: String,
}

fn parse_key(bytes: &[u8]) -> Result<String, Failure> {
    let disk: ComputerDisk = serde_json::from_slice(bytes)
        .map_err(|_| Failure::new(Cause::Damaged, "computer key is not valid"))?;
    if disk.version != 1 || !valid_computer_key(&disk.key) {
        return Err(Failure::new(Cause::Damaged, "computer key is not valid"));
    }
    Ok(disk.key)
}

/// This computer's key under `root`, made (0600, under its lease) the first time it is asked.
/// A file that is not private, or not valid, is refused, never replaced.
pub fn computer_key(root: &Path) -> Result<String, Failure> {
    // An existing key is read without the lease: it never changes once made, and readers must
    // not queue behind each other. Only making it takes the lease (and reads again under it).
    let row = registry::row(COMPUTER_ROW_ID)?;
    let path = root.join(&row.path);
    if let Ok(Some(bytes)) = read_private(root, &path) {
        return parse_key(&bytes);
    }
    let start = std::time::Instant::now();
    let store = loop {
        match registry::claim_as_agent(COMPUTER_ROW_ID, root, None) {
            Err(e)
                if e.cause == Cause::Busy
                    && start.elapsed() < std::time::Duration::from_secs(2) =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            other => break other?,
        }
    };
    match read::read(store.path(), &policy(root)) {
        Read::Bytes(bytes, _) if single_link(store.path()) => parse_key(&bytes),
        Read::Absent => {
            let mut raw = [0u8; 16];
            getrandom::fill(&mut raw)
                .map_err(|_| Failure::new(Cause::Refused, "no random source"))?;
            let key: String = raw.iter().map(|b| format!("{b:02x}")).collect();
            let mut bytes = serde_json::to_vec(&ComputerDisk {
                version: 1,
                key: key.clone(),
            })
            .map_err(|_| Failure::new(Cause::Bug, "computer key encoding"))?;
            bytes.push(b'\n');
            store.write(&bytes)?;
            Ok(key)
        }
        _ => Err(unsafe_file()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const TOKEN: &str = "sk-ant-oat01-TESTONLY-not-a-real-token-0000000000";

    fn summary(secret: &str) -> Summary {
        Summary {
            login_id: "0a1b2c3d".into(),
            kind: Kind::ClaudeToken,
            provider: "claude".into(),
            account: "claude:test-account".into(),
            fingerprint: fingerprint(secret),
            expires_at: Some("2027-11-27T00:00:00Z".into()),
            written_at: "2026-11-27T05:00:00Z".into(),
        }
    }

    fn root() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = crate::path::node::absolute(dir.path())
            .unwrap()
            .join(".ccs");
        (dir, root)
    }

    #[test]
    fn rotating_kinds_are_refused_by_type() {
        for word in ROTATING_KINDS {
            assert_eq!(Kind::admit(word), Err(Refusal::Rotating), "{word}");
        }
        assert_eq!(Kind::admit("auth-json"), Err(Refusal::Unknown));
        assert_eq!(Kind::admit("claude-token"), Ok(Kind::ClaudeToken));
        assert_eq!(Kind::admit("api-key"), Ok(Kind::ApiKey));
    }

    #[test]
    fn rotating_shapes_are_refused_whatever_the_kind() {
        for secret in [
            "sk-ant-ort01-TESTONLY-refresh",
            "{\"access_token\":\"x\",\"refresh_token\":\"y\"}",
            "eyJhbGciOiJub25lIn0.eyJzdWIiOiJ0In0.c2ln",
            "ya29.TESTONLY-google-access",
            "1//TESTONLY-google-refresh",
            "key-with-refresh-inside",
        ] {
            for kind in [Kind::ClaudeToken, Kind::ApiKey] {
                assert_eq!(
                    admit_secret(kind, secret),
                    Err(Refusal::Rotating),
                    "{secret}"
                );
            }
        }
        assert_eq!(
            admit_secret(Kind::ApiKey, "sk-ant-oat01-TESTONLY-as-key"),
            Err(Refusal::Rotating)
        );
        assert_eq!(
            admit_secret(Kind::ClaudeToken, "zai-TESTONLY-key-000"),
            Err(Refusal::Unknown)
        );
        assert_eq!(admit_secret(Kind::ClaudeToken, TOKEN), Ok(()));
        assert_eq!(admit_secret(Kind::ApiKey, "zai.TESTONLY.key0000"), Ok(()));
        assert_eq!(
            admit_secret(Kind::ApiKey, "two words"),
            Err(Refusal::Unknown)
        );
    }

    #[test]
    fn a_record_never_debugs_its_secret_and_round_trips() {
        let record = Record::new(summary(TOKEN), TOKEN.into()).unwrap();
        assert!(!format!("{record:?}").contains("TESTONLY"));
        let bytes = record.encode().unwrap();
        assert_eq!(Record::decode(&bytes).unwrap(), record);
        // A fingerprint that is not the secret's is refused.
        let mut wrong = summary(TOKEN);
        wrong.fingerprint = fingerprint("other");
        assert!(Record::new(wrong, TOKEN.into()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_replace_compares_backs_up_and_writes_0600() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, root) = root();
        let first = Record::new(summary(TOKEN), TOKEN.into()).unwrap();
        let file = CredentialFile::claim_as_agent(&root, "claude", "0a1b2c3d").unwrap();
        assert_eq!(
            file.replace(&first, |current| {
                assert!(current.is_none());
                Ok(())
            })
            .unwrap(),
            Written::Created
        );
        let path = root.join("logins/claude-0a1b2c3d.json");
        let mode = std::fs::symlink_metadata(&path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert_eq!(
            file.replace(&first, |_| Ok(())).unwrap(),
            Written::Unchanged
        );
        // A refusing decision changes nothing.
        let other = "sk-ant-oat01-TESTONLY-second-token-1111111111";
        let second = Record::new(summary(other), other.into()).unwrap();
        assert!(
            file.replace(&second, |_| Err(Failure::new(
                Cause::ConfirmationInvalid,
                "changed"
            )))
            .is_err()
        );
        assert_eq!(file.read().unwrap().unwrap(), first);
        assert_eq!(
            file.replace(&second, |_| Ok(())).unwrap(),
            Written::Replaced
        );
        assert_eq!(file.read().unwrap().unwrap(), second);
        let backup = root.join("logins/previous/claude-0a1b2c3d.json");
        let kept = Record::decode(&crate::read_bounded(&backup, MAX_BYTES).unwrap()).unwrap();
        assert_eq!(kept, first);
        let mode = std::fs::symlink_metadata(&backup)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        // The listing has the one login and no secret field.
        let listed = list(&root).unwrap();
        assert_eq!(listed.len(), 1);
        let text = serde_json::to_string(listed[0].1.as_ref().unwrap()).unwrap();
        assert!(!text.contains("TESTONLY"));
        // Remove backs up and deletes.
        assert!(file.remove(|_| Ok(())).unwrap());
        assert!(file.read().unwrap().is_none());
        assert!(!file.remove(|_| Ok(())).unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn a_loose_or_linked_file_is_refused_not_repaired() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, root) = root();
        let record = Record::new(summary(TOKEN), TOKEN.into()).unwrap();
        let file = CredentialFile::claim_as_agent(&root, "claude", "0a1b2c3d").unwrap();
        file.replace(&record, |_| Ok(())).unwrap();
        let path = root.join("logins/claude-0a1b2c3d.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(file.read().is_err());
        assert!(file.replace(&record, |_| Ok(())).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::hard_link(&path, root.join("logins/extra")).unwrap();
        assert!(file.read().is_err());
        assert!(list(&root).unwrap()[0].1.is_err());
    }

    /// Red/green (review M3): repeating an unchanged delivery keeps the previous generation.
    #[cfg(unix)]
    #[test]
    fn an_unchanged_write_keeps_the_file_and_its_backup() {
        let (_dir, root) = root();
        let other = "sk-ant-oat01-TESTONLY-second-token-1111111111";
        let file = CredentialFile::claim_as_agent(&root, "claude", "0a1b2c3d").unwrap();
        file.replace(&Record::new(summary(TOKEN), TOKEN.into()).unwrap(), |_| {
            Ok(())
        })
        .unwrap();
        let second = Record::new(summary(other), other.into()).unwrap();
        file.replace(&second, |_| Ok(())).unwrap();
        let backup = root.join("logins/previous/claude-0a1b2c3d.json");
        let before = crate::read_bounded(&backup, MAX_BYTES).unwrap();
        let mut later = summary(other);
        later.written_at = "2026-11-28T05:00:00Z".into();
        let repeat = Record::new(later, other.into()).unwrap();
        assert_eq!(
            file.replace(&repeat, |_| Ok(())).unwrap(),
            Written::Unchanged
        );
        assert_eq!(crate::read_bounded(&backup, MAX_BYTES).unwrap(), before);
        assert_eq!(file.read().unwrap().unwrap(), second);
    }

    #[cfg(unix)]
    #[test]
    fn a_computer_key_is_made_once_and_kept() {
        let (_dir, root) = root();
        let key = computer_key(&root).unwrap();
        assert!(valid_computer_key(&key));
        assert_eq!(computer_key(&root).unwrap(), key);
    }

    #[test]
    fn the_registry_row_is_a_private_agent_credential_row() {
        let row = registry::row(ROW_ID).unwrap();
        assert_eq!(row.class, crate::Class::Credential);
        assert_eq!(row.mode, "0600");
        assert!(row.agent);
        assert_eq!(row.max_bytes, MAX_BYTES as u64);
        assert!(row.file(Some("claude-0a1b2c3d.json")).is_ok());
        assert!(row.file(Some("claude-0a1b2c3d.json.lock")).is_err());
    }
}
