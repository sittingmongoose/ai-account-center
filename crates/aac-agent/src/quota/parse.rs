use super::{error, proof};
use serde_json::{Value, json};
fn python_float_text(raw: &str) -> Option<f64> {
    // Python float(str): surrounding whitespace, signs, decimal/exponent forms and digit
    // underscores. Underscores are valid only between digits; Rust's parser does not know them.
    let text = raw.trim();
    if text.is_empty() {
        return None;
    }
    let (sign, body) = if let Some(rest) = text.strip_prefix('+') {
        ("", rest)
    } else if let Some(rest) = text.strip_prefix('-') {
        ("-", rest)
    } else {
        ("", text)
    };
    let lower = body.to_ascii_lowercase();
    if matches!(lower.as_str(), "inf" | "infinity" | "nan") {
        return None; // callers need finite nonnegative values
    }
    let bytes = body.as_bytes();
    let mut cleaned = String::with_capacity(body.len());
    for (i, &c) in bytes.iter().enumerate() {
        if c == b'_' {
            let before = i > 0 && bytes[i - 1].is_ascii_digit();
            let after = i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit();
            if !before || !after {
                return None;
            }
            continue;
        }
        cleaned.push(c as char);
    }
    let cleaned = format!("{sign}{cleaned}");
    let n = cleaned.parse::<f64>().ok()?;
    (n.is_finite() && n >= 0.).then_some(n)
}
fn api_number(v: &Value) -> Option<f64> {
    // plan_common.api_number/percent: float(value), finite and nonnegative.
    match v {
        Value::Number(n) => n.as_f64().filter(|n| n.is_finite() && *n >= 0.),
        Value::String(s) => python_float_text(s),
        _ => None,
    }
}
/// `plan_common.reset_at`: numeric seconds/milliseconds or a datetime string normalized by
/// `datetime.fromisoformat(...replace("Z", "+00:00"))`, then years 2000 to 2200 only.
fn reset(v: &Value) -> Value {
    if v.is_null() || v.is_boolean() {
        return Value::Null;
    }
    let instant = if v.is_number()
        || v.as_str().is_some_and(|s| {
            let t = s.trim();
            !t.is_empty()
                && t.bytes().all(|c| {
                    c.is_ascii_digit()
                        || c == b'.'
                        || c == b'+'
                        || c == b'-'
                        || c == b'e'
                        || c == b'E'
                        || c == b'_'
                })
                && t.chars()
                    .next()
                    .is_some_and(|c| c.is_ascii_digit() || c == '+' || c == '-')
        }) {
        api_number(v).filter(|n| *n > 0.).and_then(|n| {
            aac_num::py::iso_seconds_of_timestamp(if n > 100_000_000_000. { n / 1000. } else { n })
        })
    } else if let Some(raw) = v.as_str() {
        let text = raw.trim().replace('Z', "+00:00");
        match chrono::DateTime::parse_from_rfc3339(&text) {
            Ok(d) => {
                let seconds = d.timestamp();
                aac_num::py::iso_seconds(seconds)
                    .map(|word| (word, d.format("%Y").to_string().parse::<i64>().unwrap_or(0)))
            }
            Err(_) => {
                proof::reject();
                return Value::Null;
            }
        }
    } else {
        None
    };
    instant
        .filter(|(_, year)| (2000..=2200).contains(year))
        .map(|(word, _)| json!(word))
        .unwrap_or(Value::Null)
}
#[allow(clippy::too_many_arguments)]
fn window_full(
    key: &str,
    label: &str,
    ratio: Option<f64>,
    reset_raw: &Value,
    minutes: Option<f64>,
    used: Option<f64>,
    limit: Option<f64>,
    unit: Option<&str>,
    kind: &str,
    remaining: Option<f64>,
    expires_at: Option<&Value>,
    unlimited: Option<bool>,
    enabled: Option<bool>,
) -> Value {
    let ratio = ratio.filter(|n| n.is_finite() && *n >= 0.);
    let mut w = json!({"key":key,"label":label,"usedPercent":ratio,"remainingPercent":ratio.map(|n| aac_num::py::round((100.-n).max(0.), 8).unwrap_or_else(|| { proof::reject(); n })),"resetAt":reset(reset_raw),"windowMinutes":minutes,"used":used,"limit":limit,"unit":unit});
    if matches!(kind, "rate_limit" | "balance" | "spend" | "extra_usage") {
        w["kind"] = json!(kind);
    }
    if let Some(remaining) = remaining {
        w["remaining"] = json!(remaining);
    }
    if let Some(expiry) = expires_at {
        w["expiresAt"] = reset(expiry);
    }
    if let Some(unlimited) = unlimited {
        w["unlimited"] = json!(unlimited);
    }
    if let Some(enabled) = enabled {
        w["enabled"] = json!(enabled);
    }
    w
}
#[allow(clippy::too_many_arguments)]
fn window(
    key: &str,
    label: &str,
    ratio: Option<f64>,
    reset_raw: &Value,
    minutes: Option<f64>,
    used: Option<f64>,
    limit: Option<f64>,
    unit: Option<&str>,
    remaining: &Value,
) -> Value {
    window_full(
        key,
        label,
        ratio,
        reset_raw,
        minutes,
        used,
        limit,
        unit,
        "rate_limit",
        api_number(remaining),
        None,
        None,
        None,
    )
}
fn valid(plan: Value, windows: Vec<Value>) -> Value {
    let windows: Vec<_> = windows
        .into_iter()
        .filter(|w| {
            [
                "usedPercent",
                "used",
                "limit",
                "remaining",
                "resetAt",
                "expiresAt",
                "enabled",
                "unlimited",
            ]
            .iter()
            .any(|k| !w[k].is_null())
        })
        .collect();
    if windows.is_empty() {
        error(
            "error",
            "The usage service returned no valid plan quota windows.",
        )
    } else {
        json!({"plan":plan,"windows":windows})
    }
}
pub(super) fn parse(provider: &str, payload: &Value) -> Value {
    match provider {
        "opencode-go" => go(payload),
        "kimi-code" => kimi(payload),
        "zai" => zai(payload),
        _ => proof::fallback(),
    }
}
fn go(p: &Value) -> Value {
    if !p["usage"].is_object() {
        return error(
            "error",
            "The usage service returned an invalid plan quota response.",
        );
    }
    let mut windows = Vec::new();
    for (key, label, minutes) in [
        ("rolling", "5 hours", Some(300.)),
        ("weekly", "Weekly", Some(10080.)),
        ("monthly", "Monthly", None),
    ] {
        let item = &p["usage"][key];
        let ratio = api_number(&item["percent"]);
        if !matches!(item["status"].as_str(), Some("ok" | "rate-limited")) || ratio.is_none() {
            return error(
                "error",
                "The usage service returned an incomplete plan quota response.",
            );
        }
        windows.push(window(
            key,
            label,
            ratio,
            &item["resetsAt"],
            minutes,
            None,
            None,
            None,
            &Value::Null,
        ));
    }
    valid(json!("Go"), windows)
}
fn counts(d: &Value) -> (Option<f64>, Option<f64>, Option<f64>) {
    let limit = api_number(&d["limit"]);
    let mut used = api_number(&d["used"]);
    let left = api_number(&d["remaining"]);
    if let Some(l) = limit.filter(|l| *l > 0.) {
        if used.is_none()
            && let Some(r) = left.filter(|r| *r <= l)
        {
            used = Some(l - r);
        }
        return (used.map(|u| 100. * u / l), used, limit);
    }
    (None, used, limit)
}
fn kimi_reset(d: &Value) -> &Value {
    ["resetTime", "resetAt", "reset_time", "reset_at"]
        .into_iter()
        .map(|k| &d[k])
        .find(|v| !reset(v).is_null())
        .unwrap_or(&Value::Null)
}
fn kimi_booster(p: &Value) -> Vec<Value> {
    let wallet = &p["boosterWallet"];
    if !wallet.is_object() {
        return Vec::new();
    }
    let monthly_limit = &wallet["monthlyChargeLimit"];
    let monthly_used = &wallet["monthlyUsed"];
    let currency = monthly_limit["currency"]
        .as_str()
        .or_else(|| monthly_used["currency"].as_str())
        .filter(|c| c.len() == 3 && c.bytes().all(|b| b.is_ascii_uppercase()));
    let scale = if currency.is_some() { 100. } else { 1. };
    let unit = currency.unwrap_or("cents");
    let mut result = Vec::new();
    let balance = &wallet["balance"];
    if balance["type"] == "BOOSTER" {
        let cents = |raw: Option<f64>| -> Option<f64> {
            raw.map(|n| {
                let value = n.trunc() / 1_000_000.;
                if value > 0. && value < 1. {
                    1.
                } else {
                    (value + 0.5).floor()
                }
            })
        };
        let total = cents(api_number(&balance["amount"]));
        let left = cents(api_number(&balance["amountLeft"]));
        if let Some(total) = total.filter(|n| *n > 0.) {
            let used = left.filter(|l| *l <= total).map(|l| total - l);
            let ratio = used.map(|u| 100. * u / total);
            result.push(window_full(
                "booster-balance",
                "Booster balance",
                ratio,
                &Value::Null,
                None,
                used.map(|u| u / scale),
                Some(total / scale),
                Some(unit),
                "balance",
                left.map(|l| l / scale),
                None,
                None,
                None,
            ));
        }
    }
    let limit = api_number(&monthly_limit["priceInCents"]);
    let used = api_number(&monthly_used["priceInCents"]);
    let enabled = wallet["monthlyChargeLimitEnabled"].as_bool();
    if limit.is_some() || used.is_some() || enabled.is_some() {
        let ratio = used
            .zip(limit.filter(|l| *l > 0.))
            .map(|(u, l)| 100. * u / l);
        let remaining = limit.zip(used).and_then(|(l, u)| (u <= l).then_some(l - u));
        result.push(window_full(
            "booster-monthly-spend",
            "Monthly booster spend",
            ratio,
            &Value::Null,
            None,
            used.map(|u| u / scale),
            limit.map(|l| l / scale),
            Some(unit),
            "spend",
            remaining.map(|r| r / scale),
            None,
            None,
            enabled,
        ));
    }
    result
}
fn kimi(p: &Value) -> Value {
    if p["user"]["membership"]["level"].is_array()
        || p["user"]["membership"]["level"].is_object()
        || p["limits"].as_array().is_some_and(|rows| {
            rows.iter()
                .any(|r| r["window"]["timeUnit"].is_array() || r["window"]["timeUnit"].is_object())
        })
    {
        return proof::fallback();
    }
    let mut windows = serde_json::Map::new();
    if p["usage"].is_object() {
        let d = &p["usage"];
        let (r, u, l) = counts(d);
        windows.insert(
            "weekly".into(),
            window(
                "weekly",
                "Weekly",
                r,
                kimi_reset(d),
                Some(10080.),
                u,
                l,
                None,
                &d["remaining"],
            ),
        );
    }
    if let Some(limits) = p["limits"].as_array() {
        for (i, item) in limits.iter().take(30).enumerate() {
            let (d, w) = (&item["detail"], &item["window"]);
            if !d.is_object() || !w.is_object() {
                continue;
            }
            let multiplier = match w["timeUnit"].as_str() {
                Some("TIME_UNIT_MINUTE") => Some(1.),
                Some("TIME_UNIT_HOUR") => Some(60.),
                Some("TIME_UNIT_DAY") => Some(1440.),
                Some("TIME_UNIT_WEEK") => Some(10080.),
                Some("TIME_UNIT_SECOND") => Some(1. / 60.),
                _ => None,
            };
            let minutes = api_number(&w["duration"])
                .zip(multiplier)
                .map(|(d, m)| d * m);
            let (key, label) = match minutes {
                Some(300.) => ("5h".into(), "5 hours".into()),
                Some(10080.) => ("weekly".into(), "Weekly".into()),
                _ => (format!("limit-{i}"), format!("Plan limit {}", i + 1)),
            };
            let (r, u, l) = counts(d);
            let reset = kimi_reset(d);
            if r.is_some() || !reset.is_null() || u.is_some() || l.is_some() {
                windows.insert(
                    key.clone(),
                    window(&key, &label, r, reset, minutes, u, l, None, &d["remaining"]),
                );
            }
        }
    }
    for (up, key, label, minutes) in [
        ("limit_5h", "5h", "5 hours", Some(300.)),
        ("limit_7d", "weekly", "Weekly", Some(10080.)),
        ("limit_month_total", "monthly", "Monthly total", None),
        ("limit_month_code", "monthly-code", "Monthly coding", None),
    ] {
        let item = &p["usages"][up];
        let ratio = api_number(&item["used_ratio"]).filter(|n| *n <= 1.);
        let modern_reset = reset(&item["reset_time"]);
        if item.is_object()
            && ratio == Some(0.)
            && !p["usages"]["limit_month_total"].is_object()
            && windows.get(key).is_some_and(|legacy| {
                legacy["used"].as_f64().is_some_and(|u| u > 0.)
                    && legacy["windowMinutes"].as_f64() == minutes
                    && !modern_reset.is_null()
                    && !legacy["resetAt"].is_null()
                    && legacy["resetAt"]
                        .as_str()
                        .zip(modern_reset.as_str())
                        .is_some_and(|(a, b)| {
                            chrono::DateTime::parse_from_rfc3339(a)
                                .ok()
                                .zip(chrono::DateTime::parse_from_rfc3339(b).ok())
                                .is_some_and(|(x, y)| (x.timestamp() - y.timestamp()).abs() <= 2)
                        })
            })
        {
            continue;
        }
        if item.is_object() && (ratio.is_some() || !modern_reset.is_null()) {
            windows.insert(
                key.into(),
                window(
                    key,
                    label,
                    ratio.map(|n| n * 100.),
                    &item["reset_time"],
                    minutes,
                    None,
                    None,
                    None,
                    &Value::Null,
                ),
            );
        }
    }
    let mut windows: Vec<_> = windows.into_values().collect();
    windows.sort_by(|a, b| {
        let x = a["windowMinutes"].as_f64();
        let y = b["windowMinutes"].as_f64();
        x.is_none()
            .cmp(&y.is_none())
            .then_with(|| x.unwrap_or(0.).total_cmp(&y.unwrap_or(0.)))
            .then_with(|| a["key"].as_str().cmp(&b["key"].as_str()))
    });
    windows.extend(kimi_booster(p));
    let level = p["user"]["membership"]["level"].as_str().unwrap_or("");
    let legacy = p["version"].is_null() || p["version"] == "GOODS_VERSION_V1";
    let plan = match (legacy, level) {
        (true, "LEVEL_FREE") => Some("Adagio"),
        (true, "LEVEL_TRIAL") => Some("Andante"),
        (true, "LEVEL_BASIC") => Some("Moderato"),
        (true, "LEVEL_INTERMEDIATE") => Some("Allegretto"),
        (true, "LEVEL_ADVANCED") => Some("Allegro"),
        (false, "LEVEL_FREE") => Some("Free"),
        (false, "LEVEL_TRIAL") => Some("Trial"),
        (false, "LEVEL_BASIC") => Some("Basic"),
        (false, "LEVEL_INTERMEDIATE") => Some("Intermediate"),
        (false, "LEVEL_ADVANCED") => Some("Advanced"),
        _ => None,
    };
    let plan = plan.map(str::to_owned).or_else(|| kimi_new_level(level));
    valid(json!(plan), windows)
}
/// `re.fullmatch(r"LEVEL_[A-Z]{1,16}(?:_[A-Z]{1,16}){0,2}", level)`, then each word capitalized.
fn kimi_new_level(level: &str) -> Option<String> {
    let rest = level.strip_prefix("LEVEL_")?;
    let words: Vec<&str> = rest.split('_').collect();
    if words.len() > 3
        || words
            .iter()
            .any(|w| w.is_empty() || w.len() > 16 || !w.bytes().all(|c| c.is_ascii_uppercase()))
    {
        return None;
    }
    Some(
        words
            .iter()
            .map(|w| w[..1].to_owned() + &w[1..].to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(" "),
    )
}
fn zai(p: &Value) -> Value {
    if p["code"].is_boolean() {
        return proof::fallback();
    }
    if p["success"] == false
        || !matches!(&p["code"], Value::Null)
            && ![json!(0), json!(200), json!("0"), json!("200")].contains(&p["code"])
    {
        return error(
            "error",
            "The usage service could not return this coding plan.",
        );
    }
    let Some(limits) = p["data"]["limits"].as_array() else {
        return error(
            "error",
            "The usage service returned an invalid plan quota response.",
        );
    };
    let mut windows = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (index, item) in limits.iter().take(30).enumerate() {
        let kind = match item["type"].as_str() {
            Some("TIME_LIMIT") => "Requests",
            Some("TOKENS_LIMIT") => "Tokens",
            Some("CREDIT_LIMIT") => "Credits",
            _ => continue,
        };
        let count = api_number(&item["number"]);
        let unit_value = &item["unit"];
        let unit = unit_value.as_i64();
        let monthly = kind == "Requests" && unit == Some(5) && count == Some(1.);
        let multiplier = match unit {
            Some(1) => Some(1440.),
            Some(3) => Some(60.),
            Some(6) => Some(10080.),
            _ => None,
        };
        let minutes = if monthly {
            None
        } else {
            count.zip(multiplier).map(|(n, m)| n * m)
        };
        fn py_g(n: f64) -> String {
            if n == 0. {
                return "0".into();
            }
            let exponent = n.abs().log10().floor() as i32;
            if !(-4..6).contains(&exponent) {
                let text = format!("{:.5e}", n);
                let Some(epos) = text.find('e') else {
                    return py_g(0.);
                };
                let exp = text[epos + 1..].parse::<i32>().unwrap_or(0);
                let mantissa = text[..epos].trim_end_matches('0').trim_end_matches('.');
                format!(
                    "{}e{}{:02}",
                    mantissa,
                    if exp < 0 { "-" } else { "+" },
                    exp.abs()
                )
            } else {
                let precision = (5 - exponent).max(0) as usize;
                let mut text = format!("{n:.precision$}");
                if text.contains('.') {
                    text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
                }
                text
            }
        }
        let duration = match minutes {
            Some(300.) => "5 hours".into(),
            Some(10080.) => "Weekly".into(),
            _ if monthly => "Monthly".into(),
            _ => match (count, unit) {
                (Some(n), Some(1)) => format!("{} days", py_g(n)),
                (Some(n), Some(3)) => format!("{} hours", py_g(n)),
                (Some(n), Some(6)) => format!("{} weeks", py_g(n)),
                _ => "Plan limit".into(),
            },
        };
        let meter = item["type"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .replace("_limit", "");
        // plan_usage.py `"{}".format(count)` on a float: its repr ("5.0").
        let count_key = count
            .map(aac_num::py::repr)
            .unwrap_or_else(|| "None".into());
        let unit_key = match unit_value {
            Value::Null => "None".into(),
            Value::Number(n) => n.to_string(),
            Value::String(s) => s.clone(),
            _ => {
                proof::reject();
                return proof::fallback();
            }
        };
        let mut key = format!("{meter}-{unit_key}-{count_key}");
        if !seen.insert(key.clone()) {
            key.push_str(&format!("-{index}"));
        }
        let used = api_number(&item["currentValue"]);
        let limit = api_number(&item["usage"]);
        let ratio = used
            .zip(limit.filter(|l| *l > 0.))
            .map(|(u, l)| 100. * u / l)
            .or_else(|| api_number(&item["percentage"]));
        let label = format!("{duration} · {kind}");
        let unit_name = if kind == "Requests" {
            "requests".into()
        } else {
            kind.to_lowercase()
        };
        windows.push(window(
            &key,
            &label,
            ratio,
            &item["nextResetTime"],
            minutes,
            used,
            limit,
            Some(&unit_name),
            &item["remaining"],
        ));
        if kind == "Requests"
            && let Some(details) = item["usageDetails"].as_array()
        {
            for d in details.iter().take(30) {
                // plan_usage.py tests `detail.get("modelCode") not in labels`: a list or object code
                // is unhashable there, raises TypeError, and the whole collector reports an error.
                // Skipping it would publish the parent quota as current data instead: delegate.
                if d.is_object() && (d["modelCode"].is_array() || d["modelCode"].is_object()) {
                    return proof::fallback();
                }
                let label = match d["modelCode"].as_str() {
                    Some("search-prime") => "Search",
                    Some("web-reader") => "Web reader",
                    Some("zread") => "Zread",
                    _ => continue,
                };
                if let Some(u) = api_number(&d["usage"]) {
                    let mut w = window(
                        &format!("{key}-{}", d["modelCode"].as_str().unwrap_or("")),
                        label,
                        None,
                        &Value::Null,
                        None,
                        Some(u),
                        None,
                        Some("requests"),
                        &Value::Null,
                    );
                    w["kind"] = json!("extra_usage");
                    windows.push(w);
                }
            }
        }
    }
    valid(plan_label(&p["data"]["level"]), windows)
}
fn plan_label(v: &Value) -> Value {
    let s = v.as_str().unwrap_or("").to_lowercase();
    if [
        "free",
        "lite",
        "essential",
        "basic",
        "standard",
        "pro",
        "professional",
        "premium",
        "max",
        "ultra",
        "team",
        "business",
        "enterprise",
        "go",
    ]
    .contains(&s.as_str())
    {
        json!(s)
    } else {
        // plan_common.plan_label: a tier the table does not list yet keeps the provider's own
        // short, word-shaped name; a longer or key-shaped string stays unknown.
        v.as_str()
            .and_then(plan_name)
            .map_or(Value::Null, |name| json!(name))
    }
}
/// Python `value.replace("_", " ")`, then `len <= 32`, `PLAN_NAME.fullmatch` and no `[0-9]{4}`.
fn plan_name(value: &str) -> Option<String> {
    let name = value.replace('_', " ");
    if name.chars().count() > 32 || !name.is_ascii() {
        return None;
    }
    let words: Vec<&str> = name.split(' ').collect();
    let first = words.first()?;
    let ok_first = first.len() <= 16
        && first
            .bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic())
        && first.bytes().all(|c| c.is_ascii_alphanumeric());
    let ok_rest = words.len() <= 3
        && words[1..].iter().all(|w| {
            !w.is_empty() && w.len() <= 16 && w.bytes().all(|c| c.is_ascii_alphanumeric())
        });
    let digits = name
        .as_bytes()
        .windows(4)
        .any(|w| w.iter().all(u8::is_ascii_digit));
    (ok_first && ok_rest && !digits).then_some(name)
}
pub(super) fn packs(p: &Value, now: i64) -> Value {
    if p["code"].is_boolean() {
        return proof::fallback();
    }
    if p["success"] != true
        || !p["code"].is_null()
            && ![json!(0), json!(200), json!("0"), json!("200")].contains(&p["code"])
        || !p["data"].is_object()
    {
        return Value::Null;
    }
    let mut windows = Vec::new();
    // Wonder L13: outside `iso_seconds`' domain the comparison instant is unproven; decline the
    // whole verb as the neighbouring `round` fallback does, never compare against "".
    let Some(current) = aac_num::py::iso_seconds(now) else {
        proof::reject();
        return Value::Null;
    };
    for (field, prefix, label) in [
        ("fiveHourResets", "5h", "5-hour"),
        ("weekResets", "weekly", "Weekly"),
    ] {
        let Some(rows) = p["data"][field].as_array() else {
            continue;
        };
        let mut left = 0;
        let mut used = 0;
        let mut expiries = Vec::new();
        let mut complete = rows.len() <= 100;
        for row in rows.iter().take(100) {
            if !row.is_object() {
                complete = false;
                continue;
            }
            let raw = &row["expireTime"];
            let normalized = if let Some(s) = raw
                .as_str()
                .filter(|s| s.len() == 19 && s.as_bytes()[10] == b' ')
            {
                json!(format!("{}T{}+08:00", &s[..10], &s[11..]))
            } else {
                raw.clone()
            };
            let expiry = reset(&normalized);
            let date = expiry.as_str();
            let in_date = date.is_none_or(|d| d > current.as_str());
            match row["available"].as_bool() {
                None => {
                    complete = false;
                    if in_date && let Some(d) = date {
                        expiries.push(d.to_owned());
                    }
                }
                Some(true) if in_date => {
                    left += 1;
                    if let Some(d) = date {
                        expiries.push(d.to_owned());
                    }
                }
                Some(false) if in_date && date.is_some() => used += 1,
                _ => {}
            }
        }
        let mut w = window(
            &format!("reset-packs-{prefix}"),
            &format!("{label} reset packs"),
            None,
            &Value::Null,
            None,
            complete.then_some(used as f64),
            complete.then_some((used + left) as f64),
            Some("packs"),
            &Value::Null,
        );
        w["kind"] = json!("balance");
        if complete {
            w["remaining"] = json!(left);
        }
        if let Some(expiry) = expiries.iter().min() {
            w["expiresAt"] = json!(expiry);
        }
        windows.push(w);
    }
    if windows.is_empty() {
        Value::Null
    } else {
        json!(windows)
    }
}

fn unwrap_qwen_markers(p: &Value, markers: &[&str]) -> Result<Value, Value> {
    let mut v = p.clone();
    for _ in 0..7 {
        if !v.is_object() {
            break;
        }
        if v["successResponse"] == false || v["success"] == false {
            return Err(error(
                "needs_sign_in",
                "The Qwen console session could not access token-plan usage.",
            ));
        }
        if markers.iter().any(|k| v.get(*k).is_some()) {
            return Ok(v);
        }
        let next = v
            .get("DataV2")
            .or_else(|| v.get("Data"))
            .or_else(|| v.get("data"))
            .cloned()
            .unwrap_or(Value::Null);
        v = if let Some(s) = next.as_str() {
            serde_json::from_str(s).unwrap_or(Value::Null)
        } else {
            next
        };
    }
    Err(error(
        "error",
        "The Qwen console returned no valid token-plan quota response.",
    ))
}
fn unwrap_qwen(p: &Value) -> Result<Value, Value> {
    unwrap_qwen_markers(
        p,
        &[
            "per5HourPercentage",
            "per1WeekPercentage",
            "per1MonthPercentage",
            "per5HourResetTime",
            "per1WeekResetTime",
            "per1MonthResetTime",
        ],
    )
}
pub(super) fn qwen(payloads: &[Value]) -> Value {
    let data = match unwrap_qwen(&payloads[0]) {
        Ok(data) => data,
        Err(failure) => return failure,
    };
    let subscription_data = payloads.get(1).filter(|p| !p.is_null()).and_then(|p| {
        unwrap_qwen_markers(
            p,
            &["specCode", "spec_code", "planName", "plan_name", "endTime"],
        )
        .ok()
    });
    let tier = subscription_data
        .as_ref()
        .and_then(|s| s["specCode"].as_str().or_else(|| s["spec_code"].as_str()));
    let quotas = tier
        .and_then(|tier| {
            payloads
                .get(2)
                .filter(|p| !p.is_null())
                .and_then(|p| unwrap_qwen_markers(p, &[tier]).ok())
                .map(|configs| (tier, configs))
        })
        .and_then(|(tier, configs)| configs.get(tier).cloned())
        .filter(|q| q.is_object());
    let quota = quotas.as_ref().unwrap_or(&Value::Null);
    let mut windows = Vec::new();
    for (prefix, key, label, minutes) in [
        ("per5Hour", "5h", "5 hours", Some(300.)),
        ("per1Week", "weekly", "Weekly", Some(10080.)),
        ("per1Month", "monthly", "Monthly", None),
    ] {
        let ratio = api_number(&data[format!("{prefix}Percentage")]);
        let r = &data[format!("{prefix}ResetTime")];
        if ratio.is_none() && reset(r).is_null() {
            continue;
        }
        let used_percent = ratio.map(|n| if n <= 1. { n * 100. } else { n });
        let names: &[&str] = match key {
            "5h" => &["five_hour", "fiveHour"],
            "weekly" => &["weekly"],
            _ => &["monthly"],
        };
        let limit = names.iter().find_map(|name| api_number(&quota[*name]));
        let used = limit.zip(used_percent).map(|(l, p)| l * p / 100.);
        let remaining = limit.zip(used).map(|(l, u)| l - u);
        windows.push(window_full(
            key,
            label,
            used_percent,
            r,
            minutes,
            used,
            limit,
            limit.map(|_| "credits"),
            "rate_limit",
            remaining,
            None,
            None,
            None,
        ));
    }
    let mut plan = Value::Null;
    if let Some(subscription) = subscription_data {
        plan = plan_label(
            &subscription["planName"]
                .clone()
                .as_str()
                .or_else(|| subscription["plan_name"].as_str())
                .map_or(Value::Null, |s| json!(s)),
        );
        if plan.is_null() {
            plan = plan_label(
                &subscription["specCode"]
                    .clone()
                    .as_str()
                    .or_else(|| subscription["spec_code"].as_str())
                    .map_or(Value::Null, |s| json!(s)),
            );
        }
        let expiry = &subscription["endTime"];
        if !reset(expiry).is_null() {
            windows.push(window_full(
                "subscription",
                "Plan subscription",
                None,
                &Value::Null,
                None,
                None,
                None,
                None,
                "rate_limit",
                None,
                Some(expiry),
                None,
                None,
            ));
        }
    }
    if let Some(addon) = payloads.get(3).filter(|p| !p.is_null())
        && let Ok(extras) = unwrap_qwen_markers(
            addon,
            &["remainingCredits", "totalCredits", "activeCount", "items"],
        )
    {
        let limit = api_number(&extras["totalCredits"]);
        let remaining = api_number(&extras["remainingCredits"]);
        let used = limit
            .zip(remaining)
            .and_then(|(l, r)| (r <= l).then_some(l - r));
        let ratio = used
            .zip(limit.filter(|l| *l > 0.))
            .map(|(u, l)| 100. * u / l);
        if limit.is_some() || remaining.is_some() {
            windows.push(window_full(
                "addon-credits",
                "Additional credits",
                ratio,
                &Value::Null,
                None,
                used,
                limit,
                Some("credits"),
                "balance",
                remaining,
                None,
                None,
                None,
            ));
        }
        if let Some(active) = api_number(&extras["activeCount"]) {
            windows.push(window_full(
                "addon-packs",
                "Active credit packs",
                None,
                &Value::Null,
                None,
                None,
                None,
                Some("packs"),
                "balance",
                Some(active),
                None,
                None,
                None,
            ));
        }
        if let Some(items) = extras["items"].as_array() {
            for (index, item) in items.iter().take(100).enumerate() {
                if !item.is_object() {
                    continue;
                }
                let total = api_number(&item["totalCredits"]);
                let remaining = api_number(&item["remainingCredits"]);
                let expiry = &item["endTime"];
                if total.is_none() && remaining.is_none() && reset(expiry).is_null() {
                    continue;
                }
                let spent = total
                    .zip(remaining)
                    .and_then(|(t, r)| (r <= t).then_some(t - r));
                let ratio = spent
                    .zip(total.filter(|t| *t > 0.))
                    .map(|(u, t)| u / t * 100.);
                let identifier = match &item["orderId"] {
                    Value::String(s) => s.clone(),
                    Value::Number(n) => n.to_string(),
                    _ => index.to_string(),
                };
                let digest = aac_core::sha256_hex(identifier.as_bytes());
                windows.push(window_full(
                    &format!("addon-pack-{}", &digest[..12]),
                    &format!("Additional credit pack {}", index + 1),
                    ratio,
                    &Value::Null,
                    None,
                    spent,
                    total,
                    Some("credits"),
                    "balance",
                    remaining,
                    Some(expiry),
                    None,
                    None,
                ));
            }
            if items.iter().all(|item| {
                item.is_object()
                    && (api_number(&item["totalCredits"]).is_some()
                        || api_number(&item["remainingCredits"]).is_some()
                        || !reset(&item["endTime"]).is_null())
            }) {
                windows.push(window_full(
                    "addon-listed-packs",
                    "Listed active credit packs",
                    None,
                    &Value::Null,
                    None,
                    None,
                    None,
                    Some("packs"),
                    "balance",
                    Some(items.len() as f64),
                    None,
                    None,
                    None,
                ));
            }
        }
    }
    valid(plan, windows)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn m0_plan_parser_vectors_equal_or_delegate() {
        let table: Value =
            serde_json::from_str(include_str!("../../../../tests/spec/parser-vectors.json"))
                .expect("frozen vectors");
        let mut checked = 0;
        for case in table["cases"].as_array().expect("cases") {
            if case["module"] != "plan_common" {
                continue;
            }
            let actual = proof::scope(|| {
                if !proof::proven(&case["input"]) {
                    return proof::fallback();
                }
                match case["function"].as_str() {
                    Some("number" | "percent") => json!(number(&case["input"])),
                    Some("reset_at") => reset(&case["input"]),
                    Some("retry_after_seconds") => json!(super::super::retry(
                        case["input"].as_str(),
                        table["nowUnixSeconds"].as_f64().expect("clock")
                    )),
                    _ => proof::fallback(),
                }
            });
            if case["disagreement"] == true {
                assert_eq!(actual, proof::fallback());
            }
            assert!(
                actual == proof::fallback()
                    || actual == case["python"]
                    || actual
                        .as_f64()
                        .zip(case["python"].as_f64())
                        .is_some_and(|(a, b)| a == b),
                "vector {case}: {actual}"
            );
            checked += 1;
        }
        assert_eq!(checked, 43);
    }
}

#[cfg(test)]
mod zai_feature_code_tests {
    use super::*;
    // Final Astra max MED-1: plan_usage.py raises on an unhashable `modelCode` in the first 30
    // TIME_LIMIT details (a collector error), so Rust must delegate there and only there.
    fn payload(kind: &str, details: Value) -> Value {
        json!({"success": true, "code": 200, "data": {"level": "pro", "limits": [{
            "type": kind, "number": 5, "unit": 3, "usage": 100, "currentValue": 25,
            "nextResetTime": "2026-01-01T00:00:00Z", "usageDetails": details}]}})
    }
    fn at(index: usize, code: Value) -> Value {
        let mut details: Vec<Value> = (0..index)
            .map(|_| json!({"modelCode": "other", "usage": 1}))
            .collect();
        details.push(json!({"modelCode": code, "usage": 3}));
        Value::Array(details)
    }
    fn served(v: &Value) -> bool {
        *v != proof::fallback() && v["windows"].is_array()
    }
    #[test]
    fn unhashable_codes_in_the_first_thirty_time_limit_details_delegate() {
        for code in [
            json!([]),
            json!({}),
            json!(["search-prime"]),
            json!({"name": "search-prime"}),
        ] {
            for index in [0, 29] {
                let out = proof::scope(|| zai(&payload("TIME_LIMIT", at(index, code.clone()))));
                assert_eq!(out, proof::fallback(), "{code} at {index}");
            }
            // Python reads only details[:30], and only on TIME_LIMIT.
            let past = proof::scope(|| zai(&payload("TIME_LIMIT", at(30, code.clone()))));
            assert!(served(&past), "{code} at 30: {past}");
            let tokens = proof::scope(|| zai(&payload("TOKENS_LIMIT", at(0, code.clone()))));
            assert!(served(&tokens), "{code} on TOKENS_LIMIT: {tokens}");
        }
    }
    #[test]
    fn scalar_codes_and_non_object_details_are_still_served() {
        for details in [
            at(0, json!("unknown")),
            at(0, json!("search-prime")),
            at(0, Value::Null),
            at(0, json!(7)),
            json!([[]]),
            json!([["search-prime"]]),
        ] {
            let out = proof::scope(|| zai(&payload("TIME_LIMIT", details.clone())));
            assert!(served(&out), "{details}: {out}");
        }
    }
}

#[cfg(test)]
mod rounding_tests {
    // Exact binary ties and neighbours are written out in full on purpose.
    #![allow(clippy::excessive_precision)]
    /// CPython's own answers (tests/spec/agent/make-round-table.py) through the window the collector builds: every
    /// sampled exact tie at eight places, both neighbouring doubles of each, the review's Kimi witnesses and random
    /// values. Scaling by 1e8 and rounding half away from zero missed 136 of these rows. The rounding itself is
    /// `aac_num::py::round` (crates/aac-num/tests/corpus.rs holds the wider CPython corpus).
    #[test]
    fn remaining_percent_rounds_exactly_as_python_round() {
        let table: serde_json::Value = serde_json::from_str(include_str!(
            "../../../../tests/spec/agent/python-round8.json"
        ))
        .expect("python-round8.json");
        let rows = table["rows"].as_array().expect("rows");
        assert!(rows.len() > 1000);
        for row in rows {
            let x: f64 = row[0].as_str().expect("x").parse().expect("x parses");
            let want: f64 = row[1].as_str().expect("y").parse().expect("y parses");
            let got = aac_num::py::round(x, 8).expect("finite");
            assert_eq!(got.to_bits(), want.to_bits(), "round({x}, 8)");
        }
        // The Kimi witness through the window the collector builds (3 of 2048 used).
        let null = serde_json::Value::Null;
        let w = super::window(
            "k",
            "l",
            Some(0.146484375),
            &null,
            None,
            None,
            None,
            None,
            &null,
        );
        assert_eq!(w["remainingPercent"], serde_json::json!(99.85351562));
    }

    /// `plan_common.reset_at` on a fractional timestamp: Python rounds to the microsecond first, so a time within half
    /// a microsecond of the next second is that second (the old floor wrote the second before).
    #[test]
    fn reset_seconds_round_like_fromtimestamp() {
        let at = |v: serde_json::Value| super::reset(&v);
        assert_eq!(
            at(serde_json::json!(1767225599.9999996)),
            serde_json::json!("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            at(serde_json::json!(1767225599.9999994)),
            serde_json::json!("2025-12-31T23:59:59Z")
        );
        assert_eq!(
            at(serde_json::json!(1767225599999.9996)),
            serde_json::json!("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            at(serde_json::json!(1767225600000_i64)),
            serde_json::json!("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            at(serde_json::json!("2026-01-01T00:00:00Z")),
            serde_json::json!("2026-01-01T00:00:00Z")
        );
        assert_eq!(
            at(serde_json::json!("0000-01-01T00:00:00Z")),
            serde_json::Value::Null
        );
        assert_eq!(at(serde_json::json!(946684799)), serde_json::Value::Null);
    }
}

#[cfg(test)]
mod packs_clock_tests {
    //! Wonder L13: an out-of-domain clock declines the whole verb instead of comparing against "".
    use super::*;

    fn payload() -> Value {
        json!({
            "success": true,
            "code": 0,
            "data": {
                "fiveHourResets": [{"expireTime": "2026-11-16T04:00:00Z", "available": true}],
                "weekResets": [{"expireTime": "2026-11-20T04:00:00Z", "available": false}]
            }
        })
    }

    #[test]
    fn an_out_of_domain_clock_declines_the_pack_inventory() {
        // Year 10000 and year 0: `iso_seconds` has no answer there.
        for now in [253_402_300_800_i64, -62_167_219_200_i64] {
            let outcome = proof::scope(|| packs(&payload(), now));
            assert_eq!(outcome, proof::fallback(), "now {now}");
        }
    }

    #[test]
    fn an_in_domain_clock_keeps_the_pack_windows() {
        let outcome = proof::scope(|| packs(&payload(), 1_763_200_000));
        let windows = outcome.as_array().expect("pack windows");
        assert_eq!(windows.len(), 2);
        assert_eq!(windows[0]["key"], json!("reset-packs-5h"));
        assert_eq!(windows[0]["remaining"], json!(1));
        assert_eq!(windows[1]["key"], json!("reset-packs-weekly"));
    }
}
