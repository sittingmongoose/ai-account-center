//! Private service HTTP core and opt-in M6 dashboard front door.
//! The collection, settings and account-read owners build on every platform the dashboard does;
//! the private Unix protocol and the services it alone serves stay on Unix.

mod account_approvals;
mod account_reads;
pub mod account_usage;
mod agent_transport;
pub mod allocator;
mod analytics_live;
mod analytics_route;
mod analytics_storage;
mod analytics_writers;
mod answers_owned;
pub mod antigravity_host;
pub mod antigravity_probes;
pub mod antigravity_quota;
pub mod antigravity_renewal;
pub mod antigravity_runtime;
pub mod app_update_routes;
pub mod app_updates;
mod claude_account_store;
mod claude_host;
pub mod claude_lifecycle;
pub mod claude_open;
pub mod cli;
mod codex_quotas;
mod collection;
mod collection_cache;
mod dashboard_fed;
pub mod dashboard_observations;
mod feed;
mod home_fed;
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
/// The public owners one boot runs: the private service and the public host share them.
mod owners;
mod preferences_write;
mod profile_reads;
pub mod public_auth;
mod read_files;
#[cfg(unix)]
mod read_routes;
mod registered;
mod reset_claims;
mod reset_source_route;
#[cfg(all(unix, test))]
mod runs_ingest_gate;
mod scoreboard_owned;
pub mod signin_callbacks;
mod signins;
mod status_answers_route;
mod status_cache;
mod status_collate;
mod status_fed;
mod status_owned;
mod status_produce;
mod status_run;
mod store_host;
mod table;
#[cfg(unix)]
mod unix;
mod usage_routes;
mod usage_snapshot;
pub mod view_reply;
mod visibility_write;
mod zone_facts;
pub use identity::PROTOCOL;
#[cfg(unix)]
pub use unix::{
    Boot, NativeAccountUsage, serve, serve_composed, serve_with_account_usage_cache,
    serve_with_app_update_jobs, serve_with_claude_open, serve_with_native_account_usage,
    short_name,
};
