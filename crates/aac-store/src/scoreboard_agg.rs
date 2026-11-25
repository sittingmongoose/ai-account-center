//! Scoreboard aggregate tables kept up to date in the same redb write transaction that ingests
//! a run (and its corrections, revisions, path updates, rekeying, and joined reviews), plus a
//! verified trust checkpoint and one-time migration from existing `run_head`/`run_payload` rows.
//!
//! Design:
//! - Common event (brand-new run with no `corrects`, no `void`, no `review`, no prior head at its
//!   path, and no waiting correction/review targeting its id): O(1) append into `run_sb_bucket`
//!   plus O(1) trust-digest update inside the same write transaction.
//! - Rare event (any correction, void, review, revision of an existing run, second run at an
//!   existing path, path update, catalog rekey, or refusal of a known path): marks the commit
//!   batch dirty and performs a clean full rebuild from `run_head` + `run_payload` inside the
//!   same write transaction before committing.
//! - Trust checkpoint: `(sb_agg_version, sb_agg_seq, sb_agg_catalog, sb_agg_head_count,
//!   sb_agg_head_digest)` verified on open (`finish_replay`) and before serving; any mismatch
//!   triggers a full rebuild. Replay never incrementally updates or stamps aggregates.
//!
//! Tables:
//! - `run_sb_fact`: `r u32 -> StoredRunFact` (typed per-run scoring + summary facts; ~250 B/run).
//! - `run_sb_bucket`: `BucketKey -> BucketAgg` (per-task-type, per-model, per-source, per-benchmark
//!   counters, exact median samples, time-ordered cost samples, flags/metrics/reviews, top-20 newest).
//! - `run_sb_reviews_by_target`: `(target_r u32, review_r u32) -> ()` (joined reviews).
//! - `run_sb_corrections`: `(target_r u32, corrector_r u32) -> ()` (correction targets).
//! - `run_sb_correction_refused`: `r u32 -> String` (refused correction reasons).
//! - `run_sb_path_runs`: `(path_hash [u8; 16], r u32) -> ()` (multi-run retained-path index).
//! - `run_sb_by_writer_day`: `(writer_id u32, day [u8; 10], r u32) -> ()` (scoped folder-problem trace).

use crate::db::{Key, be_u32, be_u64, prefix_end};
use crate::engine::Engine;
use crate::error::Result;
use crate::fail;
use crate::run_schema;
use crate::runs::{
    self, AcceptedReceipt, Dict, Head, RUN_BY_TIME, RUN_DICT, RUN_DICT_REV, RUN_FILE, RUN_HEAD, RUN_META, RUN_PAYLOAD,
    at, block_decode, hex, kind, lookup_id, meta_get, meta_set, path_key,
};
use redb::{
    Database, ReadTransaction, ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition,
    WriteTransaction,
};
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

#[derive(Default)]
struct BatchAggState {
    needs_full_rebuild: bool,
    appended_heads: u64,
    appended_digest: [u64; 4],
    catalog_updated: bool,
}

thread_local! {
    /// Per-thread aggregate commit state active during an `Engine::commit` write transaction.
    static BATCH_STATE: RefCell<Option<BatchAggState>> = const { RefCell::new(None) };
}

/// Scoped guard tracking O(1) appends vs full-rebuild triggers across one `Engine::commit` batch.
/// Always clears thread-local state on drop (even on error or panic).
pub(crate) struct DirtyBatchGuard {
    active: bool,
}

impl DirtyBatchGuard {
    pub(crate) fn enter(txn: &WriteTransaction, prev_runs_seq: Option<u64>) -> Result<Self> {
        let valid_before = match prev_runs_seq {
            Some(prev_seq) => is_stamp_header_consistent_tx(txn, prev_seq)?,
            None => true,
        };
        let active = BATCH_STATE.with(|cell| {
            let mut slot = cell.borrow_mut();
            if slot.is_none() {
                *slot = Some(BatchAggState {
                    needs_full_rebuild: !valid_before,
                    appended_heads: 0,
                    appended_digest: [0; 4],
                    catalog_updated: false,
                });
                true
            } else {
                if !valid_before && let Some(st) = slot.as_mut() {
                    st.needs_full_rebuild = true;
                }
                false
            }
        });
        Ok(Self { active })
    }
}

impl Drop for DirtyBatchGuard {
    fn drop(&mut self) {
        if self.active {
            BATCH_STATE.with(|cell| {
                *cell.borrow_mut() = None;
            });
        }
    }
}

/// Mark the active `Engine::commit` batch as needing a clean full aggregate rebuild from `RUN_HEAD`.
pub(crate) fn mark_full_rebuild() {
    BATCH_STATE.with(|cell| {
        if let Some(st) = cell.borrow_mut().as_mut() {
            st.needs_full_rebuild = true;
        }
    });
}

fn batch_needs_full_rebuild() -> bool {
    BATCH_STATE.with(|cell| cell.borrow().as_ref().is_none_or(|st| st.needs_full_rebuild))
}

/// Note that `run.catalog` updated `RUN_META["catalog"]` in the current commit batch.
pub(crate) fn on_catalog_changed() {
    BATCH_STATE.with(|cell| {
        if let Some(st) = cell.borrow_mut().as_mut() {
            st.catalog_updated = true;
        }
    });
}

/// Record a refusal event: if the refused path hash matches any known effective run, schedule a
/// clean full rebuild so path/refusal invariants stay exact.
pub(crate) fn on_refused_recorded(txn: &WriteTransaction, path_hash: &[u8; 16]) -> Result<()> {
    if !batch_needs_full_rebuild() && has_any_path_hash_tx(txn, path_hash)? {
        mark_full_rebuild();
    }
    Ok(())
}

/// Finalize Scoreboard aggregates inside the same `Engine::commit` write transaction before the
/// ledger write and transaction commit.
pub(crate) fn finish_commit(txn: &WriteTransaction, new_runs_seq: Option<u64>) -> Result<()> {
    let state = BATCH_STATE.with(|cell| cell.borrow_mut().take()).unwrap_or_default();
    let Some(seq) = new_runs_seq else {
        return Ok(());
    };
    if state.needs_full_rebuild {
        return rebuild_all_aggregates_in_write(txn, seq);
    }
    if state.appended_heads > 0 {
        let cur_count = meta_get(txn, META_HEAD_COUNT)?
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        let cur_digest = meta_get(txn, META_HEAD_DIGEST)?
            .as_deref()
            .and_then(parse_digest)
            .unwrap_or([0; 4]);
        let next_count = cur_count + state.appended_heads;
        let next_digest = add_digest(cur_digest, state.appended_digest);
        meta_set(txn, META_HEAD_COUNT, &next_count.to_string())?;
        meta_set(txn, META_HEAD_DIGEST, &format_digest(next_digest))?;
    }
    if state.catalog_updated {
        let cat = meta_get(txn, "catalog")?.unwrap_or_default();
        meta_set(txn, META_CATALOG, &cat)?;
    }
    meta_set(txn, META_SEQ, &seq.to_string())?;
    Ok(())
}

type Bytes = TableDefinition<'static, &'static [u8], &'static [u8]>;
type Index = TableDefinition<'static, &'static [u8], ()>;

pub const RUN_SB_FACT: Bytes = TableDefinition::new("run_sb_fact");
pub const RUN_SB_BUCKET: Bytes = TableDefinition::new("run_sb_bucket");
pub const RUN_SB_REVIEWS_BY_TARGET: Index = TableDefinition::new("run_sb_reviews_by_target");
pub const RUN_SB_CORRECTIONS: Index = TableDefinition::new("run_sb_corrections");
pub const RUN_SB_CORRECTION_REFUSED: Bytes = TableDefinition::new("run_sb_correction_refused");
pub const RUN_SB_PATH_RUNS: Index = TableDefinition::new("run_sb_path_runs");
pub const RUN_SB_BY_WRITER_DAY: Index = TableDefinition::new("run_sb_by_writer_day");

/// Legacy v1 tables cleaned up on upgrade/rebuild (v1 stored single-run `run_sb_path_hash` with
/// `&[u8]` values and `run_sb_bucket_members`).
const LEGACY_RUN_SB_PATH_HASH: Bytes = TableDefinition::new("run_sb_path_hash");
const LEGACY_RUN_SB_BUCKET_MEMBERS: Index = TableDefinition::new("run_sb_bucket_members");

/// Bump when the binary format or derivation rules of `run_sb_*` change: `ensure_migrated`
/// rebuilds the aggregate tables once from existing `run_head` + `run_payload` rows.
pub const SB_AGG_VERSION: u32 = 2;
const BIN_VERSION: u8 = 2;
const META_VERSION: &str = "sb_agg_version";
const META_SEQ: &str = "sb_agg_seq";
const META_CATALOG: &str = "sb_agg_catalog";
const META_HEAD_COUNT: &str = "sb_agg_head_count";
const META_HEAD_DIGEST: &str = "sb_agg_head_digest";
const META_EFFECTIVE_COUNT: &str = "sb_agg_effective_count";
/// Maximum newest summaries retained per bucket (Scoreboard shows at most 20 per column,
/// 20 globally in `recent`, and 10 globally in `unscored.examples`).
pub const BUCKET_RECENT_CAP: usize = 20;

/// Exact `Object.prototype` member names (matches `aac_status::read_context::PROTO_NAMES`).
const PROTO_NAMES: [&str; 12] = [
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

fn proto_named(value: &Json) -> bool {
    let named = |text: &str| PROTO_NAMES.contains(&text);
    match value {
        Json::String(text) => named(text),
        Json::Array(rows) => rows.iter().any(proto_named),
        Json::Object(map) => map.iter().any(|(key, v)| named(key) || proto_named(v)),
        _ => false,
    }
}

/// ECMAScript WhiteSpace and LineTerminator (matches `aac_status::read_context::js_whitespace`).
pub fn js_whitespace(c: char) -> bool {
    matches!(
        c,
        '\u{0009}'..='\u{000d}'
            | '\u{0020}'
            | '\u{00a0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200a}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202f}'
            | '\u{205f}'
            | '\u{3000}'
            | '\u{feff}'
    )
}

/// Outcome classification matching `aac_status::read_measures::outcome`.
pub fn outcome_code(raw: &str) -> u8 {
    let word = raw.trim_matches(js_whitespace).to_lowercase().replace('_', "-");
    if word == "done" {
        1
    } else if [
        "failed",
        "failure",
        "cancelled",
        "canceled",
        "did-not-start",
        "not-started",
        "error",
        "crashed",
        "timed-out",
        "timeout",
        "aborted",
        "killed",
        "interrupted",
        "abandoned",
    ]
    .contains(&word.as_str())
    {
        2
    } else {
        0
    }
}

/// Normalized unknown-outcome label matching `aac_status::read_scoreboard::runs_source_of`.
pub fn normalized_unknown_outcome(raw: &str) -> String {
    raw.split(js_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub fn create_tables(txn: &WriteTransaction) -> Result<()> {
    for def in [RUN_SB_FACT, RUN_SB_BUCKET, RUN_SB_CORRECTION_REFUSED] {
        txn.open_table(def)?;
    }
    for def in [
        RUN_SB_REVIEWS_BY_TARGET,
        RUN_SB_CORRECTIONS,
        RUN_SB_PATH_RUNS,
        RUN_SB_BY_WRITER_DAY,
    ] {
        txn.open_table(def)?;
    }
    Ok(())
}

pub fn delete_tables(txn: &WriteTransaction) -> Result<()> {
    for def in [RUN_SB_FACT, RUN_SB_BUCKET, RUN_SB_CORRECTION_REFUSED] {
        txn.delete_table(def)?;
    }
    for def in [
        RUN_SB_REVIEWS_BY_TARGET,
        RUN_SB_CORRECTIONS,
        RUN_SB_PATH_RUNS,
        RUN_SB_BY_WRITER_DAY,
    ] {
        txn.delete_table(def)?;
    }
    let _ = txn.delete_table(LEGACY_RUN_SB_PATH_HASH);
    let _ = txn.delete_table(LEGACY_RUN_SB_BUCKET_MEMBERS);
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Verified trust checkpoint: (version, runs/ledger seq, catalog, head count, head digest)

/// Deterministic 256-bit hash of one `RUN_HEAD` row's aggregate-relevant fields (every field in
/// `runs::head_affects_agg`, excluding `revisions` which changes on losing revisions).
fn head_entry_hash(r: u32, head: &Head) -> [u64; 4] {
    let mut h = Sha256::new();
    h.update(r.to_be_bytes());
    h.update(head.seq.to_be_bytes());
    h.update(head.written_ms.to_be_bytes());
    h.update(head.ingested_ms.to_be_bytes());
    h.update(head.ended_ms.to_be_bytes());
    h.update([head.flags, head.verdict]);
    h.update(head.sha);
    h.update(head.loc.day.to_be_bytes());
    h.update(head.loc.block.to_be_bytes());
    h.update(head.loc.slot.to_be_bytes());
    for id in head.ids {
        h.update(id.to_be_bytes());
    }
    let bytes = h.finalize();
    [
        u64::from_be_bytes(bytes[0..8].try_into().unwrap()),
        u64::from_be_bytes(bytes[8..16].try_into().unwrap()),
        u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
        u64::from_be_bytes(bytes[24..32].try_into().unwrap()),
    ]
}

fn add_digest(a: [u64; 4], b: [u64; 4]) -> [u64; 4] {
    [
        a[0].wrapping_add(b[0]),
        a[1].wrapping_add(b[1]),
        a[2].wrapping_add(b[2]),
        a[3].wrapping_add(b[3]),
    ]
}

fn format_digest(d: [u64; 4]) -> String {
    let mut out = [0u8; 32];
    for (i, lane) in d.iter().enumerate() {
        out[i * 8..(i + 1) * 8].copy_from_slice(&lane.to_be_bytes());
    }
    hex(&out)
}

fn parse_digest(text: &str) -> Option<[u64; 4]> {
    if text.len() != 64 {
        return None;
    }
    let mut bytes = [0u8; 32];
    for i in 0..32 {
        bytes[i] = u8::from_str_radix(&text[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some([
        u64::from_be_bytes(bytes[0..8].try_into().unwrap()),
        u64::from_be_bytes(bytes[8..16].try_into().unwrap()),
        u64::from_be_bytes(bytes[16..24].try_into().unwrap()),
        u64::from_be_bytes(bytes[24..32].try_into().unwrap()),
    ])
}

fn compute_heads_stamp_from_table<T: ReadableTable<&'static [u8], &'static [u8]>>(
    heads: &T,
) -> Result<(u64, [u64; 4])> {
    let mut count = 0u64;
    let mut digest = [0u64; 4];
    for item in heads.iter()? {
        let (k, v) = item?;
        let r = be_u32(k.value(), 0);
        let head = Head::decode(v.value())?;
        count += 1;
        digest = add_digest(digest, head_entry_hash(r, &head));
    }
    Ok((count, digest))
}

/// Fast O(1) header check at the start of an `Engine::commit` batch: verifies that the stored
/// aggregate metadata matches `SB_AGG_VERSION`, `prev_runs_seq`, and `RUN_META["catalog"]`.
fn is_stamp_header_consistent_tx(txn: &WriteTransaction, prev_runs_seq: u64) -> Result<bool> {
    let ver = meta_get(txn, META_VERSION)?.and_then(|v| v.parse::<u32>().ok());
    if ver != Some(SB_AGG_VERSION) {
        return Ok(false);
    }
    let seq = meta_get(txn, META_SEQ)?.and_then(|v| v.parse::<u64>().ok());
    if seq != Some(prev_runs_seq) {
        return Ok(false);
    }
    let want_cat = meta_get(txn, "catalog")?.unwrap_or_default();
    let got_cat = meta_get(txn, META_CATALOG)?;
    if got_cat.as_deref() != Some(want_cat.as_str()) {
        return Ok(false);
    }
    let has_count = meta_get(txn, META_HEAD_COUNT)?
        .and_then(|v| v.parse::<u64>().ok())
        .is_some();
    let has_digest = meta_get(txn, META_HEAD_DIGEST)?
        .as_deref()
        .and_then(parse_digest)
        .is_some();
    Ok(has_count && has_digest)
}

/// Full trust verification on open (`finish_replay`) and before serving: checks schema version,
/// `runs/ledger` checkpoint sequence, catalog hash, `RUN_HEAD` count, and the 256-bit digest of
/// all `RUN_HEAD` rows. Any mismatch triggers a full rebuild.
pub(crate) fn verify_trust_stamp_rd(read: &ReadTransaction) -> Result<bool> {
    let cp_table = read.open_table(crate::db::CHECKPOINT)?;
    let cp_seq = crate::db::read_checkpoint(&cp_table, "runs/ledger")?
        .map(|c| c.last_sequence_id)
        .unwrap_or(0);
    let meta = read.open_table(RUN_META)?;
    let read_str = |key: &str| -> Result<Option<String>> {
        Ok(meta
            .get(key)?
            .and_then(|v| std::str::from_utf8(v.value()).ok().map(str::to_owned)))
    };
    if read_str(META_VERSION)?.and_then(|v| v.parse::<u32>().ok()) != Some(SB_AGG_VERSION) {
        return Ok(false);
    }
    if read_str(META_SEQ)?.and_then(|v| v.parse::<u64>().ok()) != Some(cp_seq) {
        return Ok(false);
    }
    let want_cat = read_str("catalog")?.unwrap_or_default();
    if read_str(META_CATALOG)?.as_deref() != Some(want_cat.as_str()) {
        return Ok(false);
    }
    let Some(want_count) = read_str(META_HEAD_COUNT)?.and_then(|v| v.parse::<u64>().ok()) else {
        return Ok(false);
    };
    let Some(want_digest) = read_str(META_HEAD_DIGEST)?.as_deref().and_then(parse_digest) else {
        return Ok(false);
    };
    let heads = read.open_table(RUN_HEAD)?;
    if heads.len()? != want_count {
        return Ok(false);
    }
    let (actual_count, actual_digest) = compute_heads_stamp_from_table(&heads)?;
    Ok(actual_count == want_count && actual_digest == want_digest)
}

/// Effective (post-correction) run count directly from aggregate metadata when the trust stamp
/// verifies against the live store.
pub(crate) fn effective_run_count_in_read(read: &ReadTransaction) -> Result<Option<u64>> {
    if !verify_trust_stamp_rd(read)? {
        return Ok(None);
    }
    let meta = read.open_table(RUN_META)?;
    let count = meta
        .get(META_EFFECTIVE_COUNT)?
        .and_then(|v| std::str::from_utf8(v.value()).ok()?.parse::<u64>().ok());
    Ok(count)
}

// ---------------------------------------------------------------------------------------------
// Data model

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BucketKey {
    pub task_type: String,
    pub source_benchmark: bool,
    pub model_id: String,
    pub harness: String,
    pub effort: String,
    pub proc_id: String,
    pub proc_ver: String,
    pub benchmark: String,
}

impl BucketKey {
    pub fn from_fact(fact: &StoredRunFact) -> Self {
        let (proc_id, proc_ver) = match &fact.process {
            Some((id, ver)) => (id.clone(), ver.clone().unwrap_or_default()),
            None => (String::new(), String::new()),
        };
        Self {
            task_type: fact.task_type.clone().unwrap_or_default(),
            source_benchmark: fact.source_benchmark,
            model_id: fact.model_id.clone().unwrap_or_default(),
            harness: fact.harness.clone().unwrap_or_default(),
            effort: fact.effort.clone().unwrap_or_default(),
            proc_id,
            proc_ver,
            benchmark: fact.benchmark.clone().unwrap_or_default(),
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut w = BinWriter::with_capacity(64);
        w.str_nul(&self.task_type);
        w.u8(u8::from(self.source_benchmark));
        w.str_nul(&self.model_id);
        w.str_nul(&self.harness);
        w.str_nul(&self.effort);
        w.str_nul(&self.proc_id);
        w.str_nul(&self.proc_ver);
        w.str_nul(&self.benchmark);
        w.0
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = BinReader::new(bytes);
        let task_type = r.str_nul()?;
        let source_benchmark = r.u8()? != 0;
        let model_id = r.str_nul()?;
        let harness = r.str_nul()?;
        let effort = r.str_nul()?;
        let proc_id = r.str_nul()?;
        let proc_ver = r.str_nul()?;
        let benchmark = r.str_nul()?;
        Ok(Self {
            task_type,
            source_benchmark,
            model_id,
            harness,
            effort,
            proc_id,
            proc_ver,
            benchmark,
        })
    }

    fn task_type_prefix(task_type: &str) -> Vec<u8> {
        let mut w = BinWriter::with_capacity(task_type.len() + 1);
        w.str_nul(task_type);
        w.0
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredEstimate {
    pub source: String,
    pub method: String,
    pub credits: Option<f64>,
    pub quota: Option<(String, f64)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredCost {
    pub credits: Option<f64>,
    pub usd: Option<f64>,
    pub quota: Option<(String, f64)>,
    pub estimate: Option<StoredEstimate>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredReview {
    pub orig_of: String,
    pub effective_of: String,
    pub verdict: String,
    pub overturned: bool,
    pub disputed: bool,
    pub by: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StoredFlag {
    pub kind: String,
    pub detail: String,
    pub evidence: String,
    pub reported_by: Option<String>,
}

/// Compact typed facts for one run (one row in `run_sb_fact`).
#[derive(Clone, Debug, PartialEq)]
pub struct StoredRunFact {
    pub r: u32,
    pub writer_id: u32,
    pub cursor_ms: u64,
    pub ended_epoch: i64,
    pub ingested_ms: u64,
    pub seq: u64,
    pub id: String,
    pub writer: String,
    pub file: String,
    pub ended_at: String,
    pub written_at: String,
    pub source_benchmark: bool,
    pub benchmark: Option<String>,
    pub project: Option<String>,
    pub stream: Option<String>,
    pub task_id: String,
    pub task_type: Option<String>,
    pub task_title: String,
    /// `[records, files, items, words, tasks]`
    pub task_size: [Option<u64>; 5],
    pub model_id: Option<String>,
    pub effort: Option<String>,
    pub harness: Option<String>,
    pub process: Option<(String, Option<String>)>,
    pub outcome_raw: String,
    pub outcome_code: u8,
    pub wall_seconds: Option<f64>,
    pub imported: Option<(String, String)>,
    pub scope_files: Option<(u64, u64)>,
    pub scope_records: Option<(u64, u64)>,
    pub cost: Option<StoredCost>,
    pub review: Option<StoredReview>,
    pub flags: Vec<StoredFlag>,
    pub evidence: Vec<(String, String)>,
    pub metrics: Vec<(String, f64)>,
    pub corrects: Option<String>,
    pub has_void: bool,
    pub has_proto: bool,
    pub effective: bool,
}

impl StoredRunFact {
    /// Unit index: 0=record, 1=file, 2=item, 3=word, 4=task.
    pub fn units_for(&self, unit_idx: usize) -> Option<u64> {
        match self.task_size[unit_idx] {
            Some(n) if n > 0 => Some(n),
            Some(_) => None,
            None if unit_idx == 4 => Some(1),
            _ => None,
        }
    }

    pub fn scope_ratios(&self) -> (Option<f64>, Option<f64>, Option<f64>) {
        let files = self
            .scope_files
            .and_then(|(a, b)| (b != 0).then(|| (a as f64 / b as f64).min(1.0)));
        let records = self
            .scope_records
            .and_then(|(a, b)| (b != 0).then(|| (a as f64 / b as f64).min(1.0)));
        let min = match (files, records) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) | (None, Some(a)) => Some(a),
            (None, None) => None,
        };
        (files, records, min)
    }

    pub fn has_flag(&self, kinds: &[&str]) -> bool {
        self.flags.iter().any(|f| kinds.contains(&f.kind.as_str()))
    }

    pub fn has_negative_quota(&self) -> bool {
        self.cost.as_ref().is_some_and(|c| {
            c.quota.as_ref().is_some_and(|(_, d)| *d < 0.0)
                || c.estimate
                    .as_ref()
                    .and_then(|e| e.quota.as_ref())
                    .is_some_and(|(_, d)| *d < 0.0)
        })
    }

    pub fn to_summary(&self) -> BucketRunSummary {
        BucketRunSummary {
            r: self.r,
            cursor_ms: self.cursor_ms,
            ended_epoch: self.ended_epoch,
            id: self.id.clone(),
            writer: self.writer.clone(),
            source_benchmark: self.source_benchmark,
            benchmark: self.benchmark.clone(),
            project: self.project.clone(),
            stream: self.stream.clone(),
            task_id: self.task_id.clone(),
            task_type: self.task_type.clone(),
            task_title: self.task_title.clone(),
            model_id: self.model_id.clone(),
            harness: self.harness.clone(),
            effort: self.effort.clone(),
            process: self.process.clone(),
            outcome: self.outcome_raw.clone(),
            ended_at: self.ended_at.clone(),
            wall_seconds: self.wall_seconds,
            imported: self.imported.clone(),
            review: self.review.as_ref().map(|r| SummaryReview {
                of: r.effective_of.clone(),
                verdict: r.verdict.clone(),
                overturned: r.overturned,
                disputed: r.disputed,
                by: r.by.clone(),
            }),
            flags_len: self.flags.len() as u32,
            evidence: self.evidence.clone(),
        }
    }
}

/// Compact per-run cost sample stored inside a bucket so cost rates are summed in exact
/// `RUN_BY_TIME` `(cursor_ms, r)` order regardless of arrival order or cross-bucket column merges.
#[derive(Clone, Debug, PartialEq)]
pub struct BucketCostSample {
    pub cursor_ms: u64,
    pub r: u32,
    pub outcome_done: bool,
    pub task_size: [Option<u64>; 5],
    pub cost: StoredCost,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SpeedSample {
    pub wall_seconds: f64,
    pub task_size: [Option<u64>; 5],
}

#[derive(Clone, Debug, PartialEq)]
pub struct BucketFlag {
    pub ended_epoch: i64,
    pub run_id: String,
    pub flag_idx: u32,
    pub kind: String,
    pub detail: String,
    pub evidence: String,
    pub reported_by: Option<String>,
}

/// One review targeting a run in this bucket.
#[derive(Clone, Debug, PartialEq)]
pub struct BucketJoinedReview {
    pub target_r: u32,
    pub target_cursor_ms: u64,
    pub target_id: String,
    pub target_project: Option<String>,
    pub target_task_id: String,
    pub target_outcome_done: bool,
    pub target_ended_epoch: i64,
    pub review_r: u32,
    pub review_cursor_ms: u64,
    pub review_ended_epoch: i64,
    pub review_id: String,
    pub review_source_benchmark: bool,
    pub review_task_type: Option<String>,
    pub review_model_id: Option<String>,
    pub review_effort: Option<String>,
    pub review_file: String,
    pub review_has_proto: bool,
    pub verdict: String,
    pub overturned: bool,
    pub disputed: bool,
    pub by: Option<String>,
    pub evidence0_path: Option<String>,
}

/// For a run inside THIS bucket that itself carries a `review` block: metadata needed to count
/// `index.ignored` accurately under any `(source, taskType, from, to)` filter.
#[derive(Clone, Debug, PartialEq)]
pub struct OwnReviewRef {
    pub self_r: u32,
    pub self_ended_epoch: i64,
    pub self_task_type_is_review: bool,
    pub target_r: Option<u32>,
    pub target_cursor_ms: Option<u64>,
    pub target_ended_epoch: Option<i64>,
    pub target_source_benchmark: Option<bool>,
    pub target_task_type: Option<String>,
    pub target_effective: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SummaryReview {
    pub of: String,
    pub verdict: String,
    pub overturned: bool,
    pub disputed: bool,
    pub by: Option<String>,
}

/// Compact run summary for bounded recent windows (`column.runs`, `recent`, `unscored.examples`).
#[derive(Clone, Debug, PartialEq)]
pub struct BucketRunSummary {
    pub r: u32,
    pub cursor_ms: u64,
    pub ended_epoch: i64,
    pub id: String,
    pub writer: String,
    pub source_benchmark: bool,
    pub benchmark: Option<String>,
    pub project: Option<String>,
    pub stream: Option<String>,
    pub task_id: String,
    pub task_type: Option<String>,
    pub task_title: String,
    pub model_id: Option<String>,
    pub harness: Option<String>,
    pub effort: Option<String>,
    pub process: Option<(String, Option<String>)>,
    pub outcome: String,
    pub ended_at: String,
    pub wall_seconds: Option<f64>,
    pub imported: Option<(String, String)>,
    pub review: Option<SummaryReview>,
    pub flags_len: u32,
    pub evidence: Vec<(String, String)>,
}

/// Pre-aggregated totals for one `(task_type, source, model, harness, effort, process, benchmark)`
/// bucket, stored in `run_sb_bucket` and updated atomically at ingest time.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BucketAgg {
    pub n: u64,
    pub imported: u64,
    pub proto_count: u64,
    pub max_cursor: (u64, u32),
    pub outcome_done: u64,
    pub outcome_known: u64,
    pub unknown_outcomes: BTreeMap<String, u64>,
    pub negative_quota_runs: Vec<(u64, u32, String)>,
    pub cheating_count: u64,
    pub laziness_counted: u64,
    pub laziness_lazy: u64,
    pub speed_samples: Vec<SpeedSample>,
    pub thoroughness_files: Vec<f64>,
    pub thoroughness_records: Vec<f64>,
    pub thoroughness_min: Vec<f64>,
    pub cost_samples: Vec<BucketCostSample>,
    pub metric_rows: Vec<Vec<(String, f64)>>,
    pub flags: Vec<BucketFlag>,
    pub joined_reviews: Vec<BucketJoinedReview>,
    pub own_review_refs: Vec<OwnReviewRef>,
    /// Bounded top-20 newest runs in this bucket, ordered by `(ended_epoch desc, collate(id) asc)`.
    pub recent_runs: Vec<BucketRunSummary>,
    /// Distinct `kind::WRITER` dictionary ids of runs in this bucket.
    pub writers: BTreeSet<u32>,
}

pub fn newest_cmp(a_epoch: i64, a_id: &str, b_epoch: i64, b_id: &str) -> std::cmp::Ordering {
    b_epoch.cmp(&a_epoch).then_with(|| aac_num::intl::collate(a_id, b_id))
}

impl BucketAgg {
    /// Fold one effective run (and its effective joined reviews) into this bucket.
    /// When folding in ascending `(cursor_ms, r)` order, float sums match `main` bit-for-bit.
    pub fn fold_run(&mut self, fact: &StoredRunFact, joined: &[BucketJoinedReview], own_ref: Option<OwnReviewRef>) {
        self.n += 1;
        if fact.imported.is_some() {
            self.imported += 1;
        }
        if fact.has_proto {
            self.proto_count += 1;
        }
        let cursor = (fact.cursor_ms, fact.r);
        if cursor > self.max_cursor {
            self.max_cursor = cursor;
        }
        self.writers.insert(fact.writer_id);
        match fact.outcome_code {
            1 => {
                self.outcome_done += 1;
                self.outcome_known += 1;
            }
            2 => {
                self.outcome_known += 1;
            }
            _ => {
                let word = normalized_unknown_outcome(&fact.outcome_raw);
                *self.unknown_outcomes.entry(word).or_insert(0) += 1;
            }
        }
        if fact.has_negative_quota() {
            self.negative_quota_runs.push((fact.cursor_ms, fact.r, fact.id.clone()));
        }
        if fact.has_flag(&["shortcut", "cheating", "fabricated", "test-weakened", "false-claim"]) {
            self.cheating_count += 1;
        }
        let (files_r, records_r, min_r) = fact.scope_ratios();
        if let Some(v) = files_r {
            self.thoroughness_files.push(v);
        }
        if let Some(v) = records_r {
            self.thoroughness_records.push(v);
        }
        if let Some(v) = min_r {
            self.thoroughness_min.push(v);
        }
        let lazy_flag = fact.has_flag(&["incomplete-read", "scope-cut"]);
        if lazy_flag || min_r.is_some() {
            self.laziness_counted += 1;
            if lazy_flag || min_r.is_some_and(|v| v < 0.9) {
                self.laziness_lazy += 1;
            }
        }
        if fact.outcome_code == 1
            && let Some(wall) = fact.wall_seconds
        {
            self.speed_samples.push(SpeedSample {
                wall_seconds: wall,
                task_size: fact.task_size,
            });
        }
        if !fact.metrics.is_empty() {
            self.metric_rows.push(fact.metrics.clone());
        }
        for (idx, flag) in fact.flags.iter().enumerate() {
            self.flags.push(BucketFlag {
                ended_epoch: fact.ended_epoch,
                run_id: fact.id.clone(),
                flag_idx: idx as u32,
                kind: flag.kind.clone(),
                detail: flag.detail.clone(),
                evidence: flag.evidence.clone(),
                reported_by: flag.reported_by.clone(),
            });
        }
        // Store per-run cost samples keyed by `(cursor_ms, r)` so the composer always sums cost
        // rates in exact `RUN_BY_TIME` order across arrival orders and cross-bucket column merges.
        if let Some(cost) = &fact.cost {
            self.cost_samples.push(BucketCostSample {
                cursor_ms: fact.cursor_ms,
                r: fact.r,
                outcome_done: fact.outcome_code == 1,
                task_size: fact.task_size,
                cost: cost.clone(),
            });
        }
        if !joined.is_empty() {
            self.joined_reviews.extend(joined.iter().cloned());
        }
        if let Some(own) = own_ref {
            self.own_review_refs.push(own);
        }
        // Keep top BUCKET_RECENT_CAP newest summaries.
        let summary = fact.to_summary();
        let pos = self
            .recent_runs
            .binary_search_by(|existing| {
                newest_cmp(existing.ended_epoch, &existing.id, summary.ended_epoch, &summary.id)
            })
            .unwrap_or_else(|p| p);
        if pos < BUCKET_RECENT_CAP {
            self.recent_runs.insert(pos, summary);
            if self.recent_runs.len() > BUCKET_RECENT_CAP {
                self.recent_runs.truncate(BUCKET_RECENT_CAP);
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Compact binary serialization (exact IEEE-754 f64 bits, no JSON overhead)

struct BinWriter(Vec<u8>);

impl BinWriter {
    fn with_capacity(cap: usize) -> Self {
        Self(Vec::with_capacity(cap))
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn bool(&mut self, v: bool) {
        self.0.push(u8::from(v));
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn i64(&mut self, v: i64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn f64(&mut self, v: f64) {
        self.0.extend_from_slice(&v.to_be_bytes());
    }
    fn str(&mut self, s: &str) {
        self.u32(s.len() as u32);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn str_nul(&mut self, s: &str) {
        // Bucket key parts come from validated run fields (no NUL bytes allowed by run_schema).
        self.0.extend_from_slice(s.as_bytes());
        self.0.push(0);
    }
    fn opt_str(&mut self, s: &Option<String>) {
        match s {
            Some(s) => {
                self.u8(1);
                self.str(s);
            }
            None => self.u8(0),
        }
    }
    fn opt_u64(&mut self, v: Option<u64>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.u64(v);
            }
            None => self.u8(0),
        }
    }
    fn opt_u32(&mut self, v: Option<u32>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.u32(v);
            }
            None => self.u8(0),
        }
    }
    fn opt_i64(&mut self, v: Option<i64>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.i64(v);
            }
            None => self.u8(0),
        }
    }
    fn opt_bool(&mut self, v: Option<bool>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.bool(v);
            }
            None => self.u8(0),
        }
    }
    fn opt_f64(&mut self, v: Option<f64>) {
        match v {
            Some(v) => {
                self.u8(1);
                self.f64(v);
            }
            None => self.u8(0),
        }
    }
    fn task_size(&mut self, size: &[Option<u64>; 5]) {
        for v in size {
            self.opt_u64(*v);
        }
    }
    fn stored_cost(&mut self, c: &StoredCost) {
        self.opt_f64(c.credits);
        self.opt_f64(c.usd);
        match &c.quota {
            Some((win, d)) => {
                self.u8(1);
                self.str(win);
                self.f64(*d);
            }
            None => self.u8(0),
        }
        match &c.estimate {
            Some(e) => {
                self.u8(1);
                self.str(&e.source);
                self.str(&e.method);
                self.opt_f64(e.credits);
                match &e.quota {
                    Some((win, d)) => {
                        self.u8(1);
                        self.str(win);
                        self.f64(*d);
                    }
                    None => self.u8(0),
                }
            }
            None => self.u8(0),
        }
    }
}

struct BinReader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> BinReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        if self.at + n > self.bytes.len() {
            fail!("corrupt", "truncated scoreboard aggregate record");
        }
        let slice = &self.bytes[self.at..self.at + n];
        self.at += n;
        Ok(slice)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn bool(&mut self) -> Result<bool> {
        Ok(self.u8()? != 0)
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn str(&mut self) -> Result<String> {
        let len = self.u32()? as usize;
        let slice = self.take(len)?;
        Ok(String::from_utf8_lossy(slice).into_owned())
    }
    fn str_nul(&mut self) -> Result<String> {
        let rest = &self.bytes[self.at..];
        let Some(pos) = rest.iter().position(|&b| b == 0) else {
            fail!("corrupt", "truncated scoreboard bucket key");
        };
        let out = String::from_utf8_lossy(&rest[..pos]).into_owned();
        self.at += pos + 1;
        Ok(out)
    }
    fn opt_str(&mut self) -> Result<Option<String>> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(self.str()?))
        }
    }
    fn opt_u64(&mut self) -> Result<Option<u64>> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(self.u64()?))
        }
    }
    fn opt_u32(&mut self) -> Result<Option<u32>> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(self.u32()?))
        }
    }
    fn opt_i64(&mut self) -> Result<Option<i64>> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(self.i64()?))
        }
    }
    fn opt_bool(&mut self) -> Result<Option<bool>> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(self.bool()?))
        }
    }
    fn opt_f64(&mut self) -> Result<Option<f64>> {
        if self.u8()? == 0 {
            Ok(None)
        } else {
            Ok(Some(self.f64()?))
        }
    }
    fn task_size(&mut self) -> Result<[Option<u64>; 5]> {
        Ok([
            self.opt_u64()?,
            self.opt_u64()?,
            self.opt_u64()?,
            self.opt_u64()?,
            self.opt_u64()?,
        ])
    }
    fn stored_cost(&mut self) -> Result<StoredCost> {
        let credits = self.opt_f64()?;
        let usd = self.opt_f64()?;
        let quota = if self.u8()? == 0 {
            None
        } else {
            Some((self.str()?, self.f64()?))
        };
        let estimate = if self.u8()? == 0 {
            None
        } else {
            let source = self.str()?;
            let method = self.str()?;
            let e_credits = self.opt_f64()?;
            let e_quota = if self.u8()? == 0 {
                None
            } else {
                Some((self.str()?, self.f64()?))
            };
            Some(StoredEstimate {
                source,
                method,
                credits: e_credits,
                quota: e_quota,
            })
        };
        Ok(StoredCost {
            credits,
            usd,
            quota,
            estimate,
        })
    }
}

impl StoredRunFact {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = BinWriter::with_capacity(256);
        w.u8(BIN_VERSION);
        w.u32(self.r);
        w.u32(self.writer_id);
        w.u64(self.cursor_ms);
        w.i64(self.ended_epoch);
        w.u64(self.ingested_ms);
        w.u64(self.seq);
        w.str(&self.id);
        w.str(&self.writer);
        w.str(&self.file);
        w.str(&self.ended_at);
        w.str(&self.written_at);
        w.bool(self.source_benchmark);
        w.opt_str(&self.benchmark);
        w.opt_str(&self.project);
        w.opt_str(&self.stream);
        w.str(&self.task_id);
        w.opt_str(&self.task_type);
        w.str(&self.task_title);
        w.task_size(&self.task_size);
        w.opt_str(&self.model_id);
        w.opt_str(&self.effort);
        w.opt_str(&self.harness);
        match &self.process {
            Some((id, ver)) => {
                w.u8(1);
                w.str(id);
                w.opt_str(ver);
            }
            None => w.u8(0),
        }
        w.str(&self.outcome_raw);
        w.u8(self.outcome_code);
        w.opt_f64(self.wall_seconds);
        match &self.imported {
            Some((from, at)) => {
                w.u8(1);
                w.str(from);
                w.str(at);
            }
            None => w.u8(0),
        }
        match self.scope_files {
            Some((a, b)) => {
                w.u8(1);
                w.u64(a);
                w.u64(b);
            }
            None => w.u8(0),
        }
        match self.scope_records {
            Some((a, b)) => {
                w.u8(1);
                w.u64(a);
                w.u64(b);
            }
            None => w.u8(0),
        }
        match &self.cost {
            Some(c) => {
                w.u8(1);
                w.opt_f64(c.credits);
                w.opt_f64(c.usd);
                match &c.quota {
                    Some((win, d)) => {
                        w.u8(1);
                        w.str(win);
                        w.f64(*d);
                    }
                    None => w.u8(0),
                }
                match &c.estimate {
                    Some(e) => {
                        w.u8(1);
                        w.str(&e.source);
                        w.str(&e.method);
                        w.opt_f64(e.credits);
                        match &e.quota {
                            Some((win, d)) => {
                                w.u8(1);
                                w.str(win);
                                w.f64(*d);
                            }
                            None => w.u8(0),
                        }
                    }
                    None => w.u8(0),
                }
            }
            None => w.u8(0),
        }
        match &self.review {
            Some(rev) => {
                w.u8(1);
                w.str(&rev.orig_of);
                w.str(&rev.effective_of);
                w.str(&rev.verdict);
                w.bool(rev.overturned);
                w.bool(rev.disputed);
                w.opt_str(&rev.by);
            }
            None => w.u8(0),
        }
        w.u32(self.flags.len() as u32);
        for f in &self.flags {
            w.str(&f.kind);
            w.str(&f.detail);
            w.str(&f.evidence);
            w.opt_str(&f.reported_by);
        }
        w.u32(self.evidence.len() as u32);
        for (l, p) in &self.evidence {
            w.str(l);
            w.str(p);
        }
        w.u32(self.metrics.len() as u32);
        for (k, v) in &self.metrics {
            w.str(k);
            w.f64(*v);
        }
        w.opt_str(&self.corrects);
        w.bool(self.has_void);
        w.bool(self.has_proto);
        w.bool(self.effective);
        w.0
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = BinReader::new(bytes);
        if r.u8()? != BIN_VERSION {
            fail!("unsupported", "scoreboard fact version");
        }
        let run_r = r.u32()?;
        let writer_id = r.u32()?;
        let cursor_ms = r.u64()?;
        let ended_epoch = r.i64()?;
        let ingested_ms = r.u64()?;
        let seq = r.u64()?;
        let id = r.str()?;
        let writer = r.str()?;
        let file = r.str()?;
        let ended_at = r.str()?;
        let written_at = r.str()?;
        let source_benchmark = r.bool()?;
        let benchmark = r.opt_str()?;
        let project = r.opt_str()?;
        let stream = r.opt_str()?;
        let task_id = r.str()?;
        let task_type = r.opt_str()?;
        let task_title = r.str()?;
        let task_size = r.task_size()?;
        let model_id = r.opt_str()?;
        let effort = r.opt_str()?;
        let harness = r.opt_str()?;
        let process = if r.u8()? == 0 {
            None
        } else {
            Some((r.str()?, r.opt_str()?))
        };
        let outcome_raw = r.str()?;
        let outcome_code = r.u8()?;
        let wall_seconds = r.opt_f64()?;
        let imported = if r.u8()? == 0 { None } else { Some((r.str()?, r.str()?)) };
        let scope_files = if r.u8()? == 0 { None } else { Some((r.u64()?, r.u64()?)) };
        let scope_records = if r.u8()? == 0 { None } else { Some((r.u64()?, r.u64()?)) };
        let cost = if r.u8()? == 0 {
            None
        } else {
            let credits = r.opt_f64()?;
            let usd = r.opt_f64()?;
            let quota = if r.u8()? == 0 { None } else { Some((r.str()?, r.f64()?)) };
            let estimate = if r.u8()? == 0 {
                None
            } else {
                let source = r.str()?;
                let method = r.str()?;
                let credits = r.opt_f64()?;
                let quota = if r.u8()? == 0 { None } else { Some((r.str()?, r.f64()?)) };
                Some(StoredEstimate {
                    source,
                    method,
                    credits,
                    quota,
                })
            };
            Some(StoredCost {
                credits,
                usd,
                quota,
                estimate,
            })
        };
        let review = if r.u8()? == 0 {
            None
        } else {
            Some(StoredReview {
                orig_of: r.str()?,
                effective_of: r.str()?,
                verdict: r.str()?,
                overturned: r.bool()?,
                disputed: r.bool()?,
                by: r.opt_str()?,
            })
        };
        let flags_len = r.u32()? as usize;
        let mut flags = Vec::with_capacity(flags_len);
        for _ in 0..flags_len {
            flags.push(StoredFlag {
                kind: r.str()?,
                detail: r.str()?,
                evidence: r.str()?,
                reported_by: r.opt_str()?,
            });
        }
        let ev_len = r.u32()? as usize;
        let mut evidence = Vec::with_capacity(ev_len);
        for _ in 0..ev_len {
            evidence.push((r.str()?, r.str()?));
        }
        let m_len = r.u32()? as usize;
        let mut metrics = Vec::with_capacity(m_len);
        for _ in 0..m_len {
            metrics.push((r.str()?, r.f64()?));
        }
        let corrects = r.opt_str()?;
        let has_void = r.bool()?;
        let has_proto = r.bool()?;
        let effective = r.bool()?;
        Ok(Self {
            r: run_r,
            writer_id,
            cursor_ms,
            ended_epoch,
            ingested_ms,
            seq,
            id,
            writer,
            file,
            ended_at,
            written_at,
            source_benchmark,
            benchmark,
            project,
            stream,
            task_id,
            task_type,
            task_title,
            task_size,
            model_id,
            effort,
            harness,
            process,
            outcome_raw,
            outcome_code,
            wall_seconds,
            imported,
            scope_files,
            scope_records,
            cost,
            review,
            flags,
            evidence,
            metrics,
            corrects,
            has_void,
            has_proto,
            effective,
        })
    }
}

impl BucketAgg {
    pub fn encode(&self) -> Vec<u8> {
        let mut w = BinWriter::with_capacity(512);
        w.u8(BIN_VERSION);
        w.u64(self.n);
        w.u64(self.imported);
        w.u64(self.proto_count);
        w.u64(self.max_cursor.0);
        w.u32(self.max_cursor.1);
        w.u64(self.outcome_done);
        w.u64(self.outcome_known);
        w.u32(self.unknown_outcomes.len() as u32);
        for (k, v) in &self.unknown_outcomes {
            w.str(k);
            w.u64(*v);
        }
        w.u32(self.negative_quota_runs.len() as u32);
        for (ms, r, id) in &self.negative_quota_runs {
            w.u64(*ms);
            w.u32(*r);
            w.str(id);
        }
        w.u64(self.cheating_count);
        w.u64(self.laziness_counted);
        w.u64(self.laziness_lazy);
        w.u32(self.speed_samples.len() as u32);
        for s in &self.speed_samples {
            w.f64(s.wall_seconds);
            w.task_size(&s.task_size);
        }
        w.u32(self.thoroughness_files.len() as u32);
        for v in &self.thoroughness_files {
            w.f64(*v);
        }
        w.u32(self.thoroughness_records.len() as u32);
        for v in &self.thoroughness_records {
            w.f64(*v);
        }
        w.u32(self.thoroughness_min.len() as u32);
        for v in &self.thoroughness_min {
            w.f64(*v);
        }
        w.u32(self.cost_samples.len() as u32);
        for cs in &self.cost_samples {
            w.u64(cs.cursor_ms);
            w.u32(cs.r);
            w.bool(cs.outcome_done);
            w.task_size(&cs.task_size);
            w.stored_cost(&cs.cost);
        }
        w.u32(self.metric_rows.len() as u32);
        for row in &self.metric_rows {
            w.u32(row.len() as u32);
            for (k, v) in row {
                w.str(k);
                w.f64(*v);
            }
        }
        w.u32(self.flags.len() as u32);
        for f in &self.flags {
            w.i64(f.ended_epoch);
            w.str(&f.run_id);
            w.u32(f.flag_idx);
            w.str(&f.kind);
            w.str(&f.detail);
            w.str(&f.evidence);
            w.opt_str(&f.reported_by);
        }
        w.u32(self.joined_reviews.len() as u32);
        for rev in &self.joined_reviews {
            w.u32(rev.target_r);
            w.u64(rev.target_cursor_ms);
            w.str(&rev.target_id);
            w.opt_str(&rev.target_project);
            w.str(&rev.target_task_id);
            w.bool(rev.target_outcome_done);
            w.i64(rev.target_ended_epoch);
            w.u32(rev.review_r);
            w.u64(rev.review_cursor_ms);
            w.i64(rev.review_ended_epoch);
            w.str(&rev.review_id);
            w.bool(rev.review_source_benchmark);
            w.opt_str(&rev.review_task_type);
            w.opt_str(&rev.review_model_id);
            w.opt_str(&rev.review_effort);
            w.str(&rev.review_file);
            w.bool(rev.review_has_proto);
            w.str(&rev.verdict);
            w.bool(rev.overturned);
            w.bool(rev.disputed);
            w.opt_str(&rev.by);
            w.opt_str(&rev.evidence0_path);
        }
        w.u32(self.own_review_refs.len() as u32);
        for own in &self.own_review_refs {
            w.u32(own.self_r);
            w.i64(own.self_ended_epoch);
            w.bool(own.self_task_type_is_review);
            w.opt_u32(own.target_r);
            w.opt_u64(own.target_cursor_ms);
            w.opt_i64(own.target_ended_epoch);
            w.opt_bool(own.target_source_benchmark);
            w.opt_str(&own.target_task_type);
            w.bool(own.target_effective);
        }
        w.u32(self.recent_runs.len() as u32);
        for run in &self.recent_runs {
            w.u32(run.r);
            w.u64(run.cursor_ms);
            w.i64(run.ended_epoch);
            w.str(&run.id);
            w.str(&run.writer);
            w.bool(run.source_benchmark);
            w.opt_str(&run.benchmark);
            w.opt_str(&run.project);
            w.opt_str(&run.stream);
            w.str(&run.task_id);
            w.opt_str(&run.task_type);
            w.str(&run.task_title);
            w.opt_str(&run.model_id);
            w.opt_str(&run.harness);
            w.opt_str(&run.effort);
            match &run.process {
                Some((id, ver)) => {
                    w.u8(1);
                    w.str(id);
                    w.opt_str(ver);
                }
                None => w.u8(0),
            }
            w.str(&run.outcome);
            w.str(&run.ended_at);
            w.opt_f64(run.wall_seconds);
            match &run.imported {
                Some((from, at)) => {
                    w.u8(1);
                    w.str(from);
                    w.str(at);
                }
                None => w.u8(0),
            }
            match &run.review {
                Some(rev) => {
                    w.u8(1);
                    w.str(&rev.of);
                    w.str(&rev.verdict);
                    w.bool(rev.overturned);
                    w.bool(rev.disputed);
                    w.opt_str(&rev.by);
                }
                None => w.u8(0),
            }
            w.u32(run.flags_len);
            w.u32(run.evidence.len() as u32);
            for (l, p) in &run.evidence {
                w.str(l);
                w.str(p);
            }
        }
        w.u32(self.writers.len() as u32);
        for wid in &self.writers {
            w.u32(*wid);
        }
        w.0
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let mut r = BinReader::new(bytes);
        if r.u8()? != BIN_VERSION {
            fail!("unsupported", "scoreboard bucket version");
        }
        let n = r.u64()?;
        let imported = r.u64()?;
        let proto_count = r.u64()?;
        let max_cursor = (r.u64()?, r.u32()?);
        let outcome_done = r.u64()?;
        let outcome_known = r.u64()?;
        let uo_len = r.u32()? as usize;
        let mut unknown_outcomes = BTreeMap::new();
        for _ in 0..uo_len {
            unknown_outcomes.insert(r.str()?, r.u64()?);
        }
        let nq_len = r.u32()? as usize;
        let mut negative_quota_runs = Vec::with_capacity(nq_len);
        for _ in 0..nq_len {
            negative_quota_runs.push((r.u64()?, r.u32()?, r.str()?));
        }
        let cheating_count = r.u64()?;
        let laziness_counted = r.u64()?;
        let laziness_lazy = r.u64()?;
        let sp_len = r.u32()? as usize;
        let mut speed_samples = Vec::with_capacity(sp_len);
        for _ in 0..sp_len {
            speed_samples.push(SpeedSample {
                wall_seconds: r.f64()?,
                task_size: r.task_size()?,
            });
        }
        let tf_len = r.u32()? as usize;
        let mut thoroughness_files = Vec::with_capacity(tf_len);
        for _ in 0..tf_len {
            thoroughness_files.push(r.f64()?);
        }
        let tr_len = r.u32()? as usize;
        let mut thoroughness_records = Vec::with_capacity(tr_len);
        for _ in 0..tr_len {
            thoroughness_records.push(r.f64()?);
        }
        let tm_len = r.u32()? as usize;
        let mut thoroughness_min = Vec::with_capacity(tm_len);
        for _ in 0..tm_len {
            thoroughness_min.push(r.f64()?);
        }
        let cs_len = r.u32()? as usize;
        let mut cost_samples = Vec::with_capacity(cs_len);
        for _ in 0..cs_len {
            cost_samples.push(BucketCostSample {
                cursor_ms: r.u64()?,
                r: r.u32()?,
                outcome_done: r.bool()?,
                task_size: r.task_size()?,
                cost: r.stored_cost()?,
            });
        }
        let mr_len = r.u32()? as usize;
        let mut metric_rows = Vec::with_capacity(mr_len);
        for _ in 0..mr_len {
            let row_len = r.u32()? as usize;
            let mut row = Vec::with_capacity(row_len);
            for _ in 0..row_len {
                row.push((r.str()?, r.f64()?));
            }
            metric_rows.push(row);
        }
        let fl_len = r.u32()? as usize;
        let mut flags = Vec::with_capacity(fl_len);
        for _ in 0..fl_len {
            flags.push(BucketFlag {
                ended_epoch: r.i64()?,
                run_id: r.str()?,
                flag_idx: r.u32()?,
                kind: r.str()?,
                detail: r.str()?,
                evidence: r.str()?,
                reported_by: r.opt_str()?,
            });
        }
        let jr_len = r.u32()? as usize;
        let mut joined_reviews = Vec::with_capacity(jr_len);
        for _ in 0..jr_len {
            joined_reviews.push(BucketJoinedReview {
                target_r: r.u32()?,
                target_cursor_ms: r.u64()?,
                target_id: r.str()?,
                target_project: r.opt_str()?,
                target_task_id: r.str()?,
                target_outcome_done: r.bool()?,
                target_ended_epoch: r.i64()?,
                review_r: r.u32()?,
                review_cursor_ms: r.u64()?,
                review_ended_epoch: r.i64()?,
                review_id: r.str()?,
                review_source_benchmark: r.bool()?,
                review_task_type: r.opt_str()?,
                review_model_id: r.opt_str()?,
                review_effort: r.opt_str()?,
                review_file: r.str()?,
                review_has_proto: r.bool()?,
                verdict: r.str()?,
                overturned: r.bool()?,
                disputed: r.bool()?,
                by: r.opt_str()?,
                evidence0_path: r.opt_str()?,
            });
        }
        let or_len = r.u32()? as usize;
        let mut own_review_refs = Vec::with_capacity(or_len);
        for _ in 0..or_len {
            own_review_refs.push(OwnReviewRef {
                self_r: r.u32()?,
                self_ended_epoch: r.i64()?,
                self_task_type_is_review: r.bool()?,
                target_r: r.opt_u32()?,
                target_cursor_ms: r.opt_u64()?,
                target_ended_epoch: r.opt_i64()?,
                target_source_benchmark: r.opt_bool()?,
                target_task_type: r.opt_str()?,
                target_effective: r.bool()?,
            });
        }
        let rr_len = r.u32()? as usize;
        let mut recent_runs = Vec::with_capacity(rr_len);
        for _ in 0..rr_len {
            let run_r = r.u32()?;
            let cursor_ms = r.u64()?;
            let ended_epoch = r.i64()?;
            let id = r.str()?;
            let writer = r.str()?;
            let source_benchmark = r.bool()?;
            let benchmark = r.opt_str()?;
            let project = r.opt_str()?;
            let stream = r.opt_str()?;
            let task_id = r.str()?;
            let task_type = r.opt_str()?;
            let task_title = r.str()?;
            let model_id = r.opt_str()?;
            let harness = r.opt_str()?;
            let effort = r.opt_str()?;
            let process = if r.u8()? == 0 {
                None
            } else {
                Some((r.str()?, r.opt_str()?))
            };
            let outcome = r.str()?;
            let ended_at = r.str()?;
            let wall_seconds = r.opt_f64()?;
            let imported = if r.u8()? == 0 { None } else { Some((r.str()?, r.str()?)) };
            let review = if r.u8()? == 0 {
                None
            } else {
                Some(SummaryReview {
                    of: r.str()?,
                    verdict: r.str()?,
                    overturned: r.bool()?,
                    disputed: r.bool()?,
                    by: r.opt_str()?,
                })
            };
            let flags_len = r.u32()?;
            let ev_len = r.u32()? as usize;
            let mut evidence = Vec::with_capacity(ev_len);
            for _ in 0..ev_len {
                evidence.push((r.str()?, r.str()?));
            }
            recent_runs.push(BucketRunSummary {
                r: run_r,
                cursor_ms,
                ended_epoch,
                id,
                writer,
                source_benchmark,
                benchmark,
                project,
                stream,
                task_id,
                task_type,
                task_title,
                model_id,
                harness,
                effort,
                process,
                outcome,
                ended_at,
                wall_seconds,
                imported,
                review,
                flags_len,
                evidence,
            });
        }
        let w_len = r.u32()? as usize;
        let mut writers = BTreeSet::new();
        for _ in 0..w_len {
            writers.insert(r.u32()?);
        }
        Ok(Self {
            n,
            imported,
            proto_count,
            max_cursor,
            outcome_done,
            outcome_known,
            unknown_outcomes,
            negative_quota_runs,
            cheating_count,
            laziness_counted,
            laziness_lazy,
            speed_samples,
            thoroughness_files,
            thoroughness_records,
            thoroughness_min,
            cost_samples,
            metric_rows,
            flags,
            joined_reviews,
            own_review_refs,
            recent_runs,
            writers,
        })
    }
}

// ---------------------------------------------------------------------------------------------
// Fact extraction from Head + raw doc bytes

fn extract_fact<T: ReadableTable<&'static [u8], &'static [u8]>>(
    dict: &mut Dict<'_, T>,
    r: u32,
    head: &Head,
    doc_bytes: &[u8],
) -> Result<Option<StoredRunFact>> {
    let writer = dict.field(head, at::WRITER)?.unwrap_or_default();
    let text = String::from_utf8_lossy(doc_bytes);
    let Ok(mut doc) = run_schema::parse_run_text(&text, &writer) else {
        return Ok(None);
    };
    let shown = dict.field(head, at::MODEL)?.or(dict.field(head, at::MODEL_RAW)?);
    let effort = dict.field(head, at::EFFORT)?;
    let harness = dict.field(head, at::HARNESS)?;
    let file = dict.field(head, at::PATH)?.unwrap_or_default();
    doc["model"]["id"] = shown.as_deref().map(Json::from).unwrap_or(Json::Null);
    doc["model"]["effort"] = effort.as_deref().map(Json::from).unwrap_or(Json::Null);
    doc["model"]["harness"] = harness.as_deref().map(Json::from).unwrap_or(Json::Null);
    doc["model"]["provider"] = Json::Null;
    // Exact parity with `scoreboard_owned::held_docs` checking `proto_named(&state)`.
    let has_proto = PROTO_NAMES.contains(&file.as_str()) || proto_named(&doc);
    let ended_at = doc["endedAt"].as_str().unwrap_or_default().to_string();
    let ended_epoch = aac_num::ecma::parse_instant(&ended_at)
        .or_else(|| run_schema::iso_ms(&ended_at))
        .unwrap_or(head.ended_ms as i64);
    let written_at = doc["writtenAt"].as_str().unwrap_or_default().to_string();
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
            p.get("id").and_then(Json::as_str).unwrap_or_default().to_string(),
            p.get("version").and_then(Json::as_str).map(str::to_string),
        )
    });
    let outcome_raw = doc["outcome"].as_str().unwrap_or_default().to_string();
    let oc = outcome_code(&outcome_raw);
    let imported = doc["imported"].as_object().map(|i| {
        (
            i.get("from").and_then(Json::as_str).unwrap_or_default().to_string(),
            i.get("at").and_then(Json::as_str).unwrap_or_default().to_string(),
        )
    });
    let scope_files = match (
        doc["scope"]["filesRead"].as_u64(),
        doc["scope"]["filesAssigned"].as_u64(),
    ) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    };
    let scope_records = match (
        doc["scope"]["recordsJudged"].as_u64(),
        doc["scope"]["recordsAssigned"].as_u64(),
    ) {
        (Some(a), Some(b)) => Some((a, b)),
        _ => None,
    };
    let read_quota = |v: &Json| -> Option<(String, f64)> {
        let obj = v.as_object()?;
        let win = obj.get("window")?.as_str()?.to_string();
        let delta = obj.get("deltaPct")?.as_f64().filter(|n| n.is_finite())?;
        Some((win, delta))
    };
    let cost = doc["cost"].as_object().map(|c| {
        let estimate = c.get("estimate").and_then(Json::as_object).map(|e| StoredEstimate {
            source: e.get("source").and_then(Json::as_str).unwrap_or_default().to_string(),
            method: e.get("method").and_then(Json::as_str).unwrap_or_default().to_string(),
            credits: e.get("credits").and_then(Json::as_f64).filter(|n| n.is_finite()),
            quota: e.get("quota").and_then(read_quota),
        });
        StoredCost {
            credits: c.get("credits").and_then(Json::as_f64).filter(|n| n.is_finite()),
            usd: c.get("usd").and_then(Json::as_f64).filter(|n| n.is_finite()),
            quota: c.get("quota").and_then(read_quota),
            estimate,
        }
    });
    let review = doc["review"].as_object().map(|rev| {
        let of = rev.get("of").and_then(Json::as_str).unwrap_or_default().to_string();
        StoredReview {
            orig_of: of.clone(),
            effective_of: of,
            verdict: rev
                .get("verdict")
                .and_then(Json::as_str)
                .unwrap_or_default()
                .to_string(),
            overturned: rev.get("overturned").and_then(Json::as_bool).unwrap_or(false),
            disputed: rev.get("disputed").and_then(Json::as_bool).unwrap_or(false),
            by: rev.get("by").and_then(Json::as_str).map(str::to_string),
        }
    });
    let flags = doc["flags"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|f| StoredFlag {
                    kind: f["kind"].as_str().unwrap_or_default().to_string(),
                    detail: f["detail"].as_str().unwrap_or_default().to_string(),
                    evidence: f["evidence"].as_str().unwrap_or_default().to_string(),
                    reported_by: f["reportedBy"].as_str().map(str::to_string),
                })
                .collect()
        })
        .unwrap_or_default();
    let evidence = doc["evidence"]
        .as_array()
        .map(|items| {
            items
                .iter()
                .map(|e| {
                    (
                        e["label"].as_str().unwrap_or_default().to_string(),
                        e["path"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    let metrics = doc["metrics"]
        .as_object()
        .map(|m| {
            m.iter()
                .filter_map(|(k, v)| v.as_f64().filter(|n| n.is_finite()).map(|n| (k.clone(), n)))
                .collect()
        })
        .unwrap_or_default();
    Ok(Some(StoredRunFact {
        r,
        writer_id: head.id(at::WRITER),
        cursor_ms: head.ended_ms,
        ended_epoch,
        ingested_ms: head.ingested_ms,
        seq: head.seq,
        id: doc["id"].as_str().unwrap_or_default().to_string(),
        writer,
        file,
        ended_at,
        written_at,
        source_benchmark: doc["source"].as_str() == Some("benchmark"),
        benchmark: doc["benchmark"].as_str().map(str::to_string),
        project: doc["project"].as_str().map(str::to_string),
        stream: doc["stream"].as_str().map(str::to_string),
        task_id: task["id"].as_str().unwrap_or_default().to_string(),
        task_type: task["type"].as_str().map(str::to_string),
        task_title: task["title"].as_str().unwrap_or_default().to_string(),
        task_size,
        model_id: shown,
        effort,
        harness,
        process,
        outcome_raw,
        outcome_code: oc,
        wall_seconds: doc["wallSeconds"].as_f64().filter(|n| n.is_finite()),
        imported,
        scope_files,
        scope_records,
        cost,
        review,
        flags,
        evidence,
        metrics,
        corrects: doc.get("corrects").and_then(Json::as_str).map(str::to_string),
        has_void: doc.get("void").is_some_and(Json::is_object),
        has_proto,
        effective: true,
    }))
}

fn day_key_from_file(file: &str) -> [u8; 10] {
    // Expected layout: writers/<writer>/runs/YYYY-MM-DD/<name>.json
    let mut parts = file.split('/');
    let day = parts.nth(3).unwrap_or("");
    let mut out = [0u8; 10];
    let bytes = day.as_bytes();
    let len = bytes.len().min(10);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}

fn writer_day_key(writer_id: u32, file: &str, r: u32) -> Vec<u8> {
    let day = day_key_from_file(file);
    Key::new().u32(writer_id).bytes(&day).u32(r).0
}

fn build_joined_review(target: &StoredRunFact, review: &StoredRunFact) -> Option<BucketJoinedReview> {
    let rev = review.review.as_ref()?;
    Some(BucketJoinedReview {
        target_r: target.r,
        target_cursor_ms: target.cursor_ms,
        target_id: target.id.clone(),
        target_project: target.project.clone(),
        target_task_id: target.task_id.clone(),
        target_outcome_done: target.outcome_code == 1,
        target_ended_epoch: target.ended_epoch,
        review_r: review.r,
        review_cursor_ms: review.cursor_ms,
        review_ended_epoch: review.ended_epoch,
        review_id: review.id.clone(),
        review_source_benchmark: review.source_benchmark,
        review_task_type: review.task_type.clone(),
        review_model_id: review.model_id.clone(),
        review_effort: review.effort.clone(),
        review_file: review.file.clone(),
        review_has_proto: review.has_proto,
        verdict: rev.verdict.clone(),
        overturned: rev.overturned,
        disputed: rev.disputed,
        by: rev.by.clone(),
        evidence0_path: review.evidence.first().map(|(_, p)| p.clone()),
    })
}

fn build_own_review_ref(fact: &StoredRunFact, target: Option<&StoredRunFact>) -> Option<OwnReviewRef> {
    let _rev = fact.review.as_ref()?;
    Some(OwnReviewRef {
        self_r: fact.r,
        self_ended_epoch: fact.ended_epoch,
        self_task_type_is_review: fact.task_type.as_deref() == Some("review"),
        target_r: target.map(|t| t.r),
        target_cursor_ms: target.map(|t| t.cursor_ms),
        target_ended_epoch: target.map(|t| t.ended_epoch),
        target_source_benchmark: target.map(|t| t.source_benchmark),
        target_task_type: target.and_then(|t| t.task_type.clone()),
        target_effective: target.is_some_and(|t| t.effective),
    })
}

fn load_fact_rd<T: ReadableTable<&'static [u8], &'static [u8]>>(table: &T, r: u32) -> Result<Option<StoredRunFact>> {
    table
        .get(Key::new().u32(r).as_slice())?
        .map(|v| StoredRunFact::decode(v.value()))
        .transpose()
}

fn put_fact_tx(txn: &WriteTransaction, fact: &StoredRunFact) -> Result<()> {
    txn.open_table(RUN_SB_FACT)?
        .insert(Key::new().u32(fact.r).as_slice(), fact.encode().as_slice())?;
    Ok(())
}

fn path_run_key(path_hash: &[u8; 16], r: u32) -> Vec<u8> {
    Key::new().bytes(path_hash).u32(r).0
}

fn has_any_path_hash_tx(txn: &WriteTransaction, path_hash: &[u8; 16]) -> Result<bool> {
    let tbl = txn.open_table(RUN_SB_PATH_RUNS)?;
    let lo = Key::new().bytes(path_hash).u32(0);
    let hi = Key::new().bytes(path_hash).u32(u32::MAX);
    Ok(tbl.range(lo.as_slice()..=hi.as_slice())?.next().is_some())
}

fn runs_at_path_hash_rd<T: ReadableTable<&'static [u8], ()>>(table: &T, path_hash: &[u8]) -> Result<Vec<u32>> {
    if path_hash.len() != 16 {
        return Ok(Vec::new());
    }
    let lo = Key::new().bytes(path_hash).u32(0);
    let hi = Key::new().bytes(path_hash).u32(u32::MAX);
    let mut out = Vec::new();
    for item in table.range(lo.as_slice()..=hi.as_slice())? {
        let (k, _) = item?;
        if k.value().len() == 20 {
            out.push(be_u32(k.value(), 16));
        }
    }
    Ok(out)
}

fn has_waiting_correction_for_tx(txn: &WriteTransaction, target_r: u32) -> Result<bool> {
    let tbl = txn.open_table(RUN_SB_CORRECTIONS)?;
    let lo = Key::new().u32(target_r).u32(0);
    let hi = Key::new().u32(target_r).u32(u32::MAX);
    Ok(tbl.range(lo.as_slice()..=hi.as_slice())?.next().is_some())
}

fn has_waiting_review_for_tx(txn: &WriteTransaction, target_r: u32) -> Result<bool> {
    let tbl = txn.open_table(RUN_SB_REVIEWS_BY_TARGET)?;
    let lo = Key::new().u32(target_r).u32(0);
    let hi = Key::new().u32(target_r).u32(u32::MAX);
    Ok(tbl.range(lo.as_slice()..=hi.as_slice())?.next().is_some())
}

fn reviews_of_target_rd<T: ReadableTable<&'static [u8], ()>>(table: &T, target_r: u32) -> Result<Vec<u32>> {
    let lo = Key::new().u32(target_r).u32(0);
    let hi = Key::new().u32(target_r).u32(u32::MAX);
    let mut out = Vec::new();
    for item in table.range(lo.as_slice()..=hi.as_slice())? {
        let (k, _) = item?;
        out.push(be_u32(k.value(), 4));
    }
    Ok(out)
}

fn load_payload_doc_tx(txn: &WriteTransaction, head: &Head) -> Result<Option<Vec<u8>>> {
    let payloads = txn.open_table(RUN_PAYLOAD)?;
    let Some(bytes) = payloads.get(Key::new().u32(head.loc.day).u32(head.loc.block).as_slice())? else {
        return Ok(None);
    };
    let docs = block_decode(bytes.value())?.1;
    Ok(docs.get(head.loc.slot as usize).cloned())
}

fn read_effective_count_tx(txn: &WriteTransaction) -> Result<u64> {
    Ok(meta_get(txn, META_EFFECTIVE_COUNT)?
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(0))
}

fn write_effective_count_tx(txn: &WriteTransaction, count: u64) -> Result<()> {
    meta_set(txn, META_EFFECTIVE_COUNT, &count.to_string())
}

/// Update `run_sb_*` tables in the SAME write transaction that recorded `run_head[r]`.
///
/// Common case (O(1) append): brand-new `run_id` (`!had_prior_head`), no `corrects`, no `void`,
/// no `review`, no existing effective head at its path, and no waiting correction or review
/// targeting `r`. Every other case marks the commit batch dirty for a clean full rebuild from
/// `RUN_HEAD` + `RUN_PAYLOAD` inside `finish_commit` before committing.
pub(crate) fn on_head_recorded(
    txn: &WriteTransaction,
    r: u32,
    new_head: &Head,
    had_prior_head: bool,
    doc_bytes: Option<&[u8]>,
) -> Result<()> {
    if had_prior_head || batch_needs_full_rebuild() {
        mark_full_rebuild();
        return Ok(());
    }
    let owned_doc: Vec<u8>;
    let bytes: &[u8] = match doc_bytes {
        Some(b) => b,
        None => match load_payload_doc_tx(txn, new_head)? {
            Some(b) => {
                owned_doc = b;
                &owned_doc
            }
            None => {
                mark_full_rebuild();
                return Ok(());
            }
        },
    };
    let Some(mut new_fact) = ({
        let dict_table = txn.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        extract_fact(&mut dict, r, new_head, bytes)?
    }) else {
        mark_full_rebuild();
        return Ok(());
    };
    let ph = path_key(&new_fact.file);
    let is_rare = new_fact.corrects.is_some()
        || new_fact.has_void
        || new_fact.review.is_some()
        || has_any_path_hash_tx(txn, &ph)?
        || has_waiting_correction_for_tx(txn, r)?
        || has_waiting_review_for_tx(txn, r)?;
    if is_rare {
        mark_full_rebuild();
        return Ok(());
    }

    // Fast O(1) append for a brand-new plain run at a fresh path.
    new_fact.effective = true;
    put_fact_tx(txn, &new_fact)?;
    txn.open_table(RUN_SB_BY_WRITER_DAY)?
        .insert(writer_day_key(new_fact.writer_id, &new_fact.file, r).as_slice(), ())?;
    txn.open_table(RUN_SB_PATH_RUNS)?
        .insert(path_run_key(&ph, r).as_slice(), ())?;
    let key_bytes = BucketKey::from_fact(&new_fact).encode();
    let mut bucket = {
        let tbl = txn.open_table(RUN_SB_BUCKET)?;
        tbl.get(key_bytes.as_slice())?
            .map(|v| BucketAgg::decode(v.value()))
            .transpose()?
            .unwrap_or_default()
    };
    bucket.fold_run(&new_fact, &[], None);
    txn.open_table(RUN_SB_BUCKET)?
        .insert(key_bytes.as_slice(), bucket.encode().as_slice())?;
    let eff = read_effective_count_tx(txn)?;
    write_effective_count_tx(txn, eff + 1)?;

    let entry_hash = head_entry_hash(r, new_head);
    BATCH_STATE.with(|cell| {
        if let Some(st) = cell.borrow_mut().as_mut() {
            st.appended_heads += 1;
            st.appended_digest = add_digest(st.appended_digest, entry_hash);
        }
    });
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Verified migration & canonical full rebuild from RUN_HEAD + RUN_PAYLOAD

/// Verify the aggregate trust stamp against post-replay `RUN_HEAD` + `CHECKPOINT["runs/ledger"]`
/// and rebuild once from scratch on any mismatch (absent, version bump, rollback/upgrade, or
/// crash-recovery tail).
pub(crate) fn ensure_migrated(db: &Database) -> Result<()> {
    let up_to_date = {
        let read = db.begin_read()?;
        verify_trust_stamp_rd(&read)?
    };
    if up_to_date {
        return Ok(());
    }
    rebuild_all_aggregates(db)
}

/// Full atomic rebuild of all `run_sb_*` tables in a standalone write transaction, stamping the
/// verified trust checkpoint from inside that transaction.
pub(crate) fn rebuild_all_aggregates(db: &Database) -> Result<()> {
    let txn = crate::engine::begin(db)?;
    let cp_seq = {
        let cp_table = txn.open_table(crate::db::CHECKPOINT)?;
        crate::db::read_checkpoint(&cp_table, "runs/ledger")?
            .map(|c| c.last_sequence_id)
            .unwrap_or(0)
    };
    rebuild_all_aggregates_in_write(&txn, cp_seq)?;
    txn.commit()?;
    Ok(())
}

/// Canonical full rebuild of all `run_sb_*` tables inside an existing `WriteTransaction`, streaming
/// payload blocks with a bounded 16-block cache so RSS stays small.
pub(crate) fn rebuild_all_aggregates_in_write(txn: &WriteTransaction, checkpoint_seq: u64) -> Result<()> {
    // Pass 1: stream RUN_BY_TIME -> RUN_HEAD -> RUN_PAYLOAD into compact `StoredRunFact`s, and
    // compute the canonical `(head_count, head_digest)` directly from `RUN_HEAD`.
    let mut facts: Vec<StoredRunFact> = Vec::new();
    let (head_count, head_digest) = {
        let by_time = txn.open_table(RUN_BY_TIME)?;
        let heads = txn.open_table(RUN_HEAD)?;
        let payloads = txn.open_table(RUN_PAYLOAD)?;
        let dict_table = txn.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let mut blocks: HashMap<(u32, u32), Vec<Vec<u8>>> = HashMap::new();
        for item in by_time.iter()? {
            let (k, _) = item?;
            let r = be_u32(k.value(), 8);
            let Some(hv) = heads.get(Key::new().u32(r).as_slice())? else {
                continue;
            };
            let head = Head::decode(hv.value())?;
            let block_key = (head.loc.day, head.loc.block);
            if !blocks.contains_key(&block_key) {
                if blocks.len() >= 16 {
                    blocks.clear();
                }
                let Some(bytes) = payloads.get(Key::new().u32(head.loc.day).u32(head.loc.block).as_slice())? else {
                    continue;
                };
                blocks.insert(block_key, block_decode(bytes.value())?.1);
            }
            let Some(doc_bytes) = blocks[&block_key].get(head.loc.slot as usize) else {
                continue;
            };
            if let Some(fact) = extract_fact(&mut dict, r, &head, doc_bytes)? {
                facts.push(fact);
            }
        }
        compute_heads_stamp_from_table(&heads)?
    };

    // Pass 2: resolve corrections over the correction-chain closure only (if any `corrects` exist).
    let mut refused_reasons: Vec<(u32, String)> = Vec::new();
    let has_any_corrects = facts.iter().any(|f| f.corrects.is_some() || f.has_void);
    if has_any_corrects {
        let id_to_idx: HashMap<&str, usize> = facts.iter().enumerate().map(|(i, f)| (f.id.as_str(), i)).collect();
        let mut rev_by_target: HashMap<&str, Vec<usize>> = HashMap::new();
        for (i, f) in facts.iter().enumerate() {
            if let Some(rev) = &f.review {
                rev_by_target.entry(rev.orig_of.as_str()).or_default().push(i);
            }
        }
        let mut closure: HashSet<usize> = HashSet::new();
        let mut queue: Vec<usize> = Vec::new();
        for (i, f) in facts.iter().enumerate() {
            if (f.corrects.is_some() || f.has_void) && closure.insert(i) {
                queue.push(i);
            }
        }
        let mut pos = 0;
        while pos < queue.len() {
            let idx = queue[pos];
            pos += 1;
            if let Some(target_id) = &facts[idx].corrects
                && let Some(&t_idx) = id_to_idx.get(target_id.as_str())
                && closure.insert(t_idx)
            {
                queue.push(t_idx);
            }
            if let Some(revs) = rev_by_target.get(facts[idx].id.as_str()) {
                for &ri in revs {
                    if closure.insert(ri) {
                        queue.push(ri);
                    }
                }
            }
        }
        let mut closure_indices: Vec<usize> = closure.into_iter().collect();
        closure_indices.sort_unstable();
        let records: Vec<Json> = closure_indices
            .iter()
            .map(|&idx| {
                let f = &facts[idx];
                let mut doc = json!({
                    "id": f.id,
                    "writer": f.writer,
                    "seq": f.seq,
                    "writtenAt": f.written_at,
                });
                if let Some(target) = &f.corrects {
                    doc["corrects"] = json!(target);
                }
                if f.has_void {
                    doc["void"] = json!({"reason": "void"});
                }
                if let Some(rev) = &f.review {
                    doc["review"] = json!({"of": rev.orig_of});
                }
                json!({ "idx": idx, "doc": doc })
            })
            .collect();
        let res = crate::run_corrections::resolve(&records, |v| v.as_str().and_then(run_schema::iso_ms));
        let mut eff_of_map: HashMap<usize, Option<String>> = HashMap::new();
        for row in &res.rows {
            let idx = row["idx"].as_u64().unwrap_or(0) as usize;
            eff_of_map.insert(idx, row["doc"]["review"]["of"].as_str().map(str::to_string));
        }
        for &idx in &closure_indices {
            let f = &mut facts[idx];
            if let Some(reason) = res.refused.get(&f.id) {
                refused_reasons.push((f.r, reason.clone()));
            }
            if let Some(new_of) = eff_of_map.get(&idx) {
                f.effective = true;
                if let (Some(rev), Some(of)) = (&mut f.review, new_of) {
                    rev.effective_of = of.clone();
                }
            } else {
                f.effective = false;
            }
        }
    }

    // Pass 3: index effective runs, join reviews, and fold into `BucketAgg`s in exact
    // `RUN_BY_TIME` `(cursor_ms, r)` ascending order.
    let id_to_idx: HashMap<String, usize> = facts.iter().enumerate().map(|(i, f)| (f.id.clone(), i)).collect();
    let mut reviews_by_target_idx: HashMap<usize, Vec<usize>> = HashMap::new();
    for (i, f) in facts.iter().enumerate() {
        if f.effective
            && let Some(rev) = &f.review
            && let Some(&t_idx) = id_to_idx.get(&rev.effective_of)
        {
            reviews_by_target_idx.entry(t_idx).or_default().push(i);
        }
    }
    let mut buckets: HashMap<BucketKey, BucketAgg> = HashMap::new();
    let mut effective_count = 0u64;
    for (i, fact) in facts.iter().enumerate() {
        if !fact.effective {
            continue;
        }
        effective_count += 1;
        let joined: Vec<BucketJoinedReview> = reviews_by_target_idx
            .get(&i)
            .map(|rev_indices| {
                rev_indices
                    .iter()
                    .filter_map(|&ri| build_joined_review(fact, &facts[ri]))
                    .collect()
            })
            .unwrap_or_default();
        let own_ref = fact.review.as_ref().map(|rev| {
            let target = id_to_idx.get(&rev.effective_of).map(|&ti| &facts[ti]);
            build_own_review_ref(fact, target).unwrap()
        });
        let key = BucketKey::from_fact(fact);
        buckets.entry(key).or_default().fold_run(fact, &joined, own_ref);
    }

    // Pass 4: replace all `run_sb_*` tables + trust stamp atomically inside `txn`.
    delete_tables(txn)?;
    create_tables(txn)?;
    let mut missing_run_ids: BTreeSet<String> = BTreeSet::new();
    {
        let rev_tbl = txn.open_table(RUN_DICT_REV)?;
        for fact in &facts {
            if let Some(corr) = &fact.corrects
                && lookup_id(&rev_tbl, kind::RUN_ID, corr)?.is_none()
            {
                missing_run_ids.insert(corr.clone());
            }
            if let Some(rev) = &fact.review
                && lookup_id(&rev_tbl, kind::RUN_ID, &rev.effective_of)?.is_none()
            {
                missing_run_ids.insert(rev.effective_of.clone());
            }
        }
    }
    for missing in missing_run_ids {
        runs::intern_run_id(txn, &missing)?;
    }
    let mut run_id_cache: HashMap<String, u32> = facts.iter().map(|f| (f.id.clone(), f.r)).collect();
    {
        let rev_tbl = txn.open_table(RUN_DICT_REV)?;
        for fact in &facts {
            if let Some(corr) = &fact.corrects
                && !run_id_cache.contains_key(corr)
                && let Some(id) = lookup_id(&rev_tbl, kind::RUN_ID, corr)?
            {
                run_id_cache.insert(corr.clone(), id);
            }
            if let Some(rev) = &fact.review
                && !run_id_cache.contains_key(&rev.effective_of)
                && let Some(id) = lookup_id(&rev_tbl, kind::RUN_ID, &rev.effective_of)?
            {
                run_id_cache.insert(rev.effective_of.clone(), id);
            }
        }
    }
    {
        let mut fact_tbl = txn.open_table(RUN_SB_FACT)?;
        let mut rev_tbl = txn.open_table(RUN_SB_REVIEWS_BY_TARGET)?;
        let mut corr_tbl = txn.open_table(RUN_SB_CORRECTIONS)?;
        let mut path_tbl = txn.open_table(RUN_SB_PATH_RUNS)?;
        let mut wd_tbl = txn.open_table(RUN_SB_BY_WRITER_DAY)?;
        for fact in &facts {
            fact_tbl.insert(Key::new().u32(fact.r).as_slice(), fact.encode().as_slice())?;
            wd_tbl.insert(writer_day_key(fact.writer_id, &fact.file, fact.r).as_slice(), ())?;
            if let Some(corr) = &fact.corrects
                && let Some(&tr) = run_id_cache.get(corr)
            {
                corr_tbl.insert(Key::new().u32(tr).u32(fact.r).as_slice(), ())?;
            }
            if let Some(rev) = &fact.review
                && let Some(&tr) = run_id_cache.get(&rev.effective_of)
            {
                rev_tbl.insert(Key::new().u32(tr).u32(fact.r).as_slice(), ())?;
            }
            if fact.effective {
                let ph = path_key(&fact.file);
                path_tbl.insert(path_run_key(&ph, fact.r).as_slice(), ())?;
            }
        }
    }
    {
        let mut ref_tbl = txn.open_table(RUN_SB_CORRECTION_REFUSED)?;
        for (r, reason) in refused_reasons {
            ref_tbl.insert(Key::new().u32(r).as_slice(), reason.as_bytes())?;
        }
    }
    {
        let mut bucket_tbl = txn.open_table(RUN_SB_BUCKET)?;
        for (key, agg) in buckets {
            bucket_tbl.insert(key.encode().as_slice(), agg.encode().as_slice())?;
        }
    }
    let catalog = meta_get(txn, "catalog")?.unwrap_or_default();
    meta_set(txn, META_VERSION, &SB_AGG_VERSION.to_string())?;
    meta_set(txn, META_SEQ, &checkpoint_seq.to_string())?;
    meta_set(txn, META_CATALOG, &catalog)?;
    meta_set(txn, META_HEAD_COUNT, &head_count.to_string())?;
    meta_set(txn, META_HEAD_DIGEST, &format_digest(head_digest))?;
    write_effective_count_tx(txn, effective_count)?;
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// Fast read API for ScoreboardOwned (zero ResolvedHistory, zero payload reads)

#[derive(Clone, Debug, Default)]
pub struct ScoreboardFilter {
    pub from_ms: Option<u64>,
    pub to_ms: Option<u64>,
    /// `None` = all, `Some(false)` = observed, `Some(true)` = benchmark.
    pub source_benchmark: Option<bool>,
    pub task_type: Option<String>,
}

impl ScoreboardFilter {
    pub fn matches_fact(&self, fact: &StoredRunFact) -> bool {
        self.from_ms.is_none_or(|f| fact.cursor_ms >= f)
            && self.to_ms.is_none_or(|t| fact.cursor_ms <= t)
            && self.source_benchmark.is_none_or(|b| fact.source_benchmark == b)
            && self
                .task_type
                .as_deref()
                .is_none_or(|tt| fact.task_type.as_deref() == Some(tt))
    }

    pub fn matches_bucket_key(&self, key: &BucketKey) -> bool {
        self.source_benchmark.is_none_or(|b| key.source_benchmark == b)
            && self.task_type.as_deref().is_none_or(|tt| key.task_type == tt)
    }

    pub fn has_date_bounds(&self) -> bool {
        self.from_ms.is_some() || self.to_ms.is_some()
    }
}

/// Refused file row from `run_file` plus its resolved retained path (if a retained effective run
/// shares the path hash).
#[derive(Clone, Debug)]
pub struct StoreRefusedFile {
    pub path_hash: String,
    pub retained_file: Option<String>,
    pub reason: String,
    pub at_ms: u64,
}

/// Correction refusal problem from `run_sb_correction_refused`.
#[derive(Clone, Debug)]
pub struct CorrectionProblem {
    pub file: String,
    pub reason: String,
    pub since: u64,
}

/// Matched problem provenance computed in the same read transaction from `run_sb_path_hash` and
/// `run_sb_by_writer_day`.
#[derive(Clone, Debug, Default)]
pub struct ProblemTrace {
    /// Files among `problem_paths` that are also retained scored/joined run files (not counted as
    /// `unscored.unreadable`).
    pub retained_run_files: HashSet<String>,
    /// Earliest problem `since` epoch ms hitting each affected `BucketKey` (for `traced_run_problem`).
    pub bucket_earliest_since: HashMap<BucketKey, Option<i64>>,
    /// Buckets that hit at least one problem (even when `since` could not parse).
    pub bucket_matched: HashSet<BucketKey>,
    /// Whether any review file (in-view or joined) hit a problem, and its earliest `since`.
    pub review_source_matched: bool,
    pub review_source_earliest: Option<i64>,
}

/// Complete typed store snapshot for composing one `/api/scoreboard` view — read in one MVCC
/// redb transaction without touching `run_payload` or `ResolvedHistory`.
#[derive(Clone, Debug)]
pub struct ScoreboardRead {
    pub generation: u64,
    pub catalog_hash: Option<String>,
    pub write_seq: u64,
    pub effective_runs_total: u64,
    pub identity: Json,
    pub refused: Vec<StoreRefusedFile>,
    pub correction_problems: Vec<CorrectionProblem>,
    pub buckets: Vec<(BucketKey, BucketAgg)>,
    pub problem_trace: ProblemTrace,
    /// True when the result came directly from `run_sb_bucket` (unbounded or covering whole range).
    pub used_precomputed_buckets: bool,
}

/// Read the Scoreboard aggregates + metadata in one redb read transaction.
/// Verifies the trust stamp before serving (rebuilding from `RUN_HEAD` on any mismatch).
/// `extra_problems` are the pass problems (`(file, since)` pairs) so problem tracing completes in
/// the exact same snapshot.
pub fn read_for_scoreboard(
    engine: &Engine,
    filter: &ScoreboardFilter,
    extra_problems: &[(String, Option<i64>)],
    safe_paths: &HashMap<String, String>,
) -> Result<ScoreboardRead> {
    engine.with_db(|db| {
        let mut read = db.begin_read()?;
        if !verify_trust_stamp_rd(&read)? {
            drop(read);
            rebuild_all_aggregates(db)?;
            engine.runs_write_seq.fetch_add(1, std::sync::atomic::Ordering::Release);
            read = db.begin_read()?;
        }
        let write_seq = engine.runs_write_seq();
        let identity = runs::identity_table_in_read(&read)?;
        let cp_table = read.open_table(crate::db::CHECKPOINT)?;
        let generation = crate::db::read_checkpoint(&cp_table, "runs/ledger")?
            .map(|c| c.last_sequence_id)
            .unwrap_or(0);
        let meta = read.open_table(RUN_META)?;
        let catalog_hash = meta
            .get("catalog")?
            .map(|v| String::from_utf8_lossy(v.value()).into_owned());
        let effective_runs_total = meta
            .get(META_EFFECTIVE_COUNT)?
            .and_then(|v| std::str::from_utf8(v.value()).ok()?.parse::<u64>().ok())
            .unwrap_or(0);

        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let fact_table = read.open_table(RUN_SB_FACT)?;
        let path_runs_table = read.open_table(RUN_SB_PATH_RUNS)?;

        // 1. Read refused files and resolve any retained-run path via multi-run RUN_SB_PATH_RUNS.
        let mut refused = Vec::new();
        {
            let files_tbl = read.open_table(RUN_FILE)?;
            for item in files_tbl.iter()? {
                let (k, v) = item?;
                let bytes = v.value();
                if bytes.get(1) != Some(&2) {
                    continue;
                }
                let row = runs::decode_file_row(bytes)?;
                let hash_hex = hex(k.value());
                let mut retained_file = None;
                for r in runs_at_path_hash_rd(&path_runs_table, k.value())? {
                    if let Some(fact) = load_fact_rd(&fact_table, r)?
                        && fact.effective
                    {
                        retained_file = Some(fact.file);
                        break;
                    }
                }
                refused.push(StoreRefusedFile {
                    path_hash: hash_hex,
                    retained_file,
                    reason: dict
                        .text(kind::REASON, row.reason)?
                        .unwrap_or_else(|| "refused".to_string()),
                    at_ms: row.at_ms,
                });
            }
            refused.sort_by(|a, b| a.path_hash.cmp(&b.path_hash));
        }

        // 2. Read persisted correction refusal problems in RUN_BY_TIME (cursor_ms, r) order.
        let mut correction_problems_ordered: Vec<(u64, u32, CorrectionProblem)> = Vec::new();
        {
            let ref_tbl = read.open_table(RUN_SB_CORRECTION_REFUSED)?;
            for item in ref_tbl.iter()? {
                let (k, v) = item?;
                let r = be_u32(k.value(), 0);
                let reason = String::from_utf8_lossy(v.value()).into_owned();
                if let Some(fact) = load_fact_rd(&fact_table, r)? {
                    correction_problems_ordered.push((
                        fact.cursor_ms,
                        fact.r,
                        CorrectionProblem {
                            file: fact.file,
                            reason,
                            since: fact.ingested_ms,
                        },
                    ));
                }
            }
        }
        correction_problems_ordered.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
        let correction_problems: Vec<CorrectionProblem> =
            correction_problems_ordered.into_iter().map(|(_, _, p)| p).collect();

        // 3. Decide fast path (direct `run_sb_bucket`) vs date-range scan (`run_by_time` -> `run_sb_fact`).
        let by_time = read.open_table(RUN_BY_TIME)?;
        let covers_all_dates = if !filter.has_date_bounds() {
            true
        } else {
            let min_ms = by_time.first()?.map(|(k, _)| be_u64(k.value(), 0)).unwrap_or(0);
            let max_ms = by_time.last()?.map(|(k, _)| be_u64(k.value(), 0)).unwrap_or(0);
            filter.from_ms.is_none_or(|f| f <= min_ms) && filter.to_ms.is_none_or(|t| t >= max_ms)
        };

        let buckets: Vec<(BucketKey, BucketAgg)> = if covers_all_dates {
            let bucket_tbl = read.open_table(RUN_SB_BUCKET)?;
            let mut out = Vec::new();
            if let Some(tt) = &filter.task_type {
                let prefix = BucketKey::task_type_prefix(tt);
                let end = prefix_end(&prefix);
                for item in bucket_tbl.range(prefix.as_slice()..end.as_slice())? {
                    let (k, v) = item?;
                    let key = BucketKey::decode(k.value())?;
                    if filter.matches_bucket_key(&key) {
                        out.push((key, BucketAgg::decode(v.value())?));
                    }
                }
            } else {
                for item in bucket_tbl.iter()? {
                    let (k, v) = item?;
                    let key = BucketKey::decode(k.value())?;
                    if filter.matches_bucket_key(&key) {
                        out.push((key, BucketAgg::decode(v.value())?));
                    }
                }
            }
            out
        } else {
            // Bounded date-range scan over `run_by_time` -> `run_sb_fact` + `run_sb_reviews_by_target`.
            let rev_by_target = read.open_table(RUN_SB_REVIEWS_BY_TARGET)?;
            let rev_dict = read.open_table(RUN_DICT_REV)?;
            let lo = Key::new().u64(filter.from_ms.unwrap_or(0)).u32(0);
            let hi = Key::new().u64(filter.to_ms.unwrap_or(u64::MAX - 1)).u32(u32::MAX);
            let mut map: BTreeMap<BucketKey, BucketAgg> = BTreeMap::new();
            if lo.as_slice() <= hi.as_slice() {
                for item in by_time.range(lo.as_slice()..=hi.as_slice())? {
                    let (k, _) = item?;
                    let r = be_u32(k.value(), 8);
                    let Some(fact) = load_fact_rd(&fact_table, r)? else {
                        continue;
                    };
                    if !fact.effective || !filter.matches_fact(&fact) {
                        continue;
                    }
                    let rev_rs = reviews_of_target_rd(&rev_by_target, r)?;
                    let mut joined = Vec::new();
                    for rev_r in rev_rs {
                        if let Some(rev_fact) = load_fact_rd(&fact_table, rev_r)?
                            && rev_fact.effective
                            && let Some(jr) = build_joined_review(&fact, &rev_fact)
                        {
                            joined.push(jr);
                        }
                    }
                    joined.sort_by(|a, b| {
                        a.review_cursor_ms
                            .cmp(&b.review_cursor_ms)
                            .then_with(|| a.review_r.cmp(&b.review_r))
                    });
                    let own_ref = if let Some(rev) = &fact.review {
                        let target = match lookup_id(&rev_dict, kind::RUN_ID, &rev.effective_of)? {
                            Some(tr) => load_fact_rd(&fact_table, tr)?,
                            None => None,
                        };
                        build_own_review_ref(&fact, target.as_ref())
                    } else {
                        None
                    };
                    let key = BucketKey::from_fact(&fact);
                    map.entry(key).or_default().fold_run(&fact, &joined, own_ref);
                }
            }
            map.into_iter().collect()
        };

        // 4. Trace any `writers/...` problems against ALL matching effective runs and joined reviews.
        let mut all_writer_problems: Vec<(String, Option<i64>)> = Vec::new();
        for rf in &refused {
            let file = safe_paths
                .get(&rf.path_hash)
                .cloned()
                .or_else(|| rf.retained_file.clone())
                .unwrap_or_else(|| {
                    format!(
                        "refused/(name withheld) {}",
                        &rf.path_hash[..rf.path_hash.len().min(12)]
                    )
                });
            if file.starts_with("writers/") {
                all_writer_problems.push((file, i64::try_from(rf.at_ms).ok()));
            }
        }
        for cp in &correction_problems {
            if cp.file.starts_with("writers/") {
                all_writer_problems.push((cp.file.clone(), i64::try_from(cp.since).ok()));
            }
        }
        for (file, since) in extra_problems {
            if file.starts_with("writers/") {
                all_writer_problems.push((file.clone(), *since));
            }
        }

        let mut problem_trace = ProblemTrace::default();
        if !all_writer_problems.is_empty() {
            let rev_dict = read.open_table(RUN_DICT_REV)?;
            let wd_table = read.open_table(RUN_SB_BY_WRITER_DAY)?;
            let mut trace_fact = |fact: &StoredRunFact, p_file: &str, since: Option<i64>| -> Result<()> {
                if !fact.effective {
                    return Ok(());
                }
                let in_view = filter.matches_fact(fact);
                let joins_view = !in_view
                    && fact.review.as_ref().is_some_and(|rev| {
                        // A review outside the filter joins iff `from_ms` passes and its target
                        // is in the filter (matches `scoreboard_owned.rs:1994-2021`).
                        filter.from_ms.is_none_or(|f| fact.cursor_ms >= f)
                            && lookup_id(&rev_dict, kind::RUN_ID, &rev.effective_of)
                                .ok()
                                .flatten()
                                .and_then(|tr| load_fact_rd(&fact_table, tr).ok().flatten())
                                .is_some_and(|tf| tf.effective && filter.matches_fact(&tf))
                    });
                if !in_view && !joins_view {
                    return Ok(());
                }
                if fact.file == p_file {
                    problem_trace.retained_run_files.insert(p_file.to_string());
                }
                if in_view {
                    let bkey = BucketKey::from_fact(fact);
                    problem_trace.bucket_matched.insert(bkey.clone());
                    if let Some(s) = since {
                        problem_trace
                            .bucket_earliest_since
                            .entry(bkey)
                            .and_modify(|prev| {
                                if prev.is_none_or(|p| s < p) {
                                    *prev = Some(s);
                                }
                            })
                            .or_insert(Some(s));
                    }
                }
                if fact.review.is_some() {
                    problem_trace.review_source_matched = true;
                    if let Some(s) = since
                        && problem_trace.review_source_earliest.is_none_or(|prev| s < prev)
                    {
                        problem_trace.review_source_earliest = Some(s);
                    }
                }
                Ok(())
            };

            for (p_file, since) in &all_writer_problems {
                if p_file.ends_with(".json") {
                    let hash = path_key(p_file);
                    for r in runs_at_path_hash_rd(&path_runs_table, hash.as_slice())? {
                        if let Some(fact) = load_fact_rd(&fact_table, r)?
                            && fact.file == *p_file
                        {
                            trace_fact(&fact, p_file, *since)?;
                        }
                    }
                } else {
                    // Folder prefix: writers/<writer>[/runs[/YYYY-MM-DD]]
                    let mut parts = p_file.split('/');
                    let _ = parts.next(); // "writers"
                    if let Some(writer) = parts.next()
                        && let Some(wid) = lookup_id(&rev_dict, kind::WRITER, writer)?
                    {
                        let _runs = parts.next();
                        let day_opt = parts.next();
                        let prefix = match day_opt {
                            Some(day) if day.len() == 10 => Key::new().u32(wid).bytes(day.as_bytes()).0,
                            _ => Key::new().u32(wid).0,
                        };
                        let end = prefix_end(&prefix);
                        for item in wd_table.range(prefix.as_slice()..end.as_slice())? {
                            let (k, _) = item?;
                            let kb = k.value();
                            if kb.len() == 18 {
                                let r = be_u32(kb, 14);
                                if let Some(fact) = load_fact_rd(&fact_table, r)?
                                    && (fact.file == *p_file
                                        || fact
                                            .file
                                            .strip_prefix(p_file.as_str())
                                            .is_some_and(|rest| rest.starts_with('/')))
                                {
                                    trace_fact(&fact, p_file, *since)?;
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(ScoreboardRead {
            generation,
            catalog_hash,
            write_seq,
            effective_runs_total,
            identity,
            refused,
            correction_problems,
            buckets,
            problem_trace,
            used_precomputed_buckets: covers_all_dates,
        })
    })
}

/// Read accepted readers-check receipts directly from `run_head` + `run_dict` +
/// `run_sb_correction_refused` without touching `run_payload` or building `ResolvedHistory`.
pub fn accepted_receipts(engine: &Engine) -> Result<(u64, Vec<AcceptedReceipt>)> {
    engine.with_db(|db| {
        let mut read = db.begin_read()?;
        if !verify_trust_stamp_rd(&read)? {
            drop(read);
            rebuild_all_aggregates(db)?;
            engine.runs_write_seq.fetch_add(1, std::sync::atomic::Ordering::Release);
            read = db.begin_read()?;
        }
        let cp_table = read.open_table(crate::db::CHECKPOINT)?;
        let generation = crate::db::read_checkpoint(&cp_table, "runs/ledger")?
            .map(|c| c.last_sequence_id)
            .unwrap_or(0);
        let refused_tbl = read.open_table(RUN_SB_CORRECTION_REFUSED)?;
        let heads = read.open_table(RUN_HEAD)?;
        let dict_table = read.open_table(RUN_DICT)?;
        let mut dict = Dict::new(&dict_table);
        let mut out = Vec::with_capacity(heads.len()? as usize);
        for item in heads.iter()? {
            let (k, v) = item?;
            let r = be_u32(k.value(), 0);
            if refused_tbl.get(k.value())?.is_some() {
                continue;
            }
            let head = Head::decode(v.value())?;
            let id = dict.text(kind::RUN_ID, r)?.unwrap_or_default();
            let file = dict.field(&head, at::PATH)?.unwrap_or_default();
            out.push(AcceptedReceipt {
                file: file.into_boxed_str(),
                id: id.into_boxed_str(),
                seq: head.seq,
                sha256: hex(&head.sha).into_boxed_str(),
                first_seen_at: head.ingested_ms,
            });
        }
        Ok((generation, out))
    })
}
