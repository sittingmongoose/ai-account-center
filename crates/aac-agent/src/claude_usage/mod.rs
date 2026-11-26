//! The Claude account usage port of `scripts/account-usage/claude_usage.py`: quota
//! normalization and plan names, the saved Desktop and Code logins, the bounded OAuth client,
//! and the private reset host (capture, read and the one confirmed claim). Tokens stay in this
//! process; nothing refreshes or writes a login, follows a redirect, or emits a provider body.
pub(crate) mod desktop;
pub(crate) mod host;
pub(crate) mod http;
pub(crate) mod keychain;
pub(crate) mod keychain_bridge;
pub(crate) mod normalize;

/// `UsageUnavailable`: a fixed status word and an optional bounded provider delay.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Unavailable {
    pub status: &'static str,
    pub retry_after: Option<i64>,
}
impl Unavailable {
    pub(crate) const fn new(status: &'static str) -> Self {
        Self {
            status,
            retry_after: None,
        }
    }
}

pub(crate) fn now_ms() -> i64 {
    aac_core::UtcMs::now().map(|t| t.get() as i64).unwrap_or(0)
}

/// The home Claude's own files live under (`Path.home()`).
pub(crate) fn home() -> std::path::PathBuf {
    std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(std::path::PathBuf::from)
        .unwrap_or_default()
}
