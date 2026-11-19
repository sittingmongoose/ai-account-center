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
    let (Some(id), Some(root), filename, None) =
        (args.next(), args.next(), args.next(), args.next())
    else {
        return 74;
    };
    let Some(id) = id.to_str() else {
        return 74;
    };
    let key = if id == "account-keys" {
        let Some(name) = filename.as_deref().and_then(std::ffi::OsStr::to_str) else {
            return 74;
        };
        let Some((provider, id)) = key_name(name) else {
            return 74;
        };
        Some((name, provider, id))
    } else if filename.is_some() {
        return 74;
    } else {
        None
    };
    let schema = match id {
        "account-registry" => Some((2, "accounts")),
        "claude-pending" | "claude-desktop-profiles" => Some((1, "profiles")),
        "claude-trash" => Some((1, "entries")),
        "status-answer-log" | "account-keys" => None,
        _ => return 74,
    };
    let Ok(lock) = io::stderr().as_fd().try_clone_to_owned() else {
        return 74;
    };
    let Ok(store) = registry::inherit_writer(
        id,
        Path::new(&root),
        key.map(|(name, _, _)| name),
        File::from(lock),
    ) else {
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
    if let Some((_, provider, key_id)) = key {
        return write_key(&store, &document, &bytes, provider, key_id);
    }
    let Some((version, list)) = schema else {
        return append_answer(&store, &document, &bytes);
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

fn append_answer(store: &aac_files::OwnedStore, value: &serde_json::Value, bytes: &[u8]) -> i32 {
    use aac_files::AnswerAppendError;
    let Some(line) = value.get("line").and_then(serde_json::Value::as_str) else {
        return 74;
    };
    if line.len() != 16
        || !line.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !bytes.starts_with(b"\n")
        || !bytes.ends_with(b"\n")
        || bytes.iter().filter(|&&byte| byte == b'\n').count() != 2
    {
        return 74;
    }
    let cancel = if value.get("type").and_then(serde_json::Value::as_str) == Some("void") {
        String::new()
    } else {
        format!("\n{{\"type\":\"void\",\"line\":\"{line}\"}}\n")
    };
    match store.append_answer(bytes, cancel.as_bytes()) {
        Ok(()) => 0,
        Err(AnswerAppendError::Refused) => 75,
        Err(AnswerAppendError::Unclear) => 76,
    }
}

fn key_name(name: &str) -> Option<(&str, &str)> {
    let (provider, key_id) = name.strip_suffix(".json")?.rsplit_once('-')?;
    (aac_providers::provider(provider).is_some_and(|entry| entry.key_store)
        && key_id.len() == 8
        && key_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
    .then_some((provider, key_id))
}

fn write_key(
    store: &aac_files::OwnedStore,
    value: &serde_json::Value,
    bytes: &[u8],
    provider: &str,
    key_id: &str,
) -> i32 {
    if value.is_null() {
        return if store.remove_settings().is_ok() {
            0
        } else {
            75
        };
    }
    let Some(secret) = value.get("secret").and_then(serde_json::Value::as_str) else {
        return 74;
    };
    if !(8..=512).contains(&secret.len())
        || !secret.bytes().all(|byte| (0x21..=0x7e).contains(&byte))
        || value.get("provider").and_then(serde_json::Value::as_str) != Some(provider)
        || value.get("keyId").and_then(serde_json::Value::as_str) != Some(key_id)
        || value.get("version").and_then(serde_json::Value::as_u64) != Some(1)
    {
        return 74;
    }
    if store.replace_settings(bytes).is_ok() {
        0
    } else {
        75
    }
}
