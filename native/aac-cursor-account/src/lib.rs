//! AI Account Center's Cursor account reader (DESIGN 17 in the share folder
//! 2026-11-13-analytics-discovery): AAC's own Cursor API key, kept as a `credential` in AAC's key
//! store, traded for a token that reads the account's usage events and plan usage; the events not
//! accounted for by the local runs reader are the account's usage outside the scanned computers.
//! It runs on the dashboard's computer only and prints no key, token or account id.

pub mod api;
pub mod dedupe;
pub mod keys;
pub mod util;
