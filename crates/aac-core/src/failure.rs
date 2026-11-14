use crate::secret_policy;
use serde::{Deserialize, Deserializer, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// A nonnegative UTC millisecond timestamp exactly representable by JavaScript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub struct UtcMs(u64);

impl UtcMs {
    /// Reject timestamps beyond JavaScript's exact integer range.
    pub fn new(value: u64) -> Result<Self, Failure> {
        if value > 9_007_199_254_740_991 {
            return Err(Failure::new(
                Cause::Damaged,
                "timestamp outside exact integer range",
            ));
        }
        Ok(Self(value))
    }

    /// Return milliseconds since the Unix epoch.
    pub fn get(self) -> u64 {
        self.0
    }

    /// Read the wall clock; pre-epoch clocks are reported rather than guessed.
    pub fn now() -> Result<Self, Failure> {
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Failure::new(Cause::Bug, "wall clock precedes epoch"))?;
        let value = u64::try_from(time.as_millis())
            .map_err(|_| Failure::new(Cause::Bug, "wall clock overflow"))?;
        Self::new(value)
    }
}

impl<'de> Deserialize<'de> for UtcMs {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(u64::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Stable snake-case failure classes; feature success and progress states stay separate.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Cause {
    /// The SSH transport could not reach its peer.
    SshUnreachable,
    /// A deadline expired.
    Timeout,
    /// The pinned host key was rejected.
    HostKeyMismatch,
    /// No verified agent exists in the managed cache.
    AgentMissing,
    /// An agent hash, target, name or protocol did not match.
    AgentMismatch,
    /// A bounded input or output exceeded its cap.
    OutputTooLarge,
    /// The caller cancelled the operation.
    Cancelled,
    /// The requested tool is absent.
    NotInstalled,
    /// The installed tool has no readable usage logs.
    InstalledNoLogs,
    /// An operating-system permission check failed.
    PermissionDenied,
    /// A database is temporarily locked.
    DbBusy,
    /// Input is malformed or corrupt.
    Damaged,
    /// A destination cannot be written.
    NotWritable,
    /// A running application holds a resource.
    InUse,
    /// An explicit policy prevents the operation.
    PolicyBlocked,
    /// Provider authentication is required.
    AuthRequired,
    /// An interactive sign-in is required.
    SigninNeeded,
    /// The provider imposed a rate limit.
    RateLimited,
    /// The provider did not supply verifiable identity.
    IdentityUnavailable,
    /// Provider identity disagrees with the intended account.
    IdentityMismatch,
    /// Provider team disagrees with the intended team.
    TeamMismatch,
    /// The provider rejected or could not complete the request.
    ProviderError,
    /// A resource is occupied by another operation.
    Busy,
    /// The target is currently active.
    NotIdle,
    /// A reviewed release policy holds the operation.
    Held,
    /// A confirmation is not bound to this request.
    ConfirmationInvalid,
    /// A confirmation expired.
    ConfirmationExpired,
    /// The boundary refused unsupported or unsafe input.
    Refused,
    /// An internal invariant failed.
    Bug,
}

/// Retry intent only; callers must separately establish that a request is idempotent.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "delayMs", rename_all = "snake_case")]
pub enum Retry {
    /// Never retry automatically.
    Never,
    /// Wait at least this many milliseconds.
    After(u64),
    /// Apply the caller's bounded exponential backoff policy.
    Backoff,
}

/// Bounded safe text. Deserialization applies the same secret policy as construction.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Scrubbed(String);

impl Scrubbed {
    /// Borrow text already checked at the boundary.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for Scrubbed {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(scrub(&String::deserialize(deserializer)?))
    }
}

/// Refuse the entire detail when the shared policy detects a secret, embedded credential or
/// uncheckable JSON. Text above 4096 bytes is refused before scanning; controls are removed.
pub fn scrub(text: &str) -> Scrubbed {
    if text.len() > 4096 {
        return Scrubbed("[detail exceeded limit]".into());
    }
    let normalized: String = text.chars().filter(|c| !c.is_control()).collect();
    if [text, normalized.as_str()].iter().any(|candidate| {
        let parsed = serde_json::from_str(candidate)
            .unwrap_or_else(|_| serde_json::Value::String((*candidate).into()));
        secret_policy::find_secret_in(candidate, &parsed).is_some()
    }) {
        return Scrubbed("[redacted]".into());
    }
    Scrubbed(normalized)
}

/// A scrubbed failure with explicit retry intent and an optional known observation time.
/// Clock failures leave `at` absent; no fabricated epoch is recorded.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Failure {
    /// Stable failure class.
    pub cause: Cause,
    /// Optional scrubbed feature-specific reason, preserved separately from the class.
    pub reason: Option<Scrubbed>,
    /// Safe bounded diagnostic text.
    pub detail: Scrubbed,
    /// Automatic retry intent; defaults conservatively to never.
    pub retry: Retry,
    /// Known failure observation time, or absent if the caller has no clock.
    pub at: Option<UtcMs>,
}

impl Failure {
    /// Construct a failure without reading a process-global clock or guessing a timestamp.
    pub fn new(cause: Cause, detail: &str) -> Self {
        Self {
            cause,
            reason: None,
            detail: scrub(detail),
            retry: Retry::Never,
            at: None,
        }
    }
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.cause, self.detail.as_str())
    }
}
impl std::error::Error for Failure {}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        use std::io::ErrorKind;
        let cause = match error.kind() {
            ErrorKind::PermissionDenied => Cause::PermissionDenied,
            ErrorKind::NotFound => Cause::NotInstalled,
            ErrorKind::TimedOut => Cause::Timeout,
            ErrorKind::WouldBlock => Cause::Busy,
            ErrorKind::InvalidData => Cause::Damaged,
            _ => Cause::NotWritable,
        };
        // OS error text can contain private paths. Preserve classification without it.
        Self::new(cause, "filesystem or process operation failed")
    }
}

/// A reading cannot carry a value when unavailable; cached values retain their true time.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum Reading<T> {
    /// A successful observation at its source time.
    Live {
        /// Measured value, including a genuine zero.
        value: T,
        /// Source observation time.
        observed_at: UtcMs,
    },
    /// A previously measured value retained after a failure.
    Cached {
        /// Previously measured value.
        value: T,
        /// Original observation time, never the refresh time.
        observed_at: UtcMs,
        /// Why a fresh measurement could not be obtained.
        because: Failure,
    },
    /// No measurement exists; there is deliberately no value field.
    Unavailable {
        /// The failure preventing measurement.
        failure: Failure,
    },
}
