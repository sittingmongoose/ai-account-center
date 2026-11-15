//! Presentation facts supplied by the existing TS locale boundary. Missing facts refuse the
//! candidate; they never create a default timestamp, collation order or reading.
use serde::Deserialize;
use serde_json::{Value as V, json};
use std::{cell::Cell, cmp::Ordering, collections::HashMap};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub data: V,
    pub presentation: Presentation,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Presentation {
    dates: HashMap<String, Date>,
    epochs: HashMap<String, i64>,
    order: HashMap<String, usize>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Date {
    iso: String,
    /// Absent when the caller asked for ISO facts only; reading it then refuses the candidate.
    #[serde(default)]
    clock: Option<String>,
    #[serde(default)]
    day: Option<String>,
}
pub struct Context {
    facts: Presentation,
    order: HashMap<String, usize>,
    missing: Cell<bool>,
}
impl Context {
    pub fn new(facts: Presentation) -> Self {
        let order = facts.order.clone();
        Self {
            facts,
            order,
            missing: Cell::new(false),
        }
    }
    pub fn valid(&self) -> bool {
        !self.missing.get()
    }
    pub fn epoch(&self, value: &V) -> Option<i64> {
        if value.is_null() {
            return None;
        }
        if let Some(ms) = value.as_i64() {
            return Some(ms);
        }
        self.facts.epochs.get(value.as_str()?).copied()
    }
    fn date(&self, ms: i64) -> Option<&Date> {
        let value = self.facts.dates.get(&ms.to_string());
        if value.is_none() {
            self.missing.set(true);
        }
        value
    }
    pub fn iso(&self, ms: Option<i64>) -> V {
        ms.and_then(|m| self.date(m)).map(|d| json!(d.iso)).unwrap_or(V::Null)
    }
    pub fn clock(&self, ms: i64) -> String {
        self.word(ms, |d| d.clock.as_ref())
    }
    pub fn day(&self, ms: i64) -> String {
        self.word(ms, |d| d.day.as_ref())
    }
    fn word(&self, ms: i64, pick: impl Fn(&Date) -> Option<&String>) -> String {
        match self.date(ms).map(pick) {
            Some(Some(word)) => word.clone(),
            Some(None) => {
                self.missing.set(true);
                String::new()
            }
            None => String::new(),
        }
    }
    pub fn compare(&self, a: &str, b: &str) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        match (self.order.get(a), self.order.get(b)) {
            (Some(a), Some(b)) => a.cmp(b),
            _ => {
                self.missing.set(true);
                a.cmp(b)
            }
        }
    }
}
/// ECMAScript Math.round on the input binary64 value. Do not add 0.5: that
/// addition can itself round across a half boundary. Outside (-0.5, 0), subtracting
/// floor is exact (Sterbenz, or a zero floor); compare that exact fraction instead.
/// Integral binary64 values stay unchanged, including magnitudes >= 2^52. Ties go
/// toward positive infinity, and the negative-zero and nonfinite cases match JS.
pub fn math_round(value: f64) -> f64 {
    if !value.is_finite() || value == 0.0 {
        return value;
    }
    if (-0.5..0.0).contains(&value) {
        return -0.0;
    }
    let lower = value.floor();
    if value - lower < 0.5 { lower } else { lower + 1.0 }
}

/// Integer wording is proved identical to JS String within this decimal domain.
/// Wider/nonfinite words request the existing logged fallback, never host formatting.
pub fn round_word(value: f64) -> Option<String> {
    let rounded = math_round(value);
    if !rounded.is_finite() || rounded.abs() > 1e12 {
        return None;
    }
    Some(if rounded == 0.0 {
        "0".into()
    } else {
        rounded.to_string()
    })
}

pub fn age(now: i64, at: Option<i64>) -> V {
    at.map(|t| json!(math_round((now - t) as f64 / 1000.0).max(0.0)))
        .unwrap_or(V::Null)
}
pub fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}
pub fn s(v: &V) -> &str {
    v.as_str().unwrap_or("")
}
pub fn array(v: &V) -> Vec<V> {
    v.as_array().cloned().unwrap_or_default()
}
pub fn number(v: &V) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}
pub fn count(v: &V) -> Option<u64> {
    v.as_u64().filter(|n| *n <= 1_000_000_000)
}
/// ECMAScript toPrecision rounding over the admitted numeric domain, using the exact binary
/// significand/rational and ties toward the larger magnitude, not host half-even formatting.
pub fn significant(value: f64, digits: u32) -> Option<f64> {
    if value == 0.0 {
        return Some(0.0);
    }
    if !value.is_finite() || !(1e-22..=1e12).contains(&value.abs()) || !(1..=10).contains(&digits) {
        return None;
    }
    let exponent: i32 = format!("{:e}", value.abs()).split_once('e')?.1.parse().ok()?;
    let scale = digits as i32 - 1 - exponent;
    let bits = value.abs().to_bits();
    let mantissa = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let binary_exponent = ((bits >> 52) & 2047) as i32 - 1023 - 52;
    let mut numerator = mantissa as u128;
    let mut denominator = 1u128;
    if scale >= 0 {
        numerator = numerator.checked_mul(5u128.checked_pow(scale as u32)?)?;
    } else {
        denominator = 5u128.checked_pow((-scale) as u32)?;
    }
    let exponent = binary_exponent + scale;
    if exponent >= 0 {
        numerator = numerator.checked_shl(exponent as u32)?;
    } else {
        denominator = denominator.checked_shl((-exponent) as u32)?;
    }
    let quotient = numerator / denominator;
    let remainder = numerator % denominator;
    let rounded = quotient + u128::from(remainder >= denominator - remainder);
    let parsed: f64 = format!("{rounded}e{}", -scale).parse().ok()?;
    Some(if value.is_sign_negative() { -parsed } else { parsed })
}
pub fn rounded(value: Option<f64>) -> Option<V> {
    match value {
        Some(v) => Some(json!(significant(v, 4)?)),
        None => Some(V::Null),
    }
}
pub fn median(values: &[f64]) -> Option<f64> {
    let mut v = values.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n == 0 {
        None
    } else if n % 2 == 1 {
        Some(v[n / 2])
    } else {
        Some((v[n / 2 - 1] + v[n / 2]) / 2.0)
    }
}
pub fn js_number(value: f64) -> String {
    if value != 0.0 && value.abs() < 1e-6 {
        format!("{value:e}")
    } else {
        value.to_string()
    }
}
pub fn fixed(value: f64, places: u32) -> Option<String> {
    if !value.is_finite() || value.abs() > 1e12 || (value != 0.0 && value.abs() < 1e-22) || places > 6 {
        return None;
    }
    if value == 0.0 {
        return Some(if places == 0 {
            "0".into()
        } else {
            format!("0.{:0places$}", 0, places = places as usize)
        });
    }
    let bits = value.abs().to_bits();
    let m = (bits & ((1u64 << 52) - 1)) | (1u64 << 52);
    let e = ((bits >> 52) & 2047) as i32 - 1023 - 52;
    let mut n = m as u128;
    let mut d = 1u128;
    if e >= 0 {
        n = n.checked_shl(e as u32)?;
    } else {
        d = d.checked_shl((-e) as u32)?;
    }
    n = n.checked_mul(10u128.checked_pow(places)?)?;
    let q = n / d;
    let r = n % d;
    let units = q + u128::from(r >= d - r);
    let scale = 10u128.pow(places);
    if places == 0 {
        return Some(format!("{}{}", if value < 0.0 { "-" } else { "" }, units));
    }
    Some(format!(
        "{}{}.{:0places$}",
        if value < 0.0 { "-" } else { "" },
        units / scale,
        units % scale,
        places = places as usize
    ))
}

/// ECMAScript WhiteSpace and LineTerminator, including BOM and excluding NEL.
pub fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' |
        '\u{3000}' | '\u{feff}')
}
