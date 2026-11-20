//! Reason codes, states and plain ASCII messages for saved-login renewal
//! (`src/codex-auth/codex-renewal-types.ts`). No secrets.

use std::collections::HashMap;

/// Every renewal outcome code, in the TS union's order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexRenewalReason {
    /// Not due yet.
    Fresh,
    /// The renewal wrote rotated tokens.
    Renewed,
    /// Renew at the next check.
    Due,
    /// A refresh is in flight right now.
    Renewing,
    /// OpenAI unreachable or busy; retry with backoff.
    Transient,
    /// OpenAI sent an unexpected answer; retry.
    InvalidResponse,
    /// The renewed login could not be saved; retry.
    WriteFailed,
    /// OpenAI rejected the saved login.
    Dead,
    /// OpenAI answered for a different account.
    IdentityMismatch,
    /// Renewal is turned off.
    Disabled,
    /// No saved login with a renewal token.
    NoLogin,
    /// The file is a link, too large or unreadable.
    UnsafeFile,
    /// The login Codex is using now.
    Live,
    /// The live login could not be read.
    LiveUnverifiable,
    /// No session details to check for copies.
    Unverifiable,
    /// Running Codex apps could not be checked.
    ProcessScanFailed,
    /// A running Codex uses this profile folder.
    InUse,
    /// Another login is a copy of this one.
    SharedFamily,
    /// Another login could not be ruled out.
    FamilyUnverifiable,
    /// An activation, sign-in or removal held the lock.
    ActivationBusy,
    /// The file changed during the renewal.
    Changed,
}

impl CodexRenewalReason {
    /// The wire code, as the TS string union spells it.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Fresh => "fresh",
            Self::Renewed => "renewed",
            Self::Due => "due",
            Self::Renewing => "renewing",
            Self::Transient => "transient",
            Self::InvalidResponse => "invalid_response",
            Self::WriteFailed => "write_failed",
            Self::Dead => "dead",
            Self::IdentityMismatch => "identity_mismatch",
            Self::Disabled => "disabled",
            Self::NoLogin => "no_login",
            Self::UnsafeFile => "unsafe_file",
            Self::Live => "live",
            Self::LiveUnverifiable => "live_unverifiable",
            Self::Unverifiable => "unverifiable",
            Self::ProcessScanFailed => "process_scan_failed",
            Self::InUse => "in_use",
            Self::SharedFamily => "shared_family",
            Self::FamilyUnverifiable => "family_unverifiable",
            Self::ActivationBusy => "activation_busy",
            Self::Changed => "changed",
        }
    }

    /// Parse a recorded outcome code. The status store keeps any string the TS
    /// reader accepted; unknown codes stay `None` and never match a guard set.
    pub fn from_code(code: &str) -> Option<Self> {
        Some(match code {
            "fresh" => Self::Fresh,
            "renewed" => Self::Renewed,
            "due" => Self::Due,
            "renewing" => Self::Renewing,
            "transient" => Self::Transient,
            "invalid_response" => Self::InvalidResponse,
            "write_failed" => Self::WriteFailed,
            "dead" => Self::Dead,
            "identity_mismatch" => Self::IdentityMismatch,
            "disabled" => Self::Disabled,
            "no_login" => Self::NoLogin,
            "unsafe_file" => Self::UnsafeFile,
            "live" => Self::Live,
            "live_unverifiable" => Self::LiveUnverifiable,
            "unverifiable" => Self::Unverifiable,
            "process_scan_failed" => Self::ProcessScanFailed,
            "in_use" => Self::InUse,
            "shared_family" => Self::SharedFamily,
            "family_unverifiable" => Self::FamilyUnverifiable,
            "activation_busy" => Self::ActivationBusy,
            "changed" => Self::Changed,
            _ => return None,
        })
    }

    /// The fixed dashboard message for this code.
    pub const fn message(self) -> &'static str {
        match self {
            Self::Fresh => {
                "The saved login is current. AAC renews it about 4 days before it expires."
            }
            Self::Renewed => "AAC renewed the saved login.",
            Self::Due => "The saved login is due for renewal at the next check.",
            Self::Renewing => "AAC is renewing the saved login now.",
            Self::Transient => {
                "OpenAI could not be reached or was busy. AAC will try again automatically."
            }
            Self::InvalidResponse => {
                "OpenAI sent an unexpected answer. AAC will try again automatically."
            }
            Self::WriteFailed => {
                "The renewed login could not be saved. AAC will try again; if OpenAI then rejects it, use Sign in again."
            }
            Self::Dead => "OpenAI rejected this saved login. Use Sign in again to restore it.",
            Self::IdentityMismatch => {
                "OpenAI answered for a different account, so nothing was saved. Use Sign in again to restore this login."
            }
            Self::Disabled => {
                "Automatic renewal of saved Codex logins is turned off (CCS_CODEX_RENEWAL=0)."
            }
            Self::NoLogin => "This profile has no saved Codex login with a renewal token.",
            Self::UnsafeFile => {
                "The saved login file is a link, too large or unreadable, so AAC does not rewrite it."
            }
            Self::Live => "This is the login Codex is using now. Codex renews it itself.",
            Self::LiveUnverifiable => {
                "The current Codex login could not be read, so AAC cannot tell which saved login is live."
            }
            Self::Unverifiable => {
                "This saved login carries no session details, so AAC cannot check it for copies elsewhere."
            }
            Self::ProcessScanFailed => "Running Codex apps could not be checked safely.",
            Self::InUse => {
                "A running Codex app uses this saved profile folder directly and renews it itself."
            }
            Self::SharedFamily => {
                "Another Codex login on this machine is a copy of this saved login. Renewing one copy would sign the other out, so AAC leaves both alone. Sign in again on one of them to separate them."
            }
            Self::FamilyUnverifiable => {
                "Another Codex login on this machine could not be checked, so AAC cannot rule out a shared copy."
            }
            Self::ActivationBusy => {
                "A Codex account switch, sign-in or removal was running. AAC will try again shortly."
            }
            Self::Changed => {
                "The saved login changed while AAC was renewing it, so AAC kept the new file."
            }
        }
    }
}

/// The dashboard state word for an outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexRenewalState {
    /// Current or renewed.
    Ok,
    /// Due for renewal.
    Due,
    /// Being renewed now.
    Renewing,
    /// Waiting out a backoff.
    Retrying,
    /// OpenAI rejected it; Sign in again restores it.
    Failed,
    /// A guard leaves it alone.
    Skipped,
}

impl CodexRenewalState {
    /// The wire word.
    pub const fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Due => "due",
            Self::Renewing => "renewing",
            Self::Retrying => "retrying",
            Self::Failed => "failed",
            Self::Skipped => "skipped",
        }
    }
}

/// OpenAI rejected the saved login; only Sign in again restores it.
pub const fn is_dead_reason(reason: CodexRenewalReason) -> bool {
    matches!(
        reason,
        CodexRenewalReason::Dead | CodexRenewalReason::IdentityMismatch
    )
}

/// AAC tries again with exponential backoff.
pub const fn is_retry_reason(reason: CodexRenewalReason) -> bool {
    matches!(
        reason,
        CodexRenewalReason::Transient
            | CodexRenewalReason::InvalidResponse
            | CodexRenewalReason::WriteFailed
    )
}

/// `codexRenewalState`: the state word a reason maps to.
pub const fn codex_renewal_state(reason: CodexRenewalReason) -> CodexRenewalState {
    match reason {
        CodexRenewalReason::Fresh | CodexRenewalReason::Renewed => CodexRenewalState::Ok,
        CodexRenewalReason::Due => CodexRenewalState::Due,
        CodexRenewalReason::Renewing => CodexRenewalState::Renewing,
        r if is_retry_reason(r) => CodexRenewalState::Retrying,
        r if is_dead_reason(r) => CodexRenewalState::Failed,
        _ => CodexRenewalState::Skipped,
    }
}

/// The fixed message table, keyed by wire code (parity exports read it).
pub const CODEX_RENEWAL_MESSAGES: &[(&str, &str)] = &[
    ("fresh", CodexRenewalReason::Fresh.message()),
    ("renewed", CodexRenewalReason::Renewed.message()),
    ("due", CodexRenewalReason::Due.message()),
    ("renewing", CodexRenewalReason::Renewing.message()),
    ("transient", CodexRenewalReason::Transient.message()),
    (
        "invalid_response",
        CodexRenewalReason::InvalidResponse.message(),
    ),
    ("write_failed", CodexRenewalReason::WriteFailed.message()),
    ("dead", CodexRenewalReason::Dead.message()),
    (
        "identity_mismatch",
        CodexRenewalReason::IdentityMismatch.message(),
    ),
    ("disabled", CodexRenewalReason::Disabled.message()),
    ("no_login", CodexRenewalReason::NoLogin.message()),
    ("unsafe_file", CodexRenewalReason::UnsafeFile.message()),
    ("live", CodexRenewalReason::Live.message()),
    (
        "live_unverifiable",
        CodexRenewalReason::LiveUnverifiable.message(),
    ),
    ("unverifiable", CodexRenewalReason::Unverifiable.message()),
    (
        "process_scan_failed",
        CodexRenewalReason::ProcessScanFailed.message(),
    ),
    ("in_use", CodexRenewalReason::InUse.message()),
    ("shared_family", CodexRenewalReason::SharedFamily.message()),
    (
        "family_unverifiable",
        CodexRenewalReason::FamilyUnverifiable.message(),
    ),
    (
        "activation_busy",
        CodexRenewalReason::ActivationBusy.message(),
    ),
    ("changed", CodexRenewalReason::Changed.message()),
];

/// `CCS_CODEX_RENEWAL=0` (or false/off/no, trimmed and lowercased) turns renewal off.
pub fn is_codex_profile_renewal_enabled(env: &HashMap<String, String>) -> bool {
    let value = env
        .get("CCS_CODEX_RENEWAL")
        .map(|v| v.trim().to_lowercase());
    !matches!(
        value.as_deref(),
        Some("0") | Some("false") | Some("off") | Some("no")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disable_words_match_the_ts_reader() {
        let mut env = HashMap::new();
        assert!(is_codex_profile_renewal_enabled(&env));
        for off in ["0", "false", "off", "no", " FALSE ", "Off", "NO"] {
            env.insert("CCS_CODEX_RENEWAL".into(), off.into());
            assert!(!is_codex_profile_renewal_enabled(&env), "{off}");
        }
        for on in ["1", "true", "on", "yes", "", "  "] {
            env.insert("CCS_CODEX_RENEWAL".into(), on.into());
            assert!(is_codex_profile_renewal_enabled(&env), "{on}");
        }
    }

    #[test]
    fn state_words_cover_every_reason() {
        for (code, message) in CODEX_RENEWAL_MESSAGES {
            let reason = CodexRenewalReason::from_code(code).expect("known code");
            assert_eq!(reason.message(), *message);
            let _ = codex_renewal_state(reason).word();
        }
        assert_eq!(
            codex_renewal_state(CodexRenewalReason::Dead).word(),
            "failed"
        );
        assert_eq!(
            codex_renewal_state(CodexRenewalReason::Transient).word(),
            "retrying"
        );
        assert_eq!(
            codex_renewal_state(CodexRenewalReason::InUse).word(),
            "skipped"
        );
    }
}
