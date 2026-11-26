//! Agent-report runs, kept forever (Scoreboard v2, share `2026-11-13-scoreboard-v2/DESIGN.md`).
//!
//! The share's run files are the ingest source; the ledger log is the record (`run.*` events,
//! never pruned) and the `run_*` tables are this projector's projections of it:
//!
//! | Table | Key | Value |
//! |---|---|---|
//! | `run_dict`, `run_dict_rev` | kind u8, id u32 / kind u8, text | this projector's own strings |
//! | `run_meta` | text | current catalog hash, dictionary counters |
//! | `run_head` | r u32 | the winning revision's row (`Head`, binary v2, with the revision count) |
//! | `run_rev` | r u32, rev u16 | every revision's row (`Head`, binary v2): dedupe by run id + content hash; its path is the greatest path that content was seen at |
//! | `run_payload` | day u32, block u32 | the original documents, up to 32 per block (raw while open, zstd once full) |
//! | `run_by_time` | endedAt ms u64, r u32 | current revisions by end time (the page's date range) |
//! | `run_file` | sha256(path)[0..16] | the file's last stat and content hash (the scan's skip); for a refused file only its reason and time, never its path, name or writer |
//! | `run_catalog` | sha256 catalog[0..16] | each catalog version used (zstd) |
//!
//! The current revision is the one TypeScript's runs reader would pick: the highest `seq` (the
//! full accepted range), then the later `writtenAt`, then the greatest path (`run.path` records a
//! greater path for content already on record).
//!
//! Every key is fixed-width big-endian; values carry a leading version byte. Scanning, the
//! run@1 projection and normalization are here in Rust. Every text passes the shared secret
//! policy (config/secret-policy.json) twice: TypeScript's `jsonFromBytes` and `findSecret`
//! before `record`, then `record` itself over run documents, file paths and writer folders,
//! thread keys, documents and the values taken from them, catalog texts, refusal reasons and
//! the ids and identity of each run. It is the one detector; this module has no thresholds.

use crate::db::{Key, be_u32, be_u64};
use crate::engine::{DAY_MS, Engine, LogId, PROJECTOR_BATCH, Projector};
use crate::error::Result;
use crate::event::Event;
use crate::fail;
use crate::log::Finding;
use crate::msgpack;
use crate::run_identity::{self, Catalog, Identity, Written};
use crate::run_schema::{self, RUN_CAP_BYTES};
use crate::secret_policy::{find_secret, find_secret_in};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, WriteTransaction};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

type Bytes = TableDefinition<'static, &'static [u8], &'static [u8]>;

pub const RUN_DICT: Bytes = TableDefinition::new("run_dict");
pub const RUN_DICT_REV: TableDefinition<'static, &'static [u8], u32> = TableDefinition::new("run_dict_rev");
pub const RUN_META: TableDefinition<'static, &'static str, &'static [u8]> = TableDefinition::new("run_meta");
pub const RUN_HEAD: Bytes = TableDefinition::new("run_head");
pub const RUN_REV: Bytes = TableDefinition::new("run_rev");
pub const RUN_PAYLOAD: Bytes = TableDefinition::new("run_payload");
pub const RUN_BY_TIME: TableDefinition<'static, &'static [u8], ()> = TableDefinition::new("run_by_time");
pub const RUN_FILE: Bytes = TableDefinition::new("run_file");
pub const RUN_CATALOG: Bytes = TableDefinition::new("run_catalog");

/// Bump when projection rules change: the run tables are then rebuilt from the ledger.
pub const RUNS_PROJECTOR_VERSION: u32 = 3;
/// Original documents per payload block.
pub const BLOCK_DOCS: u16 = 32;
const ZSTD_LEVEL: i32 = 9;
const DICT_TEXT_MAX: usize = 2048;
/// Events per commit (well under the projector batch and the 1 MiB group).
const EVENTS_PER_COMMIT: usize = 256;
/// Files read per scan pass by default; the next pass continues.
pub const SCAN_BUDGET: usize = 4000;
const THREAD_CAP_BYTES: u64 = 16 * 1024;
const CATALOG_CAP_BYTES: u64 = 256 * 1024;
const UNKNOWN: u64 = u64::MAX;

/// Kinds of this projector's own dictionary.
pub(crate) mod kind {
    pub const RUN_ID: u8 = 1;
    pub const WRITER: u8 = 2;
    pub const MACHINE: u8 = 3;
    pub const COMPUTER: u8 = 4;
    pub const T3_THREAD: u8 = 5;
    pub const T3_TASK: u8 = 6;
    pub const TASK_ID: u8 = 7;
    pub const TASK_TYPE: u8 = 8;
    pub const PROJECT: u8 = 9;
    pub const STREAM: u8 = 10;
    pub const OUTCOME: u8 = 11;
    pub const BENCHMARK: u8 = 12;
    pub const PROVIDER: u8 = 13;
    pub const ACCOUNT: u8 = 14;
    pub const MODEL_RAW: u8 = 15;
    pub const MODEL: u8 = 16;
    pub const FAMILY: u8 = 17;
    pub const EFFORT_RAW: u8 = 18;
    pub const EFFORT: u8 = 19;
    pub const HARNESS_RAW: u8 = 20;
    pub const HARNESS: u8 = 21;
    pub const PATH: u8 = 22;
    pub const CATALOG: u8 = 23;
    pub const REASON: u8 = 24;
    pub const WORD: u8 = 25;
}

pub fn create_tables(txn: &WriteTransaction) -> Result<()> {
    txn.open_table(RUN_DICT)?;
    txn.open_table(RUN_DICT_REV)?;
    txn.open_table(RUN_META)?;
    for def in [RUN_HEAD, RUN_REV, RUN_PAYLOAD, RUN_FILE, RUN_CATALOG] {
        txn.open_table(def)?;
    }
    txn.open_table(RUN_BY_TIME)?;
    crate::scoreboard_agg::create_tables(txn)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Small helpers

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub(crate) fn unhex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

pub(crate) fn unhex16(text: &str) -> Option<[u8; 16]> {
    if text.len() != 32 {
        return None;
    }
    let mut out = [0u8; 16];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

/// A path's key in `run_file` and its hash in refusals: the first 16 bytes of
/// `sha256("aac-run-path-v1:" + path)` (`pathHash` in `src/web-server/scoreboard/runs-store.ts`).
pub(crate) fn path_key(path: &str) -> [u8; 16] {
    let digest = sha256(format!("aac-run-path-v1:{path}").as_bytes());
    digest[..16].try_into().unwrap()
}

fn get(txn: &WriteTransaction, def: Bytes, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let table = txn.open_table(def)?;
    let value = table.get(key)?.map(|v| v.value().to_vec());
    Ok(value)
}

fn put(txn: &WriteTransaction, def: Bytes, key: &[u8], value: &[u8]) -> Result<()> {
    txn.open_table(def)?.insert(key, value)?;
    Ok(())
}

pub(crate) fn meta_get(txn: &WriteTransaction, key: &str) -> Result<Option<String>> {
    let table = txn.open_table(RUN_META)?;
    let value = table.get(key)?.map(|v| String::from_utf8_lossy(v.value()).into_owned());
    Ok(value)
}

pub(crate) fn meta_set(txn: &WriteTransaction, key: &str, value: &str) -> Result<()> {
    txn.open_table(RUN_META)?.insert(key, value.as_bytes())?;
    Ok(())
}

pub(crate) const META_FILE_REV: &str = "file_rev";

pub(crate) fn read_file_rev_rd(read: &redb::ReadTransaction) -> Result<u64> {
    let meta = read.open_table(RUN_META)?;
    Ok(meta
        .get(META_FILE_REV)?
        .and_then(|v| std::str::from_utf8(v.value()).ok()?.parse::<u64>().ok())
        .unwrap_or(0))
}

pub(crate) fn read_file_rev(engine: &Engine) -> Result<u64> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        read_file_rev_rd(&read)
    })
}

pub(crate) fn bump_file_rev_tx(txn: &WriteTransaction) -> Result<u64> {
    let next = meta_get(txn, META_FILE_REV)?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0)
        + 1;
    meta_set(txn, META_FILE_REV, &next.to_string())?;
    Ok(next)
}

/// This projector's id for `text` under `kind` (0 means none). Ids start at 1 per kind.
fn intern(txn: &WriteTransaction, kind: u8, text: Option<&str>) -> Result<u32> {
    let Some(text) = text else { return Ok(0) };
    if text.len() > DICT_TEXT_MAX {
        fail!("invalid", "a run string is over {DICT_TEXT_MAX} bytes");
    }
    let rev_key = Key::new().u8(kind).bytes(text.as_bytes());
    {
        let rev = txn.open_table(RUN_DICT_REV)?;
        if let Some(id) = rev.get(rev_key.as_slice())? {
            return Ok(id.value());
        }
    }
    let counter = format!("dict_next:{kind}");
    let next: u32 = meta_get(txn, &counter)?.and_then(|v| v.parse().ok()).unwrap_or(1);
    meta_set(txn, &counter, &(next + 1).to_string())?;
    txn.open_table(RUN_DICT_REV)?.insert(rev_key.as_slice(), next)?;
    txn.open_table(RUN_DICT)?
        .insert(Key::new().u8(kind).u32(next).as_slice(), text.as_bytes())?;
    Ok(next)
}

pub(crate) fn lookup_id<T: ReadableTable<&'static [u8], u32>>(rev: &T, kind: u8, text: &str) -> Result<Option<u32>> {
    Ok(rev
        .get(Key::new().u8(kind).bytes(text.as_bytes()).as_slice())?
        .map(|v| v.value()))
}

/// Reads dictionary text through a cache (queries read the same few strings many times).
pub(crate) struct Dict<'a, T: ReadableTable<&'static [u8], &'static [u8]>> {
    table: &'a T,
    cache: HashMap<(u8, u32), Option<String>>,
}

impl<'a, T: ReadableTable<&'static [u8], &'static [u8]>> Dict<'a, T> {
    pub(crate) fn new(table: &'a T) -> Self {
        Dict {
            table,
            cache: HashMap::new(),
        }
    }
    pub(crate) fn text(&mut self, kind: u8, id: u32) -> Result<Option<String>> {
        if id == 0 {
            return Ok(None);
        }
        if let Some(hit) = self.cache.get(&(kind, id)) {
            return Ok(hit.clone());
        }
        let text = self
            .table
            .get(Key::new().u8(kind).u32(id).as_slice())?
            .map(|v| String::from_utf8_lossy(v.value()).into_owned());
        self.cache.insert((kind, id), text.clone());
        Ok(text)
    }
    /// A head row's string field, by its index in `HEAD_IDS`.
    pub(crate) fn field(&mut self, head: &Head, at: usize) -> Result<Option<String>> {
        self.text(HEAD_IDS[at], head.ids[at])
    }
}

// ---------------------------------------------------------------------------------------------
// Binary values (v1)

/// The fields of a head row stored as dictionary ids, in their stored order.
const HEAD_IDS: [u8; 26] = [
    kind::WRITER,
    kind::MACHINE,
    kind::COMPUTER,
    kind::T3_THREAD,
    kind::T3_TASK,
    kind::TASK_ID,
    kind::TASK_TYPE,
    kind::PROJECT,
    kind::STREAM,
    kind::OUTCOME,
    kind::BENCHMARK,
    kind::PROVIDER,
    kind::ACCOUNT,
    kind::MODEL_RAW,
    kind::MODEL,
    kind::FAMILY,
    kind::EFFORT_RAW,
    kind::EFFORT,
    kind::HARNESS_RAW,
    kind::HARNESS,
    kind::PATH,
    kind::CATALOG,
    kind::WORD, // model source
    kind::WORD, // effort source
    kind::WORD, // harness source
    kind::WORD, // t3 thread source
];

const F_BENCHMARK: u8 = 1;
const F_IMPORTED: u8 = 2;
const F_REVIEW: u8 = 4;
const F_OVERTURNED: u8 = 8;
const F_DISPUTED: u8 = 16;
const F_UNKNOWN_MODEL: u8 = 32;
const F_EFFORT_CONFLICT: u8 = 64;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Loc {
    pub day: u32,
    pub block: u32,
    pub slot: u16,
}

/// One revision's row (binary v2): every revision in `run_rev`, and the winner's copy in
/// `run_head` with the revision count. `seq` keeps the full accepted range (to 2^53 - 1), and the
/// path is the greatest path this content was seen at (the winner's tie-break, as TypeScript's).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Head {
    pub flags: u8,
    /// 0 none, 1 pass, 2 fail, 3 cannot-verify.
    pub verdict: u8,
    /// Revisions of the run (in `run_head`; 0 in `run_rev`).
    pub revisions: u16,
    pub seq: u64,
    pub written_ms: u64,
    pub ingested_ms: u64,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub wall_ms: u64,
    /// The ledger sequence of the event that recorded this revision.
    pub event_seq: u64,
    pub sha: [u8; 32],
    pub loc: Loc,
    /// Dictionary ids in `HEAD_IDS` order.
    pub ids: [u32; 26],
}

const ROW_VERSION: u8 = 2;
const HEAD_LEN: usize = 1 + 1 + 1 + 2 + 8 * 7 + 32 + 4 + 4 + 2 + 26 * 4;

impl Head {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEAD_LEN);
        out.push(ROW_VERSION);
        out.push(self.flags);
        out.push(self.verdict);
        out.extend_from_slice(&self.revisions.to_be_bytes());
        for v in [
            self.seq,
            self.written_ms,
            self.ingested_ms,
            self.started_ms,
            self.ended_ms,
            self.wall_ms,
            self.event_seq,
        ] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&self.sha);
        out.extend_from_slice(&self.loc.day.to_be_bytes());
        out.extend_from_slice(&self.loc.block.to_be_bytes());
        out.extend_from_slice(&self.loc.slot.to_be_bytes());
        for id in self.ids {
            out.extend_from_slice(&id.to_be_bytes());
        }
        out
    }

    pub fn decode(b: &[u8]) -> Result<Head> {
        if b.first() != Some(&ROW_VERSION) || b.len() != HEAD_LEN {
            fail!("unsupported", "run row value version or length");
        }
        let mut ids = [0u32; 26];
        for (i, id) in ids.iter_mut().enumerate() {
            *id = be_u32(b, 103 + i * 4);
        }
        Ok(Head {
            flags: b[1],
            verdict: b[2],
            revisions: u16::from_be_bytes([b[3], b[4]]),
            seq: be_u64(b, 5),
            written_ms: be_u64(b, 13),
            ingested_ms: be_u64(b, 21),
            started_ms: be_u64(b, 29),
            ended_ms: be_u64(b, 37),
            wall_ms: be_u64(b, 45),
            event_seq: be_u64(b, 53),
            sha: b[61..93].try_into().unwrap(),
            loc: Loc {
                day: be_u32(b, 93),
                block: be_u32(b, 97),
                slot: u16::from_be_bytes([b[101], b[102]]),
            },
            ids,
        })
    }

    pub(crate) fn id(&self, kind_at: usize) -> u32 {
        self.ids[kind_at]
    }
}

/// Index of a field in `HEAD_IDS`.
pub(crate) mod at {
    pub const WRITER: usize = 0;
    pub const MACHINE: usize = 1;
    pub const COMPUTER: usize = 2;
    pub const T3_THREAD: usize = 3;
    pub const T3_TASK: usize = 4;
    pub const TASK_ID: usize = 5;
    pub const TASK_TYPE: usize = 6;
    pub const PROJECT: usize = 7;
    pub const STREAM: usize = 8;
    pub const OUTCOME: usize = 9;
    pub const BENCHMARK: usize = 10;
    pub const PROVIDER: usize = 11;
    pub const ACCOUNT: usize = 12;
    pub const MODEL_RAW: usize = 13;
    pub const MODEL: usize = 14;
    pub const FAMILY: usize = 15;
    pub const EFFORT_RAW: usize = 16;
    pub const EFFORT: usize = 17;
    pub const HARNESS_RAW: usize = 18;
    pub const HARNESS: usize = 19;
    pub const PATH: usize = 20;
    pub const CATALOG: usize = 21;
    pub const MODEL_SOURCE: usize = 22;
    pub const EFFORT_SOURCE: usize = 23;
    pub const HARNESS_SOURCE: usize = 24;
    pub const T3_THREAD_SOURCE: usize = 25;
}

/// A file's last stat: the scan skips it while size and mtime stay the same. A refused file's
/// row holds no path, file name or writer (it is keyed by the path's hash): only the reason and
/// when it was refused.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FileRow {
    /// 1 stored, 2 refused.
    pub(crate) state: u8,
    pub(crate) size: u64,
    pub(crate) mtime_ms: u64,
    pub(crate) sha: [u8; 32],
    pub(crate) run: u32,
    pub(crate) reason: u32,
    pub(crate) at_ms: u64,
}

pub(crate) fn decode_file_row(b: &[u8]) -> Result<FileRow> {
    FileRow::decode(b)
}

const FILE_VERSION: u8 = 3;
const FILE_LEN: usize = 1 + 1 + 8 + 8 + 32 + 4 + 4 + 8;

impl FileRow {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FILE_LEN);
        out.push(FILE_VERSION);
        out.push(self.state);
        out.extend_from_slice(&self.size.to_be_bytes());
        out.extend_from_slice(&self.mtime_ms.to_be_bytes());
        out.extend_from_slice(&self.sha);
        out.extend_from_slice(&self.run.to_be_bytes());
        out.extend_from_slice(&self.reason.to_be_bytes());
        out.extend_from_slice(&self.at_ms.to_be_bytes());
        out
    }

    fn decode(b: &[u8]) -> Result<FileRow> {
        if b.first() != Some(&FILE_VERSION) || b.len() != FILE_LEN {
            fail!("unsupported", "run file value version or length");
        }
        Ok(FileRow {
            state: b[1],
            size: be_u64(b, 2),
            mtime_ms: be_u64(b, 10),
            sha: b[18..50].try_into().unwrap(),
            run: be_u32(b, 50),
            reason: be_u32(b, 54),
            at_ms: be_u64(b, 58),
        })
    }
}

const CODEC_RAW: u8 = 0;
const CODEC_ZSTD: u8 = 1;

/// A payload block: `u8 version, u8 codec, u16 count`, then the documents (each `u32 length`
/// and its bytes), zstd-compressed once the block is full or a later day starts.
fn block_encode(docs: &[Vec<u8>], compress: bool) -> Result<Vec<u8>> {
    let mut body = Vec::new();
    for doc in docs {
        body.extend_from_slice(&(doc.len() as u32).to_be_bytes());
        body.extend_from_slice(doc);
    }
    let mut out = vec![1u8, if compress { CODEC_ZSTD } else { CODEC_RAW }];
    out.extend_from_slice(&(docs.len() as u16).to_be_bytes());
    if compress {
        let packed = zstd::bulk::compress(&body, ZSTD_LEVEL)?;
        // Compression that grows a value is skipped (DATA-ARCHITECTURE section 6).
        if packed.len() < body.len() {
            out.extend_from_slice(&packed);
            return Ok(out);
        }
        out[1] = CODEC_RAW;
    }
    out.extend_from_slice(&body);
    Ok(out)
}

pub(crate) fn block_decode(b: &[u8]) -> Result<(bool, Vec<Vec<u8>>)> {
    if b.len() < 4 || b[0] != 1 {
        fail!("unsupported", "run payload block version");
    }
    let count = u16::from_be_bytes([b[2], b[3]]) as usize;
    let compressed = b[1] == CODEC_ZSTD;
    let body = if compressed {
        zstd::stream::decode_all(&b[4..])?
    } else {
        b[4..].to_vec()
    };
    let mut docs = Vec::with_capacity(count);
    let mut at = 0;
    for _ in 0..count {
        if at + 4 > body.len() {
            fail!("corrupt", "run payload block is short");
        }
        let len = be_u32(&body, at) as usize;
        at += 4;
        if at + len > body.len() {
            fail!("corrupt", "run payload block is short");
        }
        docs.push(body[at..at + len].to_vec());
        at += len;
    }
    Ok((compressed, docs))
}

/// Appends one original document to the open block of `day` and returns where it is.
fn append_doc(txn: &WriteTransaction, day: u32, doc: &[u8]) -> Result<Loc> {
    let lo = Key::new().u32(day).u32(0);
    let hi = Key::new().u32(day.saturating_add(1)).u32(0);
    let last: Option<(u32, Vec<u8>)> = {
        let table = txn.open_table(RUN_PAYLOAD)?;
        let mut range = table.range(lo.as_slice()..hi.as_slice())?;
        match range.next_back() {
            Some(item) => {
                let (k, v) = item?;
                Some((be_u32(k.value(), 4), v.value().to_vec()))
            }
            None => None,
        }
    };
    if let Some((block, value)) = &last {
        let (compressed, mut docs) = block_decode(value)?;
        if !compressed && docs.len() < BLOCK_DOCS as usize {
            docs.push(doc.to_vec());
            let slot = (docs.len() - 1) as u16;
            let full = docs.len() == BLOCK_DOCS as usize;
            put(
                txn,
                RUN_PAYLOAD,
                Key::new().u32(day).u32(*block).as_slice(),
                &block_encode(&docs, full)?,
            )?;
            return Ok(Loc {
                day,
                block: *block,
                slot,
            });
        }
    }
    let block = last.as_ref().map(|(b, _)| b + 1).unwrap_or(0);
    if block == 0 {
        // A new day: the previous day's open block will not grow again, so it is compressed.
        let earlier: Option<(Vec<u8>, Vec<u8>)> = {
            let table = txn.open_table(RUN_PAYLOAD)?;
            let mut range = table.range(..lo.as_slice())?;
            match range.next_back() {
                Some(item) => {
                    let (k, v) = item?;
                    Some((k.value().to_vec(), v.value().to_vec()))
                }
                None => None,
            }
        };
        if let Some((key, value)) = earlier {
            let (compressed, docs) = block_decode(&value)?;
            if !compressed && docs.len() > 1 {
                put(txn, RUN_PAYLOAD, &key, &block_encode(&docs, true)?)?;
            }
        }
    }
    put(
        txn,
        RUN_PAYLOAD,
        Key::new().u32(day).u32(block).as_slice(),
        &block_encode(&[doc.to_vec()], false)?,
    )?;
    Ok(Loc { day, block, slot: 0 })
}

// ---------------------------------------------------------------------------------------------
// Events

/// The ids of DATA-ARCHITECTURE section 2 that a run carries, as strings.
#[derive(Clone, Debug, Default, Deserialize, serde::Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct RunIds {
    pub run_id: String,
    pub writer: String,
    pub machine: String,
    pub computer: Option<String>,
    pub task_id: String,
    pub task_type: Option<String>,
    pub t3_thread_id: Option<String>,
    pub t3_thread_source: Option<String>,
    pub t3_task_id: Option<String>,
    pub project_id: Option<String>,
    pub stream_id: Option<String>,
    pub provider: Option<String>,
    pub account_raw: Option<String>,
    pub event_at_ms: u64,
    pub observed_at_ms: u64,
    pub started_at_ms: Option<u64>,
    pub wall_ms: Option<u64>,
    pub seq: u64,
    pub outcome: String,
    pub source: String,
    pub benchmark: Option<String>,
    pub review_verdict: Option<String>,
    pub overturned: bool,
    pub disputed: bool,
    pub imported: bool,
    pub unknown_model: bool,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityPayload {
    pub model_raw: Option<String>,
    pub model_id: Option<String>,
    pub model_source: String,
    pub model_family: Option<String>,
    pub effort_raw: Option<String>,
    pub effort: Option<String>,
    pub effort_source: String,
    pub effort_conflict: bool,
    pub harness_raw: Option<String>,
    pub harness: Option<String>,
    pub harness_source: String,
    pub model_catalog_hash: String,
}

impl From<&Identity> for IdentityPayload {
    fn from(i: &Identity) -> Self {
        IdentityPayload {
            model_raw: i.model_raw.clone(),
            model_id: i.model_id.clone(),
            model_source: i.model_source.clone(),
            model_family: i.model_family.clone(),
            effort_raw: i.effort_raw.clone(),
            effort: i.effort.clone(),
            effort_source: i.effort_source.clone(),
            effort_conflict: i.effort_conflict,
            harness_raw: i.harness_raw.clone(),
            harness: i.harness.clone(),
            harness_source: i.harness_source.clone(),
            model_catalog_hash: i.model_catalog_hash.clone(),
        }
    }
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RecordedPayload {
    event_id: String,
    path: String,
    size: u64,
    mtime_ms: u64,
    sha256: String,
    ingested_at_ms: u64,
    doc: String,
    ids: RunIds,
    identity: IdentityPayload,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct IdentityEventPayload {
    run_id: String,
    sha256: String,
    identity: IdentityPayload,
}

/// A refused file. No path, file name or writer is kept (any of them may be the very secret it
/// was refused for): only the path's hash, the reason and the time.
#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RefusedPayload {
    #[serde(default)]
    path_hash: Option<String>,
    /// Written by builds up to 6baa2f370 only: read to find its hash, never stored again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    /// Written by 7a32d39d0 only: read for compatibility, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    writer: Option<String>,
    size: u64,
    mtime_ms: u64,
    sha256: Option<String>,
    reason: String,
    ingested_at_ms: u64,
}

/// Content already recorded, seen at a greater path: the revision keeps that path as its
/// tie-break, without a second copy of the document.
#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PathPayload {
    run_id: String,
    sha256: String,
    path: String,
    size: u64,
    mtime_ms: u64,
}

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct CatalogPayload {
    catalog_hash: String,
    models: Option<String>,
    aliases: Option<String>,
    first_used_ms: u64,
}

fn event(kind: &str, at_ms: u64, key: String, payload: &impl serde::Serialize) -> Result<Event> {
    let mut e = Event::new(kind, at_ms, msgpack::from_json(&serde_json::to_value(payload)?)?);
    e.idempotency_key = Some(key);
    Ok(e)
}

// ---------------------------------------------------------------------------------------------
// Projector

pub struct RunsProjector;

impl Projector for RunsProjector {
    fn name(&self) -> &'static str {
        "runs"
    }
    fn version(&self) -> u32 {
        RUNS_PROJECTOR_VERSION
    }
    fn reads(&self, log: LogId) -> bool {
        log == LogId::Ledger
    }
    fn apply(&self, txn: &WriteTransaction, _log: LogId, event: &Event) -> Result<Vec<Finding>> {
        project_event(txn, event, true)?;
        Ok(Vec::new())
    }
    fn apply_replay(
        &self,
        txn: &WriteTransaction,
        _log: LogId,
        event: &Event,
        _readings: crate::engine::ReplayReadings,
    ) -> Result<Vec<Finding>> {
        // Never update or stamp Scoreboard aggregates during replay: `finish_replay` verifies
        // the full trust stamp against post-replay heads and rebuilds once on any mismatch.
        project_event(txn, event, false)?;
        Ok(Vec::new())
    }
    fn finish_replay(&self, db: &redb::Database) -> Result<()> {
        crate::scoreboard_agg::ensure_migrated(db)
    }
    fn reset(&self, txn: &WriteTransaction) -> Result<()> {
        for def in [RUN_HEAD, RUN_REV, RUN_PAYLOAD, RUN_FILE, RUN_CATALOG, RUN_DICT] {
            txn.delete_table(def)?;
        }
        txn.delete_table(RUN_DICT_REV)?;
        txn.delete_table(RUN_BY_TIME)?;
        txn.delete_table(RUN_META)?;
        crate::scoreboard_agg::delete_tables(txn)?;
        create_tables(txn)
    }
}

fn project_event(txn: &WriteTransaction, event: &Event, update_agg: bool) -> Result<()> {
    let payload = || msgpack::to_json(&event.payload);
    match event.event_type.as_str() {
        "run.catalog" => {
            project_catalog(txn, &serde_json::from_value(payload())?)?;
            if update_agg {
                crate::scoreboard_agg::on_catalog_changed();
            }
        }
        "run.recorded" => project_recorded(txn, &serde_json::from_value(payload())?, event.sequence_id, update_agg)?,
        "run.identity" => project_identity(txn, &serde_json::from_value(payload())?, update_agg)?,
        "run.refused" => project_refused(txn, &serde_json::from_value(payload())?, update_agg)?,
        "run.path" => project_path(txn, &serde_json::from_value(payload())?, update_agg)?,
        _ => {}
    }
    Ok(())
}

fn project_catalog(txn: &WriteTransaction, p: &CatalogPayload) -> Result<()> {
    let Some(hash) = unhex32(&p.catalog_hash) else {
        fail!("invalid", "catalog hash");
    };
    let key = &hash[..16];
    if get(txn, RUN_CATALOG, key)?.is_none() {
        let texts = msgpack::to_bytes(&json!({ "models": p.models, "aliases": p.aliases }))?;
        let mut value = vec![1u8];
        value.extend_from_slice(&p.first_used_ms.to_be_bytes());
        value.extend_from_slice(&zstd::bulk::compress(&texts, ZSTD_LEVEL)?);
        put(txn, RUN_CATALOG, key, &value)?;
    }
    meta_set(txn, "catalog", &p.catalog_hash)?;
    intern(txn, kind::CATALOG, Some(&p.catalog_hash))?;
    Ok(())
}

fn verdict_code(v: Option<&str>) -> u8 {
    match v {
        Some("pass") => 1,
        Some("fail") => 2,
        Some("cannot-verify") => 3,
        _ => 0,
    }
}

fn identity_ids(txn: &WriteTransaction, ids: &mut [u32; 26], i: &IdentityPayload) -> Result<()> {
    ids[at::MODEL_RAW] = intern(txn, kind::MODEL_RAW, i.model_raw.as_deref())?;
    ids[at::MODEL] = intern(txn, kind::MODEL, i.model_id.as_deref())?;
    ids[at::FAMILY] = intern(txn, kind::FAMILY, i.model_family.as_deref())?;
    ids[at::EFFORT_RAW] = intern(txn, kind::EFFORT_RAW, i.effort_raw.as_deref())?;
    ids[at::EFFORT] = intern(txn, kind::EFFORT, i.effort.as_deref())?;
    ids[at::HARNESS_RAW] = intern(txn, kind::HARNESS_RAW, i.harness_raw.as_deref())?;
    ids[at::HARNESS] = intern(txn, kind::HARNESS, i.harness.as_deref())?;
    ids[at::CATALOG] = intern(txn, kind::CATALOG, Some(&i.model_catalog_hash))?;
    ids[at::MODEL_SOURCE] = intern(txn, kind::WORD, Some(&i.model_source))?;
    ids[at::EFFORT_SOURCE] = intern(txn, kind::WORD, Some(&i.effort_source))?;
    ids[at::HARNESS_SOURCE] = intern(txn, kind::WORD, Some(&i.harness_source))?;
    Ok(())
}

/// Which copy of a run wins, as TypeScript's runs reader decides (`newer` in runs-reader.ts): the
/// highest seq, then the later writtenAt, then the later path.
fn newer(a: (u64, u64, &str), b: (u64, u64, &str)) -> bool {
    a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)) == std::cmp::Ordering::Greater
}

fn rev_key(r: u32, index: u16) -> Vec<u8> {
    Key::new().u32(r).bytes(&index.to_be_bytes()).0
}

/// Every revision of a run with its index, oldest first.
fn revisions(txn: &WriteTransaction, r: u32) -> Result<Vec<(u16, Head)>> {
    let table = txn.open_table(RUN_REV)?;
    let lo = Key::new().u32(r).bytes(&[0, 0]);
    let hi = Key::new().u32(r).bytes(&[0xff, 0xff]);
    let mut out = Vec::new();
    for item in table.range(lo.as_slice()..=hi.as_slice())? {
        let (k, v) = item?;
        let key = k.value();
        out.push((u16::from_be_bytes([key[4], key[5]]), Head::decode(v.value())?));
    }
    Ok(out)
}

fn text_of(txn: &WriteTransaction, kind: u8, id: u32) -> Result<Option<String>> {
    let table = txn.open_table(RUN_DICT)?;
    let mut dict = Dict::new(&table);
    dict.text(kind, id)
}

fn head_affects_agg(old: &Head, new: &Head) -> bool {
    old.sha != new.sha
        || old.ids != new.ids
        || old.ended_ms != new.ended_ms
        || old.flags != new.flags
        || old.verdict != new.verdict
        || old.seq != new.seq
        || old.written_ms != new.written_ms
        || old.ingested_ms != new.ingested_ms
        || old.loc != new.loc
}

/// Makes the head the winning revision (by `newer`) and keeps the time index on it.
/// Returns `Some((head, had_prior_head))` when the winning revision changed in a way that affects
/// Scoreboard aggregates (or was newly inserted).
fn refresh_head(txn: &WriteTransaction, r: u32) -> Result<Option<(Head, bool)>> {
    let revs = revisions(txn, r)?;
    if revs.is_empty() {
        return Ok(None);
    }
    let mut paths = Vec::with_capacity(revs.len());
    {
        let table = txn.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&table);
        for (_, row) in &revs {
            paths.push(dict.field(row, at::PATH)?.unwrap_or_default());
        }
    }
    let mut best = 0;
    for i in 1..revs.len() {
        let (a, b) = (&revs[i].1, &revs[best].1);
        if newer((a.seq, a.written_ms, &paths[i]), (b.seq, b.written_ms, &paths[best])) {
            best = i;
        }
    }
    let mut head = revs[best].1.clone();
    head.revisions = revs.len().min(u16::MAX as usize) as u16;
    let key = Key::new().u32(r).0;
    let held = get(txn, RUN_HEAD, &key)?.map(|b| Head::decode(&b)).transpose()?;
    if let Some(held) = &held {
        txn.open_table(RUN_BY_TIME)?
            .remove(Key::new().u64(held.ended_ms).u32(r).as_slice())?;
    }
    put(txn, RUN_HEAD, &key, &head.encode())?;
    txn.open_table(RUN_BY_TIME)?
        .insert(Key::new().u64(head.ended_ms).u32(r).as_slice(), ())?;
    let had_prior_head = held.is_some();
    let agg_changed = held.as_ref().is_none_or(|old| head_affects_agg(old, &head));
    Ok(agg_changed.then_some((head, had_prior_head)))
}

fn stored_file(
    txn: &WriteTransaction,
    path: &str,
    size: u64,
    mtime_ms: u64,
    sha: [u8; 32],
    r: u32,
    at_ms: u64,
) -> Result<()> {
    put(
        txn,
        RUN_FILE,
        &path_key(path),
        &FileRow {
            state: 1,
            size,
            mtime_ms,
            sha,
            run: r,
            reason: 0,
            at_ms,
        }
        .encode(),
    )
}

/// The same content at another path: a greater path becomes the revision's tie-break.
fn raise_path(txn: &WriteTransaction, r: u32, index: u16, mut row: Head, path: &str, update_agg: bool) -> Result<()> {
    let current = text_of(txn, kind::PATH, row.id(at::PATH))?.unwrap_or_default();
    if path > current.as_str() {
        row.ids[at::PATH] = intern(txn, kind::PATH, Some(path))?;
        put(txn, RUN_REV, &rev_key(r, index), &row.encode())?;
        if refresh_head(txn, r)?.is_some() && update_agg {
            crate::scoreboard_agg::mark_full_rebuild();
        }
    }
    Ok(())
}

fn project_recorded(txn: &WriteTransaction, p: &RecordedPayload, event_seq: u64, update_agg: bool) -> Result<()> {
    if serde_json::from_str::<Json>(&p.doc)?
        .get("corrects")
        .is_some_and(|v| !v.is_null())
    {
        meta_set(txn, "corrections", "1")?;
    }
    let Some(sha) = unhex32(&p.sha256) else {
        fail!("invalid", "run sha256");
    };
    let r = intern(txn, kind::RUN_ID, Some(&p.ids.run_id))?;
    let revs = revisions(txn, r)?;
    stored_file(txn, &p.path, p.size, p.mtime_ms, sha, r, p.ingested_at_ms)?;
    // Idempotent: a content already recorded under this run id is not a new revision.
    if let Some((index, row)) = revs.iter().find(|(_, row)| row.sha == sha) {
        return raise_path(txn, r, *index, row.clone(), &p.path, update_agg);
    }
    let loc = append_doc(txn, (p.ingested_at_ms / DAY_MS) as u32, p.doc.as_bytes())?;
    let ids = &p.ids;
    let mut flags = 0u8;
    for (on, bit) in [
        (ids.source == "benchmark", F_BENCHMARK),
        (ids.imported, F_IMPORTED),
        (ids.review_verdict.is_some(), F_REVIEW),
        (ids.overturned, F_OVERTURNED),
        (ids.disputed, F_DISPUTED),
        (ids.unknown_model, F_UNKNOWN_MODEL),
        (p.identity.effort_conflict, F_EFFORT_CONFLICT),
    ] {
        if on {
            flags |= bit;
        }
    }
    let mut row = Head {
        flags,
        verdict: verdict_code(ids.review_verdict.as_deref()),
        revisions: 0,
        seq: ids.seq,
        written_ms: ids.observed_at_ms,
        ingested_ms: p.ingested_at_ms,
        started_ms: ids.started_at_ms.unwrap_or(UNKNOWN),
        ended_ms: ids.event_at_ms,
        wall_ms: ids.wall_ms.unwrap_or(UNKNOWN),
        event_seq,
        sha,
        loc,
        ids: [0; 26],
    };
    row.ids[at::WRITER] = intern(txn, kind::WRITER, Some(&ids.writer))?;
    row.ids[at::MACHINE] = intern(txn, kind::MACHINE, Some(&ids.machine))?;
    row.ids[at::COMPUTER] = intern(txn, kind::COMPUTER, ids.computer.as_deref())?;
    row.ids[at::T3_THREAD] = intern(txn, kind::T3_THREAD, ids.t3_thread_id.as_deref())?;
    row.ids[at::T3_TASK] = intern(txn, kind::T3_TASK, ids.t3_task_id.as_deref())?;
    row.ids[at::TASK_ID] = intern(txn, kind::TASK_ID, Some(&ids.task_id))?;
    row.ids[at::TASK_TYPE] = intern(txn, kind::TASK_TYPE, ids.task_type.as_deref())?;
    row.ids[at::PROJECT] = intern(txn, kind::PROJECT, ids.project_id.as_deref())?;
    row.ids[at::STREAM] = intern(txn, kind::STREAM, ids.stream_id.as_deref())?;
    row.ids[at::OUTCOME] = intern(txn, kind::OUTCOME, Some(&ids.outcome))?;
    row.ids[at::BENCHMARK] = intern(txn, kind::BENCHMARK, ids.benchmark.as_deref())?;
    row.ids[at::PROVIDER] = intern(txn, kind::PROVIDER, ids.provider.as_deref())?;
    row.ids[at::ACCOUNT] = intern(txn, kind::ACCOUNT, ids.account_raw.as_deref())?;
    row.ids[at::PATH] = intern(txn, kind::PATH, Some(&p.path))?;
    row.ids[at::T3_THREAD_SOURCE] = intern(txn, kind::WORD, ids.t3_thread_source.as_deref())?;
    identity_ids(txn, &mut row.ids, &p.identity)?;
    let index = revs.len().min(u16::MAX as usize) as u16;
    put(txn, RUN_REV, &rev_key(r, index), &row.encode())?;
    if let Some((head, had_prior_head)) = refresh_head(txn, r)?
        && update_agg
    {
        let doc_bytes = (head.sha == sha).then_some(p.doc.as_bytes());
        crate::scoreboard_agg::on_head_recorded(txn, r, &head, had_prior_head, doc_bytes)?;
    }
    Ok(())
}

fn project_path(txn: &WriteTransaction, p: &PathPayload, update_agg: bool) -> Result<()> {
    let Some(sha) = unhex32(&p.sha256) else {
        fail!("invalid", "run sha256");
    };
    let r = {
        let rev = txn.open_table(RUN_DICT_REV)?;
        lookup_id(&rev, kind::RUN_ID, &p.run_id)?
    };
    let Some(r) = r else { return Ok(()) };
    let revs = revisions(txn, r)?;
    let Some((index, row)) = revs.into_iter().find(|(_, row)| row.sha == sha) else {
        return Ok(());
    };
    stored_file(txn, &p.path, p.size, p.mtime_ms, sha, r, row.ingested_ms)?;
    raise_path(txn, r, index, row, &p.path, update_agg)
}

fn project_identity(txn: &WriteTransaction, p: &IdentityEventPayload, update_agg: bool) -> Result<()> {
    let Some(sha) = unhex32(&p.sha256) else {
        fail!("invalid", "run sha256");
    };
    let r = {
        let rev = txn.open_table(RUN_DICT_REV)?;
        lookup_id(&rev, kind::RUN_ID, &p.run_id)?
    };
    let Some(r) = r else { return Ok(()) };
    // A re-keying names the revision it was computed for; the head follows the winner.
    for (index, mut row) in revisions(txn, r)? {
        if row.sha != sha {
            continue;
        }
        identity_ids(txn, &mut row.ids, &p.identity)?;
        if p.identity.effort_conflict {
            row.flags |= F_EFFORT_CONFLICT;
        } else {
            row.flags &= !F_EFFORT_CONFLICT;
        }
        put(txn, RUN_REV, &rev_key(r, index), &row.encode())?;
    }
    if refresh_head(txn, r)?.is_some() && update_agg {
        crate::scoreboard_agg::mark_full_rebuild();
    }
    Ok(())
}

/// The reason words kept when a reason itself looks like a credential (it may quote a folder).
const WITHHELD_REASON: &str = "refused (the reason named what looks like a credential)";

/// Whether the shared secret policy refuses a plain text: a path, a folder, a reason, a value.
fn secret_text(text: &str) -> bool {
    find_secret(text).is_some()
}

/// Whether the shared secret policy refuses a JSON text: its source and every decoded key and
/// string, as TypeScript's `jsonFromBytes`. Text that does not parse is checked as plain text.
fn secret_json(text: &str) -> bool {
    match serde_json::from_str::<Json>(text) {
        Ok(value) => find_secret_in(text, &value).is_some(),
        Err(_) => secret_text(text),
    }
}

fn project_refused(txn: &WriteTransaction, p: &RefusedPayload, update_agg: bool) -> Result<()> {
    let key: [u8; 16] = match (&p.path_hash, &p.path) {
        (Some(hash), _) => match unhex16(hash) {
            Some(k) => k,
            None => fail!("invalid", "refused path hash"),
        },
        (None, Some(legacy)) => path_key(legacy),
        (None, None) => return Ok(()),
    };
    let reason = if secret_text(&p.reason) {
        WITHHELD_REASON
    } else {
        p.reason.as_str()
    };
    let row = FileRow {
        state: 2,
        size: p.size,
        mtime_ms: p.mtime_ms,
        sha: p.sha256.as_deref().and_then(unhex32).unwrap_or([0; 32]),
        run: 0,
        reason: intern(txn, kind::REASON, Some(reason))?,
        at_ms: p.ingested_at_ms,
    };
    put(txn, RUN_FILE, &key, &row.encode())?;
    if update_agg {
        crate::scoreboard_agg::on_refused_recorded(txn, &key)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Computers (the package's registry; never a caller's alias)

static COMPUTERS: LazyLock<Json> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../web-dashboard/public/computers.json"))
        .expect("packaged computer registry")
});

/// Machine ids a writer name may start with.
pub fn machine_ids() -> Vec<String> {
    COMPUTERS
        .as_object()
        .map(|m| {
            m.values()
                .filter_map(|r| r["machineId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The `DashboardHost` key of a Status machine id (`vm` is `ubuntu`).
pub fn computer_of(machine: &str) -> Option<String> {
    COMPUTERS
        .as_object()?
        .iter()
        .find(|(_, row)| row["machineId"].as_str() == Some(machine))
        .map(|(key, _)| key.clone())
}

// ---------------------------------------------------------------------------------------------
// Scan (read-only: lists the share and reads new or changed files)

/// Folder mtimes seen by earlier passes: an unchanged folder is not listed again until the
/// next full pass.
static FOLDERS: LazyLock<Mutex<HashMap<PathBuf, u64>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanArgs {
    pub root: String,
    pub budget: Option<usize>,
    pub full: Option<bool>,
}

fn mtime_ms(meta: &fs::Metadata) -> u64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn is_date_folder(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 10
        && b[4] == b'-'
        && b[7] == b'-'
        && b.iter()
            .enumerate()
            .all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

fn read_capped(path: &Path, cap: u64) -> std::result::Result<String, &'static str> {
    let meta = fs::metadata(path).map_err(|_| "could not be read")?;
    if meta.len() > cap {
        return Err("too large");
    }
    let bytes = fs::read(path).map_err(|_| "could not be read")?;
    if bytes.contains(&0) {
        return Err("contains NUL bytes from a damaged write");
    }
    String::from_utf8(bytes).map_err(|_| "not UTF-8 text")
}

/// Lists `<root>/v1/writers/*/runs/<day>/*.json` and reads every new or changed file, within the
/// budget (newest days first; the next pass continues). Writes nothing.
pub fn scan(engine: &Engine, args: &ScanArgs) -> Result<Json> {
    let root = PathBuf::from(&args.root).join("v1");
    let writers_dir = root.join("writers");
    let budget = args.budget.unwrap_or(SCAN_BUDGET).max(1);
    let full = args.full.unwrap_or(false);
    let machines_owned = machine_ids();
    let machines: Vec<&str> = machines_owned.iter().map(String::as_str).collect();
    let mut problems: Vec<Json> = Vec::new();
    let entries = match fs::read_dir(&writers_dir) {
        Ok(e) => e,
        Err(e) => {
            let state = if e.kind() == std::io::ErrorKind::NotFound {
                "missing"
            } else {
                "error"
            };
            return Ok(
                json!({ "state": state, "files": [], "refused": [], "threads": {}, "catalog": catalog_texts(&root), "listed": 0, "unchanged": 0, "more": false, "problems": [] }),
            );
        }
    };
    let mut writers: Vec<String> = entries
        .flatten()
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| !n.starts_with('.') && run_schema::is_writer_name(n, &machines))
        .collect();
    writers.sort();
    // (day, rel path, absolute path, size, mtime)
    let mut candidates: Vec<(String, String, PathBuf, u64, u64)> = Vec::new();
    let mut folders = FOLDERS.lock().unwrap_or_else(|p| p.into_inner());
    if full {
        folders.clear();
    }
    let (mut listed_folders, mut skipped_folders) = (0usize, 0usize);
    for writer in &writers {
        let runs_dir = writers_dir.join(writer).join("runs");
        // A writer that never wrote a run has no `runs` folder: that is ordinary absence. Any
        // other failure hides every date folder under it, so name the folder this pass; the owner
        // keeps the first time it saw it and the composer marks the verdicts that used a retained
        // review from it (SB3, 2026-11-23 A).
        let days = match fs::read_dir(&runs_dir) {
            Ok(days) => days,
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    problems.push(
                        json!({ "file": format!("writers/{writer}/runs"), "reason": "folder could not be listed" }),
                    );
                }
                continue;
            }
        };
        for day in days.flatten() {
            let name = day.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || !is_date_folder(&name) {
                continue;
            }
            let dir = day.path();
            let Ok(meta) = fs::metadata(&dir) else { continue };
            if !meta.is_dir() {
                continue;
            }
            let mtime = mtime_ms(&meta);
            if folders.get(&dir) == Some(&mtime) {
                skipped_folders += 1;
                continue;
            }
            let files = match fs::read_dir(&dir) {
                Ok(f) => f,
                Err(_) => {
                    problems.push(json!({ "file": format!("writers/{writer}/runs/{name}"), "reason": "folder could not be listed" }));
                    continue;
                }
            };
            listed_folders += 1;
            folders.insert(dir.clone(), mtime);
            for file in files.flatten() {
                let file_name = file.file_name().to_string_lossy().into_owned();
                if file_name.starts_with('.')
                    || !file_name.ends_with(".json")
                    || !run_schema::run_file_name_readable(&file_name)
                {
                    continue;
                }
                let Ok(meta) = file.metadata() else { continue };
                if !meta.is_file() {
                    continue;
                }
                candidates.push((
                    name.clone(),
                    format!("writers/{writer}/runs/{name}/{file_name}"),
                    file.path(),
                    meta.len(),
                    mtime_ms(&meta),
                ));
            }
        }
    }
    let listed = candidates.len();
    // Unchanged files (same size and mtime as stored) are not read again.
    let known: HashMap<[u8; 16], FileRow> = engine.with_db(|db| {
        let read = db.begin_read()?;
        let table = read.open_table(RUN_FILE)?;
        let mut out = HashMap::new();
        for c in &candidates {
            let key = path_key(&c.1);
            if let Some(v) = table.get(key.as_slice())? {
                out.insert(key, FileRow::decode(v.value())?);
            }
        }
        Ok(out)
    })?;
    candidates.retain(|c| {
        known
            .get(&path_key(&c.1))
            .is_none_or(|row| row.size != c.3 || row.mtime_ms != c.4)
    });
    let unchanged = listed - candidates.len();
    candidates.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let more = candidates.len() > budget;
    if more {
        // The cut folders are listed again next pass.
        for c in &candidates[budget..] {
            if let Some(parent) = c.2.parent() {
                folders.remove(parent);
            }
        }
        candidates.truncate(budget);
    }
    drop(folders);
    let mut files = Vec::new();
    let mut refused = Vec::new();
    let mut touched: HashSet<String> = HashSet::new();
    for (_, rel, abs, size, mtime) in &candidates {
        match read_capped(abs, RUN_CAP_BYTES as u64) {
            Ok(text) => {
                touched.insert(rel.split('/').nth(1).unwrap_or("").to_string());
                files.push(json!({ "path": rel, "text": text, "size": size, "mtimeMs": mtime }));
            }
            Err(reason) => {
                let reason = if reason == "too large" {
                    format!("larger than the {} KB cap", RUN_CAP_BYTES / 1024)
                } else {
                    reason.to_string()
                };
                refused.push(json!({ "path": rel, "size": size, "mtimeMs": mtime, "reason": reason }));
            }
        }
    }
    let mut threads = serde_json::Map::new();
    for writer in touched {
        if let Ok(text) = read_capped(&writers_dir.join(&writer).join("thread.json"), THREAD_CAP_BYTES) {
            threads.insert(writer, Json::String(text));
        }
    }
    Ok(json!({
        "state": "ok",
        "files": files,
        "refused": refused,
        "threads": threads,
        "catalog": catalog_texts(&root),
        "writers": writers.len(),
        "listed": listed,
        "unchanged": unchanged,
        "folders": { "listed": listed_folders, "skipped": skipped_folders },
        "more": more,
        "problems": problems,
    }))
}

fn catalog_texts(root: &Path) -> Json {
    let read = |name: &str| read_capped(&root.join("catalog").join(name), CATALOG_CAP_BYTES).ok();
    json!({ "models": read("models.json"), "aliases": read("model-aliases.json") })
}

// ---------------------------------------------------------------------------------------------
// Record

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIn {
    pub path: String,
    pub text: String,
    pub size: u64,
    pub mtime_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusedIn {
    pub path: String,
    pub size: u64,
    pub mtime_ms: u64,
    pub reason: String,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Deserialize, Default)]
pub struct CatalogIn {
    pub models: Option<String>,
    pub aliases: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordArgs {
    #[serde(default)]
    pub files: Vec<FileIn>,
    #[serde(default)]
    pub refused: Vec<RefusedIn>,
    /// Writer name to the text of its `thread.json` (through the secret filter; checked again).
    #[serde(default)]
    pub threads: BTreeMap<String, String>,
    #[serde(default)]
    pub catalog: CatalogIn,
    pub now_ms: u64,
}

/// The catalog stored as current in an existing read transaction, if any.
fn current_catalog_in_read(read: &redb::ReadTransaction) -> Result<Option<Catalog>> {
    let meta = read.open_table(RUN_META)?;
    let Some(hash) = meta
        .get("catalog")?
        .map(|v| String::from_utf8_lossy(v.value()).into_owned())
    else {
        return Ok(None);
    };
    let Some(key) = unhex32(&hash) else { return Ok(None) };
    let table = read.open_table(RUN_CATALOG)?;
    let Some(value) = table.get(&key[..16])? else {
        return Ok(None);
    };
    let bytes = value.value();
    if bytes.first() != Some(&1) || bytes.len() < 9 {
        return Ok(None);
    }
    let texts: Json = msgpack::from_bytes(&zstd::stream::decode_all(&bytes[9..])?)?;
    Ok(Catalog::from_texts(texts["models"].as_str(), texts["aliases"].as_str()))
}

/// The catalog stored as current, if any.
fn current_catalog(engine: &Engine) -> Result<Option<Catalog>> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        current_catalog_in_read(&read)
    })
}

/// The catalog to normalize with: each file as given when it parses, else its last stored
/// version, so a damaged write never re-keys history.
fn choose_catalog(engine: &Engine, given: &CatalogIn) -> Result<Catalog> {
    let stored = current_catalog(engine)?;
    // A catalog text is stored with its version, so the secret policy covers it too.
    let models_ok = given
        .models
        .as_deref()
        .is_some_and(|t| !secret_json(t) && run_identity::parse_models(t).is_some());
    let aliases_ok = given
        .aliases
        .as_deref()
        .is_some_and(|t| !secret_json(t) && run_identity::parse_aliases(t).is_some());
    let models = if models_ok {
        given.models.clone()
    } else {
        stored.as_ref().and_then(|c| c.models_text.clone())
    };
    let aliases = if aliases_ok {
        given.aliases.clone()
    } else {
        stored.as_ref().and_then(|c| c.aliases_text.clone())
    };
    Ok(Catalog::from_texts(models.as_deref(), aliases.as_deref()).unwrap_or_default())
}

/// What a writer's `thread.json` says about where it runs.
#[derive(Default, Clone)]
struct ThreadFacts {
    t3_thread: Option<String>,
    machine: Option<String>,
}

/// The facts of one writer's `thread.json`, under the same secret policy as a run document: a
/// writer folder, document or value the policy refuses gives no facts at all.
fn thread_facts(text: &str, writer: &str) -> ThreadFacts {
    if secret_text(writer) {
        return ThreadFacts::default();
    }
    let Ok(doc) = serde_json::from_str::<Json>(text) else {
        return ThreadFacts::default();
    };
    if find_secret_in(text, &doc).is_some() {
        return ThreadFacts::default();
    }
    if doc["schema"].as_str() != Some("agent-reports/thread@1") || doc["writer"].as_str() != Some(writer) {
        return ThreadFacts::default();
    }
    let clean = |v: &Json| {
        v.as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control) && !secret_text(s))
            .map(str::to_string)
    };
    ThreadFacts {
        t3_thread: clean(&doc["agent"]["t3Thread"]),
        machine: clean(&doc["agent"]["machine"]),
    }
}

fn opt_str(v: &Json) -> Option<String> {
    v.as_str().map(str::to_string)
}

fn ms(v: &Json) -> Option<u64> {
    v.as_str().and_then(run_schema::iso_ms).map(|m| m.max(0) as u64)
}

/// The ids of one projected run.
fn run_ids(run: &Json, thread: &ThreadFacts) -> RunIds {
    let writer = run["writer"].as_str().unwrap_or("").to_string();
    let prefix = writer.split('.').next().unwrap_or("").to_string();
    // The writer's own machine wins; thread.json may name it too (when it names a known one).
    let machine = thread
        .machine
        .clone()
        .filter(|m| computer_of(m).is_some() && *m == prefix)
        .unwrap_or(prefix);
    let task_id = run["task"]["id"].as_str().unwrap_or("").to_string();
    let review = &run["review"];
    RunIds {
        run_id: run["id"].as_str().unwrap_or("").to_string(),
        computer: computer_of(&machine),
        machine,
        writer,
        t3_task_id: task_id.starts_with("node:delegated-task:").then(|| task_id.clone()),
        task_id,
        task_type: opt_str(&run["task"]["type"]),
        t3_thread_source: thread.t3_thread.as_ref().map(|_| "writer_thread_doc".to_string()),
        t3_thread_id: thread.t3_thread.clone(),
        project_id: opt_str(&run["project"]),
        stream_id: opt_str(&run["stream"]),
        provider: opt_str(&run["model"]["provider"]),
        account_raw: opt_str(&run["model"]["account"]),
        event_at_ms: ms(&run["endedAt"]).unwrap_or(0),
        observed_at_ms: ms(&run["writtenAt"]).unwrap_or(0),
        started_at_ms: ms(&run["startedAt"]),
        wall_ms: run["wallSeconds"]
            .as_f64()
            .map(|s| (s * 1000.0).round().max(0.0) as u64),
        seq: run["seq"].as_u64().unwrap_or(0),
        outcome: run["outcome"].as_str().unwrap_or("").to_string(),
        source: run["source"].as_str().unwrap_or("observed").to_string(),
        benchmark: opt_str(&run["benchmark"]),
        review_verdict: opt_str(&review["verdict"]),
        overturned: review["overturned"].as_bool().unwrap_or(false),
        disputed: review["disputed"].as_bool().unwrap_or(false),
        imported: !run["imported"].is_null(),
        unknown_model: run["unknownModel"].as_bool().unwrap_or(false),
    }
}

fn written_of(run: &Json) -> Written {
    Written {
        model: opt_str(&run["model"]["id"]),
        effort: opt_str(&run["model"]["effort"]),
        harness: opt_str(&run["model"]["harness"]),
    }
}

fn writer_of_path(path: &str) -> Option<&str> {
    let mut parts = path.split('/');
    (parts.next() == Some("writers")).then(|| parts.next()).flatten()
}

/// Records what a scan read (after the TypeScript secret filter, and through the same shared
/// policy again here): validates, normalizes and appends `run.*` events; a file already recorded
/// with the same content adds nothing.
pub fn record(engine: &Engine, args: RecordArgs) -> Result<Json> {
    let catalog = choose_catalog(engine, &args.catalog)?;
    let current = engine.with_db(|db| {
        let read = db.begin_read()?;
        let meta = read.open_table(RUN_META)?;
        Ok(meta
            .get("catalog")?
            .map(|v| String::from_utf8_lossy(v.value()).into_owned()))
    })?;
    let catalog_changed = current.as_deref() != Some(catalog.hash.as_str());
    let mut events: Vec<Event> = Vec::new();
    if catalog_changed {
        events.push(event(
            "run.catalog",
            args.now_ms,
            format!("run.catalog:{}", catalog.hash),
            &CatalogPayload {
                catalog_hash: catalog.hash.clone(),
                models: catalog.models_text.clone(),
                aliases: catalog.aliases_text.clone(),
                first_used_ms: args.now_ms,
            },
        )?);
    }
    // The greatest path each (run id, sha) is known at: on record, or seen earlier in this call.
    let mut known_paths: HashMap<(String, [u8; 32]), String> = HashMap::new();
    let stored_path = |run_id: &str, sha: &[u8; 32]| -> Result<Option<String>> {
        engine.with_db(|db| {
            let read = db.begin_read()?;
            let rev = read.open_table(RUN_DICT_REV)?;
            let Some(r) = lookup_id(&rev, kind::RUN_ID, run_id)? else {
                return Ok(None);
            };
            let dict_table = read.open_table(RUN_DICT)?;
            let mut dict = Dict::new(&dict_table);
            let table = read.open_table(RUN_REV)?;
            let lo = Key::new().u32(r).bytes(&[0, 0]);
            let hi = Key::new().u32(r).bytes(&[0xff, 0xff]);
            for item in table.range(lo.as_slice()..=hi.as_slice())? {
                let row = Head::decode(item?.1.value())?;
                if row.sha == *sha {
                    return Ok(Some(dict.field(&row, at::PATH)?.unwrap_or_default()));
                }
            }
            Ok(None)
        })
    };
    let file_state = |path: &str| -> Result<Option<FileRow>> {
        engine.with_db(|db| {
            let read = db.begin_read()?;
            let table = read.open_table(RUN_FILE)?;
            let value = table.get(path_key(path).as_slice())?;
            value.map(|v| FileRow::decode(v.value())).transpose()
        })
    };
    let threads: HashMap<String, ThreadFacts> = args
        .threads
        .iter()
        .map(|(w, text)| (w.clone(), thread_facts(text, w)))
        .collect();
    let (mut recorded, mut known, mut refused) = (0usize, 0usize, 0usize);
    // (path, size, mtime, sha, state, run_id): a stat/file-state refresh of content already on record.
    let mut touch: Vec<(String, u64, u64, [u8; 32], u8, Option<String>)> = Vec::new();
    // A refused file is recorded by its path's hash: its path, name and writer are never kept,
    // nor put in the event's idempotency key, and a reason that quotes the writer folder (the id
    // rule does) says "<writer>" instead.
    let refuse =
        |events: &mut Vec<Event>, path: &str, size: u64, mtime: u64, sha: Option<String>, reason: &str| -> Result<()> {
            let path_hash = hex(&path_key(path));
            let reason = match writer_of_path(path) {
                Some(writer) if !writer.is_empty() => reason.replace(writer, "<writer>"),
                _ => reason.to_string(),
            };
            let reason = if secret_text(&reason) {
                WITHHELD_REASON.to_string()
            } else {
                reason
            };
            let key = format!("run.refused:{path_hash}:{}", sha.as_deref().unwrap_or("-"));
            events.push(event(
                "run.refused",
                args.now_ms,
                key,
                &RefusedPayload {
                    path_hash: Some(path_hash),
                    path: None,
                    writer: None,
                    size,
                    mtime_ms: mtime,
                    sha256: sha,
                    reason: reason.to_string(),
                    ingested_at_ms: args.now_ms,
                },
            )?);
            Ok(())
        };
    for r in &args.refused {
        // A refusal already on record for this exact content is not recorded again.
        let same = file_state(&r.path)?
            .is_some_and(|row| row.state == 2 && r.sha256.as_deref().and_then(unhex32).is_none_or(|s| s == row.sha));
        if same {
            let sha = r.sha256.as_deref().and_then(unhex32).unwrap_or([0; 32]);
            touch.push((r.path.clone(), r.size, r.mtime_ms, sha, 2, None));
            known += 1;
            continue;
        }
        refuse(&mut events, &r.path, r.size, r.mtime_ms, r.sha256.clone(), &r.reason)?;
        refused += 1;
    }
    for f in &args.files {
        let sha = sha256(f.text.as_bytes());
        let sha_hex = hex(&sha);
        // A file's name or its writer folder can hold a credential too: such a file is refused
        // and its name never kept.
        if secret_text(&f.path) {
            if !file_state(&f.path)?.is_some_and(|row| row.state == 2 && row.sha == sha) {
                refuse(
                    &mut events,
                    &f.path,
                    f.size,
                    f.mtime_ms,
                    Some(sha_hex),
                    "refused: its file name looks like a credential",
                )?;
                refused += 1;
            } else {
                touch.push((f.path.clone(), f.size, f.mtime_ms, sha, 2, None));
                known += 1;
            }
            continue;
        }
        let Some(writer) = writer_of_path(&f.path) else {
            refuse(
                &mut events,
                &f.path,
                f.size,
                f.mtime_ms,
                Some(sha_hex),
                "is not under a writer's runs folder",
            )?;
            refused += 1;
            continue;
        };
        let projected = match run_schema::parse_run_text(&f.text, writer) {
            Ok(run) => run,
            Err(reason) => {
                let same = file_state(&f.path)?.is_some_and(|row| row.state == 2 && row.sha == sha);
                if same {
                    touch.push((f.path.clone(), f.size, f.mtime_ms, sha, 2, None));
                    known += 1;
                } else {
                    refuse(&mut events, &f.path, f.size, f.mtime_ms, Some(sha_hex), &reason.0)?;
                    refused += 1;
                }
                continue;
            }
        };
        // The shared policy again over the document as stored (docs/store.md 2.2): its source and
        // every decoded key and string, as the TypeScript filter checked it.
        if secret_json(&f.text) {
            refuse(
                &mut events,
                &f.path,
                f.size,
                f.mtime_ms,
                Some(sha_hex),
                "refused: it contains what looks like a credential",
            )?;
            refused += 1;
            continue;
        }
        let run_id = projected["id"].as_str().unwrap_or("").to_string();
        let key = (run_id.clone(), sha);
        let held = match known_paths.get(&key) {
            Some(path) => Some(path.clone()),
            None => stored_path(&run_id, &sha)?,
        };
        if let Some(greatest) = held {
            // The same content again. A greater path is the winner's tie-break (as TypeScript's
            // reader), so it is recorded; any other path only refreshes the scan's skip row.
            if f.path > greatest {
                events.push(event(
                    "run.path",
                    args.now_ms,
                    format!("run.path:{run_id}:{sha_hex}:{}", hex(&path_key(&f.path))),
                    &PathPayload {
                        run_id: run_id.clone(),
                        sha256: sha_hex,
                        path: f.path.clone(),
                        size: f.size,
                        mtime_ms: f.mtime_ms,
                    },
                )?);
                known_paths.insert(key, f.path.clone());
            } else {
                known_paths.insert(key, greatest);
                touch.push((f.path.clone(), f.size, f.mtime_ms, sha, 1, Some(run_id)));
            }
            known += 1;
            continue;
        }
        let ids = run_ids(&projected, threads.get(writer).unwrap_or(&ThreadFacts::default()));
        let identity: IdentityPayload = (&run_identity::identify(&catalog, &written_of(&projected))).into();
        // A last guard on what the run is indexed by: its ids and identity come from the document,
        // the thread facts and the catalog, each checked above; nothing reaches an index unchecked.
        if find_secret_in("", &serde_json::to_value(&ids)?).is_some()
            || find_secret_in("", &serde_json::to_value(&identity)?).is_some()
        {
            refuse(
                &mut events,
                &f.path,
                f.size,
                f.mtime_ms,
                Some(sha_hex),
                "refused: its ids hold what looks like a credential",
            )?;
            refused += 1;
            continue;
        }
        known_paths.insert(key, f.path.clone());
        events.push(event(
            "run.recorded",
            args.now_ms,
            format!("agent-report:run:{run_id}:{sha_hex}"),
            &RecordedPayload {
                event_id: format!("agent-report:run:{run_id}:{sha_hex}"),
                path: f.path.clone(),
                size: f.size,
                mtime_ms: f.mtime_ms,
                sha256: sha_hex,
                ingested_at_ms: args.now_ms,
                doc: f.text.clone(),
                ids,
                identity,
            },
        )?);
        recorded += 1;
    }
    for chunk in events.chunks(EVENTS_PER_COMMIT) {
        engine.commit(LogId::Ledger, |batch| {
            for e in chunk {
                batch.push(e.clone())?;
            }
            Ok(())
        })?;
    }
    // Same content seen under a new stat: the skip row follows the file (a cache, not history).
    if !touch.is_empty() {
        engine.with_db(|db| {
            let txn = crate::engine::begin(db)?;
            let mut resolved_runs: HashMap<String, u32> = HashMap::new();
            {
                let rev_tbl = txn.open_table(RUN_DICT_REV)?;
                for (_, _, _, _, _, run_id_opt) in &touch {
                    if let Some(run_id) = run_id_opt
                        && !resolved_runs.contains_key(run_id)
                        && let Some(r) = lookup_id(&rev_tbl, kind::RUN_ID, run_id)?
                    {
                        resolved_runs.insert(run_id.clone(), r);
                    }
                }
            }
            let mut any_state_changed = false;
            for (path, size, mtime, sha, state, run_id_opt) in &touch {
                let touched_run = run_id_opt
                    .as_deref()
                    .and_then(|id| resolved_runs.get(id).copied())
                    .unwrap_or(0);
                let row = match get(&txn, RUN_FILE, &path_key(path))? {
                    Some(bytes) => {
                        let mut row = FileRow {
                            size: *size,
                            mtime_ms: *mtime,
                            ..FileRow::decode(&bytes)?
                        };
                        // Valid bytes read again (an exact-byte repair or reverting a path back to
                        // already-recorded bytes of an earlier run): refresh state, sha, and run
                        // pointer and clear any active refusal. A refused stat refresh keeps its refusal.
                        if *state == 1 {
                            let changed = row.state != 1
                                || row.sha != *sha
                                || (touched_run != 0 && row.run != touched_run)
                                || row.reason != 0;
                            row.state = 1;
                            row.sha = *sha;
                            if touched_run != 0 {
                                row.run = touched_run;
                            }
                            row.reason = 0;
                            if changed {
                                row.at_ms = args.now_ms;
                                any_state_changed = true;
                            }
                        }
                        row
                    }
                    // The same content under another path: remember the path's stat so it is not
                    // read on every pass (keyed by its hash; no path text).
                    None => {
                        any_state_changed = true;
                        FileRow {
                            state: *state,
                            size: *size,
                            mtime_ms: *mtime,
                            sha: *sha,
                            run: if *state == 1 { touched_run } else { 0 },
                            reason: 0,
                            at_ms: args.now_ms,
                        }
                    }
                };
                put(&txn, RUN_FILE, &path_key(path), &row.encode())?;
            }
            let next_file_rev = if any_state_changed {
                Some(bump_file_rev_tx(&txn)?)
            } else {
                None
            };
            txn.commit()?;
            if let Some(rev) = next_file_rev {
                engine.note_runs_file_rev(rev);
            }
            Ok(())
        })?;
    }
    // Re-keying finishes once per catalog version, also after a crash part way through it.
    let rekeyed_for = engine.with_db(|db| {
        let read = db.begin_read()?;
        let meta = read.open_table(RUN_META)?;
        Ok(meta
            .get("rekeyed")?
            .map(|v| String::from_utf8_lossy(v.value()).into_owned()))
    })?;
    let rekeyed = if rekeyed_for.as_deref() != Some(catalog.hash.as_str()) {
        let n = rekey(engine, &catalog, args.now_ms)?;
        engine.with_db(|db| {
            let txn = crate::engine::begin(db)?;
            meta_set(&txn, "rekeyed", &catalog.hash)?;
            txn.commit()?;
            Ok(())
        })?;
        n
    } else {
        0
    };
    Ok(json!({
        "recorded": recorded,
        "known": known,
        "refused": refused,
        "rekeyed": rekeyed,
        "catalogHash": catalog.hash,
        "catalogChanged": catalog_changed,
        "generation": generation(engine)?,
    }))
}

/// Re-normalizes every revision under a new catalog and records a `run.identity` for each whose
/// identity changed; earlier identities stay in the ledger.
fn rekey(engine: &Engine, catalog: &Catalog, now: u64) -> Result<usize> {
    let mut changed: Vec<(String, [u8; 32], Identity)> = Vec::new();
    engine.with_db(|db| {
        let read = db.begin_read()?;
        // Every revision, not only the current ones: a later winner change must not bring back
        // an identity from an older catalog.
        let rows = read.open_table(RUN_REV)?;
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let mut memo: HashMap<(u32, u32, u32), Identity> = HashMap::new();
        for item in rows.iter()? {
            let (k, v) = item?;
            let r = be_u32(k.value(), 0);
            let head = &Head::decode(v.value())?;
            let triple = (
                head.id(at::MODEL_RAW),
                head.id(at::EFFORT_RAW),
                head.id(at::HARNESS_RAW),
            );
            let identity = match memo.get(&triple) {
                Some(i) => i.clone(),
                None => {
                    let written = Written {
                        model: dict.text(kind::MODEL_RAW, triple.0)?,
                        effort: dict.text(kind::EFFORT_RAW, triple.1)?,
                        harness: dict.text(kind::HARNESS_RAW, triple.2)?,
                    };
                    let i = run_identity::identify(catalog, &written);
                    memo.insert(triple, i.clone());
                    i
                }
            };
            let differs = dict.field(head, at::MODEL)? != identity.model_id
                || dict.field(head, at::EFFORT)? != identity.effort
                || dict.field(head, at::HARNESS)? != identity.harness
                || dict.field(head, at::FAMILY)? != identity.model_family
                || dict.field(head, at::MODEL_SOURCE)?.as_deref() != Some(identity.model_source.as_str())
                || dict.field(head, at::EFFORT_SOURCE)?.as_deref() != Some(identity.effort_source.as_str())
                || dict.field(head, at::HARNESS_SOURCE)?.as_deref() != Some(identity.harness_source.as_str());
            if differs {
                let run_id = dict.text(kind::RUN_ID, r)?.unwrap_or_default();
                changed.push((run_id, head.sha, identity));
            }
        }
        Ok(())
    })?;
    let count = changed.len();
    for chunk in changed.chunks(EVENTS_PER_COMMIT) {
        engine.commit(LogId::Ledger, |batch| {
            for (run_id, sha, identity) in chunk {
                let sha_hex = hex(sha);
                batch.push(event(
                    "run.identity",
                    now,
                    format!("run.identity:{run_id}:{sha_hex}:{}", identity.model_catalog_hash),
                    &IdentityEventPayload {
                        run_id: run_id.clone(),
                        sha256: sha_hex,
                        identity: identity.into(),
                    },
                )?)?;
            }
            Ok(())
        })?;
    }
    Ok(count)
}

/// The runs projector's last ledger sequence: changes whenever a run is recorded or re-keyed.
pub fn generation(engine: &Engine) -> Result<u64> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let table = read.open_table(crate::db::CHECKPOINT)?;
        Ok(crate::db::read_checkpoint(&table, "runs/ledger")?
            .map(|c| c.last_sequence_id)
            .unwrap_or(0))
    })
}

// ---------------------------------------------------------------------------------------------
// Queries

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct QueryArgs {
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    /// observed or benchmark; absent means both.
    pub source: Option<String>,
    pub task_type: Option<String>,
    /// Only runs with a review block: the reviews a filtered view still joins to its runs.
    pub reviews_only: Option<bool>,
    /// Include valid suppressed files for reader receipts on the first page.
    pub include_receipts: Option<bool>,
    /// Resume after this `[endedMs, r]` cursor.
    pub after: Option<(u64, u32)>,
    pub limit: Option<usize>,
    /// R4: keep each row's `_cursor` (`[endedMs, r]`), so a reader holding the unfiltered rows
    /// can apply the time bounds exactly as this query does.
    pub include_cursors: Option<bool>,
}

fn opt_json<T: Into<Json>>(v: Option<T>) -> Json {
    v.map(Into::into).unwrap_or(Json::Null)
}

/// The identity object of a head row.
fn identity_json<T: ReadableTable<&'static [u8], &'static [u8]>>(dict: &mut Dict<'_, T>, head: &Head) -> Result<Json> {
    Ok(json!({
        "modelRaw": dict.field(head, at::MODEL_RAW)?,
        "modelId": dict.field(head, at::MODEL)?,
        "modelSource": dict.field(head, at::MODEL_SOURCE)?,
        "modelFamily": dict.field(head, at::FAMILY)?,
        "effortRaw": dict.field(head, at::EFFORT_RAW)?,
        "effort": dict.field(head, at::EFFORT)?,
        "effortSource": dict.field(head, at::EFFORT_SOURCE)?,
        "effortConflict": head.flags & F_EFFORT_CONFLICT != 0,
        "harnessRaw": dict.field(head, at::HARNESS_RAW)?,
        "harness": dict.field(head, at::HARNESS)?,
        "harnessSource": dict.field(head, at::HARNESS_SOURCE)?,
        "modelCatalogHash": dict.field(head, at::CATALOG)?,
    }))
}

/// The correlation ids of a head row (DATA-ARCHITECTURE section 2 names).
fn ids_json<T: ReadableTable<&'static [u8], &'static [u8]>>(
    dict: &mut Dict<'_, T>,
    r: u32,
    head: &Head,
) -> Result<Json> {
    let project = dict.field(head, at::PROJECT)?;
    let stream = dict.field(head, at::STREAM)?;
    let t3_thread = dict.field(head, at::T3_THREAD)?;
    let t3_task = dict.field(head, at::T3_TASK)?;
    let t3_thread_source = dict.field(head, at::T3_THREAD_SOURCE)?;
    Ok(json!({
        "runId": dict.text(kind::RUN_ID, r)?,
        "writer": dict.field(head, at::WRITER)?,
        "taskId": dict.field(head, at::TASK_ID)?,
        "cardId": null,
        "taskType": dict.field(head, at::TASK_TYPE)?,
        "computer": dict.field(head, at::COMPUTER)?,
        "machine": dict.field(head, at::MACHINE)?,
        "provider": { "value": dict.field(head, at::PROVIDER)?, "identitySource": "reported", "confidence": "unreliable" },
        "accountKey": null,
        "accountRaw": dict.field(head, at::ACCOUNT)?,
        "t3EnvironmentId": null,
        "t3RunId": null,
        "t3ThreadId": t3_thread.as_ref().map(|t| json!({ "value": t, "identitySource": t3_thread_source, "confidence": "inferred" })),
        "t3TaskId": t3_task.as_ref().map(|t| json!({ "value": t, "identitySource": "reported", "confidence": "explicit" })),
        "projectId": project.map(|id| json!({ "source": "agent-report", "id": id })),
        "streamId": stream.map(|id| json!({ "source": "agent-report", "id": id })),
        "eventAtMs": head.ended_ms,
        "observedAtMs": head.written_ms,
        "ingestedAtMs": head.ingested_ms,
    }))
}

/// Current revisions in a time range, as the run@1 projection the view builder reads (model,
/// effort and harness canonical, provider dropped as the TypeScript view does) with each run's
/// identity and ids. Paged by `(endedMs, r)`.
fn has_corrections(engine: &Engine) -> Result<bool> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        Ok(read.open_table(RUN_META)?.get("corrections")?.is_some())
    })
}
fn hide_cursors(page: &mut Json) {
    if let Some(rows) = page["runs"].as_array_mut() {
        for row in rows {
            if let Some(obj) = row.as_object_mut() {
                obj.remove("_cursor");
            }
        }
    }
}
/// One accepted revision's readers-check receipt fields (5 tiny fields instead of a full Json doc).
#[derive(Clone, Debug)]
pub struct AcceptedReceipt {
    /// Relative path of the run file under the reports share.
    pub file: Box<str>,
    /// Run identifier recorded in the run doc (`doc.id`).
    pub id: Box<str>,
    /// Revision sequence number recorded in the run doc.
    pub seq: u64,
    /// Hex SHA-256 of the run file bytes.
    pub sha256: Box<str>,
    /// Epoch milliseconds when the revision was first ingested.
    pub first_seen_at: u64,
}

/// Run source on a typed `RunFact` (1 byte instead of a heap string).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RunSource {
    Observed,
    Benchmark,
}

impl RunSource {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Benchmark => "benchmark",
        }
    }

    pub fn is_benchmark(self) -> bool {
        matches!(self, Self::Benchmark)
    }
}

/// Compact review link carried on a typed `RunFact` (allocated only for review runs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunReviewLink {
    /// Original `review.of` written in the run document.
    pub orig_of: Box<str>,
    /// Effective `review.of` after correction-chain resolution.
    pub of: Box<str>,
    /// Interned review verdict (`pass`, `fail`, `cannot-verify`).
    pub verdict: Arc<str>,
    pub overturned: bool,
    pub disputed: bool,
}

/// Optional rarely-present fields packed behind one boxed allocation (8 bytes inline when absent).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunFactExtra {
    pub benchmark: Option<Arc<str>>,
    pub process: Option<(Arc<str>, Option<Arc<str>>)>,
    /// `[records, files, items, words, tasks]` using `u64::MAX` as sentinel for `None`.
    pub task_size: Option<[u64; 5]>,
    pub corrects: Option<Box<str>>,
    pub review: Option<Box<RunReviewLink>>,
}

const FACT_FLAG_VOID: u8 = 1;
const FACT_FLAG_IMPORTED: u8 = 2;
const FACT_FLAG_UNKNOWN_MODEL: u8 = 4;
const FACT_FLAG_EFFORT_CONFLICT: u8 = 8;
const NO_WALL_BITS: u64 = u64::MAX;

/// One resolved run held in memory as a small typed fact (~0.5 KB total, zero document text).
/// The full `run@1` document stays in redb `RUN_PAYLOAD` at `loc` and is loaded on demand via
/// [`load_run_doc`] / [`load_run_docs`] only when a view or query needs it.
/// Generations share unchanged facts by `Arc<RunFact>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunFact {
    /// `[endedMs, r]` cursor ordering this row in resolved history.
    pub cursor: (u64, u32),
    /// Exact payload location `(day, block, slot)` in `RUN_PAYLOAD` for on-demand doc reads.
    pub loc: Loc,
    /// Raw 32-byte SHA-256 of the winning revision's file bytes.
    pub sha: [u8; 32],
    /// Epoch milliseconds when the revision was first ingested (`firstSeenAt`).
    pub first_seen_at: u64,
    /// Revision sequence number (`doc.seq`).
    pub seq: u64,
    /// Started timestamp in epoch ms (`UNKNOWN` when absent).
    started_at_ms_raw: u64,
    /// Ended timestamp in epoch ms (`head.ended_ms`).
    pub ended_at_ms: u64,
    /// Written timestamp in epoch ms (`head.written_ms`).
    pub written_at_ms: u64,
    /// Exact IEEE-754 bits of `doc.wallSeconds` (`NO_WALL_BITS` when absent).
    wall_seconds_bits: u64,
    /// Relative path of the run file under the reports share.
    pub file: Box<str>,
    /// Run identifier (`doc.id`).
    pub id: Box<str>,
    /// Task identifier (`doc.task.id`).
    pub task_id: Box<str>,
    /// Exact ISO `doc.endedAt` string.
    pub ended_at: Box<str>,
    /// Exact ISO `doc.writtenAt` string when it differs from `ended_at` (`None` => equals `ended_at`).
    written_at_diff: Option<Box<str>>,
    /// Writer identifier (`doc.writer`, interned).
    pub writer: Arc<str>,
    /// Canonical shown model id (`head.MODEL` or fallback `head.MODEL_RAW`, interned).
    pub model_id: Option<Arc<str>>,
    /// Raw written model id (`head.MODEL_RAW`, interned).
    pub model_raw: Option<Arc<str>>,
    /// Canonical model family (`head.FAMILY`, interned).
    pub model_family: Option<Arc<str>>,
    /// Canonical effort (`head.EFFORT`, interned).
    pub effort: Option<Arc<str>>,
    /// Canonical harness (`head.HARNESS`, interned).
    pub harness: Option<Arc<str>>,
    /// Optional project id (`doc.project`, interned).
    pub project: Option<Arc<str>>,
    /// Optional stream id (`doc.stream`, interned).
    pub stream: Option<Arc<str>>,
    /// Optional task type (`doc.task.type`, interned).
    pub task_type: Option<Arc<str>>,
    /// Outcome string (`doc.outcome`, interned).
    pub outcome: Arc<str>,
    /// Rarely-present fields (`benchmark`, `process`, `task_size`, `corrects`, `review`).
    pub extra: Option<Box<RunFactExtra>>,
    /// Run source (`observed` / `benchmark`).
    pub source: RunSource,
    /// Compact boolean flags (`FACT_FLAG_*`).
    pub flags: u8,
}

impl RunFact {
    pub fn started_at_ms(&self) -> Option<u64> {
        (self.started_at_ms_raw != UNKNOWN).then_some(self.started_at_ms_raw)
    }

    pub fn wall_seconds(&self) -> Option<f64> {
        (self.wall_seconds_bits != NO_WALL_BITS).then(|| f64::from_bits(self.wall_seconds_bits))
    }

    pub fn written_at(&self) -> &str {
        self.written_at_diff.as_deref().unwrap_or(&self.ended_at)
    }

    pub fn sha256_hex(&self) -> String {
        hex(&self.sha)
    }

    pub fn source_str(&self) -> &'static str {
        self.source.as_str()
    }

    pub fn benchmark(&self) -> Option<&str> {
        self.extra.as_ref().and_then(|e| e.benchmark.as_deref())
    }

    pub fn process(&self) -> Option<(&str, Option<&str>)> {
        self.extra
            .as_ref()
            .and_then(|e| e.process.as_ref())
            .map(|(id, ver)| (id.as_ref(), ver.as_deref()))
    }

    pub fn task_size(&self) -> [Option<u64>; 5] {
        match self.extra.as_ref().and_then(|e| e.task_size) {
            Some(arr) => arr.map(|v| (v != u64::MAX).then_some(v)),
            None => [None; 5],
        }
    }

    pub fn corrects(&self) -> Option<&str> {
        self.extra.as_ref().and_then(|e| e.corrects.as_deref())
    }

    pub fn review(&self) -> Option<&RunReviewLink> {
        self.extra.as_ref().and_then(|e| e.review.as_deref())
    }

    pub fn has_review(&self) -> bool {
        self.review().is_some()
    }

    pub fn has_void(&self) -> bool {
        self.flags & FACT_FLAG_VOID != 0
    }

    pub fn imported(&self) -> bool {
        self.flags & FACT_FLAG_IMPORTED != 0
    }

    pub fn unknown_model(&self) -> bool {
        self.flags & FACT_FLAG_UNKNOWN_MODEL != 0
    }

    pub fn effort_conflict(&self) -> bool {
        self.flags & FACT_FLAG_EFFORT_CONFLICT != 0
    }

    /// Approximate per-run resident bytes (inline struct + unique per-run heap allocations,
    /// excluding interned `Arc<str>` strings shared across runs).
    pub fn estimated_bytes(&self) -> usize {
        let mut total = std::mem::size_of::<Self>()
            + self.file.len()
            + self.id.len()
            + self.task_id.len()
            + self.ended_at.len()
            + self.written_at_diff.as_deref().map_or(0, str::len);
        if let Some(extra) = &self.extra {
            total += std::mem::size_of::<RunFactExtra>();
            if let Some(c) = &extra.corrects {
                total += c.len();
            }
            if let Some(rev) = &extra.review {
                total += std::mem::size_of::<RunReviewLink>() + rev.orig_of.len() + rev.of.len();
            }
        }
        total
    }

    fn with_effective_review_of(&self, new_of: &str) -> Self {
        let mut cloned = self.clone();
        if let Some(extra) = cloned.extra.as_mut()
            && let Some(rev) = extra.review.as_mut()
        {
            rev.of = new_of.into();
        }
        cloned
    }
}

/// One immutable resolved history, keyed by the same database snapshot's run generation and
/// catalog. Holds only typed `Arc<RunFact>` rows (~0.5 KB each, shared across generations and
/// between `rows` and `accepted`); full run documents stay in redb `RUN_PAYLOAD` and load on demand.
pub struct ResolvedHistory {
    key: (u64, Option<Vec<u8>>),
    rows: Vec<Arc<RunFact>>,
    problems: Vec<Json>,
    skipped: usize,
    accepted: Vec<Arc<RunFact>>,
}

impl ResolvedHistory {
    /// The runs ledger generation this snapshot was resolved at.
    pub fn generation(&self) -> u64 {
        self.key.0
    }

    /// The catalog hash this snapshot was resolved under, as `runsQuery` pages name it.
    pub fn catalog_hash(&self) -> Option<String> {
        self.key
            .1
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
    }

    /// The effective rows (corrections resolved), as shared typed `RunFact`s.
    pub fn facts(&self) -> &[Arc<RunFact>] {
        &self.rows
    }

    /// All non-refused revisions, sharing the same `Arc<RunFact>` instances as `facts()` for
    /// effective runs (zero duplicate receipt string allocations).
    pub fn accepted_facts(&self) -> &[Arc<RunFact>] {
        &self.accepted
    }

    /// The correction refusals, as the first `runsQuery` page carries them.
    pub fn problems(&self) -> &[Json] {
        &self.problems
    }

    /// Count of stored runs whose payload failed projection when building this snapshot.
    pub fn skipped(&self) -> usize {
        self.skipped
    }
}

/// The engine's resident resolved-history snapshot: built once per generation, sharing unchanged
/// `Arc<RunFact>` rows across generations without copying the table or keeping doc text in RAM.
pub fn resolved_history_snapshot(engine: &Engine) -> Result<Arc<ResolvedHistory>> {
    resolved_history(engine)
}

/// Load and project one run's full `run@1` JSON document from redb `RUN_PAYLOAD` inside an
/// existing read transaction, reusing a bounded 16-block decompression cache.
pub(crate) fn load_run_doc_in_read<T: ReadableTable<&'static [u8], &'static [u8]>>(
    payloads: &T,
    blocks: &mut HashMap<(u32, u32), Vec<Vec<u8>>>,
    fact: &RunFact,
) -> Result<Option<Json>> {
    let block_key = (fact.loc.day, fact.loc.block);
    if !blocks.contains_key(&block_key) {
        if blocks.len() >= 16 {
            blocks.clear();
        }
        let Some(bytes) = payloads.get(Key::new().u32(fact.loc.day).u32(fact.loc.block).as_slice())? else {
            return Ok(None);
        };
        blocks.insert(block_key, block_decode(bytes.value())?.1);
    }
    let Some(doc_bytes) = blocks[&block_key].get(fact.loc.slot as usize) else {
        return Ok(None);
    };
    let text = String::from_utf8_lossy(doc_bytes);
    let Ok(mut run) = run_schema::parse_stored_run_text(&text, &fact.writer) else {
        return Ok(None);
    };
    let shown = fact.model_id.as_deref().or(fact.model_raw.as_deref());
    run["model"]["id"] = opt_json(shown);
    run["model"]["effort"] = opt_json(fact.effort.as_deref());
    run["model"]["harness"] = opt_json(fact.harness.as_deref());
    run["model"]["provider"] = Json::Null;
    if let Some(rev) = fact.review()
        && run["review"].is_object()
    {
        run["review"]["of"] = json!(&*rev.of);
    }
    Ok(Some(run))
}

/// Load one run's projected `run@1` JSON document on demand from redb `RUN_PAYLOAD`.
pub fn load_run_doc(engine: &Engine, fact: &RunFact) -> Result<Option<Json>> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let payloads = read.open_table(RUN_PAYLOAD)?;
        let mut blocks = HashMap::new();
        load_run_doc_in_read(&payloads, &mut blocks, fact)
    })
}

/// Batch-load projected `run@1` JSON documents on demand from redb `RUN_PAYLOAD` in one MVCC
/// read transaction with a bounded 16-block decompression cache.
pub fn load_run_docs(engine: &Engine, facts: &[&RunFact]) -> Result<Vec<Option<Json>>> {
    if facts.is_empty() {
        return Ok(Vec::new());
    }
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let payloads = read.open_table(RUN_PAYLOAD)?;
        let mut blocks = HashMap::new();
        let mut out = Vec::with_capacity(facts.len());
        for fact in facts {
            out.push(load_run_doc_in_read(&payloads, &mut blocks, fact)?);
        }
        Ok(out)
    })
}

/// Stream projected `run@1` JSON documents one at a time in one MVCC read transaction with a
/// bounded 16-block decompression cache. Fails immediately with `missing_doc` if any requested
/// document cannot be loaded or projected; each `Json` drops before the next is parsed.
pub fn for_each_run_doc<F>(engine: &Engine, facts: &[&RunFact], mut f: F) -> Result<()>
where
    F: FnMut(usize, Json) -> Result<()>,
{
    if facts.is_empty() {
        return Ok(());
    }
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let payloads = read.open_table(RUN_PAYLOAD)?;
        let mut blocks = HashMap::new();
        for (index, fact) in facts.iter().enumerate() {
            let Some(doc) = load_run_doc_in_read(&payloads, &mut blocks, fact)? else {
                fail!(
                    "missing_doc",
                    "run payload document could not be loaded for {}",
                    fact.id
                );
            };
            f(index, doc)?;
        }
        Ok(())
    })
}

/// Test helper: clear all `RUN_PAYLOAD` blocks on an open engine without closing it.
#[doc(hidden)]
pub fn wipe_payload_blocks_for_test(engine: &Engine) -> Result<()> {
    engine.with_db(|db| {
        let txn = crate::engine::begin(db)?;
        let keys: Vec<Vec<u8>> = {
            let tbl = txn.open_table(RUN_PAYLOAD)?;
            tbl.iter()?
                .map(|item| item.map(|(k, _)| k.value().to_vec()))
                .collect::<std::result::Result<Vec<_>, _>>()?
        };
        {
            let mut tbl = txn.open_table(RUN_PAYLOAD)?;
            for k in keys {
                tbl.remove(k.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    })
}

/// All `Head` fields that determine a run's typed `RunFact` (excluding `revisions` and
/// `event_seq`, so losing revisions do not invalidate cached `Arc<RunFact>` rows).
#[derive(Clone, Debug, PartialEq, Eq)]
struct HeadFactSig {
    flags: u8,
    verdict: u8,
    seq: u64,
    written_ms: u64,
    ingested_ms: u64,
    started_ms: u64,
    ended_ms: u64,
    wall_ms: u64,
    sha: [u8; 32],
    loc: Loc,
    ids: [u32; 26],
}

impl HeadFactSig {
    fn from_head(head: &Head) -> Self {
        Self {
            flags: head.flags,
            verdict: head.verdict,
            seq: head.seq,
            written_ms: head.written_ms,
            ingested_ms: head.ingested_ms,
            started_ms: head.started_ms,
            ended_ms: head.ended_ms,
            wall_ms: head.wall_ms,
            sha: head.sha,
            loc: head.loc,
            ids: head.ids,
        }
    }

    /// True when the underlying payload document and writer did not change (only catalog identity
    /// or path metadata moved), so `RunFact` can refresh from `Head` + `Dict` with zero payload reads.
    fn same_doc_content(&self, other: &Self) -> bool {
        self.sha == other.sha
            && self.loc == other.loc
            && self.seq == other.seq
            && self.written_ms == other.written_ms
            && self.started_ms == other.started_ms
            && self.ended_ms == other.ended_ms
            && self.wall_ms == other.wall_ms
            && self.ids[at::WRITER] == other.ids[at::WRITER]
    }
}

struct CachedBaseFact {
    sig: HeadFactSig,
    /// `None` when this revision's payload failed `parse_run_text` (counted in `skipped`).
    fact: Option<Arc<RunFact>>,
}

/// Low-cardinality string pool shared across all runs and generations in one engine.
#[derive(Default)]
struct StringInterner {
    pool: HashSet<Arc<str>>,
}

impl StringInterner {
    fn intern(&mut self, text: &str) -> Arc<str> {
        if let Some(hit) = self.pool.get(text) {
            return Arc::clone(hit);
        }
        let arc: Arc<str> = Arc::from(text);
        self.pool.insert(Arc::clone(&arc));
        arc
    }

    fn intern_opt(&mut self, text: Option<&str>) -> Option<Arc<str>> {
        text.map(|s| self.intern(s))
    }

    fn sweep(&mut self) {
        self.pool.retain(|s| Arc::strong_count(s) > 1);
        if self.pool.capacity() > 256 && self.pool.len() * 4 < self.pool.capacity() {
            self.pool.shrink_to_fit();
        }
    }

    fn clear(&mut self) {
        self.pool.clear();
    }
}

/// Per-engine cache: no cross-store sharing, at most one active generation pointer, and
/// cross-generation `Arc<RunFact>` reuse keyed by `r` + `HeadFactSig`.
#[derive(Default)]
pub(crate) struct HistoryCache {
    snapshot: Option<Arc<ResolvedHistory>>,
    by_r: HashMap<u32, CachedBaseFact>,
    retargeted: HashMap<(u32, Box<str>), (HeadFactSig, Arc<RunFact>)>,
    intern: StringInterner,
    resolutions: u64,
    payload_reads: u64,
}

impl HistoryCache {
    pub(crate) fn clear(&mut self) {
        self.snapshot = None;
        self.by_r.clear();
        self.retargeted.clear();
        self.intern.clear();
    }
}

/// Number of full correction resolutions built by this engine (not query pages).
#[doc(hidden)]
pub fn correction_resolution_count(engine: &Engine) -> u64 {
    engine
        .runs_history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .resolutions
}

/// Number of payload documents decoded from `RUN_PAYLOAD` while building `ResolvedHistory` snapshots.
#[doc(hidden)]
pub fn history_payload_read_count(engine: &Engine) -> u64 {
    engine
        .runs_history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .payload_reads
}

/// Number of live strings currently retained in this engine's `HistoryCache` string interner.
#[doc(hidden)]
pub fn history_intern_pool_len(engine: &Engine) -> usize {
    engine
        .runs_history
        .lock()
        .unwrap_or_else(|p| p.into_inner())
        .intern
        .pool
        .len()
}

fn fact_flags(has_void: bool, head_flags: u8) -> u8 {
    let mut f = 0u8;
    if has_void {
        f |= FACT_FLAG_VOID;
    }
    if head_flags & F_IMPORTED != 0 {
        f |= FACT_FLAG_IMPORTED;
    }
    if head_flags & F_UNKNOWN_MODEL != 0 {
        f |= FACT_FLAG_UNKNOWN_MODEL;
    }
    if head_flags & F_EFFORT_CONFLICT != 0 {
        f |= FACT_FLAG_EFFORT_CONFLICT;
    }
    f
}

fn pack_extra(
    benchmark: Option<Arc<str>>,
    process: Option<(Arc<str>, Option<Arc<str>>)>,
    task_size: [Option<u64>; 5],
    corrects: Option<Box<str>>,
    review: Option<RunReviewLink>,
) -> Option<Box<RunFactExtra>> {
    let packed_size = if task_size.iter().any(Option::is_some) {
        Some(task_size.map(|v| v.unwrap_or(u64::MAX)))
    } else {
        None
    };
    if benchmark.is_none() && process.is_none() && packed_size.is_none() && corrects.is_none() && review.is_none() {
        None
    } else {
        Some(Box::new(RunFactExtra {
            benchmark,
            process,
            task_size: packed_size,
            corrects,
            review: review.map(Box::new),
        }))
    }
}

fn fact_from_stored<T: ReadableTable<&'static [u8], &'static [u8]>>(
    dict: &mut Dict<'_, T>,
    intern: &mut StringInterner,
    ended_ms: u64,
    r: u32,
    head: &Head,
    sb: &crate::scoreboard_agg::StoredRunFact,
) -> Result<RunFact> {
    let model_raw = dict.field(head, at::MODEL_RAW)?;
    let model_family = dict.field(head, at::FAMILY)?;
    let benchmark = intern.intern_opt(sb.benchmark.as_deref());
    let process = sb
        .process
        .as_ref()
        .map(|(id, ver)| (intern.intern(id), intern.intern_opt(ver.as_deref())));
    let review = sb.review.as_ref().map(|rev| RunReviewLink {
        orig_of: rev.orig_of.as_str().into(),
        of: rev.orig_of.as_str().into(),
        verdict: intern.intern(&rev.verdict),
        overturned: rev.overturned,
        disputed: rev.disputed,
    });
    let corrects = sb.corrects.as_deref().map(Into::into);
    let written_at_diff = (sb.written_at != sb.ended_at).then(|| sb.written_at.as_str().into());
    Ok(RunFact {
        cursor: (ended_ms, r),
        loc: head.loc,
        sha: head.sha,
        first_seen_at: head.ingested_ms,
        seq: head.seq,
        started_at_ms_raw: head.started_ms,
        ended_at_ms: head.ended_ms,
        written_at_ms: head.written_ms,
        wall_seconds_bits: sb.wall_seconds.map(f64::to_bits).unwrap_or(NO_WALL_BITS),
        file: sb.file.as_str().into(),
        id: sb.id.as_str().into(),
        task_id: sb.task_id.as_str().into(),
        ended_at: sb.ended_at.as_str().into(),
        written_at_diff,
        writer: intern.intern(&sb.writer),
        model_id: intern.intern_opt(sb.model_id.as_deref()),
        model_raw: intern.intern_opt(model_raw.as_deref()),
        model_family: intern.intern_opt(model_family.as_deref()),
        effort: intern.intern_opt(sb.effort.as_deref()),
        harness: intern.intern_opt(sb.harness.as_deref()),
        project: intern.intern_opt(sb.project.as_deref()),
        stream: intern.intern_opt(sb.stream.as_deref()),
        task_type: intern.intern_opt(sb.task_type.as_deref()),
        outcome: intern.intern(&sb.outcome_raw),
        extra: pack_extra(benchmark, process, sb.task_size, corrects, review),
        source: if sb.source_benchmark {
            RunSource::Benchmark
        } else {
            RunSource::Observed
        },
        flags: fact_flags(sb.has_void, head.flags),
    })
}

fn fact_from_payload_bytes<T: ReadableTable<&'static [u8], &'static [u8]>>(
    dict: &mut Dict<'_, T>,
    intern: &mut StringInterner,
    ended_ms: u64,
    r: u32,
    head: &Head,
    doc_bytes: &[u8],
) -> Result<Option<RunFact>> {
    let writer = dict.field(head, at::WRITER)?.unwrap_or_default();
    let text = String::from_utf8_lossy(doc_bytes);
    let Ok(doc) = run_schema::parse_stored_run_text(&text, &writer) else {
        return Ok(None);
    };
    let model_raw = dict.field(head, at::MODEL_RAW)?;
    let shown = dict.field(head, at::MODEL)?.or_else(|| model_raw.clone());
    let model_family = dict.field(head, at::FAMILY)?;
    let effort = dict.field(head, at::EFFORT)?;
    let harness = dict.field(head, at::HARNESS)?;
    let file = dict.field(head, at::PATH)?.unwrap_or_default();
    let ended_at = doc["endedAt"].as_str().unwrap_or_default();
    let written_at = doc["writtenAt"].as_str().unwrap_or_default();
    let written_at_diff = (written_at != ended_at).then(|| written_at.into());
    let task = &doc["task"];
    let size = &task["size"];
    let task_size = [
        size["records"].as_u64(),
        size["files"].as_u64(),
        size["items"].as_u64(),
        size["words"].as_u64(),
        size["tasks"].as_u64(),
    ];
    let process = doc["process"].as_object().map(|p| {
        (
            intern.intern(p.get("id").and_then(Json::as_str).unwrap_or_default()),
            intern.intern_opt(p.get("version").and_then(Json::as_str)),
        )
    });
    let review = doc["review"].as_object().map(|rev| {
        let of = rev.get("of").and_then(Json::as_str).unwrap_or_default();
        RunReviewLink {
            orig_of: of.into(),
            of: of.into(),
            verdict: intern.intern(rev.get("verdict").and_then(Json::as_str).unwrap_or_default()),
            overturned: rev.get("overturned").and_then(Json::as_bool).unwrap_or(false),
            disputed: rev.get("disputed").and_then(Json::as_bool).unwrap_or(false),
        }
    });
    let corrects = doc.get("corrects").and_then(Json::as_str).map(Into::into);
    let has_void = doc.get("void").is_some_and(Json::is_object);
    let benchmark = intern.intern_opt(doc["benchmark"].as_str());
    let wall_seconds_bits = doc["wallSeconds"]
        .as_f64()
        .filter(|n| n.is_finite())
        .map(f64::to_bits)
        .unwrap_or(NO_WALL_BITS);
    Ok(Some(RunFact {
        cursor: (ended_ms, r),
        loc: head.loc,
        sha: head.sha,
        first_seen_at: head.ingested_ms,
        seq: head.seq,
        started_at_ms_raw: head.started_ms,
        ended_at_ms: head.ended_ms,
        written_at_ms: head.written_ms,
        wall_seconds_bits,
        file: file.into_boxed_str(),
        id: doc["id"].as_str().unwrap_or_default().into(),
        task_id: task["id"].as_str().unwrap_or_default().into(),
        ended_at: ended_at.into(),
        written_at_diff,
        writer: intern.intern(&writer),
        model_id: intern.intern_opt(shown.as_deref()),
        model_raw: intern.intern_opt(model_raw.as_deref()),
        model_family: intern.intern_opt(model_family.as_deref()),
        effort: intern.intern_opt(effort.as_deref()),
        harness: intern.intern_opt(harness.as_deref()),
        project: intern.intern_opt(doc["project"].as_str()),
        stream: intern.intern_opt(doc["stream"].as_str()),
        task_type: intern.intern_opt(task["type"].as_str()),
        outcome: intern.intern(doc["outcome"].as_str().unwrap_or_default()),
        extra: pack_extra(benchmark, process, task_size, corrects, review),
        source: if doc["source"].as_str() == Some("benchmark") {
            RunSource::Benchmark
        } else {
            RunSource::Observed
        },
        flags: fact_flags(has_void, head.flags),
    }))
}

fn refresh_fact_metadata<T: ReadableTable<&'static [u8], &'static [u8]>>(
    dict: &mut Dict<'_, T>,
    intern: &mut StringInterner,
    base: &RunFact,
    head: &Head,
) -> Result<RunFact> {
    let model_raw = dict.field(head, at::MODEL_RAW)?;
    let shown = dict.field(head, at::MODEL)?.or_else(|| model_raw.clone());
    let model_family = dict.field(head, at::FAMILY)?;
    let effort = dict.field(head, at::EFFORT)?;
    let harness = dict.field(head, at::HARNESS)?;
    let file = dict.field(head, at::PATH)?.unwrap_or_default();
    let mut updated = base.clone();
    updated.first_seen_at = head.ingested_ms;
    updated.file = file.into_boxed_str();
    updated.model_id = intern.intern_opt(shown.as_deref());
    updated.model_raw = intern.intern_opt(model_raw.as_deref());
    updated.model_family = intern.intern_opt(model_family.as_deref());
    updated.effort = intern.intern_opt(effort.as_deref());
    updated.harness = intern.intern_opt(harness.as_deref());
    updated.flags = fact_flags(base.has_void(), head.flags);
    Ok(updated)
}

fn resolved_history(engine: &Engine) -> Result<Arc<ResolvedHistory>> {
    // Lock order is database then cache, also used by close. All reads share one MVCC
    // transaction so concurrent ingestion cannot mix generations in the snapshot.
    engine.with_db(|db| {
        let mut cache_guard = engine.runs_history.lock().unwrap_or_else(|p| p.into_inner());
        let cache = &mut *cache_guard;
        let read = db.begin_read()?;
        let checkpoint = read.open_table(crate::db::CHECKPOINT)?;
        let generation = crate::db::read_checkpoint(&checkpoint, "runs/ledger")?
            .map(|c| c.last_sequence_id)
            .unwrap_or(0);
        let catalog = read.open_table(RUN_META)?.get("catalog")?.map(|v| v.value().to_vec());
        let key = (generation, catalog);
        if let Some(snapshot) = cache.snapshot.as_ref().filter(|s| s.key == key) {
            return Ok(Arc::clone(snapshot));
        }

        let by_time = read.open_table(RUN_BY_TIME)?;
        let heads = read.open_table(RUN_HEAD)?;
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);

        // Cold build fast path: when the cache is empty and `RUN_SB_FACT` passes full trust
        // verification, build typed `RunFact` rows directly from `RUN_SB_FACT` + `RUN_HEAD`
        // without decompressing `RUN_PAYLOAD` or parsing full JSON documents.
        let use_sb_facts = cache.by_r.is_empty() && crate::scoreboard_agg::verify_trust_stamp_rd(&read)?;
        let sb_fact_table = if use_sb_facts {
            Some(read.open_table(crate::scoreboard_agg::RUN_SB_FACT)?)
        } else {
            None
        };
        let mut payloads_table = None;
        let mut blocks: HashMap<(u32, u32), Vec<Vec<u8>>> = HashMap::new();

        let total_hint = heads.len()? as usize;
        let mut ordered: Vec<(u32, HeadFactSig, Arc<RunFact>)> = Vec::with_capacity(total_hint);
        let mut seen_rs: HashSet<u32> = HashSet::with_capacity(total_hint);
        let mut skipped = 0usize;

        for item in by_time.iter()? {
            let (k, _) = item?;
            let (ended_ms, r) = (be_u64(k.value(), 0), be_u32(k.value(), 8));
            let Some(hv) = heads.get(Key::new().u32(r).as_slice())? else {
                continue;
            };
            let head = Head::decode(hv.value())?;
            let sig = HeadFactSig::from_head(&head);
            seen_rs.insert(r);

            if let Some(cached) = cache.by_r.get(&r)
                && cached.sig == sig
            {
                match &cached.fact {
                    Some(fact) => ordered.push((r, sig, Arc::clone(fact))),
                    None => skipped += 1,
                }
                continue;
            }

            // Metadata-only update (catalog rekey or path raise): refresh from Head + Dict with
            // zero RUN_PAYLOAD reads.
            if let Some(cached) = cache.by_r.get(&r)
                && cached.sig.same_doc_content(&sig)
                && let Some(prev_fact) = cached.fact.as_ref()
            {
                let updated = Arc::new(refresh_fact_metadata(&mut dict, &mut cache.intern, prev_fact, &head)?);
                cache.by_r.insert(
                    r,
                    CachedBaseFact {
                        sig: sig.clone(),
                        fact: Some(Arc::clone(&updated)),
                    },
                );
                ordered.push((r, sig, updated));
                continue;
            }

            if let Some(ref sb_tbl) = sb_fact_table {
                let fact_opt = match sb_tbl.get(Key::new().u32(r).as_slice())? {
                    Some(bytes) => {
                        let sb = crate::scoreboard_agg::StoredRunFact::decode(bytes.value())?;
                        Some(Arc::new(fact_from_stored(
                            &mut dict,
                            &mut cache.intern,
                            ended_ms,
                            r,
                            &head,
                            &sb,
                        )?))
                    }
                    None => None,
                };
                match &fact_opt {
                    Some(fact) => ordered.push((r, sig.clone(), Arc::clone(fact))),
                    None => skipped += 1,
                }
                cache.by_r.insert(r, CachedBaseFact { sig, fact: fact_opt });
                continue;
            }

            // Read ONLY this changed/new run's payload block from redb.
            if payloads_table.is_none() {
                payloads_table = Some(read.open_table(RUN_PAYLOAD)?);
            }
            let payloads = payloads_table.as_ref().unwrap();
            let block_key = (head.loc.day, head.loc.block);
            if !blocks.contains_key(&block_key) {
                if blocks.len() >= 16 {
                    blocks.clear();
                }
                if let Some(bytes) = payloads.get(Key::new().u32(head.loc.day).u32(head.loc.block).as_slice())? {
                    blocks.insert(block_key, block_decode(bytes.value())?.1);
                }
            }
            let doc_opt = blocks.get(&block_key).and_then(|docs| docs.get(head.loc.slot as usize));
            let fact_opt = match doc_opt {
                Some(doc_bytes) => {
                    cache.payload_reads += 1;
                    fact_from_payload_bytes(&mut dict, &mut cache.intern, ended_ms, r, &head, doc_bytes)?.map(Arc::new)
                }
                None => None,
            };
            match &fact_opt {
                Some(fact) => ordered.push((r, sig.clone(), Arc::clone(fact))),
                None => skipped += 1,
            }
            cache.by_r.insert(r, CachedBaseFact { sig, fact: fact_opt });
        }

        if cache.by_r.len() > seen_rs.len() {
            cache.by_r.retain(|r, _| seen_rs.contains(r));
        }

        // Resolve correction chains ONLY over the correction-chain closure; every unaffected run
        // keeps its exact `Arc<RunFact>` pointer with zero JSON construction or cloning.
        let has_any_corrects = ordered.iter().any(|(_, _, f)| f.corrects().is_some() || f.has_void());

        let (rows, problems, accepted) = if !has_any_corrects {
            let rows: Vec<Arc<RunFact>> = ordered.into_iter().map(|(_, _, f)| f).collect();
            let accepted = rows.clone();
            cache.retargeted.clear();
            (rows, Vec::new(), accepted)
        } else {
            let id_to_idx: HashMap<&str, usize> =
                ordered.iter().enumerate().map(|(i, (_, _, f))| (&*f.id, i)).collect();
            let mut rev_by_target: HashMap<&str, Vec<usize>> = HashMap::new();
            for (i, (_, _, f)) in ordered.iter().enumerate() {
                if let Some(rev) = f.review() {
                    rev_by_target.entry(&*rev.orig_of).or_default().push(i);
                }
            }
            let mut closure: HashSet<usize> = HashSet::new();
            let mut queue: Vec<usize> = Vec::new();
            for (i, (_, _, f)) in ordered.iter().enumerate() {
                if (f.corrects().is_some() || f.has_void()) && closure.insert(i) {
                    queue.push(i);
                }
            }
            let mut pos = 0;
            while pos < queue.len() {
                let idx = queue[pos];
                pos += 1;
                if let Some(target_id) = ordered[idx].2.corrects()
                    && let Some(&t_idx) = id_to_idx.get(target_id)
                    && closure.insert(t_idx)
                {
                    queue.push(t_idx);
                }
                if let Some(revs) = rev_by_target.get(&*ordered[idx].2.id) {
                    for &ri in revs {
                        if closure.insert(ri) {
                            queue.push(ri);
                        }
                    }
                }
            }
            let mut closure_indices: Vec<usize> = closure.iter().copied().collect();
            closure_indices.sort_unstable();
            let stubs: Vec<Json> = closure_indices
                .iter()
                .map(|&idx| {
                    let f = &ordered[idx].2;
                    let mut doc = json!({
                        "id": &*f.id,
                        "writer": &*f.writer,
                        "seq": f.seq,
                        "writtenAt": f.written_at(),
                    });
                    if let Some(target) = f.corrects() {
                        doc["corrects"] = json!(target);
                    }
                    if f.has_void() {
                        doc["void"] = json!({"reason": "void"});
                    }
                    if let Some(rev) = f.review() {
                        doc["review"] = json!({"of": &*rev.orig_of});
                    }
                    json!({ "idx": idx, "doc": doc })
                })
                .collect();
            let res = crate::run_corrections::resolve(&stubs, |v| v.as_str().and_then(run_schema::iso_ms));
            let mut effective_in_closure: HashMap<usize, Option<String>> = HashMap::with_capacity(res.rows.len());
            for row in &res.rows {
                let idx = row["idx"].as_u64().unwrap_or(0) as usize;
                effective_in_closure.insert(idx, row["doc"]["review"]["of"].as_str().map(str::to_string));
            }

            let mut rows = Vec::with_capacity(ordered.len());
            let mut accepted = Vec::with_capacity(ordered.len());
            let mut problems = Vec::new();
            let mut active_retargeted: HashSet<(u32, Box<str>)> = HashSet::new();

            for (idx, (r, sig, base_fact)) in ordered.into_iter().enumerate() {
                if !closure.contains(&idx) {
                    accepted.push(Arc::clone(&base_fact));
                    rows.push(base_fact);
                    continue;
                }
                if let Some(reason) = res.refused.get(&*base_fact.id) {
                    problems.push(json!({
                        "file": &*base_fact.file,
                        "reason": reason,
                        "since": base_fact.first_seen_at,
                    }));
                    continue;
                }
                match effective_in_closure.get(&idx) {
                    Some(new_of_opt) => {
                        let effective_fact = if let (Some(rev), Some(new_of)) = (base_fact.review(), new_of_opt)
                            && new_of.as_str() != &*rev.orig_of
                        {
                            let rt_key: (u32, Box<str>) = (r, new_of.as_str().into());
                            active_retargeted.insert(rt_key.clone());
                            if let Some((prev_sig, prev_arc)) = cache.retargeted.get(&rt_key)
                                && *prev_sig == sig
                            {
                                Arc::clone(prev_arc)
                            } else {
                                let built = Arc::new(base_fact.with_effective_review_of(new_of));
                                cache.retargeted.insert(rt_key, (sig, Arc::clone(&built)));
                                built
                            }
                        } else {
                            base_fact
                        };
                        accepted.push(Arc::clone(&effective_fact));
                        rows.push(effective_fact);
                    }
                    None => {
                        // Non-refused but superseded or voided: kept in `accepted` receipts only.
                        accepted.push(base_fact);
                    }
                }
            }
            cache.retargeted.retain(|k, _| active_retargeted.contains(k));
            (rows, problems, accepted)
        };

        let snapshot = Arc::new(ResolvedHistory {
            key,
            rows,
            problems,
            skipped,
            accepted,
        });
        cache.resolutions += 1;
        cache.snapshot = Some(Arc::clone(&snapshot));
        cache.intern.sweep();
        Ok(snapshot)
    })
}

/// Effective leaves resolved before range/task filters. Pages share a resolved generation;
/// seeking after the cursor avoids rescanning the prefix. History/original APIs keep raw records.
/// Every page names its snapshot (generation + catalog hash), so a paged reader can restart
/// when a commit lands between pages instead of mixing two snapshots (recheck L1).
pub fn query(engine: &Engine, args: &QueryArgs) -> Result<Json> {
    if let Some(mut page) = query_uncorrected(engine, args)? {
        if args.include_cursors != Some(true) {
            hide_cursors(&mut page);
        }
        return Ok(page);
    }
    let limit = args.limit.unwrap_or(5000).clamp(1, 20_000);
    for _attempt in 0..8 {
        let snapshot = resolved_history(engine)?;
        let start = snapshot.rows.partition_point(|row| {
            args.after.is_some_and(|a| row.cursor <= a) || args.from_ms.is_some_and(|a| row.cursor.0 < a)
        });
        let mut picked: Vec<&RunFact> = Vec::new();
        let mut next = Json::Null;
        let mut last = Json::Null;
        for row in snapshot.rows.iter().skip(start) {
            let cursor = row.cursor;
            if args.to_ms.is_some_and(|a| cursor.0 > a) {
                break;
            }
            if args.source.as_deref().is_some_and(|a| row.source_str() != a)
                || args
                    .task_type
                    .as_deref()
                    .is_some_and(|a| row.task_type.as_deref() != Some(a))
                || (args.reviews_only == Some(true) && !row.has_review())
            {
                continue;
            }
            if picked.len() == limit {
                next = last;
                break;
            }
            last = json!(cursor);
            picked.push(row);
        }
        engine.fire_before_query_meta_read_hook();
        let want_gen = snapshot.key.0;
        let want_cat = snapshot
            .key
            .1
            .as_ref()
            .map(|bytes| String::from_utf8_lossy(bytes).into_owned());
        let rows_opt: Option<Vec<Json>> = engine.with_db(|db| {
            let read = db.begin_read()?;
            let (cur_gen, cur_cat) = snapshot_key(&read)?;
            if cur_gen != want_gen || cur_cat != want_cat {
                return Ok(None);
            }
            let heads = read.open_table(RUN_HEAD)?;
            let payloads = read.open_table(RUN_PAYLOAD)?;
            let dict_table = read.open_table(RUN_DICT)?;
            let mut dict = Dict::new(&dict_table);
            let mut blocks: HashMap<(u32, u32), Vec<Vec<u8>>> = HashMap::new();
            let mut rows_out = Vec::with_capacity(picked.len());
            for row in picked {
                let (ended, r) = row.cursor;
                let Some(value) = heads.get(Key::new().u32(r).as_slice())? else {
                    fail!("corrupt", "missing run head for {}", row.id);
                };
                let head = Head::decode(value.value())?;
                let identity = identity_json(&mut dict, &head)?;
                let ids = ids_json(&mut dict, r, &head)?;
                let Some(doc) = load_run_doc_in_read(&payloads, &mut blocks, row)? else {
                    fail!("corrupt", "missing run payload for {}", row.id);
                };
                let mut obj = serde_json::Map::new();
                if args.include_cursors == Some(true) {
                    obj.insert("_cursor".into(), json!([ended, r]));
                }
                obj.insert("file".into(), json!(&*row.file));
                obj.insert("sha256".into(), json!(row.sha256_hex()));
                obj.insert("firstSeenAt".into(), json!(row.first_seen_at));
                obj.insert("doc".into(), doc);
                obj.insert("identity".into(), identity);
                obj.insert("ids".into(), ids);
                rows_out.push(Json::Object(obj));
            }
            Ok(Some(rows_out))
        })?;
        let Some(out) = rows_opt else {
            continue;
        };
        let problems = if args.after.is_none() {
            snapshot.problems.clone()
        } else {
            Vec::new()
        };
        let mut page = json!({
            "runs":out,"next":next,"skipped":snapshot.skipped,"problems":problems,
            "correctionsResolved":true,"generation":want_gen,"catalogHash":want_cat
        });
        if args.include_receipts == Some(true) && args.after.is_none() {
            let accepted_json: Vec<Json> = snapshot
                .accepted
                .iter()
                .map(|r| json!({"file": &*r.file, "sha256": r.sha256_hex(), "firstSeenAt": r.first_seen_at, "doc": {"id": &*r.id, "seq": r.seq}}))
                .collect();
            page["accepted"] = Json::Array(accepted_json);
        }
        return Ok(page);
    }
    fail!("stale", "run history changed while reading page")
}

/// The snapshot a page belongs to: the runs ledger generation and the catalog hash, read in
/// the page's own transaction. The corrected path takes them from its resolved snapshot key.
fn snapshot_key(read: &redb::ReadTransaction) -> Result<(u64, Option<String>)> {
    let checkpoint = read.open_table(crate::db::CHECKPOINT)?;
    let generation = crate::db::read_checkpoint(&checkpoint, "runs/ledger")?
        .map(|c| c.last_sequence_id)
        .unwrap_or(0);
    let catalog = read
        .open_table(RUN_META)?
        .get("catalog")?
        .map(|v| String::from_utf8_lossy(v.value()).into_owned());
    Ok((generation, catalog))
}

fn query_uncorrected(engine: &Engine, args: &QueryArgs) -> Result<Option<Json>> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        // Admission and decoding share one transaction, even if the first correction is ingested now.
        if read.open_table(RUN_META)?.get("corrections")?.is_some() {
            return Ok(None);
        }
        let mut page = query_snapshot(&read, args)?;
        let (generation, catalog) = snapshot_key(&read)?;
        page["generation"] = json!(generation);
        page["catalogHash"] = json!(catalog);
        Ok(Some(page))
    })
}

fn query_snapshot(read: &redb::ReadTransaction, args: &QueryArgs) -> Result<Json> {
    query_snapshot_inner(read, args, true)
}

fn query_snapshot_inner(read: &redb::ReadTransaction, args: &QueryArgs, include_meta: bool) -> Result<Json> {
    let limit = args.limit.unwrap_or(5000).clamp(1, 20_000);
    let by_time = read.open_table(RUN_BY_TIME)?;
    let heads = read.open_table(RUN_HEAD)?;
    let payloads = read.open_table(RUN_PAYLOAD)?;
    let dict_table = read.open_table(RUN_DICT)?;
    let rev_table = read.open_table(RUN_DICT_REV)?;
    let mut dict = Dict::new(&dict_table);
    let task_type = match &args.task_type {
        Some(t) => match lookup_id(&rev_table, kind::TASK_TYPE, t)? {
            Some(id) => Some(id),
            None => return Ok(json!({ "runs": [], "next": null, "skipped": 0 })),
        },
        None => None,
    };
    let lo = match args.after {
        Some((t, r)) => Key::new().u64(t).u32(r.saturating_add(1)),
        None => Key::new().u64(args.from_ms.unwrap_or(0)).u32(0),
    };
    let hi = Key::new().u64(args.to_ms.unwrap_or(u64::MAX - 1)).u32(u32::MAX);
    if lo.as_slice() > hi.as_slice() {
        return Ok(json!({ "runs": [], "next": null, "skipped": 0 }));
    }
    let mut blocks: HashMap<(u32, u32), Vec<Vec<u8>>> = HashMap::new();
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut next = Json::Null;
    // The cursor is the last run returned, so the next page starts just after it.
    let mut last: Option<(u64, u32)> = None;
    for item in by_time.range(lo.as_slice()..=hi.as_slice())? {
        let (k, _) = item?;
        let (ended, r) = (be_u64(k.value(), 0), be_u32(k.value(), 8));
        let Some(value) = heads.get(Key::new().u32(r).as_slice())? else {
            continue;
        };
        let head = Head::decode(value.value())?;
        let benchmark = head.flags & F_BENCHMARK != 0;
        match args.source.as_deref() {
            Some("observed") if benchmark => continue,
            Some("benchmark") if !benchmark => continue,
            _ => {}
        }
        if task_type.is_some_and(|t| t != head.id(at::TASK_TYPE)) {
            continue;
        }
        if args.reviews_only == Some(true) && head.flags & F_REVIEW == 0 {
            continue;
        }
        if out.len() == limit {
            next = last.map(|(t, r)| json!([t, r])).unwrap_or(Json::Null);
            break;
        }
        let block_key = (head.loc.day, head.loc.block);
        if !blocks.contains_key(&block_key) {
            if blocks.len() >= 16 {
                blocks.clear();
            }
            let Some(bytes) = payloads.get(Key::new().u32(head.loc.day).u32(head.loc.block).as_slice())? else {
                skipped += 1;
                continue;
            };
            blocks.insert(block_key, block_decode(bytes.value())?.1);
        }
        let Some(doc) = blocks[&block_key].get(head.loc.slot as usize) else {
            skipped += 1;
            continue;
        };
        let writer = dict.field(&head, at::WRITER)?.unwrap_or_default();
        let text = String::from_utf8_lossy(doc);
        // Stored text was accepted at ingest; a projection that now fails is counted, not shown.
        let Ok(mut run) = run_schema::parse_stored_run_text(&text, &writer) else {
            skipped += 1;
            continue;
        };
        if include_meta {
            let identity = identity_json(&mut dict, &head)?;
            let shown = identity["modelId"]
                .as_str()
                .or(identity["modelRaw"].as_str())
                .map(str::to_string);
            run["model"]["id"] = opt_json(shown);
            run["model"]["effort"] = identity["effort"].clone();
            run["model"]["harness"] = identity["harness"].clone();
            run["model"]["provider"] = Json::Null;
            out.push(json!({
                "_cursor": [ended, r],
                "file": dict.field(&head, at::PATH)?,
                "sha256": hex(&head.sha),
                "firstSeenAt": head.ingested_ms,
                "doc": run,
                "identity": identity,
                "ids": ids_json(&mut dict, r, &head)?,
            }));
        } else {
            let shown = dict.field(&head, at::MODEL)?.or(dict.field(&head, at::MODEL_RAW)?);
            run["model"]["id"] = opt_json(shown);
            run["model"]["effort"] = opt_json(dict.field(&head, at::EFFORT)?);
            run["model"]["harness"] = opt_json(dict.field(&head, at::HARNESS)?);
            run["model"]["provider"] = Json::Null;
            out.push(json!({
                "_cursor": [ended, r],
                "file": dict.field(&head, at::PATH)?,
                "sha256": hex(&head.sha),
                "firstSeenAt": head.ingested_ms,
                "doc": run,
            }));
        }
        last = Some((ended, r));
    }
    Ok(json!({ "runs": out, "next": next, "skipped": skipped }))
}

/// The original text of a run's current revision (or of the revision with `sha256`).
pub fn original(engine: &Engine, run_id: &str, sha: Option<&str>) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let rev_table = read.open_table(RUN_DICT_REV)?;
        let Some(r) = lookup_id(&rev_table, kind::RUN_ID, run_id)? else {
            return Ok(Json::Null);
        };
        let loc = match sha.and_then(unhex32) {
            Some(want) => {
                let table = read.open_table(RUN_REV)?;
                let lo = Key::new().u32(r).bytes(&[0, 0]);
                let hi = Key::new().u32(r).bytes(&[0xff, 0xff]);
                let mut found = None;
                for item in table.range(lo.as_slice()..=hi.as_slice())? {
                    let row = Head::decode(item?.1.value())?;
                    if row.sha == want {
                        found = Some(row.loc);
                    }
                }
                found
            }
            None => {
                let heads = read.open_table(RUN_HEAD)?;
                let value = heads.get(Key::new().u32(r).as_slice())?;
                value.map(|v| Head::decode(v.value())).transpose()?.map(|h| h.loc)
            }
        };
        let Some(loc) = loc else { return Ok(Json::Null) };
        let payloads = read.open_table(RUN_PAYLOAD)?;
        let Some(bytes) = payloads.get(Key::new().u32(loc.day).u32(loc.block).as_slice())? else {
            return Ok(Json::Null);
        };
        let docs = block_decode(bytes.value())?.1;
        Ok(docs
            .get(loc.slot as usize)
            .map(|d| Json::String(String::from_utf8_lossy(d).into_owned()))
            .unwrap_or(Json::Null))
    })
}

/// Every revision of one run, oldest first.
pub fn history(engine: &Engine, run_id: &str) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let rev_table = read.open_table(RUN_DICT_REV)?;
        let Some(r) = lookup_id(&rev_table, kind::RUN_ID, run_id)? else {
            return Ok(json!([]));
        };
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let table = read.open_table(RUN_REV)?;
        let lo = Key::new().u32(r).bytes(&[0, 0]);
        let hi = Key::new().u32(r).bytes(&[0xff, 0xff]);
        let mut out = Vec::new();
        for item in table.range(lo.as_slice()..=hi.as_slice())? {
            let row = Head::decode(item?.1.value())?;
            out.push(json!({
                "sha256": hex(&row.sha), "seq": row.seq, "writtenAtMs": row.written_ms,
                "ingestedAtMs": row.ingested_ms, "eventSeq": row.event_seq,
                "path": dict.field(&row, at::PATH)?, "catalogHash": dict.field(&row, at::CATALOG)?,
            }));
        }
        Ok(Json::Array(out))
    })
}

/// Refused files on record: the path's hash, the reason and when it was refused. No path, file
/// name or writer is kept for a refused file.
pub fn refused(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let files = read.open_table(RUN_FILE)?;
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let mut out = Vec::new();
        for item in files.iter()? {
            let (k, v) = item?;
            let row = FileRow::decode(v.value())?;
            if row.state != 2 {
                continue;
            }
            out.push(json!({
                "pathHash": hex(k.value()),
                "reason": dict.text(kind::REASON, row.reason)?,
                "size": row.size,
                "mtimeMs": row.mtime_ms,
                "atMs": row.at_ms,
            }));
        }
        out.sort_by(|a, b| a["pathHash"].as_str().cmp(&b["pathHash"].as_str()));
        Ok(Json::Array(out))
    })
}

/// The page's identity facts under the current catalog, read from an existing transaction.
pub(crate) fn identity_table_in_read(read: &redb::ReadTransaction) -> Result<Json> {
    let catalog = current_catalog_in_read(read)?.unwrap_or_default();
    let mut families: BTreeMap<String, run_identity::Family> = BTreeMap::new();
    let mut models = serde_json::Map::new();
    for (i, m) in catalog.models.iter().enumerate() {
        let family = run_identity::family_of(&catalog, &m.model);
        if let Some(f) = &family {
            families.entry(f.id.clone()).or_insert_with(|| f.clone());
        }
        models.insert(
            m.model.clone(),
            json!({ "family": family.map(|f| f.id), "order": i, "label": m.label }),
        );
    }
    let mut list: Vec<run_identity::Family> = families.into_values().collect();
    list.sort_by(|a, b| a.order.cmp(&b.order).then(a.label.cmp(&b.label)));
    for (i, f) in list.iter_mut().enumerate() {
        f.order = i as u32 + 1;
    }
    Ok(json!({
        "catalogHash": catalog.hash,
        "families": list,
        "models": models,
        "efforts": run_identity::EFFORT_ORDER,
    }))
}

/// The page's identity facts under the current catalog: families in order, each catalog
/// model's family and order, and the effort order.
pub fn identity_table(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        identity_table_in_read(&read)
    })
}

/// Every distinct written model, effort and harness with counts and what each maps to, the
/// unmapped strings and the data-quality counts (DESIGN.md 1.1 to 1.5).
pub fn audit(engine: &Engine) -> Result<Json> {
    for _attempt in 0..8 {
        let correction_history = has_corrections(engine)?;
        let history = if correction_history {
            Some(resolved_history(engine)?)
        } else {
            None
        };
        let effective: std::collections::HashSet<&str> = history
            .iter()
            .flat_map(|h| &h.rows)
            .filter(|r| !r.id.is_empty())
            .map(|r| r.id.as_ref())
            .collect();
        let want_key = history.as_ref().map(|h| (h.generation(), h.catalog_hash()));
        let res = engine.with_db(|db| {
            let read = db.begin_read()?;
            let cur_corr = read.open_table(RUN_META)?.get("corrections")?.is_some();
            if cur_corr != correction_history {
                return Ok(None);
            }
            if let Some(want) = &want_key
                && snapshot_key(&read)? != *want
            {
                return Ok(None);
            }
            let heads = read.open_table(RUN_HEAD)?;
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let mut models: BTreeMap<(Option<String>, Option<String>, Option<String>), u64> = BTreeMap::new();
        let mut efforts: BTreeMap<(Option<String>, Option<String>, Option<String>), u64> = BTreeMap::new();
        let mut harnesses: BTreeMap<(Option<String>, Option<String>, Option<String>, Option<String>), u64> = BTreeMap::new();
        let mut unmapped: BTreeMap<String, u64> = BTreeMap::new();
        let mut quality: BTreeMap<&'static str, u64> = BTreeMap::new();
        let mut total = 0u64;
        for item in heads.iter()? {
            let (key, value) = item?;
            let head = &Head::decode(value.value())?;
            let id = dict.text(kind::RUN_ID, be_u32(key.value(), 0))?;
            if correction_history && !id.as_deref().is_some_and(|id| effective.contains(id)) { continue; }
            total += 1;
            let model_raw = dict.field(head, at::MODEL_RAW)?;
            let model = dict.field(head, at::MODEL)?;
            let model_source = dict.field(head, at::MODEL_SOURCE)?;
            let family = dict.field(head, at::FAMILY)?;
            *models.entry((model_raw.clone(), model.clone(), model_source.clone())).or_default() += 1;
            if model_source.as_deref() == Some("unmapped")
                && let Some(raw) = &model_raw
            {
                *unmapped.entry(raw.clone()).or_default() += 1;
            }
            *efforts
                .entry((
                    dict.field(head, at::EFFORT_RAW)?,
                    dict.field(head, at::EFFORT)?,
                    dict.field(head, at::EFFORT_SOURCE)?,
                ))
                .or_default() += 1;
            let harness_source = dict.field(head, at::HARNESS_SOURCE)?;
            *harnesses
                .entry((
                    dict.field(head, at::HARNESS_RAW)?,
                    family.clone(),
                    dict.field(head, at::HARNESS)?,
                    harness_source.clone(),
                ))
                .or_default() += 1;
            let mut note = |k: &'static str, on: bool| {
                if on {
                    *quality.entry(k).or_default() += 1;
                }
            };
            note("noModel", model_raw.is_none());
            note("noTaskType", head.id(at::TASK_TYPE) == 0);
            note("harnessCorrected", harness_source.as_deref() == Some("corrected"));
            note("harnessInferred", harness_source.as_deref() == Some("inferred"));
            note("harnessUnknown", harness_source.as_deref() == Some("unknown"));
            note("effortConflict", head.flags & F_EFFORT_CONFLICT != 0);
            note("effortUnrecognized", dict.field(head, at::EFFORT_SOURCE)?.as_deref() == Some("unrecognized"));
            note("noStartedAt", head.started_ms == UNKNOWN);
            note("noWallSeconds", head.wall_ms == UNKNOWN);
            note("imported", head.flags & F_IMPORTED != 0);
            note("multipleRevisions", head.revisions > 1);
            note("reviewTypeWithoutReview", dict.field(head, at::TASK_TYPE)?.as_deref() == Some("review") && head.flags & F_REVIEW == 0);
        }
        let rows = |m: BTreeMap<(Option<String>, Option<String>, Option<String>), u64>, names: [&str; 3]| -> Vec<Json> {
            let mut v: Vec<Json> = m
                .into_iter()
                .map(|((a, b, c), n)| json!({ names[0]: a, names[1]: b, names[2]: c, "runs": n }))
                .collect();
            v.sort_by(|a, b| b["runs"].as_u64().cmp(&a["runs"].as_u64()));
            v
        };
        let mut harness_rows: Vec<Json> = harnesses
            .into_iter()
            .map(|((raw, family, harness, source), n)| json!({ "harnessRaw": raw, "family": family, "harness": harness, "harnessSource": source, "runs": n }))
            .collect();
        harness_rows.sort_by(|a, b| b["runs"].as_u64().cmp(&a["runs"].as_u64()));
        let mut unmapped_rows: Vec<Json> = unmapped.into_iter().map(|(id, n)| json!({ "modelRaw": id, "runs": n })).collect();
        unmapped_rows.sort_by(|a, b| b["runs"].as_u64().cmp(&a["runs"].as_u64()));
            Ok(Some(json!({
                "runs": total,
                "models": rows(models, ["modelRaw", "modelId", "modelSource"]),
                "efforts": rows(efforts, ["effortRaw", "effort", "effortSource"]),
                "harnesses": harness_rows,
                "unmapped": unmapped_rows,
                "quality": quality,
            })))
        })?;
        if let Some(out) = res {
            return Ok(out);
        }
    }
    fail!("stale", "run history changed while auditing")
}

/// Counts and sizes of the run tables, the current catalog and the generation.
pub fn status(engine: &Engine) -> Result<Json> {
    let effective_count = if has_corrections(engine)? {
        let from_agg = engine.with_db(|db| {
            let read = db.begin_read()?;
            crate::scoreboard_agg::effective_run_count_in_read(&read)
        })?;
        match from_agg {
            Some(n) => Some(n),
            None => Some(resolved_history(engine)?.rows.len() as u64),
        }
    } else {
        None
    };
    let generation_seq = generation(engine)?;
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let mut payload_bytes = 0u64;
        let mut blocks = (0u64, 0u64);
        for item in read.open_table(RUN_PAYLOAD)?.iter()? {
            let (_, v) = item?;
            payload_bytes += v.value().len() as u64;
            if v.value().get(1) == Some(&CODEC_ZSTD) {
                blocks.0 += 1;
            } else {
                blocks.1 += 1;
            }
        }
        let mut refused = 0u64;
        for item in read.open_table(RUN_FILE)?.iter()? {
            if item?.1.value().get(1) == Some(&2) {
                refused += 1;
            }
        }
        let meta = read.open_table(RUN_META)?;
        Ok(json!({
            "runs": effective_count.unwrap_or(read.open_table(RUN_HEAD)?.len()?),
            "revisions": read.open_table(RUN_REV)?.len()?,
            "files": read.open_table(RUN_FILE)?.len()?,
            "refusedFiles": refused,
            "payloadBlocks": { "compressed": blocks.0, "open": blocks.1, "bytes": payload_bytes },
            "catalogs": read.open_table(RUN_CATALOG)?.len()?,
            "catalogHash": meta.get("catalog")?.map(|v| String::from_utf8_lossy(v.value()).into_owned()),
            "generation": generation_seq,
        }))
    })
}

/// Projector batches stay under the engine's cap.
const _: () = assert!(EVENTS_PER_COMMIT < PROJECTOR_BATCH);
