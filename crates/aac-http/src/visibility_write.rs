//! PUT replacements and PATCH deltas for the four visibility lists.
use crate::account_reads::{id_list, known_providers, store_folder, visibility};
use crate::preferences_write::{LEASE_WAIT, retry_busy_lease};
use aac_core::Cause;
use aac_files::registry::{self, Owners};
use aac_views::decline::Decline;
use serde_json::{Map, Value, json};
use std::{path::Path, time::Instant};

const LISTS: [&str; 4] = [
    "hiddenProviders",
    "hiddenAccountIds",
    "trayHiddenProviders",
    "trayHiddenAccountIds",
];

fn list(key: &str, input: &Value) -> Option<Vec<Value>> {
    if key.ends_with("Providers") {
        let providers = known_providers(input)?;
        // Known provider IDs only. The stored-file reader separately drops retired providers.
        if providers.len() != input.as_array()?.len() || providers.len() > 32 {
            return None;
        }
        let mut unique = Vec::new();
        for provider in providers {
            if !unique.contains(&provider) {
                unique.push(provider);
            }
        }
        Some(unique)
    } else {
        id_list(input)
    }
}

enum Change {
    Replace(Vec<Value>),
    Delta { hide: Vec<Value>, show: Vec<Value> },
}

fn parse(patch: bool, bytes: &[u8]) -> Option<Map<String, Value>> {
    let input: Map<String, Value> = serde_json::from_slice(bytes).ok()?;
    if input.is_empty() || !input.keys().all(|key| LISTS.contains(&key.as_str())) {
        return None;
    }
    crate::json_object::from_slice(
        bytes,
        &LISTS
            .into_iter()
            .filter(|key| input.contains_key(*key))
            .collect::<Vec<_>>(),
    )
    .ok()
    .filter(|_| patch || input.values().all(Value::is_array))
}

fn change(key: &str, input: &Value, patch: bool) -> Option<Change> {
    if !patch {
        return Some(Change::Replace(list(key, input)?));
    }
    // The TS guard normalizes both fields before sending the delta.
    let delta = input.as_object()?;
    if delta.len() != 2 {
        return None;
    }
    let hide = list(key, delta.get("hide")?)?;
    let show = list(key, delta.get("show")?)?;
    if hide.iter().any(|value| show.contains(value)) {
        return None;
    }
    Some(Change::Delta { hide, show })
}

/// Whether the body is the shape the writer accepts for this method (no file access), so the
/// public route can answer the TS `invalid_body` sentence before it opens anything.
pub(crate) fn body_valid(patch: bool, bytes: &[u8]) -> bool {
    let Some(input) = parse(patch, bytes) else {
        return false;
    };
    input
        .iter()
        .all(|(key, input)| change(key, input, patch).is_some())
}

pub(crate) fn save(
    root: &Path,
    owners: &Owners,
    patch: bool,
    bytes: &[u8],
) -> Result<Value, Decline> {
    save_until(root, owners, patch, bytes, Instant::now()).map_err(|error| match error {
        Decline::Busy => Decline::Unsupported,
        error => error,
    })
}

pub(crate) fn save_public(
    root: &Path,
    owners: &Owners,
    patch: bool,
    bytes: &[u8],
) -> Result<Value, Decline> {
    save_until(root, owners, patch, bytes, Instant::now() + LEASE_WAIT)
}

fn save_until(
    root: &Path,
    owners: &Owners,
    patch: bool,
    bytes: &[u8],
    deadline: Instant,
) -> Result<Value, Decline> {
    let input = parse(patch, bytes).ok_or(Decline::Shape)?;
    let changes = input
        .iter()
        .map(|(key, input)| Some((key.as_str(), change(key, input, patch)?)))
        .collect::<Option<Vec<_>>>()
        .ok_or(Decline::Shape)?;

    let rows = registry::rows().map_err(|_| Decline::Internal)?;
    let store = retry_busy_lease(deadline, || {
        owners
            .claim(rows, "account-visibility", root, None)
            .map_err(|error| {
                if error.cause == Cause::Busy {
                    Decline::Busy
                } else {
                    Decline::Unsupported
                }
            })
    })?;
    let full = !patch && input.len() == LISTS.len();
    let current = if full {
        let warnings = store_folder(root).ok_or(Decline::Unsupported)?;
        json!({ "folderWarnings": warnings })
    } else if let Some(current) = visibility(root) {
        current
    } else {
        return Ok(json!({ "state": "unavailable" }));
    };
    let mut lists = Map::new();
    let mut document = Map::new();
    document.insert("version".into(), json!(1));
    for key in LISTS {
        let prior = current["visibility"][key].as_array();
        let next = match changes
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, change)| change)
        {
            Some(Change::Replace(values)) => values.clone(),
            Some(Change::Delta { hide, show }) => {
                let mut next = prior.ok_or(Decline::Shape)?.clone();
                next.retain(|entry| !show.contains(entry));
                for entry in hide {
                    if !next.contains(entry) {
                        next.push(entry.clone());
                    }
                }
                list(key, &Value::Array(next)).ok_or(Decline::Shape)?
            }
            None => prior.ok_or(Decline::Shape)?.clone(),
        };
        document.insert(key.into(), Value::Array(next.clone()));
        lists.insert(key.into(), Value::Array(next));
    }
    let mut file = serde_json::to_vec_pretty(&document).map_err(|_| Decline::Internal)?;
    file.push(b'\n');
    store
        .replace_settings(&file)
        .map_err(|_| Decline::Internal)?;
    Ok(
        json!({ "state": "saved", "visibility": lists, "folderWarnings": current["folderWarnings"] }),
    )
}
