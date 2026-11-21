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
        && prior.contains_key("newestUsageMs")
        && get("mtimeMs") == current.mtime_ms
        && get("walSize") == current.wal_size.unwrap_or(0) as i64
        && get("walMtimeMs") == current.wal_mtime_ms.unwrap_or(0)
}

/// The one SQLite URI every reader opens (FIX-ROUND-BIG 11 and 13): `%`, `?` and `#` in the path are
/// percent-encoded (so `%20` in a folder name never selects another file), and on Windows the path
/// is written verbatim (`\\?\C:\...`, `\\?\UNC\...`), which SQLite passes to the system unchanged:
/// past 260 characters too. The verbatim `?` is encoded like any other and decoded by SQLite.
pub fn uri(db: &Path, immutable: bool) -> String {
    let text = util::path_text(&util::abspath(db));
    #[cfg(windows)]
    let text = verbatim(&text);
    let mut encoded = String::with_capacity(text.len() + 8);
    for c in text.chars() {
        match c {
            '%' => encoded.push_str("%25"),
            '?' => encoded.push_str("%3F"),
            '#' => encoded.push_str("%23"),
            other => encoded.push(other),
        }
    }
    // A path that starts with `//` would read as an authority after `file:`.
    let prefix = if encoded.starts_with("//") { "file://" } else { "file:" };
    format!(
        "{prefix}{encoded}?mode=ro{}",
        if immutable { "&immutable=1" } else { "" }
    )
}

/// A Windows path in its verbatim form: no length limit, no `/` or `.` rewriting.
pub fn verbatim(text: &str) -> String {
    if text.starts_with(r"\\?\") {
        return text.to_string();
    }
    let text = text.replace('/', "\\");
    if let Some(rest) = text.strip_prefix(r"\\") {
        return format!(r"\\?\UNC\{rest}");
    }
    if text.len() > 2 && text.as_bytes()[1] == b':' {
        return format!(r"\\?\{text}");
    }
    text
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

/// Note a database's print as read this scan; true when unchanged since the prior one. A database
/// whose last read could not see its write-ahead log is read again even when its bytes match: the
/// log may be visible now, and its rows must be counted.
pub fn note_db(collector: &mut Collector, kind: &str, filekey: &str, current: Print) -> bool {
    let prior = collector.prior_print(kind, filekey);
    let waited = prior.is_some_and(|p| p.get("walUnread").and_then(Value::as_bool) == Some(true));
    let unchanged = !waited && same(prior, &current);
    collector
        .pending
        .entry(kind.to_string())
        .or_default()
        .insert(filekey.to_string(), current);
    unchanged
}

/// A non-negative whole number from a JSON-extracted SQLite value (integers, or reals that are).
/// A missing counter (NULL) is not a number: the record is unreadable, never 0 (FIX-ROUND-BIG 5).
pub fn count(value: &rusqlite::types::Value) -> Option<u64> {
    match value {
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
