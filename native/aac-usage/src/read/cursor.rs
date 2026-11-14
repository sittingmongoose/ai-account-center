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
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub const MAX_PROJECTS: usize = 4096;
pub const MAX_STORES_PER_PROJECT: usize = 256;
pub const MAX_STORES: usize = 2048;

/// Sub-folders of `base` by name, at most `limit`; links count only when they stay inside `base`.
/// The third value names failed listings or metadata filters (NotFound is simply absent):
/// an enumeration error is named, never an empty success (RECHECK-BIG-1 M3).
fn folders(base: &Path, limit: usize) -> (Vec<PathBuf>, bool, Vec<PathBuf>) {
    let entries = match std::fs::read_dir(base) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return (Vec::new(), false, Vec::new()),
        Err(_) => return (Vec::new(), false, vec![base.to_path_buf()]),
    };
    let mut names: Vec<String> = Vec::new();
    let mut failures = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => {
                if names.len() > limit {
                    break;
                }
                names.push(entry.file_name().to_string_lossy().into_owned());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => failures.push(base.to_path_buf()),
        }
    }
    let cut = names.len() > limit;
    names.sort();
    names.truncate(limit);
    let found = names
        .into_iter()
        .map(|name| base.join(name))
        .filter(|path| match crate::fsx::metadata(path) {
            Ok(meta) => meta.is_dir() && (!crate::fsx::is_link(path) || crate::fsx::link_inside(path, base).is_some()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => {
                failures.push(path.clone());
                false
            }
        })
        .collect();
    (found, cut, failures)
}

/// Every run store under a home's `.cursor/projects`, bounded: the stores, whether a bound cut
/// the list, and the folders that could not be listed (RECHECK-BIG-1 M3).
pub fn stores(home: &Path) -> (Vec<PathBuf>, bool, Vec<PathBuf>) {
    let projects_dir = home.join(".cursor").join("projects");
    let (projects, mut cut, mut failures) = folders(&projects_dir, MAX_PROJECTS);
    let mut found = Vec::new();
    for project in projects {
        let store_base = project.join("sdk-agent-store");
        let (agents, cut_here, failed) = folders(&store_base, MAX_STORES_PER_PROJECT);
        cut |= cut_here;
        failures.extend(failed);
        for store in agents {
            let db = store.join("index.db");
            match crate::fsx::metadata(&db) {
                Ok(meta) if meta.is_file() => {
                    found.push(db);
                    if found.len() >= MAX_STORES {
                        return (found, true, failures);
                    }
                }
                Ok(_) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => failures.push(store),
            }
        }
    }
    (found, cut, failures)
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

/// One store: "ok", "empty" (no `runs` table yet), "error" or "cut". A run another store already
/// counted (a copied store) counts once, in the first store by path (FIX-ROUND-BIG 2). A store is
/// confirmed only when every run in it was counted or named unreadable: a row cap that drops a
/// run leaves it unconfirmed (FIX-ROUND-BIG 5).
fn scan_store(
    collector: &mut Collector,
    db: &Path,
    agents: &mut HashMap<String, ()>,
    runs_seen: &mut HashSet<String>,
) -> &'static str {
    let filekey = util::filekey("cursor", db);
    let Some((connection, immutable)) = sqlite::open(db, collector.deadline()) else {
        collector.read_failure("cursor", filekey);
        return "error";
    };
    let wal_waiting = immutable
        && collector
            .pending
            .get("cursor")
            .and_then(|m| m.get(&filekey))
            .is_some_and(|p| p.wal_size.unwrap_or(0) > 0);
    if wal_waiting {
        collector.kind_wal_unread.insert("cursor".into());
        collector.set_pending("cursor", &filekey, |print| print.wal_unread = Some(true));
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
    let mut unreadable = broken.max(0) as u64;
    for run in runs {
        let (Some(run_id), Some(agent_id)) = (sqlite::text(&run[0]), sqlite::text(&run[1])) else {
            continue;
        };
        if !runs_seen.insert(run_id.clone()) {
            continue;
        }
        // Every counter must be there: a missing one is "not logged", never 0 (FIX-ROUND-BIG 5).
        let numbers: Vec<Option<u64>> = run[5..9].iter().map(sqlite::count).collect();
        let total = match &run[9] {
            Sql::Null => None,
            other => sqlite::count(other),
        };
        let (Some(input), Some(output), Some(read), Some(write)) = (numbers[0], numbers[1], numbers[2], numbers[3])
        else {
            unreadable += 1;
            continue;
        };
        let Some(split) = tokens(input, output, read, write, total) else {
            unreadable += 1;
            continue;
        };
        let at = sqlite::text(&run[4])
            .or_else(|| sqlite::text(&run[3]))
            .and_then(|t| util::iso_ms(&t));
        let Some(epoch_ms) = at else {
            unreadable += 1;
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
        if collector.row_cap {
            return "cut";
        }
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
    if unreadable > 0 {
        *collector.unreadable_records.entry("cursor".into()).or_default() += unreadable as usize;
        collector.set_pending("cursor", &filekey, |print| print.unreadable_records = Some(unreadable));
    }
    collector.confirm_file("cursor", &filekey);
    "ok"
}

/// T3 Code's own record of which of its threads drove each Cursor agent (CURSOR.md 3): one
/// read-only query of the provider-thread projection, `driver = 'cursor'`, selecting only the thread
/// id, the provider instance id and `payload_json.nativeThreadRef.nativeId`. No other column or
/// table is read.
pub fn t3_threads(home: &Path, collector: &mut Collector) -> HashMap<String, (String, String)> {
    let db = home.join(".t3").join("userdata").join("statev2.sqlite");
    let mut found = HashMap::new();
    if !crate::fsx::discovery_metadata(collector, "cursor", &db).is_some_and(|meta| meta.is_file()) {
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
/// "No local usage records"); "error" when every store failed; else "ok". A run store without a
/// `runs` table yet counts as found, with no logs.
///
/// Stores are read together (FIX-ROUND-BIG 2): when none changed and they are the stores of the
/// last scan, nothing is read and each keeps its rows, with the unreadable runs and write-ahead-log
/// note its print carries (FIX-ROUND-BIG 5); when any changed, every store is read again in path
/// order and each run counts once, in the first store that holds it.
pub fn collect(collector: &mut Collector, home: &Path) -> &'static str {
    let projects = home.join(".cursor").join("projects");
    // Noted before its probe, like every other root, so a probe failure is reported on it.
    collector.note_root("cursor", &projects, "default");
    if collector
        .gate_dirs("cursor", std::slice::from_ref(&projects))
        .is_empty()
    {
        return "not_installed";
    }
    let (mut dbs, cut, failures) = stores(home);
    // A store folder that could not be listed is named and the kind is partial, so what an
    // earlier scan read there is kept (RECHECK-BIG-1 M3).
    for folder in &failures {
        collector.unreadable_folder("cursor", folder);
    }
    if cut {
        collector.search_limit("cursor", dbs.len(), None);
    }
    if dbs.is_empty() {
        return if failures.is_empty() { "not_installed" } else { "ok" };
    }
    dbs.sort();
    collector.count_root("cursor", &util::realpath(&projects), &dbs);
    collector.run_stores += dbs.len();
    let mut changed = false;
    let mut failed = 0usize;
    let mut keys: HashSet<String> = HashSet::new();
    for db in &dbs {
        let filekey = util::filekey("cursor", db);
        keys.insert(filekey.clone());
        let Some(current) = sqlite::print_of(db) else {
            collector.read_failure("cursor", filekey);
            failed += 1;
            continue;
        };
        if !sqlite::note_db(collector, "cursor", &filekey, current) {
            changed = true;
        }
    }
    let before: HashSet<String> = collector
        .prior_kind("cursor")
        .map(|prints| prints.keys().cloned().collect())
        .unwrap_or_default();
    if !changed && before == keys {
        for key in keys {
            if collector.is_pending("cursor", &key) {
                replay(collector, "cursor", &key);
                collector.confirm_file("cursor", &key);
            }
        }
        return if failed == dbs.len() { "error" } else { "ok" };
    }
    let mut agents = HashMap::new();
    let mut runs_seen = HashSet::new();
    let mut read = 0usize;
    for db in &dbs {
        let filekey = util::filekey("cursor", db);
        if !collector.is_pending("cursor", &filekey) {
            continue;
        }
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
        match scan_store(collector, db, &mut agents, &mut runs_seen) {
            "cut" => {
                collector.truncated = true;
                return "ok";
            }
            "error" => failed += 1,
            _ => read += 1,
        }
    }
    if read == 0 && failed > 0 {
        return "error";
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

/// An unchanged database keeps what its last read found wrong (FIX-ROUND-BIG 5): its unreadable
/// records and its write-ahead-log note, from the print the server sent back.
pub fn replay(collector: &mut Collector, kind: &str, key: &str) {
    let Some(prior) = collector.prior_print(kind, key).cloned() else {
        return;
    };
    if let Some(count) = prior
        .get("unreadableRecords")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
    {
        *collector.unreadable_records.entry(kind.to_string()).or_default() += count as usize;
        collector.set_pending(kind, key, |print| print.unreadable_records = Some(count));
    }
    if prior.get("walUnread").and_then(Value::as_bool) == Some(true) {
        collector.kind_wal_unread.insert(kind.to_string());
        collector.set_pending(kind, key, |print| print.wal_unread = Some(true));
    }
}
