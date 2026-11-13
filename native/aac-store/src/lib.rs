//! AI Account Center storage library (docs/store.md). A storage library only: it appends,
//! projects, queries and maintains. Routes, authentication, collectors and every product rule
//! stay in the TypeScript server.

// Tuples of paths, hashes and sizes read better inline than as one-use type aliases.
#![allow(clippy::type_complexity)]

pub mod activity;
pub mod capture;
pub mod contract;
pub mod db;
pub mod engine;
pub mod error;
pub mod event;
pub mod frame;
pub mod fsx;
pub mod legacy;
pub mod log;
pub mod msgpack;
pub mod ops;
pub mod pack;
#[path = "../../shared/secret_policy.rs"]
pub mod secret_policy;
// Compatibility for callers from the original capture implementation.
pub use secret_policy as report_secret;
pub mod secret;
pub mod storage_health;
pub mod usage;

#[cfg(feature = "node")]
mod node;
