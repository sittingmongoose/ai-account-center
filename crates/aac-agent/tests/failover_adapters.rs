//! Offline parity tests for `failover_special` and `failover_additional`, derived directly from
//! `failover_special_test.py`, `failover_additional_test.py`, `cursor_usage_test.py`,
//! `muse_failover_budget_test.py`, and `opencode_go_failure_test.py`.
#![allow(clippy::unwrap_used)]

use aac_agent::{
    failover_additional::{
        self as additional, AdditionalEnv, CollectionError, PlanHttpBody, PlanTransport,
        PortalError, PortalTransport,
    },
    failover_special::{
        self as special, GoogleHttpError, GoogleTransport, SpecialEnv, WalletError, WalletTransport,
    },
};
use aac_core::sha256_hex;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, VecDeque},
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

const EMAIL: &str = "alice@example.test";
const SUBJECT: &str = "123456789";
const AGY_USERINFO_URL: &str = "https://www.googleapis.com/oauth2/v2/userinfo";
const AGY_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const AGY_APP_BASE: &str = "https://cloudcode-pa.googleapis.com/v1internal:";
const AGY_CLI_BASE: &str = "https://daily-cloudcode-pa.googleapis.com/v1internal:";

fn fake_jwt(email: &str, subject: &str, suffix: &str) -> String {
    let payload = json!({ "email": email, "sub": subject }).to_string();
    format!(
        "fixture.{}.{suffix}",
        URL_SAFE_NO_PAD.encode(payload.as_bytes())
    )
}

fn agy_identity_key(email: &str, subject: &str) -> String {
    let packed = json!(["antigravity", email.to_lowercase(), subject]).to_string();
    sha256_hex(packed.as_bytes())
}

fn agy_native_json(access: &str, expiry: &str, id_token: &str) -> Value {
    json!({
        "auth_method": "consumer",
        "token": {
            "access_token": access,
            "refresh_token": "fixture-private-refresh",
            "token_type": "Bearer",
            "expiry": expiry
        },
        "id_token": id_token
    })
}

fn write_private(root: &Path, relative: &str, bytes: &[u8]) -> PathBuf {
    let file = root.join(relative);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut cur = parent.to_path_buf();
            while cur != root && cur.starts_with(root) {
                fs::set_permissions(&cur, fs::Permissions::from_mode(0o700)).unwrap();
                if !cur.pop() {
                    break;
                }
            }
        }
    }
    fs::write(&file, bytes).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[cfg(windows)]
    private_acl(&file);
    file
}

/// Windows: the owner-only, protected ACL the product's private readers require (the Unix mode
/// bits above have no effect there).
#[cfg(windows)]
fn private_acl(path: &Path) {
    // The local account name resolves under ssh too (USERDOMAIN may read WORKGROUP there),
    // and icacls refuses verbatim \\?\ paths.
    let user = std::env::var("USERNAME").expect("USERNAME");
    let text = path.to_string_lossy();
    let plain = text.strip_prefix(r"\\?\").unwrap_or(&text);
    let status = std::process::Command::new("icacls")
        .arg(plain)
        .args(["/inheritance:r", "/grant:r", &format!("{user}:(F)")])
        .stdout(std::process::Stdio::null())
        .status()
        .expect("icacls");
    assert!(status.success(), "icacls {}", path.display());
}

fn write_saved_agy_profile(
    ccs: &Path,
    name: &str,
    email: &str,
    subject: &str,
    revision_num: u64,
) -> (PathBuf, Value) {
    let raw = agy_native_json(
        "fixture-private-access",
        "2099-01-01T00:00:00Z",
        &fake_jwt(email, subject, "signature"),
    )
    .to_string();
    let fingerprint = sha256_hex(raw.as_bytes());
    write_private(
        ccs,
        &format!("antigravity-instances/{name}/ubuntu/credential-{fingerprint}.bin"),
        raw.as_bytes(),
    );
    let record = json!({
        "id": name,
        "email": email,
        "identityKey": agy_identity_key(email, subject),
        "plan": "Pro",
        "credential": {
            "fingerprint": fingerprint,
            "format": "consumer-json",
            "identity": {
                "email": email,
                "subject": subject,
                "plan": "Pro",
                "verifiedAt": "2026-01-01T00:00:00Z",
                "source": "provider-userinfo"
            }
        }
    });
    let registry = write_private(
        ccs,
        &format!("antigravity-profiles/registry-{revision_num:012}.json"),
        json!({
            "version": 1,
            "revision": revision_num,
            "profiles": [record],
            "active": null,
            "transaction": null
        })
        .to_string()
        .as_bytes(),
    );
    (registry, record)
}

fn write_oauth_client(home: &Path) {
    write_private(
        home,
        ".ccs/account-usage/antigravity-oauth-client.json",
        json!({
            "schemaVersion": 1,
            "clientId": "fixture-client.apps.googleusercontent.com",
            "clientSecret": "fixture-secret-value"
        })
        .to_string()
        .as_bytes(),
    );
}

type OmpRow<'a> = (i64, &'a str, &'a str, Option<&'a str>, Option<&'a str>);

fn write_omp_db(home: &Path, rows: &[OmpRow<'_>]) -> PathBuf {
    let path = home.join(".omp/agent/agent.db");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = rusqlite::Connection::open(&path).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS auth_credentials (id INTEGER PRIMARY KEY, provider TEXT, credential_type TEXT, data TEXT, disabled_cause TEXT)",
        [],
    )
    .unwrap();
    for (id, provider, kind, data, disabled) in rows {
        conn.execute(
            "INSERT INTO auth_credentials VALUES (?1, ?2, ?3, ?4, ?5)",
            rusqlite::params![id, provider, kind, data, disabled],
        )
        .unwrap();
    }
    path
}

fn public_fields(candidate: &Value) -> Value {
    let mut map = serde_json::Map::new();
    for key in [
        "locator",
        "selectors",
        "identity",
        "revision",
        "email",
        "plan",
    ] {
        if let Some(v) = candidate.get(key) {
            map.insert(key.to_owned(), v.clone());
        }
    }
    Value::Object(map)
}

type HeaderList = Vec<(String, String)>;
type GoogleCalls = Arc<Mutex<Vec<(String, HeaderList)>>>;

struct MockGoogle {
    calls: GoogleCalls,
    replies: Arc<Mutex<VecDeque<Result<Value, GoogleHttpError>>>>,
}

impl MockGoogle {
    fn new(replies: Vec<Result<Value, GoogleHttpError>>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            replies: Arc::new(Mutex::new(VecDeque::from(replies))),
        }
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

impl GoogleTransport for MockGoogle {
    fn request_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        _body: Option<&'a Value>,
        _form: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value, GoogleHttpError>> + Send + 'a>> {
        self.calls
            .lock()
            .unwrap()
            .push((url.to_owned(), headers.to_vec()));
        let next = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected Google call to {url}"));
        Box::pin(async move { next })
    }
}

struct MockWallet {
    calls: Arc<Mutex<Vec<String>>>,
    replies: Arc<Mutex<BTreeMap<String, Result<Value, WalletError>>>>,
}

impl MockWallet {
    fn new(entries: Vec<(&str, Result<Value, WalletError>)>) -> Self {
        let mut map = BTreeMap::new();
        for (k, v) in entries {
            map.insert(k.to_owned(), v);
        }
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            replies: Arc::new(Mutex::new(map)),
        }
    }
}

impl WalletTransport for MockWallet {
    fn get<'a>(
        &'a self,
        route: &'a str,
        _workspace: Option<&'a str>,
    ) -> Pin<Box<dyn Future<Output = Result<Value, WalletError>> + Send + 'a>> {
        self.calls.lock().unwrap().push(route.to_owned());
        let next = self
            .replies
            .lock()
            .unwrap()
            .get(route)
            .cloned()
            .unwrap_or_else(|| Ok(json!({})));
        Box::pin(async move { next })
    }
}

type PlanCalls = Arc<Mutex<Vec<(String, HeaderList, Option<Vec<u8>>)>>>;

struct MockPlanTransport {
    calls: PlanCalls,
    replies: Arc<Mutex<VecDeque<Result<PlanHttpBody, CollectionError>>>>,
}

impl MockPlanTransport {
    fn json_replies(replies: Vec<Result<Value, CollectionError>>) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            replies: Arc::new(Mutex::new(
                replies
                    .into_iter()
                    .map(|r| r.map(PlanHttpBody::Json))
                    .collect(),
            )),
        }
    }

    fn call_count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl PlanTransport for MockPlanTransport {
    fn request<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a [u8]>,
        _text: bool,
    ) -> Pin<Box<dyn Future<Output = Result<PlanHttpBody, CollectionError>> + Send + 'a>> {
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
            .unwrap_or_else(|| panic!("unexpected plan HTTP call to {url}"));
        Box::pin(async move { next })
    }
}

struct MockPortal {
    calls: Arc<Mutex<Vec<String>>>,
    email: String,
    team_id: String,
    quota: Arc<Mutex<Value>>,
    quota_error: Arc<Mutex<Option<PortalError>>>,
}

impl MockPortal {
    fn new(email: &str, team_id: &str, quota: Value) -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            email: email.to_owned(),
            team_id: team_id.to_owned(),
            quota: Arc::new(Mutex::new(quota)),
            quota_error: Arc::new(Mutex::new(None)),
        }
    }
}

impl PortalTransport for MockPortal {
    fn get<'a>(
        &'a self,
        route: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<Value, PortalError>> + Send + 'a>> {
        self.calls.lock().unwrap().push(route.to_owned());
        let reply = if route == "/api/auth/me" {
            Ok(json!({ "email": self.email, "userId": "fixture-user" }))
        } else if route == "/api/portal/teams" {
            Ok(json!({ "teams": [{ "team_id": self.team_id.parse::<u64>().unwrap() }] }))
        } else if let Some(err) = self.quota_error.lock().unwrap().clone() {
            Err(err)
        } else {
            Ok(json!({ "subscription_quota": self.quota.lock().unwrap().clone() }))
        };
        Box::pin(async move { reply })
    }
}

fn agy_summary(five: f64, weekly: f64) -> Value {
    json!({
        "groups": [{
            "displayName": "Gemini Models",
            "buckets": [
                {
                    "bucketId": "gemini-weekly",
                    "window": "weekly",
                    "remainingFraction": weekly,
                    "resetTime": "2026-11-26T22:06:23Z"
                },
                {
                    "bucketId": "gemini-5h",
                    "window": "5h",
                    "remainingFraction": five,
                    "resetTime": "2026-11-20T03:06:23Z"
                }
            ]
        }]
    })
}

fn test_env(home: &Path) -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    map.insert(
        "XDG_CONFIG_HOME".into(),
        home.join(".config").to_string_lossy().into_owned(),
    );
    map
}

// =========================================================================
// failover_special tests
// =========================================================================

#[test]
fn special_handles_exact_providers_and_rejects_unknown_or_synthetic() {
    assert!(special::handles("antigravity"));
    assert!(special::handles("opencode-wallet"));
    assert!(!special::handles("codex"));
    assert!(!special::handles("muse"));
    assert!(!special::handles("zz-synthetic"));
    assert!(!special::handles("retired-provider"));
}

#[test]
fn special_allowed_google_urls_and_retry_after_bounds() {
    assert!(special::allowed_google_url(AGY_USERINFO_URL));
    assert!(special::allowed_google_url(AGY_TOKEN_URL));
    assert!(special::allowed_google_url(&format!(
        "{AGY_APP_BASE}loadCodeAssist"
    )));
    assert!(special::allowed_google_url(&format!(
        "{AGY_APP_BASE}retrieveUserQuotaSummary"
    )));
    assert!(special::allowed_google_url(&format!(
        "{AGY_CLI_BASE}loadCodeAssist"
    )));
    assert!(special::allowed_google_url(&format!(
        "{AGY_CLI_BASE}retrieveUserQuotaSummary"
    )));
    assert!(!special::allowed_google_url(&format!(
        "{AGY_CLI_BASE}retrieveUserQuota"
    )));
    assert!(!special::allowed_google_url(
        "https://unrelated.example.test"
    ));

    // 2026-10-14T12:00:00Z = 1791979200
    let now = 1_791_979_200.0;
    assert_eq!(
        special::retry_after_seconds(Some("Wed, 14 Oct 2026 12:10:00 GMT"), now),
        600
    );
    assert_eq!(
        special::retry_after_seconds(Some("999999999999"), now),
        86400
    );
    assert_eq!(special::retry_after_seconds(Some("0"), now), 1);
    assert_eq!(special::retry_after_seconds(None, now), 60);
    assert_eq!(special::retry_after_seconds(Some("invalid"), now), 60);
}

#[test]
fn special_saved_profile_read_failure_attribution_and_corrupt_registry() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join(".ccs");

    let (_, record) = write_saved_agy_profile(&ccs, "first", EMAIL, SUBJECT, 1);
    let fp = record["credential"]["fingerprint"].as_str().unwrap();
    let cred_path = ccs.join(format!(
        "antigravity-instances/first/ubuntu/credential-{fp}.bin"
    ));
    fs::write(&cred_path, b"{damaged").unwrap();

    let inv = special::inventory("antigravity", &home, "ubuntu", Some(&ccs));
    assert!(inv.candidates.is_empty());
    assert_eq!(inv.incomplete_selectors, vec!["antigravity-profile:first"]);
    assert!(inv.read_failures > 0);

    // Malformed registry marks the profile family.
    let dir2 = tempfile::tempdir().unwrap();
    let home2 = dir2.path().canonicalize().unwrap();
    let ccs2 = home2.join(".ccs");
    write_private(
        &ccs2,
        "antigravity-profiles/registry-000000000001.json",
        json!({"version": 1, "revision": 1, "profiles": "damaged", "active": null, "transaction": null})
            .to_string()
            .as_bytes(),
    );
    let inv2 = special::inventory("antigravity", &home2, "ubuntu", Some(&ccs2));
    assert!(inv2.candidates.is_empty());
    assert_eq!(inv2.incomplete_selectors, vec!["antigravity-profile:*"]);
}

#[test]
fn special_native_locator_revision_explicit_scope_and_omp_rows() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join(".ccs");

    // Missing sources return empty without read failures.
    let empty = special::inventory("antigravity", &home, "ubuntu", Some(&ccs));
    assert!(empty.candidates.is_empty());
    assert_eq!(empty.read_failures, 0);

    // Native without strong identity is excluded.
    write_private(
        &home,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json("fixture-private-access", "2099-01-01T00:00:00Z", "")
            .to_string()
            .as_bytes(),
    );
    assert!(
        special::inventory("antigravity", &home, "ubuntu", Some(&ccs))
            .candidates
            .is_empty()
    );

    // Native with identity: locator stays constant across token rotation, revision changes.
    let token_path = write_private(
        &home,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json(
            "fixture-private-access",
            "2099-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "sig"),
        )
        .to_string()
        .as_bytes(),
    );
    let first =
        special::inventory("antigravity", &home, "windows", Some(&ccs)).candidates[0].clone();
    write_private(
        &home,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json(
            "replacement-private-access",
            "2099-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "sig"),
        )
        .to_string()
        .as_bytes(),
    );
    let second =
        special::inventory("antigravity", &home, "windows", Some(&ccs)).candidates[0].clone();
    assert_eq!(first["locator"], second["locator"]);
    assert_ne!(first["revision"], second["revision"]);
    assert_eq!(
        first["identity"],
        json!({"kind": "antigravity-subject", "key": agy_identity_key(EMAIL, SUBJECT)})
    );
    assert_eq!(first["selectors"], json!(["discover"]));
    let pub_text = public_fields(&first).to_string();
    for secret in [
        "fixture-private",
        token_path.to_string_lossy().as_ref(),
        SUBJECT,
        "id_token",
    ] {
        assert!(!pub_text.contains(secret), "leaked {secret}: {pub_text}");
    }

    // Explicit ccs scope enumerates saved profiles without mutation.
    let alt = home.join("isolated-aac-scope");
    let (reg, _) = write_saved_agy_profile(&alt, "saved", EMAIL, SUBJECT, 1);
    let before = fs::read(&reg).unwrap();
    let rows = special::inventory("antigravity", &home, "ubuntu", Some(&alt)).candidates;
    assert!(
        rows.iter()
            .any(|r| r["selectors"] == json!(["antigravity-profile:saved"]))
    );
    assert_eq!(fs::read(&reg).unwrap(), before);

    // OMP rows: enabled rows separate, disabled skipped, unusable rows mark discover incomplete.
    let dir_omp = tempfile::tempdir().unwrap();
    let home_omp = dir_omp.path().canonicalize().unwrap();
    let row1 =
        json!({"access": "private-omp-1", "email": EMAIL, "id_token": fake_jwt(EMAIL, "1", "s")})
            .to_string();
    let row2 =
        json!({"access": "private-omp-2", "email": EMAIL, "id_token": fake_jwt(EMAIL, "2", "s")})
            .to_string();
    let row3 =
        json!({"access": "private-omp-3", "email": EMAIL, "id_token": fake_jwt(EMAIL, "3", "s")})
            .to_string();
    write_omp_db(
        &home_omp,
        &[
            (1, "google-antigravity", "oauth", Some(&row1), None),
            (2, "google-antigravity", "oauth", Some(&row2), None),
            (
                3,
                "google-antigravity",
                "oauth",
                Some(&row3),
                Some("revoked"),
            ),
        ],
    );
    let omp_inv = special::inventory("antigravity", &home_omp, "ubuntu", None);
    assert_eq!(omp_inv.candidates.len(), 2);
    assert_ne!(
        omp_inv.candidates[0]["locator"],
        omp_inv.candidates[1]["locator"]
    );
    assert_ne!(
        omp_inv.candidates[0]["identity"],
        omp_inv.candidates[1]["identity"]
    );

    let dir_bad = tempfile::tempdir().unwrap();
    let home_bad = dir_bad.path().canonicalize().unwrap();
    let huge = "x".repeat(70_000);
    write_omp_db(
        &home_bad,
        &[
            (1, "google-antigravity", "oauth", None, None),
            (2, "google-antigravity", "oauth", Some(&huge), None),
        ],
    );
    let bad_inv = special::inventory("antigravity", &home_bad, "ubuntu", None);
    assert!(bad_inv.candidates.is_empty());
    assert_eq!(bad_inv.incomplete_selectors, vec!["discover"]);
}

#[cfg(unix)]
#[test]
fn special_private_storage_permissions_and_symlinks_are_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let file = write_private(
        &home,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json(
            "access",
            "2099-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "s"),
        )
        .to_string()
        .as_bytes(),
    );
    // Positive control: 0o600 regular file is discovered.
    assert_eq!(
        special::inventory("antigravity", &home, "ubuntu", None)
            .candidates
            .len(),
        1
    );

    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();
    let loose = special::inventory("antigravity", &home, "ubuntu", None);
    assert!(loose.candidates.is_empty());
    assert_eq!(loose.incomplete_selectors, vec!["discover"]);

    fs::remove_file(&file).unwrap();
    let elsewhere = write_private(
        &home,
        "elsewhere",
        agy_native_json(
            "access",
            "2099-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "s"),
        )
        .to_string()
        .as_bytes(),
    );
    symlink(&elsewhere, &file).unwrap();
    let linked = special::inventory("antigravity", &home, "ubuntu", None);
    assert!(linked.candidates.is_empty());
    assert_eq!(linked.incomplete_selectors, vec!["discover"]);
}

#[tokio::test]
async fn special_antigravity_collect_two_services_refresh_identity_rate_limit_and_races() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join(".ccs");
    let token_path = write_private(
        &home,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json(
            "fixture-private-access",
            "2099-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "sig"),
        )
        .to_string()
        .as_bytes(),
    );
    let candidate =
        special::inventory("antigravity", &home, "ubuntu", Some(&ccs)).candidates[0].clone();

    // 1. Both services read in CLI-then-app order and labelled.
    let google = MockGoogle::new(vec![
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "invented-project"})),
        Ok(agy_summary(0.7032202, 0.9152058)),
        Ok(agy_summary(1.0, 0.9991121)),
    ]);
    let env = SpecialEnv {
        google: Some(&google),
        ..Default::default()
    };
    let res = special::collect_with(&candidate, &home, "ubuntu", Some(&ccs), &env).await;
    assert_eq!(res["failureCode"], Value::Null);
    let windows = res["sample"]["windows"].as_array().unwrap();
    let keys: Vec<&str> = windows.iter().map(|w| w["key"].as_str().unwrap()).collect();
    assert_eq!(
        keys,
        vec![
            "cli-gemini-weekly",
            "cli-gemini-5h",
            "gemini-weekly",
            "gemini-5h"
        ]
    );
    assert!((windows[1]["usedPercent"].as_f64().unwrap() - 29.678).abs() < 0.01);
    assert!(
        windows[0]["label"]
            .as_str()
            .unwrap()
            .ends_with("In T3 (Antigravity CLI)")
    );
    assert!(
        windows[2]["label"]
            .as_str()
            .unwrap()
            .ends_with("In the Antigravity app")
    );
    assert_eq!(
        google.call_urls(),
        vec![
            AGY_USERINFO_URL.to_owned(),
            format!("{AGY_APP_BASE}loadCodeAssist"),
            format!("{AGY_CLI_BASE}retrieveUserQuotaSummary"),
            format!("{AGY_APP_BASE}retrieveUserQuotaSummary"),
        ]
    );

    // 2. Subject mismatch blocks quota even with matching email.
    let google_mismatch = MockGoogle::new(vec![Ok(json!({
        "email": EMAIL, "id": "999", "verified_email": true
    }))]);
    let res = special::collect_with(
        &candidate,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            google: Some(&google_mismatch),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        res,
        json!({"sample": null, "failureCode": "identity_mismatch"})
    );
    assert_eq!(google_mismatch.call_urls().len(), 1);

    // 3. Daily host 429 reaches failover hold and app host is never asked.
    let google_429_cli = MockGoogle::new(vec![
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "private-project"})),
        Err(GoogleHttpError::RateLimited(321)),
    ]);
    let res = special::collect_with(
        &candidate,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            google: Some(&google_429_cli),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        res,
        json!({"sample": null, "failureCode": "rate_limited", "retryAfterSeconds": 321})
    );
    assert!(
        !google_429_cli
            .call_urls()
            .iter()
            .any(|u| u == &format!("{AGY_APP_BASE}retrieveUserQuotaSummary"))
    );

    // 4. App host 429 after CLI answered still drops partial reading and keeps Retry-After.
    let google_429_app = MockGoogle::new(vec![
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "private-project"})),
        Ok(agy_summary(0.8, 0.9)),
        Err(GoogleHttpError::RateLimited(654)),
    ]);
    let res = special::collect_with(
        &candidate,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            google: Some(&google_429_app),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        res,
        json!({"sample": null, "failureCode": "rate_limited", "retryAfterSeconds": 654})
    );
    assert!(!res.to_string().contains("private-project"));

    // 5. 403 on one host publishes the other with failed pools marked unavailable.
    let google_403_cli = MockGoogle::new(vec![
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "private-project"})),
        Err(GoogleHttpError::NeedsSignIn("refused")),
        Ok(agy_summary(0.9, 0.9)),
    ]);
    let res = special::collect_with(
        &candidate,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            google: Some(&google_403_cli),
            ..Default::default()
        },
    )
    .await;
    let w = res["sample"]["windows"].as_array().unwrap();
    assert_eq!(w[0]["key"], "cli-gemini-weekly");
    assert_eq!(w[0]["status"], "unavailable");
    assert_eq!(w[0]["usedPercent"], Value::Null);
    assert_eq!(w[2]["key"], "gemini-weekly");
    assert!((w[2]["usedPercent"].as_f64().unwrap() - 10.0).abs() < 0.01);

    // 6. Expired token refreshes in memory without rewriting native file.
    write_oauth_client(&home);
    write_private(
        &home,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json(
            "expired-access",
            "2020-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "sig"),
        )
        .to_string()
        .as_bytes(),
    );
    let before = fs::read(&token_path).unwrap();
    let exp_cand =
        special::inventory("antigravity", &home, "ubuntu", Some(&ccs)).candidates[0].clone();
    let google_refresh = MockGoogle::new(vec![
        Ok(json!({"access_token": "renewed-private-access"})),
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "proj"})),
        Ok(agy_summary(0.75, 0.75)),
        Ok(agy_summary(0.75, 0.75)),
    ]);
    let res = special::collect_with(
        &exp_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            google: Some(&google_refresh),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(res["failureCode"], Value::Null);
    assert_eq!(fs::read(&token_path).unwrap(), before);
    assert!(!res.to_string().contains("renewed-private-access"));

    // 7. Sign-in change during query discards sample with source_changed.
    let token_path_clone = token_path.clone();
    let home_clone = home.clone();
    let hook = move || {
        write_private(
            &home_clone,
            ".gemini/antigravity-cli/antigravity-oauth-token",
            agy_native_json(
                "another-account-token",
                "2099-01-01T00:00:00Z",
                &fake_jwt(EMAIL, SUBJECT, "sig"),
            )
            .to_string()
            .as_bytes(),
        );
    };
    let google_race = MockGoogle::new(vec![
        Ok(json!({"access_token": "renewed"})),
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "proj"})),
        Ok(agy_summary(0.5, 0.5)),
        Ok(agy_summary(0.5, 0.5)),
    ]);
    let res = special::collect_with(
        &exp_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            google: Some(&google_race),
            post_collect_hook: Some(&hook),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        res,
        json!({"sample": null, "failureCode": "source_changed"})
    );

    // 8. Unreadable sign-in after query returns unavailable (not source_changed).
    let fresh_cand =
        special::inventory("antigravity", &home, "ubuntu", Some(&ccs)).candidates[0].clone();
    let corrupt_hook = move || {
        fs::write(&token_path_clone, b"{\"access_token\": ").unwrap();
    };
    let google_unreadable = MockGoogle::new(vec![
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "proj"})),
        Ok(agy_summary(0.5, 0.5)),
        Ok(agy_summary(0.5, 0.5)),
    ]);
    let res = special::collect_with(
        &fresh_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            google: Some(&google_unreadable),
            post_collect_hook: Some(&corrupt_hook),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(res, json!({"sample": null, "failureCode": "unavailable"}));
}

#[tokio::test]
async fn special_opencode_wallet_inventory_collect_partial_rate_limit_and_race() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join(".ccs");
    let workspace = "wrk_fixture";
    let now = 1_792_000_000.0;

    // Windows reports unavailable / empty.
    assert!(
        special::inventory("opencode-wallet", &home, "windows", Some(&ccs))
            .candidates
            .is_empty()
    );

    let capsule = json!({
        "schemaVersion": 1,
        "origin": "https://opencode.ai",
        "source": "browser-native-cookie-api",
        "workspaceId": workspace,
        "capturedAt": "2026-10-14T17:46:40Z",
        "cookies": [{
            "name": "auth",
            "value": "PRIVATE-WALLET-COOKIE",
            "domain": "opencode.ai",
            "path": "/",
            "secure": true,
            "hostOnly": true
        }]
    });
    let capsule_file = write_private(
        &ccs,
        "account-usage/opencode-console-session.json",
        capsule.to_string().as_bytes(),
    );

    let inv = special::inventory_with(
        "opencode-wallet",
        &home,
        "mac",
        Some(&ccs),
        &SpecialEnv {
            now_secs: Some(now),
            wallet_supported: Some(true),
            ..Default::default()
        },
    );
    assert_eq!(inv.candidates.len(), 1);
    let cand = inv.candidates[0].clone();
    assert_eq!(
        cand["identity"],
        json!({"kind": "opencode-workspace", "key": sha256_hex(workspace.as_bytes())})
    );
    assert_eq!(cand["selectors"], json!(["wallet"]));
    assert!(!public_fields(&cand).to_string().contains("PRIVATE"));

    // Collect with prepaid debt + optional /go/status rate limit preserves balance & retry.
    let wallet_mock = MockWallet::new(vec![
        ("/console/api/orgs", Ok(json!([{"id": workspace}]))),
        (
            "/console/api/billing/status",
            Ok(json!({
                "mode": "pay-as-you-go",
                "billingMode": "prepaid",
                "balanceMicroCents": "-250000000"
            })),
        ),
        (
            "/console/api/go/status",
            Err(WalletError {
                code: "rate_limited",
                retry_after_seconds: Some(321.0),
            }),
        ),
    ]);
    let res = special::collect_with(
        &cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &SpecialEnv {
            now_secs: Some(now),
            wallet: Some(&wallet_mock),
            wallet_supported: Some(true),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(res["failureCode"], "rate_limited");
    assert_eq!(res["retryAfterSeconds"], 321.0);
    assert_eq!(res["sample"]["windows"][0]["remaining"], -2.5);
    assert_eq!(res["sample"]["platform"], "ubuntu");
    assert!(!res.to_string().contains("PRIVATE-WALLET-COOKIE"));

    // Token rotation during partial rate-limited collect emits source_changed + rateLimited: true.
    let cap_clone = capsule_file.clone();
    let rotate_hook = move || {
        let mut rotated: Value = serde_json::from_slice(&fs::read(&cap_clone).unwrap()).unwrap();
        rotated["cookies"][0]["value"] = json!("PRIVATE-WALLET-COOKIE-rotated");
        fs::write(&cap_clone, rotated.to_string()).unwrap();
    };
    let raced = special::collect_with(
        &cand,
        &home,
        "mac",
        Some(&ccs),
        &SpecialEnv {
            now_secs: Some(now),
            wallet: Some(&wallet_mock),
            wallet_supported: Some(true),
            post_collect_hook: Some(&rotate_hook),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        raced,
        json!({
            "sample": null,
            "failureCode": "source_changed",
            "rateLimited": true,
            "retryAfterSeconds": 321.0
        })
    );

    // Damaged capsule on disk marks wallet incomplete; absent capsule does not.
    fs::write(&capsule_file, b"{damaged").unwrap();
    let bad = special::inventory_with(
        "opencode-wallet",
        &home,
        "mac",
        Some(&ccs),
        &SpecialEnv {
            now_secs: Some(now),
            wallet_supported: Some(true),
            ..Default::default()
        },
    );
    assert!(bad.candidates.is_empty());
    assert_eq!(bad.incomplete_selectors, vec!["wallet"]);

    fs::remove_file(&capsule_file).unwrap();
    let absent = special::inventory_with(
        "opencode-wallet",
        &home,
        "mac",
        Some(&ccs),
        &SpecialEnv {
            now_secs: Some(now),
            wallet_supported: Some(true),
            ..Default::default()
        },
    );
    assert!(absent.candidates.is_empty());
    assert!(absent.incomplete_selectors.is_empty());
    assert_eq!(absent.read_failures, 0);
}

// =========================================================================
// failover_additional tests
// =========================================================================

#[test]
fn additional_handles_exact_providers_and_rejects_unknown_or_synthetic() {
    for p in ["muse", "cursor", "kimi-code", "qwen", "zai", "opencode-go"] {
        assert!(additional::handles(p), "expected handles({p})");
    }
    for p in [
        "codex",
        "claude",
        "antigravity",
        "opencode-wallet",
        "zz-synthetic",
        "retired",
    ] {
        assert!(!additional::handles(p), "expected !handles({p})");
    }
}

#[test]
#[cfg_attr(
    windows,
    ignore = "the Muse console capsule store is Unix-only, as in muse_console.py"
)]
fn additional_muse_inventory_attribution_capsule_identity_and_expiry() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join("scope");
    let env_map = test_env(&home);

    // Unreadable slot is attributed only to its own selector.
    write_private(
        &home,
        ".config/muse/auth.json",
        json!({"providers": {"meta": {"access_token": "dca:synthetic-muse", "user_email": "one@example.test"}}})
            .to_string()
            .as_bytes(),
    );
    write_private(&ccs, "muse-homes/1234abcd/muse/auth.json", b"{damaged");

    let env = AdditionalEnv {
        env_vars: Some(&env_map),
        ..Default::default()
    };
    let inv = additional::inventory_with("muse", &home, "ubuntu", Some(&ccs), &env);
    assert_eq!(inv.candidates.len(), 1);
    assert_eq!(inv.candidates[0]["selectors"], json!(["discover"]));
    assert_eq!(
        inv.candidates[0]["identity"]["kind"],
        "muse-native-credential"
    );
    assert_eq!(inv.incomplete_selectors, vec!["config-home:1234abcd"]);

    // Damaged capsule on disk skips the login instead of changing its identity to native.
    let capsule_path = write_private(&ccs, "account-usage/muse-console-session.json", b"{damaged");
    let inv_damaged = additional::inventory_with("muse", &home, "ubuntu", Some(&ccs), &env);
    assert!(inv_damaged.candidates.is_empty());
    assert!(inv_damaged.read_failures > 0);

    // Real capsule establishes muse-team identity, survives expiry in inventory, changes with teamId.
    let stamp = 1_792_000_000.0;
    let capsule_val = json!({
        "schemaVersion": 1,
        "provider": "muse",
        "origin": "https://dev.meta.ai",
        "email": "one@example.test",
        "plan": "Pro",
        "teamId": "42",
        "cookies": [{
            "name": "llama_dev_sess",
            "value": "synthetic-session",
            "domain": "dev.meta.ai",
            "path": "/",
            "secure": true,
            "hostOnly": true,
            "expirationDate": stamp + 1.0
        }]
    });
    write_private(
        &ccs,
        "account-usage/muse-console-session.json",
        capsule_val.to_string().as_bytes(),
    );
    let first = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(stamp),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    assert_eq!(first["identity"]["kind"], "muse-team");
    let expired = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(stamp + 2.0),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    assert_eq!(first["identity"], expired["identity"]);
    assert_eq!(first["revision"], expired["revision"]);

    // Clean capsule removal reverts honestly to native.
    fs::remove_file(&capsule_path).unwrap();
    let native =
        additional::inventory_with("muse", &home, "ubuntu", Some(&ccs), &env).candidates[0].clone();
    assert_eq!(native["identity"]["kind"], "muse-native-credential");
    assert_ne!(native["identity"], first["identity"]);
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "the Muse console capsule store is Unix-only, as in muse_console.py"
)]
async fn additional_muse_portal_budget_caching_upgrade_and_429() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join("scope");
    let env_map = test_env(&home);

    let email = "fixture@example.test";
    let token = "dca:fixture-portal";
    write_private(
        &home,
        ".config/muse/auth.json",
        json!({"providers": {"meta": {"access_token": token, "user_email": email}}})
            .to_string()
            .as_bytes(),
    );
    let now = 1_792_000_000.0;
    write_private(
        &ccs,
        "account-usage/muse-console-session.json",
        json!({
            "schemaVersion": 1,
            "provider": "muse",
            "origin": "https://dev.meta.ai",
            "email": email,
            "plan": "Pro",
            "teamId": "42",
            "cookies": [{
                "name": "llama_dev_sess",
                "value": "fixture-session",
                "domain": "dev.meta.ai",
                "path": "/",
                "secure": true,
                "hostOnly": true
            }]
        })
        .to_string()
        .as_bytes(),
    );

    let inv = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    );
    assert_eq!(inv.candidates.len(), 1);
    let cand = inv.candidates[0].clone();

    let plan_client = MockPlanTransport::json_replies(vec![
        Ok(
            json!({"user_email": email, "subs_tier_name": "Pro", "is_subs_active": true, "api_key": "sk-do-not-persist"}),
        ),
        Ok(json!({"user_email": email, "subs_tier_name": "Ultra", "is_subs_active": true})),
    ]);
    let portal = MockPortal::new(
        email,
        "42",
        json!({"tier": "Pro", "window_weighted_used": 10, "window_weighted_limit": 100, "as_of": now}),
    );

    // 1. Fresh portal read -> status ok, refreshAfterSeconds 300.
    let first = additional::collect_with(
        &cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            plan_transport: Some(&plan_client),
            portal_transport: Some(&portal),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(first["failureCode"], Value::Null);
    assert_eq!(first["sample"]["status"], "ok");
    assert_eq!(first["refreshAfterSeconds"], 300.0);
    assert_eq!(plan_client.call_count(), 1);
    assert_eq!(portal.calls.lock().unwrap().len(), 3);
    assert!(
        !fs::read_to_string(ccs.join("account-usage/muse-console-usage.json"))
            .unwrap()
            .contains("sk-do-not-persist")
    );

    // 2. Reuse inside 300s budget makes zero network calls.
    let cached = additional::collect_with(
        &cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now + 1.0),
            env_vars: Some(&env_map),
            plan_transport: Some(&plan_client),
            portal_transport: Some(&portal),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(cached["sample"]["status"], "ok");
    assert_eq!(cached["refreshAfterSeconds"], 299.0);
    assert_eq!(plan_client.call_count(), 1);
    assert_eq!(portal.calls.lock().unwrap().len(), 3);

    // 3. Plan upgrade + portal 429 invalidates old Pro quota and keeps observedPlan=Ultra + 429.
    *portal.quota_error.lock().unwrap() = Some(PortalError {
        code: "rate_limited",
        retry_after: Some(900.0),
    });
    let upgraded_429 = additional::collect_with(
        &cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now + 301.0),
            env_vars: Some(&env_map),
            plan_transport: Some(&plan_client),
            portal_transport: Some(&portal),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(upgraded_429["sample"], Value::Null);
    assert_eq!(upgraded_429["failureCode"], "rate_limited");
    assert_eq!(upgraded_429["retryAfterSeconds"], 900.0);
    assert_eq!(upgraded_429["observedPlan"], "Ultra");
    assert!(upgraded_429["observedPlanAt"].is_string());
}

#[tokio::test]
async fn additional_cursor_new_tiers_legacy_tiers_email_conflict_and_rate_limit() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join("scope");
    let env_map = test_env(&home);

    // Subject survives token rotation, changes on account switch.
    write_private(
        &home,
        ".config/Cursor/auth.json",
        json!({"accessToken": fake_jwt("one@example.test", "auth0|one", "sig1")})
            .to_string()
            .as_bytes(),
    );
    let env = AdditionalEnv {
        env_vars: Some(&env_map),
        ..Default::default()
    };
    let first = additional::inventory_with("cursor", &home, "ubuntu", Some(&ccs), &env).candidates
        [0]
    .clone();
    write_private(
        &home,
        ".config/Cursor/auth.json",
        json!({"accessToken": fake_jwt("one@example.test", "auth0|one", "rotated")})
            .to_string()
            .as_bytes(),
    );
    let second = additional::inventory_with("cursor", &home, "ubuntu", Some(&ccs), &env).candidates
        [0]
    .clone();
    assert_eq!(first["identity"], second["identity"]);
    assert_eq!(first["locator"], second["locator"]);
    assert_ne!(first["revision"], second["revision"]);

    // New tier (premium) collects shared total + additional spend.
    let client_new = MockPlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "premium"})),
        Ok(json!({
            "enabled": true,
            "billingCycleEnd": "1794697200000",
            "planUsage": {"totalPercentUsed": 12.3456}
        })),
        Ok(json!({
            "isMerged": true,
            "planInfo": {
                "includedUsagePeriod": "INCLUDED_USAGE_PERIOD_WEEKLY",
                "includedUsageResetsAt": "1792022400000"
            }
        })),
        Ok(json!({
            "billingCycleEnd": "2026-11-14T00:00:00Z",
            "individualUsage": {
                "plan": {"used": 1234, "limit": 10000},
                "onDemand": {"used": 250, "limit": 2000, "remaining": 1750}
            }
        })),
        Ok(json!({})),
    ]);
    let res = additional::collect_with(
        &second,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_new),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(res["failureCode"], Value::Null);
    assert_eq!(res["observedPlan"], "premium");
    let windows = res["sample"]["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 2);
    assert_eq!(windows[0]["key"], "grok-shared-weekly");
    assert_eq!(windows[0]["label"], "Grok and Composer");
    assert_eq!(windows[0]["usedPercent"], 12.3456);
    assert_eq!(windows[0]["resetAt"], "2026-10-15T00:00:00Z");
    assert_eq!(windows[1]["kind"], "extra_usage");
    assert_eq!(windows[1]["label"], "Additional usage spend");
    assert_eq!(windows[1]["used"], 2.5);

    // Live email conflict rejects quota and clears observedPlan.
    let client_conflict = MockPlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "pro"})),
        Ok(json!({"individualUsage": {"plan": {"used": 0, "limit": 100}}})),
        Ok(json!({"email": "other@example.test"})),
    ]);
    let conflict = additional::collect_with(
        &second,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_conflict),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(conflict["failureCode"], "account_mismatch");
    assert_eq!(conflict["sample"], Value::Null);
    assert!(conflict.get("observedPlan").is_none());

    // Optional profile rate limit preserves main quota and retryAfterSeconds.
    let client_opt_429 = MockPlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "pro"})),
        Ok(json!({"individualUsage": {"plan": {"used": 10, "limit": 100}}})),
        Err(CollectionError {
            status: "error",
            message: "private",
            failure_code: Some("rate_limited"),
            retry_after: Some(900.0),
        }),
    ]);
    let opt_429 = additional::collect_with(
        &second,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_opt_429),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(opt_429["failureCode"], "rate_limited");
    assert_eq!(opt_429["retryAfterSeconds"], 900.0);
    assert_eq!(opt_429["sample"]["windows"][0]["usedPercent"], 10.0);
}

#[tokio::test]
async fn additional_plans_kimi_zai_opencode_go_and_qwen() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join("scope");
    let env_map = test_env(&home);

    // 1. Z.ai saved key + optional reset-packs 429 preserves main quota and retry.
    write_private(
        &ccs,
        "account-usage/keys/zai-11111111.json",
        json!({"version": 1, "provider": "zai", "keyId": "11111111", "secret": "sk-zai-fixture"})
            .to_string()
            .as_bytes(),
    );
    let inv_zai = additional::inventory_with(
        "zai",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            force_plaintext_keys: Some(true),
            ..Default::default()
        },
    );
    assert_eq!(inv_zai.candidates.len(), 1);
    let client_zai = MockPlanTransport::json_replies(vec![
        Ok(json!({"data": {"limits": [{"type": "TOKENS_LIMIT", "percentage": 15}]}})),
        Err(CollectionError {
            status: "error",
            message: "private",
            failure_code: Some("rate_limited"),
            retry_after: Some(700.0),
        }),
    ]);
    let zai_res = additional::collect_with(
        &inv_zai.candidates[0],
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_zai),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(zai_res["failureCode"], "rate_limited");
    assert_eq!(zai_res["retryAfterSeconds"], 700.0);
    assert_eq!(zai_res["sample"]["status"], "ok");
    assert!(!zai_res["sample"]["windows"].as_array().unwrap().is_empty());

    // 2. Second saved OpenCode Go key is collected without substituting first key.
    write_private(
        &ccs,
        "account-usage/keys/opencode-go-11111111.json",
        json!({"version": 1, "provider": "opencode-go", "keyId": "11111111", "secret": "sk-fixture-first"})
            .to_string()
            .as_bytes(),
    );
    write_private(
        &ccs,
        "account-usage/keys/opencode-go-22222222.json",
        json!({"version": 1, "provider": "opencode-go", "keyId": "22222222", "secret": "sk-fixture-second"})
            .to_string()
            .as_bytes(),
    );
    let go_cands = additional::inventory_with(
        "opencode-go",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            force_plaintext_keys: Some(true),
            ..Default::default()
        },
    )
    .candidates;
    assert_eq!(go_cands.len(), 2);
    let second_go = go_cands
        .iter()
        .find(|c| c["selectors"] == json!(["aac-key:22222222"]))
        .unwrap();
    let client_go = MockPlanTransport::json_replies(vec![Ok(json!({
        "usage": {
            "rolling": {"status": "ok", "percent": 0},
            "weekly": {"status": "ok", "percent": 0},
            "monthly": {"status": "ok", "percent": 0}
        }
    }))]);
    let go_res = additional::collect_with(
        second_go,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_go),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(go_res["failureCode"], Value::Null);
    assert_eq!(go_res["sample"]["windows"][0]["usedPercent"], 0.0);
    let sent_headers = client_go.calls.lock().unwrap()[0].1.clone();
    assert!(
        sent_headers
            .iter()
            .any(|(k, v)| k == "Authorization" && v == "Bearer sk-fixture-second")
    );

    // 3. Kimi expired OAuth (including string expiry) never calls provider.
    let expired_row = json!({"access": "expired-fixture", "expires": "1"}).to_string();
    write_omp_db(
        &home,
        &[(1, "kimi-code", "oauth", Some(&expired_row), None)],
    );
    let kimi_cands = additional::inventory_with(
        "kimi-code",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates;
    assert_eq!(kimi_cands.len(), 1);
    let client_unused = MockPlanTransport::json_replies(vec![]);
    let kimi_res = additional::collect_with(
        &kimi_cands[0],
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_unused),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(kimi_res["failureCode"], "needs_sign_in");
    assert_eq!(client_unused.call_count(), 0);

    // 4. Qwen plain inference key is excluded; multiple DPAPI capsules are enumerated.
    write_private(
        &home,
        ".local/share/opencode/auth.json",
        json!({"alibaba-token-plan": {"type": "api", "key": "sk-fixture-inference"}})
            .to_string()
            .as_bytes(),
    );
    assert!(
        additional::inventory_with(
            "qwen",
            &home,
            "ubuntu",
            Some(&ccs),
            &AdditionalEnv {
                env_vars: Some(&env_map),
                ..Default::default()
            }
        )
        .candidates
        .is_empty()
    );

    let b64 = base64::engine::general_purpose::STANDARD.encode(b"fixture");
    for name in [
        "qwen-console-session.json",
        "qwen-console-session-1234abcd.json",
    ] {
        write_private(
            &ccs,
            &format!("account-usage/{name}"),
            json!({"version": 2, "region": "intl", "cookiesDPAPI": b64})
                .to_string()
                .as_bytes(),
        );
    }
    let dpapi = |_: &[u8], _: Option<&[u8]>| -> Option<String> {
        Some(json!({"consoleCookie": "sid=fixture", "gatewayCookie": "sid=fixture"}).to_string())
    };
    let qwen_inv = additional::inventory_with(
        "qwen",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            dpapi: Some(&dpapi),
            ..Default::default()
        },
    );
    assert_eq!(qwen_inv.candidates.len(), 2);
    assert!(
        qwen_inv
            .candidates
            .iter()
            .any(|c| c["selectors"] == json!(["browser-capsule:1234abcd"]))
    );
    assert!(
        qwen_inv
            .candidates
            .iter()
            .any(|c| c["selectors"] == json!(["browser-capsule:default", "discover"]))
    );
}

// =========================================================================
// Differential Python-oracle golden digest verification & deep edge cases
// =========================================================================

fn make_binarycookies(cookies: &[(&str, &str, &str, &str, f64)]) -> Vec<u8> {
    let mut records: Vec<Vec<u8>> = Vec::new();
    for (domain, name, path, value, expiry) in cookies {
        let mut d = domain.as_bytes().to_vec();
        d.push(0);
        let mut n = name.as_bytes().to_vec();
        n.push(0);
        let mut p = path.as_bytes().to_vec();
        p.push(0);
        let mut v = value.as_bytes().to_vec();
        v.push(0);
        let off_d = 56u32;
        let off_n = off_d + d.len() as u32;
        let off_p = off_n + n.len() as u32;
        let off_v = off_p + p.len() as u32;
        let total = off_v + v.len() as u32;
        let mut rec = Vec::with_capacity(total as usize);
        rec.extend_from_slice(&total.to_le_bytes());
        rec.extend_from_slice(&0u32.to_le_bytes());
        rec.extend_from_slice(&1u32.to_le_bytes());
        rec.extend_from_slice(&0u32.to_le_bytes());
        rec.extend_from_slice(&off_d.to_le_bytes());
        rec.extend_from_slice(&off_n.to_le_bytes());
        rec.extend_from_slice(&off_p.to_le_bytes());
        rec.extend_from_slice(&off_v.to_le_bytes());
        rec.extend_from_slice(&[0u8; 8]);
        rec.extend_from_slice(&expiry.to_le_bytes());
        rec.extend_from_slice(&0.0f64.to_le_bytes());
        rec.extend_from_slice(&d);
        rec.extend_from_slice(&n);
        rec.extend_from_slice(&p);
        rec.extend_from_slice(&v);
        records.push(rec);
    }
    let header_len = 12 + records.len() * 4;
    let mut offsets = Vec::with_capacity(records.len());
    let mut pos = header_len as u32;
    for r in &records {
        offsets.push(pos);
        pos += r.len() as u32;
    }
    let mut page = Vec::new();
    page.extend_from_slice(&[0, 0, 1, 0]);
    page.extend_from_slice(&(records.len() as u32).to_le_bytes());
    for o in offsets {
        page.extend_from_slice(&o.to_le_bytes());
    }
    page.extend_from_slice(&[0, 0, 0, 0]);
    for r in records {
        page.extend_from_slice(&r);
    }
    let mut out = b"cook".to_vec();
    out.extend_from_slice(&1u32.to_be_bytes());
    out.extend_from_slice(&(page.len() as u32).to_be_bytes());
    out.extend_from_slice(&page);
    out
}

fn jwt_compact(payload: Value) -> String {
    format!(
        "hdr.{}.sig",
        URL_SAFE_NO_PAD.encode(payload.to_string().as_bytes())
    )
}

#[test]
fn provider_golden_digests_all_eight_providers() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join(".ccs");
    let now = 1_792_000_000.0;

    // 1. Antigravity saved + native + OMP
    let raw_saved = json!({
        "auth_method": "consumer",
        "token": {
            "access_token": "tok-saved",
            "refresh_token": "ref-saved",
            "token_type": "Bearer",
            "expiry": "2099-01-01T00:00:00Z"
        },
        "id_token": jwt_compact(json!({"email": "Alice@Example.Test", "sub": "123456789"}))
    })
    .to_string();
    let fp = sha256_hex(raw_saved.as_bytes());
    let id_key = agy_identity_key("Alice@Example.Test", "123456789");
    write_private(
        &ccs,
        &format!("antigravity-instances/work/ubuntu/credential-{fp}.bin"),
        raw_saved.as_bytes(),
    );
    write_private(
        &ccs,
        "antigravity-profiles/registry-000000000001.json",
        json!({
            "version": 1,
            "revision": 1,
            "profiles": [{
                "id": "work",
                "email": "Alice@Example.Test",
                "identityKey": id_key,
                "plan": "Pro",
                "credential": {
                    "fingerprint": fp,
                    "format": "consumer-json",
                    "identity": {
                        "email": "Alice@Example.Test",
                        "subject": "123456789",
                        "plan": "Pro",
                        "verifiedAt": "2026-01-01T00:00:00Z",
                        "source": "provider-userinfo"
                    }
                }
            }],
            "active": null,
            "transaction": null
        })
        .to_string()
        .as_bytes(),
    );
    write_private(
        &home,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        json!({
            "auth_method": "consumer",
            "token": {
                "access_token": "tok-native",
                "refresh_token": "ref-native",
                "token_type": "Bearer",
                "expiry": "2099-01-01T00:00:00Z"
            },
            "id_token": jwt_compact(json!({"email": "bob@example.test", "sub": "987654321"}))
        })
        .to_string()
        .as_bytes(),
    );

    // OMP DB across all providers
    let row_agy = json!({
        "access": "omp-agy",
        "email": "carol@example.test",
        "id_token": jwt_compact(json!({"email": "carol@example.test", "sub": "555"}))
    })
    .to_string();
    let row_muse = json!({
        "access": json!({"oauthAccessToken": "dca:omp-muse-packed"}).to_string(),
        "email": "muse-omp@example.test"
    })
    .to_string();
    let row_cur = json!({
        "access": jwt_compact(json!({"sub": "auth0|omp-cur", "email": "cur-omp@example.test"})),
        "email": "cur-omp@example.test"
    })
    .to_string();
    let row_kimi_exp = json!({
        "access": "kimi-omp-expired",
        "expires": 1000,
        "email": "k1@example.test"
    })
    .to_string();
    let row_kimi_key = json!({"key": "kimi-omp-key", "email": "k2@example.test"}).to_string();
    let row_zai = json!({"key": "zai-omp-key"}).to_string();
    let row_go = json!({"key": "go-omp-key"}).to_string();
    let row_qwen = json!({
        "key": json!({
            "cookie": "b=2; a=1",
            "gatewayCookie": "g2=y; g1=x",
            "baseUrl": "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
        }).to_string()
    })
    .to_string();
    write_omp_db(
        &home,
        &[
            (1, "google-antigravity", "oauth", Some(&row_agy), None),
            (2, "muse-code", "oauth", Some(&row_muse), None),
            (3, "cursor", "oauth", Some(&row_cur), None),
            (4, "kimi-code", "oauth", Some(&row_kimi_exp), None),
            (5, "kimi-code", "api_key", Some(&row_kimi_key), None),
            (6, "zai", "api_key", Some(&row_zai), None),
            (7, "opencode-go", "api_key", Some(&row_go), None),
            (8, "alibaba-token-plan", "api_key", Some(&row_qwen), None),
        ],
    );

    // 2. Wallet capsule
    write_private(
        &ccs,
        "account-usage/opencode-console-session.json",
        json!({
            "schemaVersion": 1,
            "origin": "https://opencode.ai",
            "source": "browser-native-cookie-api",
            "workspaceId": "wrk_golden01",
            "capturedAt": "2026-10-14T17:46:40Z",
            "cookies": [{
                "name": "auth",
                "value": "GOLDEN-WALLET",
                "domain": "opencode.ai",
                "path": "/",
                "secure": true,
                "hostOnly": true
            }]
        })
        .to_string()
        .as_bytes(),
    );

    // 3. Muse native + slot + team capsule
    write_private(
        &home,
        ".config/muse/auth.json",
        json!({"providers": {"meta": {"access_token": "dca:muse-native", "user_email": "Muse@Example.Test"}}})
            .to_string()
            .as_bytes(),
    );
    write_private(
        &ccs,
        "muse-homes/a1b2c3d4/muse/auth.json",
        json!({"providers": {"meta": {"access_token": "dca:muse-slot", "user_email": "slot@example.test"}}})
            .to_string()
            .as_bytes(),
    );
    write_private(
        &ccs,
        "account-usage/muse-console-session.json",
        json!({
            "schemaVersion": 1,
            "provider": "muse",
            "origin": "https://dev.meta.ai",
            "email": "muse@example.test",
            "plan": "Pro",
            "teamId": "424242",
            "cookies": [{
                "name": "llama_dev_sess",
                "value": "GOLDEN-MUSE-COOKIE",
                "domain": "dev.meta.ai",
                "path": "/",
                "secure": true,
                "hostOnly": true
            }]
        })
        .to_string()
        .as_bytes(),
    );

    // 4. Cursor sqlite + auth.json
    let croot = home.join(".config/Cursor");
    let vscdb = croot.join("User/globalStorage/state.vscdb");
    fs::create_dir_all(vscdb.parent().unwrap()).unwrap();
    let cconn = rusqlite::Connection::open(&vscdb).unwrap();
    cconn
        .execute(
            "CREATE TABLE ItemTable (key TEXT PRIMARY KEY, value TEXT)",
            [],
        )
        .unwrap();
    let sql_jwt = jwt_compact(json!({"sub": "auth0|cur-sql", "email": "sql@example.test"}));
    cconn
        .execute(
            "INSERT INTO ItemTable VALUES ('cursorAuth/accessToken', ?1)",
            [&sql_jwt],
        )
        .unwrap();
    cconn
        .execute(
            "INSERT INTO ItemTable VALUES ('cursorAuth/cachedEmail', 'cached@example.test')",
            [],
        )
        .unwrap();
    cconn
        .execute(
            "INSERT INTO ItemTable VALUES ('cursorAuth/stripeMembershipType', 'pro')",
            [],
        )
        .unwrap();
    drop(cconn);
    write_private(
        &croot,
        "auth.json",
        json!({"accessToken": "opaque-cursor-token"})
            .to_string()
            .as_bytes(),
    );

    // 5. OpenCode auth.json + aac-keys + Qwen capsules + Safari binarycookies
    write_private(
        &home,
        ".local/share/opencode/auth.json",
        json!({
            "kimi-for-coding": {"type": "api", "key": "oc-kimi-1"},
            "kimi-code": {"type": "api", "key": "oc-kimi-2"},
            "zai": {"type": "api", "key": "oc-zai-1"},
            "zai-coding-plan": {"type": "api", "key": "oc-zai-2"},
            "opencode-go": {"type": "api", "key": "oc-go-1"}
        })
        .to_string()
        .as_bytes(),
    );
    for (prov, slot, secret) in [
        ("kimi-code", "11223344", "sk-kimi-aac-key-1234"),
        ("zai", "22334455", "sk-zai-aac-key-1234"),
        ("opencode-go", "33445566", "sk-go-aac-key-1234"),
    ] {
        let fp_k = format!("sha256:{}", &sha256_hex(secret.as_bytes())[..16]);
        write_private(
            &ccs,
            &format!("account-usage/keys/{prov}-{slot}.json"),
            json!({
                "version": 1,
                "provider": prov,
                "keyId": slot,
                "secret": secret,
                "fingerprint": fp_k,
                "last4": &secret[secret.len() - 4..],
                "createdAt": "2026-01-01T00:00:00Z"
            })
            .to_string()
            .as_bytes(),
        );
    }
    let v2_plain = json!({"consoleCookie": "z=9; a=1", "gatewayCookie": "g=2"}).to_string();
    write_private(
        &ccs,
        "account-usage/qwen-console-session.json",
        json!({
            "version": 2,
            "region": "cn",
            "cookiesDPAPI": base64::engine::general_purpose::STANDARD.encode(v2_plain.as_bytes())
        })
        .to_string()
        .as_bytes(),
    );
    write_private(
        &ccs,
        "account-usage/qwen-console-session-deadbeef.json",
        json!({
            "version": 1,
            "region": "intl",
            "cookieDPAPI": base64::engine::general_purpose::STANDARD.encode(b"c=3; b=2")
        })
        .to_string()
        .as_bytes(),
    );
    let bc = make_binarycookies(&[(".qwencloud.com", "sid", "/", "safari-intl", 0.0)]);
    write_private(
        &home,
        "Library/Containers/com.apple.Safari/Data/Library/Cookies/Cookies.binarycookies",
        &bc,
    );

    let mut env_map = test_env(&home);
    env_map.insert("KIMI_CODE_API_KEY".into(), "env-kimi-key".into());
    env_map.insert("ZAI_API_KEY".into(), "env-zai-key".into());
    env_map.insert("OPENCODE_GO_API_KEY".into(), "env-go-key".into());

    let dpapi = |raw: &[u8], _: Option<&[u8]>| -> Option<String> {
        std::str::from_utf8(raw).ok().map(str::to_owned)
    };
    let s_env = SpecialEnv {
        now_secs: Some(now),
        wallet_supported: Some(true),
        ..Default::default()
    };
    let a_env = AdditionalEnv {
        now_secs: Some(now),
        env_vars: Some(&env_map),
        dpapi: Some(&dpapi),
        force_plaintext_keys: Some(true),
        ..Default::default()
    };

    let pub_rows = |rows: Vec<Value>| -> Vec<Value> { rows.iter().map(public_fields).collect() };

    // Exact Python oracle assertions across all 8 providers:
    assert_eq!(
        pub_rows(
            special::inventory_with("antigravity", &home, "ubuntu", Some(&ccs), &s_env).candidates
        ),
        vec![
            json!({
                "locator": "c69b00c6fbb6ac066e5da99ab06aacca1b5f9f766ac22ac0e84d113807efb16d",
                "selectors": ["antigravity-profile:work"],
                "identity": {"kind": "antigravity-subject", "key": "00d98021415e98ab0aeb5853dfa97a9414622802528e0386450f817be9845416"},
                "revision": "19e5695d97edd58da1d56682f59613ca8785c1836d440edc0e87f52c345e0b91",
                "email": "alice@example.test",
                "plan": "Pro"
            }),
            json!({
                "locator": "9f20a25d1fccf042437a1437a4cd6bb78dfb773a8ddc84348b63bea14022198c",
                "selectors": ["discover"],
                "identity": {"kind": "antigravity-subject", "key": "8067eb773ce33fefdc19615fb6223703ca36afc1b1b9f74106b52bedf8e7a060"},
                "revision": "b813025e7b1508569701b70b01218039145df09402ed90411cf3dc2d6d5893a7",
                "email": "bob@example.test",
                "plan": null
            }),
            json!({
                "locator": "be6f2ffd684c6a4a6c77555852993c4896bd51c77c7bcb76da5953c7fdc34afb",
                "selectors": ["discover"],
                "identity": {"kind": "antigravity-subject", "key": "d4b345721883ac9941e10f4210a8337521a9ed89bc44be5429694e9776d2bb7e"},
                "revision": "7de5190dd04de200e43f99363032f872a3a3a348a97c32754d82d2794904cece",
                "email": "carol@example.test",
                "plan": null
            }),
        ]
    );
    assert_eq!(
        pub_rows(
            special::inventory_with("opencode-wallet", &home, "mac", Some(&ccs), &s_env).candidates
        ),
        vec![json!({
            "locator": "281ce673e1c1b36c7200d61e4de2c7a27759069d9455639107dd714007d92edb",
            "selectors": ["wallet"],
            "identity": {"kind": "opencode-workspace", "key": "bf87bab01c24e812af55211b1d010d48552b811cc2fa2c8f9c3f2cfc08ca3ae5"},
            "revision": "3e2b950d9b703dd3e970d49a3c5d3a65c2370f3bc5fc24deeca959ad61754b56",
            "email": null,
            "plan": null
        })]
    );
    assert_eq!(
        pub_rows(
            additional::inventory_with("muse", &home, "ubuntu", Some(&ccs), &a_env).candidates
        ),
        vec![
            json!({
                "locator": "d060f58a3c8e3b4141efea12518118fbffa18afda51819c10a4516e95f04c22a",
                "selectors": ["discover"],
                "identity": {"kind": "muse-team", "key": "b58d9b2966aeeb4b10683a801ba46725b23388d2b5951fce4482230b4e96b729"},
                "revision": "63f206b7975ccc612766dafbacda11dad6f120bf6ebfb52b7b25f33ad3fddf5e",
                "email": "Muse@Example.Test",
                "plan": null
            }),
            json!({
                "locator": "ce19864572d8f94d6706425b07fceabd8b99d06bd5edb831c68a53b001bc480f",
                "selectors": ["config-home:a1b2c3d4"],
                "identity": {"kind": "muse-native-credential", "key": "606e21c0cce9338166d25e308d99bd92c711a2f4cd16d268de509bb29355901f"},
                "revision": "d408081939fc13e7ac9cd6a5340e3db752fae3f4ab6a252862b24ac2ef68fcda",
                "email": "slot@example.test",
                "plan": null
            }),
            json!({
                "locator": "87e5366d925c0137967b79a2ae3f868ffe8e59e5c8ddc9ffbf4f07a37e200f1a",
                "selectors": ["discover"],
                "identity": {"kind": "muse-native-credential", "key": "b457ad40db809e844a99928ffc660c5491638077154b3dbc63dfd2560fbfe403"},
                "revision": "97596d1f9814eadde0715332d1de96a88e1815bcaa4997d0476278d3bc3e1625",
                "email": "muse-omp@example.test",
                "plan": null
            }),
        ]
    );
    assert_eq!(
        pub_rows(
            additional::inventory_with("cursor", &home, "ubuntu", Some(&ccs), &a_env).candidates
        ),
        vec![
            json!({
                "locator": "b0a91aeff86721a7c5b16555236d3cd38f202e5b73d3094ddf7d6bae97b9baf1",
                "selectors": ["discover"],
                "identity": {"kind": "cursor-subject", "key": "44b5e01bb2c1a92c1e1af493c4dbd09992fa02dabaf77220c95082ce182db162"},
                "revision": "2f1411e0855dd39b364112eeb4955f3630a89ece1f0e1869435d18aac0944518",
                "email": "sql@example.test",
                "plan": "pro"
            }),
            json!({
                "locator": "d900bc964cb7f55f8d73bce5f4df07ac9f051fbd826965b6c265acd926d148bc",
                "selectors": ["discover"],
                "identity": {"kind": "cursor-credential", "key": "91e5c46201f57f3fb3c7c790e41355f31595033a5e90f088e4a8441192bd0fec"},
                "revision": "b255806feb366aeee905aebdac369b6d6d8d96c167fa530c4a784c4d8ff2185d",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "86030ccdc44e5f1007d66067c23bc8f19fd7c42a8dfc8e2cb072e03f46a411be",
                "selectors": ["discover"],
                "identity": {"kind": "cursor-subject", "key": "b4b25f29406450e3b1b7a62b489d31d55ad8e2ab97b1fafe9fdd5bc472e3839e"},
                "revision": "b33864d1ccaaf577027db1c97d3d018323e88d52e0edecdd3f3ac69384d67ee5",
                "email": "cur-omp@example.test",
                "plan": null
            }),
        ]
    );
    assert_eq!(
        pub_rows(
            additional::inventory_with("kimi-code", &home, "ubuntu", Some(&ccs), &a_env).candidates
        ),
        vec![
            json!({
                "locator": "44142c9d6876f73fba872074e9750a126270b140adc32dd73b71e4f680e18dfa",
                "selectors": ["discover"],
                "identity": {"kind": "kimi-code-credential", "key": "4f02207e7bae6e032f9ac90f20a617a6428c9ca489b9a0e9b9692521891b15ee"},
                "revision": "dfca0fbdc43807709047853ebabd251a414b3c3177b6c3d4a051836ccbe27d8d",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "21f684e9ba0e4fe750dd44267cd45ce1da9e25540d73004dae8d7805224dfb42",
                "selectors": ["discover"],
                "identity": {"kind": "kimi-code-credential", "key": "d0593244c1d6422a43369fa6802d67b4ffb43fb16c3fb59ddce5d2fbb8407072"},
                "revision": "fc270b8f802f522602a1ea774026f3b134e246c6f4c48546e5f088794540b71b",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "1f03f69404a35a22fa67bf90880f09cc73ec21dddb7ef465fab0b3928d1e1194",
                "selectors": ["discover"],
                "identity": {"kind": "kimi-code-credential", "key": "f21967408d1515eceb6161521e9629730ddb684cf6a72f4813352e6fdc1a3b26"},
                "revision": "b97cdcce8af0226a7f093330b0e09bf35b7b62cd8573e610de86622908525a9c",
                "email": "k2@example.test",
                "plan": null
            }),
            json!({
                "locator": "ca8f437999c70a290609ff40cb67cf10efef8b8e39f5db088d81fb66b0886359",
                "selectors": ["discover"],
                "identity": {"kind": "kimi-code-credential", "key": "ce569b2ab9a421e9c10e1ad1662c9e6882918f8eaaac9e27a793b05ecd415d30"},
                "revision": "0489dd165591158ec6eacb33a04e84445b2c25b34e345d961d2976a85cabed65",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "520c27c12b0d378d654cbd9ad60b9a9a1273d251dfa1a85d6490e95a2a2d611c",
                "selectors": ["aac-key:11223344"],
                "identity": {"kind": "kimi-code-credential", "key": "0f8b8e9ddbf63f496f5783c24c0dd739d34fbe76a3085ca440bced2b71a745d9"},
                "revision": "84ee8c04239ee4576fec947786e6e93653f47ba6d83b3d90f9898e2ad0df00cf",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "da7fe0e848f12847daca782cacb36e566cdf47291843da07167b299dd1c54f77",
                "selectors": ["discover"],
                "identity": {"kind": "kimi-code-credential", "key": "9f885226236dd4793d157861495c8d042d59b0c4fc45c786827903d463750a1e"},
                "revision": "47ec354ec708c1fea206b600b56617258e3aaf4f813eca36f76ba41dce6b2014",
                "email": "k1@example.test",
                "plan": null
            }),
        ]
    );
    assert_eq!(
        pub_rows(additional::inventory_with("zai", &home, "ubuntu", Some(&ccs), &a_env).candidates),
        vec![
            json!({
                "locator": "51750f7be9fc094efc2dccfb5026f191df6b0c2c2070616d50d28ad34067065e",
                "selectors": ["discover"],
                "identity": {"kind": "zai-credential", "key": "84ca8ae17aba7523a4c395c49a44fdaaed249a0c3e3ee1a92d38f4440cd0d56c"},
                "revision": "cadfd9ddc6582460e78b73b1fff7558c710b90615a62f208d8499362a4eeeedd",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "4b95202514d3863b7b161da7f674962c7452071682babc8be7f465311c891853",
                "selectors": ["discover"],
                "identity": {"kind": "zai-credential", "key": "677b39c460a75067c479f3ea23c3f31b207c583fa09b04b185e593e980283b57"},
                "revision": "0ec21c07818cd44e0b3ddc6ed9a69502dd0ce140fe8b4c7207f40f31f2da372d",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "a3b22841868affc2ea994dff74a6cf876f291da3335dbfe772bc63d945fd78e2",
                "selectors": ["discover"],
                "identity": {"kind": "zai-credential", "key": "7242fbd1b703805a0c63247c40710e48a1c883b775d5315df3980497677fdb4c"},
                "revision": "3bcd1b10fa4695a4a56e7e35bab54cc82d2bf41edc36b38ad8dba5c7443cf931",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "5fe8b7109335d5304a9358e5933d3f979ad0d3ccd5bd23e75a0141aa90adf20d",
                "selectors": ["discover"],
                "identity": {"kind": "zai-credential", "key": "6121d4d11365a423cff997f2aaae1e84c609c221ceff283040fa3b1908d46fcb"},
                "revision": "45866094e5ca584a7e660319c4bf6e42d96f20429ade9b8005385cc742e2d258",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "83d67f98f6218d44383212a97ed3efbfc3da52eed88ce1b414e40efb1fe2e3bc",
                "selectors": ["aac-key:22334455"],
                "identity": {"kind": "zai-credential", "key": "a0588caef30c7126cbe58139d800d65da29f9be2bd31cf32ec73b51b21234953"},
                "revision": "524450f57b6b7f79cb94ac156954edbfc946c2dd24ff8ca8cb46c99dc28d6149",
                "email": null,
                "plan": null
            }),
        ]
    );
    assert_eq!(
        pub_rows(
            additional::inventory_with("opencode-go", &home, "ubuntu", Some(&ccs), &a_env)
                .candidates
        ),
        vec![
            json!({
                "locator": "a17e77a7d654a20df2e748d608014d9d24a39aa2b2edc28873beea3d43aeb020",
                "selectors": ["discover"],
                "identity": {"kind": "opencode-go-credential", "key": "2c7a76b443c11855e97b23014b76a4395608711e887ad49eb5d467b8f86c655b"},
                "revision": "938ecfc64799d4b8df42221c19e557ce6d825e6cb9c5a2432df88534412a5aba",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "b22ce63a8c74e40685823508bcb261a989c166b0a76a1f91bb3b6ed9820d3ed8",
                "selectors": ["discover"],
                "identity": {"kind": "opencode-go-credential", "key": "86d51bcfa032a515b12d4e62771e42438a823139096ea372ae963cf94e02c90b"},
                "revision": "29b8a654bb2ad6d94ebb6ea609599c4a13dc2e78bbb36026163df81aa4b4dae0",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "71dfd841b8091b9747b194a619f3e913cf3bf6bb10325b4580c33e509009b904",
                "selectors": ["discover"],
                "identity": {"kind": "opencode-go-credential", "key": "a6a9459c8d61944f84af8140f47d988eedc942d1efdd1c57cda49bedf9a2f08b"},
                "revision": "26051d42b1d0a537d2d853d2b9283db4733ec27d2e61f2e162670fecb140a126",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "c990652da8c23df833e1109c68926f0277f0434f35d2e4eb35b26f7c1c16eccb",
                "selectors": ["aac-key:33445566"],
                "identity": {"kind": "opencode-go-credential", "key": "1b8f8024515ae15c3c50f3d5b52e1e78a2406f52a575fb632228963233e7be02"},
                "revision": "859ccbdd96c7adc084e35122c1239a757ade3191a3ac8a6c608ba1b20b080b6d",
                "email": null,
                "plan": null
            }),
        ]
    );
    assert_eq!(
        pub_rows(
            additional::inventory_with("qwen", &home, "ubuntu", Some(&ccs), &a_env).candidates
        ),
        vec![
            json!({
                "locator": "301b679b48406d0618ca711de28fc5ba330e4b30b8cedbbdaf4bcb0be0f6d0cf",
                "selectors": ["browser-capsule:deadbeef"],
                "identity": {"kind": "qwen-session", "key": "fb284edf1a5a6480537942a65337df170a5686d364dc890a7aa821dd167c67ce"},
                "revision": "a364ca6c4c3551a5f3d16de422046ef89a41687fef4e463ec0a11a497224ae7e",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "dbabb005c5583f49bce09dad7311c9fb7384d08c3fc95e38cc593d9f44acd4e2",
                "selectors": ["browser-capsule:default", "discover"],
                "identity": {"kind": "qwen-session", "key": "97fd7bbbf799d89cfc9ace1be920fc238dddf03093ebf513f74aa19855e86e24"},
                "revision": "02d778996d552b0a13670310c4ef941d49c7da26672dfc0faadde0085e1f223c",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "d308aee0f9cf081724ee0400952f21ba1629eb65820080e458279ac74ed30650",
                "selectors": ["discover"],
                "identity": {"kind": "qwen-session", "key": "90276a443156f59d74c391cfbd93049424c4123d82383c990968b526c51f3506"},
                "revision": "8c8a856e87ed66ae323b18464a0119225a4a202d95f9d88980f9d2ecbe6d796d",
                "email": null,
                "plan": null
            }),
            json!({
                "locator": "8a683e9ec95b69d9d81bd6e7938d3f15b1982ab32b9fc22eb3ef4725e8027441",
                "selectors": ["discover"],
                "identity": {"kind": "qwen-session", "key": "62b53b2641902b58ab7521a856ff8903ea0516afd72ff4d32e48389fa79ec8dc"},
                "revision": "b76074583d05952fe01e2348c43837f6e1b7e61260c49bf2371b78e4d33e31e7",
                "email": null,
                "plan": null
            }),
        ]
    );
}

#[tokio::test]
async fn special_saved_registry_edge_cases_and_identity_guards() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join(".ccs");

    // Multiple distinct saved profiles keep distinct identities & selectors.
    let (_, first) = write_saved_agy_profile(&ccs, "first", EMAIL, SUBJECT, 1);
    let (_, second) = write_saved_agy_profile(&ccs, "second", "bob@example.test", "987654321", 1);
    write_private(
        &ccs,
        "antigravity-profiles/registry-000000000002.json",
        json!({
            "version": 1,
            "revision": 2,
            "profiles": [first.clone(), second.clone()],
            "active": {"profileId": "first", "verifiedAt": "2026-01-01T00:00:00Z"},
            "transaction": null
        })
        .to_string()
        .as_bytes(),
    );
    let mut adapter = special::SpecialAdapter::new();
    let inv_val = adapter
        .inventory_for("antigravity", "ubuntu", &home, &ccs)
        .unwrap();
    let rows = inv_val["candidates"].as_array().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["selectors"], json!(["antigravity-profile:first"]));
    assert_eq!(rows[1]["selectors"], json!(["antigravity-profile:second"]));
    assert_ne!(rows[0]["identity"], rows[1]["identity"]);

    // Duplicate profile IDs reject entire registry and mark antigravity-profile:*.
    write_private(
        &ccs,
        "antigravity-profiles/registry-000000000003.json",
        json!({
            "version": 1,
            "revision": 3,
            "profiles": [first.clone(), first],
            "active": null,
            "transaction": null
        })
        .to_string()
        .as_bytes(),
    );
    let dup_inv = special::inventory("antigravity", &home, "ubuntu", Some(&ccs));
    assert!(dup_inv.candidates.is_empty());
    assert_eq!(dup_inv.incomplete_selectors, vec!["antigravity-profile:*"]);

    // Newer corrupt registry never resurrects older valid registry.
    write_private(
        &ccs,
        "antigravity-profiles/registry-000000000004.json",
        b"broken",
    );
    let corrupt_inv = special::inventory("antigravity", &home, "ubuntu", Some(&ccs));
    assert!(corrupt_inv.candidates.is_empty());
    assert_eq!(
        corrupt_inv.incomplete_selectors,
        vec!["antigravity-profile:*"]
    );

    // Verified email missing / false -> identity_mismatch; refresh 429 & identity 429 & empty quota.
    let dir2 = tempfile::tempdir().unwrap();
    let home2 = dir2.path().canonicalize().unwrap();
    let ccs2 = home2.join(".ccs");
    let tok_file = write_private(
        &home2,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json(
            "expired-tok",
            "2020-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "s"),
        )
        .to_string()
        .as_bytes(),
    );
    write_oauth_client(&home2);
    let before = fs::read(&tok_file).unwrap();
    let cand =
        special::inventory("antigravity", &home2, "ubuntu", Some(&ccs2)).candidates[0].clone();

    // Refresh 429 stops before userinfo/quota and leaves native file untouched.
    let g_ref_429 = MockGoogle::new(vec![Err(GoogleHttpError::RateLimited(600))]);
    let r_ref_429 = special::collect_with(
        &cand,
        &home2,
        "ubuntu",
        Some(&ccs2),
        &SpecialEnv {
            google: Some(&g_ref_429),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(
        r_ref_429,
        json!({"sample": null, "failureCode": "rate_limited", "retryAfterSeconds": 600})
    );
    assert_eq!(g_ref_429.call_urls(), vec![AGY_TOKEN_URL.to_owned()]);
    assert_eq!(fs::read(&tok_file).unwrap(), before);

    // Non-expired candidate for verified_email missing, identity 429, and empty quota omission.
    write_private(
        &home2,
        ".gemini/antigravity-cli/antigravity-oauth-token",
        agy_native_json(
            "live-tok",
            "2099-01-01T00:00:00Z",
            &fake_jwt(EMAIL, SUBJECT, "s"),
        )
        .to_string()
        .as_bytes(),
    );
    let live_cand =
        special::inventory("antigravity", &home2, "ubuntu", Some(&ccs2)).candidates[0].clone();
    let g_unverified = MockGoogle::new(vec![Ok(json!({"email": EMAIL, "id": SUBJECT}))]);
    assert_eq!(
        special::collect_with(
            &live_cand,
            &home2,
            "ubuntu",
            Some(&ccs2),
            &SpecialEnv {
                google: Some(&g_unverified),
                ..Default::default()
            },
        )
        .await,
        json!({"sample": null, "failureCode": "identity_mismatch"})
    );
    let g_id_429 = MockGoogle::new(vec![Err(GoogleHttpError::RateLimited(321))]);
    assert_eq!(
        special::collect_with(
            &live_cand,
            &home2,
            "ubuntu",
            Some(&ccs2),
            &SpecialEnv {
                google: Some(&g_id_429),
                ..Default::default()
            },
        )
        .await,
        json!({"sample": null, "failureCode": "rate_limited", "retryAfterSeconds": 321})
    );
    let g_empty_quota = MockGoogle::new(vec![
        Ok(json!({"email": EMAIL, "id": SUBJECT, "verified_email": true})),
        Ok(json!({"cloudaicompanionProject": "proj"})),
        Ok(json!({"groups": []})),
        Ok(json!({"groups": []})),
    ]);
    assert_eq!(
        special::collect_with(
            &live_cand,
            &home2,
            "ubuntu",
            Some(&ccs2),
            &SpecialEnv {
                google: Some(&g_empty_quota),
                ..Default::default()
            },
        )
        .await,
        json!({"sample": null, "failureCode": "unavailable"})
    );

    // Wallet wrong workspace -> identity_mismatch; rate_limited without retry; expired capsule.
    let now = 1_792_000_000.0;
    write_private(
        &ccs2,
        "account-usage/opencode-console-session.json",
        json!({
            "schemaVersion": 1,
            "origin": "https://opencode.ai",
            "source": "browser-native-cookie-api",
            "workspaceId": "wrk_mine",
            "capturedAt": "2026-10-14T17:46:40Z",
            "cookies": [{"name": "auth", "value": "TOK", "domain": "opencode.ai", "path": "/", "secure": true, "hostOnly": true}]
        })
        .to_string()
        .as_bytes(),
    );
    let w_cand = special::inventory_with(
        "opencode-wallet",
        &home2,
        "mac",
        Some(&ccs2),
        &SpecialEnv {
            now_secs: Some(now),
            wallet_supported: Some(true),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    let w_wrong = MockWallet::new(vec![(
        "/console/api/orgs",
        Ok(json!([{"id": "wrk_other"}])),
    )]);
    assert_eq!(
        special::collect_with(
            &w_cand,
            &home2,
            "mac",
            Some(&ccs2),
            &SpecialEnv {
                now_secs: Some(now),
                wallet: Some(&w_wrong),
                wallet_supported: Some(true),
                ..Default::default()
            },
        )
        .await,
        json!({"sample": null, "failureCode": "identity_mismatch"})
    );
    let w_429_noretry = MockWallet::new(vec![(
        "/console/api/orgs",
        Err(WalletError {
            code: "rate_limited",
            retry_after_seconds: None,
        }),
    )]);
    assert_eq!(
        special::collect_with(
            &w_cand,
            &home2,
            "mac",
            Some(&ccs2),
            &SpecialEnv {
                now_secs: Some(now),
                wallet: Some(&w_429_noretry),
                wallet_supported: Some(true),
                ..Default::default()
            },
        )
        .await,
        json!({"sample": null, "failureCode": "rate_limited"})
    );
    // Expired capsule is rejected and marks wallet incomplete because file exists.
    write_private(
        &ccs2,
        "account-usage/opencode-console-session.json",
        json!({
            "schemaVersion": 1,
            "origin": "https://opencode.ai",
            "source": "browser-native-cookie-api",
            "workspaceId": "wrk_mine",
            "capturedAt": "2020-01-01T00:00:00Z",
            "cookies": [{"name": "auth", "value": "TOK", "domain": "opencode.ai", "path": "/", "secure": true, "hostOnly": true}]
        })
        .to_string()
        .as_bytes(),
    );
    let w_exp = special::inventory_with(
        "opencode-wallet",
        &home2,
        "mac",
        Some(&ccs2),
        &SpecialEnv {
            now_secs: Some(now),
            wallet_supported: Some(true),
            ..Default::default()
        },
    );
    assert!(w_exp.candidates.is_empty());
    assert_eq!(w_exp.incomplete_selectors, vec!["wallet"]);
}

#[tokio::test]
#[cfg_attr(
    windows,
    ignore = "the Muse console capsule store is Unix-only, as in muse_console.py"
)]
async fn additional_muse_native_budget_capsule_races_and_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join("scope");
    let env_map = test_env(&home);
    let now = 1_792_000_000.0;

    write_private(
        &home,
        ".config/muse/auth.json",
        json!({"providers": {"meta": {"access_token": "dca:native-zero", "user_email": "alice@example.test"}}})
            .to_string()
            .as_bytes(),
    );

    // 1. Native real zero without capsule is cached in muse-native and reused inside budget.
    let native_cand = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    assert_eq!(native_cand["identity"]["kind"], "muse-native-credential");
    let client_zero = MockPlanTransport::json_replies(vec![Ok(json!({
        "user_email": "alice@example.test",
        "subs_tier_name": "Pro",
        "is_subs_active": true,
        "subs_usage": {
            "window": {"used_percent": 0, "window_duration_mins": 300, "resets_at": "2026-10-15T00:00:00Z"}
        }
    }))]);
    let first = additional::collect_with(
        &native_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            plan_transport: Some(&client_zero),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(first["failureCode"], Value::Null);
    assert_eq!(first["sample"]["windows"][0]["usedPercent"], 0.0);
    assert_eq!(client_zero.call_count(), 1);

    let reused = additional::collect_with(
        &native_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now + 10.0),
            env_vars: Some(&env_map),
            plan_transport: Some(&client_zero),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(reused["failureCode"], Value::Null);
    assert_eq!(reused["sample"]["windows"][0]["usedPercent"], 0.0);
    assert_eq!(client_zero.call_count(), 1);

    // 2. Live wrong email -> account_mismatch.
    let client_wrong = MockPlanTransport::json_replies(vec![Ok(json!({
        "user_email": "other@example.test",
        "subs_tier_name": "Pro",
        "is_subs_active": true
    }))]);
    let wrong = additional::collect_with(
        &native_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now + 400.0),
            env_vars: Some(&env_map),
            plan_transport: Some(&client_wrong),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(wrong["failureCode"], "account_mismatch");
    assert_eq!(wrong["sample"], Value::Null);

    // 3. Foreign capsule (different email) leaves native identity unchanged.
    write_private(
        &ccs,
        "account-usage/muse-console-session.json",
        json!({
            "schemaVersion": 1,
            "provider": "muse",
            "origin": "https://dev.meta.ai",
            "email": "stranger@example.test",
            "plan": "Pro",
            "teamId": "99",
            "cookies": [{"name": "llama_dev_sess", "value": "foreign-sess", "domain": "dev.meta.ai", "path": "/", "secure": true, "hostOnly": true}]
        })
        .to_string()
        .as_bytes(),
    );
    let foreign_inv = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    );
    assert_eq!(
        foreign_inv.candidates[0]["identity"]["kind"],
        "muse-native-credential"
    );

    // 4. Matching capsule: portal cookie rotation preserves muse-team identity & changes revision;
    //    different teamId changes identity; collecting stale candidate returns source_changed;
    //    expired capsule retains identity in inventory but fails collect with no_browser_cookie.
    let write_cap = |team: &str, cookie_val: &str, exp: Option<f64>| {
        let mut cookie = json!({
            "name": "llama_dev_sess",
            "value": cookie_val,
            "domain": "dev.meta.ai",
            "path": "/",
            "secure": true,
            "hostOnly": true
        });
        if let Some(e) = exp {
            cookie["expirationDate"] = json!(e);
        }
        write_private(
            &ccs,
            "account-usage/muse-console-session.json",
            json!({
                "schemaVersion": 1,
                "provider": "muse",
                "origin": "https://dev.meta.ai",
                "email": "alice@example.test",
                "plan": "Pro",
                "teamId": team,
                "cookies": [cookie]
            })
            .to_string()
            .as_bytes(),
        );
    };
    write_cap("101", "sess-a", None);
    let team101_a = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    // Collecting the old native_cand now that a matching capsule appeared must reject with source_changed!
    let native_raced = additional::collect_with(
        &native_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(native_raced["failureCode"], "source_changed");

    write_cap("101", "sess-b", None);
    let team101_b = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    assert_eq!(team101_a["identity"], team101_b["identity"]);
    assert_ne!(team101_a["revision"], team101_b["revision"]);

    // Stale team101_a fails collect with source_changed before making any provider calls.
    let stale_res = additional::collect_with(
        &team101_a,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(stale_res["failureCode"], "source_changed");

    write_cap("202", "sess-b", Some(now - 5.0));
    let team202_exp = additional::inventory_with(
        "muse",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    assert_eq!(team202_exp["identity"]["kind"], "muse-team");
    assert_ne!(team202_exp["identity"], team101_b["identity"]);
    let exp_collect = additional::collect_with(
        &team202_exp,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(exp_collect["failureCode"], "no_browser_cookie");

    // AdditionalAdapter entrypoint sanity check.
    let mut adapter = additional::AdditionalAdapter::new();
    let inv_obj = adapter
        .inventory_for("muse", "ubuntu", &home, &ccs)
        .unwrap();
    assert!(inv_obj["candidates"].is_array());
}

#[tokio::test]
async fn additional_cursor_all_plans_resets_and_optional_failures() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join("scope");
    let env_map = test_env(&home);

    write_private(
        &home,
        ".config/Cursor/auth.json",
        json!({"accessToken": fake_jwt("cur@example.test", "auth0|cur", "sig")})
            .to_string()
            .as_bytes(),
    );
    let cand = additional::inventory_with(
        "cursor",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();

    // All 4 new plans normalize shared pool appropriately.
    for (plan, expect_key) in [
        ("plus", "grok-shared-weekly"),
        ("premium", "grok-shared-weekly"),
        ("super", "grok-shared-weekly"),
        ("unified_ultra", "grok-shared-weekly"),
    ] {
        let w = additional::cursor_normalize_shared(
            &json!({"enabled": true, "planUsage": {"totalPercentUsed": 42.5}}),
            &json!({"isMerged": true, "planInfo": {"includedUsagePeriod": "INCLUDED_USAGE_PERIOD_WEEKLY", "includedUsageResetsAt": "1792022400000"}}),
            plan,
        );
        assert_eq!(w.len(), 1, "plan {plan}");
        assert_eq!(w[0]["key"], expect_key);
        assert_eq!(w[0]["usedPercent"], 42.5);
        assert_eq!(w[0]["resetAt"], "2026-10-15T00:00:00Z");
    }

    // Unknown tier with isMerged: true uses shared total; with isMerged: false uses legacy summary.
    let client_unknown_merged = MockPlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "future_tier"})),
        Ok(
            json!({"isMerged": true, "planInfo": {"includedUsagePeriod": "INCLUDED_USAGE_PERIOD_MONTHLY"}}),
        ),
        Ok(
            json!({"enabled": true, "billingCycleEnd": "1794697200000", "planUsage": {"totalPercentUsed": 33.0}}),
        ),
        Ok(json!({"individualUsage": {"plan": {"used": 0, "limit": 100}}})),
        Ok(json!({})),
    ]);
    let res_merged = additional::collect_with(
        &cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_unknown_merged),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(res_merged["failureCode"], Value::Null);
    assert_eq!(res_merged["sample"]["windows"][0]["usedPercent"], 33.0);

    // Primary endpoint failure still exposes observedPlan when membership succeeded, and does not fabricate sample.
    let client_primary_fail = MockPlanTransport::json_replies(vec![
        Ok(json!({"membershipType": "ultra"})),
        Err(CollectionError {
            status: "error",
            message: "down",
            failure_code: Some("rate_limited"),
            retry_after: Some(600.0),
        }),
    ]);
    let res_fail = additional::collect_with(
        &cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&client_primary_fail),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(res_fail["sample"], Value::Null);
    assert_eq!(res_fail["failureCode"], "rate_limited");
    assert_eq!(res_fail["retryAfterSeconds"], 600.0);
    assert_eq!(res_fail["observedPlan"], "ultra");
}

#[tokio::test]
async fn additional_plans_kimi_zai_go_qwen_edge_cases() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().canonicalize().unwrap();
    let ccs = home.join("scope");
    let env_map = test_env(&home);
    let now = 1_792_000_000.0;

    // 1. plan_retry_after_seconds clamps to [30, 86400] with 600 default.
    assert_eq!(additional::plan_retry_after_seconds(Some("5"), now), 30.0);
    assert_eq!(
        additional::plan_retry_after_seconds(Some("999999"), now),
        86400.0
    );
    assert_eq!(additional::plan_retry_after_seconds(None, now), 600.0);
    assert_eq!(
        additional::plan_retry_after_seconds(Some("bogus"), now),
        600.0
    );
    assert_eq!(
        additional::plan_retry_after_seconds(
            Some("Wed, 14 Oct 2026 12:10:00 GMT"),
            1_791_979_200.0
        ),
        600.0
    );

    // 2. OpenCode Go: 403 -> provider_error (never needs_sign_in), 401 -> needs_sign_in, 429 -> rate_limited.
    write_private(
        &ccs,
        "account-usage/keys/opencode-go-aaaaaaaa.json",
        json!({"version": 1, "provider": "opencode-go", "keyId": "aaaaaaaa", "secret": "sk-go-test-secret"})
            .to_string()
            .as_bytes(),
    );
    let go_cand = additional::inventory_with(
        "opencode-go",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            force_plaintext_keys: Some(true),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    let go_403 = MockPlanTransport::json_replies(vec![Err(CollectionError {
        status: "error",
        message: "OpenCode Go usage access is unavailable",
        failure_code: Some("provider_error"),
        retry_after: None,
    })]);
    let res_403 = additional::collect_with(
        &go_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&go_403),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(res_403["failureCode"], "provider_error");

    // 3. Kimi Code: boosterWallet balance + transitional zero placeholder handling.
    write_private(
        &ccs,
        "account-usage/keys/kimi-code-bbbbbbbb.json",
        json!({"version": 1, "provider": "kimi-code", "keyId": "bbbbbbbb", "secret": "sk-kimi-test-secret"})
            .to_string()
            .as_bytes(),
    );
    let kimi_cand = additional::inventory_with(
        "kimi-code",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            force_plaintext_keys: Some(true),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    let kimi_client = MockPlanTransport::json_replies(vec![Ok(json!({
        "user": {"membership": {"level": "LEVEL_ADVANCED"}},
        "version": "GOODS_VERSION_V2",
        "usage": {"limit": 100, "used": 25, "remaining": 75, "resetTime": "2026-10-20T00:00:00Z"},
        "limits": [{
            "window": {"duration": 5, "timeUnit": "TIME_UNIT_HOUR"},
            "detail": {"limit": 100, "used": 10, "remaining": 90, "resetTime": "2026-10-15T00:00:00Z"}
        }],
        "usages": {
            "limit_7d": {"used_ratio": 0, "reset_time": "2026-10-20T00:00:01Z"}
        },
        "boosterWallet": {
            "balance": {"type": "BOOSTER", "amount": 2_000_000_000_i64, "amountLeft": 1_250_000_000_i64},
            "monthlyChargeLimit": {"priceInCents": 5000, "currency": "USD"},
            "monthlyUsed": {"priceInCents": 1000, "currency": "USD"},
            "monthlyChargeLimitEnabled": true
        }
    }))]);
    let kimi_res = additional::collect_with(
        &kimi_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&kimi_client),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(kimi_res["failureCode"], Value::Null);
    assert_eq!(kimi_res["sample"]["plan"], "Advanced");
    let kw = kimi_res["sample"]["windows"].as_array().unwrap();
    assert!(
        kw.iter()
            .any(|w| w["key"] == "5h" && w["usedPercent"] == 10.0)
    );
    assert!(
        kw.iter()
            .any(|w| w["key"] == "weekly" && w["usedPercent"] == 25.0)
    );
    assert!(kw.iter().any(|w| w["key"] == "booster-balance"
        && w["kind"] == "balance"
        && w["remaining"] == 12.5
        && w["unit"] == "USD"));
    assert!(kw.iter().any(|w| w["key"] == "booster-monthly-spend"
        && w["kind"] == "spend"
        && w["used"] == 10.0
        && w["limit"] == 50.0
        && w["enabled"] == true));

    // 4. Z.ai reset packs: Beijing timestamp (+08:00 -> UTC) + expired pack filtering + network fallback.
    write_private(
        &ccs,
        "account-usage/keys/zai-cccccccc.json",
        json!({"version": 1, "provider": "zai", "keyId": "cccccccc", "secret": "sk-zai-test-secret"})
            .to_string()
            .as_bytes(),
    );
    let zai_cand = additional::inventory_with(
        "zai",
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            env_vars: Some(&env_map),
            force_plaintext_keys: Some(true),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    let zai_packs_ok = MockPlanTransport::json_replies(vec![
        Ok(
            json!({"success": true, "code": 200, "data": {"level": "pro", "limits": [{"type": "TOKENS_LIMIT", "number": 5, "unit": 3, "percentage": 20}]}}),
        ),
        Ok(json!({
            "success": true,
            "code": 200,
            "data": {
                "fiveHourResets": [
                    {"available": true, "expireTime": "2026-10-20 08:00:00"},
                    {"available": false, "expireTime": "2026-10-20 08:00:00"},
                    {"available": true, "expireTime": "2020-01-01 08:00:00"}
                ]
            }
        })),
    ]);
    let zai_ok = additional::collect_with(
        &zai_cand,
        &home,
        "ubuntu",
        Some(&ccs),
        &AdditionalEnv {
            now_secs: Some(now),
            env_vars: Some(&env_map),
            plan_transport: Some(&zai_packs_ok),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(zai_ok["failureCode"], Value::Null);
    let zw = zai_ok["sample"]["windows"].as_array().unwrap();
    let pack = zw.iter().find(|w| w["key"] == "reset-packs-5h").unwrap();
    assert_eq!(pack["remaining"], 1.0);
    assert_eq!(pack["used"], 1.0);
    assert_eq!(pack["limit"], 2.0);
    assert_eq!(pack["expiresAt"], "2026-10-20T00:00:00Z");

    // 5. Qwen cookie order-invariance: "b=2; a=1" and "a=1; b=2" produce identical identity & revision.
    let dir_q1 = tempfile::tempdir().unwrap();
    let hq1 = dir_q1.path().canonicalize().unwrap();
    let dir_q2 = tempfile::tempdir().unwrap();
    let hq2 = dir_q2.path().canonicalize().unwrap();
    let q_row1 = json!({"key": json!({"cookie": "b=2; a=1", "gatewayCookie": "y=2; x=1", "baseUrl": "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"}).to_string()}).to_string();
    let q_row2 = json!({"key": json!({"cookie": "a=1; b=2", "gatewayCookie": "x=1; y=2", "baseUrl": "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"}).to_string()}).to_string();
    write_omp_db(
        &hq1,
        &[(1, "alibaba-token-plan", "api_key", Some(&q_row1), None)],
    );
    write_omp_db(
        &hq2,
        &[(1, "alibaba-token-plan", "api_key", Some(&q_row2), None)],
    );
    let q1 = additional::inventory_with(
        "qwen",
        &hq1,
        "ubuntu",
        None,
        &AdditionalEnv {
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    let q2 = additional::inventory_with(
        "qwen",
        &hq2,
        "ubuntu",
        None,
        &AdditionalEnv {
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    assert_eq!(q1["identity"], q2["identity"]);
    assert_eq!(q1["revision"], q2["revision"]);

    // Qwen CN region HTML SEC_TOKEN extraction & quota parsing.
    let dir_qcn = tempfile::tempdir().unwrap();
    let hqcn = dir_qcn.path().canonicalize().unwrap();
    let q_cn_row = json!({"key": json!({"cookie": "sid=cn", "baseUrl": "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"}).to_string()}).to_string();
    write_omp_db(
        &hqcn,
        &[(1, "alibaba-token-plan", "api_key", Some(&q_cn_row), None)],
    );
    let qcn_cand = additional::inventory_with(
        "qwen",
        &hqcn,
        "ubuntu",
        None,
        &AdditionalEnv {
            env_vars: Some(&env_map),
            ..Default::default()
        },
    )
    .candidates[0]
        .clone();
    let qcn_transport = MockPlanTransport {
        calls: Arc::new(Mutex::new(Vec::new())),
        replies: Arc::new(Mutex::new(VecDeque::from(vec![
            Ok(PlanHttpBody::Text(
                r#"<script>window.CONFIG = { SEC_TOKEN: "cn-sec-tok-123" };</script>"#.to_owned(),
            )),
            Ok(PlanHttpBody::Json(json!({
                "successResponse": true,
                "data": {
                    "per5HourPercentage": 0.25,
                    "per5HourResetTime": "2026-10-15T00:00:00Z",
                    "per1WeekPercentage": 0.1,
                    "per1WeekResetTime": "2026-10-20T00:00:00Z"
                }
            }))),
            Ok(PlanHttpBody::Json(json!({}))),
            Ok(PlanHttpBody::Json(json!({}))),
            Ok(PlanHttpBody::Json(json!({}))),
        ]))),
    };
    let qcn_res = additional::collect_with(
        &qcn_cand,
        &hqcn,
        "ubuntu",
        None,
        &AdditionalEnv {
            env_vars: Some(&env_map),
            plan_transport: Some(&qcn_transport),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(qcn_res["failureCode"], Value::Null);
    let qw = qcn_res["sample"]["windows"].as_array().unwrap();
    assert_eq!(qw[0]["key"], "5h");
    assert_eq!(qw[0]["usedPercent"], 25.0);
    assert_eq!(qw[1]["key"], "weekly");
    assert_eq!(qw[1]["usedPercent"], 10.0);
}
