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
use aac_status::model_identity::{normalize_model, AliasTable, Model};
use aac_status::read_context::{Envelope, Presentation};
use aac_status::share::catalog::{self, CatalogReader};
use aac_files::registry::{Owners, Role};
use aac_status::share::check::{ReadersCheckWriter, RunRow};
use aac_status::share::Problem;
use aac_status::share::NodeFs;
use aac_status::validate;
use aac_views::decline::Decline;
use aac_views::js_json::Writer;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::{json, Map, Value as Json};
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

/// The request's filters and clock, as the front door parsed and read them.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Feed {
    now: i64,
    #[allow(dead_code)] // the protocol carries the clock words; the composer reads facts
    clock: String,
    #[allow(dead_code)]
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
    views: Mutex<IndexMap<String, CachedView>>,
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
            views: Mutex::new(IndexMap::new()),
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
    fn record_check(&self, runs: &Json) {
        if self.owners.owner("readers-check") != Some(Role::RustService) {
            return;
        }
        let empty: Vec<Json> = Vec::new();
        let source = runs["accepted"]
            .as_array()
            .unwrap_or_else(|| runs["runs"].as_array().unwrap_or(&empty));
        let rows: Vec<RunRow> = source
            .iter()
            .map(|row| RunRow {
                file: row["file"].as_str().unwrap_or_default().to_string(),
                seq: row["doc"]["seq"].as_u64().unwrap_or(0),
                sha256: row["sha256"].as_str().unwrap_or_default().to_string(),
                first_seen_at: row["firstSeenAt"].as_u64().unwrap_or(0),
            })
            .collect();
        let problems: Vec<Problem> = runs["problems"]
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
            .unwrap_or_default();
        {
            let mut check = self.check.lock().unwrap_or_else(|p| p.into_inner());
            check.record_runs(&rows, &problems);
        }
        if self.flushing.swap(true, Ordering::Relaxed) {
            return;
        }
        let check = Arc::clone(&self.check);
        let owners = self.owners.clone();
        let flushing = Arc::clone(&self.flushing);
        tokio::task::spawn_blocking(move || {
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
        let full = force_full
            || now.saturating_sub(self.last_full_at.load(Ordering::Relaxed)) >= FULL_MS;
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
        let (files, refused, threads, read_count) = {
            let mut safe_paths = self.safe_paths.lock().unwrap_or_else(|p| p.into_inner());
            let mut remember = |path: &str| {
                if aac_status::secret_policy::find_secret(path).is_none() {
                    safe_paths.insert(path_hash(path), path.to_string());
                }
            };
            let mut files: Vec<Json> = Vec::new();
            let mut refused: Vec<Json> = scan["refused"]
                .as_array()
                .cloned()
                .unwrap_or_default();
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
        let record = self
            .store
            .call(
                "runsRecord".to_string(),
                json!({
                    "files": files,
                    "refused": refused,
                    "threads": Json::Object(threads),
                    "catalog": {
                        "models": catalog_text(&scan["catalog"]["models"]),
                        "aliases": catalog_text(&scan["catalog"]["aliases"]),
                    },
                    "nowMs": now,
                })
                .to_string(),
            )
            .await;
        let record: Json = match record {
            Ok(text) => serde_json::from_str(&text).unwrap_or(Json::Null),
            Err(_) => return,
        };
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
        let generation = record["generation"].as_u64().unwrap_or(0);
        let catalog_hash = record["catalogHash"].as_str().unwrap_or_default().to_string();
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
    async fn read(&self, query: &Json, clock: Option<u64>) -> Result<CompletedRead, Decline> {
        let signature = json!([
            query["fromMs"], query["toMs"], query["source"], query["taskType"],
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
            let page: Json = self
                .store
                .call("runsQuery".to_string(), args.to_string())
                .await
                .map_err(|_| Decline::Internal)
                .and_then(|text| serde_json::from_str(&text).map_err(|_| Decline::Internal))?;
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
        let mut full_query = query.clone();
        full_query["includeReceipts"] = json!(true);
        let read = self.read(&full_query, clock).await?;
        let refused: Vec<Json> = self
            .store
            .call("runsRefused".to_string(), "{}".to_string())
            .await
            .ok()
            .and_then(|text| serde_json::from_str::<Json>(&text).ok())
            .and_then(|value| value.as_array().cloned())
            .unwrap_or_default();
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
        problems.extend(read.problems.iter().cloned());
        drop(safe_paths);
        let mut writers = std::collections::HashSet::new();
        let runs: Vec<Json> = read
            .runs
            .iter()
            .map(|run| {
                writers.insert(run["doc"]["writer"].as_str().unwrap_or_default().to_string());
                json!({
                    "file": run["file"],
                    "doc": run["doc"],
                    "firstSeenAt": run["firstSeenAt"],
                    "sha256": run["sha256"],
                })
            })
            .collect();
        Ok(json!({
            "correctionsResolved": true,
            "accepted": read.accepted,
            "state": if pass.state.is_empty() { "ok" } else { pass.state.as_str() },
            "scannedAt": read.at,
            "runs": runs,
            "writers": writers.len(),
            "skipped": {"runFiles": 0, "writerFolders": 0},
            "problems": problems,
        }))
    }

    /// The envelope's `docs`: each run's display model resolution over the held catalog.
    fn normalized_docs(&self, runs: &Json, catalog: &Json) -> Vec<Json> {
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
        runs["runs"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        let doc = &row["doc"];
                        let id = doc["model"]["id"].as_str().unwrap_or_default();
                        if id.is_empty() {
                            return doc.clone();
                        }
                        let resolved = normalize_model(
                            id,
                            doc["model"]["effort"].as_str(),
                            &models,
                            Some(&table),
                        );
                        let mut doc = doc.clone();
                        doc["model"]["id"] = json!(resolved.id);
                        doc["model"]["effort"] = json!(resolved.effort);
                        doc["model"]["provider"] = Json::Null;
                        doc
                    })
                    .collect()
            })
            .unwrap_or_default()
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
        let presentation: Presentation =
            serde_json::from_str(facts.facts.get()).map_err(|_| Decline::Shape)?;
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
        let cache_key = json!([feed.from, feed.to, feed.source, feed.task_type, generation, catalog_hash]).to_string();
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
        let runs = self.runs_state(&query, Some(now)).await?;
        self.record_check(&runs);
        let join_reviews = if filtered {
            let mut reviews_query = Map::new();
            if let Some(from_ms) = feed.from_ms {
                reviews_query.insert("fromMs".to_string(), json!(from_ms));
            }
            reviews_query.insert("reviewsOnly".to_string(), json!(true));
            self.read(&Json::Object(reviews_query), Some(now))
                .await?
                .runs
                .iter()
                .map(|run| run["doc"].clone())
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
        let docs = self.normalized_docs(&runs, &catalog);
        let identity: Json = self
            .store
            .call("runsIdentity".to_string(), "{}".to_string())
            .await
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or(Json::Null);
        let status_runs: Json = self
            .store
            .call("runsStatus".to_string(), "{}".to_string())
            .await
            .ok()
            .and_then(|text| serde_json::from_str::<Json>(&text).ok())
            .map(|status| status["runs"].clone())
            .unwrap_or(Json::Null);
        let task_types: Vec<Json> = catalog["taskTypes"]
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| json!({"id": row["id"], "label": row["label"]}))
                    .collect()
            })
            .unwrap_or_default();
        let importing = self
            .pass
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .more;
        let seed_failures = self
            .seed_failures
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone();
        let data = json!({
            "now": feed.now,
            "timeZone": feed.time_zone,
            "root": self.root.to_string_lossy(),
            "catalog": catalog,
            "runs": runs,
            "joinReviews": join_reviews,
            "seedFailures": seed_failures,
            "docs": docs,
            "finish": {
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
            },
        });
        let view = aac_status::read_scoreboard::compose(Envelope {
            data,
            presentation,
        })
        .ok_or(Decline::Unsupported)?;
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

/// The ingest loop: a pass now, again within a second while a backfill has more, then every
/// two minutes; single-flight (the loop awaits each pass), until the service ends.
pub(crate) async fn ingest_jobs(owner: Arc<ScoreboardOwned>, stop: Arc<std::sync::atomic::AtomicBool>) {
    owner.ingest_pass(false).await;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let more = owner
            .pass
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .more;
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
pub(crate) async fn catalog_jobs(owner: Arc<ScoreboardOwned>, stop: Arc<std::sync::atomic::AtomicBool>) {
    owner.catalog_pass(now_ms());
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(30_000)).await;
        if stop.load(Ordering::Acquire) {
            return;
        }
        owner.catalog_pass(now_ms());
    }
}
