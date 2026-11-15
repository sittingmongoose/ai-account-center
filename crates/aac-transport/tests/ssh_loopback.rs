//! Production SshPolicy against an owned loopback sshd, in a fixture mount namespace.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]
use aac_transport::SshPolicy;
#[test]
#[ignore = "requires sudo, mount namespaces and an owned loopback sshd; run explicitly with --ignored"]
fn pinned_policy_isolates_user_config_and_trust() {
    let root = tempfile::tempdir().unwrap();
    let policy = SshPolicy::new(root.path().join("known_hosts"), root.path().join("control"));
    let argv = policy.argv(&root.path().join("key"), false).unwrap();
    let args: Vec<_> = argv.iter().map(|s| s.to_str().unwrap()).collect();
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/spec/agent/ssh_loopback.py");
    let output = std::process::Command::new("sudo")
        .args(["-n", "unshare", "--mount", "--fork", "python3"])
        .arg(script)
        .arg(root.path())
        .arg(serde_json::to_string(&args).unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
