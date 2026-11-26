//! Cursor standalone discovery and desktop collection wrapping `failover_additional::cursor`
//! (`scripts/account-usage/cursor_usage.py` and `desktop_usage.py`).
use crate::desktop_usage::{
    Credential, DesktopEnv, UsageError,
    helpers::{from_internal_cred, to_internal_cred},
};
use crate::failover_additional::{
    PlanTransport, cursor as shared_cursor,
    desktop::{
        self, ReadTracker, credential_from_omp, cursor_root, jwt_claims, number, read_json_file,
        safe_text, secret,
    },
    plans::ReqwestPlanTransport,
    sqlite,
};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};

/// Discover Cursor credentials from `state.vscdb`, `auth.json`, or OMP `agent.db`
/// (`desktop_usage.cursor_credentials`).
pub fn cursor_credentials(
    home: &Path,
    platform: &str,
    env_vars: Option<&BTreeMap<String, String>>,
    tracker: &mut ReadTracker,
) -> Option<Credential> {
    let root = cursor_root(home, platform, env_vars);
    let fields =
        sqlite::cursor_desktop_vscdb_rows(&root.join("User/globalStorage/state.vscdb"), tracker);
    let auth = read_json_file(&root.join("auth.json"), tracker).unwrap_or_else(|| json!({}));
    let token = fields
        .get("cursorAuth/accessToken")
        .and_then(secret)
        .or_else(|| auth.get("accessToken").and_then(secret));
    if let Some(access) = token {
        let claims = jwt_claims(&access);
        let expires = claims.get("exp").and_then(number).map(|e| e * 1000.0);
        let refresh = fields
            .get("cursorAuth/refreshToken")
            .and_then(secret)
            .or_else(|| auth.get("refreshToken").and_then(secret))
            .unwrap_or_default();
        let addr = claims.get("email").and_then(desktop::email).or_else(|| {
            fields
                .get("cursorAuth/cachedEmail")
                .and_then(desktop::email)
        });
        let plan = fields
            .get("cursorAuth/stripeMembershipType")
            .and_then(|v| safe_text(v, 160));
        return Some(Credential {
            access,
            source: "Cursor desktop".to_owned(),
            refresh,
            expires,
            email: addr,
            project: None,
            plan,
            google_client: None,
        });
    }
    for row in sqlite::omp_desktop_oauth_rows(home, "cursor", tracker) {
        if let Some(cred) = credential_from_omp("cursor", &row) {
            return Some(from_internal_cred(cred));
        }
    }
    None
}

/// Build the Cursor session cookie (`desktop_usage.cursor_cookie`).
pub fn cursor_cookie(credential: &Credential) -> Result<String, UsageError> {
    let internal = to_internal_cred(credential);
    shared_cursor::cookie(&internal).map_err(|err| UsageError::new(err.status, err.message))
}

/// Collect Cursor usage into a desktop account result (`desktop_usage.fetch_cursor`).
pub async fn fetch_cursor(credential: &Credential, result: &mut Value, env: &DesktopEnv<'_>) {
    let owned_client;
    let client: &dyn PlanTransport = match env.plan_transport {
        Some(c) => c,
        None => match ReqwestPlanTransport::new(16, env.now_secs) {
            Ok(c) => {
                owned_client = c;
                &owned_client
            }
            Err(error) => {
                apply_cursor_error(result, &error);
                return;
            }
        },
    };
    let internal = to_internal_cred(credential);
    match shared_cursor::collect(&internal, result, client, env.now_secs).await {
        Ok(meta) => {
            if let Some(obj) = result.as_object_mut() {
                obj.extend(meta);
            }
        }
        Err(error) => apply_cursor_error(result, &error),
    }
}

fn apply_cursor_error(result: &mut Value, error: &crate::failover_additional::CollectionError) {
    let status = if error.status == "needs_sign_in" {
        "needs_sign_in"
    } else {
        "unavailable"
    };
    let failure_code = error
        .failure_code
        .unwrap_or(if error.status == "needs_sign_in" {
            "needs_sign_in"
        } else {
            "provider_error"
        });
    result["status"] = json!(status);
    result["failureCode"] = json!(failure_code);
    result["message"] = json!(error.message);
    result["windows"] = json!([]);
    if let Some(retry) = error.retry_after {
        result["retryAfterSeconds"] = json!(retry);
    }
}
