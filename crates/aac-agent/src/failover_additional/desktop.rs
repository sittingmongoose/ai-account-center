//! Shared desktop helper primitives ported from `desktop_helpers.py` and `desktop_usage.py`.
use aac_files::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use base64::{
    Engine,
    alphabet::URL_SAFE,
    engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig},
};
use chrono::{DateTime, Datelike, NaiveDateTime, SecondsFormat, TimeZone, Utc};
use regex::Regex;
use serde::{
    Deserialize, Deserializer,
    de::{self, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{SystemTime, UNIX_EPOCH},
};

/// Maximum response bytes for desktop/Google HTTP calls (`desktop_helpers.MAX_RESPONSE_BYTES`).
pub(crate) const MAX_RESPONSE_BYTES: usize = 65_536;
/// Antigravity app quota base URL (`desktop_usage.AGY_BASE`).
pub(crate) const AGY_BASE: &str = "https://cloudcode-pa.googleapis.com/v1internal:";
/// Antigravity CLI (daily) quota base URL (`desktop_usage.AGY_CLI_BASE`).
pub(crate) const AGY_CLI_BASE: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal:";
/// Antigravity CLI key prefix (`desktop_usage.AGY_CLI_KEY_PREFIX`).
pub(crate) const AGY_CLI_KEY_PREFIX: &str = "cli-";
/// Antigravity client User-Agent (`desktop_usage.AGY_USER_AGENT`).
pub(crate) const AGY_USER_AGENT: &str =
    "antigravity/hub/2.24.0 (aidev_client; os_type=linux; arch=amd64)";
/// Muse native key endpoint (`desktop_usage.MUSE_URL`).
pub(crate) const MUSE_URL: &str = "https://api.meta.ai/muse-code/key";

static SECRET_TEXT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(bearer[ \t\r\n\f\v]|dca:|sk-[A-Za-z0-9]|eyJ[A-Za-z0-9_-]{8})")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[^ \t\r\n\f\v@]+@[^ \t\r\n\f\v@]+\.[^ \t\r\n\f\v@]+\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static COUNTER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static SELECTOR_FAMILY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9_.@-]{0,62}:\*\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static SELECTOR_EXACT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9:_.@-]{0,159}\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static GOOGLE_CLIENT_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z0-9._-]{1,256}\.apps\.googleusercontent\.com\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static GOOGLE_CLIENT_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z0-9_-]{1,256}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});

static FORGIVING_URL_SAFE: LazyLock<GeneralPurpose> = LazyLock::new(|| {
    GeneralPurpose::new(
        &URL_SAFE,
        GeneralPurposeConfig::new()
            .with_decode_padding_mode(DecodePaddingMode::Indifferent)
            .with_decode_allow_trailing_bits(true),
    )
});

/// Per-inventory read-failure attribution matching `desktop_helpers.attribute_reads` and
/// `failover_usage._incomplete_selectors`.
#[derive(Debug, Default)]
pub(crate) struct ReadTracker {
    total_failures: usize,
    attributed_failures: usize,
    incomplete_selectors: BTreeSet<String>,
    explained_stack: Vec<usize>,
}

impl ReadTracker {
    pub(crate) fn record_failure(&mut self) {
        self.total_failures += 1;
    }

    pub(crate) fn total_failures(&self) -> usize {
        self.total_failures
    }

    pub(crate) fn attribute<R>(
        &mut self,
        selectors: &[String],
        f: impl FnOnce(&mut Self) -> R,
    ) -> R {
        let before = self.total_failures;
        self.explained_stack.push(0);
        let result = f(self);
        let explained = self.explained_stack.pop().unwrap_or(0);
        let added = self.total_failures.saturating_sub(before);
        if added > explained {
            for s in selectors {
                self.incomplete_selectors.insert(s.clone());
            }
        }
        if let Some(parent) = self.explained_stack.last_mut() {
            *parent += added;
        } else {
            self.attributed_failures += added;
        }
        result
    }

    pub(crate) fn incomplete_selectors(&self) -> Vec<String> {
        let mut selectors = self.incomplete_selectors.clone();
        if self.total_failures > self.attributed_failures {
            selectors.insert("*".to_owned());
        }
        let valid: Vec<String> = selectors
            .into_iter()
            .filter(|s| s == "*" || SELECTOR_FAMILY_RE.is_match(s) || SELECTOR_EXACT_RE.is_match(s))
            .collect();
        if valid.len() > 128 {
            vec!["*".to_owned()]
        } else {
            valid
        }
    }
}

/// Private in-memory desktop credential; never serialized to public wire outputs.
#[derive(Clone, PartialEq)]
pub(crate) struct Credential {
    pub access: String,
    pub source: String,
    pub refresh: String,
    pub expires: Option<f64>,
    pub email: Option<String>,
    pub project: Option<String>,
    pub plan: Option<String>,
    pub google_client: Option<String>,
}

impl fmt::Debug for Credential {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Credential")
            .field("source", &self.source)
            .field("email", &self.email)
            .field("plan", &self.plan)
            .field("google_client", &self.google_client)
            .finish()
    }
}

impl Credential {
    pub(crate) fn to_private_value(&self) -> Value {
        json!({
            "access": self.access,
            "source": self.source,
            "refresh": self.refresh,
            "expires": self.expires,
            "email": self.email,
            "project": self.project,
            "plan": self.plan,
            "google_client": self.google_client,
        })
    }

    pub(crate) fn from_private_value(value: &Value) -> Option<Self> {
        let obj = value.as_object()?;
        let access = obj.get("access")?.as_str()?.to_owned();
        let source = obj.get("source")?.as_str()?.to_owned();
        let refresh = obj
            .get("refresh")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let expires = obj.get("expires").and_then(Value::as_f64);
        let email = obj.get("email").and_then(Value::as_str).map(str::to_owned);
        let project = obj
            .get("project")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let plan = obj.get("plan").and_then(Value::as_str).map(str::to_owned);
        let google_client = obj
            .get("google_client")
            .and_then(Value::as_str)
            .map(str::to_owned);
        Some(Self {
            access,
            source,
            refresh,
            expires,
            email,
            project,
            plan,
            google_client,
        })
    }
}

/// Safe desktop usage error matching `desktop_helpers.UsageError`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct UsageError {
    pub status: &'static str,
    pub message: &'static str,
    pub failure_code: Option<&'static str>,
    pub retry_after: Option<u64>,
}

impl UsageError {
    pub(crate) fn new(status: &'static str, message: &'static str) -> Self {
        Self {
            status,
            message,
            failure_code: None,
            retry_after: None,
        }
    }
}

pub(crate) fn now_unix_secs(override_secs: Option<f64>) -> f64 {
    override_secs.unwrap_or_else(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0)
    })
}

pub(crate) fn utc_now_iso_secs(override_secs: Option<f64>) -> String {
    let secs = now_unix_secs(override_secs).trunc() as i64;
    DateTime::from_timestamp(secs, 0)
        .unwrap_or_else(|| {
            Utc.timestamp_opt(0, 0)
                .single()
                .unwrap_or(DateTime::UNIX_EPOCH)
        })
        .to_rfc3339_opts(SecondsFormat::Secs, true)
}

pub(crate) fn number(value: &Value) -> Option<f64> {
    let n = value.as_f64()?;
    n.is_finite().then_some(n)
}

pub(crate) fn nonnegative(value: &Value) -> Option<f64> {
    number(value).filter(|n| *n >= 0.0)
}

pub(crate) fn counter(value: &Value) -> Option<f64> {
    if let Some(s) = value.as_str() {
        if s.len() > 64 || !COUNTER_RE.is_match(s) {
            return None;
        }
        let parsed = s.parse::<f64>().ok()?;
        return (parsed.is_finite() && parsed >= 0.0).then_some(parsed);
    }
    nonnegative(value)
}

pub(crate) fn percent(value: &Value) -> Option<f64> {
    let n = nonnegative(value)?;
    aac_num::py::round(n, 4)
}

fn has_forbidden_control(text: &str) -> bool {
    text.chars().any(|c| {
        let cp = c as u32;
        cp <= 0x1f
            || (0x7f..=0x9f).contains(&cp)
            || cp == 0x061c
            || cp == 0x200e
            || cp == 0x200f
            || (0x2028..=0x202e).contains(&cp)
            || (0x2066..=0x2069).contains(&cp)
            || cp == 0xfeff
    })
}

pub(crate) fn safe_text(value: &Value, maximum: usize) -> Option<String> {
    let raw = value.as_str()?;
    if raw.chars().count() > maximum {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() || has_forbidden_control(trimmed) || SECRET_TEXT_RE.is_match(trimmed) {
        return None;
    }
    Some(trimmed.to_owned())
}

pub(crate) fn email(value: &Value) -> Option<String> {
    let text = safe_text(value, 254)?;
    (!text.chars().any(char::is_whitespace) && EMAIL_RE.is_match(&text)).then_some(text)
}

pub(crate) fn secret(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    if raw.chars().count() > 16_384 {
        return None;
    }
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if trimmed
        .chars()
        .any(|c| (c as u32) <= 0x20 || (c as u32) == 0x7f)
    {
        return None;
    }
    Some(trimmed.to_owned())
}

/// Parse an ISO-8601 timestamp with an explicit timezone (`Z` or `±HH:MM`) into UTC `DateTime`.
pub(crate) fn parse_iso_utc(raw: &str) -> Option<DateTime<Utc>> {
    let trimmed = raw.trim();
    if trimmed.len() > 64 {
        return None;
    }
    if let Ok(dt) = DateTime::parse_from_rfc3339(trimmed) {
        return Some(dt.with_timezone(&Utc));
    }
    // Support Python fromisoformat variants like space separator or ±HHMM.
    for fmt in [
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%dT%H:%M:%S%.f%#z",
        "%Y-%m-%dT%H:%M:%S%#z",
        "%Y-%m-%d %H:%M:%S%.f%#z",
        "%Y-%m-%d %H:%M:%S%#z",
    ] {
        if let Ok(dt) = DateTime::parse_from_str(trimmed, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    if let Some(base) = trimmed.strip_suffix('Z') {
        for fmt in [
            "%Y-%m-%dT%H:%M:%S%.f",
            "%Y-%m-%dT%H:%M:%S",
            "%Y-%m-%d %H:%M:%S%.f",
            "%Y-%m-%d %H:%M:%S",
        ] {
            if let Ok(ndt) = NaiveDateTime::parse_from_str(base, fmt) {
                return Some(Utc.from_utc_datetime(&ndt));
            }
        }
    }
    None
}

/// `desktop_helpers.reset_at`: years 2000..=2100, seconds precision.
pub(crate) fn reset_at(value: &Value) -> Option<String> {
    if let Some(mut secs) = number(value) {
        if secs > 100_000_000_000.0 {
            secs /= 1000.0;
        }
        let (iso, year) = aac_num::py::iso_seconds_of_timestamp(secs)?;
        return (2000..=2100).contains(&year).then_some(iso);
    }
    let raw = value.as_str()?;
    let dt = parse_iso_utc(raw)?;
    if !(2000..=2100).contains(&dt.year()) {
        return None;
    }
    Some(dt.to_rfc3339_opts(SecondsFormat::Secs, true))
}

#[derive(Default)]
pub(crate) struct WindowSpec<'a> {
    pub key: &'a str,
    pub label: &'a str,
    pub used_percent: Option<f64>,
    pub remaining_percent: Option<f64>,
    pub reset: Option<&'a Value>,
    pub minutes: Option<f64>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub unit: Option<String>,
    pub kind: &'a str,
    pub remaining: Option<f64>,
    pub expires: Option<&'a Value>,
    pub unlimited: Option<bool>,
    pub enabled: Option<bool>,
}

pub(crate) fn quota_window(spec: WindowSpec<'_>) -> Value {
    let mut used_pct = spec
        .used_percent
        .filter(|n| n.is_finite() && *n >= 0.0)
        .and_then(|n| aac_num::py::round(n, 4));
    let mut rem_pct = spec
        .remaining_percent
        .filter(|n| n.is_finite() && *n >= 0.0)
        .and_then(|n| aac_num::py::round(n, 4));
    if used_pct.is_none()
        && let Some(r) = rem_pct
    {
        used_pct = aac_num::py::round(100.0 - r, 4).map(|v| v.max(0.0));
    }
    if rem_pct.is_none()
        && let Some(u) = used_pct
    {
        rem_pct = aac_num::py::round(100.0 - u, 4).map(|v| v.max(0.0));
    }
    let minutes = spec
        .minutes
        .filter(|m| m.is_finite() && *m > 0.0 && *m <= 525_600.0);
    let kind = if spec.kind.is_empty() {
        "rate_limit"
    } else {
        spec.kind
    };
    let mut map = Map::new();
    map.insert("key".into(), json!(spec.key));
    map.insert("label".into(), json!(spec.label));
    map.insert("usedPercent".into(), json!(used_pct));
    map.insert("remainingPercent".into(), json!(rem_pct));
    map.insert("resetAt".into(), json!(spec.reset.and_then(reset_at)));
    map.insert("windowMinutes".into(), json!(minutes));
    map.insert(
        "used".into(),
        json!(spec.used.filter(|n| n.is_finite() && *n >= 0.0)),
    );
    map.insert(
        "limit".into(),
        json!(spec.limit.filter(|n| n.is_finite() && *n >= 0.0)),
    );
    map.insert("unit".into(), json!(spec.unit));
    map.insert("kind".into(), json!(kind));
    if let Some(rem) = spec.remaining {
        map.insert(
            "remaining".into(),
            json!((rem.is_finite() && rem >= 0.0).then_some(rem)),
        );
    }
    if let Some(exp) = spec.expires {
        map.insert("expiresAt".into(), json!(reset_at(exp)));
    }
    if let Some(u) = spec.unlimited {
        map.insert("unlimited".into(), json!(u));
    }
    if let Some(e) = spec.enabled {
        map.insert("enabled".into(), json!(e));
    }
    Value::Object(map)
}

pub(crate) fn platform_label(platform: &str) -> Option<&'static str> {
    match platform {
        "mac" => Some("Mac"),
        "windows" => Some("Windows"),
        "ubuntu" => Some("Ubuntu"),
        _ => None,
    }
}

pub(crate) fn account(provider: &str, platform: &str) -> Value {
    let label = match provider {
        "antigravity" => "Antigravity CLI",
        "muse" => "Muse Code",
        "cursor" => "Cursor",
        _ => provider,
    };
    let host = platform_label(platform).unwrap_or("Ubuntu");
    json!({
        "id": format!("native:{provider}:{platform}"),
        "provider": provider,
        "providerLabel": label,
        "label": label,
        "email": null,
        "plan": null,
        "platform": platform,
        "source": format!("Native account on {host}"),
        "status": "unavailable",
        "message": "No readable signed-in account was found on this computer.",
        "fetchedAt": null,
        "sampledAt": null,
        "isActive": false,
        "windows": [],
        "capabilities": {
            "codexProfile": null,
            "claudeProfileId": null,
            "claudePlatforms": []
        }
    })
}

pub(crate) fn jwt_claims(token: &str) -> Value {
    let mut parts = token.split('.');
    let (Some(_h), Some(payload), Some(_s), None) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return json!({});
    };
    if payload.len() > MAX_RESPONSE_BYTES {
        return json!({});
    }
    let Ok(bytes) = FORGIVING_URL_SAFE.decode(payload) else {
        return json!({});
    };
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(val) if val.is_object() => val,
        _ => json!({}),
    }
}

pub(crate) fn env_get(name: &str, env_vars: Option<&BTreeMap<String, String>>) -> Option<String> {
    if let Some(map) = env_vars {
        map.get(name).cloned()
    } else {
        std::env::var(name).ok()
    }
}

pub(crate) fn config_home(home: &Path, env_vars: Option<&BTreeMap<String, String>>) -> PathBuf {
    if let Some(value) = env_get("XDG_CONFIG_HOME", env_vars)
        && !value.is_empty()
    {
        let p = PathBuf::from(&value);
        if p.is_absolute() {
            return p;
        }
    }
    home.join(".config")
}

pub(crate) fn cursor_root(
    home: &Path,
    platform: &str,
    env_vars: Option<&BTreeMap<String, String>>,
) -> PathBuf {
    match platform {
        "mac" => home.join("Library/Application Support/Cursor"),
        "windows" => {
            if let Some(appdata) = env_get("APPDATA", env_vars).filter(|s| !s.is_empty()) {
                PathBuf::from(appdata).join("Cursor")
            } else {
                home.join("AppData/Roaming/Cursor")
            }
        }
        _ => config_home(home, env_vars).join("Cursor"),
    }
}

/// Read a JSON object file up to 64 KiB via `aac_files::read`, counting unreadable present files.
pub(crate) fn read_json_file(path: &Path, tracker: &mut ReadTracker) -> Option<Value> {
    let policy = ReadPolicy::capped(MAX_RESPONSE_BYTES as u64);
    match aac_files::read::read(path, &policy) {
        Read::Absent | Read::Unreadable(ErrorKind::NotADirectory) => None,
        Read::Bytes(bytes, _) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) if v.is_object() => Some(v),
            _ => {
                tracker.record_failure();
                None
            }
        },
        _ => {
            tracker.record_failure();
            None
        }
    }
}

pub(crate) fn credential_from_omp(provider: &str, value: &Value) -> Option<Credential> {
    let mut token_val = value.get("access").cloned().unwrap_or(Value::Null);
    if provider == "muse" {
        if let Some(raw) = token_val.as_str()
            && raw.starts_with('{')
        {
            let Ok(packed) = serde_json::from_str::<Value>(raw) else {
                return None;
            };
            if let Some(obj) = packed.as_object() {
                token_val = obj.get("oauthAccessToken").cloned().unwrap_or(Value::Null);
            }
        }
        if !token_val.as_str().is_some_and(|s| s.starts_with("dca:")) {
            return None;
        }
    }
    let token = secret(&token_val)?;
    let refresh = value.get("refresh").and_then(secret).unwrap_or_default();
    let expires = value.get("expires").and_then(number);
    let addr = value.get("email").and_then(email);
    let project = value.get("projectId").and_then(|v| safe_text(v, 160));
    Some(Credential {
        access: token,
        source: "OMP".to_owned(),
        refresh,
        expires,
        email: addr,
        project,
        plan: None,
        google_client: (provider == "antigravity").then(|| "antigravity".to_owned()),
    })
}

fn format_py_g(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

pub(crate) fn muse_window_label(key: &str, minutes: Option<f64>) -> String {
    if key == "weekly" {
        return "Weekly limit".to_owned();
    }
    let Some(m) = minutes.filter(|m| m.is_finite() && *m > 0.0 && *m <= 525_600.0) else {
        return "Current usage".to_owned();
    };
    if m % 60.0 == 0.0 {
        format!("Current usage ({}-hour)", format_py_g(m / 60.0))
    } else {
        format!("Current usage ({}-minute)", format_py_g(m))
    }
}

pub(crate) fn normalize_muse(payload: &Value) -> Vec<Value> {
    let mut windows = Vec::new();
    if let Some(portal) = payload.get("subscription_quota").and_then(Value::as_object) {
        for key in ["window", "weekly"] {
            let used = portal
                .get(&format!("{key}_weighted_used"))
                .and_then(counter);
            let limit = portal
                .get(&format!("{key}_weighted_limit"))
                .and_then(counter);
            let duration = if key == "window" {
                portal.get("window_duration_secs").and_then(counter)
            } else {
                None
            };
            let minutes = duration
                .map(|d| d / 60.0)
                .or_else(|| (key == "weekly").then_some(10_080.0));
            let label = muse_window_label(key, minutes);
            let reset_val = portal.get(&format!("{key}_resets_at"));
            let reset_owned = reset_val.map(|v| {
                if v.is_string()
                    && let Some(c) = counter(v)
                {
                    return json!(c);
                }
                v.clone()
            });
            let reset_norm = reset_owned.as_ref().and_then(reset_at);
            if used.is_none() && reset_norm.is_none() {
                continue;
            }
            let ratio = match (used, limit) {
                (Some(u), Some(l)) if l > 0.0 => Some(u / l * 100.0),
                _ => None,
            };
            windows.push(quota_window(WindowSpec {
                key,
                label: &label,
                used_percent: ratio,
                reset: reset_owned.as_ref(),
                minutes,
                used,
                limit,
                unit: Some("weighted tokens".to_owned()),
                kind: "rate_limit",
                ..Default::default()
            }));
        }
        return windows;
    }
    if let Some(usage) = payload.get("subs_usage").and_then(Value::as_object) {
        for (key, default_dur) in [("window", None), ("weekly", Some(10_080.0))] {
            if let Some(bucket) = usage.get(key).and_then(Value::as_object) {
                let dur = bucket
                    .get("window_duration_mins")
                    .and_then(number)
                    .or(default_dur);
                let label = muse_window_label(key, dur);
                let w = quota_window(WindowSpec {
                    key,
                    label: &label,
                    used_percent: bucket.get("used_percent").and_then(number),
                    reset: bucket.get("resets_at"),
                    minutes: dur,
                    kind: "rate_limit",
                    enabled: bucket.get("enabled").and_then(Value::as_bool),
                    unlimited: bucket.get("unlimited").and_then(Value::as_bool),
                    ..Default::default()
                });
                if !w["usedPercent"].is_null() || !w["resetAt"].is_null() {
                    windows.push(w);
                }
            }
        }
    }
    windows
}

pub(crate) fn agy_groups(payload: &Value) -> Vec<Value> {
    let mut cur = payload;
    for _ in 0..4 {
        let Some(obj) = cur.as_object() else {
            break;
        };
        if let Some(groups) = obj.get("groups").and_then(Value::as_array) {
            return groups.iter().take(32).cloned().collect();
        }
        if let Some(buckets) = obj.get("buckets").and_then(Value::as_array) {
            return vec![json!({ "displayName": "Antigravity", "buckets": buckets })];
        }
        let nested = ["quotaSummary", "summary", "response"]
            .into_iter()
            .find_map(|k| obj.get(k).filter(|v| v.is_object()));
        match nested {
            Some(next) => cur = next,
            None => break,
        }
    }
    Vec::new()
}

pub(crate) fn normalize_antigravity(payload: &Value) -> Vec<Value> {
    let mut windows = Vec::new();
    let mut keys = BTreeSet::new();
    for (group_index, group) in agy_groups(payload).iter().enumerate() {
        let Some(g_obj) = group.as_object() else {
            continue;
        };
        let group_label = g_obj
            .get("displayName")
            .and_then(|v| safe_text(v, 160))
            .unwrap_or_else(|| "Antigravity".to_owned());
        let Some(buckets) = g_obj.get("buckets").and_then(Value::as_array) else {
            continue;
        };
        for (bucket_index, bucket) in buckets.iter().take(32).enumerate() {
            let Some(b_obj) = bucket.as_object() else {
                continue;
            };
            let mut fraction = b_obj.get("remainingFraction").and_then(number);
            let mut remaining_amount = b_obj.get("remainingAmount").and_then(counter);
            if fraction.is_none()
                && let Some(rem) = b_obj.get("remaining").and_then(Value::as_object)
            {
                fraction = rem.get("remainingFraction").and_then(number);
                if fraction.is_none()
                    && rem.get("case").and_then(Value::as_str) == Some("remainingFraction")
                {
                    fraction = rem.get("value").and_then(number);
                }
                if remaining_amount.is_none() {
                    remaining_amount = rem.get("remainingAmount").and_then(counter);
                    if remaining_amount.is_none()
                        && rem.get("case").and_then(Value::as_str) == Some("remainingAmount")
                    {
                        remaining_amount = rem.get("value").and_then(counter);
                    }
                }
            }
            let fraction = fraction.filter(|f| (0.0..=1.0).contains(f));
            let period = b_obj.get("window").and_then(Value::as_str);
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
            let bucket_label = b_obj.get("displayName").and_then(|v| safe_text(v, 160));
            let label = if let Some(pl) = period_label {
                format!("{group_label} · {pl}")
            } else if let Some(bl) = bucket_label
                && bl != group_label
            {
                format!("{group_label} · {bl}")
            } else {
                group_label.clone()
            };
            let mut key = b_obj
                .get("bucketId")
                .and_then(|v| safe_text(v, 160))
                .unwrap_or_else(|| format!("group-{group_index}-{bucket_index}"));
            if keys.contains(&key) {
                key = format!("{key}-{group_index}-{bucket_index}");
            }
            let w = quota_window(WindowSpec {
                key: &key,
                label: &label,
                remaining_percent: fraction.map(|f| f * 100.0),
                reset: b_obj.get("resetTime"),
                minutes,
                remaining: remaining_amount,
                unit: b_obj.get("unit").and_then(|v| safe_text(v, 32)),
                kind: "rate_limit",
                enabled: b_obj.get("disabled").and_then(Value::as_bool).map(|b| !b),
                unlimited: b_obj.get("unlimited").and_then(Value::as_bool),
                ..Default::default()
            });
            if !w["remainingPercent"].is_null()
                || !w["resetAt"].is_null()
                || w.get("remaining").is_some_and(|v| !v.is_null())
            {
                keys.insert(key);
                windows.push(w);
            }
            if windows.len() >= 64 {
                return windows;
            }
        }
    }
    windows
}

pub(crate) fn antigravity_reported_plan(load: &Value) -> (Option<String>, Option<Value>) {
    let mut reported = Map::new();
    let plan_type = load
        .get("planInfo")
        .and_then(|v| v.get("planType"))
        .and_then(|v| safe_text(v, 80));
    if let Some(ref pt) = plan_type {
        reported.insert("planType".into(), json!(pt));
    }
    for key in ["paidTier", "currentTier"] {
        if let Some(tier) = load.get(key).and_then(Value::as_object) {
            let mut display = Map::new();
            for field in ["id", "name"] {
                if let Some(val) = tier.get(field).and_then(|v| safe_text(v, 80)) {
                    display.insert(field.into(), json!(val));
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
    let plan = plan_type.or_else(|| {
        tier.and_then(|t| {
            t.get("name")
                .or_else(|| t.get("id"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
    });
    let reported_out = (!reported.is_empty()).then_some(Value::Object(reported));
    (plan, reported_out)
}

pub(crate) fn normalize_antigravity_credits(load: &Value) -> Vec<Value> {
    let Some(credits) = load
        .get("paidTier")
        .and_then(|v| v.get("availableCredits"))
        .and_then(Value::as_array)
        .filter(|arr| !arr.is_empty() && arr.len() <= 64)
    else {
        return Vec::new();
    };
    let amounts: Vec<f64> = credits
        .iter()
        .filter_map(|item| item.get("creditAmount").and_then(counter))
        .collect();
    if amounts.is_empty() {
        return Vec::new();
    }
    let total: f64 = amounts.into_iter().sum();
    if !total.is_finite() || total < 0.0 {
        return Vec::new();
    }
    vec![quota_window(WindowSpec {
        key: "google-ai-credits",
        label: "AI credits (overage)",
        kind: "balance",
        remaining: Some(total),
        unit: Some("credits".to_owned()),
        ..Default::default()
    })]
}

pub(crate) fn antigravity_unavailable_windows(windows: &[Value]) -> Vec<Value> {
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
                "usedPercent": null,
                "remainingPercent": null,
                "resetAt": null,
                "windowMinutes": obj.get("windowMinutes").cloned().unwrap_or(Value::Null),
                "used": null,
                "limit": null,
                "unit": null,
                "kind": kind,
                "status": "unavailable"
            }))
        })
        .collect()
}

pub(crate) fn antigravity_service_label(mut windows: Vec<Value>, service: &str) -> Vec<Value> {
    let where_label = if service == "cli" {
        "In T3 (Antigravity CLI)"
    } else {
        "In the Antigravity app"
    };
    for w in &mut windows {
        if let Some(obj) = w.as_object_mut() {
            if service == "cli"
                && let Some(key) = obj.get("key").and_then(Value::as_str)
            {
                obj.insert("key".into(), json!(format!("{AGY_CLI_KEY_PREFIX}{key}")));
            }
            let base_label = obj
                .get("label")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
                .unwrap_or("Antigravity");
            obj.insert(
                "label".into(),
                json!(format!("{base_label} \u{00b7} {where_label}")),
            );
        }
    }
    windows
}

/// Strictly decode JSON rejecting duplicate object keys at every depth and requiring an object root.
pub(crate) fn decode_strict_json_object(raw: &[u8]) -> Option<Value> {
    let parsed: StrictValue = serde_json::from_slice(raw).ok()?;
    parsed.0.is_object().then_some(parsed.0)
}

struct StrictValue(Value);

impl<'de> Deserialize<'de> for StrictValue {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct StrictVisitor;
        impl<'de> Visitor<'de> for StrictVisitor {
            type Value = StrictValue;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("valid JSON with unique object keys")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Bool(v)))
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(v.into())))
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Number(v.into())))
            }

            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                let n = serde_json::Number::from_f64(v)
                    .ok_or_else(|| de::Error::custom("non-finite JSON number"))?;
                Ok(StrictValue(Value::Number(n)))
            }

            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v.to_owned())))
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::String(v)))
            }

            fn visit_none<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E> {
                Ok(StrictValue(Value::Null))
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                let mut vec = Vec::new();
                while let Some(elem) = seq.next_element::<StrictValue>()? {
                    vec.push(elem.0);
                }
                Ok(StrictValue(Value::Array(vec)))
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = Map::new();
                while let Some((k, v)) = map.next_entry::<String, StrictValue>()? {
                    if out.contains_key(&k) {
                        return Err(de::Error::custom("duplicate JSON key"));
                    }
                    out.insert(k, v.0);
                }
                Ok(StrictValue(Value::Object(out)))
            }
        }
        deserializer.deserialize_any(StrictVisitor)
    }
}

/// Read installed-app Antigravity OAuth client credentials privately (`desktop_usage.antigravity_oauth_client`).
pub(crate) fn antigravity_oauth_client(home: &Path) -> Result<(String, String), UsageError> {
    let failure = || {
        UsageError::new(
            "unavailable",
            "Antigravity's private app OAuth metadata is unavailable on this computer.",
        )
    };
    let path = home.join(".ccs/account-usage/antigravity-oauth-client.json");
    if let Some(parent) = path.parent()
        && std::fs::symlink_metadata(parent).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err(failure());
    }
    let root = path.ancestors().last().unwrap_or(&path);
    let policy = ReadPolicy::capped(4096)
        .ancestors(Ancestors::NoLinkBelow(root))
        .owner(Owner::Mine)
        .mode(Mode::Private);
    let (raw, meta) = match aac_files::read::read(&path, &policy) {
        Read::Bytes(bytes, meta) => (bytes, meta),
        _ => return Err(failure()),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o777 != 0o600 {
            return Err(failure());
        }
    }
    #[cfg(not(unix))]
    let _ = meta;

    let value = decode_strict_json_object(&raw).ok_or_else(failure)?;
    let obj = value.as_object().ok_or_else(failure)?;
    if obj.len() != 3 {
        return Err(failure());
    }
    let version = if obj.contains_key("schemaVersion") {
        obj.get("schemaVersion")
    } else if obj.contains_key("schema") {
        obj.get("schema")
    } else {
        return Err(failure());
    };
    if version.and_then(Value::as_i64) != Some(1) {
        return Err(failure());
    }
    let client_id = obj
        .get("clientId")
        .and_then(Value::as_str)
        .filter(|s| GOOGLE_CLIENT_ID_RE.is_match(s))
        .ok_or_else(failure)?;
    let client_secret = obj
        .get("clientSecret")
        .and_then(Value::as_str)
        .filter(|s| GOOGLE_CLIENT_SECRET_RE.is_match(s))
        .ok_or_else(failure)?;
    Ok((client_id.to_owned(), client_secret.to_owned()))
}

/// Serialize `Value` identically to Python `json.dumps(v, ensure_ascii=True, sort_keys=True, separators=(",", ":"))`.
pub(crate) fn canonical_ascii_json(value: &Value) -> String {
    let mut out = String::new();
    write_canonical_ascii(&mut out, value);
    out
}

fn write_canonical_ascii(out: &mut String, value: &Value) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                out.push_str(&i.to_string());
            } else if let Some(u) = n.as_u64() {
                out.push_str(&u.to_string());
            } else if let Some(f) = n.as_f64() {
                out.push_str(&aac_num::py::repr(f));
            }
        }
        Value::String(s) => write_ascii_json_string(out, s),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical_ascii(out, item);
            }
            out.push(']');
        }
        Value::Object(map) => {
            let sorted: BTreeMap<&str, &Value> = map.iter().map(|(k, v)| (k.as_str(), v)).collect();
            out.push('{');
            for (i, (k, v)) in sorted.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_ascii_json_string(out, k);
                out.push(':');
                write_canonical_ascii(out, v);
            }
            out.push('}');
        }
    }
}

fn write_ascii_json_string(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c if c.is_ascii() => out.push(c),
            c => {
                let mut buf = [0u16; 2];
                for unit in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}
