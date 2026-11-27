//! Central Logins step 3: Claude renewal is cut to one computer's sign-ins, one per account.
//!
//! The switch is `<CCS dir>/logins-step3.json`, written by `aac-agent --logins-quarantine
//! --claude-renewal-host <name>` and removed by `aac-agent --logins-restore`. Its other fields
//! (the Codex accounts) are the dashboard's. Absent: every computer is renewed, as before. A switch
//! that cannot be read pauses renewal: renewing everywhere could make a full sign-in again on a
//! computer whose sign-in was quarantined.

use std::path::Path;

/// Which sign-ins the runner renews.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Step3 {
    /// No switch: every covered computer.
    #[default]
    Off,
    /// Only this computer's sign-ins, the first profile (by name) of each account.
    RenewOnly(String),
    /// The switch cannot be read: nothing is renewed until it is fixed or restored.
    Paused,
}

const MAX_BYTES: u64 = 64 * 1024;

/// The switch's file under the CCS dir.
pub fn path(ccs_dir: &Path) -> std::path::PathBuf {
    ccs_dir.join("logins-step3.json")
}

/// Reads the switch.
pub fn read(ccs_dir: &Path) -> Step3 {
    let file = path(ccs_dir);
    match std::fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Step3::Off,
        Ok(meta) if meta.is_file() && meta.len() <= MAX_BYTES => {}
        _ => return Step3::Paused,
    }
    let Ok(text) = std::fs::read_to_string(&file) else {
        return Step3::Paused;
    };
    parse(&text)
}

/// Parses the switch's text.
pub fn parse(text: &str) -> Step3 {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(text) else {
        return Step3::Paused;
    };
    if value["version"] != 1 || !value.is_object() {
        return Step3::Paused;
    }
    match value.get("claudeRenewalHost") {
        None | Some(serde_json::Value::Null) => Step3::Off,
        Some(serde_json::Value::String(host)) if crate::config::safe_name(host) => {
            Step3::RenewOnly(host.clone())
        }
        Some(_) => Step3::Paused,
    }
}

/// The `host/profile` keys that may be renewed out of these sign-ins (host, profile, email), or
/// `None` when every one may. With `RenewOnly(host)`: the first profile by name of each account on
/// that computer (one with no account yet stays eligible, as before); paused: none.
pub fn renewed<'a>(
    step3: &Step3,
    signins: impl Iterator<Item = (&'a str, &'a str, Option<&'a str>)>,
) -> Option<std::collections::HashSet<String>> {
    let host = match step3 {
        Step3::Off => return None,
        Step3::Paused => return Some(Default::default()),
        Step3::RenewOnly(host) => host,
    };
    let mut here: Vec<_> = signins.filter(|(h, _, _)| *h == host.as_str()).collect();
    here.sort_by(|a, b| a.1.cmp(b.1));
    let mut accounts = std::collections::HashSet::new();
    Some(
        here.into_iter()
            .filter(|(_, _, email)| email.is_none_or(|e| accounts.insert(e.to_owned())))
            .map(|(h, p, _)| format!("{h}/{p}"))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_switch_reads_strictly() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(read(dir.path()), Step3::Off);
        std::fs::write(
            path(dir.path()),
            r#"{"version":1,"since":"2026-11-27T18:00:00Z","codex":null,"claudeRenewalHost":"ubuntu"}"#,
        )
        .expect("switch");
        assert_eq!(read(dir.path()), Step3::RenewOnly("ubuntu".into()));
        assert_eq!(parse(r#"{"version":1,"codex":{}}"#), Step3::Off);
        for bad in [
            "",
            "[]",
            r#"{"version":2,"claudeRenewalHost":"ubuntu"}"#,
            r#"{"version":1,"claudeRenewalHost":"bad;name"}"#,
            r#"{"version":1,"claudeRenewalHost":7}"#,
        ] {
            assert_eq!(parse(bad), Step3::Paused, "{bad}");
        }
    }
}
