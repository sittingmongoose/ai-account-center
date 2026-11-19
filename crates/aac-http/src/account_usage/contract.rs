//! The failover collector contract (`usage-failover-contract.ts`), the Claude optional-extra
//! rules (`usage-failover-claude-extras.ts`) and the Muse peer links (`muse-usage-peer-bindings.ts`),
//! ported for the native coordinator. Every screen is the TypeScript one; readings are normalized
//! through the same `analyticsWindow` allowlist so the persisted observation is byte-equal.

use aac_files::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// A 64-hex digest.
pub fn hex_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// JavaScript `value.length` (UTF-16 code units).
fn utf16_len(s: &str) -> usize {
    s.chars().map(char::len_utf16).sum()
}

fn control_c0(s: &str) -> bool {
    s.chars().any(|c| matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'))
}

/// JavaScript `\s`.
fn js_ws(c: char) -> bool {
    matches!(c,
        '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}'
        | '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}'
        | '\u{205f}' | '\u{3000}' | '\u{feff}')
}

fn js_trim(s: &str) -> &str {
    s.trim_matches(js_ws)
}

/// The case-insensitive credential screen shared by `safeUsageText` (no `[{}]`/`client_secret`).
fn safe_usage_secret(s: &str) -> bool {
    let lower: Vec<char> = s.chars().flat_map(char::to_lowercase).collect();
    let at = |i: usize, word: &str| -> bool {
        let w: Vec<char> = word.chars().collect();
        lower.len() >= i + w.len() && lower[i..i + w.len()] == w[..]
    };
    (0..lower.len()).any(|i| {
        ["sk-", "sk_", "dca:", "http://", "https://", "access_token", "refresh_token"]
            .iter()
            .any(|word| at(i, word))
            || (at(i, "bearer") && lower.get(i + 6).is_some_and(|c| js_ws(*c)))
            || (at(i, "eyj")
                && lower.len() >= i + 11
                && lower[i + 3..i + 11]
                    .iter()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '_' || *c == '-'))
    })
}

/// `safeUsageText(value, max)`: trimmed non-empty, bounded, no control chars or credential shapes.
pub fn safe_usage_text(value: &Value, max: usize) -> Option<String> {
    let s = value.as_str()?;
    if js_trim(s).is_empty() || utf16_len(s) > max || control_c0(s) || safe_usage_secret(s) {
        return None;
    }
    Some(js_trim(s).to_owned())
}

/// `usageEmail(value)`.
pub fn usage_email(value: &Value) -> Option<String> {
    let text = safe_usage_text(value, 254)?;
    let bytes = text.as_bytes();
    let at = bytes.iter().position(|b| *b == b'@')?;
    let local = &text[..at];
    let rest = &text[at + 1..];
    let dot = rest.iter().position(|b| *b == b'.')?;
    let name = &rest[..dot];
    let suffix = &rest[dot + 1..];
    let no_ws = |part: &str| {
        !part.is_empty()
            && !part.chars().any(|c| js_ws(c) || c == '@')
    };
    (no_ws(local) && no_ws(name) && no_ws(suffix) && !rest.contains('@')).then_some(text)
}

/// `Date.parse` for the ISO forms the contract meets; NaN is None.
pub fn parse_ms(s: &str) -> Option<f64> {
    aac_views::js::parse_date(s).ok().flatten().map(|ms| ms as f64)
}

fn iso(ms: f64) -> Option<String> {
    if !ms.is_finite() {
        return None;
    }
    aac_num::ecma::to_iso_string(ms as i64)
}

/// `usageTimestamp(value, now)`: ISO-prefixed, bounded, in [2000, now + 5 min].
pub fn usage_timestamp(value: &Value, now: f64) -> Option<String> {
    let s = value.as_str()?;
    if s.len() > 64 || !s.starts_with(|c: char| c.is_ascii_digit()) || s.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    let b = s.as_bytes();
    let shaped = b.len() >= 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit);
    if !shaped {
        return None;
    }
    let parsed = parse_ms(s)?;
    if parsed >= 946_684_800_000.0 && parsed <= now + 300_000.0 {
        iso(parsed)
    } else {
        None
    }
}

/// `analyticsTimestamp(value)`: ISO-prefixed, finite, normalized (no range bound).
fn analytics_timestamp(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let b = s.as_bytes();
    let shaped = b.len() >= 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T';
    if !shaped {
        return None;
    }
    iso(parse_ms(s)?)
}

/// A validated identity `{kind, key}`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageIdentity {
    /// The identity kind (a lowercase slug).
    pub kind: String,
    /// The 64-hex identity digest.
    pub key: String,
}

impl UsageIdentity {
    /// The JSON value the contract hashes and persists.
    pub fn value(&self) -> Value {
        json!({"kind": self.kind, "key": self.key})
    }
}

/// `parseUsageIdentity`.
pub fn parse_identity(value: &Value) -> Option<UsageIdentity> {
    let obj = value.as_object()?;
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["key", "kind"] {
        return None;
    }
    let kind = obj.get("kind")?.as_str()?;
    let kb = kind.as_bytes();
    let kind_ok = (1..=64).contains(&kb.len())
        && kb[0].is_ascii_lowercase()
        && kb[1..].iter().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-');
    let key = obj.get("key")?.as_str()?;
    (kind_ok && hex_digest(key)).then(|| UsageIdentity { kind: kind.to_owned(), key: key.to_owned() })
}

/// `sameUsageIdentity`.
pub fn same_identity(a: Option<&UsageIdentity>, b: Option<&UsageIdentity>) -> bool {
    matches!((a, b), (Some(x), Some(y)) if x.kind == y.kind && x.key == y.key)
}

/// A validated inventory candidate.
#[derive(Clone, Debug, PartialEq)]
pub struct UsageCandidate {
    /// The 64-hex source locator.
    pub locator: String,
    /// The deduped selectors.
    pub selectors: Vec<String>,
    /// The credential identity.
    pub identity: UsageIdentity,
    /// The 64-hex credential revision.
    pub revision: String,
    /// The screened email, or null.
    pub email: Option<String>,
    /// The screened plan, or null.
    pub plan: Option<String>,
}

impl UsageCandidate {
    /// The JSON value the contract persists.
    pub fn value(&self) -> Value {
        json!({
            "locator": self.locator,
            "selectors": self.selectors,
            "identity": self.identity.value(),
            "revision": self.revision,
            "email": self.email.clone().map(Value::String).unwrap_or(Value::Null),
            "plan": self.plan.clone().map(Value::String).unwrap_or(Value::Null),
        })
    }
}

fn selector_ok(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=160).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || b":_.@-".contains(c))
}

/// `parseUsageCandidate`.
pub fn parse_candidate(value: &Value) -> Option<UsageCandidate> {
    let obj = value.as_object()?;
    if obj.keys().any(|k| !["locator", "selectors", "identity", "revision", "email", "plan"].contains(&k.as_str())) {
        return None;
    }
    let identity = parse_identity(obj.get("identity")?)?;
    let locator = obj.get("locator")?.as_str()?;
    let revision = obj.get("revision")?.as_str()?;
    let selectors = obj.get("selectors")?.as_array()?;
    if !hex_digest(locator) || !hex_digest(revision) || selectors.is_empty() || selectors.len() > 32 {
        return None;
    }
    let mut dedup: Vec<String> = Vec::new();
    for s in selectors {
        let s = s.as_str().filter(|s| selector_ok(s))?;
        if !dedup.iter().any(|d| d == s) {
            dedup.push(s.to_owned());
        }
    }
    let email_raw = obj.get("email").unwrap_or(&Value::Null);
    let plan_raw = obj.get("plan").unwrap_or(&Value::Null);
    if !email_raw.is_null() && usage_email(email_raw).is_none() {
        return None;
    }
    if !plan_raw.is_null() && safe_usage_text(plan_raw, 160).is_none() {
        return None;
    }
    Some(UsageCandidate {
        locator: locator.to_owned(),
        selectors: dedup,
        identity,
        revision: revision.to_owned(),
        email: usage_email(email_raw),
        plan: safe_usage_text(plan_raw, 160),
    })
}

/// `text(value, max)` from the analytics history allowlist (no trim).
fn history_text(value: &Value, max: usize) -> Option<String> {
    let s = value.as_str()?;
    if control_c0(s) {
        return None;
    }
    let len = utf16_len(s);
    (len > 0 && len <= max).then(|| s.to_owned())
}

/// `numeric(value, percent)`.
fn numeric(value: &Value, percent: bool) -> Option<f64> {
    let n = value.as_f64().filter(|n| n.is_finite())?;
    if percent && n < 0.0 { None } else { Some(n) }
}

fn clear_slug(s: &str) -> bool {
    let b = s.as_bytes();
    (1..=40).contains(&b.len()) && b.iter().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_')
}

fn safe_pool_id(value: &Value) -> bool {
    let Some(s) = value.as_str() else { return false };
    let b = s.as_bytes();
    let shaped = (1..=128).contains(&b.len())
        && b[0].is_ascii_alphanumeric()
        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || b"._:@/-".contains(c));
    shaped && aac_views::antigravity::display_text(Some(value), 128).is_some()
}

/// `analyticsWindow(value)`: the measured-window allowlist, field order as the TS literal.
pub fn analytics_window(value: &Value) -> Option<Value> {
    let obj = value.as_object()?;
    let key = history_text(obj.get("key")?, 64)?;
    let label = history_text(obj.get("label")?, 80)?;
    let cached_sampled = if obj.get("status").and_then(Value::as_str) == Some("cached") {
        analytics_timestamp(obj.get("sampledAt").unwrap_or(&Value::Null))
    } else {
        None
    };
    if obj.get("status").and_then(Value::as_str) == Some("cached") && cached_sampled.is_none() {
        return None;
    }
    if let Some(clears) = obj.get("clears") {
        let arr = clears.as_array()?;
        if arr.len() > 8 {
            return None;
        }
        let mut seen: Vec<&str> = Vec::new();
        for slug in arr {
            let s = slug.as_str().filter(|s| clear_slug(s))?;
            if seen.contains(&s) {
                return None;
            }
            seen.push(s);
        }
    }
    let kind = obj.get("kind").and_then(Value::as_str).filter(|k| {
        ["rate_limit", "balance", "spend", "extra_usage"].contains(k)
    });
    let mut out = Map::new();
    out.insert("key".into(), json!(key));
    out.insert("label".into(), json!(label));
    out.insert("usedPercent".into(), opt_num(numeric(obj.get("usedPercent").unwrap_or(&Value::Null), true)));
    out.insert("remainingPercent".into(), opt_num(numeric(obj.get("remainingPercent").unwrap_or(&Value::Null), true)));
    out.insert("resetAt".into(), opt_str(analytics_timestamp(obj.get("resetAt").unwrap_or(&Value::Null))));
    out.insert("windowMinutes".into(), opt_num(numeric(obj.get("windowMinutes").unwrap_or(&Value::Null), false)));
    out.insert("used".into(), opt_num(numeric(obj.get("used").unwrap_or(&Value::Null), false)));
    out.insert("limit".into(), opt_num(numeric(obj.get("limit").unwrap_or(&Value::Null), false)));
    out.insert("unit".into(), opt_str(history_text(obj.get("unit").unwrap_or(&Value::Null), 40)));
    if let Some(kind) = kind {
        out.insert("kind".into(), json!(kind));
    }
    out.insert("remaining".into(), opt_num(numeric(obj.get("remaining").unwrap_or(&Value::Null), false)));
    out.insert("expiresAt".into(), opt_str(analytics_timestamp(obj.get("expiresAt").unwrap_or(&Value::Null))));
    if let Some(clears) = obj.get("clears") {
        out.insert("clears".into(), clears.clone());
    }
    if let Some(unlimited) = obj.get("unlimited").and_then(Value::as_bool) {
        out.insert("unlimited".into(), json!(unlimited));
    }
    if let Some(enabled) = obj.get("enabled").and_then(Value::as_bool) {
        out.insert("enabled".into(), json!(enabled));
    }
    if let Some(sampled) = cached_sampled {
        out.insert("status".into(), json!("cached"));
        out.insert("sampledAt".into(), json!(sampled));
    }
    let pool_id = obj.get("poolId").unwrap_or(&Value::Null);
    if safe_pool_id(pool_id) {
        out.insert("poolId".into(), pool_id.clone());
    }
    let pool_source = obj.get("poolIdSource").and_then(Value::as_str);
    if matches!(pool_source, Some("provider-id") | Some("provider-bucket-membership")) {
        out.insert("poolIdSource".into(), json!(pool_source.unwrap()));
    }
    let pool_label = obj.get("poolLabel").unwrap_or(&Value::Null);
    if let Some(display) = aac_views::antigravity::display_text(Some(pool_label), 80) {
        out.insert("poolLabel".into(), json!(display));
    }
    if let Some(models) = obj.get("modelIds").and_then(Value::as_array) {
        let filtered: Vec<Value> = models
            .iter()
            .take(64)
            .filter(|model| {
                model.as_str().is_some_and(|m| {
                    let b = m.as_bytes();
                    (1..=160).contains(&b.len())
                        && b[0].is_ascii_alphanumeric()
                        && b[1..].iter().all(|c| c.is_ascii_alphanumeric() || b"_.:/@-".contains(c))
                        && aac_views::antigravity::display_text(Some(model), 160).is_some()
                })
            })
            .cloned()
            .collect();
        out.insert("modelIds".into(), Value::Array(filtered));
    }
    Some(Value::Object(out))
}

fn opt_num(n: Option<f64>) -> Value {
    n.map(|n| serde_json::Number::from_f64(n).map(Value::Number).unwrap_or(Value::Null)).unwrap_or(Value::Null)
}

fn opt_str(s: Option<String>) -> Value {
    s.map(Value::String).unwrap_or(Value::Null)
}

/// A normalized reading (the persisted `UsageReading`).
#[derive(Clone, Debug, PartialEq)]
pub struct UsageReading {
    /// The reading status.
    pub status: String,
    /// The screened email.
    pub email: Option<String>,
    /// The screened (or antigravity-resolved) plan.
    pub plan: Option<String>,
    /// The entitlement class, when present.
    pub plan_class: Option<String>,
    /// Normalized sample time.
    pub sampled_at: Option<String>,
    /// Normalized fetch time.
    pub fetched_at: Option<String>,
    /// The allowlisted windows.
    pub windows: Vec<Value>,
    /// The private optional-extra context digest.
    pub optional_extras_context: Option<String>,
    /// The optional-extra availability.
    pub optional_extras: Option<Value>,
    /// The antigravity plan detail.
    pub antigravity_plan: Option<Value>,
}

impl UsageReading {
    /// The JSON value the contract persists, field order as the TS literal.
    pub fn value(&self) -> Value {
        let mut out = Map::new();
        out.insert("status".into(), json!(self.status));
        out.insert("email".into(), opt_str(self.email.clone()));
        out.insert("plan".into(), opt_str(self.plan.clone()));
        if let Some(class) = &self.plan_class {
            out.insert("planClass".into(), json!(class));
        }
        out.insert("sampledAt".into(), opt_str(self.sampled_at.clone()));
        out.insert("fetchedAt".into(), opt_str(self.fetched_at.clone()));
        out.insert("windows".into(), Value::Array(self.windows.clone()));
        if let Some(ctx) = &self.optional_extras_context {
            out.insert("optionalExtrasContext".into(), json!(ctx));
        }
        if let Some(extras) = &self.optional_extras {
            out.insert("optionalExtras".into(), extras.clone());
        }
        if let Some(detail) = &self.antigravity_plan {
            out.insert("antigravityPlan".into(), detail.clone());
        }
        Value::Object(out)
    }

    /// `hasUsage`.
    pub fn has_usage(&self) -> bool {
        (self.status == "ok" || self.status == "cached") && !self.windows.is_empty() && self.sampled_at.is_some()
    }
}

/// `parseUsageReading(value, now)`.
pub fn parse_reading(value: &Value, now: f64) -> Option<UsageReading> {
    let obj = value.as_object()?;
    let status = obj.get("status").and_then(Value::as_str)?;
    if !["ok", "cached", "unavailable", "error", "needs_sign_in"].contains(&status) {
        return None;
    }
    let windows_raw = obj.get("windows")?.as_array()?;
    if windows_raw.len() > 256 {
        return None;
    }
    let raw_class = obj.get("planClass");
    if let Some(class) = raw_class {
        if !class.is_null() && class.as_str() != Some("max") && class.as_str() != Some("pro") {
            return None;
        }
    }
    let sampled_at = usage_timestamp(obj.get("sampledAt").unwrap_or(&Value::Null), now);
    let fetched_at = usage_timestamp(obj.get("fetchedAt").unwrap_or(&Value::Null), now);
    let successful = status == "ok" || status == "cached";
    if successful {
        let (Some(s), Some(f)) = (&sampled_at, &fetched_at) else { return None };
        if parse_ms(s)? > parse_ms(f)? + 300_000.0 {
            return None;
        }
    }
    let mut windows: Vec<Value> = Vec::new();
    let mut keys: Vec<String> = Vec::new();
    for raw in windows_raw {
        let raw_key = raw.get("key").and_then(Value::as_str).unwrap_or("").to_owned();
        if safe_usage_text(raw.get("key").unwrap_or(&Value::Null), 64).is_none()
            || safe_usage_text(raw.get("label").unwrap_or(&Value::Null), 80).is_none()
            || keys.contains(&raw_key)
        {
            return None;
        }
        let window = analytics_window(raw)?;
        for field in ["usedPercent", "used", "limit", "windowMinutes"] {
            if let Some(n) = window.get(field).and_then(Value::as_f64) {
                if n < 0.0 {
                    return None;
                }
            }
        }
        if let Some(rp) = window.get("remainingPercent").and_then(Value::as_f64) {
            if !(0.0..=100.0).contains(&rp) {
                return None;
            }
        }
        if let Some(unit) = window.get("unit") {
            if !unit.is_null() && safe_usage_text(unit, 40).is_none() {
                return None;
            }
        }
        keys.push(window.get("key").and_then(Value::as_str).unwrap_or("").to_owned());
        windows.push(window);
    }
    let plan_class = raw_class.and_then(Value::as_str).filter(|c| *c == "max" || *c == "pro").map(str::to_owned);
    let mut reading = UsageReading {
        status: status.to_owned(),
        email: usage_email(obj.get("email").unwrap_or(&Value::Null)),
        plan: safe_usage_text(obj.get("plan").unwrap_or(&Value::Null), 160),
        plan_class,
        sampled_at,
        fetched_at,
        windows,
        optional_extras_context: None,
        optional_extras: None,
        antigravity_plan: None,
    };
    if let Some(ctx) = obj.get("optionalExtrasContext").and_then(Value::as_str) {
        if hex_digest(ctx) {
            reading.optional_extras_context = Some(ctx.to_owned());
        }
    }
    if let Some(extras) = obj.get("optionalExtras").and_then(Value::as_object) {
        let rc = extras.get("resetCredits").and_then(Value::as_str);
        let pb = extras.get("prepaidBalance").and_then(Value::as_str);
        if matches!(rc, Some("ok") | Some("unavailable")) && matches!(pb, Some("ok") | Some("unavailable")) {
            reading.optional_extras = Some(json!({"resetCredits": rc.unwrap(), "prepaidBalance": pb.unwrap()}));
        }
    }
    let observed = reading.sampled_at.as_deref().or(reading.fetched_at.as_deref()).and_then(parse_ms);
    let clock = now;
    if let Some(reported) = obj.get("reportedPlan").filter(|v| v.is_object()) {
        let (plan, detail) = aac_views::antigravity::reading_plan_display(reading.plan.clone(), observed.map(|m| m as i64), clock, Some(reported));
        reading.plan = plan.as_str().map(str::to_owned);
        reading.antigravity_plan = detail;
    } else if obj.get("antigravityPlan").is_some_and(Value::is_object) {
        let (_plan, detail) = aac_views::antigravity::reading_plan_display(reading.plan.clone(), observed.map(|m| m as i64), clock, None);
        reading.antigravity_plan = detail;
    }
    Some(reading)
}

/// The failure codes the coordinator recognizes.
pub const USAGE_FAILURE_CODES: [&str; 19] = [
    "unavailable", "not_installed", "needs_sign_in", "identity_mismatch", "account_mismatch",
    "source_changed", "rate_limited", "invalid_response", "network_error", "provider_error",
    "error", "busy", "no_browser_cookie", "local_storage_error", "identity_unavailable",
    "plan_mismatch", "team_mismatch", "choose_team", "inactive_subscription",
];

/// Whether a failure code is recognized.
pub fn is_failure_code(code: &str) -> bool {
    USAGE_FAILURE_CODES.contains(&code)
}

// ---- Claude optional extras (usage-failover-claude-extras.ts) ----

const MAX_AGE_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;



/// The optional-extra group a window key belongs to.
fn extra_group(key: &str) -> Option<&'static str> {
    if key == "prepaid_balance" {
        return Some("prepaidBalance");
    }
    if key == "reset_credits_available" || key == "session_reset_allowance" {
        return Some("resetCredits");
    }
    // reset_credit_(available|saved|used|expired)_grant_(1..20)
    for state in ["available", "saved", "used", "expired"] {
        let prefix = format!("reset_credit_{state}_grant_");
        if let Some(num) = key.strip_prefix(&prefix) {
            if let Ok(n) = num.parse::<u32>() {
                if (1..=20).contains(&n) && num == n.to_string() {
                    return Some("resetCredits");
                }
            }
        }
    }
    None
}

/// `stripClaudeFailoverExtras`.
pub fn strip_claude_extras(reading: &UsageReading) -> UsageReading {
    let mut result = reading.clone();
    result.windows.retain(|w| extra_group(w.get("key").and_then(Value::as_str).unwrap_or("")).is_none());
    result.optional_extras = None;
    result.optional_extras_context = None;
    result
}

/// `projectClaudeFailoverExtras`: reapply the optional-extra age bound.
pub fn project_claude_extras(reading: &UsageReading, now: f64) -> UsageReading {
    let windows: Vec<Value> = reading
        .windows
        .iter()
        .filter(|window| {
            let key = window.get("key").and_then(Value::as_str).unwrap_or("");
            if extra_group(key).is_none() {
                return true;
            }
            let original = if window.get("status").and_then(Value::as_str) == Some("cached") {
                window.get("sampledAt").and_then(Value::as_str)
            } else {
                window
                    .get("sampledAt")
                    .and_then(Value::as_str)
                    .or(reading.sampled_at.as_deref())
                    .or(reading.fetched_at.as_deref())
            };
            match original.and_then(parse_ms) {
                Some(sampled) => sampled <= now && now - sampled <= MAX_AGE_MS,
                None => false,
            }
        })
        .cloned()
        .collect();
    if windows.len() == reading.windows.len() {
        reading.clone()
    } else {
        let mut result = reading.clone();
        result.windows = windows;
        result
    }
}

/// `retainClaudeFailoverExtras`.
pub fn retain_claude_extras(previous: Option<&UsageReading>, next: &UsageReading, same_source: bool, now: f64) -> UsageReading {
    let Some(previous) = previous else { return next.clone() };
    let ctx_ok = next.optional_extras_context.as_deref().is_some_and(hex_digest);
    if !same_source
        || previous.email != next.email
        || previous.plan != next.plan
        || previous.plan_class != next.plan_class
        || !ctx_ok
        || next.optional_extras_context != previous.optional_extras_context
        || next.fetched_at.is_none()
    {
        return next.clone();
    }
    let Some(fetched_at) = next.fetched_at.as_deref().and_then(parse_ms) else { return next.clone() };
    let reported: Vec<Option<&str>> = next.windows.iter().map(|w| extra_group(w.get("key").and_then(Value::as_str).unwrap_or(""))).collect();
    let mut retained: Vec<Value> = Vec::new();
    for window in &previous.windows {
        let kind = extra_group(window.get("key").and_then(Value::as_str).unwrap_or(""));
        let sampled_at = window.get("sampledAt").and_then(Value::as_str).or(previous.fetched_at.as_deref());
        let timestamp = sampled_at.and_then(parse_ms);
        let unavailable = kind.and_then(|k| next.optional_extras.as_ref().and_then(|e| e.get(k)).and_then(Value::as_str)) == Some("unavailable");
        let already_reported = kind.is_some() && reported.iter().any(|r| *r == kind);
        let Some(ts) = timestamp else { continue };
        if kind.is_none() || !unavailable || already_reported || sampled_at.is_none() || now - ts > MAX_AGE_MS || ts > now || ts > fetched_at {
            continue;
        }
        let mut w = window.clone();
        w["status"] = json!("cached");
        w["sampledAt"] = json!(sampled_at.unwrap());
        retained.push(w);
    }
    if retained.is_empty() {
        next.clone()
    } else {
        let mut result = next.clone();
        result.windows.extend(retained);
        result
    }
}

// ---- Muse peer links (muse-usage-peer-bindings.ts) ----

/// A bound native Muse peer.
#[derive(Clone, Debug, PartialEq)]
pub struct MuseUsagePeer {
    /// The computer the peer lives on.
    pub host: String,
    /// The native credential identity.
    pub identity: UsageIdentity,
}

/// `musePeerBindingFile`.
pub fn muse_peer_binding_file(ccs_dir: &Path, key: &str) -> PathBuf {
    ccs_dir.join("account-usage-failover").join(format!("{key}.muse-peers.json"))
}

/// The private observation/cache read (`readUsageCache`), bounded and owner-held.
pub fn read_private_cache(file: &Path) -> Value {
    let Some(parent) = file.parent() else { return Value::Null };
    let policy = ReadPolicy::capped(256 * 1024)
        .mode(Mode::Private)
        .owner(Owner::Mine)
        .ancestors(Ancestors::NoLinkBelow(parent));
    match aac_files::read::read(file, &policy) {
        Read::Bytes(bytes, _) => serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        _ => Value::Null,
    }
}

/// `parseMuseUsagePeer`.
pub fn parse_muse_peer(value: &Value, dashboard_hosts: &[&str]) -> Option<MuseUsagePeer> {
    let obj = value.as_object()?;
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["host", "identity"] {
        return None;
    }
    let host = obj.get("host")?.as_str()?;
    if !dashboard_hosts.iter().any(|h| *h == host) {
        return None;
    }
    let identity = parse_identity(obj.get("identity")?)?;
    (identity.kind == "muse-native-credential").then(|| MuseUsagePeer { host: host.to_owned(), identity })
}

/// `isBoundMusePeer`.
pub fn is_bound_muse_peer(
    ccs_dir: &Path,
    key: &str,
    primary: Option<&UsageIdentity>,
    peer: &UsageIdentity,
    host: &str,
    dashboard_hosts: &[&str],
) -> bool {
    let Some(primary) = primary else { return false };
    if !hex_digest(key)
        || !["muse-team", "muse-native-credential"].contains(&primary.kind.as_str())
        || peer.kind != "muse-native-credential"
    {
        return false;
    }
    let value = read_private_cache(&muse_peer_binding_file(ccs_dir, key));
    let Some(obj) = value.as_object() else { return false };
    let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
    keys.sort_unstable();
    if keys != ["identity", "key", "peers", "version"] {
        return false;
    }
    if obj.get("version").and_then(Value::as_f64) != Some(1.0) || obj.get("key").and_then(Value::as_str) != Some(key) {
        return false;
    }
    if !same_identity(parse_identity(obj.get("identity").unwrap_or(&Value::Null)).as_ref(), Some(primary)) {
        return false;
    }
    let Some(peers) = obj.get("peers").and_then(Value::as_array) else { return false };
    if peers.is_empty() || peers.len() > 8 {
        return false;
    }
    let parsed: Vec<Option<MuseUsagePeer>> = peers.iter().map(|p| parse_muse_peer(p, dashboard_hosts)).collect();
    if parsed.iter().any(|p| p.is_none()) {
        return false;
    }
    parsed.iter().any(|p| p.as_ref().is_some_and(|p| p.host == host && same_identity(Some(&p.identity), Some(peer))))
}
