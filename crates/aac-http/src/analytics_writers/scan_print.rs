//! The scanner's per-file prints (`fingerprints`), typed. A print keeps the JSON text the scanner
//! wrote, held once, and the writer reads the few fields it compares straight from that text.
//!
//! As `serde_json::Value` trees a busy computer's prints (about 45k, most of their bytes in the
//! readers' `appendState`) cost several times their text, and every pass cloned the whole map
//! into the scan request. The text is what the scanner reads back, so checkpoints and requests
//! keep their bytes.
use indexmap::IndexMap;
use serde::de::{self, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::{SerializeMap, Serializer};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use serde_json::Value;
use serde_json::value::RawValue;
use std::borrow::Cow;
use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;

/// One file's print: the scanner's JSON text.
#[derive(Clone, Debug)]
pub(super) struct Print(Box<RawValue>);

/// The top-level fields of a print the writer reads, each as its JSON text (None when absent; a
/// present `null` is kept, because some checks ask whether a field is there at all).
#[derive(Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Fields<'a> {
    #[serde(default, borrow, deserialize_with = "present")]
    pub size: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub mtime_ms: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub head: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub tail: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub wal_size: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub wal_mtime_ms: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub file_identity: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub read_through: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub append_state: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub read_from: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub settled_confirmed: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub settled_checked_slot: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub path_key: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub newest_usage_ms: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub usage_skipped: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub unread_usage: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub claude_name: Option<&'a RawValue>,
    #[serde(default, borrow, deserialize_with = "present")]
    pub claude_mirror: Option<&'a RawValue>,
}

fn present<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(deserializer).map(Some)
}

/// A field's text, None when absent or `null` (as `Value` indexing reads both).
pub(super) fn text(field: Option<&RawValue>) -> Option<&str> {
    field.map(RawValue::get).filter(|text| *text != "null")
}

/// `Value::as_u64` of a field.
pub(super) fn as_u64(field: Option<&RawValue>) -> Option<u64> {
    text(field)?.parse().ok()
}

/// `Value::as_i64` of a field.
pub(super) fn as_i64(field: Option<&RawValue>) -> Option<i64> {
    text(field)?.parse().ok()
}

/// `field == true`.
pub(super) fn is_true(field: Option<&RawValue>) -> bool {
    text(field) == Some("true")
}

/// `field.is_string()`.
pub(super) fn is_string(field: Option<&RawValue>) -> bool {
    text(field).is_some_and(|text| text.starts_with('"'))
}

/// `left == right`, absent and `null` alike. The scanner writes canonical JSON, so equal values
/// have equal text; a differently spelled equal value (a legacy cache's) reads as changed, and
/// the file is read again.
pub(super) fn same(left: Option<&RawValue>, right: Option<&RawValue>) -> bool {
    text(left) == text(right)
}

impl Print {
    /// The fields the writer reads. A print that is not an object has none.
    pub fn fields(&self) -> Fields<'_> {
        if !self.is_object() {
            return Fields::default();
        }
        serde_json::from_str(self.0.get()).unwrap_or_default()
    }

    pub fn is_object(&self) -> bool {
        self.0.get().starts_with('{')
    }

    /// The JSON text.
    pub fn text(&self) -> &str {
        self.0.get()
    }

    #[cfg(test)]
    pub fn from_value(value: &Value) -> Self {
        Print(
            serde_json::value::to_raw_value(value)
                .unwrap_or_else(|_| RawValue::from_string("null".into()).expect("null")),
        )
    }

    #[cfg(test)]
    pub fn to_value(&self) -> Value {
        serde_json::from_str(self.0.get()).unwrap_or(Value::Null)
    }

    /// The print with its top-level entries changed by `change`, in their order (a duplicate key
    /// read as its last value at its first place, as a JSON object map reads it). Only an object
    /// changes.
    fn edit<'s>(&'s self, change: impl FnOnce(&mut IndexMap<String, &'s RawValue>)) -> Print {
        let Ok(mut fields) = serde_json::from_str::<Entries<'s>>(self.0.get()) else {
            return self.clone();
        };
        change(&mut fields.0);
        match serde_json::to_string(&fields).map(RawValue::from_string) {
            Ok(Ok(raw)) => Print(raw),
            _ => self.clone(),
        }
    }

    /// The print with `key` set to `value` (in place when present, else last).
    pub fn with<'s>(&'s self, key: &str, value: &'s RawValue) -> Print {
        self.edit(|fields| {
            fields.insert(key.to_owned(), value);
        })
    }

    /// The print without `key`.
    pub fn without(&self, key: &str) -> Print {
        self.edit(|fields| {
            fields.shift_remove(key);
        })
    }
}

/// An object's entries, in order, their values as text.
struct Entries<'a>(IndexMap<String, &'a RawValue>);

impl Serialize for Entries<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Entries<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Entries<'de>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a print object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Entries<'de>, A::Error> {
                let mut out = IndexMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    let value: &'de RawValue = map.next_value()?;
                    out.insert(key, value);
                }
                Ok(Entries(out))
            }
        }
        deserializer.deserialize_map(V)
    }
}

impl Serialize for Print {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for Print {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Box::<RawValue>::deserialize(deserializer).map(Print)
    }
}

/// One tool's prints by file key, in the scanner's order. The keys share the rows' strings.
#[derive(Clone, Debug, Default)]
pub(super) struct Prints(pub IndexMap<Arc<str>, Print>);

impl std::ops::Deref for Prints {
    type Target = IndexMap<Arc<str>, Print>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Prints {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[cfg(test)]
impl Prints {
    pub fn to_value(&self) -> Value {
        Value::Object(
            self.iter()
                .map(|(key, print)| (key.to_string(), print.to_value()))
                .collect(),
        )
    }

    /// Test and migration input: an object's entries as prints (anything else has none).
    pub fn from_value(value: &Value) -> Self {
        Prints(
            value
                .as_object()
                .into_iter()
                .flatten()
                .map(|(key, print)| (Arc::from(key.as_str()), Print::from_value(print)))
                .collect(),
        )
    }
}

impl Serialize for Prints {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (key, print) in self.iter() {
            map.serialize_entry(&**key, print)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Prints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer
            .deserialize_any(PrintsVisitor)
            .map(Option::unwrap_or_default)
    }
}

/// A tool's prints, or None for a value that is not an object (it holds no prints).
pub(super) struct PrintsVisitor;

impl<'de> Visitor<'de> for PrintsVisitor {
    type Value = Option<Prints>;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a map of prints")
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Option<Prints>, A::Error> {
        let mut out = IndexMap::new();
        while let Some(key) = map.next_key::<Cow<'de, str>>()? {
            let print: Print = map.next_value()?;
            out.insert(aac_store::scan_row::shared(&key), print);
        }
        Ok(Some(Prints(out)))
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Option<Prints>, A::Error> {
        while seq.next_element::<IgnoredAny>()?.is_some() {}
        Ok(None)
    }
    fn visit_bool<E: de::Error>(self, _: bool) -> Result<Option<Prints>, E> {
        Ok(None)
    }
    fn visit_i64<E: de::Error>(self, _: i64) -> Result<Option<Prints>, E> {
        Ok(None)
    }
    fn visit_u64<E: de::Error>(self, _: u64) -> Result<Option<Prints>, E> {
        Ok(None)
    }
    fn visit_f64<E: de::Error>(self, _: f64) -> Result<Option<Prints>, E> {
        Ok(None)
    }
    fn visit_str<E: de::Error>(self, _: &str) -> Result<Option<Prints>, E> {
        Ok(None)
    }
    fn visit_unit<E: de::Error>(self) -> Result<Option<Prints>, E> {
        Ok(None)
    }
}

/// Every tool's prints, in their order.
#[derive(Clone, Debug, Default)]
pub(super) struct Fingerprints(pub IndexMap<String, Prints>);

impl std::ops::Deref for Fingerprints {
    type Target = IndexMap<String, Prints>;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl std::ops::DerefMut for Fingerprints {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Fingerprints {
    /// The prints of every tool.
    pub fn count(&self) -> usize {
        self.values().map(|prints| prints.len()).sum()
    }

    #[cfg(test)]
    pub fn to_value(&self) -> Value {
        Value::Object(
            self.iter()
                .map(|(kind, prints)| (kind.clone(), prints.to_value()))
                .collect(),
        )
    }

    #[cfg(test)]
    pub fn from_value(value: &Value) -> Self {
        Fingerprints(
            value
                .as_object()
                .into_iter()
                .flatten()
                .map(|(kind, prints)| (kind.clone(), Prints::from_value(prints)))
                .collect(),
        )
    }
}

impl Serialize for Fingerprints {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.len()))?;
        for (kind, prints) in self.iter() {
            map.serialize_entry(kind, prints)?;
        }
        map.end()
    }
}

impl<'de> Deserialize<'de> for Fingerprints {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Fingerprints;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a map of tools' prints")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Fingerprints, A::Error> {
                let mut out = IndexMap::new();
                while let Some(kind) = map.next_key::<String>()? {
                    let prints: Prints = map.next_value()?;
                    out.insert(kind, prints);
                }
                Ok(Fingerprints(out))
            }
        }
        deserializer.deserialize_map(V)
    }
}

/// `aac_usage::collector::settled_print` over a typed print: read through its end, older than
/// the cutoff, no usage skipped or left undecoded.
pub(super) fn settled(fields: &Fields<'_>, cutoff: i64) -> bool {
    is_string(fields.path_key)
        && as_i64(fields.mtime_ms).is_some_and(|mtime| mtime < cutoff)
        && as_i64(fields.newest_usage_ms).is_some_and(|usage| usage < cutoff)
        && fields.read_through.is_none()
        && !is_true(fields.usage_skipped)
        && !is_true(fields.unread_usage)
}

/// The same file as last read: its size, time, head and tail digests and write-ahead log.
pub(super) fn same_file(old: &Fields<'_>, fresh: &Fields<'_>) -> bool {
    same(old.size, fresh.size)
        && same(old.mtime_ms, fresh.mtime_ms)
        && same(old.head, fresh.head)
        && same(old.tail, fresh.tail)
        && same(old.wal_size, fresh.wal_size)
        && same(old.wal_mtime_ms, fresh.wal_mtime_ms)
        && (text(old.file_identity).is_none() || same(old.file_identity, fresh.file_identity))
}

/// A Claude print's identity: its conversation name and whether this file is a mirror copy.
pub(super) fn claude_identity(fields: &Fields<'_>) -> Option<(Arc<str>, bool)> {
    let name = text(fields.claude_name).filter(|text| text.starts_with('"'))?;
    let name: Cow<'_, str> = serde_json::from_str(name).ok()?;
    if name.is_empty() {
        return None;
    }
    let mirror = match text(fields.claude_mirror)? {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    Some((Arc::from(&*name), mirror))
}

/// The Claude identities of a computer's prints, by file key: what mirror selection reads.
#[derive(Clone, Debug, Default)]
pub(super) struct Identities(HashMap<Arc<str>, (Arc<str>, bool)>);

impl Identities {
    pub fn of(prints: &Prints) -> Self {
        let mut names: HashMap<Arc<str>, Arc<str>> = HashMap::new();
        Identities(
            prints
                .iter()
                .filter_map(|(file, print)| {
                    let (name, mirror) = claude_identity(&print.fields())?;
                    let name = Arc::clone(names.entry(Arc::clone(&name)).or_insert(name));
                    Some((Arc::clone(file), (name, mirror)))
                })
                .collect(),
        )
    }

    /// Identities of a prints object.
    #[cfg(test)]
    pub fn from_value(value: &Value) -> Self {
        Self::of(&Prints::from_value(value))
    }

    pub fn get(&self, file: &str) -> Option<(&str, bool)> {
        self.0.get(file).map(|(name, mirror)| (&**name, *mirror))
    }

    pub fn values(&self) -> impl Iterator<Item = (&str, bool)> {
        self.0.values().map(|(name, mirror)| (&**name, *mirror))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn print(value: Value) -> Print {
        Print::from_value(&value)
    }

    #[test]
    fn typed_settled_check_matches_the_scanners() {
        let base = json!({"pathKey":"k","mtimeMs":5,"newestUsageMs":5,"appendState":{"a":[1,2]}});
        let mut cases = vec![base.clone()];
        for (key, value) in [
            ("pathKey", json!(null)),
            ("pathKey", json!(7)),
            ("mtimeMs", json!(20)),
            ("mtimeMs", json!(5.5)),
            ("mtimeMs", json!(null)),
            ("newestUsageMs", json!(20)),
            ("newestUsageMs", json!("5")),
            ("readThrough", json!(null)),
            ("readThrough", json!(3)),
            ("usageSkipped", json!(true)),
            ("usageSkipped", json!(false)),
            ("unreadUsage", json!(true)),
            ("unreadUsage", json!(1)),
        ] {
            let mut case = base.clone();
            case[key] = value;
            cases.push(case);
        }
        cases.push(json!(5));
        cases.push(json!({"pathKey":"k","mtimeMs":-1,"newestUsageMs":-3}));
        for case in cases {
            for cutoff in [0, 10] {
                assert_eq!(
                    settled(&print(case.clone()).fields(), cutoff),
                    aac_usage::collector::settled_print(&case, cutoff),
                    "{case} at {cutoff}"
                );
            }
        }
    }

    #[test]
    fn typed_file_comparison_matches_value_equality() {
        fn by_value(old: &Value, fresh: &Value) -> bool {
            ["size", "mtimeMs", "head", "tail", "walSize", "walMtimeMs"]
                .into_iter()
                .all(|key| old[key] == fresh[key])
                && (old["fileIdentity"].is_null() || old["fileIdentity"] == fresh["fileIdentity"])
        }
        let base = json!({"size":1,"mtimeMs":2,"head":"h","tail":"t","fileIdentity":"9:1"});
        let mut variants = vec![base.clone(), json!({}), json!(3)];
        for (key, value) in [
            ("size", json!(2)),
            ("size", json!(null)),
            ("head", json!("x")),
            ("walSize", json!(4)),
            ("walMtimeMs", json!(null)),
            ("fileIdentity", json!(null)),
            ("fileIdentity", json!("9:2")),
            ("tail", json!({"nested":[1]})),
        ] {
            let mut case = base.clone();
            case[key] = value;
            variants.push(case);
        }
        for old in &variants {
            for fresh in &variants {
                assert_eq!(
                    same_file(&print(old.clone()).fields(), &print(fresh.clone()).fields()),
                    by_value(old, fresh),
                    "{old} vs {fresh}"
                );
            }
        }
    }

    #[test]
    fn edits_keep_the_other_entries_and_their_order() {
        let original = print(json!({"size":1,"readFrom":4,"appendState":{"x":"y"},"z":null}));
        assert_eq!(
            original.without("readFrom").text(),
            r#"{"size":1,"appendState":{"x":"y"},"z":null}"#
        );
        let slot = RawValue::from_string("7".into()).expect("slot");
        assert_eq!(
            original.with("size", &slot).text(),
            r#"{"size":7,"readFrom":4,"appendState":{"x":"y"},"z":null}"#
        );
        assert_eq!(
            original.with("settledCheckedSlot", &slot).text(),
            r#"{"size":1,"readFrom":4,"appendState":{"x":"y"},"z":null,"settledCheckedSlot":7}"#
        );
        assert_eq!(print(json!(5)).without("size").text(), "5");
        // The same as editing the object map.
        let mut value = original.to_value();
        value
            .as_object_mut()
            .expect("object")
            .shift_remove("readFrom");
        assert_eq!(original.without("readFrom").text(), value.to_string());
    }

    #[test]
    fn prints_read_leniently_and_write_their_text() {
        let text =
            r#"{"codex":{"a":{"size":1,"appendState":{"p":[1,{"q":"r"}]}},"b":5},"claude":7}"#;
        let prints: Fingerprints = serde_json::from_str(text).expect("prints");
        assert_eq!(prints["codex"].len(), 2);
        assert!(prints["claude"].is_empty());
        assert_eq!(
            serde_json::to_string(&prints).expect("written"),
            r#"{"codex":{"a":{"size":1,"appendState":{"p":[1,{"q":"r"}]}},"b":5},"claude":{}}"#
        );
    }

    #[test]
    fn claude_identities_read_name_and_mirror() {
        let prints = Prints::from_value(&json!({
            "a":{"claudeName":"n\u{e9}","claudeMirror":true},
            "b":{"claudeName":"","claudeMirror":false},
            "c":{"claudeName":"m","claudeMirror":"yes"},
            "d":{"claudeName":"m","claudeMirror":false},
            "e":{"claudeName":"n\\\"q","claudeMirror":false},
        }));
        let identities = Identities::of(&prints);
        assert_eq!(identities.get("a"), Some(("n\u{e9}", true)));
        assert_eq!(identities.get("b"), None);
        assert_eq!(identities.get("c"), None);
        assert_eq!(identities.get("d"), Some(("m", false)));
        assert_eq!(identities.get("e"), Some(("n\\\"q", false)));
    }
}
