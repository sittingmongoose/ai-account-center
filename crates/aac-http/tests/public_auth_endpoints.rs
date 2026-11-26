//! Native Rust HTTP tests for public authentication, sessions, origin checks,
//! paired devices, password changes, session lifetime, rate limits, trusted networks,
//! LAN HTTPS proxies, open-mode guards, native TLS, and audit-log redaction.
//!
//! Ported from the retired TypeScript auth/session/origin/network/proxy suites so
//! every invariant stays verified directly against `aac_http::public_auth` and the
//! shipped `aac-http --public` listener after `src/` is deleted.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use aac_http::public_auth::{DashboardAuth, HubCache, PublicPeer};
use axum::{
    Router,
    extract::Request,
    http::HeaderName,
    middleware::{self, Next},
    response::Response,
    routing::{any, get},
};
use hmac::{Hmac, KeyInit, Mac};
use reqwest::{Client, Method, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener as StdTcpListener},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};

const PASSWORD: &str = "fixture-dashboard-password-1";
const USERNAME: &str = "fixture-admin";
const PEER_HEADER: &str = "x-aac-test-peer";
const TLS_HEADER: &str = "x-aac-test-tls";

trait JsonBody {
    fn json(self, value: &Value) -> Self;
}

impl JsonBody for reqwest::RequestBuilder {
    fn json(self, value: &Value) -> Self {
        self.header(header::CONTENT_TYPE, "application/json")
            .body(value.to_string())
    }
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn sha256_hex(input: &str) -> String {
    hex_lower(&Sha256::digest(input.as_bytes()))
}

fn hmac_sha256_hex(secret: &str, nonce: &str) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(nonce.as_bytes());
    hex_lower(&mac.finalize().into_bytes())
}

fn write_private(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn write_config(root: &Path, doc: &Value) {
    let yaml = serde_yaml::to_string(doc).unwrap();
    write_private(&root.join("config.yaml"), &yaml);
}

fn configured_auth_doc(extra: Option<Value>) -> Value {
    let hash = bcrypt::hash(PASSWORD, 4).unwrap();
    let mut doc = json!({
        "version": 2,
        "dashboard_auth": {
            "enabled": true,
            "username": USERNAME,
            "password_hash": hash,
        }
    });
    if let Some(Value::Object(map)) = extra {
        for (k, v) in map {
            doc[k] = v;
        }
    }
    doc
}

/// Test middleware mounted OUTSIDE `auth.protect` so tests can exercise arbitrary
/// remote LAN, public, IPv6-mapped, proxy, and TLS peer metadata against the real
/// Axum/TCP public front door.
async fn inject_test_peer(mut req: Request, next: Next) -> Response {
    let peer_hdr = req
        .headers_mut()
        .remove(HeaderName::from_static(PEER_HEADER))
        .and_then(|v| v.to_str().ok().map(str::to_owned));
    let tls_hdr = req
        .headers_mut()
        .remove(HeaderName::from_static(TLS_HEADER))
        .and_then(|v| v.to_str().ok().map(str::to_owned));
    if let Some(raw_ip) = peer_hdr {
        let ip: IpAddr = raw_ip.parse().expect("valid test peer ip");
        let tls = tls_hdr.as_deref() == Some("1");
        req.extensions_mut().insert(PublicPeer {
            address: SocketAddr::new(ip, 54321),
            tls,
        });
    }
    next.run(req).await
}

struct InProcessServer {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
    auth: DashboardAuth,
    addr: SocketAddr,
    handle: JoinHandle<()>,
    downstream_hits: Arc<AtomicUsize>,
    client: Client,
}

impl Drop for InProcessServer {
    fn drop(&mut self) {
        self.handle.abort();
        self.auth.flush();
    }
}

impl InProcessServer {
    async fn start_with_doc(doc: Value) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        write_config(&root, &doc);
        Self::open_at(Some(dir), root).await
    }

    async fn reopen(root: PathBuf) -> Self {
        Self::open_at(None, root).await
    }

    async fn open_at(dir: Option<tempfile::TempDir>, root: PathBuf) -> Self {
        let auth = DashboardAuth::open(root.clone()).expect("open DashboardAuth");
        let hits = Arc::new(AtomicUsize::new(0));
        let hits_for_route = Arc::clone(&hits);
        let downstream = Router::new()
            .route(
                "/api/health",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        r#"{"status":"ok"}"#,
                    )
                }),
            )
            .route(
                "/api/accounts/dashboard",
                any({
                    let h = Arc::clone(&hits_for_route);
                    move || {
                        let h = Arc::clone(&h);
                        async move {
                            h.fetch_add(1, Ordering::SeqCst);
                            (
                                axum::http::StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                                r#"{"accounts":[]}"#,
                            )
                        }
                    }
                }),
            )
            .route(
                "/api/accounts/settings",
                any({
                    let h = Arc::clone(&hits_for_route);
                    move || {
                        let h = Arc::clone(&h);
                        async move {
                            h.fetch_add(1, Ordering::SeqCst);
                            (
                                axum::http::StatusCode::OK,
                                [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                                r#"{"refreshIntervalSeconds":60}"#,
                            )
                        }
                    }
                }),
            )
            .route(
                "/api/claude/desktop-profiles",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        r#"{"profiles":[]}"#,
                    )
                }),
            )
            .route(
                "/api/antigravity/profiles",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        r#"{"profiles":[]}"#,
                    )
                }),
            )
            .route(
                "/api/store/status",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        r#"{"enabled":true}"#,
                    )
                }),
            )
            .route(
                "/api/usage/history",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        r#"{"points":[]}"#,
                    )
                }),
            )
            .route(
                "/api/accounts/codex:fixture/reset-claims",
                any(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        r#"{"status":"ok"}"#,
                    )
                }),
            );
        let router = auth
            .protect(auth.mount_edges(downstream, HubCache::default(), None))
            .layer(middleware::from_fn(inject_test_peer));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = tokio::spawn(async move {
            axum::serve(
                listener,
                router.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            .unwrap();
        });
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(10))
            .build()
            .unwrap();
        Self {
            _dir: dir,
            root,
            auth,
            addr,
            handle,
            downstream_hits: hits,
            client,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn origin(&self) -> String {
        format!("http://{}", self.addr)
    }

    fn alias_origin(&self) -> String {
        format!("http://localhost:{}", self.addr.port())
    }

    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client.request(method, self.url(path))
    }

    async fn login(&self, username: &str, password: &str, remember_me: Option<bool>) -> String {
        let mut body = json!({ "username": username, "password": password });
        if let Some(rem) = remember_me {
            body["rememberMe"] = json!(rem);
        }
        let resp = self
            .req(Method::POST, "/api/auth/login")
            .header(header::ORIGIN, self.origin())
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "login must succeed");
        extract_cookie(&resp).expect("login sets connect.sid")
    }

    async fn stop_and_keep_root(mut self) -> (Option<tempfile::TempDir>, PathBuf) {
        self.handle.abort();
        let _ = (&mut self.handle).await;
        self.auth.flush();
        let dir = self._dir.take();
        let root = self.root.clone();
        (dir, root)
    }
}

fn extract_cookie(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap_or(s).to_owned())
}

fn full_set_cookie(resp: &reqwest::Response) -> Option<String> {
    resp.headers()
        .get(header::SET_COOKIE)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

async fn read_json(resp: reqwest::Response) -> Value {
    let bytes = resp.bytes().await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("invalid json ({e}): {}", String::from_utf8_lossy(&bytes)))
}

struct SpawnedPublic {
    _dir: tempfile::TempDir,
    root: PathBuf,
    child: Child,
    base: String,
    tls_addr: Option<String>,
    stderr: Arc<std::sync::Mutex<String>>,
}

impl Drop for SpawnedPublic {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl SpawnedPublic {
    async fn start(doc: Option<Value>, extra_env: &[(&str, &str)]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        if let Some(doc) = doc {
            write_config(&root, &doc);
        }
        Self::start_in(dir, root, extra_env).await
    }

    async fn start_in(dir: tempfile::TempDir, root: PathBuf, extra_env: &[(&str, &str)]) -> Self {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_aac-http"));
        cmd.arg("--public")
            .env_clear()
            .env("HOME", &root)
            .env("CCS_HOME", &root)
            .env("CCS_DIR", &root)
            .env("AAC_RUST_PUBLIC_LISTEN", "127.0.0.1:0")
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (k, v) in extra_env {
            cmd.env(k, v);
        }
        let mut child = cmd.spawn().expect("spawn aac-http --public");
        let stdout = child.stdout.take().unwrap();
        let stderr_pipe = child.stderr.take().unwrap();
        let stderr = Arc::new(std::sync::Mutex::new(String::new()));
        let stderr_sink = Arc::clone(&stderr);
        tokio::spawn(async move {
            let mut reader = BufReader::new(stderr_pipe).lines();
            while let Ok(Some(line)) = reader.next_line().await {
                if let Ok(mut buf) = stderr_sink.lock() {
                    buf.push_str(&line);
                    buf.push('\n');
                }
            }
        });
        let mut lines = BufReader::new(stdout).lines();
        let mut tls_addr = None;
        let base = tokio::time::timeout(Duration::from_secs(12), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if let Some(addr) = line.strip_prefix("aac-http public TLS listening on ") {
                    tls_addr = Some(addr.trim().to_owned());
                } else if let Some(addr) = line.strip_prefix("aac-http public listening on ") {
                    return format!("http://{}", addr.trim());
                }
            }
            panic!("aac-http exited before printing listen address");
        })
        .await
        .expect("aac-http listen timeout");
        Self {
            _dir: dir,
            root,
            child,
            base,
            tls_addr,
            stderr,
        }
    }
}

#[tokio::test]
async fn auth_open_mode_localhost_remote_write_and_bar_guards() {
    let srv = InProcessServer::start_with_doc(json!({
        "version": 2,
        "dashboard_auth": { "enabled": false }
    }))
    .await;

    // Loopback open-mode check reports open + local + wsAllowed.
    let check_local = read_json(
        srv.req(Method::GET, "/api/auth/check")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(check_local["authRequired"], false);
    assert_eq!(check_local["authEnabled"], false);
    assert_eq!(check_local["isLocalAccess"], true);
    assert_eq!(check_local["accessMode"], "open");
    assert_eq!(check_local["wsAllowed"], true);

    // Remote LAN check reports open, non-local, and wsAllowed=false.
    let check_lan = read_json(
        srv.req(Method::GET, "/api/auth/check")
            .header(PEER_HEADER, "192.168.1.50")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(check_lan["authRequired"], false);
    assert_eq!(check_lan["isLocalAccess"], false);
    assert_eq!(check_lan["wsAllowed"], false);

    // Auth-only endpoints report 409 auth_not_configured while auth is off.
    for path in ["/api/auth/session", "/api/auth/network"] {
        let resp = srv.req(Method::GET, path).send().await.unwrap();
        assert_eq!(resp.status(), StatusCode::CONFLICT, "{path}");
        assert_eq!(read_json(resp).await["code"], "auth_not_configured");
    }

    // Remote read-only GET is allowed when auth is disabled.
    let lan_get = srv
        .req(Method::GET, "/api/accounts/dashboard")
        .header(PEER_HEADER, "192.168.1.50")
        .send()
        .await
        .unwrap();
    assert_eq!(lan_get.status(), StatusCode::OK);

    // Remote mutating methods are blocked with 403 BEFORE route dispatch.
    let hits_before = srv.downstream_hits.load(Ordering::SeqCst);
    for method in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
        let resp = srv
            .req(method.clone(), "/api/accounts/settings")
            .header(PEER_HEADER, "192.168.1.50")
            .header(header::ORIGIN, srv.origin())
            .json(&json!({"refreshIntervalSeconds": 120}))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::FORBIDDEN, "remote {method}");
    }
    // Loopback with foreign Host or cross-origin is also blocked on writes.
    let foreign_host = srv
        .req(Method::PUT, "/api/accounts/settings")
        .header(header::HOST, "192.168.1.50:3000")
        .header(header::ORIGIN, "http://192.168.1.50:3000")
        .json(&json!({"refreshIntervalSeconds": 120}))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign_host.status(), StatusCode::FORBIDDEN);
    let cross_origin = srv
        .req(Method::PUT, "/api/accounts/settings")
        .header(header::ORIGIN, "http://evil.example")
        .json(&json!({"refreshIntervalSeconds": 120}))
        .send()
        .await
        .unwrap();
    assert_eq!(cross_origin.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        srv.downstream_hits.load(Ordering::SeqCst),
        hits_before,
        "blocked writes must not reach downstream handlers"
    );

    // Bar local-access guard in open mode:
    // 1. Remote LAN GET /api/bar/auth is refused with 403 and mints no token file.
    let bar_remote = srv
        .req(Method::GET, "/api/bar/auth")
        .header(PEER_HEADER, "192.168.1.50")
        .header("x-ccs-bar-nonce", "abcdef0123456789")
        .send()
        .await
        .unwrap();
    assert_eq!(bar_remote.status(), StatusCode::FORBIDDEN);
    assert!(bar_remote.headers().get("x-ccs-bar-token").is_none());
    assert!(
        !srv.root.join("bar/.auth-token").exists(),
        "refused remote bar probe must not mint proof file"
    );

    // 2. Cross-origin local probe is refused before minting.
    let bar_bad_origin = srv
        .req(Method::GET, "/api/bar/auth")
        .header(header::ORIGIN, "http://evil.example")
        .header("x-ccs-bar-nonce", "abcdef0123456789")
        .send()
        .await
        .unwrap();
    assert_eq!(bar_bad_origin.status(), StatusCode::FORBIDDEN);
    assert!(!srv.root.join("bar/.auth-token").exists());

    // 3. Local probe without nonce returns liveness only and no proof header.
    let bar_plain = srv.req(Method::GET, "/api/bar/auth").send().await.unwrap();
    assert_eq!(bar_plain.status(), StatusCode::OK);
    assert!(bar_plain.headers().get("x-ccs-bar-token").is_none());
    assert_eq!(read_json(bar_plain).await, json!({"status": "ok"}));

    // 4. Local probe with short nonce (<16 hex chars) returns no proof.
    let bar_short = srv
        .req(Method::GET, "/api/bar/auth")
        .header("x-ccs-bar-nonce", "abcd")
        .send()
        .await
        .unwrap();
    assert_eq!(bar_short.status(), StatusCode::OK);
    assert!(bar_short.headers().get("x-ccs-bar-token").is_none());

    // 5. Local probe with valid hex nonce returns HMAC-SHA256 proof matching bar/.auth-token.
    let nonce = "abcdef0123456789deadbeef";
    let bar_proof = srv
        .req(Method::GET, "/api/bar/auth")
        .header("x-ccs-bar-nonce", nonce)
        .send()
        .await
        .unwrap();
    assert_eq!(bar_proof.status(), StatusCode::OK);
    let proof = bar_proof
        .headers()
        .get("x-ccs-bar-token")
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let file_token = fs::read_to_string(srv.root.join("bar/.auth-token"))
        .unwrap()
        .trim()
        .to_owned();
    assert_ne!(proof, file_token, "proof must not expose raw file token");
    assert_eq!(proof, hmac_sha256_hex(&file_token, nonce));
}

#[tokio::test]
async fn auth_first_run_setup_loopback_lan_and_env_managed() {
    // Part A: LAN first-run setup requires secure transport + one-time setup code.
    let srv_lan = InProcessServer::start_with_doc(json!({
        "version": 2,
        "dashboard_auth": { "enabled": true }
    }))
    .await;
    let setup_file = srv_lan.root.join("auth/setup-code");
    let setup_code = fs::read_to_string(&setup_file).unwrap().trim().to_owned();
    assert_eq!(
        fs::metadata(&setup_file).unwrap().permissions().mode() & 0o777,
        0o600
    );

    let check = read_json(
        srv_lan
            .req(Method::GET, "/api/auth/check")
            .header(PEER_HEADER, "192.168.1.50")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(check["accessMode"], "setup");
    assert_eq!(check["authConfigured"], false);
    assert_eq!(check["setupCodeRequired"], true);
    assert_eq!(check["secureTransport"], false);

    let setup_get = read_json(
        srv_lan
            .req(Method::GET, "/api/auth/setup")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(setup_get["configured"], false);
    assert_eq!(setup_get["sessionLifetimeDays"], 7);
    assert_eq!(setup_get["sessionTimeoutHours"], 168);
    assert_eq!(setup_get["setupCodeRequired"], false);

    // Invalid username / weak password refused with explicit codes.
    let bad_user = srv_lan
        .req(Method::POST, "/api/auth/setup")
        .header(header::ORIGIN, srv_lan.origin())
        .json(&json!({"username": "!!", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_user.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(bad_user).await["code"], "invalid_username");

    let weak_pw = srv_lan
        .req(Method::POST, "/api/auth/setup")
        .header(header::ORIGIN, srv_lan.origin())
        .json(&json!({"username": USERNAME, "password": "short"}))
        .send()
        .await
        .unwrap();
    assert_eq!(weak_pw.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(weak_pw).await["code"], "weak_password");

    // Plain HTTP from LAN refused BEFORE checking setup code.
    let lan_plain = srv_lan
        .req(Method::POST, "/api/auth/setup")
        .header(PEER_HEADER, "192.168.1.50")
        .header(header::ORIGIN, srv_lan.origin())
        .json(&json!({"username": USERNAME, "password": PASSWORD, "setupCode": "WRONG-CODE"}))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_plain.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        read_json(lan_plain).await["code"],
        "secure_transport_required"
    );

    // TLS from LAN without setup code -> setup_code_required.
    let lan_no_code = srv_lan
        .req(Method::POST, "/api/auth/setup")
        .header(PEER_HEADER, "192.168.1.50")
        .header(TLS_HEADER, "1")
        .header(header::ORIGIN, srv_lan.origin())
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_no_code.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(lan_no_code).await["code"], "setup_code_required");

    // Wrong LAN setup code counts against the login rate limiter.
    let lan_wrong_code = srv_lan
        .req(Method::POST, "/api/auth/setup")
        .header(PEER_HEADER, "192.168.1.50")
        .header(TLS_HEADER, "1")
        .header(header::ORIGIN, srv_lan.origin())
        .json(&json!({"username": USERNAME, "password": PASSWORD, "setupCode": "2222-2222"}))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_wrong_code.status(), StatusCode::FORBIDDEN);
    let wrong_body = read_json(lan_wrong_code).await;
    assert_eq!(wrong_body["code"], "setup_code_invalid");
    assert_eq!(wrong_body["triesLeft"], 4);

    // Valid LAN setup code over TLS completes setup, sets Secure cookie, consumes file.
    let lan_ok = srv_lan
        .req(Method::POST, "/api/auth/setup")
        .header(PEER_HEADER, "192.168.1.50")
        .header(TLS_HEADER, "1")
        .header(header::ORIGIN, srv_lan.origin())
        .json(&json!({"username": USERNAME, "password": PASSWORD, "setupCode": setup_code}))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_ok.status(), StatusCode::CREATED);
    let cookie = full_set_cookie(&lan_ok).unwrap();
    assert!(
        cookie.contains("HttpOnly")
            && cookie.contains("Secure")
            && cookie.contains("SameSite=Strict")
    );
    assert!(!setup_file.exists(), "setup-code file must be consumed");

    // Second setup attempt is refused as already_configured.
    let second = srv_lan
        .req(Method::POST, "/api/auth/setup")
        .header(header::ORIGIN, srv_lan.origin())
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::CONFLICT);
    assert_eq!(read_json(second).await["code"], "already_configured");

    // /api/auth/setupx is private (401 without session), not confused with /api/auth/setup.
    let setupx = srv_lan
        .req(Method::GET, "/api/auth/setupx")
        .send()
        .await
        .unwrap();
    assert_eq!(setupx.status(), StatusCode::UNAUTHORIZED);

    // Part B: Loopback setup succeeds WITHOUT a setup code and auto-signs in.
    let srv_local = InProcessServer::start_with_doc(json!({
        "version": 2,
        "dashboard_auth": { "enabled": true }
    }))
    .await;
    let local_ok = srv_local
        .req(Method::POST, "/api/auth/setup")
        .header(header::ORIGIN, srv_local.origin())
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(local_ok.status(), StatusCode::CREATED);
    let local_cookie = extract_cookie(&local_ok).unwrap();
    let sess = srv_local
        .req(Method::GET, "/api/auth/session")
        .header(header::COOKIE, &local_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(sess.status(), StatusCode::OK);
    assert_eq!(read_json(sess).await["username"], USERNAME);

    // Part C: Environment-managed credentials refuse setup & password change cleanly.
    let hash = bcrypt::hash(PASSWORD, 4).unwrap();
    let env_srv = SpawnedPublic::start(
        Some(json!({"version": 2})),
        &[
            ("CCS_DASHBOARD_AUTH_ENABLED", "true"),
            ("CCS_DASHBOARD_USERNAME", USERNAME),
            ("CCS_DASHBOARD_PASSWORD_HASH", &hash),
        ],
    )
    .await;
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    let env_setup = client
        .post(format!("{}/api/auth/setup", env_srv.base))
        .header(header::ORIGIN, &env_srv.base)
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(env_setup.status(), StatusCode::CONFLICT);
    assert_eq!(read_json(env_setup).await["code"], "managed_by_env");

    let env_login = client
        .post(format!("{}/api/auth/login", env_srv.base))
        .header(header::ORIGIN, &env_srv.base)
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(env_login.status(), StatusCode::OK);
    let env_cookie = extract_cookie(&env_login).unwrap();

    let env_sess = read_json(
        client
            .get(format!("{}/api/auth/session", env_srv.base))
            .header(header::COOKIE, &env_cookie)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(env_sess["managedBy"], "env");

    let env_pw = client
        .post(format!("{}/api/auth/password", env_srv.base))
        .header(header::COOKIE, &env_cookie)
        .header(header::ORIGIN, &env_srv.base)
        .json(&json!({"currentPassword": PASSWORD, "newPassword": "another-valid-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(env_pw.status(), StatusCode::CONFLICT);
    assert_eq!(read_json(env_pw).await["code"], "managed_by_env");
    // Must not generate a setup code for a configured server.
    assert!(!env_srv.root.join("auth/setup-code").exists());
}

#[tokio::test]
async fn auth_login_logout_sessions_lifetime_and_restart_persistence() {
    let srv = InProcessServer::start_with_doc(configured_auth_doc(None)).await;

    // Missing/bad bodies & UTF-8 length mismatch username fail cleanly.
    let empty_login = srv
        .req(Method::POST, "/api/auth/login")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(empty_login.status(), StatusCode::BAD_REQUEST);
    assert_eq!(
        empty_login.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    let bad_rem = srv
        .req(Method::POST, "/api/auth/login")
        .json(&json!({"username": USERNAME, "password": PASSWORD, "rememberMe": "yes"}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_rem.status(), StatusCode::BAD_REQUEST);

    let utf8_user = srv
        .req(Method::POST, "/api/auth/login")
        .json(&json!({"username": "fixture-adm\u{00e9}n", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(utf8_user.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(utf8_user).await["code"], "invalid_credentials");

    // Remember-me ON sets persistent Expires=; Remember-me OFF sets browser-session cookie.
    let rem_on_resp = srv
        .req(Method::POST, "/api/auth/login")
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(rem_on_resp.status(), StatusCode::OK);
    let on_hdr = full_set_cookie(&rem_on_resp).unwrap();
    assert!(on_hdr.contains("HttpOnly") && on_hdr.contains("SameSite=Strict"));
    assert!(
        on_hdr.contains("Expires="),
        "rememberMe=true must set Expires"
    );
    let cookie_a = extract_cookie(&rem_on_resp).unwrap();

    let rem_off_resp = srv
        .req(Method::POST, "/api/auth/login")
        .json(&json!({"username": USERNAME, "password": PASSWORD, "rememberMe": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(rem_off_resp.status(), StatusCode::OK);
    let off_hdr = full_set_cookie(&rem_off_resp).unwrap();
    assert!(off_hdr.contains("HttpOnly") && off_hdr.contains("SameSite=Strict"));
    assert!(
        !off_hdr.contains("Expires="),
        "rememberMe=false must issue a session cookie without Expires"
    );
    let cookie_b = extract_cookie(&rem_off_resp).unwrap();

    // GET /api/auth/session reports default 7-day lifetime and otherBrowsers=1.
    let sess_a = read_json(
        srv.req(Method::GET, "/api/auth/session")
            .header(header::COOKIE, &cookie_a)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(sess_a["username"], USERNAME);
    assert_eq!(sess_a["sessionLifetimeDays"], 7);
    assert_eq!(sess_a["sessionTimeoutHours"], 168);
    assert_eq!(sess_a["otherBrowsers"], 1);
    assert_eq!(sess_a["managedBy"], "config");

    // Query string on /api/auth/session refused.
    let sess_q = srv
        .req(Method::GET, "/api/auth/session?x=1")
        .header(header::COOKIE, &cookie_a)
        .send()
        .await
        .unwrap();
    assert_eq!(sess_q.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(sess_q).await["code"], "unexpected_query");

    // Session lifetime validation: only [1, 7, 30, 90, 365] accepted; Origin required; loopback alias allowed.
    let bad_days = srv
        .req(Method::PUT, "/api/auth/session-lifetime")
        .header(header::COOKIE, &cookie_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"days": 14}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_days.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(bad_days).await["code"], "invalid_body");

    let no_origin = srv
        .req(Method::PUT, "/api/auth/session-lifetime")
        .header(header::COOKIE, &cookie_a)
        .json(&json!({"days": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(no_origin.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(no_origin).await["code"], "origin_required");

    let alias_ok = srv
        .req(Method::PUT, "/api/auth/session-lifetime")
        .header(header::COOKIE, &cookie_a)
        .header(header::ORIGIN, srv.alias_origin())
        .json(&json!({"days": 30}))
        .send()
        .await
        .unwrap();
    assert_eq!(alias_ok.status(), StatusCode::OK);
    let lt = read_json(alias_ok).await;
    assert_eq!(lt["days"], 30);
    assert_eq!(lt["hours"], 720);
    assert_eq!(lt["options"], json!([1, 7, 30, 90, 365]));

    // Pair a device so we verify it survives restart + external credential change.
    let pair = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "Desk",
            "platform": "windows"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pair.status(), StatusCode::CREATED);
    let device_token = read_json(pair).await["token"].as_str().unwrap().to_owned();

    // Logout browser A clears cookie and invalidates session A immediately.
    let logout_a = srv
        .req(Method::POST, "/api/auth/logout")
        .header(header::COOKIE, &cookie_a)
        .send()
        .await
        .unwrap();
    assert_eq!(logout_a.status(), StatusCode::OK);
    assert_eq!(
        full_set_cookie(&logout_a).as_deref(),
        Some("connect.sid=; Path=/; Expires=Thu, 01 Jan 1970 00:00:00 GMT")
    );
    assert_eq!(
        srv.req(Method::GET, "/api/auth/session")
            .header(header::COOKIE, &cookie_a)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // Login browser C so after restart we have 2 live sessions (B and C) and 1 logged-out (A).
    let cookie_c = srv.login(USERNAME, PASSWORD, Some(true)).await;

    // Restart DashboardAuth on the same root -> B & C survive, A stays dead, otherBrowsers=1.
    let (_keep_dir, root) = srv.stop_and_keep_root().await;
    let srv2 = InProcessServer::reopen(root.clone()).await;
    assert_eq!(
        srv2.req(Method::GET, "/api/auth/session")
            .header(header::COOKIE, &cookie_a)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let sess_c = read_json(
        srv2.req(Method::GET, "/api/auth/session")
            .header(header::COOKIE, &cookie_c)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(sess_c["otherBrowsers"], 1);
    assert_eq!(sess_c["sessionLifetimeDays"], 30);

    // External password hash change in config.yaml triggers credential stamp revocation for browsers
    // while keeping paired tray device tokens valid.
    let mut cfg: Value =
        serde_yaml::from_str(&fs::read_to_string(root.join("config.yaml")).unwrap()).unwrap();
    cfg["dashboard_auth"]["password_hash"] =
        json!(bcrypt::hash("outside-changed-password", 4).unwrap());
    write_config(&root, &cfg);

    let revoked_b = srv2
        .req(Method::GET, "/api/auth/session")
        .header(header::COOKIE, &cookie_b)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked_b.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(revoked_b).await["code"], "session_revoked");

    let dev_still_ok = srv2
        .req(Method::GET, "/api/auth/devices/me")
        .header(header::AUTHORIZATION, format!("Bearer {device_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(dev_still_ok.status(), StatusCode::OK);
}

#[tokio::test]
async fn auth_password_change_session_revocation_and_rate_limits() {
    let srv = InProcessServer::start_with_doc(configured_auth_doc(Some(json!({
        "dashboard_network": {
            "trust_local_network": false,
            "trusted_networks": ["10.0.0.0/8"]
        },
        "cliproxy": { "legacy_key": "preserved-verbatim" }
    }))))
    .await;

    let mut browser_a = srv.login(USERNAME, PASSWORD, Some(true)).await;
    let browser_b = srv.login(USERNAME, PASSWORD, Some(true)).await;

    // Pair a tray device to verify password change keeps paired trays working.
    let pair = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "TrayMac",
            "platform": "mac"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pair.status(), StatusCode::CREATED);
    let device_token = read_json(pair).await["token"].as_str().unwrap().to_owned();

    // Guard matrix on POST /api/auth/password:
    assert_eq!(
        srv.req(Method::POST, "/api/auth/password")
            .header(header::ORIGIN, srv.origin())
            .json(&json!({"currentPassword": PASSWORD, "newPassword": "new-password-1"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let dev_pw = srv
        .req(Method::POST, "/api/auth/password")
        .header(header::AUTHORIZATION, format!("Bearer {device_token}"))
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"currentPassword": PASSWORD, "newPassword": "new-password-1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(dev_pw.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(dev_pw).await["code"], "device_scope");

    let text_pw = srv
        .req(Method::POST, "/api/auth/password")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .header(header::CONTENT_TYPE, "text/plain")
        .body("plain")
        .send()
        .await
        .unwrap();
    assert_eq!(text_pw.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(read_json(text_pw).await["code"], "json_required");

    let extra_key_pw = srv
        .req(Method::POST, "/api/auth/password")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"currentPassword": PASSWORD, "newPassword": "new-password-1", "extra": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(extra_key_pw.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(extra_key_pw).await["code"], "invalid_body");

    let same_pw = srv
        .req(Method::POST, "/api/auth/password")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"currentPassword": PASSWORD, "newPassword": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(same_pw.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(same_pw).await["code"], "same_password");

    let lan_plain_pw = srv
        .req(Method::POST, "/api/auth/password")
        .header(PEER_HEADER, "192.168.1.50")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"currentPassword": PASSWORD, "newPassword": "new-password-1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_plain_pw.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        read_json(lan_plain_pw).await["code"],
        "secure_transport_required"
    );

    // Password change with signOutOtherBrowsers=false keeps browser B alive & preserves config blocks.
    let new_pw_1 = "rotated-password-round-1";
    let change_keep = srv
        .req(Method::POST, "/api/auth/password")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({
            "currentPassword": PASSWORD,
            "newPassword": new_pw_1,
            "signOutOtherBrowsers": false
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(change_keep.status(), StatusCode::OK);
    let rotated_cookie = extract_cookie(&change_keep).unwrap();
    assert_ne!(
        rotated_cookie, browser_a,
        "password change rotates session cookie"
    );
    browser_a = rotated_cookie;
    let body_keep = read_json(change_keep).await;
    assert_eq!(body_keep["ok"], true);
    assert_eq!(body_keep["signedOutBrowsers"], 0);
    assert_eq!(body_keep["pairedDevices"], 1);
    assert_eq!(
        srv.req(Method::GET, "/api/auth/session")
            .header(header::COOKIE, &browser_b)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    // Replaced password no longer works for login or device pairing.
    assert_eq!(
        srv.req(Method::POST, "/api/auth/login")
            .json(&json!({"username": USERNAME, "password": PASSWORD}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.req(Method::POST, "/api/auth/devices/pair")
            .json(&json!({
                "username": USERNAME,
                "password": PASSWORD,
                "deviceName": "StalePw",
                "platform": "mac"
            }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    // Preserved dashboard_network and retired cliproxy section in config.yaml.
    let raw_yaml = fs::read_to_string(srv.root.join("config.yaml")).unwrap();
    assert!(raw_yaml.contains("10.0.0.0/8"));
    assert!(raw_yaml.contains("preserved-verbatim"));

    // Password change with default signOutOtherBrowsers=true signs out browser B, keeps tray.
    let new_pw_2 = "rotated-password-round-2";
    let change_revoke = srv
        .req(Method::POST, "/api/auth/password")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({
            "currentPassword": new_pw_1,
            "newPassword": new_pw_2
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(change_revoke.status(), StatusCode::OK);
    browser_a = extract_cookie(&change_revoke).unwrap();
    assert_eq!(read_json(change_revoke).await["signedOutBrowsers"], 1);
    let b_revoked = srv
        .req(Method::GET, "/api/auth/session")
        .header(header::COOKIE, &browser_b)
        .send()
        .await
        .unwrap();
    assert_eq!(b_revoked.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(b_revoked).await["code"], "session_revoked");
    assert_eq!(
        srv.req(Method::GET, "/api/auth/devices/me")
            .header(header::AUTHORIZATION, format!("Bearer {device_token}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // 5 wrong current passwords -> 401 counting triesLeft, 6th -> 429 with Retry-After.
    for expected_left in (0..=4).rev() {
        let wrong = srv
            .req(Method::POST, "/api/auth/password")
            .header(header::COOKIE, &browser_a)
            .header(header::ORIGIN, srv.origin())
            .json(&json!({"currentPassword": "wrong-now", "newPassword": "valid-next-password"}))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
        let j = read_json(wrong).await;
        assert_eq!(j["code"], "wrong_password");
        assert_eq!(j["triesLeft"], expected_left);
    }
    let limited_pw = srv
        .req(Method::POST, "/api/auth/password")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"currentPassword": new_pw_2, "newPassword": "valid-next-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(limited_pw.status(), StatusCode::TOO_MANY_REQUESTS);
    assert!(limited_pw.headers().get(header::RETRY_AFTER).is_some());

    // Revoke-others rate limit: 10 allowed per 15 min, 11th -> 429.
    for _ in 0..10 {
        let rev = srv
            .req(Method::POST, "/api/auth/sessions/revoke-others")
            .header(header::COOKIE, &browser_a)
            .header(header::ORIGIN, srv.origin())
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(rev.status(), StatusCode::OK);
        if let Some(next_cookie) = extract_cookie(&rev) {
            browser_a = next_cookie;
        }
    }
    let rev_limited = srv
        .req(Method::POST, "/api/auth/sessions/revoke-others")
        .header(header::COOKIE, &browser_a)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(rev_limited.status(), StatusCode::TOO_MANY_REQUESTS);

    // Login rate limit: 5 wrong logins from one peer -> 6th returns 429 and blocks pair too.
    let peer_ip = "192.168.1.77";
    for expected_left in (0..=4).rev() {
        let fail = srv
            .req(Method::POST, "/api/auth/login")
            .header(PEER_HEADER, peer_ip)
            .header(TLS_HEADER, "1")
            .json(&json!({"username": USERNAME, "password": "bad-password"}))
            .send()
            .await
            .unwrap();
        assert_eq!(fail.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(read_json(fail).await["triesLeft"], expected_left);
    }
    let login_429 = srv
        .req(Method::POST, "/api/auth/login")
        .header(PEER_HEADER, peer_ip)
        .header(TLS_HEADER, "1")
        .json(&json!({"username": USERNAME, "password": new_pw_2}))
        .send()
        .await
        .unwrap();
    assert_eq!(login_429.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(read_json(login_429).await["code"], "rate_limited");

    let pair_429 = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .header(PEER_HEADER, peer_ip)
        .header(TLS_HEADER, "1")
        .json(&json!({
            "username": USERNAME,
            "password": new_pw_2,
            "deviceName": "Blocked",
            "platform": "windows"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pair_429.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(read_json(pair_429).await["code"], "rate_limited");
}

#[tokio::test]
async fn auth_device_pairing_scope_rotation_revocation_and_hardening() {
    let srv = InProcessServer::start_with_doc(configured_auth_doc(None)).await;
    let browser = srv.login(USERNAME, PASSWORD, Some(true)).await;
    let other_browser = srv.login(USERNAME, PASSWORD, Some(true)).await;

    // Pair accepts native client without Origin, rejects foreign Origin and invalid platform.
    let bad_origin = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .header(header::ORIGIN, "http://foreign.example")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "Desk",
            "platform": "windows"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);

    let bad_platform = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "Desk",
            "platform": "linux"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_platform.status(), StatusCode::BAD_REQUEST);

    let install_id = "76A8CFC9-6C80-404D-BDA6-E5274C4EA0CB";
    let pair1 = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "Desk",
            "platform": "windows",
            "installId": install_id,
            "appVersion": "1.2.3"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pair1.status(), StatusCode::CREATED);
    let p1 = read_json(pair1).await;
    let token1 = p1["token"].as_str().unwrap().to_owned();
    assert!(token1.starts_with("aacd_") && token1.len() == 48);

    // devices.json is 0600 and stores only the SHA-256 hash, never the raw token.
    let devices_file = srv.root.join("auth/devices.json");
    assert_eq!(
        fs::metadata(&devices_file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let disk = fs::read_to_string(&devices_file).unwrap();
    assert!(
        !disk.contains(&token1),
        "raw device token must never be stored"
    );
    assert!(disk.contains(&sha256_hex(&token1)));

    // Re-pairing with the same installId revokes token1 immediately.
    let pair2 = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "Desk",
            "platform": "windows",
            "installId": install_id
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(pair2.status(), StatusCode::CREATED);
    let p2 = read_json(pair2).await;
    let token2 = p2["token"].as_str().unwrap().to_owned();
    let device2_id = p2["deviceId"].as_str().unwrap().to_owned();

    let old_try = srv
        .req(Method::GET, "/api/auth/devices/me")
        .header(header::AUTHORIZATION, format!("Bearer {token1}"))
        .send()
        .await
        .unwrap();
    assert_eq!(old_try.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(old_try).await["code"], "device_revoked");

    // Device token allowlist: allowed tray routes succeed, browser-only routes return 403 device_scope.
    for allowed in [
        "/api/accounts/dashboard",
        "/api/claude/desktop-profiles",
        "/api/antigravity/profiles",
        "/api/bar/auth",
        "/api/auth/devices/me",
        "/api/store/status",
        "/api/usage/history",
    ] {
        let resp = srv
            .req(Method::GET, allowed)
            .header(header::AUTHORIZATION, format!("Bearer {token2}"))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::OK, "device allowed: {allowed}");
    }
    for (method, forbidden) in [
        (Method::GET, "/api/auth/devices"),
        (Method::GET, "/api/auth/session"),
        (Method::GET, "/api/auth/network"),
        (Method::GET, "/api/accounts/settings"),
        (Method::GET, "/api/accounts/codex:fixture/reset-claims"),
        (Method::POST, "/api/accounts/dashboard"),
        (Method::GET, "/api/Auth/devices/me"),
    ] {
        let resp = srv
            .req(method.clone(), forbidden)
            .header(header::AUTHORIZATION, format!("Bearer {token2}"))
            .send()
            .await
            .unwrap();
        assert_eq!(
            resp.status(),
            StatusCode::FORBIDDEN,
            "device blocked: {method} {forbidden}"
        );
        assert_eq!(read_json(resp).await["code"], "device_scope");
    }

    // Browser cannot call GET/DELETE /api/auth/devices/me.
    let browser_me = srv
        .req(Method::GET, "/api/auth/devices/me")
        .header(header::COOKIE, &browser)
        .send()
        .await
        .unwrap();
    assert_eq!(browser_me.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(browser_me).await["code"], "device_required");

    // Token leakage guards: query string (raw or percent-encoded) and JSON body (nested/key) -> 400 token_in_query.
    for bad_query in [
        format!("/api/auth/check?credential={token2}"),
        format!("/api/auth/check?credential=%61{}", &token2[1..]),
    ] {
        let r = srv.req(Method::GET, &bad_query).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "{bad_query}");
        assert_eq!(read_json(r).await["code"], "token_in_query");
    }
    for body_with_token in [
        json!({"nested": [{"deep": token2}]}),
        json!({ token2.clone(): "as-key" }),
    ] {
        let r = srv
            .req(Method::POST, "/api/auth/devices/pair")
            .json(&body_with_token)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(read_json(r).await["code"], "token_in_query");
    }

    // Malformed Authorization header fails 401 invalid_token and NEVER falls back to valid browser cookie.
    let no_fallback = srv
        .req(Method::GET, "/api/auth/session")
        .header(header::COOKIE, &browser)
        .header(header::AUTHORIZATION, "Bearer not-a-valid-token")
        .send()
        .await
        .unwrap();
    assert_eq!(no_fallback.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(no_fallback).await["code"], "invalid_token");

    // Rotation: old token stays valid until new token is used once, then old token is rejected.
    let rot = srv
        .req(Method::POST, "/api/auth/devices/me/rotate")
        .header(header::AUTHORIZATION, format!("Bearer {token2}"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(rot.status(), StatusCode::OK);
    let token3 = read_json(rot).await["token"].as_str().unwrap().to_owned();
    assert_eq!(
        srv.req(Method::GET, "/api/auth/devices/me")
            .header(header::AUTHORIZATION, format!("Bearer {token2}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        srv.req(Method::GET, "/api/auth/devices/me")
            .header(header::AUTHORIZATION, format!("Bearer {token3}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let old_after_use = srv
        .req(Method::GET, "/api/auth/devices/me")
        .header(header::AUTHORIZATION, format!("Bearer {token2}"))
        .send()
        .await
        .unwrap();
    assert_eq!(old_after_use.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(old_after_use).await["code"], "invalid_token");

    // GET /api/auth/devices omits hashes; DELETE /api/auth/devices/:id requires Origin and revokes immediately.
    let list = read_json(
        srv.req(Method::GET, "/api/auth/devices")
            .header(header::COOKIE, &browser)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let list_text = list.to_string();
    assert!(!list_text.contains("tokenSha256"));
    assert!(list["devices"].as_array().unwrap().len() == 1);

    assert_eq!(
        srv.req(Method::DELETE, &format!("/api/auth/devices/{device2_id}"))
            .header(header::COOKIE, &browser)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        srv.req(Method::DELETE, &format!("/api/auth/devices/{device2_id}"))
            .header(header::COOKIE, &browser)
            .header(header::ORIGIN, srv.origin())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        srv.req(Method::GET, "/api/auth/devices/me")
            .header(header::AUTHORIZATION, format!("Bearer {token3}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // Cap at 20 active devices -> 21st returns 409 too_many_devices WITHOUT spending login budget.
    let mut cap_tokens = Vec::new();
    for i in 0..20 {
        let r = srv
            .req(Method::POST, "/api/auth/devices/pair")
            .json(&json!({
                "username": USERNAME,
                "password": PASSWORD,
                "deviceName": format!("Tray-{i}"),
                "platform": "mac"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::CREATED, "device #{i}");
        cap_tokens.push(read_json(r).await["token"].as_str().unwrap().to_owned());
    }
    for _ in 0..6 {
        let over = srv
            .req(Method::POST, "/api/auth/devices/pair")
            .json(&json!({
                "username": USERNAME,
                "password": PASSWORD,
                "deviceName": "Tray-21",
                "platform": "mac"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(over.status(), StatusCode::CONFLICT);
        assert_eq!(read_json(over).await["code"], "too_many_devices");
    }

    // DELETE /api/auth/devices/me disconnects one tray itself.
    assert_eq!(
        srv.req(Method::DELETE, "/api/auth/devices/me")
            .header(header::AUTHORIZATION, format!("Bearer {}", cap_tokens[0]))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        srv.req(Method::GET, "/api/auth/devices/me")
            .header(header::AUTHORIZATION, format!("Bearer {}", cap_tokens[0]))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );

    // Refuses devices.json with unsafe group/other permissions (0644 -> 503).
    fs::set_permissions(&devices_file, fs::Permissions::from_mode(0o644)).unwrap();
    let unsafe_store = srv
        .req(Method::GET, "/api/auth/devices/me")
        .header(header::AUTHORIZATION, format!("Bearer {}", cap_tokens[1]))
        .send()
        .await
        .unwrap();
    assert_eq!(unsafe_store.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        read_json(unsafe_store).await["code"],
        "auth_store_unavailable"
    );
    fs::set_permissions(&devices_file, fs::Permissions::from_mode(0o600)).unwrap();

    // revoke-all with signOutOtherBrowsers=false revokes all remaining devices and keeps other browser.
    let revoke_all = srv
        .req(Method::POST, "/api/auth/devices/revoke-all")
        .header(header::COOKIE, &browser)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"signOutOtherBrowsers": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(revoke_all.status(), StatusCode::OK);
    let rev_json = read_json(revoke_all).await;
    assert_eq!(rev_json["revokedDevices"], 19);
    assert_eq!(rev_json["signedOutBrowsers"], 0);
    assert_eq!(
        srv.req(Method::GET, "/api/auth/session")
            .header(header::COOKIE, &other_browser)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
}

#[tokio::test]
async fn auth_origin_trusted_networks_and_lan_https_proxy() {
    // Pick an RFC1918 proxy address guaranteed not to collide with this machine's local interfaces.
    let proxy_ip = "172.31.250.251";
    let srv = InProcessServer::start_with_doc(configured_auth_doc(Some(json!({
        "dashboard_network": {
            "trust_local_network": false,
            "trusted_networks": [
                "10.0.0.0/8",
                "172.16.0.0/12",
                "192.168.0.0/16",
                "100.64.0.0/10",
                "fc00::/7",
                "127.0.0.0/8",
                "1.0.0.0/7"
            ]
        },
        "dashboard_tls": {
            "public_origin": "https://aac.example.test",
            "trusted_proxy": "lan-https-proxy",
            "trusted_proxy_addresses": [proxy_ip]
        }
    }))))
    .await;

    let browser_local = srv.login(USERNAME, PASSWORD, Some(true)).await;

    // 1. Origin rules: same host+port & loopback aliases allowed; mismatched port / 127-DNS / null rejected.
    let port = srv.addr.port();
    for (origin, expected) in [
        (format!("http://127.0.0.1:{port}"), StatusCode::OK),
        (format!("http://localhost:{port}"), StatusCode::OK),
        (format!("http://[::1]:{port}"), StatusCode::OK),
        (format!("http://sub.localhost:{port}"), StatusCode::OK),
        (
            format!("http://localhost:{}", port.wrapping_add(1)),
            StatusCode::FORBIDDEN,
        ),
        (
            format!("http://127.evil.example:{port}"),
            StatusCode::FORBIDDEN,
        ),
        ("null".to_owned(), StatusCode::FORBIDDEN),
        ("http://evil.example".to_owned(), StatusCode::FORBIDDEN),
    ] {
        let r = srv
            .req(Method::PUT, "/api/auth/session-lifetime")
            .header(header::COOKIE, &browser_local)
            .header(header::ORIGIN, &origin)
            .json(&json!({"days": 7}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), expected, "origin={origin}");
    }

    // 2. Trusted network off by default: LAN peer refused BEFORE password check; turning ON from LAN refused.
    let lan_pair_off = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .header(PEER_HEADER, "192.168.1.50")
        .json(&json!({
            "username": USERNAME,
            "password": "wrong-password-not-checked",
            "deviceName": "LanTray",
            "platform": "windows"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_pair_off.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        read_json(lan_pair_off).await["code"],
        "secure_transport_required"
    );

    let lan_browser = {
        let r = srv
            .req(Method::POST, "/api/auth/login")
            .header(PEER_HEADER, "192.168.1.50")
            .json(&json!({"username": USERNAME, "password": PASSWORD}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::OK);
        extract_cookie(&r).unwrap()
    };
    let turn_on_from_lan = srv
        .req(Method::PUT, "/api/auth/network")
        .header(PEER_HEADER, "192.168.1.50")
        .header(header::COOKIE, &lan_browser)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"trustLocalNetwork": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(turn_on_from_lan.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        read_json(turn_on_from_lan).await["code"],
        "loopback_required"
    );

    // Turn local network trust ON from direct loopback. Invalid ranges (127.0.0.0/8, /7) were filtered out.
    let turn_on_local = srv
        .req(Method::PUT, "/api/auth/network")
        .header(header::COOKIE, &browser_local)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"trustLocalNetwork": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(turn_on_local.status(), StatusCode::OK);
    let net_doc = read_json(turn_on_local).await;
    assert_eq!(net_doc["trustLocalNetwork"], true);
    assert_eq!(net_doc["canTurnOn"], true);
    let ranges: Vec<&str> = net_doc["trustedNetworks"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(ranges.contains(&"100.64.0.0/10"));
    assert!(!ranges.contains(&"127.0.0.0/8"));
    assert!(!ranges.contains(&"1.0.0.0/7"));

    // With trust ON: private LAN, IPv4-mapped IPv6 LAN, and listed CGNAT are secure; public & link-local are NOT;
    // forwarded headers from a non-proxy LAN peer STRIP local-network trust.
    for (peer, expected_secure) in [
        ("192.168.1.50", true),
        ("::ffff:192.168.1.50", true),
        ("100.64.0.42", true),
        ("fc00::1234", true),
        ("203.0.113.10", false),
        ("169.254.1.10", false),
    ] {
        let chk = read_json(
            srv.req(Method::GET, "/api/auth/check")
                .header(PEER_HEADER, peer)
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(chk["secureTransport"], expected_secure, "peer={peer}");
    }
    let spoofed_lan = read_json(
        srv.req(Method::GET, "/api/auth/check")
            .header(PEER_HEADER, "192.168.1.50")
            .header("x-forwarded-for", "127.0.0.1")
            .header("x-forwarded-proto", "https")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        spoofed_lan["secureTransport"], false,
        "forwarded headers from non-proxy LAN peer must strip trust"
    );

    // Pair a tray over plain trusted LAN -> works on LAN, REFUSED through LAN HTTPS proxy (plain_http_token).
    let lan_pair = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .header(PEER_HEADER, "192.168.1.50")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "PlainLanTray",
            "platform": "windows"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_pair.status(), StatusCode::CREATED);
    let lan_tray_token = read_json(lan_pair).await["token"]
        .as_str()
        .unwrap()
        .to_owned();

    // 3. LAN HTTPS Proxy tests:
    // Anonymous /api/auth/setup through proxy hides owner settings; reports proxied + real rightmost client IP.
    let proxy_setup_anon = read_json(
        srv.req(Method::GET, "/api/auth/setup")
            .header(PEER_HEADER, proxy_ip)
            .header("x-forwarded-proto", "https")
            .header("x-forwarded-for", "10.0.0.1, 198.51.100.7")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(proxy_setup_anon["secureTransport"], true);
    assert_eq!(
        proxy_setup_anon["connection"],
        json!({"peer": "198.51.100.7", "trusted": false, "proxied": true})
    );
    assert!(proxy_setup_anon.get("managedBy").is_none());
    assert!(proxy_setup_anon.get("trustedLocalNetwork").is_none());

    // Without X-Forwarded-Proto: https or valid client IP, proxy connection is NOT secure.
    let proxy_no_proto = read_json(
        srv.req(Method::GET, "/api/auth/check")
            .header(PEER_HEADER, proxy_ip)
            .header("x-forwarded-for", "198.51.100.7")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(proxy_no_proto["secureTransport"], false);

    // Session signed in over plain LAN is refused through LAN HTTPS proxy, still works on LAN.
    let lan_sess_via_proxy = srv
        .req(Method::GET, "/api/auth/session")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.7")
        .header(header::COOKIE, &lan_browser)
        .send()
        .await
        .unwrap();
    assert_eq!(lan_sess_via_proxy.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(
        srv.req(Method::GET, "/api/auth/session")
            .header(PEER_HEADER, "192.168.1.50")
            .header(header::COOKIE, &lan_browser)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // Tray token paired over plain LAN is refused through LAN HTTPS proxy with 403 plain_http_token.
    let lan_tray_via_proxy = srv
        .req(Method::GET, "/api/auth/devices/me")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.7")
        .header(header::AUTHORIZATION, format!("Bearer {lan_tray_token}"))
        .send()
        .await
        .unwrap();
    assert_eq!(lan_tray_via_proxy.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        read_json(lan_tray_via_proxy).await["code"],
        "plain_http_token"
    );

    // Login through LAN HTTPS proxy sets Secure cookie and works through proxy (and canTurnOn=false).
    let proxy_login = srv
        .req(Method::POST, "/api/auth/login")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.7")
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_login.status(), StatusCode::OK);
    assert!(full_set_cookie(&proxy_login).unwrap().contains("; Secure;"));
    let proxy_cookie = extract_cookie(&proxy_login).unwrap();

    // Tray paired through proxy works through proxy, then downgraded over plain LAN fails through proxy!
    let proxy_pair = srv
        .req(Method::POST, "/api/auth/devices/pair")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.7")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "ProxyTray",
            "platform": "mac"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(proxy_pair.status(), StatusCode::CREATED);
    let proxy_tray = read_json(proxy_pair).await["token"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        srv.req(Method::GET, "/api/auth/devices/me")
            .header(PEER_HEADER, proxy_ip)
            .header("x-forwarded-proto", "https")
            .header("x-forwarded-for", "198.51.100.7")
            .header(header::AUTHORIZATION, format!("Bearer {proxy_tray}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    // Use once over plain trusted LAN -> taints credential transport to plain.
    assert_eq!(
        srv.req(Method::GET, "/api/auth/devices/me")
            .header(PEER_HEADER, "192.168.1.50")
            .header(header::AUTHORIZATION, format!("Bearer {proxy_tray}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let tainted = srv
        .req(Method::GET, "/api/auth/devices/me")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.7")
        .header(header::AUTHORIZATION, format!("Bearer {proxy_tray}"))
        .send()
        .await
        .unwrap();
    assert_eq!(tainted.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(tainted).await["code"], "plain_http_token");

    // Proxy rate-limit bucket isolation: locking out forwarded client 198.51.100.1 does NOT block .2 or loopback.
    for _ in 0..5 {
        let fail = srv
            .req(Method::POST, "/api/auth/login")
            .header(PEER_HEADER, proxy_ip)
            .header("x-forwarded-proto", "https")
            .header("x-forwarded-for", "198.51.100.1")
            .json(&json!({"username": USERNAME, "password": "wrong"}))
            .send()
            .await
            .unwrap();
        assert_eq!(fail.status(), StatusCode::UNAUTHORIZED);
    }
    let blocked_c1 = srv
        .req(Method::POST, "/api/auth/login")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.1")
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_c1.status(), StatusCode::TOO_MANY_REQUESTS);

    let ok_c2 = srv
        .req(Method::POST, "/api/auth/login")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.2")
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(ok_c2.status(), StatusCode::OK);

    // Turning network trust OFF works from a LAN browser session, and proxy cannot turn it ON.
    let off_from_lan = srv
        .req(Method::PUT, "/api/auth/network")
        .header(PEER_HEADER, "192.168.1.50")
        .header(header::COOKIE, &lan_browser)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"trustLocalNetwork": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(off_from_lan.status(), StatusCode::OK);
    assert_eq!(read_json(off_from_lan).await["trustLocalNetwork"], false);

    let on_from_proxy = srv
        .req(Method::PUT, "/api/auth/network")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.7")
        .header(header::COOKIE, &proxy_cookie)
        .header(header::ORIGIN, srv.origin())
        .json(&json!({"trustLocalNetwork": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(on_from_proxy.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(on_from_proxy).await["code"], "loopback_required");

    // 4. Non-canonical API casing cannot bypass auth wall and leaks no data even with a session.
    assert_eq!(
        srv.req(Method::GET, "/API/accounts/settings")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.req(Method::GET, "/API/accounts/settings")
            .header(header::COOKIE, &browser_local)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn auth_native_tls_listener_and_audit_log_redaction() {
    let openssl_ok = std::process::Command::new("openssl")
        .arg("version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success());
    if !openssl_ok {
        eprintln!("openssl not available on PATH: skipping native TLS listener check");
        return;
    }

    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();

    let cert_path = root.join("cert.pem");
    let key_path = root.join("key.pem");
    let gen_cert = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=dashboard.example.test",
            "-keyout",
            key_path.to_str().unwrap(),
            "-out",
            cert_path.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(gen_cert.success(), "openssl self-signed cert generation");

    // Part 1: 0644 private key keeps TLS listener OFF while plain HTTP still serves.
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    let free_port = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    write_config(
        &root,
        &json!({
            "version": 2,
            "dashboard_auth": { "enabled": true },
            "dashboard_tls": {
                "https_listener": {
                    "enabled": true,
                    "port": free_port,
                    "cert_path": cert_path,
                    "key_path": key_path,
                },
                "public_origin": "https://dashboard.example.test"
            }
        }),
    );
    let srv_bad_perm = SpawnedPublic::start_in(dir, root, &[]).await;
    assert!(
        srv_bad_perm.tls_addr.is_none(),
        "0644 key must keep TLS listener off"
    );
    let plain_ok = Client::new()
        .get(format!("{}/api/health", srv_bad_perm.base))
        .send()
        .await
        .unwrap();
    assert_eq!(plain_ok.status(), StatusCode::OK);
    drop(srv_bad_perm);

    // Part 2: 0600 key starts native TLS listener; HTTPS setup consumes code, sets Secure cookie,
    // and never leaks setup code, password, or session cookie to stderr audit logs.
    let dir2 = tempfile::tempdir().unwrap();
    let root2 = fs::canonicalize(dir2.path()).unwrap();
    fs::set_permissions(&root2, fs::Permissions::from_mode(0o700)).unwrap();
    let cert2 = root2.join("cert.pem");
    let key2 = root2.join("key.pem");
    let gen2 = std::process::Command::new("openssl")
        .args([
            "req",
            "-x509",
            "-newkey",
            "rsa:2048",
            "-nodes",
            "-days",
            "1",
            "-subj",
            "/CN=dashboard.example.test",
            "-keyout",
            key2.to_str().unwrap(),
            "-out",
            cert2.to_str().unwrap(),
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(gen2.success());
    fs::set_permissions(&key2, fs::Permissions::from_mode(0o600)).unwrap();

    let tls_port = StdTcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    write_config(
        &root2,
        &json!({
            "version": 2,
            "dashboard_auth": { "enabled": true },
            "dashboard_tls": {
                "https_listener": {
                    "enabled": true,
                    "port": tls_port,
                    "cert_path": cert2,
                    "key_path": key2,
                },
                "public_origin": "https://dashboard.example.test"
            }
        }),
    );
    let srv_tls = SpawnedPublic::start_in(dir2, root2.clone(), &[]).await;
    let tls_addr = srv_tls
        .tls_addr
        .clone()
        .expect("0600 key must start native TLS listener");
    let setup_file = root2.join("auth/setup-code");
    let setup_code = fs::read_to_string(&setup_file).unwrap().trim().to_owned();

    let client = Client::builder()
        .danger_accept_invalid_certs(true)
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Plain HTTP with foreign Host reports secureTransport=false.
    let plain_check = read_json(
        client
            .get(format!("{}/api/auth/check", srv_tls.base))
            .header(header::HOST, "dashboard.example.test")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(plain_check["secureTransport"], false);

    // HTTPS setup succeeds with setupCode, sets Secure cookie, deletes setup-code file.
    let https_setup = client
        .post(format!("https://{tls_addr}/api/auth/setup"))
        .header(header::HOST, "dashboard.example.test")
        .header(header::ORIGIN, "https://dashboard.example.test")
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "setupCode": setup_code,
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(https_setup.status(), StatusCode::CREATED);
    let set_cookie = full_set_cookie(&https_setup).unwrap();
    assert!(
        set_cookie.ends_with("; Secure; SameSite=Strict"),
        "unexpected TLS cookie: {set_cookie}"
    );
    let body = read_json(https_setup).await;
    assert_eq!(body["username"], USERNAME);
    assert!(
        !setup_file.exists(),
        "setup-code must be removed after setup"
    );

    // Allow stderr reader to flush and verify zero credential leakage.
    tokio::time::sleep(Duration::from_millis(50)).await;
    let logs = srv_tls.stderr.lock().unwrap().clone();
    let cookie_pair = set_cookie.split(';').next().unwrap();
    for secret in [
        setup_code.as_str(),
        &setup_code.replace('-', ""),
        PASSWORD,
        cookie_pair,
    ] {
        assert!(
            !logs.contains(secret),
            "secret leaked into server audit log"
        );
    }
}
