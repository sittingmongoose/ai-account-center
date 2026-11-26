//! Bounded, read-only SQLite queries for OMP `agent.db` and Cursor `state.vscdb`.
use crate::failover_additional::desktop::ReadTracker;
use rusqlite::{Connection, OpenFlags, types::ValueRef};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    io::ErrorKind,
    path::Path,
    time::{Duration, Instant},
};

const MAX_OMP_BYTES: usize = 1024 * 1024;
const MAX_AGY_OMP_BYTES: usize = 65_536;

/// One decoded OMP `auth_credentials` row for `failover_additional`.
#[derive(Debug, Clone)]
pub(crate) struct OmpAdditionalRow {
    pub row_id: String,
    pub credential_type: Option<String>,
    pub data: Value,
}

/// One raw OMP `auth_credentials` row for `failover_special` (Antigravity).
#[derive(Debug, Clone)]
pub(crate) enum OmpRawRow {
    Valid { row_id: i64, raw: String },
    Unusable,
}

fn open_readonly(
    path: &Path,
    timeout_ms: u64,
    deadline_secs: u64,
    reject_symlink: bool,
) -> Result<Option<Connection>, ()> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
            return Ok(None);
        }
        Err(_) => return Err(()),
    };
    if reject_symlink && meta.file_type().is_symlink() {
        return Ok(None);
    }
    if !meta.is_file() {
        // When symlinks are allowed (desktop_helpers.sqlite_rows), follow metadata to check is_file.
        if !reject_symlink && meta.file_type().is_symlink() {
            match fs::metadata(path) {
                Ok(m) if m.is_file() => {}
                Ok(_) => return Ok(None),
                Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                    return Ok(None);
                }
                Err(_) => return Err(()),
            }
        } else {
            return Ok(None);
        }
    }
    let resolved = path.canonicalize().map_err(|_| ())?;
    let conn = Connection::open_with_flags(
        resolved,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|_| ())?;
    conn.busy_timeout(Duration::from_millis(timeout_ms))
        .map_err(|_| ())?;
    conn.execute_batch("PRAGMA query_only=ON;")
        .map_err(|_| ())?;
    let deadline = Instant::now() + Duration::from_secs(deadline_secs);
    conn.progress_handler(1000, Some(move || Instant::now() >= deadline));
    Ok(Some(conn))
}

/// Read up to `max_rows` enabled OMP rows for `provider` (`failover_additional._omp`).
pub(crate) fn omp_additional_rows(
    home: &Path,
    provider: &str,
    max_rows: usize,
    tracker: &mut ReadTracker,
) -> Vec<OmpAdditionalRow> {
    let path = home.join(".omp/agent/agent.db");
    let conn = match open_readonly(&path, 300, 2, true) {
        Ok(Some(c)) => c,
        Ok(None) => return Vec::new(),
        Err(()) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT rowid,credential_type,data FROM auth_credentials WHERE provider=?1 \
         AND disabled_cause IS NULL ORDER BY rowid DESC LIMIT ?2",
    ) {
        Ok(s) => s,
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let rows = match stmt.query_map(rusqlite::params![provider, max_rows as i64], |row| {
        let id = row.get::<_, i64>(0)?;
        let kind = match row.get_ref(1)? {
            ValueRef::Text(b) => std::str::from_utf8(b).ok().map(str::to_owned),
            _ => None,
        };
        let raw = match row.get_ref(2)? {
            ValueRef::Text(b) => std::str::from_utf8(b).ok().map(str::to_owned),
            _ => None,
        };
        Ok((id, kind, raw))
    }) {
        Ok(iter) => iter,
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in rows {
        let Ok((id, kind, raw)) = entry else {
            tracker.record_failure();
            return Vec::new();
        };
        let parsed = raw
            .filter(|s| s.chars().count() <= MAX_OMP_BYTES)
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(Value::is_object);
        if let Some(data) = parsed {
            out.push(OmpAdditionalRow {
                row_id: id.to_string(),
                credential_type: kind,
                data,
            });
        } else {
            // A stored credential that cannot be decoded is not a removed sign-in.
            tracker.record_failure();
        }
    }
    out
}

/// Read up to 16 enabled `google-antigravity` OAuth rows (`failover_special._discover`).
pub(crate) fn omp_antigravity_rows(database: &Path, tracker: &mut ReadTracker) -> Vec<OmpRawRow> {
    let conn = match open_readonly(database, 2000, 3, true) {
        Ok(Some(c)) => c,
        Ok(None) => return Vec::new(),
        Err(()) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT rowid,data FROM auth_credentials WHERE provider=?1 AND credential_type='oauth' \
         AND disabled_cause IS NULL ORDER BY rowid DESC LIMIT 16",
    ) {
        Ok(s) => s,
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let rows = match stmt.query_map(["google-antigravity"], |row| {
        let id = match row.get_ref(0)? {
            ValueRef::Integer(n) => Some(n),
            _ => None,
        };
        let raw = match row.get_ref(1)? {
            ValueRef::Text(b) => std::str::from_utf8(b).ok().map(str::to_owned),
            _ => None,
        };
        Ok((id, raw))
    }) {
        Ok(iter) => iter,
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in rows {
        let Ok((id, raw)) = entry else {
            tracker.record_failure();
            return Vec::new();
        };
        match (id, raw) {
            (Some(row_id), Some(raw)) if raw.len() <= MAX_AGY_OMP_BYTES => {
                out.push(OmpRawRow::Valid { row_id, raw });
            }
            _ => out.push(OmpRawRow::Unusable),
        }
    }
    out
}

/// Read Cursor's `ItemTable` keys from `state.vscdb` (`failover_additional._desktop_candidates`).
pub(crate) fn cursor_vscdb_rows(
    database: &Path,
    tracker: &mut ReadTracker,
) -> BTreeMap<String, Value> {
    let conn = match open_readonly(database, 2000, 3, false) {
        Ok(Some(c)) => c,
        Ok(None) => return BTreeMap::new(),
        Err(()) => {
            tracker.record_failure();
            return BTreeMap::new();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT key,value FROM ItemTable WHERE key IN \
         ('cursorAuth/accessToken','cursorAuth/cachedEmail','cursorAuth/stripeMembershipType') \
         LIMIT 16",
    ) {
        Ok(s) => s,
        Err(_) => {
            tracker.record_failure();
            return BTreeMap::new();
        }
    };
    let rows = match stmt.query_map([], |row| {
        let key = match row.get_ref(0)? {
            ValueRef::Text(b) => std::str::from_utf8(b).ok().map(str::to_owned),
            _ => None,
        };
        let val = match row.get_ref(1)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(i) => Value::from(i),
            ValueRef::Real(f) => serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            ValueRef::Text(b) => std::str::from_utf8(b)
                .map(|s| Value::String(s.to_owned()))
                .unwrap_or(Value::Null),
            ValueRef::Blob(_) => Value::Null,
        };
        Ok((key, val))
    }) {
        Ok(iter) => iter,
        Err(_) => {
            tracker.record_failure();
            return BTreeMap::new();
        }
    };
    let mut out = BTreeMap::new();
    for entry in rows {
        let Ok((key, val)) = entry else {
            tracker.record_failure();
            return BTreeMap::new();
        };
        if let Some(k) = key {
            out.insert(k, val);
        }
    }
    out
}

/// Read up to 8 enabled OMP OAuth rows for standalone desktop collectors (`desktop_helpers.omp_credentials`).
pub(crate) fn omp_desktop_oauth_rows(
    home: &Path,
    provider: &str,
    tracker: &mut ReadTracker,
) -> Vec<Value> {
    let path = home.join(".omp/agent/agent.db");
    let conn = match open_readonly(&path, 2000, 3, false) {
        Ok(Some(c)) => c,
        Ok(None) => return Vec::new(),
        Err(()) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT data FROM auth_credentials WHERE provider=?1 AND credential_type='oauth' \
         AND disabled_cause IS NULL ORDER BY rowid DESC LIMIT 8",
    ) {
        Ok(s) => s,
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let rows = match stmt.query_map([provider], |row| {
        let raw = match row.get_ref(0)? {
            ValueRef::Text(b) => std::str::from_utf8(b).ok().map(str::to_owned),
            _ => None,
        };
        Ok(raw)
    }) {
        Ok(iter) => iter,
        Err(_) => {
            tracker.record_failure();
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in rows {
        let Ok(raw) = entry else {
            tracker.record_failure();
            return Vec::new();
        };
        let parsed = raw
            .filter(|s| s.chars().count() <= MAX_AGY_OMP_BYTES)
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .filter(Value::is_object);
        if let Some(obj) = parsed {
            out.push(obj);
        } else {
            tracker.record_failure();
        }
    }
    out
}

/// Read all four Cursor desktop keys from `state.vscdb` (`desktop_usage.cursor_credentials`).
pub(crate) fn cursor_desktop_vscdb_rows(
    database: &Path,
    tracker: &mut ReadTracker,
) -> BTreeMap<String, Value> {
    let conn = match open_readonly(database, 2000, 3, false) {
        Ok(Some(c)) => c,
        Ok(None) => return BTreeMap::new(),
        Err(()) => {
            tracker.record_failure();
            return BTreeMap::new();
        }
    };
    let mut stmt = match conn.prepare(
        "SELECT key,value FROM ItemTable WHERE key IN \
         ('cursorAuth/accessToken','cursorAuth/refreshToken','cursorAuth/cachedEmail','cursorAuth/stripeMembershipType') \
         LIMIT 16",
    ) {
        Ok(s) => s,
        Err(_) => {
            tracker.record_failure();
            return BTreeMap::new();
        }
    };
    let rows = match stmt.query_map([], |row| {
        let key = match row.get_ref(0)? {
            ValueRef::Text(b) => std::str::from_utf8(b).ok().map(str::to_owned),
            _ => None,
        };
        let val = match row.get_ref(1)? {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(i) => Value::from(i),
            ValueRef::Real(f) => serde_json::Number::from_f64(f)
                .map(Value::Number)
                .unwrap_or(Value::Null),
            ValueRef::Text(b) => std::str::from_utf8(b)
                .map(|s| Value::String(s.to_owned()))
                .unwrap_or(Value::Null),
            ValueRef::Blob(_) => Value::Null,
        };
        Ok((key, val))
    }) {
        Ok(iter) => iter,
        Err(_) => {
            tracker.record_failure();
            return BTreeMap::new();
        }
    };
    let mut out = BTreeMap::new();
    for entry in rows {
        let Ok((key, val)) = entry else {
            tracker.record_failure();
            return BTreeMap::new();
        };
        if let Some(k) = key {
            out.insert(k, val);
        }
    }
    out
}
