//! Read-only SQLite for the Cursor and Devin readers (DESIGN 16.2, 16.3). A database is opened
//! `mode=ro`, which reads its write-ahead log and never writes the database or the log; when that
//! open fails (no shared-memory index can be opened, say), `immutable=1`, which reads only the
//! checkpointed pages, and the kind says its newest rows may wait in the log. The change print is
//! zcode's: size and mtime of the file and of its `-wal`.

use crate::collector::Collector;
use crate::util::{self, Print};
use rusqlite::{Connection, OpenFlags};
use serde_json::Value;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

pub fn print_of(db: &Path) -> Option<Print> {
    let meta = fs::metadata(db).ok()?;
    let mut print = Print::plain(meta.len(), util::mtime_ms(&meta), String::new(), String::new());
    print.wal_size = Some(0);
    print.wal_mtime_ms = Some(0);
    if let Ok(wal) = fs::metadata(format!("{}-wal", util::path_text(db))) {
        print.wal_size = Some(wal.len());
        print.wal_mtime_ms = Some(util::mtime_ms(&wal));
    }
    Some(print)
}

pub fn same(prior: Option<&Value>, current: &Print) -> bool {
    let Some(Value::Object(prior)) = prior else {
        return false;
    };
    let get = |k: &str| prior.get(k).and_then(Value::as_i64).unwrap_or(0);
    get("size") == current.size as i64
        && get("mtimeMs") == current.mtime_ms
        && get("walSize") == current.wal_size.unwrap_or(0) as i64
        && get("walMtimeMs") == current.wal_mtime_ms.unwrap_or(0)
}

fn uri(db: &Path, immutable: bool) -> String {
    let text = util::path_text(db)
        .replace('%', "%25")
        .replace('?', "%3F")
        .replace('#', "%23");
    format!("file:{text}?mode=ro{}", if immutable { "&immutable=1" } else { "" })
}

/// An open read-only connection, and whether it is immutable (so blind to the write-ahead log).
pub fn open(db: &Path, deadline: Instant) -> Option<(Connection, bool)> {
    for immutable in [false, true] {
        let Ok(connection) = Connection::open_with_flags(
            uri(db, immutable),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        ) else {
            continue;
        };
        if connection.busy_timeout(Duration::from_millis(500)).is_err() {
            continue;
        }
        connection.progress_handler(20_000, Some(move || Instant::now() >= deadline));
        // A schema read proves the open works (a locked or damaged file fails here, not later).
        if connection
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get::<_, i64>(0))
            .is_ok()
        {
            return Some((connection, immutable));
        }
    }
    None
}

pub fn tables(connection: &Connection) -> rusqlite::Result<Vec<String>> {
    let mut statement = connection.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
    let names = statement
        .query_map([], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(names)
}

/// Note a database's print as read this scan; true when unchanged since the prior one.
pub fn note_db(collector: &mut Collector, kind: &str, filekey: &str, current: Print) -> bool {
    let unchanged = same(collector.prior_print(kind, filekey), &current);
    collector
        .pending
        .entry(kind.to_string())
        .or_default()
        .insert(filekey.to_string(), current);
    unchanged
}

/// A non-negative whole number from a JSON-extracted SQLite value (integers, or reals that are).
pub fn count(value: &rusqlite::types::Value) -> Option<u64> {
    match value {
        rusqlite::types::Value::Null => Some(0),
        rusqlite::types::Value::Integer(v) if *v >= 0 => Some(*v as u64),
        rusqlite::types::Value::Real(v) if v.is_finite() && *v >= 0.0 && v.fract() == 0.0 => Some(*v as u64),
        _ => None,
    }
}

pub fn text(value: &rusqlite::types::Value) -> Option<String> {
    match value {
        rusqlite::types::Value::Text(text) => Some(text.clone()),
        _ => None,
    }
}
