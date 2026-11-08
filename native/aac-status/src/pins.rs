//! T3's pinned threads, read from T3's own state database.
//!
//! The database is T3's, not AAC's, and its shape can change without notice, so this reader
//! is strict: it opens the file read-only (`SQLITE_OPEN_READ_ONLY`) with a busy timeout,
//! never writes, never holds a transaction, checks the table and payload shape it relies
//! on, and reports `shape` ("T3's state file changed shape") instead of guessing when
//! anything differs. Only id, title, pinnedAt and pinOrderKey leave this module.
//!
//! T3 stores each pin in the T3 of the machine that runs the thread, so the same rows are
//! also fetched from the other machines as `sqlite3 -json` text over the dashboard's ssh
//! route and parsed here with the same shape checks (`parse_remote_pins`), then merged
//! across machines (`merge_pins`). This process never opens a remote database itself.

use crate::error::{Error, Result};
use rusqlite::types::Value as SqlValue;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// T3's projection table for threads. Part of the expected shape, not a setting.
pub const TABLE: &str = "orchestration_v2_projection_threads";

/// Every column this reader relies on; a file without any one of them has changed shape.
const REQUIRED_COLUMNS: [&str; 5] = ["thread_id", "title", "archived_at", "deleted_at", "payload_json"];

/// How long a read waits behind T3's writer before giving up as unreadable.
pub const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The machines whose T3 databases can hold pins: this computer's own read plus the three
/// fixed ssh aliases. Part of the shape, not a setting.
pub const PIN_MACHINES: [&str; 3] = ["mac", "windows", "nas1"];

/// One pinned top-level thread. `pinned_at` and `pin_order_key` pass through with the JSON
/// type T3 wrote (text in practice); anything that is not a string or a number is a shape
/// change, never a value to invent a meaning for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PinnedThread {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub pinned_at: Json,
    #[serde(default)]
    pub pin_order_key: Json,
}

/// The `mergePins` method's shape for one machine's good list: the machine's fixed id and
/// its pins as this crate read them (`pins` or `parseRemotePins`).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MachinePinsIn {
    pub machine: String,
    pub pinned: Vec<PinnedThread>,
}

/// One pinned thread with the machine it was read from, as the merged list carries it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MergedPin {
    pub machine: String,
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

/// The columns the remote query returns, and the only keys a row of `sqlite3 -json` output
/// may carry: exactly the SELECT above, aliased, nothing else.
const REMOTE_KEYS: [&str; 7] = [
    "thread_id",
    "title",
    "pinnedAt",
    "pinnedType",
    "pinOrderKey",
    "orderType",
    "rootType",
];

/// One value of the remote output with its `json_type` name, checked against each other the
/// way the local read checks them: a text is a JSON string, an integer a JSON integer, a
/// real a finite JSON fraction. Anything else is a shape change, never a value to reinterpret.
fn remote_scalar(row: &serde_json::Map<String, Json>, value_key: &str, type_key: &str) -> Result<Json> {
    let declared = match row.get(type_key) {
        Some(Json::String(name)) => name.as_str(),
        _ => return Err(shape()),
    };
    let value = row.get(value_key).ok_or_else(shape)?;
    match (value, declared) {
        (Json::String(_), "text") => Ok(value.clone()),
        (number @ Json::Number(_), "integer") if number.as_u64().is_some() || number.as_i64().is_some() => {
            Ok(value.clone())
        }
        (number @ Json::Number(_), "real") if number.as_f64().is_some_and(|fraction| fraction.is_finite()) => {
            Ok(value.clone())
        }
        _ => Err(shape()),
    }
}

/// The `sqlite3 -json` text one remote machine printed for the pinned-threads query, parsed
/// with the same shape checks as the local read (`read_pinned_threads`): an array of row
/// objects carrying exactly the query's aliased columns, a text/integer/real scalar for
/// `pinnedAt` and `pinOrderKey` that matches its declared `json_type`, a payload root that
/// is an object. The query's WHERE clause already applied the same top-level, unarchived,
/// undeleted, pinned predicate, so no row is filtered here. Empty output means no pinned
/// threads on that machine. `machine` is one of [`PIN_MACHINES`]; any deviation is a caller
/// error, not a shape claim.
pub fn parse_remote_pins(machine: &str, text: &str) -> Result<Vec<PinnedThread>> {
    if !PIN_MACHINES.contains(&machine) {
        return Err(Error::new("invalid", "unknown machine for remote pins"));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    let rows: Vec<Json> = serde_json::from_str(trimmed).map_err(|_| shape())?;
    let mut pinned = Vec::new();
    for row in rows {
        let Json::Object(row) = row else {
            return Err(shape());
        };
        if row.len() != REMOTE_KEYS.len() || REMOTE_KEYS.iter().any(|key| !row.contains_key(*key)) {
            return Err(shape());
        }
        let root_type = match row.get("rootType") {
            Some(Json::String(name)) => name.as_str(),
            _ => return Err(shape()),
        };
        if root_type != "object" {
            return Err(shape());
        }
        let (Some(Json::String(id)), Some(Json::String(title))) = (row.get("thread_id"), row.get("title")) else {
            return Err(shape());
        };
        if id.is_empty() || title.is_empty() {
            return Err(shape());
        }
        pinned.push(PinnedThread {
            id: id.clone(),
            title: title.clone(),
            pinned_at: remote_scalar(&row, "pinnedAt", "pinnedType")?,
            pin_order_key: remote_scalar(&row, "pinOrderKey", "orderType")?,
        });
    }
    Ok(pinned)
}

/// Where a pin order key sorts, as SQLite orders it: numbers before text, numbers by value,
/// text by bytes. Two keys of the same kind compare inside the kind; anything else falls
/// back to the machine name and then the thread id, so the merged order is total.
fn order_key_cmp(a: &Json, b: &Json) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let rank = |value: &Json| match value {
        Json::Number(_) => 0,
        _ => 1,
    };
    match (a, b) {
        (Json::Number(left), Json::Number(right)) => {
            left.as_f64().partial_cmp(&right.as_f64()).unwrap_or(Ordering::Equal)
        }
        (Json::String(left), Json::String(right)) => left.cmp(right),
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Every machine's good pins in one list: ordered by `pinOrderKey` (then machine, then
/// thread id), each row tagged with its machine, and de-duplicated by thread id — the first
/// row in that order is the one kept, so a thread pinned on two machines keeps the copy
/// that sorts first, never a blend of the two.
pub fn merge_pins(machines: &[MachinePins]) -> Vec<MergedPin> {
    let mut all: Vec<MergedPin> = machines
        .iter()
        .flat_map(|machine| {
            machine.pinned.iter().map(|pin| MergedPin {
                machine: machine.name.clone(),
                id: pin.id.clone(),
                title: pin.title.clone(),
                pinned_at: pin.pinned_at.clone(),
                pin_order_key: pin.pin_order_key.clone(),
            })
        })
        .collect();
    all.sort_by(|a, b| {
        order_key_cmp(&a.pin_order_key, &b.pin_order_key)
            .then_with(|| a.machine.cmp(&b.machine))
            .then_with(|| a.id.cmp(&b.id))
    });
    let mut seen = HashSet::new();
    all.retain(|pin| seen.insert(pin.id.clone()));
    all
}

/// One machine's good pins, as [`merge_pins`] takes them.
#[derive(Debug, Clone)]
pub struct MachinePins {
    pub name: String,
    pub pinned: Vec<PinnedThread>,
}
