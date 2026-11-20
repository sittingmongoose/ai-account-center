//! The registry rows the service reads under its boot root (`config/private-files.json`, L2):
//! each file's name and cap come from the one registry TS also reads, and every read goes
//! through `aac_files::read`. A registry this build cannot parse leaves the TS reader to answer.
use aac_files::{
    read::ReadPolicy,
    registry::{self, Root},
};
use std::path::{Path, PathBuf};

/// A `ccs` row's file under the canonical boot root, with a policy capped at the row's
/// `maxBytes` (a final link refused; the caller adds its reference's other rules).
pub(crate) fn file(root: &Path, id: &str) -> Option<(PathBuf, ReadPolicy<'static>)> {
    let row = registry::row(id).ok()?;
    if row.root != Root::Ccs {
        return None;
    }
    Some((
        root.join(row.file(None).ok()?),
        ReadPolicy::capped(row.max_bytes),
    ))
}

pub(crate) fn snapshot_settings(root: &Path) -> Option<aac_snapshot::SnapshotSettings> {
    let (file, policy) = file(root, "agent-usage-snapshot")?;
    let aac_files::read::Read::Bytes(bytes, _) = aac_files::read::read(&file, &policy) else {
        return None;
    };
    aac_snapshot::settings::validate_settings(&serde_json::from_slice(&bytes).ok()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_rows_keep_their_caps() {
        // The caps the TS readers apply (account-visibility.ts, dashboard-preferences.ts,
        // claude-account-stores.ts, account-refresh-settings.ts).
        for (id, name, cap) in [
            ("account-visibility", "account-visibility.json", 32 * 1024),
            (
                "dashboard-preferences",
                "dashboard-preferences.json",
                64 * 1024,
            ),
            (
                "dashboard-preferences-extra-sources",
                "dashboard-preferences-extra-sources.json",
                64 * 1024,
            ),
            (
                "claude-desktop-profiles",
                "claude-desktop-profiles.json",
                1024 * 1024,
            ),
            (
                "account-refresh-settings",
                "account-refresh-settings.json",
                4096,
            ),
        ] {
            let (path, policy) = file(Path::new("/root"), id).expect("registered row");
            assert_eq!(path, Path::new("/root").join(name), "{id}");
            assert_eq!(policy.cap, cap, "{id}");
        }
        assert!(file(Path::new("/root"), "readers-check").is_none());
        assert!(file(Path::new("/root"), "account-keys").is_none());
    }
}
