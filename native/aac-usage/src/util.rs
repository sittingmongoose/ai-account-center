//! Rules shared by every reader, each identical to the Python helper's
//! (`scripts/analytics-remote/analytics_usage_remote.py`), so a computer can move from one helper to
//! the other without its cached rows and fingerprints changing.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

pub const MODEL_MAX_LEN: usize = 160;
pub const PROVIDER_MAX_LEN: usize = 64;
pub const SESSION_MAX_LEN: usize = 160;
pub const SESSION_KEY_PREFIX: &str = "aac-session-v1:";

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut text = String::with_capacity(64);
    for byte in digest.iter() {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// A path as text, the way Python's `str(path)` spells it on this OS.
pub fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Python's `os.path.normpath` for an absolute path: `.` dropped, `..` applied, separators collapsed.
pub fn normpath(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                let root = out.parent().is_none() || out.as_os_str().is_empty();
                if !root {
                    out.pop();
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Python's `os.path.abspath`.
pub fn abspath(path: &Path) -> PathBuf {
    if path.is_absolute() {
        normpath(path)
    } else {
        normpath(&std::env::current_dir().unwrap_or_default().join(path))
    }
}

/// Python's `os.path.realpath`: links resolved; Windows' verbatim `\\?\` prefix dropped.
pub fn realpath(path: &Path) -> PathBuf {
    // Under a mount that did not answer, nothing is resolved (FIX-ROUND-BIG 9).
    if crate::fsx::avoided(path) {
        return abspath(path);
    }
    match fs::canonicalize(path) {
        Ok(real) => strip_verbatim(real),
        Err(_) => {
            // realpath of a missing path resolves what exists and keeps the rest.
            let mut existing = path.to_path_buf();
            let mut rest = Vec::new();
            while !existing.as_os_str().is_empty() {
                if let Ok(real) = fs::canonicalize(&existing) {
                    let mut out = strip_verbatim(real);
                    for piece in rest.iter().rev() {
                        out.push(piece);
                    }
                    return normpath(&out);
                }
                match (existing.file_name().map(|name| name.to_os_string()), existing.parent()) {
                    (Some(name), Some(parent)) => {
                        rest.push(name);
                        existing = parent.to_path_buf();
                    }
                    _ => break,
                }
            }
            abspath(path)
        }
    }
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    let text = path.to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        return PathBuf::from(format!(r"\\{rest}"));
    }
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        return PathBuf::from(rest);
    }
    path
}

/// A file's key: sha256(kind NUL absolute path). A compressed Codex rollout keeps its plain owner.
pub fn filekey(kind: &str, path: &Path) -> String {
    let absolute = path_text(&abspath(path));
    let owner = if kind == "codex" && absolute.ends_with(".jsonl.zst") {
        &absolute[..absolute.len() - 4]
    } else {
        absolute.as_str()
    };
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0u8]);
    digest.update(owner.as_bytes());
    let mut text = String::with_capacity(64);
    for byte in digest.finalize().iter() {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// A file key that names what a file holds (a Codex conversation, a Muse session) rather than where
/// it lies (FIX-ROUND-BIG 1 and 2): every copy, and the file after a move, keep the same rows.
pub fn filekey_text(kind: &str, text: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0u8]);
    digest.update(text.as_bytes());
    let mut out = String::with_capacity(64);
    for byte in digest.finalize().iter() {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Python's `int(stat.st_mtime * 1000)`: seconds as a double, then milliseconds truncated.
pub fn mtime_ms(meta: &fs::Metadata) -> i64 {
    (mtime_seconds(meta) * 1000.0) as i64
}

pub fn mtime_seconds(meta: &fs::Metadata) -> f64 {
    match meta.modified() {
        Ok(time) => match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => duration.as_secs() as f64 + duration.subsec_nanos() as f64 * 1e-9,
            Err(error) => {
                let duration = error.duration();
                -(duration.as_secs() as f64 + duration.subsec_nanos() as f64 * 1e-9)
            }
        },
        Err(_) => 0.0,
    }
}

/// Nanoseconds since the epoch (Python's `st_mtime_ns`).
pub fn mtime_ns(meta: &fs::Metadata) -> i128 {
    match meta.modified() {
        Ok(time) => match time.duration_since(std::time::UNIX_EPOCH) {
            Ok(duration) => duration.as_nanos() as i128,
            Err(error) => -(error.duration().as_nanos() as i128),
        },
        Err(_) => 0,
    }
}

/// A file's fingerprint: size, mtime and digests of its first and last 256 bytes.
#[derive(Clone, Debug, PartialEq)]
pub struct Print {
    pub size: u64,
    pub mtime_ms: i64,
    pub head: String,
    pub tail: String,
    pub claude_name: Option<String>,
    pub claude_mirror: Option<bool>,
    pub claude_group: Option<String>,
    pub usage_skipped: Option<bool>,
    pub wal_size: Option<u64>,
    pub wal_mtime_ms: Option<i64>,
    /// Byte offset up to which this file's per-message records were sent (DESIGN 11.1a).
    pub msg_through: Option<u64>,
    /// Digest of the file's first min(msgThrough, 256) bytes when they were sent: a file that only
    /// grew keeps them, a rewritten one does not.
    pub msg_head: Option<String>,
    /// How far a read that the deadline cut got, and where this answer's rows for the file start
    /// (DESIGN 16.1). None for a file read whole in one call.
    pub progress: Option<ReadProgress>,
    /// Records (Cursor runs, Devin replies) this database's last read could not count: an unchanged
    /// database says so again (FIX-ROUND-BIG 5).
    pub unreadable_records: Option<u64>,
    /// The last read was immutable while the write-ahead log held rows: said again while unchanged.
    pub wal_unread: Option<bool>,
    /// The newest measured usage event in this file, retained through unchanged scans. None
    /// means no event was measured, not a zero-token event. `usage_clock` versions this evidence.
    pub newest_usage_ms: Option<i64>,
    pub usage_clock: bool,
    pub path_key: Option<String>,
    pub file_identity: Option<String>,
    pub append_state: Option<Value>,
}

/// A resumable read's place in one file (DESIGN 16.1). `through` is the decoded byte offset after
/// the last complete line parsed: None once the file was read to its end. `from` is where this
/// answer's rows for the file start, when they continue an earlier read (the server adds them to
/// the rows it kept). `state` is the reader's carry-over between lines. `name` is the file name
/// without its conversation id, and `decoded` the decoded size when the file says it.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct ReadProgress {
    pub through: Option<u64>,
    pub from: Option<u64>,
    pub state: Option<Value>,
    pub name: Option<String>,
    pub decoded: Option<u64>,
}

/// A log's name for the page (FIX-ROUND-BIG 18): never any part of the file name past what is
/// checked. A Codex rollout is `rollout-<its time>-<session key>` (the contract's 16-hex session
/// key, never the id); any other log is `log-<12 hex of its name>`. The extension is kept.
pub fn display_name(path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let extension = [".jsonl.zst", ".jsonl", ".json", ".db", ".sqlite"]
        .iter()
        .find(|e| name.ends_with(*e))
        .copied()
        .unwrap_or("");
    let time = name
        .strip_prefix("rollout-")
        .and_then(|rest| rest.get(..19))
        .filter(|stamp| {
            stamp.bytes().enumerate().all(|(i, b)| match i {
                4 | 7 | 13 | 16 => b == b'-',
                10 => b == b'T',
                _ => b.is_ascii_digit(),
            })
        });
    if let (Some(time), Some(id)) = (time, crate::read::codex::conversation_id(&name))
        && let Some(key) = session_key("codex", Some(&id))
    {
        return format!("rollout-{time}-{key}{extension}");
    }
    format!(
        "log-{}{extension}",
        &sha256_hex(format!("aac-log-name-v1:{name}").as_bytes())[..12]
    )
}

impl Print {
    pub fn plain(size: u64, mtime_ms: i64, head: String, tail: String) -> Print {
        Print {
            size,
            mtime_ms,
            head,
            tail,
            claude_name: None,
            claude_mirror: None,
            claude_group: None,
            usage_skipped: None,
            wal_size: None,
            wal_mtime_ms: None,
            msg_through: None,
            msg_head: None,
            progress: None,
            unreadable_records: None,
            wal_unread: None,
            newest_usage_ms: None,
            usage_clock: false,
            path_key: None,
            file_identity: None,
            append_state: None,
        }
    }

    pub fn to_json(&self) -> Value {
        let mut map = serde_json::Map::new();
        if self.usage_clock {
            map.insert(
                "newestUsageMs".into(),
                self.newest_usage_ms.map(Value::from).unwrap_or(Value::Null),
            );
        }
        map.insert("size".into(), Value::from(self.size));
        map.insert("mtimeMs".into(), Value::from(self.mtime_ms));
        if !self.head.is_empty() || self.wal_size.is_none() {
            map.insert("head".into(), Value::from(self.head.clone()));
            map.insert("tail".into(), Value::from(self.tail.clone()));
        }
        if let Some(name) = &self.claude_name {
            map.insert("claudeName".into(), Value::from(name.clone()));
        }
        if let Some(mirror) = self.claude_mirror {
            map.insert("claudeMirror".into(), Value::from(mirror));
        }
        if let Some(group) = &self.claude_group {
            map.insert("claudeGroup".into(), Value::from(group.clone()));
        }
        if let Some(skipped) = self.usage_skipped {
            map.insert("usageSkipped".into(), Value::from(skipped));
        }
        if let Some(size) = self.wal_size {
            map.insert("walSize".into(), Value::from(size));
        }
        if let Some(mtime) = self.wal_mtime_ms {
            map.insert("walMtimeMs".into(), Value::from(mtime));
        }
        if let Some(offset) = self.msg_through {
            map.insert("msgThrough".into(), Value::from(offset));
        }
        if let Some(head) = &self.msg_head {
            map.insert("msgHead".into(), Value::from(head.clone()));
        }
        if let Some(count) = self.unreadable_records {
            map.insert("unreadableRecords".into(), Value::from(count));
        }
        if let Some(wal) = self.wal_unread {
            map.insert("walUnread".into(), Value::from(wal));
        }
        if let Some(key) = &self.path_key {
            map.insert("pathKey".into(), json!(key));
        }
        if let Some(identity) = &self.file_identity {
            map.insert("fileIdentity".into(), json!(identity));
        }
        if let Some(state) = &self.append_state {
            map.insert("appendState".into(), state.clone());
        }
        if let Some(progress) = &self.progress {
            if let Some(through) = progress.through {
                map.insert("readThrough".into(), Value::from(through));
            }
            if let Some(from) = progress.from {
                map.insert("readFrom".into(), Value::from(from));
            }
            if let Some(state) = &progress.state {
                map.insert("readState".into(), state.clone());
            }
            if let Some(name) = &progress.name {
                map.insert("readName".into(), Value::from(name.clone()));
            }
            if let Some(decoded) = progress.decoded {
                map.insert("decodedSize".into(), Value::from(decoded));
            }
        }
        Value::Object(map)
    }
}

pub fn fingerprint(path: &Path) -> Option<Print> {
    // Regular files only, never a FIFO or a cloud placeholder (FIX-ROUND-BIG 9 and 15).
    let mut handle = crate::fsx::open_regular(path).ok()?;
    let meta = handle.metadata().ok()?;
    let mut buffer = vec![0u8; 256];
    let read = read_up_to(&mut handle, &mut buffer).ok()?;
    let head = sha256_hex(&buffer[..read]);
    let size = meta.len();
    let tail = if size > 256 {
        handle.seek(SeekFrom::Start(size.saturating_sub(256))).ok()?;
        let read = read_up_to(&mut handle, &mut buffer).ok()?;
        sha256_hex(&buffer[..read])
    } else {
        head.clone()
    };
    Some(Print::plain(size, mtime_ms(&meta), head, tail))
}

fn read_up_to(handle: &mut fs::File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        let read = handle.read(&mut buffer[total..])?;
        if read == 0 {
            break;
        }
        total += read;
    }
    Ok(total)
}

/// Python's `_same_fingerprint` against a prior print sent in the request.
pub fn same_print(prior: Option<&Value>, current: &Print) -> bool {
    let Some(Value::Object(prior)) = prior else {
        return false;
    };
    prior.get("size").and_then(Value::as_u64) == Some(current.size)
        && prior.get("mtimeMs").and_then(Value::as_i64) == Some(current.mtime_ms)
        && prior.get("head").and_then(Value::as_str) == Some(current.head.as_str())
        && prior.get("tail").and_then(Value::as_str) == Some(current.tail.as_str())
}

fn has_control(text: &str) -> bool {
    text.chars().any(|c| (c as u32) < 0x20 || c as u32 == 0x7f)
}

/// Python's `str.strip()`: Unicode whitespace on both ends.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

pub fn clean_text(value: Option<&Value>, max: usize) -> Option<String> {
    let text = py_strip(value?.as_str()?);
    if text.is_empty() || text.chars().count() > max || has_control(text) {
        return None;
    }
    Some(text.to_string())
}

pub fn clean_model(value: Option<&Value>) -> Option<String> {
    clean_text(value, MODEL_MAX_LEN)
}

pub fn clean_provider(value: Option<&Value>) -> Option<String> {
    clean_text(value, PROVIDER_MAX_LEN)
}

/// The published session key: sha256("aac-session-v1:<kind>:<id>")[0:16]; None without an id.
pub fn session_key(kind: &str, session_id: Option<&str>) -> Option<String> {
    session_hash(kind, session_id).map(|hash| hash[..16].to_string())
}

/// The full 64-hex digest of the same namespace (the DB audit's `sessionHash`).
pub fn session_hash(kind: &str, session_id: Option<&str>) -> Option<String> {
    let id = session_id?;
    if id.is_empty() {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(format!("{SESSION_KEY_PREFIX}{kind}:").as_bytes());
    digest.update(id.as_bytes());
    let mut text = String::with_capacity(64);
    for byte in digest.finalize().iter() {
        text.push_str(&format!("{byte:02x}"));
    }
    Some(text)
}

/// The first `max` characters (Python's `text[:max]`).
pub fn prefix_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Python's `_non_negative_number`: a finite number >= 0 that is not a boolean.
pub fn non_negative(value: Option<&Value>) -> Option<f64> {
    let number = match value? {
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    if !number.is_finite() || number < 0.0 {
        None
    } else {
        Some(number)
    }
}

/// Python's `int(float)`: truncation toward zero of a non-negative number.
pub fn int_of(value: f64) -> u64 {
    if value >= 18446744073709551615.0 {
        u64::MAX
    } else {
        value.trunc() as u64
    }
}

// ------------------------------------------------------------------ time

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u64;
    let mp = if month > 2 { month - 3 } else { month + 9 } as u64;
    let doy = (153 * mp + 2) / 5 + day as u64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe as i64 - 719_468
}

/// Python's `_hour_label`: "YYYY-MM-DD HH:00" in UTC, None before the epoch or past now + 1 hour.
pub fn hour_label(epoch_ms: i64, now_ms: i64) -> Option<String> {
    if epoch_ms < 0 || epoch_ms > now_ms + 3_600_000 {
        return None;
    }
    let seconds = epoch_ms / 1000;
    let days = seconds.div_euclid(86_400);
    let hour = seconds.rem_euclid(86_400) / 3600;
    let (year, month, day) = civil_from_days(days);
    if !(0..=9999).contains(&year) {
        return None;
    }
    Some(format!("{year:04}-{month:02}-{day:02} {hour:02}:00"))
}

/// ISO-8601 to epoch ms as Python's `datetime.fromisoformat` reads a log timestamp ("Z" accepted,
/// a missing offset is UTC). Fractions of 1 to 9 digits; None when unusable.
pub fn iso_ms(value: &str) -> Option<i64> {
    let text = py_strip(&value.replace('\u{feff}', "")).to_string();
    let bytes = text.as_bytes();
    if bytes.len() < 10 {
        return None;
    }
    let digits = |from: usize, len: usize| -> Option<i64> {
        let part = text.get(from..from + len)?;
        if part.bytes().all(|b| b.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    let year = digits(0, 4)?;
    if bytes.get(4) != Some(&b'-') || bytes.get(7) != Some(&b'-') {
        return None;
    }
    let month = digits(5, 2)? as u32;
    let day = digits(8, 2)? as u32;
    if !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    let mut index = 10;
    let (mut hour, mut minute, mut second, mut nanos) = (0i64, 0i64, 0i64, 0i64);
    if index < bytes.len() && (bytes[index] == b'T' || bytes[index] == b' ') {
        index += 1;
        hour = digits(index, 2)?;
        index += 2;
        if bytes.get(index) == Some(&b':') {
            minute = digits(index + 1, 2)?;
            index += 3;
            if bytes.get(index) == Some(&b':') {
                second = digits(index + 1, 2)?;
                index += 3;
                if matches!(bytes.get(index), Some(b'.') | Some(b',')) {
                    let start = index + 1;
                    let mut end = start;
                    while end < bytes.len() && bytes[end].is_ascii_digit() {
                        end += 1;
                    }
                    if end == start || end - start > 9 {
                        return None;
                    }
                    let fraction = &text[start..end];
                    nanos = format!("{fraction:0<9}").parse().ok()?;
                    index = end;
                }
            }
        }
        if hour > 23 || minute > 59 || second > 59 {
            return None;
        }
    } else if index != bytes.len() {
        return None;
    }
    let mut offset_seconds = 0i64;
    if index < bytes.len() {
        match bytes[index] {
            b'Z' | b'z' if index + 1 == bytes.len() => {}
            b'+' | b'-' => {
                let sign = if bytes[index] == b'-' { -1 } else { 1 };
                let oh = digits(index + 1, 2)?;
                let mut cursor = index + 3;
                let mut om = 0;
                if bytes.get(cursor) == Some(&b':') {
                    om = digits(cursor + 1, 2)?;
                    cursor += 3;
                } else if cursor + 2 <= bytes.len() && cursor < bytes.len() {
                    om = digits(cursor, 2)?;
                    cursor += 2;
                }
                if cursor != bytes.len() || oh > 23 || om > 59 {
                    return None;
                }
                offset_seconds = sign * (oh * 3600 + om * 60);
            }
            _ => return None,
        }
    }
    let days = days_from_civil(year, month, day);
    let seconds = days * 86_400 + hour * 3600 + minute * 60 + second - offset_seconds;
    // Python: int(moment.timestamp() * 1000), the timestamp a double.
    let timestamp = seconds as f64 + nanos as f64 / 1e9;
    Some((timestamp * 1000.0) as i64)
}

fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        _ => 28,
    }
}

/// Epoch ms as an ISO-8601 UTC string with milliseconds.
pub fn iso_from_ms(epoch_ms: i64) -> String {
    let seconds = epoch_ms.div_euclid(1000);
    let millis = epoch_ms.rem_euclid(1000);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// sha256 of a file's first `length` bytes (at most 256), for `msgHead`.
pub fn head_digest(path: &Path, length: u64) -> Option<String> {
    let mut handle = crate::fsx::open_regular(path).ok()?;
    let mut buffer = vec![0u8; length.min(256) as usize];
    let read = read_up_to(&mut handle, &mut buffer).ok()?;
    if read != buffer.len() {
        return None;
    }
    Some(sha256_hex(&buffer))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hour_labels_match_python() {
        let now = 4_102_444_800_000;
        assert_eq!(hour_label(1_790_867_100_000, now).as_deref(), Some("2026-10-01 15:00"));
        assert_eq!(hour_label(-1, now), None);
        assert_eq!(hour_label(now + 3_600_001, now), None);
    }

    #[test]
    fn iso_timestamps_read_as_python_reads_them() {
        assert_eq!(iso_ms("2026-10-01T15:05:00Z"), Some(1_790_867_100_000));
        assert_eq!(iso_ms("2026-10-01T15:05:00.123Z"), Some(1_790_867_100_123));
        assert_eq!(iso_ms("2026-10-01T17:05:00+02:00"), Some(1_790_867_100_000));
        assert_eq!(iso_ms("2026-10-01T15:05:00"), Some(1_790_867_100_000));
        assert_eq!(iso_ms("2026-10-01"), Some(1_790_812_800_000));
        assert_eq!(iso_ms("yesterday"), None);
        assert_eq!(iso_ms("2026-02-30T00:00:00Z"), None);
    }

    #[test]
    fn session_keys_use_the_published_namespace() {
        // sha256("aac-session-v1:omp:abc")
        let full = session_hash("omp", Some("abc")).unwrap();
        assert_eq!(full.len(), 64);
        assert_eq!(session_key("omp", Some("abc")).unwrap(), full[..16]);
        assert_eq!(session_key("omp", Some("")), None);
    }

    #[test]
    fn iso_round_trip() {
        assert_eq!(iso_from_ms(1_790_867_100_123), "2026-10-01T15:05:00.123Z");
    }
}
