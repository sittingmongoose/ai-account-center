//! Public M6 authentication front door. The private service still owns its Unix protocol.
// Reply errors are complete Axum responses with headers. Keep them inline like successes
// so the guard can return a response without a second allocation.
#![allow(clippy::result_large_err)]
mod analytics_routes;
mod app_updates_owner;
mod compose;
mod dashboard_events;
mod devices;
mod home_group;
mod hub;
mod lifecycle;
pub(crate) mod lifecycle_host;
mod native_group;
pub(crate) mod native_owners;
#[cfg(unix)]
mod native_reads;
pub(crate) mod native_settings;
mod owner_host;
mod policy;
mod profile_routes;
mod request_log;
mod reset_routes;
mod routes;
#[cfg(unix)]
mod service_link;
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
    /// The admitted principal this request carries to the private route table.
    pub(crate) fn service_principal(&self) -> Option<String> {
        match &self.principal {
            DashboardPrincipal::Browser { .. } => self
                .session_id
                .as_ref()
                .map(|id| json!({"kind": "session", "id": id, "role": "admin"}).to_string()),
            DashboardPrincipal::Device { id, .. } => {
                Some(json!({"kind": "device", "id": id, "role": "device"}).to_string())
            }
            DashboardPrincipal::Anonymous => None,
        }
    }

    #[cfg(all(test, unix))]
    pub(super) fn test_browser() -> Self {
        Self {
            auth_enabled: true,
            principal: DashboardPrincipal::Browser {
                username: "fixture".into(),
            },
            secure_transport: true,
            same_origin: true,
            origin_present: true,
            local_access: true,
            session_id: Some("f".repeat(64)),
            credential_transport: "loopback",
            direct_loopback: true,
            lan_proxy: false,
            client_address: None,
        }
    }

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
            downstream, cache, ui_root, None, false, None, None, None, None, None, None, None,
            None, None,
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
        native_group: Option<Arc<native_group::Routes>>,
    ) -> Router {
        let cut = cut_root.is_some();
        let hub = hub::Hub::new(self.0.clone(), cache, snapshot);
        let ui = static_ui::StaticUi::load(ui_root, partial_api);
        let dashboard = self.0.clone();
        let capture_writes = Arc::new(tokio::sync::Mutex::new(()));
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
                    return analytics_routes::capture(
                        request,
                        root.to_owned(),
                        Arc::clone(&capture_writes),
                    )
                    .await;
                }
                let access = request.extensions().get::<DashboardAccess>().cloned();
                let query = request.uri().query().map(str::to_owned);
                let cross_site = request
                    .headers()
                    .get("sec-fetch-site")
                    .and_then(|value| value.to_str().ok())
                    == Some("cross-site");
                let head = request.method() == Method::HEAD;
                let accept_gzip = crate::compress::accepts_gzip(request.headers());
                let if_none_match = header(request.headers(), "if-none-match").map(str::to_owned);
                let no_cache = crate::compress::has_no_cache(request.headers());
                return analytics_routes::analytics(
                    access,
                    query,
                    head,
                    cross_site,
                    accept_gzip,
                    if_none_match,
                    no_cache,
                    analytics_hop.clone(),
                )
                .await;
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
            if let Some(group) = &native_group
                && group.owns(request.method(), request.uri().path().trim_end_matches('/'))
            {
                let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
                    return service_error();
                };
                return native_group::handle(group.as_ref(), &access, request).await;
            }
            if !cut
                && let Some(backend) = backend
                && api_path(&request.uri().path().to_ascii_lowercase())
            {
                return backend.forward(request).await;
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
    #[cfg(unix)]
    let root = storage::resolve_public_root(root)?;
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
    let adopted = if cut {
        service_link::ServiceLink::acquire_lifecycle(&root).await?
    } else {
        None
    };
    #[cfg(unix)]
    let has_external = adopted.is_some();
    #[cfg(not(unix))]
    let has_external = false;
    // A live adopted producer keeps its lease and serves this boot's reads.
    let native_collection = has_external || groups.is_some_and(|groups| groups.collection);
    #[cfg(unix)]
    let adopted_quotas = adopted.clone();
    let groups = groups.map(|mut groups| {
        if has_external {
            groups.collection = false;
        }
        groups
    });
    let (snapshot, backend) = if cut {
        (None, None)
    } else {
        (
            std::env::var_os("AAC_RUST_HUB_CACHE").map(PathBuf::from),
            compose::Backend::from_env(address)?,
        )
    };
    let hub = HubCache::for_collection(!cut || native_collection);
    let composed = backend.is_some();
    let owner_host =
        (enabled || cut).then(|| owner_host::OwnerHost::new(root.clone(), cut && !composed));
    // The boot's one `Agents` pool, shared by the public owners and the embedded
    // private service so the `local_work` gate is not split (MEM7 lane 2). The
    // canonicalized root matches what `NativeOwners::start` used for its pool.
    let agents = std::sync::Arc::new(crate::agent_transport::Agents::with_default(
        root.canonicalize().unwrap_or_else(|_| root.clone()),
        true,
    ));
    #[cfg(unix)]
    if let Some(host) = owner_host.as_ref().filter(|_| cut) {
        host.start_cut(
            adopted.clone(),
            groups.is_some_and(|groups| groups.renewal),
            std::sync::Arc::clone(&agents),
        )
        .await?;
    }
    let native = if let Some((mut groups, host)) = groups.zip(owner_host.as_ref()) {
        groups.lifecycle = !has_external;
        let started = host.start_public(groups, hub.clone(), auth.events(), agents.clone());
        if let Err(error) = started {
            host.stop().await;
            return Err(error);
        }
        host.public_owners()
    } else {
        None
    };
    // Settings may register directly or use the native edge handlers.
    let native_settings = cut.then(|| native_settings::Settings::new(root.clone(), auth.events()));
    let snapshot = snapshot.filter(|_| !groups.is_some_and(|groups| groups.collection));
    let group_routes = owner_host
        .as_ref()
        .map(|host| Arc::new(native_group::Routes::new(host.clone(), cut, enabled)));
    let lifecycle_routes = owner_host.as_ref().filter(|_| cut).map(|host| {
        let routes = lifecycle_host::Routes::new(host);
        #[cfg(unix)]
        let routes = routes.with_private(adopted);
        host.register("lifecycle", routes.router());
        routes
    });
    // The group's one hop to the owner host: the running service's descriptor, or the
    // in-process service a worker-less boot starts. No second link.
    let analytics_hop: Option<analytics_routes::Hop> =
        owner_host
            .as_ref()
            .filter(|_| cut)
            .map(|host| -> analytics_routes::Hop {
                let host = std::sync::Arc::clone(host);
                std::sync::Arc::new(
                    move |uri: String, principal: String, headers: Vec<(String, String)>| {
                        let host = std::sync::Arc::clone(&host);
                        Box::pin(async move {
                            let refs: Vec<(&str, &str)> = headers
                                .iter()
                                .map(|(k, v)| (k.as_str(), v.as_str()))
                                .collect();
                            host.analytics(&uri, &principal, &refs).await
                        }) as analytics_routes::HopFuture
                    },
                )
            });
    let (profiles, app_updates) = if let Some(native) = native.as_ref() {
        let (agents, cache) = native.profile_inputs();
        let rows: profile_routes::RowsSource = Arc::new(move || {
            cache
                .as_ref()
                .map(|cache| crate::codex_quotas::rows_from_cache(cache, now()))
                .unwrap_or_default()
        });
        let collector = native.clone();
        let quota_rows: profile_routes::QuotaRows = Arc::new(move |at| {
            let collector = collector.clone();
            Box::pin(async move { collector.codex_profile_rows(at).await })
        });
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
        let profiles = profiles.with_quota_rows(quota_rows);
        #[cfg(unix)]
        let profiles = profiles.with_adopted_quotas(adopted_quotas);
        (Some(profiles), Some(app_updates))
    } else {
        (None, None)
    };
    let mut api = Router::new();
    if let Some(host) = &owner_host {
        if enabled && let Some(settings) = &native_settings {
            host.register("settings", settings.router(Router::new()));
        }
        if cut
            && native_collection
            && groups.is_some_and(|groups| groups.home)
            && let Some(profiles) = &profiles
            && let Some(collection) = &lifecycle_routes
        {
            let home = Arc::new(home_group::Home::new(
                host,
                root.clone(),
                profiles.open_owner(),
                collection.clone(),
            ));
            host.register("home", home.router(Router::new()));
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
        hub.clone(),
        ui_root,
        snapshot,
        !cut && !composed,
        backend,
        cut.then(|| root.clone()),
        native,
        native_settings,
        lifecycle_routes,
        analytics_hop,
        profiles,
        app_updates,
        group_routes,
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
    let status_event_task = if let Some(host) = &owner_host
        && (cut || enabled)
    {
        let host = host.clone();
        let events = auth.events();
        Some(tokio::spawn(
            async move { host.status_events(events).await },
        ))
    } else {
        None
    };
    if cut {
        #[cfg(windows)]
        if let Some(host) = &owner_host {
            let host = host.clone();
            tokio::spawn(async move { host.start_analytics_jobs().await });
        }
        record_public_build(&root);
    }
    if browser {
        crate::cli::open_browser(address);
    }
    // G10: Windows has no private service, so the public process runs the saved-login timer.
    // Unix keeps the one timer the private service starts (two would renew the same logins).
    #[cfg(not(unix))]
    let renewal = groups
        .filter(|groups| groups.renewal)
        .and_then(|_| owner_host::renewal_home())
        .map(|home| {
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
            (
                keep,
                tokio::spawn(async move { service.run_until(stop).await }),
            )
        });
    let shutdown = axum_server::Handle::new();
    let signal = shutdown.clone();
    let feed_shutdown = shutdown.clone();
    #[cfg(unix)]
    let owner_lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
    #[cfg(unix)]
    let owner_watch = owner_host.as_ref().filter(|_| cut).map(|host| {
        let host = host.clone();
        let shutdown = shutdown.clone();
        let lost = owner_lost.clone();
        let hub = hub.clone();
        tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            host.watch_cut(&hub).await;
            lost.store(true, std::sync::atomic::Ordering::Release);
            shutdown.graceful_shutdown(Some(Duration::from_secs(3)));
        }))
    });
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
        #[cfg(all(windows, feature = "parity-fixtures"))]
        if std::env::var("AAC_RUST_CUT_FIXTURE_STOP_STDIN").as_deref() == Ok("1") {
            // The M0 harness restarts the process to reset auth rate limits. Windows
            // child.kill() terminates it without giving the store a chance to seal.
            let _ = tokio::io::copy(&mut tokio::io::stdin(), &mut tokio::io::sink()).await;
        } else {
            shutdown_signal().await;
        }
        #[cfg(not(all(windows, feature = "parity-fixtures")))]
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
    if let Some(task) = status_event_task {
        task.abort();
    }
    if let Some(feed) = feed_task {
        feed.abort();
    }
    #[cfg(unix)]
    drop(owner_watch);
    if let Some(host) = &owner_host {
        host.stop_public().await;
    }
    #[cfg(not(unix))]
    if let Some((stop, task)) = renewal {
        let _ = stop.send(true);
        let _ = task.await;
    }
    if let Some(host) = &owner_host {
        host.stop().await;
    }
    auth.flush();
    #[cfg(unix)]
    if result.is_ok() && owner_lost.load(std::sync::atomic::Ordering::Acquire) {
        return Err(std::io::Error::other("the private dashboard owner stopped"));
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

fn running_package_root() -> Option<PathBuf> {
    let exe = crate::identity::executable_path_for_assets().ok()?;
    exe.ancestors()
        .skip(1)
        .find(|root| root.join("package.json").is_file() && root.join("dist").is_dir())
        .map(std::path::Path::to_path_buf)
}

fn record_public_build(root: &std::path::Path) {
    use aac_files::deployed_build::{read_packaged_build, record_running_build};
    let build = running_package_root()
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
        self.files.verify_owner()?;
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
    let raw_len = bytes.len();
    let etag = crate::compress::weak_etag_sha1(&bytes);
    json_bytes_tagged(status, Bytes::from(bytes), raw_len, false, &etag)
}

pub(crate) fn json_bytes_tagged(
    status: u16,
    bytes: Bytes,
    raw_len: usize,
    gzip: bool,
    etag: &str,
) -> Reply {
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let length = bytes.len();
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
    set_header(&mut reply, "etag", etag);
    if gzip {
        set_header(&mut reply, "content-encoding", "gzip");
        set_header(
            &mut reply,
            crate::compress::RAW_LENGTH_HEADER,
            &raw_len.to_string(),
        );
    }
    reply
}

fn empty(status: u16) -> Reply {
    StatusCode::from_u16(status)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        .into_response()
}

/// Preserve the conditional GET/HEAD behavior of an Express JSON response.
fn revalidate_json(mut reply: Reply, headers: &HeaderMap) -> Reply {
    if reply.status().is_success()
        && header(reply.headers(), "etag").is_some_and(|etag| static_ui::fresh(headers, etag, None))
    {
        *reply.status_mut() = StatusCode::NOT_MODIFIED;
        reply.headers_mut().remove(axum::http::header::CONTENT_TYPE);
        reply
            .headers_mut()
            .remove(axum::http::header::CONTENT_LENGTH);
        reply
            .headers_mut()
            .remove(axum::http::header::CONTENT_ENCODING);
        reply
            .headers_mut()
            .remove(crate::compress::RAW_LENGTH_HEADER);
        *reply.body_mut() = Body::empty();
    }
    reply
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
        let browser_session = if config.enabled {
            if bearer(&req.headers).is_some() {
                return Decision::Reply(ws_refusal(403));
            }
            let Some((id, session)) =
                session.filter(|(_, session)| trusted && session.current(epoch))
            else {
                return Decision::Reply(ws_refusal(401));
            };
            Some(state.sessions.watch(&id, session.epoch))
        } else {
            if !req.connection.peer.is_loopback() {
                return Decision::Reply(ws_refusal(403));
            }
            None
        };
        return Decision::Upgrade(websocket::Client {
            session: browser_session,
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
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
        && !reset_routes::bodyless(&parts.method, parts.uri.path());
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
    let accept_gzip = crate::compress::accepts_gzip(&parts.headers);
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
    if !upgrade {
        result = crate::compress::maybe_compress_api_json(result, accept_gzip, head).await;
    } else if head {
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

    mod analytics;

    type TestServer = (
        tempfile::TempDir,
        DashboardAuth,
        SocketAddr,
        tokio::task::JoinHandle<()>,
    );

    async fn server() -> TestServer {
        server_with_analytics(false, None).await
    }

    async fn server_with_analytics(cut: bool, hop: Option<analytics_routes::Hop>) -> TestServer {
        let home = tempfile::tempdir().unwrap();
        let root = home.path().canonicalize().unwrap();
        let hash = bcrypt::hash("fixture-password", 4).unwrap();
        Files::save_config(&aac_files::OwnedStore::claim_dashboard_config(&root).unwrap(),
            &json!({"dashboard_auth":{"enabled":true,"username":"fixture-admin","password_hash":hash}})).unwrap();
        let auth = DashboardAuth::open_with(root.clone(), Overrides::default(), 4, false).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = if cut {
            auth.edges(
                Router::new(),
                HubCache::default(),
                None,
                None,
                true,
                None,
                Some(root.clone()),
                None,
                None,
                None,
                hop,
                None,
                None,
                None,
            )
        } else {
            Router::new().fallback(|| async { decline() })
        };
        let router = if cut {
            reset_routes::mount(
                router,
                root.clone(),
                auth.events(),
                Arc::new(aac_log::Log::new(root)),
            )
        } else {
            router
        };
        let router = auth.protect(router);
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

    /// Run the lease-sensitive cases in a child process that runs only this test. A sibling
    /// test's fork inherits any lease descriptor held at fork time and keeps it until exec, so
    /// the final open here can see a transient busy lease after the owner dropped it
    /// (session-owner-second-writer-flake-T1). The child has no sibling forks. It must actually
    /// run and pass the cases: a filter that matches nothing fails here instead of passing.
    fn isolated_lease_case(name: &str, cases: fn()) {
        const CHILD: &str = "AAC_TOOL_BATCH_AUTH_CHILD";
        if std::env::var_os(CHILD).is_some() {
            cases();
            return;
        }
        let exe = std::env::current_exe().unwrap();
        let out = std::process::Command::new(exe)
            .arg("--exact")
            .arg(name)
            .arg("--test-threads=1")
            .env(CHILD, "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains("test result: ok. 1 passed"),
            "the isolated lease case did not run and pass:\n{text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    #[test]
    fn public_auth_refuses_a_second_writer_and_a_typescript_session_owner() {
        isolated_lease_case(
            "public_auth::tests::public_auth_refuses_a_second_writer_and_a_typescript_session_owner",
            public_auth_refuses_a_second_writer_cases,
        );
    }

    fn public_auth_refuses_a_second_writer_cases() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let first =
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).unwrap();
        // The second writer is refused because this process holds the claim (Busy), not
        // for any other reason; the isolation above reaps the inherited-fd race.
        let second = aac_files::auth_owner::SessionOwner::take(root);
        assert_eq!(
            second.as_ref().err().map(|e| e.cause),
            Some(aac_core::Cause::Busy),
            "{:?}",
            second.as_ref().err()
        );
        assert!(
            DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false).is_err(),
            "second DashboardAuth writer"
        );
        drop(first);
        let owner = root.join("auth/sessions.owner");
        // The file is a test stand-in for a legacy dashboard's own owner marker.
        std::fs::write(&owner, b"fixture TS process").unwrap();
        let occupied = aac_files::auth_owner::SessionOwner::take(root);
        assert_eq!(
            occupied.as_ref().err().map(|e| e.cause),
            Some(aac_core::Cause::Busy),
            "{:?}",
            occupied.as_ref().err()
        );
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
        use std::{io::Write, os::unix::fs::OpenOptionsExt, process::Command};

        const ISOLATED: &str = "AAC_TEST_ISOLATED_SESSION_OWNER";
        if std::env::var_os(ISOLATED).is_none() {
            // Another test can fork while the session lease is held. Its child inherits
            // the lock until exec, so run the reclaim assertions in a separate process.
            let child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "public_auth::tests::reclaims_only_proven_dead_legacy_session_owners",
                ])
                .env(ISOLATED, "1")
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&child.stdout);
            assert!(
                child.status.success() && stdout.contains("1 passed; 0 failed"),
                "{stdout}\n{}",
                String::from_utf8_lossy(&child.stderr)
            );
            return;
        }

        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let open = || DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4, false);
        drop(open().unwrap());
        let marker = root.join("auth/sessions.owner");
        let (host, boot) = aac_files::process::host_boot().unwrap();
        let mut old_dashboard = Command::new("sh").args(["-c", "exit 0"]).spawn().unwrap();
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

    #[tokio::test]
    async fn auth_check_reports_ws_allowed_for_signed_in_and_signed_out_browsers() {
        let (_home, _auth, addr, handle) = server().await;
        let cookie = login(addr).await;
        let signed_in = request(
            addr,
            "/api/auth/check",
            "GET",
            &format!("Cookie: {cookie}\r\n"),
            "",
        )
        .await;
        assert!(signed_in.starts_with("HTTP/1.1 200"), "{signed_in}");
        let body: Value =
            serde_json::from_str(signed_in.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["authenticated"], json!(true));
        assert_eq!(body["wsAllowed"], json!(true));
        let signed_out = request(addr, "/api/auth/check", "GET", "", "").await;
        assert!(signed_out.starts_with("HTTP/1.1 200"), "{signed_out}");
        let body: Value =
            serde_json::from_str(signed_out.split_once("\r\n\r\n").unwrap().1).unwrap();
        assert_eq!(body["authenticated"], json!(false));
        assert_eq!(body["wsAllowed"], json!(false));
        handle.abort();
        let _ = handle.await;
    }

    type SessionSocket = tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >;

    async fn session_socket(addr: SocketAddr, cookie: &str, auth: &DashboardAuth) -> SessionSocket {
        let mut upgrade = format!("ws://{addr}/ws").into_client_request().unwrap();
        upgrade
            .headers_mut()
            .insert("cookie", HeaderValue::from_str(cookie).unwrap());
        let (mut socket, _) = connect_async(upgrade).await.unwrap();
        auth.events().publish(DashboardEvent::StatusChanged {});
        assert_eq!(
            socket_message(&mut socket).await,
            Message::Text(r#"{"type":"status-changed"}"#.into())
        );
        socket
    }

    async fn socket_message(socket: &mut SessionSocket) -> Message {
        tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .expect("expected a WebSocket message within two seconds")
            .unwrap()
            .unwrap()
    }

    fn expect_signed_out(message: Message) {
        let Message::Close(Some(frame)) = message else {
            panic!("expected sign-out close, received another message");
        };
        assert_eq!(u16::from(frame.code), 4001);
        assert_eq!(frame.reason, "Signed out");
    }

    fn verification_event() -> DashboardEvent {
        serde_json::from_value(json!({
            "type": "signin-job",
            "job": {
                "id": "job_0123456789abcdef", "provider": "codex", "kind": "device-code",
                "mode": "add", "accountId": null, "profileName": null, "platform": "ubuntu",
                "state": "waiting", "result": null, "error": null,
                "verification": {"url": "https://example.test/verify", "userCode": "fixture-code", "expiresAt": null},
                "startedAt": "start", "updatedAt": "update", "expiresAt": "expiry"
            }
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn websocket_never_sends_a_later_verification_code_to_the_signed_out_session() {
        let (_home, auth, addr, handle) = server().await;
        let cookie = login(addr).await;
        let active = login(addr).await;
        let mut active_socket = session_socket(addr, &active, &auth).await;
        let mut signed_out = session_socket(addr, &cookie, &auth).await;
        socket_message(&mut active_socket).await;
        let logout = request(
            addr,
            "/api/auth/logout",
            "POST",
            &format!(
                "Content-Type: application/json\r\nOrigin: http://{addr}\r\nCookie: {cookie}\r\n"
            ),
            "{}",
        )
        .await;
        assert_eq!(logout.lines().next(), Some("HTTP/1.1 200 OK"));
        auth.events().publish(verification_event());
        let Message::Text(text) = socket_message(&mut active_socket).await else {
            panic!("the active session did not receive the sign-in event");
        };
        let event: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(event["job"]["verification"]["userCode"], "fixture-code");
        let message = socket_message(&mut signed_out).await;
        assert!(
            !matches!(&message, Message::Text(text) if text.contains("fixture-code")),
            "signed-out socket received the later verification code"
        );
        expect_signed_out(message);
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn websocket_signals_logout_and_replacement_only_to_the_affected_session() {
        let (_home, auth, addr, handle) = server().await;
        let other = login(addr).await;
        let mut other_socket = session_socket(addr, &other, &auth).await;
        for route in ["logout", "login"] {
            let cookie = login(addr).await;
            let mut expired_socket = session_socket(addr, &cookie, &auth).await;
            let action = request(
                addr,
                &format!("/api/auth/{route}"),
                "POST",
                &format!(
                    "Content-Type: application/json\r\nOrigin: http://{addr}\r\nCookie: {cookie}\r\n"
                ),
                if route == "login" {
                    r#"{"username":"fixture-admin","password":"fixture-password"}"#
                } else {
                    "{}"
                },
            )
            .await;
            assert_eq!(action.lines().next(), Some("HTTP/1.1 200 OK"), "{route}");
            // A per-session change must wake this socket without an event or a periodic tick.
            expect_signed_out(socket_message(&mut expired_socket).await);
        }
        auth.events().publish(DashboardEvent::StatusChanged {});
        // Opening the two sockets above also sent two status events to the unaffected session.
        for _ in 0..3 {
            assert_eq!(
                socket_message(&mut other_socket).await,
                Message::Text(r#"{"type":"status-changed"}"#.into())
            );
        }
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn websocket_checks_idle_expiry_before_delivery_and_on_the_periodic_tick() {
        let (_home, auth, addr, handle) = server().await;
        for periodic in [false, true] {
            let cookie = login(addr).await;
            let mut socket = session_socket(addr, &cookie, &auth).await;
            {
                let mut runtime = auth.0.runtime.lock().unwrap();
                let connection = runtime.load_config().unwrap().network.connection(
                    &HeaderMap::new(),
                    addr.ip(),
                    false,
                );
                let at = now();
                let (id, mut session) = runtime.sessions.read(&cookie, &connection, at).unwrap();
                let AuthRuntime {
                    sessions, files, ..
                } = &mut *runtime;
                // Move the record's last HTTP use into the past without deleting it or
                // changing the epoch. The socket itself never rolls the idle deadline.
                sessions.touch(files, &id, &mut session, at - 366 * DAY, DAY);
                assert!(sessions.read(&cookie, &connection, at).is_none());
            }
            if periodic {
                tokio::time::pause();
                tokio::time::advance(Duration::from_secs(31)).await;
                tokio::time::resume();
            } else {
                auth.events().publish(DashboardEvent::StatusChanged {});
            }
            expect_signed_out(socket_message(&mut socket).await);
        }
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn unavailable_paired_device_count_survives_a_successful_password_change() {
        let (home, _auth, addr, handle) = server().await;
        let cookie = login(addr).await;
        let headers = format!("Cookie: {cookie}\r\n");
        let json_headers =
            format!("{headers}Origin: http://{addr}\r\nContent-Type: application/json\r\n");
        let json_reply = |reply: &str| -> Value {
            serde_json::from_str(reply.split_once("\r\n\r\n").unwrap().1).unwrap()
        };
        let summary = request(addr, "/api/auth/session", "GET", &headers, "").await;
        assert_eq!(json_reply(&summary)["pairedDevices"], 0);

        let paired = request(
            addr,
            "/api/auth/devices/pair",
            "POST",
            &json_headers,
            r#"{"username":"fixture-admin","password":"fixture-password","deviceName":"Fixture tray","platform":"windows"}"#,
        ).await;
        assert_eq!(paired.lines().next(), Some("HTTP/1.1 201 Created"));
        let summary = request(addr, "/api/auth/session", "GET", &headers, "").await;
        assert_eq!(json_reply(&summary)["pairedDevices"], 1);

        let store = home.path().join("auth/devices.json");
        std::fs::rename(&store, store.with_extension("saved")).unwrap();
        std::fs::create_dir(&store).unwrap();
        let devices = request(addr, "/api/auth/devices", "GET", &headers, "").await;
        assert_eq!(
            devices.lines().next(),
            Some("HTTP/1.1 503 Service Unavailable")
        );
        assert_eq!(json_reply(&devices)["code"], "auth_store_unavailable");

        let summary = request(addr, "/api/auth/session", "GET", &headers, "").await;
        let changed = request(
            addr,
            "/api/auth/password",
            "POST",
            &json_headers,
            r#"{"currentPassword":"fixture-password","newPassword":"fixture-new-password-71"}"#,
        )
        .await;
        assert_eq!(changed.lines().next(), Some("HTTP/1.1 200 OK"));
        let receipt = json_reply(&changed);
        assert_eq!(receipt["ok"], true);
        let new_login = request(
            addr,
            "/api/auth/login",
            "POST",
            &format!("Origin: http://{addr}\r\nContent-Type: application/json\r\n"),
            r#"{"username":"fixture-admin","password":"fixture-new-password-71"}"#,
        )
        .await;
        assert_eq!(new_login.lines().next(), Some("HTTP/1.1 200 OK"));
        assert_eq!(
            [
                json_reply(&summary).get("pairedDevices"),
                receipt.get("pairedDevices")
            ],
            [Some(&Value::Null), Some(&Value::Null)],
        );
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn reset_inventory_head_and_options_do_not_require_a_write_body() {
        let (_home, _auth, addr, handle) = server_with_analytics(true, None).await;
        let cookie = login(addr).await;
        let headers = format!("Cookie: {cookie}\r\n");
        // Unsupported synthetic account so inventory reads cannot invoke a provider.
        let inventory = "/api/accounts/offline-fixture/reset-claims";
        let get = request(addr, inventory, "GET", &headers, "").await;
        assert_eq!(get.lines().next(), Some("HTTP/1.1 200 OK"));
        let head = request(addr, inventory, "HEAD", &headers, "").await;
        assert_eq!(head.lines().next(), Some("HTTP/1.1 200 OK"));
        let (get_headers, _) = get.split_once("\r\n\r\n").unwrap();
        let (head_headers, head_body) = head.split_once("\r\n\r\n").unwrap();
        assert!(head_body.is_empty());
        for name in [
            "content-type:",
            "content-length:",
            "cache-control:",
            "etag:",
        ] {
            let get_header = get_headers.lines().find(|line| line.starts_with(name));
            assert!(get_header.is_some(), "{name}");
            assert_eq!(
                head_headers.lines().find(|line| line.starts_with(name)),
                get_header,
                "{name}"
            );
        }
        for (suffix, allow, extra, body) in [
            ("", "GET, HEAD, POST", "", ""),
            (
                "/prepare/",
                "POST",
                "Content-Type: application/json\r\n",
                "{",
            ),
            ("/check", "POST", "", ""),
        ] {
            let options = request(
                addr,
                &format!("{inventory}{suffix}"),
                "OPTIONS",
                &format!("{headers}{extra}"),
                body,
            )
            .await;
            assert_eq!(options.lines().next(), Some("HTTP/1.1 200 OK"));
            assert!(
                options.contains(&format!("\r\nallow: {allow}\r\n")),
                "{suffix}"
            );
            assert!(options.ends_with(&format!("\r\n\r\n{allow}")), "{suffix}");
        }
        let origin =
            format!("{headers}Origin: http://{addr}\r\nContent-Type: application/json\r\n");
        for (suffix, key) in [("/prepare/", "grantKey"), ("/check/", "attemptKey")] {
            let result = request(
                addr,
                &format!("{inventory}{suffix}"),
                "POST",
                &origin,
                &json!({key:"0".repeat(64)}).to_string(),
            )
            .await;
            assert_eq!(
                result.lines().next(),
                Some("HTTP/1.1 409 Conflict"),
                "{suffix}"
            );
        }
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
        let (home, addr, handle, _) = settings_server_with_limits().await;
        (home, addr, handle)
    }

    async fn settings_server_with_limits() -> (
        tempfile::TempDir,
        SocketAddr,
        tokio::task::JoinHandle<()>,
        crate::limits::Limits,
    ) {
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
        let limits = settings.read_limits();
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
        (home, addr, handle, limits)
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

    #[tokio::test]
    async fn settings_preference_write_conflict_preserves_both_clients() {
        let (home, addr, handle) = settings_server().await;
        let first = format!(
            "Origin: http://{addr}\r\nContent-Type: application/json\r\nCookie: {}\r\n",
            login(addr).await
        );
        let second = format!(
            "Origin: http://{addr}\r\nContent-Type: application/json\r\nCookie: {}\r\n",
            login(addr).await
        );
        let path = "/api/accounts/preferences";
        let zone = request(
            addr,
            path,
            "PATCH",
            &first,
            r#"{"timeZone":"Europe/Paris"}"#,
        )
        .await;
        assert!(zone.starts_with("HTTP/1.1 200"), "{zone}");

        // Keep the registry lease held while the second client attempts a save.
        let rows = aac_files::registry::rows().unwrap();
        let owners = native_settings::owners();
        let held = owners
            .claim(rows, "dashboard-preferences", home.path(), None)
            .unwrap();
        let body = r#"{"snapshotCleanup":{"auto":false}}"#;
        let refused = request(addr, path, "PATCH", &second, body).await;
        assert!(refused.starts_with("HTTP/1.1 409"), "{refused}");
        assert!(
            refused.contains(r#""code":"preferences_busy""#),
            "{refused}"
        );
        assert!(
            refused.contains("Another preferences save is in progress. Try again."),
            "{refused}"
        );
        assert!(refused.contains("cache-control: no-store"), "{refused}");
        let unchanged = request(addr, path, "GET", &first, "").await;
        assert!(
            unchanged.contains(r#""snapshotCleanup":{"auto":true}"#),
            "{unchanged}"
        );
        drop(held);

        // Contention on a later lease in the acquisition sequence must also return 409 and
        // release any earlier lease taken during the failed attempt.
        let held_txn = owners
            .claim(rows, "dashboard-preferences-transaction", home.path(), None)
            .unwrap();
        let refused_txn = request(addr, path, "PATCH", &second, body).await;
        assert!(refused_txn.starts_with("HTTP/1.1 409"), "{refused_txn}");
        assert!(
            refused_txn.contains(r#""code":"preferences_busy""#),
            "{refused_txn}"
        );
        let primary_free = owners
            .claim(rows, "dashboard-preferences", home.path(), None)
            .unwrap();
        drop(primary_free);
        drop(held_txn);

        let saved = request(addr, path, "PATCH", &second, body).await;
        assert!(saved.starts_with("HTTP/1.1 200"), "{saved}");
        let reread = request(addr, path, "GET", &first, "").await;
        assert!(
            reread.contains(r#""timeZone":"Europe/Paris""#)
                && reread.contains(r#""snapshotCleanup":{"auto":false}"#),
            "{reread}"
        );
        handle.abort();
        let _ = handle.await;
    }

    async fn settings_write_headers(addr: SocketAddr) -> String {
        format!(
            "Origin: http://{addr}\r\nContent-Type: application/json\r\nCookie: {}\r\n",
            login(addr).await
        )
    }

    fn assert_settings_busy(reply: &str, code: &str, message: &str) {
        let (headers, body) = reply.split_once("\r\n\r\n").unwrap();
        let status = headers.lines().next().unwrap();
        assert!(status.starts_with("HTTP/1.1 409"), "{status} {body}");
        assert!(
            headers.contains("cache-control: no-store"),
            "{status} {body}"
        );
        let error: Value = serde_json::from_str(body).unwrap();
        assert_eq!(error, json!({"code": code, "error": message}));
    }

    #[tokio::test]
    async fn settings_refresh_write_conflict_leaves_the_previous_save_on_disk() {
        let (home, addr, handle) = settings_server().await;
        let first = settings_write_headers(addr).await;
        let second = settings_write_headers(addr).await;
        let path = "/api/accounts/settings";
        let saved = request(
            addr,
            path,
            "PUT",
            &first,
            r#"{"refreshIntervalSeconds":120}"#,
        )
        .await;
        assert!(
            saved.starts_with("HTTP/1.1 200"),
            "{:?}",
            saved.lines().next()
        );

        let file = home.path().join("account-refresh-settings.json");
        let previous = std::fs::read(&file).unwrap();
        let held = native_settings::owners()
            .claim(
                aac_files::registry::rows().unwrap(),
                "account-refresh-settings",
                home.path(),
                None,
            )
            .unwrap();
        let body = r#"{"refreshIntervalSeconds":180}"#;
        let started = std::time::Instant::now();
        let refused = request(addr, path, "PUT", &second, body).await;
        assert_settings_busy(
            &refused,
            "settings_busy",
            "Another settings save is in progress. Try again.",
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!(std::fs::read(&file).unwrap(), previous);
        drop(held);

        // This resource contains a single value. A successful PUT replaces it.
        let retried = request(addr, path, "PUT", &second, body).await;
        assert!(
            retried.starts_with("HTTP/1.1 200"),
            "{:?}",
            retried.lines().next()
        );
        assert_eq!(
            std::fs::read(&file).unwrap(),
            format!("{body}\n").as_bytes()
        );

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn settings_visibility_write_conflict_preserves_both_clients() {
        let (home, addr, handle) = settings_server().await;
        let first = settings_write_headers(addr).await;
        let second = settings_write_headers(addr).await;
        let path = "/api/accounts/visibility";
        let saved = request(
            addr,
            path,
            "PATCH",
            &first,
            r#"{"hiddenProviders":{"hide":["claude"],"show":[]}}"#,
        )
        .await;
        assert!(
            saved.starts_with("HTTP/1.1 200"),
            "{:?}",
            saved.lines().next()
        );

        let file = home.path().join("account-visibility.json");
        let previous = std::fs::read(&file).unwrap();
        let held = native_settings::owners()
            .claim(
                aac_files::registry::rows().unwrap(),
                "account-visibility",
                home.path(),
                None,
            )
            .unwrap();
        let body = r#"{"hiddenProviders":{"hide":["codex"],"show":[]}}"#;
        let started = std::time::Instant::now();
        let refused = request(addr, path, "PATCH", &second, body).await;
        assert_settings_busy(
            &refused,
            "visibility_busy",
            "Another visibility save is in progress. Try again.",
        );
        assert!(started.elapsed() >= Duration::from_millis(200));
        assert_eq!(std::fs::read(&file).unwrap(), previous);
        drop(held);

        let retried = request(addr, path, "PATCH", &second, body).await;
        assert!(
            retried.starts_with("HTTP/1.1 200"),
            "{:?}",
            retried.lines().next()
        );
        let disk: Value = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(disk["hiddenProviders"], json!(["claude", "codex"]));

        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn settings_all_writers_conflict_when_the_read_pool_is_full() {
        use crate::{limits::READ_PERMITS, table::Pool};

        let (home, addr, handle, limits) = settings_server_with_limits().await;
        let headers = settings_write_headers(addr).await;
        let (entered, mut starts) = tokio::sync::mpsc::unbounded_channel();
        let mut release = Vec::new();
        let mut holders = Vec::new();
        for _ in 0..READ_PERMITS {
            let (done, held) = tokio::sync::oneshot::channel::<()>();
            let entered = entered.clone();
            let limits = limits.clone();
            release.push(done);
            holders.push(tokio::spawn(async move {
                limits
                    .blocking(Pool::Reads, move || {
                        entered.send(()).unwrap();
                        let _ = held.blocking_recv();
                        Ok(())
                    })
                    .await
            }));
        }
        drop(entered);
        for _ in 0..READ_PERMITS {
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(2), starts.recv())
                    .await
                    .unwrap(),
                Some(())
            );
        }

        let (settings, preferences, visibility) = tokio::join!(
            request(
                addr,
                "/api/accounts/settings",
                "PUT",
                &headers,
                r#"{"refreshIntervalSeconds":120}"#
            ),
            request(
                addr,
                "/api/accounts/preferences",
                "PATCH",
                &headers,
                r#"{"timeZone":"Europe/Paris"}"#
            ),
            request(
                addr,
                "/api/accounts/visibility",
                "PATCH",
                &headers,
                r#"{"hiddenProviders":{"hide":["codex"],"show":[]}}"#
            ),
        );
        assert!(
            [&settings, &preferences, &visibility]
                .iter()
                .all(|reply| reply.starts_with("HTTP/1.1 409")),
            "settings: {:?}, preferences: {:?}, visibility: {:?}",
            settings.lines().next(),
            preferences.lines().next(),
            visibility.lines().next()
        );
        for (reply, code) in [
            (&settings, "settings_busy"),
            (&preferences, "preferences_busy"),
            (&visibility, "visibility_busy"),
        ] {
            assert_settings_busy(reply, code, "The dashboard is busy. Try again.");
        }
        for file in [
            "account-refresh-settings.json",
            "dashboard-preferences.json",
            "account-visibility.json",
        ] {
            assert!(!home.path().join(file).exists(), "{file}");
        }

        drop(release);
        for holder in holders {
            holder.await.unwrap().unwrap();
        }
        handle.abort();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn settings_busy_saves_leave_room_for_reads() {
        let mut failures = Vec::new();
        for (owner, path, method, body, code, message) in [
            (
                "account-refresh-settings",
                "/api/accounts/settings",
                "PUT",
                r#"{"refreshIntervalSeconds":120}"#,
                "settings_busy",
                "Another settings save is in progress. Try again.",
            ),
            (
                "account-visibility",
                "/api/accounts/visibility",
                "PATCH",
                r#"{"hiddenProviders":{"hide":["codex"],"show":[]}}"#,
                "visibility_busy",
                "Another visibility save is in progress. Try again.",
            ),
        ] {
            let (home, addr, handle) = settings_server().await;
            let headers = settings_write_headers(addr).await;
            // Same-run control: one read with no saves in flight. An oversubscribed machine
            // slows every request; the probe count below is budgeted from this instead of a
            // fixed 100 ms schedule that let the saves finish first.
            let control_began = std::time::Instant::now();
            let warm = request(addr, "/api/accounts/preferences", "GET", &headers, "").await;
            assert!(warm.starts_with("HTTP/1.1 200"), "{warm}");
            let control_read = control_began.elapsed();
            let held = native_settings::owners()
                .claim(
                    aac_files::registry::rows().unwrap(),
                    owner,
                    home.path(),
                    None,
                )
                .unwrap();
            let mut saves = tokio::task::JoinSet::new();
            for _ in 0..crate::limits::READ_PERMITS {
                let headers = headers.clone();
                saves.spawn(async move { request(addr, path, method, &headers, body).await });
            }
            // The saves live about LEASE_WAIT (250 ms) from their write turn. Probe only
            // while they are still pending, and require at least what the control read
            // fits in half that window, so reads provably get through under load too.
            // The budget tracks the slowest probe seen, so a load spike mid-phase still
            // leaves a meaningful count instead of a stale fast control.
            let mut per_probe = control_read + Duration::from_millis(10);
            let mut probes = 0;
            let mut finished = None;
            while probes < 10 {
                if let Some(reply) = saves.try_join_next() {
                    finished = Some(reply.unwrap());
                    break;
                }
                let probe_began = std::time::Instant::now();
                let read = request(addr, "/api/accounts/preferences", "GET", &headers, "").await;
                per_probe = per_probe.max(probe_began.elapsed() + Duration::from_millis(10));
                if !read.starts_with("HTTP/1.1 200") {
                    failures.push(format!("{method} {path}: {}", read.lines().next().unwrap()));
                }
                probes += 1;
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            let required = (125 / per_probe.as_millis().max(1)).clamp(1, 10) as usize;
            assert!(
                probes >= required,
                "{path}: only {probes} of {required} budgeted reads ran while saves were pending"
            );
            if let Some(reply) = finished {
                assert_settings_busy(&reply, code, message);
            }
            while let Some(reply) = saves.join_next().await {
                assert_settings_busy(&reply.unwrap(), code, message);
            }
            drop(held);
            handle.abort();
            let _ = handle.await;
        }
        assert!(
            failures.is_empty(),
            "dashboard reads failed during busy saves: {failures:?}"
        );
    }

    #[tokio::test]
    async fn settings_queued_preference_saves_have_a_bounded_wait() {
        let (home, addr, handle) = settings_server().await;
        let headers = settings_write_headers(addr).await;
        let held = native_settings::owners()
            .claim(
                aac_files::registry::rows().unwrap(),
                "dashboard-preferences-transaction",
                home.path(),
                None,
            )
            .unwrap();
        let mut saves = tokio::task::JoinSet::new();
        for _ in 0..12 {
            let headers = headers.clone();
            saves.spawn(async move {
                request(
                    addr,
                    "/api/accounts/preferences",
                    "PATCH",
                    &headers,
                    r#"{"timeZone":"Europe/Paris"}"#,
                )
                .await
            });
        }

        tokio::time::timeout(Duration::from_millis(1250), async {
            while let Some(reply) = saves.join_next().await {
                assert_settings_busy(
                    &reply.unwrap(),
                    "preferences_busy",
                    "Another preferences save is in progress. Try again.",
                );
            }
        })
        .await
        .expect("queued preferences saves exceeded the 1250 ms test bound");
        drop(held);

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
