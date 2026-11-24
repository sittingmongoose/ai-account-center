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
fn host_alias(machine: &str) -> &'static str {
    aac_status::computers::registry()[machine]["sshAlias"]
        .as_str()
        .unwrap_or("ubuntu")
}

fn hosts() -> [(&'static str, &'static str); 7] {
    [
        "ubuntu", "mac", "windows", "nas1", "alexis", "odroid", "1070ti",
    ]
    .map(|machine| (machine, host_alias(machine)))
}

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
    fs::write(
        root.join("fixture-computers.json"),
        aac_status::computers::registry().to_string(),
    )
    .expect("fixture computer registry");
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
    let cwd = package(&root);
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

/// The installed package's parts the service reads: the computer table and the helper set,
/// with a stand-in for the Codex runtime that the package build generates.
fn package(root: &Path) -> PathBuf {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let package = root.join("package");
    fs::create_dir_all(package.join("web-dashboard/public")).expect("package table folder");
    fs::copy(
        repo.join("web-dashboard/public/computers.json"),
        package.join("web-dashboard/public/computers.json"),
    )
    .expect("package computer table");
    fs::create_dir_all(package.join("scripts/app-updates")).expect("package helpers");
    for entry in fs::read_dir(repo.join("scripts/app-updates")).expect("helpers") {
        let entry = entry.expect("helper");
        if entry.file_type().expect("helper type").is_file() {
            fs::copy(
                entry.path(),
                package.join("scripts/app-updates").join(entry.file_name()),
            )
            .expect("copy helper");
        }
    }
    fs::create_dir_all(package.join("dist/app-updates")).expect("package runtime");
    fs::write(
        package.join("dist/app-updates/app_update_codex_runtime.cjs"),
        "// fixture runtime\n",
    )
    .expect("runtime stand-in");
    package
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
    assert_eq!(start.body["job"]["expectedResults"], 84);
    let done = until(&owner, |job| job["state"] == "completed").await;
    let rows = done["results"].as_array().expect("job rows");
    assert_eq!(rows.len(), 84);
    for (status, expected) in [
        ("updated", hosts().len()),
        ("held", hosts().len()),
        ("action_required", hosts().len()),
        ("not_installed", 9 * hosts().len()),
    ] {
        assert_eq!(
            rows.iter().filter(|r| r["status"] == status).count(),
            expected
        );
    }
    let fresh = AppUpdates::new(root.clone());
    assert_eq!(fresh.status().await.body["job"], done);
    // Each computer gets the helper set before its agent is asked to update. The local
    // computer's helpers are published for real and answer the agent's capability probe.
    let local = root.join(".ccs/app-updates/app_updates.py");
    let probe = Command::new("/usr/bin/python3")
        .arg("-B")
        .arg(&local)
        .args(["--platform", "ubuntu", "--agent-capabilities"])
        .output()
        .expect("local helper probe");
    assert_eq!(
        String::from_utf8_lossy(&probe.stdout),
        "{\"appWorker\":1}\n"
    );
    for (id, host) in hosts() {
        if id == "ubuntu" {
            continue;
        }
        let events = trace(&root, host);
        assert_eq!(
            events.first().map(|v| &v["stage"]),
            Some(&json!("sync")),
            "{host}"
        );
        let sync = events.iter().position(|v| v["stage"] == "sync");
        let update = events.iter().position(|v| v["stage"] == "update");
        assert!(
            sync < update && update.is_some(),
            "sync before dispatch on {host}"
        );
    }
    for (id, host) in hosts() {
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
        hosts()
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
        hosts().len()
    );
    assert_eq!(owner.cancel().await.status, 200);
    for (_, host) in hosts() {
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
        while !trace(&root, host_alias("windows"))
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
        trace(&root, host_alias("windows"))
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
    fs::remove_file(root.join("hold")).expect("stop holding capable hosts");
    fs::write(root.join("old-windows-agent"), "fixture").expect("select old agent refusal");
    assert_eq!(owner.start().await.status, 202);
    let done = until(&owner, |job| job["state"] == "failed").await;
    let results = done["results"]
        .as_array()
        .expect("complete mixed-version rows");
    let windows = results
        .iter()
        .filter(|row| row["platform"] == "windows")
        .collect::<Vec<_>>();
    assert_eq!(windows.len(), 12);
    for row in windows {
        assert_eq!(
            row["message"],
            "This computer's AAC agent needs an update before Update apps can run here."
        );
        assert_eq!(row["status"], "failed");
        assert_eq!(row["updateAttempted"], false);
    }
    assert!(
        results
            .iter()
            .any(|row| row["platform"] == "alexis" && row["status"] == "updated")
    );
    let windows_trace = trace(&root, host_alias("windows"));
    assert_eq!(
        windows_trace
            .iter()
            .filter(|row| row["stage"] == "legacy-refusal")
            .count(),
        1
    );
    assert_eq!(
        windows_trace
            .iter()
            .filter(|row| row["stage"] == "update")
            .count(),
        2
    );
    acp_follow_up(&root, &owner).await;
    helper_sync_failure(&root, &owner).await;
    retry_failed_rows(&root, &owner).await;

    // Start a fresh boot with no Mac pin and a pinned Windows binary missing on the host.
    let mut catalog = serde_json::from_slice::<Value>(
        &fs::read(root.join("catalog.json")).expect("fixture catalog"),
    )
    .expect("catalog JSON");
    catalog["artifacts"]
        .as_array_mut()
        .expect("pins")
        .retain(|pin| pin["target"] != "aarch64-apple-darwin");
    fs::write(root.join("catalog.json"), catalog.to_string()).expect("catalog without Mac");
    fs::write(root.join("missing-windows-agent"), "fixture")
        .expect("missing installed Windows agent");
    let hosts = [
        ("mac", host_alias("mac")),
        ("windows", host_alias("windows")),
    ]
    .map(|(platform, host)| {
        (
            platform,
            host,
            trace(&root, host)
                .iter()
                .filter(|row| row["stage"] == "update")
                .count(),
        )
    });
    let owner = AppUpdates::new(root.clone());
    assert_eq!(owner.start().await.status, 202);
    let done = until(&owner, |job| job["state"] == "failed").await;
    let results = done["results"]
        .as_array()
        .expect("complete setup-required rows");
    for (platform, host, earlier_updates) in hosts {
        let affected = results
            .iter()
            .filter(|row| row["platform"] == platform)
            .collect::<Vec<_>>();
        assert_eq!(affected.len(), 12);
        for row in affected {
            assert_eq!(
                row["message"],
                "This computer's AAC agent needs setup before Update apps can run here."
            );
            assert_eq!(row["status"], "failed");
            assert_eq!(row["updateAttempted"], false);
        }
        assert_eq!(
            trace(&root, host)
                .iter()
                .filter(|row| row["stage"] == "update")
                .count(),
            earlier_updates
        );
    }
    assert!(
        results
            .iter()
            .any(|row| row["platform"] == "alexis" && row["status"] == "updated")
    );
}

/// Retry runs exactly the last job's failed rows, on their computers only.
async fn retry_failed_rows(root: &Path, owner: &std::sync::Arc<AppUpdates>) {
    fs::write(root.join("fail-apps"), "zcode\nantigravity-cli\n").expect("fail two apps");
    assert_eq!(owner.start().await.status, 202);
    let failed = until(owner, |job| job["state"] == "failed").await;
    let rows = failed["results"].as_array().expect("failed job rows");
    let retryable = rows
        .iter()
        .filter(|row| {
            matches!(
                row["status"].as_str(),
                Some("failed" | "restart_failed" | "unknown")
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(retryable.len(), 2, "two rows failed: {failed}");
    assert!(retryable.iter().all(|row| row["platform"] == "ubuntu"));
    let before = hosts()
        .into_iter()
        .map(|(machine, alias)| {
            (
                machine,
                trace(root, alias)
                    .iter()
                    .filter(|row| row["stage"] == "update")
                    .count(),
            )
        })
        .collect::<Vec<_>>();
    fs::remove_file(root.join("fail-apps")).expect("let the retry rows succeed");
    let retry = owner.retry().await;
    assert_eq!(retry.status, 202, "{}", retry.body);
    assert_eq!(retry.body["job"]["expectedResults"], 2);
    assert_eq!(
        retry.body["job"]["scope"],
        json!({"ubuntu":["antigravity-cli","zcode"]})
    );
    let done = until(owner, |job| job["state"] == "completed").await;
    let rows = done["results"].as_array().expect("retry rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .map(|row| row["appId"].as_str().expect("app id"))
            .collect::<Vec<_>>(),
        ["antigravity-cli", "zcode"]
    );
    assert!(rows.iter().all(|row| row["platform"] == "ubuntu"));
    for (machine, count) in before {
        let updates = trace(root, host_alias(machine))
            .iter()
            .filter(|row| row["stage"] == "update")
            .count();
        if machine == "ubuntu" {
            assert_eq!(updates, count + 1, "the retry reached Ubuntu once");
        } else {
            assert_eq!(updates, count, "the retry never reached {machine}");
        }
    }
    let nothing = owner.retry().await;
    assert_eq!(nothing.status, 409);
    assert_eq!(nothing.body["error"], "There is nothing to retry.");
}

/// A refused helper set dispatches nothing on that computer and says why on every row.
async fn helper_sync_failure(root: &Path, owner: &std::sync::Arc<AppUpdates>) {
    let alias = host_alias("odroid");
    let updates = |root: &Path| {
        trace(root, alias)
            .iter()
            .filter(|row| row["stage"] == "update")
            .count()
    };
    let before = updates(root);
    fs::write(root.join(format!("sync-fail-{alias}")), "").expect("refuse the upload");
    assert_eq!(owner.start().await.status, 202);
    let done = until(owner, |job| job["state"] == "failed").await;
    assert_eq!(updates(root), before, "no dispatch after a failed sync");
    let rows = done["results"].as_array().expect("rows");
    let odroid: Vec<_> = rows
        .iter()
        .filter(|row| row["platform"] == "odroid")
        .collect();
    assert_eq!(odroid.len(), 12);
    for row in odroid {
        assert_eq!(row["status"], "failed");
        assert_eq!(row["updateAttempted"], false);
        assert_eq!(
            row["message"],
            "The update helpers could not be synchronized; no apps were updated on this computer."
        );
        assert_eq!(
            row["diagnostics"],
            json!({"cause":"unknown","reason":"helper_upload_damaged"})
        );
    }
    assert!(
        rows.iter()
            .any(|row| row["platform"] == "nas1" && row["status"] == "updated")
    );
    assert_eq!(
        AppUpdates::new(root.to_owned()).status().await.body["job"],
        done,
        "the saved job restores the sync cause"
    );
    fs::remove_file(root.join(format!("sync-fail-{alias}"))).expect("accept uploads again");
}

async fn acp_follow_up(root: &Path, owner: &std::sync::Arc<AppUpdates>) {
    fs::remove_file(root.join("old-windows-agent")).expect("restore fixture agent");
    fs::write(root.join("defer-acp"), "").expect("defer Linux ACP rows");
    assert_eq!(owner.start().await.status, 202);
    let done = until(owner, |job| job["state"] == "completed").await;
    for (machine, alias) in hosts() {
        let linux = matches!(machine, "ubuntu" | "nas1" | "odroid" | "1070ti");
        let row = acp_row(&done, machine);
        assert_eq!(
            row["status"],
            if linux { "updated" } else { "not_installed" }
        );
        assert_eq!(
            trace(root, alias)
                .iter()
                .filter(|row| row["stage"] == "acp-follow-up")
                .count(),
            usize::from(linux)
        );
    }
    assert_eq!(done["results"].as_array().expect("complete rows").len(), 84);

    fs::write(root.join("invalid-acp"), "").expect("invalid follow-up fixtures");
    assert_eq!(owner.start().await.status, 202);
    let done = until(owner, |job| job["state"] == "failed").await;
    assert_eq!(acp_row(&done, "ubuntu")["status"], "failed");
    for host in ["nas1", "odroid"] {
        assert_eq!(acp_row(&done, host)["status"], "unknown");
    }
    assert_ne!(
        acp_row(&done, "nas1")["message"],
        acp_row(&done, "odroid")["message"]
    );
    assert!(!done.to_string().contains("PRIVATE_SENTINEL"));
    fs::remove_file(root.join("invalid-acp")).expect("restore valid fixture");

    fs::write(root.join("hold-acp"), "").expect("hold follow-up until cancel");
    assert_eq!(owner.start().await.status, 202);
    until(owner, |job| {
        job["state"] == "running"
            && job["hosts"]["ubuntu"]["state"] == "done"
            && trace(root, "ubuntu")
                .iter()
                .filter(|row| row["stage"] == "acp-follow-up")
                .count()
                == 3
    })
    .await;
    assert_eq!(owner.cancel().await.status, 202);
    let done = until(owner, |job| job["state"] == "completed").await;
    assert_eq!(done["cancelRequested"], true);
    for host in ["ubuntu", "nas1", "odroid", "1070ti"] {
        assert_eq!(acp_row(&done, host)["status"], "skipped");
    }
    fs::remove_file(root.join("defer-acp")).expect("reset follow-up fixture");
}

fn acp_row<'a>(job: &'a Value, machine: &str) -> &'a Value {
    job["results"]
        .as_array()
        .expect("job results")
        .iter()
        .find(|row| row["platform"] == machine && row["appId"] == "antigravity-acp")
        .expect("host ACP row")
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
