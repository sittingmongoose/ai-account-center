//! The JavaScript number and date spellings the renewal DTOs and state files
//! use: `Math.round`, `new Date(ms).toISOString()`, `Date.parse` and
//! `JSON.stringify` number forms. Parity is byte-level, so the forms are pinned
//! here instead of relying on Rust's defaults.

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use regex::Regex;
use std::sync::LazyLock;

/// `Math.round`: floor(x + 0.5), which rounds negative halves toward +infinity
/// (Rust's `f64::round` rounds them away from zero instead).
pub fn js_round(value: f64) -> i64 {
    (value + 0.5).floor() as i64
}

/// `new Date(ms).toISOString()`: always milliseconds, always `Z`.
pub fn iso(ms: i64) -> Option<String> {
    let dt = DateTime::from_timestamp_millis(ms)?;
    Some(dt.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// A `serde_json::Number` spelled the way `JSON.stringify` writes the double:
/// integral values lose the fraction (96.0 -> 96), others keep the shortest form.
pub fn js_number(value: f64) -> Option<serde_json::Number> {
    if !value.is_finite() {
        return None;
    }
    if value.fract() == 0.0 && value.abs() < 9.007_199_254_740_992e15 {
        if value >= 0.0 {
            return Some(serde_json::Number::from(value as u64));
        }
        return Some(serde_json::Number::from(value as i64));
    }
    serde_json::Number::from_f64(value)
}

/// `Number.isInteger` over a JSON value: integral doubles count (1.0 is 1).
pub fn is_integer_number(value: &serde_json::Value) -> Option<f64> {
    let n = value.as_f64()?;
    if n.is_finite() && n.fract() == 0.0 {
        Some(n)
    } else {
        None
    }
}

static ISO_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^(\d{4})-(\d{2})-(\d{2})(?:[T ](\d{2}):(\d{2})(?::(\d{2})(?:\.(\d+))?)?(Z|[+-]\d{2}:?\d{2}|[+-]\d{2})?)?$",
    )
    .expect("static ISO pattern")
});
/// `Date.parse` for the ISO forms the renewal paths read (status records,
/// `last_refresh`, the Claude runner file). Other spellings JS also accepts are
/// not part of the captured behaviour and read as `None` (NaN).
pub fn parse_date_ms(text: &str) -> Option<i64> {
    let caps = ISO_RE.captures(text)?;
    let year: i32 = caps.get(1)?.as_str().parse().ok()?;
    let month: u32 = caps.get(2)?.as_str().parse().ok()?;
    let day: u32 = caps.get(3)?.as_str().parse().ok()?;
    let date = NaiveDate::from_ymd_opt(year, month, day)?;
    let Some(hour_match) = caps.get(4) else {
        // Date-only ISO forms are UTC midnight.
        return Some(date.and_hms_opt(0, 0, 0)?.and_utc().timestamp_millis());
    };
    let mut hour: u32 = hour_match.as_str().parse().ok()?;
    let minute: u32 = caps.get(5)?.as_str().parse().ok()?;
    let second: u32 = caps.get(6).map_or(0, |m| m.as_str().parse().unwrap_or(0));
    let mut extra_day = false;
    if hour == 24 {
        // ISO allows 24:00:00 as the next day's midnight.
        if minute != 0 || second != 0 {
            return None;
        }
        hour = 0;
        extra_day = true;
    }
    if minute > 59 || second > 59 {
        return None;
    }
    let millis: u32 = match caps.get(7) {
        Some(frac) => {
            let digits = frac.as_str();
            let padded = format!("{digits:0<3}");
            padded[..3].parse().ok()?
        }
        None => 0,
    };
    let time = NaiveTime::from_hms_milli_opt(hour, minute, second, millis)?;
    let mut datetime = NaiveDateTime::new(date, time);
    if extra_day {
        datetime += chrono::Duration::days(1);
    }
    match caps.get(8).map(|m| m.as_str()) {
        Some("Z") | None if caps.get(8).is_some() => {
            Some(Utc.from_utc_datetime(&datetime).timestamp_millis())
        }
        Some(offset) => {
            let sign = if offset.starts_with('-') { -1 } else { 1 };
            let digits: String = offset[1..].chars().filter(|c| *c != ':').collect();
            let (oh, om) = match digits.len() {
                2 => (digits.parse::<i32>().ok()?, 0),
                4 => (
                    digits[..2].parse::<i32>().ok()?,
                    digits[2..].parse::<i32>().ok()?,
                ),
                _ => return None,
            };
            if oh > 23 || om > 59 {
                return None;
            }
            let shift = chrono::Duration::minutes((sign * (oh * 60 + om)) as i64);
            Some(
                Utc.from_utc_datetime(&(datetime - shift))
                    .timestamp_millis(),
            )
        }
        // No zone on a date-time: JS reads it in the local zone.
        None => Some(
            Local
                .from_local_datetime(&datetime)
                .single()?
                .timestamp_millis(),
        ),
    }
}

/// `Number.isFinite(Date.parse(value))` over an unknown JSON value.
pub fn is_iso(value: &serde_json::Value) -> bool {
    value.as_str().and_then(parse_date_ms).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_matches_math_round_on_negative_halves() {
        assert_eq!(js_round(-0.5), 0);
        assert_eq!(js_round(0.5), 1);
        assert_eq!(js_round(-1.5), -1);
        assert_eq!(js_round(2.5), 3);
    }

    #[test]
    fn iso_spells_milliseconds() {
        assert_eq!(
            iso(1_759_924_800_000).as_deref(),
            Some("2025-10-08T12:00:00.000Z")
        );
        assert_eq!(iso(0).as_deref(), Some("1970-01-01T00:00:00.000Z"));
    }

    #[test]
    fn parse_reads_the_forms_the_paths_use() {
        assert_eq!(
            parse_date_ms("2026-10-08T12:00:00.000Z"),
            Some(1_791_460_800_000)
        );
        assert_eq!(
            parse_date_ms("2026-10-08T12:00:00Z"),
            Some(1_791_460_800_000)
        );
        assert_eq!(
            parse_date_ms("2026-10-08T14:00:00+02:00"),
            Some(1_791_460_800_000)
        );
        assert_eq!(parse_date_ms("2026-10-08"), Some(1_791_417_600_000));
        assert_eq!(parse_date_ms("2026-13-08T12:00:00Z"), None);
        assert_eq!(parse_date_ms("not a date"), None);
        assert_eq!(parse_date_ms(""), None);
    }

    #[test]
    fn numbers_stringify_like_javascript() {
        assert_eq!(js_number(96.0).expect("finite").to_string(), "96");
        assert_eq!(js_number(12.5).expect("finite").to_string(), "12.5");
        assert_eq!(js_number(0.0).expect("finite").to_string(), "0");
    }
}
