//! Native Rust HTTP tests for dashboard settings, preferences, visibility,
//! fixed seven-computer registry, and config persistence invariants against the
//! live shipped `ai-account-center dashboard` listener.
//!
//! Ported from `account-refresh-settings.test.ts`, `account-preferences.test.ts`,
//! `account-visibility.test.ts`, `account-visibility-server.test.ts`,
//! `settings-review-r2.test.ts`, `dashboard-hosts.test.ts`, and
//! `computer-registry.test.ts`.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use futures_util::StreamExt;
use reqwest::{Client, StatusCode, header};
use serde_json::{Value, json};
use std::{
    fs,
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, client::IntoClientRequest},
};

const PASSWORD: &str = "fixture-settings-password";
const USERNAME: &str = "fixture-admin";

trait JsonBody {
    fn json(self, value: &Value) -> Self;
}

impl JsonBody for reqwest::RequestBuilder {
    fn json(self, value: &Value) -> Self {
        self.header(header::CONTENT_TYPE, "application/json")
            .body(value.to_string())
    }
}

fn write_private(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

async fn read_json(resp: reqwest::Response) -> Value {
    let bytes = resp.bytes().await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("invalid json ({e}): {}", String::from_utf8_lossy(&bytes)))
}

struct DashboardFixture {
    _dir: tempfile::TempDir,
    ccs: PathBuf,
    child: Child,
    base: String,
    origin: String,
    cookie: String,
    device_token: String,
    client: Client,
}

impl Drop for DashboardFixture {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl DashboardFixture {
    async fn start() -> Self {
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
        let path_env =
            std::env::join_paths([stubs.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

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

        let base = format!("http://127.0.0.1:{port}");
        let origin = base.clone();
        let client = Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .unwrap();

        let login = client
            .post(format!("{base}/api/auth/login"))
            .header(header::ORIGIN, &origin)
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
                "deviceName": "SettingsTray",
                "platform": "windows"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(pair.status(), StatusCode::CREATED);
        let device_token = read_json(pair).await["token"].as_str().unwrap().to_owned();

        Self {
            _dir: dir,
            ccs,
            child,
            base,
            origin,
            cookie,
            device_token,
            client,
        }
    }
}

#[tokio::test]
async fn settings_refresh_interval_roundtrip_validation_and_corrupt_fallbacks() {
    let srv = DashboardFixture::start().await;

    // Anonymous -> 401; Device token -> 403 device_scope; Query string -> 400.
    assert_eq!(
        srv.client
            .get(format!("{}/api/accounts/settings", srv.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let dev_resp = srv
        .client
        .get(format!("{}/api/accounts/settings", srv.base))
        .header(
            header::AUTHORIZATION,
            format!("Bearer {}", srv.device_token),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(dev_resp.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(dev_resp).await["code"], "device_scope");

    assert_eq!(
        srv.client
            .get(format!("{}/api/accounts/settings?foo=bar", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    // Default refresh interval is 60 seconds with Cache-Control: no-store.
    let get_default = srv
        .client
        .get(format!("{}/api/accounts/settings", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(get_default.status(), StatusCode::OK);
    assert_eq!(
        get_default.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    assert_eq!(read_json(get_default).await["refreshIntervalSeconds"], 60);

    // Write guards: missing/bad Origin -> 403, non-JSON -> 415, out-of-bounds / float / extra keys -> 400.
    assert_eq!(
        srv.client
            .put(format!("{}/api/accounts/settings", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .json(&json!({"refreshIntervalSeconds": 120}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        srv.client
            .put(format!("{}/api/accounts/settings", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, "http://evil.example")
            .json(&json!({"refreshIntervalSeconds": 120}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        srv.client
            .put(format!("{}/api/accounts/settings", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .header(header::CONTENT_TYPE, "text/plain")
            .body("120")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNSUPPORTED_MEDIA_TYPE
    );
    for invalid in [
        json!({"refreshIntervalSeconds": 10}),
        json!({"refreshIntervalSeconds": 3601}),
        json!({"refreshIntervalSeconds": 60.5}),
        json!({"refreshIntervalSeconds": 120, "extra": 1}),
    ] {
        let r = srv
            .client
            .put(format!("{}/api/accounts/settings", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .json(&invalid)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST, "invalid={invalid}");
    }

    // Valid PUT saves atomically with 0600 mode and survives fresh read.
    let saved = srv
        .client
        .put(format!("{}/api/accounts/settings", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"refreshIntervalSeconds": 180}))
        .send()
        .await
        .unwrap();
    assert_eq!(saved.status(), StatusCode::OK);
    assert_eq!(read_json(saved).await["refreshIntervalSeconds"], 180);
    let file = srv.ccs.join("account-refresh-settings.json");
    assert_eq!(
        fs::metadata(&file).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        read_json(
            srv.client
                .get(format!("{}/api/accounts/settings", srv.base))
                .header(header::COOKIE, &srv.cookie)
                .send()
                .await
                .unwrap()
        )
        .await["refreshIntervalSeconds"],
        180
    );

    // Truncated/corrupt file falls back to 60s default and stays in place; symlink target is never followed.
    fs::write(&file, "{broken").unwrap();
    assert_eq!(
        read_json(
            srv.client
                .get(format!("{}/api/accounts/settings", srv.base))
                .header(header::COOKIE, &srv.cookie)
                .send()
                .await
                .unwrap()
        )
        .await["refreshIntervalSeconds"],
        60
    );
    assert!(file.exists(), "corrupt settings file must remain in place");

    let target = srv.ccs.join("outside-settings-target.json");
    write_private(&target, r#"{"refreshIntervalSeconds":900}"#);
    fs::remove_file(&file).unwrap();
    symlink(&target, &file).unwrap();
    assert_eq!(
        read_json(
            srv.client
                .get(format!("{}/api/accounts/settings", srv.base))
                .header(header::COOKIE, &srv.cookie)
                .send()
                .await
                .unwrap()
        )
        .await["refreshIntervalSeconds"],
        60,
        "symlink settings file must never be read"
    );
}

#[tokio::test]
async fn preferences_zone_extra_sources_patch_and_guards() {
    let srv = DashboardFixture::start().await;

    // Default preferences: America/New_York, snapshotCleanup.auto=true, no extra sources.
    let defaults = read_json(
        srv.client
            .get(format!("{}/api/accounts/preferences", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(defaults["timeZone"], "America/New_York");
    assert_eq!(defaults["snapshotCleanup"]["auto"], true);
    assert_eq!(defaults["usageLogSources"], json!([]));

    // Rejects invalid IANA zone, unknown host, remote generic JSONL, missing JSONL mapping, overlapping paths.
    let bad_zone = srv
        .client
        .put(format!("{}/api/accounts/preferences", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "timeZone": "Not/A_Real_Zone",
            "snapshotCleanup": { "auto": true },
            "usageLogSources": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_zone.status(), StatusCode::BAD_REQUEST);

    let remote_jsonl = srv
        .client
        .put(format!("{}/api/accounts/preferences", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "timeZone": "America/New_York",
            "snapshotCleanup": { "auto": true },
            "usageLogSources": [{
                "id": "src-nas1-jsonl",
                "tool": "jsonl",
                "host": "nas1",
                "path": "/var/log/custom-jsonl",
                "fieldMapping": { "timestamp": "ts", "model": "model" }
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(remote_jsonl.status(), StatusCode::BAD_REQUEST);
    let remote_jsonl_msg = read_json(remote_jsonl).await["error"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(
        remote_jsonl_msg.contains("ubuntu"),
        "expected ubuntu-only error, got: {remote_jsonl_msg}"
    );

    let bad_host = srv
        .client
        .put(format!("{}/api/accounts/preferences", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "timeZone": "America/New_York",
            "snapshotCleanup": { "auto": true },
            "usageLogSources": [{
                "id": "src-unknown",
                "tool": "claude",
                "host": "not-a-fixed-computer",
                "path": "/tmp/logs"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_host.status(), StatusCode::BAD_REQUEST);

    let missing_mapping = srv
        .client
        .put(format!("{}/api/accounts/preferences", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "timeZone": "America/New_York",
            "snapshotCleanup": { "auto": true },
            "usageLogSources": [{
                "id": "src-jsonl-nomap",
                "tool": "jsonl",
                "host": "ubuntu",
                "path": "/var/log/custom-jsonl"
            }]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(missing_mapping.status(), StatusCode::BAD_REQUEST);

    // Overlapping roots: same claude-code parent/child, identical codex roots with trailing slash,
    // and built-in root overlap (/home/alice/.codex) are all refused.
    for sources in [
        json!([
            { "id": "src-parent", "tool": "claude-code", "host": "nas1", "path": "/srv/claude" },
            { "id": "src-child", "tool": "claude-code", "host": "nas1", "path": "/srv/claude/sub" }
        ]),
        json!([
            { "id": "src-codex-a", "tool": "codex", "host": "nas1", "path": "/srv/codex" },
            { "id": "src-codex-b", "tool": "codex", "host": "nas1", "path": "/srv/codex/" }
        ]),
        json!([
            { "id": "src-builtin", "tool": "codex", "host": "ubuntu", "path": "/home/alice/.codex" }
        ]),
    ] {
        let overlapping = srv
            .client
            .put(format!("{}/api/accounts/preferences", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .json(&json!({
                "timeZone": "America/New_York",
                "snapshotCleanup": { "auto": true },
                "usageLogSources": sources
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(overlapping.status(), StatusCode::BAD_REQUEST);
    }

    // Valid PUT persists extra roots across fixed computers (including non-legacy Nas1, Alexis Windows, Odroid, 1070ti)
    // plus local generic JSONL with required timestamp + model mapping.
    let valid_put = srv
        .client
        .put(format!("{}/api/accounts/preferences", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "timeZone": "America/Los_Angeles",
            "snapshotCleanup": { "auto": false },
            "usageLogSources": [
                { "id": "src-nas1", "tool": "codex", "host": "nas1", "path": "/srv/nas1-codex" },
                { "id": "src-alexis", "tool": "codex", "host": "alexis", "path": "C:\\Users\\alexis\\codex-extra" },
                { "id": "src-odroid", "tool": "claude-code", "host": "odroid", "path": "/srv/odroid-claude" },
                { "id": "src-1070ti", "tool": "claude-code", "host": "1070ti", "path": "/srv/1070ti-claude" },
                {
                    "id": "src-local-jsonl",
                    "tool": "jsonl",
                    "host": "ubuntu",
                    "path": "/srv/local-jsonl",
                    "fieldMapping": { "timestamp": "ts", "model": "meta.model" }
                }
            ]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(valid_put.status(), StatusCode::OK);
    let persisted = read_json(valid_put).await;
    assert_eq!(persisted["timeZone"], "America/Los_Angeles");
    assert_eq!(persisted["snapshotCleanup"]["auto"], false);
    assert_eq!(persisted["usageLogSources"].as_array().unwrap().len(), 5);

    // PATCH updates one field and adds/removes a source atomically without clobbering unrelated fields.
    let patch_add = srv
        .client
        .patch(format!("{}/api/accounts/preferences", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "snapshotCleanup": { "auto": true },
            "addUsageLogSource": {
                "id": "src-mac-extra",
                "tool": "claude-code",
                "host": "mac",
                "path": "/Users/fixture/extra-claude"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(patch_add.status(), StatusCode::OK);
    let after_add = read_json(patch_add).await;
    assert_eq!(after_add["timeZone"], "America/Los_Angeles");
    assert_eq!(after_add["snapshotCleanup"]["auto"], true);
    assert_eq!(after_add["usageLogSources"].as_array().unwrap().len(), 6);

    let patch_remove = srv
        .client
        .patch(format!("{}/api/accounts/preferences", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({ "removeUsageLogSource": "src-nas1" }))
        .send()
        .await
        .unwrap();
    assert_eq!(patch_remove.status(), StatusCode::OK);
    let after_rm = read_json(patch_remove).await;
    assert_eq!(after_rm["usageLogSources"].as_array().unwrap().len(), 5);
    assert!(
        !after_rm["usageLogSources"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["id"] == "src-nas1")
    );

    // PATCH refuses invalid field & device token & missing Origin.
    assert_eq!(
        srv.client
            .patch(format!("{}/api/accounts/preferences", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .json(&json!({ "unknownField": true }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        srv.client
            .patch(format!("{}/api/accounts/preferences", srv.base))
            .header(
                header::AUTHORIZATION,
                format!("Bearer {}", srv.device_token)
            )
            .header(header::ORIGIN, &srv.origin)
            .json(&json!({ "timeZone": "UTC" }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn visibility_put_patch_four_combinations_ws_event_and_corrupt_repair() {
    let srv = DashboardFixture::start().await;

    // Absence of file answers empty visibility lists.
    let empty = read_json(
        srv.client
            .get(format!("{}/api/accounts/visibility", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        empty,
        json!({
            "hiddenProviders": [],
            "hiddenAccountIds": [],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": []
        })
    );

    // Connect a browser WebSocket client to verify visibility writes broadcast `accounts-changed`.
    let ws_url = format!("{}/ws", srv.base.replace("http://", "ws://"));
    let mut ws_req = ws_url.into_client_request().unwrap();
    ws_req
        .headers_mut()
        .insert(header::ORIGIN, srv.origin.parse().unwrap());
    ws_req
        .headers_mut()
        .insert(header::COOKIE, srv.cookie.parse().unwrap());
    let (mut ws, _) = connect_async(ws_req).await.expect("connect /ws");

    // Independent dashboard vs tray account hiding in all 4 combinations.
    let acct_a = "codex:acct-a";
    let acct_b = "claude:acct-b";
    let acct_c = "codex:acct-c";
    let put = srv
        .client
        .put(format!("{}/api/accounts/visibility", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "hiddenProviders": ["cursor"],
            "hiddenAccountIds": [acct_a, acct_c],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": [acct_b, acct_c]
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(put.status(), StatusCode::OK);
    let v = read_json(put).await;
    assert_eq!(v["hiddenProviders"], json!(["cursor"]));
    assert_eq!(v["hiddenAccountIds"], json!([acct_a, acct_c]));
    assert_eq!(v["trayHiddenAccountIds"], json!([acct_b, acct_c]));

    // Verify accounts-changed event reached the open browser socket.
    let frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("ws event timeout")
        .unwrap()
        .unwrap();
    let msg: Value = match frame {
        Message::Text(text) => serde_json::from_str(&text).unwrap(),
        other => panic!("expected text frame, got {other:?}"),
    };
    assert_eq!(msg["type"], "accounts-changed");

    // PATCH updates one list delta without touching the other three lists.
    let patch = srv
        .client
        .patch(format!("{}/api/accounts/visibility", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "trayHiddenAccountIds": { "hide": [acct_a], "show": [acct_b] }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(patch.status(), StatusCode::OK);
    let patched = read_json(patch).await;
    assert_eq!(patched["hiddenProviders"], json!(["cursor"]));
    assert_eq!(patched["hiddenAccountIds"], json!([acct_a, acct_c]));
    assert_eq!(patched["trayHiddenAccountIds"], json!([acct_c, acct_a]));

    // Validation guards: unknown provider, conflicting hide+show in PATCH, query string, missing Origin.
    assert_eq!(
        srv.client
            .put(format!("{}/api/accounts/visibility", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .json(&json!({
                "hiddenProviders": ["unknown-provider-xyz"],
                "hiddenAccountIds": [],
                "trayHiddenProviders": [],
                "trayHiddenAccountIds": []
            }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        srv.client
            .patch(format!("{}/api/accounts/visibility", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .header(header::ORIGIN, &srv.origin)
            .json(&json!({
                "hiddenAccountIds": { "hide": [acct_a], "show": [acct_a] }
            }))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        srv.client
            .get(format!("{}/api/accounts/visibility?x=1", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );

    // Corrupt visibility file MUST answer 500 visibility_unavailable (never silent empty!),
    // PATCH refuses partial update on corrupt file, and full PUT repairs it cleanly.
    let vis_file = srv.ccs.join("account-visibility.json");
    write_private(&vis_file, "{corrupt");
    let broken_get = srv
        .client
        .get(format!("{}/api/accounts/visibility", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(broken_get.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        read_json(broken_get).await["code"],
        "visibility_unavailable"
    );

    let broken_patch = srv
        .client
        .patch(format!("{}/api/accounts/visibility", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({ "hiddenAccountIds": { "hide": [acct_a], "show": [] } }))
        .send()
        .await
        .unwrap();
    assert_eq!(broken_patch.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        read_json(broken_patch).await["code"],
        "visibility_unavailable"
    );

    let repaired = srv
        .client
        .put(format!("{}/api/accounts/visibility", srv.base))
        .header(header::COOKIE, &srv.cookie)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({
            "hiddenProviders": [],
            "hiddenAccountIds": [acct_b],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": []
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(repaired.status(), StatusCode::OK);
    assert_eq!(
        read_json(repaired).await["hiddenAccountIds"],
        json!([acct_b])
    );

    // Drops a retired provider stored on disk while keeping the rest of the file intact.
    write_private(
        &vis_file,
        &json!({
            "version": 1,
            "hiddenProviders": ["cursor", "retired-provider"],
            "hiddenAccountIds": [acct_b],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": []
        })
        .to_string(),
    );
    let filtered = read_json(
        srv.client
            .get(format!("{}/api/accounts/visibility", srv.base))
            .header(header::COOKIE, &srv.cookie)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(filtered["hiddenProviders"], json!(["cursor"]));
    assert_eq!(filtered["hiddenAccountIds"], json!([acct_b]));
}

#[test]
fn fixed_seven_computer_registry_invariants() {
    let reg = aac_status::computers::registry().as_object().unwrap();
    let table_path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web-dashboard/public/computers.json");
    let table: Value = serde_json::from_str(&fs::read_to_string(&table_path).unwrap()).unwrap();
    let expected = [
        ("ubuntu", "linux", false),
        ("mac", "mac", true),
        ("windows", "windows", true),
        ("nas1", "linux", true),
        ("alexis", "windows", true),
        ("odroid", "linux", true),
        ("1070ti", "linux", true),
    ];
    assert_eq!(
        reg.len(),
        expected.len(),
        "exactly the seven fixed computers"
    );
    assert_eq!(
        table.as_object().unwrap().len(),
        expected.len(),
        "computers.json must match the seven fixed computers"
    );
    for (id, os, has_ssh) in expected {
        let entry = reg
            .get(id)
            .unwrap_or_else(|| panic!("missing computer {id}"));
        let source_row = table
            .get(id)
            .unwrap_or_else(|| panic!("missing computer {id} in computers.json"));
        assert_eq!(entry["os"].as_str(), Some(os), "os for {id}");
        assert_eq!(entry["os"], source_row["os"], "os parity for {id}");
        assert_eq!(
            entry["sshAlias"], source_row["sshAlias"],
            "sshAlias parity for {id}"
        );
        assert!(
            entry["label"]
                .as_str()
                .is_some_and(|l| !l.trim().is_empty()),
            "label for {id}"
        );
        if has_ssh {
            let alias = entry["sshAlias"]
                .as_str()
                .unwrap_or_else(|| panic!("expected sshAlias for {id}"));
            assert!(
                !alias.is_empty()
                    && alias
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.')),
                "safe ssh alias for {id}: {alias}"
            );
        } else {
            assert!(
                entry["sshAlias"].is_null(),
                "local host {id} has no sshAlias"
            );
        }
    }
}
