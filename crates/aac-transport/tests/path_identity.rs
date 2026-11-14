//! H1 regression: a verified cwd file must never be replaced by a PATH lookup.
#![allow(clippy::unwrap_used)]
#![cfg(unix)]
use aac_config::{ComputerConfig, OperatingSystem, TransportConfig};
use aac_core::{ComputerId, sha256_hex};
use aac_protocol::{Handshake, Request};
use aac_transport::{AgentArtifact, AgentClient, CallOptions, Computer, SshPolicy};
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

#[test]
fn regression_h1_local_artifact_never_searches_path() {
    if std::env::var_os("AAC_M1_PATH_PROBE_CHILD").is_some() {
        let expected = Handshake {
            name: "aac-agent".into(),
            version: "0.1.0".into(),
            protocol: 1,
            target: "fixture-target".into(),
        };
        let bytes = std::fs::read("fixture-agent").unwrap();
        let artifact =
            AgentArtifact::load("fixture-agent".into(), sha256_hex(bytes), expected).unwrap();
        let computer = Computer::new(
            ComputerConfig {
                id: ComputerId::new("fixture").unwrap(),
                label: "Fixture".into(),
                os: OperatingSystem::Linux,
                transport: TransportConfig::Local,
                roles: Vec::new(),
                metadata: Default::default(),
            },
            SshPolicy::new("/unused/known_hosts".into(), "/unused/run".into()),
        )
        .unwrap();
        let client = AgentClient::new(computer, artifact);
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            client
                .ensure(&CallOptions::new(Duration::from_secs(5), 4096))
                .await
                .unwrap();
            let request = Request {
                protocol: 1,
                verb: "health".into(),
                id: "fixture".into(),
                deadline_ms: 1000,
                args: json!({}),
            };
            client
                .call(&request, &CallOptions::new(Duration::from_secs(5), 4096))
                .await
                .unwrap();
        });
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let verified = root.path().join("verified");
    let conflicting = root.path().join("conflicting");
    std::fs::create_dir(&verified).unwrap();
    std::fs::create_dir(&conflicting).unwrap();
    let script = r#"#!/bin/sh
if [ "$1" = '--version' ]; then
  printf '%s\n' '{"name":"aac-agent","version":"0.1.0","protocol":1,"target":"fixture-target"}'
else
  IFS= read -r request
  printf '%s\n' '{"type":"result","protocol":1,"id":"fixture","value":{"status":"ok"}}'
fi
"#;
    let trusted_path = verified.join("fixture-agent");
    std::fs::write(&trusted_path, script).unwrap();
    std::fs::set_permissions(&trusted_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let conflict_path = conflicting.join("fixture-agent");
    let conflict = script.replacen(
        "#!/bin/sh\n",
        "#!/bin/sh\nprintf invoked > ../path-was-executed\n",
        1,
    );
    std::fs::write(&conflict_path, conflict).unwrap();
    std::fs::set_permissions(&conflict_path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "regression_h1_local_artifact_never_searches_path",
        ])
        .current_dir(&verified)
        .env("AAC_M1_PATH_PROBE_CHILD", "1")
        .env(
            "PATH",
            std::env::join_paths([
                conflicting.as_path(),
                std::path::Path::new("/usr/bin"),
                std::path::Path::new("/bin"),
            ])
            .unwrap(),
        )
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "isolated artifact call failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !root.path().join("path-was-executed").exists(),
        "unverified PATH executable ran"
    );
}
