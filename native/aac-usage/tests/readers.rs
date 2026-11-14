//! DESIGN 16.2 and 16.3: the Cursor run-store reader (CURSOR.md section 4) and the Devin CLI
//! reader, each on a fixture database that also holds marker text where conversation content and
//! secrets live, which must never reach the answer.

use aac_usage::discover::Env;
use aac_usage::protocol::{self, Request};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const SECRET: &str = "MARKER-THAT-MUST-NOT-LEAVE";
const AGENT: &str = "agent-7f3c1d2e";
const AT: &str = "2026-11-11T21:33:15.765Z";

fn env(home: &Path) -> Env {
    Env::from([
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("PATH".to_string(), String::new()),
    ])
}

fn request(kinds: &[&str], presence: bool) -> Request {
    protocol::parse_request(
        json!({"kinds": kinds, "minDateMs": 1_790_000_000_000u64, "deadlineMs": 60_000, "presence": presence,
               "messages": true})
        .to_string()
        .as_bytes(),
    )
    .unwrap()
}

fn cursor_store(home: &Path, project: &str) -> PathBuf {
    let db = home
        .join(".cursor/projects")
        .join(project)
        .join("sdk-agent-store/a1b2c3/index.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    db
}

/// A run store with the SDK's three tables, an agent whose secret columns hold the marker, and
/// runs in both token conventions, one in neither, one unfinished and one with broken usage JSON.
fn cursor_fixture(db: &Path) -> Connection {
    let connection = Connection::open(db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE agents (agent_id TEXT PRIMARY KEY, workspace_ref TEXT, status TEXT, name TEXT,
               metadata_json TEXT, created_at TEXT, updated_at TEXT);
             CREATE TABLE runs (run_id TEXT PRIMARY KEY, request_id TEXT, agent_id TEXT, turn_number INTEGER,
               status TEXT, model TEXT, model_params_json TEXT, usage_json TEXT, usage_ref TEXT, error_code TEXT,
               created_at TEXT, started_at TEXT, finished_at TEXT, cancelled_at TEXT, expired_at TEXT);
             CREATE TABLE run_events (run_id TEXT, payload TEXT);
             INSERT INTO agents VALUES ('{AGENT}', '/home/u/worktrees/t3-1', 'idle', '{SECRET}-name',
               '{{\"blobEncryptionKey\":\"{SECRET}-key\"}}', '{AT}', '{AT}');
             INSERT INTO run_events VALUES ('run-1', '{SECRET}-event');"
        ))
        .unwrap();
    let run = |id: &str, status: &str, usage: Option<&str>| {
        connection
            .execute(
                "INSERT INTO runs (run_id, request_id, agent_id, turn_number, status, model, usage_json,
                   created_at, started_at, finished_at) VALUES (?1, ?2, ?3, 1, ?4, 'grok-4.7', ?5, ?6, ?6, ?7)",
                rusqlite::params![
                    id,
                    format!("req-{id}"),
                    AGENT,
                    status,
                    usage,
                    AT,
                    if usage.is_some() { Some(AT) } else { None }
                ],
            )
            .unwrap();
    };
    // Cache inside input: total = input + output.
    run(
        "run-1",
        "finished",
        Some(
            r#"{"inputTokens":1000,"outputTokens":50,"cacheReadTokens":800,"cacheWriteTokens":100,"totalTokens":1050,"reasoningTokens":20}"#,
        ),
    );
    // Cache outside input: total = input + output + cacheRead + cacheWrite.
    run(
        "run-2",
        "finished",
        Some(
            r#"{"inputTokens":200,"outputTokens":30,"cacheReadTokens":500,"cacheWriteTokens":0,"totalTokens":730,"reasoningTokens":0}"#,
        ),
    );
    // Neither identity holds: unreadable, counted.
    run(
        "run-3",
        "finished",
        Some(r#"{"inputTokens":10,"outputTokens":5,"cacheReadTokens":1,"cacheWriteTokens":0,"totalTokens":999}"#),
    );
    run("run-4", "running", None);
    run("run-5", "finished", Some("{not json"));
    connection
}

fn t3_fixture(home: &Path) {
    let db = home.join(".t3/userdata/statev2.sqlite");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(&format!(
            "CREATE TABLE orchestration_v2_projection_provider_threads (provider_thread_id TEXT PRIMARY KEY,
               thread_id TEXT, owner_node_id TEXT, provider TEXT, provider_session_id TEXT, status TEXT,
               first_run_ordinal INTEGER, last_run_ordinal INTEGER, updated_at TEXT, payload_json TEXT,
               driver TEXT, provider_instance_id TEXT);
             INSERT INTO orchestration_v2_projection_provider_threads VALUES ('pt-1', 'thread-cursor-1', 'n', 'cursor',
               's', 'idle', 1, 2, '{AT}', '{{\"goal\":\"{SECRET}-goal\",\"nativeThreadRef\":{{\"nativeId\":\"{AGENT}\"}}}}',
               'cursor', 'cursor-instance-1');
             INSERT INTO orchestration_v2_projection_provider_threads VALUES ('pt-2', 'thread-codex-1', 'n', 'codex',
               's', 'idle', 1, 2, '{AT}', '{{\"nativeThreadRef\":{{\"nativeId\":\"{AGENT}\"}}}}', 'codex', 'codex-1');"
        ))
        .unwrap();
}

fn sums(answer: &Value, kind: &str) -> [u64; 5] {
    let mut out = [0; 5];
    for row in answer["rows"].as_array().unwrap().iter().filter(|r| r["k"] == kind) {
        for (i, key) in ["i", "o", "cr", "cw", "n"].iter().enumerate() {
            out[i] += row[*key].as_u64().unwrap();
        }
    }
    out
}

#[test]
fn cursor_runs_are_read_by_their_own_token_convention_and_joined_to_t3() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let _store = cursor_fixture(&cursor_store(h, "home-u-worktrees-t3-1"));
    t3_fixture(h);
    let answer = protocol::scan(&request(&["cursor"], true), h, &env(h));
    // run-1: uncached 1000 - 800 - 100; run-2: uncached 200. Reasoning stays inside output.
    assert_eq!(sums(&answer, "cursor"), [100 + 200, 50 + 30, 800 + 500, 100, 2]);
    assert_eq!(answer["kinds"]["cursor"]["state"], "ok");
    // run-3 fits neither convention, run-5 holds broken JSON: both counted, never guessed.
    assert_eq!(answer["kinds"]["cursor"]["unreadableRecords"], 2);
    assert_eq!(answer["kinds"]["cursor"]["partial"], true);
    // The session is the agent, hashed; T3's own record names its thread, explicitly.
    let key = aac_usage::util::session_key("cursor", Some(AGENT)).unwrap();
    let evidence = &answer["sessions"][&key];
    assert_eq!(evidence["t3"]["threadId"], "thread-cursor-1");
    assert_eq!(evidence["t3"]["providerInstanceId"], "cursor-instance-1");
    assert_eq!(evidence["t3"]["identitySource"], "t3_native_ref");
    assert_eq!(evidence["cwd"]["identitySource"], "inferred_cwd");
    // One message per run, keyed by the run, which itself never travels.
    let messages = answer["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 2);
    let expected = aac_usage::util::sha256_hex(b"aac-msg-v1:cursor:run-1")[..16].to_string();
    assert!(messages.iter().any(|m| m["key"] == expected.as_str()));
    // Where run stores exist the reader speaks for Cursor, not presence.
    assert!(answer["discovery"]["tools"].get("cursor").is_none());
    // Nothing secret or raw leaves: encryption key, agent name, events, T3 goal, raw ids.
    let text = answer.to_string();
    for forbidden in [SECRET, AGENT, "run-1", "req-run-1"] {
        assert!(!text.contains(forbidden), "{forbidden} left the reader");
    }
}

#[test]
fn a_cursor_store_with_rows_only_in_its_write_ahead_log_is_read() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let db = cursor_store(h, "p");
    let connection = cursor_fixture(&db);
    connection
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
        .unwrap();
    connection
        .execute(
            "INSERT INTO runs (run_id, agent_id, status, model, usage_json, started_at, finished_at)
             VALUES ('run-wal', ?1, 'finished', 'kimi-k3', ?2, ?3, ?3)",
            rusqlite::params![
                AGENT,
                r#"{"inputTokens":7,"outputTokens":3,"cacheReadTokens":0,"cacheWriteTokens":0,"totalTokens":10}"#,
                AT
            ],
        )
        .unwrap();
    assert!(fs::metadata(format!("{}-wal", db.display())).unwrap().len() > 0);
    let answer = protocol::scan(&request(&["cursor"], false), h, &env(h));
    assert!(answer["rows"].as_array().unwrap().iter().any(|r| r["m"] == "kimi-k3"));
    assert!(answer["kinds"]["cursor"].get("walUnread").is_none());
    drop(connection);
}

#[test]
fn cursor_without_a_run_store_is_left_to_presence() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    // A store with no tables yet counts as found, with no logs.
    let empty = cursor_store(h, "empty");
    Connection::open(&empty)
        .unwrap()
        .execute_batch("CREATE TABLE other (x)")
        .unwrap();
    let answer = protocol::scan(&request(&["cursor"], false), h, &env(h));
    assert_eq!(answer["kinds"]["cursor"]["state"], "ok");
    assert_eq!(sums(&answer, "cursor"), [0; 5]);
    // Only the CLI's chat store: no reader input, presence says "no local usage records".
    let other = tempfile::tempdir().unwrap();
    let o = other.path();
    let chat = o.join(".cursor/chats/x/store.db");
    fs::create_dir_all(chat.parent().unwrap()).unwrap();
    fs::write(&chat, "").unwrap();
    let answer = protocol::scan(&request(&["cursor"], true), o, &env(o));
    assert_eq!(answer["kinds"]["cursor"]["state"], "not_installed");
    assert_eq!(
        answer["discovery"]["tools"]["cursor"]["cause"],
        "no_local_usage_records"
    );
}

#[test]
fn cursor_token_conventions() {
    use aac_usage::read::cursor::tokens;
    assert_eq!(tokens(1000, 50, 800, 100, Some(1050)), Some([100, 50, 800, 100]));
    assert_eq!(tokens(200, 30, 500, 0, Some(730)), Some([200, 30, 500, 0]));
    assert_eq!(tokens(10, 5, 0, 0, Some(15)), Some([10, 5, 0, 0]));
    assert_eq!(tokens(10, 5, 1, 0, Some(999)), None);
    // Cache traffic larger than an "inside" input cannot be inside it.
    assert_eq!(tokens(10, 5, 20, 0, Some(15)), None);
    assert_eq!(tokens(10, 5, 0, 0, None), Some([10, 5, 0, 0]));
    assert_eq!(tokens(10, 5, 2, 0, None), None);
}

fn devin_fixture(home: &Path) -> PathBuf {
    let db = home.join(".local/share/devin/cli/sessions.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, working_directory TEXT, backend_type TEXT, model TEXT,
               agent_mode TEXT, created_at INTEGER, last_activity_at INTEGER, title TEXT, main_chain_id TEXT,
               shell_last_seen_index INTEGER, cogs_json TEXT, workspace_dirs TEXT, hidden INTEGER, metadata TEXT);
             CREATE TABLE message_nodes (row_id INTEGER PRIMARY KEY, session_id TEXT, node_id TEXT,
               parent_node_id TEXT, chat_message TEXT, created_at INTEGER, metadata TEXT);
             INSERT INTO sessions (id, working_directory, model) VALUES ('devin-session-1', '/home/u/proj', 'swe-2-max');",
        )
        .unwrap();
    let reply = |row: i64, id: &str, input: u64, read: u64| {
        let message = json!({"message_id": id, "role": "assistant", "content": format!("{SECRET} reply text"),
            "metadata": {"created_at": "2026-09-18T22:48:23.537068954Z", "generation_model": "swe-2-max",
                         "request_id": format!("r-{id}"),
                         "metrics": {"input_tokens": input, "output_tokens": 40, "cache_read_tokens": read,
                                     "cache_creation_tokens": null}}});
        connection
            .execute(
                "INSERT INTO message_nodes VALUES (?1, 'devin-session-1', ?2, NULL, ?3, 1789771814, NULL)",
                rusqlite::params![row, format!("node-{row}"), message.to_string()],
            )
            .unwrap();
    };
    reply(1, "m-1", 1000, 50_000);
    reply(2, "m-2", 500, 60_000);
    // A compaction copy of m-1: the same reply stored again.
    reply(3, "m-1", 1000, 50_000);
    connection
        .execute(
            "INSERT INTO message_nodes VALUES (4, 'devin-session-1', 'node-4', NULL, ?1, 1789771814, NULL)",
            [json!({"message_id": "u-1", "role": "user", "content": SECRET}).to_string()],
        )
        .unwrap();
    db
}

#[test]
fn devin_replies_count_once_each_and_no_text_leaves() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    devin_fixture(h);
    let mut ask = request(&["devin"], false);
    ask.min_date_ms = 1_788_000_000_000;
    let answer = protocol::scan(&ask, h, &env(h));
    assert_eq!(answer["kinds"]["devin"]["state"], "ok");
    assert_eq!(sums(&answer, "devin"), [1500, 80, 110_000, 0, 2]);
    assert!(answer["rows"].as_array().unwrap().iter().all(|r| r["m"] == "swe-2-max"));
    let key = aac_usage::util::session_key("devin", Some("devin-session-1")).unwrap();
    // The working folder, kept as the log wrote it (outside home, so not shortened to ~).
    assert!(
        answer["sessions"][&key]["cwd"]["path"]
            .as_str()
            .unwrap()
            .ends_with("proj")
    );
    let text = answer.to_string();
    assert!(!text.contains(SECRET));
    assert!(!text.contains("devin-session-1"));
    // Absent: not installed.
    let empty = tempfile::tempdir().unwrap();
    let answer = protocol::scan(&request(&["devin"], false), empty.path(), &env(empty.path()));
    assert_eq!(answer["kinds"]["devin"]["state"], "not_installed");
}

#[cfg(unix)]
fn rollout(session: &str, input: u64) -> String {
    [
        json!({"timestamp": AT, "type": "session_meta", "payload": {"id": session, "model_provider": "openai", "cwd": "/home/u/p"}}),
        json!({"timestamp": AT, "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}),
        json!({"timestamp": AT, "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": input, "cached_input_tokens": 0, "output_tokens": 10},
            "last_token_usage": {"input_tokens": input, "cached_input_tokens": 0, "output_tokens": 10}}}}),
    ]
    .iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n"
}

/// DESIGN 16.4: `archived_sessions` as a link out of home (the VM's points onto a network share)
/// is read when its target can be listed, and reported, never silently skipped, when it cannot.
#[cfg(unix)]
#[test]
fn a_log_folder_linked_out_of_home_is_read_or_reported() {
    let home = tempfile::tempdir().unwrap();
    let share = tempfile::tempdir().unwrap();
    let h = home.path();
    let archived = share.path().join("archived_sessions");
    fs::create_dir_all(&archived).unwrap();
    fs::write(
        archived.join("rollout-2026-08-29T10-00-00-aaaaaaaa-0000-0000-0000-000000000001.jsonl"),
        rollout("archived-1", 400),
    )
    .unwrap();
    fs::create_dir_all(h.join(".codex/sessions")).unwrap();
    std::os::unix::fs::symlink(&archived, h.join(".codex/archived_sessions")).unwrap();
    let mut ask = request(&["codex"], false);
    ask.min_date_ms = 1_780_000_000_000;
    let answer = protocol::scan(&ask, h, &env(h));
    assert_eq!(sums(&answer, "codex")[0], 400);
    // The target is gone: the root says so (a dangling link is unreachable, FIX-ROUND-BIG 17), and
    // the kind is partial, so the server keeps what it read through the link (FIX-ROUND-BIG 4).
    fs::remove_dir_all(&archived).unwrap();
    let answer = protocol::scan(&ask, h, &env(h));
    let roots = answer["discovery"]["kinds"]["codex"]["roots"].as_array().unwrap();
    assert!(
        roots
            .iter()
            .any(|r| r["state"] == "link_unreachable" && r["cause"] == "target_missing"),
        "{roots:?}"
    );
    assert_eq!(answer["kinds"]["codex"]["partial"], true);
    assert_eq!(sums(&answer, "codex")[0], 0);
}

/// DESIGN 16.5: experiment log folders outside PM-Experiments, bounded, never inside a checkout,
/// `node_modules` or past 4 levels; a hard-linked copy of a live log counts once.
#[cfg(unix)]
#[test]
fn experiment_folders_outside_pm_experiments_are_found_and_copies_count_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let live =
        h.join(".codex/sessions/2026/09/30/rollout-2026-09-30T15-07-41-bbbbbbbb-0000-0000-0000-000000000001.jsonl");
    fs::create_dir_all(live.parent().unwrap()).unwrap();
    fs::write(&live, rollout("live-1", 250)).unwrap();
    let kept = h.join("t3setup-codex-v6/retained-usage/run-1/codex/efb68f5a8d8b94d3");
    fs::create_dir_all(&kept).unwrap();
    fs::hard_link(&live, kept.join(live.file_name().unwrap())).unwrap();
    fs::write(
        kept.join("rollout-2026-09-30T16-00-00-bbbbbbbb-0000-0000-0000-000000000002.jsonl"),
        rollout("kept-only", 70),
    )
    .unwrap();
    let muse =
        h.join("t3setup-codex-v6/retained-usage/run-1/muse/a8bb139c2c98b181/01a0f37a-fed0-7a01-8ca3-437b0a1b4e0a");
    fs::create_dir_all(&muse).unwrap();
    fs::write(muse.join("session.jsonl"), "{}\n").unwrap();
    // Never found: a checkout's fixtures, node_modules, and a folder 5 levels down.
    for skipped in [
        "src/some-repo/fixtures/codex",
        "work/node_modules/pkg/codex",
        "a/b/c/d/codex",
    ] {
        let dir = h.join(skipped);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("rollout-2026-09-30T17-00-00-bbbbbbbb-0000-0000-0000-000000000009.jsonl"),
            rollout("never", 999),
        )
        .unwrap();
    }
    fs::create_dir_all(h.join("src/some-repo/.git")).unwrap();
    let roots = aac_usage::discover::experiment_roots(h, std::time::Duration::from_secs(5));
    assert!(roots.limits.is_empty());
    let found = roots.found;
    assert_eq!(
        found.get("codex").cloned().unwrap_or_default(),
        vec![h.join("t3setup-codex-v6/retained-usage/run-1/codex")]
    );
    assert_eq!(
        found.get("muse").cloned().unwrap_or_default(),
        // A Muse folder is one root (FIX-ROUND-BIG 6): its sessions are found under it.
        vec![h.join("t3setup-codex-v6/retained-usage/run-1/muse")]
    );
    let mut ask = request(&["codex"], false);
    ask.min_date_ms = 1_780_000_000_000;
    let answer = protocol::scan(&ask, h, &env(h));
    // The live log once (its hard-linked copy is the same file) and the kept-only log.
    assert_eq!(sums(&answer, "codex")[0], 250 + 70);
    let origins: Vec<&str> = answer["discovery"]["kinds"]["codex"]["roots"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|r| r["origin"].as_str())
        .collect();
    assert!(origins.contains(&"experiment"));
}
