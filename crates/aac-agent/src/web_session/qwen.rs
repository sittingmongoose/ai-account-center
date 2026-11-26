//! No-browser plain-HTTPS Qwen Token Plan reader (Option B).
//!
//! Supports both the international console (`home.qwencloud.com` / `cs-data.qwencloud.com`) and
//! the China console (`bailian.console.aliyun.com` / `bailian-cs.console.aliyun.com`), percent-
//! decoded CSRF cookie headers (`login_aliyunid_csrf` / `csrf`), and complete parsing of
//! `usage`, `subscription`, `quota-config`, and `addon/list` matching `plan_usage.py`.

use super::{
    HttpError, HttpTransport, iso_seconds, parse_iso_millis, parse_retry_after,
    percent_decode_strict,
};
use aac_files::web_session::{MAX_COOKIE_HEADER_BYTES, StoredSession};
use aac_providers::PLAN_RETRY_AFTER;
use serde_json::{Value, json};
use std::time::Duration;

const CAP: usize = 1_048_576;
const INTL_ORIGIN: &str = "https://home.qwencloud.com";
const INTL_REFERER: &str = "https://home.qwencloud.com/analytics/token-plan/individual";
const INTL_INFO_URL: &str = "https://home.qwencloud.com/tool/user/info.json";
const INTL_GATEWAY: &str = "https://cs-data.qwencloud.com/data/api.json";

const CN_ORIGIN: &str = "https://bailian.console.aliyun.com";
const CN_REFERER: &str = "https://bailian.console.aliyun.com/cn-beijing?tab=plan";
const CN_GATEWAY: &str = "https://bailian-cs.console.aliyun.com/data/api.json";

const QWEN_INTL_BASE: &str =
    "https://token-plan.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1";
const QWEN_CN_BASE: &str = "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1";

/// Successful Qwen Token Plan reading.
#[derive(Clone, Debug, PartialEq)]
pub struct QwenReading {
    /// Plan tier label (from `subscription.planName` or `specCode`), when present.
    pub plan: Option<String>,
    /// Normalized usage, subscription expiry, and addon credit-pack windows.
    pub windows: Vec<Value>,
    /// Site region (`"intl"` or `"cn"`).
    pub region: String,
    /// UTC fetch timestamp.
    pub fetched_at: String,
    /// UTC sample timestamp.
    pub sampled_at: String,
}

/// Honest error classifications for Qwen console reads (never contains secrets).
#[derive(Clone, Debug, PartialEq)]
pub enum QwenError {
    /// Console session expired, rejected, redirected, or incomplete.
    NeedsSignIn(String),
    /// Upstream rate limited the read.
    RateLimited {
        /// Bounded cooldown in seconds.
        retry_after_seconds: f64,
        /// Safe public message.
        message: String,
    },
    /// Invalid stored cookie, unsupported region, or malformed local credential.
    Unavailable(String),
    /// Transient network or upstream payload error.
    ProviderError(String),
}

fn form_encode(pairs: &[(&str, String)]) -> String {
    let mut ser = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in pairs {
        ser.append_pair(k, v);
    }
    ser.finish()
}

fn extract_csrf_token(gateway_cookie: &str) -> Result<Option<String>, QwenError> {
    for part in gateway_cookie.split(';') {
        let trimmed = part.trim();
        let Some((name, value)) = trimmed.split_once('=') else {
            continue;
        };
        if matches!(name, "login_aliyunid_csrf" | "csrf") {
            if value.bytes().any(|b| matches!(b, 0 | 10 | 13)) {
                return Err(QwenError::Unavailable(
                    "The saved Qwen console session is invalid.".into(),
                ));
            }
            let decoded = percent_decode_strict(value).ok_or_else(|| {
                QwenError::Unavailable("The saved Qwen console session is invalid.".into())
            })?;
            return Ok(Some(decoded));
        }
    }
    Ok(None)
}

fn extract_cn_sec_token(html: &str) -> Option<String> {
    let (_, after) = html.split_once("SEC_TOKEN:")?;
    let trimmed = after.trim_start_matches([' ', '\t', '\r', '\n']);
    let rest = trimmed.strip_prefix('"')?;
    let end = rest.find('"')?;
    let token = &rest[..end];
    if token.is_empty() || token.len() > 16_384 || token.bytes().any(|b| matches!(b, 0 | 10 | 13)) {
        return None;
    }
    Some(token.to_owned())
}

async fn send_json(
    transport: &dyn HttpTransport,
    method: &str,
    url: &str,
    headers: &[(&str, &str)],
    body: Option<&[u8]>,
    now_ms: i64,
) -> Result<Value, QwenError> {
    let response = transport
        .send(method, url, headers, body, CAP, Duration::from_secs(6))
        .await
        .map_err(|err| match err {
            HttpError::Redirect => QwenError::NeedsSignIn(
                "The saved Qwen console session has expired or is incomplete.".into(),
            ),
            HttpError::TooLarge => QwenError::ProviderError(
                "The usage endpoint returned an oversized response.".into(),
            ),
            HttpError::Network => QwenError::ProviderError(
                "The Qwen console usage endpoint could not be reached.".into(),
            ),
        })?;
    if matches!(response.status, 401 | 403) {
        return Err(QwenError::NeedsSignIn(
            "The saved Qwen console session was rejected; paste a fresh session.".into(),
        ));
    }
    if response.status == 429 {
        let retry = parse_retry_after(
            response.retry_after.as_deref(),
            now_ms as f64 / 1000.0,
            PLAN_RETRY_AFTER.floor_s as f64,
            PLAN_RETRY_AFTER.ceiling_s as f64,
            PLAN_RETRY_AFTER.default_s as f64,
        );
        return Err(QwenError::RateLimited {
            retry_after_seconds: retry,
            message: "Qwen rate limited the usage check; try again later.".into(),
        });
    }
    if response.status >= 400 {
        return Err(QwenError::ProviderError(
            "The Qwen console usage endpoint returned an error.".into(),
        ));
    }
    serde_json::from_slice(&response.body).map_err(|_| {
        QwenError::ProviderError(
            "The Qwen console returned no valid token-plan quota response.".into(),
        )
    })
}

async fn send_text(
    transport: &dyn HttpTransport,
    url: &str,
    headers: &[(&str, &str)],
    now_ms: i64,
) -> Result<String, QwenError> {
    let response = transport
        .send("GET", url, headers, None, CAP, Duration::from_secs(6))
        .await
        .map_err(|err| match err {
            HttpError::Redirect => QwenError::NeedsSignIn(
                "The saved Qwen console session has expired or is incomplete.".into(),
            ),
            HttpError::TooLarge => {
                QwenError::ProviderError("The Qwen console page exceeded its size limit.".into())
            }
            HttpError::Network => {
                QwenError::ProviderError("The Qwen console page could not be reached.".into())
            }
        })?;
    if matches!(response.status, 401 | 403) {
        return Err(QwenError::NeedsSignIn(
            "The saved Qwen console session was rejected; paste a fresh session.".into(),
        ));
    }
    if response.status == 429 {
        let retry = parse_retry_after(
            response.retry_after.as_deref(),
            now_ms as f64 / 1000.0,
            PLAN_RETRY_AFTER.floor_s as f64,
            PLAN_RETRY_AFTER.ceiling_s as f64,
            PLAN_RETRY_AFTER.default_s as f64,
        );
        return Err(QwenError::RateLimited {
            retry_after_seconds: retry,
            message: "Qwen rate limited the usage check; try again later.".into(),
        });
    }
    if response.status >= 400 {
        return Err(QwenError::ProviderError(
            "The Qwen console page returned an error.".into(),
        ));
    }
    String::from_utf8(response.body)
        .map_err(|_| QwenError::ProviderError("The Qwen console page could not be decoded.".into()))
}

/// Fetch Qwen Token Plan usage directly over plain HTTPS using a console cookie header.
pub async fn fetch(
    transport: &dyn HttpTransport,
    cookie: &str,
    gateway_cookie: &str,
    region: &str,
    now_ms: i64,
) -> Result<QwenReading, QwenError> {
    if cookie.trim().is_empty()
        || cookie.len() > MAX_COOKIE_HEADER_BYTES
        || cookie.bytes().any(|b| matches!(b, 0 | 10 | 13))
    {
        return Err(QwenError::Unavailable(
            "The saved Qwen console session is invalid.".into(),
        ));
    }
    if gateway_cookie.len() > MAX_COOKIE_HEADER_BYTES
        || gateway_cookie.bytes().any(|b| matches!(b, 0 | 10 | 13))
    {
        return Err(QwenError::Unavailable(
            "The saved Qwen gateway session is invalid.".into(),
        ));
    }
    if !matches!(region, "intl" | "cn") {
        return Err(QwenError::Unavailable(
            "The saved Qwen token-plan region is unsupported.".into(),
        ));
    }
    let csrf = extract_csrf_token(gateway_cookie)?;
    let (origin, referer, gateway, action, cloud_region, sec_token, cornerstone) =
        if region == "intl" {
            let info = send_json(
                transport,
                "GET",
                INTL_INFO_URL,
                &[
                    ("Accept", "application/json"),
                    ("User-Agent", "CCS-Accounts/1.0"),
                    ("Cookie", cookie),
                ],
                None,
                now_ms,
            )
            .await?;
            let token = info["data"]["secToken"]
                .as_str()
                .filter(|s| !s.is_empty() && s.len() <= 16_384)
                .map(str::to_owned)
                .ok_or_else(|| {
                    QwenError::NeedsSignIn(
                        "The saved Qwen console session has expired or is incomplete.".into(),
                    )
                })?;
            let cornerstone = json!({
                "domain": "home.qwencloud.com",
                "consoleSite": "QWENCLOUD",
                "console": "ONE_CONSOLE",
                "xsp_lang": "en-US",
                "protocol": "V2",
                "productCode": "p_efm"
            });
            (
                INTL_ORIGIN,
                INTL_REFERER,
                INTL_GATEWAY,
                "IntlBroadScopeAspnGateway",
                "ap-southeast-1",
                token,
                cornerstone,
            )
        } else {
            let html = send_text(
                transport,
                CN_REFERER,
                &[
                    ("Accept", "text/html"),
                    ("User-Agent", "CCS-Accounts/1.0"),
                    ("Cookie", cookie),
                ],
                now_ms,
            )
            .await?;
            let token = extract_cn_sec_token(&html).ok_or_else(|| {
                QwenError::NeedsSignIn(
                    "The saved Qwen console session has expired or is incomplete.".into(),
                )
            })?;
            let trace_id = format!("aac-{now_ms:x}");
            let cornerstone = json!({
                "feTraceId": trace_id,
                "feURL": format!("{CN_REFERER}#/efm/subscription/token-plan/personal"),
                "protocol": "V2",
                "console": "ONE_CONSOLE",
                "productCode": "p_efm",
                "switchUserType": 3,
                "domain": "bailian.console.aliyun.com",
                "consoleSite": "BAILIAN_ALIYUN",
                "userNickName": "",
                "userPrincipalName": "",
                "xsp_lang": "zh-CN"
            });
            (
                CN_ORIGIN,
                CN_REFERER,
                CN_GATEWAY,
                "BroadScopeAspnGateway",
                "cn-beijing",
                token,
                cornerstone,
            )
        };

    let mut headers: Vec<(&str, &str)> = vec![
        ("Accept", "application/json"),
        ("User-Agent", "CCS-Accounts/1.0"),
        ("Content-Type", "application/x-www-form-urlencoded"),
        ("Origin", origin),
        ("Referer", referer),
        ("X-Requested-With", "XMLHttpRequest"),
    ];
    if !gateway_cookie.is_empty() {
        headers.push(("Cookie", gateway_cookie));
    }
    if let Some(ref token) = csrf {
        headers.push(("x-xsrf-token", token.as_str()));
        headers.push(("x-csrf-token", token.as_str()));
    }

    let request_one = async |suffix: &str| -> Result<Value, QwenError> {
        let endpoint = format!("zeldaHttp.apikeyMgr./tokenplan/personal/api/v2/{suffix}");
        let mut data = json!({ "cornerstoneParam": cornerstone });
        if suffix == "subscription" && region == "intl" {
            data["commodityCode"] = json!("sfm_tokenplansolo_public_intl");
        }
        if suffix == "addon/list" {
            let commodity = if region == "intl" {
                "sfm_tokenplansoloaddon_public_intl"
            } else {
                "sfm_tokenplansoloaddon_public_cn"
            };
            data["commodityCode"] = json!(commodity);
            data["status"] = json!(["ACTIVE"]);
            data["pageNum"] = json!(1);
            data["pageSize"] = json!(10);
        }
        let params = json!({ "Api": endpoint, "Data": data, "V": "1.0" });
        let query = form_encode(&[
            ("product", "sfm_bailian".to_owned()),
            ("action", action.to_owned()),
            ("api", endpoint),
        ]);
        let body = form_encode(&[
            ("product", "sfm_bailian".to_owned()),
            ("action", action.to_owned()),
            ("region", cloud_region.to_owned()),
            ("sec_token", sec_token.clone()),
            ("params", params.to_string()),
        ]);
        let url = format!("{gateway}?{query}");
        send_json(
            transport,
            "POST",
            &url,
            &headers,
            Some(body.as_bytes()),
            now_ms,
        )
        .await
    };

    let usage = request_one("usage").await?;
    let addon = request_one("addon/list").await.ok();
    let subscription = request_one("subscription").await.ok();
    let quota_config = request_one("quota-config").await.ok();

    let (plan, windows) = parse_qwen(
        &usage,
        subscription.as_ref(),
        quota_config.as_ref(),
        addon.as_ref(),
    )?;
    let now_iso = iso_seconds(now_ms);
    Ok(QwenReading {
        plan,
        windows,
        region: region.to_owned(),
        fetched_at: now_iso.clone(),
        sampled_at: now_iso,
    })
}

/// Fetch Qwen usage from a stored pasted web session.
pub async fn collect_stored(
    transport: &dyn HttpTransport,
    stored: &StoredSession,
    now_ms: i64,
) -> Result<QwenReading, QwenError> {
    let Some((cookie, gateway_cookie, region)) = stored.secret.qwen_parts() else {
        return Err(QwenError::Unavailable(
            "The saved Qwen console session is invalid.".into(),
        ));
    };
    fetch(transport, cookie, gateway_cookie, region, now_ms).await
}

/// Fetch Qwen usage from an OMP/legacy packed JSON string (`{cookie, gatewayCookie?, baseUrl?}`).
pub async fn collect_packed(
    transport: &dyn HttpTransport,
    secret_json: &str,
    now_ms: i64,
) -> Result<QwenReading, QwenError> {
    let packed: Value = serde_json::from_str(secret_json).unwrap_or(Value::Null);
    if packed.get("baseUrl").is_some_and(|v| !v.is_string()) {
        return Err(QwenError::Unavailable(
            "The saved Qwen token-plan region is unsupported.".into(),
        ));
    }
    let Some(cookie) = packed["cookie"].as_str().filter(|s| !s.trim().is_empty()) else {
        return Err(QwenError::Unavailable(
            "The saved Qwen API key has no console session; live token-plan usage requires its existing console cookie."
                .into(),
        ));
    };
    let base = packed
        .get("baseUrl")
        .and_then(Value::as_str)
        .unwrap_or(QWEN_INTL_BASE)
        .trim_end_matches('/');
    let region = if base == QWEN_INTL_BASE {
        "intl"
    } else if base == QWEN_CN_BASE {
        "cn"
    } else {
        return Err(QwenError::Unavailable(
            "The saved Qwen token-plan region is unsupported.".into(),
        ));
    };
    let gateway_cookie = packed
        .get("gatewayCookie")
        .unwrap_or(&packed["cookie"])
        .as_str()
        .ok_or_else(|| {
            QwenError::Unavailable("The saved Qwen gateway session is invalid.".into())
        })?;
    fetch(transport, cookie, gateway_cookie, region, now_ms).await
}

fn api_number(value: &Value) -> Option<f64> {
    match value {
        Value::Number(n) => n.as_f64().filter(|f| f.is_finite() && *f >= 0.0),
        Value::String(s) => s
            .trim()
            .parse::<f64>()
            .ok()
            .filter(|f| f.is_finite() && *f >= 0.0),
        _ => None,
    }
}

fn percent(value: Option<f64>) -> Option<f64> {
    value.filter(|v| v.is_finite() && (0.0..=100.0).contains(v))
}

fn reset_at(value: &Value) -> Option<String> {
    match value {
        Value::Null | Value::Bool(_) => None,
        Value::Number(n) => {
            let raw = n.as_f64()?;
            if !raw.is_finite() || raw <= 0.0 {
                return None;
            }
            let millis = if raw > 1e11 {
                raw as i64
            } else {
                (raw * 1000.0) as i64
            };
            let utc = chrono::DateTime::from_timestamp_millis(millis)?;
            let year = chrono::Datelike::year(&utc);
            if !(2000..=2200).contains(&year) {
                return None;
            }
            Some(utc.format("%Y-%m-%dT%H:%M:%SZ").to_string())
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
                return reset_at(&json!(num));
            }
            parse_iso_millis(trimmed).map(|iso| iso.replace(".000Z", "Z"))
        }
        _ => None,
    }
}

fn plan_label(value: Option<&Value>) -> Option<String> {
    let raw = value?.as_str()?.trim();
    if raw.is_empty() || raw.len() > 48 {
        return None;
    }
    let clean = aac_core::scrub(raw);
    let trimmed = clean.as_str().trim();
    if trimmed.is_empty()
        || !trimmed.as_bytes()[0].is_ascii_alphabetic()
        || !trimmed
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b" ._+-".contains(&b))
    {
        return None;
    }
    Some(trimmed.to_owned())
}

fn unwrap_qwen(payload: &Value, markers: &[&str]) -> Result<Value, QwenError> {
    let mut current = payload.clone();
    for _ in 0..7 {
        let Some(obj) = current.as_object() else {
            break;
        };
        if obj.get("successResponse") == Some(&Value::Bool(false))
            || obj.get("success") == Some(&Value::Bool(false))
        {
            return Err(QwenError::NeedsSignIn(
                "The Qwen console session could not access token-plan usage.".into(),
            ));
        }
        if markers.iter().any(|k| obj.contains_key(*k)) {
            return Ok(current);
        }
        let next = obj
            .get("DataV2")
            .or_else(|| obj.get("Data"))
            .or_else(|| obj.get("data"))
            .cloned()
            .unwrap_or(Value::Null);
        current = if let Some(text) = next.as_str() {
            match serde_json::from_str::<Value>(text) {
                Ok(v) => v,
                Err(_) => break,
            }
        } else {
            next
        };
    }
    Err(QwenError::ProviderError(
        "The Qwen console returned no valid token-plan quota response.".into(),
    ))
}

#[allow(clippy::too_many_arguments)]
fn build_window(
    key: &str,
    label: &str,
    used_percent: Option<f64>,
    reset: Option<String>,
    window_minutes: Option<f64>,
    used: Option<f64>,
    limit: Option<f64>,
    remaining: Option<f64>,
    expires_at: Option<String>,
    unit: Option<&str>,
    kind: &str,
) -> Value {
    let used_pct = percent(used_percent);
    let remaining_pct = used_pct.map(|p| (100.0 - p).max(0.0));
    let mut w = json!({
        "key": key,
        "label": label,
        "kind": kind,
        "usedPercent": used_pct,
        "remainingPercent": remaining_pct,
        "resetAt": reset,
        "windowMinutes": window_minutes,
        "used": used,
        "limit": limit,
        "unit": unit,
    });
    if let Some(rem) = remaining {
        w["remaining"] = json!(rem);
    }
    if let Some(exp) = expires_at {
        w["expiresAt"] = json!(exp);
    }
    w
}

/// Parse Qwen Token Plan usage, subscription, quota-config, and addon credit-pack payloads.
pub fn parse_qwen(
    usage: &Value,
    subscription: Option<&Value>,
    quota_config: Option<&Value>,
    addon: Option<&Value>,
) -> Result<(Option<String>, Vec<Value>), QwenError> {
    let usage_markers = [
        "per5HourPercentage",
        "per1WeekPercentage",
        "per1MonthPercentage",
        "per5HourResetTime",
        "per1WeekResetTime",
        "per1MonthResetTime",
    ];
    let data = unwrap_qwen(usage, &usage_markers)?;
    let subscription_data = subscription.and_then(|sub| {
        unwrap_qwen(
            sub,
            &["specCode", "spec_code", "planName", "plan_name", "endTime"],
        )
        .ok()
    });
    let tier = subscription_data.as_ref().and_then(|sub| {
        sub.get("specCode")
            .or_else(|| sub.get("spec_code"))
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let quotas = match (&tier, quota_config) {
        (Some(tier_code), Some(cfg)) => unwrap_qwen(cfg, &[tier_code.as_str()])
            .ok()
            .and_then(|map| map.get(tier_code).cloned())
            .filter(Value::is_object),
        _ => None,
    };
    let mut windows = Vec::new();
    for (prefix, key, label, minutes, limit_keys) in [
        (
            "per5Hour",
            "5h",
            "5 hours",
            Some(300.0),
            &["five_hour", "fiveHour"][..],
        ),
        (
            "per1Week",
            "weekly",
            "Weekly",
            Some(10080.0),
            &["weekly"][..],
        ),
        ("per1Month", "monthly", "Monthly", None, &["monthly"][..]),
    ] {
        let ratio = api_number(&data[format!("{prefix}Percentage")]);
        let reset = reset_at(&data[format!("{prefix}ResetTime")]);
        if ratio.is_none() && reset.is_none() {
            continue;
        }
        let used_percent = ratio.map(|r| if r <= 1.0 { r * 100.0 } else { r });
        let limit = quotas.as_ref().and_then(|q| {
            limit_keys
                .iter()
                .find_map(|name| q.get(*name).and_then(api_number))
        });
        let valid_pct = percent(used_percent);
        let used = match (limit, valid_pct) {
            (Some(lim), Some(pct)) => Some(lim * pct / 100.0),
            _ => None,
        };
        let remaining = match (limit, used) {
            (Some(lim), Some(u)) => Some(lim - u),
            _ => None,
        };
        windows.push(build_window(
            key,
            label,
            used_percent,
            reset,
            minutes,
            used,
            limit,
            remaining,
            None,
            limit.map(|_| "credits"),
            "rate_limit",
        ));
    }

    let mut plan = None;
    if let Some(sub) = &subscription_data {
        plan = plan_label(sub.get("planName").or_else(|| sub.get("plan_name")))
            .or_else(|| tier.as_deref().and_then(|t| plan_label(Some(&json!(t)))));
        if let Some(expiry) = reset_at(&sub["endTime"]) {
            windows.push(build_window(
                "subscription",
                "Plan subscription",
                None,
                None,
                None,
                None,
                None,
                None,
                Some(expiry),
                None,
                "rate_limit",
            ));
        }
    }

    if let Some(addon_payload) = addon
        && let Ok(extras) = unwrap_qwen(
            addon_payload,
            &["remainingCredits", "totalCredits", "activeCount", "items"],
        )
        && extras.is_object()
    {
        let limit = api_number(&extras["totalCredits"]);
        let remaining = api_number(&extras["remainingCredits"]);
        let used = match (limit, remaining) {
            (Some(lim), Some(rem)) if rem <= lim => Some(lim - rem),
            _ => None,
        };
        let ratio = match (used, limit) {
            (Some(u), Some(lim)) if lim > 0.0 => Some(100.0 * u / lim),
            _ => None,
        };
        if limit.is_some() || remaining.is_some() {
            windows.push(build_window(
                "addon-credits",
                "Additional credits",
                ratio,
                None,
                None,
                used,
                limit,
                remaining,
                None,
                Some("credits"),
                "balance",
            ));
        }
        if let Some(active) = api_number(&extras["activeCount"]) {
            windows.push(build_window(
                "addon-packs",
                "Active credit packs",
                None,
                None,
                None,
                None,
                None,
                Some(active),
                None,
                Some("packs"),
                "balance",
            ));
        }
        if let Some(items) = extras.get("items").and_then(Value::as_array) {
            let mut all_valid = true;
            for (index, item) in items.iter().take(100).enumerate() {
                let Some(obj) = item.as_object() else {
                    all_valid = false;
                    continue;
                };
                let total = obj.get("totalCredits").and_then(api_number);
                let rem = obj.get("remainingCredits").and_then(api_number);
                let expiry = obj.get("endTime").and_then(reset_at);
                if total.is_none() && rem.is_none() && expiry.is_none() {
                    all_valid = false;
                    continue;
                }
                let spent = match (total, rem) {
                    (Some(t), Some(r)) if r <= t => Some(t - r),
                    _ => None,
                };
                let ratio = match (spent, total) {
                    (Some(s), Some(t)) if t > 0.0 => Some(s / t * 100.0),
                    _ => None,
                };
                let identifier = match obj.get("orderId") {
                    Some(Value::String(s)) => s.clone(),
                    Some(Value::Number(n)) => n.to_string(),
                    _ => index.to_string(),
                };
                let key = format!("addon-pack-{}", &aac_core::sha256_hex(identifier)[..12]);
                windows.push(build_window(
                    &key,
                    &format!("Additional credit pack {}", index + 1),
                    ratio,
                    None,
                    None,
                    spent,
                    total,
                    rem,
                    expiry,
                    Some("credits"),
                    "balance",
                ));
            }
            if items.len() > 100
                && !items[100..].iter().all(|item| {
                    item.as_object().is_some_and(|obj| {
                        obj.get("totalCredits").and_then(api_number).is_some()
                            || obj.get("remainingCredits").and_then(api_number).is_some()
                            || obj.get("endTime").and_then(reset_at).is_some()
                    })
                })
            {
                all_valid = false;
            }
            if all_valid {
                windows.push(build_window(
                    "addon-listed-packs",
                    "Listed active credit packs",
                    None,
                    None,
                    None,
                    None,
                    None,
                    Some(items.len() as f64),
                    None,
                    Some("packs"),
                    "balance",
                ));
            }
        }
    }

    let valid_windows: Vec<Value> = windows
        .into_iter()
        .filter(|w| {
            w["usedPercent"].is_number()
                || w["remainingPercent"].is_number()
                || w["used"].is_number()
                || w["limit"].is_number()
                || w["remaining"].is_number()
                || w["resetAt"].is_string()
                || w["expiresAt"].is_string()
        })
        .collect();
    if valid_windows.is_empty() {
        return Err(QwenError::ProviderError(
            "The Qwen console returned no usable token-plan quota windows.".into(),
        ));
    }
    Ok((plan, valid_windows))
}

/// Convert a successful [`QwenReading`] into a public dashboard account row.
pub fn to_account_row(reading: &QwenReading, platform: &str, source_label: &str) -> Value {
    json!({
        "id": "qwen:usage",
        "provider": "qwen",
        "providerLabel": "Qwen token plan",
        "label": "Qwen token plan",
        "email": null,
        "plan": reading.plan,
        "platform": platform,
        "source": source_label,
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::web_session::HttpResponse;
    use std::sync::Mutex;

    type RecordedRequest = (String, String, Vec<(String, String)>);

    struct FakeQwen {
        requests: Mutex<Vec<RecordedRequest>>,
        cn: bool,
        reject_auth: bool,
    }

    impl HttpTransport for FakeQwen {
        fn send<'a>(
            &'a self,
            method: &'a str,
            url: &'a str,
            headers: &'a [(&'a str, &'a str)],
            _body: Option<&'a [u8]>,
            _cap: usize,
            _timeout: Duration,
        ) -> std::pin::Pin<
            Box<dyn std::future::Future<Output = Result<HttpResponse, HttpError>> + Send + 'a>,
        > {
            self.requests.lock().unwrap().push((
                method.to_owned(),
                url.to_owned(),
                headers
                    .iter()
                    .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
                    .collect(),
            ));
            let response = if self.reject_auth {
                HttpResponse {
                    status: 401,
                    retry_after: None,
                    body: Vec::new(),
                }
            } else if url.contains("/tool/user/info.json") {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: json!({ "data": { "secToken": "sec-intl-123" } })
                        .to_string()
                        .into_bytes(),
                }
            } else if url.contains("bailian.console.aliyun.com/cn-beijing") {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: b"<html><script>window.CFG={SEC_TOKEN: \"sec-cn-456\"};</script></html>"
                        .to_vec(),
                }
            } else if url.contains("api/v2/usage") || url.contains("api%2Fv2%2Fusage") {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: json!({
                        "DataV2": {
                            "data": {
                                "per5HourPercentage": 0.25,
                                "per5HourResetTime": "2026-11-26T22:00:00Z",
                                "per1WeekPercentage": 40,
                                "per1WeekResetTime": "2026-12-01T00:00:00Z",
                                "per1MonthPercentage": 0.1,
                                "per1MonthResetTime": "2026-12-26T00:00:00Z"
                            }
                        }
                    })
                    .to_string()
                    .into_bytes(),
                }
            } else if url.contains("subscription") {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: json!({
                        "data": {
                            "specCode": "solo_pro",
                            "planName": "Solo Pro",
                            "endTime": "2026-12-31T00:00:00Z"
                        }
                    })
                    .to_string()
                    .into_bytes(),
                }
            } else if url.contains("quota-config") {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: json!({
                        "data": {
                            "solo_pro": {
                                "five_hour": 1000,
                                "weekly": 5000,
                                "monthly": 20000
                            }
                        }
                    })
                    .to_string()
                    .into_bytes(),
                }
            } else if url.contains("addon") {
                HttpResponse {
                    status: 200,
                    retry_after: None,
                    body: json!({
                        "data": {
                            "totalCredits": 2000,
                            "remainingCredits": 1500,
                            "activeCount": 1,
                            "items": [{
                                "orderId": "ord-99",
                                "totalCredits": 2000,
                                "remainingCredits": 1500,
                                "endTime": "2027-01-15T00:00:00Z"
                            }]
                        }
                    })
                    .to_string()
                    .into_bytes(),
                }
            } else {
                let _ = self.cn;
                HttpResponse {
                    status: 404,
                    retry_after: None,
                    body: Vec::new(),
                }
            };
            Box::pin(async move { Ok(response) })
        }
    }

    #[tokio::test]
    async fn qwen_intl_and_cn_decode_csrf_and_parse_subscription_quota_and_addons() {
        for (region, expected_origin) in [
            ("intl", "https://home.qwencloud.com"),
            ("cn", "https://bailian.console.aliyun.com"),
        ] {
            let fake = FakeQwen {
                requests: Mutex::default(),
                cn: region == "cn",
                reject_auth: false,
            };
            let reading = fetch(
                &fake,
                "cna=abc; login_aliyunid_csrf=tk%2B1%2F2",
                "cna=abc; login_aliyunid_csrf=tk%2B1%2F2",
                region,
                1_795_720_000_000,
            )
            .await
            .unwrap();
            assert_eq!(reading.plan.as_deref(), Some("Solo Pro"));
            assert_eq!(reading.region, region);
            let w5 = reading.windows.iter().find(|w| w["key"] == "5h").unwrap();
            assert_eq!(w5["usedPercent"], json!(25.0));
            assert_eq!(w5["used"], json!(250.0));
            assert_eq!(w5["limit"], json!(1000.0));
            assert_eq!(w5["unit"], json!("credits"));
            assert!(reading.windows.iter().any(|w| w["key"] == "subscription"));
            assert!(reading.windows.iter().any(|w| w["key"] == "addon-credits"));
            assert!(reading.windows.iter().any(|w| w["key"] == "addon-packs"));
            assert!(reading.windows.iter().any(|w| {
                w["key"]
                    .as_str()
                    .is_some_and(|k| k.starts_with("addon-pack-"))
            }));
            assert!(
                reading
                    .windows
                    .iter()
                    .any(|w| w["key"] == "addon-listed-packs")
            );

            let reqs = fake.requests.lock().unwrap();
            let post = reqs.iter().find(|(m, _, _)| m == "POST").unwrap();
            assert!(
                post.2
                    .iter()
                    .any(|(k, v)| k == "Origin" && v == expected_origin)
            );
            assert!(
                post.2
                    .iter()
                    .any(|(k, v)| k == "x-xsrf-token" && v == "tk+1/2")
            );
            assert!(
                post.2
                    .iter()
                    .any(|(k, v)| k == "x-csrf-token" && v == "tk+1/2")
            );
        }
    }

    #[tokio::test]
    async fn qwen_401_and_crlf_csrf_fail_honestly() {
        let fake = FakeQwen {
            requests: Mutex::default(),
            cn: false,
            reject_auth: true,
        };
        assert!(matches!(
            fetch(
                &fake,
                "cna=expired",
                "cna=expired",
                "intl",
                1_795_720_000_000
            )
            .await,
            Err(QwenError::NeedsSignIn(_))
        ));
        assert!(matches!(
            fetch(
                &fake,
                "cna=ok; csrf=bad%0d%0ainject",
                "cna=ok; csrf=bad%0d%0ainject",
                "intl",
                1_795_720_000_000,
            )
            .await,
            Err(QwenError::Unavailable(_))
        ));
    }
}
