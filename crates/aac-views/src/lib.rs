//! Read-route projections composed in Rust (M3). Each one ports a pure TS composition and
//! refuses every input whose JavaScript meaning it has not proven, so the TS bridge serves its
//! own answer instead. Inputs are what the guarded TS handler already read from its owners;
//! engine facts (`Date.parse`, `toISOString`, `localeCompare`) come from the shared M3
//! presentation envelope (`aac_status::read_context`).
pub mod analytics;
pub mod antigravity;
pub mod claude_desktop;
pub mod codex_profiles;
pub mod js;
pub mod registry;
pub mod spread;
