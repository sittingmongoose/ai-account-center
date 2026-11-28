//! Central Logins step 3: Claude renewal is cut to one computer's sign-ins, one per account.
//!
//! The switch is `<CCS dir>/logins-step3.json`, written by `aac-agent --logins-quarantine
//! --claude-renewal-host <name>` and removed by `aac-agent --logins-restore`. Its other fields
//! (the Codex accounts) are the dashboard's. Absent: every computer is renewed, as before. A switch
//! that cannot be read pauses renewal: renewing everywhere could make a full sign-in again on a
//! computer whose sign-in was quarantined.

use std::path::Path;

#[path = "../../aac-files/src/step3_switch.rs"]
mod step3_switch;

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

/// Reads the switch. Only a genuinely absent file is Off before parsing; a present file that
/// cannot be admitted or proven against the one schema is Paused, never normal renewal.
pub fn read(ccs_dir: &Path) -> Step3 {
    let file = path(ccs_dir);
    let meta = match std::fs::symlink_metadata(&file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Step3::Off,
        Ok(meta) => meta,
        Err(_) => return Step3::Paused,
    };
    if !meta.is_file() || meta.len() > MAX_BYTES {
        return Step3::Paused;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let root_owner = std::fs::metadata(ccs_dir).map(|root| root.uid()).ok();
        if meta.mode() & 0o077 != 0 || meta.nlink() != 1 || root_owner != Some(meta.uid()) {
            return Step3::Paused;
        }
    }
    let Ok(bytes) = std::fs::read(&file) else {
        return Step3::Paused;
    };
    parse(&String::from_utf8_lossy(&bytes))
}

/// Parses the switch's text through the same schema as the dashboard and Codex reader.
pub fn parse(text: &str) -> Step3 {
    let Some(switch) = step3_switch::Switch::parse(text.as_bytes()) else {
        return Step3::Paused;
    };
    match switch.claude_renewal_host {
        None => Step3::Off,
        Some(host) => Step3::RenewOnly(host),
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
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path(dir.path()), std::fs::Permissions::from_mode(0o600))
                .expect("private switch mode");
        }
        assert_eq!(read(dir.path()), Step3::RenewOnly("ubuntu".into()));
        for damaged in [
            r#"{"version":1}"#,
            r#"{"version":1,"since":"2026-11-27T18:00:00Z","claudeRenewalHos":"ubuntu"}"#,
        ] {
            std::fs::write(path(dir.path()), damaged).expect("damaged switch");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(path(dir.path()), std::fs::Permissions::from_mode(0o600))
                    .expect("private switch mode");
            }
            assert_eq!(read(dir.path()), Step3::Paused, "{damaged}");
        }
        assert_eq!(
            parse(r#"{"version":1,"since":"2026-11-27T18:00:00Z","codex":{}}"#),
            Step3::Off
        );
        for bad in [
            "",
            "[]",
            r#"{"version":1}"#,
            r#"{"version":1,"since":"2026-11-27T18:00:00Z","claudeRenewalHos":"ubuntu"}"#,
            r#"{"version":2,"since":"2026-11-27T18:00:00Z","claudeRenewalHost":"ubuntu"}"#,
            r#"{"version":1,"since":"2026-11-27T18:00:00Z","claudeRenewalHost":"bad;name"}"#,
            r#"{"version":1,"since":"2026-11-27T18:00:00Z","claudeRenewalHost":7}"#,
        ] {
            assert_eq!(parse(bad), Step3::Paused, "{bad}");
        }
    }
}
