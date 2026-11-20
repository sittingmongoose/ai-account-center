//! The dashboard's structured log in Rust (M5 lane K, gap G21): the same entries, files,
//! configuration, rotation, retention and redaction as `src/services/logging` and the request
//! log of `request-logging-middleware.ts`, so the TS server's log can go at the cut.
//!
//! Entries are JSON lines in `<ccs>/logs/current.jsonl` (0600, folders 0700) with the TS field
//! order. `logging:` in `<ccs>/config.yaml` (rechecked at most once a second, by mtime) sets
//! `enabled`, `level`, `rotate_mb`, `retain_days` and `redact`. The current file rotates by size
//! or age into `logs/archive/ccs-<iso>.jsonl`; archives past `retain_days` or the 32 MiB total
//! are removed. Logging never fails its caller.
mod redact;
mod subscriber;

pub use redact::{mask_secret_tokens, redact_argv, redact_context, redact_value, scrub_logged_url};
pub use subscriber::install;

use serde_json::{Map, Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

/// Old `.gz` archives and plain rotations share this total budget.
pub const LOG_ARCHIVE_BYTE_CAP: u64 = 32 * 1024 * 1024;
const ONE_DAY: Duration = Duration::from_secs(24 * 60 * 60);
const PRUNE_INTERVAL: Duration = Duration::from_secs(60);
const CONFIG_RECHECK: Duration = Duration::from_secs(1);

/// The four levels, most severe first.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// `error`
    Error,
    /// `warn`
    Warn,
    /// `info`
    Info,
    /// `debug`
    Debug,
}

impl Level {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Level::Error => "error",
            Level::Warn => "warn",
            Level::Info => "info",
            Level::Debug => "debug",
        }
    }
    fn priority(self) -> u8 {
        self as u8
    }
    fn named(word: &str) -> Option<Self> {
        Some(match word {
            "error" => Level::Error,
            "warn" => Level::Warn,
            "info" => Level::Info,
            "debug" => Level::Debug,
            _ => return None,
        })
    }
}

/// The effective `logging:` settings (TS `getResolvedLoggingConfig`).
#[derive(Clone, Debug, PartialEq)]
pub struct LogConfig {
    /// Write at all.
    pub enabled: bool,
    /// Minimum level written; an unknown word writes nothing, as in TS.
    pub level: Option<Level>,
    /// Rotate the current file at this many MiB (at least 1).
    pub rotate_mb: f64,
    /// Keep archives this many days.
    pub retain_days: f64,
    /// Redact context and error values before they are written.
    pub redact: bool,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            level: Some(Level::Info),
            rotate_mb: 10.0,
            retain_days: 7.0,
            redact: true,
        }
    }
}

impl LogConfig {
    /// `{...DEFAULT_LOGGING_CONFIG, ...config.logging}` field by field with `??`.
    pub fn from_yaml(text: &str) -> Self {
        let mut config = Self::default();
        let Ok(yaml) = serde_yaml::from_str::<serde_yaml::Value>(text) else {
            return config;
        };
        let Ok(value) = serde_json::to_value(yaml) else {
            return config;
        };
        // `isUnifiedConfig`: a document without a numeric `version` >= 1 is not read at all.
        if !value
            .get("version")
            .and_then(Value::as_f64)
            .is_some_and(|v| v >= 1.0)
        {
            return config;
        }
        let Some(logging) = value.get("logging").filter(|v| v.is_object()) else {
            return config;
        };
        let present = |key: &str| logging.get(key).filter(|v| !v.is_null());
        if let Some(v) = present("enabled") {
            config.enabled = truthy(v);
        }
        if let Some(v) = present("level") {
            config.level = v.as_str().and_then(Level::named);
        }
        if let Some(v) = present("rotate_mb") {
            config.rotate_mb = number(v);
        }
        if let Some(v) = present("retain_days") {
            config.retain_days = number(v);
        }
        if let Some(v) = present("redact") {
            config.redact = truthy(v);
        }
        config
    }
}

fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// JavaScript numeric coercion for the two numeric settings.
fn number(value: &Value) -> f64 {
    match value {
        Value::Number(n) => n.as_f64().unwrap_or(f64::NAN),
        Value::String(s) => s.trim().parse().unwrap_or(f64::NAN),
        Value::Bool(b) => f64::from(u8::from(*b)),
        _ => f64::NAN,
    }
}

/// A number as `JSON.stringify` writes it: integral values without a fraction.
fn js_number(value: f64) -> Value {
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        json!(value as i64)
    } else {
        json!(value)
    }
}

/// Optional parts of one entry.
#[derive(Clone, Debug, Default)]
pub struct Extra {
    /// The request this entry belongs to.
    pub request_id: Option<String>,
    /// The lifecycle stage word (`intake` ... `cleanup`).
    pub stage: Option<String>,
    /// Elapsed milliseconds.
    pub latency_ms: Option<f64>,
    /// Structured error (`{name, message, code?, stack?, cause?}`).
    pub error: Option<Value>,
}

/// One finished dashboard request, as the TS request log records it.
#[derive(Clone, Debug)]
pub struct RequestLog<'a> {
    /// `x-ccs-request-id`.
    pub request_id: &'a str,
    /// HTTP method.
    pub method: &'a str,
    /// The original URL (path and query); it is scrubbed before it is written.
    pub url: &'a str,
    /// Response status.
    pub status: u16,
    /// Milliseconds from arrival to finish.
    pub duration_ms: u64,
    /// The client (behind a trusted proxy hop) or the peer.
    pub remote_address: Option<&'a str>,
    /// The trusted proxy hop, when the request came through one.
    pub via: Option<Option<&'a str>>,
    /// `User-Agent`.
    pub user_agent: Option<&'a str>,
}

struct State {
    config: LogConfig,
    config_mtime: Option<SystemTime>,
    checked_at: Option<Instant>,
    pruned_at: Option<Instant>,
}

/// The log of one private root.
pub struct Log {
    root: PathBuf,
    run_id: String,
    state: Mutex<State>,
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut bytes = [0u8; N];
    if getrandom::getrandom(&mut bytes).is_err() {
        // Ids only correlate entries; a clock-derived fallback keeps logging alive.
        let seed = now_ms().to_le_bytes();
        for (i, b) in bytes.iter_mut().enumerate() {
            *b = seed[i % 8] ^ (i as u8).wrapping_mul(31);
        }
    }
    bytes
}

/// `crypto.randomUUID()`.
/// A UUID v4 for correlating the public request and its companion route events.
pub fn request_id() -> String {
    let mut b = random_bytes::<16>();
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

fn iso_now() -> String {
    aac_num::ecma::to_iso_string(now_ms()).unwrap_or_else(|| "1970-01-01T00:00:00.000Z".to_owned())
}

impl Log {
    /// The log under `<root>/logs`.
    pub fn new(root: PathBuf) -> Self {
        // `${Date.now()}-${process.pid}-${Math.random().toString(36).slice(2, 10)}`
        let tail: String = random_bytes::<8>()
            .iter()
            .map(|b| char::from(b"0123456789abcdefghijklmnopqrstuvwxyz"[usize::from(*b) % 36]))
            .collect();
        Self {
            root,
            run_id: format!("{}-{}-{tail}", now_ms(), std::process::id()),
            state: Mutex::new(State {
                config: LogConfig::default(),
                config_mtime: None,
                checked_at: None,
                pruned_at: None,
            }),
        }
    }

    fn logs_dir(&self) -> PathBuf {
        self.root.join("logs")
    }
    fn current(&self) -> PathBuf {
        self.logs_dir().join("current.jsonl")
    }
    fn archive_dir(&self) -> PathBuf {
        self.logs_dir().join("archive")
    }

    /// The settings now in effect.
    pub fn config(&self) -> LogConfig {
        let Ok(mut state) = self.state.lock() else {
            return LogConfig::default();
        };
        self.refresh(&mut state);
        state.config.clone()
    }

    fn refresh(&self, state: &mut State) {
        if state
            .checked_at
            .is_some_and(|t| t.elapsed() < CONFIG_RECHECK)
        {
            return;
        }
        let path = self.root.join("config.yaml");
        let mtime = fs::metadata(&path).and_then(|m| m.modified()).ok();
        if state.checked_at.is_none() || mtime != state.config_mtime {
            // The private config through the shared hardened read (L2).
            state.config = match aac_files::read::read(
                &path,
                &aac_files::read::ReadPolicy::capped(4 * 1024 * 1024),
            ) {
                aac_files::read::Read::Bytes(bytes, _) => {
                    LogConfig::from_yaml(&String::from_utf8_lossy(&bytes))
                }
                _ => LogConfig::default(),
            };
            state.config_mtime = mtime;
        }
        state.checked_at = Some(Instant::now());
    }

    /// The entry line for these parts (TS `createEntry`), without writing it.
    #[allow(
        clippy::too_many_arguments,
        reason = "the TS createEntry parts, one for one"
    )]
    pub fn entry(
        &self,
        config: &LogConfig,
        source: &str,
        level: Level,
        event: &str,
        message: &str,
        context: &Map<String, Value>,
        extra: &Extra,
    ) -> Map<String, Value> {
        let mut entry = Map::new();
        entry.insert("id".into(), json!(request_id()));
        entry.insert("timestamp".into(), json!(iso_now()));
        entry.insert("level".into(), json!(level.word()));
        entry.insert("source".into(), json!(source));
        entry.insert("event".into(), json!(event));
        entry.insert("message".into(), json!(mask_secret_tokens(message)));
        entry.insert("processId".into(), json!(std::process::id()));
        entry.insert("runId".into(), json!(self.run_id));
        let context = if config.redact {
            redact_context(context)
        } else {
            context.clone()
        };
        entry.insert("context".into(), Value::Object(context));
        if let Some(id) = &extra.request_id {
            entry.insert("requestId".into(), json!(id));
        }
        if let Some(stage) = &extra.stage {
            entry.insert("stage".into(), json!(stage));
        }
        if let Some(latency) = extra.latency_ms {
            entry.insert("latencyMs".into(), js_number(latency));
        }
        if let Some(error) = &extra.error {
            entry.insert("error".into(), redact_value(error));
        }
        entry
    }

    /// Write one entry when logging is on and the level passes. Never fails.
    pub fn write(
        &self,
        source: &str,
        level: Level,
        event: &str,
        message: &str,
        context: &Map<String, Value>,
        extra: &Extra,
    ) {
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        self.refresh(&mut state);
        let config = state.config.clone();
        let passes = config
            .level
            .is_some_and(|configured| level.priority() <= configured.priority());
        if !config.enabled || !passes {
            return;
        }
        let entry = self.entry(&config, source, level, event, message, context, extra);
        let Ok(mut line) = serde_json::to_string(&Value::Object(entry)) else {
            return;
        };
        line.push('\n');
        if self.ensure_directories().is_err() {
            return;
        }
        let Some(_lease) = self.writer_lease() else {
            return;
        };
        self.rotate_if_needed(&config);
        if append(&self.current(), line.as_bytes()).is_err() {
            return;
        }
        if state
            .pruned_at
            .is_none_or(|t| t.elapsed() >= PRUNE_INTERVAL)
        {
            self.prune_locked(&config);
            state.pruned_at = Some(Instant::now());
        }
    }

    /// `request.completed` from `web-server:http`, unless the URL is under `/api/logs`.
    pub fn request_completed(&self, request: &RequestLog<'_>) {
        if request.url.starts_with("/api/logs") {
            return;
        }
        let mut context = Map::new();
        context.insert("requestId".into(), json!(request.request_id));
        context.insert("method".into(), json!(request.method));
        context.insert("path".into(), json!(scrub_logged_url(request.url)));
        context.insert("statusCode".into(), json!(request.status));
        context.insert("durationMs".into(), json!(request.duration_ms));
        context.insert(
            "remoteAddress".into(),
            json!(request.remote_address.filter(|a| !a.is_empty())),
        );
        if let Some(via) = request.via {
            context.insert("via".into(), json!(via.filter(|a| !a.is_empty())));
        }
        context.insert(
            "userAgent".into(),
            json!(request.user_agent.filter(|a| !a.is_empty())),
        );
        self.write(
            "web-server:http",
            Level::Info,
            "request.completed",
            "Dashboard request completed",
            &context,
            &Extra {
                request_id: Some(request.request_id.to_owned()),
                ..Extra::default()
            },
        );
    }

    fn ensure_directories(&self) -> std::io::Result<()> {
        private_dir(&self.logs_dir())?;
        private_dir(&self.archive_dir())
    }

    fn writer_lease(&self) -> Option<aac_files::WriterLease> {
        aac_files::WriterLease::take(
            &self.logs_dir().join("current.jsonl.writer.lock"),
            Duration::from_millis(500),
        )
        .ok()
    }

    fn rotate_if_needed(&self, config: &LogConfig) {
        self.finish_interrupted_rotation();
        let current = self.current();
        let Ok(meta) = fs::symlink_metadata(&current) else {
            return;
        };
        let age = meta
            .modified()
            .ok()
            .and_then(|m| SystemTime::now().duration_since(m).ok())
            .unwrap_or_default();
        let rotate_bytes = config.rotate_mb.max(1.0) * 1024.0 * 1024.0;
        let exceeds_size = (meta.len() as f64) >= rotate_bytes;
        if !meta.is_file() || (!exceeds_size && age < ONE_DAY) || meta.len() == 0 {
            return;
        }
        let compact = iso_now().replace([':', '.'], "-");
        let archive_dir = self.archive_dir();
        for sequence in 0..100 {
            let name = if sequence == 0 {
                format!("ccs-{compact}.jsonl")
            } else {
                format!("ccs-{compact}-{sequence}.jsonl")
            };
            match fs::hard_link(&current, archive_dir.join(name)) {
                Ok(()) => {
                    if fs::remove_file(&current).is_ok() {
                        self.prune_locked(config);
                    }
                    return;
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(_) => return,
            }
        }
    }

    /// `pruneExpiredLogArchives`: past `retain_days`, then the oldest beyond the byte cap.
    pub fn prune(&self, config: &LogConfig) {
        let Some(_lease) = self.writer_lease() else {
            return;
        };
        self.prune_locked(config);
    }

    /// A rotation publishes `current.jsonl` under an archive name and then unlinks the
    /// original as a separate call. If the process stopped between them, one inode is still
    /// visible under both names and pruning would charge its bytes twice. The caller holds
    /// the writer lease, so unlink the original: the archive already keeps the data.
    fn finish_interrupted_rotation(&self) {
        let current = self.current();
        let Ok(meta) = fs::symlink_metadata(&current) else {
            return;
        };
        if !meta.is_file() || !multiply_linked(&meta) {
            return;
        }
        let Ok(entries) = fs::read_dir(self.archive_dir()) else {
            return;
        };
        let archived = entries.flatten().any(|entry| {
            fs::symlink_metadata(entry.path())
                .is_ok_and(|linked| linked.is_file() && same_inode(&meta, &linked))
        });
        if archived {
            let _ = fs::remove_file(&current);
        }
    }

    fn prune_locked(&self, config: &LogConfig) {
        // A crash mid-rotation leaves `current.jsonl` linked from the archive; finish that
        // rotation before measuring the archive, so this inode is charged once.
        self.finish_interrupted_rotation();
        let Ok(entries) = fs::read_dir(self.archive_dir()) else {
            return;
        };
        let cutoff_ms = now_ms() as f64 - config.retain_days * ONE_DAY.as_millis() as f64;
        let mut kept: Vec<(PathBuf, u64, f64)> = Vec::new();
        let mut charged: Vec<fs::Metadata> = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            if !meta.is_file() {
                continue;
            }
            let mtime = meta
                .modified()
                .ok()
                .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
                .map_or(0.0, |d| d.as_millis() as f64);
            if mtime < cutoff_ms {
                let _ = fs::remove_file(&path);
            } else if multiply_linked(&meta) && charged.iter().any(|seen| same_inode(seen, &meta)) {
                // A second name for an inode already counted: drop the name, not the bytes.
                let _ = fs::remove_file(&path);
            } else {
                let size = meta.len();
                kept.push((path, size, mtime));
                if multiply_linked(&meta) {
                    charged.push(meta);
                }
            }
        }
        let mut bytes: u64 = kept.iter().map(|k| k.1).sum();
        kept.sort_by(|a, b| a.2.total_cmp(&b.2).then_with(|| a.0.cmp(&b.0)));
        for (path, size, _) in kept {
            if bytes <= LOG_ARCHIVE_BYTE_CAP {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                bytes -= size;
            }
        }
    }
}

/// Whether the inode behind `meta` has another name in this filesystem namespace.
#[cfg(unix)]
fn multiply_linked(meta: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.nlink() > 1
}

#[cfg(not(unix))]
fn multiply_linked(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.number_of_links() > 1
}

#[cfg(unix)]
fn same_inode(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

#[cfg(not(unix))]
fn same_inode(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    left.creation_time() == right.creation_time()
        && left.file_size() == right.file_size()
        && left.last_write_time() == right.last_write_time()
}

fn private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

fn append(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // The shared writer lease also covers rotation and pruning.
    options.open(path)?.write_all(bytes)
}
