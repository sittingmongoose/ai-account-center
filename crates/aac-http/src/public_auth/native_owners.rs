//! Quota collection and app updates share the public service's boot and L5 pool.
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
#[cfg(unix)]
use crate::{
    app_update_routes::{BoundJobs, LaneHJobs, Operation},
    app_updates::AppUpdates,
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

mod recheck;

#[derive(Clone, Copy, Default)]
pub(super) struct Groups {
    pub(super) collection: bool,
    writes: bool,
    app_updates: bool,
    home: bool,
    analytics_writers: bool,
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
        let store = match std::env::var_os("AAC_RUST_STORE") {
            Some(value) => value == "on",
            None => {
                config["rustService"].get("store").is_none()
                    || config["rustService"]["store"] == "on"
            }
        };
        Self {
            store,
            analytics_writers: on("analytics-writers", "AAC_RUST_GROUP_ANALYTICS_WRITERS")
                && on("analytics", "AAC_RUST_GROUP_ANALYTICS"),
            collection: on("collection", "AAC_RUST_GROUP_COLLECTION"),
            writes: on("writes", "AAC_RUST_GROUP_WRITES"),
            app_updates: on("app-updates", "AAC_RUST_GROUP_APP_UPDATES"),
            home: on("home", "AAC_RUST_GROUP_HOME")
                || std::env::var("AAC_RUST_HOME_VIEW").as_deref() == Ok("on"),
        }
    }
}

#[derive(Clone)]
pub(super) struct NativeOwners {
    usage: Option<Arc<Usage>>,
    #[cfg(unix)]
    apps: Option<Arc<BoundJobs>>,
    writes: bool,
    limits: Limits,
}

pub(super) struct Jobs {
    cancel: CancellationToken,
    sampling: Option<JoinHandle<()>>,
    signins: Option<JoinHandle<()>>,
}

impl Jobs {
    pub(super) async fn stop(self) {
        self.cancel.cancel();
        if let Some(task) = self.signins {
            task.abort();
            let _ = task.await;
        }
        if let Some(task) = self.sampling {
            let _ = task.await;
        }
    }
}

struct Usage {
    root: PathBuf,
    producer: Producer,
    cache: AccountUsageCache,
    hub: HubCache,
    events: DashboardEvents,
    lifecycle: Mutex<crate::account_usage::dashboard::Lifecycle>,
    recheck_limit: Mutex<recheck::Limit>,
    signins: ClaudeSignInRenewalReader,
    antigravity: Option<crate::antigravity_host::Host>,
    server: Value,
    worker_directory: Option<PathBuf>,
    store: bool,
    analytics_writers: bool,
    owners: Owners,
    agents: Arc<Agents>,
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
        #[cfg(unix)]
        let apps = groups.app_updates.then(|| {
            #[cfg(feature = "parity-fixtures")]
            if std::env::var("AAC_RUST_CUT_APP_UPDATES_FIXTURE").as_deref() == Ok("1") {
                return Arc::new(BoundJobs::new(Arc::new(crate::app_update_routes::M0Jobs)));
            }
            let events = events.clone();
            let owner = AppUpdates::with_events(
                root.clone(),
                agents.clone(),
                Some(Box::new(move |event| {
                    events.publish(event);
                })),
            );
            Arc::new(BoundJobs::new(Arc::new(LaneHJobs(owner))))
        });
        let cancel = CancellationToken::new();
        let mut jobs = Jobs {
            cancel: cancel.clone(),
            sampling: None,
            signins: None,
        };
        let usage = if groups.collection {
            let rows = registry::rows().map_err(io::Error::other)?;
            let files = rows
                .iter()
                .map(|row| {
                    let owner = if row.moves_with.as_deref() == Some("collection")
                        || (groups.writes && row.moves_with.as_deref() == Some("writes"))
                    {
                        Role::RustService
                    } else {
                        row.owner
                    };
                    (row.id.clone(), owner.word().to_owned())
                })
                .collect();
            let owners = Owners::from_boot(rows, &files).map_err(io::Error::other)?;
            let build = env!("AAC_BUILD_COMMIT");
            let commit = build.get(..8).filter(|_| {
                build.len() == 40 && build.bytes().all(|byte| byte.is_ascii_hexdigit())
            });
            let mut server = json!({"version":env!("AAC_VERSION"),"commit":commit});
            if groups.home && worker_directory.is_some() {
                server["views"] = json!(["home"]);
            }
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
            let collection = Arc::new(Collection::default());
            let usage = Arc::new(Usage {
                producer: Producer::shared(
                    root.clone(),
                    owners.clone(),
                    agents.clone(),
                    collection,
                    Arc::default(),
                ),
                cache: AccountUsageCache::default(),
                hub,
                events: events.clone(),
                lifecycle: Mutex::new(lifecycle),
                recheck_limit: Mutex::default(),
                signins: ClaudeSignInRenewalReader::new(root.clone(), Default::default()),
                antigravity: crate::antigravity_host::Host::from_root(&root),
                server,
                store: groups.store && worker_directory.is_some(),
                worker_directory,
                analytics_writers: groups.analytics_writers,
                owners,
                agents,
                root,
            });
            // Subscribe before the HTTP listener can admit a sign-in.
            let receiver = events.0.subscribe();
            jobs.signins = Some(tokio::spawn(usage.clone().watch_signins(receiver)));
            jobs.sampling = Some(tokio::spawn(usage.clone().sample(cancel)));
            Some(usage)
        } else {
            None
        };
        Ok((
            Self {
                usage,
                #[cfg(unix)]
                apps,
                writes: groups.writes,
                limits: Limits::new(),
            },
            jobs,
        ))
    }

    pub(super) fn owns(method: &Method, path: &str) -> bool {
        Self::dashboard_route(method, path)
            || recheck::route(method, path).is_some()
            || Self::app_operation(method, path).is_some()
    }

    fn dashboard_route(method: &Method, path: &str) -> bool {
        matches!(*method, Method::GET | Method::HEAD) && path == "/api/accounts/dashboard"
    }

    #[cfg(unix)]
    fn app_operation(method: &Method, path: &str) -> Option<Operation> {
        let method = if method == Method::HEAD {
            &Method::GET
        } else {
            method
        };
        Operation::from_route(method, path)
    }

    #[cfg(not(unix))]
    fn app_operation(_method: &Method, _path: &str) -> Option<()> {
        None
    }

    pub(super) async fn handle(&self, request: Request) -> Reply {
        let path = request.uri().path().trim_end_matches('/');
        let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
            return super::service_error();
        };
        if Self::dashboard_route(request.method(), path) {
            let (parts, _) = request.into_parts();
            return self.dashboard(&access, parts.uri, parts.headers).await;
        }
        if let Some(id) = recheck::route(request.method(), path).map(str::to_owned) {
            let reply = match self.usage.as_ref().filter(|_| self.writes) {
                Some(usage) => usage.recheck(&access, &id, request).await,
                None => unavailable(),
            };
            return no_store(reply);
        }
        if let Some(operation) = Self::app_operation(request.method(), path) {
            #[cfg(unix)]
            return no_store(self.app_update(&access, operation, request).await);
            #[cfg(not(unix))]
            return {
                let _ = operation;
                super::plain(404, "API endpoint was not found.")
            };
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
        self.limits
            .blocking(Pool::Reads, move || {
                crate::dashboard_fed::latest_for_access(&cache, &uri, &headers, secure)
            })
            .await
            .unwrap_or_else(|_| unavailable())
    }

    #[cfg(unix)]
    async fn app_update(&self, access: &DashboardAccess, op: Operation, request: Request) -> Reply {
        if !matches!(access.principal, DashboardPrincipal::Browser { .. }) {
            return super::plain(401, "Authentication required");
        }
        if !matches!(op, Operation::Status) {
            if !access.origin_present || !access.same_origin {
                return super::plain(403, "App updates require the dashboard origin.");
            }
            if !json_content(&request) {
                return super::plain(415, "App updates require application/json.");
            }
            if request.uri().query().is_some_and(|q| !q.is_empty())
                || !object_body(request, 100 * 1024)
                    .await
                    .is_some_and(|body| body.is_empty())
            {
                return super::plain(400, "Provide an empty JSON object.");
            }
        }
        let Some(apps) = &self.apps else {
            return unavailable();
        };
        match apps.call(op).await {
            Ok(reply) => super::response(reply.status, reply.body),
            Err(_) => match op {
                Operation::Status => {
                    super::plain(503, "App update status could not be read safely.")
                }
                Operation::Start => super::plain(500, "App updates could not start safely."),
                Operation::Cancel => {
                    super::plain(503, "App update cancellation could not be confirmed.")
                }
            },
        }
    }
}

impl Usage {
    fn companions(&self) -> CompanionInputs {
        CompanionInputs {
            lifecycle: self
                .lifecycle
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .clone(),
            server: Some(self.server.clone()),
            ..Default::default()
        }
    }

    async fn collect_companions(self: Arc<Self>, refresh: bool) -> CompanionInputs {
        let fallback = self.companions();
        tokio::task::spawn_blocking(move || {
            let mut facts = self.companions();
            // The remaining lifecycle adapters may supply capability preflight. Collection
            // and Re-check still run with the Rust facts if that worker is absent.
            if let Some(directory) = &self.worker_directory {
                let active = facts.lifecycle.jobs.clone();
                facts.load_lifecycle(directory);
                facts.lifecycle.jobs.extend(active);
            }
            facts.signins = self.signins.read().map(|report| report.to_json());
            facts.antigravity = self.antigravity.as_ref().and_then(|host| {
                crate::antigravity_runtime::has_profiles(&self.root)
                    .then(|| host.accounts(refresh, super::now()))
                    .flatten()
            });
            facts
        })
        .await
        .unwrap_or(fallback)
    }

    fn publish(&self) {
        let now = super::now();
        if let Some(timestamp) = super::storage::iso(now) {
            self.hub
                .publish_dashboard_cache(&self.cache.hub_dashboards(&timestamp, now));
            self.events.publish(DashboardEvent::AccountsChanged {});
        }
    }

    async fn sample(self: Arc<Self>, cancel: CancellationToken) {
        self.producer
            .run(
                self.cache.clone(),
                cancel,
                |refresh| self.clone().collect_companions(refresh),
                |cache| {
                    let owner = self.clone();
                    async move {
                        owner.publish();
                        #[cfg(unix)]
                        if owner.store {
                            super::native_reads::publish_usage(
                                owner.root.clone(),
                                &cache,
                                super::now(),
                                owner.analytics_writers,
                            )
                            .await;
                        }
                        #[cfg(not(unix))]
                        let _ = &cache;
                    }
                },
            )
            .await;
    }

    async fn watch_signins(
        self: Arc<Self>,
        mut receiver: tokio::sync::broadcast::Receiver<DashboardEvent>,
    ) {
        loop {
            let event = match receiver.recv().await {
                Ok(event) => event,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                    if let Ok(mut lifecycle) = self.lifecycle.lock() {
                        lifecycle.jobs.clear();
                    }
                    continue;
                }
                Err(_) => break,
            };
            let DashboardEvent::SigninJob { job } = event else {
                continue;
            };
            let Some(id) = job.account_id else { continue };
            let Ok(mut lifecycle) = self.lifecycle.lock() else {
                continue;
            };
            match job.state.as_str() {
                "starting" | "waiting" | "awaiting_code" => {
                    lifecycle
                        .jobs
                        .insert(id, json!({"state":"signing_in","jobId":job.id}));
                }
                "verifying" => {
                    lifecycle
                        .jobs
                        .insert(id, json!({"state":"verifying","jobId":job.id}));
                }
                _ => {
                    if lifecycle
                        .jobs
                        .get(&id)
                        .is_some_and(|value| value["jobId"] == job.id)
                    {
                        lifecycle.jobs.remove(&id);
                    }
                }
            }
        }
    }
}

fn json_content(request: &Request) -> bool {
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
