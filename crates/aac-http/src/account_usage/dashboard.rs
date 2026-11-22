//! Compose the Accounts DTO and Home facts from native provider rows.
use super::{Desktop, sampler::DashboardSample};
use aac_providers::{Provider, ProviderId, dashboard_providers};
use aac_views::decline::Decline;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};

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

pub(super) fn decorate(
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

/// Expanded digest row ids observed per bare `opencode-go` discover id (deduplicated),
/// excluding any bare id that is still present itself in `rows`.
pub(super) fn discovered_opencode_digests(rows: &[Value]) -> HashMap<String, Vec<String>> {
    let mut present = HashSet::new();
    let mut candidates: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in rows {
        let Some(id) = row["id"].as_str() else {
            continue;
        };
        present.insert(id);
        if row["provider"].as_str() != Some("opencode-go") {
            continue;
        }
        if let Some(parent) = crate::account_usage::producer::recheck::discovered_key_id(id) {
            let group = candidates.entry(parent).or_default();
            if !group.contains(&id) {
                group.push(id);
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|(parent, ids)| {
            (!present.contains(parent)).then(|| {
                (
                    parent.to_owned(),
                    ids.into_iter().map(str::to_owned).collect(),
                )
            })
        })
        .collect()
}

/// Whether either account visibility list contains `target`.
pub(super) fn visibility_contains_account_id(visibility: &Value, target: &str) -> bool {
    ["hiddenAccountIds", "trayHiddenAccountIds"]
        .into_iter()
        .any(|key| {
            visibility
                .get(key)
                .and_then(Value::as_array)
                .is_some_and(|list| list.iter().any(|entry| entry.as_str() == Some(target)))
        })
}

/// Resolve unambiguous `<bare discover id> -> <digest row id>` migrations for bare ids still
/// present in `visibility`. Once a bare id has been seen with a digest key, never retarget it to
/// a different key or after multiple keys appear; entries clear once the bare id leaves
/// `visibility`.
pub(super) fn select_opencode_migrations(
    visibility: &Value,
    rows: &[Value],
    observed: &mut HashMap<String, Option<String>>,
) -> HashMap<String, String> {
    observed.retain(|parent, _| visibility_contains_account_id(visibility, parent));
    let mut migrations = HashMap::new();
    for (parent, digests) in discovered_opencode_digests(rows) {
        if !visibility_contains_account_id(visibility, &parent) {
            continue;
        }
        match digests.as_slice() {
            [only] => match observed.get(&parent) {
                None => {
                    observed.insert(parent.clone(), Some(only.clone()));
                    migrations.insert(parent, only.clone());
                }
                Some(Some(previous)) if previous == only => {
                    migrations.insert(parent, only.clone());
                }
                Some(_) => {
                    observed.insert(parent, None);
                }
            },
            many if many.len() > 1 => {
                observed.insert(parent, None);
            }
            _ => {}
        }
    }
    migrations
}

/// Replace matching account ids in place, preserving order and dropping duplicates.
pub(crate) fn replace_account_ids(
    list: &[Value],
    replacements: &HashMap<String, String>,
) -> (Vec<Value>, bool) {
    if list.is_empty() || replacements.is_empty() {
        return (list.to_vec(), false);
    }
    let mut changed = false;
    let mut next = Vec::with_capacity(list.len());
    for entry in list {
        let mapped = match entry.as_str().and_then(|id| replacements.get(id)) {
            Some(digest) => {
                changed = true;
                Value::String(digest.clone())
            }
            None => entry.clone(),
        };
        if !next.contains(&mapped) {
            next.push(mapped);
        }
    }
    (next, changed)
}

/// Produce the requested desktop and transport generation.
pub fn build(desktop: Desktop, secure: bool, input: Input<'_>) -> Result<DashboardSample, Decline> {
    let visibility = input.visibility;
    let accounts = input
        .rows
        .into_iter()
        .map(|row| decorate(row, visibility, input.lifecycle))
        .collect::<Result<Vec<_>, _>>()?;
    let providers: Vec<_> = dashboard_providers()
        .map(|row| provider(row, &accounts, visibility, input.lifecycle, secure))
        .collect();
    let facts = facts(&accounts, input.signins)?;
    let mut settings = json!({"refreshIntervalSeconds":input.refresh_seconds});
    for key in VISIBILITY {
        settings[key] = visibility.get(key).cloned().unwrap_or_else(|| json!([]));
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

#[cfg(test)]
mod tests {
    use super::*;

    fn opencode_row(id: &str, label: &str) -> Value {
        json!({
            "id": id,
            "provider": "opencode-go",
            "label": label,
            "email": null,
            "plan": "go",
            "platform": "ubuntu",
            "status": "ok",
            "isActive": false,
            "sampledAt": "2026-11-21T12:00:00Z",
            "fetchedAt": "2026-11-21T12:00:00Z",
            "windows": [],
            "capabilities": {}
        })
    }

    fn build_dashboard(rows: Vec<Value>, visibility: Value) -> Value {
        build(
            Desktop::Mac,
            true,
            Input {
                rows,
                visibility: &visibility,
                visibility_available: true,
                refresh_seconds: 60,
                lifecycle: &Lifecycle::default(),
                signins: None,
                server: None,
            },
        )
        .expect("build dashboard")
        .dashboard
    }

    fn row<'a>(dashboard: &'a Value, id: &str) -> &'a Value {
        dashboard["accounts"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["id"] == id))
            .expect("row present")
    }

    #[test]
    fn unsaved_bare_opencode_go_hide_stays_visible_and_migrations_stay_unambiguous() {
        let sample_rows = || {
            vec![
                opencode_row("opencode-go:acct:11111111", "Key 1"),
                opencode_row("opencode-go:acct:22222222", "Key 2"),
                opencode_row("opencode-go:usage:cccccccc", "Key cccccccc"),
            ]
        };

        // Unsaved bare ID in visibility: build() never remaps it in memory for display.
        let unsaved_home = build_dashboard(
            sample_rows(),
            json!({
                "hiddenProviders": [],
                "hiddenAccountIds": ["opencode-go:usage"],
                "trayHiddenProviders": [],
                "trayHiddenAccountIds": []
            }),
        );
        assert_eq!(
            unsaved_home["settings"]["hiddenAccountIds"],
            json!(["opencode-go:usage"])
        );
        assert_eq!(unsaved_home["settings"]["trayHiddenAccountIds"], json!([]));
        assert_eq!(
            row(&unsaved_home, "opencode-go:usage:cccccccc")["hidden"],
            false
        );
        assert_eq!(
            row(&unsaved_home, "opencode-go:usage:cccccccc")["trayHidden"],
            false
        );

        let unsaved_tray = build_dashboard(
            sample_rows(),
            json!({
                "hiddenProviders": [],
                "hiddenAccountIds": [],
                "trayHiddenProviders": [],
                "trayHiddenAccountIds": ["opencode-go:usage"]
            }),
        );
        assert_eq!(
            unsaved_tray["settings"]["trayHiddenAccountIds"],
            json!(["opencode-go:usage"])
        );
        assert_eq!(
            row(&unsaved_tray, "opencode-go:usage:cccccccc")["hidden"],
            false
        );
        assert_eq!(
            row(&unsaved_tray, "opencode-go:usage:cccccccc")["trayHidden"],
            false
        );

        // Migration selection + replacement keeps Home and tray independent, deduplicates and
        // preserves order.
        let mut observed = HashMap::new();
        let both_vis = json!({
            "hiddenProviders": [],
            "hiddenAccountIds": ["codex:party", "opencode-go:usage", "opencode-go:usage:cccccccc"],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": ["opencode-go:usage"]
        });
        let migrations = select_opencode_migrations(&both_vis, &sample_rows(), &mut observed);
        assert_eq!(
            migrations,
            HashMap::from([(
                "opencode-go:usage".to_owned(),
                "opencode-go:usage:cccccccc".to_owned(),
            )])
        );
        let (next_home, home_changed) = replace_account_ids(
            both_vis["hiddenAccountIds"].as_array().expect("home"),
            &migrations,
        );
        let (next_tray, tray_changed) = replace_account_ids(
            both_vis["trayHiddenAccountIds"].as_array().expect("tray"),
            &migrations,
        );
        assert!(home_changed && tray_changed);
        assert_eq!(
            next_home,
            vec![json!("codex:party"), json!("opencode-go:usage:cccccccc")]
        );
        assert_eq!(next_tray, vec![json!("opencode-go:usage:cccccccc")]);

        // Ambiguous: two digest rows -> selects nothing and locks out later retargeting to B.
        let two_rows = vec![
            opencode_row("opencode-go:usage:aaaaaaaa", "Key aaaaaaaa"),
            opencode_row("opencode-go:usage:bbbbbbbb", "Key bbbbbbbb"),
        ];
        let bare_vis = json!({
            "hiddenProviders": [],
            "hiddenAccountIds": ["opencode-go:usage"],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": ["opencode-go:usage"]
        });
        let mut seq_observed = HashMap::new();
        assert!(select_opencode_migrations(&bare_vis, &two_rows, &mut seq_observed).is_empty());
        assert!(
            select_opencode_migrations(
                &bare_vis,
                &[opencode_row("opencode-go:usage:bbbbbbbb", "Key bbbbbbbb")],
                &mut seq_observed,
            )
            .is_empty()
        );

        // Bare row still present: keep bare id on bare row, do not migrate to digest row.
        let bare_present_rows = vec![
            opencode_row("opencode-go:usage", "OpenCode Go"),
            opencode_row("opencode-go:usage:cccccccc", "Key cccccccc"),
        ];
        let mut bare_observed = HashMap::new();
        assert!(
            select_opencode_migrations(&bare_vis, &bare_present_rows, &mut bare_observed)
                .is_empty()
        );
        let bare_present = build_dashboard(bare_present_rows, bare_vis);
        assert_eq!(
            bare_present["settings"]["hiddenAccountIds"],
            json!(["opencode-go:usage"])
        );
        assert_eq!(row(&bare_present, "opencode-go:usage")["hidden"], true);
        assert_eq!(
            row(&bare_present, "opencode-go:usage:cccccccc")["hidden"],
            false
        );
    }
}
