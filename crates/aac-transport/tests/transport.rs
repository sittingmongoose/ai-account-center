//! Fake-SSH tests: every remote invocation resolves to fixture executables on child PATH.
#![allow(clippy::unwrap_used)]
#![cfg(unix)]
use aac_config::{ComputerConfig, OperatingSystem, TransportConfig};
use aac_core::{Cause, ComputerId};
use aac_protocol::{Handshake, Request};
use aac_transport::*;
use serde_json::{Value, json};
use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

struct Fixture {
    root: tempfile::TempDir,
    computer: Computer,
}
impl Fixture {
    fn new() -> Self {
        Self::for_os(OperatingSystem::Linux)
    }
    fn for_os(os: OperatingSystem) -> Self {
        let root = tempfile::tempdir().unwrap();
        for tool in ["ssh", "scp"] {
            let path = root.path().join(tool);
            std::fs::write(&path, include_str!("fake_ssh.py")).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(root.path().join("scenario.json"), "{\"mode\":\"ok\"}").unwrap();
        let config = ComputerConfig {
            id: ComputerId::new("fixture").unwrap(),
            label: "Fixture".into(),
            os,
            transport: TransportConfig::Ssh {
                host: "fixture.invalid".into(),
                user: Some("fixture".into()),
                port: Some(2222),
                key: root.path().join("key"),
            },
            roles: Vec::new(),
            metadata: Default::default(),
        };
        let mut policy = SshPolicy::new(root.path().join("known_hosts"), root.path().join("run"));
        policy.path = Some(
            std::env::join_paths([root.path(), Path::new("/usr/bin"), Path::new("/bin")]).unwrap(),
        );
        let computer = Computer::new(config, policy).unwrap();
        Self { root, computer }
    }
    fn scenario(&self, value: Value) {
        std::fs::write(
            self.root.path().join("scenario.json"),
            serde_json::to_vec(&value).unwrap(),
        )
        .unwrap();
    }
    fn traces(&self) -> Vec<Value> {
        std::fs::read_to_string(self.root.path().join("trace.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }
    fn options(&self) -> CallOptions {
        CallOptions::new(Duration::from_secs(5), 1024 * 1024)
    }
    fn artifact(&self) -> (AgentArtifact, Value) {
        let path = self.root.path().join("artifact");
        std::fs::write(&path, b"fixture binary bytes").unwrap();
        let hash = aac_core::sha256_hex(b"fixture binary bytes");
        let identity = Handshake {
            name: "aac-agent".into(),
            version: "0.1.0".into(),
            protocol: 1,
            target: "x86_64-unknown-linux-gnu".into(),
        };
        let artifact = AgentArtifact::load(path, hash.clone(), identity.clone()).unwrap();
        (
            artifact,
            json!({"mode":"shipping", "hash":hash, "handshake":identity}),
        )
    }
}

#[test]
fn ssh_policy_matches_the_design_for_short_and_scan_calls() {
    let policy = SshPolicy::new(
        "/fixture/data/ssh/known_hosts".into(),
        "/fixture/run".into(),
    );
    for scan in [false, true] {
        let args: Vec<String> = policy
            .argv(Path::new("/fixture/key with space"), scan)
            .unwrap()
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let expected = vec![
            "-T",
            "-o",
            "BatchMode=yes",
            "-o",
            "ConnectTimeout=5",
            "-o",
            "ConnectionAttempts=1",
            "-o",
            "ServerAliveInterval=5",
            "-o",
            if scan {
                "ServerAliveCountMax=6"
            } else {
                "ServerAliveCountMax=2"
            },
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "UserKnownHostsFile=/fixture/data/ssh/known_hosts",
            "-o",
            "IdentitiesOnly=yes",
            "-i",
            "/fixture/key with space",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPath=/fixture/run/cm-%C",
            "-o",
            "ControlPersist=120",
        ];
        assert_eq!(args, expected);
    }
    assert!(policy.argv(Path::new("relative"), false).is_err());
}

#[tokio::test]
async fn fake_path_records_argv_and_preserves_input_bytes() {
    let fixture = Fixture::new();
    let argv: Vec<OsString> = vec!["fixture-agent".into(), "quote ' ; $(data)".into()];
    let output = fixture
        .computer
        .execute(&argv, b"{\"fixture\":1}\n", &fixture.options())
        .await
        .unwrap();
    assert_eq!(output.stdout, b"{\"fixture\":1}\n");
    let trace = fixture.traces();
    let args = trace[0]["argv"].as_array().unwrap();
    assert!(args.iter().any(|v| v == "ControlMaster=auto"));
    assert!(args.iter().any(|v| v == "2222"));
    assert_eq!(args[args.len() - 2], "fixture.invalid");
    assert!(args.last().unwrap().as_str().unwrap().contains("'\\''"));
}

#[tokio::test]
async fn timeout_cancel_and_output_cap_terminate_processes() {
    for mode in ["timeout", "cancel", "flood"] {
        let fixture = Fixture::new();
        fixture.scenario(json!({"mode":if mode == "flood" {"flood"} else {"sleep"}}));
        let mut options = fixture.options();
        options.output_cap = 1024;
        if mode == "timeout" {
            options.deadline = tokio::time::Instant::now() + Duration::from_millis(100);
        }
        let cancel = options.cancel.clone();
        let argv = ["fixture-agent".into()];
        let call = fixture.computer.execute(&argv, &[], &options);
        let cancellation = async {
            if mode == "cancel" {
                tokio::time::sleep(Duration::from_millis(100)).await;
                cancel.cancel();
            }
        };
        let (result, ()) = tokio::join!(call, cancellation);
        assert!(
            matches!(result, Err(error) if error.cause == match mode { "timeout"=>Cause::Timeout,"cancel"=>Cause::Cancelled,_=>Cause::OutputTooLarge })
        );
        for trace in fixture.traces().iter().filter(|t| t["event"] == "start") {
            let pid = trace["pid"].as_u64().unwrap();
            assert!(
                !Path::new(&format!("/proc/{pid}")).exists(),
                "child {pid} still exists"
            );
        }
    }
}

#[tokio::test]
async fn clones_limit_a_computer_to_four_concurrent_channels() {
    let fixture = Fixture::new();
    fixture.scenario(json!({"mode":"sleep","seconds":0.1}));
    let mut calls = Vec::new();
    for _ in 0..12 {
        let computer = fixture.computer.clone();
        calls.push(tokio::spawn(async move {
            computer
                .execute(
                    &["fixture-agent".into()],
                    &[],
                    &CallOptions::new(Duration::from_secs(5), 1024),
                )
                .await
        }));
    }
    for call in calls {
        assert!(call.await.unwrap().is_ok());
    }
    let mut active = 0;
    let mut max = 0;
    for trace in fixture.traces() {
        if trace["event"] == "start" {
            active += 1;
            max = max.max(active);
        } else {
            active -= 1;
        }
    }
    assert_eq!(active, 0);
    assert_eq!(max, 4);
}

#[tokio::test]
async fn three_transport_failures_open_the_breaker_then_allow_a_probe() {
    let fixture = Fixture::new();
    fixture.scenario(json!({"mode":"unreachable"}));
    for _ in 0..3 {
        assert!(
            matches!(fixture.computer.execute(&["fixture-agent".into()], &[], &fixture.options()).await, Err(error) if error.cause == Cause::SshUnreachable)
        );
    }
    assert_eq!(fixture.traces().len(), 3);
    assert!(
        fixture
            .computer
            .execute(&["fixture-agent".into()], &[], &fixture.options())
            .await
            .is_err()
    );
    assert_eq!(fixture.traces().len(), 3);
    fixture.scenario(json!({"mode":"ok"}));
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(31)).await;
    tokio::time::resume();
    assert!(
        fixture
            .computer
            .execute(&["fixture-agent".into()], &[], &fixture.options())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn host_key_failure_is_classified_without_retaining_stderr() {
    let fixture = Fixture::new();
    fixture.scenario(json!({"mode":"host-key"}));
    assert!(
        matches!(fixture.computer.execute(&["fixture-agent".into()], &[], &fixture.options()).await, Err(error) if error.cause == Cause::HostKeyMismatch)
    );
}

#[tokio::test]
async fn missing_agent_uploads_once_then_checks_hash_and_handshake_each_time() {
    let fixture = Fixture::new();
    let (artifact, scenario) = fixture.artifact();
    fixture.scenario(scenario);
    let client = AgentClient::new(fixture.computer.clone(), artifact);
    client.ensure(&fixture.options()).await.unwrap();
    client.ensure(&fixture.options()).await.unwrap();
    let uploads = fixture
        .traces()
        .iter()
        .filter(|row| row["tool"] == "scp")
        .count();
    assert_eq!(uploads, 1);
    assert_eq!(
        std::fs::read(fixture.root.path().join("installed")).unwrap(),
        b"fixture binary bytes"
    );
    let request = Request {
        protocol: 1,
        verb: "health".into(),
        id: "fixture-1".into(),
        deadline_ms: 1000,
        args: json!({}),
    };
    let events = client.call(&request, &fixture.options()).await.unwrap();
    assert!(
        matches!(events.last(), Some(aac_protocol::Event::Result { id, .. }) if id == "fixture-1")
    );
}

#[tokio::test]
async fn corrupt_upload_does_not_replace_an_existing_agent_or_retry() {
    let fixture = Fixture::new();
    let (artifact, mut scenario) = fixture.artifact();
    scenario["corruptUpload"] = json!(true);
    fixture.scenario(scenario);
    std::fs::write(
        fixture.root.path().join("installed"),
        b"old retained binary",
    )
    .unwrap();
    let client = AgentClient::new(fixture.computer.clone(), artifact);
    assert!(client.ensure(&fixture.options()).await.is_err());
    assert_eq!(
        std::fs::read(fixture.root.path().join("installed")).unwrap(),
        b"old retained binary"
    );
    assert_eq!(
        fixture
            .traces()
            .iter()
            .filter(|row| row["tool"] == "scp")
            .count(),
        1
    );
}

#[tokio::test]
async fn every_handshake_dimension_is_checked() {
    for field in ["name", "version", "target", "protocol"] {
        let fixture = Fixture::new();
        let (artifact, mut scenario) = fixture.artifact();
        scenario["wrongHandshake"] = json!(field);
        fixture.scenario(scenario);
        let client = AgentClient::new(fixture.computer.clone(), artifact);
        assert!(
            matches!(client.ensure(&fixture.options()).await, Err(error) if error.cause == Cause::AgentMismatch)
        );
    }
}

#[tokio::test]
async fn windows_shipping_uses_the_reported_localappdata_path() {
    let fixture = Fixture::for_os(OperatingSystem::Windows);
    let (artifact, mut scenario) = fixture.artifact();
    let hash = scenario["hash"].as_str().unwrap();
    let path = format!("R:\\redirected-cache\\ai-account-center\\bin\\aac-agent-{hash}.part");
    scenario["partPath"] = json!(path);
    fixture.scenario(scenario);
    let client = AgentClient::new(fixture.computer.clone(), artifact);
    client.ensure(&fixture.options()).await.unwrap();
    let traces = fixture.traces();
    let upload = traces.iter().find(|trace| trace["tool"] == "scp").unwrap();
    assert!(
        upload["argv"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_str()
            .unwrap()
            .ends_with(&path.replace('\\', "/"))
    );
}

#[tokio::test]
async fn agent_event_correlation_and_terminal_order_are_checked() {
    for invalid in ["wrongEventId", "duplicateTerminal"] {
        let fixture = Fixture::new();
        let (artifact, mut scenario) = fixture.artifact();
        scenario[invalid] = json!(true);
        fixture.scenario(scenario);
        let client = AgentClient::new(fixture.computer.clone(), artifact);
        let request = Request {
            protocol: 1,
            verb: "health".into(),
            id: "fixture-1".into(),
            deadline_ms: 1000,
            args: json!({}),
        };
        assert!(
            matches!(client.call(&request, &fixture.options()).await, Err(error) if error.cause == Cause::AgentMismatch)
        );
    }
}

#[tokio::test]
async fn dropping_a_call_reaps_its_process_and_releases_the_channel() {
    let fixture = Fixture::new();
    fixture.scenario(json!({"mode":"sleep"}));
    let computer = fixture.computer.clone();
    let call = tokio::spawn(async move {
        computer
            .execute(
                &["fixture-agent".into()],
                &[],
                &CallOptions::new(Duration::from_secs(5), 1024),
            )
            .await
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while fixture.traces().is_empty() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "fake process did not start"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    call.abort();
    let _ = call.await;
    let pid = fixture.traces()[0]["pid"].as_u64().unwrap();
    while Path::new(&format!("/proc/{pid}")).exists() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "aborted child was not reaped"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    fixture.scenario(json!({"mode":"ok"}));
    assert!(
        fixture
            .computer
            .execute(&["fixture-agent".into()], &[], &fixture.options())
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn control_socket_directory_is_private_and_symlinks_are_refused() {
    let fixture = Fixture::new();
    fixture
        .computer
        .execute(&["fixture-agent".into()], &[], &fixture.options())
        .await
        .unwrap();
    let run = fixture.root.path().join("run");
    assert_eq!(
        std::fs::metadata(&run).unwrap().permissions().mode() & 0o777,
        0o700
    );
    std::fs::remove_dir(&run).unwrap();
    std::os::unix::fs::symlink(fixture.root.path(), &run).unwrap();
    assert!(
        matches!(fixture.computer.execute(&["fixture-agent".into()], &[], &fixture.options()).await, Err(error) if error.cause == Cause::Refused)
    );
}

#[test]
fn regression_m4_option_paths_survive_openssh_parsing_or_are_refused() {
    let policy = SshPolicy::new("/fixture/known_hosts".into(), "/fixture/run".into());
    let output = std::process::Command::new("/usr/bin/ssh")
        .args(["-G", "-F", "/dev/null"])
        .args(
            policy
                .argv(Path::new("/fixture/key with space"), false)
                .unwrap(),
        )
        .arg("fixture.invalid")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "OpenSSH rejected valid pinned argv"
    );
    let config = String::from_utf8(output.stdout).unwrap();
    assert!(
        config
            .lines()
            .any(|line| line == "userknownhostsfile /fixture/known_hosts")
    );
    assert!(config.lines().any(|line| line == "stricthostkeychecking true" || line == "stricthostkeychecking yes"));
    for path in [
        "/fixture/with space",
        "/fixture/with\"quote",
        "/fixture/with'quote",
        "/fixture/with\\escape",
        "/fixture/%h",
        "/fixture/${ENV}",
        "/fixture/~other",
    ] {
        for policy in [
            SshPolicy::new(path.into(), "/fixture/run".into()),
            SshPolicy::new("/fixture/known_hosts".into(), path.into()),
        ] {
            assert!(
                matches!(policy.argv(Path::new("/fixture/key"), false), Err(error) if error.cause == Cause::Refused),
                "unsupported OpenSSH option path must be refused"
            );
        }
    }
}
