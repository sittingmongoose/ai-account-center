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

const OLD_CLIENT: &str = "#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\nwith Path(__file__).with_name('calls').open('a') as f: f.write('old\\n')\nif 'KnownHostsCommand=none' in sys.argv:\n print('command-line: Bad configuration option: knownhostscommand', file=sys.stderr); sys.exit(255)\n";
const MODERN_CLIENT: &str = "#!/usr/bin/python3\nfrom pathlib import Path\nwith Path(__file__).with_name('calls').open('a') as f: f.write('modern\\n')\n";

/// Install an executable through a `cp` child: a write descriptor held by this process could
/// leak into another test thread's fork and make exec fail with ETXTBSY.
fn install(source: &std::path::Path, path: &std::path::Path) {
    let status = std::process::Command::new("cp")
        .arg(source)
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
    let status = std::process::Command::new("chmod")
        .arg("700")
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn client(path: &std::path::Path, body: &str) {
    let staging = path.with_extension("staging");
    std::fs::write(&staging, body).unwrap();
    install(&staging, path);
}

fn fixture_policy(root: &std::path::Path, bin: &std::path::Path) -> SshPolicy {
    let mut policy = SshPolicy::new(root.join("known_hosts"), root.join("control"));
    policy.path = Some(std::env::join_paths([bin, std::path::Path::new("/usr/bin")]).unwrap());
    policy
}

fn text(args: Vec<std::ffi::OsString>) -> Vec<String> {
    args.into_iter().map(|s| s.into_string().unwrap()).collect()
}

fn control(args: &[String]) -> String {
    args.iter()
        .find(|s| s.starts_with("ControlPath="))
        .unwrap()
        .clone()
}

#[tokio::test]
async fn capability_cache_follows_a_same_path_executable_replacement() {
    let root = tempfile::tempdir().unwrap();
    let ssh = root.path().join("ssh");
    client(&ssh, OLD_CLIENT);
    let policy = fixture_policy(root.path(), root.path());
    let key = root.path().join("key");
    let old = text(policy.argv(&key, false).await.unwrap());
    assert!(!old.iter().any(|arg| arg == "KnownHostsCommand=none"));
    let inode = std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&ssh).unwrap());
    // In-place rewrite: same path and inode, new size/mtime/ctime.
    client(&ssh, MODERN_CLIENT);
    assert_eq!(
        inode,
        std::os::unix::fs::MetadataExt::ino(&std::fs::metadata(&ssh).unwrap())
    );
    let modern = text(policy.argv(&key, false).await.unwrap());
    assert!(modern.iter().any(|arg| arg == "KnownHostsCommand=none"));
    assert_ne!(control(&old), control(&modern), "master must not be shared");
    let again = text(policy.argv(&key, false).await.unwrap());
    assert_eq!(control(&modern), control(&again));
    let calls = std::fs::read_to_string(root.path().join("calls")).unwrap();
    assert_eq!(calls.lines().count(), 4, "{calls}");
}

#[tokio::test]
async fn capability_cache_follows_symlink_and_path_changes() {
    let root = tempfile::tempdir().unwrap();
    let (first, second) = (root.path().join("first"), root.path().join("second"));
    std::fs::create_dir_all(&first).unwrap();
    std::fs::create_dir_all(&second).unwrap();
    client(&root.path().join("old"), OLD_CLIENT);
    client(&root.path().join("modern"), MODERN_CLIENT);
    std::os::unix::fs::symlink(root.path().join("old"), first.join("ssh")).unwrap();
    std::os::unix::fs::symlink(root.path().join("modern"), second.join("ssh")).unwrap();
    let key = root.path().join("key");
    let mut policy = fixture_policy(root.path(), &first);
    let has = |args: Vec<String>| args.iter().any(|arg| arg == "KnownHostsCommand=none");
    assert!(!has(text(policy.argv(&key, false).await.unwrap())));
    std::fs::remove_file(first.join("ssh")).unwrap();
    std::os::unix::fs::symlink(root.path().join("modern"), first.join("ssh")).unwrap();
    assert!(has(text(policy.argv(&key, false).await.unwrap())));
    std::fs::remove_file(first.join("ssh")).unwrap();
    std::os::unix::fs::symlink(root.path().join("old"), first.join("ssh")).unwrap();
    assert!(!has(text(policy.argv(&key, false).await.unwrap())));
    policy = fixture_policy(root.path(), &second);
    assert!(has(text(policy.argv(&key, false).await.unwrap())));
}

#[tokio::test]
async fn actual_openssh_81_replaced_at_its_path_is_reprobed() {
    let Some(old) = std::env::var_os("AAC_TEST_OPENSSH_81") else {
        return;
    };
    let root = tempfile::tempdir().unwrap();
    let ssh = root.path().join("ssh");
    install(std::path::Path::new(&old), &ssh);
    let policy = fixture_policy(root.path(), root.path());
    let key = root.path().join("key");
    let before = text(policy.argv(&key, false).await.unwrap());
    assert!(!before.iter().any(|arg| arg == "KnownHostsCommand=none"));
    install(std::path::Path::new("/usr/bin/ssh"), &ssh);
    let after = text(policy.argv(&key, false).await.unwrap());
    assert!(after.iter().any(|arg| arg == "KnownHostsCommand=none"));
    assert_ne!(control(&before), control(&after));
}

#[tokio::test]
#[ignore = "requires AAC_TEST_OPENSSH_81, sudo, mount namespaces and an owned loopback sshd"]
async fn replaced_openssh_81_policy_refuses_the_empty_pin() {
    let old = std::env::var_os("AAC_TEST_OPENSSH_81").expect("AAC_TEST_OPENSSH_81");
    let root = tempfile::tempdir().unwrap();
    let bin = tempfile::tempdir().unwrap();
    let ssh = bin.path().join("ssh");
    install(std::path::Path::new(&old), &ssh);
    let policy = fixture_policy(root.path(), bin.path());
    let key = root.path().join("key");
    std::fs::write(root.path().join("known_hosts"), "fixture trust identity").unwrap();
    let stale = text(policy.argv(&key, false).await.unwrap());
    assert!(!stale.iter().any(|arg| arg == "KnownHostsCommand=none"));
    install(std::path::Path::new("/usr/bin/ssh"), &ssh);
    let trusted = text(policy.argv(&key, false).await.unwrap());
    std::fs::write(root.path().join("known_hosts"), "").unwrap();
    let empty = text(policy.argv(&key, false).await.unwrap());
    assert!(empty.iter().any(|arg| arg == "KnownHostsCommand=none"));
    let args = serde_json::json!({"trusted": trusted, "empty": empty});
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/spec/agent/ssh_loopback.py");
    let output = std::process::Command::new("sudo")
        .args(["-n", "unshare", "--mount", "--fork", "python3"])
        .arg(script)
        .arg(root.path())
        .arg(serde_json::to_string(&args).unwrap())
        .arg("0")
        .arg(&ssh)
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
async fn a_failed_probe_is_retried_after_its_ttl() {
    let root = tempfile::tempdir().unwrap();
    client(
        &root.path().join("ssh"),
        "#!/usr/bin/python3\nimport sys\nfrom pathlib import Path\nwith Path(__file__).with_name('calls').open('a') as f: f.write('probe\\n')\nflag = Path(__file__).with_name('failed-once')\nif not flag.exists():\n    flag.write_text('x'); sys.exit(3)\n",
    );
    let policy = fixture_policy(root.path(), root.path());
    let key = root.path().join("key");
    let calls = || {
        std::fs::read_to_string(root.path().join("calls"))
            .unwrap()
            .lines()
            .count()
    };
    assert!(policy.argv(&key, false).await.is_err());
    assert!(policy.argv(&key, false).await.is_err());
    assert_eq!(calls(), 1, "a failure stays cached inside its TTL");
    tokio::time::pause();
    tokio::time::advance(std::time::Duration::from_secs(31)).await;
    tokio::time::resume();
    let args = text(policy.argv(&key, false).await.unwrap());
    assert!(args.iter().any(|arg| arg == "KnownHostsCommand=none"));
    assert_eq!(calls(), 3);
}

/// Max pair 2 M4: command-line pin policy overrides DNS trust inherited from a config.
#[tokio::test]
async fn effective_policy_disables_dns_trust_on_every_client() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config");
    std::fs::write(&config, "Host *\n VerifyHostKeyDNS yes\n").unwrap();
    let mut clients = vec![std::path::PathBuf::from("/usr/bin/ssh")];
    if let Some(old) = std::env::var_os("AAC_TEST_OPENSSH_81") {
        clients.push(old.into());
    }
    for client in clients {
        let mut policy =
            SshPolicy::new(root.path().join("known_hosts"), root.path().join("control"));
        policy.ssh = client.clone();
        let argv = policy.argv(&root.path().join("key"), false).await.unwrap();
        let output = std::process::Command::new(&client)
            .args(["-G", "-F"])
            .arg(&config)
            .args(argv)
            .arg("fixture.invalid")
            .output()
            .unwrap();
        assert!(output.status.success());
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(text.contains("verifyhostkeydns false\n"), "{text}");
        assert!(text.contains("stricthostkeychecking true\n"), "{text}");
    }
}
