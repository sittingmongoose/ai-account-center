//! Synthetic homes only. Activity is an independent signal, never an invented usage event.
use aac_usage::{discover::Env, protocol};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path};

#[test]
fn freshness_cycle_boundary_and_missing_numbers_are_honest() {
    use aac_usage::discover::{FRESHNESS_CYCLE_MS, freshness};
    assert_eq!(
        freshness(Some(1000), Some(1000 + FRESHNESS_CYCLE_MS), 0)["state"],
        "current"
    );
    assert_eq!(
        freshness(Some(1000), Some(1001 + FRESHNESS_CYCLE_MS), 0)["state"],
        "stale"
    );
    assert_eq!(freshness(None, Some(1000), 0)["state"], "stale");
    assert_eq!(freshness(None, None, 0)["state"], "unknown");
}

#[test]
fn installed_desktop_clients_are_reported_as_unmeasured_instead_of_omitted() {
    let home = tempfile::tempdir().unwrap();
    let env = Env::from([(
        "XDG_CONFIG_HOME".into(),
        home.path().join("config").to_string_lossy().into_owned(),
    )]);
    // app_dir follows the native OS conventions. No fixture touches the actual user's home.
    let base = if cfg!(windows) {
        home.path().join("AppData/Roaming")
    } else if cfg!(target_os = "macos") {
        home.path().join("Library/Application Support")
    } else {
        home.path().join("config")
    };
    for name in ["Claude", "Codex", "Cursor", "Windsurf"] {
        fs::create_dir_all(base.join(name)).unwrap();
    }
    let tools = aac_usage::discover::presence(home.path(), &env, &[], std::time::Duration::from_secs(3));
    for kind in ["claude-desktop", "codex-app", "cursor-ide", "windsurf"] {
        assert_eq!(tools[kind]["installed"], true);
        assert_eq!(tools[kind]["cause"], "no_reader_yet");
    }
}

fn scan(home: &Path, prior: Option<&Value>) -> Value {
    let request = protocol::parse_request(
        json!({
            "kinds": ["cursor"], "minDateMs": 1_790_000_000_000u64,
            "deadlineMs": 60000, "presence": true,
            "fingerprints": prior.map(|p| json!({"cursor":p["kinds"]["cursor"]["fingerprints"]})).unwrap_or(json!({}))
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    protocol::scan(
        &request,
        home,
        &Env::from([
            ("HOME".into(), home.to_string_lossy().into_owned()),
            ("PATH".into(), String::new()),
            (
                "XDG_CONFIG_HOME".into(),
                home.join("config").to_string_lossy().into_owned(),
            ),
        ]),
    )
}

fn store(home: &Path) {
    let path = home.join(".cursor/projects/project/sdk-agent-store/store/index.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let c = Connection::open(path).unwrap();
    c.execute_batch("CREATE TABLE runs (run_id TEXT, agent_id TEXT, model TEXT, started_at TEXT, finished_at TEXT, usage_json TEXT);
      INSERT INTO runs VALUES ('run-1','agent-1','fixture-model','2026-11-11T21:00:00Z','2026-11-11T21:01:00Z',
      '{\"inputTokens\":100,\"outputTokens\":20,\"cacheReadTokens\":0,\"cacheWriteTokens\":0,\"totalTokens\":120}');").unwrap();
}

#[test]
fn current_cli_chat_location_is_discovered_without_inventing_usage() {
    let home = tempfile::tempdir().unwrap();
    let chat = home.path().join("config/cursor/chats/project/chat/store.db");
    fs::create_dir_all(chat.parent().unwrap()).unwrap();
    Connection::open(chat)
        .unwrap()
        .execute_batch("CREATE TABLE blobs (id TEXT, data BLOB); CREATE TABLE meta (key TEXT, value TEXT);")
        .unwrap();
    let answer = scan(home.path(), None);
    assert!(answer["rows"].as_array().unwrap().is_empty());
    assert_eq!(answer["discovery"]["tools"]["cursor"]["counts"]["filesFound"], 1);
    assert!(answer["discovery"]["tools"]["cursor"]["counts"]["newestMtime"].is_number());
    assert_eq!(
        answer["discovery"]["tools"]["cursor"]["cause"],
        "no_local_usage_records"
    );
}

#[test]
fn sdk_activity_newer_than_measured_usage_is_stale_even_when_read_succeeded() {
    let home = tempfile::tempdir().unwrap();
    store(home.path());
    let answer = scan(home.path(), None);
    assert_eq!(answer["rows"][0]["i"], 100);
    let freshness = &answer["discovery"]["kinds"]["cursor"]["freshness"];
    assert_eq!(freshness["state"], "stale");
    assert_eq!(freshness["newestDataMs"], 1_794_430_860_000u64);
    assert!(freshness["newestActivityMs"].as_i64().unwrap() > freshness["newestDataMs"].as_i64().unwrap());
    let again = scan(home.path(), Some(&answer));
    assert!(again["rows"].as_array().unwrap().is_empty());
    assert_eq!(again["discovery"]["kinds"]["cursor"]["freshness"], *freshness);
}

#[test]
fn cli_activity_keeps_an_older_sdk_source_from_claiming_current_coverage() {
    let home = tempfile::tempdir().unwrap();
    store(home.path());
    let cli = home.path().join("config/cursor/chats/project/chat/store.db");
    fs::create_dir_all(cli.parent().unwrap()).unwrap();
    Connection::open(cli)
        .unwrap()
        .execute_batch("CREATE TABLE blobs (id TEXT, data BLOB); CREATE TABLE meta (key TEXT, value TEXT);")
        .unwrap();
    let answer = scan(home.path(), None);
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["freshness"]["state"], "stale");
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["unmeasuredFiles"], 1);
    assert_eq!(answer["rows"].as_array().unwrap().len(), 1);
}

#[test]
fn switching_from_immutable_to_read_only_reads_committed_wal_without_changing_source_bytes() {
    let home = tempfile::tempdir().unwrap();
    let db = home.path().join(".zcode/cli/db/db.sqlite");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let writer = Connection::open(&db).unwrap();
    writer.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;
        CREATE TABLE model_usage (session_id TEXT, model_id TEXT, provider_id TEXT, started_at INTEGER,
          input_tokens INTEGER, output_tokens INTEGER, cache_read_input_tokens INTEGER, cache_creation_input_tokens INTEGER);
        INSERT INTO model_usage VALUES ('s', 'fixture-model', 'builtin:zai-coding-plan', 1794430860000, 100, 20, 0, 0);
        PRAGMA wal_checkpoint(TRUNCATE);
        INSERT INTO model_usage VALUES ('s', 'fixture-model', 'builtin:zai-coding-plan', 1794772860000, 220, 30, 0, 0);").unwrap();
    let wal = db.with_file_name("db.sqlite-wal");
    let before = (fs::read(&db).unwrap(), fs::read(&wal).unwrap());
    let env = Env::from([
        ("HOME".into(), home.path().to_string_lossy().into_owned()),
        ("PATH".into(), String::new()),
    ]);
    let ask = |immutable, prior: Value| {
        protocol::parse_request(
            json!({"kinds":["zcode"],"minDateMs":1790000000000u64,
        "immutableSqlite":immutable,"fingerprints":prior})
            .to_string()
            .as_bytes(),
        )
        .unwrap()
    };
    let old = protocol::scan(&ask(true, json!({})), home.path(), &env);
    assert_eq!(old["rows"][0]["i"], 100);
    assert_eq!(old["kinds"]["zcode"]["walUnread"], true);
    let fresh = protocol::scan(
        &ask(false, json!({"zcode":old["kinds"]["zcode"]["fingerprints"]})),
        home.path(),
        &env,
    );
    let total: u64 = fresh["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["i"].as_u64().unwrap())
        .sum();
    assert_eq!(total, 320, "A mode change must reread an unchanged DB/WAL print");
    assert!(fresh["kinds"]["zcode"].get("walUnread").is_none());
    assert_eq!((fs::read(&db).unwrap(), fs::read(&wal).unwrap()), before);
}
