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
    let Some(id) = id.to_str() else {
        return 74;
    };
    let schema = match id {
        "account-registry" => Some((2, "accounts")),
        "claude-pending" | "claude-desktop-profiles" => Some((1, "profiles")),
        "claude-trash" => Some((1, "entries")),
        "status-answer-log" => None,
        _ => return 74,
    };
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
