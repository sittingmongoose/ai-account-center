//! Private service HTTP core and opt-in M6 dashboard front door.
//! The collection, settings and account-read owners build on every platform the dashboard does;
//! the private Unix protocol and the services it alone serves stay on Unix.
mod account_approvals;
mod account_reads;
pub mod account_usage;
mod agent_transport;
#[cfg(unix)]
mod analytics_writers;
#[cfg(unix)]
mod answers_owned;
pub mod antigravity_host;
pub mod antigravity_probes;
pub mod antigravity_quota;
pub mod antigravity_renewal;
pub mod antigravity_runtime;
pub mod app_update_routes;
pub mod app_updates;
mod claude_account_store;
pub mod claude_lifecycle;
pub mod claude_open;
pub mod cli;
mod collection;
mod collection_cache;
mod dashboard_fed;
pub mod dashboard_observations;
mod feed;
#[cfg(unix)]
mod home_fed;
#[cfg(unix)]
mod home_producer;
mod identity;
mod json_object;
pub mod key_lifecycle;
mod lifecycle_effect_pipe;
pub mod lifecycle_maintenance;
pub mod lifecycle_policy;
pub mod lifecycle_queue;
pub mod lifecycle_recheck;
pub mod lifecycle_signin;
mod limits;
/// Cross-platform: the models.dev fetch job G14's scheduling will run from the service.
pub mod models_dev;
mod preferences_write;
mod profile_reads;
pub mod public_auth;
#[cfg(unix)]
mod read_files;
#[cfg(unix)]
mod read_routes;
mod registered;
mod reset_claims;
mod reset_source_route;
#[cfg(all(unix, test))]
mod runs_ingest_gate;
#[cfg(unix)]
mod scoreboard_owned;
pub mod signin_callbacks;
mod signins;
mod status_cache;
mod status_fed;
#[cfg(unix)]
mod status_owned;
#[cfg(unix)]
mod status_produce;
#[cfg(unix)]
mod status_run;
mod store_host;
mod table;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
mod usage_snapshot;
pub mod view_reply;
mod visibility_write;
pub use identity::PROTOCOL;
#[cfg(unix)]
pub use unix::{
    Boot, NativeAccountUsage, serve, serve_composed, serve_with_account_usage_cache,
    serve_with_app_update_jobs, serve_with_claude_open, serve_with_native_account_usage,
    short_name,
};
