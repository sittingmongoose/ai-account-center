//! Compact pm-mail registry reader and shared snapshot.
//!
//! Replaces retained `serde_json::Value` `IndexMap` trees (both in `PmMailRegistryReader`
//! and in `StatusOwned.state.registry` / `Produce.state.registry` / `CollectedView.registry`)
//! with one shared `Arc<CompactRegistryThread>` per registry entry (~0.8 MiB vs ~18-20 MiB
//! for 5,182 entries).

use super::primitives::{
    CompactRegistryProblem, CompactTransientMisses, StatStamp, proto_named_opt, proto_named_str,
};
use aac_status::share::registry::parse_registry_entry;
use aac_status::share::{PERSISTENT_SHARE_ERROR, ReadError, ShareFs, compare};
use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

const REGISTRY_FILE_CAP: usize = 64 * 1024;

/// One parsed pm-mail registry entry in compact typed form (~96 bytes + short strings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactRegistryThread {
    pub(crate) name: Box<str>,
    pub(crate) kind: Option<Box<str>>,
    pub(crate) machine: Option<Cow<'static, str>>,
    pub(crate) about: Option<Box<str>>,
    /// Parsed epoch ms for `lastSeen` (already canonicalized via `to_iso_string`).
    pub(crate) last_seen_ms: Option<i64>,
    pub(crate) proto_unsafe: bool,
}

impl CompactRegistryThread {
    fn from_parsed_json(parsed: &Json) -> Option<Self> {
        let obj = parsed.as_object()?;
        let name = obj.get("name")?.as_str()?;
        let kind = obj.get("kind").and_then(Json::as_str);
        let machine_raw = obj.get("machine").and_then(Json::as_str);
        let machine = machine_raw.map(|m| match static_machine(m) {
            Some(s) => Cow::Borrowed(s),
            None => Cow::Owned(m.to_owned()),
        });
        let about = obj.get("about").and_then(Json::as_str);
        let last_seen_ms = obj
            .get("lastSeen")
            .and_then(Json::as_str)
            .and_then(aac_num::ecma::parse_instant);
        let proto_unsafe = proto_named_str(name)
            || proto_named_opt(kind)
            || proto_named_opt(machine_raw)
            || proto_named_opt(about);
        Some(Self {
            name: name.into(),
            kind: kind.map(Into::into),
            machine,
            about: about.map(Into::into),
            last_seen_ms,
            proto_unsafe,
        })
    }

    pub(crate) fn last_seen_iso(&self) -> Option<String> {
        self.last_seen_ms.and_then(aac_num::ecma::to_iso_string)
    }

    fn update_hasher(&self, hasher: &mut Sha256) {
        hash_str(hasher, &self.name);
        hash_opt_str(hasher, self.kind.as_deref());
        hash_opt_str(hasher, self.machine.as_deref());
        hash_opt_str(hasher, self.about.as_deref());
        match self.last_seen_ms {
            Some(ms) => {
                hasher.update([1u8]);
                hasher.update(ms.to_le_bytes());
            }
            None => hasher.update([0u8]),
        }
    }
}

/// Map a machine id string back to a static slice (matching `computers::registry()` and the
/// heuristics in `aac_status::share::registry::registry_machine`).
fn static_machine(value: &str) -> Option<&'static str> {
    if let Some(id) = aac_status::computers::exact_computer(value) {
        return Some(id);
    }
    match value {
        "vm" => Some("vm"),
        "mac" => Some("mac"),
        "windows" => Some("windows"),
        "nas1" => Some("nas1"),
        other => aac_status::computers::machine_ids()
            .into_iter()
            .find(|&id| id == other),
    }
}

/// One row in a `CompactRegistrySnapshot`: shares the reader's `Arc<CompactRegistryThread>`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactRegistryRow {
    pub(crate) thread: Arc<CompactRegistryThread>,
    pub(crate) read_at: u64,
    pub(crate) problem: Option<Box<CompactRegistryProblem>>,
}

impl CompactRegistryRow {
    pub(crate) fn to_json(&self) -> Json {
        let mut row = Map::new();
        row.insert("name".to_owned(), Json::from(self.thread.name.as_ref()));
        row.insert(
            "kind".to_owned(),
            self.thread
                .kind
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        row.insert(
            "machine".to_owned(),
            self.thread
                .machine
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        row.insert(
            "about".to_owned(),
            self.thread
                .about
                .as_deref()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        row.insert(
            "lastSeen".to_owned(),
            self.thread
                .last_seen_iso()
                .map(Json::from)
                .unwrap_or(Json::Null),
        );
        row.insert("readAt".to_owned(), Json::from(self.read_at));
        row.insert(
            "problem".to_owned(),
            self.problem
                .as_deref()
                .map(CompactRegistryProblem::to_json)
                .unwrap_or(Json::Null),
        );
        Json::Object(row)
    }

    pub(crate) fn proto_unsafe(&self) -> bool {
        self.thread.proto_unsafe
            || self
                .problem
                .as_deref()
                .is_some_and(CompactRegistryProblem::proto_unsafe)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactRegistryError {
    pub(crate) reason: Box<str>,
    pub(crate) since: u64,
}

impl CompactRegistryError {
    pub(crate) fn to_json(&self) -> Json {
        json!({"reason": self.reason.as_ref(), "since": self.since})
    }
}

/// Complete collected wrapper for the pm-mail registry (`{data, asOf, error, missing?, problems?}`),
/// shared across `StatusOwned`, `Produce`, and `status_fed::CollectedView` with zero JSON tree retention.
#[derive(Clone, Debug)]
pub(crate) struct CompactRegistrySnapshot {
    pub(crate) data: Option<Box<[CompactRegistryRow]>>,
    pub(crate) as_of: Option<u64>,
    pub(crate) error: Option<Box<CompactRegistryError>>,
    pub(crate) missing: Option<bool>,
    pub(crate) problems: Option<Box<[CompactRegistryProblem]>>,
    pub(crate) content_hash: [u8; 32],
    pub(crate) proto_unsafe: bool,
}

impl CompactRegistrySnapshot {
    pub(crate) fn initial() -> Self {
        let mut out = Self {
            data: None,
            as_of: None,
            error: None,
            missing: None,
            problems: None,
            content_hash: [0u8; 32],
            proto_unsafe: false,
        };
        out.content_hash = out.compute_hash();
        out
    }

    pub(crate) fn ok(
        rows: Vec<CompactRegistryRow>,
        problems: Vec<CompactRegistryProblem>,
        as_of: u64,
    ) -> Self {
        let proto_unsafe = rows.iter().any(CompactRegistryRow::proto_unsafe)
            || problems.iter().any(CompactRegistryProblem::proto_unsafe);
        let mut out = Self {
            data: Some(rows.into_boxed_slice()),
            as_of: Some(as_of),
            error: None,
            missing: None,
            problems: Some(problems.into_boxed_slice()),
            content_hash: [0u8; 32],
            proto_unsafe,
        };
        out.content_hash = out.compute_hash();
        out
    }

    /// Equivalent to `status_owned::failed_registry`: keeps last good rows and problems,
    /// updates `asOf` when `last_good_as_of != 0`, sets `error` and `missing`.
    pub(crate) fn failed(&self, reason: &str, now: u64, last_good_as_of: u64) -> Self {
        let missing = reason.contains("missing");
        let since = self
            .error
            .as_deref()
            .filter(|held| held.reason.as_ref() == reason)
            .map(|held| held.since)
            .unwrap_or(now);
        let as_of = if last_good_as_of != 0 {
            Some(last_good_as_of)
        } else {
            self.as_of
        };
        let proto_unsafe = self.proto_unsafe || proto_named_str(reason);
        let mut out = Self {
            data: self.data.clone(),
            as_of,
            error: Some(Box::new(CompactRegistryError {
                reason: reason.into(),
                since,
            })),
            missing: Some(missing),
            problems: self.problems.clone(),
            content_hash: [0u8; 32],
            proto_unsafe,
        };
        out.content_hash = out.compute_hash();
        out
    }

    /// Build a compact snapshot from a raw JSON collected wrapper (used by test fixtures).
    #[cfg(test)]
    pub(crate) fn from_json(value: &Json) -> Self {
        let data = value.get("data").and_then(Json::as_array).map(|rows| {
            rows.iter()
                .filter_map(|row| {
                    let thread = CompactRegistryThread::from_parsed_json(row)?;
                    let read_at = row.get("readAt").and_then(Json::as_u64).unwrap_or(0);
                    let problem = row.get("problem").and_then(Json::as_object).map(|p| {
                        Box::new(CompactRegistryProblem {
                            item: p
                                .get("item")
                                .and_then(Json::as_str)
                                .unwrap_or_default()
                                .into(),
                            reason: p
                                .get("reason")
                                .and_then(Json::as_str)
                                .unwrap_or_default()
                                .into(),
                            since: p.get("since").and_then(Json::as_u64).unwrap_or(0),
                        })
                    });
                    Some(CompactRegistryRow {
                        thread: Arc::new(thread),
                        read_at,
                        problem,
                    })
                })
                .collect::<Vec<_>>()
                .into_boxed_slice()
        });
        let as_of = value.get("asOf").and_then(Json::as_u64);
        let error = value.get("error").and_then(Json::as_object).map(|e| {
            Box::new(CompactRegistryError {
                reason: e
                    .get("reason")
                    .and_then(Json::as_str)
                    .unwrap_or_default()
                    .into(),
                since: e.get("since").and_then(Json::as_u64).unwrap_or(0),
            })
        });
        let missing = value.get("missing").and_then(Json::as_bool);
        let problems = value.get("problems").and_then(Json::as_array).map(|list| {
            list.iter()
                .filter_map(|p| {
                    Some(CompactRegistryProblem {
                        item: p.get("item")?.as_str()?.into(),
                        reason: p.get("reason")?.as_str()?.into(),
                        since: p.get("since")?.as_u64()?,
                    })
                })
                .collect::<Vec<_>>()
                .into_boxed_slice()
        });
        let proto_unsafe = data
            .as_deref()
            .is_some_and(|rows| rows.iter().any(CompactRegistryRow::proto_unsafe))
            || problems
                .as_deref()
                .is_some_and(|list| list.iter().any(CompactRegistryProblem::proto_unsafe))
            || error.as_deref().is_some_and(|e| proto_named_str(&e.reason));
        let mut out = Self {
            data,
            as_of,
            error,
            missing,
            problems,
            content_hash: [0u8; 32],
            proto_unsafe,
        };
        out.content_hash = out.compute_hash();
        out
    }

    pub(crate) fn error_reason(&self) -> Option<&str> {
        self.error.as_deref().map(|e| e.reason.as_ref())
    }

    pub(crate) fn has_error(&self) -> bool {
        self.error.is_some()
    }

    /// Materialize the exact JSON wrapper (only for legacy `/internal/status/inputs` or tests).
    pub(crate) fn to_json(&self, as_of_override: Option<u64>) -> Json {
        let mut out = Map::new();
        out.insert(
            "data".to_owned(),
            match &self.data {
                Some(rows) => Json::Array(rows.iter().map(CompactRegistryRow::to_json).collect()),
                None => Json::Null,
            },
        );
        let as_of = as_of_override.or(self.as_of);
        out.insert(
            "asOf".to_owned(),
            as_of.map(Json::from).unwrap_or(Json::Null),
        );
        out.insert(
            "error".to_owned(),
            self.error
                .as_deref()
                .map(CompactRegistryError::to_json)
                .unwrap_or(Json::Null),
        );
        if let Some(missing) = self.missing {
            out.insert("missing".to_owned(), Json::Bool(missing));
        }
        if let Some(problems) = &self.problems {
            out.insert(
                "problems".to_owned(),
                Json::Array(
                    problems
                        .iter()
                        .map(CompactRegistryProblem::to_json)
                        .collect(),
                ),
            );
        }
        Json::Object(out)
    }

    fn compute_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        match &self.data {
            Some(rows) => {
                hasher.update([1u8]);
                hasher.update((rows.len() as u64).to_le_bytes());
                for row in rows {
                    row.thread.update_hasher(&mut hasher);
                    hasher.update(row.read_at.to_le_bytes());
                    hash_opt_problem(&mut hasher, row.problem.as_deref());
                }
            }
            None => hasher.update([0u8]),
        }
        match self.as_of {
            Some(at) => {
                hasher.update([1u8]);
                hasher.update(at.to_le_bytes());
            }
            None => hasher.update([0u8]),
        }
        match self.error.as_deref() {
            Some(err) => {
                hasher.update([1u8]);
                hash_str(&mut hasher, &err.reason);
                hasher.update(err.since.to_le_bytes());
            }
            None => hasher.update([0u8]),
        }
        match self.missing {
            Some(m) => hasher.update([1u8, u8::from(m)]),
            None => hasher.update([0u8]),
        }
        match &self.problems {
            Some(list) => {
                hasher.update([1u8]);
                hasher.update((list.len() as u64).to_le_bytes());
                for p in list {
                    hash_str(&mut hasher, &p.item);
                    hash_str(&mut hasher, &p.reason);
                    hasher.update(p.since.to_le_bytes());
                }
            }
            None => hasher.update([0u8]),
        }
        hasher.finalize().into()
    }
}

fn hash_str(hasher: &mut Sha256, text: &str) {
    hasher.update((text.len() as u64).to_le_bytes());
    hasher.update(text.as_bytes());
}

fn hash_opt_str(hasher: &mut Sha256, text: Option<&str>) {
    match text {
        Some(t) => {
            hasher.update([1u8]);
            hash_str(hasher, t);
        }
        None => hasher.update([0u8]),
    }
}

fn hash_opt_problem(hasher: &mut Sha256, problem: Option<&CompactRegistryProblem>) {
    match problem {
        Some(p) => {
            hasher.update([1u8]);
            hash_str(hasher, &p.item);
            hash_str(hasher, &p.reason);
            hasher.update(p.since.to_le_bytes());
        }
        None => hasher.update([0u8]),
    }
}

fn next_problem(
    before: Option<&CompactRegistryProblem>,
    item: &str,
    reason: &str,
    now: u64,
) -> CompactRegistryProblem {
    CompactRegistryProblem {
        item: item.into(),
        reason: reason.into(),
        since: match before {
            Some(before) if before.reason.as_ref() == reason => before.since,
            _ => now,
        },
    }
}

struct CompactFileEntry {
    stamp: Option<StatStamp>,
    thread: Option<Arc<CompactRegistryThread>>,
    read_at: u64,
    problem: Option<Box<CompactRegistryProblem>>,
}

pub(crate) struct CompactRegistryReader {
    dir: PathBuf,
    files: IndexMap<Box<str>, CompactFileEntry>,
    failing: IndexMap<Box<str>, CompactRegistryProblem>,
    misses: CompactTransientMisses,
}

impl CompactRegistryReader {
    pub(crate) fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            files: IndexMap::new(),
            failing: IndexMap::new(),
            misses: CompactTransientMisses::default(),
        }
    }

    fn fail(&mut self, name: &str, reason: &str, since: u64, changed: &mut bool) {
        if let Some(entry) = self.files.get_mut(name) {
            let problem = next_problem(entry.problem.as_deref(), name, reason, since);
            if entry.problem.as_deref().map(|p| p.reason.as_ref()) != Some(reason) {
                *changed = true;
            }
            entry.problem = Some(Box::new(problem));
            entry.stamp = None;
            return;
        }
        let before = self.failing.get(name).cloned();
        if before.as_ref().map(|p| p.reason.as_ref()) != Some(reason) {
            *changed = true;
        }
        self.failing.insert(
            name.into(),
            next_problem(before.as_ref(), name, reason, since),
        );
    }

    pub(crate) fn scan_changes(
        &mut self,
        fs: &dyn ShareFs,
        now: u64,
    ) -> Result<bool, &'static str> {
        let names = match fs.list(&self.dir) {
            Ok(entries) => entries
                .into_iter()
                .map(|entry| entry.name)
                .collect::<Vec<_>>(),
            Err(error) => {
                return Err(if error.kind() == std::io::ErrorKind::NotFound {
                    "The pm-mail registry folder is missing."
                } else {
                    "The pm-mail registry folder could not be listed."
                });
            }
        };
        let mut changed = false;
        let mut seen: HashSet<Box<str>> = HashSet::new();
        for name in names {
            if name.starts_with('.') || !name.ends_with(".json") {
                continue;
            }
            let file = self.dir.join(&name);
            let stat = match fs.stat(&file) {
                Ok(stat) => stat,
                Err(error) => {
                    seen.insert(name.as_str().into());
                    if error.kind() == std::io::ErrorKind::NotFound {
                        if let Some(since) = self.misses.miss(&name, now) {
                            self.fail(&name, PERSISTENT_SHARE_ERROR, since, &mut changed);
                        }
                        continue;
                    }
                    self.misses.clear(&name);
                    self.fail(&name, "could not be read", now, &mut changed);
                    continue;
                }
            };
            self.misses.clear(&name);
            seen.insert(name.as_str().into());
            if !stat.file {
                self.fail(&name, "is not a regular file", now, &mut changed);
                continue;
            }
            if stat.size > REGISTRY_FILE_CAP as u64 {
                self.fail(&name, "is larger than the 64 KB cap", now, &mut changed);
                continue;
            }
            let stamp = StatStamp::from_stat(&stat);
            if let Some(entry) = self.files.get(name.as_str())
                && entry.stamp == Some(stamp)
                && entry.problem.is_none()
            {
                continue;
            }
            let bytes = match fs.read(&file, REGISTRY_FILE_CAP) {
                Ok(bytes) => bytes,
                Err(ReadError::OverCap) => {
                    self.fail(&name, "is not valid JSON", now, &mut changed);
                    continue;
                }
                Err(ReadError::Io(_)) => {
                    self.fail(&name, "could not be read", now, &mut changed);
                    continue;
                }
            };
            let text = String::from_utf8_lossy(&bytes);
            let raw: Json = match serde_json::from_str(&text) {
                Ok(raw) => raw,
                Err(_) => {
                    self.fail(&name, "is not valid JSON", now, &mut changed);
                    continue;
                }
            };
            let thread = parse_registry_entry(&raw)
                .and_then(|parsed| CompactRegistryThread::from_parsed_json(&parsed));
            let before = self.files.get(name.as_str());
            let same_thread = before.is_some_and(|entry| match (&entry.thread, &thread) {
                (None, None) => true,
                (Some(a), Some(b)) => a.as_ref() == b,
                _ => false,
            });
            if before.is_none()
                || before.is_some_and(|entry| entry.problem.is_some())
                || !same_thread
            {
                changed = true;
            }
            let shared_thread = if same_thread {
                before.and_then(|entry| entry.thread.clone())
            } else {
                thread.map(Arc::new)
            };
            self.files.insert(
                name.as_str().into(),
                CompactFileEntry {
                    stamp: Some(stamp),
                    thread: shared_thread,
                    read_at: now,
                    problem: None,
                },
            );
            self.failing.shift_remove(name.as_str());
        }
        self.misses.keep_only(&seen);
        let gone: Vec<Box<str>> = self
            .files
            .keys()
            .filter(|name| !seen.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            self.files.shift_remove(&name);
            changed = true;
        }
        let gone: Vec<Box<str>> = self
            .failing
            .keys()
            .filter(|name| !seen.contains(*name))
            .cloned()
            .collect();
        for name in gone {
            self.failing.shift_remove(&name);
            changed = true;
        }
        Ok(changed)
    }

    /// Snapshot sharing `Arc<CompactRegistryThread>` with the reader (zero string copies).
    pub(crate) fn snapshot(&self, now: u64) -> CompactRegistrySnapshot {
        let rows: Vec<CompactRegistryRow> = self
            .files
            .values()
            .filter_map(|entry| {
                let thread = entry.thread.clone()?;
                Some(CompactRegistryRow {
                    thread,
                    read_at: entry.read_at,
                    problem: entry.problem.clone(),
                })
            })
            .collect();
        let mut problems: Vec<CompactRegistryProblem> = self
            .files
            .values()
            .filter_map(|entry| entry.problem.as_deref().cloned())
            .chain(self.failing.values().cloned())
            .collect();
        problems.sort_by(|a, b| compare(&a.item, &b.item));
        CompactRegistrySnapshot::ok(rows, problems, now)
    }
}
