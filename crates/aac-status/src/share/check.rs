//! The readers check file (`readers-check.ts`, `agent-reports/reader@1`): what AAC did with
//! each writer's reports, written after every scan to `<reports>/v1/readers/aac/<writer>.json`
//! so `agent-report check` can tell a writer "accepted, seq 12" or "refused: <the fixed
//! reason>". The Status scan's rows and the Scoreboard's runs rows merge into one file per
//! writer. The write goes through the registry's leased writer (L2): the row moves to the
//! service with the R4 groups, and TS's writer refuses it once it has.

use super::{Problem, compare};
use crate::validate::is_writer_name;
use aac_files::registry::{self, Owners, Role};
use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};
use sha2::Digest;
use std::path::{Path, PathBuf};

pub const READERS_CHECK_SCHEMA: &str = "agent-reports/reader@1";

/// The check file's row for one file: every field `agent-report check` prints.
#[derive(Clone, Debug)]
pub struct CheckRow {
    pub file: String,
    pub seq: Option<u64>,
    pub sha256: Option<String>,
    pub read_at: String,
    pub result: &'static str,
    pub reason: Option<String>,
    /// Only on thread.json and status.json rows: the pinned thread, or null.
    pub pin: Option<Json>,
}

impl CheckRow {
    fn json(&self) -> Json {
        let mut out = Map::new();
        out.insert("file".to_string(), json!(self.file));
        out.insert("seq".to_string(), json!(self.seq));
        out.insert("sha256".to_string(), json!(self.sha256));
        out.insert("readAt".to_string(), json!(self.read_at));
        out.insert("result".to_string(), json!(self.result));
        out.insert("reason".to_string(), json!(self.reason));
        if let Some(pin) = &self.pin {
            out.insert("pin".to_string(), pin.clone());
        }
        Json::Object(out)
    }
}

/// One accepted run row, as the Scoreboard's runs scan hands it over.
pub struct RunRow {
    pub file: String,
    pub seq: u64,
    pub sha256: String,
    pub first_seen_at: u64,
}

fn iso(at: u64) -> String {
    aac_num::ecma::to_iso_string(at as i64).unwrap_or_default()
}

/// `writers/<writer>/<rest>` -> its two halves; anything else has no row here.
fn writer_and_file(rel: &str) -> Option<(String, String)> {
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() < 3 || parts[0] != "writers" {
        return None;
    }
    Some((parts[1].to_string(), parts[2..].join("/")))
}

fn decode_percent(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = String::new();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && bytes[at + 1].is_ascii_hexdigit()
            && bytes[at + 2].is_ascii_hexdigit()
        {
            let hex = std::str::from_utf8(&bytes[at + 1..at + 3]).unwrap_or("00");
            let code = u32::from_str_radix(hex, 16).unwrap_or(0);
            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
            at += 3;
        } else {
            // Copy one whole UTF-8 character.
            let ch_len = utf8_len(bytes[at]);
            let end = (at + ch_len).min(bytes.len());
            out.push_str(&String::from_utf8_lossy(&bytes[at..end]));
            at = end;
        }
    }
    out
}

fn utf8_len(byte: u8) -> usize {
    if byte < 0x80 {
        1
    } else if byte >> 5 == 0b110 {
        2
    } else if byte >> 4 == 0b1110 {
        3
    } else if byte >> 3 == 0b11110 {
        4
    } else {
        1
    }
}

/// The composer's id normalisation (crates/aac-status/src/pinned.rs): the plain id,
/// percent-encoded, a `t3-thread://` link ending in it, or the bare tail — all compare equal.
pub fn normalise_thread_id(id: &str) -> String {
    let mut text = decode_percent(id.trim()).to_lowercase();
    if let Some(at) = text.rfind('/') {
        text = text[at + 1..].to_string();
    }
    if let Some(rest) = text.strip_prefix("mcp:launch:") {
        text = rest.to_string();
    }
    text
}

/// The pinned thread a report's t3Thread is shown under, or null when no pin has that id.
pub fn match_pin(id: Option<&str>, pins: &[(String, String)]) -> Json {
    let Some(id) = id.filter(|id| !id.is_empty()) else {
        return Json::Null;
    };
    let want = normalise_thread_id(id);
    if want.is_empty() {
        return Json::Null;
    }
    for (pin_id, title) in pins {
        if normalise_thread_id(pin_id) == want {
            return json!({"t3Thread": pin_id, "title": title});
        }
    }
    Json::Null
}

/// The rows of one reports root: writer -> file -> row (the newest row per file wins).
pub struct ReadersCheckWriter {
    root: PathBuf,
    rows: IndexMap<String, IndexMap<String, CheckRow>>,
    /// Each writer's `files` as last written (a digest of its rows' JSON), so a flush
    /// rewrites only the writers whose rows changed. A check file's `writtenAt` therefore
    /// stands still while its rows do; `agent-report check` matches rows by file and seq
    /// and never reads that time.
    written: std::collections::HashMap<String, [u8; 32]>,
}

impl ReadersCheckWriter {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            rows: IndexMap::new(),
            written: std::collections::HashMap::new(),
        }
    }

    fn put(&mut self, writer: &str, row: CheckRow, current: &mut std::collections::HashSet<String>) {
        // A folder that is not a writer id never gets a check file (F9).
        if !is_writer_name(writer) {
            return;
        }
        current.insert(format!("{writer}\n{}", row.file));
        self.rows
            .entry(writer.to_string())
            .or_default()
            .insert(row.file.clone(), row);
    }

    /// Replaces the status-side rows (thread.json, tasks.json, status.json, cards).
    pub fn record_status_scan(
        &mut self,
        reports: &super::reports::AgentReportsState,
        statuses: &super::status_docs::StatusDocsState,
        pins: &[(String, String)],
    ) {
        let mut current = std::collections::HashSet::new();
        for row in &reports.threads {
            let pin = match_pin(row.doc["agent"]["t3Thread"].as_str(), pins);
            self.put(
                &row.writer,
                CheckRow {
                    file: "thread.json".to_string(),
                    seq: row.doc["seq"].as_u64(),
                    sha256: Some(row.sha256.clone()),
                    read_at: iso(row.first_seen_at),
                    result: if row.problem.is_some() { "refused" } else { "accepted" },
                    reason: row.problem.as_ref().map(|problem| problem.reason.clone()),
                    pin: Some(pin),
                },
                &mut current,
            );
        }
        for row in &reports.tasks {
            self.put(
                &row.writer,
                CheckRow {
                    file: "tasks.json".to_string(),
                    seq: row.doc["seq"].as_u64(),
                    sha256: Some(row.sha256.clone()),
                    read_at: iso(row.first_seen_at),
                    result: if row.problem.is_some() { "refused" } else { "accepted" },
                    reason: row.problem.as_ref().map(|problem| problem.reason.clone()),
                    pin: None,
                },
                &mut current,
            );
        }
        for row in &reports.cards {
            let id = row.doc["id"].as_str().unwrap_or_default();
            self.put(
                &row.writer,
                CheckRow {
                    file: format!("cards/{id}.json"),
                    seq: row.doc["seq"].as_u64(),
                    sha256: Some(row.sha256.clone()),
                    read_at: iso(row.first_seen_at),
                    result: if row.problem.is_some() { "refused" } else { "accepted" },
                    reason: row.problem.as_ref().map(|problem| problem.reason.clone()),
                    pin: None,
                },
                &mut current,
            );
        }
        for doc in &statuses.docs {
            let writer = doc["writer"].as_str().unwrap_or_default();
            let t3_thread = doc["doc"]["t3Thread"].as_str().filter(|id| !id.is_empty());
            let problem = doc["problem"].as_object();
            self.put(
                writer,
                CheckRow {
                    file: "status.json".to_string(),
                    seq: doc["seq"].as_u64(),
                    sha256: doc["sha256"].as_str().map(str::to_string),
                    read_at: iso(doc["firstSeenAt"].as_u64().unwrap_or(0)),
                    result: if problem.is_some() { "refused" } else { "accepted" },
                    reason: problem
                        .and_then(|problem| problem.get("reason"))
                        .and_then(Json::as_str)
                        .map(str::to_string),
                    pin: Some(match_pin(t3_thread, pins)),
                },
                &mut current,
            );
        }
        // Files refused with no good copy to keep: the row is the refusal itself. A row put
        // this call keeps its seq, sha and pin; an earlier scan's row never hides this one.
        let problems = reports.problems.iter().chain(statuses.problems.iter());
        for problem in problems {
            let Some((writer, rest)) = writer_and_file(&problem.file) else {
                continue;
            };
            if current.contains(&format!("{writer}\n{rest}")) {
                continue;
            }
            let pin = if rest == "thread.json" || rest == "status.json" {
                Some(Json::Null)
            } else {
                None
            };
            self.put(
                &writer,
                CheckRow {
                    file: rest,
                    seq: None,
                    sha256: None,
                    read_at: iso(problem.since),
                    result: "refused",
                    reason: Some(problem.reason.clone()),
                    pin,
                },
                &mut current,
            );
        }
    }

    /// Replaces the runs rows from one Scoreboard runs scan.
    pub fn record_runs(&mut self, runs: &[RunRow], problems: &[Problem]) {
        let mut current = std::collections::HashSet::new();
        for run in runs {
            let Some((writer, rest)) = writer_and_file(&run.file) else {
                continue;
            };
            self.put(
                &writer,
                CheckRow {
                    file: rest,
                    seq: Some(run.seq),
                    sha256: Some(run.sha256.clone()),
                    read_at: iso(run.first_seen_at),
                    result: "accepted",
                    reason: None,
                    pin: None,
                },
                &mut current,
            );
        }
        for problem in problems {
            let Some((writer, rest)) = writer_and_file(&problem.file) else {
                continue;
            };
            if !rest.starts_with("runs/") || current.contains(&format!("{writer}\n{rest}")) {
                continue;
            }
            self.put(
                &writer,
                CheckRow {
                    file: rest,
                    seq: None,
                    sha256: None,
                    read_at: iso(problem.since),
                    result: "refused",
                    reason: Some(problem.reason.clone()),
                    pin: None,
                },
                &mut current,
            );
        }
    }

    /// The rows this writer's check file holds right now (tests and the flush below).
    pub fn rows_for(&self, writer: &str) -> Vec<CheckRow> {
        let mut rows: Vec<CheckRow> = self
            .rows
            .get(writer)
            .map(|files| files.values().cloned().collect())
            .unwrap_or_default();
        rows.sort_by(|a, b| compare(&a.file, &b.file));
        rows
    }

    /// Writes each changed writer's check file atomically, through the registry's leased
    /// writer; a writer whose rows are as last written keeps its file (and its
    /// `writtenAt`) untouched, so an idle share takes no synced writes per pass.
    /// Never creates the reports root (an unmounted share must not grow a local folder);
    /// a write that fails is logged once per flush, never thrown, and is retried next
    /// flush (its rows still count as unwritten).
    pub fn flush(&mut self, owners: &Owners, now: u64) {
        if owners.owner("readers-check") != Some(Role::RustService) {
            return;
        }
        // The reports root is never created (F3): under an existing root, only AAC's own
        // folders below it are made, by the leased writer's rule.
        if !aac_files::read::stat(&self.root).is_ok_and(|stat| stat.dir) {
            return;
        }
        let rows = match registry::rows() {
            Ok(rows) => rows,
            Err(_) => return,
        };
        let written_at = iso(now);
        let mut failed = false;
        for (writer, files) in &self.rows {
            let mut sorted: Vec<&CheckRow> = files.values().collect();
            sorted.sort_by(|a, b| compare(&a.file, &b.file));
            let files_json: Vec<Json> = sorted.iter().map(|row| row.json()).collect();
            let print: [u8; 32] =
                sha2::Sha256::digest(serde_json::to_string(&files_json).unwrap_or_default().as_bytes()).into();
            if self.written.get(writer) == Some(&print) {
                continue;
            }
            let body = format!(
                "{}\n",
                serde_json::to_string_pretty(&json!({
                    "schema": READERS_CHECK_SCHEMA,
                    "reader": "aac",
                    "writer": writer,
                    "writtenAt": written_at,
                    "files": files_json,
                }))
                .unwrap_or_default()
            );
            let name = format!("{writer}.json");
            match owners.claim(rows, "readers-check", &self.root, Some(&name)) {
                Ok(store) => {
                    if store.write(body.as_bytes()).is_err() {
                        failed = true;
                    } else {
                        self.written.insert(writer.clone(), print);
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

/// One log line per failed flush run (the check is a courtesy, never load-bearing).
fn tracing_check_failure() {
    static LOGGED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
    if !LOGGED.swap(true, std::sync::atomic::Ordering::Relaxed) {
        eprintln!("readers check flush failed");
    }
}

/// The check folder under a reports root.
pub fn check_dir(root: &Path) -> PathBuf {
    root.join("v1").join("readers").join("aac")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thread_ids_normalise_as_the_composer_does() {
        assert_eq!(normalise_thread_id("ABC-123"), "abc-123");
        assert_eq!(normalise_thread_id("t3-thread://x/ABC-123"), "abc-123");
        assert_eq!(normalise_thread_id("mcp:launch:abc-123"), "abc-123");
        assert_eq!(normalise_thread_id("%61bc-123"), "abc-123");
        assert_eq!(normalise_thread_id("mcp%3Alaunch%3Aabc-123"), "abc-123");
    }

    #[test]
    fn pins_match_by_normalised_id_only() {
        let pins = vec![("ABC-123".to_string(), "A title".to_string())];
        assert_eq!(
            match_pin(Some("t3-thread://x/abc-123"), &pins),
            json!({"t3Thread": "ABC-123", "title": "A title"})
        );
        assert_eq!(match_pin(Some("other"), &pins), Json::Null);
        assert_eq!(match_pin(None, &pins), Json::Null);
    }
}
