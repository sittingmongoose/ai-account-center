//! Private Unix-socket HTTP service. The TS front door retains authentication and policy.
#[cfg(unix)]
mod account_approvals;
#[cfg(unix)]
mod account_reads;
#[cfg(unix)]
mod agent_transport;
#[cfg(unix)]
pub mod app_update_routes;
#[cfg(unix)]
mod feed;
#[cfg(unix)]
mod home_fed;
#[cfg(unix)]
mod identity;
#[cfg(unix)]
mod json_object;
#[cfg(unix)]
mod limits;
#[cfg(unix)]
pub mod lifecycle_maintenance;
#[cfg(unix)]
pub mod lifecycle_queue;
#[cfg(unix)]
mod preferences_write;
#[cfg(unix)]
mod profile_reads;
#[cfg(unix)]
mod read_files;
#[cfg(unix)]
mod read_routes;
#[cfg(unix)]
mod registered;
#[cfg(unix)]
mod status_cache;
#[cfg(unix)]
mod status_fed;
#[cfg(unix)]
mod store_host;
#[cfg(unix)]
mod table;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub mod view_reply;
#[cfg(unix)]
mod visibility_write;
#[cfg(unix)]
pub use identity::PROTOCOL;
#[cfg(unix)]
pub use unix::{Boot, serve, serve_with_app_update_jobs, short_name};
