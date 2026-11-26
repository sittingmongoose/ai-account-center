//! Windows Update apps through the fixed InteractiveToken task (W6). Python oracle:
//! `app_updates.py` `windows_interactive_apply`, `task_child_progress`,
//! `read_task_request` and the `--task-child` path.
//!
//! The agent reaches Windows over ssh, outside the user's desktop session, so GUI
//! and terminal restarts run in the pre-registered `CCS App Updates` task. That task
//! now starts this agent (`aac-agent --app-update-task`, under a headless console
//! host so no window opens) instead of `pythonw app_updates.py --task-child`; the
//! coordinator re-registers it when its action names anything else. The task starts
//! with fixed arguments, so the app and its budget travel in a nonce-bound request
//! file; progress, cancel and the result use nonce-bound files beside it.
use super::{
    AppRow, BoxFuture, Shared, Status, System, UpdateFailure,
    common::{CommandSpec, Diagnostics, Native, write_private_json},
    runner::{self, Cancelled, Job, Outcome, Phases},
    shared::execution_lock,
};
use crate::apps::{Host, Platform, read_json};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::time::Instant;

pub(crate) const TASK: &str = "CCS App Updates";
const REQUEST: &str = "windows-task-request.json";
const RESULT: &str = "windows-task-result.json";
const PROGRESS: &str = "windows-task-progress.json";
const CANCEL: &str = "windows-task-cancel.json";
/// Python `HOST_DEADLINE_SECONDS`: a task worker's budget never exceeds it.
const HOST_DEADLINE: u64 = 30 * 60;

/// The task-scheduler calls, so tests can stand in for PowerShell.
pub(crate) trait Scheduler: Send + Sync {
    /// Run one fixed PowerShell script: its stdout, or the failure (exit code kept).
    fn run<'a>(&'a self, script: &'a str) -> BoxFuture<'a, Result<String, UpdateFailure>>;
}

/// The real scheduler: Windows PowerShell with a 15 s bound (Python `task_command`).
pub(crate) struct PowerShell(pub PathBuf);

impl Scheduler for PowerShell {
    fn run<'a>(&'a self, script: &'a str) -> BoxFuture<'a, Result<String, UpdateFailure>> {
        Box::pin(async move {
            let spec = CommandSpec::powershell(self.0.as_os_str(), script)
                .timeout(Duration::from_secs(15))
                .quiet();
            let output = Native
                .run(&spec, &Mutex::new(Diagnostics::default()))
                .await?;
            Ok(output.stdout)
        })
    }
}

fn root(host: &Host) -> PathBuf {
    host.home.join(".ccs/app-updates")
}

/// A PowerShell single-quoted literal. Windows PowerShell also ends a literal at the
/// typographic single quotes, so those are doubled too (a profile may contain U+2019).
pub(crate) fn literal(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for character in value.chars() {
        if matches!(
            character,
            '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}'
        ) {
            quoted.push(character);
        }
        quoted.push(character);
    }
    quoted.push('\'');
    quoted
}

/// The check-and-register script: exit 3 while the task runs; otherwise make the task's
/// single action this agent under a headless console host, then print its last run ticks.
pub(crate) fn prepare_script(agent: &Path) -> String {
    format!(
        "$ErrorActionPreference='Stop'; $n='{TASK}'; $exe={exe}; \
$c=Join-Path $env:SystemRoot 'System32\\conhost.exe'; $a='--headless \"'+$exe+'\" --app-update-task'; \
$t=Get-ScheduledTask -TaskName $n -ErrorAction SilentlyContinue; \
if($t -and $t.State -eq 'Running'){{exit 3}}; \
if(-not $t -or @($t.Actions).Count -ne 1 -or $t.Actions[0].Execute -ne $c -or $t.Actions[0].Arguments -ne $a){{ \
$action=New-ScheduledTaskAction -Execute $c -Argument $a; \
$principal=New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited; \
$settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 35) -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries -MultipleInstances IgnoreNew -Hidden; \
Register-ScheduledTask -TaskName $n -Action $action -Principal $principal -Settings $settings -Force | Out-Null }}; \
(Get-ScheduledTaskInfo -TaskName $n).LastRunTime.Ticks",
        exe = literal(&agent.to_string_lossy()),
    )
}

const START: &str = "$ErrorActionPreference='Stop'; $t=Get-ScheduledTask -TaskName 'CCS App Updates'; if($t.State -eq 'Running'){exit 3}; if($t.Settings.ExecutionTimeLimit -ne 'PT35M'){$t.Settings.ExecutionTimeLimit='PT35M'; $t | Set-ScheduledTask | Out-Null}; Start-ScheduledTask -TaskName 'CCS App Updates'";
const STOP: &str = "$ErrorActionPreference='Stop'; Stop-ScheduledTask -TaskName 'CCS App Updates'";

fn finished_script(previous: u64) -> String {
    format!(
        "$ErrorActionPreference='Stop'; $i=Get-ScheduledTaskInfo -TaskName '{TASK}'; $t=Get-ScheduledTask -TaskName '{TASK}'; if($t.State -notin 'Running','Queued' -and $i.LastRunTime.Ticks -ne {previous}){{ 'finished' }}"
    )
}

/// Python `task_command`: exit 3 means the task already runs (`busy`).
async fn task_command(scheduler: &dyn Scheduler, script: &str) -> Result<String, UpdateFailure> {
    scheduler.run(script).await.map_err(|failure| {
        if failure.exit_code == Some(3) {
            UpdateFailure::new("busy")
        } else {
            failure
        }
    })
}

/// A fresh request nonce: 32 lowercase hex characters (Python `uuid4().hex`).
fn nonce() -> String {
    use sha2::Digest as _;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let marker = 0u8;
    let seed = format!(
        "{:?}{}{}{:p}",
        std::time::SystemTime::now(),
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        &marker
    );
    sha2::Sha256::digest(seed.as_bytes())
        .iter()
        .take(16)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn valid_nonce(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn failed(app: &str, code: &str) -> Outcome {
    Outcome::Row(runner::sanitize(
        AppRow::new(app, Platform::Windows, Status::Failed)
            .code(code)
            .to_value(),
    ))
}

fn row_for(app: &str, status: Status, code: &str) -> Value {
    runner::sanitize(
        AppRow::new(app, Platform::Windows, status)
            .code(code)
            .to_value(),
    )
}

/// Python `windows_interactive_apply(app=...)` for one app.
pub(crate) async fn coordinate(
    host: &Host,
    agent: &Path,
    app: &'static str,
    budget: Duration,
    cancelled: Cancelled,
    phases: Phases,
    scheduler: &dyn Scheduler,
) -> Outcome {
    let root = root(host);
    let _lock = match execution_lock(&root, "windows-coordinator.lock") {
        Ok(lock) => lock,
        Err(failure) => return failed(app, failure.code),
    };
    match coordinate_locked(&root, agent, app, budget, &cancelled, &phases, scheduler).await {
        Ok(outcome) => outcome,
        Err(failure) => failed(app, failure.code),
    }
}

async fn coordinate_locked(
    root: &Path,
    agent: &Path,
    app: &'static str,
    budget: Duration,
    cancelled: &Cancelled,
    phases: &Phases,
    scheduler: &dyn Scheduler,
) -> Result<Outcome, UpdateFailure> {
    let previous = task_command(scheduler, &prepare_script(agent))
        .await?
        .trim()
        .parse::<u64>()
        .map_err(|_| UpdateFailure::new("readiness_unknown"))?;
    let nonce = nonce();
    for stale in [PROGRESS, CANCEL] {
        let _ = std::fs::remove_file(root.join(stale));
    }
    let seconds = budget.as_secs().clamp(1, HOST_DEADLINE);
    write_private_json(
        &root.join(REQUEST),
        &json!({"nonce": nonce, "appId": app, "workerSeconds": seconds}),
    )
    .map_err(|_| UpdateFailure::failed())?;
    task_command(scheduler, START).await?;
    let started = Instant::now();
    let deadline = started + budget.min(Duration::from_secs(31 * 60));
    let mut next_check = started + Duration::from_secs(5);
    let (mut relayed, mut cancel_sent, mut cancel_deadline) = (0, false, None);
    let mut completed: Option<Value> = None;
    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        if !cancel_sent
            && cancelled()
            && write_private_json(&root.join(CANCEL), &json!({"nonce": nonce})).is_ok()
        {
            cancel_sent = true;
            cancel_deadline = Some(now + Duration::from_secs(7));
        }
        relayed = relay(root, &nonce, relayed, app, phases, &mut completed);
        if let Some(row) = final_row(root, &nonce, app) {
            return Ok(Outcome::Row(row));
        }
        if cancel_deadline.is_some_and(|limit| now >= limit) {
            // Only the coordinator that created the nonce holds the task lease.
            let _ = task_command(scheduler, STOP).await;
            return Ok(Outcome::Row(row_for(
                app,
                Status::Skipped,
                "skipped_cancelled",
            )));
        }
        if now >= next_check {
            next_check = now + Duration::from_secs(5);
            let state = task_command(scheduler, &finished_script(previous))
                .await
                .unwrap_or_default();
            if state.trim() == "finished" {
                // The final file can arrive between the read and this query: prefer it.
                if let Some(row) = final_row(root, &nonce, app) {
                    return Ok(Outcome::Row(row));
                }
                relay(root, &nonce, relayed, app, phases, &mut completed);
                return Ok(Outcome::Row(
                    completed.unwrap_or_else(|| row_for(app, Status::Unknown, "task_failed")),
                ));
            }
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    if !cancel_sent {
        let _ = write_private_json(&root.join(CANCEL), &json!({"nonce": nonce}));
    }
    Ok(match completed {
        Some(row) => Outcome::Row(row),
        None => Outcome::TimedOut,
    })
}

/// Python `relay_progress`: forward new events of this nonce; remember a result row.
fn relay(
    root: &Path,
    nonce: &str,
    relayed: usize,
    app: &'static str,
    phases: &Phases,
    completed: &mut Option<Value>,
) -> usize {
    let Some(progress) = read_json(&root.join(PROGRESS), 1024 * 1024) else {
        return relayed;
    };
    if progress["nonce"] != nonce {
        return relayed;
    }
    let Some(events) = progress["events"].as_array() else {
        return relayed;
    };
    for event in events.iter().skip(relayed) {
        if event["event"] == "result" && event["result"]["appId"] == app {
            *completed = Some(runner::sanitize(event["result"].clone()));
        } else if event["event"] == "app"
            && event["appId"] == app
            && let Some(phase @ ("checking" | "updating" | "downloading")) = event["phase"].as_str()
        {
            let phase = match phase {
                "checking" => "checking",
                "updating" => "updating",
                _ => "downloading",
            };
            let _ = phases.send((app, phase));
        }
    }
    relayed.max(events.len())
}

/// Python `final_result`: the task's row for this nonce and app, or `None`.
fn final_row(root: &Path, nonce: &str, app: &str) -> Option<Value> {
    let result = read_json(&root.join(RESULT), 1024 * 1024)?;
    if result["nonce"] != nonce {
        return None;
    }
    let rows = result["results"].as_array()?;
    let row = rows.first().filter(|_| rows.len() == 1)?;
    (row["appId"] == app && row["platform"] == "windows" && row["status"].is_string())
        .then(|| runner::sanitize(row.clone()))
}

/// One validated task request (Python `read_task_request` plus the app and budget checks).
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Request {
    pub nonce: String,
    pub app: &'static str,
    pub seconds: u64,
}

pub(crate) fn read_request(root: &Path) -> Option<Request> {
    let request = read_json(&root.join(REQUEST), 64 * 1024)?;
    let nonce = request["nonce"]
        .as_str()
        .filter(|value| valid_nonce(value))?;
    let app = request["appId"].as_str()?;
    let app = aac_providers::APP_INVENTORY
        .iter()
        .copied()
        .find(|known| *known == app)?;
    let seconds = request["workerSeconds"]
        .as_u64()
        .filter(|seconds| (1..=HOST_DEADLINE).contains(seconds))?;
    Some(Request {
        nonce: nonce.to_owned(),
        app,
        seconds,
    })
}

/// `aac-agent --app-update-task`: the task child. It runs one requested app in the
/// user's desktop session, writing nonce-bound progress and the final row.
pub(crate) async fn child() -> i32 {
    let Some(host) = Host::native(Platform::Windows).filter(|_| cfg!(windows)) else {
        return 2;
    };
    child_in(Arc::new(host)).await
}

pub(crate) async fn child_in(host: Arc<Host>) -> i32 {
    let root = root(&host);
    let Some(request) = read_request(&root) else {
        return 2;
    };
    let cancel_file = root.join(CANCEL);
    let nonce = request.nonce.clone();
    let cancelled: Cancelled = Arc::new(move || {
        read_json(&cancel_file, 4096).is_some_and(|value| value["nonce"] == nonce.as_str())
    });
    let (phases, mut events) = tokio::sync::mpsc::unbounded_channel();
    let job = Job {
        host,
        state: &root,
        platform: Platform::Windows,
        shared: Arc::new(Shared::default()),
        deadline: Instant::now() + Duration::from_secs(request.seconds),
        t3_row: None,
        cancelled,
    };
    let running = runner::run_app(request.app, job, phases);
    tokio::pin!(running);
    let mut written: Vec<Value> = Vec::new();
    let progress = root.join(PROGRESS);
    let outcome = loop {
        tokio::select! {
            outcome = &mut running => break outcome,
            Some((app, phase)) = events.recv() => {
                written.push(json!({"event": "app", "appId": app, "phase": phase}));
                let _ = write_private_json(&progress, &json!({"nonce": request.nonce, "events": written}));
            }
        }
    };
    let row = match outcome {
        Outcome::Row(row) => row,
        Outcome::TimedOut => row_for(request.app, Status::Unknown, "host_timeout"),
    };
    let _ = write_private_json(
        &root.join(RESULT),
        &json!({"nonce": request.nonce, "results": [row]}),
    );
    0
}

#[cfg(test)]
#[path = "windows_task_tests.rs"]
mod tests;
