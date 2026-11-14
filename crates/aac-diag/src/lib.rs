//! Honest liveness and skeleton diagnostics; no store opening or provider activity.
use aac_api::{DiagnosticReading, Diagnostics, Health};

/// Legacy public process liveness response.
pub fn health() -> Health {
    Health {
        status: "ok".into(),
    }
}

/// The service has no migrated jobs, computers or store yet.
pub fn diagnostics() -> Diagnostics {
    Diagnostics {
        jobs: vec![],
        scheduler: DiagnosticReading {
            status: "unavailable".into(),
            last_success_at: None,
            cause: "jobs_not_migrated".into(),
        },
        store: DiagnosticReading {
            status: "unavailable".into(),
            last_success_at: None,
            cause: "store_owned_by_ts".into(),
        },
        computers: vec![],
    }
}
