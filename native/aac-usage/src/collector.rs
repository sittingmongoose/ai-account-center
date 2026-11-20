//! One scan's state: fingerprints read and confirmed, hourly rows, session rows, caps and the
//! deadline. The Python helper's `Collector`, field for field.

use crate::util::{self, Print, ReadProgress};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub const MAX_FILES: usize = 20_000;
pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ROWS: usize = 50_000;
pub const MAX_SROWS: usize = 50_000;
pub const MAX_CLAUDE_KEYS: usize = 100_000;
pub const MAX_MESSAGES: usize = 20_000;

/// Four token kinds in the helper's order: input, output, cache read, cache write.
pub type Tokens = [u64; 4];

#[derive(Clone, Debug)]
pub struct Row {
    pub kind: String,
    pub filekey: String,
    pub model: String,
    pub provider: Option<String>,
    pub hour: String,
    pub tokens: Tokens,
    pub cost: f64,
    pub events: u64,
}

#[derive(Clone, Debug)]
pub struct SessionRow {
    pub kind: String,
    pub filekey: String,
    pub session: String,
    pub model: String,
    pub provider: Option<String>,
    pub first: i64,
    pub last: i64,
    pub tokens: Tokens,
    pub cost: f64,
    pub events: u64,
}

type RowKey = (String, String, String, String, String, bool);
type SessionRowKey = (String, String, String, String, String, bool);

/// Identity evidence of one session (DESIGN 11.1a): its full hash and, when the log states one, its cwd.
#[derive(Clone, Debug, Default)]
pub struct SessionEvidence {
    pub hash: String,
    pub cwd: Option<String>,
    /// T3 Code's thread and provider instance for this session, from T3's own record (Cursor).
    pub t3: Option<(String, String)>,
}

/// One usage event for `messages[]` (history contract 1.4a).
#[derive(Clone, Debug)]
pub struct Message {
    pub kind: String,
    pub filekey: String,
    pub key: String,
    pub at: i64,
    pub session: Option<String>,
    pub model: String,
    pub provider: Option<String>,
    pub tokens: Tokens,
    pub cost: f64,
}

pub struct Collector {
    pub min_date_ms: i64,
    deadline: Instant,
    pub budget: f64,
    pub now_ms: i64,
    pub prior: Map<String, Value>,
    pub fresh: BTreeMap<String, BTreeMap<String, Print>>,
    pub pending: BTreeMap<String, BTreeMap<String, Print>>,
    rows: HashMap<RowKey, usize>,
    pub row_list: Vec<Row>,
    srows: HashMap<SessionRowKey, usize>,
    pub srow_list: Vec<SessionRow>,
    pub truncated: bool,
    pub row_cap: bool,
    pub discovery_truncated: bool,
    pub wal_unread: bool,
    pub files_seen: usize,
    /// While Some, Claude and Codex parsers only collect record keys into it (T3 copy references).
    pub capture: Option<HashSet<String>>,
    /// While Some, records whose key it holds are not counted.
    pub skip: Option<HashSet<String>>,
    pub agy_unreadable: usize,
    pub codex_compressed_unread: HashSet<String>,
    pub usage_partial: HashSet<String>,
    pub read_failures: HashMap<String, BTreeSet<String>>,
    pub replaced_files: BTreeSet<String>,
    /// Discovery: each kind's roots with their origin, and files found per real root.
    pub root_origins: HashMap<String, Vec<(PathBuf, String)>>,
    pub root_counts: HashMap<(String, PathBuf), (usize, i64)>,
    /// Identity evidence per published session key.
    pub sessions: BTreeMap<String, SessionEvidence>,
    pub capture_cwd: bool,
    /// Per-message records, when the request asked for them.
    pub messages_wanted: bool,
    pub messages: Vec<Message>,
    pub messages_cut: bool,
    /// Files an earlier call left part-read, unchanged since: (kind, filekey) -> (decoded offset to
    /// resume at, the reader's carry-over state). DESIGN 16.1.
    pub resume: HashMap<(String, String), (u64, Value)>,
    /// Compressed logs the decoder could not read this call: path and size, for naming.
    pub unreadable_logs: Vec<(PathBuf, u64)>,
    /// Codex logs with no valid session header this call: path and size, for naming
    /// ("partly read: no session id", RECHECK-BIG-1 M1). Never counted.
    pub no_session_logs: Vec<(PathBuf, u64)>,
    /// Tests only: log lines left before the scan counts as past its deadline.
    pub lines_left: Option<u64>,
    /// Records (Cursor runs, Devin replies) whose numbers could not be read, per kind.
    pub unreadable_records: HashMap<String, usize>,
    /// Kinds opened immutable while a write-ahead log held data (its rows wait there).
    pub kind_wal_unread: HashSet<String>,
    /// Cursor SDK run stores found; with any, presence does not report Cursor apart.
    pub run_stores: usize,
    /// Roots (log folders, databases) that are there but could not be read (DESIGN 16.4,
    /// FIX-ROUND-BIG 4 and 9): (kind, path) -> (root state, cause). Never stat'ed again.
    pub root_failures: HashMap<(String, PathBuf), (&'static str, &'static str)>,
    /// Folders inside a root that exist but could not be listed, per kind (digests of their paths).
    pub unreadable_folders: HashMap<String, BTreeSet<String>>,
    /// A discovery bound that left roots unlooked-at, per kind: (what was read, how many were
    /// found, None when the search stopped before it could count them all). FIX-ROUND-BIG 6.
    pub search_limits: HashMap<String, (usize, Option<usize>)>,
    /// Network or FUSE mounts that did not answer their probe this scan.
    pub unanswered_mounts: usize,
    /// Roots their probe found absent: never touched again this scan (the report included).
    pub absent_roots: HashSet<(String, PathBuf)>,
    pub store_incremental: bool,
    settled_paths: HashMap<(String, String), String>,
    settled_slot: u64,
    pub settled: BTreeMap<String, Map<String, Value>>,
}

/// The owner and all readers use one ten-minute cadence.
pub const SETTLED_CHECK_MS: u64 = 10 * 60 * 1000;

pub fn settled_print(print: &Value, cutoff: i64) -> bool {
    print["pathKey"].is_string()
        && print["mtimeMs"].as_i64().is_some_and(|mtime| mtime < cutoff)
        && print["newestUsageMs"].as_i64().is_some_and(|usage| usage < cutoff)
        && print.get("readThrough").is_none()
        && print["usageSkipped"] != true
}

/// Kinds whose reads resume where the deadline cut them (DESIGN 16.1): Codex, whose largest logs
/// are single-frame zstd streams of several GB.
pub fn resumable(kind: &str) -> bool {
    kind == "codex"
}

impl Collector {
    pub fn new(min_date_ms: i64, budget_seconds: f64, prior: Map<String, Value>) -> Collector {
        let store_incremental = prior.contains_key("_store");
        let mut settled_paths = HashMap::new();
        if let Some(before) = prior
            .get("_store")
            .and_then(|settings| settings["settledBefore"].as_object())
        {
            for (kind, cutoff) in before {
                let (Some(cutoff), Some(prints)) = (cutoff.as_i64(), prior.get(kind).and_then(Value::as_object)) else {
                    continue;
                };
                for (key, print) in prints {
                    if settled_print(print, cutoff)
                        && let Some(path_key) = print["pathKey"].as_str()
                    {
                        settled_paths.insert((kind.clone(), path_key.to_owned()), key.clone());
                    }
                }
            }
        }
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        let settled_slot = prior
            .get("_store")
            .and_then(|settings| settings["settledSlot"].as_u64())
            .unwrap_or(now_ms as u64 / SETTLED_CHECK_MS);
        Collector {
            min_date_ms,
            deadline: Instant::now() + std::time::Duration::from_secs_f64(budget_seconds.max(0.0)),
            budget: budget_seconds,
            now_ms,
            prior,
            fresh: BTreeMap::new(),
            pending: BTreeMap::new(),
            rows: HashMap::new(),
            row_list: Vec::new(),
            srows: HashMap::new(),
            srow_list: Vec::new(),
            truncated: false,
            row_cap: false,
            discovery_truncated: false,
            wal_unread: false,
            files_seen: 0,
            capture: None,
            skip: None,
            agy_unreadable: 0,
            codex_compressed_unread: HashSet::new(),
            usage_partial: HashSet::new(),
            read_failures: HashMap::new(),
            replaced_files: BTreeSet::new(),
            root_origins: HashMap::new(),
            root_counts: HashMap::new(),
            sessions: BTreeMap::new(),
            capture_cwd: true,
            messages_wanted: false,
            messages: Vec::new(),
            messages_cut: false,
            resume: HashMap::new(),
            unreadable_logs: Vec::new(),
            no_session_logs: Vec::new(),
            lines_left: None,
            unreadable_records: HashMap::new(),
            kind_wal_unread: HashSet::new(),
            run_stores: 0,
            root_failures: HashMap::new(),
            unreadable_folders: HashMap::new(),
            search_limits: HashMap::new(),
            unanswered_mounts: 0,
            absent_roots: HashSet::new(),
            store_incremental,
            settled_paths,
            settled_slot,
            settled: BTreeMap::new(),
        }
    }

    pub fn expired(&self) -> bool {
        self.lines_left == Some(0) || Instant::now() >= self.deadline
    }

    /// Tests only: one more log line read (see `lines_left`).
    pub fn count_line(&mut self) {
        if let Some(left) = self.lines_left.as_mut() {
            *left = left.saturating_sub(1);
        }
    }

    pub fn deadline(&self) -> Instant {
        self.deadline
    }

    pub fn prior_print(&self, kind: &str, key: &str) -> Option<&Value> {
        self.prior.get(kind)?.as_object()?.get(key)
    }

    pub fn prior_kind(&self, kind: &str) -> Option<&Map<String, Value>> {
        self.prior.get(kind)?.as_object()
    }

    /// The owner already read this path through EOF and settled every measured hour.
    pub fn skip_settled(&mut self, kind: &str, root: &Path, path: &Path) -> bool {
        if self.messages_wanted || self.settled_paths.is_empty() {
            return false;
        }
        let lookup = (kind.to_owned(), util::filekey(kind, path));
        let Some(key) = self.settled_paths.get(&lookup) else {
            return false;
        };
        let Some(mut print) = self.prior_print(kind, key).cloned() else {
            return false;
        };
        if print["settledCheckedSlot"].as_u64() != Some(self.settled_slot) && !settled_metadata_matches(path, &print) {
            self.settled_paths.remove(&lookup);
            return false;
        }
        if let Some(print) = print.as_object_mut() {
            print.remove("readFrom");
            print.insert("settledCheckedSlot".into(), self.settled_slot.into());
        }
        let mtime = print["mtimeMs"].as_i64().unwrap_or(0);
        self.settled
            .entry(kind.to_owned())
            .or_default()
            .insert(key.clone(), print);
        let count = self.root_counts.entry((kind.to_owned(), root.to_owned())).or_default();
        count.0 += 1;
        count.1 = count.1.max(mtime);
        true
    }

    pub fn note_root(&mut self, kind: &str, path: &Path, origin: &str) {
        self.root_origins
            .entry(kind.to_string())
            .or_default()
            .push((path.to_path_buf(), origin.to_string()));
    }

    pub fn count_root(&mut self, kind: &str, directory: &Path, paths: &[PathBuf]) {
        let mut newest = 0i64;
        for path in paths {
            if let Ok(meta) = std::fs::metadata(path) {
                newest = newest.max(util::mtime_ms(&meta));
            }
            // SQLite activity can live entirely in the WAL while the main file stays old.
            if let Ok(meta) = std::fs::metadata(format!("{}-wal", util::path_text(path))) {
                newest = newest.max(util::mtime_ms(&meta));
            }
        }
        let entry = self
            .root_counts
            .entry((kind.to_string(), directory.to_path_buf()))
            .or_insert((0, 0));
        entry.0 += paths.len();
        entry.1 = entry.1.max(newest);
    }

    /// A file that was found but could not be read: counted once, as failed, never also as found
    /// (FIX-ROUND-BIG 3).
    pub fn read_failure(&mut self, kind: &str, key: String) {
        if let Some(pending) = self.pending.get_mut(kind) {
            pending.remove(&key);
        }
        self.read_failures.entry(kind.to_string()).or_default().insert(key);
        self.usage_partial.insert(kind.to_string());
    }

    /// A folder inside a root that exists but cannot be listed: the kind is partial and says how
    /// many (FIX-ROUND-BIG 4). Its cached rows stay.
    pub fn unreadable_folder(&mut self, kind: &str, directory: &Path) {
        // Error paths can lose the entry's type (a failed stat, a raced file, a socket or a
        // SQLite sidecar). Count a folder only with evidence that it is one. Never re-probe
        // an avoided mount; unknown entries remain failures without a false folder claim.
        if crate::fsx::avoided(directory) || !crate::fsx::metadata(directory).is_ok_and(|meta| meta.is_dir()) {
            self.read_failure(kind, util::filekey(kind, directory));
            return;
        }
        self.unreadable_folders
            .entry(kind.to_string())
            .or_default()
            .insert(util::filekey(kind, directory));
        self.usage_partial.insert(kind.to_string());
    }

    /// A root that is there but could not be read: named on the root, and the kind is partial.
    pub fn root_failure(&mut self, kind: &str, path: &Path, state: &'static str, cause: &'static str) {
        self.root_failures
            .insert((kind.to_string(), path.to_path_buf()), (state, cause));
        self.usage_partial.insert(kind.to_string());
    }

    /// A discovery bound cut this kind's search: `read` of `found` (None: more than could be
    /// counted). The kind is partial, so nothing it did not reach is dropped or taken over.
    pub fn search_limit(&mut self, kind: &str, read: usize, found: Option<usize>) {
        let entry = self.search_limits.entry(kind.to_string()).or_insert((read, found));
        entry.0 = entry.0.max(read);
        entry.1 = match (entry.1, found) {
            (Some(a), Some(b)) => Some(a.max(b)),
            _ => None,
        };
        self.discovery_truncated = true;
        self.usage_partial.insert(kind.to_string());
    }

    /// The filekey when the file needs parsing (new or changed); None when unchanged, capped or
    /// unreadable. `deps` (a copy's references) is folded into the tail print.
    pub fn note_file(&mut self, kind: &str, path: &Path, deps: Option<&str>) -> Option<String> {
        self.note_file_as(kind, path, util::filekey(kind, path), deps)
    }

    /// `note_file` under a key that names the file's conversation or session rather than its path
    /// (FIX-ROUND-BIG 1): a copy anywhere, or the file moved, keeps the same rows.
    pub fn note_file_as(&mut self, kind: &str, path: &Path, key: String, deps: Option<&str>) -> Option<String> {
        self.files_seen += 1;
        if self.files_seen > MAX_FILES {
            self.truncated = true;
            return None;
        }
        let Some(mut current) = util::fingerprint(path) else {
            self.read_failure(kind, key);
            return None;
        };
        if self.store_incremental && matches!(kind, "claude" | "codex" | "muse" | "omp") {
            current.path_key = Some(util::filekey(kind, path));
            current.file_identity = crate::fsx::identity(path).map(|(device, inode)| format!("{device:x}:{inode:x}"));
        }
        if kind == "claude" {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            current.claude_name = Some(util::sha256_hex(name.as_bytes()));
            current.claude_mirror = Some(crate::read::claude::is_mirror(path));
        }
        if let Some(deps) = deps {
            current.tail = util::sha256_hex(format!("{}\n{}", current.tail, deps).as_bytes());
        }
        let prior = self.prior_print(kind, &key);
        let same = util::same_print(prior, &current)
            && prior
                .and_then(|p| p["fileIdentity"].as_str())
                .is_none_or(|identity| current.file_identity.as_deref() == Some(identity));
        if same {
            current.append_state = prior.and_then(|p| p.get("appendState")).cloned();
        }
        // Old fingerprints carry no usage clock. Read them once to establish one; keeping an
        // unchanged old print forever would keep both its usage and freshness unverified.
        let clock_known = self
            .prior_print(kind, &key)
            .is_some_and(|p| p.get("newestUsageMs").is_some());
        // An unchanged file an earlier call left part-read is read on from where it stopped.
        let resume = if same && resumable(kind) {
            self.prior_print(kind, &key).and_then(|prior| {
                let through = prior.get("readThrough").and_then(Value::as_u64)?;
                let state = prior.get("readState").cloned().unwrap_or(Value::Null);
                Some((through, state))
            })
        } else {
            None
        };
        let needs_checkpoint = current.path_key.is_some() && prior.is_some_and(|p| p.get("pathKey").is_none());
        let resuming = resume.is_some();
        if let Some(point) = resume {
            self.resume.insert((kind.to_string(), key.clone()), point);
        }
        // Messages already sent stay sent while the file only grew: its first sent bytes unchanged.
        if let Some(prior) = self.prior_print(kind, &key)
            && let Some(through) = prior.get("msgThrough").and_then(Value::as_u64)
            && let Some(head) = prior.get("msgHead").and_then(Value::as_str)
            && through <= current.size
            && util::head_digest(path, through).as_deref() == Some(head)
        {
            current.msg_through = Some(through);
            current.msg_head = Some(head.to_string());
        }
        // A file whose messages were not all sent yet (a cut answer, or prints from before messages
        // were asked for) is read again, even when it did not change.
        let unsent = self.messages_wanted && current.msg_through.is_none_or(|through| through < current.size);
        self.pending
            .entry(kind.to_string())
            .or_default()
            .insert(key.clone(), current);
        if same && clock_known && !unsent && !resuming && !needs_checkpoint {
            None
        } else {
            Some(key)
        }
    }

    /// Where a resumed read of this file starts (0 for a whole read), and the state to restore.
    pub fn resume_point(&self, kind: &str, key: &str) -> (u64, Value) {
        self.resume
            .get(&(kind.to_string(), key.to_string()))
            .cloned()
            .unwrap_or((0, Value::Null))
    }

    /// The deadline cut a resumable read at `through` (after the last complete line), having
    /// started at `from`. With progress, the file's print records how far it got and its rows
    /// stay in the answer, so the next call reads on from there; without, the file stays unvisited
    /// and the server keeps what it had.
    pub fn cut_file(&mut self, kind: &str, key: &str, from: u64, through: u64, state: Value, path: &Path) {
        if through <= from {
            return;
        }
        let Some(mut print) = self.pending.get(kind).and_then(|m| m.get(key)).cloned() else {
            return;
        };
        print.progress = Some(ReadProgress {
            through: Some(through),
            from: (from > 0).then_some(from),
            state: Some(state),
            name: Some(util::display_name(path)),
            decoded: decoded_size(path),
        });
        self.fresh
            .entry(kind.to_string())
            .or_default()
            .insert(key.to_string(), print);
    }

    /// A resumed read reached the end: its rows continue the earlier ones from `from`.
    pub fn finish_resumed(&mut self, kind: &str, key: &str, from: u64) {
        if from == 0 {
            return;
        }
        if let Some(print) = self.fresh.get_mut(kind).and_then(|m| m.get_mut(key)) {
            print.progress = Some(ReadProgress {
                from: Some(from),
                ..ReadProgress::default()
            });
        }
    }

    /// After a whole read: this file's messages are sent through its current size.
    pub fn sent_through(&mut self, kind: &str, key: &str, path: &Path) {
        if !self.messages_wanted || self.messages_cut {
            return;
        }
        let Some(size) = self.pending.get(kind).and_then(|m| m.get(key)).map(|p| p.size) else {
            return;
        };
        let head = util::head_digest(path, size);
        self.set_pending(kind, key, |print| {
            print.msg_through = Some(size);
            print.msg_head = head;
        });
    }

    /// Mark a fully parsed file fresh; truncated files stay unconfirmed.
    pub fn confirm_file(&mut self, kind: &str, key: &str) {
        let Some(mut pending) = self.pending.get(kind).and_then(|m| m.get(key)).cloned() else {
            return;
        };
        if let Some(prior) = self.prior_print(kind, key)
            && util::same_print(Some(prior), &pending)
            && prior.get("usageSkipped").and_then(Value::as_bool) == Some(true)
        {
            pending.usage_skipped = Some(true);
        }
        if pending.usage_skipped == Some(true) {
            self.usage_partial.insert(kind.to_string());
        }
        self.fresh
            .entry(kind.to_string())
            .or_default()
            .insert(key.to_string(), pending);
    }

    pub fn set_pending<F: FnOnce(&mut Print)>(&mut self, kind: &str, key: &str, change: F) {
        if let Some(print) = self.pending.get_mut(kind).and_then(|m| m.get_mut(key)) {
            change(print);
        }
    }

    pub fn is_pending(&self, kind: &str, key: &str) -> bool {
        self.pending.get(kind).is_some_and(|m| m.contains_key(key))
    }

    /// False when the record is captured as a key or is a known copy.
    pub fn keep_record(&mut self, key: Option<String>) -> bool {
        if let Some(capture) = self.capture.as_mut() {
            if let Some(key) = key {
                capture.insert(key);
            }
            return false;
        }
        match (&self.skip, key) {
            (Some(skip), Some(key)) => !skip.contains(&key),
            _ => true,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add(
        &mut self,
        kind: &str,
        filekey: &str,
        model: &str,
        provider: Option<&str>,
        hour: &str,
        tokens: Tokens,
        cost: f64,
    ) {
        let key = (
            kind.to_string(),
            filekey.to_string(),
            model.to_string(),
            provider.unwrap_or("").to_string(),
            hour.to_string(),
            cost > 0.0,
        );
        let index = match self.rows.get(&key) {
            Some(index) => *index,
            None => {
                if self.rows.len() >= MAX_ROWS {
                    self.truncated = true;
                    self.row_cap = true;
                    return;
                }
                self.row_list.push(Row {
                    kind: kind.to_string(),
                    filekey: filekey.to_string(),
                    model: model.to_string(),
                    provider: provider.filter(|p| !p.is_empty()).map(str::to_string),
                    hour: hour.to_string(),
                    tokens: [0; 4],
                    cost: 0.0,
                    events: 0,
                });
                self.rows.insert(key, self.row_list.len() - 1);
                self.row_list.len() - 1
            }
        };
        let row = &mut self.row_list[index];
        for (slot, value) in row.tokens.iter_mut().zip(tokens) {
            *slot += value;
        }
        row.cost += cost;
        row.events += 1;
    }

    /// Add `events` usage events at once (zcode's grouped SQL rows).
    #[allow(clippy::too_many_arguments)]
    pub fn add_many(
        &mut self,
        kind: &str,
        filekey: &str,
        model: &str,
        provider: Option<&str>,
        hour: &str,
        tokens: Tokens,
        events: u64,
    ) -> bool {
        let key = (
            kind.to_string(),
            filekey.to_string(),
            model.to_string(),
            provider.unwrap_or("").to_string(),
            hour.to_string(),
            false,
        );
        let index = match self.rows.get(&key) {
            Some(index) => *index,
            None => {
                if self.rows.len() >= MAX_ROWS {
                    self.truncated = true;
                    self.row_cap = true;
                    return false;
                }
                self.row_list.push(Row {
                    kind: kind.to_string(),
                    filekey: filekey.to_string(),
                    model: model.to_string(),
                    provider: provider.filter(|p| !p.is_empty()).map(str::to_string),
                    hour: hour.to_string(),
                    tokens: [0; 4],
                    cost: 0.0,
                    events: 0,
                });
                self.rows.insert(key, self.row_list.len() - 1);
                self.row_list.len() - 1
            }
        };
        let row = &mut self.row_list[index];
        for (slot, value) in row.tokens.iter_mut().zip(tokens) {
            *slot += value;
        }
        row.events += events;
        true
    }

    #[allow(clippy::too_many_arguments)]
    pub fn sadd(
        &mut self,
        kind: &str,
        filekey: &str,
        session: Option<&str>,
        model: &str,
        provider: Option<&str>,
        epoch_ms: i64,
        tokens: Tokens,
        cost: f64,
    ) {
        self.sadd_range(
            kind,
            filekey,
            session,
            model,
            provider,
            (epoch_ms, epoch_ms),
            tokens,
            cost,
            1,
        );
    }

    /// One session aggregate per session, model, route and logged-cost split. False when capped.
    #[allow(clippy::too_many_arguments)]
    pub fn sadd_range(
        &mut self,
        kind: &str,
        filekey: &str,
        session: Option<&str>,
        model: &str,
        provider: Option<&str>,
        span: (i64, i64),
        tokens: Tokens,
        cost: f64,
        events: u64,
    ) -> bool {
        let Some(session) = session.filter(|s| !s.is_empty()) else {
            return true;
        };
        let key = (
            kind.to_string(),
            filekey.to_string(),
            session.to_string(),
            model.to_string(),
            provider.unwrap_or("").to_string(),
            cost > 0.0,
        );
        let index = match self.srows.get(&key) {
            Some(index) => *index,
            None => {
                if self.srows.len() >= MAX_SROWS {
                    self.truncated = true;
                    return false;
                }
                self.srow_list.push(SessionRow {
                    kind: kind.to_string(),
                    filekey: filekey.to_string(),
                    session: session.to_string(),
                    model: model.to_string(),
                    provider: provider.filter(|p| !p.is_empty()).map(str::to_string),
                    first: span.0,
                    last: span.1,
                    tokens: [0; 4],
                    cost: 0.0,
                    events: 0,
                });
                self.srows.insert(key, self.srow_list.len() - 1);
                self.srow_list.len() - 1
            }
        };
        let row = &mut self.srow_list[index];
        row.first = row.first.min(span.0);
        row.last = row.last.max(span.1);
        for (slot, value) in row.tokens.iter_mut().zip(tokens) {
            *slot += value;
        }
        row.cost += cost;
        row.events += events;
        true
    }

    /// Drop every row and session row of one file (a failed or discarded read).
    pub fn drop_file_rows(&mut self, keys: &HashSet<String>) {
        let rows: Vec<Row> = self.row_list.drain(..).filter(|r| !keys.contains(&r.filekey)).collect();
        let srows: Vec<SessionRow> = self
            .srow_list
            .drain(..)
            .filter(|r| !keys.contains(&r.filekey))
            .collect();
        self.rows.clear();
        self.srows.clear();
        for row in rows {
            let key = (
                row.kind.clone(),
                row.filekey.clone(),
                row.model.clone(),
                row.provider.clone().unwrap_or_default(),
                row.hour.clone(),
                row.cost > 0.0,
            );
            self.row_list.push(row);
            self.rows.insert(key, self.row_list.len() - 1);
        }
        for row in srows {
            let key = (
                row.kind.clone(),
                row.filekey.clone(),
                row.session.clone(),
                row.model.clone(),
                row.provider.clone().unwrap_or_default(),
                row.cost > 0.0,
            );
            self.srow_list.push(row);
            self.srows.insert(key, self.srow_list.len() - 1);
        }
        self.messages.retain(|m| !keys.contains(&m.filekey));
    }

    /// Keep only rows of confirmed files (after a cut scan).
    pub fn keep_confirmed(&mut self) {
        let confirmed: HashSet<String> = self.fresh.values().flat_map(|prints| prints.keys().cloned()).collect();
        let unconfirmed: HashSet<String> = self
            .row_list
            .iter()
            .map(|r| r.filekey.clone())
            .chain(self.srow_list.iter().map(|r| r.filekey.clone()))
            .filter(|key| !confirmed.contains(key))
            .collect();
        self.drop_file_rows(&unconfirmed);
    }

    /// Record a session's identity evidence (full hash, cwd with home as ~).
    pub fn note_session(&mut self, kind: &str, raw_id: &str, cwd: Option<String>) {
        let (Some(key), Some(hash)) = (
            util::session_key(kind, Some(raw_id)),
            util::session_hash(kind, Some(raw_id)),
        ) else {
            return;
        };
        let entry = self.sessions.entry(key).or_insert_with(|| SessionEvidence {
            hash,
            cwd: None,
            t3: None,
        });
        if self.capture_cwd && entry.cwd.is_none() {
            entry.cwd = cwd;
        }
    }

    fn probe_wait(&self) -> std::time::Duration {
        std::time::Duration::from_secs(2).min(
            self.deadline()
                .saturating_duration_since(Instant::now())
                .max(std::time::Duration::from_millis(100)),
        )
    }

    /// A kind's log folders with each probed first (DESIGN 16.4, FIX-ROUND-BIG 4 and 9): the real
    /// paths of those that answer; one that is there but cannot be listed, or does not answer
    /// within 2 s, is recorded on its root (and the kind is partial), never read.
    pub fn reachable_dirs(&mut self, kind: &str, dirs: &[PathBuf]) -> Vec<PathBuf> {
        let wait = self.probe_wait();
        self.reachable(kind, crate::fsx::probe_dirs(dirs, wait))
    }

    /// `reachable_dirs` for database files.
    pub fn reachable_files(&mut self, kind: &str, files: &[PathBuf]) -> Vec<PathBuf> {
        let wait = self.probe_wait();
        self.reachable(kind, crate::fsx::probe_files(files, wait))
    }

    /// The given paths (not their real paths) whose probe answered: folders.
    pub fn gate_dirs(&mut self, kind: &str, dirs: &[PathBuf]) -> Vec<PathBuf> {
        let wait = self.probe_wait();
        let probes = crate::fsx::probe_dirs(dirs, wait);
        self.gate(kind, probes)
    }

    /// The given paths (not their real paths) whose probe answered: database files.
    pub fn gate_files(&mut self, kind: &str, files: &[PathBuf]) -> Vec<PathBuf> {
        let wait = self.probe_wait();
        let probes = crate::fsx::probe_files(files, wait);
        self.gate(kind, probes)
    }

    fn gate(&mut self, kind: &str, probes: Vec<(PathBuf, crate::fsx::Probe)>) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for (path, probe) in probes {
            match probe {
                crate::fsx::Probe::Readable(_) => found.push(path),
                crate::fsx::Probe::Absent => {
                    self.absent_roots.insert((kind.to_string(), path.clone()));
                }
                crate::fsx::Probe::Failed { state, cause } => self.root_failure(kind, &path, state, cause),
            }
        }
        found
    }

    fn reachable(&mut self, kind: &str, probes: Vec<(PathBuf, crate::fsx::Probe)>) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for (path, probe) in probes {
            match probe {
                crate::fsx::Probe::Readable(real) => found.push(real),
                crate::fsx::Probe::Absent => {
                    self.absent_roots.insert((kind.to_string(), path.clone()));
                }
                crate::fsx::Probe::Failed { state, cause } => self.root_failure(kind, &path, state, cause),
            }
        }
        found
    }

    /// T3 Code's thread for a session, from T3's own provider-thread record (`t3_native_ref`).
    pub fn note_t3(&mut self, kind: &str, raw_id: &str, thread: &str, instance: &str) {
        let Some(key) = util::session_key(kind, Some(raw_id)) else {
            return;
        };
        if let Some(entry) = self.sessions.get_mut(&key) {
            entry.t3 = Some((thread.to_string(), instance.to_string()));
        }
    }

    /// One usage event as a per-message record, past the file's `msgThrough` offset only.
    #[allow(clippy::too_many_arguments)]
    pub fn message(
        &mut self,
        kind: &str,
        filekey: &str,
        key: String,
        at: i64,
        session: Option<&str>,
        model: &str,
        provider: Option<&str>,
        tokens: Tokens,
        cost: f64,
    ) {
        if !self.messages_wanted || self.capture.is_some() {
            return;
        }
        if self.messages.len() >= MAX_MESSAGES {
            self.messages_cut = true;
            return;
        }
        self.messages.push(Message {
            kind: kind.to_string(),
            filekey: filekey.to_string(),
            key,
            at,
            session: session.map(str::to_string),
            model: model.to_string(),
            provider: provider.map(str::to_string),
            tokens,
            cost,
        });
    }
}

fn settled_metadata_matches(path: &Path, print: &Value) -> bool {
    let Ok(metadata) = crate::fsx::metadata(path) else {
        return false;
    };
    metadata.is_file()
        && print["size"].as_u64() == Some(metadata.len())
        && print["mtimeMs"].as_i64() == Some(util::mtime_ms(&metadata))
        && crate::fsx::metadata_identity(path, &metadata)
            .is_some_and(|(device, inode)| print["fileIdentity"].as_str() == Some(&format!("{device:x}:{inode:x}")))
}

/// A compressed log's decoded size when its first frame header states it (Codex's do); for a plain
/// file, its size.
pub fn decoded_size(path: &Path) -> Option<u64> {
    if !util::path_text(path).ends_with(".zst") {
        return std::fs::metadata(path).ok().map(|m| m.len());
    }
    let mut header = [0u8; 18];
    let mut file = std::fs::File::open(path).ok()?;
    let mut read = 0;
    while read < header.len() {
        match std::io::Read::read(&mut file, &mut header[read..]) {
            Ok(0) => break,
            Ok(n) => read += n,
            Err(_) => return None,
        }
    }
    zstd::zstd_safe::get_frame_content_size(&header[..read]).ok().flatten()
}
