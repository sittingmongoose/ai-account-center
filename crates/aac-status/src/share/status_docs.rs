//! The status documents reader (`status-docs.ts`): `writers/<writer>/status.json`
//! (`agent-reports/status@1`), scanned under the agent-reports rules. The projection is this
//! crate's own `status_doc::parse_status_file`; the reader adds the share scan, the v2
//! validation the TypeScript wrapper ran before the native call, and the last-good-copy state.

use super::{
    MISSING_GRACE_MS, PERSISTENT_LISTING_ERROR, Problem, SEQ_WENT_BACK, ShareFs, TransientMisses, compare,
    folder_row_problem, is_missing, is_transient,
};
use crate::status_doc::{STATUS_SIZE_CAP, parse_status_file};
use crate::validate::{is_writer_name, obj, share_json_from_bytes};
use indexmap::IndexMap;
use serde_json::{Value as Json, json};
use std::collections::HashSet;
use std::path::PathBuf;

/// The agent-reports reader's plain listing failure.
const LISTING_ERROR: &str = "folder could not be listed";
/// How the agent-reports reader words a writer folder that is not a writer id.
const NOT_A_WRITER: &str = "folder name is not a writer name";

/// As many writers as the agent-reports reader reads, and how many documents stay retained.
pub const STATUS_DOCS_LIMITS: Limits = Limits {
    writers: 1000,
    retained: 1000,
};

#[derive(Clone, Copy)]
pub struct Limits {
    pub writers: usize,
    pub retained: usize,
}

#[derive(Clone, Debug)]
struct DocProblem {
    reason: String,
    since: u64,
}

#[derive(Clone, Debug)]
struct Entry {
    writer: String,
    doc: Json,
    sha256: String,
    seq: u64,
    written_at_ms: i64,
    first_seen_at: u64,
    problem: Option<DocProblem>,
    stat_key: Option<String>,
    missing_since: Option<u64>,
}

impl Entry {
    fn problem_only(writer: &str, reason: &str, since: u64, first_seen_at: u64) -> Self {
        Self {
            writer: writer.to_string(),
            doc: json!({}),
            sha256: String::new(),
            seq: 0,
            written_at_ms: 0,
            first_seen_at,
            problem: Some(DocProblem {
                reason: reason.to_string(),
                since,
            }),
            stat_key: None,
            missing_since: None,
        }
    }
}

/// The reader's snapshot: what the pinned panel's status documents hold.
#[derive(Clone)]
pub struct StatusDocsState {
    pub state: &'static str,
    pub scanned_at: Option<u64>,
    pub docs: Vec<Json>,
    pub problems: Vec<Problem>,
}

pub struct StatusDocsReader {
    root: PathBuf,
    limits: Limits,
    entries: IndexMap<String, Entry>,
    misses: std::collections::HashMap<String, u64>,
    listing_misses: TransientMisses,
    state: &'static str,
    scanned_at: Option<u64>,
    retained_over: Option<(usize, usize, u64)>,
}

/// The document's parse: the byte rules (with the one secret policy), the v2 validation the
/// TypeScript wrapper ran, then this crate's projection. The refusal is the reader's reason.
fn parse_status(bytes: &[u8], writer: &str) -> Result<(Json, String, u64, Option<i64>), String> {
    let raw = share_json_from_bytes(bytes, STATUS_SIZE_CAP)?;
    let doc = obj(Some(&raw), "document").map_err(|invalid| invalid.0)?;
    super::report::report_v2(doc).map_err(|invalid| invalid.0)?;
    let parsed = parse_status_file(bytes, writer)?;
    let written_at_ms = aac_num::ecma::parse_instant(&parsed.doc.written_at);
    let doc = serde_json::to_value(&parsed.doc).map_err(|_| "could not be validated".to_string())?;
    Ok((doc, parsed.sha256, parsed.doc.seq, written_at_ms))
}

impl StatusDocsReader {
    pub fn new(root: PathBuf, limits: Limits) -> Self {
        Self {
            root,
            limits,
            entries: IndexMap::new(),
            misses: std::collections::HashMap::new(),
            listing_misses: TransientMisses::default(),
            state: "missing",
            scanned_at: None,
            retained_over: None,
        }
    }

    fn mark_unreadable(entry: &mut Entry, reason: &str, since: u64) -> bool {
        let held = entry.problem.as_ref();
        let problem = match held {
            Some(held) if held.reason == reason => DocProblem {
                reason: reason.to_string(),
                since: held.since,
            },
            _ => DocProblem {
                reason: reason.to_string(),
                since,
            },
        };
        let changed = held.map(|h| (h.reason.as_str(), h.since)) != Some((problem.reason.as_str(), problem.since));
        entry.problem = Some(problem);
        entry.stat_key = None; // Retry the read next scan.
        changed
    }

    fn record_problem(&mut self, rel: &str, writer: &str, reason: &str, since: u64) -> bool {
        let changed = self
            .entries
            .get(rel)
            .and_then(|entry| entry.problem.as_ref())
            .map(|problem| problem.reason.as_str())
            != Some(reason);
        match self.entries.get_mut(rel) {
            Some(entry) => {
                entry.problem = Some(DocProblem {
                    reason: reason.to_string(),
                    since,
                });
                entry.stat_key = None;
            }
            None => {
                self.entries
                    .insert(rel.to_string(), Entry::problem_only(writer, reason, since, since));
            }
        }
        changed
    }

    /// One rescan; resolves whether anything changed.
    pub fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> bool {
        let writers_dir = self.root.join("v1").join("writers");
        let listed = match fs.list(&writers_dir) {
            Ok(listed) => listed,
            Err(error) => return self.root_listing_failed(&error, now),
        };
        let mut writers = Vec::new();
        let mut rejected = Vec::new();
        for entry in listed {
            if !entry.is_dir || entry.name.starts_with('.') {
                continue;
            }
            if is_writer_name(&entry.name) {
                writers.push(entry.name);
            } else {
                rejected.push(entry.name);
            }
        }
        let mut changed = false;
        let mut failed_listings = HashSet::new();
        for name in &rejected {
            let rel = format!("writers/{name}/status.json");
            if self.record_problem(&rel, name, NOT_A_WRITER, now) {
                changed = true;
            }
        }
        let scanned = newest_folders(writers.clone(), self.limits.writers, fs, &writers_dir);
        for writer in &scanned {
            if self.scan_writer(fs, writer, now, &mut failed_listings) {
                changed = true;
            }
        }
        self.listing_misses.keep_only(&failed_listings);
        // A writer folder no longer listed was withdrawn: the same removal grace as a
        // missing status.json. Writers past the scan's limit were not tried, not withdrawn.
        let listed: HashSet<String> = writers.into_iter().chain(rejected).collect();
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for rel in keys {
            let entry = self.entries.get_mut(&rel).expect("listed entry");
            if listed.contains(&entry.writer) {
                continue;
            }
            let missing_since = *entry.missing_since.get_or_insert(now);
            if now.saturating_sub(missing_since) > MISSING_GRACE_MS {
                self.entries.shift_remove(&rel);
                changed = true;
            }
        }
        // The retained documents themselves are bounded: past the bound the oldest-written
        // are dropped, and that is named as a problem, never done silently.
        let over = self.entries.len().saturating_sub(self.limits.retained);
        if over > 0 {
            let mut oldest: Vec<(String, i64, String)> = self
                .entries
                .iter()
                .map(|(rel, entry)| (rel.clone(), entry.written_at_ms, entry.writer.clone()))
                .collect();
            oldest.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| compare(&a.2, &b.2)));
            for (rel, _, _) in oldest.into_iter().take(over) {
                self.entries.shift_remove(&rel);
            }
            let since = self.retained_over.map(|(_, _, since)| since).unwrap_or(now);
            self.retained_over = Some((over, self.limits.retained, since));
            changed = true;
        } else if self.retained_over.is_some() {
            self.retained_over = None;
            changed = true;
        }
        self.scanned_at = Some(now);
        self.state = "ok";
        changed
    }

    /// The writers root itself could not be listed: every cached document is marked with
    /// the two-scan rule; the state stays missing/error as the folder says.
    fn root_listing_failed(&mut self, error: &std::io::Error, now: u64) -> bool {
        let next = if is_missing(error) { "missing" } else { "error" };
        let mut changed = self.state != next;
        self.state = next;
        self.scanned_at = Some(now);
        // The writer folders were not tried this scan: their own miss counts start over.
        let mut writers_only = HashSet::new();
        writers_only.insert("writers".to_string());
        self.listing_misses.keep_only(&writers_only);
        let (reason, since) = if is_transient(error) {
            match self.listing_misses.miss("writers", now) {
                None => return changed, // The share's race: keep the last good copies quiet.
                Some(first) => (PERSISTENT_LISTING_ERROR, first),
            }
        } else {
            (LISTING_ERROR, now)
        };
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for rel in keys {
            let entry = self.entries.get_mut(&rel).expect("listed entry");
            if Self::mark_unreadable(entry, &folder_row_problem(reason), since) {
                changed = true;
            }
        }
        changed
    }

    /// One writer's `status.json`. Resolves whether anything the view shows changed.
    fn scan_writer(&mut self, fs: &dyn ShareFs, writer: &str, now: u64, failed_listings: &mut HashSet<String>) -> bool {
        let rel = format!("writers/{writer}/status.json");
        let folder = self.root.join("v1").join("writers").join(writer);
        let listed = match fs.list(&folder) {
            Ok(listed) => listed,
            Err(error) => {
                let (reason, since) = if is_transient(&error) {
                    failed_listings.insert(format!("writers/{writer}"));
                    match self.listing_misses.miss(&format!("writers/{writer}"), now) {
                        None => return false,
                        Some(first) => (PERSISTENT_LISTING_ERROR, first),
                    }
                } else {
                    (LISTING_ERROR, now)
                };
                let reason = folder_row_problem(reason);
                return match self.entries.get_mut(&rel) {
                    Some(entry) => Self::mark_unreadable(entry, &reason, since),
                    None => {
                        self.entries
                            .insert(rel, Entry::problem_only(writer, &reason, since, now));
                        true
                    }
                };
            }
        };
        if !listed.iter().any(|entry| entry.name == "status.json") {
            // Not listed is not withdrawn until the grace passes.
            return match self.entries.get_mut(&rel) {
                None => false,
                Some(entry) => {
                    let missing_since = *entry.missing_since.get_or_insert(now);
                    if now.saturating_sub(missing_since) > MISSING_GRACE_MS {
                        self.entries.shift_remove(&rel);
                        return true;
                    }
                    false
                }
            };
        }
        if let Some(entry) = self.entries.get_mut(&rel) {
            entry.missing_since = None;
        }
        let file = folder.join("status.json");
        let mut problem: Option<(String, u64)> = None;
        let mut raced = false;
        let mut stat_key: Option<String> = None;
        let mut bytes: Option<Vec<u8>> = None;
        match fs.stat(&file) {
            Ok(stat) => {
                if !stat.file {
                    problem = Some(("is not a regular file".to_string(), now));
                } else {
                    let key = format!("{}:{}", aac_num::ecma::to_string(stat.mtime_ms), stat.size);
                    let unchanged = self.entries.get(&rel).is_some_and(|entry| {
                        entry.stat_key.as_deref() == Some(key.as_str()) && entry.problem.is_none()
                    });
                    if unchanged {
                        self.misses.remove(&rel);
                        return false;
                    }
                    if stat.size > STATUS_SIZE_CAP as u64 {
                        problem = Some(("is over the size cap".to_string(), now));
                    } else {
                        stat_key = Some(key);
                        match fs.read(&file, STATUS_SIZE_CAP) {
                            Ok(read) => bytes = Some(read),
                            Err(super::ReadError::OverCap) => {
                                problem = Some(("is over the size cap".to_string(), now));
                            }
                            Err(super::ReadError::Io(error)) => {
                                problem = Some(Self::read_error_problem(
                                    &mut self.misses,
                                    &rel,
                                    &error,
                                    now,
                                    &mut raced,
                                ));
                            }
                        }
                    }
                }
            }
            Err(error) => {
                problem = Some(Self::read_error_problem(
                    &mut self.misses,
                    &rel,
                    &error,
                    now,
                    &mut raced,
                ));
            }
        }
        if raced {
            return false;
        }
        if let Some((reason, since)) = problem {
            self.misses.remove(&rel);
            return self.record_problem(&rel, writer, &reason, since);
        }
        self.misses.remove(&rel);
        let bytes = bytes.unwrap_or_default();
        let (doc, sha256, seq, written_at_ms) = match parse_status(&bytes, writer) {
            Ok(parsed) => parsed,
            Err(reason) => {
                return self.record_problem(&rel, writer, &format!("status.json {reason}"), now);
            }
        };
        let held = self.entries.get(&rel);
        if let Some(held) = held {
            if !held.sha256.is_empty() && seq < held.seq {
                let entry = self.entries.get_mut(&rel).expect("held entry");
                return Self::mark_unreadable(entry, &format!("status.json {SEQ_WENT_BACK}"), now);
            }
            if held.sha256 == sha256 && held.problem.is_none() {
                return false;
            }
        }
        self.entries.insert(
            rel,
            Entry {
                writer: writer.to_string(),
                doc,
                sha256,
                seq,
                written_at_ms: written_at_ms.unwrap_or(now as i64),
                first_seen_at: now,
                problem: None,
                stat_key,
                missing_since: None,
            },
        );
        true
    }

    /// ENOENT or ESTALE once is the share's race; the same error on the scan before too is
    /// a file this reader cannot read (the status reader's own short words).
    fn read_error_problem(
        misses: &mut std::collections::HashMap<String, u64>,
        rel: &str,
        error: &std::io::Error,
        now: u64,
        raced: &mut bool,
    ) -> (String, u64) {
        if is_transient(error) {
            match misses.get(rel) {
                None => {
                    misses.insert(rel.to_string(), now);
                    *raced = true;
                    (String::new(), now)
                }
                Some(first) => ("could not be read on two scans in a row".to_string(), *first),
            }
        } else {
            ("could not be read".to_string(), now)
        }
    }

    pub fn snapshot(&self) -> StatusDocsState {
        let mut entries: Vec<&Entry> = self.entries.values().collect();
        entries.sort_by(|a, b| compare(&a.writer, &b.writer));
        let docs: Vec<Json> = entries
            .iter()
            .filter(|entry| !entry.sha256.is_empty())
            .map(|entry| {
                json!({
                    "writer": entry.writer,
                    "doc": entry.doc,
                    "sha256": entry.sha256,
                    "seq": entry.seq,
                    "writtenAtMs": entry.written_at_ms,
                    "firstSeenAt": entry.first_seen_at,
                    "problem": entry.problem.as_ref().map(|problem| {
                        json!({"reason": problem.reason, "since": problem.since})
                    }),
                })
            })
            .collect();
        let mut problems: Vec<Problem> = entries
            .iter()
            .filter_map(|entry| {
                let problem = entry.problem.as_ref()?;
                Some(Problem {
                    file: format!("writers/{}/status.json", entry.writer),
                    reason: problem.reason.clone(),
                    since: problem.since,
                })
            })
            .collect();
        if let Some((count, limit, since)) = self.retained_over {
            problems.push(Problem {
                file: "limits/status-docs".to_string(),
                reason: format!(
                    "{count} documents past the retained limit of {limit} were not kept (the most recently written are kept)"
                ),
                since,
            });
        }
        StatusDocsState {
            state: self.state,
            scanned_at: self.scanned_at,
            docs,
            problems,
        }
    }
}

/// The agent-reports reader's newest-folder selection, over the writer folders.
fn newest_folders(names: Vec<String>, limit: usize, fs: &dyn ShareFs, dir: &std::path::Path) -> Vec<String> {
    if names.len() <= limit {
        return names;
    }
    let mut dated: Vec<(String, f64)> = names
        .into_iter()
        .map(|name| {
            let mtime = fs.stat(&dir.join(&name)).map(|stat| stat.mtime_ms).unwrap_or(0.0);
            (name, mtime)
        })
        .collect();
    dated.sort_by(|a, b| {
        b.1.partial_cmp(&a.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| compare(&a.0, &b.0))
    });
    dated.truncate(limit);
    dated.into_iter().map(|(name, _)| name).collect()
}

/// The snapshot as the pinned panel's status documents member.
pub fn state_json(state: &StatusDocsState) -> Json {
    json!({
        "state": state.state,
        "scannedAt": state.scanned_at,
        "docs": state.docs,
        "problems": state.problems.iter().map(|problem| {
            json!({"file": problem.file, "reason": problem.reason, "since": problem.since})
        }).collect::<Vec<_>>(),
    })
}
