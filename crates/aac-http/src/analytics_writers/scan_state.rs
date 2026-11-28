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

/// One part: the prints and rows of one tool's files in one bucket (read whole by the tests'
/// oracle; the pass reads a part's prints and rows apart).
#[cfg(test)]
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
    /// The request arguments without prints, and the store settings sent with them.
    fn frame(args: Value, settled: &HashMap<String, u64>, now: i64) -> (Map<String, Value>, Value) {
        let slot = now.max(0) as u64 / aac_usage::collector::SETTLED_CHECK_MS;
        let settings = json!({ "settledBefore": settled, "settledSlot": slot });
        let mut args = match args {
            Value::Object(args) => args,
            _ => Map::new(),
        };
        args.shift_remove("fingerprints");
        (args, settings)
    }

    fn assemble(
        args: Map<String, Value>,
        settings: Value,
        settled: &HashMap<String, u64>,
        kinds: &[String],
        whole: bool,
    ) -> Self {
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

    /// The requests from every tool's prints at once (the historical scans; the tests' oracle
    /// for [`Checkpoint::requests`]).
    pub fn new(
        args: Value,
        kinds: &[String],
        fingerprints: &Fingerprints,
        settled: &HashMap<String, u64>,
        now: i64,
    ) -> Self {
        let (args, settings) = Self::frame(args, settled, now);
        let whole = json_len(&Frame {
            args: &args,
            prints: PrintsArg::All(fingerprints, &settings),
        }) <= REQUEST_BYTES;
        Self::assemble(args, settings, settled, kinds, whole)
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
    /// Whether an apply ran the 31-day window over the rows.
    windowed: bool,
}

/// How the store holds a computer's live checkpoint: in parts (the header read, the parts not),
/// or as a document or legacy seed read whole (migrated to parts on the next save).
enum Opened {
    Parts(Header, [u8; 32]),
    Whole(Cache),
}

/// The computer's live checkpoint as the store holds it: its parts, or a revision 1 document
/// (migrated on the next save), seeded from a legacy remote cache when it has no current one.
fn open(store: &Engine, host: &str) -> Result<Opened> {
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
            return Ok(Opened::Parts(header, sum));
        }
        // A rolled-back build wrote the revision 1 document after the migration: it is the
        // newer state. Its parts are replaced on the next save.
        let mut cache = Cache::read_legacy(store, host, legacy.as_deref())?;
        cache.saved.parts = header
            .parts
            .iter()
            .map(|(kind, bucket)| (part_key(host, kind, *bucket), [0; 32]))
            .collect();
        cache.saved.migrated_from = legacy_digest;
        return Ok(Opened::Whole(cache));
    }
    let mut cache = Cache::read_legacy(store, host, legacy.as_deref())?;
    cache.saved.migrated_from = legacy_digest;
    Ok(Opened::Whole(cache))
}

/// One part's prints, its rows skipped unread.
#[derive(Deserialize)]
struct PartPrints {
    #[serde(default)]
    fingerprints: Prints,
}

fn changed() -> aac_store::error::Error {
    aac_store::error::Error::new("changed", "Analytics scanner state changed during a scan")
}

/// A computer's live checkpoint for one pass, read one tool at a time: the header, then a tool's
/// prints only while its request is built, and a tool's prints and rows only while its answer
/// is applied ([`Checkpoint::change`]). The pass never holds the whole checkpoint (tens of
/// megabytes on a busy computer), only its largest tool. A checkpoint the store does not hold
/// in parts yet is held whole until that save.
pub(super) struct Checkpoint {
    /// The tools of the prints, in the order a whole load holds them.
    kinds: Vec<String>,
    states: Map<String, Value>,
    saved: Saved,
    /// A checkpoint the store does not hold in parts: a revision 1 document or a legacy seed.
    whole: Option<Box<Cache>>,
    /// Every tool's prints while the sizing found they fit one request.
    held: Option<Fingerprints>,
    /// The digest of each part when this pass first read it: a part changed by another writer
    /// before the answers are applied is refused (`changed`).
    seen: HashMap<String, [u8; 32]>,
    strings: Interner,
}

impl Checkpoint {
    pub fn load(store: &Engine, host: &str) -> Result<Self> {
        Ok(match open(store, host)? {
            Opened::Parts(header, sum) => {
                let mut kinds = header.kinds;
                for (kind, _) in &header.parts {
                    if !kinds.contains(kind) {
                        kinds.push(kind.clone());
                    }
                }
                Checkpoint {
                    kinds,
                    states: header.states,
                    saved: Saved {
                        header: Some(sum),
                        migrated_from: header.migrated_from,
                        listed: header.parts,
                        ..Saved::default()
                    },
                    whole: None,
                    held: None,
                    seen: HashMap::new(),
                    strings: Interner::default(),
                }
            }
            Opened::Whole(mut cache) => Checkpoint {
                kinds: cache.fingerprints.keys().cloned().collect(),
                states: std::mem::take(&mut cache.states),
                saved: std::mem::take(&mut cache.saved),
                whole: Some(Box::new(cache)),
                held: None,
                seen: HashMap::new(),
                strings: Interner::default(),
            },
        })
    }

    fn listed_keys(&self, host: &str, kind: &str) -> Vec<String> {
        self.saved
            .listed
            .iter()
            .filter(|(listed, _)| listed == kind)
            .map(|(_, bucket)| part_key(host, kind, *bucket))
            .collect()
    }

    /// Note a part's digest as read now: the first read of this pass, or the same bytes again.
    fn note(seen: &mut HashMap<String, [u8; 32]>, key: &str, bytes: &[u8]) -> Result<[u8; 32]> {
        let sum = digest(bytes);
        match seen.get(key) {
            Some(held) if *held != sum => Err(changed()),
            Some(_) => Ok(sum),
            None => {
                seen.insert(key.to_owned(), sum);
                Ok(sum)
            }
        }
    }

    /// One tool's prints as the store holds them (their rows are not read).
    fn prints(&mut self, store: &Engine, host: &str, kind: &str) -> Result<Prints> {
        if let Some(whole) = &self.whole {
            return Ok(whole.fingerprints.get(kind).cloned().unwrap_or_default());
        }
        let keys = self.listed_keys(host, kind);
        let mut prints = Prints::default();
        let mut found = 0;
        let seen = &mut self.seen;
        let (result, _) = with_interner(Interner::default(), || {
            analytics_cache::read_each(store, &keys, |key, bytes| {
                Self::note(seen, key, bytes)?;
                found += 1;
                prints.extend(serde_json::from_slice::<PartPrints>(bytes)?.fingerprints.0);
                Ok(())
            })
        });
        result?;
        if found != keys.len() {
            return Err(aac_store::error::Error::new(
                "io",
                "Analytics scanner state part missing",
            ));
        }
        Ok(prints)
    }

    /// The pass's requests, sized one tool at a time: the whole frame is the frame without
    /// prints plus each tool's entry, so a busy computer's are known to need one request per
    /// tool once the tools read so far pass the limit. Every tool's prints are kept only when
    /// they fit one request (the request is built from them).
    pub fn requests(
        &mut self,
        store: &Engine,
        host: &str,
        args: Value,
        kinds: &[String],
        settled: &HashMap<String, u64>,
        now: i64,
    ) -> Result<Requests> {
        let (args, settings) = Requests::frame(args, settled, now);
        if let Some(whole) = &self.whole {
            let whole = json_len(&Frame {
                args: &args,
                prints: PrintsArg::All(&whole.fingerprints, &settings),
            }) <= REQUEST_BYTES;
            return Ok(Requests::assemble(args, settings, settled, kinds, whole));
        }
        let mut total = json_len(&Frame {
            args: &args,
            prints: PrintsArg::All(&Fingerprints::default(), &settings),
        });
        let mut held = Fingerprints::default();
        let mut whole = true;
        for kind in self.kinds.clone() {
            let prints = self.prints(store, host, &kind)?;
            // `"<kind>":<prints>,` before the store settings.
            total += json_len(kind.as_str()) + json_len(&prints) + 2;
            if total > REQUEST_BYTES {
                whole = false;
                break;
            }
            held.insert(kind, prints);
        }
        self.held = whole.then_some(held);
        Ok(Requests::assemble(args, settings, settled, kinds, whole))
    }

    /// The arguments of the request for `batch`, read from the prints as the pass found them;
    /// None when the tool's prints cannot be carried within the frame.
    pub fn args(
        &mut self,
        store: &Engine,
        host: &str,
        requests: &Requests,
        batch: &[String],
    ) -> Result<Option<ScanArgs>> {
        if let Some(whole) = &self.whole {
            return Ok(requests.args(batch, &whole.fingerprints));
        }
        if requests.whole {
            let held = self.held.take().unwrap_or_default();
            return Ok(requests.args(batch, &held));
        }
        let mut one = Fingerprints::default();
        if let Some(kind) = batch.first() {
            one.insert(kind.clone(), self.prints(store, host, kind)?);
        }
        Ok(requests.args(batch, &one))
    }

    /// Apply the pass's answers in one store transaction, one tool at a time ([`Change`]). `run`
    /// returns its result and whether the checkpoint is saved; a save that cannot be made
    /// (`source_limit`, a failed commit) changes nothing in the store. `keep`'s rows, session rows
    /// and identities are moved out as the save leaves them. An error (the checkpoint changed
    /// since it was read, a part that cannot be read) changes nothing and is returned.
    pub fn change<T>(
        &mut self,
        store: &Engine,
        host: &str,
        cutoff: i64,
        keep: Option<&str>,
        run: impl FnOnce(&mut Change<'_, '_, '_>) -> Result<(T, bool)>,
    ) -> Result<Changed<T>> {
        let (result, committed) = analytics_cache::rewrite(store, |txn| {
            if self.saved.header.is_some() {
                let header = txn.read(&header_key(host), digest)?;
                if header != self.saved.header {
                    return Err(changed());
                }
            }
            let mut change = Change::new(self, txn, host, cutoff, keep);
            let (out, save) = run(&mut change)?;
            let saved = save.then(|| change.finish());
            if let Some(Ok((written, _))) = &saved
                && written.keys > 0
            {
                change.txn.commit();
            }
            let kept_parts = match (&saved, keep.and_then(|kind| change.tools.get(kind))) {
                (Some(Ok(_)), Some(tool)) => Some(KeptParts {
                    parts: tool
                        .parts
                        .iter()
                        .map(|(bucket, sum)| {
                            (part_key(host, keep.unwrap_or_default(), *bucket), *sum)
                        })
                        .collect(),
                    rows: tool.rows,
                    srows: tool.srows,
                }),
                _ => None,
            };
            Ok(Changed {
                out,
                saved,
                kept: change.kept.take(),
                kept_parts,
            })
        })?;
        let mut changed = result;
        match (&mut changed.saved, committed) {
            (Some(Ok((_, Some(header)))), Ok(())) => mark_checked(store, host, *header),
            (Some(saved), Err(error)) => {
                *saved = Err(error);
                changed.kept_parts = None;
            }
            _ => {}
        }
        Ok(changed)
    }
}

/// What [`Checkpoint::change`] gave: `run`'s result, the save (None when not saved; the header
/// digest when it wrote one), and the kept tool's rows.
pub(super) struct Changed<T> {
    pub out: T,
    pub saved: Option<Result<(Written, Option<[u8; 32]>)>>,
    pub kept: Option<(Vec<ScanRow>, Vec<ScanRow>, Identities)>,
    /// The kept tool's parts once the store holds them as this pass saved them: [`read_kept`]
    /// reads its rows back.
    pub kept_parts: Option<KeptParts>,
}

/// A tool's saved parts (key, digest), and its row and session row counts.
pub(super) struct KeptParts {
    pub parts: Vec<(String, [u8; 32])>,
    pub rows: usize,
    pub srows: usize,
}

/// A tool's rows, session rows and identities read back from the parts a pass saved: what
/// [`Changed::kept`] held. A part that is not what the pass saved is refused (`changed`).
pub(super) fn read_kept(
    store: &Engine,
    kept: &KeptParts,
) -> Result<(Vec<ScanRow>, Vec<ScanRow>, Identities)> {
    let keys: Vec<String> = kept.parts.iter().map(|(key, _)| key.clone()).collect();
    let sums: HashMap<&str, &[u8; 32]> = kept
        .parts
        .iter()
        .map(|(key, sum)| (key.as_str(), sum))
        .collect();
    // Sized as saved: grown a part at a time, the rows would double past their count.
    let mut rows = Vec::with_capacity(kept.rows);
    let mut srows = Vec::with_capacity(kept.srows);
    let mut identities = Identities::default();
    let mut names = HashMap::new();
    let mut found = 0;
    let (result, _) = with_interner(Interner::default(), || {
        analytics_cache::read_each(store, &keys, |key, bytes| {
            if sums.get(key).copied() != Some(&digest(bytes)) {
                return Err(changed());
            }
            found += 1;
            let part: PartKept<'_> = serde_json::from_slice(bytes)?;
            for (file, print) in part.fingerprints {
                identities.add_text(aac_store::scan_row::shared(&file), print, &mut names);
            }
            rows.extend(part.rows);
            srows.extend(part.srows);
            Ok(())
        })
    });
    result?;
    if found != keys.len() {
        return Err(changed());
    }
    rows.shrink_to_fit();
    srows.shrink_to_fit();
    Ok((rows, srows, identities))
}

/// One tool's parts as the transaction now holds them.
struct ToolParts {
    /// (bucket, digest), by bucket.
    parts: Vec<(u32, [u8; 32])>,
    rows: usize,
    srows: usize,
    prints: usize,
    bytes: usize,
}

/// One part read back for its tool's identities and rows: its prints read in place, uncopied.
#[derive(Deserialize)]
struct PartKept<'a> {
    #[serde(borrow, default)]
    fingerprints: HashMap<std::borrow::Cow<'a, str>, &'a RawValue>,
    #[serde(default)]
    rows: Vec<ScanRow>,
    #[serde(default)]
    srows: Vec<ScanRow>,
}

/// One part's rows, its prints skipped unread.
#[derive(Deserialize)]
struct PartRows {
    #[serde(default)]
    rows: Vec<ScanRow>,
    #[serde(default)]
    srows: Vec<ScanRow>,
}

/// What [`Cache::apply`]'s loop does with one fresh print, given the tool's saved print of that
/// file (the same decision, taken twice: once for the whole answer, once per bucket).
enum Outcome {
    /// A settled print confirmed: the saved one kept (`edit`: with the new check slot).
    Settled { edit: bool },
    /// A settled confirmation without a saved print: the read is not complete.
    Unsaved,
    /// A continued read that does not continue the saved one.
    Rejected,
    /// Accepted: `cursor` (a continued read) leaves the file's rows; `changed` (another file)
    /// and `whole` (read from its start) replace them.
    Accepted {
        cursor: bool,
        changed: bool,
        whole: bool,
    },
}

fn outcome(print: &Print, old: Option<&Print>) -> Outcome {
    let fields = print.fields();
    if scan_print::is_true(fields.settled_confirmed) {
        let slot = fields.settled_checked_slot.unwrap_or(RawValue::NULL);
        return match old {
            Some(saved) if saved.is_object() => Outcome::Settled {
                edit: saved.fields().settled_checked_slot.map(RawValue::get) != Some(slot.get()),
            },
            _ => Outcome::Unsaved,
        };
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
            return Outcome::Rejected;
        }
        return Outcome::Accepted {
            cursor: true,
            changed: false,
            whole: false,
        };
    }
    Outcome::Accepted {
        cursor: false,
        changed: !old.is_some_and(|old| scan_print::same_file(&old.fields(), &fields)),
        whole: scan_print::text(fields.read_from).is_none(),
    }
}

/// An applied answer's prints, kept until the tool is written back one bucket at a time.
struct Plan {
    /// The answer's prints, by bucket, in the answer's order.
    fresh: BTreeMap<u32, Vec<(Arc<str>, Print)>>,
    /// The files whose rows the loop replaced (before the files read whole are added).
    replace: HashSet<Arc<str>>,
    rejected: HashSet<Arc<str>>,
    /// Whether only the answer's prints are kept (otherwise the saved ones it left stay).
    enumerated: bool,
}

/// The pass's change to one computer's checkpoint, inside one store transaction: each tool is
/// read ([`Change::tool`]: its rows), applied by the caller ([`Change::apply`]), then written back
/// ([`Change::put`]) one file bucket at a time: each bucket's saved prints are read, the answer's
/// applied to them, and the part written when its bytes changed. A tool's prints are never held
/// whole. The save ([`Change::finish`]) reads and writes back every tool this pass did not
/// apply, then the header, so the store holds what one save of the whole checkpoint writes.
///
/// One apply runs the 31-day window over every row the checkpoint held then (see
/// [`Cache::apply`]): a tool read after a windowed apply is windowed as it is read, and one read
/// before it is read again and windowed by the save.
pub(super) struct Change<'c, 'r, 't> {
    checkpoint: &'c mut Checkpoint,
    txn: &'r mut analytics_cache::Rewrite<'t>,
    host: &'c str,
    cutoff: i64,
    /// Whether an applied answer has run the window over the checkpoint.
    windowed: bool,
    /// The tools written back, in order, and whether their rows have been through the window.
    done: Vec<(String, bool)>,
    tools: BTreeMap<String, ToolParts>,
    /// The digest of each part as the transaction holds it.
    current: HashMap<String, [u8; 32]>,
    /// The parts read once in this transaction (their digest checked against the pass's reads).
    verified: HashSet<String>,
    written: Written,
    keep: Option<&'c str>,
    kept: Option<(Vec<ScanRow>, Vec<ScanRow>, Identities)>,
    /// A tool is read and not yet written back; whether it was windowed as it was read.
    loaded: bool,
    /// The read tool's prints by bucket, when the checkpoint is held whole.
    loose: Option<BTreeMap<u32, Prints>>,
    plan: Option<Plan>,
}

impl Change<'_, '_, '_> {
    fn new<'c, 'r, 't>(
        checkpoint: &'c mut Checkpoint,
        txn: &'r mut analytics_cache::Rewrite<'t>,
        host: &'c str,
        cutoff: i64,
        keep: Option<&'c str>,
    ) -> Change<'c, 'r, 't> {
        let current = checkpoint.saved.parts.clone();
        Change {
            checkpoint,
            txn,
            host,
            cutoff,
            windowed: false,
            done: Vec::new(),
            tools: BTreeMap::new(),
            current,
            verified: HashSet::new(),
            written: Written::default(),
            keep,
            kept: None,
            loaded: false,
            loose: None,
            plan: None,
        }
    }

    /// The string table an answer's rows share with the tools' rows.
    pub fn strings(&mut self) -> &mut Interner {
        &mut self.checkpoint.strings
    }

    /// The kept tool's rows, session rows and identities, once it is written back (again by the
    /// save when it windows that tool).
    pub fn take_kept(&mut self) -> Option<(Vec<ScanRow>, Vec<ScanRow>, Identities)> {
        self.kept.take()
    }

    /// The buckets of the tool's parts as the transaction holds them.
    fn buckets(&self, kind: &str) -> Vec<u32> {
        match self.tools.get(kind) {
            Some(tool) => tool.parts.iter().map(|(bucket, _)| *bucket).collect(),
            None => self
                .checkpoint
                .saved
                .listed
                .iter()
                .filter(|(listed, _)| listed == kind)
                .map(|(_, bucket)| *bucket)
                .collect(),
        }
    }

    /// One part's bytes handed to `read`: the first read of a part in this transaction must find
    /// the bytes this pass read before (`changed` otherwise).
    fn part<T>(&mut self, key: &str, read: impl FnOnce(&[u8]) -> Result<T>) -> Result<T> {
        let first = !self.verified.contains(key);
        let seen = &mut self.checkpoint.seen;
        let out = self.txn.read(key, |bytes| -> Result<_> {
            let sum = if first {
                Some(Checkpoint::note(seen, key, bytes)?)
            } else {
                None
            };
            Ok((sum, read(bytes)?))
        })?;
        let Some(out) = out else {
            return Err(aac_store::error::Error::new(
                "io",
                "Analytics scanner state part missing",
            ));
        };
        let (sum, out) = out?;
        if let Some(sum) = sum {
            self.current.insert(key.to_owned(), sum);
            self.verified.insert(key.to_owned());
        }
        Ok(out)
    }

    /// The tool's saved prints of one bucket.
    fn prior(&mut self, kind: &str, bucket: u32, held: bool) -> Result<Prints> {
        if let Some(loose) = &mut self.loose {
            return Ok(loose.get(&bucket).cloned().unwrap_or_default());
        }
        if !held {
            return Ok(Prints::default());
        }
        let key = part_key(self.host, kind, bucket);
        let (prints, _) = with_interner(Interner::default(), || {
            self.part(&key, |bytes| {
                Ok(serde_json::from_slice::<PartPrints>(bytes)?.fingerprints)
            })
        });
        prints
    }

    /// One tool's rows as a cache of that tool alone (holding the tools' read states while it is
    /// out; its prints stay in the store), windowed when an earlier apply ran the window.
    pub fn tool(&mut self, kind: &str) -> Result<Cache> {
        let mut rows = Vec::new();
        let mut srows = Vec::new();
        self.loose = None;
        self.plan = None;
        if !self.tools.contains_key(kind)
            && let Some(whole) = self.checkpoint.whole.as_mut()
        {
            let prints = whole
                .fingerprints
                .get_mut(kind)
                .map(std::mem::take)
                .unwrap_or_default();
            let mut loose: BTreeMap<u32, Prints> = BTreeMap::new();
            for (key, print) in prints.0 {
                loose.entry(bucket(&key)).or_default().insert(key, print);
            }
            self.loose = Some(loose);
            let split = |rows: &mut Vec<ScanRow>| {
                let (taken, kept): (Vec<_>, Vec<_>) = std::mem::take(rows)
                    .into_iter()
                    .partition(|row| &*row.kind == kind);
                *rows = kept;
                taken
            };
            rows = split(&mut whole.rows);
            srows = split(&mut whole.srows);
        } else {
            let mut strings = std::mem::take(&mut self.checkpoint.strings);
            let mut result = Ok(());
            for bucket in self.buckets(kind) {
                let key = part_key(self.host, kind, bucket);
                let (part, held) = with_interner(std::mem::take(&mut strings), || {
                    self.part(&key, |bytes| Ok(serde_json::from_slice::<PartRows>(bytes)?))
                });
                strings = held;
                match part {
                    Ok(part) => {
                        rows.extend(part.rows);
                        srows.extend(part.srows);
                    }
                    Err(error) => {
                        result = Err(error);
                        break;
                    }
                }
            }
            self.checkpoint.strings = strings;
            result?;
        }
        rows.shrink_to_fit();
        srows.shrink_to_fit();
        let mut cache = Cache {
            rows,
            srows,
            states: std::mem::take(&mut self.checkpoint.states),
            strings: std::mem::take(&mut self.checkpoint.strings),
            ..Cache::default()
        };
        self.loaded = self.windowed;
        if self.windowed {
            cache.window(self.cutoff);
        }
        Ok(cache)
    }

    /// [`Cache::apply`] of the read tool, its saved prints read one bucket at a time: the same
    /// rows, read state and answer, and the same prints once [`Change::put`] writes them.
    pub fn apply(
        &mut self,
        kind: &str,
        cache: &mut Cache,
        answer: &mut Answer,
        mut complete: bool,
    ) -> Result<bool> {
        let state = answer.read(kind);
        if matches!(
            state["state"].as_str(),
            Some("pending" | "error" | "unreadable")
        ) || kind == "antigravity" && state["unreadable"] == true
        {
            return Ok(false);
        }
        let truncated = answer.truncated.clone();
        let Some(read) = answer.kinds.as_mut().and_then(|kinds| kinds.get_mut(kind)) else {
            return Ok(false);
        };
        let Some(fresh) = read.fingerprints.take() else {
            return Ok(false);
        };
        let metadata = read.meta.clone();
        let state = &metadata;
        let mut replace: HashSet<Arc<str>> = read
            .replaced
            .iter()
            .map(|file| Arc::from(file.as_str()))
            .collect();
        let mut by_bucket: BTreeMap<u32, Vec<(Arc<str>, Print)>> = BTreeMap::new();
        for (key, print) in fresh.0 {
            by_bucket
                .entry(bucket(&key))
                .or_default()
                .push((key, print));
        }
        let held: HashSet<u32> = self.buckets(kind).into_iter().collect();
        let mut accepted: HashSet<Arc<str>> = HashSet::new();
        let mut rejected: HashSet<Arc<str>> = HashSet::new();
        // Accepted files read whole (no `readFrom`): their cached rows are replaced.
        let mut whole: HashSet<Arc<str>> = HashSet::new();
        // The files the loop keeps a print of.
        let mut kept: HashSet<Arc<str>> = HashSet::new();
        for (bucket, entries) in &by_bucket {
            let prior = self.prior(kind, *bucket, held.contains(bucket))?;
            for (key, print) in entries {
                match outcome(print, prior.get(key)) {
                    Outcome::Settled { .. } => {
                        kept.insert(Arc::clone(key));
                    }
                    Outcome::Unsaved => complete = false,
                    Outcome::Rejected => {
                        replace.remove(key);
                        rejected.insert(Arc::clone(key));
                        complete = false;
                    }
                    Outcome::Accepted {
                        cursor,
                        changed,
                        whole: read_whole,
                    } => {
                        if cursor {
                            replace.remove(key);
                        }
                        if changed {
                            replace.insert(Arc::clone(key));
                        }
                        if read_whole {
                            whole.insert(Arc::clone(key));
                        }
                        accepted.insert(Arc::clone(key));
                        kept.insert(Arc::clone(key));
                    }
                }
            }
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
        let (rows, srows) = answer.take_rows(kind, |file| accepted.contains(file));
        let mut replace_rows = replace.clone();
        for row in rows.iter().chain(&srows) {
            if whole.contains(&row.file) {
                replace_rows.insert(Arc::clone(&row.file));
            }
        }
        // Without an enumerated read, the saved prints the loop did not replace stay: their
        // rows stay with them.
        let retain = |row: &ScanRow| {
            &*row.kind != kind
                || !replace_rows.contains(&row.file) && (!enumerated || kept.contains(&row.file))
        };
        let mut window = Window::new(self.cutoff);
        cache.windowed = true;
        cache
            .rows
            .retain(|row| retain(row) && window.keeps(row, false));
        cache
            .srows
            .retain(|row| retain(row) && window.keeps(row, true));
        cache.rows.reserve_exact(rows.len());
        cache.srows.reserve_exact(srows.len());
        cache
            .rows
            .extend(rows.into_iter().filter(|row| window.keeps(row, false)));
        cache
            .srows
            .extend(srows.into_iter().filter(|row| window.keeps(row, true)));
        fold(&mut cache.rows, false);
        fold(&mut cache.srows, true);
        cache.order_rows();
        cache.strings.prune();
        cache.states.insert(kind.to_owned(), metadata);
        self.plan = Some(Plan {
            fresh: by_bucket,
            replace,
            rejected,
            enumerated,
        });
        Ok(complete)
    }

    /// Write a tool back one bucket at a time: each bucket's prints (its saved prints, with the
    /// applied answer's) and rows, written when the transaction does not hold those bytes yet;
    /// then the tool is dropped (the kept tool's rows are moved out first).
    pub fn put(&mut self, kind: &str, mut cache: Cache) -> Result<()> {
        let windowed = cache.windowed || self.loaded;
        if cache.windowed {
            self.windowed = true;
            // The apply added the tool's prints entry (a whole cache's map keeps its order).
            if !self.checkpoint.kinds.iter().any(|held| held == kind) {
                self.checkpoint.kinds.push(kind.to_owned());
            }
        }
        self.checkpoint.states = std::mem::take(&mut cache.states);
        self.checkpoint.strings = std::mem::take(&mut cache.strings);
        let mut plan = self.plan.take();
        let held: Vec<u32> = match &self.loose {
            Some(loose) => loose.keys().copied().collect(),
            None => self.buckets(kind),
        };
        let mut rows: BTreeMap<u32, (Vec<&ScanRow>, Vec<&ScanRow>)> = BTreeMap::new();
        for row in &cache.rows {
            rows.entry(bucket(&row.file)).or_default().0.push(row);
        }
        for row in &cache.srows {
            rows.entry(bucket(&row.file)).or_default().1.push(row);
        }
        let mut buckets: std::collections::BTreeSet<u32> = held.iter().copied().collect();
        buckets.extend(rows.keys());
        if let Some(plan) = &plan {
            buckets.extend(plan.fresh.keys());
        }
        let held: HashSet<u32> = held.into_iter().collect();
        let keep = self.keep == Some(kind);
        let mut identities = Identities::default();
        let mut names = HashMap::new();
        let mut tool = ToolParts {
            parts: Vec::new(),
            rows: cache.rows.len(),
            srows: cache.srows.len(),
            prints: 0,
            bytes: 0,
        };
        for bucket in buckets {
            let mut prior = match self.loose.as_mut() {
                Some(loose) => loose.remove(&bucket).unwrap_or_default(),
                None => self.prior(kind, bucket, held.contains(&bucket))?,
            };
            let prints = match plan.as_mut() {
                None => prior,
                Some(plan) => {
                    let mut prints = Prints::default();
                    for (key, print) in plan.fresh.remove(&bucket).unwrap_or_default() {
                        let saved = match outcome(&print, prior.get(&key)) {
                            Outcome::Settled { edit: true } => {
                                let slot = print
                                    .fields()
                                    .settled_checked_slot
                                    .unwrap_or(RawValue::NULL);
                                prior
                                    .get(&key)
                                    .map(|saved| saved.with("settledCheckedSlot", slot))
                            }
                            Outcome::Settled { edit: false } => prior.swap_remove(&key),
                            Outcome::Unsaved | Outcome::Rejected => None,
                            Outcome::Accepted { .. } => {
                                Some(if print.fields().read_from.is_some() {
                                    print.without("readFrom")
                                } else {
                                    print
                                })
                            }
                        };
                        if let Some(saved) = saved {
                            prints.insert(key, saved);
                        }
                    }
                    if !plan.enumerated {
                        for (key, old) in prior.0 {
                            if !plan.replace.contains(&key) && !plan.rejected.contains(&key) {
                                prints.entry(key).or_insert(old);
                            }
                        }
                    }
                    prints
                }
            };
            let (bucket_rows, bucket_srows) = rows.remove(&bucket).unwrap_or_default();
            if prints.is_empty() && bucket_rows.is_empty() && bucket_srows.is_empty() {
                continue;
            }
            tool.prints += prints.len();
            if keep {
                identities.add(&prints, &mut names);
            }
            let mut sorted: Vec<(&Arc<str>, &Print)> = prints.iter().collect();
            sorted.sort_by(|left, right| left.0.cmp(right.0));
            let bytes = serde_json::to_vec(&PartRef {
                fingerprints: PrintRefs(sorted),
                rows: &bucket_rows,
                srows: &bucket_srows,
            })?;
            tool.bytes += bytes.len();
            let key = part_key(self.host, kind, bucket);
            let sum = digest(&bytes);
            if self.current.get(&key) != Some(&sum) {
                self.txn.insert(&key, &bytes)?;
                self.written.keys += 1;
                self.written.bytes += bytes.len();
                self.current.insert(key, sum);
            }
            tool.parts.push((bucket, sum));
        }
        drop(rows);
        self.loose = None;
        self.tools.insert(kind.to_owned(), tool);
        match self.done.iter_mut().find(|(done, _)| done == kind) {
            Some(done) => done.1 |= windowed,
            None => self.done.push((kind.to_owned(), windowed)),
        }
        if keep {
            self.kept = Some((
                std::mem::take(&mut cache.rows),
                std::mem::take(&mut cache.srows),
                identities,
            ));
        }
        drop(cache);
        self.checkpoint.strings.prune();
        Ok(())
    }

    /// The save: every tool not windowed yet is windowed when an apply ran the window, every
    /// tool this pass did not apply is written back as it is, then the limits are checked, the
    /// emptied parts removed and the header written. Returns what it wrote and the header's
    /// digest when it wrote one; a `source_limit` error leaves the transaction to be dropped.
    fn finish(&mut self) -> Result<(Written, Option<[u8; 32]>)> {
        if self.windowed {
            let behind: Vec<String> = self
                .done
                .iter()
                .filter(|(_, windowed)| !windowed)
                .map(|(kind, _)| kind.clone())
                .collect();
            for kind in behind {
                let cache = self.tool(&kind)?;
                self.put(&kind, cache)?;
            }
        }
        let mut rest: Vec<String> = self.checkpoint.kinds.clone();
        if let Some(whole) = &self.checkpoint.whole {
            for row in whole.rows.iter().chain(&whole.srows) {
                if !rest.iter().any(|kind| **kind == *row.kind) {
                    rest.push(row.kind.to_string());
                }
            }
        }
        rest.retain(|kind| !self.tools.contains_key(kind));
        for kind in rest {
            let cache = self.tool(&kind)?;
            self.put(&kind, cache)?;
        }
        let total = |count: fn(&ToolParts) -> usize| self.tools.values().map(count).sum::<usize>();
        if total(|tool| tool.rows) > CACHE_ROWS
            || total(|tool| tool.srows) > CACHE_ROWS
            || total(|tool| tool.prints) > CACHE_ROWS
        {
            return Err(limit("Analytics scanner row limit"));
        }
        if total(|tool| tool.bytes) > CACHE_BYTES {
            return Err(limit("Analytics scanner state size limit"));
        }
        let mut listed = Vec::new();
        let mut parts = HashMap::new();
        for (kind, tool) in &self.tools {
            for (bucket, sum) in &tool.parts {
                listed.push((kind.clone(), *bucket));
                parts.insert(part_key(self.host, kind, *bucket), *sum);
            }
        }
        let mut removed: Vec<&String> = self
            .current
            .keys()
            .filter(|key| !parts.contains_key(*key))
            .collect();
        removed.sort();
        for key in &removed {
            self.txn.remove(key)?;
        }
        self.written.keys += removed.len();
        let header = serde_json::to_vec(&Header {
            revision: 2,
            kinds: self.checkpoint.kinds.clone(),
            parts: listed,
            states: self.checkpoint.states.clone(),
            migrated_from: self.checkpoint.saved.migrated_from.clone(),
        })?;
        let header_sum = digest(&header);
        let mut wrote = None;
        if self.checkpoint.saved.header != Some(header_sum) {
            self.txn.insert(&header_key(self.host), &header)?;
            self.written.keys += 1;
            self.written.bytes += header.len();
            wrote = Some(header_sum);
        }
        if self.written.keys > 0 && wrote.is_none() {
            // Parts changed under an unchanged header: the check of the next load is by header.
            wrote = self.checkpoint.saved.header;
        }
        Ok((std::mem::take(&mut self.written), wrote))
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
    /// The computer's live checkpoint read whole (the flow before spike 8, kept as the tests'
    /// oracle): its parts, or a revision 1 document (migrated on the next save).
    #[cfg(test)]
    pub fn load(store: &Engine, host: &str) -> Result<Self> {
        match open(store, host)? {
            Opened::Parts(header, sum) => Self::load_parts(store, host, header, sum),
            Opened::Whole(cache) => Ok(cache),
        }
    }

    #[cfg(test)]
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
    /// error leaves the store as it was. (The whole-cache save before spike 8: the tests'
    /// oracle for [`Change`].)
    #[cfg(test)]
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
        mark_checked(store, host, header_sum);
        Ok(written)
    }

    /// Move one tool's rows out, with the Claude identities of its prints, for a reader that
    /// outlives this cache.
    #[cfg(test)]
    pub fn take_kind(&mut self, kind: &str) -> (Vec<ScanRow>, Vec<ScanRow>, Identities) {
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

    /// What an apply of another tool does to these rows: the 31-day window, then the merge and
    /// order of every apply.
    fn window(&mut self, cutoff: i64) {
        let mut window = Window::new(cutoff);
        self.rows.retain(|row| window.keeps(row, false));
        self.srows.retain(|row| window.keeps(row, true));
        fold(&mut self.rows, false);
        fold(&mut self.srows, true);
        self.order_rows();
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
        self.windowed = true;
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

/// Merge rows of one (tool, file, model, provider, hour or session) and logged-cost class, in
/// place: each first row keeps its order and takes the later ones' sums.
fn fold(rows: &mut Vec<ScanRow>, session: bool) {
    let none: Arc<str> = Arc::from("");
    let mut positions: HashMap<_, usize> = HashMap::new();
    let mut write = 0;
    for read in 0..rows.len() {
        let row = &rows[read];
        let slot = if session { &row.session } else { &row.hour };
        let key = (
            Arc::clone(&row.kind),
            Arc::clone(&row.file),
            Arc::clone(&row.model),
            row.provider.clone().unwrap_or_else(|| Arc::clone(&none)),
            slot.clone().unwrap_or_else(|| Arc::clone(&none)),
            row.cost > 0.0,
        );
        match positions.get(&key) {
            Some(&position) => {
                // Rows before `write` are the first rows kept; the merged ones are dropped.
                let (kept, rest) = rows.split_at_mut(read);
                let (previous, row) = (&mut kept[position], &rest[0]);
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
            }
            None => {
                positions.insert(key, write);
                rows.swap(write, read);
                write += 1;
            }
        }
    }
    rows.truncate(write);
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

    /// The kept tool's rows and identities, comparable.
    fn kept_value(kept: &(Vec<ScanRow>, Vec<ScanRow>, Identities)) -> Value {
        let mut identities: Vec<(String, String, bool)> = kept
            .2
            .entries()
            .map(|(file, name, mirror)| (file.to_owned(), name.to_owned(), mirror))
            .collect();
        identities.sort();
        json!({
            "rows": kept.0.iter().map(ScanRow::to_value).collect::<Vec<_>>(),
            "srows": kept.1.iter().map(ScanRow::to_value).collect::<Vec<_>>(),
            "identities": identities,
        })
    }

    /// One pass's reads applied both ways: the whole cache (the flow before spike 8) to
    /// `whole`, the checkpoint one tool at a time to `parts`. Each tool's rows before its apply
    /// (what its source state reads) and each apply's answer must agree, and both stores must
    /// then hold the same bytes and the same Claude part.
    fn both_ways(whole: &Engine, parts: &Engine, reads: &[(&str, Value)], cutoff: i64) {
        let mut cache = Cache::load(whole, "fixture").expect("whole load");
        let mut expected = Vec::new();
        for (kind, read) in reads {
            let before = content(&cache)["rows"]
                .as_array()
                .expect("rows")
                .iter()
                .filter(|row| row["k"] == *kind)
                .count();
            expected.push((before, cache.apply_value(kind, read, true, cutoff)));
        }
        cache.save(whole, "fixture").expect("whole save");
        let whole_kept = cache.take_kind("claude");
        let mut checkpoint = Checkpoint::load(parts, "fixture").expect("checkpoint load");
        let changed = checkpoint
            .change(parts, "fixture", cutoff, Some("claude"), |change| {
                let mut seen = Vec::new();
                for (kind, read) in reads {
                    let mut cache = change.tool(kind)?;
                    let before = cache.rows.len();
                    let mut answer = Answer::from_value(read);
                    seen.push((before, change.apply(kind, &mut cache, &mut answer, true)?));
                    change.put(kind, cache)?;
                }
                Ok((seen, true))
            })
            .expect("change");
        assert_eq!(changed.out, expected);
        changed.saved.expect("saved").expect("save");
        same_store(&stored(parts), &stored(whole));
        assert_eq!(
            kept_value(&changed.kept.expect("kept")),
            kept_value(&whole_kept)
        );
        // The Claude part read back from the saved checkpoint is the same part.
        let read = read_kept(parts, &changed.kept_parts.expect("kept parts")).expect("read back");
        assert_eq!(kept_value(&read), kept_value(&whole_kept));
    }

    /// The two stores' checkpoints, compared key by key (the first difference shown as text).
    fn same_store(left: &[(String, Vec<u8>)], right: &[(String, Vec<u8>)]) {
        let keys = |held: &[(String, Vec<u8>)]| {
            held.iter().map(|(key, _)| key.clone()).collect::<Vec<_>>()
        };
        for ((key, left), (_, right)) in left.iter().zip(right) {
            if left != right {
                let at = left.iter().zip(right).take_while(|(l, r)| l == r).count();
                let show = |bytes: &[u8]| {
                    String::from_utf8_lossy(
                        &bytes[at.saturating_sub(200)..(at + 300).min(bytes.len())],
                    )
                    .into_owned()
                };
                panic!(
                    "{key} differs at {at}:\n  one tool at a time: {}\n  whole cache:        {}",
                    show(left),
                    show(right)
                );
            }
        }
        assert_eq!(keys(left), keys(right));
    }

    #[test]
    fn spike8_one_tool_at_a_time_saves_what_the_whole_cache_saves() {
        let (one, two) = (
            tempfile::tempdir().expect("one"),
            tempfile::tempdir().expect("two"),
        );
        let (whole, parts) = (fixture_store(one.path()), fixture_store(two.path()));
        seed_parts(&[&whole, &parts]);
        // Every tool read again.
        let all: Vec<(&str, Value)> = PARK_KINDS
            .iter()
            .enumerate()
            .map(|(index, kind)| (*kind, tool_read(kind, 95 + index * 10, 2)))
            .collect();
        both_ways(&whole, &parts, &all, 0);
        // Unchanged: nothing to write.
        both_ways(&whole, &parts, &all, 0);
        // The first tool's read is still pending (its apply stops before the window), the second
        // runs the window over every tool (the hour 03 rows and every session fall out of it),
        // and Claude has no answer: the save windows the first and the last as the whole
        // cache's apply did.
        let mut pending = tool_read("codex", 95, 3);
        pending["kinds"]["codex"]["state"] = json!("pending");
        let cutoff = aac_views::js::parse_date("2026-11-03T04:00:00Z")
            .expect("date")
            .expect("cutoff");
        both_ways(
            &whole,
            &parts,
            &[("codex", pending), ("omp", tool_read("omp", 80, 3))],
            cutoff,
        );
        // A tool new to the checkpoint, and one tool's read only.
        both_ways(
            &whole,
            &parts,
            &[
                ("cursor", tool_read("cursor", 12, 1)),
                ("claude", tool_read("claude", 60, 4)),
            ],
            cutoff,
        );
    }

    #[test]
    fn spike8_a_document_checkpoint_migrates_one_tool_at_a_time() {
        let (one, two) = (
            tempfile::tempdir().expect("one"),
            tempfile::tempdir().expect("two"),
        );
        let (whole, parts) = (fixture_store(one.path()), fixture_store(two.path()));
        let mut old = Cache::default();
        for (index, kind) in PARK_KINDS.iter().enumerate() {
            assert!(old.apply_value(kind, &tool_read(kind, 30 + index, 1), true, 0));
        }
        let document = old.encode().expect("revision 1 document");
        for store in [&whole, &parts] {
            analytics_cache::put(store, &legacy_key("fixture"), &document).expect("old save");
        }
        forget_checked();
        both_ways(&whole, &parts, &[("omp", tool_read("omp", 33, 2))], 0);
        assert_eq!(
            analytics_cache::get(&parts, &legacy_key("fixture")).expect("kept"),
            Some(document)
        );
        both_ways(&whole, &parts, &[("codex", tool_read("codex", 31, 2))], 0);
    }

    #[test]
    fn spike8_requests_sized_one_tool_at_a_time_match_the_whole_frame() {
        let home = tempfile::tempdir().expect("fixture home");
        let store = fixture_store(home.path());
        seed_parts(&[&store]);
        let kinds: Vec<String> = PARK_KINDS.iter().map(|kind| (*kind).to_owned()).collect();
        let settled = HashMap::from([("codex".to_owned(), 1_000u64)]);
        let cache = Cache::load(&store, "fixture").expect("load");
        let args = super::super::scan_request(&kinds, 0, json!({}), false, None);
        let whole = Requests::new(args.clone(), &kinds, &cache.fingerprints, &settled, 600_000);
        let mut checkpoint = Checkpoint::load(&store, "fixture").expect("checkpoint");
        let sized = checkpoint
            .requests(&store, "fixture", args.clone(), &kinds, &settled, 600_000)
            .expect("sized");
        assert!(whole.whole && sized.whole);
        assert_eq!(sized.batches, whole.batches);
        let text = |args: Option<ScanArgs>| args.expect("fits").text.get().to_owned();
        assert_eq!(
            text(
                checkpoint
                    .args(&store, "fixture", &sized, &kinds)
                    .expect("args")
            ),
            text(whole.args(&kinds, &cache.fingerprints))
        );
        // The whole frame's length is the frame without prints plus each tool's entry.
        let (frame, settings) = Requests::frame(args, &settled, 600_000);
        let mut total = json_len(&Frame {
            args: &frame,
            prints: PrintsArg::All(&Fingerprints::default(), &settings),
        });
        for (kind, prints) in cache.fingerprints.iter() {
            total += json_len(kind.as_str()) + json_len(prints) + 2;
        }
        assert_eq!(
            total,
            json_len(&Frame {
                args: &frame,
                prints: PrintsArg::All(&cache.fingerprints, &settings),
            })
        );
        // One request per tool: each built from that tool's prints alone.
        let split = Requests::assemble(frame, settings, &settled, &kinds, false);
        for kind in &kinds {
            let batch = [kind.clone()];
            assert_eq!(
                text(
                    checkpoint
                        .args(&store, "fixture", &split, &batch)
                        .expect("args")
                ),
                text(split.args(&batch, &cache.fingerprints))
            );
        }
    }

    #[test]
    fn spike8_a_checkpoint_changed_during_the_calls_is_refused() {
        let home = tempfile::tempdir().expect("fixture home");
        let store = fixture_store(home.path());
        seed_parts(&[&store]);
        let kinds: Vec<String> = PARK_KINDS.iter().map(|kind| (*kind).to_owned()).collect();
        let request = |checkpoint: &mut Checkpoint| {
            let args = super::super::scan_request(&kinds, 0, json!({}), false, None);
            let requests = checkpoint
                .requests(&store, "fixture", args, &kinds, &HashMap::new(), 600_000)
                .expect("requests");
            checkpoint
                .args(&store, "fixture", &requests, &kinds)
                .expect("args");
        };
        let unchanged = |change: &mut Change<'_, '_, '_>| -> Result<((), bool)> {
            for kind in PARK_KINDS {
                let cache = change.tool(kind)?;
                change.put(kind, cache)?;
            }
            Ok(((), true))
        };
        // Another save of this computer (a newer header).
        let mut checkpoint = Checkpoint::load(&store, "fixture").expect("load");
        request(&mut checkpoint);
        let mut other = Cache::load(&store, "fixture").expect("load");
        assert!(other.apply_value("codex", &tool_read("codex", 90, 3), true, 0));
        other.save(&store, "fixture").expect("other save");
        let before = stored(&store);
        let refused = checkpoint
            .change(&store, "fixture", 0, None, unchanged)
            .err()
            .expect("refused");
        assert_eq!(refused.code, "changed");
        assert_eq!(stored(&store), before);
        // Nothing to save: the store is not written.
        let mut checkpoint = Checkpoint::load(&store, "fixture").expect("load");
        let changed = checkpoint
            .change(&store, "fixture", 0, None, unchanged)
            .expect("unchanged");
        assert_eq!(
            changed.saved.expect("saved").expect("save").0,
            Written::default()
        );
        assert_eq!(stored(&store), before);
        // One part's bytes replaced under the same header after its request was built.
        let mut checkpoint = Checkpoint::load(&store, "fixture").expect("load");
        request(&mut checkpoint);
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
        let before = stored(&store);
        let refused = checkpoint
            .change(&store, "fixture", 0, None, unchanged)
            .err()
            .expect("refused");
        assert_eq!(refused.code, "changed");
        assert_eq!(stored(&store), before);
    }
}
