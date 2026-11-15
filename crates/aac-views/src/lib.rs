//! Read-route projections composed in Rust (M3) and screen view models (D7). Each one ports a pure
//! JavaScript composition and refuses every input whose JavaScript meaning it has not proven, so the
//! TS bridge serves its own answer instead (for Home: the page builds its own view). Inputs are what
//! the guarded TS handler already read from its owners; engine facts (`Date.parse`, `toISOString`,
//! `localeCompare`, display-zone clock and day words) come from the shared M3 presentation envelope
//! (`aac_status::read_context`). The specs: the M3 parity corpora, and `tests/spec/views/home` for Home.
pub mod analytics;
pub mod antigravity;
pub mod claude_desktop;
pub mod codex_profiles;
pub mod decline;
pub mod home;
pub mod js;
pub mod registry;
pub mod spread;
