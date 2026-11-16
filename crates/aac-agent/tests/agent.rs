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

#[tokio::test]
async fn signin_fixture_is_read_only_and_reports_existing_expiry() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude");
    std::fs::create_dir(&dir).unwrap();
    let credentials = br#"{"claudeAiOauth":{"accessToken":"fixture-access","refreshToken":"fixture-refresh","expiresAt":1800007200123,"refreshTokenExpiresAt":1800108000123}}"#;
    std::fs::write(dir.join(".credentials.json"), credentials).unwrap();
    let payload = json!({"protocol":1,"verb":"signin-expiry","id":"expiry","deadlineMs":1000,"args":{"home":home.path(),"nowMs":1800000000123_i64}}).to_string()+"\n";
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output)
        .await
        .unwrap();
    assert_eq!(code, 0);
    let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(event["value"]["profiles"][0]["state"], "ok");
    assert_eq!(
        std::fs::read(dir.join(".credentials.json")).unwrap(),
        credentials
    );
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("fixture-access")
    );
}

/// An unreadable settings.json leaf is Python's `read_failed()`: discovery is incomplete,
/// the inventory still answers. Only a denied ancestor chain (or a platform without a
/// search-capable descriptor) may decline the whole verb (qwen lane 2 recheck L1).
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_settings_leaf_is_incomplete_discovery_not_a_verb_decline() {
    use std::os::unix::fs::PermissionsExt;
    if aac_files::path::uid() == 0 {
        return; // Root bypasses the fixture's permission control.
    }
    let home = tempfile::tempdir().unwrap();
    let userdata = home.path().join(".t3/userdata");
    std::fs::create_dir_all(&userdata).unwrap();
    let settings = userdata.join("settings.json");
    std::fs::write(&settings, json!({"providerInstances":{}}).to_string()).unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o000)).unwrap();
    let payload = json!({"protocol":1,"verb":"signin-expiry","id":"leaf","deadlineMs":5000,"args":{"home":home.path(),"nowMs":1800000000123_i64}}).to_string()+"\n";
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output).await;
    // Restore before asserting so the private fixture can always be removed.
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(code.unwrap(), 0);
    let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(event["type"], "result");
    let value = &event["value"];
    assert_ne!(value, &json!({"fallback":"python"}), "the leaf alone never declines the verb");
    let profiles = value["profiles"].as_array().expect("an inventory answers");
    assert!(
        profiles
            .iter()
            .any(|p| p["name"] == "saved-profiles" && p["state"] == "unavailable"),
        "read_failed() marks discovery incomplete: {value}"
    );
}

#[cfg(not(feature = "parity-fixtures"))]
#[tokio::test]
async fn production_verbs_refuse_fixture_injection() {
    for (verb, args) in [
        ("signin-expiry", json!({"keychain":{}})),
        ("inventory", json!({"probe":"cores=1","platform":"linux"})),
        (
            "scan-usage",
            json!({"kinds":["codex"],"minDateMs":0,"_scanFixture":{"home":"/unused","env":{}}}),
        ),
    ] {
        let payload = json!({"protocol":1,"verb":verb,"id":"fixture","deadlineMs":1000,"args":args})
            .to_string() + "\n";
        let mut output = Vec::new();
        assert_eq!(
            aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output)
                .await
                .unwrap(),
            1
        );
        let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(event["failure"]["cause"], "refused");
    }
}

#[cfg(feature = "parity-fixtures")]
#[tokio::test]
async fn scan_uses_the_shared_library_without_changing_its_reply_bytes() {
    let home = tempfile::tempdir().unwrap();
    let args = json!({"kinds":["codex","claude","cursor","devin"],"minDateMs":0,"presence":true,"cwd":false});
    let parsed = aac_usage::protocol::parse_request(&serde_json::to_vec(&args).unwrap()).unwrap();
    let env = aac_usage::discover::env_from(std::iter::empty::<(String, String)>());
    let expected =
        serde_json::to_string(&aac_usage::protocol::scan(&parsed, home.path(), &env)).unwrap();
    let mut fixture_args = args;
    fixture_args["_scanFixture"] = json!({"home":home.path(),"env":{}});
    let input = json!({"protocol":1,"verb":"scan-usage","id":"scan-fixture","deadlineMs":5000,"args":fixture_args}).to_string() + "\n";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aac-agent"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    let bytes = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        bytes,
        format!(
            "{{\"type\":\"result\",\"protocol\":1,\"id\":\"scan-fixture\",\"value\":{expected}}}\n"
        )
    );
}
