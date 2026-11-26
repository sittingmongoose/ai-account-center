//! Claude quota normalization, ported from `claude_usage.py` (normalize_windows, the
//! limits[] reader, reset grants, the weekly session reset, prepaid balance, plan names and
//! the private reset buckets). Provider bodies never leave as text: only these bounded windows.
use regex::Regex;
use serde_json::{Map, Number, Value, json};
use std::sync::LazyLock;

pub(crate) const MAX_RESET_GRANTS: usize = 20;
pub(crate) const MAX_SCOPED_LIMITS: usize = 64;
const MAX_WINDOW_KEY_LEN: usize = 64;
const MAX_WINDOW_LABEL_LEN: usize = 80;
const LEGACY: [(&str, &str, i64); 6] = [
    ("five_hour", "Five-hour usage", 300),
    ("seven_day", "Weekly usage", 10080),
    ("seven_day_opus", "Weekly Opus usage", 10080),
    ("seven_day_sonnet", "Weekly Sonnet usage", 10080),
    ("seven_day_oauth_apps", "Weekly OAuth app usage", 10080),
    ("seven_day_cowork", "Weekly Cowork usage", 10080),
];

macro_rules! re {
    ($name:ident, $pattern:literal) => {
        static $name: LazyLock<Regex> =
            LazyLock::new(|| Regex::new($pattern).expect("static pattern"));
    };
}
re!(KEY, r"\A[a-z0-9][a-z0-9_]{0,63}\z");
re!(KIND, r"\A[a-z0-9_]+\z");
re!(SEVEN_DAY, r"\Aseven_day_[a-z0-9_]+\z");
re!(CURRENCY, r"\A[A-Z]{3}\z");
re!(CLEAR, r"\A[a-z0-9_]{1,40}\z");
re!(SAFE_FLAG, r"\Ahas_claude_[a-z0-9_]{1,40}\z");
re!(
    PLAN_CONTROL,
    r"[\x00-\x1f\x7f-\x9f\x{061c}\x{200e}\x{200f}\x{2028}-\x{202e}\x{2066}-\x{2069}\x{feff}]"
);
re!(
    PLAN_SECRET,
    r"(?i)(bearer\s|dca:|sk[-_]|eyJ[A-Za-z0-9_-]{8})"
);
re!(
    PLAN_MAX,
    r"(?i)(?:^|[\s/_-])max(?:\s*(?:5|20)x)?(?:$|[\s/_-])"
);
re!(PLAN_PRO, r"(?i)(?:^|[\s/_-])pro(?:$|[\s/_-])");
re!(TIER_SPLIT, r"[^a-z0-9]+");
re!(SLUG, r"[^a-z0-9]+");

/// Python's `str.isspace` for `strip()`.
pub(crate) fn py_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}
pub(crate) fn py_strip(s: &str) -> &str {
    s.trim_matches(py_space)
}

/// `number`: a finite, nonnegative JSON number (never a bool), its int/float kind kept.
pub(crate) fn number(v: &Value) -> Option<&Number> {
    match v {
        Value::Number(n) if n.as_f64().is_some_and(|f| f.is_finite() && f >= 0.0) => Some(n),
        _ => None,
    }
}
fn int(n: &Number) -> Option<i128> {
    n.as_i64()
        .map(i128::from)
        .or_else(|| n.as_u64().map(i128::from))
}
fn is_int(n: &Number) -> bool {
    n.is_i64() || n.is_u64()
}
fn f(n: &Number) -> f64 {
    n.as_f64().unwrap_or(0.0)
}
/// Python's `max(0, 100 - used)`: an int stays an int; a nonpositive result is the int 0.
fn remaining(used: &Number) -> Value {
    if is_int(used) {
        let v = 100i128 - int(used).unwrap_or(i128::MAX);
        json!(v.max(0) as i64)
    } else {
        let v = 100.0 - f(used);
        if v <= 0.0 { json!(0) } else { json!(v) }
    }
}
/// Python equality on JSON values: `1 == 1.0`.
pub(crate) fn py_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            if is_int(x) && is_int(y) {
                int(x) == int(y)
            } else {
                f(x) == f(y)
            }
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(a, b)| py_eq(a, b))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => a == b,
    }
}

/// `datetime.fromisoformat(value.replace("Z", "+00:00"))` as UTC microseconds, aware values only.
pub(crate) fn iso_micros(value: &str) -> Option<i64> {
    let text = value.replace('Z', "+00:00");
    let b = text.as_bytes();
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = text.get(from..from + len)?;
        part.bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| part.parse().ok())?
    };
    let (year, month, day, mut i) = if b.get(4) == Some(&b'-') {
        if b.get(7) != Some(&b'-') {
            return None;
        }
        (digits(0, 4)?, digits(5, 2)?, digits(8, 2)?, 10)
    } else {
        (digits(0, 4)?, digits(4, 2)?, digits(6, 2)?, 8)
    };
    if !(1..=12).contains(&month)
        || day < 1
        || day > aac_num::civil::days_in_month(year, month)
        || year < 1
    {
        return None;
    }
    if !matches!(b.get(i), Some(b'T' | b't' | b' ')) {
        return None;
    }
    i += 1;
    let hour = digits(i, 2)?;
    i += 2;
    let (mut minute, mut second, mut micro) = (0, 0, 0);
    let extended = b.get(i) == Some(&b':');
    let field = |i: &mut usize| -> Option<Option<i64>> {
        if extended {
            if b.get(*i) != Some(&b':') {
                return Some(None);
            }
            *i += 1;
        } else if !b.get(*i).is_some_and(u8::is_ascii_digit) {
            return Some(None);
        }
        let v = digits(*i, 2)?;
        *i += 2;
        Some(Some(v))
    };
    if let Some(m) = field(&mut i)? {
        minute = m;
        if let Some(s) = field(&mut i)? {
            second = s;
            if matches!(b.get(i), Some(b'.' | b',')) {
                let start = i + 1;
                let mut end = start;
                while b.get(end).is_some_and(u8::is_ascii_digit) {
                    end += 1;
                }
                // Python 3.11+ also reads an empty fraction ("...:00.Z") as zero.
                let frac = &text[start..end.min(start + 6)];
                micro = format!("{frac:0<6}").parse().ok()?;
                i = end;
            }
        }
    }
    let mut offset = None;
    if i < b.len() {
        let sign = match b[i] {
            b'+' => 1,
            b'-' => -1,
            _ => return None,
        };
        let oh = digits(i + 1, 2)?;
        let mut j = i + 3;
        let mut om = 0;
        let mut os = 0;
        let mut ou = 0;
        if b.get(j) == Some(&b':') {
            om = digits(j + 1, 2)?;
            j += 3;
            if b.get(j) == Some(&b':') {
                os = digits(j + 1, 2)?;
                j += 3;
                if b.get(j) == Some(&b'.') {
                    let start = j + 1;
                    let mut end = start;
                    while b.get(end).is_some_and(u8::is_ascii_digit) {
                        end += 1;
                    }
                    if end - start != 6 {
                        return None;
                    }
                    ou = text[start..end].parse().ok()?;
                    j = end;
                }
            }
        } else if j < b.len() {
            om = digits(j, 2)?;
            j += 2;
        }
        if j != b.len() || om > 59 || os > 59 {
            return None;
        }
        let total = (oh * 3600 + om * 60 + os) * 1_000_000 + ou;
        if total >= 86_400_000_000 {
            return None;
        }
        offset = Some(sign * total);
    }
    let offset = offset?;
    let (hour, extra_day) = if hour == 24 && minute == 0 && second == 0 && micro == 0 {
        (0, 1)
    } else {
        (hour, 0)
    };
    if hour > 23 || minute > 59 || second > 59 {
        return None;
    }
    let days = aac_num::civil::days_from_civil(year, month, day) + extra_day;
    Some((days * 86_400 + hour * 3600 + minute * 60 + second) * 1_000_000 + micro - offset)
}

/// `reset_at`: an aware ISO timestamp from 2000 to 2200, as UTC with milliseconds.
pub(crate) fn reset_at(v: &Value) -> Value {
    let Some(s) = v.as_str().filter(|s| s.chars().count() <= 64) else {
        return Value::Null;
    };
    let Some(micros) = iso_micros(s) else {
        return Value::Null;
    };
    // The year check is on the parsed (local) value; offsets keep it inside the same range
    // except within a day of the edges, where Python's own check also reads the local year.
    let local_year = {
        let t = s.replace('Z', "+00:00");
        t.get(0..4).and_then(|y| y.parse::<i64>().ok())
    };
    if !local_year.is_some_and(|y| (2000..=2200).contains(&y)) {
        return Value::Null;
    }
    Value::String(iso_ms(micros.div_euclid(1000)))
}
/// Epoch milliseconds as `YYYY-MM-DDTHH:MM:SS.mmmZ`.
pub(crate) fn iso_ms(ms: i64) -> String {
    let seconds = ms.div_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (y, m, d) = aac_num::civil::civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        ms.rem_euclid(1000)
    )
}
pub(crate) fn reset_ms(v: &Value) -> Option<i64> {
    iso_micros(v.as_str()?).map(|m| m.div_euclid(1000))
}

fn slug(value: &str) -> Option<String> {
    let lower = value.to_lowercase();
    let replaced = SLUG.replace_all(&lower, "_");
    let trimmed: String = replaced.trim_matches('_').chars().take(40).collect();
    let out = trimmed.trim_end_matches('_').to_owned();
    (!out.is_empty()).then_some(out)
}
fn capitalize(part: &str) -> String {
    let mut chars = part.chars();
    match chars.next() {
        Some(first) => first
            .to_uppercase()
            .chain(chars.flat_map(char::to_lowercase))
            .collect(),
        None => String::new(),
    }
}
fn words(slug: &str) -> String {
    slug.split('_')
        .filter(|p| !p.is_empty())
        .map(capitalize)
        .collect::<Vec<_>>()
        .join(" ")
}
fn group_minutes(group: &Value) -> Value {
    match group.as_str() {
        Some("weekly") => json!(10080),
        Some("session") => json!(300),
        _ => Value::Null,
    }
}
/// `_plan_name`: bounded plain text, never a control or credential-shaped string.
pub(crate) fn plan_name(v: &Value) -> Option<String> {
    let name = py_strip(v.as_str()?);
    let len = name.chars().count();
    if !(1..=80).contains(&len) || PLAN_CONTROL.is_match(name) || PLAN_SECRET.is_match(name) {
        return None;
    }
    Some(name.to_owned())
}
fn scoped_model(scope: Option<&Map<String, Value>>) -> Option<(String, String)> {
    let model = scope?.get("model")?.as_object()?;
    let display = plan_name(model.get("display_name").unwrap_or(&Value::Null))?;
    Some((slug(&display)?, display))
}
fn scoped_surface(scope: Option<&Map<String, Value>>) -> Option<(String, String)> {
    let surface = scope?.get("surface")?;
    let candidates: Vec<&Value> = match surface {
        Value::Object(o) => vec![
            o.get("id").unwrap_or(&Value::Null),
            o.get("display_name").unwrap_or(&Value::Null),
        ],
        Value::String(_) => vec![surface],
        _ => vec![],
    };
    for raw in candidates {
        let Some(name) = plan_name(raw) else {
            continue;
        };
        let Some(s) = slug(&name) else {
            continue;
        };
        let w = words(&s);
        return Some((s, w));
    }
    None
}
fn bounds_ok(key: &str, label: &str) -> bool {
    key.chars().count() <= MAX_WINDOW_KEY_LEN
        && KEY.is_match(key)
        && label.chars().count() <= MAX_WINDOW_LABEL_LEN
}
fn weekly_scoped_allowed(kind: &Value, group: &Value) -> bool {
    if group != "weekly" {
        return false;
    }
    match kind.as_str() {
        Some("weekly_scoped") => true,
        Some("session" | "weekly_all") => false,
        Some(k) => KIND.is_match(k),
        None => false,
    }
}
fn model_scope_ok(scope: Option<&Map<String, Value>>) -> bool {
    let Some(model) = scope.and_then(|s| s.get("model")) else {
        return true;
    };
    if model.is_null() {
        return true;
    }
    let Some(model) = model.as_object() else {
        return false;
    };
    if model.keys().any(|k| k != "id" && k != "display_name") {
        return false;
    }
    model
        .get("id")
        .is_none_or(|id| id.is_null() || id.is_string())
}
pub(crate) fn reset_unscoped(scope: &Value) -> bool {
    match scope {
        Value::Null => true,
        Value::Object(o) => {
            o.keys().all(|k| k == "model" || k == "surface") && o.values().all(Value::is_null)
        }
        _ => false,
    }
}
fn rate_window(
    key: &str,
    label: &str,
    used: &Number,
    reset: Value,
    expires: Value,
    minutes: Value,
) -> Value {
    json!({"key":key,"label":label,"kind":"rate_limit","usedPercent":used,"remainingPercent":remaining(used),
        "resetAt":reset,"expiresAt":expires,"windowMinutes":minutes,"used":null,"limit":null,"unit":null})
}
fn limits_window(entry: &Map<String, Value>, used: &Number) -> Option<Value> {
    let null = Value::Null;
    let kind = entry.get("kind").unwrap_or(&null);
    let group = entry.get("group").unwrap_or(&null);
    let window = |key: &str, label: &str, minutes: Value| {
        rate_window(
            key,
            label,
            used,
            reset_at(entry.get("resets_at").unwrap_or(&null)),
            Value::Null,
            minutes,
        )
    };
    let raw_scope = entry.get("scope").unwrap_or(&null);
    let scope = raw_scope.as_object();
    if !model_scope_ok(scope) {
        return None;
    }
    if kind == "session" {
        if !raw_scope.is_null() && !raw_scope.is_object() {
            return None;
        }
        let unscoped = match scope {
            None => true,
            Some(_) => reset_unscoped(raw_scope),
        };
        return unscoped.then(|| window("five_hour", "Five-hour usage", json!(300)));
    }
    if kind == "weekly_all" {
        return Some(window("seven_day", "Weekly usage", json!(10080)));
    }
    let model = scoped_model(scope);
    let surface = scoped_surface(scope);
    let minutes = if kind != "weekly_scoped" {
        group_minutes(group)
    } else if group == "weekly" {
        json!(10080)
    } else {
        Value::Null
    };
    if let Some((model_slug, display)) = model {
        if !weekly_scoped_allowed(kind, group) {
            return None;
        }
        let mut key = format!("seven_day_{model_slug}");
        let mut label = format!("Weekly {display} usage");
        if let Some((s, w)) = surface {
            key.push('_');
            key.push_str(&s);
            label.push_str(&format!(" ({w})"));
        }
        return bounds_ok(&key, &label).then(|| window(&key, &label, minutes));
    }
    if let Some((s, w)) = surface {
        if !weekly_scoped_allowed(kind, group) {
            return None;
        }
        let key = format!("seven_day_{s}");
        let label = format!("Weekly {w} usage");
        return bounds_ok(&key, &label).then(|| window(&key, &label, minutes));
    }
    if kind == "weekly_scoped" {
        return None;
    }
    let kind = kind.as_str().filter(|k| KIND.is_match(k))?;
    let mut parts: Vec<&str> = kind.split('_').collect();
    while parts.last().is_some_and(|p| *p == "all" || *p == "scoped") {
        parts.pop();
    }
    if parts.is_empty() {
        return None;
    }
    let label = format!(
        "{} usage",
        parts
            .iter()
            .map(|p| capitalize(p))
            .collect::<Vec<_>>()
            .join(" ")
    );
    bounds_ok(kind, &label).then(|| window(kind, &label, minutes))
}
fn normalize_limits(usage: &Map<String, Value>) -> Vec<Value> {
    let Some(limits) = usage
        .get("limits")
        .and_then(Value::as_array)
        .filter(|l| l.len() <= MAX_SCOPED_LIMITS)
    else {
        return vec![];
    };
    let mut windows: Vec<(String, Option<Value>)> = vec![];
    for entry in limits {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let Some(used) = number(entry.get("percent").unwrap_or(&Value::Null)) else {
            continue;
        };
        let Some(candidate) = limits_window(entry, used) else {
            continue;
        };
        let key = candidate["key"].as_str().unwrap_or("").to_owned();
        if let Some((_, slot)) = windows.iter_mut().find(|(k, _)| *k == key) {
            if slot.as_ref().is_some_and(|w| !py_eq(w, &candidate)) {
                *slot = None;
            }
            continue;
        }
        windows.push((key, Some(candidate)));
    }
    windows.into_iter().filter_map(|(_, w)| w).collect()
}

/// `normalize_windows`: legacy windows, future `seven_day_*` keys, limits[], extra usage.
pub(crate) fn normalize_windows(usage: &Value) -> Vec<Value> {
    let Some(usage) = usage.as_object() else {
        return vec![];
    };
    let null = Value::Null;
    let mut windows = vec![];
    let mut seen: Vec<String> = vec![];
    for (key, label, minutes) in LEGACY {
        let Some(value) = usage.get(key).and_then(Value::as_object) else {
            continue;
        };
        let Some(used) = number(value.get("utilization").unwrap_or(&null)) else {
            continue;
        };
        windows.push(rate_window(
            key,
            label,
            used,
            reset_at(value.get("resets_at").unwrap_or(&null)),
            reset_at(value.get("expires_at").unwrap_or(&null)),
            json!(minutes),
        ));
        seen.push(key.into());
    }
    let mut keys: Vec<&String> = usage.keys().collect();
    keys.sort();
    for key in keys {
        if seen.contains(key) || LEGACY.iter().any(|(k, _, _)| k == key) || !SEVEN_DAY.is_match(key)
        {
            continue;
        }
        let Some(value) = usage[key].as_object() else {
            continue;
        };
        let Some(used) = number(value.get("utilization").unwrap_or(&null)) else {
            continue;
        };
        let label = format!("Weekly {} usage", words(&key["seven_day_".len()..]));
        if !bounds_ok(key, &label) {
            continue;
        }
        windows.push(rate_window(
            key,
            &label,
            used,
            reset_at(value.get("resets_at").unwrap_or(&null)),
            reset_at(value.get("expires_at").unwrap_or(&null)),
            json!(10080),
        ));
        seen.push(key.clone());
    }
    for window in normalize_limits(usage) {
        let key = window["key"].as_str().unwrap_or("").to_owned();
        if !seen.contains(&key) {
            windows.push(window);
            seen.push(key);
        }
    }
    if let Some(extra) = usage.get("extra_usage").and_then(Value::as_object)
        && let Some(enabled) = extra.get("is_enabled").and_then(Value::as_bool)
    {
        let get = |k: &str| extra.get(k).unwrap_or(&null);
        let mut used = number(get("used_credits")).map(|n| json!(n));
        let mut limit = number(get("monthly_limit")).map(|n| json!(n));
        let utilization = number(get("utilization"));
        let currency = get("currency").as_str().filter(|c| CURRENCY.is_match(c));
        if currency.is_some() {
            used = used.map(|u| json!(u.as_f64().unwrap_or(0.0) / 100.0));
            limit = limit.map(|l| json!(l.as_f64().unwrap_or(0.0) / 100.0));
        }
        let left = match (&used, &limit) {
            (Some(u), Some(l)) => Some(sub_floor0(l, u)),
            _ => None,
        };
        let unit = currency.map(|c| json!(c)).unwrap_or_else(|| {
            if used.is_some() || limit.is_some() {
                json!("credits")
            } else {
                Value::Null
            }
        });
        let mut window = json!({"key":"extra_usage","label":"Extra usage","kind":"extra_usage",
            "usedPercent":utilization,"remainingPercent":utilization.map(remaining),
            "resetAt":reset_at(get("resets_at")),"expiresAt":reset_at(get("expires_at")),
            "windowMinutes":null,"used":used,"limit":limit,"remaining":left,"unit":unit,"enabled":enabled});
        if let Some(unlimited) = get("is_unlimited").as_bool() {
            window["unlimited"] = json!(unlimited);
        }
        windows.push(window);
    }
    windows
        .into_iter()
        .filter(|w| match (w["key"].as_str(), w["label"].as_str()) {
            (Some(k), Some(l)) => bounds_ok(k, l),
            _ => false,
        })
        .collect()
}
/// Python's `max(0, a - b)` on two numbers, int kept when both are ints.
fn sub_floor0(a: &Value, b: &Value) -> Value {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) if is_int(x) && is_int(y) => {
            json!((int(x).unwrap_or(0) - int(y).unwrap_or(0)).max(0) as i64)
        }
        _ => {
            let v = a.as_f64().unwrap_or(0.0) - b.as_f64().unwrap_or(0.0);
            if v <= 0.0 { json!(0) } else { json!(v) }
        }
    }
}

pub(crate) fn balance_window(
    key: &str,
    label: &str,
    remaining: Value,
    unit: &str,
    limit: Value,
    used: Value,
    expires: Value,
) -> Value {
    json!({"key":key,"label":label,"kind":"balance","usedPercent":null,"remainingPercent":null,
        "resetAt":null,"expiresAt":expires,"windowMinutes":null,"used":used,"limit":limit,
        "remaining":remaining,"unit":unit})
}
fn small_int(v: &Value, max: i64) -> Option<i64> {
    match v {
        Value::Number(n) if is_int(n) => n.as_i64().filter(|x| (0..=max).contains(x)),
        _ => None,
    }
}

/// `normalize_reset_credits(value, now)`: `now` in seconds. Malformed inventory is no inventory.
pub(crate) fn normalize_reset_credits(value: &Value, now: f64) -> Vec<Value> {
    let Some(obj) = value.as_object() else {
        return vec![];
    };
    if obj.get("eligible") != Some(&json!(true)) {
        return vec![];
    }
    let Some(list) = obj
        .get("grants")
        .and_then(Value::as_array)
        .filter(|g| g.len() <= MAX_RESET_GRANTS)
    else {
        return vec![];
    };
    let null = Value::Null;
    let mut grants = vec![];
    let mut available = 0i64;
    for (index, grant) in list.iter().enumerate() {
        let index = index + 1;
        let Some(grant) = grant.as_object() else {
            return vec![];
        };
        let get = |k: &str| grant.get(k).unwrap_or(&null);
        let Some(left) = small_int(get("resets_left"), 50) else {
            return vec![];
        };
        let total = get("resets_total");
        let total = if total.is_null() {
            None
        } else {
            match small_int(total, 50) {
                Some(t) if left <= t => Some(t),
                _ => return vec![],
            }
        };
        let Some(paused) = get("paused").as_bool() else {
            return vec![];
        };
        let (start, end) = (reset_at(get("starts_at")), reset_at(get("ends_at")));
        if (!get("starts_at").is_null() && start.is_null())
            || (!get("ends_at").is_null() && end.is_null())
        {
            return vec![];
        }
        let mut clears = None;
        if let Some(raw) = grant.get("clears") {
            let Some(items) = raw.as_array().filter(|a| a.len() <= 8) else {
                return vec![];
            };
            if items
                .iter()
                .any(|i| !i.as_str().is_some_and(|s| CLEAR.is_match(s)))
            {
                return vec![];
            }
            let mut unique: Vec<Value> = vec![];
            for item in items {
                if !unique.contains(item) {
                    unique.push(item.clone());
                }
            }
            clears = Some(unique);
        }
        let seconds = |v: &Value| reset_ms(v).map(|ms| ms as f64 / 1000.0).unwrap_or(f64::NAN);
        let started = start.is_null() || seconds(&start) <= now;
        let expired = !end.is_null() && seconds(&end) <= now;
        let usable = !paused && started && !expired && left > 0;
        if usable {
            available += left;
        }
        if available > 50 {
            return vec![];
        }
        let category = if left == 0 {
            "used"
        } else if expired {
            "expired"
        } else if usable {
            "available"
        } else {
            "saved"
        };
        let mut window = balance_window(
            &format!("reset_credit_{category}_grant_{index}"),
            &format!("{} rate-limit reset grant {index}", capitalize(category)),
            json!(left),
            "resets",
            total.map(|t| json!(t)).unwrap_or(Value::Null),
            total.map(|t| json!(t - left)).unwrap_or(Value::Null),
            end,
        );
        if let Some(c) = clears.filter(|c| !c.is_empty()) {
            window["clears"] = json!(c);
        }
        grants.push(window);
    }
    let mut out = vec![balance_window(
        "reset_credits_available",
        "Rate-limit resets available",
        json!(available),
        "resets",
        Value::Null,
        Value::Null,
        Value::Null,
    )];
    out.extend(grants);
    out
}

/// `normalize_session_reset_allowance` (juniper_tide).
pub(crate) fn normalize_session_reset_allowance(value: &Value) -> Vec<Value> {
    let Some(obj) = value.as_object() else {
        return vec![];
    };
    if obj.get("eligible") != Some(&json!(true)) {
        return vec![];
    }
    let Some(available) = obj.get("available").and_then(Value::as_bool) else {
        return vec![];
    };
    let Some(per_week) = obj
        .get("resets_per_week")
        .and_then(|v| small_int(v, 50))
        .filter(|n| *n >= 1)
    else {
        return vec![];
    };
    let left = if available { per_week } else { 0 };
    let mut window = balance_window(
        "session_reset_allowance",
        "Weekly session reset",
        json!(left),
        "resets",
        json!(per_week),
        json!(per_week - left),
        Value::Null,
    );
    window["resetAt"] = reset_at(obj.get("next_available_at").unwrap_or(&Value::Null));
    vec![window]
}

/// `normalize_prepaid`: the organization's prepaid balance (OAuth `prepaid/credits`).
pub(crate) fn normalize_prepaid(value: &Value) -> Vec<Value> {
    let Some(obj) = value.as_object() else {
        return vec![];
    };
    let Some(amount) = number(obj.get("amount").unwrap_or(&Value::Null)) else {
        return vec![];
    };
    let currency = obj.get("currency").unwrap_or(&Value::Null);
    if !currency.is_null() && !currency.as_str().is_some_and(|c| CURRENCY.is_match(c)) {
        return vec![];
    }
    let currency = currency.as_str();
    vec![balance_window(
        "prepaid_balance",
        if currency.is_some() {
            "Prepaid balance"
        } else {
            "Prepaid credit balance"
        },
        if currency.is_some() {
            json!(f(amount) / 100.0)
        } else {
            json!(amount)
        },
        currency.unwrap_or("credits"),
        Value::Null,
        Value::Null,
        reset_at(obj.get("next_expires_at").unwrap_or(&Value::Null)),
    )]
}

/// `claude_plan(account, organization, fallback)`.
pub(crate) fn claude_plan(
    account: &Value,
    organization: &Value,
    fallback: &Value,
) -> Option<String> {
    for source in [organization, account] {
        let Some(source) = source.as_object() else {
            continue;
        };
        for key in [
            "plan_display_name",
            "plan_display_label",
            "plan_type",
            "planType",
            "tier",
            "tier_name",
            "tierName",
            "plan_name",
            "planName",
        ] {
            if let Some(name) = source.get(key).and_then(plan_name) {
                return Some(if name == name.to_lowercase() && name.contains('_') {
                    name.replace('_', " ")
                } else {
                    name
                });
            }
        }
    }
    let empty = Map::new();
    let account = account.as_object().unwrap_or(&empty);
    let mut flags: Vec<String> = account
        .iter()
        .filter(|(k, v)| v.as_bool() == Some(true) && SAFE_FLAG.is_match(k))
        .map(|(k, _)| k["has_claude_".len()..].to_owned())
        .collect();
    flags.sort();
    flags.dedup();
    let names: Vec<String> = flags
        .iter()
        .map(|f| f.replace('_', " "))
        .filter(|n| plan_name(&json!(n)).is_some())
        .collect();
    if names.len() == 1 {
        return names.into_iter().next();
    }
    if account.get("has_claude_max") == Some(&json!(true)) {
        return Some("max".into());
    }
    if account.get("has_claude_pro") == Some(&json!(true)) {
        return Some("pro".into());
    }
    plan_name(fallback)
}

/// `claude_plan_class(account, organization, name)`.
pub(crate) fn claude_plan_class(
    account: &Value,
    organization: &Value,
    name: Option<&str>,
) -> Option<&'static str> {
    if let Some(tier) = organization.get("rate_limit_tier").and_then(Value::as_str) {
        let lower = tier.to_lowercase();
        let tokens: Vec<&str> = TIER_SPLIT.split(&lower).collect();
        if tokens.contains(&"max") {
            return Some("max");
        }
        if tokens.contains(&"pro") {
            return Some("pro");
        }
    }
    if account.get("has_claude_max") == Some(&json!(true)) {
        return Some("max");
    }
    if account.get("has_claude_pro") == Some(&json!(true)) {
        return Some("pro");
    }
    let name = name.filter(|n| !n.is_empty())?;
    if PLAN_MAX.is_match(name) {
        return Some("max");
    }
    if PLAN_PRO.is_match(name) {
        return Some("pro");
    }
    None
}

fn reset_limit_key(entry: &Map<String, Value>) -> Option<&'static str> {
    let null = Value::Null;
    let (kind, group, scope) = (
        entry.get("kind").unwrap_or(&null),
        entry.get("group").unwrap_or(&null),
        entry.get("scope").unwrap_or(&null),
    );
    if reset_unscoped(scope) {
        if kind == "session" && group == "session" {
            return Some("five_hour");
        }
        if kind == "weekly_all" && group == "weekly" {
            return Some("seven_day");
        }
    }
    let s = scope.as_object()?;
    (kind == "weekly_scoped"
        && group == "weekly"
        && s.keys().all(|k| k == "model" || k == "surface")
        && s.get("surface").is_none_or(Value::is_null)
        && s.get("model")
            .and_then(Value::as_object)
            .is_some_and(|m| m.get("display_name") == Some(&json!("Fable")))
        && model_scope_ok(Some(s)))
    .then_some("seven_day_overage_included")
}

/// `reset_windows`: the private refill buckets, provenance kept until bucket selection.
pub(crate) fn reset_windows(usage: &Value) -> Map<String, Value> {
    let mut windows = Map::new();
    let null = Value::Null;
    for key in ["five_hour", "seven_day", "seven_day_overage_included"] {
        let Some(value) = usage.get(key).and_then(Value::as_object) else {
            continue;
        };
        if !reset_unscoped(value.get("scope").unwrap_or(&null)) {
            continue;
        }
        if let Some(used) = number(value.get("utilization").unwrap_or(&null)) {
            windows.insert(key.into(), json!(used));
        }
    }
    let Some(limits) = usage
        .get("limits")
        .and_then(Value::as_array)
        .filter(|l| l.len() <= MAX_SCOPED_LIMITS)
    else {
        return windows;
    };
    let mut dynamic: Vec<(&'static str, Option<(Value, Value)>)> = vec![];
    for entry in limits {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        let (Some(key), Some(used)) = (
            reset_limit_key(entry),
            number(entry.get("percent").unwrap_or(&null)),
        ) else {
            continue;
        };
        if windows.contains_key(key) {
            continue;
        }
        let candidate = (
            json!(used),
            reset_at(entry.get("resets_at").unwrap_or(&null)),
        );
        if let Some((_, slot)) = dynamic.iter_mut().find(|(k, _)| *k == key) {
            if slot
                .as_ref()
                .is_some_and(|(u, r)| !(py_eq(u, &candidate.0) && r == &candidate.1))
            {
                *slot = None;
            }
        } else {
            dynamic.push((key, Some(candidate)));
        }
    }
    for (key, value) in dynamic {
        if let Some((used, _)) = value {
            windows.insert(key.into(), used);
        }
    }
    windows
}

/// `claim_outcome`: anything unrecognized stays ambiguous, never success.
pub(crate) fn claim_outcome(answer: &Value) -> &'static str {
    let result = answer.get("result").and_then(Value::as_str);
    if result == Some("ineligible") {
        return match answer.get("reason").and_then(Value::as_str) {
            Some("expired" | "unknown_grant" | "no_grant") => "noCredit",
            Some("already_used") => "already-used",
            Some("not_limited") => "nothingToReset",
            Some("cooldown") => "cooldown",
            Some(
                "surface" | "cli_version" | "config_off" | "tier" | "seat" | "mobile" | "tenure"
                | "other_experiment" | "paused" | "not_next_grant" | "grant_id_required",
            ) => "unavailable",
            _ => "ambiguous",
        };
    }
    match result {
        Some("reset") => "reset",
        Some("not_limited") => "nothingToReset",
        Some("already_used") => "already-used",
        Some("cooldown") => "cooldown",
        _ => "ambiguous",
    }
}

/// True when a normalized window list already carries reset grants.
pub(crate) fn has_reset_grants(windows: &[Value]) -> bool {
    windows.iter().any(|w| {
        w["key"]
            .as_str()
            .is_some_and(|k| k == "reset_credits_available" || k.starts_with("reset_credit_"))
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// 100 synthetic cases regenerated by `tests/fixtures/claude-usage.py` from claude_usage.py.
    #[test]
    fn python_claude_usage_oracle() {
        let cases: Vec<Value> =
            serde_json::from_str(include_str!("../../tests/fixtures/claude-usage.json")).unwrap();
        assert_eq!(cases.len(), 100);
        let mut failed = vec![];
        for case in &cases {
            let input = &case["input"];
            let actual = match case["kind"].as_str().unwrap() {
                "reset_at" => reset_at(input),
                "windows" => json!(normalize_windows(input)),
                "reset_windows" => Value::Object(reset_windows(input)),
                "reset_credits" => json!(normalize_reset_credits(
                    input,
                    case["now"].as_f64().unwrap()
                )),
                "session_allowance" => json!(normalize_session_reset_allowance(input)),
                "prepaid" => json!(normalize_prepaid(input)),
                "plan" => {
                    let plan = claude_plan(
                        &input["account"],
                        &input["organization"],
                        &input["fallback"],
                    );
                    let class = claude_plan_class(
                        &input["account"],
                        &input["organization"],
                        plan.as_deref(),
                    );
                    json!({"plan": plan, "planClass": class})
                }
                "claim_outcome" => json!(claim_outcome(input)),
                other => panic!("unknown case {other}"),
            };
            if !py_eq(&actual, &case["expected"]) {
                failed.push(format!(
                    "{}: {input}\n  rust   {actual}\n  python {}",
                    case["kind"], case["expected"]
                ));
            }
        }
        assert!(
            failed.is_empty(),
            "{} mismatches:\n{}",
            failed.len(),
            failed.join("\n")
        );
    }
}
