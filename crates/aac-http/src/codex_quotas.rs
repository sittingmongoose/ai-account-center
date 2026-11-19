//! The Codex profile quota answer, shared by the public route and the private service.
//!
//! The saved registry and each login's display-safe validity are read here; the collector rows
//! are supplied by the caller (the public owner's fresh pass or the adopted service's held
//! generation), and the existing `aac_views::codex_profiles::quotas` port composes the reply.

use aac_renewal::jwt::{decode_id_token, has_structurally_valid_id_token};
use aac_renewal::registry::read_codex_profile_registry;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};

pub(crate) fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(PathBuf::from).unwrap_or_default()
}

struct AuthIdentity {
    valid: bool,
}

/// `decodeAccountIdentity` + the structural-token check, over the bounded read.
fn read_auth(file: &Path) -> AuthIdentity {
    let aac_renewal::family::AuthBytes::Bytes(bytes) = aac_renewal::family::read_auth_bytes(file)
    else {
        return AuthIdentity { valid: false };
    };
    let Some(parsed) = aac_renewal::family::parse_auth_json(&bytes) else {
        return AuthIdentity { valid: false };
    };
    let Some(token) = parsed
        .get("tokens")
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
    else {
        return AuthIdentity { valid: false };
    };
    let identity = decode_id_token(token);
    AuthIdentity {
        valid: identity.email.is_some()
            || identity.plan_type.is_some()
            || identity.account_id.is_some()
            || has_structurally_valid_id_token(token),
    }
}

/// The latest held Accounts rows that carry a saved Codex profile, one per name. The adopted
/// private service and the public cache read the same generations.
pub(crate) fn rows_from_cache(
    cache: &crate::account_usage::AccountUsageCache,
    now: i64,
) -> Vec<Value> {
    let Some(timestamp) = aac_num::ecma::to_iso_string(now) else {
        return Vec::new();
    };
    let mut seen = std::collections::HashSet::new();
    let mut rows = Vec::new();
    for dashboard in cache.hub_dashboards(&timestamp, now) {
        for row in dashboard["accounts"].as_array().into_iter().flatten() {
            let Some(name) = row["capabilities"]["codexProfile"].as_str() else {
                continue;
            };
            if seen.insert(name.to_owned()) {
                rows.push(row.clone());
            }
        }
    }
    rows
}

fn remember_instant(epochs: &mut Map<String, Value>, text: &str) {
    if let Some(ms) = aac_num::ecma::parse_instant(text) {
        epochs.insert(text.to_owned(), json!(ms));
    }
}

/// The renewal notes for the profiles the dashboard shows, read from the status store only.
fn renewal_entries(root: &Path, now: i64) -> Vec<Value> {
    let options = aac_renewal::renewal::CodexProfileRenewalOptions {
        env: aac_renewal::planner::CodexRenewalEnvironment {
            ccs_dir: root.to_path_buf(),
            codex_home: Some(home_dir().join(".codex")),
            home_dir: Some(home_dir()),
            ..Default::default()
        },
        now: std::sync::Arc::new(move || now),
        ..Default::default()
    };
    match aac_renewal::renewal::get_codex_profile_renewal_status(&options) {
        Ok(status) => status
            .profiles
            .iter()
            .map(|profile| {
                json!([
                    profile.name,
                    {"state": profile.state.word(), "message": profile.message}
                ])
            })
            .collect(),
        Err(()) => Vec::new(),
    }
}

fn empty_envelope(data: Value) -> aac_status::read_context::Envelope {
    serde_json::from_value(json!({
        "data": data,
        "presentation": {"dates": {}, "epochs": {}, "order": {}},
    }))
    .expect("empty presentation facts parse")
}

/// The saved profiles as the quota view reads them: name and login validity.
pub(crate) fn saved_profiles(root: &Path) -> Vec<Value> {
    let Ok((_, records)) = read_codex_profile_registry(root) else {
        return Vec::new();
    };
    records
        .iter()
        .map(|record| {
            let identity = read_auth(
                &root
                    .join("codex-instances")
                    .join(&record.name)
                    .join("auth.json"),
            );
            json!({"name": record.name, "authValid": identity.valid})
        })
        .collect()
}

/// The public quota answer from the saved profiles and the collector rows the caller holds.
pub(crate) fn answer(root: &Path, dashboard_rows: Vec<Value>, now: i64) -> Option<Value> {
    let profiles = saved_profiles(root);
    if profiles.is_empty() {
        return aac_views::codex_profiles::quotas(empty_envelope(json!({
            "profiles": [],
            "rows": [],
            "renewals": [],
            "now": now as f64,
        })))
        .ok();
    }
    let mut quota_rows = Vec::new();
    let mut epochs = Map::new();
    for profile in &profiles {
        let name = profile["name"].as_str().unwrap_or_default();
        let Some(row) = dashboard_rows.iter().find(|row| {
            row.get("capabilities")
                .and_then(|caps| caps.get("codexProfile"))
                .and_then(Value::as_str)
                == Some(name)
        }) else {
            continue;
        };
        let status = row["status"].as_str().unwrap_or("unavailable");
        let (quota_status, needs_reauth) = match status {
            "ok" | "cached" => ("ok", false),
            "needs_sign_in" | "reauth_required" => ("error", true),
            _ => ("error", false),
        };
        for field in ["fetchedAt", "sampledAt"] {
            if let Some(text) = row[field].as_str() {
                remember_instant(&mut epochs, text);
            }
        }
        let mut windows = Vec::new();
        for window in row["windows"].as_array().into_iter().flatten() {
            let key = window["key"].as_str().unwrap_or_default();
            let label = match key {
                "five_hour" => "5-hour limit",
                "seven_day" => "Weekly limit",
                _ => window["label"].as_str().unwrap_or("Usage"),
            };
            let reset = window["resetAt"].as_str();
            if let Some(text) = reset {
                remember_instant(&mut epochs, text);
            }
            windows.push(json!({
                "key": key,
                "label": label,
                "usedPercent": window["usedPercent"],
                "resetAt": reset,
            }));
        }
        quota_rows.push(json!({
            "profile": name,
            "quotaStatus": quota_status,
            "needsReauth": needs_reauth,
            "fetchedAt": row["fetchedAt"],
            "staleAsOf": row["sampledAt"],
            "quotaWindows": windows,
        }));
    }
    let renewals = renewal_entries(root, now);
    let data = json!({
        "profiles": profiles,
        "rows": quota_rows,
        "renewals": renewals,
        "now": now as f64,
    });
    let envelope = serde_json::from_value(json!({
        "data": data,
        "presentation": {"dates": {}, "epochs": epochs, "order": {}},
    }))
    .ok()?;
    aac_views::codex_profiles::quotas(envelope).ok()
}
