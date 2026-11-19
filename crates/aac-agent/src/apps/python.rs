//! The fixed one-app Python adapter. The coordinator retains admission, scheduling and journaling.
use super::*;
use std::future::Future;
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::Command,
};

const APP_FRAME: usize = 65_536;

#[derive(Clone)]
pub(super) struct Python {
    binary: PathBuf,
    helper: PathBuf,
}

impl Python {
    pub(super) fn resolve(host: &Host) -> Option<Self> {
        let helper = if let Some(configured) = std::env::var_os("AAC_APP_UPDATE_HELPER") {
            PathBuf::from(configured)
        } else if let Some(package) = std::env::var_os("AAC_AGENT_PACKAGE_ROOT") {
            let packaged = PathBuf::from(package).join("scripts/app-updates/app_updates.py");
            if packaged.is_file() {
                packaged
            } else {
                host.home.join(".ccs/app-updates/app_updates.py")
            }
        } else {
            host.home.join(".ccs/app-updates/app_updates.py")
        };
        let binary = if let Some(configured) = std::env::var_os("AAC_APP_UPDATE_PYTHON") {
            PathBuf::from(configured)
        } else if host.platform == Platform::Windows {
            host.which("python")?
        } else {
            host.system("/usr/bin/python3")
        };
        (binary.is_absolute() && binary.is_file() && helper.is_absolute() && helper.is_file())
            .then_some(Self { binary, helper })
    }

    fn command(&self, platform: Platform) -> Command {
        let mut command = Command::new(&self.binary);
        command
            .args(["-B", "-u"])
            .arg(&self.helper)
            .args(["--platform", platform.name()])
            .kill_on_drop(true)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .env("PYTHONUTF8", "1")
            .env("PYTHONIOENCODING", "utf-8")
            .env("AAC_UPDATE_PROGRESS", "1")
            .env("MUSE_NO_AUTO_UPDATE", "1")
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
            .env("DISABLE_AUTOUPDATER", "1");
        strip_dashboard_secrets(&mut command);
        #[cfg(windows)]
        command.creation_flags(0x08000000);
        command
    }

    pub(super) async fn compatible(&self, platform: Platform) -> bool {
        let Ok(mut child) = self
            .command(platform)
            .arg("--agent-capabilities")
            .stdin(Stdio::null())
            .spawn()
        else {
            return false;
        };
        let Some(stdout) = child.stdout.take() else {
            return false;
        };
        let probe = async {
            let mut reader = BufReader::new(stdout);
            let line = crate::read_line(&mut reader, 256)
                .await?
                .ok_or_else(invalid)?;
            let status = child.wait().await?;
            Ok::<_, Failure>(
                status.success()
                    && serde_json::from_slice::<Value>(&line).ok() == Some(json!({"appWorker":1})),
            )
        };
        matches!(
            tokio::time::timeout(Duration::from_secs(5), probe).await,
            Ok(Ok(true))
        )
    }

    pub(super) async fn install<F: Future<Output = ()>>(
        &self,
        app: &str,
        platform: Platform,
        state: &Path,
        job: &str,
        budget: Duration,
        t3_restart_pending: bool,
        cancelled: impl Fn() -> Result<bool, Failure>,
        mut progress: impl FnMut(String) -> F,
    ) -> Result<Value, Failure> {
        let seconds = budget.as_secs().clamp(1, 1800);
        let mut command = self.command(platform);
        command.kill_on_drop(false);
        command
            .args([
                "--apply",
                "--agent-managed",
                "--app",
                app,
                "--worker-seconds",
                &seconds.to_string(),
            ])
            .env("AAC_UPDATE_STATE_DIR", state)
            .env("AAC_UPDATE_AGENT_JOB", job)
            .stdin(Stdio::piped());
        if t3_restart_pending && app == "antigravity-acp" {
            command.arg("--t3-restart-pending");
        }
        let mut child = command.spawn()?;
        let mut control = child.stdin.take();
        let mut stream = BufReader::new(child.stdout.take().ok_or_else(invalid)?);
        let (frames, mut replies) = tokio::sync::mpsc::channel(4);
        let reader = tokio::spawn(async move {
            loop {
                // This read lives outside select: dropping a partially read line would
                // otherwise lose its prefix when the cancel timer wins.
                let line = crate::read_line(&mut stream, APP_FRAME).await;
                let last = !matches!(line, Ok(Some(_)));
                if frames.send(line).await.is_err() || last {
                    break;
                }
            }
        });
        let result = async {
            let mut final_row = None;
            let hard_deadline = tokio::time::Instant::now() + budget;
            let mut drain_until = hard_deadline;
            let mut tick = tokio::time::interval(Duration::from_millis(150));
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(drain_until) => return Err(Failure::new(Cause::Timeout, "app installer deadline")),
                    _ = tick.tick(), if control.is_some() => {
                        if cancelled()? {
                            if let Some(mut stdin) = control.take() {
                                let _ = stdin.write_all(b"cancel\n").await;
                            }
                            drain_until = hard_deadline.min(tokio::time::Instant::now() + Duration::from_secs(10));
                        }
                    }
                    message = replies.recv() => {
                        let Some(line) = message.transpose()?.flatten() else { break };
                        let event: Value = serde_json::from_slice(&line).map_err(|_| invalid())?;
                        if final_row.is_some() {
                            return Err(invalid());
                        }
                        if event["event"] == "app" && event["appId"] == app {
                            if let Some(phase @ ("checking" | "updating" | "downloading")) = event["phase"].as_str() {
                                progress(phase.to_owned()).await;
                            }
                        } else if let Some(rows) = event["results"].as_array().filter(|rows| rows.len() == 1) {
                            let row = &rows[0];
                            if row["appId"] != app || row["platform"] != platform.name() || !row["status"].is_string() {
                                return Err(invalid());
                            }
                            final_row = Some(row.clone());
                        } else {
                            return Err(invalid());
                        }
                    }
                }
            }
            if !tokio::time::timeout_at(drain_until, child.wait()).await.map_err(|_| invalid())??.success() {
                return Err(invalid());
            }
            final_row.ok_or_else(invalid)
        }.await;
        if result.is_err() {
            // A closed pipe asks the supervisor to kill its worker tree even when
            // the transport deadline expired. Give it bounded time to reap.
            drop(control);
            let _ = tokio::time::timeout(Duration::from_secs(8), child.wait()).await;
        }
        reader.abort();
        result
    }
}

fn invalid() -> Failure {
    Failure::new(Cause::Damaged, "invalid app installer reply")
}
