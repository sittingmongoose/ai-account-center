//! The JavaScript screening and text rules the snapshot pipeline is written against:
//! `JSON.stringify` bytes (through `aac-views::js_json`), `Date.parse` / `toISOString`,
//! UTF-16 length counts, the control-character and credential screens, and the identity
//! hashes the quota history keys its records by. Every rule names its TypeScript source.

use crate::js::private::NULL;
use aac_num::ecma;
use serde_json::{Map, Value, json};

mod private {
    use serde_json::Value;
    /// The value a missing member reads as: TypeScript's `undefined` screens like `null`
    /// in every guard this pipeline applies, except the explicit `!== undefined` checks,
    /// which use [`super::has`].
    pub static NULL: Value = Value::Null;
}

/// A member of an object, or `null` when the value is not an object or the key is absent.
pub fn member<'a>(value: &'a Value, key: &str) -> &'a Value {
    value
        .as_object()
        .and_then(|map| map.get(key))
        .unwrap_or(&NULL)
}

/// Whether an object carries the key at all (TypeScript's `!== undefined` distinction).
pub fn has(value: &Value, key: &str) -> bool {
    value.as_object().is_some_and(|map| map.contains_key(key))
}

/// JavaScript's `value.length`: UTF-16 code units, not code points.
pub fn utf16_len(text: &str) -> usize {
    text.chars().map(char::len_utf16).sum()
}

/// The projection's CONTROL screen (`agent-usage-projection.ts`): C0, DEL, C1, the bidi
/// marks and isolates, the line/paragraph separators and the BOM.
pub fn control_char(c: char) -> bool {
    matches!(c,
        '\u{0000}'..='\u{001f}'
        | '\u{007f}'..='\u{009f}'
        | '\u{061c}'
        | '\u{200e}'
        | '\u{200f}'
        | '\u{2028}'..='\u{202e}'
        | '\u{2066}'..='\u{2069}'
        | '\u{feff}'
    )
}

/// The settings' narrower control screen (`agent-usage-settings.ts`): C0 and DEL only.
pub fn settings_control_char(c: char) -> bool {
    matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}')
}

/// `text(value, max)`: a non-empty string within the UTF-16 bound, without control characters.
pub fn text(value: &Value, max: usize) -> Option<String> {
    let s = value.as_str()?;
    let len = utf16_len(s);
    if len == 0 || len > max || s.chars().any(control_char) {
        return None;
    }
    Some(s.to_owned())
}

/// `finite(value)`: a JSON number that is a finite double.
pub fn finite(value: &Value) -> Option<f64> {
    value.as_f64().filter(|x| x.is_finite())
}

/// JavaScript `\s` (WhiteSpace and LineTerminator), as the credential screen's `bearer\s` reads it.
pub fn js_whitespace(c: char) -> bool {
    matches!(c,
        '\u{0009}'..='\u{000d}'
        | '\u{0020}'
        | '\u{00a0}'
        | '\u{1680}'
        | '\u{2000}'..='\u{200a}'
        | '\u{2028}'
        | '\u{2029}'
        | '\u{202f}'
        | '\u{205f}'
        | '\u{3000}'
        | '\u{feff}'
    )
}

/// `String.prototype.trimEnd`'s trailing set (the same WhiteSpace and LineTerminator).
pub fn js_trim_end(text: &str) -> &str {
    text.trim_end_matches(js_whitespace)
}

/// `Date.parse` for the forms the pipeline meets: the explicit-zone ISO date-times
/// (`aac-num`'s proven domain), date-only strings (UTC midnight by the spec) and fractional
/// seconds of one to three digits. Everything else is NaN: the engine-dependent spellings
/// decline exactly as the other lanes' readers do.
pub fn date_parse_str(text: &str) -> Option<i64> {
    if let Some(ms) = ecma::parse_instant(text) {
        return Some(ms);
    }
    let bytes = text.as_bytes();
    // Date-only: YYYY-MM-DD, in-range fields, UTC midnight.
    if bytes.len() == 10 && bytes[4] == b'-' && bytes[7] == b'-' {
        let normalized = format!("{text}T00:00Z");
        return ecma::parse_instant(&normalized);
    }
    // A fraction of one or two digits: pad to the three the proven parser reads.
    if let Some(dot) = bytes.iter().rposition(|&b| b == b'.') {
        let tail = &bytes[dot + 1..];
        let digits = tail.iter().take_while(|b| b.is_ascii_digit()).count();
        if (1..=2).contains(&digits) && dot >= 16 {
            let mut padded = text[..dot + 1 + digits].to_owned();
            for _ in digits..3 {
                padded.push('0');
            }
            padded.push_str(&text[dot + 1 + digits..]);
            return ecma::parse_instant(&padded);
        }
    }
    None
}

/// `Date.parse(value)` for a JSON value: only strings parse.
pub fn date_parse(value: &Value) -> Option<i64> {
    date_parse_str(value.as_str()?)
}

/// `iso(value)`: `Date.parse` then `toISOString`, null when either declines.
pub fn iso(value: &Value) -> Option<String> {
    ecma::to_iso_string(date_parse(value)?)
}

/// `new Date(ms).toISOString()`; None outside the four-digit-year domain.
pub fn iso_of(ms: i64) -> Option<String> {
    ecma::to_iso_string(ms)
}

/// `minuteIso`: the reading's minute, `YYYY-MM-DDTHH:MMZ` (the floored ISO without `:00.000`).
pub fn minute_iso(ms: i64) -> Option<String> {
    let floored = ms.div_euclid(60_000) * 60_000;
    let full = ecma::to_iso_string(floored)?;
    Some(full.replace(":00.000Z", "Z"))
}

/// `utc(iso)`: `2026-10-15T12:00:00.000Z` becomes `2026-10-15 12:00 UTC`.
pub fn utc(iso_text: &str) -> String {
    let spaced = iso_text.replacen('T', " ", 1);
    // The trailing /:\d\d(\.\d+)?Z$ the renderer strips.
    let bytes = spaced.as_bytes();
    if bytes.last() == Some(&b'Z') {
        let mut cut = bytes.len() - 1;
        // Optional fraction.
        if cut >= 2 && bytes[cut - 1].is_ascii_digit() {
            let mut start = cut - 1;
            while start > 0 && bytes[start - 1].is_ascii_digit() {
                start -= 1;
            }
            if start > 0 && bytes[start - 1] == b'.' {
                cut = start - 1;
            }
        }
        if cut >= 3
            && bytes[cut - 1].is_ascii_digit()
            && bytes[cut - 2].is_ascii_digit()
            && bytes[cut - 3] == b':'
        {
            return format!("{} UTC", &spaced[..cut - 3]);
        }
    }
    spaced
}

/// `span(seconds)`: "2h 35m", "3d 4h", "45s"; a past or unknown time is "now".
pub fn span(seconds: f64) -> String {
    if !seconds.is_finite() || seconds <= 0.0 {
        return "now".into();
    }
    let days = (seconds / 86_400.0).floor() as i64;
    let hours = ((seconds % 86_400.0) / 3600.0).floor() as i64;
    let minutes = ((seconds % 3600.0) / 60.0).floor() as i64;
    if days > 0 {
        return format!("{days}d {hours}h");
    }
    if hours > 0 {
        return format!("{hours}h {minutes}m");
    }
    if minutes > 0 {
        return format!("{minutes}m");
    }
    format!("{}s", seconds.floor() as i64)
}

/// `amount(value)`: integers as their digits, other numbers with two fraction digits.
pub fn amount(value: f64) -> String {
    if value.is_finite() && value.fract() == 0.0 && value.abs() < 1e21 {
        ecma::to_string(value)
    } else {
        ecma::to_fixed(value, 2).unwrap_or_else(|| ecma::to_string(value))
    }
}

/// `Number.isInteger`.
pub fn is_integer(value: f64) -> bool {
    value.is_finite() && value.fract() == 0.0
}

/// `JSON.stringify(value)` byte for byte (compact).
pub fn stringify(value: &Value) -> String {
    let mut writer = aac_views::js_json::Writer::plain(256);
    writer.value(value);
    String::from_utf8(writer.into_bytes()).unwrap_or_default()
}

/// `JSON.stringify(value, null, 2)` byte for byte: two spaces a level, empty containers inline.
pub fn stringify_pretty(value: &Value) -> String {
    let mut out = Vec::with_capacity(512);
    pretty(value, 0, &mut out);
    String::from_utf8(out).unwrap_or_default()
}

fn scalar(value: &Value, out: &mut Vec<u8>) {
    let mut writer = aac_views::js_json::Writer::plain(64);
    writer.value(value);
    out.extend_from_slice(&writer.into_bytes());
}

fn pretty(value: &Value, depth: usize, out: &mut Vec<u8>) {
    let pad = "  ".repeat(depth);
    let inner = "  ".repeat(depth + 1);
    match value {
        Value::Object(map) if !map.is_empty() => {
            out.extend_from_slice(b"{\n");
            let members = aac_views::js_json::ordered(map);
            for (index, (key, member)) in members.iter().enumerate() {
                out.extend_from_slice(inner.as_bytes());
                scalar(&Value::String((*key).to_owned()), out);
                out.extend_from_slice(b": ");
                pretty(member, depth + 1, out);
                if index + 1 < members.len() {
                    out.push(b',');
                }
                out.push(b'\n');
            }
            out.extend_from_slice(pad.as_bytes());
            out.push(b'}');
        }
        Value::Array(items) if !items.is_empty() => {
            out.extend_from_slice(b"[\n");
            for (index, item) in items.iter().enumerate() {
                out.extend_from_slice(inner.as_bytes());
                pretty(item, depth + 1, out);
                if index + 1 < items.len() {
                    out.push(b',');
                }
                out.push(b'\n');
            }
            out.extend_from_slice(pad.as_bytes());
            out.push(b']');
        }
        other => scalar(other, out),
    }
}

/// An object built in insertion order, the way the TypeScript literals write their fields.
pub fn object(members: Vec<(&str, Value)>) -> Value {
    let mut map = Map::new();
    for (key, value) in members {
        map.insert(key.to_owned(), value);
    }
    Value::Object(map)
}

/// A number as JSON: non-finite screens to null (the projection's `finite` already refused them).
pub fn number(value: f64) -> Value {
    serde_json::Number::from_f64(value)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

/// An optional number: null when absent.
pub fn opt_number(value: Option<f64>) -> Value {
    value.map(number).unwrap_or(Value::Null)
}

/// An optional string: null when absent.
pub fn opt_str(value: Option<&str>) -> Value {
    value
        .map(|s| Value::String(s.to_owned()))
        .unwrap_or(Value::Null)
}

/// JavaScript truthiness for the pool-id and kind guards.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `accountAnalyticsIdentity`: SHA-256 of `JSON.stringify([provider, id, email.toLowerCase() ?? null])`.
pub fn account_identity(row: &Value) -> String {
    let email = match member(row, "email") {
        Value::String(s) => Value::String(s.to_lowercase()),
        _ => Value::Null,
    };
    let array = json!([member(row, "provider"), member(row, "id"), email]);
    aac_core::sha256_hex(stringify(&array))
}

/// `analyticsWindowIdentity`: `JSON.stringify([key, kind ?? null, unit])`, plus a truthy pool id.
pub fn window_identity(window: &Value) -> String {
    let kind = member(window, "kind");
    let kind = if kind.is_null() {
        Value::Null
    } else {
        kind.clone()
    };
    let mut parts = vec![
        member(window, "key").clone(),
        kind,
        member(window, "unit").clone(),
    ];
    let pool = member(window, "poolId");
    if truthy(pool) {
        parts.push(pool.clone());
    }
    stringify(&Value::Array(parts))
}

/// `longBurnKey` / the history's series key: account identity and window identity.
pub fn long_burn_key(row: &Value, window: &Value) -> String {
    stringify(&json!([account_identity(row), window_identity(window)]))
}

/// `burnKey`: the window's key, unit and pool, as the rates map tells windows apart.
pub fn burn_key(key: &str, unit: Option<&str>, pool_id: Option<&str>) -> String {
    stringify(&json!([key, unit, pool_id]))
}

/// `boundaryIdentity`: the window without its optional unit, so a dropped unit still bounds periods.
pub fn boundary_identity(window: &Value) -> String {
    let kind = member(window, "kind");
    let kind = if kind.is_null() {
        Value::Null
    } else {
        kind.clone()
    };
    let pool = member(window, "poolId");
    let pool = if pool.is_null() {
        Value::Null
    } else {
        pool.clone()
    };
    stringify(&json!([member(window, "key"), kind, pool]))
}

/// `boundaryKey`: account identity plus the unit-less window identity.
pub fn boundary_key(row: &Value, window: &Value) -> String {
    stringify(&json!([account_identity(row), boundary_identity(window)]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_strips_seconds_and_fraction() {
        assert_eq!(utc("2026-10-15T12:00:00.000Z"), "2026-10-15 12:00 UTC");
        assert_eq!(utc("2026-11-30T23:04:00Z"), "2026-11-30 23:04 UTC");
        assert_eq!(utc("2026-10-15T12:00Z"), "2026-10-15 12 UTC");
    }

    #[test]
    fn span_words() {
        assert_eq!(span(0.0), "now");
        assert_eq!(span(-5.0), "now");
        assert_eq!(span(45.9), "45s");
        assert_eq!(span(90.0), "1m");
        assert_eq!(span(3600.0 + 35.0 * 60.0), "1h 35m");
        assert_eq!(span(3.0 * 86_400.0 + 4.0 * 3600.0), "3d 4h");
        assert_eq!(span(60.0), "1m");
    }

    #[test]
    fn amount_words() {
        assert_eq!(amount(36.0), "36");
        assert_eq!(amount(12.5), "12.50");
        assert_eq!(amount(-9.6), "-9.60");
        assert_eq!(amount(0.125), "0.13");
    }

    #[test]
    fn pretty_matches_json_stringify() {
        let value = json!({"version": 1, "locations": [{"where": "the Mac", "path": "/Volumes/x"}], "empty": [], "obj": {}});
        assert_eq!(
            stringify_pretty(&value),
            "{\n  \"version\": 1,\n  \"locations\": [\n    {\n      \"where\": \"the Mac\",\n      \"path\": \"/Volumes/x\"\n    }\n  ],\n  \"empty\": [],\n  \"obj\": {}\n}"
        );
    }

    #[test]
    fn date_parse_forms() {
        assert_eq!(
            date_parse_str("2026-10-15T12:00:00.000Z"),
            Some(1_792_065_600_000)
        );
        assert_eq!(
            date_parse_str("2026-10-15"),
            date_parse_str("2026-10-15T00:00:00.000Z")
        );
        assert_eq!(
            date_parse_str("2026-10-15T12:00:00.5Z"),
            date_parse_str("2026-10-15T12:00:00.500Z")
        );
        assert_eq!(date_parse_str("not a date"), None);
        assert_eq!(date_parse_str(""), None);
    }

    #[test]
    fn identities_hash_the_json_text() {
        let row = json!({"provider": "codex", "id": "codex:gmail", "email": "Person@Example.com"});
        let expected = aac_core::sha256_hex(r#"["codex","codex:gmail","person@example.com"]"#);
        assert_eq!(account_identity(&row), expected);
        let window = json!({"key": "five_hour", "kind": "rate_limit", "unit": null});
        assert_eq!(
            window_identity(&window),
            r#"["five_hour","rate_limit",null]"#
        );
        let pooled = json!({"key": "weekly", "unit": "credits", "poolId": "pool-a"});
        assert_eq!(
            window_identity(&pooled),
            r#"["weekly",null,"credits","pool-a"]"#
        );
    }
}
