//! Shared Claude lifecycle record validation for Open and Accounts.
use aac_files::{
    read::{self, Read},
    registry,
};
use serde_json::{Value, json};
use std::path::Path;

/// The pending registry's tighter `CLAUDE_PROFILE_ID` (`^[a-z][a-z0-9-]{1,31}$`).
pub(crate) fn pending_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// JavaScript `\s` for the email and trim rules.
pub(crate) fn is_js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

/// The manifest reader's `readText`: nonempty after JS trim, bounded UTF-16 length, no C0/DEL.
pub(crate) fn read_text(value: &Value, max: usize) -> Option<&str> {
    let text = value.as_str()?;
    if text.trim_matches(is_js_space).is_empty()
        || text.encode_utf16().count() > max
        || text.bytes().any(|b| b < 0x20 || b == 0x7f)
    {
        return None;
    }
    Some(text)
}

/// One launcher as the Open path reads it. Absent optional fields stay absent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Launcher {
    /// The wrapper's display name.
    pub launcher_name: String,
    /// The configured wrapper path, when present.
    pub launcher_path: Option<String>,
    /// The profile data folder, when present.
    pub profile_path: Option<String>,
    /// The Windows Start Menu shortcut, when present.
    pub start_menu_path: Option<String>,
    /// The inventory's default marker, when present.
    pub is_default: Option<bool>,
    /// The SSH alias, when present.
    pub ssh_host: Option<String>,
}

/// The pending registry's `parseLauncher`: exact key allowlist, required name/path/alias.
pub(crate) fn parse_pending_launcher(value: &Value) -> Option<Launcher> {
    let map = value.as_object()?;
    const ALLOWED: [&str; 5] = [
        "launcherName",
        "launcherPath",
        "profilePath",
        "startMenuPath",
        "sshHost",
    ];
    if map.keys().any(|key| !ALLOWED.contains(&key.as_str())) {
        return None;
    }
    let launcher_name = read_text(map.get("launcherName")?, 255)?.to_string();
    let profile_path = read_text(map.get("profilePath")?, 4096)?.to_string();
    let ssh_host = map.get("sshHost")?.as_str()?;
    let bytes = ssh_host.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 128
        || !bytes[0].is_ascii_alphanumeric()
        || !bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
    {
        return None;
    }
    let mut launcher = Launcher {
        launcher_name,
        profile_path: Some(profile_path),
        ssh_host: Some(ssh_host.to_string()),
        ..Launcher::default()
    };
    for (field, slot) in [
        ("launcherPath", &mut launcher.launcher_path),
        ("startMenuPath", &mut launcher.start_menu_path),
    ] {
        if let Some(value) = map.get(field) {
            *slot = Some(read_text(value, 4096)?.to_string());
        }
    }
    Some(launcher)
}

/// The pending registry's `TIMESTAMP` shape.
pub(crate) fn timestamp_shape(value: &str) -> bool {
    let bytes = value.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|part| part.iter().all(u8::is_ascii_digit))
    };
    if !(bytes.len() == 20 || bytes.len() == 24) {
        return false;
    }
    digits(0..4)
        && bytes[4] == b'-'
        && digits(5..7)
        && bytes[7] == b'-'
        && digits(8..10)
        && bytes[10] == b'T'
        && digits(11..13)
        && bytes[13] == b':'
        && digits(14..16)
        && bytes[16] == b':'
        && digits(17..19)
        && (bytes.len() == 20 || (bytes[19] == b'.' && digits(20..23)))
        && bytes[bytes.len() - 1] == b'Z'
}

fn trash_id(id: &str) -> bool {
    id.strip_prefix("tr_").is_some_and(|hex| {
        hex.len() == 16
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

fn trash_name(name: &str) -> bool {
    let Some((id, stamp)) = name.rsplit_once('-') else {
        return false;
    };
    let bytes = stamp.as_bytes();
    pending_id(id)
        && bytes.len() == 16
        && bytes[8] == b'T'
        && bytes[15] == b'Z'
        && bytes[..8]
            .iter()
            .chain(&bytes[9..15])
            .all(u8::is_ascii_digit)
}

fn public_trash_entry(entry: &Value) -> Option<Value> {
    let id = entry["trashId"].as_str().filter(|id| trash_id(id))?;
    let label = read_text(&entry["label"], 254)?;
    let trashed = entry["trashedAt"]
        .as_str()
        .filter(|at| timestamp_shape(at))?;
    let purge = entry["purgeAfter"]
        .as_str()
        .filter(|at| timestamp_shape(at))?;
    let state = entry["state"]
        .as_str()
        .filter(|state| matches!(*state, "trashed" | "deleting"))?;
    if entry["provider"] != "claude"
        || !entry["accountId"]
            .as_str()
            .and_then(|id| id.strip_prefix("claude:"))
            .is_some_and(pending_id)
        || !matches!(entry["source"].as_str(), Some("inventory" | "pending"))
        || !entry["entry"].is_object()
    {
        return None;
    }
    for (host, detail) in entry["hosts"].as_object()? {
        if !matches!(host.as_str(), "mac" | "windows")
            || !detail["trashName"].as_str().is_some_and(trash_name)
            || parse_pending_launcher(&detail["launcher"]).is_none()
        {
            return None;
        }
    }
    Some(
        json!({"trashId":id,"provider":"claude","label":label,"trashedAt":trashed,"purgeAfter":purge,"state":state}),
    )
}

pub(crate) fn list_trash(root: &Path) -> Option<Vec<Value>> {
    let row = registry::row("claude-trash").ok()?;
    let file = root.join(row.file(None).ok()?);
    let bytes = match read::private_store(&file, row.max_bytes) {
        Read::Absent => return Some(Vec::new()),
        Read::Bytes(bytes, _) => bytes,
        _ => return None,
    };
    let doc: Value = serde_json::from_str(&String::from_utf8_lossy(&bytes)).ok()?;
    let entries = doc["entries"].as_array()?;
    if doc["version"].as_f64() != Some(1.0) || entries.len() > 64 {
        return None;
    }
    entries.iter().map(public_trash_entry).collect()
}
