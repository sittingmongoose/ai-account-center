//! zcode's SQLite database (`~/.zcode/cli/db/db.sqlite`): `model_usage` grouped by hour and by
//! session. Model names and integers only. The Python helper's `_scan_zcode_db`.

use crate::collector::{Collector, MAX_SROWS};
use crate::discover::Env;
use crate::util::{self, Print};
use rusqlite::{Connection, OpenFlags, types::Value as Sql};
use serde_json::Value;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn print_of(db: &Path) -> Option<Print> {
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

fn same(prior: Option<&Value>, current: &Print) -> bool {
    let Some(Value::Object(prior)) = prior else {
        return false;
    };
    let get = |k: &str| prior.get(k).and_then(Value::as_i64).unwrap_or(0);
    get("size") == current.size as i64
        && prior.contains_key("newestUsageMs")
        && prior.get("walUnread").and_then(Value::as_bool).unwrap_or(false) == current.wal_unread.unwrap_or(false)
        && get("mtimeMs") == current.mtime_ms
        && get("walSize") == current.wal_size.unwrap_or(0) as i64
        && get("walMtimeMs") == current.wal_mtime_ms.unwrap_or(0)
}

fn open(db: &Path, immutable: bool) -> rusqlite::Result<Connection> {
    // The shared builder encodes `%` too (FIX-ROUND-BIG 11): `account%20one` is that folder, never
    // `account one`.
    let connection = Connection::open_with_flags(
        super::sqlite::uri(db, immutable),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

fn int(value: &Sql) -> Option<i64> {
    match value {
        Sql::Integer(v) => Some(*v),
        _ => None,
    }
}

fn number(value: &Sql) -> Option<f64> {
    // Python: _non_negative_number(value or 0): NULL reads 0; text is no number.
    match value {
        Sql::Null => Some(0.0),
        Sql::Integer(v) if *v >= 0 => Some(*v as f64),
        Sql::Real(v) if v.is_finite() && *v >= 0.0 => Some(*v),
        Sql::Real(v) if *v == 0.0 => Some(0.0),
        _ => None,
    }
}

fn text(value: &Sql) -> Option<Value> {
    match value {
        Sql::Text(text) => Some(Value::String(text.clone())),
        _ => None,
    }
}

fn scan_db(collector: &mut Collector, db: &Path, immutable: bool) -> &'static str {
    let filekey = util::filekey("zcode", db);
    // Only probed databases reach here, so a print that cannot be taken is a named read
    // failure, never an absence (RECHECK-BIG-1 M3).
    let Some(mut current) = print_of(db) else {
        collector.read_failure("zcode", filekey);
        return "error";
    };
    if immutable && current.wal_size.unwrap_or(0) > 0 {
        // An immutable open never reads the write-ahead log.
        collector.wal_unread = true;
        current.wal_unread = Some(true);
    }
    let unchanged = same(collector.prior_print("zcode", &filekey), &current);
    collector
        .pending
        .entry("zcode".into())
        .or_default()
        .insert(filekey.clone(), current);
    if unchanged {
        collector.confirm_file("zcode", &filekey);
        return "ok";
    }
    let Ok(connection) = open(db, immutable) else {
        collector.read_failure("zcode", filekey);
        return "error";
    };
    let hourly: rusqlite::Result<Vec<Vec<Sql>>> = (|| {
        let mut statement = connection.prepare(
            "SELECT model_id, provider_id, CAST(started_at / 3600000 AS INTEGER) * 3600, \
             SUM(input_tokens), SUM(output_tokens), SUM(cache_read_input_tokens), \
             SUM(cache_creation_input_tokens), COUNT(*) FROM model_usage WHERE started_at >= ? \
             GROUP BY model_id, provider_id, CAST(started_at / 3600000 AS INTEGER)",
        )?;
        let rows = statement.query_map([collector.min_date_ms], |row| {
            (0..8)
                .map(|i| row.get::<_, Sql>(i))
                .collect::<rusqlite::Result<Vec<Sql>>>()
        })?;
        rows.collect()
    })();
    let Ok(groups) = hourly else {
        collector.read_failure("zcode", filekey);
        return "error";
    };
    for group in groups {
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
        let Some(model) = text(&group[0]).and_then(|t| util::clean_model(Some(&t))) else {
            continue;
        };
        let provider = text(&group[1]).and_then(|t| util::clean_provider(Some(&t)));
        let Some(hour_seconds) = int(&group[2]) else {
            continue;
        };
        let epoch_ms = hour_seconds * 1000;
        if epoch_ms < collector.min_date_ms {
            continue;
        }
        let sums: Vec<Option<f64>> = group[3..7].iter().map(number).collect();
        if sums.iter().any(Option::is_none) {
            continue;
        }
        let sums: Vec<u64> = sums.into_iter().map(|v| util::int_of(v.unwrap_or(0.0))).collect();
        let Some(count) = int(&group[7]).filter(|c| *c > 0) else {
            continue;
        };
        let tokens = [sums[0].saturating_sub(sums[2]), sums[1], sums[2], sums[3]];
        let Some(hour) = util::hour_label(epoch_ms, collector.now_ms) else {
            continue;
        };
        if !collector.add_many(
            "zcode",
            &filekey,
            &model,
            provider.as_deref(),
            &hour,
            tokens,
            count as u64,
        ) {
            return "ok";
        }
    }
    // Sessions: the same integers by the log's own session id, with first and last event.
    let sessions: Vec<Vec<Sql>> = (|| -> rusqlite::Result<Vec<Vec<Sql>>> {
        let connection = open(db, immutable)?;
        let mut statement = connection.prepare(
            "SELECT session_id, model_id, provider_id, MIN(started_at), MAX(started_at), \
             SUM(input_tokens), SUM(output_tokens), SUM(cache_read_input_tokens), \
             SUM(cache_creation_input_tokens), COUNT(*) FROM model_usage WHERE started_at >= ? \
             GROUP BY session_id, model_id, provider_id",
        )?;
        let rows = statement.query_map([collector.min_date_ms], |row| {
            (0..10)
                .map(|i| row.get::<_, Sql>(i))
                .collect::<rusqlite::Result<Vec<Sql>>>()
        })?;
        rows.collect()
    })()
    .unwrap_or_default();
    for group in sessions {
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
        let raw = match &group[0] {
            Sql::Text(text) => Some(text.clone()),
            _ => None,
        };
        let session = util::session_key("zcode", raw.as_deref());
        let model = text(&group[1]).and_then(|t| util::clean_model(Some(&t)));
        let (Some(session), Some(model)) = (session, model) else {
            continue;
        };
        let provider = text(&group[2]).and_then(|t| util::clean_provider(Some(&t)));
        let (Some(first), Some(last)) = (int(&group[3]), int(&group[4])) else {
            continue;
        };
        if first < collector.min_date_ms || last < first {
            continue;
        }
        let sums: Vec<Option<f64>> = group[5..9].iter().map(number).collect();
        if sums.iter().any(Option::is_none) {
            continue;
        }
        let sums: Vec<u64> = sums.into_iter().map(|v| util::int_of(v.unwrap_or(0.0))).collect();
        let Some(count) = int(&group[9]).filter(|c| *c > 0) else {
            continue;
        };
        let tokens = [sums[0].saturating_sub(sums[2]), sums[1], sums[2], sums[3]];
        if let Some(raw) = raw.as_deref() {
            collector.note_session("zcode", raw, None);
        }
        if collector.srow_list.len() >= MAX_SROWS {
            collector.truncated = true;
            break;
        }
        if !collector.sadd_range(
            "zcode",
            &filekey,
            Some(&session),
            &model,
            provider.as_deref(),
            (first, last),
            tokens,
            0.0,
            count as u64,
        ) {
            break;
        }
    }
    collector.confirm_file("zcode", &filekey);
    "ok"
}

/// Python's `_collect_zcode`.
pub fn collect(collector: &mut Collector, home: &Path, env: &Env, immutable: bool, extra: &[PathBuf]) -> &'static str {
    let override_path = env
        .get("ZCODE_DB_PATH")
        .map(|v| v.trim())
        .filter(|v| crate::fsx::absolute_text(v));
    let db = match override_path {
        Some(path) => PathBuf::from(path),
        None => home.join(".zcode").join("cli").join("db").join("db.sqlite"),
    };
    let mut seen = std::collections::HashSet::new();
    let mut dbs = Vec::new();
    for (index, candidate) in std::iter::once(&db).chain(extra.iter()).enumerate() {
        let absolute = util::abspath(candidate);
        if seen.insert(absolute.clone()) {
            let origin = if index > 0 {
                "extra"
            } else if override_path.is_some() {
                "env"
            } else {
                "default"
            };
            collector.note_root("zcode", &absolute, origin);
            dbs.push(absolute);
        }
    }
    let mut states = Vec::new();
    // Each database is probed first (FIX-ROUND-BIG 9): one that does not answer, or is there but
    // cannot be read, is named on its root.
    // The probe is the first touch: even a link test would follow a linked folder above it.
    let answered = collector.gate_files("zcode", &dbs);
    for candidate in answered.into_iter().filter(|db| !crate::fsx::is_link(db)) {
        collector.count_root("zcode", &util::realpath(&candidate), std::slice::from_ref(&candidate));
        states.push(scan_db(collector, &candidate, immutable));
    }
    if states.is_empty() || states.iter().all(|s| *s == "not_installed") {
        "not_installed"
    } else if states.contains(&"error") {
        "error"
    } else {
        "ok"
    }
}
