//! Login-family fingerprints for saved-login renewal
//! (`src/codex-auth/codex-login-family.ts`). A saved login may only be renewed
//! when no other Codex login this machine holds can belong to its family.
//! Fingerprints stay in memory; callers report paths and match labels only.

use crate::jwt::{
    CodexActivationIdentity, OPENAI_AUTH_CLAIM, decode_codex_activation_identity, decode_id_token,
    decode_jwt_payload, matches_codex_activation_identity,
};
use crate::procscan::{CodexProcessScan, ScanProcesses, scan_codex_process_homes};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

const MAX_AUTH_BYTES: u64 = 1024 * 1024;
const MAX_DIRECTORY_ENTRIES: usize = 1000;

/// Private and in-memory only: the family markers of one login.
#[derive(Debug, Clone)]
pub struct CodexLoginFingerprint {
    /// SHA-256 hex of the refresh token, when it carries one.
    pub refresh_digest: Option<String>,
    /// `session_id` (access) and `sid` (id token) values.
    pub session_ids: BTreeSet<String>,
    /// Original sign-in times (`auth_time`), seconds.
    pub auth_times: Vec<f64>,
    /// The account binding decoded from the id token.
    pub binding: Option<CodexActivationIdentity>,
    /// The id token's display email.
    pub email: Option<String>,
    /// Account id claimed by the access token, when it carries one.
    pub access_account_id: Option<String>,
    /// Access-token expiry, epoch ms.
    pub access_expires_at: Option<i64>,
    /// `last_refresh` from the file, epoch ms.
    pub last_refresh_at: Option<i64>,
    /// Whether an access token is present.
    pub has_access_token: bool,
}

/// Where a login source was found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexLoginSourceKind {
    /// The shared native login (`~/.codex`).
    Live,
    /// A saved profile under codex-instances.
    Profile,
    /// A `.codex*` home under the user's home folder.
    CodexHome,
    /// A home named by a running Codex process.
    ProcessHome,
    /// A CLIProxy token file.
    Cliproxy,
    /// An injected extra home.
    Extra,
}

impl CodexLoginSourceKind {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Live => "live",
            Self::Profile => "profile",
            Self::CodexHome => "codex_home",
            Self::ProcessHome => "process_home",
            Self::Cliproxy => "cliproxy",
            Self::Extra => "extra",
        }
    }
}

/// Whether a source holds a readable token login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// A token login was fingerprinted.
    Ok,
    /// Readable, but no token login (an API-key file, another provider).
    None,
    /// The file could not be read or parsed.
    Unreadable,
}

impl SourceState {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::None => "none",
            Self::Unreadable => "unreadable",
        }
    }
}

/// One login file AAC can see.
#[derive(Debug, Clone)]
pub struct CodexLoginSource {
    /// The file path as collected.
    pub path: PathBuf,
    /// Its realpath, when resolvable.
    pub real_path: Option<PathBuf>,
    /// Where it was found.
    pub kind: CodexLoginSourceKind,
    /// Read outcome.
    pub state: SourceState,
    /// The fingerprint, when the file holds a token login.
    pub fingerprint: Option<CodexLoginFingerprint>,
}

/// How another login may belong to the target's family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexFamilyMatch {
    /// The very same file (by realpath).
    SameFile,
    /// The same refresh token.
    RefreshToken,
    /// A shared session id.
    Session,
    /// The same account signed in at the same time.
    AuthTime,
}

impl CodexFamilyMatch {
    /// The wire label.
    pub fn word(self) -> &'static str {
        match self {
            Self::SameFile => "same_file",
            Self::RefreshToken => "refresh_token",
            Self::Session => "session",
            Self::AuthTime => "auth_time",
        }
    }
}

/// The family comparison's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FamilyComparison {
    /// At least one marker matches.
    Matches(Vec<CodexFamilyMatch>),
    /// The other login cannot be ruled out.
    Unverifiable,
    /// The other login cannot belong to the family.
    Unrelated,
}

fn text(value: Option<&Value>) -> Option<String> {
    match value {
        Some(Value::String(s)) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn seconds(value: Option<&Value>) -> Option<f64> {
    let n = value?.as_f64()?;
    if n.is_finite() && n > 0.0 {
        Some(n)
    } else {
        None
    }
}

fn collect_markers(
    claims: Option<&Value>,
    session_ids: &mut BTreeSet<String>,
    auth_times: &mut Vec<f64>,
) {
    let Some(claims) = claims.and_then(Value::as_object) else {
        return;
    };
    let nested = claims.get(OPENAI_AUTH_CLAIM).and_then(Value::as_object);
    for scope in [Some(claims), nested] {
        let Some(scope) = scope else { continue };
        for key in ["session_id", "sid"] {
            if let Some(value) = text(scope.get(key)) {
                session_ids.insert(value);
            }
        }
        if let Some(auth_time) = seconds(scope.get("auth_time"))
            && !auth_times.contains(&auth_time)
        {
            auth_times.push(auth_time);
        }
    }
}

/// `fingerprintCodexLogin`: Codex's `{ tokens: {...} }` shape or a flat token
/// file (CLIProxy). None when it holds no refresh or access token.
pub fn fingerprint_codex_login(parsed: &Value) -> Option<CodexLoginFingerprint> {
    let root = parsed.as_object()?;
    let tokens = match root.get("tokens") {
        Some(Value::Object(nested)) => nested,
        _ => root,
    };
    let refresh_token = text(tokens.get("refresh_token"));
    let access_token = text(tokens.get("access_token"));
    if refresh_token.is_none() && access_token.is_none() {
        return None;
    }
    let id_token = text(tokens.get("id_token"));
    let id_claims = id_token.as_deref().and_then(decode_jwt_payload);
    let access_claims = access_token.as_deref().and_then(decode_jwt_payload);
    let mut session_ids = BTreeSet::new();
    let mut auth_times = Vec::new();
    collect_markers(id_claims.as_ref(), &mut session_ids, &mut auth_times);
    collect_markers(access_claims.as_ref(), &mut session_ids, &mut auth_times);
    let access_account_id = access_claims
        .as_ref()
        .and_then(|c| c.get(OPENAI_AUTH_CLAIM))
        .and_then(Value::as_object)
        .and_then(|auth| text(auth.get("chatgpt_account_id")));
    let access_expires_at = access_claims
        .as_ref()
        .and_then(|c| seconds(c.get("exp")))
        .map(|exp| (exp * 1000.0) as i64);
    let last_refresh_at = match root.get("last_refresh") {
        Some(Value::String(text)) => crate::jsnum::parse_date_ms(text),
        _ => None,
    };
    let binding = id_token.as_deref().map(|token| {
        decode_codex_activation_identity(token, tokens.get("account_id").unwrap_or(&Value::Null))
    });
    let email = id_token
        .as_deref()
        .and_then(|token| decode_id_token(token).email);
    Some(CodexLoginFingerprint {
        refresh_digest: refresh_token.map(|token| {
            let digest = Sha256::digest(token.as_bytes());
            digest.iter().map(|b| format!("{b:02x}")).collect()
        }),
        session_ids,
        auth_times,
        binding: binding.flatten(),
        email,
        access_account_id,
        access_expires_at,
        last_refresh_at,
        has_access_token: access_token.is_some(),
    })
}

/// The bounded read's outcome: bytes, an unreadable file, or a missing one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthBytes {
    /// The file's bytes.
    Bytes(Vec<u8>),
    /// Present but not a bounded regular file, or a read failure.
    Unreadable,
    /// ENOENT / ENOTDIR.
    Missing,
}

fn missing_or_unreadable(error: &std::io::Error) -> AuthBytes {
    match error.kind() {
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory => AuthBytes::Missing,
        _ => AuthBytes::Unreadable,
    }
}

/// `readAuthBytes`: a bounded read of a regular file (links followed).
pub fn read_auth_bytes(file: &Path) -> AuthBytes {
    let stat = match std::fs::metadata(file) {
        Ok(stat) => stat,
        Err(error) => return missing_or_unreadable(&error),
    };
    if !stat.is_file() || stat.len() > MAX_AUTH_BYTES {
        return AuthBytes::Unreadable;
    }
    match std::fs::read(file) {
        Ok(bytes) => AuthBytes::Bytes(bytes),
        Err(error) => missing_or_unreadable(&error),
    }
}

/// `parseAuthJson`: None on any parse failure (parser text can embed tokens).
pub fn parse_auth_json(content: &[u8]) -> Option<Value> {
    let text = String::from_utf8_lossy(content);
    serde_json::from_str(&text).ok()
}

fn real_path_of(file: &Path) -> Option<PathBuf> {
    std::fs::canonicalize(file).ok()
}

/// `readCodexLoginSource`: None for a missing file; unreadable and token-less
/// files stay in the list with their state.
pub fn read_codex_login_source(
    file: &Path,
    kind: CodexLoginSourceKind,
) -> Option<CodexLoginSource> {
    let content = read_auth_bytes(file);
    if content == AuthBytes::Missing {
        return None;
    }
    let base = |state: SourceState, fingerprint: Option<CodexLoginFingerprint>| CodexLoginSource {
        path: file.to_path_buf(),
        real_path: real_path_of(file),
        kind,
        state,
        fingerprint,
    };
    let AuthBytes::Bytes(bytes) = content else {
        return Some(base(SourceState::Unreadable, None));
    };
    let Some(parsed) = parse_auth_json(&bytes) else {
        return Some(base(SourceState::Unreadable, None));
    };
    let fingerprint = fingerprint_codex_login(&parsed);
    let state = if fingerprint.is_some() {
        SourceState::Ok
    } else {
        SourceState::None
    };
    Some(base(state, fingerprint))
}

struct ListEntry {
    name: String,
    is_dir: bool,
}

/// `readdirSync(...).slice(0, 1000)` with the TS `isDirectory` rule: a symlink
/// counts only when its target is a directory.
fn list_directory(directory: &Path) -> Vec<ListEntry> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.take(MAX_DIRECTORY_ENTRIES) {
        let Ok(entry) = entry else { continue };
        let name = entry.file_name().to_string_lossy().into_owned();
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };
        let is_dir = if file_type.is_dir() {
            true
        } else if file_type.is_symlink() {
            std::fs::metadata(directory.join(&name))
                .map(|m| m.is_dir())
                .unwrap_or(false)
        } else {
            false
        };
        out.push(ListEntry { name, is_dir });
    }
    out
}

/// `<home>/.codex*` homes and their direct child homes (such as `~/.codex-t3/<name>`).
pub fn find_home_codex_auth_files(home_dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in list_directory(home_dir) {
        if !entry.name.starts_with(".codex") || !entry.is_dir {
            continue;
        }
        let home = home_dir.join(&entry.name);
        files.push(home.join("auth.json"));
        for child in list_directory(&home) {
            if child.is_dir {
                files.push(home.join(&child.name).join("auth.json"));
            }
        }
    }
    files
}

/// Every `<instances>/<dir>/auth.json`, registered or not (staging, trash).
pub fn find_profile_auth_files(instances_dir: &Path) -> Vec<PathBuf> {
    list_directory(instances_dir)
        .into_iter()
        .filter(|entry| entry.is_dir)
        .map(|entry| instances_dir.join(entry.name).join("auth.json"))
        .collect()
}

/// CLIProxy Codex token files; other providers' files are not Codex logins.
pub fn read_cliproxy_codex_sources(ccs_dir: &Path) -> Vec<CodexLoginSource> {
    let mut sources = Vec::new();
    for folder in ["auth", "auth-paused"] {
        let directory = ccs_dir.join("cliproxy").join(folder);
        for entry in list_directory(&directory) {
            if !entry.name.ends_with(".json") {
                continue;
            }
            let file = directory.join(&entry.name);
            let content = read_auth_bytes(&file);
            if content == AuthBytes::Missing {
                continue;
            }
            let parsed = match &content {
                AuthBytes::Bytes(bytes) => parse_auth_json(bytes),
                _ => None,
            };
            let named = entry.name.to_lowercase().starts_with("codex");
            let Some(parsed) = parsed else {
                if named {
                    sources.push(CodexLoginSource {
                        path: file,
                        real_path: None,
                        kind: CodexLoginSourceKind::Cliproxy,
                        state: SourceState::Unreadable,
                        fingerprint: None,
                    });
                }
                continue;
            };
            let kind_value = parsed.get("type");
            let is_codex = kind_value == Some(&Value::String("codex".into()))
                || (kind_value.is_none() && named);
            if !is_codex {
                continue;
            }
            let fingerprint = fingerprint_codex_login(&parsed);
            let state = if fingerprint.is_some() {
                SourceState::Ok
            } else {
                SourceState::None
            };
            sources.push(CodexLoginSource {
                path: file.clone(),
                real_path: real_path_of(&file),
                kind: CodexLoginSourceKind::Cliproxy,
                state,
                fingerprint,
            });
        }
    }
    sources
}

/// The process scan, re-exported for the planner's environment.
pub fn scan_processes(scan: Option<&ScanProcesses>) -> CodexProcessScan {
    scan_codex_process_homes(scan)
}

fn intersects(left: &BTreeSet<String>, right: &BTreeSet<String>) -> bool {
    left.iter().any(|value| right.contains(value))
}

fn auth_time_intersects(left: &[f64], right: &[f64]) -> bool {
    left.iter().any(|value| right.contains(value))
}

/// Same account, or unknown on either side (an unknown account is never ruled out).
pub fn same_account_or_unknown(
    left: &CodexLoginFingerprint,
    right: &CodexLoginFingerprint,
) -> bool {
    if let (Some(a), Some(b)) = (&left.binding, &right.binding) {
        return matches_codex_activation_identity(a, b) || matches_codex_activation_identity(b, a);
    }
    if let (Some(a), Some(b)) = (&left.email, &right.email) {
        return a.to_lowercase() == b.to_lowercase();
    }
    true
}

/// Whether `other` may hold a login of the target's family: the same file, the
/// same refresh token, a shared session id, or the same account signed in at
/// the same time. Unverifiable when it cannot be read, or may be the same
/// account but carries no family markers to tell the logins apart.
pub fn compare_login_family(
    target_fingerprint: &CodexLoginFingerprint,
    target_real_path: Option<&Path>,
    other: &CodexLoginSource,
) -> FamilyComparison {
    if let Some(real) = target_real_path
        && other.real_path.as_deref() == Some(real)
    {
        return FamilyComparison::Matches(vec![CodexFamilyMatch::SameFile]);
    }
    if other.state == SourceState::Unreadable {
        return FamilyComparison::Unverifiable;
    }
    let Some(theirs) = &other.fingerprint else {
        return FamilyComparison::Unrelated;
    };
    // Without a refresh token a copy can never rotate or replay the family.
    if theirs.refresh_digest.is_none() {
        return FamilyComparison::Unrelated;
    }
    let ours = target_fingerprint;
    let mut matches = Vec::new();
    if let (Some(a), Some(b)) = (&ours.refresh_digest, &theirs.refresh_digest)
        && a == b
    {
        matches.push(CodexFamilyMatch::RefreshToken);
    }
    if intersects(&ours.session_ids, &theirs.session_ids) {
        matches.push(CodexFamilyMatch::Session);
    }
    let related = same_account_or_unknown(ours, theirs);
    if related && auth_time_intersects(&ours.auth_times, &theirs.auth_times) {
        matches.push(CodexFamilyMatch::AuthTime);
    }
    if !matches.is_empty() {
        return FamilyComparison::Matches(matches);
    }
    if related && theirs.session_ids.is_empty() && theirs.auth_times.is_empty() {
        return FamilyComparison::Unverifiable;
    }
    FamilyComparison::Unrelated
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn flat_and_nested_token_shapes_fingerprint() {
        let nested = json!({"tokens": {"refresh_token": "rt-FAKE", "access_token": ""}});
        let flat = json!({"refresh_token": "rt-FAKE"});
        let a = fingerprint_codex_login(&nested).expect("nested");
        let b = fingerprint_codex_login(&flat).expect("flat");
        assert_eq!(a.refresh_digest, b.refresh_digest);
        assert!(!a.has_access_token);
        assert!(fingerprint_codex_login(&json!({"OPENAI_API_KEY": "sk-FAKE"})).is_none());
        assert!(fingerprint_codex_login(&json!([])).is_none());
    }

    #[test]
    fn markers_collect_from_both_claims_and_scopes() {
        use base64::Engine;
        let b64 =
            |v: &Value| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string());
        let payload = json!({
            "session_id": "sess-access",
            "auth_time": 1_780_000_000,
            "https://api.openai.com/auth": {"sid": "sess-nested", "auth_time": 1_780_000_100}
        });
        let access = format!(
            "{}.{}.{}",
            b64(&json!({"alg":"RS256"})),
            b64(&payload),
            b64(&json!("FAKE-SIG"))
        );
        let parsed = json!({"tokens": {"access_token": access}});
        let fp = fingerprint_codex_login(&parsed).expect("fp");
        assert!(fp.session_ids.contains("sess-access"));
        assert!(fp.session_ids.contains("sess-nested"));
        assert_eq!(fp.auth_times.len(), 2);
    }

    #[test]
    fn family_comparison_rules() {
        let ours = fingerprint_codex_login(&json!({
            "tokens": {"refresh_token": "rt-FAKE-a", "access_token": "x"}
        }))
        .expect("ours");
        let mut other = CodexLoginSource {
            path: PathBuf::from("/other/auth.json"),
            real_path: None,
            kind: CodexLoginSourceKind::CodexHome,
            state: SourceState::Ok,
            fingerprint: fingerprint_codex_login(
                &json!({"tokens": {"refresh_token": "rt-FAKE-a"}}),
            ),
        };
        assert_eq!(
            compare_login_family(&ours, None, &other),
            FamilyComparison::Matches(vec![CodexFamilyMatch::RefreshToken])
        );
        other.state = SourceState::Unreadable;
        other.fingerprint = None;
        assert_eq!(
            compare_login_family(&ours, None, &other),
            FamilyComparison::Unverifiable
        );
        other.state = SourceState::None;
        assert_eq!(
            compare_login_family(&ours, None, &other),
            FamilyComparison::Unrelated
        );
    }
}
