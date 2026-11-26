//! Exact port of `scripts/account-usage/plan_common.py` and `scripts/account-usage/plan_usage.py`.
use crate::failover_additional::{
    CollectionError, DpapiUnprotect, PlanHttpBody, PlanTransport,
    desktop::{ReadTracker, now_unix_secs, parse_iso_utc},
};
use aac_core::sha256_hex;
use aac_files::read::{Mode, Read, ReadPolicy};
use aac_providers::{PLAN_RETRY_AFTER, ProviderId};
use base64::{Engine, engine::general_purpose::STANDARD};
use chrono::{DateTime, Datelike, SecondsFormat};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    future::Future,
    io::ErrorKind,
    path::Path,
    pin::Pin,
    sync::LazyLock,
    time::{Duration, Instant},
};

pub(crate) const MAX_BYTES: usize = 1024 * 1024;
const MAX_KEY_FILE_BYTES: usize = 4096;
const KEY_ENTROPY: &[u8] = b"AAC/account-key/v1";
const OPENCODE_GO_URL: &str = "https://opencode.ai/zen/go/v1/usage";

static KEY_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[a-f0-9]{8}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static CAPSULE_ID_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A(?:default|[a-f0-9]{8})\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static KEY_SECRET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[\x21-\x7e]{8,512}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static EMAIL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\A[^ \t\r\n\f\v@\x00-\x1f]+@[^ \t\r\n\f\v@\x00-\x1f]+\.[^ \t\r\n\f\v@\x00-\x1f]+\z",
    )
    .unwrap_or_else(|_| unreachable!("constant regex"))
});
static PLAN_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Za-z][A-Za-z0-9]{0,15}(?: [A-Za-z0-9]{1,16}){0,2}\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static CURRENCY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[A-Z]{3}\z").unwrap_or_else(|_| unreachable!("constant regex"))
});
static KIMI_LEVEL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\ALEVEL_[A-Z]{1,16}(?:_[A-Z]{1,16}){0,2}\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static ZAI_CN_TIME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[0-9]{4}-[0-9]{2}-[0-9]{2} [0-9]{2}:[0-9]{2}:[0-9]{2}\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static SEC_TOKEN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"SEC_TOKEN:[ \t\r\n\f\v]*"([^"\r\n]+)""#)
        .unwrap_or_else(|_| unreachable!("constant regex"))
});
static SAFARI_COOKIE_NAME_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A[!#$%&'*+\-.^_`|~0-9A-Za-z]{1,256}\z")
        .unwrap_or_else(|_| unreachable!("constant regex"))
});

/// `plan_common.retry_after_seconds` clamped to `aac_providers::PLAN_RETRY_AFTER`.
pub(crate) fn retry_after_seconds(value: Option<&str>, now_secs: f64) -> f64 {
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
        Some(s) => s.clamp(
            PLAN_RETRY_AFTER.floor_s as f64,
            PLAN_RETRY_AFTER.ceiling_s as f64,
        ),
        None => PLAN_RETRY_AFTER.default_s as f64,
    }
}

pub(crate) fn plan_string(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    if s.is_empty() || s.chars().count() > 32_768 || s.contains('\0') {
        return None;
    }
    Some(s.to_owned())
}

pub(crate) fn email_address(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    if s.chars().count() > 254 {
        return None;
    }
    EMAIL_RE.is_match(s).then(|| s.to_owned())
}

pub(crate) fn api_number(value: &Value) -> Option<f64> {
    let parsed = match value {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    (parsed.is_finite() && parsed >= 0.0).then_some(parsed)
}

pub(crate) fn reset_at(value: &Value) -> Option<String> {
    if value.is_null() || value.is_boolean() {
        return None;
    }
    let timestamp = match value {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite() && *x >= 0.0),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|x| x.is_finite() && *x >= 0.0),
        _ => None,
    };
    if let Some(mut ts) = timestamp {
        if ts <= 0.0 {
            return None;
        }
        if ts > 100_000_000_000.0 {
            ts /= 1000.0;
        }
        let (iso, year) = aac_num::py::iso_seconds_of_timestamp(ts)?;
        return (2000..=2200).contains(&year).then_some(iso);
    }
    let raw = value.as_str()?;
    let dt = parse_iso_utc(raw)?;
    if !(2000..=2200).contains(&dt.year()) {
        return None;
    }
    Some(dt.to_rfc3339_opts(SecondsFormat::Secs, true))
}

pub(crate) fn plan_label(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    let lower = raw.to_ascii_lowercase();
    if matches!(
        lower.as_str(),
        "free"
            | "lite"
            | "essential"
            | "basic"
            | "standard"
            | "pro"
            | "professional"
            | "premium"
            | "max"
            | "ultra"
            | "team"
            | "business"
            | "enterprise"
            | "go"
    ) {
        return Some(lower);
    }
    let name = raw.replace('_', " ");
    let has_four_digits = name
        .as_bytes()
        .windows(4)
        .any(|w| w.iter().all(u8::is_ascii_digit));
    if name.len() <= 32 && PLAN_NAME_RE.is_match(&name) && !has_four_digits {
        Some(name)
    } else {
        None
    }
}

#[derive(Default)]
struct UsageWindowSpec<'a> {
    key: &'a str,
    label: &'a str,
    used_percent: Option<f64>,
    reset: Option<&'a Value>,
    minutes: Option<f64>,
    used: Option<f64>,
    limit: Option<f64>,
    unit: Option<&'a str>,
    kind: Option<&'a str>,
    remaining: Option<f64>,
    expires_at: Option<&'a Value>,
    unlimited: Option<bool>,
    enabled: Option<bool>,
}

fn usage_window(spec: UsageWindowSpec<'_>) -> Value {
    let used_pct = spec.used_percent.filter(|n| n.is_finite() && *n >= 0.0);
    let rem_pct = used_pct.and_then(|u| aac_num::py::round((100.0 - u).max(0.0), 8));
    let mut map = Map::new();
    map.insert("key".into(), json!(spec.key));
    map.insert("label".into(), json!(spec.label));
    map.insert("usedPercent".into(), json!(used_pct));
    map.insert("remainingPercent".into(), json!(rem_pct));
    map.insert("resetAt".into(), json!(spec.reset.and_then(reset_at)));
    map.insert(
        "windowMinutes".into(),
        json!(spec.minutes.filter(|n| n.is_finite() && *n >= 0.0)),
    );
    map.insert(
        "used".into(),
        json!(spec.used.filter(|n| n.is_finite() && *n >= 0.0)),
    );
    map.insert(
        "limit".into(),
        json!(spec.limit.filter(|n| n.is_finite() && *n >= 0.0)),
    );
    map.insert("unit".into(), json!(spec.unit));
    if let Some(kind) = spec
        .kind
        .filter(|k| matches!(*k, "rate_limit" | "balance" | "spend" | "extra_usage"))
    {
        map.insert("kind".into(), json!(kind));
    }
    if let Some(rem) = spec.remaining {
        map.insert(
            "remaining".into(),
            json!((rem.is_finite() && rem >= 0.0).then_some(rem)),
        );
    }
    if let Some(exp) = spec.expires_at {
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

pub(crate) fn plan_account(provider: &str, platform: &str) -> Value {
    let label = aac_providers::provider(provider)
        .map(|row| row.label)
        .unwrap_or(provider);
    let host = match platform {
        "mac" => "Mac",
        "windows" => "Windows",
        _ => "Ubuntu",
    };
    json!({
        "id": format!("plan-{provider}-{platform}"),
        "provider": provider,
        "providerLabel": label,
        "label": label,
        "email": null,
        "plan": null,
        "platform": platform,
        "source": format!("Local credential store on {host}"),
        "status": "unavailable",
        "message": null,
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

fn valid_windows(windows: Vec<Value>) -> Result<Vec<Value>, CollectionError> {
    let result: Vec<Value> = windows
        .into_iter()
        .filter(|w| {
            [
                "usedPercent",
                "used",
                "limit",
                "remaining",
                "resetAt",
                "expiresAt",
                "unlimited",
                "enabled",
            ]
            .iter()
            .any(|k| w.get(*k).is_some_and(|v| !v.is_null()))
        })
        .collect();
    if result.is_empty() {
        return Err(CollectionError {
            status: "error",
            message: "The usage service returned no valid plan quota windows.",
            failure_code: None,
            retry_after: None,
        });
    }
    Ok(result)
}

pub(crate) fn parse_go(payload: &Value) -> Result<(Option<String>, Vec<Value>), CollectionError> {
    let Some(usage) = payload.get("usage").and_then(Value::as_object) else {
        return Err(CollectionError {
            status: "error",
            message: "The usage service returned an invalid plan quota response.",
            failure_code: None,
            retry_after: None,
        });
    };
    let mut windows = Vec::new();
    for (key, label, minutes) in [
        ("rolling", "5 hours", Some(300.0)),
        ("weekly", "Weekly", Some(10_080.0)),
        ("monthly", "Monthly", None),
    ] {
        let Some(item) = usage.get(key).and_then(Value::as_object) else {
            return Err(CollectionError {
                status: "error",
                message: "The usage service returned an incomplete plan quota response.",
                failure_code: None,
                retry_after: None,
            });
        };
        let status_ok = matches!(
            item.get("status").and_then(Value::as_str),
            Some("ok" | "rate-limited")
        );
        let Some(pct) = item.get("percent").and_then(api_number) else {
            return Err(CollectionError {
                status: "error",
                message: "The usage service returned an incomplete plan quota response.",
                failure_code: None,
                retry_after: None,
            });
        };
        if !status_ok {
            return Err(CollectionError {
                status: "error",
                message: "The usage service returned an incomplete plan quota response.",
                failure_code: None,
                retry_after: None,
            });
        }
        windows.push(usage_window(UsageWindowSpec {
            key,
            label,
            used_percent: Some(pct),
            reset: item.get("resetsAt"),
            minutes,
            kind: Some("rate_limit"),
            ..Default::default()
        }));
    }
    Ok((Some("Go".to_owned()), windows))
}

fn kimi_counts(detail: &Map<String, Value>) -> (Option<f64>, Option<f64>, Option<f64>) {
    let limit = detail.get("limit").and_then(api_number);
    let mut used = detail.get("used").and_then(api_number);
    let remaining = detail.get("remaining").and_then(api_number);
    if let Some(l) = limit.filter(|l| *l > 0.0) {
        if used.is_none()
            && let Some(r) = remaining.filter(|r| *r <= l)
        {
            used = Some(l - r);
        }
        if let Some(u) = used {
            let ratio = 100.0 * u / l;
            return (ratio.is_finite().then_some(ratio), Some(u), Some(l));
        }
    }
    (None, used, limit)
}

fn kimi_reset(detail: &Map<String, Value>) -> Option<&Value> {
    ["resetTime", "resetAt", "reset_time", "reset_at"]
        .into_iter()
        .filter_map(|k| detail.get(k))
        .find(|v| reset_at(v).is_some())
}

fn kimi_booster(payload: &Value) -> Vec<Value> {
    let Some(wallet) = payload.get("boosterWallet").and_then(Value::as_object) else {
        return Vec::new();
    };
    let monthly_limit = wallet.get("monthlyChargeLimit").and_then(Value::as_object);
    let monthly_used = wallet.get("monthlyUsed").and_then(Value::as_object);
    let currency = monthly_limit
        .and_then(|m| m.get("currency"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            monthly_used
                .and_then(|m| m.get("currency"))
                .and_then(Value::as_str)
        })
        .filter(|c| CURRENCY_RE.is_match(c));
    let scale = if currency.is_some() { 100.0 } else { 1.0 };
    let unit = currency.unwrap_or("cents");
    let mut result = Vec::new();
    if let Some(balance) = wallet.get("balance").and_then(Value::as_object)
        && balance.get("type").and_then(Value::as_str) == Some("BOOSTER")
    {
        let cents = |raw: Option<f64>| -> Option<f64> {
            let v = raw?.trunc() / 1_000_000.0;
            if v > 0.0 && v < 1.0 {
                Some(1.0)
            } else {
                Some((v + 0.5).floor())
            }
        };
        let total = cents(balance.get("amount").and_then(api_number));
        let left = cents(balance.get("amountLeft").and_then(api_number));
        if let Some(t) = total.filter(|t| *t > 0.0) {
            let used = left.filter(|l| *l <= t).map(|l| t - l);
            let ratio = used.map(|u| 100.0 * u / t);
            result.push(usage_window(UsageWindowSpec {
                key: "booster-balance",
                label: "Booster balance",
                used_percent: ratio,
                used: used.map(|u| u / scale),
                limit: Some(t / scale),
                remaining: left.map(|l| l / scale),
                unit: Some(unit),
                kind: Some("balance"),
                ..Default::default()
            }));
        }
    }
    let limit = monthly_limit
        .and_then(|m| m.get("priceInCents"))
        .and_then(api_number);
    let used = monthly_used
        .and_then(|m| m.get("priceInCents"))
        .and_then(api_number);
    let enabled = wallet
        .get("monthlyChargeLimitEnabled")
        .and_then(Value::as_bool);
    if limit.is_some() || used.is_some() || enabled.is_some() {
        let ratio = match (used, limit) {
            (Some(u), Some(l)) if l > 0.0 => Some(100.0 * u / l),
            _ => None,
        };
        let remaining = match (limit, used) {
            (Some(l), Some(u)) if u <= l => Some(l - u),
            _ => None,
        };
        result.push(usage_window(UsageWindowSpec {
            key: "booster-monthly-spend",
            label: "Monthly booster spend",
            used_percent: ratio,
            used: used.map(|u| u / scale),
            limit: limit.map(|l| l / scale),
            remaining: remaining.map(|r| r / scale),
            unit: Some(unit),
            kind: Some("spend"),
            enabled,
            ..Default::default()
        }));
    }
    result
}

pub(crate) fn parse_kimi(payload: &Value) -> Result<(Option<String>, Vec<Value>), CollectionError> {
    let mut windows: BTreeMap<String, Value> = BTreeMap::new();
    if let Some(usage) = payload.get("usage").and_then(Value::as_object) {
        let (ratio, used, limit) = kimi_counts(usage);
        windows.insert(
            "weekly".into(),
            usage_window(UsageWindowSpec {
                key: "weekly",
                label: "Weekly",
                used_percent: ratio,
                reset: kimi_reset(usage),
                minutes: Some(10_080.0),
                used,
                limit,
                kind: Some("rate_limit"),
                remaining: usage.get("remaining").and_then(api_number),
                ..Default::default()
            }),
        );
    }
    if let Some(limits) = payload.get("limits").and_then(Value::as_array) {
        for (index, item) in limits.iter().take(30).enumerate() {
            let Some(detail) = item.get("detail").and_then(Value::as_object) else {
                continue;
            };
            let Some(window) = item.get("window").and_then(Value::as_object) else {
                continue;
            };
            let duration = window.get("duration").and_then(api_number);
            let mult = match window.get("timeUnit").and_then(Value::as_str) {
                Some("TIME_UNIT_MINUTE") => Some(1.0),
                Some("TIME_UNIT_HOUR") => Some(60.0),
                Some("TIME_UNIT_DAY") => Some(1440.0),
                Some("TIME_UNIT_WEEK") => Some(10_080.0),
                Some("TIME_UNIT_SECOND") => Some(1.0 / 60.0),
                _ => None,
            };
            let minutes = duration.zip(mult).map(|(d, m)| d * m);
            let (key, label) = match minutes {
                Some(300.0) => ("5h".to_owned(), "5 hours".to_owned()),
                Some(10_080.0) => ("weekly".to_owned(), "Weekly".to_owned()),
                _ => (
                    format!("limit-{index}"),
                    format!("Plan limit {}", index + 1),
                ),
            };
            let (ratio, used, limit) = kimi_counts(detail);
            let reset = kimi_reset(detail);
            if ratio.is_some() || reset.is_some() || used.is_some() || limit.is_some() {
                windows.insert(
                    key.clone(),
                    usage_window(UsageWindowSpec {
                        key: &key,
                        label: &label,
                        used_percent: ratio,
                        reset,
                        minutes,
                        used,
                        limit,
                        kind: Some("rate_limit"),
                        remaining: detail.get("remaining").and_then(api_number),
                        ..Default::default()
                    }),
                );
            }
        }
    }
    if let Some(usages) = payload.get("usages").and_then(Value::as_object) {
        for (upstream, key, label, minutes) in [
            ("limit_5h", "5h", "5 hours", Some(300.0)),
            ("limit_7d", "weekly", "Weekly", Some(10_080.0)),
            ("limit_month_total", "monthly", "Monthly total", None),
            ("limit_month_code", "monthly-code", "Monthly coding", None),
        ] {
            let Some(item) = usages.get(upstream).and_then(Value::as_object) else {
                continue;
            };
            let ratio = item.get("used_ratio").and_then(api_number);
            let modern_reset = item.get("reset_time").and_then(reset_at);
            if ratio == Some(0.0)
                && !usages
                    .get("limit_month_total")
                    .is_some_and(Value::is_object)
                && let Some(legacy) = windows.get(key)
                && legacy
                    .get("used")
                    .and_then(Value::as_f64)
                    .is_some_and(|u| u > 0.0)
                && legacy.get("windowMinutes").and_then(Value::as_f64) == minutes
                && let (Some(mr), Some(lr)) = (
                    modern_reset.as_deref().and_then(parse_iso_utc),
                    legacy
                        .get("resetAt")
                        .and_then(Value::as_str)
                        .and_then(parse_iso_utc),
                )
                && (mr.timestamp() - lr.timestamp()).abs() <= 2
            {
                continue;
            }
            if ratio.is_some_and(|r| r <= 1.0) || modern_reset.is_some() {
                windows.insert(
                    key.to_owned(),
                    usage_window(UsageWindowSpec {
                        key,
                        label,
                        used_percent: ratio.filter(|r| *r <= 1.0).map(|r| r * 100.0),
                        reset: item.get("reset_time"),
                        minutes,
                        kind: Some("rate_limit"),
                        ..Default::default()
                    }),
                );
            }
        }
    }
    let mut ordered: Vec<Value> = windows.into_values().collect();
    ordered.sort_by(|a, b| {
        let ma = a.get("windowMinutes").and_then(Value::as_f64);
        let mb = b.get("windowMinutes").and_then(Value::as_f64);
        ma.is_none()
            .cmp(&mb.is_none())
            .then_with(|| ma.unwrap_or(0.0).total_cmp(&mb.unwrap_or(0.0)))
            .then_with(|| {
                a.get("key")
                    .and_then(Value::as_str)
                    .cmp(&b.get("key").and_then(Value::as_str))
            })
    });
    ordered.extend(kimi_booster(payload));
    let level = payload
        .get("user")
        .and_then(|u| u.get("membership"))
        .and_then(|m| m.get("level"))
        .and_then(Value::as_str);
    let legacy_version = payload.get("version").is_none()
        || payload.get("version") == Some(&Value::Null)
        || payload.get("version").and_then(Value::as_str) == Some("GOODS_VERSION_V1");
    let mut plan = match (legacy_version, level) {
        (true, Some("LEVEL_FREE")) => Some("Adagio".to_owned()),
        (true, Some("LEVEL_TRIAL")) => Some("Andante".to_owned()),
        (true, Some("LEVEL_BASIC")) => Some("Moderato".to_owned()),
        (true, Some("LEVEL_INTERMEDIATE")) => Some("Allegretto".to_owned()),
        (true, Some("LEVEL_ADVANCED")) => Some("Allegro".to_owned()),
        (false, Some("LEVEL_FREE")) => Some("Free".to_owned()),
        (false, Some("LEVEL_TRIAL")) => Some("Trial".to_owned()),
        (false, Some("LEVEL_BASIC")) => Some("Basic".to_owned()),
        (false, Some("LEVEL_INTERMEDIATE")) => Some("Intermediate".to_owned()),
        (false, Some("LEVEL_ADVANCED")) => Some("Advanced".to_owned()),
        _ => None,
    };
    if plan.is_none()
        && let Some(lvl) = level
        && KIMI_LEVEL_RE.is_match(lvl)
    {
        let words: Vec<String> = lvl[6..]
            .split('_')
            .map(|w| format!("{}{}", &w[..1], w[1..].to_ascii_lowercase()))
            .collect();
        plan = Some(words.join(" "));
    }
    Ok((plan, valid_windows(ordered)?))
}

fn py_format_opt_num(v: Option<f64>) -> String {
    match v {
        Some(n) => aac_num::py::repr(n),
        None => "None".to_owned(),
    }
}

fn py_format_unit(v: Option<&Value>) -> String {
    match v {
        None | Some(Value::Null) => "None".to_owned(),
        Some(Value::Number(n)) => {
            if let Some(i) = n.as_i64() {
                i.to_string()
            } else if let Some(u) = n.as_u64() {
                u.to_string()
            } else if let Some(f) = n.as_f64() {
                aac_num::py::repr(f)
            } else {
                "None".to_owned()
            }
        }
        Some(Value::String(s)) => s.clone(),
        Some(Value::Bool(true)) => "True".to_owned(),
        Some(Value::Bool(false)) => "False".to_owned(),
        Some(other) => other.to_string(),
    }
}

fn py_format_g(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        let s = format!("{n:.6}");
        s.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

pub(crate) fn parse_zai(payload: &Value) -> Result<(Option<String>, Vec<Value>), CollectionError> {
    let bad_code = match payload.get("code") {
        None | Some(Value::Null) => false,
        Some(Value::Number(n)) => !matches!(n.as_i64(), Some(0 | 200)),
        Some(Value::String(s)) => !matches!(s.as_str(), "0" | "200"),
        _ => true,
    };
    if payload.get("success") == Some(&Value::Bool(false)) || bad_code {
        return Err(CollectionError {
            status: "error",
            message: "The usage service could not return this coding plan.",
            failure_code: None,
            retry_after: None,
        });
    }
    let Some(data) = payload.get("data").and_then(Value::as_object) else {
        return Err(CollectionError {
            status: "error",
            message: "The usage service returned an invalid plan quota response.",
            failure_code: None,
            retry_after: None,
        });
    };
    let Some(limits) = data.get("limits").and_then(Value::as_array) else {
        return Err(CollectionError {
            status: "error",
            message: "The usage service returned an invalid plan quota response.",
            failure_code: None,
            retry_after: None,
        });
    };
    let mut windows = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, item) in limits.iter().take(30).enumerate() {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let Some(meter_type) = obj.get("type").and_then(Value::as_str) else {
            continue;
        };
        let kind_label = match meter_type {
            "TIME_LIMIT" => "Requests",
            "TOKENS_LIMIT" => "Tokens",
            "CREDIT_LIMIT" => "Credits",
            _ => continue,
        };
        let count = obj.get("number").and_then(api_number);
        let unit_val = obj.get("unit");
        let unit_int = unit_val.and_then(|v| (!v.is_boolean()).then(|| v.as_i64()).flatten());
        let monthly_requests =
            meter_type == "TIME_LIMIT" && unit_int == Some(5) && count == Some(1.0);
        let mult = match unit_int {
            Some(1) => Some(1440.0),
            Some(3) => Some(60.0),
            Some(6) => Some(10_080.0),
            _ => None,
        };
        let minutes = if monthly_requests {
            None
        } else {
            count.zip(mult).map(|(c, m)| c * m)
        };
        let duration_label = match unit_int {
            Some(1) => Some("days"),
            Some(3) => Some("hours"),
            Some(6) => Some("weeks"),
            _ => None,
        };
        let duration = if minutes == Some(300.0) {
            "5 hours".to_owned()
        } else if minutes == Some(10_080.0) {
            "Weekly".to_owned()
        } else if monthly_requests {
            "Monthly".to_owned()
        } else if let (Some(c), Some(dl)) = (count, duration_label) {
            format!("{} {dl}", py_format_g(c))
        } else {
            "Plan limit".to_owned()
        };
        let meter_prefix = meter_type.to_ascii_lowercase().replace("_limit", "");
        let mut key = format!(
            "{meter_prefix}-{}-{}",
            py_format_unit(unit_val),
            py_format_opt_num(count)
        );
        if !seen.insert(key.clone()) {
            key = format!("{key}-{index}");
            seen.insert(key.clone());
        }
        let used = obj.get("currentValue").and_then(api_number);
        let limit = obj.get("usage").and_then(api_number);
        let ratio = match (used, limit) {
            (Some(u), Some(l)) if l > 0.0 => Some(100.0 * u / l),
            _ => obj.get("percentage").and_then(api_number),
        };
        let unit_name = if meter_type == "TIME_LIMIT" {
            "requests"
        } else if meter_type == "TOKENS_LIMIT" {
            "tokens"
        } else {
            "credits"
        };
        let label = format!("{duration} \u{00b7} {kind_label}");
        windows.push(usage_window(UsageWindowSpec {
            key: &key,
            label: &label,
            used_percent: ratio,
            reset: obj.get("nextResetTime"),
            minutes,
            used,
            limit,
            unit: Some(unit_name),
            kind: Some("rate_limit"),
            remaining: obj.get("remaining").and_then(api_number),
            ..Default::default()
        }));
        if meter_type == "TIME_LIMIT"
            && let Some(details) = obj.get("usageDetails").and_then(Value::as_array)
        {
            for detail in details.iter().take(30) {
                let Some(d) = detail.as_object() else {
                    continue;
                };
                if d.get("modelCode")
                    .is_some_and(|v| v.is_array() || v.is_object())
                {
                    return Err(CollectionError {
                        status: "error",
                        message: "The usage service returned an invalid plan quota response.",
                        failure_code: None,
                        retry_after: None,
                    });
                }
                let Some(code) = d.get("modelCode").and_then(Value::as_str) else {
                    continue;
                };
                let d_label = match code {
                    "search-prime" => "Search",
                    "web-reader" => "Web reader",
                    "zread" => "Zread",
                    _ => continue,
                };
                let Some(d_used) = d.get("usage").and_then(api_number) else {
                    continue;
                };
                let d_key = format!("{key}-{code}");
                windows.push(usage_window(UsageWindowSpec {
                    key: &d_key,
                    label: d_label,
                    used: Some(d_used),
                    unit: Some("requests"),
                    kind: Some("extra_usage"),
                    ..Default::default()
                }));
            }
        }
    }
    let plan = data.get("level").and_then(plan_label);
    Ok((plan, valid_windows(windows)?))
}

fn zai_pack_expiry(value: Option<&Value>) -> Option<String> {
    let val = value?;
    if let Some(s) = val.as_str()
        && ZAI_CN_TIME_RE.is_match(s)
    {
        let converted = format!("{}T{}+08:00", &s[..10], &s[11..]);
        return reset_at(&Value::String(converted));
    }
    reset_at(val)
}

pub(crate) fn parse_zai_reset_packs(
    payload: &Value,
    now_secs: Option<f64>,
) -> Result<Vec<Value>, CollectionError> {
    let err = || CollectionError {
        status: "error",
        message: "Reset pack inventory could not be read.",
        failure_code: None,
        retry_after: None,
    };
    let bad_code = match payload.get("code") {
        None | Some(Value::Null) => false,
        Some(Value::Number(n)) => !matches!(n.as_i64(), Some(0 | 200)),
        Some(Value::String(s)) => !matches!(s.as_str(), "0" | "200"),
        _ => true,
    };
    if payload.get("success") != Some(&Value::Bool(true)) || bad_code {
        return Err(err());
    }
    let Some(data) = payload.get("data").and_then(Value::as_object) else {
        return Err(err());
    };
    let current = crate::failover_additional::desktop::utc_now_iso_secs(now_secs);
    let mut windows = Vec::new();
    let mut recognized = false;
    for (field, prefix, label) in [
        ("fiveHourResets", "5h", "5-hour"),
        ("weekResets", "weekly", "Weekly"),
    ] {
        let Some(rows) = data.get(field).and_then(Value::as_array) else {
            continue;
        };
        recognized = true;
        let mut left: u64 = 0;
        let mut used: u64 = 0;
        let mut expiries: Vec<String> = Vec::new();
        let mut complete = rows.len() <= 100;
        for row in rows.iter().take(100) {
            let Some(obj) = row.as_object() else {
                complete = false;
                continue;
            };
            let expiry = zai_pack_expiry(obj.get("expireTime"));
            let in_date = expiry.as_deref().is_none_or(|e| e > current.as_str());
            match obj.get("available").and_then(Value::as_bool) {
                None => {
                    complete = false;
                    if let Some(e) = expiry.filter(|_| in_date) {
                        expiries.push(e);
                    }
                }
                Some(true) if in_date => {
                    left += 1;
                    if let Some(e) = expiry {
                        expiries.push(e);
                    }
                }
                Some(false) if in_date && expiry.is_some() => {
                    used += 1;
                }
                _ => {}
            }
        }
        let min_expiry = expiries.into_iter().min().map(Value::String);
        let key = format!("reset-packs-{prefix}");
        let wlabel = format!("{label} reset packs");
        windows.push(usage_window(UsageWindowSpec {
            key: &key,
            label: &wlabel,
            used: complete.then_some(used as f64),
            limit: complete.then_some((left + used) as f64),
            remaining: complete.then_some(left as f64),
            expires_at: min_expiry.as_ref(),
            unit: Some("packs"),
            kind: Some("balance"),
            ..Default::default()
        }));
    }
    if !recognized {
        return Err(err());
    }
    Ok(windows)
}

pub(crate) fn qwen_credential(secret: &str) -> Result<(String, &'static str), CollectionError> {
    let packed = serde_json::from_str::<Value>(secret).ok();
    let Some(obj) = packed.as_ref().and_then(Value::as_object) else {
        return Err(CollectionError {
            status: "unavailable",
            message: "The saved Qwen API key has no console session; live token-plan usage requires its existing console cookie.",
            failure_code: None,
            retry_after: None,
        });
    };
    let Some(cookie) = obj
        .get("cookie")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
    else {
        return Err(CollectionError {
            status: "unavailable",
            message: "The saved Qwen API key has no console session; live token-plan usage requires its existing console cookie.",
            failure_code: None,
            retry_after: None,
        });
    };
    if cookie.chars().count() > 32_768
        || cookie.contains('\r')
        || cookie.contains('\n')
        || cookie.contains('\0')
    {
        return Err(CollectionError {
            status: "unavailable",
            message: "The saved Qwen console session is invalid.",
            failure_code: None,
            retry_after: None,
        });
    }
    let base = match obj.get("baseUrl") {
        None => "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1",
        Some(v) => v.as_str().ok_or(CollectionError {
            status: "unavailable",
            message: "The saved Qwen token-plan region is unsupported.",
            failure_code: None,
            retry_after: None,
        })?,
    };
    let region = match base.trim_end_matches('/') {
        "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1" => "intl",
        "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1" => "cn",
        _ => {
            return Err(CollectionError {
                status: "unavailable",
                message: "The saved Qwen token-plan region is unsupported.",
                failure_code: None,
                retry_after: None,
            });
        }
    };
    Ok((cookie.to_owned(), region))
}

pub(crate) fn qwen_gateway_cookie(
    secret: &str,
    console_cookie: &str,
) -> Result<String, CollectionError> {
    let packed: Value = serde_json::from_str(secret).map_err(|_| CollectionError {
        status: "unavailable",
        message: "The saved Qwen gateway session is invalid.",
        failure_code: None,
        retry_after: None,
    })?;
    let cookie = match packed.get("gatewayCookie") {
        None => console_cookie,
        Some(v) => v.as_str().ok_or(CollectionError {
            status: "unavailable",
            message: "The saved Qwen gateway session is invalid.",
            failure_code: None,
            retry_after: None,
        })?,
    };
    if cookie.chars().count() > 32_768
        || cookie.contains('\r')
        || cookie.contains('\n')
        || cookie.contains('\0')
    {
        return Err(CollectionError {
            status: "unavailable",
            message: "The saved Qwen gateway session is invalid.",
            failure_code: None,
            retry_after: None,
        });
    }
    Ok(cookie.to_owned())
}

fn percent_decode(raw: &str) -> String {
    let bytes = raw.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push((hi << 4) | lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

fn urlencode_pairs(pairs: &[(&str, &str)]) -> String {
    let mut serializer = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        serializer.append_pair(k, v);
    }
    serializer.finish()
}

fn unwrap_qwen(payload: &Value, markers: &[&str]) -> Result<Value, CollectionError> {
    let mut cur = payload.clone();
    for _ in 0..7 {
        let Some(obj) = cur.as_object() else {
            break;
        };
        if obj.get("successResponse") == Some(&Value::Bool(false))
            || obj.get("success") == Some(&Value::Bool(false))
        {
            return Err(CollectionError {
                status: "needs_sign_in",
                message: "The Qwen console session could not access token-plan usage.",
                failure_code: None,
                retry_after: None,
            });
        }
        if markers.iter().any(|k| obj.contains_key(*k)) {
            return Ok(cur);
        }
        let next = obj
            .get("DataV2")
            .or_else(|| obj.get("Data"))
            .or_else(|| obj.get("data"))
            .cloned()
            .unwrap_or(Value::Null);
        cur = if let Some(s) = next.as_str() {
            match serde_json::from_str::<Value>(s) {
                Ok(v) => v,
                Err(_) => break,
            }
        } else {
            next
        };
    }
    Err(CollectionError {
        status: "error",
        message: "The Qwen console returned no valid token-plan quota response.",
        failure_code: None,
        retry_after: None,
    })
}

pub(crate) async fn fetch_qwen(
    client: &dyn PlanTransport,
    secret: &str,
) -> Result<(Value, Option<Value>, Option<Value>, Option<Value>), CollectionError> {
    let (cookie, region) = qwen_credential(secret)?;
    let gateway_cookie = qwen_gateway_cookie(secret, &cookie)?;
    let (origin, referer, sec_token, cornerstone, gateway, action, cloud_region) =
        if region == "intl" {
            let origin = "https://home.qwencloud.com";
            let referer = "https://home.qwencloud.com/analytics/token-plan/individual";
            let info = match client
                .request(
                    "https://home.qwencloud.com/tool/user/info.json",
                    &[("Cookie".into(), cookie.clone())],
                    None,
                    false,
                )
                .await?
            {
                PlanHttpBody::Json(v) => v,
                PlanHttpBody::Text(_) => Value::Null,
            };
            let tok = info
                .get("data")
                .and_then(|d| d.get("secToken"))
                .and_then(Value::as_str)
                .map(str::to_owned);
            let cs = json!({
                "domain": "home.qwencloud.com",
                "consoleSite": "QWENCLOUD",
                "console": "ONE_CONSOLE",
                "xsp_lang": "en-US",
                "protocol": "V2",
                "productCode": "p_efm"
            });
            (
                origin,
                referer,
                tok,
                cs,
                "https://cs-data.qwencloud.com/data/api.json",
                "IntlBroadScopeAspnGateway",
                "ap-southeast-1",
            )
        } else {
            let origin = "https://bailian.console.aliyun.com";
            let referer = "https://bailian.console.aliyun.com/cn-beijing?tab=plan";
            let page = match client
                .request(
                    referer,
                    &[
                        ("Cookie".into(), cookie.clone()),
                        ("Accept".into(), "text/html".into()),
                    ],
                    None,
                    true,
                )
                .await?
            {
                PlanHttpBody::Text(s) => s,
                PlanHttpBody::Json(v) => v.to_string(),
            };
            let tok = SEC_TOKEN_RE
                .captures(&page)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_owned());
            let trace = sha256_hex(format!("{referer}:{}", now_unix_secs(None)));
            let fe_trace_id = format!(
                "{}-{}-{}-{}-{}",
                &trace[0..8],
                &trace[8..12],
                &trace[12..16],
                &trace[16..20],
                &trace[20..32]
            );
            let cs = json!({
                "feTraceId": fe_trace_id,
                "feURL": format!("{referer}#/efm/subscription/token-plan/personal"),
                "protocol": "V2",
                "console": "ONE_CONSOLE",
                "productCode": "p_efm",
                "switchUserType": 3,
                "domain": "bailian.console.aliyun.com",
                "consoleSite": "BAILIAN_ALIYUN",
                "userNickName": "",
                "userPrincipalName": "",
                "xsp_lang": "zh-CN"
            });
            (
                origin,
                referer,
                tok,
                cs,
                "https://bailian-cs.console.aliyun.com/data/api.json",
                "BroadScopeAspnGateway",
                "cn-beijing",
            )
        };
    let Some(sec_token) = sec_token.filter(|s| !s.is_empty() && s.len() <= 16_384) else {
        return Err(CollectionError {
            status: "needs_sign_in",
            message: "The saved Qwen console session has expired or is incomplete.",
            failure_code: None,
            retry_after: None,
        });
    };
    let mut headers = vec![
        (
            "Content-Type".to_owned(),
            "application/x-www-form-urlencoded".to_owned(),
        ),
        ("Origin".to_owned(), origin.to_owned()),
        ("Referer".to_owned(), referer.to_owned()),
        ("X-Requested-With".to_owned(), "XMLHttpRequest".to_owned()),
    ];
    if !gateway_cookie.is_empty() {
        headers.push(("Cookie".to_owned(), gateway_cookie.clone()));
    }
    for part in gateway_cookie.split(';') {
        if let Some((name, val)) = part.trim().split_once('=')
            && matches!(name, "login_aliyunid_csrf" | "csrf")
            && !val.contains('\r')
            && !val.contains('\n')
        {
            let decoded = percent_decode(val);
            if decoded.contains('\r') || decoded.contains('\n') || decoded.contains('\0') {
                return Err(CollectionError {
                    status: "unavailable",
                    message: "The saved Qwen console session is invalid.",
                    failure_code: None,
                    retry_after: None,
                });
            }
            headers.push(("x-xsrf-token".to_owned(), decoded.clone()));
            headers.push(("x-csrf-token".to_owned(), decoded));
            break;
        }
    }
    let call = |suffix: &'static str, extra: Option<Map<String, Value>>| {
        let endpoint = format!("zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/{suffix}");
        let mut data = Map::new();
        data.insert("cornerstoneParam".into(), cornerstone.clone());
        if let Some(ex) = extra {
            data.extend(ex);
        }
        let params =
            json!({ "Api": endpoint, "Data": Value::Object(data), "V": "1.0" }).to_string();
        let body = urlencode_pairs(&[
            ("product", "sfm_bailian"),
            ("action", action),
            ("region", cloud_region),
            ("sec_token", &sec_token),
            ("params", &params),
        ]);
        let query = urlencode_pairs(&[
            ("product", "sfm_bailian"),
            ("action", action),
            ("api", &endpoint),
        ]);
        let url = format!("{gateway}?{query}");
        let hdrs = headers.clone();
        async move {
            match client
                .request(&url, &hdrs, Some(body.as_bytes()), false)
                .await?
            {
                PlanHttpBody::Json(v) => Ok(v),
                PlanHttpBody::Text(_) => Err(CollectionError {
                    status: "error",
                    message: "The usage service is unavailable or returned an invalid response.",
                    failure_code: None,
                    retry_after: None,
                }),
            }
        }
    };

    let usage = call("usage", None).await?;
    let mut addon_extra = Map::new();
    addon_extra.insert(
        "commodityCode".into(),
        json!(format!(
            "sfm_tokenplansoloaddon_public_{}",
            if region == "intl" { "intl" } else { "cn" }
        )),
    );
    addon_extra.insert("status".into(), json!(["ACTIVE"]));
    addon_extra.insert("pageNum".into(), json!(1));
    addon_extra.insert("pageSize".into(), json!(10));
    let addon = call("addon/list", Some(addon_extra)).await.ok();

    let sub_extra = if region == "intl" {
        let mut m = Map::new();
        m.insert(
            "commodityCode".into(),
            json!("sfm_tokenplansolo_public_intl"),
        );
        Some(m)
    } else {
        None
    };
    let subscription = call("subscription", sub_extra).await.ok();
    let quota_config = call("quota-config", None).await.ok();
    Ok((usage, subscription, quota_config, addon))
}

pub(crate) fn parse_qwen(
    payload: &Value,
    subscription: Option<&Value>,
    quota_config: Option<&Value>,
    addon: Option<&Value>,
) -> Result<(Option<String>, Vec<Value>), CollectionError> {
    let data = unwrap_qwen(
        payload,
        &[
            "per5HourPercentage",
            "per1WeekPercentage",
            "per1MonthPercentage",
            "per5HourResetTime",
            "per1WeekResetTime",
            "per1MonthResetTime",
        ],
    )?;
    let subscription_data = subscription.and_then(|s| {
        unwrap_qwen(
            s,
            &["specCode", "spec_code", "planName", "plan_name", "endTime"],
        )
        .ok()
    });
    let tier = subscription_data
        .as_ref()
        .and_then(|s| s.get("specCode").or_else(|| s.get("spec_code")))
        .and_then(Value::as_str);
    let quotas = tier
        .and_then(|t| {
            quota_config
                .and_then(|qc| unwrap_qwen(qc, &[t]).ok())
                .and_then(|configs| configs.get(t).cloned())
        })
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    let mut windows = Vec::new();
    for (prefix, key, label, minutes, limit_keys) in [
        (
            "per5Hour",
            "5h",
            "5 hours",
            Some(300.0),
            &["five_hour", "fiveHour"][..],
        ),
        (
            "per1Week",
            "weekly",
            "Weekly",
            Some(10_080.0),
            &["weekly"][..],
        ),
        ("per1Month", "monthly", "Monthly", None, &["monthly"][..]),
    ] {
        let ratio = data.get(format!("{prefix}Percentage")).and_then(api_number);
        let reset_val = data.get(format!("{prefix}ResetTime"));
        if ratio.is_none() && reset_val.and_then(reset_at).is_none() {
            continue;
        }
        let used_percent = ratio.map(|r| if r <= 1.0 { r * 100.0 } else { r });
        let limit = limit_keys
            .iter()
            .find_map(|k| quotas.get(*k).and_then(api_number));
        let used = limit.zip(used_percent).map(|(l, p)| l * p / 100.0);
        let remaining = limit.zip(used).map(|(l, u)| l - u);
        windows.push(usage_window(UsageWindowSpec {
            key,
            label,
            used_percent,
            reset: reset_val,
            minutes,
            used,
            limit,
            remaining,
            unit: limit.map(|_| "credits"),
            kind: Some("rate_limit"),
            ..Default::default()
        }));
    }
    let mut plan = None;
    if let Some(sub) = subscription_data.as_ref().and_then(Value::as_object) {
        plan = sub
            .get("planName")
            .or_else(|| sub.get("plan_name"))
            .and_then(plan_label)
            .or_else(|| tier.and_then(|t| plan_label(&Value::String(t.to_owned()))));
        if let Some(expiry) = sub.get("endTime").filter(|v| reset_at(v).is_some()) {
            windows.push(usage_window(UsageWindowSpec {
                key: "subscription",
                label: "Plan subscription",
                expires_at: Some(expiry),
                kind: Some("rate_limit"),
                ..Default::default()
            }));
        }
    }
    if let Some(extras) = addon
        .and_then(|a| {
            unwrap_qwen(
                a,
                &["remainingCredits", "totalCredits", "activeCount", "items"],
            )
            .ok()
        })
        .and_then(|v| v.as_object().cloned())
    {
        let limit = extras.get("totalCredits").and_then(api_number);
        let remaining = extras.get("remainingCredits").and_then(api_number);
        let used = match (limit, remaining) {
            (Some(l), Some(r)) if r <= l => Some(l - r),
            _ => None,
        };
        let ratio = match (used, limit) {
            (Some(u), Some(l)) if l > 0.0 => Some(100.0 * u / l),
            _ => None,
        };
        if limit.is_some() || remaining.is_some() {
            windows.push(usage_window(UsageWindowSpec {
                key: "addon-credits",
                label: "Additional credits",
                used_percent: ratio,
                used,
                limit,
                remaining,
                unit: Some("credits"),
                kind: Some("balance"),
                ..Default::default()
            }));
        }
        if let Some(active) = extras.get("activeCount").and_then(api_number) {
            windows.push(usage_window(UsageWindowSpec {
                key: "addon-packs",
                label: "Active credit packs",
                remaining: Some(active),
                unit: Some("packs"),
                kind: Some("balance"),
                ..Default::default()
            }));
        }
        if let Some(items) = extras.get("items").and_then(Value::as_array) {
            for (index, item) in items.iter().take(100).enumerate() {
                let Some(obj) = item.as_object() else {
                    continue;
                };
                let total = obj.get("totalCredits").and_then(api_number);
                let rem = obj.get("remainingCredits").and_then(api_number);
                let exp = obj.get("endTime").and_then(reset_at);
                if total.is_none() && rem.is_none() && exp.is_none() {
                    continue;
                }
                let spent = match (total, rem) {
                    (Some(t), Some(r)) if r <= t => Some(t - r),
                    _ => None,
                };
                let ratio = match (spent, total) {
                    (Some(s), Some(t)) if t > 0.0 => Some(s / t * 100.0),
                    _ => None,
                };
                let identifier = match obj.get("orderId") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => index.to_string(),
                };
                let key = format!("addon-pack-{}", &sha256_hex(identifier.as_bytes())[..12]);
                let label = format!("Additional credit pack {}", index + 1);
                let exp_val = exp.map(Value::String);
                windows.push(usage_window(UsageWindowSpec {
                    key: &key,
                    label: &label,
                    used_percent: ratio,
                    used: spent,
                    limit: total,
                    remaining: rem,
                    expires_at: exp_val.as_ref(),
                    unit: Some("credits"),
                    kind: Some("balance"),
                    ..Default::default()
                }));
            }
            if items.iter().all(|item| {
                item.as_object().is_some_and(|obj| {
                    obj.get("totalCredits").and_then(api_number).is_some()
                        || obj.get("remainingCredits").and_then(api_number).is_some()
                        || obj.get("endTime").and_then(reset_at).is_some()
                })
            }) {
                windows.push(usage_window(UsageWindowSpec {
                    key: "addon-listed-packs",
                    label: "Listed active credit packs",
                    remaining: Some(items.len() as f64),
                    unit: Some("packs"),
                    kind: Some("balance"),
                    ..Default::default()
                }));
            }
        }
    }
    Ok((plan, valid_windows(windows)?))
}

fn load_plan_json(path: &Path, tracker: &mut ReadTracker) -> Option<Value> {
    let policy = ReadPolicy::capped(MAX_BYTES as u64);
    match aac_files::read::read(path, &policy) {
        Read::Absent | Read::Unreadable(ErrorKind::NotADirectory) => None,
        Read::Bytes(bytes, _) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(v) => Some(v),
            Err(_) => {
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

fn read_private_key_bytes(path: &Path, limit: usize, tracker: &mut ReadTracker) -> Option<Vec<u8>> {
    let policy = ReadPolicy::capped(limit as u64).mode(Mode::Private);
    match aac_files::read::read(path, &policy) {
        Read::Absent | Read::Unreadable(ErrorKind::NotADirectory) => None,
        Read::Bytes(bytes, _) => Some(bytes),
        _ => {
            tracker.record_failure();
            None
        }
    }
}

pub(crate) fn key_fingerprint(secret: &str) -> String {
    format!("sha256:{}", &sha256_hex(secret.as_bytes())[..16])
}

/// Read one AAC-owned key store record (`plan_common.aac_key_credential`).
pub(crate) fn aac_key_credential(
    provider: &str,
    key_id: &str,
    home: &Path,
    ccs_dir: Option<&Path>,
    force_plaintext: Option<bool>,
    dpapi: Option<&DpapiUnprotect>,
    tracker: &mut ReadTracker,
) -> Option<Value> {
    if !aac_providers::provider(provider).is_some_and(|row| row.key_store)
        || !KEY_ID_RE.is_match(key_id)
    {
        return None;
    }
    let root = ccs_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".ccs"));
    let directory = root.join("account-usage/keys");
    let info = match fs::symlink_metadata(&directory) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return None;
        }
        Err(_) => {
            tracker.record_failure();
            return None;
        }
    };
    if info.file_type().is_symlink() || !info.is_dir() {
        tracker.record_failure();
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if info.mode() & 0o077 != 0 {
            tracker.record_failure();
            return None;
        }
    }
    let windows_store = match force_plaintext {
        Some(true) => false,
        Some(false) => true,
        None => cfg!(windows),
    };
    let key_file = directory.join(format!(
        "{provider}-{key_id}.{}",
        if windows_store { "dpapi" } else { "json" }
    ));
    let text = if windows_store {
        let raw = read_private_key_bytes(&key_file, MAX_KEY_FILE_BYTES * 4, tracker);
        raw.and_then(|bytes| dpapi.and_then(|f| f(&bytes, Some(KEY_ENTROPY))))
    } else {
        read_private_key_bytes(&key_file, MAX_KEY_FILE_BYTES, tracker)
            .and_then(|bytes| String::from_utf8(bytes).ok())
    };
    let record = text
        .filter(|t| t.chars().count() <= MAX_KEY_FILE_BYTES)
        .and_then(|t| serde_json::from_str::<Value>(&t).ok());
    let valid = record.as_ref().and_then(|val| {
        let obj = val.as_object()?;
        if obj.keys().any(|k| {
            !matches!(
                k.as_str(),
                "version" | "provider" | "keyId" | "secret" | "fingerprint" | "last4" | "createdAt"
            )
        }) {
            return None;
        }
        if obj.get("version").and_then(Value::as_i64) != Some(1)
            || obj.get("provider").and_then(Value::as_str) != Some(provider)
            || obj.get("keyId").and_then(Value::as_str) != Some(key_id)
        {
            return None;
        }
        let secret = obj
            .get("secret")
            .and_then(Value::as_str)
            .filter(|s| KEY_SECRET_RE.is_match(s))?;
        if let Some(fp) = obj.get("fingerprint")
            && fp.as_str() != Some(key_fingerprint(secret).as_str())
        {
            return None;
        }
        Some(json!({
            "secret": secret,
            "source": "Dashboard key",
            "email": null,
            "expires": null
        }))
    });
    if valid.is_none() && fs::symlink_metadata(&key_file).is_ok() {
        tracker.record_failure();
    }
    valid
}

/// Read one Qwen browser console capsule (`plan_common._qwen_console_capsule`).
pub(crate) fn qwen_console_capsule(
    home: &Path,
    capsule_id: &str,
    ccs_dir: Option<&Path>,
    dpapi: Option<&DpapiUnprotect>,
    tracker: &mut ReadTracker,
) -> Option<Value> {
    if !CAPSULE_ID_RE.is_match(capsule_id) {
        return None;
    }
    let name = if capsule_id == "default" {
        "qwen-console-session.json".to_owned()
    } else {
        format!("qwen-console-session-{capsule_id}.json")
    };
    let root = ccs_dir
        .map(Path::to_path_buf)
        .unwrap_or_else(|| home.join(".ccs"));
    let path = root.join("account-usage").join(name);
    let parsed = read_qwen_console_capsule(&path, dpapi, tracker);
    if parsed.is_none() && fs::symlink_metadata(&path).is_ok() {
        tracker.record_failure();
    }
    let (cookie, region, gateway_cookie) = parsed?;
    let mut packed = Map::new();
    packed.insert("cookie".into(), json!(cookie));
    packed.insert(
        "baseUrl".into(),
        json!(format!(
            "https://token-plan.{}.maas.aliyuncs.com/compatible-mode/v1",
            if region == "intl" {
                "ap-southeast-1"
            } else {
                "cn-beijing"
            }
        )),
    );
    if let Some(gw) = gateway_cookie {
        packed.insert("gatewayCookie".into(), json!(gw));
    }
    Some(json!({
        "secret": Value::Object(packed).to_string(),
        "source": "Brave console",
        "email": null,
        "expires": null
    }))
}

fn read_qwen_console_capsule(
    path: &Path,
    dpapi: Option<&DpapiUnprotect>,
    tracker: &mut ReadTracker,
) -> Option<(String, &'static str, Option<String>)> {
    let capsule = load_plan_json(path, tracker)?;
    let obj = capsule.as_object()?;
    let region = match obj.get("region").and_then(Value::as_str)? {
        "intl" => "intl",
        "cn" => "cn",
        _ => return None,
    };
    let version2 = obj.get("version").and_then(Value::as_i64) == Some(2);
    let encoded = obj
        .get(if version2 {
            "cookiesDPAPI"
        } else {
            "cookieDPAPI"
        })
        .and_then(Value::as_str)?;
    let cap = if version2 { 196_608 } else { 65_536 };
    if encoded.is_empty() || encoded.len() > cap {
        return None;
    }
    let ciphertext = STANDARD.decode(encoded).ok()?;
    let decrypted = dpapi.and_then(|f| f(&ciphertext, None))?;
    let (cookie, gateway_cookie) = if version2 {
        let packed = serde_json::from_str::<Value>(&decrypted).ok()?;
        let pobj = packed.as_object()?;
        let gw = pobj.get("gatewayCookie")?.as_str()?.to_owned();
        let ck = pobj.get("consoleCookie")?.as_str()?.to_owned();
        (ck, Some(gw))
    } else {
        (decrypted, None)
    };
    if cookie.is_empty()
        || cookie.chars().count() > 32_768
        || cookie.contains('\r')
        || cookie.contains('\n')
        || cookie.contains('\0')
    {
        return None;
    }
    if let Some(ref gw) = gateway_cookie
        && (gw.chars().count() > 32_768
            || gw.contains('\r')
            || gw.contains('\n')
            || gw.contains('\0'))
    {
        return None;
    }
    Some((cookie, region, gateway_cookie))
}

struct SafariCookie {
    name: String,
    value: String,
    domain: String,
    path: String,
    expiry: f64,
    host_only: bool,
}

fn safari_qwen_cookies(contents: &[u8], allowed_domains: &[&str], now: f64) -> Vec<SafariCookie> {
    if contents.len() > 16 * MAX_BYTES || contents.len() < 8 || &contents[..4] != b"cook" {
        return Vec::new();
    }
    let read_be_u32 = |buf: &[u8], off: usize| -> Option<usize> {
        let slice = buf.get(off..off + 4)?;
        Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]) as usize)
    };
    let read_le_u32 = |buf: &[u8], off: usize| -> Option<usize> {
        let slice = buf.get(off..off + 4)?;
        Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]) as usize)
    };
    let read_le_f64 = |buf: &[u8], off: usize| -> Option<f64> {
        let slice = buf.get(off..off + 8)?;
        Some(f64::from_le_bytes([
            slice[0], slice[1], slice[2], slice[3], slice[4], slice[5], slice[6], slice[7],
        ]))
    };
    let Some(page_count) = read_be_u32(contents, 4) else {
        return Vec::new();
    };
    if page_count > 4096 || 8 + page_count * 4 > contents.len() {
        return Vec::new();
    }
    let mut pos = 8 + page_count * 4;
    let mut result = Vec::new();
    let mut total_records = 0usize;
    for page_index in 0..page_count {
        let Some(page_size) = read_be_u32(contents, 8 + page_index * 4) else {
            return Vec::new();
        };
        if page_size < 12
            || pos
                .checked_add(page_size)
                .is_none_or(|end| end > contents.len())
        {
            return Vec::new();
        }
        let page = &contents[pos..pos + page_size];
        pos += page_size;
        if &page[..4] != b"\x00\x00\x01\x00" {
            return Vec::new();
        }
        let Some(count) = read_le_u32(page, 4) else {
            return Vec::new();
        };
        total_records = total_records.saturating_add(count);
        let Some(header_end) = count.checked_mul(4).and_then(|n| n.checked_add(12)) else {
            return Vec::new();
        };
        if count > 10_000
            || total_records > 20_000
            || header_end > page.len()
            || &page[header_end - 4..header_end] != b"\x00\x00\x00\x00"
        {
            return Vec::new();
        }
        let mut seen_offsets = BTreeSet::new();
        for index in 0..count {
            let Some(offset) = read_le_u32(page, 8 + index * 4) else {
                return Vec::new();
            };
            if !seen_offsets.insert(offset)
                || offset < header_end
                || offset.checked_add(56).is_none_or(|end| end > page.len())
            {
                return Vec::new();
            }
            let Some(size) = read_le_u32(page, offset) else {
                return Vec::new();
            };
            if size < 56 || offset.checked_add(size).is_none_or(|end| end > page.len()) {
                return Vec::new();
            }
            let record = &page[offset..offset + size];
            let read_str = |field_offset: usize| -> Option<String> {
                let s_off = read_le_u32(record, field_offset)?;
                if s_off < 56 || s_off >= record.len() {
                    return None;
                }
                let rel_end = record[s_off..].iter().position(|&b| b == 0)?;
                if rel_end > 8192 {
                    return None;
                }
                std::str::from_utf8(&record[s_off..s_off + rel_end])
                    .ok()
                    .map(str::to_owned)
            };
            let Some(raw_domain) = read_str(16).map(|s| s.to_ascii_lowercase()) else {
                return Vec::new();
            };
            let domain = raw_domain.trim_start_matches('.').to_owned();
            if !allowed_domains.contains(&domain.as_str()) {
                continue;
            }
            let Some(expiry) = read_le_f64(record, 40) else {
                return Vec::new();
            };
            if !expiry.is_finite()
                || expiry < 0.0
                || (expiry != 0.0 && expiry + 978_307_200.0 <= now)
            {
                continue;
            }
            let (Some(name), Some(path), Some(value)) = (read_str(20), read_str(24), read_str(28))
            else {
                return Vec::new();
            };
            if !SAFARI_COOKIE_NAME_RE.is_match(&name) || value.is_empty() {
                continue;
            }
            if value.contains(';')
                || value.contains('\r')
                || value.contains('\n')
                || value.contains('\0')
            {
                continue;
            }
            if !path.starts_with('/') {
                continue;
            }
            let prefix = format!("{}/", path.trim_end_matches('/'));
            if !["/tool/user/info.json", "/data/api.json", "/cn-beijing"]
                .iter()
                .any(|rp| *rp == path || rp.starts_with(&prefix))
            {
                continue;
            }
            result.push(SafariCookie {
                name,
                value,
                domain,
                path,
                expiry,
                host_only: !raw_domain.starts_with('.'),
            });
        }
    }
    result
}

pub(crate) fn safari_qwen_candidates(
    home: &Path,
    now_secs: Option<f64>,
    tracker: &mut ReadTracker,
) -> Vec<Value> {
    let path =
        home.join("Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies");
    let contents = match aac_files::read::read(&path, &ReadPolicy::capped((16 * MAX_BYTES) as u64))
    {
        Read::Absent | Read::Unreadable(ErrorKind::NotADirectory) => return Vec::new(),
        Read::Bytes(bytes, _) => bytes,
        Read::TooLarge => return Vec::new(),
        _ => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let now = now_unix_secs(now_secs);
    let regions: [(&str, &[&str]); 2] = [
        (
            "intl",
            &[
                "qwencloud.com",
                "home.qwencloud.com",
                "cs-data.qwencloud.com",
            ],
        ),
        (
            "cn",
            &[
                "aliyun.com",
                "console.aliyun.com",
                "bailian.console.aliyun.com",
                "bailian-cs.console.aliyun.com",
            ],
        ),
    ];
    let mut result = Vec::new();
    for (region, allowed) in regions {
        let mut cookies = safari_qwen_cookies(&contents, allowed, now);
        cookies.sort_by(|a, b| {
            b.domain
                .len()
                .cmp(&a.domain.len())
                .then_with(|| b.path.len().cmp(&a.path.len()))
                .then_with(|| b.expiry.total_cmp(&a.expiry))
        });
        let header_for = |host: &str, req_path: &str| -> String {
            let mut selected: Vec<(String, String)> = Vec::new();
            let mut seen = BTreeSet::new();
            for c in &cookies {
                let domain_matches =
                    host == c.domain || (!c.host_only && host.ends_with(&format!(".{}", c.domain)));
                let prefix = format!("{}/", c.path.trim_end_matches('/'));
                let path_matches = req_path == c.path || req_path.starts_with(&prefix);
                if domain_matches && path_matches && seen.insert(c.name.clone()) {
                    selected.push((c.name.clone(), c.value.clone()));
                }
            }
            selected
                .into_iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join("; ")
        };
        let (console_host, console_path, gateway_host) = if region == "intl" {
            (
                "home.qwencloud.com",
                "/tool/user/info.json",
                "cs-data.qwencloud.com",
            )
        } else {
            (
                "bailian.console.aliyun.com",
                "/cn-beijing",
                "bailian-cs.console.aliyun.com",
            )
        };
        let header = header_for(console_host, console_path);
        let gateway_header = header_for(gateway_host, "/data/api.json");
        if header.is_empty() || header.len() > 32_768 || gateway_header.len() > 32_768 {
            continue;
        }
        let packed = json!({
            "cookie": header,
            "gatewayCookie": gateway_header,
            "baseUrl": format!(
                "https://token-plan.{}.maas.aliyuncs.com/compatible-mode/v1",
                if region == "intl" { "ap-southeast-1" } else { "cn-beijing" }
            )
        });
        result.push(json!({
            "secret": packed.to_string(),
            "source": "Safari console",
            "email": null,
            "expires": null
        }));
    }
    result
}

/// Default `reqwest` implementation of [`PlanTransport`] matching `plan_common.HttpClient`.
pub(crate) struct ReqwestPlanTransport {
    client: reqwest::Client,
    deadline: Instant,
    now_override: Option<f64>,
}

impl ReqwestPlanTransport {
    pub(crate) fn new(
        deadline_secs: u64,
        now_override: Option<f64>,
    ) -> Result<Self, CollectionError> {
        let client = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| CollectionError {
                status: "error",
                message: "The usage service is unavailable or returned an invalid response.",
                failure_code: None,
                retry_after: None,
            })?;
        Ok(Self {
            client,
            deadline: Instant::now() + Duration::from_secs(deadline_secs),
            now_override,
        })
    }
}

impl PlanTransport for ReqwestPlanTransport {
    fn request<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a [u8]>,
        text: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PlanHttpBody, CollectionError>> + Send + 'a>> {
        Box::pin(async move {
            let remaining = self.deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(CollectionError {
                    status: "error",
                    message: "The usage service timed out.",
                    failure_code: None,
                    retry_after: None,
                });
            }
            let mut req = if let Some(b) = body {
                self.client.post(url).body(b.to_vec())
            } else {
                self.client.get(url)
            };
            req = req
                .timeout(remaining.min(Duration::from_secs(4)))
                .header("Accept", "application/json")
                .header("User-Agent", "CCS-Accounts/1.0");
            for (k, v) in headers {
                req = req.header(k.as_str(), v.as_str());
            }
            let mut resp = req.send().await.map_err(|_| CollectionError {
                status: "error",
                message: "The usage service is unavailable or returned an invalid response.",
                failure_code: None,
                retry_after: None,
            })?;
            let status = resp.status().as_u16();
            match status {
                401 => {
                    return Err(CollectionError {
                        status: "needs_sign_in",
                        message: "The saved credential was rejected by the usage service.",
                        failure_code: None,
                        retry_after: None,
                    });
                }
                403 if url == OPENCODE_GO_URL => {
                    return Err(CollectionError {
                        status: "error",
                        message: "OpenCode Go usage access is unavailable; the saved key's sign-in status could not be verified.",
                        failure_code: Some("provider_error"),
                        retry_after: None,
                    });
                }
                403 => {
                    return Err(CollectionError {
                        status: "needs_sign_in",
                        message: "The saved account cannot access this plan's usage.",
                        failure_code: None,
                        retry_after: None,
                    });
                }
                429 => {
                    let retry_hdr = resp
                        .headers()
                        .get("retry-after")
                        .and_then(|h| h.to_str().ok());
                    let retry = retry_after_seconds(retry_hdr, now_unix_secs(self.now_override));
                    return Err(CollectionError {
                        status: "error",
                        message: "The usage service asked to slow down; try again later.",
                        failure_code: Some("rate_limited"),
                        retry_after: Some(retry),
                    });
                }
                300.. => {
                    return Err(CollectionError {
                        status: "error",
                        message: "The usage service could not complete the request.",
                        failure_code: None,
                        retry_after: None,
                    });
                }
                _ => {}
            }
            let mut raw = Vec::new();
            while let Some(chunk) = resp.chunk().await.map_err(|_| CollectionError {
                status: "error",
                message: "The usage service is unavailable or returned an invalid response.",
                failure_code: None,
                retry_after: None,
            })? {
                if Instant::now() >= self.deadline {
                    return Err(CollectionError {
                        status: "error",
                        message: "The usage service timed out.",
                        failure_code: None,
                        retry_after: None,
                    });
                }
                let take = chunk.len().min(MAX_BYTES + 1 - raw.len());
                raw.extend_from_slice(&chunk[..take]);
                if raw.len() > MAX_BYTES {
                    return Err(CollectionError {
                        status: "error",
                        message: "The usage service returned an oversized response.",
                        failure_code: None,
                        retry_after: None,
                    });
                }
            }
            if text {
                let s = String::from_utf8(raw).map_err(|_| CollectionError {
                    status: "error",
                    message: "The usage service is unavailable or returned an invalid response.",
                    failure_code: None,
                    retry_after: None,
                })?;
                return Ok(PlanHttpBody::Text(s));
            }
            let val: Value = serde_json::from_slice(&raw).map_err(|_| CollectionError {
                status: "error",
                message: "The usage service is unavailable or returned an invalid response.",
                failure_code: None,
                retry_after: None,
            })?;
            if !val.is_object() {
                return Err(CollectionError {
                    status: "error",
                    message: "The usage service is unavailable or returned an invalid response.",
                    failure_code: None,
                    retry_after: None,
                });
            }
            Ok(PlanHttpBody::Json(val))
        })
    }
}

/// Aliases in `~/.local/share/opencode/auth.json` for plan providers.
pub(crate) fn opencode_aliases(provider: ProviderId) -> &'static [&'static str] {
    match provider {
        ProviderId::KimiCode => &["kimi-for-coding", "kimi-code"],
        ProviderId::Qwen => &["alibaba-token-plan"],
        ProviderId::Zai => &["zai", "zai-coding-plan"],
        ProviderId::OpencodeGo => &["opencode-go"],
        _ => &[],
    }
}

/// Environment variable names for plan providers.
pub(crate) fn env_var_names(provider: ProviderId) -> &'static [&'static str] {
    match provider {
        ProviderId::KimiCode => &["KIMI_CODE_API_KEY", "KIMI_API_KEY"],
        ProviderId::Qwen => &["BAILIAN_TOKEN_PLAN_API_KEY"],
        ProviderId::Zai => &["ZAI_API_KEY", "ZHIPU_API_KEY"],
        ProviderId::OpencodeGo => &["OPENCODE_GO_API_KEY", "OPENCODE_API_KEY"],
        _ => &[],
    }
}
