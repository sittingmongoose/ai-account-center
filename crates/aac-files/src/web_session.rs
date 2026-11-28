//! Write-only pasted web-session credential storage (`config/private-files.json` row
//! `web-sessions`, class `credential`, 0600 on Unix and DPAPI CurrentUser on Windows).
//!
//! Secrets are validated strictly per surface, written atomically with fsync through
//! [`crate::OwnedStore`], never echoed in error or debug output, and read only through the
//! hardened private-file policy (`0600`, single link, owner-only, no symlinks below `root`).

use crate::{
    read::{self, Ancestors, Mode, Owner, Read, ReadPolicy},
    registry::{self, Owners},
    single_link,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::BTreeMap, fmt, path::Path};

/// Private-file registry row id for pasted web sessions.
pub const ROW_ID: &str = "web-sessions";

/// Hard byte cap for one stored web-session file (matches `config/private-files.json`).
pub const MAX_BYTES: usize = 65_536;

/// Maximum accepted length of a pasted raw Cookie header (matches `plan_usage.py`).
pub const MAX_COOKIE_HEADER_BYTES: usize = 32_768;

/// Maximum accepted length of one extracted session cookie value.
pub const MAX_SINGLE_COOKIE_BYTES: usize = 8_192;

const QWEN_INTL_BASE: &str =
    "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1";
const QWEN_CN_BASE: &str = "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1";

/// Supported no-browser web-session surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Surface {
    /// Qwen Token Plan console (`home.qwencloud.com` / `bailian.console.aliyun.com`).
    Qwen,
    /// OpenCode Zen prepaid console wallet (`opencode.ai`).
    OpencodeWallet,
    /// Muse Code portal quota (`dev.meta.ai`).
    Muse,
}

impl Surface {
    /// Parse a public surface identifier.
    pub fn parse(raw: &str) -> Option<Self> {
        match raw {
            "qwen" => Some(Self::Qwen),
            "opencode-wallet" => Some(Self::OpencodeWallet),
            "muse" => Some(Self::Muse),
            _ => None,
        }
    }

    /// Canonical surface identifier used in URLs, JSON summaries, and disk metadata.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Qwen => "qwen",
            Self::OpencodeWallet => "opencode-wallet",
            Self::Muse => "muse",
        }
    }

    /// Plain filename under `<ccs>/account-usage/sessions/`.
    pub fn file_name(self) -> &'static str {
        match self {
            Self::Qwen => "qwen.json",
            Self::OpencodeWallet => "opencode-wallet.json",
            Self::Muse => "muse.json",
        }
    }

    /// Dashboard provider id that owns this surface in the Accounts UI.
    pub fn provider_id(self) -> &'static str {
        match self {
            Self::Qwen => "qwen",
            Self::OpencodeWallet => "opencode-go",
            Self::Muse => "muse",
        }
    }

    /// Every supported surface in deterministic display order.
    pub fn all() -> [Self; 3] {
        [Self::Qwen, Self::OpencodeWallet, Self::Muse]
    }
}

/// One validated cookie name/value pair. Debug formatting redacts the value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredCookie {
    /// Cookie name (strictly allowlisted per surface).
    pub name: String,
    /// Cookie value (never logged or echoed).
    pub value: String,
}

impl fmt::Debug for StoredCookie {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredCookie")
            .field("name", &self.name)
            .field("value", &"[REDACTED]")
            .finish()
    }
}

/// Secret payload stored inside one credential record. Debug output redacts every secret.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum SessionSecret {
    /// Qwen console cookie header (plus optional distinct gateway cookie and site region).
    Qwen {
        /// Primary console `Cookie` header sent to the Qwen console origin.
        cookie: String,
        /// Optional separate `Cookie` header sent to the cs-data gateway.
        #[serde(
            rename = "gatewayCookie",
            default,
            skip_serializing_if = "Option::is_none"
        )]
        gateway_cookie: Option<String>,
        /// Region selector: `"intl"` or `"cn"`.
        region: String,
    },
    /// OpenCode Zen console allowlisted cookies (`auth` and/or `__Host-console_session`).
    OpencodeWallet {
        /// Validated, allowlisted `opencode.ai` session cookies.
        cookies: Vec<StoredCookie>,
    },
    /// Muse Code portal allowlisted cookies (`llama_dev_sess` and/or `llm_sess`).
    Muse {
        /// Validated, allowlisted `dev.meta.ai` session cookies.
        cookies: Vec<StoredCookie>,
    },
}

impl fmt::Debug for SessionSecret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Qwen { region, .. } => f
                .debug_struct("SessionSecret::Qwen")
                .field("region", region)
                .field("cookie", &"[REDACTED]")
                .finish(),
            Self::OpencodeWallet { cookies } => f
                .debug_struct("SessionSecret::OpencodeWallet")
                .field("cookieCount", &cookies.len())
                .finish(),
            Self::Muse { cookies } => f
                .debug_struct("SessionSecret::Muse")
                .field("cookieCount", &cookies.len())
                .finish(),
        }
    }
}

impl SessionSecret {
    /// Render a `Cookie` header from stored allowlisted cookies or Qwen's primary cookie.
    pub fn cookie_header(&self) -> String {
        match self {
            Self::Qwen { cookie, .. } => cookie.clone(),
            Self::OpencodeWallet { cookies } | Self::Muse { cookies } => cookies
                .iter()
                .map(|c| format!("{}={}", c.name, c.value))
                .collect::<Vec<_>>()
                .join("; "),
        }
    }

    /// Allowlisted cookies for OpenCode or Muse sessions.
    pub fn cookies(&self) -> Option<&[StoredCookie]> {
        match self {
            Self::OpencodeWallet { cookies } | Self::Muse { cookies } => Some(cookies),
            Self::Qwen { .. } => None,
        }
    }

    /// Qwen primary cookie, gateway cookie, and region (`"intl"` or `"cn"`).
    pub fn qwen_parts(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::Qwen {
                cookie,
                gateway_cookie,
                region,
            } => Some((
                cookie.as_str(),
                gateway_cookie.as_deref().unwrap_or(cookie.as_str()),
                region.as_str(),
            )),
            _ => None,
        }
    }
}

/// Non-secret metadata stored alongside a pasted web session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SessionMeta {
    /// Canonical surface id.
    pub surface: String,
    /// Non-reversible SHA-256 fingerprint (`sha256:<16 hex>`).
    pub fingerprint: String,
    /// UTC timestamp when this session was saved.
    pub stored_at: String,
    /// Number of cookie pairs captured.
    pub cookie_count: usize,
    /// Qwen site region (`"intl"` or `"cn"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<String>,
    /// Verified OpenCode workspace id (`wrk_*` or `org_*`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// Verified Muse portal numeric team id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub team_id: Option<String>,
    /// Verified Muse account email.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// Verified Muse subscription tier/plan label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    /// Internal SHA-256 pin of `muse-user:<userId>` (never echoed in public summary).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_hash: Option<String>,
    /// The OpenCode Go account this session was pasted for (`opencode-go:...`). `None`
    /// only on the legacy single-file record, which is still readable but bound to no
    /// account.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

impl SessionMeta {
    /// Safe public JSON projection for HTTP responses (never exposes secrets or internal pins).
    pub fn public_summary(&self) -> Value {
        let mut out = json!({
            "surface": self.surface,
            "fingerprint": self.fingerprint,
            "storedAt": self.stored_at,
            "cookieCount": self.cookie_count,
        });
        if let Some(region) = &self.region {
            out["region"] = json!(region);
        }
        if let Some(workspace_id) = &self.workspace_id {
            out["workspaceId"] = json!(workspace_id);
        }
        if let Some(team_id) = &self.team_id {
            out["teamId"] = json!(team_id);
        }
        if let Some(email) = &self.email {
            out["email"] = json!(email);
        }
        if let Some(plan) = &self.plan {
            out["plan"] = json!(plan);
        }
        if let Some(account_id) = &self.account_id {
            out["accountId"] = json!(account_id);
        }
        out
    }
}

/// Complete stored web session loaded from disk or parsed before saving.
#[derive(Clone, PartialEq, Eq)]
pub struct StoredSession {
    /// Safe non-secret metadata.
    pub meta: SessionMeta,
    /// Redacted secret payload.
    pub secret: SessionSecret,
}

impl fmt::Debug for StoredSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StoredSession")
            .field("meta", &self.meta)
            .field("secret", &"[REDACTED]")
            .finish()
    }
}

/// Optional binding fields accompanying a pasted web session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PasteOptions {
    /// Optional Qwen region (`"intl"` or `"cn"`).
    pub region: Option<String>,
    /// Optional OpenCode workspace id (`wrk_*` / `org_*`).
    pub workspace_id: Option<String>,
    /// Optional Muse portal team id (digits).
    pub team_id: Option<String>,
    /// Optional Muse account email.
    pub email: Option<String>,
    /// Optional Muse plan tier label.
    pub plan: Option<String>,
    /// Optional Muse SHA-256 user pin.
    pub user_hash: Option<String>,
}

/// Safe, secret-free error codes for pasted web-session validation and storage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    /// Input was empty, oversized, malformed, or contained control characters.
    InvalidInput,
    /// Pasted cookie header lacked the required allowlisted session cookie for this surface.
    MissingRequiredCookie,
    /// Input contained a disallowed CLI/API token prefix (`dca:`, `sk-`).
    DisallowedToken,
    /// Optional metadata (region, workspace id, team id, email, plan, user hash) was invalid.
    InvalidMetadata,
    /// The private credential file could not be read, locked, or replaced safely.
    StorageUnavailable,
    /// The on-disk encoding is unsupported on this OS (e.g. Windows DPAPI blob on Unix).
    UnsupportedEncoding,
}

impl fmt::Display for SessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let msg = match self {
            Self::InvalidInput => "pasted web session is invalid",
            Self::MissingRequiredCookie => "pasted web session is missing its required cookie",
            Self::DisallowedToken => "API or device tokens cannot be saved as a web session",
            Self::InvalidMetadata => "web session binding metadata is invalid",
            Self::StorageUnavailable => "web session store could not be read or saved safely",
            Self::UnsupportedEncoding => "stored web session uses an unsupported encoding",
        };
        f.write_str(msg)
    }
}

impl std::error::Error for SessionError {}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct DiskRecord {
    version: u32,
    surface: String,
    fingerprint: String,
    stored_at: String,
    cookie_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    region: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    team_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    user_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    account_id: Option<String>,
    encoding: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    secret: Option<SessionSecret>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ciphertext: Option<String>,
}

/// Validate an OpenCode workspace identifier (`wrk_*` or `org_*`).
pub fn valid_workspace_id(value: &str) -> bool {
    let Some(rest) = value
        .strip_prefix("wrk_")
        .or_else(|| value.strip_prefix("org_"))
    else {
        return false;
    };
    (1..=128).contains(&rest.len())
        && rest
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

/// Validate a Muse portal numeric team identifier.
pub fn valid_team_id(value: &str) -> bool {
    (1..=32).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
}

/// Validate the OpenCode Go account id a pasted wallet session is bound to
/// (`opencode-go:usage`, `opencode-go:acct:<8 hex>`, `opencode-go:usage:<8 hex>`):
/// the surface's provider prefix plus one or two plain selector segments.
pub fn valid_wallet_account_id(value: &str) -> bool {
    let Some(rest) = value.strip_prefix(&format!("{}:", Surface::OpencodeWallet.provider_id()))
    else {
        return false;
    };
    if value.is_empty() || value.len() > 128 {
        return false;
    }
    let segments: Vec<&str> = rest.split(':').collect();
    (1..=2).contains(&segments.len())
        && segments.iter().all(|segment| {
            !segment.is_empty()
                && segment.len() <= 64
                && segment
                    .bytes()
                    .next()
                    .is_some_and(|b| b.is_ascii_alphanumeric())
                && segment
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
        })
        && !rest.contains("..")
}

/// Plain filename under `<ccs>/account-usage/sessions/` for one account's OpenCode
/// wallet session: derived from the account id so no account id ever touches the
/// filesystem as a path.
fn wallet_file_name(account_id: &str) -> String {
    format!(
        "opencode-wallet-{}.json",
        &aac_core::sha256_hex(account_id)[..12]
    )
}

/// The private file a session must be saved to: a new OpenCode wallet session
/// requires its account binding, and Qwen/Muse sessions accept none.
fn session_file_name(surface: Surface, session: &StoredSession) -> Result<String, SessionError> {
    match surface {
        Surface::OpencodeWallet => match session.meta.account_id.as_deref() {
            Some(account_id) if valid_wallet_account_id(account_id) => {
                Ok(wallet_file_name(account_id))
            }
            _ => Err(SessionError::InvalidMetadata),
        },
        Surface::Qwen | Surface::Muse => {
            if session.meta.account_id.is_some() {
                Err(SessionError::InvalidMetadata)
            } else {
                Ok(surface.file_name().to_owned())
            }
        }
    }
}

/// Validate a 64-character lowercase hex SHA-256 digest.
pub fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Validate a plain account email address.
pub fn valid_email(value: &str) -> bool {
    if value.is_empty() || value.len() > 254 || value.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    let Some((host, tld)) = domain.rsplit_once('.') else {
        return false;
    };
    !local.is_empty() && !host.is_empty() && !tld.is_empty() && !domain.contains('@')
}

/// Validate a safe non-secret plan label.
pub fn valid_plan(value: &str) -> bool {
    if value.is_empty() || value.len() > 160 {
        return false;
    }
    if value.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return false;
    }
    let lower = value.to_ascii_lowercase();
    !lower.contains("dca:")
        && !lower.contains("bearer ")
        && !lower.contains("sk-")
        && !value.contains("eyJ")
}

/// Validate a Qwen site region (`"intl"` or `"cn"`).
pub fn valid_qwen_region(value: &str) -> bool {
    matches!(value, "intl" | "cn")
}

fn valid_cookie_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .bytes()
            .all(|b| (0x21..=0x7e).contains(&b) && !b"=;,".contains(&b))
}

fn valid_single_cookie_value(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_SINGLE_COOKIE_BYTES
        && value.bytes().all(|b| b > 0x20 && b != 0x7f && b != b';')
}

fn valid_header_string(raw: &str) -> bool {
    let trimmed = raw.trim();
    !trimmed.is_empty()
        && trimmed.len() <= MAX_COOKIE_HEADER_BYTES
        && !trimmed.bytes().any(|b| matches!(b, 0 | 10 | 13))
}

fn strip_cookie_prefix(raw: &str) -> &str {
    let trimmed = raw.trim();
    if trimmed.len() >= 7 && trimmed[..7].eq_ignore_ascii_case("cookie:") {
        trimmed[7..].trim()
    } else {
        trimmed
    }
}

fn parse_cookie_pairs(header: &str) -> Result<Vec<(String, String)>, SessionError> {
    if !valid_header_string(header) {
        return Err(SessionError::InvalidInput);
    }
    let mut pairs = Vec::new();
    for segment in header.split(';') {
        let part = segment.trim();
        if part.is_empty() {
            continue;
        }
        let Some((name, value)) = part.split_once('=') else {
            return Err(SessionError::InvalidInput);
        };
        let name = name.trim();
        let value = value.trim();
        if !valid_cookie_name(name) || value.is_empty() || value.len() > MAX_COOKIE_HEADER_BYTES {
            return Err(SessionError::InvalidInput);
        }
        pairs.push((name.to_owned(), value.to_owned()));
    }
    if pairs.is_empty() {
        return Err(SessionError::InvalidInput);
    }
    Ok(pairs)
}

fn parse_json_cookie_list(value: &Value) -> Result<Vec<(String, String)>, SessionError> {
    let Some(items) = value.as_array() else {
        return Err(SessionError::InvalidInput);
    };
    if items.is_empty() || items.len() > 256 {
        return Err(SessionError::InvalidInput);
    }
    let mut pairs = Vec::with_capacity(items.len());
    for item in items {
        let Some(obj) = item.as_object() else {
            return Err(SessionError::InvalidInput);
        };
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            return Err(SessionError::InvalidInput);
        };
        let Some(val) = obj.get("value").and_then(Value::as_str) else {
            return Err(SessionError::InvalidInput);
        };
        if !valid_cookie_name(name)
            || val.is_empty()
            || val.len() > MAX_COOKIE_HEADER_BYTES
            || val.bytes().any(|b| matches!(b, 0 | 10 | 13))
        {
            return Err(SessionError::InvalidInput);
        }
        pairs.push((name.to_owned(), val.to_owned()));
    }
    Ok(pairs)
}

fn extract_allowlisted_cookies(
    pairs: Vec<(String, String)>,
    allowed: &[&str],
    reject_device_prefixes: bool,
) -> Result<Vec<StoredCookie>, SessionError> {
    let mut kept: BTreeMap<String, String> = BTreeMap::new();
    for (name, value) in pairs {
        if reject_device_prefixes
            && (value.starts_with("dca:")
                || value.starts_with("sk-")
                || name.starts_with("dca:")
                || name.starts_with("sk-"))
        {
            return Err(SessionError::DisallowedToken);
        }
        if !allowed.contains(&name.as_str()) {
            continue;
        }
        if !valid_single_cookie_value(&value) {
            return Err(SessionError::InvalidInput);
        }
        if kept.insert(name, value).is_some() {
            return Err(SessionError::InvalidInput);
        }
    }
    if kept.is_empty() {
        return Err(SessionError::MissingRequiredCookie);
    }
    Ok(kept
        .into_iter()
        .map(|(name, value)| StoredCookie { name, value })
        .collect())
}

fn fingerprint_for(tag: &str, payload: &str) -> String {
    let digest = aac_core::sha256_hex(format!("{tag}:{payload}"));
    format!("sha256:{}", &digest[..16])
}

/// Parse and strictly validate a user-pasted web session for `surface`.
///
/// Accepts a raw `Cookie` header (with or without `Cookie:` prefix), a JSON cookie array
/// (`[{"name": "...", "value": "..."}]`), or for Qwen a JSON object with `cookie` (and optional
/// `gatewayCookie`, `region`, or OMP `baseUrl`).
pub fn parse_pasted(
    surface: Surface,
    raw: &str,
    options: &PasteOptions,
    stored_at: &str,
) -> Result<StoredSession, SessionError> {
    let trimmed = raw.trim_end_matches("\r\n").trim_end_matches('\n').trim();
    if trimmed.is_empty() || trimmed.len() > MAX_COOKIE_HEADER_BYTES {
        return Err(SessionError::InvalidInput);
    }
    if stored_at.is_empty()
        || stored_at.len() > 40
        || stored_at.bytes().any(|b| b < 0x20 || b == 0x7f)
    {
        return Err(SessionError::InvalidMetadata);
    }

    match surface {
        Surface::Qwen => parse_qwen(trimmed, options, stored_at),
        Surface::OpencodeWallet => parse_opencode(trimmed, options, stored_at),
        Surface::Muse => parse_muse(trimmed, options, stored_at),
    }
}

fn parse_qwen(
    trimmed: &str,
    options: &PasteOptions,
    stored_at: &str,
) -> Result<StoredSession, SessionError> {
    if options.workspace_id.is_some()
        || options.team_id.is_some()
        || options.email.is_some()
        || options.plan.is_some()
        || options.user_hash.is_some()
    {
        return Err(SessionError::InvalidMetadata);
    }
    let mut region_from_input: Option<String> = None;
    let mut gateway_cookie: Option<String> = None;
    let cookie: String;

    if trimmed.starts_with('{') {
        let parsed: Value =
            serde_json::from_str(trimmed).map_err(|_| SessionError::InvalidInput)?;
        let Some(obj) = parsed.as_object() else {
            return Err(SessionError::InvalidInput);
        };
        if let Some(raw_cookie) = obj.get("cookie").and_then(Value::as_str) {
            let cleaned = strip_cookie_prefix(raw_cookie);
            let _ = parse_cookie_pairs(cleaned)?;
            cookie = cleaned.to_owned();
        } else {
            return Err(SessionError::MissingRequiredCookie);
        }
        if let Some(raw_gw) = obj.get("gatewayCookie") {
            let Some(gw_str) = raw_gw.as_str() else {
                return Err(SessionError::InvalidInput);
            };
            let cleaned = strip_cookie_prefix(gw_str);
            if !cleaned.is_empty() {
                let _ = parse_cookie_pairs(cleaned)?;
                if cleaned != cookie {
                    gateway_cookie = Some(cleaned.to_owned());
                }
            }
        }
        if let Some(raw_region) = obj.get("region") {
            let Some(r) = raw_region.as_str() else {
                return Err(SessionError::InvalidMetadata);
            };
            if !valid_qwen_region(r) {
                return Err(SessionError::InvalidMetadata);
            }
            region_from_input = Some(r.to_owned());
        } else if let Some(raw_base) = obj.get("baseUrl") {
            let Some(base) = raw_base.as_str() else {
                return Err(SessionError::InvalidMetadata);
            };
            let base = base.trim_end_matches('/');
            region_from_input = Some(if base == QWEN_INTL_BASE {
                "intl".to_owned()
            } else if base == QWEN_CN_BASE {
                "cn".to_owned()
            } else {
                return Err(SessionError::InvalidMetadata);
            });
        }
    } else if trimmed.starts_with('[') {
        let parsed: Value =
            serde_json::from_str(trimmed).map_err(|_| SessionError::InvalidInput)?;
        let pairs = parse_json_cookie_list(&parsed)?;
        let joined = pairs
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ");
        if !valid_header_string(&joined) {
            return Err(SessionError::InvalidInput);
        }
        cookie = joined;
    } else {
        let cleaned = strip_cookie_prefix(trimmed);
        let _ = parse_cookie_pairs(cleaned)?;
        cookie = cleaned.to_owned();
    }

    let region = match options.region.as_deref() {
        Some(r) if valid_qwen_region(r) => r.to_owned(),
        Some(_) => return Err(SessionError::InvalidMetadata),
        None => region_from_input.unwrap_or_else(|| "intl".to_owned()),
    };
    let cookie_count = parse_cookie_pairs(&cookie)?.len();
    let fingerprint = fingerprint_for("qwen-web-session-v1", &format!("{region}\0{cookie}"));
    Ok(StoredSession {
        meta: SessionMeta {
            surface: Surface::Qwen.as_str().to_owned(),
            fingerprint,
            stored_at: stored_at.to_owned(),
            cookie_count,
            region: Some(region.clone()),
            workspace_id: None,
            team_id: None,
            email: None,
            plan: None,
            user_hash: None,
            account_id: None,
        },
        secret: SessionSecret::Qwen {
            cookie,
            gateway_cookie,
            region,
        },
    })
}

fn parse_opencode(
    trimmed: &str,
    options: &PasteOptions,
    stored_at: &str,
) -> Result<StoredSession, SessionError> {
    if options.region.is_some()
        || options.team_id.is_some()
        || options.email.is_some()
        || options.plan.is_some()
        || options.user_hash.is_some()
    {
        return Err(SessionError::InvalidMetadata);
    }
    if let Some(workspace_id) = options.workspace_id.as_deref()
        && !valid_workspace_id(workspace_id)
    {
        return Err(SessionError::InvalidMetadata);
    }
    let pairs = if trimmed.starts_with('[') {
        let parsed: Value =
            serde_json::from_str(trimmed).map_err(|_| SessionError::InvalidInput)?;
        parse_json_cookie_list(&parsed)?
    } else if trimmed.starts_with('{') {
        let parsed: Value =
            serde_json::from_str(trimmed).map_err(|_| SessionError::InvalidInput)?;
        if let Some(list) = parsed.get("cookies") {
            parse_json_cookie_list(list)?
        } else if let Some(header) = parsed.get("cookie").and_then(Value::as_str) {
            parse_cookie_pairs(strip_cookie_prefix(header))?
        } else {
            return Err(SessionError::InvalidInput);
        }
    } else {
        parse_cookie_pairs(strip_cookie_prefix(trimmed))?
    };
    let cookies = extract_allowlisted_cookies(pairs, &["auth", "__Host-console_session"], false)?;
    let secret = SessionSecret::OpencodeWallet {
        cookies: cookies.clone(),
    };
    let fingerprint = fingerprint_for("opencode-wallet-session-v1", &secret.cookie_header());
    Ok(StoredSession {
        meta: SessionMeta {
            surface: Surface::OpencodeWallet.as_str().to_owned(),
            fingerprint,
            stored_at: stored_at.to_owned(),
            cookie_count: cookies.len(),
            region: None,
            workspace_id: options.workspace_id.clone(),
            team_id: None,
            email: None,
            plan: None,
            user_hash: None,
            account_id: None,
        },
        secret,
    })
}

fn parse_muse(
    trimmed: &str,
    options: &PasteOptions,
    stored_at: &str,
) -> Result<StoredSession, SessionError> {
    if options.region.is_some() || options.workspace_id.is_some() {
        return Err(SessionError::InvalidMetadata);
    }
    if let Some(team_id) = options.team_id.as_deref()
        && !valid_team_id(team_id)
    {
        return Err(SessionError::InvalidMetadata);
    }
    if let Some(email) = options.email.as_deref()
        && !valid_email(email)
    {
        return Err(SessionError::InvalidMetadata);
    }
    if let Some(plan) = options.plan.as_deref()
        && !valid_plan(plan)
    {
        return Err(SessionError::InvalidMetadata);
    }
    if let Some(user_hash) = options.user_hash.as_deref()
        && !valid_sha256_hex(user_hash)
    {
        return Err(SessionError::InvalidMetadata);
    }
    if trimmed.starts_with("dca:") || trimmed.starts_with("sk-") {
        return Err(SessionError::DisallowedToken);
    }
    let pairs = if trimmed.starts_with('[') {
        let parsed: Value =
            serde_json::from_str(trimmed).map_err(|_| SessionError::InvalidInput)?;
        parse_json_cookie_list(&parsed)?
    } else if trimmed.starts_with('{') {
        let parsed: Value =
            serde_json::from_str(trimmed).map_err(|_| SessionError::InvalidInput)?;
        if let Some(list) = parsed.get("cookies") {
            parse_json_cookie_list(list)?
        } else if let Some(header) = parsed.get("cookie").and_then(Value::as_str) {
            if header.trim().starts_with("dca:") || header.trim().starts_with("sk-") {
                return Err(SessionError::DisallowedToken);
            }
            parse_cookie_pairs(strip_cookie_prefix(header))?
        } else {
            return Err(SessionError::InvalidInput);
        }
    } else {
        parse_cookie_pairs(strip_cookie_prefix(trimmed))?
    };
    let cookies = extract_allowlisted_cookies(pairs, &["llama_dev_sess", "llm_sess"], true)?;
    let secret = SessionSecret::Muse {
        cookies: cookies.clone(),
    };
    let fingerprint = fingerprint_for("muse-web-session-v1", &secret.cookie_header());
    Ok(StoredSession {
        meta: SessionMeta {
            surface: Surface::Muse.as_str().to_owned(),
            fingerprint,
            stored_at: stored_at.to_owned(),
            cookie_count: cookies.len(),
            region: None,
            workspace_id: None,
            team_id: options.team_id.clone(),
            email: options.email.clone(),
            plan: options.plan.clone(),
            user_hash: options.user_hash.clone(),
            account_id: None,
        },
        secret,
    })
}

fn encode_disk(session: &StoredSession) -> Result<Vec<u8>, SessionError> {
    #[cfg(windows)]
    let (encoding, secret, ciphertext) = {
        let plain =
            serde_json::to_vec(&session.secret).map_err(|_| SessionError::StorageUnavailable)?;
        let protected = dpapi::protect(&plain).ok_or(SessionError::StorageUnavailable)?;
        use base64::{Engine, engine::general_purpose::STANDARD};
        (
            "dpapi-v1".to_owned(),
            None,
            Some(STANDARD.encode(protected)),
        )
    };
    #[cfg(not(windows))]
    let (encoding, secret, ciphertext) = (
        "plain".to_owned(),
        Some(session.secret.clone()),
        None::<String>,
    );

    let record = DiskRecord {
        version: 1,
        surface: session.meta.surface.clone(),
        fingerprint: session.meta.fingerprint.clone(),
        stored_at: session.meta.stored_at.clone(),
        cookie_count: session.meta.cookie_count,
        region: session.meta.region.clone(),
        workspace_id: session.meta.workspace_id.clone(),
        team_id: session.meta.team_id.clone(),
        email: session.meta.email.clone(),
        plan: session.meta.plan.clone(),
        user_hash: session.meta.user_hash.clone(),
        account_id: session.meta.account_id.clone(),
        encoding,
        secret,
        ciphertext,
    };
    let mut bytes = serde_json::to_vec(&record).map_err(|_| SessionError::StorageUnavailable)?;
    bytes.push(b'\n');
    if bytes.len() > MAX_BYTES {
        return Err(SessionError::InvalidInput);
    }
    Ok(bytes)
}

fn decode_disk(surface: Surface, bytes: &[u8]) -> Result<StoredSession, SessionError> {
    let record: DiskRecord =
        serde_json::from_slice(bytes).map_err(|_| SessionError::StorageUnavailable)?;
    if record.version != 1 || record.surface != surface.as_str() {
        return Err(SessionError::StorageUnavailable);
    }
    let secret = match record.encoding.as_str() {
        "plain" => {
            if record.ciphertext.is_some() {
                return Err(SessionError::StorageUnavailable);
            }
            record
                .secret
                .clone()
                .ok_or(SessionError::StorageUnavailable)?
        }
        "dpapi-v1" => {
            #[cfg(windows)]
            {
                if record.secret.is_some() {
                    return Err(SessionError::StorageUnavailable);
                }
                let raw = record
                    .ciphertext
                    .as_deref()
                    .ok_or(SessionError::StorageUnavailable)?;
                use base64::{Engine, engine::general_purpose::STANDARD};
                let blob = STANDARD
                    .decode(raw)
                    .map_err(|_| SessionError::StorageUnavailable)?;
                let plain = dpapi::unprotect(&blob).ok_or(SessionError::StorageUnavailable)?;
                serde_json::from_slice::<SessionSecret>(&plain)
                    .map_err(|_| SessionError::StorageUnavailable)?
            }
            #[cfg(not(windows))]
            {
                return Err(SessionError::UnsupportedEncoding);
            }
        }
        _ => return Err(SessionError::UnsupportedEncoding),
    };
    validate_loaded(surface, &record, &secret)?;
    Ok(StoredSession {
        meta: SessionMeta {
            surface: record.surface,
            fingerprint: record.fingerprint,
            stored_at: record.stored_at,
            cookie_count: record.cookie_count,
            region: record.region,
            workspace_id: record.workspace_id,
            team_id: record.team_id,
            email: record.email,
            plan: record.plan,
            user_hash: record.user_hash,
            account_id: record.account_id,
        },
        secret,
    })
}

fn validate_loaded(
    surface: Surface,
    record: &DiskRecord,
    secret: &SessionSecret,
) -> Result<(), SessionError> {
    match (surface, secret) {
        (
            Surface::Qwen,
            SessionSecret::Qwen {
                cookie,
                gateway_cookie,
                region,
            },
        ) => {
            if !valid_qwen_region(region) || record.region.as_deref() != Some(region.as_str()) {
                return Err(SessionError::StorageUnavailable);
            }
            let _ = parse_cookie_pairs(cookie)?;
            if let Some(gw) = gateway_cookie {
                let _ = parse_cookie_pairs(gw)?;
            }
            if record.account_id.is_some() {
                return Err(SessionError::StorageUnavailable);
            }
            Ok(())
        }
        (Surface::OpencodeWallet, SessionSecret::OpencodeWallet { cookies }) => {
            if cookies.is_empty()
                || cookies.len() > 2
                || !cookies.iter().all(|c| {
                    matches!(c.name.as_str(), "auth" | "__Host-console_session")
                        && valid_single_cookie_value(&c.value)
                })
            {
                return Err(SessionError::StorageUnavailable);
            }
            if let Some(ws) = record.workspace_id.as_deref()
                && !valid_workspace_id(ws)
            {
                return Err(SessionError::StorageUnavailable);
            }
            if let Some(account_id) = record.account_id.as_deref()
                && !valid_wallet_account_id(account_id)
            {
                return Err(SessionError::StorageUnavailable);
            }
            Ok(())
        }
        (Surface::Muse, SessionSecret::Muse { cookies }) => {
            if cookies.is_empty()
                || cookies.len() > 2
                || !cookies.iter().all(|c| {
                    matches!(c.name.as_str(), "llama_dev_sess" | "llm_sess")
                        && valid_single_cookie_value(&c.value)
                        && !c.value.starts_with("dca:")
                        && !c.value.starts_with("sk-")
                })
            {
                return Err(SessionError::StorageUnavailable);
            }
            if let Some(team) = record.team_id.as_deref()
                && !valid_team_id(team)
            {
                return Err(SessionError::StorageUnavailable);
            }
            if let Some(email) = record.email.as_deref()
                && !valid_email(email)
            {
                return Err(SessionError::StorageUnavailable);
            }
            if let Some(plan) = record.plan.as_deref()
                && !valid_plan(plan)
            {
                return Err(SessionError::StorageUnavailable);
            }
            if let Some(hash) = record.user_hash.as_deref()
                && !valid_sha256_hex(hash)
            {
                return Err(SessionError::StorageUnavailable);
            }
            if record.account_id.is_some() {
                return Err(SessionError::StorageUnavailable);
            }
            Ok(())
        }
        _ => Err(SessionError::StorageUnavailable),
    }
}

/// Save a validated web session under the Rust service's private-file ownership lease.
pub fn save_with_owners(
    owners: &Owners,
    root: &Path,
    surface: Surface,
    session: &StoredSession,
) -> Result<(), SessionError> {
    if session.meta.surface != surface.as_str() {
        return Err(SessionError::InvalidMetadata);
    }
    let file_name = session_file_name(surface, session)?;
    let bytes = encode_disk(session)?;
    let rows = registry::rows().map_err(|_| SessionError::StorageUnavailable)?;
    let store = owners
        .claim(rows, ROW_ID, root, Some(file_name.as_str()))
        .map_err(|_| SessionError::StorageUnavailable)?;
    store
        .write(&bytes)
        .map_err(|_| SessionError::StorageUnavailable)
}

/// Save a validated web session under the host agent's private-file lease.
pub fn save_as_agent(
    root: &Path,
    surface: Surface,
    session: &StoredSession,
) -> Result<(), SessionError> {
    if session.meta.surface != surface.as_str() {
        return Err(SessionError::InvalidMetadata);
    }
    let file_name = session_file_name(surface, session)?;
    let bytes = encode_disk(session)?;
    let store = registry::claim_as_agent(ROW_ID, root, Some(file_name.as_str()))
        .map_err(|_| SessionError::StorageUnavailable)?;
    store
        .write(&bytes)
        .map_err(|_| SessionError::StorageUnavailable)
}

/// Delete a stored web session under the Rust service's lease. Missing files succeed.
pub fn delete_with_owners(
    owners: &Owners,
    root: &Path,
    surface: Surface,
) -> Result<(), SessionError> {
    let rows = registry::rows().map_err(|_| SessionError::StorageUnavailable)?;
    let store = owners
        .claim(rows, ROW_ID, root, Some(surface.file_name()))
        .map_err(|_| SessionError::StorageUnavailable)?;
    store.remove().map_err(|_| SessionError::StorageUnavailable)
}

/// Delete a stored web session under the host agent's lease. Missing files succeed.
pub fn delete_as_agent(root: &Path, surface: Surface) -> Result<(), SessionError> {
    let store = registry::claim_as_agent(ROW_ID, root, Some(surface.file_name()))
        .map_err(|_| SessionError::StorageUnavailable)?;
    store.remove().map_err(|_| SessionError::StorageUnavailable)
}

/// Delete one account's OpenCode wallet session under the Rust service's lease.
pub fn delete_for_account_with_owners(
    owners: &Owners,
    root: &Path,
    surface: Surface,
    account_id: &str,
) -> Result<(), SessionError> {
    if surface != Surface::OpencodeWallet || !valid_wallet_account_id(account_id) {
        return Err(SessionError::InvalidMetadata);
    }
    let rows = registry::rows().map_err(|_| SessionError::StorageUnavailable)?;
    let store = owners
        .claim(
            rows,
            ROW_ID,
            root,
            Some(wallet_file_name(account_id).as_str()),
        )
        .map_err(|_| SessionError::StorageUnavailable)?;
    store.remove().map_err(|_| SessionError::StorageUnavailable)
}

/// Delete one account's OpenCode wallet session under the host agent's lease.
pub fn delete_for_account_as_agent(
    root: &Path,
    surface: Surface,
    account_id: &str,
) -> Result<(), SessionError> {
    if surface != Surface::OpencodeWallet || !valid_wallet_account_id(account_id) {
        return Err(SessionError::InvalidMetadata);
    }
    let store = registry::claim_as_agent(ROW_ID, root, Some(wallet_file_name(account_id).as_str()))
        .map_err(|_| SessionError::StorageUnavailable)?;
    store.remove().map_err(|_| SessionError::StorageUnavailable)
}

/// Load a stored web session through the hardened private-file reader.
pub fn load(root: &Path, surface: Surface) -> Result<Option<StoredSession>, SessionError> {
    load_named(root, surface, surface.file_name())
}

fn load_named(
    root: &Path,
    surface: Surface,
    file_name: &str,
) -> Result<Option<StoredSession>, SessionError> {
    let row = registry::row(ROW_ID).map_err(|_| SessionError::StorageUnavailable)?;
    let relative = row
        .file(Some(file_name))
        .map_err(|_| SessionError::StorageUnavailable)?;
    let file = root.join(relative);
    let policy = ReadPolicy::capped(row.max_bytes)
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(root));
    match read::read(&file, &policy) {
        Read::Absent => Ok(None),
        Read::Bytes(bytes, _) => {
            if !single_link(&file) {
                return Err(SessionError::StorageUnavailable);
            }
            decode_disk(surface, &bytes).map(Some)
        }
        _ => Err(SessionError::StorageUnavailable),
    }
}

/// Load one OpenCode Go account's pasted wallet session. A file whose recorded
/// binding does not match the requested account reads as absent, never as another
/// account's session.
pub fn load_for_account(
    root: &Path,
    surface: Surface,
    account_id: &str,
) -> Result<Option<StoredSession>, SessionError> {
    if surface != Surface::OpencodeWallet || !valid_wallet_account_id(account_id) {
        return Err(SessionError::InvalidMetadata);
    }
    let stored = load_named(root, surface, &wallet_file_name(account_id))?;
    Ok(stored.filter(|stored| stored.meta.account_id.as_deref() == Some(account_id)))
}

/// Read only the non-secret metadata summary for one surface.
pub fn summary(root: &Path, surface: Surface) -> Result<Option<SessionMeta>, SessionError> {
    Ok(load(root, surface)?.map(|stored| stored.meta))
}

/// Read only the non-secret metadata summary for one account's wallet session.
pub fn summary_for_account(
    root: &Path,
    surface: Surface,
    account_id: &str,
) -> Result<Option<SessionMeta>, SessionError> {
    Ok(load_for_account(root, surface, account_id)?.map(|stored| stored.meta))
}

/// Read non-secret metadata summaries for every supported surface.
pub fn summaries(root: &Path) -> BTreeMap<&'static str, Option<SessionMeta>> {
    let mut out = BTreeMap::new();
    for surface in Surface::all() {
        let meta = summary(root, surface).ok().flatten();
        out.insert(surface.as_str(), meta);
    }
    out
}

#[cfg(windows)]
pub(crate) mod dpapi {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::Cryptography::{
            CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
        },
    };

    const ENTROPY: &[u8] = b"AAC/web-session/v1";

    pub(super) fn protect(plain: &[u8]) -> Option<Vec<u8>> {
        protect_with(ENTROPY, plain)
    }

    pub(super) fn unprotect(ciphertext: &[u8]) -> Option<Vec<u8>> {
        unprotect_with(ENTROPY, ciphertext)
    }

    /// DPAPI `CurrentUser` protection under a caller's entropy (the `credential` class uses its own).
    #[allow(unsafe_code)]
    pub(crate) fn protect_with(entropy: &[u8], plain: &[u8]) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(plain.len()).ok()?,
            pbData: plain.as_ptr() as *mut u8,
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(entropy.len()).ok()?,
            pbData: entropy.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: null_mut(),
        };
        // SAFETY: input and entropy point to valid immutable slices for the call; output is
        // allocated by Windows and released via LocalFree before returning.
        let ok = unsafe {
            CryptProtectData(
                &input,
                null(),
                &entropy,
                null_mut(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 || output.pbData.is_null() {
            return None;
        }
        // SAFETY: CryptProtectData succeeded and initialized pbData[..cbData].
        let bytes = unsafe {
            let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
            let copied = slice.to_vec();
            LocalFree(output.pbData as _);
            copied
        };
        Some(bytes)
    }

    /// The inverse of [`protect_with`] under the same entropy.
    #[allow(unsafe_code)]
    pub(crate) fn unprotect_with(entropy: &[u8], ciphertext: &[u8]) -> Option<Vec<u8>> {
        let input = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(ciphertext.len()).ok()?,
            pbData: ciphertext.as_ptr() as *mut u8,
        };
        let entropy = CRYPT_INTEGER_BLOB {
            cbData: u32::try_from(entropy.len()).ok()?,
            pbData: entropy.as_ptr() as *mut u8,
        };
        let mut output = CRYPT_INTEGER_BLOB {
            cbData: 0,
            pbData: null_mut(),
        };
        // SAFETY: input and entropy point to valid buffers; output is released via LocalFree.
        let ok = unsafe {
            CryptUnprotectData(
                &input,
                null_mut(),
                &entropy,
                null_mut(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        };
        if ok == 0 || output.pbData.is_null() {
            return None;
        }
        // SAFETY: CryptUnprotectData succeeded and initialized pbData[..cbData].
        let bytes = unsafe {
            let slice = std::slice::from_raw_parts(output.pbData, output.cbData as usize);
            let copied = slice.to_vec();
            LocalFree(output.pbData as _);
            copied
        };
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn owners_for_writes() -> Owners {
        let rows = registry::rows().unwrap();
        let files = rows
            .iter()
            .map(|r| {
                let role = if r.moves_with.as_deref() == Some("writes") {
                    "rust-service"
                } else {
                    "ts"
                };
                (r.id.clone(), role.to_owned())
            })
            .collect();
        Owners::from_boot(rows, &files).unwrap()
    }

    #[test]
    fn qwen_pasted_cookie_and_omp_json_round_trip_with_private_mode() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let owners = owners_for_writes();
        let raw = "Cookie: cna=abc123; login_aliyunid_csrf=tk%2B1; _tb_token_=tok99";
        let parsed = parse_pasted(
            Surface::Qwen,
            raw,
            &PasteOptions::default(),
            "2026-11-26T20:00:00Z",
        )
        .unwrap();
        assert_eq!(parsed.meta.region.as_deref(), Some("intl"));
        assert_eq!(parsed.meta.cookie_count, 3);
        assert!(parsed.meta.fingerprint.starts_with("sha256:"));
        assert!(!format!("{parsed:?}").contains("tok99"));
        assert!(!parsed.meta.public_summary().to_string().contains("tok99"));

        save_with_owners(&owners, &root, Surface::Qwen, &parsed).unwrap();
        let path = root.join("account-usage/sessions/qwen.json");
        #[cfg(not(unix))]
        let _ = &path;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(std::fs::metadata(&path).unwrap().mode() & 0o777, 0o600);
        }
        let loaded = load(&root, Surface::Qwen).unwrap().unwrap();
        assert_eq!(loaded.meta, parsed.meta);
        assert_eq!(loaded.secret, parsed.secret);

        // OMP packed CN object also parses and infers region "cn".
        let omp = json!({
            "cookie": "login_aliyunid_csrf=cn%20tok; cna=xyz",
            "baseUrl": "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1",
            "token": "unused-inference-key"
        })
        .to_string();
        let cn = parse_pasted(
            Surface::Qwen,
            &omp,
            &PasteOptions::default(),
            "2026-11-26T20:01:00Z",
        )
        .unwrap();
        assert_eq!(cn.meta.region.as_deref(), Some("cn"));
        assert_eq!(cn.secret.qwen_parts().unwrap().2, "cn");

        delete_with_owners(&owners, &root, Surface::Qwen).unwrap();
        assert!(load(&root, Surface::Qwen).unwrap().is_none());
    }

    #[test]
    fn opencode_and_muse_strip_unlisted_cookies_and_reject_device_tokens() {
        // OpenCode keeps only `auth` and `__Host-console_session`.
        let raw =
            "cf_clearance=dropme; __Host-console_session=sess-value-1; auth=auth-val-2; _ga=drop";
        let oc = parse_pasted(
            Surface::OpencodeWallet,
            raw,
            &PasteOptions {
                workspace_id: Some("wrk_01ABC".into()),
                ..Default::default()
            },
            "2026-11-26T20:00:00Z",
        )
        .unwrap();
        assert_eq!(oc.meta.cookie_count, 2);
        let header = oc.secret.cookie_header();
        assert!(!header.contains("cf_clearance"));
        assert!(!header.contains("_ga"));
        assert!(header.contains("__Host-console_session=sess-value-1"));
        assert!(header.contains("auth=auth-val-2"));

        // Missing allowlisted cookie is rejected.
        assert_eq!(
            parse_pasted(
                Surface::OpencodeWallet,
                "cf_clearance=only",
                &PasteOptions::default(),
                "2026-11-26T20:00:00Z",
            )
            .unwrap_err(),
            SessionError::MissingRequiredCookie
        );

        // Muse rejects CLI `dca:` tokens and keeps only `llama_dev_sess` / `llm_sess`.
        assert_eq!(
            parse_pasted(
                Surface::Muse,
                "dca:live-device-token",
                &PasteOptions::default(),
                "2026-11-26T20:00:00Z",
            )
            .unwrap_err(),
            SessionError::DisallowedToken
        );
        assert_eq!(
            parse_pasted(
                Surface::Muse,
                "llama_dev_sess=dca:smuggled",
                &PasteOptions::default(),
                "2026-11-26T20:00:00Z",
            )
            .unwrap_err(),
            SessionError::DisallowedToken
        );
        let muse = parse_pasted(
            Surface::Muse,
            "other=ignored; llama_dev_sess=web-sess-99; llm_sess=web-llm-88",
            &PasteOptions {
                email: Some("owner@example.test".into()),
                plan: Some("Pro".into()),
                team_id: Some("12345".into()),
                user_hash: Some("a".repeat(64)),
                ..Default::default()
            },
            "2026-11-26T20:00:00Z",
        )
        .unwrap();
        assert_eq!(muse.meta.cookie_count, 2);
        let summary = muse.meta.public_summary().to_string();
        assert!(!summary.contains("web-sess-99"));
        assert!(!summary.contains("userHash"));
        assert!(summary.contains("owner@example.test"));
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_loose_mode_and_symlinks() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let parsed = parse_pasted(
            Surface::Qwen,
            "cna=ok1; token=ok2",
            &PasteOptions::default(),
            "2026-11-26T20:00:00Z",
        )
        .unwrap();
        save_as_agent(&root, Surface::Qwen, &parsed).unwrap();
        let path = root.join("account-usage/sessions/qwen.json");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            load(&root, Surface::Qwen).unwrap_err(),
            SessionError::StorageUnavailable
        );
    }

    /// Each OpenCode Go account pastes its own console session: two accounts keep
    /// independent files, the legacy single file still decodes, and a new save without
    /// an account binding is refused.
    #[test]
    fn opencode_per_account_sessions_round_trip_and_legacy_keeps_decoding() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let mut first = parse_pasted(
            Surface::OpencodeWallet,
            "auth=auth-val-1",
            &PasteOptions::default(),
            "2026-11-26T20:00:00Z",
        )
        .unwrap();
        first.meta.account_id = Some("opencode-go:usage".to_owned());
        let mut second = parse_pasted(
            Surface::OpencodeWallet,
            "auth=auth-val-2",
            &PasteOptions::default(),
            "2026-11-26T20:01:00Z",
        )
        .unwrap();
        second.meta.account_id = Some("opencode-go:acct:52f3251b".to_owned());

        save_as_agent(&root, Surface::OpencodeWallet, &first).unwrap();
        save_as_agent(&root, Surface::OpencodeWallet, &second).unwrap();
        assert!(load(&root, Surface::OpencodeWallet).unwrap().is_none());
        let loaded_first = load_for_account(&root, Surface::OpencodeWallet, "opencode-go:usage")
            .unwrap()
            .unwrap();
        let loaded_second =
            load_for_account(&root, Surface::OpencodeWallet, "opencode-go:acct:52f3251b")
                .unwrap()
                .unwrap();
        assert_eq!(loaded_first.secret, first.secret);
        assert_eq!(loaded_second.secret, second.secret);
        assert_eq!(
            loaded_first.meta.account_id.as_deref(),
            Some("opencode-go:usage")
        );
        assert_eq!(
            loaded_first.meta.public_summary()["accountId"],
            "opencode-go:usage"
        );

        // Deleting one account's session leaves the other's file untouched.
        delete_for_account_as_agent(&root, Surface::OpencodeWallet, "opencode-go:usage").unwrap();
        assert!(
            load_for_account(&root, Surface::OpencodeWallet, "opencode-go:usage")
                .unwrap()
                .is_none()
        );
        assert!(
            load_for_account(&root, Surface::OpencodeWallet, "opencode-go:acct:52f3251b")
                .unwrap()
                .is_some()
        );

        // A file whose bytes belong to another account is not adopted (no guessing).
        let spare = parse_pasted(
            Surface::OpencodeWallet,
            "auth=auth-val-3",
            &PasteOptions::default(),
            "2026-11-26T20:02:00Z",
        )
        .unwrap();
        assert_eq!(
            save_as_agent(&root, Surface::OpencodeWallet, &spare),
            Err(SessionError::InvalidMetadata)
        );

        // The pre-per-account single file (no account binding) still decodes.
        let legacy = std::fs::read_dir(root.join("account-usage/sessions"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(legacy, 1, "only the second account file remains");
        let legacy_bytes = br#"{"version":1,"surface":"opencode-wallet","fingerprint":"sha256:0123456789abcdef","storedAt":"2026-11-25T10:00:00Z","cookieCount":1,"encoding":"plain","secret":{"kind":"opencode-wallet","cookies":[{"name":"auth","value":"auth-val-legacy"}]}}"#;
        let legacy_path = root.join("account-usage/sessions/opencode-wallet.json");
        std::fs::write(&legacy_path, legacy_bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&legacy_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let legacy_loaded = load(&root, Surface::OpencodeWallet)
            .unwrap()
            .expect("legacy global session still loads");
        assert_eq!(legacy_loaded.meta.account_id, None);

        // Two accounts never share one file, and Qwen/Muse refuse account bindings.
        assert_eq!(
            load_for_account(&root, Surface::Qwen, "opencode-go:usage"),
            Err(SessionError::InvalidMetadata)
        );
        assert!(!valid_wallet_account_id("opencode-go:"));
        assert!(!valid_wallet_account_id("opencode-go:../escape"));
        assert!(!valid_wallet_account_id("muse:usage"));
        assert!(valid_wallet_account_id("opencode-go:usage"));
        assert!(valid_wallet_account_id("opencode-go:usage:cccccccc"));
        assert!(valid_wallet_account_id("opencode-go:acct:52f3251b"));
    }
}
