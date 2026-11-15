//! Private Unix-socket HTTP service. The TS front door retains authentication and policy.
#[cfg(unix)]
mod account_reads;
#[cfg(unix)]
mod json_object;
#[cfg(unix)]
mod private_file;
#[cfg(unix)]
mod profile_reads;
#[cfg(unix)]
mod read_files;
#[cfg(unix)]
mod read_routes;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{Boot, serve};
