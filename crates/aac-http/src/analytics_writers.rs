//! The quota-history sampler and activity scheduler share the host's store Engine.
//!
//! The quota sampler coalesces both platforms' fed dashboards, imports the JSON history once, and
//! records the live readings on the account-refresh cadence. The activity scheduler calls lane E's
//! `scan-usage` verb per configured computer (local and remote), shapes the rows into reports and
//! ingests them on the converge cadence, keeping each source's state for the owned read.
//!
//! Registered agents discover their own standard and T3 roots. The dashboard computer's
//! PM-Experiments roots are supplied by the discovery job below. Only the saved
//! dashboard's additional roots need to travel with the scan request.
use crate::store_host::StoreHost;
use aac_store::activity;
use aac_store::collect::{self, Pricing, PricingResolution, Rates};
use aac_store::report::{self, Ingestor, ReportArgs};
use aac_store::sampler::{self, BackfillSource};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod analytics_jsonl;
mod experiment_roots;
mod experiment_signatures;
mod fixed_scanner;

/// The job's tick: the activity background's every-15s cadence; each writer checks its own deadline.
const TICK_MS: u64 = 15_000;
/// The first pass waits for the front door to feed its inputs.
const FIRST_PASS_MS: u64 = 20_000;
const DAY: i64 = 86_400_000;
/// The activity converge window and interval (`account-analytics-activity.ts`).
const CONVERGE_WINDOW_MS: i64 = 10 * 60_000;
const CONVERGE_INTERVAL_MS: i64 = 60_000;
/// One scan's budget and response cap (the transport's scan frame cap times two).
const SCAN_DEADLINE_MS: u64 = 120_000;
const SCAN_MAX_BYTES: usize = 16 * 1024 * 1024;
/// The archive walk's bounds (`usage-backfill.ts`).
const MAX_ARCHIVE_FILES: usize = 2000;
const MAX_ARCHIVE_BYTES: u64 = 64 * 1024 * 1024;

/// The writers' shared state across passes: the once-only backfill flag, the ingest trimmer, the
/// activity source states and cadence marks, and the last quota sample time.
#[derive(Default)]
struct WriterState {
    ingestor: Ingestor,
    states: Vec<Value>,
    fetched_at: i64,
    scanning_since: Option<i64>,
    generation: u64,
    experiments: Option<Arc<experiment_roots::Snapshot>>,
}

#[derive(Default)]
struct QuotaState {
    backfilled: bool,
    last_quota_ms: i64,
}

/// The collection aggregation's [`Pricing`] seam over the G15 Rust resolver: the same
/// `getModelPricingWithSource` lookup the TS collector uses, reading the registry cache the
/// scheduled `ModelsDevRefresh` keeps fresh. Interior mutability carries the resolver's memo.
struct ResolverPricing {
    resolver: Mutex<aac_views::pricing::Resolver>,
}

impl ResolverPricing {
    fn new(ccs_dir: &Path) -> Self {
        ResolverPricing {
            resolver: Mutex::new(aac_views::pricing::Resolver::new(ccs_dir)),
        }
    }
}

impl Pricing for ResolverPricing {
    fn resolve(&self, model: &str, provider: Option<&str>) -> PricingResolution {
        let mut resolver = self.resolver.lock().unwrap_or_else(|p| p.into_inner());
        let resolution = resolver.get_model_pricing_with_source(model, provider, None);
        let rates = resolution.pricing.rates;
        PricingResolution {
            rates: Rates {
                input: rates.input_per_million,
                output: rates.output_per_million,
                cache_creation: rates.cache_creation_per_million,
                cache_read: rates.cache_read_per_million,
            },
            source: resolution.source.as_str().to_string(),
        }
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `archiveFiles`: a bounded depth-first walk (depth ≤ 3, ≤ 2000 files), sorted by name.
fn archive_files(directory: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if depth > 3 || out.len() >= MAX_ARCHIVE_FILES {
        return;
    }
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
    paths.sort();
    for path in paths {
        if out.len() >= MAX_ARCHIVE_FILES {
            return;
        }
        let Ok(kind) = path.symlink_metadata().map(|meta| meta.file_type()) else {
            continue;
        };
        if kind.is_dir() {
            archive_files(&path, depth + 1, out);
        } else if kind.is_file() {
            out.push(path);
        }
    }
}

/// `readArchiveFile`: a file's bytes, gunzipped when `.gz`, bounded at 64 MiB.
fn read_archive_file(root: &Path, file: &Path) -> Option<Vec<u8>> {
    use aac_files::read::{Ancestors, Read, ReadPolicy};
    let policy = ReadPolicy::capped(MAX_ARCHIVE_BYTES).ancestors(Ancestors::NoLinkBelow(root));
    let Read::Bytes(bytes, _) = aac_files::read::read(file, &policy) else {
        return None;
    };
    if file.extension().and_then(|e| e.to_str()) == Some("gz") {
        use std::io::Read;
        let decoder = flate2::read::GzDecoder::new(&bytes[..]);
        let mut out = Vec::new();
        decoder
            .take(MAX_ARCHIVE_BYTES + 1)
            .read_to_end(&mut out)
            .ok()?;
        if out.len() as u64 > MAX_ARCHIVE_BYTES {
            return None;
        }
        Some(out)
    } else {
        Some(bytes)
    }
}

/// The backfill sources the host reads: the archive files plus the main history file.
fn backfill_sources(ccs_dir: &Path) -> (Vec<BackfillSource>, u64) {
    let base = ccs_dir.join("account-analytics");
    let archive = base.join("archive");
    let mut files = Vec::new();
    if [&base, &archive].iter().all(|path| {
        path.symlink_metadata()
            .is_ok_and(|meta| meta.file_type().is_dir())
    }) {
        archive_files(&archive, 0, &mut files);
    }
    let mut sources = Vec::new();
    let mut unreadable = 0u64;
    for file in files {
        let Some(bytes) = read_archive_file(ccs_dir, &file) else {
            unreadable += 1;
            continue;
        };
        let Ok(text) = std::str::from_utf8(&bytes) else {
            unreadable += 1;
            continue;
        };
        let (records, parsed) = sampler::records_from_text(text);
        if !parsed {
            unreadable += 1;
            continue;
        }
        let rel = file
            .strip_prefix(&archive)
            .unwrap_or(&file)
            .to_string_lossy()
            .replace('\\', "/");
        sources.push(BackfillSource {
            label: format!("archive/{rel}"),
            sha256: sha256_hex(&bytes),
            records,
        });
    }
    let main = base.join("quota-history-v1.json");
    if main.symlink_metadata().is_ok() {
        match read_archive_file(ccs_dir, &main) {
            Some(bytes) => {
                let (records, parsed) = std::str::from_utf8(&bytes)
                    .map(sampler::records_from_text)
                    .unwrap_or_default();
                if parsed {
                    sources.push(BackfillSource {
                        label: "quota-history-v1.json".to_string(),
                        sha256: sha256_hex(&bytes),
                        records,
                    });
                } else {
                    unreadable += 1;
                }
            }
            None => unreadable += 1,
        }
    }
    (sources, unreadable)
}

/// The quota sampler's pass: import the history once, then record the coalesced fed dashboards.
fn quota_pass(
    engine: &aac_store::engine::Engine,
    snapshot: Option<&Value>,
    ccs_dir: &Path,
    state: &mut QuotaState,
    now: i64,
) {
    let dashboards: Vec<Value> = snapshot
        .and_then(|doc| doc["dashboards"].as_array())
        .into_iter()
        .flatten()
        .map(|entry| {
            entry
                .get("dashboard")
                .cloned()
                .unwrap_or_else(|| entry.clone())
        })
        .collect();
    let merged = sampler::merge_accounts(&dashboards);
    if merged.is_empty() {
        return;
    }
    if !state.backfilled {
        let (sources, unreadable) = backfill_sources(ccs_dir);
        let providers = sampler::providers_of(&merged);
        if sampler::backfill(engine, &sources, &providers, now, unreadable).is_ok() {
            state.backfilled = true;
        }
    }
    if sampler::record_sample(engine, &merged, now).is_ok() {
        state.last_quota_ms = now;
    }
}

/// The quota cadence: the account-refresh interval clamped to [30s, 1h] (`schedule()`).
fn quota_interval_ms(refresh_seconds: i64) -> i64 {
    let configured = refresh_seconds.saturating_mul(1000);
    configured.clamp(30_000, 3_600_000)
}

/// Whether the activity collection is due (`startIfDue`): the converge interval while a scan is
/// converging, else the refresh interval, since the last fetch.
fn activity_due(state: &WriterState, now: i64, refresh_interval_ms: i64) -> bool {
    let interval = match state.scanning_since {
        Some(since) if now - since < CONVERGE_WINDOW_MS => {
            refresh_interval_ms.min(CONVERGE_INTERVAL_MS)
        }
        _ => refresh_interval_ms,
    };
    now - state.fetched_at >= interval
}

/// Validate the scanner's v2 answer: the reader name and the kinds set the request named.
fn valid_scan_answer(answer: &Value, kinds: &[String]) -> bool {
    if answer["version"].as_u64() != Some(2) {
        return false;
    }
    if answer["reader"]["name"].as_str() != Some("aac-usage-scan") {
        return false;
    }
    let Some(answer_kinds) = answer["kinds"].as_object() else {
        return false;
    };
    let mut requested: Vec<&str> = kinds.iter().map(String::as_str).collect();
    requested.sort_unstable();
    requested.dedup();
    let mut answered: Vec<&str> = answer_kinds.keys().map(String::as_str).collect();
    answered.sort_unstable();
    requested == answered
}

fn extra_roots(preferences: Option<&Value>, host: &str) -> (Value, HashSet<&'static str>, bool) {
    let mut roots: Map<String, Value> = Map::new();
    let mut overflow = HashSet::new();
    let mut generic = false;
    for source in preferences
        .and_then(|prefs| prefs["usageLogSources"].as_array())
        .into_iter()
        .flatten()
    {
        if source["host"].as_str() != Some(host) {
            continue;
        }
        let kind = match source["tool"].as_str() {
            Some("claude-code") => "claude",
            Some("codex") => "codex",
            Some("omp") => "omp",
            Some("muse") => "muse",
            Some("zcode") => "zcode",
            Some("jsonl") => {
                generic = true;
                continue;
            }
            _ => continue,
        };
        let paths = roots.entry(kind.to_owned()).or_insert_with(|| json!([]));
        if let Some(paths) = paths.as_array_mut() {
            if paths.contains(&source["path"]) {
                continue;
            }
            if paths.len() == 16 {
                overflow.insert(kind);
            } else {
                paths.push(source["path"].clone());
            }
        }
    }
    (Value::Object(roots), overflow, generic)
}

fn scan_state(answer: &Value, kind: &str) -> (&'static str, Option<&'static str>) {
    let read = &answer["kinds"][kind];
    let state = read["state"].as_str();
    if read["unreadable"] == true
        || ["readFailures", "unreadableFolders", "unreadableRecords"]
            .iter()
            .any(|key| read[key].as_u64().is_some_and(|count| count > 0))
    {
        return ("unreadable", Some("read_failed"));
    }
    if read["partial"] == true || state == Some("pending") {
        return ("scanning", None);
    }
    match state {
        Some("ok") => ("ok", None),
        Some("empty") => ("no_usage", None),
        Some("absent" | "not_installed") => ("not_installed", None),
        Some("unreadable") => ("unreadable", Some("read_failed")),
        _ => ("scan_failed", None),
    }
}

fn scan_request(
    kinds: &[String],
    cutoff: i64,
    extra: Value,
    cwd: bool,
    experiment_roots: Option<&Map<String, Value>>,
) -> Value {
    let mut request = json!({
        "kinds": kinds,
        "minDateMs": cutoff,
        "immutableSqlite": false,
        "extraRoots": extra,
        "cwd": cwd,
        "deadlineMs": SCAN_DEADLINE_MS,
    });
    if let Some(roots) = experiment_roots.filter(|roots| !roots.is_empty()) {
        request["experimentRoots"] = json!(roots);
    }
    request
}

fn failed_scan_states(host: &str, kinds: &[String], agent_missing: bool) -> Vec<Value> {
    kinds
        .iter()
        .map(|kind| {
            json!({
                "tool": kind,
                "host": host,
                "state": if host == "ubuntu" && !agent_missing { "scan_failed" } else { "unreadable" },
                "cause": agent_missing.then_some("agent_missing"),
                "detail": "scan-usage unavailable",
            })
        })
        .collect()
}

/// The activity scheduler's pass: scan each configured computer, shape and ingest the reports.
async fn activity_pass(
    engine: &aac_store::engine::Engine,
    agents: &Arc<crate::agent_transport::Agents>,
    state: &Mutex<WriterState>,
    ccs_dir: &Path,
    models_dev: &Arc<crate::models_dev::ModelsDevRefresh>,
    live: &Arc<crate::analytics_live::Live>,
    now: i64,
) {
    // Claim a generation under the lock, then release it for the scans: a std mutex guard must
    // not cross an await (the job's future stays Send). A newer pass supersedes this one.
    let (generation, experiments) = {
        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
        if !activity_due(&guard, now, quota_interval_ms(refresh_seconds(ccs_dir))) {
            return;
        }
        guard.generation += 1;
        (guard.generation, guard.experiments.clone())
    };
    let computers = agents.usage_hosts().await;
    if computers.is_empty() {
        state.lock().unwrap_or_else(|p| p.into_inner()).fetched_at = now;
        return;
    }
    let cutoff = now - 31 * DAY;
    // The read reports `refreshing` while this pass runs (the TS activity service's pending pass).
    let _pass = live.pass();
    // Refresh the models.dev registry where the TS collection calls startModelsDevRegistryRefresh:
    // coalesced, fresh-cache short-circuit, stale fallback. The blocking fetch runs off-thread.
    if computers
        .iter()
        .any(|host| host.agent_available || fixed_scanner::eligible(host))
    {
        let refresh = Arc::clone(models_dev);
        let now_ms = now as f64;
        tokio::task::spawn_blocking(move || {
            refresh.refresh(false, now_ms);
        });
    }
    let pricing = ResolverPricing::new(ccs_dir);
    let mut remote: Vec<Value> = Vec::new();
    let mut states: Vec<Value> = Vec::new();
    let preferences = crate::account_reads::preferences(ccs_dir);
    let kinds = aac_providers::scanner_kinds()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    // Saved Generic JSONL sources have no scanner kind: this writer reads them itself.
    let mut mapped: Vec<Value> = Vec::new();
    let mut mapped_state = false;
    let mut mapped_complete = false;
    for computer in &computers {
        let host = computer.id.as_str();
        let (mut extra_roots, mut overflow, generic) = extra_roots(preferences.as_ref(), host);
        let local = computer.alias.is_none();
        let experiments = experiments.as_deref().filter(|_| local);
        if let Some(experiments) = experiments {
            experiments.add_zcode_paths(&mut extra_roots, &mut overflow);
        }
        if generic {
            let read = analytics_jsonl::read(preferences.as_ref(), host, cutoff, now, &pricing);
            mapped_state |= read.state.is_some();
            mapped_complete = read.complete;
            if let Some(state) = read.state {
                states.push(state);
            }
            mapped = read.data;
        }
        let args = scan_request(
            &kinds,
            cutoff,
            extra_roots,
            crate::analytics_route::capture_cwd(ccs_dir),
            experiments
                .map(experiment_roots::Snapshot::scanner_roots)
                .as_ref(),
        );
        let call_id = format!("g14-activity-{generation}-{host}");
        let mut answer = if computer.agent_available {
            agents
                .scan_usage(
                    computer.platform,
                    computer.alias.as_deref(),
                    &args,
                    SCAN_DEADLINE_MS,
                    SCAN_MAX_BYTES,
                    &call_id,
                )
                .await
        } else {
            Err(aac_core::Cause::AgentMissing)
        };
        if matches!(
            answer,
            Err(aac_core::Cause::AgentMissing | aac_core::Cause::AgentMismatch)
        ) {
            answer = fixed_scanner::scan(agents, computer, &args, SCAN_DEADLINE_MS, SCAN_MAX_BYTES)
                .await;
        }
        match answer {
            Ok(answer) if valid_scan_answer(&answer, &kinds) => {
                let rows = answer["rows"].as_array().cloned().unwrap_or_default();
                let srows = answer["srows"].as_array().cloned().unwrap_or_default();
                for kind in &kinds {
                    let kind_rows: Vec<Value> = rows
                        .iter()
                        .filter(|r| r["k"].as_str() == Some(kind.as_str()))
                        .cloned()
                        .collect();
                    let kind_srows: Vec<Value> = srows
                        .iter()
                        .filter(|r| r["k"].as_str() == Some(kind.as_str()))
                        .cloned()
                        .collect();
                    let worker = collect::worker_from_scan(
                        &kind_rows,
                        &kind_srows,
                        kind,
                        host,
                        now,
                        &pricing,
                    );
                    remote.push(json!({ "tool": kind, "host": host, "data": worker }));
                    let (mut state, mut cause) = scan_state(&answer, kind);
                    if overflow.contains(kind.as_str())
                        || experiments.is_some_and(|snapshot| snapshot.limited(kind))
                    {
                        (state, cause) = ("unreadable", Some("source_limit"));
                    } else if matches!(state, "ok" | "not_installed" | "no_usage")
                        && local
                        && experiments.map_or_else(
                            || experiment_roots::tracks(kind),
                            |snapshot| snapshot.discovering(kind),
                        )
                    {
                        (state, cause) = ("scanning", None);
                    }
                    states.push(json!({
                        "tool": kind,
                        "host": host,
                        "state": state,
                        "cause": cause,
                        "lastScanAt": aac_num::ecma::to_iso_string(now).unwrap_or_default(),
                    }));
                }
            }
            Err(aac_core::Cause::AgentMissing) => {
                states.extend(failed_scan_states(host, &kinds, true))
            }
            _ => states.extend(failed_scan_states(host, &kinds, false)),
        }
    }
    // Shape and ingest under the lock; a superseding generation drops this pass's writes.
    let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
    if generation != guard.generation {
        return;
    }
    let mut local: HashMap<String, Vec<Value>> = HashMap::new();
    let mut local_complete: HashSet<String> = HashSet::new();
    if mapped_state {
        local.insert("jsonl".into(), mapped);
        if mapped_complete {
            local_complete.insert("jsonl".into());
        }
    }
    let scanner_owned: HashSet<String> = kinds.into_iter().collect();
    let args = ReportArgs {
        local: &local,
        remote: &remote,
        states: &states,
        window_from_ms: cutoff,
        now,
        local_complete: &local_complete,
        scanner_owned: &scanner_owned,
    };
    let reports = report::build_activity_reports(&args);
    let (trimmed, remember) = guard.ingestor.trim(&reports);
    if let Ok(typed) = report::contract_reports(&trimmed)
        && !typed.is_empty()
    {
        match activity::ingest(engine, &typed) {
            Ok(result) => {
                let refused: HashSet<usize> = result
                    .refused
                    .iter()
                    .filter_map(|r| r["index"].as_u64().map(|i| i as usize))
                    .collect();
                guard.ingestor.accept(remember, &refused);
            }
            Err(_) => guard.ingestor.forget(&remember),
        }
    }
    guard.states = states;
    guard.fetched_at = now;
    if guard
        .states
        .iter()
        .any(|source| source["state"] == "scanning")
    {
        guard.scanning_since.get_or_insert(now);
    } else {
        guard.scanning_since = None;
    }
}

/// The refresh interval the front door's account-refresh settings name (seconds), default 60.
fn refresh_seconds(ccs_dir: &Path) -> i64 {
    crate::registered::file(ccs_dir, "account-refresh-settings")
        .and_then(|(file, policy)| {
            let aac_files::read::Read::Bytes(bytes, _) = aac_files::read::read(&file, &policy)
            else {
                return None;
            };
            serde_json::from_slice::<Value>(&bytes).ok()
        })
        .and_then(|v| v["refreshIntervalSeconds"].as_i64())
        .filter(|s| (30..=3600).contains(s))
        .unwrap_or(60)
}

/// The hosted store's writer jobs consume the existing collection generation and agent pool.
pub(crate) async fn analytics_jobs(
    store: Arc<StoreHost>,
    dashboard: impl Fn(i64) -> Option<Value> + Send + Sync + 'static,
    agents: Arc<crate::agent_transport::Agents>,
    ccs_dir: PathBuf,
    live: Arc<crate::analytics_live::Live>,
    stop: Arc<AtomicBool>,
) {
    tokio::time::sleep(Duration::from_millis(FIRST_PASS_MS)).await;
    let quota = async {
        let state = Arc::new(Mutex::new(QuotaState::default()));
        let dashboard = Arc::new(dashboard);
        while !stop.load(Ordering::Acquire) {
            if let Some(engine) = store.analytics_engine() {
                let state = state.clone();
                let dashboard = dashboard.clone();
                let root = ccs_dir.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let now = aac_store::engine::now_ms() as i64;
                    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                    if now - state.last_quota_ms >= quota_interval_ms(refresh_seconds(&root)) {
                        quota_pass(&engine, dashboard(now).as_ref(), &root, &mut state, now);
                    }
                })
                .await;
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    };
    let state = Mutex::new(WriterState::default());
    let activity = async {
        let models_dev = Arc::new(crate::models_dev::ModelsDevRefresh::new(&ccs_dir));
        while !stop.load(Ordering::Acquire) {
            if let Some(engine) = store.analytics_engine() {
                let now = aac_store::engine::now_ms() as i64;
                if live.take_refresh() {
                    state.lock().unwrap_or_else(|p| p.into_inner()).fetched_at = 0;
                }
                activity_pass(&engine, &agents, &state, &ccs_dir, &models_dev, &live, now).await;
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    };
    let discovery = async {
        let Some(home) = std::env::home_dir() else {
            return;
        };
        let mut next = 0;
        while !stop.load(Ordering::Acquire) {
            let now = aac_store::engine::now_ms() as i64;
            if now >= next && store.analytics_engine().is_some() {
                let root = ccs_dir.clone();
                let home = home.clone();
                if let Ok(snapshot) = tokio::task::spawn_blocking(move || {
                    experiment_roots::run_slice(&root, &home, now)
                })
                .await
                {
                    next = snapshot.next_slice(now);
                    let done = next > now;
                    state.lock().unwrap_or_else(|p| p.into_inner()).experiments =
                        Some(Arc::new(snapshot));
                    if done {
                        live.request_refresh();
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    };
    tokio::join!(quota, discovery, activity);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analytics_local_scan_request_carries_experiment_folders() {
        let home = tempfile::tempdir().expect("fixture home");
        let ccs = tempfile::tempdir().expect("fixture CCS_HOME");
        let root = home.path().join(
            [
                "PM-Experiments",
                "exp-1",
                "retained-usage",
                "run-1",
                "codex",
            ]
            .iter()
            .collect::<PathBuf>(),
        );
        std::fs::create_dir_all(&root).expect("experiment folder");
        std::fs::write(
            root.join("rollout-fixture.jsonl"),
            concat!(
                r#"{"type":"session_meta","payload":{"id":"pm-fixture","originator":"codex_cli_rs","cli_version":"1"}}"#, "\n",
                r#"{"timestamp":"2026-11-15T13:00:00Z","type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#, "\n",
                r#"{"timestamp":"2026-11-15T13:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":123,"output_tokens":4}}}}"#, "\n",
            ),
        )
        .expect("fixture rollout");
        let snapshot = experiment_roots::run_slice(ccs.path(), home.path(), 1);
        let roots = snapshot.scanner_roots();
        let kinds = aac_providers::scanner_kinds()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let request = scan_request(&kinds, 0, json!({}), false, Some(&roots));
        assert_eq!(request["experimentRoots"], json!({"codex": [root]}));
        assert_eq!(request["cwd"], false);
        let native_scan = |request: &Value| {
            let args = aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("native scanner request");
            aac_usage::protocol::scan(&args, home.path(), &aac_usage::discover::Env::default())
        };
        let local = native_scan(&request);
        assert_eq!(local["rows"].as_array().expect("usage rows").len(), 1);
        let remote = scan_request(&kinds, 0, json!({}), false, None);
        assert_eq!(remote["experimentRoots"], Value::Null);
        assert_eq!(native_scan(&remote)["rows"], json!([]));
    }

    #[test]
    fn analytics_missing_agent_replaces_a_previous_ok_report() {
        let dir = tempfile::tempdir().expect("temp dir");
        let engine = aac_store::ops::open_engine(dir.path().to_str().expect("path"), None, None)
            .expect("open engine");
        let before = json!({
            "schema": report::USAGE_REPORT_SCHEMA, "computer": "mac", "tool": "codex",
            "state": "ok", "readAt": "2026-11-15T12:00:00.000Z", "complete": true,
            "coverFrom": "2026-11-14T00:00:00.000Z", "sessions": [],
            "cells": [{
                "hour": "2026-11-15T11:00:00.000Z", "route": "openai", "modelRaw": "gpt-6.1-sol",
                "inputTokens": 123, "outputTokens": 4, "cacheCreationTokens": 0, "cacheReadTokens": 0,
                "costUsd": 0.01, "events": 1,
            }],
        });
        assert_eq!(
            ingest_reports(&engine, &[before]).expect("seed")["refused"],
            json!([])
        );
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-15T13:00:00Z")
            .expect("clock")
            .timestamp_millis();
        let source = || {
            let body = aac_store::ops::call(
                &engine,
                "activitySources",
                &json!({ "fromMs": now - DAY, "toMs": now }).to_string(),
            )
            .expect("activitySources");
            body["sources"][0].clone()
        };
        assert_eq!(source()["state"], "ok");
        let states = failed_scan_states("mac", &["codex".to_owned()], true);
        let reports = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &[],
            states: &states,
            window_from_ms: now - DAY,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &HashSet::new(),
        });
        assert_eq!(
            ingest_reports(&engine, &reports).expect("ingest")["refused"],
            json!([])
        );
        let source = source();
        assert_eq!(source["state"], "unreadable");
        assert_eq!(source["cause"], "agent_missing");
    }
    /// The activity source states the last pass produced (the owned read's live grid).
    fn source_states(state: &Mutex<WriterState>) -> Vec<Value> {
        state.lock().map(|s| s.states.clone()).unwrap_or_default()
    }

    /// Ingest a pass's reports directly (the owned read / backfill path).
    fn ingest_reports(
        engine: &aac_store::engine::Engine,
        reports: &[Value],
    ) -> Result<Value, String> {
        let typed = report::contract_reports(reports).map_err(|e| e.to_string())?;
        let result = activity::ingest(engine, &typed).map_err(|e| e.to_string())?;
        serde_json::to_value(result).map_err(|e| e.to_string())
    }

    /// The quota cadence clamp.
    fn interval_for(refresh_seconds: i64) -> i64 {
        quota_interval_ms(refresh_seconds)
    }

    #[test]
    fn quota_cadence_clamps_the_refresh_interval() {
        assert_eq!(interval_for(60), 60_000);
        assert_eq!(interval_for(10), 30_000);
        assert_eq!(interval_for(7200), 3_600_000);
        assert_eq!(interval_for(3600), 3_600_000);
    }

    #[test]
    fn analytics_backfill_waits_for_the_account_dashboard() {
        let dir = tempfile::tempdir().expect("temp dir");
        let engine = aac_store::ops::open_engine(dir.path().to_str().expect("path"), None, None)
            .expect("open engine");
        let mut state = QuotaState::default();
        quota_pass(&engine, None, dir.path(), &mut state, 1_794_700_800_000);
        assert!(!state.backfilled);
        assert_eq!(state.last_quota_ms, 0);
    }

    #[test]
    fn activity_converge_shortens_the_interval() {
        let mut state = WriterState::default();
        let now = 1_000_000_000;
        // Never fetched: due.
        assert!(activity_due(&state, now, 300_000));
        // Just fetched: not due until the refresh interval passes.
        state.fetched_at = now;
        assert!(!activity_due(&state, now + 1000, 300_000));
        assert!(activity_due(&state, now + 300_000, 300_000));
        // Converging: the shorter interval applies.
        state.scanning_since = Some(now);
        assert!(activity_due(&state, now + CONVERGE_INTERVAL_MS, 300_000));
        assert!(!activity_due(&state, now + 1000, 300_000));
    }

    #[test]
    fn scan_answer_validation_checks_version_reader_and_kinds() {
        let kinds = vec!["claude".to_string(), "codex".to_string()];
        let good = json!({
            "version": 2,
            "reader": { "name": "aac-usage-scan" },
            "kinds": { "claude": {}, "codex": {} },
        });
        assert!(valid_scan_answer(&good, &kinds));
        let wrong_version = json!({ "version": 1, "reader": { "name": "aac-usage-scan" }, "kinds": { "claude": {}, "codex": {} } });
        assert!(!valid_scan_answer(&wrong_version, &kinds));
        let wrong_kinds = json!({ "version": 2, "reader": { "name": "aac-usage-scan" }, "kinds": { "claude": {} } });
        assert!(!valid_scan_answer(&wrong_kinds, &kinds));
    }

    #[test]
    fn partial_and_failed_scans_do_not_claim_empty_usage() {
        let answer = json!({"kinds": {
            "claude": {"state":"ok", "partial":true},
            "codex": {"state":"ok", "readFailures":1},
            "antigravity": {"state":"unreadable"},
            "omp": {"state":"ok"},
        }});
        assert_eq!(scan_state(&answer, "claude"), ("scanning", None));
        assert_eq!(
            scan_state(&answer, "codex"),
            ("unreadable", Some("read_failed"))
        );
        assert_eq!(
            scan_state(&answer, "antigravity"),
            ("unreadable", Some("read_failed"))
        );
        assert_eq!(scan_state(&answer, "omp"), ("ok", None));
    }

    #[test]
    fn ingest_reports_writes_and_source_states_reads() {
        let dir = tempfile::tempdir().expect("temp dir");
        let engine = aac_store::ops::open_engine(dir.path().to_str().expect("path"), None, None)
            .expect("open engine");
        let report = json!({
            "schema": report::USAGE_REPORT_SCHEMA,
            "computer": "ubuntu",
            "tool": "claude",
            "readAt": "2026-11-15T12:00:00.000Z",
            "state": "ok",
            "complete": true,
            "coverFrom": "2026-11-14T00:00:00.000Z",
            "cells": [{
                "hour": "2026-11-14T10:00:00.000Z",
                "route": "anthropic",
                "modelRaw": "claude-sonnet-4-5",
                "inputTokens": 1000,
                "outputTokens": 200,
                "cacheCreationTokens": 50,
                "cacheReadTokens": 300,
                "costUsd": 0.01,
                "events": 4
            }],
            "sessions": []
        });
        let result = ingest_reports(&engine, &[report]).expect("ingest");
        assert!(result["refused"].as_array().is_some_and(|a| a.is_empty()));
        let state = Mutex::new(WriterState {
            states: vec![json!({ "tool": "claude", "host": "ubuntu", "state": "ok" })],
            ..Default::default()
        });
        assert_eq!(source_states(&state).len(), 1);
    }
}
