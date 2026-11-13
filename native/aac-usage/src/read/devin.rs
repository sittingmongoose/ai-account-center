//! Devin CLI (DESIGN 16.3): `$XDG_DATA_HOME/devin/cli/sessions.db`, else
//! `~/.local/share/devin/cli/sessions.db`. One query over `message_nodes` selects, through
//! `json_extract` only, each reply's message and request ids, time, model and its four token counts
//! (`metadata.metrics`), with the session's working folder; message text, tool calls and extensions
//! stay inside SQLite. A reply is stored up to five times (copies when a conversation is
//! compacted): each counts once, by its message id (else its request id).

use super::sqlite;
use crate::collector::Collector;
use crate::discover::{Env, env_dir};
use crate::util;
use rusqlite::types::Value as Sql;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub fn database(home: &Path, env: &Env) -> PathBuf {
    env_dir(env, "XDG_DATA_HOME", home)
        .unwrap_or_else(|| home.join(".local").join("share"))
        .join("devin")
        .join("cli")
        .join("sessions.db")
}

const REPLIES: &str = "SELECT m.session_id, \
     json_extract(m.chat_message, '$.message_id'), json_extract(m.chat_message, '$.metadata.request_id'), \
     json_extract(m.chat_message, '$.metadata.created_at'), m.created_at, \
     json_extract(m.chat_message, '$.metadata.generation_model'), \
     json_extract(m.chat_message, '$.metadata.metrics.input_tokens'), \
     json_extract(m.chat_message, '$.metadata.metrics.output_tokens'), \
     json_extract(m.chat_message, '$.metadata.metrics.cache_read_tokens'), \
     json_extract(m.chat_message, '$.metadata.metrics.cache_creation_tokens'), \
     s.working_directory, s.model \
     FROM message_nodes m LEFT JOIN sessions s ON s.id = m.session_id \
     WHERE json_valid(m.chat_message) \
     AND json_extract(m.chat_message, '$.metadata.metrics.output_tokens') IS NOT NULL";

/// A reply's time: its own ISO stamp, else the node's epoch seconds (or milliseconds).
fn epoch_ms(stamp: &Sql, node: &Sql) -> Option<i64> {
    if let Some(text) = sqlite::text(stamp)
        && let Some(ms) = util::iso_ms(&text)
    {
        return Some(ms);
    }
    match node {
        Sql::Integer(v) if *v > 1_000_000_000_000 => Some(*v),
        Sql::Integer(v) if *v > 0 => Some(*v * 1000),
        _ => None,
    }
}

/// "not_installed" without the database, else "ok" (or "error" when it cannot be read).
pub fn collect(collector: &mut Collector, home: &Path, env: &Env) -> &'static str {
    let db = database(home, env);
    if !crate::fsx::is_file(&db) {
        return "not_installed";
    }
    collector.note_root("devin", db.parent().unwrap_or(&db), "default");
    collector.count_root(
        "devin",
        &util::realpath(db.parent().unwrap_or(&db)),
        std::slice::from_ref(&db),
    );
    let filekey = util::filekey("devin", &db);
    let Some(current) = sqlite::print_of(&db) else {
        return "error";
    };
    if sqlite::note_db(collector, "devin", &filekey, current) {
        collector.confirm_file("devin", &filekey);
        return "ok";
    }
    let Some((connection, immutable)) = sqlite::open(&db, collector.deadline()) else {
        collector.read_failure("devin", filekey);
        return "error";
    };
    if immutable
        && collector
            .pending
            .get("devin")
            .and_then(|m| m.get(&filekey))
            .is_some_and(|p| p.wal_size.unwrap_or(0) > 0)
    {
        collector.kind_wal_unread.insert("devin".into());
    }
    let tables = sqlite::tables(&connection).unwrap_or_default();
    if !tables.iter().any(|t| t == "message_nodes") || !tables.iter().any(|t| t == "sessions") {
        collector.confirm_file("devin", &filekey);
        return "ok";
    }
    let read: rusqlite::Result<Vec<Vec<Sql>>> = (|| {
        let mut statement = connection.prepare(REPLIES)?;
        let rows = statement.query_map([], |row| (0..12).map(|i| row.get::<_, Sql>(i)).collect())?;
        rows.collect()
    })();
    let Ok(replies) = read else {
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
        collector.read_failure("devin", filekey);
        return "error";
    };
    let mut seen: HashSet<String> = HashSet::new();
    for reply in replies {
        let Some(session) = sqlite::text(&reply[0]) else {
            continue;
        };
        let Some(id) = sqlite::text(&reply[1]).or_else(|| sqlite::text(&reply[2])) else {
            *collector.unreadable_records.entry("devin".into()).or_default() += 1;
            continue;
        };
        if !seen.insert(id.clone()) {
            continue;
        }
        let numbers: Vec<Option<u64>> = reply[6..10].iter().map(sqlite::count).collect();
        let (Some(input), Some(output), Some(read), Some(write)) = (numbers[0], numbers[1], numbers[2], numbers[3])
        else {
            *collector.unreadable_records.entry("devin".into()).or_default() += 1;
            continue;
        };
        let Some(at) = epoch_ms(&reply[3], &reply[4]) else {
            *collector.unreadable_records.entry("devin".into()).or_default() += 1;
            continue;
        };
        if at < collector.min_date_ms {
            continue;
        }
        let Some(hour) = util::hour_label(at, collector.now_ms) else {
            continue;
        };
        let raw_model = sqlite::text(&reply[5])
            .or_else(|| sqlite::text(&reply[11]))
            .unwrap_or_else(|| "unknown-devin-model".into());
        let model = util::clean_model(Some(&Value::String(raw_model))).unwrap_or_else(|| "unknown-devin-model".into());
        // Devin's input_tokens holds no cache traffic (VM, deduplicated: 11.7M input, 181M cache reads).
        let tokens = [input, output, read, write];
        collector.add("devin", &filekey, &model, None, &hour, tokens, 0.0);
        let session_key = util::session_key("devin", Some(&session));
        collector.sadd("devin", &filekey, session_key.as_deref(), &model, None, at, tokens, 0.0);
        collector.note_session("devin", &session, sqlite::text(&reply[10]));
        let key = util::sha256_hex(format!("aac-msg-v1:devin:{id}").as_bytes())[..16].to_string();
        collector.message(
            "devin",
            &filekey,
            key,
            at,
            session_key.as_deref(),
            &model,
            None,
            tokens,
            0.0,
        );
    }
    collector.confirm_file("devin", &filekey);
    "ok"
}
