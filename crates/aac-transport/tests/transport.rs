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
impl Drop for Fixture {
    fn drop(&mut self) {
        let run = self.root.path().join("run");
        let hash = aac_core::sha256_hex(run.to_string_lossy().as_bytes());
        let directory =
            Path::new("/tmp").join(format!("aac-{}-{}", aac_files::path::uid(), &hash[..8]));
        let _ = std::fs::remove_dir(directory);
    }
}

impl Fixture {
    fn new() -> Self {
        Self::for_os(OperatingSystem::Linux)
    }
    fn for_os(os: OperatingSystem) -> Self {
        Self::for_endpoint(os, "fixture.invalid")
    }
    fn for_endpoint(os: OperatingSystem, host: &str) -> Self {
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
                host: host.into(),
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

#[tokio::test]
async fn max1_ipv6_shipping_uses_one_endpoint_for_ssh_and_scp() {
    for host in ["2001:db8::1", "[2001:db8::1]"] {
        let fixture = Fixture::for_endpoint(OperatingSystem::Linux, host);
        let (artifact, scenario) = fixture.artifact();
        fixture.scenario(scenario);
        AgentClient::new(fixture.computer.clone(), artifact)
            .ensure(&fixture.options())
            .await
            .unwrap();
        let traces = fixture.traces();
        for trace in traces.iter().filter(|trace| trace["tool"] == "ssh") {
            let argv = trace["argv"].as_array().unwrap();
            let target = argv[argv.len() - 2].as_str().unwrap();
            assert_eq!(target, "[2001:db8::1]");
            let parsed = std::process::Command::new("/usr/bin/ssh")
                .args(["-G", "-F", "/dev/null"])
                .args(
                    argv[..argv.len() - 1]
                        .iter()
                        .map(|value| value.as_str().unwrap()),
                )
                .output()
                .unwrap();
            assert!(parsed.status.success());
            assert!(
                String::from_utf8(parsed.stdout)
                    .unwrap()
                    .lines()
                    .any(|line| line == "hostname 2001:db8::1"),
                "SSH parser selected a different IPv6 endpoint"
            );
        }
        let upload = traces.iter().find(|trace| trace["tool"] == "scp").unwrap();
        assert!(
            upload["argv"]
                .as_array()
                .unwrap()
                .last()
                .unwrap()
                .as_str()
                .unwrap()
                .starts_with("fixture@[2001:db8::1]:")
        );
    }
}

#[tokio::test]
async fn max1_real_scp_parser_keeps_the_complete_ipv6_host() {
    let fixture = Fixture::for_endpoint(OperatingSystem::Linux, "2001:db8::1");
    let (artifact, scenario) = fixture.artifact();
    fixture.scenario(scenario);
    let recorder = r#"#!/usr/bin/python3
import json,sys
from pathlib import Path
(Path(__file__).parent/'scp-selected.json').write_text(json.dumps(sys.argv[1:]))
sys.exit(1)
"#;
    // The production -S names the probed SSH; record it, then let the recorder stand in for it.
    let wrapper = r#"#!/usr/bin/python3
import subprocess,sys
from pathlib import Path
root=Path(__file__).parent
args=sys.argv[1:]
i=args.index('-S')
(root/'scp-ssh.txt').write_text(args[i+1])
args[i+1]=str(root/'scp-recorder')
sys.exit(subprocess.run(['/usr/bin/scp',*args]).returncode)
"#;
    for (name, body) in [("scp", wrapper), ("scp-recorder", recorder)] {
        let path = fixture.root.path().join(name);
        std::fs::write(&path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let _ = AgentClient::new(fixture.computer.clone(), artifact)
        .ensure(&fixture.options())
        .await;
    let args: Vec<String> = serde_json::from_slice(
        &std::fs::read(fixture.root.path().join("scp-selected.json")).unwrap(),
    )
    .unwrap();
    assert!(
        args.iter().any(|arg| arg == "2001:db8::1"),
        "SCP selected a different host: {args:?}"
    );
    assert!(!args.iter().any(|arg| arg == "2001"));
    assert_eq!(
        std::fs::read_to_string(fixture.root.path().join("scp-ssh.txt")).unwrap(),
        fixture.root.path().join("ssh").to_string_lossy(),
        "scp must run the probed SSH file"
    );
}

#[test]
fn max1_unformattable_endpoint_literals_are_refused() {
    let root = tempfile::tempdir().unwrap();
    for host in [
        "alias:2222",
        "[fixture.invalid]",
        "[2001:db8::1]:22",
        "2001:db8::1]",
        "[[2001:db8::1]]",
        "fe80::1%eth0",
    ] {
        let config = ComputerConfig {
            id: ComputerId::new("fixture").unwrap(),
            label: "Fixture".into(),
            os: OperatingSystem::Linux,
            transport: TransportConfig::Ssh {
                host: host.into(),
                user: None,
                port: None,
                key: root.path().join("key"),
            },
            roles: Vec::new(),
            metadata: Default::default(),
        };
        assert!(
            matches!(Computer::new(config,SshPolicy::new(root.path().join("known_hosts"),root.path().join("run"))),Err(error) if error.cause==Cause::Refused)
        );
    }
}

#[tokio::test]
async fn ssh_policy_matches_the_design_for_short_and_scan_calls() {
    let policy = SshPolicy::new(
        "/fixture/data/ssh/known_hosts".into(),
        "/fixture/run".into(),
    );
    for scan in [false, true] {
        let mut args: Vec<String> = policy
            .argv(Path::new("/fixture/key with space"), scan)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let control = args.pop().unwrap();
        assert!(control.starts_with("ControlPath=/fixture/run/cm-"));
        assert!(control.ends_with("-%C"));
        assert_eq!(args.pop().unwrap(), "-o");
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
            // R5a (S3): unified to 6 for both scan and non-scan -> one mux per host
            "ServerAliveCountMax=6",
            "-o",
            "StrictHostKeyChecking=yes",
            "-o",
            "VerifyHostKeyDNS=no",
            "-o",
            "UpdateHostKeys=no",
            "-o",
            "ForwardAgent=no",
            "-o",
            "ForwardX11=no",
            "-o",
            "GlobalKnownHostsFile=none",
            "-o",
            "PermitLocalCommand=no",
            "-o",
            "ClearAllForwardings=yes",
            "-o",
            "UserKnownHostsFile=/fixture/data/ssh/known_hosts",
            "-o",
            "IdentitiesOnly=yes",
            "-i",
            "/fixture/key with space",
            "-o",
            "ControlMaster=auto",
            "-o",
            "ControlPersist=120",
            "-o",
            "KnownHostsCommand=none",
            "-o",
            "SetEnv=AAC_SSH_POLICY=1",
        ];
        assert_eq!(args, expected);
    }
    assert!(policy.argv(Path::new("relative"), false).await.is_err());
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
    // Each call holds its channel long enough that four overlap even when the gate host is
    // loaded (0.1 s once let a slow fourth spawn start after the first call ended).
    fixture.scenario(json!({"mode":"sleep","seconds":1.0}));
    let mut calls = Vec::new();
    for _ in 0..12 {
        let computer = fixture.computer.clone();
        calls.push(tokio::spawn(async move {
            computer
                .execute(
                    &["fixture-agent".into()],
                    &[],
                    &CallOptions::new(Duration::from_secs(10), 1024),
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
    assert!(max <= 4, "{max} channels ran at once; the limit is 4");
    assert_eq!(max, 4, "the four channels never overlapped");
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
async fn scan_shipping_is_fake_only_uploads_once_and_keeps_discovery_frame_capacity() {
    let fixture = Fixture::new();
    let (artifact, mut scenario) = fixture.artifact();
    let value = json!({"version":2,"truncated":false,"kinds":{},"rows":[],"srows":[],"fixture":"x".repeat(1_100_000)});
    scenario["value"] = value.clone();
    fixture.scenario(scenario);
    let client = AgentClient::new(fixture.computer.clone(), artifact);
    let mut request = Request {
        protocol: 1,
        verb: "scan-usage".into(),
        id: "scan-fixture".into(),
        deadline_ms: 1000,
        args: json!({"kinds":["codex"],"minDateMs":0,"fingerprints":{},"fixture":"x".repeat(1_100_000)}),
    };
    let options = CallOptions::new(Duration::from_secs(5), 8 * 1024 * 1024);
    for _ in 0..2 {
        let events = client.call(&request, &options).await.unwrap();
        assert!(
            matches!(events.last(), Some(aac_protocol::Event::Result { value: actual, .. }) if actual == &value)
        );
    }
    assert_eq!(
        fixture
            .traces()
            .iter()
            .filter(|row| row["tool"] == "scp")
            .count(),
        1
    );
    request.verb = "health".into();
    assert_eq!(
        client.call(&request, &options).await.unwrap_err().cause,
        Cause::OutputTooLarge
    );
}

#[tokio::test]
async fn mem7_scan_arguments_are_sent_byte_for_byte_inside_the_envelope_and_let_go() {
    let fixture = Fixture::new();
    let (artifact, mut scenario) = fixture.artifact();
    scenario["recordStdin"] = json!(true);
    scenario["value"] = json!({"version": 2});
    fixture.scenario(scenario);
    let client = AgentClient::new(fixture.computer.clone(), artifact);
    // Raw spellings (spacing, escapes, number forms) are kept as the caller wrote them.
    let text = format!(
        r#"{{ "kinds":["codex"], "minDateMs":0e0, "fingerprints":{{"a\u00e9":{{"x":1.50}}}}, "fixture":"{}" }}"#,
        "x".repeat(200_000)
    );
    let args: StdinPart = std::sync::Arc::new(text.clone().into_bytes());
    let options = CallOptions::new(Duration::from_secs(5), 1024 * 1024);
    let (_input, controls) = tokio::sync::mpsc::channel(8);
    let (progress, _events) = tokio::sync::mpsc::channel(4);
    let terminal = client
        .call_controlled_args(
            "scan-usage",
            "g14-activity-1-ubuntu-codex",
            120_000,
            std::sync::Arc::clone(&args),
            &options,
            controls,
            progress,
        )
        .await
        .unwrap();
    assert!(matches!(terminal, RawTerminal::Result(value) if value == br#"{"version": 2}"#));
    // The frame the dashboard wrote before (the envelope, then the arguments' text) and its
    // line end.
    let expected = format!(
        r#"{{"protocol":1,"verb":"scan-usage","id":"g14-activity-1-ubuntu-codex","deadlineMs":120000,"args":{text}}}"#
    ) + "\n";
    assert_eq!(
        std::fs::read(fixture.root.path().join("stdin.bin")).unwrap(),
        expected.into_bytes()
    );
    assert_eq!(
        std::sync::Arc::strong_count(&args),
        1,
        "the transport holds no share of the arguments after the call"
    );
    for (verb, id, text) in [
        ("scan-usage", "fixture", "[1]"),
        ("scan-usage", "fixture", "{\"a\":"),
        ("scan-usage", "bad id", "{}"),
        ("Scan", "fixture", "{}"),
    ] {
        let (_input, controls) = tokio::sync::mpsc::channel(8);
        let (progress, _events) = tokio::sync::mpsc::channel(4);
        let refused = client
            .call_controlled_args(
                verb,
                id,
                1000,
                std::sync::Arc::new(text.as_bytes().to_vec()),
                &options,
                controls,
                progress,
            )
            .await;
        assert!(
            matches!(refused, Err(ref failure) if failure.cause == Cause::Refused),
            "{verb} {id} {text}"
        );
    }
}

#[tokio::test]
async fn mem7_shared_stdin_parts_are_written_in_order() {
    let fixture = Fixture::new();
    let argv: Vec<OsString> = vec!["fixture-agent".into()];
    let first: StdinPart = std::sync::Arc::new(b"{\"fixture\":".to_vec());
    let stdin = Stdin::default()
        .part(std::sync::Arc::clone(&first))
        .part(std::sync::Arc::new(b"1}"))
        .part(std::sync::Arc::new(b"\n"));
    assert_eq!(stdin.len(), 14);
    let output = fixture
        .computer
        .execute_stdin(&argv, stdin, &fixture.options())
        .await
        .unwrap();
    assert_eq!(output.stdout, b"{\"fixture\":1}\n");
    assert_eq!(std::sync::Arc::strong_count(&first), 1);
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

#[tokio::test]
async fn regression_m4_option_paths_survive_openssh_parsing_or_are_refused() {
    let policy = SshPolicy::new("/fixture/known_hosts".into(), "/fixture/run".into());
    let output = std::process::Command::new("/usr/bin/ssh")
        .args(["-G", "-F", "/dev/null"])
        .args(
            policy
                .argv(Path::new("/fixture/key with space"), false)
                .await
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
                matches!(policy.argv(Path::new("/fixture/key"), false).await, Err(error) if error.cause == Cause::Refused),
                "unsupported OpenSSH option path must be refused"
            );
        }
    }
}

#[tokio::test]
async fn l5_openssh_option_paths_with_spaces_are_one_literal_path() {
    let policy = SshPolicy::new("/fixture/known hosts".into(), "/fixture/run space".into());
    let argv = policy
        .argv(Path::new("/fixture/key with space"), false)
        .await
        .unwrap();
    assert!(
        argv.windows(2)
            .any(|a| a[0] == "-o" && a[1] == "UpdateHostKeys=no")
    );
    let output = std::process::Command::new("/usr/bin/ssh")
        .args(["-G", "-F", "/dev/null"])
        .args(argv)
        .arg("fixture.invalid")
        .output()
        .unwrap();
    assert!(output.status.success());
    let config = String::from_utf8(output.stdout).unwrap();
    assert!(config.lines().any(|line| line == "updatehostkeys false"));
    assert!(
        config
            .lines()
            .any(|l| l == "userknownhostsfile /fixture/known hosts")
    );
    assert!(
        config
            .lines()
            .any(|l| l.starts_with("controlpath /fixture/run space/cm-"))
    );
}

#[tokio::test]
async fn l5_early_exit_keeps_87_instead_of_epipe_and_does_not_open_breaker() {
    let fixture = Fixture::new();
    let ssh = fixture.root.path().join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nif [ \"$1\" = \"-G\" ]; then exit 0; fi\nexit 87\n",
    )
    .unwrap();
    for _ in 0..4 {
        let output = fixture
            .computer
            .execute(
                &["fixed-launcher".into()],
                &vec![b'x'; 1024 * 1024],
                &fixture.options(),
            )
            .await
            .unwrap();
        assert_eq!(output.code, Some(87));
    }
}

#[tokio::test]
async fn l5_second_call_reuses_a_verified_handshake() {
    let fixture = Fixture::new();
    let (artifact, scenario) = fixture.artifact();
    fixture.scenario(scenario);
    let client = AgentClient::new(fixture.computer.clone(), artifact);
    let request = Request {
        protocol: 1,
        verb: "health".into(),
        id: "reuse".into(),
        deadline_ms: 1000,
        args: json!({}),
    };
    client.call(&request, &fixture.options()).await.unwrap();
    client
        .clone()
        .call(&request, &fixture.options())
        .await
        .unwrap();
    let traces = fixture.traces();
    let probes = traces
        .iter()
        .filter(|t| {
            t["tool"] == "ssh"
                && t["argv"]
                    .as_array()
                    .unwrap()
                    .last()
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .contains("probe")
        })
        .count();
    assert_eq!(probes, 1, "one verified handshake per shared client");
}

#[tokio::test]
async fn l5_existing_only_never_prepares_uploads_or_changes_remote_cache() {
    for wrong in [false, true] {
        let fixture = Fixture::new();
        let (artifact, mut scenario) = fixture.artifact();
        if wrong {
            std::fs::write(
                fixture.root.path().join("installed"),
                b"fixture binary bytes",
            )
            .unwrap();
            scenario["wrongHandshake"] = json!("version");
        }
        fixture.scenario(scenario);
        assert!(
            AgentClient::existing(fixture.computer.clone(), artifact)
                .ensure(&fixture.options())
                .await
                .is_err()
        );
        let traces = fixture.traces();
        assert_eq!(traces.len(), 1);
        assert_eq!(traces[0]["tool"], "ssh");
        assert!(!fixture.root.path().join("upload.part").exists());
    }
}

#[tokio::test]
async fn l5_powershell_smart_quotes_are_refused_before_ssh() {
    let fixture = Fixture::for_os(OperatingSystem::Windows);
    for quote in ['\u{2018}', '\u{2019}', '\u{201a}', '\u{201b}'] {
        let result = fixture
            .computer
            .execute(
                &["fixed".into(), format!("x{quote}; injected").into()],
                &[],
                &fixture.options(),
            )
            .await;
        assert!(matches!(result,Err(e) if e.cause==Cause::Refused));
    }
    assert!(fixture.traces().is_empty());
}

#[tokio::test]
async fn a_client_replaced_while_a_call_waits_for_a_channel_is_refused_not_launched() {
    let fixture = Fixture::new();
    fixture.scenario(json!({"mode":"sleep","seconds":1.5}));
    let call = |computer: Computer| {
        tokio::spawn(async move {
            computer
                .execute(
                    &["fixture-agent".into()],
                    &[],
                    &CallOptions::new(Duration::from_secs(10), 1024),
                )
                .await
        })
    };
    let busy: Vec<_> = (0..4).map(|_| call(fixture.computer.clone())).collect();
    while fixture
        .traces()
        .iter()
        .filter(|t| t["event"] == "start")
        .count()
        < 4
    {
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // The fifth call builds its argv for the probed client, then waits for a channel.
    let waiting = call(fixture.computer.clone());
    tokio::time::sleep(Duration::from_millis(300)).await;
    let staging = fixture.root.path().join("ssh.next");
    std::fs::write(
        &staging,
        "#!/usr/bin/python3\nfrom pathlib import Path\nPath(__file__).with_name('replacement-ran').write_text('x')\n",
    )
    .unwrap();
    std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::rename(&staging, fixture.root.path().join("ssh")).unwrap();
    for call in busy {
        assert!(call.await.unwrap().is_ok());
    }
    let refused = waiting.await.unwrap().err().unwrap();
    assert_eq!(refused.cause, Cause::Refused);
    assert!(!fixture.root.path().join("replacement-ran").exists());
}

/// S3 R5a: scan and non-scan must hash to one ControlPath per host (was two).
#[tokio::test]
async fn s3_scan_and_short_share_one_mux_per_host() {
    let policy = SshPolicy::new("/fixture/kh".into(), "/fixture/run".into());
    let make = |policy: SshPolicy, scan: bool| async move {
        let args: Vec<String> = policy
            .argv(Path::new("/fixture/key"), scan)
            .await
            .unwrap()
            .into_iter()
            .map(|s| s.into_string().unwrap())
            .collect();
        let cp = args
            .iter()
            .find(|s| s.starts_with("ControlPath="))
            .unwrap()
            .clone();
        let keepalive = args
            .iter()
            .find(|s| s.starts_with("ServerAliveCountMax="))
            .unwrap()
            .clone();
        (cp, keepalive, args)
    };
    let (cp_scan, ka_scan, args_scan) = make(policy.clone(), true).await;
    let (cp_short, ka_short, args_short) = make(policy, false).await;
    assert_eq!(ka_scan, "ServerAliveCountMax=6");
    assert_eq!(ka_short, "ServerAliveCountMax=6");
    assert_eq!(cp_scan, cp_short, "one mux per host, not two");
    for args in [&args_scan, &args_short] {
        assert!(args.iter().any(|a| a == "ControlPersist=120"));
        assert!(args.iter().any(|a| a == "ControlMaster=auto"));
    }
}

/// S3 ssh-mux-orphans: stale cm-* removed, live kept, non-cm* untouched.
#[tokio::test]
async fn s3_stale_cm_sockets_are_purged_and_live_kept() {
    let dir = tempfile::tempdir().unwrap();
    // Live master: keep listener alive.
    let live_path = dir.path().join("cm-live-aaaa-%C");
    let _live = std::os::unix::net::UnixListener::bind(&live_path).unwrap();

    // Stale: bind then drop listener -> file remains but connect refuses.
    // Age it past the 2 s guard so cleanup may purge it.
    let stale_path = dir.path().join("cm-stale-bbbb-%C");
    {
        let l = std::os::unix::net::UnixListener::bind(&stale_path).unwrap();
        drop(l);
    }
    // Ensure the file still exists and connect fails (dead master).
    assert!(stale_path.exists());
    assert!(std::os::unix::net::UnixStream::connect(&stale_path).is_err());
    // Age the stale file past the 2 s mid-creation guard.
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;

    // Fresh stale (should survive because of the 2 s guard).
    let fresh_path = dir.path().join("cm-fresh-cccc-%C");
    {
        let l = std::os::unix::net::UnixListener::bind(&fresh_path).unwrap();
        drop(l);
    }
    assert!(fresh_path.exists());
    // Fresh file's mtime is now (<2 s), so it must survive.

    // Unrelated file must survive.
    let other = dir.path().join("not-a-mux");
    std::fs::write(&other, b"x").unwrap();

    SshPolicy::cleanup_stale_sockets(dir.path());

    assert!(live_path.exists(), "live master socket was purged");
    assert!(
        !stale_path.exists(),
        "stale cm-* socket survived startup purge"
    );
    assert!(
        fresh_path.exists(),
        "fresh cm-* (mid-creation) must survive the 2 s guard"
    );
    assert!(other.exists());
}

/// MEM5 helpers (c): a live master from an older instance (still accepting connects, so S3's
/// stale purge must keep it) is asked to exit at startup; a dead one's file is purged; fresh
/// (mid-creation) files, non-sockets and non-`cm-*` files are untouched. The exit for each live
/// master is `ssh -S <sock> -O exit` (no other ssh flags).
#[tokio::test]
async fn close_foreign_masters_exits_live_and_purges_dead() {
    let dir = tempfile::tempdir().unwrap();
    // Fake ssh: records each invocation line, exits 0.
    let log = dir.path().join("calls.log");
    let ssh = dir.path().join("ssh");
    std::fs::write(
        &ssh,
        format!("#!/bin/sh\necho \"$*\" >> '{}'\nexit 0\n", log.display()),
    )
    .unwrap();
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o700)).unwrap();

    // Live master from another instance.
    let live_path = dir
        .path()
        .join("cm-0000000000000000-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa");
    let _live = std::os::unix::net::UnixListener::bind(&live_path).unwrap();
    // Dead master: bind then drop.
    let dead_path = dir
        .path()
        .join("cm-1111111111111111-bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb");
    {
        let l = std::os::unix::net::UnixListener::bind(&dead_path).unwrap();
        drop(l);
    }
    assert!(dead_path.exists());
    // Age past the 2 s mid-creation guard.
    tokio::time::sleep(std::time::Duration::from_millis(2100)).await;
    // Fresh file, mid-creation: must survive.
    let fresh_path = dir
        .path()
        .join("cm-2222222222222222-cccccccccccccccccccccccccccccccccccccccc");
    {
        let l = std::os::unix::net::UnixListener::bind(&fresh_path).unwrap();
        drop(l);
    }
    // Non-socket and non-cm-* entries: untouched.
    let not_socket = dir.path().join("cm-notasocket");
    std::fs::write(&not_socket, b"x").unwrap();
    let other = dir.path().join("not-a-mux");
    std::fs::write(&other, b"x").unwrap();

    SshPolicy::close_foreign_masters(dir.path(), &ssh, None);

    assert!(!live_path.exists(), "live foreign master socket survived");
    assert!(!dead_path.exists(), "dead cm-* socket survived");
    assert!(fresh_path.exists(), "fresh cm-* removed");
    assert!(not_socket.exists());
    assert!(other.exists());
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.lines().count(),
        1,
        "only the live master is asked to exit: {calls}"
    );
    assert!(
        calls.contains(&format!("-S {} -O exit", live_path.display())),
        "exit argv: {calls}"
    );
}

/// The sweep never fails the boot: no directory, no ssh binary, no masters — still returns.
#[test]
fn close_foreign_masters_is_best_effort() {
    let dir = tempfile::tempdir().unwrap();
    SshPolicy::close_foreign_masters(dir.path(), dir.path().join("missing-ssh").as_path(), None);
    SshPolicy::close_foreign_masters(dir.path(), Path::new("/nonexistent-ssh"), None);
}
