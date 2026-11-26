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

#[test]
fn settled_packed_parses_into_typed_prior_without_value_map_and_keeps_scan_parity() {
    let home = tempfile::tempdir().unwrap();
    let path = fixture(home.path());
    let body = rollout(2, 120);
    fs::write(&path, &body).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis((AT - 10_000) as u64))
        .unwrap();

    let first = scan(home.path(), None, AT + 10_000);
    let first_prints = first["kinds"]["codex"]["fingerprints"].as_object().unwrap();
    let packed = protocol::pack_settled(first_prints).expect("pack settled");
    let wire = json!({
        "kinds": ["codex"],
        "minDateMs": 0,
        "cwd": false,
        "fingerprints": {
            "_store": {
                "settledBefore": {"codex": AT + 10_000},
                "settledSlot": 1,
                "settledPacked": {"codex": packed}
            },
            "codex": {}
        }
    });
    let request = protocol::parse_request(wire.to_string().as_bytes()).expect("parse packed request");
    assert!(
        request.fingerprints.is_empty(),
        "settledPacked must parse into typed PriorPrint structs instead of Value maps"
    );
    let key = first_prints.keys().next().unwrap();
    assert_eq!(request.prior_print_json("codex", key).as_ref(), first_prints.get(key));

    let env = Env::from([
        ("HOME".into(), home.path().to_string_lossy().into_owned()),
        ("PATH".into(), String::new()),
    ]);
    let via_value = protocol::scan(&request, home.path(), &env);
    let via_bytes: Value = serde_json::from_slice(&protocol::scan_owned_bytes(request, home.path(), &env)).unwrap();
    assert_eq!(via_value, via_bytes);
    assert_eq!(via_bytes["kinds"]["codex"]["settledPacked"], true);
    assert_eq!(
        via_bytes["kinds"]["codex"]["fingerprints"][key]["settledConfirmed"],
        true
    );
}

#[test]
fn collector_drop_kind_releases_finished_kind_and_settled_packed_append_resumes() {
    let home = tempfile::tempdir().unwrap();
    let path = fixture(home.path());
    let body = rollout(2, 120);
    fs::write(&path, &body).unwrap();
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis((AT - 10_000) as u64))
        .unwrap();

    let first = scan(home.path(), None, AT + 10_000);
    let first_prints = first["kinds"]["codex"]["fingerprints"].as_object().unwrap();
    let key = first_prints.keys().next().unwrap().clone();
    let packed = protocol::pack_settled(first_prints).expect("pack settled");
    let wire = json!({
        "kinds": ["codex", "claude"],
        "minDateMs": 0,
        "cwd": false,
        "fingerprints": {
            "_store": {
                "settledBefore": {"codex": AT + 10_000},
                "settledSlot": 1,
                "settledPacked": {"codex": packed}
            },
            "codex": {},
            "claude": {"keep": {"size": 1, "mtimeMs": 1, "head": "h", "tail": "t"}}
        }
    });
    let request = protocol::parse_request(wire.to_string().as_bytes()).expect("parse request");

    // Verify Collector::drop_kind releases codex state without touching claude.
    let mut collector = aac_usage::collector::Collector::with_prior(0, 20.0, request.prior.clone());
    assert!(collector.prior_kind("codex").is_some_and(|m| !m.is_empty()));
    assert!(collector.prior_kind("claude").is_some_and(|m| !m.is_empty()));
    collector.drop_kind("codex");
    assert!(collector.prior_kind("codex").is_none());
    assert!(!collector.settled.contains_key("codex"));
    assert!(!collector.fresh.contains_key("codex"));
    assert!(
        collector
            .prior_kind("claude")
            .is_some_and(|m| m.get_str("keep").is_some())
    );

    // Grow the settledPacked file and advance settledSlot so revalidation sees the growth and
    // resumes via the block-compressed AppendBlobStore appendState.
    let grown = rollout(3, 120);
    fs::write(&path, &grown).unwrap();
    let mut grown_wire = wire.clone();
    grown_wire["fingerprints"]["_store"]["settledSlot"] = json!(2);
    let grown_req = protocol::parse_request(grown_wire.to_string().as_bytes()).expect("parse grown request");
    let env = Env::from([
        ("HOME".into(), home.path().to_string_lossy().into_owned()),
        ("PATH".into(), String::new()),
    ]);
    let after = protocol::scan(&grown_req, home.path(), &env);
    assert_eq!(
        after["rows"][0]["i"], 120,
        "expected append-only read of grown settledPacked file: {after}"
    );
    assert_eq!(
        after["kinds"]["codex"]["fingerprints"][&key]["readFrom"].as_u64(),
        Some(body.len() as u64)
    );
}
