//! Server snapshots own attempt state. Local send obligations have a separate lifetime.
use super::{ClaimResult, Refusal, Said, refusal_said, result_said};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct AttemptRef {
    pub attempt_key: String,
    pub attempt_order: String,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ClaimTokens {
    pub until_ms: Option<f64>,
    pub token_keys: Vec<String>,
}

#[derive(Clone, Debug, Default, Deserialize, PartialEq)]
#[serde(default, rename_all = "camelCase")]
pub struct Evidence {
    pub canonical_account_key: String,
    pub revision: String,
    pub latest_attempt: Option<ClaimResult>,
    pub pending_attempts: Vec<AttemptRef>,
    pub claim_tokens: Option<ClaimTokens>,
}

fn counter(value: &str) -> Option<u64> {
    (!value.is_empty() && value.bytes().all(|c| c.is_ascii_digit()))
        .then(|| value.parse().ok())
        .flatten()
}

impl Evidence {
    pub(super) fn valid(&self) -> bool {
        if self.canonical_account_key.is_empty() || counter(&self.revision).is_none() {
            return false;
        }
        let Some(tokens) = &self.claim_tokens else {
            return false;
        };
        if tokens.token_keys.is_empty() != tokens.until_ms.is_none()
            || tokens.until_ms.is_some_and(|ms| !ms.is_finite() || ms < 0.)
            || tokens
                .token_keys
                .iter()
                .any(|key| key.len() != 64 || !key.bytes().all(|c| c.is_ascii_hexdigit()))
            || tokens
                .token_keys
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != tokens.token_keys.len()
        {
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
    claim_ready: bool,
    confirmation_keys: Vec<String>,
}
impl Default for Unresolved {
    fn default() -> Self {
        Self {
            key: None,
            blocked: true,
            claim_ready: false,
            confirmation_keys: vec![],
        }
    }
}
impl Unresolved {
    pub fn blocks(&self) -> bool {
        self.blocked
    }
    /// Only the originating dialog can consume its own sole outstanding token.
    pub fn owns_confirmation(&self, key: &str) -> bool {
        self.key.is_none() && self.confirmation_keys.as_slice() == [key]
    }
    pub fn can_confirm(&self, key: &str) -> bool {
        self.claim_ready && self.owns_confirmation(key)
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ActionAnswer {
    Preparing,
    Refused(Refusal),
    NotSent(String),
    Unanswered(Option<String>),
    Completed { attempt_key: String },
}

#[derive(Clone, Debug, PartialEq)]
struct LocalAction {
    id: u64,
    /// Inventory revisions can advance without creating a new attempt. Keep the
    /// action's answer until an actual newer attempt/result supersedes this frontier.
    frontier: Option<ClaimResult>,
    answer: ActionAnswer,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AccountState {
    snapshot: Option<Evidence>,
    /// Kept independently, so settling one identity cannot erase another identity's evidence.
    attempts: BTreeMap<String, AttemptRef>,
    receipts: BTreeMap<String, (u64, ClaimResult)>,
    unanswered: BTreeMap<u64, f64>,
    read_after_ms: Option<f64>,
    refresh_required: bool,
    source_failure: Option<bool>,
    action: Option<LocalAction>,
}
impl AccountState {
    pub fn is_current_action(&self, id: u64) -> bool {
        self.action.as_ref().is_some_and(|a| a.id == id)
    }
    fn latest(&self) -> Option<&ClaimResult> {
        self.snapshot
            .as_ref()
            .and_then(|s| s.latest_attempt.as_ref())
    }
    pub fn begin_action(&mut self, id: u64) {
        if self.action.as_ref().is_none_or(|a| id > a.id) {
            if let Some(snapshot) = &self.snapshot
                && let Some(result) = &snapshot.latest_attempt
                && let Some(revision) = counter(&snapshot.revision)
                && self
                    .receipts
                    .get(&result.attempt_key)
                    .is_none_or(|(old, _)| *old <= revision)
            {
                self.receipts
                    .insert(result.attempt_key.clone(), (revision, result.clone()));
            }
            self.action = Some(LocalAction {
                id,
                frontier: self.latest().cloned(),
                answer: ActionAnswer::Preparing,
            });
        }
    }
    pub fn action_not_sent(&mut self, id: u64, text: String) {
        let latest = self.latest().cloned();
        if let Some(action) = &mut self.action
            && action.id == id
            && !matches!(action.answer, ActionAnswer::Completed { .. })
            && action.frontier == latest
        {
            action.answer = ActionAnswer::NotSent(text);
        }
    }
    pub fn action_unanswered(&mut self, id: u64, text: Option<String>) {
        if let Some(action) = &mut self.action
            && action.id == id
            && !matches!(action.answer, ActionAnswer::Completed { .. })
        {
            action.answer = ActionAnswer::Unanswered(text);
        }
    }
    pub fn action_refused(&mut self, id: u64, refusal: &Refusal) {
        let latest = self.latest().cloned();
        if let Some(e) = &refusal.evidence
            && (!e.valid()
                || self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.canonical_account_key != e.canonical_account_key))
        {
            return;
        }
        if let Some(action) = &mut self.action
            && action.id == id
            && !matches!(action.answer, ActionAnswer::Completed { .. })
            && latest == action.frontier
        {
            let mut refusal = refusal.clone();
            refusal.evidence = None;
            action.answer = ActionAnswer::Refused(refusal);
        }
    }
    fn action_said(&self, provider: &str) -> Option<Said> {
        let action = self.action.as_ref()?;
        if let ActionAnswer::Completed { attempt_key } = &action.answer {
            return self
                .latest()
                .filter(|r| &r.attempt_key == attempt_key)
                .map(|r| result_said(r, provider));
        }
        if action.frontier.as_ref() != self.latest() {
            return None;
        }
        Some(match &action.answer {
            ActionAnswer::Completed { .. } => return None,
            ActionAnswer::Preparing => Said {
                text: "No reset has been sent for this confirmation.".into(),
                bad: false,
                check: false,
            },
            ActionAnswer::Refused(r) => refusal_said(r, provider),
            ActionAnswer::NotSent(text) => Said {
                text: text.clone(),
                bad: true,
                check: true,
            },
            ActionAnswer::Unanswered(reason) => Said {
                text: format!(
                    "{}AAC could not confirm the answer to your latest reset request. Earlier claim results are in history; check again.",
                    reason.as_ref().map(|s| format!("{s} ")).unwrap_or_default()
                ),
                bad: true,
                check: true,
            },
        })
    }
    pub fn needs_refresh(&self) -> bool {
        self.refresh_required || self.snapshot.is_none()
    }
    pub fn invalidate(&mut self) {
        self.refresh_required = true;
    }
    pub fn refreshed(&mut self) {
        self.refresh_required = false;
        self.source_failure = None;
    }
    pub fn unavailable(&mut self, account_changed: bool) {
        self.refresh_required = true;
        self.source_failure = Some(account_changed);
    }
    #[cfg(test)]
    pub fn matches(&self, evidence: &Evidence) -> bool {
        self.snapshot.as_ref() == Some(evidence)
    }
    pub fn token_deadline(&self) -> Option<f64> {
        self.snapshot.as_ref()?.claim_tokens.as_ref()?.until_ms
    }
    pub fn lock(&self) -> Unresolved {
        let claim_ready = !self.refresh_required
            && self.snapshot.is_some()
            && self.read_after_ms.is_none()
            && self.attempts.is_empty()
            && self.unanswered.is_empty();
        let confirmation_keys = self
            .snapshot
            .as_ref()
            .and_then(|e| e.claim_tokens.as_ref())
            .map(|tokens| tokens.token_keys.clone())
            .unwrap_or_default();
        Unresolved {
            key: self
                .attempts
                .values()
                .max_by_key(|a| counter(&a.attempt_order))
                .map(|a| a.attempt_key.clone()),
            blocked: !claim_ready || !confirmation_keys.is_empty(),
            claim_ready,
            confirmation_keys,
        }
    }
    pub fn begin_send(&mut self, id: u64, proof_after_ms: f64) {
        self.begin_action(id);
        if let Some(action) = &mut self.action
            && action.id == id
        {
            action.answer = ActionAnswer::Unanswered(None);
        }
        self.unanswered.insert(id, proof_after_ms);
    }
    pub fn unknown(&mut self, received_ms: f64) {
        self.read_after_ms = Some(self.read_after_ms.unwrap_or(received_ms).max(received_ms));
    }
    pub fn inherit_sends(&mut self, previous: &mut Self) {
        self.unanswered.append(&mut previous.unanswered);
        if let Some(action) = previous.action.take()
            && self.action.as_ref().is_none_or(|a| action.id > a.id)
        {
            self.action = Some(action);
        }
        if let Some(after) = previous.read_after_ms {
            self.unknown(after);
        }
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
        if let Some(started) = read_started_ms.filter(|n| n.is_finite()) {
            self.unanswered.retain(|_, deadline| started < *deadline);
            if self.read_after_ms.is_some_and(|after| started >= after) {
                self.read_after_ms = None;
            }
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
            if let Some(action) = &mut self.action
                && action.id == id
            {
                action.answer = ActionAnswer::Completed {
                    attempt_key: result.attempt_key.clone(),
                };
            }
        }
        accepted
    }
    pub fn not_sent(&mut self, id: u64) {
        self.unanswered.remove(&id);
    }
    pub fn current(&self, provider: &str) -> Option<Said> {
        let action = self.action_said(provider);
        if !self.unanswered.is_empty() {
            let text = "A reset request has no confirmed answer yet. The reset may have been used; check again.";
            return Some(Said {
                text: action
                    .filter(|_| {
                        self.action.as_ref().is_some_and(|a| match &a.answer {
                            ActionAnswer::Unanswered(None) => false,
                            ActionAnswer::Refused(_) => !self.unanswered.contains_key(&a.id),
                            _ => true,
                        })
                    })
                    .map_or_else(|| text.into(), |a| format!("{} {text}", a.text)),
                bad: true,
                check: true,
            });
        }
        if let Some(mut said) = action {
            said.check |= self.needs_refresh()
                || self.read_after_ms.is_some()
                || !self.attempts.is_empty()
                || self.token_deadline().is_some();
            return Some(said);
        }
        if let Some(changed) = self.source_failure {
            return Some(Said {
                text: if changed { "Account changed; usage unavailable. Check again before using a reset." } else { "Usage is unavailable. Earlier claim results are in history; check again before using a reset." }.into(),
                bad: true,
                check: true,
            });
        }
        if self.refresh_required || self.read_after_ms.is_some() || self.snapshot.is_none() {
            return Some(Said {
                text: "AAC needs a current account read before a reset can be used.".into(),
                bad: true,
                check: true,
            });
        }
        if self.token_deadline().is_some() {
            return Some(Said {
                text: "A claim started on this or another page may still arrive.".into(),
                bad: true,
                check: true,
            });
        }
        self.latest().filter(|r| r.pending).map(|_| Said {
            text:
                "A reset claim on this account is still unconfirmed. Check it before using another."
                    .into(),
            bad: true,
            check: true,
        })
    }
    pub fn older_receipts(&self) -> Vec<String> {
        let superseded = self.action.as_ref().is_some_and(|a| {
            !matches!(a.answer, ActionAnswer::Completed { .. })
                && a.frontier.as_ref() == self.latest()
        });
        let newest = self
            .snapshot
            .as_ref()
            .and_then(|s| s.latest_attempt.as_ref())
            .map(|r| &r.attempt_key);
        let mut rows: BTreeMap<String, String> = self
            .receipts
            .iter()
            .filter(|(key, _)| superseded || Some(*key) != newest)
            .map(|(key, (_, r))| {
                (
                    key.clone(),
                    format!("Older claim: {}", super::history_word(&r.outcome)),
                )
            })
            .collect();
        if superseded && let Some(r) = self.latest() {
            rows.insert(
                r.attempt_key.clone(),
                format!("Older claim: {}", super::history_word(&r.outcome)),
            );
        }
        rows.into_values().collect()
    }
}

#[cfg(test)]
mod frontier_invariants {
    use super::*;
    fn trace() -> serde_json::Value {
        serde_json::from_str(include_str!(
            "../tests/fixtures/reset-refusal-after-success.json"
        ))
        .unwrap()
    }
    #[test]
    fn refusal_evidence_never_reassigns_the_original_action_frontier() {
        let t = trace();
        for (start, body) in [
            ("before", "refused"),
            ("before", "consumedWhilePending"),
            ("before", "consumedAfterSuccess"),
            ("duringRetry", "consumedAfterSuccess"),
        ] {
            let mut state = AccountState::default();
            let before: Evidence = serde_json::from_value(t[start]["evidence"].clone()).unwrap();
            state.observe(&before, Some(1.));
            state.begin_action(1);
            let original = state.action.as_ref().unwrap().frontier.clone();
            let refusal: Refusal = serde_json::from_value(t[body].clone()).unwrap();
            state.observe(refusal.evidence.as_ref().unwrap(), None);
            state.action_refused(1, &refusal);
            assert_eq!(state.action.as_ref().unwrap().frontier, original);
        }
    }
    #[test]
    fn canonical_binding_moves_the_action_without_reassigning_its_frontier() {
        let t = trace();
        let mut prior = AccountState::default();
        prior.begin_action(1);
        let original = prior.action.as_ref().unwrap().frontier.clone();
        let mut canonical = AccountState::default();
        canonical.observe(
            &serde_json::from_value(t["afterRetry"]["evidence"].clone()).unwrap(),
            None,
        );
        canonical.inherit_sends(&mut prior);
        assert_eq!(canonical.action.as_ref().unwrap().frontier, original);
    }
}
