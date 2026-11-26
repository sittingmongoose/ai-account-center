//! The Windows task coordinator and child (Python `windows_interactive_apply`,
//! `read_task_request`, `task_child_progress`), with the scheduler faked: no task
//! is registered, started or stopped on the machine running the tests.
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Answers each script by its role; `START` plays the task child by writing files.
struct FakeScheduler {
    root: PathBuf,
    scripts: Mutex<Vec<String>>,
    child: fn(&Path),
    busy: bool,
    polls: AtomicUsize,
}

impl Scheduler for FakeScheduler {
    fn run<'a>(&'a self, script: &'a str) -> BoxFuture<'a, Result<String, UpdateFailure>> {
        Box::pin(async move {
            self.scripts
                .lock()
                .expect("scripts")
                .push(script.to_owned());
            if script.contains("--app-update-task") {
                if self.busy {
                    return Err(UpdateFailure {
                        exit_code: Some(3),
                        ..UpdateFailure::failed()
                    });
                }
                return Ok("638000000000000000\r\n".into());
            }
            if script.contains("Start-ScheduledTask") {
                (self.child)(&self.root);
                return Ok(String::new());
            }
            if script.contains("'finished'") {
                self.polls.fetch_add(1, Ordering::SeqCst);
                return Ok("finished".into());
            }
            Ok(String::new())
        })
    }
}

fn host(root: &Path) -> Arc<Host> {
    crate::apps::updater::tests::host(root, Platform::Windows)
}

fn scheduler(root: &Path, child: fn(&Path), busy: bool) -> FakeScheduler {
    FakeScheduler {
        root: root.join("home/.ccs/app-updates"),
        scripts: Mutex::new(Vec::new()),
        child,
        busy,
        polls: AtomicUsize::new(0),
    }
}

fn request(root: &Path) -> Value {
    read_json(&root.join(REQUEST), 4096).expect("request")
}

fn answering_child(root: &Path) {
    let asked = request(root);
    let nonce = asked["nonce"].clone();
    write_private_json(
        &root.join(PROGRESS),
        &json!({"nonce": nonce, "events": [
            {"event": "app", "appId": "omp", "phase": "checking"},
            {"event": "app", "appId": "omp", "phase": "updating"},
            {"event": "app", "appId": "zcode", "phase": "updating"},
        ]}),
    )
    .expect("progress");
    write_private_json(
        &root.join(RESULT),
        &json!({"nonce": nonce, "results": [{
            "appId": "omp", "platform": "windows", "status": "updated", "previousVersion": "1.0.0",
            "version": "1.1.0", "manager": "native", "messageCode": "updated",
            "updateAttempted": true, "restartedProcesses": 0, "output": "never kept"
        }]}),
    )
    .expect("result");
}

fn silent_child(_: &Path) {}

#[test]
fn literals_double_every_single_quote_powershell_honours() {
    assert_eq!(literal("C:\\Users\\Jo's"), "'C:\\Users\\Jo''s'");
    assert_eq!(
        literal("C:\\Users\\Jo\u{2019}s"),
        "'C:\\Users\\Jo\u{2019}\u{2019}s'"
    );
}

#[test]
fn the_task_action_is_this_agent_under_a_headless_console_host() {
    let script = prepare_script(Path::new("C:\\AAC\\aac-agent.exe"));
    assert!(script.contains("$exe='C:\\AAC\\aac-agent.exe'"));
    assert!(script.contains("System32\\conhost.exe"));
    assert!(script.contains("'--headless \"'+$exe+'\" --app-update-task'"));
    assert!(script.contains("-LogonType Interactive -RunLevel Limited"));
    assert!(script.contains("if($t -and $t.State -eq 'Running'){exit 3}"));
    assert!(!script.contains("python"));
}

#[test]
fn a_task_request_needs_a_nonce_a_known_app_and_a_bounded_budget() {
    let root = tempfile::tempdir().expect("root");
    let write =
        |value: Value| write_private_json(&root.path().join(REQUEST), &value).expect("request");
    let nonce = "0123456789abcdef0123456789abcdef";
    write(json!({"nonce": nonce, "appId": "omp", "workerSeconds": 600}));
    assert_eq!(
        read_request(root.path()),
        Some(Request {
            nonce: nonce.into(),
            app: "omp",
            seconds: 600
        })
    );
    for invalid in [
        json!({"nonce": "XYZ", "appId": "omp", "workerSeconds": 600}),
        json!({"nonce": nonce, "appId": "not-an-app", "workerSeconds": 600}),
        json!({"nonce": nonce, "appId": "omp", "workerSeconds": 0}),
        json!({"nonce": nonce, "appId": "omp", "workerSeconds": 1801}),
        json!({"nonce": nonce, "workerSeconds": 600}),
    ] {
        write(invalid.clone());
        assert_eq!(read_request(root.path()), None, "{invalid}");
    }
}

#[tokio::test]
async fn the_coordinator_relays_this_apps_phases_and_returns_the_tasks_row() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path());
    let fake = scheduler(root.path(), answering_child, false);
    let (phases, mut events) = tokio::sync::mpsc::unbounded_channel();
    let outcome = coordinate(
        &host,
        Path::new("C:\\AAC\\aac-agent.exe"),
        "omp",
        Duration::from_secs(120),
        Arc::new(|| false),
        phases,
        &fake,
    )
    .await;
    let Outcome::Row(row) = outcome else {
        panic!("a row");
    };
    assert_eq!(row["status"], "updated");
    assert!(row.get("output").is_none(), "the U13 whitelist applies");
    let mut seen = Vec::new();
    while let Ok(event) = events.try_recv() {
        seen.push(event);
    }
    assert_eq!(seen, [("omp", "checking"), ("omp", "updating")]);
    let asked = request(&root.path().join("home/.ccs/app-updates"));
    assert_eq!(asked["appId"], "omp");
    assert_eq!(asked["workerSeconds"], 120);
    assert!(valid_nonce(asked["nonce"].as_str().expect("nonce")));
}

#[tokio::test]
async fn a_running_task_is_busy_and_nothing_starts() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path());
    let fake = scheduler(root.path(), answering_child, true);
    let (phases, _events) = tokio::sync::mpsc::unbounded_channel();
    let Outcome::Row(row) = coordinate(
        &host,
        Path::new("C:\\a.exe"),
        "omp",
        Duration::from_secs(60),
        Arc::new(|| false),
        phases,
        &fake,
    )
    .await
    else {
        panic!("a row");
    };
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("failed"), Some("busy"))
    );
    assert!(
        !fake
            .scripts
            .lock()
            .expect("scripts")
            .iter()
            .any(|script| script.contains("Start-ScheduledTask"))
    );
}

#[tokio::test(start_paused = true)]
async fn a_task_that_finished_without_a_row_is_task_failed() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path());
    let fake = scheduler(root.path(), silent_child, false);
    let (phases, _events) = tokio::sync::mpsc::unbounded_channel();
    let Outcome::Row(row) = coordinate(
        &host,
        Path::new("C:\\a.exe"),
        "omp",
        Duration::from_secs(600),
        Arc::new(|| false),
        phases,
        &fake,
    )
    .await
    else {
        panic!("a row");
    };
    assert_eq!(
        (row["status"].as_str(), row["messageCode"].as_str()),
        (Some("unknown"), Some("task_failed"))
    );
    assert_eq!(fake.polls.load(Ordering::SeqCst), 1);
}

#[tokio::test(start_paused = true)]
async fn a_cancel_is_forwarded_then_the_task_is_stopped_after_seven_seconds() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path());
    let fake = scheduler(root.path(), silent_child, false);
    // Never report finished: the cancel deadline must decide.
    struct Waiting(FakeScheduler);
    impl Scheduler for Waiting {
        fn run<'a>(&'a self, script: &'a str) -> BoxFuture<'a, Result<String, UpdateFailure>> {
            if script.contains("'finished'") {
                return Box::pin(async { Ok(String::new()) });
            }
            self.0.run(script)
        }
    }
    let waiting = Waiting(fake);
    let (phases, _events) = tokio::sync::mpsc::unbounded_channel();
    let Outcome::Row(row) = coordinate(
        &host,
        Path::new("C:\\a.exe"),
        "omp",
        Duration::from_secs(600),
        Arc::new(|| true),
        phases,
        &waiting,
    )
    .await
    else {
        panic!("a row");
    };
    assert_eq!(row["messageCode"], "skipped_cancelled");
    let root = root.path().join("home/.ccs/app-updates");
    assert_eq!(
        read_json(&root.join(CANCEL), 4096).expect("cancel")["nonce"],
        request(&root)["nonce"]
    );
    assert!(
        waiting
            .0
            .scripts
            .lock()
            .expect("scripts")
            .iter()
            .any(|script| script.contains("Stop-ScheduledTask"))
    );
}

#[tokio::test]
async fn the_child_runs_the_requested_app_and_writes_its_nonce_bound_row() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path());
    let state = root.path().join("home/.ccs/app-updates");
    let nonce = "fedcba9876543210fedcba9876543210";
    write_private_json(
        &state.join(REQUEST),
        &json!({"nonce": nonce, "appId": "omp", "workerSeconds": 60}),
    )
    .expect("request");
    assert_eq!(child_in(host).await, 0);
    let result = read_json(&state.join(RESULT), 65_536).expect("result");
    assert_eq!(result["nonce"], nonce);
    assert_eq!(result["results"][0]["appId"], "omp");
    assert_eq!(result["results"][0]["status"], "not_installed");
}

#[tokio::test]
async fn the_child_refuses_a_request_without_a_valid_nonce() {
    let root = tempfile::tempdir().expect("root");
    let host = host(root.path());
    let state = root.path().join("home/.ccs/app-updates");
    write_private_json(
        &state.join(REQUEST),
        &json!({"nonce": "bad", "appId": "omp", "workerSeconds": 60}),
    )
    .expect("request");
    assert_eq!(child_in(host).await, 2);
    assert!(!state.join(RESULT).exists());
}
