//! Offline parity suite for `desktop_helpers.py`, `desktop_usage.py`, and `cursor_usage.py`.
//!
//! Covers all 58 `tests/spec/parser-vectors.json` vectors for `desktop_helpers`/`cursor_usage`,
//! all behavioral scenarios from `desktop_usage_test.py`, `cursor_usage_test.py`, and
//! `cursor_failure_contract_fixture.py`, plus local TCP HTTP safety tests.
#![allow(clippy::unwrap_used)]

use aac_agent::desktop_usage::{
    self as du, AGY_BASE, AGY_CLI_BASE, AGY_OPTIONAL_BUDGET_SECONDS, AGY_USER_AGENT,
    CURSOR_CURRENT_USAGE_URL, CURSOR_MEMBERSHIP_URL, CURSOR_PLAN_INFO_URL, CURSOR_PROFILE_URL,
    CURSOR_SUMMARY_URL, Credential, DesktopEnv, DesktopTransport, ReqwestDesktopTransport,
    UsageError,
};
use aac_agent::failover_additional::{
    self as additional, AdditionalEnv, CollectionError, PlanHttpBody, PlanTransport, PortalError,
    PortalTransport,
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::Connection;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const NOW_ISO: &str = "2026-10-14T12:00:00Z";
const NOW_SECS: f64 = 1_791_979_200.0; // 2026-10-14T12:00:00Z
const RESET_MS: i64 = 1_792_022_400_000;
const RESET_ISO: &str = "2026-10-15T00:00:00Z";

fn fake_jwt(claims: Value) -> String {
    let payload = URL_SAFE_NO_PAD.encode(claims.to_string().as_bytes());
    format!("fake.{payload}.signature")
}

fn sha256_file(path: &Path) -> String {
    let mut h = Sha256::new();
    h.update(fs::read(path).expect("read file for sha256"));
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

fn test_env_map(home: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(
        "XDG_CONFIG_HOME".into(),
        home.join(".config").to_string_lossy().into_owned(),
    );
    map
}

#[derive(Clone)]
struct RecordedDesktopCall {
    url: String,
    headers: Vec<(String, String)>,
    body: Option<Value>,
    form: bool,
}

struct QueueDesktopTransport {
    replies: Mutex<VecDeque<Result<Value, UsageError>>>,
    calls: Mutex<Vec<RecordedDesktopCall>>,
}

impl QueueDesktopTransport {
    fn new(replies: Vec<Result<Value, UsageError>>) -> Self {
        Self {
            replies: Mutex::new(replies.into()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<RecordedDesktopCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl DesktopTransport for QueueDesktopTransport {
    fn request_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a Value>,
        form: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, UsageError>> + Send + 'a>> {
        self.calls.lock().unwrap().push(RecordedDesktopCall {
            url: url.to_owned(),
            headers: headers.to_vec(),
            body: body.cloned(),
            form,
        });
        let next = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected DesktopTransport call to {url}"));
        Box::pin(async move { next })
    }
}

struct RouterDesktopTransport {
    replies: BTreeMap<String, Result<Value, UsageError>>,
    durations: BTreeMap<String, f64>,
    clock: Arc<Mutex<f64>>,
    calls: Mutex<Vec<(String, f64)>>,
}

impl RouterDesktopTransport {
    fn new(replies: Vec<(String, Result<Value, UsageError>)>) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            durations: BTreeMap::new(),
            clock: Arc::new(Mutex::new(0.0)),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn timed(
        started: f64,
        durations: Vec<(String, f64)>,
        replies: Vec<(String, Result<Value, UsageError>)>,
    ) -> Self {
        Self {
            replies: replies.into_iter().collect(),
            durations: durations.into_iter().collect(),
            clock: Arc::new(Mutex::new(started)),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn elapsed_now(&self) -> f64 {
        *self.clock.lock().unwrap()
    }

    fn call_urls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(u, _)| u.clone())
            .collect()
    }
}

impl DesktopTransport for RouterDesktopTransport {
    fn request_json<'a>(
        &'a self,
        url: &'a str,
        _headers: &'a [(String, String)],
        _body: Option<&'a Value>,
        _form: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, UsageError>> + Send + 'a>> {
        let mut t = self.clock.lock().unwrap();
        let now = *t;
        self.calls.lock().unwrap().push((url.to_owned(), now));
        if let Some(d) = self.durations.get(url) {
            *t += *d;
        } else if !self.durations.is_empty() {
            *t += 1.0;
        }
        let reply = self
            .replies
            .get(url)
            .cloned()
            .unwrap_or_else(|| panic!("unexpected routed URL: {url}"));
        Box::pin(async move { reply })
    }
}

type PlanCall = (String, Vec<(String, String)>, Option<Vec<u8>>);

struct QueuePlanTransport {
    replies: Mutex<VecDeque<Result<PlanHttpBody, CollectionError>>>,
    calls: Mutex<Vec<PlanCall>>,
}

impl QueuePlanTransport {
    fn json_replies(replies: Vec<Result<Value, CollectionError>>) -> Self {
        Self {
            replies: Mutex::new(
                replies
                    .into_iter()
                    .map(|r| r.map(PlanHttpBody::Json))
                    .collect(),
            ),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn call_urls(&self) -> Vec<String> {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .map(|(u, _, _)| u.clone())
            .collect()
    }

    fn calls(&self) -> Vec<PlanCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl PlanTransport for QueuePlanTransport {
    fn request<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a [u8]>,
        _text: bool,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<PlanHttpBody, CollectionError>> + Send + 'a>>
    {
        self.calls.lock().unwrap().push((
            url.to_owned(),
            headers.to_vec(),
            body.map(|b| b.to_vec()),
        ));
        let next = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected PlanTransport call to {url}"));
        Box::pin(async move { next })
    }
}

struct MockPortal {
    email: String,
    team_id: String,
    quota: Value,
    calls: AtomicUsize,
}

impl MockPortal {
    fn new(email: &str, team_id: &str, quota: Value) -> Self {
        Self {
            email: email.to_owned(),
            team_id: team_id.to_owned(),
            quota,
            calls: AtomicUsize::new(0),
        }
    }
}

impl PortalTransport for MockPortal {
    fn get<'a>(
        &'a self,
        route: &'a str,
    ) -> Pin<Box<dyn std::future::Future<Output = Result<Value, PortalError>> + Send + 'a>> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let resp = if route == "/api/auth/me" {
            Ok(json!({ "email": self.email, "userId": "u-1" }))
        } else if route == "/api/portal/teams" {
            Ok(json!({ "teams": [{ "team_id": self.team_id, "team_name": "Fixture" }] }))
        } else {
            Ok(json!({ "subscription_quota": self.quota }))
        };
        Box::pin(async move { resp })
    }
}

fn cli_summary_pro() -> Value {
    json!({"groups": [
        {"displayName": "Gemini Models", "buckets": [
            {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.9152058, "resetTime": "2026-11-26T22:06:23Z"},
            {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 0.7032202, "resetTime": "2026-11-20T03:06:23Z"}]},
        {"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-26T23:56:34Z"},
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-11-20T04:56:34Z"}]},
        {"displayName": "Gemini 4 Argon", "buckets": [
            {"bucketId": "gemini-argon-wtu-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-26T23:56:34Z"}]}]})
}

fn app_summary_pro() -> Value {
    json!({"groups": [
        {"displayName": "Gemini Models", "buckets": [
            {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 0.9991121, "resetTime": "2026-11-20T05:47:39Z"},
            {"bucketId": "gemini-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-11-20T04:55:30Z"}]},
        {"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-26T23:55:30Z"},
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-11-20T04:55:30Z"}]},
        {"displayName": "Gemini 4 Argon", "buckets": [
            {"bucketId": "gemini-argon-wtu-weekly", "window": "weekly", "remainingFraction": 0.6216592, "resetTime": "2026-11-20T05:47:49Z"}]}]})
}

fn free_summary() -> Value {
    json!({"groups": [
        {"displayName": "Gemini Models", "buckets": [
            {"bucketId": "gemini-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-26T23:55:31Z"}]},
        {"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-26T23:55:31Z"}]}]})
}

fn load_pro() -> Value {
    json!({"cloudaicompanionProject": "project-fixture", "paidTier": {"id": "g1-pro-tier", "name": "Google AI Pro"}})
}

fn load_free() -> Value {
    json!({"cloudaicompanionProject": "project-fixture", "paidTier": {"id": "free-tier", "name": "Antigravity Starter Quota"}})
}

fn cursor_cred(plan: Option<&str>) -> Credential {
    let token = fake_jwt(json!({"sub": "auth0|fixture", "email": "one@example.test"}));
    Credential {
        access: token,
        source: "Fixture".into(),
        refresh: String::new(),
        expires: None,
        email: Some("one@example.test".into()),
        project: None,
        plan: plan.map(str::to_owned),
        google_client: None,
    }
}

fn cursor_plan_info(overrides: Value) -> Value {
    let mut info = json!({
        "includedUsagePeriod": "INCLUDED_USAGE_PERIOD_WEEKLY",
        "includedUsageResetsAt": RESET_MS.to_string(),
        "billingCycleEnd": "1794697200000",
        "planName": "Premium"
    });
    if let (Some(dst), Some(src)) = (info.as_object_mut(), overrides.as_object()) {
        for (k, v) in src {
            dst.insert(k.clone(), v.clone());
        }
    }
    json!({ "isMerged": true, "planInfo": info })
}

fn cursor_current(value: Value) -> Value {
    json!({
        "enabled": true,
        "billingCycleEnd": "1794697200000",
        "planUsage": {
            "totalPercentUsed": value,
            "autoPercentUsed": 91,
            "apiPercentUsed": 98,
            "includedSpend": 100,
            "totalSpend": 100,
            "limit": 10000
        },
        "private": "not-for-export"
    })
}

fn cursor_summary() -> Value {
    json!({
        "billingCycleEnd": "2026-11-14T00:00:00Z",
        "individualUsage": {
            "plan": {"used": 1234, "limit": 10000, "autoPercentUsed": 7, "apiPercentUsed": 35},
            "onDemand": {"used": 250, "limit": 2000, "remaining": 1750}
        }
    })
}

fn write_oauth_metadata(home: &Path, payload: &Value, mode: u32) -> PathBuf {
    let path = home.join(".ccs/account-usage/antigravity-oauth-client.json");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, payload.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&path, fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    let _ = mode;
    path
}

// =========================================================================
// 1. Parser-vectors.json (58 cases) + desktop_helpers subtle parity tests
// =========================================================================

#[test]
fn parser_vectors_json_all_desktop_helpers_and_cursor_cases() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let raw = fs::read_to_string(root.join("tests/spec/parser-vectors.json"))
        .expect("read parser-vectors.json");
    let doc: Value = serde_json::from_str(&raw).expect("parse parser-vectors.json");
    let now = doc["nowUnixSeconds"].as_f64().expect("nowUnixSeconds");
    let cases = doc["cases"].as_array().expect("cases");
    let mut checked = 0usize;
    for case in cases {
        let module = case["module"].as_str().unwrap_or("");
        let func = case["function"].as_str().unwrap_or("");
        let input = &case["input"];
        let expected = &case["python"];
        match (module, func) {
            ("desktop_helpers", "number") => {
                let actual = du::number(input).map(|n| json!(n)).unwrap_or(Value::Null);
                if let (Some(a), Some(e)) = (actual.as_f64(), expected.as_f64()) {
                    assert!((a - e).abs() < 1e-12, "number({input}): got {a}, want {e}");
                } else {
                    assert_eq!(actual, *expected, "number({input})");
                }
                checked += 1;
            }
            ("desktop_helpers", "reset_at") => {
                let actual = du::reset_at(input)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                assert_eq!(actual, *expected, "reset_at({input})");
                checked += 1;
            }
            ("desktop_helpers", "retry_after_seconds") => {
                let actual = json!(du::retry_after_seconds(input, Some(now)));
                assert_eq!(actual, *expected, "retry_after_seconds({input})");
                checked += 1;
            }
            ("cursor_usage", "_milliseconds_reset") => {
                let actual = du::cursor_milliseconds_reset(input)
                    .map(Value::String)
                    .unwrap_or(Value::Null);
                assert_eq!(actual, *expected, "_milliseconds_reset({input})");
                checked += 1;
            }
            _ => {}
        }
    }
    assert_eq!(
        checked, 58,
        "expected all 58 desktop_helpers + cursor_usage vectors"
    );
}

#[test]
fn desktop_helpers_safe_text_reset_at_retry_after_and_invalid_grant_parity() {
    // Unicode whitespace after Bearer must be rejected (matches Python bearer\s).
    assert_eq!(du::safe_text(&json!("Bearer\u{00a0}secret"), 160), None);
    assert_eq!(du::safe_text(&json!("bearer\tsecret"), 160), None);
    // Python str.strip() strips \x1f before the control-char check.
    assert_eq!(
        du::safe_text(&json!("\x1fhello"), 160).as_deref(),
        Some("hello")
    );
    assert_eq!(du::safe_text(&json!("hel\x1flo"), 160), None);
    // Python fromisoformat does not trim whitespace or accept lowercase 'z'.
    assert_eq!(du::reset_at(&json!(" 2026-10-01T00:00:00Z ")), None);
    assert_eq!(du::reset_at(&json!("2026-10-01T00:00:00z")), None);
    assert_eq!(
        du::reset_at(&json!("2026-10-01T00:00:00-04:00")).as_deref(),
        Some("2026-10-01T04:00:00Z")
    );
    // Fractional HTTP-date delta ceils to whole integer seconds (desktop_usage_test.py:791).
    assert_eq!(
        du::retry_after_seconds(&json!("Thu, 01 Jan 1970 00:46:40 GMT"), Some(1000.25)),
        1800
    );
    assert_eq!(du::retry_after_seconds(&json!("0"), Some(1000.25)), 300);
    assert_eq!(
        du::retry_after_seconds(&json!("999999999999"), Some(1000.25)),
        86_400
    );
    assert_eq!(du::retry_after_seconds(&Value::Null, Some(1000.25)), 600);
    assert_eq!(du::retry_after_seconds(&json!("-1"), Some(1000.25)), 600);
    assert_eq!(
        du::retry_after_seconds(&json!("x".repeat(129)), Some(1000.25)),
        600
    );

    // is_invalid_grant
    assert!(du::is_invalid_grant(
        br#"{"error":"invalid_grant","error_description":"revoked"}"#
    ));
    for bad in [
        br#"{"error":"invalid_request"}"#.as_slice(),
        b"not json",
        b"",
        br#"["invalid_grant"]"#,
        br#"{"error":["invalid_grant"]}"#,
        b"{\"error\":\"invalid_grant \xff\"}",
    ] {
        assert!(!du::is_invalid_grant(bad), "should reject {bad:?}");
    }

    // normalize_muse explicit null window_duration_mins overrides default 10080 (Python dict.get).
    let muse_null_dur = du::normalize_muse(&json!({
        "subs_usage": {
            "weekly": {"used_percent": 5, "window_duration_mins": null}
        }
    }));
    assert_eq!(muse_null_dur.len(), 1);
    assert!(muse_null_dur[0]["windowMinutes"].is_null());
}

// =========================================================================
// 2. Antigravity Gemini per-model derivation + Argon/Hydrogen/Neon parity
// =========================================================================

#[test]
fn antigravity_gemini_derivation_clustering_anchors_proto3_and_argon_exclusion() {
    let summary = du::normalize_antigravity(
        &json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
        {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-01T15:27:22Z"},
        {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:42:30Z"}]}]}),
    );
    let payload = json!({"buckets": [
        {"modelId": "gemini-3.8-flash-tiered", "tokenType": "WTUS", "remainingFraction": 0.55, "resetTime": "2026-10-25T20:42:36Z"},
        {"modelId": "gemini-3.1-pro", "tokenType": "WTUS", "remainingFraction": 0.4, "resetTime": "2026-10-25T20:42:36Z"},
        {"modelId": "claude-opus-5-5-high", "tokenType": "WTUS", "remainingFraction": 0.9, "resetTime": "2026-10-25T20:42:36Z"},
        {"modelId": "gemini-2.5-pro", "tokenType": "WTUS", "remainingFraction": 0.8},
    ]});
    let windows = du::normalize_antigravity_gemini(&payload, &summary);
    assert_eq!(windows.len(), 1);
    let w = &windows[0];
    assert_eq!(w["key"], "gemini-5h");
    assert_eq!(w["label"], "Gemini Models · 5-hour");
    assert_eq!(w["remainingPercent"], 40.0);
    assert_eq!(w["usedPercent"], 60.0);
    assert_eq!(w["resetAt"], "2026-10-25T20:42:36Z");
    assert_eq!(w["windowMinutes"], 300.0);
    assert_eq!(
        w["modelIds"],
        json!(["gemini-3.1-pro", "gemini-3.8-flash-tiered"])
    );
    assert!(!w.to_string().contains("WTUS"));

    // Weekly and unmatched period
    let weekly = du::normalize_antigravity_gemini(
        &json!({"buckets": [{"modelId": "gemini-3.8-flash-tiered", "remainingFraction": 0.35, "resetTime": "2026-11-01T15:27:30Z"}]}),
        &summary,
    );
    assert_eq!(weekly[0]["key"], "gemini-weekly");
    assert_eq!(weekly[0]["label"], "Gemini Models · Weekly");
    assert_eq!(weekly[0]["windowMinutes"], 10080.0);

    let unmatched = du::normalize_antigravity_gemini(
        &json!({"buckets": [{"modelId": "gemini-3.1-pro", "remainingFraction": 0.5, "resetTime": "2026-10-26T09:00:00Z"}]}),
        &summary,
    );
    assert_eq!(unmatched[0]["key"], "gemini-window");
    assert_eq!(unmatched[0]["label"], "Gemini Models");
    assert!(unmatched[0]["windowMinutes"].is_null());

    // Conflicting 5h + weekly anchors within 600s stay unknown regardless of order.
    let conflict_a = du::normalize_antigravity(
        &json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
        {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-10-25T20:45:00Z"},
        {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:50:00Z"}]}]}),
    );
    let conflict_b = du::normalize_antigravity(
        &json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
        {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:50:00Z"},
        {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-10-25T20:45:00Z"}]}]}),
    );
    for anchors in [conflict_a, conflict_b] {
        let got = du::normalize_antigravity_gemini(
            &json!({"buckets": [{"modelId": "gemini-3.8-flash", "remainingFraction": 0.5, "resetTime": "2026-10-25T20:50:06Z"}]}),
            &anchors,
        );
        assert_eq!(got[0]["key"], "gemini-window");
        assert!(got[0]["windowMinutes"].is_null());
        assert_eq!(got[0]["label"], "Gemini Models");
    }

    // Repeated same-period groups get -2 suffix; lone weekly keeps gemini-weekly.
    let repeated_anchors = du::normalize_antigravity(
        &json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
        {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-10-25T20:50:00Z"}]}]}),
    );
    let repeated = du::normalize_antigravity_gemini(
        &json!({"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": 0.5, "resetTime": "2026-10-25T20:50:06Z"},
            {"modelId": "gemini-3.1-pro", "remainingFraction": 0.7, "resetTime": "2026-10-25T20:53:00Z"}
        ]}),
        &repeated_anchors,
    );
    let keys: Vec<_> = repeated
        .iter()
        .map(|w| w["key"].as_str().unwrap())
        .collect();
    assert_eq!(keys, vec!["gemini-5h", "gemini-5h-2"]);

    // Cross-second boundary clustering uses earliest normalized reset.
    let clustered = du::normalize_antigravity_gemini(
        &json!({"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": 0.5, "resetTime": "2026-10-25T20:42:34.900Z"},
            {"modelId": "gemini-3.1-pro", "remainingFraction": 0.6, "resetTime": "2026-10-25T20:42:36.100Z"}
        ]}),
        &summary,
    );
    assert_eq!(clustered.len(), 1);
    assert_eq!(clustered[0]["resetAt"], "2026-10-25T20:42:34Z");
    assert_eq!(clustered[0]["remainingPercent"], 50.0);

    // Proto3 omitted fraction = 0.0; amount-only skipped; unsafe modelId counted but unlisted.
    let exhausted = du::normalize_antigravity_gemini(
        &json!({"buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": 0.55, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.1-pro", "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-sk-task-1", "remainingFraction": 0.9, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.8-flash", "remainingFraction": 0.7, "resetTime": "2026-10-25T20:42:36Z"},
            {"modelId": "gemini-3.5-flash", "remainingAmount": "1200", "resetTime": "2026-10-25T20:42:36Z"}
        ]}),
        &summary,
    );
    assert_eq!(exhausted.len(), 1);
    assert_eq!(exhausted[0]["remainingPercent"], 0.0);
    assert_eq!(exhausted[0]["usedPercent"], 100.0);
    assert_eq!(
        exhausted[0]["modelIds"],
        json!(["gemini-3.1-pro", "gemini-3.8-flash"])
    );
    assert!(
        du::normalize_antigravity_gemini(
            &json!({"buckets": [{"modelId": "gemini-3.5-flash", "remainingAmount": "1200", "resetTime": "2026-10-25T20:42:36Z"}]}),
            &summary,
        )
        .is_empty()
    );

    // Argon summary does NOT stand down Gemini derivation, and Hydrogen joins Gemini while Argon stays out.
    let argon_summary = du::normalize_antigravity(&json!({"groups": [
        {"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 1, "resetTime": "2026-11-20T05:56:22Z"},
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 1, "resetTime": "2026-11-13T10:47:39Z"}]},
        {"displayName": "Gemini 4 Argon", "buckets": [
            {"bucketId": "gemini-argon-wtu-weekly", "window": "weekly", "remainingFraction": 0.9898667, "resetTime": "2026-11-20T05:47:49Z"}]}]}));
    assert!(!du::antigravity_summary_has_gemini(&argon_summary));
    let hydro = du::normalize_antigravity_gemini(
        &json!({"buckets": [
            {"modelId": "gemini-3.8-flash-high", "remainingFraction": 0.9984486, "resetTime": "2026-11-13T10:47:39Z"},
            {"modelId": "gemini-4-hydrogen-high", "remainingFraction": 0.9984486, "resetTime": "2026-11-13T10:47:39Z"},
            {"modelId": "gemini-4-argon-high", "remainingFraction": 0.5, "resetTime": "2026-11-20T05:47:49Z"}
        ]}),
        &argon_summary,
    );
    assert_eq!(hydro.len(), 1);
    assert_eq!(hydro[0]["key"], "gemini-5h");
    assert_eq!(
        hydro[0]["modelIds"],
        json!(["gemini-3.8-flash-high", "gemini-4-hydrogen-high"])
    );
    assert!(!json!(hydro).to_string().to_lowercase().contains("argon"));

    // Summary with Gemini key OR Gemini Models label stands down derivation.
    let mut with_label = summary.clone();
    with_label.push(json!({"key": "group-0-0", "label": "Gemini Models · 5-hour", "resetAt": "2026-10-25T20:42:30Z", "windowMinutes": 300}));
    assert!(du::antigravity_summary_has_gemini(&with_label));
    assert!(du::normalize_antigravity_gemini(&payload, &with_label).is_empty());
}

// =========================================================================
// 3. Standalone credential discovery (Muse, Antigravity, Cursor)
// =========================================================================

#[test]
fn standalone_credential_discovery_reads_without_mutation() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let env_map = test_env_map(home);

    // OMP muse-code packed JSON + disabled/api_key ignored; DB SHA-256 unchanged.
    let omp_path = home.join(".omp/agent/agent.db");
    fs::create_dir_all(omp_path.parent().unwrap()).unwrap();
    let conn = Connection::open(&omp_path).unwrap();
    conn.execute(
        "CREATE TABLE auth_credentials(provider TEXT,credential_type TEXT,data TEXT,disabled_cause TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_credentials VALUES(?,?,?,?)",
        rusqlite::params![
            "muse-code",
            "oauth",
            json!({"access": json!({"oauthAccessToken": "dca:fixture-login", "apiKey": "sk-never-export"}).to_string()}).to_string(),
            Option::<String>::None
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_credentials VALUES(?,?,?,?)",
        rusqlite::params![
            "muse-code",
            "oauth",
            json!({"access": "dca:disabled"}).to_string(),
            Some("disabled")
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_credentials VALUES(?,?,?,?)",
        rusqlite::params![
            "muse-code",
            "api_key",
            json!({"access": "sk-key"}).to_string(),
            Option::<String>::None
        ],
    )
    .unwrap();
    drop(conn);
    let before = sha256_file(&omp_path);
    let cred = du::muse_credentials(home, Some(&env_map)).expect("omp muse cred");
    assert_eq!(cred.access, "dca:fixture-login");
    assert_eq!(cred.source, "OMP");
    assert_eq!(sha256_file(&omp_path), before);
    assert!(!format!("{cred:?}").contains("fixture-login"));

    // Native Muse auth.json preferred; relative XDG_CONFIG_HOME ignored.
    let muse_auth = home.join(".config/muse/auth.json");
    fs::create_dir_all(muse_auth.parent().unwrap()).unwrap();
    fs::write(
        &muse_auth,
        json!({"providers": {"meta": {"access_token": "dca:native-login", "user_email": "example@example.com"}}}).to_string(),
    )
    .unwrap();
    let mut rel_env = BTreeMap::new();
    rel_env.insert("XDG_CONFIG_HOME".into(), "relative-ignored".into());
    let native_muse = du::muse_credentials(home, Some(&rel_env)).expect("native muse");
    assert_eq!(native_muse.access, "dca:native-login");
    assert_eq!(native_muse.source, "Muse CLI");
    assert_eq!(native_muse.email.as_deref(), Some("example@example.com"));

    // Cursor Mac state.vscdb with ALL 4 keys including refreshToken + JWT exp * 1000.
    let vscdb = home.join("Library/Application Support/Cursor/User/globalStorage/state.vscdb");
    fs::create_dir_all(vscdb.parent().unwrap()).unwrap();
    let cconn = Connection::open(&vscdb).unwrap();
    cconn
        .execute(
            "CREATE TABLE ItemTable(key TEXT PRIMARY KEY,value TEXT)",
            [],
        )
        .unwrap();
    let token = fake_jwt(json!({"sub": "user_123", "exp": 2_000_000_000}));
    cconn
        .execute(
            "INSERT INTO ItemTable VALUES(?,?)",
            rusqlite::params!["cursorAuth/accessToken", &token],
        )
        .unwrap();
    cconn
        .execute(
            "INSERT INTO ItemTable VALUES(?,?)",
            rusqlite::params!["cursorAuth/refreshToken", "cursor-refresh-secret"],
        )
        .unwrap();
    cconn
        .execute(
            "INSERT INTO ItemTable VALUES(?,?)",
            rusqlite::params!["cursorAuth/cachedEmail", "example@example.com"],
        )
        .unwrap();
    cconn
        .execute(
            "INSERT INTO ItemTable VALUES(?,?)",
            rusqlite::params!["cursorAuth/stripeMembershipType", "premium"],
        )
        .unwrap();
    drop(cconn);
    let ccred = du::cursor_credentials(home, "mac", Some(&env_map)).expect("cursor cred");
    assert_eq!(ccred.access, token);
    assert_eq!(ccred.refresh, "cursor-refresh-secret");
    assert_eq!(ccred.expires, Some(2_000_000_000_000.0));
    assert_eq!(ccred.email.as_deref(), Some("example@example.com"));
    assert_eq!(ccred.plan.as_deref(), Some("premium"));

    // Antigravity native consumer OAuth file.
    let agy_path = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
    fs::create_dir_all(agy_path.parent().unwrap()).unwrap();
    fs::write(
        &agy_path,
        json!({
            "auth_method": "consumer",
            "id_token": fake_jwt(json!({"email": "agy@example.com", "sub": "12345"})),
            "token": {
                "access_token": "agy-access",
                "refresh_token": "agy-refresh",
                "expiry": "2026-10-15T00:00:00Z"
            }
        })
        .to_string(),
    )
    .unwrap();
    let acred = du::antigravity_credentials(home).expect("antigravity cred");
    assert_eq!(acred.access, "agy-access");
    assert_eq!(acred.refresh, "agy-refresh");
    assert_eq!(acred.email.as_deref(), Some("agy@example.com"));
    assert_eq!(acred.expires, Some(RESET_MS as f64));
    assert_eq!(acred.google_client.as_deref(), Some("consumer"));
}

// =========================================================================
// 4. Antigravity OAuth client metadata, refresh, two services, deadlines
// =========================================================================

#[tokio::test]
async fn antigravity_oauth_metadata_windows_acl_and_refresh_contract() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let meta = json!({
        "schemaVersion": 1,
        "clientId": "fixture-client.apps.googleusercontent.com",
        "clientSecret": "synthetic-app-value"
    });
    let path = write_oauth_metadata(home, &meta, 0o600);
    let before = fs::read(&path).unwrap();
    assert_eq!(
        du::antigravity_oauth_client(home).unwrap(),
        (
            "fixture-client.apps.googleusercontent.com".to_owned(),
            "synthetic-app-value".to_owned()
        )
    );
    assert_eq!(fs::read(&path).unwrap(), before);

    // Windows ACL hook failing closed rejects even a valid file without leaking secret.
    let denied = du::antigravity_oauth_client_with(home, Some(&|_| false)).unwrap_err();
    assert_eq!(denied.status, "unavailable");
    assert!(!denied.message.contains("synthetic-app-value"));

    // Shared 0o644 and symlink rejected on Unix.
    #[cfg(unix)]
    {
        use std::os::unix::fs::{PermissionsExt, symlink};
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert!(du::antigravity_oauth_client(home).is_err());
        fs::remove_file(&path).unwrap();
        let target = home.join("outside.json");
        fs::write(&target, meta.to_string()).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        symlink(&target, &path).unwrap();
        assert!(du::antigravity_oauth_client(home).is_err());
        fs::remove_file(&path).unwrap();
    }

    // Malformed payloads rejected safely.
    for bad in [
        json!({}),
        json!({"schemaVersion": true, "clientId": "fixture-client.apps.googleusercontent.com", "clientSecret": "synthetic-app-value"}),
        json!({"schemaVersion": 1, "clientId": "fixture-client.apps.googleusercontent.com", "clientSecret": "synthetic-app-value", "token": "private"}),
        json!({"schemaVersion": 1, "clientId": "https://other.example", "clientSecret": "synthetic-app-value"}),
        json!({"schemaVersion": 1, "clientId": "fixture-client.apps.googleusercontent.com", "clientSecret": "bad secret"}),
    ] {
        write_oauth_metadata(home, &bad, 0o600);
        let err = du::antigravity_oauth_client(home).unwrap_err();
        assert!(!err.message.contains("bad secret"));
    }

    // Refresh keeps original immutable and uses fixed Google route.
    write_oauth_metadata(home, &meta, 0o600);
    let transport =
        QueueDesktopTransport::new(vec![Ok(json!({"access_token": "renewed-fixture"}))]);
    let original = Credential {
        access: "old-access".into(),
        source: "Antigravity CLI".into(),
        refresh: "synthetic-refresh".into(),
        expires: None,
        email: Some("example@example.com".into()),
        project: None,
        plan: None,
        google_client: None,
    };
    let renewed = du::refresh_antigravity(&original, home, &transport, None)
        .await
        .unwrap();
    let calls = transport.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].url, "https://oauth2.googleapis.com/token");
    assert!(calls[0].form);
    assert_eq!(
        calls[0].body.as_ref().unwrap()["client_id"],
        "fixture-client.apps.googleusercontent.com"
    );
    assert_eq!(renewed.access, "renewed-fixture");
    assert_eq!(original.access, "old-access");
    assert_eq!(renewed.refresh, original.refresh);
    assert_eq!(renewed.email, original.email);

    // 401 refreshes at most once.
    let retry_transport = QueueDesktopTransport::new(vec![
        Err(UsageError::new("needs_sign_in", "Auth failed.")),
        Ok(json!({"access_token": "renewed-access"})),
        Err(UsageError::new("needs_sign_in", "Auth failed.")),
    ]);
    let mut sample = du::account("antigravity", "ubuntu");
    let env = DesktopEnv {
        now_secs: Some(NOW_SECS),
        desktop_transport: Some(&retry_transport),
        ..Default::default()
    };
    let err = du::fetch_antigravity(&original, &mut sample, home, &env)
        .await
        .unwrap_err();
    assert_eq!(err.status, "needs_sign_in");
    assert_eq!(retry_transport.calls().len(), 3);
}

#[tokio::test]
async fn antigravity_two_services_failover_throttles_budgets_and_neon_derivation() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let cred = Credential {
        access: "fixture-access".into(),
        source: "Antigravity CLI".into(),
        refresh: String::new(),
        expires: None,
        email: None,
        project: None,
        plan: None,
        google_client: None,
    };

    // 1. Both CLI and app services remain separate (CLI first, cli- prefix).
    let router = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            Ok(cli_summary_pro()),
        ),
        (
            format!("{AGY_BASE}retrieveUserQuotaSummary"),
            Ok(app_summary_pro()),
        ),
    ]);
    let mut result = du::account("antigravity", "ubuntu");
    let env = DesktopEnv {
        desktop_transport: Some(&router),
        ..Default::default()
    };
    du::fetch_antigravity_quota(&cred, &mut result, &env)
        .await
        .unwrap();
    let keys: Vec<_> = result["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["key"].as_str().unwrap())
        .collect();
    assert_eq!(
        keys,
        vec![
            "cli-gemini-weekly",
            "cli-gemini-5h",
            "cli-3p-weekly",
            "cli-3p-5h",
            "cli-gemini-argon-wtu-weekly",
            "gemini-weekly",
            "gemini-5h",
            "3p-weekly",
            "3p-5h",
            "gemini-argon-wtu-weekly"
        ]
    );
    let by: BTreeMap<_, _> = result["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| (w["key"].as_str().unwrap().to_owned(), w.clone()))
        .collect();
    assert!((by["cli-gemini-5h"]["usedPercent"].as_f64().unwrap() - 29.68).abs() < 0.01);
    assert!((by["gemini-weekly"]["usedPercent"].as_f64().unwrap() - 0.09).abs() < 0.01);
    assert_eq!(
        by["cli-gemini-5h"]["label"],
        "Gemini Models · 5-hour · In T3 (Antigravity CLI)"
    );
    assert_eq!(
        by["gemini-5h"]["label"],
        "Gemini Models · 5-hour · In the Antigravity app"
    );
    assert_eq!(result["plan"], "Google AI Pro");

    // 2. One service failing produces unavailable pools while the other still shows.
    let down = Err(UsageError::new("error", "down"));
    let router_one_down = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            down.clone(),
        ),
        (
            format!("{AGY_BASE}retrieveUserQuotaSummary"),
            Ok(app_summary_pro()),
        ),
    ]);
    let mut r2 = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r2,
        &DesktopEnv {
            desktop_transport: Some(&router_one_down),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r2["windows"][0]["status"], "unavailable");
    assert!(r2["windows"][0]["usedPercent"].is_null());
    assert!(r2["windows"][9].get("status").is_none());

    // 3. 429 on first service raises immediately without asking second; 429 on second keeps first + retryAfterSeconds.
    let throttled = UsageError {
        status: "error",
        message: "Throttled.",
        failure_code: Some("rate_limited"),
        retry_after: Some(321),
    };
    let router_429_first = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            Err(throttled),
        ),
    ]);
    let mut r3 = du::account("antigravity", "ubuntu");
    let caught = du::fetch_antigravity_quota(
        &cred,
        &mut r3,
        &DesktopEnv {
            desktop_transport: Some(&router_429_first),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(caught.failure_code, Some("rate_limited"));
    assert_eq!(caught.retry_after, Some(321));

    let router_429_second = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            Ok(cli_summary_pro()),
        ),
        (
            format!("{AGY_BASE}retrieveUserQuotaSummary"),
            Err(UsageError {
                status: "error",
                message: "Throttled.",
                failure_code: Some("rate_limited"),
                retry_after: Some(654),
            }),
        ),
    ]);
    let mut r4 = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r4,
        &DesktopEnv {
            desktop_transport: Some(&router_429_second),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r4["retryAfterSeconds"], 654);
    assert_eq!(r4["windows"][5]["status"], "unavailable");

    // 4. Both required summaries run before optional per-model read inside one deadline (<= 22s).
    let no_gemini = json!({"groups": [cli_summary_pro()["groups"][1]]});
    let timed = RouterDesktopTransport::timed(
        4.0,
        vec![
            (format!("{AGY_BASE}loadCodeAssist"), 5.0),
            (format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"), 3.0),
            (format!("{AGY_CLI_BASE}retrieveUserQuota"), 8.0),
            (format!("{AGY_BASE}retrieveUserQuotaSummary"), 8.0),
        ],
        vec![
            (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
            (
                format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
                Ok(no_gemini.clone()),
            ),
            (
                format!("{AGY_BASE}retrieveUserQuotaSummary"),
                Ok(app_summary_pro()),
            ),
        ],
    );
    let mut r5 = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r5,
        &DesktopEnv {
            desktop_transport: Some(&timed),
            elapsed: Some(&|| timed.elapsed_now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        timed.call_urls(),
        vec![
            format!("{AGY_BASE}loadCodeAssist"),
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            format!("{AGY_BASE}retrieveUserQuotaSummary"),
        ]
    );
    assert!(timed.elapsed_now() <= 22.0);

    // 5. Optional read boundary is inclusive at AGY_OPTIONAL_BUDGET_SECONDS (13.0).
    let single_app: [(&str, &str); 1] = [("app", AGY_BASE)];
    let inclusive = QueueDesktopTransport::new(vec![
        Ok(load_pro()),
        Ok(
            json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 0.7, "resetTime": "2026-10-25T20:42:30Z"}]}]}),
        ),
        Ok(
            json!({"buckets": [{"modelId": "gemini-3.8-flash", "remainingFraction": 0.55, "resetTime": "2026-10-25T20:42:30Z"}]}),
        ),
    ]);
    let mut r6 = du::account("antigravity", "ubuntu");
    du::fetch_antigravity(
        &cred,
        &mut r6,
        home,
        &DesktopEnv {
            desktop_transport: Some(&inclusive),
            elapsed: Some(&|| AGY_OPTIONAL_BUDGET_SECONDS),
            agy_services: Some(&single_app),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(inclusive.calls().len(), 3);
    assert_eq!(r6["windows"][1]["key"], "gemini-5h");

    // 6. Neon models join CLI Gemini Models window without creating a Neon pool, and loadCodeAssist falls back to CLI.
    let neon_per_model = json!({"buckets": [
        {"modelId": "gemini-3.8-flash-high", "remainingFraction": 1, "resetTime": "2026-11-20T03:06:23Z"},
        {"modelId": "gemini-4-neon-xhigh", "remainingFraction": 0.7032202, "resetTime": "2026-11-20T03:06:23Z"},
        {"modelId": "gemini-4-neon-high", "remainingFraction": 0.7032202, "resetTime": "2026-11-20T03:06:23Z"}
    ]});
    let neon_router = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), down),
        (format!("{AGY_CLI_BASE}loadCodeAssist"), Ok(load_pro())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            Ok(no_gemini),
        ),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuota"),
            Ok(neon_per_model),
        ),
        (
            format!("{AGY_BASE}retrieveUserQuotaSummary"),
            Ok(app_summary_pro()),
        ),
    ]);
    let mut r7 = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r7,
        &DesktopEnv {
            desktop_transport: Some(&neon_router),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let cli_gemini = r7["windows"]
        .as_array()
        .unwrap()
        .iter()
        .find(|w| {
            w["key"].as_str().unwrap().starts_with("cli-")
                && w["label"].as_str().unwrap().contains("Gemini")
                && !w["label"].as_str().unwrap().contains("Argon")
        })
        .expect("derived cli gemini window");
    assert!((cli_gemini["usedPercent"].as_f64().unwrap() - 29.68).abs() < 0.01);
    assert_eq!(
        cli_gemini["modelIds"],
        json!([
            "gemini-3.8-flash-high",
            "gemini-4-neon-high",
            "gemini-4-neon-xhigh"
        ])
    );
    assert!(!r7["windows"].as_array().unwrap().iter().any(|w| {
        w["key"]
            .as_str()
            .unwrap_or("")
            .to_lowercase()
            .contains("neon")
            || w["label"].as_str().unwrap_or("").contains("Neon")
    }));

    // 7. Free tier shows zero on both services, weekly only.
    let free_router = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), Ok(load_free())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            Ok(free_summary()),
        ),
        (
            format!("{AGY_BASE}retrieveUserQuotaSummary"),
            Ok(free_summary()),
        ),
    ]);
    let mut r8 = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r8,
        &DesktopEnv {
            desktop_transport: Some(&free_router),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let summary_tuples: Vec<_> = r8["windows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| {
            (
                w["key"].as_str().unwrap().to_owned(),
                w["usedPercent"].as_f64().unwrap(),
                w["windowMinutes"].as_f64().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        summary_tuples,
        vec![
            ("cli-gemini-weekly".into(), 0.0, 10080.0),
            ("cli-3p-weekly".into(), 0.0, 10080.0),
            ("gemini-weekly".into(), 0.0, 10080.0),
            ("3p-weekly".into(), 0.0, 10080.0),
        ]
    );
}

// =========================================================================
// 5. Standalone desktop_usage::collect for Muse, Antigravity, Cursor
// =========================================================================

#[tokio::test]
async fn standalone_muse_collect_contract_and_portal_capsule_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let env_map = test_env_map(home);

    // 1. Unavailable 15-key contract when no credential exists.
    let empty = du::collect_with(
        "muse",
        "ubuntu",
        home,
        &DesktopEnv {
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(empty["status"], "unavailable");
    assert_eq!(empty["windows"], json!([]));
    let keys: Vec<_> = empty.as_object().unwrap().keys().cloned().collect();
    for required in [
        "id",
        "provider",
        "providerLabel",
        "label",
        "email",
        "plan",
        "platform",
        "source",
        "status",
        "message",
        "fetchedAt",
        "sampledAt",
        "isActive",
        "windows",
        "capabilities",
    ] {
        assert!(keys.iter().any(|k| k == required), "missing key {required}");
    }
    assert_eq!(keys.len(), 15);

    // 2. Active Muse with omitted usage is unavailable (not needs_sign_in) and never leaks api_key.
    let muse_cred = Credential {
        access: "dca:private-fixture".into(),
        source: "Muse CLI".into(),
        refresh: String::new(),
        expires: None,
        email: None,
        project: None,
        plan: None,
        google_client: None,
    };
    let transport = QueueDesktopTransport::new(vec![Ok(json!({
        "user_email": "example@example.com",
        "subs_tier_name": "High Usage",
        "is_subs_active": true,
        "api_key": "sk-never-export"
    }))]);
    let omitted = du::collect_with(
        "muse",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            env_vars: Some(&env_map),
            desktop_transport: Some(&transport),
            credential: Some(muse_cred.clone()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(omitted["status"], "unavailable");
    assert_eq!(omitted["email"], "example@example.com");
    assert_eq!(omitted["plan"], "High Usage");
    assert!(omitted["message"].as_str().unwrap().contains("omitted"));
    assert_eq!(omitted["windows"], json!([]));
    assert!(!omitted.to_string().contains("never-export"));
    assert!(!omitted.to_string().contains("private-fixture"));

    // 3. Successful Muse native response + capsule fallback with portal.
    let ok_transport = QueueDesktopTransport::new(vec![Ok(json!({
        "api_key": "sk-never-export",
        "user_email": "example@example.com",
        "subs_tier_name": "High Usage",
        "subs_usage": {"weekly": {"used_percent": 6, "resets_at": 1790812800}}
    }))]);
    let ok = du::collect_with(
        "muse",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            env_vars: Some(&env_map),
            desktop_transport: Some(&ok_transport),
            credential: Some(muse_cred.clone()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(ok["status"], "ok");
    assert_eq!(ok["email"], "example@example.com");
    assert_eq!(ok["windows"][0]["usedPercent"], 6.0);
    assert!(!ok.to_string().contains("never-export"));

    // 4. Capsule-first path appends " + authenticated Meta web quota" and avoids native key mint when cached.
    let capsule_dir = home.join(".ccs/account-usage");
    fs::create_dir_all(&capsule_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&capsule_dir, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let capsule_path = capsule_dir.join("muse-console-session.json");
    fs::write(
        &capsule_path,
        json!({
            "schemaVersion": 1,
            "provider": "muse",
            "origin": "https://dev.meta.ai",
            "email": "example@example.com",
            "plan": "High Usage",
            "teamId": "42",
            "cookies": [
                {"name": "llama_dev_sess", "value": "sess-val", "domain": ".dev.meta.ai", "path": "/", "secure": true, "hostOnly": false, "expirationDate": NOW_SECS + 3600.0},
                {"name": "llm_sess", "value": "llm-val", "domain": ".dev.meta.ai", "path": "/", "secure": true, "hostOnly": false, "expirationDate": NOW_SECS + 3600.0}
            ]
        })
        .to_string(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&capsule_path, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let portal = MockPortal::new(
        "example@example.com",
        "42",
        json!({
            "tier": "High Usage",
            "window_weighted_used": "250",
            "window_weighted_limit": "1000",
            "window_duration_secs": 18000,
            "window_resets_at": "1792022400",
            "weekly_weighted_used": "500",
            "weekly_weighted_limit": "5000",
            "weekly_resets_at": "1792540800"
        }),
    );
    let verify_transport = QueueDesktopTransport::new(vec![Ok(json!({
        "user_email": "example@example.com",
        "subs_tier_name": "High Usage",
        "is_subs_active": true
    }))]);
    let mut muse_with_email = muse_cred;
    muse_with_email.email = Some("example@example.com".into());
    let web = du::collect_with(
        "muse",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            env_vars: Some(&env_map),
            desktop_transport: Some(&verify_transport),
            portal_transport: Some(&portal),
            credential: Some(muse_with_email),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(web["status"], "ok");
    assert!(
        web["source"]
            .as_str()
            .unwrap()
            .ends_with(" + authenticated Meta web quota")
    );
    assert_eq!(web["windows"][0]["usedPercent"], 25.0);
}

// =========================================================================
// 6. Cursor native + failover parity & cursor_failure_contract_fixture
// =========================================================================

#[tokio::test]
async fn cursor_native_and_failover_parity_all_plans_edge_cases_and_identity_conflict() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    // 1. cursor_cookie encodes subject and rejects non-JWT with UsageError.
    let cookie = du::cursor_cookie(&cursor_cred(None)).unwrap();
    assert!(cookie.starts_with("WorkosCursorSessionToken=fixture%3A%3A"));
    let bad_cookie = du::cursor_cookie(&Credential {
        access: "not-a-jwt".into(),
        source: "Fixture".into(),
        refresh: String::new(),
        expires: None,
        email: None,
        project: None,
        plan: None,
        google_client: None,
    })
    .unwrap_err();
    assert_eq!(bad_cookie.status, "needs_sign_in");

    // 2. normalize_legacy falsy billingCycleEnd falls through to endOfMonth, and float period 2.0 accepted.
    let legacy = du::cursor_normalize_legacy(&json!({
        "billingCycleEnd": 0,
        "endOfMonth": "2026-10-01T00:00:00Z",
        "individualUsage": {"plan": {"used": 667, "limit": 2000}}
    }));
    assert_eq!(legacy[0]["resetAt"], "2026-10-01T00:00:00Z");
    let float_period = du::cursor_normalize_shared(
        &cursor_current(json!(12.3456)),
        &cursor_plan_info(json!({"includedUsagePeriod": 2.0})),
        "premium",
    );
    assert_eq!(float_period[0]["key"], "grok-shared-weekly");
    assert_eq!(float_period[0]["resetAt"], RESET_ISO);

    // 3. Native desktop_usage::collect("cursor") and failover_additional::collect_with match identically.
    let replies = || {
        vec![
            Ok(json!({"membershipType": "premium"})),
            Ok(cursor_current(json!(0))),
            Ok(cursor_plan_info(json!({}))),
            Ok(json!({})),
            Ok(json!({})),
        ]
    };
    let native_client = QueuePlanTransport::json_replies(replies());
    let native = du::collect_with(
        "cursor",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            plan_transport: Some(&native_client),
            credential: Some(cursor_cred(Some("ultra"))),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(native["status"], "ok");
    assert_eq!(native["observedPlan"], "premium");
    assert_eq!(native["observedPlanAt"], NOW_ISO);

    let fo_client = QueuePlanTransport::json_replies(replies());
    let fo_candidate = json!({
        "_provider": "cursor",
        "_credential": {
            "access": cursor_cred(Some("ultra")).access,
            "source": "Fixture",
            "refresh": "",
            "expires": null,
            "email": "one@example.test",
            "project": null,
            "plan": "ultra",
            "google_client": null
        },
        "email": "one@example.test",
        "plan": "ultra"
    });
    let failover = additional::collect_with(
        &fo_candidate,
        home,
        "ubuntu",
        None,
        &AdditionalEnv {
            now_secs: Some(NOW_SECS),
            plan_transport: Some(&fo_client),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(native["windows"], failover["sample"]["windows"]);
    assert_eq!(native["windows"][0]["usedPercent"], 0.0);

    // 4. Plan observation survives primary quota failure on native desktop collector.
    let fail_client = QueuePlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "premium"})),
        Err(CollectionError {
            status: "error",
            message: "Provider unavailable",
            failure_code: None,
            retry_after: None,
        }),
    ]);
    let failed_native = du::collect_with(
        "cursor",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            plan_transport: Some(&fail_client),
            credential: Some(cursor_cred(Some("ultra"))),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(failed_native["status"], "unavailable");
    assert_eq!(failed_native["windows"], json!([]));
    assert_eq!(failed_native["observedPlan"], "premium");
    assert_eq!(failed_native["observedPlanAt"], NOW_ISO);

    // 5. Identity conflict discards plan and quota on native desktop collector.
    let mismatch_client = QueuePlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "premium"})),
        Ok(cursor_current(json!(12.3456))),
        Ok(cursor_plan_info(json!({}))),
        Ok(cursor_summary()),
        Ok(json!({"email": "other@example.test"})),
    ]);
    let mismatch_native = du::collect_with(
        "cursor",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            plan_transport: Some(&mismatch_client),
            credential: Some(cursor_cred(Some("ultra"))),
            ..Default::default()
        },
    )
    .await;
    assert!(mismatch_native["plan"].is_null());
    assert_eq!(mismatch_native["windows"], json!([]));
    assert!(mismatch_native.get("observedPlan").is_none());
    assert_eq!(mismatch_native["failureCode"], "account_mismatch");

    // 6. All 4 new plans verify Connect headers (Authorization, Connect-Protocol-Version, POST "{}") vs Cookie GET.
    let hdr_client = QueuePlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "plus"})),
        Ok(cursor_current(json!(12.3456))),
        Ok(cursor_plan_info(json!({}))),
        Ok(cursor_summary()),
        Ok(json!({})),
    ]);
    let plus = du::collect_with(
        "cursor",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            plan_transport: Some(&hdr_client),
            credential: Some(cursor_cred(Some("ultra"))),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(plus["windows"][0]["label"], "Grok");
    assert_eq!(
        hdr_client.call_urls(),
        vec![
            CURSOR_MEMBERSHIP_URL,
            CURSOR_CURRENT_USAGE_URL,
            CURSOR_PLAN_INFO_URL,
            CURSOR_SUMMARY_URL,
            CURSOR_PROFILE_URL,
        ]
    );
    let calls = hdr_client.calls();
    assert_eq!(calls[1].2.as_deref(), Some(b"{}".as_slice()));
    assert!(
        calls[1]
            .1
            .iter()
            .any(|(k, v)| k == "Connect-Protocol-Version" && v == "1")
    );
    assert!(
        !calls[1]
            .1
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("cookie"))
    );
    assert!(
        calls[0]
            .1
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("cookie"))
    );
    assert!(
        !calls[0]
            .1
            .iter()
            .any(|(k, _)| k.eq_ignore_ascii_case("authorization"))
    );
}

// =========================================================================
// 7. Real local TCP HTTP safety tests for ReqwestDesktopTransport
// =========================================================================

async fn spawn_one_shot_http(response: Vec<u8>) -> (String, tokio::task::JoinHandle<String>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 4096];
        let n = socket.read(&mut buf).await.unwrap_or(0);
        let req = String::from_utf8_lossy(&buf[..n]).into_owned();
        let _ = socket.write_all(&response).await;
        let _ = socket.shutdown().await;
        req
    });
    (format!("http://{addr}/test"), handle)
}

#[tokio::test]
async fn reqwest_desktop_transport_enforces_no_redirect_cap_429_401_and_400_invalid_grant() {
    let transport = ReqwestDesktopTransport::new(Some(1000.25)).unwrap();

    // 1. Preserves custom AGY_USER_AGENT and accepts JSON object.
    let (url, req_task) = spawn_one_shot_http(
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 13\r\n\r\n{\"groups\":[]}".to_vec(),
    )
    .await;
    let resp = transport
        .request_json(
            &url,
            &[("User-Agent".into(), AGY_USER_AGENT.into())],
            Some(&json!({})),
            false,
        )
        .await
        .unwrap();
    assert_eq!(resp, json!({"groups": []}));
    let raw_req = req_task.await.unwrap();
    assert!(
        raw_req
            .to_lowercase()
            .contains(&format!("user-agent: {}", AGY_USER_AGENT.to_lowercase()))
    );

    // 2. 302 redirect is refused (never followed) with exact desktop_helpers message.
    let (url302, _t) = spawn_one_shot_http(
        b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:1/other\r\nContent-Length: 0\r\n\r\n"
            .to_vec(),
    )
    .await;
    let err302 = transport
        .request_json(&url302, &[], None, false)
        .await
        .unwrap_err();
    assert_eq!(err302.status, "error");
    assert_eq!(
        err302.message,
        "The usage service returned an unexpected redirect."
    );

    // 3. 429 carries bounded integer retry_after without leaking upstream body.
    let (url429, _t) = spawn_one_shot_http(
        b"HTTP/1.1 429 Too Many Requests\r\nRetry-After: Thu, 01 Jan 1970 00:46:40 GMT\r\nContent-Length: 19\r\n\r\nsk-private-response"
            .to_vec(),
    )
    .await;
    let err429 = transport
        .request_json(&url429, &[], None, false)
        .await
        .unwrap_err();
    assert_eq!(err429.status, "error");
    assert_eq!(err429.failure_code, Some("rate_limited"));
    assert_eq!(err429.retry_after, Some(1800));
    assert!(!err429.message.contains("sk-private"));

    // 4. 401 -> needs_sign_in without echoing body.
    let (url401, _t) = spawn_one_shot_http(
        b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 14\r\n\r\nprivate-secret".to_vec(),
    )
    .await;
    let err401 = transport
        .request_json(&url401, &[], None, false)
        .await
        .unwrap_err();
    assert_eq!(err401.status, "needs_sign_in");
    assert!(!err401.message.contains("private-secret"));

    // 5. 400 invalid_grant -> needs_sign_in; other 400 and 500 invalid_grant -> error.
    let (url400_ig, _t) = spawn_one_shot_http(
        b"HTTP/1.1 400 Bad Request\r\nContent-Length: 62\r\n\r\n{\"error\":\"invalid_grant\",\"error_description\":\"ya29.FAKE-body\"}"
            .to_vec(),
    )
    .await;
    let err400_ig = transport
        .request_json(&url400_ig, &[], None, false)
        .await
        .unwrap_err();
    assert_eq!(err400_ig.status, "needs_sign_in");
    assert_eq!(
        err400_ig.message,
        "The saved account's sign-in has expired or was revoked."
    );
    assert!(!err400_ig.message.contains("ya29"));

    let (url500_ig, _t) = spawn_one_shot_http(
        b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 25\r\n\r\n{\"error\":\"invalid_grant\"}"
            .to_vec(),
    )
    .await;
    let err500_ig = transport
        .request_json(&url500_ig, &[], None, false)
        .await
        .unwrap_err();
    assert_eq!(err500_ig.status, "error");

    // 6. Oversized 200 (> 65536 bytes) refused with exact size-limit message.
    let big_body = format!("{{\"a\":\"{}\"}}", "x".repeat(66_000));
    let raw_big = format!(
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{}",
        big_body.len(),
        big_body
    )
    .into_bytes();
    let (url_big, _t) = spawn_one_shot_http(raw_big).await;
    let err_big = transport
        .request_json(&url_big, &[], None, false)
        .await
        .unwrap_err();
    assert_eq!(err_big.status, "error");
    assert_eq!(
        err_big.message,
        "The usage service response exceeded its size limit."
    );
}

#[tokio::test]
async fn antigravity_per_model_throttle_403_split_and_slow_summary_budget_skip() {
    let cred = Credential {
        access: "fixture-access".into(),
        source: "Antigravity CLI".into(),
        refresh: String::new(),
        expires: None,
        email: None,
        project: None,
        plan: None,
        google_client: None,
    };

    // 1. Optional per-model 429 preserves summary windows and attaches retryAfterSeconds.
    let single_app: [(&str, &str); 1] = [("app", AGY_BASE)];
    let per_model_429 = QueueDesktopTransport::new(vec![
        Ok(load_pro()),
        Ok(
            json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 0.7, "resetTime": "2026-10-25T20:42:30Z"}]}]}),
        ),
        Err(UsageError {
            status: "rate_limited",
            message: "Throttled.",
            failure_code: Some("rate_limited"),
            retry_after: Some(1800),
        }),
    ]);
    let mut r_pm = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r_pm,
        &DesktopEnv {
            desktop_transport: Some(&per_model_429),
            agy_services: Some(&single_app),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r_pm["windows"].as_array().unwrap().len(), 1);
    assert_eq!(r_pm["windows"][0]["key"], "3p-5h");
    assert_eq!(r_pm["retryAfterSeconds"], 1800);

    // 2. 403 on one service keeps the other; 403 on every service raises needs_sign_in.
    let refused = Err(UsageError::new(
        "needs_sign_in",
        "The saved account could not authenticate with the usage service.",
    ));
    let split_403 = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            refused.clone(),
        ),
        (
            format!("{AGY_BASE}retrieveUserQuotaSummary"),
            Ok(app_summary_pro()),
        ),
    ]);
    let mut r_split = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r_split,
        &DesktopEnv {
            desktop_transport: Some(&split_403),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(r_split["windows"][0]["status"], "unavailable");
    assert!(r_split["windows"][5].get("status").is_none());

    let both_403 = RouterDesktopTransport::new(vec![
        (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
        (
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            refused.clone(),
        ),
        (format!("{AGY_BASE}retrieveUserQuotaSummary"), refused),
    ]);
    let mut r_both = du::account("antigravity", "ubuntu");
    let err_both = du::fetch_antigravity_quota(
        &cred,
        &mut r_both,
        &DesktopEnv {
            desktop_transport: Some(&both_403),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(err_both.status, "needs_sign_in");

    // 3. Slow first summary (> 14 s) skips the second service without calling it.
    let slow = RouterDesktopTransport::timed(
        6.0,
        vec![
            (format!("{AGY_BASE}loadCodeAssist"), 5.0),
            (format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"), 8.0),
        ],
        vec![
            (format!("{AGY_BASE}loadCodeAssist"), Ok(load_pro())),
            (
                format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
                Ok(cli_summary_pro()),
            ),
        ],
    );
    let mut r_slow = du::account("antigravity", "ubuntu");
    du::fetch_antigravity_quota(
        &cred,
        &mut r_slow,
        &DesktopEnv {
            desktop_transport: Some(&slow),
            elapsed: Some(&|| slow.elapsed_now()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert!(
        !slow
            .call_urls()
            .contains(&format!("{AGY_BASE}retrieveUserQuotaSummary"))
    );
    assert!(slow.elapsed_now() <= 22.0);
    assert_eq!(r_slow["windows"][5]["status"], "unavailable");

    // 4. per_model=false skips retrieveUserQuota even when Gemini is absent.
    let no_gemini = json!({"groups": [cli_summary_pro()["groups"][1]]});
    let summary_only = QueueDesktopTransport::new(vec![Ok(no_gemini), Ok(app_summary_pro())]);
    let svcs = du::antigravity_services(
        &summary_only,
        &[("Authorization".into(), "Bearer fixture".into())],
        "project-fixture",
        &|| 0.0,
        false,
        None,
    )
    .await
    .unwrap();
    assert_eq!(summary_only.calls().len(), 2);
    assert_eq!(svcs.len(), 2);
}

#[test]
fn standalone_credentials_omp_and_auth_json_fallbacks_and_handles_table() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let env_map = test_env_map(home);

    // Cursor auth.json fallback (when state.vscdb absent)
    let auth_path = home.join(".config/Cursor/auth.json");
    fs::create_dir_all(auth_path.parent().unwrap()).unwrap();
    let tok =
        fake_jwt(json!({"sub": "auth0|u", "email": "authjson@example.test", "exp": 1_800_000_000}));
    fs::write(
        &auth_path,
        json!({"accessToken": tok, "refreshToken": "ref-1"}).to_string(),
    )
    .unwrap();
    let c = du::cursor_credentials(home, "ubuntu", Some(&env_map)).expect("auth.json cursor");
    assert_eq!(c.source, "Cursor desktop");
    assert_eq!(c.refresh, "ref-1");
    assert_eq!(c.email.as_deref(), Some("authjson@example.test"));
    fs::remove_file(&auth_path).unwrap();

    // OMP fallback for both cursor and google-antigravity
    let omp_path = home.join(".omp/agent/agent.db");
    fs::create_dir_all(omp_path.parent().unwrap()).unwrap();
    let conn = Connection::open(&omp_path).unwrap();
    conn.execute(
        "CREATE TABLE auth_credentials(provider TEXT,credential_type TEXT,data TEXT,disabled_cause TEXT)",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_credentials VALUES(?,?,?,?)",
        rusqlite::params![
            "cursor",
            "oauth",
            json!({"access": tok, "refresh": "omp-cur-ref", "email": "omp-cur@example.test"})
                .to_string(),
            Option::<String>::None
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO auth_credentials VALUES(?,?,?,?)",
        rusqlite::params![
            "google-antigravity",
            "oauth",
            json!({"access": "omp-agy-tok", "refresh": "omp-agy-ref", "projectId": "proj-1", "email": "omp-agy@example.test"}).to_string(),
            Option::<String>::None
        ],
    )
    .unwrap();
    drop(conn);

    let omp_cur = du::cursor_credentials(home, "ubuntu", Some(&env_map)).expect("omp cursor");
    assert_eq!(omp_cur.source, "OMP");
    assert_eq!(omp_cur.refresh, "omp-cur-ref");

    let omp_agy = du::antigravity_credentials(home).expect("omp agy");
    assert_eq!(omp_agy.source, "OMP");
    assert_eq!(omp_agy.project.as_deref(), Some("proj-1"));
    assert_eq!(omp_agy.google_client.as_deref(), Some("antigravity"));

    // handles() derives from aac-providers without accepting unknown/synthetic ids
    assert!(du::handles("antigravity"));
    assert!(du::handles("muse"));
    assert!(du::handles("cursor"));
    assert!(!du::handles("codex"));
    assert!(!du::handles("retired-provider"));
    assert!(!du::handles("zz-synthetic"));
}

#[tokio::test]
async fn antigravity_budget_spans_token_refresh_and_muse_mismatch_inactive_contracts() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let env_map = test_env_map(home);
    let meta = json!({
        "schemaVersion": 1,
        "clientId": "fixture-client.apps.googleusercontent.com",
        "clientSecret": "synthetic-app-value"
    });
    write_oauth_metadata(home, &meta, 0o600);

    // 1. Budget spans token refresh: elapsed=18s skips optional per-model read (calls == 3).
    let single_app: [(&str, &str); 1] = [("app", AGY_BASE)];
    let transport = QueueDesktopTransport::new(vec![
        Ok(json!({"access_token": "renewed-access", "expires_in": 3600})),
        Ok(json!({"cloudaicompanionProject": "project-fixture", "planInfo": {"planType": "pro"}})),
        Ok(
            json!({"groups": [{"displayName": "Claude and GPT models", "buckets": [
            {"bucketId": "3p-5h", "window": "5h", "remainingFraction": 0.7, "resetTime": "2026-10-25T20:42:30Z"}]}]}),
        ),
    ]);
    let expired_cred = Credential {
        access: "expired-access".into(),
        source: "OMP".into(),
        refresh: "fixture-refresh".into(),
        expires: Some(1.0),
        email: Some("example@example.com".into()),
        project: Some("project-fixture".into()),
        plan: None,
        google_client: None,
    };
    let mut r = du::account("antigravity", "ubuntu");
    du::fetch_antigravity(
        &expired_cred,
        &mut r,
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            desktop_transport: Some(&transport),
            elapsed: Some(&|| 18.0),
            agy_services: Some(&single_app),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let refresh_calls = transport.calls();
    assert_eq!(refresh_calls.len(), 3);
    assert!(
        refresh_calls[1]
            .headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer renewed-access")
    );
    assert_eq!(r["windows"].as_array().unwrap().len(), 1);
    assert_eq!(r["windows"][0]["key"], "3p-5h");
    assert_eq!(expired_cred.access, "expired-access");

    // 2. Muse account_mismatch & inactive_subscription on standalone desktop_usage::collect.
    let muse_cred = Credential {
        access: "dca:fixture".into(),
        source: "Muse CLI".into(),
        refresh: String::new(),
        expires: None,
        email: Some("expected@example.com".into()),
        project: None,
        plan: None,
        google_client: None,
    };
    let mismatch_t = QueueDesktopTransport::new(vec![Ok(json!({
        "user_email": "other@example.com",
        "subs_tier_name": "Pro",
        "is_subs_active": true,
        "subs_usage": {"weekly": {"used_percent": 5}}
    }))]);
    let mismatch = du::collect_with(
        "muse",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            env_vars: Some(&env_map),
            desktop_transport: Some(&mismatch_t),
            credential: Some(muse_cred.clone()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(mismatch["status"], "unavailable");
    assert_eq!(mismatch["failureCode"], "account_mismatch");
    assert_eq!(mismatch["windows"], json!([]));

    let inactive_t = QueueDesktopTransport::new(vec![Ok(json!({
        "user_email": "expected@example.com",
        "subs_tier_name": "Pro",
        "is_subs_active": false,
        "subs_usage": {"weekly": {"used_percent": 5}}
    }))]);
    let inactive = du::collect_with(
        "muse",
        "ubuntu",
        home,
        &DesktopEnv {
            now_secs: Some(NOW_SECS),
            env_vars: Some(&env_map),
            desktop_transport: Some(&inactive_t),
            credential: Some(muse_cred),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(inactive["status"], "unavailable");
    assert_eq!(inactive["failureCode"], "inactive_subscription");
    assert_eq!(inactive["windows"], json!([]));
}

/// Compare Rust numbers/values against Python JSON values (treating integer 0 == float 0.0).
fn assert_json_num_eq(actual: &Value, expected: &Value, label: &str) {
    match (actual, expected) {
        (Value::Number(a), Value::Number(b)) => {
            let af = a.as_f64().unwrap();
            let bf = b.as_f64().unwrap();
            assert!(
                (af - bf).abs() <= 1e-9,
                "{label}: number mismatch actual={actual} expected={expected}"
            );
        }
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(
                a.len(),
                b.len(),
                "{label}: len actual={actual} expected={expected}"
            );
            for (i, (av, bv)) in a.iter().zip(b.iter()).enumerate() {
                assert_json_num_eq(av, bv, &format!("{label}[{i}]"));
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            assert_eq!(
                a.len(),
                b.len(),
                "{label}: keys actual={actual} expected={expected}"
            );
            for (k, bv) in b {
                let av = a
                    .get(k)
                    .unwrap_or_else(|| panic!("{label}: missing key {k} in {actual}"));
                assert_json_num_eq(av, bv, &format!("{label}.{k}"));
            }
        }
        _ => assert_eq!(actual, expected, "{label}: mismatch"),
    }
}

#[test]
fn live_python_oracle_exact_parity_across_desktop_helpers_desktop_usage_and_cursor_usage() {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let dir = tempfile::tempdir().unwrap();
    let scripts = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/account-usage");

    let agy_summary_payload = json!({
        "response": {"summary": {"groups": [
            {"displayName": "Claude and GPT models", "buckets": [
                {"bucketId": "3p-weekly", "window": "weekly", "remainingFraction": 0.5, "resetTime": "2026-11-01T15:27:22Z"},
                {"bucketId": "3p-5h", "window": "5h", "remaining": {"case": "remainingFraction", "value": 0}, "resetTime": "2026-10-25T20:42:30Z"},
                {"bucketId": "dup", "remainingFraction": 1, "disabled": true},
                {"bucketId": "dup", "remainingAmount": "100"},
                {"bucketId": "invalid", "remainingFraction": 5, "resetTime": "2026-10-01T04:00:00Z"}
            ]},
            {"displayName": "Gemini 4 Argon", "buckets": [
                {"bucketId": "gemini-argon-wtu-weekly", "window": "weekly", "remainingFraction": 0.93, "resetTime": "2026-11-01T11:10:00Z"}
            ]}
        ]}}
    });
    let agy_per_model_payload = json!({
        "buckets": [
            {"modelId": "gemini-3.8-flash", "remainingFraction": 0.55, "resetTime": "2026-10-25T20:42:34.900Z"},
            {"modelId": "gemini-4-neon-xhigh", "remainingFraction": 0.4, "resetTime": "2026-10-25T20:42:36.100Z"},
            {"modelId": "gemini-4-argon-high", "remainingFraction": 0.1, "resetTime": "2026-10-25T20:42:36.100Z"},
            {"modelId": "gemini-3.1-pro", "resetTime": "2026-11-01T15:27:30Z"},
            {"modelId": "gemini-sk-task-1", "remainingFraction": 0.9, "resetTime": "2026-11-01T15:27:30Z"},
            {"modelId": "gemini-3.5-flash", "remainingAmount": "1200", "resetTime": "2026-10-26T09:00:00Z"}
        ]
    });
    let agy_load_payload = json!({
        "planInfo": {"planType": "pro"},
        "paidTier": {"id": "g1-pro-tier", "name": "Google AI Pro", "availableCredits": [
            {"creditAmount": "125.5"}, {"creditAmount": 20}, {"creditAmount": "1e1"}, {"creditAmount": true}
        ]},
        "currentTier": {"id": "free-tier", "name": "Antigravity"}
    });
    let muse_subs_payload = json!({
        "subs_usage": {
            "window": {"used_percent": 5.12345, "resets_at": 1790812800, "window_duration_mins": 240},
            "weekly": {"used_percent": 0, "resets_at": "2026-10-01T00:00:00-04:00", "window_duration_mins": null}
        }
    });
    let muse_portal_payload = json!({
        "subscription_quota": {
            "tier": "High Usage",
            "window_weighted_used": "0",
            "window_weighted_limit": "1000",
            "window_duration_secs": "5400",
            "window_resets_at": null,
            "weekly_weighted_used": "1200",
            "weekly_weighted_limit": "1000",
            "weekly_resets_at": "1790812800"
        }
    });
    let cursor_legacy_payload = json!({
        "billingCycleEnd": 0,
        "endOfMonth": "2026-10-01T00:00:00Z",
        "isUnlimited": true,
        "individualUsage": {
            "plan": {"used": 667, "limit": 2000, "remaining": 1333, "enabled": true, "totalPercentUsed": 1.4461, "autoPercentUsed": 0.36},
            "onDemand": {"used": 250, "limit": 2000, "remaining": 1750, "enabled": false}
        }
    });
    let cursor_shared_payload = cursor_current(json!(113.5));
    let cursor_shared_info = cursor_plan_info(json!({"includedUsagePeriod": 2.0}));

    let input = json!({
        "agySummary": agy_summary_payload,
        "agyPerModel": agy_per_model_payload,
        "agyLoad": agy_load_payload,
        "museSubs": muse_subs_payload,
        "musePortal": muse_portal_payload,
        "cursorLegacy": cursor_legacy_payload,
        "cursorShared": cursor_shared_payload,
        "cursorInfo": cursor_shared_info,
        "safeTexts": ["ok", "  trimmed  ", "\x1fhello", "hel\x1flo", "Bearer\u{00a0}tok", "sk-abc123", "eyJ12345678"],
        "memberships": ["Plus", "UNIFIED_ULTRA", "future_tier", "bad-tier", "", "pro_student"]
    });

    let py = r#"
import sys, json
sys.path.insert(0, sys.argv[1])
import desktop_helpers as h, desktop_usage as d, cursor_usage as c
inp = json.load(sys.stdin)
agy_sum = d.normalize_antigravity(inp["agySummary"])
plan, reported = d.antigravity_reported_plan(inp["agyLoad"])
out = {
    "agySummary": agy_sum,
    "agyHasGemini": d.antigravity_summary_has_gemini(agy_sum),
    "agyGemini": d.normalize_antigravity_gemini(inp["agyPerModel"], agy_sum),
    "agyPlan": plan,
    "agyReported": reported or None,
    "agyCredits": d.normalize_antigravity_credits(inp["agyLoad"]),
    "museSubs": d.normalize_muse(inp["museSubs"]),
    "musePortal": d.normalize_muse(inp["musePortal"]),
    "cursorLegacy": c.normalize_legacy(inp["cursorLegacy"]),
    "cursorShared": c.normalize_shared(inp["cursorShared"], inp["cursorInfo"], "plus"),
    "cursorAdditional": c.normalize_additional(inp["cursorLegacy"]),
    "safeTexts": [h.safe_text(s) for s in inp["safeTexts"]],
    "memberships": [c.membership(m) for m in inp["memberships"]],
}
print(json.dumps(out, allow_nan=False))
"#;
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(py)
        .arg(&scripts)
        .env("HOME", dir.path())
        .env("CCS_HOME", dir.path().join(".ccs"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn python3 oracle");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().expect("wait python3 oracle");
    assert!(
        output.status.success(),
        "python oracle failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected: Value = serde_json::from_slice(&output.stdout).expect("parse oracle JSON");

    let rust_agy_sum = du::normalize_antigravity(&input["agySummary"]);
    let (rust_plan, rust_reported) = du::antigravity_reported_plan(&input["agyLoad"]);
    let rust_out = json!({
        "agySummary": rust_agy_sum,
        "agyHasGemini": du::antigravity_summary_has_gemini(&rust_agy_sum),
        "agyGemini": du::normalize_antigravity_gemini(&input["agyPerModel"], &rust_agy_sum),
        "agyPlan": rust_plan,
        "agyReported": rust_reported,
        "agyCredits": du::normalize_antigravity_credits(&input["agyLoad"]),
        "museSubs": du::normalize_muse(&input["museSubs"]),
        "musePortal": du::normalize_muse(&input["musePortal"]),
        "cursorLegacy": du::cursor_normalize_legacy(&input["cursorLegacy"]),
        "cursorShared": du::cursor_normalize_shared(&input["cursorShared"], &input["cursorInfo"], "plus"),
        "cursorAdditional": du::cursor_normalize_additional(&input["cursorLegacy"]),
        "safeTexts": input["safeTexts"].as_array().unwrap().iter().map(|s| du::safe_text(s, 160)).collect::<Vec<_>>(),
        "memberships": input["memberships"].as_array().unwrap().iter().map(du::cursor_membership).collect::<Vec<_>>(),
    });
    assert_json_num_eq(&rust_out, &expected, "rust_vs_python_oracle");
}
