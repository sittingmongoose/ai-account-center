use super::super::storage::hex;
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(super) const CODEX: &str = "https://chatgpt.com/backend-api/wham/usage";
pub(super) const CLAUDE: &str = "https://api.anthropic.com/api/oauth/usage";
pub(super) const CREDITS: &str = "https://chatgpt.com/backend-api/wham/rate-limit-reset-credits";

pub(super) struct Account {
    pub(super) file: Value,
    pub(super) usage: Option<Value>,
}

fn date(value: &Value) -> Option<DateTime<Utc>> {
    let text = value.as_str()?;
    DateTime::parse_from_rfc3339(text)
        .map(|date| date.to_utc())
        .ok()
        .or_else(|| {
            chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d")
                .ok()?
                .and_hms_opt(0, 0, 0)
                .map(|date| date.and_utc())
        })
}

fn iso(value: &Value) -> Option<String> {
    date(value).map(|date| date.to_rfc3339_opts(SecondsFormat::Millis, true))
}

fn reading(window: &Value) -> Option<Value> {
    let percent = window["usedPercent"].as_f64()?;
    (percent >= 0.0
        && window["resetPassed"] != true
        && (window.get("kind").is_none() || window["kind"] == "rate_limit"))
        .then(|| window["usedPercent"].clone())
}

fn codex_window(window: &Value) -> Option<Value> {
    let mut value = json!({
        "used_percent": reading(window)?,
        "reset_at": date(&window["resetAt"]).map(|date|date.timestamp()).filter(|seconds|*seconds > 0),
    });
    if let Some(minutes) = window["windowMinutes"].as_f64().filter(|mins| *mins > 0.0) {
        value["limit_window_seconds"] = json!((minutes * 60.0).round() as u64);
    }
    Some(value)
}

fn claude_window(window: &Value) -> Option<Value> {
    Some(json!({"utilization":reading(window)?, "resets_at":iso(&window["resetAt"])}))
}

fn model_name(window: &Value) -> Option<String> {
    let slug = window["key"].as_str()?.strip_prefix("seven_day_")?;
    if slug.len() > 54
        || !slug
            .bytes()
            .next()
            .is_some_and(|ch| ch.is_ascii_digit() || ch.is_ascii_lowercase())
        || !slug
            .bytes()
            .all(|ch| ch == b'_' || ch.is_ascii_digit() || ch.is_ascii_lowercase())
    {
        return None;
    }
    let label = window["label"]
        .as_str()
        .and_then(|label| label.strip_prefix("Weekly ")?.strip_suffix(" usage"))
        .map(str::trim)
        .filter(|label| !label.is_empty());
    Some(label.map_or_else(
        || {
            slug.split('_')
                .filter(|part| !part.is_empty())
                .map(|part| format!("{}{}", part[..1].to_ascii_uppercase(), &part[1..]))
                .collect::<Vec<_>>()
                .join(" ")
        },
        str::to_owned,
    ))
}

fn usage(row: &Value, provider: &str) -> Option<Value> {
    let windows = row["windows"].as_array()?;
    let find = |key| windows.iter().find(|window| window["key"] == key);
    if provider == "codex" {
        let primary = find("five_hour").and_then(codex_window);
        let secondary = find("seven_day").and_then(codex_window);
        if primary.is_none() && secondary.is_none() {
            return None;
        }
        let mut result = json!({});
        if let Some(plan) = row["plan"].as_str().filter(|plan| {
            (1..=64).contains(&plan.len())
                && plan
                    .bytes()
                    .all(|ch| ch == b'_' || ch.is_ascii_digit() || ch.is_ascii_lowercase())
        }) {
            result["plan_type"] = json!(plan);
        }
        result["rate_limit"] = json!({"primary_window":primary,"secondary_window":secondary});
        return Some(result);
    }
    let five = find("five_hour").and_then(claude_window);
    let seven = find("seven_day").and_then(claude_window);
    let mut names = std::collections::HashSet::new();
    let limits: Vec<_> =
        windows
            .iter()
            .filter_map(|window| {
                let percent = reading(window)?;
                let name = model_name(window)?;
                names.insert(name.clone()).then(|| json!({
            "kind":"weekly_scoped", "percent":percent, "resets_at":iso(&window["resetAt"]),
            "scope":{"model":{"display_name":name}},
        }))
            })
            .collect();
    (five.is_some() || seven.is_some() || !limits.is_empty())
        .then(|| json!({"five_hour":five,"seven_day":seven,"limits":limits}))
}

fn sample(row: &Value) -> &Value {
    let sampled = &row["sampledAt"];
    if sampled.is_null() {
        &row["fetchedAt"]
    } else {
        sampled
    }
}

fn account(row: &Value, id: &str, provider: &str) -> Account {
    let usage = usage(row, provider);
    let index = Sha256::digest([b"aac-usage-hub\0".as_slice(), id.as_bytes()].concat());
    let label = row["label"]
        .as_str()
        .filter(|text| !text.is_empty())
        .unwrap_or(id);
    let mut file = json!({
        "id":id,"auth_index":hex(&index[..8]),"provider":provider,
        "type":provider,"label":label,
    });
    if let Some(email) = row["email"].as_str().filter(|text| !text.is_empty()) {
        file["email"] = json!(email);
    }
    file["disabled"] = json!(false);
    file["status"] = json!(if usage.is_some() { "active" } else { "error" });
    file["status_message"] = json!(if usage.is_some() {
        "Cached AI Account Center reading."
    } else if row["status"] == "needs_sign_in" {
        "This account needs to sign in again in AI Account Center."
    } else {
        "AI Account Center has no current reading for this account."
    });
    file["sampled_at"] = json!(usage.as_ref().and_then(|_| iso(sample(row))));
    Account { file, usage }
}

pub(super) fn project(dashboards: &[Value]) -> Vec<Account> {
    let mut index = HashMap::<&str, usize>::new();
    let mut ordered = Vec::<(Option<DateTime<Utc>>, Account)>::new();
    for row in dashboards
        .iter()
        .filter_map(|dashboard| dashboard["accounts"].as_array())
        .flatten()
    {
        let (Some(id), Some(provider @ ("codex" | "claude"))) =
            (row["id"].as_str(), row["provider"].as_str())
        else {
            continue;
        };
        if row["lifecycle"]["state"] == "pending_sign_in" {
            continue;
        }
        let candidate = (date(sample(row)), account(row, id, provider));
        if let Some(previous) = index.get(id).and_then(|idx| ordered.get_mut(*idx)) {
            if (candidate.1.usage.is_some(), candidate.0) > (previous.1.usage.is_some(), previous.0)
            {
                *previous = candidate;
            }
        } else {
            index.insert(id, ordered.len());
            ordered.push(candidate);
        }
    }
    ordered.sort_by_key(|(_, account)| account.file["provider"] != "codex");
    ordered.into_iter().map(|(_, account)| account).collect()
}
