//! G13: the Claude desktop Open and history-sync orchestration the TypeScript front door
//! delegates to once the `claude-open` group is on. The pieces mirror the TS owners one for
//! one: the pinned copy core (`sync`), its durable marker protocol (`markers`), the Open
//! progress store (`operations`), the manifest and pending reads (`manifest`), the private
//! policy revalidation (`policy`) and the copy/retention wrapper (`sync_service`). The
//! TypeScript path stays intact until the cut; every unproven input declines back to it.
// The helper boundary's errors are opaque by design: no error detail may cross it, exactly
// as the TS adapters throw untyped errors the core reduces to fixed reason tokens.
#![allow(clippy::result_unit_err)]
pub mod js;
pub mod manifest;
// The POSIX marker, policy, copy and Open orchestrators serve the Linux Open path only: the
// TS core refuses to arm a marker on a non-POSIX host, so Windows keeps its existing owner.
#[cfg(unix)]
pub mod markers;
#[cfg(unix)]
pub mod operations;
#[cfg(unix)]
pub mod open;
#[cfg(unix)]
pub mod policy;
#[cfg(unix)]
pub mod remote;
#[cfg(unix)]
pub mod sync;
#[cfg(unix)]
pub mod sync_service;
#[cfg(all(unix, test))]
mod fixture_tests;
#[cfg(all(unix, test))]
mod marker_tests;
#[cfg(all(unix, test))]
mod socket_tests;
#[cfg(all(unix, test))]
mod table_tests;
