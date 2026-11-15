//! Optional experiment discovery must not turn a complete known-root read into a partial read.
use aac_usage::{discover::Env, protocol};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn rollout() -> String {
    [
        json!({"type":"session_meta","payload":{"id":"search-bound-fixture"}}),
        json!({"timestamp":"2026-11-01T12:00:00Z","type":"turn_context","payload":{"model":"gpt-6.1-sol"}}),
        json!({"timestamp":"2026-11-01T12:00:00Z","type":"event_msg","payload":{"type":"token_count","info":{"total_token_usage":{"input_tokens":123,"output_tokens":4}}}}),
    ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n"
}

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn scan(home: &Path, fingerprints: Value) -> Value {
    let request = protocol::parse_request(
        json!({
            "kinds":["codex","omp","muse"],"minDateMs":0,"deadlineMs":60000,
            "fingerprints":fingerprints,
        })
        .to_string()
        .as_bytes(),
    )
    .unwrap();
    protocol::scan(&request, home, &Env::new())
}

#[test]
fn optional_search_bound_keeps_complete_known_roots_complete_and_unknown_prior_files_partial() {
    let home = tempfile::tempdir().unwrap();
    write(&home.path().join(".codex/sessions/rollout-fixture.jsonl"), &rollout());
    let unrelated = home.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    // Deterministic entry bound, independent of machine speed or a sleep.
    for n in 0..20_010 {
        fs::write(unrelated.join(format!("file-{n:05}")), "").unwrap();
    }
    let answer = scan(home.path(), json!({}));
    for kind in ["codex", "omp", "muse"] {
        assert_ne!(answer["kinds"][kind]["partial"], true, "{kind}");
        assert!(answer["kinds"][kind]["searchLimit"].is_null(), "{kind}");
        assert_eq!(answer["kinds"][kind]["optionalSearchLimited"], true, "{kind}");
    }
    assert_eq!(answer["rows"][0]["i"], 123);
    let mut prior = answer["kinds"]["codex"]["fingerprints"].clone();
    prior["ab".repeat(32)] = json!({"size":10,"mtimeMs":20});
    let answer = scan(home.path(), json!({"codex":prior}));
    assert_eq!(answer["kinds"]["codex"]["unconfirmedFiles"], 1);
    assert_eq!(
        answer["kinds"]["codex"]["partial"], true,
        "a real previously read candidate was not confirmed; do not advance its clock or drop its usage"
    );
}

#[test]
fn bounded_candidate_preflight_falls_back_to_the_real_reader() {
    let home = tempfile::tempdir().unwrap();
    let candidate = home.path().join("lab/codex");
    fs::create_dir_all(&candidate).unwrap();
    for n in 0..401 {
        fs::write(candidate.join(format!("a-{n:03}")), "").unwrap();
    }
    write(&candidate.join("rollout-fixture.jsonl"), &rollout());
    let answer = scan(home.path(), json!({}));
    assert_eq!(
        answer["rows"][0]["i"], 123,
        "400 unrelated entries cannot prove a real candidate has no logs"
    );
    assert_ne!(answer["kinds"]["codex"]["partial"], true);
}

#[test]
fn bounded_candidate_depth_falls_back_to_the_real_reader() {
    let home = tempfile::tempdir().unwrap();
    write(&home.path().join("lab/codex/a/b/c/d/rollout-fixture.jsonl"), &rollout());
    let answer = scan(home.path(), json!({}));
    assert_eq!(answer["rows"][0]["i"], 123);
    assert_ne!(answer["kinds"]["codex"]["partial"], true);
}

#[test]
fn optional_time_bound_does_not_assert_that_unknown_log_roots_are_unread() {
    let home = tempfile::tempdir().unwrap();
    let found = aac_usage::discover::experiment_roots(home.path(), std::time::Duration::ZERO);
    assert!(
        found.limits.is_empty(),
        "a time cutoff has not established any unread log root"
    );
}

// Search-bound REVIEW-cheap M1: a forwarded candidate whose log lies below the reader's depth
// cap must give the usage or say partial, never a complete empty read.
#[test]
fn deep_known_candidate_never_looks_complete() {
    let home = tempfile::tempdir().unwrap();
    let mut folder = home.path().join("z-lab/codex");
    for _ in 0..65 {
        folder = folder.join("d");
    }
    write(&folder.join("rollout-fixture.jsonl"), &rollout());
    let unrelated = home.path().join("a-unrelated");
    fs::create_dir(&unrelated).unwrap();
    for n in 0..20_010 {
        fs::write(unrelated.join(format!("file-{n:05}")), "").unwrap();
    }
    let answer = scan(home.path(), json!({}));
    assert!(
        answer["rows"].as_array().unwrap().iter().any(|r| r["i"] == 123) || answer["kinds"]["codex"]["partial"] == true,
        "{answer}"
    );
}
