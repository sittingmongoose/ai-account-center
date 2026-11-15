//! ECMAScript (ECMA-262) number text, rounding and dates, as V8 and JavaScriptCore give them.
//!
//! Number text uses [`crate::digits::shortest`]; `toFixed` and `toPrecision` round the exact binary value with ties
//! to the larger magnitude. Each function states its domain: outside it the answer is `None` and the caller declines.
use crate::civil;
use crate::digits::{Decimal, Tie, shortest};
use alloc::{format, string::String, vec::Vec};

/// `Number.MAX_SAFE_INTEGER`.
pub const MAX_SAFE_INTEGER: i64 = 9_007_199_254_740_991;

/// The largest time value a `Date` holds, in milliseconds either side of 1970 (ECMA-262 21.4.1.1).
pub const MAX_TIME: i64 = 8_640_000_000_000_000;

/// `Number.prototype.toString()` in radix 10, for every binary64 (`-0` is `"0"`). Domain: all of binary64.
pub fn to_string(x: f64) -> String {
    if x.is_nan() {
        return "NaN".into();
    }
    if x == 0.0 {
        return "0".into();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    let d = shortest(x);
    let s = d.text();
    let k = s.len() as i32;
    let n = d.lead() + 1;
    let mut out = String::new();
    if x < 0.0 {
        out.push('-');
    }
    if k <= n && n <= 21 {
        out.push_str(s);
        out.extend(core::iter::repeat_n('0', (n - k) as usize));
    } else if 0 < n && n <= 21 {
        out.push_str(&s[..n as usize]);
        out.push('.');
        out.push_str(&s[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        out.extend(core::iter::repeat_n('0', (-n) as usize));
        out.push_str(s);
    } else {
        let e = n - 1;
        out.push_str(&s[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&s[1..]);
        }
        out.push_str(&format!(
            "e{}{}",
            if e > 0 { "+" } else { "-" },
            e.unsigned_abs()
        ));
    }
    out
}

/// `JSON.stringify(x)` for a number: [`to_string`], with NaN and the infinities as `null`. Domain: all of binary64.
pub fn json(x: f64) -> String {
    if x.is_finite() {
        to_string(x)
    } else {
        "null".into()
    }
}

/// `Math.round(x)` on the binary64 value: ties toward +infinity; `-0`, values in `[-0.5, 0)`, NaN and the infinities
/// as JavaScript gives them. It never adds 0.5, which could itself round across a tie. Domain: all of binary64.
pub fn math_round(x: f64) -> f64 {
    if !x.is_finite() || x == 0.0 {
        return x;
    }
    if (-0.5..0.0).contains(&x) {
        return -0.0;
    }
    let lower = crate::floor(x);
    // Exact: Sterbenz outside (-0.5, 0), or a zero floor.
    if x - lower < 0.5 { lower } else { lower + 1.0 }
}

/// `values.reduce((sum, v) => sum + v, 0)`: from +0, left to right. (Rust's `Iterator::sum` starts from -0, so a
/// list of `-0` sums to `-0` there and to `+0` here.) Domain: all of binary64.
pub fn sum(values: impl IntoIterator<Item = f64>) -> f64 {
    values.into_iter().fold(0.0, |total, v| total + v)
}

/// The integer `n` of `value` written with `places` fraction digits, as ASCII (at least `places + 1` digits).
fn with_point(n: &Decimal, places: u32) -> String {
    // n is an integer times 10^n.exp with n.exp >= -places.
    let mut digits: Vec<u8> = n.text().bytes().collect();
    if !n.is_zero() {
        digits.extend(core::iter::repeat_n(b'0', (n.exp + places as i32) as usize));
    }
    let places = places as usize;
    if places == 0 {
        return String::from_utf8(digits).unwrap_or_default();
    }
    if digits.len() <= places {
        let pad = places + 1 - digits.len();
        digits.splice(0..0, core::iter::repeat_n(b'0', pad));
    }
    let cut = digits.len() - places;
    digits.insert(cut, b'.');
    String::from_utf8(digits).unwrap_or_default()
}

/// `x.toFixed(places)`: the integer `n` for which `n / 10^places - x` is closest to zero on the exact value, the
/// larger `n` on a tie; `|x| >= 1e21` and non-finite `x` are `String(x)`. Domain: all of binary64 with
/// `places <= 100` (JavaScript's `RangeError` beyond is `None`).
pub fn to_fixed(x: f64, places: u32) -> Option<String> {
    if places > 100 {
        return None;
    }
    if !x.is_finite() || crate::abs(x) >= 1e21 {
        return Some(to_string(x));
    }
    let n = Decimal::exact(x)?.round_at(-(places as i32), Tie::Away);
    Some(format!(
        "{}{}",
        if x < 0.0 { "-" } else { "" },
        with_point(&n, places)
    ))
}

/// `x.toPrecision(p)`: the `p`-digit `n` and exponent `e` for which `n × 10^(e-p+1) - x` is closest to zero on the
/// exact value, the larger on a tie. Domain: all of binary64 with `1 <= p <= 100` (`RangeError` beyond is `None`).
pub fn to_precision(x: f64, p: u32) -> Option<String> {
    if !(1..=100).contains(&p) {
        return None;
    }
    if !x.is_finite() {
        return Some(to_string(x));
    }
    let p = p as i32;
    let sign = if x < 0.0 { "-" } else { "" };
    let (m, e): (Vec<u8>, i32) = if x == 0.0 {
        (alloc::vec![b'0'; p as usize], 0)
    } else {
        let exact = Decimal::exact(x)?;
        let lead = exact.lead();
        let n = exact.round_at(lead - p + 1, Tie::Away);
        let e = n.lead();
        let mut m: Vec<u8> = n.digits.clone();
        m.extend(core::iter::repeat_n(
            b'0',
            (p as usize).saturating_sub(m.len()),
        ));
        (m, e)
    };
    let m = String::from_utf8(m).unwrap_or_default();
    Some(if e < -6 || e >= p {
        let mut text = String::from(&m[..1]);
        if p != 1 {
            text.push('.');
            text.push_str(&m[1..]);
        }
        format!(
            "{sign}{text}e{}{}",
            if e > 0 { "+" } else { "-" },
            e.unsigned_abs()
        )
    } else if e == p - 1 {
        format!("{sign}{m}")
    } else if e >= 0 {
        let cut = (e + 1) as usize;
        format!("{sign}{}.{}", &m[..cut], &m[cut..])
    } else {
        format!("{sign}0.{}{m}", "0".repeat((-(e + 1)) as usize))
    })
}

/// `Number(x.toPrecision(p))`. Same domain as [`to_precision`].
pub fn precision_value(x: f64, p: u32) -> Option<f64> {
    to_precision(x, p)?.parse::<f64>().ok()
}

/// `/^\d{4}-\d{2}-\d{2}T/.test(text)`: the strings the presentation envelope gives a `Date.parse` epoch whenever one is
/// finite (`rust-read-input.ts`).
pub fn iso_prefixed(text: &str) -> bool {
    let b = text.as_bytes();
    b.len() >= 11
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
}

/// The first and last time values [`to_iso_string`] admits: 1970-01-01 to 9999-12-31, four-digit years.
pub const ISO_STRING_DOMAIN: (i64, i64) = (0, 253_402_300_799_999);

/// `new Date(ms).toISOString()`. Domain: [`ISO_STRING_DOMAIN`]; earlier times and the six-digit extended years
/// decline rather than guess.
pub fn to_iso_string(ms: i64) -> Option<String> {
    if !(ISO_STRING_DOMAIN.0..=ISO_STRING_DOMAIN.1).contains(&ms) {
        return None;
    }
    let (y, mo, d, h, mi, s, milli) = civil::utc_fields(ms);
    Some(format!(
        "{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{milli:03}Z"
    ))
}

/// `Date.parse(text)` for the ECMAScript date-time string forms every engine reads alike (ECMA-262 21.4.1.32 with an
/// explicit zone): `YYYY-MM-DDTHH:mm`, optionally `:ss` and then `.sss`, ending in `Z` or `±HH:mm`. Domain: exactly
/// those forms with in-range fields. Everything else is `None`: the empty string (NaN), date-only forms (UTC by the
/// spec, but they carry no clock), local-time forms (the engine's zone), `T24:00`, other spellings and extended years.
pub fn parse_instant(text: &str) -> Option<i64> {
    let b = text.as_bytes();
    let num = |from: usize, len: usize| -> Option<i64> {
        let part = b.get(from..from + len)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0i64, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let year = num(0, 4)?;
    if b.get(4) != Some(&b'-') || b.get(7) != Some(&b'-') || b.get(10) != Some(&b'T') {
        return None;
    }
    let (month, day) = (num(5, 2)?, num(8, 2)?);
    if !(1..=12).contains(&month) || day < 1 || day > civil::days_in_month(year, month) {
        return None;
    }
    let (h, mi) = (num(11, 2)?, num(14, 2)?);
    if b.get(13) != Some(&b':') || h > 23 || mi > 59 {
        return None;
    }
    let mut ms =
        civil::days_from_civil(year, month, day) * civil::DAY_MS + h * 3_600_000 + mi * 60_000;
    let mut at = 16;
    if b.get(at) == Some(&b':') {
        let sec = num(at + 1, 2)?;
        if sec > 59 {
            return None;
        }
        ms += sec * 1000;
        at += 3;
        if b.get(at) == Some(&b'.') {
            ms += num(at + 1, 3)?;
            at += 4;
        }
    }
    match b.get(at) {
        Some(b'Z') if at + 1 == b.len() => Some(ms),
        Some(sign @ (b'+' | b'-')) if at + 6 == b.len() && b.get(at + 3) == Some(&b':') => {
            let (oh, om) = (num(at + 1, 2)?, num(at + 4, 2)?);
            if oh > 23 || om > 59 {
                return None;
            }
            let offset = (oh * 60 + om) * 60_000;
            Some(if *sign == b'+' {
                ms - offset
            } else {
                ms + offset
            })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    // Exact binary ties and neighbours are written out in full on purpose.
    #![allow(clippy::excessive_precision)]
    use super::*;

    #[test]
    fn number_text_matches_javascript() {
        for (x, js) in [
            (0.0, "0"),
            (-0.0, "0"),
            (100.0, "100"),
            (37.5, "37.5"),
            (33.333333333333336, "33.333333333333336"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (1.5e-7, "1.5e-7"),
            (1e21, "1e+21"),
            (1.5e20, "150000000000000000000"),
            (123456789012345680000.0, "123456789012345680000"),
            (-2.5, "-2.5"),
            (2251799813685247.25, "2251799813685247.2"),
            (140737488355327.125, "140737488355327.12"),
            (5e-324, "5e-324"),
            (f64::MAX, "1.7976931348623157e+308"),
            (f64::NAN, "NaN"),
            (f64::NEG_INFINITY, "-Infinity"),
        ] {
            assert_eq!(to_string(x), js, "{x}");
        }
        assert_eq!(json(f64::INFINITY), "null");
    }

    #[test]
    fn fixed_and_precision_round_the_exact_value() {
        assert_eq!(to_fixed(4.85, 1).as_deref(), Some("4.8"));
        assert_eq!(to_fixed(0.125, 2).as_deref(), Some("0.13"));
        assert_eq!(to_fixed(-0.0001, 2).as_deref(), Some("-0.00"));
        assert_eq!(to_fixed(-0.0, 2).as_deref(), Some("0.00"));
        assert_eq!(to_fixed(1e21, 2).as_deref(), Some("1e+21"));
        assert_eq!(to_fixed(123.456, 0).as_deref(), Some("123"));
        assert_eq!(to_fixed(0.5, 0).as_deref(), Some("1"));
        assert_eq!(to_fixed(1.0, 101), None);
        assert_eq!(to_precision(0.0, 4).as_deref(), Some("0.000"));
        assert_eq!(to_precision(9.9995, 4).as_deref(), Some("9.999"));
        assert_eq!(to_precision(99999.0, 2).as_deref(), Some("1.0e+5"));
        assert_eq!(to_precision(0.000001234, 2).as_deref(), Some("0.0000012"));
        assert_eq!(to_precision(0.0000001234, 2).as_deref(), Some("1.2e-7"));
        assert_eq!(to_precision(-1.25, 2).as_deref(), Some("-1.3"));
        assert_eq!(to_precision(123.0, 3).as_deref(), Some("123"));
        assert_eq!(to_precision(5.0, 1).as_deref(), Some("5"));
        assert_eq!(
            precision_value(-0.0, 4).map(f64::is_sign_positive),
            Some(true)
        );
    }

    #[test]
    fn rounding_and_sums_keep_javascript_zeros() {
        assert_eq!(math_round(-0.5).to_bits(), (-0.0f64).to_bits());
        assert_eq!(math_round(2.5), 3.0);
        assert_eq!(math_round(-2.5), -2.0);
        assert_eq!(math_round(0.49999999999999994), 0.0);
        assert_eq!(math_round(4503599627370497.0), 4503599627370497.0);
        assert!(sum([-0.0, -0.0]).is_sign_positive());
        assert_eq!(sum([0.1, 0.2, 0.3]), 0.1 + 0.2 + 0.3);
    }

    #[test]
    fn dates() {
        assert_eq!(
            parse_instant("2026-01-05T00:07:00.000Z"),
            Some(1_767_571_620_000)
        );
        assert_eq!(
            parse_instant("2026-01-05T00:07:00+05:30"),
            Some(1_767_551_820_000)
        );
        for s in [
            "",
            "2026-01-05",
            "2026-01-05T00:07:00",
            "2026-01-05T24:00Z",
            "+002026-01-05T00:07Z",
        ] {
            assert_eq!(parse_instant(s), None, "{s}");
        }
        assert_eq!(
            to_iso_string(253_402_300_799_999).as_deref(),
            Some("9999-12-31T23:59:59.999Z")
        );
        assert_eq!(to_iso_string(-1), None);
        assert!(iso_prefixed("2026-11-15T") && !iso_prefixed("2026-11-15 "));
    }
}
