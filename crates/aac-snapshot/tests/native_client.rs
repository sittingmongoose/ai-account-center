//! Publication and native launcher integration, with no installed product or provider required.
#![cfg_attr(windows, allow(unused_imports))]
use aac_snapshot::client;
use serde_json::json;
use std::{fs, path::Path};
#[test]
fn publishes_the_python_reader_as_aac_usage_and_the_native_launchers_beside_it() {
    let dir = tempfile::tempdir().expect("share fixture").keep();
    let computers = dir.join("source-computers.json");
    fs::write(&computers, b"{}").expect("computer registry");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/agent-usage");
    client::sync_snapshot_client(&dir, &source, &computers).expect("native publication");
    // Every computer's agents run `python3 <share>/aac-usage`; most have no native CLI.
    let reader = fs::read_to_string(dir.join("aac-usage")).expect("Python reader");
    assert!(reader.starts_with("#!/usr/bin/env python3\n"));
    let windows = fs::read_to_string(dir.join("aac-usage.cmd")).expect("Windows reader launcher");
    assert!(windows.contains("python"));
    assert!(windows.contains("\r\n"));
    let posix = fs::read_to_string(dir.join("aac-usage-native")).expect("POSIX native launcher");
    assert!(posix.starts_with("#!/bin/sh\n"));
    assert!(posix.contains("ai-account-center usage"));
    let native_cmd =
        fs::read_to_string(dir.join("aac-usage-native.cmd")).expect("Windows native launcher");
    assert!(native_cmd.contains("ai-account-center usage"));
    assert!(native_cmd.contains("\r\n"));
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
    let output = Command::new(share.join("aac-usage-native"))
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
    let output = Command::new(share.join("aac-usage-native"))
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
