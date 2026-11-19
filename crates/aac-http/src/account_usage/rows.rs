//! Public quota rows from the native agent and the packaged provider helper.
use super::{Desktop, sources::Source};
use aac_views::antigravity::display_text;
use serde_json::{Value, json};

pub(super) fn capabilities() -> Value {
    json!({"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]})
}

pub(super) fn stamp(value: &Value) -> Option<String> {
    let text = value.as_str()?;
    if text.len() > 40 || text.as_bytes().get(10) != Some(&b'T') {
        return None;
    }
    let ms = aac_views::js::parse_date(text).ok()??;
    let iso = aac_num::ecma::to_iso_string(ms)?;
    (("2000"..="2200").contains(&&iso[..4])).then_some(iso)
}

pub(super) fn email(value: &Value) -> Option<String> {
    let text = display_text(Some(value), 254)?;
    let (local, domain) = text.split_once('@')?;
    let (name, suffix) = domain.rsplit_once('.')?;
    (!local.is_empty()
        && !name.is_empty()
        && !suffix.is_empty()
        && !domain.contains('@')
        && !text.chars().any(char::is_whitespace))
    .then_some(text)
}

fn number(value: &Value) -> Option<f64> {
    value.as_f64().filter(|n| n.is_finite() && *n >= 0.0)
}

pub(super) fn windows(value: &Value, limit: usize) -> Vec<Value> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .take(limit)
        .enumerate()
        .filter_map(|(index, window)| {
            let object = window.as_object()?;
            let used_pct = number(&window["usedPercent"]);
            let remaining_pct = number(&window["remainingPercent"])
                .filter(|n| *n <= 100.0)
                .or_else(|| used_pct.map(|n| (100.0 - n).max(0.0)));
            let used = number(&window["used"]);
            let limit = number(&window["limit"]);
            let remaining = window["remaining"].as_f64().filter(|n| n.is_finite());
            let reset = stamp(&window["resetAt"]);
            let expires = stamp(&window["expiresAt"]);
            if used_pct.is_none()
                && remaining_pct.is_none()
                && used.is_none()
                && limit.is_none()
                && remaining.is_none()
                && reset.is_none()
                && expires.is_none()
                && !window["unlimited"].is_boolean()
                && !window["enabled"].is_boolean()
            {
                return None;
            }
            let key = display_text(Some(&window["key"]), 64)
                .filter(|key| {
                    key.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                })
                .unwrap_or_else(|| format!("usage-{}", index + 1));
            let mut row = json!({
                "key":key,
                "label":display_text(Some(&window["label"]),80).as_deref().unwrap_or("Usage"),
                "usedPercent":used_pct,"remainingPercent":remaining_pct,"resetAt":reset,
                "windowMinutes":number(&window["windowMinutes"]),"used":used,"limit":limit,
                "unit":display_text(Some(&window["unit"]),24)
            });
            if let Some(kind) = window["kind"]
                .as_str()
                .filter(|kind| ["rate_limit", "balance", "spend", "extra_usage"].contains(kind))
            {
                row["kind"] = json!(kind);
            }
            for (key, value) in [
                ("remaining", json!(remaining)),
                ("expiresAt", json!(expires)),
                ("unlimited", window["unlimited"].clone()),
                ("enabled", window["enabled"].clone()),
            ] {
                if object.contains_key(key)
                    && (key == "remaining" || key == "expiresAt" || value.is_boolean())
                {
                    row[key] = value;
                }
            }
            if window["status"] == "cached"
                && let Some(stamp) = stamp(&window["sampledAt"])
            {
                row["status"] = json!("cached");
                row["sampledAt"] = json!(stamp);
            }
            Some(row)
        })
        .collect()
}

const LABELS: [(&str, &str); 3] = [
    ("antigravity", "Antigravity"),
    ("qwen", "Qwen token plan"),
    ("zai", "Z.ai coding plan"),
];

fn label(id: &str) -> &str {
    LABELS
        .iter()
        .find(|entry| entry.0 == id)
        .map(|entry| entry.1)
        .or_else(|| aac_providers::provider(id).map(|row| row.label))
        .unwrap_or(id)
}

pub(super) fn additional(
    source: &Source,
    host: &str,
    host_label: &str,
    raw: Option<&Value>,
) -> Value {
    let raw = raw.unwrap_or(&Value::Null);
    let email = email(&raw["email"]);
    let plan = display_text(Some(&raw["plan"]), 48).filter(|plan| {
        plan.as_bytes()[0].is_ascii_alphabetic()
            && plan
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b" ._+-".contains(&b))
    });
    let mut status = raw["status"]
        .as_str()
        .filter(|status| {
            matches!(
                *status,
                "ok" | "cached" | "needs_sign_in" | "error" | "unavailable"
            )
        })
        .unwrap_or("unavailable");
    let mut windows = if matches!(status, "ok" | "cached") {
        windows(&raw["windows"], 256)
    } else {
        vec![]
    };
    let invalid_muse = source.provider == "muse"
        && (email.is_none() || plan.is_none())
        && matches!(status, "ok" | "cached");
    if invalid_muse || (windows.is_empty() && matches!(status, "ok" | "cached")) {
        status = "error";
        windows.clear();
    }
    let message = match status {
        "ok" | "cached" => None,
        "needs_sign_in" => Some("The saved account needs to sign in again."),
        "error" if invalid_muse => Some("The saved Muse account identity could not be verified."),
        "error" => Some("Account usage is temporarily unavailable."),
        _ => Some("Saved account usage is unavailable on this computer."),
    };
    let label = label(&source.provider);
    let verified = raw["verifiedSource"].is_object();
    let fallback = if verified {
        label
    } else {
        email.as_deref().unwrap_or(label)
    };
    let mut row = json!({
        "id":source.id, "provider":source.provider, "providerLabel":label,
        "label":source.label.as_deref().unwrap_or(fallback),
        "email":email,"plan":plan,"platform":source.platform
    });
    if host != source.platform {
        row["host"] = json!(host);
    }
    row["source"] = json!(format!("Account on {host_label}"));
    if let Some(computer) = raw["verifiedSource"]["host"].as_str() {
        apply_verified_source(&mut row, computer, &raw["verifiedSource"]);
    }
    row["status"] = json!(status);
    if status == "cached" {
        row["cacheReason"] = json!(raw["cacheReason"].as_str().unwrap_or("outcome_unknown"));
    }
    row["message"] = json!(if status == "cached" && raw["verifiedSource"].is_object() {
        Some(match row["cacheReason"].as_str() {
            Some("rate_limited") => {
                "Usage refresh is rate limited; showing the last successful reading."
            }
            Some("read_failed") => {
                "The usage read failed or was skipped; showing the last successful reading."
            }
            _ => "The previous usage read outcome was not recorded; showing the saved reading.",
        })
    } else {
        message
    });
    row["fetchedAt"] = json!(stamp(&raw["fetchedAt"]));
    row["sampledAt"] = json!(stamp(&raw["sampledAt"]));
    row["isActive"] = json!(false);
    row["windows"] = json!(windows);
    row["capabilities"] = capabilities();
    row
}

fn apply_verified_source(row: &mut Value, host: &str, verified: &Value) {
    let computers: Value =
        serde_json::from_str(crate::account_reads::COMPUTERS).expect("bundled computers");
    let computer = &computers[host];
    let Some(label) = computer["label"].as_str() else {
        return;
    };
    let Some(os) = computer["os"].as_str() else {
        return;
    };
    let platform = if os == "linux" { "ubuntu" } else { os };
    let quota = if verified["linked"] == true {
        "native subscription quota"
    } else {
        "quota"
    };
    let fallback = if verified["fallback"] == true {
        " (fallback)"
    } else {
        ""
    };
    row["source"] = json!(format!("Verified {quota} on {label}{fallback}"));
    if row["provider"] != "claude" {
        row["platform"] = json!(platform);
    }
    if row["platform"] != host {
        row["host"] = json!(host);
    } else if let Some(fields) = row.as_object_mut() {
        fields.remove("host");
    }
}

pub(super) fn claude(
    profile: &Value,
    desktop: Desktop,
    raw: Option<&Value>,
    history: Option<&Value>,
) -> Option<Value> {
    let email = email(&profile["email"])?;
    let id = profile["id"].as_str();
    let raw = raw.unwrap_or(&Value::Null);
    let windows = windows(&raw["windows"], 256);
    let live = !windows.is_empty() && matches!(raw["status"].as_str(), Some("ok" | "cached"));
    let mut launchers = Vec::new();
    if profile["mac"]["canOpen"] == true {
        launchers.push("mac");
    }
    if id.is_some() && profile["windows"].is_object() {
        launchers.push("windows");
    }
    let label = id.unwrap_or(&email);
    let place = if desktop == Desktop::Mac {
        "Mac"
    } else {
        "Windows"
    };
    let mut row = json!({
        "id":format!("claude:{label}"),"provider":"claude","providerLabel":"Claude","label":label,
        "email":email,"plan":if live { display_text(Some(&raw["plan"]),80) } else {None},
        "platform":desktop.word(),"source":if live && raw.get("profileId").is_none() {
            "Verified quota on Windows".to_owned()
        } else if live {
            "Claude Desktop live quota on Windows".to_owned()
        } else {
            format!("Claude desktop on {place}")
        },
        "status":if live { raw["status"].as_str().unwrap_or("cached") } else { "unavailable" }
    });
    if live && matches!(raw["planClass"].as_str(), Some("max" | "pro")) {
        row["planClass"] = raw["planClass"].clone();
    }
    if raw["status"] == "cached" && live {
        row["cacheReason"] = json!("read_failed");
    }
    row["message"] = if live {
        Value::Null
    } else {
        json!("Claude desktop usage is temporarily unavailable.")
    };
    row["fetchedAt"] = json!(stamp(&raw["fetchedAt"]));
    row["sampledAt"] = json!(stamp(&raw["sampledAt"]));
    if let Some(history) = history {
        apply_claude_history(&mut row, history);
    } else {
        row["windows"] = json!(windows);
    }
    row["isActive"] = json!(false);
    row["capabilities"] =
        json!({"codexProfile":null,"claudeProfileId":id,"claudePlatforms":launchers});
    Some(row)
}

fn apply_claude_history(row: &mut Value, sample: &Value) {
    let (status, message) = match sample["status"].as_str() {
        Some("cached") => (
            "cached",
            "Live Claude usage was not read; showing the earlier desktop history sample.",
        ),
        Some("needs-sign-in") => (
            "needs_sign_in",
            "No usage sample is available. This profile may need its first sign-in on this computer.",
        ),
        _ => (
            "unavailable",
            "Claude desktop usage is temporarily unavailable.",
        ),
    };
    let mut raw = Vec::new();
    let fields = [
        ("fiveHour", "five_hour", "5-hour limit", Some(300)),
        ("weekly", "seven_day", "Weekly limit", Some(10080)),
        ("weeklyOpus", "seven_day_opus", "Opus weekly", Some(10080)),
        (
            "weeklySonnet",
            "seven_day_sonnet",
            "Sonnet weekly",
            Some(10080),
        ),
        ("extra", "extra_usage", "Extra usage", None),
    ];
    for (field, key, label, minutes) in fields {
        if let Some(used) = number(&sample["utilization"][field]) {
            let mut window =
                json!({"key":key,"label":label,"usedPercent":used,"windowMinutes":minutes});
            if field == "extra" {
                window["kind"] = json!("extra_usage");
            }
            raw.push(window);
        }
    }
    row["status"] = json!(status);
    row["message"] = json!(message);
    row["fetchedAt"] = json!(stamp(&sample["fetchedAt"]));
    row["sampledAt"] = json!(stamp(&sample["sampledAt"]));
    row["windows"] = json!(windows(&json!(raw), 5));
    if status == "cached" {
        row["cacheReason"] = json!("outcome_unknown");
    }
}

pub(super) fn pending_claude(profile: &Value, desktop: Desktop) -> Value {
    let id = profile["id"].as_str().unwrap_or_default();
    let name = if desktop == Desktop::Mac {
        "Mac"
    } else {
        "Windows"
    };
    json!({
        "id":format!("claude:{id}"),"provider":"claude","providerLabel":"Claude",
        "label":profile["label"].as_str().unwrap_or(id),"email":null,"plan":null,
        "platform":desktop.word(),"source":format!("Claude desktop on {name}"),
        "status":"needs_sign_in","message":"Open Claude on Mac or Windows and sign in to finish setting up this profile.",
        "fetchedAt":null,"sampledAt":null,"isActive":false,"windows":[],
        "capabilities":{"codexProfile":null,"claudeProfileId":id,"claudePlatforms":[]},
        "lifecycle":{"state":"pending_sign_in","jobId":null}
    })
}

/// The row's own `needsReauth` flag, as the TS projection reads it: a parked
/// profile, a rejected login (even with no sample), or a sample that reports it.
pub(super) fn codex(
    name: &str,
    active: bool,
    needs_reauth: bool,
    candidate: Option<&Value>,
    sample: Option<&Value>,
    note: Option<&aac_renewal::notes::CodexRenewalNote>,
) -> Value {
    let candidate = candidate.unwrap_or(&Value::Null);
    let sample = sample.unwrap_or(&Value::Null);
    let windows = windows(&sample["windows"], 256);
    let status = if needs_reauth {
        "needs_sign_in"
    } else if matches!(sample["status"].as_str(), Some("ok" | "cached")) && !windows.is_empty() {
        sample["status"].as_str().unwrap_or("unavailable")
    } else {
        "unavailable"
    };
    let mut row = json!({
        "id":format!("codex:{name}"),"provider":"codex","providerLabel":"Codex","label":name,
        "email":email(&candidate["email"]),"plan":display_text(Some(&candidate["plan"]),80),
        "platform":"ubuntu","source":if sample["usageSource"] == "local" {"Codex local quota"} else {"Codex saved login on Ubuntu"},"status":status
    });
    if status == "cached" {
        row["cacheReason"] = json!("read_failed");
    }
    // The TS overlay: a renewal note replaces the message, and only OpenAI's
    // rejection replaces the status. cacheReason keeps the measured status.
    row["message"] = match note {
        Some(note) => json!(note.message),
        None => match status {
            "needs_sign_in" => json!("This saved Codex login needs to be renewed."),
            "cached" => json!(
                "The Codex usage read failed or was skipped; showing the last successful reading."
            ),
            "unavailable" => json!("Codex usage is temporarily unavailable."),
            _ => Value::Null,
        },
    };
    if let Some(word) = note.and_then(|note| note.status) {
        row["status"] = json!(word);
    }
    row["fetchedAt"] = json!(stamp(&sample["fetchedAt"]));
    row["sampledAt"] = json!(stamp(&sample["sampledAt"]));
    row["isActive"] = json!(active);
    row["windows"] = json!(windows);
    row["capabilities"] = json!({"codexProfile":name,"claudeProfileId":null,"claudePlatforms":[]});
    row
}
