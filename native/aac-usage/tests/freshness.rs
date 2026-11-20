//! Synthetic homes only. Activity is an independent signal, never an invented usage event.
use aac_usage::{discover::Env, protocol};
use rusqlite::Connection;
use serde_json::{Value, json};
use std::{fs, path::Path};

#[test]
fn freshness_cycle_boundary_and_missing_numbers_are_honest() {
    use aac_usage::discover::{FRESHNESS_CYCLE_MS, freshness};
    assert_eq!(
        freshness(Some(1000), Some(1000 + FRESHNESS_CYCLE_MS), 0, true)["state"],
        "current"
    );
    assert_eq!(
        freshness(Some(1000), Some(1001 + FRESHNESS_CYCLE_MS), 0, true)["state"],
        "stale"
    );
    assert_eq!(freshness(None, Some(1000), 0, true)["state"], "stale");
    assert_eq!(freshness(None, None, 0, true)["state"], "unknown");
}

#[test]
fn a_new_install_signal_does_not_establish_activity() {
    let home = tempfile::tempdir().unwrap();
    let bin = home.path().join(".local/bin").join(if cfg!(windows) {
        "cursor-agent.cmd"
    } else {
        "cursor-agent"
    });
    fs::create_dir_all(bin.parent().unwrap()).unwrap();
    fs::write(&bin, "fixture marker; never executed").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let answer = scan(home.path(), None);
    let found = &answer["discovery"]["kinds"]["cursor"];
    assert_eq!(found["installed"], true);
    assert_eq!(found["signals"], json!(["binary"]));
    assert_eq!(
        found["freshness"],
        json!({"state":"unknown","newestDataMs":null,"newestActivityMs":null,"activityComplete":true}),
    );
    assert!(answer["rows"].as_array().unwrap().is_empty());
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
fn a_fresh_store_file_without_a_new_run_stays_at_its_measured_usage() {
    let home = tempfile::tempdir().unwrap();
    store(home.path());
    let answer = scan(home.path(), None);
    assert_eq!(answer["rows"][0]["i"], 100);
    let freshness = &answer["discovery"]["kinds"]["cursor"]["freshness"];
    assert_eq!(freshness["state"], "current");
    assert_eq!(
        freshness,
        &json!({"state":"current","newestDataMs":1_794_430_860_000u64,
        "newestActivityMs":1_794_430_860_000u64,"activityComplete":true})
    );
    let again = scan(home.path(), Some(&answer));
    assert!(again["rows"].as_array().unwrap().is_empty());
    assert_eq!(again["discovery"]["kinds"]["cursor"]["freshness"], *freshness);
}

#[test]
fn cli_chats_without_usage_leave_an_older_sdk_source_current() {
    let home = tempfile::tempdir().unwrap();
    store(home.path());
    let cli = home.path().join("config/cursor/chats/project/chat/store.db");
    fs::create_dir_all(cli.parent().unwrap()).unwrap();
    Connection::open(cli)
        .unwrap()
        .execute_batch("CREATE TABLE blobs (id TEXT, data BLOB); CREATE TABLE meta (key TEXT, value TEXT);")
        .unwrap();
    let answer = scan(home.path(), None);
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["freshness"]["state"], "current");
    assert_eq!(
        answer["discovery"]["kinds"]["cursor"]["freshness"]["newestActivityMs"],
        answer["discovery"]["kinds"]["cursor"]["freshness"]["newestDataMs"]
    );
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

#[cfg(unix)]
fn modified(path: &Path, at: i64) {
    let time = std::time::UNIX_EPOCH + std::time::Duration::from_millis(at as u64);
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(time))
        .unwrap();
}

#[cfg(unix)]
fn quiet_sdk(home: &Path) {
    store(home);
    modified(
        &home.join(".cursor/projects/project/sdk-agent-store/store/index.db"),
        1794430860000,
    );
    modified(&home.join(".cursor"), 1794430860000);
}

#[cfg(unix)]
#[test]
fn chat_store_depth_cap_is_unverified_and_a_listed_store_is_counted() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    quiet_sdk(h);
    assert_eq!(
        scan(h, None)["discovery"]["kinds"]["cursor"]["freshness"]["state"],
        "current"
    );
    // A store deeper than the bounded listing can reach leaves current coverage unverified.
    let root = h.join("config/cursor/chats");
    let deep = root.join("0/1/2/3/4/5/6/7/8/store.db");
    fs::create_dir_all(deep.parent().unwrap()).unwrap();
    fs::write(&deep, "fixture blobs/meta store marker; no usage numbers").unwrap();
    modified(&deep, 1794434460000);
    let answer = scan(h, None);
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["freshness"]["state"], "unknown");
    assert_eq!(
        answer["discovery"]["kinds"]["cursor"]["freshness"]["activityComplete"],
        false
    );
    // A second store the cap cannot reach either keeps the same verdict, never a stale claim.
    let visible = root.join("visible/store.db");
    fs::create_dir_all(visible.parent().unwrap()).unwrap();
    fs::copy(&deep, &visible).unwrap();
    modified(&visible, 1794434460000);
    let known = scan(h, None);
    assert_eq!(known["discovery"]["kinds"]["cursor"]["freshness"]["state"], "unknown");
    assert_eq!(
        known["discovery"]["kinds"]["cursor"]["freshness"]["activityComplete"],
        false
    );
    fs::remove_file(&visible).unwrap();
    // Within the bound the same token-less store is counted, and the source stays current.
    let shallow = deep.parent().unwrap().parent().unwrap().join("store.db");
    fs::rename(&deep, &shallow).unwrap();
    fs::remove_dir(deep.parent().unwrap()).unwrap();
    let answer = scan(h, None);
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["freshness"]["state"], "current");
    assert_eq!(
        answer["discovery"]["kinds"]["cursor"]["freshness"]["activityComplete"],
        true
    );
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["unmeasuredFiles"], 1);
}

#[cfg(unix)]
#[test]
fn activity_listing_errors_are_unverified_instead_of_absent() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    quiet_sdk(h);
    fs::create_dir_all(h.join("config/cursor")).unwrap();
    fs::write(
        h.join("config/cursor/chats"),
        "a non-directory cannot be exhaustively listed",
    )
    .unwrap();
    let answer = scan(h, None);
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["freshness"]["state"], "unknown");
    assert_eq!(
        answer["discovery"]["kinds"]["cursor"]["freshness"]["activityComplete"],
        false
    );
}

fn varint(mut value: u64) -> Vec<u8> {
    let mut out = Vec::new();
    while value >= 128 {
        out.push((value as u8 & 127) | 128);
        value >>= 7;
    }
    out.push(value as u8);
    out
}

fn number(field: u64, value: u64) -> Vec<u8> {
    [varint(field * 8), varint(value)].concat()
}

fn bytes(field: u64, value: &[u8]) -> Vec<u8> {
    [varint(field * 8 + 2), varint(value.len() as u64), value.to_vec()].concat()
}

#[test]
fn omp_and_antigravity_files_are_dated_by_their_usage_entries() {
    for tool in ["omp", "antigravity"] {
        let home = tempfile::tempdir().unwrap();
        let h = home.path();
        if tool == "omp" {
            let file = h.join(".omp/agent/sessions/2026-11-11T21-00-00_fixture.jsonl");
            fs::create_dir_all(file.parent().unwrap()).unwrap();
            fs::write(
                file,
                json!({"type":"message","timestamp":"2026-11-11T21:01:00Z",
                "message":{"role":"assistant","model":"fixture-model","provider":"fixture-provider",
                "usage":{"input":100,"output":20,"cacheRead":0,"cacheWrite":0}}})
                .to_string()
                    + "\n",
            )
            .unwrap();
        } else {
            let db = h.join(".gemini/antigravity/conversations/fixture.db");
            fs::create_dir_all(db.parent().unwrap()).unwrap();
            let connection = Connection::open(db).unwrap();
            connection
                .execute_batch("CREATE TABLE steps (idx INTEGER, metadata BLOB)")
                .unwrap();
            let metadata = [
                bytes(24, &bytes(12, b"claude-opus-4-6")),
                bytes(8, &number(1, 1794430860)),
                bytes(9, &[number(2, 100), number(3, 20)].concat()),
            ]
            .concat();
            connection
                .execute("INSERT INTO steps VALUES (1, ?1)", [metadata])
                .unwrap();
        }
        let ask = |prints| {
            protocol::parse_request(
                json!({"kinds":[tool],
            "minDateMs":1790000000000u64,"deadlineMs":60000,"fingerprints":prints})
                .to_string()
                .as_bytes(),
            )
            .unwrap()
        };
        let env = Env::from([("PATH".into(), String::new())]);
        let first = protocol::scan(&ask(json!({})), h, &env);
        assert_eq!(first["rows"][0]["i"], 100, "{tool}");
        let freshness = &first["discovery"]["kinds"][tool]["freshness"];
        assert_eq!(freshness["state"], "current", "{tool}");
        assert_eq!(freshness["newestDataMs"], 1794430860000u64, "{tool}");
        assert_eq!(freshness["newestActivityMs"], 1794430860000u64, "{tool}");
        let again = protocol::scan(&ask(json!({tool:first["kinds"][tool]["fingerprints"]})), h, &env);
        assert!(again["rows"].as_array().unwrap().is_empty(), "{tool}");
        assert_eq!(again["discovery"]["kinds"][tool]["freshness"], *freshness, "{tool}");
    }
}

#[cfg(unix)]
#[test]
fn t3_thread_records_never_date_activity() {
    let home = tempfile::tempdir().unwrap();
    quiet_sdk(home.path());
    let db = home.path().join(".t3/userdata/statev2.sqlite");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    Connection::open(db).unwrap().execute_batch(
        "CREATE TABLE orchestration_v2_projection_provider_threads (driver TEXT, provider_instance_id TEXT, updated_at TEXT);
        INSERT INTO orchestration_v2_projection_provider_threads VALUES ('cursor', 'fixture-instance', '2026-11-15T23:00:00Z');"
    ).unwrap();
    let answer = scan(home.path(), None);
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["freshness"]["state"], "current");
    assert_eq!(
        answer["discovery"]["kinds"]["cursor"]["freshness"]["newestActivityMs"],
        1794430860000u64
    );
    assert_eq!(
        answer["discovery"]["kinds"]["cursor"]["freshness"]["newestDataMs"],
        1794430860000u64
    );
}

#[test]
fn native_default_cli_root_is_unmeasured_and_discovered_without_xdg_overrides() {
    let home = tempfile::tempdir().unwrap();
    let base = if cfg!(windows) {
        "AppData/Roaming/cursor/chats"
    } else if cfg!(target_os = "macos") {
        "Library/Application Support/cursor/chats"
    } else {
        ".config/cursor/chats"
    };
    let chat = home.path().join(base).join("project/chat/store.db");
    fs::create_dir_all(chat.parent().unwrap()).unwrap();
    Connection::open(chat)
        .unwrap()
        .execute_batch("CREATE TABLE blobs (id TEXT, data BLOB); CREATE TABLE meta (key TEXT, value TEXT);")
        .unwrap();
    let request = protocol::parse_request(
        json!({"kinds":["cursor"],"minDateMs":0,"deadlineMs":60000})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let answer = protocol::scan(&request, home.path(), &Env::from([("PATH".into(), String::new())]));
    assert_eq!(answer["discovery"]["kinds"]["cursor"]["unmeasuredFiles"], 1);
    assert!(answer["rows"].as_array().unwrap().is_empty());
    // A chat store holds no numeric usage: no data and no activity date, never a stale claim.
    assert_eq!(
        answer["discovery"]["kinds"]["cursor"]["freshness"],
        json!({"state":"unknown","newestDataMs":null,"newestActivityMs":null,"activityComplete":true})
    );
}
