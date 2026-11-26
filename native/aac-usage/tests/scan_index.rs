//! Persistent optional discovery must advance past the first bounded page.
use aac_usage::discover;
use std::{fs, time::Duration};

#[test]
fn durable_walk_advances_and_revalidates_appends_and_removals() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for n in 0..12 {
        fs::create_dir_all(home.path().join(format!("lab-{n:02}/codex"))).unwrap();
        fs::write(home.path().join(format!("lab-{n:02}/codex/rollout-test.jsonl")), "{}\n").unwrap();
    }
    let first = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 4);
    assert!(
        first.optional_limited,
        "the first bounded page cannot claim a complete walk"
    );
    let mut completed = false;
    for _ in 0..100 {
        let result = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 4);
        if !result.optional_limited {
            assert_eq!(result.found["codex"].len(), 12);
            completed = true;
            break;
        }
    }
    assert!(completed, "a new process must continue the durable frontier");
    fs::create_dir_all(home.path().join("new/claude")).unwrap();
    fs::write(home.path().join("new/claude/a.jsonl"), "{}\n").unwrap();
    // Move outside the scanned tree, without deleting fixture files.
    fs::rename(home.path().join("lab-00"), cache.path().join("removed")).unwrap();
    let result = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 20_000);
    assert!(!result.optional_limited);
    assert_eq!(result.found["codex"].len(), 11);
    assert_eq!(result.found["claude"].len(), 1);
}

#[test]
fn build_trees_never_supply_experiment_logs() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for skip in ["target", "node_modules", ".git"] {
        let path = home.path().join(skip).join("lab/codex");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("rollout-test.jsonl"), "{}\n").unwrap();
    }
    let found = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 20_000);
    assert!(found.found.is_empty());
    assert!(!found.optional_limited);
}

#[test]
fn cached_walk_matches_cold_discovery_and_survives_a_corrupt_index() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    for kind in ["codex", "claude", "omp", "muse"] {
        let root = home.path().join("lab").join(kind);
        fs::create_dir_all(&root).unwrap();
        let name = match kind {
            "codex" => "rollout-test.jsonl",
            "omp" => "time_id.jsonl",
            "muse" => "session.jsonl",
            _ => "test.jsonl",
        };
        fs::write(root.join(name), "{}\n").unwrap();
    }
    let cold = discover::experiment_roots(home.path(), Duration::from_secs(5));
    for _ in 0..2 {
        let warm = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 20_000);
        assert_eq!(warm.found, cold.found);
        assert_eq!(warm.failed, cold.failed);
        assert_eq!(warm.limits, cold.limits);
        assert_eq!(warm.optional_limited, cold.optional_limited);
    }
    fs::write(cache.path().join("index.json"), b"incomplete JSON").unwrap();
    let recovered = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 20_000);
    assert_eq!(recovered.found, cold.found);
}

#[test]
fn changed_directory_rewinds_its_partial_listing() {
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    fs::create_dir_all(home.path().join("zzz")).unwrap();
    let first = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 1);
    // Even if that tiny initial walk completed, additions must invalidate its listing.
    let _ = first;
    fs::create_dir_all(home.path().join("aaa/codex")).unwrap();
    fs::write(home.path().join("aaa/codex/rollout-test.jsonl"), "{}\n").unwrap();
    let changed = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 20_000);
    assert_eq!(changed.found["codex"].len(), 1);
}

#[cfg(unix)]
#[test]
fn index_is_private_and_symlinked_index_falls_back_without_overwriting() {
    use std::os::unix::{fs::PermissionsExt, fs::symlink};
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 20_000);
    assert_eq!(
        fs::metadata(cache.path().join("index.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let linked = tempfile::tempdir().unwrap();
    let target = linked.path().join("untouched");
    fs::write(&target, "keep this").unwrap();
    symlink(&target, linked.path().join("index.json")).unwrap();
    let found = discover::experiment_roots_indexed(home.path(), linked.path(), Duration::from_secs(5), 20_000);
    assert!(!found.optional_limited);
    assert_eq!(fs::read_to_string(target).unwrap(), "keep this");
}

#[test]
fn persistent_scans_preserve_reader_resume_append_and_truncate_parity() {
    use aac_usage::{discover::Env, protocol};
    use serde_json::{Value, json};
    use std::io::Write;
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let log = home.path().join(".codex/sessions/rollout-fixture.jsonl");
    fs::create_dir_all(log.parent().unwrap()).unwrap();
    let event = |input| {
        json!({"timestamp":"2026-11-01T12:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":input,"output_tokens":0}}}}).to_string() + "\n"
    };
    let head = json!({"type":"session_meta","payload":{"id":"index-parity-fixture"}}).to_string() + "\n";
    let context = json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}).to_string() + "\n";
    fs::write(&log, head.clone() + &context + &event(10) + &event(30) + &event(50)).unwrap();
    let env = Env::from([("CCS_HOME".into(), cache.path().to_string_lossy().into_owned())]);
    let mut prior = json!({"_store":{}});
    let mut total = 0;
    let mut completed = false;
    for _ in 0..10 {
        let mut request = protocol::parse_request(
            json!({"kinds":["codex"],"minDateMs":0,"fingerprints":prior,"deadlineMs":60000})
                .to_string()
                .as_bytes(),
        )
        .unwrap();
        request.line_budget = Some(3);
        let answer = protocol::scan_persistent(&request, home.path(), &env);
        let prints = &answer["kinds"]["codex"]["fingerprints"];
        if prints.as_object().unwrap().values().any(|p| p["readFrom"].is_null()) {
            total = 0;
        }
        total += answer["rows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["i"].as_u64().unwrap())
            .sum::<u64>();
        prior = json!({"codex":prints,"_store":{}});
        if prints.as_object().unwrap().values().all(|p| p["readThrough"].is_null()) {
            completed = true;
            break;
        }
    }
    assert!(completed);
    assert_eq!(total, 50);
    let request = protocol::parse_request(
        json!({"kinds":["codex"],"minDateMs":0,"fingerprints":prior,"deadlineMs":60000})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&log)
        .unwrap()
        .write_all(event(70).as_bytes())
        .unwrap();
    let appended = protocol::scan_persistent(&request, home.path(), &env);
    let fresh = &appended["kinds"]["codex"]["fingerprints"];
    let delta: u64 = appended["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["i"].as_u64().unwrap())
        .sum();
    assert!(
        fresh
            .as_object()
            .unwrap()
            .values()
            .all(|p| p["readFrom"].as_u64().is_some())
    );
    assert_eq!(total + delta, 70);
    fs::write(&log, head + &context + &event(7)).unwrap();
    let request = protocol::parse_request(
        json!({"kinds":["codex"],"minDateMs":0,"fingerprints":{"codex":fresh},"deadlineMs":60000})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let replaced = protocol::scan_persistent(&request, home.path(), &env);
    assert!(
        replaced["kinds"]["codex"]["fingerprints"]
            .as_object()
            .unwrap()
            .values()
            .all(|p| p["readFrom"].is_null())
    );
    let mut cold = protocol::parse_request(
        json!({"kinds":["codex"],"minDateMs":0,"deadlineMs":60000})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    cold.fingerprints.clear();
    let cold: Value = protocol::scan(&cold, home.path(), &env);
    assert_eq!(replaced["rows"], cold["rows"]);
    assert_eq!(replaced["srows"], cold["srows"]);
    assert_eq!(replaced["rows"][0]["i"], 7);
    assert!(cache.path().join("analytics-scan-index-v1/index.json").exists());
}

#[cfg(unix)]
#[test]
fn a_saved_candidate_never_follows_a_replaced_parent_symlink() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let cache = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    for lab in ["a", "b"] {
        fs::create_dir_all(home.path().join(lab).join("codex")).unwrap();
        fs::write(home.path().join(lab).join("codex/rollout-test.jsonl"), "{}\n").unwrap();
    }
    let mut first = None;
    for _ in 0..10 {
        let answer = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 1);
        if answer.optional_limited && answer.found.get("codex").is_some_and(|p| !p.is_empty()) {
            first = Some(answer.found["codex"][0].parent().unwrap().to_owned());
            break;
        }
    }
    let lab = first.expect("a candidate is saved before the frontier completes");
    fs::rename(&lab, outside.path().join("moved")).unwrap();
    symlink(outside.path().join("moved"), &lab).unwrap();
    let answer = discover::experiment_roots_indexed(home.path(), cache.path(), Duration::from_secs(5), 20_000);
    assert!(
        !answer
            .found
            .get("codex")
            .is_some_and(|roots| roots.iter().any(|r| r.starts_with(&lab)))
    );
}
