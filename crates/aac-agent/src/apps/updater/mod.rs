//! The per-app updater contract (`AppUpdater`), frozen by lane W1 on 2026-11-26.
//!
//! This replaces the one-app Python worker (`python.rs` driving
//! `scripts/app-updates/app_updates.py --app <id>`). The coordinator in `update.rs`
//! keeps admission, scheduling, journaling, cancellation and progress framing; it
//! asks [`updater`] for the app's [`AppUpdater`] and runs [`attempt`] for one row.
//!
//! Ownership (each lane edits only its own files; this file and `common.rs`,
//! `processes.rs`, `terminal.rs` belong to W1):
//!
//! | File | Lane | Apps | Python oracle |
//! | --- | --- | --- | --- |
//! | `desktop.rs` | W2 | codex-desktop, claude-desktop | `app_update_desktop.py`, `app_update_confirmed_codex.py`, `app_update_macos.py` |
//! | `zcode.rs` | W3 | zcode, t3-acp-adapters | `app_update_zcode.py` |
//! | `antigravity.rs`, `cursor.rs` | W4 | antigravity-acp, cursor-cli | `app_update_antigravity*.py`, `app_update_cursor.py` |
//! | `t3.rs` | W5 | t3-code (and the T3 RPC client W4 calls) | `app_update_t3.py`, `app_update_t3_rpc.py` |
//! | `cli.rs` | W6 | antigravity-cli, muse-code, omp, codex-cli, claude-code | `app_updates.py` (detect_cli, update_cli, claude_current, Muse/OMP) |
//!
//! W6 wired [`updater`] into `update.rs` through `runner.rs` (one app's lock, stage,
//! cancel and deadline) and `windows_task.rs` (the interactive task on Windows).
//!
//! Rules every implementation follows (they are the Python behavior):
//! - Rows are the same JSON the Python worker printed: [`AppRow::to_value`] is the wire.
//! - Never invent a version, a reset or an outcome. A step that could not run is
//!   `unknown`, a step that ran and said no is `failed` or `action_required`.
//! - Every external command goes through [`System::run`] so fixtures can answer it
//!   and so diagnostics record it (tool word and exit code only, never output or argv).
//! - Respect [`Context::deadline`] and poll [`Context::cancelled`] before any step
//!   that stops, installs or restarts something.
//! - Scratch files live only under [`Context::staging`]; the coordinator owns it.
pub(crate) mod common;
#[cfg(target_os = "macos")]
mod os_mac;
#[cfg(windows)]
mod os_windows;
pub(crate) mod processes;
pub(crate) mod runner;
pub(crate) mod shared;
pub(crate) mod terminal;
pub(crate) mod windows_task;

mod antigravity;
mod cli;
mod cursor;
mod desktop;
mod t3;
mod zcode;

#[cfg(test)]
mod tests;

pub(crate) use common::{CommandOutput, CommandSpec, Diagnostics, Downloaded, Fingerprint};
pub(crate) use processes::{Process, ProcessTable};

use super::{Host, Platform};
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, atomic::AtomicBool},
    time::Duration,
};
use tokio::time::Instant;

/// A boxed future, so [`AppUpdater`] stays object safe and its futures can run in a `JoinSet`.
pub(crate) type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// One app's updater. Implementations are stateless; per-run state lives in [`Context`].
pub(crate) trait AppUpdater: Send + Sync {
    /// The fixed app id, one of `aac_providers::APP_INVENTORY`.
    fn app_id(&self) -> &'static str;

    /// Read-only detection of the installation an update would act on
    /// (Python `_detect_one`). Never installs, stops or writes anything.
    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection>;

    /// Shared installers this app reserves while it runs (Python `update_resources`).
    /// The default is the Python rule; override only where Python does.
    fn resources(&self, install: &Installed) -> Resources {
        Resources::for_install(install)
    }

    /// An answer before the readiness gate, or `None` to continue (Python: the
    /// `claude_current` short cut for claude-code). Errors mean "no answer".
    fn current<'a>(
        &'a self,
        _cx: &'a Context,
        _install: &'a Installed,
    ) -> BoxFuture<'a, Option<AppRow>> {
        Box::pin(async { None })
    }

    /// Read-only pre-flight (Python `check_readiness`). `Ok(())` when ready,
    /// `Err((status, code))` to report that row instead of updating.
    /// The default is the generic rule for apps with their own updater: only
    /// `manager == "unsupported"` refuses. W6 overrides it for the CLIs.
    fn readiness<'a>(
        &'a self,
        _cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<(), (Status, &'static str)>> {
        Box::pin(async move {
            if install.manager == "unsupported" {
                Err((Status::Failed, "unsupported"))
            } else {
                Ok(())
            }
        })
    }

    /// The update itself (Python `update_<app>`), after `checking` and `updating`
    /// phases were reported. Report further phases with [`Context::phase`]
    /// (`downloading` is the only other one the dashboard shows).
    ///
    /// `Err(failure)` becomes a `failed` row with the failure's code, the
    /// installed version as both versions, and the failure's cause/reason in
    /// diagnostics. Where Python turns a failure into another status
    /// (`action_required`, `unknown`, `restart_failed`), return `Ok(row)`.
    fn update<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>>;
}

/// The updater for one app id; `None` for an id outside the inventory.
pub(crate) fn updater(app: &str) -> Option<&'static dyn AppUpdater> {
    Some(match app {
        "antigravity-cli" => &cli::ANTIGRAVITY_CLI,
        "muse-code" => &cli::MUSE,
        "omp" => &cli::OMP,
        "codex-cli" => &cli::CODEX,
        "claude-code" => &cli::CLAUDE,
        "codex-desktop" => &desktop::CODEX_DESKTOP,
        "claude-desktop" => &desktop::CLAUDE_DESKTOP,
        "zcode" => &zcode::ZCODE,
        "t3-acp-adapters" => &zcode::ADAPTERS,
        "t3-code" => &t3::T3,
        "cursor-cli" => &cursor::CURSOR,
        "antigravity-acp" => &antigravity::ACP,
        _ => return None,
    })
}

/// A row status. `held` exists only for Antigravity ACP's held versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Status {
    Updated,
    Current,
    NotInstalled,
    Skipped,
    ActionRequired,
    Failed,
    RestartFailed,
    Unknown,
    Held,
}

impl Status {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Updated => "updated",
            Self::Current => "current",
            Self::NotInstalled => "not_installed",
            Self::Skipped => "skipped",
            Self::ActionRequired => "action_required",
            Self::Failed => "failed",
            Self::RestartFailed => "restart_failed",
            Self::Unknown => "unknown",
            Self::Held => "held",
        }
    }
}

/// The fixed cause classes of a row's diagnostics (Python `CAUSES`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailureCause {
    None,
    Shadowed,
    AuthRequired,
    SigninNeeded,
    PolicyBlocked,
    WrongPrefix,
    NotWritable,
    LostSessionReply,
    T3Absent,
    T3NotReady,
    T3RestartPending,
    InstallerFailed,
    InUse,
    Timeout,
    Unknown,
}

impl FailureCause {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Shadowed => "shadowed",
            Self::AuthRequired => "auth_required",
            Self::SigninNeeded => "signin_needed",
            Self::PolicyBlocked => "policy_blocked",
            Self::WrongPrefix => "wrong_prefix",
            Self::NotWritable => "not_writable",
            Self::LostSessionReply => "lost_session_reply",
            Self::T3Absent => "t3_absent",
            Self::T3NotReady => "t3_not_ready",
            Self::T3RestartPending => "t3_restart_pending",
            Self::InstallerFailed => "installer_failed",
            Self::InUse => "in_use",
            Self::Timeout => "timeout",
            Self::Unknown => "unknown",
        }
    }
}

/// A failed step (Python `UpdateFailure`). `output` is for in-memory
/// classification only and is never written to a row or a log.
#[derive(Clone, Debug, Default)]
pub(crate) struct UpdateFailure {
    /// The row's `messageCode` (`update_failed`, `timeout`, `busy`, `in_use`, ...).
    pub code: &'static str,
    pub output: String,
    pub exit_code: Option<i64>,
    pub cause: Option<FailureCause>,
    /// A fixed reason word (`[a-z0-9][a-z0-9_.:-]{0,63}`).
    pub reason: Option<&'static str>,
    /// Muse's own printed reset line for a used-up subscription, verbatim and bounded.
    pub quota_reset: Option<String>,
}

impl UpdateFailure {
    pub(crate) fn new(code: &'static str) -> Self {
        Self {
            code,
            ..Self::default()
        }
    }

    /// Python's bare `UpdateFailure()`.
    pub(crate) fn failed() -> Self {
        Self::new("update_failed")
    }

    pub(crate) fn with_cause(mut self, cause: FailureCause, reason: Option<&'static str>) -> Self {
        self.cause = Some(cause);
        self.reason = reason;
        self
    }
}

/// What the updater found (Python's `Install` or `None`).
#[derive(Clone, Debug)]
pub(crate) enum Detection {
    /// A successful absent answer: the row reads `not_installed`.
    Absent,
    /// An installation; its `probe` may still say the version check failed.
    Found(Box<Installed>),
}

/// The probe outcome of a detected installation (Python `Install.probe`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Probe {
    Ok,
    /// `unknown` / `check_timeout`.
    Timeout,
    /// `unknown` / `readiness_unknown`, or `policy_blocked` with that reason.
    Failed,
}

/// The installation an update acts on (Python `app_update_common.Install`).
#[derive(Clone, Debug)]
pub(crate) struct Installed {
    pub app_id: &'static str,
    pub platform: Platform,
    pub path: PathBuf,
    pub version: Option<String>,
    /// `native`, `npm`, `apt`, `msix`, `official-download`, `unsupported`, ...
    pub manager: &'static str,
    pub identity: Option<String>,
    pub publisher: Option<String>,
    pub package_root: Option<PathBuf>,
    pub command: Vec<String>,
    pub probe: Probe,
    /// A fixed word naming why the probe failed (`policy_blocked`, `no_version_dir`, ...).
    pub probe_reason: Option<&'static str>,
    /// Other copies beside a managed Codex or Claude install (row `strays`).
    pub strays: Vec<Value>,
    /// Which copy detection chose and how (`diagnostics.owner`): fixed words, home-relative paths.
    pub owner: Option<Value>,
    /// Antigravity ACP only: `harness` (1.3/1.4 pair) or `native` (1.5+ CLI).
    pub runtime_kind: Option<&'static str>,
    /// Cursor only: another copy first on PATH that runs instead (`{"path","version"}`).
    pub shadow: Option<Value>,
}

impl Installed {
    pub(crate) fn new(
        app_id: &'static str,
        platform: Platform,
        path: PathBuf,
        version: Option<String>,
        manager: &'static str,
    ) -> Self {
        Self {
            app_id,
            platform,
            path,
            version,
            manager,
            identity: None,
            publisher: None,
            package_root: None,
            command: Vec::new(),
            probe: Probe::Ok,
            probe_reason: None,
            strays: Vec::new(),
            owner: None,
            runtime_kind: None,
            shadow: None,
        }
    }
}

/// Shared installers and runtimes one app reserves (Python `update_resources`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Resources(pub u8);

impl Resources {
    pub(crate) const NPM: u8 = 1;
    pub(crate) const CODEX: u8 = 2;
    pub(crate) const APT: u8 = 4;
    pub(crate) const MSIX: u8 = 8;

    pub(crate) fn for_install(install: &Installed) -> Self {
        let mut bits = 0;
        if install.manager == "npm"
            || install.app_id == "t3-acp-adapters"
            || (install.app_id == "zcode" && install.platform == Platform::Ubuntu)
        {
            bits |= Self::NPM;
        }
        if matches!(install.app_id, "codex-cli" | "codex-desktop") {
            bits |= Self::CODEX;
        }
        bits |= match install.manager {
            "apt" => Self::APT,
            "msix" => Self::MSIX,
            _ => 0,
        };
        Self(bits)
    }

    pub(crate) fn overlaps(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }
}

/// One result row (the Python `result()` dict plus the optional fields the
/// worker whitelist keeps). [`AppRow::to_value`] is the exact wire form.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct AppRow {
    pub app_id: String,
    pub platform: Platform,
    pub status: Status,
    pub previous_version: Option<String>,
    pub version: Option<String>,
    pub manager: Option<String>,
    /// `None` means the status word (Python `code or status`).
    pub message_code: Option<String>,
    pub update_attempted: bool,
    pub restarted_processes: u32,
    pub forced_stops: Option<u32>,
    pub held_version: Option<String>,
    pub restart_targets: Option<Vec<Value>>,
    pub parts: Option<Value>,
    pub strays: Option<Vec<Value>>,
    /// Muse's own printed reset time for a used-up quota, verbatim (`quotaReset`).
    pub quota_reset: Option<String>,
    /// Set by [`attempt`] from the row's [`Diagnostics`] unless the updater set it.
    pub diagnostics: Option<Value>,
}

impl AppRow {
    /// Python `result(app_id, platform, status, before, after, manager, code)`.
    pub(crate) fn new(app_id: &str, platform: Platform, status: Status) -> Self {
        Self {
            app_id: app_id.to_owned(),
            platform,
            status,
            previous_version: None,
            version: None,
            manager: None,
            message_code: None,
            update_attempted: false,
            restarted_processes: 0,
            forced_stops: None,
            held_version: None,
            restart_targets: None,
            parts: None,
            strays: None,
            quota_reset: None,
            diagnostics: None,
        }
    }

    /// The common shape `result(app, platform, status, before, after, manager, code)`.
    pub(crate) fn of(
        install: &Installed,
        status: Status,
        before: Option<&str>,
        after: Option<&str>,
        code: &str,
    ) -> Self {
        let mut row = Self::new(install.app_id, install.platform, status);
        row.previous_version = before.map(str::to_owned);
        row.version = after.map(str::to_owned);
        row.manager = Some(install.manager.to_owned());
        row.message_code = Some(code.to_owned());
        row
    }

    pub(crate) fn code(mut self, code: &str) -> Self {
        self.message_code = Some(code.to_owned());
        self
    }

    pub(crate) fn attempted(mut self, restarted: u32) -> Self {
        self.update_attempted = true;
        self.restarted_processes = restarted;
        self
    }

    pub(crate) fn message_code(&self) -> &str {
        self.message_code.as_deref().unwrap_or(self.status.as_str())
    }

    pub(crate) fn to_value(&self) -> Value {
        let mut value = json!({
            "appId": self.app_id, "platform": self.platform.name(), "status": self.status.as_str(),
            "previousVersion": self.previous_version, "version": self.version, "manager": self.manager,
            "messageCode": self.message_code(), "updateAttempted": self.update_attempted,
            "restartedProcesses": self.restarted_processes,
        });
        let fields = value.as_object_mut().expect("row object");
        let mut put = |key: &str, item: Option<Value>| {
            if let Some(item) = item {
                fields.insert(key.into(), item);
            }
        };
        put("forcedStops", self.forced_stops.map(Value::from));
        put("heldVersion", self.held_version.clone().map(Value::from));
        put(
            "restartTargets",
            self.restart_targets.clone().map(Value::Array),
        );
        put("parts", self.parts.clone());
        put("strays", self.strays.clone().map(Value::Array));
        put("quotaReset", self.quota_reset.clone().map(Value::from));
        put("diagnostics", self.diagnostics.clone());
        value
    }
}

/// The host services an updater uses. The native implementation is
/// [`common::Native`]; tests and parity fixtures supply their own answers.
pub(crate) trait System: Send + Sync {
    /// Run one fixed command (Python `command()` / `powershell()`): no TTY, empty
    /// stdin, its own session, killed at `spec.timeout`. A non-zero exit is
    /// `Err(UpdateFailure { code: "update_failed", exit_code, output })`, a
    /// timeout `Err(code "timeout")`, a spawn failure `Err(reason "not_started")`.
    /// Records the run in `diagnostics` unless `spec.quiet`.
    fn run<'a>(
        &'a self,
        spec: &'a CommandSpec,
        diagnostics: &'a Mutex<Diagnostics>,
    ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>>;

    /// Scan the processes relevant to app updates (Python `processes.scan`).
    fn processes(&self, platform: Platform) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>>;

    /// Download one fixed official HTTPS URL to `destination` (Python `download`):
    /// HTTP 403 or an HTML challenge page is `download_blocked`, over `maximum` is
    /// `update_failed`, past `timeout` is `timeout`.
    fn download<'a>(
        &'a self,
        url: &'a str,
        destination: &'a Path,
        maximum: u64,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>>;

    /// A small GET (release feeds, `latest.yml`, version pointers), capped at `maximum`
    /// bytes, with a hard deadline (Python `read_bounded`): anything but HTTP 200 at a
    /// final URL starting with `prefix` is `update_failed`; past `timeout` is `timeout`.
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        prefix: &'a str,
        maximum: u64,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>>;

    /// One HEAD request (Python `remote_fingerprint`); `None` when the server is unclear.
    fn fingerprint<'a>(
        &'a self,
        url: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Option<Fingerprint>>;

    /// One HTTP byte-range GET (Python `RangeReader._get`): only a `206` at an https
    /// URL with a matching `Content-Range` is an answer. Use [`shared::remote_zip_member`].
    /// Added after the freeze with a default, so existing implementations still build.
    fn range<'a>(
        &'a self,
        url: &'a str,
        range: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<shared::RangeReply, UpdateFailure>> {
        let _ = (url, range, timeout);
        Box::pin(async { Err(UpdateFailure::failed()) })
    }
}

/// State shared by every app of one host run (Python module globals).
#[derive(Default)]
pub(crate) struct Shared {
    /// Python `UBUNTU_RUN["unsettled"]`: a timed-out wait for Ubuntu's adapter
    /// oneshot. While set, npm and T3 work reports `busy` until the unit finished.
    pub adapter_unit_unsettled: AtomicBool,
}

/// Everything one app's attempt may use. Built by the coordinator per app.
pub(crate) struct Context {
    pub platform: Platform,
    pub host: Arc<Host>,
    pub system: Arc<dyn System>,
    pub shared: Arc<Shared>,
    /// The row's recorder (Python `row_diagnostics()`).
    pub diagnostics: Mutex<Diagnostics>,
    /// When this app must stop starting new steps (Python `deadline`).
    pub deadline: Instant,
    /// This app's private scratch folder (Python `worker_stage`, `AAC_UPDATE_WORKER_STAGING`).
    pub staging: PathBuf,
    /// The host's app-update state folder (Python `AAC_UPDATE_STATE_DIR` or `~/.ccs/app-updates`).
    pub state: PathBuf,
    /// The T3 row this job already produced, for Antigravity ACP (Python `t3_row`).
    pub t3_row: Option<Value>,
    cancel: Box<dyn Fn() -> bool + Send + Sync>,
    phases: Box<dyn Fn(&'static str) + Send + Sync>,
}

impl Context {
    #[expect(
        clippy::too_many_arguments,
        reason = "one context carries a run's fixed inputs"
    )]
    pub(crate) fn new(
        platform: Platform,
        host: Arc<Host>,
        system: Arc<dyn System>,
        shared: Arc<Shared>,
        deadline: Instant,
        staging: PathBuf,
        state: PathBuf,
        t3_row: Option<Value>,
        cancel: impl Fn() -> bool + Send + Sync + 'static,
        phases: impl Fn(&'static str) + Send + Sync + 'static,
    ) -> Self {
        Self {
            platform,
            host,
            system,
            shared,
            diagnostics: Mutex::new(Diagnostics::default()),
            deadline,
            staging,
            state,
            t3_row,
            cancel: Box::new(cancel),
            phases: Box::new(phases),
        }
    }

    /// True once the job was cancelled. Poll before stopping, installing or restarting.
    pub(crate) fn cancelled(&self) -> bool {
        (self.cancel)()
    }

    /// Report a phase (`checking`, `updating`, `downloading`, or any fixed word for
    /// diagnostics timing only) and time it in the row's diagnostics.
    pub(crate) fn phase(&self, name: &'static str) {
        self.record(|record| record.phase(Some(name)));
        (self.phases)(name);
    }

    /// Time left before [`Context::deadline`].
    pub(crate) fn remaining(&self) -> Duration {
        self.deadline.saturating_duration_since(Instant::now())
    }

    /// Python `_clamp(seconds, deadline)`: a step's timeout, never past the deadline, at least one second.
    pub(crate) fn clamp(&self, seconds: u64) -> Duration {
        Duration::from_secs(seconds)
            .min(self.remaining())
            .max(Duration::from_secs(1))
    }

    /// Run a command with this row's recorder.
    pub(crate) async fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, UpdateFailure> {
        self.system.run(spec, &self.diagnostics).await
    }

    /// Change the row's recorder (blame, note, owner).
    pub(crate) fn record<T>(&self, change: impl FnOnce(&mut Diagnostics) -> T) -> T {
        let mut record = self
            .diagnostics
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        change(&mut record)
    }
}

/// One app's whole attempt (Python `update_one_app` + `attempt_one`), always a row.
/// The coordinator handles `skipped` for a job cancelled before start and the
/// host deadline before start; this repeats both checks, as Python did.
pub(crate) async fn attempt(updater: &dyn AppUpdater, cx: &Context) -> AppRow {
    let app = updater.app_id();
    let platform = cx.platform;
    let detection = if cx.cancelled() {
        None
    } else {
        Some(updater.detect(cx).await)
    };
    let install = match &detection {
        Some(Detection::Found(install)) => Some(install.as_ref()),
        _ => None,
    };
    if let Some(owner) = install.and_then(|install| install.owner.clone()) {
        cx.record(|record| record.owner = Some(owner));
    }
    let mut row = match detection {
        None => AppRow::new(app, platform, Status::Skipped).code("skipped_cancelled"),
        Some(Detection::Absent) => AppRow::new(app, platform, Status::NotInstalled),
        Some(Detection::Found(ref install)) => attempt_found(updater, cx, install).await,
    };
    if row.strays.is_none()
        && let Some(install) = install.filter(|install| !install.strays.is_empty())
    {
        row.strays = Some(install.strays.clone());
    }
    if row.diagnostics.is_none() {
        let value = row.to_value();
        row.diagnostics = Some(cx.record(|record| record.export(&value)));
    }
    row
}

async fn attempt_found(updater: &dyn AppUpdater, cx: &Context, install: &Installed) -> AppRow {
    let app = updater.app_id();
    let platform = cx.platform;
    let version = install.version.as_deref();
    let failed = |code: &str| AppRow::of(install, Status::Failed, version, version, code);
    match install.probe {
        Probe::Timeout => {
            let mut row = AppRow::new(app, platform, Status::Unknown).code("check_timeout");
            row.manager = Some(install.manager.into());
            return row;
        }
        Probe::Failed if install.probe_reason == Some("policy_blocked") => {
            cx.record(|record| record.blame(FailureCause::PolicyBlocked, Some("execution_policy")));
            let mut row = AppRow::new(app, platform, Status::ActionRequired).code("policy_blocked");
            row.manager = Some(install.manager.into());
            return row;
        }
        Probe::Failed => {
            let reason = install.probe_reason.unwrap_or("probe_failed");
            cx.record(|record| record.blame(FailureCause::Unknown, Some(reason)));
            return AppRow::new(app, platform, Status::Unknown).code("readiness_unknown");
        }
        Probe::Ok => {}
    }
    if Instant::now() >= cx.deadline {
        return failed("timeout");
    }
    if cx.cancelled() {
        return AppRow::new(app, platform, Status::Skipped).code("skipped_cancelled");
    }
    if platform == Platform::Ubuntu
        && (updater
            .resources(install)
            .overlaps(Resources(Resources::NPM))
            || app == "t3-code")
        && zcode::adapter_unit_busy(cx).await
    {
        return AppRow::of(install, Status::ActionRequired, version, version, "busy");
    }
    cx.phase("checking");
    if let Some(row) = updater.current(cx, install).await {
        return row;
    }
    if let Err((status, code)) = updater.readiness(cx, install).await {
        return AppRow::of(install, status, version, version, code);
    }
    if cx.cancelled() {
        return AppRow::new(app, platform, Status::Skipped).code("skipped_cancelled");
    }
    cx.phase("updating");
    match updater.update(cx, install).await {
        Ok(row) => row,
        Err(failure) => {
            if let Some(cause) = failure.cause {
                cx.record(|record| record.blame(cause, failure.reason));
            }
            failed(failure.code)
        }
    }
}

/// The app's private scratch folder (Python `worker_stage`): a folder left by an
/// earlier worker is removed only when it carries this marker, never through a link.
pub(crate) struct Stage {
    directory: PathBuf,
}

const STAGE_MARKER: &str = ".aac-app-worker";
const STAGE_STAMP: &[u8] = b"aac app worker stage v1";

impl Stage {
    pub(crate) fn open(state: &Path, app: &str) -> Result<Self, UpdateFailure> {
        let unsafe_folder = || UpdateFailure::new("readiness_unknown");
        let root = state.join("agent-staging");
        let directory = root.join(app);
        let linked = |path: &Path| {
            std::fs::symlink_metadata(path)
                .is_ok_and(|info| info.file_type().is_symlink() || common::is_junction(&info))
        };
        if linked(&root) || linked(&directory) {
            return Err(unsafe_folder());
        }
        if std::fs::symlink_metadata(&directory).is_ok() {
            let marker = directory.join(STAGE_MARKER);
            if linked(&marker) {
                return Err(unsafe_folder());
            }
            match super::read::read(&marker, &super::ReadPolicy::capped(64)) {
                super::Read::Bytes(bytes, _) if bytes == STAGE_STAMP => {}
                _ => return Err(unsafe_folder()),
            }
            std::fs::remove_dir_all(&directory).map_err(|_| unsafe_folder())?;
        }
        common::private_dir(&directory).map_err(|_| unsafe_folder())?;
        std::fs::write(directory.join(STAGE_MARKER), STAGE_STAMP).map_err(|_| unsafe_folder())?;
        Ok(Self { directory })
    }

    pub(crate) fn path(&self) -> &Path {
        &self.directory
    }
}

impl Drop for Stage {
    fn drop(&mut self) {
        if std::fs::symlink_metadata(&self.directory).is_ok_and(|info| info.is_dir()) {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

/// An unported app's honest answer: its detection could not run, so the row is
/// `unknown` / `readiness_unknown` and nothing is installed. Each lane replaces it.
pub(crate) struct Unported(pub &'static str);

impl AppUpdater for Unported {
    fn app_id(&self) -> &'static str {
        self.0
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            let mut install = Installed::new(self.0, cx.platform, PathBuf::new(), None, "native");
            install.probe = Probe::Failed;
            install.probe_reason = Some("not_ported");
            Detection::Found(Box::new(install))
        })
    }

    fn update<'a>(
        &'a self,
        _cx: &'a Context,
        _install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>> {
        Box::pin(async { Err(UpdateFailure::failed()) })
    }
}
