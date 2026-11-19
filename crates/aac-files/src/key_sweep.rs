//! Delete old unreferenced key files under the registered key-folder lease.
use crate::registry;
use aac_core::{Cause, Failure};
use std::{
    collections::HashSet,
    fs::{self, File},
    path::Path,
    time::{Duration, SystemTime},
};

const MIN_AGE_MS: u64 = 60 * 60_000;

fn key_id(id: &str) -> bool {
    id.len() == 8
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Sweep one provider under its workflow queue and inherited folder lease.
/// Callers obtain the named keys from a validated v2 registry before invoking
/// this function. Key bytes are never read.
pub fn sweep(
    root: &Path,
    provider: &str,
    named: &[String],
    now_ms: u64,
    lease: File,
) -> Result<usize, Failure> {
    let marker = format!("{provider}-00000000.json");
    let store = registry::inherit_writer("account-keys", root, Some(&marker), lease)?;
    sweep_held(&store, provider, named, now_ms)
}

/// Sweep one provider from the in-process owner while its workflow queue is held.
pub fn sweep_owned(
    root: &Path,
    owners: &registry::Owners,
    provider: &str,
    named: &[String],
    now_ms: u64,
) -> Result<usize, Failure> {
    let marker = format!("{provider}-00000000.json");
    let store = owners.claim(registry::rows()?, "account-keys", root, Some(&marker))?;
    sweep_held(&store, provider, named, now_ms)
}

fn sweep_held(
    store: &crate::OwnedStore,
    provider: &str,
    named: &[String],
    now_ms: u64,
) -> Result<usize, Failure> {
    let refused = || Failure::new(Cause::Refused, "invalid key sweep inputs");
    if provider.is_empty()
        || provider.len() > 32
        || !provider
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || named.iter().any(|id| !key_id(id))
    {
        return Err(refused());
    }
    let Some(cutoff) = SystemTime::UNIX_EPOCH.checked_add(Duration::from_millis(
        now_ms.checked_sub(MIN_AGE_MS).ok_or_else(refused)?,
    )) else {
        return Err(refused());
    };
    let parent = store.path.parent().ok_or_else(refused)?;
    let prefix = format!("{provider}-");
    let named: HashSet<_> = named.iter().map(String::as_str).collect();
    let mut count = 0;
    for entry in fs::read_dir(parent)?.flatten() {
        let name = entry.file_name();
        let id = name
            .to_str()
            .and_then(|name| name.strip_prefix(&prefix))
            .and_then(|name| name.strip_suffix(".json"));
        if !id.is_some_and(|id| key_id(id) && !named.contains(id)) {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_file()
            && meta.modified().is_ok_and(|time| time <= cutoff)
            && fs::remove_file(&path).is_ok()
        {
            count += 1;
        }
    }
    if count > 0 {
        let _ = crate::sync_directory(parent);
    }
    Ok(count)
}
