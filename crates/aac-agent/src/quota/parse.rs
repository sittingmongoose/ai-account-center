use super::{error, proof};
use chrono::Datelike;
use serde_json::{Value, json};
fn number(v: &Value) -> Option<f64> {
    let n = match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            if s.contains('.') || s != proof::strip(s) {
                proof::reject();
                return None;
            }
            let s = proof::strip(s);
            if !proof::decimal(s) {
                return None;
            }
            s.parse().ok()
        }
        _ => None,
    }?;
    (n.is_finite() && n >= 0.).then_some(n)
}
fn reset(v: &Value) -> Value {
    if v.is_null() || v.is_boolean() {
        return Value::Null;
    }
    let d = if v.is_number()
        || v.as_str()
            .is_some_and(|s| s.bytes().all(|c| c.is_ascii_digit()))
    {
        number(v).filter(|n| *n > 0.).and_then(|n| {
            chrono::DateTime::from_timestamp(
                (if n > 100_000_000_000. { n / 1000. } else { n }).floor() as i64,
                0,
            )
        })
    } else if let Some(s) = v.as_str() {
        // Only this raw ISO shape is proved against plan_common.reset_at.
        let b = s.as_bytes();
        if b.len() != 20
            || b[4] != b'-'
            || b[7] != b'-'
            || b[10] != b'T'
            || b[13] != b':'
            || b[16] != b':'
            || b[19] != b'Z'
            || b.iter()
                .enumerate()
                .any(|(i, c)| ![4, 7, 10, 13, 16, 19].contains(&i) && !c.is_ascii_digit())
            || b[17] > b'5'
        {
            proof::reject();
            return Value::Null;
        }
        match chrono::DateTime::parse_from_rfc3339(s) {
            Ok(d) => Some(d.to_utc()),
            Err(_) => {
                proof::reject();
                None
            }
        }
    } else {
        None
    };
    d.filter(|d| (2000..=2200).contains(&d.year()))
        .map(|d| json!(d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)))
        .unwrap_or(Value::Null)
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
    let mut w = json!({"key":key,"label":label,"usedPercent":ratio,"remainingPercent":ratio.map(|n| ((100.-n).max(0.) * 1e8).round()/1e8),"resetAt":reset(reset_raw),"windowMinutes":minutes,"used":used,"limit":limit,"unit":unit,"kind":"rate_limit"});
    if !remaining.is_null() {
        w["remaining"] = json!(number(remaining));
    }
    w
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
        let ratio = number(&item["percent"]);
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
    let limit = number(&d["limit"]);
    let mut used = number(&d["used"]);
    let left = number(&d["remaining"]);
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
    // Wallet and transitional modern/legacy overlap require their own proof table.
    if p.get("boosterWallet").is_some()
        || (p["usages"].is_object() && (p["usage"].is_object() || p["limits"].is_array()))
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
            let minutes = number(&w["duration"]).zip(multiplier).map(|(d, m)| d * m);
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
        let ratio = number(&item["used_ratio"]).filter(|n| *n <= 1.);
        if item.is_object() && (ratio.is_some() || !reset(&item["reset_time"]).is_null()) {
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
    valid(json!(plan), windows)
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
        let count = number(&item["number"]);
        let unit = item["unit"].as_i64();
        // Python hashes a float's spelling into its public key. Prove only small integral counts.
        if count.is_none_or(|n| n.fract() != 0. || n > 999999.) || unit.is_none() {
            return proof::fallback();
        }
        let n = count.unwrap_or(0.);
        let unit = unit.unwrap_or(0);
        let monthly = kind == "Requests" && unit == 5 && n == 1.;
        let multiplier = match unit {
            1 => Some(1440.),
            3 => Some(60.),
            6 => Some(10080.),
            _ => None,
        };
        let minutes = if monthly {
            None
        } else {
            multiplier.map(|m| n * m)
        };
        let duration = match minutes {
            Some(300.) => "5 hours".into(),
            Some(10080.) => "Weekly".into(),
            _ if monthly => "Monthly".into(),
            _ => match unit {
                1 => format!("{n} days"),
                3 => format!("{n} hours"),
                6 => format!("{n} weeks"),
                _ => "Plan limit".into(),
            },
        };
        let meter = item["type"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .replace("_limit", "");
        let mut key = format!("{meter}-{unit}-{n:.1}");
        if !seen.insert(key.clone()) {
            key.push_str(&format!("-{index}"));
        }
        let used = number(&item["currentValue"]);
        let limit = number(&item["usage"]);
        let ratio = used
            .zip(limit.filter(|l| *l > 0.))
            .map(|(u, l)| 100. * u / l)
            .or_else(|| number(&item["percentage"]));
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
                let label = match d["modelCode"].as_str() {
                    Some("search-prime") => "Search",
                    Some("web-reader") => "Web reader",
                    Some("zread") => "Zread",
                    _ => continue,
                };
                if let Some(u) = number(&d["usage"]) {
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
        Value::Null
    }
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
    let current = chrono::DateTime::from_timestamp(now, 0)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
        .unwrap_or_default();
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

fn unwrap_qwen(p: &Value) -> Result<Value, Value> {
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
        if [
            "per5HourPercentage",
            "per1WeekPercentage",
            "per1MonthPercentage",
            "per5HourResetTime",
            "per1WeekResetTime",
            "per1MonthResetTime",
        ]
        .iter()
        .any(|k| v.get(k).is_some())
        {
            return Ok(v);
        }
        let next = v
            .get("DataV2")
            .or_else(|| v.get("Data"))
            .or_else(|| v.get("data"))
            .cloned()
            .unwrap_or(Value::Null);
        v = if let Some(s) = next.as_str() {
            proof::json(s.as_bytes(), super::CAP, false).unwrap_or(Value::Null)
        } else {
            next
        };
    }
    Err(error(
        "error",
        "The Qwen console returned no valid token-plan quota response.",
    ))
}
pub(super) fn qwen(payloads: &[Value]) -> Value {
    let data = match unwrap_qwen(&payloads[0]) {
        Ok(data) => data,
        Err(failure) => return failure,
    };
    // Optional subscription/config/pack layouts delegate as a unit until proven.
    if payloads[1..].iter().any(|p| !p.is_null()) {
        return proof::fallback();
    }
    let mut windows = Vec::new();
    for (prefix, key, label, minutes) in [
        ("per5Hour", "5h", "5 hours", Some(300.)),
        ("per1Week", "weekly", "Weekly", Some(10080.)),
        ("per1Month", "monthly", "Monthly", None),
    ] {
        let ratio = number(&data[format!("{prefix}Percentage")]);
        let r = &data[format!("{prefix}ResetTime")];
        if ratio.is_none() && reset(r).is_null() {
            continue;
        }
        windows.push(window(
            key,
            label,
            ratio.map(|n| if n <= 1. { n * 100. } else { n }),
            r,
            minutes,
            None,
            None,
            None,
            &Value::Null,
        ));
    }
    valid(Value::Null, windows)
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
