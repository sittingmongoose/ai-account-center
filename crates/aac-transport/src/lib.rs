//! One bounded child/SSH transport. No live connections occur until an explicit call.
mod process;
mod shipping;

pub use process::{
    CallOptions, Computer, InteractiveCommand, ProcessOutput, SshPolicy, Stdin, StdinPart,
};
pub use shipping::{AgentArtifact, AgentClient, RawTerminal, artifact_cap};
pub use tokio_util::sync::CancellationToken;
