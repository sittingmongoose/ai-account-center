//! Recheck 1 of the five-model review fixes (RECHECK-BIG-1.md, FIX-ROUND-BIG-2.md in the share
//! folder 2026-11-13-analytics-discovery): one test per remaining item (M1 to M4), each failing
//! at 106f05a5c. Synthetic fixtures only.

use aac_usage::discover::Env;
use aac_usage::protocol::{self, Request};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const AT: &str = "2026-11-11T21:33:15.765Z";
const MIN_DATE: u64 = 1_780_000_000_000;

fn env(home: &Path) -> Env {
    Env::from([
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("PATH".to_string(), String::new()),
    ])
}

fn ask(kinds: &[&str], prior: Option<&Value>) -> Request {
    let mut fingerprints = Map::new();
    if let Some(prior) = prior {
        for kind in kinds {
            fingerprints.insert(kind.to_string(), prior["kinds"][kind]["fingerprints"].clone());
        }
    }
    protocol::parse_request(
        json!({"kinds": kinds, "minDateMs": MIN_DATE, "deadlineMs": 60_000, "fingerprints": fingerprints})
            .to_string()
            .as_bytes(),
    )
    .unwrap()
}

fn scan(home: &Path, kinds: &[&str], prior: Option<&Value>) -> Value {
    protocol::scan(&ask(kinds, prior), home, &env(home))
}

/// Input, output, cache read, cache write and events of one kind's rows.
fn sums(answer: &Value, kind: &str) -> [u64; 5] {
    let mut out = [0; 5];
    for row in answer["rows"].as_array().unwrap().iter().filter(|r| r["k"] == kind) {
        for (i, key) in ["i", "o", "cr", "cw", "n"].iter().enumerate() {
            out[i] += row[*key].as_u64().unwrap();
        }
    }
    out
}

/// A Codex rollout with one cumulative token count per entry of `totals` (input; output 10 each).
fn rollout(session: &str, totals: &[u64]) -> String {
    let mut lines = vec![
        json!({"timestamp": AT, "type": "session_meta", "payload": {"id": session, "model_provider": "openai"}}),
        json!({"timestamp": AT, "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}),
    ];
    for (index, total) in totals.iter().enumerate() {
        let previous = if index == 0 { 0 } else { totals[index - 1] };
        lines.push(json!({"timestamp": AT, "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": total, "cached_input_tokens": 0, "output_tokens": 10 * (index as u64 + 1)},
            "last_token_usage": {"input_tokens": total - previous, "cached_input_tokens": 0, "output_tokens": 10}}}}));
    }
    lines.iter().map(|v| format!("{v}\n")).collect()
}

const ID: &str = "aaaaaaaa-1111-2222-3333-444444444444";

fn rollout_name(id: &str) -> String {
    format!("rollout-2026-11-11T21-00-00-{id}.jsonl")
}

fn write(path: &Path, body: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

#[cfg(unix)]
fn deny(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
}

#[cfg(unix)]
fn allow(path: &Path, mode: u32) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

#[cfg(unix)]
fn running_as_root() -> bool {
    (unsafe { libc::geteuid() }) == 0
}

// ----- M1: Codex identity is the session header, never the file name ------------------------------

/// RECHECK-BIG-1 M1: two logs of one session under different file names with cumulative prefixes
/// of one and two events. True: 2 events, 300 input. 106f05a5c: 3 events, 400 input.
#[test]
fn codex_copies_with_an_opaque_session_id_count_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".codex/sessions/rollout-2026-11-10T00-00-00-opaque-fixture-id.jsonl"),
        &rollout("opaque-fixture-id", &[100]),
    );
    write(
        &h.join(".codex/sessions/rollout-2026-11-11T00-00-00-opaque-fixture-id.jsonl"),
        &rollout("opaque-fixture-id", &[100, 300]),
    );
    let answer = scan(h, &["codex"], None);
    assert_eq!(sums(&answer, "codex"), [300, 20, 0, 0, 2], "{answer}");
}

/// M1: the file name's id never decides. Two names, one payload id: once. One name in two
/// folders, two payload ids: both conversations count.
#[test]
fn codex_identity_comes_from_the_header_not_the_name() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let other = "bbbbbbbb-1111-2222-3333-444444444444";
    let third = "cccccccc-1111-2222-3333-444444444444";
    write(
        &h.join(".codex/sessions").join(rollout_name(ID)),
        &rollout(other, &[100, 300]),
    );
    write(
        &h.join(".codex/archived_sessions").join(rollout_name(third)),
        &rollout(other, &[100]),
    );
    let answer = scan(h, &["codex"], None);
    assert_eq!(sums(&answer, "codex"), [300, 20, 0, 0, 2], "{answer}");

    let home2 = tempfile::tempdir().unwrap();
    let h2 = home2.path();
    write(
        &h2.join(".codex/sessions").join(rollout_name(ID)),
        &rollout(other, &[100]),
    );
    write(
        &h2.join(".codex/archived_sessions").join(rollout_name(ID)),
        &rollout(third, &[200]),
    );
    let answer2 = scan(h2, &["codex"], None);
    assert_eq!(sums(&answer2, "codex"), [300, 20, 0, 0, 2], "{answer2}");
}

/// M1: a log with no valid session header is a named partial result, never counted (and its
/// copies never counted separately).
#[test]
fn a_codex_log_without_a_session_header_is_named_and_never_counted() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let headerless = format!(
        "{}\n{}\n",
        json!({"timestamp": AT, "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}),
        json!({"timestamp": AT, "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": 100, "cached_input_tokens": 0, "output_tokens": 10},
            "last_token_usage": {"input_tokens": 100, "cached_input_tokens": 0, "output_tokens": 10}}}})
    );
    write(&h.join(".codex/sessions").join(rollout_name(ID)), &headerless);
    write(&h.join(".codex/archived_sessions").join(rollout_name(ID)), &headerless);
    // A header whose id is empty is no header either.
    let empty_id = rollout("", &[100]);
    write(
        &h.join(".codex/sessions")
            .join(rollout_name("dddddddd-1111-2222-3333-444444444444")),
        &empty_id,
    );
    let answer = scan(h, &["codex"], None);
    assert_eq!(sums(&answer, "codex"), [0, 0, 0, 0, 0], "{answer}");
    assert_eq!(answer["kinds"]["codex"]["partial"], true, "{answer}");
    assert_eq!(answer["kinds"]["codex"]["readFailures"], 3, "{answer}");
    let unfinished = answer["kinds"]["codex"]["unfinished"].as_array().unwrap();
    assert_eq!(answer["kinds"]["codex"]["unfinishedCount"], 3, "{answer}");
    assert!(
        unfinished.iter().all(|log| log["reason"] == "no_session_id"),
        "{unfinished:?}"
    );
}

// ----- M2: Muse copies under a renamed folder count once ------------------------------------------

fn muse_session(input: u64) -> String {
    format!(
        "{}\n",
        json!({"recorded_at": 1_790_000_000, "payload": {"event": {"kind": "model_completed", "model": "glm-5",
            "usage": {"input_tokens": input, "output_tokens": 10, "cache_read_tokens": 0, "cache_write_tokens": 0}}}})
    )
}

/// RECHECK-BIG-1 M2: a hard link of a Muse session under a different session folder name.
/// True: 100 input, 1 event. 106f05a5c: 200 input, 2 events.
#[cfg(unix)]
#[test]
fn a_muse_hard_link_under_a_renamed_folder_counts_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let live = h.join(".local/share/muse/sessions/session-a/session.jsonl");
    write(&live, &muse_session(100));
    let kept = h.join("lab/retained/run/muse/session-b/session.jsonl");
    fs::create_dir_all(kept.parent().unwrap()).unwrap();
    fs::hard_link(&live, &kept).unwrap();
    let answer = scan(h, &["muse"], None);
    assert_eq!(sums(&answer, "muse"), [100, 10, 0, 0, 1], "{answer}");
    assert_eq!(answer["kinds"]["muse"]["fingerprints"].as_object().unwrap().len(), 1);
    // Ownership is stable: the next scan confirms the same owner and adds nothing.
    let again = scan(h, &["muse"], Some(&answer));
    assert_eq!(sums(&again, "muse"), [0, 0, 0, 0, 0], "{again}");
    assert_eq!(
        again["kinds"]["muse"]["fingerprints"],
        answer["kinds"]["muse"]["fingerprints"]
    );
}

/// M2: the same for a byte-identical copy under a renamed folder.
#[test]
fn a_muse_byte_copy_under_a_renamed_folder_counts_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".local/share/muse/sessions/session-a/session.jsonl"),
        &muse_session(100),
    );
    write(
        &h.join("lab/retained/run/muse/session-b/session.jsonl"),
        &muse_session(100),
    );
    let answer = scan(h, &["muse"], None);
    assert_eq!(sums(&answer, "muse"), [100, 10, 0, 0, 1], "{answer}");
}

// ----- M3: enumeration errors are named, never an empty success -----------------------------------

fn cursor_db(home: &Path, project: &str, store: &str) -> PathBuf {
    let db = home
        .join(".cursor/projects")
        .join(project)
        .join("sdk-agent-store")
        .join(store)
        .join("index.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let connection = Connection::open(&db).unwrap();
    connection
        .execute_batch(
            "CREATE TABLE agents (agent_id TEXT PRIMARY KEY, workspace_ref TEXT, name TEXT, metadata_json TEXT);
             CREATE TABLE runs (run_id TEXT PRIMARY KEY, agent_id TEXT, model TEXT, usage_json TEXT,
               started_at TEXT, finished_at TEXT);
             INSERT INTO agents VALUES ('agent-1', '/home/u/p', 'n', '{}');",
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO runs VALUES ('run-1', 'agent-1', 'grok-4.7', ?1, ?2, ?2)",
            rusqlite::params![
                r#"{"inputTokens":300,"outputTokens":50,"cacheReadTokens":0,"cacheWriteTokens":0,"totalTokens":350}"#,
                AT
            ],
        )
        .unwrap();
    db
}

/// RECHECK-BIG-1 M3: `sdk-agent-store` at mode 000 after a good read. The next answer names the
/// folder and is partial (so the server keeps the cached usage). 106f05a5c: "not_installed",
/// no partial flag, no roots, no failures.
#[cfg(unix)]
#[test]
fn an_unreadable_nested_cursor_folder_is_named_and_partial() {
    if running_as_root() {
        return; // root reads anything: the case cannot be built
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    cursor_db(h, "project", "store");
    let first = scan(h, &["cursor"], None);
    assert_eq!(sums(&first, "cursor")[0], 300);
    let denied = h.join(".cursor/projects/project/sdk-agent-store");
    deny(&denied);
    let answer = scan(h, &["cursor"], Some(&first));
    allow(&denied, 0o755);
    assert_ne!(answer["kinds"]["cursor"]["state"], "not_installed", "{answer}");
    assert_eq!(answer["kinds"]["cursor"]["partial"], true, "{answer}");
    assert!(
        answer["kinds"]["cursor"]["unreadableFolders"].as_u64().unwrap_or(0) >= 1,
        "{answer}"
    );
    let roots = answer["discovery"]["kinds"]["cursor"]["roots"].as_array().unwrap();
    assert!(!roots.is_empty(), "{answer}");
}

/// M3: one project's store folder unreadable; another project's store still reads.
#[cfg(unix)]
#[test]
fn a_cursor_store_beside_an_unreadable_one_still_reads() {
    if running_as_root() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    cursor_db(h, "closed", "store");
    cursor_db(h, "open", "store");
    let denied = h.join(".cursor/projects/closed/sdk-agent-store");
    deny(&denied);
    let answer = scan(h, &["cursor"], None);
    allow(&denied, 0o755);
    assert_eq!(sums(&answer, "cursor")[0], 300, "{answer}");
    assert_eq!(answer["kinds"]["cursor"]["partial"], true, "{answer}");
}

/// M3 in another reader: the OMP marker search over `~/PM-Experiments` cannot list one folder.
/// 106f05a5c skipped it silently.
#[cfg(unix)]
#[test]
fn an_unreadable_folder_in_the_omp_marker_search_is_counted() {
    if running_as_root() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let session = h.join(".omp/agent/sessions/2026-11-11T21-00-00_deadbeef.jsonl");
    write(
        &session,
        &format!(
            "{}\n",
            json!({"type": "message", "timestamp": AT, "message": {"role": "assistant", "model": "glm-5",
                "provider": "zai", "usage": {"input": 10, "output": 2, "cacheRead": 0, "cacheWrite": 0}}})
        ),
    );
    let secret = h.join("PM-Experiments/locked");
    fs::create_dir_all(&secret).unwrap();
    deny(&secret);
    let answer = scan(h, &["omp"], None);
    allow(&secret, 0o755);
    assert_eq!(sums(&answer, "omp")[0], 10, "{answer}");
    assert_eq!(answer["kinds"]["omp"]["partial"], true, "{answer}");
    assert!(
        answer["kinds"]["omp"]["unreadableFolders"].as_u64().unwrap_or(0) >= 1,
        "{answer}"
    );
}

/// M3 in discovery: a T3 account base that cannot be listed is named, not silently empty.
/// 106f05a5c: no T3 roots, no partial flag.
#[cfg(unix)]
#[test]
fn an_unreadable_t3_base_is_counted_for_claude() {
    if running_as_root() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join(".claude/projects/p/s.jsonl"), "{}\n");
    let base = h.join(".claude-t3");
    write(&base.join("me/projects/p/s.jsonl"), "{}\n");
    deny(&base);
    let answer = scan(h, &["claude"], None);
    allow(&base, 0o755);
    allow(&base.join("me"), 0o755);
    assert_eq!(answer["kinds"]["claude"]["partial"], true, "{answer}");
    assert!(
        answer["kinds"]["claude"]["unreadableFolders"].as_u64().unwrap_or(0) >= 1,
        "{answer}"
    );
}

// ----- M4: the longest readable copy owns the session ----------------------------------------------

/// RECHECK-BIG-1 M4: the longer archive copy cannot be opened; the readable live copy counts.
/// True: 2 events, 300 input, 1 failed file. 106f05a5c: 0 events.
#[cfg(unix)]
#[test]
fn an_unreadable_longest_codex_copy_falls_back_to_a_readable_one() {
    if running_as_root() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".codex/sessions").join(rollout_name(ID)),
        &rollout(ID, &[100, 300]),
    );
    let archive = h.join(".codex/archived_sessions").join(rollout_name(ID));
    write(&archive, &format!("{}{}", rollout(ID, &[100, 300]), " ".repeat(100)));
    deny(&archive);
    let answer = scan(h, &["codex"], None);
    allow(&archive, 0o644);
    assert_eq!(sums(&answer, "codex"), [300, 20, 0, 0, 2], "{answer}");
    assert_eq!(answer["kinds"]["codex"]["readFailures"], 1, "{answer}");
    assert_eq!(answer["kinds"]["codex"]["partial"], true, "{answer}");
}

/// M4: a compressed copy that does not decode never hides a readable plain one.
#[test]
fn an_undecodable_codex_copy_falls_back_to_a_readable_one() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".codex/sessions").join(rollout_name(ID)),
        &rollout(ID, &[100, 300]),
    );
    let zst = h
        .join(".codex/archived_sessions")
        .join(format!("{}.zst", rollout_name(ID)));
    fs::create_dir_all(zst.parent().unwrap()).unwrap();
    fs::write(&zst, "this is not a zstd stream at all ".repeat(100)).unwrap();
    let answer = scan(h, &["codex"], None);
    assert_eq!(sums(&answer, "codex"), [300, 20, 0, 0, 2], "{answer}");
    assert_eq!(answer["kinds"]["codex"]["readFailures"], 1, "{answer}");
}

/// M4 for Muse: the longer copy of a session cannot be opened; the shorter readable one counts.
#[cfg(unix)]
#[test]
fn an_unreadable_longest_muse_copy_falls_back_to_a_readable_one() {
    if running_as_root() {
        return;
    }
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".local/share/muse/sessions/sess-0001/session.jsonl"),
        &muse_session(100),
    );
    let longer = h.join("lab/run/muse/sess-0001/session.jsonl");
    write(&longer, &format!("{}{}", muse_session(100), muse_session(100)));
    deny(&longer);
    let answer = scan(h, &["muse"], None);
    allow(&longer, 0o644);
    assert_eq!(sums(&answer, "muse"), [100, 10, 0, 0, 1], "{answer}");
    assert_eq!(answer["kinds"]["muse"]["readFailures"], 1, "{answer}");
}
