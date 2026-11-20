//! Claude Code sign-in renewal reader
//! (`src/web-server/services/claude-signin-renewal.ts`): the runner on the
//! computer with the browser and the person publishes `claude-signins.json`
//! into the usage snapshot folder after every run. This reads that file, keeps
//! only allowlisted fields and marks it stale when the runner stopped writing.
//! Anything shaped like a link or a token is refused, so the dashboard can
//! never echo one.

use crate::jsnum::{iso, js_number, parse_date_ms};
use crate::planner::insert_opt;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};

/// The published file name.
pub const RENEWAL_FILE: &str = "claude-signins.json";
const MAX_BYTES: u64 = 512 * 1024;
const CACHE_MS: i64 = 30 * 1000;
const MAX_SETTINGS_BYTES: u64 = 64 * 1024;

static NAME: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$").expect("static"));
static EMAIL: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[^\s@/\\]{1,64}@[^\s@/\\]{1,190}$").expect("static"));
static WORD: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^[a-z][a-z0-9_]{0,39}$").expect("static"));
static ISO: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d{1,3})?Z$").expect("static")
});
static SECRETISH: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"https?:|[A-Za-z0-9_-]{20,}#|sk-ant|[A-Za-z0-9_-]{40,}").expect("static")
});
static RUNNER_VERSION: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"^\d{1,3}\.\d{1,3}\.\d{1,3}$").expect("static"));

/// The fixed runner state words.
pub const RENEWAL_STATES: [&str; 6] = [
    "ok",
    "logged_out",
    "no_refresh_token",
    "no_end_date",
    "unreadable",
    "not_signed_in",
];

fn str_pattern(value: Option<&Value>, pattern: &regex::Regex) -> Option<String> {
    let text = value?.as_str()?;
    if pattern.is_match(text) {
        Some(text.to_owned())
    } else {
        None
    }
}

fn time(value: Option<&Value>) -> Option<String> {
    str_pattern(value, &ISO)
}

fn plain(value: Option<&Value>, max: usize) -> Option<String> {
    let text = value?.as_str()?;
    let len = text.chars().count();
    if len == 0 || len > max || SECRETISH.is_match(text) {
        return None;
    }
    if text
        .chars()
        .any(|c| ('\u{0}'..='\u{1f}').contains(&c) || c == '\u{7f}')
    {
        return None;
    }
    Some(text.to_owned())
}

fn email_of(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    str_pattern(Some(&Value::String(text.to_lowercase())), &EMAIL)
}

fn flag(value: Option<&Value>) -> Option<bool> {
    value.and_then(Value::as_bool)
}

/// One sign-in row, allowlisted fields only.
#[derive(Debug, Clone)]
pub struct ClaudeSignInRenewalRow {
    /// The computer name.
    pub host: String,
    /// The Claude profile name.
    pub profile: String,
    /// The account email, lowercased.
    pub email: Option<String>,
    /// A fixed state word.
    pub state: String,
    /// When the sign-in ends, ISO.
    pub sign_in_ends_at: Option<String>,
    /// When the logout happened, ISO.
    pub logout_at: Option<String>,
    /// When the runner starts renewing this sign-in by itself.
    pub renew_from: Option<String>,
    /// Whether the runner renews automatically.
    pub auto_renew: bool,
    /// Last renewal time, ISO.
    pub last_renew_at: Option<String>,
    /// A fixed result word.
    pub last_result: Option<String>,
    /// Next verification time, ISO.
    pub next_verify_at: Option<String>,
    /// Retry time, ISO.
    pub retry_after: Option<String>,
    /// How often an older Claude process wrote its old sign-in back over a
    /// renewal (capped at 999).
    pub reverts: f64,
    /// When the runner read this row, ISO.
    pub read_at: Option<String>,
}

impl ClaudeSignInRenewalRow {
    /// The row in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("host".into(), Value::String(self.host.clone()));
        out.insert("profile".into(), Value::String(self.profile.clone()));
        insert_opt(&mut out, "email", &self.email);
        out.insert("state".into(), Value::String(self.state.clone()));
        insert_opt(&mut out, "signInEndsAt", &self.sign_in_ends_at);
        insert_opt(&mut out, "logoutAt", &self.logout_at);
        insert_opt(&mut out, "renewFrom", &self.renew_from);
        out.insert("autoRenew".into(), Value::Bool(self.auto_renew));
        insert_opt(&mut out, "lastRenewAt", &self.last_renew_at);
        insert_opt(&mut out, "lastResult", &self.last_result);
        insert_opt(&mut out, "nextVerifyAt", &self.next_verify_at);
        insert_opt(&mut out, "retryAfter", &self.retry_after);
        if let Some(n) = js_number(self.reverts) {
            out.insert("reverts".into(), Value::Number(n));
        }
        insert_opt(&mut out, "readAt", &self.read_at);
        Value::Object(out)
    }
}

fn row(raw: &Value) -> Option<ClaudeSignInRenewalRow> {
    let record = raw.as_object()?;
    let host = str_pattern(record.get("host"), &NAME)?;
    let profile = str_pattern(record.get("profile"), &NAME)?;
    let state = record.get("state")?.as_str()?;
    if !RENEWAL_STATES.contains(&state) {
        return None;
    }
    let reverts = record
        .get("reverts")
        .and_then(crate::jsnum::is_integer_number)
        .filter(|n| *n >= 0.0)
        .map(|n| n.min(999.0))
        .unwrap_or(0.0);
    Some(ClaudeSignInRenewalRow {
        host,
        profile,
        email: email_of(record.get("email")),
        state: state.to_owned(),
        sign_in_ends_at: time(record.get("signInEndsAt")),
        logout_at: time(record.get("logoutAt")),
        renew_from: time(record.get("renewFrom")),
        auto_renew: record.get("autoRenew") != Some(&Value::Bool(false)),
        last_renew_at: time(record.get("lastRenewAt")),
        last_result: str_pattern(record.get("lastResult"), &WORD),
        next_verify_at: time(record.get("nextVerifyAt")),
        retry_after: time(record.get("retryAfter")),
        reverts,
        read_at: time(record.get("readAt")),
    })
}

/// One thing the runner needs a person for.
#[derive(Debug, Clone)]
pub struct ClaudeSignInNeed {
    /// The account email, lowercased.
    pub email: String,
    /// captcha, mail_link_missing, wrong_account, browser_signin_failed,
    /// renewal_failing.
    pub reason: String,
    /// One plain sentence for the person: what to do and where.
    pub how: String,
    /// Since when, ISO.
    pub since: Option<String>,
}

impl ClaudeSignInNeed {
    /// The need in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("email".into(), Value::String(self.email.clone()));
        out.insert("reason".into(), Value::String(self.reason.clone()));
        out.insert("how".into(), Value::String(self.how.clone()));
        insert_opt(&mut out, "since", &self.since);
        Value::Object(out)
    }
}

fn need(raw: &Value) -> Option<ClaudeSignInNeed> {
    let record = raw.as_object()?;
    let email = email_of(record.get("email"))?;
    let reason = str_pattern(record.get("reason"), &WORD)?;
    Some(ClaudeSignInNeed {
        email,
        reason,
        how: plain(record.get("how"), 400).unwrap_or_default(),
        since: time(record.get("since")),
    })
}

/// One computer's read outcome.
#[derive(Debug, Clone)]
pub struct ClaudeRenewalHost {
    /// The computer name.
    pub host: String,
    /// When the runner read it, ISO.
    pub read_at: Option<String>,
    /// A bounded plain error, when the read failed.
    pub error: Option<String>,
}

impl ClaudeRenewalHost {
    /// The host in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("host".into(), Value::String(self.host.clone()));
        insert_opt(&mut out, "readAt", &self.read_at);
        insert_opt(&mut out, "error", &self.error);
        Value::Object(out)
    }
}

/// One account's claude.ai session in the Chrome profile that approves its
/// renewals. Once one has ended, that account's next renewal needs a person
/// for one CAPTCHA.
#[derive(Debug, Clone)]
pub struct ClaudeBrowserSession {
    /// The account email, lowercased.
    pub email: String,
    /// Whether the session is signed in, when measured.
    pub signed_in: Option<bool>,
    /// When the session ends, ISO.
    pub session_ends_at: Option<String>,
    /// When it was checked, ISO.
    pub checked_at: Option<String>,
    /// Whether a visit to claude.ai moved the session end later; null until
    /// measured.
    pub visit_extends: Option<bool>,
}

impl ClaudeBrowserSession {
    /// The session in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("email".into(), Value::String(self.email.clone()));
        out.insert(
            "signedIn".into(),
            self.signed_in.map(Value::Bool).unwrap_or(Value::Null),
        );
        insert_opt(&mut out, "sessionEndsAt", &self.session_ends_at);
        insert_opt(&mut out, "checkedAt", &self.checked_at);
        out.insert(
            "visitExtends".into(),
            self.visit_extends.map(Value::Bool).unwrap_or(Value::Null),
        );
        Value::Object(out)
    }
}

/// The runner's latest published state.
#[derive(Debug, Clone)]
pub struct ClaudeSignInRenewal {
    /// The runner computer name.
    pub runner: String,
    /// The runner version.
    pub runner_version: String,
    /// Publication time, ISO.
    pub generated_at: String,
    /// The runner has not written for longer than it promised.
    pub stale: bool,
    /// Fixed diagnostic for a runner that could not start its browser helper.
    pub browser_error: Option<String>,
    /// The renewal lead the runner works with, hours.
    pub renew_within_hours: f64,
    /// What needs a person.
    pub needs_you: Vec<ClaudeSignInNeed>,
    /// Per-computer read outcomes.
    pub hosts: Vec<ClaudeRenewalHost>,
    /// The sign-in rows.
    pub sign_ins: Vec<ClaudeSignInRenewalRow>,
    /// The approving browser sessions.
    pub browser_sessions: Vec<ClaudeBrowserSession>,
}

impl ClaudeSignInRenewal {
    /// The document in the TS key order (browserError first, when present).
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        if let Some(error) = &self.browser_error {
            out.insert("browserError".into(), Value::String(error.clone()));
        }
        out.insert("runner".into(), Value::String(self.runner.clone()));
        out.insert(
            "runnerVersion".into(),
            Value::String(self.runner_version.clone()),
        );
        out.insert(
            "generatedAt".into(),
            Value::String(self.generated_at.clone()),
        );
        out.insert("stale".into(), Value::Bool(self.stale));
        if let Some(n) = js_number(self.renew_within_hours) {
            out.insert("renewWithinHours".into(), Value::Number(n));
        }
        out.insert(
            "needsYou".into(),
            Value::Array(self.needs_you.iter().map(|n| n.to_json()).collect()),
        );
        out.insert(
            "hosts".into(),
            Value::Array(self.hosts.iter().map(|h| h.to_json()).collect()),
        );
        out.insert(
            "signIns".into(),
            Value::Array(self.sign_ins.iter().map(|r| r.to_json()).collect()),
        );
        out.insert(
            "browserSessions".into(),
            Value::Array(self.browser_sessions.iter().map(|s| s.to_json()).collect()),
        );
        Value::Object(out)
    }
}

fn list(value: Option<&Value>, limit: usize) -> Vec<Value> {
    value
        .and_then(Value::as_array)
        .map(|items| items.iter().take(limit).cloned().collect())
        .unwrap_or_default()
}

/// `parseClaudeSignInRenewal`: None when it is not a renewal file this version
/// understands.
pub fn parse_claude_signin_renewal(text: &str, now: i64) -> Option<ClaudeSignInRenewal> {
    let doc: Value = serde_json::from_str(text).ok()?;
    let doc = doc.as_object()?;
    let generated_at = time(doc.get("generatedAt"))?;
    let version_ok = doc.get("version").and_then(Value::as_f64) == Some(1.0);
    if !version_ok {
        return None;
    }
    let stale_after = doc
        .get("staleAfterSeconds")
        .and_then(Value::as_f64)
        .filter(|s| *s >= 600.0)
        .map(|s| s.min(86400.0))
        .unwrap_or(3600.0);
    let generated_ms = parse_date_ms(&generated_at)?;
    let stale = now - generated_ms > (stale_after * 1000.0) as i64;
    let browser_error = match doc.get("browserError").and_then(Value::as_str) {
        Some("no_node") => Some("Node.js executable not found on the renewal runner.".to_owned()),
        Some("node_launch_failed") => {
            Some("Node.js could not be launched on the renewal runner.".to_owned())
        }
        _ => None,
    };
    let renew_within_hours = doc
        .get("renewWithinHours")
        .and_then(Value::as_f64)
        .filter(|h| *h > 0.0 && *h <= 720.0)
        .unwrap_or(96.0);
    let needs_you = list(doc.get("needsYou"), 32)
        .iter()
        .filter_map(need)
        .collect();
    let hosts = list(doc.get("hosts"), 16)
        .iter()
        .filter_map(|raw| {
            let empty = Map::new();
            let record = raw.as_object().unwrap_or(&empty);
            let host = str_pattern(record.get("host"), &NAME)?;
            Some(ClaudeRenewalHost {
                host,
                read_at: time(record.get("readAt")),
                error: plain(record.get("error"), 160),
            })
        })
        .collect();
    let sign_ins = list(doc.get("signIns"), 128)
        .iter()
        .filter_map(row)
        .collect();
    let browser_sessions = list(doc.get("browserSessions"), 32)
        .iter()
        .filter_map(|raw| {
            let empty = Map::new();
            let record = raw.as_object().unwrap_or(&empty);
            let email = email_of(record.get("email"))?;
            Some(ClaudeBrowserSession {
                email,
                signed_in: flag(record.get("signedIn")),
                session_ends_at: time(record.get("sessionEndsAt")),
                checked_at: time(record.get("checkedAt")),
                visit_extends: flag(record.get("visitExtends")),
            })
        })
        .collect();
    Some(ClaudeSignInRenewal {
        runner: str_pattern(doc.get("runner"), &NAME).unwrap_or_else(|| "unknown".into()),
        runner_version: str_pattern(doc.get("runnerVersion"), &RUNNER_VERSION)
            .unwrap_or_else(|| "unknown".into()),
        generated_at,
        stale,
        browser_error,
        renew_within_hours,
        needs_you,
        hosts,
        sign_ins,
        browser_sessions,
    })
}

/// The snapshot folder setting (`agent-usage-snapshot.json`), as the reader
/// resolves it: a private regular file, version 1, an absolute folder without
/// control characters or `..` segments.
pub fn snapshot_directory(ccs_dir: &Path) -> Option<PathBuf> {
    let file = ccs_dir.join("agent-usage-snapshot.json");
    let link = std::fs::symlink_metadata(&file).ok()?;
    if link.file_type().is_symlink() || !link.is_file() || link.len() > MAX_SETTINGS_BYTES {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if link.mode() & 0o077 != 0 {
            return None;
        }
    }
    let bytes = std::fs::read(&file).ok()?;
    if bytes.len() as u64 > MAX_SETTINGS_BYTES {
        return None;
    }
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let record = value.as_object()?;
    if record.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let directory = record.get("directory")?.as_str()?;
    if !is_snapshot_directory(directory) {
        return None;
    }
    Some(PathBuf::from(directory))
}

/// `isSnapshotDirectory`: an absolute folder path without control characters
/// or `..` segments.
pub fn is_snapshot_directory(value: &str) -> bool {
    let len = value.chars().count();
    if len <= 1 || len > 1024 {
        return false;
    }
    if value
        .chars()
        .any(|c| ('\u{0}'..='\u{1f}').contains(&c) || c == '\u{7f}')
    {
        return false;
    }
    let absolute = if cfg!(windows) {
        let bytes = value.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && (bytes[2] == b'/' || bytes[2] == b'\\'))
            || value.starts_with('/')
            || value.starts_with('\\')
    } else {
        value.starts_with('/')
    };
    if !absolute {
        return false;
    }
    !value.split(['/', '\\']).any(|segment| segment == "..")
}

/// The file-text seam for the reader.
pub type ReadFileSeam = Arc<dyn Fn(&Path) -> Result<String, ()> + Send + Sync>;

/// The injectable reader seams.
#[derive(Clone, Default)]
pub struct RenewalReaderDeps {
    /// The snapshot folder seam (None reads the setting file).
    pub folder: Option<Arc<dyn Fn() -> Option<PathBuf> + Send + Sync>>,
    /// The file text seam.
    pub read_file: Option<ReadFileSeam>,
    /// Epoch ms clock.
    pub now: Option<Arc<dyn Fn() -> i64 + Send + Sync>>,
}

/// The runner's latest published state, or None when no runner publishes into
/// the snapshot folder. Uninjected reads cache for 30 seconds.
pub struct ClaudeSignInRenewalReader {
    ccs_dir: PathBuf,
    deps: RenewalReaderDeps,
    cache: Mutex<Option<(i64, Option<ClaudeSignInRenewal>)>>,
}

impl ClaudeSignInRenewalReader {
    /// A reader for one CCS root.
    pub fn new(ccs_dir: PathBuf, deps: RenewalReaderDeps) -> Self {
        Self {
            ccs_dir,
            deps,
            cache: Mutex::new(None),
        }
    }

    fn now(&self) -> i64 {
        match &self.deps.now {
            Some(now) => now(),
            None => crate::renewal::system_now_ms(),
        }
    }

    /// Read the latest published state (cached for 30 s without seams).
    pub fn read(&self) -> Option<ClaudeSignInRenewal> {
        let now = self.now();
        let injected = self.deps.folder.is_some() || self.deps.read_file.is_some();
        if !injected {
            let cache = self.cache.lock().expect("renewal reader cache");
            if let Some((at, value)) = cache.as_ref()
                && now - at < CACHE_MS
            {
                return value.clone();
            }
        }
        let value = self.read_uncached(now);
        if !injected {
            *self.cache.lock().expect("renewal reader cache") = Some((now, value.clone()));
        }
        value
    }

    fn read_uncached(&self, now: i64) -> Option<ClaudeSignInRenewal> {
        let folder = match &self.deps.folder {
            Some(seam) => seam()?,
            None => snapshot_directory(&self.ccs_dir)?,
        };
        let file = folder.join(RENEWAL_FILE);
        let text = match &self.deps.read_file {
            Some(seam) => seam(&file).ok()?,
            None => {
                let stat = std::fs::metadata(&file).ok()?;
                if !stat.is_file() || stat.len() > MAX_BYTES {
                    String::new()
                } else {
                    let bytes = std::fs::read(&file).ok()?;
                    String::from_utf8_lossy(&bytes).into_owned()
                }
            }
        };
        parse_claude_signin_renewal(&text, now)
    }

    /// Drop the cache (the TS invalidate hook).
    pub fn invalidate(&self) {
        *self.cache.lock().expect("renewal reader cache") = None;
    }
}

/// The fixed generated-at spelling for parity exports.
pub fn generated_at_iso(ms: i64) -> Option<String> {
    iso(ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: i64 = 1_793_707_200_000; // 2026-11-03T12:00:00Z

    fn at(ms: i64) -> String {
        iso(ms).expect("iso").replace(".000Z", "Z")
    }

    fn file() -> Value {
        json!({
            "version": 1,
            "runner": "mac",
            "runnerVersion": "1.0.0",
            "generatedAt": at(NOW - 5 * 60 * 1000),
            "staleAfterSeconds": 3600,
            "renewWithinHours": 96,
            "needsYou": [
                {"email": "A@Example.com", "reason": "captcha", "how": "Solve the CAPTCHA.", "since": at(NOW)}
            ],
            "hosts": [
                {"host": "mac", "readAt": at(NOW), "error": null},
                {"host": "windows", "readAt": at(NOW - 3_600_000), "error": "ssh_timeout"}
            ],
            "signIns": [
                {"host": "mac", "profile": "gmail", "email": "a@example.com", "state": "ok",
                 "signInEndsAt": at(NOW + 40 * 24 * 3_600_000), "renewFrom": at(NOW + 36 * 24 * 3_600_000),
                 "autoRenew": true, "lastRenewAt": at(NOW - 3_600_000), "lastResult": "renewed",
                 "nextVerifyAt": at(NOW + 8 * 3_600_000), "retryAfter": null, "reverts": 0, "readAt": at(NOW)},
                {"host": "windows", "profile": "gmail", "email": "a@example.com", "state": "ok", "lastResult": "end_not_moved"}
            ]
        })
    }

    #[test]
    fn allowlist_lowercase_and_defaults() {
        let parsed = parse_claude_signin_renewal(&file().to_string(), NOW).expect("parsed");
        assert!(!parsed.stale);
        assert_eq!(parsed.needs_you[0].email, "a@example.com");
        assert_eq!(parsed.sign_ins[1].sign_in_ends_at, None);
        assert!(parsed.sign_ins[1].auto_renew);
        assert_eq!(parsed.sign_ins[1].reverts, 0.0);
        assert_eq!(parsed.runner, "mac");
    }

    #[test]
    fn stale_runner_and_refusals() {
        let mut stale = file();
        stale["generatedAt"] = json!(at(NOW - 2 * 3_600_000));
        assert!(
            parse_claude_signin_renewal(&stale.to_string(), NOW)
                .expect("parsed")
                .stale
        );
        let mut version = file();
        version["version"] = json!(2);
        assert!(parse_claude_signin_renewal(&version.to_string(), NOW).is_none());
        assert!(parse_claude_signin_renewal("{", NOW).is_none());
    }

    #[test]
    fn secretish_how_and_bad_rows_drop() {
        let mut doc = file();
        doc["needsYou"] = json!([
            {"email": "a@example.com", "reason": "captcha", "how": "open https://claude.ai/magic-link#x"},
            {"email": "b@example.com", "reason": "captcha", "how": format!("type {}#{}", "A".repeat(24), "B".repeat(24))},
            {"email": "not-an-email", "reason": "captcha"}
        ]);
        doc["signIns"] = json!([
            {"host": "../etc", "profile": "p", "state": "ok"},
            {"host": "mac", "profile": "p", "state": "weird"}
        ]);
        let parsed = parse_claude_signin_renewal(&doc.to_string(), NOW).expect("parsed");
        assert_eq!(
            parsed
                .needs_you
                .iter()
                .map(|n| n.how.as_str())
                .collect::<Vec<_>>(),
            vec!["", ""]
        );
        assert!(parsed.sign_ins.is_empty());
        assert!(!parsed.to_json().to_string().contains("magic-link"));
    }

    #[test]
    fn browser_sessions_keep_ends_never_values() {
        let mut doc = file();
        doc["browserSessions"] = json!([
            {"email": "a@example.com", "signedIn": true, "sessionEndsAt": at(NOW + 28 * 24 * 3_600_000), "visitExtends": false, "value": "sk"}
        ]);
        let parsed = parse_claude_signin_renewal(&doc.to_string(), NOW).expect("parsed");
        assert_eq!(parsed.browser_sessions.len(), 1);
        let session = &parsed.browser_sessions[0];
        assert_eq!(session.signed_in, Some(true));
        assert_eq!(session.visit_extends, Some(false));
        assert_eq!(session.checked_at, None);
        assert!(!parsed.to_json().to_string().contains("\"value\""));
    }

    #[test]
    fn browser_error_maps_only_the_fixed_words() {
        for (word, expected) in [
            (
                "no_node",
                "Node.js executable not found on the renewal runner.",
            ),
            (
                "node_launch_failed",
                "Node.js could not be launched on the renewal runner.",
            ),
        ] {
            let mut doc = file();
            doc["browserError"] = json!(word);
            let parsed = parse_claude_signin_renewal(&doc.to_string(), NOW).expect("parsed");
            assert_eq!(parsed.browser_error.as_deref(), Some(expected));
        }
        let mut doc = file();
        doc["browserError"] = json!("sk-ant-fixture-not-for-the-dashboard");
        let parsed = parse_claude_signin_renewal(&doc.to_string(), NOW).expect("parsed");
        assert!(parsed.browser_error.is_none());
    }

    #[test]
    fn reader_seams_and_cache() {
        let reader = ClaudeSignInRenewalReader::new(
            PathBuf::from("/nonexistent-ccs"),
            RenewalReaderDeps {
                folder: Some(Arc::new(|| None)),
                ..Default::default()
            },
        );
        assert!(reader.read().is_none());
        let text = file().to_string();
        let reader = ClaudeSignInRenewalReader::new(
            PathBuf::from("/nonexistent-ccs"),
            RenewalReaderDeps {
                folder: Some(Arc::new(|| Some(PathBuf::from("/share")))),
                read_file: Some(Arc::new(move |file: &Path| {
                    assert_eq!(file, Path::new("/share").join(RENEWAL_FILE));
                    Ok(text.clone())
                })),
                now: Some(Arc::new(|| NOW)),
            },
        );
        assert_eq!(reader.read().expect("read").runner, "mac");
    }

    #[test]
    fn snapshot_directory_rules() {
        assert!(is_snapshot_directory("/share/aac-usage"));
        assert!(!is_snapshot_directory("relative/path"));
        assert!(!is_snapshot_directory("/share/../etc"));
        assert!(!is_snapshot_directory("/"));
        let dir = tempfile::tempdir().expect("tempdir");
        assert!(snapshot_directory(dir.path()).is_none());
        std::fs::write(
            dir.path().join("agent-usage-snapshot.json"),
            serde_json::json!({"version": 1, "directory": "/share/aac-usage"}).to_string(),
        )
        .expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                dir.path().join("agent-usage-snapshot.json"),
                std::fs::Permissions::from_mode(0o600),
            )
            .expect("mode");
            assert_eq!(
                snapshot_directory(dir.path()),
                Some(PathBuf::from("/share/aac-usage"))
            );
        }
    }
}
