//! Saved-login renewal (cut gap G10): the Codex idle saved-login renewal timer,
//! the Antigravity live-login renewal guards and the Claude renewal reader,
//! ported from `src/codex-auth/codex-*renewal*`, `src/antigravity/live-login-renewal.ts`
//! and `src/web-server/services/claude-signin-renewal.ts`.
//!
//! Every path is injected (the service boot supplies the CCS root and the Codex
//! homes); nothing here discovers a real home on its own. Credentials stay in
//! memory: plan entries, attempts, status files and logs carry names, codes and
//! timestamps only. The TypeScript path stays the fallback until the cut.

// The port mirrors the TS fixed-message failures: callers branch on success or
// failure only, never on a cause (no detail may leak credential paths or text).
#![allow(clippy::result_unit_err)]

pub mod antigravity;
pub mod atomic;
pub mod claude;
pub mod family;
pub mod jsnum;
pub mod jwt;
pub mod lock;
pub mod notes;
#[cfg(feature = "parity-fixtures")]
pub mod parity;
pub mod planner;
pub mod procscan;
pub mod refresh;
pub mod registry;
pub mod renewal;
pub mod service;
pub mod status_store;
pub mod tokens;
pub mod types;

pub use types::{
    CODEX_RENEWAL_MESSAGES, CodexRenewalReason, CodexRenewalState, codex_renewal_state,
    is_codex_profile_renewal_enabled,
};
