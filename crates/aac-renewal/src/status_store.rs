//! Secret-free renewal history per saved profile
//! (`src/codex-auth/codex-renewal-status-store.ts`):
//! `<ccs dir>/codex-renewal-status.json`, 0600, atomic replace. Timestamps, an
//! outcome code, a failure count and a binding to the auth file version —
//! never token data or digests of it.

use crate::atomic::replace_file_atomically;
use crate::jsnum::{is_integer_number, is_iso, iso, js_number, js_round, parse_date_ms};
use crate::types::{CodexRenewalReason, is_retry_reason};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The history file name under the CCS root.
pub const CODEX_RENEWAL_STATUS_FILE: &str = "codex-renewal-status.json";
const MAX_STATUS_BYTES: u64 = 256 * 1024;
const BACKOFF_BASE_MS: f64 = 15.0 * 60_000.0;
const BACKOFF_MAX_MS: f64 = 12.0 * 60.0 * 60_000.0;
/// A busy activation lock is retried after this long.
pub const CODEX_RENEWAL_BUSY_RETRY_MS: i64 = 15 * 60_000;

/// One profile's recorded outcome, bound to the auth file version it applies to.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexRenewalRecord {
    /// Stat identity of the saved auth.json the outcome applies to.
    pub auth_version: String,
    /// The outcome code as recorded (any string the reader accepted).
    pub outcome: String,
    /// ISO timestamp of the last attempt.
    pub last_attempt_at: String,
    /// ISO timestamp of the last success, when any.
    pub last_success_at: Option<String>,
    /// Consecutive failures on this file version.
    pub consecutive_failures: f64,
    /// Backoff or busy retry time, when scheduled.
    pub next_attempt_at: Option<String>,
    /// Access expiry the outcome saw, when known.
    pub access_expires_at: Option<String>,
}

impl CodexRenewalRecord {
    /// The record in the file's key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert(
            "authVersion".into(),
            Value::String(self.auth_version.clone()),
        );
        out.insert("outcome".into(), Value::String(self.outcome.clone()));
        out.insert(
            "lastAttemptAt".into(),
            Value::String(self.last_attempt_at.clone()),
        );
        if let Some(n) = js_number(self.consecutive_failures) {
            out.insert("consecutiveFailures".into(), Value::Number(n));
        }
        if let Some(value) = &self.last_success_at {
            out.insert("lastSuccessAt".into(), Value::String(value.clone()));
        }
        if let Some(value) = &self.next_attempt_at {
            out.insert("nextAttemptAt".into(), Value::String(value.clone()));
        }
        if let Some(value) = &self.access_expires_at {
            out.insert("accessExpiresAt".into(), Value::String(value.clone()));
        }
        Value::Object(out)
    }

    /// The recorded outcome as a known reason, when it is one.
    pub fn reason(&self) -> Option<CodexRenewalReason> {
        CodexRenewalReason::from_code(&self.outcome)
    }
}

/// The history keyed by profile name, in file order.
pub type CodexRenewalRecords = Vec<(String, CodexRenewalRecord)>;

/// Look up one profile's record.
pub fn record<'a>(records: &'a CodexRenewalRecords, name: &str) -> Option<&'a CodexRenewalRecord> {
    records.iter().find(|(key, _)| key == name).map(|(_, r)| r)
}

/// Insert or replace one profile's record, keeping the other entries' order.
pub fn set_record(records: &mut CodexRenewalRecords, name: &str, value: CodexRenewalRecord) {
    match records.iter_mut().find(|(key, _)| key == name) {
        Some(entry) => entry.1 = value,
        None => records.push((name.to_owned(), value)),
    }
}

/// The history file under the CCS root.
pub fn codex_renewal_status_path(ccs_dir: &Path) -> PathBuf {
    ccs_dir.join(CODEX_RENEWAL_STATUS_FILE)
}

/// `codexAuthFileVersion`: the file's stat identity without reading it; null
/// when absent. Linux matches Node's bigint `dev:ino:size:mtimeNs` exactly;
/// Windows file indexes differ between Node (128-bit) and Rust (64-bit), so a
/// record written by one runtime reads as stale in the other — the safe
/// direction (one extra attempt, every guard still applies).
pub fn codex_auth_file_version(file: &Path) -> Option<String> {
    let stat = std::fs::metadata(file).ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(format!(
            "{}:{}:{}:{}",
            stat.dev(),
            stat.ino(),
            stat.size(),
            stat.mtime_nsec() + stat.mtime() * 1_000_000_000
        ))
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Node's bigint dev/ino on Windows use the 128-bit file id, which std
        // does not expose stably. Size + last-write time (100 ns FILETIME)
        // gives the same binding semantics within this runtime: every writer
        // replaces the file by rename of a freshly written temp, so the
        // identity changes with the content. Cross-runtime records read as
        // stale on Windows (the safe direction); Linux matches Node exactly.
        Some(format!(
            "win:{}:{}",
            stat.file_size(),
            stat.last_write_time()
        ))
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = stat;
        None
    }
}

fn valid_record(value: &Value) -> Option<CodexRenewalRecord> {
    let record = value.as_object()?;
    let auth_version = record.get("authVersion")?.as_str()?.to_owned();
    let outcome = record.get("outcome")?.as_str()?.to_owned();
    let last_attempt_at = record.get("lastAttemptAt")?.as_str()?.to_owned();
    if !is_iso(record.get("lastAttemptAt")?) {
        return None;
    }
    let failures = is_integer_number(record.get("consecutiveFailures")?)?;
    if failures < 0.0 {
        return None;
    }
    for key in ["lastSuccessAt", "nextAttemptAt", "accessExpiresAt"] {
        if let Some(value) = record.get(key)
            && !is_iso(value)
        {
            return None;
        }
    }
    let iso_field = |key: &str| -> Option<String> {
        record
            .get(key)
            .filter(|v| is_iso(v))
            .and_then(Value::as_str)
            .map(str::to_owned)
    };
    Some(CodexRenewalRecord {
        auth_version,
        outcome,
        last_attempt_at,
        consecutive_failures: failures,
        last_success_at: iso_field("lastSuccessAt"),
        next_attempt_at: iso_field("nextAttemptAt"),
        access_expires_at: iso_field("accessExpiresAt"),
    })
}

/// `readCodexRenewalRecords`: missing or invalid files read as empty history;
/// the next write replaces them.
pub fn read_codex_renewal_records(file: &Path) -> CodexRenewalRecords {
    let Ok(stat) = std::fs::symlink_metadata(file) else {
        return Vec::new();
    };
    if !stat.is_file() || stat.len() > MAX_STATUS_BYTES {
        return Vec::new();
    }
    let Ok(bytes) = std::fs::read(file) else {
        return Vec::new();
    };
    let Ok(parsed) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    let version_ok = parsed
        .get("version")
        .and_then(Value::as_f64)
        .is_some_and(|v| v == 1.0);
    if !version_ok {
        return Vec::new();
    }
    let Some(profiles) = parsed.get("profiles").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut records = Vec::new();
    for (name, value) in profiles {
        if let Some(record) = valid_record(value) {
            records.push((name.clone(), record));
        }
    }
    records
}

/// One global queue serializes read-modify-write passes. TS serializes per
/// file; the service writes one history file per root, and a cross-root pass
/// waiting its turn changes no outcome (the history is advisory).
static UPDATE_QUEUE: Mutex<()> = Mutex::new(());

/// `updateCodexRenewalRecords`: read-modify-write under the queue, pruning
/// profiles outside `keep`, names sorted, atomic 0600 replace.
pub fn update_codex_renewal_records(
    file: &Path,
    keep: Option<&[String]>,
    update: impl FnOnce(&mut CodexRenewalRecords),
) -> Result<(), ()> {
    let _guard = UPDATE_QUEUE.lock().map_err(|_| ())?;
    let mut records = read_codex_renewal_records(file);
    update(&mut records);
    let mut sorted: BTreeMap<&str, &CodexRenewalRecord> = BTreeMap::new();
    for (name, record) in &records {
        if keep.is_none_or(|keep| keep.iter().any(|k| k == name)) {
            sorted.insert(name.as_str(), record);
        }
    }
    let mut profiles = Map::new();
    for (name, record) in sorted {
        profiles.insert(name.to_owned(), record.to_json());
    }
    let mut doc = Map::new();
    doc.insert("version".into(), Value::Number(1.into()));
    doc.insert("profiles".into(), Value::Object(profiles));
    let text = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(doc)).map_err(|_| ())?
    );
    if let Some(parent) = file.parent() {
        create_private_dir(parent)?;
    }
    replace_file_atomically(file, text.as_bytes())
}

/// `fs.mkdirSync(dir, { recursive: true, mode: 0o700 })`.
pub fn create_private_dir(dir: &Path) -> Result<(), ()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|_| ())
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir).map_err(|_| ())
    }
}

/// What one renewal attempt concluded, as the history records it.
#[derive(Debug, Clone)]
pub struct CodexRenewalAttemptOutcome {
    /// The outcome code.
    pub reason: CodexRenewalReason,
    /// True only when a refresh request was sent.
    pub attempted: bool,
    /// Version of the saved file the outcome applies to.
    pub version: Option<String>,
    /// Access expiry the outcome saw, epoch ms.
    pub access_expires_at: Option<i64>,
    /// Whether the renewed tokens kept a session id or sign-in time (log only).
    pub family_markers_kept: Option<bool>,
}

/// Exponential backoff from 15 minutes, doubling to at most 12 hours, +/- 20% jitter.
pub fn codex_renewal_backoff_ms(failures: f64, random: f64) -> i64 {
    let exponent = (failures - 1.0).max(0.0);
    let base = BACKOFF_MAX_MS.min(BACKOFF_BASE_MS * 2f64.powf(exponent));
    js_round(base * (0.8 + 0.4 * random))
}

/// `nextCodexRenewalRecord`: the failure count carries only across the same
/// file version; a renewal clears it; busy and retry outcomes schedule a retry.
pub fn next_codex_renewal_record(
    previous: Option<&CodexRenewalRecord>,
    outcome: &CodexRenewalAttemptOutcome,
    at: i64,
    random: f64,
) -> CodexRenewalRecord {
    let same_version =
        previous.is_some_and(|p| Some(p.auth_version.as_str()) == outcome.version.as_deref());
    let carried = if same_version {
        previous.map(|p| p.consecutive_failures).unwrap_or(0.0)
    } else {
        0.0
    };
    let failures = if outcome.reason == CodexRenewalReason::Renewed {
        0.0
    } else if outcome.attempted {
        carried + 1.0
    } else {
        carried
    };
    let next_attempt_at = if outcome.reason == CodexRenewalReason::ActivationBusy {
        Some(at + CODEX_RENEWAL_BUSY_RETRY_MS)
    } else if is_retry_reason(outcome.reason) {
        Some(at + codex_renewal_backoff_ms(failures, random))
    } else {
        None
    };
    let last_success_at = if outcome.reason == CodexRenewalReason::Renewed {
        iso(at)
    } else {
        previous.and_then(|p| p.last_success_at.clone())
    };
    CodexRenewalRecord {
        auth_version: outcome.version.clone().unwrap_or_else(|| "missing".into()),
        outcome: outcome.reason.code().to_owned(),
        last_attempt_at: iso(at).unwrap_or_default(),
        consecutive_failures: failures,
        last_success_at,
        next_attempt_at: next_attempt_at.and_then(iso),
        access_expires_at: outcome.access_expires_at.and_then(iso),
    }
}

/// Whether a recorded backoff still applies at `now`.
pub fn backoff_active(record: &CodexRenewalRecord, now: i64) -> bool {
    record
        .next_attempt_at
        .as_deref()
        .and_then(parse_date_ms)
        .is_some_and(|retry| retry > now)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CodexRenewalReason;

    fn outcome(
        reason: CodexRenewalReason,
        attempted: bool,
        version: Option<&str>,
    ) -> CodexRenewalAttemptOutcome {
        CodexRenewalAttemptOutcome {
            reason,
            attempted,
            version: version.map(str::to_owned),
            access_expires_at: None,
            family_markers_kept: None,
        }
    }

    #[test]
    fn backoff_doubles_and_jitters_like_the_ts_store() {
        // random 0.5 -> factor 1.0 exactly.
        assert_eq!(codex_renewal_backoff_ms(1.0, 0.5), 15 * 60_000);
        assert_eq!(codex_renewal_backoff_ms(2.0, 0.5), 30 * 60_000);
        assert_eq!(codex_renewal_backoff_ms(3.0, 0.5), 60 * 60_000);
        assert_eq!(codex_renewal_backoff_ms(40.0, 0.5), 12 * 60 * 60_000);
        assert_eq!(codex_renewal_backoff_ms(1.0, 0.0), 12 * 60_000);
        assert_eq!(codex_renewal_backoff_ms(1.0, 1.0), 18 * 60_000);
    }

    #[test]
    fn failure_count_carries_only_on_the_same_version() {
        let at = 1_791_460_800_000;
        let first = next_codex_renewal_record(
            None,
            &outcome(CodexRenewalReason::Transient, true, Some("v1")),
            at,
            0.5,
        );
        assert_eq!(first.consecutive_failures, 1.0);
        assert_eq!(
            first.next_attempt_at.as_deref().and_then(parse_date_ms),
            Some(at + 15 * 60_000)
        );
        let second = next_codex_renewal_record(
            Some(&first),
            &outcome(CodexRenewalReason::Transient, true, Some("v1")),
            at,
            0.5,
        );
        assert_eq!(second.consecutive_failures, 2.0);
        let changed = next_codex_renewal_record(
            Some(&second),
            &outcome(CodexRenewalReason::Transient, true, Some("v2")),
            at,
            0.5,
        );
        assert_eq!(changed.consecutive_failures, 1.0);
        let renewed = next_codex_renewal_record(
            Some(&changed),
            &outcome(CodexRenewalReason::Renewed, true, Some("v2")),
            at,
            0.5,
        );
        assert_eq!(renewed.consecutive_failures, 0.0);
        assert_eq!(renewed.last_success_at, iso(at));
        assert!(renewed.next_attempt_at.is_none());
        let busy = next_codex_renewal_record(
            None,
            &outcome(CodexRenewalReason::ActivationBusy, false, Some("v1")),
            at,
            0.5,
        );
        assert_eq!(busy.consecutive_failures, 0.0);
        assert_eq!(
            busy.next_attempt_at.as_deref().and_then(parse_date_ms),
            Some(at + CODEX_RENEWAL_BUSY_RETRY_MS)
        );
    }

    #[test]
    fn invalid_history_reads_as_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join(CODEX_RENEWAL_STATUS_FILE);
        assert!(read_codex_renewal_records(&file).is_empty());
        std::fs::write(&file, "{\"version\":2,\"profiles\":{}}").expect("write");
        assert!(read_codex_renewal_records(&file).is_empty());
        std::fs::write(
            &file,
            "{\"version\":1,\"profiles\":{\"a\":{\"authVersion\":\"v\",\"outcome\":\"dead\",\"lastAttemptAt\":\"2026-10-08T12:00:00.000Z\",\"consecutiveFailures\":1,\"nextAttemptAt\":null}}}",
        )
        .expect("write");
        // A present-but-null optional field invalidates the whole record.
        assert!(read_codex_renewal_records(&file).is_empty());
    }

    #[test]
    fn update_sorts_prunes_and_appends_a_newline() {
        let dir = tempfile::tempdir().expect("tempdir");
        let file = dir.path().join(CODEX_RENEWAL_STATUS_FILE);
        let at = 1_791_460_800_000;
        update_codex_renewal_records(
            &file,
            Some(&["zeta".to_owned(), "alpha".to_owned()]),
            |records| {
                set_record(
                    records,
                    "zeta",
                    next_codex_renewal_record(
                        None,
                        &outcome(CodexRenewalReason::Renewed, true, Some("v")),
                        at,
                        0.5,
                    ),
                );
                set_record(
                    records,
                    "ghost",
                    next_codex_renewal_record(
                        None,
                        &outcome(CodexRenewalReason::Dead, true, Some("v")),
                        at,
                        0.5,
                    ),
                );
                set_record(
                    records,
                    "alpha",
                    next_codex_renewal_record(
                        None,
                        &outcome(CodexRenewalReason::Transient, true, Some("v")),
                        at,
                        0.5,
                    ),
                );
            },
        )
        .expect("update");
        let text = std::fs::read_to_string(&file).expect("read");
        assert!(text.ends_with("}\n"));
        let parsed: Value = serde_json::from_str(&text).expect("json");
        let keys: Vec<&String> = parsed["profiles"]
            .as_object()
            .expect("object")
            .keys()
            .collect();
        assert_eq!(keys, vec!["alpha", "zeta"]);
    }
}
