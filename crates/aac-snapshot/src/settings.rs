//! The snapshot setting (`~/.ccs/agent-usage-snapshot.json`), as `agent-usage-settings.ts`
//! reads and screens it: an absolute folder without control characters or `..` segments, and
//! at most eight named locations. The writer is the `usage-snapshot set` CLI (gap G19); this
//! crate reads the setting on every publish, so a change needs no restart. A damaged location
//! list only loses the locations, never the folder.

use crate::js::{self, member, settings_control_char, utf16_len};
use serde_json::{Value, json};

/// The setting file under the CCS directory (registry row `agent-usage-snapshot`).
pub const SNAPSHOT_SETTINGS_FILE: &str = "agent-usage-snapshot.json";
/// The read cap: the longest valid setting is about 30 KB.
pub const MAX_SETTINGS_BYTES: u64 = 64 * 1024;
const MAX_DIRECTORY_LENGTH: usize = 1024;
const MAX_LOCATIONS: usize = 8;
const MAX_WHERE_LENGTH: usize = 60;

/// One named place and the path another group of computers finds the folder at.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotLocation {
    /// The group's name, such as "the Mac".
    pub where_: String,
    /// The folder path on that group's computers, any form.
    pub path: String,
}

/// The validated setting: the folder and its optional locations.
#[derive(Clone, Debug, PartialEq)]
pub struct SnapshotSettings {
    /// The share folder the dashboard publishes into.
    pub directory: String,
    /// Where each group of computers finds the folder; empty when none are saved.
    pub locations: Vec<SnapshotLocation>,
}

/// `isSnapshotDirectory`: an absolute folder path without control characters or `..` segments.
/// The absolute rule is the platform's (`path.isAbsolute`): the service reads POSIX paths; the
/// Windows parity tests read Windows paths.
pub fn is_snapshot_directory(value: &Value) -> bool {
    let Some(s) = value.as_str() else {
        return false;
    };
    let len = utf16_len(s);
    if len <= 1 || len > MAX_DIRECTORY_LENGTH || s.chars().any(settings_control_char) {
        return false;
    }
    if !is_absolute(s) {
        return false;
    }
    !s.split(['/', '\\']).any(|part| part == "..")
}

#[cfg(not(windows))]
fn is_absolute(s: &str) -> bool {
    s.starts_with('/')
}

#[cfg(windows)]
fn is_absolute(s: &str) -> bool {
    let bytes = s.as_bytes();
    if bytes.first() == Some(&b'/') || bytes.first() == Some(&b'\\') {
        return true;
    }
    bytes.len() >= 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes.len() == 2 || bytes[2] == b'\\' || bytes[2] == b'/')
}

/// `plainText`: trimmed, non-empty, bounded, without the settings control characters.
fn plain_text(value: &Value, max: usize) -> Option<String> {
    let s = value.as_str()?;
    if s.trim() != s {
        return None;
    }
    let len = utf16_len(s);
    if len == 0 || len > max || s.chars().any(settings_control_char) {
        return None;
    }
    Some(s.to_owned())
}

/// `isSnapshotLocation`: a named place and a path without control characters.
fn is_snapshot_location(value: &Value) -> Option<SnapshotLocation> {
    if !value.is_object() {
        return None;
    }
    Some(SnapshotLocation {
        where_: plain_text(member(value, "where"), MAX_WHERE_LENGTH)?,
        path: plain_text(member(value, "path"), MAX_DIRECTORY_LENGTH)?,
    })
}

/// `isSnapshotLocationList`: at most eight valid locations.
fn location_list(value: &Value) -> Option<Vec<SnapshotLocation>> {
    let items = value.as_array()?;
    if items.len() > MAX_LOCATIONS {
        return None;
    }
    items.iter().map(is_snapshot_location).collect()
}

/// `readSnapshotSettings`' validation of the parsed file: null unless version 1 names a valid
/// folder; a damaged location list only loses the locations.
pub fn validate_settings(value: &Value) -> Option<SnapshotSettings> {
    if !value.is_object() || member(value, "version").as_f64() != Some(1.0) {
        return None;
    }
    let directory = member(value, "directory");
    if !is_snapshot_directory(directory) {
        return None;
    }
    let locations = location_list(member(value, "locations")).unwrap_or_default();
    Some(SnapshotSettings {
        directory: directory.as_str()?.to_owned(),
        locations,
    })
}

/// `renderSnapshotLocations`: the `locations.json` body, pretty JSON with a trailing newline.
pub fn render_snapshot_locations(locations: &[SnapshotLocation]) -> String {
    let list: Vec<Value> = locations
        .iter()
        .map(|location| json!({"where": location.where_, "path": location.path}))
        .collect();
    format!(
        "{}\n",
        js::stringify_pretty(&json!({"version": 1, "locations": list}))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folders_screen_like_the_setting() {
        assert!(is_snapshot_directory(&json!("/share/aac-usage")));
        assert!(!is_snapshot_directory(&json!("share/aac-usage")));
        assert!(!is_snapshot_directory(&json!("/share/../x")));
        assert!(!is_snapshot_directory(&json!("/share/..\\x")));
        assert!(!is_snapshot_directory(&json!("/")));
        assert!(!is_snapshot_directory(&json!("/share/\u{0000}x")));
        assert!(!is_snapshot_directory(&json!(42)));
    }

    #[test]
    fn damaged_locations_keep_the_folder() {
        let value = json!({"version": 1, "directory": "/share", "locations": [{"where": 1}]});
        let settings = validate_settings(&value).expect("folder survives");
        assert_eq!(settings.directory, "/share");
        assert!(settings.locations.is_empty());
        assert!(validate_settings(&json!({"version": 2, "directory": "/share"})).is_none());
        assert!(validate_settings(&json!({"version": 1, "directory": "relative"})).is_none());
    }

    #[test]
    fn locations_render_pretty() {
        let locations = vec![SnapshotLocation {
            where_: "the Mac".into(),
            path: "/Volumes/share/aac-usage".into(),
        }];
        assert_eq!(
            render_snapshot_locations(&locations),
            "{\n  \"version\": 1,\n  \"locations\": [\n    {\n      \"where\": \"the Mac\",\n      \"path\": \"/Volumes/share/aac-usage\"\n    }\n  ]\n}\n"
        );
        assert_eq!(
            render_snapshot_locations(&[]),
            "{\n  \"version\": 1,\n  \"locations\": []\n}\n"
        );
    }
}
