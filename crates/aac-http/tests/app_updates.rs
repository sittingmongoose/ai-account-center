//! Exercise the public job-owner API over the real L5 client with a closed SSH recorder.
#![cfg(unix)]
use aac_http::app_updates::AppUpdates;
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

const SOURCE: &str = include_str!("../../../tests/spec/agent/app-update-transport-fixture.py");
const HOSTS: [(&str, &str); 5] = [
    ("ubuntu", "ubuntu"),
    ("mac", "jared-mac"),
    ("windows", "jared-windows"),
    ("nas1", "nas1-agent"),
    ("alexis", "alexis-windows"),
];

#[test]
fn fixture_transport_and_service_api() {
    let dir = tempfile::tempdir().expect("fixture root");
    let root = dir.path().canonicalize().expect("canonical fixture root");
    let bin = root.join("worker");
    let run = root.join("run");
    fs::create_dir(&run).expect("private boot run directory");
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).expect("private boot directory");
    for path in [&bin, &root.join("ssh")] {
        fs::write(path, SOURCE).expect("write fixture command");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).expect("fixture executable");
    }
    let artifact = aac_core::sha256_hex(SOURCE.as_bytes());
    let catalog = json!({"artifacts":[
        {"file":bin,"target":"x86_64-unknown-linux-gnu","version":"0.1.0","sha256":artifact},
        {"file":bin,"target":"aarch64-apple-darwin","version":"0.1.0","sha256":artifact},
        {"file":bin,"target":"x86_64-pc-windows-msvc","version":"0.1.0","sha256":artifact}
    ]});
    fs::write(root.join("catalog.json"), catalog.to_string()).expect("fixture catalog");
    fs::write(
        root.join("config.yaml"),
        json!({
            "agentTransport":"on","agentTransportSsh":{
                "identityFile":root.join("fixture-private-key"),
                "knownHostsFile":root.join("fixture-known-hosts")
            }
        })
        .to_string(),
    )
    .expect("fixture config");
    fs::write(root.join("fixture-private-key"), "fixture").expect("fixture key");
    fs::write(root.join("fixture-known-hosts"), "fixture").expect("fixture pin");
    let cwd = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("fixture package");
    let path = std::env::join_paths([
        root.clone(),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/bin"),
    ])
    .expect("fixture PATH");
    let child = Command::new(std::env::current_exe().expect("test bin"))
        .args(["--exact", "service_fixture_child", "--nocapture"])
        .env_clear()
        .env("PATH", path)
        .env("HOME", &root)
        .env("CCS_HOME", &root)
        .env("AAC_AGENT_PACKAGE_ROOT", cwd)
        .env("AAC_AGENT_IDENTITY_FILE", root.join("fixture-private-key"))
        .env(
            "AAC_AGENT_KNOWN_HOSTS_FILE",
            root.join("fixture-known-hosts"),
        )
        .env("AAC_AGENT_CATALOG", root.join("catalog.json"))
        .env("AAC_TEST_UPDATE_SERVICE_ROOT", &root)
        .output()
        .expect("run isolated test service");
    print!("{}", String::from_utf8_lossy(&child.stdout));
    if !child.status.success() {
        let errors = fs::read_to_string(root.join("fixture-errors")).unwrap_or_default();
        eprintln!("{errors}");
    }
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
    // L5 hashes a long control directory down to a private directory in /tmp.
    let hash = aac_core::sha256_hex(root.join("run/agent-ssh").as_os_str().as_encoded_bytes());
    let uid = fs::metadata(&root)
        .map(|meta| {
            use std::os::unix::fs::MetadataExt;
            meta.uid()
        })
        .expect("fixture uid");
    let control = PathBuf::from(format!("/tmp/aac-{uid}-{}", &hash[..8]));
    if control.is_dir() {
        fs::remove_dir(control).expect("only the fixture control directory");
    }
}

#[tokio::test]
async fn service_fixture_child() {
    let Some(root) = std::env::var_os("AAC_TEST_UPDATE_SERVICE_ROOT").map(PathBuf::from) else {
        return;
    };
    let owner = AppUpdates::new(root.clone());
    assert_eq!(owner.status().await.body, json!({"job":null}));
    assert_eq!(owner.cancel().await.status, 200);
    let start = owner.start().await;
    assert_eq!(start.status, 202, "{}", start.body);
    assert_eq!(start.body["job"]["expectedResults"], 60);
    let done = until(&owner, |job| job["state"] == "completed").await;
    let rows = done["results"].as_array().expect("job rows");
    assert_eq!(rows.len(), 60);
    for (status, expected) in [
        ("updated", 5),
        ("held", 5),
        ("action_required", 5),
        ("not_installed", 45),
    ] {
        assert_eq!(
            rows.iter().filter(|r| r["status"] == status).count(),
            expected
        );
    }
    let fresh = AppUpdates::new(root.clone());
    assert_eq!(fresh.status().await.body["job"], done);
    for (id, host) in HOSTS {
        let events = trace(&root, host);
        assert_eq!(
            events.iter().filter(|v| v["stage"] == "probe").count(),
            1,
            "L5 {host}: {events:?}"
        );
        assert_eq!(events.iter().filter(|v| v["stage"] == "update").count(), 1);
        if id != "ubuntu" {
            assert!(
                events
                    .iter()
                    .any(|v| v["stage"] == "run" && v["warm"] == true),
                "pooled run {host}"
            );
        }
    }

    fs::write(root.join("hold"), "fixture").expect("hold fixture workers");
    assert_eq!(owner.start().await.status, 202);
    assert_eq!(owner.start().await.status, 409);
    assert_eq!(fresh.cancel().await.status, 409, "other job owner's cancel");
    until(&owner, |job| {
        HOSTS
            .iter()
            .all(|(id, _)| job["hosts"][id]["phase"] == "downloading")
    })
    .await;
    let cancel = owner.cancel().await;
    assert_eq!(cancel.status, 202);
    assert_eq!(cancel.body["cancelling"], true);
    let done = until(&owner, |job| job["state"] == "completed").await;
    assert_eq!(done["cancelRequested"], true);
    assert_eq!(
        done["results"]
            .as_array()
            .expect("complete rows")
            .iter()
            .filter(|v| v["status"] == "skipped")
            .count(),
        5
    );
    assert_eq!(owner.cancel().await.status, 200);
    for (_, host) in HOSTS {
        let trace = trace(&root, host);
        assert!(
            trace
                .iter()
                .any(|v| v["stage"] == "cancel" && v["warm"] == true),
            "cancel not sent to {host}"
        );
        assert_eq!(
            trace.iter().filter(|v| v["stage"] == "probe").count(),
            1,
            "the next job and cancel should reuse the validated agent on {host}"
        );
    }
    // Hold the next Windows run channel before its first request reaches the agent.
    fs::write(root.join("hold-windows-connection"), "fixture").expect("hold Windows SSH");
    assert_eq!(owner.start().await.status, 202);
    tokio::time::timeout(Duration::from_secs(5), async {
        while !trace(&root, "jared-windows")
            .iter()
            .any(|row| row["stage"] == "connecting")
        {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("Windows run channel reached the held connection");
    assert_eq!(owner.cancel().await.status, 202);
    fs::write(root.join("release-windows-connection"), "fixture").expect("release Windows SSH");
    let done = until(&owner, |job| job["state"] == "completed").await;
    assert_eq!(
        trace(&root, "jared-windows")
            .iter()
            .filter(|row| row["stage"] == "update")
            .count(),
        2,
        "the cancelled job must never reach Windows update admission"
    );
    assert_eq!(
        done["results"]
            .as_array()
            .expect("complete rows")
            .iter()
            .filter(|row| {
                row["platform"] == "windows"
                    && row["status"] == "skipped"
                    && row["updateAttempted"] == false
            })
            .count(),
        12
    );
}

fn trace(root: &Path, host: &str) -> Vec<Value> {
    fs::read_to_string(root.join(format!("trace-{host}.jsonl")))
        .expect("fixture trace")
        .lines()
        .map(|s| serde_json::from_str(s).expect("trace row"))
        .collect()
}

async fn until(owner: &AppUpdates, check: impl Fn(&Value) -> bool) -> Value {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let job = owner.status().await.body["job"].clone();
            if check(&job) {
                return job;
            }
            if job["state"] == "failed" {
                let rows: Vec<_> = job["results"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|row| row["status"] == "unknown" && row["appId"] == "codex-desktop")
                    .map(|row| row["platform"].as_str())
                    .collect();
                panic!("fixture job failed with unavailable hosts {rows:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture job phase deadline")
}
