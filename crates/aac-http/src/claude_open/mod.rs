//! G13: the Claude desktop Open and history-sync orchestration the TypeScript front door
//! delegates to once the `claude-open` group is on. The pieces mirror the TS owners one for
//! one: the pinned copy core (`sync`), its durable marker protocol (`markers`), the Open
//! progress store (`operations`), the manifest and pending reads (`manifest`), the private
//! policy revalidation (`policy`) and the copy/retention wrapper (`sync_service`). The
//! TypeScript path stays intact until the cut; every unproven input declines back to it.
pub mod js;
pub mod manifest;
pub mod markers;
pub mod operations;
pub mod open;
pub mod policy;
pub mod sync;
pub mod remote;
pub mod sync_service;
#[cfg(test)]
mod table_tests;
#[cfg(test)]
mod marker_tests;
#[cfg(test)]
mod fixture_tests;
#[cfg(test)]
mod socket_tests;
