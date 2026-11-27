//! Per-host incremental scanner state. A scan that was cut short retains unvisited files.
//!
//! A computer's live checkpoint is saved in parts (revision 2): a small header with the tools'
//! read states, and one part per tool and file bucket holding those files' prints and rows. A
//! pass rewrites only the parts that changed, where the single document (revision 1, 38-70 MB
//! on a busy computer) was rewritten whole every pass. Revision 1 stays readable, and the
//! migration leaves its document in place for a rolled-back build (see [`Cache::load`]).
use super::scan_answer::Answer;
use super::scan_print::{self, Fingerprints, Identities, Print, Prints};
use crate::agent_transport::ArgsText;
use aac_store::scan_row::{Interner, ScanRow, with_interner};
use aac_store::{analytics_cache, engine::Engine, error::Result};
use serde::{Deserialize, Serialize, Serializer, ser::SerializeMap};
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

const REQUEST_BYTES: usize = 8 * 1024 * 1024;
const CACHE_BYTES: usize = 64 * 1024 * 1024;
const CACHE_ROWS: usize = 100_000;
/// File buckets per tool in a saved checkpoint.
const BUCKETS: u32 = 64;
pub(super) const REMOTE_ARCHIVE: &str = "legacy/cache/analytics-remote-v1/";

/// A legacy remote cache, read once by the migration and as a first live scan's seed.
pub(super) struct LegacyCache {
    version: Option<u64>,
    pub scan: Cache,
    pub last_scan_at: Option<String>,
}

impl LegacyCache {
    /// A legacy cache version rebuilds a tool's rows up to the version the tool table names
    /// (`legacyRebuildThrough`); v7 and v8 wrote the current per-file cells.
    pub fn rebuilds(&self, tool: &str) -> bool {
        match self.version {
            Some(version @ 3..=6) => aac_providers::legacy_rebuild_through(tool)
                .is_some_and(|through| version <= u64::from(through)),
            Some(7..=8) => false,
            _ => true,
        }
    }

    pub fn read(bytes: &[u8]) -> Result<Self> {
        let stored = Stored::read(bytes, Interner::default())?;
        Ok(LegacyCache {
            version: stored.version,
            last_scan_at: stored.last_scan_at.clone(),
            scan: stored.into_cache(),
        })
    }
}

/// A checkpoint as one document (`_nativeRevision` 1, the historical scans' form) or as a
/// legacy cache wrote it, read straight into typed rows and prints: no whole-document tree, and
/// each repeated string held once.
#[derive(Deserialize)]
struct Stored {
    #[serde(rename = "_nativeRevision")]
    revision: Option<u8>,
    version: Option<u64>,
    #[serde(rename = "lastScanAt")]
    last_scan_at: Option<String>,
    fingerprints: Fingerprints,
    rows: Vec<ScanRow>,
    #[serde(default)]
    srows: Vec<ScanRow>,
    #[serde(default)]
    states: Map<String, Value>,
    #[serde(skip)]
    strings: Interner,
}

impl Stored {
    fn read(bytes: &[u8], strings: Interner) -> Result<Self> {
        let (stored, strings) = with_interner(strings, || serde_json::from_slice::<Stored>(bytes));
        let mut stored = stored?;
        stored.strings = strings;
        Ok(stored)
    }

    fn into_cache(self) -> Cache {
        let mut cache = Cache {
            fingerprints: self.fingerprints,
            rows: self.rows,
            srows: self.srows,
            states: self.states,
            strings: self.strings,
            ..Cache::default()
        };
        cache.order_rows();
        cache
    }
}

/// The header of a checkpoint saved in parts (`_nativeRevision` 2).
#[derive(Serialize, Deserialize)]
struct Header {
    #[serde(rename = "_nativeRevision")]
    revision: u8,
    /// The tools with prints, in their order (a tool may have none).
    #[serde(default)]
    kinds: Vec<String>,
    /// (tool, file bucket) of each part, in order.
    parts: Vec<(String, u32)>,
    states: Map<String, Value>,
    /// The digest of the revision 1 document this checkpoint was migrated from (left in place
    /// for a rolled-back build). A revision 1 document with another digest was written by such a
    /// build after the migration, and is migrated again.
    #[serde(rename = "migratedFrom")]
    migrated_from: Option<String>,
}

/// One part: the prints and rows of one tool's files in one bucket.
#[derive(Deserialize)]
struct Part {
    #[serde(default)]
    fingerprints: Prints,
    #[serde(default)]
    rows: Vec<ScanRow>,
    #[serde(default)]
    srows: Vec<ScanRow>,
}

#[derive(Serialize)]
struct PartRef<'a> {
    fingerprints: PrintRefs<'a>,
    rows: &'a [&'a ScanRow],
    srows: &'a [&'a ScanRow],
}

/// Borrowed prints by file key, written in their order (a part sorts them by key first, so an
/// unchanged part keeps its bytes).
struct PrintRefs<'a>(Vec<(&'a Arc<str>, &'a Print)>);

impl Serialize for PrintRefs<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, print) in &self.0 {
            map.serialize_entry(&***key, print)?;
        }
        map.end()
    }
}

/// What one save wrote: keys written or removed, and the bytes written.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Written {
    pub keys: usize,
    pub bytes: usize,
}

/// What the store holds of this cache, by digest.
#[derive(Default)]
struct Saved {
    header: Option<[u8; 32]>,
    parts: HashMap<String, [u8; 32]>,
    /// (tool, file bucket) of each stored part, in order.
    listed: Vec<(String, u32)>,
    /// The revision 1 document's digest, carried into the header.
    migrated_from: Option<String>,
}

fn legacy_key(host: &str) -> String {
    format!("live/scanner/{host}")
}

fn header_key(host: &str) -> String {
    format!("live/scanner2/{host}")
}

fn part_key(host: &str, kind: &str, bucket: u32) -> String {
    format!("live/scanner2/{host}/{kind}/{bucket:02x}")
}

/// A file's bucket: FNV-1a of its key.
fn bucket(file: &str) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for byte in file.bytes() {
        hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
    }
    hash % BUCKETS
}

/// The header (by digest) of each computer whose revision 1 document this process already
/// compared with it, by (store instance, host): the document is read once per process (a
/// rolled-back build only writes it while this one is stopped), not every pass. A header this
/// process did not read or write, such as a restored store's, is compared again.
type Checked = HashMap<(String, String), [u8; 32]>;
static CHECKED: Mutex<Option<Checked>> = Mutex::new(None);

fn checked_lock() -> std::sync::MutexGuard<'static, Option<Checked>> {
    CHECKED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn checked(store: &Engine, host: &str, header: &[u8; 32]) -> bool {
    checked_lock()
        .as_ref()
        .and_then(|map| map.get(&(store.instance_id.clone(), host.to_owned())))
        == Some(header)
}

fn mark_checked(store: &Engine, host: &str, header: [u8; 32]) {
    checked_lock()
        .get_or_insert_with(HashMap::new)
        .insert((store.instance_id.clone(), host.to_owned()), header);
}

/// A restarted process: every revision 1 document is compared again.
#[cfg(test)]
fn forget_checked() {
    *checked_lock() = None;
}

/// The serialized length of `value`, counted without building its text.
fn json_len<T: Serialize + ?Sized>(value: &T) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(0);
    let _ = serde_json::to_writer(&mut count, value);
    count.0
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One scan request's arguments: the frame text (held once, shared with the transport), and the
/// tools whose settled prints it packed.
pub(super) struct ScanArgs {
    pub text: Arc<ArgsText>,
    pub packed: HashSet<String>,
}

#[cfg(test)]
impl ScanArgs {
    pub fn value(&self) -> Value {
        serde_json::from_str(self.text.get()).unwrap_or(Value::Null)
    }
}

impl std::fmt::Debug for ScanArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ScanArgs")
    }
}

#[cfg(test)]
impl std::fmt::Display for ScanArgs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.text.get())
    }
}

/// The `fingerprints` argument, written from the borrowed prints.
enum PrintsArg<'a> {
    /// Every tool's prints, then the store settings.
    All(&'a Fingerprints, &'a Value),
    /// The store settings, then the selected prints of one tool (none yet while sizing).
    One(&'a Value, &'a str, Option<&'a [(&'a Arc<str>, &'a Print)]>),
}

impl Serialize for PrintsArg<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        match self {
            PrintsArg::All(fingerprints, store) => {
                for (kind, prints) in fingerprints.iter() {
                    map.serialize_entry(kind, prints)?;
                }
                map.serialize_entry("_store", store)?;
            }
            PrintsArg::One(store, kind, selected) => {
                map.serialize_entry("_store", store)?;
                if let Some(selected) = selected {
                    map.serialize_entry(*kind, &PrintRefs(selected.to_vec()))?;
                }
            }
        }
        map.end()
    }
}

/// The request arguments with `fingerprints` last, as the scanner reads them.
struct Frame<'a> {
    args: &'a Map<String, Value>,
    prints: PrintsArg<'a>,
}

impl Serialize for Frame<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(None)?;
        for (key, value) in self.args {
            map.serialize_entry(key, value)?;
        }
        map.serialize_entry("fingerprints", &self.prints)?;
        map.end()
    }
}

fn frame_text(frame: &Frame<'_>) -> Option<Arc<ArgsText>> {
    let text = RawValue::from_string(serde_json::to_string(frame).ok()?).ok()?;
    Some(Arc::new(ArgsText::new(text)))
}

/// Retain the full checkpoint of settled files (`settledPacked`): their prints as one JSON
/// object, zstd-compressed and base64-encoded as `aac_usage::protocol::pack_settled` writes it,
/// streamed from the borrowed prints.
fn pack_settled(settled: &[(&Arc<str>, &Print)]) -> Option<String> {
    use base64::Engine as _;
    let prints = PrintRefs(settled.to_vec());
    let length = json_len(&prints);
    if length > aac_usage::prior::MAX_SETTLED_BYTES {
        return None;
    }
    let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).ok()?;
    encoder.set_pledged_src_size(Some(length as u64)).ok()?;
    encoder.include_contentsize(true).ok()?;
    serde_json::to_writer(&mut encoder, &prints).ok()?;
    let compressed = encoder.finish().ok()?;
    Some(base64::engine::general_purpose::STANDARD_NO_PAD.encode(compressed))
}

/// A computer's scan requests for one pass: one request for every tool when the whole frame
/// fits, else one per tool. Each request's text is built only when [`Requests::args`] asks for
/// it, just before its call, from the prints as the pass found them (a tool's prints change only
/// when its own answer is applied).
pub(super) struct Requests {
    args: Map<String, Value>,
    settings: Value,
    settled: HashMap<String, u64>,
    pub batches: Vec<Vec<String>>,
    whole: bool,
}

impl Requests {
    pub fn new(
        args: Value,
        kinds: &[String],
        fingerprints: &Fingerprints,
        settled: &HashMap<String, u64>,
        now: i64,
    ) -> Self {
        let slot = now.max(0) as u64 / aac_usage::collector::SETTLED_CHECK_MS;
        let settings = json!({ "settledBefore": settled, "settledSlot": slot });
        let mut args = match args {
            Value::Object(args) => args,
            _ => Map::new(),
        };
        args.shift_remove("fingerprints");
        let whole = json_len(&Frame {
            args: &args,
            prints: PrintsArg::All(fingerprints, &settings),
        }) <= REQUEST_BYTES;
        let batches = if whole {
            vec![kinds.to_vec()]
        } else {
            kinds.iter().map(|kind| vec![kind.clone()]).collect()
        };
        Requests {
            args,
            settings,
            settled: settled.clone(),
            batches,
            whole,
        }
    }

    /// The arguments of the request for `batch` (one of [`Self::batches`]), or None when the
    /// tool's prints cannot be carried within the frame.
    pub fn args(&self, batch: &[String], fingerprints: &Fingerprints) -> Option<ScanArgs> {
        if self.whole {
            return frame_text(&Frame {
                args: &self.args,
                prints: PrintsArg::All(fingerprints, &self.settings),
            })
            .map(|text| ScanArgs {
                text,
                packed: HashSet::new(),
            });
        }
        let kind = batch.first()?;
        let mut one = self.args.clone();
        one.insert("kinds".into(), json!([kind]));
        let mut store = self.settings.clone();
        let mut packed = HashSet::new();
        let mut files = Vec::new();
        if let Some(prints) = fingerprints.get(kind) {
            let cutoff = self.settled.get(kind).copied().unwrap_or(0) as i64;
            let mut settled = Vec::new();
            for (key, print) in prints.iter() {
                let fields = print.fields();
                if scan_print::settled(&fields, cutoff) {
                    settled.push((key, print));
                } else {
                    let unfinished = scan_print::text(fields.read_through).is_none();
                    let mtime = scan_print::as_i64(fields.mtime_ms);
                    files.push(((unfinished, std::cmp::Reverse(mtime)), key, print));
                }
            }
            if !settled.is_empty() {
                store["settledPacked"][kind] = json!(pack_settled(&settled)?);
                packed.insert(kind.clone());
            }
            // Carry unfinished cursors first. Other files can be re-read to recover any
            // fingerprint omitted to fit the existing scanner frame.
            files.sort_by_key(|(order, _, _)| *order);
        }
        let base = json_len(&Frame {
            args: &one,
            prints: PrintsArg::One(&store, kind, None),
        });
        let mut remaining = REQUEST_BYTES.saturating_sub(base + kind.len() + 12);
        let mut selected = Vec::new();
        for (_, key, print) in files {
            let bytes = json_len(&**key) + print.text().len() + 2;
            if bytes <= remaining {
                selected.push((key, print));
                remaining -= bytes;
            }
        }
        let frame = Frame {
            args: &one,
            prints: PrintsArg::One(&store, kind, Some(&selected)),
        };
        if json_len(&frame) > REQUEST_BYTES {
            return None;
        }
        frame_text(&frame).map(|text| ScanArgs { text, packed })
    }
}

/// Every request of a pass, built at once.
pub(super) fn requests(
    args: Value,
    kinds: &[String],
    fingerprints: &Fingerprints,
    settled: &HashMap<String, u64>,
    now: i64,
) -> Vec<(Vec<String>, Option<ScanArgs>)> {
    let requests = Requests::new(args, kinds, fingerprints, settled, now);
    requests
        .batches
        .iter()
        .map(|batch| (batch.clone(), requests.args(batch, fingerprints)))
        .collect()
}

/// One computer's scanner state: per-file prints, the cached hour and session rows (typed, their
/// strings shared), and the read state of each tool.
#[derive(Default)]
pub(super) struct Cache {
    pub fingerprints: Fingerprints,
    pub rows: Vec<ScanRow>,
    pub srows: Vec<ScanRow>,
    states: Map<String, Value>,
    /// One copy of each row string, for rows this cache reads and gains.
    pub strings: Interner,
    saved: Saved,
    /// Whether the tools this cache has not changed are what the store holds (it was read from,
    /// or saved as, parts).
    backed: bool,
    /// The tools changed since then.
    touched: HashSet<String>,
}

/// A computer's scanner state set aside while a scan call waits, so the pass does not hold it
/// (tens of megabytes on a busy computer) for the whole call: the tools this pass changed,
/// each compressed, and the store's digests of the rest, which [`Parked::restore`] reads back.
pub(super) enum Parked {
    /// Kept as it is (it could not be set aside).
    Held(Box<Cache>),
    Aside(Box<Aside>),
}

pub(super) struct Aside {
    kinds: Vec<String>,
    states: Map<String, Value>,
    saved: Saved,
    backed: bool,
    touched: HashSet<String>,
    /// Each set-aside tool's prints and rows in their order, as one compressed part.
    held: Vec<(String, Vec<u8>)>,
}

/// Where a part read back comes from.
enum Source<'a> {
    Stored(&'a [u8]),
    Compressed(&'a [u8]),
}

impl Parked {
    /// The cache as it was set aside. The parts read back must be the ones the store held then;
    /// any other content is refused (`changed`).
    pub fn restore(self, store: &Engine, host: &str) -> Result<Cache> {
        let aside = match self {
            Parked::Held(cache) => return Ok(*cache),
            Parked::Aside(aside) => *aside,
        };
        let changed = || {
            aac_store::error::Error::new("changed", "Analytics scanner state changed during a scan")
        };
        let mut cache = Cache {
            fingerprints: Fingerprints(
                aside
                    .kinds
                    .into_iter()
                    .map(|kind| (kind, Prints::default()))
                    .collect(),
            ),
            states: aside.states,
            saved: aside.saved,
            backed: aside.backed,
            touched: aside.touched,
            ..Cache::default()
        };
        let mut strings = Interner::default();
        let mut read = |cache: &mut Cache, kind: &str, source: Source<'_>| -> Result<()> {
            let (part, held) = with_interner(std::mem::take(&mut strings), || match source {
                Source::Stored(bytes) => serde_json::from_slice::<Part>(bytes).map_err(Into::into),
                // Streamed: the part's text is never held whole.
                Source::Compressed(bytes) => zstd::stream::read::Decoder::new(bytes)
                    .map_err(aac_store::error::Error::from)
                    .and_then(|decoder| {
                        serde_json::from_reader::<_, Part>(std::io::BufReader::new(decoder))
                            .map_err(Into::into)
                    }),
            });
            strings = held;
            let part: Part = part?;
            // A tool's entry is in the set-aside order already; one it did not have (it holds
            // only rows) is not added.
            if !part.fingerprints.0.is_empty() {
                cache
                    .fingerprints
                    .entry(kind.to_owned())
                    .or_default()
                    .extend(part.fingerprints.0);
            }
            cache.rows.extend(part.rows);
            cache.srows.extend(part.srows);
            Ok(())
        };
        if cache.backed {
            let header = analytics_cache::get(store, &header_key(host))?;
            if header.as_deref().map(digest) != cache.saved.header {
                return Err(changed());
            }
            let aside: HashSet<&str> = aside.held.iter().map(|(kind, _)| kind.as_str()).collect();
            let wanted: Vec<(String, &str)> = cache
                .saved
                .listed
                .iter()
                .filter(|(kind, _)| !aside.contains(kind.as_str()))
                .map(|(kind, bucket)| (part_key(host, kind, *bucket), kind.as_str()))
                .collect();
            let kinds: HashMap<String, String> = wanted
                .iter()
                .map(|(key, kind)| (key.clone(), (*kind).to_owned()))
                .collect();
            let keys: Vec<String> = wanted.into_iter().map(|(key, _)| key).collect();
            let sums = std::mem::take(&mut cache.saved.parts);
            let mut found = 0;
            let result = analytics_cache::read_each(store, &keys, |key, bytes| {
                if sums.get(key) != Some(&digest(bytes)) {
                    return Err(changed());
                }
                found += 1;
                read(&mut cache, &kinds[key], Source::Stored(bytes))
            });
            cache.saved.parts = sums;
            result?;
            if found != keys.len() {
                return Err(changed());
            }
        }
        for (kind, compressed) in aside.held {
            read(&mut cache, &kind, Source::Compressed(&compressed))?;
        }
        cache.strings = strings;
        cache.order_rows();
        Ok(cache)
    }
}

/// The rows of one tool, borrowed.
pub(super) fn of_kind<'a>(rows: &'a [ScanRow], kind: &str) -> Vec<&'a ScanRow> {
    rows.iter().filter(|row| &*row.kind == kind).collect()
}

fn limit(what: &str) -> aac_store::error::Error {
    aac_store::error::Error::new("source_limit", what)
}

impl Cache {
    /// The computer's live checkpoint: its parts, or a revision 1 document (migrated on the next
    /// save), seeded from a legacy remote cache when it has no current one.
    pub fn load(store: &Engine, host: &str) -> Result<Self> {
        let header = analytics_cache::get(store, &header_key(host))?
            .map(|bytes| -> Result<_> {
                Ok((serde_json::from_slice::<Header>(&bytes)?, digest(&bytes)))
            })
            .transpose()?;
        let legacy = match &header {
            Some((_, sum)) if checked(store, host, sum) => None,
            _ => analytics_cache::get(store, &legacy_key(host))?,
        };
        let legacy_digest = legacy.as_deref().map(|bytes| hex(&digest(bytes)));
        if let Some((header, sum)) = header {
            if legacy_digest.is_none() || legacy_digest == header.migrated_from {
                drop(legacy);
                mark_checked(store, host, sum);
                return Self::load_parts(store, host, header, sum);
            }
            // A rolled-back build wrote the revision 1 document after the migration: it is the
            // newer state. Its parts are replaced on the next save.
            let mut cache = Self::read_legacy(store, host, legacy.as_deref())?;
            cache.saved.parts = header
                .parts
                .iter()
                .map(|(kind, bucket)| (part_key(host, kind, *bucket), [0; 32]))
                .collect();
            cache.saved.migrated_from = legacy_digest;
            return Ok(cache);
        }
        let mut cache = Self::read_legacy(store, host, legacy.as_deref())?;
        cache.saved.migrated_from = legacy_digest;
        Ok(cache)
    }

    fn load_parts(store: &Engine, host: &str, header: Header, sum: [u8; 32]) -> Result<Self> {
        let mut cache = Cache {
            fingerprints: Fingerprints(
                header
                    .kinds
                    .into_iter()
                    .map(|kind| (kind, Prints::default()))
                    .collect(),
            ),
            states: header.states,
            saved: Saved {
                header: Some(sum),
                migrated_from: header.migrated_from,
                listed: header.parts.clone(),
                ..Saved::default()
            },
            backed: true,
            ..Cache::default()
        };
        let keys: Vec<String> = header
            .parts
            .iter()
            .map(|(kind, bucket)| part_key(host, kind, *bucket))
            .collect();
        let kinds: HashMap<&str, &str> = keys
            .iter()
            .zip(&header.parts)
            .map(|(key, (kind, _))| (key.as_str(), kind.as_str()))
            .collect();
        let mut strings = std::mem::take(&mut cache.strings);
        analytics_cache::read_each(store, &keys, |key, bytes| {
            let (part, held) = with_interner(std::mem::take(&mut strings), || {
                serde_json::from_slice::<Part>(bytes)
            });
            strings = held;
            let part = part?;
            let kind = kinds.get(key).copied().unwrap_or_default();
            cache
                .fingerprints
                .entry(kind.to_owned())
                .or_default()
                .extend(part.fingerprints.0);
            cache.rows.extend(part.rows);
            cache.srows.extend(part.srows);
            cache.saved.parts.insert(key.to_owned(), digest(bytes));
            Ok(())
        })?;
        cache.strings = strings;
        if cache.saved.parts.len() != keys.len() {
            return Err(aac_store::error::Error::new(
                "io",
                "Analytics scanner state part missing",
            ));
        }
        Ok(cache)
    }

    /// A revision 1 document, or the legacy remote cache's seed.
    fn read_legacy(store: &Engine, host: &str, bytes: Option<&[u8]>) -> Result<Self> {
        let saved = bytes
            .map(|bytes| Stored::read(bytes, Interner::default()))
            .transpose()?;
        let mut cache = match saved {
            Some(stored) if stored.revision == Some(1) => return Ok(stored.into_cache()),
            Some(stored) => Some(stored.into_cache()),
            None => None,
        };
        if let Some(legacy) =
            analytics_cache::archived(store, &format!("{REMOTE_ARCHIVE}{host}.json"))?
                .and_then(|bytes| LegacyCache::read(&bytes).ok())
        {
            let rebuilt: HashSet<_> = aac_providers::scanner_kinds()
                .filter(|tool| legacy.rebuilds(tool))
                .collect();
            let scan = cache.get_or_insert(legacy.scan);
            scan.rows.retain(|row| !rebuilt.contains(&*row.kind));
            scan.srows.retain(|row| !rebuilt.contains(&*row.kind));
            for tool in rebuilt {
                scan.fingerprints.shift_remove(tool);
                scan.states.shift_remove(tool);
            }
        }
        Ok(cache.unwrap_or_default())
    }

    /// A checkpoint's bytes as a cache (any revision; a legacy form's version is not checked).
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        Ok(Stored::read(bytes, Interner::default())?.into_cache())
    }

    fn check_rows(&self) -> Result<()> {
        if self.rows.len() > CACHE_ROWS
            || self.srows.len() > CACHE_ROWS
            || self.fingerprints.count() > CACHE_ROWS
        {
            return Err(limit("Analytics scanner row limit"));
        }
        Ok(())
    }

    /// The cache as one document (revision 1): the historical scans' checkpoint.
    pub fn encode(&self) -> Result<Vec<u8>> {
        self.check_rows()?;
        #[derive(Serialize)]
        struct Checkpoint<'a> {
            #[serde(rename = "_nativeRevision")]
            revision: u8,
            fingerprints: &'a Fingerprints,
            rows: &'a [ScanRow],
            srows: &'a [ScanRow],
            states: &'a Map<String, Value>,
        }
        let bytes = serde_json::to_vec(&Checkpoint {
            revision: 1,
            fingerprints: &self.fingerprints,
            rows: &self.rows,
            srows: &self.srows,
            states: &self.states,
        })?;
        if bytes.len() > CACHE_BYTES {
            return Err(limit("Analytics scanner state size limit"));
        }
        Ok(bytes)
    }

    /// Save the live checkpoint in parts, writing only the parts (and header) whose bytes
    /// changed, in one transaction. Returns what it wrote. A `source_limit`
    /// error leaves the store as it was.
    pub fn save(&mut self, store: &Engine, host: &str) -> Result<Written> {
        self.check_rows()?;
        type Group<'a> = (
            Vec<(&'a Arc<str>, &'a Print)>,
            Vec<&'a ScanRow>,
            Vec<&'a ScanRow>,
        );
        let mut groups: BTreeMap<(&str, u32), Group<'_>> = BTreeMap::new();
        for (kind, prints) in self.fingerprints.iter() {
            for (key, print) in prints.iter() {
                groups
                    .entry((kind, bucket(key)))
                    .or_default()
                    .0
                    .push((key, print));
            }
        }
        for row in &self.rows {
            groups
                .entry((&row.kind, bucket(&row.file)))
                .or_default()
                .1
                .push(row);
        }
        for row in &self.srows {
            groups
                .entry((&row.kind, bucket(&row.file)))
                .or_default()
                .2
                .push(row);
        }
        let mut total = 0;
        let mut writes = Vec::new();
        let mut parts = HashMap::new();
        let mut listed = Vec::new();
        for ((kind, bucket), (mut prints, rows, srows)) in groups {
            prints.sort_by(|left, right| left.0.cmp(right.0));
            let bytes = serde_json::to_vec(&PartRef {
                fingerprints: PrintRefs(prints),
                rows: &rows,
                srows: &srows,
            })?;
            total += bytes.len();
            if total > CACHE_BYTES {
                return Err(limit("Analytics scanner state size limit"));
            }
            let key = part_key(host, kind, bucket);
            let sum = digest(&bytes);
            if self.saved.parts.get(&key) != Some(&sum) {
                writes.push((key.clone(), bytes));
            }
            parts.insert(key, sum);
            listed.push((kind.to_owned(), bucket));
        }
        let removed: Vec<String> = self
            .saved
            .parts
            .keys()
            .filter(|key| !parts.contains_key(*key))
            .cloned()
            .collect();
        let header = serde_json::to_vec(&Header {
            revision: 2,
            kinds: self.fingerprints.keys().cloned().collect(),
            parts: listed.clone(),
            states: self.states.clone(),
            migrated_from: self.saved.migrated_from.clone(),
        })?;
        let header_sum = digest(&header);
        if self.saved.header != Some(header_sum) {
            writes.push((header_key(host), header));
        }
        if writes.is_empty() && removed.is_empty() {
            self.saved.listed = listed;
            self.backed = true;
            self.touched.clear();
            return Ok(Written::default());
        }
        let written = Written {
            keys: writes.len() + removed.len(),
            bytes: writes.iter().map(|(_, bytes)| bytes.len()).sum(),
        };
        analytics_cache::replace_many(store, &writes, &removed)?;
        self.saved.header = Some(header_sum);
        self.saved.parts = parts;
        self.saved.listed = listed;
        self.backed = true;
        self.touched.clear();
        mark_checked(store, host, header_sum);
        Ok(written)
    }

    /// Set the cache aside for a scan call's wait (see [`Parked`]): only the tools changed since
    /// it was read or saved (every tool, for a cache the store does not hold) stay in memory,
    /// compressed; the rest is read back from the store by [`Parked::restore`].
    pub fn park(self) -> Parked {
        let mut aside: std::collections::BTreeSet<&str> =
            self.touched.iter().map(String::as_str).collect();
        if !self.backed {
            aside.extend(self.fingerprints.keys().map(String::as_str));
            aside.extend(self.rows.iter().chain(&self.srows).map(|row| &*row.kind));
        }
        let aside: Vec<String> = aside.into_iter().map(str::to_owned).collect();
        let mut held = Vec::with_capacity(aside.len());
        for kind in aside {
            let prints = self
                .fingerprints
                .get(&kind)
                .map(|prints| prints.iter().collect())
                .unwrap_or_default();
            let part = PartRef {
                fingerprints: PrintRefs(prints),
                rows: &of_kind(&self.rows, &kind),
                srows: &of_kind(&self.srows, &kind),
            };
            let compressed = (|| {
                let mut encoder = zstd::stream::write::Encoder::new(Vec::new(), 1).ok()?;
                serde_json::to_writer(&mut encoder, &part).ok()?;
                encoder.finish().ok()
            })();
            let Some(compressed) = compressed else {
                return Parked::Held(Box::new(self));
            };
            held.push((kind, compressed));
        }
        Parked::Aside(Box::new(Aside {
            kinds: self.fingerprints.keys().cloned().collect(),
            states: self.states,
            saved: self.saved,
            backed: self.backed,
            touched: self.touched,
            held,
        }))
    }

    /// Move one tool's rows out, with the Claude identities of its prints, for a reader that
    /// outlives this cache.
    pub fn take_kind(&mut self, kind: &str) -> (Vec<ScanRow>, Vec<ScanRow>, Identities) {
        self.touched.insert(kind.to_owned());
        let split = |rows: &mut Vec<ScanRow>| {
            let (taken, kept) = std::mem::take(rows)
                .into_iter()
                .partition(|row| &*row.kind == kind);
            *rows = kept;
            taken
        };
        let rows = split(&mut self.rows);
        let srows = split(&mut self.srows);
        let prints = self.fingerprints.shift_remove(kind).unwrap_or_default();
        (rows, srows, Identities::of(&prints))
    }

    /// Hold the rows in the order a saved checkpoint reads them back (by tool and file bucket,
    /// each bucket's rows in their order), so the per-cell sums keep their order across passes.
    fn order_rows(&mut self) {
        for rows in [&mut self.rows, &mut self.srows] {
            rows.sort_by_cached_key(|row| (Arc::clone(&row.kind), bucket(&row.file)));
        }
    }

    /// Apply one tool's read from the answer, moving its prints and accepted rows into the cache.
    pub fn apply(
        &mut self,
        kind: &str,
        answer: &mut Answer,
        mut complete: bool,
        cutoff: i64,
    ) -> bool {
        let state = answer.read(kind);
        if matches!(
            state["state"].as_str(),
            Some("pending" | "error" | "unreadable")
        ) || kind == "antigravity" && state["unreadable"] == true
        {
            return false;
        }
        let truncated = answer.truncated.clone();
        let Some(read) = answer.kinds.as_mut().and_then(|kinds| kinds.get_mut(kind)) else {
            return false;
        };
        let Some(fresh) = read.fingerprints.take() else {
            return false;
        };
        self.touched.insert(kind.to_owned());
        let metadata = read.meta.clone();
        let state = &metadata;
        let mut prior = self
            .fingerprints
            .get_mut(kind)
            .map(std::mem::take)
            .unwrap_or_default();
        let mut replace: HashSet<Arc<str>> = read
            .replaced
            .iter()
            .map(|file| Arc::from(file.as_str()))
            .collect();
        let mut accepted: HashSet<Arc<str>> = HashSet::new();
        let mut rejected: HashSet<Arc<str>> = HashSet::new();
        // Accepted files read whole (no `readFrom`): their cached rows are replaced.
        let mut whole: HashSet<Arc<str>> = HashSet::new();
        let mut prints = Prints::default();
        for (key, print) in fresh.0 {
            let fields = print.fields();
            let old = prior.get(&key);
            if scan_print::is_true(fields.settled_confirmed) {
                let slot = fields.settled_checked_slot.unwrap_or(RawValue::NULL);
                let edited = match prior.get(&key) {
                    Some(saved) if saved.is_object() => {
                        let same = saved.fields().settled_checked_slot.map(RawValue::get)
                            == Some(slot.get());
                        (!same).then(|| saved.with("settledCheckedSlot", slot))
                    }
                    _ => {
                        complete = false;
                        continue;
                    }
                };
                if let Some(saved) = edited.or_else(|| prior.swap_remove(&key)) {
                    prints.insert(key, saved);
                }
                continue;
            }
            if let Some(from) = scan_print::as_u64(fields.read_from) {
                let matches = old.is_some_and(|old| {
                    let old = old.fields();
                    (scan_print::as_u64(old.read_through) == Some(from)
                        && scan_print::same_file(&old, &fields))
                        || (scan_print::text(old.read_through).is_none()
                            && old.append_state.is_some()
                            && scan_print::as_u64(old.size) == Some(from)
                            && scan_print::is_string(fields.file_identity)
                            && scan_print::same(old.file_identity, fields.file_identity))
                });
                if !matches {
                    replace.remove(&key);
                    rejected.insert(key);
                    complete = false;
                    continue;
                }
                replace.remove(&key);
            } else {
                if !old.is_some_and(|old| scan_print::same_file(&old.fields(), &fields)) {
                    replace.insert(Arc::clone(&key));
                }
                if scan_print::text(fields.read_from).is_none() {
                    whole.insert(Arc::clone(&key));
                }
            }
            let saved = if fields.read_from.is_some() {
                print.without("readFrom")
            } else {
                print
            };
            accepted.insert(Arc::clone(&key));
            prints.insert(key, saved);
        }
        // Cursor deduplicates run IDs across every database it reads. With a full
        // inventory, only confirmed paths may contribute, even if some records in
        // those databases were unreadable. A limited inventory keeps missing paths.
        let enumerated = complete
            || kind == "cursor"
                && truncated == false
                && state["state"] == "ok"
                && state["unreadableRecords"].as_u64().is_some_and(|n| n > 0)
                && ["readFailures", "unreadableFolders"]
                    .iter()
                    .all(|field| state[field].as_u64().unwrap_or(0) == 0)
                && state["searchLimit"].is_null()
                && state["walUnread"] != true;
        if !enumerated {
            for (key, old) in prior.0 {
                if !replace.contains(&key) && !rejected.contains(&key) {
                    prints.entry(key).or_insert(old);
                }
            }
        }
        let (rows, srows) = answer.take_rows(kind, |file| accepted.contains(file));
        for row in rows.iter().chain(&srows) {
            if whole.contains(&row.file) {
                replace.insert(Arc::clone(&row.file));
            }
        }
        let retain = |row: &ScanRow| {
            &*row.kind != kind
                || !replace.contains(&row.file) && (!enumerated || prints.contains_key(&row.file))
        };
        let mut window = Window::new(cutoff);
        self.rows
            .retain(|row| retain(row) && window.keeps(row, false));
        self.srows
            .retain(|row| retain(row) && window.keeps(row, true));
        self.rows
            .extend(rows.into_iter().filter(|row| window.keeps(row, false)));
        self.srows
            .extend(srows.into_iter().filter(|row| window.keeps(row, true)));
        fold(&mut self.rows, false);
        fold(&mut self.srows, true);
        self.order_rows();
        self.fingerprints.insert(kind.to_owned(), prints);
        self.strings.prune();
        self.states.insert(kind.to_owned(), metadata);
        complete
    }
}

#[cfg(test)]
impl Cache {
    /// [`Cache::apply`] of an answer given as a value.
    pub fn apply_value(&mut self, kind: &str, answer: &Value, complete: bool, cutoff: i64) -> bool {
        self.apply(kind, &mut Answer::from_value(answer), complete, cutoff)
    }
}

/// The 31-day window: a session row by its last activity, an hour row by its hour (each distinct
/// hour parsed once).
struct Window {
    cutoff: i64,
    hours: HashMap<Arc<str>, bool>,
}

impl Window {
    fn new(cutoff: i64) -> Self {
        Window {
            cutoff,
            hours: HashMap::new(),
        }
    }

    fn keeps(&mut self, row: &ScanRow, session: bool) -> bool {
        if session {
            return row.last.is_some_and(|end| end >= self.cutoff);
        }
        let Some(hour) = &row.hour else {
            return false;
        };
        if let Some(keep) = self.hours.get(hour) {
            return *keep;
        }
        let keep = aac_views::js::parse_date(&format!("{}:00Z", hour.replace(' ', "T")))
            .ok()
            .flatten()
            .is_some_and(|at| at >= self.cutoff.div_euclid(3_600_000) * 3_600_000);
        self.hours.insert(Arc::clone(hour), keep);
        keep
    }
}

/// Merge rows of one (tool, file, model, provider, hour or session) and logged-cost class.
fn fold(rows: &mut Vec<ScanRow>, session: bool) {
    let none: Arc<str> = Arc::from("");
    let mut positions: HashMap<_, usize> = HashMap::new();
    let mut out: Vec<ScanRow> = Vec::with_capacity(rows.len());
    for row in rows.drain(..) {
        let slot = if session { &row.session } else { &row.hour };
        let key = (
            Arc::clone(&row.kind),
            Arc::clone(&row.file),
            Arc::clone(&row.model),
            row.provider.clone().unwrap_or_else(|| Arc::clone(&none)),
            slot.clone().unwrap_or_else(|| Arc::clone(&none)),
            row.cost > 0.0,
        );
        let position = *positions.entry(key).or_insert(out.len());
        if let Some(previous) = out.get_mut(position) {
            for (left, right) in previous.tokens.iter_mut().zip(row.tokens) {
                *left = left.saturating_add(right);
            }
            previous.events = previous.events.saturating_add(row.events);
            previous.cost += row.cost;
            if session {
                if let (Some(left), Some(right)) = (previous.first, row.first) {
                    previous.first = Some(left.min(right));
                }
                if let (Some(left), Some(right)) = (previous.last, row.last) {
                    previous.last = Some(left.max(right));
                }
            }
        } else {
            out.push(row);
        }
    }
    *rows = out;
}

#[cfg(test)]
#[path = "scan_state_oracle.rs"]
mod oracle;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Write;

    #[test]
    fn analytics_deep_cursor_move_keeps_one_readable_contribution() {
        use aac_store::{collect, report};

        let home = tempfile::tempdir().expect("fixture home");
        let projects = home.path().join(".cursor/projects");
        let db = projects.join("original/sdk-agent-store/a/index.db");
        std::fs::create_dir_all(db.parent().expect("store parent")).expect("store directory");
        let now = aac_store::engine::now_ms() as i64;
        let at = aac_num::ecma::to_iso_string(now - 3_600_000).expect("run time");
        let conn = rusqlite::Connection::open(&db).expect("Cursor database");
        conn.execute_batch(
            "CREATE TABLE runs (run_id TEXT, agent_id TEXT, model TEXT,
            started_at TEXT, finished_at TEXT, usage_json TEXT);",
        )
        .expect("Cursor schema");
        for (id, usage) in [
            (
                "readable",
                r#"{"inputTokens":100,"outputTokens":10,"cacheReadTokens":0,"cacheWriteTokens":0,"totalTokens":110}"#,
            ),
            ("bad", r#"{"outputTokens":7}"#),
        ] {
            conn.execute(
                "INSERT INTO runs VALUES (?, 'fixture-agent', 'grok-4.7', ?, ?, ?)",
                rusqlite::params![id, at, at, usage],
            )
            .expect("fixture run");
        }
        conn.close().expect("close fixture database");

        let mut cache = Cache::default();
        let store = aac_store::ops::open_engine(
            home.path()
                .join("store")
                .to_str()
                .expect("fixture store path"),
            None,
            None,
        )
        .expect("activity store");
        let pricing = super::super::ResolverPricing::new(home.path());
        let env = aac_usage::discover::Env::new();
        for moved in [false, true] {
            if moved {
                std::fs::rename(projects.join("original"), projects.join("moved"))
                    .expect("move project");
            }
            let wire = json!({"kinds":["cursor"],"minDateMs":0,"fingerprints":cache.fingerprints});
            let request = aac_usage::protocol::parse_request(wire.to_string().as_bytes())
                .expect("scan request");
            let answer = aac_usage::protocol::scan(&request, home.path(), &env);
            assert!(!cache.apply_value("cursor", &answer, false, 0));
            assert_eq!(cache.states["cursor"]["unreadableRecords"], 1);
            let worker = collect::worker_from_rows(
                &of_kind(&cache.rows, "cursor"),
                &of_kind(&cache.srows, "cursor"),
                "ubuntu",
                now,
                &pricing,
            );
            super::super::history::commit_report(
                &store,
                report::rows_report(&report::RowsReportArgs {
                    computer: "ubuntu",
                    tool: "cursor",
                    data: &[worker],
                    kind: "backfill",
                    read_at: now,
                    complete: false,
                    unreadable_records: Some(1),
                    cause: None,
                    now,
                }),
            )
            .expect("commit scan report");
            let range = aac_store::ops::call(
                &store,
                "activityRange",
                &json!({"fromMs": now - 7_200_000, "toMs": now}).to_string(),
            )
            .expect("stored usage");
            let input: u64 = range["rows"]
                .as_array()
                .expect("hourly rows")
                .iter()
                .filter_map(|row| row[5].as_u64())
                .sum();
            assert_eq!(input, 100, "one run after moved={moved}: {range}");
        }
        let key = cache.rows[0].file.to_string();
        let inaccessible = json!({
            "truncated": false,
            "kinds":{"cursor":{"state":"ok","partial":true,"unreadableRecords":1,
                "unreadableFolders":1,"fingerprints":{}}},
        });
        assert!(!cache.apply_value("cursor", &inaccessible, false, 0));
        assert!(cache.rows.iter().any(|row| *row.file == *key));
    }

    #[test]
    fn scanner_cache_keeps_rows_on_restart_and_adds_or_replaces_per_file() {
        let home = tempfile::tempdir().expect("fixture home");
        let path = home.path().join("store");
        let open = || {
            aac_store::ops::open_engine(&path.to_string_lossy(), None, None)
                .expect("open fixture store")
        };
        let first = json!({"size":100,"mtimeMs":1,"fileIdentity":"9:1","appendState":{}});
        let second =
            json!({"size":150,"mtimeMs":2,"fileIdentity":"9:1","appendState":{},"readFrom":100});
        let replaced = json!({"size":80,"mtimeMs":3,"fileIdentity":"9:2","appendState":{}});
        let answer = |fingerprints: Value, rows: Vec<Value>| {
            json!({
                "kinds":{"codex":{"state":"ok","fingerprints":fingerprints}},"rows":rows,
            })
        };
        let row = |tokens| json!({"k":"codex","f":"file","m":"model","h":"2026-11-03 03:00","i":tokens,"n":1,"c":0});
        let store = open();
        let mut cache = Cache::default();
        assert!(cache.apply_value(
            "codex",
            &answer(json!({"file":first}), vec![row(100)]),
            true,
            0
        ));
        cache.save(&store, "fixture").expect("save scan cache");
        store.close().expect("close fixture store");
        drop(store);
        let store = open();
        cache = Cache::load(&store, "fixture").expect("load scan cache");
        assert!(cache.apply_value(
            "codex",
            &answer(json!({"file":second}), vec![row(50)]),
            true,
            0
        ));
        assert_eq!(cache.rows[0].tokens[0], 150);
        assert_eq!(cache.rows[0].events, 2);
        let prints = cache.fingerprints["codex"].to_value();
        assert!(cache.apply_value("codex", &answer(prints, vec![]), true, 0));
        assert_eq!(cache.rows[0].tokens[0], 150);
        assert!(!cache.apply_value("codex", &answer(json!({}), vec![]), false, 0));
        assert_eq!(cache.rows[0].tokens[0], 150);
        assert!(cache.apply_value(
            "codex",
            &answer(json!({"file":replaced}), vec![row(20)]),
            true,
            0
        ));
        assert_eq!(cache.rows[0].tokens[0], 20);
    }

    fn fixture_store(home: &std::path::Path) -> Engine {
        aac_store::ops::open_engine(&home.join("store").to_string_lossy(), None, None)
            .expect("open fixture store")
    }

    /// Two files of one tool in different parts, read whole (the second one only when given).
    fn files(first: u64, second: Option<u64>) -> Value {
        let (one, two) = two_buckets();
        let mut prints = Map::new();
        let mut rows = Vec::new();
        for (key, size) in [(one, Some(first)), (two, second)] {
            let Some(size) = size else { continue };
            prints.insert(
                key.clone(),
                json!({"size":size,"mtimeMs":size,"fileIdentity":key,"appendState":{}}),
            );
            rows.push(
                json!({"k":"codex","f":key,"m":"model","h":"2026-11-03 03","i":size,"n":1,"c":0}),
            );
        }
        json!({"kinds":{"codex":{"state":"ok","fingerprints":prints}},"rows":rows})
    }

    fn two_files(first: u64, second: u64) -> Value {
        files(first, Some(second))
    }

    /// The cache's content, print order aside.
    fn content(cache: &Cache) -> Value {
        json!({
            "fingerprints": cache.fingerprints.to_value(),
            "rows": cache.rows.iter().map(ScanRow::to_value).collect::<Vec<_>>(),
            "srows": cache.srows.iter().map(ScanRow::to_value).collect::<Vec<_>>(),
            "states": cache.states,
        })
    }

    fn two_buckets() -> (String, String) {
        let one = "file-0".to_owned();
        let two = (1..)
            .map(|n| format!("file-{n}"))
            .find(|key| bucket(key) != bucket(&one))
            .expect("a second bucket");
        (one, two)
    }

    #[test]
    fn an_unchanged_checkpoint_is_not_written_again() {
        let home = tempfile::tempdir().expect("fixture home");
        let store = fixture_store(home.path());
        let mut cache = Cache::default();
        assert!(cache.apply_value("codex", &two_files(5, 7), true, 0));
        // Two parts and the header.
        assert_eq!(cache.save(&store, "fixture").expect("first save").keys, 3);
        assert_eq!(cache.save(&store, "fixture").expect("same bytes").keys, 0);
        // A fresh load knows the stored bytes: an unchanged pass skips its write.
        let mut loaded = Cache::load(&store, "fixture").expect("load");
        assert_eq!(content(&loaded), content(&cache));
        assert!(loaded.apply_value("codex", &two_files(5, 7), true, 0));
        assert_eq!(loaded.save(&store, "fixture").expect("unchanged").keys, 0);
        // One file changed: only its part is written.
        let mut stored = Cache::load(&store, "fixture").expect("reload");
        assert!(stored.apply_value("codex", &two_files(5, 9), true, 0));
        assert_eq!(stored.save(&store, "fixture").expect("changed").keys, 1);
        let reloaded = Cache::load(&store, "fixture").expect("load change");
        let mut tokens: Vec<u64> = reloaded.rows.iter().map(|row| row.tokens[0]).collect();
        tokens.sort_unstable();
        assert_eq!(tokens, [5, 9]);
        assert_eq!(content(&reloaded), content(&stored));
        // A file gone from a complete read: its part is removed with it (and the header lists
        // one part fewer).
        let (one, _) = two_buckets();
        let mut fewer = Cache::load(&store, "fixture").expect("reload");
        assert!(fewer.apply_value("codex", &files(5, None), true, 0));
        assert_eq!(fewer.save(&store, "fixture").expect("removed").keys, 2);
        let left = Cache::load(&store, "fixture").expect("load fewer");
        let keys: Vec<String> = left.fingerprints["codex"]
            .keys()
            .map(|k| k.to_string())
            .collect();
        assert_eq!(keys, [one]);
        assert_eq!(left.rows.len(), 1);
    }

    #[test]
    fn a_single_document_checkpoint_migrates_to_parts_and_stays_for_a_rollback() {
        let home = tempfile::tempdir().expect("fixture home");
        let store = fixture_store(home.path());
        let mut old = Cache::default();
        assert!(old.apply_value("codex", &two_files(5, 7), true, 0));
        let document = old.encode().expect("revision 1 document");
        analytics_cache::put(&store, &legacy_key("fixture"), &document).expect("old build save");

        // The first load reads the document; the save writes parts and keeps it.
        forget_checked();
        let mut migrated = Cache::load(&store, "fixture").expect("migrate");
        assert_eq!(migrated.encode().expect("encode"), document);
        assert_eq!(migrated.save(&store, "fixture").expect("parts").keys, 3);
        assert_eq!(
            analytics_cache::get(&store, &legacy_key("fixture")).expect("kept"),
            Some(document.clone())
        );
        let header: Header = serde_json::from_slice(
            &analytics_cache::get(&store, &header_key("fixture"))
                .expect("header")
                .expect("saved header"),
        )
        .expect("header json");
        assert_eq!(header.revision, 2);
        assert_eq!(header.migrated_from, Some(hex(&digest(&document))));

        // The new build moves on; the kept document is not read back.
        let mut newer = Cache::load(&store, "fixture").expect("parts");
        assert!(newer.apply_value("codex", &two_files(5, 9), true, 0));
        assert_eq!(newer.save(&store, "fixture").expect("one part").keys, 1);
        forget_checked();
        let restarted = Cache::load(&store, "fixture").expect("restart");
        assert_eq!(content(&restarted), content(&newer));

        // A rolled-back build reads the kept document (as it saved it) and writes a newer one.
        let mut rolled_back = Cache::decode(&document).expect("old build load");
        assert!(rolled_back.apply_value("codex", &two_files(6, 11), true, 0));
        let rolled_document = rolled_back.encode().expect("old build encode");
        analytics_cache::put(&store, &legacy_key("fixture"), &rolled_document)
            .expect("old build save");

        // Rolled forward again: the newer document wins and replaces the parts.
        forget_checked();
        let mut forward = Cache::load(&store, "fixture").expect("roll forward");
        assert_eq!(forward.encode().expect("encode"), rolled_document);
        assert_eq!(forward.save(&store, "fixture").expect("rewrite").keys, 3);
        forget_checked();
        let again = Cache::load(&store, "fixture").expect("after roll forward");
        assert_eq!(content(&again), content(&rolled_back));
    }

    #[test]
    fn rows_keep_one_order_across_save_and_load() {
        let home = tempfile::tempdir().expect("fixture home");
        let store = fixture_store(home.path());
        let mut cache = Cache::default();
        let prints: Map<String, Value> = (0..40)
            .map(|n| (format!("f{n}"), json!({"size":n,"mtimeMs":n})))
            .collect();
        let rows: Vec<Value> = (0..40)
            .flat_map(|n| {
                ["omp", "codex"].map(|kind| {
                    json!({"k":kind,"f":format!("f{n}"),"m":"m",
                    "h":"2026-11-03 03","i":n,"n":1,"c":0.1})
                })
            })
            .collect();
        for kind in ["omp", "codex"] {
            let answer = json!({"kinds":{(kind):{"state":"ok","fingerprints":prints}},
                "rows":rows});
            assert!(cache.apply_value(kind, &answer, true, 0));
        }
        cache.save(&store, "fixture").expect("save");
        let loaded = Cache::load(&store, "fixture").expect("load");
        assert_eq!(loaded.rows, cache.rows);
    }

    #[test]
    fn analytics_split_request_keeps_settled_fingerprints_and_cached_usage() {
        let kinds = ["codex".to_owned(), "claude".to_owned()];
        let print = json!({
            "pathKey":"settled-path", "size":100, "mtimeMs":1, "newestUsageMs":1,
            "fileIdentity":"9:1", "head":"first", "tail":"last", "appendState":{"model":"fixture"},
        });
        let fingerprints = Fingerprints::from_value(&json!({
            "codex":{"settled":print,"too-large-active":{"extra":"a".repeat(REQUEST_BYTES)}},
        }));
        let args = super::super::scan_request(&kinds, 0, json!({}), false, None);
        let batches = requests(
            args,
            &kinds,
            &fingerprints,
            &[("codex".into(), 2)].into_iter().collect(),
            600_000,
        );
        assert_eq!(batches.len(), 2);
        let args = batches[0].1.as_ref().expect("codex arguments").value();
        assert_eq!(args["fingerprints"]["codex"], json!({}));
        let request = aac_usage::protocol::parse_request(args.to_string().as_bytes())
            .expect("decode the split scanner frame");
        assert!(request.fingerprints.is_empty());
        assert_eq!(
            request.prior_print_json("codex", "settled").as_ref(),
            Some(&print)
        );

        let mut cache = Cache {
            fingerprints,
            rows: vec![
                serde_json::from_value(
                    json!({"k":"codex","f":"settled","m":"fixture","h":"2026-11-03 03:00","i":100}),
                )
                .expect("fixture row"),
            ],
            ..Cache::default()
        };
        let confirmed = json!({"kinds":{"codex":{"state":"ok","settledPacked":true,
            "fingerprints":{"settled":{"settledConfirmed":true,"settledCheckedSlot":1}}
        }}});
        assert!(cache.apply_value("codex", &confirmed, true, 0));
        assert_eq!(cache.rows[0].tokens[0], 100);
        let saved = cache.fingerprints["codex"]["settled"].to_value();
        assert_eq!(saved["appendState"], print["appendState"]);
        assert_eq!(saved["settledCheckedSlot"], 1);
    }

    #[test]
    fn analytics_deadline_slices_reach_rollout_tail_with_full_read_totals() {
        let home = tempfile::tempdir().expect("fixture home");
        let store_dir = home.path().join("store");
        let store = aac_store::ops::open_engine(&store_dir.to_string_lossy(), None, None)
            .expect("open fixture store");
        let path = home.path().join(".codex/sessions/2026/11/15/rollout-2026-11-15T13-00-00-4a16b43d-60b9-4aab-bbf0-2908136b7bde.jsonl");
        std::fs::create_dir_all(path.parent().expect("fixture parent"))
            .expect("create fixture directory");
        let mut file = std::fs::File::create(path).expect("create fixture rollout");
        writeln!(file, "{}", json!({"type":"session_meta","payload":{"id":"long-fixture","originator":"codex_cli_rs","cli_version":"1"}})).expect("write fixture record");
        writeln!(
            file,
            "{}",
            json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}})
        )
        .expect("write fixture record");
        let filler = "a".repeat(4096);
        let n = 600;
        for index in 1..=n {
            writeln!(
                file,
                "{}",
                json!({"type":"response_item","payload":{"type":"message","content":filler}})
            )
            .expect("write fixture record");
            let at = if index == n {
                "2026-11-15T14:00:00Z"
            } else {
                "2026-11-15T13:00:00Z"
            };
            writeln!(file, "{}", json!({"timestamp":at,"type":"event_msg","payload":{
                "type":"token_count","info":{"total_token_usage":{"input_tokens":100*index,"output_tokens":index}}
            }})).expect("write fixture record");
        }
        drop(file);
        let kinds = ["codex".to_owned()];
        let scan = |cache: &Cache, line_budget: Option<u64>| {
            let args = super::super::scan_request(&kinds, 0, json!({}), false, None);
            let [(batch, request)] =
                requests(args, &kinds, &cache.fingerprints, &HashMap::new(), 0)
                    .try_into()
                    .expect("one scanner request");
            let mut request = aac_usage::protocol::parse_request(
                request.expect("writer arguments").to_string().as_bytes(),
            )
            .expect("parse writer request");
            request.line_budget = line_budget;
            let answer = aac_usage::protocol::scan(
                &request,
                home.path(),
                &aac_usage::discover::Env::default(),
            );
            assert!(super::super::valid_scan_answer(
                &Answer::from_value(&answer),
                &batch
            ));
            answer
        };
        let mut full = Cache::default();
        let full_scan = scan(&full, None);
        assert!(full.apply_value("codex", &full_scan, true, 0));
        assert_eq!(full.rows.len(), 2);

        let mut last_cursor = 0;
        for pass in 1..30 {
            let mut sliced = Cache::load(&store, "fixture").expect("load scan cache");
            let answer = scan(&sliced, Some(79));
            let print = answer["kinds"]["codex"]["fingerprints"]
                .as_object()
                .expect("scanner fingerprints")
                .values()
                .next()
                .expect("rollout fingerprint");
            if let Some(cursor) = print["readThrough"].as_u64() {
                assert!(cursor > last_cursor, "pass {pass} returned an old cursor");
                last_cursor = cursor;
            }
            let readable =
                super::super::has_readable_records(&Answer::from_value(&answer), &sliced, "codex");
            let complete =
                super::super::scan_state(&Answer::from_value(&answer), "codex", readable).0 == "ok";
            assert_eq!(sliced.apply_value("codex", &answer, complete, 0), complete);
            sliced.save(&store, "fixture").expect("save sliced cache");
            if complete {
                assert!(pass > 10);
                assert_eq!(sliced.rows, full.rows);
                assert_eq!(sliced.srows, full.srows);
                assert_eq!(sliced.srows[0].events, n);
                return;
            }
        }
        panic!("the writer never reached the tail of the rollout");
    }

    #[test]
    fn analytics_settled_scan_reports_current_complete_coverage() {
        use crate::analytics_writers::{ResolverPricing, report, scan_source_state, source_state};
        use std::time::{Duration, UNIX_EPOCH};
        const DAY: i64 = 86_400_000;
        let now: i64 = 1_795_212_000_000;
        let used: i64 = 1_794_657_600_000; // 2026-11-14T12:00:00Z
        let home = tempfile::tempdir().expect("fixture home");
        let path = home.path().join(
            ".codex/sessions/2026/11/14/rollout-2026-11-14T12-00-00-3e43c7f5-744b-4790-9d0b-06042ecf0a20.jsonl",
        );
        std::fs::create_dir_all(path.parent().expect("fixture parent")).expect("fixture folder");
        let mut file = std::fs::File::create(&path).expect("fixture rollout");
        for line in [
            json!({"type":"session_meta","payload":{"id":"quiet","model_provider":"openai"}}),
            json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}),
            json!({"timestamp":"2026-11-14T12:00:00Z","type":"event_msg","payload":{
                "type":"token_count","info":{"total_token_usage":{"input_tokens":100,"output_tokens":1}}}}),
        ] {
            writeln!(file, "{line}").expect("fixture record");
        }
        file.set_modified(UNIX_EPOCH + Duration::from_millis((now - 5 * DAY) as u64))
            .expect("fixture time");
        drop(file);
        let kinds = ["codex".to_owned()];
        let settled_before: HashMap<String, u64> = [("codex".to_owned(), (now - 3 * DAY) as u64)]
            .into_iter()
            .collect();
        let cutoff = now - 31 * DAY;
        let pass = |cache: &mut Cache| {
            let args = super::super::scan_request(&kinds, cutoff, json!({}), false, None);
            let [(batch, request)] =
                requests(args, &kinds, &cache.fingerprints, &settled_before, now)
                    .try_into()
                    .expect("one scanner request");
            let request = aac_usage::protocol::parse_request(
                request.expect("writer arguments").to_string().as_bytes(),
            )
            .expect("parse writer request");
            let answer = aac_usage::protocol::scan(
                &request,
                home.path(),
                &aac_usage::discover::Env::default(),
            );
            assert!(super::super::valid_scan_answer(
                &Answer::from_value(&answer),
                &batch
            ));
            let readable =
                super::super::has_readable_records(&Answer::from_value(&answer), cache, "codex");
            let (state, cause) = source_state(&Answer::from_value(&answer), "codex", readable);
            assert!(cache.apply_value("codex", &answer, state == "ok", cutoff));
            let source = scan_source_state(
                &Answer::from_value(&answer),
                "codex",
                "mac",
                now,
                state,
                cause,
            );
            let worker = aac_store::collect::worker_from_rows(
                &of_kind(&cache.rows, "codex"),
                &of_kind(&cache.srows, "codex"),
                "mac",
                now,
                &ResolverPricing::new(home.path()),
            );
            let reports = report::build_activity_reports(&report::ReportArgs {
                local: &HashMap::new(),
                remote: &[json!({"tool":"codex","host":"mac","data":worker})],
                states: &[source],
                window_from_ms: cutoff,
                now,
                local_complete: &HashSet::new(),
                scanner_owned: &HashSet::new(),
            });
            (answer, reports.into_iter().next().expect("codex report"))
        };
        let mut cache = Cache::default();
        let (full, full_report) = pass(&mut cache);
        assert_eq!(full_report["state"], "ok");
        // An older print disclosed an estimated clock: settling must carry the disclosure.
        let disclosed = RawValue::from_string("true".into()).expect("true");
        for print in cache.fingerprints["codex"].values_mut() {
            *print = print.with("estimatedClock", &disclosed);
        }
        let (settled, report) = pass(&mut cache);
        let print = settled["kinds"]["codex"]["fingerprints"]
            .as_object()
            .expect("settled prints")
            .values()
            .next()
            .expect("settled print");
        assert!(print["settledCheckedSlot"].is_u64(), "the rollout settled");
        let discovered = &settled["discovery"]["kinds"]["codex"];
        let before = &full["discovery"]["kinds"]["codex"];
        assert_eq!(discovered["freshness"]["state"], "current");
        assert_eq!(discovered["freshness"]["newestDataMs"], used);
        assert_eq!(
            discovered["counts"]["filesFound"],
            before["counts"]["filesFound"]
        );
        assert_eq!(
            discovered["counts"]["filesRead"],
            before["counts"]["filesRead"]
        );
        assert_eq!(discovered["counts"]["filesRead"], 1);
        assert_eq!(report["state"], "ok");
        assert_eq!(report["complete"], true);
        assert_eq!(report["cause"], Value::Null);
        assert!(report["coverFrom"].is_string());
        assert_eq!(report["counts"]["freshness"]["state"], "current");
        assert_eq!(report["counts"]["freshness"]["newestDataMs"], used);
        assert_eq!(report["counts"]["estimatedClock"], true);
        assert_eq!(report["cells"], full_report["cells"]);
    }

    /// One tool's read: `files` files over many buckets, each with two hour rows and a session,
    /// their sizes and counts set by `generation`; an `appendState` cursor and a Claude identity
    /// on each print, so the prints carry what live ones do.
    fn tool_read(kind: &str, files: usize, generation: u64) -> Value {
        let mut prints = Map::new();
        let mut rows = Vec::new();
        let mut srows = Vec::new();
        for n in 0..files {
            let key = format!("{kind}:/fixture/{n:03}.jsonl");
            let size = 1000 + n as u64 + generation * 10;
            prints.insert(
                key.clone(),
                json!({"size":size,"mtimeMs":size,"fileIdentity":format!("9:{n}"),
                    "head":"h","tail":format!("t{generation}"),
                    "appendState":{"model":"fixture","seen":[n, generation],"pending":""},
                    "claudeName":format!("conversation-{}", n % 7),"claudeMirror":n % 5 == 0}),
            );
            for hour in ["2026-11-03 03", "2026-11-03 04"] {
                rows.push(
                    json!({"k":kind,"f":key,"m":format!("model-{}", n % 3),"h":hour,
                    "i":size,"o":n,"cr":7,"cw":1,"n":1,"c":0.1 * n as f64 + 0.03}),
                );
            }
            srows.push(
                json!({"k":kind,"f":key,"s":format!("session-{n}"),"m":"model-0",
                "a":1_000 + n as i64,"z":2_000 + n as i64 + generation as i64,"i":size,"n":1,
                "c":0.07}),
            );
        }
        json!({"kinds":{(kind):{"state":"ok","fingerprints":prints,"files":files}},
            "rows":rows,"srows":srows})
    }

    const PARK_KINDS: [&str; 3] = ["codex", "omp", "claude"];

    /// A stored checkpoint of three tools, saved the same way to `stores`.
    fn seed_parts(stores: &[&Engine]) {
        for store in stores {
            let mut cache = Cache::default();
            for (index, kind) in PARK_KINDS.iter().enumerate() {
                assert!(cache.apply_value(kind, &tool_read(kind, 90 + index * 10, 1), true, 0));
            }
            cache.save(store, "fixture").expect("seed save");
        }
    }

    fn request_text(cache: &Cache, kind: &str) -> String {
        let kinds: Vec<String> = PARK_KINDS.iter().map(|kind| (*kind).to_owned()).collect();
        let args = super::super::scan_request(&kinds, 0, json!({}), false, None);
        let requests = Requests::new(args, &kinds, &cache.fingerprints, &HashMap::new(), 600_000);
        requests
            .args(&[kind.to_owned()], &cache.fingerprints)
            .expect("request")
            .text
            .get()
            .to_owned()
    }

    fn stored(store: &Engine) -> Vec<(String, Vec<u8>)> {
        let header = analytics_cache::get(store, &header_key("fixture"))
            .expect("header")
            .expect("saved header");
        let parsed: Header = serde_json::from_slice(&header).expect("header json");
        let mut out = vec![(header_key("fixture"), header)];
        for (kind, bucket) in parsed.parts {
            let key = part_key("fixture", &kind, bucket);
            let bytes = analytics_cache::get(store, &key)
                .expect("part")
                .expect("part bytes");
            out.push((key, bytes));
        }
        out
    }

    #[test]
    fn mem7_a_parked_cache_comes_back_exactly_as_it_was_and_saves_the_same_bytes() {
        let (one, two) = (
            tempfile::tempdir().expect("one"),
            tempfile::tempdir().expect("two"),
        );
        let (parked_store, held_store) = (fixture_store(one.path()), fixture_store(two.path()));
        seed_parts(&[&parked_store, &held_store]);
        let mut parked = Cache::load(&parked_store, "fixture").expect("load");
        let mut held = Cache::load(&held_store, "fixture").expect("load");
        // A pass's tools one call at a time, as `activity_pass` runs them: the parked cache is
        // set aside across each call; the held one is the cache as the pass kept it before.
        for kind in PARK_KINDS {
            assert_eq!(request_text(&parked, kind), request_text(&held, kind));
            let aside = parked.park();
            match &aside {
                Parked::Aside(aside) => {
                    let kinds: Vec<&str> = aside.held.iter().map(|(k, _)| k.as_str()).collect();
                    let mut changed: Vec<&str> = PARK_KINDS
                        .iter()
                        .copied()
                        .take_while(|done| *done != kind)
                        .collect();
                    changed.sort_unstable();
                    assert_eq!(kinds, changed, "only the tools this pass changed stay");
                }
                Parked::Held(_) => panic!("a stored cache is set aside"),
            }
            parked = aside.restore(&parked_store, "fixture").expect("restore");
            assert_eq!(
                parked.encode().expect("parked"),
                held.encode().expect("held")
            );
            let read = tool_read(kind, 95, 2);
            assert!(parked.apply_value(kind, &read, true, 0));
            assert!(held.apply_value(kind, &read, true, 0));
            assert_eq!(
                parked.encode().expect("parked"),
                held.encode().expect("held")
            );
        }
        // One last set-aside after every tool changed, then the save writes the same parts.
        parked = parked
            .park()
            .restore(&parked_store, "fixture")
            .expect("restore");
        assert_eq!(parked.rows, held.rows);
        assert_eq!(parked.srows, held.srows);
        let written = parked.save(&parked_store, "fixture").expect("save parked");
        assert_eq!(
            written,
            held.save(&held_store, "fixture").expect("save held")
        );
        assert!(written.keys > 0);
        assert_eq!(stored(&parked_store), stored(&held_store));
        let (rows, srows, _) = parked.take_kind("claude");
        let (held_rows, held_srows, _) = held.take_kind("claude");
        assert_eq!((rows, srows), (held_rows, held_srows));
    }

    #[test]
    fn mem7_a_cache_the_store_does_not_hold_is_set_aside_whole() {
        let home = tempfile::tempdir().expect("fixture home");
        let store = fixture_store(home.path());
        let mut old = Cache::default();
        for kind in PARK_KINDS {
            assert!(old.apply_value(kind, &tool_read(kind, 40, 1), true, 0));
        }
        let document = old.encode().expect("revision 1 document");
        analytics_cache::put(&store, &legacy_key("fixture"), &document).expect("old build save");
        forget_checked();
        let legacy = Cache::load(&store, "fixture").expect("legacy load");
        let aside = legacy.park();
        match &aside {
            Parked::Aside(aside) => assert_eq!(aside.held.len(), PARK_KINDS.len()),
            Parked::Held(_) => panic!("set aside"),
        }
        let back = aside.restore(&store, "fixture").expect("restore");
        assert_eq!(back.encode().expect("encode"), document);
        // A computer with no checkpoint yet.
        let empty = Cache::load(&store, "none")
            .expect("empty")
            .park()
            .restore(&store, "none")
            .expect("restore empty");
        assert_eq!(content(&empty), content(&Cache::default()));
    }

    #[test]
    fn mem7_a_checkpoint_changed_during_the_wait_is_refused() {
        let home = tempfile::tempdir().expect("fixture home");
        let store = fixture_store(home.path());
        seed_parts(&[&store]);
        // Another save of this computer (a newer header).
        let aside = Cache::load(&store, "fixture").expect("load").park();
        let mut other = Cache::load(&store, "fixture").expect("load");
        assert!(other.apply_value("codex", &tool_read("codex", 90, 3), true, 0));
        other.save(&store, "fixture").expect("other save");
        let refused = aside.restore(&store, "fixture").err().expect("refused");
        assert_eq!(refused.code, "changed");
        // One part's bytes replaced under the same header.
        let aside = Cache::load(&store, "fixture").expect("load").park();
        let header: Header = serde_json::from_slice(
            &analytics_cache::get(&store, &header_key("fixture"))
                .expect("header")
                .expect("header bytes"),
        )
        .expect("header json");
        let (kind, bucket) = &header.parts[0];
        analytics_cache::put(
            &store,
            &part_key("fixture", kind, *bucket),
            br#"{"fingerprints":{},"rows":[],"srows":[]}"#,
        )
        .expect("replace a part");
        let refused = aside.restore(&store, "fixture").err().expect("refused");
        assert_eq!(refused.code, "changed");
    }
}
