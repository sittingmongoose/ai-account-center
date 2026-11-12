//! Server snapshots own attempt state. Local send obligations have a separate lifetime.
use super::{ClaimResult, Said, result_said};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AttemptRef {
    pub attempt_key: String,
    pub attempt_order: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Evidence {
    pub canonical_account_key: String,
    pub revision: String,
    pub latest_attempt: Option<ClaimResult>,
    pub pending_attempts: Vec<AttemptRef>,
}

fn counter(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

impl Evidence {
    fn valid(&self) -> bool {
        if self.canonical_account_key.is_empty() || counter(&self.revision).is_none() {
            return false;
        }
        let Some(latest) = &self.latest_attempt else {
            return self.pending_attempts.is_empty();
        };
        let Some(order) = counter(&latest.attempt_order) else {
            return false;
        };
        if latest.attempt_key.is_empty()
            || latest.outcome.is_empty()
            || latest.evidence.is_some()
            || latest.pending != (latest.outcome == "claimed-unconfirmed")
        {
            return false;
        }
        // The durable store permits one pending attempt per canonical account, always the newest.
        match self.pending_attempts.as_slice() {
            [] => !latest.pending,
            [pending] => {
                latest.pending
                    && pending.attempt_key == latest.attempt_key
                    && counter(&pending.attempt_order) == Some(order)
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Unresolved {
    pub key: Option<String>,
    blocked: bool,
}
impl Default for Unresolved {
    fn default() -> Self {
        Self {
            key: None,
            blocked: true,
        }
    }
}
impl Unresolved {
    pub fn blocks(&self) -> bool {
        self.blocked
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountState {
    snapshot: Option<Evidence>,
    /// Kept independently, so settling one identity cannot erase another identity's evidence.
    attempts: BTreeMap<String, AttemptRef>,
    receipts: BTreeMap<String, (u64, ClaimResult)>,
    unanswered: BTreeMap<u64, f64>,
    uncertain: bool,
}
impl AccountState {
    pub fn matches(&self, evidence: &Evidence) -> bool {
        self.snapshot.as_ref() == Some(evidence)
    }
    pub fn lock(&self) -> Unresolved {
        Unresolved {
            key: self
                .attempts
                .values()
                .max_by_key(|a| counter(&a.attempt_order))
                .map(|a| a.attempt_key.clone()),
            blocked: self.snapshot.is_none()
                || self.uncertain
                || !self.attempts.is_empty()
                || !self.unanswered.is_empty(),
        }
    }
    pub fn begin_send(&mut self, id: u64, proof_after_ms: f64) {
        self.unanswered.insert(id, proof_after_ms);
    }
    pub fn unknown(&mut self) {
        self.uncertain = true;
    }
    pub fn inherit_sends(&mut self, previous: &mut Self) {
        self.unanswered.append(&mut previous.unanswered);
        self.uncertain |= previous.uncertain;
    }
    /// Stale snapshots can add receipts/history outside this method, but cannot mutate current state.
    pub fn observe(&mut self, evidence: &Evidence, read_started_ms: Option<f64>) -> bool {
        if !evidence.valid() {
            return false;
        }
        if let Some(previous) = &self.snapshot {
            let rev = counter(&evidence.revision).unwrap();
            let old = counter(&previous.revision).unwrap();
            if previous.canonical_account_key != evidence.canonical_account_key
                || rev < old
                || (rev == old && evidence != previous)
            {
                return false;
            }
            let old_order = previous
                .latest_attempt
                .as_ref()
                .and_then(|a| counter(&a.attempt_order));
            let order = evidence
                .latest_attempt
                .as_ref()
                .and_then(|a| counter(&a.attempt_order));
            if order < old_order {
                return false;
            }
        }
        self.attempts = evidence
            .pending_attempts
            .iter()
            .map(|a| (a.attempt_key.clone(), a.clone()))
            .collect();
        self.snapshot = Some(evidence.clone());
        self.uncertain = false;
        if let Some(started) = read_started_ms.filter(|n| n.is_finite()) {
            self.unanswered.retain(|_, deadline| started < *deadline);
        }
        true
    }
    pub fn receipt(
        &mut self,
        result: &ClaimResult,
        read_started_ms: Option<f64>,
        send: Option<u64>,
    ) -> bool {
        let Some(evidence) = &result.evidence else {
            return false;
        };
        let Some(revision) = counter(&evidence.revision) else {
            return false;
        };
        let Some(order) = counter(&result.attempt_order) else {
            return false;
        };
        if result.attempt_key.is_empty()
            || result.outcome.is_empty()
            || !evidence.valid()
            || evidence
                .latest_attempt
                .as_ref()
                .and_then(|a| counter(&a.attempt_order))
                .is_none_or(|newest| order > newest)
        {
            return false;
        }
        let mut receipt = result.clone();
        receipt.evidence = None;
        if self
            .receipts
            .get(&result.attempt_key)
            .is_none_or(|(old, _)| revision >= *old)
        {
            self.receipts
                .insert(result.attempt_key.clone(), (revision, receipt));
        }
        let accepted = self.observe(evidence, read_started_ms);
        if accepted && let Some(id) = send {
            self.unanswered.remove(&id);
        }
        accepted
    }
    pub fn not_sent(&mut self, id: u64) {
        self.unanswered.remove(&id);
    }
    pub fn current(&self, provider: &str) -> Option<Said> {
        if !self.unanswered.is_empty() {
            return Some(Said { text: "A reset request has no confirmed answer yet. The reset may have been used; check again.".into(), bad:true, check:true });
        }
        if self.uncertain {
            return Some(Said {
                text: "AAC must read this account again before another reset can be used.".into(),
                bad: true,
                check: true,
            });
        }
        self.snapshot
            .as_ref()?
            .latest_attempt
            .as_ref()
            .map(|r| result_said(r, provider))
    }
    pub fn older_receipts(&self) -> Vec<String> {
        let newest = self
            .snapshot
            .as_ref()
            .and_then(|s| s.latest_attempt.as_ref())
            .map(|r| &r.attempt_key);
        self.receipts
            .iter()
            .filter(|(key, _)| Some(*key) != newest)
            .map(|(_, (_, r))| format!("Older claim: {}", super::history_word(&r.outcome)))
            .collect()
    }
}
