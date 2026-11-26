//! RED/GREEN parity & cutover tests for P1 in `aac-http`:
//! - `quota_adapter` / `quota_worker` / `native_status_userinfo`
//! - `antigravity_host::Host::from_root` runs natively with zero scripts/Python
//! - `lifecycle_antigravity` prepares `/usr/bin/bwrap` + `antigravityDriver` with zero Python
//! - `cli::antigravity` runs `signin <profile>` natively without delegating to Node/TS.
#![allow(clippy::unwrap_used)]

use aac_files::antigravity_store::{PrivateCredential, ProviderCallError, sha256_hex};
use aac_http::antigravity_worker::{
    JsonRequester, QuotaDeps, QuotaTransportError, SavedProfile, antigravity_load,
    antigravity_reported_plan, antigravity_service_label, antigravity_services,
    antigravity_summary_has_gemini, collect_snapshot, handle_worker_bytes, normalize_antigravity,
    normalize_antigravity_credits, normalize_antigravity_gemini, request_json_http,
    request_userinfo_http, retry_after_seconds, structural_pools,
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Mutex;

fn sample_cred(access: &str, refresh: &str, expiry: &str) -> Vec<u8> {
    json!({
        "auth_method": "consumer",
        "token": {
            "access_token": access,
            "refresh_token": refresh,
            "token_type": "Bearer",
            "expiry": expiry,
        }
    })
    .to_string()
    .into_bytes()
}

fn spawn_one_shot_http(status: &str, headers: &str, body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{headers}Connection: close\r\n\r\n",
        body.len()
    );
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut req_buf = [0u8; 4096];
        let _ = stream.read(&mut req_buf);
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&body);
    });
    format!("http://{addr}/endpoint")
}

#[test]
fn userinfo_http_enforces_no_redirect_cap_duplicate_keys_and_401() {
    let url = spawn_one_shot_http(
        "200 OK",
        "Content-Type: application/json\r\n",
        br#"{"email":"a@b.test","id":"42","verified_email":true}"#.to_vec(),
    );
    let val = request_userinfo_http(&url, "tok").unwrap();
    assert_eq!(val["email"], "a@b.test");

    // 302 redirect refused
    let redir = spawn_one_shot_http("302 Found", "Location: http://127.0.0.1:1/x\r\n", vec![]);
    assert_eq!(
        request_userinfo_http(&redir, "tok"),
        Err(ProviderCallError::Unavailable)
    );

    // > 16384 bytes refused
    let big = spawn_one_shot_http(
        "200 OK",
        "Content-Type: application/json\r\n",
        format!(r#"{{"pad":"{}"}}"#, "x".repeat(17_000)).into_bytes(),
    );
    assert_eq!(
        request_userinfo_http(&big, "tok"),
        Err(ProviderCallError::Unavailable)
    );

    // Duplicate key refused
    let dup = spawn_one_shot_http(
        "200 OK",
        "Content-Type: application/json\r\n",
        br#"{"email":"a@b.test","email":"evil@b.test"}"#.to_vec(),
    );
    assert_eq!(
        request_userinfo_http(&dup, "tok"),
        Err(ProviderCallError::Unavailable)
    );

    // 401 -> NeedsSignIn
    let unauth = spawn_one_shot_http("401 Unauthorized", "", b"{}".to_vec());
    assert_eq!(
        request_userinfo_http(&unauth, "tok"),
        Err(ProviderCallError::NeedsSignIn)
    );
}

#[test]
fn request_json_http_handles_429_retry_after_and_invalid_grant() {
    assert_eq!(retry_after_seconds(Some("120"), 1000.0), 300);
    assert_eq!(retry_after_seconds(Some("900"), 1000.0), 900);
    assert_eq!(retry_after_seconds(Some("999999"), 1000.0), 86400);
    assert_eq!(retry_after_seconds(None, 1000.0), 600);

    let url429 = spawn_one_shot_http(
        "429 Too Many Requests",
        "Retry-After: 900\r\n",
        b"{}".to_vec(),
    );
    let err = request_json_http(&url429, &[], None, false).unwrap_err();
    assert_eq!(err.failure_code, Some("rate_limited"));
    assert_eq!(err.retry_after, Some(900));

    let url400 = spawn_one_shot_http(
        "400 Bad Request",
        "Content-Type: application/json\r\n",
        br#"{"error":"invalid_grant"}"#.to_vec(),
    );
    let err = request_json_http(&url400, &[], None, false).unwrap_err();
    assert_eq!(err.status, "needs_sign_in");
}

#[test]
fn structural_pools_separates_cli_and_app_and_vetoes_malformed_flags() {
    let groups = vec![json!({
        "displayName": "Claude Models",
        "modelIds": ["claude-opus-4-6"],
        "buckets": [
            {"bucketId": "b-weekly", "window": "weekly", "remainingFraction": 0.6, "resetTime": "2026-12-01T00:00:00Z"},
            {"bucketId": "b-5h", "window": "5h", "remainingFraction": 0.9, "resetTime": "2026-11-26T23:00:00Z"}
        ]
    })];
    let mut app_windows = normalize_antigravity(&json!({"groups": groups}));
    let app_pools = structural_pools(&groups, &mut app_windows, "app");
    assert_eq!(app_pools.len(), 1);
    assert_eq!(app_pools[0]["complete"], true);
    assert_eq!(app_pools[0]["eligibility"], "reported-quota");
    assert_eq!(
        app_pools[0]["id"],
        format!("bucket-group:{}", sha256_hex(br#"["b-5h","b-weekly"]"#))
    );

    let mut cli_windows = normalize_antigravity(&json!({"groups": groups}));
    let cli_pools = structural_pools(&groups, &mut cli_windows, "cli");
    assert_eq!(
        cli_pools[0]["id"],
        format!(
            "bucket-group:{}",
            sha256_hex(br#"["cli","b-5h","b-weekly"]"#)
        )
    );
    antigravity_service_label(&mut cli_windows, "cli");
    assert_eq!(cli_windows[0]["key"], "cli-b-weekly");
    assert!(
        cli_windows[0]["label"]
            .as_str()
            .unwrap()
            .ends_with("In T3 (Antigravity CLI)")
    );

    // Malformed disabled flag vetoes complete & sets unverified
    let bad_groups = vec![json!({
        "displayName": "Bad",
        "buckets": [{"bucketId": "b-1", "window": "5h", "remainingFraction": 0.5, "resetTime": "2026-11-26T23:00:00Z", "disabled": "false"}]
    })];
    let mut bad_windows = normalize_antigravity(&json!({"groups": bad_groups}));
    let bad_pools = structural_pools(&bad_groups, &mut bad_windows, "app");
    assert_eq!(bad_pools[0]["complete"], false);
    assert_eq!(bad_pools[0]["eligibility"], "unverified");
}

#[test]
fn gemini_derivation_argon_exclusion_and_credits_match_python() {
    // Argon does not count as Gemini summary
    let summary_with_argon = vec![
        json!({"key":"argon-5h","label":"Gemini 4 Argon · 5-hour","resetAt":"2026-11-26T23:00:00Z","windowMinutes":300}),
        json!({"key":"claude-weekly","label":"Claude · Weekly","resetAt":"2026-12-03T00:00:00Z","windowMinutes":10080}),
    ];
    assert!(!antigravity_summary_has_gemini(&summary_with_argon));

    let per_model = json!({
        "buckets": [
            {"modelId": "gemini-4-argon", "remainingFraction": 0.0, "resetTime": "2026-11-26T23:00:05Z"},
            {"modelId": "gemini-4-neon", "remainingFraction": 0.7, "resetTime": "2026-11-26T23:00:04Z"},
            {"modelId": "gemini-4-hydrogen", "remainingFraction": 0.5, "resetTime": "2026-11-26T23:00:09Z"},
            {"modelId": "gemini-3-pro", "resetTime": "2026-12-03T00:00:00Z"}
        ]
    });
    let derived = normalize_antigravity_gemini(Some(&per_model), &summary_with_argon);
    assert_eq!(derived.len(), 2);
    assert_eq!(derived[0]["key"], "gemini-5h");
    assert_eq!(derived[0]["remainingPercent"], 50.0);
    assert_eq!(derived[0]["usedPercent"], 50.0);
    assert_eq!(
        derived[0]["modelIds"],
        json!(["gemini-4-hydrogen", "gemini-4-neon"])
    );
    // Missing remainingFraction + no remainingAmount => exhausted (0% remaining, 100% used)
    assert_eq!(derived[1]["key"], "gemini-weekly");
    assert_eq!(derived[1]["remainingPercent"], 0.0);
    assert_eq!(derived[1]["usedPercent"], 100.0);

    // Reported plan & credits
    let load = json!({
        "planInfo": {"planType": "Google AI Ultra"},
        "paidTier": {"id": "g1-ultra-tier", "name": "Google AI Ultra", "availableCredits": [{"creditAmount": "25000"}]}
    });
    let (plan, reported) = antigravity_reported_plan(&load);
    assert_eq!(plan.as_deref(), Some("Google AI Ultra"));
    assert_eq!(reported.unwrap()["paidTier"]["id"], "g1-ultra-tier");
    let credits = normalize_antigravity_credits(&load);
    assert_eq!(credits.len(), 1);
    assert_eq!(credits[0]["key"], "google-ai-credits");
    assert_eq!(credits[0]["remaining"], 25000.0);
}

struct MockReq {
    responses: Mutex<HashMap<String, Result<Value, QuotaTransportError>>>,
    calls: Mutex<Vec<String>>,
}

impl JsonRequester for MockReq {
    fn request_json(
        &self,
        url: &str,
        _headers: &[(&str, &str)],
        _body: Option<&Value>,
        _form: bool,
    ) -> Result<Value, QuotaTransportError> {
        self.calls.lock().unwrap().push(url.to_string());
        self.responses
            .lock()
            .unwrap()
            .remove(url)
            .unwrap_or_else(|| Err(QuotaTransportError::new("error", "unexpected url")))
    }
}

#[test]
fn antigravity_services_cli_429_stops_immediately_while_app_429_keeps_cli() {
    // CLI 429 fails immediately without calling app
    let mut map = HashMap::new();
    map.insert(
        "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary".into(),
        Err(QuotaTransportError::rate_limited(900)),
    );
    let req = MockReq {
        responses: Mutex::new(map),
        calls: Mutex::new(Vec::new()),
    };
    let err = antigravity_services(&req, &[], "proj-1", || 0.0, true).unwrap_err();
    assert_eq!(err.failure_code, Some("rate_limited"));
    assert_eq!(req.calls.lock().unwrap().len(), 1);

    // CLI ok + app 429 keeps CLI reading and marks app unavailable + retryAfter
    let mut map2 = HashMap::new();
    map2.insert(
        "https://daily-cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary".into(),
        Ok(json!({
            "groups": [{
                "displayName": "Gemini Models",
                "buckets": [{"bucketId": "g-5h", "window": "5h", "remainingFraction": 0.8, "resetTime": "2026-11-26T23:00:00Z"}]
            }]
        })),
    );
    map2.insert(
        "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary".into(),
        Err(QuotaTransportError::rate_limited(1200)),
    );
    let req2 = MockReq {
        responses: Mutex::new(map2),
        calls: Mutex::new(Vec::new()),
    };
    let services = antigravity_services(&req2, &[], "proj-1", || 0.0, true).unwrap();
    assert_eq!(services.len(), 2);
    assert_eq!(services[0].status, "ok");
    assert_eq!(services[1].status, "rate_limited");
    assert_eq!(services[1].retry_after, Some(1200));
    assert_eq!(services[1].windows[0]["status"], "unavailable");

    // antigravity_load falls back to daily when prod fails with non-auth error
    let mut map3 = HashMap::new();
    map3.insert(
        "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist".into(),
        Err(QuotaTransportError::new("error", "temp")),
    );
    map3.insert(
        "https://daily-cloudcode-pa.googleapis.com/v1internal:loadCodeAssist".into(),
        Ok(json!({"cloudaicompanionProject": "proj-cli"})),
    );
    let req3 = MockReq {
        responses: Mutex::new(map3),
        calls: Mutex::new(Vec::new()),
    };
    let loaded = antigravity_load(&req3, &[]).unwrap();
    assert_eq!(loaded["cloudaicompanionProject"], "proj-cli");
}

struct StubDeps {
    now_ms: i64,
    email: String,
    subject: String,
}

impl QuotaDeps for StubDeps {
    fn userinfo(&self, _access: &str) -> Result<Value, QuotaTransportError> {
        Ok(json!({"email": self.email, "id": self.subject, "verified_email": true}))
    }
    fn refresh(&self, _refresh_token: &str) -> Result<Value, QuotaTransportError> {
        Ok(json!({"access_token": "refreshed-acc", "expires_in": 3600}))
    }
    fn quota(&self, _access: &str, email: &str) -> Result<Value, QuotaTransportError> {
        Ok(json!({
            "email": email,
            "plan": "Google AI Pro",
            "windows": [{
                "key": "gemini-5h",
                "label": "Gemini · 5-hour",
                "usedPercent": 20.0,
                "remainingPercent": 80.0,
                "resetAt": "2026-11-26T23:00:00Z",
                "windowMinutes": 300,
                "kind": "rate_limit",
                "status": "ignored-non-unavailable"
            }],
            "pools": [],
            "reportedPlan": {"planType": "Google AI Pro"}
        }))
    }
    fn now_ms(&self) -> i64 {
        self.now_ms
    }
}

#[test]
fn collect_snapshot_and_worker_bytes_bind_identity_and_strip_unknown_window_status() {
    let raw = sample_cred("acc-1", "ref-1", "2026-11-26T23:00:00Z");
    let cred = PrivateCredential::new(raw.clone()).unwrap();
    let rev = sha256_hex(&raw);
    let id_key = sha256_hex(br#"["antigravity","alice@example.com","10001"]"#);
    let deps = StubDeps {
        now_ms: 1_795_728_000_000,
        email: "alice@example.com".into(),
        subject: "10001".into(),
    };
    let profile = SavedProfile {
        id: "work".into(),
        email: "alice@example.com".into(),
        identity_key: id_key.clone(),
        credential_revision: rev.clone(),
        credential: cred,
    };
    let snap = collect_snapshot(&profile, &deps);
    assert_eq!(snap["status"], "fresh");
    assert_eq!(snap["identityVerified"], true);
    assert_eq!(snap["identityValidation"], "verified");
    assert!(snap["windows"][0].get("status").is_none());

    // Worker byte interface: identity operation
    let id_req = json!({"operation": "identity", "credentialBase64": STANDARD.encode(&raw)});
    let out = handle_worker_bytes(&serde_json::to_vec(&id_req).unwrap(), &deps).unwrap();
    let parsed: Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(parsed["email"], "alice@example.com");
    assert_eq!(parsed["identityKey"], id_key);
    assert_eq!(parsed["credentialRevision"], rev);
}

#[test]
fn cli_antigravity_signin_runs_natively_without_node_or_ts() {
    let dir = tempfile::tempdir().unwrap();
    // Host::from_root must construct a native Rust host with zero scripts directory present.
    assert!(
        aac_http::antigravity_host::Host::from_root(dir.path()).is_some(),
        "Host::from_root must succeed natively without scripts/antigravity/"
    );
    // Invalid profile name must fail natively without delegating to Node/TS.
    let code = aac_http::cli::antigravity::run(&["signin".into(), "Bad Name".into()], dir.path());
    assert_eq!(code, 1);
}
