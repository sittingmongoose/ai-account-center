//! Interactive sign-in sessions (M4c): the job state machine, the output parser, the PTY
//! process layer, the journal that survives a TypeScript restart, and the `signin-session`
//! verb that drives one CLI session per agent process. The install commit stays with the
//! TypeScript front door until M5.

pub mod jobs;
pub mod journal;
pub mod parser;
pub mod process;
pub mod verb;
