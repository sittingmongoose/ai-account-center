//! Activity is dated only by usage-bearing evidence (synthetic homes only, never the real home).
//! A log, database or chat store touched without usage must not make a source look stale, and a
//! real usage event must still advance both clocks.

use aac_usage::{discover::Env, protocol, util::iso_ms};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path, path::PathBuf};

const HOUR: i64 = 3_600_000;

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn touched(path: &Path, at: i64) {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(at as u64);
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
}

fn write(path: &Path, text: &str) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
    path.to_path_buf()
}

fn request(kinds: &[&str], prior: Option<&Value>) -> protocol::Request {
    request_with(kinds, prior, json!({}))
}

fn request_with(kinds: &[&str], prior: Option<&Value>, extra: Value) -> protocol::Request {
    let fingerprints: Value = prior
        .map(|answer| {
            let mut map = serde_json::Map::new();
            for kind in kinds {
                map.insert((*kind).to_string(), answer["kinds"][kind]["fingerprints"].clone());
            }
            Value::Object(map)
        })
        .unwrap_or_else(|| json!({}));
    let mut body = json!({
        "kinds": kinds,
        "minDateMs": 0,
        "deadlineMs": 60000,
        "presence": false,
        "fingerprints": fingerprints,
    });
    if let (Some(body), Some(extra)) = (body.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            body.insert(key.clone(), value.clone());
        }
    }
    protocol::parse_request(body.to_string().as_bytes()).unwrap()
}

fn scan_with(home: &Path, kinds: &[&str], prior: Option<&Value>, extra: Value) -> Value {
    let env = Env::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), String::new()),
        (
            "XDG_CONFIG_HOME".into(),
            home.join("config").to_string_lossy().into_owned(),
        ),
    ]);
    protocol::scan(&request_with(kinds, prior, extra), home, &env)
}

fn scan(home: &Path, kinds: &[&str], prior: Option<&Value>) -> Value {
    let env = Env::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), String::new()),
        (
            "XDG_CONFIG_HOME".into(),
            home.join("config").to_string_lossy().into_owned(),
        ),
    ]);
    protocol::scan(&request(kinds, prior), home, &env)
}

/// State, data clock, activity clock and whether the usage read was complete.
fn clock(answer: &Value, kind: &str) -> (String, Option<i64>, Option<i64>, bool) {
    let freshness = &answer["discovery"]["kinds"][kind]["freshness"];
    (
        freshness["state"].as_str().unwrap().to_string(),
        freshness["newestDataMs"].as_i64(),
        freshness["newestActivityMs"].as_i64(),
        freshness["activityComplete"].as_bool().unwrap(),
    )
}

fn current(at: i64) -> (String, Option<i64>, Option<i64>, bool) {
    ("current".into(), Some(at), Some(at), true)
}

/// One OMP session event carrying usage, or a probe event that carries none.
fn omp_event(at: i64, usage: bool) -> String {
    let message = if usage {
        json!({"role":"assistant","model":"fixture-model","provider":"fixture-provider",
               "usage":{"input":100,"output":20,"cacheRead":0,"cacheWrite":0}})
    } else {
        json!({"role":"user","content":"probe without a model call"})
    };
    json!({"type":"message","timestamp":at,"message":message}).to_string() + "\n"
}

#[test]
fn omp_probe_sessions_never_date_activity() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let at1 = (now_ms() - 6 * HOUR) / 1000 * 1000;
    let real = write(
        &h.join(".omp/agent/sessions/2026-11-20T10-00-00_fixture.jsonl"),
        &omp_event(at1, true),
    );
    let first = scan(h, &["omp"], None);
    assert_eq!(clock(&first, "omp"), current(at1));

    // An hourly probe session, written now and holding no usage at all, is metadata only.
    let probe = write(
        &h.join(".omp/agent/sessions/2026-11-20T16-00-00_probe.jsonl"),
        &omp_event(now_ms() - HOUR, false),
    );
    touched(&probe, now_ms() - HOUR);
    let quiet = scan(h, &["omp"], Some(&first));
    assert_eq!(
        quiet["discovery"]["kinds"]["omp"]["counts"]["filesRead"].as_u64(),
        Some(2)
    );
    assert_eq!(clock(&quiet, "omp"), current(at1));

    // A real usage event still advances the data and the activity clock.
    let at2 = (now_ms() - 2 * HOUR) / 1000 * 1000;
    let mut text = fs::read_to_string(&real).unwrap();
    text.push_str(&omp_event(at2, true));
    fs::write(&real, text).unwrap();
    let grown = scan(h, &["omp"], Some(&quiet));
    assert_eq!(clock(&grown, "omp"), current(at2));
}

fn muse_event(at: i64, usage: bool) -> String {
    let event = if usage {
        json!({"kind":"model_completed","model":"fixture-model",
               "usage":{"input_tokens":100,"output_tokens":20,"cache_read_tokens":0,"cache_write_tokens":0}})
    } else {
        json!({"kind":"model_requested","model":"fixture-model"})
    };
    json!({"recorded_at":at,"payload":{"event":event}}).to_string() + "\n"
}

#[test]
fn muse_session_files_touched_without_a_completed_model_never_date_activity() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let at1 = (now_ms() - 6 * HOUR) / 1000 * 1000;
    let real = write(
        &h.join(".local/share/muse/sessions/session-one/session.jsonl"),
        &muse_event(at1, true),
    );
    let first = scan(h, &["muse"], None);
    assert_eq!(clock(&first, "muse"), current(at1));

    // The 18:28Z update touched every Muse session file without completing a model call.
    touched(&real, now_ms() - HOUR);
    let probe = write(
        &h.join(".local/share/muse/sessions/session-two/session.jsonl"),
        &muse_event(now_ms() - HOUR, false),
    );
    touched(&probe, now_ms() - HOUR);
    let quiet = scan(h, &["muse"], Some(&first));
    assert_eq!(
        quiet["discovery"]["kinds"]["muse"]["counts"]["filesRead"].as_u64(),
        Some(2)
    );
    assert_eq!(clock(&quiet, "muse"), current(at1));

    let at2 = (now_ms() - 2 * HOUR) / 1000 * 1000;
    let mut text = fs::read_to_string(&real).unwrap();
    text.push_str(&muse_event(at2, true));
    fs::write(&real, text).unwrap();
    let grown = scan(h, &["muse"], Some(&quiet));
    assert_eq!(clock(&grown, "muse"), current(at2));
}

fn zcode_db(home: &Path) -> PathBuf {
    let db = home.join(".zcode/cli/db/db.sqlite");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE model_usage (session_id TEXT, model_id TEXT, provider_id TEXT,
              started_at INTEGER, input_tokens INTEGER, output_tokens INTEGER,
              cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER);",
        )
        .unwrap();
    db
}

fn zcode_row(db: &Path, at: i64, input: i64) {
    Connection::open(db)
        .unwrap()
        .execute(
            "INSERT INTO model_usage VALUES ('session-one', 'fixture-model', 'fixture-provider', ?1, ?2, 20, 0, 0)",
            rusqlite::params![at, input],
        )
        .unwrap();
}

#[test]
fn zcode_database_touches_and_thread_records_never_date_activity() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let at1 = (now_ms() - 6 * HOUR) / 1000 * 1000;
    let db = zcode_db(h);
    zcode_row(&db, at1, 100);
    let first = scan(h, &["zcode"], None);
    assert_eq!(clock(&first, "zcode"), current(at1));

    // A database touch with no new row, an empty write-ahead log and T3's thread record for the
    // same provider instance: all metadata, none of it usage.
    touched(&db, now_ms() - HOUR);
    write(&h.join(".zcode/cli/db/db.sqlite-wal"), "");
    let t3 = h.join(".t3/userdata/statev2.sqlite");
    fs::create_dir_all(t3.parent().unwrap()).unwrap();
    Connection::open(&t3)
        .unwrap()
        .execute_batch(
            "CREATE TABLE orchestration_v2_projection_provider_threads (driver TEXT, provider_instance_id TEXT, updated_at TEXT);
             INSERT INTO orchestration_v2_projection_provider_threads VALUES ('zcode', 'fixture-zcode', '2026-11-20T18:31:43Z');",
        )
        .unwrap();
    write(
        &h.join(".t3/userdata/settings.json"),
        &json!({"providerInstances":{"fixture-zcode":{"driver":"acp","enabled":true,
            "config":{"command":"zcode-acp"}}}})
        .to_string(),
    );
    let quiet = scan(h, &["zcode"], Some(&first));
    assert_eq!(clock(&quiet, "zcode"), current(at1));

    // A real message row still advances the data clock.
    let at2 = (now_ms() - 2 * HOUR) / 1000 * 1000;
    zcode_row(&db, at2, 220);
    let grown = scan(h, &["zcode"], Some(&quiet));
    assert_eq!(clock(&grown, "zcode"), current(at2));
}

fn cursor_store(home: &Path) -> PathBuf {
    let db = home.join(".cursor/projects/project/sdk-agent-store/store/index.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(&db)
        .unwrap()
        .execute_batch(
            "CREATE TABLE runs (run_id TEXT, agent_id TEXT, model TEXT, started_at TEXT,
              finished_at TEXT, usage_json TEXT);",
        )
        .unwrap();
    db
}

fn cursor_run(db: &Path, stamp: &str, input: u64) {
    Connection::open(db)
        .unwrap()
        .execute(
            "INSERT INTO runs VALUES (?1, 'agent-one', 'fixture-model', ?2, ?2, ?3)",
            rusqlite::params![
                format!("run-{input}"),
                stamp,
                json!({"inputTokens":input,"outputTokens":20,"cacheReadTokens":0,
                       "cacheWriteTokens":0,"totalTokens":input + 20})
                .to_string()
            ],
        )
        .unwrap();
}

#[test]
fn cursor_chats_without_token_counts_never_date_activity() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let (first_run, second_run) = ("2026-11-11T21:00:00Z", "2026-11-12T21:00:00Z");
    let at1 = iso_ms(first_run).unwrap();
    let at2 = iso_ms(second_run).unwrap();
    let store = cursor_store(h);
    cursor_run(&store, first_run, 100);
    let first = scan(h, &["cursor"], None);
    assert_eq!(clock(&first, "cursor"), current(at1));

    // CLI transcripts and chat stores carry no numeric usage. Newer files must not claim staleness,
    // and no token counts may be invented from them.
    let chat = h.join(".cursor/chats/project/chat/store.db");
    fs::create_dir_all(chat.parent().unwrap()).unwrap();
    Connection::open(&chat)
        .unwrap()
        .execute_batch("CREATE TABLE blobs (id TEXT, data BLOB); CREATE TABLE meta (key TEXT, value TEXT);")
        .unwrap();
    touched(&chat, now_ms() - HOUR);
    let transcript = write(
        &h.join(".cursor/projects/project/agent-transcripts/transcript-one.jsonl"),
        "{\"role\":\"user\",\"content\":\"a chat without token counts\"}\n",
    );
    touched(&transcript, now_ms() - HOUR);
    let quiet = scan(h, &["cursor"], None);
    assert_eq!(
        quiet["discovery"]["kinds"]["cursor"]["unmeasuredFiles"].as_u64(),
        Some(1)
    );
    assert_eq!(clock(&quiet, "cursor"), current(at1));
    let inputs: u64 = quiet["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["i"].as_u64().unwrap())
        .sum();
    assert_eq!(inputs, 100);

    // A completed SDK run with usage still advances the data clock.
    cursor_run(&store, second_run, 220);
    let grown = scan(h, &["cursor"], Some(&quiet));
    assert_eq!(clock(&grown, "cursor"), current(at2));
}

// ---- Medium 1: a usage-shaped append the reader cannot decode ------------------------------

/// OMP's real field order: role, content, model, usage. A tear usually lands inside `content`,
/// before any `"usage"` text exists.
fn torn_omp_event(at: i64) -> String {
    format!(
        "{{\"type\":\"message\",\"timestamp\":{at},\"message\":{{\"role\":\"assistant\",\
         \"content\":[{{\"type\":\"text\",\"text\":\"a long answer cut mid-wri"
    )
}

/// The rest of `torn_omp_event`: closes the text, names the model and carries the usage.
const TORN_OMP_REST: &str = "ting\"}],\"model\":\"fixture-model\",\
    \"usage\":{\"input\":220,\"output\":30,\"cacheRead\":0,\"cacheWrite\":0}}}";

#[test]
fn an_unreadable_omp_usage_append_is_unknown_and_retained() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let at1 = (now_ms() - 6 * HOUR) / 1000 * 1000;
    let at2 = (now_ms() - 2 * HOUR) / 1000 * 1000;
    let log = write(
        &h.join(".omp/agent/sessions/2026-11-20T10-00-00_fixture.jsonl"),
        &omp_event(at1, true),
    );
    let first = scan(h, &["omp"], None);
    assert_eq!(clock(&first, "omp"), current(at1));

    // A torn usage record appended by an interrupted write cannot be measured. The measured
    // date is kept, but coverage is unverified: never current.
    let torn = torn_omp_event(at2);
    let mut text = fs::read_to_string(&log).unwrap();
    text.push_str(&torn);
    fs::write(&log, text).unwrap();
    let unread = scan(h, &["omp"], Some(&first));
    assert_eq!(clock(&unread, "omp"), ("unknown".into(), Some(at1), Some(at1), false));
    assert_eq!(unread["kinds"]["omp"]["unreadUsage"], true);
    assert_eq!(
        unread["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        100
    );

    // The evidence travels on the fingerprint through an unchanged rescan.
    let again = scan(h, &["omp"], Some(&unread));
    assert_eq!(clock(&again, "omp"), ("unknown".into(), Some(at1), Some(at1), false));
    assert_eq!(again["kinds"]["omp"]["unreadUsage"], true);
    assert!(again["rows"].as_array().unwrap().is_empty());

    // Completing the record at the same bytes verifies the usage and clears the flag.
    let mut text = fs::read_to_string(&log).unwrap();
    text.push_str(TORN_OMP_REST);
    text.push('\n');
    fs::write(&log, text).unwrap();
    let repaired = scan(h, &["omp"], Some(&again));
    assert_eq!(clock(&repaired, "omp"), current(at2));
    assert!(repaired["kinds"]["omp"].get("unreadUsage").is_none());
    assert_eq!(
        repaired["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        320
    );
}

// ---- Medium 2: a write-ahead log the read could not see -----------------------------------

#[cfg(unix)]
#[test]
fn cursor_unread_wal_is_incomplete_and_retried() {
    use std::os::unix::fs::PermissionsExt;

    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let (first_run, second_run) = ("2026-11-11T21:00:00Z", "2026-11-12T21:00:00Z");
    let at1 = iso_ms(first_run).unwrap();
    let at2 = iso_ms(second_run).unwrap();
    // A staged store with a live write-ahead log: the first run is checkpointed, the second is
    // committed only in the log.
    let stage = h.join("stage");
    fs::create_dir_all(&stage).unwrap();
    let source = stage.join("index.db");
    let writer = Connection::open(&source).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             CREATE TABLE runs (run_id TEXT, agent_id TEXT, model TEXT, started_at TEXT,
               finished_at TEXT, usage_json TEXT);",
        )
        .unwrap();
    for (stamp, input) in [(first_run, 100u64), (second_run, 220)] {
        writer
            .execute(
                "INSERT INTO runs VALUES (?1, 'agent-one', 'fixture-model', ?2, ?2, ?3)",
                rusqlite::params![
                    format!("run-{input}"),
                    stamp,
                    json!({"inputTokens":input,"outputTokens":20,"cacheReadTokens":0,
                           "cacheWriteTokens":0,"totalTokens":input + 20})
                    .to_string()
                ],
            )
            .unwrap();
        if input == 100 {
            writer.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
        }
    }
    let store = h.join(".cursor/projects/project/sdk-agent-store/store/index.db");
    fs::create_dir_all(store.parent().unwrap()).unwrap();
    let wal = |path: &Path| PathBuf::from(format!("{}-wal", path.display()));
    fs::copy(&source, &store).unwrap();
    fs::copy(wal(&source), wal(&store)).unwrap();
    drop(writer);
    fs::remove_dir_all(&stage).ok();

    // No shared-memory index can be created in a read-only folder: only the checkpointed run is
    // visible, and the source must say so.
    fs::set_permissions(store.parent().unwrap(), fs::Permissions::from_mode(0o555)).unwrap();
    let blind = scan(h, &["cursor"], None);
    assert_eq!(blind["kinds"]["cursor"]["walUnread"], true);
    assert_eq!(clock(&blind, "cursor"), ("unknown".into(), Some(at1), Some(at1), false));
    assert_eq!(
        blind["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        100
    );

    // Once the folder is writable the normal read-only open must be retried even though the
    // database and log bytes are unchanged, and its committed run counted.
    fs::set_permissions(store.parent().unwrap(), fs::Permissions::from_mode(0o755)).unwrap();
    let retried = scan(h, &["cursor"], Some(&blind));
    assert!(retried["kinds"]["cursor"].get("walUnread").is_none());
    assert_eq!(clock(&retried, "cursor"), current(at2));
    assert_eq!(
        retried["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        320
    );
}

#[test]
fn zcode_unread_wal_is_incomplete_coverage() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let at1 = 1_794_430_860_000i64;
    let db = zcode_db(h);
    let writer = Connection::open(&db).unwrap();
    writer
        .execute_batch(
            "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
             INSERT INTO model_usage VALUES ('s', 'fixture-model', 'fixture-provider', 1794430860000, 100, 20, 0, 0);
             PRAGMA wal_checkpoint(TRUNCATE);
             INSERT INTO model_usage VALUES ('s', 'fixture-model', 'fixture-provider', 1794517260000, 220, 30, 0, 0);",
        )
        .unwrap();
    // Immutable reads never see the log: the measured date stays at the checkpointed row and
    // coverage is unverified, never current.
    let answer = scan_with(h, &["zcode"], None, json!({"immutableSqlite": true}));
    assert_eq!(answer["kinds"]["zcode"]["walUnread"], true);
    assert_eq!(clock(&answer, "zcode"), ("unknown".into(), Some(at1), Some(at1), false));
    drop(writer);
}

// ---- Medium 4: a usage record with no event timestamp -------------------------------------

#[cfg(unix)]
fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

#[cfg(unix)]
fn field_number(field: u64, value: u64) -> Vec<u8> {
    [varint(field * 8), varint(value)].concat()
}

#[cfg(unix)]
fn field_bytes(field: u64, value: &[u8]) -> Vec<u8> {
    [varint(field * 8 + 2), varint(value.len() as u64), value.to_vec()].concat()
}

#[cfg(unix)]
#[test]
fn an_untimed_antigravity_step_is_counted_without_a_usage_date() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let db = h.join(".gemini/antigravity/conversations/fixture.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch("CREATE TABLE steps (idx INTEGER, metadata BLOB)")
        .unwrap();
    // Model and counters, no step timestamp and no trajectory: the reader's only date would be
    // the file time.
    let metadata = [
        field_bytes(24, &field_bytes(12, b"claude-opus-4-6")),
        field_bytes(9, &[field_number(2, 100), field_number(3, 20)].concat()),
    ]
    .concat();
    connection
        .execute("INSERT INTO steps VALUES (1, ?1)", [metadata])
        .unwrap();
    drop(connection);

    let first = scan(h, &["antigravity"], None);
    assert_eq!(
        first["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        100
    );
    assert_eq!(first["kinds"]["antigravity"]["estimatedClock"], true);
    assert_eq!(clock(&first, "antigravity"), ("unknown".into(), None, None, true));

    // A file-time change alone is not a usage date.
    touched(&db, now_ms() + 50 * 60 * 1000);
    let moved = scan(h, &["antigravity"], None);
    assert_eq!(clock(&moved, "antigravity"), ("unknown".into(), None, None, true));
    assert_eq!(
        moved["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        100
    );
}

#[test]
fn an_omp_tear_inside_content_and_a_joined_record_are_both_accounted_for() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let at1 = (now_ms() - 6 * HOUR) / 1000 * 1000;
    let at2 = (now_ms() - 4 * HOUR) / 1000 * 1000;
    let at3 = (now_ms() - 2 * HOUR) / 1000 * 1000;
    let log = h.join(".omp/agent/sessions/2026-11-20T10-00-00_fixture.jsonl");
    let mut bytes = omp_event(at1, true).into_bytes();
    // A tear lands inside `content`, before any `"usage"` text exists, and splits a multi-byte
    // character (`U+1F600`): the line is not valid UTF-8.
    bytes.extend_from_slice(
        format!(
            "{{\"type\":\"message\",\"timestamp\":{at2},\"message\":{{\"role\":\"assistant\",\
             \"content\":[{{\"type\":\"text\",\"text\":\"an emoji "
        )
        .as_bytes(),
    );
    bytes.extend_from_slice(&[0xF0, 0x9F]);
    // The writer continued on the same line: a whole usage record follows the torn bytes.
    bytes.extend_from_slice(
        json!({"type":"message","timestamp":at3,
               "message":{"role":"assistant","model":"fixture-model",
                          "usage":{"input":330,"output":40,"cacheRead":0,"cacheWrite":0}}})
        .to_string()
        .as_bytes(),
    );
    bytes.push(b'\n');
    write(&log, ""); // create the session folder
    fs::write(&log, bytes).unwrap();

    let first = scan(h, &["omp"], None);
    assert_eq!(clock(&first, "omp"), ("unknown".into(), Some(at3), Some(at3), false));
    assert_eq!(first["kinds"]["omp"]["unreadUsage"], true);
    assert_eq!(
        first["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        430
    );

    // The evidence and the joined record both survive an unchanged rescan.
    let again = scan(h, &["omp"], Some(&first));
    assert_eq!(clock(&again, "omp"), ("unknown".into(), Some(at3), Some(at3), false));
    assert_eq!(again["kinds"]["omp"]["unreadUsage"], true);
    assert!(again["rows"].as_array().unwrap().is_empty());
}

#[test]
fn omp_tears_before_role_are_unread_even_with_a_joined_user_record() {
    let at = (now_ms() - 2 * HOUR) / 1000 * 1000;
    for (prefix, unread) in [
        (r#"{"type":"message","id":"fixture","message":{"ro"#, true),
        (r#"{"type":"message","id":"fixture","message":{"role":"ass"#, true),
        (
            r#"{"type":"message","id":"fixture","message":{"role":"user","content":"torn"#,
            false,
        ),
        (
            r#"{"type":"message","id":"fixture","message":{"role":"toolResult","content":"torn"#,
            false,
        ),
    ] {
        let home = tempfile::tempdir().unwrap();
        let log = home
            .path()
            .join(".omp/agent/sessions/2026-11-20T10-00-00_fixture.jsonl");
        write(
            &log,
            &format!(
                "{}{prefix}{{\"type\":\"message\",\"message\":{{\"role\":\"user\"}}}}\n",
                omp_event(at, true),
            ),
        );
        let result = scan(home.path(), &["omp"], None);
        assert_eq!(result["kinds"]["omp"]["unreadUsage"].as_bool().unwrap_or(false), unread);
        assert_eq!(
            clock(&result, "omp"),
            (
                if unread { "unknown" } else { "current" }.into(),
                Some(at),
                Some(at),
                !unread
            ),
        );
    }
}

#[cfg(unix)]
#[test]
fn an_estimated_antigravity_date_is_disclosed_across_unchanged_rescans() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let db = h.join(".gemini/antigravity/conversations/fixture.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch("CREATE TABLE steps (idx INTEGER, metadata BLOB)")
        .unwrap();
    // One timed step (a logged event date) and one untimed step (only the file time).
    let timed = [
        field_bytes(24, &field_bytes(12, b"claude-opus-4-6")),
        field_bytes(8, &field_number(1, 1_794_430_860)),
        field_bytes(9, &[field_number(2, 100), field_number(3, 20)].concat()),
        field_bytes(11, b"call-timed"),
    ]
    .concat();
    let untimed = [
        field_bytes(24, &field_bytes(12, b"claude-opus-4-6")),
        field_bytes(9, &[field_number(2, 220), field_number(3, 30)].concat()),
        field_bytes(11, b"call-untimed"),
    ]
    .concat();
    connection.execute("INSERT INTO steps VALUES (1, ?1)", [timed]).unwrap();
    connection
        .execute("INSERT INTO steps VALUES (2, ?1)", [untimed])
        .unwrap();
    drop(connection);
    let timed_at = 1_794_430_860_000i64;

    let first = scan(h, &["antigravity"], None);
    assert_eq!(
        first["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>(),
        320
    );
    assert_eq!(clock(&first, "antigravity"), current(timed_at));
    assert_eq!(first["kinds"]["antigravity"]["estimatedClock"], true);

    // The note persists while the database is unchanged: the flag is on the store print.
    let again = scan(h, &["antigravity"], Some(&first));
    assert_eq!(clock(&again, "antigravity"), current(timed_at));
    assert_eq!(again["kinds"]["antigravity"]["estimatedClock"], true);
    assert!(again["rows"].as_array().unwrap().is_empty());
}
