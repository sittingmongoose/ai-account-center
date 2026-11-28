//! G14 activity report builder, ported from `src/store/activity-reports.ts`.
//!
//! One published pass's worker results become the store's usage reports (`build_activity_reports`),
//! and rows read outside a live pass become an import/backfill report (`rows_report`). The shapes
//! here are the TypeScript `UsageReport` JSON exactly, so the parity vectors compare them directly;
//! [`contract_reports`] deserializes them into the typed [`crate::contract::Report`] the store
//! ingests. Ingest decisions, retention, day close and coverage stay in `activity.rs`.
//!
//! Cells and sessions arrive already priced by the collector (the pricing facts the front door
//! supplies); this module never prices, it only shapes, bounds and trims.
use aac_num::ecma::{parse_instant, to_iso_string};
use serde_json::{Map, Value, json};
use sha1::{Digest, Sha1};
use std::collections::{HashMap, HashSet};

const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
/// Records dated more than this after the read are counted, not written (a clock running ahead).
pub const FUTURE_SLACK_MS: i64 = 5 * 60_000;
/// The report schema word the store checks.
pub const USAGE_REPORT_SCHEMA: &str = "aac.usage-report.v1";
/// Tools only the usage scanner reads on the dashboard's computer.
const SCANNER_ONLY_TOOLS: [&str; 2] = ["cursor", "devin"];

fn iso(ms: i64) -> String {
    to_iso_string(ms).expect("report time within the ISO domain")
}

/// `hourOf`: an hourly bucket label (`2026-11-14 21:00`) as epoch milliseconds.
fn hour_of(text: &str) -> Option<i64> {
    parse_instant(&format!("{}:00Z", text.replace(' ', "T")))
}

fn as_f64(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

/// `Math.max(0, Math.floor(v))` as the contract's unsigned token count.
fn count(value: &Value) -> u64 {
    let n = as_f64(value);
    if n.is_finite() && n > 0.0 { n.floor() as u64 } else { 0 }
}

/// A lower-cased, bounded route label (ASCII provider ids).
fn route(value: &Value) -> String {
    value.as_str().unwrap_or("").to_lowercase().chars().take(64).collect()
}

/// A bounded model label, never empty.
fn model_raw(value: &Value) -> String {
    let text = value.as_str().unwrap_or("");
    let text = if text.is_empty() { "unknown" } else { text };
    text.chars().take(200).collect()
}

fn is_hex16(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn is_target(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 32
        && text
            .bytes()
            .all(|b: u8| b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'-')
}

fn is_fixed_cause(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.bytes().all(|b: u8| {
            b.is_ascii_digit() || b.is_ascii_lowercase() || b == b'_' || b == b':' || b == b'.' || b == b'-'
        })
}

/// Cells for a saved result that predates them: one per hourly breakdown, tied to no session.
pub fn cells_from_hourly(hourly: &[Value]) -> Vec<Value> {
    let mut cells = Vec::new();
    for hour in hourly {
        let Some(at) = hour["hour"].as_str().and_then(hour_of) else {
            continue;
        };
        let empty = Vec::new();
        let breakdowns = hour["modelBreakdowns"].as_array().unwrap_or(&empty);
        for b in breakdowns {
            let tokens = as_f64(&b["inputTokens"])
                + as_f64(&b["outputTokens"])
                + as_f64(&b["cacheCreationTokens"])
                + as_f64(&b["cacheReadTokens"]);
            let cost = as_f64(&b["cost"]);
            if ![tokens, cost].into_iter().any(|value| value > 0.0) {
                continue;
            }
            let events = if b["requestCount"].is_number() {
                b["requestCount"].clone()
            } else if breakdowns.len() == 1 && hour["requestCount"].is_number() {
                hour["requestCount"].clone()
            } else {
                Value::Null
            };
            cells.push(json!({
                "hour": iso(at),
                "sessionKey": Value::Null,
                "route": route(&b["provider"]),
                "modelRaw": model_raw(&b["modelName"]),
                "inputTokens": count(&b["inputTokens"]),
                "outputTokens": count(&b["outputTokens"]),
                "cacheCreationTokens": count(&b["cacheCreationTokens"]),
                "cacheReadTokens": count(&b["cacheReadTokens"]),
                "costUsd": cost.max(0.0),
                "fallbackCostUsd": as_f64(&b["fallbackCost"]).max(0.0),
                "events": events,
            }));
        }
    }
    cells
}

/// Cells dated after `limit` are left out and counted: one such cell would move the store's
/// 72-hour settle line past now and stop every live write after it.
pub fn past_cells(cells: Vec<Value>, limit: i64) -> (Vec<Value>, u64) {
    let mut kept = Vec::new();
    let mut future = 0u64;
    for cell in cells {
        let at = cell["hour"].as_str().and_then(parse_instant);
        match at {
            Some(at) if at > limit => {
                let events = as_f64(&cell["events"]).floor().max(0.0) as u64;
                future += events.max(1);
            }
            _ => kept.push(cell),
        }
    }
    (kept, future)
}

fn session_tokens(session: &Value) -> u64 {
    session["parts"]
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .map(|p| {
                    count(&p["inputTokens"])
                        + count(&p["outputTokens"])
                        + count(&p["cacheCreationTokens"])
                        + count(&p["cacheReadTokens"])
                })
                .sum()
        })
        .unwrap_or(0)
}

/// Sessions as the contract's; one that starts after `limit` is left out, a later end is cut to it.
/// Deduped by key, keeping the greater token sum (insertion order of first appearance).
pub fn sessions_of(rows: &[Value], limit: i64) -> Vec<Value> {
    let mut sessions = SessionsOf::new(limit);
    for row in rows {
        sessions.push(row);
    }
    sessions.finish()
}

/// [`sessions_of`] a row at a time: each worker session row is turned into the contract's form
/// as it comes, so the rows need not all be held. Sessions already in the contract's form (each
/// one source's [`sessions_of`]) join the same dedup ([`SessionsOf::push_session`]): a key's
/// session is the first with the greatest token sum either way, in order of first appearance.
pub struct SessionsOf {
    limit: i64,
    order: Vec<String>,
    out: HashMap<String, Value>,
}

impl SessionsOf {
    pub fn new(limit: i64) -> Self {
        SessionsOf {
            limit,
            order: Vec::new(),
            out: HashMap::new(),
        }
    }

    /// One worker session row (`sessionId`, `firstActivity`, `lastActivity`, ...).
    pub fn push(&mut self, row: &Value) {
        if let Some((key, session)) = contract_session(row, self.limit) {
            self.push_session(key, session);
        }
    }

    /// One session in the contract's form.
    pub fn push_session(&mut self, key: String, session: Value) {
        let replace = match self.out.get(&key) {
            Some(prior) => session_tokens(&session) > session_tokens(prior),
            None => {
                self.order.push(key.clone());
                true
            }
        };
        if replace {
            self.out.insert(key, session);
        }
    }

    pub fn finish(mut self) -> Vec<Value> {
        self.order.into_iter().filter_map(|k| self.out.remove(&k)).collect()
    }
}

/// One worker session row in the contract's form, with its key; None for a row the contract
/// leaves out.
fn contract_session(row: &Value, limit: i64) -> Option<(String, Value)> {
    let key = row["sessionId"].as_str()?;
    if !is_hex16(key) {
        return None;
    }
    let last_raw = row["lastActivity"].as_str().and_then(parse_instant)?;
    let last = last_raw.min(limit);
    let first_text = row["firstActivity"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| row["lastActivity"].as_str().unwrap_or(""));
    let first = parse_instant(first_text)?;
    if first > last {
        return None;
    }
    let empty = Vec::new();
    let parts: Vec<Value> = row["modelBreakdowns"]
        .as_array()
        .unwrap_or(&empty)
        .iter()
        .take(32)
        .map(|b| {
            json!({
                "route": route(&b["provider"]),
                "modelRaw": model_raw(&b["modelName"]),
                "inputTokens": count(&b["inputTokens"]),
                "outputTokens": count(&b["outputTokens"]),
                "cacheCreationTokens": count(&b["cacheCreationTokens"]),
                "cacheReadTokens": count(&b["cacheReadTokens"]),
                "costUsd": as_f64(&b["cost"]).max(0.0),
                "fallbackCostUsd": as_f64(&b["fallbackCost"]).max(0.0),
            })
        })
        .collect();
    let target = row["target"]
        .as_str()
        .filter(|t| is_target(t))
        .map(Value::from)
        .unwrap_or(Value::Null);
    let session = json!({
        "sessionKey": key,
        "first": iso(first),
        "last": iso(last),
        "parts": parts,
        "target": target,
    });
    Some((key.to_string(), session))
}

/// The contract state for one source cell of the "Included usage" grid.
pub fn report_state(source: &Value, has_cells: bool) -> (String, Option<String>) {
    let detail = source["detail"].as_str().unwrap_or("");
    let fixed = source["cause"]
        .as_str()
        .filter(|c| is_fixed_cause(c))
        .map(str::to_string);
    let state = source["state"].as_str().unwrap_or("");
    // A state that implies readable usage must carry published cells (r2 High): however the
    // rows were dropped (cache eviction, worker filtering, anything else), zero published
    // cells with unreadable records is a read failure, never "Scanning" with a
    // readable-usage claim. Decided from what is published here.
    let unreadable_records = source["counts"]["unreadableRecords"]
        .as_u64()
        .is_some_and(|count| count > 0);
    if !has_cells && unreadable_records && matches!(state, "scanning" | "cached") {
        return ("unreadable".into(), Some("read_failed".into()));
    }
    let owned = |s: &str| (s.to_string(), fixed.clone());
    match state {
        "scan_failed" | "unreadable" | "installed_no_logs" | "no_reader" => {
            (if has_cells { "partial".into() } else { state.into() }, fixed)
        }
        "ok" | "no_usage" => (if has_cells { "ok".into() } else { "no_usage".into() }, None),
        "cached" => (if has_cells { "partial".into() } else { "cached".into() }, fixed),
        "scanning" => (if has_cells { "partial".into() } else { "scanning".into() }, fixed),
        "not_installed" => ("not_installed".into(), None),
        "unavailable" => {
            if detail.to_lowercase().contains("no local usage log") {
                return ("no_reader".into(), Some("no_local_usage_records".into()));
            }
            if has_cells {
                return ("partial".into(), None);
            }
            let state = if source["host"].as_str() == Some("ubuntu") {
                "scan_failed"
            } else {
                "unreadable"
            };
            let cause = fixed.or_else(|| {
                detail
                    .to_lowercase()
                    .contains("timed out")
                    .then(|| "timeout".to_string())
            });
            (state.into(), cause)
        }
        _ => owned("scan_failed"),
    }
}

/// The inputs of one published pass. `local_complete` names the tools whose local read finished;
/// `scanner_owned` the tools the usage scanner owns on this computer.
pub struct ReportArgs<'a> {
    /// Tool to the local collector's worker results.
    pub local: &'a HashMap<String, Vec<Value>>,
    /// Scanner/remote reads: `{tool, host?, data}` (host falls back to `data.host`).
    pub remote: &'a [Value],
    /// The grid's source states (`AccountAnalyticsSource`).
    pub states: &'a [Value],
    /// The start of the window the readers read (31 days before the pass).
    pub window_from_ms: i64,
    /// The pass clock.
    pub now: i64,
    /// Tools whose local read finished this pass.
    pub local_complete: &'a HashSet<String>,
    /// Tools the usage scanner owns on this computer.
    pub scanner_owned: &'a HashSet<String>,
}

fn host_of(entry: &Value) -> Option<&str> {
    entry["host"].as_str().or_else(|| entry["data"]["host"].as_str())
}

/// One report per computer and tool of a published pass.
pub fn build_activity_reports(args: &ReportArgs<'_>) -> Vec<Value> {
    build_activity_reports_owned(OwnedReportArgs {
        local: args.local.clone(),
        remote: args.remote.to_vec(),
        states: args.states,
        window_from_ms: args.window_from_ms,
        now: args.now,
        local_complete: args.local_complete,
        scanner_owned: args.scanner_owned,
    })
}

/// [`ReportArgs`] with the worker results handed over: the reports take their cells and sessions
/// instead of copying them, and the results are gone once the reports are built.
pub struct OwnedReportArgs<'a> {
    /// Tool to the local collector's worker results.
    pub local: HashMap<String, Vec<Value>>,
    /// Scanner/remote reads: `{tool, host?, data}` (host falls back to `data.host`).
    pub remote: Vec<Value>,
    /// The grid's source states (`AccountAnalyticsSource`).
    pub states: &'a [Value],
    /// The start of the window the readers read (31 days before the pass).
    pub window_from_ms: i64,
    /// The pass clock.
    pub now: i64,
    /// Tools whose local read finished this pass.
    pub local_complete: &'a HashSet<String>,
    /// Tools the usage scanner owns on this computer.
    pub scanner_owned: &'a HashSet<String>,
}

/// Which worker results one source's report reads.
enum ReportData {
    /// The scanner/remote entries at these indexes (each entry's `data`).
    Remote(Vec<usize>),
    /// The local collector's results for the source's tool.
    Local,
}

/// One worker result's cells and sessions into a report's reads. The last report to read a result
/// takes its cells; an earlier one (a source listed twice) copies them, as every read once did.
/// Its session rows are turned into the report's sessions as they are read (a worker built for
/// the report carries them in that form already, `reportSessions`, for the same `limit`).
fn read_worker(
    entry: &mut Value,
    last: bool,
    read: &mut Vec<Value>,
    sessions: &mut SessionsOf,
    derived: &mut bool,
) -> u64 {
    if let Some(cells) = entry.get_mut("cells").and_then(Value::as_array_mut) {
        if last {
            read.append(cells);
        } else {
            read.extend(cells.iter().cloned());
        }
    } else if let Some(hourly) = entry["hourly"].as_array()
        && !hourly.is_empty()
    {
        *derived = true;
        read.extend(cells_from_hourly(hourly));
    }
    if let Some(rows) = entry.get_mut("session").and_then(Value::as_array_mut) {
        if last {
            for row in std::mem::take(rows) {
                sessions.push(&row);
            }
        } else {
            for row in rows.iter() {
                sessions.push(row);
            }
        }
    }
    debug_assert!(
        entry["reportSessions"].is_null() || entry["reportSessionsLimit"].as_i64() == Some(sessions.limit),
        "a worker's report sessions are cut at the report's limit"
    );
    if let Some(ready) = entry.get_mut("reportSessions").and_then(Value::as_array_mut) {
        let ready: Vec<Value> = if last { std::mem::take(ready) } else { ready.clone() };
        for session in ready {
            if let Some(key) = session["sessionKey"].as_str().map(str::to_owned) {
                sessions.push_session(key, session);
            }
        }
    }
    count(&entry["futureDated"])
}

/// [`build_activity_reports`] over handed-over worker results: the same reports, byte for byte.
pub fn build_activity_reports_owned(args: OwnedReportArgs<'_>) -> Vec<Value> {
    let OwnedReportArgs {
        mut local,
        mut remote,
        states,
        window_from_ms,
        now,
        local_complete,
        scanner_owned,
    } = args;
    let mut reports = Vec::new();
    let cover_from = div_ceil(window_from_ms, HOUR) * HOUR;
    let limit = now + FUTURE_SLACK_MS;
    // First which results each source reads, so the last reader of each can take its arrays.
    let mut remote_reads = vec![0usize; remote.len()];
    let mut local_reads: HashMap<&str, usize> = HashMap::new();
    let plans: Vec<(ReportData, bool)> = states
        .iter()
        .map(|source| {
            let tool = source["tool"].as_str().unwrap_or("");
            let host = source["host"].as_str().unwrap_or("");
            let scanner_rows: Vec<usize> = remote
                .iter()
                .enumerate()
                .filter(|(_, entry)| entry["tool"].as_str() == Some(tool) && host_of(entry) == Some(host))
                .map(|(index, _)| index)
                .collect();
            let scanner_here = host == "ubuntu"
                && (SCANNER_ONLY_TOOLS.contains(&tool) || !scanner_rows.is_empty() || scanner_owned.contains(tool));
            if host != "ubuntu" || scanner_here {
                for index in &scanner_rows {
                    remote_reads[*index] += 1;
                }
                (ReportData::Remote(scanner_rows), scanner_here)
            } else {
                *local_reads.entry(tool).or_default() += 1;
                (ReportData::Local, scanner_here)
            }
        })
        .collect();
    for (source, (plan, scanner_here)) in states.iter().zip(plans) {
        let tool = source["tool"].as_str().unwrap_or("");
        let host = source["host"].as_str().unwrap_or("");
        let mut derived = false;
        let mut read: Vec<Value> = Vec::new();
        let mut sessions = SessionsOf::new(limit);
        let mut future_dated = 0u64;
        match plan {
            ReportData::Remote(indexes) => {
                for index in indexes {
                    remote_reads[index] -= 1;
                    let last = remote_reads[index] == 0;
                    let mut missing = Value::Null;
                    let entry = remote[index].get_mut("data").unwrap_or(&mut missing);
                    future_dated += read_worker(entry, last, &mut read, &mut sessions, &mut derived);
                }
            }
            ReportData::Local => {
                let reads = local_reads.get_mut(tool).expect("planned local read");
                *reads -= 1;
                let last = *reads == 0;
                if let Some(entries) = local.get_mut(tool) {
                    for entry in entries.iter_mut() {
                        future_dated += read_worker(entry, last, &mut read, &mut sessions, &mut derived);
                    }
                }
            }
        }
        let (cells, future) = past_cells(read, limit);
        future_dated += future;
        let (mut state, mut cause) = report_state(source, !cells.is_empty());
        let mut counts = source["counts"].as_object().cloned().unwrap_or_default();
        let freshness = &source["counts"]["freshness"];
        if cause.is_none() && matches!(state.as_str(), "ok" | "no_usage") {
            cause = match freshness["state"].as_str() {
                Some("stale") => Some("newer_activity".into()),
                Some("unknown") if source["state"] == "ok" || freshness["activityComplete"] == false => {
                    Some("freshness_unverified".into())
                }
                _ => None,
            };
            if cause.is_some() {
                // Keep measured cells, but an incomplete read cannot replace previous coverage.
                state = "partial".into();
            }
        }
        let newest_cell = cells
            .iter()
            .filter_map(|c| c["hour"].as_str().and_then(parse_instant))
            .fold(0i64, i64::max);
        let scanned = source["lastScanAt"].as_str().and_then(parse_instant);
        let read_at = now.min(scanned.unwrap_or(now).max(newest_cell));
        let complete = (state == "ok" || state == "no_usage")
            && !derived
            && (host != "ubuntu" || scanner_here || local_complete.contains(tool));
        let report_state_word = if complete {
            state.clone()
        } else if state == "ok" {
            "partial".to_string()
        } else {
            state.clone()
        };
        let mut report = Map::new();
        report.insert("schema".into(), json!(USAGE_REPORT_SCHEMA));
        report.insert("computer".into(), json!(host));
        report.insert("tool".into(), json!(tool));
        report.insert("readAt".into(), json!(iso(read_at)));
        report.insert("state".into(), json!(report_state_word));
        report.insert("complete".into(), json!(complete));
        report.insert(
            "coverFrom".into(),
            if complete {
                json!(iso(cover_from.min(read_at.div_euclid(HOUR) * HOUR)))
            } else {
                Value::Null
            },
        );
        report.insert("cause".into(), json!(cause));
        if future_dated > 0 {
            counts.insert("futureDated".into(), json!(future_dated));
        }
        if !counts.is_empty() {
            report.insert("counts".into(), Value::Object(counts));
        }
        report.insert(
            "cells".into(),
            if state == "ok" || state == "partial" {
                Value::Array(cells)
            } else {
                json!([])
            },
        );
        report.insert("sessions".into(), Value::Array(sessions.finish()));
        reports.push(Value::Object(report));
    }
    reports
}

/// The inputs of an import/backfill report.
pub struct RowsReportArgs<'a> {
    /// The computer the rows were read on.
    pub computer: &'a str,
    /// The tool the rows belong to.
    pub tool: &'a str,
    /// The worker results read outside a live pass.
    pub data: &'a [Value],
    /// `import` (an old cache file) or `backfill` (a deep read).
    pub kind: &'a str,
    /// The read time the rows carry.
    pub read_at: i64,
    /// Whether the read finished.
    pub complete: bool,
    /// Records the reader could not read beside readable ones; kept with the report so the
    /// page can name the evidence (r1 backfill).
    pub unreadable_records: Option<u64>,
    /// A fixed cause word, when the read names one.
    pub cause: Option<&'a str>,
    /// The clock; read times never pass it.
    pub now: i64,
}

/// One report of rows read outside a live pass: the store only adds and raises from these.
pub fn rows_report(args: &RowsReportArgs<'_>) -> Value {
    let limit = args.now + FUTURE_SLACK_MS;
    let mut read: Vec<Value> = Vec::new();
    let mut sessions: Vec<Value> = Vec::new();
    let mut future_dated = 0u64;
    for entry in args.data {
        if let Some(cells) = entry["cells"].as_array() {
            read.extend(cells.iter().cloned());
        } else if let Some(hourly) = entry["hourly"].as_array() {
            read.extend(cells_from_hourly(hourly));
        }
        if let Some(session) = entry["session"].as_array() {
            sessions.extend(session.iter().cloned());
        }
        future_dated += count(&entry["futureDated"]);
    }
    let (cells, future) = past_cells(read, limit);
    future_dated += future;
    let newest = cells
        .iter()
        .filter_map(|c| c["hour"].as_str().and_then(parse_instant))
        .fold(0i64, i64::max);
    let read_at = args.now.min(args.read_at.max(newest));
    let oldest = cells
        .iter()
        .filter_map(|c| c["hour"].as_str().and_then(parse_instant))
        .fold(read_at, i64::min);
    let complete = args.complete && args.kind == "backfill";
    let state = if !cells.is_empty() {
        if complete { "ok" } else { "partial" }
    } else if complete {
        "no_usage"
    } else {
        "scanning"
    };
    let mut report = Map::new();
    report.insert("schema".into(), json!(USAGE_REPORT_SCHEMA));
    report.insert("computer".into(), json!(args.computer));
    report.insert("tool".into(), json!(args.tool));
    report.insert("readAt".into(), json!(iso(read_at)));
    report.insert("state".into(), json!(state));
    report.insert("complete".into(), json!(complete));
    report.insert(
        "coverFrom".into(),
        if complete {
            json!(iso(oldest.div_euclid(HOUR) * HOUR))
        } else {
            Value::Null
        },
    );
    report.insert("cause".into(), json!(args.cause));
    if future_dated > 0 || args.unreadable_records.is_some() {
        let mut counts = Map::new();
        if future_dated > 0 {
            counts.insert("futureDated".into(), json!(future_dated));
        }
        if let Some(unreadable_records) = args.unreadable_records {
            counts.insert("unreadableRecords".into(), json!(unreadable_records));
        }
        report.insert("counts".into(), Value::Object(counts));
    }
    report.insert("cells".into(), Value::Array(cells));
    report.insert("sessions".into(), Value::Array(sessions_of(&sessions, limit)));
    report.insert("kind".into(), json!(args.kind));
    Value::Object(report)
}

/// `Math.ceil(a / b)` for positive `b`, without float rounding.
fn div_ceil(a: i64, b: i64) -> i64 {
    a.div_euclid(b) + i64::from(a.rem_euclid(b) != 0)
}

/// A report part's SHA-1, kept as 20 bytes: the remembered digests are compared, never shown, and
/// a hex `String` per day and session was one more heap allocation each pass kept until the next.
type PartDigest = [u8; 20];

fn digest<T: serde::Serialize + ?Sized>(value: &T) -> PartDigest {
    // Streamed into the hasher: the same bytes `serde_json::to_vec` would make, without the copy.
    struct Hash<'a>(&'a mut Sha1);
    impl std::io::Write for Hash<'_> {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0.update(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hasher = Sha1::new();
    serde_json::to_writer(Hash(&mut hasher), value).expect("report JSON");
    hasher.finalize().into()
}

/// Hands reports to the store leaving out each source's days and sessions it already sent
/// unchanged (the store treats those days as untouched). A refused or failed call forgets what was
/// sent, so the next pass sends everything again. Ported from `ActivityIngestor`.
#[derive(Default)]
pub struct Ingestor {
    days: HashMap<String, HashMap<i64, PartDigest>>,
    sessions: HashMap<String, HashMap<String, PartDigest>>,
}

impl Ingestor {
    /// A new trimmer with no memory of earlier passes.
    pub fn new() -> Self {
        Self::default()
    }

    /// Trim a pass's reports against what was last accepted, returning the reports to send (with
    /// `sameDays`) and the digests to remember on acceptance.
    pub fn trim(&self, reports: &[Value]) -> (Vec<Value>, Vec<Remember>) {
        let mut trimmed = Vec::new();
        let mut remember = Vec::new();
        for report in reports {
            let key = format!(
                "{}\u{0}{}\u{0}{}",
                report["computer"].as_str().unwrap_or(""),
                report["tool"].as_str().unwrap_or(""),
                report["kind"].as_str().unwrap_or("live")
            );
            // Borrowed cells: a `Vec<&Value>` serializes to the same bytes as the cloned array.
            let mut by_day: HashMap<i64, Vec<&Value>> = HashMap::new();
            let mut day_order: Vec<i64> = Vec::new();
            if let Some(cells) = report["cells"].as_array() {
                for cell in cells {
                    let day = cell["hour"]
                        .as_str()
                        .and_then(parse_instant)
                        .map(|at| at.div_euclid(DAY))
                        .unwrap_or(0);
                    if !by_day.contains_key(&day) {
                        day_order.push(day);
                    }
                    by_day.entry(day).or_default().push(cell);
                }
            }
            let mut day_digests = HashMap::new();
            for day in &day_order {
                day_digests.insert(*day, digest(&by_day[day]));
            }
            let before = self.days.get(&key);
            let same: Vec<i64> = day_order
                .iter()
                .filter(|day| before.and_then(|b| b.get(day)) == day_digests.get(day))
                .copied()
                .collect();
            let same_set: HashSet<i64> = same.iter().copied().collect();
            let empty = Vec::new();
            let session_digests: HashMap<String, PartDigest> = report["sessions"]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .filter_map(|s| s["sessionKey"].as_str().map(|k| (k.to_string(), digest(s))))
                .collect();
            let before_sessions = self.sessions.get(&key);
            // Every field, with the two rebuilt below held as null in their places (cloning the
            // whole report first copied all of its cells and sessions once more).
            let mut out: Map<String, Value> = report
                .as_object()
                .map(|fields| {
                    fields
                        .iter()
                        .map(|(name, value)| match name.as_str() {
                            "cells" | "sessions" => (name.clone(), Value::Null),
                            _ => (name.clone(), value.clone()),
                        })
                        .collect()
                })
                .unwrap_or_default();
            out.insert(
                "cells".into(),
                Value::Array(
                    report["cells"]
                        .as_array()
                        .unwrap_or(&empty)
                        .iter()
                        .filter(|cell| {
                            let day = cell["hour"]
                                .as_str()
                                .and_then(parse_instant)
                                .map(|at| at.div_euclid(DAY))
                                .unwrap_or(0);
                            !same_set.contains(&day)
                        })
                        .cloned()
                        .collect(),
                ),
            );
            out.insert(
                "sessions".into(),
                Value::Array(
                    report["sessions"]
                        .as_array()
                        .unwrap_or(&empty)
                        .iter()
                        .filter(|s| {
                            let k = s["sessionKey"].as_str().unwrap_or("");
                            before_sessions.and_then(|b| b.get(k)) != session_digests.get(k)
                        })
                        .cloned()
                        .collect(),
                ),
            );
            if !same.is_empty() {
                out.insert("sameDays".into(), Value::Array(same.iter().map(|d| json!(d)).collect()));
            }
            trimmed.push(Value::Object(out));
            remember.push(Remember {
                key,
                days: day_digests,
                sessions: session_digests,
            });
        }
        (trimmed, remember)
    }

    /// Remember a pass's digests as accepted (a refused report's key is dropped instead).
    pub fn accept(&mut self, remember: Vec<Remember>, refused: &HashSet<usize>) {
        for (index, item) in remember.into_iter().enumerate() {
            if refused.contains(&index) {
                self.days.remove(&item.key);
                self.sessions.remove(&item.key);
            } else {
                self.days.insert(item.key.clone(), item.days);
                self.sessions.insert(item.key, item.sessions);
            }
        }
    }

    /// Forget every remembered digest (a failed call sends everything again next pass).
    pub fn forget(&mut self, remember: &[Remember]) {
        for item in remember {
            self.days.remove(&item.key);
            self.sessions.remove(&item.key);
        }
    }
}

/// One report's digests to remember once the store accepts it.
pub struct Remember {
    key: String,
    days: HashMap<i64, PartDigest>,
    sessions: HashMap<String, PartDigest>,
}

/// Deserialize shaped reports into the typed contract reports the store ingests.
pub fn contract_reports(reports: &[Value]) -> Result<Vec<crate::contract::Report>, serde_json::Error> {
    use serde::Deserialize;
    reports.iter().map(crate::contract::Report::deserialize).collect()
}
