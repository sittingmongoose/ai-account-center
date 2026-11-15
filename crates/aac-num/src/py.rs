//! CPython 3 number text, rounding and the `datetime` conversions AAC's Python helpers use.
//!
//! These differ from ECMAScript on purpose: `round` breaks ties to even, `repr` switches to an exponent at 1e16
//! and below 1e-4 and writes at least two exponent digits, and `fromtimestamp` rounds a float timestamp to the
//! microsecond before any field is read. Each function states its domain and answers `None` outside it.
use crate::civil;
use crate::digits::{Decimal, Tie, shortest};
use alloc::{format, string::String};

/// `round(x, ndigits)` for a float (`float.__round__`): the exact binary value rounded half-even to `ndigits`
/// decimal places (`_Py_dg_dtoa` mode 3), read back as the nearest double (`_Py_dg_strtod`). NaN and the infinities
/// are themselves; `ndigits > 323` keeps `x`; `ndigits < -308` gives a zero with the sign of `x`. Domain: all of
/// binary64 and every `ndigits`; `None` is Python's `OverflowError` (a rounding past the largest double).
pub fn round(x: f64, ndigits: i32) -> Option<f64> {
    if !x.is_finite() || ndigits > 323 {
        return Some(x);
    }
    if ndigits < -308 {
        return Some(if x.is_sign_negative() { -0.0 } else { 0.0 });
    }
    let magnitude = Decimal::exact(x)?.round_at(-ndigits, Tie::Even).to_f64();
    if magnitude.is_infinite() {
        return None;
    }
    Some(if x.is_sign_negative() {
        -magnitude
    } else {
        magnitude
    })
}

/// `repr(x)` for a float (`float_repr_style` "short"): the shortest round-trip digits, fixed notation when the
/// decimal point position is in `(-4, 16]` (with `.0` on integers), otherwise `d.ddde±XX`. Domain: all of binary64.
pub fn repr(x: f64) -> String {
    if x.is_nan() {
        return "nan".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "inf" } else { "-inf" }.into();
    }
    let sign = if x.is_sign_negative() { "-" } else { "" };
    if x == 0.0 {
        return format!("{sign}0.0");
    }
    let d = shortest(x);
    let s = d.text();
    let len = s.len() as i32;
    let decpt = d.lead() + 1;
    if decpt <= -4 || decpt > 16 {
        let exp = decpt - 1;
        let rest = if len > 1 {
            format!(".{}", &s[1..])
        } else {
            String::new()
        };
        let exp_sign = if exp < 0 { "-" } else { "+" };
        return format!(
            "{sign}{}{rest}e{exp_sign}{:02}",
            &s[..1],
            exp.unsigned_abs()
        );
    }
    if decpt <= 0 {
        format!("{sign}0.{}{s}", "0".repeat((-decpt) as usize))
    } else if decpt < len {
        format!("{sign}{}.{}", &s[..decpt as usize], &s[decpt as usize..])
    } else {
        format!("{sign}{s}{}.0", "0".repeat((decpt - len) as usize))
    }
}

/// `json.dumps(x)` for a float (default `allow_nan=True`): `NaN`, `Infinity`, `-Infinity`, else [`repr`]. Domain:
/// all of binary64.
pub fn json_dumps(x: f64) -> String {
    if x.is_nan() {
        "NaN".into()
    } else if x.is_infinite() {
        if x > 0.0 { "Infinity" } else { "-Infinity" }.into()
    } else {
        repr(x)
    }
}

/// C `round()`: halfway cases away from zero (exact: never adds 0.5).
fn c_round(x: f64) -> f64 {
    let t = crate::trunc(x);
    let rest = x - t;
    if rest >= 0.5 {
        t + 1.0
    } else if rest <= -0.5 {
        t - 1.0
    } else {
        t
    }
}

/// The seconds and microseconds `datetime.fromtimestamp(t, timezone.utc)` holds for a float `t`: CPython's
/// `_PyTime_ObjectToTimeval(t, ROUND_HALF_EVEN)`, which splits `t` with `modf`, scales the fraction by 1e6 in
/// binary64, rounds it half-even and carries a full second. Domain: finite `t` with `|t| < 2^53` whose second falls
/// in years 1 to 9999 (outside it Python raises: `None`).
pub fn fromtimestamp(t: f64) -> Option<(i64, u32)> {
    if !t.is_finite() || crate::abs(t) >= 9_007_199_254_740_992.0 {
        return None;
    }
    let int = crate::trunc(t);
    let mut whole = int;
    let mut micros = (t - int) * 1e6;
    let rounded = c_round(micros);
    micros = if crate::abs(micros - rounded) == 0.5 {
        2.0 * c_round(micros / 2.0)
    } else {
        rounded
    };
    if micros >= 1e6 {
        micros -= 1e6;
        whole += 1.0;
    } else if micros < 0.0 {
        micros += 1e6;
        whole -= 1.0;
    }
    let seconds = whole as i64;
    let year = civil::civil_from_days(seconds.div_euclid(86_400)).0;
    if !(1..=9999).contains(&year) {
        return None;
    }
    Some((seconds, micros as u32))
}

/// `.isoformat(timespec=...)` of a UTC `datetime` with `+00:00` written `Z`: `YYYY-MM-DDTHH:MM:SS`, then `.mmm`
/// (the microseconds truncated) when `millis` is set.
fn utc_iso(seconds: i64, micros: u32, millis: bool) -> String {
    let (y, mo, d, h, mi, s, _) = civil::utc_fields(seconds * 1000);
    let mut out = format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}");
    if millis {
        out.push_str(&format!(".{:03}", micros / 1000));
    }
    out.push('Z');
    out
}

/// `datetime.datetime.fromtimestamp(ms / 1000, timezone.utc).isoformat(timespec="milliseconds")` with `Z`
/// (`signin_expiry.py` `iso`). Python divides in binary64 and rounds to the microsecond first, so some far-future
/// milliseconds print as the one before (8796093022210 gives `.209Z`). Domain: `|ms| < 2^53` in years 1 to 9999.
pub fn iso_millis(ms: i64) -> Option<String> {
    if ms.unsigned_abs() >= 1 << 53 {
        return None;
    }
    let (seconds, micros) = fromtimestamp(ms as f64 / 1000.0)?;
    Some(utc_iso(seconds, micros, true))
}

/// `datetime.datetime.fromtimestamp(t, timezone.utc)` written with `.isoformat(timespec="seconds")` and `Z`
/// (`plan_common.reset_at` on a number), with the instant's year. Domain: as [`fromtimestamp`].
pub fn iso_seconds_of_timestamp(t: f64) -> Option<(String, i64)> {
    let (seconds, _) = fromtimestamp(t)?;
    let year = civil::civil_from_days(seconds.div_euclid(86_400)).0;
    Some((utc_iso(seconds, 0, false), year))
}

/// A whole UTC second written `YYYY-MM-DDTHH:MM:SSZ` (`isoformat(timespec="seconds")` with `Z`). Domain: years 1 to
/// 9999.
pub fn iso_seconds(seconds: i64) -> Option<String> {
    let year = civil::civil_from_days(seconds.div_euclid(86_400)).0;
    (1..=9999)
        .contains(&year)
        .then(|| utc_iso(seconds, 0, false))
}

/// The epoch seconds of `YYYY-MM-DDTHH:MM:SSZ`, the one raw shape proved against `plan_common.reset_at`
/// (`datetime.fromisoformat(value.replace("Z", "+00:00"))`), and its year. Domain: exactly 20 bytes of that shape
/// with a real month, day, hour, minute and second (`00`-`59`); year 0 is admitted (the caller's year range then
/// rejects it, as Python's `ValueError` gives no time). Anything else is `None`.
pub fn parse_z_seconds(text: &str) -> Option<(i64, i64)> {
    let b = text.as_bytes();
    if b.len() != 20
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'Z'
    {
        return None;
    }
    let num = |from: usize| -> Option<i64> {
        let part = b.get(from..from + 2)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let year = num(0)? * 100 + num(2)?;
    let (month, day, hour, minute, second) = (num(5)?, num(8)?, num(11)?, num(14)?, num(17)?);
    if !(1..=12).contains(&month)
        || day < 1
        || day > civil::days_in_month(year, month)
        || hour > 23
        || minute > 59
        || second > 59
    {
        return None;
    }
    let days = civil::days_from_civil(year, month, day);
    Some((days * 86_400 + hour * 3600 + minute * 60 + second, year))
}

#[cfg(test)]
mod tests {
    // Exact binary ties and neighbours are written out in full on purpose.
    #![allow(clippy::excessive_precision)]
    use super::*;

    #[test]
    fn round_is_half_even_on_the_exact_value() {
        assert_eq!(round(99.853515625, 8), Some(99.85351562));
        assert_eq!(round(2.675, 2), Some(2.67));
        assert_eq!(round(0.125, 2), Some(0.12));
        assert_eq!(round(0.375, 2), Some(0.38));
        assert_eq!(round(2.5, 0), Some(2.0));
        assert_eq!(round(-0.0001, 2).map(f64::is_sign_negative), Some(true));
        assert_eq!(round(1.7976931348623157e308, -308), None);
        assert_eq!(round(12345.0, -2), Some(12300.0));
        assert_eq!(round(1.5, 400), Some(1.5));
    }

    #[test]
    fn repr_and_json_text() {
        for (x, text) in [
            (0.0, "0.0"),
            (-0.0, "-0.0"),
            (1e16, "1e+16"),
            (1e15, "1000000000000000.0"),
            (0.0001, "0.0001"),
            (0.00001, "1e-05"),
            (1.5, "1.5"),
            (2251799813685247.25, "2251799813685247.2"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (123456789012345678.0, "1.2345678901234568e+17"),
        ] {
            assert_eq!(repr(x), text, "{x}");
        }
        assert_eq!(json_dumps(f64::NEG_INFINITY), "-Infinity");
    }

    #[test]
    fn timestamps_round_to_the_microsecond_first() {
        assert_eq!(
            iso_millis(8_796_093_022_210).as_deref(),
            Some("2248-09-26T15:10:22.209Z")
        );
        assert_eq!(
            iso_millis(9_999_999_999_001).as_deref(),
            Some("2286-11-20T17:46:39.000Z")
        );
        assert_eq!(
            iso_millis(1_767_225_600_000).as_deref(),
            Some("2026-01-01T00:00:00.000Z")
        );
        assert_eq!(iso_millis(-1).as_deref(), Some("1969-12-31T23:59:59.999Z"));
        assert_eq!(
            parse_z_seconds("2026-01-01T00:00:00Z"),
            Some((1_767_225_600, 2026))
        );
        assert_eq!(parse_z_seconds("2026-02-29T00:00:00Z"), None);
        assert_eq!(parse_z_seconds("2026-01-01T00:00:60Z"), None);
        assert_eq!(
            parse_z_seconds("0000-01-01T00:00:00Z").map(|(_, y)| y),
            Some(0)
        );
    }
}
