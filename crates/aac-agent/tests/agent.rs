//! The built binary is exercised over the same bounded transport used for a remote agent.
#![allow(clippy::unwrap_used)]
use aac_config::{ComputerConfig, OperatingSystem, TransportConfig};
use aac_core::{Cause, ComputerId, sha256_hex};
use aac_protocol::{Event, Request};
use aac_transport::*;
use serde_json::json;
use std::time::Duration;

fn computer() -> Computer {
    Computer::new(
        ComputerConfig {
            id: ComputerId::new("local-fixture").unwrap(),
            label: "Fixture".into(),
            os: OperatingSystem::Linux,
            transport: TransportConfig::Local,
            roles: Vec::new(),
            metadata: Default::default(),
        },
        SshPolicy::new("/unused/known_hosts".into(), "/unused/run".into()),
    )
    .unwrap()
}

#[tokio::test]
async fn built_agent_hash_handshake_and_health_work_through_local_transport() {
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_aac-agent"));
    let hash = sha256_hex(std::fs::read(&binary).unwrap());
    let artifact = AgentArtifact::load(binary, hash, aac_agent::handshake()).unwrap();
    let client = AgentClient::new(computer(), artifact);
    let request = Request {
        protocol: 1,
        verb: "health".into(),
        id: "health-fixture".into(),
        deadline_ms: 1000,
        args: json!({}),
    };
    let output = client
        .call(&request, &CallOptions::new(Duration::from_secs(5), 4096))
        .await
        .unwrap();
    assert!(
        matches!(output.last(), Some(Event::Result { value, .. }) if value["status"] == "ok" && value["observedAt"].as_u64().is_some())
    );
}

#[tokio::test]
async fn protocol_failures_are_terminal_and_never_dispatch_unknown_verbs() {
    for (payload, cause) in [
        ("{bad}\n", Cause::Damaged),
        (
            "{\"protocol\":2,\"verb\":\"health\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{}}\n",
            Cause::AgentMismatch,
        ),
        (
            "{\"protocol\":1,\"verb\":\"scan-usage\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{}}\n",
            Cause::Refused,
        ),
        (
            "{\"protocol\":1,\"verb\":\"health\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{\"path\":\"/unused\"}}\n",
            Cause::Refused,
        ),
    ] {
        let computer = computer();
        let output = computer
            .execute(
                &[env!("CARGO_BIN_EXE_aac-agent").into()],
                payload.as_bytes(),
                &CallOptions::new(Duration::from_secs(5), 4096),
            )
            .await
            .unwrap();
        assert_eq!(output.code, Some(1));
        let event: Event = serde_json::from_slice(&output.stdout).unwrap();
        assert!(matches!(event, Event::Failure { failure, .. } if failure.cause == cause));
        assert_eq!(output.stdout.iter().filter(|c| **c == b'\n').count(), 1);
    }
}

#[tokio::test]
async fn cancellation_on_open_stdin_is_a_classified_terminal_failure() {
    let input = b"{\"protocol\":1,\"verb\":\"health\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{}}\n{\"cancel\":true}\n";
    let mut output = Vec::new();
    assert_eq!(
        aac_agent::run(tokio::io::BufReader::new(&input[..]), &mut output)
            .await
            .unwrap(),
        1
    );
    assert!(
        matches!(serde_json::from_slice::<Event>(&output).unwrap(), Event::Failure { failure, .. } if failure.cause == Cause::Cancelled)
    );
}

#[tokio::test]
async fn frame_caps_and_truncated_frames_are_enforced() {
    let bytes = b"12345\n";
    let mut reader = tokio::io::BufReader::new(&bytes[..]);
    assert_eq!(
        aac_agent::read_line(&mut reader, 5)
            .await
            .unwrap_err()
            .cause,
        Cause::OutputTooLarge
    );
    let mut reader = tokio::io::BufReader::new(&b"truncated"[..]);
    assert_eq!(
        aac_agent::read_line(&mut reader, 100)
            .await
            .unwrap_err()
            .cause,
        Cause::Damaged
    );
}

#[tokio::test(start_paused = true)]
async fn idle_stdin_has_a_bounded_initial_read_deadline() {
    let (input, _keep_open) = tokio::io::duplex(128);
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(input), &mut output)
        .await
        .unwrap();
    assert_eq!(code, 1);
    assert!(
        matches!(serde_json::from_slice::<Event>(&output).unwrap(), Event::Failure { failure, .. } if failure.cause == Cause::Timeout)
    );
}
