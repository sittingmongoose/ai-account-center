//! No-browser plain-HTTPS Muse Code portal quota reader.
//!
//! Calls `https://dev.meta.ai/api/{auth/me,portal/teams,portal/teams/<id>/subscription-quota}`
//! using allowlisted pasted session cookies (`llama_dev_sess` and/or `llm_sess`), preserving
//! exact email binding, `userId` SHA-256 pinning, team/tier verification, `as_of` bounds,
//! weighted-token normalization, and 429 cooldown rules from `muse_console.py` / `desktop_usage.py`.

use super::{HttpError, HttpTransport, iso_seconds, parse_iso_millis, parse_retry_after};
use aac_files::{
    read::{Ancestors, Read, ReadPolicy},
    web_session::{StoredCookie, valid_email, valid_plan, valid_sha256_hex, valid_team_id},
};
use serde_json::{Value, json};
use std::{path::Path, time::Duration};

const ORIGIN: &str = "https://dev.meta.ai";
const CAP: usize = 65_536;

/// Minimum seconds between live Muse portal quota refreshes.
pub const REFRESH_SECONDS: f64 = 300.0;
/// Default cooldown after HTTP 429 from `dev.meta.ai`.
pub const RATE_LIMIT_SECONDS: f64 = 600.0;
/// Maximum honored cooldown after HTTP 429.
pub const MAX_RETRY_SECONDS: f64 = 86_400.0;

const MIN_PROVIDER_AS_OF: f64 = 946_684_800.0;
const MAX_PROVIDER_AS_OF: f64 = 4_102_444_800.0;
const AS_OF_FUTURE_SKEW_SECONDS: f64 = 300.0;

/// Safe non-secret team option returned when the Muse portal account belongs to multiple teams.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TeamChoice {
    /// Numeric portal team id.
    pub id: String,
    /// Sanitized display name.
    pub name: String,
}

/// Verification and identity binding inputs for a Muse portal read.
#[derive(Clone, Debug, Default)]
pub struct MuseBinding<'a> {
    /// Expected CLI / pinned email (case-insensitive match when set).
    pub expected_email: Option<&'a str>,
    /// Expected CLI subscription tier (exact match when set).
    pub expected_plan: Option<&'a str>,
    /// Explicitly selected numeric team id.
    pub requested_team: Option<&'a str>,
    /// Previously email-verified team id for email-less fallback with `bound_user_hash`.
    pub bound_team: Option<&'a str>,
    /// Pinned SHA-256 hash of `muse-user:<userId>`.
    pub bound_user_hash: Option<&'a str>,
}

/// Successful Muse Code portal quota reading.
#[derive(Clone, Debug, PartialEq)]
pub struct MuseReading {
    /// Verified account email.
    pub email: String,
    /// Verified portal subscription tier.
    pub plan: String,
    /// Verified numeric portal team id.
    pub team_id: String,
    /// SHA-256 pin of `muse-user:<userId>` (when returned by `/api/auth/me`).
    pub user_hash: Option<String>,
    /// Binding mode used (`"email"` or `"user"`).
    pub binding_mode: &'static str,
    /// Normalized rolling and weekly weighted-token windows.
    pub windows: Vec<Value>,
    /// UTC fetch timestamp.
    pub fetched_at: String,
    /// Provider `as_of` timestamp (when valid) or UTC fetch timestamp.
    pub sampled_at: String,
}

/// Honest error classifications for Muse portal reads (never contains secrets).
#[derive(Clone, Debug, PartialEq)]
pub enum MuseError {
    /// Session expired, rejected (401/403), or redirected.
    NeedsSignIn,
    /// Web account email or pinned `userId` differs from the expected Muse account.
    AccountMismatch,
    /// `/api/auth/me` returned no usable email and no verified user pin matched.
    IdentityUnavailable,
    /// Portal team subscription tier differs from the expected CLI plan.
    PlanMismatch,
    /// Account belongs to multiple teams; caller must pick one explicitly.
    ChooseTeam(Vec<TeamChoice>),
    /// Requested or bound team id is not listed for this web account.
    TeamMismatch,
    /// Portal rate limited the read.
    RateLimited {
        /// Bounded cooldown seconds (`300..=86400`, default `600`).
        retry_after_seconds: f64,
    },
    /// Upstream payload lacked valid rolling or weekly quota.
    InvalidResponse,
    /// Upstream HTTP 5xx or unexpected status.
    ProviderError,
    /// Network, TLS, or timeout failure.
    NetworkError,
}

/// Compute the SHA-256 pin for `/api/auth/me`'s `userId` (`muse-user:<id>`).
pub fn user_hash(me: &Value) -> Option<String> {
    let raw = match me.get("userId")? {
        Value::String(s) => s.clone(),
        Value::Number(n) if n.is_i64() || n.is_u64() => n.to_string(),
        _ => return None,
    };
    if raw.is_empty() || raw.len() > 128 || raw.bytes().any(|b| b <= 0x20 || b == 0x7f) {
        return None;
    }
    Some(aac_core::sha256_hex(format!("muse-user:{raw}")))
}

/// Read the local Muse CLI account email (read-only, never exposes or sends the `dca:` token).
pub fn read_local_cli_email(home: &Path) -> Option<String> {
    let policy = ReadPolicy::capped(65_536).ancestors(Ancestors::NoLinkBelow(home));
    for rel in [".local/share/muse/auth.json", ".config/muse/auth.json"] {
        let path = home.join(rel);
        if let Read::Bytes(bytes, _) = aac_files::read::read(&path, &policy)
            && let Ok(doc) = serde_json::from_slice::<Value>(&bytes)
            && let Some(email) = doc
                .get("email")
                .or_else(|| doc.get("user_email"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|e| valid_email(e))
        {
            return Some(email.to_owned());
        }
    }
    None
}

fn cookie_header(cookies: &[StoredCookie]) -> Option<String> {
    if cookies.is_empty() || cookies.len() > 2 {
        return None;
    }
    let mut parts = Vec::with_capacity(cookies.len());
    for cookie in cookies {
        if !matches!(cookie.name.as_str(), "llama_dev_sess" | "llm_sess")
            || cookie.value.starts_with("dca:")
            || cookie.value.starts_with("sk-")
        {
            return None;
        }
        parts.push(format!("{}={}", cookie.name, cookie.value));
    }
    Some(parts.join("; "))
}

fn valid_portal_route(route: &str) -> bool {
    if matches!(route, "/api/auth/me" | "/api/portal/teams") {
        return true;
    }
    route
        .strip_prefix("/api/portal/teams/")
        .and_then(|rest| rest.strip_suffix("/subscription-quota"))
        .is_some_and(valid_team_id)
}

async fn get_portal(
    transport: &dyn HttpTransport,
    cookie_header: &str,
    route: &str,
    now_ms: i64,
) -> Result<Value, MuseError> {
    if !valid_portal_route(route) {
        return Err(MuseError::InvalidResponse);
    }
    let url = format!("{ORIGIN}{route}");
    let headers = [
        ("Cookie", cookie_header),
        ("Accept", "application/json"),
        ("User-Agent", "CCS-Muse-Usage"),
        ("Referer", "https://dev.meta.ai/"),
    ];
    let response = transport
        .send("GET", &url, &headers, None, CAP, Duration::from_secs(8))
        .await
        .map_err(|err| match err {
            HttpError::Redirect => MuseError::NeedsSignIn,
            HttpError::TooLarge => MuseError::InvalidResponse,
            HttpError::Network => MuseError::NetworkError,
        })?;
    if matches!(response.status, 401 | 403) {
        return Err(MuseError::NeedsSignIn);
    }
    if response.status == 429 {
        let retry = parse_retry_after(
            response.retry_after.as_deref(),
            now_ms as f64 / 1000.0,
            REFRESH_SECONDS,
            MAX_RETRY_SECONDS,
            RATE_LIMIT_SECONDS,
        );
        return Err(MuseError::RateLimited {
            retry_after_seconds: retry,
        });
    }
    if response.status != 200 {
        return Err(MuseError::ProviderError);
    }
    let value: Value =
        serde_json::from_slice(&response.body).map_err(|_| MuseError::InvalidResponse)?;
    if !value.is_object() {
        return Err(MuseError::InvalidResponse);
    }
    Ok(value)
}

fn parse_teams(payload: &Value) -> Result<Vec<TeamChoice>, MuseError> {
    let Some(rows) = payload.get("teams").and_then(Value::as_array) else {
        return Err(MuseError::InvalidResponse);
    };
    if rows.is_empty() || rows.len() > 100 {
        return Err(MuseError::InvalidResponse);
    }
    let mut teams = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(obj) = row.as_object() else {
            return Err(MuseError::InvalidResponse);
        };
        let id = match obj.get("team_id") {
            Some(Value::String(s)) => s.clone(),
            Some(Value::Number(n)) if n.is_i64() || n.is_u64() => n.to_string(),
            _ => return Err(MuseError::InvalidResponse),
        };
        if !valid_team_id(&id) || teams.iter().any(|t: &TeamChoice| t.id == id) {
            return Err(MuseError::InvalidResponse);
        }
        let name = obj
            .get("team_name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|n| valid_plan(n))
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Team {id}"));
        teams.push(TeamChoice { id, name });
    }
    Ok(teams)
}

fn counter(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite() && *f >= 0.0),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && *f >= 0.0),
        _ => None,
    }
}

fn reset_value(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::Null | Value::Bool(_) => None,
        Value::Number(n) => {
            let secs = n.as_f64()?;
            if !secs.is_finite() || secs <= 0.0 {
                return None;
            }
            let ms = if secs > 1e11 {
                secs as i64
            } else {
                (secs * 1000.0) as i64
            };
            let utc = chrono::DateTime::from_timestamp_millis(ms)?;
            let year = chrono::Datelike::year(&utc);
            (2000..=2200)
                .contains(&year)
                .then(|| utc.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        }
        Value::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                return None;
            }
            if let Ok(num) = trimmed.parse::<f64>()
                && num.is_finite()
                && num > 0.0
            {
                return reset_value(Some(&json!(num)));
            }
            parse_iso_millis(trimmed).map(|iso| iso.replace(".000Z", "Z"))
        }
        _ => None,
    }
}

fn provider_as_of_iso(value: Option<&Value>, now_ms: i64) -> Option<String> {
    let raw = match value? {
        Value::Number(n) => n.as_f64()?,
        Value::String(s) if s.len() <= 64 => s.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    let now_secs = now_ms as f64 / 1000.0;
    let upper = MAX_PROVIDER_AS_OF.min(now_secs + AS_OF_FUTURE_SKEW_SECONDS);
    if !raw.is_finite() || raw < MIN_PROVIDER_AS_OF || raw > upper {
        return None;
    }
    let ms = (raw * 1000.0).round() as i64;
    let utc = chrono::DateTime::from_timestamp_millis(ms)?;
    Some(utc.format("%Y-%m-%dT%H:%M:%SZ").to_string())
}

fn window_label(key: &str, minutes: Option<f64>) -> String {
    if key == "weekly" {
        return "Weekly limit".to_owned();
    }
    let Some(mins) = minutes.filter(|m| *m > 0.0 && *m <= 525_600.0) else {
        return "Current usage".to_owned();
    };
    if (mins % 60.0).abs() < f64::EPSILON {
        let hours = mins / 60.0;
        format!("Current usage ({hours}-hour)")
    } else {
        format!("Current usage ({mins}-minute)")
    }
}

/// Normalize `subscription_quota` into dashboard windows (matches `desktop_usage.py::normalize_muse`).
pub fn normalize_portal_quota(quota: &Value) -> Vec<Value> {
    let Some(obj) = quota.as_object() else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    for key in ["window", "weekly"] {
        let used = counter(obj.get(&format!("{key}_weighted_used")));
        let limit = counter(obj.get(&format!("{key}_weighted_limit")));
        let duration = if key == "window" {
            counter(obj.get("window_duration_secs"))
        } else {
            None
        };
        let minutes = match (key, duration) {
            ("window", Some(secs)) => Some(secs / 60.0),
            ("weekly", _) => Some(10080.0),
            _ => None,
        };
        let reset = reset_value(obj.get(&format!("{key}_resets_at")));
        if used.is_none() && reset.is_none() {
            continue;
        }
        let used_pct = match (used, limit) {
            (Some(u), Some(l)) if l > 0.0 => Some(u / l * 100.0),
            _ => None,
        };
        let valid_pct = used_pct.filter(|p| p.is_finite() && (0.0..=100.0).contains(p));
        let remaining_pct = valid_pct.map(|p| (100.0 - p).max(0.0));
        windows.push(json!({
            "key": key,
            "label": window_label(key, minutes),
            "usedPercent": valid_pct,
            "remainingPercent": remaining_pct,
            "resetAt": reset,
            "windowMinutes": minutes,
            "used": used,
            "limit": limit,
            "unit": "weighted tokens"
        }));
    }
    windows
}

/// Collect Muse Code portal quota over plain HTTPS, enforcing email, `userId` SHA-256 pin,
/// team membership, and subscription tier invariants.
pub async fn collect(
    transport: &dyn HttpTransport,
    cookies: &[StoredCookie],
    binding: &MuseBinding<'_>,
    now_ms: i64,
) -> Result<MuseReading, MuseError> {
    if let Some(expected) = binding.expected_email
        && !valid_email(expected)
    {
        return Err(MuseError::AccountMismatch);
    }
    if let Some(plan) = binding.expected_plan
        && !valid_plan(plan)
    {
        return Err(MuseError::AccountMismatch);
    }
    if let Some(bound_team) = binding.bound_team
        && let Some(req) = binding.requested_team
        && req != bound_team
    {
        return Err(MuseError::IdentityUnavailable);
    }
    let header = cookie_header(cookies).ok_or(MuseError::NeedsSignIn)?;
    let me = get_portal(transport, &header, "/api/auth/me", now_ms).await?;
    let actual_email = me
        .get("email")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|e| valid_email(e));
    let actual_user_hash = user_hash(&me);

    let (resolved_email, binding_mode, effective_requested_team) =
        if let Some(actual) = actual_email {
            if let Some(expected) = binding.expected_email
                && !actual.eq_ignore_ascii_case(expected.trim())
            {
                return Err(MuseError::AccountMismatch);
            }
            (actual.to_owned(), "email", binding.requested_team)
        } else {
            let Some(bound_team) = binding.bound_team.filter(|t| valid_team_id(t)) else {
                return Err(MuseError::IdentityUnavailable);
            };
            let Some(actual_hash) = actual_user_hash.as_deref() else {
                return Err(MuseError::IdentityUnavailable);
            };
            let Some(pinned_hash) = binding.bound_user_hash.filter(|h| valid_sha256_hex(h)) else {
                return Err(MuseError::IdentityUnavailable);
            };
            if actual_hash != pinned_hash {
                return Err(MuseError::AccountMismatch);
            }
            let Some(expected) = binding.expected_email else {
                return Err(MuseError::IdentityUnavailable);
            };
            (expected.trim().to_owned(), "user", Some(bound_team))
        };

    let listed = get_portal(transport, &header, "/api/portal/teams", now_ms).await?;
    if effective_requested_team.is_some()
        && listed
            .get("teams")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
    {
        return Err(MuseError::TeamMismatch);
    }
    let teams = parse_teams(&listed)?;
    let selected_team = match effective_requested_team {
        Some(req) => {
            if !teams.iter().any(|t| t.id == req) {
                return Err(MuseError::TeamMismatch);
            }
            req.to_owned()
        }
        None => {
            if teams.len() != 1 {
                return Err(MuseError::ChooseTeam(teams));
            }
            teams[0].id.clone()
        }
    };

    let quota_route = format!("/api/portal/teams/{selected_team}/subscription-quota");
    let response = get_portal(transport, &header, &quota_route, now_ms).await?;
    let Some(quota) = response.get("subscription_quota").filter(|v| v.is_object()) else {
        return Err(MuseError::InvalidResponse);
    };
    let Some(tier) = quota
        .get("tier")
        .and_then(Value::as_str)
        .filter(|t| valid_plan(t))
    else {
        return Err(MuseError::InvalidResponse);
    };
    if let Some(expected_plan) = binding.expected_plan
        && tier != expected_plan
    {
        return Err(MuseError::PlanMismatch);
    }
    let allowed_fields = [
        "tier",
        "window_weighted_used",
        "window_weighted_limit",
        "window_duration_secs",
        "window_resets_at",
        "weekly_weighted_used",
        "weekly_weighted_limit",
        "weekly_resets_at",
        "as_of",
    ];
    if let Some(obj) = quota.as_object() {
        for key in allowed_fields {
            if let Some(val) = obj.get(key)
                && (val.is_object()
                    || val.is_array()
                    || val.as_str().is_some_and(|s| s.len() > 160))
            {
                return Err(MuseError::InvalidResponse);
            }
        }
    }
    let windows = normalize_portal_quota(quota);
    if windows.is_empty() {
        return Err(MuseError::InvalidResponse);
    }
    let fetched_at = iso_seconds(now_ms);
    let sampled_at =
        provider_as_of_iso(quota.get("as_of"), now_ms).unwrap_or_else(|| fetched_at.clone());
    Ok(MuseReading {
        email: resolved_email,
        plan: tier.to_owned(),
        team_id: selected_team,
        user_hash: actual_user_hash,
        binding_mode,
        windows,
        fetched_at,
        sampled_at,
    })
}

/// Convert a successful [`MuseReading`] into a public dashboard account row.
pub fn to_account_row(reading: &MuseReading, platform: &str) -> Value {
    json!({
        "id": "muse:usage",
        "provider": "muse",
        "providerLabel": "Muse Code",
        "label": reading.email,
        "email": reading.email,
        "plan": reading.plan,
        "platform": platform,
        "source": "Pasted Meta web session",
        "status": "ok",
        "message": null,
        "fetchedAt": reading.fetched_at,
        "sampledAt": reading.sampled_at,
        "isActive": false,
        "windows": reading.windows,
        "capabilities": {
            "codexProfile": null,
            "claudeProfileId": null,
            "claudePlatforms": []
        }
    })
}

/// Project a [`MuseError`] to a safe JSON status object.
pub fn error_json(err: &MuseError) -> Value {
    match err {
        MuseError::NeedsSignIn => json!({
            "status": "needs_sign_in",
            "code": "needs_sign_in",
            "message": "The pasted Muse web session expired. Paste a fresh session in Accounts."
        }),
        MuseError::AccountMismatch => json!({
            "status": "unavailable",
            "code": "account_mismatch",
            "message": "The Muse web account differs from the signed-in Muse CLI account."
        }),
        MuseError::IdentityUnavailable => json!({
            "status": "unavailable",
            "code": "identity_unavailable",
            "message": "Muse is not returning the signed-in web account email right now."
        }),
        MuseError::PlanMismatch => json!({
            "status": "unavailable",
            "code": "plan_mismatch",
            "message": "The selected web team's subscription differs from the Muse CLI subscription."
        }),
        MuseError::ChooseTeam(teams) => json!({
            "status": "unavailable",
            "code": "choose_team",
            "teams": teams.iter().map(|t| json!({"id": t.id, "name": t.name})).collect::<Vec<_>>()
        }),
        MuseError::TeamMismatch => json!({
            "status": "unavailable",
            "code": "team_mismatch",
            "message": "The selected Muse team is not available to this signed-in account."
        }),
        MuseError::RateLimited {
            retry_after_seconds,
        } => json!({
            "status": "error",
            "code": "rate_limited",
            "retryAfterSeconds": retry_after_seconds,
            "message": "Muse is limiting usage requests. Refresh resumes automatically after a cooldown."
        }),
        MuseError::InvalidResponse => json!({
            "status": "error",
            "code": "invalid_response",
            "message": "Muse returned no usable rolling or weekly quota."
        }),
        MuseError::ProviderError | MuseError::NetworkError => json!({
            "status": "error",
            "code": "provider_unreachable",
            "message": "Muse portal could not be reached right now."
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::web_session::HttpResponse;

    struct FakeMuse {
        me_status: u16,
        me: Value,
        teams: Value,
        quota_status: u16,
        quota: Value,
    }

    impl HttpTransport for FakeMuse {
        fn send<'a>(
            &'a self,
            _method: &'a str,
            url: &'a str,
            _headers: &'a [(&'a str, &'a str)],
            _body: Option<&'a [u8]>,
            _cap: usize,
            _timeout: Duration,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>,
        > {
            let resp = if url.ends_with("/api/auth/me") {
                HttpResponse {
                    status: self.me_status,
                    retry_after: None,
                    body: if self.me_status < 400 {
                        self.me.to_string().into_bytes()
                    } else {
                        Vec::new()
                    },
                }
            } else if url.ends_with("/api/portal/teams") {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: self.teams.to_string().into_bytes(),
                }
            } else {
                HttpResponse {
                    status: self.quota_status,
                    retry_after: (self.quota_status == 429).then(|| "900".to_owned()),
                    body: if self.quota_status < 400 {
                        self.quota.to_string().into_bytes()
                    } else {
                        Vec::new()
                    },
                }
            };
            Box::pin(async move { Ok(resp) })
        }
    }

    fn muse_cookies() -> Vec<StoredCookie> {
        vec![StoredCookie {
            name: "llama_dev_sess".into(),
            value: "web-sess-value".into(),
        }]
    }

    #[tokio::test]
    async fn enforces_email_userid_pin_tier_team_and_429_cooldown() {
        let now_ms = 1_795_720_000_000i64;
        let fake = FakeMuse {
            me_status: 200,
            me: json!({ "email": "owner@example.test", "userId": "u-99" }),
            teams: json!({ "teams": [{ "team_id": "42", "team_name": "Core Team" }] }),
            quota_status: 200,
            quota: json!({
                "subscription_quota": {
                    "tier": "Pro",
                    "window_weighted_used": 250,
                    "window_weighted_limit": 1000,
                    "window_duration_secs": 18000,
                    "window_resets_at": "2026-11-26T22:00:00Z",
                    "weekly_weighted_used": 1200,
                    "weekly_weighted_limit": 4000,
                    "weekly_resets_at": "2026-12-01T00:00:00Z",
                    "as_of": 1795719900
                }
            }),
        };

        // Email-verified happy path + userId SHA-256 pin.
        let reading = collect(
            &fake,
            &muse_cookies(),
            &MuseBinding {
                expected_email: Some("Owner@Example.Test"),
                expected_plan: Some("Pro"),
                ..Default::default()
            },
            now_ms,
        )
        .await
        .unwrap();
        assert_eq!(reading.email, "owner@example.test");
        assert_eq!(reading.team_id, "42");
        assert_eq!(reading.binding_mode, "email");
        assert_eq!(
            reading.user_hash.as_deref(),
            Some(aac_core::sha256_hex("muse-user:u-99").as_str())
        );
        assert_eq!(reading.windows.len(), 2);
        assert_eq!(reading.windows[0]["label"], "Current usage (5-hour)");
        assert_eq!(reading.windows[0]["usedPercent"], json!(25.0));
        assert_eq!(reading.windows[1]["label"], "Weekly limit");
        assert_eq!(reading.windows[1]["usedPercent"], json!(30.0));

        // Email mismatch fails closed.
        assert_eq!(
            collect(
                &fake,
                &muse_cookies(),
                &MuseBinding {
                    expected_email: Some("other@example.test"),
                    expected_plan: Some("Pro"),
                    ..Default::default()
                },
                now_ms,
            )
            .await
            .unwrap_err(),
            MuseError::AccountMismatch
        );

        // Plan mismatch fails closed.
        assert_eq!(
            collect(
                &fake,
                &muse_cookies(),
                &MuseBinding {
                    expected_email: Some("owner@example.test"),
                    expected_plan: Some("Max"),
                    ..Default::default()
                },
                now_ms,
            )
            .await
            .unwrap_err(),
            MuseError::PlanMismatch
        );

        // Email-less /api/auth/me succeeds ONLY when bound_user_hash + bound_team match.
        let emailless = FakeMuse {
            me: json!({ "userId": "u-99" }),
            ..fake
        };
        let pinned = collect(
            &emailless,
            &muse_cookies(),
            &MuseBinding {
                expected_email: Some("owner@example.test"),
                expected_plan: Some("Pro"),
                bound_team: Some("42"),
                bound_user_hash: reading.user_hash.as_deref(),
                ..Default::default()
            },
            now_ms,
        )
        .await
        .unwrap();
        assert_eq!(pinned.binding_mode, "user");

        // Wrong userId pin fails closed with AccountMismatch.
        let wrong_user = FakeMuse {
            me: json!({ "userId": "u-different" }),
            ..emailless
        };
        assert_eq!(
            collect(
                &wrong_user,
                &muse_cookies(),
                &MuseBinding {
                    expected_email: Some("owner@example.test"),
                    expected_plan: Some("Pro"),
                    bound_team: Some("42"),
                    bound_user_hash: reading.user_hash.as_deref(),
                    ..Default::default()
                },
                now_ms,
            )
            .await
            .unwrap_err(),
            MuseError::AccountMismatch
        );

        // 429 honors Retry-After cleanly.
        let limited = FakeMuse {
            me: json!({ "email": "owner@example.test", "userId": "u-99" }),
            quota_status: 429,
            ..wrong_user
        };
        assert_eq!(
            collect(
                &limited,
                &muse_cookies(),
                &MuseBinding {
                    expected_email: Some("owner@example.test"),
                    expected_plan: Some("Pro"),
                    ..Default::default()
                },
                now_ms,
            )
            .await
            .unwrap_err(),
            MuseError::RateLimited {
                retry_after_seconds: 900.0
            }
        );
    }
}
