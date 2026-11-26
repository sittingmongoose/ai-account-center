//! Native Rust contract tests against the live `aac-http` and `ai-account-center`
//! binaries and `public_auth` front door:
//! - Usage Hub (`/v0/management/*`) full guard, projection, rate-limit, proxy, and rotation contract
//! - Static UI serving, precompressed `.br`/`.gz` negotiation, PWA, traversal, dot-root, error JSON
//! - M0 public route × role admission contract (`tests/spec/routes.json`) + product surface + store-off
//! - WebSocket `/ws` event redaction, role isolation, and logout/epoch closure
//! - CLI edge & retired compatibility binary contracts (`ai-account-center`, `ccs-droid`, `ccsx`, ...)
//!
//! Ported from `m6-edge-parity.ts`, `m6-events-parity.ts`, `m6-public-contract.ts`,
//! `usage-hub-router.test.ts`, `usage-hub-endpoint.test.ts`, `static-ui-serving.test.ts`,
//! `static-ui-pwa.test.ts`, `static-ui-dot-root.test.ts`, `api-product-surface.test.ts`,
//! `api-compression.test.ts`, `dashboard-events.test.ts`, `store-native-routes.test.ts`,
//! `status-store-off.test.ts`, and `tests/unit/commands/*.test.ts`.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use aac_http::public_auth::{DashboardAuth, HubCache, PublicPeer};
use axum::{
    Router,
    extract::Request,
    http::HeaderName,
    middleware::{self, Next},
    response::Response,
    routing::get,
};
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use futures_util::StreamExt;
use reqwest::{Client, Method, StatusCode, header};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command as StdCommand, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Error as WsError, Message, client::IntoClientRequest},
};

const USERNAME: &str = "fixture-admin";
const PASSWORD: &str = "fixture-contract-password";
const PEER_HEADER: &str = "x-aac-test-peer";
const TLS_HEADER: &str = "x-aac-test-tls";
const CODEX_USAGE_URL: &str = "https://chatgpt.com/backend-api/wham/usage";
const CLAUDE_USAGE_URL: &str = "https://api.anthropic.com/api/oauth/usage";
const CODEX_CREDITS_URL: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

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

fn sha256_hex(bytes: &[u8]) -> String {
    hex_lower(&Sha256::digest(bytes))
}

fn write_private(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn gzip_bytes(raw: &[u8]) -> Vec<u8> {
    let mut enc = GzEncoder::new(Vec::new(), Compression::default());
    enc.write_all(raw).unwrap();
    enc.finish().unwrap()
}

fn gunzip_bytes(raw: &[u8]) -> Vec<u8> {
    let mut dec = GzDecoder::new(raw);
    let mut out = Vec::new();
    dec.read_to_end(&mut out).unwrap();
    out
}

async fn read_json(resp: reqwest::Response) -> Value {
    let bytes = resp.bytes().await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("invalid json ({e}): {}", String::from_utf8_lossy(&bytes)))
}

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
        let ip: IpAddr = raw_ip.parse().expect("valid peer ip");
        let tls = tls_hdr.as_deref() == Some("1");
        req.extensions_mut().insert(PublicPeer {
            address: SocketAddr::new(ip, 54321),
            tls,
        });
    }
    next.run(req).await
}

struct EdgeServer {
    _dir: tempfile::TempDir,
    root: PathBuf,
    auth: DashboardAuth,
    addr: SocketAddr,
    handle: JoinHandle<()>,
    client: Client,
}

impl Drop for EdgeServer {
    fn drop(&mut self) {
        self.handle.abort();
        self.auth.flush();
    }
}

impl EdgeServer {
    async fn start(doc: Value, cache: HubCache, ui_root: Option<PathBuf>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
        write_private(
            &root.join("config.yaml"),
            &serde_yaml::to_string(&doc).unwrap(),
        );
        let auth = DashboardAuth::open(root.clone()).unwrap();
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
                "/api/big-json",
                get(|| async {
                    let payload = json!({
                        "items": (0..120).map(|i| json!({"id": i, "text": "compressible-dashboard-payload-row"})).collect::<Vec<_>>()
                    })
                    .to_string();
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        payload,
                    )
                }),
            )
            .route(
                "/api/small-json",
                get(|| async {
                    (
                        axum::http::StatusCode::OK,
                        [(header::CONTENT_TYPE, "application/json; charset=utf-8")],
                        r#"{"ok":true}"#,
                    )
                }),
            );
        let router = auth
            .protect(auth.mount_edges(downstream, cache, ui_root))
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
            client,
        }
    }

    fn url(&self, path: &str) -> String {
        format!("http://{}{path}", self.addr)
    }

    fn req(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.client.request(method, self.url(path))
    }
}

#[tokio::test]
async fn usage_hub_full_contract_guards_projection_proxy_and_rotation() {
    let proxy_ip = "172.31.250.252";
    let key1 = format!("aacu_{}", "f".repeat(43));
    let key2 = format!("aacu_{}", "b".repeat(43));

    // Seed HubCache directly from the checked-in M6 dashboard golden fixture.
    let m6_dashboard: Value =
        serde_json::from_str(include_str!("../../../tests/spec/m6-dashboard.json")).unwrap();
    let cache = HubCache::default();
    cache.publish_dashboard_cache(&[json!({
        "schemaVersion": 1,
        "updatedAt": "2026-10-06T12:00:00Z",
        "accounts": m6_dashboard["accounts"]
    })]);

    let srv = EdgeServer::start(
        json!({
            "version": 2,
            "dashboard_auth": {
                "enabled": true,
                "username": USERNAME,
                "password_hash": bcrypt::hash(PASSWORD, 4).unwrap()
            },
            "dashboard_network": { "trust_local_network": false },
            "dashboard_tls": {
                "trusted_proxy": "lan-https-proxy",
                "trusted_proxy_addresses": [proxy_ip]
            }
        }),
        cache,
        None,
    )
    .await;

    // 1. Off (404 usage_hub_off) until key file is configured.
    let off = srv
        .req(Method::GET, "/v0/management/auth-files")
        .header(header::AUTHORIZATION, format!("Bearer {key1}"))
        .send()
        .await
        .unwrap();
    assert_eq!(off.status(), StatusCode::NOT_FOUND);
    assert_eq!(read_json(off).await["code"], "usage_hub_off");

    // 2. Write key file with unsafe 0644 permissions -> 503 usage_hub_key_unreadable.
    let key_file = srv.root.join("auth/usage-hub-key.json");
    write_private(
        &key_file,
        &json!({
            "version": 1,
            "keySha256": sha256_hex(key1.as_bytes()),
            "createdAt": "2026-11-26T00:00:00.000Z"
        })
        .to_string(),
    );
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o644)).unwrap();
    let unsafe_key = srv
        .req(Method::GET, "/v0/management/auth-files")
        .header(header::AUTHORIZATION, format!("Bearer {key1}"))
        .send()
        .await
        .unwrap();
    assert_eq!(unsafe_key.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        read_json(unsafe_key).await["code"],
        "usage_hub_key_unreadable"
    );
    fs::set_permissions(&key_file, fs::Permissions::from_mode(0o600)).unwrap();

    // 3. LAN HTTPS proxy -> 404 BEFORE anything else.
    let via_proxy = srv
        .req(Method::GET, "/v0/management/auth-files")
        .header(PEER_HEADER, proxy_ip)
        .header("x-forwarded-proto", "https")
        .header("x-forwarded-for", "198.51.100.7")
        .header(header::AUTHORIZATION, format!("Bearer {key1}"))
        .send()
        .await
        .unwrap();
    assert_eq!(via_proxy.status(), StatusCode::NOT_FOUND);

    // 4. Plain LAN without trust (and loopback with foreign Host) -> 403 secure_transport_required BEFORE key check!
    for (peer, host) in [
        ("192.168.1.50", None),
        ("127.0.0.1", Some("evil.example:3000")),
    ] {
        let mut req = srv
            .req(Method::GET, "/v0/management/auth-files")
            .header(PEER_HEADER, peer);
        if let Some(h) = host {
            req = req.header(header::HOST, h);
        }
        let r = req.send().await.unwrap();
        assert_eq!(
            r.status(),
            StatusCode::FORBIDDEN,
            "peer={peer} host={host:?}"
        );
        assert_eq!(read_json(r).await["code"], "secure_transport_required");
    }

    // 5. Key in query string -> 400 key_in_query; missing key -> 401; wrong key -> 401.
    let in_q = srv
        .req(
            Method::GET,
            &format!("/v0/management/auth-files?key={key1}"),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(in_q.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(in_q).await["code"], "key_in_query");

    let missing = srv
        .req(Method::GET, "/v0/management/auth-files")
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(missing).await["code"], "missing_management_key");

    let wrong = srv
        .req(Method::GET, "/v0/management/auth-files")
        .header(header::AUTHORIZATION, format!("Bearer {key2}"))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(read_json(wrong).await["code"], "invalid_management_key");

    // 6. Valid key via Bearer AND via X-Management-Key returns Codex & Claude accounts in T3 shape.
    let auth_files_bearer = read_json(
        srv.req(Method::GET, "/v0/management/auth-files")
            .header(header::AUTHORIZATION, format!("Bearer {key1}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let auth_files_hdr = read_json(
        srv.req(Method::GET, "/v0/management/auth-files")
            .header("x-management-key", &key1)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(auth_files_bearer, auth_files_hdr);
    let files = auth_files_bearer["files"].as_array().unwrap();
    assert_eq!(files.len(), 3);
    assert!(!auth_files_bearer.to_string().contains(&key1));

    let find_index = |email: &str| {
        files
            .iter()
            .find(|f| f["email"].as_str() == Some(email))
            .unwrap()["auth_index"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let codex_alpha = find_index("codex:alpha@example.com");
    let claude_beta = find_index("claude:beta@example.com");
    let codex_gamma = find_index("codex:gamma@example.com");

    // 7. POST /v0/management/api-call serves Codex wham/usage, Claude oauth/usage, and 503 upstream for missing reading.
    let codex_call = read_json(
        srv.req(Method::POST, "/v0/management/api-call")
            .header(header::AUTHORIZATION, format!("Bearer {key1}"))
            .json(&json!({ "auth_index": codex_alpha, "method": "GET", "url": CODEX_USAGE_URL }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(codex_call["status_code"], 200);
    let codex_inner: Value = serde_json::from_str(codex_call["body"].as_str().unwrap()).unwrap();
    assert_eq!(codex_inner["plan_type"], "pro");
    assert_eq!(
        codex_inner["rate_limit"]["primary_window"]["used_percent"],
        30
    );
    assert_eq!(
        codex_inner["rate_limit"]["secondary_window"]["used_percent"],
        78
    );

    let claude_call = read_json(
        srv.req(Method::POST, "/v0/management/api-call")
            .header(header::AUTHORIZATION, format!("Bearer {key1}"))
            .json(&json!({ "auth_index": claude_beta, "method": "GET", "url": CLAUDE_USAGE_URL }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(claude_call["status_code"], 200);
    let claude_inner: Value = serde_json::from_str(claude_call["body"].as_str().unwrap()).unwrap();
    assert_eq!(claude_inner["five_hour"]["utilization"], 10);
    assert_eq!(claude_inner["seven_day"]["utilization"], 50);

    let gamma_call = read_json(
        srv.req(Method::POST, "/v0/management/api-call")
            .header(header::AUTHORIZATION, format!("Bearer {key1}"))
            .json(&json!({ "auth_index": codex_gamma, "method": "GET", "url": CODEX_USAGE_URL }))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(gamma_call["status_code"], 503);

    // 8. Refusals: unknown auth_index (404), provider mismatch (403), foreign proxy URL (403),
    // reset credits (403), POST/data write (403), /reset-quota (403), other paths (404), bad JSON (400).
    for (path, body, expected_status, expected_code) in [
        (
            "/v0/management/api-call",
            json!({"auth_index": "missing-id", "method": "GET", "url": CODEX_USAGE_URL}),
            StatusCode::NOT_FOUND,
            "unknown_auth_index",
        ),
        (
            "/v0/management/api-call",
            json!({"auth_index": codex_alpha, "method": "GET", "url": CLAUDE_USAGE_URL}),
            StatusCode::FORBIDDEN,
            "unsupported_api_call",
        ),
        (
            "/v0/management/api-call",
            json!({"auth_index": codex_alpha, "method": "GET", "url": "https://example.com/usage"}),
            StatusCode::FORBIDDEN,
            "unsupported_api_call",
        ),
        (
            "/v0/management/api-call",
            json!({"auth_index": codex_alpha, "method": "GET", "url": CODEX_CREDITS_URL}),
            StatusCode::FORBIDDEN,
            "unsupported_api_call",
        ),
        (
            "/v0/management/api-call",
            json!({"auth_index": codex_alpha, "method": "POST", "url": CODEX_USAGE_URL, "data": {}}),
            StatusCode::FORBIDDEN,
            "unsupported_api_call",
        ),
        (
            "/v0/management/reset-quota",
            json!({}),
            StatusCode::FORBIDDEN,
            "unsupported_api_call",
        ),
        (
            "/v0/management/other",
            json!({}),
            StatusCode::NOT_FOUND,
            "not_found",
        ),
    ] {
        let r = srv
            .req(Method::POST, path)
            .header(header::AUTHORIZATION, format!("Bearer {key1}"))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), expected_status, "{path} {body}");
        assert_eq!(read_json(r).await["code"], expected_code);
    }
    let bad_json = srv
        .req(Method::POST, "/v0/management/api-call")
        .header(header::AUTHORIZATION, format!("Bearer {key1}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body("{")
        .send()
        .await
        .unwrap();
    assert_eq!(bad_json.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(bad_json).await["code"], "invalid_json");

    // 9. Key rotation stops old key immediately and admits new key.
    write_private(
        &key_file,
        &json!({
            "version": 1,
            "keySha256": sha256_hex(key2.as_bytes()),
            "createdAt": "2026-11-26T00:01:00.000Z"
        })
        .to_string(),
    );
    assert_eq!(
        srv.req(Method::GET, "/v0/management/auth-files")
            .header(header::AUTHORIZATION, format!("Bearer {key1}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        srv.req(Method::GET, "/v0/management/auth-files")
            .header(header::AUTHORIZATION, format!("Bearer {key2}"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );

    // 10. Rate limit on wrong keys: 10 bad attempts from one peer -> 11th returns 429.
    let bad_peer = "192.168.1.99";
    for _ in 0..10 {
        let r = srv
            .req(Method::GET, "/v0/management/auth-files")
            .header(PEER_HEADER, bad_peer)
            .header(TLS_HEADER, "1")
            .header(header::AUTHORIZATION, format!("Bearer {key1}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::UNAUTHORIZED);
    }
    let rate_429 = srv
        .req(Method::GET, "/v0/management/auth-files")
        .header(PEER_HEADER, bad_peer)
        .header(TLS_HEADER, "1")
        .header(header::AUTHORIZATION, format!("Bearer {key2}"))
        .send()
        .await
        .unwrap();
    assert_eq!(rate_429.status(), StatusCode::TOO_MANY_REQUESTS);
}

#[tokio::test]
async fn static_ui_precompressed_pwa_traversal_dot_root_and_api_compression() {
    // Place UI fixture under a dot-prefixed ancestor directory (`.dot-install/ui`) to prove
    // dot-segment ancestor paths serve cleanly while dot-files inside the UI root remain hidden.
    let ui_tmp = tempfile::tempdir().unwrap();
    let ui_root = ui_tmp.path().join(".dot-install/ui");
    let build_id = "0123456789ab";
    let pkg_dir = ui_root.join(format!("pkg/{build_id}"));
    fs::create_dir_all(ui_root.join("app")).unwrap();
    fs::create_dir_all(&pkg_dir).unwrap();
    fs::create_dir_all(ui_root.join("icons")).unwrap();

    let index_html = b"<!doctype html><div id=\"aac-app-fixture\"></div>";
    let app_css = b"body { color: #eee; }";
    let bridge_js = b"console.log('bridge');";
    let sw_js = b"self.addEventListener('fetch', () => {});";
    let sw_route_js = b"export const route = 1;";
    let manifest_web = br#"{"name":"AI Account Center","icons":[{"src":"/icons/aac-192.png"}]}"#;
    let icon_png = b"\x89PNG\r\n\x1a\nfixture-icon";
    let apple_icon = b"\x89PNG\r\n\x1a\nfixture-apple-icon";
    let web_js =
        b"export function boot() { return 'aac-web-bundle-bytes-repeated-repeated-repeated'; }";
    let web_js_gz = gzip_bytes(web_js);
    let web_js_br = b"fixture-brotli-bytes-for-aac-web-js";
    let wasm_bytes = b"\0asm\x01\0\0\0fixture-wasm-bytes";
    let wasm_br = b"fixture-brotli-bytes-for-wasm";
    let tampered_js = b"export const tampered = true;";
    let tampered_br = b"tampered-br-bytes";

    fs::write(ui_root.join("app/index.html"), index_html).unwrap();
    fs::write(ui_root.join("app/app.css"), app_css).unwrap();
    fs::write(ui_root.join("bridge.js"), bridge_js).unwrap();
    fs::write(ui_root.join("sw.js"), sw_js).unwrap();
    fs::write(ui_root.join("sw-route.js"), sw_route_js).unwrap();
    fs::write(ui_root.join("manifest.webmanifest"), manifest_web).unwrap();
    fs::write(ui_root.join("icons/aac-192.png"), icon_png).unwrap();
    fs::write(
        ui_root.join("icons/apple-touch-icon-180-v2.png"),
        apple_icon,
    )
    .unwrap();
    fs::write(pkg_dir.join("aac_web.js"), web_js).unwrap();
    fs::write(pkg_dir.join("aac_web.js.gz"), &web_js_gz).unwrap();
    fs::write(pkg_dir.join("aac_web.js.br"), web_js_br).unwrap();
    fs::write(pkg_dir.join("aac_web_bg.wasm"), wasm_bytes).unwrap();
    fs::write(pkg_dir.join("aac_web_bg.wasm.br"), wasm_br).unwrap();
    fs::write(pkg_dir.join("tampered.js"), tampered_js).unwrap();
    fs::write(pkg_dir.join("tampered.js.br"), tampered_br).unwrap();

    fs::write(
        ui_root.join("ui-build-manifest.json"),
        json!({
            "precompressed": [
                {
                    "path": format!("pkg/{build_id}/aac_web.js"),
                    "file": format!("pkg/{build_id}/aac_web.js.br"),
                    "bytes": web_js_br.len(),
                    "sha256": sha256_hex(web_js_br),
                    "encoding": "br"
                },
                {
                    "path": format!("pkg/{build_id}/aac_web.js"),
                    "file": format!("pkg/{build_id}/aac_web.js.gz"),
                    "bytes": web_js_gz.len(),
                    "sha256": sha256_hex(&web_js_gz),
                    "encoding": "gzip"
                },
                {
                    "path": format!("pkg/{build_id}/aac_web_bg.wasm"),
                    "file": format!("pkg/{build_id}/aac_web_bg.wasm.br"),
                    "bytes": wasm_br.len(),
                    "sha256": sha256_hex(wasm_br),
                    "encoding": "br"
                },
                {
                    // Tampered SHA-256: must be disabled at startup and fall back to identity.
                    "path": format!("pkg/{build_id}/tampered.js"),
                    "file": format!("pkg/{build_id}/tampered.js.br"),
                    "bytes": tampered_br.len(),
                    "sha256": "0".repeat(64),
                    "encoding": "br"
                }
            ]
        })
        .to_string(),
    )
    .unwrap();

    let srv = EdgeServer::start(
        json!({
            "version": 2,
            "dashboard_auth": {
                "enabled": true,
                "username": USERNAME,
                "password_hash": bcrypt::hash(PASSWORD, 4).unwrap()
            }
        }),
        HubCache::default(),
        Some(ui_root.clone()),
    )
    .await;

    // 1. Shell pages serve app/index.html publicly with no-cache and security headers, even under dot-root!
    for page in [
        "/",
        "/app/home",
        "/app/status",
        "/app/accounts",
        "/app/analytics",
        "/app/scoreboard",
        "/login",
    ] {
        let r = srv.req(Method::GET, page).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::OK, "page {page}");
        assert_eq!(r.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
        assert_eq!(
            r.headers().get("x-content-type-options").unwrap(),
            "nosniff"
        );
        assert_eq!(
            r.headers().get("content-security-policy").unwrap(),
            "frame-ancestors 'self'"
        );
        assert_eq!(r.headers().get("referrer-policy").unwrap(), "same-origin");
        assert!(r.headers().get("x-powered-by").is_none());
        assert_eq!(r.bytes().await.unwrap().as_ref(), index_html);
    }

    // HEAD on a page ignores query and returns empty body.
    let head_page = srv
        .req(Method::HEAD, "/app/home?ignored=1")
        .send()
        .await
        .unwrap();
    assert_eq!(head_page.status(), StatusCode::OK);
    assert!(head_page.bytes().await.unwrap().is_empty());

    // 2. /bridge.js served as JavaScript, never HTML; PWA manifest, sw.js, sw-route.js, icons & /favicon.ico public.
    let bridge = srv.req(Method::GET, "/bridge.js").send().await.unwrap();
    assert_eq!(bridge.status(), StatusCode::OK);
    assert_eq!(
        bridge.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(bridge.bytes().await.unwrap().as_ref(), bridge_js);

    let pwa = srv
        .req(Method::GET, "/manifest.webmanifest")
        .send()
        .await
        .unwrap();
    assert_eq!(pwa.status(), StatusCode::OK);
    assert_eq!(
        pwa.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/manifest+json"
    );
    assert_eq!(pwa.bytes().await.unwrap().as_ref(), manifest_web);

    let sw = srv.req(Method::GET, "/sw.js").send().await.unwrap();
    assert_eq!(sw.status(), StatusCode::OK);
    assert_eq!(sw.headers().get(header::CACHE_CONTROL).unwrap(), "no-cache");
    assert_eq!(
        sw.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );

    let sw_route = srv.req(Method::GET, "/sw-route.js").send().await.unwrap();
    assert_eq!(sw_route.status(), StatusCode::OK);
    assert_eq!(
        sw_route.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    assert_eq!(
        sw_route.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );

    let icon_192 = srv
        .req(Method::GET, "/icons/aac-192.png")
        .send()
        .await
        .unwrap();
    assert_eq!(icon_192.status(), StatusCode::OK);
    assert_eq!(
        icon_192.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    assert_eq!(
        icon_192.headers().get(header::CONTENT_TYPE).unwrap(),
        "image/png"
    );
    assert_eq!(icon_192.bytes().await.unwrap().as_ref(), icon_png);

    let fav = srv.req(Method::GET, "/favicon.ico").send().await.unwrap();
    assert_eq!(fav.status(), StatusCode::OK);
    assert_eq!(
        fav.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    assert_eq!(fav.bytes().await.unwrap().as_ref(), apple_icon);

    let wasm_resp = srv
        .req(Method::GET, &format!("/pkg/{build_id}/aac_web_bg.wasm"))
        .header(header::ACCEPT_ENCODING, "br")
        .send()
        .await
        .unwrap();
    assert_eq!(wasm_resp.status(), StatusCode::OK);
    assert_eq!(
        wasm_resp.headers().get(header::CONTENT_ENCODING).unwrap(),
        "br"
    );
    assert_eq!(
        wasm_resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        wasm_resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "application/wasm"
    );
    assert_eq!(wasm_resp.bytes().await.unwrap().as_ref(), wasm_br);

    // 3. Precompressed .br / .gz negotiation (including q-values), immutable cache, ETag 304,
    // and tampered variant fallback to identity.
    let pkg_url = format!("/pkg/{build_id}/aac_web.js");
    let br_resp = srv
        .req(Method::GET, &pkg_url)
        .header(header::ACCEPT_ENCODING, "gzip;q=0.5, br;q=1.0")
        .send()
        .await
        .unwrap();
    assert_eq!(br_resp.status(), StatusCode::OK);
    assert_eq!(
        br_resp.headers().get(header::CONTENT_ENCODING).unwrap(),
        "br"
    );
    assert_eq!(
        br_resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "public, max-age=31536000, immutable"
    );
    assert_eq!(
        br_resp.headers().get(header::CONTENT_TYPE).unwrap(),
        "text/javascript; charset=utf-8"
    );
    assert_eq!(br_resp.bytes().await.unwrap().as_ref(), web_js_br);

    let gz_resp = srv
        .req(Method::GET, &pkg_url)
        .header(header::ACCEPT_ENCODING, "br;q=0, gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(gz_resp.status(), StatusCode::OK);
    assert_eq!(
        gz_resp.headers().get(header::CONTENT_ENCODING).unwrap(),
        "gzip"
    );
    let gz_etag = gz_resp
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(gunzip_bytes(&gz_resp.bytes().await.unwrap()), web_js);

    let not_mod = srv
        .req(Method::GET, &pkg_url)
        .header(header::ACCEPT_ENCODING, "gzip")
        .header(header::IF_NONE_MATCH, &gz_etag)
        .send()
        .await
        .unwrap();
    assert_eq!(not_mod.status(), StatusCode::NOT_MODIFIED);

    let tampered_resp = srv
        .req(Method::GET, &format!("/pkg/{build_id}/tampered.js"))
        .header(header::ACCEPT_ENCODING, "br, gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(tampered_resp.status(), StatusCode::OK);
    assert!(
        tampered_resp
            .headers()
            .get(header::CONTENT_ENCODING)
            .is_none(),
        "tampered variant must fall back to identity"
    );
    assert_eq!(tampered_resp.bytes().await.unwrap().as_ref(), tampered_js);

    // 4. Denied files, direct .br/.gz, build manifest, dot-segment traversal, trailing /. -> 404.
    for denied in [
        format!("/pkg/{build_id}/aac_web.js.br"),
        format!("/pkg/{build_id}/aac_web.js.gz"),
        "/ui-build-manifest.json".to_owned(),
    ] {
        let r = srv.req(Method::GET, &denied).send().await.unwrap();
        assert_eq!(r.status(), StatusCode::NOT_FOUND, "denied path: {denied}");
    }
    // Send raw wire requests for encoded dot-segments and trailing /. so reqwest/url does not pre-normalize them.
    for raw_denied in [
        "/ui-build-manifest.json/.",
        "/app/%2e%2e/%2e%2e/package.json",
        "/app/..%2f..%2fpackage.json",
    ] {
        let mut stream = tokio::net::TcpStream::connect(srv.addr).await.unwrap();
        let wire = format!(
            "GET {raw_denied} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            srv.addr
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        reader.read_line(&mut status_line).await.unwrap();
        assert!(
            status_line.contains(" 404 "),
            "raw denied path {raw_denied} must return 404, got: {status_line:?}"
        );
    }

    // Legacy aliases redirect cleanly; unknown page redirects to /.
    let redir_settings = srv.req(Method::GET, "/settings").send().await.unwrap();
    assert_eq!(redir_settings.status(), StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        redir_settings.headers().get(header::LOCATION).unwrap(),
        "/accounts"
    );

    let unknown_page = srv
        .req(Method::GET, "/definitely-not-a-page")
        .send()
        .await
        .unwrap();
    assert_eq!(unknown_page.status(), StatusCode::FOUND);
    assert_eq!(unknown_page.headers().get(header::LOCATION).unwrap(), "/");

    // 5. Rolling session cookie lands on pages & API, NOT on static assets; API compression & error JSON.
    let login = srv
        .req(Method::POST, "/api/auth/login")
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    let cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let page_with_cookie = srv
        .req(Method::GET, "/")
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert!(
        page_with_cookie.headers().get(header::SET_COOKIE).is_some(),
        "pages carry rolling session cookie"
    );
    let asset_with_cookie = srv
        .req(Method::GET, "/app/app.css")
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert!(
        asset_with_cookie
            .headers()
            .get(header::SET_COOKIE)
            .is_none(),
        "static assets must not carry rolling session cookie"
    );

    // Large /api JSON is gzipped when Accept-Encoding: gzip; small /api JSON stays uncompressed.
    let big_gz = srv
        .req(Method::GET, "/api/big-json")
        .header(header::COOKIE, &cookie)
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(big_gz.status(), StatusCode::OK);
    assert_eq!(
        big_gz.headers().get(header::CONTENT_ENCODING).unwrap(),
        "gzip"
    );
    let big_decoded: Value =
        serde_json::from_slice(&gunzip_bytes(&big_gz.bytes().await.unwrap())).unwrap();
    assert_eq!(big_decoded["items"].as_array().unwrap().len(), 120);

    let small_gz = srv
        .req(Method::GET, "/api/small-json")
        .header(header::COOKIE, &cookie)
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(small_gz.status(), StatusCode::OK);
    assert!(small_gz.headers().get(header::CONTENT_ENCODING).is_none());

    // Unknown /api returns JSON 404; oversized body returns JSON 413; malformed JSON returns JSON 400 (no stack trace).
    let api_404 = srv
        .req(Method::GET, "/api/definitely-not-a-route")
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(api_404.status(), StatusCode::NOT_FOUND);
    assert!(read_json(api_404).await["error"].is_string());

    let oversized = srv
        .req(Method::POST, "/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body(format!(r#"{{"pad":"{}"}}"#, "x".repeat(120 * 1024)))
        .send()
        .await
        .unwrap();
    assert_eq!(oversized.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let over_json = read_json(oversized).await;
    assert_eq!(over_json["code"], "body_too_large");

    let malformed = srv
        .req(Method::POST, "/api/auth/login")
        .header(header::CONTENT_TYPE, "application/json")
        .body("{not-json")
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(malformed).await["code"], "invalid_json");
}

#[tokio::test]
async fn m0_public_route_role_contract_product_surface_and_store_off() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    let ccs = root.join("home/.ccs");
    let reports = root.join("reports");
    let registry = root.join("registry");
    let stubs = root.join("stubs");
    let xdg = root.join("xdg");
    for d in [&ccs, &reports, &registry, &stubs, &xdg] {
        fs::create_dir_all(d).unwrap();
    }
    fs::set_permissions(&ccs, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&xdg, fs::Permissions::from_mode(0o700)).unwrap();
    for cmd in ["ssh", "simlock", "browser-check", "pm-mail"] {
        let p = stubs.join(cmd);
        fs::write(&p, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    }
    write_private(
        &ccs.join("config.yaml"),
        &serde_yaml::to_string(&json!({
            "version": 2,
            "dashboard_auth": {
                "enabled": true,
                "username": USERNAME,
                "password_hash": bcrypt::hash(PASSWORD, 4).unwrap()
            }
        }))
        .unwrap(),
    );

    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let path_env = std::env::join_paths([stubs.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
    let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);

    struct Guard(Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.start_kill();
        }
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-account-center"))
        .args([
            "dashboard",
            "--no-open",
            "--host",
            "127.0.0.1",
            "--port",
            &port.to_string(),
            "--config-dir",
        ])
        .arg(&ccs)
        .env_clear()
        .env("PATH", path_env)
        .env("HOME", root.join("home"))
        .env("CCS_HOME", root.join("home"))
        .env("CCS_DIR", &ccs)
        .env("XDG_RUNTIME_DIR", &xdg)
        .env("AAC_AGENT_REPORTS_DIR", &reports)
        .env("AAC_PM_MAIL_REGISTRY_DIR", &registry)
        .env("AAC_AGENT_TRANSPORT", "off")
        .env("AAC_AGENT_VERBS", "off")
        .env("AAC_STATUS_DOORBELL", "off")
        .env("AAC_STORE_SHARE_BACKUP_DIR", "off")
        .env("AAC_STORE", "off")
        .env("AAC_RUST_GROUP_COLLECTION", "off")
        .env("AAC_RUST_GROUP_RENEWAL", "off")
        .env("AAC_CURSOR_BILLING_ENABLED", "0")
        .env("AAC_AGENT_PACKAGE_ROOT", package)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    tokio::time::timeout(Duration::from_secs(20), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if line.contains("public listening on") {
                break;
            }
        }
    })
    .await
    .expect("dashboard listener started");
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    let _guard = Guard(child);

    let base = format!("http://127.0.0.1:{port}");
    let client = Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .unwrap();

    let login = client
        .post(format!("{base}/api/auth/login"))
        .header(header::ORIGIN, &base)
        .json(&json!({"username": USERNAME, "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login
        .headers()
        .get(header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_owned();

    let pair = client
        .post(format!("{base}/api/auth/devices/pair"))
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "ContractTray",
            "platform": "windows"
        }))
        .send()
        .await
        .unwrap();
    let device_token = read_json(pair).await["token"].as_str().unwrap().to_owned();

    // 1. Replay the checked-in M0 route inventory (`tests/spec/routes.json`):
    //    every non-public `/api/*` route enforces 401 when signed out, and every non-tray route
    //    enforces 403 device_scope for a paired device token.
    let routes: Vec<Value> =
        serde_json::from_str(include_str!("../../../tests/spec/routes.json")).unwrap();
    assert!(routes.len() >= 40, "routes.json must remain populated");

    let public_api = [
        ("GET", "/api/health"),
        ("GET", "/api/auth/check"),
        ("GET", "/api/auth/setup"),
        ("POST", "/api/auth/login"),
        ("POST", "/api/auth/setup"),
        ("POST", "/api/auth/devices/pair"),
    ];
    let device_allowed_get = [
        "/api/accounts/dashboard",
        "/api/claude/desktop-profiles",
        "/api/antigravity/profiles",
        "/api/bar/auth",
        "/api/auth/devices/me",
        "/api/store/status",
    ];

    for entry in &routes {
        let method_str = entry["method"].as_str().unwrap();
        let raw_path = entry["path"].as_str().unwrap();
        if !raw_path.starts_with("/api/") {
            continue;
        }
        let concrete = raw_path
            .replace(":accountId", "codex:fixture")
            .replace(":id", "fixture-id")
            .replace(":jobId", "job_0123456789abcdef")
            .replace(":name", "fixture-name")
            .replace(":kind", "history")
            .replace(":filename", "fixture.txt")
            .replace("{*splat}", "fixture");
        if concrete.contains('{') || concrete.contains(':') {
            continue;
        }
        let method: Method = method_str.parse().unwrap();
        let is_public = public_api
            .iter()
            .any(|(m, p)| *m == method_str && *p == raw_path);

        if !is_public {
            let signed_out = client
                .request(method.clone(), format!("{base}{concrete}"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                signed_out.status(),
                StatusCode::UNAUTHORIZED,
                "signed-out must get 401 on {method_str} {concrete}"
            );
        }

        let is_device_allowed = (method_str == "GET"
            && (device_allowed_get.contains(&concrete.as_str())
                || concrete.starts_with("/api/usage/")))
            || (method_str == "POST"
                && (concrete == "/api/auth/devices/me/rotate"
                    || (concrete.starts_with("/api/claude/desktop-profiles/")
                        && concrete.ends_with("/open"))))
            || (method_str == "DELETE" && concrete == "/api/auth/devices/me");
        if !is_public && !is_device_allowed {
            let dev = client
                .request(method, format!("{base}{concrete}"))
                .header(header::AUTHORIZATION, format!("Bearer {device_token}"))
                .send()
                .await
                .unwrap();
            assert_eq!(
                dev.status(),
                StatusCode::FORBIDDEN,
                "device token must get 403 on {method_str} {concrete}"
            );
        }
    }

    // 2. Product surface: lightweight /api/health is public; retired routes answer 404.
    let health = client
        .get(format!("{base}/api/health"))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert_eq!(read_json(health).await, json!({"status": "ok"}));

    for (method, retired) in [
        (Method::GET, "/api/health/system"),
        (Method::POST, "/api/health/repair"),
        (Method::POST, "/api/codex/activate"),
        (Method::PUT, "/api/codex/auto-switch"),
        (Method::POST, "/internal/dashboard-events"),
    ] {
        let r = client
            .request(method.clone(), format!("{base}{retired}"))
            .header(header::COOKIE, &cookie)
            .header(header::ORIGIN, &base)
            .send()
            .await
            .unwrap();
        assert_eq!(
            r.status(),
            StatusCode::NOT_FOUND,
            "retired {method} {retired}"
        );
    }

    // 3. Store-off invariants (`AAC_STORE=off`): store routes report explicit unavailability without
    // creating aac.redb, admit browser & device HEAD, and /api/status stays alive.
    assert!(!ccs.join("store/aac.redb").exists());
    for path in [
        "/api/store/status",
        "/api/usage/history",
        "/api/usage/at",
        "/api/usage/burn",
    ] {
        let head_dev = client
            .head(format!("{base}{path}"))
            .header(header::AUTHORIZATION, format!("Bearer {device_token}"))
            .send()
            .await
            .unwrap();
        assert_ne!(head_dev.status(), StatusCode::UNAUTHORIZED, "HEAD {path}");
        assert_ne!(head_dev.status(), StatusCode::FORBIDDEN, "HEAD {path}");
    }
    let store_status = client
        .get(format!("{base}/api/store/status"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(store_status.status(), StatusCode::OK);
    let store_json = read_json(store_status).await;
    assert_eq!(store_json["state"], "unavailable");
    assert_eq!(store_json["reason"], "disabled");
    assert!(!ccs.join("store/aac.redb").exists());

    let status_live = client
        .get(format!("{base}/api/status"))
        .header(header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(status_live.status(), StatusCode::OK);
}

#[tokio::test]
async fn websocket_events_redaction_role_isolation_and_logout_close() {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    write_private(
        &root.join("config.yaml"),
        &serde_yaml::to_string(&json!({
            "version": 2,
            "dashboard_auth": {
                "enabled": true,
                "username": USERNAME,
                "password_hash": bcrypt::hash(PASSWORD, 4).unwrap()
            }
        }))
        .unwrap(),
    );

    struct Guard(Child);
    impl Drop for Guard {
        fn drop(&mut self) {
            let _ = self.0.start_kill();
        }
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_aac-http"))
        .arg("--public")
        .env_clear()
        .env("HOME", &root)
        .env("CCS_HOME", &root)
        .env("CCS_DIR", &root)
        .env("AAC_RUST_PUBLIC_LISTEN", "127.0.0.1:0")
        .env("AAC_RUST_PUBLIC_EVENT_STDIN", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let mut feed = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut lines = BufReader::new(stdout).lines();
    let addr = tokio::time::timeout(Duration::from_secs(12), async {
        while let Ok(Some(line)) = lines.next_line().await {
            if let Some(addr) = line.strip_prefix("aac-http public listening on ") {
                return addr.trim().to_owned();
            }
        }
        panic!("aac-http did not print listen address");
    })
    .await
    .unwrap();
    tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });
    let _guard = Guard(child);

    let base = format!("http://{addr}");
    let ws_base = format!("ws://{addr}");
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();

    // Login secure loopback browser + plain-transport browser (foreign Host) + pair a device.
    let login_host = |host: String| {
        let client = client.clone();
        let base = base.clone();
        async move {
            let r = client
                .post(format!("{base}/api/auth/login"))
                .header(header::HOST, &host)
                .json(&json!({"username": USERNAME, "password": PASSWORD}))
                .send()
                .await
                .unwrap();
            assert_eq!(r.status(), StatusCode::OK);
            r.headers()
                .get(header::SET_COOKIE)
                .unwrap()
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        }
    };
    let secure_cookie = login_host(addr.clone()).await;
    let plain_host = "dashboard.example.test".to_owned();
    let plain_cookie = login_host(plain_host.clone()).await;

    let pair = client
        .post(format!("{base}/api/auth/devices/pair"))
        .json(&json!({
            "username": USERNAME,
            "password": PASSWORD,
            "deviceName": "WsTray",
            "platform": "mac"
        }))
        .send()
        .await
        .unwrap();
    let device_token = read_json(pair).await["token"].as_str().unwrap().to_owned();

    // WS admission guards: signed-out (401), device token (403), cross-site origin (403), bad path (404).
    let ws_status =
        |path: &str, host: &str, origin: &str, cookie: Option<&str>, bearer: Option<&str>| {
            let mut req = format!("{ws_base}{path}").into_client_request().unwrap();
            req.headers_mut()
                .insert(header::HOST, host.parse().unwrap());
            req.headers_mut()
                .insert(header::ORIGIN, origin.parse().unwrap());
            if let Some(c) = cookie {
                req.headers_mut().insert(header::COOKIE, c.parse().unwrap());
            }
            if let Some(b) = bearer {
                req.headers_mut().insert(
                    header::AUTHORIZATION,
                    format!("Bearer {b}").parse().unwrap(),
                );
            }
            async move {
                match connect_async(req).await {
                    Err(WsError::Http(resp)) => resp.status(),
                    other => panic!("expected HTTP refusal, got {other:?}"),
                }
            }
        };
    assert_eq!(
        ws_status("/ws", &addr, &base, None, None).await,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        ws_status("/ws", &addr, &base, None, Some(&device_token)).await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ws_status(
            "/ws",
            &addr,
            "http://evil.example",
            Some(&secure_cookie),
            None
        )
        .await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        ws_status("/ws/other", &addr, &base, Some(&secure_cookie), None).await,
        StatusCode::NOT_FOUND
    );

    // Connect secure + plain browser sockets.
    let open_ws = |host: &str, cookie: &str| {
        let mut req = format!("{ws_base}/ws").into_client_request().unwrap();
        req.headers_mut()
            .insert(header::HOST, host.parse().unwrap());
        req.headers_mut()
            .insert(header::ORIGIN, format!("http://{host}").parse().unwrap());
        req.headers_mut()
            .insert(header::COOKIE, cookie.parse().unwrap());
        async move { connect_async(req).await.unwrap().0 }
    };
    let mut secure_ws = open_ws(&addr, &secure_cookie).await;
    let mut plain_ws = open_ws(&plain_host, &plain_cookie).await;

    // Feed all 6 event types over the process-private stdin pipe.
    let clock = "2026-11-19T00:00:00.000Z";
    let waiting_job = json!({
        "id": "job_0123456789abcdef",
        "provider": "codex",
        "kind": "device-code",
        "mode": "add",
        "accountId": null,
        "profileName": "test-profile",
        "platform": "ubuntu",
        "state": "waiting",
        "verification": { "url": "https://example.test/verify", "userCode": "FIXTURE-CODE", "expiresAt": null },
        "result": null,
        "error": null,
        "startedAt": clock,
        "updatedAt": clock,
        "expiresAt": clock
    });
    let done_job = json!({
        "id": "job_0123456789abcdef",
        "provider": "codex",
        "kind": "device-code",
        "mode": "add",
        "accountId": null,
        "profileName": "test-profile",
        "platform": "ubuntu",
        "state": "succeeded",
        "verification": null,
        "result": { "accountId": "fixture", "plan": "pro", "email": "user@example.test" },
        "error": null,
        "startedAt": clock,
        "updatedAt": clock,
        "expiresAt": clock
    });
    let events = [
        json!({"type": "accounts-changed"}),
        json!({"type": "status-changed"}),
        json!({"type": "analytics-changed"}),
        json!({"type": "reset-claims-changed", "id": "a".repeat(32)}),
        json!({"type": "signin-job", "job": waiting_job}),
        json!({"type": "signin-job", "job": done_job}),
        json!({
            "type": "app-update-job",
            "job": {
                "id": "fixture-update",
                "state": "running",
                "cancelRequested": false,
                "completedResults": 0,
                "expectedResults": 1,
                "hosts": [{ "host": "ubuntu", "state": "running", "currentApp": "codex-cli", "phase": "downloading" }]
            }
        }),
    ];
    for ev in &events {
        feed.write_all(format!("{ev}\n").as_bytes()).await.unwrap();
    }
    feed.flush().await.unwrap();

    let mut secure_got = Vec::new();
    let mut plain_got = Vec::new();
    for _ in 0..events.len() {
        let s = tokio::time::timeout(Duration::from_secs(5), secure_ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let p = tokio::time::timeout(Duration::from_secs(5), plain_ws.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Text(st) = s else {
            panic!("expected text frame")
        };
        let Message::Text(pt) = p else {
            panic!("expected text frame")
        };
        secure_got.push(serde_json::from_str::<Value>(&st).unwrap());
        plain_got.push(serde_json::from_str::<Value>(&pt).unwrap());
    }

    // Secure socket gets verification userCode & result email; plain socket gets both redacted to null!
    assert_eq!(
        secure_got[4]["job"]["verification"]["userCode"],
        "FIXTURE-CODE"
    );
    assert_eq!(secure_got[5]["job"]["result"]["email"], "user@example.test");
    assert_eq!(plain_got[4]["job"]["verification"], Value::Null);
    assert_eq!(plain_got[5]["job"]["result"]["email"], Value::Null);
    assert_eq!(secure_got[6]["job"]["id"], "fixture-update");
    assert_eq!(plain_got[6]["job"]["hosts"][0]["phase"], "downloading");

    // Logging out plain_cookie closes ONLY plain_ws; secure_ws stays open and receives next event.
    let logout = client
        .post(format!("{base}/api/auth/logout"))
        .header(header::HOST, &plain_host)
        .header(header::COOKIE, &plain_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), StatusCode::OK);

    let closed = tokio::time::timeout(Duration::from_secs(5), plain_ws.next())
        .await
        .expect("logged-out socket must close");
    assert!(
        matches!(closed, None | Some(Ok(Message::Close(_))) | Some(Err(_))),
        "expected closed plain socket, got {closed:?}"
    );

    feed.write_all(b"{\"type\":\"status-changed\"}\n")
        .await
        .unwrap();
    feed.flush().await.unwrap();
    let still_open = tokio::time::timeout(Duration::from_secs(5), secure_ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(txt) = still_open else {
        panic!("expected text frame")
    };
    assert_eq!(
        serde_json::from_str::<Value>(&txt).unwrap()["type"],
        "status-changed"
    );
}

#[test]
fn cli_edge_and_retired_bins_contract() {
    let dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(dir.path()).unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_ai-account-center"));

    let run = |exe: &Path, args: &[&str]| {
        StdCommand::new(exe)
            .args(args)
            .env_clear()
            .env("HOME", &home)
            .env("CCS_HOME", &home)
            .env("CI", "1")
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };

    // Core help, version, dashboard help, and config alias help exit 0 with ASCII text.
    for args in [
        &["--help"][..],
        &["--version"][..],
        &["dashboard", "--help"][..],
        &["config", "--help"][..],
    ] {
        let out = run(bin, args);
        assert!(
            out.status.success(),
            "args={args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.is_ascii(), "CLI output must stay ASCII for {args:?}");
        assert!(!stdout.trim().is_empty());
    }

    // Retired commands exit non-zero with clear guidance.
    for args in [&["update"][..], &["launch", "work"][..]] {
        let out = run(bin, args);
        assert!(
            !out.status.success(),
            "retired command {args:?} must exit non-zero"
        );
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            combined.contains("ai-account-center") || combined.contains("retired"),
            "missing guidance for {args:?}: {combined}"
        );
    }

    // Retired legacy compatibility bins print migration guidance and exit 1.
    for retired_name in ["ccs-droid", "ccsd", "ccs-codex", "ccsx", "ccsxp"] {
        let compat = home.join(retired_name);
        if fs::hard_link(bin, &compat).is_err() {
            fs::copy(bin, &compat).unwrap();
        }
        let out = run(&compat, &[]);
        assert_eq!(out.status.code(), Some(1), "retired bin {retired_name}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(
            stderr.contains(retired_name),
            "{retired_name} stderr: {stderr}"
        );
        assert!(
            stderr.contains("ai-account-center dashboard"),
            "{retired_name} missing dashboard guidance: {stderr}"
        );
        assert!(stderr.is_ascii());
    }

    // Also prove the symlinked/non-existent --config-dir error contract exits 1 cleanly.
    let missing_dir = home.join("does-not-exist");
    let missing = run(
        bin,
        &["--config-dir", missing_dir.to_str().unwrap(), "--version"],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("Config directory not found"),
        "expected missing config-dir guidance"
    );

    // Enforce the recorded CLI contract in tests/spec/contract/m6-edge.json verbatim (replacing m6-edge-parity.ts).
    let edge_golden: Value = serde_json::from_str(
        &fs::read_to_string(
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/contract/m6-edge.json"),
        )
        .unwrap(),
    )
    .unwrap();
    let real_bin = fs::canonicalize(bin).unwrap();
    for entry in edge_golden["cli"].as_array().unwrap() {
        let case = entry["case"].as_str().unwrap();
        let argv: Vec<&str> = entry["argv"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        let exe = if case == "cli: retired legacy bin" {
            home.join("ccs-droid")
        } else {
            bin.to_path_buf()
        };
        let out = run(&exe, &argv);
        assert_eq!(
            out.status.code(),
            Some(entry["status"].as_i64().unwrap() as i32),
            "m6-edge.json CLI status mismatch for {case}"
        );
        let norm = |raw: &[u8]| -> String {
            String::from_utf8_lossy(raw)
                .replace(real_bin.to_str().unwrap(), "<executable>")
                .replace(bin.to_str().unwrap(), "<executable>")
                .replace(home.to_str().unwrap(), "<fixture-home>")
        };
        assert_eq!(
            norm(&out.stdout),
            entry["stdout"].as_str().unwrap(),
            "m6-edge.json CLI stdout mismatch for {case}"
        );
        assert_eq!(
            norm(&out.stderr),
            entry["stderr"].as_str().unwrap(),
            "m6-edge.json CLI stderr mismatch for {case}"
        );
    }

    // Symlink smoke (ensure symlink helper import is exercised cleanly for dot-root/file checks).
    let link = home.join("smoke-link");
    symlink(home.join("does-not-exist"), &link).unwrap();
    assert!(
        fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn cli_usage_hub_and_proxy_commands() {
    let dir = tempfile::tempdir().unwrap();
    let home = fs::canonicalize(dir.path()).unwrap();
    let ccs = home.join(".ccs");
    fs::create_dir_all(&ccs).unwrap();
    fs::set_permissions(&ccs, fs::Permissions::from_mode(0o700)).unwrap();
    let bin = Path::new(env!("CARGO_BIN_EXE_ai-account-center"));

    let run = |args: &[&str]| {
        StdCommand::new(bin)
            .args(args)
            .env_clear()
            .env("HOME", &home)
            .env("CCS_HOME", &home)
            .env("CI", "1")
            .env("NO_COLOR", "1")
            .output()
            .unwrap()
    };

    // 1. usage-hub: status (off) -> generate requires --stdout -> generate --stdout -> duplicate refused -> status (on) -> rotate -> off.
    let status_off = run(&["dashboard", "usage-hub", "status"]);
    assert!(status_off.status.success());
    assert!(String::from_utf8_lossy(&status_off.stdout).contains("Usage hub: off"));

    let gen_no_flag = run(&["dashboard", "usage-hub", "generate"]);
    assert_eq!(gen_no_flag.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&gen_no_flag.stderr).contains("--stdout"));

    let gen_ok = run(&["dashboard", "usage-hub", "generate", "--stdout"]);
    assert!(
        gen_ok.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&gen_ok.stderr)
    );
    let key1 = String::from_utf8_lossy(&gen_ok.stdout).trim().to_owned();
    assert!(
        key1.starts_with("aacu_"),
        "unexpected hub key format: {key1}"
    );

    let gen_dup = run(&["dashboard", "usage-hub", "generate", "--stdout"]);
    assert_eq!(gen_dup.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&gen_dup.stderr).contains("already exists"));

    let status_on = run(&["dashboard", "usage-hub", "status"]);
    assert!(status_on.status.success());
    assert!(String::from_utf8_lossy(&status_on.stdout).contains("Usage hub: on"));
    assert!(!String::from_utf8_lossy(&status_on.stdout).contains(&key1));

    let rot_ok = run(&["dashboard", "usage-hub", "rotate", "--stdout"]);
    assert!(rot_ok.status.success());
    let key2 = String::from_utf8_lossy(&rot_ok.stdout).trim().to_owned();
    assert!(key2.starts_with("aacu_") && key2 != key1);

    let off_ok = run(&["dashboard", "usage-hub", "off"]);
    assert!(off_ok.status.success());
    let off_again = run(&["dashboard", "usage-hub", "off"]);
    assert!(off_again.status.success());
    assert!(String::from_utf8_lossy(&off_again.stderr).contains("already off"));

    // 2. dashboard proxy: status (off) -> invalid args refused -> set valid LAN IP + origin -> status (on) + backup created -> off.
    let proxy_off = run(&["dashboard", "proxy", "status"]);
    assert!(proxy_off.status.success());
    assert!(String::from_utf8_lossy(&proxy_off.stdout).contains("LAN HTTPS proxy: off"));

    let bad_loopback = run(&[
        "dashboard",
        "proxy",
        "set",
        "--address",
        "127.0.0.1",
        "--origin",
        "https://aac.example.test",
    ]);
    assert_eq!(bad_loopback.status.code(), Some(1));

    let bad_http = run(&[
        "dashboard",
        "proxy",
        "set",
        "--address",
        "192.168.1.20",
        "--origin",
        "http://aac.example.test",
    ]);
    assert_eq!(bad_http.status.code(), Some(1));

    // Seed an existing config.yaml so backup behavior is exercised too.
    write_private(&ccs.join("config.yaml"), "version: 2\n");
    let proxy_set = run(&[
        "dashboard",
        "proxy",
        "set",
        "--address",
        "192.168.1.20",
        "--origin",
        "https://aac.example.test",
    ]);
    assert!(
        proxy_set.status.success(),
        "proxy set failed: {}",
        String::from_utf8_lossy(&proxy_set.stderr)
    );
    let proxy_on = run(&["dashboard", "proxy", "status"]);
    assert!(proxy_on.status.success());
    let on_out = String::from_utf8_lossy(&proxy_on.stdout);
    assert!(on_out.contains("LAN HTTPS proxy: on"), "{on_out}");
    assert!(on_out.contains("192.168.1.20"), "{on_out}");
    assert!(on_out.contains("https://aac.example.test"), "{on_out}");

    // Backup file created with 0600 permissions.
    let backups: Vec<_> = fs::read_dir(&ccs)
        .unwrap()
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("config.yaml.bak-proxy-")
        })
        .collect();
    assert!(
        !backups.is_empty(),
        "expected proxy backup file next to config.yaml"
    );
    assert_eq!(
        fs::metadata(backups[0].path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );

    let proxy_turn_off = run(&["dashboard", "proxy", "off"]);
    assert!(proxy_turn_off.status.success());
    let proxy_after_off = run(&["dashboard", "proxy", "status"]);
    assert!(String::from_utf8_lossy(&proxy_after_off.stdout).contains("LAN HTTPS proxy: off"));
}
