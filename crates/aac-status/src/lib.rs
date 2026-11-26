//! AI Account Center status readers, as a native module like crates/aac-store (owner rule
//! 2026-11-08: every new server piece is Rust; the TypeScript server stays as it is).
//!
//! Status readers: T3's pinned threads from its state database, opened read-only, the
//! `agent-reports/status@1` documents agents write on the share, and (R4, DESIGN-M3-COMPLETE
//! 1.2/1.4) the share scanners themselves: agent reports, the quota report, the pm-mail
//! registry, status documents and the Scoreboard catalog, under the same list-first,
//! last-good-copy, `seq`-ordering rules the TypeScript readers kept. Routes and
//! authentication stay in the TypeScript server. Reset inventory, guarded claim
//! coordination and the profile-bound Codex transport also live here; only their route and
//! collector adapters are TypeScript.
//!
//! There is exactly one secret policy (`src/shared/secret-policy.json`), compiled into both
//! the TypeScript readers and this crate (`secret_policy`, shared with aac-store). The R4
//! owned readers apply it themselves, as `jsonFromBytes` does; the NAPI `parseStatus` path
//! still receives only reports TypeScript accepted, and the check is idempotent there.

pub mod answer_delivery;
pub mod answer_signing;
pub mod answers_engine;
pub mod computers;
pub mod error;
pub mod http_views;
pub mod model_identity;
pub mod ops;
pub mod pinned;
pub mod pins;
pub mod produce;
#[allow(missing_docs)]
pub mod read_context;
#[allow(missing_docs)]
pub mod read_measures;
#[allow(missing_docs)]
pub mod read_scoreboard;
#[allow(missing_docs)]
pub mod read_status;
pub mod reset;
pub mod reset_claude_source;
pub mod reset_inventory;
pub mod reset_rules;
pub mod reset_source;
pub mod reset_zai;
pub mod scoreboard;
pub mod share;
pub mod spend;
pub mod status_doc;
pub mod validate;
pub mod writer_lock;

#[cfg(feature = "node")]
mod node;

#[path = "../../../native/shared/run_corrections.rs"]
pub mod run_corrections;

#[path = "../../../native/shared/secret_policy.rs"]
pub mod secret_policy;
/// The typed-secret check for the owner's notes; with `secret_policy`, the one secret filter.
pub mod typed_secrets;
