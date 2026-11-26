//! Host update coordinator with durable results. Each app runs through its Rust
//! `AppUpdater` (`updater::runner`); Windows apps run in the interactive task.
use super::{
    updater::{
        Shared,
        runner::{self, Cancelled, Job, Outcome, Phases},
        windows_task,
    },
    *,
};
use aac_files::{BackupClass, Class, OwnedStore, Registry, StoreSpec};
use aac_protocol::{Event, Request};
use aac_providers::APP_INVENTORY;
use serde::{Deserialize, Serialize};
use std::{
    io::ErrorKind,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{io::AsyncWrite, sync::mpsc, task::JoinSet, time::Instant};

const MAX_JOURNAL: usize = 64 * 1024;
const DESKTOP: &str = "codex-desktop";
const ACP: &str = "antigravity-acp";
const TAILED: [&str; 2] = ["t3-code", ACP];
const MAX_PARALLEL: usize = 4;
const NEXT_VERSION: &str = "26.1113.36452.0";
#[cfg(feature = "parity-fixtures")]
const OLD_VERSION: &str = "26.1101.9.0";

#[derive(Clone)]
enum Installer {
    Native(Arc<Host>),
    #[cfg(feature = "parity-fixtures")]
    Fixture,
}

#[derive(Clone)]
struct Worker {
    root: PathBuf,
    helper_lock_root: PathBuf,
    installer: Installer,
}

pub(crate) async fn run(
    request: &Request,
    out: &mut (impl AsyncWrite + Unpin),
) -> Result<Value, Failure> {
    let platform = match request.args["platform"].as_str() {
        Some("ubuntu") => Platform::Ubuntu,
        Some("mac") => Platform::Mac,
        Some("windows") => Platform::Windows,
        _ => return Err(refused()),
    };
    let Some(job) = request.args["jobId"].as_str().filter(|job| valid_job(job)) else {
        return Err(refused());
    };
    let scope = match request.args.get("apps") {
        Some(value) => Some(scope(value).ok_or_else(refused)?),
        None => None,
    };
    let args = if scope.is_some() { 4 } else { 3 };
    if request
        .args
        .as_object()
        .is_none_or(|object| object.len() != args)
    {
        return Err(refused());
    }
    let Some(worker) = Worker::open(platform) else {
        return Ok(crate::proof::fallback());
    };
    match request.args["action"].as_str() {
        Some("update") => {
            worker
                .update(request, out, platform, job, scope.as_deref())
                .await
        }
        Some("cancel") if scope.is_none() => worker.cancel(platform, job),
        Some("acp-follow-up") if scope.is_none() && platform == Platform::Ubuntu => {
            worker.follow_up(job, request.deadline_ms).await
        }
        _ => Err(refused()),
    }
}

/// The optional app subset older dashboards never send. It is deliberately a
/// closed list of known app IDs, never an argument for the installer.
fn scope(value: &Value) -> Option<Vec<&'static str>> {
    let listed = value.as_array()?;
    if listed.is_empty() || listed.len() > APP_INVENTORY.len() {
        return None;
    }
    let mut apps: Vec<&'static str> = Vec::with_capacity(listed.len());
    for app in listed {
        let app = app.as_str()?;
        let app = APP_INVENTORY.iter().copied().find(|known| *known == app)?;
        if apps.contains(&app) {
            return None;
        }
        apps.push(app);
    }
    Some(apps)
}

impl Worker {
    fn open(platform: Platform) -> Option<Self> {
        #[cfg(feature = "parity-fixtures")]
        if let Some(root) = std::env::var_os("AAC_APPS_FIXTURE_ROOT").map(PathBuf::from)
            && root.is_absolute()
            && read_text(&root.join("app-updates-fixture"), 64)
                .is_some_and(|marker| marker.trim() == "AAC fixture installers v1")
        {
            return Some(Self {
                helper_lock_root: root.clone(),
                root,
                installer: Installer::Fixture,
            });
        }
        let host = Platform::native()
            .filter(|native| *native == platform)
            .and_then(Host::native)?;
        let helper_lock_root = host.home.join(".ccs").join("app-updates");
        let root = std::env::var_os("AAC_APP_UPDATE_STATE_DIR")
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .unwrap_or_else(|| helper_lock_root.clone());
        let installer = Installer::Native(Arc::new(host));
        Some(Self {
            root,
            helper_lock_root,
            installer,
        })
    }

    fn fixture(&self) -> bool {
        match self.installer {
            Installer::Native(_) => false,
            #[cfg(feature = "parity-fixtures")]
            Installer::Fixture => true,
        }
    }

    fn name(&self, suffix: &str) -> String {
        format!(
            "{}-{suffix}",
            if self.fixture() { "fixture" } else { "agent" }
        )
    }

    async fn update(
        &self,
        request: &Request,
        out: &mut (impl AsyncWrite + Unpin),
        platform: Platform,
        job: &str,
        scope: Option<&[&'static str]>,
    ) -> Result<Value, Failure> {
        // The same kernel lease as the old helper covers every app run and the
        // deferred T3 restarter. The journal has its own writer lease.
        let _host_lease = claim(&self.helper_lock_root, "helper-update")?;
        let journal_store = claim(&self.root, &self.name("journal.json"))?;
        let saved = self.journal()?;
        if !self.fixture() {
            for app in APP_INVENTORY {
                // An orphaned supervisor still owns this lease while it reaps children.
                claim(&self.root, &format!("agent-worker-{app}"))?;
            }
        }
        // Know exactly which partial belongs to this installer. Never search
        // for or remove executables, installed versions or other scratch files.
        if self.fixture() {
            self.clear_partial()?;
        }
        let expected = expected(platform, scope);
        if let Some(mut journal) = saved
            && journal.job == job
        {
            if journal.platform != platform.name() || !journal.same_scope(scope) {
                return Err(refused());
            }
            // A foreground owner disappeared. Preserve known results and
            // close missing rows as unknown. An explicit new job can try again.
            for &app in &expected {
                if !journal.results.contains_key(app) {
                    journal.finish(
                        app,
                        result(platform, app, "unknown", "host_unknown", None, None, None),
                    );
                }
            }
            journal.save(&journal_store)?;
            return Ok(journal.payload(platform));
        }

        let mut journal = Journal {
            job: job.into(),
            platform: platform.name().into(),
            results: serde_json::Map::new(),
            running: Vec::new(),
            apps: scope.map(|_| expected.iter().map(|app| (*app).to_owned()).collect()),
        };
        journal.save(&journal_store)?;
        let mut progress = Progress {
            id: &request.id,
            out,
            active: Vec::new(),
            visible: None,
        };
        progress.app(None, "checking").await?;
        let all_installed = !self.fixture() || self.root.join("fixture-install-all").is_file();
        // Leave time for a timed-out app run to stop its commands, flush its
        // terminal row and settle the host journal before the wire deadline.
        let deadline = Instant::now()
            + Duration::from_millis(
                request
                    .deadline_ms
                    .saturating_sub((request.deadline_ms / 10).min(12_000))
                    .min(1_800_000),
            );
        let (phase_tx, mut phases) = mpsc::unbounded_channel();
        // One run's cross-app state (Python module globals such as UBUNTU_RUN).
        let shared = Arc::new(Shared::default());
        let mut pending = expected;
        let mut workers = JoinSet::new();
        while !pending.is_empty() || !workers.is_empty() {
            let mut position = 0;
            while position < pending.len() && workers.len() < MAX_PARALLEL {
                let app = pending[position];
                let cancelled = self.cancelled(job)?;
                let installed_here = all_installed || app == DESKTOP;
                let needed = if installed_here && !cancelled {
                    self.resources(platform, app)
                } else {
                    0
                };
                if !ready_to_start(app, platform, &journal)
                    || journal
                        .running
                        .iter()
                        .any(|active| needed & self.resources(platform, active) != 0)
                {
                    position += 1;
                    continue;
                }
                pending.remove(position);
                if cancelled || !installed_here || Instant::now() >= deadline && !self.fixture() {
                    let row = if cancelled {
                        result(
                            platform,
                            app,
                            "skipped",
                            "skipped_cancelled",
                            None,
                            None,
                            None,
                        )
                    } else if !installed_here {
                        result(
                            platform,
                            app,
                            "not_installed",
                            "not_installed",
                            None,
                            None,
                            None,
                        )
                    } else {
                        result(platform, app, "unknown", "host_timeout", None, None, None)
                    };
                    journal.finish(app, row.clone());
                    journal.save(&journal_store)?;
                    progress.result(app, row).await?;
                    continue;
                }
                journal.running.push(app.into());
                journal.save(&journal_store)?;
                if self.fixture() {
                    progress.app(Some(app), "checking").await?;
                    progress.app(Some(app), "updating").await?;
                    if app == DESKTOP && !self.installed(app)? {
                        self.start_download()?;
                        progress.app(Some(app), "downloading").await?;
                    }
                }
                let installer = self.clone();
                let phases = phase_tx.clone();
                let job = job.to_owned();
                let t3_row = journal.results.get("t3-code").cloned();
                let shared = shared.clone();
                workers.spawn(async move {
                    let update = installer
                        .run_app(app, platform, &job, deadline, t3_row, shared, phases)
                        .await;
                    let row = match update {
                        Ok(row) => row,
                        Err(failure) => {
                            if installer.cancelled(&job).unwrap_or(false) {
                                result(
                                    platform,
                                    app,
                                    "skipped",
                                    "skipped_cancelled",
                                    None,
                                    None,
                                    None,
                                )
                            } else if failure.cause == Cause::Timeout {
                                result(platform, app, "unknown", "host_timeout", None, None, None)
                            } else {
                                result(platform, app, "failed", "helper_invalid", None, None, None)
                            }
                        }
                    };
                    (app, row)
                });
            }
            let finished = loop {
                tokio::select! {
                    biased;
                    Some((app, phase)) = phases.recv() => progress.app(Some(app), phase).await?,
                    result = workers.join_next() => break result,
                }
            };
            if let Some(result) = finished {
                let (app, row) =
                    result.map_err(|_| Failure::new(Cause::Bug, "app installer worker failed"))?;
                journal.finish(app, row.clone());
                journal.save(&journal_store)?;
                progress.result(app, row).await?;
            } else if !pending.is_empty() {
                return Err(Failure::new(Cause::Bug, "app update dependencies stalled"));
            }
        }
        Ok(journal.payload(platform))
    }

    async fn follow_up(&self, job: &str, deadline_ms: u64) -> Result<Value, Failure> {
        let platform = Platform::Ubuntu;
        // Claim only the journal. The detached restart needs the host lease and
        // must continue to see the completed first pass while the helper waits.
        let store = claim(&self.root, &self.name("journal.json"))?;
        let Some(mut journal) = self.journal()?.filter(|journal| {
            journal.job == job && journal.platform == platform.name() && journal.acp_pending()
        }) else {
            return Err(refused());
        };
        let row = if self.cancelled(job)? {
            result(
                platform,
                ACP,
                "skipped",
                "skipped_cancelled",
                None,
                None,
                None,
            )
        } else {
            // A deferred ACP row only follows a T3 restart this host journal
            // recorded. A subset without T3 cannot supply that version.
            let Some(t3) = journal.results.get("t3-code") else {
                return Err(refused());
            };
            let version = follow_up_version(t3);
            let budget = Duration::from_millis(deadline_ms.saturating_sub(12_000))
                .min(Duration::from_millis(
                    aac_protocol::ACP_FOLLOW_UP_DEADLINE_MS,
                ))
                .max(Duration::from_secs(1));
            let outcome = match &self.installer {
                Installer::Native(host) => {
                    let expected = (version != "any").then_some(version);
                    runner::follow_up(
                        Job {
                            host: host.clone(),
                            state: &self.root,
                            platform,
                            shared: Arc::new(Shared::default()),
                            deadline: Instant::now() + budget,
                            t3_row: Some(t3.clone()),
                            cancelled: self.cancel_check(job),
                        },
                        expected,
                    )
                    .await
                }
                #[cfg(feature = "parity-fixtures")]
                Installer::Fixture => return Err(refused()),
            };
            match outcome {
                Outcome::Row(row)
                    if row["appId"] == ACP
                        && row["platform"] == platform.name()
                        && row["status"].is_string()
                        && row["messageCode"] != "t3_restart_pending" =>
                {
                    row
                }
                Outcome::TimedOut => {
                    result(platform, ACP, "unknown", "host_timeout", None, None, None)
                }
                Outcome::Row(_) => {
                    result(platform, ACP, "failed", "helper_invalid", None, None, None)
                }
            }
        };
        journal.finish(ACP, row.clone());
        journal.save(&store)?;
        Ok(json!({"results":[row]}))
    }

    fn resources(&self, platform: Platform, app: &str) -> u8 {
        // Windows GUI work shares the pre-registered InteractiveToken task.
        // Its nonce and output file admit one request at a time.
        if platform == Platform::Windows && !self.fixture() {
            16
        } else {
            resources(platform, app)
        }
    }

    /// The job's cancel file as a check the runner polls. An unreadable marker
    /// counts as cancelled: stopping is the safe answer for an installer.
    fn cancel_check(&self, job: &str) -> Cancelled {
        let worker = self.clone();
        let job = job.to_owned();
        Arc::new(move || worker.cancelled(&job).unwrap_or(true))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "one app run carries its job, deadline, dependency row and progress"
    )]
    async fn run_app(
        &self,
        app: &'static str,
        platform: Platform,
        job: &str,
        deadline: Instant,
        t3_row: Option<Value>,
        shared: Arc<Shared>,
        phases: Phases,
    ) -> Result<Value, Failure> {
        match &self.installer {
            Installer::Native(host) => {
                let cancelled = self.cancel_check(job);
                let outcome = if platform == Platform::Windows {
                    let agent = std::env::current_exe()
                        .map_err(|_| Failure::new(Cause::Bug, "agent path unknown"))?;
                    windows_task::coordinate(
                        host,
                        &agent,
                        app,
                        deadline.saturating_duration_since(Instant::now()),
                        cancelled,
                        phases,
                        &windows_task::PowerShell(updater::shared::powershell_path(host)),
                    )
                    .await
                } else {
                    let job = Job {
                        host: host.clone(),
                        state: &self.root,
                        platform,
                        shared,
                        deadline,
                        t3_row,
                        cancelled,
                    };
                    runner::run_app(app, job, phases).await
                };
                match outcome {
                    Outcome::Row(row)
                        if row["appId"] == app
                            && row["platform"] == platform.name()
                            && row["status"].is_string() =>
                    {
                        Ok(row)
                    }
                    Outcome::Row(_) => Err(Failure::new(Cause::Damaged, "invalid app row")),
                    Outcome::TimedOut => {
                        Err(Failure::new(Cause::Timeout, "app installer deadline"))
                    }
                }
            }
            #[cfg(feature = "parity-fixtures")]
            Installer::Fixture => {
                self.fixture_install(platform, app, self.installed(app)?)
                    .await
            }
        }
    }

    fn installed(&self, app: &str) -> Result<bool, Failure> {
        match read::read(&self.root.join(receipt(app)), &ReadPolicy::capped(64)) {
            Read::Absent => Ok(false),
            Read::Bytes(bytes, _) if bytes == NEXT_VERSION.as_bytes() => Ok(true),
            _ => Err(damaged()),
        }
    }

    fn start_download(&self) -> Result<(), Failure> {
        claim(&self.root, "fixture-download.part")?.write(b"fixture package")
    }

    fn clear_partial(&self) -> Result<(), Failure> {
        match std::fs::remove_file(self.root.join("fixture-download.part")) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    #[cfg(feature = "parity-fixtures")]
    async fn fixture_install(
        &self,
        platform: Platform,
        app: &str,
        installed: bool,
    ) -> Result<Value, Failure> {
        let (status, before) = if installed {
            ("current", NEXT_VERSION)
        } else {
            if app == DESKTOP {
                // A fixture can stop at the download boundary for cancel/kill tests.
                while self.root.join("fixture-hold-download").is_file()
                    && !self.root.join("fixture-release-download").is_file()
                {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
            claim(&self.root, &receipt(app))?.write(NEXT_VERSION.as_bytes())?;
            if app == DESKTOP {
                self.clear_partial()?;
            }
            ("updated", OLD_VERSION)
        };
        Ok(result(
            platform,
            app,
            status,
            status,
            Some(before),
            Some(NEXT_VERSION),
            Some(manager(platform, app)),
        ))
    }

    fn cancelled(&self, job: &str) -> Result<bool, Failure> {
        let path = self.root.join(self.name(&format!("cancel-{job}.json")));
        match read::read(&path, &ReadPolicy::capped(4)) {
            Read::Absent => Ok(false),
            Read::Bytes(bytes, _) if bytes == b"true" => Ok(true),
            _ => Err(damaged()),
        }
    }

    fn journal(&self) -> Result<Option<Journal>, Failure> {
        match read::read(
            &self.root.join(self.name("journal.json")),
            &ReadPolicy::capped(MAX_JOURNAL as u64),
        ) {
            Read::Absent => Ok(None),
            Read::Bytes(bytes, _) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|_| damaged()),
            _ => Err(damaged()),
        }
    }
    fn cancel(&self, platform: Platform, job: &str) -> Result<Value, Failure> {
        let active = self.journal()?.is_some_and(|journal| {
            journal.job == job
                && journal.platform == platform.name()
                && (!journal.complete(platform) || journal.acp_pending())
        });
        if active {
            claim(&self.root, &self.name(&format!("cancel-{job}.json")))?.write(b"true")?;
        }
        Ok(json!({"cancelled": active}))
    }
}

fn follow_up_version(row: &Value) -> &str {
    static NIGHTLY: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}\.[0-9]+\z")
            .expect("T3 nightly version")
    });
    row["version"]
        .as_str()
        .filter(|version| {
            row["status"] == "updated" && version.len() <= 64 && NIGHTLY.is_match(version)
        })
        .unwrap_or("any")
}

fn receipt(app: &str) -> String {
    format!("fixture-installed-{app}.txt")
}

fn manager(platform: Platform, app: &str) -> &'static str {
    match app {
        "codex-cli" | "t3-acp-adapters" => "npm",
        "codex-desktop" | "claude-desktop" => match platform {
            Platform::Windows => "msix",
            Platform::Mac => "official-download",
            Platform::Ubuntu => "apt",
        },
        "zcode" | "t3-code" | "antigravity-acp" => "official-download",
        _ => "native",
    }
}

fn resources(platform: Platform, app: &str) -> u8 {
    const NPM: u8 = 1;
    const CODEX: u8 = 2;
    const APT: u8 = 4;
    const MSIX: u8 = 8;
    let manager_lock = match manager(platform, app) {
        "npm" => NPM,
        "apt" => APT,
        "msix" => MSIX,
        _ if platform == Platform::Ubuntu && app == "zcode" => NPM,
        _ => 0,
    };
    manager_lock
        | if matches!(app, "codex-cli" | DESKTOP) {
            CODEX
        } else {
            0
        }
}

fn ordered(platform: Platform) -> Vec<&'static str> {
    let mut order: Vec<_> = APP_INVENTORY
        .into_iter()
        .filter(|app| *app != DESKTOP && !TAILED.contains(app))
        .collect();
    if platform == Platform::Ubuntu {
        order.push(DESKTOP);
        order.extend(TAILED);
    } else {
        order.extend(TAILED);
        order.push(DESKTOP);
    }
    order
}

/// The requested apps in dispatch order. Without a request scope (older dashboards)
/// this is the fixed full sweep, exactly as before.
fn expected(platform: Platform, scope: Option<&[&'static str]>) -> Vec<&'static str> {
    ordered(platform)
        .into_iter()
        .filter(|app| scope.is_none_or(|scope| scope.contains(app)))
        .collect()
}

fn ready_to_start(app: &str, platform: Platform, journal: &Journal) -> bool {
    // A prerequisite outside this job's scope was not asked to run again, so it
    // must not hold the requested apps back. Within the scope the order stays.
    let scoped = |id: &str| {
        journal
            .apps
            .as_ref()
            .is_none_or(|apps| apps.iter().any(|saved| saved.as_str() == id))
    };
    let done = |id: &str| !scoped(id) || journal.results.contains_key(id);
    match app {
        DESKTOP => done("codex-cli"),
        "t3-acp-adapters" => done("zcode"),
        "antigravity-acp" => done("t3-code"),
        "t3-code" => APP_INVENTORY
            .into_iter()
            .filter(|id| !TAILED.contains(id) && (*id != DESKTOP || platform == Platform::Ubuntu))
            .all(done),
        _ => true,
    }
}

fn refused() -> Failure {
    Failure::new(Cause::Refused, "invalid app update request")
}

fn damaged() -> Failure {
    Failure::new(Cause::Damaged, "invalid app update journal")
}

fn claim(root: &Path, path: &str) -> Result<OwnedStore, Failure> {
    let mut registry = Registry::default();
    registry.register(StoreSpec {
        path: path.into(),
        class: Class::State,
        mode: 0o600,
        max_size: MAX_JOURNAL,
        owner: "app-updates".into(),
        schema: "app-updates-v1".into(),
        backup: BackupClass::Rebuild,
    })?;
    registry.claim(root, Path::new(path), "app-updates")
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    job: String,
    platform: String,
    results: serde_json::Map<String, Value>,
    running: Vec<String>,
    /// The requested apps in the platform's dispatch order. A journal saved by an
    /// older agent (or a full sweep) has none, and that keeps the full sweep.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    apps: Option<Vec<String>>,
}

impl Journal {
    fn same_scope(&self, scope: Option<&[&'static str]>) -> bool {
        match (&self.apps, scope) {
            (None, None) => true,
            (Some(saved), Some(incoming)) => {
                saved.len() == incoming.len()
                    && incoming
                        .iter()
                        .all(|app| saved.iter().any(|saved| saved.as_str() == *app))
            }
            _ => false,
        }
    }

    fn expected(&self, platform: Platform) -> Vec<&'static str> {
        let recorded = self.apps.as_deref();
        ordered(platform)
            .into_iter()
            .filter(|app| {
                recorded.is_none_or(|apps| apps.iter().any(|saved| saved.as_str() == *app))
            })
            .collect()
    }

    fn complete(&self, platform: Platform) -> bool {
        self.expected(platform)
            .iter()
            .all(|app| self.results.contains_key(*app))
    }

    fn acp_pending(&self) -> bool {
        self.platform == "ubuntu"
            && self.running.is_empty()
            && self.expected(Platform::Ubuntu).contains(&ACP)
            && self.complete(Platform::Ubuntu)
            && self.results.contains_key("t3-code")
            && self.results.get(ACP).is_some_and(|row| {
                row["status"] == "action_required" && row["messageCode"] == "t3_restart_pending"
            })
    }

    fn save(&self, store: &OwnedStore) -> Result<(), Failure> {
        store.write(&serde_json::to_vec(self).map_err(|_| damaged())?)
    }

    fn finish(&mut self, app: &str, row: Value) {
        self.running.retain(|id| id != app);
        self.results.insert(app.into(), row);
    }

    fn payload(&self, platform: Platform) -> Value {
        json!({"results": self.expected(platform).into_iter().filter_map(|app| self.results.get(app)).collect::<Vec<_>>()})
    }
}

fn result(
    platform: Platform,
    app: &str,
    status: &str,
    code: &str,
    before: Option<&str>,
    after: Option<&str>,
    manager: Option<&str>,
) -> Value {
    let diagnostic = if matches!(status, "unknown" | "failed") {
        "unknown"
    } else {
        "none"
    };
    json!({
        "appId":app, "platform":platform.name(), "status":status,
        "previousVersion":before, "version":after, "manager":manager,
        "messageCode":code, "updateAttempted":manager.is_some(), "restartedProcesses":0,
        "diagnostics":{"cause":diagnostic}
    })
}

struct Progress<'a, W> {
    id: &'a str,
    out: &'a mut W,
    visible: Option<&'static str>,
    active: Vec<(&'static str, &'static str)>,
}

impl<W: AsyncWrite + Unpin> Progress<'_, W> {
    async fn emit(&mut self, data: Value) -> Result<(), Failure> {
        crate::write_event(
            self.out,
            &Event::Progress {
                protocol: aac_protocol::PROTOCOL_VERSION,
                id: self.id.into(),
                data,
            },
            aac_protocol::MAX_FRAME_BYTES,
        )
        .await
    }

    async fn app(&mut self, app: Option<&'static str>, phase: &'static str) -> Result<(), Failure> {
        if let Some(app) = app {
            if let Some(entry) = self.active.iter_mut().find(|(id, _)| id == &app) {
                entry.1 = phase;
            } else {
                self.active.push((app, phase));
            }
        }
        self.visible = app;
        self.emit(json!({"event":"app","appId":app,"phase":phase}))
            .await
    }

    async fn result(&mut self, app: &str, row: Value) -> Result<(), Failure> {
        self.active.retain(|(id, _)| id != &app);
        self.emit(json!({"event":"result","result":row})).await?;
        if self.visible == Some(app)
            && let Some(&(app, phase)) = self.active.last()
        {
            self.app(Some(app), phase).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(platform: Platform, app: &str, status: &str, code: &str) -> Value {
        result(platform, app, status, code, None, None, None)
    }

    fn saved(platform: Platform, apps: Option<&[&str]>) -> Journal {
        Journal {
            job: "job".into(),
            platform: platform.name().into(),
            results: serde_json::Map::new(),
            running: Vec::new(),
            apps: apps.map(|apps| apps.iter().map(|app| (*app).to_owned()).collect()),
        }
    }

    #[test]
    fn a_scope_is_a_closed_nonempty_unique_app_list() {
        assert_eq!(scope(&json!([])), None);
        assert_eq!(scope(&json!(["omp", "omp"])), None);
        assert_eq!(scope(&json!(["not-an-app"])), None);
        assert_eq!(scope(&json!(["omp", 8])), None);
        assert_eq!(scope(&json!(["zcode", "omp"])), Some(vec!["zcode", "omp"]));
    }

    #[test]
    fn a_subset_journal_completes_on_its_own_apps() {
        let mut journal = saved(Platform::Ubuntu, Some(&["zcode", "antigravity-cli"]));
        assert!(!journal.complete(Platform::Ubuntu));
        journal.finish(
            "zcode",
            row(Platform::Ubuntu, "zcode", "current", "current"),
        );
        assert!(!journal.complete(Platform::Ubuntu));
        journal.finish(
            "antigravity-cli",
            row(Platform::Ubuntu, "antigravity-cli", "updated", "updated"),
        );
        assert!(journal.complete(Platform::Ubuntu));
        let rows = journal.payload(Platform::Ubuntu);
        assert_eq!(rows["results"].as_array().expect("rows").len(), 2);
        assert_eq!(rows["results"][0]["appId"], "antigravity-cli");
    }

    #[test]
    fn acp_follow_up_requires_acp_inside_the_scope() {
        let mut journal = saved(Platform::Ubuntu, Some(&["t3-code"]));
        journal.finish(
            "t3-code",
            row(
                Platform::Ubuntu,
                "t3-code",
                "updated",
                "t3_restart_scheduled",
            ),
        );
        assert!(!journal.acp_pending());
        journal.apps = Some(vec!["t3-code".into(), ACP.into()]);
        journal.finish(
            ACP,
            row(
                Platform::Ubuntu,
                ACP,
                "action_required",
                "t3_restart_pending",
            ),
        );
        assert!(journal.acp_pending());
        journal.apps = None;
        assert!(
            !journal.acp_pending(),
            "a full journal still needs every app"
        );
        for app in APP_INVENTORY {
            if !journal.results.contains_key(app) {
                journal.finish(app, row(Platform::Ubuntu, app, "current", "current"));
            }
        }
        journal.finish(
            ACP,
            row(
                Platform::Ubuntu,
                ACP,
                "action_required",
                "t3_restart_pending",
            ),
        );
        assert!(journal.acp_pending());
    }

    #[test]
    fn subset_readiness_ignores_prerequisites_outside_it() {
        let journal = saved(Platform::Mac, Some(&[DESKTOP]));
        assert!(ready_to_start(DESKTOP, Platform::Mac, &journal));
        let mut journal = saved(Platform::Mac, Some(&[DESKTOP, "codex-cli"]));
        assert!(!ready_to_start(DESKTOP, Platform::Mac, &journal));
        journal.finish(
            "codex-cli",
            row(Platform::Mac, "codex-cli", "current", "current"),
        );
        assert!(ready_to_start(DESKTOP, Platform::Mac, &journal));
        let journal = saved(Platform::Ubuntu, Some(&["t3-code"]));
        assert!(ready_to_start("t3-code", Platform::Ubuntu, &journal));
        let journal = saved(Platform::Ubuntu, Some(&["t3-code", ACP]));
        assert!(!ready_to_start(ACP, Platform::Ubuntu, &journal));
    }

    #[test]
    fn a_recovery_request_must_repeat_the_same_scope() {
        let journal = saved(Platform::Ubuntu, Some(&["zcode"]));
        assert!(journal.same_scope(Some(&["zcode"])));
        assert!(!journal.same_scope(Some(&["omp"])));
        assert!(!journal.same_scope(None));
        let full = saved(Platform::Ubuntu, None);
        assert!(full.same_scope(None));
        assert!(!full.same_scope(Some(&["zcode"])));
    }
}
