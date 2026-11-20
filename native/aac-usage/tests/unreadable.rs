//! Regression fixtures for the installed Analytics unreadable/cached report, 2026-11-14.
//! No real tool homes are read. Permission checks require an unprivileged Unix user.
#[cfg(unix)]
use aac_usage::discover;
use aac_usage::{discover::Env, protocol};
use serde_json::{Value, json};
use std::{fs, path::Path};

fn scan(home: &Path) -> Value {
    let request = protocol::parse_request(
        json!({"kinds":["claude","codex"], "minDateMs":0, "deadlineMs":60000})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    protocol::scan(&request, home, &Env::new())
}

#[cfg(unix)]
#[test]
fn protected_search_candidate_is_one_named_permission_failure_not_three_log_folders() {
    use std::os::unix::fs::PermissionsExt;
    assert_ne!(unsafe { libc::geteuid() }, 0, "run permission regressions unprivileged");
    let home = tempfile::tempdir().unwrap();
    let backup = home.path().join("protected-backup");
    fs::create_dir(&backup).unwrap();
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o000)).unwrap();
    let answer = scan(home.path());
    fs::set_permissions(&backup, fs::Permissions::from_mode(0o700)).unwrap();
    for kind in ["claude", "codex"] {
        assert!(answer["kinds"][kind]["unreadableFolders"].is_null(), "{answer}");
        assert_eq!(
            answer["kinds"][kind]["partial"], true,
            "permission failure must remain visible"
        );
        let roots = answer["discovery"]["kinds"][kind]["roots"].as_array().unwrap();
        let failed: Vec<_> = roots.iter().filter(|r| r["state"] == "unreadable").collect();
        assert_eq!(failed.len(), 1, "{answer}");
        assert_eq!(failed[0]["cause"], "permission_denied");
        let origin = failed[0]["origin"].as_str().unwrap();
        assert!(matches!(origin, "signature" | "experiment"));
        assert_eq!(failed[0]["root"], discover::root_id(kind, origin, &backup, home.path()));
    }
}

#[test]
fn a_file_in_place_of_a_folder_never_counts_as_an_unreadable_folder() {
    let home = tempfile::tempdir().unwrap();
    fs::write(home.path().join(".claude-t3"), "ordinary file").unwrap();
    fs::create_dir(home.path().join(".codex")).unwrap();
    fs::write(home.path().join(".codex/sessions"), "not a folder").unwrap();
    let answer = scan(home.path());
    for kind in ["claude", "codex"] {
        assert!(answer["kinds"][kind]["unreadableFolders"].is_null(), "{answer}");
        assert_ne!(answer["kinds"][kind]["partial"], true, "{answer}");
    }
}

#[test]
fn sidecar_or_busy_file_failure_never_counts_as_a_folder() {
    use aac_usage::collector::Collector;
    let home = tempfile::tempdir().unwrap();
    let sidecar = home.path().join("state.sqlite-wal");
    fs::write(&sidecar, "sidecar").unwrap();
    let mut collector = Collector::new(0, 10.0, Default::default());
    collector.unreadable_folder("codex", &sidecar);
    assert!(collector.unreadable_folders.is_empty());
    assert_eq!(collector.read_failures["codex"].len(), 1);
}

#[cfg(unix)]
#[test]
fn broken_file_link_is_a_file_failure_not_an_unreadable_folder() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join(".codex/sessions");
    fs::create_dir_all(&logs).unwrap();
    symlink("rollout-loop.jsonl", logs.join("rollout-loop.jsonl")).unwrap();
    let answer = scan(home.path());
    assert!(answer["kinds"]["codex"]["unreadableFolders"].is_null(), "{answer}");
    assert_eq!(answer["kinds"]["codex"]["readFailures"], 1, "{answer}");
}

#[test]
fn empty_rollout_is_fully_read_and_is_revisited_if_it_grows() {
    let home = tempfile::tempdir().unwrap();
    let logs = home.path().join(".codex/sessions");
    fs::create_dir_all(&logs).unwrap();
    let path = logs.join("rollout-empty.jsonl");
    fs::write(&path, "").unwrap();
    let empty = scan(home.path());
    assert_ne!(empty["kinds"]["codex"]["partial"], true, "{empty}");
    assert!(empty["kinds"]["codex"]["unfinished"].is_null(), "{empty}");
    fs::write(&path, "{\"type\":\"event_msg\"}\n").unwrap();
    let grown = scan(home.path());
    assert_eq!(
        grown["kinds"]["codex"]["partial"], true,
        "nonempty headerless data must remain a failure"
    );
    assert_eq!(grown["kinds"]["codex"]["unfinished"][0]["reason"], "no_session_id");
}

// REVIEW-cheap M3: a folder under a mount that did not answer is a read failure, decided from the
// avoided list alone. (The removed extra stat was shown with strace; this pins the outcome.)
#[test]
fn folder_under_an_unanswered_mount_is_a_read_failure_without_a_folder_claim() {
    use aac_usage::{collector::Collector, fsx};
    let home = tempfile::tempdir().unwrap();
    let mount = home.path().join("unanswered-mount");
    let child = mount.join("child");
    fs::create_dir_all(&child).unwrap();
    fsx::avoid(&mount);
    let mut collector = Collector::new(0, 10.0, Default::default());
    let meta = fsx::discovery_metadata(&mut collector, "codex", &child);
    fsx::reset_avoided();
    assert!(meta.is_none());
    assert!(collector.unreadable_folders.is_empty());
    assert_eq!(collector.read_failures["codex"].len(), 1);
}

#[cfg(unix)]
#[test]
fn unlistable_symlinked_directory_is_a_folder_failure() {
    use aac_usage::collector::Collector;
    use std::os::unix::fs::{PermissionsExt, symlink};
    assert_ne!(unsafe { libc::geteuid() }, 0);
    let home = tempfile::tempdir().unwrap();
    let directory = home.path().join("protected");
    let link = home.path().join("linked-logs");
    fs::create_dir(&directory).unwrap();
    symlink(&directory, &link).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o000)).unwrap();
    assert!(aac_usage::fsx::list_dir(&link).is_err());
    let mut collector = Collector::new(0, 10.0, Default::default());
    collector.unreadable_folder("codex", &link);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(collector.unreadable_folders.get("codex").map(|f| f.len()), Some(1));
    assert!(collector.read_failures.is_empty());
    assert!(collector.usage_partial.contains("codex"));
}
