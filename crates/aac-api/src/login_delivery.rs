//! Login delivery view v1 (Central Logins step 2): the static logins AAC has delivered, where each
//! one is, its fingerprint and when it ends, plus what the write-only form may offer.
//!
//! The server (`aac-http`'s login delivery routes) builds it from its fingerprint and expiry
//! registry and the agents' inventories; the Leptos Logins section draws it. **No type here has a
//! field that could carry a secret.** A fingerprint is `sha256:` and 16 hex digits of the
//! secret's SHA-256: it tells two secrets apart without revealing either.
use crate::home::nullable;
use serde::{Deserialize, Serialize};

/// The view version the page draws.
pub const DELIVERY_VIEW_VERSION: u32 = 1;

/// The delivery panel.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct DeliveryView {
    /// Always [`DELIVERY_VIEW_VERSION`].
    pub version: u32,
    /// When the view was built, ISO.
    #[serde(deserialize_with = "nullable")]
    pub generated_at: String,
    /// When the computers were last asked what they hold, ISO, if ever.
    pub checked_at: Option<String>,
    /// The computers a login can be delivered to.
    #[serde(deserialize_with = "nullable")]
    pub computers: Vec<DeliveryComputer>,
    /// The accounts a static login can belong to.
    #[serde(deserialize_with = "nullable")]
    pub accounts: Vec<DeliveryAccount>,
    /// The static kinds and the providers each one admits.
    #[serde(deserialize_with = "nullable")]
    pub kinds: Vec<DeliveryKind>,
    /// Every delivered static login.
    #[serde(deserialize_with = "nullable")]
    pub logins: Vec<DeliveredLogin>,
}

/// One computer.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DeliveryComputer {
    /// The computer's id (the transport's name for it).
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// Its name in words.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// `enrolled` (its identity is pinned), `new` (never checked: check the computers first) or
    /// `other` (another computer answered for it; trust it only if it was rebuilt or renamed).
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    /// The host name it reported when it was pinned, or the other one that answered.
    #[serde(deserialize_with = "nullable")]
    pub host: String,
    /// For `other`: the key of the computer that answered, which a trust confirmation names.
    #[serde(deserialize_with = "nullable")]
    pub seen_key: String,
}

/// One account a static login can belong to.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DeliveryAccount {
    /// The dashboard's account id.
    #[serde(deserialize_with = "nullable")]
    pub id: String,
    /// The provider id.
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    /// The account as the Accounts page shows it.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
}

/// A static kind.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DeliveryKind {
    /// `claude-token` or `api-key`.
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    /// The kind in words.
    #[serde(deserialize_with = "nullable")]
    pub label: String,
    /// The providers it admits.
    #[serde(deserialize_with = "nullable")]
    pub providers: Vec<String>,
}

/// One delivered static login.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DeliveredLogin {
    /// The registry's id.
    #[serde(deserialize_with = "nullable")]
    pub login_id: String,
    /// `claude-token` or `api-key`.
    #[serde(deserialize_with = "nullable")]
    pub kind: String,
    /// The provider id.
    #[serde(deserialize_with = "nullable")]
    pub provider: String,
    /// The account id.
    #[serde(deserialize_with = "nullable")]
    pub account: String,
    /// The account in words.
    #[serde(deserialize_with = "nullable")]
    pub account_label: String,
    /// The current generation's fingerprint; empty when the computers disagree and no delivery
    /// has settled which one is current.
    #[serde(deserialize_with = "nullable")]
    pub fingerprint: String,
    /// When it ends, ISO, when that is known.
    pub expires_at: Option<String>,
    /// The end in words.
    #[serde(deserialize_with = "nullable")]
    pub ends_text: String,
    /// `ok`, `expiring` (30 days or less), `ended` or `unknown`.
    #[serde(deserialize_with = "nullable")]
    pub health: String,
    /// Where it is.
    #[serde(deserialize_with = "nullable")]
    pub holders: Vec<LoginHolder>,
}

/// One computer that should hold a login.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct LoginHolder {
    /// The computer's id.
    #[serde(deserialize_with = "nullable")]
    pub computer: String,
    /// When it was delivered there, ISO.
    #[serde(deserialize_with = "nullable")]
    pub delivered_at: String,
    /// The fingerprint last seen there; empty when it could not be read.
    #[serde(deserialize_with = "nullable")]
    pub fingerprint: String,
    /// `ok` (holds the current generation), `differs` (holds another generation, named by
    /// `fingerprint`), `unreadable` (a file is there but cannot be read safely), `unchecked` (not
    /// reached at the last check), `other-account` (another account or kind under this id) or
    /// `wrong-computer` (another computer answered).
    #[serde(deserialize_with = "nullable")]
    pub state: String,
}

/// The answer to one delivery, copy or removal: one result per computer. Never a secret.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DeliveryReceipt {
    /// The registry's id.
    #[serde(deserialize_with = "nullable")]
    pub login_id: String,
    /// The fingerprint now recorded (empty after a removal from every computer).
    #[serde(deserialize_with = "nullable")]
    pub fingerprint: String,
    /// One per computer asked.
    #[serde(deserialize_with = "nullable")]
    pub results: Vec<DeliveryResult>,
}

/// One computer's result.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase")]
pub struct DeliveryResult {
    /// The computer's id.
    #[serde(deserialize_with = "nullable")]
    pub computer: String,
    /// `delivered`, `unchanged`, `removed`, `absent`, `refused`, `mismatch`, `changed`,
    /// `unreadable`, `unenrolled`, `wrong-computer` or `unreachable`.
    #[serde(deserialize_with = "nullable")]
    pub state: String,
    /// The state in words.
    #[serde(deserialize_with = "nullable")]
    pub text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_null_list_reads_empty() {
        let view: DeliveryView =
            serde_json::from_str(r#"{"version":1,"computers":null,"logins":[{"holders":null}]}"#)
                .expect("test value");
        assert!(view.computers.is_empty());
        assert!(view.logins[0].holders.is_empty());
    }
}
