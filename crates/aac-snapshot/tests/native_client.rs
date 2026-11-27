//! Publication and native launcher integration, with no installed product or provider required.
#![cfg_attr(windows, allow(unused_imports))]
use aac_snapshot::client;
use serde_json::json;
use std::{fs, path::Path};
#[test]
fn publishes_native_launchers_and_keeps_the_python_oracle_out_of_the_share() {
    let dir = tempfile::tempdir().expect("share fixture").keep();
    let computers = dir.join("source-computers.json");
    fs::write(&computers, b"{}").expect("computer registry");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/agent-usage");
    client::sync_snapshot_client(&dir, &source, &computers, None).expect("native publication");
    let posix = fs::read_to_string(dir.join("aac-usage")).expect("POSIX launcher");
    assert!(posix.starts_with("#!/bin/sh\n"));
    assert!(posix.contains("ai-account-center usage"));
    assert!(!posix.contains("python"));
    let windows = fs::read_to_string(dir.join("aac-usage.cmd")).expect("Windows launcher");
    assert!(windows.contains("ai-account-center usage"));
    assert!(windows.contains("\r\n"));
    assert!(!windows.contains("python"));
}
#[cfg(unix)]
#[test]
fn published_posix_launcher_forwards_its_directory_and_arguments_to_native_reader() {
    use std::{os::unix::fs::PermissionsExt, process::Command};
    let temp = tempfile::tempdir().expect("fixture").keep();
    let share = temp.join("share with ' quotes");
    let bin = temp.join("bin");
    fs::create_dir(&share).expect("share");
    fs::create_dir(&bin).expect("bin");
    let computers = temp.join("computers.json");
    fs::write(&computers, b"{}").expect("computers");
    client::sync_snapshot_client(
        &share,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/agent-usage"),
        &computers,
        None,
    )
    .expect("publication");
    fs::write(
        share.join("usage.json"),
        json!({"accounts":[{"provider":"codex","id":"work","windows":[]},{"provider":"claude"}]})
            .to_string(),
    )
    .expect("snapshot");
    let command = bin.join("ai-account-center");
    fs::write(
        &command,
        "#!/bin/sh\n[ \"$1\" = usage ] || exit 23\nshift\nexec \"$AAC_READER_BIN\" \"$@\"\n",
    )
    .expect("native command double");
    fs::set_permissions(&command, fs::Permissions::from_mode(0o755)).expect("executable");
    let output = Command::new(share.join("aac-usage"))
        .args(["--provider", "openai", "--json"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("AAC_READER_BIN", env!("CARGO_BIN_EXE_aac-usage"))
        .env("HOME", &temp)
        .env("CCS_HOME", &temp)
        .output()
        .expect("published launcher");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("native JSON");
    assert_eq!(value["accounts"].as_array().expect("accounts").len(), 1);
    assert_eq!(value["accounts"][0]["provider"], "codex");
    let configured = temp.join("configured snapshot");
    fs::create_dir(&configured).expect("configured folder");
    fs::write(
        configured.join("usage.json"),
        json!({"accounts":[{"provider":"codex","id":"configured","windows":[]}]}).to_string(),
    )
    .expect("configured snapshot");
    let output = Command::new(share.join("aac-usage"))
        .args(["--json"])
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env("AAC_READER_BIN", env!("CARGO_BIN_EXE_aac-usage"))
        .env("AAC_USAGE_DIR", &configured)
        .env("HOME", &temp)
        .env("CCS_HOME", &temp)
        .output()
        .expect("configured launcher");
    let value: serde_json::Value = serde_json::from_slice(&output.stdout).expect("configured JSON");
    assert_eq!(value["accounts"][0]["id"], "configured");
}
