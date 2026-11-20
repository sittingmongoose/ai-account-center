//! Shared, bounded one-shot account approvals for this service boot.
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

const TTL_MS: u64 = 120_000;
const MAX_PENDING: usize = 32;
const MAX_PER_SESSION: usize = 8;

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
enum Action {
    Remove,
    TrashRestore,
    TrashPurge,
    SigninAgainActive,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum Operation {
    Issue,
    Consume,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    operation: Operation,
    action: Action,
    subject: String,
    session_key: String,
    state_fingerprint: String,
    token_digest: String,
}

impl Input {
    fn well_formed(&self) -> bool {
        let digest = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        };
        !self.subject.is_empty()
            && self.subject.len() <= 512
            && !self.subject.chars().any(char::is_control)
            && digest(&self.session_key)
            && digest(&self.state_fingerprint)
            && digest(&self.token_digest)
    }

    fn same_binding(&self, other: &Self) -> bool {
        self.action == other.action
            && self.subject == other.subject
            && self.session_key == other.session_key
            && self.state_fingerprint == other.state_fingerprint
    }
}

struct Pending {
    input: Input,
    expires_at_ms: u64,
}

#[derive(Default)]
pub(crate) struct Approvals {
    pending: Mutex<VecDeque<Pending>>,
}

impl Approvals {
    pub(crate) fn apply(&self, bytes: &[u8]) -> Option<Value> {
        let input = crate::json_object::from_slice::<Input>(
            bytes,
            &[
                "operation",
                "action",
                "subject",
                "sessionKey",
                "stateFingerprint",
                "tokenDigest",
            ],
        )
        .ok()?;
        if !input.well_formed() {
            return None;
        }
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis(),
        )
        .ok()?;
        self.apply_at(input, now)
    }

    fn apply_at(&self, input: Input, now: u64) -> Option<Value> {
        let mut pending = self.pending.lock().ok()?;
        match input.operation {
            Operation::Issue => {
                let expires_at_ms = now.checked_add(TTL_MS)?;
                pending.retain(|entry| entry.expires_at_ms > now);
                if pending
                    .iter()
                    .any(|entry| entry.input.token_digest == input.token_digest)
                {
                    return None;
                }
                let mut own = pending
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry.input.session_key == input.session_key);
                if own.clone().count() >= MAX_PER_SESSION {
                    let index = own.next()?.0;
                    pending.remove(index);
                }
                if pending.len() >= MAX_PENDING {
                    pending.pop_front();
                }
                pending.push_back(Pending {
                    input,
                    expires_at_ms,
                });
                Some(json!({ "expiresAtMs": expires_at_ms }))
            }
            Operation::Consume => {
                let index = pending
                    .iter()
                    .position(|entry| entry.input.token_digest == input.token_digest);
                // Remove a token before checking the binding. A stale or wrong attempt spends it.
                let consumed = index
                    .and_then(|index| pending.remove(index))
                    .is_some_and(|entry| {
                        entry.expires_at_ms > now && input.same_binding(&entry.input)
                    });
                Some(json!({ "consumed": consumed }))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(operation: Operation) -> Input {
        Input {
            operation,
            action: Action::Remove,
            subject: "codex:fixture@example.com".into(),
            session_key: "a".repeat(64),
            state_fingerprint: "b".repeat(64),
            token_digest: "c".repeat(64),
        }
    }

    #[test]
    fn expires_at_the_same_millisecond_and_cannot_be_replayed() {
        let approvals = Approvals::default();
        assert_eq!(
            approvals.apply_at(input(Operation::Issue), 5_000),
            Some(json!({ "expiresAtMs": 125_000 }))
        );
        assert_eq!(
            approvals.apply_at(input(Operation::Consume), 125_000),
            Some(json!({ "consumed": false }))
        );
        assert_eq!(
            approvals.apply_at(input(Operation::Consume), 124_999),
            Some(json!({ "consumed": false }))
        );
        approvals.apply_at(input(Operation::Issue), 5_000);
        assert_eq!(
            approvals.apply_at(input(Operation::Consume), 124_999),
            Some(json!({ "consumed": true }))
        );
        assert_eq!(
            approvals.apply_at(input(Operation::Consume), 124_999),
            Some(json!({ "consumed": false }))
        );
    }
}
