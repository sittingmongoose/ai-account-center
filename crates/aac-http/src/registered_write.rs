//! Local commit helper. The front door holds the registry lease through its transaction
//! and inherits it on fd 2. Stdin contains the existing wire bytes, never argv.
use aac_files::registry;
use std::{
    fs::File,
    io::{self, Read},
    os::fd::AsFd,
    path::Path,
};

pub(crate) fn commit() -> i32 {
    let mut args = std::env::args_os().skip(2);
    let (Some(id), Some(root), None) = (args.next(), args.next(), args.next()) else {
        return 74;
    };
    let (version, list) = match id.to_str() {
        Some("account-registry") => (2, "accounts"),
        Some("claude-pending" | "claude-desktop-profiles") => (1, "profiles"),
        Some("claude-trash") => (1, "entries"),
        _ => return 74,
    };
    let id = id.to_str().expect("admitted id");
    let Ok(lock) = io::stderr().as_fd().try_clone_to_owned() else {
        return 74;
    };
    let Ok(store) = registry::inherit_writer(id, Path::new(&root), File::from(lock)) else {
        return 74;
    };
    let Ok(row) = registry::row(id) else {
        return 74;
    };
    let mut bytes = Vec::new();
    if io::stdin()
        .take(row.max_bytes + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > row.max_bytes
    {
        return 74;
    }
    let Ok(document) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return 74;
    };
    if document.get("version").and_then(serde_json::Value::as_u64) != Some(version)
        || !document.get(list).is_some_and(serde_json::Value::is_array)
    {
        return 74;
    }
    if store.replace_settings(&bytes).is_ok() {
        0
    } else {
        75
    }
}
