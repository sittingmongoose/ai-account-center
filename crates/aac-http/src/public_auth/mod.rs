//! Public M6 authentication front door. The private service still owns its Unix protocol.
// Reply errors are complete Axum responses with headers. Keep them inline like successes
// so the guard can return a response without a second allocation.
#![allow(clippy::result_large_err)]
mod devices;
mod policy;
mod routes;
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
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
    time::Duration,
};
use storage::{Config, DAY, Files, Overrides, Sessions, Setup, StoreError, now};
use tokio::sync::{Semaphore, broadcast, watch};

pub use websocket::{DashboardEvent, DashboardEvents};

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
        Self::open_with(root, Overrides::from_process(), 12)
            .map_err(|_| std::io::Error::other("dashboard auth initialisation failed"))
    }

    fn open_with(root: PathBuf, env: Overrides, hash_cost: u32) -> Result<Self, StoreError> {
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
            code.print();
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
    let listener = tokio::net::TcpListener::bind(address).await?;
    let auth = DashboardAuth::open(root)?;
    let router = auth.protect(
        Router::new()
            .route(
                "/api/health",
                axum::routing::get(|| async { response(200, json!({"status":"ok"})) }),
            )
            .fallback(decline),
    );
    let tls_server = tls::listener(&auth, address.ip()).await;
    println!("aac-http public listening on {}", listener.local_addr()?);
    let shutdown = axum_server::Handle::new();
    let signal = shutdown.clone();
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
    let result = if let Some(server) = tls_server {
        let tls_router = router.layer(middleware::from_fn(tls::mark_encrypted));
        let tls = server
            .handle(shutdown)
            .serve(tls_router.into_make_service_with_connect_info::<SocketAddr>());
        tokio::try_join!(plain, tls).map(|_| ())
    } else {
        plain.await
    };
    signal_task.abort();
    auth.flush();
    result
}

async fn shutdown_signal() {
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

async fn decline() -> Reply {
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
    let status = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let text = value.to_string();
    let length = text.len().to_string();
    let mut reply = (
        status,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        text,
    )
        .into_response();
    set_header(&mut reply, "content-length", &length);
    reply
}

fn empty(status: u16) -> Reply {
    StatusCode::from_u16(status)
        .unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
        .into_response()
}

fn plain(status: u16, message: &str) -> Reply {
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
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST),
        message,
    )
        .into_response()
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
        },
        set_cookie,
    )
}

async fn front_door(State(dashboard): State<Dashboard>, request: Request, next: Next) -> Reply {
    let (mut parts, original_body) = request.into_parts();
    let upgrade = header(&parts.headers, "upgrade")
        .is_some_and(|value| value.eq_ignore_ascii_case("websocket"));
    let lower = parts.uri.path().to_ascii_lowercase();
    if !upgrade && !api_path(&lower) {
        return next.run(Request::from_parts(parts, original_body)).await;
    }
    let Some(peer) = parts.extensions.get::<PublicPeer>().copied().or_else(|| {
        parts
            .extensions
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| PublicPeer {
                address: info.0,
                tls: false,
            })
    }) else {
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
    let json_content = header(&parts.headers, "content-type").is_some_and(|content| {
        content
            .split(';')
            .next()
            .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
    });
    let (parsed, bytes, length) = if json_content && !upgrade {
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
                    plain(413, "Request body is too large.")
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
            plain(400, "Invalid JSON in request body")
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
        let auth = DashboardAuth::open_with(root, Overrides::default(), 4).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = auth.protect(Router::new().fallback(decline));
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

    #[test]
    fn public_auth_refuses_a_second_writer_and_a_typescript_session_owner() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let first = DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4).unwrap();
        assert!(DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4).is_err());
        drop(first);
        let owner = root.join("auth/sessions.owner");
        // The file is a test stand-in for a legacy dashboard's own owner marker.
        std::fs::write(&owner, b"fixture TS process").unwrap();
        assert!(DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4).is_err());
        std::fs::remove_file(&owner).unwrap();
        assert!(DashboardAuth::open_with(root.to_path_buf(), Overrides::default(), 4).is_ok());
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
        auth.events().publish(DashboardEvent::StatusChanged);
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
}
