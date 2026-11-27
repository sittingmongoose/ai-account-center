//! End-to-end parity checks for synthetic fixture commands.
use std::process::Command;
#[test]
fn fixture_commands_are_native_and_refuse_overwrite() {
    let dir = tempfile::tempdir().expect("temp folder").keep();
    for kind in ["status", "scoreboard"] {
        let path = dir.join(kind);
        let args = [
            "fixture",
            kind,
            path.to_str().expect("utf8"),
            "--now",
            "1793534400123",
        ];
        assert!(
            Command::new(env!("CARGO_BIN_EXE_aac-xtask"))
                .env("HOME", &dir)
                .env_remove("CCS_DIR")
                .env_remove("CCS_HOME")
                .args(args)
                .args(if kind == "status" {
                    vec!["--no-video"]
                } else {
                    vec![]
                })
                .status()
                .expect("spawn")
                .success()
        );
        assert!(
            !Command::new(env!("CARGO_BIN_EXE_aac-xtask"))
                .env("HOME", &dir)
                .env_remove("CCS_DIR")
                .env_remove("CCS_HOME")
                .args(args)
                .status()
                .expect("spawn")
                .success()
        );
    }
}

#[test]
fn fixture_matches_javascript_oracles() {
    let status = Command::new("node")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixture-parity.mjs"
        ))
        .arg(env!("CARGO_BIN_EXE_aac-xtask"))
        .status()
        .expect("Node oracle runs");
    assert!(status.success());
}
