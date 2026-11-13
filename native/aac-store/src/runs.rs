//! Agent-report runs, kept forever (Scoreboard v2, share `2026-11-13-scoreboard-v2/DESIGN.md`).
//!
//! The share's run files are the ingest source; the ledger log is the record (`run.*` events,
//! never pruned) and the `run_*` tables are this projector's projections of it:
//!
//! | Table | Key | Value |
//! |---|---|---|
//! | `run_dict`, `run_dict_rev` | kind u8, id u32 / kind u8, text | this projector's own strings |
//! | `run_meta` | text | current catalog hash, dictionary counters |
//! | `run_head` | r u32 | the current revision's row (`Head`, binary v1) |
//! | `run_rev` | r u32, rev u16 | every revision (`Rev`, binary v1): dedupe by run id + content hash |
//! | `run_payload` | day u32, block u32 | the original documents, up to 32 per block (raw while open, zstd once full) |
//! | `run_by_time` | endedAt ms u64, r u32 | current revisions by end time (the page's date range) |
//! | `run_file` | sha256(path)[0..16] | the file's last stat and content hash (the scan's skip) |
//! | `run_catalog` | sha256 catalog[0..16] | each catalog version used (zstd) |
//!
//! Every key is fixed-width big-endian; values carry a leading version byte. Scanning, the
//! run@1 projection and normalization are here in Rust; the secret check stays the TypeScript
//! `jsonFromBytes` (owner decision 2026-11-09), which every text passes before `record`.

use crate::db::{Key, be_u32, be_u64};
use crate::engine::{DAY_MS, Engine, LogId, PROJECTOR_BATCH, Projector};
use crate::error::Result;
use crate::event::Event;
use crate::fail;
use crate::log::Finding;
use crate::msgpack;
use crate::run_identity::{self, Catalog, Identity, Written};
use crate::run_schema::{self, RUN_CAP_BYTES};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, WriteTransaction};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

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
pub const RUNS_PROJECTOR_VERSION: u32 = 1;
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
mod kind {
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
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Small helpers

pub fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex32(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, chunk) in text.as_bytes().chunks(2).enumerate() {
        out[i] = u8::from_str_radix(std::str::from_utf8(chunk).ok()?, 16).ok()?;
    }
    Some(out)
}

fn path_key(path: &str) -> [u8; 16] {
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

fn meta_get(txn: &WriteTransaction, key: &str) -> Result<Option<String>> {
    let table = txn.open_table(RUN_META)?;
    let value = table.get(key)?.map(|v| String::from_utf8_lossy(v.value()).into_owned());
    Ok(value)
}

fn meta_set(txn: &WriteTransaction, key: &str, value: &str) -> Result<()> {
    txn.open_table(RUN_META)?.insert(key, value.as_bytes())?;
    Ok(())
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

fn lookup_id<T: ReadableTable<&'static [u8], u32>>(rev: &T, kind: u8, text: &str) -> Result<Option<u32>> {
    Ok(rev
        .get(Key::new().u8(kind).bytes(text.as_bytes()).as_slice())?
        .map(|v| v.value()))
}

/// Reads dictionary text through a cache (queries read the same few strings many times).
struct Dict<'a, T: ReadableTable<&'static [u8], &'static [u8]>> {
    table: &'a T,
    cache: HashMap<(u8, u32), Option<String>>,
}

impl<'a, T: ReadableTable<&'static [u8], &'static [u8]>> Dict<'a, T> {
    fn new(table: &'a T) -> Self {
        Dict {
            table,
            cache: HashMap::new(),
        }
    }
    fn text(&mut self, kind: u8, id: u32) -> Result<Option<String>> {
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
    fn field(&mut self, head: &Head, at: usize) -> Result<Option<String>> {
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

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Loc {
    pub day: u32,
    pub block: u32,
    pub slot: u16,
}

/// The current revision of one run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Head {
    pub flags: u8,
    /// 0 none, 1 pass, 2 fail, 3 cannot-verify.
    pub verdict: u8,
    pub revisions: u16,
    pub seq: u32,
    pub written_ms: u64,
    pub ingested_ms: u64,
    pub started_ms: u64,
    pub ended_ms: u64,
    pub wall_ms: u64,
    pub sha: [u8; 32],
    pub loc: Loc,
    /// Dictionary ids in `HEAD_IDS` order.
    pub ids: [u32; 26],
}

const HEAD_LEN: usize = 1 + 1 + 1 + 2 + 4 + 8 * 5 + 32 + 4 + 4 + 2 + 26 * 4;

impl Head {
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEAD_LEN);
        out.push(1);
        out.push(self.flags);
        out.push(self.verdict);
        out.extend_from_slice(&self.revisions.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        for v in [self.written_ms, self.ingested_ms, self.started_ms, self.ended_ms, self.wall_ms] {
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
        if b.first() != Some(&1) || b.len() != HEAD_LEN {
            fail!("unsupported", "run head value version or length");
        }
        let mut ids = [0u32; 26];
        for (i, id) in ids.iter_mut().enumerate() {
            *id = be_u32(b, 91 + i * 4);
        }
        Ok(Head {
            flags: b[1],
            verdict: b[2],
            revisions: u16::from_be_bytes([b[3], b[4]]),
            seq: be_u32(b, 5),
            written_ms: be_u64(b, 9),
            ingested_ms: be_u64(b, 17),
            started_ms: be_u64(b, 25),
            ended_ms: be_u64(b, 33),
            wall_ms: be_u64(b, 41),
            sha: b[49..81].try_into().unwrap(),
            loc: Loc {
                day: be_u32(b, 81),
                block: be_u32(b, 85),
                slot: u16::from_be_bytes([b[89], b[90]]),
            },
            ids,
        })
    }

    fn id(&self, kind_at: usize) -> u32 {
        self.ids[kind_at]
    }
}

/// Index of a field in `HEAD_IDS`.
mod at {
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

/// One revision of a run (every distinct content AAC has seen under the run id).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Rev {
    pub sha: [u8; 32],
    pub seq: u32,
    pub written_ms: u64,
    pub ingested_ms: u64,
    pub event_seq: u64,
    pub loc: Loc,
    pub catalog: u32,
    pub path: u32,
}

const REV_LEN: usize = 1 + 32 + 4 + 8 * 3 + 4 + 4 + 2 + 4 + 4;

impl Rev {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(REV_LEN);
        out.push(1);
        out.extend_from_slice(&self.sha);
        out.extend_from_slice(&self.seq.to_be_bytes());
        for v in [self.written_ms, self.ingested_ms, self.event_seq] {
            out.extend_from_slice(&v.to_be_bytes());
        }
        out.extend_from_slice(&self.loc.day.to_be_bytes());
        out.extend_from_slice(&self.loc.block.to_be_bytes());
        out.extend_from_slice(&self.loc.slot.to_be_bytes());
        out.extend_from_slice(&self.catalog.to_be_bytes());
        out.extend_from_slice(&self.path.to_be_bytes());
        out
    }

    fn decode(b: &[u8]) -> Result<Rev> {
        if b.first() != Some(&1) || b.len() != REV_LEN {
            fail!("unsupported", "run revision value version or length");
        }
        Ok(Rev {
            sha: b[1..33].try_into().unwrap(),
            seq: be_u32(b, 33),
            written_ms: be_u64(b, 37),
            ingested_ms: be_u64(b, 45),
            event_seq: be_u64(b, 53),
            loc: Loc {
                day: be_u32(b, 61),
                block: be_u32(b, 65),
                slot: u16::from_be_bytes([b[69], b[70]]),
            },
            catalog: be_u32(b, 71),
            path: be_u32(b, 75),
        })
    }
}

/// A file's last stat: the scan skips it while size and mtime stay the same.
#[derive(Clone, Debug, Default, PartialEq)]
struct FileRow {
    /// 1 stored, 2 refused.
    state: u8,
    size: u64,
    mtime_ms: u64,
    sha: [u8; 32],
    run: u32,
    reason: u32,
}

const FILE_LEN: usize = 1 + 1 + 8 + 8 + 32 + 4 + 4;

impl FileRow {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(FILE_LEN);
        out.push(1);
        out.push(self.state);
        out.extend_from_slice(&self.size.to_be_bytes());
        out.extend_from_slice(&self.mtime_ms.to_be_bytes());
        out.extend_from_slice(&self.sha);
        out.extend_from_slice(&self.run.to_be_bytes());
        out.extend_from_slice(&self.reason.to_be_bytes());
        out
    }

    fn decode(b: &[u8]) -> Result<FileRow> {
        if b.first() != Some(&1) || b.len() != FILE_LEN {
            fail!("unsupported", "run file value version or length");
        }
        Ok(FileRow {
            state: b[1],
            size: be_u64(b, 2),
            mtime_ms: be_u64(b, 10),
            sha: b[18..50].try_into().unwrap(),
            run: be_u32(b, 50),
            reason: be_u32(b, 54),
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

fn block_decode(b: &[u8]) -> Result<(bool, Vec<Vec<u8>>)> {
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
            put(txn, RUN_PAYLOAD, Key::new().u32(day).u32(*block).as_slice(), &block_encode(&docs, full)?)?;
            return Ok(Loc { day, block: *block, slot });
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

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct RefusedPayload {
    path: String,
    size: u64,
    mtime_ms: u64,
    sha256: Option<String>,
    reason: String,
    ingested_at_ms: u64,
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
        let payload = || msgpack::to_json(&event.payload);
        match event.event_type.as_str() {
            "run.catalog" => project_catalog(txn, &serde_json::from_value(payload())?)?,
            "run.recorded" => project_recorded(txn, &serde_json::from_value(payload())?, event.sequence_id)?,
            "run.identity" => project_identity(txn, &serde_json::from_value(payload())?)?,
            "run.refused" => project_refused(txn, &serde_json::from_value(payload())?)?,
            _ => {}
        }
        Ok(Vec::new())
    }
    fn reset(&self, txn: &WriteTransaction) -> Result<()> {
        for def in [RUN_HEAD, RUN_REV, RUN_PAYLOAD, RUN_FILE, RUN_CATALOG, RUN_DICT] {
            txn.delete_table(def)?;
        }
        txn.delete_table(RUN_DICT_REV)?;
        txn.delete_table(RUN_BY_TIME)?;
        txn.delete_table(RUN_META)?;
        create_tables(txn)
    }
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

/// Which copy of a run wins: the highest seq, then the later writtenAt, then the later path.
fn newer(a: (u64, u64, &str), b: (u64, u64, &str)) -> bool {
    a.0.cmp(&b.0).then(a.1.cmp(&b.1)).then(a.2.cmp(b.2)) == std::cmp::Ordering::Greater
}

fn revisions(txn: &WriteTransaction, r: u32) -> Result<Vec<Rev>> {
    let table = txn.open_table(RUN_REV)?;
    let lo = Key::new().u32(r).bytes(&[0, 0]);
    let hi = Key::new().u32(r).bytes(&[0xff, 0xff]);
    let mut out = Vec::new();
    for item in table.range(lo.as_slice()..=hi.as_slice())? {
        out.push(Rev::decode(item?.1.value())?);
    }
    Ok(out)
}

fn project_recorded(txn: &WriteTransaction, p: &RecordedPayload, event_seq: u64) -> Result<()> {
    let Some(sha) = unhex32(&p.sha256) else {
        fail!("invalid", "run sha256");
    };
    let r = intern(txn, kind::RUN_ID, Some(&p.ids.run_id))?;
    let revs = revisions(txn, r)?;
    // Idempotent: a content already recorded under this run id is not a new revision.
    if revs.iter().any(|rev| rev.sha == sha) {
        return Ok(());
    }
    let loc = append_doc(txn, (p.ingested_at_ms / DAY_MS) as u32, p.doc.as_bytes())?;
    let path = intern(txn, kind::PATH, Some(&p.path))?;
    let catalog = intern(txn, kind::CATALOG, Some(&p.identity.model_catalog_hash))?;
    let rev = Rev {
        sha,
        seq: p.ids.seq.min(u32::MAX as u64) as u32,
        written_ms: p.ids.observed_at_ms,
        ingested_ms: p.ingested_at_ms,
        event_seq,
        loc: loc.clone(),
        catalog,
        path,
    };
    put(
        txn,
        RUN_REV,
        Key::new().u32(r).bytes(&(revs.len() as u16).to_be_bytes()).as_slice(),
        &rev.encode(),
    )?;
    put(
        txn,
        RUN_FILE,
        &path_key(&p.path),
        &FileRow {
            state: 1,
            size: p.size,
            mtime_ms: p.mtime_ms,
            sha,
            run: r,
            reason: 0,
        }
        .encode(),
    )?;
    let held = get(txn, RUN_HEAD, &Key::new().u32(r).0)?.map(|b| Head::decode(&b)).transpose()?;
    let mut dict_paths: HashMap<u32, String> = HashMap::new();
    if let Some(h) = &held {
        let table = txn.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&table);
        if let Some(text) = dict.text(kind::PATH, h.id(at::PATH))? {
            dict_paths.insert(h.id(at::PATH), text);
        }
    }
    let wins = match &held {
        None => true,
        Some(h) => newer(
            (p.ids.seq, p.ids.observed_at_ms, &p.path),
            (
                h.seq as u64,
                h.written_ms,
                dict_paths.get(&h.id(at::PATH)).map(String::as_str).unwrap_or(""),
            ),
        ),
    };
    let revisions = (revs.len() + 1).min(u16::MAX as usize) as u16;
    if !wins {
        let mut h = held.expect("a losing revision has a head");
        h.revisions = revisions;
        put(txn, RUN_HEAD, &Key::new().u32(r).0, &h.encode())?;
        return Ok(());
    }
    if let Some(h) = &held {
        txn.open_table(RUN_BY_TIME)?
            .remove(Key::new().u64(h.ended_ms).u32(r).as_slice())?;
    }
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
    let mut head = Head {
        flags,
        verdict: verdict_code(ids.review_verdict.as_deref()),
        revisions,
        seq: rev.seq,
        written_ms: ids.observed_at_ms,
        ingested_ms: p.ingested_at_ms,
        started_ms: ids.started_at_ms.unwrap_or(UNKNOWN),
        ended_ms: ids.event_at_ms,
        wall_ms: ids.wall_ms.unwrap_or(UNKNOWN),
        sha,
        loc,
        ids: [0; 26],
    };
    head.ids[at::WRITER] = intern(txn, kind::WRITER, Some(&ids.writer))?;
    head.ids[at::MACHINE] = intern(txn, kind::MACHINE, Some(&ids.machine))?;
    head.ids[at::COMPUTER] = intern(txn, kind::COMPUTER, ids.computer.as_deref())?;
    head.ids[at::T3_THREAD] = intern(txn, kind::T3_THREAD, ids.t3_thread_id.as_deref())?;
    head.ids[at::T3_TASK] = intern(txn, kind::T3_TASK, ids.t3_task_id.as_deref())?;
    head.ids[at::TASK_ID] = intern(txn, kind::TASK_ID, Some(&ids.task_id))?;
    head.ids[at::TASK_TYPE] = intern(txn, kind::TASK_TYPE, ids.task_type.as_deref())?;
    head.ids[at::PROJECT] = intern(txn, kind::PROJECT, ids.project_id.as_deref())?;
    head.ids[at::STREAM] = intern(txn, kind::STREAM, ids.stream_id.as_deref())?;
    head.ids[at::OUTCOME] = intern(txn, kind::OUTCOME, Some(&ids.outcome))?;
    head.ids[at::BENCHMARK] = intern(txn, kind::BENCHMARK, ids.benchmark.as_deref())?;
    head.ids[at::PROVIDER] = intern(txn, kind::PROVIDER, ids.provider.as_deref())?;
    head.ids[at::ACCOUNT] = intern(txn, kind::ACCOUNT, ids.account_raw.as_deref())?;
    head.ids[at::PATH] = path;
    head.ids[at::T3_THREAD_SOURCE] = intern(txn, kind::WORD, ids.t3_thread_source.as_deref())?;
    identity_ids(txn, &mut head.ids, &p.identity)?;
    put(txn, RUN_HEAD, &Key::new().u32(r).0, &head.encode())?;
    txn.open_table(RUN_BY_TIME)?
        .insert(Key::new().u64(head.ended_ms).u32(r).as_slice(), ())?;
    Ok(())
}

fn project_identity(txn: &WriteTransaction, p: &IdentityEventPayload) -> Result<()> {
    let Some(sha) = unhex32(&p.sha256) else {
        fail!("invalid", "run sha256");
    };
    let r = {
        let rev = txn.open_table(RUN_DICT_REV)?;
        lookup_id(&rev, kind::RUN_ID, &p.run_id)?
    };
    let Some(r) = r else { return Ok(()) };
    let Some(bytes) = get(txn, RUN_HEAD, &Key::new().u32(r).0)? else {
        return Ok(());
    };
    let mut head = Head::decode(&bytes)?;
    // A re-keying names the revision it was computed for; a head that has moved on keeps its own.
    if head.sha != sha {
        return Ok(());
    }
    identity_ids(txn, &mut head.ids, &p.identity)?;
    if p.identity.effort_conflict {
        head.flags |= F_EFFORT_CONFLICT;
    } else {
        head.flags &= !F_EFFORT_CONFLICT;
    }
    put(txn, RUN_HEAD, &Key::new().u32(r).0, &head.encode())
}

fn project_refused(txn: &WriteTransaction, p: &RefusedPayload) -> Result<()> {
    let reason = intern(txn, kind::REASON, Some(&p.reason))?;
    intern(txn, kind::PATH, Some(&p.path))?;
    put(
        txn,
        RUN_FILE,
        &path_key(&p.path),
        &FileRow {
            state: 2,
            size: p.size,
            mtime_ms: p.mtime_ms,
            sha: p.sha256.as_deref().and_then(unhex32).unwrap_or([0; 32]),
            run: 0,
            reason,
        }
        .encode(),
    )
}

// ---------------------------------------------------------------------------------------------
// Computers (the package's registry; never a caller's alias)

static COMPUTERS: LazyLock<Json> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../web-dashboard/public/computers.json")).expect("packaged computer registry")
});

/// Machine ids a writer name may start with.
pub fn machine_ids() -> Vec<String> {
    COMPUTERS
        .as_object()
        .map(|m| m.values().filter_map(|r| r["machineId"].as_str().map(str::to_string)).collect())
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
        && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
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
            let state = if e.kind() == std::io::ErrorKind::NotFound { "missing" } else { "error" };
            return Ok(json!({ "state": state, "files": [], "refused": [], "threads": {}, "catalog": catalog_texts(&root), "listed": 0, "unchanged": 0, "more": false, "problems": [] }));
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
        let Ok(days) = fs::read_dir(&runs_dir) else { continue };
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
                if file_name.starts_with('.') || !file_name.ends_with(".json") || !run_schema::run_file_name_readable(&file_name) {
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
    /// Writer name to the text of its `thread.json` (already through the secret filter).
    #[serde(default)]
    pub threads: BTreeMap<String, String>,
    #[serde(default)]
    pub catalog: CatalogIn,
    pub now_ms: u64,
}

/// The catalog stored as current, if any.
fn current_catalog(engine: &Engine) -> Result<Option<Catalog>> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let meta = read.open_table(RUN_META)?;
        let Some(hash) = meta.get("catalog")?.map(|v| String::from_utf8_lossy(v.value()).into_owned()) else {
            return Ok(None);
        };
        let Some(key) = unhex32(&hash) else { return Ok(None) };
        let table = read.open_table(RUN_CATALOG)?;
        let Some(value) = table.get(&key[..16])? else { return Ok(None) };
        let bytes = value.value();
        if bytes.first() != Some(&1) || bytes.len() < 9 {
            return Ok(None);
        }
        let texts: Json = msgpack::from_bytes(&zstd::stream::decode_all(&bytes[9..])?)?;
        Ok(Catalog::from_texts(texts["models"].as_str(), texts["aliases"].as_str()))
    })
}

/// The catalog to normalize with: each file as given when it parses, else its last stored
/// version, so a damaged write never re-keys history.
fn choose_catalog(engine: &Engine, given: &CatalogIn) -> Result<Catalog> {
    let stored = current_catalog(engine)?;
    let models_ok = given.models.as_deref().is_some_and(|t| run_identity::parse_models(t).is_some());
    let aliases_ok = given.aliases.as_deref().is_some_and(|t| run_identity::parse_aliases(t).is_some());
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

fn thread_facts(text: &str, writer: &str) -> ThreadFacts {
    let Ok(doc) = serde_json::from_str::<Json>(text) else {
        return ThreadFacts::default();
    };
    if doc["schema"].as_str() != Some("agent-reports/thread@1") || doc["writer"].as_str() != Some(writer) {
        return ThreadFacts::default();
    }
    let clean = |v: &Json| {
        v.as_str()
            .map(str::trim)
            .filter(|s| !s.is_empty() && s.len() <= 1024 && !s.chars().any(char::is_control))
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
        wall_ms: run["wallSeconds"].as_f64().map(|s| (s * 1000.0).round().max(0.0) as u64),
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

/// Records what a scan read (after the TypeScript secret filter): validates, normalizes and
/// appends `run.*` events; a file already recorded with the same content adds nothing.
pub fn record(engine: &Engine, args: RecordArgs) -> Result<Json> {
    let catalog = choose_catalog(engine, &args.catalog)?;
    let current = engine.with_db(|db| {
        let read = db.begin_read()?;
        let meta = read.open_table(RUN_META)?;
        Ok(meta.get("catalog")?.map(|v| String::from_utf8_lossy(v.value()).into_owned()))
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
    // Which (run id, sha) pairs and paths are already stored.
    let mut seen: HashSet<(String, [u8; 32])> = HashSet::new();
    let stored = |run_id: &str, sha: &[u8; 32]| -> Result<bool> {
        engine.with_db(|db| {
            let read = db.begin_read()?;
            let rev = read.open_table(RUN_DICT_REV)?;
            let Some(r) = lookup_id(&rev, kind::RUN_ID, run_id)? else {
                return Ok(false);
            };
            let table = read.open_table(RUN_REV)?;
            let lo = Key::new().u32(r).bytes(&[0, 0]);
            let hi = Key::new().u32(r).bytes(&[0xff, 0xff]);
            for item in table.range(lo.as_slice()..=hi.as_slice())? {
                if Rev::decode(item?.1.value())?.sha == *sha {
                    return Ok(true);
                }
            }
            Ok(false)
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
    // (path, size, mtime, sha, state): a stat refresh of content already on record.
    let mut touch: Vec<(String, u64, u64, [u8; 32], u8)> = Vec::new();
    let refuse = |events: &mut Vec<Event>, path: &str, size: u64, mtime: u64, sha: Option<String>, reason: &str| -> Result<()> {
        let key = format!("run.refused:{path}:{}:{reason}", sha.clone().unwrap_or_default());
        events.push(event(
            "run.refused",
            args.now_ms,
            key,
            &RefusedPayload {
                path: path.to_string(),
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
        let same = file_state(&r.path)?.is_some_and(|row| {
            row.state == 2 && r.sha256.as_deref().and_then(unhex32).is_none_or(|s| s == row.sha)
        });
        if same {
            let sha = r.sha256.as_deref().and_then(unhex32).unwrap_or([0; 32]);
            touch.push((r.path.clone(), r.size, r.mtime_ms, sha, 2));
            known += 1;
            continue;
        }
        refuse(&mut events, &r.path, r.size, r.mtime_ms, r.sha256.clone(), &r.reason)?;
        refused += 1;
    }
    for f in &args.files {
        let sha = sha256(f.text.as_bytes());
        let sha_hex = hex(&sha);
        let Some(writer) = writer_of_path(&f.path) else {
            refuse(&mut events, &f.path, f.size, f.mtime_ms, Some(sha_hex), "is not under a writer's runs folder")?;
            refused += 1;
            continue;
        };
        let projected = match run_schema::parse_run_text(&f.text, writer) {
            Ok(run) => run,
            Err(reason) => {
                let same = file_state(&f.path)?.is_some_and(|row| row.state == 2 && row.sha == sha);
                if same {
                    touch.push((f.path.clone(), f.size, f.mtime_ms, sha, 2));
                    known += 1;
                } else {
                    refuse(&mut events, &f.path, f.size, f.mtime_ms, Some(sha_hex), &reason.0)?;
                    refused += 1;
                }
                continue;
            }
        };
        // The store's own rule for persisted text (docs/store.md 2.2), on top of the filter.
        if crate::secret::looks_secret(&f.text) {
            refuse(&mut events, &f.path, f.size, f.mtime_ms, Some(sha_hex), "refused: it contains what looks like a credential")?;
            refused += 1;
            continue;
        }
        let run_id = projected["id"].as_str().unwrap_or("").to_string();
        if !seen.insert((run_id.clone(), sha)) || stored(&run_id, &sha)? {
            touch.push((f.path.clone(), f.size, f.mtime_ms, sha, 1));
            known += 1;
            continue;
        }
        let ids = run_ids(&projected, threads.get(writer).unwrap_or(&ThreadFacts::default()));
        let identity = run_identity::identify(&catalog, &written_of(&projected));
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
                identity: (&identity).into(),
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
            for (path, size, mtime, sha, state) in &touch {
                let row = match get(&txn, RUN_FILE, &path_key(path))? {
                    Some(bytes) => FileRow {
                        size: *size,
                        mtime_ms: *mtime,
                        ..FileRow::decode(&bytes)?
                    },
                    // The same content under a second path: remember the path so it is not read
                    // on every pass.
                    None => FileRow {
                        state: *state,
                        size: *size,
                        mtime_ms: *mtime,
                        sha: *sha,
                        run: 0,
                        reason: 0,
                    },
                };
                put(&txn, RUN_FILE, &path_key(path), &row.encode())?;
            }
            txn.commit()?;
            Ok(())
        })?;
    }
    // Re-keying finishes once per catalog version, also after a crash part way through it.
    let rekeyed_for = engine.with_db(|db| {
        let read = db.begin_read()?;
        let meta = read.open_table(RUN_META)?;
        Ok(meta.get("rekeyed")?.map(|v| String::from_utf8_lossy(v.value()).into_owned()))
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

/// Re-normalizes every current revision under a new catalog and records a `run.identity` for
/// each whose identity changed; earlier identities stay in the ledger.
fn rekey(engine: &Engine, catalog: &Catalog, now: u64) -> Result<usize> {
    let mut changed: Vec<(String, [u8; 32], Identity)> = Vec::new();
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let heads = read.open_table(RUN_HEAD)?;
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let mut memo: HashMap<(u32, u32, u32), Identity> = HashMap::new();
        for item in heads.iter()? {
            let (k, v) = item?;
            let r = be_u32(k.value(), 0);
            let head = &Head::decode(v.value())?;
            let triple = (head.id(at::MODEL_RAW), head.id(at::EFFORT_RAW), head.id(at::HARNESS_RAW));
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
    /// Resume after this `[endedMs, r]` cursor.
    pub after: Option<(u64, u32)>,
    pub limit: Option<usize>,
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
fn ids_json<T: ReadableTable<&'static [u8], &'static [u8]>>(dict: &mut Dict<'_, T>, r: u32, head: &Head) -> Result<Json> {
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
pub fn query(engine: &Engine, args: &QueryArgs) -> Result<Json> {
    let limit = args.limit.unwrap_or(5000).clamp(1, 20_000);
    engine.with_db(|db| {
        let read = db.begin_read()?;
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
            let Some(value) = heads.get(Key::new().u32(r).as_slice())? else { continue };
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
            let Ok(mut run) = run_schema::parse_run_text(&text, &writer) else {
                skipped += 1;
                continue;
            };
            let identity = identity_json(&mut dict, &head)?;
            let shown = identity["modelId"].as_str().or(identity["modelRaw"].as_str()).map(str::to_string);
            run["model"]["id"] = opt_json(shown);
            run["model"]["effort"] = identity["effort"].clone();
            run["model"]["harness"] = identity["harness"].clone();
            run["model"]["provider"] = Json::Null;
            out.push(json!({
                "file": dict.field(&head, at::PATH)?,
                "sha256": hex(&head.sha),
                "firstSeenAt": head.ingested_ms,
                "doc": run,
                "identity": identity,
                "ids": ids_json(&mut dict, r, &head)?,
            }));
            last = Some((ended, r));
        }
        Ok(json!({ "runs": out, "next": next, "skipped": skipped }))
    })
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
                    let rev = Rev::decode(item?.1.value())?;
                    if rev.sha == want {
                        found = Some(rev.loc);
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
            let rev = Rev::decode(item?.1.value())?;
            out.push(json!({
                "sha256": hex(&rev.sha), "seq": rev.seq, "writtenAtMs": rev.written_ms,
                "ingestedAtMs": rev.ingested_ms, "eventSeq": rev.event_seq,
                "path": dict.text(kind::PATH, rev.path)?, "catalogHash": dict.text(kind::CATALOG, rev.catalog)?,
            }));
        }
        Ok(Json::Array(out))
    })
}

/// Refused files on record, newest content first by path.
pub fn refused(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let files = read.open_table(RUN_FILE)?;
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let rev = read.open_table(RUN_DICT_REV)?;
        // Paths are interned; the file table is keyed by their hash, so read them back by id.
        let mut by_key: HashMap<[u8; 16], String> = HashMap::new();
        for item in rev.range(Key::new().u8(kind::PATH).as_slice()..Key::new().u8(kind::PATH + 1).as_slice())? {
            let (k, _) = item?;
            let text = String::from_utf8_lossy(&k.value()[1..]).into_owned();
            by_key.insert(path_key(&text), text);
        }
        let mut out = Vec::new();
        for item in files.iter()? {
            let (k, v) = item?;
            let row = FileRow::decode(v.value())?;
            if row.state != 2 {
                continue;
            }
            let key: [u8; 16] = k.value().try_into().unwrap_or([0; 16]);
            out.push(json!({
                "file": by_key.get(&key),
                "reason": dict.text(kind::REASON, row.reason)?,
                "size": row.size,
                "mtimeMs": row.mtime_ms,
            }));
        }
        out.sort_by(|a, b| a["file"].as_str().cmp(&b["file"].as_str()));
        Ok(Json::Array(out))
    })
}

/// The page's identity facts under the current catalog: families in order, each catalog
/// model's family and order, and the effort order.
pub fn identity_table(engine: &Engine) -> Result<Json> {
    let catalog = current_catalog(engine)?.unwrap_or_default();
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

/// Every distinct written model, effort and harness with counts and what each maps to, the
/// unmapped strings and the data-quality counts (DESIGN.md 1.1 to 1.5).
pub fn audit(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
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
            let head = &Head::decode(item?.1.value())?;
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
        Ok(json!({
            "runs": total,
            "models": rows(models, ["modelRaw", "modelId", "modelSource"]),
            "efforts": rows(efforts, ["effortRaw", "effort", "effortSource"]),
            "harnesses": harness_rows,
            "unmapped": unmapped_rows,
            "quality": quality,
        }))
    })
}

/// Counts and sizes of the run tables, the current catalog and the generation.
pub fn status(engine: &Engine) -> Result<Json> {
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
            "runs": read.open_table(RUN_HEAD)?.len()?,
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
