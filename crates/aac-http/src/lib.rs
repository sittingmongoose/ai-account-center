//! Private Unix-socket HTTP service and opt-in M6 dashboard front door.
#[cfg(unix)]
mod account_approvals;
#[cfg(unix)]
mod account_reads;
#[cfg(unix)]
pub mod account_usage;
#[cfg(unix)]
mod agent_transport;
#[cfg(unix)]
mod analytics_writers;
#[cfg(unix)]
mod answers_owned;
pub mod antigravity_host;
pub mod antigravity_probes;
pub mod antigravity_quota;
#[cfg(unix)]
pub mod antigravity_renewal;
pub mod antigravity_runtime;
#[cfg(unix)]
pub mod app_update_routes;
#[cfg(unix)]
pub mod app_updates;
#[cfg(unix)]
pub mod claude_lifecycle;
#[cfg(unix)]
pub mod claude_open;
pub mod cli;
#[cfg(unix)]
mod collection;
#[cfg(unix)]
mod collection_cache;
#[cfg(unix)]
mod dashboard_fed;
#[cfg(unix)]
pub mod dashboard_observations;
#[cfg(unix)]
mod feed;
#[cfg(unix)]
mod home_fed;
#[cfg(unix)]
mod home_producer;
#[cfg(unix)]
mod identity;
#[cfg(unix)]
mod json_object;
#[cfg(unix)]
pub mod key_lifecycle;
#[cfg(unix)]
mod lifecycle_effect_pipe;
#[cfg(unix)]
pub mod lifecycle_maintenance;
#[cfg(unix)]
pub mod lifecycle_queue;
#[cfg(unix)]
pub mod lifecycle_recheck;
#[cfg(unix)]
pub mod lifecycle_signin;
#[cfg(unix)]
mod limits;
/// Cross-platform: the models.dev fetch job G14's scheduling will run from the service.
pub mod models_dev;
#[cfg(unix)]
mod preferences_write;
#[cfg(unix)]
mod profile_reads;
pub mod public_auth;
#[cfg(unix)]
mod read_files;
#[cfg(unix)]
mod read_routes;
#[cfg(unix)]
mod registered;
mod reset_claims;
mod reset_source_route;
#[cfg(all(unix, test))]
mod runs_ingest_gate;
#[cfg(unix)]
mod scoreboard_owned;
#[cfg(unix)]
mod signins;
#[cfg(unix)]
mod status_cache;
#[cfg(unix)]
mod status_fed;
#[cfg(unix)]
mod status_owned;
#[cfg(unix)]
mod status_produce;
#[cfg(unix)]
mod status_run;
#[cfg(unix)]
mod store_host;
#[cfg(unix)]
mod table;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
mod usage_snapshot;
#[cfg(unix)]
pub mod view_reply;
#[cfg(unix)]
mod visibility_write;
#[cfg(unix)]
pub use identity::PROTOCOL;
#[cfg(unix)]
pub use unix::{
    Boot, NativeAccountUsage, serve, serve_composed, serve_with_account_usage_cache,
    serve_with_app_update_jobs, serve_with_claude_open, serve_with_native_account_usage,
    short_name,
};
