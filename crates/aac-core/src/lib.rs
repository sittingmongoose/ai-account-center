//! Small shared contracts: validated identifiers, honest readings and bounded, scrubbed failures.

mod failure;
mod ids;
mod json;
mod wire;

// Include the identical implementation used by aac_store::secret_policy. No store engine,
// Node binding or second credential detector is pulled into the foundation or WASM graph.
#[allow(dead_code, clippy::unwrap_used)]
#[rustfmt::skip]
#[path = "../../../native/shared/secret_policy.rs"]
mod secret_policy;

pub use failure::{Cause, Failure, Reading, Retry, Scrubbed, UtcMs, scrub};
pub use ids::{
    AccountKey, ComputerId, EventId, ProviderId, SessionHash, SessionKey, ToolId, sha256_hex,
};
pub use json::{bounded_json, bounded_read};
pub use wire::{WireWord, wire_word, wire_words};
