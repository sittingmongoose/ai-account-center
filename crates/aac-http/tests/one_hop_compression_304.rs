//! Red-first integration tests for Phase 4 step 4: one hop, cached gzip compression,
//! and ETag / 304 Not Modified across Status, Scoreboard, Analytics, Home, and Accounts.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use flate2::read::GzDecoder;
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use std::{fs, io::Read, os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};

struct RunningServer {
    _fixture: tempfile::TempDir,
    child: Child,
    base: String,
    origin: String,
    cookie: String,
    client: Client,
}

impl Drop for RunningServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

fn write_private(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = GzDecoder::new(bytes);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).expect("valid gzip stream");
    out
}

fn strip_clocks(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for key in [
                "generatedAt",
                "cacheAgeSeconds",
                "ageSeconds",
                "updatedAt",
                "asOf",
                "checkedAt",
                "scannedAt",
            ] {
                map.remove(key);
            }
            for child in map.values_mut() {
                strip_clocks(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                strip_clocks(child);
            }
        }
        _ => {}
    }
}

async fn start_fixture_server() -> RunningServer {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let ccs = root.join("home/.ccs");
    let reports = root.join("reports");
    let registry = root.join("registry");
    let stubs = root.join("stubs");
    let xdg = root.join("xdg");
    for dir in [&ccs, &reports, &registry, &stubs, &xdg] {
        fs::create_dir_all(dir).unwrap();
    }
    fs::set_permissions(&ccs, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&xdg, fs::Permissions::from_mode(0o700)).unwrap();

    // Populate several synthetic writers so Status & Scoreboard are solidly > 1 KB.
    let writers = reports.join("v1/writers");
    for i in 0..8 {
        let wdir = writers.join(format!("synth-w-{i:02}"));
        fs::create_dir_all(&wdir).unwrap();
        fs::write(
            wdir.join("thread.json"),
            json!({
                "schema": "agent-report.thread.v1",
                "writer": format!("synth-w-{i:02}"),
                "project": "fixture-proj",
                "topic": format!("Fixture worker {i}"),
                "host": "ubuntu-fixture",
                "startedAt": "2026-11-25T10:00:00.000Z",
                "updatedAt": "2026-11-25T12:00:00.000Z"
            })
            .to_string(),
        )
        .unwrap();
        fs::write(
            wdir.join("status.json"),
            json!({
                "schema": "agent-report.status.v1",
                "writer": format!("synth-w-{i:02}"),
                "state": "working",
                "summary": format!("Synthetic status payload entry {i} for compression coverage."),
                "updatedAt": "2026-11-25T12:00:00.000Z"
            })
            .to_string(),
        )
        .unwrap();
    }

    for cmd in ["ssh", "simlock", "browser-check", "pm-mail"] {
        let path = stubs.join(cmd);
        fs::write(&path, "#!/bin/sh\nexit 1\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    let password = "fixture-one-hop-password";
    write_private(
        &ccs.join("config.yaml"),
        &json!({
            "version": 2,
            "dashboard_auth": {
                "enabled": true,
                "username": "fixture-admin",
                "password_hash": bcrypt::hash(password, 4).unwrap()
            }
        })
        .to_string(),
    );

    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let path_env = std::env::join_paths([stubs.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = socket.local_addr().unwrap().port();
    drop(socket);

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

    let base = format!("http://127.0.0.1:{port}");
    let origin = base.clone();
    let client = Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .unwrap();

    let login = client
        .post(format!("{base}/api/auth/login"))
        .header(header::ORIGIN, &origin)
        .header(header::CONTENT_TYPE, "application/json")
        .body(
            json!({
                "username": "fixture-admin",
                "password": password,
                "rememberMe": true
            })
            .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(|s| s.split(';').next().unwrap().to_owned())
        .collect::<Vec<_>>()
        .join("; ");
    assert!(!cookie.is_empty(), "session cookie present");

    for path in [
        "/api/status",
        "/api/scoreboard",
        "/api/accounts/analytics?platform=mac&range=7d&tz=UTC",
        "/api/views/home?platform=mac&refresh=false",
        "/api/accounts/dashboard?platform=mac&refresh=false",
    ] {
        let mut ready = false;
        for _ in 0..40 {
            let resp = client
                .get(format!("{base}{path}"))
                .header(header::COOKIE, &cookie)
                .header(header::ORIGIN, &origin)
                .send()
                .await
                .unwrap();
            if resp.status() == StatusCode::OK {
                ready = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
        assert!(ready, "route {path} did not become ready");
    }

    RunningServer {
        _fixture: fixture,
        child,
        base,
        origin,
        cookie,
        client,
    }
}

fn vary_has_accept_encoding(headers: &header::HeaderMap) -> bool {
    headers
        .get_all(header::VARY)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|s| s.split(','))
        .any(|token| token.trim().eq_ignore_ascii_case("accept-encoding"))
}

#[tokio::test]
async fn cached_api_routes_compress_with_gzip_and_respect_accept_encoding() {
    let srv = start_fixture_server().await;
    let routes = [
        "/api/status",
        "/api/scoreboard",
        "/api/accounts/analytics?platform=mac&range=7d&tz=UTC",
        "/api/views/home?platform=mac&refresh=false",
        "/api/accounts/dashboard?platform=mac&refresh=false",
    ];

    for path in routes {
        let plain = srv
            .client
            .get(format!("{}{path}", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .send()
            .await
            .unwrap();
        assert_eq!(plain.status(), StatusCode::OK, "{path} plain");
        assert!(
            plain.headers().get(header::CONTENT_ENCODING).is_none(),
            "{path} must not send Content-Encoding without Accept-Encoding"
        );
        let plain_bytes = plain.bytes().await.unwrap();
        assert!(
            plain_bytes.len() >= 1024,
            "{path} fixture payload should exceed 1024 bytes, got {}",
            plain_bytes.len()
        );
        let mut plain_json: Value = serde_json::from_slice(&plain_bytes).unwrap();
        strip_clocks(&mut plain_json);

        let gz = srv
            .client
            .get(format!("{}{path}", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .header(header::ACCEPT_ENCODING, "br, gzip")
            .send()
            .await
            .unwrap();
        assert_eq!(gz.status(), StatusCode::OK, "{path} gzip");
        assert_eq!(
            gz.headers()
                .get(header::CONTENT_ENCODING)
                .and_then(|v| v.to_str().ok()),
            Some("gzip"),
            "{path} must reply Content-Encoding: gzip"
        );
        assert!(
            vary_has_accept_encoding(gz.headers()),
            "{path} must include Accept-Encoding in Vary"
        );
        let gz_bytes = gz.bytes().await.unwrap();
        assert!(
            gz_bytes.len() < plain_bytes.len(),
            "{path} gzip ({}) must be smaller than plain ({})",
            gz_bytes.len(),
            plain_bytes.len()
        );
        let decoded = gunzip(&gz_bytes);
        let mut decoded_json: Value = serde_json::from_slice(&decoded).unwrap();
        strip_clocks(&mut decoded_json);
        assert_eq!(decoded_json, plain_json, "{path} decompressed JSON parity");

        let refused = srv
            .client
            .get(format!("{}{path}", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .header(header::ACCEPT_ENCODING, "gzip;q=0")
            .send()
            .await
            .unwrap();
        assert_eq!(refused.status(), StatusCode::OK, "{path} gzip;q=0");
        assert!(
            refused.headers().get(header::CONTENT_ENCODING).is_none(),
            "{path} must respect gzip;q=0"
        );
        let refused_bytes = refused.bytes().await.unwrap();
        assert!(serde_json::from_slice::<Value>(&refused_bytes).is_ok());
    }

    let health = srv
        .client
        .get(format!("{}/api/health", srv.base))
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), StatusCode::OK);
    assert!(health.headers().get(header::CONTENT_ENCODING).is_none());
    assert!(!vary_has_accept_encoding(health.headers()));

    let head = srv
        .client
        .head(format!("{}/api/scoreboard", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .header(header::ACCEPT_ENCODING, "gzip")
        .send()
        .await
        .unwrap();
    assert_eq!(head.status(), StatusCode::OK);
    assert!(head.headers().get(header::ETAG).is_some());
    assert!(head.headers().get(header::CONTENT_ENCODING).is_none());
    assert!(head.bytes().await.unwrap().is_empty());
}

#[tokio::test]
async fn cached_api_routes_return_etag_and_304_on_if_none_match() {
    let srv = start_fixture_server().await;
    let home_ctx = "%7B%22now%22%3A1795651200000%2C%22timeZone%22%3A%22UTC%22%2C%22platform%22%3A%22mac%22%2C%22intervalSeconds%22%3A60%2C%22username%22%3A%22fixture-admin%22%2C%22host%22%3A%22127.0.0.1%22%2C%22formatProbe%22%3A%5B%5D%7D";
    let routes: [(&str, Option<&str>); 6] = [
        ("/api/status", None),
        ("/api/scoreboard", None),
        ("/api/accounts/analytics?platform=mac&range=7d&tz=UTC", None),
        ("/api/views/home?platform=mac&refresh=false", None),
        ("/api/views/home?platform=mac&refresh=false", Some(home_ctx)),
        ("/api/accounts/dashboard?platform=mac&refresh=false", None),
    ];

    for (path, home_context) in routes {
        let mut req1 = srv
            .client
            .get(format!("{}{path}", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .header(header::ACCEPT_ENCODING, "gzip");
        if let Some(ctx) = home_context {
            req1 = req1.header("x-aac-home-context", ctx);
        }
        let first = req1.send().await.unwrap();
        assert_eq!(first.status(), StatusCode::OK, "{path} first");
        let etag = first
            .headers()
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| panic!("{path} missing ETag"));
        assert!(etag.starts_with("W/\""), "{path} weak etag: {etag}");
        let _ = first.bytes().await.unwrap();

        let mut req2 = srv
            .client
            .get(format!("{}{path}", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .header(header::ACCEPT_ENCODING, "gzip")
            .header(header::IF_NONE_MATCH, &etag);
        if let Some(ctx) = home_context {
            req2 = req2.header("x-aac-home-context", ctx);
        }
        let second = req2.send().await.unwrap();
        assert_eq!(
            second.status(),
            StatusCode::NOT_MODIFIED,
            "{path} (ctx={}) expected 304 for matching ETag {etag}",
            home_context.is_some()
        );
        assert_eq!(
            second
                .headers()
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok()),
            Some(etag.as_str()),
            "{path} 304 must preserve ETag"
        );
        assert!(
            second.bytes().await.unwrap().is_empty(),
            "{path} 304 body must be empty"
        );

        let mut req3 = srv
            .client
            .get(format!("{}{path}", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .header(header::ACCEPT_ENCODING, "gzip")
            .header(header::IF_NONE_MATCH, "W/\"definitely-stale-etag\"");
        if let Some(ctx) = home_context {
            req3 = req3.header("x-aac-home-context", ctx);
        }
        let third = req3.send().await.unwrap();
        assert_eq!(third.status(), StatusCode::OK, "{path} stale etag -> 200");
        assert!(!third.bytes().await.unwrap().is_empty());
    }

    let analytics_path = "/api/accounts/analytics?platform=mac&range=7d&tz=UTC";
    let first = srv
        .client
        .get(format!("{}{analytics_path}", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .send()
        .await
        .unwrap();
    let etag = first
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let forced = srv
        .client
        .get(format!("{}{analytics_path}&refresh=true", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .header(header::IF_NONE_MATCH, &etag)
        .send()
        .await
        .unwrap();
    assert_eq!(
        forced.status(),
        StatusCode::OK,
        "refresh=true must bypass 304"
    );
}
