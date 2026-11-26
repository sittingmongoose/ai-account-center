//! Exact port of `scripts/account-usage/cursor_usage.py`.
use crate::failover_additional::{
    CollectionError, PlanHttpBody, PlanTransport,
    desktop::{
        Credential, WindowSpec, email, jwt_claims, nonnegative, number, percent, quota_window,
        reset_at, utc_now_iso_secs,
    },
};
use serde_json::{Map, Value, json};

pub(crate) const SUMMARY_URL: &str = "https://cursor.com/api/usage-summary";
pub(crate) const PROFILE_URL: &str = "https://cursor.com/api/auth/me";
pub(crate) const MEMBERSHIP_URL: &str = "https://cursor.com/api/auth/stripe";
pub(crate) const PLAN_INFO_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetPlanInfo";
pub(crate) const CURRENT_USAGE_URL: &str =
    "https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage";

fn is_new_plan(plan: &str) -> bool {
    matches!(plan, "plus" | "premium" | "super" | "unified_ultra")
}

fn is_legacy_plan(plan: &str) -> bool {
    matches!(
        plan,
        "free"
            | "pro"
            | "pro_plus"
            | "ultra"
            | "enterprise"
            | "free_trial"
            | "pro_student"
            | "express"
            | "team"
            | "business"
    )
}

pub(crate) fn membership(value: &Value) -> Option<String> {
    let raw = value.as_str()?;
    if raw.is_empty()
        || raw.len() > 64
        || !raw.is_ascii()
        || !raw.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return None;
    }
    let lower = raw.to_ascii_lowercase();
    if is_new_plan(&lower) || is_legacy_plan(&lower) {
        Some(lower)
    } else {
        Some("unknown".to_owned())
    }
}

fn percent_encode_strict(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for &b in raw.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub(crate) fn cookie(credential: &Credential) -> Result<String, CollectionError> {
    let claims = jwt_claims(&credential.access);
    let Some(subject) = claims
        .get("sub")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty() && s.len() <= 256)
    else {
        return Err(CollectionError {
            status: "needs_sign_in",
            message: "The saved Cursor account has no usable session identity.",
            failure_code: None,
            retry_after: None,
        });
    };
    let user_id = subject.rsplit('|').next().unwrap_or("").trim();
    if user_id.is_empty() || user_id.bytes().any(|b| b < 32) {
        return Err(CollectionError {
            status: "needs_sign_in",
            message: "The saved Cursor account has no usable session identity.",
            failure_code: None,
            retry_after: None,
        });
    }
    Ok(format!(
        "WorkosCursorSessionToken={}",
        percent_encode_strict(&format!("{user_id}::{}", credential.access))
    ))
}

pub(crate) fn normalize_legacy(payload: &Value) -> Vec<Value> {
    let Some(usage) = payload.get("individualUsage").and_then(Value::as_object) else {
        return Vec::new();
    };
    let reset = ["billingCycleEnd", "endOfMonth", "resetsAt", "nextReset"]
        .into_iter()
        .find_map(|k| {
            payload
                .get(k)
                .filter(|v| !v.is_null() && v.as_str() != Some(""))
        });
    let mut windows = Vec::new();
    for (key, label) in [("plan", "Plan spend"), ("onDemand", "On-demand spend")] {
        let Some(bucket) = usage.get(key).and_then(Value::as_object) else {
            continue;
        };
        let used = bucket.get("used").and_then(nonnegative);
        let limit = bucket.get("limit").and_then(nonnegative);
        let reported_percentage = bucket.get("totalPercentUsed").and_then(percent);
        let spend_percentage = match (used, limit) {
            (Some(u), Some(l)) if l > 0.0 => Some((u / l * 100.0).min(100.0)),
            _ => None,
        };
        let enabled = bucket.get("enabled").and_then(Value::as_bool);
        let plan_unlimited = payload.get("isUnlimited").and_then(Value::as_bool);
        let bucket_unlimited = bucket.get("unlimited").and_then(Value::as_bool);
        if let Some(rp) = reported_percentage {
            windows.push(quota_window(WindowSpec {
                key: &format!("{key}-reported"),
                label: if key == "plan" {
                    "Included usage"
                } else {
                    "On-demand usage"
                },
                used_percent: Some(rp),
                reset,
                kind: "rate_limit",
                enabled,
                unlimited: if key == "plan" {
                    plan_unlimited
                } else {
                    bucket_unlimited
                },
                ..Default::default()
            }));
        }
        if key == "plan" {
            for (field, detail_label) in [
                ("autoPercentUsed", "Cursor models"),
                ("apiPercentUsed", "Other models"),
            ] {
                if let Some(p) = bucket.get(field).and_then(percent) {
                    windows.push(quota_window(WindowSpec {
                        key: field,
                        label: detail_label,
                        used_percent: Some(p),
                        reset,
                        kind: "rate_limit",
                        enabled,
                        unlimited: plan_unlimited,
                        ..Default::default()
                    }));
                }
            }
        }
        if let Some(u) = used {
            let rem = bucket
                .get("remaining")
                .and_then(nonnegative)
                .map(|r| r / 100.0);
            windows.push(quota_window(WindowSpec {
                key,
                label,
                used_percent: spend_percentage,
                reset,
                used: Some(u / 100.0),
                limit: limit.map(|l| l / 100.0),
                unit: Some("USD".to_owned()),
                kind: if key == "plan" {
                    "spend"
                } else {
                    "extra_usage"
                },
                remaining: rem,
                enabled,
                unlimited: bucket_unlimited,
                ..Default::default()
            }));
        }
    }
    if windows.is_empty()
        && let Some(overall) = usage.get("overall").filter(|v| v.is_object())
    {
        let mut cloned = payload.clone();
        if let Some(obj) = cloned.as_object_mut() {
            obj.insert("individualUsage".into(), json!({ "plan": overall }));
            return normalize_legacy(&cloned);
        }
    }
    windows
}

fn period(plan_info: &Value) -> Option<&'static str> {
    let info = plan_info.get("planInfo")?.as_object()?;
    let val = info.get("includedUsagePeriod")?;
    if val.is_boolean() {
        return None;
    }
    if val.as_i64() == Some(2) || val.as_str() == Some("INCLUDED_USAGE_PERIOD_WEEKLY") {
        return Some("weekly");
    }
    if val.as_i64() == Some(1) || val.as_str() == Some("INCLUDED_USAGE_PERIOD_MONTHLY") {
        return Some("monthly");
    }
    None
}

fn milliseconds_reset(value: Option<&Value>) -> Option<String> {
    let val = value?;
    if val.is_boolean() {
        return None;
    }
    let ms = if let Some(s) = val.as_str() {
        if !s.is_ascii() || s.is_empty() || s.len() > 16 || !s.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        s.parse::<f64>().ok()?
    } else {
        number(val)?
    };
    reset_at(&json!(ms / 1000.0))
}

pub(crate) fn normalize_shared(payload: &Value, plan_info: &Value, plan: &str) -> Vec<Value> {
    if plan_info.get("isMerged") == Some(&Value::Bool(false)) {
        return Vec::new();
    }
    let Some(val) = payload
        .get("planUsage")
        .and_then(|b| b.get("totalPercentUsed"))
        .and_then(percent)
    else {
        return Vec::new();
    };
    let label = if plan == "plus" {
        "Grok"
    } else {
        "Grok and Composer"
    };
    let info = plan_info
        .get("planInfo")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or_else(|| json!({}));
    let cadence = period(plan_info);
    let reset = match cadence {
        Some("weekly") => milliseconds_reset(info.get("includedUsageResetsAt")),
        Some("monthly") => milliseconds_reset(payload.get("billingCycleEnd"))
            .or_else(|| milliseconds_reset(info.get("billingCycleEnd"))),
        _ => None,
    };
    let reset_val = reset.map(Value::String);
    let key = format!("grok-shared-{}", cadence.unwrap_or("usage"));
    vec![quota_window(WindowSpec {
        key: &key,
        label,
        used_percent: Some(val),
        reset: reset_val.as_ref(),
        minutes: (cadence == Some("weekly")).then_some(10_080.0),
        kind: "rate_limit",
        ..Default::default()
    })]
}

pub(crate) fn normalize_additional(payload: &Value) -> Vec<Value> {
    let Some(on_demand) = payload
        .get("individualUsage")
        .and_then(|u| u.get("onDemand"))
        .filter(|v| v.is_object())
    else {
        return Vec::new();
    };
    let synth = json!({
        "individualUsage": { "onDemand": on_demand },
        "billingCycleEnd": payload.get("billingCycleEnd").cloned().unwrap_or(Value::Null),
    });
    normalize_legacy(&synth)
        .into_iter()
        .filter(|w| w.get("kind").and_then(Value::as_str) == Some("extra_usage"))
        .map(|mut w| {
            if let Some(obj) = w.as_object_mut() {
                obj.insert("label".into(), json!("Additional usage spend"));
            }
            w
        })
        .collect()
}

pub(crate) fn observation(result: &Value, failure_code: Option<&str>) -> Map<String, Value> {
    let mut out = Map::new();
    if failure_code == Some("account_mismatch") {
        return out;
    }
    let plan = result.get("observedPlan").and_then(Value::as_str);
    let at = result.get("observedPlanAt").and_then(Value::as_str);
    if let (Some(plan), Some(at)) = (plan, at)
        && (is_new_plan(plan) || is_legacy_plan(plan) || plan == "unknown")
        && reset_at(&Value::String(at.to_owned())).is_some()
    {
        out.insert("observedPlan".into(), json!(plan));
        out.insert("observedPlanAt".into(), json!(at));
    }
    out
}

fn cooldown(error: &CollectionError) -> Map<String, Value> {
    let mut out = Map::new();
    if error.failure_code == Some("rate_limited") {
        out.insert("failureCode".into(), json!("rate_limited"));
        out.insert(
            "retryAfterSeconds".into(),
            json!(error.retry_after.unwrap_or(600.0)),
        );
    }
    out
}

async fn get_json(
    client: &dyn PlanTransport,
    url: &str,
    headers: &[(String, String)],
    body: Option<&[u8]>,
) -> Result<Value, CollectionError> {
    match client.request(url, headers, body, false).await? {
        PlanHttpBody::Json(v) => Ok(v),
        PlanHttpBody::Text(_) => Err(CollectionError {
            status: "error",
            message: "The usage service is unavailable or returned an invalid response.",
            failure_code: None,
            retry_after: None,
        }),
    }
}

pub(crate) async fn collect(
    credential: &Credential,
    result: &mut Value,
    client: &dyn PlanTransport,
    now_secs: Option<f64>,
) -> Result<Map<String, Value>, CollectionError> {
    result["plan"] = Value::Null;
    let headers = vec![("Cookie".to_owned(), cookie(credential)?)];
    let stripe = get_json(client, MEMBERSHIP_URL, &headers, None).await?;
    let Some(plan) = stripe.get("membershipType").and_then(membership) else {
        return Err(CollectionError {
            status: "unavailable",
            message: "Cursor did not report a recognized subscription identity.",
            failure_code: Some("invalid_response"),
            retry_after: None,
        });
    };
    let observed_at = utc_now_iso_secs(now_secs);
    result["plan"] = json!(plan);
    result["observedPlan"] = json!(plan);
    result["observedPlanAt"] = json!(observed_at);

    let rpc_headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credential.access),
        ),
        ("Content-Type".to_owned(), "application/json".to_owned()),
        ("Connect-Protocol-Version".to_owned(), "1".to_owned()),
    ];
    let mut info = json!({});
    if plan == "unknown" {
        info = get_json(client, PLAN_INFO_URL, &rpc_headers, Some(b"{}")).await?;
        if info.get("isMerged") != Some(&Value::Bool(true)) {
            return Err(CollectionError {
                status: "unavailable",
                message: "Cursor returned an unsupported subscription type.",
                failure_code: Some("invalid_response"),
                retry_after: None,
            });
        }
    }
    if is_new_plan(&plan) || plan == "unknown" {
        let payload = get_json(client, CURRENT_USAGE_URL, &rpc_headers, Some(b"{}")).await?;
        let windows = normalize_shared(&payload, &info, &plan);
        if windows.is_empty() {
            return Err(CollectionError {
                status: "unavailable",
                message: "Cursor did not report its shared usage percentage.",
                failure_code: Some("invalid_response"),
                retry_after: None,
            });
        }
        result["windows"] = Value::Array(windows);
        if plan != "unknown" {
            match get_json(client, PLAN_INFO_URL, &rpc_headers, Some(b"{}")).await {
                Err(error) => {
                    result["message"] = json!("Usage is current; the usage reset is unavailable.");
                    let cd = cooldown(&error);
                    if !cd.is_empty() {
                        let mut out = observation(result, None);
                        out.extend(cd);
                        return Ok(out);
                    }
                }
                Ok(fetched_info) => {
                    if fetched_info.get("isMerged") == Some(&Value::Bool(false)) {
                        result["windows"] = json!([]);
                        return Err(CollectionError {
                            status: "unavailable",
                            message: "Cursor returned inconsistent subscription metadata.",
                            failure_code: Some("invalid_response"),
                            retry_after: None,
                        });
                    }
                    result["windows"] =
                        Value::Array(normalize_shared(&payload, &fetched_info, &plan));
                }
            }
        }
        match get_json(client, SUMMARY_URL, &headers, None).await {
            Ok(summary) => {
                if let Some(arr) = result["windows"].as_array_mut() {
                    arr.extend(normalize_additional(&summary));
                }
            }
            Err(error) => {
                result["message"] =
                    json!("Included usage is current; additional usage spend is unavailable.");
                let cd = cooldown(&error);
                if !cd.is_empty() {
                    let mut out = observation(result, None);
                    out.extend(cd);
                    return Ok(out);
                }
            }
        }
    } else {
        let payload = get_json(client, SUMMARY_URL, &headers, None).await?;
        result["windows"] = Value::Array(normalize_legacy(&payload));
    }

    let profile = match get_json(client, PROFILE_URL, &headers, None).await {
        Ok(p) => p,
        Err(error) => {
            let mut out = observation(result, None);
            out.extend(cooldown(&error));
            return Ok(out);
        }
    };
    let user = profile
        .get("user")
        .filter(|v| v.is_object())
        .unwrap_or(&profile);
    let address = user.get("email").and_then(email);
    if let (Some(addr), Some(expected)) = (&address, &credential.email)
        && !addr.eq_ignore_ascii_case(expected)
    {
        if let Some(obj) = result.as_object_mut() {
            obj.remove("observedPlan");
            obj.remove("observedPlanAt");
        }
        result["plan"] = Value::Null;
        return Err(CollectionError {
            status: "unavailable",
            message: "The saved account identity could not be verified.",
            failure_code: Some("account_mismatch"),
            retry_after: None,
        });
    }
    if let Some(addr) = address {
        result["email"] = json!(addr);
    }
    Ok(observation(result, None))
}
