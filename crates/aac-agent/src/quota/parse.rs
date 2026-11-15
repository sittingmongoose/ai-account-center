use super::{error, proof};
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
/// `plan_common.reset_at`: a positive number (milliseconds above 1e11) read by `datetime.fromtimestamp`, which
/// rounds to the microsecond before the second is written, or the one raw ISO shape proved against it; then
/// years 2000 to 2200 only. Both readings are aac-num's (`py`).
fn reset(v: &Value) -> Value {
    if v.is_null() || v.is_boolean() {
        return Value::Null;
    }
    let instant = if v.is_number()
        || v.as_str()
            .is_some_and(|s| s.bytes().all(|c| c.is_ascii_digit()))
    {
        number(v).filter(|n| *n > 0.).and_then(|n| {
            aac_num::py::iso_seconds_of_timestamp(if n > 100_000_000_000. { n / 1000. } else { n })
        })
    } else if let Some(s) = v.as_str() {
        match aac_num::py::parse_z_seconds(s) {
            // Year 0 parses but has no Python word: it is then no time, as Python's ValueError gives.
            Some((seconds, year)) => aac_num::py::iso_seconds(seconds).map(|word| (word, year)),
            None => {
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
    let mut w = json!({"key":key,"label":label,"usedPercent":ratio,"remainingPercent":ratio.map(|n| aac_num::py::round((100.-n).max(0.), 8).unwrap_or_else(|| { proof::reject(); n })),"resetAt":reset(reset_raw),"windowMinutes":minutes,"used":used,"limit":limit,"unit":unit,"kind":"rate_limit"});
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
        // plan_usage.py `"{}".format(count)` on a float: its repr ("5.0").
        let mut key = format!("{meter}-{unit}-{}", aac_num::py::repr(n));
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
    let current = aac_num::py::iso_seconds(now).unwrap_or_default();
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
