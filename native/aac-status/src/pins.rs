//! T3's pinned threads, read from T3's own state database.
//!
//! The database is T3's, not AAC's, and its shape can change without notice, so this reader
//! is strict: it opens the file read-only (`SQLITE_OPEN_READ_ONLY`) with a busy timeout,
//! never writes, never holds a transaction, checks the table and payload shape it relies
//! on, and reports `shape` ("T3's state file changed shape") instead of guessing when
//! anything differs. Only id, title, pinnedAt and pinOrderKey leave this module.

use crate::error::{Error, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use serde::Serialize;
use serde_json::Value as Json;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// T3's projection table for threads. Part of the expected shape, not a setting.
pub const TABLE: &str = "orchestration_v2_projection_threads";

/// Every column this reader relies on; a file without any one of them has changed shape.
const REQUIRED_COLUMNS: [&str; 5] = ["thread_id", "title", "archived_at", "deleted_at", "payload_json"];

/// How long a read waits behind T3's writer before giving up as unreadable.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// One pinned top-level thread. `pinned_at` and `pin_order_key` pass through with the JSON
/// type T3 wrote (text in practice); anything that is not a string or a number is a shape
/// change, never a value to invent a meaning for.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedThread {
    pub id: String,
    pub title: String,
    pub pinned_at: Json,
    pub pin_order_key: Json,
}

/// The database to read: an explicit path first, then `AAC_T3_STATE_DB`, then
/// `~/.t3/userdata/statev2.sqlite`. Blank values count as unset.
pub fn resolve_db_path(explicit: Option<&str>, env: Option<String>, home: Option<PathBuf>) -> Option<PathBuf> {
    for value in [explicit.map(str::to_string), env].into_iter().flatten() {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            return Some(PathBuf::from(trimmed));
        }
    }
    home.map(|dir| dir.join(".t3").join("userdata").join("statev2.sqlite"))
}

/// `resolve_db_path` over this process's environment.
pub fn default_db_path(explicit: Option<&str>) -> Option<PathBuf> {
    let env = std::env::var("AAC_T3_STATE_DB").ok();
    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|value| !value.is_empty()))
        .map(PathBuf::from);
    resolve_db_path(explicit, env, home)
}

fn missing() -> Error {
    Error::new("missing", "T3's state file is missing")
}

fn shape() -> Error {
    Error::new("shape", "T3's state file changed shape")
}

/// An SQLite failure while reading: a busy or locked file could not be read; anything else
/// (a missing table or column, malformed JSON in a payload, a failed JSON function) means
/// the file no longer has the shape this reader was written against.
fn map_sqlite(error: rusqlite::Error) -> Error {
    if let rusqlite::Error::SqliteFailure(ffi, _) = &error {
        use rusqlite::ffi::ErrorCode;
        if matches!(ffi.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked) {
            return Error::new("io", "T3's state file could not be read");
        }
    }
    shape()
}

/// A payload scalar as JSON. Only text and numbers are values this reader passes on.
fn scalar(value: SqlValue) -> Result<Json> {
    match value {
        SqlValue::Text(text) => Ok(Json::String(text)),
        SqlValue::Integer(number) => Ok(Json::Number(number.into())),
        SqlValue::Real(number) => serde_json::Number::from_f64(number).map(Json::Number).ok_or_else(shape),
        SqlValue::Null | SqlValue::Blob(_) => Err(shape()),
    }
}

/// Every pinned top-level thread in `path`, in T3's pin order. Error codes: `missing` when
/// there is no file, `shape` when the table or a payload differs from the expected shape,
/// `io` when the file could not be read.
pub fn read_pinned_threads(path: &Path) -> Result<Vec<PinnedThread>> {
    if !path.is_file() {
        return Err(missing());
    }
    let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|_| Error::new("io", "T3's state file could not be read"))?;
    conn.busy_timeout(BUSY_TIMEOUT)
        .map_err(|_| Error::new("io", "T3's state file could not be read"))?;
    // Belt and braces behind the read-only open: this connection can never write.
    conn.pragma_update(None, "query_only", true)
        .map_err(|_| Error::new("io", "T3's state file could not be read"))?;

    let columns: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .map_err(map_sqlite)?;
        let rows = stmt
            .query_map([TABLE], |row| row.get::<_, String>(0))
            .map_err(map_sqlite)?;
        let mut names = Vec::new();
        for row in rows {
            names.push(row.map_err(map_sqlite)?);
        }
        names
    };
    if REQUIRED_COLUMNS
        .iter()
        .any(|need| !columns.iter().any(|have| have == need))
    {
        return Err(shape());
    }

    // `pinnedAt` is present-but-null on unpinned threads, so "set" is a non-null extract;
    // the same extract is null when `lineage.parentThreadId` is null or missing entirely,
    // which is exactly the top-level rule. The statement is dropped before the connection,
    // so no read transaction outlives this call.
    let mut stmt = conn
        .prepare(
            "SELECT thread_id, title, \
                    json_extract(payload_json, '$.pinnedAt'), \
                    json_type(payload_json, '$.pinnedAt'), \
                    json_extract(payload_json, '$.pinOrderKey'), \
                    json_type(payload_json, '$.pinOrderKey'), \
                    json_type(payload_json) \
             FROM orchestration_v2_projection_threads \
             WHERE archived_at IS NULL AND deleted_at IS NULL \
               AND json_extract(payload_json, '$.pinnedAt') IS NOT NULL \
               AND json_extract(payload_json, '$.lineage.parentThreadId') IS NULL \
             ORDER BY json_extract(payload_json, '$.pinOrderKey') ASC, thread_id ASC",
        )
        .map_err(map_sqlite)?;
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, SqlValue>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, SqlValue>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
            ))
        })
        .map_err(map_sqlite)?;

    let mut pinned = Vec::new();
    for row in rows {
        let (id, title, pinned_at, pinned_type, pin_order_key, order_type, root_type) = match row {
            Ok(values) => values,
            // A value SQLite cannot give this reader as the column's type is not T3's shape.
            Err(rusqlite::Error::InvalidColumnType(..)) => return Err(shape()),
            Err(error) => return Err(map_sqlite(error)),
        };
        if root_type.as_deref() != Some("object")
            || !matches!(pinned_type.as_deref(), Some("text" | "integer" | "real"))
            || !matches!(order_type.as_deref(), Some("text" | "integer" | "real"))
        {
            return Err(shape());
        }
        pinned.push(PinnedThread {
            id,
            title,
            pinned_at: scalar(pinned_at)?,
            pin_order_key: scalar(pin_order_key)?,
        });
    }
    Ok(pinned)
}
