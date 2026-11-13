//! Discovery rules (DESIGN section 2) and the scanner's additions (11.1a), each on a fixture home.
//! Every test names the rule it covers; each fails on the Python helper of origin/main, which has
//! none of these rules (evidence/stage1-fail-before-main.txt).

use aac_usage::discover::{self, Env};
use aac_usage::protocol::{self, Request};
use serde_json::{Map, Value, json};
use std::fs;
use std::path::{Path, PathBuf};

const AT: &str = "2026-10-01T15:05:00Z";

fn write(path: &Path, text: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn codex_rollout(session: &str, input: u64) -> String {
    [
        json!({"timestamp": AT, "type": "session_meta", "payload": {"id": session, "model_provider": "openai", "cwd": "/home/u/proj"}}),
        json!({"timestamp": AT, "type": "turn_context", "payload": {"model": "gpt-6.1-sol"}}),
        json!({"timestamp": AT, "type": "event_msg", "payload": {"type": "token_count", "info": {
            "total_token_usage": {"input_tokens": input, "cached_input_tokens": 0, "output_tokens": 10},
            "last_token_usage": {"input_tokens": input, "cached_input_tokens": 0, "output_tokens": 10}}}}),
    ]
    .iter()
    .map(|v| v.to_string())
    .collect::<Vec<_>>()
    .join("\n")
        + "\n"
}

fn claude_line(id: &str, session: &str, cwd: &str) -> String {
    json!({"type": "assistant", "timestamp": AT, "sessionId": session, "cwd": cwd, "requestId": format!("r-{id}"),
        "message": {"id": id, "model": "claude-opus-5-5", "usage": {"input_tokens": 100, "output_tokens": 20}}})
    .to_string()
        + "\n"
}

fn request(kinds: &[&str]) -> Request {
    protocol::parse_request(
        json!({"kinds": kinds, "minDateMs": 1_790_000_000_000u64, "deadlineMs": 60_000})
            .to_string()
            .as_bytes(),
    )
    .unwrap()
}

fn env(home: &Path) -> Env {
    Env::from([
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("PATH".to_string(), String::new()),
    ])
}

fn events(answer: &Value, kind: &str) -> u64 {
    answer["rows"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["k"] == kind)
        .map(|r| r["n"].as_u64().unwrap())
        .sum()
}

fn origins(answer: &Value, kind: &str) -> Vec<(String, u64)> {
    let mut list: Vec<(String, u64)> = answer["discovery"]["kinds"][kind]["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["origin"].as_str().unwrap().to_string(),
                r["counts"]["filesFound"].as_u64().unwrap(),
            )
        })
        .collect();
    list.sort();
    list
}

#[test]
fn codex_reads_archived_sessions_of_every_home() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".codex/sessions/2026/10/01/rollout-2026-10-01T15-00-00-aaaaaaaa-0000-0000-0000-000000000001.jsonl"),
        &codex_rollout("s1", 100),
    );
    // Codex moves a thread's rollout here when the thread is archived; it was read by no one.
    write(
        &h.join(".codex/archived_sessions/rollout-2026-10-01T15-00-00-aaaaaaaa-0000-0000-0000-000000000002.jsonl"),
        &codex_rollout("s2", 200),
    );
    let answer = protocol::scan(&request(&["codex"]), h, &env(h));
    assert_eq!(events(&answer, "codex"), 2);
    assert_eq!(
        origins(&answer, "codex"),
        vec![("default".into(), 1), ("default".into(), 1)]
    );
}

#[test]
fn signature_homes_are_found_by_what_they_hold_not_their_name() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    // The owner's main PC: six ~/.codex-lean* homes, ~2,000 sessions nobody read.
    write(&h.join(".codex-lean-b/config.toml"), "");
    write(
        &h.join(
            ".codex-lean-b/archived_sessions/rollout-2026-10-01T15-00-00-bbbbbbbb-0000-0000-0000-000000000001.jsonl",
        ),
        &codex_rollout("lean-1", 300),
    );
    write(&h.join("codex-work/auth.json"), "{}");
    write(
        &h.join(
            "codex-work/sessions/2026/10/01/rollout-2026-10-01T15-00-00-bbbbbbbb-0000-0000-0000-000000000002.jsonl",
        ),
        &codex_rollout("lean-2", 300),
    );
    // A Claude config dir under another name: `projects` plus one of Claude's own files.
    write(&h.join(".claude-relogin/settings.json"), "{}");
    write(&h.join(".claude-relogin/shell-snapshots/snapshot.sh"), "");
    // Not Claude (DESIGN 16.6): Qoder and Qwen Code keep `projects` with generic files beside it.
    write(&h.join(".qoder/settings.json"), "{}");
    write(&h.join(".qoder/session-env/x"), "");
    write(
        &h.join(".qoder/projects/p/q.jsonl"),
        &claude_line("q1", "q", "/home/u/p"),
    );
    write(&h.join(".qwen/settings.json"), "{}");
    write(&h.join(".qwen/todos/x.json"), "{}");
    write(
        &h.join(".qwen/projects/p/w.jsonl"),
        &claude_line("w1", "w", "/home/u/p"),
    );
    write(
        &h.join(".claude-relogin/projects/p/a.jsonl"),
        &claude_line("m1", "c1", "/home/u/p"),
    );
    // Not homes: no marker, a skipped tree, the experiment tree.
    write(&h.join(".muse-like/sessions/x.jsonl"), &codex_rollout("no", 1));
    write(&h.join("node_modules/config.toml"), "");
    write(
        &h.join("node_modules/sessions/rollout-2026-10-01T15-00-00-cccccccc-0000-0000-0000-000000000001.jsonl"),
        &codex_rollout("nm", 1),
    );
    write(&h.join("PM-Experiments/config.toml"), "");
    write(
        &h.join("PM-Experiments/sessions/rollout-2026-10-01T15-00-00-cccccccc-0000-0000-0000-000000000002.jsonl"),
        &codex_rollout("pm", 1),
    );
    let (codex, claude) = discover::signature_homes(h);
    let names = |list: &[PathBuf]| {
        list.iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&codex), vec![".codex-lean-b", "codex-work"]);
    assert_eq!(names(&claude), vec![".claude-relogin"]);
    let answer = protocol::scan(&request(&["codex", "claude"]), h, &env(h));
    assert_eq!(events(&answer, "codex"), 2);
    assert_eq!(events(&answer, "claude"), 1);
    assert!(origins(&answer, "codex").contains(&("signature".into(), 1)));
}

#[cfg(unix)]
#[test]
fn a_signature_home_linked_out_of_home_is_never_entered() {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write(&outside.path().join("config.toml"), "");
    write(
        &outside
            .path()
            .join("sessions/rollout-2026-10-01T15-00-00-dddddddd-0000-0000-0000-000000000001.jsonl"),
        &codex_rollout("out", 1),
    );
    std::os::unix::fs::symlink(outside.path(), home.path().join(".codex-mount")).unwrap();
    assert!(discover::signature_homes(home.path()).0.is_empty());
}

#[test]
fn t3_instance_homes_come_from_settings_whatever_their_folder() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(".t3/userdata/settings.json"),
        &json!({"providerInstances": {
            "claude_x": {"driver": "claudeAgent", "enabled": false, "config": {"homePath": "~/work/claude-x"}},
            "codex_y": {"driver": "codex", "config": {"homePath": "~/.codex", "shadowHomePath": "~/shadows/y"}},
            "omp": {"driver": "acpRegistry", "config": {"commandPath": "/usr/local/bin/omp", "commandArgs": ["--mode", "acp", "--session-dir", "~/omp-runs"]}},
        }})
        .to_string(),
    );
    // A disabled instance's home still holds the logs it wrote.
    write(
        &h.join("work/claude-x/projects/p/a.jsonl"),
        &claude_line("m1", "c1", "/w"),
    );
    write(
        &h.join("shadows/y/sessions/rollout-2026-10-01T15-00-00-eeeeeeee-0000-0000-0000-000000000001.jsonl"),
        &codex_rollout("t3", 5),
    );
    write(&h.join("omp-runs/2026-10-01T15-00_uuid.jsonl"), &(json!({"type": "message", "timestamp": AT, "message": {"role": "assistant", "model": "m", "usage": {"input": 1, "output": 2, "cacheRead": 0, "cacheWrite": 0}}}).to_string() + "\n"));
    let answer = protocol::scan(&request(&["claude", "codex", "omp"]), h, &env(h));
    assert_eq!(events(&answer, "claude"), 1);
    assert_eq!(events(&answer, "codex"), 1);
    assert_eq!(events(&answer, "omp"), 1);
    assert!(origins(&answer, "omp").contains(&("tool_config".into(), 1)));
    // A disabled instance is a root, never an install signal.
    let instances = discover::t3_instances(h);
    assert!(!discover::installed_signals(h, &env(h), "claude", &instances).contains(&"t3_instance"));
    assert!(discover::installed_signals(h, &env(h), "codex", &instances).contains(&"t3_instance"));
}

#[test]
fn env_vars_add_their_homes_beside_the_defaults() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join("alt-claude/projects/p/a.jsonl"), &claude_line("m1", "c1", "/w"));
    write(&h.join(".claude/projects/p/b.jsonl"), &claude_line("m2", "c2", "/w"));
    write(
        &h.join("alt-codex/sessions/rollout-2026-10-01T15-00-00-ffffffff-0000-0000-0000-000000000001.jsonl"),
        &codex_rollout("e1", 5),
    );
    write(&h.join("xdg/claude/projects/p/c.jsonl"), &claude_line("m3", "c3", "/w"));
    let mut e = env(h);
    e.insert(
        "CLAUDE_CONFIG_DIR".into(),
        h.join("alt-claude").to_string_lossy().into_owned(),
    );
    e.insert("CODEX_HOME".into(), h.join("alt-codex").to_string_lossy().into_owned());
    e.insert("XDG_CONFIG_HOME".into(), h.join("xdg").to_string_lossy().into_owned());
    let answer = protocol::scan(&request(&["claude", "codex"]), h, &e);
    assert_eq!(events(&answer, "claude"), 3);
    assert_eq!(events(&answer, "codex"), 1);
    assert!(origins(&answer, "codex").contains(&("env".into(), 1)));
}

#[test]
fn aac_profile_homes_are_read() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(
        &h.join(
            ".ccs/codex-instances/work/sessions/rollout-2026-10-01T15-00-00-abababab-0000-0000-0000-000000000001.jsonl",
        ),
        &codex_rollout("ccs", 5),
    );
    write(
        &h.join(".ccs/instances/acct/projects/p/a.jsonl"),
        &claude_line("m1", "c1", "/w"),
    );
    let answer = protocol::scan(&request(&["claude", "codex"]), h, &env(h));
    assert_eq!(events(&answer, "codex"), 1);
    assert_eq!(events(&answer, "claude"), 1);
    assert!(origins(&answer, "codex").contains(&("ccs_instance".into(), 1)));
}

#[test]
fn a_copy_in_another_home_counts_once() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let name = "rollout-2026-10-01T15-00-00-12121212-0000-0000-0000-000000000001.jsonl";
    write(&h.join(".codex/sessions").join(name), &codex_rollout("same", 7));
    write(&h.join(".codex-lean/config.toml"), "");
    write(
        &h.join(".codex-lean/archived_sessions").join(name),
        &codex_rollout("same", 7),
    );
    let answer = protocol::scan(&request(&["codex"]), h, &env(h));
    assert_eq!(events(&answer, "codex"), 1);
}

#[test]
fn installed_without_logs_is_told_apart_from_not_installed() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    fs::create_dir_all(h.join(".claude")).unwrap();
    let answer = protocol::scan(&request(&["claude", "codex"]), h, &env(h));
    assert_eq!(answer["kinds"]["claude"]["state"], "not_installed");
    assert_eq!(answer["discovery"]["kinds"]["claude"]["installed"], true);
    assert_eq!(answer["discovery"]["kinds"]["claude"]["signals"], json!(["home"]));
    assert_eq!(answer["discovery"]["kinds"]["codex"]["installed"], false);
}

#[test]
fn tools_without_a_reader_are_reported_where_present() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join(".cursor/projects/p/sdk-agent-store/x/agents/a/store.db"), "");
    write(
        &h.join(".gemini/tmp/proj/chats/session-2026-10-01T15-00-ab.jsonl"),
        "{}\n",
    );
    write(&h.join(".local/share/opencode/storage/message/s/m.json"), "{}");
    write(
        &h.join(".t3/userdata/settings.json"),
        &json!({"providerInstances": {"fx": {"driver": "fx", "config": {}}}}).to_string(),
    );
    let mut r = request(&["claude"]);
    r.presence = true;
    let answer = protocol::scan(&r, h, &env(h));
    let tools = &answer["discovery"]["tools"];
    assert_eq!(tools["cursor"]["cause"], "no_local_usage_records");
    assert_eq!(tools["cursor"]["counts"]["filesFound"], 1);
    assert_eq!(tools["gemini-cli"]["counts"]["filesFound"], 1);
    assert_eq!(tools["opencode"]["counts"]["filesFound"], 1);
    assert_eq!(tools["fx"]["signals"], json!(["t3_instance"]));
    assert!(tools.get("kimi").is_none() && tools.get("grok").is_none() && tools.get("pi").is_none());
}

#[test]
fn another_profile_beside_home_is_never_read() {
    let parent = tempfile::tempdir().unwrap();
    let mine = parent.path().join("me");
    let theirs = parent.path().join("someone");
    write(
        &theirs.join(".codex/sessions/rollout-2026-10-01T15-00-00-34343434-0000-0000-0000-000000000001.jsonl"),
        &codex_rollout("theirs", 9),
    );
    fs::create_dir_all(&mine).unwrap();
    let answer = protocol::scan(&request(&["codex"]), &mine, &env(&mine));
    assert_eq!(events(&answer, "codex"), 0);
    assert_eq!(answer["kinds"]["codex"]["state"], "not_installed");
}

#[test]
fn roots_travel_as_digests_never_paths() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join(".codex-secret-project/config.toml"), "");
    write(
        &h.join(
            ".codex-secret-project/sessions/rollout-2026-10-01T15-00-00-56565656-0000-0000-0000-000000000001.jsonl",
        ),
        &codex_rollout("x", 1),
    );
    let mut r = request(&["codex"]);
    r.presence = true;
    let text = protocol::scan(&r, h, &env(h)).to_string();
    assert!(!text.contains("secret-project"));
    assert!(!text.contains(&h.to_string_lossy().into_owned()));
    let id = discover::root_id("codex", "signature", &h.join(".codex-secret-project/sessions"), h);
    assert_eq!(id.len(), 16);
    assert_eq!(
        discover::tilde(&h.join("a/b"), h),
        format!("~{}a{}b", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR)
    );
}

#[test]
fn sessions_carry_their_full_hash_and_reported_cwd_unless_capture_is_off() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let cwd = h.join("code/proj").to_string_lossy().into_owned();
    write(
        &h.join(".claude/projects/p/a.jsonl"),
        &claude_line("m1", "sess-1", &cwd),
    );
    let answer = protocol::scan(&request(&["claude"]), h, &env(h));
    let key = aac_usage::util::session_key("claude", Some("sess-1")).unwrap();
    let entry = &answer["sessions"][&key];
    assert_eq!(
        entry["hash"],
        aac_usage::util::session_hash("claude", Some("sess-1")).unwrap()
    );
    assert_eq!(
        entry["cwd"]["path"],
        format!("~{}code{}proj", std::path::MAIN_SEPARATOR, std::path::MAIN_SEPARATOR)
    );
    assert_eq!(entry["cwd"]["identitySource"], "inferred_cwd");
    assert_eq!(entry["cwd"]["confidence"], "inferred");
    assert!(!answer.to_string().contains("sess-1"));
    let mut off = request(&["claude"]);
    off.cwd = false;
    assert_eq!(protocol::scan(&off, h, &env(h))["sessions"][&key]["cwd"], Value::Null);
}

#[test]
fn messages_are_sent_once_per_line_across_scans() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let file = h.join(".claude/projects/p/a.jsonl");
    write(&file, &claude_line("m1", "s", "/w"));
    let scan = |prints: &Map<String, Value>| {
        let mut r = request(&["claude"]);
        r.messages = true;
        r.fingerprints = prints.clone();
        protocol::scan(&r, h, &env(h))
    };
    let first = scan(&Map::new());
    assert_eq!(first["messages"].as_array().unwrap().len(), 1);
    let prints = Map::from_iter([("claude".to_string(), first["kinds"]["claude"]["fingerprints"].clone())]);
    // Unchanged: nothing is read again, nothing is sent again.
    assert_eq!(scan(&prints)["messages"].as_array().unwrap().len(), 0);
    // Appended: only the new response is sent.
    fs::write(&file, claude_line("m1", "s", "/w") + &claude_line("m2", "s", "/w")).unwrap();
    let appended = scan(&prints);
    assert_eq!(appended["messages"].as_array().unwrap().len(), 1);
    assert_eq!(events(&appended, "claude"), 2);
}

#[test]
fn requests_with_unknown_fields_or_records_mode_are_refused() {
    assert!(protocol::parse_request(br#"{"kinds":["codex"],"minDateMs":0,"paths":["/"]}"#).is_err());
    assert!(protocol::parse_request(br#"{"kinds":["codex"],"minDateMs":0,"zcodeRecords":true}"#).is_err());
    assert!(protocol::parse_request(br#"{"kinds":["gemini-cli"],"minDateMs":0}"#).is_err());
    let version = protocol::version_json();
    assert_eq!(version["name"], "aac-usage-scan");
    assert_eq!(version["protocol"], protocol::PROTOCOL);
}

#[test]
fn a_transcript_past_260_characters_is_read() {
    // Claude Code names project folders after the working path; on the owner's main PC one ran to 291
    // characters, past Windows' MAX_PATH, and the scanner's file identity skipped it.
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write(&h.join(".claude-t3/acct/settings.json"), "{}");
    // Several folder names, each under the 255-character limit of one name on ext4, APFS and NTFS.
    let mut file = h.join(".claude-t3/acct/projects");
    for part in 0..3 {
        file = file.join(format!(
            "C--Users-x-{part}-{}",
            "very-long-project-folder-name-".repeat(4)
        ));
    }
    let file = file.join("ad66e34b-f9d0-4c32-89b2-2cc98c95b583.jsonl");
    assert!(file.components().all(|c| c.as_os_str().len() < 255));
    assert!(file.to_string_lossy().len() > 300);
    write(&file, &claude_line("m-long", "long", "/w"));
    let answer = protocol::scan(&request(&["claude"]), h, &env(h));
    assert_eq!(events(&answer, "claude"), 1);
}
