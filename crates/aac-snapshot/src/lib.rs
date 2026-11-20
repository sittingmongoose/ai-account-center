//! The aac-usage share snapshot writer (cut gap G18): the TypeScript
//! `src/web-server/agent-usage/*` pipeline in Rust, byte-equal on the same inputs.
//!
//! Every agent on every machine reads `usage.json` / `usage.md` / `history.json` from the share
//! folder the owner configured. This crate owns the whole publication: the allowlist projection
//! of dashboard rows, the measured burn rates and rolling history, the plain-words rendering,
//! the publish-time sign-in deadline recheck and the one-write-at-a-time publish loop. File
//! mechanics live in `aac-files` (the one file policy); the service wires the deps.
//!
//! Parity rules: numbers and dates go through `aac-num` (ECMA-262), JSON text through
//! `aac-views::js_json` (`JSON.stringify` byte for byte), and every screening guard is the
//! TypeScript one, including its UTF-16 length counts. Inputs stay `serde_json::Value` so a
//! malformed field screens to null exactly as the TS type guards do, never a parse refusal.

pub mod client;
pub mod exports;
pub mod history;
pub mod js;
pub mod markdown;
pub mod projection;
pub mod publisher;
pub mod renewal;
pub mod settings;
pub mod snapshot;

pub use projection::{ProjectedAccount, ProjectedWindow, project_agent_usage};
pub use publisher::{Deps, SnapshotPublisher};
pub use settings::SnapshotSettings;
pub use snapshot::{SampleEvent, build_snapshot_files, downgrade_crossed_sign_in_rows};

/// `usage.json`, the commit point of each publish pass.
pub const SNAPSHOT_JSON: &str = "usage.json";
/// `usage.md`, the same snapshot in plain words.
pub const SNAPSHOT_MARKDOWN: &str = "usage.md";
/// `history.json`, the rolling 7-day readings.
pub const SNAPSHOT_HISTORY: &str = "history.json";
/// `locations.json`, where each group of computers finds the folder.
pub const SNAPSHOT_LOCATIONS: &str = "locations.json";
/// The store's hourly rollup export, written once an hour.
pub const SNAPSHOT_HISTORY_HOURLY: &str = "history-hourly.json";
/// The store's daily rollup export, written once an hour.
pub const SNAPSHOT_HISTORY_DAILY: &str = "history-daily.json";
/// The rolling history span: 7 days.
pub const HISTORY_HOURS: i64 = 7 * 24;
