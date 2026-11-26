//! Codex update process runtime, ported from the update-only TypeScript closure.
//! No account activation, auth writes, CLI fallback, or persisted launch contexts.
//! Consumers retain a Runtime across stop/install/start and call dispose on failure.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
#[cfg(target_os = "linux")]
mod native;
mod process;
mod runtime;
#[cfg(target_os = "linux")]
pub use native::{Native, read_processes, rollout_lifecycle};
pub use process::{Process, systemd_unit_of};
use process::{descendants, desktop_launcher};
pub use runtime::{Backend, Identity, Launch, RestartPlan, RestartState, Runtime};
/// Sanitized runtime failures; private subprocess output never enters errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// Work is running or its idle state cannot be proven.
    Busy,
    /// A safe shutdown, launch, inventory, or readiness check failed.
    RestartFailed,
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Busy => "Codex is busy or idle status is unavailable.",
            Self::RestartFailed => "Codex restart failed.",
        })
    }
}
impl std::error::Error for Error {}
/// Sanitized runtime result.
pub type Result<T> = std::result::Result<T, Error>;
/// Chromium switches, with duplicate and terminator semantics.
pub fn chromium_switches(argv: &[String]) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for word in argv.iter().skip(1) {
        let word =
            word.trim_matches(|c: char| matches!(c, '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' '));
        if word == "--" {
            break;
        }
        let prefix = if word.starts_with("--") {
            2
        } else if word.starts_with('-') {
            1
        } else {
            0
        };
        if prefix == 0 || prefix == word.len() {
            continue;
        }
        let (key, value) = word[prefix..]
            .split_once('=')
            .unwrap_or((&word[prefix..], ""));
        out.insert(key.into(), value.into());
    }
    out
}
#[cfg(test)]
mod tests;
