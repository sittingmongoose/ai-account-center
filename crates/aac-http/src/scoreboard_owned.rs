//! R4 (DESIGN-M3-COMPLETE 1.4): the owned Scoreboard. The service scans the catalog every
//! 30 s, ingests the share's runs into the hosted store (a pass every two minutes, every
//! second while a backfill has more, a full pass every fifteen minutes), and answers
//! `GET /api/scoreboard` from the store in process: the paged snapshot read with its restart
//! budget, the catalog it holds, the display model resolution, and the existing composer
//! (`read_scoreboard::compose`, by-model and groups in process). The view cache keeps the
//! composed view per (filters, generation) for 60 s, as the TypeScript service does; a hit
//! fills only `cacheAgeSeconds`. The front door forwards the filters, the clock and the
//! facts generation; anything unproven declines and today's handler answers.

use crate::feed::Feeds;
use crate::store_host::StoreHost;
use aac_files::registry::{Owners, Role};
use aac_status::model_identity::{AliasTable, Model, normalize_model};
use aac_status::read_context::{Date, Presentation};
use aac_status::share::NodeFs;
use aac_status::share::Problem;
use aac_status::share::catalog::{self, CatalogReader};
use aac_status::share::check::{ReadersCheckWriter, RunRow};
use aac_status::validate;
use aac_views::decline::Decline;
use aac_views::js_json::Writer;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// A read within this long of the last view gets that view (the TS service's cache time).
const CACHE_MS: u64 = 60_000;
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
    /// Whether the row or its doc holds a prototype name (`read_context::proto_named`): a
    /// filter that picks one declines, as the envelope path does.
    proto: Vec<bool>,
    /// What compose reads of each row (`read_scoreboard::row_facts`).
    facts: Vec<aac_status::read_scoreboard::RowFacts>,
    /// Each doc's position in the newest order, per facts generation id (None: not provable).
    order: Mutex<Option<(String, Option<Arc<Vec<usize>>>)>>,
}

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
        }
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
    generation: u64,
    catalog_hash: String,
    facts: String,
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

struct CachedView {
    at: u64,
    view: Json,
    etag: String,
}

pub(crate) struct ScoreboardOwned {
    root: PathBuf,
    store: Arc<StoreHost>,
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
    /// The last pushed facts generation, parsed once: (its id, its presentation).
    parsed_facts: Mutex<Option<(String, std::sync::Arc<Presentation>)>>,
    /// The view-cache keys being composed now: one compose per key at a time.
    composing: Mutex<std::collections::HashSet<String>>,
    last_full_at: AtomicU64,
    /// The readers-check rows this owner shares with the Status owner (one file per writer).
    check: Arc<Mutex<ReadersCheckWriter>>,
    owners: Owners,
    flushing: Arc<AtomicBool>,
}

impl ScoreboardOwned {
    pub(crate) fn new(
        root: PathBuf,
        store: Arc<StoreHost>,
        feeds: Feeds,
        check: Arc<Mutex<ReadersCheckWriter>>,
        owners: Owners,
        flushing: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
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
            parsed_facts: Mutex::new(None),
            composing: Mutex::new(std::collections::HashSet::new()),
            last_full_at: AtomicU64::new(0),
            check,
            owners,
            flushing,
        })
    }

    pub(crate) fn set_seed_failures(&self, failures: Vec<Json>) {
        *self.seed_failures.lock().unwrap_or_else(|p| p.into_inner()) = failures;
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

    pub(crate) fn generation(&self) -> (u64, String) {
        let pass = self.pass.lock().unwrap_or_else(|p| p.into_inner());
        (pass.generation, pass.catalog_hash.clone())
    }

    /// One ingest pass (`RunsStore.pass`): the store scans the share, this side applies the
    /// one secret policy between scan and record exactly as the TypeScript shuttle does,
    /// and the store records what passes.
    pub(crate) async fn ingest_pass(&self, force_full: bool) {
        let now = now_ms();
        let full =
            force_full || now.saturating_sub(self.last_full_at.load(Ordering::Relaxed)) >= FULL_MS;
        let scan = match self
            .store
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
        let record = self
            .store
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
    /// The held unfiltered snapshot for the owner's current (generation, catalog hash), read
    /// once from the store when the generation moved (see [`HeldRuns`]).
    async fn held(&self, clock: Option<u64>) -> Result<Arc<HeldRuns>, Decline> {
        let key = self.generation();
        let held = self
            .held_runs
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .filter(|held| held.key == key);
        if let Some(held) = held {
            return Ok(held);
        }
        // One unfiltered read of the snapshot, with the rows' cursors and the receipts.
        let full = self
            .read_store(
                &json!({"includeReceipts": true, "includeCursors": true}),
                clock,
            )
            .await?;
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
        }
        Ok(held)
    }

    /// The store's paged read of one query (see [`Self::read`] for the held snapshot).
    async fn read_store(&self, query: &Json, clock: Option<u64>) -> Result<CompletedRead, Decline> {
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
            let text = self
                .store
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

    /// The runs as the view builder reads them (`RunsStore.state`), from the store.
    async fn runs_state(&self, query: &Json, clock: Option<u64>) -> Result<Json, Decline> {
        let held = self.held(clock).await?;
        let picked = held.pick(query);
        self.state_of(&held, &picked, clock.unwrap_or_else(now_ms))
            .await
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
        if self.store_epoch.load(Ordering::SeqCst) == epoch && epoch % 2 == 0 {
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
        let mut problems: Vec<Json> = refused
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
            .collect();
        problems.extend(pass.problems.iter().cloned());
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
        let state = if pass.state.is_empty() {
            "ok"
        } else {
            pass.state.as_str()
        };
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
        out.insert("scannedAt".to_string(), json!(clock));
        out.insert("runs".to_string(), Json::Array(runs));
        out.insert("writers".to_string(), json!(writers));
        out.insert(
            "skipped".to_string(),
            json!({"runFiles": 0, "writerFolders": 0}),
        );
        out.insert("problems".to_string(), Json::Array(problems));
        Ok(Json::Object(out))
    }

    /// The envelope's `docs`: each run's display model resolution over the held catalog.
    fn normalized_docs(&self, runs: &Json, catalog: &Json) -> Vec<Json> {
        let docs: Vec<&Json> = runs["runs"]
            .as_array()
            .map(|rows| rows.iter().map(|row| &row["doc"]).collect())
            .unwrap_or_default();
        self.normalized_doc_list(docs.into_iter(), catalog)
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
                aac_status::read_context::proto_named(row)
                    || aac_status::read_context::proto_named(doc)
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

    /// The owned answer: the final bytes and ETag, or a named decline.
    pub(crate) async fn answer(&self, feed: &str) -> Result<(Vec<u8>, String), Decline> {
        let feed: Feed = crate::json_object::from_slice(
            feed.as_bytes(),
            &[
                "now",
                "clock",
                "day",
                "timeZone",
                "from",
                "to",
                "source",
                "taskType",
                "fromMs",
                "toMs",
                "generation",
                "catalogHash",
                "facts",
            ],
        )
        .map_err(|_| Decline::Shape)?;
        let (generation, catalog_hash) = self.generation();
        if feed.generation != generation || feed.catalog_hash != catalog_hash {
            return Err(Decline::Stale);
        }
        let facts = self
            .feeds
            .current("scoreboard-facts", &feed.facts)
            .map_err(|_| Decline::Stale)?;
        let filtered = feed.from.is_some()
            || feed.to.is_some()
            || feed.source != "all"
            || feed.task_type.is_some();
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
        let cache_key = json!([
            feed.from,
            feed.to,
            feed.source,
            feed.task_type,
            generation,
            catalog_hash
        ])
        .to_string();
        let now = feed.now.max(0) as u64;
        // The 60 s view cache: a hit fills only cacheAgeSeconds, as the TS service does.
        {
            let views = self.views.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(hit) = views.get(&cache_key) {
                if now.saturating_sub(hit.at) < CACHE_MS {
                    let mut view = hit.view.clone();
                    view["cacheAgeSeconds"] = json!((now.saturating_sub(hit.at)) as f64 / 1000.0);
                    let mut writer = Writer::with_capacity(64 * 1024);
                    writer.value(&view);
                    let (bytes, _) = writer.finish();
                    return Ok((bytes, hit.etag.clone()));
                }
            }
        }
        // One compose per view-cache key: a request that finds its key being composed (its
        // first request outlived the budget and still runs) is `busy`; today's handler answers.
        let _composing = {
            let mut composing = self.composing.lock().unwrap_or_else(|p| p.into_inner());
            if !composing.insert(cache_key.clone()) {
                return Err(Decline::Busy);
            }
            Composing {
                set: &self.composing,
                key: cache_key.clone(),
            }
        };
        // The facts generation is pushed once per runs generation and shared by its requests;
        // parsed once per id. The request's own `now` comes from its header words.
        let presentation = {
            let mut held = self.parsed_facts.lock().unwrap_or_else(|p| p.into_inner());
            match held.as_ref() {
                Some((id, parsed)) if *id == feed.facts => parsed.clone(),
                _ => {
                    let parsed: Presentation =
                        serde_json::from_str(facts.facts.get()).map_err(|_| Decline::Shape)?;
                    let parsed = std::sync::Arc::new(parsed);
                    *held = Some((feed.facts.clone(), parsed.clone()));
                    parsed
                }
            }
        };
        let now_date =
            Date::of_now(feed.now, feed.clock.clone(), feed.day.clone()).ok_or(Decline::Shape)?;
        // The held snapshot answers every filter: the store is read once per generation.
        let held = self.held(Some(now)).await?;
        let picked = held.pick(&query);
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
        let catalog = self
            .catalog_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        if catalog.is_null() {
            return Err(Decline::Stale);
        }
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
        let positions = prepared.positions(&feed.facts, &presentation);
        let order: Option<Vec<usize>> =
            positions.map(|positions| picked.iter().map(|&index| positions[index]).collect());
        let store_facts = self.store_facts().await;
        let identity = store_facts.identity.clone();
        let status_runs = store_facts.status_runs.clone();
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
                    "from": feed.from,
                    "to": feed.to,
                    "source": feed.source,
                    "taskType": feed.task_type,
                    "available": true,
                    "taskTypes": task_types,
                },
                "identity": identity,
                "history": {"store": "on", "runs": status_runs, "importing": importing},
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
                    view,
                    etag: etag.clone(),
                },
            );
            while views.len() > CACHE_ENTRIES {
                views.shift_remove_index(0);
            }
        }
        Ok((bytes, etag))
    }

    /// The input the front door computes the facts over: the unfiltered runs state (a
    /// superset of every filter's rows), the catalog, the normalized docs and the finish
    /// parts, at the current generation.
    pub(crate) async fn inputs(&self) -> Result<Json, Decline> {
        let (generation, catalog_hash) = self.generation();
        let runs = self.runs_state(&json!({}), None).await?;
        let catalog = self
            .catalog_state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let docs = self.normalized_docs(&runs, &catalog);
        let identity: Json = self
            .store
            .call("runsIdentity".to_string(), "{}".to_string())
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Json::Null);
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
        }))
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
) {
    owner.ingest_pass(false).await;
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
) {
    owner.catalog_pass(now_ms());
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
