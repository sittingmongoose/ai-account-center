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
            .all(|b| b.is_ascii_digit() || (b'a'..=b'z').contains(&b) || b == b'-')
}

fn is_fixed_cause(text: &str) -> bool {
    !text.is_empty()
        && text.len() <= 64
        && text.bytes().all(|b| {
            b.is_ascii_digit() || (b'a'..=b'z').contains(&b) || b == b'_' || b == b':' || b == b'.' || b == b'-'
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
            if !(tokens > 0.0) && !(cost > 0.0) {
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
    let mut order: Vec<String> = Vec::new();
    let mut out: HashMap<String, Value> = HashMap::new();
    for row in rows {
        let Some(key) = row["sessionId"].as_str() else {
            continue;
        };
        if !is_hex16(key) {
            continue;
        }
        let last_raw = row["lastActivity"].as_str().and_then(parse_instant);
        let Some(last_raw) = last_raw else {
            continue;
        };
        let last = last_raw.min(limit);
        let first_text = row["firstActivity"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| row["lastActivity"].as_str().unwrap_or(""));
        let Some(first) = parse_instant(first_text) else {
            continue;
        };
        if first > last {
            continue;
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
        let replace = match out.get(key) {
            Some(prior) => session_tokens(&session) > session_tokens(prior),
            None => {
                order.push(key.to_string());
                true
            }
        };
        if replace {
            out.insert(key.to_string(), session);
        }
    }
    order.into_iter().filter_map(|k| out.remove(&k)).collect()
}

/// The contract state for one source cell of the "Included usage" grid.
pub fn report_state(source: &Value, has_cells: bool) -> (String, Option<String>) {
    let detail = source["detail"].as_str().unwrap_or("");
    let fixed = source["cause"]
        .as_str()
        .filter(|c| is_fixed_cause(c))
        .map(str::to_string);
    let state = source["state"].as_str().unwrap_or("");
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
    let mut reports = Vec::new();
    let cover_from = div_ceil(args.window_from_ms, HOUR) * HOUR;
    let limit = args.now + FUTURE_SLACK_MS;
    for source in args.states {
        let tool = source["tool"].as_str().unwrap_or("");
        let host = source["host"].as_str().unwrap_or("");
        let scanner_rows: Vec<Value> = args
            .remote
            .iter()
            .filter(|entry| entry["tool"].as_str() == Some(tool) && host_of(entry) == Some(host))
            .map(|entry| entry["data"].clone())
            .collect();
        let scanner_here = host == "ubuntu"
            && (SCANNER_ONLY_TOOLS.contains(&tool) || !scanner_rows.is_empty() || args.scanner_owned.contains(tool));
        let empty = Vec::new();
        let data: Vec<Value> = if host != "ubuntu" || scanner_here {
            scanner_rows
        } else {
            args.local.get(tool).unwrap_or(&empty).clone()
        };
        let mut derived = false;
        let mut read: Vec<Value> = Vec::new();
        let mut session_rows: Vec<Value> = Vec::new();
        let mut future_dated = 0u64;
        for entry in &data {
            if let Some(cells) = entry["cells"].as_array() {
                read.extend(cells.iter().cloned());
            } else if let Some(hourly) = entry["hourly"].as_array() {
                if !hourly.is_empty() {
                    derived = true;
                    read.extend(cells_from_hourly(hourly));
                }
            }
            if let Some(session) = entry["session"].as_array() {
                session_rows.extend(session.iter().cloned());
            }
            future_dated += count(&entry["futureDated"]);
        }
        let (cells, future) = past_cells(read, limit);
        future_dated += future;
        let (state, cause) = report_state(source, !cells.is_empty());
        let newest_cell = cells
            .iter()
            .filter_map(|c| c["hour"].as_str().and_then(parse_instant))
            .fold(0i64, i64::max);
        let scanned = source["lastScanAt"].as_str().and_then(parse_instant);
        let read_at = args.now.min(scanned.unwrap_or(args.now).max(newest_cell));
        let complete = (state == "ok" || state == "no_usage")
            && !derived
            && (host != "ubuntu" || scanner_here || args.local_complete.contains(tool));
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
            report.insert("counts".into(), json!({ "futureDated": future_dated }));
        }
        report.insert(
            "cells".into(),
            if state == "ok" || state == "partial" {
                Value::Array(cells)
            } else {
                json!([])
            },
        );
        report.insert("sessions".into(), Value::Array(sessions_of(&session_rows, limit)));
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
    if future_dated > 0 {
        report.insert("counts".into(), json!({ "futureDated": future_dated }));
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

fn digest(value: &Value) -> String {
    let mut hasher = Sha1::new();
    hasher.update(serde_json::to_vec(value).expect("report JSON"));
    let out = hasher.finalize();
    out.iter().map(|b| format!("{b:02x}")).collect()
}

/// Hands reports to the store leaving out each source's days and sessions it already sent
/// unchanged (the store treats those days as untouched). A refused or failed call forgets what was
/// sent, so the next pass sends everything again. Ported from `ActivityIngestor`.
#[derive(Default)]
pub struct Ingestor {
    days: HashMap<String, HashMap<i64, String>>,
    sessions: HashMap<String, HashMap<String, String>>,
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
            let mut by_day: HashMap<i64, Vec<Value>> = HashMap::new();
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
                    by_day.entry(day).or_default().push(cell.clone());
                }
            }
            let mut day_digests = HashMap::new();
            for day in &day_order {
                day_digests.insert(*day, digest(&Value::Array(by_day[day].clone())));
            }
            let before = self.days.get(&key);
            let same: Vec<i64> = day_order
                .iter()
                .filter(|day| before.and_then(|b| b.get(day)) == day_digests.get(day))
                .copied()
                .collect();
            let same_set: HashSet<i64> = same.iter().copied().collect();
            let empty = Vec::new();
            let session_digests: HashMap<String, String> = report["sessions"]
                .as_array()
                .unwrap_or(&empty)
                .iter()
                .filter_map(|s| s["sessionKey"].as_str().map(|k| (k.to_string(), digest(s))))
                .collect();
            let before_sessions = self.sessions.get(&key);
            let mut out = report.as_object().cloned().unwrap_or_default();
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
    days: HashMap<i64, String>,
    sessions: HashMap<String, String>,
}

/// Deserialize shaped reports into the typed contract reports the store ingests.
pub fn contract_reports(reports: &[Value]) -> Result<Vec<crate::contract::Report>, serde_json::Error> {
    reports.iter().map(|r| serde_json::from_value(r.clone())).collect()
}
