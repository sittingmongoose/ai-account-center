//! OpenCode console GET-only workspace wallet reader, ported from
//! `browser-bridge/opencode-muse/native-host/opencode_console.py`.
use crate::{
    failover_additional::desktop::{now_unix_secs, parse_iso_utc, utc_now_iso_secs},
    failover_special::{WalletError, WalletTransport},
};
use aac_core::sha256_hex;
use chrono::{DateTime, Datelike, SecondsFormat};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeSet,
    future::Future,
    pin::Pin,
    sync::LazyLock,
    time::{Duration, Instant},
};

pub(crate) const ORIGIN: &str = "https://opencode.ai";
pub(crate) const MAX_BYTES: usize = 1024 * 1024;
const MAX_COOKIES: usize = 20;
pub(crate) const CAPSULE_NAME: &str = "opencode-console-session.json";
const GO_RATE_LIMIT_MESSAGE: &str =
    "Wallet balance is current. OpenCode Go usage is rate limited; try again later.";

const COOKIE_NAMES: [&str; 2] = ["auth", "__Host-console_session"];
const ALLOWED_COOKIE_KEYS: [&str; 7] = [
    "name",
    "value",
    "domain",
    "path",
    "secure",
    "hostOnly",
    "expirationDate",
];
const PUBLIC_SAMPLE_KEYS: [&str; 15] = [
    "id",
    "provider",
    "providerLabel",
    "label",
    "email",
    "plan",
    "platform",
    "source",
    "status",
    "message",
    "fetchedAt",
    "sampledAt",
    "isActive",
    "windows",
    "capabilities",
];

static WORKSPACE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?:wrk_|org_)[A-Za-z0-9_-]{1,128}\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static SIGNED_INT20_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A-?[0-9]{1,20}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static UNSIGNED_INT20_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9]{1,20}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});

impl WalletError {
    pub(crate) fn new(code: &'static str) -> Self {
        Self {
            code,
            retry_after_seconds: None,
        }
    }

    pub(crate) fn with_retry(code: &'static str, retry_after_seconds: Option<f64>) -> Self {
        Self {
            code,
            retry_after_seconds,
        }
    }
}

/// Parse `Retry-After` using `opencode_console.retry_after` (`0..=86400`, ceiling, `None` on bad input).
pub(crate) fn retry_after(value: Option<&str>, now_secs: f64) -> Option<f64> {
    let raw = value?;
    if raw.len() > 128 {
        return None;
    }
    let seconds = match raw.trim().parse::<f64>() {
        Ok(n) if n.is_finite() => n,
        _ => {
            let parsed = DateTime::parse_from_rfc2822(raw.trim()).ok()?;
            parsed.timestamp() as f64 - now_secs
        }
    };
    if !seconds.is_finite() {
        return None;
    }
    Some(seconds.ceil().clamp(0.0, 86_400.0))
}

/// Normalize ISO-8601 UTC timestamp with millisecond precision (`opencode_console.iso`).
pub(crate) fn iso(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?;
    if raw.len() > 64 {
        return None;
    }
    let dt = parse_iso_utc(raw)?;
    if !(2000..=2200).contains(&dt.year()) {
        return None;
    }
    Some(dt.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn finite_number(value: &Value) -> Option<f64> {
    let n = value.as_f64()?;
    n.is_finite().then_some(n)
}

fn has_invalid_cookie_char(value: &str) -> bool {
    value
        .chars()
        .any(|c| c == ';' || (c as u32) <= 0x20 || (c as u32) == 0x7f)
}

/// Validate OpenCode console cookies (`opencode_console.validate_cookies`).
pub(crate) fn validate_cookies(values: &Value, now_secs: f64) -> Result<Vec<Value>, WalletError> {
    let Some(arr) = values.as_array() else {
        return Err(WalletError::new("no_browser_cookie"));
    };
    if arr.is_empty() || arr.len() > MAX_COOKIES {
        return Err(WalletError::new("no_browser_cookie"));
    }
    let mut result = Vec::new();
    let mut seen = BTreeSet::new();
    for row in arr {
        let Some(obj) = row.as_object() else {
            return Err(WalletError::new("invalid_request"));
        };
        if obj
            .keys()
            .any(|k| !ALLOWED_COOKIE_KEYS.contains(&k.as_str()))
        {
            return Err(WalletError::new("invalid_request"));
        }
        let Some(name) = obj.get("name").and_then(Value::as_str) else {
            return Err(WalletError::new("invalid_request"));
        };
        let Some(value) = obj.get("value").and_then(Value::as_str) else {
            return Err(WalletError::new("invalid_request"));
        };
        let Some(domain) = obj.get("domain").and_then(Value::as_str) else {
            return Err(WalletError::new("invalid_request"));
        };
        let Some(path) = obj.get("path").and_then(Value::as_str) else {
            return Err(WalletError::new("invalid_request"));
        };
        let Some(secure) = obj.get("secure").and_then(Value::as_bool) else {
            return Err(WalletError::new("invalid_request"));
        };
        let Some(host_only) = obj.get("hostOnly").and_then(Value::as_bool) else {
            return Err(WalletError::new("invalid_request"));
        };
        if !COOKIE_NAMES.contains(&name)
            || !matches!(domain, "opencode.ai" | ".opencode.ai")
            || path != "/"
            || !secure
            || value.is_empty()
            || value.chars().count() > 8192
            || has_invalid_cookie_char(value)
        {
            return Err(WalletError::new("invalid_request"));
        }
        if name == "__Host-console_session" && (domain != "opencode.ai" || !host_only) {
            return Err(WalletError::new("invalid_request"));
        }
        let expiration = match obj.get("expirationDate") {
            None | Some(Value::Null) => None,
            Some(v) => {
                let Some(exp) = finite_number(v) else {
                    continue;
                };
                if exp <= now_secs {
                    continue;
                }
                Some(v.clone())
            }
        };
        if !seen.insert(name.to_owned()) {
            return Err(WalletError::new("invalid_request"));
        }
        let mut safe = Map::new();
        safe.insert("name".into(), json!(name));
        safe.insert("value".into(), json!(value));
        safe.insert("domain".into(), json!(domain));
        safe.insert("path".into(), json!(path));
        safe.insert("secure".into(), json!(secure));
        safe.insert("hostOnly".into(), json!(host_only));
        if let Some(exp) = expiration {
            safe.insert("expirationDate".into(), exp);
        }
        result.push(Value::Object(safe));
    }
    if result.is_empty() {
        return Err(WalletError::new("no_browser_cookie"));
    }
    Ok(result)
}

pub(crate) fn cookie_header(cookies: &[Value]) -> String {
    cookies
        .iter()
        .filter_map(|row| {
            let name = row.get("name")?.as_str()?;
            let value = row.get("value")?.as_str()?;
            Some(format!("{name}={value}"))
        })
        .collect::<Vec<_>>()
        .join("; ")
}

/// Validate capsule bytes already read via `failover_special::strict_read` (never touches disk).
pub(crate) fn parse_capsule_bytes(
    raw: &[u8],
    now_secs: f64,
) -> Result<(Vec<Value>, String), WalletError> {
    if raw.len() > MAX_BYTES {
        return Err(WalletError::new("local_storage_error"));
    }
    let payload: Value =
        serde_json::from_slice(raw).map_err(|_| WalletError::new("local_storage_error"))?;
    let Some(obj) = payload.as_object() else {
        return Err(WalletError::new("local_storage_error"));
    };
    if obj.get("schemaVersion").and_then(Value::as_i64) != Some(1)
        || obj.get("origin").and_then(Value::as_str) != Some(ORIGIN)
        || obj.get("source").and_then(Value::as_str) != Some("browser-native-cookie-api")
    {
        return Err(WalletError::new("local_storage_error"));
    }
    let captured_iso =
        iso(obj.get("capturedAt")).ok_or_else(|| WalletError::new("local_storage_error"))?;
    let captured_dt =
        parse_iso_utc(&captured_iso).ok_or_else(|| WalletError::new("local_storage_error"))?;
    let captured_secs =
        captured_dt.timestamp() as f64 + (captured_dt.timestamp_subsec_nanos() as f64 / 1e9);
    let age = now_secs - captured_secs;
    if !(-60.0..=24.0 * 3600.0).contains(&age) {
        return Err(WalletError::new("needs_sign_in"));
    }
    let cookies = validate_cookies(obj.get("cookies").unwrap_or(&Value::Null), now_secs)?;
    let requested = obj
        .get("workspaceId")
        .and_then(Value::as_str)
        .filter(|s| WORKSPACE_RE.is_match(s))
        .ok_or_else(|| WalletError::new("local_storage_error"))?;
    Ok((cookies, requested.to_owned()))
}

pub(crate) fn select_workspace(
    payload: &Value,
    requested: Option<&str>,
) -> Result<String, WalletError> {
    let Some(arr) = payload.as_array() else {
        return Err(WalletError::new("invalid_response"));
    };
    if arr.is_empty() || arr.len() > 100 {
        return Err(WalletError::new("invalid_response"));
    }
    let mut ids = Vec::with_capacity(arr.len());
    for row in arr {
        let Some(id) = row
            .as_object()
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)
            .filter(|s| WORKSPACE_RE.is_match(s))
        else {
            return Err(WalletError::new("invalid_response"));
        };
        if ids.iter().any(|existing: &String| existing == id) {
            return Err(WalletError::new("invalid_response"));
        }
        ids.push(id.to_owned());
    }
    if let Some(req) = requested {
        if !WORKSPACE_RE.is_match(req) || !ids.iter().any(|existing| existing == req) {
            return Err(WalletError::new("workspace_mismatch"));
        }
        return Ok(req.to_owned());
    }
    if ids.len() != 1 {
        return Err(WalletError::new("choose_workspace"));
    }
    Ok(ids.into_iter().next().unwrap_or_default())
}

pub(crate) fn balance_window(payload: &Value) -> Result<Value, WalletError> {
    let Some(obj) = payload.as_object() else {
        return Err(WalletError::new("invalid_response"));
    };
    let mode = obj.get("mode").and_then(Value::as_str);
    let billing = obj.get("billingMode").and_then(Value::as_str);
    if !matches!(mode, Some("pay-as-you-go" | "invoiceable"))
        || !matches!(billing, Some("prepaid" | "legacy" | "seat" | "credit"))
    {
        return Err(WalletError::new("invalid_response"));
    }
    if mode != Some("pay-as-you-go") || billing != Some("prepaid") {
        return Err(WalletError::new("wallet_not_prepaid"));
    }
    let raw = obj
        .get("balanceMicroCents")
        .and_then(Value::as_str)
        .filter(|s| SIGNED_INT20_RE.is_match(s))
        .ok_or_else(|| WalletError::new("invalid_response"))?;
    let micro: i128 = raw
        .parse()
        .map_err(|_| WalletError::new("invalid_response"))?;
    // Retain real debt (negative prepaid balance).
    let balance = (micro as f64) / 100_000_000.0;
    let expiration = ["expiresAt", "expires_at", "expiration"]
        .into_iter()
        .find_map(|k| iso(obj.get(k)));
    Ok(json!({
        "key": "zen-balance",
        "label": "Zen balance",
        "kind": "balance",
        "usedPercent": null,
        "remainingPercent": null,
        "used": null,
        "limit": null,
        "remaining": balance,
        "unit": "USD",
        "resetAt": null,
        "expiresAt": expiration,
        "windowMinutes": null
    }))
}

pub(crate) fn go_windows(payload: &Value) -> Vec<Value> {
    let Some(access) = payload
        .as_object()
        .and_then(|o| o.get("access"))
        .and_then(Value::as_object)
    else {
        return Vec::new();
    };
    let Some(meters) = access.get("meters").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    for (key, label, minutes) in [
        ("fiveHour", "5 hours", Some(300i64)),
        ("week", "Weekly", Some(10_080i64)),
        ("month", "Monthly", None),
    ] {
        let Some(meter) = meters.get(key).and_then(Value::as_object) else {
            continue;
        };
        let Some(used_str) = meter
            .get("usedMicroCents")
            .and_then(Value::as_str)
            .filter(|s| UNSIGNED_INT20_RE.is_match(s))
        else {
            continue;
        };
        let Some(limit_str) = meter
            .get("limitMicroCents")
            .and_then(Value::as_str)
            .filter(|s| UNSIGNED_INT20_RE.is_match(s))
        else {
            continue;
        };
        let (Ok(used), Ok(limit)) = (used_str.parse::<u128>(), limit_str.parse::<u128>()) else {
            continue;
        };
        if limit == 0 {
            continue;
        }
        let percent = (used as f64) * 100.0 / (limit as f64);
        if !percent.is_finite() || percent < 0.0 {
            continue;
        }
        let mut reset = iso(meter.get("resetsAt"));
        if key == "month" && reset.is_none() {
            reset = iso(access.get("endsAt"));
        }
        windows.push(json!({
            "key": format!("console-{key}"),
            "label": label,
            "kind": "rate_limit",
            "usedPercent": percent,
            "remainingPercent": (100.0 - percent).max(0.0),
            "used": null,
            "limit": null,
            "remaining": null,
            "unit": null,
            "resetAt": reset,
            "expiresAt": null,
            "windowMinutes": minutes
        }));
    }
    windows
}

pub(crate) async fn collect(
    cookies_val: &Value,
    requested: Option<&str>,
    client: &dyn WalletTransport,
    now_secs: Option<f64>,
) -> Result<(String, Value), WalletError> {
    let now = now_unix_secs(now_secs);
    let _validated = validate_cookies(cookies_val, now)?;
    let orgs = client.get("/console/api/orgs", None).await?;
    let workspace = select_workspace(&orgs, requested)?;
    let billing = client
        .get("/console/api/billing/status", Some(&workspace))
        .await?;
    let wallet = balance_window(&billing)?;
    let mut windows = Vec::new();
    let mut failure: Option<Value> = None;
    match client.get("/console/api/go/status", Some(&workspace)).await {
        Ok(go_payload) => {
            windows = go_windows(&go_payload);
        }
        Err(err) => {
            if err.code == "rate_limited" {
                let mut f = json!({ "code": "rate_limited" });
                if let Some(retry) = err.retry_after_seconds.filter(|r| r.is_finite()) {
                    f["retryAfterSeconds"] = json!(retry.clamp(0.0, 86_400.0));
                }
                failure = Some(f);
            }
        }
    }
    windows.push(wallet);
    let checked = utc_now_iso_secs(now_secs);
    let suffix = &sha256_hex(workspace.as_bytes())[..12];
    let plan: Option<&str> = (windows.len() > 1).then_some("Go");
    let mut sample = json!({
        "id": format!("plan-opencode-go-console-mac-{suffix}"),
        "provider": "opencode-go",
        "providerLabel": "OpenCode Go",
        "label": "OpenCode console wallet",
        "email": null,
        "plan": plan,
        "platform": "mac",
        "source": "Authenticated OpenCode console workspace on Mac",
        "status": "ok",
        "message": "Console workspace; API-key account identity has not been linked.",
        "fetchedAt": checked,
        "sampledAt": checked,
        "isActive": false,
        "windows": windows,
        "capabilities": {
            "codexProfile": null,
            "claudeProfileId": null,
            "claudePlatforms": []
        }
    });
    if let Some(f) = failure {
        sample["_usageFailure"] = f;
    }
    Ok((workspace, sample))
}

pub(crate) fn public_sample(sample: &Value) -> Value {
    let mut result = Map::new();
    if let Some(obj) = sample.as_object() {
        for key in PUBLIC_SAMPLE_KEYS {
            if let Some(v) = obj.get(key) {
                result.insert(key.to_owned(), v.clone());
            }
        }
        if obj
            .get("_usageFailure")
            .and_then(Value::as_object)
            .and_then(|f| f.get("code"))
            .and_then(Value::as_str)
            == Some("rate_limited")
        {
            result.insert("goUsageStatus".into(), json!("rate_limited"));
            result.insert("message".into(), json!(GO_RATE_LIMIT_MESSAGE));
        }
    }
    Value::Object(result)
}

/// Production reqwest-backed transport for `https://opencode.ai/console/api/...`.
pub(crate) struct ReqwestWalletTransport {
    header: String,
    deadline: Instant,
    now_secs: Option<f64>,
    client: Option<reqwest::Client>,
}

impl ReqwestWalletTransport {
    pub(crate) fn new(cookies: &[Value], now_secs: Option<f64>) -> Self {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .ok();
        Self {
            header: cookie_header(cookies),
            deadline: Instant::now() + Duration::from_secs(20),
            now_secs,
            client,
        }
    }
}

impl WalletTransport for ReqwestWalletTransport {
    fn get<'a>(
        &'a self,
        route: &'a str,
        workspace: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WalletError>> + Send + 'a>> {
        Box::pin(async move {
            if !matches!(
                route,
                "/console/api/orgs" | "/console/api/billing/status" | "/console/api/go/status"
            ) {
                return Err(WalletError::new("invalid_request"));
            }
            if let Some(ws) = workspace
                && !WORKSPACE_RE.is_match(ws)
            {
                return Err(WalletError::new("invalid_request"));
            }
            if (route == "/console/api/orgs") != workspace.is_none() {
                return Err(WalletError::new("invalid_request"));
            }
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(WalletError::new("network_error"));
            }
            let Some(ref client) = self.client else {
                return Err(WalletError::new("network_error"));
            };
            let url = format!("{ORIGIN}{route}");
            let mut req = client
                .get(&url)
                .timeout(remaining.min(Duration::from_secs(8)))
                .header("Cookie", &self.header)
                .header("Accept", "application/json")
                .header("Origin", ORIGIN)
                .header("Referer", format!("{ORIGIN}/console/"));
            if let Some(ws) = workspace {
                req = req.header("x-org-id", ws);
            }
            let mut resp = req
                .send()
                .await
                .map_err(|_| WalletError::new("network_error"))?;
            let status = resp.status().as_u16();
            if status == 401 || status == 403 {
                return Err(WalletError::new("needs_sign_in"));
            }
            if status == 429 {
                let hdr = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|h| h.to_str().ok());
                return Err(WalletError::with_retry(
                    "rate_limited",
                    retry_after(hdr, now_unix_secs(self.now_secs)),
                ));
            }
            if status != 200 {
                return Err(WalletError::new("provider_error"));
            }
            let mut body = Vec::new();
            while let Some(chunk) = resp
                .chunk()
                .await
                .map_err(|_| WalletError::new("network_error"))?
            {
                body.extend_from_slice(&chunk);
                if body.len() > MAX_BYTES || Instant::now() >= self.deadline {
                    return Err(WalletError::new("network_error"));
                }
            }
            serde_json::from_slice::<Value>(&body).map_err(|_| WalletError::new("network_error"))
        })
    }
}
