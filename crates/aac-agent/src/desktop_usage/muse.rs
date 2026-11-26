//! Muse standalone discovery and collection ported from `scripts/account-usage/desktop_usage.py`.
use crate::desktop_usage::{
    Credential, DesktopEnv, DesktopTransport, MUSE_URL, ReqwestDesktopTransport,
    helpers::from_internal_cred,
};
use crate::failover_additional::{
    PortalTransport,
    desktop::{
        self, ReadTracker, config_home, credential_from_omp, normalize_muse, read_json_file,
        safe_text, secret,
    },
    muse::{self, MuseError, ReqwestPortalTransport},
    sqlite,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

/// Fixed user-facing error messages matching `muse_console.ERROR_MESSAGES`.
pub(crate) fn muse_error_message(code: &str) -> &'static str {
    match code {
        "no_browser_cookie" => {
            "No existing Muse web sign-in was found. Open dev.meta.ai in this browser and sync Muse again."
        }
        "needs_sign_in" => {
            "The existing Muse web session expired. Open dev.meta.ai in this browser and sync Muse again."
        }
        "account_mismatch" => "The Muse web account differs from the signed-in Muse CLI account.",
        "identity_unavailable" => {
            "Muse is not returning the signed-in web account email right now. Usage will refresh automatically when it is available."
        }
        "plan_mismatch" => {
            "The selected web team's subscription differs from the Muse CLI subscription."
        }
        "choose_team" => {
            "This Muse account has several teams. Select the team that owns your coding subscription."
        }
        "team_mismatch" => "The selected Muse team is not available to this signed-in account.",
        "inactive_subscription" => "This account has no active Muse Code subscription.",
        "network_error" => "The Muse web usage request did not finish. Try again later.",
        "provider_error" => "Muse could not return its web usage right now.",
        "rate_limited" => {
            "Muse is limiting usage requests. Usage will refresh automatically after a cooldown."
        }
        "invalid_response" => "Muse returned no usable rolling or weekly quota.",
        "invalid_request" => "The Muse browser usage request was invalid.",
        "local_storage_error" => "The private Muse web session could not be read or saved.",
        "host_unavailable" => "The Mac browser usage helper could not start.",
        "busy" => "A Muse usage refresh is already running.",
        _ => "Muse web usage could not be read.",
    }
}

/// Discover Muse credentials from `config_home/muse/auth.json` or OMP `agent.db`
/// (`desktop_usage.muse_credentials`).
pub fn muse_credentials(
    home: &Path,
    env_vars: Option<&BTreeMap<String, String>>,
    tracker: &mut ReadTracker,
) -> Option<Credential> {
    let path = config_home(home, env_vars).join("muse/auth.json");
    if let Some(value) = read_json_file(&path, tracker)
        && let Some(meta) = value
            .get("providers")
            .and_then(|p| p.get("meta"))
            .and_then(Value::as_object)
        && let Some(token) = meta
            .get("access_token")
            .and_then(secret)
            .filter(|t| t.starts_with("dca:"))
    {
        return Some(Credential {
            access: token,
            source: "Muse CLI".to_owned(),
            refresh: String::new(),
            expires: None,
            email: meta.get("user_email").and_then(desktop::email),
            project: None,
            plan: None,
            google_client: None,
        });
    }
    for row in sqlite::omp_desktop_oauth_rows(home, "muse-code", tracker) {
        if let Some(cred) = credential_from_omp("muse", &row) {
            return Some(from_internal_cred(cred));
        }
    }
    None
}

fn mark_failed(result: &mut Value, code: &str, message: &str) {
    result["status"] = json!(if code == "needs_sign_in" {
        "needs_sign_in"
    } else {
        "unavailable"
    });
    result["failureCode"] = json!(code);
    result["message"] = json!(message);
    result["windows"] = json!([]);
    result["sampledAt"] = Value::Null;
}

async fn verify_device_identity(
    credential: &Credential,
    expected_email: &str,
    transport: &dyn DesktopTransport,
) -> Result<String, MuseError> {
    let headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credential.access),
        ),
        ("x-api-version".to_owned(), "1.0.0".to_owned()),
    ];
    let payload = transport
        .request_json(MUSE_URL, &headers, Some(&json!({})), false)
        .await
        .map_err(|err| {
            if err.status == "needs_sign_in" {
                MuseError::new("needs_sign_in")
            } else if err.failure_code == Some("rate_limited") {
                MuseError::with_retry("rate_limited", err.retry_after.map(|r| r as f64))
            } else {
                MuseError::new("provider_error")
            }
        })?;
    let Some(address) = payload.get("user_email").and_then(desktop::email) else {
        return Err(MuseError::new("account_mismatch"));
    };
    if !address.eq_ignore_ascii_case(expected_email) {
        return Err(MuseError::new("account_mismatch"));
    }
    if payload.get("is_subs_active") == Some(&Value::Bool(false)) {
        return Err(MuseError::new("inactive_subscription"));
    }
    let Some(plan) = payload
        .get("subs_tier_name")
        .and_then(|v| safe_text(v, 160))
    else {
        return Err(MuseError::new("invalid_response"));
    };
    Ok(plan)
}

fn append_web_source(result: &mut Value) {
    let current = result
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned();
    result["source"] = json!(format!("{current} + authenticated Meta web quota"));
}

/// Fetch standalone Muse usage (`desktop_usage.fetch_muse`).
pub async fn fetch_muse(
    credential: &Credential,
    result: &mut Value,
    home: &Path,
    env: &DesktopEnv<'_>,
) {
    let root = home.join(".ccs/account-usage");
    let owned_transport;
    let transport: &dyn DesktopTransport = match env.desktop_transport {
        Some(t) => t,
        None => match ReqwestDesktopTransport::new(env.now_secs) {
            Ok(t) => {
                owned_transport = t;
                &owned_transport
            }
            Err(err) => {
                mark_failed(result, "error", err.message);
                return;
            }
        },
    };

    // 1. Capsule-first verified observation during shared cooldown (avoids minting an unused key).
    match muse::read_capsule(&root, false, env.now_secs) {
        Ok((cookies, team, bound_email, bound_plan)) => {
            if credential
                .email
                .as_deref()
                .is_some_and(|e| e.eq_ignore_ascii_case(&bound_email))
            {
                result["email"] = json!(bound_email);
                result["plan"] = json!(bound_plan);
                let owned_portal;
                let portal_opt: Result<&dyn PortalTransport, MuseError> = match env.portal_transport
                {
                    Some(p) => Ok(p),
                    None => match ReqwestPortalTransport::new(&cookies, env.now_secs) {
                        Ok(p) => {
                            owned_portal = p;
                            Ok(&owned_portal)
                        }
                        Err(e) => Err(e),
                    },
                };
                match portal_opt {
                    Ok(portal) => {
                        let email_copy = bound_email.clone();
                        match muse::quota_sample(
                            &root,
                            &cookies,
                            &bound_email,
                            bound_plan,
                            Some(&team),
                            &credential.access,
                            portal,
                            env.now_secs,
                            Some(&team),
                            Some(|| verify_device_identity(credential, &email_copy, transport)),
                        )
                        .await
                        {
                            Ok(cached) => {
                                result["email"] = json!(bound_email);
                                result["plan"] = cached["quota"]["tier"].clone();
                                result["status"] =
                                    json!(if cached["cached"].as_bool() == Some(true) {
                                        "cached"
                                    } else {
                                        "ok"
                                    });
                                result["message"] = cached["message"].clone();
                                result["sampledAt"] = cached["sampledAt"].clone();
                                result["windows"] = Value::Array(normalize_muse(&json!({
                                    "subscription_quota": cached["quota"]
                                })));
                                append_web_source(result);
                                return;
                            }
                            Err(err) => {
                                if !matches!(err.code, "no_browser_cookie" | "local_storage_error")
                                {
                                    if let Some(plan) = err.plan {
                                        result["plan"] = json!(plan);
                                    }
                                    mark_failed(result, err.code, muse_error_message(err.code));
                                    return;
                                }
                            }
                        }
                    }
                    Err(err) => {
                        if !matches!(err.code, "no_browser_cookie" | "local_storage_error") {
                            if let Some(plan) = err.plan {
                                result["plan"] = json!(plan);
                            }
                            mark_failed(result, err.code, muse_error_message(err.code));
                            return;
                        }
                    }
                }
            }
        }
        Err(err) => {
            if !matches!(err.code, "no_browser_cookie" | "local_storage_error") {
                if let Some(plan) = err.plan {
                    result["plan"] = json!(plan);
                }
                mark_failed(result, err.code, muse_error_message(err.code));
                return;
            }
        }
    }

    // 2. Native Muse key endpoint.
    let headers = vec![
        (
            "Authorization".to_owned(),
            format!("Bearer {}", credential.access),
        ),
        ("x-api-version".to_owned(), "1.0.0".to_owned()),
    ];
    let payload = match transport
        .request_json(MUSE_URL, &headers, Some(&json!({})), false)
        .await
    {
        Ok(v) => v,
        Err(err) => {
            let code = if err.status == "needs_sign_in" {
                "needs_sign_in"
            } else {
                "error"
            };
            mark_failed(result, code, err.message);
            return;
        }
    };
    let resolved_email = payload
        .get("user_email")
        .and_then(desktop::email)
        .or_else(|| credential.email.clone());
    let resolved_plan = payload
        .get("subs_tier_name")
        .and_then(|v| safe_text(v, 160));
    result["email"] = json!(resolved_email);
    result["plan"] = json!(resolved_plan);
    result["windows"] = Value::Array(normalize_muse(&payload));

    if let (Some(cred_email), Some(resp_email)) = (&credential.email, &resolved_email)
        && !cred_email.eq_ignore_ascii_case(resp_email)
    {
        mark_failed(
            result,
            "account_mismatch",
            "The Muse usage account differs from the signed-in Muse CLI account.",
        );
        return;
    }
    if payload.get("is_subs_active") == Some(&Value::Bool(false)) {
        mark_failed(
            result,
            "inactive_subscription",
            "This account has no active Muse Code subscription.",
        );
        return;
    }
    if result["windows"].as_array().is_some_and(|w| !w.is_empty()) {
        return;
    }

    // 3. Omitted subs_usage fallback to authenticated Meta web quota capsule.
    let omitted_msg = "Muse confirmed this subscription but omitted its usage. Sync Muse in the Mac browser usage bridge to read the rolling and weekly limits.";
    result["status"] = json!("unavailable");
    result["message"] = json!(omitted_msg);
    result["failureCode"] = json!("invalid_response");

    let (cookies, team, bound_email, _bound_plan) =
        match muse::read_capsule(&root, false, env.now_secs) {
            Ok(tuple) => tuple,
            Err(err) => {
                if let Some(plan) = err.plan {
                    result["plan"] = json!(plan);
                }
                let msg = if err.code == "no_browser_cookie" {
                    omitted_msg
                } else {
                    muse_error_message(err.code)
                };
                mark_failed(result, err.code, msg);
                return;
            }
        };
    let (Some(ref res_email), Some(ref res_plan)) = (resolved_email, resolved_plan) else {
        mark_failed(
            result,
            "account_mismatch",
            muse_error_message("account_mismatch"),
        );
        return;
    };
    if !bound_email.eq_ignore_ascii_case(res_email) {
        mark_failed(
            result,
            "account_mismatch",
            muse_error_message("account_mismatch"),
        );
        return;
    }
    let owned_portal;
    let portal: &dyn PortalTransport = match env.portal_transport {
        Some(p) => p,
        None => match ReqwestPortalTransport::new(&cookies, env.now_secs) {
            Ok(p) => {
                owned_portal = p;
                &owned_portal
            }
            Err(err) => {
                if let Some(plan) = err.plan {
                    result["plan"] = json!(plan);
                }
                let msg = if err.code == "no_browser_cookie" {
                    omitted_msg
                } else {
                    muse_error_message(err.code)
                };
                mark_failed(result, err.code, msg);
                return;
            }
        },
    };
    type IdentityFuture = std::future::Ready<Result<String, MuseError>>;
    type NoIdentityLoader = fn() -> IdentityFuture;
    let no_loader: Option<NoIdentityLoader> = None;
    match muse::quota_sample(
        &root,
        &cookies,
        res_email,
        res_plan.clone(),
        Some(&team),
        &credential.access,
        portal,
        env.now_secs,
        Some(&team),
        no_loader,
    )
    .await
    {
        Ok(sample) => {
            let windows = normalize_muse(&json!({ "subscription_quota": sample["quota"] }));
            if !windows.is_empty() {
                result["windows"] = Value::Array(windows);
                result["status"] = json!(if sample["cached"].as_bool() == Some(true) {
                    "cached"
                } else {
                    "ok"
                });
                result["message"] = sample["message"].clone();
                result["sampledAt"] = sample["sampledAt"].clone();
                if let Some(obj) = result.as_object_mut() {
                    obj.remove("failureCode");
                }
                append_web_source(result);
            }
        }
        Err(err) => {
            if let Some(plan) = err.plan {
                result["plan"] = json!(plan);
            }
            let msg = if err.code == "no_browser_cookie" {
                omitted_msg
            } else {
                muse_error_message(err.code)
            };
            mark_failed(result, err.code, msg);
        }
    }
}
