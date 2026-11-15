//! Production SshPolicy against an owned loopback sshd, in a fixture mount namespace.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]
use aac_transport::SshPolicy;
#[tokio::test]
#[ignore = "requires sudo, mount namespaces and an owned loopback sshd; run explicitly with --ignored"]
async fn pinned_policy_isolates_user_config_and_trust() {
    let root = tempfile::tempdir().unwrap();
    let policy = SshPolicy::new(root.path().join("known_hosts"), root.path().join("control"));
    std::fs::write(root.path().join("known_hosts"), "fixture trust identity").unwrap();
    let trusted = policy.argv(&root.path().join("key"), false).await.unwrap();
    std::fs::write(root.path().join("known_hosts"), "").unwrap();
    let empty = policy.argv(&root.path().join("key"), false).await.unwrap();
    let as_text = |args: Vec<std::ffi::OsString>| {
        args.into_iter()
            .map(|s| s.into_string().unwrap())
            .collect::<Vec<_>>()
    };
    let args = serde_json::json!({"trusted": as_text(trusted), "empty": as_text(empty)});
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

#[tokio::test]
async fn actual_openssh_81_omits_unsupported_policy_option() {
    let Some(executable) = std::env::var_os("AAC_TEST_OPENSSH_81") else {
        return;
    };
    let mut policy = SshPolicy::new("/fixture/known_hosts".into(), "/fixture/control".into());
    policy.ssh = executable.into();
    let args = policy
        .argv(std::path::Path::new("/fixture/key"), false)
        .await
        .unwrap();
    assert!(!args.iter().any(|arg| arg == "KnownHostsCommand=none"));
    let output = std::process::Command::new(&policy.ssh)
        .args(["-G", "-F", "none"])
        .args(args)
        .arg("localhost")
        .output()
        .unwrap();
    assert!(output.status.success(), "old client rejected policy argv");
}

#[tokio::test]
async fn control_path_changes_with_pin_identity_and_stays_short_on_long_roots() {
    let root = tempfile::tempdir().unwrap();
    let long = root.path().join("long".repeat(40));
    let policy = SshPolicy::new(root.path().join("known_hosts"), long);
    std::fs::write(&policy.known_hosts, "first").unwrap();
    let args1 = policy.argv(&root.path().join("key"), false).await.unwrap();
    std::fs::write(&policy.known_hosts, "second trust identity").unwrap();
    let args2 = policy.argv(&root.path().join("key"), false).await.unwrap();
    let control = |args: Vec<std::ffi::OsString>| {
        args.into_iter()
            .map(|s| s.into_string().unwrap())
            .find(|s| s.starts_with("ControlPath="))
            .unwrap()
    };
    let first = control(args1);
    let second = control(args2);
    assert_ne!(first, second);
    assert!(
        second
            .trim_start_matches("ControlPath=")
            .replace("%C", &"f".repeat(40))
            .len()
            < 104
    );
}
