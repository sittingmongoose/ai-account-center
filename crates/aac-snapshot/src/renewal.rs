//! The Rust renewal job's published state: the server writes `claude-signins.json` into the
//! snapshot folder after every run; this reads it, keeps only allowlisted fields and marks it
//! stale when the job stopped writing. Anything shaped like a link or a token is refused, so
//! the dashboard can never echo one.

use crate::js::{js_whitespace, member, utf16_len};
use aac_files::read::{self, Read, ReadPolicy};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::Mutex;

/// The renewal job's file in the snapshot folder.
pub const RENEWAL_FILE: &str = "claude-signins.json";
const MAX_BYTES: u64 = 512 * 1024;
const CACHE_MS: i64 = 30 * 1000;
const RENEWAL_STATES: [&str; 6] = [
    "ok",
    "logged_out",
    "no_refresh_token",
    "no_end_date",
    "unreadable",
    "not_signed_in",
];

/// `NAME`: an identifier start, then up to 63 identifier characters.
fn name(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let bytes = s.as_bytes();
    if !(1..=64).contains(&bytes.len()) || !bytes[0].is_ascii_alphanumeric() {
        return None;
    }
    if !bytes[1..]
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(b))
    {
        return None;
    }
    Some(s.to_owned())
}

/// `WORD`: a lowercase state word.
fn word(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let bytes = s.as_bytes();
    if !(1..=40).contains(&bytes.len()) || !bytes[0].is_ascii_lowercase() {
        return None;
    }
    if !bytes[1..]
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
    {
        return None;
    }
    Some(s.to_owned())
}

/// `EMAIL`: local@domain, neither part carrying whitespace, @, / or \.
fn email(value: &Value) -> Option<String> {
    let s = value.as_str()?.to_lowercase();
    let (local, domain) = s.split_once('@')?;
    let part = |text: &str, max: usize| {
        let len = text.chars().count();
        (1..=max).contains(&len)
            && !text
                .chars()
                .any(|c| js_whitespace(c) || c == '@' || c == '/' || c == '\\')
    };
    if part(local, 64) && part(domain, 190) && !domain.contains('@') {
        Some(s)
    } else {
        None
    }
}

/// `ISO`: the exact published time form, `YYYY-MM-DDTHH:MM:SS(.f..fff)Z`.
fn time(value: &Value) -> Option<String> {
    let s = value.as_str()?;
    let b = s.as_bytes();
    let digits = |from: usize, len: usize| {
        b.get(from..from + len)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    if b.len() < 20
        || !digits(0, 4)
        || b[4] != b'-'
        || !digits(5, 2)
        || b[7] != b'-'
        || !digits(8, 2)
        || b[10] != b'T'
        || !digits(11, 2)
        || b[13] != b':'
        || !digits(14, 2)
        || b[16] != b':'
        || !digits(17, 2)
    {
        return None;
    }
    let mut at = 19;
    if b.get(at) == Some(&b'.') {
        let start = at + 1;
        let mut end = start;
        while end < b.len() && b[end].is_ascii_digit() && end - start < 3 {
            end += 1;
        }
        if end == start || end - start > 3 {
            return None;
        }
        at = end;
    }
    if b.get(at) == Some(&b'Z') && at + 1 == b.len() {
        Some(s.to_owned())
    } else {
        None
    }
}

/// `SECRETISH`: link shapes, long token runs and the Anthropic key prefix.
fn secretish(s: &str) -> bool {
    if s.contains("http:") || s.contains("https:") || s.contains("sk-ant") {
        return true;
    }
    let token = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    let bytes = s.as_bytes();
    let mut run = 0usize;
    for (at, byte) in bytes.iter().enumerate() {
        if token(*byte) {
            run += 1;
            if run >= 40 {
                return true;
            }
            if run >= 20 && bytes.get(at + 1) == Some(&b'#') {
                return true;
            }
        } else {
            run = 0;
        }
    }
    false
}

/// `plain`: bounded text without secrets or control characters.
fn plain(value: &Value, max: usize) -> Option<String> {
    let s = value.as_str()?;
    let len = utf16_len(s);
    if len == 0
        || len > max
        || secretish(s)
        || s.chars()
            .any(|c| matches!(c, '\u{0000}'..='\u{001f}' | '\u{007f}'))
    {
        return None;
    }
    Some(s.to_owned())
}

fn list(value: &Value, limit: usize) -> Vec<Value> {
    value
        .as_array()
        .map(|items| items.iter().take(limit).cloned().collect())
        .unwrap_or_default()
}

fn need(raw: &Value) -> Option<Value> {
    if !raw.is_object() {
        return None;
    }
    let email = email(member(raw, "email"))?;
    let reason = word(member(raw, "reason"))?;
    Some(json!({
        "email": email,
        "reason": reason,
        "how": plain(member(raw, "how"), 400).unwrap_or_default(),
        "since": time(member(raw, "since")).map(Value::String).unwrap_or(Value::Null),
    }))
}

fn row(raw: &Value) -> Option<Value> {
    if !raw.is_object() {
        return None;
    }
    let host = name(member(raw, "host"))?;
    let profile = name(member(raw, "profile"))?;
    let state = RENEWAL_STATES
        .iter()
        .find(|state| member(raw, "state").as_str() == Some(*state))?;
    let reverts = member(raw, "reverts")
        .as_f64()
        .filter(|r| r.is_finite() && r.fract() == 0.0 && *r >= 0.0);
    Some(json!({
        "host": host,
        "profile": profile,
        "email": email(member(raw, "email")).map(Value::String).unwrap_or(Value::Null),
        "state": state,
        "signInEndsAt": time(member(raw, "signInEndsAt")).map(Value::String).unwrap_or(Value::Null),
        "logoutAt": time(member(raw, "logoutAt")).map(Value::String).unwrap_or(Value::Null),
        "renewFrom": time(member(raw, "renewFrom")).map(Value::String).unwrap_or(Value::Null),
        "autoRenew": member(raw, "autoRenew") != &json!(false),
        "lastRenewAt": time(member(raw, "lastRenewAt")).map(Value::String).unwrap_or(Value::Null),
        "lastResult": word(member(raw, "lastResult")).map(Value::String).unwrap_or(Value::Null),
        "nextVerifyAt": time(member(raw, "nextVerifyAt")).map(Value::String).unwrap_or(Value::Null),
        "retryAfter": time(member(raw, "retryAfter")).map(Value::String).unwrap_or(Value::Null),
        "reverts": reverts.map(|r| (r as i64).min(999)).unwrap_or(0),
        "readAt": time(member(raw, "readAt")).map(Value::String).unwrap_or(Value::Null),
    }))
}

/// `parseClaudeSignInRenewal`: one published file, or None when it is not a renewal file this
/// version understands.
pub fn parse_claude_sign_in_renewal(text: &str, now: i64) -> Option<Value> {
    let doc: Value = serde_json::from_str(text).ok()?;
    if !doc.is_object() {
        return None;
    }
    let generated_at = time(member(&doc, "generatedAt"))?;
    if member(&doc, "version").as_f64() != Some(1.0) {
        return None;
    }
    let stale_after = member(&doc, "staleAfterSeconds")
        .as_f64()
        .filter(|seconds| *seconds >= 600.0)
        .map(|seconds| seconds.min(86_400.0))
        .unwrap_or(3600.0);
    let browser_error = match member(&doc, "browserError").as_str() {
        Some("no_node") => Some("Node.js executable not found on the renewal runner."),
        Some("node_launch_failed") => Some("Node.js could not be launched on the renewal runner."),
        _ => None,
    };
    let generated_ms = crate::js::date_parse_str(&generated_at)?;
    let stale = (now - generated_ms) as f64 > stale_after * 1000.0;
    let renew_within = member(&doc, "renewWithinHours")
        .as_f64()
        .filter(|hours| *hours > 0.0 && *hours <= 720.0)
        .unwrap_or(96.0);
    let runner_version = member(&doc, "runnerVersion").as_str().filter(|v| {
        let parts: Vec<&str> = v.split('.').collect();
        parts.len() == 3
            && parts.iter().all(|part| {
                (1..=3).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit())
            })
    });
    let needs_you: Vec<Value> = list(member(&doc, "needsYou"), 32)
        .iter()
        .filter_map(need)
        .collect();
    let hosts: Vec<Value> = list(member(&doc, "hosts"), 16)
        .iter()
        .filter_map(|host| {
            let name = name(member(host, "host"))?;
            Some(json!({
                "host": name,
                "readAt": time(member(host, "readAt")).map(Value::String).unwrap_or(Value::Null),
                "error": plain(member(host, "error"), 160).map(Value::String).unwrap_or(Value::Null),
            }))
        })
        .collect();
    let sign_ins: Vec<Value> = list(member(&doc, "signIns"), 128)
        .iter()
        .filter_map(row)
        .collect();
    let browser_sessions: Vec<Value> = list(member(&doc, "browserSessions"), 32)
        .iter()
        .filter_map(|session| {
            let email = email(member(session, "email"))?;
            let flag = |value: &Value| value.as_bool().map(Value::Bool).unwrap_or(Value::Null);
            Some(json!({
                "email": email,
                "signedIn": flag(member(session, "signedIn")),
                "sessionEndsAt": time(member(session, "sessionEndsAt")).map(Value::String).unwrap_or(Value::Null),
                "checkedAt": time(member(session, "checkedAt")).map(Value::String).unwrap_or(Value::Null),
                "visitExtends": flag(member(session, "visitExtends")),
            }))
        })
        .collect();
    let mut out = serde_json::Map::new();
    if let Some(error) = browser_error {
        out.insert("browserError".into(), json!(error));
    }
    out.insert(
        "runner".into(),
        json!(name(member(&doc, "runner")).unwrap_or_else(|| "unknown".into())),
    );
    out.insert(
        "runnerVersion".into(),
        json!(runner_version.unwrap_or("unknown")),
    );
    out.insert("generatedAt".into(), json!(generated_at));
    out.insert("stale".into(), json!(stale));
    out.insert("renewWithinHours".into(), crate::js::number(renew_within));
    out.insert("needsYou".into(), Value::Array(needs_you));
    out.insert("hosts".into(), Value::Array(hosts));
    out.insert("signIns".into(), Value::Array(sign_ins));
    out.insert("browserSessions".into(), Value::Array(browser_sessions));
    Some(Value::Object(out))
}

/// `readClaudeSignInRenewal`'s file read: a regular file under the cap, else empty text (which
/// parses to None); any read failure is None.
pub fn read_renewal_file(file: &Path) -> Option<String> {
    match read::read(file, &ReadPolicy::capped(MAX_BYTES)) {
        Read::Bytes(bytes, _) => String::from_utf8(bytes).ok(),
        _ => None,
    }
}

/// The 30-second cached reader the dashboard deploys: one read per half minute, best effort.
#[derive(Default)]
pub struct RenewalReader {
    cache: Mutex<Option<(i64, Option<Value>)>>,
}

impl RenewalReader {
    /// A fresh reader.
    pub fn new() -> Self {
        Self::default()
    }

    /// The runner's latest published state in the folder, or None without one.
    pub fn read(&self, folder: Option<&Path>, now: i64) -> Option<Value> {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, value)) = cache.as_ref()
            && now - at < CACHE_MS
        {
            return value.clone();
        }
        let value = folder.and_then(|folder| {
            let text = read_renewal_file(&folder.join(RENEWAL_FILE))?;
            parse_claude_sign_in_renewal(&text, now)
        });
        *cache = Some((now, value.clone()));
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_065_600_000;

    #[test]
    fn parses_a_published_file() {
        let text = json!({
            "version": 1,
            "runner": "mac",
            "runnerVersion": "1.2.3",
            "generatedAt": "2026-10-15T11:50:00.000Z",
            "staleAfterSeconds": 3600,
            "renewWithinHours": 96,
            "needsYou": [{"email": "A@Example.com", "reason": "captcha", "how": "Solve the CAPTCHA.", "since": "2026-10-15T08:00:00Z"}],
            "hosts": [{"host": "mac", "readAt": "2026-10-15T11:50:00Z", "error": null}],
            "signIns": [{"host": "mac", "profile": "party", "email": "a@example.com", "state": "ok",
                          "signInEndsAt": "2026-11-12T12:00:00Z", "autoRenew": true, "reverts": 2}],
            "browserSessions": []
        }).to_string();
        let parsed = parse_claude_sign_in_renewal(&text, NOW).expect("parse");
        assert_eq!(parsed["runner"], json!("mac"));
        assert_eq!(parsed["stale"], json!(false));
        assert_eq!(parsed["needsYou"][0]["email"], json!("a@example.com"));
        assert_eq!(parsed["signIns"][0]["reverts"], json!(2));
        assert_eq!(parsed["signIns"][0]["autoRenew"], json!(true));
    }

    #[test]
    fn marks_a_stopped_runner_stale() {
        let text = json!({"version": 1, "generatedAt": "2026-10-15T09:00:00.000Z"}).to_string();
        let parsed = parse_claude_sign_in_renewal(&text, NOW).expect("parse");
        assert_eq!(parsed["stale"], json!(true));
        assert_eq!(parsed["runner"], json!("unknown"));
        assert_eq!(parsed["renewWithinHours"].as_f64(), Some(96.0));
    }

    #[test]
    fn refuses_secrets_and_bad_versions() {
        assert!(parse_claude_sign_in_renewal("not json", NOW).is_none());
        assert!(parse_claude_sign_in_renewal("[]", NOW).is_none());
        let text = json!({"version": 2, "generatedAt": "2026-10-15T11:50:00.000Z"}).to_string();
        assert!(parse_claude_sign_in_renewal(&text, NOW).is_none());
        let text = json!({"version": 1, "generatedAt": "2026-10-15T11:50:00.000Z",
                          "needsYou": [{"email": "a@example.com", "reason": "captcha", "how": "visit https://evil.example"}]}).to_string();
        let parsed = parse_claude_sign_in_renewal(&text, NOW).expect("parse");
        assert_eq!(parsed["needsYou"][0]["how"], json!(""));
        assert!(!secretish("plain words"));
        assert!(secretish("sk-ant-abc"));
        assert!(secretish(&format!("{}#", "a".repeat(20))));
        assert!(secretish(&"a".repeat(40)));
    }

    #[test]
    fn time_screen_is_exact() {
        assert!(time(&json!("2026-10-15T11:50:00Z")).is_some());
        assert!(time(&json!("2026-10-15T11:50:00.123Z")).is_some());
        assert!(time(&json!("2026-10-15T11:50:00.1234Z")).is_none());
        assert!(time(&json!("2026-10-15T11:50:00+02:00")).is_none());
        assert!(time(&json!("2026-10-15")).is_none());
    }
}
