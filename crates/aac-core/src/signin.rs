//! Shared sign-in job identifiers and public errors for the service and agent.
use serde_json::{Value, json};

/// The job error codes (`SignInJobErrorCode`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ErrorCode {
    /// The sign-in tool is absent.
    ToolMissing,
    /// An unrecognized answer.
    UnexpectedOutput,
    /// Another account's sign-in.
    IdentityMismatch,
    /// An account already saved elsewhere.
    DuplicateIdentity,
    /// The overall timeout.
    Timeout,
    /// The CLI refused or its code expired.
    ProviderDenied,
    /// A safe save failed.
    WriteFailed,
    /// The live login could not be updated.
    LiveLoginNotUpdated,
    /// The server restarted.
    ServerRestarted,
}

impl ErrorCode {
    /// Convert the public failure code to its fixed message class.
    pub fn from_word(word: &str) -> Option<Self> {
        [
            Self::ToolMissing,
            Self::UnexpectedOutput,
            Self::IdentityMismatch,
            Self::DuplicateIdentity,
            Self::Timeout,
            Self::ProviderDenied,
            Self::WriteFailed,
            Self::LiveLoginNotUpdated,
            Self::ServerRestarted,
        ]
        .into_iter()
        .find(|kind| kind.word() == word)
    }

    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            ErrorCode::ToolMissing => "tool_missing",
            ErrorCode::UnexpectedOutput => "unexpected_output",
            ErrorCode::IdentityMismatch => "identity_mismatch",
            ErrorCode::DuplicateIdentity => "duplicate_identity",
            ErrorCode::Timeout => "timeout",
            ErrorCode::ProviderDenied => "provider_denied",
            ErrorCode::WriteFailed => "write_failed",
            ErrorCode::LiveLoginNotUpdated => "live_login_not_updated",
            ErrorCode::ServerRestarted => "server_restarted",
        }
    }

    /// The fixed message (`SIGNIN_ERROR_MESSAGES`).
    pub fn message(self) -> &'static str {
        match self {
            ErrorCode::ToolMissing => "The sign-in tool is not installed on this server.",
            ErrorCode::UnexpectedOutput => {
                "The sign-in tool answered in a way this page does not recognize. Nothing was changed."
            }
            ErrorCode::IdentityMismatch => {
                "That sign-in belongs to a different account. Nothing was changed."
            }
            ErrorCode::DuplicateIdentity => {
                "That account is already saved in another profile. Nothing was changed."
            }
            ErrorCode::Timeout => "The sign-in was not finished in time. Nothing was changed.",
            ErrorCode::ProviderDenied => {
                "The sign-in was not approved, or its code expired. Nothing was changed."
            }
            ErrorCode::WriteFailed => {
                "The new sign-in could not be saved safely. Nothing was changed."
            }
            ErrorCode::LiveLoginNotUpdated => {
                "The new sign-in was saved to its profile, but the live login on this computer could not be updated. Sign in again."
            }
            ErrorCode::ServerRestarted => {
                "The server restarted, so this sign-in stopped. Start it again."
            }
        }
    }
}

/// The authorization code grammar (`SIGNIN_CODE_PATTERN`): URL-safe plus `/`, one line.
pub fn valid_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 2048
        && code
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._~/-".contains(&b))
}

/// The job id grammar (`JOB_ID_PATTERN`).
pub fn valid_job_id(id: &str) -> bool {
    id.len() == 20
        && id.starts_with("job_")
        && id[4..]
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// Public job DTO for a valid id unknown to the current boot.
pub fn restarted_placeholder(id: &str, updated_at: Option<String>) -> Value {
    json!({
        "id": id,
        "provider": Value::Null,
        "kind": Value::Null,
        "mode": Value::Null,
        "accountId": Value::Null,
        "profileName": Value::Null,
        "platform": Value::Null,
        "state": "failed",
        "verification": Value::Null,
        "result": Value::Null,
        "error": {
            "code": ErrorCode::ServerRestarted.word(),
            "message": ErrorCode::ServerRestarted.message(),
        },
        "startedAt": Value::Null,
        "updatedAt": updated_at,
        "expiresAt": Value::Null,
    })
}
