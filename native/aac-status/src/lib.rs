//! AI Account Center status readers, as a native module like native/aac-store (owner rule
//! 2026-11-08: every new server piece is Rust; the TypeScript server stays as it is).
//!
//! Status readers: T3's pinned threads from its state database, opened read-only, and the
//! `agent-reports/status@1` documents agents write on the share. Routes, authentication,
//! share scanning (list first, last good copy, `seq` ordering) stay in the TypeScript
//! server. Reset inventory, guarded claim coordination and the profile-bound Codex
//! transport also live here; only their route and collector adapters are TypeScript.
//!
//! TypeScript checks status reports for secrets; the native module only reads reports
//! TypeScript accepted (owner decision 2026-11-09). There is exactly one secret filter,
//! the TypeScript one (`jsonFromBytes` in src/web-server/status/schema-helpers.ts); this
//! crate keeps no copy of it.

pub mod computers;
pub mod error;
pub mod ops;
pub mod pinned;
pub mod pins;
pub mod reset;
pub mod reset_inventory;
pub mod reset_source;
pub mod scoreboard;
pub mod spend;
pub mod status_doc;
pub mod validate;

#[cfg(feature = "node")]
mod node;
