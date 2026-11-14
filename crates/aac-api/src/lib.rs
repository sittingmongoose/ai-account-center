//! Portable HTTP DTOs for the first strangler routes. No filesystem or runtime dependencies.
use serde::{Deserialize, Serialize};

/// Public liveness only; no readiness or provider claims.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Health {
    /// The legacy wire spelling.
    pub status: String,
}

/// Existing account refresh setting, still written exclusively by TypeScript.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AccountRefreshSettings {
    /// Whole seconds, inclusive range 30..=3600.
    pub refresh_interval_seconds: u32,
}

/// Trust source verified by the TypeScript front door, never a caller's assertion.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalKind {
    /// Browser session (also the reserved public-health envelope).
    Session,
    /// Paired device.
    Device,
    /// Native bar with verified credentials.
    Bar,
    /// Usage hub key.
    Hub,
}

/// Rights asserted by the front door. Public never means a signed-in browser.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PrincipalRole {
    /// Only the public liveness route; id must be `public-health`.
    Public,
    /// The existing dashboard has one configured administrator.
    Admin,
    /// Existing paired-device scope.
    Device,
    /// Existing read-only hub scope.
    Hub,
}

/// Forwarded only alongside the service's boot secret over its private socket.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPrincipal {
    /// Credential category.
    pub kind: PrincipalKind,
    /// Opaque identifier (session ids are SHA-256 hashed by TS).
    pub id: String,
    /// Existing authority, without new permissions.
    pub role: PrincipalRole,
}

impl VerifiedPrincipal {
    /// Reject impossible combinations and unbounded/unsafe identifiers.
    pub fn valid(&self) -> bool {
        let id_ok = !self.id.is_empty()
            && self.id.len() <= 128
            && self
                .id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b));
        id_ok
            && match (&self.kind, &self.role) {
                (PrincipalKind::Session, PrincipalRole::Public) => self.id == "public-health",
                (PrincipalKind::Session | PrincipalKind::Bar, PrincipalRole::Admin) => {
                    self.id != "public-health"
                }
                (PrincipalKind::Device, PrincipalRole::Device)
                | (PrincipalKind::Hub, PrincipalRole::Hub) => true,
                _ => false,
            }
    }

    /// Settings and diagnostics require the already-verified dashboard session.
    pub fn dashboard_session(&self) -> bool {
        self.role == PrincipalRole::Admin
            && matches!(self.kind, PrincipalKind::Session | PrincipalKind::Bar)
    }
}

/// A skeleton subsystem is honestly unavailable until its owning jobs/store move.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticReading {
    /// Currently `unavailable`.
    pub status: String,
    /// UTC ms of last success, absent while no Rust job exists.
    pub last_success_at: Option<u64>,
    /// Stable cause, without paths, credentials or guessed health.
    pub cause: String,
}

/// One named job's diagnostic state.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct JobDiagnostic {
    /// Stable job name.
    pub name: String,
    /// Last success and current cause.
    #[serde(flatten)]
    pub reading: DiagnosticReading,
}

/// Signed-in diagnostic response. No probes or provider refresh are triggered.
#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Diagnostics {
    /// No Rust jobs exist in M3 part 1.
    pub jobs: Vec<JobDiagnostic>,
    /// Explicit aggregate state even when the job list is empty.
    pub scheduler: DiagnosticReading,
    /// The native store stays owned by TS, so Rust cannot assert its health.
    pub store: DiagnosticReading,
    /// Agent transports are not owned by the HTTP skeleton.
    pub computers: Vec<serde_json::Value>,
}
