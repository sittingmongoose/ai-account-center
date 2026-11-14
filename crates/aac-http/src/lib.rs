//! Private Unix-socket HTTP service. The TS front door retains authentication and policy.
#[cfg(unix)]
mod json_object;
#[cfg(unix)]
mod unix;
#[cfg(unix)]
pub use unix::{Boot, serve};
