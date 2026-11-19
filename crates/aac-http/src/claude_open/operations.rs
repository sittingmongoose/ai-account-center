//! Port of `claude-open-operations.ts`: the in-memory Open progress store the 202 progress
//! view and GET /desktop-profiles' `openOperation` read. Operations live in memory only: a
//! service restart never resumes one (the durable history marker still holds an unconfirmed
//! copy). Counts and states only — never UUIDs, titles, transcript text, ssh details or paths.
use serde::Serialize;
use serde_json::{Value, json};
use std::future::Future;
use std::sync::{Arc, Mutex};
use tokio::sync::watch;

/// The fixed refusal sentence for an unconfirmed history copy (the open service's constant).
pub const CLAUDE_HISTORY_UNCONFIRMED_MESSAGE: &str =
    "Claude history copy is unconfirmed. Verify it has stopped before opening this profile.";

/// The operation states, in the TS union's order.
pub const STATES: [&str; 6] = [
    "checking",
    "copying",
    "opening",
    "opened",
    "blocked_uncertain",
    "failed",
];

/// The fixed failure sentences (`CLAUDE_OPEN_FAILURE_MESSAGES`).
pub fn failure_message(kind: OpenErrorKind) -> &'static str {
    match kind {
        OpenErrorKind::NotFound => "Claude desktop profile was not found.",
        OpenErrorKind::NotConfigured => "Claude desktop launcher is not configured for this platform.",
        OpenErrorKind::TransportTimeout => "Claude desktop request timed out.",
        OpenErrorKind::Transport => "Claude desktop request failed.",
        OpenErrorKind::Other => "Claude account could not be opened safely.",
        OpenErrorKind::HistoryHeld => CLAUDE_HISTORY_UNCONFIRMED_MESSAGE,
    }
}

/// How an Open ended, classified exactly as `claudeOpenErrorKind`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenErrorKind {
    /// An unconfirmed history copy holds Open.
    HistoryHeld,
    /// The profile is unknown.
    NotFound,
    /// The platform launcher is unconfigured.
    NotConfigured,
    /// The desktop transport timed out.
    TransportTimeout,
    /// The desktop transport failed.
    Transport,
    /// Any other failure.
    Other,
}

impl OpenErrorKind {
    /// The short server-log class; never a message, stack or path.
    pub fn word(self) -> &'static str {
        match self {
            Self::HistoryHeld => "history_held",
            Self::NotFound => "not_found",
            Self::NotConfigured => "not_configured",
            Self::TransportTimeout => "transport_timeout",
            Self::Transport => "transport",
            Self::Other => "other",
        }
    }
}

/// One Open's progress, serialized with the TS field names and order.
#[derive(Clone, Debug, Serialize)]
pub struct Operation {
    /// `op_` plus 24 random hex digits.
    pub id: String,
    /// The target platform.
    pub platform: String,
    /// One of [`STATES`].
    pub state: String,
    /// Records confirmed so far; null before the first batch.
    #[serde(rename = "confirmedCount")]
    pub confirmed_count: Option<u64>,
    /// The whole plan's record count; null before the first batch.
    #[serde(rename = "totalCount")]
    pub total_count: Option<u64>,
    /// A fixed sentence for blocked_uncertain and failed; null otherwise.
    pub message: Option<String>,
}

impl Operation {
    /// The JSON form the profiles list and the 202 body carry.
    pub fn to_json(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

/// Whether a state is terminal (`isTerminalClaudeOpenState`).
pub fn is_terminal(state: &str) -> bool {
    matches!(state, "opened" | "blocked_uncertain" | "failed")
}

/// How an Open ended, for a caller that waits for it. Never a panic.
pub type Outcome = Result<(), OpenErrorKind>;

const DEFAULT_RETAIN_MS: u64 = 10 * 60_000;
const MAX_OPERATIONS: usize = 64;

struct Entry {
    scope: String,
    profile_id: String,
    operation: Operation,
    started_at: u64,
    finished_at: Option<u64>,
    settled: watch::Sender<Option<Outcome>>,
}

/// The progress store's clock and retention options (the TS constructor options).
pub struct Options {
    /// Milliseconds since the epoch.
    pub now: Arc<dyn Fn() -> u64 + Send + Sync>,
    /// How long a finished operation stays visible for the clients' last poll.
    pub retain_ms: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            now: Arc::new(|| {
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0)
            }),
            retain_ms: DEFAULT_RETAIN_MS,
        }
    }
}

struct Inner {
    /// Insertion order, as the TS Map keeps it.
    entries: Vec<(String, Entry)>,
}

/// The process-wide Open progress store (`ClaudeOpenOperations`).
pub struct Operations {
    inner: Arc<Mutex<Inner>>,
    options: Options,
}

fn key(scope: &str, profile_id: &str, platform: &str) -> String {
    json!([scope, profile_id, platform]).to_string()
}

/// 12 random bytes as lowercase hex for the operation id.
fn operation_id() -> String {
    let mut bytes = [0u8; 12];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes))
        .is_err()
    {
        // A missing urandom cannot happen on the Linux host; a fixed id would collide, so
        // fall back to the clock and a counter-free unique suffix is impossible: refuse.
        return String::new();
    }
    format!("op_{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
}

impl Operations {
    /// A store with the default clock and retention.
    pub fn new() -> Self {
        Self::with_options(Options::default())
    }

    /// A store with an injected clock (the parity fixtures fix it).
    pub fn with_options(options: Options) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Inner { entries: Vec::new() })),
            options,
        }
    }

    fn now(&self) -> u64 {
        (self.options.now)()
    }

    fn prune(&self, inner: &mut Inner) {
        let now = self.now();
        let retain_ms = self.options.retain_ms;
        inner
            .entries
            .retain(|(_, entry)| entry.finished_at.is_none_or(|at| now.saturating_sub(at) < retain_ms));
        // Bounded even if many profiles finish at once: drop the oldest finished first.
        while inner.entries.len() > MAX_OPERATIONS {
            let oldest = inner
                .entries
                .iter()
                .enumerate()
                .filter(|(_, (_, entry))| entry.finished_at.is_some())
                .min_by_key(|(_, (_, entry))| entry.finished_at.unwrap_or(0))
                .map(|(index, _)| index);
            match oldest {
                Some(index) => {
                    inner.entries.remove(index);
                }
                None => break,
            }
        }
    }

    /// Start an Open in the background, or return the one already running for the same scope,
    /// profile and platform; a running Open is never started twice.
    pub fn start<F, R>(&self, scope: &str, profile_id: &str, platform: &str, run: R) -> Operation
    where
        R: FnOnce(Observer) -> F + Send + 'static,
        F: Future<Output = Outcome> + Send + 'static,
    {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune(&mut inner);
        let entry_key = key(scope, profile_id, platform);
        if let Some((_, entry)) = inner.entries.iter().find(|(k, _)| *k == entry_key)
            && entry.finished_at.is_none()
        {
            return entry.operation.clone();
        }
        let id = operation_id();
        let operation = Operation {
            id,
            platform: platform.to_string(),
            state: "checking".into(),
            confirmed_count: None,
            total_count: None,
            message: None,
        };
        let (settled, _) = watch::channel(None);
        let entry = Entry {
            scope: scope.to_string(),
            profile_id: profile_id.to_string(),
            operation: operation.clone(),
            started_at: self.now(),
            finished_at: None,
            settled,
        };
        // Map.set keeps an existing key's position; a new key goes last.
        match inner.entries.iter_mut().find(|(k, _)| *k == entry_key) {
            Some(slot) => slot.1 = entry,
            None => inner.entries.push((entry_key.clone(), entry)),
        }
        let observer = Observer {
            inner: Arc::clone(&self.inner),
            key: entry_key.clone(),
        };
        let inner = Arc::clone(&self.inner);
        let now = Arc::clone(&self.options.now);
        // The TS store calls run(observer) synchronously; only its outcome waits in the
        // background, so the caller's observer handles exist when start returns.
        let running = run(observer);
        tokio::spawn(async move {
            let outcome = running.await;
            let mut guard = inner.lock().unwrap_or_else(|e| e.into_inner());
            let Some((_, entry)) = guard.entries.iter_mut().find(|(k, _)| *k == entry_key) else {
                return;
            };
            if entry.finished_at.is_some() {
                return;
            }
            let (state, message) = match outcome {
                Ok(()) => ("opened", None),
                Err(kind) => (
                    if kind == OpenErrorKind::HistoryHeld {
                        "blocked_uncertain"
                    } else {
                        "failed"
                    },
                    Some(failure_message(kind).to_string()),
                ),
            };
            entry.operation.state = state.into();
            entry.operation.message = message;
            entry.finished_at = Some(now());
            let _ = entry.settled.send(Some(outcome));
            // State and error class only: no message, stack, ids or paths.
            let kind_word = match outcome {
                Ok(()) => "none",
                Err(kind) => kind.word(),
            };
            if outcome.is_ok() {
                tracing::info!(target: "aac_http", state, kind = kind_word, platform = %entry.operation.platform, "claude.open.finished");
            } else {
                tracing::warn!(target: "aac_http", state, kind = kind_word, platform = %entry.operation.platform, "claude.open.finished");
            }
        });
        operation
    }

    /// How the current Open for this profile and platform ends (a running one, or the last
    /// finished one while it is retained); None when none is known. Never panics.
    pub fn settled(
        &self,
        scope: &str,
        profile_id: &str,
        platform: &str,
    ) -> Option<impl Future<Output = Outcome> + use<>> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let entry_key = key(scope, profile_id, platform);
        let mut receiver = inner
            .entries
            .iter()
            .find(|(k, _)| *k == entry_key)?
            .1
            .settled
            .subscribe();
        Some(async move {
            loop {
                if let Some(outcome) = *receiver.borrow_and_update() {
                    return outcome;
                }
                if receiver.changed().await.is_err() {
                    return Err(OpenErrorKind::Other);
                }
            }
        })
    }

    /// The running operation for this profile and platform, if any.
    pub fn running(&self, scope: &str, profile_id: &str, platform: &str) -> Option<Operation> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let entry_key = key(scope, profile_id, platform);
        inner
            .entries
            .iter()
            .find(|(k, _)| *k == entry_key)
            .filter(|(_, entry)| entry.finished_at.is_none())
            .map(|(_, entry)| entry.operation.clone())
    }

    /// The operation to show for a profile: a running one first, else the newest finished one.
    pub fn for_profile(&self, scope: &str, profile_id: &str) -> Option<Operation> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune(&mut inner);
        let mut chosen: Option<&Entry> = None;
        for (_, entry) in &inner.entries {
            if entry.scope != scope || entry.profile_id != profile_id {
                continue;
            }
            let running = entry.finished_at.is_none();
            let chosen_running = chosen.is_some_and(|c| c.finished_at.is_none());
            if chosen.is_none()
                || (running && !chosen_running)
                || (running == chosen_running
                    && entry.started_at >= chosen.map_or(0, |c| c.started_at))
            {
                chosen = Some(entry);
            }
        }
        chosen.map(|entry| entry.operation.clone())
    }

    /// `forScope`: `[profileId, operation]` pairs in first-seen order.
    pub fn for_scope(&self, scope: &str) -> Vec<(String, Operation)> {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        self.prune(&mut inner);
        let mut ids: Vec<String> = Vec::new();
        for (_, entry) in &inner.entries {
            if entry.scope == scope && !ids.contains(&entry.profile_id) {
                ids.push(entry.profile_id.clone());
            }
        }
        drop(inner);
        ids.into_iter()
            .filter_map(|id| self.for_profile(scope, &id).map(|op| (id, op)))
            .collect()
    }
}

impl Default for Operations {
    fn default() -> Self {
        Self::new()
    }
}

/// The observer the running Open updates progress through; counts only move forward.
pub struct Observer {
    inner: Arc<Mutex<Inner>>,
    key: String,
}

impl Observer {
    fn update(&self, patch: impl FnOnce(&mut Operation)) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, entry)) = inner.entries.iter_mut().find(|(k, _)| *k == self.key)
            && entry.finished_at.is_none()
        {
            patch(&mut entry.operation);
        }
    }

    /// The copy observer: a bounded batch is about to be appended.
    pub fn copying(&self, total_count: u64, confirmed_count: u64) {
        self.update(|operation| {
            let forward = confirmed_count >= operation.confirmed_count.unwrap_or(0);
            if forward && confirmed_count <= total_count {
                operation.state = "copying".into();
                operation.total_count = Some(total_count);
                operation.confirmed_count = Some(confirmed_count);
            }
        });
    }

    /// The optional history copy finished; a refusal after some batches keeps their count.
    pub fn synchronized(&self, status: &str, created_count: u64) {
        self.update(|operation| {
            if matches!(status, "synchronized" | "refused" | "partial")
                && operation.total_count.is_some()
                && created_count >= operation.confirmed_count.unwrap_or(0)
                && created_count <= operation.total_count.unwrap_or(0)
            {
                operation.confirmed_count = Some(created_count);
            }
        });
    }

    /// The launcher is about to run.
    pub fn opening(&self) {
        self.update(|operation| {
            operation.state = "opening".into();
        });
    }
}
