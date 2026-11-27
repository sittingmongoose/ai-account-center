//! Logins view v1 (Central Logins step 1): every account x computer x tool, its kind, when it ends, who renews it
//! and its health. Read-only.
//!
//! The server (`aac-views::logins`) builds it and the Leptos Accounts & Settings page (`web-dashboard/leptos`) draws
//! it, so the two share these types. **No type here has a field that could carry a secret:** no token, cookie, key,
//! file path, file contents or command. The account is the email the Accounts page already shows (or the profile's
//! label). Reading is lenient the way the other views read: missing or `null` text reads as empty.
use crate::home::nullable;
use serde::{Deserialize, Serialize};

/// The view version the page draws.
pub const LOGINS_VIEW_VERSION: u32 = 1;

/// The whole Logins view.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct LoginsView {
    /// Always [`LOGINS_VIEW_VERSION`].
    pub version: u32,
    /// When the view was built, ISO.
    #[serde(deserialize_with = "nullable")]
    pub generated_at: String,
    /// One row per account x computer x tool, worst health first.
    #[serde(deserialize_with = "nullable")]
    pub rows: Vec<LoginRow>,
    /// The counts the section's head shows.
    pub counts: LoginCounts,
    /// Plain sentences about where the facts came from (an old renewal report, a missing one).
    #[serde(deserialize_with = "nullable")]
    pub notes: Vec<String>,
}

/// The head's counts.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct LoginCounts {
    /// Every row.
    pub total: u32,
    /// Rows whose health is `needs_you` or `ended`.
    pub needs_you: u32,
    /// Rows whose health is `expiring`.
    pub expiring: u32,
}

/// One login.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct LoginRow {
    /// A stable key for the list (provider, computer, tool and account).
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// The provider id (`claude`, `codex`, ...).
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    /// The provider's name.
    #[serde(deserialize_with = "nullable")]
    pub provider_label: String,
    /// The account as the Accounts page shows it: its email, else its label.
    #[serde(deserialize_with = "nullable")]
    pub account: String,
    /// The computer's name.
    #[serde(deserialize_with = "nullable")]
    pub computer: String,
    /// The tool that holds the login (with its profile when there are several).
    #[serde(deserialize_with = "nullable")]
    pub tool: String,
    /// `static` (a key that does not rotate), `tool` (the tool's own sign-in) or `renewed` (AAC renews it: "Auto-renewed").
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    /// The kind in words.
    #[serde(deserialize_with = "nullable")]
    pub kind_label: String,
    /// When the login ends, ISO, when that is known.
    pub ends_at: Option<String>,
    /// The end in words ("in 22 days", "No set end", "Not known").
    #[serde(deserialize_with = "nullable")]
    pub ends_text: String,
    /// Who renews it.
    #[serde(deserialize_with = "nullable")]
    pub renewer: String,
    /// `ok`, `expiring`, `needs_you`, `ended` or `unknown`.
    #[serde(deserialize_with = "nullable")]
    pub health: String,
    /// The health in words.
    #[serde(deserialize_with = "nullable")]
    pub health_text: String,
    /// The last renewal in words, when one is known.
    #[serde(deserialize_with = "nullable")]
    pub last_renewal: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_text_reads_empty_and_an_unknown_end_stays_unknown() {
        let view: LoginsView = serde_json::from_str(
            r#"{"version":1,"generatedAt":null,"rows":[{"id":"a","account":null,"endsAt":null}],"notes":null}"#,
        )
        .expect("test value");
        assert_eq!(view.rows[0].account, "");
        assert_eq!(view.rows[0].ends_at, None);
        assert!(view.notes.is_empty());
    }
}
