//! G14: the Analytics writer's observation port — dashboard rows and stored JSON observations
//! become the store's `usage.sampled` payload, exactly as `src/store/usage-observations.ts` and
//! the `account-analytics-history.ts` whitelist build them. The store decides which readings
//! are new; this module only shapes and whitelists. Raw helper bodies never enter the store.
//!
//! Domain: the ISO time forms every engine reads alike (`aac-num`'s proved grammar); dashboard
//! rows always carry `toISOString` spellings. Account identities reuse `aac_core::AccountKey`
//! (ASCII addresses, as the owned Analytics read admits). Anything outside the proved domain
//! is dropped here, never guessed — the TS sampler's own answer for such a row is the fallback
//! the cut keeps until the row's owner moves.
use crate::usage::{AccountObs, Sampled, WindowObs};
use aac_core::{AccountKey, ProviderId};
use regex::Regex;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

pub const MAX_ACCOUNTS_PER_SAMPLE: usize = 64;
const MAX_WINDOWS: usize = 256;
const VALID_STATUSES: [&str; 5] = ["ok", "cached", "unavailable", "error", "needs_sign_in"];
const KINDS: [&str; 4] = ["rate_limit", "balance", "spend", "extra_usage"];
static CREDENTIAL_SHAPE: LazyLock<Regex> = LazyLock::new(|| {
    // JS `\s` is exactly this class; the workspace regex carries no unicode-perl classes.
    Regex::new(r"(?i)(?:bearer[\t\n\x{000b}\x{000c}\r \x{00a0}\x{1680}\x{2000}-\x{200a}\x{2028}\x{2029}\x{202f}\x{205f}\x{3000}\x{feff}]|dca:|sk[-_]|eyJ[A-Za-z0-9_-]{8}|https?://|[{}]|access_token|refresh_token|client_secret)")
        .expect("static credential regex")
});
static CLEAR_SLUG: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[a-z0-9_]{1,40}$").expect("static slug regex"));
static POOL_ID_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9._:@/-]{0,127}$").expect("static pool regex"));
static MODEL_ID_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.:/@-]{0,159}$").expect("static model regex"));
static TIMESTAMP_SHAPE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[0-9]{4}-[0-9]{2}-[0-9]{2}T").expect("static timestamp regex"));
static COMPUTERS: LazyLock<Map<String, Value>> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../web-dashboard/public/computers.json")).unwrap_or_default()
});
static DASHBOARD_IDS: LazyLock<Vec<String>> = LazyLock::new(|| {
    serde_json::from_str::<Value>(include_str!("../../../config/tables/providers.json"))
        .ok()
        .and_then(|table| {
            table.get("providers").and_then(Value::as_object).map(|providers| {
                providers
                    .iter()
                    .filter(|(_, row)| row.get("dashboard").and_then(Value::as_bool).unwrap_or(false))
                    .map(|(id, _)| id.clone())
                    .collect()
            })
        })
        .unwrap_or_default()
});

fn object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}
fn utf16_len(text: &str) -> usize {
    text.encode_utf16().count()
}
fn has_control(text: &str) -> bool {
    text.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}')
}

/// `displayText`: bounded plain text, no control characters, nothing credential-shaped; the
/// trimmed text, or null. JS `trim` also strips the BOM.
pub fn display_text(value: &Value, maximum: usize) -> Option<String> {
    let text = value.as_str()?;
    if utf16_len(text) > maximum || has_control(text) || CREDENTIAL_SHAPE.is_match(text) {
        return None;
    }
    let trimmed = text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_owned())
    }
}

/// `safePoolId`.
fn safe_pool_id(value: &Value) -> Option<&str> {
    let text = value.as_str()?;
    if POOL_ID_SHAPE.is_match(text) && display_text(value, 128).is_some() {
        Some(text)
    } else {
        None
    }
}

/// `text(value, max)`: a nonempty bounded string without control characters (UTF-16 length).
fn text(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    if text.is_empty() || utf16_len(text) > max || has_control(text) {
        return None;
    }
    Some(text.to_owned())
}

/// `analyticsTimestamp`: an ISO-shaped string the proved grammar reads, normalized to the
/// `toISOString` spelling. Local-time and other engine-dependent forms are outside the domain.
pub fn analytics_timestamp(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    if !TIMESTAMP_SHAPE.is_match(text) {
        return None;
    }
    let ms = aac_num::ecma::parse_instant(text)?;
    aac_num::ecma::to_iso_string(ms)
}

fn ms(value: Option<&str>) -> Option<i64> {
    value.and_then(aac_num::ecma::parse_instant)
}

/// `numeric`: a finite number; a negative percentage is not a measurement.
fn numeric(value: Option<&Value>, percent: bool) -> Option<f64> {
    let number = value?.as_f64()?;
    if !number.is_finite() || (percent && number < 0.0) {
        return None;
    }
    Some(number)
}

fn opt_number(value: Option<f64>) -> Value {
    value.map(|n| serde_json::json!(n)).unwrap_or(Value::Null)
}

/// `analyticsWindow`: the JSON history's whitelist. The returned object keeps the TS field
/// order; conditional members appear only when the source proves them.
pub fn analytics_window(value: &Value) -> Option<Map<String, Value>> {
    let window = object(value)?;
    let key = text(window.get("key"), 64)?;
    let label = text(window.get("label"), 80)?;
    let cached = window.get("status").and_then(Value::as_str) == Some("cached");
    let cached_sampled_at = if cached {
        analytics_timestamp(window.get("sampledAt").unwrap_or(&Value::Null))?
    } else {
        String::new()
    };
    if let Some(clears) = window.get("clears") {
        let list = clears.as_array()?;
        if list.len() > 8 {
            return None;
        }
        let mut seen = HashSet::new();
        for slug in list {
            let slug = slug.as_str()?;
            if !CLEAR_SLUG.is_match(slug) || !seen.insert(slug) {
                return None;
            }
        }
    }
    let kind = window.get("kind").and_then(Value::as_str).filter(|k| KINDS.contains(k));
    let mut out = Map::new();
    out.insert("key".into(), Value::String(key));
    out.insert("label".into(), Value::String(label));
    out.insert(
        "usedPercent".into(),
        opt_number(numeric(window.get("usedPercent"), true)),
    );
    out.insert(
        "remainingPercent".into(),
        opt_number(numeric(window.get("remainingPercent"), true)),
    );
    out.insert(
        "resetAt".into(),
        analytics_timestamp(window.get("resetAt").unwrap_or(&Value::Null))
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    out.insert(
        "windowMinutes".into(),
        opt_number(numeric(window.get("windowMinutes"), false)),
    );
    out.insert("used".into(), opt_number(numeric(window.get("used"), false)));
    out.insert("limit".into(), opt_number(numeric(window.get("limit"), false)));
    out.insert(
        "unit".into(),
        text(window.get("unit"), 40).map(Value::String).unwrap_or(Value::Null),
    );
    if let Some(kind) = kind {
        out.insert("kind".into(), Value::String(kind.to_owned()));
    }
    out.insert("remaining".into(), opt_number(numeric(window.get("remaining"), false)));
    out.insert(
        "expiresAt".into(),
        analytics_timestamp(window.get("expiresAt").unwrap_or(&Value::Null))
            .map(Value::String)
            .unwrap_or(Value::Null),
    );
    if let Some(clears) = window.get("clears") {
        out.insert("clears".into(), clears.clone());
    }
    if let Some(unlimited) = window.get("unlimited").and_then(Value::as_bool) {
        out.insert("unlimited".into(), Value::Bool(unlimited));
    }
    if let Some(enabled) = window.get("enabled").and_then(Value::as_bool) {
        out.insert("enabled".into(), Value::Bool(enabled));
    }
    if cached {
        out.insert("status".into(), Value::String("cached".into()));
        out.insert("sampledAt".into(), Value::String(cached_sampled_at));
    }
    if let Some(pool) = safe_pool_id(window.get("poolId").unwrap_or(&Value::Null)) {
        out.insert("poolId".into(), Value::String(pool.to_owned()));
    }
    if let Some(source) = window.get("poolIdSource").and_then(Value::as_str)
        && (source == "provider-id" || source == "provider-bucket-membership")
    {
        out.insert("poolIdSource".into(), Value::String(source.to_owned()));
    }
    if let Some(pool_label) = display_text(window.get("poolLabel").unwrap_or(&Value::Null), 80) {
        out.insert("poolLabel".into(), Value::String(pool_label));
    }
    if let Some(models) = window.get("modelIds").and_then(Value::as_array) {
        let kept: Vec<Value> = models
            .iter()
            .take(64)
            .filter(|model| {
                model
                    .as_str()
                    .is_some_and(|m| MODEL_ID_SHAPE.is_match(m) && display_text(model, 160).is_some())
            })
            .cloned()
            .collect();
        out.insert("modelIds".into(), Value::Array(kept));
    }
    Some(out)
}

/// `analyticsWindowIdentity`: `[key, kind ?? null, unit]` plus a truthy pool id, JSON-spelled.
pub fn window_identity(window: &Map<String, Value>) -> String {
    let mut parts = vec![
        window.get("key").cloned().unwrap_or(Value::Null),
        window.get("kind").cloned().unwrap_or(Value::Null),
        window.get("unit").cloned().unwrap_or(Value::Null),
    ];
    if let Some(pool) = window.get("poolId").and_then(Value::as_str)
        && !pool.is_empty()
    {
        parts.push(Value::String(pool.to_owned()));
    }
    Value::Array(parts).to_string()
}

/// `accountAnalyticsIdentity` through the shared key derivation.
pub fn account_identity(account: &Map<String, Value>) -> Option<String> {
    let provider = ProviderId::new(account.get("provider")?.as_str()?).ok()?;
    let id = account.get("id")?.as_str()?;
    let email = match account.get("email") {
        None | Some(Value::Null) => None,
        Some(Value::String(email)) => Some(email.as_str()),
        Some(_) => return None,
    };
    AccountKey::derive(&provider, id, email)
        .ok()
        .map(|key| key.as_str().to_owned())
}

/// The computer table's OS, as `helperPlatform` maps it (linux hosts answer as ubuntu).
fn host_platform(host: &str) -> Option<&'static str> {
    let os = COMPUTERS.get(host)?.get("os")?.as_str()?;
    Some(match os {
        "linux" => "ubuntu",
        "mac" => "mac",
        "windows" => "windows",
        _ => return None,
    })
}

/// `sourcePlatform`: the live reading's host wins over the launch selector.
pub fn source_platform(account: &Map<String, Value>) -> String {
    if let Some(host) = account.get("host").and_then(Value::as_str)
        && let Some(platform) = host_platform(host)
    {
        return platform.to_owned();
    }
    let source = account.get("source").and_then(Value::as_str).unwrap_or("");
    if source.contains("on Windows") {
        return "windows".into();
    }
    if source.contains("on Mac") {
        return "mac".into();
    }
    account
        .get("platform")
        .and_then(Value::as_str)
        .unwrap_or("ubuntu")
        .to_owned()
}

/// The first `max` UTF-16 units, as JS slice counts them; a lone surrogate becomes U+FFFD
/// (the store's text checks refuse such a string anyway).
fn js_slice(text: &str, max: usize) -> String {
    let units: Vec<u16> = text.encode_utf16().take(max).collect();
    String::from_utf16_lossy(&units)
}

/// The store shape of one whitelisted window (shared by the live and backfill paths).
fn stored_window(safe: &Map<String, Value>) -> Option<WindowObs> {
    let cached_sampled_at_ms = if safe.get("status").and_then(Value::as_str) == Some("cached") {
        ms(safe.get("sampledAt").and_then(Value::as_str))
    } else {
        None
    };
    Some(WindowObs {
        identity: window_identity(safe),
        key: safe.get("key")?.as_str()?.to_owned(),
        label: safe.get("label")?.as_str()?.to_owned(),
        kind: safe.get("kind").and_then(Value::as_str).map(str::to_owned),
        unit: safe.get("unit").and_then(Value::as_str).map(str::to_owned),
        used_pct: safe.get("usedPercent").and_then(Value::as_f64),
        remaining_pct: safe.get("remainingPercent").and_then(Value::as_f64),
        used: safe.get("used").and_then(Value::as_f64),
        limit: safe.get("limit").and_then(Value::as_f64),
        remaining: safe.get("remaining").and_then(Value::as_f64),
        reset_at_ms: ms(safe.get("resetAt").and_then(Value::as_str)),
        expires_at_ms: ms(safe.get("expiresAt").and_then(Value::as_str)),
        window_minutes: safe.get("windowMinutes").and_then(Value::as_f64),
        unlimited: safe.get("unlimited").and_then(Value::as_bool),
        enabled: safe.get("enabled").and_then(Value::as_bool),
        cached_sampled_at_ms,
        pool_id: safe.get("poolId").and_then(Value::as_str).map(str::to_owned),
        pool_id_source: safe.get("poolIdSource").and_then(Value::as_str).map(str::to_owned),
        pool_label: safe.get("poolLabel").and_then(Value::as_str).map(str::to_owned),
        model_ids: safe
            .get("modelIds")
            .and_then(Value::as_array)
            .map(|models| models.iter().filter_map(|m| m.as_str().map(str::to_owned)).collect()),
    })
}

/// `storeWindow`: one whitelisted window in the store's shape.
pub fn store_window(value: &Value) -> Option<WindowObs> {
    if value["status"] == "unavailable" {
        return None;
    }
    stored_window(&analytics_window(value)?)
}

/// `storeObservationFromAccount`: the live sample of one dashboard row. A failed row records
/// its status with no windows at the sampling time; a retained (cached) window keeps its own
/// measured time and only inside the retention; no time in the future.
pub fn observation_from_account(account: &Value, now: i64, retention_ms: i64) -> Option<Sampled> {
    let row = object(account)?;
    // Infinity retention (the live sampler) saturates to the earliest time, keeping every reading.
    let cutoff = now.saturating_sub(retention_ms);
    let status = row.get("status")?.as_str()?;
    let healthy = status == "ok" || status == "cached";
    let mut windows: Vec<WindowObs> = Vec::new();
    if healthy {
        for window in row.get("windows")?.as_array()?.iter().take(MAX_WINDOWS) {
            let Some(stored) = store_window(window) else {
                continue;
            };
            if let Some(cached) = stored.cached_sampled_at_ms
                && (cached > now || cached < cutoff)
            {
                continue;
            }
            windows.push(stored);
        }
    }
    let sampled_iso = if healthy {
        analytics_timestamp(row.get("sampledAt").unwrap_or(&Value::Null))
            .or_else(|| analytics_timestamp(row.get("fetchedAt").unwrap_or(&Value::Null)))
    } else {
        aac_num::ecma::to_iso_string(now)
    }?;
    let sampled = ms(Some(&sampled_iso))?;
    if sampled > now || sampled < cutoff {
        return None;
    }
    let provider = row.get("provider")?.as_str()?.to_owned();
    let id = row.get("id")?.as_str()?;
    Some(Sampled {
        account: AccountObs {
            identity: account_identity(row)?,
            provider,
            account_id: js_slice(id, 128),
            plan: row
                .get("plan")
                .and_then(Value::as_str)
                .filter(|p| !p.is_empty())
                .map(|p| js_slice(p, 80)),
            platform: source_platform(row),
            source: js_slice(row.get("source").and_then(Value::as_str).unwrap_or(""), 120),
            status: status.to_owned(),
            is_active: row.get("isActive").and_then(Value::as_bool).unwrap_or(false),
        },
        sampled_at_ms: sampled,
        observed_at_ms: now,
        windows,
        import_ref: None,
    })
}

/// One decoded JSON-history observation (the whitelist already applied to its windows).
pub struct DecodedObservation {
    pub identity: String,
    pub account_id: String,
    pub sampled_at: String,
    pub observed_at: String,
    pub platform: String,
    pub source: String,
    pub status: String,
    pub is_active: bool,
    pub windows: Vec<Map<String, Value>>,
}

/// `decodeAccountAnalyticsObservation`: one stored observation, validated field by field.
pub fn decode_observation(value: &Value) -> Option<DecodedObservation> {
    let record = object(value)?;
    let sampled_at = analytics_timestamp(record.get("sampledAt").unwrap_or(&Value::Null))?;
    let observed_at = analytics_timestamp(record.get("observedAt").unwrap_or(&Value::Null))?;
    let identity = record.get("identity")?.as_str()?;
    if identity.len() != 64
        || !identity
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return None;
    }
    let account_id = text(record.get("accountId"), 128)?;
    let platform = record.get("platform")?.as_str()?;
    if !["ubuntu", "mac", "windows"].contains(&platform) {
        return None;
    }
    let status = record.get("status")?.as_str()?;
    if !VALID_STATUSES.contains(&status) {
        return None;
    }
    let source = text(record.get("source"), 120)?;
    let windows = record.get("windows")?.as_array()?;
    if windows.len() > MAX_WINDOWS {
        return None;
    }
    Some(DecodedObservation {
        identity: identity.to_owned(),
        account_id,
        sampled_at,
        observed_at,
        platform: platform.to_owned(),
        source,
        status: status.to_owned(),
        is_active: record.get("isActive").and_then(Value::as_bool).unwrap_or(false),
        windows: windows.iter().filter_map(analytics_window).collect(),
    })
}

/// `storeObservationFromRecord`: one backfill observation of the JSON history.
pub fn observation_from_record(record: &DecodedObservation, provider: &str, import_ref: &str) -> Option<Sampled> {
    let sampled = ms(Some(&record.sampled_at))?;
    let observed = ms(Some(&record.observed_at))?;
    let windows: Vec<WindowObs> = record.windows.iter().filter_map(stored_window).collect();
    Some(Sampled {
        account: AccountObs {
            identity: record.identity.clone(),
            provider: provider.to_owned(),
            account_id: record.account_id.clone(),
            plan: None,
            platform: record.platform.clone(),
            source: record.source.clone(),
            status: record.status.clone(),
            is_active: record.is_active,
        },
        sampled_at_ms: sampled,
        observed_at_ms: observed,
        windows,
        import_ref: Some(import_ref.to_owned()),
    })
}

/// `recordsFromText`: a history object, an array, or JSON lines; `parsed` says whether any
/// shape read at all.
pub fn records_from_text(text: &str) -> (Vec<Value>, bool) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return (Vec::new(), true);
    }
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        if let Some(list) = value.as_array() {
            return (list.clone(), true);
        }
        if let Some(record) = value.as_object() {
            if let Some(list) = record.get("records").and_then(Value::as_array) {
                return (list.clone(), true);
            }
            return (vec![value], true);
        }
        return (Vec::new(), false);
    }
    let mut records = Vec::new();
    let mut any = false;
    for line in trimmed.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(value) => {
                records.push(value);
                any = true;
            }
            Err(_) => records.push(Value::Null),
        }
    }
    (records, any)
}

/// `providerFor`: the live dashboard's provider for the identity, else the account id's prefix
/// when the dashboard table names it, else unknown.
pub fn provider_for(record: &DecodedObservation, known: &HashMap<String, String>) -> String {
    if let Some(live) = known.get(&record.identity) {
        return live.clone();
    }
    let prefix = record.account_id.split(':').next().unwrap_or("");
    if DASHBOARD_IDS.iter().any(|id| id == prefix) {
        prefix.to_owned()
    } else {
        "unknown".into()
    }
}
