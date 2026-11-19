//! Compose the Accounts DTO and Home facts from native provider rows.
use super::{Desktop, sampler::DashboardSample};
use aac_providers::{Provider, ProviderId, dashboard_providers};
use aac_views::decline::Decline;
use serde_json::{Map, Value, json};

const VISIBILITY: [&str; 4] = [
    "hiddenProviders",
    "hiddenAccountIds",
    "trayHiddenProviders",
    "trayHiddenAccountIds",
];

pub(super) fn empty_visibility() -> Value {
    Value::Object(
        VISIBILITY
            .into_iter()
            .map(|key| (key.into(), json!([])))
            .collect(),
    )
}

/// Lifecycle facts from the native route owner. If that owner is absent, all
/// lifecycle actions remain unavailable.
#[derive(Clone, Default)]
pub struct Lifecycle {
    /// The public listener mounts the lifecycle routes.
    pub mounted: bool,
    /// Per-provider unavailability reason from the workflow preflight.
    pub blocked: Map<String, Value>,
    /// Providers whose removal workflow is unavailable.
    pub remove_disabled: Vec<ProviderId>,
    /// Providers whose recheck workflow is unavailable.
    pub recheck_disabled: Vec<ProviderId>,
    /// Running sign-in jobs keyed by public account id.
    pub jobs: Map<String, Value>,
}

fn listed(value: &Value, word: &Value) -> bool {
    value.as_array().is_some_and(|list| list.contains(word))
}

fn provider(
    row: &Provider,
    accounts: &[Value],
    visibility: &Value,
    lifecycle: &Lifecycle,
    secure: bool,
) -> Value {
    use ProviderId::*;
    let id = row.id.as_str();
    let id_value = json!(id);
    let (kind, label, platforms) = match row.id {
        Claude => (
            "desktop-profile",
            "Desktop profile",
            &["mac", "windows"][..],
        ),
        Codex => ("device-code", "Device-code sign-in", &["ubuntu"][..]),
        Antigravity => ("supervised-cli", "Supervised CLI login", &["ubuntu"][..]),
        Cursor => (
            "app-session",
            "Desktop app session",
            &["mac", "windows"][..],
        ),
        Muse => ("device-code", "Device-code sign-in", &["mac"][..]),
        Qwen => (
            "browser-session",
            "Console session by browser extension",
            &["windows"][..],
        ),
        _ => ("api-key", "API key", &["ubuntu"][..]),
    };
    let requires_secure = matches!(kind, "api-key" | "device-code" | "supervised-cli");
    let reason = if !lifecycle.mounted {
        json!("not_implemented")
    } else if let Some(reason) = lifecycle.blocked.get(id) {
        reason.clone()
    } else if requires_secure && !secure {
        json!("secure_transport_required")
    } else {
        Value::Null
    };
    let available = reason.is_null();
    let multi = matches!(
        row.id,
        Claude | Codex | Antigravity | KimiCode | Zai | OpencodeGo
    );
    let count = accounts
        .iter()
        .filter(|account| account["provider"] == id)
        .count();
    let extras = match row.id {
        Muse => {
            json!({"kind":"browser-extension","platform":"mac","label":"Quota sync by browser extension"})
        }
        OpencodeGo => {
            json!({"kind":"browser-extension","platform":"mac","label":"Console wallet by browser extension"})
        }
        _ => Value::Null,
    };
    let open = match row.id {
        Claude => json!(["mac", "windows"]),
        Cursor if lifecycle.mounted => json!(["mac"]),
        _ => json!([]),
    };
    json!({
        "id":id, "label":row.label, "longLabel":row.long_label, "iconKey":id, "order":row.index,
        "visible":!listed(&visibility["hiddenProviders"], &id_value),
        "trayVisible":!listed(&visibility["trayHiddenProviders"], &id_value),
        "accountCount":count,
        "signIn":{"kind":kind,"label":label,"platforms":platforms,"secureTransportRequired":requires_secure,
            "available":available,"unavailableReason":reason},
        "extras":extras,
        "capabilities":{
            "multiAccount":multi,
            "add": available && count < 16 && (multi || count == 0),
            "signInAgain":lifecycle.mounted && kind != "api-key" &&
                (available || matches!(kind,"desktop-profile"|"app-session"|"browser-session")),
            "replaceKey":kind == "api-key" && available,
            "remove":lifecycle.mounted && !lifecycle.remove_disabled.contains(&row.id),
            "openApp":open,
            "recheck":lifecycle.mounted && !lifecycle.recheck_disabled.contains(&row.id)
        }
    })
}

fn decorate(
    mut account: Value,
    visibility: &Value,
    lifecycle: &Lifecycle,
) -> Result<Value, Decline> {
    let row = account.as_object_mut().ok_or(Decline::Shape)?;
    let id = row
        .get("id")
        .filter(|id| id.is_string())
        .ok_or(Decline::Shape)?;
    let provider = row
        .get("provider")
        .filter(|id| {
            id.as_str()
                .is_some_and(aac_providers::is_dashboard_provider)
        })
        .ok_or(Decline::Shape)?;
    let hidden = listed(&visibility["hiddenProviders"], provider)
        || listed(&visibility["hiddenAccountIds"], id);
    let tray_hidden = listed(&visibility["trayHiddenProviders"], provider)
        || listed(&visibility["trayHiddenAccountIds"], id);
    let job = id
        .as_str()
        .and_then(|id| lifecycle.jobs.get(id))
        .filter(|job| {
            matches!(job["state"].as_str(), Some("signing_in" | "verifying"))
                && job["jobId"].is_string()
        })
        .map(|job| json!({"state":job["state"], "jobId":job["jobId"]}));
    if let Some(job) = job {
        row.insert("lifecycle".into(), job);
    } else if row.get("lifecycle").is_none() {
        row.insert("lifecycle".into(), json!({"state":"ready","jobId":null}));
    }
    row.insert("hidden".into(), json!(hidden));
    row.insert("trayHidden".into(), json!(tray_hidden));
    Ok(account)
}

pub(super) fn facts(accounts: &[Value], signins: Option<&Value>) -> Result<Value, Decline> {
    let mut words: Vec<_> = accounts
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    if let Some(rows) = signins.and_then(|value| value["signIns"].as_array()) {
        for row in rows {
            words.push(row["profile"].as_str().ok_or(Decline::Shape)?);
        }
    }
    if words
        .iter()
        .any(|word| !word.bytes().all(|b| (32..=126).contains(&b)))
    {
        return Err(Decline::Locale);
    }
    words.sort_by(|left, right| aac_num::intl::ascii_compare(left, right).expect("checked ASCII"));
    words.dedup();
    let order: Map<String, Value> = words
        .into_iter()
        .enumerate()
        .map(|(rank, word)| (word.into(), json!(rank)))
        .collect();
    let mut epochs = Map::new();
    let mut add = |stamp: &Value| -> Result<(), Decline> {
        if let Some(stamp) = stamp.as_str() {
            let value = aac_views::js::parse_date(stamp).map_err(|_| Decline::Date)?;
            epochs.insert(stamp.into(), json!(value));
        }
        Ok(())
    };
    for account in accounts {
        add(&account["sampledAt"])?;
        for window in account["windows"].as_array().ok_or(Decline::Shape)? {
            add(&window["sampledAt"])?;
            add(&window["resetAt"])?;
        }
    }
    Ok(json!({"order":order, "epochs":epochs}))
}

/// A complete generation's inputs. The native collection owns the rows and the
/// route owner supplies lifecycle, companion sign-in and server facts.
pub struct Input<'a> {
    /// Ordered, public provider rows.
    pub rows: Vec<Value>,
    /// The last verified visibility lists.
    pub visibility: &'a Value,
    /// The current visibility read succeeded.
    pub visibility_available: bool,
    /// Settings for this pass, in seconds.
    pub refresh_seconds: u64,
    /// Native lifecycle controller facts.
    pub lifecycle: &'a Lifecycle,
    /// Sign-in health from its native owner.
    pub signins: Option<&'a Value>,
    /// Safe listener facts, if mounted by composition.
    pub server: Option<&'a Value>,
}

/// Produce the requested desktop and transport generation.
pub fn build(desktop: Desktop, secure: bool, input: Input<'_>) -> Result<DashboardSample, Decline> {
    let accounts = input
        .rows
        .into_iter()
        .map(|row| decorate(row, input.visibility, input.lifecycle))
        .collect::<Result<Vec<_>, _>>()?;
    let providers: Vec<_> = dashboard_providers()
        .map(|row| provider(row, &accounts, input.visibility, input.lifecycle, secure))
        .collect();
    let facts = facts(&accounts, input.signins)?;
    let mut settings = json!({"refreshIntervalSeconds":input.refresh_seconds});
    for key in VISIBILITY {
        settings[key] = input
            .visibility
            .get(key)
            .cloned()
            .unwrap_or_else(|| json!([]));
    }
    settings["visibilityAvailable"] = json!(input.visibility_available);
    let mut dashboard = json!({
        "schemaVersion":1,"updatedAt":"","settings":settings,"providers":providers,"accounts":accounts,
        "codexAutoSwitch":{"enabled":false,"thresholdPercent":5,"thresholdUsedPercent":95,
            "pollIntervalSeconds":60,"outcome":"disabled",
            "message":"Account switching was removed. AI Account Center shows usage only.",
            "activationInProgress":false}
    });
    if let Some(server) = input.server {
        dashboard["server"] = server.clone();
    }
    if let Some(signins) = input.signins {
        dashboard["claudeSignIns"] = signins.clone();
    }
    Ok(DashboardSample {
        desktop,
        secure,
        dashboard,
        facts,
    })
}
