//! Exact port of the `muse_console.py` and `muse_native.py` behaviors used by `failover_additional.py`.
use crate::failover_additional::{
    PortalError, PortalTransport,
    desktop::{
        canonical_ascii_json, normalize_muse, now_unix_secs, parse_iso_utc, reset_at, safe_text,
        utc_now_iso_secs,
    },
};
use aac_core::sha256_hex;
use aac_files::{
    BackupClass, Class, OwnedStore, Registry, StoreSpec,
    read::{Ancestors, Mode, Owner, Read, ReadPolicy},
};
use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    fs,
    future::Future,
    io::ErrorKind,
    path::Path,
    pin::Pin,
    sync::LazyLock,
    time::{Duration, Instant},
};

pub(crate) const ORIGIN: &str = "https://dev.meta.ai";
pub(crate) const CAPSULE_NAME: &str = "muse-console-session.json";
const CACHE_NAME: &str = "muse-console-usage.json";
const PLAN_NAME: &str = "muse-console-plan-verification.json";
const BINDING_NAME: &str = "muse-console-binding.json";

const REFRESH_SECONDS: f64 = 300.0;
const RATE_LIMIT_SECONDS: f64 = 600.0;
const MAX_RETRY_SECONDS: f64 = 86_400.0;
const MIN_PROVIDER_AS_OF: f64 = 946_684_800.0;
const MAX_PROVIDER_AS_OF: f64 = 4_102_444_800.0;
const AS_OF_FUTURE_SKEW_SECONDS: f64 = 300.0;
const MAX_BYTES: usize = 65_536;
const NATIVE_MAX_BYTES: usize = 128 * 1024;

const USAGE_FIELDS: [&str; 9] = [
    "schemaVersion",
    "email",
    "plan",
    "teamId",
    "credentialFingerprint",
    "quota",
    "sampledAt",
    "nextRequestAt",
    "lastError",
];
const QUOTA_FIELDS: [&str; 9] = [
    "tier",
    "window_weighted_used",
    "window_weighted_limit",
    "window_duration_secs",
    "window_resets_at",
    "weekly_weighted_used",
    "weekly_weighted_limit",
    "weekly_resets_at",
    "as_of",
];

static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[^ \t\r\n\f\v@]+@[^ \t\r\n\f\v@]+\.[^ \t\r\n\f\v@]+\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static PLAN_BAD_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\x00-\x1f\x7f]|dca:|bearer[ \t\r\n\f\v]|sk-|eyJ")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static TEAM_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9]{1,32}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static HASH64_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9a-f]{64}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static AS_OF_STR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9]+(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static PORTAL_QUOTA_ROUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A/api/portal/teams/[0-9]{1,32}/subscription-quota\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});

/// Structured error matching `muse_console.MuseError`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MuseError {
    pub code: &'static str,
    pub team_id: Option<String>,
    pub retry_after: Option<f64>,
    pub plan: Option<String>,
    pub plan_at: Option<String>,
}

impl MuseError {
    pub(crate) fn new(code: &'static str) -> Self {
        Self {
            code: normalize_error_code(code),
            team_id: None,
            retry_after: None,
            plan: None,
            plan_at: None,
        }
    }

    pub(crate) fn with_retry(code: &'static str, retry_after: Option<f64>) -> Self {
        Self {
            code: normalize_error_code(code),
            team_id: None,
            retry_after,
            plan: None,
            plan_at: None,
        }
    }

    pub(crate) fn with_plan(
        code: &'static str,
        retry_after: Option<f64>,
        plan: Option<String>,
        plan_at: Option<String>,
    ) -> Self {
        let valid = plan.filter(|p| valid_plan(p));
        let valid_at = if valid.is_some() { plan_at } else { None };
        Self {
            code: normalize_error_code(code),
            team_id: None,
            retry_after,
            plan: valid,
            plan_at: valid_at,
        }
    }
}

pub(crate) fn normalize_error_code(code: &str) -> &'static str {
    match code {
        "no_browser_cookie" => "no_browser_cookie",
        "needs_sign_in" => "needs_sign_in",
        "account_mismatch" => "account_mismatch",
        "identity_unavailable" => "identity_unavailable",
        "plan_mismatch" => "plan_mismatch",
        "choose_team" => "choose_team",
        "team_mismatch" => "team_mismatch",
        "inactive_subscription" => "inactive_subscription",
        "network_error" => "network_error",
        "provider_error" => "provider_error",
        "rate_limited" => "rate_limited",
        "invalid_response" => "invalid_response",
        "invalid_request" => "invalid_request",
        "local_storage_error" => "local_storage_error",
        "host_unavailable" => "host_unavailable",
        "busy" => "busy",
        _ => "error",
    }
}

fn valid_email(s: &str) -> bool {
    s.chars().count() <= 254 && EMAIL_RE.is_match(s)
}

pub(crate) fn valid_plan(s: &str) -> bool {
    !s.is_empty() && s.chars().count() <= 160 && !PLAN_BAD_RE.is_match(s)
}

fn valid_team(s: &str) -> bool {
    TEAM_ID_RE.is_match(s)
}

fn valid_hash(s: &str) -> bool {
    HASH64_RE.is_match(s)
}

pub(crate) fn plan_timestamp(now_secs: f64) -> String {
    let ms = (now_secs * 1000.0).trunc() as i64;
    aac_num::py::iso_millis(ms).unwrap_or_else(|| utc_now_iso_secs(Some(now_secs)))
}

pub(crate) fn valid_plan_timestamp(value: Option<&str>, now_secs: Option<f64>) -> bool {
    let Some(s) = value.filter(|s| s.len() <= 40) else {
        return false;
    };
    let Some(dt) = parse_iso_utc(s) else {
        return false;
    };
    let stamp = dt.timestamp() as f64 + f64::from(dt.timestamp_subsec_nanos()) / 1e9;
    let now = now_unix_secs(now_secs);
    stamp >= 0.0 && stamp <= now + 5.0
}

pub(crate) fn session_values(cookies: &[Value]) -> Vec<[String; 2]> {
    let mut pairs: Vec<[String; 2]> = cookies
        .iter()
        .filter_map(|row| {
            let name = row.get("name")?.as_str()?.to_owned();
            let value = row.get("value")?.as_str()?.to_owned();
            Some([name, value])
        })
        .collect();
    pairs.sort();
    pairs
}

pub(crate) fn validate_cookies(values: &Value, now: f64) -> Result<Vec<Value>, MuseError> {
    let Some(rows) = values.as_array() else {
        return Err(MuseError::new("no_browser_cookie"));
    };
    if rows.is_empty() || rows.len() > 8 {
        return Err(MuseError::new("no_browser_cookie"));
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let Some(obj) = row.as_object() else {
            return Err(MuseError::new("invalid_request"));
        };
        if obj.keys().any(|k| {
            !matches!(
                k.as_str(),
                "name" | "value" | "domain" | "path" | "secure" | "hostOnly" | "expirationDate"
            )
        }) {
            return Err(MuseError::new("invalid_request"));
        }
        let Some(name) = obj
            .get("name")
            .and_then(Value::as_str)
            .filter(|n| matches!(*n, "llama_dev_sess" | "llm_sess"))
        else {
            return Err(MuseError::new("invalid_request"));
        };
        let Some(domain) = obj
            .get("domain")
            .and_then(Value::as_str)
            .filter(|d| matches!(*d, "dev.meta.ai" | ".dev.meta.ai"))
        else {
            return Err(MuseError::new("invalid_request"));
        };
        let Some(host_only) = obj.get("hostOnly").and_then(Value::as_bool) else {
            return Err(MuseError::new("invalid_request"));
        };
        let Some(val) = obj.get("value").and_then(Value::as_str) else {
            return Err(MuseError::new("invalid_request"));
        };
        if obj.get("path").and_then(Value::as_str) != Some("/")
            || obj.get("secure") != Some(&Value::Bool(true))
            || val.is_empty()
            || val.chars().count() > 8192
            || val
                .chars()
                .any(|c| c == ';' || (c as u32) <= 0x20 || (c as u32) == 0x7f)
            || val.starts_with("dca:")
            || val.starts_with("sk-")
        {
            return Err(MuseError::new("invalid_request"));
        }
        if host_only && domain != "dev.meta.ai" {
            return Err(MuseError::new("invalid_request"));
        }
        if let Some(exp_val) = obj.get("expirationDate").filter(|v| !v.is_null()) {
            let Some(exp) = exp_val.as_f64().filter(|n| n.is_finite()) else {
                return Err(MuseError::new("invalid_request"));
            };
            if exp <= now {
                continue;
            }
        }
        if !seen.insert(name.to_owned()) {
            return Err(MuseError::new("invalid_request"));
        }
        result.push(Value::Object(obj.clone()));
    }
    if result.is_empty() {
        return Err(MuseError::new("no_browser_cookie"));
    }
    Ok(result)
}

fn check_private_root(root: &Path) -> Result<(), MuseError> {
    #[cfg(not(unix))]
    {
        let _ = root;
        Err(MuseError::new("local_storage_error"))
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::symlink_metadata(root).map_err(|_| MuseError::new("local_storage_error"))?;
        if meta.file_type().is_symlink() || !meta.is_dir() {
            return Err(MuseError::new("local_storage_error"));
        }
        if meta.uid() != aac_files::path::uid() || (meta.mode() & 0o777) != 0o700 {
            return Err(MuseError::new("local_storage_error"));
        }
        Ok(())
    }
}

fn read_private_json_exact(root: &Path, name: &str) -> Result<Option<Value>, MuseError> {
    check_private_root(root)?;
    let path = root.join(name);
    match fs::symlink_metadata(&path) {
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(MuseError::new("local_storage_error")),
        Ok(m) if m.file_type().is_symlink() => return Err(MuseError::new("local_storage_error")),
        Ok(_) => {}
    }
    let policy = ReadPolicy::capped(MAX_BYTES as u64)
        .owner(Owner::Mine)
        .mode(Mode::Private);
    let (bytes, meta) = match aac_files::read::read(&path, &policy) {
        Read::Absent => return Ok(None),
        Read::Bytes(b, m) => (b, m),
        _ => return Err(MuseError::new("local_storage_error")),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if (meta.mode() & 0o777) != 0o600 {
            return Err(MuseError::new("local_storage_error"));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| MuseError::new("local_storage_error"))
}

/// Read `muse-console-session.json` (`muse_console.read_capsule`).
pub(crate) fn read_capsule(
    root: &Path,
    allow_expired: bool,
    now_secs: Option<f64>,
) -> Result<(Vec<Value>, String, String, String), MuseError> {
    let path = root.join(CAPSULE_NAME);
    match fs::symlink_metadata(&path) {
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Err(MuseError::new("no_browser_cookie"));
        }
        Err(_) => return Err(MuseError::new("local_storage_error")),
        Ok(m) if m.file_type().is_symlink() => return Err(MuseError::new("local_storage_error")),
        Ok(_) => {}
    }
    let Some(value) = read_private_json_exact(root, CAPSULE_NAME)? else {
        return Err(MuseError::new("no_browser_cookie"));
    };
    let Some(obj) = value.as_object() else {
        return Err(MuseError::new("local_storage_error"));
    };
    let expected_keys = [
        "schemaVersion",
        "provider",
        "origin",
        "email",
        "plan",
        "teamId",
        "cookies",
    ];
    if obj.len() != expected_keys.len() || !expected_keys.iter().all(|k| obj.contains_key(*k)) {
        return Err(MuseError::new("local_storage_error"));
    }
    if obj.get("schemaVersion").and_then(Value::as_i64) != Some(1)
        || obj.get("provider").and_then(Value::as_str) != Some("muse")
        || obj.get("origin").and_then(Value::as_str) != Some(ORIGIN)
    {
        return Err(MuseError::new("local_storage_error"));
    }
    let email = obj
        .get("email")
        .and_then(Value::as_str)
        .filter(|e| valid_email(e))
        .ok_or_else(|| MuseError::new("local_storage_error"))?;
    let plan = obj
        .get("plan")
        .and_then(Value::as_str)
        .filter(|p| valid_plan(p))
        .ok_or_else(|| MuseError::new("local_storage_error"))?;
    let team_id = obj
        .get("teamId")
        .and_then(Value::as_str)
        .filter(|t| valid_team(t))
        .ok_or_else(|| MuseError::new("local_storage_error"))?;
    let now = if allow_expired {
        0.0
    } else {
        now_unix_secs(now_secs)
    };
    let cookies = validate_cookies(obj.get("cookies").unwrap_or(&Value::Null), now)?;
    Ok((
        cookies,
        team_id.to_owned(),
        email.to_owned(),
        plan.to_owned(),
    ))
}

fn claim_store(root: &Path, name: &str, max_size: usize) -> Result<OwnedStore, MuseError> {
    let canonical = aac_files::path::python::normpath(root);
    let mut registry = Registry::default();
    registry
        .register(StoreSpec {
            path: name.into(),
            class: Class::Cache,
            mode: 0o600,
            max_size,
            owner: "muse-usage".into(),
            schema: "muse-usage-v1".into(),
            backup: BackupClass::Rebuild,
        })
        .map_err(|_| MuseError::new("local_storage_error"))?;
    registry
        .claim(&canonical, Path::new(name), "muse-usage")
        .map_err(|e| {
            if e.cause == aac_core::Cause::Busy {
                MuseError::new("busy")
            } else {
                MuseError::new("local_storage_error")
            }
        })
}

fn write_atomic_private(
    root: &Path,
    name: &str,
    value: &Value,
    max_size: usize,
) -> Result<(), MuseError> {
    check_private_root(root)?;
    let dest = root.join(name);
    if fs::symlink_metadata(&dest).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(MuseError::new("local_storage_error"));
    }
    let bytes = serde_json::to_vec(value).map_err(|_| MuseError::new("local_storage_error"))?;
    if bytes.len() > max_size {
        return Err(MuseError::new("local_storage_error"));
    }
    let store = claim_store(root, name, max_size)?;
    store
        .write_private_cache(&bytes)
        .map_err(|_| MuseError::new("local_storage_error"))
}

fn fingerprint(access: &str) -> Result<String, MuseError> {
    if !access.starts_with("dca:") || access.chars().count() > 32_768 {
        return Err(MuseError::new("account_mismatch"));
    }
    Ok(sha256_hex(access.as_bytes()))
}

fn usage_state_fingerprint(state: &Map<String, Value>) -> String {
    let mut payload = Map::new();
    for key in USAGE_FIELDS {
        if let Some(v) = state.get(key) {
            payload.insert(key.to_owned(), v.clone());
        }
    }
    sha256_hex(canonical_ascii_json(&Value::Object(payload)).as_bytes())
}

fn read_plan_verification(
    root: &Path,
    state: &Map<String, Value>,
    now: f64,
) -> (Option<String>, bool) {
    let Ok(Some(value)) = read_private_json_exact(root, PLAN_NAME) else {
        return (None, false);
    };
    let Some(obj) = value.as_object() else {
        return (None, false);
    };
    let Some(version) = obj.get("schemaVersion").and_then(Value::as_i64) else {
        return (None, false);
    };
    if !matches!(version, 1 | 2) {
        return (None, false);
    }
    let expected_len = if version == 2 { 4 } else { 3 };
    if obj.len() != expected_len
        || !obj.contains_key("stateHash")
        || !obj.contains_key("verifiedAt")
        || (version == 2 && !obj.get("restoreAllowed").is_some_and(Value::is_boolean))
    {
        return (None, false);
    }
    if obj.get("stateHash").and_then(Value::as_str) != Some(usage_state_fingerprint(state).as_str())
    {
        return (None, false);
    }
    let verified_at = obj.get("verifiedAt").and_then(Value::as_str);
    if !valid_plan_timestamp(verified_at, Some(now)) {
        return (None, false);
    }
    let restore = obj
        .get("restoreAllowed")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    (verified_at.map(str::to_owned), restore)
}

#[derive(Clone)]
struct UsageState {
    raw: Map<String, Value>,
    plan_verified: bool,
    plan_verified_at: Option<String>,
    restore_allowed: bool,
}

fn read_usage_state(root: &Path, now: f64) -> Result<Option<UsageState>, MuseError> {
    let Some(value) = read_private_json_exact(root, CACHE_NAME)? else {
        return Ok(None);
    };
    let Some(obj) = value.as_object() else {
        return Err(MuseError::new("local_storage_error"));
    };
    if obj.len() != USAGE_FIELDS.len() || !USAGE_FIELDS.iter().all(|k| obj.contains_key(*k)) {
        return Err(MuseError::new("local_storage_error"));
    }
    if obj.get("schemaVersion").and_then(Value::as_i64) != Some(1)
        || !obj
            .get("email")
            .and_then(Value::as_str)
            .is_some_and(valid_email)
        || !obj
            .get("plan")
            .and_then(Value::as_str)
            .is_some_and(valid_plan)
        || !obj
            .get("teamId")
            .and_then(Value::as_str)
            .is_some_and(valid_team)
        || !obj
            .get("credentialFingerprint")
            .and_then(Value::as_str)
            .is_some_and(valid_hash)
        || !obj
            .get("nextRequestAt")
            .and_then(Value::as_f64)
            .is_some_and(|n| n.is_finite() && n >= 0.0)
    {
        return Err(MuseError::new("local_storage_error"));
    }
    if let Some(last_err) = obj.get("lastError").filter(|v| !v.is_null()) {
        let Some(code) = last_err.as_str() else {
            return Err(MuseError::new("local_storage_error"));
        };
        if normalize_error_code(code) != code {
            return Err(MuseError::new("local_storage_error"));
        }
    }
    let quota = obj.get("quota").unwrap_or(&Value::Null);
    if quota.is_null() {
        if !obj.get("sampledAt").is_none_or(Value::is_null) {
            return Err(MuseError::new("local_storage_error"));
        }
    } else {
        let Some(qobj) = quota.as_object() else {
            return Err(MuseError::new("local_storage_error"));
        };
        if qobj.keys().any(|k| !QUOTA_FIELDS.contains(&k.as_str()))
            || qobj.get("tier") != obj.get("plan")
            || qobj.values().any(|v| {
                v.is_object() || v.is_array() || v.as_str().is_some_and(|s| s.chars().count() > 160)
            })
            || !obj
                .get("sampledAt")
                .is_some_and(|v| v.is_string() && reset_at(v).is_some())
            || normalize_muse(&json!({"subscription_quota": quota})).is_empty()
        {
            return Err(MuseError::new("local_storage_error"));
        }
    }
    let (verified_at, restore_allowed) = read_plan_verification(root, obj, now);
    let plan_verified = !quota.is_null() || verified_at.is_some();
    Ok(Some(UsageState {
        raw: obj.clone(),
        plan_verified,
        plan_verified_at: verified_at,
        restore_allowed,
    }))
}

fn write_usage_state(root: &Path, state: &UsageState, store: &OwnedStore) -> Result<(), MuseError> {
    let mut payload = Map::new();
    for key in USAGE_FIELDS {
        payload.insert(
            key.to_owned(),
            state.raw.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    let bytes = serde_json::to_vec(&Value::Object(payload.clone()))
        .map_err(|_| MuseError::new("local_storage_error"))?;
    store
        .write_private_cache(&bytes)
        .map_err(|_| MuseError::new("local_storage_error"))?;
    let sidecar = json!({
        "schemaVersion": 2,
        "stateHash": usage_state_fingerprint(&payload),
        "verifiedAt": state.plan_verified_at,
        "restoreAllowed": state.restore_allowed,
    });
    let _ = write_atomic_private(root, PLAN_NAME, &sidecar, MAX_BYTES);
    Ok(())
}

fn verified_state_plan(state: Option<&UsageState>) -> Option<String> {
    let s = state?;
    s.plan_verified_at.as_ref()?;
    s.raw.get("plan").and_then(Value::as_str).map(str::to_owned)
}

fn matches_state(
    state: Option<&UsageState>,
    email: &str,
    plan: Option<&str>,
    team: Option<&str>,
    access_fp: &str,
) -> bool {
    let Some(s) = state else {
        return false;
    };
    let same_email = s
        .raw
        .get("email")
        .and_then(Value::as_str)
        .is_some_and(|e| e.eq_ignore_ascii_case(email));
    let same_plan = plan.is_none_or(|p| s.raw.get("plan").and_then(Value::as_str) == Some(p));
    let same_team = team.is_none_or(|t| s.raw.get("teamId").and_then(Value::as_str) == Some(t));
    let same_fp = s.raw.get("credentialFingerprint").and_then(Value::as_str) == Some(access_fp);
    same_email && same_plan && same_team && same_fp
}

fn state_sample(state: &UsageState, cached: bool, now: f64) -> Value {
    let last_error = state.raw.get("lastError").and_then(Value::as_str);
    let retained = cached && last_error.is_some();
    let message = if retained && last_error == Some("rate_limited") {
        Some(
            "Muse is limiting requests; showing the last successful usage reading. Refresh resumes automatically.",
        )
    } else {
        None
    };
    let failure_code = if retained { last_error } else { None };
    let mut out = json!({
        "teamId": state.raw.get("teamId").cloned().unwrap_or(Value::Null),
        "quota": state.raw.get("quota").cloned().unwrap_or(Value::Null),
        "sampledAt": state.raw.get("sampledAt").cloned().unwrap_or(Value::Null),
        "cached": cached,
        "retained": retained,
        "message": message,
        "failureCode": failure_code,
    });
    if let Some(ref verified_at) = state.plan_verified_at {
        out["observedPlan"] = state.raw.get("plan").cloned().unwrap_or(Value::Null);
        out["observedPlanAt"] = json!(verified_at);
    }
    let next_at = state
        .raw
        .get("nextRequestAt")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let remaining = (next_at - now).clamp(0.0, MAX_RETRY_SECONDS);
    if remaining > 0.0 {
        if failure_code == Some("rate_limited") {
            out["retryAfterSeconds"] = json!(remaining);
        } else if failure_code.is_none() {
            out["refreshAfterSeconds"] = json!(remaining);
        }
    }
    out
}

fn user_hash(me: &Value) -> Option<String> {
    let val = me.get("userId")?;
    let text = match val {
        Value::Number(n) => n
            .as_i64()
            .map(|i| i.to_string())
            .or_else(|| n.as_u64().map(|u| u.to_string()))?,
        Value::String(s) => s.clone(),
        _ => return None,
    };
    if text.is_empty()
        || text.chars().count() > 128
        || text
            .chars()
            .any(|c| (c as u32) <= 0x20 || (c as u32) == 0x7f)
    {
        return None;
    }
    Some(sha256_hex(format!("muse-user:{text}").as_bytes()))
}

enum PinnedUser {
    None,
    Invalid,
    Hash(String),
}

fn pinned_user(root: &Path, email: &str, team: &str) -> PinnedUser {
    let Ok(opt) = read_private_json_exact(root, BINDING_NAME) else {
        return PinnedUser::Invalid;
    };
    let Some(val) = opt else {
        return PinnedUser::None;
    };
    let Some(obj) = val.as_object() else {
        return PinnedUser::Invalid;
    };
    if obj.len() != 5
        || obj.get("schemaVersion").and_then(Value::as_i64) != Some(1)
        || !obj
            .get("email")
            .and_then(Value::as_str)
            .is_some_and(valid_email)
        || !obj
            .get("plan")
            .and_then(Value::as_str)
            .is_some_and(valid_plan)
        || !obj
            .get("teamId")
            .and_then(Value::as_str)
            .is_some_and(valid_team)
        || !obj
            .get("userHash")
            .and_then(Value::as_str)
            .is_some_and(valid_hash)
    {
        return PinnedUser::Invalid;
    }
    let same_email = obj
        .get("email")
        .and_then(Value::as_str)
        .is_some_and(|e| e.eq_ignore_ascii_case(email));
    let same_team = obj.get("teamId").and_then(Value::as_str) == Some(team);
    if !same_email || !same_team {
        return PinnedUser::Invalid;
    }
    PinnedUser::Hash(
        obj.get("userHash")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    )
}

fn pin_user(root: &Path, email: &str, plan: &str, team: &str, mode: &str, user_hash: Option<&str>) {
    if !matches!(mode, "email" | "user" | "session") {
        return;
    }
    let Some(hash) = user_hash.filter(|h| valid_hash(h)) else {
        return;
    };
    let _ = write_atomic_private(
        root,
        BINDING_NAME,
        &json!({
            "schemaVersion": 1,
            "email": email,
            "plan": plan,
            "teamId": team,
            "userHash": hash,
        }),
        MAX_BYTES,
    );
}

fn is_capsule_session(
    root: &Path,
    cookies: &[Value],
    email: &str,
    team: &str,
    now_secs: Option<f64>,
) -> bool {
    let Ok((stored, stored_team, stored_email, _)) = read_capsule(root, false, now_secs) else {
        return false;
    };
    let Ok(presented) = validate_cookies(&Value::Array(cookies.to_vec()), now_unix_secs(now_secs))
    else {
        return false;
    };
    stored_team == team
        && stored_email.eq_ignore_ascii_case(email)
        && session_values(&stored) == session_values(&presented)
}

fn provider_as_of(value: Option<&Value>, now: f64) -> Option<f64> {
    let val = value?;
    if val.is_boolean() {
        return None;
    }
    let secs = match val {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => {
            if s.len() > 64 || !AS_OF_STR_RE.is_match(s) {
                return None;
            }
            s.parse::<f64>().ok()?
        }
        _ => return None,
    };
    if !secs.is_finite() || !now.is_finite() {
        return None;
    }
    let upper = MAX_PROVIDER_AS_OF.min(now + AS_OF_FUTURE_SKEW_SECONDS);
    (secs >= MIN_PROVIDER_AS_OF && secs <= upper).then_some(secs)
}

fn provider_sampled_at(quota: &Map<String, Value>, now: f64) -> Option<String> {
    let secs = provider_as_of(quota.get("as_of"), now)?;
    let whole = secs.trunc() as i64;
    let nanos = ((secs - secs.trunc()) * 1e9).round() as u32;
    let dt = DateTime::<Utc>::from_timestamp(whole, nanos.min(999_999_999))?;
    if nanos == 0 {
        Some(dt.to_rfc3339_opts(SecondsFormat::Secs, true))
    } else {
        Some(dt.to_rfc3339_opts(SecondsFormat::Micros, true))
    }
}

fn team_rows(payload: &Value) -> Result<Vec<(String, String)>, MuseError> {
    let Some(rows) = payload.get("teams").and_then(Value::as_array) else {
        return Err(MuseError::new("invalid_response"));
    };
    if rows.is_empty() || rows.len() > 100 {
        return Err(MuseError::new("invalid_response"));
    }
    let mut teams = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let raw = row.get("team_id");
        let id = match raw {
            Some(Value::Number(n)) => n
                .as_i64()
                .map(|i| i.to_string())
                .or_else(|| n.as_u64().map(|u| u.to_string())),
            Some(Value::String(s)) => Some(s.clone()),
            _ => None,
        };
        let Some(id) = id.filter(|s| valid_team(s)) else {
            return Err(MuseError::new("invalid_response"));
        };
        if !seen.insert(id.clone()) {
            return Err(MuseError::new("invalid_response"));
        }
        let name = row
            .get("team_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| !n.is_empty() && n.chars().count() <= 160 && !PLAN_BAD_RE.is_match(n))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Team {id}"));
        teams.push((id, name));
    }
    Ok(teams)
}

type QuotaFetch = (String, Map<String, Value>, &'static str, Option<String>);

#[allow(clippy::too_many_arguments)]
async fn fetch_quota(
    cookies: &[Value],
    expected_email: &str,
    expected_plan: &str,
    mut requested: Option<String>,
    client: &dyn PortalTransport,
    now: f64,
    bound_team: Option<&str>,
    bound_user: PinnedUser,
    capsule_session: bool,
) -> Result<QuotaFetch, MuseError> {
    if !valid_email(expected_email) || !valid_plan(expected_plan) {
        return Err(MuseError::new("account_mismatch"));
    }
    if let Some(bt) = bound_team
        && requested.as_deref().is_some_and(|r| r != bt)
    {
        return Err(MuseError::new("identity_unavailable"));
    }
    let _ = validate_cookies(&Value::Array(cookies.to_vec()), now)?;
    let me = client.get("/api/auth/me").await.map_err(from_portal_err)?;
    let actual = me.get("email").and_then(Value::as_str);
    let uhash = user_hash(&me);
    let mode = if let Some(addr) = actual.filter(|e| valid_email(e)) {
        if !addr.trim().eq_ignore_ascii_case(expected_email.trim()) {
            return Err(MuseError::new("account_mismatch"));
        }
        "email"
    } else if bound_team.is_none_or(|bt| !valid_team(bt)) || uhash.is_none() {
        return Err(MuseError::new("identity_unavailable"));
    } else {
        let bt = bound_team.unwrap_or_default().to_owned();
        match bound_user {
            PinnedUser::None if capsule_session => {
                requested = Some(bt);
                "session"
            }
            PinnedUser::Hash(ref pinned) => {
                if uhash.as_deref() != Some(pinned.as_str()) {
                    return Err(MuseError::new("account_mismatch"));
                }
                requested = Some(bt);
                "user"
            }
            _ => return Err(MuseError::new("identity_unavailable")),
        }
    };
    let listed = client
        .get("/api/portal/teams")
        .await
        .map_err(from_portal_err)?;
    if requested.is_some()
        && listed
            .get("teams")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        return Err(MuseError::new("team_mismatch"));
    }
    let teams = team_rows(&listed)?;
    if let Some(ref req) = requested
        && !teams.iter().any(|(id, _)| id == req)
    {
        return Err(MuseError::new("team_mismatch"));
    }
    if mode == "session" && teams.len() != 1 {
        return Err(MuseError::new("identity_unavailable"));
    }
    let selected = match requested {
        Some(req) => req,
        None => {
            if teams.len() != 1 {
                return Err(MuseError::new("choose_team"));
            }
            teams[0].0.clone()
        }
    };
    let route = format!("/api/portal/teams/{selected}/subscription-quota");
    let response = client.get(&route).await.map_err(|e| {
        let mut err = from_portal_err(e);
        err.team_id = Some(selected.clone());
        err
    })?;
    let Some(quota) = response
        .get("subscription_quota")
        .and_then(Value::as_object)
    else {
        return Err(MuseError::new("invalid_response"));
    };
    if quota.get("tier").and_then(Value::as_str) != Some(expected_plan) {
        return Err(MuseError::new("plan_mismatch"));
    }
    let mut safe = Map::new();
    for key in QUOTA_FIELDS {
        if key == "as_of" {
            continue;
        }
        if let Some(v) = quota.get(key) {
            if v.is_object() || v.is_array() || v.as_str().is_some_and(|s| s.chars().count() > 160)
            {
                return Err(MuseError::new("invalid_response"));
            }
            safe.insert(key.to_owned(), v.clone());
        }
    }
    if let Some(as_of) = provider_as_of(quota.get("as_of"), now) {
        safe.insert("as_of".into(), json!(as_of));
    }
    Ok((selected, safe, mode, uhash))
}

fn from_portal_err(err: PortalError) -> MuseError {
    MuseError::with_retry(err.code, err.retry_after)
}

/// Exact port of `muse_console.quota_sample`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn quota_sample<F, Fut>(
    root: &Path,
    cookies: &[Value],
    expected_email: &str,
    mut expected_plan: String,
    requested: Option<&str>,
    access: &str,
    portal: &dyn PortalTransport,
    now_secs: Option<f64>,
    bound_team: Option<&str>,
    identity_loader: Option<F>,
) -> Result<Value, MuseError>
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<String, MuseError>>,
{
    let live_clock = now_secs.is_none();
    let now = now_unix_secs(now_secs);
    let has_loader = identity_loader.is_some();
    let mut verified_at = (!has_loader).then(|| plan_timestamp(now));
    if !valid_email(expected_email) || !valid_plan(&expected_plan) {
        return Err(MuseError::new("account_mismatch"));
    }
    if let Some(bt) = bound_team
        && requested.is_some_and(|r| r != bt)
    {
        return Err(MuseError::new("identity_unavailable"));
    }
    let _ = validate_cookies(&Value::Array(cookies.to_vec()), now)?;
    check_private_root(root)?;
    let store = claim_store(root, CACHE_NAME, MAX_BYTES)?;
    let access_fp = fingerprint(access)?;
    let mut state = read_usage_state(root, now)?;

    if let Some(ref mut st) = state {
        let team_differs =
            requested.is_some_and(|r| st.raw.get("teamId").and_then(Value::as_str) != Some(r));
        let plan_differs = !has_loader
            && st.raw.get("plan").and_then(Value::as_str) != Some(expected_plan.as_str());
        let next_at = st
            .raw
            .get("nextRequestAt")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if (team_differs || plan_differs)
            && matches_state(Some(st), expected_email, None, None, &access_fp)
            && st.raw.get("lastError").and_then(Value::as_str) == Some("rate_limited")
            && next_at > now
        {
            if plan_differs {
                st.raw.insert("plan".into(), json!(expected_plan));
                st.raw.insert("quota".into(), Value::Null);
                st.raw.insert("sampledAt".into(), Value::Null);
                st.plan_verified = true;
                st.plan_verified_at = Some(plan_timestamp(now));
                st.restore_allowed = true;
                write_usage_state(root, st, &store)?;
            }
            return Err(MuseError::with_plan(
                "rate_limited",
                Some(next_at - now),
                verified_state_plan(Some(st)),
                st.plan_verified_at.clone(),
            ));
        }
    }

    let cache_plan = (!has_loader).then_some(expected_plan.as_str());
    if !matches_state(
        state.as_ref(),
        expected_email,
        cache_plan,
        requested,
        &access_fp,
    ) {
        state = None;
    }
    if let Some(ref st) = state {
        let next_at = st
            .raw
            .get("nextRequestAt")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if next_at > now {
            if !st.raw.get("quota").is_none_or(Value::is_null) {
                return Ok(state_sample(st, true, now));
            }
            let code = normalize_error_code(
                st.raw
                    .get("lastError")
                    .and_then(Value::as_str)
                    .unwrap_or("provider_error"),
            );
            return Err(MuseError::with_plan(
                code,
                (code == "rate_limited").then_some(next_at - now),
                verified_state_plan(Some(st)),
                st.plan_verified_at.clone(),
            ));
        }
    }

    if state.is_none()
        && let Some(req) = requested
    {
        if !valid_team(req) {
            return Err(MuseError::new("team_mismatch"));
        }
        let mut raw = Map::new();
        raw.insert("schemaVersion".into(), json!(1));
        raw.insert("email".into(), json!(expected_email));
        raw.insert("plan".into(), json!(expected_plan));
        raw.insert("teamId".into(), json!(req));
        raw.insert("credentialFingerprint".into(), json!(access_fp));
        raw.insert("quota".into(), Value::Null);
        raw.insert("sampledAt".into(), Value::Null);
        raw.insert("nextRequestAt".into(), json!(now));
        raw.insert("lastError".into(), json!("network_error"));
        state = Some(UsageState {
            raw,
            plan_verified: !has_loader,
            plan_verified_at: verified_at.clone(),
            restore_allowed: false,
        });
    }
    if let Some(ref mut st) = state {
        st.raw
            .insert("nextRequestAt".into(), json!(now + REFRESH_SECONDS));
        st.restore_allowed = false;
        st.raw.insert("lastError".into(), json!("network_error"));
        write_usage_state(root, st, &store)?;
    }

    let mut verified_plan = !has_loader;
    let attempt: Result<QuotaFetch, MuseError> = async {
        if let Some(loader) = identity_loader {
            let loaded_plan = loader().await?;
            if !valid_plan(&loaded_plan) {
                return Err(MuseError::new("invalid_response"));
            }
            expected_plan = loaded_plan;
            verified_plan = true;
            verified_at = Some(plan_timestamp(if live_clock {
                now_unix_secs(None)
            } else {
                now
            }));
            if let Some(ref mut st) = state {
                if st.raw.get("plan").and_then(Value::as_str) != Some(expected_plan.as_str()) {
                    st.raw.insert("quota".into(), Value::Null);
                    st.raw.insert("sampledAt".into(), Value::Null);
                }
                st.raw.insert("plan".into(), json!(expected_plan));
                st.plan_verified = true;
                st.plan_verified_at = verified_at.clone();
                write_usage_state(root, st, &store)?;
            }
        } else if let Some(ref mut st) = state {
            st.plan_verified = true;
            st.plan_verified_at = verified_at.clone();
            write_usage_state(root, st, &store)?;
        }
        let (bu, cs) = if let Some(bt) = bound_team {
            (
                pinned_user(root, expected_email, bt),
                is_capsule_session(root, cookies, expected_email, bt, now_secs),
            )
        } else {
            (PinnedUser::None, false)
        };
        let (team, quota, mode, uhash) = fetch_quota(
            cookies,
            expected_email,
            &expected_plan,
            requested.map(str::to_owned),
            portal,
            now,
            bound_team,
            bu,
            cs,
        )
        .await?;
        if normalize_muse(&json!({ "subscription_quota": quota })).is_empty() {
            return Err(MuseError::new("invalid_response"));
        }
        Ok((team, quota, mode, uhash))
    }
    .await;

    match attempt {
        Ok((team, quota, mode, uhash)) => {
            let sampled_at =
                provider_sampled_at(&quota, now).unwrap_or_else(|| utc_now_iso_secs(now_secs));
            let mut raw = Map::new();
            raw.insert("schemaVersion".into(), json!(1));
            raw.insert("email".into(), json!(expected_email));
            raw.insert("plan".into(), json!(expected_plan));
            raw.insert("teamId".into(), json!(team));
            raw.insert("credentialFingerprint".into(), json!(access_fp));
            raw.insert("quota".into(), Value::Object(quota));
            raw.insert("sampledAt".into(), json!(sampled_at));
            raw.insert("nextRequestAt".into(), json!(now + REFRESH_SECONDS));
            raw.insert("lastError".into(), Value::Null);
            let next_state = UsageState {
                raw,
                plan_verified: true,
                plan_verified_at: verified_at,
                restore_allowed: true,
            };
            write_usage_state(root, &next_state, &store)?;
            pin_user(
                root,
                expected_email,
                &expected_plan,
                &team,
                mode,
                uhash.as_deref(),
            );
            let mut out = state_sample(&next_state, false, now);
            out["binding"] = json!(mode);
            Ok(out)
        }
        Err(mut error) => {
            let resolved_team = requested
                .map(str::to_owned)
                .or_else(|| error.team_id.clone());
            if resolved_team.is_none() && state.is_none() {
                return Err(error);
            }
            let st = state.get_or_insert_with(|| {
                let mut raw = Map::new();
                raw.insert("schemaVersion".into(), json!(1));
                raw.insert("email".into(), json!(expected_email));
                raw.insert("plan".into(), json!(expected_plan));
                raw.insert("teamId".into(), json!(resolved_team.unwrap_or_default()));
                raw.insert("credentialFingerprint".into(), json!(access_fp));
                raw.insert("quota".into(), Value::Null);
                raw.insert("sampledAt".into(), Value::Null);
                raw.insert("nextRequestAt".into(), json!(now));
                raw.insert("lastError".into(), Value::Null);
                UsageState {
                    raw,
                    plan_verified: verified_plan,
                    plan_verified_at: if verified_plan { verified_at } else { None },
                    restore_allowed: false,
                }
            });
            let transient = matches!(
                error.code,
                "rate_limited" | "provider_error" | "network_error"
            );
            if !transient {
                st.raw.insert("quota".into(), Value::Null);
                st.raw.insert("sampledAt".into(), Value::Null);
            }
            st.restore_allowed = verified_plan && error.code == "rate_limited";
            st.raw.insert("lastError".into(), json!(error.code));
            let wait = if error.code == "rate_limited" {
                error
                    .retry_after
                    .unwrap_or(RATE_LIMIT_SECONDS)
                    .clamp(REFRESH_SECONDS, MAX_RETRY_SECONDS)
            } else {
                REFRESH_SECONDS
            };
            st.raw.insert("nextRequestAt".into(), json!(now + wait));
            write_usage_state(root, st, &store)?;
            if transient && !st.raw.get("quota").is_none_or(Value::is_null) {
                return Ok(state_sample(st, true, now));
            }
            error.plan = verified_state_plan(Some(st));
            error.plan_at = st.plan_verified_at.clone();
            Err(error)
        }
    }
}

/// Default `reqwest` implementation of [`PortalTransport`] (`muse_console.PortalClient`).
pub(crate) struct ReqwestPortalTransport {
    client: reqwest::Client,
    cookie_header: String,
    deadline: Instant,
    now_override: Option<f64>,
}

impl ReqwestPortalTransport {
    pub(crate) fn new(cookies: &[Value], now_override: Option<f64>) -> Result<Self, MuseError> {
        let valid = validate_cookies(&Value::Array(cookies.to_vec()), now_unix_secs(now_override))?;
        let header = valid
            .iter()
            .filter_map(|row| {
                Some(format!(
                    "{}={}",
                    row.get("name")?.as_str()?,
                    row.get("value")?.as_str()?
                ))
            })
            .collect::<Vec<_>>()
            .join("; ");
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| MuseError::new("network_error"))?;
        Ok(Self {
            client,
            cookie_header: header,
            deadline: Instant::now() + Duration::from_secs(16),
            now_override,
        })
    }
}

fn portal_retry_seconds(value: Option<&str>, now_secs: f64) -> f64 {
    let mut seconds: Option<f64> = None;
    if let Some(raw) = value.filter(|s| s.len() <= 128) {
        let trimmed = raw.trim();
        if !trimmed.is_empty() && trimmed.len() <= 12 && trimmed.bytes().all(|b| b.is_ascii_digit())
        {
            seconds = trimmed.parse::<f64>().ok();
        } else if let Ok(parsed) = DateTime::parse_from_rfc2822(trimmed) {
            seconds = Some(parsed.timestamp() as f64 - now_secs);
        }
    }
    match seconds.filter(|s| s.is_finite()) {
        Some(s) => s.clamp(REFRESH_SECONDS, MAX_RETRY_SECONDS),
        None => RATE_LIMIT_SECONDS,
    }
}

impl PortalTransport for ReqwestPortalTransport {
    fn get<'a>(
        &'a self,
        route: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, PortalError>> + Send + 'a>> {
        Box::pin(async move {
            if !matches!(route, "/api/auth/me" | "/api/portal/teams")
                && !PORTAL_QUOTA_ROUTE_RE.is_match(route)
            {
                return Err(PortalError {
                    code: "invalid_request",
                    retry_after: None,
                });
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(PortalError {
                    code: "network_error",
                    retry_after: None,
                });
            }
            let url = format!("{ORIGIN}{route}");
            let mut resp = self
                .client
                .get(&url)
                .timeout(remaining.min(Duration::from_secs(8)))
                .header("Cookie", &self.cookie_header)
                .header("Accept", "application/json")
                .header("User-Agent", "CCS-Muse-Usage")
                .header("Referer", format!("{ORIGIN}/"))
                .send()
                .await
                .map_err(|_| PortalError {
                    code: "network_error",
                    retry_after: None,
                })?;
            let status = resp.status().as_u16();
            if status == 429 {
                let hdr = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok());
                return Err(PortalError {
                    code: "rate_limited",
                    retry_after: Some(portal_retry_seconds(hdr, now_unix_secs(self.now_override))),
                });
            }
            if matches!(status, 300..=399 | 401 | 403) {
                return Err(PortalError {
                    code: "needs_sign_in",
                    retry_after: None,
                });
            }
            if status != 200 {
                return Err(PortalError {
                    code: "provider_error",
                    retry_after: None,
                });
            }
            let mut raw = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| PortalError {
                code: "network_error",
                retry_after: None,
            })? {
                if Instant::now() > self.deadline {
                    return Err(PortalError {
                        code: "invalid_response",
                        retry_after: None,
                    });
                }
                let take = chunk.len().min(MAX_BYTES + 1 - raw.len());
                raw.extend_from_slice(&chunk[..take]);
                if raw.len() > MAX_BYTES {
                    return Err(PortalError {
                        code: "invalid_response",
                        retry_after: None,
                    });
                }
            }
            let payload: Value = serde_json::from_slice(&raw).map_err(|_| PortalError {
                code: "network_error",
                retry_after: None,
            })?;
            if !payload.is_object() {
                return Err(PortalError {
                    code: "invalid_response",
                    retry_after: None,
                });
            }
            Ok(payload)
        })
    }
}

// =========================================================================
// muse_native.py exact port
// =========================================================================

fn ensure_native_root(root: &Path) -> Result<(), MuseError> {
    let abs = aac_files::path::python::normpath(root);
    let mut ancestors: Vec<&Path> = abs.ancestors().collect();
    ancestors.reverse();
    for parent in ancestors {
        match fs::symlink_metadata(parent) {
            Ok(meta) => {
                if meta.file_type().is_symlink() || !meta.is_dir() {
                    return Err(MuseError::new("local_storage_error"));
                }
            }
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err(MuseError::new("local_storage_error")),
        }
    }
    fs::create_dir_all(&abs).map_err(|_| MuseError::new("local_storage_error"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let meta = fs::symlink_metadata(&abs).map_err(|_| MuseError::new("local_storage_error"))?;
        if meta.uid() != aac_files::path::uid() {
            return Err(MuseError::new("local_storage_error"));
        }
        if meta.mode() & 0o077 != 0 {
            let _ = fs::set_permissions(&abs, fs::Permissions::from_mode(0o700));
            let recheck =
                fs::symlink_metadata(&abs).map_err(|_| MuseError::new("local_storage_error"))?;
            if recheck.mode() & 0o077 != 0 {
                return Err(MuseError::new("local_storage_error"));
            }
        }
    }
    Ok(())
}

fn sanitize_muse_sample(value: &Value, platform: &str) -> Option<Value> {
    let obj = value.as_object()?;
    let status = obj.get("status")?.as_str()?;
    if !matches!(status, "ok" | "cached") {
        return None;
    }
    if obj.get("provider").and_then(Value::as_str) != Some("muse")
        || obj.get("platform").and_then(Value::as_str) != Some(platform)
    {
        return None;
    }
    let sampled = obj
        .get("sampledAt")
        .and_then(Value::as_str)
        .and_then(parse_iso_utc)?;
    let fetched = obj
        .get("fetchedAt")
        .and_then(Value::as_str)
        .and_then(parse_iso_utc)?;
    if sampled > fetched {
        return None;
    }
    let windows = obj.get("windows")?.as_array()?;
    if windows.is_empty() || windows.len() > 256 {
        return None;
    }
    let email = match obj.get("email") {
        None | Some(Value::Null) => None,
        Some(v) => Some(crate::failover_additional::desktop::email(v)?),
    };
    let plan = obj.get("plan").and_then(|v| safe_text(v, 160));
    let host = match platform {
        "mac" => "Mac",
        "windows" => "Windows",
        _ => "Ubuntu",
    };
    Some(json!({
        "id": format!("native:muse:{platform}"),
        "provider": "muse",
        "providerLabel": "Muse Code",
        "label": "Muse Code",
        "email": email,
        "plan": plan,
        "platform": platform,
        "source": format!("Muse Code usage on {host}"),
        "status": status,
        "message": null,
        "sampledAt": sampled.to_rfc3339_opts(SecondsFormat::Millis, true),
        "fetchedAt": fetched.to_rfc3339_opts(SecondsFormat::Millis, true),
        "isActive": false,
        "windows": windows,
        "capabilities": {
            "codexProfile": null,
            "claudeProfileId": null,
            "claudePlatforms": []
        }
    }))
}

fn read_native_state(
    root: &Path,
    key: &str,
    platform: &str,
    now: f64,
) -> Result<Option<Map<String, Value>>, MuseError> {
    let name = format!("muse-native-{key}.json");
    let path = root.join(&name);
    let abs = aac_files::path::python::normpath(root);
    let policy = ReadPolicy::capped(NATIVE_MAX_BYTES as u64)
        .ancestors(Ancestors::NoLinkBelow(
            abs.ancestors().last().unwrap_or(&abs),
        ))
        .owner(Owner::Mine)
        .mode(Mode::Private);
    let (raw, meta) = match aac_files::read::read(&path, &policy) {
        Read::Absent => return Ok(None),
        Read::Bytes(b, m) => (b, m),
        _ => return Err(MuseError::new("local_storage_error")),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.nlink() != 1 {
            return Err(MuseError::new("local_storage_error"));
        }
    }
    #[cfg(not(unix))]
    let _ = meta;
    let value: Value =
        serde_json::from_slice(&raw).map_err(|_| MuseError::new("local_storage_error"))?;
    let Some(mut obj) = value.as_object().cloned() else {
        return Err(MuseError::new("local_storage_error"));
    };
    let Some(version) = obj.get("version").and_then(Value::as_i64) else {
        return Err(MuseError::new("local_storage_error"));
    };
    if !matches!(version, 1 | 2) {
        return Err(MuseError::new("local_storage_error"));
    }
    let base_fields = [
        "version",
        "key",
        "sample",
        "failureCode",
        "nextRequestAt",
        "observedPlan",
    ];
    let expected_len = if version == 1 { 6 } else { 7 };
    if obj.len() != expected_len
        || !base_fields.iter().all(|k| obj.contains_key(*k))
        || (version == 2 && !obj.contains_key("observedPlanAt"))
        || obj.get("key").and_then(Value::as_str) != Some(key)
    {
        return Err(MuseError::new("local_storage_error"));
    }
    if let Some(fc) = obj.get("failureCode").filter(|v| !v.is_null()) {
        let Some(code) = fc.as_str() else {
            return Err(MuseError::new("local_storage_error"));
        };
        if normalize_error_code(code) != code {
            return Err(MuseError::new("local_storage_error"));
        }
    }
    let Some(next_at) = obj.get("nextRequestAt").and_then(Value::as_f64) else {
        return Err(MuseError::new("local_storage_error"));
    };
    if !next_at.is_finite() || next_at < 0.0 || next_at > now + MAX_RETRY_SECONDS {
        return Err(MuseError::new("local_storage_error"));
    }
    if let Some(op) = obj.get("observedPlan").filter(|v| !v.is_null())
        && safe_text(op, 160).as_deref() != op.as_str()
    {
        return Err(MuseError::new("local_storage_error"));
    }
    if version == 1 {
        obj.insert("version".into(), json!(2));
        obj.insert("observedPlanAt".into(), Value::Null);
    } else if let Some(at) = obj.get("observedPlanAt").filter(|v| !v.is_null())
        && (obj.get("observedPlan").is_none_or(Value::is_null)
            || !valid_plan_timestamp(at.as_str(), Some(now)))
    {
        return Err(MuseError::new("local_storage_error"));
    }
    let raw_sample = obj.get("sample").cloned().unwrap_or(Value::Null);
    let sanitized = if raw_sample.is_null() {
        None
    } else {
        let Some(s) = sanitize_muse_sample(&raw_sample, platform) else {
            return Err(MuseError::new("local_storage_error"));
        };
        Some(s)
    };
    obj.insert("sample".into(), sanitized.unwrap_or(Value::Null));
    Ok(Some(obj))
}

fn native_result(state: &Map<String, Value>, now: f64, cached: bool) -> Value {
    let mut sample = state.get("sample").cloned().unwrap_or(Value::Null);
    if cached && sample.is_object() {
        sample["status"] = json!("cached");
    }
    let failure_code = state.get("failureCode").cloned().unwrap_or(Value::Null);
    let mut out = json!({
        "sample": sample,
        "failureCode": failure_code,
    });
    if let (Some(op), Some(at)) = (
        state.get("observedPlan").filter(|v| !v.is_null()),
        state.get("observedPlanAt").filter(|v| !v.is_null()),
    ) {
        out["observedPlan"] = op.clone();
        out["observedPlanAt"] = at.clone();
    }
    let next_at = state
        .get("nextRequestAt")
        .and_then(Value::as_f64)
        .unwrap_or(0.0);
    let remaining = (next_at - now).clamp(0.0, MAX_RETRY_SECONDS);
    if remaining > 0.0 {
        if state.get("failureCode").and_then(Value::as_str) == Some("rate_limited") {
            out["retryAfterSeconds"] = json!(remaining);
        } else if state.get("failureCode").is_none_or(Value::is_null) {
            out["refreshAfterSeconds"] = json!(remaining);
        }
    }
    out
}

/// Exact port of `muse_native.collect`.
pub(crate) async fn native_collect<F, Fut>(
    root: &Path,
    access: &str,
    expected_email: Option<&str>,
    platform: &str,
    now_secs: Option<f64>,
    loader: F,
) -> Value
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = Value>,
{
    let live_clock = now_secs.is_none();
    let now = now_unix_secs(now_secs);
    let key = sha256_hex(format!("muse-native-v1:{access}").as_bytes());
    let run: Result<Value, MuseError> = async {
        ensure_native_root(root)?;
        let file_name = format!("muse-native-{key}.json");
        let store = claim_store(root, &file_name, NATIVE_MAX_BYTES)?;
        let write_state = |st: &Map<String, Value>| -> Result<(), MuseError> {
            let bytes = serde_json::to_vec(&Value::Object(st.clone()))
                .map_err(|_| MuseError::new("local_storage_error"))?;
            if bytes.len() > NATIVE_MAX_BYTES {
                return Err(MuseError::new("local_storage_error"));
            }
            store
                .write_private_cache(&bytes)
                .map_err(|_| MuseError::new("local_storage_error"))
        };
        let state = read_native_state(root, &key, platform, now)?;
        if let Some(mut st) = state.clone()
            && let Some(sample_email) = st
                .get("sample")
                .and_then(|s| s.get("email"))
                .and_then(Value::as_str)
            && let Some(exp) = expected_email
            && !sample_email.eq_ignore_ascii_case(exp)
        {
            st.insert("sample".into(), Value::Null);
            st.insert("failureCode".into(), json!("account_mismatch"));
            write_state(&st)?;
            return Ok(native_result(&st, now, true));
        }
        if let Some(ref st) = state
            && st
                .get("nextRequestAt")
                .and_then(Value::as_f64)
                .is_some_and(|t| t > now)
        {
            let has_err = st.get("failureCode").is_some_and(|v| !v.is_null());
            return Ok(native_result(st, now, has_err));
        }
        let mut previous = state
            .as_ref()
            .and_then(|s| s.get("sample"))
            .filter(|v| !v.is_null())
            .cloned();
        let previous_plan = state
            .as_ref()
            .and_then(|s| s.get("observedPlan"))
            .cloned()
            .unwrap_or(Value::Null);
        let previous_plan_at = state
            .as_ref()
            .and_then(|s| s.get("observedPlanAt"))
            .cloned()
            .unwrap_or(Value::Null);

        let mut reserved = Map::new();
        reserved.insert("version".into(), json!(2));
        reserved.insert("key".into(), json!(key));
        reserved.insert("sample".into(), previous.clone().unwrap_or(Value::Null));
        reserved.insert("failureCode".into(), json!("network_error"));
        reserved.insert("nextRequestAt".into(), json!(now + REFRESH_SECONDS));
        reserved.insert("observedPlan".into(), previous_plan);
        reserved.insert("observedPlanAt".into(), previous_plan_at);
        write_state(&reserved)?;

        let response = loader().await;
        let mut code = response
            .get("failureCode")
            .and_then(Value::as_str)
            .map(normalize_error_code);
        let sample = response
            .get("sample")
            .filter(|v| !v.is_null())
            .and_then(|s| sanitize_muse_sample(s, platform));
        if sample.is_none() && code.is_none() {
            code = Some("invalid_response");
        }
        let verified_plan = response
            .get("_verifiedPlan")
            .and_then(|v| safe_text(v, 160));
        if let Some(vp) = verified_plan {
            let ts = plan_timestamp(if live_clock { now_unix_secs(None) } else { now });
            reserved.insert("observedPlan".into(), json!(vp));
            reserved.insert("observedPlanAt".into(), json!(ts));
            if previous
                .as_ref()
                .and_then(|p| p.get("plan"))
                .and_then(Value::as_str)
                .is_some_and(|old| old != vp)
            {
                previous = None;
            }
        } else if let Some(ref s) = sample {
            let plan = s.get("plan").cloned().unwrap_or(Value::Null);
            let ts = if !plan.is_null() {
                json!(plan_timestamp(if live_clock {
                    now_unix_secs(None)
                } else {
                    now
                }))
            } else {
                Value::Null
            };
            reserved.insert("observedPlan".into(), plan);
            reserved.insert("observedPlanAt".into(), ts);
        }
        let transient =
            code.is_some_and(|c| matches!(c, "rate_limited" | "network_error" | "provider_error"));
        let stored_sample = if let Some(s) = sample.clone() {
            s
        } else if transient {
            previous.unwrap_or(Value::Null)
        } else {
            Value::Null
        };
        reserved.insert("sample".into(), stored_sample);
        reserved.insert("failureCode".into(), json!(code));
        if code == Some("rate_limited") {
            let retry = response
                .get("retryAfterSeconds")
                .and_then(Value::as_f64)
                .filter(|r| r.is_finite())
                .unwrap_or(RATE_LIMIT_SECONDS);
            reserved.insert(
                "nextRequestAt".into(),
                json!(now + retry.clamp(REFRESH_SECONDS, MAX_RETRY_SECONDS)),
            );
        }
        write_state(&reserved)?;
        Ok(native_result(&reserved, now, sample.is_none()))
    }
    .await;

    match run {
        Ok(v) => v,
        Err(err) => json!({ "sample": null, "failureCode": err.code }),
    }
}
