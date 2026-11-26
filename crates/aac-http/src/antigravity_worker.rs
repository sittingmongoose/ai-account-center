//! Native Rust ports of `scripts/antigravity/quota_adapter.py`,
//! `scripts/antigravity/quota_worker.py`, `scripts/antigravity/native_status_userinfo.py`,
//! and the Antigravity quota normalization helpers from `scripts/account-usage/desktop_usage.py`
//! and `scripts/account-usage/desktop_helpers.py`.

use crate::antigravity_quota::QuotaRequest;
use crate::antigravity_runtime::{LiveIdentity, sha256_hex};
use aac_files::antigravity_store::{
    AuthError, AuthErrorKind, PrivateCredential, ProviderCallError, parse_credential,
    parse_iso_datetime_utc_ms, parse_strict_json_object, read_antigravity_oauth_client,
    refresh_in_memory, verify_identity,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Google OAuth2 userinfo endpoint (`native_status_userinfo.py`).
pub const USERINFO_ENDPOINT: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
/// Google OAuth2 token refresh endpoint.
pub const TOKEN_ENDPOINT: &str = "https://oauth2.googleapis.com/token";
/// Production Antigravity app quota API base URL.
pub const AGY_BASE: &str = "https://cloudcode-pa.googleapis.com/v1internal:";
/// Daily Antigravity CLI (T3) quota API base URL.
pub const AGY_CLI_BASE: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal:";
/// Native client User-Agent required by Google's Antigravity quota control plane.
pub const AGY_USER_AGENT: &str = "antigravity/hub/2.24.0 (aidev_client; os_type=linux; arch=amd64)";
/// Monotonic elapsed-seconds ceiling before starting the second required service summary.
pub const AGY_REQUIRED_BUDGET_SECONDS: f64 = 14.0;
/// Monotonic elapsed-seconds ceiling before starting an optional per-model Gemini read.
pub const AGY_OPTIONAL_BUDGET_SECONDS: f64 = 13.0;

const USERINFO_MAX_BYTES: usize = 16_384;
const USERINFO_TIMEOUT_SECS: u64 = 5;
const HTTP_MAX_BYTES: usize = 65_536;
const HTTP_TIMEOUT_SECS: u64 = 8;
const MAX_WORKER_REQUEST_BYTES: usize = 65_536;
const MAX_WORKER_RESPONSE_BYTES: usize = 65_536;
const MAX_BASE64_CREDENTIAL_LEN: usize = 22_000;
const GEMINI_RESET_CLUSTER_SECONDS: i64 = 10;

/// Saved profile descriptor bound to its immutable credential bytes (`quota_adapter.py:SavedProfile`).
#[derive(Clone, Debug)]
pub struct SavedProfile {
    /// Saved profile identifier.
    pub id: String,
    /// Saved account email.
    pub email: String,
    /// Stable SHA-256 identity key.
    pub identity_key: String,
    /// Expected SHA-256 revision of the credential bytes.
    pub credential_revision: String,
    /// Immutable private credential.
    pub credential: PrivateCredential,
}

/// Safe transport/collector error carrying no upstream response bodies (`desktop_helpers.py:UsageError`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QuotaTransportError {
    /// Logical status (`"needs_sign_in"`, `"unavailable"`, `"rate_limited"`, or `"error"`).
    pub status: &'static str,
    /// Fixed safe description.
    pub message: &'static str,
    /// Optional machine failure code (e.g. `"rate_limited"`).
    pub failure_code: Option<&'static str>,
    /// Optional bounded Retry-After interval in seconds.
    pub retry_after: Option<u64>,
}

impl QuotaTransportError {
    /// Build a transport error without rate-limit metadata.
    pub fn new(status: &'static str, message: &'static str) -> Self {
        Self {
            status,
            message,
            failure_code: None,
            retry_after: None,
        }
    }

    /// Build a bounded HTTP 429 rate-limit error.
    pub fn rate_limited(retry_after: u64) -> Self {
        Self {
            status: "error",
            message: "The usage service is limiting requests. Refresh resumes after a cooldown.",
            failure_code: Some("rate_limited"),
            retry_after: Some(retry_after),
        }
    }

    /// Whether this error represents a rate limit.
    pub fn is_rate_limited(&self) -> bool {
        self.status == "rate_limited" || self.failure_code == Some("rate_limited")
    }

    /// Validated `retry_after` in `1..=86400`.
    pub fn valid_retry_after(&self) -> Option<u64> {
        self.retry_after.filter(|&s| (1..=86_400).contains(&s))
    }

    fn as_provider_error(&self) -> ProviderCallError {
        if self.status == "needs_sign_in" {
            ProviderCallError::NeedsSignIn
        } else if self.is_rate_limited() {
            ProviderCallError::RateLimited {
                retry_after: self.valid_retry_after(),
            }
        } else {
            ProviderCallError::Unavailable
        }
    }
}

/// Dependency seam for [`collect_snapshot`] and [`handle_worker_bytes`].
pub trait QuotaDeps {
    /// Call Google OAuth2 userinfo with the bearer access token.
    fn userinfo(&self, access: &str) -> Result<Value, QuotaTransportError>;
    /// Exchange a refresh token for a fresh access token.
    fn refresh(&self, refresh_token: &str) -> Result<Value, QuotaTransportError>;
    /// Collect normalized quota windows, pools, plan, and optional retryAfterSeconds.
    fn quota(&self, access: &str, email: &str) -> Result<Value, QuotaTransportError>;
    /// Current UTC epoch milliseconds.
    fn now_ms(&self) -> i64;
}

/// HTTP JSON requester seam for [`antigravity_load`], [`antigravity_services`], and [`CollectorDeps`].
pub trait JsonRequester {
    /// Perform a bounded GET/POST JSON or form-encoded HTTP request.
    fn request_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
        form: bool,
    ) -> Result<Value, QuotaTransportError>;
}

/// Per-service Antigravity quota read result (`desktop_usage.py:antigravity_services`).
#[derive(Clone, Debug)]
pub struct ServiceResult {
    /// `"cli"` or `"app"`.
    pub service: &'static str,
    /// API base URL used for this service.
    pub base: &'static str,
    /// `"ok"`, `"unavailable"`, or `"rate_limited"`.
    pub status: &'static str,
    /// Raw provider quota groups (for [`structural_pools`]).
    pub groups: Vec<Value>,
    /// Normalized windows (or unavailable placeholders).
    pub windows: Vec<Value>,
    /// Optional bounded Retry-After seconds from a summary or per-model 429.
    pub retry_after: Option<u64>,
}

/// Live HTTP JSON requester using `reqwest::blocking`.
pub struct LiveHttpRequester;

impl JsonRequester for LiveHttpRequester {
    fn request_json(
        &self,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<&Value>,
        form: bool,
    ) -> Result<Value, QuotaTransportError> {
        request_json_http(url, headers, body, form)
    }
}

// ---------------------------------------------------------------------------
// Bounded HTTP transports (`native_status_userinfo.py` + `desktop_helpers.py:request_json`)
// ---------------------------------------------------------------------------

/// Clamp an HTTP `Retry-After` header (integer seconds or HTTP-date) to `300..=86400`, defaulting to `600`.
pub fn retry_after_seconds(header: Option<&str>, now_secs: f64) -> u64 {
    let mut seconds: Option<f64> = None;
    if let Some(raw) = header
        && raw.len() <= 128
    {
        let trimmed = raw.trim();
        if (1..=12).contains(&trimmed.len()) && trimmed.bytes().all(|b| b.is_ascii_digit()) {
            if let Ok(n) = trimmed.parse::<u64>() {
                seconds = Some(n as f64);
            }
        } else if let Ok(instant) = httpdate::parse_http_date(trimmed)
            && let Ok(dur) = instant.duration_since(UNIX_EPOCH)
        {
            seconds = Some(dur.as_secs_f64() - now_secs);
        }
    }
    match seconds.filter(|s| s.is_finite()) {
        Some(s) => {
            let ceiled = s.ceil();
            if ceiled <= 300.0 {
                300
            } else if ceiled >= 86_400.0 {
                86_400
            } else {
                ceiled as u64
            }
        }
        None => 600,
    }
}

/// Fixed bounded read-only userinfo HTTP transport (`native_status_userinfo.py:request_userinfo`):
/// 5s timeout, no redirects, no proxy, 16 KiB response cap, strict JSON (no duplicate keys/NaN).
pub fn request_userinfo_http(
    endpoint: &str,
    access_token: &str,
) -> Result<Value, ProviderCallError> {
    if access_token.is_empty() || access_token.len() > USERINFO_MAX_BYTES {
        return Err(ProviderCallError::Unavailable);
    }
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .timeout(Duration::from_secs(USERINFO_TIMEOUT_SECS))
        .build()
        .map_err(|_| ProviderCallError::Unavailable)?;
    let resp = client
        .get(endpoint)
        .header("Authorization", format!("Bearer {access_token}"))
        .send()
        .map_err(|_| ProviderCallError::Unavailable)?;
    let status = resp.status().as_u16();
    if status == 401 || status == 403 {
        return Err(ProviderCallError::NeedsSignIn);
    }
    if !(200..300).contains(&status) {
        return Err(ProviderCallError::Unavailable);
    }
    let mut raw = Vec::new();
    resp.take((USERINFO_MAX_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| ProviderCallError::Unavailable)?;
    if raw.len() > USERINFO_MAX_BYTES {
        return Err(ProviderCallError::Unavailable);
    }
    let map = parse_strict_json_object(&raw).map_err(|_| ProviderCallError::Unavailable)?;
    Ok(Value::Object(map))
}

/// Fixed bounded JSON/form HTTP transport (`desktop_helpers.py:request_json`):
/// 8s timeout, no redirects, 64 KiB cap, 429 Retry-After clamping, 401/403 & 400 `invalid_grant` classification.
pub fn request_json_http(
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&Value>,
    form: bool,
) -> Result<Value, QuotaTransportError> {
    let client = reqwest::blocking::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
        .build()
        .map_err(|_| {
            QuotaTransportError::new(
                "error",
                "The usage service could not be reached within its time limit.",
            )
        })?;
    let mut req = if body.is_some() {
        client.post(url)
    } else {
        client.get(url)
    };
    let mut has_ua = false;
    for &(k, v) in headers {
        if k.eq_ignore_ascii_case("user-agent") {
            has_ua = true;
        }
        req = req.header(k, v);
    }
    req = req.header("Accept", "application/json");
    if !has_ua {
        req = req.header("User-Agent", "CCS-Account-Usage/1.0");
    }
    if let Some(payload) = body {
        if form {
            let mut ser = url::form_urlencoded::Serializer::new(String::new());
            if let Some(obj) = payload.as_object() {
                for (k, v) in obj {
                    if let Some(s) = v.as_str() {
                        ser.append_pair(k, s);
                    }
                }
            }
            req = req
                .header("Content-Type", "application/x-www-form-urlencoded")
                .body(ser.finish());
        } else {
            let bytes = serde_json::to_vec(payload).map_err(|_| {
                QuotaTransportError::new(
                    "error",
                    "The usage service returned an unsupported response.",
                )
            })?;
            req = req.header("Content-Type", "application/json").body(bytes);
        }
    }
    let resp = req.send().map_err(|_| {
        QuotaTransportError::new(
            "error",
            "The usage service could not be reached within its time limit.",
        )
    })?;
    let status = resp.status().as_u16();
    if (300..400).contains(&status) {
        return Err(QuotaTransportError::new(
            "error",
            "The usage service returned an unexpected redirect.",
        ));
    }
    if status == 429 {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let header_val = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok());
        return Err(QuotaTransportError::rate_limited(retry_after_seconds(
            header_val, now_secs,
        )));
    }
    if status == 401 || status == 403 {
        return Err(QuotaTransportError::new(
            "needs_sign_in",
            "The saved account could not authenticate with the usage service.",
        ));
    }
    if status == 400 {
        let mut err_body = Vec::new();
        let _ = resp.take(4096).read_to_end(&mut err_body);
        if let Ok(Value::Object(map)) = serde_json::from_slice::<Value>(&err_body)
            && map.get("error").and_then(Value::as_str) == Some("invalid_grant")
        {
            return Err(QuotaTransportError::new(
                "needs_sign_in",
                "The saved account's sign-in has expired or was revoked.",
            ));
        }
        return Err(QuotaTransportError::new(
            "error",
            "The usage service is temporarily unavailable.",
        ));
    }
    if !(200..300).contains(&status) {
        return Err(QuotaTransportError::new(
            "error",
            "The usage service is temporarily unavailable.",
        ));
    }
    let mut raw = Vec::new();
    resp.take((HTTP_MAX_BYTES + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|_| {
            QuotaTransportError::new(
                "error",
                "The usage service could not be reached within its time limit.",
            )
        })?;
    if raw.len() > HTTP_MAX_BYTES {
        return Err(QuotaTransportError::new(
            "error",
            "The usage service response exceeded its size limit.",
        ));
    }
    let value: Value = serde_json::from_slice(&raw).map_err(|_| {
        QuotaTransportError::new(
            "error",
            "The usage service returned an unsupported response.",
        )
    })?;
    if !value.is_object() {
        return Err(QuotaTransportError::new(
            "error",
            "The usage service returned an unsupported response.",
        ));
    }
    Ok(value)
}

// ---------------------------------------------------------------------------
// Normalization helpers (`desktop_helpers.py` & `desktop_usage.py`)
// ---------------------------------------------------------------------------

fn number_val(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if v.is_boolean() {
        return None;
    }
    v.as_f64().filter(|n| n.is_finite())
}

fn nonnegative_val(value: Option<&Value>) -> Option<f64> {
    number_val(value).filter(|&n| n >= 0.0)
}

fn counter_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^\d+(?:\.\d+)?(?:[eE][+-]?\d+)?$").expect("valid counter regex"))
}

fn counter_val(value: Option<&Value>) -> Option<f64> {
    let v = value?;
    if let Some(s) = v.as_str() {
        if s.len() > 64 || !counter_re().is_match(s) {
            return None;
        }
        let parsed: f64 = s.parse().ok()?;
        return (parsed.is_finite() && parsed >= 0.0).then_some(parsed);
    }
    nonnegative_val(Some(v))
}

fn percent_val(value: Option<f64>) -> Option<f64> {
    let n = value.filter(|n| n.is_finite() && *n >= 0.0)?;
    aac_num::py::round(n, 4)
}

fn secret_leak_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?i)(bearer\s|dca:|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8})")
            .expect("valid leak regex")
    })
}

fn has_unsafe_display_char(text: &str) -> bool {
    text.chars().any(|c| {
        let code = c as u32;
        code <= 0x1f
            || (0x7f..=0x9f).contains(&code)
            || matches!(
                code,
                0x061c | 0x200e | 0x200f | 0x2028..=0x202e | 0x2066..=0x2069 | 0xfeff
            )
    })
}

fn safe_text(value: Option<&Value>, maximum: usize) -> Option<String> {
    let raw = value?.as_str()?;
    if raw.len() > maximum {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() || has_unsafe_display_char(trimmed) || secret_leak_re().is_match(trimmed)
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn reset_at_val(value: Option<&Value>) -> Option<String> {
    let v = value?;
    if let Some(mut secs) = number_val(Some(v)) {
        if secs > 100_000_000_000.0 {
            secs /= 1000.0;
        }
        let (iso, year) = aac_num::py::iso_seconds_of_timestamp(secs)?;
        return (2000..=2100).contains(&year).then_some(iso);
    }
    if let Some(text) = v.as_str() {
        let (utc_ms, _local_year) = parse_iso_datetime_utc_ms(text)?;
        let secs = utc_ms.div_euclid(1000);
        let year = aac_num::civil::civil_from_days(secs.div_euclid(86_400)).0;
        if !(2000..=2100).contains(&year) {
            return None;
        }
        return aac_num::py::iso_seconds(secs);
    }
    None
}

#[allow(clippy::too_many_arguments)]
fn quota_window(
    key: String,
    label: String,
    used_percent: Option<f64>,
    remaining_percent: Option<f64>,
    reset: Option<&Value>,
    minutes: Option<f64>,
    used: Option<f64>,
    limit: Option<f64>,
    unit: Option<String>,
    kind: &str,
    remaining: Option<f64>,
    expires: Option<&Value>,
    unlimited: Option<bool>,
    enabled: Option<bool>,
) -> Value {
    let mut up = percent_val(used_percent);
    let mut rp = percent_val(remaining_percent);
    if up.is_none()
        && let Some(rem) = rp
    {
        up = aac_num::py::round(100.0 - rem, 4).map(|v| v.max(0.0));
    }
    if rp.is_none()
        && let Some(u) = up
    {
        rp = aac_num::py::round(100.0 - u, 4).map(|v| v.max(0.0));
    }
    let window_minutes = minutes.filter(|&m| m.is_finite() && m > 0.0 && m <= 525_600.0);
    let mut obj = Map::new();
    obj.insert("key".into(), Value::String(key));
    obj.insert("label".into(), Value::String(label));
    obj.insert("usedPercent".into(), up.map_or(Value::Null, |v| json!(v)));
    obj.insert(
        "remainingPercent".into(),
        rp.map_or(Value::Null, |v| json!(v)),
    );
    obj.insert(
        "resetAt".into(),
        reset_at_val(reset).map_or(Value::Null, Value::String),
    );
    obj.insert(
        "windowMinutes".into(),
        window_minutes.map_or(Value::Null, |m| {
            if m.fract() == 0.0 {
                json!(m as i64)
            } else {
                json!(m)
            }
        }),
    );
    obj.insert(
        "used".into(),
        used.filter(|n| n.is_finite() && *n >= 0.0)
            .map_or(Value::Null, |v| json!(v)),
    );
    obj.insert(
        "limit".into(),
        limit
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map_or(Value::Null, |v| json!(v)),
    );
    obj.insert("unit".into(), unit.map_or(Value::Null, Value::String));
    obj.insert("kind".into(), Value::String(kind.to_owned()));
    if let Some(rem) = remaining {
        let val = (rem.is_finite() && rem >= 0.0).then_some(rem);
        obj.insert("remaining".into(), val.map_or(Value::Null, |v| json!(v)));
    }
    if expires.is_some() {
        obj.insert(
            "expiresAt".into(),
            reset_at_val(expires).map_or(Value::Null, Value::String),
        );
    }
    if let Some(u) = unlimited {
        obj.insert("unlimited".into(), Value::Bool(u));
    }
    if let Some(e) = enabled {
        obj.insert("enabled".into(), Value::Bool(e));
    }
    Value::Object(obj)
}

/// Extract provider quota groups from `retrieveUserQuotaSummary` (`desktop_usage.py:agy_groups`).
pub fn agy_groups(payload: &Value) -> Vec<Value> {
    let mut current = payload;
    for _ in 0..4 {
        let Some(obj) = current.as_object() else {
            break;
        };
        if let Some(groups) = obj.get("groups").and_then(Value::as_array) {
            return groups.iter().take(32).cloned().collect();
        }
        if let Some(buckets) = obj.get("buckets").and_then(Value::as_array) {
            return vec![json!({"displayName": "Antigravity", "buckets": buckets})];
        }
        let nested = ["quotaSummary", "summary", "response"]
            .into_iter()
            .find_map(|k| obj.get(k).filter(|v| v.is_object()));
        match nested {
            Some(next) => current = next,
            None => break,
        }
    }
    Vec::new()
}

/// Normalize Antigravity summary groups into display windows (`desktop_usage.py:normalize_antigravity`).
pub fn normalize_antigravity(payload: &Value) -> Vec<Value> {
    let mut windows = Vec::new();
    let mut keys = HashSet::new();
    for (group_index, group) in agy_groups(payload).into_iter().enumerate() {
        let Some(group_obj) = group.as_object() else {
            continue;
        };
        let group_label = safe_text(group_obj.get("displayName"), 160)
            .unwrap_or_else(|| "Antigravity".to_owned());
        let Some(buckets) = group_obj.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        for (bucket_index, bucket) in buckets.iter().take(32).enumerate() {
            let Some(bucket_obj) = bucket.as_object() else {
                continue;
            };
            let mut fraction = number_val(bucket_obj.get("remainingFraction"));
            let mut remaining_amount = counter_val(bucket_obj.get("remainingAmount"));
            if fraction.is_none()
                && let Some(rem) = bucket_obj.get("remaining").and_then(Value::as_object)
            {
                fraction = number_val(rem.get("remainingFraction"));
                if fraction.is_none()
                    && rem.get("case").and_then(Value::as_str) == Some("remainingFraction")
                {
                    fraction = number_val(rem.get("value"));
                }
                if remaining_amount.is_none() {
                    remaining_amount = counter_val(rem.get("remainingAmount"));
                    if remaining_amount.is_none()
                        && rem.get("case").and_then(Value::as_str) == Some("remainingAmount")
                    {
                        remaining_amount = counter_val(rem.get("value"));
                    }
                }
            }
            let fraction = fraction.filter(|&f| (0.0..=1.0).contains(&f));
            let period = bucket_obj.get("window").and_then(Value::as_str);
            let minutes = match period {
                Some("weekly") => Some(10_080.0),
                Some("5h") => Some(300.0),
                _ => None,
            };
            let period_label = match period {
                Some("weekly") => Some("Weekly"),
                Some("5h") => Some("5-hour"),
                _ => None,
            };
            let bucket_label = safe_text(bucket_obj.get("displayName"), 160);
            let label = if let Some(pl) = period_label {
                format!("{group_label} \u{00b7} {pl}")
            } else if let Some(bl) = bucket_label
                && bl != group_label
            {
                format!("{group_label} \u{00b7} {bl}")
            } else {
                group_label.clone()
            };
            let mut key = safe_text(bucket_obj.get("bucketId"), 160)
                .unwrap_or_else(|| format!("group-{group_index}-{bucket_index}"));
            if keys.contains(&key) {
                key.push_str(&format!("-{group_index}-{bucket_index}"));
            }
            let enabled = bucket_obj
                .get("disabled")
                .and_then(Value::as_bool)
                .map(|d| !d);
            let unlimited = bucket_obj.get("unlimited").and_then(Value::as_bool);
            let window = quota_window(
                key.clone(),
                label,
                None,
                fraction.map(|f| f * 100.0),
                bucket_obj.get("resetTime"),
                minutes,
                None,
                None,
                safe_text(bucket_obj.get("unit"), 32),
                "rate_limit",
                remaining_amount,
                None,
                unlimited,
                enabled,
            );
            let has_signal = !window["remainingPercent"].is_null()
                || !window["resetAt"].is_null()
                || window.get("remaining").is_some_and(|v| !v.is_null());
            if has_signal {
                windows.push(window);
                keys.insert(key);
            }
            if windows.len() >= 64 {
                return windows;
            }
        }
    }
    windows
}

/// Whether the summary windows already report a non-Argon Gemini group
/// (`desktop_usage.py:antigravity_summary_has_gemini`).
pub fn antigravity_summary_has_gemini(summary_windows: &[Value]) -> bool {
    summary_windows.iter().any(|w| {
        let Some(obj) = w.as_object() else {
            return false;
        };
        let key = obj
            .get("key")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let label = obj
            .get("label")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        (key.contains("gemini") || label.starts_with("gemini"))
            && !key.contains("argon")
            && !label.contains("argon")
    })
}

fn gemini_period(
    reset: &str,
    anchors: &[(Option<&str>, Option<i64>)],
) -> (Option<&'static str>, Option<f64>) {
    let Some((stamp_ms, _)) = parse_iso_datetime_utc_ms(reset) else {
        return (None, None);
    };
    let mut matched: BTreeSet<(&'static str, i64)> = BTreeSet::new();
    for &(anchor_reset, minutes) in anchors {
        let Some(ar) = anchor_reset else { continue };
        let Some(mins) = minutes.filter(|&m| m == 300 || m == 10_080) else {
            continue;
        };
        let Some((anchor_ms, _)) = parse_iso_datetime_utc_ms(ar) else {
            continue;
        };
        if (stamp_ms - anchor_ms).abs() <= 600_000 {
            matched.insert(if mins == 300 {
                ("5h", 300)
            } else {
                ("weekly", 10_080)
            });
        }
    }
    if matched.len() == 1
        && let Some(&(p, m)) = matched.iter().next()
    {
        return (Some(p), Some(m as f64));
    }
    (None, None)
}

/// Derive Gemini shared windows from per-model `retrieveUserQuota` buckets
/// (`desktop_usage.py:normalize_antigravity_gemini`).
pub fn normalize_antigravity_gemini(
    payload: Option<&Value>,
    summary_windows: &[Value],
) -> Vec<Value> {
    let Some(obj) = payload.and_then(Value::as_object) else {
        return Vec::new();
    };
    if antigravity_summary_has_gemini(summary_windows) {
        return Vec::new();
    }
    let anchors: Vec<(Option<&str>, Option<i64>)> = summary_windows
        .iter()
        .filter_map(|w| w.as_object())
        .map(|w| {
            let r = w.get("resetAt").and_then(Value::as_str);
            let m = w
                .get("windowMinutes")
                .and_then(|v| (!v.is_boolean()).then(|| v.as_f64()).flatten())
                .filter(|m| m.is_finite() && m.fract() == 0.0)
                .map(|m| m as i64);
            (r, m)
        })
        .collect();

    let mut collected: Vec<(i64, String, Option<String>, f64)> = Vec::new();
    if let Some(buckets) = obj.get("buckets").and_then(Value::as_array) {
        for bucket in buckets.iter().take(256) {
            let Some(b) = bucket.as_object() else {
                continue;
            };
            let Some(reset) = reset_at_val(b.get("resetTime")) else {
                continue;
            };
            let Some(model_id) = b.get("modelId").and_then(Value::as_str) else {
                continue;
            };
            if !model_id.starts_with("gemini-")
                || model_id.to_lowercase().contains("argon")
                || model_id.is_empty()
                || model_id.len() > 160
            {
                continue;
            }
            let fraction = match number_val(b.get("remainingFraction")) {
                Some(f) => f,
                None => {
                    if b.get("remainingAmount").is_some_and(|v| !v.is_null()) {
                        continue;
                    }
                    0.0
                }
            };
            if !(0.0..=1.0).contains(&fraction) {
                continue;
            }
            let Some((stamp_ms, _)) = parse_iso_datetime_utc_ms(&reset) else {
                continue;
            };
            collected.push((
                stamp_ms,
                reset,
                safe_text(Some(&Value::String(model_id.to_owned())), 160),
                fraction,
            ));
        }
    }
    type GeminiMember = (Option<String>, f64);
    type GeminiResetGroup = (i64, String, Vec<GeminiMember>);

    collected.sort_by_key(|entry| entry.0);
    let mut groups: Vec<GeminiResetGroup> = Vec::new();
    for (stamp_ms, reset, model_id, fraction) in collected {
        if let Some(last) = groups.last_mut()
            && stamp_ms - last.0 <= GEMINI_RESET_CLUSTER_SECONDS * 1000
        {
            last.2.push((model_id, fraction));
        } else {
            groups.push((stamp_ms, reset, vec![(model_id, fraction)]));
        }
    }

    let mut seen_base_keys: HashMap<String, usize> = HashMap::new();
    let mut windows = Vec::new();
    for (_stamp, reset, members) in groups.into_iter().take(4) {
        let (period, minutes) = gemini_period(&reset, &anchors);
        let base = format!("gemini-{}", period.unwrap_or("window"));
        let count = seen_base_keys.entry(base.clone()).or_insert(0);
        *count += 1;
        let key = if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        };
        let label = match period {
            Some("5h") => "Gemini Models \u{00b7} 5-hour".to_owned(),
            Some("weekly") => "Gemini Models \u{00b7} Weekly".to_owned(),
            _ => "Gemini Models".to_owned(),
        };
        let min_frac = members
            .iter()
            .map(|(_, f)| *f)
            .fold(f64::INFINITY, f64::min);
        let mut window = quota_window(
            key,
            label,
            None,
            Some(min_frac * 100.0),
            Some(&Value::String(reset)),
            minutes,
            None,
            None,
            None,
            "rate_limit",
            None,
            None,
            None,
            None,
        );
        let model_ids: Vec<String> = members
            .into_iter()
            .filter_map(|(m, _)| m)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(64)
            .collect();
        if !model_ids.is_empty()
            && let Some(obj) = window.as_object_mut()
        {
            obj.insert("modelIds".into(), json!(model_ids));
        }
        windows.push(window);
    }
    windows
}

/// Extract display plan string and bounded `reportedPlan` object from `loadCodeAssist`
/// (`desktop_usage.py:antigravity_reported_plan`).
pub fn antigravity_reported_plan(load: &Value) -> (Option<String>, Option<Value>) {
    let Some(obj) = load.as_object() else {
        return (None, None);
    };
    let mut reported = Map::new();
    let plan_type = obj
        .get("planInfo")
        .and_then(Value::as_object)
        .and_then(|info| safe_text(info.get("planType"), 80));
    if let Some(pt) = &plan_type {
        reported.insert("planType".into(), Value::String(pt.clone()));
    }
    for key in ["paidTier", "currentTier"] {
        if let Some(tier) = obj.get(key).and_then(Value::as_object) {
            let mut display = Map::new();
            for field in ["id", "name"] {
                if let Some(val) = safe_text(tier.get(field), 80) {
                    display.insert(field.into(), Value::String(val));
                }
            }
            if !display.is_empty() {
                reported.insert(key.into(), Value::Object(display));
            }
        }
    }
    let tier = reported
        .get("paidTier")
        .or_else(|| reported.get("currentTier"))
        .and_then(Value::as_object);
    let plan = plan_type
        .or_else(|| {
            tier.and_then(|t| t.get("name").and_then(Value::as_str))
                .map(str::to_owned)
        })
        .or_else(|| {
            tier.and_then(|t| t.get("id").and_then(Value::as_str))
                .map(str::to_owned)
        });
    let reported_opt = (!reported.is_empty()).then_some(Value::Object(reported));
    (plan, reported_opt)
}

/// Normalize optional shared Google AI credits from `loadCodeAssist`
/// (`desktop_usage.py:normalize_antigravity_credits`).
pub fn normalize_antigravity_credits(load: &Value) -> Vec<Value> {
    let Some(credits) = load
        .get("paidTier")
        .and_then(Value::as_object)
        .and_then(|t| t.get("availableCredits"))
        .and_then(Value::as_array)
    else {
        return Vec::new();
    };
    if credits.is_empty() || credits.len() > 64 {
        return Vec::new();
    }
    let amounts: Vec<f64> = credits
        .iter()
        .filter_map(|item| {
            item.as_object()
                .and_then(|o| counter_val(o.get("creditAmount")))
        })
        .collect();
    if amounts.is_empty() {
        return Vec::new();
    }
    let sum: f64 = amounts.into_iter().sum();
    if !sum.is_finite() || sum < 0.0 {
        return Vec::new();
    }
    vec![quota_window(
        "google-ai-credits".into(),
        "AI credits (overage)".into(),
        None,
        None,
        None,
        None,
        None,
        None,
        Some("credits".into()),
        "balance",
        Some(sum),
        None,
        None,
        None,
    )]
}

/// Prefix CLI window keys with `"cli-"` and append the service location suffix to labels
/// (`desktop_usage.py:antigravity_service_label`).
pub fn antigravity_service_label(windows: &mut [Value], service: &str) {
    let suffix = if service == "cli" {
        "In T3 (Antigravity CLI)"
    } else {
        "In the Antigravity app"
    };
    for window in windows.iter_mut() {
        let Some(obj) = window.as_object_mut() else {
            continue;
        };
        if service == "cli"
            && let Some(key) = obj.get("key").and_then(Value::as_str).map(str::to_owned)
        {
            obj.insert("key".into(), Value::String(format!("cli-{key}")));
        }
        let base_label = obj
            .get("label")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("Antigravity")
            .to_owned();
        obj.insert(
            "label".into(),
            Value::String(format!("{base_label} \u{00b7} {suffix}")),
        );
    }
}

fn antigravity_unavailable_windows(windows: &[Value]) -> Vec<Value> {
    windows
        .iter()
        .filter_map(|w| {
            let obj = w.as_object()?;
            let kind = obj
                .get("kind")
                .and_then(Value::as_str)
                .unwrap_or("rate_limit");
            if kind != "rate_limit" {
                return None;
            }
            Some(json!({
                "key": obj.get("key").cloned().unwrap_or(Value::Null),
                "label": obj.get("label").cloned().unwrap_or(Value::Null),
                "usedPercent": Value::Null,
                "remainingPercent": Value::Null,
                "resetAt": Value::Null,
                "windowMinutes": obj.get("windowMinutes").cloned().unwrap_or(Value::Null),
                "used": Value::Null,
                "limit": Value::Null,
                "unit": Value::Null,
                "kind": kind,
                "status": "unavailable",
            }))
        })
        .collect()
}

/// Call `loadCodeAssist` on the production app service, falling back to the daily CLI service
/// on non-auth errors (`desktop_usage.py:antigravity_load`).
pub fn antigravity_load<R: JsonRequester>(
    req: &R,
    headers: &[(&str, &str)],
) -> Result<Value, QuotaTransportError> {
    let body = json!({
        "metadata": {
            "ideType": "ANTIGRAVITY",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });
    match req.request_json(
        &format!("{AGY_BASE}loadCodeAssist"),
        headers,
        Some(&body),
        false,
    ) {
        Ok(val) => Ok(val),
        Err(err) if err.status == "needs_sign_in" => Err(err),
        Err(_) => req.request_json(
            &format!("{AGY_CLI_BASE}loadCodeAssist"),
            headers,
            Some(&body),
            false,
        ),
    }
}

/// Read both quota services (CLI first, then app) within the shared worker budget
/// (`desktop_usage.py:antigravity_services`).
pub fn antigravity_services<R: JsonRequester, E: Fn() -> f64>(
    req: &R,
    headers: &[(&str, &str)],
    project: &str,
    elapsed: E,
    per_model: bool,
) -> Result<Vec<ServiceResult>, QuotaTransportError> {
    let services: [(&'static str, &'static str); 2] = [("cli", AGY_CLI_BASE), ("app", AGY_BASE)];
    let mut results: Vec<ServiceResult> = Vec::with_capacity(2);
    let mut failures: Vec<QuotaTransportError> = Vec::new();
    let project_body = json!({"project": project});

    for (index, &(service, base)) in services.iter().enumerate() {
        let mut res = ServiceResult {
            service,
            base,
            status: "unavailable",
            groups: Vec::new(),
            windows: Vec::new(),
            retry_after: None,
        };
        if index > 0 && elapsed() > AGY_REQUIRED_BUDGET_SECONDS {
            results.push(res);
            continue;
        }
        match req.request_json(
            &format!("{base}retrieveUserQuotaSummary"),
            headers,
            Some(&project_body),
            false,
        ) {
            Ok(summary) => {
                res.status = "ok";
                res.groups = agy_groups(&summary);
                res.windows = normalize_antigravity(&summary);
                results.push(res);
            }
            Err(err) => {
                let is_rl = err.is_rate_limited();
                let retry = err.valid_retry_after();
                failures.push(err.clone());
                if is_rl {
                    if !results.iter().any(|r| r.status == "ok") {
                        return Err(err);
                    }
                    res.status = "rate_limited";
                    res.retry_after = retry;
                    results.push(res);
                    break;
                }
                results.push(res);
            }
        }
    }

    let first_ok_idx = results.iter().position(|r| r.status == "ok");
    let Some(ok_idx) = first_ok_idx else {
        if !failures.is_empty() && failures.iter().all(|f| f.status == "needs_sign_in") {
            return Err(failures[0].clone());
        }
        return Err(failures.pop().unwrap_or_else(|| {
            QuotaTransportError::new(
                "unavailable",
                "There was no time left to read Antigravity's quota.",
            )
        }));
    };

    let known = results[ok_idx].windows.clone();
    for res in &mut results {
        if res.status != "ok" {
            res.windows = antigravity_unavailable_windows(&known);
        }
    }

    if per_model {
        for res in &mut results {
            if res.status != "ok"
                || antigravity_summary_has_gemini(&res.windows)
                || elapsed() > AGY_OPTIONAL_BUDGET_SECONDS
            {
                continue;
            }
            let per_model_reply = match req.request_json(
                &format!("{}retrieveUserQuota", res.base),
                headers,
                Some(&project_body),
                false,
            ) {
                Ok(reply) => Some(reply),
                Err(err) => {
                    if res.retry_after.is_none() {
                        res.retry_after = err.valid_retry_after();
                    }
                    None
                }
            };
            let derived = normalize_antigravity_gemini(per_model_reply.as_ref(), &res.windows);
            res.windows.extend(derived);
        }
    }
    Ok(results)
}

// ---------------------------------------------------------------------------
// Structural pools & snapshot collection (`quota_adapter.py`)
// ---------------------------------------------------------------------------

fn valid_bucket_id(id: &str) -> bool {
    (1..=160).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

fn valid_model_id(id: &str) -> bool {
    (1..=160).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:/-".contains(&b))
}

fn valid_pool_label(label: &str) -> bool {
    (1..=160).contains(&label.len())
        && !label.bytes().any(|b| b < 0x20 || b == 0x7f)
        && !secret_leak_re().is_match(label)
}

/// Derive structural pools from exact provider bucket membership (`quota_adapter.py:structural_pools`).
/// Mutates matching `windows` in place with `poolId`, `poolIdSource`, `poolLabel`, and `modelIds`.
pub fn structural_pools(groups: &[Value], windows: &mut [Value], service: &str) -> Vec<Value> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for group in groups {
        if let Some(buckets) = group.get("buckets").and_then(Value::as_array) {
            for bucket in buckets {
                if let Some(id) = bucket.get("bucketId").and_then(Value::as_str) {
                    *counts.entry(id.to_owned()).or_insert(0) += 1;
                }
            }
        }
    }
    let key_to_idx: HashMap<String, usize> = windows
        .iter()
        .enumerate()
        .filter_map(|(idx, w)| {
            w.get("key")
                .and_then(Value::as_str)
                .map(|k| (k.to_owned(), idx))
        })
        .collect();

    let mut pools = Vec::new();
    for group in groups.iter().take(32) {
        let Some(group_obj) = group.as_object() else {
            continue;
        };
        let Some(buckets) = group_obj.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        let ids: Vec<Option<&str>> = buckets
            .iter()
            .filter_map(|b| {
                b.as_object()
                    .map(|o| o.get("bucketId").and_then(Value::as_str))
            })
            .collect();
        if ids.len() != buckets.len() || ids.is_empty() || ids.len() > 32 {
            continue;
        }
        let mut id_strings: Vec<String> = Vec::with_capacity(ids.len());
        let mut all_valid = true;
        for id in ids {
            match id {
                Some(s) if valid_bucket_id(s) => id_strings.push(s.to_owned()),
                _ => {
                    all_valid = false;
                    break;
                }
            }
        }
        if !all_valid {
            continue;
        }
        let mut sorted_ids = id_strings.clone();
        sorted_ids.sort();
        let members: Vec<&str> = if service == "app" {
            sorted_ids.iter().map(String::as_str).collect()
        } else {
            let mut m = Vec::with_capacity(sorted_ids.len() + 1);
            m.push(service);
            m.extend(sorted_ids.iter().map(String::as_str));
            m
        };
        let pool_id = format!(
            "bucket-group:{}",
            sha256_hex(json!(members).to_string().as_bytes())
        );

        let selected_indices: Vec<usize> = id_strings
            .iter()
            .filter_map(|k| key_to_idx.get(k).copied())
            .collect();

        let flags_valid = buckets.iter().all(|b| {
            let Some(o) = b.as_object() else { return false };
            ["disabled", "unlimited"]
                .into_iter()
                .all(|key| !o.contains_key(key) || o[key].is_boolean())
        });
        let any_disabled = buckets
            .iter()
            .any(|b| b.get("disabled") == Some(&Value::Bool(true)));
        let mut complete = flags_valid
            && selected_indices.len() == id_strings.len()
            && id_strings.iter().all(|k| counts.get(k) == Some(&1));
        if complete {
            complete = selected_indices.iter().all(|&idx| {
                let w = &windows[idx];
                w.get("kind").and_then(Value::as_str) == Some("rate_limit")
                    && number_val(w.get("remainingPercent"))
                        .is_some_and(|p| (0.0..=100.0).contains(&p))
                    && w.get("resetAt").and_then(Value::as_str).is_some()
            });
        }
        let eligibility = if !flags_valid || any_disabled {
            "unverified"
        } else {
            "reported-quota"
        };

        let actual_label = group_obj
            .get("displayName")
            .and_then(Value::as_str)
            .filter(|s| valid_pool_label(s))
            .map(str::to_owned);
        let actual_models: Option<Vec<String>> = group_obj
            .get("modelIds")
            .and_then(Value::as_array)
            .filter(|arr| {
                !arr.is_empty() && arr.iter().all(|v| v.as_str().is_some_and(valid_model_id))
            })
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect()
            });

        let mut pool_windows = Vec::with_capacity(selected_indices.len());
        for &idx in &selected_indices {
            if let Some(w_obj) = windows[idx].as_object_mut() {
                if w_obj
                    .get("key")
                    .and_then(Value::as_str)
                    .and_then(|k| counts.get(k))
                    == Some(&1)
                {
                    w_obj.insert("poolId".into(), Value::String(pool_id.clone()));
                    w_obj.insert(
                        "poolIdSource".into(),
                        Value::String("provider-bucket-membership".into()),
                    );
                }
                if let Some(label) = &actual_label {
                    w_obj.insert("poolLabel".into(), Value::String(label.clone()));
                }
                if let Some(models) = &actual_models {
                    w_obj.insert("modelIds".into(), json!(models));
                }
            }
            pool_windows.push(windows[idx].clone());
        }

        pools.push(json!({
            "id": pool_id,
            "idSource": "provider-bucket-membership",
            "eligibility": eligibility,
            "complete": complete,
            "bucketIds": sorted_ids,
            "windows": pool_windows,
        }));
    }
    pools
}

const ALLOWED_WINDOW_KEYS: [&str; 19] = [
    "key",
    "label",
    "usedPercent",
    "remainingPercent",
    "resetAt",
    "windowMinutes",
    "used",
    "limit",
    "unit",
    "kind",
    "remaining",
    "expiresAt",
    "unlimited",
    "enabled",
    "poolId",
    "poolIdSource",
    "poolLabel",
    "modelIds",
    "status",
];

fn bounded_plan_text(val: Option<&Value>) -> Option<&str> {
    let s = val?.as_str()?;
    (!s.is_empty() && s.len() <= 80 && !s.bytes().any(|b| b < 0x20 || b == 0x7f)).then_some(s)
}

/// Collect one identity-bound quota snapshot (`quota_adapter.py:collect_snapshot`).
pub fn collect_snapshot(profile: &SavedProfile, deps: &dyn QuotaDeps) -> Value {
    let mut base = json!({
        "profileId": profile.id,
        "identityKey": profile.identity_key,
        "credentialRevision": profile.credential_revision,
        "status": "unavailable",
        "email": profile.email,
        "plan": Value::Null,
        "fetchedAt": Value::Null,
        "sampledAt": Value::Null,
        "windows": [],
        "source": "native-consumer",
        "identityVerified": false,
        "identityValidation": "unavailable",
        "pools": [],
    });

    enum CollectErr {
        Auth(AuthError),
        Transport(QuotaTransportError),
    }
    impl From<AuthError> for CollectErr {
        fn from(e: AuthError) -> Self {
            Self::Auth(e)
        }
    }
    impl From<QuotaTransportError> for CollectErr {
        fn from(e: QuotaTransportError) -> Self {
            Self::Transport(e)
        }
    }

    let step = (|| -> Result<(), CollectErr> {
        if profile.credential.revision() != profile.credential_revision {
            return Err(AuthError::identity_mismatch(
                "The saved Antigravity account changed before its usage query.",
            )
            .into());
        }
        let mut current = profile.credential.clone();
        let now_ms = deps.now_ms();
        let parsed = parse_credential(current.raw())?;
        if parsed.expiry_ms <= now_ms.saturating_add(30_000) {
            current = refresh_in_memory(
                &current,
                |rt| deps.refresh(rt).map_err(|e| e.as_provider_error()),
                now_ms,
            )?;
        }
        let identity = verify_identity(
            &current,
            |tok| deps.userinfo(tok).map_err(|e| e.as_provider_error()),
            now_ms,
        )?;
        if identity.identity_key() != profile.identity_key
            || identity.email != profile.email.to_lowercase()
        {
            return Err(AuthError::identity_mismatch(
                "The saved Antigravity login belongs to another account.",
            )
            .into());
        }
        base["identityVerified"] = json!(true);
        base["identityValidation"] = json!("verified");

        let access = parse_credential(current.raw())?.access_token;
        let data = deps.quota(&access, &identity.email)?;
        let Some(data_obj) = data.as_object() else {
            return Err(
                AuthError::auth("Antigravity did not return a usable quota sample.").into(),
            );
        };
        let Some(raw_windows) = data_obj.get("windows").and_then(Value::as_array) else {
            return Err(
                AuthError::auth("Antigravity did not return a usable quota sample.").into(),
            );
        };
        let data_email = data_obj
            .get("email")
            .and_then(Value::as_str)
            .unwrap_or(&identity.email);
        if data_email.to_lowercase() != identity.email {
            return Err(AuthError::identity_mismatch(
                "Antigravity quota identity did not match the saved account.",
            )
            .into());
        }
        let mut windows = Vec::new();
        for w in raw_windows.iter().take(64) {
            if let Some(obj) = w.as_object() {
                let mut filtered = Map::new();
                for (k, v) in obj {
                    if ALLOWED_WINDOW_KEYS.contains(&k.as_str()) {
                        filtered.insert(k.clone(), v.clone());
                    }
                }
                if filtered.get("status").and_then(Value::as_str) != Some("unavailable") {
                    filtered.remove("status");
                }
                windows.push(Value::Object(filtered));
            }
        }
        let stamp_secs = deps.now_ms().div_euclid(1000);
        let stamp = aac_num::py::iso_seconds(stamp_secs)
            .ok_or_else(|| AuthError::auth("Invalid observation clock."))?;
        base["status"] = json!("fresh");
        base["email"] = json!(identity.email);
        base["plan"] = data_obj.get("plan").cloned().unwrap_or(Value::Null);
        base["fetchedAt"] = json!(stamp);
        base["sampledAt"] = json!(stamp);
        base["windows"] = Value::Array(windows);
        base["pools"] = data_obj
            .get("pools")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new()));
        if let Some(throttled) = data_obj.get("retryAfterSeconds")
            && let Some(n) = throttled.as_i64()
            && !throttled.is_boolean()
            && (1..=86_400).contains(&n)
        {
            base["retryAfterSeconds"] = json!(n);
        }
        if let Some(reported) = data_obj.get("reportedPlan").and_then(Value::as_object) {
            let mut display = Map::new();
            if let Some(pt) = bounded_plan_text(reported.get("planType")) {
                display.insert("planType".into(), Value::String(pt.to_owned()));
            }
            for key in ["paidTier", "currentTier"] {
                if let Some(tier) = reported.get(key).and_then(Value::as_object) {
                    let mut fields = Map::new();
                    for f in ["id", "name"] {
                        if let Some(val) = bounded_plan_text(tier.get(f)) {
                            fields.insert(f.into(), Value::String(val.to_owned()));
                        }
                    }
                    if !fields.is_empty() {
                        display.insert(key.into(), Value::Object(fields));
                    }
                }
            }
            if !display.is_empty() {
                base["reportedPlan"] = Value::Object(display);
            }
        }
        Ok(())
    })();

    if let Err(err) = step {
        match err {
            CollectErr::Auth(a) => match a.kind {
                AuthErrorKind::IdentityMismatch => {
                    base["status"] = json!("needs_sign_in");
                    base["identityVerified"] = json!(false);
                    base["identityValidation"] = json!("mismatch");
                }
                AuthErrorKind::NeedsSignIn => {
                    base["status"] = json!("needs_sign_in");
                    base["identityVerified"] = json!(false);
                    base["identityValidation"] = json!("needs_sign_in");
                }
                AuthErrorKind::IdentityUnavailable => {
                    base["status"] = json!("unavailable");
                    base["identityVerified"] = json!(false);
                    base["identityValidation"] = json!("unavailable");
                }
                AuthErrorKind::Auth => {
                    base["status"] = json!("unavailable");
                }
            },
            CollectErr::Transport(t) => {
                base["status"] = json!(if t.is_rate_limited() {
                    "rate_limited"
                } else if matches!(t.status, "needs_sign_in" | "unavailable") {
                    "unavailable"
                } else {
                    "error"
                });
            }
        }
    }
    base
}

/// Production collector dependencies (`quota_adapter.py:existing_collector_dependencies`).
pub struct CollectorDeps<R: JsonRequester, C: Fn() -> i64> {
    home: PathBuf,
    requester: R,
    clock: C,
    started_ms: i64,
}

impl<R: JsonRequester, C: Fn() -> i64> CollectorDeps<R, C> {
    /// Construct dependencies starting the shared worker budget immediately.
    pub fn new(home: &Path, requester: R, clock: C) -> Self {
        let started_ms = (clock)();
        Self {
            home: home.to_path_buf(),
            requester,
            clock,
            started_ms,
        }
    }
}

impl<R: JsonRequester, C: Fn() -> i64> QuotaDeps for CollectorDeps<R, C> {
    fn userinfo(&self, access: &str) -> Result<Value, QuotaTransportError> {
        let auth = format!("Bearer {access}");
        self.requester.request_json(
            USERINFO_ENDPOINT,
            &[("Authorization", auth.as_str())],
            None,
            false,
        )
    }

    fn refresh(&self, refresh_token: &str) -> Result<Value, QuotaTransportError> {
        let client = read_antigravity_oauth_client(&self.home)
            .map_err(|e| QuotaTransportError::new("unavailable", e.message))?;
        let body = json!({
            "grant_type": "refresh_token",
            "refresh_token": refresh_token,
            "client_id": client.client_id,
            "client_secret": client.client_secret,
        });
        self.requester
            .request_json(TOKEN_ENDPOINT, &[], Some(&body), true)
    }

    fn quota(&self, access: &str, email: &str) -> Result<Value, QuotaTransportError> {
        let auth = format!("Bearer {access}");
        let headers = [
            ("Authorization", auth.as_str()),
            ("User-Agent", AGY_USER_AGENT),
        ];
        let load = antigravity_load(&self.requester, &headers)?;
        let project_raw = match load.get("cloudaicompanionProject") {
            Some(Value::Object(obj)) => obj.get("id").or_else(|| obj.get("projectId")),
            other => other,
        };
        let Some(project) = safe_text(project_raw, 160) else {
            return Err(QuotaTransportError::new(
                "unavailable",
                "Antigravity did not return a usable quota project.",
            ));
        };
        let (plan, reported) = antigravity_reported_plan(&load);
        let mut windows = Vec::new();
        let mut pools = Vec::new();
        let mut throttled: Option<u64> = None;
        let services = antigravity_services(
            &self.requester,
            &headers,
            &project,
            || ((self.clock)() - self.started_ms) as f64 / 1000.0,
            true,
        )?;
        for mut svc in services {
            if svc.status == "ok" {
                let mut svc_pools = structural_pools(&svc.groups, &mut svc.windows, svc.service);
                // Replicate Python's in-place dict mutation so pool windows share labelled keys/labels.
                for pool in &mut svc_pools {
                    if let Some(pw) = pool.get_mut("windows").and_then(Value::as_array_mut) {
                        antigravity_service_label(pw, svc.service);
                    }
                }
                pools.extend(svc_pools);
            }
            antigravity_service_label(&mut svc.windows, svc.service);
            windows.extend(svc.windows);
            if svc.retry_after.is_some() && throttled.is_none() {
                throttled = svc.retry_after;
            }
        }
        windows.extend(normalize_antigravity_credits(&load));
        let mut result = json!({
            "email": email,
            "plan": plan,
            "windows": windows,
            "pools": pools,
        });
        if let Some(rep) = reported {
            result["reportedPlan"] = rep;
        }
        if let Some(retry) = throttled {
            result["retryAfterSeconds"] = json!(retry);
        }
        Ok(result)
    }

    fn now_ms(&self) -> i64 {
        (self.clock)()
    }
}

// ---------------------------------------------------------------------------
// Quota worker request/response protocol (`quota_worker.py`)
// ---------------------------------------------------------------------------

/// Decoded request for the private Antigravity identity/quota worker protocol.
#[derive(Clone, Debug)]
pub enum WorkerRequest {
    /// Verify credential identity (refreshing in memory if expiring within 30s).
    Identity {
        /// Validated private credential bytes.
        credential: PrivateCredential,
    },
    /// Collect an identity-bound quota snapshot.
    Quota {
        /// Bound saved profile descriptor.
        profile: SavedProfile,
    },
}

fn valid_worker_profile_id(id: &str) -> bool {
    let b = id.as_bytes();
    (1..=64).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b.iter()
            .all(|&c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
}

fn valid_worker_email(email: &str) -> bool {
    if email.is_empty() || email.len() > 254 {
        return false;
    }
    if email
        .chars()
        .any(|c| c.is_whitespace() || (c as u32) < 0x20)
    {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && !domain.is_empty()
        && !domain.contains('@')
        && domain
            .split_once('.')
            .is_some_and(|(a, b)| !a.is_empty() && !b.is_empty())
}

fn valid_hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Strictly decode one bounded quota-worker stdin packet (`quota_worker.py:decode_request`).
pub fn decode_worker_request(raw: &[u8]) -> Result<WorkerRequest, AuthError> {
    let invalid = || AuthError::auth("Invalid private request.");
    if raw.is_empty() || raw.len() > MAX_WORKER_REQUEST_BYTES {
        return Err(invalid());
    }
    let map = parse_strict_json_object(raw).map_err(|_| invalid())?;
    let keys: BTreeSet<&str> = map.keys().map(String::as_str).collect();
    let op = map
        .get("operation")
        .and_then(Value::as_str)
        .ok_or_else(invalid)?;
    let b64 = map
        .get("credentialBase64")
        .and_then(Value::as_str)
        .filter(|s| s.len() <= MAX_BASE64_CREDENTIAL_LEN)
        .ok_or_else(invalid)?;
    let bytes = STANDARD.decode(b64).map_err(|_| invalid())?;
    let cred = PrivateCredential::new(bytes)?;

    match op {
        "identity" if keys == BTreeSet::from(["operation", "credentialBase64"]) => {
            Ok(WorkerRequest::Identity { credential: cred })
        }
        "quota"
            if keys
                == BTreeSet::from([
                    "operation",
                    "credentialBase64",
                    "profileId",
                    "email",
                    "identityKey",
                    "credentialRevision",
                ]) =>
        {
            let profile_id = map
                .get("profileId")
                .and_then(Value::as_str)
                .filter(|s| valid_worker_profile_id(s))
                .ok_or_else(invalid)?;
            let email = map
                .get("email")
                .and_then(Value::as_str)
                .filter(|s| valid_worker_email(s))
                .ok_or_else(invalid)?;
            let identity_key = map
                .get("identityKey")
                .and_then(Value::as_str)
                .filter(|s| valid_hex64(s))
                .ok_or_else(invalid)?;
            let credential_revision = map
                .get("credentialRevision")
                .and_then(Value::as_str)
                .filter(|s| valid_hex64(s))
                .ok_or_else(invalid)?;
            Ok(WorkerRequest::Quota {
                profile: SavedProfile {
                    id: profile_id.to_owned(),
                    email: email.to_owned(),
                    identity_key: identity_key.to_owned(),
                    credential_revision: credential_revision.to_owned(),
                    credential: cred,
                },
            })
        }
        _ => Err(invalid()),
    }
}

/// Process one decoded worker request (`quota_worker.py:process_request`).
pub fn process_worker_request(
    req: &WorkerRequest,
    deps: &dyn QuotaDeps,
) -> Result<Value, AuthError> {
    match req {
        WorkerRequest::Quota { profile } => Ok(collect_snapshot(profile, deps)),
        WorkerRequest::Identity { credential } => {
            let mut current = credential.clone();
            let now_ms = deps.now_ms();
            let parsed = parse_credential(current.raw())?;
            if parsed.expiry_ms <= now_ms.saturating_add(30_000) {
                current = refresh_in_memory(
                    &current,
                    |rt| deps.refresh(rt).map_err(|e| e.as_provider_error()),
                    now_ms,
                )?;
            }
            let identity = verify_identity(
                &current,
                |tok| deps.userinfo(tok).map_err(|e| e.as_provider_error()),
                now_ms,
            )?;
            Ok(json!({
                "email": identity.email,
                "subject": identity.subject,
                "plan": Value::Null,
                "verifiedAt": identity.verified_at,
                "source": identity.source,
                "identityKey": identity.identity_key(),
                "credentialRevision": credential.revision(),
            }))
        }
    }
}

/// End-to-end bounded byte handler matching `quota_worker.py:handle_stream`.
pub fn handle_worker_bytes(raw: &[u8], deps: &dyn QuotaDeps) -> Result<Vec<u8>, &'static str> {
    let req = decode_worker_request(raw).map_err(|e| match e.kind {
        AuthErrorKind::NeedsSignIn | AuthErrorKind::IdentityMismatch => {
            "Antigravity account identity needs sign-in.\n"
        }
        AuthErrorKind::IdentityUnavailable => {
            "Antigravity account identity is temporarily unavailable.\n"
        }
        AuthErrorKind::Auth => "Antigravity private account request could not be completed.\n",
    })?;
    let result = process_worker_request(&req, deps).map_err(|e| match e.kind {
        AuthErrorKind::NeedsSignIn | AuthErrorKind::IdentityMismatch => {
            "Antigravity account identity needs sign-in.\n"
        }
        AuthErrorKind::IdentityUnavailable => {
            "Antigravity account identity is temporarily unavailable.\n"
        }
        AuthErrorKind::Auth => "Antigravity private account request could not be completed.\n",
    })?;
    let mut out = serde_json::to_vec(&result)
        .map_err(|_| "Antigravity private account request could not be completed.\n")?;
    if out.len() > MAX_WORKER_RESPONSE_BYTES {
        return Err("Antigravity private account request could not be completed.\n");
    }
    out.push(b'\n');
    Ok(out)
}

fn system_now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Native live credential identity validation (replaces `quota_worker.py` `identity` subprocess).
pub fn validate_credential_live(home: &Path, bytes: &[u8]) -> Option<LiveIdentity> {
    if bytes.is_empty() || bytes.len() > aac_files::antigravity_store::MAX_CREDENTIAL_BYTES {
        return None;
    }
    let cred = PrivateCredential::new(bytes.to_vec()).ok()?;
    let deps = CollectorDeps::new(home, LiveHttpRequester, system_now_ms);
    let result =
        process_worker_request(&WorkerRequest::Identity { credential: cred }, &deps).ok()?;
    let identity = LiveIdentity::from_value(&json!({
        "email": result.get("email"),
        "subject": result.get("subject"),
        "plan": result.get("plan"),
        "verifiedAt": result.get("verifiedAt"),
        "source": result.get("source"),
    }))?;
    if result.get("credentialRevision").and_then(Value::as_str) != Some(sha256_hex(bytes).as_str())
        || result.get("identityKey").and_then(Value::as_str)
            != Some(identity.identity_key().as_str())
    {
        return None;
    }
    Some(identity)
}

/// Native live saved-profile quota collection (replaces `quota_worker.py` `quota` subprocess).
pub fn collect_quota_live(home: &Path, request: &QuotaRequest<'_>) -> Option<Value> {
    let bytes = request.credential_bytes;
    if bytes.is_empty() || bytes.len() > aac_files::antigravity_store::MAX_CREDENTIAL_BYTES {
        return None;
    }
    if !valid_worker_profile_id(request.profile_id)
        || !valid_hex64(request.identity_key)
        || !valid_hex64(request.credential_revision)
        || request.email.to_lowercase() != request.identity.email
        || request.identity.identity_key() != request.identity_key
        || sha256_hex(bytes) != request.credential_revision
    {
        return None;
    }
    let cred = PrivateCredential::new(bytes.to_vec()).ok()?;
    let profile = SavedProfile {
        id: request.profile_id.to_owned(),
        email: request.email.to_owned(),
        identity_key: request.identity_key.to_owned(),
        credential_revision: request.credential_revision.to_owned(),
        credential: cred,
    };
    let deps = CollectorDeps::new(home, LiveHttpRequester, system_now_ms);
    Some(collect_snapshot(&profile, &deps))
}
