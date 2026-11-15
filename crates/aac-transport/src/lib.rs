//! One bounded child/SSH transport. No live connections occur until an explicit call.
mod process;
mod shipping;

pub use process::{CallOptions, Computer, ProcessOutput, SshPolicy};
pub use shipping::{AgentArtifact, AgentClient, artifact_cap};
pub use tokio_util::sync::CancellationToken;
