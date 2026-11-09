//! AI Account Center status readers, as a native module like native/aac-store (owner rule
//! 2026-11-08: every new server piece is Rust; the TypeScript server stays as it is).
//!
//! Readers only: T3's pinned threads from its state database, opened read-only, and the
//! `agent-reports/status@1` documents agents write on the share. Routes, authentication,
//! share scanning (list first, last good copy, `seq` ordering) and every product rule stay
//! in the TypeScript server; this crate validates and projects, it never guesses.
//!
//! TypeScript checks status reports for secrets; the native module only reads reports
//! TypeScript accepted (owner decision 2026-11-09). There is exactly one secret filter,
//! the TypeScript one (`jsonFromBytes` in src/web-server/status/schema-helpers.ts); this
//! crate keeps no copy of it.

pub mod error;
pub mod ops;
pub mod pinned;
pub mod pins;
pub mod status_doc;
pub mod validate;

#[cfg(feature = "node")]
mod node;
