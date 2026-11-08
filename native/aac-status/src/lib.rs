//! AI Account Center status readers, as a native module like native/aac-store (owner rule
//! 2026-11-08: every new server piece is Rust; the TypeScript server stays as it is).
//!
//! Readers only: T3's pinned threads from its state database, opened read-only, and the
//! `agent-reports/status@1` documents agents write on the share. Routes, authentication,
//! share scanning (list first, last good copy, `seq` ordering) and every product rule stay
//! in the TypeScript server; this crate validates and projects, it never guesses.

pub mod error;
pub mod ops;
pub mod pins;
pub mod secret;
pub mod status_doc;
pub mod validate;

#[cfg(feature = "node")]
mod node;
