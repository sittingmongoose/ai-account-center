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
/// The largest facts body held: the facts over a real-size run set (8,915 runs, a 33 MB
/// inputs body) pass the feed's historical 4 MiB, so the cap fits the set with room to grow.
const FACTS_CAP: usize = 64 * 1024 * 1024;
/// Filtered views kept at once.
const CACHE_ENTRIES: usize = 16;
/// A pass every two minutes; again within a second while a backfill has more.
pub(crate) const PASS_MS: u64 = 120_000;
pub(crate) const MORE_MS: u64 = 1_000;
/// A full pass (every folder listed, whatever its mtime) at least this often.
const FULL_MS: u64 = 15 * 60_000;
/// A paged read's restart budget, and the completed reads kept for its fallback.
const QUERY_RESTARTS: u32 = 3;
const READ_CACHE_ENTRIES: usize = 8;
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

/// One fully completed paged read: its rows and the snapshot they were read against.
#[derive(Clone)]
struct CompletedRead {
    runs: Vec<Json>,
    problems: Vec<Json>,
    accepted: Option<Vec<Json>>,
    generation: u64,
    at: u64,
}

/// The unfiltered runs of one store snapshot, held while the owner's (generation, catalog
/// hash) names it: every filter is answered from it as `runsQuery` answers (time bounds on the
/// row's `[endedMs, r]` cursor, inclusive; source, task type and reviews on the run), so a new
/// filter reads no store page. A new generation replaces it, as it replaces the view cache.
/// Per held snapshot and catalog: what the composer's envelope path derives from every row
/// before it composes, made once instead of per request.
struct PreparedDocs {
    /// Each row's display-normalized doc with its run's review merged in (the envelope
    /// path's `effective` step), aligned with `HeldRuns::rows`.
    docs: Vec<Json>,
    /// Whether the row or its doc holds an exact prototype name (`status_fed::proto_exact`): a
    /// filter that picks one declines. Prose holding such a name as a word is answered.
    proto: Vec<bool>,
    /// What compose reads of each row (`read_scoreboard::row_facts`).
    facts: Vec<aac_status::read_scoreboard::RowFacts>,
    /// Each doc's position in the newest order, per facts generation id (None: not provable).
    order: Mutex<Option<PositionsByFacts>>,
}

type PositionsByFacts = (String, Option<Arc<Vec<usize>>>);

impl PreparedDocs {
    /// The newest positions under this facts generation, made once per id.
    fn positions(
        &self,
        facts_id: &str,
        facts: &std::sync::Arc<Presentation>,
    ) -> Option<Arc<Vec<usize>>> {
        let mut held = self.order.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((id, positions)) = held.as_ref()
            && id == facts_id
        {
            return positions.clone();
        }
        let docs: Vec<&Json> = self.docs.iter().collect();
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
    /// Each row as `runsQuery` returns it (no `_cursor`), with its cursor's time.
    rows: Vec<(u64, Json)>,
    /// Each row as the runs state carries it (`file`, `doc`, `firstSeenAt`, `sha256`).
    state_rows: Vec<Json>,
    problems: Vec<Json>,
    accepted: Option<Vec<Json>>,
    /// Each row's composer doc over the catalog with this hash (made on first use).
    docs: Mutex<Option<(String, Arc<PreparedDocs>)>>,
    /// The readers-check rows of the snapshot's receipts (made on first use).
    accepted_rows: std::sync::OnceLock<Vec<RunRow>>,
    /// Present when this snapshot came from the share, not `runsQuery`.
    share: Option<ShareRead>,
}

impl HeldRuns {
    fn new(
        key: (u64, String),
        rows: Vec<(u64, Json)>,
        problems: Vec<Json>,
        accepted: Option<Vec<Json>>,
    ) -> Self {
        let state_rows = rows
            .iter()
            .map(|(_, run)| {
                json!({
                    "file": run["file"],
                    "doc": run["doc"],
                    "firstSeenAt": run["firstSeenAt"],
                    "sha256": run["sha256"],
                })
            })
            .collect();
        Self {
            key,
            rows,
            state_rows,
            problems,
            accepted,
            docs: Mutex::new(None),
            accepted_rows: std::sync::OnceLock::new(),
            share: None,
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
            .filter(|(_, (at, row))| {
                let doc = &row["doc"];
                from.is_none_or(|from| *at >= from)
                    && to.is_none_or(|to| *at <= to)
                    && source.is_none_or(|want| doc["source"].as_str() == Some(want))
                    && task_type.is_none_or(|want| doc["task"]["type"].as_str() == Some(want))
                    && (!reviews_only || doc["review"].is_object())
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

/// Clears the share-scan flag however the scan ends.
struct ScanRunning<'a>(&'a AtomicBool);

impl Drop for ScanRunning<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

struct CachedView {
    at: u64,
    view: Json,
    etag: String,
}

/// A served view's bytes at `now`: the view itself with only `cacheAgeSeconds` filled.
/// A stale serve (stale-while-revalidate, or the last good view during a compose) also
/// says so where the page reads it: `refreshing`, each source's age advanced by the
/// cache age with `ok` turned `stale` past its own `staleAfterSeconds`, and an ETag
/// recomputed over the changed bytes. A fresh hit keeps the composed ETag, as before.
fn cached_bytes(view: &Json, at: u64, etag: &str, now: u64, stale: bool) -> (Vec<u8>, String) {
    let age_seconds = now.saturating_sub(at) as f64 / 1000.0;
    let mut view = view.clone();
    view["cacheAgeSeconds"] = json!(age_seconds);
    if stale {
        view["refreshing"] = json!(true);
        if let Some(sources) = view["sources"].as_array_mut() {
            for source in sources {
                let aged = source["ageSeconds"].as_f64().unwrap_or(0.0) + age_seconds;
                source["ageSeconds"] = json!(aged);
                let stale_after = source["staleAfterSeconds"].as_f64().unwrap_or(180.0);
                if aged > stale_after && source["state"].as_str() == Some("ok") {
                    source["state"] = json!("stale");
                }
            }
        }
    }
    let mut writer = Writer::with_capacity(64 * 1024);
    writer.value(&view);
    let (bytes, identity_bytes) = writer.finish();
    if stale {
        let digest = Sha256::digest(&identity_bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        (bytes, format!("W/\"{hex}\""))
    } else {
        (bytes, etag.to_string())
    }
}

/// What one computed presentation was made over: the held snapshot (by sequence, rows and
/// its generation key), the catalog snapshot it saw (by its scan stamp: a catalog pass
/// changes the content and the stamp together), the seed failures and one display zone.
/// Any move recomputes.
#[derive(Clone, PartialEq, Eq)]
struct FactsKey {
    held_seq: u64,
    rows: usize,
    generation: u64,
    catalog_hash: String,
    catalog_scanned_at: Option<u64>,
    seed_hash: String,
    time_zone: String,
}

impl FactsKey {
    /// The facts generation id these facts are held under (feed word: short, no zone name,
    /// which carries a `/`; the zone enters as its hash's first digits).
    fn id(&self) -> String {
        let short: String = self.catalog_hash.chars().take(12).collect();
        let zone: String = sha256_text(&self.time_zone).chars().take(8).collect();
        format!(
            "{}:{}:{}:{}",
            self.generation,
            short,
            self.catalog_scanned_at.unwrap_or(0),
            zone
        )
    }
}

/// The facts this owner computed itself and their parsed presentation, once per key.
struct ComputedFacts {
    key: FactsKey,
    presentation: Arc<Presentation>,
}

/// What one built inputs value was made over (see [`FactsKey`], plus the catalog it carries).
#[derive(Clone, PartialEq)]
struct InputsKey {
    held_seq: u64,
    rows: usize,
    generation: u64,
    catalog_hash: String,
    catalog_scanned_at: Json,
}

/// One share run file's scan outcome, reused while its stat print (size, mtime, inode,
/// ctime) holds: the parsed row (with its first-seen stamp) or the refusal reason.
#[derive(Clone)]
enum CachedOutcome {
    Row { at: u64, row: Json },
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
    safe_paths: Mutex<HashMap<String, String>>,
    seed_failures: Mutex<Vec<Json>>,
    reads: Mutex<IndexMap<String, CompletedRead>>,
    /// The current snapshot's unfiltered runs (see [`HeldRuns`]).
    held_runs: Mutex<Option<Arc<HeldRuns>>>,
    /// Bumped before and after every `runsRecord` (this owner is the runs tables' only
    /// writer): the store facts below are held while it has not moved.
    store_epoch: AtomicU64,
    /// `runsRefused`, `runsIdentity` and `runsStatus.runs` as of a store epoch.
    store_facts: Mutex<Option<(u64, Arc<StoreFacts>)>>,
    /// The newest snapshot generation whose readers-check rows were recorded.
    recorded: Arc<Mutex<u64>>,
    views: Mutex<IndexMap<String, CachedView>>,
    /// The last good view per filter, across generations: served while its refresh runs.
    last_good: Mutex<IndexMap<String, CachedView>>,
    /// The last pushed facts generation, parsed once: (its id, its presentation). Only
    /// generations pushed into the feed land here; computed facts never do (they would
    /// shadow a newer snapshot under an old snapshot-blind id).
    parsed_facts: Mutex<Option<(String, std::sync::Arc<Presentation>)>>,
    /// The facts computed here when the front door pushed none, once per held snapshot.
    computed_facts: Mutex<Option<ComputedFacts>>,
    /// One facts computation at a time (the computation itself runs on the blocking pool).
    facts_compute: tokio::sync::Mutex<()>,
    /// The last built inputs value, built once per snapshot under its lock (singleflight).
    inputs_cache: tokio::sync::Mutex<Option<(InputsKey, Json)>>,
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
            safe_paths: Mutex::new(HashMap::new()),
            seed_failures: Mutex::new(Vec::new()),
            reads: Mutex::new(IndexMap::new()),
            held_runs: Mutex::new(None),
            store_epoch: AtomicU64::new(0),
            store_facts: Mutex::new(None),
            recorded: Arc::new(Mutex::new(0)),
            views: Mutex::new(IndexMap::new()),
            last_good: Mutex::new(IndexMap::new()),
            parsed_facts: Mutex::new(None),
            computed_facts: Mutex::new(None),
            facts_compute: tokio::sync::Mutex::new(()),
            inputs_cache: tokio::sync::Mutex::new(None),
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
        let picked: Option<Vec<usize>> = held.accepted.is_none().then(|| picked.to_vec());
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
                        .get_or_init(|| check_rows(held.accepted.as_deref().unwrap_or(&[]))),
                    Some(picked) => {
                        let state: Vec<&Json> = picked
                            .iter()
                            .map(|&index| &held.state_rows[index])
                            .collect();
                        made = check_rows_of(&state);
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
        let Some(store) = &self.store else {
            return;
        };
        let now = now_ms();
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
        self.store_epoch.fetch_add(1, Ordering::SeqCst);
        let record = store
            .call("runsRecord".to_string(), request.record.to_string())
            .await;
        self.store_epoch.fetch_add(1, Ordering::SeqCst);
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
        // One unfiltered read of the snapshot, with the rows' cursors and the receipts.
        match self
            .read_store(
                &json!({"includeReceipts": true, "includeCursors": true}),
                clock,
            )
            .await
        {
            Ok(full) => {
                self.share_fallback.store(false, Ordering::SeqCst);
                let rows = full
                    .runs
                    .into_iter()
                    .map(|mut row| {
                        let at = row["_cursor"][0].as_u64().unwrap_or(0);
                        if let Some(object) = row.as_object_mut() {
                            object.remove("_cursor");
                        }
                        (at, row)
                    })
                    .collect();
                let held = Arc::new(HeldRuns::new(
                    (full.generation, key.1.clone()),
                    rows,
                    full.problems,
                    full.accepted,
                ));
                // Held only when the read is the snapshot the owner names (an ingest may have landed).
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
    /// one scan to land.
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
                self.kick_share_scan(now);
                if !self.share_scan_running.load(Ordering::SeqCst) {
                    return Err(Decline::Internal);
                }
            }
            notified.await;
        }
    }

    /// Start the detached share scan if none is running. The running flag and the cache
    /// live in that task, so no caller's drop can clear the flag early or lose the cache.
    fn kick_share_scan(&self, now: u64) {
        if self.share_scan_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let Some(owner) = self.me.get().and_then(std::sync::Weak::upgrade) else {
            self.share_scan_running.store(false, Ordering::SeqCst);
            self.share_scan_failed.store(true, Ordering::SeqCst);
            self.share_scan_done.notify_waiters();
            return;
        };
        tokio::spawn(async move {
            let _running = ScanRunning(&owner.share_scan_running);
            owner.run_share_scan(now).await;
            owner.share_scan_done.notify_waiters();
        });
    }

    /// One detached share scan: the blocking scan over the shared per-file cache, then
    /// its snapshot held for every later caller.
    async fn run_share_scan(&self, now: u64) {
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
                    HeldRuns::new(self.generation(), scan.rows, scan.problems, None)
                        .with_share(scan.read),
                );
                *self.held_runs.lock().unwrap_or_else(|p| p.into_inner()) = Some(held);
                self.held_seq.fetch_add(1, Ordering::SeqCst);
            }
            Err(_) => {
                self.share_scan_failed.store(true, Ordering::SeqCst);
                eprintln!("aac-http: scoreboard share scan task failed");
            }
        }
    }

    /// The store's paged read of one query (see [`Self::read`] for the held snapshot).
    async fn read_store(&self, query: &Json, clock: Option<u64>) -> Result<CompletedRead, Decline> {
        let store = self.store.as_ref().ok_or(Decline::Unsupported)?;
        let signature = json!([
            query["fromMs"],
            query["toMs"],
            query["source"],
            query["taskType"],
            query["reviewsOnly"].as_bool().unwrap_or(false),
        ])
        .to_string();
        let mut runs: Vec<Json> = Vec::new();
        let mut after: Json = Json::Null;
        let mut snapshot = String::new();
        let mut generation = 0u64;
        let mut problems: Vec<Json> = Vec::new();
        let mut accepted: Option<Vec<Json>> = None;
        let mut at = clock.unwrap_or_else(now_ms);
        let mut restarts = 0u32;
        loop {
            let mut args = query.clone();
            args["after"] = after.clone();
            let text = store
                .call("runsQuery".to_string(), args.to_string())
                .await
                .map_err(|_| Decline::Internal)?;
            let page: Json = serde_json::from_str(&text).map_err(|_| Decline::Internal)?;
            let key = format!(
                "{}:{}",
                page["generation"].as_u64().unwrap_or(0),
                page["catalogHash"].as_str().unwrap_or("")
            );
            if after.is_null() {
                snapshot = key;
                generation = page["generation"].as_u64().unwrap_or(0);
                at = clock.unwrap_or_else(now_ms);
                problems = page["problems"].as_array().cloned().unwrap_or_default();
                accepted = page["accepted"].as_array().cloned();
            } else if key != snapshot {
                if restarts >= QUERY_RESTARTS {
                    let kept = self
                        .reads
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .get(&signature)
                        .cloned();
                    return match kept {
                        Some(kept) => {
                            let mut problems = kept.problems.clone();
                            problems.push(json!({
                                "file": "store/read",
                                "reason": format!(
                                    "The run history kept importing during the read; this view is the last complete read (generation {}).",
                                    kept.generation
                                ),
                                "since": kept.at,
                            }));
                            Ok(CompletedRead { problems, ..kept })
                        }
                        None => Err(Decline::Internal),
                    };
                }
                restarts += 1;
                runs.clear();
                after = Json::Null;
                continue;
            }
            runs.extend(page["runs"].as_array().cloned().unwrap_or_default());
            let next = page["next"].clone();
            if next.is_null() {
                let read = CompletedRead {
                    runs,
                    problems,
                    accepted,
                    generation,
                    at,
                };
                let mut reads = self.reads.lock().unwrap_or_else(|p| p.into_inner());
                reads.shift_remove(&signature);
                reads.insert(signature, read.clone());
                while reads.len() > READ_CACHE_ENTRIES {
                    reads.shift_remove_index(0);
                }
                return Ok(read);
            }
            after = next;
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
        let mut problems: Vec<Json> = if held.share.is_some() {
            Vec::new()
        } else {
            refused
                .iter()
                .map(|row| {
                    let hash = row["pathHash"].as_str().unwrap_or_default();
                    json!({
                        "file": safe_paths.get(hash).cloned().unwrap_or_else(|| {
                            format!("refused/{NAME_WITHHELD} {}", &hash[..hash.len().min(12)])
                        }),
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
                writers.insert(
                    held.rows[index].1["doc"]["writer"]
                        .as_str()
                        .unwrap_or_default(),
                );
                index
            })
            .collect::<Vec<usize>>()
            .into_iter()
            .filter(|_| rows)
            .map(|index| held.state_rows[index].clone())
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
            match (receipts, &held.accepted) {
                (true, Some(accepted)) => Json::Array(accepted.clone()),
                _ => Json::Null,
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

    /// Each doc's display model resolution over the held catalog.
    fn normalized_doc_list<'d>(
        &self,
        docs: impl Iterator<Item = &'d Json>,
        catalog: &Json,
    ) -> Vec<Json> {
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
        docs.map(|doc| {
            let id = doc["model"]["id"].as_str().unwrap_or_default();
            if id.is_empty() {
                return doc.clone();
            }
            let resolved =
                normalize_model(id, doc["model"]["effort"].as_str(), &models, Some(&table));
            let mut doc = doc.clone();
            doc["model"]["id"] = json!(resolved.id);
            doc["model"]["effort"] = json!(resolved.effort);
            doc["model"]["provider"] = Json::Null;
            doc
        })
        .collect()
    }

    /// The held snapshot's normalized docs over this catalog, made once per catalog hash.
    fn held_docs(&self, held: &HeldRuns, catalog: &Json, catalog_hash: &str) -> Arc<PreparedDocs> {
        let mut docs = held.docs.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((hash, made)) = docs.as_ref()
            && hash == catalog_hash
        {
            return made.clone();
        }
        let mut normalized =
            self.normalized_doc_list(held.rows.iter().map(|(_, row)| &row["doc"]), catalog);
        for (doc, (_, row)) in normalized.iter_mut().zip(&held.rows) {
            if row["doc"]["review"].is_object() {
                doc["review"] = row["doc"]["review"].clone();
            }
        }
        let proto = held
            .state_rows
            .iter()
            .zip(&normalized)
            .map(|(row, doc)| {
                crate::status_fed::proto_exact(row) || crate::status_fed::proto_exact(doc)
            })
            .collect();
        let facts = held
            .state_rows
            .iter()
            .zip(&normalized)
            .map(|(row, doc)| aac_status::read_scoreboard::row_facts(row, doc))
            .collect();
        let made = Arc::new(PreparedDocs {
            docs: normalized,
            proto,
            facts,
            order: Mutex::new(None),
        });
        *docs = Some((catalog_hash.to_string(), made.clone()));
        made
    }

    pub(crate) fn publish_facts(&self, body: &[u8]) -> Result<Json, Decline> {
        if body.len() > FACTS_CAP {
            eprintln!(
                "aac-http: scoreboard facts refused ({} bytes over the {FACTS_CAP} byte cap)",
                body.len()
            );
            return Err(Decline::Shape);
        }
        self.feeds.put("scoreboard-facts", body)
    }

    /// The presentation for one answer, and the generation id it is held under (the
    /// newest-order memo keys on it): a generation the caller pushed into the feed wins,
    /// parsed once per id; otherwise the facts computed here over the held snapshot and
    /// this answer's one catalog snapshot — the same values the front door computed over
    /// `inputs`, without shipping the whole run set to it and back, and without the feed
    /// body's transport cap. Computed once per [`FactsKey`], singleflight, on the
    /// blocking pool. Computed facts are held only under their own key-derived id, never
    /// under a caller-named one, so a later snapshot can never be served this key's facts.
    async fn presentation(
        &self,
        feed: &Feed,
        held: &Arc<HeldRuns>,
        catalog: &Json,
        now: u64,
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
        let picked: Vec<usize> = (0..held.rows.len()).collect();
        let runs = self.state_of(held, &picked, now).await?;
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
        // it sent), the catalog, the unfiltered runs state and every normalized doc.
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
        let time_zone = feed.time_zone.clone();
        let feed_now = feed.now;
        let catalog_owned = catalog.clone();
        let (facts_text, presentation) =
            tokio::task::spawn_blocking(move || -> Result<(String, Presentation), Decline> {
                let offsets = crate::zone_facts::server_zone_facts(&time_zone, feed_now)
                    .ok_or(Decline::Shape)?;
                let mut values: Vec<&Json> = Vec::with_capacity(prepared.docs.len() + 3);
                values.push(&skeleton);
                values.push(&catalog_owned);
                values.push(&runs);
                values.extend(prepared.docs.iter());
                let facts = aac_status::produce::facts::Facts::of(&values, Some(&offsets.zone));
                // Every word ranked, as the front door completed them (`status_collate`).
                let facts_text =
                    crate::status_collate::complete_order_of(&values, &facts.to_text())
                        .unwrap_or_else(|| facts.to_text());
                let presentation: Presentation =
                    serde_json::from_str(&facts_text).map_err(|_| Decline::Shape)?;
                Ok((facts_text, presentation))
            })
            .await
            .map_err(|_| Decline::Internal)??;
        let presentation = Arc::new(presentation);
        let id = key.id();
        // Held for the feed's other readers too, under this key's own id. A refusal (the
        // same id holding other bytes) keeps this answer on the computed presentation
        // and is named in the log.
        let body = format!(
            "{{\"generation\":{},\"data\":{},\"facts\":{}}}",
            Json::String(id.clone()),
            json!({"timeZone": feed.time_zone}),
            facts_text
        );
        eprintln!(
            "aac-http: scoreboard facts computed in service ({} bytes)",
            body.len()
        );
        if let Err(reason) = self.publish_facts(body.as_bytes()) {
            eprintln!("aac-http: scoreboard facts not held ({})", reason.as_str());
        }
        *self
            .computed_facts
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = Some(ComputedFacts {
            key,
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
        let held = self.held(Some(now)).await?;
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
            views
                .get(&cache_key)
                .map(|hit| (hit.at, hit.view.clone(), hit.etag.clone()))
        };
        if let Some((at, view, etag)) = hit {
            if now.saturating_sub(at) < CACHE_MS {
                return Ok(cached_bytes(&view, at, &etag, now, false));
            }
            if !refresh {
                self.spawn_refresh(feed_text, &cache_key);
                return Ok(cached_bytes(&view, at, &etag, now, true));
            }
        }
        // One compose per view-cache key. A request that finds its key being composed (its
        // first request outlived the budget and still runs) serves the last good view for
        // its filter while that compose finishes; only a filter with no good view is `busy`.
        // A refresh finds its slot already taken by `spawn_refresh` and owns its release.
        let _composing = {
            let mut composing = self.composing.lock().unwrap_or_else(|p| p.into_inner());
            if refresh {
                composing.insert(cache_key.clone());
            } else if !composing.insert(cache_key.clone()) {
                drop(composing);
                let hit = {
                    let views = self.views.lock().unwrap_or_else(|p| p.into_inner());
                    views
                        .get(&cache_key)
                        .map(|hit| (hit.at, hit.view.clone(), hit.etag.clone()))
                }
                .or_else(|| {
                    let good = self.last_good.lock().unwrap_or_else(|p| p.into_inner());
                    good.get(&filter_key)
                        .map(|hit| (hit.at, hit.view.clone(), hit.etag.clone()))
                });
                if let Some((at, view, etag)) = hit {
                    return Ok(cached_bytes(&view, at, &etag, now, true));
                }
                return Err(Decline::Busy);
            }
            Composing {
                set: &self.composing,
                key: cache_key.clone(),
            }
        };
        // The facts generation the request names, pushed by its caller, or the facts
        // computed over this snapshot when none was pushed. The request's own `now`
        // comes from its header words.
        let (presentation, order_key) = self.presentation(&feed, &held, &catalog, now).await?;
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
        self.record_check_held(&held, &picked, &runs);
        let join_reviews = if filtered {
            let mut reviews_query = Map::new();
            if let Some(from_ms) = feed.from_ms {
                reviews_query.insert("fromMs".to_string(), json!(from_ms));
            }
            reviews_query.insert("reviewsOnly".to_string(), json!(true));
            held.pick(&Json::Object(reviews_query))
                .into_iter()
                .map(|index| held.rows[index].1["doc"].clone())
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        let prepared = self.held_docs(&held, &catalog, &catalog_hash);
        if picked.iter().any(|&index| prepared.proto[index]) {
            return Err(Decline::Unsupported);
        }
        let rows: Vec<&Json> = picked
            .iter()
            .map(|&index| &held.state_rows[index])
            .collect();
        let docs: Vec<&Json> = picked.iter().map(|&index| &prepared.docs[index]).collect();
        let row_facts: Vec<&aac_status::read_scoreboard::RowFacts> =
            picked.iter().map(|&index| &prepared.facts[index]).collect();
        let positions = prepared.positions(&order_key, &presentation);
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
        let view = aac_status::read_scoreboard::compose_fed_parts(
            &data,
            &rows,
            &docs,
            &row_facts,
            order.as_deref(),
            presentation,
            (feed.now, now_date),
        )
        .ok_or(Decline::Stale)?;
        let mut writer = Writer::with_capacity(64 * 1024);
        writer.value(&view);
        let (bytes, identity_bytes) = writer.finish();
        let digest = Sha256::digest(&identity_bytes);
        let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
        let etag = format!("W/\"{hex}\"");
        {
            let mut views = self.views.lock().unwrap_or_else(|p| p.into_inner());
            views.shift_remove(&cache_key);
            views.insert(
                cache_key,
                CachedView {
                    at: now,
                    view: view.clone(),
                    etag: etag.clone(),
                },
            );
            while views.len() > CACHE_ENTRIES {
                views.shift_remove_index(0);
            }
        }
        {
            let mut good = self.last_good.lock().unwrap_or_else(|p| p.into_inner());
            good.shift_remove(&filter_key);
            good.insert(
                filter_key,
                CachedView {
                    at: now,
                    view,
                    etag: etag.clone(),
                },
            );
            while good.len() > CACHE_ENTRIES {
                good.shift_remove_index(0);
            }
        }
        Ok((bytes, etag))
    }

    /// The service's unfiltered input (a superset of every filter's rows), the catalog,
    /// the normalized docs and the finish parts, at the current generation: what the
    /// facts are computed over here, kept for the readers that still ask. Built once per
    /// held snapshot and catalog scan, singleflight under its lock: the value clones every
    /// run, so no request rebuilds the last one's build.
    pub(crate) async fn inputs(&self) -> Result<Json, Decline> {
        let (generation, catalog_hash) = self.generation();
        let held = self.held(None).await?;
        let catalog = self
            .catalog_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let key = InputsKey {
            held_seq: self.held_seq.load(Ordering::SeqCst),
            rows: held.rows.len(),
            generation,
            catalog_hash: catalog_hash.clone(),
            catalog_scanned_at: catalog["scannedAt"].clone(),
        };
        let mut cache = self.inputs_cache.lock().await;
        if let Some((held_key, value)) = cache.as_ref()
            && *held_key == key
        {
            return Ok(value.clone());
        }
        let picked: Vec<usize> = (0..held.rows.len()).collect();
        let runs = self.state_of(&held, &picked, now_ms()).await?;
        let docs = self.held_docs(&held, &catalog, &catalog_hash).docs.clone();
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
        let value = json!({
            "generation": generation,
            "catalogHash": catalog_hash,
            "catalog": catalog,
            "runs": runs,
            "docs": docs,
            "identity": identity,
            "seedFailures": seed_failures,
            "shareOnly": held.share.is_some(),
        });
        *cache = Some((key, value.clone()));
        Ok(value)
    }
}

struct ShareScan {
    rows: Vec<(u64, Json)>,
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
    Row(u64, Json, bool),
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
                ended_at_ms(&doc["endedAt"]),
                json!({
                    "file": rel,
                    "doc": doc,
                    "firstSeenAt": now,
                    "sha256": sha256_text(text),
                }),
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
    let mut parsed: Vec<(u64, Json)> = Vec::new();
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
                CachedOutcome::Row { at, row } => parsed.push((*at, row.clone())),
                CachedOutcome::Problem { reason } => {
                    problems.push(share_problem(rel, reason.clone(), now));
                }
            }
            continue;
        }
        match read_share_run(&fs, &abs, &rel, now, &print, size) {
            ShareFile::Row(at, row, cacheable) => {
                if cacheable && !print.is_empty() {
                    cache.insert(
                        rel,
                        CachedRun {
                            print,
                            outcome: CachedOutcome::Row {
                                at,
                                row: row.clone(),
                            },
                        },
                    );
                }
                parsed.push((at, row));
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
    let mut best: HashMap<String, (u64, Json)> = HashMap::new();
    for (at, row) in parsed {
        let id = row["doc"]["id"].as_str().unwrap_or_default().to_string();
        let newer = match best.get(&id) {
            None => true,
            Some((_, held)) => {
                let a_seq = row["doc"]["seq"].as_u64().unwrap_or(0);
                let b_seq = held["doc"]["seq"].as_u64().unwrap_or(0);
                let a_written = row["doc"]["writtenAt"].as_str().unwrap_or_default();
                let b_written = held["doc"]["writtenAt"].as_str().unwrap_or_default();
                let a_file = row["file"].as_str().unwrap_or_default();
                let b_file = held["file"].as_str().unwrap_or_default();
                a_seq > b_seq
                    || (a_seq == b_seq && a_written > b_written)
                    || (a_seq == b_seq && a_written == b_written && a_file > b_file)
            }
        };
        if newer {
            best.insert(id, (at, row));
        }
    }
    let mut records: Vec<(u64, Json)> = best.into_values().collect();
    records.sort_by(|left, right| {
        left.1["doc"]["id"]
            .as_str()
            .unwrap_or_default()
            .cmp(right.1["doc"]["id"].as_str().unwrap_or_default())
    });
    let docs: Vec<Json> = records.iter().map(|(_, row)| row.clone()).collect();
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
    let rows = resolved
        .rows
        .into_iter()
        .map(|row| (ended_at_ms(&row["doc"]["endedAt"]), row))
        .collect();
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

pub(crate) fn ingest_request(
    scan: &Json,
    now: u64,
    safe_paths: &mut HashMap<String, String>,
) -> IngestRequest {
    let (files, refused, threads, read_count) = {
        let mut remember = |path: &str| {
            if aac_status::secret_policy::find_secret(path).is_none() {
                safe_paths.insert(path_hash(path), path.to_string());
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

#[cfg(test)]
mod tests {
    use super::{HeldRuns, ScoreboardOwned, scan_share_runs, scan_share_runs_cached};
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
        held.rows
            .iter()
            .any(|(_, row)| row["doc"]["id"].as_str() == Some("vm.lead--bench"))
    }

    #[test]
    fn the_share_scan_reads_the_reviewers_valid_run() {
        let root = tempfile::tempdir().expect("reports");
        write_bench(root.path());
        let scan = scan_share_runs(root.path(), 1_790_000_000_000);
        assert_eq!(scan.read.state, "ok");
        assert_eq!(scan.read.scanned_at, Some(1_790_000_000_000));
        assert!(
            scan.rows
                .iter()
                .any(|(_, row)| { row["doc"]["id"].as_str() == Some("vm.lead--bench") }),
            "{:?}",
            scan.rows
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
        assert!(has_bench(&held), "missing vm.lead--bench: {:?}", held.rows);
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
        for (index, (_, row)) in held.rows.iter().enumerate() {
            match row["doc"]["id"].as_str() {
                Some("vm.lead--bench") => {
                    seen += 1;
                    assert!(
                        aac_status::read_context::proto_named(row),
                        "word rule sees it"
                    );
                    assert!(!prepared.proto[index], "prose run must be answered");
                }
                Some("vm.lead--exact") => {
                    seen += 1;
                    assert!(prepared.proto[index], "an exact name still declines");
                }
                _ => {}
            }
        }
        assert_eq!(seen, 2, "{:?}", held.rows);
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
        assert!(has_bench(&held), "missing vm.lead--bench: {:?}", held.rows);
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
        for _ in 0..300 {
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
        let published = owner.publish_facts(body.as_bytes());
        assert!(published.is_ok(), "publish_facts: {published:?}");
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
                .find(|(_, row)| row["doc"]["id"].as_str() == Some(id))
                .and_then(|(_, row)| row["firstSeenAt"].as_u64())
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
            .find(|(_, row)| row["doc"]["id"].as_str() == Some("vm.lead--b"))
            .map(|(_, row)| {
                row["doc"]["task"]["title"]
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
        owner.publish_facts(body.as_bytes()).expect("publish");
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
        owner.publish_facts(body.as_bytes()).expect("publish again");
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
            .publish_facts(body.as_bytes())
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
            held.rows
                .iter()
                .any(|(_, row)| row["doc"]["id"].as_str() == Some("vm.lead--new-tie")),
            "the new run landed: {:?}",
            held.rows
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
        assert!(
            has_bench(&held),
            "the detached scan landed: {:?}",
            held.rows
        );
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
    #[tokio::test]
    async fn a_real_size_share_answers_without_the_inputs_body() {
        let now = large_now();
        let root = tempfile::tempdir().expect("reports");
        let dir = root.path().join("v1/writers/vm.lead/runs/2026-11-04");
        std::fs::create_dir_all(&dir).expect("run folder");
        let base: serde_json::Value = serde_json::from_str(BENCH).expect("bench doc");
        for i in 0..9_000 {
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
            std::fs::write(dir.join(format!("vm.lead--bench{i}.json")), doc.to_string())
                .expect("run file");
        }
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
}
