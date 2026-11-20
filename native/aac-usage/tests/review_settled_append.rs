//! A resumed rollout keeps accruing usage after its previous hours settle.
use aac_usage::{discover::Env, protocol};
use serde_json::{Value, json};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

const SLOT_MS: i64 = 600_000;
const NOW: i64 = 1_795_212_000_000;
const OLD: i64 = NOW - 5 * 86_400_000;

fn scan(home: &Path, previous: Option<&Value>, slot: i64) -> Value {
    let mut prints = json!({"_store": {
        "settledBefore": {"codex": NOW - 3 * 86_400_000}, "settledSlot": slot,
    }});
    if let Some(previous) = previous {
        prints["codex"] = previous["kinds"]["codex"]["fingerprints"].clone();
    }
    let input = json!({"kinds":["codex"],"minDateMs":0,"fingerprints":prints,"cwd":false});
    let request = protocol::parse_request(input.to_string().as_bytes()).unwrap();
    protocol::scan(&request, home, &Env::default())
}

fn turn(timestamp: &str, input: u64) -> Value {
    json!({"timestamp":timestamp,"type":"event_msg","payload":{
        "type":"token_count","info":{"total_token_usage":{"input_tokens":input,"output_tokens":1}}
    }})
}

fn input(answer: &Value) -> u64 {
    answer["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["i"].as_u64())
        .sum()
}

#[test]
fn settled_rollout_resumes_within_one_cadence() {
    let home = tempfile::tempdir().unwrap();
    let path = home
        .path()
        .join(".codex/sessions/fixture/rollout-2026-11-15T12-00-00-3e43c7f5-744b-4790-9d0b-06042ecf0a20.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let original = [
        json!({"type":"session_meta","payload":{"id":"resumed","model_provider":"openai"}}),
        json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}),
        turn("2026-11-15T12:00:00Z", 100),
    ];
    let mut file = OpenOptions::new().create_new(true).write(true).open(&path).unwrap();
    for row in original {
        writeln!(file, "{row}").unwrap();
    }
    file.set_modified(UNIX_EPOCH + Duration::from_millis(OLD as u64))
        .unwrap();
    let old_size = file.metadata().unwrap().len();
    drop(file);
    let slot = NOW / SLOT_MS;
    let first = scan(home.path(), None, slot);
    assert_eq!(input(&first), 100);
    let settled = scan(home.path(), Some(&first), slot);
    assert_eq!(input(&settled), 0);
    let mut file = OpenOptions::new().append(true).open(&path).unwrap();
    writeln!(file, "{}", turn("2026-11-20T22:00:00Z", 1100)).unwrap();
    drop(file);
    let skipped = scan(home.path(), Some(&settled), slot);
    assert_eq!(input(&skipped), 0);
    let resumed = scan(home.path(), Some(&skipped), slot + 1);
    assert_eq!(input(&resumed), 1_000);
    let print = resumed["kinds"]["codex"]["fingerprints"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap();
    assert_eq!(print["readFrom"], old_size);
    assert_eq!(input(&scan(home.path(), Some(&resumed), slot + 1)), 0);
}
