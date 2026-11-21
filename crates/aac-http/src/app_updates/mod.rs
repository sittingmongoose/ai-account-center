//! App-update job API for the guarded HTTP routes. One job fans out over L5.
mod helpers;
mod rows;
mod storage;

use crate::agent_transport::Selector;
#[cfg(unix)]
use crate::feed::Feeds;
use crate::public_auth::{AppUpdateHostProgress, AppUpdateProgress, DashboardEvent};
use aac_protocol::{
    ACP_FOLLOW_UP_HOST_TIMEOUT_MS, APP_UPDATE_ADMISSION, APP_UPDATE_ADMIT,
    APP_UPDATE_HOST_TIMEOUT_MS, Event, MAX_FRAME_BYTES,
};
use aac_transport::{CallOptions, CancellationToken};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::{
    path::PathBuf,
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{
    sync::{Mutex, mpsc},
    task::JoinSet,
    time::Instant,
};

const ACP: &str = "antigravity-acp";

#[derive(Clone, Copy)]
struct Host {
    id: &'static str,
    os: &'static str,
    alias: Option<&'static str>,
    legacy: bool,
}

static TABLE: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!(
        "../../../../web-dashboard/public/computers.json"
    ))
    .expect("packaged computer table")
});
static LEGACY: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../../config/tables/hosts.json"))
        .expect("packaged legacy hosts")
});

fn hosts() -> impl Iterator<Item = Host> {
    TABLE
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(id, row)| {
            let native = row["os"].as_str()?;
            Some(Host {
                id,
                os: if native == "linux" { "ubuntu" } else { native },
                alias: row["sshAlias"].as_str(),
                legacy: LEGACY["legacyStorageHosts"]
                    .as_array()
                    .is_some_and(|rows| rows.iter().any(|row| row == id)),
            })
        })
}

fn expected_results() -> usize {
    hosts().count() * rows::app_ids().count()
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Job {
    id: String,
    state: String,
    started_at: String,
    finished_at: Option<String>,
    active_platform: Option<String>,
    hosts: Option<Map<String, Value>>,
    cancel_requested: bool,
    results: Vec<Value>,
    expected_results: Option<usize>,
}

impl Job {
    fn new(id: String, now: String) -> Self {
        Self {
            id,
            state: "running".into(),
            started_at: now,
            finished_at: None,
            active_platform: None,
            hosts: Some(
                hosts()
                    .map(|host| (host.id.to_owned(), host_progress("waiting")))
                    .collect(),
            ),
            cancel_requested: false,
            results: vec![],
            expected_results: Some(expected_results()),
        }
    }

    fn running(&self) -> bool {
        self.state == "running"
    }

    fn owned_host(&mut self, host: Host) -> Option<&mut Value> {
        self.hosts.as_mut()?.get_mut(host.id)
    }

    fn refresh_active(&mut self) {
        self.active_platform = hosts()
            .find(|host| {
                self.hosts
                    .as_ref()
                    .and_then(|map| map.get(host.id))
                    .is_some_and(|state| state["state"] == "running")
            })
            .map(|host| host.id.to_owned());
    }

    fn has(&self, host: Host, app: &str) -> Option<usize> {
        self.results
            .iter()
            .position(|row| row["platform"] == host.id && row["appId"] == app)
    }

    fn fill(&mut self, host: Host, status: &str, code: &str) {
        for app in rows::app_ids() {
            if self.has(host, app).is_none() {
                self.results.push(rows::blank(host, app, status, code));
            }
        }
    }

    fn fill_sync_failure(&mut self, host: Host, reason: &str, cause: &str) {
        for app in rows::app_ids() {
            if self.has(host, app).is_none() {
                self.results
                    .push(rows::sync_failure(host, app, reason, cause));
            }
        }
    }

    fn event(&mut self, host: Host, data: &Value) {
        match data["event"].as_str() {
            Some("app") => {
                let id = &data["appId"];
                let phase = data["phase"].as_str();
                if !(id.is_null() || id.as_str().is_some_and(rows::app_known))
                    || !matches!(phase, Some("checking" | "updating" | "downloading"))
                {
                    return;
                }
                if let Some(state) = self.owned_host(host) {
                    if state["currentApp"] != *id || state["phase"] != data["phase"] {
                        state["phaseSince"] = json!(clock());
                    }
                    state["currentApp"] = id.clone();
                    state["phase"] = data["phase"].clone();
                }
            }
            Some("result") => {
                let row = &data["result"];
                let Some(app) = row["appId"].as_str().filter(|app| rows::app_known(app)) else {
                    return;
                };
                if self.has(host, app).is_none() {
                    self.results.push(
                        rows::normalize(host, app, row)
                            .unwrap_or_else(|| rows::blank(host, app, "failed", "helper_invalid")),
                    );
                    if let Some(state) = self.owned_host(host)
                        && state["currentApp"] == app
                    {
                        state["currentApp"] = Value::Null;
                    }
                }
            }
            _ => {}
        }
    }

    fn finish_host(&mut self, host: Host, terminal: Option<&Event>, timeout: bool) {
        match terminal {
            Some(Event::Result { value, .. }) if value["fallback"] == "python" => {
                // The agent found no capable helper under its own account after the sync.
                self.fill_sync_failure(host, "helper_capability_missing", "unknown")
            }
            Some(Event::Result { value, .. }) => {
                for app in rows::app_ids() {
                    let mut candidates = value["results"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter(|row| row["appId"] == app);
                    let row = candidates.next().filter(|_| candidates.next().is_none());
                    let valid = row.and_then(|row| rows::normalize(host, app, row));
                    if let Some(index) = self.has(host, app) {
                        if let Some(valid) = valid
                            && !row.is_some_and(|row| {
                                row["status"] == "unknown"
                                    && matches!(
                                        row["messageCode"].as_str(),
                                        Some("host_timeout" | "task_failed")
                                    )
                            })
                        {
                            self.results[index] = valid;
                        }
                    } else {
                        self.results.push(
                            valid.unwrap_or_else(|| {
                                rows::blank(host, app, "failed", "helper_invalid")
                            }),
                        );
                    }
                }
            }
            Some(Event::Failure { failure, .. }) if failure.cause == aac_core::Cause::Busy => {
                self.fill(host, "failed", "busy")
            }
            Some(Event::Failure { failure, .. }) if failure.cause == aac_core::Cause::Timeout => {
                self.fill(host, "unknown", "host_timeout")
            }
            _ => self.fill(
                host,
                "unknown",
                if timeout {
                    "host_timeout"
                } else {
                    "host_unknown"
                },
            ),
        }
        self.host_done(host);
    }

    fn pending_acp(&self, host: Host) -> Option<usize> {
        self.has(host, ACP)
            .filter(|&index| host.os == "ubuntu" && rows::is_acp_deferred(&self.results[index]))
    }

    fn finish_acp(&mut self, host: Host, terminal: &HostCall) {
        let Some(index) = self.pending_acp(host) else {
            return;
        };
        let setup_message = terminal.setup_message();
        self.results[index] = match terminal.event() {
            Some(Event::Result { value, .. })
                if terminal.admitted && value["fallback"] != "python" =>
            {
                value["results"]
                    .as_array()
                    .filter(|results| results.len() == 1)
                    .and_then(|results| rows::normalize(host, ACP, &results[0]))
                    .filter(|row| !rows::is_acp_deferred(row))
                    .unwrap_or_else(|| rows::blank(host, ACP, "failed", "helper_invalid"))
            }
            _ if !terminal.admitted && self.cancel_requested => {
                rows::blank(host, ACP, "skipped", "skipped_cancelled")
            }
            _ => match setup_message {
                Some(message) => rows::blank(host, ACP, "failed", message),
                None => rows::blank(
                    host,
                    ACP,
                    "unknown",
                    if terminal.timed_out() {
                        "host_timeout"
                    } else {
                        "host_unknown"
                    },
                ),
            },
        };
    }

    fn host_done(&mut self, host: Host) {
        if let Some(state) = self.owned_host(host) {
            *state = host_progress("done");
        }
        self.refresh_active();
    }

    fn finish(&mut self) {
        let failed = self.results.iter().any(|row| {
            matches!(
                row["status"].as_str(),
                Some("failed" | "restart_failed" | "unknown")
            )
        });
        self.state = if failed { "failed" } else { "completed" }.into();
        self.cancel_requested &= self.results.iter().any(|row| row["status"] == "skipped");
        self.active_platform = None;
        self.finished_at = Some(clock());
    }
}

fn host_progress(state: &str) -> Value {
    json!({"state":state,"currentApp":null,"phase":null})
}

fn clock() -> String {
    chrono::DateTime::<chrono::Utc>::from(std::time::SystemTime::now())
        .to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

fn job_id() -> Result<(String, String), aac_core::Failure> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|_| aac_core::Failure::new(aac_core::Cause::Bug, "job randomness unavailable"))?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let id = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    Ok((
        format!(
            "{}-{}-{}-{}-{}",
            &id[..8],
            &id[8..12],
            &id[12..16],
            &id[16..20],
            &id[20..]
        ),
        id,
    ))
}

/// Status code and body matching the existing dashboard route contract.
pub struct AppUpdateReply {
    /// HTTP status to use after the route's session and request guards.
    pub status: u16,
    /// Public job envelope. Contains only normalized rows and progress.
    pub body: Value,
}

fn reply(status: u16, body: Value) -> AppUpdateReply {
    AppUpdateReply { status, body }
}

struct HostCall {
    admitted: bool,
    terminal: Result<Vec<Event>, aac_core::Failure>,
    until: Instant,
}

impl HostCall {
    fn event(&self) -> Option<&Event> {
        self.terminal.as_ref().ok()?.last()
    }

    fn setup_message(&self) -> Option<&'static str> {
        if self.admitted {
            None
        } else if self.terminal.as_ref().is_err_and(|error| {
            matches!(
                error.cause,
                aac_core::Cause::AgentMissing | aac_core::Cause::AgentMismatch
            )
        }) {
            Some("agent_setup_required")
        } else if matches!(
            self.event(),
            Some(Event::Failure { failure, .. })
                if failure.cause == aac_core::Cause::Refused
                    && failure.detail.as_str() == "invalid apps action"
        ) {
            Some("agent_update_required")
        } else {
            None
        }
    }

    fn timed_out(&self) -> bool {
        Instant::now() >= self.until
            || self
                .terminal
                .as_ref()
                .is_err_and(|err| err.cause == aac_core::Cause::Timeout)
            || matches!(self.event(), Some(Event::Failure { failure, .. })
                if failure.cause == aac_core::Cause::Timeout)
    }
}

struct Active {
    job: Job,
    cancel: CancellationToken,
    lease: storage::Writer,
}

/// One dashboard's application update service. Route callers own authorization.
pub struct AppUpdates {
    directory: PathBuf,
    agents: Arc<crate::agent_transport::Agents>,
    active: Mutex<Option<Active>>,
    events: Option<Box<dyn Fn(DashboardEvent) + Send + Sync>>,
}

impl AppUpdates {
    /// Construct one service for the resolved configuration root. Keep the Arc
    /// for the service lifetime so status and cancel see the same job owner.
    pub fn new(root: PathBuf) -> Arc<Self> {
        let agents = Arc::new(crate::agent_transport::Agents::new(root.clone()));
        Self::with_events(root, agents, None)
    }

    #[cfg(unix)]
    pub(crate) fn with_pool(
        root: PathBuf,
        agents: Arc<crate::agent_transport::Agents>,
        events: Option<Feeds>,
    ) -> Arc<Self> {
        Self::with_events(
            root,
            agents,
            events.map(|feeds| {
                Box::new(move |event| {
                    if let Ok(value) = serde_json::to_value(event) {
                        feeds.publish_event(value);
                    }
                }) as Box<dyn Fn(DashboardEvent) + Send + Sync>
            }),
        )
    }

    pub(crate) fn with_events(
        root: PathBuf,
        agents: Arc<crate::agent_transport::Agents>,
        events: Option<Box<dyn Fn(DashboardEvent) + Send + Sync>>,
    ) -> Arc<Self> {
        Arc::new(Self {
            directory: root.join("app-updates"),
            agents,
            active: Mutex::new(None),
            events,
        })
    }

    fn publish(&self, job: &Job) {
        let Some(events) = &self.events else {
            return;
        };
        let hosts = job
            .hosts
            .iter()
            .flatten()
            .map(|(host, progress)| AppUpdateHostProgress {
                host: host.clone(),
                state: progress["state"].as_str().unwrap_or("waiting").to_owned(),
                current_app: progress["currentApp"].as_str().map(str::to_owned),
                phase: progress["phase"].as_str().map(str::to_owned),
                phase_since: progress["phaseSince"].as_str().map(str::to_owned),
            })
            .collect();
        let event = DashboardEvent::AppUpdateJob {
            job: AppUpdateProgress {
                id: job.id.clone(),
                state: job.state.clone(),
                cancel_requested: job.cancel_requested,
                completed_results: u32::try_from(job.results.len()).unwrap_or(u32::MAX),
                expected_results: job
                    .expected_results
                    .and_then(|total| u32::try_from(total).ok()),
                hosts,
            },
        };
        events(event);
    }

    /// Read a job without launching or reconciling any host process.
    pub async fn status(&self) -> AppUpdateReply {
        let active = self.active.lock().await;
        let job = active
            .as_ref()
            .map(|state| state.job.clone())
            .or_else(|| storage::load(&self.directory));
        reply(200, json!({"job":job}))
    }

    /// Save the new job and launch its fixed host tasks. Call only after the
    /// explicit authenticated start route accepts the request.
    pub async fn start(self: &Arc<Self>) -> AppUpdateReply {
        let mut active = self.active.lock().await;
        if let Some(state) = active.as_ref() {
            return busy(Some(&state.job));
        }
        let started = match (job_id(), storage::Writer::claim(&self.directory)) {
            (Ok((public_id, id)), Ok(lease)) => {
                let job = Job::new(public_id, clock());
                match lease.save(&job) {
                    Ok(()) => Ok((job, id, lease)),
                    Err(error) => Err(error),
                }
            }
            (Err(error), _) | (_, Err(error)) => Err(error),
        };
        let (job, id, lease) = match started {
            Ok(values) => values,
            Err(error) if error.cause == aac_core::Cause::Busy => {
                return busy(storage::load(&self.directory).as_ref());
            }
            Err(_) => return reply(500, json!({"error":"App updates could not start safely."})),
        };
        let cancel = CancellationToken::new();
        self.publish(&job);
        let result = reply(202, json!({"job":job}));
        *active = Some(Active {
            job,
            cancel: cancel.clone(),
            lease,
        });
        let this = self.clone();
        tokio::spawn(async move {
            this.execute(id, cancel).await;
        });
        result
    }

    /// Acknowledge cancellation of this service's job and signal each active host.
    pub async fn cancel(&self) -> AppUpdateReply {
        let mut active = self.active.lock().await;
        if let Some(state) = active.as_mut() {
            state.job.cancel_requested = true;
            state.cancel.cancel();
            let _ = state.lease.save(&state.job);
            self.publish(&state.job);
            return reply(202, json!({"job":state.job,"cancelling":true}));
        }
        let job = storage::load(&self.directory);
        if job.as_ref().is_some_and(Job::running) {
            reply(
                409,
                json!({
                    "error":"This update is run by another dashboard process, so it cannot be cancelled here.",
                    "code":"not_owner","job":job,"cancelling":false
                }),
            )
        } else {
            reply(200, json!({"job":job,"cancelling":false}))
        }
    }

    async fn change(&self, update: impl FnOnce(&mut Job)) {
        if let Some(active) = self.active.lock().await.as_mut() {
            update(&mut active.job);
            let _ = active.lease.save(&active.job);
            self.publish(&active.job);
        }
    }

    async fn execute(self: Arc<Self>, id: String, cancel: CancellationToken) {
        // One read of the package's helper set serves every computer in this job.
        let helpers = Arc::new(
            tokio::task::spawn_blocking(|| {
                crate::agent_transport::package_root()
                    .ok_or(helpers::SyncError::Failed {
                        reason: "helper_set_missing",
                        cause: "unknown",
                    })
                    .and_then(|package| helpers::HelperSet::load(&package))
            })
            .await
            .unwrap_or(Err(helpers::SyncError::Failed {
                reason: "helper_set_missing",
                cause: "unknown",
            })),
        );
        let mut work = JoinSet::new();
        for host in hosts() {
            let this = self.clone();
            let id = id.clone();
            let cancel = cancel.clone();
            let helpers = helpers.clone();
            work.spawn(async move {
                this.host_job(host, id, cancel, &helpers).await;
            });
        }
        while work.join_next().await.is_some() {}
        if let Some(mut active) = self.active.lock().await.take() {
            let missing = hosts()
                .filter(|host| {
                    active
                        .job
                        .hosts
                        .as_ref()
                        .and_then(|v| v.get(host.id))
                        .is_none_or(|state| state["state"] != "done")
                })
                .collect::<Vec<_>>();
            for host in missing {
                active.job.finish_host(host, None, false);
            }
            active.job.finish();
            let _ = active.lease.save(&active.job);
            self.publish(&active.job);
        }
    }

    async fn host_job(
        &self,
        host: Host,
        id: String,
        cancel: CancellationToken,
        helpers: &Result<helpers::HelperSet, helpers::SyncError>,
    ) {
        if cancel.is_cancelled() {
            self.change(|job| {
                job.fill(host, "skipped", "skipped_cancelled");
                job.host_done(host);
            })
            .await;
            return;
        }
        self.change(|job| {
            if let Some(state) = job.owned_host(host) {
                *state = json!({"state":"running","currentApp":null,"phase":"checking","phaseSince":clock()});
            }
            job.refresh_active();
        }).await;
        // The computer's budget covers its helper sync, as Wave 84's did.
        let started = Instant::now();
        match self.sync_helpers(host, helpers, &cancel).await {
            Ok(()) => {}
            Err(helpers::SyncError::Cancelled) => {
                self.change(|job| {
                    job.fill(host, "skipped", "skipped_cancelled");
                    job.host_done(host);
                })
                .await;
                return;
            }
            Err(helpers::SyncError::Failed { reason, cause }) => {
                tracing::warn!(host = host.id, reason, "app update helper sync failed");
                self.change(|job| {
                    job.fill_sync_failure(host, reason, cause);
                    job.host_done(host);
                })
                .await;
                return;
            }
        }
        let call = self.call_host(host, &id, &cancel, false, started).await;
        if !call.admitted && cancel.is_cancelled() {
            self.change(|job| {
                job.fill(host, "skipped", "skipped_cancelled");
                job.host_done(host);
            })
            .await;
            return;
        }
        if let Err(error) = &call.terminal {
            tracing::warn!(host=host.id,cause=?error.cause,"app update transport ended early");
        }
        let event = call.event();
        let timed_out = call.timed_out();
        let mut follow_up = false;
        self.change(|job| {
            if let Some(message) = call.setup_message() {
                job.fill(host, "failed", message);
                job.host_done(host);
            } else {
                job.finish_host(host, event, timed_out);
                follow_up = job.pending_acp(host).is_some();
            }
        })
        .await;
        if follow_up {
            // The saved host boundary releases the restart. The overall job
            // stays active for cancellation and the one final ACP replacement.
            let call = self.call_host(host, &id, &cancel, true, Instant::now())
                .await;
            self.change(|job| job.finish_acp(host, &call)).await;
        }
    }

    /// Give an admitted computer the package's helper set before its agent is asked to update.
    /// A computer without a usable pinned agent is left to the call, which names the setup step.
    async fn sync_helpers(
        &self,
        host: Host,
        helpers: &Result<helpers::HelperSet, helpers::SyncError>,
        cancel: &CancellationToken,
    ) -> Result<(), helpers::SyncError> {
        let selector = Selector::usage(host.os, host.alias);
        let deadline = Instant::now() + Duration::from_secs(10);
        if self.agents.client(&selector, deadline).await.is_none() {
            return Ok(());
        }
        let Some(computer) = self.agents.computer(&selector, deadline).await else {
            return Ok(());
        };
        let set = helpers.as_ref().map_err(Clone::clone)?;
        helpers::sync(&computer, set, host.os, cancel).await
    }

    async fn call_host(
        &self,
        host: Host,
        id: &str,
        cancel: &CancellationToken,
        follow_up: bool,
        started: Instant,
    ) -> HostCall {
        let (progress, mut events) = mpsc::channel(8);
        let (admission, controls) = mpsc::channel(1);
        let mut options = CallOptions::new(Duration::ZERO, MAX_FRAME_BYTES * 2);
        options.deadline = started
            + Duration::from_millis(if follow_up {
                ACP_FOLLOW_UP_HOST_TIMEOUT_MS
            } else {
                APP_UPDATE_HOST_TIMEOUT_MS
            });
        let until = options.deadline;
        let transport_cancel = options.cancel.clone();
        let work = self.agents.app_call(
            Selector::usage(host.os, host.alias),
            format!("{id}-{}{}", host.id, if follow_up { "-acp" } else { "" }),
            json!({
                "action": if follow_up { "prepare-acp-follow-up" } else { "prepare-update" },
                "platform": host.os, "jobId": id,
            }),
            options,
            controls,
            progress,
        );
        let signal = self.signal_cancel(host, id, cancel.clone(), until);
        tokio::pin!(work, signal);
        let mut admitted = false;
        let terminal = loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled(), if !admitted && !transport_cancel.is_cancelled() => {
                    transport_cancel.cancel();
                }
                Some(Event::Progress { data, .. }) = events.recv() => {
                    if data["event"] == APP_UPDATE_ADMISSION {
                        // Serialize granting the connected host permission with the HTTP cancel
                        // acknowledgement. Only a granted host needs job-bound cancellation.
                        let _active = self.active.lock().await;
                        if !admitted && !cancel.is_cancelled() {
                            admitted = admission.try_send(format!("{APP_UPDATE_ADMIT}\n").into_bytes()).is_ok();
                        }
                    } else if !follow_up {
                        self.change(|job| job.event(host, &data)).await;
                    }
                }
                answer = &mut work => break answer,
                _ = &mut signal, if admitted => {},
            }
        };
        if !follow_up && (admitted || !cancel.is_cancelled()) {
            while let Ok(Event::Progress { data, .. }) = events.try_recv() {
                self.change(|job| job.event(host, &data)).await;
            }
        }
        HostCall {
            admitted,
            terminal,
            until,
        }
    }

    async fn signal_cancel(&self, host: Host, id: &str, token: CancellationToken, until: Instant) {
        token.cancelled().await;
        while Instant::now() < until {
            let (send, _progress) = mpsc::channel(1);
            let (_input, controls) = mpsc::channel(1);
            let answer = self
                .agents
                .app_call(
                    Selector::usage(host.os, host.alias),
                    format!("{id}-{}-cancel", host.id),
                    json!({"action":"cancel","platform":host.os,"jobId":id}),
                    CallOptions::new(Duration::from_secs(5), MAX_FRAME_BYTES * 2),
                    controls,
                    send,
                )
                .await;
            if answer.is_ok_and(|events| {
                matches!(events.last(),
                Some(Event::Result{value,..}) if value["cancelled"] == true)
            }) {
                break;
            }
            // An admitted update may still be creating its journal. A false
            // acknowledgement leaves the real job running.
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        std::future::pending::<()>().await;
    }
}

fn busy(job: Option<&Job>) -> AppUpdateReply {
    reply(
        409,
        json!({"error":"An app update is already running.","job":job}),
    )
}

#[cfg(test)]
mod tests;
