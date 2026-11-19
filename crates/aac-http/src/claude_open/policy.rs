//! Port of `loadClaudeHistoryPolicy` and `parseClaudeHistoryPolicy` from
//! `claude-history-sync-service.ts`: the private managed policy is read only from a revalidated
//! manifest entry, so a changed manifest can never redirect a copy. Unknown or absent policy
//! never enrolls the current native identity as a new trusted identity.
use super::js;
use super::manifest::{launcher_field_matches, profile_id_pattern};
use super::sync::validate_policy;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

const MAX_MANIFEST_BYTES: u64 = 256 * 1024;

/// The profile fields the policy rebind compares: the launcher the Open already selected.
pub struct ExpectedLauncher<'a> {
    /// The SSH alias the Open resolved.
    pub ssh_host: Option<&'a str>,
    /// The profile data folder the Open resolved.
    pub profile_path: Option<&'a str>,
    /// The wrapper path the Open resolved.
    pub launcher_path: Option<&'a str>,
}

/// `parseClaudeHistoryPolicy`: the allowlist shape, then the core's full value validation.
/// Only the validated allowlist is cloned forward; private unknown fields never enter a
/// transport request.
pub fn parse_policy(value: &Value) -> Option<Value> {
    let object = |value: &Value| value.is_object();
    let keys_are = |value: &Value, allowed: &[&str]| {
        value
            .as_object()
            .is_some_and(|map| map.keys().all(|key| allowed.contains(&key.as_str())))
    };
    if !object(value)
        || !keys_are(
            value,
            &["version", "enabled", "sourcePlatform", "identity", "project", "ssh"],
        )
        || !object(&value["identity"])
        || !keys_are(&value["identity"], &["accountUuid", "organizationUuid"])
        || !object(&value["project"])
        || !keys_are(&value["project"], &["cwd", "originCwd", "transcriptRoot"])
        || !object(&value["ssh"])
        || !keys_are(&value["ssh"], &["mac", "windows"])
    {
        return None;
    }
    if !value["identity"]["accountUuid"]
        .as_str()
        .is_some_and(js::uuid)
        || !value["identity"]["organizationUuid"]
            .as_str()
            .is_some_and(js::uuid)
    {
        return None;
    }
    for platform in ["mac", "windows"] {
        let endpoint = &value["ssh"][platform];
        if !object(endpoint) || !keys_are(endpoint, &["alias", "hostname", "username", "port"]) {
            return None;
        }
    }
    // The core validates all field values; a refusal is an absent policy.
    let cloned = js::parse(js::stringify(value).as_bytes())?;
    validate_policy(&cloned).ok()?;
    Some(cloned)
}

/// `loadClaudeHistoryPolicy`: revalidate the manifest entry the Open selected, then parse its
/// `historySync`. Every read guard failure answers None (the ordinary Open proceeds).
pub fn load_policy(
    ccs_dir: &Path,
    profile_id: &str,
    profile_email: &str,
    mac: Option<ExpectedLauncher<'_>>,
    windows: Option<ExpectedLauncher<'_>>,
) -> Option<Value> {
    if profile_id.is_empty() || !profile_id_pattern(profile_id) {
        return None;
    }
    let filename = ccs_dir.join("claude-desktop-profiles.json");
    let before = fs::symlink_metadata(&filename).ok()?;
    if !before.is_file()
        || before.file_type().is_symlink()
        || before.len() > MAX_MANIFEST_BYTES
        || before.uid() != super::markers::process_uid()
        || before.permissions().mode() & 0o077 != 0
    {
        return None;
    }
    let mut handle = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&filename)
        .ok()?;
    let opened = handle.metadata().ok()?;
    if opened.dev() != before.dev() || opened.ino() != before.ino() || opened.len() != before.len()
    {
        return None;
    }
    let mut bytes = Vec::new();
    handle.read_to_end(&mut bytes).ok()?;
    let after = fs::symlink_metadata(&filename).ok()?;
    if bytes.len() as u64 > MAX_MANIFEST_BYTES
        || after.dev() != opened.dev()
        || after.ino() != opened.ino()
        || after.len() != opened.len()
        || mtime_ms(&after) != mtime_ms(&opened)
        || ctime_ms(&after) != ctime_ms(&opened)
    {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let manifest = js::parse(text.as_bytes())?;
    let rows = super::manifest::manifest_rows(&manifest)?;
    let matches: Vec<&Value> = rows
        .iter()
        .filter(|entry| entry.is_object() && entry["id"] == Value::String(profile_id.to_string()))
        .collect();
    if matches.len() != 1 {
        return None;
    }
    let entry = matches[0];
    if !entry.is_object() || entry["email"].as_str() != Some(profile_email) {
        return None;
    }
    // Rebind the launcher paths/hosts to the entry the authenticated Open selected.
    for (platform, expected) in [("mac", mac), ("windows", windows)] {
        let expected = expected?;
        let actual = entry.get(platform).filter(|value| value.is_object())?;
        if !launcher_field_matches(&actual["sshHost"], expected.ssh_host)
            || !launcher_field_matches(&actual["profilePath"], expected.profile_path)
            || !launcher_field_matches(&actual["launcherPath"], expected.launcher_path)
        {
            return None;
        }
    }
    parse_policy(&entry["historySync"])
}

/// Node's `stat.mtimeMs` float milliseconds.
fn mtime_ms(meta: &fs::Metadata) -> f64 {
    meta.mtime() as f64 * 1000.0 + meta.mtime_nsec() as f64 / 1_000_000.0
}

/// Node's `stat.ctimeMs`.
fn ctime_ms(meta: &fs::Metadata) -> f64 {
    meta.ctime() as f64 * 1000.0 + meta.ctime_nsec() as f64 / 1_000_000.0
}
