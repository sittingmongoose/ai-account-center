//! PUT replacements and PATCH deltas for the four visibility lists.
use crate::account_reads::{id_list, known_providers, store_folder, visibility};
use crate::preferences_write::retry_busy_lease;
use aac_core::Cause;
use aac_files::{
    OwnedStore,
    registry::{self, Owners},
};
use aac_views::decline::Decline;
use serde_json::{Map, Value, json};
use std::{collections::HashMap, path::Path, time::Instant};

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

fn write_document(store: &OwnedStore, document: &Map<String, Value>) -> Result<(), Decline> {
    let mut file = serde_json::to_vec_pretty(document).map_err(|_| Decline::Internal)?;
    file.push(b'\n');
    store.replace_settings(&file).map_err(|_| Decline::Internal)
}

/// Persist unambiguous bare-to-digest account ID migrations in place under the visibility file
/// lease. Updates `hiddenAccountIds` and `trayHiddenAccountIds` independently, only for bare IDs
/// still present while the lease is held. Returns `Ok(None)` when the file is unavailable.
pub(crate) fn migrate_account_ids(
    root: &Path,
    owners: &Owners,
    replacements: &HashMap<String, String>,
) -> Result<Option<Value>, Decline> {
    if replacements.is_empty() {
        return Ok(visibility(root).map(|mut current| current["visibility"].take()));
    }
    let rows = registry::rows().map_err(|_| Decline::Internal)?;
    let store = owners
        .claim(rows, "account-visibility", root, None)
        .map_err(|_| Decline::Unsupported)?;
    let Some(current) = visibility(root) else {
        return Ok(None);
    };
    let mut any_changed = false;
    let mut lists = Map::new();
    let mut document = Map::new();
    document.insert("version".into(), json!(1));
    for key in LISTS {
        let prior = current["visibility"][key]
            .as_array()
            .ok_or(Decline::Shape)?;
        let next = if key.ends_with("AccountIds") {
            let (replaced, changed) =
                crate::account_usage::dashboard::replace_account_ids(prior, replacements);
            if changed {
                any_changed = true;
                list(key, &Value::Array(replaced)).ok_or(Decline::Shape)?
            } else {
                prior.clone()
            }
        } else {
            prior.clone()
        };
        document.insert(key.into(), Value::Array(next.clone()));
        lists.insert(key.into(), Value::Array(next));
    }
    if any_changed {
        write_document(&store, &document)?;
    }
    Ok(Some(Value::Object(lists)))
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
    deadline: Instant,
) -> Result<Value, Decline> {
    save_until(root, owners, patch, bytes, deadline)
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
                let show_parents: Vec<&str> = if key.ends_with("AccountIds") {
                    show.iter()
                        .filter_map(|entry| {
                            entry.as_str().and_then(
                                crate::account_usage::producer::recheck::discovered_key_id,
                            )
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                next.retain(|entry| {
                    !show.contains(entry)
                        && !entry
                            .as_str()
                            .is_some_and(|id| show_parents.contains(&id) && !hide.contains(entry))
                });
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
    write_document(&store, &document)?;
    Ok(
        json!({ "state": "saved", "visibility": lists, "folderWarnings": current["folderWarnings"] }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn patch_show_on_digest_id_clears_legacy_bare_id_in_patched_list_only() {
        let dir = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod dir");
        let file = dir.path().join("account-visibility.json");
        fs::write(
            &file,
            r#"{"version":1,"hiddenProviders":[],"hiddenAccountIds":["codex:party","opencode-go:usage"],"trayHiddenProviders":[],"trayHiddenAccountIds":["opencode-go:usage"]}"#,
        )
        .expect("write visibility");
        #[cfg(unix)]
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod file");

        let rows = registry::rows().expect("registry rows");
        let files: std::collections::BTreeMap<String, String> = rows
            .iter()
            .map(|row| {
                let owner = if row.id == "account-visibility" {
                    "rust-service"
                } else {
                    row.owner.word()
                };
                (row.id.clone(), owner.to_owned())
            })
            .collect();
        let owners = Owners::from_boot(rows, &files).expect("owners");
        let after_home = save(
            dir.path(),
            &owners,
            true,
            br#"{"hiddenAccountIds":{"hide":[],"show":["opencode-go:usage:cccccccc"]}}"#,
        )
        .expect("patch home");
        assert_eq!(
            after_home["visibility"],
            json!({
                "hiddenProviders": [],
                "hiddenAccountIds": ["codex:party"],
                "trayHiddenProviders": [],
                "trayHiddenAccountIds": ["opencode-go:usage"]
            })
        );

        let after_tray = save(
            dir.path(),
            &owners,
            true,
            br#"{"trayHiddenAccountIds":{"hide":[],"show":["opencode-go:usage:cccccccc"]}}"#,
        )
        .expect("patch tray");
        assert_eq!(
            after_tray["visibility"],
            json!({
                "hiddenProviders": [],
                "hiddenAccountIds": ["codex:party"],
                "trayHiddenProviders": [],
                "trayHiddenAccountIds": []
            })
        );
    }

    #[test]
    fn migrate_account_ids_persists_independently_under_lease_and_leaves_direct_digests_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        #[cfg(unix)]
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).expect("chmod dir");
        let file = dir.path().join("account-visibility.json");
        fs::write(
            &file,
            r#"{"version":1,"hiddenProviders":["cursor"],"hiddenAccountIds":["codex:party","opencode-go:usage","opencode-go:usage:aaaaaaaa"],"trayHiddenProviders":["qwen"],"trayHiddenAccountIds":["opencode-go:usage:bbbbbbbb"]}"#,
        )
        .expect("write visibility");
        #[cfg(unix)]
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("chmod file");

        let rows = registry::rows().expect("registry rows");
        let ts_owners = Owners::defaults(rows);
        let replacements = HashMap::from([(
            "opencode-go:usage".to_owned(),
            "opencode-go:usage:aaaaaaaa".to_owned(),
        )]);
        // Refuses when another process owns the visibility row.
        assert!(migrate_account_ids(dir.path(), &ts_owners, &replacements).is_err());

        let files: std::collections::BTreeMap<String, String> = rows
            .iter()
            .map(|row| {
                let owner = if row.id == "account-visibility" {
                    "rust-service"
                } else {
                    row.owner.word()
                };
                (row.id.clone(), owner.to_owned())
            })
            .collect();
        let owners = Owners::from_boot(rows, &files).expect("owners");

        let migrated = migrate_account_ids(dir.path(), &owners, &replacements)
            .expect("migrate")
            .expect("available");
        assert_eq!(
            migrated,
            json!({
                "hiddenProviders": ["cursor"],
                "hiddenAccountIds": ["codex:party", "opencode-go:usage:aaaaaaaa"],
                "trayHiddenProviders": ["qwen"],
                "trayHiddenAccountIds": ["opencode-go:usage:bbbbbbbb"]
            })
        );
        // No new file created beside account-visibility.json (only its .lock lease).
        for entry in fs::read_dir(dir.path()).expect("read dir") {
            let name = entry.expect("entry").file_name();
            let name = name.to_string_lossy();
            assert!(
                name == "account-visibility.json" || name == "account-visibility.json.lock",
                "unexpected file {name}"
            );
        }

        // Subsequent migration with a different digest is a no-op because the bare ID is gone.
        let bytes_after = fs::read(&file).expect("read migrated bytes");
        let second = migrate_account_ids(
            dir.path(),
            &owners,
            &HashMap::from([(
                "opencode-go:usage".to_owned(),
                "opencode-go:usage:cccccccc".to_owned(),
            )]),
        )
        .expect("second migrate")
        .expect("available");
        assert_eq!(second, migrated);
        assert_eq!(fs::read(&file).expect("read unchanged bytes"), bytes_after);
    }
}
