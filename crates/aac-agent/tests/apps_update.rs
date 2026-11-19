//! App-update wire, cancel and on-disk recovery gates with fixture installers.
#![cfg(feature = "parity-fixtures")]

use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use aac_protocol::Event;
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout, Command};

const JOB: &str = "cdc2d248a04f48a894be80bd777d866d";
const RETRY: &str = "b6327c4b522c448db5ba2a4c6964d1b0";
const PYTHON_REF: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/spec/agent/app-update.py"
);

fn temp_root() -> TempDir {
    #[cfg(unix)]
    let root = tempfile::tempdir_in(std::env::temp_dir().canonicalize().expect("temp parent"))
        .expect("fixture temp root");
    #[cfg(windows)]
    let root = tempfile::tempdir().expect("fixture temp root");
    root
}

fn root() -> TempDir {
    let root = temp_root();
    put(&root, "app-updates-fixture", "AAC fixture installers v1");
    root
}

fn put(root: &TempDir, path: &str, value: &str) {
    std::fs::write(root.path().join(path), value).expect("write fixture");
}

type LinesFromAgent = Lines<BufReader<ChildStdout>>;

async fn agent(root: &Path, platform: &str, job: &str, action: &str) -> (Child, LinesFromAgent) {
    spawn_agent(root, platform, job, action, &[]).await
}

async fn spawn_agent(
    root: &Path,
    platform: &str,
    job: &str,
    action: &str,
    extra: &[(&str, &Path)],
) -> (Child, LinesFromAgent) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_aac-agent"));
    cmd.kill_on_drop(true)
        .env_clear()
        .env(
            "SystemRoot",
            std::env::var_os("SystemRoot").unwrap_or_default(),
        )
        .env("CCS_HOME", root)
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("AAC_APPS_FIXTURE_ROOT", root)
        .envs(extra.iter().copied())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped());
    let mut child = cmd.spawn().expect("spawn fixture agent");
    let mut stdin = child.stdin.take().expect("agent stdin");
    let request = json!({
        "protocol":1,"verb":"apps","id":job,"deadlineMs":15000,
        "args":{"action":action,"platform":platform,"jobId":job}
    });
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("send request");
    drop(stdin);
    let stdout = child.stdout.take().expect("agent stdout");
    (child, BufReader::new(stdout).lines())
}

async fn event(lines: &mut LinesFromAgent) -> Event {
    let line = tokio::time::timeout(Duration::from_secs(8), lines.next_line())
        .await
        .expect("fixture agent timed out")
        .expect("read agent")
        .expect("agent exited");
    serde_json::from_str(&line).expect("agent event")
}

async fn terminal(child: &mut Child, lines: &mut LinesFromAgent) -> (Vec<Value>, Value) {
    let mut events = Vec::new();
    loop {
        match event(lines).await {
            Event::Progress { data, .. } => events.push(data),
            Event::Result { value, .. } => {
                assert!(child.wait().await.expect("reap agent").success());
                return (events, value);
            }
            failure => panic!("fixture agent error {failure:?}"),
        }
    }
}

async fn downloading(lines: &mut LinesFromAgent) -> Vec<Value> {
    let mut seen = Vec::new();
    loop {
        let frame = event(lines).await;
        let Event::Progress { data, .. } = frame else {
            panic!("expected download progress, got {frame:?}");
        };
        let download = data["appId"] == "codex-desktop" && data["phase"] == "downloading";
        seen.push(data);
        if download {
            return seen;
        }
    }
}

async fn other_call(root: &TempDir, platform: &str, job: &str, action: &str) -> Value {
    let (mut child, mut lines) = agent(root.path(), platform, job, action).await;
    terminal(&mut child, &mut lines).await.1
}

fn reference(platform: &str, scenario: &str) -> Value {
    let python = std::env::var_os("AAC_TEST_PYTHON").unwrap_or_else(|| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    let result = std::process::Command::new(python)
        .args(["-B", PYTHON_REF, platform, scenario])
        .output()
        .expect("start Python reference");
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    serde_json::from_slice(&result.stdout).expect("Python reference reply")
}

fn compare(events: Vec<Value>, payload: &Value, platform: &str, scenario: &str) {
    let expected = reference(platform, scenario);
    assert_eq!(
        payload.to_string(),
        expected["payload"].to_string(),
        "{platform}: {scenario}"
    );
    assert_eq!(
        events.first(),
        expected["events"].get(0),
        "initial progress"
    );
    assert_eq!(
        events.len(),
        expected["events"].as_array().expect("events").len()
    );
    // Threads may report different apps in a different order. Compare every
    // app's event sequence individually, including its terminal result.
    for row in payload["results"].as_array().expect("result rows") {
        let id = &row["appId"];
        let per_app = |events: &[Value]| {
            events
                .iter()
                .filter(|event| &event["appId"] == id || &event["result"]["appId"] == id)
                .map(Value::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            per_app(&events),
            per_app(expected["events"].as_array().expect("events")),
            "{id}"
        );
    }
}

#[tokio::test]
async fn update_and_next_run_match_python_on_three_platforms() {
    for platform in ["ubuntu", "mac", "windows"] {
        let root = root();
        for (job, scenario) in [(JOB, "update"), (RETRY, "current")] {
            let (mut child, mut lines) = agent(root.path(), platform, job, "update").await;
            let (events, payload) = terminal(&mut child, &mut lines).await;
            compare(events, &payload, platform, scenario);
            let stored = other_call(&root, platform, job, "update").await;
            assert_eq!(stored.to_string(), payload.to_string());
        }
    }
}

#[tokio::test]
async fn cancel_during_download_finishes_active_and_skips_pending() {
    let root = root();
    put(&root, "fixture-hold-download", "");
    let (mut child, mut lines) = agent(root.path(), "ubuntu", JOB, "update").await;
    let mut progress = downloading(&mut lines).await;
    assert_eq!(
        other_call(&root, "ubuntu", RETRY, "cancel").await,
        json!({"cancelled":false})
    );
    assert_eq!(
        other_call(&root, "ubuntu", JOB, "cancel").await,
        json!({"cancelled":true})
    );
    assert_eq!(
        other_call(&root, "ubuntu", JOB, "cancel").await,
        json!({"cancelled":true})
    );
    put(&root, "fixture-release-download", "");
    let (later, payload) = terminal(&mut child, &mut lines).await;
    progress.extend(later);
    compare(progress, &payload, "ubuntu", "cancel");
    assert_eq!(
        other_call(&root, "ubuntu", JOB, "cancel").await,
        json!({"cancelled":false})
    );
}

#[tokio::test]
async fn killed_worker_recovers_rows_and_cleans_its_partial_without_replaying() {
    let root = root();
    put(&root, "fixture-hold-download", "");
    put(&root, "other-app.exe", "keep this file");
    let (mut child, mut lines) = agent(root.path(), "ubuntu", JOB, "update").await;
    let partial_events = downloading(&mut lines).await;
    let (mut competing, mut replies) = agent(root.path(), "ubuntu", RETRY, "update").await;
    assert!(matches!(event(&mut replies).await,
        Event::Failure { failure, .. } if failure.cause == aac_core::Cause::Busy));
    assert!(
        !competing
            .wait()
            .await
            .expect("reap competing agent")
            .success()
    );

    child.kill().await.expect("kill only the fixture agent");
    assert!(root.path().join("fixture-download.part").is_file());
    let recovered = other_call(&root, "ubuntu", JOB, "update").await;
    let rows = recovered["results"].as_array().expect("recovered rows");
    assert_eq!(rows.len(), 12);
    let preserved = partial_events
        .iter()
        .filter_map(|event| event.get("result"))
        .collect::<Vec<_>>();
    assert_eq!(preserved.len(), 9);
    for old in preserved {
        assert!(rows.contains(old));
    }
    for app in ["codex-desktop", "t3-code", "antigravity-acp"] {
        let row = rows
            .iter()
            .find(|row| row["appId"] == app)
            .expect("interrupted row");
        assert_eq!(row["status"], "unknown");
        assert_eq!(row["messageCode"], "host_unknown");
    }
    assert!(
        !root
            .path()
            .join("fixture-installed-codex-desktop.txt")
            .exists()
    );
    assert!(!root.path().join("fixture-download.part").exists());
    assert_eq!(
        std::fs::read_to_string(root.path().join("other-app.exe")).expect("unrelated file"),
        "keep this file"
    );

    put(&root, "fixture-release-download", "");
    let (mut child, mut lines) = agent(root.path(), "ubuntu", RETRY, "update").await;
    let (events, payload) = terminal(&mut child, &mut lines).await;
    compare(events, &payload, "ubuntu", "update");
}

#[tokio::test]
async fn all_app_fixtures_match_python_and_respect_parallel_installer_gates() {
    use std::collections::HashSet;
    for platform in ["ubuntu", "mac", "windows"] {
        let root = root();
        put(&root, "fixture-install-all", "");
        let (mut child, mut lines) = agent(root.path(), platform, JOB, "update").await;
        let (events, payload) = terminal(&mut child, &mut lines).await;
        assert_eq!(
            payload.to_string(),
            reference(platform, "all")["payload"].to_string()
        );

        let mut active = HashSet::new();
        let mut finished = HashSet::new();
        let mut peak = 0;
        for event in &events {
            if event["event"] == "result" {
                let app = event["result"]["appId"].as_str().expect("app row");
                assert!(active.remove(app));
                finished.insert(app);
                continue;
            }
            let Some(app) = event["appId"].as_str() else {
                continue;
            };
            if !active.insert(app) {
                continue; // An update phase or a re-display of active progress.
            }
            peak = peak.max(active.len());
            match app {
                "codex-desktop" => assert!(finished.contains("codex-cli")),
                "t3-acp-adapters" => assert!(finished.contains("zcode")),
                "antigravity-acp" => assert!(finished.contains("t3-code")),
                "t3-code" => assert!(finished.len() >= if platform == "ubuntu" { 10 } else { 9 }),
                _ => {}
            }
            for group in [
                &["codex-cli", "codex-desktop"][..],
                if platform == "mac" {
                    &[]
                } else {
                    &["codex-desktop", "claude-desktop"]
                },
                if platform == "ubuntu" {
                    &["codex-cli", "zcode", "t3-acp-adapters"]
                } else {
                    &["codex-cli", "t3-acp-adapters"]
                },
            ] {
                assert!(group.iter().filter(|app| active.contains(*app)).count() <= 1);
            }
        }
        assert_eq!(peak, 4);
        assert_eq!(finished.len(), 12);
    }
}

#[tokio::test]
async fn production_backend_streams_the_one_app_worker_and_routes_cancel() {
    let root = temp_root();
    put(&root, "fake-python-installer-root", "");
    put(&root, "hold-fixture-worker", "");
    let python = std::env::var_os("AAC_TEST_PYTHON").unwrap_or_else(|| {
        if cfg!(windows) {
            "python".into()
        } else {
            "python3".into()
        }
    });
    let bin = std::process::Command::new(python)
        .args(["-B", "-c", "import sys; print(sys.executable)"])
        .output()
        .expect("python location");
    assert!(bin.status.success());
    let bin = Path::new(
        std::str::from_utf8(&bin.stdout)
            .expect("python path")
            .trim(),
    );
    let helper = Path::new(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/spec/agent/app-worker-fixture.py"
    ));
    let env = [
        ("AAC_APP_UPDATE_PYTHON", bin),
        ("AAC_APP_UPDATE_HELPER", helper),
        ("AAC_APP_UPDATE_STATE_DIR", root.path()),
    ];
    let platform = if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "mac"
    } else {
        "ubuntu"
    };
    let (mut child, mut lines) = spawn_agent(root.path(), platform, JOB, "update", &env).await;
    let _ = downloading(&mut lines).await;
    let (mut cancel, mut reply) = spawn_agent(root.path(), platform, JOB, "cancel", &env).await;
    assert_eq!(
        terminal(&mut cancel, &mut reply).await.1,
        json!({"cancelled":true})
    );
    let (progress, result) = terminal(&mut child, &mut lines).await;
    assert!(root.path().join("fixture-cancel-observed").is_file());
    assert!(
        progress
            .iter()
            .any(|ev| ev["result"]["appId"] == "codex-desktop"
                && ev["result"]["messageCode"] == "skipped_cancelled")
    );
    let rows = result["results"].as_array().expect("complete result");
    assert_eq!(rows.len(), 12);
    assert!(rows.iter().any(|row| row["status"] == "held"));
    assert!(rows.iter().any(|row| row["parts"][0]["inUse"] == true));
}
