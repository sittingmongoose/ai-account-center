//! Compact status-documents reader (`writers/<writer>/status.json`, `agent-reports/status@1`).
//!
//! Replaces the two full copies of `serde_json::Value` DOM trees (`StatusDocsReader.entries` +
//! `StatusDocsState.docs`) with a single shared `Arc<CompactStatusEntry>` per writer holding
//! compact typed fields (`Box<str>`, `Box<[]>`, `[u8; 32]` SHA-256, numeric `MtimeSizeStamp`).

use super::primitives::{CompactProblem, CompactSha, CompactTransientMisses, MtimeSizeStamp};
use aac_status::share::report::report_v2;
use aac_status::share::status_docs::{Limits, StatusDocsState};
use aac_status::share::{
    MISSING_GRACE_MS, PERSISTENT_LISTING_ERROR, ReadError, SEQ_WENT_BACK, ShareFs, compare,
    folder_row_problem, is_missing, is_transient,
};
use aac_status::status_doc::{LoopingState, STATUS_SIZE_CAP, StatusDoc, parse_status_file};
use aac_status::validate::{is_writer_name, obj, share_json_from_bytes};
use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

const LISTING_ERROR: &str = "folder could not be listed";
const NOT_A_WRITER: &str = "folder name is not a writer name";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactStatusItem {
    pub(crate) id: Box<str>,
    pub(crate) title: Box<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactStatusBlocker {
    /// Raw compact JSON object of `additions` when non-empty (`None` for the common empty case).
    pub(crate) additions_json: Option<Box<[u8]>>,
    pub(crate) text: Box<str>,
    pub(crate) waiting_on: Box<str>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactStatusLooping {
    pub(crate) state: LoopingState,
    pub(crate) text: Option<Box<str>>,
}

/// Compact typed projection of `agent-reports/status@1`.
#[derive(Clone, Debug)]
pub(crate) struct CompactStatusDoc {
    /// Raw compact JSON object of `additions` (v2/v3 fields) only when non-empty.
    pub(crate) additions_json: Option<Box<[u8]>>,
    pub(crate) writer: Box<str>,
    pub(crate) seq: u64,
    pub(crate) written_at: Box<str>,
    pub(crate) t3_thread: Box<str>,
    pub(crate) summary: Box<str>,
    pub(crate) percent: Option<u64>,
    pub(crate) done: Box<[CompactStatusItem]>,
    pub(crate) left: Box<[CompactStatusItem]>,
    pub(crate) blockers: Box<[CompactStatusBlocker]>,
    pub(crate) looping: Option<Box<CompactStatusLooping>>,
}

impl CompactStatusDoc {
    fn from_status_doc(doc: StatusDoc) -> Self {
        let additions_json = if doc.additions.is_empty() {
            None
        } else {
            serde_json::to_vec(&doc.additions)
                .ok()
                .map(Vec::into_boxed_slice)
        };
        let done = doc
            .done
            .into_iter()
            .map(|item| CompactStatusItem {
                id: item.id.into_boxed_str(),
                title: item.title.into_boxed_str(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let left = doc
            .left
            .into_iter()
            .map(|item| CompactStatusItem {
                id: item.id.into_boxed_str(),
                title: item.title.into_boxed_str(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let blockers = doc
            .blockers
            .into_iter()
            .map(|b| CompactStatusBlocker {
                additions_json: if b.additions.is_empty() {
                    None
                } else {
                    serde_json::to_vec(&b.additions)
                        .ok()
                        .map(Vec::into_boxed_slice)
                },
                text: b.text.into_boxed_str(),
                waiting_on: b.waiting_on.into_boxed_str(),
            })
            .collect::<Vec<_>>()
            .into_boxed_slice();
        let looping = doc.looping.map(|l| {
            Box::new(CompactStatusLooping {
                state: l.state,
                text: l.text.map(String::into_boxed_str),
            })
        });
        Self {
            additions_json,
            writer: doc.writer.into_boxed_str(),
            seq: doc.seq,
            written_at: doc.written_at.into_boxed_str(),
            t3_thread: doc.t3_thread.into_boxed_str(),
            summary: doc.summary.into_boxed_str(),
            percent: doc.percent,
            done,
            left,
            blockers,
            looping,
        }
    }

    /// Materialize the exact `serde_json::to_value(&StatusDoc)` shape (in `StatusDoc` key order).
    pub(crate) fn to_json(&self) -> Json {
        let mut out = Map::new();
        if let Some(bytes) = &self.additions_json
            && let Ok(Json::Object(additions)) = serde_json::from_slice::<Json>(bytes)
        {
            for (k, v) in additions {
                out.insert(k, v);
            }
        }
        out.insert("writer".to_owned(), Json::from(self.writer.as_ref()));
        out.insert("seq".to_owned(), Json::from(self.seq));
        out.insert("writtenAt".to_owned(), Json::from(self.written_at.as_ref()));
        out.insert("t3Thread".to_owned(), Json::from(self.t3_thread.as_ref()));
        out.insert("summary".to_owned(), Json::from(self.summary.as_ref()));
        out.insert(
            "percent".to_owned(),
            self.percent.map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "done".to_owned(),
            Json::Array(
                self.done
                    .iter()
                    .map(|item| json!({"id": item.id.as_ref(), "title": item.title.as_ref()}))
                    .collect(),
            ),
        );
        out.insert(
            "left".to_owned(),
            Json::Array(
                self.left
                    .iter()
                    .map(|item| json!({"id": item.id.as_ref(), "title": item.title.as_ref()}))
                    .collect(),
            ),
        );
        out.insert(
            "blockers".to_owned(),
            Json::Array(
                self.blockers
                    .iter()
                    .map(|b| {
                        let mut m = Map::new();
                        if let Some(bytes) = &b.additions_json
                            && let Ok(Json::Object(additions)) =
                                serde_json::from_slice::<Json>(bytes)
                        {
                            for (k, v) in additions {
                                m.insert(k, v);
                            }
                        }
                        m.insert("text".to_owned(), Json::from(b.text.as_ref()));
                        m.insert("waitingOn".to_owned(), Json::from(b.waiting_on.as_ref()));
                        Json::Object(m)
                    })
                    .collect(),
            ),
        );
        out.insert(
            "looping".to_owned(),
            match self.looping.as_deref() {
                None => Json::Null,
                Some(l) => json!({
                    "state": match l.state {
                        LoopingState::Yes => "yes",
                        LoopingState::No => "no",
                        LoopingState::Unsure => "unsure",
                    },
                    "text": l.text.as_deref(),
                }),
            },
        );
        Json::Object(out)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactDocProblem {
    pub(crate) reason: Box<str>,
    pub(crate) since: u64,
}

/// One accepted status document revision, shared (`Arc`) between the reader and snapshot.
#[derive(Clone, Debug)]
pub(crate) struct CompactStatusEntry {
    pub(crate) writer: Box<str>,
    pub(crate) doc: CompactStatusDoc,
    pub(crate) sha256: CompactSha,
    pub(crate) written_at_ms: i64,
    pub(crate) first_seen_at: u64,
    pub(crate) problem: Option<Box<CompactDocProblem>>,
}

impl CompactStatusEntry {
    pub(crate) fn to_doc_row_json(&self) -> Json {
        json!({
            "writer": self.writer.as_ref(),
            "doc": self.doc.to_json(),
            "sha256": self.sha256.to_hex_string(),
            "seq": self.doc.seq,
            "writtenAtMs": self.written_at_ms,
            "firstSeenAt": self.first_seen_at,
            "problem": self.problem.as_deref().map(|p| json!({
                "reason": p.reason.as_ref(),
                "since": p.since,
            })),
        })
    }
}

#[derive(Clone, Debug)]
struct ReaderEntry {
    writer: Box<str>,
    good: Option<Arc<CompactStatusEntry>>,
    written_at_ms: i64,
    problem: Option<Box<CompactDocProblem>>,
    stamp: Option<MtimeSizeStamp>,
    missing_since: Option<u64>,
}

impl ReaderEntry {
    fn problem_only(writer: &str, reason: &str, since: u64) -> Self {
        Self {
            writer: writer.into(),
            good: None,
            written_at_ms: 0,
            problem: Some(Box::new(CompactDocProblem {
                reason: reason.into(),
                since,
            })),
            stamp: None,
            missing_since: None,
        }
    }
}

/// Compact snapshot of `StatusDocsReader`, holding shared `Arc<CompactStatusEntry>` rows.
#[derive(Clone, Debug)]
pub(crate) struct CompactStatusDocsState {
    pub(crate) state: &'static str,
    pub(crate) scanned_at: Option<u64>,
    pub(crate) docs: Box<[Arc<CompactStatusEntry>]>,
    pub(crate) problems: Box<[CompactProblem]>,
}

impl CompactStatusDocsState {
    pub(crate) fn empty() -> Self {
        Self {
            state: "missing",
            scanned_at: None,
            docs: Box::new([]),
            problems: Box::new([]),
        }
    }

    pub(crate) fn to_state_json(&self) -> Json {
        json!({
            "state": self.state,
            "scannedAt": self.scanned_at,
            "docs": self.docs.iter().map(|e| e.to_doc_row_json()).collect::<Vec<_>>(),
            "problems": self.problems.iter().map(CompactProblem::to_json).collect::<Vec<_>>(),
        })
    }

    /// Materialize transient `aac_status::share::status_docs::StatusDocsState` for `pinned_input::compose_input`
    /// only when pinned composition runs (never retained).
    pub(crate) fn to_status_docs_state(&self) -> StatusDocsState {
        StatusDocsState {
            state: self.state,
            scanned_at: self.scanned_at,
            docs: self.docs.iter().map(|e| e.to_doc_row_json()).collect(),
            problems: self
                .problems
                .iter()
                .map(CompactProblem::to_share_problem)
                .collect(),
        }
    }
}

fn parse_status_compact(
    bytes: &[u8],
    writer: &str,
) -> Result<(CompactStatusDoc, CompactSha, Option<i64>), String> {
    let raw = share_json_from_bytes(bytes, STATUS_SIZE_CAP)?;
    let doc = obj(Some(&raw), "document").map_err(|invalid| invalid.0)?;
    report_v2(doc).map_err(|invalid| invalid.0)?;
    let parsed = parse_status_file(bytes, writer)?;
    let written_at_ms = aac_num::ecma::parse_instant(&parsed.doc.written_at);
    let sha = CompactSha::from_hex_or_str(&parsed.sha256);
    let compact = CompactStatusDoc::from_status_doc(parsed.doc);
    Ok((compact, sha, written_at_ms))
}

pub(crate) struct CompactStatusDocsReader {
    root: PathBuf,
    limits: Limits,
    entries: IndexMap<Box<str>, ReaderEntry>,
    misses: HashMap<Box<str>, u64>,
    listing_misses: CompactTransientMisses,
    state: &'static str,
    scanned_at: Option<u64>,
    retained_over: Option<(usize, usize, u64)>,
}

impl CompactStatusDocsReader {
    pub(crate) fn new(root: PathBuf, limits: Limits) -> Self {
        Self {
            root,
            limits,
            entries: IndexMap::new(),
            misses: HashMap::new(),
            listing_misses: CompactTransientMisses::default(),
            state: "missing",
            scanned_at: None,
            retained_over: None,
        }
    }

    fn mark_unreadable(entry: &mut ReaderEntry, reason: &str, since: u64) -> bool {
        let held = entry.problem.as_deref();
        let problem = match held {
            Some(held) if held.reason.as_ref() == reason => CompactDocProblem {
                reason: reason.into(),
                since: held.since,
            },
            _ => CompactDocProblem {
                reason: reason.into(),
                since,
            },
        };
        let changed = held.map(|h| (h.reason.as_ref(), h.since))
            != Some((problem.reason.as_ref(), problem.since));
        let boxed = Box::new(problem);
        if changed && let Some(good) = &mut entry.good {
            Arc::make_mut(good).problem = Some(boxed.clone());
        }
        entry.problem = Some(boxed);
        entry.stamp = None;
        changed
    }

    fn record_problem(&mut self, rel: &str, writer: &str, reason: &str, since: u64) -> bool {
        let changed = self
            .entries
            .get(rel)
            .and_then(|entry| entry.problem.as_deref())
            .map(|problem| problem.reason.as_ref())
            != Some(reason);
        match self.entries.get_mut(rel) {
            Some(entry) => {
                let boxed = Box::new(CompactDocProblem {
                    reason: reason.into(),
                    since,
                });
                if let Some(good) = &mut entry.good {
                    Arc::make_mut(good).problem = Some(boxed.clone());
                }
                entry.problem = Some(boxed);
                entry.stamp = None;
            }
            None => {
                self.entries
                    .insert(rel.into(), ReaderEntry::problem_only(writer, reason, since));
            }
        }
        changed
    }

    pub(crate) fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> bool {
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
        let mut failed_listings: HashSet<Box<str>> = HashSet::new();
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
        let listed: HashSet<String> = writers.into_iter().chain(rejected).collect();
        let keys: Vec<Box<str>> = self.entries.keys().cloned().collect();
        for rel in keys {
            let entry = self.entries.get_mut(&rel).expect("listed entry");
            if listed.contains(entry.writer.as_ref()) {
                continue;
            }
            let missing_since = *entry.missing_since.get_or_insert(now);
            if now.saturating_sub(missing_since) > MISSING_GRACE_MS {
                self.entries.shift_remove(&rel);
                changed = true;
            }
        }
        let over = self.entries.len().saturating_sub(self.limits.retained);
        if over > 0 {
            let mut oldest: Vec<(Box<str>, i64, Box<str>)> = self
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

    fn root_listing_failed(&mut self, error: &std::io::Error, now: u64) -> bool {
        let next = if is_missing(error) {
            "missing"
        } else {
            "error"
        };
        let mut changed = self.state != next;
        self.state = next;
        self.scanned_at = Some(now);
        let mut writers_only: HashSet<Box<str>> = HashSet::new();
        writers_only.insert("writers".into());
        self.listing_misses.keep_only(&writers_only);
        let (reason, since) = if is_transient(error) {
            match self.listing_misses.miss("writers", now) {
                None => return changed,
                Some(first) => (PERSISTENT_LISTING_ERROR, first),
            }
        } else {
            (LISTING_ERROR, now)
        };
        let row_reason = folder_row_problem(reason);
        let keys: Vec<Box<str>> = self.entries.keys().cloned().collect();
        for rel in keys {
            let entry = self.entries.get_mut(&rel).expect("listed entry");
            if Self::mark_unreadable(entry, &row_reason, since) {
                changed = true;
            }
        }
        changed
    }

    fn scan_writer(
        &mut self,
        fs: &dyn ShareFs,
        writer: &str,
        now: u64,
        failed_listings: &mut HashSet<Box<str>>,
    ) -> bool {
        let rel = format!("writers/{writer}/status.json");
        let folder = self.root.join("v1").join("writers").join(writer);
        let listed = match fs.list(&folder) {
            Ok(listed) => listed,
            Err(error) => {
                let (reason, since) = if is_transient(&error) {
                    let key: Box<str> = format!("writers/{writer}").into();
                    let miss = self.listing_misses.miss(&key, now);
                    failed_listings.insert(key);
                    match miss {
                        None => return false,
                        Some(first) => (PERSISTENT_LISTING_ERROR, first),
                    }
                } else {
                    (LISTING_ERROR, now)
                };
                let reason = folder_row_problem(reason);
                return match self.entries.get_mut(rel.as_str()) {
                    Some(entry) => Self::mark_unreadable(entry, &reason, since),
                    None => {
                        self.entries.insert(
                            rel.into(),
                            ReaderEntry::problem_only(writer, &reason, since),
                        );
                        true
                    }
                };
            }
        };
        if !listed.iter().any(|entry| entry.name == "status.json") {
            return match self.entries.get_mut(rel.as_str()) {
                None => false,
                Some(entry) => {
                    let missing_since = *entry.missing_since.get_or_insert(now);
                    if now.saturating_sub(missing_since) > MISSING_GRACE_MS {
                        self.entries.shift_remove(rel.as_str());
                        return true;
                    }
                    false
                }
            };
        }
        if let Some(entry) = self.entries.get_mut(rel.as_str()) {
            entry.missing_since = None;
        }
        let file = folder.join("status.json");
        let mut problem: Option<(String, u64)> = None;
        let mut raced = false;
        let mut stamp: Option<MtimeSizeStamp> = None;
        let mut bytes: Option<Vec<u8>> = None;
        match fs.stat(&file) {
            Ok(stat) => {
                if !stat.file {
                    problem = Some(("is not a regular file".to_owned(), now));
                } else {
                    let next_stamp = MtimeSizeStamp::from_stat(&stat);
                    let unchanged = self.entries.get(rel.as_str()).is_some_and(|entry| {
                        entry.stamp == Some(next_stamp) && entry.problem.is_none()
                    });
                    if unchanged {
                        self.misses.remove(rel.as_str());
                        return false;
                    }
                    if stat.size > STATUS_SIZE_CAP as u64 {
                        problem = Some(("is over the size cap".to_owned(), now));
                    } else {
                        stamp = Some(next_stamp);
                        match fs.read(&file, STATUS_SIZE_CAP) {
                            Ok(read) => bytes = Some(read),
                            Err(ReadError::OverCap) => {
                                problem = Some(("is over the size cap".to_owned(), now));
                            }
                            Err(ReadError::Io(error)) => {
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
            self.misses.remove(rel.as_str());
            return self.record_problem(&rel, writer, &reason, since);
        }
        self.misses.remove(rel.as_str());
        let bytes = bytes.unwrap_or_default();
        let (doc, sha256, written_at_ms) = match parse_status_compact(&bytes, writer) {
            Ok(parsed) => parsed,
            Err(reason) => {
                return self.record_problem(&rel, writer, &format!("status.json {reason}"), now);
            }
        };
        let seq = doc.seq;
        if let Some(held) = self.entries.get(rel.as_str())
            && let Some(good) = &held.good
        {
            if seq < good.doc.seq {
                let entry = self.entries.get_mut(rel.as_str()).expect("held entry");
                return Self::mark_unreadable(entry, &format!("status.json {SEQ_WENT_BACK}"), now);
            }
            if good.sha256 == sha256 && held.problem.is_none() {
                return false;
            }
        }
        let written_at_ms = written_at_ms.unwrap_or(now as i64);
        let shared = Arc::new(CompactStatusEntry {
            writer: writer.into(),
            doc,
            sha256,
            written_at_ms,
            first_seen_at: now,
            problem: None,
        });
        self.entries.insert(
            rel.into(),
            ReaderEntry {
                writer: writer.into(),
                good: Some(shared),
                written_at_ms,
                problem: None,
                stamp,
                missing_since: None,
            },
        );
        true
    }

    fn read_error_problem(
        misses: &mut HashMap<Box<str>, u64>,
        rel: &str,
        error: &std::io::Error,
        now: u64,
        raced: &mut bool,
    ) -> (String, u64) {
        if is_transient(error) {
            match misses.get(rel) {
                None => {
                    misses.insert(rel.into(), now);
                    *raced = true;
                    (String::new(), now)
                }
                Some(first) => ("could not be read on two scans in a row".to_owned(), *first),
            }
        } else {
            ("could not be read".to_owned(), now)
        }
    }

    pub(crate) fn snapshot(&self) -> CompactStatusDocsState {
        let mut entries: Vec<&ReaderEntry> = self.entries.values().collect();
        entries.sort_by(|a, b| compare(&a.writer, &b.writer));
        let docs: Vec<Arc<CompactStatusEntry>> = entries
            .iter()
            .filter_map(|entry| entry.good.clone())
            .collect();
        let mut problems: Vec<CompactProblem> = entries
            .iter()
            .filter_map(|entry| {
                let problem = entry.problem.as_deref()?;
                Some(CompactProblem::new(
                    &format!("writers/{}/status.json", entry.writer),
                    &problem.reason,
                    problem.since,
                ))
            })
            .collect();
        if let Some((count, limit, since)) = self.retained_over {
            problems.push(CompactProblem::new(
                "limits/status-docs",
                &format!(
                    "{count} documents past the retained limit of {limit} were not kept (the most recently written are kept)"
                ),
                since,
            ));
        }
        CompactStatusDocsState {
            state: self.state,
            scanned_at: self.scanned_at,
            docs: docs.into_boxed_slice(),
            problems: problems.into_boxed_slice(),
        }
    }
}

fn newest_folders(
    names: Vec<String>,
    limit: usize,
    fs: &dyn ShareFs,
    dir: &std::path::Path,
) -> Vec<String> {
    if names.len() <= limit {
        return names;
    }
    let mut dated: Vec<(String, f64)> = names
        .into_iter()
        .map(|name| {
            let mtime = fs
                .stat(&dir.join(&name))
                .map(|stat| stat.mtime_ms)
                .unwrap_or(0.0);
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
