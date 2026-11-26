//! The saved state: `<CCS dir>/claude-renew/state.json` (0600). Times, emails, profile and
//! computer names and fixed words only.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// What is remembered about one `host/profile` sign-in.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct ProfileState {
    /// The account email at the last reading.
    pub email: Option<String>,
    /// The state word at the last reading.
    pub state: Option<String>,
    /// Its config folder.
    pub dir: Option<String>,
    /// Whether it is `~/.claude`.
    pub default: bool,
    /// The sign-in end at the last reading (ms).
    pub ends_at: Option<i64>,
    /// When it was last read (ms).
    pub read_at: Option<i64>,
    /// When it was last renewed (ms).
    pub last_renew_at: Option<i64>,
    /// The last result word (`renewed`, `waiting_for_you`, a failure word).
    pub last_result: Option<String>,
    /// When the last attempt ran (ms).
    pub last_attempt_at: Option<i64>,
    /// Not before this time (ms).
    pub retry_after: Option<i64>,
    /// Failures in a row.
    pub failures: u32,
    /// The end before the last renewal.
    pub previous_end: Option<i64>,
    /// The end the last renewal produced, until every later reading confirmed it.
    pub expect_end: Option<i64>,
    /// When to read it again (ms).
    pub verify: Vec<i64>,
    /// When every later reading confirmed the last renewal.
    pub verified_at: Option<i64>,
    /// When a renewed sign-in was found back on its old end.
    pub reverted_at: Option<i64>,
    /// How often that happened in a row.
    pub reverts: u32,
}

/// What is remembered about one account's browser profile.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct AccountState {
    /// Why a person is needed (a fixed word), if one is.
    pub needs: Option<String>,
    /// What that person should do, in one sentence.
    pub needs_how: Option<String>,
    /// Since when (ms).
    pub needs_since: Option<i64>,
    /// When its claude.ai browser session ends (the session cookie's expiry, never its value).
    pub session_ends_at: Option<i64>,
    /// When that was read.
    pub session_checked_at: Option<i64>,
    /// When claude.ai was last opened in it to keep the session sliding.
    pub visited_at: Option<i64>,
    /// Whether the last visit moved the session's end later.
    pub visit_extends: Option<bool>,
    /// When a sign-in link was last asked for.
    pub link_requested_at: Option<i64>,
}

/// What is remembered about one computer.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct HostState {
    /// When it was last read.
    pub read_at: Option<i64>,
    /// The last reading's error word, if it failed.
    pub error: Option<String>,
}

/// One event, kept for the last 50.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct Event {
    /// When (ms).
    pub at: i64,
    /// What (`renewed`, `renew_failed`, `reverted`, `browser_signed_in`).
    pub kind: String,
    /// `host/profile`, when it is about one.
    pub profile: Option<String>,
    /// The account.
    pub email: Option<String>,
    /// A fixed word.
    pub reason: Option<String>,
}

/// The whole state file.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct State {
    /// Schema version.
    pub version: u32,
    /// Per `host/profile`.
    pub profiles: BTreeMap<String, ProfileState>,
    /// Per account email.
    pub accounts: BTreeMap<String, AccountState>,
    /// Per computer.
    pub hosts: BTreeMap<String, HostState>,
    /// The last events, oldest first.
    pub events: Vec<Event>,
    /// When the last run finished.
    pub last_tick_at: Option<i64>,
}

impl State {
    /// Reads the state, or an empty one when it is missing or unreadable.
    pub fn load(path: &Path) -> State {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default()
    }

    /// Writes the state atomically, private to this user.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let text = serde_json::to_string_pretty(self).map_err(std::io::Error::other)?;
        write_private(path, text.as_bytes())
    }

    /// Records an event, keeping the last 50.
    pub fn event(
        &mut self,
        at: i64,
        kind: &str,
        profile: Option<&str>,
        email: Option<&str>,
        reason: Option<&str>,
    ) {
        self.events.push(Event {
            at,
            kind: kind.into(),
            profile: profile.map(Into::into),
            email: email.map(Into::into),
            reason: reason.map(Into::into),
        });
        let extra = self.events.len().saturating_sub(50);
        self.events.drain(..extra);
    }

    /// Marks an account as needing a person.
    pub fn need_person(&mut self, email: &str, reason: &str, how: &str, now: i64) {
        let acc = self.accounts.entry(email.into()).or_default();
        if acc.needs.as_deref() != Some(reason) {
            acc.needs_since = Some(now);
        }
        acc.needs = Some(reason.into());
        acc.needs_how = Some(how.into());
    }

    /// Clears that.
    pub fn clear_need(&mut self, email: &str) {
        if let Some(acc) = self.accounts.get_mut(email) {
            acc.needs = None;
            acc.needs_how = None;
            acc.needs_since = None;
        }
    }
}

/// Writes a file atomically with mode 0600 (on Unix), creating its folder 0700.
pub fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = path.with_extension("tmp");
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_keeps_50_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("claude-renew").join("state.json");
        let mut st = State::default();
        st.profiles.insert(
            "mac/party".into(),
            ProfileState {
                ends_at: Some(5),
                ..Default::default()
            },
        );
        for i in 0..60 {
            st.event(i, "renewed", Some("mac/party"), None, None);
        }
        st.need_person("a@b.com", "captcha", "Solve it", 7);
        st.save(&path).expect("save");
        let back = State::load(&path);
        assert_eq!(back, st);
        assert_eq!(back.events.len(), 50);
        assert_eq!(back.events[0].at, 10);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert_eq!(
            State::load(&dir.path().join("missing.json")),
            State::default()
        );
    }
}
