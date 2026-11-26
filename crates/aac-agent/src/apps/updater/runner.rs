//! One app's run inside the agent (W6), replacing the Python worker and its
//! supervisor (`app_updates.py` `supervised_app`, `apply_single_app`, the
//! `agent-worker-<app>.lock` and `worker_stage`). The coordinator in
//! `apps/update.rs` keeps admission, scheduling, journaling and progress framing.
use super::{
    AppRow, Context, Shared, Stage, Status, attempt, common::Native, shared::execution_lock,
    updater,
};
use crate::apps::{Host, Platform};
use serde_json::{Map, Value};
use std::{
    path::Path,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{sync::mpsc::UnboundedSender, time::Instant};

/// Phases the dashboard shows, as `(app, phase)`.
pub(crate) type Phases = UnboundedSender<(&'static str, &'static str)>;
/// The job's cancel check (the coordinator's cancel file).
pub(crate) type Cancelled = Arc<dyn Fn() -> bool + Send + Sync>;

/// After the deadline a step that respects it gets this long to return its own row.
const GRACE: Duration = Duration::from_secs(2);

/// The inputs of one app run.
pub(crate) struct Job<'a> {
    pub host: Arc<Host>,
    /// The host's app-update state folder (`AAC_UPDATE_STATE_DIR` or `~/.ccs/app-updates`).
    pub state: &'a Path,
    pub platform: Platform,
    pub shared: Arc<Shared>,
    /// When the app must stop starting steps (Python's worker `deadline`).
    pub deadline: Instant,
    /// The T3 row this job already produced (Antigravity ACP reads it).
    pub t3_row: Option<Value>,
    pub cancelled: Cancelled,
}

/// The end of one run: a wire row, or the hard deadline passed (`unknown`/`host_timeout`).
#[derive(Debug, PartialEq)]
pub(crate) enum Outcome {
    Row(Value),
    TimedOut,
}

fn row(app: &str, platform: Platform, status: Status, code: &str) -> Outcome {
    Outcome::Row(sanitize(
        AppRow::new(app, platform, status).code(code).to_value(),
    ))
}

impl Job<'_> {
    fn context(
        self,
        stage: &Stage,
        phases: impl Fn(&'static str) + Send + Sync + 'static,
    ) -> Context {
        let cancelled = self.cancelled;
        Context::new(
            self.platform,
            self.host,
            Arc::new(Native),
            self.shared,
            self.deadline,
            stage.path().to_path_buf(),
            self.state.to_path_buf(),
            self.t3_row,
            move || cancelled(),
            phases,
        )
    }
}

/// Python `supervised_app` + `apply_single_app`: the app's own lock and stage, then
/// `attempt`. A cancel before the row is known stops the run (`skipped_cancelled`),
/// and dropping the attempt kills its commands.
pub(crate) async fn run_app(app: &'static str, job: Job<'_>, phases: Phases) -> Outcome {
    let platform = job.platform;
    let Some(updater) = updater(app) else {
        return row(app, platform, Status::Failed, "update_failed");
    };
    let _lock = match execution_lock(job.state, &format!("agent-worker-{app}.lock")) {
        Ok(lock) => lock,
        Err(failure) => return row(app, platform, Status::Failed, failure.code),
    };
    let stage = match Stage::open(job.state, app) {
        Ok(stage) => stage,
        Err(failure) => return row(app, platform, Status::Failed, failure.code),
    };
    let hard = job.deadline + GRACE;
    let cancelled = job.cancelled.clone();
    let cx = job.context(&stage, move |phase| {
        if matches!(phase, "checking" | "updating" | "downloading") {
            let _ = phases.send((app, phase));
        }
    });
    let running = attempt(updater, &cx);
    tokio::pin!(running);
    let mut tick = tokio::time::interval(Duration::from_millis(150));
    loop {
        tokio::select! {
            biased;
            answer = &mut running => return Outcome::Row(sanitize(answer.to_value())),
            () = tokio::time::sleep_until(hard) => return Outcome::TimedOut,
            _ = tick.tick() => {
                if cancelled() {
                    return row(app, platform, Status::Skipped, "skipped_cancelled");
                }
            }
        }
    }
}

/// Python's supervised `--acp-after-t3-restart`: Antigravity ACP against the T3 server
/// the deferred restart left running. W4 owns the follow-up itself; it polls the cancel
/// check until its guarded install starts, so a cancel never cuts that install.
pub(crate) async fn follow_up(job: Job<'_>, expected: Option<&str>) -> Outcome {
    let app = "antigravity-acp";
    let platform = job.platform;
    let _lock = match execution_lock(job.state, &format!("agent-worker-{app}.lock")) {
        Ok(lock) => lock,
        Err(failure) => return row(app, platform, Status::Failed, failure.code),
    };
    let stage = match Stage::open(job.state, app) {
        Ok(stage) => stage,
        Err(failure) => return row(app, platform, Status::Failed, failure.code),
    };
    let hard = job.deadline + GRACE;
    let cx = job.context(&stage, |_| {});
    tokio::select! {
        answer = acp_follow_up(&cx, expected) => Outcome::Row(sanitize(answer.to_value())),
        () = tokio::time::sleep_until(hard) => Outcome::TimedOut,
    }
}

/// W4's `antigravity::follow_up(cx, expected)` (lead decision). Until it lands the
/// follow-up row is an honest unknown and nothing is installed.
async fn acp_follow_up(cx: &Context, expected: Option<&str>) -> AppRow {
    let _ = expected;
    cx.record(|record| {
        record.blame(super::FailureCause::Unknown, Some("acp_follow_up"));
    });
    let mut row =
        AppRow::new("antigravity-acp", cx.platform, Status::Unknown).code("readiness_unknown");
    let value = row.to_value();
    row.diagnostics = Some(cx.record(|record| record.export(&value)));
    row
}

/// The row fields and diagnostics keys a worker row may carry (U13), with every
/// free-text diagnostics value scrubbed of token-like strings (defense in depth:
/// rows carry no command output, only fixed words and home-relative paths).
pub(crate) fn sanitize(mut row: Value) -> Value {
    if let Some(fields) = row.as_object_mut() {
        fields.retain(|key, _| {
            matches!(
                key.as_str(),
                "appId"
                    | "platform"
                    | "status"
                    | "previousVersion"
                    | "version"
                    | "manager"
                    | "messageCode"
                    | "updateAttempted"
                    | "restartedProcesses"
                    | "forcedStops"
                    | "heldVersion"
                    | "restartTargets"
                    | "parts"
                    | "strays"
                    | "quotaReset"
                    | "diagnostics"
            )
        });
    }
    if let Some(diagnostics) = row.get_mut("diagnostics").and_then(Value::as_object_mut) {
        diagnostics.retain(|key, _| {
            matches!(
                key.as_str(),
                "cause" | "tool" | "reason" | "exitCode" | "phases" | "notes" | "owner"
            )
        });
        scrub_strings(diagnostics);
    }
    row
}

fn scrub_strings(fields: &mut Map<String, Value>) {
    for value in fields.values_mut() {
        scrub_value(value);
    }
}

fn scrub_value(value: &mut Value) {
    match value {
        Value::String(text) => *text = scrub(text),
        Value::Array(items) => items.iter_mut().for_each(scrub_value),
        Value::Object(fields) => scrub_strings(fields),
        _ => {}
    }
}

/// The token patterns of Python `scrub` (`app_update_common._SECRET_PATTERNS`), minimal:
/// bearer/basic values, credential `key=value` tails, JWTs, well-known key prefixes,
/// AWS key ids, URL user info and query strings, email addresses, long hex and long
/// base64-like runs. The workspace regex has no look-around and no Unicode classes.
static SECRETS: LazyLock<Vec<(regex::Regex, &'static str)>> = LazyLock::new(|| {
    let word = r"(?:token|secret|password|passwd|passphrase|pwd|apikey|api[_-]key|authorization|credential|session[_-]?id|bearer|access[_-]?key|private[_-]?key|client[_-]?secret|cookie)";
    [
        (
            r"(?im)((?:set-)?cookie|(?:proxy-)?authorization|x-api-key|x-auth-token|private-token)[ \t]*:.{0,600}$".to_owned(),
            "${1}: <redacted>",
        ),
        (r"(?im)^.{0,600}PRIVATE KEY.{0,600}$".to_owned(), "<redacted>"),
        (
            r"(?i)(bearer|basic)[ \t]+[A-Za-z0-9._~+/=-]{1,600}".to_owned(),
            "${1} <redacted>",
        ),
        (
            format!(
                r#"(?im)(["']?[a-z0-9_.-]{{0,40}}{word}[a-z0-9_.-]{{0,40}}["']?[ \t]*[:=][ \t]*)[^ \t\r\n].{{0,600}}$"#
            ),
            "${1}<redacted>",
        ),
        (
            r"eyJ[A-Za-z0-9_-]{8,600}\.[A-Za-z0-9_-]{8,600}(?:\.[A-Za-z0-9_-]{1,600})?".to_owned(),
            "<redacted>",
        ),
        (
            r"(?i)(?:npm_|ghp_|gho_|ghs_|ghu_|github_pat_|sk-|xox[abpr]-|aiza)[A-Za-z0-9_-]{8,600}"
                .to_owned(),
            "<redacted>",
        ),
        (r"(?:AKIA|ASIA)[0-9A-Z]{16}".to_owned(), "<redacted>"),
        (
            r"(?i)([a-z][a-z0-9+.-]{1,20})://[^/ \t\r\n@:]{1,200}(?::[^/ \t\r\n@]{1,200})?@".to_owned(),
            "${1}://",
        ),
        (
            r#"(?i)((?:https?|wss?)://[^ \t\r\n?#"']{1,600})[?#][^ \t\r\n"']{0,600}"#.to_owned(),
            "${1}",
        ),
        (
            r"[A-Za-z0-9_.+-]{1,64}@[A-Za-z0-9-]{1,63}(?:\.[A-Za-z0-9-]{1,63}){0,8}\.[A-Za-z]{2,24}"
                .to_owned(),
            "<email>",
        ),
        (r"[0-9a-fA-F]{32,600}".to_owned(), "<redacted>"),
        (r"[A-Za-z0-9+_=-]{40,600}".to_owned(), "<redacted>"),
    ]
    .into_iter()
    .map(|(pattern, replacement)| {
        (
            regex::Regex::new(&pattern).expect("scrub pattern"),
            replacement,
        )
    })
    .collect()
});

/// Redact token-like strings in one free-text diagnostic value (bounded to 1200 chars).
pub(crate) fn scrub(text: &str) -> String {
    let mut text: String = text.chars().take(1200).collect();
    for (pattern, replacement) in SECRETS.iter() {
        if pattern.is_match(&text) {
            text = pattern.replace_all(&text, *replacement).into_owned();
        }
    }
    text
}

#[cfg(test)]
#[path = "runner_tests.rs"]
mod tests;
