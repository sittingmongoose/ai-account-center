//! The Ubuntu Codex update bridge (`scripts/app-updates/app_update_codex.cjs`): one
//! Codex CLI or Codex Desktop update around the shared Codex process runtime
//! ([`crate::codex_update`], lane P2). Runtime snapshots and their environments stay
//! in memory; the only file written is the desktop's pending-restart marker.
//!
//! The CLI (owner decision 2026-11-08): each click stands alone. It runs the updater
//! and, only if the installed version changed, restarts the shared daemon now and
//! checks it came up; nothing is carried to a later click. Codex Desktop: the
//! desktops a run closed are owed until AAC's own relaunch of each is seen coming up,
//! kept in the marker for the next click.
use crate::apps::updater::{AppRow, Context, Platform, Status, common::write_private_json};
use serde_json::{Map, Value, json};
use std::{path::PathBuf, sync::LazyLock, time::Duration};

#[cfg(target_os = "linux")]
use crate::codex_update::{Backend, Launch, Native, Runtime};
use crate::codex_update::{Identity, RestartPlan, RestartState};

/// Which Codex the bridge updates (`--operation`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BridgeMode {
    /// `codex-desktop` through apt.
    Desktop,
    /// `codex-cli` through `codex update`.
    Cli,
}

/// Shared Codex work is often busy for hours. Waiting longer than this for idle only
/// stalls Update all; a busy Codex becomes an honest `codex_busy` row instead.
const IDLE_WAIT_SECONDS: u64 = 60;
const LOCK_WAIT_SECONDS: u64 = 30;
const RESTART_SETTLE_MS: u64 = 10_000;
/// proper-lockfile's `stale` and `update` for `~/.codex/.ccs-activation.lock`.
const LOCK_STALE: Duration = Duration::from_secs(120);
const LOCK_UPDATE: Duration = Duration::from_secs(5);

static NUMERIC: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[0-9]+\.[0-9]+(?:\.[0-9]+){0,3}").expect("numeric version")
});
static FULL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[0-9]+\.[0-9]+(?:\.[0-9]+){0,3}(?:[-+][0-9A-Za-z.+-]+)?")
        .expect("full version")
});

/// The bridge's `version()`: the numeric part, for marker matching and comparisons.
pub(super) fn version(text: Option<&str>) -> Option<String> {
    NUMERIC.find(text?).map(|found| found.as_str().to_owned())
}

/// The bridge's `fullVersion()`: the whole version, a prerelease included.
pub(super) fn full_version(text: Option<&str>) -> Option<String> {
    FULL.find(text?).map(|found| found.as_str().to_owned())
}

/// The bridge's `desktopsOwed(marker, legacyDesktop)`.
pub(super) fn desktops_owed(marker: Option<&Map<String, Value>>, legacy: bool) -> usize {
    let listed = match marker.and_then(|marker| marker.get("desktops")) {
        Some(Value::Array(items)) => items.iter().filter(|item| item.is_string()).count(),
        Some(Value::Number(number)) => number
            .as_u64()
            .filter(|count| *count > 0)
            .and_then(|count| usize::try_from(count).ok())
            .unwrap_or(0),
        _ => 0,
    };
    if listed > 0 {
        return listed;
    }
    usize::from(
        marker.and_then(|marker| marker.get("desktop")) == Some(&Value::Bool(true)) || legacy,
    )
}

/// The bridge's `markerMatches(marker, installed)`.
pub(super) fn marker_matches(marker: Option<&Map<String, Value>>, installed: Option<&str>) -> bool {
    let Some(want) = version(installed) else {
        return false;
    };
    let Some(marker) = marker else {
        return false;
    };
    ["version", "target"].iter().any(|key| {
        version(marker.get(*key).and_then(Value::as_str)).as_deref() == Some(want.as_str())
    })
}

/// What a run does to the desktop marker.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum MarkerChange {
    /// The CLI keeps no marker.
    Keep,
    Clear,
    Write(Value),
}

/// A runtime failure: `busy` is retried until the idle deadline.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum StopError {
    Busy,
    Failed,
}

/// Everything [`execute`] needs (the bridge's `bridgeDeps` plus its runtime).
/// The native implementation is [`NativeDeps`]; tests script their own.
#[allow(async_fn_in_trait)]
pub(super) trait Deps {
    /// Milliseconds on a monotonic clock.
    fn now(&self) -> u64;
    async fn sleep(&mut self, millis: u64);
    /// The installed version's text (dpkg-query or `codex --version`); `Err` when it failed.
    async fn read_version(&mut self) -> Result<String, ()>;
    /// The fixed installer (apt-get or `codex update`).
    async fn update(&mut self) -> Result<(), ()>;
    /// The pending-restart marker, when one is readable.
    fn marked(&self) -> Option<Map<String, Value>>;
    /// Apply a marker change; `false` when the write failed.
    fn change(&mut self, change: &MarkerChange) -> bool;
    /// CLI only: replace old supervised `app-server proxy` processes; the forced stops.
    async fn restart_proxies(&mut self) -> Result<u32, ()>;
    /// CLI only: whether an old proxy is still up after the restart (and how many fresh ones came).
    async fn has_old_proxies(&mut self) -> (bool, u32);
    /// [`crate::codex_update::Runtime::stop`]: `record` names the marker change to apply
    /// under the runtime's lock, before any signal; a failed write closes nothing.
    async fn stop(
        &mut self,
        record: &(dyn Fn(RestartPlan) -> MarkerChange + Sync),
    ) -> Result<(), StopError>;
    fn stop_plan(&self) -> RestartPlan;
    async fn retired_plan(&mut self) -> Result<RestartPlan, ()>;
    async fn restart_state(&mut self) -> Result<RestartState, ()>;
    async fn verify_running_daemon(&mut self) -> Result<Option<Identity>, ()>;
    async fn start(&mut self) -> Result<(), ()>;
    async fn dispose(&mut self);
    /// How many processes the runtime relaunched so far.
    fn launches(&self) -> u32;
}

/// The bridge's `output`.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Output {
    pub status: Status,
    pub previous: Option<String>,
    pub version: Option<String>,
    pub code: &'static str,
    pub attempted: bool,
    pub restarted: u32,
    pub forced: u32,
}

impl Default for Output {
    fn default() -> Self {
        Self {
            status: Status::Failed,
            previous: None,
            version: None,
            code: "update_failed",
            attempted: false,
            restarted: 0,
            forced: 0,
        }
    }
}

impl Output {
    pub(super) fn row(&self, mode: BridgeMode) -> AppRow {
        let (app, manager) = match mode {
            BridgeMode::Desktop => ("codex-desktop", "apt"),
            BridgeMode::Cli => ("codex-cli", "native"),
        };
        let mut row = AppRow::new(app, Platform::Ubuntu, self.status);
        row.previous_version.clone_from(&self.previous);
        row.version.clone_from(&self.version);
        row.manager = Some(manager.to_owned());
        row.message_code = Some(self.code.to_owned());
        row.update_attempted = self.attempted;
        row.restarted_processes = self.restarted;
        row.forced_stops = Some(self.forced);
        row
    }
}

/// One request (the bridge's flags).
#[derive(Clone, Debug)]
pub(super) struct Request {
    pub mode: BridgeMode,
    pub expected: Option<String>,
    pub restart_only: bool,
    pub seconds: u64,
    /// Seconds to wait for idle; [`IDLE_WAIT_SECONDS`] unless a test shortens it.
    pub idle_seconds: u64,
    pub settle_ms: u64,
}

fn counted(plan: RestartPlan) -> (bool, usize) {
    (plan.daemon, plan.desktops)
}

/// The bridge's `verifiedState`: what runs now, with a daemon that owes readiness held
/// to the runtime's own bounded check, judged on a scan taken after it. A daemon that
/// replaced the checked one gets one more check; one that keeps being replaced is not ready.
async fn verified_daemon(deps: &mut impl Deps, owed_daemon: bool) -> Result<bool, ()> {
    let mut after = deps.restart_state().await?;
    if !owed_daemon || after.daemon.is_none() {
        return Ok(after.daemon.is_some());
    }
    for _ in 0..2 {
        let checked = deps.verify_running_daemon().await?;
        after = deps.restart_state().await?;
        let (Some(checked), Some(now)) = (checked, after.daemon.as_ref()) else {
            return Ok(false);
        };
        if checked == *now {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The bridge's `execute(deps)`: one Codex update, always an [`Output`].
pub(super) async fn execute(deps: &mut impl Deps, request: &Request) -> Output {
    let mut output = Output::default();
    let mut state = Run::default();
    let outcome = attempt(deps, request, &mut output, &mut state).await;
    if outcome.is_err() {
        recover(deps, request, &mut output, &mut state).await;
    }
    if state.runtime {
        deps.dispose().await;
    }
    output
}

#[derive(Default)]
struct Run {
    runtime: bool,
    closed: Option<(bool, usize)>,
    stopped: bool,
    installed: bool,
    updating: bool,
    busy: bool,
    stop_failed: bool,
    prior: usize,
    nothing_owed: bool,
    full_previous: Option<String>,
    full_now: Option<String>,
}

impl Run {
    fn changed(&self) -> bool {
        self.full_now != self.full_previous
    }
}

/// The marker a desktop run keeps: desktops owed, or none (the CLI keeps no marker).
fn record(
    deps: &mut impl Deps,
    request: &Request,
    output: &Output,
    desktops: usize,
    with_target: bool,
) -> bool {
    deps.change(&marker_change(
        request,
        output.version.as_deref(),
        desktops,
        with_target,
    ))
}

/// The bridge's `record(desktops, withTarget)` as a change: `{version, desktop, desktops[, target]}`.
pub(super) fn marker_change(
    request: &Request,
    installed: Option<&str>,
    desktops: usize,
    with_target: bool,
) -> MarkerChange {
    if request.mode != BridgeMode::Desktop {
        return MarkerChange::Keep;
    }
    if desktops == 0 {
        return MarkerChange::Clear;
    }
    let mut value = json!({
        "version": version(installed), "desktop": true, "desktops": desktops,
    });
    if with_target
        && !request.restart_only
        && let Some(target) = version(request.expected.as_deref())
    {
        value["target"] = Value::from(target);
    }
    MarkerChange::Write(value)
}

/// The installed version now, into the output (the bridge's `takeInstalled`).
async fn take_installed(deps: &mut impl Deps, output: &mut Output, state: &mut Run) -> bool {
    match deps.read_version().await {
        Ok(read) if version(Some(&read)).is_some() => {
            output.version = full_version(Some(&read));
            state.full_now.clone_from(&output.version);
            true
        }
        _ => {
            output.version = None;
            output.code = "version_unknown";
            false
        }
    }
}

async fn attempt(
    deps: &mut impl Deps,
    request: &Request,
    output: &mut Output,
    state: &mut Run,
) -> Result<(), ()> {
    let desktop = request.mode == BridgeMode::Desktop;
    let idle = request.seconds.min(request.idle_seconds).max(1);
    let deadline = deps.now() + idle * 1000;
    let first = deps.read_version().await?;
    output.previous = full_version(Some(&first));
    output.version.clone_from(&output.previous);
    state.full_previous.clone_from(&output.version);
    state.full_now.clone_from(&output.version);
    if version(Some(&first)).is_none() {
        output.version = None;
        output.code = "version_unknown";
        return Ok(());
    }
    let expected = request.expected.as_deref().filter(|text| !text.is_empty());
    if desktop {
        // A desktop marker from before these fields counts as one desktop when it is for this install.
        let marker = deps.marked();
        let unnamed = marker.as_ref().is_some_and(|marker| {
            !marker.contains_key("desktop") && !marker.contains_key("daemon")
        });
        let legacy = unnamed
            && (request.restart_only
                || marker_matches(marker.as_ref(), output.previous.as_deref()));
        state.prior = desktops_owed(marker.as_ref(), legacy);
    } else {
        output.attempted = true;
        state.updating = true;
        deps.update().await?;
        state.updating = false;
        if !take_installed(deps, output, state).await {
            return Ok(());
        }
        if expected.is_some() && version(output.version.as_deref()) != version(expected) {
            return Err(());
        }
        if !state.changed() {
            output.status = Status::Current;
            output.code = "current";
            return Ok(());
        }
    }
    // A desktop run records what stop() is about to close under its lock, before it
    // signals anything, so a run cut off inside stop() still leaves it.
    state.runtime = true;
    loop {
        let prior = state.prior;
        let installed = output.version.clone();
        let hook = |plan: RestartPlan| {
            marker_change(request, installed.as_deref(), prior + plan.desktops, true)
        };
        match deps.stop(&hook).await {
            Ok(()) => {
                state.stopped = true;
                break;
            }
            Err(StopError::Failed) => {
                state.stop_failed = true;
                return Err(());
            }
            Err(StopError::Busy) => {
                if deps.now() >= deadline {
                    state.busy = true;
                    return Err(());
                }
                let pause = 5000.min(deadline.saturating_sub(deps.now()));
                deps.sleep(pause).await;
            }
        }
    }
    let closed = counted(deps.stop_plan());
    state.closed = Some(closed);
    if !record(deps, request, output, state.prior + closed.1, true) {
        return Err(());
    }
    if desktop {
        output.attempted = !request.restart_only;
        state.updating = true;
        if !request.restart_only {
            deps.update().await?;
        }
        state.updating = false;
        if !take_installed(deps, output, state).await {
            return Err(());
        }
        if (!request.restart_only && !state.changed())
            || (expected.is_some() && version(output.version.as_deref()) != version(expected))
        {
            return Err(());
        }
        state.installed = true;
        if !record(deps, request, output, state.prior + closed.1, false) {
            return Err(());
        }
    }
    output.forced += deps.restart_proxies().await?;
    let started = deps.start().await;
    output.restarted = deps.launches();
    started?;
    state.stopped = false;
    let (old, fresh) = deps.has_old_proxies().await;
    if old {
        return Err(());
    }
    output.restarted += fresh;
    // The daemon must be ready on a scan taken now. A desktop is back only once start()
    // relaunched it and saw the process it started come up, so what an earlier desktop
    // run closed stays owed: a retry says so once and clears it.
    let left = deps.retired_plan().await?.desktops;
    let (owed_daemon, owed_desktops) = (closed.0, state.prior + left);
    let settle_by = deps.now() + request.settle_ms;
    let mut live = deps.restart_state().await?;
    while owed_daemon && live.daemon.is_none() && live.proxy && deps.now() < settle_by {
        deps.sleep(250).await;
        live = deps.restart_state().await?;
    }
    let daemon_down = owed_daemon && !verified_daemon(deps, owed_daemon).await?;
    if daemon_down || owed_desktops > 0 {
        let written = if state.prior > 0 {
            output.code = "restart_not_running";
            record(deps, request, output, 0, false)
        } else {
            output.code = "restart_failed";
            record(deps, request, output, left, false)
        };
        if !written {
            return Err(());
        }
        output.status = Status::RestartFailed;
        return Ok(());
    }
    if !record(deps, request, output, 0, false) {
        return Err(());
    }
    output.status = Status::Updated;
    output.code = "updated";
    Ok(())
}

/// The bridge's `catch`: bring back what was stopped and name what is owed.
async fn recover(deps: &mut impl Deps, request: &Request, output: &mut Output, state: &mut Run) {
    let desktop = request.mode == BridgeMode::Desktop;
    // An installer that failed may still have replaced the app: the row names what is installed now.
    if state.updating {
        match deps.read_version().await {
            Ok(read) if version(Some(&read)).is_some() => {
                output.version = full_version(Some(&read));
                state.full_now.clone_from(&output.version);
            }
            _ => {
                output.version = None;
                output.code = "version_unknown";
            }
        }
    }
    if state.stopped {
        let _ = deps.start().await;
        output.restarted = deps.launches();
        // A desktop install that failed owes no restart once everything is back.
        if desktop
            && !state.installed
            && let Some(closed) = state.closed
            && let Ok(retired) = deps.retired_plan().await
        {
            let left = retired.desktops;
            if let Ok(up) = verified_daemon(deps, closed.0).await {
                let daemon_down = closed.0 && !up;
                if !daemon_down && state.prior + left == 0 {
                    record(deps, request, output, 0, false);
                    state.nothing_owed = true;
                } else {
                    record(deps, request, output, state.prior + left, true);
                }
            }
        }
    }
    // A desktop stop() that failed partway owes the desktops it closed and did not bring back.
    if state.stop_failed && desktop {
        let desktops = match deps.retired_plan().await {
            Ok(retired) => retired.desktops,
            Err(()) => deps.stop_plan().desktops,
        };
        record(deps, request, output, state.prior + desktops, true);
    }
    if state.busy {
        // Nothing was stopped. A CLI package may already be new; its daemon restarts later.
        output.status = Status::ActionRequired;
        output.code = "codex_busy";
    } else if output.code == "version_unknown" && output.version.is_none() {
        output.status = Status::Failed;
    } else if !desktop {
        // Installed but not restarted, or not installed: either way the old daemon keeps running.
        let restart_phase =
            state.changed() && (state.stop_failed || state.stopped || state.closed.is_some());
        (output.status, output.code) = if restart_phase {
            (Status::RestartFailed, "restart_failed")
        } else {
            (Status::Failed, "update_failed")
        };
    } else {
        let pending = marker_matches(deps.marked().as_ref(), output.version.as_deref());
        let owed = !state.nothing_owed && (state.prior > 0 || state.changed() || pending);
        (output.status, output.code) = if owed {
            (Status::RestartFailed, "restart_failed")
        } else {
            (Status::Failed, "update_failed")
        };
    }
}

/// The bridge's lock: proper-lockfile's directory lock at `~/.codex/.ccs-activation.lock`
/// (stale after 120 s, its time refreshed every 5 s while held), so a bridge still
/// running from an older install never updates Codex at the same time.
pub(super) struct ActivationLock {
    path: PathBuf,
    refresh: Option<tokio::task::JoinHandle<()>>,
}

impl ActivationLock {
    pub(super) async fn acquire(codex_home: &std::path::Path, seconds: u64) -> Option<Self> {
        let path = codex_home.join(".ccs-activation.lock");
        let retries = (seconds.min(LOCK_WAIT_SECONDS) / 5).max(1);
        for attempt in 0..=retries {
            match std::fs::create_dir(&path) {
                Ok(()) => {
                    let touched = path.clone();
                    let refresh = tokio::spawn(async move {
                        loop {
                            tokio::time::sleep(LOCK_UPDATE).await;
                            let _ = std::fs::File::open(&touched)
                                .and_then(|file| file.set_modified(std::time::SystemTime::now()));
                        }
                    });
                    return Some(Self {
                        path,
                        refresh: Some(refresh),
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    let stale = std::fs::metadata(&path)
                        .and_then(|info| info.modified())
                        .ok()
                        .and_then(|modified| modified.elapsed().ok())
                        .is_some_and(|age| age > LOCK_STALE);
                    if stale && std::fs::remove_dir(&path).is_ok() {
                        continue;
                    }
                }
                Err(_) => return None,
            }
            if attempt < retries {
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
        None
    }
}

impl Drop for ActivationLock {
    fn drop(&mut self) {
        if let Some(refresh) = self.refresh.take() {
            refresh.abort();
        }
        let _ = std::fs::remove_dir(&self.path);
    }
}

/// One Codex update on this Ubuntu host (the bridge's `main`), as its row. `expected`
/// is the version the installer must leave (`--expected-version`); `restart_only`
/// relaunches what an earlier run closed without installing; `seconds` bounds the run.
/// Takes `~/.codex/.ccs-activation.lock` itself: callers must not hold it.
pub(crate) async fn codex_bridge(
    cx: &Context,
    mode: BridgeMode,
    expected: Option<&str>,
    restart_only: bool,
    seconds: u64,
) -> AppRow {
    let request = Request {
        mode,
        expected: expected.map(str::to_owned),
        restart_only,
        seconds: seconds.clamp(30, 900),
        idle_seconds: IDLE_WAIT_SECONDS,
        settle_ms: RESTART_SETTLE_MS,
    };
    native(cx, &request).await.row(mode)
}

#[cfg(not(target_os = "linux"))]
async fn native(_cx: &Context, _request: &Request) -> Output {
    Output::default()
}

#[cfg(target_os = "linux")]
async fn native(cx: &Context, request: &Request) -> Output {
    let executable = cx.host.home.join(".local/bin/codex");
    if !executable.exists() {
        return Output::default();
    }
    let codex_home = cx.host.home.join(".codex");
    let Some(lock) = ActivationLock::acquire(&codex_home, request.seconds).await else {
        // Another Codex update holds the lock: come back when it is done.
        return Output {
            status: Status::ActionRequired,
            code: "codex_busy",
            ..Output::default()
        };
    };
    let Some(mut deps) = NativeDeps::new(cx, request.mode, executable, codex_home) else {
        return Output::default();
    };
    let output = execute(&mut deps, request).await;
    drop(lock);
    output
}

/// [`Native`] with a count of its relaunches (the bridge's `restartedProcesses++`).
#[cfg(target_os = "linux")]
pub(super) struct Counting {
    inner: Native,
    launches: std::sync::Arc<std::sync::atomic::AtomicU32>,
}

#[cfg(target_os = "linux")]
impl Backend for Counting {
    async fn scan(&mut self) -> crate::codex_update::Result<Vec<crate::codex_update::Process>> {
        self.inner.scan().await
    }
    async fn lock(&mut self) -> crate::codex_update::Result<()> {
        self.inner.lock().await
    }
    async fn release(&mut self) {
        self.inner.release().await;
    }
    async fn assert_idle(
        &mut self,
        processes: &[crate::codex_update::Process],
    ) -> crate::codex_update::Result<()> {
        self.inner.assert_idle(processes).await
    }
    async fn signal(
        &mut self,
        process: &crate::codex_update::Process,
        kill: bool,
    ) -> crate::codex_update::Result<()> {
        self.inner.signal(process, kill).await
    }
    async fn launch(
        &mut self,
        process: &crate::codex_update::Process,
        desktop: bool,
    ) -> crate::codex_update::Result<Launch> {
        let launch = self.inner.launch(process, desktop).await?;
        self.launches
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(launch)
    }
    async fn verify_daemon(
        &mut self,
        process: &crate::codex_update::Process,
    ) -> crate::codex_update::Result<()> {
        self.inner.verify_daemon(process).await
    }
    async fn remove_socket(&mut self) -> crate::codex_update::Result<()> {
        self.inner.remove_socket().await
    }
    fn now(&self) -> u64 {
        self.inner.now()
    }
    async fn sleep(&mut self, duration: Duration) {
        self.inner.sleep(duration).await;
    }
}

/// The real host behind [`Deps`] (the bridge's `bridgeDeps`).
#[cfg(target_os = "linux")]
pub(super) struct NativeDeps<'a> {
    cx: &'a Context,
    mode: BridgeMode,
    executable: PathBuf,
    marker: PathBuf,
    runtime: Runtime<Counting>,
    launches: std::sync::Arc<std::sync::atomic::AtomicU32>,
    old_proxies: Vec<crate::codex_update::Process>,
    supervised: Vec<crate::codex_update::Process>,
    epoch: std::time::Instant,
}

#[cfg(target_os = "linux")]
fn same(left: &crate::codex_update::Process, right: &crate::codex_update::Process) -> bool {
    left.pid == right.pid && left.start_time == right.start_time
}

#[cfg(target_os = "linux")]
impl<'a> NativeDeps<'a> {
    fn new(
        cx: &'a Context,
        mode: BridgeMode,
        executable: PathBuf,
        codex_home: PathBuf,
    ) -> Option<Self> {
        let initial = crate::codex_update::read_processes().ok()?;
        let releases = cx.host.home.join(".codex/packages/standalone/releases");
        let old_proxies: Vec<_> = initial
            .iter()
            .filter(|item| {
                item.args.iter().any(|arg| arg == "app-server")
                    && item.args.iter().any(|arg| arg == "proxy")
                    && (std::path::Path::new(&item.exe) == executable
                        || std::path::Path::new(&item.exe).starts_with(&releases))
            })
            .cloned()
            .collect();
        // OpenSSH's same-user session helper is deliberately non-dumpable; its kernel-owned
        // name and ancestry still show a proxy the desktop's SSH transport supervises.
        let supervised = old_proxies
            .iter()
            .filter(|item| {
                let mut visited = std::collections::HashSet::new();
                let mut parent = item.ppid;
                while parent != 0 && visited.insert(parent) {
                    let Some(process) = initial.iter().find(|row| row.pid == parent) else {
                        break;
                    };
                    let name = std::path::Path::new(&process.exe)
                        .file_name()
                        .map(|name| name.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    let rest = name
                        .strip_prefix("sshd-session")
                        .or_else(|| name.strip_prefix("sshd"));
                    if rest.is_some_and(|rest| {
                        rest.is_empty()
                            || rest
                                .strip_prefix(':')
                                .is_some_and(|tail| tail.starts_with(char::is_whitespace))
                    }) {
                        return true;
                    }
                    parent = process.ppid;
                }
                false
            })
            .cloned()
            .collect();
        let app = match mode {
            BridgeMode::Desktop => "codex-desktop",
            BridgeMode::Cli => "codex-cli",
        };
        let launches = std::sync::Arc::new(std::sync::atomic::AtomicU32::new(0));
        Some(Self {
            cx,
            mode,
            marker: super::restart_marker(cx, app),
            runtime: Runtime::new(
                codex_home.clone(),
                cx.host.home.clone(),
                Counting {
                    inner: Native::new(codex_home, executable.clone()),
                    launches: launches.clone(),
                },
            ),
            launches,
            executable,
            old_proxies,
            supervised,
            epoch: std::time::Instant::now(),
        })
    }

    async fn run(&self, argv: &[&std::ffi::OsStr], seconds: u64) -> Result<String, ()> {
        let spec = crate::apps::updater::CommandSpec::new(argv.iter().copied())
            .timeout(Duration::from_secs(seconds))
            .capture()
            .env("DEBIAN_FRONTEND", "noninteractive");
        self.cx
            .run(&spec)
            .await
            .map(|output| output.stdout)
            .map_err(drop)
    }

    fn alive(&self, targets: &[crate::codex_update::Process]) -> Vec<crate::codex_update::Process> {
        let current = crate::codex_update::read_processes().unwrap_or_default();
        targets
            .iter()
            .filter(|target| current.iter().any(|item| same(item, target)))
            .cloned()
            .collect()
    }
}

#[cfg(target_os = "linux")]
impl Deps for NativeDeps<'_> {
    fn now(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    async fn sleep(&mut self, millis: u64) {
        tokio::time::sleep(Duration::from_millis(millis)).await;
    }

    async fn read_version(&mut self) -> Result<String, ()> {
        match self.mode {
            BridgeMode::Desktop => {
                self.run(
                    &["/usr/bin/dpkg-query", "-W", "-f=${Version}", "chatgpt"]
                        .map(std::ffi::OsStr::new),
                    10,
                )
                .await
            }
            BridgeMode::Cli => {
                let executable = self.executable.clone();
                self.run(
                    &[executable.as_os_str(), std::ffi::OsStr::new("--version")],
                    10,
                )
                .await
            }
        }
    }

    async fn update(&mut self) -> Result<(), ()> {
        match self.mode {
            BridgeMode::Desktop => self
                .run(
                    &[
                        "/usr/bin/sudo",
                        "-n",
                        "/usr/bin/apt-get",
                        "install",
                        "--only-upgrade",
                        "-y",
                        "chatgpt",
                    ]
                    .map(std::ffi::OsStr::new),
                    180,
                )
                .await
                .map(drop),
            BridgeMode::Cli => {
                let executable = self.executable.clone();
                self.run(
                    &[executable.as_os_str(), std::ffi::OsStr::new("update")],
                    180,
                )
                .await
                .map(drop)
            }
        }
    }

    fn marked(&self) -> Option<Map<String, Value>> {
        match crate::apps::read_json(&self.marker, 64 * 1024) {
            Some(Value::Object(fields)) => Some(fields),
            _ => None,
        }
    }

    fn change(&mut self, change: &MarkerChange) -> bool {
        apply(&self.marker, change)
    }

    async fn restart_proxies(&mut self) -> Result<u32, ()> {
        if self.mode != BridgeMode::Cli {
            return Ok(0);
        }
        // The installed desktop SSH-WebSocket transport reconnects and creates a new fixed
        // `codex app-server proxy` whenever this one exits.
        let signal = |item: &crate::codex_update::Process, kill: bool| {
            if let Ok(pid) = i32::try_from(item.pid) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(pid),
                    if kill {
                        nix::sys::signal::Signal::SIGKILL
                    } else {
                        nix::sys::signal::Signal::SIGTERM
                    },
                );
            }
        };
        for target in self.alive(&self.supervised) {
            signal(&target, false);
        }
        let limit = std::time::Instant::now() + Duration::from_secs(5);
        while std::time::Instant::now() < limit && !self.alive(&self.supervised).is_empty() {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let mut forced = 0;
        for target in self.alive(&self.supervised) {
            signal(&target, true);
            forced += 1;
        }
        Ok(forced)
    }

    async fn has_old_proxies(&mut self) -> (bool, u32) {
        if self.mode != BridgeMode::Cli {
            return (false, 0);
        }
        let real = crate::apps::real(&self.executable);
        let limit = std::time::Instant::now() + Duration::from_secs(30);
        loop {
            let current = crate::codex_update::read_processes().unwrap_or_default();
            let old = self
                .old_proxies
                .iter()
                .any(|target| current.iter().any(|item| same(item, target)));
            let fresh = current
                .iter()
                .filter(|item| {
                    std::path::Path::new(&item.exe) == real
                        && item.args.iter().any(|arg| arg == "app-server")
                        && item.args.iter().any(|arg| arg == "proxy")
                })
                .count();
            if !old && fresh >= self.supervised.len() {
                return (
                    false,
                    u32::try_from(self.supervised.len()).unwrap_or(u32::MAX),
                );
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
            if std::time::Instant::now() >= limit {
                return (true, 0);
            }
        }
    }

    async fn stop(
        &mut self,
        record: &(dyn Fn(RestartPlan) -> MarkerChange + Sync),
    ) -> Result<(), StopError> {
        let marker = self.marker.clone();
        self.runtime
            .stop(|plan| {
                if apply(&marker, &record(plan)) {
                    Ok(())
                } else {
                    Err(crate::codex_update::Error::RestartFailed)
                }
            })
            .await
            .map_err(|error| match error {
                crate::codex_update::Error::Busy => StopError::Busy,
                crate::codex_update::Error::RestartFailed => StopError::Failed,
            })
    }

    fn stop_plan(&self) -> RestartPlan {
        self.runtime.stop_plan()
    }

    async fn retired_plan(&mut self) -> Result<RestartPlan, ()> {
        self.runtime.retired_plan().await.map_err(drop)
    }

    async fn restart_state(&mut self) -> Result<RestartState, ()> {
        self.runtime.restart_state().await.map_err(drop)
    }

    async fn verify_running_daemon(&mut self) -> Result<Option<Identity>, ()> {
        self.runtime.verify_running_daemon().await.map_err(drop)
    }

    async fn start(&mut self) -> Result<(), ()> {
        self.runtime.start().await.map_err(drop)
    }

    async fn dispose(&mut self) {
        self.runtime.dispose().await;
    }

    fn launches(&self) -> u32 {
        self.launches.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Apply a marker change to the marker file (the bridge's `writeMarker` / unlink).
pub(super) fn apply(marker: &std::path::Path, change: &MarkerChange) -> bool {
    match change {
        MarkerChange::Keep => true,
        MarkerChange::Clear => {
            let _ = std::fs::remove_file(marker);
            true
        }
        MarkerChange::Write(value) => write_private_json(marker, value).is_ok(),
    }
}
