//! Quota collection and lifecycle share the public service's boot and L5 pool.
use super::{
    DashboardAccess, DashboardEvent, DashboardEvents, DashboardPrincipal, HubCache, Reply,
};
use crate::{
    account_usage::{
        AccountUsageCache,
        producer::{CompanionInputs, Producer},
    },
    agent_transport::Agents,
    collection::Collection,
    limits::Limits,
    table::Pool,
};
use aac_files::registry::{self, Owners, Role};
use aac_providers::ProviderId;
use aac_renewal::claude::ClaudeSignInRenewalReader;
use aac_transport::CancellationToken;
use axum::{
    extract::Request,
    http::{Method, Uri},
};
use serde_json::{Map, Value, json};
use std::{
    io,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use tokio::task::JoinHandle;

mod lifecycle_antigravity;
mod lifecycle_codex;
mod lifecycle_jobs;
mod lifecycle_keys;
mod lifecycle_label;
mod lifecycle_login;
mod lifecycle_maintenance;
mod lifecycle_open;
mod lifecycle_registry;
mod lifecycle_remove;
mod lifecycle_trash;
pub(super) mod recheck;

#[derive(Clone, Copy, Default)]
pub(super) struct Groups {
    pub(super) collection: bool,
    pub(super) lifecycle: bool,
    writes: bool,
    pub(super) home: bool,
    #[cfg(unix)]
    analytics_writers: bool,
    /// G18: the service owns the aac-usage share writer this boot.
    #[cfg(unix)]
    usage_snapshot: bool,
    #[cfg(unix)]
    store: bool,
}

impl Groups {
    pub(super) fn from_config(config: &Value) -> Self {
        let on = |name: &str, env: &str| match std::env::var_os(env) {
            Some(value) => value == "on",
            None => {
                let groups = &config["rustService"]["groups"];
                groups.get(name).is_none() || groups[name] == "on"
            }
        };
        #[cfg(unix)]
        let store = match std::env::var_os("AAC_RUST_STORE") {
            Some(value) => value == "on",
            None => {
                config["rustService"].get("store").is_none()
                    || config["rustService"]["store"] == "on"
            }
        };
        Self {
            lifecycle: false,
            #[cfg(unix)]
            store,
            #[cfg(unix)]
            analytics_writers: on("analytics-writers", "AAC_RUST_GROUP_ANALYTICS_WRITERS")
                && on("analytics", "AAC_RUST_GROUP_ANALYTICS"),
            collection: on("collection", "AAC_RUST_GROUP_COLLECTION"),
            writes: on("writes", "AAC_RUST_GROUP_WRITES"),
            home: on("home", "AAC_RUST_GROUP_HOME")
                || std::env::var("AAC_RUST_HOME_VIEW").as_deref() == Ok("on"),
            #[cfg(unix)]
            usage_snapshot: on("usageSnapshot", "AAC_RUST_GROUP_USAGESNAPSHOT"),
        }
    }
}

#[derive(Clone)]
pub(crate) struct NativeOwners {
    agents: Arc<Agents>,
    owners: Owners,
    queues: Arc<crate::lifecycle_queue::LifecycleQueues>,
    lifecycle_jobs: Option<Arc<lifecycle_jobs::Jobs>>,
    trash: Option<Arc<lifecycle_trash::Trash>>,
    events: DashboardEvents,
    usage: Option<Arc<Usage>>,
    writes: bool,
    limits: Limits,
}

pub(crate) struct Jobs {
    cancel: CancellationToken,
    sampling: Option<JoinHandle<()>>,
    signins: Option<JoinHandle<()>>,
    maintenance: Option<JoinHandle<()>>,
}

impl Jobs {
    pub(crate) async fn stop(self) {
        self.cancel.cancel();
        if let Some(task) = self.signins {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.sampling {
            let _ = task.await;
        }
        if let Some(task) = self.maintenance {
            let _ = task.await;
        }
    }
}

struct Usage {
    root: PathBuf,
    producer: Arc<Producer>,
    cache: AccountUsageCache,
    publication: Publication,
    events: DashboardEvents,
    jobs: Option<Arc<lifecycle_jobs::Jobs>>,
    recheck_limit: Mutex<recheck::Limit>,
    antigravity: Option<Arc<crate::antigravity_host::Host>>,
    owners: Owners,
    agents: Arc<Agents>,
    /// The sign-in expiry rows and when they were read (the TS health cache's five minutes).
    #[cfg(unix)]
    signin_cache: Mutex<Option<(i64, Value)>>,
}

#[cfg(unix)]
type Published = Arc<dyn Fn(&AccountUsageCache) + Send + Sync>;

enum Publication {
    Public(HubCache),
    #[cfg(unix)]
    Private(Published),
}

struct PublicSampling {
    signins: ClaudeSignInRenewalReader,
    server: Value,
    #[cfg(unix)]
    store: bool,
    #[cfg(unix)]
    analytics_writers: bool,
    #[cfg(unix)]
    usage_snapshot: bool,
    #[cfg(windows)]
    worker_snapshot: bool,
}

fn lifecycle_facts() -> crate::account_usage::dashboard::Lifecycle {
    use ProviderId::{Antigravity, Claude, Codex, Muse};
    let claude = std::env::var("CCS_CLAUDE_HOST_LIFECYCLE").as_deref() == Ok("on");
    let mut lifecycle = crate::account_usage::dashboard::Lifecycle {
        mounted: true,
        recheck_disabled: vec![Codex, Claude, Antigravity],
        ..Default::default()
    };
    if !claude {
        lifecycle.remove_disabled.push(Claude);
        lifecycle
            .blocked
            .insert("claude".into(), json!("not_implemented"));
    }
    if std::env::var("CCS_MUSE_SIGNIN").as_deref() != Ok("on") {
        lifecycle
            .blocked
            .insert(Muse.as_str().into(), json!("not_implemented"));
    }
    lifecycle
}

impl NativeOwners {
    pub(super) fn start(
        root: PathBuf,
        groups: Groups,
        hub: HubCache,
        events: DashboardEvents,
        worker_directory: Option<PathBuf>,
    ) -> io::Result<(Self, Jobs)> {
        let root = root.canonicalize()?;
        let agents = Arc::new(Agents::with_default(root.clone(), true));
        let cancel = CancellationToken::new();
        let mut jobs = Jobs {
            cancel: cancel.clone(),
            sampling: None,
            signins: None,
            maintenance: None,
        };
        let rows = registry::rows().map_err(io::Error::other)?;
        let files = rows
            .iter()
            .map(|row| {
                let owner = if (groups.collection
                    && row.moves_with.as_deref() == Some("collection"))
                    || (groups.writes && row.moves_with.as_deref() == Some("writes"))
                    || (groups.lifecycle
                        && (row.moves_with.as_deref() == Some("lifecycle")
                            || matches!(
                                row.id.as_str(),
                                "account-registry"
                                    | "account-keys"
                                    | "claude-desktop-profiles"
                                    | "claude-pending"
                                    | "claude-trash"
                            ))) {
                    Role::RustService
                } else {
                    row.owner
                };
                (row.id.clone(), owner.word().to_owned())
            })
            .collect();
        let owners = Owners::from_boot(rows, &files).map_err(io::Error::other)?;
        let collection_lease = groups
            .collection
            .then(|| aac_files::OwnedStore::claim_collection(&root).map_err(std::io::Error::other))
            .transpose()?;
        let queues = Arc::new(crate::lifecycle_queue::LifecycleQueues::new(root.clone()));
        let lifecycle_jobs = groups
            .lifecycle
            .then(|| Arc::new(lifecycle_jobs::Jobs::default()));
        let trash = groups.lifecycle.then(|| {
            Arc::new(lifecycle_trash::Trash::new(
                root.clone(),
                owners.clone(),
                agents.clone(),
                queues.clone(),
            ))
        });
        if let Some(trash) = trash.clone() {
            jobs.maintenance = Some(lifecycle_maintenance::start(
                root.clone(),
                owners.clone(),
                trash,
                cancel.clone(),
            ));
        }
        let usage = if groups.collection {
            let build = env!("AAC_BUILD_COMMIT");
            let commit = build.get(..8).filter(|_| {
                build.len() == 40 && build.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
            let mut server = json!({"version":env!("AAC_VERSION"),"commit":commit});
            if groups.home && worker_directory.is_some() {
                server["views"] = json!(["home"]);
            }
            let collection = Arc::new(Collection::default());
            let usage = Arc::new(Usage {
                producer: Arc::new(Producer::shared(
                    root.clone(),
                    owners.clone(),
                    agents.clone(),
                    collection,
                    Arc::default(),
                )),
                cache: AccountUsageCache::default(),
                publication: Publication::Public(hub),
                events: events.clone(),
                jobs: lifecycle_jobs.clone(),
                recheck_limit: Mutex::default(),
                antigravity: crate::antigravity_host::Host::from_root(&root).map(Arc::new),
                owners: owners.clone(),
                agents: agents.clone(),
                root: root.clone(),
                #[cfg(unix)]
                signin_cache: Mutex::default(),
            });
            let sampling = PublicSampling {
                signins: ClaudeSignInRenewalReader::new(root, Default::default()),
                server,
                #[cfg(unix)]
                store: groups.store && worker_directory.is_some(),
                #[cfg(unix)]
                analytics_writers: groups.analytics_writers,
                #[cfg(unix)]
                usage_snapshot: groups.usage_snapshot,
                #[cfg(windows)]
                worker_snapshot: worker_directory.is_some(),
            };
            let sampler = usage.clone();
            jobs.sampling = Some(tokio::spawn(async move {
                let _lease = collection_lease;
                sampler.sample(cancel, Arc::new(sampling)).await;
            }));
            Some(usage)
        } else {
            None
        };
        Ok((
            Self {
                agents,
                owners,
                queues,
                lifecycle_jobs,
                trash,
                events,
                usage,
                writes: groups.writes,
                limits: Limits::new(),
            },
            jobs,
        ))
    }

    pub(crate) fn owns_lifecycle(method: &Method, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        super::lifecycle::Route::of(method, path).is_some()
            || lifecycle_open::route(method, path).is_some()
            || lifecycle_label::route(method, path).is_some()
            || lifecycle_jobs::route(method, path).is_some()
            || lifecycle_registry::route(method, path)
            || lifecycle_keys::route(method, path).is_some()
            || lifecycle_remove::route(method, path).is_some()
            || lifecycle_login::route(method, path).is_some()
            || lifecycle_trash::route(method, path).is_some()
    }

    #[cfg(unix)]
    pub(crate) fn root(&self) -> &std::path::Path {
        self.agents.root()
    }

    #[cfg(unix)]
    pub(crate) async fn private_share_sample(
        &self,
        cache: &AccountUsageCache,
        signins: &ClaudeSignInRenewalReader,
    ) -> Option<Value> {
        self.usage.as_ref()?.share_sample(cache, signins).await
    }

    #[cfg(unix)]
    pub(crate) fn for_private_lifecycle(
        agents: Arc<Agents>,
        owners: Owners,
        cache: AccountUsageCache,
        producer: Arc<Producer>,
        approvals: Arc<crate::account_approvals::Approvals>,
        feeds: crate::feed::Feeds,
        published: Published,
    ) -> (Self, Jobs) {
        let root = agents.root().to_owned();
        let queues = Arc::new(crate::lifecycle_queue::LifecycleQueues::new(root.clone()));
        let trash = Arc::new(lifecycle_trash::Trash::with_approvals(
            root.clone(),
            owners.clone(),
            agents.clone(),
            queues.clone(),
            approvals,
        ));
        let cancel = CancellationToken::new();
        let events = DashboardEvents(tokio::sync::broadcast::channel(64).0);
        let mut receiver = events.0.subscribe();
        let signins = tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => {
                        if let Ok(event) = serde_json::to_value(event) {
                            feeds.publish_event(event);
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
        let maintenance = lifecycle_maintenance::start(
            root.clone(),
            owners.clone(),
            trash.clone(),
            cancel.clone(),
        );
        let lifecycle_jobs = Arc::new(lifecycle_jobs::Jobs::default());
        let usage = Usage {
            producer,
            cache,
            publication: Publication::Private(published),
            events: events.clone(),
            jobs: Some(lifecycle_jobs.clone()),
            recheck_limit: Mutex::default(),
            antigravity: crate::antigravity_host::Host::from_root(&root).map(Arc::new),
            owners: owners.clone(),
            agents: agents.clone(),
            root,
            signin_cache: Mutex::default(),
        };
        (
            Self {
                agents,
                owners,
                queues,
                lifecycle_jobs: Some(lifecycle_jobs),
                trash: Some(trash),
                events,
                usage: Some(Arc::new(usage)),
                writes: true,
                limits: Limits::new(),
            },
            Jobs {
                cancel,
                sampling: None,
                signins: Some(signins),
                maintenance: Some(maintenance),
            },
        )
    }

    #[cfg(unix)]
    pub(crate) async fn private_companions(
        &self,
        mut facts: CompanionInputs,
        refresh: bool,
    ) -> CompanionInputs {
        if let Some(usage) = &self.usage {
            facts.lifecycle = usage.lifecycle();
            let usage = usage.clone();
            facts.antigravity =
                tokio::task::spawn_blocking(move || usage.antigravity_accounts(refresh))
                    .await
                    .ok()
                    .flatten();
        }
        facts
    }

    pub(super) fn owns(&self, method: &Method, path: &str) -> bool {
        Self::dashboard_route(method, path)
            || recheck::route(method, path).is_some()
            || lifecycle_open::route(method, path).is_some()
            || lifecycle_label::route(method, path).is_some()
            || (self.lifecycle_jobs.is_some() && Self::owns_lifecycle(method, path))
    }

    fn changed(&self) {
        if let Some(usage) = &self.usage {
            usage.cache.request_refresh();
        }
        self.events.publish(DashboardEvent::AccountsChanged {});
    }

    pub(super) fn owns_collection(method: &Method, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        Self::dashboard_route(method, path) || recheck::route(method, path).is_some()
    }

    pub(super) fn dashboard_route(method: &Method, path: &str) -> bool {
        matches!(*method, Method::GET | Method::HEAD) && path == "/api/accounts/dashboard"
    }

    pub(super) fn profile_inputs(&self) -> (Arc<Agents>, Option<AccountUsageCache>) {
        (self.agents.clone(), self.usage_cache())
    }

    pub(super) fn usage_cache(&self) -> Option<AccountUsageCache> {
        self.usage.as_ref().map(|usage| usage.cache.clone())
    }

    pub(super) async fn codex_profile_rows(&self, now: i64) -> Vec<Value> {
        match &self.usage {
            Some(usage) => usage.producer.codex_profile_rows(now).await,
            None => Vec::new(),
        }
    }

    pub(crate) async fn handle(&self, request: Request) -> Reply {
        let path = request.uri().path().trim_end_matches('/');
        let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
            return super::service_error();
        };
        if Self::dashboard_route(request.method(), path) {
            let (parts, _) = request.into_parts();
            return self.dashboard(&access, parts.uri, parts.headers).await;
        }
        if let Some(id) = lifecycle_open::route(request.method(), path).map(str::to_owned) {
            return lifecycle_open::handle(&self.agents, &id, request).await;
        }
        if self.lifecycle_jobs.is_some()
            && let Some(id) = lifecycle_login::route(request.method(), path)
        {
            let id = id.to_owned();
            return lifecycle_login::handle(self, &id, request).await;
        }
        if self.trash.is_some()
            && let Some(id) = lifecycle_remove::route(request.method(), path)
        {
            let id = id.to_owned();
            return lifecycle_remove::handle(self, &id, request).await;
        }
        if self.lifecycle_jobs.is_some()
            && let Some((action, id)) = lifecycle_keys::route(request.method(), path)
        {
            let id = id.to_owned();
            return lifecycle_keys::handle(self, action, &id, request).await;
        }
        if let Some(jobs) = &self.lifecycle_jobs
            && lifecycle_registry::route(request.method(), path)
        {
            return lifecycle_registry::handle(self, jobs, request).await;
        }
        if let Some(jobs) = &self.lifecycle_jobs
            && let Some((action, id)) = lifecycle_jobs::route(request.method(), path)
        {
            let id = id.to_owned();
            return jobs.clone().handle(self, action, &id, request).await;
        }
        if let Some(trash) = &self.trash
            && let Some((action, id)) = lifecycle_trash::route(request.method(), path)
        {
            let id = id.to_owned();
            return trash
                .clone()
                .handle(action, &id, request, self.events.clone())
                .await;
        }
        if let Some(id) = lifecycle_label::route(request.method(), path) {
            return lifecycle_label::handle(self, &id, request).await;
        }
        if let Some(id) = recheck::route(request.method(), path).map(str::to_owned) {
            let reply = match self.usage.as_ref().filter(|_| self.writes) {
                Some(usage) => usage.recheck(&access, &id, request).await,
                None => unavailable(),
            };
            return no_store(reply);
        }
        super::plain(404, "API endpoint was not found.")
    }

    async fn dashboard(
        &self,
        access: &DashboardAccess,
        uri: Uri,
        headers: axum::http::HeaderMap,
    ) -> Reply {
        if matches!(access.principal, DashboardPrincipal::Anonymous) {
            return no_store(super::plain(401, "Authentication required"));
        }
        let Some(usage) = &self.usage else {
            return unavailable();
        };
        let cache = usage.cache.clone();
        let secure = access.secure_transport;
        let read = |cache: crate::account_usage::AccountUsageCache,
                    uri: Uri,
                    headers: axum::http::HeaderMap| async move {
            self.limits
                .blocking(Pool::Reads, move || {
                    crate::dashboard_fed::latest_for_access(&cache, &uri, &headers, secure)
                })
                .await
                .unwrap_or_else(|_| unavailable())
        };
        let reply = read(cache.clone(), uri.clone(), headers.clone()).await;
        // A reader that arrives before the first generation asks for one and waits, so the
        // Accounts page never answers "unavailable" during a cold start (TS collects on demand).
        if reply.status() != 503 {
            return reply;
        }
        let desktop = crate::account_usage::Desktop::from_query(&uri);
        cache.request_refresh();
        if !cache
            .await_generation(desktop, secure, std::time::Duration::from_secs(12))
            .await
        {
            return no_store(reply);
        }
        read(cache, uri, headers).await
    }
}

impl Usage {
    fn lifecycle(&self) -> crate::account_usage::dashboard::Lifecycle {
        let mut facts = lifecycle_facts();
        if let Some(jobs) = &self.jobs {
            for job in jobs.running(false) {
                if let Some(id) = job.account_id {
                    let state = if job.state == "verifying" {
                        "verifying"
                    } else {
                        "signing_in"
                    };
                    facts.jobs.insert(id, json!({"state":state,"jobId":job.id}));
                }
            }
        }
        facts
    }

    fn companions(&self) -> CompanionInputs {
        CompanionInputs {
            lifecycle: self.lifecycle(),
            ..Default::default()
        }
    }

    fn antigravity_accounts(&self, refresh: bool) -> Option<Vec<Value>> {
        self.antigravity.as_ref().and_then(|host| {
            crate::antigravity_runtime::has_profiles(&self.root)
                .then(|| host.accounts(refresh, super::now()))
                .flatten()
        })
    }

    async fn collect_companions(
        self: Arc<Self>,
        sampling: Arc<PublicSampling>,
        refresh: bool,
    ) -> CompanionInputs {
        let mut facts = self.companions();
        facts.server = Some(sampling.server.clone());
        let fallback = facts.clone();
        tokio::task::spawn_blocking(move || {
            facts.signins = sampling.signins.read().map(|report| report.to_json());
            facts.antigravity = self.antigravity_accounts(refresh);
            facts
        })
        .await
        .unwrap_or(fallback)
    }

    fn publish(&self) {
        match &self.publication {
            Publication::Public(hub) => {
                let now = super::now();
                let Some(timestamp) = super::storage::iso(now) else {
                    return;
                };
                hub.publish_dashboard_cache(&self.cache.hub_dashboards(&timestamp, now));
            }
            #[cfg(unix)]
            Publication::Private(published) => published(&self.cache),
        }
        self.events.publish(DashboardEvent::AccountsChanged {});
    }

    async fn sample(self: Arc<Self>, cancel: CancellationToken, sampling: Arc<PublicSampling>) {
        self.producer
            .run(
                self.cache.clone(),
                cancel,
                |refresh| self.clone().collect_companions(sampling.clone(), refresh),
                |cache| {
                    let owner = self.clone();
                    let sampling = sampling.clone();
                    async move {
                        owner.publish();
                        #[cfg(unix)]
                        if sampling.store {
                            super::native_reads::publish_usage(
                                owner.root.clone(),
                                &cache,
                                super::now(),
                                sampling.analytics_writers,
                            )
                            .await;
                        }
                        #[cfg(windows)]
                        if sampling.worker_snapshot {
                            let now = super::now();
                            if let Some(timestamp) = super::storage::iso(now) {
                                cache.publish_completed_sample(&timestamp, now);
                            }
                        }
                        #[cfg(not(any(unix, windows)))]
                        let _ = (&sampling, &cache);
                        // G18: the native sample triggers the share writer itself: the store's
                        // history, long burn and rollups are added by the service, and the
                        // sign-in facts are collected here (only while a folder is set). The
                        // publisher and its facts are the Unix service's; Windows keeps the TS
                        // writer.
                        #[cfg(unix)]
                        if sampling.usage_snapshot
                            && sampling.analytics_writers
                            && let Some(payload) =
                                owner.share_sample(&cache, &sampling.signins).await
                        {
                            super::native_reads::usage_sample(owner.root.clone(), payload).await;
                        }
                    }
                },
            )
            .await;
    }
    #[cfg(unix)]
    /// G18: push this sample to the service's share writer. Every sample's readings count, even
    /// one whose write the service skips, so the reading ring sees the same cadence; the sign-in
    /// probes and the store facts run only while a folder is set and there are accounts.
    async fn share_sample(
        self: &Arc<Self>,
        cache: &AccountUsageCache,
        signins: &ClaudeSignInRenewalReader,
    ) -> Option<Value> {
        let now = super::now();
        let timestamp = super::storage::iso(now)?;
        let accounts = cache.merged_accounts(&timestamp, now).ok()?;
        let active = crate::usage_snapshot::sample_active(&self.root, accounts.len());
        let sign_in = if active {
            self.sign_in_facts(signins).await
        } else {
            json!({ "rows": [], "failed": false, "since": now })
        };
        let renewal = if active {
            signins.read().map(|report| {
                let full = report.to_json();
                json!({
                    "runner": full["runner"],
                    "generatedAt": full["generatedAt"],
                    "stale": full["stale"],
                    "needsYou": full["needsYou"],
                })
            })
        } else {
            None
        };
        Some(json!({
            "event": { "accounts": accounts, "history": null, "sampledAt": now },
            "signIn": sign_in,
            "signInRenewal": renewal,
            "rollups": null,
        }))
    }

    #[cfg(unix)]
    /// The sign-in expiry rows of every configured computer, under the publisher's 60 s limit; a
    /// failed or timed-out pass is named (`failed`), never sent as an empty success.
    async fn sign_in_facts(self: &Arc<Self>, signins: &ClaudeSignInRenewalReader) -> Value {
        let since = super::now();
        // Five minutes, like the TypeScript health cache, so one page cannot make every
        // computer run the expiry collector on every sample.
        let cached = {
            let guard = self.signin_cache.lock().unwrap_or_else(|p| p.into_inner());
            guard
                .as_ref()
                .filter(|entry| since - entry.0 < 300_000)
                .map(|entry| entry.1.clone())
        };
        if let Some(mut fresh) = cached {
            if let Some(map) = fresh.as_object_mut() {
                map.insert("since".into(), json!(since));
            }
            return fresh;
        }
        let hosts = configured_signin_hosts();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(60),
            crate::signins::poll::read(&self.agents, &self.root, hosts),
        )
        .await;
        let Ok(Ok(batch)) = read else {
            return json!({ "rows": [], "failed": true, "since": since });
        };
        let rows = match signins.read() {
            Some(renewal) => {
                let merged =
                    crate::signins::merge_renewal(batch.machines, &renewal.to_json(), super::now());
                merged.unwrap_or_default()
            }
            None => batch.machines,
        };
        let rows: Vec<Value> = rows
            .values()
            .filter_map(Value::as_array)
            .flatten()
            .cloned()
            .collect();
        let facts = json!({ "rows": rows, "failed": false, "since": since });
        *self.signin_cache.lock().unwrap_or_else(|p| p.into_inner()) = Some((since, facts.clone()));
        facts
    }
}

#[cfg(unix)]
/// The computers a sign-in poll reads, in the account collectors' order: this one, then every
/// remote whose packaged alias is set (`configuredUsageHosts`). The same packaged registry
/// (`web-dashboard/public/computers.json`) the collectors and the coordinator read.
fn configured_signin_hosts() -> Vec<crate::signins::poll::Host> {
    static COMPUTERS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!(
            "../../../../web-dashboard/public/computers.json"
        ))
        .unwrap_or(Value::Null)
    });
    let entries = COMPUTERS.as_object().cloned().unwrap_or_default();
    let hostname = std::env::var("HOSTNAME")
        .ok()
        .or_else(|| {
            aac_files::read_bounded(std::path::Path::new("/proc/sys/kernel/hostname"), 256)
                .ok()
                .and_then(|bytes| String::from_utf8(bytes).ok())
                .map(|text| text.trim().to_owned())
        })
        .unwrap_or_default()
        .to_lowercase();
    let platform = |id: &str| -> String {
        match entries
            .get(id)
            .and_then(|row| row.get("os"))
            .and_then(Value::as_str)
        {
            Some("linux") => "ubuntu".to_owned(),
            Some(other) => other.to_owned(),
            None => "ubuntu".to_owned(),
        }
    };
    // `localComputer`: a known name of this platform, else its generic platform host.
    let local = entries
        .iter()
        .find(|(id, row)| {
            let names = row.get("names").and_then(Value::as_array);
            let alias = row.get("sshAlias").and_then(Value::as_str);
            [Some(id.as_str()), alias]
                .into_iter()
                .flatten()
                .chain(names.into_iter().flatten().filter_map(Value::as_str))
                .any(|name| name.to_lowercase() == hostname)
        })
        .map(|(id, _)| id.clone())
        .filter(|id| platform(id) == instance_platform())
        .unwrap_or_else(instance_platform);
    let mut hosts = vec![crate::signins::poll::Host {
        host: local.clone(),
        platform: platform(&local),
        ssh_host: None,
    }];
    for (id, row) in &entries {
        if *id == local || *id == "ubuntu" {
            continue;
        }
        let Some(alias) = row.get("sshAlias").and_then(Value::as_str) else {
            continue;
        };
        if alias.is_empty() {
            continue;
        }
        hosts.push(crate::signins::poll::Host {
            host: id.clone(),
            platform: platform(id),
            ssh_host: Some(alias.to_owned()),
        });
    }
    hosts
}

/// This service's own helper platform (`helperPlatform(localComputer())`).
#[cfg(unix)]
fn instance_platform() -> String {
    if cfg!(target_os = "macos") {
        "mac".to_owned()
    } else {
        "ubuntu".to_owned()
    }
}

pub(super) fn json_content(request: &Request) -> bool {
    super::header(request.headers(), "content-type")
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

async fn object_body(request: Request, limit: usize) -> Option<Map<String, Value>> {
    use axum::http::header::{CONTENT_LENGTH, TRANSFER_ENCODING};
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|size| size.to_str().ok());
    if declared
        .and_then(|size| size.parse::<usize>().ok())
        .is_some_and(|size| size > limit)
    {
        return None;
    }
    let has_body = declared.is_some() || request.headers().contains_key(TRANSFER_ENCODING);
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .ok()?;
    if bytes.is_empty() {
        return has_body.then(Map::new);
    }
    serde_json::from_slice(&bytes).ok()
}

pub(super) fn no_store(mut reply: Reply) -> Reply {
    super::set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn unavailable() -> Reply {
    super::decline()
}

pub(super) fn is_recheck(request: &Request) -> bool {
    recheck::route(request.method(), request.uri().path().trim_end_matches('/')).is_some()
}
