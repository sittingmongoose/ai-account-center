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
    /// A time in three states: absent (`null`, or a `YYYY-MM-DDT` string the envelope gave no
    /// finite parse: NaN in TS), a time (a safe integer, or a string with an epoch fact), or
    /// unreadable. Unreadable (a fractional or unsafe number, another string or type) marks the
    /// context invalid, so the view declines instead of reading the value as "no time".
    pub fn epoch(&self, value: &V) -> Option<i64> {
        if value.is_null() {
            return None;
        }
        if let Some(ms) = value.as_i64().filter(|ms| ms.unsigned_abs() <= 9_007_199_254_740_991) {
            return Some(ms);
        }
        if let Some(text) = value.as_str() {
            if let Some(ms) = self.facts.epochs.get(text) {
                return Some(*ms);
            }
            if iso_prefixed(text) {
                return None;
            }
        }
        self.missing.set(true);
        None
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
/// `Object.prototype` member names. TS reads `{...}[name]` on a plain object as the inherited member (a function,
/// or `Object.prototype` itself), which no JSON lookup reproduces.
pub const PROTO_NAMES: [&str; 12] = [
    "constructor",
    "__defineGetter__",
    "__defineSetter__",
    "hasOwnProperty",
    "__lookupGetter__",
    "__lookupSetter__",
    "isPrototypeOf",
    "propertyIsEnumerable",
    "toString",
    "valueOf",
    "__proto__",
    "toLocaleString",
];

/// True when a key or string anywhere in `value` holds an `Object.prototype` member name as a word (split at every
/// character an identifier cannot hold, so "constructor.fixture" counts). A Status or Scoreboard input with one
/// declines: TS may look such a name up in a plain object (a quota meter label, a catalog metric) and answer with
/// the inherited member (big review, Fable L1).
pub fn proto_named(value: &V) -> bool {
    let named = |text: &str| {
        text.split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '$'))
            .any(|word| PROTO_NAMES.contains(&word))
    };
    match value {
        V::String(text) => named(text),
        V::Array(rows) => rows.iter().any(proto_named),
        V::Object(map) => map.iter().any(|(key, v)| named(key) || proto_named(v)),
        _ => false,
    }
}

/// `/^\d{4}-\d{2}-\d{2}T/`: the strings the presentation envelope gives a `Date.parse` epoch whenever
/// one is finite (`rust-read-input.ts`), so a missing fact for one of them is NaN, not unknown.
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
    at.map(|t| age_of((now - t) as f64)).unwrap_or(V::Null)
}
/// status-view-words.ts `ageSeconds` on a binary64 difference: `Math.max(0, Math.round(d / 1000))`,
/// whose `Math.max` answers +0 for a negative or -0 rounding (`f64::max` may keep -0).
pub fn age_of(difference_ms: f64) -> V {
    let seconds = math_round(difference_ms / 1000.0);
    json!(if seconds > 0.0 { seconds } else { 0.0 })
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
/// JS String(number) only within the counts/significant-number domain exercised by these
/// composers. Wider shortest-decimal and exponent forms refuse the read for TS fallback.
pub fn js_number(value: f64) -> Option<String> {
    if !value.is_finite() || value.abs() > 1e12 || (value != 0.0 && value.abs() < 1e-22) {
        return None;
    }
    if value.fract() != 0.0 && significant(value, 10)? != value {
        return None;
    }
    Some(if value == 0.0 {
        "0".into()
    } else if value.abs() < 1e-6 {
        format!("{value:e}")
    } else {
        value.to_string()
    })
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

#[cfg(test)]
mod lows_tests {
    use super::js_number;
    use serde_json::{Value, json};

    /// Big review (Fable L1): a prototype member name anywhere in a Status or Scoreboard input declines it.
    #[test]
    fn prototype_member_names_are_found_as_words() {
        for value in [
            json!({"meter": "constructor"}),
            json!({"metric": {"of": "__proto__"}}),
            json!([{"writer": "constructor.fixture"}]),
            json!({"toString": 1}),
            json!({"a": ["x", "x valueOf y"]}),
        ] {
            assert!(super::proto_named(&value), "{value}");
        }
        for value in [
            json!({"meter": "five_hour", "label": "Constructor"}),
            json!({"note": "construction toStrings value"}),
            json!([1, null, true]),
        ] {
            assert!(!super::proto_named(&value), "{value}");
        }
    }

    /// Big review (Fable M1): a time the context cannot read declines the view; it is never "no time".
    #[test]
    fn epoch_is_absent_a_time_or_unreadable() {
        let fresh = || {
            let facts = json!({"data": {}, "presentation": {
                "dates": {}, "epochs": {"2026-01-01T00:00:00Z": 1767225600000_i64}, "order": {}}});
            super::Context::new(serde_json::from_value::<super::Envelope>(facts).unwrap().presentation)
        };
        for (value, want) in [
            (json!(null), None),
            (json!(1767225600000_i64), Some(1767225600000)),
            (json!("2026-01-01T00:00:00Z"), Some(1767225600000)),
            (json!("2026-13-01T00:00:00Z"), None),
        ] {
            let ctx = fresh();
            assert_eq!(ctx.epoch(&value), want, "{value}");
            assert!(ctx.valid(), "{value} is readable");
        }
        for value in [
            json!(1767225600000.4567),
            json!(9_007_199_254_740_993_u64),
            json!("2026-01-05"),
            json!("yesterday"),
            json!(true),
            json!([1]),
        ] {
            let ctx = fresh();
            assert_eq!(ctx.epoch(&value), None, "{value}");
            assert!(!ctx.valid(), "{value} must decline the view");
        }
        // Math.max(0, Math.round(d / 1000)): never -0, whole seconds, ties toward +infinity.
        assert_eq!(super::age_of(-300.0), json!(0.0));
        assert!(super::age_of(-300.0).as_f64().is_some_and(f64::is_sign_positive));
        assert_eq!(super::age_of(3_599_999.543_3), json!(3600.0));
        assert_eq!(super::age_of(4_999.394), json!(5.0));
    }

    #[test]
    fn js_number_refuses_unproven_forms_and_normalizes_negative_zero() {
        for value in [
            1e21,
            -1e21,
            2251799813685247.2,
            0.12345678901234567,
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            1e-23,
        ] {
            assert_eq!(serde_json::to_value(js_number(value)).unwrap(), Value::Null, "{value}");
        }
        for (value, expected) in [
            (-0.0, "0"),
            (0.0, "0"),
            (1e12, "1000000000000"),
            (0.25, "0.25"),
            (1e-7, "1e-7"),
            (1e-22, "1e-22"),
        ] {
            assert_eq!(serde_json::to_value(js_number(value)).unwrap(), json!(expected));
        }
    }
}
