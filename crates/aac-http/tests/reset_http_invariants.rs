//! Native Rust HTTP tests for reset-claim invariants, approval/confirmation
//! boundaries, persisted holds/pending recovery, live offline Codex credit claim
//! & refusal behavior, and reset URL / request-ID log scrubbing.
//!
//! Ported from `reset-claims.test.ts`, `reset-any-usage-native.test.ts`,
//! `reset-refusal-native.test.ts`, `reset-claim-parity.ts`,
//! `account-confirmations.test.ts`, and `request-context-middleware.test.ts`.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use reqwest::{Client, Method, StatusCode, header};
use serde_json::{Value, json};
use std::{
    fs,
    net::{Ipv4Addr, TcpListener},
    os::unix::fs::PermissionsExt,
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

const USERNAME: &str = "fixture-admin";
const PASSWORD: &str = "fixture-reset-password";

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

fn fake_codex_jwt(account_id: &str) -> String {
    let hdr = URL_SAFE_NO_PAD.encode(br#"{"alg":"none","typ":"JWT"}"#);
    let payload = URL_SAFE_NO_PAD.encode(
        json!({
            "email": format!("fixture-{account_id}@example.invalid"),
            "https://api.openai.com/auth": {
                "chatgpt_account_id": account_id
            }
        })
        .to_string()
        .as_bytes(),
    );
    format!("{hdr}.{payload}.sig")
}

async fn read_json(resp: reqwest::Response) -> Value {
    let bytes = resp.bytes().await.unwrap();
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("invalid json ({e}): {}", String::from_utf8_lossy(&bytes)))
}

struct ResetServer {
    _dir: tempfile::TempDir,
    ccs: PathBuf,
    child: Child,
    base: String,
    origin: String,
    cookie_a: String,
    cookie_b: String,
    device_token: String,
    client: Client,
}

impl Drop for ResetServer {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

impl ResetServer {
    async fn start(fake_codex_script: Option<&str>, setup_before_boot: impl FnOnce(&Path)) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = fs::canonicalize(dir.path()).unwrap();
        let ccs = root.join("home/.ccs");
        let bin_dir = root.join("bin");
        let xdg = root.join("xdg");
        for d in [&ccs, &bin_dir, &xdg] {
            fs::create_dir_all(d).unwrap();
        }
        fs::set_permissions(&ccs, fs::Permissions::from_mode(0o700)).unwrap();
        fs::set_permissions(&xdg, fs::Permissions::from_mode(0o700)).unwrap();

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

        if let Some(script) = fake_codex_script {
            let codex_bin = bin_dir.join("codex");
            fs::write(&codex_bin, script).unwrap();
            fs::set_permissions(&codex_bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        setup_before_boot(&ccs);

        let path_env =
            std::env::join_paths([bin_dir.clone(), "/usr/bin".into(), "/bin".into()]).unwrap();
        let probe = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = probe.local_addr().unwrap().port();
        drop(probe);

        let mut child = Command::new(env!("CARGO_BIN_EXE_aac-http"))
            .arg("--public")
            .env_clear()
            .env("PATH", path_env)
            .env("HOME", root.join("home"))
            .env("CCS_HOME", root.join("home"))
            .env("CCS_DIR", &ccs)
            .env("XDG_RUNTIME_DIR", &xdg)
            .env("AAC_RUST_PUBLIC_LISTEN", format!("127.0.0.1:{port}"))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();

        let stdout = child.stdout.take().unwrap();
        let mut lines = BufReader::new(stdout).lines();
        tokio::time::timeout(Duration::from_secs(15), async {
            while let Ok(Some(line)) = lines.next_line().await {
                if line.contains("public listening on") {
                    break;
                }
            }
        })
        .await
        .expect("aac-http --public started");
        tokio::spawn(async move { while let Ok(Some(_)) = lines.next_line().await {} });

        let base = format!("http://127.0.0.1:{port}");
        let origin = base.clone();
        let client = Client::builder()
            .timeout(Duration::from_secs(20))
            .build()
            .unwrap();

        let login = |client: Client, base: String, origin: String| async move {
            let resp = client
                .post(format!("{base}/api/auth/login"))
                .header(header::ORIGIN, &origin)
                .json(&json!({"username": USERNAME, "password": PASSWORD}))
                .send()
                .await
                .unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
            resp.headers()
                .get(header::SET_COOKIE)
                .unwrap()
                .to_str()
                .unwrap()
                .split(';')
                .next()
                .unwrap()
                .to_owned()
        };
        let cookie_a = login(client.clone(), base.clone(), origin.clone()).await;
        let cookie_b = login(client.clone(), base.clone(), origin.clone()).await;

        let pair = client
            .post(format!("{base}/api/auth/devices/pair"))
            .json(&json!({
                "username": USERNAME,
                "password": PASSWORD,
                "deviceName": "ResetTray",
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
            cookie_a,
            cookie_b,
            device_token,
            client,
        }
    }
}

fn seed_codex_profile(ccs: &Path, profile: &str, account_id: &str, state: Value) -> PathBuf {
    let instance_dir = ccs.join("codex-instances").join(profile);
    fs::create_dir_all(&instance_dir).unwrap();
    fs::set_permissions(&instance_dir, fs::Permissions::from_mode(0o700)).unwrap();
    write_private(
        &ccs.join("codex-profiles.yaml"),
        &serde_yaml::to_string(&json!({
            "profiles": {
                profile: {
                    "type": "codex",
                    "created": "2026-11-01T00:00:00.000Z",
                    "last_used": "2026-11-26T00:00:00.000Z",
                    "email": format!("{profile}@example.invalid"),
                    "plan_type": "pro",
                    "account_id": account_id
                }
            }
        }))
        .unwrap(),
    );
    write_private(
        &instance_dir.join("auth.json"),
        &json!({
            "tokens": {
                "id_token": fake_codex_jwt(account_id),
                "account_id": account_id
            }
        })
        .to_string(),
    );
    write_private(&instance_dir.join("fixture-state.json"), &state.to_string());
    instance_dir
}

const FAKE_CODEX_SCRIPT: &str = r#"#!/usr/bin/python3
import json, os, sys
home = os.environ['CODEX_HOME']
who = json.load(open(os.path.join(home, 'auth.json')))['tokens']['account_id']
for line in sys.stdin:
    req = json.loads(line)
    if 'id' not in req:
        continue
    method = req['method']
    if method == 'initialize':
        result = {}
    elif method == 'account/read':
        result = {'account': {'type': 'chatgpt', 'email': 'fixture-' + who + '@example.invalid'}}
    elif method == 'account/rateLimits/read':
        state = json.load(open(os.path.join(home, 'fixture-state.json')))
        with open(os.path.join(home, 'read-count'), 'a') as f:
            f.write('1\n')
        if state.get('failRead'):
            sys.exit(2)
        result = {
            'accountId': who,
            'rateLimits': {
                'primary': {'usedPercent': state['primary']},
                'secondary': {'usedPercent': state['secondary']}
            },
            'rateLimitResetCredits': {
                'availableCount': 0 if state.get('spent') else 1,
                'credits': [{
                    'id': 'credit-1',
                    'resetType': 'codexRateLimits',
                    'status': 'redeemed' if state.get('spent') else 'available'
                }]
            }
        }
    elif method == 'account/rateLimitResetCredit/consume':
        state = json.load(open(os.path.join(home, 'fixture-state.json')))
        with open(os.path.join(home, 'send-count'), 'a') as f:
            f.write('1\n')
        if state.get('sendOutcome') == 'refuse':
            result = {'outcome': 'rejected', 'reason': 'synthetic-provider-secret-text'}
        elif state.get('sendOutcome') == 'ambiguous':
            sys.exit(3)
        else:
            state['primary'] = 0
            state['secondary'] = 0
            state['spent'] = True
            with open(os.path.join(home, 'fixture-state.json'), 'w') as f:
                json.dump(state, f)
            result = {'outcome': 'reset'}
    else:
        result = {}
    print(json.dumps({'id': req['id'], 'result': result}), flush=True)
"#;

#[tokio::test]
async fn reset_routes_enforce_session_origin_json_query_and_body_before_io() {
    let srv = ResetServer::start(Some(FAKE_CODEX_SCRIPT), |ccs| {
        seed_codex_profile(
            ccs,
            "guardacct",
            "acct-guard-1",
            json!({"primary": 10, "secondary": 60, "spent": false}),
        );
    })
    .await;
    let instance_dir = srv.ccs.join("codex-instances/guardacct");
    let grant_key = "b".repeat(64);
    let token = "c".repeat(64);

    // Anonymous -> 401; paired device token -> 403 device_scope across all reset routes.
    for (method, suffix, body) in [
        (Method::GET, "", None),
        (
            Method::POST,
            "/prepare",
            Some(json!({"grantKey": grant_key})),
        ),
        (
            Method::POST,
            "",
            Some(json!({"grantKey": grant_key, "confirmationToken": token})),
        ),
        (
            Method::POST,
            "/check",
            Some(json!({"attemptKey": grant_key})),
        ),
    ] {
        let url = format!(
            "{}/api/accounts/codex:guardacct/reset-claims{suffix}",
            srv.base
        );
        let mut anon = srv.client.request(method.clone(), &url);
        if let Some(b) = &body {
            anon = anon.header(header::ORIGIN, &srv.origin).json(b);
        }
        assert_eq!(
            anon.send().await.unwrap().status(),
            StatusCode::UNAUTHORIZED
        );

        let mut dev = srv.client.request(method.clone(), &url).header(
            header::AUTHORIZATION,
            format!("Bearer {}", srv.device_token),
        );
        if let Some(b) = &body {
            dev = dev.header(header::ORIGIN, &srv.origin).json(b);
        }
        let dev_resp = dev.send().await.unwrap();
        assert_eq!(dev_resp.status(), StatusCode::FORBIDDEN);
        assert_eq!(read_json(dev_resp).await["code"], "device_scope");
    }

    // OPTIONS returns Allow header and Cache-Control: no-store without body.
    let opts = srv
        .client
        .request(
            Method::OPTIONS,
            format!("{}/api/accounts/codex:guardacct/reset-claims", srv.base),
        )
        .header(header::COOKIE, &srv.cookie_a)
        .send()
        .await
        .unwrap();
    assert_eq!(opts.status(), StatusCode::OK);
    assert_eq!(
        opts.headers().get(header::ALLOW).unwrap(),
        "GET, HEAD, POST"
    );
    assert_eq!(
        opts.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );

    // Query string, foreign/missing Origin, non-JSON Content-Type, invalid grantKey, extra provider fields
    // are ALL rejected BEFORE touching the fake codex executable!
    let bad_query = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:guardacct/reset-claims/prepare?unexpected=1",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_query.status(), StatusCode::BAD_REQUEST);
    assert_eq!(read_json(bad_query).await["code"], "unexpected-query");

    let bad_origin = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:guardacct/reset-claims/prepare",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, "https://wrong.invalid")
        .json(&json!({"grantKey": grant_key}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad_origin.status(), StatusCode::FORBIDDEN);
    assert_eq!(read_json(bad_origin).await["code"], "origin-required");

    let bad_media = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:guardacct/reset-claims/prepare",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, &srv.origin)
        .header(header::CONTENT_TYPE, "text/plain")
        .body("raw")
        .send()
        .await
        .unwrap();
    assert_eq!(bad_media.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(read_json(bad_media).await["code"], "json-required");

    for bad_body in [
        json!({"grantKey": "not-64-hex"}),
        json!({"grantKey": grant_key, "providerGrantId": "injected"}),
        json!({}),
    ] {
        let r = srv
            .client
            .post(format!(
                "{}/api/accounts/codex:guardacct/reset-claims/prepare",
                srv.base
            ))
            .header(header::COOKIE, &srv.cookie_a)
            .header(header::ORIGIN, &srv.origin)
            .json(&bad_body)
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), StatusCode::BAD_REQUEST);
        assert_eq!(read_json(r).await["code"], "invalid-body");
    }

    assert!(
        !instance_dir.join("read-count").exists(),
        "rejected requests must never invoke provider source reads"
    );
    assert!(
        !instance_dir.join("send-count").exists(),
        "rejected requests must never invoke provider sends"
    );
}

#[tokio::test]
async fn reset_live_codex_any_usage_prepare_wrong_caller_and_claim_over_http() {
    let srv = ResetServer::start(Some(FAKE_CODEX_SCRIPT), |ccs| {
        seed_codex_profile(
            ccs,
            "claimacct",
            "acct-claim-1",
            // 60% weekly usage (not 100%) — banked Codex credit must be usable at any usage.
            json!({"primary": 2, "secondary": 60, "spent": false}),
        );
    })
    .await;
    let instance_dir = srv.ccs.join("codex-instances/claimacct");

    // Connect a browser WebSocket client to confirm prepare + claim emit reset-claims-changed.
    let ws_url = format!("{}/ws", srv.base.replace("http://", "ws://"));
    let mut ws_req = ws_url.into_client_request().unwrap();
    ws_req
        .headers_mut()
        .insert(header::ORIGIN, srv.origin.parse().unwrap());
    ws_req
        .headers_mut()
        .insert(header::COOKIE, srv.cookie_a.parse().unwrap());
    let (mut ws, _) = connect_async(ws_req).await.unwrap();

    // 1. Inventory read lists the available Codex reset grant at 60% weekly usage.
    let inv_resp = srv
        .client
        .get(format!(
            "{}/api/accounts/codex:claimacct/reset-claims",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .send()
        .await
        .unwrap();
    assert_eq!(inv_resp.status(), StatusCode::OK);
    assert_eq!(
        inv_resp.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let inv = read_json(inv_resp).await;
    assert_eq!(inv["status"], "ok");
    let grants = inv["grants"].as_array().unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0]["claimable"], true);
    let grant_key = grants[0]["grantKey"].as_str().unwrap().to_owned();

    // 2. Prepare mints a 64-hex one-shot confirmation token bound to browser A.
    let prep_resp = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:claimacct/reset-claims/prepare",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key}))
        .send()
        .await
        .unwrap();
    assert_eq!(prep_resp.status(), StatusCode::OK);
    let prep = read_json(prep_resp).await;
    let token_a = prep["confirmationToken"].as_str().unwrap().to_owned();
    assert_eq!(token_a.len(), 64);

    // WebSocket receives reset-claims-changed with a 32-hex id.
    let ws_frame = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let ws_msg: Value = match ws_frame {
        Message::Text(t) => serde_json::from_str(&t).unwrap(),
        other => panic!("unexpected ws frame: {other:?}"),
    };
    assert_eq!(ws_msg["type"], "reset-claims-changed");
    assert_eq!(ws_msg["id"].as_str().unwrap().len(), 32);

    // Outstanding token refuses a second prepare BEFORE any provider read.
    let reads_before_second_prep = fs::read_to_string(instance_dir.join("read-count"))
        .unwrap()
        .lines()
        .count();
    let second_prep = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:claimacct/reset-claims/prepare",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key}))
        .send()
        .await
        .unwrap();
    assert_eq!(second_prep.status(), StatusCode::CONFLICT);
    assert_eq!(
        read_json(second_prep).await["reason"],
        "claim-token-outstanding"
    );
    assert_eq!(
        fs::read_to_string(instance_dir.join("read-count"))
            .unwrap()
            .lines()
            .count(),
        reads_before_second_prep,
        "outstanding-token refusal must happen before provider read"
    );

    // 3. Wrong browser session (browser B) using browser A's token is refused AND burns the token,
    // without invoking provider send.
    let wrong_caller = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:claimacct/reset-claims",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_b)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key, "confirmationToken": token_a}))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong_caller.status(), StatusCode::CONFLICT);
    let wrong_json = read_json(wrong_caller).await;
    assert_eq!(wrong_json["reason"], "confirmation-invalid");
    assert_eq!(wrong_json["sent"], false);
    assert!(!instance_dir.join("send-count").exists());

    // Even browser A cannot reuse the burned token.
    let replay = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:claimacct/reset-claims",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key, "confirmationToken": token_a}))
        .send()
        .await
        .unwrap();
    assert_eq!(replay.status(), StatusCode::CONFLICT);
    assert_eq!(read_json(replay).await["reason"], "confirmation-invalid");
    assert!(!instance_dir.join("send-count").exists());

    // 4. Fresh prepare + valid claim by browser A redeems exactly once and confirms refill.
    let prep2 = read_json(
        srv.client
            .post(format!(
                "{}/api/accounts/codex:claimacct/reset-claims/prepare",
                srv.base
            ))
            .header(header::COOKIE, &srv.cookie_a)
            .header(header::ORIGIN, &srv.origin)
            .json(&json!({"grantKey": grant_key}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let token_valid = prep2["confirmationToken"].as_str().unwrap().to_owned();

    let claim = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:claimacct/reset-claims",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key, "confirmationToken": token_valid}))
        .send()
        .await
        .unwrap();
    assert_eq!(claim.status(), StatusCode::OK);
    let claim_json = read_json(claim).await;
    assert_eq!(claim_json["outcome"], "claimed");
    assert_eq!(claim_json["verification"], "confirmed");
    assert_eq!(claim_json["pending"], false);
    assert_eq!(
        fs::read_to_string(instance_dir.join("send-count"))
            .unwrap()
            .lines()
            .count(),
        1,
        "provider send must happen exactly once"
    );
}

#[tokio::test]
async fn reset_ambiguous_send_persisted_hold_and_check_again_read_only() {
    let srv = ResetServer::start(Some(FAKE_CODEX_SCRIPT), |ccs| {
        seed_codex_profile(
            ccs,
            "ambigacct",
            "acct-ambig-1",
            json!({"primary": 20, "secondary": 80, "spent": false, "sendOutcome": "ambiguous"}),
        );
    })
    .await;
    let instance_dir = srv.ccs.join("codex-instances/ambigacct");

    let inv = read_json(
        srv.client
            .get(format!(
                "{}/api/accounts/codex:ambigacct/reset-claims",
                srv.base
            ))
            .header(header::COOKIE, &srv.cookie_a)
            .send()
            .await
            .unwrap(),
    )
    .await;
    let grant_key = inv["grants"][0]["grantKey"].as_str().unwrap().to_owned();

    let prep = read_json(
        srv.client
            .post(format!(
                "{}/api/accounts/codex:ambigacct/reset-claims/prepare",
                srv.base
            ))
            .header(header::COOKIE, &srv.cookie_a)
            .header(header::ORIGIN, &srv.origin)
            .json(&json!({"grantKey": grant_key}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let token = prep["confirmationToken"].as_str().unwrap().to_owned();

    // Ambiguous send makes 1 send attempt and records an unresolved pending attempt.
    let claim = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:ambigacct/reset-claims",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key, "confirmationToken": token}))
        .send()
        .await
        .unwrap();
    assert_eq!(claim.status(), StatusCode::OK);
    let claim_body = read_json(claim).await;
    assert_eq!(claim_body["outcome"], "claimed-unconfirmed");
    assert_eq!(claim_body["verification"], "unconfirmed");
    assert_eq!(claim_body["pending"], true);
    let attempt_key = claim_body["attemptKey"].as_str().unwrap().to_owned();
    assert_eq!(attempt_key.len(), 64);
    assert_eq!(
        fs::read_to_string(instance_dir.join("send-count"))
            .unwrap()
            .lines()
            .count(),
        1
    );

    // Even when subsequent source read fails, GET inventory recovers the pending attempt cleanly
    // without leaking provider text, and a new prepare is refused BEFORE provider read!
    write_private(
        &instance_dir.join("fixture-state.json"),
        &json!({"primary": 20, "secondary": 80, "spent": false, "failRead": true}).to_string(),
    );
    let recovered = read_json(
        srv.client
            .get(format!(
                "{}/api/accounts/codex:ambigacct/reset-claims",
                srv.base
            ))
            .header(header::COOKIE, &srv.cookie_b)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(recovered["status"], "unavailable");
    assert_eq!(recovered["attemptKey"], attempt_key);
    assert_eq!(recovered["pendingAttempt"], attempt_key);

    let reads_before_blocked_prep = fs::read_to_string(instance_dir.join("read-count"))
        .unwrap()
        .lines()
        .count();
    let blocked_prep = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:ambigacct/reset-claims/prepare",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_b)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"grantKey": grant_key}))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked_prep.status(), StatusCode::CONFLICT);
    assert_eq!(
        read_json(blocked_prep).await["reason"],
        "check-again-required"
    );
    assert_eq!(
        fs::read_to_string(instance_dir.join("read-count"))
            .unwrap()
            .lines()
            .count(),
        reads_before_blocked_prep,
        "persisted hold must block prepare before any provider read"
    );

    // Now simulate the provider actually refilled; browser B runs read-only check-again ->
    // confirms without ever issuing a second provider send!
    write_private(
        &instance_dir.join("fixture-state.json"),
        &json!({"primary": 0, "secondary": 0, "spent": true}).to_string(),
    );
    let checked = srv
        .client
        .post(format!(
            "{}/api/accounts/codex:ambigacct/reset-claims/check",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_b)
        .header(header::ORIGIN, &srv.origin)
        .json(&json!({"attemptKey": attempt_key}))
        .send()
        .await
        .unwrap();
    assert_eq!(checked.status(), StatusCode::OK);
    let checked_json = read_json(checked).await;
    assert_eq!(checked_json["outcome"], "claimed");
    assert_eq!(checked_json["verification"], "confirmed");
    assert_eq!(checked_json["pending"], false);
    assert_eq!(
        fs::read_to_string(instance_dir.join("send-count"))
            .unwrap()
            .lines()
            .count(),
        1,
        "check-again must be strictly read-only and never invoke provider send"
    );

    // Verify request-id correlation and reset URL alias/query scrubbing in logs/current.jsonl.
    let probe_resp = srv
        .client
        .get(format!(
            "{}/api/accounts/private-alias-secret/reset-claims?confirmationToken=secret-token-value",
            srv.base
        ))
        .header(header::COOKIE, &srv.cookie_a)
        .send()
        .await
        .unwrap();
    assert_eq!(probe_resp.status(), StatusCode::BAD_REQUEST);
    let req_id = probe_resp
        .headers()
        .get("x-ccs-request-id")
        .expect("x-ccs-request-id header present")
        .to_str()
        .unwrap()
        .to_owned();
    assert!(!req_id.is_empty());

    // Poll briefly for fire-and-forget request log flush and verify scrubbing + requestId match.
    let log_path = srv.ccs.join("logs/current.jsonl");
    let mut matched_entry: Option<Value> = None;
    for _ in 0..40 {
        if let Ok(content) = fs::read_to_string(&log_path) {
            for line in content.lines() {
                if let Ok(entry) = serde_json::from_str::<Value>(line)
                    && entry["requestId"].as_str() == Some(req_id.as_str())
                {
                    matched_entry = Some(entry);
                    break;
                }
            }
        }
        if matched_entry.is_some() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    let entry = matched_entry.expect("request log entry with matching x-ccs-request-id");
    let log_text = entry.to_string();
    assert!(!log_text.contains("private-alias-secret"));
    assert!(!log_text.contains("secret-token-value"));
    assert_eq!(
        entry["context"]["path"].as_str(),
        Some("/api/accounts/[account]/reset-claims")
    );

    // Direct check on non-Codex/non-Claude/non-Z.ai provider -> honest read-only refusal.
    let ro = read_json(
        srv.client
            .get(format!(
                "{}/api/accounts/cursor:demo/reset-claims",
                srv.base
            ))
            .header(header::COOKIE, &srv.cookie_a)
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(ro["status"], "unavailable");
    assert_eq!(ro["reason"], "provider-read-only");
}

#[test]
fn reset_url_scrubbing_matches_security_contract() {
    for url in [
        "/api/accounts/private-alias/reset-claims?confirmationToken=private-value",
        "/api/accounts/private-alias/reset-claims/prepare",
        "/api/accounts/private%2falias/reset-claims?unexpected=private-value",
        "/%61pi/accounts/private-alias/reset-claims/check?unexpected=private-value",
    ] {
        assert_eq!(
            aac_log::scrub_logged_url(url),
            "/api/accounts/[account]/reset-claims",
            "url={url}"
        );
    }
    // Raw, percent-encoded, and double-encoded device/hub tokens are scrubbed; ordinary URLs stay intact.
    let token = format!("aacd_{}", "a".repeat(43));
    assert!(!aac_log::scrub_logged_url(&format!("/api/health?t={token}")).contains(&token));
    assert!(
        !aac_log::scrub_logged_url(&format!("/api/health?t=%61{}", &token[1..]))
            .contains(&token[5..])
    );
    assert_eq!(
        aac_log::scrub_logged_url("/api/accounts/dashboard?view=summary"),
        "/api/accounts/dashboard?view=summary"
    );
}
