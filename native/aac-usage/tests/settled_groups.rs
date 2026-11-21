//! Settled files keep their freshness, counts and disclosures (FINAL-NEON H1), and copy groups are
//! ranked with their settled owners still present (FINAL-NEON M1).
use aac_usage::{collector, discover::Env, protocol};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, UNIX_EPOCH},
};

const SLOT_MS: i64 = 600_000;
const NOW: i64 = 1_795_212_000_000;
const OLD: i64 = NOW - 5 * 86_400_000;
const CUTOFF: i64 = NOW - 3 * 86_400_000;
/// 2026-11-14T12:00:00Z, before the settle line.
const USED: i64 = 1_794_657_600_000;
const USED_TEXT: &str = "2026-11-14T12:00:00Z";

fn write_old(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let text: String = lines.iter().map(|line| format!("{line}\n")).collect();
    fs::write(path, text).unwrap();
    fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(OLD as u64))
        .unwrap();
}

fn scan_with(home: &Path, kind: &str, previous: Option<&Value>, extra: Value) -> Value {
    let mut prints = json!({"_store": {
        "settledBefore": {kind: CUTOFF}, "settledSlot": NOW / SLOT_MS,
    }});
    if let Some(previous) = previous {
        prints[kind] = previous["kinds"][kind]["fingerprints"].clone();
    }
    let input = json!({"kinds":[kind],"minDateMs":0,"fingerprints":prints,"cwd":false,"extraRoots":extra});
    let request = protocol::parse_request(input.to_string().as_bytes()).unwrap();
    protocol::scan(&request, home, &Env::default())
}

fn scan(home: &Path, kind: &str, previous: Option<&Value>) -> Value {
    scan_with(home, kind, previous, json!({}))
}

fn input(answer: &Value) -> u64 {
    answer["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|row| row["i"].as_u64())
        .sum()
}

fn prints<'a>(answer: &'a Value, kind: &str) -> &'a serde_json::Map<String, Value> {
    answer["kinds"][kind]["fingerprints"].as_object().unwrap()
}

fn codex_lines(session: &str, totals: &[u64]) -> Vec<Value> {
    let mut lines = vec![
        json!({"type":"session_meta","payload":{"id":session,"model_provider":"openai"}}),
        json!({"type":"turn_context","payload":{"model":"gpt-6.1-sol"}}),
    ];
    for total in totals {
        lines.push(json!({"timestamp":USED_TEXT,"type":"event_msg","payload":{
            "type":"token_count","info":{"total_token_usage":{"input_tokens":total,"output_tokens":1}}
        }}));
    }
    lines
}

fn codex_path(home: &Path, name: &str) -> PathBuf {
    home.join(".codex/sessions/2026/11/14").join(name)
}

const OWNER: &str = "rollout-2026-11-14T12-00-00-3e43c7f5-744b-4790-9d0b-06042ecf0a20.jsonl";
const COPY: &str = "rollout-2026-11-14T12-00-01-7d2b9c11-0c55-4d55-8f0e-1f2e3d4c5b6a.jsonl";

#[test]
fn unread_usage_prints_never_settle() {
    let print = json!({"pathKey":"p","mtimeMs":1,"newestUsageMs":1,"unreadUsage":true});
    assert!(!collector::settled_print(&print, 10));
    let print = json!({"pathKey":"p","mtimeMs":1,"newestUsageMs":1});
    assert!(collector::settled_print(&print, 10));
}

#[test]
fn settled_logs_keep_freshness_counts_and_disclosures() {
    let home = tempfile::tempdir().unwrap();
    write_old(&codex_path(home.path(), OWNER), &codex_lines("quiet", &[100]));
    let first = scan(home.path(), "codex", None);
    assert_eq!(input(&first), 100);
    let full = &first["discovery"]["kinds"]["codex"];
    assert_eq!(full["freshness"]["state"], "current");
    assert_eq!(full["freshness"]["newestDataMs"], USED);

    // An older print disclosed an estimated clock; settling must not drop it.
    let mut previous = first.clone();
    for print in previous["kinds"]["codex"]["fingerprints"]
        .as_object_mut()
        .unwrap()
        .values_mut()
    {
        print["estimatedClock"] = json!(true);
    }
    let settled = scan(home.path(), "codex", Some(&previous));
    assert_eq!(input(&settled), 0);
    let print = prints(&settled, "codex").values().next().unwrap();
    assert_eq!(print["settledCheckedSlot"], NOW / SLOT_MS, "the file settled");
    let discovered = &settled["discovery"]["kinds"]["codex"];
    assert_eq!(discovered["freshness"]["state"], "current");
    assert_eq!(discovered["freshness"]["newestDataMs"], USED);
    assert_eq!(discovered["freshness"]["newestActivityMs"], USED);
    assert_eq!(discovered["freshness"]["activityComplete"], true);
    assert_eq!(discovered["counts"]["filesFound"], full["counts"]["filesFound"]);
    assert_eq!(discovered["counts"]["filesRead"], full["counts"]["filesRead"]);
    assert_eq!(discovered["counts"]["filesFound"], 1);
    assert_eq!(discovered["estimatedClock"], true);
    assert_eq!(settled["kinds"]["codex"]["estimatedClock"], true);
    assert_ne!(settled["kinds"]["codex"]["partial"], true);
}

#[test]
fn a_shorter_codex_copy_never_replaces_its_settled_owner() {
    let home = tempfile::tempdir().unwrap();
    let owner = codex_path(home.path(), OWNER);
    write_old(&owner, &codex_lines("dup", &[200, 500]));
    let first = scan(home.path(), "codex", None);
    assert_eq!(input(&first), 500);
    let settled = scan(home.path(), "codex", Some(&first));
    assert_eq!(input(&settled), 0);
    let (key, owner_print) = prints(&settled, "codex").iter().next().unwrap();
    assert_eq!(owner_print["settledCheckedSlot"], NOW / SLOT_MS);

    // An earlier, truncated copy of the same conversation turns up beside it.
    write_old(&codex_path(home.path(), COPY), &codex_lines("dup", &[200]));
    let after = scan(home.path(), "codex", Some(&settled));
    assert_eq!(
        input(&after),
        0,
        "the shorter copy was read in the settled owner's place"
    );
    let print = &prints(&after, "codex")[key];
    assert_eq!(print["pathKey"], owner_print["pathKey"]);
    assert_eq!(after["discovery"]["kinds"]["codex"]["counts"]["filesFound"], 1);

    // A longer copy outranks the settled owner: the conversation is read again, from that copy.
    write_old(&codex_path(home.path(), COPY), &codex_lines("dup", &[200, 500, 900]));
    let longer = scan(home.path(), "codex", Some(&after));
    assert_eq!(input(&longer), 900);
    assert_ne!(prints(&longer, "codex")[key]["pathKey"], owner_print["pathKey"]);
}

fn claude_line(id: &str) -> Value {
    json!({"type":"assistant","timestamp":USED_TEXT,"sessionId":"s-1","requestId":format!("r-{id}"),
        "message":{"id":id,"model":"claude-opus-5-5","usage":{"input_tokens":100,"output_tokens":20}}})
}

#[test]
fn an_unsettled_claude_copy_reads_its_settled_group_whole() {
    let home = tempfile::tempdir().unwrap();
    let name = "0b6e8f8e-6d7c-4b8b-9d43-0e3f7b0d2a11.jsonl";
    let owner = home.path().join(".claude/projects/-home-u-a").join(name);
    write_old(&owner, &[claude_line("m1"), claude_line("m2")]);
    let first = scan(home.path(), "claude", None);
    assert_eq!(input(&first), 200);
    let settled = scan(home.path(), "claude", Some(&first));
    assert_eq!(input(&settled), 0);
    assert_eq!(
        prints(&settled, "claude").values().next().unwrap()["settledCheckedSlot"],
        NOW / SLOT_MS
    );

    // A backup holding the session's first response appears in another project folder.
    let copy = home.path().join(".claude/projects/-home-u-b").join(name);
    write_old(&copy, &[claude_line("m1")]);
    let after = scan(home.path(), "claude", Some(&settled));
    // The group is read whole, so its one count replaces the owner's cached rows; the copy alone
    // would add its response again beside them.
    assert_eq!(input(&after), 200);
    let fresh = prints(&after, "claude");
    assert_eq!(fresh.len(), 2);
    assert!(fresh.values().all(|print| print["settledCheckedSlot"].is_null()));
    let groups: Vec<&Value> = fresh.values().map(|print| &print["claudeGroup"]).collect();
    assert!(groups[0].is_string() && groups[0] == groups[1]);
}

fn muse_lines(events: usize) -> Vec<Value> {
    (0..events)
        .map(|_| {
            json!({"recorded_at":USED,"payload":{"event":{"kind":"model_completed","model":"fixture-model",
                "usage":{"input_tokens":100,"output_tokens":20,"cache_read_tokens":0,"cache_write_tokens":0}}}})
        })
        .collect()
}

#[test]
fn a_shorter_muse_copy_never_replaces_its_settled_owner() {
    let home = tempfile::tempdir().unwrap();
    let session = "01a0f37a-fed0-7a01-8ca3-437b0a1b4e0a";
    let owner = home
        .path()
        .join(".local/share/muse/sessions")
        .join(session)
        .join("session.jsonl");
    write_old(&owner, &muse_lines(2));
    let backup = home.path().join("muse-backup");
    fs::create_dir_all(&backup).unwrap();
    let extra = json!({"muse": [backup]});
    let first = scan_with(home.path(), "muse", None, extra.clone());
    assert_eq!(input(&first), 200);
    let settled = scan_with(home.path(), "muse", Some(&first), extra.clone());
    assert_eq!(input(&settled), 0);
    let (key, owner_print) = prints(&settled, "muse").iter().next().unwrap();
    assert_eq!(owner_print["settledCheckedSlot"], NOW / SLOT_MS);

    write_old(&backup.join(session).join("session.jsonl"), &muse_lines(1));
    let after = scan_with(home.path(), "muse", Some(&settled), extra);
    assert_eq!(
        input(&after),
        0,
        "the shorter copy was read in the settled owner's place"
    );
    assert_eq!(prints(&after, "muse")[key]["pathKey"], owner_print["pathKey"]);
}
