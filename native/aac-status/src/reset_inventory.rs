//! Private normalization only. Never add these IDs to the legacy exported balance rows.
use serde::{Deserialize, Serialize};
pub type Result<T> = std::result::Result<T, &'static str>;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Identity {
    pub provider: String,
    pub account: String,
    pub organization: String,
    pub profile: String,
    pub host: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Grant {
    pub id: Option<String>,
    pub kind: String,
    pub clears: Vec<String>,
    pub left: u32,
    pub expires_at_ms: Option<i64>,
    pub usable: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Snapshot {
    pub identity: Identity,
    pub observed_at_ms: i64,
    pub grants: Vec<Grant>,
    pub windows: std::collections::BTreeMap<String, f64>,
    /// False for Mac-only login, missing exact profile binding or an unproven host executor.
    pub executor_ready: bool,
    pub source_binding: String,
    #[serde(default)]
    pub account_label: String,
}

use serde_json::Value;
fn count(v: &Value) -> Result<u32> {
    v.as_u64().filter(|n| *n <= 1000).map(|n| n as u32).ok_or("shape")
}
fn id(v: &Value) -> Result<Option<String>> {
    if v.is_null() {
        return Ok(None);
    }
    let s = v.as_str().ok_or("shape")?;
    if s.is_empty() || s.len() > 256 || !s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
        return Err("shape");
    }
    Ok(Some(s.to_owned()))
}
/// Expiry values must be normalized by the existing collector's date parser; raw provider
/// strings are NOT accepted here. Missing expiry remains unknown, never an invented date.
pub fn claude(cedar: &Value, expiries: &[Option<i64>]) -> Result<Vec<Grant>> {
    if cedar["eligible"] != true {
        return Err("inventory-unavailable");
    }
    let rows = cedar["grants"].as_array().ok_or("shape")?;
    if rows.len() > 256 || expiries.len() != rows.len() {
        return Err("shape");
    }
    rows.iter()
        .enumerate()
        .map(|(i, g)| {
            let left = count(&g["resets_left"])?;
            if !g["resets_total"].is_null() && count(&g["resets_total"])? < left {
                return Err("shape");
            }
            let paused = g["paused"].as_bool().ok_or("shape")?;
            let usable = g["usable_now"].as_bool().unwrap_or(false);
            let clears = g["clears"]
                .as_array()
                .ok_or("shape")?
                .iter()
                .map(|v| {
                    let s = v.as_str().ok_or("shape")?;
                    if s.len() > 64
                        || s.is_empty()
                        || !s
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
                    {
                        return Err("shape");
                    }
                    Ok(s.to_owned())
                })
                .collect::<Result<Vec<_>>>()?;
            if clears.len() > 16 {
                return Err("shape");
            }
            let known = clears
                .iter()
                .all(|s| matches!(s.as_str(), "five_hour" | "seven_day" | "seven_day_overage_included"));
            if !g["type"].is_null() && !g["type"].is_string() {
                return Err("shape");
            }
            if !g["gives_back_weekly"].is_null() && !g["gives_back_weekly"].is_boolean() {
                return Err("shape");
            }
            let explicit = g["type"].as_str();
            let kind = if !known {
                "unknown"
            } else if g["gives_back_weekly"] == true {
                "fable"
            } else if let Some(t) = explicit {
                match t {
                    "session" | "weekly" | "full" => t,
                    _ => "unknown",
                }
            } else if clears.is_empty() {
                "unknown"
            } else {
                let session = clears.iter().any(|s| s == "five_hour");
                let week = clears.iter().any(|s| s == "seven_day");
                if session && week {
                    "full"
                } else if session {
                    "session"
                } else if week {
                    "weekly"
                } else {
                    "unknown"
                }
            };
            let session = clears.iter().any(|k| k == "five_hour");
            let weekly = clears.iter().any(|k| k == "seven_day");
            let kind = match kind {
                "session" if !session || weekly => "unknown",
                "weekly" if !weekly || session => "unknown",
                "full" if !session || !weekly => "unknown",
                other => other,
            };
            Ok(Grant {
                id: id(&g["id"])?,
                kind: kind.to_owned(),
                clears,
                left,
                expires_at_ms: expiries[i],
                usable: usable && !paused,
            })
        })
        .collect()
}
/// Exact-ID details from app-server or wham. No aggregate count can stand in for a grant.
/// `known_type` must come from the pinned provider schema, not the response being parsed.
pub fn codex(details: &Value, expiries: &[Option<i64>], known_type: &str, clears: &[String]) -> Result<Vec<Grant>> {
    let rows = details["credits"].as_array().ok_or("inventory-unavailable")?;
    if rows.len() > 256 || rows.len() != expiries.len() {
        return Err("shape");
    }
    rows.iter()
        .enumerate()
        .map(|(i, g)| {
            let status = g["status"].as_str().ok_or("shape")?;
            let ty = g["reset_type"].as_str().ok_or("shape")?;
            Ok(Grant {
                id: id(&g["id"])?,
                kind: if !known_type.is_empty() && ty == known_type {
                    "banked"
                } else {
                    "unknown"
                }
                .to_owned(),
                clears: clears.to_vec(),
                left: if status == "available" { 1 } else { 0 },
                expires_at_ms: expiries[i],
                usable: status == "available",
            })
        })
        .collect()
}

pub fn normalize_claude(args: &str) -> Result<Value> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Read {
        identity: Identity,
        observed_at_ms: i64,
        raw: Value,
        expiries: Vec<Option<i64>>,
        windows: std::collections::BTreeMap<String, f64>,
        source_binding: String,
        #[serde(default)]
        account_label: String,
    }
    let a: Read = serde_json::from_str(args).map_err(|_| "inventory-unavailable")?;
    let grants = claude(&a.raw, &a.expiries)?;
    Ok(serde_json::json!(Snapshot {
        identity: a.identity,
        observed_at_ms: a.observed_at_ms,
        grants,
        windows: a.windows,
        executor_ready: true,
        source_binding: a.source_binding,
        account_label: a.account_label
    }))
}
