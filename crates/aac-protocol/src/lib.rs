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
/// Discovery's existing 8 MiB payload cap plus bounded agent envelope overhead.
pub const MAX_SCAN_FRAME_BYTES: usize = 8 * 1024 * 1024 + 1024;

/// Claude history retains the private helper's 24 MiB request budget.
pub const MAX_HISTORY_FRAME_BYTES: usize = 24 * 1024 * 1024;

/// Keep larger discovery and history frames isolated from other verbs.
pub fn frame_cap(verb: &str) -> usize {
    if verb == "claude-history" {
        MAX_HISTORY_FRAME_BYTES
    } else if verb == "scan-usage" {
        MAX_SCAN_FRAME_BYTES
    } else {
        MAX_FRAME_BYTES
    }
}
/// Largest allowed per-request relative deadline in milliseconds.
pub const MAX_DEADLINE_MS: u64 = 600_000;
/// Interactive sign-in sessions keep the CLI's own 15-minute window plus the install and
/// cleanup margin; every other verb keeps the ten-minute cap.
pub const MAX_SIGNIN_SESSION_DEADLINE_MS: u64 = 960_000;

/// The deadline cap for one verb.
pub fn max_deadline_ms(verb: &str) -> u64 {
    if verb == "signin-session" {
        MAX_SIGNIN_SESSION_DEADLINE_MS
    } else {
        MAX_DEADLINE_MS
    }
}

/// Time a single application update call may spend on its host.
pub const APP_UPDATE_DEADLINE_MS: u64 = 31 * 60_000;
/// Time the dashboard may spend on connection, update and cleanup for one host.
pub const APP_UPDATE_HOST_TIMEOUT_MS: u64 = 33 * 60_000;

/// Time the Linux ACP follow-up may spend waiting for T3 and installing through it.
pub const ACP_FOLLOW_UP_DEADLINE_MS: u64 = 36 * 60_000;
/// Includes transport setup and cleanup around the ACP follow-up's helper budget.
pub const ACP_FOLLOW_UP_HOST_TIMEOUT_MS: u64 = 37 * 60_000;

/// The host progress event that requests permission to begin an app update.
pub const APP_UPDATE_ADMISSION: &str = "admission";
/// The one control line that grants a prepared app update.
pub const APP_UPDATE_ADMIT: &str = r#"{"admit":true}"#;

/// Long-running app updates have their own bound. Other agent verbs keep the short bound.
pub fn deadline_cap(verb: &str, args: &Value) -> u64 {
    match (verb, args["action"].as_str()) {
        ("apps", Some("update" | "prepare-update")) => APP_UPDATE_DEADLINE_MS,
        ("apps", Some("acp-follow-up" | "prepare-acp-follow-up")) => ACP_FOLLOW_UP_DEADLINE_MS,
        _ => max_deadline_ms(verb),
    }
}

/// Exactly one request starts an agent process; later stdin lines carry controls.
/// Argument values deliberately cannot be formatted with Debug.
/// ```compile_fail
/// let request = aac_protocol::Request { protocol: 1, verb: "health".into(), id: "fixture".into(), deadline_ms: 1000, args: serde_json::json!({}) };
/// let _ = format!("{request:?}");
/// ```
#[derive(Clone, PartialEq, Serialize, Deserialize)]
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
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
            || self.deadline_ms == 0
            || self.deadline_ms > deadline_cap(&self.verb, &self.args)
            || !self.args.is_object()
        {
            return Err(Failure::new(Cause::Refused, "invalid request envelope"));
        }
        Ok(())
    }
}

/// Interactive controls retain input only in memory and must never be logged.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged, deny_unknown_fields)]
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
