//! Presentation facts supplied by the existing TS locale boundary. Missing facts refuse the
//! candidate; they never create a default timestamp, collation order or reading.
use serde::Deserialize;
use serde_json::{Value as V, json};
use std::{cell::Cell, cmp::Ordering, collections::HashMap, sync::Arc};

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
pub struct Date {
    iso: String,
    /// Absent when the caller asked for ISO facts only; reading it then refuses the candidate.
    #[serde(default)]
    clock: Option<String>,
    #[serde(default)]
    day: Option<String>,
}
impl Date {
    /// R2: the request's own clock, whose words the front door formats per request (its ISO text is
    /// `Date#toISOString`, which aac-num writes exactly).
    pub fn of_now(ms: i64, clock: String, day: String) -> Option<Self> {
        Some(Self {
            iso: aac_num::ecma::to_iso_string(ms)?,
            clock: Some(clock),
            day: Some(day),
        })
    }
    /// `Date#toISOString` of this instant. In the admitted domain it is 24 bytes.
    pub fn iso_text(&self) -> &str {
        &self.iso
    }
}
pub struct Context {
    facts: Arc<Presentation>,
    /// R2: the request's `now`, which a fed generation's facts cannot hold.
    now: Option<(i64, Date)>,
    missing: Cell<bool>,
}
impl Context {
    pub fn new(facts: Presentation) -> Self {
        Self::shared(Arc::new(facts), None)
    }
    /// R2: facts held once per fed generation, shared by every request that names it, plus the
    /// request's own `now` (its date facts), which is looked up before the generation's.
    pub fn shared(facts: Arc<Presentation>, now: Option<(i64, Date)>) -> Self {
        Self {
            facts,
            now,
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
        if let Some(ms) = value
            .as_i64()
            .filter(|ms| ms.unsigned_abs() <= aac_num::ecma::MAX_SAFE_INTEGER as u64)
        {
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
            eprintln!("COMPOSE-MISS epoch: {value}");
        None
    }
    fn date(&self, ms: i64) -> Option<&Date> {
        if let Some((at, date)) = &self.now
            && *at == ms
        {
            return Some(date);
        }
        let value = self.facts.dates.get(&ms.to_string());
        if value.is_none() {
            self.missing.set(true);
            eprintln!("COMPOSE-MISS date: {ms}");
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
                eprintln!("COMPOSE-MISS word: {ms}");
                String::new()
            }
            None => String::new(),
        }
    }
    /// The envelope's `localeCompare` rank of a word, without marking it missing when absent.
    pub fn rank(&self, word: &str) -> Option<usize> {
        self.facts.order.get(word).copied()
    }
    pub fn compare(&self, a: &str, b: &str) -> Ordering {
        if a == b {
            return Ordering::Equal;
        }
        match (self.facts.order.get(a), self.facts.order.get(b)) {
            (Some(a), Some(b)) => a.cmp(b),
            _ => {
                self.missing.set(true);
                eprintln!("COMPOSE-MISS compare: {a} / {b}");
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

/// `/^\d{4}-\d{2}-\d{2}T/` (one rule, in aac-num): the strings the presentation envelope gives a `Date.parse`
/// epoch whenever one is finite (`rust-read-input.ts`), so a missing fact for one of them is NaN, not unknown.
pub use aac_num::ecma::iso_prefixed;

pub fn age(now: i64, at: Option<i64>) -> V {
    at.map(|t| age_of((now - t) as f64)).unwrap_or(V::Null)
}
/// status-view-words.ts `ageSeconds` on a binary64 difference: `Math.max(0, Math.round(d / 1000))`,
/// whose `Math.max` answers +0 for a negative or -0 rounding (`f64::max` may keep -0).
pub fn age_of(difference_ms: f64) -> V {
    let seconds = aac_num::ecma::math_round(difference_ms / 1000.0);
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
/// [`array`] without the copy: the rows of an array, or none. For rows that are only read (R2: a per-row copy of
/// every card or thread made the large share's view take 170 ms).
pub fn rows(v: &V) -> &[V] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}
pub fn number(v: &V) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}
pub fn count(v: &V) -> Option<u64> {
    v.as_u64().filter(|n| *n <= 1_000_000_000)
}
pub fn rounded(value: Option<f64>) -> Option<V> {
    match value {
        Some(v) => Some(json!(aac_num::view::precision(v, 4)?)),
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
/// ECMAScript WhiteSpace and LineTerminator, including BOM and excluding NEL.
pub fn js_whitespace(c: char) -> bool {
    matches!(c, '\u{0009}'..='\u{000d}' | '\u{0020}' | '\u{00a0}' | '\u{1680}' |
        '\u{2000}'..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' |
        '\u{3000}' | '\u{feff}')
}

#[cfg(test)]
mod lows_tests {
    use serde_json::json;

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
}
