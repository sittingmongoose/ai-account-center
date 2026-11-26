//! Antigravity standalone discovery, Gemini per-model derivation, refresh, and two-service
//! quota collection ported from `scripts/account-usage/desktop_usage.py`.
use crate::desktop_usage::{
    AGY_BASE, AGY_CLI_BASE, AGY_OPTIONAL_BUDGET_SECONDS, AGY_REQUIRED_BUDGET_SECONDS,
    AGY_USER_AGENT, AntigravityServiceResult, Credential, DesktopEnv, DesktopTransport,
    ElapsedClock, GEMINI_RESET_CLUSTER_SECONDS, ReqwestDesktopTransport, UsageError,
    WindowsAclCheck,
    helpers::{from_internal_cred, from_internal_err},
};
use crate::failover_additional::{
    desktop::{
        self, ReadTracker, WindowSpec, agy_groups, antigravity_oauth_client_with,
        antigravity_reported_plan, antigravity_service_label, antigravity_unavailable_windows,
        credential_from_omp, jwt_claims, normalize_antigravity, normalize_antigravity_credits,
        now_unix_secs, number, parse_iso_utc_strict, quota_window, read_json_file, reset_at,
        safe_text, secret,
    },
    sqlite,
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

const DEFAULT_AGY_SERVICES: [(&str, &str); 2] = [("cli", AGY_CLI_BASE), ("app", AGY_BASE)];
const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";

/// Discover Antigravity credentials from native consumer file or OMP `agent.db`.
pub fn antigravity_credentials(home: &Path, tracker: &mut ReadTracker) -> Option<Credential> {
    let path = home.join(".gemini/antigravity-cli/antigravity-oauth-token");
    if let Some(value) = read_json_file(&path, tracker)
        && value.get("auth_method").and_then(Value::as_str) == Some("consumer")
        && let Some(token) = value.get("token").and_then(Value::as_object)
        && let Some(access) = token.get("access_token").and_then(secret)
    {
        let id_tok = value.get("id_token").and_then(Value::as_str).unwrap_or("");
        let claims = jwt_claims(id_tok);
        let expiry = token
            .get("expiry")
            .and_then(reset_at)
            .and_then(|iso| parse_iso_utc_strict(&iso))
            .map(|dt| (dt.timestamp() as f64) * 1000.0);
        let refresh = token
            .get("refresh_token")
            .and_then(secret)
            .unwrap_or_default();
        return Some(Credential {
            access,
            source: "Antigravity CLI".to_owned(),
            refresh,
            expires: expiry,
            email: claims.get("email").and_then(desktop::email),
            project: None,
            plan: None,
            google_client: Some("consumer".to_owned()),
        });
    }
    for row in sqlite::omp_desktop_oauth_rows(home, "google-antigravity", tracker) {
        if let Some(cred) = credential_from_omp("antigravity", &row) {
            return Some(from_internal_cred(cred));
        }
    }
    None
}

/// Whether the quota summary already reports the Gemini Models group (excluding Gemini 4 Argon).
pub fn antigravity_summary_has_gemini(summary_windows: &[Value]) -> bool {
    summary_windows.iter().any(|window| {
        let Some(obj) = window.as_object() else {
            return false;
        };
        let key = obj
            .get("key")
            .map(|v| match v.as_str() {
                Some(s) => s.to_lowercase(),
                None => v.to_string().to_lowercase(),
            })
            .unwrap_or_default();
        let label = obj
            .get("label")
            .map(|v| match v.as_str() {
                Some(s) => s.to_lowercase(),
                None => v.to_string().to_lowercase(),
            })
            .unwrap_or_default();
        (key.contains("gemini") || label.starts_with("gemini"))
            && !key.contains("argon")
            && !label.contains("argon")
    })
}

fn antigravity_gemini_period(
    reset: &str,
    anchors: &[(Option<&str>, Option<f64>)],
) -> Option<(&'static str, f64)> {
    let stamp = parse_iso_utc_strict(reset)?.timestamp();
    let mut has_5h = false;
    let mut has_weekly = false;
    for &(anchor_reset, minutes) in anchors {
        let Some(anchor_str) = anchor_reset else {
            continue;
        };
        let Some(mins) = minutes else {
            continue;
        };
        let is_5h = mins == 300.0;
        let is_weekly = mins == 10_080.0;
        if !is_5h && !is_weekly {
            continue;
        }
        let Some(anchor_ts) = parse_iso_utc_strict(anchor_str).map(|d| d.timestamp()) else {
            continue;
        };
        if (stamp - anchor_ts).abs() <= 600 {
            if is_5h {
                has_5h = true;
            } else {
                has_weekly = true;
            }
        }
    }
    match (has_5h, has_weekly) {
        (true, false) => Some(("5h", 300.0)),
        (false, true) => Some(("weekly", 10_080.0)),
        _ => None,
    }
}

type GeminiMember = (Option<String>, f64);
type GeminiGroup = (String, Vec<GeminiMember>);
type TimestampedGeminiGroup = (i64, String, Vec<GeminiMember>);

fn antigravity_gemini_groups(buckets: Option<&Value>) -> Vec<GeminiGroup> {
    let Some(items) = buckets.and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut collected: Vec<(i64, String, Option<String>, f64)> = Vec::new();
    for bucket in items.iter().take(256) {
        let Some(obj) = bucket.as_object() else {
            continue;
        };
        let Some(reset) = obj.get("resetTime").and_then(reset_at) else {
            continue;
        };
        let Some(model_id) = obj.get("modelId").and_then(Value::as_str) else {
            continue;
        };
        let char_len = model_id.chars().count();
        if !model_id.starts_with("gemini-")
            || model_id.to_lowercase().contains("argon")
            || char_len == 0
            || char_len > 160
        {
            continue;
        }
        let fraction = match obj.get("remainingFraction").and_then(number) {
            Some(f) => f,
            None => {
                if obj.get("remainingAmount").is_some_and(|v| !v.is_null()) {
                    // An amount without a fraction has no denominator; stay unknown.
                    continue;
                }
                // proto3 JSON omits zero-valued floats: no fraction and no amount is exhausted.
                0.0
            }
        };
        if !(0.0..=1.0).contains(&fraction) {
            continue;
        }
        let Some(stamp) = parse_iso_utc_strict(&reset).map(|d| d.timestamp()) else {
            continue;
        };
        let display_model = safe_text(&Value::String(model_id.to_owned()), 160);
        collected.push((stamp, reset, display_model, fraction));
    }
    collected.sort_by_key(|entry| entry.0);
    let mut groups: Vec<TimestampedGeminiGroup> = Vec::new();
    for (stamp, reset, model_id, fraction) in collected {
        if let Some(last) = groups.last_mut()
            && ((stamp - last.0) as f64) <= GEMINI_RESET_CLUSTER_SECONDS
        {
            last.2.push((model_id, fraction));
        } else {
            groups.push((stamp, reset, vec![(model_id, fraction)]));
        }
    }
    groups
        .into_iter()
        .map(|(_, reset, members)| (reset, members))
        .collect()
}

/// Derive Gemini Models windows from `retrieveUserQuota` per-model buckets
/// (`desktop_usage.normalize_antigravity_gemini`).
pub fn normalize_antigravity_gemini(payload: &Value, summary_windows: &[Value]) -> Vec<Value> {
    if !payload.is_object() || antigravity_summary_has_gemini(summary_windows) {
        return Vec::new();
    }
    let anchors: Vec<(Option<&str>, Option<f64>)> = summary_windows
        .iter()
        .filter_map(|w| {
            let obj = w.as_object()?;
            Some((
                obj.get("resetAt").and_then(Value::as_str),
                obj.get("windowMinutes").and_then(Value::as_f64),
            ))
        })
        .collect();
    let mut seen_base_keys: BTreeMap<String, usize> = BTreeMap::new();
    let mut windows = Vec::new();
    for (reset, members) in antigravity_gemini_groups(payload.get("buckets"))
        .into_iter()
        .take(4)
    {
        let matched = antigravity_gemini_period(&reset, &anchors);
        let period = matched.map(|(p, _)| p);
        let minutes = matched.map(|(_, m)| m);
        let base = format!("gemini-{}", period.unwrap_or("window"));
        let count = seen_base_keys.entry(base.clone()).or_insert(0);
        *count += 1;
        let key = if *count == 1 {
            base
        } else {
            format!("{base}-{count}")
        };
        let label = match period {
            Some("5h") => "Gemini Models · 5-hour",
            Some("weekly") => "Gemini Models · Weekly",
            _ => "Gemini Models",
        };
        let min_fraction = members
            .iter()
            .map(|(_, f)| *f)
            .fold(f64::INFINITY, f64::min);
        let reset_val = Value::String(reset);
        let mut window = quota_window(WindowSpec {
            key: &key,
            label,
            remaining_percent: Some(min_fraction * 100.0),
            reset: Some(&reset_val),
            minutes,
            kind: "rate_limit",
            ..Default::default()
        });
        let model_ids: Vec<String> = members
            .into_iter()
            .filter_map(|(m, _)| m)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .take(64)
            .collect();
        if !model_ids.is_empty()
            && let Some(obj) = window.as_object_mut()
        {
            obj.insert("modelIds".into(), json!(model_ids));
        }
        windows.push(window);
    }
    windows
}

/// Refresh an Antigravity OAuth access token in memory (`desktop_usage.refresh_antigravity`).
pub async fn refresh_antigravity<'a>(
    credential: &Credential,
    home: &Path,
    transport: &dyn DesktopTransport,
    windows_acl: Option<&'a WindowsAclCheck<'a>>,
) -> Result<Credential, UsageError> {
    if credential.refresh.is_empty() {
        return Err(UsageError::new(
            "needs_sign_in",
            "The saved Antigravity account needs a renewed sign-in.",
        ));
    }
    let (client_id, client_secret) =
        antigravity_oauth_client_with(home, windows_acl).map_err(from_internal_err)?;
    let body = json!({
        "grant_type": "refresh_token",
        "refresh_token": credential.refresh,
        "client_id": client_id,
        "client_secret": client_secret,
    });
    let payload = transport
        .request_json(GOOGLE_TOKEN_URL, &[], Some(&body), true)
        .await?;
    let Some(token) = payload.get("access_token").and_then(secret) else {
        return Err(UsageError::new(
            "needs_sign_in",
            "The saved Antigravity account could not renew its session.",
        ));
    };
    Ok(Credential {
        access: token,
        source: credential.source.clone(),
        refresh: credential.refresh.clone(),
        expires: None,
        email: credential.email.clone(),
        project: credential.project.clone(),
        plan: credential.plan.clone(),
        google_client: credential.google_client.clone(),
    })
}

/// `desktop_usage.antigravity_load`: loadCodeAssist from app service, falling back to CLI service.
pub async fn antigravity_load(
    transport: &dyn DesktopTransport,
    headers: &[(String, String)],
) -> Result<Value, UsageError> {
    let body = json!({
        "metadata": {
            "ideType": "ANTIGRAVITY",
            "platform": "PLATFORM_UNSPECIFIED",
            "pluginType": "GEMINI"
        }
    });
    match transport
        .request_json(
            &format!("{AGY_BASE}loadCodeAssist"),
            headers,
            Some(&body),
            false,
        )
        .await
    {
        Ok(v) => Ok(v),
        Err(err) if err.status == "needs_sign_in" => Err(err),
        Err(_) => {
            transport
                .request_json(
                    &format!("{AGY_CLI_BASE}loadCodeAssist"),
                    headers,
                    Some(&body),
                    false,
                )
                .await
        }
    }
}

fn antigravity_rate_limited(error: &UsageError) -> bool {
    error.status == "rate_limited" || error.failure_code == Some("rate_limited")
}

fn antigravity_retry_after(error: &UsageError) -> Option<u64> {
    error.retry_after.filter(|&v| v > 0 && v <= 86_400)
}

/// Read both Antigravity quota services (`desktop_usage.antigravity_services`), CLI first then app,
/// with required summary deadline (14 s) and optional per-model fallback deadline (13 s inclusive).
pub async fn antigravity_services<'a>(
    transport: &dyn DesktopTransport,
    headers: &[(String, String)],
    project: &str,
    elapsed: &'a ElapsedClock<'a>,
    per_model: bool,
    services_override: Option<&[(&'static str, &'static str)]>,
) -> Result<Vec<AntigravityServiceResult>, UsageError> {
    let services = services_override.unwrap_or(&DEFAULT_AGY_SERVICES);
    let body = json!({ "project": project });
    let mut results: Vec<AntigravityServiceResult> = Vec::with_capacity(services.len());
    let mut failures: Vec<UsageError> = Vec::new();

    for (index, &(service, base)) in services.iter().enumerate() {
        let mut entry = AntigravityServiceResult {
            service,
            base,
            status: "unavailable",
            groups: Vec::new(),
            windows: Vec::new(),
            retry_after: None,
            error: None,
        };
        if index > 0 && elapsed() > AGY_REQUIRED_BUDGET_SECONDS {
            results.push(entry);
            continue;
        }
        let url = format!("{base}retrieveUserQuotaSummary");
        match transport
            .request_json(&url, headers, Some(&body), false)
            .await
        {
            Ok(summary) => {
                entry.status = "ok";
                entry.groups = agy_groups(&summary);
                entry.windows = normalize_antigravity(&summary);
                results.push(entry);
            }
            Err(error) => {
                let is_rl = antigravity_rate_limited(&error);
                let retry = antigravity_retry_after(&error);
                failures.push(error.clone());
                entry.error = Some(error.clone());
                if is_rl {
                    if !results.iter().any(|r| r.status == "ok") {
                        return Err(error);
                    }
                    entry.status = "rate_limited";
                    entry.retry_after = retry;
                    results.push(entry);
                    break;
                }
                results.push(entry);
            }
        }
    }

    let first_ok_idx = results.iter().position(|r| r.status == "ok");
    let Some(ok_idx) = first_ok_idx else {
        if !failures.is_empty() && failures.iter().all(|e| e.status == "needs_sign_in") {
            return Err(failures.into_iter().next().unwrap_or_else(|| {
                UsageError::new(
                    "needs_sign_in",
                    "The saved account could not authenticate with the usage service.",
                )
            }));
        }
        return Err(failures.into_iter().last().unwrap_or_else(|| {
            UsageError::new(
                "unavailable",
                "There was no time left to read Antigravity's quota.",
            )
        }));
    };

    let known = results[ok_idx].windows.clone();
    for res in &mut results {
        if res.status != "ok" {
            res.windows = antigravity_unavailable_windows(&known);
        }
    }

    if per_model {
        for res in &mut results {
            if res.status != "ok" {
                continue;
            }
            if antigravity_summary_has_gemini(&res.windows)
                || elapsed() > AGY_OPTIONAL_BUDGET_SECONDS
            {
                continue;
            }
            let url = format!("{}retrieveUserQuota", res.base);
            match transport
                .request_json(&url, headers, Some(&body), false)
                .await
            {
                Ok(per_model_reply) => {
                    let derived = normalize_antigravity_gemini(&per_model_reply, &res.windows);
                    res.windows.extend(derived);
                }
                Err(error) => {
                    res.retry_after = res.retry_after.or_else(|| antigravity_retry_after(&error));
                }
            }
        }
    }

    Ok(results)
}

async fn fetch_antigravity_quota_with_budget<'a>(
    credential: &Credential,
    result: &mut Value,
    transport: &dyn DesktopTransport,
    elapsed: &'a ElapsedClock<'a>,
    services_override: Option<&[(&'static str, &'static str)]>,
) -> Result<(), UsageError> {
    let headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credential.access),
        ),
        ("User-Agent".to_owned(), AGY_USER_AGENT.to_owned()),
    ];
    let load = antigravity_load(transport, &headers).await?;
    let project_raw = match load.get("cloudaicompanionProject") {
        Some(Value::Object(obj)) => obj.get("id").or_else(|| obj.get("projectId")),
        other => other,
    };
    let project = project_raw
        .and_then(|v| safe_text(v, 160))
        .or_else(|| credential.project.clone())
        .ok_or_else(|| {
            UsageError::new(
                "unavailable",
                "The signed-in account did not return an Antigravity quota project.",
            )
        })?;
    let (plan, reported) = antigravity_reported_plan(&load);
    result["plan"] = json!(plan);
    if let Some(rep) = reported {
        result["reportedPlan"] = rep;
    }
    let services = antigravity_services(
        transport,
        &headers,
        &project,
        elapsed,
        true,
        services_override,
    )
    .await?;
    let mut windows = Vec::new();
    for svc in services {
        windows.extend(antigravity_service_label(svc.windows, svc.service));
        if let Some(retry) = svc.retry_after
            && result.get("retryAfterSeconds").is_none()
        {
            result["retryAfterSeconds"] = json!(retry);
        }
    }
    windows.extend(normalize_antigravity_credits(&load));
    result["windows"] = Value::Array(windows);
    Ok(())
}

/// Fetch Antigravity quota using the provided access token (`desktop_usage.fetch_antigravity_quota`).
pub async fn fetch_antigravity_quota(
    credential: &Credential,
    result: &mut Value,
    env: &DesktopEnv<'_>,
) -> Result<(), UsageError> {
    let owned_transport;
    let transport: &dyn DesktopTransport = match env.desktop_transport {
        Some(t) => t,
        None => {
            owned_transport = ReqwestDesktopTransport::new(env.now_secs)?;
            &owned_transport
        }
    };
    let started = Instant::now();
    let default_elapsed = || started.elapsed().as_secs_f64();
    let elapsed: &ElapsedClock<'_> = env.elapsed.unwrap_or(&default_elapsed);
    fetch_antigravity_quota_with_budget(credential, result, transport, elapsed, env.agy_services)
        .await
}

/// Fetch Antigravity quota with 30 s proactive expiry refresh and at-most-once 401 retry
/// (`desktop_usage.fetch_antigravity`).
pub async fn fetch_antigravity(
    credential: &Credential,
    result: &mut Value,
    home: &Path,
    env: &DesktopEnv<'_>,
) -> Result<(), UsageError> {
    let owned_transport;
    let transport: &dyn DesktopTransport = match env.desktop_transport {
        Some(t) => t,
        None => {
            owned_transport = ReqwestDesktopTransport::new(env.now_secs)?;
            &owned_transport
        }
    };
    let started = Instant::now();
    let default_elapsed = || started.elapsed().as_secs_f64();
    let elapsed: &ElapsedClock<'_> = env.elapsed.unwrap_or(&default_elapsed);
    let now_ms = now_unix_secs(env.now_secs) * 1000.0;
    let mut current = credential.clone();
    let mut refreshed = false;
    if let Some(exp) = current.expires
        && exp <= now_ms + 30_000.0
    {
        current = refresh_antigravity(&current, home, transport, env.windows_acl).await?;
        refreshed = true;
    }
    match fetch_antigravity_quota_with_budget(
        &current,
        result,
        transport,
        elapsed,
        env.agy_services,
    )
    .await
    {
        Ok(()) => Ok(()),
        Err(error)
            if error.status == "needs_sign_in" && !refreshed && !current.refresh.is_empty() =>
        {
            let renewed = refresh_antigravity(&current, home, transport, env.windows_acl).await?;
            fetch_antigravity_quota_with_budget(
                &renewed,
                result,
                transport,
                elapsed,
                env.agy_services,
            )
            .await
        }
        Err(error) => Err(error),
    }
}
