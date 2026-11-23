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
    // Read earlier from a folder only the optional walk found: the cut walk may not reach it now.
    prior["ab".repeat(32)] = json!({"size":10,"mtimeMs":20,"optionalRoot":true});
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

// Big review Wonder L7: the depth cap marks partial only when it leaves an entry unread. An empty
// folder past the cap (or one holding only names every walk skips) is a complete read, as in the
// Python reference; a log past the cap is still partial.
#[test]
fn depth_cap_is_partial_only_when_it_leaves_an_entry_unread() {
    let deep = |home: &Path| {
        let mut folder = home.join(".codex/sessions");
        for _ in 0..65 {
            folder = folder.join("d");
        }
        folder
    };
    let partial = |home: &Path| scan(home, json!({}))["kinds"]["codex"]["partial"] == true;

    let empty = tempfile::tempdir().unwrap();
    write(&empty.path().join(".codex/sessions/rollout-fixture.jsonl"), &rollout());
    fs::create_dir_all(deep(empty.path())).unwrap();
    assert!(
        !partial(empty.path()),
        "an empty folder past the cap left nothing unread"
    );

    let skipped = tempfile::tempdir().unwrap();
    write(
        &skipped.path().join(".codex/sessions/rollout-fixture.jsonl"),
        &rollout(),
    );
    fs::create_dir_all(deep(skipped.path()).join("node_modules")).unwrap();
    assert!(!partial(skipped.path()), "a skipped name past the cap is never read");

    let log = tempfile::tempdir().unwrap();
    write(&log.path().join(".codex/sessions/rollout-fixture.jsonl"), &rollout());
    write(&deep(log.path()).join("rollout-deep.jsonl"), &rollout());
    assert!(partial(log.path()), "a log past the cap is unread");
}

fn rollout_for(id: &str) -> String {
    rollout().replace("search-bound-fixture", id)
}

/// Monday scan-partial (2026-11-23): Claude Code and Codex delete their own old logs. A file that
/// was read from a known log folder and is gone now cannot be hiding in a place the cut optional
/// walk did not reach: its retained print must not keep the kind partial on every scan. A file
/// first found under a folder only the optional walk discovered stays unconfirmed when it is
/// missing, because a later cut walk may simply not have reached that folder.
#[test]
fn a_known_root_file_that_is_gone_is_not_unconfirmed_but_an_optional_root_file_is() {
    let home = tempfile::tempdir().unwrap();
    write(
        &home.path().join(".codex/sessions/rollout-known.jsonl"),
        &rollout_for("search-bound-known"),
    );
    // Popped first by the walk (it takes the last-sorted child first), so it is found before the
    // unrelated folder spends the entry bound.
    let optional = home.path().join("zlab/run/codex/rollout-optional.jsonl");
    write(&optional, &rollout_for("search-bound-optional"));
    let unrelated = home.path().join("unrelated");
    fs::create_dir(&unrelated).unwrap();
    for n in 0..20_010 {
        fs::write(unrelated.join(format!("file-{n:05}")), "").unwrap();
    }
    let first = scan(home.path(), json!({}));
    assert_eq!(first["kinds"]["codex"]["optionalSearchLimited"], true, "{first}");
    assert_ne!(first["kinds"]["codex"]["partial"], true, "{first}");
    let prints = first["kinds"]["codex"]["fingerprints"].as_object().unwrap().clone();
    assert_eq!(prints.len(), 2, "{first}");
    let flagged: Vec<_> = prints.values().filter(|p| p["optionalRoot"] == true).collect();
    assert_eq!(
        flagged.len(),
        1,
        "only the walk-found folder's file is optional: {first}"
    );

    // A known-root file read earlier and deleted since (a tool's own cleanup).
    let mut prior = prints.clone();
    prior.insert("cd".repeat(32), json!({"size":10,"mtimeMs":20,"newestUsageMs":null}));
    let answer = scan(home.path(), json!({"codex":prior}));
    assert!(answer["kinds"]["codex"]["unconfirmedFiles"].is_null(), "{answer}");
    assert_ne!(
        answer["kinds"]["codex"]["partial"], true,
        "a gone file from a fully listed known folder is not unknown: {answer}"
    );

    // The walk-found file goes missing: it may sit in a folder a cut walk did not reach.
    fs::remove_file(&optional).unwrap();
    let answer = scan(home.path(), json!({"codex":prints}));
    assert_eq!(answer["kinds"]["codex"]["unconfirmedFiles"], 1, "{answer}");
    assert_eq!(answer["kinds"]["codex"]["partial"], true, "{answer}");
}
