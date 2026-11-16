//! T3's pinned threads, read from T3's own state database.
//!
//! The database is T3's, not AAC's, and its shape can change without notice, so this reader
//! is strict: it opens the file read-only (`SQLITE_OPEN_READ_ONLY`) with a busy timeout,
//! never writes, never holds a transaction, checks the table and payload shape it relies
//! on, and reports `shape` ("T3's state file changed shape") instead of guessing when
//! anything differs. Public pins carry only id, title, pinnedAt and pinOrderKey;
//! remote helper rows additionally retain SQLite type and lineage shape facts.
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

/// The local read's budget, the remote route's caps (a fixed byte allowance per answer) in
/// local form: at most this many rows shown, and at most this many bytes of pin text, per
/// read. Past the row budget the read shows the first rows in pin order and counts the
/// rest (`PinsRead::not_shown`), never a blanked list; past the byte budget the read is
/// still refused (`limit`), never a silently truncated pin list.
pub const MAX_PINNED_ROWS: usize = 1000;
pub const MAX_PIN_BYTES: usize = 1024 * 1024;

/// T3 auto-pins every delegated child thread at creation, so a pinned child counts only
/// when the owner pinned it explicitly: its `pinnedAt` more than this long after its
/// `createdAt`. Auto-pins land within seconds; explicit pins are minutes to days later.
const EXPLICIT_CHILD_AFTER_MS: i64 = 60_000;

/// One counted pinned thread (top-level, or an explicitly pinned child). `pinned_at` and `pin_order_key` pass through
/// with the JSON type T3 wrote (text in practice); `pin_order_key` may also carry JSON null when T3 wrote
/// no key (see the reader), ordered after every keyed row; anything else that is not a
/// string or a number for either field is a shape change, never a value to invent.
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

/// One machine's pins as read: the first [`MAX_PINNED_ROWS`] in pin order, plus how many
/// explicit pins past that budget were not shown. `not_shown` is reported, never dropped
/// silently; it is zero on every ordinary read.
#[derive(Debug, Clone, PartialEq)]
pub struct PinsRead {
    pub pinned: Vec<PinnedThread>,
    pub not_shown: usize,
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

fn limit() -> Error {
    Error::new("limit", "T3's state file is past this reader's pin limits")
}

/// Check the lineage's shape without filtering pins: the owner explicitly chose each pin.
/// Absent or JSON null lineage is valid; an object may have an absent, null or text
/// `parentThreadId`. Every other lineage container or parent type is a shape change.
/// A path extract alone cannot distinguish an absent parent from a wrong container.
fn validate_lineage(lineage: Option<&str>, parent: Option<&str>) -> Result<()> {
    match lineage {
        None | Some("null") => Ok(()),
        Some("object") if matches!(parent, None | Some("null" | "text")) => Ok(()),
        Some(_) => Err(shape()),
    }
}

/// Whether the lineage facts name a child thread (an object with a text parent). Only
/// children face the explicit-pin check; every other valid lineage is top-level.
fn is_child(lineage: Option<&str>, parent: Option<&str>) -> bool {
    lineage == Some("object") && parent == Some("text")
}

/// Milliseconds since the epoch from a strict UTC timestamp:
/// `YYYY-MM-DDTHH:MM[:SS[.f{1,9}]]Z` naming a real calendar moment (leap years counted,
/// no leap second, no 24:00). An offset tail, a rolled-over date (Feb 30) or any looser
/// grammar is None, so an auto-pinned child can never pass on a reinterpreted time.
fn parse_utc_ms(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let digits = |from: usize, count: usize| -> Option<i64> {
        let span = bytes.get(from..from + count)?;
        if span.iter().all(|c| c.is_ascii_digit()) {
            std::str::from_utf8(span).ok()?.parse().ok()
        } else {
            None
        }
    };
    // "YYYY-MM-DDTHH:MMZ" is the shortest accepted form (17 bytes).
    if bytes.len() < 17 || bytes[4] != b'-' || bytes[7] != b'-' || bytes[10] != b'T' || bytes[13] != b':' {
        return None;
    }
    let year = digits(0, 4)?;
    let month = digits(5, 2)?;
    let day = digits(8, 2)?;
    let hour = digits(11, 2)?;
    let minute = digits(14, 2)?;
    let mut at = 16;
    let mut second = 0i64;
    let mut milli = 0i64;
    if bytes.get(at) == Some(&b':') {
        second = digits(at + 1, 2)?;
        at += 3;
        if bytes.get(at) == Some(&b'.') {
            let start = at + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() && end - start < 9 {
                end += 1;
            }
            let fraction = &bytes[start..end];
            if fraction.is_empty() || bytes.get(end).is_some_and(|c| c.is_ascii_digit()) {
                return None; // no digits at all, or past the grammar's nine
            }
            let scaled = format!("{:0<3}", std::str::from_utf8(fraction).ok()?);
            milli = scaled[..3].parse().unwrap_or(0);
            at = end;
        }
    }
    if bytes.get(at..) != Some(b"Z".as_slice()) || !(1..=12).contains(&month) || hour > 23 || minute > 59 || second > 59
    {
        return None;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let last_day = match month {
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if day < 1 || day > last_day {
        return None;
    }
    // Days from the civil calendar (Howard Hinnant's days_from_civil), proleptic Gregorian.
    let (y, m) = (
        if month <= 2 { year - 1 } else { year },
        if month <= 2 { month + 9 } else { month - 3 },
    );
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let epoch_seconds = days * 86_400 + hour * 3_600 + minute * 60 + second;
    Some(epoch_seconds.checked_mul(1000)? + milli)
}

/// Whether a pinned child was pinned explicitly: both times strict UTC timestamps with
/// `pinnedAt` more than [`EXPLICIT_CHILD_AFTER_MS`] after `createdAt`. A missing or
/// unparseable time is not explicit; the row is simply not counted, never a failed read.
fn explicit_child_pin(pinned_at: Option<&str>, created_at: Option<&str>) -> bool {
    match (pinned_at.and_then(parse_utc_ms), created_at.and_then(parse_utc_ms)) {
        (Some(pinned), Some(created)) => pinned - created > EXPLICIT_CHILD_AFTER_MS,
        _ => false,
    }
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

/// Every counted pinned thread in `path`, in T3's pin order: top-level pins, plus child
/// pins the owner made explicitly (see [`explicit_child_pin`]); T3's auto-pinned children
/// are not counted. Error codes: `missing` when there is no file, `shape` when the table
/// or a counted payload (including its `lineage`) differs from the expected shape, `io`
/// when the file could not be read, `limit` when the shown pins pass [`MAX_PIN_BYTES`].
/// More than [`MAX_PINNED_ROWS`] counted pins show the first rows and count the rest in
/// `not_shown` instead of failing.
pub fn read_pinned_threads(path: &Path) -> Result<PinsRead> {
    read(path, false).map(|(pinned, _, not_shown)| PinsRead { pinned, not_shown })
}

/// Read the same bounded SQLite cursor in the remote helper's complete eleven-column
/// row format. Preserve counted child rows, absent/null type columns and scalar types so
/// an agent reply can replace sqlite3 JSON without normalizing or dropping any field.
/// Both representations count the same pins; this remote representation additionally
/// bounds every transported row and serialized byte, so past either cap it still refuses
/// (`limit`) and the caller falls back to the `sqlite3 -json` route, whose parser shows
/// the first rows and counts the rest.
pub fn read_remote_rows(path: &Path) -> Result<Vec<Json>> {
    read(path, true).map(|(_, rows, _)| rows)
}

fn read(path: &Path, remote: bool) -> Result<(Vec<PinnedThread>, Vec<Json>, usize)> {
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

    // `pinnedAt` is present-but-null on unpinned threads, so "set" is a non-null extract. The
    // lineage shape is checked from its own type facts below, in Rust: a path extract cannot tell "no parent" from "a lineage a path cannot walk", so the query
    // returns every counted row's lineage facts instead of filtering on the extract. One
    // filter does live in SQL, so auto-pinned children never count toward the row budget:
    // a child (object lineage, text parent) is returned only when both times are text and
    // SQLite's own date math puts the pin past the explicit gap (a hair under the Rust
    // threshold, so the strict Rust recheck below is the one that decides). Every other
    // lineage — top-level and wrong-typed alike — is still returned, so the shape refusal
    // below keeps working. The statement is dropped before the connection, so no read
    // transaction outlives this call.
    let mut stmt = conn
        .prepare(
            "SELECT thread_id, title, \
                    json_extract(payload_json, '$.pinnedAt'), \
                    json_type(payload_json, '$.pinnedAt'), \
                    json_extract(payload_json, '$.pinOrderKey'), \
                    json_type(payload_json, '$.pinOrderKey'), \
                    json_type(payload_json), \
                    json_type(payload_json, '$.lineage'), \
                    json_type(payload_json, '$.lineage.parentThreadId'), \
                    json_extract(payload_json, '$.createdAt'), \
                    json_type(payload_json, '$.createdAt') \
             FROM orchestration_v2_projection_threads \
             WHERE archived_at IS NULL AND deleted_at IS NULL \
               AND json_extract(payload_json, '$.pinnedAt') IS NOT NULL \
               AND (json_type(payload_json, '$.lineage') IS NOT 'object' \
                 OR json_type(payload_json, '$.lineage.parentThreadId') IS NOT 'text' \
                 OR (json_type(payload_json, '$.pinnedAt') = 'text' \
                     AND json_type(payload_json, '$.createdAt') = 'text' \
                     AND (julianday(json_extract(payload_json, '$.pinnedAt')) \
                        - julianday(json_extract(payload_json, '$.createdAt'))) * 86400.0 > 59.999)) \
             ORDER BY (json_extract(payload_json, '$.pinOrderKey') IS NULL) ASC, \
                    json_extract(payload_json, '$.pinOrderKey') ASC, \
                    json_extract(payload_json, '$.pinnedAt') ASC, thread_id ASC",
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
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, SqlValue>(9)?,
                row.get::<_, Option<String>>(10)?,
            ))
        })
        .map_err(map_sqlite)?;

    let mut pinned = Vec::new();
    let mut not_shown = 0usize;
    let mut spent = 0usize;
    let mut remote_rows = Vec::new();
    let mut remote_bytes = 0usize;
    for row in rows {
        let (
            id,
            title,
            pinned_at,
            pinned_type,
            pin_order_key,
            order_type,
            root_type,
            lineage_type,
            parent_type,
            created_at,
            created_type,
        ) = match row {
            Ok(values) => values,
            // A value SQLite cannot give this reader as the column's type is not T3's shape.
            Err(rusqlite::Error::InvalidColumnType(..)) => return Err(shape()),
            Err(error) => return Err(map_sqlite(error)),
        };
        if root_type.as_deref() != Some("object")
            || !matches!(pinned_type.as_deref(), Some("text" | "integer" | "real"))
            || !matches!(order_type.as_deref(), Some("text" | "integer" | "real" | "null") | None)
        {
            return Err(shape());
        }
        validate_lineage(lineage_type.as_deref(), parent_type.as_deref())?;
        // A child counts only when pinned explicitly. The SQL filter already dropped the
        // auto-pinned bulk; this strict recheck drops the rest (SQLite's date math accepts
        // offsets and rolled-over dates this reader does not). Skipped, never a failure.
        if is_child(lineage_type.as_deref(), parent_type.as_deref()) {
            let pinned_text = match (&pinned_at, pinned_type.as_deref()) {
                (SqlValue::Text(text), Some("text")) => Some(text.as_str()),
                _ => None,
            };
            let created_text = match (&created_at, created_type.as_deref()) {
                (SqlValue::Text(text), Some("text")) => Some(text.as_str()),
                _ => None,
            };
            if !explicit_child_pin(pinned_text, created_text) {
                continue;
            }
        }
        if remote {
            // `createdAt` is carried for the parser's own explicit-pin recheck and is
            // never itself a shape fact: any payload value passes through (a blob, which
            // a JSON extract never yields in practice, travels as null and simply cannot
            // prove a child explicit on the far side).
            let created_json = match created_at.clone() {
                SqlValue::Null | SqlValue::Blob(_) => Json::Null,
                other => scalar(other).unwrap_or(Json::Null),
            };
            let row = serde_json::json!({
                "thread_id": id, "title": title,
                "pinnedAt": scalar(pinned_at.clone())?, "pinnedType": pinned_type,
                "pinOrderKey": if matches!(pin_order_key, SqlValue::Null) { Json::Null } else { scalar(pin_order_key.clone())? },
                "orderType": order_type, "rootType": root_type,
                "lineageType": lineage_type, "parentType": parent_type,
                "createdAt": created_json, "createdType": created_type,
            });
            remote_bytes += serde_json::to_vec(&row).map_err(|_| shape())?.len();
            if remote_rows.len() >= MAX_PINNED_ROWS || remote_bytes > MAX_PIN_BYTES {
                return Err(limit());
            }
            remote_rows.push(row);
        }
        let pinned_at = scalar(pinned_at)?;
        // T3 writes no key as JSON null or not at all: the extract is SQL NULL and the type
        // is "null" or absent. That row is still pinned, carried as JSON null. Any other
        // declared type is a shape change; `scalar` is strict about the rest.
        let pin_order_key = match order_type.as_deref() {
            None | Some("null") => {
                if matches!(pin_order_key, SqlValue::Null) {
                    Json::Null
                } else {
                    return Err(shape());
                }
            }
            Some("text") | Some("integer") | Some("real") => scalar(pin_order_key)?,
            Some(_) => return Err(shape()),
        };
        // The read's own budget: past the row budget the rest are counted, not shown —
        // the list is never blanked for size alone. The byte budget stays hard: shown
        // pins past it refuse the whole read, never a silently cut title or list.
        if pinned.len() >= MAX_PINNED_ROWS {
            not_shown += 1;
            continue;
        }
        spent += id.len()
            + title.len()
            + serde_json::to_string(&pinned_at).map(|text| text.len()).unwrap_or(0)
            + serde_json::to_string(&pin_order_key)
                .map(|text| text.len())
                .unwrap_or(0);
        if spent > MAX_PIN_BYTES {
            return Err(limit());
        }
        pinned.push(PinnedThread {
            id,
            title,
            pinned_at,
            pin_order_key,
        });
    }
    Ok((pinned, remote_rows, not_shown))
}

/// The columns the remote query returns, and the only keys a row of `sqlite3 -json` output
/// may carry: exactly the SELECT above, aliased, nothing else.
const REMOTE_KEYS: [&str; 11] = [
    "thread_id",
    "title",
    "pinnedAt",
    "pinnedType",
    "pinOrderKey",
    "orderType",
    "rootType",
    "lineageType",
    "parentType",
    "createdAt",
    "createdType",
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

/// One remote type column as [`validate_lineage`] takes it: JSON null means SQLite had
/// no value there (the path found nothing); a string is a `json_type` name; anything else is a
/// changed shape.
fn remote_type_name(value: &Json) -> Result<Option<&str>> {
    match value {
        Json::Null => Ok(None),
        Json::String(name) => Ok(Some(name.as_str())),
        _ => Err(shape()),
    }
}

/// A remote value as the text the explicit-pin check reads: a JSON string whose declared
/// `json_type` is "text", and nothing else. Any other pairing simply cannot prove a child
/// explicit; whether it is also a shape change is the scalar checks' call, not this one's.
fn remote_text<'a>(row: &'a serde_json::Map<String, Json>, value_key: &str, type_key: &str) -> Option<&'a str> {
    match (row.get(value_key), row.get(type_key)) {
        (Some(Json::String(value)), Some(Json::String(declared))) if declared == "text" => Some(value.as_str()),
        _ => None,
    }
}

/// The `sqlite3 -json` text one remote machine printed for the pinned-threads query, parsed
/// with the same shape checks as the local read (`read_pinned_threads`): an array of row
/// objects carrying exactly the query's aliased columns, a text/integer/real scalar for
/// `pinnedAt` and a text/integer/real scalar — or JSON null when T3 wrote no key — for
/// `pinOrderKey`, each matching its declared `json_type`, a payload root that is an object.
/// `createdAt`/`createdType` carry no shape rule of their own; they only feed the
/// explicit-pin check. The query's WHERE clause applied the same unarchived, undeleted,
/// pinned predicate and already dropped auto-pinned children; the lineage is checked here
/// by the same `validate_lineage` as the local read, and a child is counted only when the
/// same strict explicit-pin check passes, while a wrong-typed lineage is refused instead
/// of read as "no parent". Past [`MAX_PINNED_ROWS`] counted pins the first rows are shown
/// and the rest counted in `not_shown`, as the local read does; the route's own byte cap
/// bounds the text itself. Empty output means no pinned threads on that machine.
/// `machine` is a remote computer in the packaged registry; any deviation is a caller error, not a shape claim.
pub fn parse_remote_pins(machine: &str, text: &str) -> Result<PinsRead> {
    if !crate::computers::known_machine(machine, true) {
        return Err(Error::new("invalid", "unknown machine for remote pins"));
    }
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(PinsRead {
            pinned: Vec::new(),
            not_shown: 0,
        });
    }
    let rows: Vec<Json> = serde_json::from_str(trimmed).map_err(|_| shape())?;
    let mut pinned = Vec::new();
    let mut not_shown = 0usize;
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
        // A type column prints as JSON null when SQLite had no value there (the path found
        // nothing); every other shape is a changed shape, as the scalar checks below are.
        let lineage_type = remote_type_name(row.get("lineageType").ok_or_else(shape)?)?;
        let parent_type = remote_type_name(row.get("parentType").ok_or_else(shape)?)?;
        validate_lineage(lineage_type, parent_type)?;
        // The same explicit-pin rule as the local read, decided before the scalar checks:
        // a child that cannot prove an explicit pin is not counted, whatever else its row
        // carries, and the read does not fail for it.
        if is_child(lineage_type, parent_type)
            && !explicit_child_pin(
                remote_text(&row, "pinnedAt", "pinnedType"),
                remote_text(&row, "createdAt", "createdType"),
            )
        {
            continue;
        }
        let (Some(Json::String(id)), Some(Json::String(title))) = (row.get("thread_id"), row.get("title")) else {
            return Err(shape());
        };
        if id.is_empty() || title.is_empty() {
            return Err(shape());
        }
        let pin_order_key = match row.get("orderType") {
            Some(Json::Null) => {
                // No key at all: the query's extract and type are both SQL NULL, printed
                // as JSON null. T3 pins without a key are still pinned.
                match row.get("pinOrderKey") {
                    Some(Json::Null) => Json::Null,
                    _ => return Err(shape()),
                }
            }
            Some(Json::String(name)) if name == "null" => {
                // JSON null key: the extract is SQL NULL, the type is "null".
                match row.get("pinOrderKey") {
                    Some(Json::Null) => Json::Null,
                    _ => return Err(shape()),
                }
            }
            _ => remote_scalar(&row, "pinOrderKey", "orderType")?,
        };
        let thread = PinnedThread {
            id: id.clone(),
            title: title.clone(),
            pinned_at: remote_scalar(&row, "pinnedAt", "pinnedType")?,
            pin_order_key,
        };
        // Past the row budget the rest are counted, not shown, as the local read does.
        if pinned.len() >= MAX_PINNED_ROWS {
            not_shown += 1;
        } else {
            pinned.push(thread);
        }
    }
    Ok(PinsRead { pinned, not_shown })
}

/// Where a pin order key sorts, as SQLite orders it: numbers before text, numbers by value,
/// text by bytes, and JSON null (a pin with no key) after every keyed row. Two keys of the
/// same kind compare inside the kind; anything else falls back to the machine name and
/// then the thread id, so the merged order is total. Null-vs-null is equal here; the caller
/// breaks equal-key ties by `pinnedAt`.
fn order_key_cmp(a: &Json, b: &Json) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    // Null sorts after every keyed row; numbers before text, as SQLite orders them.
    let rank = |value: &Json| match value {
        Json::Number(_) => 0,
        Json::String(_) => 1,
        Json::Null => 2,
        _ => 3,
    };
    match (a, b) {
        (Json::Null, Json::Null) => Ordering::Equal,
        (Json::Null, _) => Ordering::Greater,
        (_, Json::Null) => Ordering::Less,
        (Json::Number(left), Json::Number(right)) => {
            left.as_f64().partial_cmp(&right.as_f64()).unwrap_or(Ordering::Equal)
        }
        (Json::String(left), Json::String(right)) => left.cmp(right),
        (Json::Number(_), Json::String(_)) => Ordering::Less,
        (Json::String(_), Json::Number(_)) => Ordering::Greater,
        _ => rank(a).cmp(&rank(b)),
    }
}

/// Every machine's good pins in one list: keyed rows ordered by `pinOrderKey` (then
/// `pinnedAt`, then machine, then thread id), then null-key rows by `pinnedAt` (then
/// machine, then thread id), each row tagged with its machine, and de-duplicated by
/// thread id — the first row in that order is the one kept, so a thread pinned on two
/// machines keeps the copy that sorts first, never a blend of the two.
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
            // Equal keys order by `pinnedAt`, the same tiebreak the per-machine SQL reads
            // use (pinOrderKey, then pinnedAt, then thread id); machine and id only settle
            // what `pinnedAt` cannot.
            .then_with(|| order_key_cmp(&a.pinned_at, &b.pinned_at))
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
