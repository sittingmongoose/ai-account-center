//! Versioned, WASM-safe agent types. No process, filesystem or async runtime dependency.
use aac_core::{Cause, Failure};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Current stdin/stdout protocol version. Incompatible peers must fail the handshake.
pub const PROTOCOL_VERSION: u32 = 1;
/// Launcher exit status requesting one verified upload.
pub const EXIT_AGENT_MISSING: i32 = 87;
/// Largest request or NDJSON event accepted at a protocol boundary.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
/// Largest allowed per-request relative deadline in milliseconds.
pub const MAX_DEADLINE_MS: u64 = 600_000;

/// Exactly one request starts an agent process; later stdin lines carry controls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    /// Must equal `PROTOCOL_VERSION`.
    pub protocol: u32,
    /// A stable verb spelling; unsupported verbs are refused, never executed as commands.
    pub verb: String,
    /// Bounded opaque correlation id echoed in every event.
    pub id: String,
    /// Relative budget from request receipt; includes input and execution after receipt.
    pub deadline_ms: u64,
    /// Verb-specific object, validated by its handler.
    pub args: Value,
}

impl Request {
    /// Validate version, correlation id, deadline and argument envelope before dispatch.
    pub fn validate(&self) -> Result<(), Failure> {
        if self.protocol != PROTOCOL_VERSION {
            return Err(Failure::new(
                Cause::AgentMismatch,
                "agent protocol mismatch",
            ));
        }
        if self.id.is_empty()
            || self.id.len() > 128
            || !self
                .id
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"_.:-".contains(&c))
            || self.verb.is_empty()
            || self.verb.len() > 80
            || !self
                .verb
                .bytes()
                .all(|c| c.is_ascii_lowercase() || c == b'-')
            || self.deadline_ms == 0
            || self.deadline_ms > MAX_DEADLINE_MS
            || !self.args.is_object()
        {
            return Err(Failure::new(Cause::Refused, "invalid request envelope"));
        }
        Ok(())
    }
}

/// Interactive controls retain input only in memory and must never be logged.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Control {
    /// Request cancellation; false does not cancel.
    Cancel {
        /// True requests cooperative cancellation followed by transport termination.
        cancel: bool,
    },
    /// Forward an interactive sign-in answer to its verb handler.
    Input {
        /// Opaque private input; never persisted or formatted for diagnostics.
        input: String,
    },
}

/// One bounded NDJSON event; a process emits at most one terminal result or failure.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Nonterminal progress for the named request.
    Progress {
        /// Negotiated protocol version.
        protocol: u32,
        /// Matching request id.
        id: String,
        /// Verb-specific progress object.
        data: Value,
    },
    /// Successful terminal result.
    Result {
        /// Negotiated protocol version.
        protocol: u32,
        /// Matching request id.
        id: String,
        /// Verb-specific result value.
        value: Value,
    },
    /// Failed terminal result; no guessed value is attached.
    Failure {
        /// Negotiated protocol version.
        protocol: u32,
        /// Matching request id, or empty for an unreadable request.
        id: String,
        /// Shared scrubbed failure.
        failure: Failure,
    },
}

/// The version handshake returned by `aac-agent --version` after verifying its hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    /// Must be `aac-agent`.
    pub name: String,
    /// Build's package version, checked against the artifact manifest.
    pub version: String,
    /// Supported wire protocol version.
    pub protocol: u32,
    /// Rust target triple, checked against the artifact manifest.
    pub target: String,
}
