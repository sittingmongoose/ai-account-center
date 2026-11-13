//! One scan's state: fingerprints read and confirmed, hourly rows, session rows, caps and the
//! deadline. The Python helper's `Collector`, field for field.

use crate::util::{self, Print};
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
}

impl Collector {
    pub fn new(min_date_ms: i64, budget_seconds: f64, prior: Map<String, Value>) -> Collector {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
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
        }
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.deadline
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
        }
        let entry = self
            .root_counts
            .entry((kind.to_string(), directory.to_path_buf()))
            .or_insert((0, 0));
        entry.0 += paths.len();
        entry.1 = entry.1.max(newest);
    }

    pub fn read_failure(&mut self, kind: &str, key: String) {
        self.read_failures.entry(kind.to_string()).or_default().insert(key);
        self.usage_partial.insert(kind.to_string());
    }

    /// The filekey when the file needs parsing (new or changed); None when unchanged, capped or
    /// unreadable. `deps` (a copy's references) is folded into the tail print.
    pub fn note_file(&mut self, kind: &str, path: &Path, deps: Option<&str>) -> Option<String> {
        self.files_seen += 1;
        if self.files_seen > MAX_FILES {
            self.truncated = true;
            return None;
        }
        let Some(mut current) = util::fingerprint(path) else {
            self.read_failure(kind, util::filekey(kind, path));
            return None;
        };
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
        let key = util::filekey(kind, path);
        let same = util::same_print(self.prior_print(kind, &key), &current);
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
        if same && !unsent { None } else { Some(key) }
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
        let entry = self
            .sessions
            .entry(key)
            .or_insert_with(|| SessionEvidence { hash, cwd: None });
        if self.capture_cwd && entry.cwd.is_none() {
            entry.cwd = cwd;
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
