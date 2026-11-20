//! The store's text log checkpoints distinguish an append from a replacement.
use aac_usage::{discover::Env, protocol};
use serde_json::{Value, json};
use std::{
    fs,
    path::Path,
    time::{Duration, UNIX_EPOCH},
};

const AT: i64 = 1_793_677_111_000;
const HEADER: &str = r#"{"type":"session_meta","payload":{"id":"append-fixture","model_provider":"openai"}}"#;
const MODEL: &str = r#"{"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}"#;

fn rollout(turns: u64, input: u64) -> String {
    let mut out = format!("{HEADER}\n{MODEL}\n");
    for turn in 1..=turns {
        let record = json!({"timestamp":"2026-11-03T03:38:31Z","type":"event_msg","payload":{
            "type":"token_count","info":{"total_token_usage":{"input_tokens":turn*input,"output_tokens":turn}}
        }});
        out.push_str(&format!("{record}\n"));
    }
    out
}

fn scan(home: &Path, last: Option<&Value>, settle: i64) -> Value {
    scan_at_slot(home, last, settle, 1)
}

fn scan_at_slot(home: &Path, last: Option<&Value>, settle: i64, slot: u64) -> Value {
    let mut prints = json!({"_store":{"settledBefore":{"codex":settle},"settledSlot":slot}});
    if let Some(last) = last {
        prints["codex"] = last["kinds"]["codex"]["fingerprints"].clone();
    }
    let input = json!({"kinds":["codex"],"minDateMs":0,"fingerprints":prints,"cwd":false});
    let request = protocol::parse_request(input.to_string().as_bytes()).unwrap();
    let env = Env::from([
        ("HOME".into(), home.to_string_lossy().into_owned()),
        ("PATH".into(), String::new()),
    ]);
    protocol::scan(&request, home, &env)
}

fn print(answer: &Value) -> &Value {
    answer["kinds"]["codex"]["fingerprints"]
        .as_object()
        .unwrap()
        .values()
        .next()
        .unwrap()
}

fn fixture(home: &Path) -> std::path::PathBuf {
    let path =
        home.join(".codex/sessions/2026/11/03/rollout-2026-11-03T03-38-31-01a19f7c-b191-7a42-9bd9-c0b3dd7f3f33.jsonl");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    path
}

#[test]
fn grown_log_reads_only_the_suffix_and_replacements_read_from_zero() {
    let home = tempfile::tempdir().unwrap();
    let path = fixture(home.path());
    let old = rollout(3, 100);
    fs::write(&path, &old).unwrap();
    let first = scan(home.path(), None, 0);
    assert_eq!(first["rows"][0]["i"], 300);
    fs::write(&path, rollout(4, 100)).unwrap();
    let grown = scan(home.path(), Some(&first), 0);
    assert_eq!(print(&grown)["readFrom"], old.len());
    assert_eq!(grown["rows"][0]["i"], 100);
    assert_eq!(grown["rows"][0]["n"], 1);

    let replacement = path.with_extension("replacement");
    fs::write(&replacement, rollout(5, 100)).unwrap();
    fs::rename(&replacement, &path).unwrap();
    let rotated = scan(home.path(), Some(&grown), 0);
    assert!(print(&rotated).get("readFrom").is_none());
    assert_eq!(rotated["rows"][0]["i"], 500);

    fs::write(&path, rollout(1, 250)).unwrap();
    let truncated = scan(home.path(), Some(&rotated), 0);
    assert!(print(&truncated).get("readFrom").is_none());
    assert_eq!(truncated["rows"][0]["i"], 250);

    fs::write(&path, rollout(2, 750)).unwrap();
    let rewritten = scan(home.path(), Some(&truncated), 0);
    assert!(print(&rewritten).get("readFrom").is_none());
    assert_eq!(rewritten["rows"][0]["i"], 1500);
}

#[test]
fn a_settled_log_is_checked_again_on_the_next_cadence() {
    let home = tempfile::tempdir().unwrap();
    let path = fixture(home.path());
    check_settled(home.path(), &path);
}

#[test]
fn a_settled_log_in_a_t3_home_is_checked_again_on_the_next_cadence() {
    let home = tempfile::tempdir().unwrap();
    let t3_home = home.path().join(".codex-t3/fixture");
    let path = fixture(&t3_home);
    let t3_sessions = t3_home.join("sessions");
    fs::rename(t3_home.join(".codex/sessions"), &t3_sessions).unwrap();
    let path = t3_sessions.join(path.strip_prefix(t3_home.join(".codex/sessions")).unwrap());
    check_settled(home.path(), &path);
}

fn check_settled(home: &Path, path: &Path) {
    fs::write(path, rollout(1, 100)).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(AT as u64))
        .unwrap();
    let first = scan(home, None, 0);
    assert_eq!(first["rows"][0]["i"], 100);
    let cutoff = AT + 3_600_000;
    let checked = scan(home, Some(&first), cutoff);
    assert_eq!(checked["rows"], json!([]));
    assert_eq!(print(&checked)["settledCheckedSlot"], 1);

    let live = path.with_extension("saved");
    fs::rename(path, live).unwrap();
    fs::write(path, rollout(1, 300)).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(AT as u64))
        .unwrap();
    let settled = scan(home, Some(&checked), cutoff);
    assert_eq!(settled["rows"], json!([]));
    assert_eq!(settled["kinds"]["codex"]["readFailures"], Value::Null);
    assert_eq!(print(&settled), print(&checked));
    let replaced = scan_at_slot(home, Some(&settled), cutoff, 2);
    assert_eq!(replaced["rows"][0]["i"], 300);
    assert!(print(&replaced).get("readFrom").is_none());
}
