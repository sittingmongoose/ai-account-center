//! The scanner's answer (version 2), typed as the writers read it: each tool's read with its prints
//! as text, the rows as typed rows sharing the cache's strings, and the small discovery parts as
//! values. The identity evidence and messages, which the writers do not read, are skipped
//! unparsed. A `serde_json::Value` tree of a busy computer's answer (up to 16 MiB of text) was
//! several times its size, and the transport cloned it once more.
use super::scan_print::{Prints, PrintsVisitor};
use aac_store::scan_row::{Interner, ScanRow, with_interner};
use indexmap::IndexMap;
use serde::Deserialize;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};
use std::collections::HashSet;
use std::fmt;

static NULL: Value = Value::Null;

#[derive(Default)]
pub(super) struct Answer {
    pub version: Value,
    pub reader: Value,
    pub truncated: Value,
    /// None when `kinds` is not an object.
    pub kinds: Option<IndexMap<String, KindRead>>,
    pub rows: Vec<ScanRow>,
    pub srows: Vec<ScanRow>,
    pub discovery: Value,
    /// The tools with rows or session rows in the answer as it was read (applying a tool moves
    /// its rows into the cache).
    row_kinds: HashSet<String>,
}

/// An agent's answer bytes held until the pass applies them, after the computer's last call:
/// compressed while another call waits.
pub(super) enum Held {
    Plain(Vec<u8>),
    Compressed(Vec<u8>),
}

impl Held {
    /// Compress the bytes (kept as they are when they cannot be).
    pub fn squeeze(&mut self) {
        if let Held::Plain(bytes) = self
            && let Ok(compressed) = zstd::bulk::compress(bytes, 1)
        {
            *self = Held::Compressed(compressed);
        }
    }

    /// The answer bytes.
    pub fn bytes(self) -> std::io::Result<Vec<u8>> {
        match self {
            Held::Plain(bytes) => Ok(bytes),
            Held::Compressed(compressed) => zstd::stream::decode_all(&compressed[..]),
        }
    }
}

/// One tool's read.
#[derive(Default)]
pub(super) struct KindRead {
    /// The prints; None when `fingerprints` is missing or not an object.
    pub fingerprints: Option<Prints>,
    /// `replacedFiles`' strings.
    pub replaced: Vec<String>,
    /// Every other field in its order (the read itself when it is not an object).
    pub meta: Value,
}

impl Answer {
    /// Read an answer, its row strings and print keys shared through `strings`.
    pub fn parse(bytes: &[u8], strings: &mut Interner) -> serde_json::Result<Answer> {
        let (answer, held) = with_interner(std::mem::take(strings), || {
            serde_json::from_slice::<Answer>(bytes)
        });
        *strings = held;
        let mut answer = answer?;
        answer.row_kinds = answer
            .rows
            .iter()
            .chain(&answer.srows)
            .map(|row| row.kind.to_string())
            .collect();
        Ok(answer)
    }

    /// An answer given as a value.
    #[cfg(test)]
    pub fn from_value(value: &Value) -> Answer {
        let bytes = serde_json::to_vec(value).unwrap_or_default();
        Answer::parse(&bytes, &mut Interner::default()).unwrap_or_default()
    }

    /// The tool's read fields (`answer["kinds"][kind]` without its prints and replaced files).
    pub fn read(&self, kind: &str) -> &Value {
        self.kinds
            .as_ref()
            .and_then(|kinds| kinds.get(kind))
            .map_or(&NULL, |read| &read.meta)
    }

    /// Whether the answer as read held rows of `kind`.
    pub fn had_rows(&self, kind: &str) -> bool {
        self.row_kinds.contains(kind)
    }

    /// Move out the rows and session rows of `kind` whose file `keep` accepts.
    pub fn take_rows(
        &mut self,
        kind: &str,
        keep: impl Fn(&str) -> bool,
    ) -> (Vec<ScanRow>, Vec<ScanRow>) {
        let split = |rows: &mut Vec<ScanRow>| {
            let (taken, kept) = std::mem::take(rows)
                .into_iter()
                .partition(|row| &*row.kind == kind && keep(&row.file));
            *rows = kept;
            taken
        };
        (split(&mut self.rows), split(&mut self.srows))
    }
}

impl<'de> Deserialize<'de> for Answer {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Answer;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a scanner answer object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Answer, A::Error> {
                let mut answer = Answer::default();
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    match &*key {
                        "version" => answer.version = map.next_value()?,
                        "reader" => answer.reader = map.next_value()?,
                        "truncated" => answer.truncated = map.next_value()?,
                        "discovery" => answer.discovery = map.next_value()?,
                        "kinds" => answer.kinds = map.next_value::<Kinds>()?.0,
                        "rows" => answer.rows = map.next_value::<Rows>()?.0,
                        "srows" => answer.srows = map.next_value::<Rows>()?.0,
                        _ => {
                            map.next_value::<IgnoredAny>()?;
                        }
                    }
                }
                Ok(answer)
            }
        }
        deserializer.deserialize_map(V)
    }
}

/// The `kinds` object, or None for anything else.
struct Kinds(Option<IndexMap<String, KindRead>>);

impl<'de> Deserialize<'de> for Kinds {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Kinds;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("the scanner's tools")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Kinds, A::Error> {
                let mut kinds = IndexMap::new();
                while let Some(kind) = map.next_key::<String>()? {
                    let read: KindRead = map.next_value()?;
                    kinds.insert(kind, read);
                }
                Ok(Kinds(Some(kinds)))
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Kinds, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(Kinds(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Kinds, E> {
                Ok(Kinds(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Kinds, E> {
                Ok(Kinds(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Kinds, E> {
                Ok(Kinds(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Kinds, E> {
                Ok(Kinds(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Kinds, E> {
                Ok(Kinds(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Kinds, E> {
                Ok(Kinds(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

impl<'de> Deserialize<'de> for KindRead {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        fn other(meta: Value) -> KindRead {
            KindRead {
                meta,
                ..KindRead::default()
            }
        }
        impl<'de> Visitor<'de> for V {
            type Value = KindRead;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("one tool's read")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<KindRead, A::Error> {
                let mut read = KindRead::default();
                let mut meta = Map::new();
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "fingerprints" => read.fingerprints = map.next_value::<OptPrints>()?.0,
                        "replacedFiles" => read.replaced = map.next_value::<Strings>()?.0,
                        _ => {
                            let value: Value = map.next_value()?;
                            meta.insert(key, value);
                        }
                    }
                }
                read.meta = Value::Object(meta);
                Ok(read)
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<KindRead, A::Error> {
                let mut items = Vec::new();
                while let Some(item) = seq.next_element::<Value>()? {
                    items.push(item);
                }
                Ok(other(Value::Array(items)))
            }
            fn visit_bool<E: de::Error>(self, value: bool) -> Result<KindRead, E> {
                Ok(other(Value::Bool(value)))
            }
            fn visit_i64<E: de::Error>(self, value: i64) -> Result<KindRead, E> {
                Ok(other(Value::from(value)))
            }
            fn visit_u64<E: de::Error>(self, value: u64) -> Result<KindRead, E> {
                Ok(other(Value::from(value)))
            }
            fn visit_f64<E: de::Error>(self, value: f64) -> Result<KindRead, E> {
                Ok(other(Value::from(value)))
            }
            fn visit_str<E: de::Error>(self, value: &str) -> Result<KindRead, E> {
                Ok(other(Value::from(value)))
            }
            fn visit_unit<E: de::Error>(self) -> Result<KindRead, E> {
                Ok(other(Value::Null))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct OptPrints(Option<Prints>);

impl<'de> Deserialize<'de> for OptPrints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(PrintsVisitor).map(OptPrints)
    }
}

/// The strings of an array (anything else has none).
struct Strings(Vec<String>);

impl<'de> Deserialize<'de> for Strings {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(Strings(match Value::deserialize(deserializer)? {
            Value::Array(items) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::String(text) => Some(text),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }))
    }
}

/// The rows of an array (anything else has none).
struct Rows(Vec<ScanRow>);

impl<'de> Deserialize<'de> for Rows {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Rows;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("scanner rows")
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Rows, A::Error> {
                let mut rows = Vec::with_capacity(seq.size_hint().unwrap_or(0).min(1 << 16));
                while let Some(row) = seq.next_element::<ScanRow>()? {
                    rows.push(row);
                }
                Ok(Rows(rows))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Rows, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(Rows(Vec::new()))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Rows, E> {
                Ok(Rows(Vec::new()))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Rows, E> {
                Ok(Rows(Vec::new()))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Rows, E> {
                Ok(Rows(Vec::new()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Rows, E> {
                Ok(Rows(Vec::new()))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Rows, E> {
                Ok(Rows(Vec::new()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Rows, E> {
                Ok(Rows(Vec::new()))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_typed_answer_reads_what_the_value_answer_read() {
        let value = json!({
            "version":2,"truncated":false,"reader":{"name":"aac-usage-scan"},
            "kinds":{
                "codex":{"state":"ok","fingerprints":{"f":{"size":1,"readFrom":0}},
                    "replacedFiles":["f",3],"partial":true,"unreadableRecords":2},
                "cursor":{"state":"pending","fingerprints":[1]},
                "devin":7
            },
            "rows":[{"k":"codex","f":"f","m":"m","h":"2026-11-03 03","i":5,"n":1,"c":0},[1],
                {"k":"omp","f":"g","m":"m","h":"2026-11-03 03"}],
            "srows":{"not":"rows"},
            "discovery":{"kinds":{"codex":{"freshness":{"state":"current"}}}},
            "sessions":{"big":{"cwd":"/private"}},
            "messages":[1,2,3],
        });
        let answer = Answer::from_value(&value);
        assert_eq!(answer.version, 2);
        assert_eq!(answer.reader["name"], "aac-usage-scan");
        assert_eq!(answer.truncated, false);
        let kinds = answer.kinds.as_ref().expect("kinds");
        assert_eq!(
            kinds.keys().collect::<Vec<_>>(),
            ["codex", "cursor", "devin"]
        );
        let codex = &kinds["codex"];
        assert_eq!(codex.replaced, ["f"]);
        assert_eq!(
            codex.meta,
            json!({"state":"ok","partial":true,"unreadableRecords":2})
        );
        assert_eq!(
            codex.fingerprints.as_ref().expect("prints")["f"].text(),
            r#"{"size":1,"readFrom":0}"#
        );
        assert!(kinds["cursor"].fingerprints.is_none());
        assert_eq!(answer.read("cursor")["state"], "pending");
        assert_eq!(answer.read("devin"), &json!(7));
        assert_eq!(answer.read("absent"), &Value::Null);
        assert_eq!(answer.rows.len(), 3);
        assert_eq!(&*answer.rows[1].kind, "");
        assert!(answer.srows.is_empty());
        assert!(answer.had_rows("codex") && answer.had_rows("omp") && !answer.had_rows("devin"));
        assert_eq!(
            answer.discovery["kinds"]["codex"]["freshness"]["state"],
            "current"
        );
        assert!(Answer::parse(b"[1]", &mut Interner::default()).is_err());
        let bare = Answer::from_value(&json!({"kinds":5}));
        assert!(bare.kinds.is_none());
    }
}
