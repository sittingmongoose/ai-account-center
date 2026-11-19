//! The write boundary for dashboard-scope quota observations and remote snapshots.
use aac_core::{Cause, Failure};
use aac_files::registry::{self, Owners};
use serde_json::Value;
use std::path::Path;

pub(crate) const OBSERVATIONS: &str = "account-usage-observations";
pub(crate) const SNAPSHOTS: &str = "claude-live-snapshots";
const OBSERVATION_FIELDS: &[&str] = &[
    "version",
    "key",
    "identity",
    "primary",
    "saved",
    "nextAttempt",
    "fresh",
    "freshUntil",
    "primaryFailure",
    "primarySignIn",
    "observedPlan",
    "observedPlanAt",
    "observedPlanPeer",
];

fn digest(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

fn observation(key: &str, value: &Value) -> bool {
    value.as_object().is_some_and(|row| {
        row.keys()
            .all(|field| OBSERVATION_FIELDS.contains(&field.as_str()))
            && value["version"] == 1
            && value["key"] == key
            && value["nextAttempt"].is_number()
            && ["identity", "primary", "saved"].iter().all(|field| {
                row.get(*field)
                    .is_some_and(|v| v.is_null() || v.is_object())
            })
    })
}

fn snapshot(id: &str, value: &Value) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        && value.as_object().is_some_and(|row| row.len() == 3)
        && value["schemaVersion"] == 1
        && value["manifestHash"].as_str().is_some_and(digest)
        && value["sample"]["profileId"] == id
        && value["sample"]["platform"] == "windows"
        && value["sample"]["windows"].is_array()
}

pub(crate) fn write(
    owners: &Owners,
    root: &Path,
    kind: &str,
    key: &str,
    bytes: &[u8],
) -> Result<(), Failure> {
    let refused = || Failure::new(Cause::Refused, "invalid collection cache");
    let value: Value = serde_json::from_slice(bytes).map_err(|_| refused())?;
    let registered = match kind {
        OBSERVATIONS if digest(key) && observation(key, &value) => OBSERVATIONS,
        SNAPSHOTS if snapshot(key, &value) => SNAPSHOTS,
        _ => return Err(refused()),
    };
    let store = owners.claim(
        registry::rows()?,
        registered,
        root,
        Some(&format!("{key}.json")),
    )?;
    store.write_private_cache(bytes)?;
    if registered == OBSERVATIONS {
        store.prune_private_cache(256, |name, bytes| {
            name.strip_suffix(".json").is_some_and(|old| {
                digest(old)
                    && serde_json::from_slice(bytes).is_ok_and(|value| observation(old, &value))
            })
        });
    }
    Ok(())
}
