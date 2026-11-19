//! The Open path's profile reads, ported from `claude-desktop-profile-service.ts` (the manifest
//! parse and the can-open rules), `claude-account-stores.ts` (the pending registry) and
//! `claude-account-records.ts` (the default-folder rule). The orchestrator resolves membership
//! and launchers from these exactly as `resolveOpenTarget` does; client paths never enter.
use super::js::utf16_len;
use serde_json::{Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

/// `CLAUDE_PROFILE_ID_PATTERN`: the safe-ID shape every Claude desktop path checks first.
pub fn profile_id_pattern(id: &str) -> bool {
    let bytes = id.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
}

/// The pending registry's tighter `CLAUDE_PROFILE_ID` (`^[a-z][a-z0-9-]{1,31}$`).
fn pending_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (2..=32).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// JavaScript `\s` for the email and trim rules.
fn is_js_space(c: char) -> bool {
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
fn read_text(value: &Value, max: usize) -> Option<&str> {
    let text = value.as_str()?;
    if text.trim_matches(is_js_space).is_empty()
        || utf16_len(text) > max
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

/// A manifest profile (`ClaudeDesktopProfile`).
#[derive(Clone, Debug, Default)]
pub struct Profile {
    /// The manifest id; rows without one cannot be opened.
    pub id: Option<String>,
    /// The confirmed account email.
    pub email: String,
    /// The Mac launcher, when configured.
    pub mac: Option<Launcher>,
    /// The Windows launcher, when configured.
    pub windows: Option<Launcher>,
}

/// `canOpenClaudeMacProfile`.
pub fn can_open_mac(profile: &Profile) -> bool {
    profile.id.is_some()
        && profile.mac.as_ref().is_some_and(|mac| {
            mac.ssh_host.is_some()
                && mac
                    .launcher_path
                    .as_deref()
                    .is_some_and(|path| path.starts_with('/') && path.ends_with(".app"))
        })
}

/// `canOpenClaudeWindowsProfile`.
pub fn can_open_windows(profile: &Profile) -> bool {
    profile.id.as_deref().is_some_and(profile_id_pattern)
        && profile
            .windows
            .as_ref()
            .is_some_and(|windows| windows.ssh_host.is_some())
}

/// The manifest's `readLauncher`: known fields only, unknown keys ignored.
fn read_launcher(value: &Value, windows: bool) -> Option<Launcher> {
    let map = value.as_object()?;
    let mut launcher = Launcher {
        launcher_name: read_text(map.get("launcherName")?, 255)?.to_string(),
        ..Launcher::default()
    };
    for (field, slot) in [
        ("launcherPath", &mut launcher.launcher_path),
        ("profilePath", &mut launcher.profile_path),
    ] {
        if let Some(value) = map.get(field) {
            *slot = Some(read_text(value, 4096)?.to_string());
        }
    }
    if windows && let Some(value) = map.get("startMenuPath") {
        launcher.start_menu_path = Some(read_text(value, 4096)?.to_string());
    }
    if let Some(value) = map.get("isDefault") {
        launcher.is_default = Some(value.as_bool()?);
    }
    if let Some(value) = map.get("sshHost") {
        let host = read_text(value, 128)?;
        let bytes = host.as_bytes();
        if !bytes[0].is_ascii_alphanumeric()
            || !bytes[1..]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(b))
        {
            return None;
        }
        launcher.ssh_host = Some(host.to_string());
    }
    Some(launcher)
}

/// The email shape `/^[^\s@]+@[^\s@]+\.[^\s@]+$/`.
fn email_shape(email: &str) -> bool {
    if email.chars().any(is_js_space) {
        return false;
    }
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return false;
    }
    domain
        .char_indices()
        .any(|(index, c)| c == '.' && index > 0 && index < domain.len() - 1)
}

/// The manifest's `readProfile`.
fn read_profile(value: &Value) -> Option<Profile> {
    let map = value.as_object()?;
    let email = read_text(map.get("email")?, 254)?;
    if !email_shape(email) {
        return None;
    }
    let mut profile = Profile {
        email: email.to_string(),
        ..Profile::default()
    };
    if let Some(value) = map.get("id") {
        let id = read_text(value, 64)?;
        let bytes = id.as_bytes();
        if !bytes[0].is_ascii_alphanumeric()
            || !bytes[1..]
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || *b == b'_' || *b == b'-')
        {
            return None;
        }
        profile.id = Some(id.to_string());
    }
    if let Some(value) = map.get("mac") {
        profile.mac = Some(read_launcher(value, false)?);
    }
    if let Some(value) = map.get("windows") {
        profile.windows = Some(read_launcher(value, true)?);
    }
    if profile.mac.is_none() && profile.windows.is_none() {
        return None;
    }
    Some(profile)
}

/// `listClaudeDesktopProfiles`: the private inventory, or an empty list when absent. Any other
/// read or shape failure is an error the Open answers as its generic refusal.
pub fn list_profiles(ccs_dir: &Path) -> Result<Vec<Profile>, ()> {
    let filename = ccs_dir.join("claude-desktop-profiles.json");
    let bytes = match fs::read(&filename) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(_) => return Err(()),
    };
    let contents = String::from_utf8_lossy(&bytes);
    if utf16_len(&contents) > 1024 * 1024 {
        return Err(());
    }
    let manifest = super::js::parse(contents.as_bytes()).ok_or(())?;
    let version_ok = manifest["version"] == serde_json::json!(1);
    let Some(rows) = manifest["profiles"].as_array() else {
        return Err(());
    };
    if !manifest.is_object() || !version_ok || rows.len() > 1000 {
        return Err(());
    }
    let mut profiles = Vec::new();
    let mut ids: Vec<String> = Vec::new();
    for row in rows {
        let profile = read_profile(row).ok_or(())?;
        if let Some(id) = &profile.id {
            if ids.contains(id) {
                return Err(());
            }
            ids.push(id.clone());
        }
        profiles.push(profile);
    }
    Ok(profiles)
}

/// One pending profile (`PendingClaudeProfile`), as the Open path reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingProfile {
    /// The profile id.
    pub id: String,
    /// The display label, when set.
    pub label: Option<String>,
    /// The Mac host launcher.
    pub mac: Launcher,
    /// The Windows host launcher.
    pub windows: Launcher,
    /// The creation timestamp.
    pub created_at: String,
    /// The expected email, when asserted.
    pub expected_email: Option<String>,
}

/// The pending registry's `parseLauncher`: exact key allowlist, required name/path/alias.
fn parse_pending_launcher(value: &Value) -> Option<Launcher> {
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
fn timestamp_shape(value: &str) -> bool {
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

/// The pending registry's `parsePending`.
pub(crate) fn parse_pending(value: &Value) -> Option<Vec<PendingProfile>> {
    let map = value.as_object()?;
    if map.get("version")? != &serde_json::json!(1) {
        return None;
    }
    let rows = map.get("profiles")?.as_array()?;
    if rows.len() > 64 {
        return None;
    }
    let mut profiles: Vec<PendingProfile> = Vec::new();
    for item in rows {
        let item = item.as_object()?;
        let id = item.get("id")?.as_str()?;
        if !pending_id(id) {
            return None;
        }
        let mac = parse_pending_launcher(item.get("mac")?)?;
        let windows = parse_pending_launcher(item.get("windows")?)?;
        let expected_email = match item.get("expectedEmail") {
            None | Some(Value::Null) => None,
            Some(value) => Some(read_text(value, 254)?.to_string()),
        };
        let label = match item.get("label") {
            Some(Value::Null) => None,
            Some(value) => Some(read_text(value, 96)?.to_string()),
            None => return None,
        };
        let created_at = item.get("createdAt")?.as_str()?;
        if !timestamp_shape(created_at) || profiles.iter().any(|profile| profile.id == id) {
            return None;
        }
        profiles.push(PendingProfile {
            id: id.to_string(),
            label,
            mac,
            windows,
            created_at: created_at.to_string(),
            expected_email,
        });
    }
    Some(profiles)
}

/// The private JSON store's read result.
enum PrivateRead {
    Absent,
    Invalid,
    Ok(Value),
}

/// `checkStoreFolder`: others-writable is unsafe; group-writable and symlink folders are
/// accepted (the one-time warning belongs to the TS reader's log until the cut).
fn store_folder_ok(directory: &Path) -> bool {
    let meta = match fs::symlink_metadata(directory) {
        Ok(meta) => meta,
        Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
    };
    let meta = if meta.file_type().is_symlink() {
        match fs::metadata(directory) {
            Ok(meta) => meta,
            Err(error) => return error.kind() == std::io::ErrorKind::NotFound,
        }
    } else {
        meta
    };
    meta.is_dir() && meta.permissions().mode() & 0o002 == 0
}

/// `readPrivateJsonFile` with the store's caps and privacy rules.
fn read_private_json(file: &Path, max_bytes: u64) -> PrivateRead {
    if !file.parent().is_some_and(store_folder_ok) {
        return PrivateRead::Invalid;
    }
    let link = match fs::symlink_metadata(file) {
        Ok(link) => link,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                PrivateRead::Absent
            } else {
                PrivateRead::Invalid
            };
        }
    };
    if link.file_type().is_symlink() || !link.is_file() {
        return PrivateRead::Invalid;
    }
    let mut handle = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(file)
    {
        Ok(handle) => handle,
        Err(error) => {
            return if error.kind() == std::io::ErrorKind::NotFound {
                PrivateRead::Absent
            } else {
                PrivateRead::Invalid
            };
        }
    };
    let stat = match handle.metadata() {
        Ok(stat) => stat,
        Err(_) => return PrivateRead::Invalid,
    };
    if !stat.is_file() || stat.len() > max_bytes || stat.permissions().mode() & 0o077 != 0 {
        return PrivateRead::Invalid;
    }
    let mut buffer = vec![0u8; max_bytes as usize + 1];
    let read = match handle.read(&mut buffer) {
        Ok(read) => read,
        Err(_) => return PrivateRead::Invalid,
    };
    if read as u64 > max_bytes {
        return PrivateRead::Invalid;
    }
    let contents = String::from_utf8_lossy(&buffer[..read]);
    match super::js::parse(contents.as_bytes()) {
        Some(value) => PrivateRead::Ok(value),
        None => PrivateRead::Invalid,
    }
}

/// `readPendingProfiles`: absent is empty; an unreadable or malformed store is an error.
pub fn read_pending_profiles(ccs_dir: &Path) -> Result<Vec<PendingProfile>, ()> {
    let file = ccs_dir.join("accounts").join("claude-pending.json");
    match read_private_json(&file, 256 * 1024) {
        PrivateRead::Absent => Ok(Vec::new()),
        PrivateRead::Ok(value) => parse_pending(&value).ok_or(()),
        PrivateRead::Invalid => Err(()),
    }
}

/// `pendingOpenProfile`: a pending profile opens like a manifest launcher, with no confirmed
/// email, so the managed history policy never matches it.
pub fn pending_open_profile(pending: &PendingProfile) -> Profile {
    Profile {
        id: Some(pending.id.clone()),
        email: String::new(),
        mac: Some(Launcher {
            launcher_name: pending.mac.launcher_name.clone(),
            launcher_path: pending.mac.launcher_path.clone(),
            profile_path: pending.mac.profile_path.clone(),
            ssh_host: pending.mac.ssh_host.clone(),
            ..Launcher::default()
        }),
        windows: Some(Launcher {
            launcher_name: pending.windows.launcher_name.clone(),
            launcher_path: pending.windows.launcher_path.clone(),
            profile_path: pending.windows.profile_path.clone(),
            start_menu_path: pending.windows.start_menu_path.clone(),
            ssh_host: pending.windows.ssh_host.clone(),
            ..Launcher::default()
        }),
    }
}

/// `isDefaultClaudeDataFolder`: the last path segment is `claude`, case-insensitive.
pub fn is_default_data_folder(profile_path: Option<&str>) -> bool {
    let Some(path) = profile_path else {
        return false;
    };
    let trimmed = path.trim_end_matches(['\\', '/']);
    let last = trimmed.split(['\\', '/']).next_back().unwrap_or("");
    last.eq_ignore_ascii_case("claude")
}

/// `claudeWindowsLauncherAccountList`: the default Windows profile first, then every other
/// Windows-openable profile in manifest order; null unless exactly one default and at most 64.
pub fn windows_launcher_account_list(
    manifest: &[Profile],
    pending: &[PendingProfile],
) -> Option<Vec<String>> {
    let mut windows: Vec<Profile> = manifest.to_vec();
    windows.extend(pending.iter().map(pending_open_profile));
    let windows: Vec<Profile> = windows
        .into_iter()
        .filter(|profile| profile.id.is_some() && can_open_windows(profile))
        .collect();
    let defaults: Vec<&Profile> = windows
        .iter()
        .filter(|profile| {
            profile.windows.as_ref().is_some_and(|launcher| {
                launcher.is_default == Some(true)
                    || is_default_data_folder(launcher.profile_path.as_deref())
            })
        })
        .collect();
    if defaults.len() != 1 {
        return None;
    }
    let mut ids = vec![defaults[0].id.clone()?];
    for profile in &windows {
        let id = profile.id.clone()?;
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    (ids.len() <= 64).then_some(ids)
}

/// The raw manifest entry fields the policy revalidation compares (no `readProfile` parse).
pub fn raw_manifest(ccs_dir: &Path) -> Option<Value> {
    let bytes = fs::read(ccs_dir.join("claude-desktop-profiles.json")).ok()?;
    super::js::parse(&bytes)
}

/// The manifest object's `profiles` array, when shaped.
pub fn manifest_rows(manifest: &Value) -> Option<&Vec<Value>> {
    if manifest.as_object()?.get("version")? != &serde_json::json!(1) {
        return None;
    }
    manifest.as_object()?.get("profiles")?.as_array()
}

/// One raw entry's launcher field equality for the policy rebind (`undefined`-aware).
pub fn launcher_field_matches(actual: &Value, expected: Option<&str>) -> bool {
    match (actual, expected) {
        (Value::Null, None) => true,
        (Value::String(actual), Some(expected)) => actual == expected,
        _ => false,
    }
}

/// The entry's platform object, when it is one.
pub fn entry_platform(entry: &Value, platform: &str) -> Option<Map<String, Value>> {
    entry.as_object()?.get(platform)?.as_object().cloned()
}

/// One manifest row as the Open path reads it (the parity table feeds rows directly).
pub fn profile_from_value(value: &Value) -> Option<Profile> {
    read_profile(value)
}

/// A pending registry document as the Open path reads it (the parity table feeds it directly).
pub fn pending_from_value(value: &Value) -> Option<Vec<PendingProfile>> {
    parse_pending(value)
}
