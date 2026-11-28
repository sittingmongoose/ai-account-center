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
use aac_store::report::{self, Ingestor, OwnedReportArgs};
use aac_store::sampler::{self, BackfillSource};
use scan_answer::Answer;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

mod analytics_jsonl;
mod claude_contributions;
mod claude_mirrors;
mod claude_owners;
mod experiment_roots;
mod experiment_signatures;
mod fixed_scanner;
mod history;
mod local_inventory;
#[cfg(all(test, feature = "memory-twin"))]
mod memory_twin;
mod migration;
mod scan_answer;
mod scan_print;
mod scan_state;

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
/// The scanner's own budget inside one scan call. The scanner caps it at 120 s and the agent
/// stops the call at SCAN_DEADLINE_MS; a shorter budget lets a long first scan return its
/// partial rows and fingerprints (resumed next pass) instead of the agent deadline killing it.
const SCANNER_BUDGET_MS: u64 = 90_000;
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
    archives: Vec<(String, String)>,
    last_quota_ms: i64,
}

/// The collection aggregation's [`Pricing`] seam over the G15 Rust resolver: the same
/// `getModelPricingWithSource` lookup the TS collector uses, reading the registry cache the
/// scheduled `ModelsDevRefresh` keeps fresh. Interior mutability carries the resolver's memo.
/// One pass's resolutions are remembered: every computer and tool asks for the same few models.
struct ResolverPricing {
    resolver: Mutex<aac_views::pricing::Resolver>,
    resolved: Mutex<HashMap<(String, Option<String>), PricingResolution>>,
}

impl ResolverPricing {
    fn new(ccs_dir: &Path) -> Self {
        ResolverPricing {
            resolver: Mutex::new(aac_views::pricing::Resolver::new(ccs_dir)),
            resolved: Mutex::new(HashMap::new()),
        }
    }
}

impl Pricing for ResolverPricing {
    fn resolve(&self, model: &str, provider: Option<&str>) -> PricingResolution {
        let key = (model.to_owned(), provider.map(str::to_owned));
        if let Some(found) = self
            .resolved
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get(&key)
        {
            return found.clone();
        }
        let mut resolver = self.resolver.lock().unwrap_or_else(|p| p.into_inner());
        let resolution = resolver.get_model_pricing_with_source(model, provider, None);
        let rates = resolution.pricing.rates;
        let found = PricingResolution {
            rates: Rates {
                input: rates.input_per_million,
                output: rates.output_per_million,
                cache_creation: rates.cache_creation_per_million,
                cache_read: rates.cache_read_per_million,
            },
            source: resolution.source.as_str().to_string(),
        };
        self.resolved
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .insert(key, found.clone());
        found
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
fn backfill_sources(
    engine: &aac_store::engine::Engine,
    ccs_dir: &Path,
    staged: &[(String, String)],
) -> (Vec<BackfillSource>, u64) {
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
    for (key, digest) in staged {
        if engine
            .get_meta(&format!("ts:backfill:{digest}"))
            .is_ok_and(|receipt| receipt.is_some())
        {
            continue;
        }
        let Ok(Some(bytes)) = aac_store::analytics_cache::archived(engine, key) else {
            unreadable += 1;
            continue;
        };
        if sha256_hex(&bytes) != *digest {
            unreadable += 1;
            continue;
        }
        let (records, parsed) = std::str::from_utf8(&bytes)
            .map(sampler::records_from_text)
            .unwrap_or_default();
        if parsed {
            sources.push(BackfillSource {
                label: key
                    .trim_start_matches("legacy/account-analytics/")
                    .to_owned(),
                sha256: digest.clone(),
                records,
            });
        } else if key == migration::QUOTA {
            unreadable += 1;
        }
    }
    let mut seen = HashSet::new();
    sources.retain(|source| seen.insert(source.sha256.clone()));
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
    if let Ok(staged) = aac_store::analytics_cache::archived_index(engine, migration::QUOTA)
        && (!state.backfilled || staged != state.archives)
    {
        let (sources, unreadable) = backfill_sources(engine, ccs_dir, &staged);
        let providers = sampler::providers_of(&merged);
        if sampler::backfill(engine, &sources, &providers, now, unreadable).is_ok() {
            state.backfilled = true;
            state.archives = staged;
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

/// What every report of one pass reads besides its own sources and worker results.
struct ReportFrame<'a> {
    window_from_ms: i64,
    now: i64,
    local_complete: &'a HashSet<String>,
    scanner_owned: &'a HashSet<String>,
}

/// One pass's reports, built a group of sources at a time as soon as the group's worker results
/// are ready, then trimmed against the last accepted pass and typed. The pass holds these
/// trimmed typed reports (what changed since the last accepted pass) instead of every
/// computer's worker trees until its end.
///
/// The same reports as one `build_activity_reports_owned` call over every source: a report reads
/// only its own source and the worker results of its tool and computer, and a group always
/// holds every source of each tool and computer it builds (the caller's grouping guarantees it).
#[derive(Default)]
struct Shaped {
    /// By the source's index in the pass's states, so they are ingested in that order.
    reports: std::collections::BTreeMap<usize, (aac_store::contract::Report, report::Remember)>,
    /// Every source whose report was built, refused or not.
    built: HashSet<usize>,
    /// A report the typed contract refused: the pass ingests nothing, as when the one
    /// `contract_reports` call over every report failed.
    refused: bool,
}

impl Shaped {
    /// The reports of `states[indexes]`, reading `local` and `remote` (handed over).
    fn build(
        frame: &ReportFrame<'_>,
        states: &[Value],
        indexes: Vec<usize>,
        local: HashMap<String, Vec<Value>>,
        remote: Vec<Value>,
    ) -> Vec<(usize, Value)> {
        if indexes.is_empty() {
            return Vec::new();
        }
        let sources: Vec<Value> = indexes.iter().map(|&index| states[index].clone()).collect();
        let reports = report::build_activity_reports_owned(OwnedReportArgs {
            local,
            remote,
            states: &sources,
            window_from_ms: frame.window_from_ms,
            now: frame.now,
            local_complete: frame.local_complete,
            scanner_owned: frame.scanner_owned,
        });
        indexes.into_iter().zip(reports).collect()
    }

    /// Trim and type built reports, one at a time: each full report is dropped once trimmed.
    fn keep(&mut self, ingestor: &Ingestor, built: Vec<(usize, Value)>) {
        for (index, built) in built {
            self.built.insert(index);
            let (trimmed, mut remember) = ingestor.trim(std::slice::from_ref(&built));
            drop(built);
            match report::contract_reports(&trimmed) {
                Ok(mut typed) => {
                    if let (Some(typed), Some(remember)) = (typed.pop(), remember.pop()) {
                        self.reports.insert(index, (typed, remember));
                    }
                }
                Err(_) => self.refused = true,
            }
        }
    }

    fn is_built(&self, index: usize) -> bool {
        self.built.contains(&index)
    }

    /// One computer's sources of the scanner's tools other than Claude (`states[start..]`, the
    /// computer's): each reads only its own source and this computer's worker results of its
    /// tool, and every source of those tools on this computer is here. Claude's wait for the
    /// mirror owners; sources of other tools may read the local results.
    fn computer_sources(
        states: &[Value],
        start: usize,
        scanner_owned: &HashSet<String>,
    ) -> Vec<usize> {
        (start..states.len())
            .filter(|&index| {
                states[index]["tool"]
                    .as_str()
                    .is_some_and(|tool| tool != "claude" && scanner_owned.contains(tool))
            })
            .collect()
    }

    /// Every Claude source of `host` not built yet: they read that computer's Claude worker.
    fn claude_sources(&self, states: &[Value], host: &str) -> Vec<usize> {
        (0..states.len())
            .filter(|&index| {
                !self.is_built(index)
                    && states[index]["tool"] == "claude"
                    && states[index]["host"].as_str() == Some(host)
            })
            .collect()
    }

    /// The sources not built yet: no worker result of the pass names them (failed reads,
    /// Claude without a part), or they read the local results.
    fn rest_sources(&self, states: &[Value]) -> Vec<usize> {
        (0..states.len())
            .filter(|&index| !self.is_built(index))
            .collect()
    }

    /// The typed reports in source order with the digests to remember, or `None` when one
    /// report was refused by the contract.
    fn finish(self) -> Option<(Vec<aac_store::contract::Report>, Vec<report::Remember>)> {
        if self.refused {
            return None;
        }
        Some(self.reports.into_values().unzip())
    }
}

/// The pass's Claude reports: the parts read back, the mirror owners chosen over all of them,
/// then one computer's worker and reports at a time, the smallest first, each part dropped once
/// its worker is built (the order changes nothing: each report reads only its own computer's).
/// When the owners cannot be updated, every part's source is unreadable and no part is read.
fn shape_claude(
    engine: &aac_store::engine::Engine,
    parked: Vec<claude_mirrors::Parked<'_>>,
    states: &mut [Value],
    pricing: &dyn Pricing,
    frame: &ReportFrame<'_>,
    shaped: &mut Shaped,
    ingestor: &Ingestor,
) {
    let indexes: Vec<usize> = parked
        .iter()
        .map(claude_mirrors::Parked::state_index)
        .collect();
    let parts = parked
        .into_iter()
        .map(claude_mirrors::Parked::restore)
        .collect::<aac_store::error::Result<Vec<_>>>()
        .and_then(|mut parts| {
            claude_contributions::prepare_live(engine, &mut parts, states, frame.now, pricing)?;
            Ok(parts)
        });
    match parts {
        Ok(mut parts) => {
            parts.sort_by_key(|part| part.rows.len() + part.sessions.len());
            // Every Claude source of a computer reads its part (computers are distinct, so
            // each has one part).
            for part in parts {
                let group = shaped.claude_sources(states, part.host);
                let worker = part.worker(frame.now, pricing);
                let remote = vec![json!({ "tool": "claude", "host": part.host, "data": worker })];
                drop(part);
                let built = Shaped::build(frame, states, group, HashMap::new(), remote);
                shaped.keep(ingestor, built);
            }
        }
        Err(error) => {
            tracing::warn!(
                code = error.code,
                "Claude mirror owners could not be updated"
            );
            for index in indexes {
                states[index]["state"] = json!("unreadable");
                states[index]["cause"] = json!("store_unavailable");
            }
        }
    }
}

/// A pass's start: the scheduler's clock reading and the monotonic instant it was taken.
struct PassClock {
    started_ms: i64,
    started: std::time::Instant,
}

impl PassClock {
    fn start(now: i64) -> Self {
        PassClock {
            started_ms: now,
            started: std::time::Instant::now(),
        }
    }

    /// Record the pass as fetched when it finished, so the next one waits a full interval
    /// after this one ends (a 65 s pass with a 60 s interval must not restart 15 s later).
    fn finish(&self, state: &mut WriterState) {
        let elapsed = i64::try_from(self.started.elapsed().as_millis()).unwrap_or(i64::MAX);
        state.fetched_at = self.started_ms.saturating_add(elapsed);
    }
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
fn valid_scan_answer(answer: &Answer, kinds: &[String]) -> bool {
    if answer.version.as_u64() != Some(2) {
        return false;
    }
    if answer.reader["name"].as_str() != Some("aac-usage-scan") {
        return false;
    }
    let Some(answer_kinds) = answer.kinds.as_ref() else {
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

/// A scan's source state. `readable` says the kind has readable usage worth keeping: rows in
/// the answer or rows the cache already holds for it. A few unreadable records beside readable
/// rows stay partial (the Devin case); only a scan with no readable records, or a read
/// failure, is unreadable.
fn scan_state(answer: &Answer, kind: &str, readable: bool) -> (&'static str, Option<&'static str>) {
    let read = answer.read(kind);
    let state = read["state"].as_str();
    if read["unreadable"] == true
        || ["readFailures", "unreadableFolders"]
            .iter()
            .any(|key| read[key].as_u64().is_some_and(|count| count > 0))
    {
        return ("unreadable", Some("read_failed"));
    }
    let unreadable_records = read["unreadableRecords"]
        .as_u64()
        .is_some_and(|count| count > 0);
    if unreadable_records && !readable {
        return ("unreadable", Some("read_failed"));
    }
    if read["partial"] == true || unreadable_records || state == Some("pending") {
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

/// Whether the kind has readable rows: in the answer or in the cache's hold.
fn has_readable_records(answer: &Answer, cache: &scan_state::Cache, kind: &str) -> bool {
    answer.had_rows(kind)
        || cache
            .rows
            .iter()
            .chain(cache.srows.iter())
            .any(|row| &*row.kind == kind)
}

/// Whether the answer itself is incomplete: record-level read errors or a non-final read
/// state. Such an answer is applied as incomplete, whatever the cache held before (r1 High).
fn answer_incomplete(answer: &Answer, kind: &str) -> bool {
    let read = answer.read(kind);
    read["unreadable"] == true
        || ["readFailures", "unreadableFolders", "unreadableRecords"]
            .iter()
            .any(|key| read[key].as_u64().is_some_and(|count| count > 0))
        || read["partial"] == true
        || matches!(
            read["state"].as_str(),
            Some("pending" | "error" | "unreadable")
        )
}

/// One tool of a valid scanner answer applied to the cache: its source state. The writer reports
/// the tool's cached rows, read in place from the cache ([`scan_state::of_kind`]).
/// `flags` are (settled prints packed, source limited, experiment discovery still running).
fn scanned_kind(
    answer: &mut Answer,
    kind: &str,
    host: &str,
    (now, cutoff): (i64, i64),
    (packed, limited, discovering): (bool, bool, bool),
    cache: &mut scan_state::Cache,
) -> Value {
    let settle = |answer: &Answer, readable: bool| -> (&'static str, Option<&'static str>) {
        let (state, cause) = source_state(answer, kind, readable);
        if limited {
            ("unreadable", Some("source_limit"))
        } else if matches!(state, "ok" | "not_installed" | "no_usage") && discovering {
            ("scanning", None)
        } else {
            (state, cause)
        }
    };
    let state = settle(answer, has_readable_records(answer, cache, kind)).0;
    let complete = matches!(state, "ok" | "no_usage");
    let compatible = !packed || answer.read(kind)["settledPacked"] == true;
    let incomplete = answer_incomplete(answer, kind);
    let applied = cache.apply(kind, answer, complete && compatible && !incomplete, cutoff);
    // Whether readable usage survives is decided from the cache as the apply left it: a
    // changed file whose cached rows were replaced by none leaves nothing readable, so the
    // scan is a read failure, never "Scanning" with zero published cells (r1 High).
    let (mut state, mut cause) = settle(answer, has_readable_records(answer, cache, kind));
    if !applied && complete {
        (state, cause) = ("scanning", None);
    }
    // A complete read that holds no usage is no_usage, as Wave 84 reported it (FINAL-ASTRA H2):
    // with no usage there is no data clock to verify. Failed reads, incomplete discovery and
    // unread usage never reach `ok` here, and newer activity still reads as stale in the report.
    let holds = |rows: &[aac_store::scan_row::ScanRow]| rows.iter().any(|r| &*r.kind == kind);
    if state == "ok" && !holds(&cache.rows) && !holds(&cache.srows) {
        state = "no_usage";
    }
    scan_source_state(answer, kind, host, now, state, cause)
}

fn scan_source_state(
    answer: &Answer,
    kind: &str,
    host: &str,
    now: i64,
    state: &str,
    cause: Option<&str>,
) -> Value {
    use aac_store::contract::{Counts, Freshness, MAX_FILE_COUNT};

    let discovery = &answer.discovery["kinds"][kind];
    let freshness = serde_json::from_value::<Freshness>(discovery["freshness"].clone())
        .ok()
        .filter(Freshness::valid_clocks);
    let unmeasured_files = discovery["unmeasuredFiles"]
        .as_u64()
        .filter(|count| (1..=MAX_FILE_COUNT).contains(count));
    let unread_usage = (discovery["unreadUsage"] == true).then_some(true);
    let estimated_clock = (discovery["estimatedClock"] == true).then_some(true);
    let unreadable_records = answer.read(kind)["unreadableRecords"]
        .as_u64()
        .filter(|count| (1..=MAX_FILE_COUNT).contains(count));
    let mut source = json!({
        "tool": kind,
        "host": host,
        "state": state,
        "cause": cause,
        "lastScanAt": aac_num::ecma::to_iso_string(now).unwrap_or_default(),
    });
    if freshness.is_some()
        || unmeasured_files.is_some()
        || unread_usage.is_some()
        || estimated_clock.is_some()
        || unreadable_records.is_some()
    {
        source["counts"] = json!(Counts {
            freshness,
            unmeasured_files,
            unread_usage,
            estimated_clock,
            unreadable_records,
            ..Counts::default()
        });
    }
    source
}

fn presence_state(answer: &Answer, kind: &str) -> Option<(&'static str, Option<&'static str>)> {
    let tool = answer.discovery.get("tools")?.get(kind)?;
    if tool.get("installed") != Some(&Value::Bool(true)) {
        return None;
    }
    match tool.get("cause").and_then(Value::as_str) {
        Some("no_local_usage_records") => Some(("no_reader", Some("no_local_usage_records"))),
        Some("no_reader_yet") => Some(("no_reader", Some("no_reader_yet"))),
        _ => Some(("installed_no_logs", None)),
    }
}

fn source_state(
    answer: &Answer,
    kind: &str,
    readable: bool,
) -> (&'static str, Option<&'static str>) {
    let (state, cause) = scan_state(answer, kind, readable);
    if matches!(state, "not_installed")
        && let Some(presence) = presence_state(answer, kind)
    {
        return presence;
    }
    (state, cause)
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
        "deadlineMs": SCANNER_BUDGET_MS,
        "presence": true,
    });
    if let Some(roots) = experiment_roots.filter(|roots| !roots.is_empty()) {
        request["experimentRoots"] = json!(roots);
    }
    request
}

/// One scan request. The arguments' text is held once: shared with the transport, and kept
/// past the agent's call only for the fixed scanner's fallback.
async fn scan_host(
    agents: &Arc<crate::agent_transport::Agents>,
    host: &crate::agent_transport::UsageHost,
    args: Arc<crate::agent_transport::ArgsText>,
    timeout_ms: u64,
    call_id: &str,
) -> Result<Vec<u8>, aac_core::Cause> {
    let answer = if host.agent_available {
        agents
            .scan_usage(
                host.platform,
                host.alias.as_deref(),
                Arc::clone(&args),
                timeout_ms,
                SCAN_MAX_BYTES,
                call_id,
            )
            .await
    } else {
        Err(aac_core::Cause::AgentMissing)
    };
    if matches!(
        answer,
        Err(aac_core::Cause::AgentMissing | aac_core::Cause::AgentMismatch)
    ) {
        fixed_scanner::scan(agents, host, args, timeout_ms, SCAN_MAX_BYTES).await
    } else {
        answer
    }
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
    let _reclaim = crate::allocator::reclaim_on_drop();
    let clock = PassClock::start(now);
    let computers = agents.usage_hosts().await;
    if computers.is_empty() {
        let _pass = live.pass();
        clock.finish(&mut state.lock().unwrap_or_else(|p| p.into_inner()));
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
            refresh.ensure(false, now_ms);
        });
    }
    let pricing = ResolverPricing::new(ccs_dir);
    let mut claude = Vec::new();
    let mut states: Vec<Value> = Vec::new();
    let preferences = crate::account_reads::preferences(ccs_dir);
    let kinds = aac_providers::scanner_kinds()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let scanner_owned: HashSet<String> = kinds.iter().cloned().collect();
    // Only the jsonl read finishes locally; it is known once the computers are read, so the
    // frame of the reports built before then never names it (they never read `local`).
    let no_local: HashSet<String> = HashSet::new();
    let frame = ReportFrame {
        window_from_ms: cutoff,
        now,
        local_complete: &no_local,
        scanner_owned: &scanner_owned,
    };
    let mut shaped = Shaped::default();
    // Saved Generic JSONL sources have no scanner kind: this writer reads them itself.
    let mut mapped: Vec<Value> = Vec::new();
    let mut mapped_state = false;
    let mut mapped_complete = false;
    'computers: for computer in &computers {
        let host = computer.id.as_str();
        let mut cache = match scan_state::Cache::load(engine, host) {
            Ok(cache) => cache,
            Err(error) => {
                tracing::warn!(
                    code = error.code,
                    "Stored Analytics scanner state could not be loaded"
                );
                states.extend(failed_scan_states(host, &kinds, false));
                continue;
            }
        };
        let (mut extra_roots, mut overflow, generic) = extra_roots(preferences.as_ref(), host);
        let local = computer.alias.is_none();
        let experiments = experiments.as_deref().filter(|_| local);
        if let Some(experiments) = experiments {
            experiments.add_zcode_paths(&mut extra_roots, &mut overflow);
        }
        if generic {
            // Generic JSONL folders are read in place: from a dashboard elsewhere, the VM's are
            // unreadable, never empty.
            let read = if local {
                analytics_jsonl::read(preferences.as_ref(), host, cutoff, now, &pricing)
            } else {
                analytics_jsonl::not_in_place(preferences.as_ref(), host, now)
            };
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
        let settled_before = activity::settled_before(engine, host).unwrap_or_default();
        let requests =
            scan_state::Requests::new(args, &kinds, &cache.fingerprints, &settled_before, now);
        let mut scanned = false;
        let mut claude_state = None;
        let start_state = states.len();
        // This computer's worker results: shaped into its reports once its checkpoint is saved.
        let mut remote: Vec<Value> = Vec::new();
        for batch in &requests.batches {
            let batch = batch.clone();
            let Some(scan_state::ScanArgs { text, packed }) =
                requests.args(&batch, &cache.fingerprints)
            else {
                for kind in batch {
                    states.push(json!({
                        "tool": kind, "host": host, "state": "unreadable", "cause": "source_limit",
                    }));
                }
                continue;
            };
            let call_id = format!("g14-activity-{generation}-{host}-{}", batch[0]);
            // The checkpoint is not held while the call waits: what this pass changed stays,
            // compressed, and the rest is read back from the store after it.
            let parked = cache.park();
            let answer = scan_host(agents, computer, text, SCAN_DEADLINE_MS, &call_id).await;
            cache = match parked.restore(engine, host) {
                Ok(cache) => cache,
                Err(error) => {
                    tracing::warn!(
                        code = error.code,
                        "Stored Analytics scanner state could not be read back"
                    );
                    // Nothing of this computer's pass is kept or saved.
                    states.truncate(start_state);
                    drop(remote);
                    states.extend(requests.batches.iter().flatten().map(|kind| {
                        json!({
                            "tool": kind, "host": host, "state": "unreadable",
                            "cause": "store_unavailable",
                        })
                    }));
                    continue 'computers;
                }
            };
            let answer = answer.and_then(|bytes| {
                Answer::parse(&bytes, &mut cache.strings).map_err(|_| aac_core::Cause::Damaged)
            });
            match answer {
                Ok(mut answer) if valid_scan_answer(&answer, &batch) => {
                    scanned = true;
                    for kind in &batch {
                        let packed = packed.contains(kind);
                        let limited = overflow.contains(kind.as_str())
                            || experiments.is_some_and(|snapshot| snapshot.limited(kind));
                        let discovering = local
                            && experiments.map_or_else(
                                || experiment_roots::tracks(kind),
                                |snapshot| snapshot.discovering(kind),
                            );
                        let source = scanned_kind(
                            &mut answer,
                            kind,
                            host,
                            (now, cutoff),
                            (packed, limited, discovering),
                            &mut cache,
                        );
                        if kind == "claude" {
                            // Moved out of the cache once its checkpoint is encoded.
                            claude_state = Some(states.len());
                        } else {
                            let worker = collect::worker_from_rows(
                                &scan_state::of_kind(&cache.rows, kind),
                                &scan_state::of_kind(&cache.srows, kind),
                                host,
                                now,
                                &pricing,
                            );
                            remote.push(json!({ "tool": kind, "host": host, "data": worker }));
                        }
                        states.push(source);
                    }
                }
                Err(aac_core::Cause::AgentMissing) => {
                    states.extend(failed_scan_states(host, &batch, true))
                }
                _ => states.extend(failed_scan_states(host, &batch, false)),
            }
        }
        // Saved now, one computer at a time, and only the parts that changed; a superseded pass
        // leaves the store to the newer one.
        let current =
            scanned && state.lock().unwrap_or_else(|p| p.into_inner()).generation == generation;
        if current {
            match cache.save(engine, host) {
                Ok(written) => tracing::debug!(
                    keys = written.keys,
                    bytes = written.bytes,
                    "Analytics scanner state saved"
                ),
                Err(error) if error.code != "source_limit" => tracing::warn!(
                    code = error.code,
                    "Analytics scanner state could not be saved"
                ),
                Err(error) => {
                    tracing::warn!(
                        code = error.code,
                        "Analytics scanner state could not be saved"
                    );
                    for source in &mut states[start_state..] {
                        if matches!(source["state"].as_str(), Some("ok" | "no_usage")) {
                            source["state"] = json!("unreadable");
                            source["cause"] = json!("source_limit");
                        }
                    }
                }
            }
        }
        if let Some(state_index) = claude_state {
            let (rows, sessions, identities) = cache.take_kind("claude");
            // Set aside, compressed, while the other computers are read.
            claude.push(
                claude_mirrors::Part {
                    host,
                    state_index,
                    rows,
                    sessions,
                    identities,
                }
                .park(),
            );
        }
        drop(cache);
        // This computer's reports of the scanner's tools other than Claude, built now and held
        // trimmed and typed: its worker trees are gone before the next computer is read.
        let early = Shaped::computer_sources(&states, start_state, &scanner_owned);
        let built = Shaped::build(&frame, &states, early, HashMap::new(), remote);
        shaped.keep(
            &state.lock().unwrap_or_else(|p| p.into_inner()).ingestor,
            built,
        );
    }
    // Shape and ingest under the lock; a superseding generation drops this pass's writes.
    let mut guard = state.lock().unwrap_or_else(|p| p.into_inner());
    if generation != guard.generation {
        return;
    }
    shape_claude(
        engine,
        claude,
        &mut states,
        &pricing,
        &frame,
        &mut shaped,
        &guard.ingestor,
    );
    let mut local: HashMap<String, Vec<Value>> = HashMap::new();
    let mut local_complete: HashSet<String> = HashSet::new();
    if mapped_state {
        local.insert("jsonl".into(), mapped);
        if mapped_complete {
            local_complete.insert("jsonl".into());
        }
    }
    let rest = shaped.rest_sources(&states);
    let frame = ReportFrame {
        local_complete: &local_complete,
        ..frame
    };
    let built = Shaped::build(&frame, &states, rest, local, Vec::new());
    shaped.keep(&guard.ingestor, built);
    if let Some((typed, remember)) = shaped.finish()
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
    clock.finish(&mut guard);
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

async fn migrate_files(
    engine: Arc<aac_store::engine::Engine>,
    agents: &crate::agent_transport::Agents,
    root: &Path,
) -> bool {
    let local = agents
        .usage_hosts()
        .await
        .into_iter()
        .find(|host| host.alias.is_none())
        .map(|host| host.id)
        .or_else(|| crate::dashboard_host::this_computer().map(str::to_owned));
    let root = root.to_path_buf();
    tokio::task::spawn_blocking(move || {
        if matches!(migration::Migration::needed(&engine), Ok(false)) {
            return true;
        }
        let env =
            aac_usage::discover::env_from(std::env::vars_os().filter_map(|(key, value)| {
                Some((key.into_string().ok()?, value.into_string().ok()?))
            }));
        let local_originals = local.as_deref().zip(aac_usage::home_dir(&env)).map_or_else(
            std::collections::BTreeSet::new,
            |(host, home)| {
                let prefs = crate::account_reads::preferences(&root);
                let (configured, _, _) = extra_roots(prefs.as_ref(), host);
                let experiments =
                    experiment_roots::run_slice(&engine, &home, aac_store::engine::now_ms() as i64)
                        .scanner_roots();
                let extras: Vec<_> = [
                    &configured["claude"],
                    experiments.get("claude").unwrap_or(&Value::Null),
                ]
                .into_iter()
                .flat_map(|roots| roots.as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .map(std::path::PathBuf::from)
                .collect();
                local_inventory::original_names(&home, &env, &extras)
            },
        );
        let migration = migration::Migration {
            store: &engine,
            root: &root,
            local: local.as_deref(),
            local_originals,
            now: aac_store::engine::now_ms() as i64,
            pricing: ResolverPricing::new(&root),
        };
        match migration.run() {
            Ok(done) => done,
            Err(error) => {
                tracing::warn!(code = error.code, "Analytics file migration incomplete");
                false
            }
        }
    })
    .await
    .unwrap_or(false)
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
    let mut migrated = loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        if let Some(engine) = store.analytics_engine() {
            break migrate_files(engine, &agents, &ccs_dir).await;
        }
        tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
    };
    let state = Arc::new(Mutex::new(WriterState::default()));
    let quota_store = Arc::clone(&store);
    let quota_stop = Arc::clone(&stop);
    let quota_root = ccs_dir.clone();
    let quota = async move {
        let state = Arc::new(Mutex::new(QuotaState::default()));
        let dashboard = Arc::new(dashboard);
        while !quota_stop.load(Ordering::Acquire) {
            if let Some(engine) = quota_store.analytics_engine() {
                let state = state.clone();
                let dashboard = dashboard.clone();
                let root = quota_root.clone();
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    let now = aac_store::engine::now_ms() as i64;
                    let mut state = state.lock().unwrap_or_else(|p| p.into_inner());
                    if now - state.last_quota_ms >= quota_interval_ms(refresh_seconds(&root)) {
                        quota_pass(&engine, dashboard(now).as_ref(), &root, &mut state, now);
                    }
                })
                .await
                {
                    report_writer_failure("quota", &error);
                }
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    };
    let activity_store = Arc::clone(&store);
    let activity_stop = Arc::clone(&stop);
    let activity_root = ccs_dir.clone();
    let activity_state = Arc::clone(&state);
    let activity_live = Arc::clone(&live);
    let activity = async move {
        let models_dev = Arc::new(crate::models_dev::ModelsDevRefresh::new(&activity_root));
        let mut folders_cleared = false;
        let mut history = history::Job::default();
        let mut migration_retry = aac_store::engine::now_ms() + 120_000;
        while !activity_stop.load(Ordering::Acquire) {
            if let Some(engine) = activity_store.analytics_engine() {
                let now = aac_store::engine::now_ms();
                // Migration and the historical writer share the Claude owner map.
                if !migrated && now >= migration_retry {
                    migrated = migrate_files(engine.clone(), &agents, &activity_root).await;
                    migration_retry = now + 120_000;
                }
                if crate::analytics_route::capture_cwd(&activity_root) {
                    folders_cleared = false;
                } else if !folders_cleared {
                    folders_cleared =
                        aac_store::analytics_cache::clear_remote_folders(&engine).is_ok();
                }
                if activity_live.take_refresh() {
                    activity_state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .fetched_at = 0;
                }
                activity_pass(
                    &engine,
                    &agents,
                    &activity_state,
                    &activity_root,
                    &models_dev,
                    &activity_live,
                    now as i64,
                )
                .await;
                if !activity_stop.load(Ordering::Acquire) {
                    let experiments = activity_state
                        .lock()
                        .unwrap_or_else(|p| p.into_inner())
                        .experiments
                        .clone();
                    history
                        .run(
                            &engine,
                            &agents,
                            &activity_root,
                            experiments.as_deref(),
                            aac_store::engine::now_ms() as i64,
                        )
                        .await;
                }
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    };
    let discovery = async move {
        let Some(home) = std::env::home_dir() else {
            return;
        };
        let mut next = 0;
        while !stop.load(Ordering::Acquire) {
            let now = aac_store::engine::now_ms() as i64;
            if now >= next
                && let Some(engine) = store.analytics_engine()
            {
                let home = home.clone();
                match tokio::task::spawn_blocking(move || {
                    experiment_roots::run_slice(&engine, &home, now)
                })
                .await
                {
                    Ok(snapshot) => {
                        next = snapshot.next_slice(now);
                        let done = next > now;
                        state.lock().unwrap_or_else(|p| p.into_inner()).experiments =
                            Some(Arc::new(snapshot));
                        if done {
                            live.request_refresh();
                        }
                    }
                    Err(error) => report_writer_failure("experiment discovery", &error),
                }
            }
            tokio::time::sleep(Duration::from_millis(TICK_MS)).await;
        }
    };
    tokio::join!(
        run_writer("quota", quota),
        run_writer("experiment discovery", discovery),
        run_writer("activity", activity),
    );
}

async fn run_writer(name: &'static str, writer: impl Future<Output = ()> + Send + 'static) {
    if let Err(error) = tokio::spawn(writer).await {
        report_writer_failure(name, &error);
    }
}

fn report_writer_failure(name: &'static str, error: &tokio::task::JoinError) {
    use std::io::Write as _;

    // The packaged dashboard can launch without a tracing subscriber.
    let cause = if error.is_panic() {
        "panicked"
    } else {
        "cancelled"
    };
    let _ = writeln!(
        std::io::stderr(),
        "aac-http: Analytics {name} writer task {cause}"
    );
}

#[cfg(test)]
mod tests {
    use aac_store::scan_row::ScanRow;

    /// The writer's per-tool step, plus the tool's cached rows as the scanner's objects.
    fn scanned_kind(
        answer: &serde_json::Value,
        kind: &str,
        host: &str,
        times: (i64, i64),
        flags: (bool, bool, bool),
        cache: &mut super::scan_state::Cache,
    ) -> (
        serde_json::Value,
        Vec<serde_json::Value>,
        Vec<serde_json::Value>,
    ) {
        let source = super::scanned_kind(
            &mut super::Answer::from_value(answer),
            kind,
            host,
            times,
            flags,
            cache,
        );
        let values = |rows: &[ScanRow]| {
            super::scan_state::of_kind(rows, kind)
                .into_iter()
                .map(ScanRow::to_value)
                .collect()
        };
        (source, values(&cache.rows), values(&cache.srows))
    }

    fn typed(rows: &[serde_json::Value]) -> Vec<ScanRow> {
        rows.iter()
            .map(|row| serde_json::from_value(row.clone()).expect("scanner row"))
            .collect()
    }

    #[test]
    fn a_long_activity_pass_waits_a_full_interval_after_it_ends() {
        let Some(started) =
            std::time::Instant::now().checked_sub(std::time::Duration::from_secs(65))
        else {
            return;
        };
        let start = 1_800_000_000_000;
        let mut state = super::WriterState::default();
        super::PassClock {
            started_ms: start,
            started,
        }
        .finish(&mut state);
        // The 65 s pass ended 5 s after the 60 s interval would have run from its start.
        assert!(!super::activity_due(&state, start + 60_000, 60_000));
        assert!(!super::activity_due(&state, start + 120_000, 60_000));
        assert!(super::activity_due(&state, start + 126_000, 60_000));
    }

    #[test]
    fn the_scanner_budget_leaves_room_inside_the_scan_call() {
        // The agent stops a scan call at SCAN_DEADLINE_MS; the scanner must return its partial
        // rows before that, so a long first scan resumes instead of failing the whole computer.
        let request = scan_request(&["codex".to_owned()], 0, serde_json::json!({}), false, None);
        let budget = request["deadlineMs"].as_u64().expect("deadlineMs");
        assert!(budget <= 120_000, "the scanner refuses budgets over 120 s");
        assert!(
            budget + 20_000 <= SCAN_DEADLINE_MS,
            "budget {budget} leaves no margin"
        );
    }

    use super::*;
    use aac_store::report::ReportArgs;

    #[tokio::test]
    async fn analytics_writer_failure_leaves_a_later_quota_pass_running() {
        let (failed, after_failure) = tokio::sync::oneshot::channel();
        let (sampled, sample) = tokio::sync::oneshot::channel();
        let activity = async {
            run_writer("activity", async { panic!("fixture activity failure") }).await;
            failed.send(()).expect("quota still waiting");
        };
        let quota = run_writer("quota", async {
            after_failure.await.expect("activity failure observed");
            sampled.send(()).expect("later quota pass observed");
        });
        tokio::join!(activity, quota);
        sample.await.expect("quota ran after the activity panic");
    }

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
        let store =
            aac_store::ops::open_engine(&ccs.path().join("store").to_string_lossy(), None, None)
                .expect("open fixture store");
        let snapshot = experiment_roots::run_slice(&store, home.path(), 1);
        let roots = snapshot.scanner_roots();
        let kinds = aac_providers::scanner_kinds()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        let request = scan_request(&kinds, 0, json!({}), false, Some(&roots));
        assert_eq!(request["experimentRoots"], json!({"codex": [root]}));
        assert_eq!(request["cwd"], false);
        assert_eq!(request["presence"], true);
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
    fn analytics_projects_scanner_coverage_through_store_to_view() {
        let dir = tempfile::tempdir().expect("fixture home");
        let engine = Arc::new(
            aac_store::ops::open_engine(dir.path().to_str().expect("fixture path"), None, None)
                .expect("open activity store"),
        );
        let now = aac_store::engine::now_ms() as i64 - 60_000;
        let data_at = now - 3_600_000;
        let hour = aac_num::ecma::to_iso_string(data_at).expect("hour");
        let mut answer = json!({
            "version": 2, "reader": {"name": "aac-usage-scan"},
            "rows": [{"f": "sdk", "k": "cursor", "p": "anthropic", "m": "claude-opus-4-5",
                "h": hour[..13].replace('T', " ") + ":00", "i": 100, "o": 0, "n": 1}],
            "srows": [],
            "kinds": {"cursor": {"state":"ok"}},
            "discovery": {"kinds": {"cursor": {
                "freshness": {"state":"current", "newestDataMs":data_at,
                    "newestActivityMs":data_at, "activityComplete":true},
                "unmeasuredFiles":4
            }}}
        });
        let pass = |answer: &Value, now| {
            let typed = Answer::from_value(answer);
            assert!(valid_scan_answer(&typed, &["cursor".into()]));
            let readable = answer["rows"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["k"].as_str() == Some("cursor")));
            let (state, cause) = scan_state(&typed, "cursor", readable);
            let source = scan_source_state(&typed, "cursor", "ubuntu", now, state, cause);
            let worker = collect::worker_from_scan(
                answer["rows"].as_array().expect("scanner hourly rows"),
                &[],
                "cursor",
                "ubuntu",
                now,
                &ResolverPricing::new(dir.path()),
            );
            let reports = report::build_activity_reports(&ReportArgs {
                local: &HashMap::new(),
                remote: &[json!({"tool":"cursor", "host":"ubuntu", "data":worker})],
                states: &[source],
                window_from_ms: now - DAY,
                now,
                local_complete: &HashSet::new(),
                scanner_owned: &["cursor".into()].into_iter().collect(),
            });
            let receipt = ingest_reports(&engine, &reports).expect("ingest scanner report");
            assert_eq!(receipt["refused"], json!([]));
            let range = aac_store::ops::call(
                &engine,
                "activityRange",
                &json!({"fromMs": now - DAY, "toMs": now}).to_string(),
            )
            .expect("stored activity range");
            let tokens: u64 = range["rows"]
                .as_array()
                .expect("range rows")
                .iter()
                .map(|row| row[5].as_u64().expect("input tokens"))
                .sum();
            assert_eq!(tokens, 100, "keep the last measured SDK activity");
            crate::analytics_route::compose(
                engine.clone(),
                dir.path(),
                None,
                &crate::analytics_live::Live::default(),
                Some("range=7d&tz=UTC"),
                now,
            )
            .expect("native Analytics answer")["activity"]
                .clone()
        };
        let current = pass(&answer, now);
        assert_eq!(current["sources"][0]["state"], "ok");
        assert_eq!(current["sources"][0]["unmeasuredFiles"], 4);
        assert!(
            current["sources"][0]["detail"]
                .as_str()
                .expect("unmeasured disclosure")
                .contains("4 local chat data files expose no numeric usage")
        );

        let measured = answer["rows"].clone();
        answer["rows"] = json!([]);
        let freshness = &mut answer["discovery"]["kinds"]["cursor"]["freshness"];
        freshness["state"] = json!("unknown");
        freshness["activityComplete"] = json!(false);
        let unknown = pass(&answer, now + 1);
        assert_eq!(unknown["sources"][0]["state"], "cached");
        assert_eq!(unknown["sources"][0]["cause"], "freshness_unverified");
        assert_eq!(
            unknown["history"]["sources"][0]["freshness"]["newestDataMs"],
            data_at
        );
        assert_eq!(
            unknown["sources"][0]["lastScanAt"],
            current["sources"][0]["lastScanAt"]
        );

        // A usage-shaped record the reader could not decode and a file-time estimate: the measured
        // totals and dates stand, and the view names both pieces of evidence.
        let discovery = &mut answer["discovery"]["kinds"]["cursor"];
        discovery["unreadUsage"] = json!(true);
        discovery["estimatedClock"] = json!(true);
        let disclosed = pass(&answer, now + 2);
        assert_eq!(disclosed["sources"][0]["state"], "cached");
        assert_eq!(disclosed["sources"][0]["cause"], "freshness_unverified");
        let detail = disclosed["sources"][0]["detail"]
            .as_str()
            .expect("coverage disclosure");
        assert!(detail.contains("current usage coverage is unverified"));
        assert!(detail.contains(
            "a usage-shaped record could not be read; its tokens are not counted until a later scan reads it"
        ));
        assert!(detail.contains(
            "some usage records carry no event timestamp; their file times are not used as usage dates"
        ));
        assert_eq!(disclosed["sources"][0]["unmeasuredFiles"], 4);
        assert_eq!(
            disclosed["history"]["sources"][0]["freshness"]["newestDataMs"], data_at,
            "measured date kept while the new evidence is incomplete"
        );
        assert_eq!(
            disclosed["history"]["sources"][0]["unreadUsage"],
            json!(true)
        );
        assert_eq!(
            disclosed["history"]["sources"][0]["estimatedClock"],
            json!(true)
        );

        // An estimated date alone is disclosed and keeps a current clock: numbers are counted.
        let discovery = &mut answer["discovery"]["kinds"]["cursor"];
        discovery["unreadUsage"] = Value::Null;
        discovery["freshness"]["state"] = json!("current");
        discovery["freshness"]["activityComplete"] = json!(true);
        answer["rows"] = measured;
        let estimated = pass(&answer, now + 3);
        assert_eq!(estimated["sources"][0]["state"], "ok");
        assert_eq!(estimated["sources"][0]["cause"], Value::Null);
        let detail = estimated["sources"][0]["detail"]
            .as_str()
            .expect("estimate disclosure");
        assert!(detail.contains("no event timestamp"));
        assert!(!detail.contains("usage-shaped record"));
        assert_eq!(
            estimated["history"]["sources"][0]["unreadUsage"],
            Value::Null,
            "a verified read clears the disclosure"
        );

        let freshness = &mut answer["discovery"]["kinds"]["cursor"]["freshness"];
        freshness["state"] = json!("stale");
        freshness["newestActivityMs"] = json!(now);
        let stale = pass(&answer, now + 4);
        assert_eq!(stale["sources"][0]["state"], "stale");
        assert_eq!(stale["sources"][0]["cause"], "newer_activity");
        let detail = stale["sources"][0]["detail"]
            .as_str()
            .expect("stale disclosure");
        assert!(detail.contains("newer activity"));
        assert!(detail.contains("no numeric usage"));

        answer["rows"] = json!([]);
        answer["kinds"]["cursor"]["readFailures"] = json!(1);
        let failed = pass(&answer, now + 5);
        assert_eq!(failed["sources"][0]["state"], "unreadable");
        assert_eq!(failed["sources"][0]["cause"], "read_failed");
        let detail = failed["sources"][0]["detail"]
            .as_str()
            .expect("failure disclosure");
        assert!(detail.contains("its logs were found but could not be read"));
        assert!(detail.contains("no numeric usage"));
    }

    /// A few unreadable records keep the readable usage: the source is partial and the page
    /// names how many records were unreadable (the Devin case).
    #[test]
    fn unreadable_records_project_to_a_partial_source_with_the_count() {
        let dir = tempfile::tempdir().expect("fixture home");
        let engine = Arc::new(
            aac_store::ops::open_engine(dir.path().to_str().expect("fixture path"), None, None)
                .expect("open activity store"),
        );
        let now = aac_store::engine::now_ms() as i64 - 60_000;
        let data_at = now - 3_600_000;
        let hour = aac_num::ecma::to_iso_string(data_at).expect("hour");
        let answer = json!({
            "version": 2, "reader": {"name": "aac-usage-scan"},
            "rows": [{"f": "sdk", "k": "cursor", "p": "anthropic", "m": "claude-opus-4-5",
                "h": hour[..13].replace('T', " ") + ":00", "i": 100, "o": 0, "n": 1}],
            "srows": [],
            "kinds": {"cursor": {"state":"ok", "partial":true, "unreadableRecords":2}},
            "discovery": {"kinds": {"cursor": {
                "freshness": {"state":"current", "newestDataMs":data_at,
                    "newestActivityMs":data_at, "activityComplete":true}
            }}}
        });
        assert!(valid_scan_answer(
            &Answer::from_value(&answer),
            &["cursor".into()]
        ));
        let (state, cause) = scan_state(&Answer::from_value(&answer), "cursor", true);
        let source = scan_source_state(
            &Answer::from_value(&answer),
            "cursor",
            "ubuntu",
            now,
            state,
            cause,
        );
        let worker = collect::worker_from_scan(
            answer["rows"].as_array().expect("scanner hourly rows"),
            &[],
            "cursor",
            "ubuntu",
            now,
            &ResolverPricing::new(dir.path()),
        );
        let reports = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &[json!({"tool":"cursor", "host":"ubuntu", "data":worker})],
            states: &[source],
            window_from_ms: now - DAY,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &["cursor".into()].into_iter().collect(),
        });
        assert_eq!(reports.len(), 1);
        assert_eq!(
            reports[0]["state"], "partial",
            "readable usage stays partial"
        );
        assert_eq!(reports[0]["cause"], Value::Null);
        assert_eq!(
            reports[0]["counts"]["unreadableRecords"],
            json!(2),
            "the unreadable count reaches the report"
        );
        assert_eq!(
            reports[0]["cells"].as_array().expect("report cells").len(),
            1,
            "the readable hour is kept"
        );
        let receipt = ingest_reports(&engine, &reports).expect("ingest scanner report");
        assert_eq!(receipt["refused"], json!([]));
        let activity = crate::analytics_route::compose(
            engine.clone(),
            dir.path(),
            None,
            &crate::analytics_live::Live::default(),
            Some("range=7d&tz=UTC"),
            now,
        )
        .expect("native Analytics answer")["activity"]
            .clone();
        assert_eq!(activity["sources"][0]["state"], "cached");
        assert_eq!(activity["sources"][0]["cause"], Value::Null);
        let detail = activity["sources"][0]["detail"]
            .as_str()
            .expect("partial disclosure");
        assert!(
            detail.contains("2 records unreadable"),
            "the page names the count: {detail}"
        );
        assert_eq!(activity["history"]["sources"][0]["state"], "partial");
        assert_eq!(
            activity["history"]["sources"][0]["unreadableRecords"],
            json!(2)
        );
        let range = aac_store::ops::call(
            &engine,
            "activityRange",
            &json!({"fromMs": now - DAY, "toMs": now}).to_string(),
        )
        .expect("stored activity range");
        let tokens: u64 = range["rows"]
            .as_array()
            .expect("range rows")
            .iter()
            .map(|row| row[5].as_u64().expect("input tokens"))
            .sum();
        assert_eq!(tokens, 100, "the readable usage is still counted");
    }

    #[test]
    fn an_installed_cursor_cli_with_chats_is_not_uninstalled() {
        let home = tempfile::tempdir().expect("fixture home");
        let chat = home.path().join(".cursor/chats/x/store.db");
        std::fs::create_dir_all(chat.parent().expect("chat folder")).expect("chat folder");
        std::fs::write(&chat, "").expect("chat store");
        let env = aac_usage::discover::Env::from([
            (
                "HOME".to_string(),
                home.path().to_string_lossy().into_owned(),
            ),
            ("PATH".to_string(), String::new()),
        ]);
        let request = scan_request(
            &["cursor".to_owned()],
            1_790_000_000_000,
            json!({}),
            false,
            None,
        );
        let answer = aac_usage::protocol::scan(
            &aac_usage::protocol::parse_request(request.to_string().as_bytes())
                .expect("native scanner request"),
            home.path(),
            &env,
        );
        assert_eq!(answer["kinds"]["cursor"]["state"], "not_installed");
        assert_eq!(
            answer["discovery"]["tools"]["cursor"]["cause"],
            "no_local_usage_records"
        );
        assert_eq!(
            source_state(&Answer::from_value(&answer), "cursor", false),
            ("no_reader", Some("no_local_usage_records"))
        );
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "cursor", false),
            ("not_installed", None)
        );
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
    /// FINAL-ASTRA H2: complete empty reads are no_usage with a complete read, and unreadable
    /// reads stay incomplete, through the scanner, the writer's per-tool pass, Claude's mirror
    /// selection, the reports and the Store.
    #[cfg(unix)]
    #[test]
    fn analytics_empty_and_unreadable_sources_reach_the_store() {
        use std::os::unix::fs::PermissionsExt;
        let store_dir = tempfile::tempdir().expect("fixture store");
        let engine = aac_store::ops::open_engine(
            store_dir.path().to_str().expect("fixture path"),
            None,
            None,
        )
        .expect("open activity store");
        let now = aac_store::engine::now_ms() as i64 - 60_000;
        let cutoff = now - 31 * DAY;
        let kinds = ["codex".to_owned(), "claude".to_owned()];
        let empty = tempfile::tempdir().expect("empty home");
        let unreadable = tempfile::tempdir().expect("unreadable home");
        let mut locked = Vec::new();
        for folder in [".codex/sessions", ".claude/projects"] {
            std::fs::create_dir_all(empty.path().join(folder)).expect("empty log folder");
            let inner = unreadable.path().join(folder).join("held");
            std::fs::create_dir_all(&inner).expect("held log folder");
            std::fs::set_permissions(&inner, std::fs::Permissions::from_mode(0o000))
                .expect("lock folder");
            locked.push(inner);
        }
        let pricing = ResolverPricing::new(store_dir.path());
        let mut states: Vec<Value> = Vec::new();
        let mut remote: Vec<Value> = Vec::new();
        let mut claude = Vec::new();
        for (host, home) in [("mac", empty.path()), ("nas1", unreadable.path())] {
            let mut cache = scan_state::Cache::default();
            let args = scan_request(&kinds, cutoff, json!({}), false, None);
            let [(batch, request)] =
                scan_state::requests(args, &kinds, &cache.fingerprints, &HashMap::new(), now)
                    .try_into()
                    .expect("one scanner request");
            let request = aac_usage::protocol::parse_request(
                request.expect("scanner arguments").to_string().as_bytes(),
            )
            .expect("scanner request");
            let answer =
                aac_usage::protocol::scan(&request, home, &aac_usage::discover::Env::default());
            assert!(valid_scan_answer(&Answer::from_value(&answer), &batch));
            for kind in &batch {
                let (source, rows, srows) = scanned_kind(
                    &answer,
                    kind,
                    host,
                    (now, cutoff),
                    (false, false, false),
                    &mut cache,
                );
                if kind == "claude" {
                    claude.push(claude_mirrors::Part {
                        host,
                        state_index: states.len(),
                        rows: typed(&rows),
                        sessions: typed(&srows),
                        identities: cache
                            .fingerprints
                            .get("claude")
                            .map(claude_mirrors::Identities::of)
                            .unwrap_or_default(),
                    });
                } else {
                    let worker =
                        collect::worker_from_scan(&rows, &srows, kind, host, now, &pricing);
                    remote.push(json!({ "tool": kind, "host": host, "data": worker }));
                }
                states.push(source);
            }
        }
        for folder in &locked {
            std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))
                .expect("unlock folder");
        }
        claude_mirrors::select_live(&engine, &mut claude, &mut states).expect("mirror owners");
        for part in claude {
            let worker = part.worker(now, &pricing);
            remote.push(json!({ "tool": "claude", "host": part.host, "data": worker }));
        }
        let reports = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &remote,
            states: &states,
            window_from_ms: cutoff,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &kinds.iter().cloned().collect(),
        });
        assert_eq!(
            ingest_reports(&engine, &reports).expect("ingest")["refused"],
            json!([])
        );
        let sources = aac_store::ops::call(
            &engine,
            "activitySources",
            &json!({ "fromMs": cutoff, "toMs": now }).to_string(),
        )
        .expect("activitySources");
        let source = |host: &str, tool: &str| {
            sources["sources"]
                .as_array()
                .expect("sources")
                .iter()
                .find(|s| s["computer"] == host && s["tool"] == tool)
                .cloned()
                .unwrap_or_else(|| panic!("no {tool} source on {host}"))
        };
        for tool in ["codex", "claude"] {
            let idle = source("mac", tool);
            assert_eq!(
                (&idle["state"], &idle["cause"], &idle["complete"]),
                (&json!("no_usage"), &Value::Null, &json!(true)),
                "{tool} idle install"
            );
            assert!(idle["lastCompleteMs"].is_number(), "{tool} complete read");
            assert_eq!(idle["counts"]["freshness"]["activityComplete"], true);
            let held = source("nas1", tool);
            assert_eq!(held["state"], "unreadable", "{tool} unreadable folder");
            assert_eq!(held["cause"], "read_failed", "{tool} unreadable folder");
            assert_eq!(held["complete"], false, "{tool} unreadable folder");
            assert!(held["lastCompleteMs"].is_null(), "{tool} unreadable folder");
        }
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
        assert!(valid_scan_answer(&Answer::from_value(&good), &kinds));
        let wrong_version = json!({ "version": 1, "reader": { "name": "aac-usage-scan" }, "kinds": { "claude": {}, "codex": {} } });
        assert!(!valid_scan_answer(
            &Answer::from_value(&wrong_version),
            &kinds
        ));
        let wrong_kinds = json!({ "version": 2, "reader": { "name": "aac-usage-scan" }, "kinds": { "claude": {} } });
        assert!(!valid_scan_answer(
            &Answer::from_value(&wrong_kinds),
            &kinds
        ));
    }

    #[test]
    fn partial_and_failed_scans_do_not_claim_empty_usage() {
        let answer = json!({"kinds": {
            "claude": {"state":"ok", "partial":true},
            "codex": {"state":"ok", "readFailures":1},
            "antigravity": {"state":"unreadable"},
            "omp": {"state":"ok"},
        }});
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "claude", true),
            ("scanning", None)
        );
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "codex", true),
            ("unreadable", Some("read_failed"))
        );
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "antigravity", false),
            ("unreadable", Some("read_failed"))
        );
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "omp", false),
            ("ok", None)
        );
    }

    /// Some records unreadable beside readable ones: the scan is partial, not a read failure,
    /// and the source counts carry how many records were unreadable (the Devin case).
    #[test]
    fn unreadable_records_with_readable_rows_is_partial() {
        let answer = json!({
            "rows": [{"k": "devin", "f": "sessions.db"}],
            "srows": [],
            "kinds": {"devin": {"state": "ok", "partial": true, "unreadableRecords": 2}},
            "discovery": {"kinds": {"devin": {}}},
        });
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "devin", true),
            ("scanning", None)
        );
        let source = scan_source_state(
            &Answer::from_value(&answer),
            "devin",
            "ubuntu",
            1_700_000_000_000,
            "scanning",
            None,
        );
        assert_eq!(source["state"], "scanning");
        assert_eq!(source["counts"]["unreadableRecords"], json!(2));
    }

    /// r1 Medium: a file read failure with a readable cell keeps the failure; the
    /// mixed-record note never appears on cause `read_failed`, even through the Store
    /// (report `partial` -> grid `cached`) with the count attached.
    #[test]
    fn a_read_failure_never_gains_the_mixed_record_note() {
        let dir = tempfile::tempdir().expect("fixture home");
        let engine = Arc::new(
            aac_store::ops::open_engine(dir.path().to_str().expect("fixture path"), None, None)
                .expect("open activity store"),
        );
        let now = aac_store::engine::now_ms() as i64 - 60_000;
        let data_at = now - 3_600_000;
        let hour = aac_num::ecma::to_iso_string(data_at).expect("hour");
        let answer = json!({
            "version": 2, "reader": {"name": "aac-usage-scan"},
            "rows": [{"f": "store.db", "k": "cursor", "p": "anthropic", "m": "claude-opus-4-5",
                "h": hour[..13].replace('T', " ") + ":00", "i": 100, "o": 0, "n": 1}],
            "srows": [],
            "kinds": {"cursor": {"state":"ok", "partial":true, "unreadableRecords":2, "readFailures":1}},
            "discovery": {"kinds": {"cursor": {
                "freshness": {"state":"current", "newestDataMs":data_at,
                    "newestActivityMs":data_at, "activityComplete":true}
            }}}
        });
        assert!(valid_scan_answer(
            &Answer::from_value(&answer),
            &["cursor".into()]
        ));
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "cursor", true),
            ("unreadable", Some("read_failed"))
        );
        let source = scan_source_state(
            &Answer::from_value(&answer),
            "cursor",
            "ubuntu",
            now,
            "unreadable",
            Some("read_failed"),
        );
        let worker = collect::worker_from_scan(
            answer["rows"].as_array().expect("scanner hourly rows"),
            &[],
            "cursor",
            "ubuntu",
            now,
            &ResolverPricing::new(dir.path()),
        );
        let reports = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &[json!({"tool":"cursor", "host":"ubuntu", "data":worker})],
            states: &[source],
            window_from_ms: now - DAY,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &["cursor".into()].into_iter().collect(),
        });
        assert_eq!(
            reports[0]["state"], "partial",
            "verified cells stay partial"
        );
        assert_eq!(reports[0]["cause"], json!("read_failed"));
        assert_eq!(reports[0]["counts"]["unreadableRecords"], json!(2));
        let receipt = ingest_reports(&engine, &reports).expect("ingest scanner report");
        assert_eq!(receipt["refused"], json!([]));
        let activity = crate::analytics_route::compose(
            engine.clone(),
            dir.path(),
            None,
            &crate::analytics_live::Live::default(),
            Some("range=7d&tz=UTC"),
            now,
        )
        .expect("native Analytics answer")["activity"]
            .clone();
        assert_eq!(activity["sources"][0]["state"], "cached");
        assert_eq!(activity["sources"][0]["cause"], json!("read_failed"));
        let detail = activity["sources"][0]["detail"]
            .as_str()
            .expect("failure disclosure");
        assert!(
            detail.contains("could not be read"),
            "the failure stands: {detail}"
        );
        assert!(
            !detail.contains("records unreadable"),
            "no mixed-record note on a read failure: {detail}"
        );
    }

    /// r2 High: rows dropped after the cache apply (here the worker's future-dated cut-off)
    /// must still end `unreadable/read_failed` when the published report has zero cells with
    /// unreadable records. Two real Devin replies on a fixed writer clock.
    #[test]
    fn a_worker_dropped_row_leaves_no_readable_usage_claim() {
        let dir = tempfile::tempdir().expect("fixture home");
        let engine = Arc::new(
            aac_store::ops::open_engine(
                dir.path().join("store").to_str().expect("fixture path"),
                None,
                None,
            )
            .expect("open activity store"),
        );
        // The recheck's clock: 12:40 unreadable, 12:50 writer, 13:00 valid (past the 5-minute
        // future slack, so the worker drops it after the cache kept it).
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-25T12:50:00Z")
            .expect("clock")
            .timestamp_millis();
        let db = dir.path().join(".local/share/devin/cli/sessions.db");
        std::fs::create_dir_all(db.parent().expect("fixture parent")).expect("fixture folder");
        let conn = rusqlite::Connection::open(&db).expect("open fixture db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (id TEXT, working_directory TEXT, model TEXT);
             CREATE TABLE IF NOT EXISTS message_nodes (session_id TEXT, chat_message TEXT, created_at INTEGER);",
        )
        .expect("fixture schema");
        conn.execute(
            "INSERT INTO sessions VALUES ('s1', '/tmp', 'devin-model')",
            [],
        )
        .expect("fixture session");
        for (id, at, metrics) in [
            ("m1", now - 600_000, json!({"output_tokens": 5})),
            (
                "m2",
                now + 600_000,
                json!({"input_tokens": 100, "output_tokens": 10, "cache_read_tokens": 0, "cache_creation_tokens": 0}),
            ),
        ] {
            let message = json!({
                "message_id": id,
                "metadata": {
                    "request_id": id,
                    "created_at": aac_num::ecma::to_iso_string(at).expect("reply date"),
                    "generation_model": "devin-model",
                    "metrics": metrics,
                },
            })
            .to_string();
            conn.execute(
                "INSERT INTO message_nodes VALUES ('s1', ?, ?)",
                rusqlite::params![message, at / 1000],
            )
            .expect("fixture reply");
        }
        conn.close().expect("close fixture db");
        let env = aac_usage::discover::Env::from([
            (
                "HOME".to_string(),
                dir.path().to_string_lossy().into_owned(),
            ),
            ("PATH".to_string(), String::new()),
        ]);
        let answer = aac_usage::protocol::scan(
            &aac_usage::protocol::parse_request(
                json!({"kinds":["devin"],"minDateMs":now - DAY,"fingerprints":{}})
                    .to_string()
                    .as_bytes(),
            )
            .expect("scanner request"),
            dir.path(),
            &env,
        );
        assert!(
            valid_scan_answer(&Answer::from_value(&answer), &["devin".into()]),
            "the backend answer is valid: {answer}"
        );
        assert_eq!(answer["kinds"]["devin"]["unreadableRecords"], json!(1));
        assert_eq!(
            answer["rows"].as_array().expect("scanner rows").len(),
            1,
            "the 13:00 row reaches the writer"
        );
        let mut cache = scan_state::Cache::default();
        let (source, kind_rows, _) = scanned_kind(
            &answer,
            "devin",
            "ubuntu",
            (now, now - 31 * DAY),
            (false, false, false),
            &mut cache,
        );
        assert_eq!(
            source["state"], "scanning",
            "the cache still holds the 13:00 row"
        );
        assert_eq!(kind_rows.len(), 1);
        let worker = collect::worker_from_scan(
            &kind_rows,
            &[],
            "devin",
            "ubuntu",
            now,
            &ResolverPricing::new(dir.path()),
        );
        assert!(
            worker["cells"].as_array().expect("worker cells").is_empty(),
            "the worker drops the future-dated hour"
        );
        let reports = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &[json!({"tool":"devin", "host":"ubuntu", "data":worker})],
            states: &[source],
            window_from_ms: now - DAY,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &["devin".into()].into_iter().collect(),
        });
        assert_eq!(reports[0]["state"], "unreadable");
        assert_eq!(reports[0]["cause"], json!("read_failed"));
        let receipt = ingest_reports(&engine, &reports).expect("ingest scanner report");
        assert_eq!(receipt["refused"], json!([]));
        let activity = crate::analytics_route::compose(
            engine.clone(),
            dir.path(),
            None,
            &crate::analytics_live::Live::default(),
            Some("range=7d&tz=UTC"),
            now,
        )
        .expect("native Analytics answer")["activity"]
            .clone();
        assert_eq!(activity["sources"][0]["state"], "unreadable");
        assert_eq!(activity["sources"][0]["cause"], json!("read_failed"));
        let detail = activity["sources"][0]["detail"]
            .as_str()
            .expect("failure disclosure");
        assert!(
            detail.contains("could not be read"),
            "the failure detail stands: {detail}"
        );
        assert!(
            !detail.contains("readable usage included"),
            "no readable-usage claim without published cells: {detail}"
        );
    }

    /// A mixed live read whose only readable hour falls before the Store's settle line adds
    /// no cells, so the source has no usable usage: it is `unreadable/read_failed`, never
    /// `cached` with a "Cached" badge for a source with no usage. Two real Devin scans on a
    /// fixed writer clock: a complete no_usage read advances the settle line, then a mixed
    /// read whose readable hour is older than it.
    #[test]
    fn a_mixed_read_with_no_ingested_cells_is_a_read_failure() {
        let dir = tempfile::tempdir().expect("fixture home");
        let engine = Arc::new(
            aac_store::ops::open_engine(
                dir.path().join("store").to_str().expect("fixture path"),
                None,
                None,
            )
            .expect("open activity store"),
        );
        // The follow-up's clock: 2026-11-25 12:50 UTC.
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-25T12:50:00Z")
            .expect("clock")
            .timestamp_millis();
        // Four days back: older than the 72-hour settle line a complete read sets.
        let old = now - 4 * DAY - 600_000;
        let db = dir.path().join(".local/share/devin/cli/sessions.db");
        std::fs::create_dir_all(db.parent().expect("fixture parent")).expect("fixture folder");
        let conn = rusqlite::Connection::open(&db).expect("open fixture db");
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (id TEXT, working_directory TEXT, model TEXT);
             CREATE TABLE IF NOT EXISTS message_nodes (session_id TEXT, chat_message TEXT, created_at INTEGER);",
        )
        .expect("fixture schema");
        conn.execute(
            "INSERT INTO sessions VALUES ('s1', '/tmp', 'devin-model')",
            [],
        )
        .expect("fixture session");
        conn.close().expect("close fixture db");
        let env = aac_usage::discover::Env::from([
            (
                "HOME".to_string(),
                dir.path().to_string_lossy().into_owned(),
            ),
            ("PATH".to_string(), String::new()),
        ]);
        let scan = |cache: &scan_state::Cache| {
            let mut prints = cache.fingerprints.to_value();
            prints["_store"] = json!({"settledBefore":{}, "settledSlot": 1});
            aac_usage::protocol::scan(
                &aac_usage::protocol::parse_request(
                    json!({"kinds":["devin"],"minDateMs":now - 31 * DAY,"fingerprints":prints})
                        .to_string()
                        .as_bytes(),
                )
                .expect("scanner request"),
                dir.path(),
                &env,
            )
        };
        let mut cache = scan_state::Cache::default();
        // Pass 1: the empty database is a complete no_usage read; it advances the settle line.
        let (source1, _, _) = scanned_kind(
            &scan(&cache),
            "devin",
            "ubuntu",
            (now, now - 31 * DAY),
            (false, false, false),
            &mut cache,
        );
        let worker1 = collect::worker_from_scan(
            &[],
            &[],
            "devin",
            "ubuntu",
            now,
            &ResolverPricing::new(dir.path()),
        );
        let reports1 = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &[json!({"tool":"devin", "host":"ubuntu", "data":worker1})],
            states: &[source1],
            window_from_ms: now - DAY,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &["devin".into()].into_iter().collect(),
        });
        assert_eq!(reports1[0]["state"], "no_usage");
        assert_eq!(reports1[0]["complete"], json!(true));
        let receipt1 = ingest_reports(&engine, &reports1).expect("ingest empty read");
        assert_eq!(receipt1["refused"], json!([]));
        // Pass 2: one readable reply four days old, one unreadable reply now.
        let conn = rusqlite::Connection::open(&db).expect("open fixture db");
        for (id, at, metrics) in [
            (
                "m1",
                old,
                json!({"input_tokens": 100, "output_tokens": 10, "cache_read_tokens": 0, "cache_creation_tokens": 0}),
            ),
            ("m2", now - 60_000, json!({"output_tokens": 5})),
        ] {
            let message = json!({
                "message_id": id,
                "metadata": {
                    "request_id": id,
                    "created_at": aac_num::ecma::to_iso_string(at).expect("reply date"),
                    "generation_model": "devin-model",
                    "metrics": metrics,
                },
            })
            .to_string();
            conn.execute(
                "INSERT INTO message_nodes VALUES ('s1', ?, ?)",
                rusqlite::params![message, at / 1000],
            )
            .expect("fixture reply");
        }
        conn.close().expect("close fixture db");
        let answer2 = scan(&cache);
        assert_eq!(
            answer2["kinds"]["devin"]["unreadableRecords"],
            json!(1),
            "the backend answer is mixed: {answer2}"
        );
        let (source2, kind_rows, _) = scanned_kind(
            &answer2,
            "devin",
            "ubuntu",
            (now, now - 31 * DAY),
            (false, false, false),
            &mut cache,
        );
        assert_eq!(source2["state"], "scanning", "the mixed scan is partial");
        let worker2 = collect::worker_from_scan(
            &kind_rows,
            &[],
            "devin",
            "ubuntu",
            now,
            &ResolverPricing::new(dir.path()),
        );
        let reports2 = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &[json!({"tool":"devin", "host":"ubuntu", "data":worker2})],
            states: &[source2],
            window_from_ms: now - DAY,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &["devin".into()].into_iter().collect(),
        });
        assert_eq!(
            reports2[0]["cells"].as_array().expect("cells").len(),
            1,
            "the mixed report reaches ingest with its one cell: {}",
            reports2[0]
        );
        assert_eq!(reports2[0]["state"], "partial");
        assert_eq!(reports2[0]["counts"]["unreadableRecords"], json!(1));
        let receipt = ingest_reports(&engine, &reports2).expect("ingest mixed report");
        assert_eq!(receipt["refused"], json!([]));
        assert_eq!(receipt["put"], json!(0), "the old hour adds no cells");
        assert_eq!(receipt["deleted"], json!(0));
        let activity = crate::analytics_route::compose(
            engine.clone(),
            dir.path(),
            None,
            &crate::analytics_live::Live::default(),
            Some("range=7d&tz=UTC"),
            now,
        )
        .expect("native Analytics answer")["activity"]
            .clone();
        assert_eq!(activity["sources"][0]["state"], "unreadable");
        assert_eq!(activity["sources"][0]["cause"], json!("read_failed"));
        let detail = activity["sources"][0]["detail"]
            .as_str()
            .expect("failure disclosure");
        assert!(
            detail.contains("could not be read"),
            "the failure detail stands: {detail}"
        );
        assert!(
            !detail.contains("readable usage included"),
            "no readable-usage claim without usable cells: {detail}"
        );
        assert_eq!(activity["sources"][0]["unreadableRecords"], json!(1));
    }

    /// Every record unreadable and nothing readable: the scan is unreadable.
    #[test]
    fn unreadable_records_without_readable_rows_is_unreadable() {
        let answer = json!({
            "rows": [],
            "srows": [],
            "kinds": {"devin": {"state": "ok", "partial": true, "unreadableRecords": 5}},
        });
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "devin", false),
            ("unreadable", Some("read_failed"))
        );
    }

    /// A file-level read failure stays unreadable even beside readable rows.
    #[test]
    fn a_read_failure_stays_unreadable() {
        let answer = json!({
            "rows": [{"k": "cursor", "f": "store.db"}],
            "srows": [],
            "kinds": {"cursor": {"state": "ok", "partial": true, "unreadableRecords": 2, "readFailures": 1}},
        });
        assert_eq!(
            scan_state(&Answer::from_value(&answer), "cursor", true),
            ("unreadable", Some("read_failed"))
        );
    }

    /// r1 High: the last readable row must not be evicted into a "Scanning" state. Two real
    /// Devin scanner answers through the writer and the Store: the second replaces the only
    /// cached reply with an unreadable one, so the source is unreadable/read_failed with the
    /// failure detail, never "Scanning" with zero published cells.
    #[test]
    fn a_replaced_last_readable_row_is_a_read_failure_not_scanning() {
        let dir = tempfile::tempdir().expect("fixture home");
        let engine = Arc::new(
            aac_store::ops::open_engine(
                dir.path().join("store").to_str().expect("fixture path"),
                None,
                None,
            )
            .expect("open activity store"),
        );
        let now = aac_store::engine::now_ms() as i64 - 60_000;
        let data_at = now - 3_600_000;
        let stamp = aac_num::ecma::to_iso_string(data_at).expect("reply date");
        let db = dir.path().join(".local/share/devin/cli/sessions.db");
        std::fs::create_dir_all(db.parent().expect("fixture parent")).expect("fixture folder");
        let write = |metrics: &serde_json::Map<String, Value>| {
            let conn = rusqlite::Connection::open(&db).expect("open fixture db");
            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS sessions (id TEXT, working_directory TEXT, model TEXT);
                 CREATE TABLE IF NOT EXISTS message_nodes (session_id TEXT, chat_message TEXT, created_at INTEGER);
                 DELETE FROM message_nodes;",
            )
            .expect("fixture schema");
            conn.execute(
                "INSERT INTO sessions VALUES ('s1', '/tmp', 'devin-model')",
                [],
            )
            .expect("fixture session");
            let message = json!({
                "message_id": "m1",
                "metadata": {
                    "request_id": "r1",
                    "created_at": stamp,
                    "generation_model": "devin-model",
                    "metrics": metrics,
                },
            })
            .to_string();
            conn.execute(
                "INSERT INTO message_nodes VALUES ('s1', ?, ?)",
                rusqlite::params![message, data_at / 1000],
            )
            .expect("fixture reply");
            conn.close().expect("close fixture db");
        };
        let readable = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "input_tokens": 100, "output_tokens": 10,
            "cache_read_tokens": 0, "cache_creation_tokens": 0,
        }))
        .expect("readable metrics");
        write(&readable);
        let env = aac_usage::discover::Env::from([
            (
                "HOME".to_string(),
                dir.path().to_string_lossy().into_owned(),
            ),
            ("PATH".to_string(), String::new()),
        ]);
        let scan = |cache: &scan_state::Cache| {
            let mut prints = cache.fingerprints.to_value();
            prints["_store"] = json!({"settledBefore":{}, "settledSlot": 1});
            aac_usage::protocol::scan(
                &aac_usage::protocol::parse_request(
                    json!({"kinds":["devin"],"minDateMs":now - DAY,"fingerprints":prints})
                        .to_string()
                        .as_bytes(),
                )
                .expect("scanner request"),
                dir.path(),
                &env,
            )
        };
        let mut cache = scan_state::Cache::default();
        let (first, first_rows, _) = scanned_kind(
            &scan(&cache),
            "devin",
            "ubuntu",
            (now, now - 31 * DAY),
            (false, false, false),
            &mut cache,
        );
        assert_eq!(first["state"], "ok", "the first scan reads the reply");
        assert_eq!(first_rows.len(), 1, "the first scan caches one hourly row");
        let unreadable = serde_json::from_value::<serde_json::Map<String, Value>>(json!({
            "output_tokens": 10,
        }))
        .expect("unreadable metrics");
        write(&unreadable);
        let (second, second_rows, _) = scanned_kind(
            &scan(&cache),
            "devin",
            "ubuntu",
            (now, now - 31 * DAY),
            (false, false, false),
            &mut cache,
        );
        assert_eq!(
            second["state"], "unreadable",
            "no readable usage survives the rescan"
        );
        assert_eq!(second["cause"], json!("read_failed"));
        assert!(
            second_rows.is_empty(),
            "the replaced reply leaves no hourly rows"
        );
        let worker = collect::worker_from_scan(
            &second_rows,
            &[],
            "devin",
            "ubuntu",
            now,
            &ResolverPricing::new(dir.path()),
        );
        let reports = report::build_activity_reports(&ReportArgs {
            local: &HashMap::new(),
            remote: &[json!({"tool":"devin", "host":"ubuntu", "data":worker})],
            states: &[second],
            window_from_ms: now - DAY,
            now,
            local_complete: &HashSet::new(),
            scanner_owned: &["devin".into()].into_iter().collect(),
        });
        assert_eq!(reports[0]["state"], "unreadable");
        let receipt = ingest_reports(&engine, &reports).expect("ingest scanner report");
        assert_eq!(receipt["refused"], json!([]));
        let activity = crate::analytics_route::compose(
            engine.clone(),
            dir.path(),
            None,
            &crate::analytics_live::Live::default(),
            Some("range=7d&tz=UTC"),
            now,
        )
        .expect("native Analytics answer")["activity"]
            .clone();
        assert_eq!(activity["sources"][0]["state"], "unreadable");
        assert_eq!(activity["sources"][0]["cause"], json!("read_failed"));
        let detail = activity["sources"][0]["detail"]
            .as_str()
            .expect("failure disclosure");
        assert!(
            detail.contains("could not be read"),
            "the failure detail stands: {detail}"
        );
        assert!(
            !detail.contains("readable usage included"),
            "no readable-usage claim without usage: {detail}"
        );
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

    #[tokio::test]
    async fn activity_pass_reclaims_allocator_only_when_due() {
        let dir = tempfile::tempdir().expect("temp dir");
        let engine = aac_store::ops::open_engine(
            dir.path().join("store").to_str().expect("store path"),
            None,
            None,
        )
        .expect("open engine");
        let agents = Arc::new(crate::agent_transport::Agents::new(dir.path().to_owned()));
        let models_dev = Arc::new(crate::models_dev::ModelsDevRefresh::new(dir.path()));
        let live = Arc::new(crate::analytics_live::Live::default());
        let now = 1_800_000_000_000;
        let state = Mutex::new(WriterState {
            fetched_at: now,
            ..Default::default()
        });

        let before = crate::allocator::thread_reclaim_passes();
        activity_pass(
            &engine,
            &agents,
            &state,
            dir.path(),
            &models_dev,
            &live,
            now,
        )
        .await;
        assert_eq!(
            crate::allocator::thread_reclaim_passes(),
            before,
            "skipped (not-due) activity_pass must not trigger post-pass reclaim"
        );

        state.lock().unwrap_or_else(|p| p.into_inner()).fetched_at = 0;
        activity_pass(
            &engine,
            &agents,
            &state,
            dir.path(),
            &models_dev,
            &live,
            now,
        )
        .await;
        assert_eq!(
            crate::allocator::thread_reclaim_passes(),
            before + 1,
            "due activity_pass must reclaim allocator pages when the pass ends"
        );
    }
}

#[cfg(test)]
mod shaped_tests {
    use super::*;

    const NOW: i64 = 1_795_212_000_000;
    const HOUR_MS: i64 = 3_600_000;

    fn session_id(seed: usize) -> String {
        format!("{:016x}", 0x5eed_0000_u64 + seed as u64)
    }

    /// One tool's worker result on `host`: `files` files, each a session with hour rows over the
    /// last three days; `bump` raises one file's tokens (a later pass that changed one day).
    fn worker(tool: &str, host: &str, files: usize, bump: u64, dir: &Path) -> Value {
        let mut rows = Vec::new();
        let mut srows = Vec::new();
        for file in 0..files {
            let key = format!("{tool}:/fixture/{host}/{file}.jsonl");
            for hour in 0..3 {
                let at = NOW - (file as i64 % 3) * DAY - hour * HOUR_MS - HOUR_MS;
                let iso = aac_num::ecma::to_iso_string(at).expect("hour");
                let extra = if file == 0 { bump } else { 0 };
                rows.push(
                    json!({"k": tool, "f": key, "m": format!("model-{}", file % 3),
                    "h": format!("{} {}:00", &iso[..10], &iso[11..13]),
                    "i": 100 + file as u64 + extra, "o": 20, "cr": 500, "cw": 30, "n": 2}),
                );
            }
            srows.push(json!({"k": tool, "f": key, "s": session_id(file),
                "m": format!("model-{}", file % 3), "a": NOW - 3 * DAY, "z": NOW - HOUR_MS,
                "i": 300 + file as u64 + bump, "o": 60, "cr": 1500, "cw": 90, "n": 6}));
        }
        collect::worker_from_scan(&rows, &srows, tool, host, NOW, &ResolverPricing::new(dir))
    }

    fn source(tool: &str, host: &str, state: &str) -> Value {
        json!({"tool": tool, "host": host, "state": state, "lastScanAt": iso(NOW - 60_000),
            "counts": {"freshness": {"state": "current", "newestDataMs": NOW - 60_000,
                "newestActivityMs": NOW - 60_000, "activityComplete": true}}})
    }

    fn iso(at: i64) -> String {
        aac_num::ecma::to_iso_string(at).expect("iso")
    }

    /// A pass's sources, computer by computer (as the pass pushes them), with the worker
    /// results each computer's scans returned, the Claude workers, and the local results.
    struct Pass {
        states: Vec<Value>,
        computers: Vec<(usize, Vec<Value>)>,
        claude: Vec<(&'static str, Value)>,
        local: HashMap<String, Vec<Value>>,
        local_complete: HashSet<String>,
    }

    fn pass(bump: u64, dir: &Path) -> Pass {
        let entry = |tool: &str, host: &str, files| json!({"tool": tool, "host": host, "data": worker(tool, host, files, bump, dir)});
        let mut states = Vec::new();
        let mut computers = Vec::new();
        // ubuntu: Claude with a part, codex read twice (an earlier source copies, the last takes),
        // omp read, a failed muse batch, and the Generic JSONL read in place.
        states.extend([
            source("claude", "ubuntu", "ok"),
            source("codex", "ubuntu", "ok"),
            source("codex", "ubuntu", "ok"),
            source("omp", "ubuntu", "ok"),
            json!({"tool": "muse", "host": "ubuntu", "state": "scan_failed",
                "detail": "scan-usage unavailable"}),
            source("jsonl", "ubuntu", "ok"),
        ]);
        computers.push((
            0,
            vec![entry("codex", "ubuntu", 12), entry("omp", "ubuntu", 5)],
        ));
        // nas1: Claude failed (no part), codex read.
        let start = states.len();
        states.extend([
            json!({"tool": "claude", "host": "nas1", "state": "unreadable",
                "cause": "agent_missing"}),
            source("codex", "nas1", "ok"),
        ]);
        computers.push((start, vec![entry("codex", "nas1", 7)]));
        // mac: Claude with a part, a scanner-only tool read, and a store that failed back.
        let start = states.len();
        states.extend([
            source("claude", "mac", "ok"),
            source("cursor", "mac", "ok"),
            json!({"tool": "omp", "host": "mac", "state": "unreadable",
                "cause": "store_unavailable"}),
        ]);
        computers.push((start, vec![entry("cursor", "mac", 4)]));
        let claude = vec![
            ("ubuntu", worker("claude", "ubuntu", 20, bump, dir)),
            ("mac", worker("claude", "mac", 9, bump, dir)),
        ];
        let local = HashMap::from([(
            "jsonl".to_string(),
            vec![worker("jsonl", "ubuntu", 3, bump, dir)],
        )]);
        Pass {
            states,
            computers,
            claude,
            local,
            local_complete: HashSet::from(["jsonl".to_string()]),
        }
    }

    fn scanner_owned() -> HashSet<String> {
        aac_providers::scanner_kinds().map(str::to_owned).collect()
    }

    /// Before: every worker result held, one report call, one trim, one typed contract.
    fn all_at_once(pass: Pass, ingestor: &Ingestor) -> (Vec<u8>, Vec<report::Remember>) {
        let mut remote: Vec<Value> = pass.computers.into_iter().flat_map(|(_, r)| r).collect();
        for (host, worker) in pass.claude {
            remote.push(json!({"tool": "claude", "host": host, "data": worker}));
        }
        let reports = report::build_activity_reports_owned(OwnedReportArgs {
            local: pass.local,
            remote,
            states: &pass.states,
            window_from_ms: NOW - 31 * DAY,
            now: NOW,
            local_complete: &pass.local_complete,
            scanner_owned: &scanner_owned(),
        });
        let (trimmed, remember) = ingestor.trim(&reports);
        let typed = report::contract_reports(&trimmed).expect("contract");
        (serde_json::to_vec(&typed).expect("typed"), remember)
    }

    /// After: each computer's reports as it is read, Claude one computer at a time, the rest.
    fn shaped(pass: Pass, ingestor: &Ingestor) -> (Vec<u8>, Vec<report::Remember>) {
        let owned = scanner_owned();
        let none = HashSet::new();
        let frame = ReportFrame {
            window_from_ms: NOW - 31 * DAY,
            now: NOW,
            local_complete: &none,
            scanner_owned: &owned,
        };
        let mut shaped = Shaped::default();
        for (start, remote) in pass.computers {
            let group = Shaped::computer_sources(&pass.states, start, &owned);
            let built = Shaped::build(&frame, &pass.states, group, HashMap::new(), remote);
            shaped.keep(ingestor, built);
        }
        for (host, worker) in pass.claude {
            let group = shaped.claude_sources(&pass.states, host);
            let remote = vec![json!({"tool": "claude", "host": host, "data": worker})];
            let built = Shaped::build(&frame, &pass.states, group, HashMap::new(), remote);
            shaped.keep(ingestor, built);
        }
        let frame = ReportFrame {
            local_complete: &pass.local_complete,
            ..frame
        };
        let rest = shaped.rest_sources(&pass.states);
        let built = Shaped::build(&frame, &pass.states, rest, pass.local, Vec::new());
        shaped.keep(ingestor, built);
        let (typed, remember) = shaped.finish().expect("contract");
        (serde_json::to_vec(&typed).expect("typed"), remember)
    }

    #[test]
    fn reports_shaped_per_computer_match_one_call_over_the_pass() {
        let dir = tempfile::tempdir().expect("dir");
        let (mut before, mut after) = (Ingestor::new(), Ingestor::new());
        // A first pass (nothing remembered), the same pass again (every day trimmed), and a
        // pass that changed one file's day.
        for bump in [0, 0, 7] {
            let (old, old_remember) = all_at_once(pass(bump, dir.path()), &before);
            let (new, new_remember) = shaped(pass(bump, dir.path()), &after);
            assert!(old.len() > 2, "the pass sends reports");
            assert_eq!(
                String::from_utf8(new).expect("utf-8"),
                String::from_utf8(old).expect("utf-8"),
                "pass with bump {bump}"
            );
            assert_eq!(new_remember.len(), old_remember.len());
            before.accept(old_remember, &HashSet::new());
            after.accept(new_remember, &HashSet::new());
        }
    }

    /// A computer's Claude part: `files` files, a session each; every third file a mirror.
    fn claude_part(
        host: &'static str,
        state_index: usize,
        files: usize,
    ) -> claude_mirrors::Part<'static> {
        let mut rows = Vec::new();
        let mut srows = Vec::new();
        let mut prints = Map::new();
        for file in 0..files {
            let key = format!("claude:/fixture/{host}/{file}.jsonl");
            let at = NOW - (file as i64 % 3) * DAY - HOUR_MS;
            let iso = iso(at);
            rows.push(json!({"k": "claude", "f": key, "m": "claude-opus-4-5",
                "h": format!("{} {}:00", &iso[..10], &iso[11..13]),
                "i": 100 + file as u64, "o": 20, "cr": 500, "cw": 30, "n": 2}));
            srows.push(json!({"k": "claude", "f": key, "s": session_id(file),
                "m": "claude-opus-4-5", "a": at - HOUR_MS, "z": at,
                "i": 100 + file as u64, "o": 20, "cr": 500, "cw": 30, "n": 2}));
            prints.insert(
                key,
                json!({"claudeName": format!("name-{}", file % 5),
                    "claudeMirror": file % 3 == 0}),
            );
        }
        let typed = |rows: &[Value]| -> Vec<aac_store::scan_row::ScanRow> {
            rows.iter()
                .map(|row| serde_json::from_value(row.clone()).expect("scanner row"))
                .collect()
        };
        claude_mirrors::Part {
            host,
            state_index,
            rows: typed(&rows),
            sessions: typed(&srows),
            identities: scan_print::Identities::from_value(&Value::Object(prints)),
        }
    }

    fn claude_states() -> Vec<Value> {
        vec![
            source("claude", "ubuntu", "ok"),
            source("codex", "ubuntu", "ok"),
            source("claude", "mac", "ok"),
            json!({"tool": "claude", "host": "nas1", "state": "unreadable",
                "cause": "agent_missing"}),
            source("claude", "odroid", "ok"),
        ]
    }

    fn claude_parts() -> Vec<claude_mirrors::Part<'static>> {
        vec![
            claude_part("ubuntu", 0, 30),
            claude_part("mac", 2, 12),
            claude_part("odroid", 4, 3),
        ]
    }

    #[test]
    fn claude_parts_set_aside_and_shaped_one_at_a_time_match_one_call() {
        let dir = tempfile::tempdir().expect("dir");
        let pricing = ResolverPricing::new(dir.path());
        let owned = scanner_owned();
        let none = HashSet::new();
        let open = |name: &str| {
            aac_store::ops::open_engine(&dir.path().join(name).to_string_lossy(), None, None)
                .expect("store")
        };
        let (old_store, new_store) = (open("old"), open("new"));
        let (old_ingestor, new_ingestor) = (Ingestor::new(), Ingestor::new());
        for round in 0..2 {
            // Before: owners over the parts, every worker held, one report call.
            let mut states = claude_states();
            let mut parts = claude_parts();
            claude_contributions::prepare_live(&old_store, &mut parts, &mut states, NOW, &pricing)
                .expect("owners");
            let remote: Vec<Value> = parts
                .iter()
                .map(|part| {
                    json!({"tool": "claude", "host": part.host,
                        "data": part.worker(NOW, &pricing)})
                })
                .collect();
            let reports = report::build_activity_reports_owned(OwnedReportArgs {
                local: HashMap::new(),
                remote,
                states: &states,
                window_from_ms: NOW - 31 * DAY,
                now: NOW,
                local_complete: &none,
                scanner_owned: &owned,
            });
            let (trimmed, _) = old_ingestor.trim(&reports);
            let old = serde_json::to_vec(&report::contract_reports(&trimmed).expect("typed"))
                .expect("bytes");
            let old_states = states;
            // After: the parts set aside, read back, shaped one computer at a time.
            let mut states = claude_states();
            let parked: Vec<_> = claude_parts().into_iter().map(|part| part.park()).collect();
            let frame = ReportFrame {
                window_from_ms: NOW - 31 * DAY,
                now: NOW,
                local_complete: &none,
                scanner_owned: &owned,
            };
            let mut shaped = Shaped::default();
            shape_claude(
                &new_store,
                parked,
                &mut states,
                &pricing,
                &frame,
                &mut shaped,
                &new_ingestor,
            );
            let rest = shaped.rest_sources(&states);
            let built = Shaped::build(&frame, &states, rest, HashMap::new(), Vec::new());
            shaped.keep(&new_ingestor, built);
            let (typed, _) = shaped.finish().expect("typed");
            let new = serde_json::to_vec(&typed).expect("bytes");
            assert_eq!(states, old_states, "round {round}: states");
            assert_eq!(
                String::from_utf8(new).expect("utf-8"),
                String::from_utf8(old).expect("utf-8"),
                "round {round}: reports"
            );
            for key in [
                claude_mirrors::LIVE,
                claude_mirrors::HISTORY,
                claude_mirrors::HISTORY_MIRRORS,
            ] {
                assert_eq!(
                    aac_store::analytics_cache::get(&new_store, key).expect("new"),
                    aac_store::analytics_cache::get(&old_store, key).expect("old"),
                    "round {round}: {key}"
                );
            }
        }
    }

    #[test]
    fn a_refused_report_stops_the_whole_pass_ingest() {
        let owned = scanner_owned();
        let none = HashSet::new();
        let frame = ReportFrame {
            window_from_ms: NOW - 31 * DAY,
            now: NOW,
            local_complete: &none,
            scanner_owned: &owned,
        };
        let mut shaped = Shaped::default();
        let ingestor = Ingestor::new();
        let states = vec![source("codex", "ubuntu", "ok")];
        let built = Shaped::build(&frame, &states, vec![0], HashMap::new(), Vec::new());
        shaped.keep(&ingestor, built);
        // A report the typed contract cannot read (its schema field is not text).
        shaped.keep(&ingestor, vec![(1, json!({"schema": 1}))]);
        assert!(shaped.is_built(1));
        assert!(shaped.finish().is_none());
    }
}
