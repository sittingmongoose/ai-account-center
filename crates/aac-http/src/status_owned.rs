//! R4 (DESIGN-M3-COMPLETE 1.2): the owned Status readers. The service scans the agent-reports
//! share itself — reports, the quota report, status documents every 10 s, the pm-mail registry
//! every 30 s — on the bounded blocking pool, single-flight per pass. Report captures go into
//! the hosted store in process; the readers-check rows are written through the registry's
//! leased writer when the R4 groups own that row. The Status producer shares these snapshots,
//! and `GET /internal/status/inputs` can project their JSON on demand. Scan clocks can
//! advance without changing the content generation or sending a `status-changed` event.

use crate::feed::Feeds;
use crate::store_host::StoreHost;
use aac_files::registry::{Owners, Role};
use aac_status::share::NodeFs;
use aac_status::share::check::ReadersCheckWriter;
use aac_status::share::quota::{self, QuotaReportReader};
use aac_status::share::registry::PmMailRegistryReader;
use aac_status::share::reports::{self, AgentReportsReader, AgentReportsState, Limits};
use aac_status::share::status_docs::{self, StatusDocsReader, StatusDocsState};
use indexmap::IndexMap;
use serde_json::{Value as Json, json};
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
    reports_rows: Option<Arc<AgentReportsState>>,
    quota: Arc<Json>,
    registry: Arc<Json>,
    docs_rows: Option<Arc<StatusDocsState>>,
    times: ScanTimes,
}

/// G16c: one pass's snapshots for the producers. The front door's `GET
/// /internal/status/inputs` still reads the JSON lazily from the held rows; the producers read these.
#[cfg(test)]
pub(crate) struct Scan {
    pub reports: AgentReportsState,
    pub docs: StatusDocsState,
    pub quota: Json,
    pub registry: Json,
}

/// One pass's outcome: the shared typed snapshots the check writer rows and producers read.
struct Outcome {
    changed: bool,
    times: ScanTimes,
    reports: Arc<AgentReportsState>,
    docs: Arc<StatusDocsState>,
    quota: Arc<Json>,
    registry: Arc<Json>,
}

/// Freshness is independent of a content generation. An old published snapshot keeps
/// its own clock if a later scan observes new rows.
#[derive(Clone, Default)]
pub(crate) struct ScanTimes {
    pub(crate) reports: Arc<AtomicU64>,
    pub(crate) registry: Arc<AtomicU64>,
}

pub(crate) struct StatusOwned {
    readers: Mutex<Readers>,
    check: Arc<Mutex<ReadersCheckWriter>>,
    state: Mutex<OwnedState>,
    /// Accepted report revisions, by `[kind, writer, objectId]` text (the DB deduplicates
    /// across restarts; this map only skips the calls).
    accepted: Mutex<IndexMap<String, String>>,
    store: Option<Arc<StoreHost>>,
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
        store: Option<Arc<StoreHost>>,
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
                registry: Arc::new(json!({"data": null, "asOf": null, "error": null})),
                reports_rows: None,
                quota: Arc::new(Json::Null),
                docs_rows: None,
                generation: 0,
                times: ScanTimes::default(),
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

    /// Scan on the blocking pool, replacing each shared snapshot only on content changes.
    fn scan_blocking(&self, now: u64, registry_due: bool) -> Option<Outcome> {
        let fs = NodeFs;
        let mut readers = self.readers.lock().unwrap_or_else(|p| p.into_inner());
        let reports_changed = readers.reports.scan(&fs, now);
        let quota_changed = readers.quota.scan(&fs, now);
        let docs_changed = readers.docs.scan(&fs, now);
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let first = state.reports_rows.is_none();
        let reports_changed = reports_changed
            || state
                .reports_rows
                .as_ref()
                .is_some_and(|held| held.complete != readers.reports.complete());

        let mut registry_changed = false;
        if registry_due {
            match readers.registry.scan_changes(&fs, now) {
                Ok(changed) => {
                    registry_changed = changed
                        || !state.registry["error"].is_null()
                        || state.registry["asOf"].is_null();
                    if registry_changed {
                        let scan = readers.registry.snapshot(changed);
                        state.registry = Arc::new(json!({
                            "data": scan.threads,
                            "asOf": now,
                            "error": null,
                            "problems": scan.problems.iter().map(|p| json!({"item": p.item, "reason": p.reason, "since": p.since})).collect::<Vec<_>>(),
                        }));
                        state.times.registry = Arc::new(AtomicU64::new(now));
                    } else {
                        state.times.registry.store(now, Ordering::Release);
                    }
                }
                Err(reason) => {
                    if state.registry["error"]["reason"].as_str() != Some(reason) {
                        let mut failed = failed_registry(&state.registry, reason, now);
                        let last_good = state.times.registry.load(Ordering::Acquire);
                        if last_good != 0 {
                            failed["asOf"] = json!(last_good);
                        }
                        state.registry = Arc::new(failed);
                        registry_changed = true;
                    }
                }
            }
        }
        let stamp = readers.reports.scanned_at().unwrap_or(0);
        if first || reports_changed {
            state.reports_rows = Some(Arc::new(readers.reports.snapshot()));
            state.times.reports = Arc::new(AtomicU64::new(stamp));
        } else {
            state.times.reports.store(stamp, Ordering::Release);
        }
        if first || docs_changed {
            state.docs_rows = Some(Arc::new(readers.docs.snapshot()));
        }
        if first || quota_changed {
            state.quota = Arc::new(quota::state_json(&readers.quota.snapshot()));
        }
        let changed = reports_changed || quota_changed || docs_changed || registry_changed;
        if !first && !changed {
            return None;
        }
        Some(Outcome {
            changed,
            times: state.times.clone(),
            reports: state.reports_rows.clone()?,
            docs: state.docs_rows.clone()?,
            quota: state.quota.clone(),
            registry: state.registry.clone(),
        })
    }

    /// One full pass: scan, hold shared Arc snapshots and tell the front door. Returns `None`
    /// on quiet unchanged ticks so neither producers nor capture/check writers churn at idle.
    async fn scan_pass(self: &Arc<Self>, registry_due: bool) -> Option<(Outcome, u64)> {
        let now = now_ms();
        let owner = Arc::clone(self);
        let outcome = tokio::task::spawn_blocking(move || owner.scan_blocking(now, registry_due))
            .await
            .unwrap_or(None)?;
        let generation = self.next_generation(now);
        self.state
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .generation = generation;
        if outcome.changed {
            self.feeds.publish_event(json!({
                "event": "status-changed",
                "generation": generation,
            }));
        }
        Some((outcome, now))
    }

    /// Capture the scan's new revisions into the hosted store and flush the readers-check rows.
    async fn finish_pass(self: &Arc<Self>, outcome: Outcome, now: u64) {
        self.capture(&outcome.reports, &outcome.docs).await;
        let owner = Arc::clone(self);
        let reports = outcome.reports;
        let docs = outcome.docs;
        let _ = tokio::task::spawn_blocking(move || owner.flush_check(&reports, &docs, now)).await;
    }

    /// The snapshots the front door forwards (and its fallback composer reads), materialized
    /// lazily from the held shared rows only if this legacy endpoint is called.
    pub(crate) fn inputs(&self) -> Json {
        let (generation, reports_rows, quota, registry, docs_rows, times) = {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            (
                state.generation,
                state.reports_rows.clone(),
                state.quota.clone(),
                state.registry.clone(),
                state.docs_rows.clone(),
                state.times.clone(),
            )
        };
        let mut reports = reports_rows
            .as_deref()
            .map(reports::state_json)
            .unwrap_or(Json::Null);
        if reports.is_object() {
            let stamp = times.reports.load(Ordering::Acquire);
            reports["scannedAt"] = if stamp == 0 { Json::Null } else { json!(stamp) };
        }
        let mut registry = Json::clone(&registry);
        let stamp = times.registry.load(Ordering::Acquire);
        if stamp != 0 {
            registry["asOf"] = json!(stamp);
        }
        let docs = docs_rows
            .as_deref()
            .map(status_docs::state_json)
            .unwrap_or(Json::Null);
        json!({
            "generation": generation,
            "reports": reports,
            "quota": Json::clone(&quota),
            "registry": registry,
            "docs": docs,
        })
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
            .ok()
            .and_then(|generation| serde_json::from_str::<Json>(generation.data.get()).ok())
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

    /// Report captures into the hosted store (`StatusService.captureReportHistory`): check
    /// `self.accepted` BEFORE cloning any report doc, and send batches of 64 instead of 4.
    async fn capture(&self, reports: &AgentReportsState, docs: &StatusDocsState) {
        let Some(store) = &self.store else {
            return;
        };
        let (captures, keys) = {
            let accepted = self.accepted.lock().unwrap_or_else(|p| p.into_inner());
            let mut captures: Vec<Json> = Vec::new();
            let mut keys: Vec<String> = Vec::new();
            let mut push_if_new = |kind: &str,
                                   writer: &str,
                                   doc: &Json,
                                   written_at_ms: i64,
                                   first_seen_at: u64,
                                   sha: &str| {
                let object_id = if kind == "card" {
                    doc["id"].as_str().unwrap_or_default()
                } else {
                    writer
                };
                let key = json!([kind, writer, object_id]).to_string();
                if accepted.get(&key).map(String::as_str) == Some(sha) {
                    return;
                }
                let agent = doc
                    .get("agent")
                    .filter(|a| a.is_object())
                    .cloned()
                    .unwrap_or(json!({}));
                let machine = writer.split('.').next().unwrap_or_default();
                let t3_thread = doc
                    .get("t3Thread")
                    .filter(|v| !v.is_null())
                    .or_else(|| agent.get("t3Thread").filter(|v| !v.is_null()))
                    .cloned()
                    .unwrap_or(Json::Null);
                let project = doc
                    .get("project")
                    .filter(|v| v.is_string())
                    .map(|v| json!({"source": "agent-report", "id": v}));
                let stream = doc
                    .get("stream")
                    .filter(|v| v.is_string())
                    .map(|v| json!({"source": "agent-report", "id": v}));
                let mut doc = doc.clone();
                if let Some(object) = doc.as_object_mut() {
                    object.insert(
                        "schema".to_string(),
                        json!(format!("agent-reports/{kind}@1")),
                    );
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
            };
            for row in &reports.threads {
                push_if_new(
                    "thread",
                    &row.writer,
                    &row.doc,
                    row.written_at_ms,
                    row.first_seen_at,
                    &row.sha256,
                );
            }
            for row in &reports.tasks {
                push_if_new(
                    "tasks",
                    &row.writer,
                    &row.doc,
                    row.written_at_ms,
                    row.first_seen_at,
                    &row.sha256,
                );
            }
            for row in &reports.cards {
                push_if_new(
                    "card",
                    &row.writer,
                    &row.doc,
                    row.written_at_ms,
                    row.first_seen_at,
                    &row.sha256,
                );
            }
            for row in &docs.docs {
                let sha = row["sha256"].as_str().unwrap_or_default();
                if sha.is_empty() {
                    continue;
                }
                push_if_new(
                    "status",
                    row["writer"].as_str().unwrap_or_default(),
                    &row["doc"],
                    row["writtenAtMs"].as_i64().unwrap_or(0),
                    row["firstSeenAt"].as_u64().unwrap_or(0),
                    sha,
                );
            }
            (captures, keys)
        };
        const BATCH: usize = 64;
        let mut index = 0;
        while index < captures.len() {
            let end = captures.len().min(index + BATCH);
            let batch: Vec<Json> = captures[index..end].to_vec();
            let batch_keys: Vec<String> = keys[index..end].to_vec();
            let batch_shas: Vec<String> = batch
                .iter()
                .map(|row| row["sourceSha256"].as_str().unwrap_or_default().to_string())
                .collect();
            index = end;
            let args = Json::Array(batch.clone()).to_string();
            match store.call("captureReports".to_string(), args).await {
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
                        if store
                            .call(
                                "captureReports".to_string(),
                                Json::Array(vec![row.clone()]).to_string(),
                            )
                            .await
                            .is_ok()
                        {
                            acknowledge(
                                &self.accepted,
                                std::slice::from_ref(key),
                                std::slice::from_ref(&sha),
                            );
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
    out.insert(
        "error".to_string(),
        json!({"reason": reason, "since": since}),
    );
    out.insert("missing".to_string(), json!(missing));
    if let Some(problems) = before.get("problems") {
        out.insert("problems".to_string(), problems.clone());
    }
    Json::Object(out)
}

#[allow(dead_code)]
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
pub(crate) async fn jobs(
    owner: Arc<StatusOwned>,
    produce: Option<Arc<crate::status_produce::Produce>>,
    stop: Arc<std::sync::atomic::AtomicBool>,
) {
    let mut tick: u64 = 0;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let registry_due = tick.is_multiple_of(REGISTRY_EVERY_PASSES);
        if let Some((outcome, now)) = owner.scan_pass(registry_due).await {
            let emit_hint = tick == 0 || outcome.changed;
            if let Some(produce) = &produce {
                produce
                    .after_scan_shared(
                        Arc::clone(&outcome.reports),
                        Arc::clone(&outcome.docs),
                        outcome.quota.clone(),
                        outcome.registry.clone(),
                        emit_hint,
                        outcome.times.clone(),
                    )
                    .await;
            }
            if tick == 0 || outcome.changed {
                owner.finish_pass(outcome, now).await;
            }
        } else if registry_due && let Some(produce) = &produce {
            // A signed answer can arrive without a report revision. The answer owner
            // checks it independently and republishes only if the feed actually changes.
            produce.poll_answers();
        }
        tick = tick.wrapping_add(1);
        tokio::time::sleep(std::time::Duration::from_millis(SCAN_EVERY_MS)).await;
    }
}

/// Prepare the existing share's report folders and publish the package's JSON schemas.
pub(crate) async fn initialize_reports(root: std::path::PathBuf) {
    let _ = tokio::task::spawn_blocking(move || {
        use std::fs;
        if !root.is_dir() {
            return;
        }
        let v1 = root.join("v1");
        let _ = fs::create_dir(&v1);
        if !fs::symlink_metadata(&v1).is_ok_and(|meta| meta.is_dir()) {
            return;
        }
        for leaf in ["writers", "answers", "keys", "schemas", "catalog", "readers"] {
            let directory = v1.join(leaf);
            #[cfg(unix)]
            if leaf == "writers" {
                use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
                if fs::DirBuilder::new().mode(0o777).create(&directory).is_ok() {
                    let _ = fs::set_permissions(directory, fs::Permissions::from_mode(0o777));
                }
                continue;
            }
            let _ = fs::create_dir(directory);
        }
        let target = v1.join("schemas");
        if !fs::symlink_metadata(&target).is_ok_and(|meta| meta.is_dir()) {
            return;
        }
        let Some(package) = crate::agent_transport::package_root() else {
            return;
        };
        let Ok(files) = fs::read_dir(package.join("docs/agent-reports/schemas")) else {
            return;
        };
        for file in files.flatten() {
            let name = file.file_name();
            let Some((name, stem)) = name.to_str().and_then(|name| name.strip_suffix(".json").map(|stem| (name, stem))) else {
                continue;
            };
            if !stem.starts_with(|c: char| c.is_ascii_lowercase())
                || !stem.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            {
                continue;
            }
            let Ok(mut bytes) = fs::read_to_string(file.path()) else {
                continue;
            };
            let Ok(mut schema) = serde_json::from_str::<Json>(&bytes) else {
                continue;
            };
            let ids = aac_status::computers::machine_ids();
            let mut changed = false;
            if let Some(writer) = schema.pointer_mut("/properties/writer") {
                writer["pattern"] = json!(format!(
                    r"^(?!.*--)({})\.[a-z0-9][a-z0-9.-]{{0,62}}$",
                    ids.join("|")
                ));
                changed = true;
            }
            if let Some(machine) = schema.pointer_mut("/properties/agent/properties/machine") {
                machine["enum"] = Json::Array(ids.iter().map(|id| json!(id)).chain([Json::Null]).collect());
                changed = true;
            }
            if changed && let Ok(body) = serde_json::to_string_pretty(&schema) {
                bytes = body + "\n";
            }
            let policy = aac_files::read::ReadPolicy::capped(bytes.len() as u64);
            if matches!(aac_files::read::read(&target.join(name), &policy), aac_files::read::Read::Bytes(current, _) if current == bytes.as_bytes()) {
                continue;
            }
            let _ = aac_files::share::write_atomically(&target, name, &bytes, 0o644);
        }
    }).await;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The registry JSON is one shared copy: a pass that does not rescan the registry hands the
    /// producers the same Arc the held state keeps, instead of a deep clone per pass.
    #[test]
    fn a_quiet_pass_shares_the_registry_arc() {
        let root = tempfile::tempdir().expect("share");
        let reports_root = root.path().join("reports");
        let registry_dir = root.path().join("registry");
        std::fs::create_dir_all(&reports_root).expect("reports root");
        std::fs::create_dir_all(&registry_dir).expect("registry dir");
        let rows = aac_files::registry::rows().expect("registry rows");
        let owner = StatusOwned::new(
            reports_root.clone(),
            registry_dir,
            None,
            Feeds::new(),
            Owners::defaults(rows),
            Arc::new(Mutex::new(ReadersCheckWriter::new(reports_root))),
            Arc::new(AtomicBool::new(false)),
        );
        let first = owner
            .scan_blocking(1_790_000_000_000, true)
            .expect("first pass");
        assert!(owner.scan_blocking(1_790_000_010_000, true).is_none());
        let state = owner.state.lock().unwrap_or_else(|p| p.into_inner());
        assert!(Arc::ptr_eq(&state.registry, &first.registry));
        assert!(Arc::ptr_eq(
            state
                .reports_rows
                .as_ref()
                .expect("the first scan set the reports snapshot"),
            &first.reports
        ));
        assert_eq!(
            first.times.reports.load(Ordering::Acquire),
            1_790_000_010_000
        );
        assert_eq!(
            first.times.registry.load(Ordering::Acquire),
            1_790_000_010_000
        );
    }
}
