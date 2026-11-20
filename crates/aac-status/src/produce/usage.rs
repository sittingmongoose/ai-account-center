//! G16c: the usage sample the Status feed carries.
//!
//! The dashboard's analytics sample (`AccountAnalyticsSampleEvent`) is produced by the
//! Analytics owner (lane I/D and G18); the Status collector's part is the input the view
//! reads: the snapshot accounts, the sample's time and the snapshot's stale limit. That input
//! crosses once per sample (`recordUsage`'s result) and is held here; `null` before the first
//! sample, exactly as the TypeScript service's `usage` field.
//!
//! The snapshot projection itself (`buildSnapshotFiles`) stays with the owner that writes
//! `usage.json`/`usage.md` (G18), so a Status view never disagrees with the files agents read.

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// `{accounts, sampledAt, staleAfterSeconds}` as `status-collected` carries it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageInput {
    /// The snapshot's accounts (the page draws a subset; the feed carries what the sampler built).
    pub accounts: Vec<Json>,
    pub sampled_at: i64,
    pub stale_after_seconds: i64,
}

impl UsageInput {
    /// Parse a forwarded sample; `None` when it is not this shape.
    #[must_use]
    pub fn parse(value: &Json) -> Option<Self> {
        let input: Self = serde_json::from_value(value.clone()).ok()?;
        (input.sampled_at >= 0 && input.stale_after_seconds >= 0).then_some(input)
    }

    /// The SpendView's account order: the snapshot's accounts by first appearance.
    #[must_use]
    pub fn account_order(&self) -> Vec<String> {
        self.accounts
            .iter()
            .filter_map(|account| account.get("id").and_then(Json::as_str))
            .map(str::to_owned)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_sample_is_the_three_members_and_its_order() {
        let input = UsageInput::parse(&json!({
            "accounts": [{"id": "claude-max"}, {"id": "codex-pro"}],
            "sampledAt": 1_790_000_000_000_i64,
            "staleAfterSeconds": 180
        }))
        .unwrap();
        assert_eq!(input.account_order(), ["claude-max", "codex-pro"]);
        assert!(UsageInput::parse(&json!({"accounts": "no"})).is_none());
        assert!(UsageInput::parse(&json!({"accounts": [], "sampledAt": -1, "staleAfterSeconds": 1})).is_none());
    }
}
