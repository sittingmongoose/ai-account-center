//! Own-host Antigravity and OpenCode workspace sources for usage failover, ported from
//! `scripts/account-usage/failover_special.py`.
//!
//! Inventory is read-only and makes no provider requests. Its identity hints select a
//! candidate; collection must prove that identity again with the provider. Fields
//! prefixed with `_` are private process data, never part of the wire inventory.
pub(crate) mod wallet;

pub use crate::failover_additional::InventoryResult;
use crate::failover_additional::{
    desktop::{
        AGY_BASE, AGY_CLI_BASE, AGY_USER_AGENT, Credential, MAX_RESPONSE_BYTES, ReadTracker,
        account, agy_groups, antigravity_oauth_client, antigravity_reported_plan,
        antigravity_service_label, antigravity_unavailable_windows, credential_from_omp,
        decode_strict_json_object, email as desktop_email, jwt_claims, normalize_antigravity,
        normalize_antigravity_credits, now_unix_secs, parse_iso_utc, platform_label, reset_at,
        safe_text, secret as desktop_secret, utc_now_iso_secs,
    },
    sqlite::{self, OmpRawRow},
};
use aac_core::sha256_hex;
use aac_files::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use aac_providers::ProviderId;
use chrono::DateTime;
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::BTreeSet,
    fs,
    future::Future,
    io::ErrorKind,
    path::{Component, Path, PathBuf},
    pin::Pin,
    sync::LazyLock,
    time::Duration,
};

const MAX_BYTES: usize = 1024 * 1024;
const IDENTITY_KIND: &str = "antigravity-subject";
const WALLET_KIND: &str = "opencode-workspace";
const USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const HTTP_TIMEOUT_SECS: u64 = 8;

static HASH_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-f0-9]{64}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static PROFILE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-z][a-z0-9_-]{0,47}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static REGISTRY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\Aregistry-([0-9]{12})\.json\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static SUBJECT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9]{1,128}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static DIGITS_1_12_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9]{1,12}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});

/// Google HTTP transport error matching `failover_special.HttpClient`.
#[derive(Debug, Clone, PartialEq)]
pub enum GoogleHttpError {
    /// HTTP 429 rate limit with bounded `Retry-After` seconds (`1..=86400`, default `60`).
    RateLimited(u64),
    /// HTTP 401/403 or OAuth 400 `invalid_grant`.
    NeedsSignIn(&'static str),
    /// Provider or transport failure.
    Error(&'static str),
}

/// Injectable HTTP transport for the fixed Google OAuth/Antigravity endpoints.
pub trait GoogleTransport: Send + Sync {
    /// Perform a bounded JSON or form-encoded request against an allowed Google URL.
    fn request_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a Value>,
        form: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GoogleHttpError>> + Send + 'a>>;
}

/// OpenCode console wallet transport error matching `opencode_console.Unavailable`.
#[derive(Debug, Clone, PartialEq)]
pub struct WalletError {
    /// Fixed error code (`"rate_limited"`, `"needs_sign_in"`, `"workspace_mismatch"`, etc.).
    pub code: &'static str,
    /// Optional bounded retry-after seconds.
    pub retry_after_seconds: Option<f64>,
}

/// Injectable HTTP transport for OpenCode console routes (`https://opencode.ai/console/api/...`).
pub trait WalletTransport: Send + Sync {
    /// Perform a GET request against one allowed console API route.
    fn get<'a>(
        &'a self,
        route: &'a str,
        workspace: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WalletError>> + Send + 'a>>;
}

/// Optional overrides for deterministic offline tests; production uses `Default`.
#[derive(Default)]
pub struct SpecialEnv<'a> {
    /// Override current UTC timestamp in Unix seconds.
    pub now_secs: Option<f64>,
    /// Override Google OAuth/Antigravity HTTP transport.
    pub google: Option<&'a dyn GoogleTransport>,
    /// Override OpenCode console HTTP transport.
    pub wallet: Option<&'a dyn WalletTransport>,
    /// Override POSIX wallet support check.
    pub wallet_supported: Option<bool>,
    /// Optional hook run immediately after provider collection and before the post-collection
    /// local inventory race check (for deterministic race/rotation tests).
    pub post_collect_hook: Option<&'a (dyn Fn() + Send + Sync)>,
}

/// Stateful adapter entrypoint aligned with `failover::Adapter`.
#[derive(Debug, Default)]
pub struct SpecialAdapter;

impl SpecialAdapter {
    /// Create a new special-provider failover adapter.
    pub fn new() -> Self {
        Self
    }

    /// Run read-only local inventory and return the dispatcher's inventory object.
    #[allow(clippy::result_unit_err)]
    pub fn inventory_for(
        &mut self,
        provider: &str,
        platform: &str,
        home: &Path,
        ccs: &Path,
    ) -> Result<Value, ()> {
        Ok(inventory(provider, home, platform, Some(ccs)).into_inventory_value())
    }

    /// Collect one previously inventoried private candidate.
    pub async fn collect_for(
        &mut self,
        candidate: &Value,
        platform: &str,
        home: &Path,
        ccs: &Path,
    ) -> Value {
        collect(candidate, home, platform, Some(ccs)).await
    }
}

fn parse_special_provider(provider: &str) -> Option<ProviderId> {
    match ProviderId::parse(provider)? {
        id @ (ProviderId::Antigravity | ProviderId::OpencodeWallet) => Some(id),
        _ => None,
    }
}

/// Whether `failover_special` owns this provider id.
pub fn handles(provider: &str) -> bool {
    parse_special_provider(provider).is_some()
}

/// Whether `url` is one of the six fixed Google URLs allowed by `failover_special.HttpClient`.
pub fn allowed_google_url(url: &str) -> bool {
    url == USERINFO_URL
        || url == TOKEN_URL
        || url
            .strip_prefix(AGY_BASE)
            .is_some_and(|s| matches!(s, "loadCodeAssist" | "retrieveUserQuotaSummary"))
        || url
            .strip_prefix(AGY_CLI_BASE)
            .is_some_and(|s| matches!(s, "loadCodeAssist" | "retrieveUserQuotaSummary"))
}

/// Parse `Retry-After` using `failover_special._retry_after` rules (`1..=86400`, default `60`).
pub fn retry_after_seconds(value: Option<&str>, now_secs: f64) -> u64 {
    let Some(raw) = value else {
        return 60;
    };
    if raw.len() > 128 {
        return 60;
    }
    let trimmed = raw.trim();
    if DIGITS_1_12_RE.is_match(trimmed) {
        return trimmed
            .parse::<u64>()
            .map(|n| n.clamp(1, 86_400))
            .unwrap_or(60);
    }
    let Ok(instant) = DateTime::parse_from_rfc2822(trimmed) else {
        return 60;
    };
    let delta = instant.timestamp() as f64 - now_secs;
    if !delta.is_finite() {
        return 60;
    }
    let secs = delta.ceil();
    if secs <= 1.0 {
        1
    } else if secs >= 86_400.0 {
        86_400
    } else {
        secs as u64
    }
}

/// Production reqwest-backed transport for `failover_special.HttpClient`.
struct ReqwestGoogleTransport {
    now_secs: Option<f64>,
    client: Option<reqwest::Client>,
}

impl ReqwestGoogleTransport {
    fn new(now_secs: Option<f64>) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        Self { now_secs, client }
    }
}

fn encode_form_body(body: &Value) -> Result<String, GoogleHttpError> {
    let Some(obj) = body.as_object() else {
        return Err(GoogleHttpError::Error(
            "The account usage request could not be completed.",
        ));
    };
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in obj {
        let text = match v {
            Value::String(s) => s.clone(),
            Value::Number(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            _ => {
                return Err(GoogleHttpError::Error(
                    "The account usage request could not be completed.",
                ));
            }
        };
        serializer.append_pair(k, &text);
    }
    Ok(serializer.finish())
}

impl GoogleTransport for ReqwestGoogleTransport {
    fn request_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a Value>,
        form: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GoogleHttpError>> + Send + 'a>> {
        Box::pin(async move {
            if !allowed_google_url(url) {
                return Err(GoogleHttpError::Error(
                    "The account request is unavailable.",
                ));
            }
            let Some(ref client) = self.client else {
                return Err(GoogleHttpError::Error(
                    "The account usage request could not be completed.",
                ));
            };
            let mut has_ua = false;
            let mut req = if body.is_some() {
                client.post(url)
            } else {
                client.get(url)
            };
            req = req.timeout(Duration::from_secs(HTTP_TIMEOUT_SECS));
            for (k, v) in headers {
                if k.eq_ignore_ascii_case("user-agent") {
                    has_ua = true;
                }
                req = req.header(k.as_str(), v.as_str());
            }
            req = req.header("Accept", "application/json");
            if !has_ua {
                req = req.header("User-Agent", "CCS-Account-Usage/1.0");
            }
            if let Some(payload) = body {
                if form {
                    let encoded = encode_form_body(payload)?;
                    req = req
                        .header("Content-Type", "application/x-www-form-urlencoded")
                        .body(encoded);
                } else {
                    let encoded = serde_json::to_string(payload).map_err(|_| {
                        GoogleHttpError::Error("The account usage request could not be completed.")
                    })?;
                    req = req.header("Content-Type", "application/json").body(encoded);
                }
            }
            let mut resp = req.send().await.map_err(|_| {
                GoogleHttpError::Error("The account usage request could not be completed.")
            })?;
            let status = resp.status().as_u16();
            if status == 429 {
                // Never read 429 error bodies; propagate Retry-After immediately.
                let hdr = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok());
                return Err(GoogleHttpError::RateLimited(retry_after_seconds(
                    hdr,
                    now_unix_secs(self.now_secs),
                )));
            }
            if status == 401 || status == 403 {
                return Err(GoogleHttpError::NeedsSignIn(
                    "The saved Antigravity account needs a renewed sign-in.",
                ));
            }
            if status == 400 {
                // `desktop_helpers.is_invalid_grant`: read at most 4096 bytes.
                let mut err_body = Vec::new();
                while let Ok(Some(chunk)) = resp.chunk().await {
                    let remaining = 4096usize.saturating_sub(err_body.len());
                    if remaining == 0 {
                        break;
                    }
                    let take = chunk.len().min(remaining);
                    err_body.extend_from_slice(&chunk[..take]);
                    if err_body.len() >= 4096 {
                        break;
                    }
                }
                if let Ok(parsed) = serde_json::from_slice::<Value>(&err_body)
                    && parsed.get("error").and_then(Value::as_str) == Some("invalid_grant")
                {
                    return Err(GoogleHttpError::NeedsSignIn(
                        "The saved Antigravity account needs a renewed sign-in.",
                    ));
                }
                return Err(GoogleHttpError::Error(
                    "The account usage service is temporarily unavailable.",
                ));
            }
            if !(200..300).contains(&status) {
                return Err(GoogleHttpError::Error(
                    "The account usage service is temporarily unavailable.",
                ));
            }
            let mut raw = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| {
                GoogleHttpError::Error("The account usage request could not be completed.")
            })? {
                let take = chunk
                    .len()
                    .min((MAX_RESPONSE_BYTES + 1).saturating_sub(raw.len()));
                raw.extend_from_slice(&chunk[..take]);
                if raw.len() > MAX_RESPONSE_BYTES {
                    return Err(GoogleHttpError::Error(
                        "The account response exceeded its size limit.",
                    ));
                }
            }
            decode_strict_json_object(&raw).ok_or(GoogleHttpError::Error(
                "The account usage request could not be completed.",
            ))
        })
    }
}

enum StrictErr {
    Missing,
    Failed,
}

fn is_linked(meta: &fs::Metadata) -> bool {
    if meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    false
}

fn make_absolute(path: &Path) -> Result<PathBuf, StrictErr> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .map_err(|_| StrictErr::Failed)
    }
}

/// Fixed own-host paths only; never follow links, junctions, or a foreign owner (`failover_special._path`).
fn strict_path(root: &Path, relative: &Path) -> Result<PathBuf, StrictErr> {
    if relative.is_absolute() {
        return Err(StrictErr::Failed);
    }
    let mut parts = Vec::new();
    for comp in relative.components() {
        match comp {
            Component::Normal(p) => parts.push(p),
            Component::CurDir => {}
            _ => return Err(StrictErr::Failed),
        }
    }
    let abs_root = make_absolute(root)?;
    let mut ancestors: Vec<&Path> = abs_root.ancestors().collect();
    ancestors.reverse();
    for current in ancestors {
        if current.as_os_str().is_empty() {
            continue;
        }
        let info = fs::symlink_metadata(current).map_err(|e| {
            if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) {
                StrictErr::Missing
            } else {
                StrictErr::Failed
            }
        })?;
        if is_linked(&info) || !info.is_dir() {
            return Err(StrictErr::Failed);
        }
    }
    let mut current = abs_root;
    let total = parts.len();
    for (index, part) in parts.into_iter().enumerate() {
        current.push(part);
        let info = fs::symlink_metadata(&current).map_err(|e| {
            if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) {
                StrictErr::Missing
            } else {
                StrictErr::Failed
            }
        })?;
        if is_linked(&info) || (index + 1 < total && !info.is_dir()) {
            return Err(StrictErr::Failed);
        }
        if aac_files::path::foreign(&info) {
            return Err(StrictErr::Failed);
        }
    }
    Ok(current)
}

/// Hardened private file read matching `failover_special._read` through `aac_files::read::read`.
fn strict_read(root: &Path, relative: &Path, maximum: usize) -> Result<Vec<u8>, StrictErr> {
    let abs_root = make_absolute(root)?;
    let file = strict_path(&abs_root, relative)?;
    let before = fs::symlink_metadata(&file).map_err(|e| {
        if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) {
            StrictErr::Missing
        } else {
            StrictErr::Failed
        }
    })?;
    if !before.is_file() || before.len() > maximum as u64 {
        return Err(StrictErr::Failed);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.mode() & 0o077 != 0 {
            return Err(StrictErr::Failed);
        }
    }
    let policy = ReadPolicy::capped(maximum as u64)
        .ancestors(Ancestors::NoLinkBelow(&abs_root))
        .owner(Owner::Mine)
        .mode(Mode::Private);
    let (raw, opened) = match aac_files::read::read(&file, &policy) {
        Read::Bytes(bytes, meta) => (bytes, meta),
        Read::Absent | Read::Unreadable(ErrorKind::NotADirectory) => {
            return Err(StrictErr::Missing);
        }
        _ => return Err(StrictErr::Failed),
    };
    if raw.len() > maximum {
        return Err(StrictErr::Failed);
    }
    let after = fs::symlink_metadata(&file).map_err(|_| StrictErr::Failed)?;
    if is_linked(&after) {
        return Err(StrictErr::Failed);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (opened.dev(), opened.ino()) != (before.dev(), before.ino()) {
            return Err(StrictErr::Failed);
        }
        if (
            after.dev(),
            after.ino(),
            after.mtime(),
            after.mtime_nsec(),
            after.size(),
        ) != (
            opened.dev(),
            opened.ino(),
            opened.mtime(),
            opened.mtime_nsec(),
            opened.size(),
        ) {
            return Err(StrictErr::Failed);
        }
    }
    #[cfg(not(unix))]
    {
        if after.len() != opened.len() || after.modified().ok() != opened.modified().ok() {
            return Err(StrictErr::Failed);
        }
    }
    Ok(raw)
}

fn agy_identity(
    email_val: Option<&Value>,
    subject_val: Option<&Value>,
) -> Option<(String, String)> {
    let addr = email_val.and_then(desktop_email)?;
    let subject = subject_val
        .and_then(Value::as_str)
        .filter(|s| SUBJECT_RE.is_match(s))?;
    let lower = addr.to_lowercase();
    let packed = serde_json::to_string(&json!(["antigravity", &lower, subject])).ok()?;
    Some((lower, sha256_hex(packed.as_bytes())))
}

fn agy_hint(value: &Value) -> Option<(String, String)> {
    let id_token = value
        .get("id_token")
        .or_else(|| value.get("idToken"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let claims = jwt_claims(id_token);
    let email_val = claims
        .get("email")
        .filter(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty()))
        .or_else(|| value.get("email"));
    let sub_val = claims
        .get("sub")
        .filter(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty()))
        .or_else(|| {
            value
                .get("subject")
                .filter(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty()))
        })
        .or_else(|| value.get("sub"));
    agy_identity(email_val, sub_val)
}

fn agy_native(value: &Value) -> Option<Credential> {
    if value.get("auth_method").and_then(Value::as_str) != Some("consumer") {
        return None;
    }
    let token = value.get("token")?.as_object()?;
    let access = token.get("access_token").and_then(desktop_secret)?;
    let expires = token
        .get("expiry")
        .and_then(reset_at)
        .and_then(|iso| parse_iso_utc(&iso))
        .map(|dt| (dt.timestamp() as f64) * 1000.0);
    let hint = agy_hint(value);
    Some(Credential {
        access,
        source: "Antigravity CLI".to_owned(),
        refresh: token
            .get("refresh_token")
            .and_then(desktop_secret)
            .unwrap_or_default(),
        expires,
        email: hint.map(|(e, _)| e),
        project: None,
        plan: None,
        google_client: Some("consumer".to_owned()),
    })
}

fn agy_candidate(
    location: &str,
    selectors: Vec<String>,
    identity: (String, String),
    revision: String,
    credential: Credential,
    plan: Option<&Value>,
) -> Value {
    let (email, key) = identity;
    json!({
        "locator": sha256_hex(format!("antigravity:{location}").as_bytes()),
        "selectors": selectors,
        "identity": {
            "kind": IDENTITY_KIND,
            "key": key
        },
        "revision": revision,
        "email": email,
        "plan": plan.and_then(|v| safe_text(v, 80)),
        "_provider": "antigravity",
        "_credential": credential.to_private_value()
    })
}

fn saved_profiles(ccs_dir: &Path, tracker: &mut ReadTracker) -> Vec<Value> {
    let directory = match strict_path(ccs_dir, Path::new("antigravity-profiles")) {
        Ok(d) => d,
        Err(StrictErr::Missing) => return Vec::new(),
        Err(StrictErr::Failed) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let read_dir = match fs::read_dir(&directory) {
        Ok(rd) => rd,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Vec::new();
        }
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut names = Vec::new();
    for entry in read_dir {
        let Ok(entry) = entry else {
            tracker.record_failure();
            return Vec::new();
        };
        if let Some(name) = entry.file_name().to_str()
            && REGISTRY_RE.is_match(name)
        {
            names.push(name.to_owned());
        }
    }
    names.sort();
    let Some(latest) = names.last() else {
        return Vec::new();
    };
    let expected_rev = REGISTRY_RE
        .captures(latest)
        .and_then(|c| c.get(1))
        .and_then(|m| m.as_str().parse::<i64>().ok());
    let raw = match strict_read(
        ccs_dir,
        &Path::new("antigravity-profiles").join(latest),
        MAX_BYTES,
    ) {
        Ok(bytes) => bytes,
        Err(StrictErr::Missing) => return Vec::new(),
        Err(StrictErr::Failed) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let Some(value) = decode_strict_json_object(&raw) else {
        tracker.record_failure();
        return Vec::new();
    };
    let version_ok = value.get("version").and_then(Value::as_i64) == Some(1);
    let rev_ok =
        expected_rev.is_some() && value.get("revision").and_then(Value::as_i64) == expected_rev;
    let Some(profiles) = value
        .get("profiles")
        .and_then(Value::as_array)
        .filter(|arr| version_ok && rev_ok && arr.len() <= 16)
    else {
        tracker.record_failure();
        return Vec::new();
    };

    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for profile in profiles {
        let Some(p_obj) = profile.as_object() else {
            tracker.record_failure();
            return Vec::new();
        };
        let Some(name) = p_obj
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| PROFILE_RE.is_match(s))
        else {
            tracker.record_failure();
            return Vec::new();
        };
        if !seen.insert(name.to_owned()) {
            tracker.record_failure();
            return Vec::new();
        }
        let Some(saved) = p_obj.get("credential").and_then(Value::as_object) else {
            tracker.record_failure();
            return Vec::new();
        };
        let Some(verified) = saved.get("identity").and_then(Value::as_object) else {
            continue;
        };
        if !matches!(
            verified.get("source").and_then(Value::as_str),
            Some("provider-userinfo" | "native-runtime")
        ) {
            continue;
        }
        let Some(identity) = agy_identity(verified.get("email"), verified.get("subject")) else {
            continue;
        };
        let Some(revision) = saved
            .get("fingerprint")
            .and_then(Value::as_str)
            .filter(|s| HASH_RE.is_match(s))
        else {
            continue;
        };
        let email_ok = p_obj.get("email").and_then(desktop_email).is_some()
            && p_obj
                .get("email")
                .and_then(Value::as_str)
                .is_some_and(|s| s.to_lowercase() == identity.0);
        let verified_at_ok = verified
            .get("verifiedAt")
            .filter(|v| v.is_string())
            .and_then(reset_at)
            .is_some();
        if p_obj.get("identityKey").and_then(Value::as_str) != Some(identity.1.as_str())
            || !email_ok
            || !verified_at_ok
        {
            continue;
        }
        let sel = format!("antigravity-profile:{name}");
        let rel = Path::new("antigravity-instances")
            .join(name)
            .join("ubuntu")
            .join(format!("credential-{revision}.bin"));
        tracker.attribute(std::slice::from_ref(&sel), |t| {
            match strict_read(ccs_dir, &rel, 16_384) {
                Ok(cred_raw) => match decode_strict_json_object(&cred_raw) {
                    Some(decoded) => {
                        if let Some(mut cred) = agy_native(&decoded)
                            && sha256_hex(&cred_raw) == revision
                        {
                            cred.email = Some(identity.0.clone());
                            result.push(agy_candidate(
                                &format!("saved:{name}"),
                                vec![sel.clone()],
                                identity.clone(),
                                revision.to_owned(),
                                cred,
                                p_obj.get("plan"),
                            ));
                        }
                    }
                    None => t.record_failure(),
                },
                Err(StrictErr::Missing) => {}
                Err(StrictErr::Failed) => t.record_failure(),
            }
        });
    }
    result
}

fn discover_antigravity(home: &Path, tracker: &mut ReadTracker) -> Vec<Value> {
    let mut result = Vec::new();
    match strict_read(
        home,
        Path::new(".gemini/antigravity-cli/antigravity-oauth-token"),
        16_384,
    ) {
        Ok(raw) => match decode_strict_json_object(&raw) {
            Some(value) => {
                if let (Some(cred), Some(ident)) = (agy_native(&value), agy_hint(&value)) {
                    result.push(agy_candidate(
                        "native",
                        vec!["discover".to_owned()],
                        ident,
                        sha256_hex(&raw),
                        cred,
                        None,
                    ));
                }
            }
            None => tracker.record_failure(),
        },
        Err(StrictErr::Missing) => {}
        Err(StrictErr::Failed) => tracker.record_failure(),
    }

    match strict_path(home, Path::new(".omp/agent/agent.db")) {
        Ok(db_path) => {
            if !fs::symlink_metadata(&db_path).is_ok_and(|m| m.is_file()) {
                return result;
            }
            for row in sqlite::omp_antigravity_rows(&db_path, tracker) {
                match row {
                    OmpRawRow::Unusable => {
                        tracker.record_failure();
                    }
                    OmpRawRow::Valid { row_id, raw } => {
                        let Some(value) = decode_strict_json_object(raw.as_bytes()) else {
                            tracker.record_failure();
                            continue;
                        };
                        if let (Some(ident), Some(cred)) =
                            (agy_hint(&value), credential_from_omp("antigravity", &value))
                        {
                            result.push(agy_candidate(
                                &format!("omp:{row_id}"),
                                vec!["discover".to_owned()],
                                ident,
                                sha256_hex(raw.as_bytes()),
                                cred,
                                None,
                            ));
                        }
                    }
                }
            }
        }
        Err(StrictErr::Missing) => {}
        Err(StrictErr::Failed) => tracker.record_failure(),
    }
    result
}

fn check_private_dir_no_create(dir: &Path) -> Result<(), ()> {
    let meta = fs::symlink_metadata(dir).map_err(|_| ())?;
    if is_linked(&meta) || !meta.is_dir() || aac_files::path::foreign(&meta) {
        return Err(());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o077 != 0 {
            return Err(());
        }
    }
    Ok(())
}

fn discover_wallet(
    ccs_dir: &Path,
    platform: &str,
    env: &SpecialEnv<'_>,
    tracker: &mut ReadTracker,
) -> Vec<Value> {
    let supported = env
        .wallet_supported
        .unwrap_or(cfg!(unix) && platform != "windows");
    if !supported {
        return Vec::new();
    }
    let relative = Path::new("account-usage").join(wallet::CAPSULE_NAME);
    let read_attempt = || -> Result<Value, ()> {
        let raw = strict_read(ccs_dir, &relative, MAX_BYTES).map_err(|_| ())?;
        let root = strict_path(ccs_dir, Path::new("account-usage")).map_err(|_| ())?;
        check_private_dir_no_create(&root)?;
        let (cookies, workspace) =
            wallet::parse_capsule_bytes(&raw, now_unix_secs(env.now_secs)).map_err(|_| ())?;
        let second = strict_read(ccs_dir, &relative, MAX_BYTES).map_err(|_| ())?;
        if second != raw {
            return Err(());
        }
        Ok(json!({
            "locator": sha256_hex(b"opencode-wallet:browser-capsule"),
            "selectors": ["wallet"],
            "identity": {
                "kind": WALLET_KIND,
                "key": sha256_hex(workspace.as_bytes())
            },
            "revision": sha256_hex(&raw),
            "email": null,
            "plan": null,
            "_provider": "opencode-wallet",
            "_cookies": cookies,
            "_workspace": workspace
        }))
    };

    match read_attempt() {
        Ok(candidate) => vec![candidate],
        Err(()) => {
            let capsule_path = ccs_dir.join("account-usage").join(wallet::CAPSULE_NAME);
            match fs::symlink_metadata(&capsule_path) {
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                    Vec::new()
                }
                _ => {
                    tracker.record_failure();
                    Vec::new()
                }
            }
        }
    }
}

/// Read-only local credential inventory for Antigravity and OpenCode console wallet.
pub fn inventory(
    provider: &str,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
) -> InventoryResult {
    inventory_with(provider, home, platform, ccs_dir, &SpecialEnv::default())
}

/// Read-only local credential inventory with explicit test/clock overrides.
pub fn inventory_with(
    provider: &str,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
    env: &SpecialEnv<'_>,
) -> InventoryResult {
    let Some(provider_id) = parse_special_provider(provider) else {
        return InventoryResult::empty();
    };
    if platform_label(platform).is_none() {
        return InventoryResult::empty();
    }
    let ccs = ccs_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".ccs"));
    let mut tracker = ReadTracker::default();
    let candidates = match provider_id {
        ProviderId::Antigravity => {
            let mut out = tracker.attribute(&["antigravity-profile:*".to_owned()], |t| {
                saved_profiles(&ccs, t)
            });
            let disc =
                tracker.attribute(&["discover".to_owned()], |t| discover_antigravity(home, t));
            out.extend(disc);
            out
        }
        ProviderId::OpencodeWallet => tracker.attribute(&["wallet".to_owned()], |t| {
            discover_wallet(&ccs, platform, env, t)
        }),
        _ => Vec::new(),
    };
    InventoryResult {
        candidates,
        incomplete_selectors: tracker.incomplete_selectors(),
        read_failures: tracker.total_failures(),
    }
}

async fn verify_google_identity(
    credential: &Credential,
    client: &dyn GoogleTransport,
) -> Result<Option<(String, String)>, GoogleHttpError> {
    let headers = vec![(
        "Authorization".to_owned(),
        format!("Bearer {}", credential.access),
    )];
    let user = client
        .request_json(USERINFO_URL, &headers, None, false)
        .await?;
    let verified = user
        .get("verified_email")
        .or_else(|| user.get("email_verified"))
        .and_then(Value::as_bool);
    if verified != Some(true) {
        return Ok(None);
    }
    let sub = user
        .get("id")
        .filter(|v| !v.is_null() && v.as_str().is_none_or(|s| !s.is_empty()))
        .or_else(|| user.get("sub"));
    Ok(agy_identity(user.get("email"), sub))
}

async fn refresh_google_token(
    credential: &Credential,
    home: &Path,
    client: &dyn GoogleTransport,
) -> Result<Credential, GoogleHttpError> {
    if credential.refresh.is_empty() {
        return Err(GoogleHttpError::NeedsSignIn(
            "The saved Antigravity account needs a renewed sign-in.",
        ));
    }
    let (client_id, client_secret) = antigravity_oauth_client(home).map_err(|_| {
        GoogleHttpError::Error(
            "Antigravity's private app OAuth metadata is unavailable on this computer.",
        )
    })?;
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": credential.refresh,
        "client_id": client_id,
        "client_secret": client_secret
    });
    let resp = client
        .request_json(TOKEN_URL, &[], Some(&body), true)
        .await?;
    let Some(access) = resp.get("access_token").and_then(desktop_secret) else {
        return Err(GoogleHttpError::NeedsSignIn(
            "The saved Antigravity session could not be renewed.",
        ));
    };
    Ok(Credential {
        access,
        source: credential.source.clone(),
        refresh: credential.refresh.clone(),
        expires: None,
        email: credential.email.clone(),
        project: credential.project.clone(),
        plan: credential.plan.clone(),
        google_client: credential.google_client.clone(),
    })
}

async fn antigravity_load(
    client: &dyn GoogleTransport,
    headers: &[(String, String)],
) -> Result<Value, GoogleHttpError> {
    let body = json!({
        "metadata": {
            "ideType": "ANTIGRAVITY",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });
    match client
        .request_json(
            &format!("{AGY_BASE}loadCodeAssist"),
            headers,
            Some(&body),
            false,
        )
        .await
    {
        Ok(v) => Ok(v),
        Err(err @ GoogleHttpError::NeedsSignIn(_)) => Err(err),
        Err(_) => {
            client
                .request_json(
                    &format!("{AGY_CLI_BASE}loadCodeAssist"),
                    headers,
                    Some(&body),
                    false,
                )
                .await
        }
    }
}

struct ServiceResult {
    service: &'static str,
    status: &'static str,
    windows: Vec<Value>,
    error: Option<GoogleHttpError>,
}

async fn antigravity_services_summary_only(
    client: &dyn GoogleTransport,
    headers: &[(String, String)],
    project: &str,
) -> Result<Vec<ServiceResult>, GoogleHttpError> {
    let services: [(&'static str, &'static str); 2] = [("cli", AGY_CLI_BASE), ("app", AGY_BASE)];
    let body = json!({ "project": project });
    let mut results: Vec<ServiceResult> = Vec::with_capacity(2);
    let mut failures: Vec<GoogleHttpError> = Vec::new();

    for (service, base) in services {
        let url = format!("{base}retrieveUserQuotaSummary");
        match client.request_json(&url, headers, Some(&body), false).await {
            Ok(summary) => {
                let _groups = agy_groups(&summary);
                let windows = normalize_antigravity(&summary);
                results.push(ServiceResult {
                    service,
                    status: "ok",
                    windows,
                    error: None,
                });
            }
            Err(err) => {
                let is_rate_limited = matches!(err, GoogleHttpError::RateLimited(_));
                failures.push(err.clone());
                if is_rate_limited {
                    if !results.iter().any(|r| r.status == "ok") {
                        return Err(err);
                    }
                    results.push(ServiceResult {
                        service,
                        status: "rate_limited",
                        windows: Vec::new(),
                        error: Some(err),
                    });
                    break;
                }
                results.push(ServiceResult {
                    service,
                    status: "unavailable",
                    windows: Vec::new(),
                    error: Some(err),
                });
            }
        }
    }

    let first_answered = results.iter().find(|r| r.status == "ok");
    let Some(answered) = first_answered else {
        if !failures.is_empty()
            && failures
                .iter()
                .all(|e| matches!(e, GoogleHttpError::NeedsSignIn(_)))
        {
            return Err(failures
                .into_iter()
                .next()
                .unwrap_or(GoogleHttpError::NeedsSignIn(
                    "The saved Antigravity account needs a renewed sign-in.",
                )));
        }
        return Err(failures
            .into_iter()
            .last()
            .unwrap_or(GoogleHttpError::Error(
                "There was no time left to read Antigravity's quota.",
            )));
    };
    let known = answered.windows.clone();
    for res in &mut results {
        if res.status != "ok" {
            res.windows = antigravity_unavailable_windows(&known);
        }
    }
    Ok(results)
}

async fn collect_antigravity_quota(
    credential: &Credential,
    sample: &mut Value,
    client: &dyn GoogleTransport,
) -> Result<(), GoogleHttpError> {
    let headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credential.access),
        ),
        ("User-Agent".to_owned(), AGY_USER_AGENT.to_owned()),
    ];
    let load = antigravity_load(client, &headers).await?;
    let project_raw = match load.get("cloudaicompanionProject") {
        Some(Value::Object(obj)) => obj.get("id").or_else(|| obj.get("projectId")),
        other => other,
    };
    let project = project_raw
        .and_then(|v| safe_text(v, 160))
        .or_else(|| credential.project.clone())
        .ok_or(GoogleHttpError::Error(
            "The signed-in account did not return an Antigravity quota project.",
        ))?;
    let (plan, reported) = antigravity_reported_plan(&load);
    sample["plan"] = json!(plan);
    if let Some(rep) = reported {
        sample["reportedPlan"] = rep;
    }
    let services = antigravity_services_summary_only(client, &headers, &project).await?;
    let mut windows = Vec::new();
    for svc in services {
        if svc.status == "rate_limited" {
            return Err(svc.error.unwrap_or(GoogleHttpError::RateLimited(60)));
        }
        windows.extend(antigravity_service_label(svc.windows, svc.service));
    }
    windows.extend(normalize_antigravity_credits(&load));
    sample["windows"] = Value::Array(windows);
    Ok(())
}

async fn collect_antigravity(
    candidate: &Value,
    home: &Path,
    platform: &str,
    env: &SpecialEnv<'_>,
) -> Result<Value, GoogleHttpError> {
    let Some(mut credential) = candidate
        .get("_credential")
        .and_then(Credential::from_private_value)
    else {
        return Ok(json!({ "sample": null, "failureCode": "unavailable" }));
    };
    let default_client;
    let client: &dyn GoogleTransport = match env.google {
        Some(c) => c,
        None => {
            default_client = ReqwestGoogleTransport::new(env.now_secs);
            &default_client
        }
    };
    let now_ms = now_unix_secs(env.now_secs) * 1000.0;
    let mut refreshed = false;
    if let Some(exp) = credential.expires
        && exp <= now_ms + 30_000.0
    {
        credential = refresh_google_token(&credential, home, client).await?;
        refreshed = true;
    }
    let identity = match verify_google_identity(&credential, client).await {
        Ok(id) => id,
        Err(GoogleHttpError::NeedsSignIn(_)) if !refreshed && !credential.refresh.is_empty() => {
            credential = refresh_google_token(&credential, home, client).await?;
            verify_google_identity(&credential, client).await?
        }
        Err(err) => return Err(err),
    };
    let Some((verified_email, verified_key)) = identity else {
        return Ok(json!({ "sample": null, "failureCode": "identity_mismatch" }));
    };
    let expected_identity = json!({ "kind": IDENTITY_KIND, "key": verified_key });
    if candidate.get("email").and_then(Value::as_str) != Some(verified_email.as_str())
        || candidate.get("identity") != Some(&expected_identity)
    {
        return Ok(json!({ "sample": null, "failureCode": "identity_mismatch" }));
    }
    let host_label = platform_label(platform).unwrap_or("Ubuntu");
    let mut sample = account("antigravity", platform);
    sample["email"] = json!(verified_email);
    sample["label"] = json!(verified_email);
    sample["plan"] = candidate.get("plan").cloned().unwrap_or(Value::Null);
    sample["status"] = json!("ok");
    sample["message"] = Value::Null;
    sample["source"] = json!(format!("Verified Antigravity account on {host_label}"));
    collect_antigravity_quota(&credential, &mut sample, client).await?;
    if sample
        .get("windows")
        .and_then(Value::as_array)
        .is_none_or(Vec::is_empty)
    {
        return Ok(json!({ "sample": null, "failureCode": "unavailable" }));
    }
    let stamped = utc_now_iso_secs(env.now_secs);
    sample["fetchedAt"] = json!(stamped);
    sample["sampledAt"] = json!(stamped);
    Ok(json!({ "sample": sample, "failureCode": null }))
}

async fn collect_wallet(
    candidate: &Value,
    platform: &str,
    env: &SpecialEnv<'_>,
) -> Result<Value, WalletError> {
    let cookies_val = candidate.get("_cookies").unwrap_or(&Value::Null);
    let requested = candidate.get("_workspace").and_then(Value::as_str);
    let default_client;
    let client: &dyn WalletTransport = match env.wallet {
        Some(c) => c,
        None => {
            let valid = wallet::validate_cookies(cookies_val, now_unix_secs(env.now_secs))?;
            default_client = wallet::ReqwestWalletTransport::new(&valid, env.now_secs);
            &default_client
        }
    };
    let (workspace, raw_sample) =
        wallet::collect(cookies_val, requested, client, env.now_secs).await?;
    let expected_identity = json!({
        "kind": WALLET_KIND,
        "key": sha256_hex(workspace.as_bytes())
    });
    let expected_id = format!(
        "plan-opencode-go-console-mac-{}",
        &sha256_hex(workspace.as_bytes())[..12]
    );
    if Some(workspace.as_str()) != requested
        || candidate.get("identity") != Some(&expected_identity)
        || raw_sample.get("id").and_then(Value::as_str) != Some(expected_id.as_str())
    {
        return Ok(json!({ "sample": null, "failureCode": "identity_mismatch" }));
    }
    let failure = raw_sample.get("_usageFailure").cloned();
    let mut sample = wallet::public_sample(&raw_sample);
    let host_label = platform_label(platform).unwrap_or("Ubuntu");
    sample["platform"] = json!(platform);
    sample["source"] = json!(format!(
        "Authenticated OpenCode console workspace on {host_label}"
    ));
    let mut result = json!({ "sample": sample, "failureCode": null });
    if let Some(f) = failure.as_ref().and_then(Value::as_object)
        && f.get("code").and_then(Value::as_str) == Some("rate_limited")
    {
        result["failureCode"] = json!("rate_limited");
        if let Some(retry) = f
            .get("retryAfterSeconds")
            .and_then(Value::as_f64)
            .filter(|r| r.is_finite())
        {
            result["retryAfterSeconds"] = json!(retry.clamp(0.0, 86_400.0));
        }
    }
    Ok(result)
}

/// Collect usage for one freshly inventoried private candidate.
pub async fn collect(
    candidate: &Value,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
) -> Value {
    collect_with(candidate, home, platform, ccs_dir, &SpecialEnv::default()).await
}

/// Collect usage for one private candidate with explicit test/transport overrides.
pub async fn collect_with(
    candidate: &Value,
    home: &Path,
    platform: &str,
    ccs_dir: Option<&Path>,
    env: &SpecialEnv<'_>,
) -> Value {
    let Some(provider_str) = candidate.get("_provider").and_then(Value::as_str) else {
        return json!({ "sample": null, "failureCode": "unavailable" });
    };
    let Some(provider_id) = parse_special_provider(provider_str) else {
        return json!({ "sample": null, "failureCode": "unavailable" });
    };
    if platform_label(platform).is_none() {
        return json!({ "sample": null, "failureCode": "unavailable" });
    }
    let result = match provider_id {
        ProviderId::Antigravity => {
            match collect_antigravity(candidate, home, platform, env).await {
                Ok(v) => v,
                Err(GoogleHttpError::RateLimited(retry)) => {
                    return json!({
                        "sample": null,
                        "failureCode": "rate_limited",
                        "retryAfterSeconds": retry
                    });
                }
                Err(GoogleHttpError::NeedsSignIn(_)) => {
                    return json!({ "sample": null, "failureCode": "needs_sign_in" });
                }
                Err(GoogleHttpError::Error(_)) => {
                    return json!({ "sample": null, "failureCode": "provider_error" });
                }
            }
        }
        ProviderId::OpencodeWallet => match collect_wallet(candidate, platform, env).await {
            Ok(v) => v,
            Err(err) => {
                if err.code == "rate_limited" {
                    let mut out = json!({ "sample": null, "failureCode": "rate_limited" });
                    if let Some(retry) = err.retry_after_seconds.filter(|r| r.is_finite()) {
                        out["retryAfterSeconds"] = json!(retry.clamp(0.0, 86_400.0));
                    }
                    return out;
                }
                let failure = match err.code {
                    "needs_sign_in" | "no_browser_cookie" => "needs_sign_in",
                    "workspace_mismatch" => "identity_mismatch",
                    _ => "provider_error",
                };
                return json!({ "sample": null, "failureCode": failure });
            }
        },
        _ => return json!({ "sample": null, "failureCode": "unavailable" }),
    };

    if !result["sample"].is_null() {
        if let Some(hook) = env.post_collect_hook {
            hook();
        }
        let current_inv = inventory_with(provider_str, home, platform, ccs_dir, env);
        let unchanged = current_inv.candidates.iter().any(|current| {
            current.get("locator") == candidate.get("locator")
                && current.get("identity") == candidate.get("identity")
                && current.get("revision") == candidate.get("revision")
        });
        let unreadable = current_inv.read_failures > 0;
        if !unchanged && unreadable {
            let mut failed = json!({ "sample": null, "failureCode": "unavailable" });
            if result.get("failureCode").and_then(Value::as_str) == Some("rate_limited") {
                failed["failureCode"] = json!("rate_limited");
                if let Some(retry) = result
                    .get("retryAfterSeconds")
                    .and_then(Value::as_f64)
                    .filter(|r| r.is_finite())
                {
                    failed["retryAfterSeconds"] = json!(retry.clamp(0.0, 86_400.0));
                }
            }
            return failed;
        }
        if !unchanged {
            let mut failed = json!({ "sample": null, "failureCode": "source_changed" });
            if result.get("failureCode").and_then(Value::as_str) == Some("rate_limited") {
                failed["rateLimited"] = json!(true);
                if let Some(retry) = result
                    .get("retryAfterSeconds")
                    .and_then(Value::as_f64)
                    .filter(|r| r.is_finite())
                {
                    failed["retryAfterSeconds"] = json!(retry.clamp(0.0, 86_400.0));
                }
            }
            return failed;
        }
    }
    result
}
