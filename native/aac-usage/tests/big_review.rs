//! The five-model review of b09066667 (FIX-ROUND-BIG.md in the share folder
//! 2026-11-13-analytics-discovery): one test per fixed item, each built on the reviewers'
//! reproductions and each failing at b09066667. Synthetic fixtures only.

use aac_usage::discover::Env;
use aac_usage::protocol::{self, Request};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const AT: &str = "2026-11-11T21:33:15.765Z";
const MIN_DATE: u64 = 1_780_000_000_000;

fn env(home: &Path) -> Env {
    Env::from([
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("PATH".to_string(), String::new()),
    ])
}

/// A request for `kinds`, with the prints an earlier answer returned (the server sends them back).
fn ask(kinds: &[&str], prior: Option<&Value>) -> Request {
    let mut fingerprints = Map::new();
    if let Some(prior) = prior {
        for kind in kinds {
            fingerprints.insert(kind.to_string(), prior["kinds"][kind]["fingerprints"].clone());
        }
    }
    protocol::parse_request(
        json!({"kinds": kinds, "minDateMs": MIN_DATE, "deadlineMs": 60_000, "fingerprints": fingerprints})
            .to_string()
            .as_bytes(),
    )
    .unwrap()
}

fn scan(home: &Path, kinds: &[&str], prior: Option<&Value>) -> Value {
    protocol::scan(&ask(kinds, prior), home, &env(home))
}

/// Input, output, cache read, cache write and events of one kind's rows.
fn sums(answer: &Value, kind: &str) -> [u64; 5] {
    let mut out = [0; 5];
    for row in answer["rows"].as_array().unwrap().iter().filter(|r| r["k"] == kind) {
        for (i, key) in ["i", "o", "cr", "cw", "n"].iter().enumerate() {
            out[i] += row[*key].as_u64().unwrap();
        }
    }
    out
}

/// A Codex rollout with one cumulative token count per entry of `totals` (input; output 10 each).
fn rollout(session: &str, totals: &[u64]) -> String {
    let mut lines = vec![
        json!({"timestamp": AT, "type": "session_meta", "payload": {"id": session, "model_provider": "openai"}}),
        json!({"timestamp": AT, "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}),
    ];
    for (index, total) in totals.iter().enumerate() {
        let previous = if index == 0 { 0 } else { totals[index - 1] };
        lines.push(json!({"timestamp": AT, "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": total, "cached_input_tokens": 0, "output_tokens": 10 * (index as u64 + 1)},
            "last_token_usage": {"input_tokens": total - previous, "cached_input_tokens": 0, "output_tokens": 10}}}}));
    }
    lines.iter().map(|v| format!("{v}\n")).collect()
}

const ID: &str = "aaaaaaaa-1111-2222-3333-444444444444";

fn rollout_name(id: &str) -> String {
    format!("rollout-2026-11-11T21-00-00-{id}.jsonl")
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

fn zst(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    // The frame states its decoded size, as Codex's do.
    fs::write(path, zstd::bulk::compress(body.as_bytes(), 3).unwrap()).unwrap();
}

// ----- A1: Codex copies anywhere count once ------------------------------------------------------

/// Wonder M1, Sol M1, Astra M1: the live log in `sessions`, its archived prefix in
/// `archived_sessions` under the same name. True: 2 events, 300 input. b09066667: 3 and 400.
#[test]
fn a_codex_conversation_in_sessions_and_archived_sessions_counts_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".codex/sessions/2026/11/11").join(rollout_name(ID)),
        &rollout(ID, &[100, 300]),
    );
    write(
        &h.join(".codex/archived_sessions").join(rollout_name(ID)),
        &rollout(ID, &[100]),
    );
    let answer = scan(h, &["codex"], None);
    assert_eq!(sums(&answer, "codex"), [300, 20, 0, 0, 2]);
}

/// Fable M2 (T01c): `CODEX_HOME` holds a continued copy of a conversation whose start stays in
/// `~/.codex`. True: 10 events. b09066667: 15.
#[test]
fn codex_copies_in_two_default_homes_count_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let totals: Vec<u64> = (1..=10).map(|n| n * 100).collect();
    write(
        &h.join(".codex/sessions").join(rollout_name(ID)),
        &rollout(ID, &totals[..5]),
    );
    write(
        &h.join("other-home/sessions").join(rollout_name(ID)),
        &rollout(ID, &totals),
    );
    let mut environment = env(h);
    environment.insert("CODEX_HOME".into(), h.join("other-home").to_string_lossy().into_owned());
    let answer = protocol::scan(&ask(&["codex"], None), h, &environment);
    assert_eq!(sums(&answer, "codex")[4], 10);
    assert_eq!(sums(&answer, "codex")[0], 1000);
}

/// Fable M2 (T14), Astra M1: the live log compressed (10 events), an older plain copy of its first
/// 5 elsewhere. True: 10. b09066667: 5 (any plain copy suppressed the compressed one).
#[test]
fn a_longer_compressed_copy_is_read_over_a_plain_prefix() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let totals: Vec<u64> = (1..=10).map(|n| n * 100).collect();
    zst(
        &h.join(".codex/sessions").join(format!("{}.zst", rollout_name(ID))),
        &rollout(ID, &totals),
    );
    write(
        &h.join(".codex/archived_sessions").join(rollout_name(ID)),
        &rollout(ID, &totals[..5]),
    );
    let answer = scan(h, &["codex"], None);
    assert_eq!(sums(&answer, "codex")[4], 10);
}

/// Fable M1 (T09): a conversation's log moved to `archived_sessions` keeps its key, so a server
/// that keeps the old rows while the kind is partial never adds the moved copy to them. b09066667
/// keyed the log by its path: the moved log was new rows beside the kept ones.
#[test]
fn a_codex_log_moved_to_archived_sessions_keeps_its_key() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let live = h.join(".codex/sessions/2026/11/11").join(rollout_name(ID));
    write(&live, &rollout(ID, &[100, 300]));
    let first = scan(h, &["codex"], None);
    let keys: Vec<String> = first["kinds"]["codex"]["fingerprints"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(keys.len(), 1);
    let archived = h.join(".codex/archived_sessions").join(rollout_name(ID));
    fs::create_dir_all(archived.parent().unwrap()).unwrap();
    fs::rename(&live, &archived).unwrap();
    let second = scan(h, &["codex"], Some(&first));
    let after: Vec<String> = second["kinds"]["codex"]["fingerprints"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect();
    assert_eq!(after, keys);
    // Unchanged: nothing re-read, nothing new to add.
    assert_eq!(sums(&second, "codex"), [0, 0, 0, 0, 0]);
}

// ----- A2: hard links and copied files count once ------------------------------------------------

fn muse_session(input: u64) -> String {
    format!(
        "{}\n",
        json!({"recorded_at": 1_790_000_000, "payload": {"event": {"kind": "model_completed", "model": "glm-5",
            "usage": {"input_tokens": input, "output_tokens": 10, "cache_read_tokens": 0, "cache_write_tokens": 0}}}})
    )
}

/// Sol M2, Astra M2, Fable M3: a live Muse session hard-linked into a retained-usage folder, and a
/// plain copy in a backup. True: 100 input once. b09066667: 200, then 300 with the copy.
#[cfg(unix)]
#[test]
fn a_muse_session_hard_linked_or_copied_counts_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let live = h.join(".local/share/muse/sessions/sess-0001/session.jsonl");
    write(&live, &muse_session(100));
    let kept = h.join("lab/kept/run/muse/bucket/sess-0001/session.jsonl");
    fs::create_dir_all(kept.parent().unwrap()).unwrap();
    fs::hard_link(&live, &kept).unwrap();
    write(
        &h.join("backup/2026-11/muse/sess-0001/session.jsonl"),
        &muse_session(100),
    );
    let answer = scan(h, &["muse"], None);
    assert_eq!(sums(&answer, "muse")[0], 100);
    assert_eq!(sums(&answer, "muse")[4], 1);
}

fn cursor_db(home: &Path, project: &str, store: &str, runs: &[(&str, &str)]) -> PathBuf {
    let db = home
        .join(".cursor/projects")
        .join(project)
        .join("sdk-agent-store")
        .join(store)
        .join("index.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE agents (agent_id TEXT PRIMARY KEY, workspace_ref TEXT, name TEXT, metadata_json TEXT);
             CREATE TABLE runs (run_id TEXT PRIMARY KEY, agent_id TEXT, model TEXT, usage_json TEXT,
               started_at TEXT, finished_at TEXT);
             INSERT INTO agents VALUES ('agent-1', '/home/u/p', 'n', '{}');",
        )
        .unwrap();
    for (id, usage) in runs {
        connection
            .execute(
                "INSERT INTO runs VALUES (?1, 'agent-1', 'grok-4.7', ?2, ?3, ?3)",
                rusqlite::params![id, usage, AT],
            )
            .unwrap();
    }
    db
}

const RUN: &str = r#"{"inputTokens":300,"outputTokens":50,"cacheReadTokens":0,"cacheWriteTokens":0,"totalTokens":350}"#;

/// Sol M3, Astra M3, Wonder L1: a run store copied into another project counts each run once.
/// True: 300 input, 1 event. b09066667: 600 and 2.
#[test]
fn a_copied_cursor_run_store_counts_each_run_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    cursor_db(h, "proj-a", "store-1", &[("run-1", RUN)]);
    cursor_db(h, "proj-b", "store-2-copy", &[("run-1", RUN)]);
    let answer = scan(h, &["cursor"], None);
    assert_eq!(sums(&answer, "cursor")[0], 300);
    assert_eq!(sums(&answer, "cursor")[4], 1);
    // And still once when one store changes and the other is unchanged.
    cursor_db(h, "proj-b", "store-3", &[("run-2", RUN)]);
    let again = scan(h, &["cursor"], Some(&answer));
    assert_eq!(sums(&again, "cursor")[0], 600);
    assert_eq!(sums(&again, "cursor")[4], 2);
}

// ----- A3: file counts are true -------------------------------------------------------------------

/// Argon M2: a database that cannot be opened is one file found and one failed, not two found.
#[test]
fn a_failed_database_is_counted_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let db = h.join(".cursor/projects/p/sdk-agent-store/s/index.db");
    write(
        &db,
        "this is not a database, and longer than one SQLite header page would be...",
    );
    let answer = scan(h, &["cursor"], None);
    let counts = &answer["discovery"]["kinds"]["cursor"]["counts"];
    assert_eq!(counts["filesFound"], 1, "{counts}");
    assert_eq!(counts["filesFailed"], 1, "{counts}");
    // Every store failed: the kind failed, never "ok" (Argon H3).
    assert_eq!(answer["kinds"]["cursor"]["state"], "error");
}

/// Argon M2: zcode reports its own open failures.
#[test]
fn a_zcode_database_that_cannot_be_read_is_counted_as_failed() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".zcode/cli/db/db.sqlite"),
        "this is not a database at all, not even close to one",
    );
    let answer = scan(h, &["zcode"], None);
    assert_eq!(answer["kinds"]["zcode"]["state"], "error");
    assert_eq!(answer["discovery"]["kinds"]["zcode"]["counts"]["filesFailed"], 1);
}

// ----- B4: unreadable folders ---------------------------------------------------------------------

#[cfg(unix)]
fn deny(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
}

#[cfg(unix)]
fn allow(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

/// Argon H1, Astra M4, Sol M5, Wonder M2, Fable M7: `~/.codex/sessions` at mode 000 with a log in
/// it is a named unreadable root and a partial kind. b09066667: "not_installed", root
/// "installed_no_logs", nothing failed.
#[cfg(unix)]
#[test]
fn an_unreadable_log_folder_is_named_never_empty() {
    if unsafe { libc::geteuid() } == 0 {
        return; // root reads anything: the case cannot be built
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let sessions = h.join(".codex/sessions");
    write(&sessions.join(rollout_name(ID)), &rollout(ID, &[100]));
    deny(&sessions);
    let answer = scan(h, &["codex"], None);
    allow(&sessions);
    assert_eq!(
        answer["kinds"]["codex"]["partial"], true,
        "{}",
        answer["kinds"]["codex"]
    );
    let roots = answer["discovery"]["kinds"]["codex"]["roots"].as_array().unwrap();
    assert!(
        roots
            .iter()
            .any(|r| r["state"] == "unreadable" && r["cause"] == "permission_denied"),
        "{roots:?}"
    );
}

/// The same for a folder inside a readable root: counted, and the kind is partial.
#[cfg(unix)]
#[test]
fn an_unreadable_folder_inside_a_root_is_counted() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let month = h.join(".codex/sessions/2026/11");
    write(&month.join("11").join(rollout_name(ID)), &rollout(ID, &[100]));
    deny(&month);
    let answer = scan(h, &["codex"], None);
    allow(&month);
    assert_eq!(answer["kinds"]["codex"]["partial"], true);
    assert_eq!(answer["kinds"]["codex"]["unreadableFolders"], 1);
}

// ----- B5: Cursor stores --------------------------------------------------------------------------

/// Astra M5, Sol M4: a run that fits neither convention is counted unreadable, and an unchanged
/// store says so again. b09066667: the second, unchanged scan was "ok" with no note.
#[test]
fn an_unchanged_store_keeps_its_unreadable_runs() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    cursor_db(
        h,
        "p",
        "s",
        &[
            ("run-1", RUN),
            (
                "run-2",
                r#"{"inputTokens":300,"outputTokens":50,"cacheReadTokens":10,"cacheWriteTokens":0,"totalTokens":999}"#,
            ),
        ],
    );
    let first = scan(h, &["cursor"], None);
    assert_eq!(first["kinds"]["cursor"]["unreadableRecords"], 1);
    let second = scan(h, &["cursor"], Some(&first));
    assert_eq!(second["kinds"]["cursor"]["partial"], true);
    assert_eq!(second["kinds"]["cursor"]["unreadableRecords"], 1);
}

/// Sol M4: the same for Devin.
#[test]
fn an_unchanged_devin_database_keeps_its_unreadable_replies() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let db = h.join(".local/share/devin/cli/sessions.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE sessions (id TEXT PRIMARY KEY, working_directory TEXT, model TEXT);
             CREATE TABLE message_nodes (row_id INTEGER PRIMARY KEY, session_id TEXT, chat_message TEXT,
               created_at INTEGER);
             INSERT INTO sessions VALUES ('s-1', '/p', 'swe-2-max');",
        )
        .unwrap();
    for (row, input) in [(1, json!(100)), (2, json!("lots"))] {
        let message = json!({"message_id": format!("m-{row}"), "metadata": {"created_at": AT,
            "metrics": {"input_tokens": input, "output_tokens": 40}}});
        connection
            .execute(
                "INSERT INTO message_nodes VALUES (?1, 's-1', ?2, 1789771814)",
                rusqlite::params![row, message.to_string()],
            )
            .unwrap();
    }
    drop(connection);
    let first = scan(h, &["devin"], None);
    assert_eq!(first["kinds"]["devin"]["unreadableRecords"], 1);
    let second = scan(h, &["devin"], Some(&first));
    assert_eq!(second["kinds"]["devin"]["unreadableRecords"], 1);
}

/// Astra M8, Wonder L8: a run without its input counter is unreadable, never 0 input.
#[test]
fn a_missing_cursor_counter_is_unreadable_never_zero() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    cursor_db(
        h,
        "p",
        "s",
        &[(
            "run-1",
            r#"{"outputTokens":10,"cacheReadTokens":0,"cacheWriteTokens":0}"#,
        )],
    );
    let answer = scan(h, &["cursor"], None);
    assert_eq!(sums(&answer, "cursor"), [0, 0, 0, 0, 0]);
    assert_eq!(answer["kinds"]["cursor"]["unreadableRecords"], 1);
}

/// Astra M6: a row cap that drops a run leaves the store unconfirmed and the kind partial, so the
/// next scan reads it again. b09066667 confirmed it: the dropped run was never read.
#[test]
fn a_row_cap_never_confirms_a_store() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let db = cursor_db(h, "p", "s", &[]);
    let mut connection = Connection::open(&db).unwrap();
    let batch = connection.transaction().unwrap();
    for n in 0..50_001u64 {
        // A distinct model per run: one hourly row each.
        batch
            .execute(
                "INSERT INTO runs VALUES (?1, 'agent-1', ?2, ?3, ?4, ?4)",
                rusqlite::params![format!("run-{n}"), format!("model-{n}"), RUN, AT],
            )
            .unwrap();
    }
    batch.commit().unwrap();
    drop(connection);
    let answer = scan(h, &["cursor"], None);
    assert_eq!(answer["kinds"]["cursor"]["partial"], true);
    assert!(
        answer["kinds"]["cursor"]["fingerprints"]
            .as_object()
            .unwrap()
            .is_empty()
    );
}

// ----- B6: limits are always reported -------------------------------------------------------------

/// Astra M7, Fable M4: 65 experiment folders of Codex logs. The search keeps 64 and says so; the
/// kind is partial, so it is never "complete" and never taken over. b09066667: no word, not partial.
#[test]
fn an_experiment_search_limit_is_named_on_the_kind() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    for n in 0..65 {
        let id = format!("bbbbbbbb-0000-0000-0000-{n:012}");
        write(
            &h.join(format!("lab/run-{n:03}/codex")).join(rollout_name(&id)),
            &rollout(&id, &[100]),
        );
    }
    let answer = scan(h, &["codex"], None);
    assert_eq!(answer["kinds"]["codex"]["partial"], true);
    assert_eq!(
        answer["kinds"]["codex"]["searchLimit"],
        json!({"read": 64, "found": 65})
    );
    assert_eq!(sums(&answer, "codex")[0], 6400);
}

/// Fable M4 (T05d): a Muse folder with many session folders is one root and never uses up the
/// other kinds' search. b09066667 counted each session folder against one cap for all kinds.
#[test]
fn many_muse_sessions_never_use_up_another_kinds_search() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    for n in 0..100 {
        write(
            &h.join(format!("lab/a-run/muse/bucket-{n:03}/sess-{n:03}/session.jsonl")),
            &muse_session(1),
        );
    }
    write(&h.join("lab/z-run/codex").join(rollout_name(ID)), &rollout(ID, &[100]));
    let answer = scan(h, &["codex", "muse"], None);
    assert_eq!(sums(&answer, "codex")[0], 100);
    assert_eq!(sums(&answer, "muse")[4], 100);
    assert!(answer["kinds"]["codex"].get("searchLimit").is_none());
}

/// Sol L3: home with more children than the signature search looks at says so.
#[test]
fn a_signature_search_limit_is_named() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    for n in 0..520 {
        fs::create_dir_all(h.join(format!("dir-{n:04}"))).unwrap();
    }
    let answer = scan(h, &["codex"], None);
    assert_eq!(answer["kinds"]["codex"]["searchLimit"]["found"], 520);
}

// ----- C8: non-ASCII names ------------------------------------------------------------------------

/// Astra M9, Sol M6, Wonder M4, Fable M6: names whose byte length puts a cut inside a character.
/// Every one answers normally. b09066667 panicked (and aborted, in the release build).
#[test]
fn multibyte_names_never_cut_inside_a_character() {
    let pieces = ["é", "用户", "界", "€", "日本", "ж"];
    for piece in pieces {
        for pad in 25..45 {
            let home = tempfile::tempdir().unwrap();
            let h = home.path();
            // A Claude project folder of about 40 bytes, and a Codex log name with multi-byte text.
            let folder = format!("{}{}", piece.repeat(4), "a".repeat(pad));
            let claude = h.join(".claude/projects").join(&folder).join("s.jsonl");
            write(&claude, "{}\n");
            let codex = h
                .join(".codex/sessions")
                .join(format!("rollout-{}{}.jsonl", piece.repeat(3), "a".repeat(pad)));
            write(&codex, &rollout("x", &[1]));
            let answer = scan(h, &["claude", "codex"], None);
            assert_eq!(answer["kinds"]["codex"]["state"], "ok");
        }
    }
}

// ----- C9: non-regular files ----------------------------------------------------------------------

/// Sol M8: a FIFO at T3's settings file, with no writer, never holds the scan up. b09066667 blocked
/// on it for good.
#[cfg(unix)]
#[test]
fn a_fifo_settings_file_never_holds_up_the_scan() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path().to_path_buf();
    let settings = h.join(".t3/userdata/settings.json");
    fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let text = std::ffi::CString::new(settings.to_string_lossy().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(text.as_ptr(), 0o600) }, 0);
    write(&h.join(".codex/sessions").join(rollout_name(ID)), &rollout(ID, &[100]));
    let (sender, receiver) = std::sync::mpsc::channel();
    let path = h.clone();
    std::thread::spawn(move || {
        let _ = sender.send(scan(&path, &["codex"], None));
    });
    let answer = receiver
        .recv_timeout(Duration::from_secs(20))
        .expect("the scan answered");
    assert_eq!(sums(&answer, "codex")[0], 100);
}

/// The answer for a scan that did not finish names every kind pending (the server keeps what it
/// had) and says it stalled.
#[test]
fn a_stalled_scan_answers_pending() {
    let answer = protocol::stalled_answer(&ask(&["codex", "cursor"], None));
    assert_eq!(answer["stalled"], true);
    assert_eq!(answer["kinds"]["codex"]["state"], "pending");
    assert_eq!(answer["kinds"]["cursor"]["state"], "pending");
}

// ----- C11: SQLite URIs ---------------------------------------------------------------------------

/// Astra M10, Argon L2: `ZCODE_DB_PATH` with `%20` in a folder name reads that folder, never the
/// folder `account one` beside it. b09066667 read the other database (900 input).
#[test]
fn a_percent_sequence_in_a_zcode_path_reads_that_path() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let zcode = |dir: &str, input: i64| {
        let db = h.join(dir).join("db.sqlite");
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        let connection = Connection::open(&db).unwrap();
        connection
            .execute_batch(
                "CREATE TABLE model_usage (session_id TEXT, model_id TEXT, provider_id TEXT, started_at INTEGER,
                   input_tokens INTEGER, output_tokens INTEGER, cache_read_input_tokens INTEGER,
                   cache_creation_input_tokens INTEGER);",
            )
            .unwrap();
        connection
            .execute(
                "INSERT INTO model_usage VALUES ('s', 'glm-5', 'zai', 1790000000000, ?1, 1, 0, 0)",
                [input],
            )
            .unwrap();
        db
    };
    let configured = zcode("account%20one", 100);
    zcode("account one", 900);
    let mut environment = env(h);
    environment.insert("ZCODE_DB_PATH".into(), configured.to_string_lossy().into_owned());
    let answer = protocol::scan(&ask(&["zcode"], None), h, &environment);
    assert_eq!(sums(&answer, "zcode")[0], 100);
}

/// Argon H3: SQLite paths on Windows are written verbatim (no 260-character limit), and the `?`
/// of the verbatim prefix survives the URI's encoding.
#[test]
fn sqlite_uris_encode_percent_and_keep_verbatim_paths() {
    use aac_usage::read::sqlite::{uri, verbatim};
    assert_eq!(verbatim(r"C:\Users\u\x.db"), r"\\?\C:\Users\u\x.db");
    assert_eq!(verbatim(r"\\server\share\x.db"), r"\\?\UNC\server\share\x.db");
    assert_eq!(verbatim(r"\\?\C:\x.db"), r"\\?\C:\x.db");
    #[cfg(unix)]
    assert_eq!(
        uri(Path::new("/h/a%20b/x?.db#1"), true),
        "file:/h/a%2520b/x%3F.db%231?mode=ro&immutable=1"
    );
}

/// Argon H3 on Windows itself: a Cursor store, a Devin and a zcode database past 260 characters.
#[cfg(windows)]
#[test]
fn sqlite_databases_past_260_characters_are_read() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let project = "p".repeat(200);
    cursor_db(h, &project, "s1", &[("run-1", RUN)]);
    let answer = scan(h, &["cursor"], None);
    assert_eq!(answer["kinds"]["cursor"]["state"], "ok");
    assert_eq!(sums(&answer, "cursor")[0], 300);
}

/// Argon H2: Windows names the path variable `Path`; binaries on it are found.
#[cfg(windows)]
#[test]
fn a_binary_on_a_windows_path_variable_of_any_case_is_found() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let bin = h.join("bin");
    fs::create_dir_all(&bin).unwrap();
    fs::write(bin.join("codex.cmd"), "@echo off\r\n").unwrap();
    let environment = aac_usage::discover::env_from(vec![
        ("USERPROFILE".to_string(), h.to_string_lossy().into_owned()),
        ("Path".to_string(), bin.to_string_lossy().into_owned()),
    ]);
    let answer = protocol::scan(&ask(&["codex"], None), h, &environment);
    let signals = answer["discovery"]["kinds"]["codex"]["signals"].as_array().unwrap();
    assert!(signals.iter().any(|s| s == "binary"), "{signals:?}");
}

// ----- D15: cloud-synced folders ------------------------------------------------------------------

/// Argon M5, Fable L12: walks from home never enter OneDrive or personal folders.
#[test]
fn walks_from_home_never_enter_cloud_or_personal_folders() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    for folder in ["OneDrive", "OneDrive - Company", "Documents", "Dropbox"] {
        let id = format!("cccccccc-0000-0000-0000-{:012}", folder.len());
        write(
            &h.join(folder).join("lab/codex").join(rollout_name(&id)),
            &rollout(&id, &[100]),
        );
    }
    let answer = scan(h, &["codex"], None);
    assert_eq!(sums(&answer, "codex")[0], 0);
}
