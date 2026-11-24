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
    spawn_request(root, platform, job, action, None, extra).await
}

async fn spawn_scoped(
    root: &Path,
    platform: &str,
    job: &str,
    action: &str,
    apps: &[&str],
) -> (Child, LinesFromAgent) {
    spawn_request(root, platform, job, action, Some(apps), &[]).await
}

async fn spawn_request(
    root: &Path,
    platform: &str,
    job: &str,
    action: &str,
    apps: Option<&[&str]>,
    extra: &[(&str, &Path)],
) -> (Child, LinesFromAgent) {
    let mut args = json!({"action":action,"platform":platform,"jobId":job});
    if let Some(apps) = apps {
        args["apps"] = json!(apps);
    }
    spawn_raw(root, args, action, extra).await
}

async fn spawn_raw(
    root: &Path,
    args: Value,
    action: &str,
    extra: &[(&str, &Path)],
) -> (Child, LinesFromAgent) {
    let job = args["jobId"].as_str().expect("fixture job id");
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
        "protocol":1,"verb":"apps","id":job,"deadlineMs":15000,"args":args
    });
    stdin
        .write_all(format!("{request}\n").as_bytes())
        .await
        .expect("send request");
    if matches!(action, "prepare-update" | "prepare-acp-follow-up") {
        child.stdin = Some(stdin);
    } else {
        drop(stdin);
    }
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
async fn without_apps_keeps_the_full_sweep() {
    for platform in ["ubuntu", "mac", "windows"] {
        let root = root();
        let (mut child, mut lines) = agent(root.path(), platform, JOB, "update").await;
        let (events, payload) = terminal(&mut child, &mut lines).await;
        compare(events, &payload, platform, "update");
        assert_eq!(payload["results"].as_array().expect("rows").len(), 12);
        let journal: Value = serde_json::from_str(
            &std::fs::read_to_string(root.path().join("fixture-journal.json"))
                .expect("fixture journal"),
        )
        .expect("journal JSON");
        assert!(
            journal.get("apps").is_none(),
            "{platform}: an unscoped request keeps the full sweep"
        );
    }
}

#[tokio::test]
async fn a_requested_subset_runs_only_those_apps() {
    let root = root();
    put(&root, "fixture-install-all", "");
    let (mut child, mut lines) = spawn_scoped(
        root.path(),
        "ubuntu",
        JOB,
        "update",
        &["zcode", "antigravity-cli"],
    )
    .await;
    let (events, payload) = terminal(&mut child, &mut lines).await;
    let ids: Vec<_> = payload["results"]
        .as_array()
        .expect("rows")
        .iter()
        .map(|row| row["appId"].as_str().expect("app id").to_owned())
        .collect();
    assert_eq!(ids, ["antigravity-cli", "zcode"]);
    for event in &events {
        let app = event["appId"]
            .as_str()
            .or_else(|| event["result"]["appId"].as_str());
        if let Some(app) = app {
            assert!(
                ["antigravity-cli", "zcode"].contains(&app),
                "{app} ran outside the requested scope"
            );
        }
    }
    let journal: Value = serde_json::from_str(
        &std::fs::read_to_string(root.path().join("fixture-journal.json"))
            .expect("fixture journal"),
    )
    .expect("journal JSON");
    assert_eq!(journal["apps"], json!(["antigravity-cli", "zcode"]));
    for app in ["antigravity-cli", "zcode"] {
        assert!(
            root.path()
                .join(format!("fixture-installed-{app}.txt"))
                .is_file(),
            "{app} ran"
        );
    }
    assert!(!root.path().join("fixture-installed-omp.txt").exists());
}

#[tokio::test]
async fn a_subset_cancel_covers_its_own_apps_only() {
    let root = root();
    put(&root, "fixture-hold-download", "");
    let (mut child, mut lines) = spawn_scoped(
        root.path(),
        "ubuntu",
        JOB,
        "update",
        &["codex-desktop", "t3-code"],
    )
    .await;
    let _ = downloading(&mut lines).await;
    assert_eq!(
        other_call(&root, "ubuntu", JOB, "cancel").await,
        json!({"cancelled":true})
    );
    put(&root, "fixture-release-download", "");
    let (_, payload) = terminal(&mut child, &mut lines).await;
    let rows = payload["results"].as_array().expect("rows");
    assert_eq!(rows.len(), 2);
    assert_eq!(
        rows.iter()
            .find(|row| row["appId"] == "codex-desktop")
            .expect("desktop row")["status"],
        "updated"
    );
    assert_eq!(
        rows.iter()
            .find(|row| row["appId"] == "t3-code")
            .expect("t3-code row")["messageCode"],
        "skipped_cancelled"
    );
    assert_eq!(
        other_call(&root, "ubuntu", JOB, "cancel").await,
        json!({"cancelled":false})
    );
}

#[tokio::test]
async fn a_malformed_scope_is_refused_before_any_work() {
    for apps in [
        json!([]),
        json!(["omp", "omp"]),
        json!(["not-an-app"]),
        json!(["omp", 8]),
    ] {
        let root = root();
        let args = json!({"action":"update","platform":"ubuntu","jobId":JOB,"apps":apps});
        let (mut child, mut lines) = spawn_raw(root.path(), args, "update", &[]).await;
        assert!(matches!(
            event(&mut lines).await,
            Event::Failure { failure, .. } if failure.cause == aac_core::Cause::Refused
        ));
        assert!(!child.wait().await.expect("reap refusing agent").success());
        assert!(!root.path().join("fixture-journal.json").exists());
    }
    let root = root();
    let args = json!({"action":"cancel","platform":"ubuntu","jobId":JOB,"apps":["omp"]});
    let (mut child, mut lines) = spawn_raw(root.path(), args, "cancel", &[]).await;
    assert!(matches!(
        event(&mut lines).await,
        Event::Failure { failure, .. } if failure.cause == aac_core::Cause::Refused
    ));
    assert!(!child.wait().await.expect("reap refusing agent").success());
}

#[tokio::test]
async fn windows_update_waits_for_host_admission() {
    let root = root();
    let (mut child, mut lines) = agent(root.path(), "windows", JOB, "prepare-update").await;
    let ready = event(&mut lines).await;
    assert!(
        matches!(ready, Event::Progress { data, .. } if data == json!({"event":"admission"})),
        "expected the host to ask for admission"
    );
    assert!(!root.path().join("fixture-journal.json").exists());
    drop(child.stdin.take());
    assert!(matches!(event(&mut lines).await,
        Event::Failure { failure, .. } if failure.cause == aac_core::Cause::Cancelled));
    assert!(!child.wait().await.expect("reap cancelled agent").success());
    assert!(!root.path().join("fixture-journal.json").exists());

    let (mut child, mut lines) = agent(root.path(), "windows", JOB, "prepare-update").await;
    assert!(matches!(event(&mut lines).await,
        Event::Progress { data, .. } if data == json!({"event":"admission"})));
    child
        .stdin
        .take()
        .expect("agent stdin")
        .write_all(b"{\"admit\":true}\n")
        .await
        .expect("grant admission");
    let (events, payload) = terminal(&mut child, &mut lines).await;
    compare(events, &payload, "windows", "update");
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
    let mut seen = downloading(&mut lines).await;
    // Let the independent rows under test settle before cancelling the held download.
    while !["antigravity-cli", "t3-acp-adapters"]
        .iter()
        .all(|app| seen.iter().any(|data| data["result"]["appId"] == *app))
    {
        let progress = event(&mut lines).await;
        let Event::Progress { data, .. } = progress else {
            panic!("expected the independent app rows, got {progress:?}");
        };
        seen.push(data);
    }
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
    let held = rows
        .iter()
        .find(|row| row["status"] == "held")
        .expect("held row");
    assert_eq!(held["diagnostics"], json!({"cause":"none"}));
    let journal =
        std::fs::read_to_string(root.path().join("agent-journal.json")).expect("host journal");
    assert!(!journal.contains("fixture leak sampler") && !journal.contains("fixture command"));
    assert!(rows.iter().any(|row| row["parts"][0]["inUse"] == true));

    if platform == "ubuntu" {
        acp_follow_up_uses_the_completed_linux_job(&root, &env).await;
    }
}

async fn acp_follow_up_uses_the_completed_linux_job(root: &TempDir, env: &[(&str, &Path)]) {
    std::fs::remove_file(root.path().join("hold-fixture-worker")).expect("release installer");
    for (job, installed, expected) in [
        (
            RETRY,
            "0.1.12-nightly.20261120.4994",
            "0.1.12-nightly.20261120.4994",
        ),
        ("110ecb7d47d24221a58111c31af6c828", "invalid target", "any"),
    ] {
        put(root, "fixture-t3-version", installed);
        let (mut child, mut lines) = spawn_agent(root.path(), "ubuntu", job, "update", env).await;
        let (_, payload) = terminal(&mut child, &mut lines).await;
        assert!(
            payload["results"]
                .as_array()
                .expect("initial rows")
                .iter()
                .any(|row| {
                    row["appId"] == "antigravity-acp" && row["messageCode"] == "t3_restart_pending"
                })
        );

        let (mut child, mut lines) =
            spawn_agent(root.path(), "ubuntu", job, "prepare-acp-follow-up", env).await;
        assert!(
            matches!(event(&mut lines).await, Event::Progress { data, .. }
            if data["event"] == aac_protocol::APP_UPDATE_ADMISSION)
        );
        child
            .stdin
            .as_mut()
            .expect("agent stdin")
            .write_all(format!("{}\n", aac_protocol::APP_UPDATE_ADMIT).as_bytes())
            .await
            .expect("admit follow-up");
        let (_, payload) = terminal(&mut child, &mut lines).await;
        assert_eq!(
            payload["results"].as_array().expect("follow-up rows").len(),
            1
        );
        assert_eq!(
            payload["results"][0]["messageCode"],
            "antigravity_acp_updated"
        );
        assert_eq!(
            std::fs::read_to_string(root.path().join("fixture-acp-expected")).expect("version"),
            expected
        );
    }
}
