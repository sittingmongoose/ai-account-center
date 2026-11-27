//! Compact typed share readers and readers-check writer for owned Status and Scoreboard
//! (MEM7 Lane 4a).
//!
//! Replaces retained `serde_json::Value` DOM trees, duplicated `IndexMap` strings, hex SHA
//! strings, and full `CheckRow` maps inside `aac-http` with compact typed structures while
//! keeping `crates/aac-status` (part of the pinned agent closure) untouched and maintaining
//! byte-for-byte parity with the `aac_status::share` reference readers.

pub(crate) mod catalog;
pub(crate) mod check;
#[cfg(test)]
mod parity_tests;
pub(crate) mod primitives;
pub(crate) mod quota;
pub(crate) mod registry;
pub(crate) mod reports;
pub(crate) mod status_docs;
