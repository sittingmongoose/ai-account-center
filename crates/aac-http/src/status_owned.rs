//! R4 (DESIGN-M3-COMPLETE 1.2): the owned Status readers. The service scans the agent-reports
//! share itself — reports, the quota report, status documents every 10 s, the pm-mail registry
//! every 30 s — on the bounded blocking pool, single-flight per pass. Report captures go into
//! the hosted store in process; the readers-check rows are written through the registry's
//! leased writer when the R4 groups own that row. The front door reads the snapshots through
//! `GET /internal/status/inputs`, pushes them as the fed Status generations (so the request
//! path, the composer and its cache stay the proven R2 ones) and answers the facts the
//! composer needs. A `status-changed` event carries each new generation.

use crate::feed::Feeds;
use crate::store_host::StoreHost;
use aac_files::registry::{Owners, Role};
use aac_status::share::check::ReadersCheckWriter;
use aac_status::share::quota::{self, QuotaReportReader, QuotaReportState};
use aac_status::share::registry::PmMailRegistryReader;
use aac_status::share::reports::{self, AgentReportsReader, AgentReportsState, Limits};
use aac_status::share::status_docs::{self, StatusDocsReader, StatusDocsState};
use aac_status::share::NodeFs;
use indexmap::IndexMap;
use serde_json::{json, Value as Json};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The share scan cadence; the registry rides every third pass (30 s).
pub(crate) const SCAN_EVERY_MS: u64 = 10_000;
const REGISTRY_EVERY_PASSES: u64 = 3;

struct Readers {
    reports: AgentReportsReader,
    quota: QuotaReportReader,
    registry: PmMailRegistryReader,
    docs: StatusDocsReader,
}

#[derive(Default)]
struct OwnedState {
    generation: u64,
    reports: Json,
    quota: Json,
    registry: Json,
    docs: Json,
}

/// One pass's outcome: the typed snapshots the check writer rows, and the JSON the front
/// door forwards.
struct Outcome {
    changed: bool,
    reports: AgentReportsState,
    docs: StatusDocsState,
    quota: QuotaReportState,
    registry: Json,
}

pub(crate) struct StatusOwned {
    readers: Mutex<Readers>,
    check: Arc<Mutex<ReadersCheckWriter>>,
    state: Mutex<OwnedState>,
    /// Accepted report revisions, by `[kind, writer, objectId]` text (the DB deduplicates
    /// across restarts; this map only skips the calls).
    accepted: Mutex<IndexMap<String, String>>,
    store: Arc<StoreHost>,
    feeds: Feeds,
    owners: Owners,
    flushing: Arc<AtomicBool>,
    generation_clock: AtomicU64,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

impl StatusOwned {
    pub(crate) fn new(
        reports_root: PathBuf,
        registry_dir: PathBuf,
        store: Arc<StoreHost>,
        feeds: Feeds,
        owners: Owners,
        check: Arc<Mutex<ReadersCheckWriter>>,
        flushing: Arc<AtomicBool>,
    ) -> Arc<Self> {
        Arc::new(Self {
            check,
            flushing,
            readers: Mutex::new(Readers {
                reports: AgentReportsReader::new(reports_root.clone(), Limits::default()),
                quota: QuotaReportReader::new(reports_root.clone()),
                registry: PmMailRegistryReader::new(registry_dir),
                docs: StatusDocsReader::new(reports_root, status_docs::STATUS_DOCS_LIMITS),
            }),
            state: Mutex::new(OwnedState {
                registry: json!({"data": null, "asOf": null, "error": null}),
                reports: Json::Null,
                quota: Json::Null,
                docs: Json::Null,
                generation: 0,
            }),
            accepted: Mutex::new(IndexMap::new()),
            store,
            feeds,
            owners,
            generation_clock: AtomicU64::new(0),
        })
    }

    fn next_generation(&self, now: u64) -> u64 {
        let mut last = self.generation_clock.load(Ordering::Relaxed);
        loop {
            let next = (last + 1).max(now.saturating_mul(1000));
            match self.generation_clock.compare_exchange(
                last,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(held) => last = held,
            }
        }
    }

    /// One scan pass on the blocking pool: the four readers, the registry's collected wrapper.
    fn scan_blocking(&self, now: u64, registry_due: bool) -> Outcome {
        let fs = NodeFs;
        let mut readers = self.readers.lock().unwrap_or_else(|p| p.into_inner());
        let mut changed = readers.reports.scan(&fs, now);
        if readers.quota.scan(&fs, now) {
            changed = true;
        }
        if readers.docs.scan(&fs, now) {
            changed = true;
        }
        let reports = readers.reports.snapshot();
        let docs = readers.docs.snapshot();
        let quota = readers.quota.snapshot();
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let registry = if registry_due {
            match readers.registry.scan(&fs, now) {
                Ok(scan) => {
                    if scan.changed {
                        changed = true;
                    }
                    json!({
                        "data": scan.threads,
                        "asOf": now,
                        "error": null,
                        "problems": scan.problems.iter().map(|p| json!({"item": p.item, "reason": p.reason, "since": p.since})).collect::<Vec<_>>(),
                    })
                }
                Err(reason) => {
                    changed = true;
                    failed_registry(&state.registry, reason, now)
                }
            }
        } else {
            state.registry.clone()
        };
        state.registry = registry.clone();
        drop(state);
        Outcome {
            changed,
            reports,
            docs,
            quota,
            registry,
        }
    }

    /// One full pass: scan, capture the new revisions into the hosted store, flush the
    /// readers-check rows, hold the snapshots and tell the front door.
    pub(crate) async fn pass(self: &Arc<Self>, registry_due: bool) {
        let now = now_ms();
        let owner = Arc::clone(self);
        let outcome = tokio::task::spawn_blocking(move || owner.scan_blocking(now, registry_due))
            .await
            .unwrap_or_else(|_| Outcome {
                changed: false,
                reports: empty_reports(),
                docs: StatusDocsState {
                    state: "missing",
                    scanned_at: None,
                    docs: Vec::new(),
                    problems: Vec::new(),
                },
                quota: QuotaReportState {
                    state: "missing",
                    scanned_at: None,
                    good: None,
                    problem: None,
                },
                registry: json!({"data": null, "asOf": null, "error": null}),
            });
        self.capture(&outcome.reports, &outcome.docs).await;
        let owner = Arc::clone(self);
        let reports = outcome.reports.clone();
        let docs = outcome.docs.clone();
        let _ = tokio::task::spawn_blocking(move || owner.flush_check(&reports, &docs, now)).await;
        let generation = self.next_generation(now);
        {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.generation = generation;
            state.reports = reports::state_json(&outcome.reports);
            state.quota = quota::state_json(&outcome.quota);
            state.docs = status_docs::state_json(&outcome.docs);
            state.registry = outcome.registry;
        }
        if outcome.changed {
            self.feeds.publish_event(json!({
                "event": "status-changed",
                "generation": generation,
            }));
        }
    }

    /// The snapshots the front door forwards (and its fallback composer reads).
    pub(crate) fn inputs(&self) -> Json {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        json!({
            "generation": state.generation,
            "reports": state.reports,
            "quota": state.quota,
            "registry": state.registry,
            "docs": state.docs,
        })
    }

    pub(crate) fn generation(&self) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .generation
    }

    /// The readers-check rows, when the R4 groups own that row: the status scan's rows with
    /// the pins the front door last pushed (`owned-pins`), written through the leased writer.
    fn flush_check(&self, reports: &AgentReportsState, docs: &StatusDocsState, now: u64) {
        if self.owners.owner("readers-check") != Some(Role::RustService) {
            return;
        }
        let pins: Vec<(String, String)> = self
            .feeds
            .latest("owned-pins")
            .and_then(|generation| {
                serde_json::from_str::<Json>(generation.data.get()).ok()
            })
            .and_then(|value| {
                value.as_array().map(|rows| {
                    rows.iter()
                        .filter_map(|row| {
                            Some((
                                row["id"].as_str()?.to_string(),
                                row["title"].as_str()?.to_string(),
                            ))
                        })
                        .collect()
                })
            })
            .unwrap_or_default();
        let mut check = self.check.lock().unwrap_or_else(|p| p.into_inner());
        check.record_status_scan(reports, docs, &pins);
        if self.flushing.swap(true, Ordering::Relaxed) {
            return;
        }
        check.flush(&self.owners, now);
        self.flushing.store(false, Ordering::Relaxed);
    }

    /// Report captures into the hosted store (`StatusService.captureReportHistory`): the
    /// accepted revisions this server has not acknowledged yet, in batches of four; a policy
    /// refusal retries the batch's rows one by one, a storage outage stops the pass.
    async fn capture(&self, reports: &AgentReportsState, docs: &StatusDocsState) {
        let mut input: Vec<(String, String, Json, i64, u64, String)> = Vec::new(); // kind, writer, doc, writtenAtMs, firstSeenAt, sha
        for row in &reports.threads {
            input.push(("thread".into(), row.writer.clone(), row.doc.clone(), row.written_at_ms, row.first_seen_at, row.sha256.clone()));
        }
        for row in &reports.tasks {
            input.push(("tasks".into(), row.writer.clone(), row.doc.clone(), row.written_at_ms, row.first_seen_at, row.sha256.clone()));
        }
        for row in &reports.cards {
            input.push(("card".into(), row.writer.clone(), row.doc.clone(), row.written_at_ms, row.first_seen_at, row.sha256.clone()));
        }
        for row in &docs.docs {
            let sha = row["sha256"].as_str().unwrap_or_default();
            if sha.is_empty() {
                continue;
            }
            input.push((
                "status".into(),
                row["writer"].as_str().unwrap_or_default().to_string(),
                row["doc"].clone(),
                row["writtenAtMs"].as_i64().unwrap_or(0),
                row["firstSeenAt"].as_u64().unwrap_or(0),
                sha.to_string(),
            ));
        }
        let mut accepted = self.accepted.lock().unwrap_or_else(|p| p.into_inner());
        let mut captures: Vec<Json> = Vec::new();
        let mut keys: Vec<String> = Vec::new();
        for (kind, writer, doc, written_at_ms, first_seen_at, sha) in input {
            let object_id = if kind == "card" {
                doc["id"].as_str().unwrap_or_default().to_string()
            } else {
                writer.clone()
            };
            let key = json!([kind, writer, object_id]).to_string();
            if accepted.get(&key).map(String::as_str) == Some(sha.as_str()) {
                continue;
            }
            let agent = doc.get("agent").filter(|a| a.is_object()).cloned().unwrap_or(json!({}));
            let machine = writer.split('.').next().unwrap_or_default();
            let t3_thread = doc
                .get("t3Thread")
                .filter(|v| !v.is_null())
                .or_else(|| agent.get("t3Thread").filter(|v| !v.is_null()))
                .cloned()
                .unwrap_or(Json::Null);
            let project = doc.get("project").filter(|v| v.is_string()).map(|v| {
                json!({"source": "agent-report", "id": v})
            });
            let stream = doc.get("stream").filter(|v| v.is_string()).map(|v| {
                json!({"source": "agent-report", "id": v})
            });
            let mut doc = doc.clone();
            if let Some(object) = doc.as_object_mut() {
                object.insert("schema".to_string(), json!(format!("agent-reports/{kind}@1")));
            }
            captures.push(json!({
                "kind": kind,
                "writer": writer,
                "objectId": object_id,
                "seq": doc["seq"],
                "writtenAtMs": written_at_ms,
                "observedAtMs": first_seen_at,
                "sourceSha256": sha,
                "ids": {
                    "computer": if machine == "vm" { "ubuntu" } else { machine },
                    "writer": writer,
                    "t3ThreadId": t3_thread,
                    "t3EnvironmentId": null,
                    "t3TaskId": null,
                    "runId": null,
                    "cardId": if kind == "card" { doc["id"].clone() } else { Json::Null },
                    "projectId": project.unwrap_or(Json::Null),
                    "streamId": stream.unwrap_or(Json::Null),
                    "modelRaw": agent.get("model").filter(|v| !v.is_null()).cloned().unwrap_or(Json::Null),
                    "effort": agent.get("effort").filter(|v| !v.is_null()).cloned().unwrap_or(Json::Null),
                    "identitySource": "reported",
                },
                "doc": doc,
            }));
            keys.push(key);
        }
        drop(accepted);
        let mut index = 0;
        while index < captures.len() {
            let batch: Vec<Json> = captures[index..captures.len().min(index + 4)].to_vec();
            let batch_keys: Vec<String> = keys[index..captures.len().min(index + 4)].to_vec();
            let batch_shas: Vec<String> = batch
                .iter()
                .map(|row| row["sourceSha256"].as_str().unwrap_or_default().to_string())
                .collect();
            index += 4;
            let args = Json::Array(batch.clone()).to_string();
            match self.store.call("captureReports".to_string(), args).await {
                Ok(_) => acknowledge(&self.accepted, &batch_keys, &batch_shas),
                Err(error) => {
                    if error_code(&error) != "invalid" {
                        break; // A storage outage is different from a per-document refusal.
                    }
                    if batch.len() == 1 {
                        continue;
                    }
                    for (row, key) in batch.iter().zip(&batch_keys) {
                        let sha = row["sourceSha256"].as_str().unwrap_or_default().to_string();
                        if self
                            .store
                            .call("captureReports".to_string(), Json::Array(vec![row.clone()]).to_string())
                            .await
                            .is_ok()
                        {
                            acknowledge(&self.accepted, std::slice::from_ref(key), std::slice::from_ref(&sha));
                        }
                    }
                }
            }
        }
        let mut accepted = self.accepted.lock().unwrap_or_else(|p| p.into_inner());
        while accepted.len() > 5000 {
            accepted.shift_remove_index(0);
        }
    }
}

fn acknowledge(accepted: &Mutex<IndexMap<String, String>>, keys: &[String], shas: &[String]) {
    let mut accepted = accepted.lock().unwrap_or_else(|p| p.into_inner());
    for (key, sha) in keys.iter().zip(shas) {
        accepted.insert(key.clone(), sha.clone());
    }
}

/// A store error's fixed code, as TS's `storeErrorCode` reads it.
fn error_code(error: &str) -> &str {
    let code = error.split(": ").next().unwrap_or("");
    if !code.is_empty() && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        code
    } else {
        "unknown"
    }
}


/// The registry's collected wrapper on a failed scan: the last good rows with their own time,
/// the named error since it began, and whether the folder is missing.
fn failed_registry(before: &Json, reason: &str, now: u64) -> Json {
    let missing = reason.contains("missing");
    let since = before["error"]["reason"]
        .as_str()
        .filter(|held| *held == reason)
        .and(before["error"]["since"].as_u64())
        .unwrap_or(now);
    let mut out = serde_json::Map::new();
    out.insert("data".to_string(), before["data"].clone());
    out.insert("asOf".to_string(), before["asOf"].clone());
    out.insert("error".to_string(), json!({"reason": reason, "since": since}));
    out.insert("missing".to_string(), json!(missing));
    if let Some(problems) = before.get("problems") {
        out.insert("problems".to_string(), problems.clone());
    }
    Json::Object(out)
}

fn empty_reports() -> AgentReportsState {
    AgentReportsState {
        root: String::new(),
        state: "missing",
        scanned_at: None,
        complete: false,
        threads: Vec::new(),
        tasks: Vec::new(),
        cards: Vec::new(),
        problems: Vec::new(),
        boards: serde_json::Map::new(),
    }
}

/// The job loop: a pass every 10 s, the registry every third pass, single-flight (the loop
/// awaits each pass), until the service ends.
pub(crate) async fn jobs(owner: Arc<StatusOwned>, stop: Arc<std::sync::atomic::AtomicBool>) {
    let mut tick: u64 = 0;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        owner.pass(tick % REGISTRY_EVERY_PASSES == 0).await;
        tick = tick.wrapping_add(1);
        tokio::time::sleep(std::time::Duration::from_millis(SCAN_EVERY_MS)).await;
    }
}
