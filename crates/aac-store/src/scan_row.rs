//! The usage scanner's compact row (`aac-usage-scan` `rows`/`srows`) as a typed value.
//!
//! The Analytics writer holds every cached row of every computer across a pass (about 100k rows
//! for a busy computer). As `serde_json::Value` objects each row cost about 1 KB; typed, with
//! the repeated strings (tool, file, model, provider, hour) shared through one interner, a row
//! costs about 200 bytes. The JSON keys stay the scanner's, so saved checkpoints keep their form.
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::cell::RefCell;
use std::collections::HashSet;
use std::fmt;
use std::sync::Arc;

/// One shared copy of each distinct string.
#[derive(Default)]
pub struct Interner {
    strings: HashSet<Arc<str>>,
}

impl Interner {
    /// The shared copy of `text`.
    pub fn intern(&mut self, text: &str) -> Arc<str> {
        if let Some(held) = self.strings.get(text) {
            return Arc::clone(held);
        }
        let held: Arc<str> = Arc::from(text);
        self.strings.insert(Arc::clone(&held));
        held
    }

    /// Forget strings no row holds any more.
    pub fn prune(&mut self) {
        self.strings.retain(|held| Arc::strong_count(held) > 1);
    }
}

thread_local! {
    static INTERNER: RefCell<Option<Interner>> = const { RefCell::new(None) };
}

/// Run `parse` with `interner` serving every row string it deserializes, and hand it back.
pub fn with_interner<T>(interner: Interner, parse: impl FnOnce() -> T) -> (T, Interner) {
    let previous = INTERNER.with(|cell| cell.replace(Some(interner)));
    let out = parse();
    let interner = INTERNER
        .with(|cell| std::mem::replace(&mut *cell.borrow_mut(), previous))
        .unwrap_or_default();
    (out, interner)
}

/// The shared copy of `text` from the interner [`with_interner`] installed, or a fresh one: for
/// other typed readers (file keys of the scanner's prints) that share the rows' strings.
pub fn shared(text: &str) -> Arc<str> {
    INTERNER.with(|cell| match cell.borrow_mut().as_mut() {
        Some(interner) => interner.intern(text),
        None => Arc::from(text),
    })
}

/// One compact usage row: an hour row (`h`) or a session row (`s`, `a`, `z`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ScanRow {
    /// `k`: the scanner kind (tool).
    pub kind: Arc<str>,
    /// `f`: the file key.
    pub file: Arc<str>,
    /// `m`: the model.
    pub model: Arc<str>,
    /// `p`: the routing provider, when the log named one.
    pub provider: Option<Arc<str>>,
    /// `h`: the UTC hour ("YYYY-MM-DD HH") of an hour row.
    pub hour: Option<Arc<str>>,
    /// `s`: the session of a session row.
    pub session: Option<Arc<str>>,
    /// `a`: a session row's first activity (ms).
    pub first: Option<i64>,
    /// `z`: a session row's last activity (ms).
    pub last: Option<i64>,
    /// `i`, `o`, `cr`, `cw`: input, output, cache read and cache write tokens.
    pub tokens: [u64; 4],
    /// `c`: the logged cost.
    pub cost: f64,
    /// `n`: the event count.
    pub events: u64,
    /// `costBasis`: a legacy row's cost basis ("reported" or "unknown").
    pub cost_basis: Option<Arc<str>>,
}

/// The token fields, in [`ScanRow::tokens`] order.
const TOKEN_KEYS: [&str; 4] = ["i", "o", "cr", "cw"];

/// A finite positive number floored, else 0 (the collector's `count`).
fn count(value: &Value) -> u64 {
    if let Some(n) = value.as_u64() {
        return n;
    }
    let n = value.as_f64().unwrap_or(0.0);
    if n.is_finite() && n > 0.0 { n.floor() as u64 } else { 0 }
}

fn instant(value: &Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_f64().filter(|n| n.is_finite()).map(|n| n as i64))
}

impl ScanRow {
    fn set(&mut self, key: &str, value: &Value, intern: &mut dyn FnMut(&str) -> Arc<str>) {
        let text = |value: &Value, intern: &mut dyn FnMut(&str) -> Arc<str>| value.as_str().map(&mut *intern);
        match key {
            "k" => self.kind = text(value, intern).unwrap_or_default(),
            "f" => self.file = text(value, intern).unwrap_or_default(),
            "m" => self.model = text(value, intern).unwrap_or_default(),
            "p" => self.provider = text(value, intern),
            "h" => self.hour = text(value, intern),
            "s" => self.session = text(value, intern),
            "a" => self.first = instant(value),
            "z" => self.last = instant(value),
            "c" => self.cost = value.as_f64().unwrap_or(0.0),
            "n" => self.events = count(value),
            "costBasis" => self.cost_basis = text(value, intern),
            _ => {
                if let Some(index) = TOKEN_KEYS.iter().position(|token| *token == key) {
                    self.tokens[index] = count(value);
                }
            }
        }
    }

    /// A row of a scanner answer, its strings shared through `interner`.
    pub fn from_value(row: &Value, interner: &mut Interner) -> Self {
        let mut out = ScanRow::default();
        if let Some(fields) = row.as_object() {
            let mut intern = |text: &str| interner.intern(text);
            for (key, value) in fields {
                out.set(key, value, &mut intern);
            }
        }
        out
    }

    /// The row as the scanner's JSON object.
    pub fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// The provider, empty when none.
    pub fn provider_str(&self) -> &str {
        self.provider.as_deref().unwrap_or("")
    }
}

impl Serialize for ScanRow {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("k", &*self.kind)?;
        map.serialize_entry("f", &*self.file)?;
        if let Some(session) = &self.session {
            map.serialize_entry("s", &**session)?;
        }
        map.serialize_entry("m", &*self.model)?;
        if let Some(provider) = &self.provider {
            map.serialize_entry("p", &**provider)?;
        }
        if let Some(hour) = &self.hour {
            map.serialize_entry("h", &**hour)?;
        }
        if let Some(first) = self.first {
            map.serialize_entry("a", &first)?;
        }
        if let Some(last) = self.last {
            map.serialize_entry("z", &last)?;
        }
        for (key, value) in TOKEN_KEYS.iter().zip(self.tokens) {
            map.serialize_entry(key, &value)?;
        }
        map.serialize_entry("c", &self.cost)?;
        map.serialize_entry("n", &self.events)?;
        if let Some(basis) = &self.cost_basis {
            map.serialize_entry("costBasis", &**basis)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for ScanRow {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct RowVisitor;
        impl<'de> Visitor<'de> for RowVisitor {
            type Value = ScanRow;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a scanner row object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ScanRow, A::Error> {
                let mut row = ScanRow::default();
                let mut intern = |text: &str| shared(text);
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    // One field at a time: a small value, never the whole row as a tree.
                    let value: Value = map.next_value()?;
                    row.set(&key, &value, &mut intern);
                }
                Ok(row)
            }
            // Anything but an object (a damaged legacy cache) reads as a row of no tool, which
            // no tool's report ever counts, as the JSON-object rows behaved.
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<ScanRow, A::Error> {
                while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(ScanRow::default())
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<ScanRow, E> {
                Ok(ScanRow::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<ScanRow, E> {
                Ok(ScanRow::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<ScanRow, E> {
                Ok(ScanRow::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<ScanRow, E> {
                Ok(ScanRow::default())
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<ScanRow, E> {
                Ok(ScanRow::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<ScanRow, E> {
                Ok(ScanRow::default())
            }
        }
        deserializer.deserialize_any(RowVisitor)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn rows_round_trip_the_scanner_keys() {
        for row in [
            json!({"k":"codex","f":"a","m":"gpt","p":"openai","h":"2026-11-26 15",
                   "i":1,"o":2,"cr":3,"cw":4,"c":0.5,"n":2}),
            json!({"k":"claude","f":"b","s":"x","m":"opus","a":10,"z":20,
                   "i":1,"o":0,"cr":0,"cw":0,"c":0.0,"n":1}),
            json!({"k":"codex","f":"c","m":"m","h":"2026-11-26 15","i":1,"o":0,"cr":0,"cw":0,
                   "c":1.25,"n":1,"costBasis":"reported"}),
        ] {
            let typed: ScanRow = serde_json::from_value(row.clone()).expect("typed row");
            assert_eq!(typed.to_value(), row);
            assert_eq!(ScanRow::from_value(&row, &mut Interner::default()), typed);
        }
    }

    #[test]
    fn odd_numbers_and_types_read_as_the_collector_counts_them() {
        let row: ScanRow = serde_json::from_value(json!({
            "k":"codex","f":null,"m":7,"i":2.9,"o":-3,"cr":"x","n":1.5,"c":"bad"
        }))
        .expect("lenient row");
        assert_eq!(row.tokens, [2, 0, 0, 0]);
        assert_eq!(row.events, 1);
        assert_eq!(row.cost, 0.0);
        assert_eq!(&*row.file, "");
        assert_eq!(&*row.model, "");
    }

    #[test]
    fn a_damaged_row_reads_as_a_row_of_no_tool() {
        let rows: Vec<ScanRow> = serde_json::from_str(r#"[1, "x", null, [2, 3], true, {"k":"codex"}]"#).expect("rows");
        assert_eq!(rows.len(), 6);
        assert!(rows[..5].iter().all(|row| row.kind.is_empty()));
        assert_eq!(&*rows[5].kind, "codex");
    }

    #[test]
    fn an_interner_shares_one_copy_of_each_string() {
        let text = r#"[{"k":"codex","f":"a","m":"gpt","h":"2026-11-26 15"},
                       {"k":"codex","f":"a","m":"gpt","h":"2026-11-26 15"}]"#;
        let (rows, mut interner) = with_interner(Interner::default(), || {
            serde_json::from_str::<Vec<ScanRow>>(text).expect("rows")
        });
        assert!(Arc::ptr_eq(&rows[0].file, &rows[1].file));
        assert!(Arc::ptr_eq(
            &rows[0].hour.clone().unwrap(),
            &rows[1].hour.clone().unwrap()
        ));
        drop(rows);
        interner.prune();
        assert!(interner.strings.is_empty());
    }
}
