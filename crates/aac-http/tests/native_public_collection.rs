//! Exercise the shipped public boot without a compatibility worker or provider.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use futures_util::StreamExt;
use reqwest::{Client, Response, StatusCode, header};
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

use tokio_tungstenite::{connect_async, tungstenite::client::IntoClientRequest};

fn file(root: &Path, name: &str, body: Value) {
    let file = root.join(name);
    fs::write(&file, body.to_string()).unwrap();
    fs::set_permissions(file, fs::Permissions::from_mode(0o600)).unwrap();
}

async fn json_reply(response: Response, status: StatusCode, no_store: bool) -> Value {
    assert_eq!(response.status(), status);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/json; charset=utf-8"
    );
    assert_eq!(
        response
            .headers()
            .get(header::CACHE_CONTROL)
            .and_then(|value| value.to_str().ok()),
        no_store.then_some("no-store")
    );
    serde_json::from_slice(&response.bytes().await.unwrap()).unwrap()
}

#[tokio::test]
async fn the_public_boot_serves_accounts_recheck_and_app_jobs_without_the_worker() {
    public_transport_fixture(false, false).await;
}

#[tokio::test]
async fn the_lifecycle_session_probe_reaches_the_pinned_agent() {
    public_transport_fixture(true, false).await;
}

#[tokio::test]
async fn public_usage_reads_populated_history_on_both_hosts() {
    public_transport_fixture(false, true).await;
}

async fn public_transport_fixture(agent_on: bool, history_only: bool) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let password = "fixture-public-password";
    let windows_profile = if history_only {
        "C:/Users/fixture/AppData/Roaming/Claude-fixture/"
    } else {
        r"C:\Users\fixture\AppData\Roaming\Claude-fixture"
    };
    file(
        &root,
        "config.yaml",
        json!({
            "dashboard_auth": {"enabled":true, "username":"fixture-admin",
                "password_hash":bcrypt::hash(password, 4).unwrap()},
            "agentTransportSsh":{"identityFile":root.join("fixture-key"),
                "knownHostsFile":root.join("fixture-known-hosts")}
        }),
    );
    file(
        &root,
        "account-usage-accounts.json",
        json!({"version":2,"accounts":[
            {"id":"cursor:usage","provider":"cursor","platform":"ubuntu","label":"Fixture",
             "credential":{"kind":"discover"}}
        ]}),
    );
    file(
        &root,
        "claude-desktop-profiles.json",
        json!({"version":1,"profiles":[{
            "id":"fixture-mac","email":"fixture@example.invalid",
            "mac":{"launcherName":"Claude Fixture","launcherPath":"/Applications/Claude Fixture.app",
                "profilePath":"/Users/fixture/Library/Application Support/Claude-fixture",
                "sshHost":"jared-mac"}
        },{
            "id":"fixture-win","email":"windows@example.invalid",
            "windows":{"launcherName":"Claude Fixture","profilePath":windows_profile,
                "sshHost":"jared-windows","isDefault":false}
        }]}),
    );
    fs::write(root.join("fixture-key"), "test identity").unwrap();
    fs::write(root.join("fixture-known-hosts"), "test host pin").unwrap();
    fs::set_permissions(root.join("fixture-key"), fs::Permissions::from_mode(0o600)).unwrap();
    let run = root.join("run");
    fs::create_dir(&run).unwrap();
    fs::set_permissions(&run, fs::Permissions::from_mode(0o700)).unwrap();
    let ssh = root.join("ssh");
    fs::write(
        &ssh,
        r#"#!/usr/bin/python3
import base64
import json
from pathlib import Path
import re
import shlex
import sys

root = Path(__file__).parent
args = sys.argv[1:]
if args[0] == "-G":
    sys.exit(0)
assert args[-3] == "--"
assert "StrictHostKeyChecking=yes" in args
host, command = args[-2:]
assert host in {"jared-mac", "jared-windows", "nas1-agent", "alexis-windows"}
if command.startswith("powershell.exe"):
    assert len(command) < 8191
    outer = base64.b64decode(command.rsplit(" ", 1)[1]).decode("utf-16-le")
    command = base64.b64decode(re.search(r"'-EncodedCommand' '([A-Za-z0-9+/=]+)'", outer)[1]).decode("utf-16-le")
    if "$encoded = [Console]::In.ReadToEnd()" in command:
        command = base64.b64decode(sys.stdin.buffer.read(), validate=True).decode("utf-16-le")
agent_program = shlex.split(command)[-1] if host == "jared-mac" else ""
if "case 'probe'" in agent_program:
    assert host == "jared-mac"
    print(json.dumps({"name":"aac-agent","version":"0.1.0","protocol":1,"target":"aarch64-apple-darwin"}))
elif "case 'run'" in agent_program:
    request = json.loads(sys.stdin.buffer.readline())
    if request["verb"] == "claude-host":
        assert host == "jared-mac"
        if request["args"]["op"] == "open":
            (root / "agent-open").write_text(host)
            sys.exit(1)
        assert request["args"]["op"] == "session"
        assert request["args"]["launcher"]["sshHost"] == host
        receipt = "pending-session" if request["args"]["launcher"]["profilePath"].endswith("Claude-pending") else "agent-session"
        (root / receipt).write_text(host)
        value = {"signedIn":False}
    else:
        value = {"fallback":"python"}
    print(json.dumps({"type":"result","protocol":1,"id":request["id"],"value":value}))
elif command.startswith('/usr/bin/python3 -c '):
    assert host == "jared-mac"
    envelope = json.load(sys.stdin)
    assert envelope["request"] == {
        "op": "session",
        "profilePath": "/Users/fixture/Library/Application Support/Claude-fixture",
        "launcherName": "Claude Fixture",
    }
    (root / "host-session").write_text(host)
    print('{"signedIn":false}')
elif "aac-desktop-history" in command or "[IO.File]::ReadAllText($history)" in command:
    if host == "jared-mac":
        path = shlex.split(command)[-1]
        expected = "/Users/fixture/Library/Application Support/Claude-fixture/plan-usage-history.json"
    else:
        if "$history = '" in command:
            path = command.split("$history = '", 1)[1].split("'", 1)[0]
        else:
            path = base64.b64decode(sys.stdin.buffer.read(), validate=True).decode()
        expected = r"C:\Users\fixture\AppData\Roaming\Claude-fixture\plan-usage-history.json"
    (root / (host + "-history")).write_text(path)
    # The fake models a populated file at the exact path, and no file at the directory.
    if path == expected:
        print(json.dumps({"version":2,"samples":[{"t":1795123200000,"u":{"fh":25 if host == "jared-mac" else 42}}]}))
    else:
        print("CCS_DESKTOP_USAGE_MISSING")
else:
    if host == "jared-mac":
        argv = shlex.split(command)
        assert argv[:2] == ["/bin/sh", "-c"]
        assert "/Applications/Claude Fixture.app" in argv[2]
    else:
        assert host == "jared-windows"
        assert "Start-ScheduledTask" in command and "fixture-win" in command
    (root / (host + "-open")).write_text(host)
"#,
    )
    .unwrap();
    fs::set_permissions(&ssh, fs::Permissions::from_mode(0o700)).unwrap();
    if agent_on {
        let accounts = root.join("accounts");
        fs::create_dir(&accounts).unwrap();
        fs::set_permissions(&accounts, fs::Permissions::from_mode(0o700)).unwrap();
        file(
            &accounts,
            "claude-pending.json",
            json!({"version":1,"profiles":[{
                "id":"fixture-pending","label":"Pending fixture","createdAt":"2026-11-19T20:00:00.000Z",
                "mac":{"launcherName":"Claude Pending","launcherPath":"/Applications/Claude Pending.app",
                    "profilePath":"/Users/fixture/Library/Application Support/Claude-pending",
                    "sshHost":"jared-mac"},
                "windows":{"launcherName":"Claude Pending","profilePath":"C:\\Users\\fixture\\AppData\\Roaming\\Claude-pending",
                    "sshHost":"jared-windows"}
            }]}),
        );
        file(
            &root,
            "agent-catalog.json",
            json!({"artifacts":[{
                "file":ssh,"target":"aarch64-apple-darwin","version":"0.1.0",
                "sha256":aac_core::sha256_hex(&fs::read(&ssh).unwrap())
            }]}),
        );
    }
    let package = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap();
    let path = std::env::join_paths([root.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let mut child = Command::new(env!("CARGO_BIN_EXE_ai-account-center"))
        .args(["dashboard", "--no-open", "--host", "127.0.0.1", "--port"])
        .arg(address.port().to_string())
        .env("PATH", path)
        .env("CCS_HOME", &root)
        .env("CCS_DIR", &root)
        .env("AAC_TS_SIDECAR", "off")
        .env("AAC_AGENT_TRANSPORT", if agent_on { "on" } else { "off" })
        .env("AAC_AGENT_PACKAGE_ROOT", package)
        .env("AAC_AGENT_CATALOG", root.join("agent-catalog.json"))
        .env("CCS_CLAUDE_HOST_LIFECYCLE", "on")
        .env("AAC_RUST_GROUP_COLLECTION", "on")
        .env("AAC_RUST_GROUP_WRITES", "on")
        .env("AAC_RUST_GROUP_APP_UPDATES", "on")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let base = format!("http://{address}");
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(5), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if line == format!("aac-http public listening on {address}") {
                return;
            }
        }
        panic!("public fixture exited before listening");
    })
    .await
    .unwrap();

    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let credentials = json!({"username":"fixture-admin", "password":password});
    let login = client
        .post(format!("{base}/api/auth/login"))
        .header(header::ORIGIN, &base)
        .header(header::CONTENT_TYPE, "application/json")
        .body(credentials.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    let dashboard = format!("{base}/api/accounts/dashboard?platform=windows");
    let get = || client.get(&dashboard).header(header::COOKIE, cookie);
    let current = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let response = get().send().await.unwrap();
            if response.status() != StatusCode::SERVICE_UNAVAILABLE {
                break response;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    let tag = current.headers()[header::ETAG].clone();
    let accounts = json_reply(current, StatusCode::OK, true).await;
    let row = accounts["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "cursor:usage")
        .unwrap()
        .clone();
    assert_eq!(row["status"], "unavailable");
    let mac = client
        .get(format!("{base}/api/accounts/dashboard?platform=mac"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    let mac = json_reply(mac, StatusCode::OK, true).await;
    let profile = mac["accounts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "claude:fixture-mac")
        .unwrap();
    assert_eq!(
        profile["signInNeeded"],
        json!(["mac"]),
        "agent-off Mac sign-in probe"
    );
    assert_eq!(profile["status"], "needs_sign_in");
    if agent_on {
        assert_eq!(
            fs::read_to_string(root.join("agent-session"))
                .ok()
                .as_deref(),
            Some("jared-mac"),
            "agent-on lifecycle session reached the pinned agent"
        );
        assert!(!root.join("host-session").exists());
        let recheck = client
            .post(format!(
                "{base}/api/accounts/claude:fixture-pending/recheck"
            ))
            .header(header::COOKIE, cookie)
            .header(header::ORIGIN, &base)
            .header(header::CONTENT_TYPE, "application/json")
            .body(r#"{"platform":"mac"}"#)
            .send()
            .await
            .unwrap();
        let state = json_reply(recheck, StatusCode::OK, true).await;
        assert_eq!(state["account"]["status"], "needs_sign_in");
        assert_eq!(
            fs::read_to_string(root.join("pending-session")).unwrap(),
            "jared-mac"
        );
        let uncertain = client
            .post(format!(
                "{base}/api/claude/desktop-profiles/fixture-mac/open"
            ))
            .header(header::COOKIE, cookie)
            .header(header::ORIGIN, &base)
            .header(header::CONTENT_TYPE, "application/json")
            .body(r#"{"platform":"mac"}"#)
            .send()
            .await
            .unwrap();
        assert_eq!(uncertain.status(), StatusCode::BAD_GATEWAY);
        assert_eq!(
            fs::read_to_string(root.join("agent-open")).unwrap(),
            "jared-mac"
        );
        assert!(
            !root.join("jared-mac-open").exists(),
            "indeterminate Open replayed via fixed SSH"
        );
        child.kill().await.unwrap();
        return;
    }

    assert_eq!(
        fs::read_to_string(root.join("host-session")).unwrap(),
        "jared-mac"
    );
    if history_only {
        for (platform, id, percent, host, expected_path) in [
            (
                "mac",
                "fixture-mac",
                25,
                "jared-mac",
                "/Users/fixture/Library/Application Support/Claude-fixture/plan-usage-history.json",
            ),
            (
                "windows",
                "fixture-win",
                42,
                "jared-windows",
                r"C:\Users\fixture\AppData\Roaming\Claude-fixture\plan-usage-history.json",
            ),
        ] {
            let response = client
                .get(format!(
                    "{base}/api/claude/desktop-profiles/usage?platform={platform}"
                ))
                .header(header::COOKIE, cookie)
                .send()
                .await
                .unwrap();
            let usage = json_reply(response, StatusCode::OK, false).await;
            let profile = usage["profiles"]
                .as_array()
                .unwrap()
                .iter()
                .find(|profile| profile["id"] == id)
                .unwrap();
            assert_eq!(profile["status"], "cached", "populated {platform} history");
            assert_eq!(profile["utilization"]["fiveHour"], percent);
            assert_eq!(
                fs::read_to_string(root.join(format!("{host}-history"))).unwrap(),
                expected_path
            );
        }
        child.kill().await.unwrap();
        return;
    }
    for (platform, profile_id, host) in [
        ("mac", "fixture-mac", "jared-mac"),
        ("windows", "fixture-win", "jared-windows"),
    ] {
        let response = client
            .post(format!(
                "{base}/api/claude/desktop-profiles/{profile_id}/open"
            ))
            .header(header::COOKIE, cookie)
            .header(header::ORIGIN, &base)
            .header(header::CONTENT_TYPE, "application/json")
            .body(json!({"platform":platform}).to_string())
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "agent-off {platform} Open"
        );
        assert_eq!(
            fs::read_to_string(root.join(format!("{host}-open"))).unwrap(),
            host
        );
        let response = client
            .get(format!(
                "{base}/api/claude/desktop-profiles/usage?platform={platform}"
            ))
            .header(header::COOKIE, cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(root.join(format!("{host}-history")).is_file());
    }
    let current = get()
        .header(header::IF_NONE_MATCH, tag.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(current.status(), StatusCode::NOT_MODIFIED);
    assert!(current.bytes().await.unwrap().is_empty());
    let reload = get()
        .header(header::IF_NONE_MATCH, tag)
        .header(header::CACHE_CONTROL, "no-cache")
        .send()
        .await
        .unwrap();
    assert_eq!(reload.status(), StatusCode::OK);

    let invalid = client
        .get(format!("{base}/api/accounts/dashboard?fixture=invalid"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_reply(invalid, StatusCode::BAD_REQUEST, false).await,
        json!({"error":"Select a valid desktop platform and refresh value."})
    );
    let recheck_path = |id: &str| format!("{base}/api/accounts/{id}/recheck");
    let post = |url: String| {
        client
            .post(url)
            .header(header::CONTENT_TYPE, "application/json")
            .body("{}")
    };
    let signed = |url: String| post(url).header(header::COOKIE, cookie);
    let origin = |url: String| signed(url).header(header::ORIGIN, &base);
    let url = recheck_path("cursor:usage");
    let signed_out = post(url.clone()).send().await.unwrap();
    assert_eq!(
        json_reply(signed_out, StatusCode::UNAUTHORIZED, true).await["code"],
        "auth_required"
    );
    let no_origin = signed(url.clone()).send().await.unwrap();
    assert_eq!(
        json_reply(no_origin, StatusCode::FORBIDDEN, true).await["code"],
        "origin_required"
    );
    let wrong_origin = signed(url.clone())
        .header(header::ORIGIN, "https://wrong.invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_reply(wrong_origin, StatusCode::FORBIDDEN, true).await["code"],
        "origin_required"
    );
    let wrong_body = origin(url.clone())
        .body(r#"{"refresh":true}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_reply(wrong_body, StatusCode::BAD_REQUEST, true).await["code"],
        "invalid_body"
    );
    let unknown = origin(recheck_path("zai:acct:00000000"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_reply(unknown, StatusCode::NOT_FOUND, true).await,
        json!({"error":"That account was not found.","code":"unknown_account"})
    );

    let refreshed = origin(url.clone()).send().await.unwrap();
    let mut expected = row;
    let object = expected.as_object_mut().unwrap();
    object.shift_remove("hidden");
    object.shift_remove("trayHidden");
    assert_eq!(
        json_reply(refreshed, StatusCode::OK, true).await,
        json!({"account": expected})
    );
    let limited = origin(url).send().await.unwrap();
    assert_eq!(limited.headers()[header::RETRY_AFTER], "10");
    assert_eq!(
        json_reply(limited, StatusCode::TOO_MANY_REQUESTS, true).await,
        json!({
            "error":"This account was checked a moment ago. Try again shortly.",
            "code":"rate_limited", "retryAfterSeconds":10,
        })
    );

    let status = client
        .get(format!("{base}/api/app-updates/status"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_reply(status, StatusCode::OK, true).await,
        json!({"job":null})
    );
    let cancel = origin(format!("{base}/api/app-updates/cancel"))
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_reply(cancel, StatusCode::OK, true).await,
        json!({"job":null,"cancelling":false})
    );

    // The transport is disabled in this fixture. Start still owns the durable
    // acknowledgement and publishes progress through the public event bus.
    let mut request = format!("ws://{address}/ws").into_client_request().unwrap();
    request
        .headers_mut()
        .insert(header::ORIGIN, base.parse().unwrap());
    request
        .headers_mut()
        .insert(header::COOKIE, cookie.parse().unwrap());
    let (mut websocket, _) = connect_async(request).await.unwrap();
    let start = origin(format!("{base}/api/app-updates/start"))
        .send()
        .await
        .unwrap();
    let job = json_reply(start, StatusCode::ACCEPTED, true).await;
    let id = job["job"]["id"].as_str().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        let mut running = false;
        loop {
            let event = websocket
                .next()
                .await
                .unwrap()
                .unwrap()
                .into_text()
                .unwrap();
            let event: Value = serde_json::from_str(&event).unwrap();
            if event["type"] != "app-update-job" || event["job"]["id"] != id {
                continue;
            }
            assert!(event["job"].get("results").is_none());
            if event["job"]["state"] != "running" {
                assert!(running);
                break;
            }
            running = true;
        }
    })
    .await
    .unwrap();
    drop(websocket);

    // Close only our own fixture process and require a clean stop.
    let pid = child.id().unwrap();
    assert!(
        Command::new("/bin/kill")
            .args(["-TERM", &pid.to_string()])
            .status()
            .await
            .unwrap()
            .success()
    );
    assert!(
        tokio::time::timeout(Duration::from_secs(5), child.wait())
            .await
            .unwrap()
            .unwrap()
            .success()
    );
}

#[tokio::test]
async fn collection_off_routes_only_accounts_and_recheck_to_the_worker() {
    for env_override in [false, true] {
        worker_serves_disabled_collection(env_override).await;
    }
}

async fn worker_serves_disabled_collection(env_override: bool) {
    let fixture = tempfile::tempdir().unwrap();
    let root = fixture.path().canonicalize().unwrap();
    let password = "fixture-public-password";
    file(
        &root,
        "config.yaml",
        json!({
            "dashboard_auth":{"enabled":true,"username":"fixture-admin",
                "password_hash":bcrypt::hash(password, 4).unwrap()},
            "rustService":{"groups":{"collection":if env_override {"on"} else {"off"}}}
        }),
    );
    let script = root.join("worker.js");
    fs::write(&script, r#"
        const http = require('node:http');
        const readline = require('node:readline');
        const input = readline.createInterface({input: process.stdin});
        input.once('line', proof => {
            const server = http.createServer((req, res) => {
                const context = JSON.parse(Buffer.from(req.headers['x-aac-compose-access'], 'base64url'));
                if (req.headers['x-aac-compose-proof'] !== proof ||
                    context.principal.kind !== 'session' ||
                    process.env.AAC_CUT_WORKER_COLLECTION !== '1') {
                    res.writeHead(403).end();
                    return;
                }
                res.writeHead(200, {'Content-Type': 'application/json'});
                res.end(JSON.stringify({from: 'worker', method: req.method, path: req.url}));
            }).listen(0, '127.0.0.1', () => {
                console.log(JSON.stringify({sidecar:1,port:server.address().port}));
            });
            input.once('close', () => server.close());
        });
    "#).unwrap();

    let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap();
    drop(socket);
    let mut command = Command::new(env!("CARGO_BIN_EXE_ai-account-center"));
    command
        .args(["dashboard", "--no-open", "--host", "127.0.0.1", "--port"])
        .arg(address.port().to_string())
        .env("CCS_HOME", &root)
        .env("CCS_DIR", &root)
        .env("AAC_TS_SIDECAR", &script)
        .env("AAC_AGENT_TRANSPORT", "off")
        .env("AAC_RUST_GROUP_HOME", "off")
        .env("AAC_RUST_GROUP_USAGESNAPSHOT", "off")
        .env("AAC_RUST_GROUP_ANALYTICS_WRITERS", "off")
        .env("AAC_RUST_CUT_NATIVE_ROUTES", "on")
        .env_remove("AAC_RUST_GROUP_COLLECTION")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .kill_on_drop(true);
    if env_override {
        command.env("AAC_RUST_GROUP_COLLECTION", "off");
    }
    let mut child = command.spawn().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap()).lines();
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(line) = output.next_line().await.unwrap() {
            if line == format!("aac-http public listening on {address}") {
                return;
            }
        }
        panic!("public fixture exited before listening");
    })
    .await
    .unwrap();

    assert!(
        aac_files::OwnedStore::claim_collection(&root).is_err(),
        "a native sampler must wait for the worker's collection lease"
    );
    let client = Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap();
    let base = format!("http://{address}");
    let login = client
        .post(format!("{base}/api/auth/login"))
        .header(header::ORIGIN, &base)
        .header(header::CONTENT_TYPE, "application/json")
        .body(json!({"username":"fixture-admin","password":password}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), StatusCode::OK);
    let cookie = login.headers()[header::SET_COOKIE]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap();
    for (method, path) in [
        (
            reqwest::Method::GET,
            "/api/accounts/dashboard?platform=windows",
        ),
        (reqwest::Method::HEAD, "/api/accounts/dashboard"),
        (reqwest::Method::POST, "/api/accounts/fixture:usage/recheck"),
    ] {
        let response = client
            .request(method.clone(), format!("{base}{path}"))
            .header(header::COOKIE, cookie)
            .header(header::ORIGIN, &base)
            .header(header::CONTENT_TYPE, "application/json")
            .body(if method == reqwest::Method::POST {
                "{}"
            } else {
                ""
            })
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK, "{method} {path}");
        if method == reqwest::Method::HEAD {
            assert!(response.bytes().await.unwrap().is_empty());
        } else {
            let body: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
            assert_eq!(
                body,
                json!({"from":"worker","method":method.as_str(),"path":path})
            );
        }
    }
    let settings = client
        .get(format!("{base}/api/accounts/settings"))
        .header(header::COOKIE, cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        json_reply(settings, StatusCode::OK, true).await,
        json!({"refreshIntervalSeconds":60})
    );
    child.kill().await.unwrap();
    assert!(aac_files::OwnedStore::claim_collection(&root).is_ok());
}
