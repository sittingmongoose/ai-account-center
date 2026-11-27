//! Compact readers-check writer (`agent-reports/reader@1`), replacing retained
//! `IndexMap<String, IndexMap<String, CheckRow>>` JSON trees with sorted per-writer
//! compact row vectors, inline `[u8; 32]` SHA-256 digests, numeric timestamps, interned
//! pins, and dirty-only flushing (zero allocations on quiet flushes).
//!
//! Byte-identical output to `aac_status::share::check::ReadersCheckWriter`.

use super::primitives::{CompactProblem, CompactSha, write_json_str};
use super::reports::CompactAgentReportsState;
use super::status_docs::CompactStatusDocsState;
use aac_files::registry::{self, Owners, Role};
use aac_status::share::check::{READERS_CHECK_SCHEMA, RunRow, normalise_thread_id};
use aac_status::share::{Problem, compare};
use aac_status::validate::is_writer_name;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::PathBuf;

/// Compact representation of a writer-relative report filename.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CheckFile {
    Thread,
    Tasks,
    Status,
    Card(Box<str>),
    Run(Box<str>),
    Other(Box<str>),
}

impl CheckFile {
    fn from_rest(rest: &str) -> Self {
        match rest {
            "thread.json" => Self::Thread,
            "tasks.json" => Self::Tasks,
            "status.json" => Self::Status,
            other => {
                if let Some(stem) = other
                    .strip_prefix("cards/")
                    .and_then(|s| s.strip_suffix(".json"))
                {
                    Self::Card(stem.into())
                } else if let Some(tail) = other.strip_prefix("runs/") {
                    Self::Run(tail.into())
                } else {
                    Self::Other(other.into())
                }
            }
        }
    }

    fn write_name(&self, out: &mut String) {
        match self {
            Self::Thread => out.push_str("thread.json"),
            Self::Tasks => out.push_str("tasks.json"),
            Self::Status => out.push_str("status.json"),
            Self::Card(id) => {
                out.push_str("cards/");
                out.push_str(id);
                out.push_str(".json");
            }
            Self::Run(tail) => {
                out.push_str("runs/");
                out.push_str(tail);
            }
            Self::Other(other) => out.push_str(other),
        }
    }

    fn compare_name(&self, other: &Self) -> std::cmp::Ordering {
        // Fast path avoiding allocation when comparing fixed variants.
        match (self.as_fixed_str(), other.as_fixed_str()) {
            (Some(a), Some(b)) => compare(a, b),
            (Some(a), None) => {
                let mut b = String::new();
                other.write_name(&mut b);
                compare(a, &b)
            }
            (None, Some(b)) => {
                let mut a = String::new();
                self.write_name(&mut a);
                compare(&a, b)
            }
            (None, None) => {
                let mut a = String::new();
                let mut b = String::new();
                self.write_name(&mut a);
                other.write_name(&mut b);
                compare(&a, &b)
            }
        }
    }

    #[inline]
    fn as_fixed_str(&self) -> Option<&str> {
        match self {
            Self::Thread => Some("thread.json"),
            Self::Tasks => Some("tasks.json"),
            Self::Status => Some("status.json"),
            Self::Other(s) => Some(s),
            Self::Card(_) | Self::Run(_) => None,
        }
    }
}

/// Compact representation of the optional `pin` member on a check row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckPin {
    /// No `pin` property written (tasks, cards, runs).
    Absent,
    /// `"pin": null` written (thread.json / status.json with no matched pin).
    Null,
    /// `"pin": {"t3Thread": ..., "title": ...}` indexed into `pin_pool`.
    Matched(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct CompactCheckRow {
    file: CheckFile,
    seq: Option<u64>,
    sha256: Option<CompactSha>,
    read_at_ms: u64,
    accepted: bool,
    reason: Option<Box<str>>,
    pin: CheckPin,
}

struct WriterState {
    /// Kept sorted by `CheckFile::compare_name` so lookup is O(log n) and flush needs no sort.
    rows: Vec<CompactCheckRow>,
    written: Option<[u8; 32]>,
    dirty: bool,
}

impl WriterState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            written: None,
            dirty: true,
        }
    }

    fn upsert(&mut self, row: CompactCheckRow) {
        match self
            .rows
            .binary_search_by(|existing| existing.file.compare_name(&row.file))
        {
            Ok(idx) => {
                if self.rows[idx] != row {
                    self.rows[idx] = row;
                    self.dirty = true;
                }
            }
            Err(idx) => {
                self.rows.insert(idx, row);
                self.dirty = true;
            }
        }
    }
}

/// Compact run receipt handed from Scoreboard without forcing String allocations.
pub(crate) struct CompactRunReceipt<'a> {
    pub(crate) file: &'a str,
    pub(crate) seq: u64,
    pub(crate) sha256: CompactSha,
    pub(crate) first_seen_at: u64,
}

fn iso(at: u64) -> String {
    aac_num::ecma::to_iso_string(at as i64).unwrap_or_default()
}

/// `writers/<writer>/<rest>` -> `(&writer, &rest)`. Zero heap allocations.
fn writer_and_file(rel: &str) -> Option<(&str, &str)> {
    let rest = rel.strip_prefix("writers/")?;
    let slash = rest.find('/')?;
    let writer = &rest[..slash];
    let file = &rest[slash + 1..];
    // Original split('/') required parts.len() >= 3, which means at least two slashes.
    Some((writer, file))
}

/// Interned pin table so matching rows store a 4-byte index instead of cloning JSON maps.
#[derive(Default)]
struct PinPool {
    entries: Vec<(Box<str>, Box<str>)>,
    index: HashMap<(Box<str>, Box<str>), u32>,
}

impl PinPool {
    fn intern(&mut self, t3_thread: &str, title: &str) -> u32 {
        // Linear lookup on tiny table (typically < 30 pins) avoids allocating a tuple on hit.
        if let Some((idx, _)) = self
            .entries
            .iter()
            .enumerate()
            .find(|(_, (id, t))| id.as_ref() == t3_thread && t.as_ref() == title)
        {
            return idx as u32;
        }
        let id = self.entries.len() as u32;
        let pair: (Box<str>, Box<str>) = (t3_thread.into(), title.into());
        self.entries.push(pair.clone());
        self.index.insert(pair, id);
        id
    }

    fn get(&self, id: u32) -> Option<(&str, &str)> {
        self.entries
            .get(id as usize)
            .map(|(a, b)| (a.as_ref(), b.as_ref()))
    }
}

/// Compact replacement for `aac_status::share::check::ReadersCheckWriter`.
pub(crate) struct CompactReadersCheckWriter {
    root: PathBuf,
    writers: HashMap<Box<str>, WriterState>,
    pin_pool: PinPool,
}

impl CompactReadersCheckWriter {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            writers: HashMap::new(),
            pin_pool: PinPool::default(),
        }
    }

    fn put_with_key(
        &mut self,
        writer: &str,
        row: CompactCheckRow,
        current: &mut HashSet<String>,
        file_name: &str,
    ) {
        if !is_writer_name(writer) {
            return;
        }
        let mut key = String::with_capacity(writer.len() + 1 + file_name.len());
        key.push_str(writer);
        key.push('\n');
        key.push_str(file_name);
        current.insert(key);
        if let Some(state) = self.writers.get_mut(writer) {
            state.upsert(row);
        } else {
            let mut state = WriterState::new();
            state.upsert(row);
            self.writers.insert(writer.into(), state);
        }
    }

    fn match_pin_compact(
        &mut self,
        id: Option<&str>,
        norm_pins: &[(String, &str, &str)],
    ) -> CheckPin {
        let Some(id) = id.filter(|id| !id.is_empty()) else {
            return CheckPin::Null;
        };
        let want = normalise_thread_id(id);
        if want.is_empty() {
            return CheckPin::Null;
        }
        for (norm_pin, pin_id, title) in norm_pins {
            if norm_pin == &want {
                return CheckPin::Matched(self.pin_pool.intern(pin_id, title));
            }
        }
        CheckPin::Null
    }

    /// Replaces the status-side rows (thread.json, tasks.json, status.json, cards) directly
    /// from the compact snapshots without materializing JSON trees.
    pub(crate) fn record_status_scan(
        &mut self,
        reports: &CompactAgentReportsState,
        statuses: &CompactStatusDocsState,
        pins: &[(String, String)],
    ) {
        let norm_pins: Vec<(String, &str, &str)> = pins
            .iter()
            .map(|(pin_id, title)| (normalise_thread_id(pin_id), pin_id.as_str(), title.as_str()))
            .collect();
        let mut current: HashSet<String> = HashSet::new();

        for row in &reports.threads {
            let pin = self.match_pin_compact(row.thread_t3_thread(), &norm_pins);
            let check_row = CompactCheckRow {
                file: CheckFile::Thread,
                seq: Some(row.seq()),
                sha256: Some(row.sha256.clone()),
                read_at_ms: row.first_seen_at,
                accepted: row.problem.is_none(),
                reason: row.problem.as_ref().map(|p| p.reason.clone()),
                pin,
            };
            self.put_with_key(&row.writer, check_row, &mut current, "thread.json");
        }

        for row in &reports.tasks {
            let check_row = CompactCheckRow {
                file: CheckFile::Tasks,
                seq: Some(row.seq()),
                sha256: Some(row.sha256.clone()),
                read_at_ms: row.first_seen_at,
                accepted: row.problem.is_none(),
                reason: row.problem.as_ref().map(|p| p.reason.clone()),
                pin: CheckPin::Absent,
            };
            self.put_with_key(&row.writer, check_row, &mut current, "tasks.json");
        }

        for row in &reports.cards {
            let id = row.card_id().unwrap_or_default();
            let file_name = format!("cards/{id}.json");
            let check_row = CompactCheckRow {
                file: CheckFile::Card(id.into()),
                seq: Some(row.seq()),
                sha256: Some(row.sha256.clone()),
                read_at_ms: row.first_seen_at,
                accepted: row.problem.is_none(),
                reason: row.problem.as_ref().map(|p| p.reason.clone()),
                pin: CheckPin::Absent,
            };
            self.put_with_key(&row.writer, check_row, &mut current, &file_name);
        }

        for entry in &statuses.docs {
            let t3_thread =
                (!entry.doc.t3_thread.is_empty()).then_some(entry.doc.t3_thread.as_ref());
            let pin = self.match_pin_compact(t3_thread, &norm_pins);
            let check_row = CompactCheckRow {
                file: CheckFile::Status,
                seq: Some(entry.doc.seq),
                sha256: Some(entry.sha256.clone()),
                read_at_ms: entry.first_seen_at,
                accepted: entry.problem.is_none(),
                reason: entry.problem.as_ref().map(|p| p.reason.clone()),
                pin,
            };
            self.put_with_key(&entry.writer, check_row, &mut current, "status.json");
        }

        let problems = reports.problems.iter().chain(statuses.problems.iter());
        for problem in problems {
            self.record_status_problem(problem, &mut current);
        }
    }

    fn record_status_problem(&mut self, problem: &CompactProblem, current: &mut HashSet<String>) {
        let Some((writer, rest)) = writer_and_file(&problem.file) else {
            return;
        };
        let mut key = String::with_capacity(writer.len() + 1 + rest.len());
        key.push_str(writer);
        key.push('\n');
        key.push_str(rest);
        if current.contains(&key) {
            return;
        }
        let pin = if rest == "thread.json" || rest == "status.json" {
            CheckPin::Null
        } else {
            CheckPin::Absent
        };
        let row = CompactCheckRow {
            file: CheckFile::from_rest(rest),
            seq: None,
            sha256: None,
            read_at_ms: problem.since,
            accepted: false,
            reason: Some(problem.reason.clone()),
            pin,
        };
        self.put_with_key(writer, row, current, rest);
    }

    /// Replaces the runs rows from one Scoreboard runs scan (`RunRow` slice compatibility).
    pub(crate) fn record_runs(&mut self, runs: &[RunRow], problems: &[Problem]) {
        let iter = runs.iter().map(|run| CompactRunReceipt {
            file: &run.file,
            seq: run.seq,
            sha256: CompactSha::from_hex_or_str(&run.sha256),
            first_seen_at: run.first_seen_at,
        });
        self.record_run_receipts(iter, problems);
    }

    /// Zero-intermediate-Vec runs recording for store-backed Scoreboard receipts.
    pub(crate) fn record_run_receipts<'a>(
        &mut self,
        runs: impl IntoIterator<Item = CompactRunReceipt<'a>>,
        problems: &[Problem],
    ) {
        let mut current: HashSet<String> = HashSet::new();
        for run in runs {
            let Some((writer, rest)) = writer_and_file(run.file) else {
                continue;
            };
            let row = CompactCheckRow {
                file: CheckFile::from_rest(rest),
                seq: Some(run.seq),
                sha256: Some(run.sha256),
                read_at_ms: run.first_seen_at,
                accepted: true,
                reason: None,
                pin: CheckPin::Absent,
            };
            self.put_with_key(writer, row, &mut current, rest);
        }
        for problem in problems {
            let Some((writer, rest)) = writer_and_file(&problem.file) else {
                continue;
            };
            if !rest.starts_with("runs/") {
                continue;
            }
            let mut key = String::with_capacity(writer.len() + 1 + rest.len());
            key.push_str(writer);
            key.push('\n');
            key.push_str(rest);
            if current.contains(&key) {
                continue;
            }
            let row = CompactCheckRow {
                file: CheckFile::from_rest(rest),
                seq: None,
                sha256: None,
                read_at_ms: problem.since,
                accepted: false,
                reason: Some(problem.reason.as_str().into()),
                pin: CheckPin::Absent,
            };
            self.put_with_key(writer, row, &mut current, rest);
        }
    }

    /// Writes each changed writer's check file atomically through the registry's leased
    /// writer. Unchanged writers (`!dirty && written.is_some()`) perform zero allocations
    /// and zero serialization; dirty writers stream compact JSON into one reused scratch
    /// buffer to check the exact SHA-256 digest before formatting pretty JSON.
    pub(crate) fn flush(&mut self, owners: &Owners, now: u64) {
        if owners.owner("readers-check") != Some(Role::RustService) {
            return;
        }
        if !aac_files::read::stat(&self.root).is_ok_and(|stat| stat.dir) {
            return;
        }
        let rows = match registry::rows() {
            Ok(rows) => rows,
            Err(_) => return,
        };
        let written_at = iso(now);
        let mut failed = false;
        let mut compact_buf = String::new();
        let mut pretty_buf = String::new();
        let mut name_buf = String::new();
        let pin_pool = &self.pin_pool;
        for (writer, state) in &mut self.writers {
            if !state.dirty && state.written.is_some() {
                continue;
            }
            compact_buf.clear();
            write_files_compact(&state.rows, pin_pool, &mut compact_buf);
            let print: [u8; 32] = Sha256::digest(compact_buf.as_bytes()).into();
            if state.written == Some(print) {
                state.dirty = false;
                continue;
            }
            pretty_buf.clear();
            write_check_doc_pretty(writer, &written_at, &state.rows, pin_pool, &mut pretty_buf);
            name_buf.clear();
            name_buf.push_str(writer);
            name_buf.push_str(".json");
            match owners.claim(rows, "readers-check", &self.root, Some(&name_buf)) {
                Ok(store) => {
                    if store.write(pretty_buf.as_bytes()).is_err() {
                        failed = true;
                    } else {
                        state.written = Some(print);
                        state.dirty = false;
                    }
                }
                Err(_) => failed = true,
            }
        }
        if failed {
            tracing_check_failure();
        }
    }
}

/// Write `files` in exact `serde_json::to_string` compact format so the SHA-256 matches
/// `aac_status::share::check::ReadersCheckWriter` byte for byte.
fn write_files_compact(rows: &[CompactCheckRow], pins: &PinPool, out: &mut String) {
    out.push('[');
    let mut name_scratch = String::new();
    let mut sha_scratch = String::new();
    for (idx, row) in rows.iter().enumerate() {
        if idx > 0 {
            out.push(',');
        }
        out.push_str("{\"file\":");
        name_scratch.clear();
        row.file.write_name(&mut name_scratch);
        write_json_str(out, &name_scratch);
        out.push_str(",\"seq\":");
        match row.seq {
            Some(seq) => {
                let _ = write!(out, "{seq}");
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"sha256\":");
        match &row.sha256 {
            Some(sha) => {
                sha_scratch.clear();
                sha.write_hex(&mut sha_scratch);
                write_json_str(out, &sha_scratch);
            }
            None => out.push_str("null"),
        }
        out.push_str(",\"readAt\":");
        write_json_str(out, &iso(row.read_at_ms));
        out.push_str(",\"result\":");
        out.push_str(if row.accepted {
            "\"accepted\""
        } else {
            "\"refused\""
        });
        out.push_str(",\"reason\":");
        match &row.reason {
            Some(reason) => write_json_str(out, reason),
            None => out.push_str("null"),
        }
        match row.pin {
            CheckPin::Absent => {}
            CheckPin::Null => out.push_str(",\"pin\":null"),
            CheckPin::Matched(id) => match pins.get(id) {
                Some((t3_thread, title)) => {
                    out.push_str(",\"pin\":{\"t3Thread\":");
                    write_json_str(out, t3_thread);
                    out.push_str(",\"title\":");
                    write_json_str(out, title);
                    out.push('}');
                }
                None => out.push_str(",\"pin\":null"),
            },
        }
        out.push('}');
    }
    out.push(']');
}

/// Write the check file in exact `serde_json::to_string_pretty` + `\n` format without
/// allocating `serde_json::Value` trees.
fn write_check_doc_pretty(
    writer: &str,
    written_at: &str,
    rows: &[CompactCheckRow],
    pins: &PinPool,
    out: &mut String,
) {
    out.push_str("{\n  \"schema\": ");
    write_json_str(out, READERS_CHECK_SCHEMA);
    out.push_str(",\n  \"reader\": \"aac\",\n  \"writer\": ");
    write_json_str(out, writer);
    out.push_str(",\n  \"writtenAt\": ");
    write_json_str(out, written_at);
    if rows.is_empty() {
        out.push_str(",\n  \"files\": []\n}\n");
        return;
    }
    out.push_str(",\n  \"files\": [\n");
    let mut name_scratch = String::new();
    let mut sha_scratch = String::new();
    for (idx, row) in rows.iter().enumerate() {
        if idx > 0 {
            out.push_str(",\n");
        }
        out.push_str("    {\n      \"file\": ");
        name_scratch.clear();
        row.file.write_name(&mut name_scratch);
        write_json_str(out, &name_scratch);
        out.push_str(",\n      \"seq\": ");
        match row.seq {
            Some(seq) => {
                let _ = write!(out, "{seq}");
            }
            None => out.push_str("null"),
        }
        out.push_str(",\n      \"sha256\": ");
        match &row.sha256 {
            Some(sha) => {
                sha_scratch.clear();
                sha.write_hex(&mut sha_scratch);
                write_json_str(out, &sha_scratch);
            }
            None => out.push_str("null"),
        }
        out.push_str(",\n      \"readAt\": ");
        write_json_str(out, &iso(row.read_at_ms));
        out.push_str(",\n      \"result\": ");
        out.push_str(if row.accepted {
            "\"accepted\""
        } else {
            "\"refused\""
        });
        out.push_str(",\n      \"reason\": ");
        match &row.reason {
            Some(reason) => write_json_str(out, reason),
            None => out.push_str("null"),
        }
        match row.pin {
            CheckPin::Absent => {}
            CheckPin::Null => out.push_str(",\n      \"pin\": null"),
            CheckPin::Matched(id) => match pins.get(id) {
                Some((t3_thread, title)) => {
                    out.push_str(",\n      \"pin\": {\n        \"t3Thread\": ");
                    write_json_str(out, t3_thread);
                    out.push_str(",\n        \"title\": ");
                    write_json_str(out, title);
                    out.push_str("\n      }");
                }
                None => out.push_str(",\n      \"pin\": null"),
            },
        }
        out.push_str("\n    }");
    }
    out.push_str("\n  ]\n}\n");
}

fn tracing_check_failure() {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("readers check flush failed");
    }
}
