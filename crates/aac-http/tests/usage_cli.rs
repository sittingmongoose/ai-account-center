//! The published launcher enters the native offline reader without booting the dashboard.
use serde_json::{Value, json};
use std::{fs, process::Command};
#[test]
fn native_usage_entrypoint_reads_only_the_explicit_snapshot() {
    let root = tempfile::tempdir().expect("isolated usage folder").keep();
    fs::write(root.join("usage.json"),json!({"accounts":[{"provider":"codex","sampledAt":"2000-01-01T00:00:00Z","windows":[{"burn":{"pace":1}}]},{"provider":"claude"}]}).to_string()).expect("offline snapshot");
    let output = Command::new(env!("CARGO_BIN_EXE_ccs"))
        .args(["usage", "--dir"])
        .arg(&root)
        .args(["--provider", "openai", "--json"])
        .env("HOME", &root)
        .env("CCS_HOME", &root)
        .env_remove("AAC_USAGE_DIR")
        .output()
        .expect("native offline CLI");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let doc: Value = serde_json::from_slice(&output.stdout).expect("JSON output");
    assert_eq!(
        doc["accounts"].as_array().expect("filtered accounts").len(),
        1
    );
    assert!(doc["accounts"][0]["windows"][0]["burn"].is_null());
    assert!(!root.join(".ccs").exists());
}
