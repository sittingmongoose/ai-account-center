//! Cursor SDK run stores (DESIGN 16.2; CURSOR.md section 4): `<home>/.cursor/projects/*/
//! sdk-agent-store/*/index.db`, written by T3 Code's Cursor threads through Cursor's SDK. One query
//! over `runs` joined to `agents` selects only the run and agent ids, status, model, times, the six
//! `usage_json` numbers (through `json_extract`) and the agent's working folder.
//! `agents.metadata_json` (it holds a `blobEncryptionKey`), `agents.name` and every `run_events`
//! column are never selected, so they never reach this process. Raw ids stay here: sessions and
//! messages travel as hashes.

use super::sqlite;
use crate::collector::Collector;
use crate::util;
use rusqlite::types::Value as Sql;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub const MAX_PROJECTS: usize = 4096;
pub const MAX_STORES_PER_PROJECT: usize = 256;
pub const MAX_STORES: usize = 2048;

/// Sub-folders of `base` by name, at most `limit`; links count only when they stay inside `base`.
fn folders(base: &Path, limit: usize) -> (Vec<PathBuf>, bool) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return (Vec::new(), false);
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        if names.len() > limit {
            break;
        }
        names.push(entry.file_name().to_string_lossy().into_owned());
    }
    let cut = names.len() > limit;
    names.sort();
    names.truncate(limit);
    let found = names
        .into_iter()
        .map(|name| base.join(name))
        .filter(|path| {
            crate::fsx::is_dir(path) && (!crate::fsx::is_link(path) || crate::fsx::link_inside(path, base).is_some())
        })
        .collect();
    (found, cut)
}

/// Every run store under a home's `.cursor/projects`, bounded; true when a bound cut the list.
pub fn stores(home: &Path) -> (Vec<PathBuf>, bool) {
    let (projects, mut cut) = folders(&home.join(".cursor").join("projects"), MAX_PROJECTS);
    let mut found = Vec::new();
    for project in projects {
        let (agents, cut_here) = folders(&project.join("sdk-agent-store"), MAX_STORES_PER_PROJECT);
        cut |= cut_here;
        for store in agents {
            let db = store.join("index.db");
            if crate::fsx::is_file(&db) {
                found.push(db);
                if found.len() >= MAX_STORES {
                    return (found, true);
                }
            }
        }
    }
    (found, cut)
}

/// The run's four disjoint token kinds (input, output, cache read, cache write) by the run's own
/// convention (CURSOR.md 2.3): `total = input + output` means cache reads and writes are inside
/// `input`; `total = input + output + cacheRead + cacheWrite` means they are outside. Neither: None
/// (the run is counted as unreadable). Never decided by date or SDK version.
pub fn tokens(input: u64, output: u64, read: u64, write: u64, total: Option<u64>) -> Option<[u64; 4]> {
    let inside = input.checked_add(output)?;
    let outside = inside.checked_add(read)?.checked_add(write)?;
    match total {
        Some(total) if total == inside => Some([input.checked_sub(read)?.checked_sub(write)?, output, read, write]),
        Some(total) if total == outside => Some([input, output, read, write]),
        // Without a total the two conventions agree only when there is no cache traffic.
        None if read == 0 && write == 0 => Some([input, output, 0, 0]),
        _ => None,
    }
}

const RUNS: &str = "SELECT r.run_id, r.agent_id, r.model, r.started_at, r.finished_at, \
     json_extract(r.usage_json, '$.inputTokens'), json_extract(r.usage_json, '$.outputTokens'), \
     json_extract(r.usage_json, '$.cacheReadTokens'), json_extract(r.usage_json, '$.cacheWriteTokens'), \
     json_extract(r.usage_json, '$.totalTokens'), a.workspace_ref \
     FROM runs r LEFT JOIN agents a ON a.agent_id = r.agent_id \
     WHERE r.usage_json IS NOT NULL AND json_valid(r.usage_json)";
const RUNS_NO_AGENTS: &str = "SELECT r.run_id, r.agent_id, r.model, r.started_at, r.finished_at, \
     json_extract(r.usage_json, '$.inputTokens'), json_extract(r.usage_json, '$.outputTokens'), \
     json_extract(r.usage_json, '$.cacheReadTokens'), json_extract(r.usage_json, '$.cacheWriteTokens'), \
     json_extract(r.usage_json, '$.totalTokens'), NULL \
     FROM runs r WHERE r.usage_json IS NOT NULL AND json_valid(r.usage_json)";
const BROKEN: &str = "SELECT count(*) FROM runs WHERE usage_json IS NOT NULL AND NOT json_valid(usage_json)";

/// One store: "ok", "empty" (no `runs` table yet), "error" or "cut".
fn scan_store(collector: &mut Collector, db: &Path, agents: &mut HashMap<String, ()>) -> &'static str {
    let filekey = util::filekey("cursor", db);
    let Some(current) = sqlite::print_of(db) else {
        return "error";
    };
    if sqlite::note_db(collector, "cursor", &filekey, current) {
        collector.confirm_file("cursor", &filekey);
        return "ok";
    }
    let Some((connection, immutable)) = sqlite::open(db, collector.deadline()) else {
        collector.read_failure("cursor", filekey);
        return "error";
    };
    if immutable
        && collector
            .pending
            .get("cursor")
            .and_then(|m| m.get(&filekey))
            .is_some_and(|p| p.wal_size.unwrap_or(0) > 0)
    {
        collector.kind_wal_unread.insert("cursor".into());
    }
    let Ok(tables) = sqlite::tables(&connection) else {
        collector.read_failure("cursor", filekey);
        return "error";
    };
    if !tables.iter().any(|t| t == "runs") {
        collector.confirm_file("cursor", &filekey);
        return "empty";
    }
    let query = if tables.iter().any(|t| t == "agents") {
        RUNS
    } else {
        RUNS_NO_AGENTS
    };
    let read: rusqlite::Result<Vec<Vec<Sql>>> = (|| {
        let mut statement = connection.prepare(query)?;
        let rows = statement.query_map([], |row| (0..11).map(|i| row.get::<_, Sql>(i)).collect())?;
        rows.collect()
    })();
    let Ok(runs) = read else {
        if collector.expired() {
            collector.truncated = true;
            return "cut";
        }
        collector.read_failure("cursor", filekey);
        return "error";
    };
    let broken: i64 = connection.query_row(BROKEN, [], |row| row.get(0)).unwrap_or(0);
    *collector.unreadable_records.entry("cursor".into()).or_default() += broken.max(0) as usize;
    for run in runs {
        let (Some(run_id), Some(agent_id)) = (sqlite::text(&run[0]), sqlite::text(&run[1])) else {
            continue;
        };
        let numbers: Vec<Option<u64>> = run[5..9].iter().map(sqlite::count).collect();
        let total = match &run[9] {
            Sql::Null => None,
            other => sqlite::count(other),
        };
        let (Some(input), Some(output), Some(read), Some(write)) = (numbers[0], numbers[1], numbers[2], numbers[3])
        else {
            *collector.unreadable_records.entry("cursor".into()).or_default() += 1;
            continue;
        };
        let Some(split) = tokens(input, output, read, write, total) else {
            *collector.unreadable_records.entry("cursor".into()).or_default() += 1;
            continue;
        };
        let at = sqlite::text(&run[4])
            .or_else(|| sqlite::text(&run[3]))
            .and_then(|t| util::iso_ms(&t));
        let Some(epoch_ms) = at else {
            *collector.unreadable_records.entry("cursor".into()).or_default() += 1;
            continue;
        };
        if epoch_ms < collector.min_date_ms {
            continue;
        }
        let Some(hour) = util::hour_label(epoch_ms, collector.now_ms) else {
            continue;
        };
        let model = util::clean_model(Some(&Value::String(
            sqlite::text(&run[2]).unwrap_or_else(|| "unknown-cursor-model".into()),
        )))
        .unwrap_or_else(|| "unknown-cursor-model".into());
        collector.add("cursor", &filekey, &model, None, &hour, split, 0.0);
        let session_key = util::session_key("cursor", Some(&agent_id));
        collector.sadd(
            "cursor",
            &filekey,
            session_key.as_deref(),
            &model,
            None,
            epoch_ms,
            split,
            0.0,
        );
        collector.note_session("cursor", &agent_id, sqlite::text(&run[10]));
        agents.insert(agent_id, ());
        let key = util::sha256_hex(format!("aac-msg-v1:cursor:{run_id}").as_bytes())[..16].to_string();
        collector.message(
            "cursor",
            &filekey,
            key,
            epoch_ms,
            session_key.as_deref(),
            &model,
            None,
            split,
            0.0,
        );
    }
    collector.confirm_file("cursor", &filekey);
    "ok"
}

/// T3 Code's own record of which of its threads drove each Cursor agent (CURSOR.md 3): one
/// read-only query of the provider-thread projection, `driver = 'cursor'`, selecting only the thread
/// id, the provider instance id and `payload_json.nativeThreadRef.nativeId`. No other column or
/// table is read.
pub fn t3_threads(home: &Path, collector: &Collector) -> HashMap<String, (String, String)> {
    let db = home.join(".t3").join("userdata").join("statev2.sqlite");
    let mut found = HashMap::new();
    if !crate::fsx::is_file(&db) {
        return found;
    }
    let Some((connection, _)) = sqlite::open(&db, collector.deadline()) else {
        return found;
    };
    let read: rusqlite::Result<Vec<(Sql, Sql, Sql)>> = (|| {
        let mut statement = connection.prepare(
            "SELECT thread_id, provider_instance_id, \
             json_extract(payload_json, '$.nativeThreadRef.nativeId') \
             FROM orchestration_v2_projection_provider_threads \
             WHERE driver = 'cursor' AND json_valid(payload_json) LIMIT 100000",
        )?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
        rows.collect()
    })();
    for (thread, instance, native) in read.unwrap_or_default() {
        let (Some(thread), Some(instance), Some(native)) =
            (sqlite::text(&thread), sqlite::text(&instance), sqlite::text(&native))
        else {
            continue;
        };
        let bounded = |t: &str| !t.is_empty() && t.len() <= 200 && !t.chars().any(char::is_control);
        if bounded(&thread) && bounded(&instance) {
            found.insert(native, (thread, instance));
        }
    }
    found
}

/// "not_installed" without a run store (presence then says whether Cursor is there at all, with
/// "No local usage records"), else "ok"; a run store without a `runs` table yet counts as found,
/// with no logs.
pub fn collect(collector: &mut Collector, home: &Path) -> &'static str {
    let projects = home.join(".cursor").join("projects");
    let (dbs, cut) = stores(home);
    if cut {
        collector.discovery_truncated = true;
    }
    if dbs.is_empty() {
        return "not_installed";
    }
    collector.note_root("cursor", &projects, "default");
    collector.count_root("cursor", &util::realpath(&projects), &dbs);
    collector.run_stores += dbs.len();
    let mut agents = HashMap::new();
    for db in super::newest_first(&dbs) {
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
        if scan_store(collector, &db, &mut agents) == "cut" {
            return "ok";
        }
    }
    if !agents.is_empty() && !collector.expired() {
        let threads = t3_threads(home, collector);
        for agent in agents.keys() {
            if let Some((thread, instance)) = threads.get(agent) {
                collector.note_t3("cursor", agent, thread, instance);
            }
        }
    }
    "ok"
}
