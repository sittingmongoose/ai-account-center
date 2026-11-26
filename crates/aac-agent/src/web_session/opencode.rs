//! No-browser plain-HTTPS OpenCode Zen console wallet reader.
//!
//! Calls `https://opencode.ai/console/api/{orgs,billing/status,go/status}` directly over
//! `reqwest` (`rustls-tls`) using allowlisted pasted session cookies (`auth` and/or
//! `__Host-console_session`), preserving exact workspace selection, signed micro-cent USD
//! wallet math, optional Go usage windows, and 429 handling from `opencode_console.py`.

use super::{HttpError, HttpTransport, iso_seconds, parse_iso_millis, parse_retry_after};
use aac_files::web_session::{StoredCookie, valid_workspace_id};
use serde_json::{Value, json};
use std::time::Duration;

const ORIGIN: &str = "https://opencode.ai";
const ORGS_ROUTE: &str = "/console/api/orgs";
const BILLING_ROUTE: &str = "/console/api/billing/status";
const GO_ROUTE: &str = "/console/api/go/status";
const CAP: usize = 1_048_576;

/// Public message shown when wallet balance is current while `/go/status` is rate limited.
pub const GO_RATE_LIMIT_MESSAGE: &str =
    "Wallet balance is current. OpenCode Go usage is rate limited; try again later.";

/// Safe non-secret workspace option returned when the console account belongs to multiple orgs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceChoice {
    /// Verified workspace id (`wrk_*` or `org_*`).
    pub id: String,
    /// Sanitized display label.
    pub name: String,
}

/// Successful OpenCode Zen console wallet reading.
#[derive(Clone, Debug, PartialEq)]
pub struct OpenCodeReading {
    /// Verified workspace id (`wrk_*` or `org_*`).
    pub workspace_id: String,
    /// Deterministic dashboard account id (`plan-opencode-go-console-web-<12 hex>`).
    pub account_id: String,
    /// `"Go"` when console Go windows are present alongside the Zen balance.
    pub plan: Option<String>,
    /// Normalized Go rate-limit windows (when present) plus `zen-balance`.
    pub windows: Vec<Value>,
    /// UTC fetch timestamp.
    pub fetched_at: String,
    /// UTC sample timestamp.
    pub sampled_at: String,
    /// True when `/go/status` returned HTTP 429 while wallet balance succeeded.
    pub go_rate_limited: bool,
    /// Bounded cooldown seconds when `/go/status` returned HTTP 429.
    pub go_retry_after_seconds: Option<f64>,
}

/// Honest error classifications for OpenCode console reads (never contains secrets).
#[derive(Clone, Debug, PartialEq)]
pub enum OpenCodeError {
    /// Session expired, rejected (401/403), or redirected.
    NeedsSignIn,
    /// Console endpoint returned HTTP 429.
    RateLimited {
        /// Optional bounded Retry-After seconds.
        retry_after_seconds: Option<f64>,
    },
    /// Account has multiple workspaces; caller must pick one explicitly.
    ChooseWorkspace(Vec<WorkspaceChoice>),
    /// Requested workspace id is not in the signed-in account's org list.
    WorkspaceMismatch,
    /// Workspace is not a pay-as-you-go prepaid Zen wallet.
    WalletNotPrepaid,
    /// Upstream payload did not match expected schema.
    InvalidResponse,
    /// Upstream HTTP 5xx or unexpected 4xx.
    ProviderError,
    /// Network, TLS, or timeout failure.
    NetworkError,
}

/// Deterministic dashboard account id for a pasted-session OpenCode console wallet.
pub fn wallet_account_id(workspace_id: &str) -> String {
    let suffix = &aac_core::sha256_hex(workspace_id)[..12];
    format!("plan-opencode-go-console-web-{suffix}")
}

fn cookie_header(cookies: &[StoredCookie]) -> Option<String> {
    if cookies.is_empty() || cookies.len() > 2 {
        return None;
    }
    let mut parts = Vec::with_capacity(cookies.len());
    for cookie in cookies {
        if !matches!(cookie.name.as_str(), "auth" | "__Host-console_session") {
            return None;
        }
        parts.push(format!("{}={}", cookie.name, cookie.value));
    }
    Some(parts.join("; "))
}

async fn get_console(
    transport: &dyn HttpTransport,
    cookie_header: &str,
    route: &str,
    workspace: Option<&str>,
    now_ms: i64,
) -> Result<Value, OpenCodeError> {
    if !matches!(route, ORGS_ROUTE | BILLING_ROUTE | GO_ROUTE) {
        return Err(OpenCodeError::InvalidResponse);
    }
    if (route == ORGS_ROUTE) != workspace.is_none() {
        return Err(OpenCodeError::InvalidResponse);
    }
    if let Some(ws) = workspace
        && !valid_workspace_id(ws)
    {
        return Err(OpenCodeError::WorkspaceMismatch);
    }
    let url = format!("{ORIGIN}{route}");
    let mut headers = vec![
        ("Cookie", cookie_header),
        ("Accept", "application/json"),
        ("Origin", ORIGIN),
        ("Referer", "https://opencode.ai/console/"),
    ];
    if let Some(ws) = workspace {
        headers.push(("x-org-id", ws));
    }
    let response = transport
        .send("GET", &url, &headers, None, CAP, Duration::from_secs(8))
        .await
        .map_err(|err| match err {
            HttpError::Redirect => OpenCodeError::NeedsSignIn,
            HttpError::TooLarge | HttpError::Network => OpenCodeError::NetworkError,
        })?;
    if matches!(response.status, 401 | 403) {
        return Err(OpenCodeError::NeedsSignIn);
    }
    if response.status == 429 {
        let retry = response.retry_after.as_deref().map(|raw| {
            parse_retry_after(raw.into(), now_ms as f64 / 1000.0, 0.0, 86_400.0, 60.0).ceil()
        });
        return Err(OpenCodeError::RateLimited {
            retry_after_seconds: retry,
        });
    }
    if response.status != 200 {
        return Err(OpenCodeError::ProviderError);
    }
    serde_json::from_slice(&response.body).map_err(|_| OpenCodeError::InvalidResponse)
}

fn safe_org_name(raw: Option<&str>, id: &str) -> String {
    let Some(text) = raw.map(str::trim) else {
        return id.to_owned();
    };
    if text.is_empty() || text.len() > 120 || text.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return id.to_owned();
    }
    let cleaned = aac_core::scrub(text);
    let trimmed = cleaned.as_str().trim();
    if trimmed.is_empty() {
        id.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Select a workspace from `/console/api/orgs`.
pub fn select_workspace(payload: &Value, requested: Option<&str>) -> Result<String, OpenCodeError> {
    let Some(rows) = payload.as_array() else {
        return Err(OpenCodeError::InvalidResponse);
    };
    if rows.is_empty() || rows.len() > 100 {
        return Err(OpenCodeError::InvalidResponse);
    }
    let mut choices = Vec::with_capacity(rows.len());
    for row in rows {
        let Some(obj) = row.as_object() else {
            return Err(OpenCodeError::InvalidResponse);
        };
        let Some(id) = obj.get("id").and_then(Value::as_str) else {
            return Err(OpenCodeError::InvalidResponse);
        };
        if !valid_workspace_id(id) || choices.iter().any(|c: &WorkspaceChoice| c.id == id) {
            return Err(OpenCodeError::InvalidResponse);
        }
        let name = safe_org_name(
            obj.get("name")
                .or_else(|| obj.get("slug"))
                .and_then(Value::as_str),
            id,
        );
        choices.push(WorkspaceChoice {
            id: id.to_owned(),
            name,
        });
    }
    if let Some(req) = requested {
        if !valid_workspace_id(req) || !choices.iter().any(|c| c.id == req) {
            return Err(OpenCodeError::WorkspaceMismatch);
        }
        return Ok(req.to_owned());
    }
    if choices.len() != 1 {
        return Err(OpenCodeError::ChooseWorkspace(choices));
    }
    Ok(choices[0].id.clone())
}

fn signed_digits(raw: &str) -> Option<i128> {
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    if !(1..=20).contains(&digits.len()) || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<i128>().ok()
}

fn unsigned_digits(raw: &str) -> Option<u128> {
    if !(1..=20).contains(&raw.len()) || !raw.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    raw.parse::<u128>().ok()
}

/// Parse `/console/api/billing/status` into the `zen-balance` USD balance window.
pub fn parse_balance_window(payload: &Value) -> Result<Value, OpenCodeError> {
    let Some(obj) = payload.as_object() else {
        return Err(OpenCodeError::InvalidResponse);
    };
    let mode = obj.get("mode").and_then(Value::as_str);
    let billing = obj.get("billingMode").and_then(Value::as_str);
    if !matches!(mode, Some("pay-as-you-go" | "invoiceable"))
        || !matches!(billing, Some("prepaid" | "legacy" | "seat" | "credit"))
    {
        return Err(OpenCodeError::InvalidResponse);
    }
    if mode != Some("pay-as-you-go") || billing != Some("prepaid") {
        return Err(OpenCodeError::WalletNotPrepaid);
    }
    let raw = obj
        .get("balanceMicroCents")
        .and_then(Value::as_str)
        .and_then(signed_digits)
        .ok_or(OpenCodeError::InvalidResponse)?;
    let balance = raw as f64 / 100_000_000.0;
    if !balance.is_finite() {
        return Err(OpenCodeError::InvalidResponse);
    }
    let expiration = ["expiresAt", "expires_at", "expiration"]
        .iter()
        .find_map(|key| {
            obj.get(*key)
                .and_then(Value::as_str)
                .and_then(parse_iso_millis)
        });
    Ok(json!({
        "key": "zen-balance",
        "label": "Zen balance",
        "kind": "balance",
        "usedPercent": null,
        "remainingPercent": null,
        "used": null,
        "limit": null,
        "remaining": balance,
        "unit": "USD",
        "resetAt": null,
        "expiresAt": expiration,
        "windowMinutes": null
    }))
}

/// Parse `/console/api/go/status` into optional console Go usage windows.
pub fn parse_go_windows(payload: &Value) -> Vec<Value> {
    let Some(access) = payload.get("access").and_then(Value::as_object) else {
        return Vec::new();
    };
    let Some(meters) = access.get("meters").and_then(Value::as_object) else {
        return Vec::new();
    };
    let mut windows = Vec::new();
    for (key, label, minutes) in [
        ("fiveHour", "5 hours", Some(300)),
        ("week", "Weekly", Some(10080)),
        ("month", "Monthly", None),
    ] {
        let Some(meter) = meters.get(key).and_then(Value::as_object) else {
            continue;
        };
        let Some(used) = meter
            .get("usedMicroCents")
            .and_then(Value::as_str)
            .and_then(unsigned_digits)
        else {
            continue;
        };
        let Some(limit) = meter
            .get("limitMicroCents")
            .and_then(Value::as_str)
            .and_then(unsigned_digits)
            .filter(|l| *l > 0)
        else {
            continue;
        };
        let pct = (used as f64) * 100.0 / (limit as f64);
        if !pct.is_finite() || pct < 0.0 {
            continue;
        }
        let mut reset = meter
            .get("resetsAt")
            .and_then(Value::as_str)
            .and_then(parse_iso_millis);
        if key == "month" && reset.is_none() {
            reset = access
                .get("endsAt")
                .and_then(Value::as_str)
                .and_then(parse_iso_millis);
        }
        windows.push(json!({
            "key": format!("console-{key}"),
            "label": label,
            "kind": "rate_limit",
            "usedPercent": pct,
            "remainingPercent": (100.0 - pct).max(0.0),
            "used": null,
            "limit": null,
            "remaining": null,
            "unit": null,
            "resetAt": reset,
            "expiresAt": null,
            "windowMinutes": minutes
        }));
    }
    windows
}

/// Collect OpenCode Zen prepaid wallet balance (and optional console Go meters) over plain HTTPS.
pub async fn collect(
    transport: &dyn HttpTransport,
    cookies: &[StoredCookie],
    requested_workspace: Option<&str>,
    now_ms: i64,
) -> Result<OpenCodeReading, OpenCodeError> {
    let header = cookie_header(cookies).ok_or(OpenCodeError::NeedsSignIn)?;
    let orgs = get_console(transport, &header, ORGS_ROUTE, None, now_ms).await?;
    let workspace = select_workspace(&orgs, requested_workspace)?;
    let billing = get_console(transport, &header, BILLING_ROUTE, Some(&workspace), now_ms).await?;
    let wallet = parse_balance_window(&billing)?;

    let mut go_rate_limited = false;
    let mut go_retry_after_seconds = None;
    let mut windows =
        match get_console(transport, &header, GO_ROUTE, Some(&workspace), now_ms).await {
            Ok(payload) => parse_go_windows(&payload),
            Err(OpenCodeError::RateLimited {
                retry_after_seconds,
            }) => {
                go_rate_limited = true;
                go_retry_after_seconds = retry_after_seconds;
                Vec::new()
            }
            Err(_) => Vec::new(),
        };
    windows.push(wallet);
    let plan = (windows.len() > 1).then(|| "Go".to_owned());
    let checked = iso_seconds(now_ms);
    Ok(OpenCodeReading {
        account_id: wallet_account_id(&workspace),
        workspace_id: workspace,
        plan,
        windows,
        fetched_at: checked.clone(),
        sampled_at: checked,
        go_rate_limited,
        go_retry_after_seconds,
    })
}

/// Convert a successful [`OpenCodeReading`] into a public dashboard account row.
pub fn to_account_row(reading: &OpenCodeReading, platform: &str) -> Value {
    let message = if reading.go_rate_limited {
        GO_RATE_LIMIT_MESSAGE
    } else {
        "Console workspace; API-key account identity has not been linked."
    };
    json!({
        "id": reading.account_id,
        "provider": "opencode-go",
        "providerLabel": "OpenCode Go",
        "label": "OpenCode console wallet",
        "email": null,
        "plan": reading.plan,
        "platform": platform,
        "source": "Pasted OpenCode console session",
        "status": "ok",
        "message": message,
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

/// Project an [`OpenCodeError`] to a safe JSON status object.
pub fn error_json(err: &OpenCodeError) -> Value {
    match err {
        OpenCodeError::NeedsSignIn => json!({
            "status": "needs_sign_in",
            "code": "needs_sign_in",
            "message": "The pasted OpenCode console session expired. Paste a fresh session in Accounts."
        }),
        OpenCodeError::RateLimited {
            retry_after_seconds,
        } => json!({
            "status": "error",
            "code": "rate_limited",
            "retryAfterSeconds": retry_after_seconds,
            "message": "OpenCode rate limited the wallet check. Try again later."
        }),
        OpenCodeError::ChooseWorkspace(choices) => json!({
            "status": "unavailable",
            "code": "choose_workspace",
            "workspaces": choices.iter().map(|c| json!({"id": c.id, "name": c.name})).collect::<Vec<_>>()
        }),
        OpenCodeError::WorkspaceMismatch => json!({
            "status": "unavailable",
            "code": "workspace_mismatch",
            "message": "The selected OpenCode workspace is not available to this session."
        }),
        OpenCodeError::WalletNotPrepaid => json!({
            "status": "unavailable",
            "code": "wallet_not_prepaid",
            "message": "This OpenCode workspace does not have a prepaid Zen wallet."
        }),
        OpenCodeError::InvalidResponse => json!({
            "status": "error",
            "code": "invalid_response",
            "message": "OpenCode returned an unexpected wallet response."
        }),
        OpenCodeError::ProviderError | OpenCodeError::NetworkError => json!({
            "status": "error",
            "code": "provider_unreachable",
            "message": "OpenCode console could not be reached right now."
        }),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::web_session::HttpResponse;

    struct FakeOpenCode {
        orgs_status: u16,
        orgs: Value,
        billing: Value,
        go_status: u16,
        go: Value,
    }

    impl HttpTransport for FakeOpenCode {
        fn send<'a>(
            &'a self,
            _method: &'a str,
            url: &'a str,
            headers: &'a [(&'a str, &'a str)],
            _body: Option<&'a [u8]>,
            _cap: usize,
            _timeout: Duration,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>,
        > {
            assert!(headers.iter().any(|(k, v)| *k == "Origin" && *v == ORIGIN));
            let resp = if url.ends_with(ORGS_ROUTE) {
                HttpResponse {
                    status: self.orgs_status,
                    retry_after: None,
                    body: if self.orgs_status < 400 {
                        self.orgs.to_string().into_bytes()
                    } else {
                        Vec::new()
                    },
                }
            } else if url.ends_with(BILLING_ROUTE) {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: self.billing.to_string().into_bytes(),
                }
            } else {
                HttpResponse {
                    status: self.go_status,
                    retry_after: (self.go_status == 429).then(|| "90".to_owned()),
                    body: if self.go_status < 400 {
                        self.go.to_string().into_bytes()
                    } else {
                        Vec::new()
                    },
                }
            };
            Box::pin(async move { Ok(resp) })
        }
    }

    fn sample_cookies() -> Vec<StoredCookie> {
        vec![StoredCookie {
            name: "__Host-console_session".into(),
            value: "sess-123".into(),
        }]
    }

    #[tokio::test]
    async fn collects_prepaid_balance_and_go_meters_and_handles_go_429() {
        let fake = FakeOpenCode {
            orgs_status: 200,
            orgs: json!([{"id": "wrk_01ABC", "name": "Personal"}]),
            billing: json!({
                "mode": "pay-as-you-go",
                "billingMode": "prepaid",
                "balanceMicroCents": "4250000000",
                "expiresAt": "2027-01-01T00:00:00Z"
            }),
            go_status: 200,
            go: json!({
                "access": {
                    "endsAt": "2026-12-01T00:00:00Z",
                    "meters": {
                        "fiveHour": { "usedMicroCents": "2500", "limitMicroCents": "10000", "resetsAt": "2026-11-26T22:00:00Z" },
                        "month": { "usedMicroCents": "1000", "limitMicroCents": "5000" }
                    }
                }
            }),
        };
        let reading = collect(&fake, &sample_cookies(), None, 1_795_720_000_000)
            .await
            .unwrap();
        assert_eq!(reading.workspace_id, "wrk_01ABC");
        assert!(
            reading
                .account_id
                .starts_with("plan-opencode-go-console-web-")
        );
        assert_eq!(reading.plan.as_deref(), Some("Go"));
        assert_eq!(reading.windows.len(), 3);
        let wallet = reading
            .windows
            .iter()
            .find(|w| w["key"] == "zen-balance")
            .unwrap();
        assert_eq!(wallet["remaining"], json!(42.5));
        assert_eq!(wallet["unit"], json!("USD"));
        let month = reading
            .windows
            .iter()
            .find(|w| w["key"] == "console-month")
            .unwrap();
        assert_eq!(month["usedPercent"], json!(20.0));
        assert_eq!(month["resetAt"], json!("2026-12-01T00:00:00.000Z"));

        // Go 429 retains wallet balance cleanly with rate-limit flag.
        let limited = FakeOpenCode {
            go_status: 429,
            ..fake
        };
        let reading = collect(&limited, &sample_cookies(), None, 1_795_720_000_000)
            .await
            .unwrap();
        assert!(reading.go_rate_limited);
        assert_eq!(reading.go_retry_after_seconds, Some(90.0));
        assert_eq!(reading.windows.len(), 1);
        assert_eq!(reading.windows[0]["key"], "zen-balance");
        assert_eq!(
            to_account_row(&reading, "ubuntu")["message"],
            GO_RATE_LIMIT_MESSAGE
        );
    }

    #[tokio::test]
    async fn handles_multi_org_choose_workspace_and_401_needs_sign_in() {
        let multi = FakeOpenCode {
            orgs_status: 200,
            orgs: json!([
                {"id": "wrk_1", "name": "Alpha"},
                {"id": "wrk_2", "name": "Beta"}
            ]),
            billing: json!({"mode": "pay-as-you-go", "billingMode": "prepaid", "balanceMicroCents": "0"}),
            go_status: 200,
            go: json!({}),
        };
        let err = collect(&multi, &sample_cookies(), None, 1_795_720_000_000)
            .await
            .unwrap_err();
        assert!(matches!(err, OpenCodeError::ChooseWorkspace(ref list) if list.len() == 2));

        let chosen = collect(&multi, &sample_cookies(), Some("wrk_2"), 1_795_720_000_000)
            .await
            .unwrap();
        assert_eq!(chosen.workspace_id, "wrk_2");

        let expired = FakeOpenCode {
            orgs_status: 401,
            ..multi
        };
        assert_eq!(
            collect(&expired, &sample_cookies(), None, 1_795_720_000_000)
                .await
                .unwrap_err(),
            OpenCodeError::NeedsSignIn
        );
    }
}
