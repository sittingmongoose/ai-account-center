//! Compact Scoreboard catalog reader (`v1/catalog/*.json`).
//!
//! Replaces retained `serde_json::Value` DOM trees in `CatalogReader` with compact JSON
//! byte buffers, inline `[u8; 32]` SHA-256 digests, numeric `StatStamp`, and change tracking
//! so `ScoreboardOwned::catalog_pass` only rebuilds `catalog_state` when the catalog changes
//! (instead of rebuilding every 30 s tick).

use super::primitives::{CompactProblem, CompactSha, CompactTransientMisses, StatStamp};
use aac_status::share::catalog::{
    CATALOG_CAP_BYTES, CATALOG_FILES, DEFAULT_MIN_RUNS, parse_catalog_file,
};
use aac_status::share::{
    MISSING_GRACE_MS, PERSISTENT_SHARE_ERROR, ReadError, ShareFs, compare, is_missing,
    is_transient, js_text,
};
use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};
use std::collections::HashSet;
use std::path::PathBuf;

struct CompactCatalogEntry {
    stamp: Option<StatStamp>,
    /// Compact `js_text` UTF-8 bytes of the projected catalog file + digest.
    good: Option<(Box<[u8]>, u64, CompactSha)>,
    problem: Option<CompactProblem>,
    missing_since: Option<u64>,
}

pub(crate) struct CompactCatalogReader {
    root: PathBuf,
    entries: IndexMap<Box<str>, CompactCatalogEntry>,
    misses: CompactTransientMisses,
    state: &'static str,
    scanned_at: Option<u64>,
    failed_since: Option<u64>,
}

impl CompactCatalogReader {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            entries: IndexMap::new(),
            misses: CompactTransientMisses::default(),
            state: "missing",
            scanned_at: None,
            failed_since: None,
        }
    }

    fn entry(&mut self, file: &str) -> &mut CompactCatalogEntry {
        if !self.entries.contains_key(file) {
            self.entries.insert(
                file.into(),
                CompactCatalogEntry {
                    stamp: None,
                    good: None,
                    problem: None,
                    missing_since: None,
                },
            );
        }
        self.entries.get_mut(file).expect("inserted entry")
    }

    fn set_problem(
        &mut self,
        file: &str,
        reason: &str,
        stamp: Option<StatStamp>,
        since: u64,
    ) -> bool {
        let entry = self.entry(file);
        let changed = entry.problem.as_ref().map(|p| p.reason.as_ref()) != Some(reason);
        if changed {
            entry.problem = Some(CompactProblem::new(file, reason, since));
        }
        entry.stamp = stamp;
        changed
    }

    fn transient(&mut self, file: &str, now: u64) -> bool {
        if let Some(since) = self.misses.miss(file, now) {
            return self.set_problem(file, PERSISTENT_SHARE_ERROR, None, since);
        }
        false
    }

    fn visit(
        &mut self,
        fs: &dyn ShareFs,
        absolute: &std::path::Path,
        name: &str,
        file: &str,
        seen: &mut HashSet<Box<str>>,
        now: u64,
    ) -> bool {
        let stat = match fs.stat(absolute) {
            Ok(stat) => {
                seen.insert(file.into());
                stat
            }
            Err(error) => {
                seen.insert(file.into());
                return if is_transient(&error) {
                    self.transient(file, now)
                } else {
                    self.misses.clear(file);
                    self.set_problem(file, "could not be read", None, now)
                };
            }
        };
        if !stat.file {
            return self.set_problem(file, "is not a regular file", None, now);
        }
        let stamp = StatStamp::from_stat(&stat);
        if stat.size > CATALOG_CAP_BYTES as u64 {
            return self.set_problem(
                file,
                &format!("larger than the {} KB cap", CATALOG_CAP_BYTES / 1024),
                Some(stamp),
                now,
            );
        }
        if self
            .entries
            .get(file)
            .is_some_and(|entry| entry.stamp == Some(stamp))
        {
            self.misses.clear(file);
            return false;
        }
        let bytes = match fs.read(absolute, CATALOG_CAP_BYTES) {
            Ok(bytes) => bytes,
            Err(ReadError::OverCap) => {
                return self.set_problem(
                    file,
                    &format!("larger than the {} KB cap", CATALOG_CAP_BYTES / 1024),
                    Some(stamp),
                    now,
                );
            }
            Err(ReadError::Io(error)) => {
                return if is_transient(&error) {
                    self.transient(file, now)
                } else {
                    self.misses.clear(file);
                    self.set_problem(file, "could not be read", None, now)
                };
            }
        };
        self.misses.clear(file);
        let doc = match parse_catalog_file(name, &bytes) {
            Ok(doc) => doc,
            Err(reason) => {
                return self.set_problem(file, &reason, Some(stamp), now);
            }
        };
        let encoded: Box<[u8]> = js_text(&doc).into_bytes().into_boxed_slice();
        let sha = CompactSha::digest_bytes(&bytes);
        let entry = self.entry(file);
        let changed = entry.problem.is_some()
            || entry
                .good
                .as_ref()
                .is_none_or(|(old_bytes, _, old_sha)| *old_sha != sha || *old_bytes != encoded);
        entry.stamp = Some(stamp);
        entry.problem = None;
        entry.good = Some((encoded, now, sha));
        changed
    }

    fn keep(&self, prefix: &str, seen: &mut HashSet<Box<str>>) {
        for file in self.entries.keys() {
            if file.starts_with(prefix) {
                seen.insert(file.clone());
            }
        }
    }

    fn sweep(&mut self, seen: &HashSet<Box<str>>, now: u64) -> bool {
        self.misses.keep_only(seen);
        let mut changed = false;
        let keys: Vec<Box<str>> = self.entries.keys().cloned().collect();
        for file in keys {
            let entry = self.entries.get_mut(&file).expect("listed entry");
            if seen.contains(&file) {
                entry.missing_since = None;
                continue;
            }
            let missing_since = *entry.missing_since.get_or_insert(now);
            if now.saturating_sub(missing_since) >= MISSING_GRACE_MS {
                self.entries.shift_remove(&file);
                changed = true;
            }
        }
        changed
    }

    /// One rescan. Returns whether content/state/problems changed (beyond `scanned_at`).
    pub(crate) fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> bool {
        let dir = self.root.join("v1").join("catalog");
        let mut seen: HashSet<Box<str>> = HashSet::new();
        let mut changed = false;
        let listed: HashSet<String> = match fs.list(&dir) {
            Ok(entries) => {
                if self.state != "ok" || self.failed_since.is_some() {
                    changed = true;
                }
                self.state = "ok";
                self.failed_since = None;
                entries.into_iter().map(|entry| entry.name).collect()
            }
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound && is_transient(&error) {
                    return false;
                }
                let next = if is_missing(&error) {
                    "missing"
                } else {
                    "error"
                };
                if self.state != next {
                    changed = true;
                }
                self.state = next;
                if self.state == "error" {
                    if self.failed_since.is_none() {
                        changed = true;
                    }
                    self.failed_since.get_or_insert(now);
                    self.keep("catalog/", &mut seen);
                } else {
                    if self.failed_since.is_some() {
                        changed = true;
                    }
                    self.failed_since = None;
                }
                changed |= self.sweep(&seen, now);
                self.scanned_at = Some(now);
                return changed;
            }
        };
        for name in CATALOG_FILES {
            if listed.contains(name) {
                changed |= self.visit(
                    fs,
                    &dir.join(name),
                    name,
                    &format!("catalog/{name}"),
                    &mut seen,
                    now,
                );
            }
        }
        changed |= self.sweep(&seen, now);
        self.scanned_at = Some(now);
        changed
    }

    pub(crate) fn scanned_at(&self) -> Option<u64> {
        self.scanned_at
    }

    /// Materialize the exact `aac_status::share::catalog::state_json` output.
    pub(crate) fn snapshot_json(&self) -> Json {
        let parsed = |name: &str, kind: &str| -> Option<Json> {
            let (bytes, _, _) = self
                .entries
                .get(format!("catalog/{name}").as_str())?
                .good
                .as_ref()?;
            let doc: Json = serde_json::from_slice(bytes).ok()?;
            (doc["kind"] == json!(kind)).then_some(doc)
        };
        let mut problems: Vec<&CompactProblem> = self
            .entries
            .values()
            .filter_map(|entry| entry.problem.as_ref())
            .collect();
        problems.sort_by(|a, b| compare(&a.file, &b.file));
        let named: HashSet<&str> = problems.iter().map(|p| p.file.as_ref()).collect();
        let measures_file = parsed("measures.json", "measures");
        let aliases_file = parsed("model-aliases.json", "aliases");
        let array = |doc: Option<Json>, key: &str| -> Vec<Json> {
            doc.and_then(|mut doc| {
                doc.get_mut(key)
                    .and_then(Json::as_array_mut)
                    .map(std::mem::take)
            })
            .unwrap_or_default()
        };
        let missing_files: Vec<String> = CATALOG_FILES
            .iter()
            .filter(|name| {
                let rel = format!("catalog/{name}");
                self.entries
                    .get(rel.as_str())
                    .and_then(|e| e.good.as_ref())
                    .is_none()
                    && !named.contains(rel.as_str())
            })
            .map(|name| (*name).to_owned())
            .collect();
        let min_runs = measures_file
            .as_ref()
            .and_then(|doc| doc["minRuns"].as_u64())
            .unwrap_or(DEFAULT_MIN_RUNS);
        let aliases = aliases_file
            .as_ref()
            .and_then(|doc| doc["aliases"].as_object())
            .cloned()
            .unwrap_or_else(Map::new);
        let effort_suffixes: Vec<String> = aliases_file
            .as_ref()
            .and_then(|doc| doc["effortSuffixes"].as_array())
            .map(|suffixes| {
                suffixes
                    .iter()
                    .filter_map(Json::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        let mut out = json!({
            "state": self.state,
            "scannedAt": self.scanned_at,
            "missingFiles": missing_files,
            "problems": problems.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
            "minRuns": min_runs,
            "models": array(parsed("models.json", "models"), "models"),
            "aliases": Json::Object(aliases),
            "effortSuffixes": effort_suffixes,
            "taskTypes": array(parsed("task-types.json", "taskTypes"), "taskTypes"),
            "measures": array(measures_file, "measures"),
            "uses": array(parsed("uses.json", "uses"), "uses"),
            "benchmarks": array(parsed("benchmarks.json", "benchmarks"), "benchmarks"),
        });
        if let Some(since) = self.failed_since {
            out["failedSince"] = json!(since);
        }
        out
    }
}
