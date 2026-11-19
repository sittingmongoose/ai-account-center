//! Guarded writes to the shared Antigravity saved-profile registry.
use super::{NativeOwners, lifecycle_codex::RemoveError, lifecycle_maintenance, lifecycle_remove};
use crate::{
    antigravity_runtime::{self as runtime, RegistryState},
    public_auth::storage,
};
use aac_files::{profile_writes::AntigravityMaintenanceLease, read};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn review(native: &NativeOwners, name: &str) -> Result<String, RemoveError> {
    let root = native.agents.root();
    let (document, state, _) = document(root, name)?;
    if exists(&root.join("antigravity-profiles/.transaction-lock"))? {
        return Err(RemoveError::Busy);
    }
    guard(native, &document, &state, name)?;
    Ok(fingerprint(&document))
}

fn document(root: &Path, name: &str) -> Result<(Value, RegistryState, i64), RemoveError> {
    if !runtime::profile_id_ok(name) {
        return Err(RemoveError::Missing);
    }
    let (document, state, generation) = runtime::read_document(root).ok_or(RemoveError::Missing)?;
    if state.profile(name).is_none() {
        return Err(RemoveError::Missing);
    }
    Ok((document, state, generation))
}

fn exists(path: &Path) -> Result<bool, RemoveError> {
    read::entry_exists(path).map_err(|_| RemoveError::Failed)
}

fn guard(
    native: &NativeOwners,
    document: &Value,
    state: &RegistryState,
    name: &str,
) -> Result<(), RemoveError> {
    if !document["transaction"].is_null() {
        return Err(RemoveError::Busy);
    }
    if state.active_for(name).is_some() {
        return Err(RemoveError::Active);
    }
    let profile = state.profile(name).ok_or(RemoveError::Missing)?;
    let home = std::env::home_dir().ok_or(RemoveError::Failed)?;
    if exists(&home.join(".gemini/antigravity-cli/antigravity-oauth-token"))? {
        let host = native
            .usage
            .as_ref()
            .and_then(|usage| usage.antigravity.as_ref())
            .ok_or(RemoveError::Failed)?;
        let (credential, fingerprint) = host.read_native_credential().ok_or(RemoveError::Failed)?;
        if fingerprint == profile.credential.fingerprint {
            return Err(RemoveError::Active);
        }
        let identity = host
            .validate_credential(&credential)
            .ok_or(RemoveError::Failed)?;
        if identity.identity_key() == profile.identity_key {
            return Err(RemoveError::Active);
        }
    }
    let root = native.agents.root();
    if lifecycle_remove::running(native, &format!("antigravity:profile:{name}"))
        || lifecycle_maintenance::antigravity_signin_running(root, name)
    {
        return Err(RemoveError::Running);
    }
    Ok(())
}

fn fingerprint(document: &Value) -> String {
    storage::sha(&document.to_string())
}

pub(super) fn remove(native: &NativeOwners, name: &str, reviewed: &str) -> Result<(), RemoveError> {
    let root = native.agents.root();
    let record = lifecycle_maintenance::holder_record().ok_or(RemoveError::Failed)?;
    let lease = AntigravityMaintenanceLease::claim(root, record.as_bytes())
        .map_err(|_| RemoveError::Busy)?;
    let (mut document, state, _) = document(root, name)?;
    guard(native, &document, &state, name)?;
    if reviewed != fingerprint(&document) {
        return Err(RemoveError::Stale);
    }
    let profiles = document["profiles"]
        .as_array_mut()
        .ok_or(RemoveError::Failed)?;
    profiles.retain(|profile| profile["id"].as_str() != Some(name));
    publish(root, &lease, document).ok_or(RemoveError::Failed)?;
    let left = lease.writer().antigravity_discard(root, name).unwrap_or(1);
    if left > 0 {
        eprintln!("accounts.remove.left_in_place provider=antigravity count={left}");
    }
    Ok(())
}

fn publish(root: &Path, lease: &AntigravityMaintenanceLease, mut document: Value) -> Option<()> {
    let generation = document["revision"].as_i64()?.checked_add(1)?;
    document["revision"] = json!(generation);
    runtime::validate_state(&document)?;
    lease
        .writer()
        .antigravity_index(
            root,
            &format!("{generation:012}"),
            &aac_views::js_json::write(&document),
        )
        .ok()
}
