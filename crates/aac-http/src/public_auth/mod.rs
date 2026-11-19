//! Public M6 authentication front door. The private service still owns its Unix protocol.
// Reply errors are complete Axum responses with headers. Keep them inline like successes
// so the guard can return a response without a second allocation.
#![allow(clippy::result_large_err)]
mod analytics_routes;
mod app_updates_owner;
mod compose;
mod dashboard_events;
mod devices;
mod hub;
mod lifecycle;
pub(crate) mod lifecycle_host;
pub(crate) mod native_owners;
#[cfg(unix)]
mod native_reads;
mod native_settings;
// Merged before its first registering group: the host is exercised by the groups that land
// on top of it (W-status-home first), so its API is not dead in the composed cut.
#[allow(dead_code)]
mod owner_host;
mod policy;
mod profile_routes;
mod request_log;
mod reset_routes;
mod routes;
#[cfg(unix)]
mod service_link;
mod sidecar;
mod static_ui;
mod storage;
mod tls;
mod websocket;

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{ConnectInfo, FromRequestParts, Request, State, ws::WebSocketUpgrade},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use devices::{DeviceError, Devices, token_valid};
use policy::{Connection, bearer, device_scope, header, local_access, origin_allowed, public};
use routes::Limits;
use serde_json::{Value, json};
use std::{
    io::IsTerminal,
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use storage::{Config, DAY, Files, Overrides, Sessions, Setup, StoreError, now};
use tokio::sync::{Semaphore, broadcast, watch};

pub use dashboard_events::{
    AppUpdateHostProgress, AppUpdateProgress, DashboardEvent, DashboardEvents, DashboardSignInJob,
    SignInError, SignInResult, SignInVerification,
};
pub use hub::HubCache;

/// Publish the native producer's transport-neutral Accounts rows to the public usage hub.
#[cfg(unix)]
pub fn publish_native_hub(
    directory: &std::path::Path,
    cache: &crate::account_usage::AccountUsageCache,
) {
    let now = now();
    let Some(timestamp) = storage::iso(now) else {
        return;
    };
    hub::publish_native_snapshot(directory, cache.hub_dashboards(&timestamp, now));
}

/// The latest native Accounts rows that carry a saved Codex profile, one per name. The public
/// Codex quota route reads these same collector rows the Accounts dashboard shows.
fn codex_dashboard_rows(cache: &crate::account_usage::AccountUsageCache) -> Vec<Value> {
    let at = now();
    let Some(timestamp) = storage::iso(at) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for dashboard in cache.hub_dashboards(&timestamp, at) {
        for row in dashboard["accounts"].as_array().into_iter().flatten() {
            let Some(name) = row["capabilities"]["codexProfile"].as_str() else {
                continue;
            };
            if seen.insert(name.to_owned()) {
                rows.push(row.clone());
            }
        }
    }
    rows
}

/// Trust facts for downstream public routes, produced exclusively from the accepted request.
#[derive(Clone)]
pub struct DashboardAccess {
    /// Auth configuration was enabled for this request.
    pub auth_enabled: bool,
    /// Identity admitted by the public API guard.
    pub principal: DashboardPrincipal,
    /// The credential transport rules admit this connection as secure.
    pub secure_transport: bool,
    /// The origin passes the dashboard's host and loopback-alias checks.
    pub same_origin: bool,
    /// An Origin header was present and nonempty.
    pub origin_present: bool,
    /// The socket and HTTP authority both establish localhost access.
    pub local_access: bool,
    session_id: Option<String>,
    credential_transport: &'static str,
    direct_loopback: bool,
    lan_proxy: bool,
    client_address: Option<String>,
}

impl DashboardAccess {
    /// The admitted session's identifier for the private service's principal (never a cookie).
    pub(super) fn session_key(&self) -> String {
        self.session_id
            .as_deref()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 128
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
            })
            .unwrap_or("session")
            .to_owned()
    }

    /// The verified principal the private service's route table adjudicates. The host hop
    /// carries this exactly as the TypeScript front door carried its own.
    #[cfg(unix)]
    pub(super) fn host_principal(&self) -> String {
        let (kind, role, id) = match &self.principal {
            DashboardPrincipal::Browser { .. } => (
                "session",
                "admin",
                self.session_id.as_deref().unwrap_or("dashboard"),
            ),
            DashboardPrincipal::Device { id, .. } => ("device", "device", id.as_str()),
            DashboardPrincipal::Anonymous => ("session", "admin", "dashboard"),
        };
        json!({"kind":kind,"id":id,"role":role}).to_string()
    }
}

/// Browser or tray identity admitted from a real session cookie or bearer token.
#[derive(Clone)]
pub enum DashboardPrincipal {
    /// Public or auth-disabled request.
    Anonymous,
    /// Current browser session.
    Browser {
        /// Signed-in account name.
        username: String,
    },
    /// Paired tray restricted to the device allowlist.
    Device {
        /// Public device ID.
        id: String,
        /// Native tray platform.
        platform: String,
    },
}

/// The confirmed TLS bit is set by a native TLS acceptor, never by a request header.
#[derive(Clone, Copy)]
pub struct PublicPeer {
    /// TCP peer from the acceptor.
    pub address: SocketAddr,
    /// The application itself terminated TLS.
    pub tls: bool,
}

#[derive(Clone)]
struct Dashboard {
    runtime: Arc<Mutex<AuthRuntime>>,
    permits: Arc<Semaphore>,
    events: DashboardEvents,
}

/// Independent Rust owner of public dashboard authentication.
#[derive(Clone)]
pub struct DashboardAuth(Dashboard);

impl DashboardAuth {
    /// Opens explicit private-root stores and claims exclusive Rust writer leases.
    /// Run one front door per private root. The TS dashboard must be stopped before a handoff.
    pub fn open(root: PathBuf) -> std::io::Result<Self> {
        Self::open_public(root, std::io::stdout().is_terminal())
    }

    fn open_public(root: PathBuf, interactive: bool) -> std::io::Result<Self> {
        Self::open_with(root, Overrides::from_process(), 12, interactive)
            .map_err(|_| std::io::Error::other("dashboard auth initialisation failed"))
    }

    fn open_with(
        root: PathBuf,
        env: Overrides,
        hash_cost: u32,
        interactive: bool,
    ) -> Result<Self, StoreError> {
        let files = Files::claim(root)?;
        let config = Config::new(files.config()?, &env);
        let credential_stamp = config.stamp();
        files.sync_credential(&config)?;
        let secret = files.secret(&env)?;
        let sessions = Sessions::open(&files, secret);
        let setup = files.setup(&config).unwrap_or_else(|_| {
            tracing::warn!("auth.setup.code_failed");
            None
        });
        if let Some(code) = &setup {
            code.print(interactive);
        }
        let (epoch_changed, _) = watch::channel(0);
        let (events, _) = broadcast::channel(64);
        Ok(Self(Dashboard {
            runtime: Arc::new(Mutex::new(AuthRuntime {
                files,
                env,
                sessions,
                setup,
                hash_cost,
                epoch_changed,
                credential_stamp: Some(credential_stamp),
                login: Limits::default(),
                login_server: Limits::default(),
                password: Limits::default(),
                password_server: Limits::default(),
                rotation: Limits::default(),
            })),
            permits: Arc::new(Semaphore::new(16)),
            events: DashboardEvents(events),
        }))
    }

    /// Adds the auth endpoints and authorization guard to the public router.
    /// Supply socket metadata with ConnectInfo, or insert PublicPeer from a native TLS acceptor.
    /// Keep the private Unix router out of the returned public router.
    pub fn protect(&self, downstream: Router) -> Router {
        downstream.layer(middleware::from_fn_with_state(self.0.clone(), front_door))
    }

    /// Adds the hub and static assets as the public router's terminal handlers.
    /// Merge other lanes' API routers before applying the authentication guard.
    pub fn mount_edges(
        &self,
        downstream: Router,
        cache: HubCache,
        ui_root: Option<PathBuf>,
    ) -> Router {
        self.edges(
            downstream, cache, ui_root, None, false, None, None, None, None, None, None, None, None,
        )
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "the listener modes supply distinct optional runtime owners"
    )]
    fn edges(
        &self,
        downstream: Router,
        cache: HubCache,
        ui_root: Option<PathBuf>,
        snapshot: Option<PathBuf>,
        partial_api: bool,
        backend: Option<compose::Backend>,
        cut_root: Option<PathBuf>,
        native_owners: Option<native_owners::NativeOwners>,
        native_settings: Option<native_settings::Settings>,
        lifecycle_routes: Option<lifecycle_host::Routes>,
        analytics_hop: Option<analytics_routes::Hop>,
        profiles: Option<profile_routes::Profiles>,
        app_updates: Option<app_updates_owner::Owner>,
    ) -> Router {
        let cut = cut_root.is_some();
        let hub = hub::Hub::new(self.0.clone(), cache, snapshot, cut);
        let ui = static_ui::StaticUi::load(ui_root, partial_api);
        let dashboard = self.0.clone();
        downstream.fallback(move |request: Request| async move {
            if let Some(settings) = &native_settings
                && let Some(family) = native_settings::route(request.method(), request.uri().path())
            {
                let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
                    return service_error();
                };
                return settings.handle(&access, family, request).await;
            }
            // The account-id PATCH route shares this path depth with the settings literals.
            // Let those routes keep precedence and accept trailing slashes as the TS router did.
            if let Some(routes) = &lifecycle_routes
                && routes.owns(&request)
            {
                return routes.handle(request).await;
            }
            if let Some(root) = cut_root.as_deref()
                && analytics_routes::owns(request.method(), request.uri().path())
            {
                if analytics_routes::is_capture(request.uri().path()) {
                    return analytics_routes::capture(request, root.to_owned()).await;
                }
                // The request itself stays here: a declined composed read still reaches the
                // worker with its headers and body untouched.
                let access = request.extensions().get::<DashboardAccess>().cloned();
                let query = request.uri().query().map(str::to_owned);
                let cross_site = request
                    .headers()
                    .get("sec-fetch-site")
                    .and_then(|value| value.to_str().ok())
                    == Some("cross-site");
                let head = request.method() == Method::HEAD;
                if let Some(reply) = analytics_routes::analytics(
                    access,
                    query,
                    head,
                    cross_site,
                    root.to_owned(),
                    analytics_hop.clone(),
                )
                .await
                {
                    return reply;
                }
            }
            if let Some(native) = &native_owners
                && native.owns(request.method(), request.uri().path().trim_end_matches('/'))
            {
                return native.handle(request).await;
            }
            if let Some(profiles) = &profiles
                && profile_routes::Profiles::owns(request.method(), request.uri().path())
            {
                return profiles.handle(request).await;
            }
            if let Some(app_updates) = &app_updates
                && app_updates_owner::Owner::owns(request.method(), request.uri().path())
            {
                return app_updates.handle(request).await;
            }
            if let Some(backend) = backend
                && api_path(&request.uri().path().to_ascii_lowercase())
            {
                if !cut || compose::cut_route(request.method(), request.uri().path()) {
                    return backend.forward(request).await;
                }
                return plain(404, "API endpoint was not found.");
            }
            let headers = request.headers();
            let session = (!api_path(&request.uri().path().to_ascii_lowercase()))
                .then(|| {
                    Some((
                        peer(request.extensions())?,
                        headers.clone(),
                        header(headers, "cookie")?.to_owned(),
                    ))
                })
                .flatten();
            let mut reply = if hub::Hub::owns(request.uri().path()) {
                hub.handle(request).await
            } else {
                ui.handle(request).await
            };
            if reply
                .extensions()
                .get::<static_ui::AssetResponse>()
                .is_none()
                && let Some((peer, headers, cookie)) = session
                && let Ok(permit) = dashboard.permits.clone().try_acquire_owned()
                && let Some(cookie) = tokio::task::spawn_blocking(move || {
                    let _permit = permit;
                    let mut state = dashboard.runtime.lock().ok()?;
                    let config = state.load_config().ok()?;
                    let connection =
                        config
                            .network
                            .connection(&headers, peer.address.ip(), peer.tls);
                    let at = now();
                    let (id, mut session) = state.sessions.read(&cookie, &connection, at)?;
                    let AuthRuntime {
                        files, sessions, ..
                    } = &mut *state;
                    sessions.touch(files, &id, &mut session, at, config.days * DAY);
                    sessions.cookie(&id, &session, &connection)
                })
                .await
                .ok()
                .flatten()
            {
                set_header(&mut reply, "set-cookie", &cookie);
            }
            reply
        })
    }

    /// Returns a publisher for dashboard WebSocket hints.
    pub fn events(&self) -> DashboardEvents {
        self.0.events.clone()
    }

    /// Best-effort flush of rolling session expiry on normal shutdown.
    pub fn flush(&self) {
        if let Ok(mut state) = self.0.runtime.lock() {
            let AuthRuntime {
                sessions, files, ..
            } = &mut *state;
            sessions.flush(files);
        }
    }
}

/// Binds only when the caller selected public mode and supplied the listen address and private root.
pub async fn run_public() -> std::io::Result<()> {
    let refused = || {
        std::io::Error::other(
            "public mode requires AAC_RUST_PUBLIC_LISTEN and an explicit CCS_DIR or CCS_HOME",
        )
    };
    let address: SocketAddr = std::env::var("AAC_RUST_PUBLIC_LISTEN")
        .ok()
        .and_then(|value| value.parse().ok())
        .ok_or_else(refused)?;
    let root = storage::root_from_env().ok_or_else(refused)?;
    run_public_at(&[address], root, false, false).await
}

pub(crate) async fn run_public_at(
    addresses: &[SocketAddr],
    root: PathBuf,
    browser: bool,
    cut: bool,
) -> std::io::Result<()> {
    #[cfg(not(feature = "parity-fixtures"))]
    if cut && std::env::var("AAC_RUST_CUT_APP_UPDATES_FIXTURE").as_deref() == Ok("1") {
        return Err(std::io::Error::other(
            "the M0 app-update fixture requires a parity-fixtures build",
        ));
    }
    let mut listener = None;
    let mut last_error = None;
    for address in addresses {
        match tokio::net::TcpListener::bind(address).await {
            Ok(bound) => {
                listener = Some(bound);
                break;
            }
            Err(error) => last_error = Some(error),
        }
    }
    let listener = listener.ok_or_else(|| {
        last_error
            .unwrap_or_else(|| std::io::Error::other("no acceptable dashboard listen address"))
    })?;
    let address = listener.local_addr()?;
    let log = Arc::new(aac_log::Log::new(root.clone()));
    let ui_root = std::env::var_os("AAC_RUST_UI_DIR").map(PathBuf::from);
    let interactive = std::io::stdout().is_terminal()
        || (!cut && std::env::var("AAC_RUST_COMPOSE_STDOUT_TTY").as_deref() == Ok("1"));
    let auth = DashboardAuth::open_public(root.clone(), interactive)?;
    let groups = if cut {
        let config = auth
            .0
            .runtime
            .lock()
            .ok()
            .and_then(|state| state.files.config().ok());
        Some(native_owners::Groups::from_config(
            &config.unwrap_or_default(),
        ))
    } else {
        None
    };
    let enabled = owner_host::enabled();
    #[cfg(unix)]
    let has_external =
        cut && service_link::ServiceLink::load(&root).is_some_and(|link| link.owns_lifecycle());
    #[cfg(not(unix))]
    let has_external = false;
    let groups = groups.map(|mut groups| {
        if has_external {
            groups.collection = false;
        }
        groups
    });
    let mut worker = if cut {
        sidecar::Sidecar::start(
            &root,
            address,
            groups.is_some_and(|groups| groups.collection),
            has_external,
        )
        .await?
    } else {
        None
    };
    let (snapshot, backend) = if cut {
        (
            worker.as_ref().map(|child| child.cache.clone()),
            worker.as_ref().map(|child| child.backend.clone()),
        )
    } else {
        (
            std::env::var_os("AAC_RUST_HUB_CACHE").map(PathBuf::from),
            compose::Backend::from_env(address)?,
        )
    };
    let hub = HubCache::default();
    let composed = backend.is_some();
    let owner_host = (enabled || cut)
        .then(|| owner_host::OwnerHost::new(root.clone(), enabled && cut && !composed));
    let native = if let Some((mut groups, host)) = groups.zip(owner_host.as_ref()) {
        groups.lifecycle = !has_external;
        let worker_directory = snapshot
            .as_deref()
            .and_then(std::path::Path::parent)
            .map(std::path::Path::to_owned);
        host.start_public(groups, hub.clone(), auth.events(), worker_directory)?;
        host.public_owners()
    } else {
        None
    };
    // The settings group registers its own routes when the owner host is on; without the host
    // the `edges` fallback answers them, so the shipped worker keeps its cutover switch.
    let native_settings = cut.then(|| native_settings::Settings::new(root.clone(), auth.events()));
    let snapshot = snapshot.filter(|_| !groups.is_some_and(|groups| groups.collection));
    let lifecycle_routes = owner_host.as_ref().filter(|_| cut).map(|host| {
        let routes = lifecycle_host::Routes::new(host, has_external);
        host.register("lifecycle", routes.router());
        routes
    });
    // The group's one hop to the owner host: the running service's descriptor, or the
    // in-process service a worker-less boot starts. No second link.
    #[cfg(unix)]
    let analytics_hop: Option<analytics_routes::Hop> =
        owner_host
            .as_ref()
            .filter(|_| enabled)
            .map(|host| -> analytics_routes::Hop {
                let host = std::sync::Arc::clone(host);
                std::sync::Arc::new(move |uri: String, principal: String| {
                    let host = std::sync::Arc::clone(&host);
                    Box::pin(async move {
                        host.request("GET", &uri, &principal, &[], None)
                            .await
                            .map(|reply| (reply.status, reply.body))
                    }) as analytics_routes::HopFuture
                })
            });
    #[cfg(not(unix))]
    let analytics_hop: Option<analytics_routes::Hop> = None;
    let (profiles, app_updates) = if let Some(native) = native.as_ref() {
        let (agents, cache) = native.profile_inputs();
        let rows: profile_routes::RowsSource =
            Arc::new(move || cache.as_ref().map(codex_dashboard_rows).unwrap_or_default());
        #[cfg(unix)]
        let (profiles, app_updates) = if let Some(host) = owner_host.as_ref().filter(|_| enabled) {
            (
                profile_routes::Profiles::start_with_host(
                    root.clone(),
                    rows,
                    agents.clone(),
                    host.clone(),
                ),
                app_updates_owner::Owner::start_with_host(
                    root.clone(),
                    auth.events(),
                    agents,
                    host.clone(),
                ),
            )
        } else {
            (
                profile_routes::Profiles::start(root.clone(), rows, agents.clone()),
                app_updates_owner::Owner::start(root.clone(), auth.events(), agents),
            )
        };
        #[cfg(not(unix))]
        let (profiles, app_updates) = (
            profile_routes::Profiles::start(root.clone(), rows, agents.clone()),
            app_updates_owner::Owner::start(root.clone(), auth.events(), agents),
        );
        (Some(profiles), Some(app_updates))
    } else {
        (None, None)
    };
    let mut api = Router::new();
    if let Some(host) = &owner_host {
        if enabled && let Some(settings) = &native_settings {
            host.register("settings", settings.router(Router::new()));
        }
        api = host.mount(api);
    }
    if cut || !composed {
        api = api.route(
            "/api/health",
            axum::routing::get(|| async { response(200, json!({"status":"ok"})) }),
        );
    }
    if cut {
        api = api.route("/api/diag", axum::routing::get(cut_diagnostics));
    }
    let edges = auth.edges(
        api,
        hub,
        ui_root,
        snapshot,
        !composed,
        backend,
        cut.then(|| root.clone()),
        native,
        native_settings,
        lifecycle_routes,
        analytics_hop,
        profiles,
        app_updates,
    );
    let router = auth
        .protect(reset_routes::mount(
            edges,
            root.clone(),
            auth.events(),
            log.clone(),
        ))
        .layer(middleware::from_fn_with_state(
            request_log::Audit {
                dashboard: auth.0.clone(),
                log,
            },
            request_log::record,
        ));
    let router = if cut {
        router.layer(middleware::from_fn(
            |request: Request, next: Next| async move {
                let recheck = native_owners::is_recheck(&request);
                let reply = next.run(request).await;
                if recheck {
                    native_owners::no_store(reply)
                } else {
                    reply
                }
            },
        ))
    } else {
        router
    };
    let tls_server = tls::listener(&auth, address.ip()).await;
    if let Some((address, _)) = &tls_server {
        println!("aac-http public TLS listening on {address}");
    }
    println!("aac-http public listening on {address}");
    if let Some(host) = &owner_host {
        let host = host.clone();
        tokio::spawn(async move { host.warm().await });
    }
    if cut {
        record_public_build(&root);
    }
    if browser {
        crate::cli::open_browser(address);
    }
    // G10: Windows has no private service, so the public process runs the saved-login timer.
    // Unix keeps the one timer the private service starts (two would renew the same logins).
    #[cfg(not(unix))]
    if cut {
        let home = std::env::var_os("USERPROFILE")
            .or_else(|| std::env::var_os("HOME"))
            .map(PathBuf::from)
            .unwrap_or_default();
        let service = aac_renewal::service::CodexProfileRenewalService::new(
            aac_renewal::service::CodexProfileRenewalServiceDeps {
                ccs_dir: root.clone(),
                renewal: aac_renewal::renewal::CodexProfileRenewalOptions {
                    env: aac_renewal::planner::CodexRenewalEnvironment {
                        ccs_dir: root.clone(),
                        codex_home: Some(home.join(".codex")),
                        home_dir: Some(home),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        let (keep, stop) = tokio::sync::watch::channel(false);
        tokio::spawn(async move {
            let _keep = keep;
            std::future::pending::<()>().await;
        });
        tokio::spawn(async move {
            service.run_until(stop).await;
        });
    }
    let shutdown = axum_server::Handle::new();
    let signal = shutdown.clone();
    let feed_shutdown = shutdown.clone();
    let worker_lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let worker_feed = worker
        .as_mut()
        .and_then(|child| child.attach(auth.events(), shutdown.clone(), worker_lost.clone()));
    let feed_task = (!cut && std::env::var("AAC_RUST_PUBLIC_EVENT_STDIN").as_deref() == Ok("1"))
        .then(|| {
            let events = auth.events();
            tokio::spawn(async move {
                if events.read_feed(tokio::io::stdin()).await.is_err() {
                    tracing::warn!("dashboard.event_feed.closed");
                }
                if composed {
                    feed_shutdown.graceful_shutdown(Some(Duration::from_secs(3)));
                }
            })
        });
    let signal_task = tokio::spawn(async move {
        shutdown_signal().await;
        signal.graceful_shutdown(Some(Duration::from_secs(3)));
    });
    let plain_listener = listener.into_std()?;
    let plain = axum_server::from_tcp(plain_listener)?
        .handle(shutdown.clone())
        .serve(
            router
                .clone()
                .into_make_service_with_connect_info::<SocketAddr>(),
        );
    let result = if let Some((_, server)) = tls_server {
        let tls_router = router.layer(middleware::from_fn(tls::mark_encrypted));
        let tls = server
            .handle(shutdown)
            .serve(tls_router.into_make_service_with_connect_info::<SocketAddr>());
        tokio::try_join!(plain, tls).map(|_| ())
    } else {
        plain.await
    };
    signal_task.abort();
    if let Some(feed) = feed_task {
        feed.abort();
    }
    if let Some(feed) = worker_feed {
        feed.abort();
        let _ = feed.await;
    }
    if let Some(host) = &owner_host {
        host.stop_public().await;
    }
    if let Some(host) = &owner_host {
        host.stop().await;
    }
    #[cfg(unix)]
    if let Some(host) = &owner_host {
        host.stop().await;
    }
    if let Some(worker) = worker {
        worker.stop().await;
    }
    auth.flush();
    if result.is_ok() && worker_lost.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(std::io::Error::other(
            "the private dashboard sidecar stopped",
        ));
    }
    result
}

async fn cut_diagnostics(axum::Extension(access): axum::Extension<DashboardAccess>) -> Reply {
    let mut reply = if matches!(access.principal, DashboardPrincipal::Browser { .. }) {
        response(200, json!(aac_diag::diagnostics()))
    } else {
        plain(401, "Authentication required")
    };
    set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn record_public_build(root: &std::path::Path) {
    use aac_files::deployed_build::{read_packaged_build, record_running_build};
    let build = sidecar::package_root()
        .as_deref()
        .and_then(read_packaged_build);
    let home = std::env::home_dir().unwrap_or_default();
    if record_running_build(
        root,
        build.as_ref(),
        std::env::var("CCS_DIR").ok().as_deref(),
        std::env::var("CCS_HOME").ok().as_deref(),
        &home,
        now(),
    ) > 0
    {
        tracing::warn!("web.deployed_build.unrecorded");
    }
}

async fn shutdown_signal() {
    #[cfg(windows)]
    if let Ok(mut terminate) = tokio::signal::windows::ctrl_break() {
        tokio::select! {
            _ = terminate.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
        return;
    }
    #[cfg(unix)]
    {
        if let Ok(mut terminate) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! {
                _ = terminate.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            return;
        }
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn decline() -> Reply {
    error(
        503,
        "route_unavailable",
        "This route is not available on the Rust dashboard.",
    )
}

struct AuthRuntime {
    files: Files,
    env: Overrides,
    sessions: Sessions,
    setup: Option<Setup>,
    hash_cost: u32,
    credential_stamp: Option<String>,
    epoch_changed: watch::Sender<u64>,
    login: Limits,
    login_server: Limits,
    password: Limits,
    password_server: Limits,
    rotation: Limits,
}

impl AuthRuntime {
    fn load_config(&self) -> Result<Config, StoreError> {
        Ok(Config::new(self.files.config()?, &self.env))
    }
    fn update_config(
        &mut self,
        current: &mut Config,
        update: impl FnOnce(&mut Config) -> Result<(), Reply>,
    ) -> Result<(), Reply> {
        let writer = self.files.config_writer()?;
        let mut latest = self.load_config()?;
        if current.stamp() != latest.stamp() {
            return Err(error(
                409,
                "credential_changed",
                "Dashboard sign-in changed. Try again.",
            ));
        }
        update(&mut latest)?;
        if latest.doc.get("version").is_none() {
            // Version 2 is the first unified YAML schema. Older auth-only roots
            // and fresh setup both need a document the retained TS readers can open.
            latest.doc["version"] = json!(2);
        }
        Files::save_config(&writer, &latest.doc)?;
        *current = Config::new(latest.doc, &self.env);
        Ok(())
    }
    fn settle_credential(&mut self, config: &Config) -> bool {
        let stamp = config.stamp();
        if self.credential_stamp.as_ref() != Some(&stamp) {
            if self.files.sync_credential(config).is_err() {
                self.credential_stamp = None;
                return false;
            }
            self.credential_stamp = Some(stamp);
            self.epoch_changed
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        true
    }
    fn mark_credential(&mut self, config: &Config) {
        self.credential_stamp = self
            .files
            .save_credential(config)
            .ok()
            .map(|_| config.stamp());
    }
    fn credential_unchanged(&self, old: &Config) -> bool {
        self.load_config().is_ok_and(|live| {
            live.hash == old.hash && live.username == old.username && live.enabled == old.enabled
        })
    }
    fn bump_epoch(&self) -> Result<(), StoreError> {
        self.files.bump_epoch()?;
        self.epoch_changed
            .send_modify(|generation| *generation = generation.wrapping_add(1));
        Ok(())
    }
}

type Reply = Response;

fn set_header(reply: &mut Reply, name: &'static str, value: &str) {
    if let Ok(value) = HeaderValue::from_str(value) {
        reply
            .headers_mut()
            .insert(HeaderName::from_static(name), value);
    }
}

fn response(status: u16, value: Value) -> Reply {
    json_bytes(status, value.to_string().into_bytes())
}

pub(crate) fn json_bytes(status: u16, bytes: Vec<u8>) -> Reply {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
    use sha1::{Digest, Sha1};
    let length = bytes.len();
    let digest = STANDARD_NO_PAD.encode(Sha1::digest(&bytes));
    let etag = format!(r#"W/"{length:x}-{digest}""#);
    let mut reply = (
        status,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        bytes,
    )
        .into_response();
    set_header(&mut reply, "content-length", &length.to_string());
    set_header(&mut reply, "etag", &etag);
    reply
}

fn empty(status: u16) -> Reply {
    StatusCode::from_u16(status)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        .into_response()
}

pub(crate) fn plain(status: u16, message: &str) -> Reply {
    response(status, json!({"error":message}))
}

fn error(status: u16, code: &str, message: &str) -> Reply {
    let mut reply = response(status, json!({"error":message,"code":code}));
    set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn invalid_body() -> Reply {
    error(
        400,
        "invalid_body",
        "The request body is not valid for this action.",
    )
}

fn service_error() -> Reply {
    plain(500, "The request could not be completed safely.")
}

struct RequestInfo {
    path: String,
    raw_path: String,
    query: Option<String>,
    method: Method,
    headers: HeaderMap,
    body: Value,
    body_bytes: usize,
    json_content: bool,
    has_body: bool,
    connection: Connection,
    tls: bool,
    at: i64,
}

struct Facts {
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    parsed: Value,
    bytes: usize,
    json_content: bool,
    peer: PublicPeer,
}

fn api_path(path: &str) -> bool {
    path == "/api" || path.starts_with("/api/")
}

fn coded_path(path: &str) -> bool {
    [
        "/api/accounts",
        "/api/auth",
        "/api/status",
        "/api/scoreboard",
    ]
    .iter()
    .any(|prefix| {
        path == *prefix
            || path
                .strip_prefix(prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    })
}

pub(super) fn decode_component(raw: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut iter = raw.bytes();
    while let Some(byte) = iter.next() {
        if byte == b'%' {
            let hi = char::from(iter.next()?).to_digit(16)?;
            let lo = char::from(iter.next()?).to_digit(16)?;
            bytes.push((hi * 16 + lo) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}

fn outside_header(req: &RequestInfo) -> bool {
    static SHORT: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| regex::Regex::new("aacd_[A-Za-z0-9_-]{16,}").ok());
    static FULL: LazyLock<Option<regex::Regex>> =
        LazyLock::new(|| regex::Regex::new("aacd_[A-Za-z0-9_-]{43}").ok());
    let (Some(short), Some(full)) = (&*SHORT, &*FULL) else {
        return true;
    };
    if req.query.as_ref().is_some_and(|query| {
        short.is_match(query)
            || decode_component(query).is_some_and(|decoded| short.is_match(&decoded))
    }) {
        return true;
    }
    let mut pending = vec![&req.body];
    while let Some(value) = pending.pop() {
        match value {
            Value::String(text) if full.is_match(text) => return true,
            Value::Array(values) => pending.extend(values),
            Value::Object(object) => {
                for (key, child) in object {
                    if full.is_match(key) {
                        return true;
                    }
                    pending.push(child);
                }
            }
            _ => (),
        }
    }
    false
}

fn ws_refusal(status: u16) -> Reply {
    let message = if status == 404 {
        "WebSocket endpoint not found"
    } else {
        "WebSocket access denied"
    };
    let mut reply = (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        message,
    )
        .into_response();
    set_header(&mut reply, "connection", "close");
    reply
}

enum Decision {
    Reply(Reply),
    Continue(DashboardAccess, Option<String>),
    Upgrade(websocket::Client),
}

fn authorize(state: &mut AuthRuntime, facts: Facts, ws: bool) -> Decision {
    let Ok(mut config) = state.load_config() else {
        return Decision::Reply(service_error());
    };
    let trusted = state.settle_credential(&config);
    let path = facts
        .uri
        .path()
        .strip_suffix('/')
        .filter(|value| !value.is_empty())
        .unwrap_or(facts.uri.path())
        .to_owned();
    let connection =
        config
            .network
            .connection(&facts.headers, facts.peer.address.ip(), facts.peer.tls);
    let req = RequestInfo {
        path,
        raw_path: facts.uri.path().into(),
        query: facts.uri.query().map(Into::into),
        method: facts.method,
        has_body: facts.bytes > 0 || facts.headers.contains_key("transfer-encoding"),
        body_bytes: facts.bytes,
        body: facts.parsed,
        json_content: facts.json_content,
        headers: facts.headers,
        connection,
        tls: facts.peer.tls,
        at: now(),
    };
    let cookie = header(&req.headers, "cookie").unwrap_or_default();
    let mut session = state.sessions.read(cookie, &req.connection, req.at);
    let epoch = state.files.epoch();
    if ws {
        if req.raw_path != "/ws" {
            return Decision::Reply(ws_refusal(404));
        }
        if !origin_allowed(&req.headers) {
            return Decision::Reply(ws_refusal(403));
        }
        let browser_epoch = if config.enabled {
            if bearer(&req.headers).is_some() {
                return Decision::Reply(ws_refusal(403));
            }
            let Some((_, session)) =
                session.filter(|(_, session)| trusted && session.current(epoch))
            else {
                return Decision::Reply(ws_refusal(401));
            };
            Some(session.epoch)
        } else {
            if !req.connection.peer.is_loopback() {
                return Decision::Reply(ws_refusal(403));
            }
            None
        };
        return Decision::Upgrade(websocket::Client {
            epoch: browser_epoch,
            peer: facts.peer,
            headers: req.headers,
            secure_at_upgrade: req.connection.secure,
            changes: state.epoch_changed.subscribe(),
        });
    }

    if let Some((_, value)) = session.as_mut()
        && value.authenticated
        && !(trusted && value.current(epoch))
    {
        value.authenticated = false;
        value.username = None;
        value.epoch = 0;
        value.revoked = true;
    }
    let lower = req.path.to_ascii_lowercase();
    let auth_path = lower == "/api/auth" || lower.starts_with("/api/auth/");
    let local = local_access(&req.headers, req.connection.peer);
    let origin = origin_allowed(&req.headers);
    let mut device = None;
    let mut reply = (|| {
        if config.enabled {
            if outside_header(&req) {
                return Some(error(
                    400,
                    "token_in_query",
                    "Send the device token only in the Authorization header.",
                ));
            }
            if !public(&req.method, &req.path) {
                if let Some(token) = bearer(&req.headers) {
                    if req.method == Method::POST
                        && req.path == "/api/auth/devices/me/rotate"
                        && let Err(reply) = routes::secure(&config, &req)
                    {
                        return Some(reply);
                    }
                    let result = if token_valid(token) {
                        Devices::load(&state.files).and_then(|devices| {
                            devices.authenticate(&state.files, token, &req.connection, req.at)
                        })
                    } else {
                        Err(DeviceError::Invalid)
                    };
                    match result {
                        Ok(candidate) if device_scope(&req.method, &req.path) => {
                            device = Some(candidate)
                        }
                        Ok(candidate) => {
                            tracing::info!(device_id = %candidate.id, reason = "device_scope", "auth.device.rejected");
                            return Some(routes::device_scope());
                        }
                        Err(kind) => {
                            tracing::info!(reason = kind.code(), "auth.device.rejected");
                            return Some(routes::device_read_error(kind));
                        }
                    }
                } else if !session
                    .as_ref()
                    .is_some_and(|(_, session)| session.authenticated)
                {
                    let mut reply = if session.as_ref().is_some_and(|(_, session)| session.revoked)
                    {
                        error(
                            401,
                            "session_revoked",
                            "This browser was signed out. Sign in again.",
                        )
                    } else if coded_path(&lower) {
                        error(401, "auth_required", "Authentication required")
                    } else {
                        plain(401, "Authentication required")
                    };
                    set_header(&mut reply, "cache-control", "no-store");
                    return Some(reply);
                }
            }
        }
        if req.path == "/api/bar" || req.path.starts_with("/api/bar/") {
            if !config.enabled && !local {
                return Some(plain(
                    403,
                    "CCS Bar endpoints require localhost access when dashboard auth is disabled.",
                ));
            }
            if !origin {
                return Some(plain(
                    403,
                    "Native bar probes require the dashboard origin.",
                ));
            }
        } else if !config.enabled
            && !local
            && matches!(
                req.method,
                Method::POST | Method::PUT | Method::PATCH | Method::DELETE
            )
        {
            return Some(plain(
                403,
                "Remote dashboard writes require localhost access when dashboard auth is disabled.",
            ));
        }
        if let Some(reply) =
            routes::dispatch(state, &mut config, &req, &mut session, device.as_ref())
        {
            return Some(reply);
        }
        if auth_path {
            return Some(error(404, "not_found", "API endpoint was not found."));
        }
        None
    })();

    let set_cookie = session.as_mut().and_then(|(id, value)| {
        if reply
            .as_ref()
            .is_some_and(|reply| reply.headers().contains_key("set-cookie"))
        {
            return None;
        }
        state
            .sessions
            .touch(&state.files, id, value, req.at, config.days * DAY);
        state.sessions.cookie(id, value, &req.connection)
    });
    if let Some(mut reply) = reply.take() {
        if auth_path {
            set_header(&mut reply, "cache-control", "no-store");
        }
        if let Some(cookie) = set_cookie {
            set_header(&mut reply, "set-cookie", &cookie);
        }
        return Decision::Reply(reply);
    }
    let session_id = session
        .as_ref()
        .filter(|(_, value)| value.authenticated)
        .map(|(id, _)| id.clone());
    let principal = if let Some(device) = device {
        DashboardPrincipal::Device {
            id: device.id,
            platform: device.platform,
        }
    } else if let Some((_, value)) = session.filter(|(_, value)| value.authenticated) {
        DashboardPrincipal::Browser {
            username: value.username.unwrap_or_default(),
        }
    } else {
        DashboardPrincipal::Anonymous
    };
    Decision::Continue(
        DashboardAccess {
            auth_enabled: config.enabled,
            principal,
            secure_transport: req.connection.secure,
            same_origin: origin,
            origin_present: header(&req.headers, "origin").is_some_and(|value| !value.is_empty()),
            local_access: local,
            session_id,
            credential_transport: req.connection.transport.word(),
            direct_loopback: req.connection.direct_loopback,
            lan_proxy: req.connection.lan_proxy,
            client_address: req.connection.client_address(),
        },
        set_cookie,
    )
}

fn peer(extensions: &axum::http::Extensions) -> Option<PublicPeer> {
    extensions.get::<PublicPeer>().copied().or_else(|| {
        extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| PublicPeer {
                address: info.0,
                tls: false,
            })
    })
}

async fn front_door(State(dashboard): State<Dashboard>, request: Request, next: Next) -> Reply {
    let (mut parts, original_body) = request.into_parts();
    let upgrade = header(&parts.headers, "upgrade")
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let lower = parts.uri.path().to_ascii_lowercase();
    if !upgrade && !api_path(&lower) {
        return next.run(Request::from_parts(parts, original_body)).await;
    }
    let Some(peer) = peer(&parts.extensions) else {
        return service_error();
    };
    let auth_path = lower == "/api/auth" || lower.starts_with("/api/auth/");
    // HTTP/2 exposes the canonical request authority in the URI.
    if !parts.headers.contains_key("host")
        && let Some(authority) = parts.uri.authority()
        && let Ok(host) = HeaderValue::from_str(authority.as_str())
    {
        parts.headers.insert("host", host);
    }
    let mut content_type = header(&parts.headers, "content-type")
        .unwrap_or_default()
        .split(';');
    let json_content = content_type
        .next()
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
    let (parsed, bytes, length) = if json_content && !upgrade {
        if content_type
            .filter_map(|parameter| parameter.split_once('='))
            .any(|(name, value)| {
                name.trim().eq_ignore_ascii_case("charset")
                    && !value
                        .trim()
                        .trim_matches('"')
                        .to_ascii_lowercase()
                        .starts_with("utf-")
            })
        {
            return plain(415, "The request could not be processed.");
        }
        match parse_body(original_body, auth_path).await {
            Ok((value, bytes)) => {
                let length = bytes.len();
                (value, Body::from(bytes), length)
            }
            Err(reply) => return reply,
        }
    } else {
        let length = header(&parts.headers, "content-length")
            .and_then(|value| value.parse().ok())
            .unwrap_or(0);
        (Value::Null, original_body, length)
    };
    let facts = Facts {
        method: parts.method.clone(),
        uri: parts.uri.clone(),
        headers: parts.headers.clone(),
        json_content,
        parsed,
        bytes: length,
        peer,
    };
    let Ok(permit) = dashboard.permits.clone().try_acquire_owned() else {
        return error(503, "auth_busy", "The dashboard is busy. Try again later.");
    };
    let runtime = dashboard.runtime.clone();
    let decision = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        runtime.lock().map_or_else(
            |_| Decision::Reply(service_error()),
            |mut state| authorize(&mut state, facts, upgrade),
        )
    })
    .await
    .unwrap_or_else(|_| Decision::Reply(service_error()));
    let head = parts.method == Method::HEAD;
    let mut result = match decision {
        Decision::Reply(reply) => reply,
        Decision::Continue(access, cookie) => {
            parts.extensions.insert(access);
            let mut reply = next.run(Request::from_parts(parts, bytes)).await;
            if let Some(cookie) = cookie {
                set_header(&mut reply, "set-cookie", &cookie);
            }
            reply
        }
        Decision::Upgrade(client) => {
            let Ok(ws) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await else {
                return (StatusCode::BAD_REQUEST, "Invalid WebSocket upgrade request")
                    .into_response();
            };
            let receiver = dashboard.events.0.subscribe();
            ws.max_message_size(1024 * 1024)
                .max_frame_size(1024 * 1024)
                .on_upgrade(move |socket| {
                    websocket::serve(socket, receiver, client, dashboard.runtime)
                })
        }
    };
    if head {
        *result.body_mut() = Body::empty();
    }
    tracing::info!(status = result.status().as_u16(), "request.completed");
    result
}

async fn parse_body(body: Body, auth: bool) -> Result<(Value, Bytes), Reply> {
    let bytes =
        match tokio::time::timeout(Duration::from_secs(10), to_bytes(body, 100 * 1024)).await {
            Ok(Ok(bytes)) => bytes,
            _ => {
                return Err(if auth {
                    error(413, "body_too_large", "Request body is too large.")
                } else {
                    response(
                        413,
                        json!({"error":"Request body is too large.","code":"body_too_large"}),
                    )
                });
            }
        };
    if bytes.is_empty() {
        return Ok((Value::Null, bytes));
    }
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(value) if value.is_object() || value.is_array() => Ok((value, bytes)),
        _ => Err(if auth {
            error(400, "invalid_json", "Invalid JSON in request body")
        } else {
            response(
                400,
                json!({"error":"Invalid JSON in request body","code":"invalid_json"}),
            )
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use futures_util::StreamExt;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio_tungstenite::{
        connect_async,
        tungstenite::{Error as WsError, Message, client::IntoClientRequest},
    };

    async fn server() -> (
        tempfile::TempDir,
        DashboardAuth,
        SocketAddr,
        tokio::task::JoinHandle<()>,
    ) {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().to_path_buf();
        let hash = bcrypt::hash("fixture-password", 4).unwrap();
        Files::save_config(&aac_files::OwnedStore::claim_dashboard_config(&root).unwrap(),
            &json!({"dashboard_auth":{"enabled":true,"username":"fixture-admin","password_hash":hash}})).unwrap();
        let auth = DashboardAuth::open_with(root, Overrides::default(), 4, false).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = auth.protect(Router::new().fallback(|| async { decline() }));
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (home, auth, addr, handle)
    }

    async fn request(
        addr: SocketAddr,
        path: &str,
        method: &str,
        extra: &str,
        body: &str,
    ) -> String {
        let status = tokio::time::timeout(Duration::from_secs(4), async {
            let mut connection = tokio::net::TcpStream::connect(addr).await.unwrap();
            let wire = format!("{method} {path} HTTP/1.1\r\nHost: {addr}\r\nConnection: close\r\nContent-Length: {}\r\n{extra}\r\n{body}", body.len());
            connection.write_all(wire.as_bytes()).await.unwrap();
            let mut bytes = Vec::new();
            connection.read_to_end(&mut bytes).await.unwrap();
            String::from_utf8(bytes).unwrap()
        }).await;
        status.unwrap()
    }

    async fn login(addr: SocketAddr) -> String {
        let reply = request(
            addr,
            "/api/auth/login",
            "POST",
            "Content-Type: application/json\r\n",
            r#"{"username":"fixture-admin","password":"fixture-password"}"#,
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        reply
            .lines()
            .find_map(|line| line.strip_prefix("set-cookie: "))
            .unwrap()
            .split(';')
            .next()
            .unwrap()
            .into()
    }

    #[cfg(unix)]
    #[test]
    fn auth_accepts_a_legacy_private_group_root_and_bar_folder() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o775)).unwrap();
        let bar = root.join("bar");
        std::fs::create_dir(&bar).unwrap();
        std::fs::set_permissions(&bar, std::fs::Permissions::from_mode(0o775)).unwrap();
        let auth =
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).unwrap();
        assert!(
            auth.0
                .runtime
                .lock()
                .unwrap()
                .files
                .bar_proof("0123456789abcdef")
                .is_ok()
        );
        drop(auth);
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o777)).unwrap();
        assert!(
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).is_err()
        );
    }

    #[test]
    fn public_auth_refuses_a_second_writer_and_a_typescript_session_owner() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let first =
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).unwrap();
        assert!(
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).is_err()
        );
        drop(first);
        let owner = root.join("auth/sessions.owner");
        // The file is a test stand-in for a legacy dashboard's own owner marker.
        std::fs::write(&owner, b"fixture TS process").unwrap();
        assert!(
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).is_err()
        );
        std::fs::remove_file(&owner).unwrap();
        assert!(
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).is_ok()
        );
    }

    #[test]
    #[cfg(unix)]
    fn reclaims_only_proven_dead_legacy_session_owners() {
        use std::{io::Write, os::unix::fs::OpenOptionsExt};
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let open = || DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false);
        drop(open().unwrap());
        let marker = root.join("auth/sessions.owner");
        let (host, boot) = aac_files::process::host_boot().unwrap();
        let mut old_dashboard = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let dead = old_dashboard.id();
        assert!(old_dashboard.wait().unwrap().success());
        let live = std::process::id();
        for (pid, hostname, boot_minute, can_reclaim) in [
            (live, host.as_str(), boot, false),
            (dead, "another-host.invalid", boot, false),
            (dead, host.as_str(), boot + 3, false),
            (dead, host.as_str(), boot, true),
            (live, host.as_str(), boot - 3, true),
        ] {
            let mut owner = std::fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(0o600)
                .open(&marker)
                .unwrap();
            let claim = json!({"pid": pid, "host": hostname, "boot": boot_minute}).to_string();
            owner.write_all(claim.as_bytes()).unwrap();
            drop(owner);
            let result = open();
            assert_eq!(result.is_ok(), can_reclaim, "claim: {claim}");
            if can_reclaim {
                assert_ne!(std::fs::read_to_string(&marker).unwrap(), claim);
                drop(result);
                assert!(!marker.exists());
            } else {
                assert_eq!(std::fs::read_to_string(&marker).unwrap(), claim);
            }
        }
    }

    #[tokio::test]
    async fn websocket_checks_origin_and_principal_and_closes_the_revoked_epoch() {
        let (_home, auth, addr, handle) = server().await;
        let url = format!("ws://{addr}/ws");
        for (field, value, expected) in [
            (None, String::new(), 401),
            (Some("origin"), "https://other.example".to_owned(), 403),
            (
                Some("authorization"),
                "Bearer fixture-token".to_owned(),
                403,
            ),
        ] {
            let mut request = url.clone().into_client_request().unwrap();
            if let Some(field) = field {
                request.headers_mut().insert(
                    HeaderName::from_static(field),
                    HeaderValue::from_str(&value).unwrap(),
                );
            }
            match connect_async(request).await {
                Err(WsError::Http(reply)) => assert_eq!(reply.status().as_u16(), expected),
                _ => panic!("wrong WebSocket upgrade result"),
            }
        }

        let cookie = login(addr).await;
        let mut upgrade = url.into_client_request().unwrap();
        upgrade.headers_mut().insert(
            "origin",
            HeaderValue::from_str(&format!("http://localhost:{}", addr.port())).unwrap(),
        );
        upgrade
            .headers_mut()
            .insert("cookie", HeaderValue::from_str(&cookie).unwrap());
        let (mut socket, reply) = connect_async(upgrade).await.unwrap();
        assert_eq!(reply.status().as_u16(), 101);
        auth.events().publish(DashboardEvent::StatusChanged {});
        let event = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(event, Message::Text(r#"{"type":"status-changed"}"#.into()));
        let revoke = request(
            addr,
            "/api/auth/sessions/revoke-others",
            "POST",
            &format!(
                "Content-Type: application/json\r\nOrigin: http://{addr}\r\nCookie: {cookie}\r\n"
            ),
            "{}",
        )
        .await;
        assert!(revoke.starts_with("HTTP/1.1 200"), "{revoke}");
        let close = tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Close(Some(frame)) = close else {
            panic!("expected WebSocket close");
        };
        assert_eq!(u16::from(frame.code), 4001);
        assert_eq!(frame.reason, "Signed out");
        handle.abort();
        let _ = handle.await;
    }

    #[test]
    fn retired_recursive_launcher_block_survives_auth_start_and_settings_save() {
        for original in [
            "  # Root indentation is valid YAML.\n  dashboard:\n    port: 3000\n  'channels': &channels\n    self: *channels\n    text: |\n      ---\n      ...\n  active_literal: 'retired: not a root key'\n",
            "{ dashboard: {port: 3000}, 'channels': &channels {self: *channels, unicode: 'λ'}, active_literal: 'retired: not a root key' }\n",
        ] {
            let home = tempfile::tempdir().unwrap();
            let root = home.path();
            std::fs::write(root.join("config.yaml"), original).unwrap();
            let auth = DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false)
                .unwrap();
            let mut runtime = auth.0.runtime.lock().unwrap();
            let mut config = runtime.load_config().unwrap();
            runtime
                .update_config(&mut config, |config| {
                    config.doc["dashboard_auth"] = json!({"session_lifetime_days": 30});
                    Ok(())
                })
                .unwrap();
            let saved = std::fs::read_to_string(root.join("config.yaml")).unwrap();
            let (active, retired) = aac_config::split_active(&saved).unwrap();
            let (_, before) = aac_config::split_active(original).unwrap();
            assert_eq!(active["dashboard"]["port"], 3000);
            assert_eq!(active["dashboard_auth"]["session_lifetime_days"], 30);
            assert_eq!(active["active_literal"], "retired: not a root key");
            assert!(retired["channels"].contains("self: *channels"));
            assert!(saved.contains(&before["channels"]));
        }
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn session_lifetime_can_be_saved_after_a_config_writer_crashes() {
        let (home, _auth, addr, handle) = server().await;
        let cookie = login(addr).await;
        let mut writer = std::process::Command::new("sh")
            .args(["-c", "exit 0"])
            .spawn()
            .unwrap();
        let pid = writer.id();
        assert!(writer.wait().unwrap().success());
        let marker = home.path().join("config.yaml.lock");
        std::fs::write(
            &marker,
            format!("{pid}\n{}\ncrashed-writer", storage::now()),
        )
        .unwrap();

        let reply = request(
            addr,
            "/api/auth/session-lifetime",
            "PUT",
            &format!(
                "Content-Type: application/json\r\nOrigin: http://{addr}\r\nCookie: {cookie}\r\n"
            ),
            r#"{"days":30}"#,
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains(r#""days":30"#));
        assert!(!marker.exists());
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn session_lifetime_changes_share_the_config_lease() {
        let (home, _auth, addr, handle) = server().await;
        let cookie = login(addr).await;
        let held = aac_files::OwnedStore::claim_dashboard_config(home.path()).unwrap();
        let headers = format!(
            "Content-Type: application/json\r\nOrigin: http://{addr}\r\nCookie: {cookie}\r\n"
        );
        let reply = request(
            addr,
            "/api/auth/session-lifetime",
            "PUT",
            &headers,
            r#"{"days":30}"#,
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 500"), "{reply}");
        assert!(reply.contains(r#""code":"write_failed""#));
        drop(held);
        let reply = request(
            addr,
            "/api/auth/session-lifetime",
            "PUT",
            &headers,
            r#"{"days":30}"#,
        )
        .await;
        assert!(reply.starts_with("HTTP/1.1 200"), "{reply}");
        assert!(reply.contains(r#""days":30"#));
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn declared_auth_bodies_and_path_errors_match_the_public_contract() {
        let (_home, _auth, addr, handle) = server().await;
        let malformed = request(
            addr,
            "/api/auth/login",
            "POST",
            "Content-Type: application/json\r\n",
            "{",
        )
        .await;
        assert!(malformed.starts_with("HTTP/1.1 400"));
        assert!(malformed.contains(r#""code":"invalid_json""#));
        assert!(malformed.contains("cache-control: no-store"));
        let cookie = login(addr).await;
        let headers = format!("Cookie: {cookie}\r\n");
        let missing = request(addr, "/api/auth/nonexistent", "GET", &headers, "").await;
        assert!(missing.starts_with("HTTP/1.1 404"));
        assert!(missing.contains(r#""code":"not_found""#));
        let head = request(addr, "/api/auth/session", "HEAD", &headers, "").await;
        assert!(head.starts_with("HTTP/1.1 200"));
        assert!(head.ends_with("\r\n\r\n"));
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn private_routes_and_tcp_mode_are_separate() {
        let (_home, _auth, addr, handle) = server().await;
        let cookie = login(addr).await;
        let session = format!("Cookie: {cookie}\r\n");
        let internal = request(addr, "/internal/events", "GET", &session, "").await;
        assert!(internal.starts_with("HTTP/1.1 503"));
        let unsigned = request(addr, "/api/accounts/dashboard", "GET", "", "").await;
        assert!(unsigned.starts_with("HTTP/1.1 401"));
        let signed = request(addr, "/api/accounts/dashboard", "GET", &session, "").await;
        assert!(signed.starts_with("HTTP/1.1 503"));
        handle.abort();
        let _ = handle.await;
    }

    async fn settings_server() -> (tempfile::TempDir, SocketAddr, tokio::task::JoinHandle<()>) {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().to_path_buf();
        let hash = bcrypt::hash("fixture-password", 4).unwrap();
        Files::save_config(
            &aac_files::OwnedStore::claim_dashboard_config(&root).unwrap(),
            &json!({"dashboard_auth":{"enabled":true,"username":"fixture-admin","password_hash":hash}}),
        )
        .unwrap();
        let auth = DashboardAuth::open_with(root.clone(), Overrides::default(), 4, false).unwrap();
        let settings = native_settings::Settings::new(root.clone(), auth.events());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = auth.protect(auth.edges(
            settings.router(Router::new()),
            HubCache::default(),
            None,
            None,
            true,
            None,
            Some(root.clone()),
            None,
            Some(settings),
            None,
            None,
            None,
            None,
        ));
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        (home, addr, handle)
    }

    #[tokio::test]
    async fn settings_heads_keep_the_read_headers_without_a_body() {
        let (_home, addr, handle) = settings_server().await;
        let session = format!("Cookie: {}\r\n", login(addr).await);
        for path in [
            "/api/accounts/settings",
            "/api/accounts/preferences",
            "/api/accounts/visibility",
        ] {
            let get = request(addr, path, "GET", &session, "").await;
            let head = request(addr, path, "HEAD", &session, "").await;
            assert!(get.starts_with("HTTP/1.1 200"), "{get}");
            assert!(head.starts_with("HTTP/1.1 200"), "{head}");
            let (get_headers, _) = get.split_once("\r\n\r\n").unwrap();
            let (head_headers, body) = head.split_once("\r\n\r\n").unwrap();
            assert!(body.is_empty());
            for name in [
                "content-type:",
                "content-length:",
                "cache-control:",
                "etag:",
            ] {
                let expected = get_headers
                    .lines()
                    .find(|line| line.starts_with(name))
                    .unwrap();
                assert!(head_headers.lines().any(|line| line == expected), "{head}");
            }
        }
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn settings_concurrent_preference_patches_preserve_both_clients() {
        let (_home, addr, handle) = settings_server().await;
        let first = format!(
            "Origin: http://{addr}\r\nContent-Type: application/json\r\nCookie: {}\r\n",
            login(addr).await
        );
        let second = format!(
            "Origin: http://{addr}\r\nContent-Type: application/json\r\nCookie: {}\r\n",
            login(addr).await
        );
        let path = "/api/accounts/preferences";
        for _ in 0..20 {
            let reset = request(
                addr,
                path,
                "PUT",
                &first,
                r#"{"timeZone":"UTC","snapshotCleanup":{"auto":false},"usageLogSources":[]}"#,
            )
            .await;
            assert!(reset.starts_with("HTTP/1.1 200"), "{reset}");
            let (zone, cleanup) = tokio::join!(
                request(
                    addr,
                    path,
                    "PATCH",
                    &first,
                    r#"{"timeZone":"Europe/Paris"}"#
                ),
                request(
                    addr,
                    path,
                    "PATCH",
                    &second,
                    r#"{"snapshotCleanup":{"auto":true}}"#
                ),
            );
            assert!(
                zone.starts_with("HTTP/1.1 200") && cleanup.starts_with("HTTP/1.1 200"),
                "{zone}\n{cleanup}"
            );
            let saved = request(addr, path, "GET", &first, "").await;
            assert!(
                saved.contains(r#""timeZone":"Europe/Paris""#)
                    && saved.contains(r#""snapshotCleanup":{"auto":true}"#),
                "{saved}"
            );
        }
        handle.abort();
        let _ = handle.await;
    }

    /// The cut's own settings, preferences and visibility routes: the TS admission sentences
    /// and the files the existing owners write.
    #[tokio::test]
    async fn the_cut_writes_settings_preferences_and_visibility_under_the_ts_guards() {
        let (home, addr, handle) = settings_server().await;
        let root = home.path();
        let cookie = login(addr).await;
        let origin = format!("Origin: http://{addr}\r\n");
        let json = format!("{origin}Content-Type: application/json\r\nCookie: {cookie}\r\n");

        let default = request(
            addr,
            "/api/accounts/settings",
            "GET",
            &format!("Cookie: {cookie}\r\n"),
            "",
        )
        .await;
        assert!(default.starts_with("HTTP/1.1 200"), "{default}");
        assert!(
            default.contains(r#"{"refreshIntervalSeconds":60}"#),
            "{default}"
        );

        let saved = request(
            addr,
            "/api/accounts/settings",
            "PUT",
            &json,
            r#"{"refreshIntervalSeconds":45}"#,
        )
        .await;
        assert!(saved.starts_with("HTTP/1.1 200"), "{saved}");
        assert!(
            saved.contains(r#"{"refreshIntervalSeconds":45}"#),
            "{saved}"
        );
        assert_eq!(
            std::fs::read(root.join("account-refresh-settings.json")).unwrap(),
            b"{\"refreshIntervalSeconds\":45}\n"
        );
        let alias = request(
            addr,
            "/api/accounts/settings/",
            "GET",
            &format!("Cookie: {cookie}\r\n"),
            "",
        )
        .await;
        assert!(alias.starts_with("HTTP/1.1 200"), "{alias}");
        assert!(
            alias.contains(r#"{"refreshIntervalSeconds":45}"#),
            "{alias}"
        );

        let shape = request(
            addr,
            "/api/accounts/settings",
            "PUT",
            &json,
            r#"{"refreshIntervalSeconds":29}"#,
        )
        .await;
        assert!(shape.starts_with("HTTP/1.1 400"), "{shape}");
        assert!(
            shape.contains("Choose a whole number of seconds from 30 to 3600."),
            "{shape}"
        );

        let no_origin = request(
            addr,
            "/api/accounts/settings",
            "PUT",
            &format!("Content-Type: application/json\r\nCookie: {cookie}\r\n"),
            r#"{"refreshIntervalSeconds":60}"#,
        )
        .await;
        assert!(no_origin.starts_with("HTTP/1.1 403"), "{no_origin}");

        let visibility = request(
            addr,
            "/api/accounts/visibility",
            "PUT",
            &json,
            r#"{"hiddenProviders":["claude"]}"#,
        )
        .await;
        assert!(visibility.starts_with("HTTP/1.1 200"), "{visibility}");
        assert!(
            visibility.contains(
                r#"{"hiddenProviders":["claude"],"hiddenAccountIds":[],"trayHiddenProviders":[],"trayHiddenAccountIds":[]}"#
            ),
            "{visibility}"
        );
        let read = request(
            addr,
            "/api/accounts/visibility",
            "GET",
            &format!("Cookie: {cookie}\r\n"),
            "",
        )
        .await;
        assert!(read.contains(r#""hiddenProviders":["claude"]"#), "{read}");

        let preferences = request(
            addr,
            "/api/accounts/preferences",
            "PUT",
            &json,
            r#"{"timeZone":"UTC","snapshotCleanup":{"auto":false},"usageLogSources":[]}"#,
        )
        .await;
        assert!(preferences.starts_with("HTTP/1.1 200"), "{preferences}");
        assert!(preferences.contains(r#""timeZone":"UTC""#), "{preferences}");
        let patched = request(
            addr,
            "/api/accounts/preferences",
            "PATCH",
            &json,
            r#"{"addUsageLogSource":{"id":"extra","tool":"claude-code","host":"ubuntu","path":"/var/log/extra"}}"#,
        )
        .await;
        assert!(patched.starts_with("HTTP/1.1 200"), "{patched}");
        assert!(patched.contains(r#""id":"extra""#), "{patched}");
        let overlap = request(
            addr,
            "/api/accounts/preferences",
            "PATCH",
            &json,
            r#"{"addUsageLogSource":{"id":"again","tool":"claude-code","host":"ubuntu","path":"/home/fixture/.claude/projects/nested"}}"#,
        )
        .await;
        assert!(overlap.starts_with("HTTP/1.1 400"), "{overlap}");
        assert!(overlap.contains("overlaps a built-in root"), "{overlap}");

        handle.abort();
        let _ = handle.await;
    }
}
