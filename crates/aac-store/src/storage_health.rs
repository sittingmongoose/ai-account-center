//! Metadata-only observation of the legacy quota writer's temporary files. These are not
//! classified as orphans: even an old file may belong to a suspended, live writer.
use serde_json::{Value as Json, json};
use std::fs;
use std::path::Path;

fn quota_temporary(name: &str) -> bool {
    let Some(uuid) = name
        .strip_prefix("quota-history-v1.json.")
        .and_then(|name| name.strip_suffix(".tmp"))
    else {
        return false;
    };
    let bytes = uuid.as_bytes();
    bytes.len() == 36
        && bytes[14] == b'4'
        && b"89ab".contains(&bytes[19])
        && bytes.iter().enumerate().all(|(at, &c)| {
            if [8, 13, 18, 23].contains(&at) {
                c == b'-'
            } else {
                c.is_ascii_digit() || (b'a'..=b'f').contains(&c)
            }
        })
}

pub fn quota_temporaries(store_dir: &Path) -> Json {
    const DIAGNOSTIC: &str = "Quota temporary files may belong to live writers. Offline maintenance only: stop every writer using this CCS_HOME before removing confirmed orphan files. No automatic deletion.";
    let mut count = 0u64;
    let mut bytes = 0u64;
    let mut errors = 0u64;
    let directory = store_dir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("account-analytics");
    let entries = match fs::symlink_metadata(&directory) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => match fs::read_dir(&directory) {
            Ok(entries) => Some(entries),
            Err(_) => {
                errors += 1;
                None
            }
        },
        _ => {
            errors += 1;
            None
        }
    };
    if let Some(entries) = entries {
        for entry in entries {
            let Ok(entry) = entry else {
                errors += 1;
                continue;
            };
            if !quota_temporary(&entry.file_name().to_string_lossy()) {
                continue;
            }
            match fs::symlink_metadata(entry.path()) {
                Ok(metadata) if metadata.is_file() => {
                    count += 1;
                    bytes = bytes.saturating_add(metadata.len());
                }
                Ok(_) => {} // Symlinks and directories do not contribute to apparent file bytes.
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {} // A writer published.
                Err(_) => errors += 1,
            }
        }
    }
    json!({
        "count": count,
        "bytes": bytes,
        "scan": if errors == 0 { "complete" } else { "incomplete" },
        "scanErrors": errors,
        "warning": count > 0 || errors > 0,
        "automaticDeletion": false,
        "diagnostic": if count > 0 || errors > 0 { Some(DIAGNOSTIC) } else { None },
    })
}
