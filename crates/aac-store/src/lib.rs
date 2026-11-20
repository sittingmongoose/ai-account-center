//! AI Account Center storage library (docs/store.md). A storage library only: it appends,
//! projects, queries and maintains. Routes, authentication, collectors and every product rule
//! stay in the TypeScript server.

// Tuples of paths, hashes and sizes read better inline than as one-use type aliases.
#![allow(clippy::type_complexity)]

pub mod activity;
pub mod capture;
pub mod collect;
pub mod contract;
pub mod db;
pub mod engine;
pub mod error;
pub mod event;
pub mod frame;
pub mod fsx;
pub mod kimi_history;
pub mod legacy;
pub mod log;
pub mod msgpack;
pub mod observe;
pub mod ops;
pub mod pack;
pub mod read_status;
pub mod report;
pub mod run_identity;
pub mod run_schema;
pub mod runs;
pub mod sampler;
#[path = "../../../native/shared/secret_policy.rs"]
pub mod secret_policy;
// Compatibility for callers from the original capture implementation.
pub use secret_policy as report_secret;
pub mod secret;
pub mod storage_health;
pub mod usage;

#[cfg(feature = "node")]
mod node;

/// Binding contract version, shared by the service and the Node module.
pub const BINDING_VERSION: u32 = 2;

/// R3 handoff guard. Both store handles must carry this exact compiled identity.
pub fn build_identity() -> serde_json::Value {
    serde_json::json!({
        "commit": env!("AAC_BUILD_COMMIT"),
        "storeSchema": db::STORE_SCHEMA_VERSION,
        "bindingVersion": BINDING_VERSION,
    })
}

#[path = "../../../native/shared/run_corrections.rs"]
pub mod run_corrections;
