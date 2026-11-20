//! The saved Generic JSONL reader under the native Analytics writer.
//!
//! Settings still offers a mapped local usage-log source (`tool: "jsonl"` with
//! `usageLogFieldMapping` dot paths). The cut's writer reported it as `no_reader`; this module is
//! the bounded reader that replaces `account-activity-collector.ts`'s `jsonl` request: it applies
//! the saved-source checks (an absolute, `..`-free root on the dashboard's Ubuntu host; the
//! mapping keys `config/tables/tools.json` names, with `timestamp` and `model` required), walks the
//! root with the collector's file, depth, entry, line and row bounds, maps each record's tokens
//! through its dot paths, and prices the rows through the same [`Pricing`] seam every other source
//! uses. Malformed lines are skipped, never an error; an unsafe root, a bad mapping or a read that
//! hits a bound is an honest unavailable state, never a measured zero.
//!
//! The writer keeps its side of the contract tiny: [`read`] answers the `jsonl` worker results, the
//! `jsonl` source-state row and whether the read was complete, and the writer puts the first into
//! `local`, the last into `local_complete`. Shared roots are read once: a source that resolves
//! inside another saved source's root, or repeats another generic source's root and mapping, is
//! left to that source (the collector's `claim` rule), and a saved source's root nested inside a
//! generic root is excluded from this walk.
use aac_store::collect::{self, Pricing};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

/// `MAX_FILES`: one source reads at most this many `.jsonl` files.
const MAX_FILES: usize = 20_000;
/// `MAX_DEPTH`: the walk never descends past this depth below the saved root.
const MAX_DEPTH: usize = 64;
/// `MAX_DIRECTORIES`: one source visits at most this many directories.
const MAX_DIRECTORIES: usize = 10_000;
/// `MAX_ENTRIES`: one source looks at at most this many directory entries.
const MAX_ENTRIES: usize = 100_000;
/// `MAX_LINE_BYTES`: a longer line is not a usage record and is left out.
const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
/// `MAX_FILE_ROWS`: one file contributes at most this many mapped rows.
const MAX_FILE_ROWS: usize = 10_000;
/// `MAX_TOTAL_ROWS`: one pass keeps at most this many mapped rows across its sources.
const MAX_TOTAL_ROWS: usize = 100_000;
/// The writer's pass budget: the bytes one pass reads across its sources.
const MAX_PASS_BYTES: u64 = 64 * 1024 * 1024;
/// The longest path a saved source may hold (`isUsageLogSource`).
const MAX_PATH_UNITS: usize = 1024;
/// `usageLogSources` holds at most this many sources (`isDashboardPreferences`).
const MAX_SOURCES: usize = 64;

/// One saved source's field mapping, checked against `usageLogFieldMapping`.
#[derive(Clone, PartialEq, Eq)]
struct Mapping {
    timestamp: String,
    model: String,
    input: Option<String>,
    output: Option<String>,
    cost: Option<String>,
}

impl Mapping {
    /// The mapping the source saved, or `None` for one the writer must refuse.
    fn parse(value: Option<&Value>) -> Option<Self> {
        let map = value?.as_object()?;
        if map.is_empty() || map.len() > aac_providers::USAGE_LOG_FIELD_MAPPING.len() {
            return None;
        }
        for (key, value) in map {
            let known = aac_providers::USAGE_LOG_FIELD_MAPPING
                .iter()
                .any(|row| row.name == key);
            if !known || !value.as_str().is_some_and(field_path) {
                return None;
            }
        }
        let field = |key: &str| map.get(key).and_then(Value::as_str).map(str::to_owned);
        let required = |key: &str| {
            aac_providers::USAGE_LOG_FIELD_MAPPING
                .iter()
                .any(|row| row.name == key && row.required)
        };
        if aac_providers::USAGE_LOG_FIELD_MAPPING
            .iter()
            .filter(|row| row.required)
            .any(|row| map.get(row.name).and_then(Value::as_str).is_none())
        {
            return None;
        }
        Some(Mapping {
            timestamp: field("timestamp")?,
            model: if required("model") {
                field("model")?
            } else {
                field("model").unwrap_or_default()
            },
            input: field("inputTokens"),
            output: field("outputTokens"),
            cost: field("cost"),
        })
    }
}

/// `^[A-Za-z][A-Za-z0-9_]*(?:\.[A-Za-z][A-Za-z0-9_]*)*$` (`FIELD_PATH`).
fn field_path(path: &str) -> bool {
    path.split('.').all(|part| {
        let mut bytes = part.bytes();
        bytes.next().is_some_and(|b| b.is_ascii_alphabetic())
            && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
    })
}

/// `mappedField`: one dot-path step at a time; arrays are never traversed.
fn mapped_field<'a>(record: &'a Map<String, Value>, path: &str) -> Option<&'a Value> {
    let mut parts = path.split('.');
    let mut current = record.get(parts.next()?)?;
    for part in parts {
        current = current.as_object()?.get(part)?;
    }
    Some(current)
}

/// `mappedCount`: a finite, positive number floors to its token count; anything else is 0.
fn mapped_count(value: Option<&Value>) -> u64 {
    value
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n > 0.0)
        .map_or(0, |n| n.floor() as u64)
}

/// The mapped row a line contributes, or `None` when it is not a usable record.
fn mapped_row(line: &[u8], mapping: &Mapping, file: &str, cutoff: i64) -> Option<Value> {
    let record: Value = serde_json::from_slice(line).ok()?;
    let record = record.as_object()?;
    let stamp = mapped_field(record, &mapping.timestamp)?;
    let at = match stamp {
        Value::String(text) if !text.is_empty() => aac_num::ecma::parse_instant(text)?,
        Value::Number(number) => {
            let seconds = number.as_f64().filter(|n| n.is_finite() && *n > 0.0)?;
            let ms = if seconds < 1e11 {
                seconds * 1000.0
            } else {
                seconds
            };
            (ms <= i64::MAX as f64).then_some(ms as i64)?
        }
        _ => return None,
    };
    if at < cutoff {
        return None;
    }
    let model = match mapping.model.is_empty() {
        true => String::new(),
        false => mapped_field(record, &mapping.model)
            .and_then(Value::as_str)
            .unwrap_or("")
            .chars()
            .take(256)
            .collect(),
    };
    let hour = aac_num::ecma::to_iso_string(at.div_euclid(3_600_000) * 3_600_000)?;
    let mut row = Map::new();
    row.insert("k".into(), json!("jsonl"));
    row.insert("h".into(), json!(hour.get(..16).unwrap_or(&hour)));
    row.insert("m".into(), json!(model));
    row.insert(
        "i".into(),
        json!(mapped_count(
            mapping
                .input
                .as_deref()
                .and_then(|path| mapped_field(record, path))
        )),
    );
    row.insert(
        "o".into(),
        json!(mapped_count(
            mapping
                .output
                .as_deref()
                .and_then(|path| mapped_field(record, path))
        )),
    );
    row.insert("cw".into(), json!(0));
    row.insert("cr".into(), json!(0));
    row.insert("n".into(), json!(1));
    row.insert("f".into(), json!(file));
    let cost = mapping
        .cost
        .as_deref()
        .and_then(|path| mapped_field(record, path))
        .and_then(Value::as_f64)
        .filter(|cost| cost.is_finite() && *cost > 0.0);
    if let Some(cost) = cost {
        row.insert("c".into(), json!(cost));
    }
    Some(Value::Object(row))
}

/// The row budget one pass still has.
struct Budget {
    bytes: u64,
    rows: usize,
}

/// The `.jsonl` files under one root, the saved roots to skip inside it, and whether the walk
/// stayed inside every bound. Names are visited in sorted order so a truncated walk is stable.
fn candidate_files(
    root: &Path,
    exclude: &[PathBuf],
    budget: &mut Budget,
) -> (Vec<PathBuf>, Option<&'static str>) {
    let mut files = Vec::new();
    let mut fault = None;
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    let (mut directories, mut entries) = (0usize, 0usize);
    while let Some((directory, depth)) = pending.pop() {
        if directories >= MAX_DIRECTORIES || entries >= MAX_ENTRIES || files.len() >= MAX_FILES {
            fault.get_or_insert("source_limit");
            break;
        }
        directories += 1;
        let Ok(read) = std::fs::read_dir(&directory) else {
            fault.get_or_insert("unreadable_folder");
            continue;
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        for entry in read {
            let Ok(entry) = entry else {
                fault.get_or_insert("unreadable_folder");
                continue;
            };
            entries += 1;
            if entries > MAX_ENTRIES {
                fault.get_or_insert("source_limit");
                break;
            }
            paths.push(entry.path());
        }
        paths.sort();
        for path in paths {
            if files.len() >= MAX_FILES {
                fault.get_or_insert("source_limit");
                break;
            }
            if exclude.iter().any(|skip| path.starts_with(skip)) {
                continue;
            }
            let Ok(kind) = path.symlink_metadata().map(|meta| meta.file_type()) else {
                fault.get_or_insert("unreadable_folder");
                continue;
            };
            // Symlinks are never followed: a saved root cannot be escaped through one.
            if kind.is_symlink() {
                continue;
            }
            if kind.is_dir() {
                if depth >= MAX_DEPTH {
                    fault.get_or_insert("source_limit");
                    continue;
                }
                pending.push((path, depth + 1));
            } else if kind.is_file() && path.to_string_lossy().ends_with(".jsonl") {
                files.push(path);
            }
        }
        // A pass out of bytes or rows stops finding more work, honestly.
        if budget.bytes == 0 || budget.rows == 0 {
            fault.get_or_insert("source_limit");
            break;
        }
    }
    (files, fault)
}

/// Read one file's mapped rows: `None` rows within its line and row bounds.
fn file_rows(
    file: &Path,
    mapping: &Mapping,
    cutoff: i64,
    budget: &mut Budget,
) -> (Vec<Value>, Option<&'static str>) {
    let mut rows: Vec<Value> = Vec::new();
    let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut fault = None;
    use aac_files::read::{Read, ReadPolicy};

    let bytes = match aac_files::read::read(file, &ReadPolicy::capped(budget.bytes)) {
        Read::Bytes(bytes, _) => bytes,
        Read::TooLarge => {
            budget.bytes = 0;
            return (rows, Some("source_limit"));
        }
        _ => return (rows, Some("read_failed")),
    };
    budget.bytes -= bytes.len() as u64;
    let name = file.to_string_lossy();
    for line in bytes.split(|byte| *byte == b'\n') {
        if budget.rows == 0 {
            fault.get_or_insert("source_limit");
            break;
        }
        if line.len() > MAX_LINE_BYTES {
            fault.get_or_insert("read_failed");
            continue;
        }
        let line = line.strip_suffix(&b"\r"[..]).unwrap_or(line);
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let Some(row) = mapped_row(line, mapping, &name, cutoff) else {
            // Anything else on the line (including malformed JSON) is skipped, never an error.
            continue;
        };
        // The collector's `addEntry`: one row per hour and model per file, events counted.
        let key = format!(
            "{}\u{0}{}",
            row["h"].as_str().unwrap_or(""),
            row["m"].as_str().unwrap_or("")
        );
        match index.get(&key).copied() {
            Some(at) => merge_row(&mut rows[at], &row),
            None => {
                if rows.len() >= MAX_FILE_ROWS {
                    fault.get_or_insert("source_limit");
                    break;
                }
                index.insert(key, rows.len());
                rows.push(row);
                budget.rows -= 1;
            }
        }
    }
    (rows, fault)
}

/// Add one line's tokens, logged cost and events to the row already counted for its hour.
fn merge_row(into: &mut Value, line: &Value) {
    let Some(into) = into.as_object_mut() else {
        return;
    };
    for field in ["i", "o", "cw", "cr", "n"] {
        let sum = into.get(field).and_then(Value::as_u64).unwrap_or(0)
            + line[field].as_u64().unwrap_or(0);
        into.insert(field.into(), json!(sum));
    }
    if line["c"].is_number() {
        let sum = into.get("c").and_then(Value::as_f64).unwrap_or(0.0)
            + line["c"].as_f64().unwrap_or(0.0);
        into.insert("c".into(), json!(sum));
    }
}

/// `isUsageLogSource`'s path rule for the dashboard's own Ubuntu host: absolute, bounded, and
/// never an upward escape.
fn safe_root(path: &str) -> Option<&Path> {
    let units = aac_views::spread::utf16_len(path);
    if units == 0 || units > MAX_PATH_UNITS || path.contains('\0') || path.contains("..") {
        return None;
    }
    path.starts_with('/').then(|| Path::new(path))
}

/// A saved non-generic source's resolved root on this host (the collector's `claim` key).
fn saved_root(source: &Value, host: &str) -> Option<(String, PathBuf)> {
    if source["host"].as_str() != Some(host) {
        return None;
    }
    let tool = source["tool"].as_str()?;
    let path = source["path"].as_str()?;
    let root = match tool {
        "codex" => Path::new(path).join("sessions"),
        _ => Path::new(path).to_path_buf(),
    };
    let root = std::fs::canonicalize(root).ok()?;
    Some((tool.to_owned(), root))
}

/// One pass's `jsonl` read: the worker results, the source-state row, and completeness.
#[derive(Default)]
pub(crate) struct Read {
    /// One worker result per source that was read (the writer's `local["jsonl"]`).
    pub(crate) data: Vec<Value>,
    /// The `jsonl` state row for the writer's states, when a source was configured.
    pub(crate) state: Option<Value>,
    /// Whether every configured source was read inside its bounds.
    pub(crate) complete: bool,
}

/// The saved Generic JSONL sources on `host`, read under `cutoff` and priced at `now`.
pub(crate) fn read(
    preferences: Option<&Value>,
    host: &str,
    cutoff: i64,
    now: i64,
    pricing: &dyn Pricing,
) -> Read {
    // Generic JSONL is a local Ubuntu setting (`isUsageLogSource(..., false)`).
    if host != "ubuntu" {
        return Read::default();
    }
    let sources: Vec<&Value> = preferences
        .and_then(|prefs| prefs["usageLogSources"].as_array())
        .into_iter()
        .flatten()
        .filter(|source| {
            source["tool"].as_str() == Some("jsonl") && source["host"].as_str() == Some(host)
        })
        .collect();
    if sources.is_empty() {
        return Read::default();
    }
    let mut read = Read::default();
    let mut complete = true;
    let mut cause: Option<&'static str> = None;
    if sources.len() > MAX_SOURCES {
        read.state = Some(state_row(host, "unreadable", Some("source_limit"), now));
        return read;
    }
    let saved: Vec<(String, PathBuf)> = preferences
        .and_then(|prefs| prefs["usageLogSources"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|source| saved_root(source, host))
        .collect();
    let mut claimed: Vec<(PathBuf, Mapping)> = Vec::new();
    let mut budget = Budget {
        bytes: MAX_PASS_BYTES,
        rows: MAX_TOTAL_ROWS,
    };
    for source in sources {
        let Some(mapping) = Mapping::parse(source.get("fieldMapping")) else {
            complete = false;
            cause.get_or_insert("bad_mapping");
            continue;
        };
        let Some(root) = source["path"].as_str().and_then(safe_root) else {
            complete = false;
            cause.get_or_insert("unsafe_root");
            continue;
        };
        let root = match std::fs::canonicalize(root) {
            Ok(root) if root.is_dir() => root,
            Ok(_) => {
                complete = false;
                cause.get_or_insert("unsafe_root");
                continue;
            }
            Err(_) => {
                complete = false;
                cause.get_or_insert("unreadable_folder");
                continue;
            }
        };
        // Another saved source already reads this root with the same mapping, or reads it as its
        // own tool's logs: this source adds nothing, and no state is published for it.
        if claimed
            .iter()
            .any(|(taken, prior)| *prior == mapping && *taken == root)
        {
            continue;
        }
        if saved
            .iter()
            .any(|(_, other)| other != &root && root.starts_with(other))
        {
            continue;
        }
        claimed.push((root.clone(), mapping.clone()));
        // A saved root inside this one belongs to its own tool: skip it in this walk.
        let exclude: Vec<PathBuf> = saved
            .iter()
            .map(|(_, other)| other.clone())
            .filter(|other| other != &root && other.starts_with(&root))
            .collect();
        let (files, mut fault) = candidate_files(&root, &exclude, &mut budget);
        let mut rows: Vec<Value> = Vec::new();
        for file in files {
            let (mut read_rows, file_fault) = file_rows(&file, &mapping, cutoff, &mut budget);
            rows.append(&mut read_rows);
            if fault.is_none() {
                fault = file_fault;
            }
        }
        if let Some(reason) = fault {
            complete = false;
            cause.get_or_insert(reason);
        }
        read.data.push(collect::worker_from_scan(
            &rows,
            &[],
            "jsonl",
            host,
            now,
            pricing,
        ));
    }
    let has_rows = read.data.iter().any(|worker| {
        worker["cells"]
            .as_array()
            .is_some_and(|cells| !cells.is_empty())
    });
    let state = match (cause, has_rows) {
        (Some(reason), _) => state_row(host, "unreadable", Some(reason), now),
        (None, true) => state_row(host, "ok", None, now),
        (None, false) => state_row(host, "no_usage", None, now),
    };
    read.state = Some(state);
    read.complete = complete;
    read
}

/// The writer's `states` row for the `jsonl` tool on `host`.
fn state_row(host: &str, state: &str, cause: Option<&str>, now: i64) -> Value {
    json!({
        "tool": "jsonl",
        "host": host,
        "state": state,
        "cause": cause,
        "lastScanAt": aac_num::ecma::to_iso_string(now).unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed rate table: the writer's pricing seam, without the registry.
    struct FixturePricing;

    impl Pricing for FixturePricing {
        fn resolve(&self, _model: &str, _provider: Option<&str>) -> collect::PricingResolution {
            collect::PricingResolution {
                rates: collect::Rates {
                    input: 1.0,
                    output: 2.0,
                    cache_creation: 0.0,
                    cache_read: 0.0,
                },
                source: "fixture".into(),
            }
        }
    }

    #[test]
    fn a_file_larger_than_the_remaining_budget_is_a_source_limit() {
        let root = tempfile::tempdir().expect("fixture home");
        let file = root.path().join("rows.jsonl");
        std::fs::write(&file, b"{\"ts\":1,\"model\":\"fixture\"}\n").expect("fixture log");
        let fields = json!({"timestamp": "ts", "model": "model"});
        let mapping = Mapping::parse(Some(&fields)).expect("fixture fields");
        let mut budget = Budget { bytes: 8, rows: 1 };
        let (rows, fault) = file_rows(&file, &mapping, 0, &mut budget);
        assert!(rows.is_empty());
        assert_eq!(fault, Some("source_limit"));
        assert_eq!(budget.bytes, 0);
    }

    #[test]
    fn a_saved_mapped_source_reads_its_rows_into_the_range() {
        let now = 1_795_176_000_000i64; // 2026-11-20T12:00:00Z
        let cutoff = now - 31 * 86_400_000;
        let home = tempfile::tempdir().expect("fixture home");
        let logs = home.path().join("logs");
        std::fs::create_dir_all(&logs).expect("log folder");
        std::fs::write(
            logs.join("rows.jsonl"),
            concat!(
                r#"{"ts":"2026-11-20T10:05:00Z","model":"fixture-model","usage":{"input_tokens":1000,"output_tokens":100}}"#, "\n",
                r#"{"ts":"2026-11-20T10:45:00Z","model":"fixture-model","usage":{"input_tokens":500,"output_tokens":150}}"#, "\n",
            ),
        )
        .expect("fixture rows");
        let preferences = json!({
            "timeZone": "UTC",
            "snapshotCleanup": {"auto": true},
            "usageLogSources": [{
                "id": "mapped",
                "tool": "jsonl",
                "host": "ubuntu",
                "path": logs.to_string_lossy(),
                "fieldMapping": {
                    "timestamp": "ts",
                    "model": "model",
                    "inputTokens": "usage.input_tokens",
                    "outputTokens": "usage.output_tokens",
                },
            }],
        });
        let read = read(Some(&preferences), "ubuntu", cutoff, now, &FixturePricing);
        assert!(read.complete, "the saved source was read whole");
        assert_eq!(read.state.as_ref().expect("state")["state"], "ok");
        let cells = read.data[0]["cells"].as_array().expect("cells");
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0]["inputTokens"], 1500);
        assert_eq!(cells[0]["outputTokens"], 250);
    }

    #[test]
    fn saved_mapped_rows_reach_the_analytics_range() {
        use aac_store::report::{self, ReportArgs};
        use aac_store::{activity, ops};
        use std::collections::{HashMap, HashSet};

        let now = 1_795_176_000_000i64; // 2026-11-20T12:00:00Z
        let cutoff = now - 31 * 86_400_000;
        let ccs = tempfile::tempdir().expect("fixture CCS_HOME");
        let logs = ccs.path().join("logs");
        std::fs::create_dir_all(&logs).expect("log folder");
        std::fs::write(
            logs.join("rows.jsonl"),
            concat!(
                r#"{"ts":"2026-11-20T10:05:00Z","model":"fixture-model","usage":{"input_tokens":1000,"output_tokens":100}}"#, "\n",
                r#"{"ts":"2026-11-20T10:45:00Z","model":"fixture-model","usage":{"input_tokens":500,"output_tokens":150}}"#, "\n",
            ),
        )
        .expect("fixture rows");
        std::fs::write(
            ccs.path().join("dashboard-preferences.json"),
            json!({
                "timeZone": "UTC",
                "snapshotCleanup": {"auto": true},
                "usageLogSources": [{
                    "id": "mapped",
                    "tool": "jsonl",
                    "host": "ubuntu",
                    "path": logs.to_string_lossy(),
                    "fieldMapping": {
                        "timestamp": "ts",
                        "model": "model",
                        "inputTokens": "usage.input_tokens",
                        "outputTokens": "usage.output_tokens",
                    },
                }],
            })
            .to_string(),
        )
        .expect("saved preferences");
        let preferences =
            crate::account_reads::preferences(ccs.path()).expect("readable preferences");
        let read = read(Some(&preferences), "ubuntu", cutoff, now, &FixturePricing);
        assert!(read.complete, "the saved source was read whole");
        let state = read.state.clone().expect("state");
        assert_eq!(state["state"], "ok");
        let mut local: HashMap<String, Vec<Value>> = HashMap::new();
        local.insert("jsonl".into(), read.data);
        let mut local_complete: HashSet<String> = HashSet::new();
        local_complete.insert("jsonl".into());
        let reports = report::build_activity_reports(&ReportArgs {
            local: &local,
            remote: &[],
            states: &[state],
            window_from_ms: cutoff,
            now,
            local_complete: &local_complete,
            scanner_owned: &HashSet::new(),
        });
        let engine = std::sync::Arc::new(
            ops::open_engine(
                ccs.path().join("store").to_str().expect("store path"),
                None,
                None,
            )
            .expect("open store"),
        );
        let typed = report::contract_reports(&reports).expect("typed report");
        activity::ingest(&engine, &typed).expect("ingest");
        let range = ops::call(
            &engine,
            "activityRange",
            &json!({
                "fromMs": now - 7 * 86_400_000,
                "toMs": now,
                "resolution": "hour",
                "sessionSample": 200,
            })
            .to_string(),
        )
        .expect("activity range");
        let tools: Vec<&str> = range["tools"]
            .as_array()
            .expect("tools")
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let (mut input, mut output) = (0u64, 0u64);
        for row in range["rows"].as_array().expect("rows") {
            let cells = row.as_array().expect("row");
            let tool = cells
                .get(2)
                .and_then(Value::as_u64)
                .map(|index| tools[index as usize]);
            if tool == Some("jsonl") {
                input += cells[5].as_u64().unwrap_or(0);
                output += cells[6].as_u64().unwrap_or(0);
            }
        }
        assert_eq!(
            (input, output),
            (1500, 250),
            "the mapped rows reach the range"
        );
        let sources = ops::call(
            &engine,
            "activitySources",
            &json!({ "fromMs": now - 7 * 86_400_000, "toMs": now }).to_string(),
        )
        .expect("activity sources");
        let jsonl = sources["sources"]
            .as_array()
            .expect("sources")
            .iter()
            .find(|source| source["tool"] == "jsonl")
            .expect("the jsonl source");
        assert_eq!(jsonl["state"], "ok");
        // The dashboard's own read (the function /api/accounts/analytics calls) counts them.
        let answer = crate::analytics_route::compose(
            std::sync::Arc::clone(&engine),
            ccs.path(),
            None,
            &crate::analytics_live::Live::default(),
            Some("range=7d&tz=UTC"),
            now,
        )
        .expect("analytics answer");
        let provider = answer["activity"]["providers"]
            .as_array()
            .expect("providers")
            .iter()
            .find(|provider| provider["provider"] == "other")
            .expect("the generic source's provider row");
        assert_eq!(provider["totals"]["inputTokens"], 1500);
        assert_eq!(provider["totals"]["outputTokens"], 250);
    }

    #[test]
    fn unsafe_roots_and_bad_mappings_are_refused() {
        let now = 1_795_176_000_000i64;
        let cutoff = now - 31 * 86_400_000;
        let source = |path: &str, mapping: Value| {
            json!({
                "timeZone": "UTC",
                "snapshotCleanup": {"auto": true},
                "usageLogSources": [{
                    "id": "mapped", "tool": "jsonl", "host": "ubuntu", "path": path,
                    "fieldMapping": mapping,
                }],
            })
        };
        for path in ["relative/logs", "/var/log/../escape", "/var/log\0x"] {
            let read = read(
                Some(&source(path, json!({"timestamp": "ts", "model": "m"}))),
                "ubuntu",
                cutoff,
                now,
                &FixturePricing,
            );
            assert!(!read.complete, "{path}");
            assert!(read.data.is_empty(), "{path}");
            let state = read.state.expect("state");
            assert_eq!(state["state"], "unreadable", "{path}");
            assert_eq!(state["cause"], "unsafe_root", "{path}");
        }
        for mapping in [
            json!({"timestamp": "ts"}),
            json!({"timestamp": "ts", "model": "m", "cost": "9bad"}),
            json!({"timestamp": "ts", "model": "m", "extra": "x"}),
        ] {
            let read = read(
                Some(&source("/var/log/fixture", mapping.clone())),
                "ubuntu",
                cutoff,
                now,
                &FixturePricing,
            );
            assert!(!read.complete, "{mapping}");
            assert!(read.data.is_empty(), "{mapping}");
            let state = read.state.expect("state");
            assert_eq!(state["state"], "unreadable", "{mapping}");
            assert_eq!(state["cause"], "bad_mapping", "{mapping}");
        }
    }
}
