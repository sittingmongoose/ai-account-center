//! The agent reports reader (`agent-reports-reader.ts`): `<reports>/v1/writers/<writer>/
//! {thread.json,tasks.json,cards/*.json}`, scanned under the share rules: list first, stat
//! then read only what changed, the last good copy with a named problem, `seq` ordering,
//! the two-scan transient rule, the removal grace, and the scan's writer/card/byte limits.

use super::{
    MISSING_GRACE_MS, PERSISTENT_LISTING_ERROR, PERSISTENT_SHARE_ERROR, Problem, SEQ_WENT_BACK, ShareFs,
    TransientMisses, compare, folder_row_problem, is_missing, is_transient, sha256_hex, share_local_path, showable,
    stat_key,
};
use crate::validate::is_writer_name;
use indexmap::IndexMap;
use serde_json::{Map, Value as Json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What one scan reads, so the share cannot grow the server without bound.
#[derive(Clone, Copy)]
pub struct Limits {
    pub writers: usize,
    pub cards: usize,
    pub bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            writers: 1000,
            cards: 500,
            bytes: 64 * 1024 * 1024,
        }
    }
}

/// One accepted revision: the projected document, its digest, and when AAC first saw it.
#[derive(Clone, Debug)]
pub struct SeenReport {
    pub file: String,
    pub writer: String,
    pub doc: Json,
    pub sha256: String,
    pub first_seen_at: u64,
    pub written_at_ms: i64,
    pub problem: Option<Problem>,
}

#[derive(Clone, Debug)]
struct Entry {
    kind: &'static str,
    writer: String,
    stat_key: String,
    good: Option<SeenReport>,
    problem: Option<Problem>,
    missing_since: Option<u64>,
}

/// The reader's snapshot: what the Status view's `reports` member holds.
#[derive(Clone, Debug)]
pub struct AgentReportsState {
    pub root: String,
    pub state: &'static str,
    pub scanned_at: Option<u64>,
    pub complete: bool,
    pub threads: Vec<SeenReport>,
    pub tasks: Vec<SeenReport>,
    pub cards: Vec<SeenReport>,
    pub problems: Vec<Problem>,
    /// Board files by the path a writer gave: modification time, or null.
    pub boards: Map<String, Json>,
}

/// At most `limit` names: all of them when they fit, else those whose folder changed most
/// recently (never read, 0, last), the tie in `localeCompare` order.
fn newest_folders(names: Vec<String>, limit: usize, fs: &dyn ShareFs, dir: &Path) -> Vec<String> {
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

pub struct AgentReportsReader {
    root: PathBuf,
    limits: Limits,
    entries: IndexMap<String, Entry>,
    folder_problems: IndexMap<String, Problem>,
    boards: Map<String, Json>,
    state: &'static str,
    scanned_at: Option<u64>,
    complete: bool,
    scan_partial: bool,
    scan_bytes: u64,
    over_bytes: u64,
    misses: TransientMisses,
    listing_misses: TransientMisses,
}

impl AgentReportsReader {
    pub fn new(root: PathBuf, limits: Limits) -> Self {
        Self {
            root,
            limits,
            entries: IndexMap::new(),
            folder_problems: IndexMap::new(),
            boards: Map::new(),
            state: "missing",
            scanned_at: None,
            complete: false,
            scan_partial: false,
            scan_bytes: 0,
            over_bytes: 0,
            misses: TransientMisses::default(),
            listing_misses: TransientMisses::default(),
        }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn set_problem(
        &mut self,
        rel: &str,
        kind: &'static str,
        writer: &str,
        reason: &str,
        stat_key: &str,
        since: u64,
    ) -> bool {
        let mut entry = self.entries.get(rel).cloned().unwrap_or(Entry {
            kind,
            writer: writer.to_string(),
            stat_key: String::new(),
            good: None,
            problem: None,
            missing_since: None,
        });
        let changed = entry.problem.as_ref().map(|p| p.reason.as_str()) != Some(reason);
        if changed {
            entry.problem = Some(Problem {
                file: rel.to_string(),
                reason: reason.to_string(),
                since,
            });
        }
        // A read error keeps the key empty so the next scan tries again.
        entry.stat_key = stat_key.to_string();
        if let Some(good) = &mut entry.good {
            good.problem = entry.problem.clone();
        }
        self.entries.insert(rel.to_string(), entry);
        changed
    }

    fn transient(&mut self, rel: &str, kind: &'static str, writer: &str, now: u64) -> bool {
        match self.misses.miss(rel, now) {
            None => false,
            Some(since) => self.set_problem(rel, kind, writer, PERSISTENT_SHARE_ERROR, "", since),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the scan preserves document and file provenance"
    )]
    fn accept(
        &mut self,
        rel: &str,
        kind: &'static str,
        writer: &str,
        doc: Json,
        sha256: String,
        stat_key: &str,
        now: u64,
    ) -> bool {
        let seq = doc["seq"].as_u64().unwrap_or(0);
        let written_at = doc["writtenAt"].as_str().unwrap_or_default();
        let mut entry = self.entries.get(rel).cloned().unwrap_or(Entry {
            kind,
            writer: writer.to_string(),
            stat_key: stat_key.to_string(),
            good: None,
            problem: None,
            missing_since: None,
        });
        if let Some(good) = &entry.good
            && seq < good.doc["seq"].as_u64().unwrap_or(0)
        {
            return self.set_problem(rel, kind, writer, SEQ_WENT_BACK, stat_key, now);
        }
        let had_problem = entry.problem.is_some();
        entry.stat_key = stat_key.to_string();
        entry.problem = None;
        if let Some(good) = &mut entry.good
            && good.sha256 == sha256
        {
            good.problem = None;
            self.entries.insert(rel.to_string(), entry);
            return had_problem;
        }
        entry.good = Some(SeenReport {
            file: rel.to_string(),
            writer: writer.to_string(),
            written_at_ms: aac_num::ecma::parse_instant(written_at).unwrap_or(0),
            doc,
            sha256,
            first_seen_at: now,
            problem: None,
        });
        self.entries.insert(rel.to_string(), entry);
        true
    }

    /// Stat and, when changed, read one listed file. Resolves whether it changed the view.
    #[expect(
        clippy::too_many_arguments,
        reason = "the scan retains bounded read context for each document"
    )]
    fn visit(
        &mut self,
        fs: &dyn ShareFs,
        file: &Path,
        rel: &str,
        kind: &'static str,
        writer: &str,
        card_id: &str,
        seen: &mut HashSet<String>,
        now: u64,
    ) -> bool {
        let cap = match kind {
            "thread" => super::report::THREAD_CAP,
            "tasks" => super::report::TASKS_CAP,
            _ => super::report::CARD_CAP,
        };
        let stat = match fs.stat(file) {
            Ok(stat) => stat,
            Err(error) => {
                seen.insert(rel.to_string());
                self.scan_partial = true;
                if is_transient(&error) {
                    return self.transient(rel, kind, writer, now);
                }
                self.misses.clear(rel);
                return self.set_problem(rel, kind, writer, "could not be read", "", now);
            }
        };
        if !stat.file {
            seen.insert(rel.to_string());
            return self.set_problem(rel, kind, writer, "is not a regular file", "", now);
        }
        // A file over its own cap is never read or held, so it takes nothing from the budget.
        if stat.size <= cap as u64 {
            if self.scan_bytes + stat.size > self.limits.bytes {
                self.over_bytes += 1;
                return false;
            }
            self.scan_bytes += stat.size;
        }
        seen.insert(rel.to_string());
        let key = stat_key(&stat);
        if let Some(entry) = self.entries.get(rel)
            && entry.stat_key == key
        {
            self.misses.clear(rel);
            return false;
        }
        if stat.size > cap as u64 {
            return self.set_problem(
                rel,
                kind,
                writer,
                &format!("larger than the {} KB cap", cap / 1024),
                &key,
                now,
            );
        }
        let bytes = match fs.read(file, cap) {
            Ok(bytes) => bytes,
            Err(super::ReadError::OverCap) => {
                // The file grew past its cap after the stat: the reader's own cap refusal.
                return self.set_problem(
                    rel,
                    kind,
                    writer,
                    &format!("larger than the {} KB cap", cap / 1024),
                    &key,
                    now,
                );
            }
            Err(super::ReadError::Io(error)) => {
                self.scan_partial = true;
                if is_transient(&error) {
                    return self.transient(rel, kind, writer, now);
                }
                self.misses.clear(rel);
                return self.set_problem(rel, kind, writer, "could not be read", "", now);
            }
        };
        self.misses.clear(rel);
        match super::report::parse_report_file(kind, &bytes, writer, card_id) {
            Err(reason) => self.set_problem(rel, kind, writer, &reason, &key, now),
            Ok((doc, sha256)) => self.accept(rel, kind, writer, doc, sha256, &key, now),
        }
    }

    /// A writer or `cards/` folder that could not be listed: not listed is not withdrawn;
    /// its known files are kept, and a lasting failure is named on the folder and its rows.
    #[expect(
        clippy::too_many_arguments,
        reason = "the scan distinguishes failure scopes and transient retries"
    )]
    fn listing_failed(
        &mut self,
        name: &str,
        folder: &str,
        error: &std::io::Error,
        seen: &mut HashSet<String>,
        failed_listings: &mut HashSet<String>,
        now: u64,
        folder_problems: &mut IndexMap<String, Problem>,
        before: &IndexMap<String, Problem>,
    ) -> bool {
        self.scan_partial = true;
        let rel = if folder.is_empty() {
            format!("writers/{name}")
        } else {
            format!("writers/{name}/cards")
        };
        let mut reason = Some("folder could not be listed".to_string());
        let mut since = now;
        if is_transient(error) {
            failed_listings.insert(rel.clone());
            match self.listing_misses.miss(&rel, since) {
                None => reason = None,
                Some(first) => {
                    reason = Some(PERSISTENT_LISTING_ERROR.to_string());
                    since = first;
                }
            }
        }
        if let Some(reason) = &reason {
            put_folder_problem(folder_problems, before, &rel, reason, since);
        }
        let prefix = format!("writers/{name}/{folder}");
        let keys: Vec<String> = self
            .entries
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        let mut changed = false;
        for key in keys {
            seen.insert(key.clone());
            if let Some(reason) = &reason {
                let entry = self.entries.get(&key).cloned().expect("listed entry");
                if self.set_problem(&key, entry.kind, &entry.writer, &folder_row_problem(reason), "", since) {
                    changed = true;
                }
            }
        }
        changed
    }

    /// One rescan; resolves whether anything the view shows changed.
    pub fn scan(&mut self, fs: &dyn ShareFs, now: u64) -> bool {
        let writers_dir = self.root.join("v1").join("writers");
        let mut changed = false;
        self.scan_partial = false;
        self.scan_bytes = 0;
        self.over_bytes = 0;
        let writers = match fs.list(&writers_dir) {
            Ok(writers) => writers,
            Err(error) => {
                self.complete = false;
                // A stale handle on the share: keep everything and try again next scan.
                if error.kind() != std::io::ErrorKind::NotFound && is_transient(&error) {
                    return false;
                }
                let next = if is_missing(&error) { "missing" } else { "error" };
                changed = self.state != next;
                self.state = next;
                self.scanned_at = Some(now);
                return changed;
            }
        };
        if self.state != "ok" {
            changed = true;
        }
        self.state = "ok";
        let mut seen = HashSet::new();
        let mut folder_problems = IndexMap::new();
        let before = self.folder_problems.clone();
        let mut failed_listings = HashSet::new();
        let mut names = Vec::new();
        for dirent in &writers {
            let name = &dirent.name;
            if name.starts_with('.') || !dirent.is_dir {
                continue;
            }
            if !is_writer_name(name) {
                if showable(name) {
                    put_folder_problem(
                        &mut folder_problems,
                        &before,
                        &format!("writers/{name}"),
                        "folder name is not a writer name",
                        now,
                    );
                }
                continue;
            }
            names.push(name.clone());
        }
        let kept = newest_folders(names.clone(), self.limits.writers, fs, &writers_dir);
        if kept.len() < names.len() {
            self.scan_partial = true;
            put_folder_problem(
                &mut folder_problems,
                &before,
                "limits/writers",
                &format!(
                    "{} active writers past the limit of {} were not read; the most recently changed are kept",
                    names.len() - kept.len(),
                    self.limits.writers
                ),
                now,
            );
        }
        let mut cards_visited = 0usize;
        let mut cards_over = 0usize;
        for name in &kept {
            let base = writers_dir.join(name);
            // List the folder first, then read: a listing refreshes the NFS view of a rename-over.
            let listed: HashSet<String> = match fs.list(&base) {
                Ok(entries) => entries.into_iter().map(|entry| entry.name).collect(),
                Err(error) => {
                    if self.listing_failed(
                        name,
                        "",
                        &error,
                        &mut seen,
                        &mut failed_listings,
                        now,
                        &mut folder_problems,
                        &before,
                    ) {
                        changed = true;
                    }
                    continue;
                }
            };
            for (file, kind) in [("thread.json", "thread"), ("tasks.json", "tasks")] {
                if !listed.contains(file) {
                    continue;
                }
                let rel = format!("writers/{name}/{file}");
                if self.visit(fs, &base.join(file), &rel, kind, name, "", &mut seen, now) {
                    changed = true;
                }
            }
            if !listed.contains("cards") {
                continue;
            }
            let mut cards = match fs.list(&base.join("cards")) {
                Ok(cards) => cards,
                Err(error) => {
                    // A listing failure is not a withdrawal: keep the cards already known.
                    if self.listing_failed(
                        name,
                        "cards/",
                        &error,
                        &mut seen,
                        &mut failed_listings,
                        now,
                        &mut folder_problems,
                        &before,
                    ) {
                        changed = true;
                    }
                    Vec::new()
                }
            };
            cards.sort_by(|a, b| compare(&a.name, &b.name));
            for card in cards {
                if card.name.starts_with('.') || !card.name.ends_with(".json") {
                    continue;
                }
                let id = &card.name[..card.name.len() - ".json".len()];
                let rel = format!("writers/{name}/cards/{}", card.name);
                if !showable(&card.name) {
                    continue;
                }
                if !card.is_file {
                    // A FIFO or a link where a card belongs: it may stand for an open card.
                    self.scan_partial = true;
                    if self.entries.contains_key(&rel) {
                        seen.insert(rel.clone());
                        self.misses.clear(&rel);
                        if self.set_problem(&rel, "card", name, "is not a regular file", "", now) {
                            changed = true;
                        }
                    } else {
                        put_folder_problem(&mut folder_problems, &before, &rel, "is not a regular file", now);
                    }
                    continue;
                }
                if cards_visited >= self.limits.cards {
                    cards_over += 1;
                    continue;
                }
                cards_visited += 1;
                if self.visit(
                    fs,
                    &base.join("cards").join(&card.name),
                    &rel,
                    "card",
                    name,
                    id,
                    &mut seen,
                    now,
                ) {
                    changed = true;
                }
            }
        }
        if cards_over > 0 {
            self.scan_partial = true;
            put_folder_problem(
                &mut folder_problems,
                &before,
                "limits/cards",
                &format!(
                    "{cards_over} card files past the limit of {} were not read",
                    self.limits.cards
                ),
                now,
            );
        }
        if self.over_bytes > 0 {
            self.scan_partial = true;
            put_folder_problem(
                &mut folder_problems,
                &before,
                "limits/bytes",
                &format!(
                    "{} files past the limit of {} MB were not read",
                    self.over_bytes,
                    self.limits.bytes / (1024 * 1024)
                ),
                now,
            );
        }
        let keys: Vec<String> = self.entries.keys().cloned().collect();
        for key in keys {
            let entry = self.entries.get_mut(&key).expect("listed entry");
            if seen.contains(&key) {
                entry.missing_since = None;
                continue;
            }
            let missing_since = *entry.missing_since.get_or_insert(now);
            if now.saturating_sub(missing_since) >= MISSING_GRACE_MS {
                self.entries.shift_remove(&key);
                changed = true;
            }
        }
        self.misses.keep_only(&seen);
        self.listing_misses.keep_only(&failed_listings);
        if !same_problems(&self.folder_problems, &folder_problems) {
            changed = true;
        }
        self.folder_problems = folder_problems;
        if self.stat_boards(fs) {
            changed = true;
        }
        self.scanned_at = Some(now);
        // A card refused for its content with no earlier good copy is absent from the list
        // though it may be open: the list then proves nothing about closed cards.
        let unseen_card = self
            .entries
            .values()
            .any(|entry| entry.kind == "card" && entry.problem.is_some() && entry.good.is_none());
        self.complete = !self.scan_partial && !unseen_card;
        changed
    }

    fn collect(&self) -> (Vec<SeenReport>, Vec<SeenReport>, Vec<SeenReport>) {
        let mut threads = Vec::new();
        let mut tasks = Vec::new();
        let mut cards = Vec::new();
        for entry in self.entries.values() {
            let Some(good) = &entry.good else { continue };
            match entry.kind {
                "thread" => threads.push(good.clone()),
                "tasks" => tasks.push(good.clone()),
                _ => cards.push(good.clone()),
            }
        }
        (threads, tasks, cards)
    }

    fn stat_boards(&mut self, fs: &dyn ShareFs) -> bool {
        let (threads, tasks, _) = self.collect();
        let mut paths: Vec<String> = Vec::new();
        let mut push = |path: &str| {
            if !paths.iter().any(|held| held == path) {
                paths.push(path.to_string());
            }
        };
        for row in &tasks {
            if let Some(board) = row.doc["board"].as_str() {
                push(board);
            }
        }
        for row in &threads {
            if let Some(links) = row.doc["links"].as_array() {
                for link in links {
                    if link["label"]
                        .as_str()
                        .is_some_and(|label| label.to_ascii_lowercase().contains("board"))
                        && let Some(path) = link["path"].as_str()
                    {
                        push(path);
                    }
                }
            }
        }
        let mut next = Map::new();
        for given in paths {
            let local = share_local_path(&given, &self.root);
            let mtime = local.and_then(|local| fs.stat(&local).ok().filter(|stat| stat.file).map(|stat| stat.mtime_ms));
            next.insert(given, mtime.map(Json::from).unwrap_or(Json::Null));
        }
        let changed =
            next.len() != self.boards.len() || next.iter().any(|(key, value)| self.boards.get(key) != Some(value));
        self.boards = next;
        changed
    }

    pub fn snapshot(&self) -> AgentReportsState {
        let mut problems: Vec<Problem> = self
            .entries
            .values()
            .filter_map(|entry| entry.problem.clone())
            .chain(self.folder_problems.values().cloned())
            .collect();
        problems.sort_by(|a, b| compare(&a.file, &b.file));
        let (threads, tasks, cards) = self.collect();
        AgentReportsState {
            root: self.root.to_string_lossy().into_owned(),
            state: self.state,
            scanned_at: self.scanned_at,
            complete: self.complete,
            threads,
            tasks,
            cards,
            problems,
            boards: self.boards.clone(),
        }
    }

    /// The card as it is on disk now (for an answer): read fresh, never from the last scan.
    /// None when the share says the card is not there, the file does not pass the reader's
    /// rules, or it is older than a seq this reader has already read. An error when the
    /// share cannot say: that is not the card changing.
    pub fn read_card_now(&self, fs: &dyn ShareFs, card_id: &str) -> Result<Option<(Json, String)>, std::io::Error> {
        // `cardId.slice(0, cardId.indexOf('--'))`: without a `--`, all but the last character.
        let at = card_id.find("--").unwrap_or_else(|| card_id.len().saturating_sub(1));
        let writer = &card_id[..at];
        if !is_writer_name(writer) {
            return Ok(None);
        }
        let folder = self.root.join("v1").join("writers").join(writer).join("cards");
        let file = folder.join(format!("{card_id}.json"));
        let listed = match fs.list(&folder) {
            Ok(listed) => listed,
            Err(error) => {
                if !is_missing(&error) {
                    return Err(error);
                }
                // No cards/ under a share that is there means no such card; an unmounted
                // root looks the same, so ask v1 (this stat rejects if the share is gone).
                fs.stat(&self.root.join("v1"))?;
                return Ok(None);
            }
        };
        if !listed.iter().any(|entry| entry.name == format!("{card_id}.json")) {
            return Ok(None);
        }
        let stat = fs.stat(&file)?;
        if !stat.file || stat.size > super::report::CARD_CAP as u64 {
            return Ok(None);
        }
        let bytes = match fs.read(&file, super::report::CARD_CAP) {
            Ok(bytes) => bytes,
            Err(super::ReadError::OverCap) => return Ok(None),
            Err(super::ReadError::Io(error)) => return Err(error),
        };
        let Ok((doc, sha256)) = super::report::parse_report_file("card", &bytes, writer, card_id) else {
            return Ok(None);
        };
        let held = self
            .entries
            .get(&format!("writers/{writer}/cards/{card_id}.json"))
            .and_then(|entry| entry.good.as_ref());
        if let Some(held) = held
            && doc["seq"].as_u64().unwrap_or(0) < held.doc["seq"].as_u64().unwrap_or(0)
        {
            return Ok(None);
        }
        Ok(Some((doc, sha256)))
    }
}

fn same_problems(before: &IndexMap<String, Problem>, after: &IndexMap<String, Problem>) -> bool {
    before.len() == after.len()
        && after
            .iter()
            .all(|(key, value)| before.get(key).map(|p| p.reason.as_str()) == Some(value.reason.as_str()))
}

/// One scan's folder problem: the prior scan's `since` stays while the reason does.
fn put_folder_problem(
    folder_problems: &mut IndexMap<String, Problem>,
    before: &IndexMap<String, Problem>,
    file: &str,
    reason: &str,
    since: u64,
) {
    let since = match before.get(file) {
        Some(prior) if prior.reason == reason => prior.since,
        _ => since,
    };
    folder_problems.insert(
        file.to_string(),
        Problem {
            file: file.to_string(),
            reason: reason.to_string(),
            since,
        },
    );
}

/// The snapshot as the Status envelope's `reports` member: Maps become objects, in order.
pub fn state_json(state: &AgentReportsState) -> Json {
    let seen = |rows: &[SeenReport]| {
        Json::Array(
            rows.iter()
                .map(|row| {
                    let mut out = Map::new();
                    out.insert("file".to_string(), Json::from(row.file.clone()));
                    out.insert("writer".to_string(), Json::from(row.writer.clone()));
                    out.insert("doc".to_string(), row.doc.clone());
                    out.insert("sha256".to_string(), Json::from(row.sha256.clone()));
                    out.insert("firstSeenAt".to_string(), Json::from(row.first_seen_at));
                    out.insert("writtenAtMs".to_string(), Json::from(row.written_at_ms));
                    out.insert("problem".to_string(), problem_json(row.problem.as_ref()));
                    Json::Object(out)
                })
                .collect(),
        )
    };
    let mut out = Map::new();
    out.insert("root".to_string(), Json::from(state.root.clone()));
    out.insert("state".to_string(), Json::from(state.state));
    out.insert(
        "scannedAt".to_string(),
        state.scanned_at.map(Json::from).unwrap_or(Json::Null),
    );
    out.insert("complete".to_string(), Json::from(state.complete));
    out.insert("threads".to_string(), seen(&state.threads));
    out.insert("tasks".to_string(), seen(&state.tasks));
    out.insert("cards".to_string(), seen(&state.cards));
    out.insert(
        "problems".to_string(),
        Json::Array(state.problems.iter().map(problem_json_ref).collect()),
    );
    out.insert("boards".to_string(), Json::Object(state.boards.clone()));
    Json::Object(out)
}

fn problem_json(problem: Option<&Problem>) -> Json {
    match problem {
        None => Json::Null,
        Some(problem) => problem_json_ref(problem),
    }
}

fn problem_json_ref(problem: &Problem) -> Json {
    serde_json::json!({
        "file": problem.file,
        "reason": problem.reason,
        "since": problem.since,
    })
}

/// The digest helper the readers share (re-exported for the check writer).
pub fn digest(bytes: &[u8]) -> String {
    sha256_hex(bytes)
}
