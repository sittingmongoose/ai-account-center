//! The Central Logins step 3 switch schema, shared as source with the Claude renewal
//! runner (the no-new-dependency rule keeps it out of Cargo manifests; both crates compile
//! this same file). No secret is part of the switch.
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The identity a saved Codex copy proved before it was moved: the agent inventory's identity key
/// (a digest of the workspace and principal binding, never a token) and the login's email.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CodexPin {
    /// 64 lowercase hex digits (`codex-principal-v1` identity key).
    pub identity: String,
    /// The login's email, for the row while no T3 login answers.
    pub email: String,
}

/// The step 3 switch. Absent: everything reads and renews as before step 3.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Switch {
    /// Always 1.
    pub version: u32,
    /// When the quarantine ran (UTC, seconds).
    pub since: String,
    /// Set: Codex accounts are read only through a T3 instance's own login with the pinned
    /// identity, never through a saved copy. Profiles not named here are shown unavailable.
    #[serde(default)]
    pub codex: Option<BTreeMap<String, CodexPin>>,
    /// Set: the Claude renewal runner renews only this computer's sign-ins, one per account.
    #[serde(default)]
    pub claude_renewal_host: Option<String>,
}

/// A profile or computer name: 1-64 of letters, digits, `.`, `_`, `-`, starting with a letter or
/// digit (the renewal runner's and the Codex registry's names both fit).
pub fn valid_name(value: &str) -> bool {
    (1..=64).contains(&value.len())
        && value.as_bytes()[0].is_ascii_alphanumeric()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn valid_identity(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn valid_email(value: &str) -> bool {
    (3..=254).contains(&value.len())
        && value.contains('@')
        && value.bytes().all(|b| b.is_ascii_graphic())
}

pub(crate) fn valid_stamp(value: &str) -> bool {
    value.len() == 20
        && value.ends_with('Z')
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || b"-:TZ".contains(&b))
}

impl Switch {
    /// Every field's shape.
    pub fn valid(&self) -> bool {
        self.version == 1
            && valid_stamp(&self.since)
            && self.codex.as_ref().is_none_or(|pins| {
                pins.len() <= 64
                    && pins.iter().all(|(name, pin)| {
                        valid_name(name) && valid_identity(&pin.identity) && valid_email(&pin.email)
                    })
            })
            && self.claude_renewal_host.as_deref().is_none_or(valid_name)
    }

    /// Parse and check the switch's bytes.
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        serde_json::from_slice::<Self>(bytes)
            .ok()
            .filter(Self::valid)
    }
}
