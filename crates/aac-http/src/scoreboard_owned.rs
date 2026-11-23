//! R4 (DESIGN-M3-COMPLETE 1.4): the owned Scoreboard. The service scans the catalog every
//! 30 s, ingests the share's runs into the hosted store (a pass every two minutes, every
//! second while a backfill has more, a full pass every fifteen minutes), and answers
//! `GET /api/scoreboard` from the store in process: the paged snapshot read with its restart
//! budget, the catalog it holds, the display model resolution, and the existing composer
//! (`read_scoreboard::compose`, by-model and groups in process). The view cache keeps the
//! composed view per (filters, generation) for 60 s, as the TypeScript service does; a hit
//! fills only `cacheAgeSeconds` (a stale serve also marks itself refreshing and ages
//! its sources). The front door forwards only the filters and the clock: the snapshot
//! and its presentation facts stay here, computed once per snapshot. Anything unproven
//! declines and today's handler answers.
//!
//! When the store is off or a store read is declined (the lock held), the owner still answers
//! from a bounded, validated share scan: history filters stay off, and the runs state names
//! that share read.

use crate::feed::Feeds;
use crate::store_host::StoreHost;
use aac_files::registry::{Owners, Role};
use aac_status::model_identity::{AliasTable, Model, normalize_model};
use aac_status::read_context::{Date, Presentation};
use aac_status::share::catalog::{self, CatalogReader};
use aac_status::share::check::{ReadersCheckWriter, RunRow};
use aac_status::share::{NodeFs, Problem, ReadError, ShareFs, is_missing, showable, stat_key};
use aac_status::validate;
use aac_views::decline::Decline;
use aac_views::js_json::Writer;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A read within this long of the last view gets that view (the TS service's cache time).
const CACHE_MS: u64 = 60_000;
/// Filtered views kept at once.
const CACHE_ENTRIES: usize = 4;
/// Bytes one view-cache map keeps at once: the entry cap bounds the filter count, this bounds
/// the memory when the views themselves grow (a bigger run set writes a bigger body). The two
/// maps share each view's `Arc`, so the resident bytes are at most this cap, not twice it.
const CACHE_BYTES: usize = 16 << 20;
/// A pass every two minutes; again within a second while a backfill has more.
pub(crate) const PASS_MS: u64 = 120_000;
pub(crate) const MORE_MS: u64 = 1_000;
/// A full pass (every folder listed, whatever its mtime) at least this often.
const FULL_MS: u64 = 15 * 60_000;
/// Remembered safe paths kept for naming refused files: bounded, oldest out first.
const SAFE_PATHS_CAP: usize = 4_096;
const RUN_CAP: usize = 32 * 1024;
const THREAD_CAP: usize = 16 * 1024;
const CATALOG_CAP: usize = 256 * 1024;
/// Share-only reads use the TypeScript `RunsReader` caps (newest days kept).
const SHARE_WRITER_CAP: usize = 20_000;
const SHARE_RUN_CAP: usize = 50_000;
const SHARE_BYTE_CAP: u64 = 256 * 1024 * 1024;

/// The fixed reason for a file whose name looks like a credential (the store's words too).
const SECRET_NAME_REASON: &str = "refused: its file name looks like a credential";
const NAME_WITHHELD: &str = "(name withheld)";

/// A path's hash, as the store keys files and names refusals (`path_key` in runs.rs).
fn path_hash(path: &str) -> String {
    let digest = Sha256::digest(format!("aac-run-path-v1:{path}").as_bytes());
    digest.iter().take(16).map(|b| format!("{b:02x}")).collect()
}

fn shown(path: &str) -> String {
    if aac_status::secret_policy::find_secret(path).is_none() {
        path.to_string()
    } else {
        NAME_WITHHELD.to_string()
    }
}

/// The catalog's content fingerprint without its scan stamp (a pass changes both
/// together; the stamp alone proves nothing about content).
fn catalog_fingerprint(catalog: &Json) -> String {
    let mut catalog = catalog.clone();
    if let Some(object) = catalog.as_object_mut() {
        object.remove("scannedAt");
    }
    sha256_text(&catalog.to_string())
}

/// The kept facts text plus the one date fact a new catalog scan stamp needs (its ISO
/// text and zone words), inserted into the `dates` object the completed text opens
/// with. `None` when the zone cannot write that instant (the caller recomputes).
fn patch_scanned_at_fact(facts_text: &str, scanned_ms: u64, feed: &Feed) -> Option<String> {
    let offsets = crate::zone_facts::server_zone_facts(&feed.time_zone, feed.now)?;
    let ms = i64::try_from(scanned_ms).ok()?;
    let (clock, day) = offsets.zone.words(ms)?;
    let iso = aac_num::ecma::to_iso_string(ms)?;
    let entry = format!(
        "{}:{{\"iso\":{},\"clock\":{},\"day\":{}}}",
        Json::String(scanned_ms.to_string()),
        Json::String(iso),
        Json::String(clock),
        Json::String(day)
    );
    let marker = "\"dates\":{";
    let at = facts_text.find(marker)? + marker.len();
    let mut patched = String::with_capacity(facts_text.len() + entry.len() + 1);
    patched.push_str(&facts_text[..at]);
    patched.push_str(&entry);
    if !facts_text[at..].starts_with('}') {
        patched.push(',');
    }
    patched.push_str(&facts_text[at..]);
    Some(patched)
}

fn sha256_text(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// One ingest pass's state (`PassState`).
#[derive(Clone, Default)]
struct PassState {
    state: String,
    more: bool,
    generation: u64,
    catalog_hash: String,
    problems: Vec<Json>,
}

/// Where one entry's projected doc lives: its serialized text (share rows), or the
/// store's resident resolved history shared by `Arc` (store rows — no second copy).
enum DocSource {
    Text(Arc<str>),
    Store {
        history: Arc<aac_store::runs::ResolvedHistory>,
        index: usize,
    },
}

/// One run held compactly: its doc source (parsed back transiently where the composer
/// needs JSON) plus the typed fields every reader picks by. One entry is the only
/// per-run copy this owner keeps — the share scan cache and the held snapshot share
/// the same `Arc`s, and store rows reference the store's own resident history —
/// instead of several parsed `serde_json::Value` copies of the same doc.
struct RunEntry {
    /// The row's cursor time (`endedMs` for share rows, the query cursor for store rows).
    at: u64,
    file: String,
    first_seen_at: u64,
    sha256: String,
    doc_source: DocSource,
    id: String,
    writer: String,
    source: Option<String>,
    task_type: Option<String>,
    has_review: bool,
    seq: u64,
    written_at: String,
    corrects: Option<String>,
    has_void: bool,
}

impl std::fmt::Debug for RunEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunEntry")
            .field("id", &self.id)
            .field("file", &self.file)
            .field("at", &self.at)
            .finish_non_exhaustive()
    }
}

impl RunEntry {
    /// The entry for one projected doc and its row facts.
    fn from_doc(at: u64, file: String, first_seen_at: u64, sha256: String, doc: &Json) -> Self {
        Self::from_parts(
            at,
            file,
            first_seen_at,
            sha256,
            doc,
            DocSource::Text(Arc::from(doc.to_string())),
        )
    }

    /// The entry for one row of the store's resident resolved history: typed fields copied
    /// directly from CompactRun — the doc stays compact UTF-8 JSON bytes, parsed on demand.
    fn from_store_row(history: &Arc<aac_store::runs::ResolvedHistory>, index: usize) -> Self {
        let row = &history.compact_rows()[index];
        Self {
            at: row.cursor.0,
            file: row.file.to_string(),
            first_seen_at: row.first_seen_at,
            sha256: row.sha256.to_string(),
            doc_source: DocSource::Store {
                history: Arc::clone(history),
                index,
            },
            id: row.id.to_string(),
            writer: row.writer.to_string(),
            source: row.source.as_deref().map(str::to_string),
            task_type: row.task_type.as_deref().map(str::to_string),
            has_review: row.has_review,
            seq: row.seq,
            written_at: row.written_at.to_string(),
            corrects: row.corrects.as_deref().map(str::to_string),
            has_void: row.has_void,
        }
    }

    fn from_parts(
        at: u64,
        file: String,
        first_seen_at: u64,
        sha256: String,
        doc: &Json,
        doc_source: DocSource,
    ) -> Self {
        Self {
            at,
            file,
            first_seen_at,
            sha256,
            doc_source,
            id: doc["id"].as_str().unwrap_or_default().to_string(),
            writer: doc["writer"].as_str().unwrap_or_default().to_string(),
            source: doc["source"].as_str().map(str::to_string),
            task_type: doc["task"]["type"].as_str().map(str::to_string),
            has_review: doc["review"].is_object(),
            seq: doc["seq"].as_u64().unwrap_or(0),
            written_at: doc["writtenAt"].as_str().unwrap_or_default().to_string(),
            corrects: doc["corrects"].as_str().map(str::to_string),
            has_void: doc["void"].is_object(),
        }
    }

    /// The entry for one runs-state row (`file`, `doc`, `firstSeenAt`, `sha256`).
    fn from_row(at: u64, row: &Json) -> Self {
        Self::from_doc(
            at,
            row["file"].as_str().unwrap_or_default().to_string(),
            row["firstSeenAt"].as_u64().unwrap_or(0),
            row["sha256"].as_str().unwrap_or_default().to_string(),
            &row["doc"],
        )
    }

    /// The projected doc, parsed (or cloned from the store snapshot) for a compose or
    /// a facts pass.
    fn doc(&self) -> Json {
        match &self.doc_source {
            DocSource::Text(text) => serde_json::from_str(text).unwrap_or(Json::Null),
            DocSource::Store { history, index } => history.compact_rows()[*index].doc(),
        }
    }

    /// This entry as the runs state carries it (`file`, `doc`, `firstSeenAt`, `sha256`).
    fn state_row(&self) -> Json {
        json!({
            "file": self.file,
            "doc": self.doc(),
            "firstSeenAt": self.first_seen_at,
            "sha256": self.sha256,
        })
    }
}

/// Per held snapshot and catalog: what the composer derives from every row before it
/// composes, made once instead of per request — compactly (a model patch and typed
/// facts per row, never a second parsed copy of every doc).
struct PreparedDocs {
    /// Each row's display model resolution (`normalize_model`): applying it to the raw
    /// doc gives the composer's doc. `None` keeps the doc as written (no model id).
    patches: Vec<Option<aac_status::model_identity::Resolved>>,
    /// Whether the row or its normalized doc holds an exact prototype name
    /// (`status_fed::proto_exact`): a filter that picks one declines.
    proto: Vec<bool>,
    /// What compose reads of each row (`read_scoreboard::row_facts`).
    facts: Vec<aac_status::read_scoreboard::RowFacts>,
    /// Each doc's position in the newest order, per facts generation id (None: not provable).
    order: Mutex<Option<PositionsByFacts>>,
}

type PositionsByFacts = (String, Option<Arc<Vec<usize>>>);

/// One row's composer doc: its raw doc with the model patch applied (the envelope
/// path's `effective` step; the doc's review already travels with the raw doc).
fn normalized_doc(raw: Json, patch: &Option<aac_status::model_identity::Resolved>) -> Json {
    let mut doc = raw;
    if let Some(patch) = patch {
        doc["model"]["id"] = json!(patch.id);
        doc["model"]["effort"] = json!(patch.effort);
        doc["model"]["provider"] = Json::Null;
    }
    doc
}

impl PreparedDocs {
    /// The newest positions under this facts generation, made once per id (the docs are
    /// parsed transiently for the sort and dropped with the positions kept).
    fn positions(
        &self,
        rows: &[Arc<RunEntry>],
        facts_id: &str,
        facts: &std::sync::Arc<Presentation>,
    ) -> Option<Arc<Vec<usize>>> {
        let mut held = self.order.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((id, positions)) = held.as_ref()
            && id == facts_id
        {
            return positions.clone();
        }
        let docs_owned: Vec<Json> = rows
            .iter()
            .enumerate()
            .map(|(index, entry)| normalized_doc(entry.doc(), &self.patches[index]))
            .collect();
        let docs: Vec<&Json> = docs_owned.iter().collect();
        let positions =
            aac_status::read_scoreboard::newest_positions(&docs, facts.clone()).map(Arc::new);
        *held = Some((facts_id.to_string(), positions.clone()));
        positions
    }
}

/// The store's own facts the runs state and the view carry, which change only when the runs
/// tables are written (`runsRecord`).
struct StoreFacts {
    refused: Vec<Json>,
    identity: Json,
    status_runs: Json,
}

/// What a share-only read reports instead of store history (`RunsReader.snapshot`).
struct ShareRead {
    state: String,
    scanned_at: Option<u64>,
    skipped: Json,
}

struct HeldRuns {
    key: (u64, String),
    /// Each row compactly (see [`RunEntry`]), shared with the share scan cache.
    rows: Vec<Arc<RunEntry>>,
    problems: Vec<Json>,
    accepted: Option<Vec<Json>>,
    /// Each row's composer preparation over the catalog with this hash (made on first use).
    docs: Mutex<Option<(String, Arc<PreparedDocs>)>>,
    /// The readers-check rows of the snapshot's receipts (made on first use).
    accepted_rows: std::sync::OnceLock<Vec<RunRow>>,
    /// Present when this snapshot came from the share, not the store.
    share: Option<ShareRead>,
    /// The store snapshot these rows reference, when they came from it: held so its
    /// accepted receipts stay readable without a copy.
    store_history: Option<Arc<aac_store::runs::ResolvedHistory>>,
}

impl HeldRuns {
    #[cfg(test)]
    fn new(
        key: (u64, String),
        rows: Vec<(u64, Json)>,
        problems: Vec<Json>,
        accepted: Option<Vec<Json>>,
    ) -> Self {
        let rows = rows
            .into_iter()
            .map(|(at, row)| Arc::new(RunEntry::from_row(at, &row)))
            .collect();
        Self::from_entries(key, rows, problems, accepted)
    }

    fn from_entries(
        key: (u64, String),
        rows: Vec<Arc<RunEntry>>,
        problems: Vec<Json>,
        accepted: Option<Vec<Json>>,
    ) -> Self {
        Self {
            key,
            rows,
            problems,
            accepted,
            docs: Mutex::new(None),
            accepted_rows: std::sync::OnceLock::new(),
            share: None,
            store_history: None,
        }
    }

    fn with_store_history(mut self, history: Arc<aac_store::runs::ResolvedHistory>) -> Self {
        self.store_history = Some(history);
        self
    }

    fn has_accepted_receipts(&self) -> bool {
        self.store_history.is_some() || self.accepted.is_some()
    }

    fn build_accepted_run_rows(&self) -> Vec<RunRow> {
        if let Some(history) = &self.store_history {
            history
                .accepted_receipts()
                .iter()
                .map(|r| RunRow {
                    file: r.file.to_string(),
                    seq: r.seq,
                    sha256: r.sha256.to_string(),
                    first_seen_at: r.first_seen_at,
                })
                .collect()
        } else {
            check_rows(self.accepted.as_deref().unwrap_or(&[]))
        }
    }

    /// Accepted receipts as JSON (only materialized if legacy `state_rows(..., receipts=true)` asks).
    fn accepted_json(&self) -> Option<Vec<Json>> {
        if let Some(history) = &self.store_history {
            Some(
                history
                    .accepted_receipts()
                    .iter()
                    .map(|r| {
                        json!({
                            "file": &*r.file,
                            "sha256": &*r.sha256,
                            "firstSeenAt": r.first_seen_at,
                            "doc": { "id": &*r.id, "seq": r.seq },
                        })
                    })
                    .collect(),
            )
        } else {
            self.accepted.clone()
        }
    }

    fn with_share(mut self, share: ShareRead) -> Self {
        self.share = Some(share);
        self
    }

    /// The rows `runsQuery` answers this query with, in its order (indices into `rows`).
    fn pick(&self, query: &Json) -> Vec<usize> {
        let from = query["fromMs"].as_u64();
        let to = query["toMs"].as_u64();
        let source = query["source"].as_str();
        let task_type = query["taskType"].as_str();
        let reviews_only = query["reviewsOnly"].as_bool() == Some(true);
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, entry)| {
                from.is_none_or(|from| entry.at >= from)
                    && to.is_none_or(|to| entry.at <= to)
                    && source.is_none_or(|want| entry.source.as_deref() == Some(want))
                    && task_type.is_none_or(|want| entry.task_type.as_deref() == Some(want))
                    && (!reviews_only || entry.has_review)
            })
            .map(|(index, _)| index)
            .collect()
    }
}

/// The request's filters and clock, as the front door parsed and read them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Feed {
    now: i64,
    /// The request's own clock words (its `now` date fact; the facts generation is shared).
    clock: String,
    day: String,
    time_zone: String,
    from: Option<String>,
    to: Option<String>,
    source: String,
    task_type: Option<String>,
    from_ms: Option<i64>,
    to_ms: Option<i64>,
    /// The snapshot the caller read, when it names one: a moved generation is `stale`.
    /// Unnamed (the front door today), the answer is for the snapshot held here, so the
    /// inputs body never has to cross to the caller just to name it.
    #[serde(default)]
    generation: Option<u64>,
    #[serde(default)]
    catalog_hash: Option<String>,
    /// The facts generation the caller pushed, when it pushed one.
    #[serde(default)]
    facts: Option<String>,
}

/// Removes its key from the composing set however the compose ends.
struct Composing<'a> {
    set: &'a Mutex<std::collections::HashSet<String>>,
    key: String,
}

impl Drop for Composing<'_> {
    fn drop(&mut self) {
        self.set
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.key);
    }
}

/// How long a cold share caller waits for the detached scan before declining honestly.
const SHARE_COLD_WAIT_MS: u64 = 120_000;

/// Ends one detached share scan however it exits — success, failure, or unwind: an
/// uncompleted scan counts failed, the running flag clears first, and waiters are
/// notified last, so no cold caller can park on a scan that already ended.
struct ScanRunning<'a> {
    owner: &'a ScoreboardOwned,
    completed: bool,
}

impl Drop for ScanRunning<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.owner.share_scan_failed.store(true, Ordering::SeqCst);
        }
        self.owner.share_scan_running.store(false, Ordering::SeqCst);
        self.owner.share_scan_done.notify_waiters();
    }
}

struct CachedView {
    at: u64,
    /// The held-snapshot sequence this view was composed from: a newer snapshot makes
    /// the view stale however young it is.
    seq: u64,
    /// Pre-serialized view bytes (with `"cacheAgeSeconds":0`), shared by `Arc` between
    /// `views` and `last_good` — ~3 MiB bytes instead of 2x ~25 MiB `serde_json::Value` trees.
    bytes: Vec<u8>,
    /// Start byte offset of the `0` value in `"cacheAgeSeconds":0` (fast splice on fresh hits).
    age_value_offset: Option<usize>,
    etag: String,
}

fn find_age_zero_offset(bytes: &[u8]) -> Option<usize> {
    const NEEDLE: &[u8] = b"\"cacheAgeSeconds\":0";
    if bytes.len() < NEEDLE.len() {
        return None;
    }
    // `cacheAgeSeconds` is written near the tail of the view; search from the end.
    for i in (0..=bytes.len() - NEEDLE.len()).rev() {
        if &bytes[i..i + NEEDLE.len()] == NEEDLE {
            let after = bytes.get(i + NEEDLE.len()).copied();
            if matches!(after, Some(b'}' | b',')) {
                return Some(i + NEEDLE.len() - 1);
            }
        }
    }
    None
}

/// A served view's bytes at `now`: the view itself with only `cacheAgeSeconds` filled.
/// Fresh hits splice the formatted number directly into the cached byte buffer (zero JSON clone,
/// zero re-serialization). Rare stale serves parse the cached bytes and apply the stale markers.
fn cached_bytes(cached: &CachedView, now: u64, stale: bool) -> (Vec<u8>, String) {
    let age_seconds = now.saturating_sub(cached.at) as f64 / 1000.0;
    if !stale && let Some(pos) = cached.age_value_offset {
        let age_bytes = aac_views::js_json::write(&json!(age_seconds));
        let mut out = Vec::with_capacity(cached.bytes.len() + age_bytes.len());
        out.extend_from_slice(&cached.bytes[..pos]);
        out.extend_from_slice(&age_bytes);
        out.extend_from_slice(&cached.bytes[pos + 1..]);
        return (out, cached.etag.clone());
    }
    let mut view: Json = serde_json::from_slice(&cached.bytes).unwrap_or(Json::Null);
    view["cacheAgeSeconds"] = json!(age_seconds);
    if stale {
        view["refreshing"] = json!(true);
        if let Some(sources) = view["sources"].as_array_mut() {
            for source in sources {
                // A null age (no scan yet) stays null: no fabricated age.
                if let Some(age) = source["ageSeconds"].as_f64() {
                    let aged = age + age_seconds;
                    source["ageSeconds"] = json!(aged);
                    let stale_after = source["staleAfterSeconds"].as_f64().unwrap_or(180.0);
                    if aged > stale_after && source["state"].as_str() == Some("ok") {
                        source["state"] = json!("stale");
                    }
                }
            }
        }
    }
    let mut writer = Writer::with_capacity(cached.bytes.len() + 256);
    writer.value(&view);
    let (bytes, identity_bytes) = writer.finish();
    if stale {
        let digest = Sha256::digest(&identity_bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        (bytes, format!("W/\"{hex}\""))
    } else {
        (bytes, cached.etag.clone())
    }
}

/// Trim one view-cache map to its bounds: at most [`CACHE_ENTRIES`] views, and at most
/// [`CACHE_BYTES`] of their bytes. The oldest go first; the newest view always survives, even
/// when it alone is over the byte cap (an empty cache would only recompose it at once).
fn trim_view_cache(map: &mut IndexMap<String, Arc<CachedView>>) {
    while map.len() > CACHE_ENTRIES {
        map.shift_remove_index(0);
    }
    let mut bytes: usize = map.values().map(|view| view.bytes.len()).sum();
    while bytes > CACHE_BYTES && map.len() > 1 {
        if let Some((_, evicted)) = map.shift_remove_index(0) {
            bytes -= evicted.bytes.len();
        }
    }
}

impl ScoreboardOwned {
    /// Keep one composed view under its cache key and its filter's last-good key, trimmed to
    /// the entry and byte bounds.
    fn keep_view(&self, cache_key: String, filter_key: String, cached: Arc<CachedView>) {
        {
            let mut views = self.views.lock().unwrap_or_else(|p| p.into_inner());
            views.shift_remove(&cache_key);
            views.insert(cache_key, Arc::clone(&cached));
            trim_view_cache(&mut views);
        }
        let mut good = self.last_good.lock().unwrap_or_else(|p| p.into_inner());
        good.shift_remove(&filter_key);
        good.insert(filter_key, cached);
        trim_view_cache(&mut good);
    }
}

/// What one computed presentation was made over: the held snapshot (by sequence, rows and
/// its generation key), the catalog snapshot it saw (by content fingerprint and scan
/// stamp), the runs problems, the seed failures and one display zone. Any move recomputes —
/// except a catalog scan-stamp-only move, whose one new date fact is patched into the kept
/// facts text.
#[derive(Clone, PartialEq, Eq)]
struct FactsKey {
    held_seq: u64,
    rows: usize,
    generation: u64,
    catalog_hash: String,
    catalog_scanned_at: Option<u64>,
    catalog_fingerprint: String,
    runs_problems_hash: String,
    seed_hash: String,
    time_zone: String,
}

impl FactsKey {
    /// The facts generation id these facts are keyed by (feed word: short, no zone name,
    /// which carries a `/`; the zone enters as its hash's first digits).
    fn id(&self) -> String {
        let short: String = self.catalog_hash.chars().take(12).collect();
        let probs: String = self.runs_problems_hash.chars().take(8).collect();
        let zone: String = sha256_text(&self.time_zone).chars().take(8).collect();
        format!(
            "{}:{}:{}:{}:{}:{}",
            self.generation,
            self.held_seq,
            short,
            self.catalog_scanned_at.unwrap_or(0),
            probs,
            zone
        )
    }

    /// Only the catalog scan stamp moved (the 30 s tick): the kept facts patch forward.
    fn same_but_scanned(&self, other: &FactsKey) -> bool {
        self.held_seq == other.held_seq
            && self.rows == other.rows
            && self.generation == other.generation
            && self.catalog_hash == other.catalog_hash
            && self.catalog_fingerprint == other.catalog_fingerprint
            && self.runs_problems_hash == other.runs_problems_hash
            && self.seed_hash == other.seed_hash
            && self.time_zone == other.time_zone
    }
}

/// The facts this owner computed itself, their completed text (kept for the scan-stamp
/// patch) and their parsed presentation, once per key.
struct ComputedFacts {
    key: FactsKey,
    facts_text: String,
    presentation: Arc<Presentation>,
}

/// One share run file's scan outcome, reused while its stat print (size, mtime, inode,
/// ctime) holds: the parsed row (with its first-seen stamp) or the refusal reason.
#[derive(Clone)]
enum CachedOutcome {
    Row { row: Arc<RunEntry> },
    Problem { reason: String },
}

#[derive(Clone)]
struct CachedRun {
    print: String,
    outcome: CachedOutcome,
}

pub(crate) struct ScoreboardOwned {
    root: PathBuf,
    store: Option<Arc<StoreHost>>,
    feeds: Feeds,
    catalog: Mutex<CatalogReader>,
    catalog_state: Mutex<Json>,
    pass: Mutex<PassState>,
    /// Paths this server has seen, by their hash, for naming a refused file (memory only,
    /// and only paths the secret filter passes; the store keeps none).
    safe_paths: Mutex<SafePaths>,
    seed_failures: Mutex<Vec<Json>>,
    /// The current snapshot's unfiltered runs (see [`HeldRuns`]).
    held_runs: Mutex<Option<Arc<HeldRuns>>>,
    /// Bumped before and after every `runsRecord` (this owner is the runs tables' only
    /// writer): the store facts below are held while it has not moved.
    store_epoch: AtomicU64,
    /// `runsRefused`, `runsIdentity` and `runsStatus.runs` as of a store epoch.
    store_facts: Mutex<Option<(u64, Arc<StoreFacts>)>>,
    /// The newest snapshot generation whose readers-check rows were recorded.
    recorded: Arc<Mutex<u64>>,
    views: Mutex<IndexMap<String, Arc<CachedView>>>,
    /// The last good view per filter, across generations: served while its refresh runs.
    last_good: Mutex<IndexMap<String, Arc<CachedView>>>,
    /// The last pushed facts generation, parsed once: (its id, its presentation). Only
    /// generations pushed into the feed land here; computed facts never do (they would
    /// shadow a newer snapshot under an old snapshot-blind id).
    parsed_facts: Mutex<Option<(String, std::sync::Arc<Presentation>)>>,
    /// The facts computed here when the front door pushed none, once per held snapshot.
    computed_facts: Mutex<Option<ComputedFacts>>,
    /// One facts computation at a time (the computation itself runs on the blocking pool).
    facts_compute: tokio::sync::Mutex<()>,
    /// This owner itself, for the background view refresh (set once by `new`).
    me: std::sync::OnceLock<std::sync::Weak<Self>>,
    /// Bumped every time a new runs snapshot is held (the caches above key on it).
    held_seq: AtomicU64,
    /// Per-file share scan outcomes by relative path (see [`CachedRun`]). Shared with the
    /// detached scan task, which mutates it in place: a dropped caller can never wipe it.
    share_cache: Arc<Mutex<HashMap<String, CachedRun>>>,
    /// A share scan is running in its detached task: warm callers take the held snapshot.
    share_scan_running: AtomicBool,
    /// The last detached share scan failed (its blocking task died): cold callers decline.
    share_scan_failed: AtomicBool,
    /// Cold callers wait here for the detached scan to land its snapshot.
    share_scan_done: tokio::sync::Notify,
    /// The view-cache keys being composed now: one compose per key at a time.
    composing: Mutex<std::collections::HashSet<String>>,
    last_full_at: AtomicU64,
    /// Set when the last completed snapshot was the share fallback, not a store page.
    share_fallback: AtomicBool,
    /// When the share snapshot in `held_runs` was taken (the 60 s reuse window).
    share_held_at: AtomicU64,
    /// The readers-check rows this owner shares with the Status owner (one file per writer).
    check: Arc<Mutex<ReadersCheckWriter>>,
    owners: Owners,
    flushing: Arc<AtomicBool>,
}

impl ScoreboardOwned {
    pub(crate) fn new(
        root: PathBuf,
        store: Option<Arc<StoreHost>>,
        feeds: Feeds,
        check: Arc<Mutex<ReadersCheckWriter>>,
        owners: Owners,
        flushing: Arc<AtomicBool>,
    ) -> Arc<Self> {
        let owner = Arc::new(Self {
            catalog: Mutex::new(CatalogReader::new(root.clone())),
            catalog_state: Mutex::new(Json::Null),
            root,
            store,
            feeds,
            pass: Mutex::new(PassState::default()),
            safe_paths: Mutex::new(SafePaths::new()),
            seed_failures: Mutex::new(Vec::new()),
            held_runs: Mutex::new(None),
            store_epoch: AtomicU64::new(0),
            store_facts: Mutex::new(None),
            recorded: Arc::new(Mutex::new(0)),
            views: Mutex::new(IndexMap::new()),
            last_good: Mutex::new(IndexMap::new()),
            parsed_facts: Mutex::new(None),
            computed_facts: Mutex::new(None),
            facts_compute: tokio::sync::Mutex::new(()),
            me: std::sync::OnceLock::new(),
            held_seq: AtomicU64::new(0),
            share_cache: Arc::new(Mutex::new(HashMap::new())),
            share_scan_running: AtomicBool::new(false),
            share_scan_failed: AtomicBool::new(false),
            share_scan_done: tokio::sync::Notify::new(),
            composing: Mutex::new(std::collections::HashSet::new()),
            last_full_at: AtomicU64::new(0),
            share_fallback: AtomicBool::new(false),
            share_held_at: AtomicU64::new(0),
            check,
            owners,
            flushing,
        });
        let _ = owner.me.set(Arc::downgrade(&owner));
        owner
    }

    pub(crate) async fn seed(self: &Arc<Self>) {
        let owner = self.clone();
        let _ = tokio::task::spawn_blocking(move || {
            use aac_status::share::seed::{embedded_seed, seed_catalog};
            let mut failures = Vec::new();
            seed_catalog(
                &owner.root,
                &|name| embedded_seed(name).map(<[u8]>::to_vec),
                &mut failures,
            );
            *owner
                .seed_failures
                .lock()
                .unwrap_or_else(|p| p.into_inner()) = failures
                .into_iter()
                .map(|failure| json!({"file": failure.file, "code": failure.code}))
                .collect();
        })
        .await;
    }

    /// The check file's runs rows (`ScoreboardService.recordCheck`): the accepted revisions
    /// this view read, and the named refusals. Fire and forget behind one in-flight guard,
    /// as the TS writer does; a hung share never stalls the answer.
    /// The answer's readers-check record, off the request path: the held receipts' rows and
    /// the runs state's problems, recorded on the blocking pool; a snapshot older than one
    /// already recorded is dropped, so the newest generation always wins.
    fn record_check_held(&self, held: &Arc<HeldRuns>, picked: &[usize], runs: &Json) {
        if self.owners.owner("readers-check") != Some(Role::RustService) {
            return;
        }
        let problems = check_problems(runs);
        // Without receipts the rows are the picked runs' (as `record_check` reads `runs`).
        let picked: Option<Vec<usize>> = (!held.has_accepted_receipts()).then(|| picked.to_vec());
        let held = Arc::clone(held);
        let check = Arc::clone(&self.check);
        let recorded = Arc::clone(&self.recorded);
        let owners = self.owners.clone();
        let flushing = Arc::clone(&self.flushing);
        tokio::task::spawn_blocking(move || {
            {
                let mut newest = recorded.lock().unwrap_or_else(|p| p.into_inner());
                if held.key.0 < *newest {
                    return;
                }
                *newest = held.key.0;
                let made: Vec<RunRow>;
                let rows: &[RunRow] = match &picked {
                    None => held
                        .accepted_rows
                        .get_or_init(|| held.build_accepted_run_rows()),
                    Some(picked) => {
                        made = picked
                            .iter()
                            .map(|&index| {
                                let entry = &held.rows[index];
                                RunRow {
                                    file: entry.file.clone(),
                                    seq: entry.seq,
                                    sha256: entry.sha256.clone(),
                                    first_seen_at: entry.first_seen_at,
                                }
                            })
                            .collect();
                        &made
                    }
                };
                let mut check = check.lock().unwrap_or_else(|p| p.into_inner());
                check.record_runs(rows, &problems);
            }
            if flushing.swap(true, Ordering::Relaxed) {
                return;
            }
            let check = check.lock().unwrap_or_else(|p| p.into_inner());
            check.flush(&owners, now_ms());
            flushing.store(false, Ordering::Relaxed);
        });
    }

    /// The catalog scan (30 s): the reader holds the last good copies; the snapshot is what
    /// the view composes over.
    pub(crate) fn catalog_pass(&self, now: u64) {
        let fs = NodeFs;
        let mut catalog = self.catalog.lock().unwrap_or_else(|p| p.into_inner());
        catalog.scan(&fs, now);
        *self.catalog_state.lock().unwrap_or_else(|p| p.into_inner()) =
            catalog::state_json(&catalog.snapshot());
    }

    #[cfg(not(unix))]
    pub(crate) async fn prepare_local(self: &Arc<Self>) {
        let catalog = self.clone();
        let _ = tokio::task::spawn_blocking(move || catalog.catalog_pass(now_ms())).await;
        self.ingest_pass(false).await;
    }

    pub(crate) fn generation(&self) -> (u64, String) {
        let pass = self.pass.lock().unwrap_or_else(|p| p.into_inner());
        (pass.generation, pass.catalog_hash.clone())
    }

    /// One ingest pass (`RunsStore.pass`): the store scans the share, this side applies the
    /// one secret policy between scan and record exactly as the TypeScript shuttle does,
    /// and the store records what passes.
    pub(crate) async fn ingest_pass(&self, force_full: bool) {
        self.ingest_pass_at(force_full, now_ms()).await;
    }

    /// [`Self::ingest_pass`] at a stated clock (tests drive the age of a continuing problem).
    pub(crate) async fn ingest_pass_at(&self, force_full: bool, now: u64) {
        let Some(store) = &self.store else {
            return;
        };
        let full =
            force_full || now.saturating_sub(self.last_full_at.load(Ordering::Relaxed)) >= FULL_MS;
        let scan = match store
            .call(
                "runsScan".to_string(),
                json!({"root": self.root.to_string_lossy(), "full": full}).to_string(),
            )
            .await
        {
            Ok(text) => match serde_json::from_str::<Json>(&text) {
                Ok(scan) => scan,
                Err(_) => return,
            },
            Err(_) => return,
        };
        if full {
            self.last_full_at.store(now, Ordering::Relaxed);
        }
        let request = {
            let mut safe_paths = self.safe_paths.lock().unwrap_or_else(|p| p.into_inner());
            ingest_request(&scan, now, &mut safe_paths)
        };
        let has_writes = !request.record["runs"].as_array().is_none_or(Vec::is_empty)
            || !request.record["refused"]
                .as_array()
                .is_none_or(Vec::is_empty);
        if has_writes {
            self.store_epoch.fetch_add(1, Ordering::SeqCst);
        }
        let record = store
            .call("runsRecord".to_string(), request.record.to_string())
            .await;
        if has_writes {
            self.store_epoch.fetch_add(1, Ordering::SeqCst);
        }
        let record: Json = match record {
            Ok(text) => serde_json::from_str(&text).unwrap_or(Json::Null),
            Err(_) => return,
        };
        let (problems, more) = (request.problems, request.more);
        let generation = record["generation"].as_u64().unwrap_or(0);
        let catalog_hash = record["catalogHash"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let changed = {
            let mut pass = self.pass.lock().unwrap_or_else(|p| p.into_inner());
            let changed = pass.generation != generation || pass.catalog_hash != catalog_hash;
            // A folder problem that continues keeps its first observed time; a successful
            // listing drops it, and a new problem starts at this pass (M3).
            let problems = merge_pass_problems(&pass.problems, problems);
            *pass = PassState {
                state: scan["state"].as_str().unwrap_or("error").to_string(),
                more,
                generation,
                catalog_hash: catalog_hash.clone(),
                problems,
            };
            changed
        };
        if changed {
            self.feeds.publish_event(json!({
                "event": "scoreboard-changed",
                "generation": generation,
                "catalogHash": catalog_hash,
            }));
        }
    }

    /// One snapshot per read (`RunsStore.read`): every page names its generation and catalog
    /// hash; a commit between pages restarts the read, bounded; past the budget, the last
    /// complete read for this filter with a disclosed moving store, or an explicit failure.
    ///
    /// `clock` stamps the read (`at`, the runs state's `scannedAt`). On the answer path it is
    /// the request's clock: TS reads the store inside the request, at the instant its view
    /// builder formats, and the forwarded presentation facts hold exactly that instant; a
    /// service-clock stamp a few ms later has no date fact and the compose declines.
    /// Scan the catalog when this owner has none yet, so a store-off answer is not `stale`.
    fn ensure_catalog(&self, now: u64) {
        if self
            .catalog_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .is_null()
        {
            self.catalog_pass(now);
        }
    }

    /// The held unfiltered snapshot for the owner's current (generation, catalog hash), read
    /// once from the store when the generation moved (see [`HeldRuns`]). A disabled or declined
    /// store falls back to a bounded share scan instead of empty runs or an Internal decline.
    async fn held(&self, clock: Option<u64>) -> Result<Arc<HeldRuns>, Decline> {
        let now = clock.unwrap_or_else(now_ms);
        self.ensure_catalog(now);
        let key = self.generation();
        let held = self
            .held_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .filter(|held| held.key == key);
        if let Some(held) = held {
            let share_fresh = held.share.is_none()
                || now.saturating_sub(self.share_held_at.load(Ordering::SeqCst)) < CACHE_MS;
            if share_fresh {
                return Ok(held);
            }
        }
        if self.store.is_none() {
            return self.share_held(clock).await;
        }
        // The store's resident resolved snapshot, shared by Arc: its rows are referenced
        // (typed fields extracted once), never serialized through `runsQuery` text into a
        // second parsed copy here.
        let Some(store) = &self.store else {
            return self.share_held(clock).await;
        };
        match store.runs_history().await {
            Ok(snapshot) => {
                self.share_fallback.store(false, Ordering::SeqCst);
                let rows: Vec<Arc<RunEntry>> = (0..snapshot.compact_rows().len())
                    .map(|index| Arc::new(RunEntry::from_store_row(&snapshot, index)))
                    .collect();
                let held = Arc::new(
                    HeldRuns::from_entries(
                        (snapshot.generation(), key.1.clone()),
                        rows,
                        snapshot.problems().to_vec(),
                        None,
                    )
                    .with_store_history(snapshot),
                );
                // Held only when the snapshot is the one the owner names (an ingest may have landed).
                if held.key == key {
                    *self.held_runs.lock().unwrap_or_else(|p| p.into_inner()) = Some(held.clone());
                    self.held_seq.fetch_add(1, Ordering::SeqCst);
                }
                Ok(held)
            }
            Err(_) => self.share_held(clock).await,
        }
    }

    /// Bounded, validated share runs when history is off or the store will not answer.
    /// The scan runs in one detached task (singleflight, cancellation-safe: dropping this
    /// caller — a route budget timeout — neither stops it nor loses its cache), and it
    /// rereads only the files whose stat print changed (see [`CachedRun`]). A warm caller
    /// takes the held snapshot while the scan refreshes it; a cold caller waits for the
    /// one scan to land, bounded: a failed scan declines at once, a scan that never lands
    /// declines `deadline` at the bound instead of wedging the caller forever.
    async fn share_held(&self, clock: Option<u64>) -> Result<Arc<HeldRuns>, Decline> {
        let now = clock.unwrap_or_else(now_ms);
        self.ensure_catalog(now);
        self.kick_share_scan(now);
        let held = self
            .held_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if let Some(held) = held {
            return Ok(held);
        }
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_millis(SHARE_COLD_WAIT_MS);
        loop {
            let notified = self.share_scan_done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let held = self
                .held_runs
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .clone();
            if let Some(held) = held {
                return Ok(held);
            }
            if !self.share_scan_running.load(Ordering::SeqCst) {
                if self.share_scan_failed.load(Ordering::SeqCst) {
                    return Err(Decline::Internal);
                }
                // Nothing running and nothing held (a kick that could not start): kick
                // again and re-check from the top, where a just-landed snapshot or the
                // failure flag decides.
                self.kick_share_scan(now);
                continue;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero()
                || tokio::time::timeout(remaining, notified.as_mut())
                    .await
                    .is_err()
            {
                let held = self
                    .held_runs
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .clone();
                if let Some(held) = held {
                    return Ok(held);
                }
                eprintln!(
                    "aac-http: scoreboard share scan did not land within {SHARE_COLD_WAIT_MS} ms"
                );
                return Err(Decline::Deadline);
            }
        }
    }

    /// Wait until no share scan is running (bounded like a cold caller). Used by a
    /// background refresh so it composes over the rescanned snapshot, not the old one.
    async fn wait_share_scan_done(&self) {
        let deadline =
            tokio::time::Instant::now() + std::time::Duration::from_millis(SHARE_COLD_WAIT_MS);
        loop {
            let notified = self.share_scan_done.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if !self.share_scan_running.load(Ordering::SeqCst) {
                return;
            }
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            if remaining.is_zero()
                || tokio::time::timeout(remaining, notified.as_mut())
                    .await
                    .is_err()
            {
                return;
            }
        }
    }

    /// Start the detached share scan if none is running. The running flag, the failure
    /// flag and the wake-up all live in that task's `ScanRunning` guard, so no caller's
    /// drop can clear the flag early, skip the wake-up, or lose the cache.
    fn kick_share_scan(&self, now: u64) {
        if self.share_scan_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(owner) = self.me.get().and_then(std::sync::Weak::upgrade) else {
            self.share_scan_failed.store(true, Ordering::SeqCst);
            self.share_scan_running.store(false, Ordering::SeqCst);
            self.share_scan_done.notify_waiters();
            return;
        };
        tokio::spawn(async move {
            let mut running = ScanRunning {
                owner: &owner,
                completed: false,
            };
            running.completed = owner.run_share_scan(now).await;
        });
    }

    /// One detached share scan: the blocking scan over the shared per-file cache, then
    /// its snapshot held for every later caller. Whether it landed a snapshot.
    async fn run_share_scan(&self, now: u64) -> bool {
        let root = self.root.clone();
        let cache = Arc::clone(&self.share_cache);
        let scanned = tokio::task::spawn_blocking(move || {
            let mut cache = cache.lock().unwrap_or_else(|p| p.into_inner());
            scan_share_runs_cached(&root, now, &mut cache)
        })
        .await;
        match scanned {
            Ok(scan) => {
                self.share_scan_failed.store(false, Ordering::SeqCst);
                self.share_fallback.store(true, Ordering::SeqCst);
                self.share_held_at.store(now, Ordering::SeqCst);
                let held = Arc::new(
                    HeldRuns::from_entries(self.generation(), scan.rows, scan.problems, None)
                        .with_share(scan.read),
                );
                *self.held_runs.lock().unwrap_or_else(|p| p.into_inner()) = Some(held);
                self.held_seq.fetch_add(1, Ordering::SeqCst);
                true
            }
            Err(_) => {
                self.share_scan_failed.store(true, Ordering::SeqCst);
                eprintln!("aac-http: scoreboard share scan task failed");
                false
            }
        }
    }

    /// The runs state (`RunsStore.state`) of the held snapshot's `picked` rows: the receipts,
    /// the problems (refused files, the pass's, the snapshot's) and the rows, at `clock`.
    async fn state_of(
        &self,
        held: &HeldRuns,
        picked: &[usize],
        clock: u64,
    ) -> Result<Json, Decline> {
        self.state_with(held, picked, clock, true).await
    }

    /// The store's facts as of now: held until the next `runsRecord` (see `store_epoch`); a
    /// read that overlaps a write is used once and not held.
    async fn store_facts(&self) -> Arc<StoreFacts> {
        let epoch = self.store_epoch.load(Ordering::SeqCst);
        if let Some((at, facts)) = self
            .store_facts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && *at == epoch
        {
            return facts.clone();
        }
        let call = |method: &'static str| async move {
            self.store
                .as_ref()?
                .call(method.to_string(), "{}".to_string())
                .await
                .ok()
                .and_then(|text| serde_json::from_str::<Json>(&text).ok())
        };
        let refused = call("runsRefused")
            .await
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
        let identity = call("runsIdentity").await.unwrap_or(Json::Null);
        let status_runs = call("runsStatus")
            .await
            .map(|status| status["runs"].clone())
            .unwrap_or(Json::Null);
        let facts = Arc::new(StoreFacts {
            refused,
            identity,
            status_runs,
        });
        if self.store_epoch.load(Ordering::SeqCst) == epoch && epoch.is_multiple_of(2) {
            *self.store_facts.lock().unwrap_or_else(|p| p.into_inner()) =
                Some((epoch, facts.clone()));
        }
        facts
    }

    /// [`Self::state_of`]; without `receipts` the state leaves out `accepted` (the composer
    /// never reads it; the readers-check rows take the held receipts directly).
    async fn state_with(
        &self,
        held: &HeldRuns,
        picked: &[usize],
        clock: u64,
        receipts: bool,
    ) -> Result<Json, Decline> {
        self.state_rows(held, picked, clock, receipts, true).await
    }

    /// [`Self::state_with`]; without `rows` the state's `runs` is empty (the composer takes the
    /// held rows by reference).
    async fn state_rows(
        &self,
        held: &HeldRuns,
        picked: &[usize],
        clock: u64,
        receipts: bool,
        rows: bool,
    ) -> Result<Json, Decline> {
        let store_facts = self.store_facts().await;
        let refused = &store_facts.refused;
        let safe_paths = self.safe_paths.lock().unwrap_or_else(|p| p.into_inner());
        let pass = self.pass.lock().unwrap_or_else(|p| p.into_inner());
        // After a restart the scan's path map starts empty, but a refused file that shares its
        // path with a retained run is still that run's file: the refusal names it again so the
        // row's warning matches. Other refused paths stay withheld (M2).
        let retained_paths: HashMap<String, &str> = held
            .state_rows
            .iter()
            .filter_map(|row| row["file"].as_str().map(|file| (path_hash(file), file)))
            .collect();
        let mut problems: Vec<Json> = if held.share.is_some() {
            Vec::new()
        } else {
            refused
                .iter()
                .map(|row| {
                    let hash = row["pathHash"].as_str().unwrap_or_default();
                    let file = safe_paths
                        .get(hash)
                        .map(String::as_str)
                        .or_else(|| retained_paths.get(hash).copied())
                        .map(str::to_string)
                        .unwrap_or_else(|| {
                            format!("refused/{NAME_WITHHELD} {}", &hash[..hash.len().min(12)])
                        });
                    json!({
                        "file": file,
                        "reason": row["reason"].as_str().unwrap_or("refused"),
                        "since": row["atMs"],
                    })
                })
                .collect()
        };
        if held.share.is_none() {
            problems.extend(pass.problems.iter().cloned());
        }
        problems.extend(held.problems.iter().cloned());
        drop(safe_paths);
        let mut writers = std::collections::HashSet::new();
        let runs: Vec<Json> = picked
            .iter()
            .map(|&index| {
                writers.insert(held.rows[index].writer.as_str());
                index
            })
            .collect::<Vec<usize>>()
            .into_iter()
            .filter(|_| rows)
            .map(|index| held.rows[index].state_row())
            .collect();
        // Built by moving the rows in (`json!` would serialize, a deep copy of every row).
        let writers = writers.len();
        let state = if let Some(share) = &held.share {
            share.state.as_str()
        } else if self.store.is_none() {
            "disabled"
        } else if pass.state.is_empty() {
            "ok"
        } else {
            pass.state.as_str()
        };
        let scanned_at = if let Some(share) = &held.share {
            share.scanned_at
        } else {
            self.store.as_ref().map(|_| clock)
        };
        let skipped = held
            .share
            .as_ref()
            .map(|share| share.skipped.clone())
            .unwrap_or_else(|| json!({"runFiles": 0, "writerFolders": 0}));
        let mut out = Map::new();
        out.insert("correctionsResolved".to_string(), Json::Bool(true));
        out.insert(
            "accepted".to_string(),
            if receipts {
                held.accepted_json().map(Json::Array).unwrap_or(Json::Null)
            } else {
                Json::Null
            },
        );
        out.insert("state".to_string(), json!(state));
        out.insert("scannedAt".to_string(), json!(scanned_at));
        out.insert("runs".to_string(), Json::Array(runs));
        out.insert("writers".to_string(), json!(writers));
        out.insert("skipped".to_string(), skipped);
        out.insert("problems".to_string(), Json::Array(problems));
        Ok(Json::Object(out))
    }

    /// The held snapshot's per-row preparation over this catalog, made once per catalog
    /// hash: each row's model patch, prototype flag and composer facts. Docs are parsed
    /// one at a time and dropped; only the compact results are held.
    fn held_docs(&self, held: &HeldRuns, catalog: &Json, catalog_hash: &str) -> Arc<PreparedDocs> {
        let mut docs = held.docs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((hash, made)) = docs.as_ref()
            && hash == catalog_hash
        {
            return made.clone();
        }
        let models: Vec<Model<'_>> = catalog["models"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| Model {
                        model: row["model"].as_str().unwrap_or_default(),
                        efforts: &[],
                    })
                    .collect()
            })
            .unwrap_or_default();
        // The catalog's efforts live in each model row; collect them per model for the lookup.
        let efforts: Vec<Vec<String>> = catalog["models"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        row["efforts"]
                            .as_array()
                            .map(|efforts| {
                                efforts
                                    .iter()
                                    .filter_map(Json::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default()
                    })
                    .collect()
            })
            .unwrap_or_default();
        let models: Vec<Model<'_>> = models
            .iter()
            .zip(&efforts)
            .map(|(model, efforts)| Model {
                model: model.model,
                efforts,
            })
            .collect();
        let empty_aliases = Map::new();
        let aliases = catalog["aliases"].as_object().unwrap_or(&empty_aliases);
        let empty_suffixes: Vec<String> = Vec::new();
        let suffixes: Vec<String> = catalog["effortSuffixes"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .filter_map(Json::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let suffixes = if suffixes.is_empty() {
            &empty_suffixes
        } else {
            &suffixes
        };
        let table = AliasTable {
            aliases,
            effort_suffixes: suffixes,
        };
        let mut patches = Vec::with_capacity(held.rows.len());
        let mut proto = Vec::with_capacity(held.rows.len());
        let mut facts = Vec::with_capacity(held.rows.len());
        for entry in &held.rows {
            let raw = entry.doc();
            let id = raw["model"]["id"].as_str().unwrap_or_default();
            let patch = if id.is_empty() {
                None
            } else {
                Some(normalize_model(
                    id,
                    raw["model"]["effort"].as_str(),
                    &models,
                    Some(&table),
                ))
            };
            let normalized = normalized_doc(raw.clone(), &patch);
            let state = json!({
                "file": entry.file,
                "doc": raw,
                "firstSeenAt": entry.first_seen_at,
                "sha256": entry.sha256,
            });
            proto.push(
                crate::status_fed::proto_exact(&state)
                    || crate::status_fed::proto_exact(&normalized),
            );
            facts.push(aac_status::read_scoreboard::row_facts(&state, &normalized));
            patches.push(patch);
        }
        let made = Arc::new(PreparedDocs {
            patches,
            proto,
            facts,
            order: Mutex::new(None),
        });
        *docs = Some((catalog_hash.to_string(), made.clone()));
        made
    }

    /// The presentation for one answer, and the generation id it is held under (the
    /// newest-order memo keys on it): a generation the caller pushed into the feed wins,
    /// parsed once per id; otherwise the facts computed here over the held snapshot and
    /// this answer's one catalog snapshot — the same values the front door computed over
    /// `inputs`, without shipping the whole run set to it and back, and without the feed
    /// body's transport cap. Computed once per [`FactsKey`], singleflight, on the
    /// blocking pool; a catalog scan-stamp-only move patches the one new date fact into
    /// the kept text instead of recomputing. Computed facts are held only under their
    /// own key-derived id, never under a caller-named one, and are no longer published
    /// into the feed (no reader remains, and the raw copy cost real memory).
    async fn presentation(
        &self,
        feed: &Feed,
        held: &Arc<HeldRuns>,
        catalog: &Json,
        runs: &Json,
    ) -> Result<(Arc<Presentation>, String), Decline> {
        if let Some(id) = &feed.facts {
            let mut parsed = self.parsed_facts.lock().unwrap_or_else(|p| p.into_inner());
            if let Some((held_id, presentation)) = parsed.as_ref()
                && held_id == id
            {
                return Ok((presentation.clone(), id.clone()));
            }
            if let Ok(generation) = self.feeds.current("scoreboard-facts", id) {
                let presentation: Presentation =
                    serde_json::from_str(generation.facts.get()).map_err(|_| Decline::Shape)?;
                let presentation = Arc::new(presentation);
                *parsed = Some((id.clone(), presentation.clone()));
                return Ok((presentation, id.clone()));
            }
        }
        let seed_failures = self
            .seed_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let key = FactsKey {
            held_seq: self.held_seq.load(Ordering::SeqCst),
            rows: held.rows.len(),
            generation: held.key.0,
            catalog_hash: held.key.1.clone(),
            catalog_scanned_at: catalog["scannedAt"].as_u64(),
            catalog_fingerprint: catalog_fingerprint(catalog),
            runs_problems_hash: sha256_text(&runs["problems"].to_string()),
            seed_hash: sha256_text(&json!(seed_failures).to_string()),
            time_zone: feed.time_zone.clone(),
        };
        if let Some(computed) = self
            .computed_facts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && computed.key == key
        {
            return Ok((computed.presentation.clone(), key.id()));
        }
        let prepared = self.held_docs(held, catalog, &held.key.1);
        let share_only = held.share.is_some();
        let identity = if share_only {
            Json::Null
        } else {
            self.store_facts().await.identity.clone()
        };
        let task_types: Vec<Json> = catalog["taskTypes"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| json!({"id": row["id"], "label": row["label"]}))
                    .collect()
            })
            .unwrap_or_default();
        // The front door's facts input, leaf for leaf: its skeleton (with `root` empty, as
        // it sent), the catalog, the runs state and every doc (raw, with the normalized
        // model words, synthesized row keys and normalized unknown outcomes named alongside).
        let skeleton = json!({
            "now": feed.now,
            "timeZone": feed.time_zone,
            "root": "",
            "joinReviews": [],
            "seedFailures": seed_failures,
            "finish": {
                "filters": {
                    "from": Json::Null,
                    "to": Json::Null,
                    "source": "all",
                    "taskType": Json::Null,
                    "available": !share_only,
                    "taskTypes": task_types,
                },
                "identity": identity,
                "history": {
                    "store": if share_only { "off" } else { "on" },
                    "runs": Json::Null,
                    "importing": false,
                },
            },
        });
        let _compute = self.facts_compute.lock().await;
        if let Some(computed) = self
            .computed_facts
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            && computed.key == key
        {
            return Ok((computed.presentation.clone(), key.id()));
        }
        // A catalog tick that moved only the scan stamp patches its one date fact in.
        let patch_from = {
            let computed = self
                .computed_facts
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            match computed.as_ref() {
                Some(computed)
                    if key.same_but_scanned(&computed.key)
                        && key.catalog_scanned_at.is_some()
                        && key.catalog_scanned_at != computed.key.catalog_scanned_at =>
                {
                    Some(computed.facts_text.clone())
                }
                _ => None,
            }
        };
        if let Some(facts_text) = patch_from
            && let Some(scanned) = key.catalog_scanned_at
            && let Some(patched) = patch_scanned_at_fact(&facts_text, scanned, feed)
        {
            let patched_text = patched.clone();
            let parsed = tokio::task::spawn_blocking(move || -> Result<Presentation, Decline> {
                serde_json::from_str(&patched_text).map_err(|_| Decline::Shape)
            })
            .await
            .map_err(|_| Decline::Internal)?;
            if let Ok(presentation) = parsed {
                let presentation = Arc::new(presentation);
                let id = key.id();
                *self
                    .computed_facts
                    .lock()
                    .unwrap_or_else(|p| p.into_inner()) = Some(ComputedFacts {
                    key,
                    facts_text: patched,
                    presentation: presentation.clone(),
                });
                return Ok((presentation, id));
            }
        }
        let patch_words: Vec<Json> = prepared
            .patches
            .iter()
            .filter_map(|patch| patch.as_ref())
            .flat_map(|patch| {
                [Some(patch.id.clone()), patch.effort.clone()]
                    .into_iter()
                    .flatten()
                    .map(Json::String)
            })
            .collect();
        let patch_words = Json::Array(patch_words);
        let time_zone = feed.time_zone.clone();
        let feed_now = feed.now;
        let catalog_owned = catalog.clone();
        let runs_owned = runs.clone();
        let rows = held.rows.clone();
        let prepared_owned = Arc::clone(&prepared);

        let (facts_text, presentation) =
            tokio::task::spawn_blocking(move || -> Result<(String, Presentation), Decline> {
                let offsets = crate::zone_facts::server_zone_facts(&time_zone, feed_now)
                    .ok_or(Decline::Shape)?;
                // Collect full facts only from the small metadata objects (`skeleton`, `catalog`,
                // `runs`, `patch_words`), and extract ONLY the compared words + parsed timestamps
                // from each run row (skipping prose, evidence, notes, sha256, and `accepted`).
                let mut acc = aac_status::produce::facts::Acc::default();
                let mut words: indexmap::IndexSet<String> = indexmap::IndexSet::new();
                for value in [&skeleton, &catalog_owned, &runs_owned, &patch_words] {
                    acc.read(value, true);
                    words.extend(aac_status::produce::facts::words_of(value));
                }
                let mut extra_epochs: Map<String, Json> = Map::new();
                let mut add_epoch = |text: &str| {
                    if aac_num::ecma::iso_prefixed(text)
                        && !extra_epochs.contains_key(text)
                        && let Some(ms) = aac_num::ecma::parse_instant(text)
                    {
                        extra_epochs.insert(text.to_owned(), Json::from(ms));
                    }
                };
                for (index, entry) in rows.iter().enumerate() {
                    if !entry.id.is_empty() {
                        words.insert(entry.id.clone());
                    }
                    if let Some(tt) = &entry.task_type {
                        words.insert(tt.clone());
                    }
                    if !entry.written_at.is_empty() {
                        add_epoch(&entry.written_at);
                    }
                    let doc = entry.doc();
                    if let Some(ended) = doc["endedAt"].as_str() {
                        add_epoch(ended);
                    }
                    if let Some(bench) = doc["benchmark"].as_str()
                        && !bench.is_empty()
                    {
                        words.insert(bench.to_owned());
                    }
                    if let Some(raw_model) = doc["model"]["id"].as_str()
                        && !raw_model.is_empty()
                    {
                        words.insert(raw_model.to_owned());
                    }
                    let patch = prepared_owned.patches.get(index).and_then(Option::as_ref);
                    let model_id = patch
                        .map(|p| p.id.as_str())
                        .filter(|s| !s.is_empty())
                        .or_else(|| doc["model"]["id"].as_str())
                        .unwrap_or("");
                    if !model_id.is_empty() {
                        let harness = doc["model"]["harness"].as_str().unwrap_or("");
                        let effort = patch
                            .and_then(|p| p.effort.as_deref())
                            .or_else(|| doc["model"]["effort"].as_str())
                            .unwrap_or("");
                        let base_key = format!("{model_id}|{harness}|{effort}");
                        let proc_id = doc["process"]["id"].as_str().unwrap_or("");
                        let proc_ver = doc["process"]["version"].as_str().unwrap_or("");
                        let proc_key = format!("{base_key}|{proc_id}|{proc_ver}");
                        words.insert(base_key);
                        words.insert(proc_key);
                    }
                    if aac_status::read_measures::outcome(&doc) == 0 {
                        words.insert(normalized_unknown_outcome(&doc));
                    }
                }
                let base = aac_status::produce::facts::Facts::build(&acc, Some(&offsets.zone));
                let mut dates = Map::with_capacity(base.dates.len());
                for (ms, date) in &base.dates {
                    dates.insert(
                        ms.to_string(),
                        json!({"iso": date.iso, "clock": date.clock, "day": date.day}),
                    );
                }
                let mut epochs = Map::with_capacity(base.epochs.len() + extra_epochs.len());
                for (k, ms) in base.epochs {
                    epochs.insert(k, Json::from(ms));
                }
                for (k, v) in extra_epochs {
                    epochs.entry(k).or_insert(v);
                }
                let order = crate::status_collate::rank_words(words.iter());
                let mut facts_obj = Map::with_capacity(3);
                facts_obj.insert("dates".to_string(), Json::Object(dates));
                facts_obj.insert("epochs".to_string(), Json::Object(epochs));
                facts_obj.insert("order".to_string(), Json::Object(order));
                let facts_text =
                    serde_json::to_string(&Json::Object(facts_obj)).map_err(|_| Decline::Shape)?;
                let presentation: Presentation =
                    serde_json::from_str(&facts_text).map_err(|_| Decline::Shape)?;
                Ok((facts_text, presentation))
            })
            .await
            .map_err(|_| Decline::Internal)??;
        let presentation = Arc::new(presentation);
        let id = key.id();
        eprintln!(
            "aac-http: scoreboard facts computed in service ({} bytes)",
            facts_text.len()
        );
        *self
            .computed_facts
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(ComputedFacts {
            key,
            facts_text,
            presentation: presentation.clone(),
        });
        Ok((presentation, id))
    }

    /// Refresh one stale view behind the answer that served it. The composing slot is taken
    /// here, so a second spawn while the refresh runs does nothing (singleflight), and a
    /// request that finds the slot taken serves the last good view instead.
    fn spawn_refresh(&self, feed_text: &str, cache_key: &str) {
        let Some(owner) = self.me.get().and_then(std::sync::Weak::upgrade) else {
            return;
        };
        if !self
            .composing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(cache_key.to_string())
        {
            return;
        }
        let feed_text = feed_text.to_string();
        let cache_key = cache_key.to_string();
        tokio::spawn(async move {
            // The slot taken above is released however the refresh ends — including the
            // early returns (a fresh view, a moved generation) that never reach the
            // refresh's own guard.
            let _slot = Composing {
                set: &owner.composing,
                key: cache_key,
            };
            if let Err(reason) = owner.answer_inner(&feed_text, true).await {
                eprintln!(
                    "aac-http: scoreboard refresh declined ({})",
                    reason.as_str()
                );
            }
        });
    }

    /// The owned answer: the final bytes and ETag, or a named decline.
    pub(crate) async fn answer(&self, feed: &str) -> Result<(Vec<u8>, String), Decline> {
        self.answer_inner(feed, false).await
    }

    /// [`Self::answer`]; a refresh composes instead of serving the stale view it replaces.
    async fn answer_inner(
        &self,
        feed_text: &str,
        refresh: bool,
    ) -> Result<(Vec<u8>, String), Decline> {
        let feed: Feed = crate::json_object::from_slice_with_optional(
            feed_text.as_bytes(),
            &[
                "now", "clock", "day", "timeZone", "from", "to", "source", "taskType", "fromMs",
                "toMs",
            ],
            &["generation", "catalogHash", "facts"],
        )
        .map_err(|_| Decline::Shape)?;
        // An explicit null snapshot name is a shape error, not "unnamed" (serde's
        // Option cannot tell the two apart; the feed is small, so read it again).
        let explicit_null = serde_json::from_slice::<Json>(feed_text.as_bytes())
            .map(|raw| {
                raw.get("generation") == Some(&Json::Null)
                    || raw.get("catalogHash") == Some(&Json::Null)
            })
            .unwrap_or(false);
        if explicit_null {
            return Err(Decline::Shape);
        }
        let (generation, catalog_hash) = self.generation();
        // A feed that names a snapshot must name this one; an unnamed feed takes it.
        if feed.generation.is_some_and(|named| named != generation)
            || feed
                .catalog_hash
                .as_deref()
                .is_some_and(|named| named != catalog_hash)
        {
            return Err(Decline::Stale);
        }
        let mut query = Map::new();
        if let Some(from_ms) = feed.from_ms {
            query.insert("fromMs".to_string(), json!(from_ms));
        }
        if let Some(to_ms) = feed.to_ms {
            query.insert("toMs".to_string(), json!(to_ms));
        }
        if feed.source == "real" {
            query.insert("source".to_string(), json!("observed"));
        } else if feed.source == "benchmark" {
            query.insert("source".to_string(), json!("benchmark"));
        }
        if let Some(task_type) = &feed.task_type {
            query.insert("taskType".to_string(), json!(task_type));
        }
        let query = Json::Object(query);
        let now = feed.now.max(0) as u64;
        let now_date =
            Date::of_now(feed.now, feed.clock.clone(), feed.day.clone()).ok_or(Decline::Shape)?;
        // The held snapshot answers every filter: the store is read once per generation,
        // and a share snapshot refreshes behind the held one. The cache keys and the facts
        // are derived from the snapshot actually held and this answer's one catalog
        // snapshot, in this zone — never from a pre-read hint. Share-only snapshots
        // ignore history filters (they are advertised unavailable).
        let mut held = self.held(Some(now)).await?;
        if refresh && held.share.is_some() && self.share_scan_running.load(Ordering::SeqCst) {
            // Compose over the snapshot the in-flight rescan lands, not the old one.
            self.wait_share_scan_done().await;
            held = self.held(Some(now)).await?;
        }
        let seq = self.held_seq.load(Ordering::SeqCst);
        let catalog = self
            .catalog_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if catalog.is_null() {
            return Err(Decline::Stale);
        }
        let share_only = held.share.is_some();
        let cache_key = if share_only {
            format!("share:{generation}:{catalog_hash}:{}", feed.time_zone)
        } else {
            json!([
                feed.from,
                feed.to,
                feed.source,
                feed.task_type,
                generation,
                catalog_hash,
                feed.time_zone
            ])
            .to_string()
        };
        // The filter alone, without the generation: its last good view (see `last_good`).
        let filter_key = if share_only {
            format!("share:{}", feed.time_zone)
        } else {
            json!([
                feed.from,
                feed.to,
                feed.source,
                feed.task_type,
                feed.time_zone
            ])
            .to_string()
        };
        // The 60 s view cache: a hit fills only cacheAgeSeconds, as the TS service does. A
        // stale hit answers at once (marked stale and refreshing) and refreshes behind
        // it; a refresh itself composes.
        let hit = {
            let views = self.views.lock().unwrap_or_else(|p| p.into_inner());
            views.get(&cache_key).cloned()
        };
        if let Some(hit) = hit {
            // Fresh only on the snapshot it was composed from: a landed rescan makes
            // even a young view stale (served marked, refreshed behind).
            if hit.seq == seq && now.saturating_sub(hit.at) < CACHE_MS {
                return Ok(cached_bytes(&hit, now, false));
            }
            if !refresh {
                self.spawn_refresh(feed_text, &cache_key);
                return Ok(cached_bytes(&hit, now, true));
            }
        }
        // One compose per view-cache key. A request that finds its key being composed (its
        // first request outlived the budget and still runs) serves the last good view for
        // its filter while that compose finishes; only a filter with no good view is `busy`.
        // A spawned refresh finds its slot taken by `spawn_refresh`, whose guard owns the
        // release (no second guard here: two removals could free a newer compose's slot);
        // a direct refresh takes and owns the slot itself.
        let _composing: Option<Composing<'_>> = {
            let mut composing = self.composing.lock().unwrap_or_else(|p| p.into_inner());
            let owned = if composing.contains(&cache_key) && refresh {
                false
            } else if !composing.insert(cache_key.clone()) {
                drop(composing);
                let hit = {
                    let views = self.views.lock().unwrap_or_else(|p| p.into_inner());
                    views.get(&cache_key).cloned()
                }
                .or_else(|| {
                    let good = self.last_good.lock().unwrap_or_else(|p| p.into_inner());
                    good.get(&filter_key).cloned()
                });
                if let Some(hit) = hit {
                    return Ok(cached_bytes(&hit, now, true));
                }
                return Err(Decline::Busy);
            } else {
                true
            };
            if owned {
                Some(Composing {
                    set: &self.composing,
                    key: cache_key.clone(),
                })
            } else {
                None
            }
        };
        let picked = if share_only {
            (0..held.rows.len()).collect::<Vec<_>>()
        } else {
            held.pick(&query)
        };
        let filtered = !share_only
            && (feed.from.is_some()
                || feed.to.is_some()
                || feed.source != "all"
                || feed.task_type.is_some());
        let runs = self.state_rows(&held, &picked, now, false, false).await?;
        // The facts generation the request names, pushed by its caller, or the facts
        // computed over this snapshot and runs state when none was pushed. The request's
        // own `now` comes from its header words.
        let (presentation, order_key) = self.presentation(&feed, &held, &catalog, &runs).await?;
        self.record_check_held(&held, &picked, &runs);
        let prepared = self.held_docs(&held, &catalog, &catalog_hash);
        if picked.iter().any(|&index| prepared.proto[index]) {
            return Err(Decline::Unsupported);
        }
        // Reviews outside the filter still join its rows; their validated files come along so the
        // composer can disclose a review source this pass could not read (2026-11-23 A).
        let (join_reviews, join_review_files) = if filtered {
            let picked_ids: std::collections::HashSet<&str> = picked
                .iter()
                .map(|&index| held.rows[index].id.as_str())
                .collect();
            let mut reviews_query = Map::new();
            if let Some(from_ms) = feed.from_ms {
                reviews_query.insert("fromMs".to_string(), json!(from_ms));
            }
            reviews_query.insert("reviewsOnly".to_string(), json!(true));
            let mut joined = Vec::new();
            let mut joined_files = Vec::new();
            for index in held.pick(&Json::Object(reviews_query)) {
                if picked_ids.contains(held.rows[index].id.as_str()) {
                    continue;
                }
                let raw = held.rows[index].doc();
                if raw["review"]["of"]
                    .as_str()
                    .is_some_and(|target| picked_ids.contains(target))
                {
                    if prepared.proto[index] {
                        return Err(Decline::Unsupported);
                    }
                    joined.push(join_review_doc(&raw));
                    joined_files.push(json!(held.rows[index].file.as_ref()));
                }
            }
            (joined, joined_files)
        } else {
            (Vec::new(), Vec::new())
        };
        let mut rows_owned: Vec<Json> = Vec::with_capacity(picked.len());
        let mut docs_owned: Vec<Json> = Vec::with_capacity(picked.len());
        for &index in &picked {
            let entry = &held.rows[index];
            let raw = entry.doc();
            // `compose_core` reads only `id`, `model.id`, `task.type`, and `endedAt` from `rows[i]["doc"]`
            // (for unscored examples); avoid cloning the entire raw run document.
            rows_owned.push(json!({
                "doc": {
                    "id": raw["id"],
                    "model": { "id": raw["model"]["id"] },
                    "task": { "type": raw["task"]["type"] },
                    "endedAt": raw["endedAt"],
                },
            }));
            docs_owned.push(normalized_doc(raw, &prepared.patches[index]));
        }
        let rows: Vec<&Json> = rows_owned.iter().collect();
        let docs: Vec<&Json> = docs_owned.iter().collect();
        let row_facts: Vec<&aac_status::read_scoreboard::RowFacts> =
            picked.iter().map(|&index| &prepared.facts[index]).collect();
        let positions = prepared.positions(&held.rows, &order_key, &presentation);
        let order: Option<Vec<usize>> =
            positions.map(|positions| picked.iter().map(|&index| positions[index]).collect());
        let store_facts = self.store_facts().await;
        let identity = if share_only {
            Json::Null
        } else {
            store_facts.identity.clone()
        };
        let status_runs = if share_only {
            Json::Null
        } else {
            store_facts.status_runs.clone()
        };
        let task_types: Vec<Json> = catalog["taskTypes"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| json!({"id": row["id"], "label": row["label"]}))
                    .collect()
            })
            .unwrap_or_default();
        let importing = self.pass.lock().unwrap_or_else(|p| p.into_inner()).more;
        let seed_failures = self
            .seed_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        // Moved in, not `json!`-serialized (a deep copy of every row and doc).
        let mut data = Map::new();
        data.insert("now".to_string(), json!(feed.now));
        data.insert("timeZone".to_string(), json!(feed.time_zone));
        data.insert("root".to_string(), json!(self.root.to_string_lossy()));
        data.insert("catalog".to_string(), catalog);
        data.insert("runs".to_string(), runs);
        data.insert("joinReviews".to_string(), Json::Array(join_reviews));
        data.insert(
            "joinReviewFiles".to_string(),
            Json::Array(join_review_files),
        );
        data.insert("seedFailures".to_string(), json!(seed_failures));
        data.insert(
            "finish".to_string(),
            json!({
                "filters": {
                    "from": if share_only { Json::Null } else { json!(feed.from) },
                    "to": if share_only { Json::Null } else { json!(feed.to) },
                    "source": if share_only { Json::from("all") } else { json!(feed.source) },
                    "taskType": if share_only { Json::Null } else { json!(feed.task_type) },
                    "available": !share_only,
                    "taskTypes": task_types,
                },
                "identity": identity,
                "history": {
                    "store": if share_only || self.store.is_none() { "off" } else { "on" },
                    "runs": status_runs,
                    "importing": !share_only && importing,
                },
                "native": true,
            }),
        );
        let data = Json::Object(data);
        // A miss is a facts generation older than the data (the catalog's 30 s scan moves its
        // `scannedAt` without a new generation): `stale`, so the front door pushes fresh facts
        // and today's handler answers this one request.
        let view = match aac_status::read_scoreboard::compose_fed_parts(
            &data,
            &rows,
            &docs,
            &row_facts,
            order.as_deref(),
            presentation.clone(),
            (feed.now, now_date),
        ) {
            Some(view) => view,
            None => {
                let reason = Date::of_now(feed.now, feed.clock.clone(), feed.day.clone())
                    .map(|date| {
                        compose_decline_reason(
                            &data,
                            &rows,
                            &docs,
                            &row_facts,
                            order.as_deref(),
                            presentation,
                            (feed.now, date),
                        )
                    })
                    .unwrap_or("compose-returned-none");
                eprintln!("aac-http: scoreboard compose declined ({reason})");
                return Err(Decline::Stale);
            }
        };
        drop(rows);
        drop(docs);
        drop(rows_owned);
        drop(docs_owned);
        drop(data);
        let mut writer = Writer::with_capacity(64 * 1024);
        writer.value(&view);
        drop(view);
        let (bytes, identity_bytes) = writer.finish();
        let digest = Sha256::digest(&identity_bytes);
        drop(identity_bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let etag = format!("W/\"{hex}\"");
        let age_value_offset = find_age_zero_offset(&bytes);
        let cached = Arc::new(CachedView {
            at: now,
            seq,
            bytes: bytes.clone(),
            age_value_offset,
            etag: etag.clone(),
        });
        self.keep_view(cache_key, filter_key, cached);
        Ok((bytes, etag))
    }

    /// The service's unfiltered input (a superset of every filter's rows), the catalog,
    /// the normalized docs and the finish parts, at the current generation: what the
    /// facts are computed over here, kept for the readers that still ask. Built per call
    /// and dropped with it — the front door no longer reads it, and holding a built copy
    /// pinned a full run set in memory for good.
    pub(crate) async fn inputs(&self) -> Result<Json, Decline> {
        let (generation, catalog_hash) = self.generation();
        let held = self.held(None).await?;
        let catalog = self
            .catalog_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let picked: Vec<usize> = (0..held.rows.len()).collect();
        let runs = self.state_of(&held, &picked, now_ms()).await?;
        let prepared = self.held_docs(&held, &catalog, &catalog_hash);
        let docs: Vec<Json> = held
            .rows
            .iter()
            .enumerate()
            .map(|(index, entry)| normalized_doc(entry.doc(), &prepared.patches[index]))
            .collect();
        let identity = if held.share.is_some() {
            Json::Null
        } else {
            self.store_facts().await.identity.clone()
        };
        let seed_failures = self
            .seed_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        Ok(json!({
            "generation": generation,
            "catalogHash": catalog_hash,
            "catalog": catalog,
            "runs": runs,
            "docs": docs,
            "identity": identity,
            "seedFailures": seed_failures,
            "shareOnly": held.share.is_some(),
        }))
    }
}

struct ShareScan {
    rows: Vec<Arc<RunEntry>>,
    problems: Vec<Json>,
    read: ShareRead,
}

fn date_folder(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes
            .iter()
            .enumerate()
            .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
}

fn newest_date(dates: &[String]) -> String {
    dates.iter().max().cloned().unwrap_or_default()
}

fn share_problem(file: impl Into<String>, reason: impl Into<String>, since: u64) -> Json {
    json!({ "file": file.into(), "reason": reason.into(), "since": since })
}

fn ended_at_ms(value: &Json) -> u64 {
    u64::try_from(
        value
            .as_str()
            .and_then(aac_store::run_schema::iso_ms)
            .unwrap_or(0)
            .max(0),
    )
    .unwrap_or_default()
}

fn list_or_problem(
    fs: &NodeFs,
    dir: &Path,
    rel: &str,
    problems: &mut Vec<Json>,
    now: u64,
) -> Option<Vec<aac_status::share::Dirent>> {
    match fs.list(dir) {
        Ok(entries) => Some(entries),
        Err(_) => {
            problems.push(share_problem(rel, "folder could not be listed", now));
            None
        }
    }
}

/// One run file's read at `now`: its row, or its refusal reason; each says whether it may
/// be cached under the file's pre-read stat print. Only the size-cap refusal (a fact of
/// the print's own size) and a row whose file still stats identically after the read
/// qualify: a half-written file's UTF-8/JSON/schema refusal is a fact of the moment, and
/// rechecking the handful of bad files on a rescan is cheap.
enum ShareFile {
    Row(Arc<RunEntry>, bool),
    Problem(String, bool),
}

fn read_share_run(
    fs: &NodeFs,
    abs: &Path,
    rel: &str,
    now: u64,
    print: &str,
    size: u64,
) -> ShareFile {
    let bytes = match fs.read(abs, RUN_CAP) {
        Ok(bytes) => bytes,
        Err(ReadError::OverCap) => {
            return ShareFile::Problem(format!("larger than the {} KB cap", RUN_CAP / 1024), true);
        }
        Err(_) => return ShareFile::Problem("could not be read".to_string(), false),
    };
    let Some(writer) = rel.split('/').nth(1) else {
        return ShareFile::Problem("could not be read".to_string(), false);
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(_) => return ShareFile::Problem("not UTF-8 text".to_string(), false),
    };
    if let Err(reason) = validate::share_json_from_bytes(text.as_bytes(), RUN_CAP) {
        return ShareFile::Problem(reason, false);
    }
    match aac_store::run_schema::parse_run_text(text, writer) {
        Err(invalid) => ShareFile::Problem(invalid.0, false),
        Ok(doc) => {
            // The bytes just read are the row either way; they are only cached when the
            // file still is what the pre-read stat saw (same length, same print).
            let cacheable = bytes.len() as u64 == size
                && fs
                    .stat(abs)
                    .map(|stat| stat_key(&stat) == print)
                    .unwrap_or(false);
            ShareFile::Row(
                Arc::new(RunEntry::from_doc(
                    ended_at_ms(&doc["endedAt"]),
                    rel.to_string(),
                    now,
                    sha256_text(text),
                    &doc,
                )),
                cacheable,
            )
        }
    }
}

/// Bounded, validated share run reads (`RunsReader`): writer/run/byte caps, the run@1
/// projection, the secret filter, and the newest copy of each run id.
#[cfg(test)]
fn scan_share_runs(root: &Path, now: u64) -> ShareScan {
    scan_share_runs_cached(root, now, &mut HashMap::new())
}

/// [`scan_share_runs`], reusing `cache`: a file whose stat print (size, mtime, inode,
/// ctime) still holds contributes its last outcome without a read, so a rescan of a large
/// share stats files instead of rereading them. Files no longer listed leave the cache.
#[allow(clippy::too_many_lines)]
fn scan_share_runs_cached(
    root: &Path,
    now: u64,
    cache: &mut HashMap<String, CachedRun>,
) -> ShareScan {
    let fs = NodeFs;
    let writers_dir = root.join("v1").join("writers");
    let listed = match fs.list(&writers_dir) {
        Ok(entries) => entries,
        Err(error) => {
            let state = if is_missing(&error) {
                // The writers folder itself is gone: its cached outcomes go with it.
                cache.clear();
                "missing"
            } else {
                "error"
            };
            return ShareScan {
                rows: Vec::new(),
                problems: Vec::new(),
                read: ShareRead {
                    state: state.to_string(),
                    scanned_at: Some(now),
                    skipped: json!({ "runFiles": 0, "writerFolders": 0 }),
                },
            };
        }
    };
    let mut problems = Vec::new();
    let mut names: Vec<String> = listed
        .into_iter()
        .filter(|entry| {
            !entry.name.starts_with('.') && entry.is_dir && validate::is_writer_name(&entry.name)
        })
        .map(|entry| entry.name)
        .collect();
    names.sort();
    let mut ranked: Vec<(String, Vec<String>)> = Vec::new();
    for name in &names {
        let writer_rel = format!("writers/{name}");
        let Some(in_writer) = list_or_problem(
            &fs,
            &writers_dir.join(name),
            &writer_rel,
            &mut problems,
            now,
        ) else {
            ranked.push((name.clone(), Vec::new()));
            continue;
        };
        if !in_writer
            .iter()
            .any(|entry| entry.name == "runs" && entry.is_dir)
        {
            ranked.push((name.clone(), Vec::new()));
            continue;
        }
        let runs_rel = format!("{writer_rel}/runs");
        let Some(entries) = list_or_problem(
            &fs,
            &writers_dir.join(name).join("runs"),
            &runs_rel,
            &mut problems,
            now,
        ) else {
            ranked.push((name.clone(), Vec::new()));
            continue;
        };
        let mut dates = Vec::new();
        for entry in entries {
            if entry.name.starts_with('.') {
                continue;
            }
            if !entry.is_dir {
                if showable(&entry.name) {
                    problems.push(share_problem(
                        format!("{runs_rel}/{}", entry.name),
                        "is not a folder",
                        now,
                    ));
                }
                continue;
            }
            if !date_folder(&entry.name) {
                if showable(&entry.name) {
                    problems.push(share_problem(
                        format!("{runs_rel}/{}", entry.name),
                        "folder name is not a date",
                        now,
                    ));
                }
                continue;
            }
            dates.push(entry.name);
        }
        ranked.push((name.clone(), dates));
    }
    ranked.sort_by(|left, right| {
        newest_date(&right.1)
            .cmp(&newest_date(&left.1))
            .then(left.0.cmp(&right.0))
    });
    let writer_skipped = ranked.len().saturating_sub(SHARE_WRITER_CAP);
    if writer_skipped > 0 {
        problems.push(share_problem(
            "limits/run-writers",
            format!(
                "{writer_skipped} writer folders past the limit of {SHARE_WRITER_CAP} were not listed (writers rank by their newest run-date folder, which may be empty, so a newer empty date folder can outrank a writer with newer runs), so their run files are not read and not counted: the not-read run file count is a lower bound"
            ),
            now,
        ));
    }
    let mut candidates: Vec<(String, String, PathBuf)> = Vec::new();
    for (name, dates) in ranked.into_iter().take(SHARE_WRITER_CAP) {
        for date in dates {
            let date_rel = format!("writers/{name}/runs/{date}");
            let date_dir = writers_dir.join(&name).join("runs").join(&date);
            let Some(entries) = list_or_problem(&fs, &date_dir, &date_rel, &mut problems, now)
            else {
                continue;
            };
            for file in entries {
                if file.name.starts_with('.') || !file.name.ends_with(".json") {
                    continue;
                }
                if !aac_store::run_schema::run_file_name_readable(&file.name) {
                    continue;
                }
                if !file.is_file {
                    problems.push(share_problem(
                        format!("{date_rel}/{}", file.name),
                        "is not a regular file",
                        now,
                    ));
                    continue;
                }
                candidates.push((
                    date.clone(),
                    format!("{date_rel}/{}", file.name),
                    date_dir.join(&file.name),
                ));
            }
        }
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0).then(left.1.cmp(&right.1)));
    let mut visited = 0usize;
    let mut bytes = 0u64;
    let mut files = Vec::new();
    for (_date, rel, abs) in &candidates {
        if visited >= SHARE_RUN_CAP || bytes >= SHARE_BYTE_CAP {
            break;
        }
        let stat = fs.stat(abs).ok();
        let size = stat.as_ref().map(|stat| stat.size).unwrap_or(0);
        let print = stat.as_ref().map(stat_key).unwrap_or_default();
        bytes = bytes.saturating_add(size);
        visited += 1;
        files.push((rel.clone(), abs.clone(), print, size));
    }
    let skipped_runs = candidates.len().saturating_sub(visited);
    if skipped_runs > 0 {
        problems.push(share_problem(
            "limits/runs",
            format!(
                "{skipped_runs} run files past the limit of {SHARE_RUN_CAP} run files or {} MB were not read (the newest days are kept)",
                SHARE_BYTE_CAP / (1024 * 1024)
            ),
            now,
        ));
    }
    let visited_files: std::collections::HashSet<String> =
        files.iter().map(|(rel, _, _, _)| rel.clone()).collect();
    let mut parsed: Vec<Arc<RunEntry>> = Vec::new();
    for (rel, abs, print, size) in files {
        if aac_status::secret_policy::find_secret(&rel).is_some() {
            problems.push(share_problem(shown(&rel), SECRET_NAME_REASON, now));
            continue;
        }
        // Unchanged since its last read: the held outcome stands (its row keeps the
        // first-seen stamp of that read).
        if !print.is_empty()
            && let Some(hit) = cache.get(&rel).filter(|cached| cached.print == print)
        {
            match &hit.outcome {
                CachedOutcome::Row { row } => parsed.push(Arc::clone(row)),
                CachedOutcome::Problem { reason } => {
                    problems.push(share_problem(rel, reason.clone(), now));
                }
            }
            continue;
        }
        match read_share_run(&fs, &abs, &rel, now, &print, size) {
            ShareFile::Row(row, cacheable) => {
                if cacheable && !print.is_empty() {
                    cache.insert(
                        rel,
                        CachedRun {
                            print,
                            outcome: CachedOutcome::Row {
                                row: Arc::clone(&row),
                            },
                        },
                    );
                }
                parsed.push(row);
            }
            ShareFile::Problem(reason, cacheable) => {
                if cacheable && !print.is_empty() {
                    cache.insert(
                        rel.clone(),
                        CachedRun {
                            print,
                            outcome: CachedOutcome::Problem {
                                reason: reason.clone(),
                            },
                        },
                    );
                }
                problems.push(share_problem(rel, reason, now));
            }
        }
    }
    // Only what this scan visited stays cached (files past the caps leave with it).
    cache.retain(|rel, _| visited_files.contains(rel.as_str()));
    // The newest copy of each run id (seq, then writtenAt, then file), as before.
    let mut best: HashMap<String, Arc<RunEntry>> = HashMap::new();
    for entry in parsed {
        let newer = match best.get(&entry.id) {
            None => true,
            Some(held) => {
                entry.seq > held.seq
                    || (entry.seq == held.seq && entry.written_at > held.written_at)
                    || (entry.seq == held.seq
                        && entry.written_at == held.written_at
                        && entry.file > held.file)
            }
        };
        if newer {
            best.insert(entry.id.clone(), entry);
        }
    }
    let mut records: Vec<Arc<RunEntry>> = best.into_values().collect();
    records.sort_by(|left, right| left.id.cmp(&right.id));
    // Correction chains are rare: with no `corrects` and no `void` anywhere, resolution
    // is the identity (verified against `resolve`), so the entries pass through shared
    // instead of being cloned through JSON. Otherwise the legacy path runs unchanged.
    let plain = records
        .iter()
        .all(|entry| entry.corrects.is_none() && !entry.has_void);
    let rows: Vec<Arc<RunEntry>> = if plain {
        records
    } else {
        let docs: Vec<Json> = records.iter().map(|entry| entry.state_row()).collect();
        let resolved = aac_store::run_corrections::resolve(&docs, |value| {
            value.as_str().and_then(aac_store::run_schema::iso_ms)
        });
        for row in &docs {
            if let Some(id) = row["doc"]["id"].as_str()
                && let Some(reason) = resolved.refused.get(id)
            {
                problems.push(share_problem(
                    row["file"].as_str().unwrap_or_default(),
                    reason.clone(),
                    row["firstSeenAt"].as_u64().unwrap_or(now),
                ));
            }
        }
        resolved
            .rows
            .iter()
            .map(|row| Arc::new(RunEntry::from_row(ended_at_ms(&row["doc"]["endedAt"]), row)))
            .collect()
    };
    ShareScan {
        rows,
        problems,
        read: ShareRead {
            state: "ok".to_string(),
            scanned_at: Some(now),
            skipped: json!({
                "runFiles": skipped_runs,
                "writerFolders": writer_skipped,
            }),
        },
    }
}

/// Remembered non-secret paths by hash, for naming a refused file: a bounded FIFO
/// (a path seen again is refreshed to newest; the oldest leaves past the cap).
pub(crate) type SafePaths = IndexMap<String, String>;

fn remember_safe_path(safe_paths: &mut SafePaths, path: &str) {
    let hash = path_hash(path);
    safe_paths.shift_remove(&hash);
    safe_paths.insert(hash, path.to_string());
    while safe_paths.len() > SAFE_PATHS_CAP {
        safe_paths.shift_remove_index(0);
    }
}

/// G16b: the ingest's decision step between `runsScan` and `runsRecord` (`RunsStore.runPass`):
/// every path and writer folder through the secret filter, every text through
/// `share_json_from_bytes`, the refusals with their fixed reasons, and the pass problems. A pure
/// function of the scan and the clock, so the runs-ingest gate replays the TS ingest on it.
pub(crate) struct IngestRequest {
    /// The `runsRecord` arguments.
    pub(crate) record: Json,
    /// The pass's problems.
    pub(crate) problems: Vec<Json>,
    /// Whether the backfill has more to read.
    pub(crate) more: bool,
}

pub(crate) fn ingest_request(scan: &Json, now: u64, safe_paths: &mut SafePaths) -> IngestRequest {
    let (files, refused, threads, read_count) = {
        let mut remember = |path: &str| {
            if aac_status::secret_policy::find_secret(path).is_none() {
                remember_safe_path(safe_paths, path);
            }
        };
        let mut files: Vec<Json> = Vec::new();
        let mut refused: Vec<Json> = scan["refused"].as_array().cloned().unwrap_or_default();
        for row in scan["refused"].as_array().cloned().unwrap_or_default() {
            if let Some(path) = row["path"].as_str() {
                remember(path);
            }
        }
        for file in scan["files"].as_array().cloned().unwrap_or_default() {
            let path = file["path"].as_str().unwrap_or_default();
            let text = file["text"].as_str().unwrap_or_default();
            remember(path);
            // A file's name can hold a credential too: refused, its name kept nowhere.
            if aac_status::secret_policy::find_secret(path).is_some() {
                refused.push(json!({
                    "path": path,
                    "size": file["size"],
                    "mtimeMs": file["mtimeMs"],
                    "reason": SECRET_NAME_REASON,
                    "sha256": sha256_text(text),
                }));
                continue;
            }
            match validate::share_json_from_bytes(text.as_bytes(), RUN_CAP) {
                Ok(_) => files.push(file),
                Err(reason) => refused.push(json!({
                    "path": path,
                    "size": file["size"],
                    "mtimeMs": file["mtimeMs"],
                    "reason": reason,
                    "sha256": sha256_text(text),
                })),
            }
        }
        let mut threads = Map::new();
        if let Some(map) = scan["threads"].as_object() {
            for (writer, text) in map {
                let text = text.as_str().unwrap_or_default();
                if aac_status::secret_policy::find_secret(writer).is_none()
                    && validate::share_json_from_bytes(text.as_bytes(), THREAD_CAP).is_ok()
                {
                    threads.insert(writer.clone(), json!(text));
                }
            }
        }
        let read_count = files.len();
        (files, refused, threads, read_count)
    };
    let catalog_text = |value: &Json| -> Json {
        match value.as_str() {
            Some(text) if validate::share_json_from_bytes(text.as_bytes(), CATALOG_CAP).is_ok() => {
                json!(text)
            }
            _ => Json::Null,
        }
    };
    let record = json!({
        "files": files,
        "refused": refused,
        "threads": Json::Object(threads),
        "catalog": {
            "models": catalog_text(&scan["catalog"]["models"]),
            "aliases": catalog_text(&scan["catalog"]["aliases"]),
        },
        "nowMs": now,
    });
    let mut problems: Vec<Json> = scan["problems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|problem| {
            json!({
                "file": shown(problem["file"].as_str().unwrap_or_default()),
                "reason": problem["reason"],
                "since": now,
            })
        })
        .collect();
    let more = scan["more"].as_bool().unwrap_or(false);
    if more {
        problems.push(json!({
            "file": "limits/import",
            "reason": format!(
                "Importing run history: {} files read this pass; the rest follow in the next passes.",
                read_count + refused.len()
            ),
            "since": now,
        }));
    }
    IngestRequest {
        record,
        problems,
        more,
    }
}

/// The ingest loop: a pass now, again within a second while a backfill has more, then every
/// two minutes; single-flight (the loop awaits each pass), until the service ends.
pub(crate) async fn ingest_jobs(
    owner: Arc<ScoreboardOwned>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    initial_pass: bool,
) {
    if initial_pass {
        owner.ingest_pass(false).await;
    }
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let more = owner.pass.lock().unwrap_or_else(|p| p.into_inner()).more;
        tokio::time::sleep(std::time::Duration::from_millis(if more {
            MORE_MS
        } else {
            PASS_MS
        }))
        .await;
        if stop.load(Ordering::Acquire) {
            return;
        }
        owner.ingest_pass(false).await;
    }
}

/// The catalog loop: a scan now and every 30 s, until the service ends.
pub(crate) async fn catalog_jobs(
    owner: Arc<ScoreboardOwned>,
    stop: Arc<std::sync::atomic::AtomicBool>,
    initial_pass: bool,
) {
    if initial_pass {
        owner.catalog_pass(now_ms());
    }
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(30_000)).await;
        if stop.load(Ordering::Acquire) {
            return;
        }
        owner.catalog_pass(now_ms());
    }
}

/// The readers-check rows of runs-state rows (`file`, `doc.seq`, `sha256`, `firstSeenAt`).
fn check_rows(rows: &[Json]) -> Vec<RunRow> {
    check_rows_of(&rows.iter().collect::<Vec<_>>())
}

fn check_rows_of(rows: &[&Json]) -> Vec<RunRow> {
    rows.iter()
        .map(|row| RunRow {
            file: row["file"].as_str().unwrap_or_default().to_string(),
            seq: row["doc"]["seq"].as_u64().unwrap_or(0),
            sha256: row["sha256"].as_str().unwrap_or_default().to_string(),
            first_seen_at: row["firstSeenAt"].as_u64().unwrap_or(0),
        })
        .collect()
}

/// This pass's problems, with a continuing problem keeping the time it was first observed: the
/// scanner names the failing folder every pass and the owner stamps it, so the age must come from
/// the first pass that saw it. A problem that stopped is dropped with the pass that no longer
/// reports it (M3).
fn merge_pass_problems(previous: &[Json], fresh: Vec<Json>) -> Vec<Json> {
    if previous.is_empty() {
        return fresh;
    }
    let first: HashMap<&str, u64> = previous
        .iter()
        .filter_map(|problem| Some((problem["file"].as_str()?, problem["since"].as_u64()?)))
        .collect();
    fresh
        .into_iter()
        .map(|mut problem| {
            let since = problem["file"]
                .as_str()
                .and_then(|file| first.get(file).copied());
            if let (Some(since), Some(obj)) = (since, problem.as_object_mut()) {
                obj.insert("since".to_string(), json!(since));
            }
            problem
        })
        .collect()
}

/// The readers-check problems of a runs state.
fn check_problems(runs: &Json) -> Vec<Problem> {
    runs["problems"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .map(|problem| Problem {
                    file: problem["file"].as_str().unwrap_or_default().to_string(),
                    reason: problem["reason"].as_str().unwrap_or_default().to_string(),
                    since: problem["since"].as_u64().unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The few fields `read_measures::Index`, `quality`, and `overclaim` read from a joined review
/// run outside the filtered row set. Unused prose (`task.title`, `outcome`, `flags`) stays out of
/// `data["joinReviews"]` so a prototype word inside prose never trips `proto_named(data)`.
fn join_review_doc(raw: &Json) -> Json {
    let evidence = raw["evidence"]
        .as_array()
        .and_then(|items| items.first())
        .map(|first| json!([{"path": first["path"]}]))
        .unwrap_or(Json::Null);
    json!({
        "id": raw["id"],
        "endedAt": raw["endedAt"],
        "task": {"type": raw["task"]["type"]},
        "model": {
            "id": raw["model"]["id"],
            "provider": raw["model"]["provider"],
            "effort": raw["model"]["effort"],
        },
        "review": {
            "of": raw["review"]["of"],
            "verdict": raw["review"]["verdict"],
            "overturned": raw["review"]["overturned"],
            "disputed": raw["review"]["disputed"],
            "by": raw["review"]["by"],
        },
        "evidence": evidence,
    })
}

/// Normalized unknown-outcome label matching `read_scoreboard::runs_source_of`.
fn normalized_unknown_outcome(doc: &Json) -> String {
    doc["outcome"]
        .as_str()
        .unwrap_or("")
        .split(aac_status::read_context::js_whitespace)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Name why `compose_fed_parts` declined (`None`), for one journal line. Called only on the
/// decline path so the happy path pays nothing; returns only a fixed reason tag, never run text.
#[allow(clippy::too_many_arguments)]
fn compose_decline_reason(
    data: &Json,
    rows: &[&Json],
    docs: &[&Json],
    row_facts: &[&aac_status::read_scoreboard::RowFacts],
    order: Option<&[usize]>,
    facts: Arc<Presentation>,
    now: (i64, Date),
) -> &'static str {
    if data["runs"]["correctionsResolved"] != true {
        return "corrections-unresolved";
    }
    if aac_status::read_context::proto_named(data) {
        return "proto-named";
    }
    if rows.len() != docs.len()
        || row_facts.len() != rows.len()
        || order.is_some_and(|o| o.len() != rows.len())
    {
        return "length-mismatch";
    }
    let ctx = aac_status::read_context::Context::shared(facts.clone(), Some(now)).with_iso_any();
    let missing_clock = |value: &Json| -> bool {
        if value.is_null() {
            return false;
        }
        let Some(ms) = ctx.epoch(value) else {
            return true;
        };
        let _ = ctx.clock(ms);
        !ctx.valid()
    };
    if missing_clock(&data["runs"]["scannedAt"]) || missing_clock(&data["catalog"]["scannedAt"]) {
        return "missing-date-fact";
    }
    if let Some(problems) = data["runs"]["problems"].as_array()
        && problems
            .iter()
            .filter(|p| !p["file"].as_str().unwrap_or("").starts_with("limits/"))
            .any(|p| missing_clock(&p["since"]))
    {
        return "missing-runs-problem-date";
    }
    if let Some(problems) = data["catalog"]["problems"].as_array()
        && problems.iter().any(|p| missing_clock(&p["since"]))
    {
        return "missing-catalog-problem-date";
    }
    for doc in docs {
        if !doc["endedAt"].is_null() {
            let _ = ctx.epoch(&doc["endedAt"]);
            if !ctx.valid() {
                return "missing-epoch-fact";
            }
        }
        let id = doc["id"].as_str().unwrap_or("");
        if !id.is_empty() && ctx.rank(id).is_none() {
            return "missing-id-rank";
        }
        if let Some(bench) = doc["benchmark"].as_str()
            && !bench.is_empty()
            && ctx.rank(bench).is_none()
        {
            return "missing-benchmark-rank";
        }
        if let Some(model) = doc["model"]["id"].as_str()
            && !model.is_empty()
        {
            let harness = doc["model"]["harness"].as_str().unwrap_or("");
            let effort = doc["model"]["effort"].as_str().unwrap_or("");
            let base_key = format!("{model}|{harness}|{effort}");
            let proc_id = doc["process"]["id"].as_str().unwrap_or("");
            let proc_ver = doc["process"]["version"].as_str().unwrap_or("");
            let proc_key = format!("{base_key}|{proc_id}|{proc_ver}");
            if ctx.rank(&base_key).is_none() || ctx.rank(&proc_key).is_none() {
                return "missing-row-key-rank";
            }
        }
        if aac_status::read_measures::outcome(doc) == 0
            && ctx.rank(&normalized_unknown_outcome(doc)).is_none()
        {
            return "missing-outcome-rank";
        }
    }
    if data["finish"]["native"] == true {
        let mut no_native = data.clone();
        no_native["finish"]["native"] = json!(false);
        if let Some(now_ms) = data["now"].as_i64()
            && let Some(date) = Date::of_now(now_ms, String::new(), String::new())
            && aac_status::read_scoreboard::compose_fed_parts(
                &no_native,
                rows,
                docs,
                row_facts,
                order,
                facts,
                (now_ms, date),
            )
            .is_some()
        {
            return "native-by-model-shape";
        }
    }
    "compose-returned-none"
}

#[cfg(test)]
mod tests {
    use super::{
        CACHE_BYTES, CACHE_ENTRIES, CachedView, HeldRuns, ScoreboardOwned, scan_share_runs,
        scan_share_runs_cached,
    };
    use crate::store_host::{StoreBoot, StoreHost};
    use aac_files::registry::Owners;
    use aac_status::share::check::ReadersCheckWriter;
    use serde_json::json;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    const BENCH: &str = r#"{"schema":"agent-reports/run@1","id":"vm.lead--bench","writer":"vm.lead","seq":1,"writtenAt":"2026-11-04T16:30:00Z","source":"benchmark","project":"proj","stream":null,"task":{"id":"bench","type":"build","title":"Task bench","size":{"tasks":1}},"model":{"provider":"anthropic","id":"claude-opus-5-5","harness":"claude-code","effort":"high"},"startedAt":null,"endedAt":"2026-11-04T16:30:00Z","wallSeconds":100,"outcome":"done","benchmark":"pilot"}"#;

    fn write_bench(root: &Path) {
        let dir = root.join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        std::fs::write(dir.join("vm.lead--bench.json"), BENCH).expect("run file");
    }

    const REVIEW: &str = r#"{"schema":"agent-reports/run@1","id":"mac.reviews--R1","writer":"mac.reviews","seq":1,"writtenAt":"2026-11-04T16:45:00Z","source":"observed","project":"proj","stream":null,"task":{"id":"review-bench","type":"review","title":"Review bench","size":{"tasks":1}},"model":{"provider":"anthropic","id":"claude-opus-5-5","harness":"claude-code","effort":"high"},"startedAt":null,"endedAt":"2026-11-04T16:45:00Z","wallSeconds":60,"outcome":"done","benchmark":null,"review":{"of":"vm.lead--bench","verdict":"pass","findings":{"high":0,"medium":0,"low":0},"overturned":false,"disputed":false,"by":null}}"#;

    /// The bench run's blind review by `mac.reviews`, which the WorkNode build filter joins from
    /// outside it.
    fn write_review(root: &Path) {
        let dir = root.join("v1/writers/mac.reviews/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("review folder");
        std::fs::write(dir.join("mac.reviews--R1.json"), REVIEW).expect("review file");
    }

    fn boot(dir: PathBuf) -> StoreBoot {
        StoreBoot {
            binding_identity: aac_store::build_identity(),
            dir,
            retention_days: 0,
            cache_bytes: None,
            activity_retention_days: None,
            snapshot_directory: None,
            share_backup_dir: None,
            share_backup_off: true,
        }
    }

    fn owner(root: PathBuf, store: Option<Arc<StoreHost>>) -> Arc<ScoreboardOwned> {
        let rows = aac_files::registry::rows().expect("registry");
        ScoreboardOwned::new(
            root.clone(),
            store,
            crate::feed::Feeds::new(),
            Arc::new(Mutex::new(ReadersCheckWriter::new(root))),
            Owners::defaults(rows),
            Arc::new(AtomicBool::new(false)),
        )
    }

    fn has_bench(held: &HeldRuns) -> bool {
        held.rows.iter().any(|entry| entry.id == "vm.lead--bench")
    }

    #[test]
    fn the_share_scan_reads_the_reviewers_valid_run() {
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let scan = scan_share_runs(root.path(), 1_790_000_000_000);
        assert_eq!(scan.read.state, "ok");
        assert_eq!(scan.read.scanned_at, Some(1_790_000_000_000));
        assert!(
            scan.rows.iter().any(|entry| entry.id == "vm.lead--bench"),
            "{} rows",
            scan.rows.len()
        );
    }

    #[tokio::test]
    async fn store_off_still_returns_the_share_run() {
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.catalog_pass(1_790_000_000_000);
        let held = owner.held(Some(1_790_000_000_000)).await.expect("held");
        assert!(held.share.is_some(), "share fallback");
        assert!(
            has_bench(&held),
            "missing vm.lead--bench ({} rows)",
            held.rows.len()
        );
        let picked: Vec<usize> = (0..held.rows.len()).collect();
        let state = owner
            .state_of(&held, &picked, 1_790_000_000_000)
            .await
            .expect("state");
        assert_eq!(state["state"], "ok");
        assert_eq!(state["scannedAt"], json!(1_790_000_000_000u64));
        assert_eq!(state["skipped"]["runFiles"], json!(0));
        assert!(owner.share_fallback.load(Ordering::SeqCst));
        let inputs = owner.inputs().await.expect("inputs");
        assert_eq!(inputs["shareOnly"], json!(true));
        assert!(
            inputs["runs"]["runs"]
                .as_array()
                .expect("runs")
                .iter()
                .any(|row| row["doc"]["id"].as_str() == Some("vm.lead--bench")),
            "{}",
            inputs["runs"]
        );
    }

    /// Live Nov 23: run notes such as "C8-B1 constructor, cuts" made every Scoreboard filter
    /// that picked the run decline. Prose answers; an exact prototype name still declines.
    #[tokio::test]
    async fn a_run_whose_prose_holds_a_prototype_word_is_answered() {
        let root = tempfile::tempdir().expect("reports");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        let prose = BENCH.replace("\"Task bench\"", "\"FIX8: C8-B1 constructor, cuts\"");
        let exact = BENCH
            .replace("vm.lead--bench", "vm.lead--exact")
            .replace("\"Task bench\"", "\"constructor\"");
        std::fs::write(dir.join("vm.lead--bench.json"), prose).expect("run file");
        std::fs::write(dir.join("vm.lead--exact.json"), exact).expect("run file");
        let owner = owner(root.path().to_path_buf(), None);
        owner.catalog_pass(1_790_000_000_000);
        let held = owner.held(Some(1_790_000_000_000)).await.expect("held");
        let catalog = owner
            .catalog_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        assert!(!catalog.is_null(), "catalog");
        let prepared = owner.held_docs(&held, &catalog, "test-catalog");
        let mut seen = 0;
        for (index, entry) in held.rows.iter().enumerate() {
            let row = entry.state_row();
            match entry.id.as_str() {
                "vm.lead--bench" => {
                    seen += 1;
                    assert!(
                        aac_status::read_context::proto_named(&row),
                        "word rule sees it"
                    );
                    assert!(!prepared.proto[index], "prose run must be answered");
                }
                "vm.lead--exact" => {
                    seen += 1;
                    assert!(prepared.proto[index], "an exact name still declines");
                }
                _ => {}
            }
        }
        assert_eq!(seen, 2, "{} rows", held.rows.len());
    }

    #[tokio::test]
    async fn a_held_store_lock_still_returns_the_share_run() {
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let ccs = tempfile::tempdir().expect("ccs");
        let store_dir = ccs.path().join("store");
        std::fs::create_dir_all(&store_dir).expect("store dir");
        let holder = StoreHost::new(boot(store_dir.clone()));
        holder.open_at_boot();
        let locked = StoreHost::new(boot(store_dir));
        locked.open_at_boot();
        let owner = owner(root.path().to_path_buf(), Some(locked));
        owner.catalog_pass(1_790_000_000_000);
        let held = owner.held(Some(1_790_000_000_000)).await.expect("held");
        assert!(held.share.is_some(), "share fallback after lock_held");
        assert!(
            has_bench(&held),
            "missing vm.lead--bench ({} rows)",
            held.rows.len()
        );
        let picked: Vec<usize> = (0..held.rows.len()).collect();
        let state = owner
            .state_of(&held, &picked, 1_790_000_000_000)
            .await
            .expect("state");
        assert_eq!(state["state"], "ok");
        assert!(state["scannedAt"].as_u64().is_some());
        assert!(owner.share_fallback.load(Ordering::SeqCst));
    }

    const LARGE_NOW: &str = "2026-11-05T12:00:00Z";

    fn large_now() -> u64 {
        u64::try_from(aac_store::run_schema::iso_ms(LARGE_NOW).expect("now")).expect("now ms")
    }

    /// `count` held rows with per-run unique words and instants (the live set is 8,915 run
    /// files): enough distinct strings and dates that the presentation facts pass 4 MiB.
    fn large_rows(count: usize, now: u64) -> Vec<(u64, serde_json::Value)> {
        let base: serde_json::Value = serde_json::from_str(BENCH).expect("bench doc");
        (0..count)
            .map(|i| {
                let mut doc = base.clone();
                let ended = format!(
                    "2026-11-04T{:02}:{:02}:{:02}Z",
                    (i / 3600) % 24,
                    (i / 60) % 60,
                    i % 60
                );
                doc["id"] = json!(format!("vm.lead--bench{i}"));
                doc["task"]["id"] = json!(format!("bench{i}"));
                doc["task"]["title"] = json!(format!("Run {i} {}", "x".repeat(120)));
                doc["project"] = json!(format!("project-{i}-{}", "p".repeat(60)));
                doc["writtenAt"] = json!(ended);
                doc["endedAt"] = json!(ended);
                let at = super::ended_at_ms(&doc["endedAt"]);
                (
                    at,
                    json!({
                        "file": format!("writers/vm.lead/runs/2026-11-04/vm.lead--bench{i}.json"),
                        "doc": doc,
                        "firstSeenAt": now,
                        "sha256": "ab".repeat(32),
                    }),
                )
            })
            .collect()
    }

    async fn large_owner(count: usize, now: u64) -> Arc<ScoreboardOwned> {
        let root = tempfile::tempdir().expect("reports");
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        let held = Arc::new(HeldRuns::new(
            (0, String::new()),
            large_rows(count, now),
            Vec::new(),
            None,
        ));
        *owner.held_runs.lock().unwrap_or_else(|p| p.into_inner()) = Some(held);
        // The tempdir must outlive the owner (its catalog reads the root): leak it into the
        // test process, as the other owner tests keep theirs alive in scope.
        std::mem::forget(root);
        owner
    }

    /// The front door's facts body over the owner's inputs, byte for byte as
    /// `native_group::scoreboard_facts` builds it.
    async fn front_door_facts(owner: &ScoreboardOwned, now: u64, facts_id: &str) -> String {
        let input = owner.inputs().await.expect("inputs");
        let catalog = input["catalog"].clone();
        let task_types: Vec<serde_json::Value> = catalog["taskTypes"]
            .as_array()
            .map(|types| {
                types
                    .iter()
                    .map(|kind| json!({"id": kind["id"], "label": kind["label"]}))
                    .collect()
            })
            .unwrap_or_default();
        let share_only = input["shareOnly"].as_bool() == Some(true);
        let data = json!({
            "now": now,
            "timeZone": "UTC",
            "root": "",
            "catalog": catalog,
            "runs": input["runs"],
            "joinReviews": [],
            "seedFailures": input["seedFailures"],
            "docs": input["docs"],
            "finish": {
                "filters": {
                    "from": serde_json::Value::Null,
                    "to": serde_json::Value::Null,
                    "source": "all",
                    "taskType": serde_json::Value::Null,
                    "available": !share_only,
                    "taskTypes": task_types,
                },
                "identity": if share_only {
                    serde_json::Value::Null
                } else {
                    input["identity"].clone()
                },
                "history": {
                    "store": if share_only { "off" } else { "on" },
                    "runs": serde_json::Value::Null,
                    "importing": false
                },
            },
        });
        let offsets = crate::zone_facts::server_zone_facts("UTC", i64::try_from(now).expect("now"))
            .expect("zone");
        let facts = aac_status::produce::facts::Facts::of(&[&data], Some(&offsets.zone));
        let facts_text = crate::status_collate::complete_order_of(&[&data], &facts.to_text())
            .unwrap_or_else(|| facts.to_text());
        format!(
            "{{\"generation\":{},\"data\":{},\"facts\":{}}}",
            serde_json::Value::String(facts_id.to_string()),
            json!({"timeZone": "UTC"}),
            facts_text
        )
    }

    /// A feed in one zone naming generation 0 and (optionally) a facts generation.
    fn feed_text_zone(now: u64, facts_id: Option<&str>, zone: &str) -> String {
        let offsets = crate::zone_facts::server_zone_facts(zone, i64::try_from(now).expect("now"))
            .expect("zone");
        let (clock, day) = offsets
            .zone
            .words(i64::try_from(now).expect("now"))
            .expect("words");
        let mut feed = json!({
            "now": now,
            "clock": clock,
            "day": day,
            "timeZone": zone,
            "from": serde_json::Value::Null,
            "to": serde_json::Value::Null,
            "source": "all",
            "taskType": serde_json::Value::Null,
            "fromMs": serde_json::Value::Null,
            "toMs": serde_json::Value::Null,
            "generation": 0,
            "catalogHash": "",
        });
        if let Some(facts_id) = facts_id {
            feed["facts"] = json!(facts_id);
        }
        feed.to_string()
    }

    /// A feed naming no snapshot at all: the service answers for the snapshot it holds.
    fn feed_text_bare(now: u64, zone: &str) -> String {
        let offsets = crate::zone_facts::server_zone_facts(zone, i64::try_from(now).expect("now"))
            .expect("zone");
        let (clock, day) = offsets
            .zone
            .words(i64::try_from(now).expect("now"))
            .expect("words");
        json!({
            "now": now,
            "clock": clock,
            "day": day,
            "timeZone": zone,
            "from": serde_json::Value::Null,
            "to": serde_json::Value::Null,
            "source": "all",
            "taskType": serde_json::Value::Null,
            "fromMs": serde_json::Value::Null,
            "toMs": serde_json::Value::Null,
        })
        .to_string()
    }

    /// The held snapshot once the share scan kicked at `at` has landed.
    async fn held_after_scan(owner: &ScoreboardOwned, at: u64) -> Arc<HeldRuns> {
        for _ in 0..1_000 {
            let held = owner.held(Some(at)).await.expect("held");
            if owner.share_held_at.load(Ordering::SeqCst) >= at
                && !owner.share_scan_running.load(Ordering::SeqCst)
            {
                return held;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("share scan at {at} did not land");
    }

    async fn wait_composing_empty(owner: &ScoreboardOwned) {
        // A debug compose over thousands of runs takes tens of seconds; a leaked slot
        // never empties at all, so the budget only has to outlast a real compose.
        for _ in 0..12_000 {
            if owner
                .composing
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty()
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!(
            "compose slot leaked: {:?}",
            owner.composing.lock().unwrap_or_else(|p| p.into_inner())
        );
    }

    fn catalog_detail(bytes: &[u8]) -> String {
        let view: serde_json::Value = serde_json::from_slice(bytes).expect("view");
        view["sources"]
            .as_array()
            .expect("sources")
            .iter()
            .find(|source| source["id"].as_str() == Some("scoreboard-catalog"))
            .and_then(|source| source["detail"].as_str())
            .expect("catalog detail")
            .to_string()
    }

    fn feed_text(now: u64, facts_id: &str) -> String {
        let offsets = crate::zone_facts::server_zone_facts("UTC", i64::try_from(now).expect("now"))
            .expect("zone");
        let (clock, day) = offsets
            .zone
            .words(i64::try_from(now).expect("now"))
            .expect("words");
        json!({
            "now": now,
            "clock": clock,
            "day": day,
            "timeZone": "UTC",
            "from": serde_json::Value::Null,
            "to": serde_json::Value::Null,
            "source": "all",
            "taskType": serde_json::Value::Null,
            "fromMs": serde_json::Value::Null,
            "toMs": serde_json::Value::Null,
            "generation": 0,
            "catalogHash": "",
            "facts": facts_id,
        })
        .to_string()
    }

    /// Live Nov 23: the facts over a real-size run set pass the 4 MiB publish cap, so every
    /// Scoreboard request declined. The owner must take the facts its own inputs produce.
    #[tokio::test]
    async fn facts_over_a_large_run_set_are_published_and_answered() {
        let now = large_now();
        let owner = large_owner(9_000, now).await;
        let body = front_door_facts(&owner, now, "0::large").await;
        assert!(
            body.len() > 4 * 1024 * 1024,
            "the fixture must pass the old cap ({} bytes)",
            body.len()
        );
        let published = owner.feeds.put("scoreboard-facts", body.as_bytes());
        assert!(published.is_ok(), "feeds.put: {published:?}");
        let started = std::time::Instant::now();
        let (bytes, _) = owner
            .answer(&feed_text(now, "0::large"))
            .await
            .expect("first answer");
        let first = started.elapsed();
        assert!(!bytes.is_empty());
        let started = std::time::Instant::now();
        let (again, _) = owner
            .answer(&feed_text(now + 5_000, "0::large"))
            .await
            .expect("second answer");
        let second = started.elapsed();
        let view: serde_json::Value = serde_json::from_slice(&again).expect("view");
        assert_eq!(view["cacheAgeSeconds"].as_f64(), Some(5.0));
        assert!(
            second < std::time::Duration::from_millis(2_000) && second < first,
            "second request must be the fast cache hit (first {first:?}, second {second:?})"
        );
    }

    /// The service keeps the facts itself: an answer whose facts generation was never pushed
    /// is composed from facts the owner computes over its held snapshot, not declined.
    #[tokio::test]
    async fn an_answer_without_pushed_facts_is_composed_in_the_service() {
        let now = large_now();
        let owner = large_owner(9_000, now).await;
        let started = std::time::Instant::now();
        let answer = owner.answer(&feed_text(now, "0::owner-computed")).await;
        let first = started.elapsed();
        let (bytes, _) = answer.expect("owner-computed facts answer");
        assert!(!bytes.is_empty());
        let started = std::time::Instant::now();
        owner
            .answer(&feed_text(now + 5_000, "0::owner-computed"))
            .await
            .expect("second answer");
        let second = started.elapsed();
        assert!(
            second < std::time::Duration::from_millis(2_000) && second < first,
            "second request must be fast (first {first:?}, second {second:?})"
        );
    }

    /// A share rescan rereads only what changed: an unchanged file keeps its first-seen
    /// stamp (its parsed row is reused), a rewritten file is read again.
    #[tokio::test]
    async fn a_share_rescan_rereads_only_changed_files() {
        let root = tempfile::tempdir().expect("reports");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        for (name, title) in [("a", "Task a"), ("b", "Task b"), ("c", "Task c")] {
            let text = BENCH
                .replace("vm.lead--bench", &format!("vm.lead--{name}"))
                .replace("\"Task bench\"", &format!("\"{title}\""));
            std::fs::write(dir.join(format!("vm.lead--{name}.json")), text).expect("run file");
        }
        let owner = owner(root.path().to_path_buf(), None);
        owner.catalog_pass(1_790_000_000_000);
        let t1 = 1_790_000_000_000u64;
        let first_seen = |held: &HeldRuns, id: &str| {
            held.rows
                .iter()
                .find(|entry| entry.id == id)
                .map(|entry| entry.first_seen_at)
        };
        let held = owner.held(Some(t1)).await.expect("first scan");
        assert_eq!(first_seen(&held, "vm.lead--a"), Some(t1));
        assert_eq!(first_seen(&held, "vm.lead--b"), Some(t1));
        // Past the 60 s share reuse window the owner rescans: unchanged files keep t1.
        let t2 = t1 + 61_000;
        let held = held_after_scan(&owner, t2).await;
        assert_eq!(
            first_seen(&held, "vm.lead--a"),
            Some(t1),
            "an unchanged file is not reread"
        );
        assert_eq!(first_seen(&held, "vm.lead--b"), Some(t1));
        // Rewriting one file (a longer title changes its print) rereads only that file.
        let changed = BENCH
            .replace("vm.lead--bench", "vm.lead--b")
            .replace("\"Task bench\"", "\"Task b, rewritten longer\"");
        std::fs::write(dir.join("vm.lead--b.json"), changed).expect("rewrite");
        let t3 = t2 + 61_000;
        let held = held_after_scan(&owner, t3).await;
        assert_eq!(first_seen(&held, "vm.lead--a"), Some(t1));
        assert_eq!(
            first_seen(&held, "vm.lead--b"),
            Some(t3),
            "the changed file is reread"
        );
        let title = held
            .rows
            .iter()
            .find(|entry| entry.id == "vm.lead--b")
            .map(|entry| {
                entry.doc()["task"]["title"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            });
        assert_eq!(title.as_deref(), Some("Task b, rewritten longer"));
    }

    /// While a compose runs, the last good view answers instead of a `busy` decline.
    #[tokio::test]
    async fn a_stale_view_answers_while_its_refresh_runs() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        let body = front_door_facts(&owner, now, "0::swr").await;
        owner
            .feeds
            .put("scoreboard-facts", body.as_bytes())
            .expect("publish");
        owner
            .answer(&feed_text(now, "0::swr"))
            .await
            .expect("first answer");
        // A refresh for this view is in flight (the composing set holds its key).
        let key = owner
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .next()
            .expect("view key")
            .clone();
        owner
            .composing
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key);
        let later = now + 120_000;
        let body = front_door_facts(&owner, later, "0::swr2").await;
        owner
            .feeds
            .put("scoreboard-facts", body.as_bytes())
            .expect("publish again");
        let answer = owner.answer(&feed_text(later, "0::swr2")).await;
        let (bytes, _) = answer.expect("stale view while the refresh runs");
        let view: serde_json::Value = serde_json::from_slice(&bytes).expect("view");
        assert_eq!(view["cacheAgeSeconds"].as_f64(), Some(120.0));
    }

    /// H2: the 30 s catalog pass advances the catalog's scan stamp; the next compose must
    /// recompute facts over that catalog (not serve the old scan's presentation into a
    /// `Stale`), and a pushed facts generation wins over anything computed here.
    #[tokio::test]
    async fn a_catalog_rescan_recomputes_facts_and_pushed_generations_win() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        owner
            .answer(&feed_text(now, "0::t0"))
            .await
            .expect("first answer");
        owner.catalog_pass(now + 30_000);
        owner
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        owner
            .answer(&feed_text(now + 35_000, "0::t1"))
            .await
            .expect("answer after the catalog rescan");
        // A pushed generation for the current snapshot is honored, not shadowed.
        front_door_facts(&owner, now + 35_000, "0::warm").await;
        for _ in 0..300 {
            if !owner.share_scan_running.load(Ordering::SeqCst) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let body = front_door_facts(&owner, now + 35_000, "0::pushed-t1").await;
        owner
            .feeds
            .put("scoreboard-facts", body.as_bytes())
            .expect("publish pushed");
        owner
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        owner
            .answer(&feed_text(now + 35_000, "0::pushed-t1"))
            .await
            .expect("pushed generation answer");
    }

    /// H3: after the 60 s share rescan picks up a new run, the background refresh
    /// recomputes facts for the new snapshot instead of reusing the boot snapshot's
    /// presentation under the old facts id (which declined `Stale` forever).
    #[tokio::test]
    async fn a_share_refresh_after_rescan_recomputes_facts() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        owner
            .answer(&feed_text(now, "0::t0"))
            .await
            .expect("first answer");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        let new_run = BENCH
            .replace("vm.lead--bench", "vm.lead--new-tie")
            .replace("\"Task bench\"", "\"Task newtie quixotic\"");
        std::fs::write(dir.join("vm.lead--new-tie.json"), new_run).expect("new run");
        owner
            .answer_inner(&feed_text(now + 65_000, "0::t0"), true)
            .await
            .expect("refresh after the rescan window");
        let held = held_after_scan(&owner, now + 65_000).await;
        assert!(
            held.rows.iter().any(|entry| entry.id == "vm.lead--new-tie"),
            "the new run landed ({} rows)",
            held.rows.len()
        );
        owner
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        let (bytes, _) = owner
            .answer(&feed_text(now + 66_000, "0::t0"))
            .await
            .expect("answer over the rescanned snapshot");
        assert!(
            String::from_utf8_lossy(&bytes).contains("quixotic"),
            "the new run is in the view"
        );
    }

    /// H4: a refresh that returns before composing (a fresh view, a moved generation)
    /// still releases the compose slot `spawn_refresh` took for it.
    #[tokio::test]
    async fn a_refresh_early_return_releases_its_compose_slot() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        owner
            .answer(&feed_text(now, "0::slot"))
            .await
            .expect("first answer");
        let key = owner
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .keys()
            .next()
            .expect("view key")
            .clone();
        // Fresh-view early return (1 s later, inside the 60 s cache).
        owner.spawn_refresh(&feed_text(now + 1_000, "0::slot"), &key);
        wait_composing_empty(&owner).await;
        // Generation-mismatch early return.
        let stale_feed =
            feed_text(now + 120_000, "0::slot").replace("\"generation\":0", "\"generation\":999");
        owner.spawn_refresh(&stale_feed, &key);
        wait_composing_empty(&owner).await;
    }

    /// M1: a view served stale says so: refreshing, advanced source ages, stale state.
    #[tokio::test]
    async fn a_stale_view_says_it_is_stale_and_refreshing() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        owner
            .answer(&feed_text(now, "0::stale-view"))
            .await
            .expect("first answer");
        let (bytes, _) = owner
            .answer(&feed_text(now + 300_000, "0::stale-view"))
            .await
            .expect("stale answer");
        let view: serde_json::Value = serde_json::from_slice(&bytes).expect("view");
        assert_eq!(view["cacheAgeSeconds"].as_f64(), Some(300.0));
        assert_eq!(view["refreshing"], json!(true), "view: {view}");
        let runs = view["sources"]
            .as_array()
            .expect("sources")
            .iter()
            .find(|source| source["id"].as_str() == Some("scoreboard-runs"))
            .expect("runs source")
            .clone();
        assert!(
            runs["ageSeconds"].as_f64().unwrap_or(0.0) >= 299.0,
            "runs source age: {runs}"
        );
        assert_eq!(runs["state"], json!("stale"), "runs source: {runs}");
    }

    /// M2: after a zone change, the same facts id must not serve the old zone's words.
    #[tokio::test]
    async fn a_zone_change_formats_in_the_new_zone() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        let (bytes, _) = owner
            .answer(&feed_text_zone(
                now + 5_000,
                Some("0::same"),
                "America/New_York",
            ))
            .await
            .expect("New York answer");
        assert!(
            catalog_detail(&bytes).contains("7:00 AM"),
            "NY detail: {}",
            catalog_detail(&bytes)
        );
        owner
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        let (bytes, _) = owner
            .answer(&feed_text_zone(
                now + 5_000,
                Some("0::same"),
                "America/Los_Angeles",
            ))
            .await
            .expect("Los Angeles answer");
        assert!(
            catalog_detail(&bytes).contains("4:00 AM"),
            "LA detail: {}",
            catalog_detail(&bytes)
        );
    }

    /// H5: dropping a `share_held` call mid-scan (the route's budget timeout) must not
    /// wipe the per-file cache: the scan runs detached and still lands its snapshot.
    #[tokio::test]
    async fn dropping_a_share_held_call_keeps_the_cache_and_scan() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.catalog_pass(now);
        owner
            .share_cache
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(
                "writers/vm.lead/runs/2026-11-04/vm.lead--bench.json".to_string(),
                super::CachedRun {
                    print: "stale-print".to_string(),
                    outcome: super::CachedOutcome::Problem {
                        reason: "earlier".to_string(),
                    },
                },
            );
        let mut fut = Box::pin(owner.share_held(Some(now)));
        let waker = futures_util::task::noop_waker();
        let mut cx = std::task::Context::from_waker(&waker);
        let _ = std::future::Future::poll(fut.as_mut(), &mut cx);
        drop(fut);
        assert!(
            !owner
                .share_cache
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .is_empty(),
            "a timeout drop wiped share_cache"
        );
        let held = held_after_scan(&owner, now).await;
        assert!(has_bench(&held), "the detached scan landed");
    }

    /// H5: two cold-boot share reads at once share one scan and one snapshot.
    #[tokio::test]
    async fn concurrent_cold_share_reads_share_one_scan() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        for i in 0..24 {
            let text = BENCH.replace("vm.lead--bench", &format!("vm.lead--bench{i}"));
            std::fs::write(dir.join(format!("vm.lead--bench{i}.json")), text).expect("run file");
        }
        let owner = owner(root.path().to_path_buf(), None);
        owner.catalog_pass(now);
        let (first, second) =
            tokio::join!(owner.share_held(Some(now)), owner.share_held(Some(now)));
        let first = first.expect("first cold read");
        let second = second.expect("second cold read");
        assert!(
            Arc::ptr_eq(&first, &second),
            "cold callers must land on one snapshot"
        );
        assert_eq!(first.rows.len(), 24);
    }

    /// M3: a half-written run file is refused for this scan but not cached as refused,
    /// and removing `v1/writers` empties the per-file cache with it.
    #[test]
    fn half_written_runs_are_not_cached_and_removed_writers_leave_the_cache() {
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::write(
            dir.join("vm.lead--half.json"),
            "{\"schema\":\"agent-reports/run@1\",",
        )
        .expect("half file");
        let mut cache: std::collections::HashMap<String, super::CachedRun> = Default::default();
        let scan = scan_share_runs_cached(root.path(), 1_790_000_000_000, &mut cache);
        assert!(
            scan.problems.iter().any(|problem| problem["file"]
                .as_str()
                .unwrap_or_default()
                .contains("half")),
            "the half file is refused: {:?}",
            scan.problems
        );
        assert!(
            !cache.contains_key("writers/vm.lead/runs/2026-11-04/vm.lead--half.json"),
            "half-written file cached: {:?}",
            cache.keys().collect::<Vec<_>>()
        );
        assert!(
            cache.contains_key("writers/vm.lead/runs/2026-11-04/vm.lead--bench.json"),
            "the good file is cached"
        );
        std::fs::remove_dir_all(root.path().join("v1/writers")).expect("remove writers");
        let scan = scan_share_runs_cached(root.path(), 1_790_000_060_000, &mut cache);
        assert_eq!(scan.read.state, "missing");
        assert!(
            cache.is_empty(),
            "removed writers leave the cache: {:?}",
            cache.keys().collect::<Vec<_>>()
        );
    }

    /// H1 at real size: 9,000 run files whose inputs body passes the front door's 16 MiB
    /// reply cap. The answer names no snapshot (the service resolves its own), so the
    /// page answers without the inputs body ever crossing to the front door.
    /// One real-size run file's text (the size and memory fixtures share it).
    fn real_size_run(base: &serde_json::Value, i: usize) -> String {
        let mut doc = base.clone();
        let ended = format!(
            "2026-11-04T{:02}:{:02}:{:02}Z",
            (i / 3600) % 24,
            (i / 60) % 60,
            i % 60
        );
        doc["id"] = json!(format!("vm.lead--bench{i}"));
        doc["task"]["id"] = json!(format!("bench{i}-{}", "t".repeat(140)));
        doc["task"]["title"] = json!(format!("Run {i} {}", "x".repeat(280)));
        doc["benchmark"] = json!(format!("pilot-{}", "b".repeat(104)));
        doc["writtenAt"] = json!(ended);
        doc["endedAt"] = json!(ended);
        doc.to_string()
    }

    /// 9,000 real run files (inputs past the 16 MiB reply cap); returns their folder.
    fn write_real_size_share(root: &Path) -> PathBuf {
        let dir = root.join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        let base: serde_json::Value = serde_json::from_str(BENCH).expect("bench doc");
        for i in 0..9_000 {
            std::fs::write(
                dir.join(format!("vm.lead--bench{i}.json")),
                real_size_run(&base, i),
            )
            .expect("run file");
        }
        dir
    }

    #[tokio::test]
    async fn a_real_size_share_answers_without_the_inputs_body() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_real_size_share(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        let inputs = owner.inputs().await.expect("inputs");
        let inputs_len = serde_json::to_vec(&inputs).expect("inputs bytes").len();
        assert!(
            inputs_len > 16 * 1024 * 1024,
            "the fixture's inputs must pass the 16 MiB reply cap ({inputs_len} bytes)"
        );
        let started = std::time::Instant::now();
        let (bytes, _) = owner
            .answer(&feed_text_bare(now, "UTC"))
            .await
            .expect("the page answers at real size");
        let first = started.elapsed();
        assert!(!bytes.is_empty());
        let started = std::time::Instant::now();
        owner
            .answer(&feed_text_bare(now + 5_000, "UTC"))
            .await
            .expect("second answer");
        let second = started.elapsed();
        assert!(
            second < std::time::Duration::from_millis(2_000) && second < first,
            "second request must be fast (first {first:?}, second {second:?})"
        );
    }

    /// Memory plan item 3: a store read shares the store's resident resolved history
    /// by `Arc` (no `runsQuery` text round-trip, no second parsed copy here).
    #[tokio::test]
    async fn a_store_snapshot_is_shared_not_reparsed() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let ccs = tempfile::tempdir().expect("ccs");
        let store_dir = ccs.path().join("store");
        std::fs::create_dir_all(&store_dir).expect("store dir");
        let store = StoreHost::new(boot(store_dir));
        store.open_at_boot();
        let owner = owner(root.path().to_path_buf(), Some(Arc::clone(&store)));
        owner.seed().await;
        owner.catalog_pass(now);
        owner.ingest_pass(false).await;
        let held = owner.held(Some(now)).await.expect("store held");
        assert!(
            held.share.is_none(),
            "store snapshot, not the share fallback"
        );
        assert!(has_bench(&held), "the ingested run is held");
        let history = held
            .store_history
            .as_ref()
            .expect("held from the store snapshot");
        let again = store.runs_history().await.expect("snapshot again");
        assert!(
            Arc::ptr_eq(history, &again),
            "the same resident snapshot Arc is shared"
        );
        owner
            .answer(&feed_text_bare(now, "UTC"))
            .await
            .expect("answer over the store snapshot");
    }

    /// M1 (round 3): a cold share waiter whose scan never lands (failed or panicked
    /// detached task) ends in an honest decline inside the bound instead of waiting
    /// forever. The paused clock makes the bound fire without real waiting.
    #[tokio::test(start_paused = true)]
    async fn a_cold_share_wait_ends_instead_of_wedging() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.catalog_pass(now);
        // A scan that claims to be running but will never finish or notify.
        owner.share_scan_running.store(true, Ordering::SeqCst);
        let waited = tokio::time::timeout(
            std::time::Duration::from_secs(600),
            owner.share_held(Some(now)),
        )
        .await;
        match waited {
            Ok(Err(reason)) => {
                assert_eq!(reason, aac_views::decline::Decline::Deadline);
            }
            Ok(Ok(_)) => panic!("a cold waiter with no scan must not answer"),
            Err(_) => panic!("a cold waiter wedged past every bound"),
        }
    }

    /// M2 (round 3): after the 60 s rescan lands a new run, the next request (no manual
    /// view clearing) serves the new snapshot's view — never the pre-rescan view as fresh.
    #[tokio::test]
    async fn a_rescan_landing_replaces_the_view_without_a_clear() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        owner
            .answer(&feed_text(now, "0::m2"))
            .await
            .expect("first answer");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        let new_run = BENCH
            .replace("vm.lead--bench", "vm.lead--new-tie")
            .replace("\"Task bench\"", "\"Task newtie quixotic\"");
        std::fs::write(dir.join("vm.lead--new-tie.json"), new_run).expect("new run");
        // Hold the rescan "in flight" by hand so the refresh provably starts before it
        // lands, without needing thousands of files to slow the real scan down. The
        // finisher performs the actual scan after a delay, then ends it the way the
        // detached task does (snapshot held, flag cleared, waiters woken).
        owner.share_scan_running.store(true, Ordering::SeqCst);
        let finisher = Arc::clone(&owner);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            finisher.run_share_scan(now + 65_000).await;
            finisher.share_scan_running.store(false, Ordering::SeqCst);
            finisher.share_scan_done.notify_waiters();
        });
        // The stale-while-revalidate request: old view now, refresh behind it.
        owner
            .answer(&feed_text(now + 65_000, "0::m2"))
            .await
            .expect("stale answer");
        held_after_scan(&owner, now + 65_000).await;
        wait_composing_empty(&owner).await;
        let (bytes, _) = owner
            .answer(&feed_text(now + 70_000, "0::m2"))
            .await
            .expect("answer after the rescan landed");
        assert!(
            String::from_utf8_lossy(&bytes).contains("quixotic"),
            "the rescanned run must be in the served view"
        );
    }

    /// L2 (round 3): a source whose age is null (no scan yet) keeps its null when a
    /// stale serve advances source ages — no fabricated age.
    #[test]
    fn a_stale_serve_keeps_a_null_source_age_null() {
        let view = json!({
            "cacheAgeSeconds": 0,
            "refreshing": false,
            "sources": [
                {"id": "scoreboard-runs", "ageSeconds": serde_json::Value::Null, "staleAfterSeconds": 180, "state": "missing"},
                {"id": "scoreboard-catalog", "ageSeconds": 10, "staleAfterSeconds": 180, "state": "ok"}
            ]
        });
        let bytes = serde_json::to_vec(&view).expect("view bytes");
        let cached = super::CachedView {
            at: 1_000_000,
            seq: 1,
            age_value_offset: super::find_age_zero_offset(&bytes),
            bytes,
            etag: "W/\"x\"".to_string(),
        };
        let (bytes, _) = super::cached_bytes(&cached, 1_300_000, true);
        let served: serde_json::Value = serde_json::from_slice(&bytes).expect("view");
        assert!(
            served["sources"][0]["ageSeconds"].is_null(),
            "served: {served}"
        );
        assert_eq!(served["sources"][0]["state"], json!("missing"));
        assert_eq!(served["sources"][1]["ageSeconds"].as_f64(), Some(310.0));
        assert_eq!(served["sources"][1]["state"], json!("stale"));
    }

    /// L4 (round 3): an explicit null snapshot name is a shape error, not "unnamed".
    #[tokio::test]
    async fn an_explicit_null_generation_is_a_shape_decline() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let owner = owner(root.path().to_path_buf(), None);
        owner.catalog_pass(now);
        let feed = feed_text(now, "0::null").replace("\"generation\":0", "\"generation\":null");
        let answer = owner.answer(&feed).await;
        assert_eq!(
            answer.err(),
            Some(aac_views::decline::Decline::Shape),
            "explicit null generation must decline Shape"
        );
    }

    /// Memory-plan item 5: remembered safe paths are a bounded FIFO, not a map that
    /// grows with every distinct path ever ingested.
    #[test]
    fn safe_paths_are_capped_fifo() {
        let root = tempfile::tempdir().expect("reports");
        let owner = owner(root.path().to_path_buf(), None);
        let files: Vec<serde_json::Value> = (0..5_000)
            .map(|i| {
                json!({
                    "path": format!("writers/w/runs/2026-11-04/run{i}.json"),
                    "text": "{}",
                    "size": 2,
                    "mtimeMs": 1,
                })
            })
            .collect();
        let scan = json!({
            "files": files,
            "refused": [],
            "threads": {},
            "catalog": {"models": serde_json::Value::Null, "aliases": serde_json::Value::Null},
            "problems": [],
            "more": false,
        });
        let mut safe_paths = owner.safe_paths.lock().unwrap_or_else(|p| p.into_inner());
        let _ = super::ingest_request(&scan, 1, &mut safe_paths);
        assert!(
            safe_paths.len() <= 4_096,
            "safe_paths must be capped, got {}",
            safe_paths.len()
        );
        assert!(
            safe_paths.values().any(|p| p.ends_with("/run4999.json")),
            "the newest path is kept"
        );
        assert!(
            !safe_paths.values().any(|p| p.ends_with("/run0.json")),
            "the oldest path is evicted"
        );
    }

    /// VmRSS/VmHWM in MiB from /proc/self/status (0 off Linux).
    fn rss_mib() -> (f64, f64) {
        let text = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let field = |name: &str| {
            text.lines()
                .find(|line| line.starts_with(name))
                .and_then(|line| line.split_whitespace().nth(1))
                .and_then(|kib| kib.parse::<f64>().ok())
                .map(|kib| kib / 1024.0)
                .unwrap_or(0.0)
        };
        (field("VmRSS:"), field("VmHWM:"))
    }

    /// M3 measurement (not a gate): RSS over cold answer, 20 warm hits, 5 rescans and one
    /// inputs() call on the 9,000-file fixture. Run in release, one test:
    /// `cargo test --release -p aac-http --lib memory_over_time -- --ignored --nocapture`.
    #[tokio::test]
    #[ignore = "memory measurement: run explicitly in release"]
    async fn memory_over_time_9000_files() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        let (rss, hwm) = rss_mib();
        eprintln!("rss start {rss:.1} MiB (hwm {hwm:.1})");
        let dir = write_real_size_share(root.path());
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        let (rss, hwm) = rss_mib();
        eprintln!("rss files-written {rss:.1} MiB (hwm {hwm:.1})");
        let started = std::time::Instant::now();
        owner
            .answer(&feed_text_bare(now, "UTC"))
            .await
            .expect("cold answer");
        let (rss, hwm) = rss_mib();
        eprintln!(
            "rss cold {:?} {rss:.1} MiB (hwm {hwm:.1})",
            started.elapsed()
        );
        let started = std::time::Instant::now();
        for step in 1..=20u64 {
            owner
                .answer(&feed_text_bare(now + step * 1_000, "UTC"))
                .await
                .expect("warm answer");
        }
        let (rss, hwm) = rss_mib();
        eprintln!(
            "rss warm-20 {:?} {rss:.1} MiB (hwm {hwm:.1})",
            started.elapsed()
        );
        let base: serde_json::Value = serde_json::from_str(BENCH).expect("bench doc");
        let mut clock = now;
        for round in 1..=5usize {
            for k in 0..50usize {
                let i = (round * 100 + k) % 9_000;
                let mut text = real_size_run(&base, i);
                text.push(' ');
                std::fs::write(dir.join(format!("vm.lead--bench{i}.json")), text).expect("modify");
            }
            for k in 0..10usize {
                let i = 9_000 + round * 10 + k;
                std::fs::write(
                    dir.join(format!("vm.lead--bench{i}.json")),
                    real_size_run(&base, i),
                )
                .expect("add");
            }
            for k in 0..10usize {
                let i = round * 10 + k;
                let _ = std::fs::remove_file(dir.join(format!("vm.lead--bench{i}.json")));
            }
            clock += 65_000;
            owner.catalog_pass(clock);
            owner
                .answer(&feed_text_bare(clock, "UTC"))
                .await
                .expect("rescan answer");
            held_after_scan(&owner, clock).await;
            wait_composing_empty(&owner).await;
            owner
                .answer(&feed_text_bare(clock + 5_000, "UTC"))
                .await
                .expect("post-rescan answer");
            wait_composing_empty(&owner).await;
            let (rss, hwm) = rss_mib();
            eprintln!("rss rescan-{round} {rss:.1} MiB (hwm {hwm:.1})");
        }
        let inputs = owner.inputs().await.expect("inputs");
        let inputs_len = serde_json::to_vec(&inputs).expect("inputs bytes").len();
        drop(inputs);
        let (rss, hwm) = rss_mib();
        eprintln!("rss after-inputs ({inputs_len} bytes, dropped) {rss:.1} MiB (hwm {hwm:.1})");
        let held = owner.held(Some(clock)).await.expect("held");
        assert_eq!(held.rows.len(), 9_000);
    }

    /// Live Nov 24: `judged_rows` breaks ties on `(basis, rank, n)` by comparing synthesized
    /// row keys (`model|harness|effort`, and `...|process.id|process.version` when `byProcess`
    /// is true). Those keys must be ranked in the owner-computed presentation facts, or every
    /// real multi-model read declines `stale`.
    #[tokio::test]
    async fn tied_multi_model_and_by_process_rows_answer_without_stale_decline() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let base: serde_json::Value = serde_json::from_str(BENCH).expect("bench doc");
        let runs = [
            (
                "build-a",
                "build",
                "anthropic",
                "claude-opus-5-5",
                "high",
                None,
            ),
            (
                "build-b",
                "build",
                "anthropic",
                "claude-sonnet-5-5",
                "high",
                None,
            ),
            (
                "scout-a",
                "scout",
                "anthropic",
                "claude-opus-5-5",
                "high",
                Some(("proc-a", "1.0")),
            ),
            (
                "scout-b",
                "scout",
                "anthropic",
                "claude-opus-5-5",
                "high",
                Some(("proc-b", "2.0")),
            ),
        ];
        for (suffix, task_type, provider, model, effort, process) in runs {
            let mut doc = base.clone();
            doc["id"] = json!(format!("vm.lead--{suffix}"));
            doc["task"]["id"] = json!(suffix);
            doc["task"]["type"] = json!(task_type);
            doc["model"]["provider"] = json!(provider);
            doc["model"]["id"] = json!(model);
            doc["model"]["effort"] = json!(effort);
            if let Some((pid, pver)) = process {
                doc["process"] = json!({"id": pid, "version": pver});
            }
            std::fs::write(dir.join(format!("vm.lead--{suffix}.json")), doc.to_string())
                .expect("run file");
        }
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        let (bytes, _) = owner
            .answer(&feed_text_bare(now, "America/New_York"))
            .await
            .expect("multi-model and byProcess ties must answer 200");
        let view: serde_json::Value = serde_json::from_slice(&bytes).expect("view json");
        assert_eq!(view["version"], json!(1));
        let rows_count: usize = view["uses"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|u| u["taskTypes"].as_array().into_iter().flatten())
            .filter_map(|t| t["rows"].as_array().map(Vec::len))
            .sum();
        assert!(
            rows_count >= 4,
            "expected scored rows across build and scout, got {rows_count}"
        );
    }

    /// Live Nov 24: `runs_source_of` counts runs whose outcome is neither done nor a known
    /// failure word after collapsing whitespace and lowercasing, then sorts those words via
    /// `ctx.compare`. Owner-computed facts must rank the normalized unknown-outcome strings.
    #[tokio::test]
    async fn capitalized_unknown_outcomes_answer_without_stale_decline() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let base: serde_json::Value = serde_json::from_str(BENCH).expect("bench doc");
        for (suffix, outcome) in [
            ("unk-a", "Status routes in Rust, 30/30 both"),
            ("unk-b", "Analytics  in\tRust, 15/15 both"),
        ] {
            let mut doc = base.clone();
            doc["id"] = json!(format!("vm.lead--{suffix}"));
            doc["task"]["id"] = json!(suffix);
            doc["outcome"] = json!(outcome);
            std::fs::write(dir.join(format!("vm.lead--{suffix}.json")), doc.to_string())
                .expect("run file");
        }
        let owner = owner(root.path().to_path_buf(), None);
        owner.seed().await;
        owner.catalog_pass(now);
        let (bytes, _) = owner
            .answer(&feed_text_bare(now, "America/New_York"))
            .await
            .expect("capitalized unknown outcomes must answer 200");
        let view: serde_json::Value = serde_json::from_slice(&bytes).expect("view json");
        let detail = view["sources"]
            .as_array()
            .and_then(|sources| {
                sources
                    .iter()
                    .find(|s| s["id"].as_str() == Some("scoreboard-runs"))
            })
            .and_then(|s| s["detail"].as_str())
            .unwrap_or("");
        assert!(
            detail.contains("analytics in rust, 15/15 both")
                && detail.contains("status routes in rust, 30/30 both"),
            "detail: {detail}"
        );
    }

    /// A new runs-state problem arriving without a generation change introduces a new `since`
    /// timestamp that `runs_source_of` formats with `ctx.clock`; `FactsKey` must invalidate so
    /// cached facts do not miss that timestamp and decline `stale`.
    #[tokio::test]
    async fn new_runs_problem_since_invalidates_cached_facts() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let ccs = tempfile::tempdir().expect("ccs");
        let store_dir = ccs.path().join("store");
        std::fs::create_dir_all(&store_dir).expect("store dir");
        let store = StoreHost::new(boot(store_dir));
        store.open_at_boot();
        let owner = owner(root.path().to_path_buf(), Some(Arc::clone(&store)));
        owner.seed().await;
        owner.catalog_pass(now);
        owner.ingest_pass(false).await;
        owner
            .answer(&feed_text_bare(now, "America/New_York"))
            .await
            .expect("first answer");
        // An ingest pass problem appears with a new `since` timestamp without changing generation.
        owner
            .pass
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .problems
            .push(json!({
                "file": "writers/vm.lead/runs/2026-11-04/broken.json",
                "reason": "invalid json",
                "since": now + 15_000,
            }));
        owner
            .views
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clear();
        owner
            .answer(&feed_text_bare(now + 20_000, "America/New_York"))
            .await
            .expect("answer after new runs problem must recompute facts and succeed");
    }

    /// Live Nov 24: filtered reads carry `joinReviews` inside `data`, which `compose_fed_parts`
    /// checks with `proto_named`. A review run whose unused prose (`task.title`) contains a
    /// prototype word (such as `"constructor"`) must not cause filtered reads to decline `stale`.
    #[tokio::test]
    async fn filtered_read_with_review_prose_prototype_word_answers() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        std::fs::create_dir_all(root.path().join("v1/catalog")).expect("catalog folder");
        let ccs = tempfile::tempdir().expect("ccs");
        let store_dir = ccs.path().join("store");
        std::fs::create_dir_all(&store_dir).expect("store dir");
        let base: serde_json::Value = serde_json::from_str(BENCH).expect("bench doc");
        // One observed build run and one review run targeting it whose prose title holds
        // "constructor" (as in live ea-rev-toolbatch1-solhigh.json).
        let mut target = base.clone();
        target["id"] = json!("vm.lead--build-1");
        target["source"] = json!("observed");
        target["benchmark"] = serde_json::Value::Null;
        target["task"]["id"] = json!("build-1");
        target["task"]["type"] = json!("build");
        target["endedAt"] = json!("2026-11-04T16:30:00Z");
        std::fs::write(dir.join("vm.lead--build-1.json"), target.to_string()).expect("target");
        let mut review = base;
        review["id"] = json!("vm.lead--rev-1");
        review["source"] = json!("observed");
        review["benchmark"] = serde_json::Value::Null;
        review["task"]["id"] = json!("rev-1");
        review["task"]["type"] = json!("review");
        review["task"]["title"] = json!("C4-B1 Invariant constructor check");
        review["endedAt"] = json!("2026-11-04T16:35:00Z");
        review["review"] = json!({
            "of": "vm.lead--build-1",
            "verdict": "pass",
            "findings": {"high": 0, "medium": 0, "low": 0},
            "overturned": false,
            "disputed": false,
        });
        std::fs::write(dir.join("vm.lead--rev-1.json"), review.to_string()).expect("review");
        let store = StoreHost::new(boot(store_dir));
        store.open_at_boot();
        let owner = owner(root.path().to_path_buf(), Some(Arc::clone(&store)));
        owner.seed().await;
        owner.catalog_pass(now);
        owner.ingest_pass(false).await;
        let now_i64 = i64::try_from(now).expect("now");
        let offsets =
            crate::zone_facts::server_zone_facts("America/New_York", now_i64).expect("zone");
        let (clock, day) = offsets.zone.words(now_i64).expect("words");
        let feed = json!({
            "now": now,
            "clock": clock,
            "day": day,
            "timeZone": "America/New_York",
            "from": serde_json::Value::Null,
            "to": serde_json::Value::Null,
            "source": "real",
            "taskType": "build",
            "fromMs": serde_json::Value::Null,
            "toMs": serde_json::Value::Null,
        })
        .to_string();
        let (bytes, _) = owner
            .answer(&feed)
            .await
            .expect("filtered read with review prose proto word must answer 200");
        let view: serde_json::Value = serde_json::from_slice(&bytes).expect("view");
        assert_eq!(view["version"], json!(1));
        // Confirm the review actually joined onto the filtered build row.
        let build_reviews = view["uses"]
            .as_array()
            .into_iter()
            .flatten()
            .flat_map(|u| u["taskTypes"].as_array().into_iter().flatten())
            .filter(|t| t["id"].as_str() == Some("build"))
            .flat_map(|t| t["rows"].as_array().into_iter().flatten())
            .find_map(|r| r["observed"]["reviews"].as_u64());
        assert_eq!(build_reviews, Some(1), "review must join onto build row");
    }

    #[test]
    fn compose_decline_reason_names_each_failure_class() {
        let now = i64::try_from(large_now()).expect("now");
        let date = || {
            aac_status::read_context::Date::of_now(now, "12:00 PM".into(), "Nov 5".into())
                .expect("date")
        };
        let empty = Arc::new(aac_status::read_context::Presentation::empty());
        let base_data = json!({
            "now": now,
            "runs": {"correctionsResolved": true, "scannedAt": now, "problems": []},
            "catalog": {"scannedAt": now, "problems": []},
            "finish": {"native": false},
        });
        let mut unresolved = base_data.clone();
        unresolved["runs"]["correctionsResolved"] = json!(false);
        assert_eq!(
            super::compose_decline_reason(
                &unresolved,
                &[],
                &[],
                &[],
                None,
                empty.clone(),
                (now, date())
            ),
            "corrections-unresolved"
        );
        let mut proto = base_data.clone();
        proto["root"] = json!("constructor");
        assert_eq!(
            super::compose_decline_reason(
                &proto,
                &[],
                &[],
                &[],
                None,
                empty.clone(),
                (now, date())
            ),
            "proto-named"
        );
        let mut missing_prob_date = base_data.clone();
        missing_prob_date["runs"]["problems"] = json!([{"file": "writers/a/runs/2026-11-04/x.json", "reason": "bad", "since": now - 10_000}]);
        assert_eq!(
            super::compose_decline_reason(
                &missing_prob_date,
                &[],
                &[],
                &[],
                None,
                empty.clone(),
                (now, date())
            ),
            "missing-runs-problem-date"
        );
        let doc: serde_json::Value = serde_json::from_str(BENCH).expect("bench");
        let row =
            json!({"file": "writers/vm.lead/runs/2026-11-04/vm.lead--bench.json", "doc": doc});
        let fact = aac_status::read_scoreboard::row_facts(&row, &doc);
        let pres_with_id: aac_status::read_context::Presentation = serde_json::from_value(json!({
            "dates": {},
            "epochs": {"2026-11-04T16:30:00Z": 1793809800000_i64},
            "order": {"vm.lead--bench": 0, "pilot": 1}
        }))
        .expect("pres");
        assert_eq!(
            super::compose_decline_reason(
                &base_data,
                &[&row],
                &[&doc],
                &[&fact],
                None,
                Arc::new(pres_with_id),
                (now, date())
            ),
            "missing-row-key-rank"
        );
        let mut unk_doc = doc.clone();
        unk_doc["outcome"] = json!("Custom Upper");
        let unk_row =
            json!({"file": "writers/vm.lead/runs/2026-11-04/vm.lead--bench.json", "doc": unk_doc});
        let unk_fact = aac_status::read_scoreboard::row_facts(&unk_row, &unk_doc);
        let pres_with_keys: aac_status::read_context::Presentation =
            serde_json::from_value(json!({
                "dates": {},
                "epochs": {"2026-11-04T16:30:00Z": 1793809800000_i64},
                "order": {
                    "vm.lead--bench": 0,
                    "pilot": 1,
                    "claude-opus-5-5|claude-code|high": 2,
                    "claude-opus-5-5|claude-code|high||": 3
                }
            }))
            .expect("pres");
        assert_eq!(
            super::compose_decline_reason(
                &base_data,
                &[&unk_row],
                &[&unk_doc],
                &[&unk_fact],
                None,
                Arc::new(pres_with_keys),
                (now, date())
            ),
            "missing-outcome-rank"

    /// The runs state of the owner's held snapshot at `clock`, as the page reads it.
    async fn held_state(owner: &ScoreboardOwned, clock: u64) -> (serde_json::Value, bool) {
        let held = owner.held(Some(clock)).await.expect("held runs");
        let picked: Vec<usize> = (0..held.rows.len()).collect();
        let state = owner
            .state_of(&held, &picked, clock)
            .await
            .expect("runs state");
        let run_kept = has_bench(&held);
        (state, run_kept)
    }

    /// M2 (2026-11-22): a refusal names the retained run's path again when the owner restarts
    /// (its in-memory path map starts empty while the store's refusal persists), and an
    /// exact-byte repair ends the refusal instead of leaving Cached on a healthy file.
    #[tokio::test]
    async fn a_refused_run_is_named_after_an_owner_restart_and_cleared_by_an_exact_repair() {
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let ccs = tempfile::tempdir().expect("ccs");
        let store_dir = ccs.path().join("store");
        std::fs::create_dir_all(&store_dir).expect("store dir");
        let path = root
            .path()
            .join("v1/writers/vm.lead/runs/2026-11-04/vm.lead--bench.json");
        let label = "writers/vm.lead/runs/2026-11-04/vm.lead--bench.json";
        let good = std::fs::read(&path).expect("the good run's bytes");
        let at = 1_790_000_000_000u64;
        let host = StoreHost::new(boot(store_dir));
        host.open_at_boot();
        {
            let owner = owner(root.path().to_path_buf(), Some(host.clone()));
            owner.catalog_pass(at);
            owner.ingest_pass_at(true, at).await;
            let (state, kept) = held_state(&owner, at).await;
            assert!(kept, "the good run is accepted: {state}");
            assert!(
                state["problems"]
                    .as_array()
                    .expect("problems")
                    .iter()
                    .all(|p| p["file"] != json!(label)),
                "a good run is not refused: {state}"
            );
            // An invalid replacement of the same file is refused, and the refusal names it.
            std::fs::write(&path, [0xff_u8, 0xfe, 0x00]).expect("invalid replacement");
            owner.ingest_pass_at(true, at + 60_000).await;
            let (state, kept) = held_state(&owner, at + 60_000).await;
            assert!(kept, "the recorded run stays: {state}");
            assert!(
                state["problems"]
                    .as_array()
                    .expect("problems")
                    .iter()
                    .any(|p| p["file"] == json!(label)),
                "the refusal names the file this pass: {state}"
            );
        }
        // The owner restarts over the store: its in-memory path map is empty, the refusal is
        // still on record, and the scan skips the unchanged refused file.
        let at = at + 120_000;
        let owner = owner(root.path().to_path_buf(), Some(host));
        owner.catalog_pass(at);
        owner.ingest_pass_at(true, at).await;
        let (state, kept) = held_state(&owner, at).await;
        assert!(kept, "the recorded run stays after the restart: {state}");
        let refused: Vec<&serde_json::Value> = state["problems"]
            .as_array()
            .expect("problems")
            .iter()
            .filter(|p| p["file"] == json!(label))
            .collect();
        assert_eq!(
            refused.len(),
            1,
            "the persisted refusal names the retained run's path after a restart: {state}"
        );
        // The exact original bytes return: the refusal ends with them.
        std::fs::write(&path, &good).expect("exact-byte repair");
        owner.ingest_pass_at(true, at + 60_000).await;
        let (state, kept) = held_state(&owner, at + 60_000).await;
        assert!(kept, "the repaired run stays: {state}");
        assert!(
            state["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .all(|p| p["file"] != json!(label)),
            "a repaired file is no longer refused: {state}"
        );
    }

    /// M3 (2026-11-22): a folder problem that continues keeps the time it was first observed;
    /// a successful listing clears it.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_continuing_folder_problem_keeps_its_first_time_across_ingest_passes() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let day = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        let folder = "writers/vm.lead/runs/2026-11-04";
        let ccs = tempfile::tempdir().expect("ccs");
        let store_dir = ccs.path().join("store");
        std::fs::create_dir_all(&store_dir).expect("store dir");
        let host = StoreHost::new(boot(store_dir));
        host.open_at_boot();
        let owner = owner(root.path().to_path_buf(), Some(host));
        let t0 = 1_790_000_000_000u64;
        owner.catalog_pass(t0);
        owner.ingest_pass_at(true, t0).await;
        let (state, _) = held_state(&owner, t0).await;
        assert!(
            state["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .all(|p| p["file"] != json!(folder)),
            "a listed folder has no problem: {state}"
        );
        let original = std::fs::metadata(&day).expect("folder").permissions();
        std::fs::set_permissions(&day, std::fs::Permissions::from_mode(0o000))
            .expect("folder made unreadable");
        let first = t0 + 3_600_000;
        owner.ingest_pass_at(true, first).await;
        let (state, kept) = held_state(&owner, first).await;
        assert!(kept, "the run is served from history: {state}");
        let since = |state: &serde_json::Value| {
            state["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .find(|p| p["file"] == json!(folder))
                .map(|p| p["since"].as_u64().expect("since"))
                .expect("the unreadable folder is a problem")
        };
        assert_eq!(since(&state), first);
        let second = first + 3_600_000;
        owner.ingest_pass_at(true, second).await;
        let (state, _) = held_state(&owner, second).await;
        assert_eq!(
            since(&state),
            first,
            "the continuing folder problem keeps the time it was first observed"
        );
        std::fs::set_permissions(&day, original).expect("folder restored");
        let third = second + 3_600_000;
        owner.ingest_pass_at(true, third).await;
        let (state, _) = held_state(&owner, third).await;
        assert!(
            state["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .all(|p| p["file"] != json!(folder)),
            "a successful listing clears the folder problem: {state}"
        );
    }

    /// The view cache is bounded in bytes, not only entries: oversized views evict the oldest
    /// until the kept bytes fit the cap, the newest view always survives, and a single view
    /// bigger than the cap is still kept (an empty cache would only recompose it at once).
    #[test]
    fn the_view_cache_is_bounded_in_bytes() {
        let root = tempfile::tempdir().expect("reports");
        let owner = owner(root.path().to_path_buf(), None);
        let big = 5 * 1024 * 1024;
        for i in 0..4u64 {
            let cached = Arc::new(CachedView {
                at: 1_790_000_000_000 + i * 1_000,
                seq: 1,
                bytes: vec![b'x'; big],
                age_value_offset: None,
                etag: format!("W/\"{i}\""),
            });
            owner.keep_view(format!("cache-{i}"), format!("filter-{i}"), cached);
        }
        {
            let views = owner.views.lock().unwrap_or_else(|p| p.into_inner());
            let total: usize = views.values().map(|view| view.bytes.len()).sum();
            assert!(
                total <= CACHE_BYTES,
                "views held {total} bytes over the {CACHE_BYTES} cap"
            );
            assert!(views.contains_key("cache-3"), "the newest view survives");
            assert!(
                views.len() < CACHE_ENTRIES,
                "oversized views evict by bytes, not only by count"
            );
        }
        {
            let good = owner.last_good.lock().unwrap_or_else(|p| p.into_inner());
            let total: usize = good.values().map(|view| view.bytes.len()).sum();
            assert!(total <= CACHE_BYTES, "last good held {total} bytes");
            assert!(good.contains_key("filter-3"));
        }
        owner.keep_view(
            "solo".into(),
            "solo-filter".into(),
            Arc::new(CachedView {
                at: 1_790_000_100_000,
                seq: 2,
                bytes: vec![b'y'; CACHE_BYTES + 1],
                age_value_offset: None,
                etag: "W/\"solo\"".into(),
            }),
        );
        let views = owner.views.lock().unwrap_or_else(|p| p.into_inner());
        assert_eq!(views.len(), 1, "one oversized view keeps only itself");
        assert!(views.contains_key("solo"));
    }

    /// SB3 (2026-11-23 A): a writer's `runs` folder that cannot be listed is named in the runs
    /// state, its retained review stays, and a continuing failure keeps its first time. The
    /// composer matches the ancestor path, so the verdict that used the review says so.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_unreadable_writer_runs_folder_is_named_with_its_first_time_and_clears_on_a_good_listing()
     {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        write_review(root.path());
        let folder = "writers/mac.reviews/runs";
        let runs = root.path().join("v1").join(folder);
        let ccs = tempfile::tempdir().expect("ccs");
        let store_dir = ccs.path().join("store");
        std::fs::create_dir_all(&store_dir).expect("store dir");
        let host = StoreHost::new(boot(store_dir));
        host.open_at_boot();
        let owner = owner(root.path().to_path_buf(), Some(host));
        let t0 = 1_790_000_000_000u64;
        owner.catalog_pass(t0);
        owner.ingest_pass_at(true, t0).await;
        let (state, _) = held_state(&owner, t0).await;
        assert!(
            state["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .all(|p| p["file"] != json!(folder)),
            "a listed folder has no problem: {state}"
        );
        let original = std::fs::metadata(&runs).expect("folder").permissions();
        std::fs::set_permissions(&runs, std::fs::Permissions::from_mode(0o000))
            .expect("folder made unreadable");
        let first = t0 + 3_600_000;
        owner.ingest_pass_at(true, first).await;
        let (state, kept) = held_state(&owner, first).await;
        assert!(kept, "the bench run is served from history: {state}");
        let review = owner
            .held(Some(first))
            .await
            .expect("held")
            .rows
            .iter()
            .any(|(_, row)| row["doc"]["id"].as_str() == Some("mac.reviews--R1"));
        assert!(review, "the review under the unlistable folder is retained");
        let since = |state: &serde_json::Value| {
            state["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .find(|p| p["file"] == json!(folder))
                .map(|p| p["since"].as_u64().expect("since"))
                .expect("the unlistable writer folder is a problem")
        };
        assert_eq!(since(&state), first);
        let second = first + 3_600_000;
        owner.ingest_pass_at(true, second).await;
        let (state, _) = held_state(&owner, second).await;
        assert_eq!(
            since(&state),
            first,
            "the continuing writer folder problem keeps the time it was first observed"
        );
        std::fs::set_permissions(&runs, original).expect("folder restored");
        let third = second + 3_600_000;
        owner.ingest_pass_at(true, third).await;
        let (state, _) = held_state(&owner, third).await;
        assert!(
            state["problems"]
                .as_array()
                .expect("problems")
                .iter()
                .all(|p| p["file"] != json!(folder)),
            "a successful listing clears the writer folder problem: {state}"
        );
    }
}
