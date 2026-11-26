//! Port of `scripts/account-usage/desktop_helpers.py`, `desktop_usage.py`, and `cursor_usage.py`.
//!
//! Shares core parsing, SQLite, Cursor, and Muse portal/native logic with `failover_additional`
//! while providing the complete standalone desktop collector surface (including Antigravity
//! per-model Gemini derivation, two-service deadline budgets, Windows ACL metadata checks, and
//! bounded `desktop_helpers.request_json` HTTP safety).
mod antigravity;
mod cursor;
mod helpers;
mod muse;

pub use helpers::ReqwestDesktopTransport;

use crate::failover_additional::{
    CollectionError, PlanTransport, PortalTransport, cursor as shared_cursor, desktop,
};
use aac_providers::ProviderId;
use serde_json::{Map, Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};

/// Maximum response bytes for desktop HTTP calls (`desktop_helpers.MAX_RESPONSE_BYTES`).
pub const MAX_RESPONSE_BYTES: usize = desktop::MAX_RESPONSE_BYTES;
/// Per-request socket timeout in seconds (`desktop_helpers.HTTP_TIMEOUT`).
pub const HTTP_TIMEOUT_SECS: u64 = 8;
/// Muse native key endpoint (`desktop_usage.MUSE_URL`).
pub const MUSE_URL: &str = desktop::MUSE_URL;
/// Antigravity production (app) quota base URL (`desktop_usage.AGY_BASE`).
pub const AGY_BASE: &str = desktop::AGY_BASE;
/// Antigravity daily (CLI / T3) quota base URL (`desktop_usage.AGY_CLI_BASE`).
pub const AGY_CLI_BASE: &str = desktop::AGY_CLI_BASE;
/// Antigravity CLI key prefix (`desktop_usage.AGY_CLI_KEY_PREFIX`).
pub const AGY_CLI_KEY_PREFIX: &str = desktop::AGY_CLI_KEY_PREFIX;
/// Antigravity native client User-Agent (`desktop_usage.AGY_USER_AGENT`).
pub const AGY_USER_AGENT: &str = desktop::AGY_USER_AGENT;
/// Required Antigravity summary deadline budget in seconds (`desktop_usage.AGY_REQUIRED_BUDGET_SECONDS`).
pub const AGY_REQUIRED_BUDGET_SECONDS: f64 = 14.0;
/// Optional Antigravity per-model deadline budget in seconds (`desktop_usage.AGY_OPTIONAL_BUDGET_SECONDS`).
pub const AGY_OPTIONAL_BUDGET_SECONDS: f64 = 13.0;
/// Gemini reset cluster threshold in seconds (`desktop_usage.GEMINI_RESET_CLUSTER_SECONDS`).
pub const GEMINI_RESET_CLUSTER_SECONDS: f64 = 10.0;
/// Cursor legacy summary URL (`cursor_usage.SUMMARY_URL`).
pub const CURSOR_SUMMARY_URL: &str = shared_cursor::SUMMARY_URL;
/// Cursor profile URL (`cursor_usage.PROFILE_URL`).
pub const CURSOR_PROFILE_URL: &str = shared_cursor::PROFILE_URL;
/// Cursor Stripe membership URL (`cursor_usage.MEMBERSHIP_URL`).
pub const CURSOR_MEMBERSHIP_URL: &str = shared_cursor::MEMBERSHIP_URL;
/// Cursor Connect GetPlanInfo URL (`cursor_usage.PLAN_INFO_URL`).
pub const CURSOR_PLAN_INFO_URL: &str = shared_cursor::PLAN_INFO_URL;
/// Cursor Connect GetCurrentPeriodUsage URL (`cursor_usage.CURRENT_USAGE_URL`).
pub const CURSOR_CURRENT_USAGE_URL: &str = shared_cursor::CURRENT_USAGE_URL;

/// Safe desktop usage error matching `desktop_helpers.UsageError`.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageError {
    /// Fixed status (`"error"`, `"needs_sign_in"`, `"unavailable"`, `"rate_limited"`).
    pub status: &'static str,
    /// Fixed scrubbed human message (never upstream text or credentials).
    pub message: &'static str,
    /// Optional structured failure code (`"rate_limited"`, etc.).
    pub failure_code: Option<&'static str>,
    /// Optional integer retry-after seconds (`300..=86400` from HTTP 429).
    pub retry_after: Option<u64>,
}

impl UsageError {
    /// Create a plain `UsageError` without structured failure code or retry interval.
    pub fn new(status: &'static str, message: &'static str) -> Self {
        Self {
            status,
            message,
            failure_code: None,
            retry_after: None,
        }
    }
}

/// Private in-memory desktop credential (`desktop_helpers.Credential`); secrets are redacted in `Debug`.
#[derive(Clone, PartialEq)]
pub struct Credential {
    /// Access token (never printed in `Debug`).
    pub access: String,
    /// Human credential source label.
    pub source: String,
    /// Optional refresh token (never printed in `Debug`).
    pub refresh: String,
    /// Optional expiration timestamp in Unix milliseconds.
    pub expires: Option<f64>,
    /// Optional email hint.
    pub email: Option<String>,
    /// Optional Google project ID hint.
    pub project: Option<String>,
    /// Optional plan hint.
    pub plan: Option<String>,
    /// Optional Google OAuth client kind (`"consumer"` or `"antigravity"`).
    pub google_client: Option<String>,
}

impl std::fmt::Debug for Credential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Credential")
            .field("source", &self.source)
            .field("email", &self.email)
            .field("plan", &self.plan)
            .field("google_client", &self.google_client)
            .finish()
    }
}

/// Injectable HTTP transport matching `desktop_helpers.request_json`.
pub trait DesktopTransport: Send + Sync {
    /// Issue a bounded JSON or form-encoded HTTP request.
    fn request_json<'a>(
        &'a self,
        url: &'a str,
        headers: &'a [(String, String)],
        body: Option<&'a Value>,
        form: bool,
    ) -> Pin<Box<dyn Future<Output = Result<Value, UsageError>> + Send + 'a>>;
}

/// Result of one Antigravity quota service read (`desktop_usage.antigravity_services`).
#[derive(Debug, Clone)]
pub struct AntigravityServiceResult {
    /// Service key (`"cli"` or `"app"`).
    pub service: &'static str,
    /// Service base URL.
    pub base: &'static str,
    /// Service status (`"ok"`, `"unavailable"`, or `"rate_limited"`).
    pub status: &'static str,
    /// Raw group objects extracted from summary.
    pub groups: Vec<Value>,
    /// Normalized windows (unlabelled by service).
    pub windows: Vec<Value>,
    /// Optional retry-after seconds from summary or optional per-model throttle.
    pub retry_after: Option<u64>,
    /// Optional service failure.
    pub error: Option<UsageError>,
}

/// Optional Windows ACL verification hook for cross-OS tests.
pub type WindowsAclCheck<'a> = dyn Fn(&Path) -> bool + Send + Sync + 'a;

/// Optional elapsed-seconds clock hook for deterministic Antigravity budget tests.
pub type ElapsedClock<'a> = dyn Fn() -> f64 + Send + Sync + 'a;

/// Optional overrides for deterministic offline tests; production uses `Default`.
#[derive(Default)]
pub struct DesktopEnv<'a> {
    /// Override current UTC timestamp in Unix seconds.
    pub now_secs: Option<f64>,
    /// Override elapsed seconds since collection start (for Antigravity deadline budget).
    pub elapsed: Option<&'a ElapsedClock<'a>>,
    /// Override environment variables (`XDG_CONFIG_HOME`, `APPDATA`).
    pub env_vars: Option<&'a BTreeMap<String, String>>,
    /// Override `desktop_helpers.request_json` HTTP transport.
    pub desktop_transport: Option<&'a dyn DesktopTransport>,
    /// Override `plan_common.HttpClient` transport (used by Cursor).
    pub plan_transport: Option<&'a dyn PlanTransport>,
    /// Override `muse_console.PortalClient` transport (used by Muse).
    pub portal_transport: Option<&'a dyn PortalTransport>,
    /// Override Windows private-metadata ACL verification.
    pub windows_acl: Option<&'a WindowsAclCheck<'a>>,
    /// Override discovered credential (mirrors patching `*_credentials` in Python unit tests).
    pub credential: Option<Credential>,
    /// Override Antigravity services list (e.g. legacy single-service fixtures).
    pub agy_services: Option<&'a [(&'static str, &'static str)]>,
}

/// Parse a finite JSON number (rejecting booleans, NaN, Inf).
pub fn number(value: &Value) -> Option<f64> {
    desktop::number(value)
}

/// Parse a non-negative finite JSON number.
pub fn nonnegative(value: &Value) -> Option<f64> {
    desktop::nonnegative(value)
}

/// Parse a non-negative counter (number or numeric string).
pub fn counter(value: &Value) -> Option<f64> {
    desktop::counter(value)
}

/// Parse a non-negative percentage rounded to 4 decimal places.
pub fn percent(value: &Value) -> Option<f64> {
    desktop::percent(value)
}

/// Sanitize display text (`desktop_helpers.safe_text`).
pub fn safe_text(value: &Value, maximum: usize) -> Option<String> {
    desktop::safe_text(value, maximum)
}

/// Validate a display email address (`desktop_helpers.email`).
pub fn email(value: &Value) -> Option<String> {
    desktop::email(value)
}

/// Validate an in-memory secret string (`desktop_helpers.secret`).
pub fn secret(value: &Value) -> Option<String> {
    desktop::secret(value)
}

/// Normalize a supplied timestamp into UTC ISO-8601 seconds (`desktop_helpers.reset_at`).
pub fn reset_at(value: &Value) -> Option<String> {
    desktop::reset_at(value)
}

/// Bound an HTTP `Retry-After` header (`desktop_helpers.retry_after_seconds`, `300..=86400`, default `600`).
pub fn retry_after_seconds(value: &Value, now_secs: Option<f64>) -> u64 {
    desktop::desktop_retry_after_seconds(value, now_secs)
}

/// Recognize an OAuth 400 `invalid_grant` JSON body (`desktop_helpers.is_invalid_grant`).
pub fn is_invalid_grant(raw: &[u8]) -> bool {
    desktop::is_invalid_grant(raw)
}

/// Decode unverified JWT payload claims (`desktop_helpers.jwt_claims`).
pub fn jwt_claims(token: &str) -> Value {
    desktop::jwt_claims(token)
}

/// Build an unavailable desktop account skeleton (`desktop_helpers.account`).
pub fn account(provider: &str, platform: &str) -> Value {
    desktop::account(provider, platform)
}

/// Resolve `XDG_CONFIG_HOME` or `<home>/.config`.
pub fn config_home(home: &Path, env_vars: Option<&BTreeMap<String, String>>) -> PathBuf {
    desktop::config_home(home, env_vars)
}

/// Resolve platform Cursor configuration root (`desktop_usage.cursor_root`).
pub fn cursor_root(
    home: &Path,
    platform: &str,
    env_vars: Option<&BTreeMap<String, String>>,
) -> PathBuf {
    desktop::cursor_root(home, platform, env_vars)
}

/// Discover Muse credentials (`desktop_usage.muse_credentials`).
pub fn muse_credentials(
    home: &Path,
    env_vars: Option<&BTreeMap<String, String>>,
) -> Option<Credential> {
    let mut tracker = desktop::ReadTracker::default();
    muse::muse_credentials(home, env_vars, &mut tracker)
}

/// Discover Antigravity credentials (`desktop_usage.antigravity_credentials`).
pub fn antigravity_credentials(home: &Path) -> Option<Credential> {
    let mut tracker = desktop::ReadTracker::default();
    antigravity::antigravity_credentials(home, &mut tracker)
}

/// Discover Cursor credentials (`desktop_usage.cursor_credentials`).
pub fn cursor_credentials(
    home: &Path,
    platform: &str,
    env_vars: Option<&BTreeMap<String, String>>,
) -> Option<Credential> {
    let mut tracker = desktop::ReadTracker::default();
    cursor::cursor_credentials(home, platform, env_vars, &mut tracker)
}

/// Format a Muse window label (`desktop_usage.muse_window_label`).
pub fn muse_window_label(key: &str, minutes: Option<f64>) -> String {
    desktop::muse_window_label(key, minutes)
}

/// Normalize Muse quota windows (`desktop_usage.normalize_muse`).
pub fn normalize_muse(payload: &Value) -> Vec<Value> {
    desktop::normalize_muse(payload)
}

/// Extract Antigravity quota groups (`desktop_usage.agy_groups`).
pub fn agy_groups(payload: &Value) -> Vec<Value> {
    desktop::agy_groups(payload)
}

/// Normalize Antigravity summary quota windows (`desktop_usage.normalize_antigravity`).
pub fn normalize_antigravity(payload: &Value) -> Vec<Value> {
    desktop::normalize_antigravity(payload)
}

/// Build unavailable Antigravity service windows from a sibling service's pools
/// (`desktop_usage.antigravity_unavailable_windows`).
pub fn antigravity_unavailable_windows(windows: &[Value]) -> Vec<Value> {
    desktop::antigravity_unavailable_windows(windows)
}

/// Prefix and label Antigravity windows by service (`desktop_usage.antigravity_service_label`).
pub fn antigravity_service_label(windows: Vec<Value>, service: &str) -> Vec<Value> {
    desktop::antigravity_service_label(windows, service)
}

/// Load Antigravity account project/plan via `loadCodeAssist` (`desktop_usage.antigravity_load`).
pub async fn antigravity_load(
    transport: &dyn DesktopTransport,
    headers: &[(String, String)],
) -> Result<Value, UsageError> {
    antigravity::antigravity_load(transport, headers).await
}

/// Whether summary windows already report the Gemini Models group (`desktop_usage.antigravity_summary_has_gemini`).
pub fn antigravity_summary_has_gemini(summary_windows: &[Value]) -> bool {
    antigravity::antigravity_summary_has_gemini(summary_windows)
}

/// Derive Gemini Models windows from per-model buckets (`desktop_usage.normalize_antigravity_gemini`).
pub fn normalize_antigravity_gemini(payload: &Value, summary_windows: &[Value]) -> Vec<Value> {
    antigravity::normalize_antigravity_gemini(payload, summary_windows)
}

/// Extract Antigravity reported plan fields (`desktop_usage.antigravity_reported_plan`).
pub fn antigravity_reported_plan(load: &Value) -> (Option<String>, Option<Value>) {
    desktop::antigravity_reported_plan(load)
}

/// Normalize Antigravity optional shared AI credits (`desktop_usage.normalize_antigravity_credits`).
pub fn normalize_antigravity_credits(load: &Value) -> Vec<Value> {
    desktop::normalize_antigravity_credits(load)
}

/// Read installed-app Antigravity OAuth client metadata privately (`desktop_usage.antigravity_oauth_client`).
pub fn antigravity_oauth_client_with<'a>(
    home: &Path,
    windows_acl: Option<&'a WindowsAclCheck<'a>>,
) -> Result<(String, String), UsageError> {
    desktop::antigravity_oauth_client_with(home, windows_acl).map_err(helpers::from_internal_err)
}

/// Read installed-app Antigravity OAuth client metadata privately (`desktop_usage.antigravity_oauth_client`).
pub fn antigravity_oauth_client(home: &Path) -> Result<(String, String), UsageError> {
    antigravity_oauth_client_with(home, None)
}

/// Refresh an Antigravity OAuth access token in memory (`desktop_usage.refresh_antigravity`).
pub async fn refresh_antigravity<'a>(
    credential: &Credential,
    home: &Path,
    transport: &dyn DesktopTransport,
    windows_acl: Option<&'a WindowsAclCheck<'a>>,
) -> Result<Credential, UsageError> {
    antigravity::refresh_antigravity(credential, home, transport, windows_acl).await
}

/// Read both Antigravity quota services (`desktop_usage.antigravity_services`).
pub async fn antigravity_services<'a>(
    transport: &dyn DesktopTransport,
    headers: &[(String, String)],
    project: &str,
    elapsed: &'a ElapsedClock<'a>,
    per_model: bool,
    services_override: Option<&[(&'static str, &'static str)]>,
) -> Result<Vec<AntigravityServiceResult>, UsageError> {
    antigravity::antigravity_services(
        transport,
        headers,
        project,
        elapsed,
        per_model,
        services_override,
    )
    .await
}

/// Fetch Antigravity quota with proactive/401 refresh (`desktop_usage.fetch_antigravity`).
pub async fn fetch_antigravity(
    credential: &Credential,
    result: &mut Value,
    home: &Path,
    env: &DesktopEnv<'_>,
) -> Result<(), UsageError> {
    antigravity::fetch_antigravity(credential, result, home, env).await
}

/// Fetch Antigravity quota using the provided access token (`desktop_usage.fetch_antigravity_quota`).
pub async fn fetch_antigravity_quota(
    credential: &Credential,
    result: &mut Value,
    env: &DesktopEnv<'_>,
) -> Result<(), UsageError> {
    antigravity::fetch_antigravity_quota(credential, result, env).await
}

/// Build Cursor session cookie (`desktop_usage.cursor_cookie`).
pub fn cursor_cookie(credential: &Credential) -> Result<String, UsageError> {
    cursor::cursor_cookie(credential)
}

/// Normalize Cursor legacy usage summary (`cursor_usage.normalize_legacy` / `desktop_usage.normalize_cursor`).
pub fn cursor_normalize_legacy(payload: &Value) -> Vec<Value> {
    shared_cursor::normalize_legacy(payload)
}

/// Normalize Cursor shared pool usage (`cursor_usage.normalize_shared`).
pub fn cursor_normalize_shared(payload: &Value, plan_info: &Value, plan: &str) -> Vec<Value> {
    shared_cursor::normalize_shared(payload, plan_info, plan)
}

/// Normalize Cursor additional on-demand spend (`cursor_usage.normalize_additional`).
pub fn cursor_normalize_additional(payload: &Value) -> Vec<Value> {
    shared_cursor::normalize_additional(payload)
}

/// Parse Cursor Stripe membership tier (`cursor_usage.membership`).
pub fn cursor_membership(value: &Value) -> Option<String> {
    shared_cursor::membership(value)
}

/// Parse Cursor millisecond reset timestamp (`cursor_usage._milliseconds_reset`).
pub fn cursor_milliseconds_reset(value: &Value) -> Option<String> {
    shared_cursor::milliseconds_reset(Some(value))
}

/// Collect Cursor usage (`cursor_usage.collect`).
pub async fn cursor_collect(
    credential: &Credential,
    result: &mut Value,
    client: &dyn PlanTransport,
    now_secs: Option<f64>,
) -> Result<Map<String, Value>, CollectionError> {
    let internal = helpers::to_internal_cred(credential);
    shared_cursor::collect(&internal, result, client, now_secs).await
}

fn parse_desktop_provider(provider: &str) -> Option<ProviderId> {
    match ProviderId::parse(provider)? {
        id @ (ProviderId::Antigravity | ProviderId::Muse | ProviderId::Cursor) => Some(id),
        _ => None,
    }
}

/// Whether `desktop_usage` owns this standalone provider id.
pub fn handles(provider: &str) -> bool {
    parse_desktop_provider(provider).is_some()
}

/// Collect standalone desktop account usage (`desktop_usage.collect`).
pub async fn collect(provider: &str, platform: &str, home: &Path) -> Value {
    collect_with(provider, platform, home, &DesktopEnv::default()).await
}

/// Collect standalone desktop account usage with test overrides (`desktop_usage.collect`).
pub async fn collect_with(
    provider: &str,
    platform: &str,
    home: &Path,
    env: &DesktopEnv<'_>,
) -> Value {
    let Some(provider_id) = parse_desktop_provider(provider) else {
        return json!({
            "status": "error",
            "message": "The saved account usage could not be collected.",
            "windows": []
        });
    };
    let Some(host_label) = desktop::platform_label(platform) else {
        return json!({
            "status": "error",
            "message": "The saved account usage could not be collected.",
            "windows": []
        });
    };
    let mut result = desktop::account(provider_id.as_str(), platform);
    let mut tracker = desktop::ReadTracker::default();
    let credential = match env.credential.clone() {
        Some(c) => Some(c),
        None => match provider_id {
            ProviderId::Muse => muse::muse_credentials(home, env.env_vars, &mut tracker),
            ProviderId::Antigravity => antigravity::antigravity_credentials(home, &mut tracker),
            ProviderId::Cursor => {
                cursor::cursor_credentials(home, platform, env.env_vars, &mut tracker)
            }
            _ => None,
        },
    };
    let Some(cred) = credential else {
        return result;
    };
    result["source"] = json!(format!("{} on {host_label}", cred.source));
    result["email"] = json!(cred.email);
    result["status"] = json!("ok");
    result["message"] = Value::Null;

    let fetch_res: Result<(), UsageError> = match provider_id {
        ProviderId::Antigravity => {
            antigravity::fetch_antigravity(&cred, &mut result, home, env).await
        }
        ProviderId::Muse => {
            muse::fetch_muse(&cred, &mut result, home, env).await;
            Ok(())
        }
        ProviderId::Cursor => {
            cursor::fetch_cursor(&cred, &mut result, env).await;
            Ok(())
        }
        _ => Ok(()),
    };

    if let Err(error) = fetch_res {
        result["status"] = json!(error.status);
        result["message"] = json!(error.message);
        result["windows"] = json!([]);
        return result;
    }

    let fetched_at = desktop::utc_now_iso_secs(env.now_secs);
    result["fetchedAt"] = json!(fetched_at);
    let has_windows = result["windows"].as_array().is_some_and(|w| !w.is_empty());
    if has_windows && result["sampledAt"].is_null() {
        result["sampledAt"] = result["fetchedAt"].clone();
    }
    if !has_windows && result["status"] == "ok" {
        result["status"] = json!("unavailable");
        result["message"] =
            json!("The account is signed in, but the service returned no recognized usage limits.");
    }
    result
}
