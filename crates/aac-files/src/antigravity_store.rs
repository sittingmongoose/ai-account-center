//! Antigravity private credential store, Ubuntu D-Bus Secret Service probe,
//! native credential worker protocol, OAuth client reader, masked credential checks,
//! and runtime bundle health checks.
//!
//! Native Rust port of `scripts/antigravity/auth_platforms.py`,
//! `scripts/antigravity/native_credential_worker.py`,
//! `scripts/antigravity/runtime_health.py`,
//! `scripts/account-usage/desktop_usage.py:antigravity_oauth_client`, and
//! `src/antigravity/signin-sandbox.ts:maskedCredentialState` / `readMaskedCredential`.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::de::{self, DeserializeSeed, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Number, Value, json};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::fs;
#[cfg(target_os = "linux")]
use std::io::Write;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Maximum size of a single Antigravity credential envelope in bytes.
pub const MAX_CREDENTIAL_BYTES: usize = 16_384;
/// Maximum size of a private native-credential worker JSON packet in bytes.
pub const MAX_PACKET_BYTES: usize = 65_536;
/// Wire format identifier for Antigravity consumer OAuth credentials.
pub const CREDENTIAL_FORMAT: &str = "antigravity-consumer-json";
/// Relative path under `$HOME` for the Ubuntu fallback credential file.
pub const FALLBACK_RELATIVE: &str = ".gemini/antigravity-cli/antigravity-oauth-token";
/// Relative path under `$HOME` for the private Antigravity OAuth client metadata.
pub const OAUTH_CLIENT_RELATIVE: &str = ".ccs/account-usage/antigravity-oauth-client.json";
/// Secret Service attribute `service` queried by the Ubuntu backend probe.
pub const SERVICE: &str = "gemini";
/// Secret Service attribute `username` queried by the Ubuntu backend probe.
pub const ACCOUNT: &str = "antigravity";

const MAX_JSON_DEPTH: usize = 128;
const MAX_BASE64_CREDENTIAL_LEN: usize = 22_000;
const MAX_OAUTH_CLIENT_BYTES: usize = 4_096;

/// Safe classification for Antigravity credential store and identity errors.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthErrorKind {
    /// Generic store, validation, or race error.
    Auth,
    /// Provider or network temporarily unavailable.
    IdentityUnavailable,
    /// Provider returned an unverified or mismatched identity.
    IdentityMismatch,
    /// Saved credential was rejected or revoked and needs sign-in again.
    NeedsSignIn,
}

/// Fixed safe error carrying no provider response bodies or credential secrets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthError {
    /// Machine-readable classification.
    pub kind: AuthErrorKind,
    /// Fixed safe description.
    pub message: &'static str,
}

impl AuthError {
    /// Generic safe auth/store error.
    pub fn auth(message: &'static str) -> Self {
        Self {
            kind: AuthErrorKind::Auth,
            message,
        }
    }

    /// Temporary identity verification or refresh failure.
    pub fn identity_unavailable(message: &'static str) -> Self {
        Self {
            kind: AuthErrorKind::IdentityUnavailable,
            message,
        }
    }

    /// Unverified or mismatched account identity.
    pub fn identity_mismatch(message: &'static str) -> Self {
        Self {
            kind: AuthErrorKind::IdentityMismatch,
            message,
        }
    }

    /// Credential rejected by provider; user must sign in again.
    pub fn needs_sign_in(message: &'static str) -> Self {
        Self {
            kind: AuthErrorKind::NeedsSignIn,
            message,
        }
    }
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}

impl std::error::Error for AuthError {}

/// Lowercase SHA-256 hex digest of raw bytes.
pub fn sha256_hex(raw: &[u8]) -> String {
    let digest = Sha256::digest(raw);
    let mut out = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

/// Private raw credential bytes and their SHA-256 revision; redacted in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateCredential {
    raw: Vec<u8>,
    revision: String,
}

impl fmt::Debug for PrivateCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateCredential").finish_non_exhaustive()
    }
}

impl PrivateCredential {
    /// Validate consumer credential JSON bytes and wrap them with their SHA-256 revision.
    pub fn new(raw: Vec<u8>) -> Result<Self, AuthError> {
        parse_credential(&raw)?;
        let revision = sha256_hex(&raw);
        Ok(Self { raw, revision })
    }

    /// Wrap bytes without re-validating schema (for post-exchange displaced snapshots).
    pub fn from_unchecked(raw: Vec<u8>) -> Self {
        let revision = sha256_hex(&raw);
        Self { raw, revision }
    }

    /// Raw credential bytes.
    pub fn raw(&self) -> &[u8] {
        &self.raw
    }

    /// Lowercase 64-hex SHA-256 digest of the raw credential bytes.
    pub fn revision(&self) -> &str {
        &self.revision
    }
}

/// Validated Antigravity consumer OAuth credential fields; tokens are redacted in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct ParsedCredential {
    /// OAuth bearer access token.
    pub access_token: String,
    /// OAuth refresh token.
    pub refresh_token: String,
    /// Original ISO-8601 expiry string.
    pub expiry: String,
    /// Expiry instant as UTC epoch milliseconds.
    pub expiry_ms: i64,
    /// Optional OIDC id_token string.
    pub id_token: Option<String>,
    /// Validated top-level JSON object for in-memory token renewal.
    pub raw_object: Map<String, Value>,
}

impl fmt::Debug for ParsedCredential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ParsedCredential")
            .field("expiry", &self.expiry)
            .finish_non_exhaustive()
    }
}

/// Verified Antigravity Google account identity; subject is redacted in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedIdentity {
    /// Canonical lower-case email address.
    pub email: String,
    /// Authoritative numeric Google account subject.
    pub subject: String,
    /// ISO-8601 millisecond UTC timestamp of verification.
    pub verified_at: String,
    /// Human-readable verification authority.
    pub authority: &'static str,
    /// Machine-readable identity source word.
    pub source: &'static str,
}

impl fmt::Debug for VerifiedIdentity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VerifiedIdentity")
            .field("email", &self.email)
            .field("verified_at", &self.verified_at)
            .field("authority", &self.authority)
            .field("source", &self.source)
            .finish_non_exhaustive()
    }
}

impl VerifiedIdentity {
    /// Stable identity key: SHA-256 of compact JSON `["antigravity", email.lower(), subject]`.
    pub fn identity_key(&self) -> String {
        let payload = json!(["antigravity", self.email.to_lowercase(), self.subject]).to_string();
        sha256_hex(payload.as_bytes())
    }
}

/// Provider HTTP call classification passed into [`verify_identity`] and [`refresh_in_memory`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProviderCallError {
    /// HTTP 401/403 or OAuth `invalid_grant`.
    NeedsSignIn,
    /// HTTP 429 rate limit with optional clamped retry interval in seconds.
    RateLimited {
        /// Suggested retry interval in seconds.
        retry_after: Option<u64>,
    },
    /// Any other transient transport or parse error.
    Unavailable,
}

/// Bound snapshot of the Ubuntu fallback credential file inode and bytes.
#[derive(Clone, PartialEq, Eq)]
pub struct FileSnapshot {
    /// Credential bytes and revision.
    pub credential: PrivateCredential,
    /// POSIX `st_dev`.
    pub device: u64,
    /// POSIX `st_ino`.
    pub inode: u64,
    /// POSIX modification time in nanoseconds since the Unix epoch.
    pub modified_ns: i128,
}

impl fmt::Debug for FileSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileSnapshot")
            .field("device", &self.device)
            .field("inode", &self.inode)
            .field("modified_ns", &self.modified_ns)
            .finish_non_exhaustive()
    }
}

/// Receipt proving what was replaced and what was installed during an atomic exchange.
#[derive(Clone, PartialEq, Eq)]
pub struct InstallReceipt {
    /// Previous live snapshot displaced by the exchange.
    pub before: FileSnapshot,
    /// Newly installed live snapshot.
    pub installed: FileSnapshot,
}

impl fmt::Debug for InstallReceipt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InstallReceipt").finish_non_exhaustive()
    }
}

/// Read-only status of the Ubuntu Secret Service backend for Antigravity credentials.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UbuntuBackendProbe {
    /// One of `"fallback-file"`, `"secret-service"`, `"locked"`, `"ambiguous"`, `"unavailable"`.
    pub store: &'static str,
    /// Whether `org.freedesktop.secrets` currently has a bus owner.
    pub service_running: bool,
    /// Count of matching unlocked keyring items.
    pub unlocked_matches: usize,
    /// Count of matching locked keyring items.
    pub locked_matches: usize,
}

/// Private installed-app OAuth client values; secret is redacted in `Debug`.
#[derive(Clone, PartialEq, Eq)]
pub struct OAuthClient {
    /// Google OAuth client ID ending in `.apps.googleusercontent.com`.
    pub client_id: String,
    /// Google OAuth client secret.
    pub client_secret: String,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthClient")
            .field("client_id", &self.client_id)
            .finish_non_exhaustive()
    }
}

/// State of the staged masked OAuth token file during isolated sign-in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskedCredentialState {
    /// File does not exist yet.
    Absent,
    /// File exists but is incomplete, invalid JSON, non-regular, or unreadable.
    Partial,
    /// Complete consumer credential in an owned `0600` regular file with `nlink == 1`.
    Complete,
    /// Complete consumer credential in a file failing ownership, mode, or link-count guards.
    Unsafe,
}

// ---------------------------------------------------------------------------
// Strict JSON parser (rejects duplicate keys, non-finite numbers, deep nesting)
// ---------------------------------------------------------------------------

struct StrictValueSeed(usize);

impl<'de> DeserializeSeed<'de> for StrictValueSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.0 > MAX_JSON_DEPTH {
            return Err(de::Error::custom("json depth exceeded"));
        }
        deserializer.deserialize_any(StrictValueVisitor(self.0))
    }
}

struct StrictValueVisitor(usize);

impl<'de> Visitor<'de> for StrictValueVisitor {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("strict JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_u64<E>(self, v: u64) -> Result<Value, E> {
        Ok(Value::Number(v.into()))
    }

    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Value, E> {
        if !v.is_finite() {
            return Err(de::Error::custom("non-finite float"));
        }
        Number::from_f64(v)
            .map(Value::Number)
            .ok_or_else(|| de::Error::custom("non-finite float"))
    }

    fn visit_str<E: de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        if self.0 >= MAX_JSON_DEPTH {
            return Err(de::Error::custom("json depth exceeded"));
        }
        let mut out = Vec::new();
        while let Some(elem) = seq.next_element_seed(StrictValueSeed(self.0 + 1))? {
            out.push(elem);
        }
        Ok(Value::Array(out))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Value, A::Error> {
        if self.0 >= MAX_JSON_DEPTH {
            return Err(de::Error::custom("json depth exceeded"));
        }
        let mut map = Map::new();
        while let Some(key) = access.next_key::<String>()? {
            if map.contains_key(&key) {
                return Err(de::Error::custom("duplicate json key"));
            }
            let val = access.next_value_seed(StrictValueSeed(self.0 + 1))?;
            map.insert(key, val);
        }
        Ok(Value::Object(map))
    }
}

/// Parse strict UTF-8 JSON requiring a top-level object with unique keys at every nesting depth
/// and finite numbers only.
pub fn parse_strict_json_object(raw: &[u8]) -> Result<Map<String, Value>, AuthError> {
    let text = std::str::from_utf8(raw)
        .map_err(|_| AuthError::auth("Invalid Antigravity JSON encoding."))?;
    let mut de = serde_json::Deserializer::from_str(text);
    let value = StrictValueSeed(0)
        .deserialize(&mut de)
        .map_err(|_| AuthError::auth("Invalid Antigravity JSON syntax."))?;
    de.end()
        .map_err(|_| AuthError::auth("Trailing data in Antigravity JSON."))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err(AuthError::auth("Expected Antigravity JSON object.")),
    }
}

// ---------------------------------------------------------------------------
// ISO-8601 timestamp helpers (matches Python datetime.fromisoformat + year 2000..=2100)
// ---------------------------------------------------------------------------

/// Parse a timezone-aware ISO-8601 timestamp (`Z` or `±HH:MM[:SS]`) into `(utc_ms, local_year)`.
/// Accepts `T` or space separator and optional fractional seconds.
pub fn parse_iso_datetime_utc_ms(text: &str) -> Option<(i64, i64)> {
    let b = text.as_bytes();
    if b.len() < 20 {
        return None;
    }
    let digits = |slice: &[u8]| -> Option<i64> {
        if slice.is_empty() || !slice.iter().all(u8::is_ascii_digit) {
            return None;
        }
        slice.iter().try_fold(0i64, |acc, &d| {
            acc.checked_mul(10)?.checked_add(i64::from(d - b'0'))
        })
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't' | b' ')
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let year = digits(&b[0..4])?;
    let month = digits(&b[5..7])?;
    let day = digits(&b[8..10])?;
    let hour = digits(&b[11..13])?;
    let minute = digits(&b[14..16])?;
    let second = digits(&b[17..19])?;
    if !(1..=12).contains(&month)
        || day < 1
        || day > aac_num::civil::days_in_month(year, month)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=59).contains(&second)
    {
        return None;
    }
    let mut pos = 19usize;
    let mut millis: i64 = 0;
    if b.get(pos) == Some(&b'.') {
        pos += 1;
        let start = pos;
        while pos < b.len() && b[pos].is_ascii_digit() {
            pos += 1;
        }
        let frac = &b[start..pos];
        if frac.is_empty() || frac.len() > 9 {
            return None;
        }
        // Truncate/pad to milliseconds, matching Python datetime microsecond storage.
        let mut ms_val = 0i64;
        for i in 0..3 {
            let d = frac.get(i).copied().unwrap_or(b'0') - b'0';
            ms_val = ms_val * 10 + i64::from(d);
        }
        millis = ms_val;
    }
    let tz = &b[pos..];
    let offset_secs: i64 = match tz {
        b"Z" | b"z" => 0,
        _ => {
            let sign: i64 = match *tz.first()? {
                b'+' => 1,
                b'-' => -1,
                _ => return None,
            };
            let rest = &tz[1..];
            let (oh, om, os) = if rest.len() == 5 && rest[2] == b':' {
                (digits(&rest[0..2])?, digits(&rest[3..5])?, 0)
            } else if rest.len() == 4 {
                (digits(&rest[0..2])?, digits(&rest[2..4])?, 0)
            } else if rest.len() == 8 && rest[2] == b':' && rest[5] == b':' {
                (
                    digits(&rest[0..2])?,
                    digits(&rest[3..5])?,
                    digits(&rest[6..8])?,
                )
            } else {
                return None;
            };
            if oh > 23 || om > 59 || os > 59 {
                return None;
            }
            sign * (oh * 3600 + om * 60 + os)
        }
    };
    let days = aac_num::civil::days_from_civil(year, month, day);
    let secs = days
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?
        .checked_sub(offset_secs)?;
    let utc_ms = secs.checked_mul(1000)?.checked_add(millis)?;
    Some((utc_ms, year))
}

fn format_iso_micros_z(epoch_seconds: f64) -> Option<String> {
    let (seconds, micros) = aac_num::py::fromtimestamp(epoch_seconds)?;
    let (y, mo, d, h, mi, s, _) = aac_num::civil::utc_fields(seconds.checked_mul(1000)?);
    Some(format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{micros:06}Z"
    ))
}

// ---------------------------------------------------------------------------
// Credential parsing, identity verification, in-memory renewal
// ---------------------------------------------------------------------------

fn has_token_control_or_space(text: &str) -> bool {
    text.bytes().any(|b| b <= 0x20 || b == 0x7f)
}

fn valid_email(email: &str) -> bool {
    if email.is_empty() || email.len() > 254 {
        return false;
    }
    if email.chars().any(char::is_whitespace) {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    let Some((left, right)) = domain.split_once('.') else {
        return false;
    };
    !left.is_empty() && !right.is_empty() && !right.ends_with('.')
}

fn valid_numeric_subject(subject: &str) -> bool {
    (1..=128).contains(&subject.len()) && subject.bytes().all(|b| b.is_ascii_digit())
}

/// Validate an Antigravity consumer OAuth credential payload (`auth_platforms.py:parse_credential`).
pub fn parse_credential(raw: &[u8]) -> Result<ParsedCredential, AuthError> {
    if raw.is_empty() || raw.len() > MAX_CREDENTIAL_BYTES {
        return Err(AuthError::auth("Invalid Antigravity credential size."));
    }
    let text = std::str::from_utf8(raw)
        .map_err(|_| AuthError::auth("Invalid Antigravity credential format."))?;
    let value: Value = serde_json::from_str(text)
        .map_err(|_| AuthError::auth("Invalid Antigravity credential format."))?;
    let Some(obj) = value.as_object() else {
        return Err(AuthError::auth(
            "The saved login is not an Antigravity consumer credential.",
        ));
    };
    if obj.get("auth_method").and_then(Value::as_str) != Some("consumer") {
        return Err(AuthError::auth(
            "The saved login is not an Antigravity consumer credential.",
        ));
    }
    for key in obj.keys() {
        if !matches!(key.as_str(), "auth_method" | "token" | "id_token") {
            return Err(AuthError::auth(
                "Unsupported Antigravity credential fields.",
            ));
        }
    }
    let Some(token) = obj.get("token").and_then(Value::as_object) else {
        return Err(AuthError::auth("Unsupported Antigravity token fields."));
    };
    for key in token.keys() {
        if !matches!(
            key.as_str(),
            "access_token" | "refresh_token" | "token_type" | "expiry"
        ) {
            return Err(AuthError::auth("Unsupported Antigravity token fields."));
        }
    }
    let access_token = match token.get("access_token").and_then(Value::as_str) {
        Some(t)
            if !t.is_empty()
                && t.len() <= MAX_CREDENTIAL_BYTES
                && !has_token_control_or_space(t) =>
        {
            t.to_owned()
        }
        _ => {
            return Err(AuthError::auth(
                "The saved Antigravity login is incomplete.",
            ));
        }
    };
    let refresh_token = match token.get("refresh_token").and_then(Value::as_str) {
        Some(t)
            if !t.is_empty()
                && t.len() <= MAX_CREDENTIAL_BYTES
                && !has_token_control_or_space(t) =>
        {
            t.to_owned()
        }
        _ => {
            return Err(AuthError::auth(
                "The saved Antigravity login is incomplete.",
            ));
        }
    };
    if token.get("token_type").and_then(Value::as_str) != Some("Bearer") {
        return Err(AuthError::auth("Unsupported Antigravity token type."));
    }
    let expiry_str = token
        .get("expiry")
        .and_then(Value::as_str)
        .ok_or_else(|| AuthError::auth("Invalid Antigravity token expiry."))?;
    let (expiry_ms, local_year) = parse_iso_datetime_utc_ms(expiry_str)
        .ok_or_else(|| AuthError::auth("Invalid Antigravity token expiry."))?;
    if !(2000..=2100).contains(&local_year) {
        return Err(AuthError::auth("Invalid Antigravity token expiry."));
    }
    let id_token = match obj.get("id_token") {
        None => None,
        Some(Value::String(s)) if s.len() <= MAX_CREDENTIAL_BYTES => Some(s.clone()),
        Some(_) => return Err(AuthError::auth("Invalid Antigravity identity token.")),
    };
    Ok(ParsedCredential {
        access_token,
        refresh_token,
        expiry: expiry_str.to_owned(),
        expiry_ms,
        id_token,
        raw_object: obj.clone(),
    })
}

/// Verify an Antigravity credential against a userinfo callback (`auth_platforms.py:verify_identity`).
pub fn verify_identity<F>(
    cred: &PrivateCredential,
    userinfo: F,
    now_ms: i64,
) -> Result<VerifiedIdentity, AuthError>
where
    F: FnOnce(&str) -> Result<Value, ProviderCallError>,
{
    let parsed = parse_credential(cred.raw())?;
    let data = userinfo(&parsed.access_token).map_err(|err| match err {
        ProviderCallError::NeedsSignIn => {
            AuthError::needs_sign_in("The saved Antigravity account needs a renewed sign-in.")
        }
        ProviderCallError::RateLimited { .. } | ProviderCallError::Unavailable => {
            AuthError::identity_unavailable("The saved Antigravity identity could not be verified.")
        }
    })?;
    let Some(obj) = data.as_object() else {
        return Err(AuthError::identity_mismatch(
            "Google did not return a verified Antigravity account identity.",
        ));
    };
    let verified_flag = obj
        .get("verified_email")
        .or_else(|| obj.get("email_verified"));
    let email = obj.get("email").and_then(Value::as_str);
    let subject = match obj.get("id") {
        Some(Value::String(s)) if !s.is_empty() => Some(s.as_str()),
        _ => obj.get("sub").and_then(Value::as_str),
    };
    let (Some(email), Some(subject)) = (email, subject) else {
        return Err(AuthError::identity_mismatch(
            "Google did not return a verified Antigravity account identity.",
        ));
    };
    if verified_flag != Some(&Value::Bool(true))
        || !valid_email(email)
        || !valid_numeric_subject(subject)
    {
        return Err(AuthError::identity_mismatch(
            "Google did not return a verified Antigravity account identity.",
        ));
    }
    let stamp = aac_num::py::iso_millis(now_ms).ok_or_else(|| {
        AuthError::identity_unavailable("The saved Antigravity identity could not be verified.")
    })?;
    Ok(VerifiedIdentity {
        email: email.to_lowercase(),
        subject: subject.to_owned(),
        verified_at: stamp,
        authority: "Google OAuth2 userinfo",
        source: "provider-userinfo",
    })
}

/// Renew an access token in memory without touching disk (`auth_platforms.py:refresh_in_memory`).
pub fn refresh_in_memory<F>(
    cred: &PrivateCredential,
    refresh: F,
    now_ms: i64,
) -> Result<PrivateCredential, AuthError>
where
    F: FnOnce(&str) -> Result<Value, ProviderCallError>,
{
    let mut parsed = parse_credential(cred.raw())?;
    let response = refresh(&parsed.refresh_token).map_err(|err| match err {
        ProviderCallError::NeedsSignIn => {
            AuthError::needs_sign_in("The saved Antigravity account needs a renewed sign-in.")
        }
        ProviderCallError::RateLimited { .. } | ProviderCallError::Unavailable => {
            AuthError::identity_unavailable(
                "The saved Antigravity session could not be renewed temporarily.",
            )
        }
    })?;
    let unusable =
        || AuthError::needs_sign_in("The saved Antigravity session returned an unusable renewal.");
    let Some(obj) = response.as_object() else {
        return Err(unusable());
    };
    let Some(access) = obj.get("access_token").and_then(Value::as_str) else {
        return Err(unusable());
    };
    let Some(seconds) = obj
        .get("expires_in")
        .and_then(|v| (!v.is_boolean()).then(|| v.as_f64()).flatten())
    else {
        return Err(unusable());
    };
    if !seconds.is_finite() || !(seconds > 0.0 && seconds <= 86_400.0) {
        return Err(unusable());
    }
    let expiry_iso =
        format_iso_micros_z((now_ms as f64) / 1000.0 + seconds).ok_or_else(unusable)?;
    let Some(token_obj) = parsed
        .raw_object
        .get_mut("token")
        .and_then(Value::as_object_mut)
    else {
        return Err(unusable());
    };
    token_obj.insert("access_token".into(), Value::String(access.to_owned()));
    token_obj.insert("expiry".into(), Value::String(expiry_iso));
    if let Some(rt) = obj.get("refresh_token") {
        token_obj.insert("refresh_token".into(), rt.clone());
    }
    if let Some(id_tok) = obj.get("id_token") {
        parsed.raw_object.insert("id_token".into(), id_tok.clone());
    }
    let encoded = serde_json::to_vec(&Value::Object(parsed.raw_object)).map_err(|_| unusable())?;
    PrivateCredential::new(encoded).map_err(|_| unusable())
}

// ---------------------------------------------------------------------------
// SecureFileStore (Linux RENAME_EXCHANGE verified native fallback file store)
// ---------------------------------------------------------------------------

/// Ubuntu's verified native fallback file store (`auth_platforms.py:SecureFileStore`).
pub struct SecureFileStore {
    home: PathBuf,
    path: PathBuf,
}

impl SecureFileStore {
    /// Construct a store bound to `home/.gemini/antigravity-cli/antigravity-oauth-token`.
    pub fn new(home: &Path) -> Self {
        let abs = std::path::absolute(home).unwrap_or_else(|_| home.to_path_buf());
        let path = abs.join(FALLBACK_RELATIVE);
        Self { home: abs, path }
    }

    /// Read and validate the current fallback credential file.
    pub fn read_current(&self) -> Result<FileSnapshot, AuthError> {
        #[cfg(unix)]
        {
            let parent = self.open_parent()?;
            let name = self
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    AuthError::auth("The Antigravity credential directory is not readable.")
                })?;
            Self::read_at(&parent, name, true)
        }
        #[cfg(not(unix))]
        {
            Err(AuthError::auth(
                "This verified file adapter requires POSIX ownership checks.",
            ))
        }
    }

    /// Atomically exchange `next` into place if the current live snapshot still matches `expected`.
    pub fn install(
        &self,
        next: &PrivateCredential,
        expected: &FileSnapshot,
    ) -> Result<InstallReceipt, AuthError> {
        self.install_with_hook(next, expected, |_| {})
    }

    /// Like [`install`](Self::install), invoking `after_stage` after staging the replacement file
    /// and before the pre-exchange verification and atomic exchange (race test seam).
    pub fn install_with_hook<F>(
        &self,
        next: &PrivateCredential,
        expected: &FileSnapshot,
        after_stage: F,
    ) -> Result<InstallReceipt, AuthError>
    where
        F: FnOnce(&Path),
    {
        #[cfg(target_os = "linux")]
        {
            use rustix::fs::{
                AtFlags, Mode, OFlags, RenameFlags, fsync, openat, renameat_with, unlinkat,
            };

            parse_credential(next.raw())?;
            let parent = self.open_parent()?;
            let parent_path = self.path.parent().ok_or_else(|| {
                AuthError::auth("The Antigravity credential directory is not readable.")
            })?;
            let target_name = self
                .path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| {
                    AuthError::auth("The Antigravity credential directory is not readable.")
                })?;
            let staging = format!(".aac-credential-{}", random_hex_16()?);
            let mut staged_snapshot: Option<FileSnapshot> = None;
            let mut retain = false;

            let outcome = (|| -> Result<InstallReceipt, AuthError> {
                let current = Self::read_at(&parent, target_name, true)?;
                if !Self::same(&current, expected) {
                    return Err(AuthError::auth(
                        "Another process changed the Antigravity login. Review again.",
                    ));
                }
                let fd = openat(
                    &parent,
                    &staging,
                    OFlags::WRONLY
                        | OFlags::CREATE
                        | OFlags::EXCL
                        | OFlags::NOFOLLOW
                        | OFlags::CLOEXEC,
                    Mode::from_raw_mode(0o600),
                )
                .map_err(|_| {
                    AuthError::auth("The Antigravity login could not be installed safely.")
                })?;
                let mut stream = fs::File::from(fd);
                stream
                    .write_all(next.raw())
                    .and_then(|()| stream.flush())
                    .and_then(|()| stream.sync_all())
                    .map_err(|_| {
                        AuthError::auth("The Antigravity login could not be installed safely.")
                    })?;
                drop(stream);

                let staged = Self::read_at(&parent, &staging, true)?;
                staged_snapshot = Some(staged.clone());

                after_stage(parent_path);

                let before_exchange = Self::read_at(&parent, target_name, true)?;
                if !Self::same(&before_exchange, expected) {
                    return Err(AuthError::auth(
                        "Another process changed the Antigravity login. Review again.",
                    ));
                }

                renameat_with(
                    &parent,
                    &staging,
                    &parent,
                    target_name,
                    RenameFlags::EXCHANGE,
                )
                .map_err(|_| AuthError::auth("Atomic credential exchange failed."))?;

                let displaced = Self::read_at(&parent, &staging, false)?;
                let live = Self::read_at(&parent, target_name, false)?;
                if !Self::same(&displaced, expected) || !Self::same(&live, &staged) {
                    retain = true;
                    if Self::same(&live, &staged)
                        && renameat_with(
                            &parent,
                            &staging,
                            &parent,
                            target_name,
                            RenameFlags::EXCHANGE,
                        )
                        .is_ok()
                        && let (Ok(restored), Ok(still_staged)) = (
                            Self::read_at(&parent, target_name, false),
                            Self::read_at(&parent, &staging, false),
                        )
                        && Self::same(&restored, &displaced)
                        && Self::same(&still_staged, &staged)
                    {
                        retain = false;
                    }
                    return Err(AuthError::auth(
                        "A concurrent login change was preserved. Review again.",
                    ));
                }
                fsync(&parent).map_err(|_| {
                    AuthError::auth("The Antigravity login could not be installed safely.")
                })?;
                Ok(InstallReceipt {
                    before: displaced,
                    installed: live,
                })
            })();

            if !retain
                && let Some(staged) = staged_snapshot.as_ref()
                && let Ok(remaining) = Self::read_at(&parent, &staging, true)
                && (Self::same(&remaining, expected) || Self::same(&remaining, staged))
            {
                let _ = unlinkat(&parent, &staging, AtFlags::empty());
            }

            outcome
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = (next, expected, after_stage);
            Err(AuthError::auth(
                "Atomic credential exchange is unsupported on this host.",
            ))
        }
    }

    /// Restore the displaced snapshot from a receipt, refusing if a foreign writer intervened.
    pub fn rollback(&self, receipt: &InstallReceipt) -> Result<InstallReceipt, AuthError> {
        self.install(&receipt.before.credential, &receipt.installed)
    }

    #[cfg(target_os = "linux")]
    fn same(left: &FileSnapshot, right: &FileSnapshot) -> bool {
        (
            left.device,
            left.inode,
            left.modified_ns,
            left.credential.revision(),
        ) == (
            right.device,
            right.inode,
            right.modified_ns,
            right.credential.revision(),
        )
    }

    #[cfg(unix)]
    fn open_parent(&self) -> Result<std::os::fd::OwnedFd, AuthError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, open};
        use std::os::unix::fs::MetadataExt;

        let uid = crate::path::uid();
        let mut current = self.home.clone();
        for part in ["", ".gemini", "antigravity-cli"] {
            if !part.is_empty() {
                current = current.join(part);
            }
            let info = fs::symlink_metadata(&current).map_err(|_| {
                AuthError::auth("The Antigravity credential directory is not readable.")
            })?;
            if !info.is_dir()
                || info.file_type().is_symlink()
                || info.uid() != uid
                || info.mode() & 0o002 != 0
            {
                return Err(AuthError::auth(
                    "The Antigravity credential directory is not owned safely.",
                ));
            }
        }
        let parent_path = self.path.parent().ok_or_else(|| {
            AuthError::auth("The Antigravity credential directory is not readable.")
        })?;
        let fd = open(
            parent_path,
            OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| AuthError::auth("The Antigravity credential directory is not readable."))?;
        let info = fstat(&fd).map_err(|_| {
            AuthError::auth("The Antigravity credential directory is not readable.")
        })?;
        if info.st_uid != uid || FileType::from_raw_mode(info.st_mode) != FileType::Directory {
            return Err(AuthError::auth(
                "The Antigravity credential directory changed.",
            ));
        }
        Ok(fd)
    }

    #[cfg(unix)]
    fn read_at(
        parent: &std::os::fd::OwnedFd,
        name: &str,
        validate: bool,
    ) -> Result<FileSnapshot, AuthError> {
        use rustix::fs::{FileType, Mode, OFlags, fstat, openat};

        let uid = crate::path::uid();
        let fd = openat(
            parent,
            name,
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::empty(),
        )
        .map_err(|_| AuthError::auth("The protected Antigravity credential is not readable."))?;
        let mut file = fs::File::from(fd);
        let info = fstat(&file).map_err(|_| {
            AuthError::auth("The protected Antigravity credential is not readable.")
        })?;
        if FileType::from_raw_mode(info.st_mode) != FileType::RegularFile
            || info.st_uid != uid
            || (info.st_mode & 0o777) != 0o600
            || info.st_nlink != 1
            || info.st_size <= 0
            || info.st_size > MAX_CREDENTIAL_BYTES as i64
        {
            return Err(AuthError::auth(
                "The Antigravity credential file is not protected safely.",
            ));
        }
        let mut raw = Vec::new();
        (&mut file)
            .take((MAX_CREDENTIAL_BYTES + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|_| {
                AuthError::auth("The protected Antigravity credential is not readable.")
            })?;
        let after = fstat(&file).map_err(|_| {
            AuthError::auth("The protected Antigravity credential is not readable.")
        })?;
        if (
            info.st_dev,
            info.st_ino,
            info.st_mtime,
            info.st_mtime_nsec,
            info.st_size,
        ) != (
            after.st_dev,
            after.st_ino,
            after.st_mtime,
            after.st_mtime_nsec,
            after.st_size,
        ) {
            return Err(AuthError::auth(
                "The Antigravity credential changed while being read.",
            ));
        }
        let cred = if validate {
            PrivateCredential::new(raw)?
        } else {
            PrivateCredential::from_unchecked(raw)
        };
        let modified_ns =
            i128::from(info.st_mtime) * 1_000_000_000 + i128::from(info.st_mtime_nsec);
        Ok(FileSnapshot {
            credential: cred,
            device: info.st_dev as u64,
            inode: info.st_ino as u64,
            modified_ns,
        })
    }
}

#[cfg(target_os = "linux")]
fn random_hex_16() -> Result<String, AuthError> {
    use std::collections::hash_map::RandomState;
    use std::hash::{BuildHasher, Hasher};

    let mut bytes = [0u8; 16];
    let mut filled = false;
    if let Ok(mut urandom) = fs::File::open("/dev/urandom")
        && urandom.read_exact(&mut bytes).is_ok()
    {
        filled = true;
    }
    if !filled {
        for chunk in bytes.as_chunks_mut::<8>().0 {
            let v = RandomState::new().build_hasher().finish().to_le_bytes();
            chunk.copy_from_slice(&v);
        }
    }
    let mut out = String::with_capacity(32);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pure-Rust Ubuntu D-Bus Secret Service probe (zero external dbus crates)
// ---------------------------------------------------------------------------

/// Read Ubuntu Secret Service metadata only; never activates, unlocks, or creates a keyring item.
pub fn probe_ubuntu_backend() -> UbuntuBackendProbe {
    #[cfg(target_os = "linux")]
    {
        let addr = std::env::var("DBUS_SESSION_BUS_ADDRESS").ok();
        let xdg = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from);
        probe_ubuntu_backend_at(
            addr.as_deref(),
            xdg.as_deref(),
            crate::path::uid(),
            Duration::from_secs(5),
        )
    }
    #[cfg(not(target_os = "linux"))]
    {
        UbuntuBackendProbe {
            store: "unavailable",
            service_running: false,
            unlocked_matches: 0,
            locked_matches: 0,
        }
    }
}

/// Probe a specific D-Bus session bus address with explicit UID and timeout.
pub fn probe_ubuntu_backend_at(
    bus_address: Option<&str>,
    xdg_runtime_dir: Option<&Path>,
    uid: u32,
    timeout: Duration,
) -> UbuntuBackendProbe {
    #[cfg(target_os = "linux")]
    {
        match dbus_probe_linux(bus_address, xdg_runtime_dir, uid, timeout) {
            Ok(probe) => probe,
            Err(()) => UbuntuBackendProbe {
                store: "unavailable",
                service_running: false,
                unlocked_matches: 0,
                locked_matches: 0,
            },
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = (bus_address, xdg_runtime_dir, uid, timeout);
        UbuntuBackendProbe {
            store: "unavailable",
            service_running: false,
            unlocked_matches: 0,
            locked_matches: 0,
        }
    }
}

#[cfg(target_os = "linux")]
fn dbus_probe_linux(
    bus_address: Option<&str>,
    xdg_runtime_dir: Option<&Path>,
    uid: u32,
    timeout: Duration,
) -> Result<UbuntuBackendProbe, ()> {
    use std::os::linux::net::SocketAddrExt;
    use std::os::unix::net::{SocketAddr, UnixStream};

    let mut stream = if let Some(addr_str) = bus_address.filter(|s| !s.is_empty()) {
        let mut connected = None;
        for entry in addr_str.split(';') {
            let Some(params) = entry.strip_prefix("unix:") else {
                continue;
            };
            for kv in params.split(',') {
                let Some((k, v)) = kv.split_once('=') else {
                    continue;
                };
                match k {
                    "path" => {
                        let decoded = dbus_unescape(v)?;
                        let path = PathBuf::from(String::from_utf8(decoded).map_err(|_| ())?);
                        if let Ok(s) = UnixStream::connect(path) {
                            connected = Some(s);
                            break;
                        }
                    }
                    "abstract" => {
                        let decoded = dbus_unescape(v)?;
                        if let Ok(sa) = SocketAddr::from_abstract_name(&decoded)
                            && let Ok(s) = UnixStream::connect_addr(&sa)
                        {
                            connected = Some(s);
                            break;
                        }
                    }
                    "runtime" if v == "yes" => {
                        if let Some(dir) = xdg_runtime_dir
                            && let Ok(s) = UnixStream::connect(dir.join("bus"))
                        {
                            connected = Some(s);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if connected.is_some() {
                break;
            }
        }
        connected.ok_or(())?
    } else if let Some(dir) = xdg_runtime_dir {
        UnixStream::connect(dir.join("bus")).map_err(|_| ())?
    } else {
        return Err(());
    };

    stream.set_read_timeout(Some(timeout)).map_err(|_| ())?;
    stream.set_write_timeout(Some(timeout)).map_err(|_| ())?;

    // SASL EXTERNAL auth
    let uid_ascii = uid.to_string();
    let mut uid_hex = String::with_capacity(uid_ascii.len() * 2);
    for b in uid_ascii.as_bytes() {
        use std::fmt::Write as _;
        let _ = write!(uid_hex, "{b:02x}");
    }
    let auth_cmd = format!("\0AUTH EXTERNAL {uid_hex}\r\n");
    stream.write_all(auth_cmd.as_bytes()).map_err(|_| ())?;
    let mut read_buf = Vec::new();
    let auth_line = dbus_read_crlf_line(&mut stream, &mut read_buf)?;
    if !auth_line.starts_with(b"OK ") {
        return Err(());
    }
    stream.write_all(b"BEGIN\r\n").map_err(|_| ())?;

    let mut serial = 1u32;
    let mut call = |dest: &str,
                    path: &str,
                    iface: &str,
                    member: &str,
                    sig: &str,
                    body: &[u8]|
     -> Result<(u8, Vec<u8>), ()> {
        let my_serial = serial;
        serial += 1;
        let msg = dbus_build_call(my_serial, dest, path, iface, member, sig, body);
        stream.write_all(&msg).map_err(|_| ())?;
        for _ in 0..64 {
            let (endian, msg_type, reply_serial, reply_body) =
                dbus_read_message(&mut stream, &mut read_buf)?;
            if reply_serial == Some(my_serial) {
                if msg_type == 2 {
                    return Ok((endian, reply_body));
                }
                return Err(());
            }
        }
        Err(())
    };

    // 1. Hello()
    let _ = call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "Hello",
        "",
        &[],
    )?;

    // 2. NameHasOwner("org.freedesktop.secrets")
    let mut body = Vec::new();
    dbus_write_string(&mut body, "org.freedesktop.secrets");
    let (endian, reply) = call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        "s",
        &body,
    )?;
    if reply.len() < 4 {
        return Err(());
    }
    let has_owner = dbus_read_u32(endian, &reply[..4]) != 0;
    if !has_owner {
        return Ok(UbuntuBackendProbe {
            store: "fallback-file",
            service_running: false,
            unlocked_matches: 0,
            locked_matches: 0,
        });
    }

    // 3. GetNameOwner("org.freedesktop.secrets")
    let (endian, reply) = call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetNameOwner",
        "s",
        &body,
    )?;
    let (owner, _) = dbus_read_string(endian, &reply, 0)?;

    // 4. GetConnectionUnixUser(owner)
    let mut owner_body = Vec::new();
    dbus_write_string(&mut owner_body, &owner);
    let (endian, reply) = call(
        "org.freedesktop.DBus",
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "GetConnectionUnixUser",
        "s",
        &owner_body,
    )?;
    if reply.len() < 4 {
        return Err(());
    }
    let owner_uid = dbus_read_u32(endian, &reply[..4]);
    if owner_uid != uid {
        return Ok(UbuntuBackendProbe {
            store: "unavailable",
            service_running: true,
            unlocked_matches: 0,
            locked_matches: 0,
        });
    }

    // 5. SearchItems({"service": "gemini", "username": "antigravity"})
    let mut search_body = Vec::new();
    let mut entries = Vec::new();
    dbus_write_dict_entry_ss(&mut entries, "service", SERVICE);
    dbus_write_dict_entry_ss(&mut entries, "username", ACCOUNT);
    search_body.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    // Align to 8 bytes for dict-entry array elements (offset 4 -> pad 4 bytes).
    search_body.extend_from_slice(&[0u8; 4]);
    search_body.extend_from_slice(&entries);
    let (endian, reply) = call(
        &owner,
        "/org/freedesktop/secrets",
        "org.freedesktop.Secret.Service",
        "SearchItems",
        "a{ss}",
        &search_body,
    )?;
    let (unlocked, next_pos) = dbus_read_object_path_array(endian, &reply, 0)?;
    let (locked, _) = dbus_read_object_path_array(endian, &reply, next_pos)?;
    let total = unlocked + locked;
    let store = if total == 0 {
        "fallback-file"
    } else if locked > 0 {
        "locked"
    } else if total == 1 {
        "secret-service"
    } else {
        "ambiguous"
    };
    Ok(UbuntuBackendProbe {
        store,
        service_running: true,
        unlocked_matches: unlocked,
        locked_matches: locked,
    })
}

#[cfg(target_os = "linux")]
fn dbus_unescape(input: &str) -> Result<Vec<u8>, ()> {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(());
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).map_err(|_| ())?;
            let val = u8::from_str_radix(hex, 16).map_err(|_| ())?;
            out.push(val);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

#[cfg(target_os = "linux")]
fn dbus_read_crlf_line(
    stream: &mut std::os::unix::net::UnixStream,
    buf: &mut Vec<u8>,
) -> Result<Vec<u8>, ()> {
    loop {
        if let Some(pos) = buf.windows(2).position(|w| w == b"\r\n") {
            let line = buf[..pos].to_vec();
            buf.drain(..pos + 2);
            return Ok(line);
        }
        if buf.len() > 1024 {
            return Err(());
        }
        let mut tmp = [0u8; 256];
        let n = stream.read(&mut tmp).map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        buf.extend_from_slice(&tmp[..n]);
    }
}

#[cfg(target_os = "linux")]
fn dbus_align(buf: &mut Vec<u8>, align: usize) {
    while !buf.len().is_multiple_of(align) {
        buf.push(0);
    }
}

#[cfg(target_os = "linux")]
fn dbus_align_pos(pos: usize, align: usize) -> usize {
    (pos + align - 1) & !(align - 1)
}

#[cfg(target_os = "linux")]
fn dbus_write_string(buf: &mut Vec<u8>, text: &str) {
    dbus_align(buf, 4);
    buf.extend_from_slice(&(text.len() as u32).to_le_bytes());
    buf.extend_from_slice(text.as_bytes());
    buf.push(0);
}

#[cfg(target_os = "linux")]
fn dbus_write_dict_entry_ss(buf: &mut Vec<u8>, key: &str, value: &str) {
    dbus_align(buf, 8);
    dbus_write_string(buf, key);
    dbus_write_string(buf, value);
}

#[cfg(target_os = "linux")]
fn dbus_push_field_str(fields: &mut Vec<u8>, code: u8, sig_char: u8, value: &str) {
    dbus_align(fields, 8);
    fields.push(code);
    fields.push(1);
    fields.push(sig_char);
    fields.push(0);
    dbus_write_string(fields, value);
}

#[cfg(target_os = "linux")]
fn dbus_build_call(
    serial: u32,
    dest: &str,
    path: &str,
    iface: &str,
    member: &str,
    sig: &str,
    body: &[u8],
) -> Vec<u8> {
    let mut fields = Vec::new();
    dbus_push_field_str(&mut fields, 1, b'o', path);
    dbus_push_field_str(&mut fields, 2, b's', iface);
    dbus_push_field_str(&mut fields, 3, b's', member);
    dbus_push_field_str(&mut fields, 6, b's', dest);
    if !sig.is_empty() {
        dbus_align(&mut fields, 8);
        fields.push(8);
        fields.push(1);
        fields.push(b'g');
        fields.push(0);
        fields.push(sig.len() as u8);
        fields.extend_from_slice(sig.as_bytes());
        fields.push(0);
    }
    let mut msg = Vec::with_capacity(16 + fields.len() + 8 + body.len());
    msg.extend_from_slice(&[b'l', 1, 0, 1]);
    msg.extend_from_slice(&(body.len() as u32).to_le_bytes());
    msg.extend_from_slice(&serial.to_le_bytes());
    msg.extend_from_slice(&(fields.len() as u32).to_le_bytes());
    msg.extend_from_slice(&fields);
    dbus_align(&mut msg, 8);
    msg.extend_from_slice(body);
    msg
}

#[cfg(target_os = "linux")]
fn dbus_read_u32(endian: u8, bytes: &[u8]) -> u32 {
    let arr: [u8; 4] = [bytes[0], bytes[1], bytes[2], bytes[3]];
    if endian == b'B' {
        u32::from_be_bytes(arr)
    } else {
        u32::from_le_bytes(arr)
    }
}

#[cfg(target_os = "linux")]
fn dbus_read_exact_into(
    stream: &mut std::os::unix::net::UnixStream,
    buf: &mut Vec<u8>,
    need: usize,
) -> Result<(), ()> {
    while buf.len() < need {
        let mut tmp = [0u8; 1024];
        let n = stream.read(&mut tmp).map_err(|_| ())?;
        if n == 0 {
            return Err(());
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn dbus_read_string(endian: u8, data: &[u8], pos: usize) -> Result<(String, usize), ()> {
    let p = dbus_align_pos(pos, 4);
    if p + 4 > data.len() {
        return Err(());
    }
    let len = dbus_read_u32(endian, &data[p..p + 4]) as usize;
    let start = p + 4;
    let end = start.checked_add(len).ok_or(())?;
    if end >= data.len() || data[end] != 0 {
        return Err(());
    }
    let text = std::str::from_utf8(&data[start..end])
        .map_err(|_| ())?
        .to_owned();
    Ok((text, end + 1))
}

#[cfg(target_os = "linux")]
fn dbus_read_object_path_array(endian: u8, data: &[u8], pos: usize) -> Result<(usize, usize), ()> {
    let p = dbus_align_pos(pos, 4);
    if p + 4 > data.len() {
        return Err(());
    }
    let byte_len = dbus_read_u32(endian, &data[p..p + 4]) as usize;
    let start = p + 4;
    let end = start.checked_add(byte_len).ok_or(())?;
    if end > data.len() {
        return Err(());
    }
    let mut cursor = start;
    let mut count = 0usize;
    while cursor < end {
        let (_, next) = dbus_read_string(endian, data, cursor)?;
        if next > end {
            return Err(());
        }
        count += 1;
        cursor = next;
    }
    Ok((count, end))
}

#[cfg(target_os = "linux")]
fn dbus_read_message(
    stream: &mut std::os::unix::net::UnixStream,
    buf: &mut Vec<u8>,
) -> Result<(u8, u8, Option<u32>, Vec<u8>), ()> {
    dbus_read_exact_into(stream, buf, 16)?;
    let endian = buf[0];
    if endian != b'l' && endian != b'B' {
        return Err(());
    }
    let msg_type = buf[1];
    let body_len = dbus_read_u32(endian, &buf[4..8]) as usize;
    let fields_len = dbus_read_u32(endian, &buf[12..16]) as usize;
    if fields_len > 65_536 || body_len > 1_048_576 {
        return Err(());
    }
    let header_padded = dbus_align_pos(16 + fields_len, 8);
    let total = header_padded.checked_add(body_len).ok_or(())?;
    dbus_read_exact_into(stream, buf, total)?;
    let raw: Vec<u8> = buf.drain(..total).collect();

    // Parse header fields array starting at offset 16, length fields_len.
    let mut reply_serial = None;
    let fields = &raw[16..16 + fields_len];
    let mut pos = 0usize;
    while pos < fields.len() {
        pos = dbus_align_pos(pos, 8);
        if pos >= fields.len() {
            break;
        }
        let code = fields[pos];
        pos += 1;
        if pos >= fields.len() {
            return Err(());
        }
        let sig_len = fields[pos] as usize;
        pos += 1;
        if pos + sig_len >= fields.len() {
            return Err(());
        }
        let sig = &fields[pos..pos + sig_len];
        pos += sig_len + 1; // skip NUL
        match sig {
            b"u" => {
                pos = dbus_align_pos(pos, 4);
                if pos + 4 > fields.len() {
                    return Err(());
                }
                let val = dbus_read_u32(endian, &fields[pos..pos + 4]);
                pos += 4;
                if code == 5 {
                    reply_serial = Some(val);
                }
            }
            b"s" | b"o" => {
                let (_, next) = dbus_read_string(endian, fields, pos)?;
                pos = next;
            }
            b"g" => {
                if pos >= fields.len() {
                    return Err(());
                }
                let glen = fields[pos] as usize;
                pos = pos.checked_add(1 + glen + 1).ok_or(())?;
                if pos > fields.len() {
                    return Err(());
                }
            }
            _ => return Err(()),
        }
    }
    let body = raw[header_padded..total].to_vec();
    Ok((endian, msg_type, reply_serial, body))
}

// ---------------------------------------------------------------------------
// UbuntuFileStore (fallback file store guarded by UbuntuBackendProbe)
// ---------------------------------------------------------------------------

/// Production Ubuntu fallback store with pre/post native-backend guard (`auth_platforms.py:UbuntuFileStore`).
pub struct UbuntuFileStore<P = fn() -> UbuntuBackendProbe> {
    file: SecureFileStore,
    probe: P,
}

impl UbuntuFileStore<fn() -> UbuntuBackendProbe> {
    /// Construct a production store using the live D-Bus probe.
    pub fn new(home: &Path) -> Self {
        Self {
            file: SecureFileStore::new(home),
            probe: probe_ubuntu_backend,
        }
    }
}

impl<P: Fn() -> UbuntuBackendProbe> UbuntuFileStore<P> {
    /// Construct a store with a custom backend probe (for tests).
    pub fn with_probe(home: &Path, probe: P) -> Self {
        Self {
            file: SecureFileStore::new(home),
            probe,
        }
    }

    fn guard(&self) -> Result<(), AuthError> {
        if (self.probe)().store != "fallback-file" {
            return Err(AuthError::auth(
                "The native Antigravity credential backend changed. Review again.",
            ));
        }
        Ok(())
    }

    /// Read the current fallback credential, probing the backend before and after.
    pub fn read_current(&self) -> Result<FileSnapshot, AuthError> {
        self.guard()?;
        let result = self.file.read_current()?;
        self.guard()?;
        Ok(result)
    }

    /// Install a replacement credential, rolling back if a Secret Service item appears mid-install.
    pub fn install(
        &self,
        next: &PrivateCredential,
        expected: &FileSnapshot,
    ) -> Result<InstallReceipt, AuthError> {
        self.guard()?;
        let receipt = self.file.install(next, expected)?;
        if self.guard().is_err() {
            let _ = self.file.rollback(&receipt);
            return Err(AuthError::auth(
                "A native Antigravity login changed during activation; the file update was rolled back.",
            ));
        }
        Ok(receipt)
    }

    /// Roll back a previous install receipt directly through the file store.
    pub fn rollback(&self, receipt: &InstallReceipt) -> Result<InstallReceipt, AuthError> {
        self.file.rollback(receipt)
    }
}

// ---------------------------------------------------------------------------
// Native credential worker protocol (`native_credential_worker.py`)
// ---------------------------------------------------------------------------

fn store_unavailable() -> AuthError {
    AuthError::auth("antigravity-private-store-unavailable")
}

fn decode_credential_b64(value: &Value) -> Result<PrivateCredential, AuthError> {
    let Some(s) = value.as_str() else {
        return Err(store_unavailable());
    };
    if s.len() > MAX_BASE64_CREDENTIAL_LEN {
        return Err(store_unavailable());
    }
    let raw = STANDARD.decode(s).map_err(|_| store_unavailable())?;
    if raw.is_empty() || raw.len() > MAX_CREDENTIAL_BYTES {
        return Err(store_unavailable());
    }
    PrivateCredential::new(raw).map_err(|_| store_unavailable())
}

fn check_fingerprint(value: &Value) -> Result<&str, AuthError> {
    let Some(s) = value.as_str() else {
        return Err(store_unavailable());
    };
    if s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        Ok(s)
    } else {
        Err(store_unavailable())
    }
}

fn snapshot_to_dto(value: &FileSnapshot) -> Value {
    json!({
        "device": value.device.to_string(),
        "inode": value.inode.to_string(),
        "modifiedNs": value.modified_ns.to_string(),
        "credentialBase64": STANDARD.encode(value.credential.raw()),
    })
}

fn parse_snapshot_dto(value: &Value) -> Result<FileSnapshot, AuthError> {
    let Some(obj) = value.as_object() else {
        return Err(store_unavailable());
    };
    let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = ["device", "inode", "modifiedNs", "credentialBase64"]
        .into_iter()
        .collect();
    if keys != expected {
        return Err(store_unavailable());
    }
    let decimal_str = |key: &str| -> Result<&str, AuthError> {
        let s = obj
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(store_unavailable)?;
        if (1..=24).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) {
            Ok(s)
        } else {
            Err(store_unavailable())
        }
    };
    let device: u64 = decimal_str("device")?
        .parse()
        .map_err(|_| store_unavailable())?;
    let inode: u64 = decimal_str("inode")?
        .parse()
        .map_err(|_| store_unavailable())?;
    let modified_ns: i128 = decimal_str("modifiedNs")?
        .parse()
        .map_err(|_| store_unavailable())?;
    let cred = decode_credential_b64(&obj["credentialBase64"])?;
    Ok(FileSnapshot {
        credential: cred,
        device,
        inode,
        modified_ns,
    })
}

/// Execute one native credential worker operation (`bind-current`, `read`, `install`, `rollback`).
pub fn execute_native_packet<P, U>(
    packet: &Value,
    store: &UbuntuFileStore<P>,
    userinfo: U,
    now_ms: i64,
) -> Result<Value, AuthError>
where
    P: Fn() -> UbuntuBackendProbe,
    U: FnOnce(&str) -> Result<Value, ProviderCallError>,
{
    let Some(obj) = packet.as_object() else {
        return Err(store_unavailable());
    };
    let keys: BTreeSet<&str> = obj.keys().map(String::as_str).collect();
    let op = obj.get("operation").and_then(Value::as_str);
    match op {
        Some("bind-current") if keys.len() == 1 => {
            let before = store.read_current()?;
            let identity = verify_identity(&before.credential, userinfo, now_ms)?;
            let after = store.read_current()?;
            if (
                before.device,
                before.inode,
                before.modified_ns,
                before.credential.revision(),
            ) != (
                after.device,
                after.inode,
                after.modified_ns,
                after.credential.revision(),
            ) {
                return Err(store_unavailable());
            }
            let mod_ns_json: Value = i64::try_from(after.modified_ns)
                .map(Value::from)
                .unwrap_or_else(|_| Value::String(after.modified_ns.to_string()));
            Ok(json!({
                "email": identity.email,
                "subject": identity.subject,
                "source": identity.authority,
                "verifiedAt": identity.verified_at,
                "credentialFingerprint": after.credential.revision(),
                "credentialCurrent": true,
                "nativeSnapshotIdentity": [
                    after.device,
                    after.inode,
                    mod_ns_json,
                    after.credential.revision(),
                ],
            }))
        }
        Some("read") if keys.len() == 1 => {
            let value = store.read_current()?.credential;
            Ok(json!({
                "format": CREDENTIAL_FORMAT,
                "credentialBase64": STANDARD.encode(value.raw()),
                "revision": value.revision(),
            }))
        }
        Some("install")
            if keys == BTreeSet::from(["operation", "credentialBase64", "expectedFingerprint"]) =>
        {
            let expected = check_fingerprint(&obj["expectedFingerprint"])?;
            let replacement = decode_credential_b64(&obj["credentialBase64"])?;
            let before = store.read_current()?;
            if before.credential.revision() != expected {
                return Err(store_unavailable());
            }
            let installed = store.install(&replacement, &before)?;
            Ok(json!({
                "installedFingerprint": installed.installed.credential.revision(),
                "rollbackState": {
                    "before": snapshot_to_dto(&installed.before),
                    "installed": snapshot_to_dto(&installed.installed),
                },
            }))
        }
        Some("rollback")
            if keys == BTreeSet::from(["operation", "receipt", "previousCredentialBase64"]) =>
        {
            let receipt_obj = obj
                .get("receipt")
                .and_then(Value::as_object)
                .ok_or_else(store_unavailable)?;
            let rkeys: BTreeSet<&str> = receipt_obj.keys().map(String::as_str).collect();
            if rkeys != BTreeSet::from(["before", "installed"]) {
                return Err(store_unavailable());
            }
            let before = parse_snapshot_dto(&receipt_obj["before"])?;
            let installed = parse_snapshot_dto(&receipt_obj["installed"])?;
            let previous = decode_credential_b64(&obj["previousCredentialBase64"])?;
            if previous.revision() != before.credential.revision() {
                return Err(store_unavailable());
            }
            let restored = store.rollback(&InstallReceipt { before, installed })?;
            Ok(json!({
                "restored": restored.installed.credential.revision() == previous.revision(),
            }))
        }
        _ => Err(store_unavailable()),
    }
}

/// Strict bounded byte-stream entry point for the native credential worker protocol.
pub fn execute_native_bytes<P, U>(
    raw: &[u8],
    store: &UbuntuFileStore<P>,
    userinfo: U,
    now_ms: i64,
) -> Result<Vec<u8>, AuthError>
where
    P: Fn() -> UbuntuBackendProbe,
    U: FnOnce(&str) -> Result<Value, ProviderCallError>,
{
    if raw.len() > MAX_PACKET_BYTES {
        return Err(store_unavailable());
    }
    let map = parse_strict_json_object(raw).map_err(|_| store_unavailable())?;
    let result = execute_native_packet(&Value::Object(map), store, userinfo, now_ms)?;
    let out = serde_json::to_vec(&result).map_err(|_| store_unavailable())?;
    if out.len() > MAX_PACKET_BYTES {
        return Err(store_unavailable());
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Private installed-app OAuth client reader (`desktop_usage.py:antigravity_oauth_client`)
// ---------------------------------------------------------------------------

const OAUTH_UNAVAILABLE: &str =
    "Antigravity's private app OAuth metadata is unavailable on this computer.";

fn valid_oauth_client_id(value: &str) -> bool {
    let Some(prefix) = value.strip_suffix(".apps.googleusercontent.com") else {
        return false;
    };
    (1..=256).contains(&prefix.len())
        && prefix
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn valid_oauth_client_secret(value: &str) -> bool {
    (1..=256).contains(&value.len())
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

#[cfg(windows)]
fn windows_private_metadata(path: &Path) -> bool {
    let literal = path.to_string_lossy().replace('\'', "''");
    let script = format!(
        "$ErrorActionPreference='Stop';\
         $sid=[Security.Principal.WindowsIdentity]::GetCurrent().User.Value;\
         $acl=Get-Acl -LiteralPath '{literal}';\
         if(-not $acl.AreAccessRulesProtected){{exit 1}};\
         $owner=$acl.GetOwner([Security.Principal.SecurityIdentifier]).Value;\
         if($owner -ne $sid -and $owner -ne 'S-1-5-32-544'){{exit 1}};\
         $rules=$acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]);\
         if($rules.Count -gt 16){{exit 1}};\
         foreach($rule in $rules){{\
         if($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -ne $sid \
         -and $rule.IdentityReference.Value -ne 'S-1-5-18'){{exit 1}}}};\
         [Console]::Write('private')"
    );
    std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
        .is_ok_and(|o| o.status.success() && o.stdout == b"private")
}

/// Read `<home>/.ccs/account-usage/antigravity-oauth-client.json` with strict permission,
/// symlink, duplicate-key, and integer `schemaVersion` checks.
pub fn read_antigravity_oauth_client(home: &Path) -> Result<OAuthClient, AuthError> {
    let path = home.join(OAUTH_CLIENT_RELATIVE);
    let fail = || AuthError::auth(OAUTH_UNAVAILABLE);
    let parent = path.parent().ok_or_else(fail)?;
    if fs::symlink_metadata(&path)
        .map_err(|_| fail())?
        .file_type()
        .is_symlink()
        || fs::symlink_metadata(parent)
            .map_err(|_| fail())?
            .file_type()
            .is_symlink()
    {
        return Err(fail());
    }
    #[cfg(windows)]
    if !windows_private_metadata(&path) {
        return Err(fail());
    }
    let mut raw = Vec::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
        let mut opts = fs::OpenOptions::new();
        opts.read(true).custom_flags(libc::O_NOFOLLOW);
        let mut file = opts.open(&path).map_err(|_| fail())?;
        let meta = file.metadata().map_err(|_| fail())?;
        if !meta.is_file()
            || meta.len() > MAX_OAUTH_CLIENT_BYTES as u64
            || meta.uid() != crate::path::uid()
            || (meta.mode() & 0o777) != 0o600
        {
            return Err(fail());
        }
        (&mut file)
            .take((MAX_OAUTH_CLIENT_BYTES + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|_| fail())?;
    }
    #[cfg(not(unix))]
    {
        let mut file = fs::File::open(&path).map_err(|_| fail())?;
        let meta = file.metadata().map_err(|_| fail())?;
        if !meta.is_file() || meta.len() > MAX_OAUTH_CLIENT_BYTES as u64 {
            return Err(fail());
        }
        (&mut file)
            .take((MAX_OAUTH_CLIENT_BYTES + 1) as u64)
            .read_to_end(&mut raw)
            .map_err(|_| fail())?;
    }
    if raw.len() > MAX_OAUTH_CLIENT_BYTES {
        return Err(fail());
    }
    let map = parse_strict_json_object(&raw).map_err(|_| fail())?;
    let keys: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    let schema_val = if keys == BTreeSet::from(["schemaVersion", "clientId", "clientSecret"]) {
        &map["schemaVersion"]
    } else if keys == BTreeSet::from(["schema", "clientId", "clientSecret"]) {
        &map["schema"]
    } else {
        return Err(fail());
    };
    // Must be an integer 1, NOT float 1.0 (serde_json distinguishes u64/i64 from f64).
    let is_int_one = match schema_val {
        Value::Number(n) => {
            (n.is_u64() && n.as_u64() == Some(1)) || (n.is_i64() && n.as_i64() == Some(1))
        }
        _ => false,
    };
    if !is_int_one {
        return Err(fail());
    }
    let client_id = map
        .get("clientId")
        .and_then(Value::as_str)
        .filter(|s| valid_oauth_client_id(s))
        .ok_or_else(fail)?;
    let client_secret = map
        .get("clientSecret")
        .and_then(Value::as_str)
        .filter(|s| valid_oauth_client_secret(s))
        .ok_or_else(fail)?;
    Ok(OAuthClient {
        client_id: client_id.to_owned(),
        client_secret: client_secret.to_owned(),
    })
}

// ---------------------------------------------------------------------------
// Masked credential inspection & read (`signin_driver.py` + `signin-sandbox.ts`)
// ---------------------------------------------------------------------------

/// Inspect the staged OAuth token file without following links or leaking its contents.
pub fn masked_credential_state(token_path: &Path) -> MaskedCredentialState {
    let Ok(before) = fs::symlink_metadata(token_path) else {
        return MaskedCredentialState::Absent;
    };
    if !before.is_file() || before.file_type().is_symlink() {
        return MaskedCredentialState::Partial;
    }
    #[cfg(unix)]
    let open_res = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(token_path)
    };
    #[cfg(not(unix))]
    let open_res = fs::File::open(token_path);

    let Ok(mut file) = open_res else {
        return MaskedCredentialState::Partial;
    };
    let Ok(opened) = file.metadata() else {
        return MaskedCredentialState::Partial;
    };
    if !opened.is_file() || opened.len() == 0 || opened.len() > MAX_CREDENTIAL_BYTES as u64 {
        return MaskedCredentialState::Partial;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != before.dev() || opened.ino() != before.ino() {
            return MaskedCredentialState::Partial;
        }
    }
    let mut raw = Vec::new();
    if (&mut file)
        .take((MAX_CREDENTIAL_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .is_err()
        || raw.is_empty()
        || raw.len() > MAX_CREDENTIAL_BYTES
    {
        return MaskedCredentialState::Partial;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        return MaskedCredentialState::Partial;
    };
    let complete = value.get("auth_method").and_then(Value::as_str) == Some("consumer")
        && value
            .get("token")
            .and_then(|t| t.get("access_token"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty())
        && value
            .get("token")
            .and_then(|t| t.get("refresh_token"))
            .and_then(Value::as_str)
            .is_some_and(|s| !s.is_empty());
    if !complete {
        return MaskedCredentialState::Partial;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.nlink() != 1
            || (opened.mode() & 0o777) != 0o600
            || opened.uid() != crate::path::uid()
        {
            return MaskedCredentialState::Unsafe;
        }
    }
    MaskedCredentialState::Complete
}

/// Read the staged OAuth token bytes with exact `readMaskedCredential` guards:
/// owned `0600` regular file, `nlink == 1`, unchanged `(dev, ino)`, `1..=16384` bytes.
pub fn read_masked_credential(token_path: &Path) -> Result<Vec<u8>, AuthError> {
    let fail = || AuthError::auth("The staged Antigravity credential could not be read safely.");
    let before = fs::symlink_metadata(token_path).map_err(|_| fail())?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err(fail());
    }
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(token_path)
            .map_err(|_| fail())?
    };
    #[cfg(not(unix))]
    let mut file = fs::File::open(token_path).map_err(|_| fail())?;

    let opened = file.metadata().map_err(|_| fail())?;
    if !opened.is_file() || opened.len() == 0 || opened.len() > MAX_CREDENTIAL_BYTES as u64 {
        return Err(fail());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if opened.dev() != before.dev()
            || opened.ino() != before.ino()
            || opened.nlink() != 1
            || (opened.mode() & 0o777) != 0o600
            || opened.uid() != crate::path::uid()
        {
            return Err(fail());
        }
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take((MAX_CREDENTIAL_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| fail())?;
    if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_BYTES {
        return Err(fail());
    }
    Ok(bytes)
}

// ---------------------------------------------------------------------------
// Runtime bundle health (`scripts/antigravity/runtime_health.py`)
// ---------------------------------------------------------------------------

/// Read-only inspection and formatting for installed runtime bundle parser health.
pub mod runtime_health {
    use super::*;

    /// Pinned parser packages and their required exact versions.
    pub const PARSER_PINS: [(&str, &str); 2] = [("pyte", "0.8.2"), ("wcwidth", "0.9.1")];

    const BROKEN_REASONS: [&str; 3] = [
        "missing-python-module",
        "parser-mismatch",
        "parser-import-failed",
    ];

    fn valid_module(text: &str) -> bool {
        let b = text.as_bytes();
        (1..=64).contains(&b.len())
            && (b[0].is_ascii_alphabetic() || b[0] == b'_')
            && b.iter()
                .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'.')
    }

    fn valid_minor(text: &str) -> bool {
        let Some((major, minor)) = text.split_once('.') else {
            return false;
        };
        (1..=2).contains(&major.len())
            && major.bytes().all(|b| b.is_ascii_digit())
            && (1..=3).contains(&minor.len())
            && minor.bytes().all(|b| b.is_ascii_digit())
    }

    /// The Python `major.minor` version recorded in `<bundle>/venv/pyvenv.cfg`, if present.
    pub fn built_for(bundle: &Path) -> Option<String> {
        let mut file = fs::File::open(bundle.join("venv/pyvenv.cfg")).ok()?;
        let mut raw = Vec::new();
        (&mut file).take(4096).read_to_end(&mut raw).ok()?;
        let text = String::from_utf8_lossy(&raw);
        for line in text.lines() {
            let trimmed = line.trim_start();
            let rest = if let Some(r) = trimmed.strip_prefix("version_info") {
                r
            } else if let Some(r) = trimmed.strip_prefix("version") {
                r
            } else {
                continue;
            };
            let rest = rest.trim_start();
            let Some(after_eq) = rest.strip_prefix('=') else {
                continue;
            };
            let val = after_eq.trim_start();
            let mut parts = val.split('.');
            let major = parts.next()?;
            let minor_raw = parts.next()?;
            let minor: String = minor_raw
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            let candidate = format!("{major}.{minor}");
            if valid_minor(&candidate) {
                return Some(candidate);
            }
        }
        None
    }

    /// Directory where the bundle's parser packages reside (`<bundle>/parser` or
    /// `<bundle>/venv/lib/pythonX.Y/site-packages`).
    pub fn parser_directory(bundle: &Path, version_info: Option<(u32, u32)>) -> PathBuf {
        let neutral = bundle.join("parser");
        if neutral.is_dir() {
            return neutral;
        }
        let (major, minor) = version_info.unwrap_or((3, 12));
        bundle
            .join("venv/lib")
            .join(format!("python{major}.{minor}"))
            .join("site-packages")
    }

    /// Produce a bounded copy of a probe result; unexpected inputs become `probe-failed`.
    pub fn normalize(value: Option<&Value>) -> Value {
        let Some(obj) = value.and_then(Value::as_object) else {
            return json!({
                "ok": false,
                "reason": "probe-failed",
                "module": Value::Null,
                "python": Value::Null,
                "builtFor": Value::Null,
            });
        };
        let ok = obj.get("ok").and_then(Value::as_bool) == Some(true);
        let reason = if ok {
            Value::Null
        } else {
            let r = obj
                .get("reason")
                .and_then(Value::as_str)
                .filter(|r| BROKEN_REASONS.contains(r))
                .unwrap_or("probe-failed");
            Value::String(r.to_owned())
        };
        let module = obj
            .get("module")
            .and_then(Value::as_str)
            .filter(|m| valid_module(m))
            .map_or(Value::Null, |m| Value::String(m.to_owned()));
        let python = obj
            .get("python")
            .and_then(Value::as_str)
            .filter(|p| valid_minor(p))
            .map_or(Value::Null, |p| Value::String(p.to_owned()));
        let built_for = obj
            .get("builtFor")
            .and_then(Value::as_str)
            .filter(|b| valid_minor(b))
            .map_or(Value::Null, |b| Value::String(b.to_owned()));
        json!({
            "ok": ok,
            "reason": reason,
            "module": module,
            "python": python,
            "builtFor": built_for,
        })
    }

    /// Human-readable description of a normalized runtime-health result.
    pub fn describe(result: &Value) -> String {
        if result.get("ok").and_then(Value::as_bool) == Some(true) {
            return "the parser loads".to_owned();
        }
        let module = result
            .get("module")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("unknown");
        let mut text = match result.get("reason").and_then(Value::as_str) {
            Some("missing-python-module") => format!("missing Python module {module}"),
            Some("parser-mismatch") => format!("Python module {module} is not the pinned version"),
            Some("parser-import-failed") => format!("Python module {module} does not load"),
            _ => return "the parser check could not run".to_owned(),
        };
        if let (Some(python), Some(built)) = (
            result.get("python").and_then(Value::as_str),
            result.get("builtFor").and_then(Value::as_str),
        ) && !python.is_empty()
            && !built.is_empty()
            && python != built
        {
            text.push_str(&format!(
                " (system Python is {python}; the bundle was built for {built})"
            ));
        }
        text
    }

    fn parse_minor_pair(text: &str) -> Option<(u32, u32)> {
        let (a, b) = text.split_once('.')?;
        Some((a.parse().ok()?, b.parse().ok()?))
    }

    fn dist_info_version(dir: &Path, package: &str) -> Option<String> {
        let entries = fs::read_dir(dir).ok()?;
        let prefix = format!("{}-", package.to_ascii_lowercase());
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name_str) = name.to_str() else {
                continue;
            };
            let lower = name_str.to_ascii_lowercase();
            if lower.starts_with(&prefix) && lower.ends_with(".dist-info") {
                let meta_path = entry.path().join("METADATA");
                let mut file = match fs::File::open(meta_path) {
                    Ok(f) => f,
                    Err(_) => continue,
                };
                let mut raw = Vec::new();
                if (&mut file).take(8192).read_to_end(&mut raw).is_err() {
                    continue;
                }
                let text = String::from_utf8_lossy(&raw);
                for line in text.lines() {
                    if let Some(ver) = line.strip_prefix("Version:") {
                        return Some(ver.trim().to_owned());
                    }
                }
            }
        }
        None
    }

    fn module_present(dir: &Path, package: &str) -> bool {
        dir.join(package).join("__init__.py").is_file()
            || dir.join(format!("{package}.py")).is_file()
    }

    /// Pure-Rust read-only inspection of a bundle's pinned parser files (spawns no Python).
    pub fn check_bundle(bundle: &Path, python: Option<&str>) -> Value {
        let built = built_for(bundle);
        let py_str = python
            .filter(|p| valid_minor(p))
            .map(str::to_owned)
            .or_else(|| built.clone());
        let ver_pair = py_str.as_deref().and_then(parse_minor_pair);
        let dir = parser_directory(bundle, ver_pair);
        if !dir.is_dir() {
            return normalize(Some(&json!({
                "ok": false,
                "reason": "missing-python-module",
                "module": PARSER_PINS[0].0,
                "python": py_str,
                "builtFor": built,
            })));
        }
        for (name, pinned_version) in PARSER_PINS {
            if !module_present(&dir, name) {
                return normalize(Some(&json!({
                    "ok": false,
                    "reason": "missing-python-module",
                    "module": name,
                    "python": py_str,
                    "builtFor": built,
                })));
            }
            let installed = dist_info_version(&dir, name);
            if installed.as_deref() != Some(pinned_version) {
                return normalize(Some(&json!({
                    "ok": false,
                    "reason": "parser-mismatch",
                    "module": name,
                    "python": py_str,
                    "builtFor": built,
                })));
            }
        }
        normalize(Some(&json!({
            "ok": true,
            "reason": Value::Null,
            "module": Value::Null,
            "python": py_str,
            "builtFor": built,
        })))
    }
}

/// Remove a private file inside `antigravity-signin` or a staging directory helper if needed.
#[allow(dead_code)]
pub(crate) fn _ensure_io_import(_: io::ErrorKind) {}
