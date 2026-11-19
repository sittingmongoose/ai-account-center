//! G14: the Analytics writers the Rust service owns when `rustService.groups.analytics-writers` is
//! on — the quota-history sampler and the activity collection scheduler. Both write through the
//! hosted store's Engine in process (the ports in `aac_store::{sampler, collect, report}`); the TS
//! front door feeds the inputs (dashboards, the activity plan, resolved pricing facts) and stands
//! its own writers down, so nothing is written or scanned twice.
//!
//! The quota sampler coalesces both platforms' fed dashboards, imports the JSON history once, and
//! records the live readings on the account-refresh cadence. The activity scheduler calls lane E's
//! `scan-usage` verb per configured computer (local and remote), shapes the rows into reports and
//! ingests them on the converge cadence, keeping each source's state for the owned read.
//!
//! Residuals fed from TS until their own ports land: the plan worker's root discovery
//! (`extraRoots`/`experimentRoots`/fingerprints arrive in the plan feed), pricing resolution (the
//! facts feed), and the main history file's strict-reader validation (the job reads its records).
use crate::feed::Feeds;
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

/// The control feed: the front door says whether the writers own collection this boot.
const CONTROL: &str = "analytics-writers";
/// The fed dashboards: `{dashboards: [mac, windows]}` (the collectors' bounded accounts).
const DASHBOARD: &str = "analytics-dashboard";
/// The fed activity plan: `{computers: [{platform, sshHost?, kinds, fingerprints?, extraRoots?,
/// experimentRoots?}], windowDays?}`.
const PLAN: &str = "analytics-plan";
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
    backfilled: bool,
    ingestor: Ingestor,
    states: Vec<Value>,
    fetched_at: i64,
    scanning_since: Option<i64>,
    generation: u64,
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

fn fed_json(feeds: &Feeds, source: &str) -> Option<Value> {
    let generation = feeds.latest(source).ok()?;
    serde_json::from_str(generation.data.get()).ok()
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
        if path.is_dir() {
            archive_files(&path, depth + 1, out);
        } else if path.is_file() {
            out.push(path);
        }
    }
}

/// `readArchiveFile`: a file's bytes, gunzipped when `.gz`, bounded at 64 MiB.
fn read_archive_file(file: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(file).ok()?;
    if bytes.len() as u64 > MAX_ARCHIVE_BYTES {
        return None;
    }
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
    archive_files(&archive, 0, &mut files);
    let mut sources = Vec::new();
    let mut unreadable = 0u64;
    for file in files {
        let Some(bytes) = read_archive_file(&file) else {
            unreadable += 1;
            continue;
        };
        let Ok(text) = String::from_utf8(bytes.clone()) else {
            unreadable += 1;
            continue;
        };
        let (records, parsed) = sampler::records_from_text(&text);
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
    if main.is_file() {
        match std::fs::read(&main) {
            Ok(bytes) => {
                let text = String::from_utf8_lossy(&bytes);
                let (records, parsed) = sampler::records_from_text(&text);
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
            Err(_) => unreadable += 1,
        }
    }
    (sources, unreadable)
}

/// The quota sampler's pass: import the history once, then record the coalesced fed dashboards.
fn quota_pass(
    engine: &aac_store::engine::Engine,
    feeds: &Feeds,
    ccs_dir: &Path,
    state: &mut WriterState,
    now: i64,
) {
    let Some(doc) = fed_json(feeds, DASHBOARD) else {
        return;
    };
    let Some(dashboards) = doc["dashboards"].as_array() else {
        return;
    };
    let merged = sampler::merge_accounts(dashboards);
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

/// The activity scheduler's pass: scan each configured computer, shape and ingest the reports.
#[expect(
    clippy::too_many_arguments,
    reason = "the pass borrows its store, source and clock dependencies"
)]
async fn activity_pass(
    engine: &aac_store::engine::Engine,
    feeds: &Feeds,
    agents: &Arc<crate::agent_transport::Agents>,
    state: &Mutex<WriterState>,
    ccs_dir: &Path,
    models_dev: &Arc<crate::models_dev::ModelsDevRefresh>,
    now: i64,
    refresh_interval_ms: i64,
) {
    // Claim a generation under the lock, then release it for the scans: a std mutex guard must
    // not cross an await (the job's future stays Send). A newer pass supersedes this one.
    let (generation, computers, window_from, cutoff) = {
        let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
        if !activity_due(&guard, now, refresh_interval_ms) {
            return;
        }
        let Some(plan) = fed_json(feeds, PLAN) else {
            return;
        };
        let Some(computers) = plan["computers"].as_array().cloned() else {
            return;
        };
        let window_days = plan["windowDays"].as_i64().unwrap_or(31);
        guard.generation += 1;
        guard.scanning_since = Some(now);
        (
            guard.generation,
            computers,
            now - window_days * DAY,
            now - 31 * DAY,
        )
    };
    // Refresh the models.dev registry where the TS collection calls startModelsDevRegistryRefresh:
    // coalesced, fresh-cache short-circuit, stale fallback. The blocking fetch runs off-thread.
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
    for computer in &computers {
        let platform = computer["platform"].as_str().unwrap_or("ubuntu");
        let ssh_host = computer["sshHost"].as_str();
        let host = computer["host"].as_str().unwrap_or(platform);
        let kinds: Vec<String> = computer["kinds"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();
        if kinds.is_empty() {
            continue;
        }
        let mut args = Map::new();
        args.insert("kinds".into(), json!(kinds));
        args.insert("minDateMs".into(), json!(cutoff));
        args.insert(
            "immutableSqlite".into(),
            json!(!kinds.iter().any(|k| k == "zcode")),
        );
        if let Some(fingerprints) = computer.get("fingerprints") {
            args.insert("fingerprints".into(), fingerprints.clone());
        }
        args.insert(
            "extraRoots".into(),
            computer.get("extraRoots").cloned().unwrap_or(json!({})),
        );
        if let Some(experiment) = computer.get("experimentRoots") {
            args.insert("experimentRoots".into(), experiment.clone());
        }
        args.insert("deadlineMs".into(), json!(SCAN_DEADLINE_MS));
        let call_id = format!("g14-activity-{generation}-{host}");
        let answer = agents
            .scan_usage(
                platform,
                ssh_host,
                &Value::Object(args),
                SCAN_DEADLINE_MS,
                SCAN_MAX_BYTES,
                &call_id,
            )
            .await;
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
                    states.push(json!({
                        "tool": kind,
                        "host": host,
                        "state": "ok",
                        "lastScanAt": aac_num::ecma::to_iso_string(now).unwrap_or_default(),
                    }));
                }
            }
            _ => {
                for kind in &kinds {
                    states.push(json!({
                        "tool": kind,
                        "host": host,
                        "state": if host == "ubuntu" { "scan_failed" } else { "unreadable" },
                        "detail": "scan-usage unavailable",
                    }));
                }
            }
        }
    }
    // Shape and ingest under the lock; a superseding generation drops this pass's writes.
    let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
    if generation != guard.generation {
        return;
    }
    let local: HashMap<String, Vec<Value>> = HashMap::new();
    let local_complete: HashSet<String> = HashSet::new();
    let scanner_owned: HashSet<String> = kinds_set(&computers);
    let args = ReportArgs {
        local: &local,
        remote: &remote,
        states: &states,
        window_from_ms: window_from,
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
    guard.scanning_since = None;
}

/// The tools the scanner owns on the dashboard's computer (every kind the plan names for ubuntu).
fn kinds_set(computers: &[Value]) -> HashSet<String> {
    let mut owned = HashSet::new();
    for computer in computers {
        if (computer["host"].as_str() == Some("ubuntu")
            || computer["platform"].as_str() == Some("ubuntu"))
            && let Some(kinds) = computer["kinds"].as_array()
        {
            for kind in kinds.iter().filter_map(|v| v.as_str()) {
                owned.insert(kind.to_string());
            }
        }
    }
    owned
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

/// The writers' job loop: each tick, when the front door says the writers own collection, sample
/// the quota history on its cadence and collect activity on its converge cadence. The flag ends
/// the loop when the service ends.
pub(crate) async fn analytics_jobs(
    store: Arc<StoreHost>,
    feeds: Feeds,
    agents: Arc<crate::agent_transport::Agents>,
    ccs_dir: PathBuf,
    stop: Arc<AtomicBool>,
) {
    tokio::time::sleep(Duration::from_millis(FIRST_PASS_MS)).await;
    let state = Arc::new(Mutex::new(WriterState::default()));
    let models_dev = Arc::new(crate::models_dev::ModelsDevRefresh::new(&ccs_dir));
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let enabled = fed_json(&feeds, CONTROL)
            .and_then(|doc| doc["enabled"].as_bool())
            .unwrap_or(false);
        if enabled && let Some(engine) = store.analytics_engine() {
            let now = aac_store::engine::now_ms() as i64;
            let refresh = refresh_seconds(&ccs_dir);
            let interval = quota_interval_ms(refresh);
            {
                let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
                if now - guard.last_quota_ms >= interval {
                    quota_pass(&engine, &feeds, &ccs_dir, &mut guard, now);
                }
            }
            activity_pass(
                &engine,
                &feeds,
                &agents,
                &state,
                &ccs_dir,
                &models_dev,
                now,
                interval,
            )
            .await;
        }
        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
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
    fn scanner_owned_collects_ubuntu_kinds() {
        let computers = vec![
            json!({ "host": "ubuntu", "platform": "ubuntu", "kinds": ["claude", "cursor"] }),
            json!({ "host": "mac", "platform": "mac", "kinds": ["codex"] }),
        ];
        let owned = kinds_set(&computers);
        assert!(owned.contains("claude"));
        assert!(owned.contains("cursor"));
        assert!(!owned.contains("codex"));
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
