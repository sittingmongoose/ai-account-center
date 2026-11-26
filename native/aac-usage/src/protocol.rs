//! The wire format: today's helper request and response (version 2), with `discovery`, `reader`,
//! `sessions` (identity evidence) and `messages` added. Rows, session rows and fingerprints are the
//! Python helper's, so the server's merge, cache and incremental rules are unchanged.

use crate::collector::{Collector, Message, Row, SessionRow};
use crate::discover::{self, Env};
use crate::prior::{CompactHash, PriorPrint, PriorState, RawFingerprints, SettledConfirmed, unpack_settled_into};
use crate::read;
use crate::util::{self, Print};
use base64::{Engine as _, engine::general_purpose::STANDARD_NO_PAD};
use serde::de::{self, Deserializer, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Serialize, Serializer};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const VERSION: u64 = 2;
pub const PROTOCOL: u64 = 3;
pub const NAME: &str = "aac-usage-scan";
pub const KINDS: [&str; 8] = [
    "claude",
    "codex",
    "omp",
    "muse",
    "zcode",
    "antigravity",
    "cursor",
    "devin",
];
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const MAX_SETTLED_BYTES: usize = crate::prior::MAX_SETTLED_BYTES;
const EXTRA_ROOTS_MAX: usize = 16;
const EXTRA_ZCODE_DBS_MAX: usize = 2048;
const EXPERIMENT_ROOTS_MAX: usize = 256;

#[derive(Clone)]
pub struct Request {
    pub kinds: Vec<String>,
    pub min_date_ms: i64,
    pub immutable: bool,
    /// Legacy/test override map; `parse_request` leaves this empty and stores parsed fingerprints
    /// directly in `prior` as compact typed `PriorPrint` structs (R4).
    pub fingerprints: Map<String, Value>,
    /// Compact typed prior fingerprints parsed from both inline `fingerprints` and `settledPacked`.
    pub prior: PriorState,
    pub extra_roots: Map<String, Value>,
    /// Log folders an experiment harness kept (DESIGN 16.5): read through the copy rule, origin
    /// `experiment`. Unlike `extraRoots`, which are tool homes read as defaults.
    pub experiment_roots: Map<String, Value>,
    pub budget: f64,
    pub presence: bool,
    pub messages: bool,
    pub cwd: bool,
    /// Tests only, never read from a request: cut the scan after this many log lines, so a resumed
    /// read is tested at fixed points instead of wall-clock ones.
    pub line_budget: Option<u64>,
}

impl Request {
    /// Reconstruct one parsed prior fingerprint as JSON (used by tests verifying `settledPacked`).
    pub fn prior_print_json(&self, kind: &str, key: &str) -> Option<Value> {
        if let Some(val) = self.fingerprints.get(kind).and_then(|m| m.get(key)) {
            return Some(val.clone());
        }
        self.prior
            .kinds
            .get(kind)
            .and_then(|m| m.get_str(key).map(|p| m.print_to_json(p)))
    }
}

/// Retain the full checkpoint for settled files even when active fingerprints must be trimmed.
/// A file that changes on revalidation still has its hashes and its append cursor.
pub fn pack_settled(prints: &Map<String, Value>) -> Option<String> {
    let bytes = serde_json::to_vec(prints).ok()?;
    if bytes.len() > MAX_SETTLED_BYTES {
        return None;
    }
    Some(STANDARD_NO_PAD.encode(zstd::bulk::compress(&bytes, 1).ok()?))
}

/// Stream-compress raw settled-fingerprints JSON bytes directly into a `settledPacked` string
/// without buffering a `serde_json::Map<String, Value>` tree.
pub fn pack_settled_from_reader(mut reader: impl std::io::Read) -> Option<String> {
    let mut compressed = Vec::new();
    let mut encoder = zstd::stream::write::Encoder::new(&mut compressed, 1).ok()?;
    let copied = std::io::copy(&mut reader, &mut encoder).ok()? as usize;
    if copied > MAX_SETTLED_BYTES {
        return None;
    }
    encoder.finish().ok()?;
    Some(STANDARD_NO_PAD.encode(&compressed))
}

fn valid_extra_root(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    if text.is_empty() || text.chars().count() > 1024 || text.contains('\0') {
        return false;
    }
    if text.replace('\\', "/").split('/').any(|part| part == "..") {
        return false;
    }
    crate::fsx::absolute_text(text)
}

#[derive(Default)]
struct RawRequestFields<'de> {
    kinds: Option<Value>,
    min_date_ms: Option<Value>,
    immutable_sqlite: Option<Value>,
    zcode_records: Option<Value>,
    presence: Option<Value>,
    messages: Option<Value>,
    cwd: Option<Value>,
    deadline_ms: Option<Value>,
    fingerprints_present: bool,
    fingerprints_valid: bool,
    fingerprints: RawFingerprints<'de>,
    extra_roots: Option<Value>,
    experiment_roots: Option<Value>,
    fixture: Option<Value>,
    unknown_field: bool,
}

struct RawRequestSeed {
    allow_fixture: bool,
}

impl<'de> de::DeserializeSeed<'de> for RawRequestSeed {
    type Value = Result<RawRequestFields<'de>, &'static str>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct V {
            allow_fixture: bool,
        }
        impl<'de> Visitor<'de> for V {
            type Value = Result<RawRequestFields<'de>, &'static str>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a scan request JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut fields = RawRequestFields {
                    fingerprints_valid: true,
                    ..RawRequestFields::default()
                };
                while let Some(key) = map.next_key::<std::borrow::Cow<'de, str>>()? {
                    match key.as_ref() {
                        "kinds" => fields.kinds = Some(map.next_value()?),
                        "minDateMs" => fields.min_date_ms = Some(map.next_value()?),
                        "immutableSqlite" => fields.immutable_sqlite = Some(map.next_value()?),
                        "zcodeRecords" => fields.zcode_records = Some(map.next_value()?),
                        "presence" => fields.presence = Some(map.next_value()?),
                        "messages" => fields.messages = Some(map.next_value()?),
                        "cwd" => fields.cwd = Some(map.next_value()?),
                        "deadlineMs" => fields.deadline_ms = Some(map.next_value()?),
                        "extraRoots" => fields.extra_roots = Some(map.next_value()?),
                        "experimentRoots" => fields.experiment_roots = Some(map.next_value()?),
                        "fingerprints" => {
                            fields.fingerprints_present = true;
                            let parsed: FingerprintsOrInvalid<'de> = map.next_value()?;
                            match parsed {
                                FingerprintsOrInvalid::Valid(fp) => {
                                    fields.fingerprints = *fp;
                                    fields.fingerprints_valid = true;
                                }
                                FingerprintsOrInvalid::Invalid => {
                                    fields.fingerprints = RawFingerprints::default();
                                    fields.fingerprints_valid = false;
                                }
                            }
                        }
                        "_scanFixture" if self.allow_fixture => {
                            fields.fixture = Some(map.next_value()?);
                        }
                        _ => {
                            let _: de::IgnoredAny = map.next_value()?;
                            fields.unknown_field = true;
                        }
                    }
                }
                Ok(Ok(fields))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(Err("request must be an object"))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(Err("request must be an object"))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(Err("request must be an object"))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(Err("request must be an object"))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(Err("request must be an object"))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(Err("request must be an object"))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(Err("request must be an object"))
            }
        }
        deserializer.deserialize_any(V {
            allow_fixture: self.allow_fixture,
        })
    }
}

enum FingerprintsOrInvalid<'de> {
    Valid(Box<RawFingerprints<'de>>),
    Invalid,
}

impl<'de> Deserialize<'de> for FingerprintsOrInvalid<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = FingerprintsOrInvalid<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a fingerprints object")
            }
            fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                let fp = RawFingerprints::deserialize(de::value::MapAccessDeserializer::new(map))?;
                Ok(FingerprintsOrInvalid::Valid(Box::new(fp)))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(FingerprintsOrInvalid::Invalid)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(FingerprintsOrInvalid::Invalid)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(FingerprintsOrInvalid::Invalid)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(FingerprintsOrInvalid::Invalid)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(FingerprintsOrInvalid::Invalid)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(FingerprintsOrInvalid::Invalid)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<de::IgnoredAny>()?.is_some() {}
                Ok(FingerprintsOrInvalid::Invalid)
            }
        }
        deserializer.deserialize_any(V)
    }
}

fn parse_request_inner(raw: &[u8], allow_fixture: bool) -> Result<(Request, Option<Value>), String> {
    if raw.is_empty() || raw.len() > MAX_REQUEST_BYTES {
        return Err("request is missing or too large".into());
    }
    let mut de = serde_json::Deserializer::from_slice(raw);
    let fields = de::DeserializeSeed::deserialize(RawRequestSeed { allow_fixture }, &mut de)
        .map_err(|_| "request is not valid JSON".to_string())?;
    de.end().map_err(|_| "request is not valid JSON".to_string())?;
    let mut fields = fields.map_err(str::to_string)?;
    if fields.unknown_field {
        return Err("request has unknown fields".into());
    }
    let kinds = fields
        .kinds
        .as_ref()
        .and_then(Value::as_array)
        .ok_or("request.kinds must list kinds")?;
    if kinds.is_empty() || kinds.iter().any(|k| !k.as_str().is_some_and(|k| KINDS.contains(&k))) {
        return Err("request.kinds must list claude, codex, omp, muse, zcode and/or antigravity".into());
    }
    let mut ordered: Vec<String> = Vec::new();
    for kind in kinds.iter().filter_map(Value::as_str) {
        if !ordered.iter().any(|k| k == kind) {
            ordered.push(kind.to_string());
        }
    }
    let min_date = match fields.min_date_ms.as_ref() {
        Some(Value::Number(n)) if n.as_f64().is_some_and(|v| v >= 0.0) => n.as_f64().unwrap_or(0.0) as i64,
        _ => return Err("request.minDateMs must be a non-negative number".into()),
    };
    let flag = |val: Option<&Value>, name: &str, default: bool| -> Result<bool, String> {
        match val {
            None => Ok(default),
            Some(Value::Bool(value)) => Ok(*value),
            _ => Err(format!("request.{name} must be a boolean")),
        }
    };
    let immutable = flag(fields.immutable_sqlite.as_ref(), "immutableSqlite", true)?;
    if flag(fields.zcode_records.as_ref(), "zcodeRecords", false)? {
        // zcodeRecords is the dashboard computer's experiment mode, never sent to this scanner.
        return Err("request.zcodeRecords is not supported by this reader".into());
    }
    if fields.fingerprints_present && !fields.fingerprints_valid {
        return Err("request.fingerprints must be an object".into());
    }
    let mut prior = fields.fingerprints.state;
    let non_object_kinds = fields.fingerprints.non_object_kinds;
    let mut packed_raw = fields.fingerprints.settled_packed_raw;
    for kind in &ordered {
        if let Some(encoded) = packed_raw.remove(kind) {
            if non_object_kinds.contains(kind) {
                return Err("invalid fingerprints for packed kind".into());
            }
            let entry = prior.kinds.entry(kind.clone()).or_default();
            unpack_settled_into(&encoded, entry).ok_or("invalid packed settled fingerprints")?;
            drop(encoded);
            prior.settled_packed_kinds.insert(kind.clone());
        }
    }
    drop(packed_raw);
    let extra_roots = match fields.extra_roots.take() {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        _ => return Err("request.extraRoots must map kinds to absolute paths".into()),
    };
    for (kind, paths) in &extra_roots {
        let max = if kind == "zcode" {
            EXTRA_ZCODE_DBS_MAX
        } else {
            EXTRA_ROOTS_MAX
        };
        let ok = ["claude", "codex", "omp", "muse", "zcode"].contains(&kind.as_str())
            && paths
                .as_array()
                .is_some_and(|list| list.len() <= max && list.iter().all(valid_extra_root));
        if !ok {
            return Err("request.extraRoots must map kinds to absolute paths".into());
        }
    }
    let experiment_roots = match fields.experiment_roots.take() {
        None => Map::new(),
        Some(Value::Object(map)) => map,
        _ => return Err("request.experimentRoots must map kinds to absolute paths".into()),
    };
    for (kind, paths) in &experiment_roots {
        let ok = ["claude", "codex", "omp", "muse"].contains(&kind.as_str())
            && paths
                .as_array()
                .is_some_and(|list| list.len() <= EXPERIMENT_ROOTS_MAX && list.iter().all(valid_extra_root));
        if !ok {
            return Err("request.experimentRoots must map kinds to absolute paths".into());
        }
    }
    let budget = match fields.deadline_ms.as_ref() {
        None => 20.0,
        Some(Value::Number(n)) => match n.as_f64() {
            Some(v) if (1000.0..=120_000.0).contains(&v) => v / 1000.0,
            _ => return Err("request.deadlineMs must be within 1s and 120s".into()),
        },
        _ => return Err("request.deadlineMs must be within 1s and 120s".into()),
    };
    let request = Request {
        kinds: ordered,
        min_date_ms: min_date,
        immutable,
        fingerprints: Map::new(),
        prior,
        extra_roots,
        experiment_roots,
        budget,
        presence: flag(fields.presence.as_ref(), "presence", false)?,
        messages: flag(fields.messages.as_ref(), "messages", false)?,
        cwd: flag(fields.cwd.as_ref(), "cwd", true)?,
        line_budget: None,
    };
    Ok((request, fields.fixture))
}

/// The request, validated exactly as the Python helper validates it (unknown fields refused).
/// Parses `fingerprints` and `settledPacked` directly into compact typed [`PriorPrint`] structs.
pub fn parse_request(raw: &[u8]) -> Result<Request, String> {
    parse_request_inner(raw, false).map(|(request, _)| request)
}

/// Parse a scan request while allowing an optional `_scanFixture` field (test/parity builds only).
pub fn parse_request_with_fixture(raw: &[u8]) -> Result<(Request, Option<Value>), String> {
    parse_request_inner(raw, true)
}

fn extras(request: &Request, kind: &str) -> Vec<PathBuf> {
    paths_of(&request.extra_roots, kind)
}

/// One kind's experiment folders: those to read, the search bound it hit, those it could not look in.
type ExperimentDirs = (Vec<PathBuf>, Option<(usize, Option<usize>)>, Vec<PathBuf>);

fn experiment_dirs(
    found: &mut Option<discover::ExperimentRoots>,
    request: &Request,
    home: &Path,
    kind: &str,
    index: Option<&Path>,
) -> ExperimentDirs {
    let discovered = found.get_or_insert_with(|| match index {
        Some(cache) => {
            discover::experiment_roots_indexed(home, cache, Duration::from_secs(2), discover::EXPERIMENT_MAX_ENTRIES)
        }
        None => discover::experiment_roots(home, Duration::from_secs(2)),
    });
    let mut list = paths_of(&request.experiment_roots, kind);
    for path in discovered.found.get(kind).cloned().unwrap_or_default() {
        if !list.contains(&path) {
            list.push(path);
        }
    }
    (
        list,
        discovered.limits.get(kind).copied(),
        discovered.failed.get(kind).cloned().unwrap_or_default(),
    )
}

fn paths_of(map: &Map<String, Value>, kind: &str) -> Vec<PathBuf> {
    map.get(kind)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
        .unwrap_or_default()
}

struct RowWire<'a>(&'a Row);

impl Serialize for RowWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let row = self.0;
        let len = if row.provider.is_some() { 11 } else { 10 };
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("k", &row.kind)?;
        map.serialize_entry("f", &row.filekey)?;
        map.serialize_entry("m", &row.model)?;
        if let Some(provider) = &row.provider {
            map.serialize_entry("p", provider)?;
        }
        map.serialize_entry("h", &row.hour)?;
        map.serialize_entry("i", &row.tokens[0])?;
        map.serialize_entry("o", &row.tokens[1])?;
        map.serialize_entry("cr", &row.tokens[2])?;
        map.serialize_entry("cw", &row.tokens[3])?;
        map.serialize_entry("c", &row.cost)?;
        map.serialize_entry("n", &row.events)?;
        map.end()
    }
}

struct SessionRowWire<'a>(&'a SessionRow);

impl Serialize for SessionRowWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let row = self.0;
        let len = if row.provider.is_some() { 13 } else { 12 };
        let mut map = serializer.serialize_map(Some(len))?;
        map.serialize_entry("k", &row.kind)?;
        map.serialize_entry("f", &row.filekey)?;
        map.serialize_entry("s", &row.session)?;
        map.serialize_entry("m", &row.model)?;
        if let Some(provider) = &row.provider {
            map.serialize_entry("p", provider)?;
        }
        map.serialize_entry("a", &row.first)?;
        map.serialize_entry("z", &row.last)?;
        map.serialize_entry("i", &row.tokens[0])?;
        map.serialize_entry("o", &row.tokens[1])?;
        map.serialize_entry("cr", &row.tokens[2])?;
        map.serialize_entry("cw", &row.tokens[3])?;
        map.serialize_entry("c", &row.cost)?;
        map.serialize_entry("n", &row.events)?;
        map.end()
    }
}

struct MessageWire<'a>(&'a Message);

impl Serialize for MessageWire<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let m = self.0;
        let mut map = serializer.serialize_map(Some(12))?;
        map.serialize_entry("k", &m.kind)?;
        map.serialize_entry("f", &m.filekey)?;
        map.serialize_entry("key", &m.key)?;
        map.serialize_entry("at", &m.at)?;
        map.serialize_entry("s", &m.session)?;
        map.serialize_entry("m", &m.model)?;
        map.serialize_entry("p", &m.provider)?;
        map.serialize_entry("i", &m.tokens[0])?;
        map.serialize_entry("o", &m.tokens[1])?;
        map.serialize_entry("cr", &m.tokens[2])?;
        map.serialize_entry("cw", &m.tokens[3])?;
        map.serialize_entry("c", &m.cost)?;
        map.end()
    }
}

enum SettledWire {
    Packed(Vec<(CompactHash, u64)>),
    Full(Vec<(CompactHash, Value)>),
}

struct KindFingerprintsWire {
    settled: SettledWire,
    fresh: BTreeMap<String, Print>,
}

impl Serialize for KindFingerprintsWire {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match &self.settled {
            SettledWire::Packed(entries) => {
                for (key, slot) in entries {
                    if !self.fresh.is_empty() && self.fresh.contains_key(&key.to_hex_string()) {
                        continue;
                    }
                    map.serialize_entry(key, &SettledConfirmed(*slot))?;
                }
            }
            SettledWire::Full(entries) => {
                for (key, val) in entries {
                    if !self.fresh.is_empty() && self.fresh.contains_key(&key.to_hex_string()) {
                        continue;
                    }
                    map.serialize_entry(key, val)?;
                }
            }
        }
        for (key, print) in &self.fresh {
            map.serialize_entry(key, print)?;
        }
        map.end()
    }
}

struct KindAnswer {
    state: &'static str,
    fingerprints: KindFingerprintsWire,
    settled_packed: bool,
    partial: bool,
    read_failures: Option<usize>,
    replaced_files: Option<Vec<String>>,
    compressed_unread: Option<usize>,
    unfinished: Option<(Vec<Value>, usize)>,
    wal_unread: bool,
    unread_usage: bool,
    estimated_clock: bool,
    unreadable_records: Option<usize>,
    unreadable_folders: Option<usize>,
    optional_search_limited: bool,
    unconfirmed_files: Option<usize>,
    search_limit: Option<(usize, Option<usize>)>,
    unreadable: bool,
}

impl Serialize for KindAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("state", self.state)?;
        map.serialize_entry("fingerprints", &self.fingerprints)?;
        if self.settled_packed {
            map.serialize_entry("settledPacked", &true)?;
        }
        if self.partial {
            map.serialize_entry("partial", &true)?;
        }
        if let Some(count) = self.read_failures {
            map.serialize_entry("readFailures", &count)?;
        }
        if let Some(files) = &self.replaced_files {
            map.serialize_entry("replacedFiles", files)?;
        }
        if let Some(count) = self.compressed_unread {
            map.serialize_entry("compressedUnread", &count)?;
        }
        if let Some((unfinished, count)) = &self.unfinished {
            map.serialize_entry("unfinished", unfinished)?;
            map.serialize_entry("unfinishedCount", count)?;
        }
        if self.wal_unread {
            map.serialize_entry("walUnread", &true)?;
        }
        if self.unread_usage {
            map.serialize_entry("unreadUsage", &true)?;
        }
        if self.estimated_clock {
            map.serialize_entry("estimatedClock", &true)?;
        }
        if let Some(count) = self.unreadable_records {
            map.serialize_entry("unreadableRecords", &count)?;
        }
        if let Some(count) = self.unreadable_folders {
            map.serialize_entry("unreadableFolders", &count)?;
        }
        if self.optional_search_limited {
            map.serialize_entry("optionalSearchLimited", &true)?;
        }
        if let Some(count) = self.unconfirmed_files {
            map.serialize_entry("unconfirmedFiles", &count)?;
        }
        if let Some((read, seen)) = self.search_limit {
            map.serialize_entry("searchLimit", &json!({"read": read, "found": seen}))?;
        }
        if self.unreadable {
            map.serialize_entry("unreadable", &true)?;
        }
        map.end()
    }
}

struct RowsSlice<'a>(&'a [Row]);

impl Serialize for RowsSlice<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            seq.serialize_element(&RowWire(row))?;
        }
        seq.end()
    }
}

struct SessionRowsSlice<'a>(&'a [SessionRow]);

impl Serialize for SessionRowsSlice<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for row in self.0 {
            seq.serialize_element(&SessionRowWire(row))?;
        }
        seq.end()
    }
}

struct MessagesSlice<'a>(&'a [Message]);

impl Serialize for MessagesSlice<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeSeq;
        let mut seq = serializer.serialize_seq(Some(self.0.len()))?;
        for m in self.0 {
            seq.serialize_element(&MessageWire(m))?;
        }
        seq.end()
    }
}

struct ScanAnswer {
    truncated: bool,
    discovery_truncated: bool,
    kinds: Vec<(String, KindAnswer)>,
    rows: Vec<Row>,
    srows: Vec<SessionRow>,
    discovery: Value,
    sessions: Map<String, Value>,
    unanswered_mounts: usize,
    messages_wanted: bool,
    messages: Vec<Message>,
    messages_cut: bool,
}

impl Serialize for ScanAnswer {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("version", &VERSION)?;
        map.serialize_entry("truncated", &self.truncated)?;
        map.serialize_entry("discoveryTruncated", &self.discovery_truncated)?;
        map.serialize_entry("kinds", &OrderedKindsWireRef(&self.kinds))?;
        map.serialize_entry("rows", &RowsSlice(&self.rows))?;
        map.serialize_entry("srows", &SessionRowsSlice(&self.srows))?;
        map.serialize_entry("discovery", &self.discovery)?;
        map.serialize_entry("reader", &version_json())?;
        map.serialize_entry("sessions", &self.sessions)?;
        if self.unanswered_mounts > 0 {
            map.serialize_entry("unansweredMounts", &self.unanswered_mounts)?;
        }
        if self.messages_wanted {
            map.serialize_entry("messages", &MessagesSlice(&self.messages))?;
            if self.messages_cut {
                map.serialize_entry("messagesCut", &true)?;
            }
        }
        map.end()
    }
}

struct OrderedKindsWireRef<'a>(&'a [(String, KindAnswer)]);

impl Serialize for OrderedKindsWireRef<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (kind, entry) in self.0 {
            map.serialize_entry(kind, entry)?;
        }
        map.end()
    }
}

/// This binary's identity, for the version handshake.
pub fn version_json() -> Value {
    json!({
        "name": NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL,
        "target": target(),
    })
}

pub fn target() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

/// The answer goes out once: from the scan, or from the watchdog when the scan is stuck.
static ANSWERED: std::sync::Mutex<bool> = std::sync::Mutex::new(false);

/// Write the answer unless the watchdog already did; exit 1 when stdout fails.
pub fn answer(response: &Value) {
    let mut answered = ANSWERED.lock().unwrap_or_else(|e| e.into_inner());
    if *answered {
        return;
    }
    let mut out = std::io::stdout().lock();
    if serde_json::to_writer(&mut out, response).is_err() || std::io::Write::flush(&mut out).is_err() {
        std::process::exit(1);
    }
    *answered = true;
}

/// Write pre-serialized JSON bytes unless the watchdog already did; exit 1 when stdout fails.
pub fn answer_bytes(response: &[u8]) {
    let mut answered = ANSWERED.lock().unwrap_or_else(|e| e.into_inner());
    if *answered {
        return;
    }
    let mut out = std::io::stdout().lock();
    if std::io::Write::write_all(&mut out, response).is_err() || std::io::Write::flush(&mut out).is_err() {
        std::process::exit(1);
    }
    *answered = true;
}

/// The answer for a scan that did not finish: every kind "pending" (nothing confirmed, nothing
/// dropped), `stalled` set.
pub fn stalled_answer(request: &Request) -> Value {
    let kinds: Map<String, Value> = request
        .kinds
        .iter()
        .map(|kind| (kind.clone(), json!({"state": "pending", "fingerprints": {}})))
        .collect();
    json!({
        "version": VERSION,
        "truncated": true,
        "discoveryTruncated": false,
        "kinds": kinds,
        "rows": [],
        "srows": [],
        "discovery": {"kinds": {}},
        "reader": version_json(),
        "sessions": {},
        "stalled": true,
    })
}

/// Start the watchdog: at the deadline plus a grace (a fifth of the budget, 1 to 4 s) it answers
/// for a scan that has not, and ends the process.
pub fn watchdog(request: &Request) {
    let grace = (request.budget * 0.2).clamp(1.0, 4.0);
    let wait = Duration::from_secs_f64(request.budget + grace);
    let fallback = stalled_answer(request);
    std::thread::spawn(move || {
        std::thread::sleep(wait);
        let mut answered = ANSWERED.lock().unwrap_or_else(|e| e.into_inner());
        if !*answered {
            let mut out = std::io::stdout().lock();
            let _ = serde_json::to_writer(&mut out, &fallback);
            let _ = std::io::Write::flush(&mut out);
            *answered = true;
            std::process::exit(0);
        }
    });
}

fn finalize_kind_clocks(collector: &mut Collector, kind: &str) {
    let mut data_clocks: HashMap<String, i64> = HashMap::new();
    for ((k, filekey), at) in &collector.verified_clocks {
        if k == kind {
            data_clocks
                .entry(filekey.clone())
                .and_modify(|seen| *seen = (*seen).max(*at))
                .or_insert(*at);
        }
    }
    let Some(mut fresh) = collector.fresh.remove(kind) else {
        return;
    };
    for (key, print) in &mut fresh {
        let newest = data_clocks.get(key).copied();
        let prior = collector.prior_print(kind, key);
        let unchanged = if print.wal_size.is_some() {
            read::sqlite::same(prior, print)
                && prior.is_some_and(PriorPrint::wal_unread) == print.wal_unread.unwrap_or(false)
        } else {
            util::same_prior_print(prior, print)
        };
        let retained = (unchanged || print.progress.is_some())
            .then(|| prior.and_then(PriorPrint::newest_usage_ms))
            .flatten();
        print.newest_usage_ms = newest.into_iter().chain(retained).max();
        print.usage_clock = true;
    }
    collector.fresh.insert(kind.to_string(), fresh);
}

#[allow(clippy::too_many_arguments)]
fn finalize_kind(
    collector: &mut Collector,
    kind: &str,
    state: &'static str,
    ran: bool,
    partial_from_run: bool,
    packed: bool,
    optional_limited: bool,
    home: &Path,
    env: &Env,
    instances: &[discover::T3Instance],
) -> (KindAnswer, Option<Value>) {
    if ran {
        if collector.truncated || collector.row_cap {
            collector.keep_confirmed_kind(kind);
        }
        finalize_kind_clocks(collector, kind);
    }

    let mut partial = partial_from_run;
    let read_failures = collector.read_failures.get(kind).filter(|f| !f.is_empty()).map(|f| {
        partial = true;
        f.len()
    });
    let replaced_files = (kind == "claude" && !collector.replaced_files.is_empty())
        .then(|| collector.replaced_files.iter().cloned().collect::<Vec<_>>());
    let compressed_unread = (kind == "codex" && !collector.codex_compressed_unread.is_empty()).then(|| {
        partial = true;
        collector.codex_compressed_unread.len()
    });
    let unfinished = if crate::collector::resumable(kind) {
        let (list, count) = unfinished_logs(collector, kind);
        if count > 0 {
            partial = true;
            Some((list, count))
        } else {
            None
        }
    } else {
        None
    };
    let wal_unread = (kind == "zcode" && collector.wal_unread) || collector.kind_wal_unread.contains(kind);
    let unread_usage = collector
        .fresh
        .get(kind)
        .is_some_and(|prints| prints.values().any(|p| p.unread_usage == Some(true)))
        || discover::settled_flag(collector, kind, "unreadUsage");
    if unread_usage {
        partial = true;
    }
    let estimated_clock = collector.estimated_clocks.contains(kind)
        || collector
            .fresh
            .get(kind)
            .is_some_and(|prints| prints.values().any(|p| p.estimated_clock == Some(true)))
        || discover::settled_flag(collector, kind, "estimatedClock");
    let unreadable_records = collector.unreadable_records.get(kind).copied().filter(|n| *n > 0);
    if unreadable_records.is_some() {
        partial = true;
    }
    let unreadable_folders = collector
        .unreadable_folders
        .get(kind)
        .filter(|f| !f.is_empty())
        .map(|f| {
            partial = true;
            f.len()
        });
    let optional_search_limited = ["claude", "codex", "omp", "muse"].contains(&kind) && ran && optional_limited;
    let unconfirmed_files = if optional_search_limited {
        let settled = collector.settled.get(kind);
        let fresh = collector.fresh.get(kind);
        let count = collector.prior_kind(kind).map_or(0, |prior| {
            prior
                .iter()
                .filter(|(_, print)| print.optional_root())
                .filter(|(key, _)| {
                    if settled.is_some_and(|s| s.contains_key(key)) {
                        return false;
                    }
                    let text = key.to_hex_string();
                    !fresh.is_some_and(|f| f.contains_key(&text))
                        && !(kind == "claude" && collector.replaced_files.contains(&text))
                })
                .count()
        });
        if count > 0 {
            partial = true;
            Some(count)
        } else {
            None
        }
    } else {
        None
    };
    let search_limit = collector.search_limits.get(kind).copied();
    if search_limit.is_some() {
        partial = true;
    }
    let unreadable = kind == "antigravity" && collector.agy_unreadable > 0;

    // Build discovery before taking `settled` / `fresh` / dropping `prior` for this kind.
    let discovery = if ran {
        let mut discovered = discover::discovery_kind(collector, home, env, kind, instances);
        if partial {
            let fresh = collector.fresh.get(kind);
            let retained = collector
                .prior_kind(kind)
                .into_iter()
                .flat_map(|p| p.iter())
                .filter(|(key, _)| !fresh.is_some_and(|f| f.contains_key(&key.to_hex_string())))
                .filter_map(|(_, p)| p.newest_usage_ms())
                .max();
            let data = discovered["freshness"]["newestDataMs"]
                .as_i64()
                .into_iter()
                .chain(retained)
                .max();
            let activity = discovered["freshness"]["newestActivityMs"].as_i64();
            if retained.is_some() {
                let complete = discovered["freshness"]["activityComplete"] == true;
                discovered["freshness"] = discover::freshness(data, data.or(activity), collector.min_date_ms, complete);
            }
        }
        Some(discovered)
    } else {
        None
    };

    let settled_entries = collector.settled.remove(kind).unwrap_or_default();
    let settled = if packed {
        SettledWire::Packed(settled_entries.into_iter().collect())
    } else {
        let mut full = Vec::with_capacity(settled_entries.len());
        if let Some(prior_map) = collector.prior_kind(kind) {
            for (key, slot) in settled_entries {
                if let Some(prior) = prior_map.get_hash(&key) {
                    full.push((key, prior_map.print_to_settled_json(prior, slot)));
                }
            }
        }
        SettledWire::Full(full)
    };
    let fresh = collector.fresh.remove(kind).unwrap_or_default();

    let answer = KindAnswer {
        state,
        fingerprints: KindFingerprintsWire { settled, fresh },
        settled_packed: packed,
        partial,
        read_failures,
        replaced_files,
        compressed_unread,
        unfinished,
        wal_unread,
        unread_usage,
        estimated_clock,
        unreadable_records,
        unreadable_folders,
        optional_search_limited,
        unconfirmed_files,
        search_limit,
        unreadable,
    };

    // Release all per-kind collector buffers immediately now that this kind is finalized (R4).
    collector.drop_kind(kind);

    (answer, discovery)
}

fn scan_inner(mut request: Request, home: &Path, env: &Env, index: Option<&Path>) -> ScanAnswer {
    if !request.fingerprints.is_empty() {
        request.prior.overlay_map(&request.fingerprints);
        request.fingerprints.clear();
    }
    let packed_kinds: HashSet<String> = request.prior.settled_packed_kinds.clone();
    let prior = std::mem::take(&mut request.prior);
    let mut collector = Collector::with_prior(request.min_date_ms, request.budget, prior);
    collector.capture_cwd = request.cwd;
    collector.messages_wanted = request.messages;
    collector.lines_left = request.line_budget;
    // Network and FUSE mounts answer a probe first, or are never touched (FIX-ROUND-BIG 9). One under
    // home may hold logs a walk from home would have found: those kinds are partial.
    collector.unanswered_mounts = crate::fsx::guard_mounts(Duration::from_secs(2));
    let mounts_under_home = crate::fsx::avoided_under(home) > 0;
    let instances = discover::t3_instances(home);
    // Experiment log folders outside PM-Experiments, found once per scan (DESIGN 16.5).
    let mut found: Option<discover::ExperimentRoots> = None;
    let mut kind_answers: Vec<(String, KindAnswer)> = Vec::with_capacity(request.kinds.len());
    let mut discovery_kinds = Map::new();
    let mut ran_kinds: HashSet<String> = HashSet::with_capacity(request.kinds.len());

    for kind in &request.kinds {
        collector.fresh.entry(kind.clone()).or_default();
        let cut_before = collector.truncated;
        let experiment_list = if ["claude", "codex", "omp", "muse"].contains(&kind.as_str()) {
            let (list, limit, failed) = experiment_dirs(&mut found, &request, home, kind, index);
            let requested = paths_of(&request.experiment_roots, kind);
            collector.optional_roots.insert(
                kind.clone(),
                list.iter()
                    .filter(|path| !requested.contains(path))
                    .map(|path| (path.clone(), crate::util::realpath(path)))
                    .collect(),
            );
            if let Some((read, seen)) = limit {
                collector.search_limit(kind, read, seen);
            }
            for folder in &failed {
                collector.note_root(kind, folder, "experiment");
                let cause = if crate::fsx::avoided(folder) {
                    "did_not_answer"
                } else {
                    crate::fsx::list_dir(folder)
                        .err()
                        .as_ref()
                        .map(crate::fsx::error_cause)
                        .unwrap_or("io_error")
                };
                collector.root_failure(kind, folder, "unreadable", cause);
            }
            if mounts_under_home {
                collector.usage_partial.insert(kind.clone());
            }
            list
        } else {
            Vec::new()
        };
        let state = match kind.as_str() {
            "claude" => read::claude::collect(
                &mut collector,
                home,
                env,
                &extras(&request, "claude"),
                &experiment_list,
                &instances,
            ),
            "codex" => read::codex::collect(
                &mut collector,
                home,
                env,
                &extras(&request, "codex"),
                &experiment_list,
                &instances,
            ),
            "omp" => read::omp::collect(
                &mut collector,
                home,
                env,
                &extras(&request, "omp"),
                &experiment_list,
                &instances,
            ),
            "muse" => read::muse::collect(&mut collector, home, env, &extras(&request, "muse"), &experiment_list),
            "zcode" => read::zcode::collect(&mut collector, home, env, request.immutable, &extras(&request, "zcode")),
            "cursor" => read::cursor::collect(&mut collector, home),
            "devin" => read::devin::collect(&mut collector, home, env),
            _ => read::antigravity::collect(&mut collector, home, env),
        };
        ran_kinds.insert(kind.clone());
        let expired = collector.expired();
        let partial_from_run = collector.usage_partial.contains(kind)
            || (collector.truncated && !cut_before)
            || collector.row_cap
            || expired;
        if expired {
            collector.truncated = true;
        }
        let optional_limited = found.as_ref().is_some_and(|f| f.optional_limited);
        let (kind_answer, discovered) = finalize_kind(
            &mut collector,
            kind,
            state,
            true,
            partial_from_run,
            packed_kinds.contains(kind),
            optional_limited,
            home,
            env,
            &instances,
        );
        kind_answers.push((kind.clone(), kind_answer));
        if let Some(discovered) = discovered {
            discovery_kinds.insert(kind.clone(), discovered);
        }
        if expired {
            break;
        }
    }

    // Any remaining kinds skipped after an earlier kind hit the deadline stay "pending".
    for kind in &request.kinds {
        if ran_kinds.contains(kind) {
            continue;
        }
        let (kind_answer, _) = finalize_kind(
            &mut collector,
            kind,
            "pending",
            false,
            false,
            packed_kinds.contains(kind),
            false,
            home,
            env,
            &instances,
        );
        kind_answers.push((kind.clone(), kind_answer));
    }

    let mut discovery = json!({"kinds": discovery_kinds});
    if request.presence && !collector.expired() {
        let mut tools = discover::presence(home, env, &instances, Duration::from_secs(3));
        if request.kinds.iter().any(|k| k == "cursor") && collector.run_stores > 0 {
            tools.remove("cursor");
        }
        discovery["tools"] = Value::Object(tools);
    }

    let mut rows = std::mem::take(&mut collector.row_list);
    rows.sort_by(|a, b| {
        (&a.hour, &a.kind, a.provider.as_deref().unwrap_or(""), &a.model).cmp(&(
            &b.hour,
            &b.kind,
            b.provider.as_deref().unwrap_or(""),
            &b.model,
        ))
    });
    let mut srows = std::mem::take(&mut collector.srow_list);
    srows.sort_by(|a, b| (a.last, &a.kind, &a.session, &a.model).cmp(&(b.last, &b.kind, &b.session, &b.model)));

    let named: HashSet<&str> = srows.iter().map(|r| r.session.as_str()).collect();
    let sessions: Map<String, Value> = collector
        .sessions
        .iter()
        .filter(|(key, _)| named.contains(key.as_str()))
        .map(|(key, evidence)| {
            let cwd = evidence.cwd.as_ref().map(|cwd| {
                let path = discover::tilde(Path::new(cwd), home);
                json!({
                    "key": util::sha256_hex(format!("aac-cwd-v1:{path}").as_bytes())[..16].to_string(),
                    "path": path,
                    "identitySource": "inferred_cwd",
                    "confidence": "inferred",
                })
            });
            let t3 = evidence.t3.as_ref().map(|(thread, instance)| {
                json!({
                    "threadId": thread,
                    "providerInstanceId": instance,
                    "identitySource": "t3_native_ref",
                    "confidence": "explicit",
                })
            });
            let mut item = json!({"hash": evidence.hash, "cwd": cwd});
            if let Some(t3) = t3 {
                item["t3"] = t3;
            }
            (key.clone(), item)
        })
        .collect();

    let messages = if request.messages {
        std::mem::take(&mut collector.messages)
    } else {
        Vec::new()
    };

    ScanAnswer {
        truncated: collector.truncated,
        discovery_truncated: collector.discovery_truncated,
        kinds: kind_answers,
        rows,
        srows,
        discovery,
        sessions,
        unanswered_mounts: collector.unanswered_mounts,
        messages_wanted: request.messages,
        messages,
        messages_cut: collector.messages_cut,
    }
}

/// One scan: every requested kind in order, then the response as a JSON `Value`.
pub fn scan(request: &Request, home: &Path, env: &Env) -> Value {
    let answer = scan_inner(request.clone(), home, env, None);
    serde_json::to_value(&answer).unwrap_or(Value::Null)
}

/// Owned scan that consumes `request` without cloning prior fingerprints and serializes the
/// response directly to JSON bytes without allocating an intermediate `serde_json::Value` tree.
pub fn scan_owned_bytes(request: Request, home: &Path, env: &Env) -> Vec<u8> {
    let answer = scan_inner(request, home, env, None);
    serde_json::to_vec(&answer).unwrap_or_else(|_| b"{}".to_vec())
}

/// The durable discovery index installed scanners share between short-lived pulls. Reader cursors
/// and their measured usage remain owned by the existing fingerprint protocol.
fn persistent_index(home: &Path, env: &Env) -> PathBuf {
    env.get("CCS_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".ccs"))
        .join("analytics-scan-index-v1")
}

/// Installed scanners share a durable discovery index between short-lived pulls.
pub fn scan_persistent(request: &Request, home: &Path, env: &Env) -> Value {
    let index = persistent_index(home, env);
    let answer = scan_inner(request.clone(), home, env, Some(&index));
    serde_json::to_value(&answer).unwrap_or(Value::Null)
}

/// `scan_owned_bytes` with the durable discovery index: what the installed agent and scanner run.
pub fn scan_persistent_owned_bytes(request: Request, home: &Path, env: &Env) -> Vec<u8> {
    let index = persistent_index(home, env);
    let answer = scan_inner(request, home, env, Some(&index));
    serde_json::to_vec(&answer).unwrap_or_else(|_| b"{}".to_vec())
}

/// The logs of a resumable kind not read to their end, named so the page can say which (DESIGN
/// 16.1): those this call left part-read, those an earlier call did that this one did not reach
/// (only while the kind was cut, else a file the scan did not find is gone), and those the decoder
/// could not read. At most 20, largest first; the count is of all of them.
pub fn unfinished_logs(collector: &Collector, kind: &str) -> (Vec<Value>, usize) {
    let mut found: Vec<(u8, u64, Value)> = Vec::new();
    let fresh = collector.fresh.get(kind);
    if let Some(prints) = fresh {
        for print in prints.values() {
            if let Some(progress) = &print.progress
                && let Some(through) = progress.through
            {
                found.push((
                    0,
                    print.size,
                    json!({"name": progress.name, "sizeBytes": print.size, "readThroughBytes": through,
                           "decodedBytes": progress.decoded, "reason": "deadline"}),
                ));
            }
        }
    }
    if collector.truncated
        && let Some(prior) = collector.prior_kind(kind)
    {
        for (key, print) in prior.iter() {
            if fresh.is_some_and(|m| m.contains_key(&key.to_hex_string())) {
                continue;
            }
            let Some(through) = print.read_through() else {
                continue;
            };
            let size = print.size().unwrap_or(0);
            let name = print.read_name().map(|n| util::prefix_chars(n, 120));
            found.push((
                0,
                size,
                json!({"name": name, "sizeBytes": size, "readThroughBytes": through,
                       "decodedBytes": print.decoded_size(), "reason": "deadline"}),
            ));
        }
    }
    let mut seen = HashSet::new();
    for (path, size) in &collector.unreadable_logs {
        if seen.insert(path.clone()) {
            found.push((
                1,
                *size,
                json!({"name": util::display_name(path), "sizeBytes": size, "readThroughBytes": null,
                       "decodedBytes": null, "reason": "unreadable"}),
            ));
        }
    }
    // Logs with no valid session header: named, never counted (RECHECK-BIG-1 M1).
    let mut headerless = HashSet::new();
    for (path, size) in &collector.no_session_logs {
        if headerless.insert(path.clone()) {
            found.push((
                1,
                *size,
                json!({"name": util::display_name(path), "sizeBytes": size, "readThroughBytes": null,
                       "decodedBytes": null, "reason": "no_session_id"}),
            ));
        }
    }
    let count = found.len();
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    (found.into_iter().take(20).map(|(_, _, v)| v).collect(), count)
}
