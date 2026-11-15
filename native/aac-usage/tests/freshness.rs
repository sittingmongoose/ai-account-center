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
