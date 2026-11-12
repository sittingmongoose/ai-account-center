//! One canonical account's reset state: four concerns, each with one owner. The current text is a pure function of
//! the first three and owns no state of its own.
//!
//! 1. **Server snapshot admission** (`snapshot`, `attempts`, [`AccountState::observe`]). A snapshot is admitted
//!    only if it is valid, names the same canonical account, has a higher revision (or the same revision with
//!    identical content) and does not lower the latest attempt order. A stale snapshot may add a dated receipt to
//!    history, but never rolls back evidence, pending attempts, tokens or locks.
//! 2. **Action provenance** (`action`). Every local prepare/claim gets an increasing id and an immutable starting
//!    frontier: the admitted latest attempt/result when it began, written once in [`AccountState::begin_action`].
//!    Only a callback carrying that id records its answer, and an answer whose evidence predates the frontier is not
//!    its answer. The claim's own receipt (its id, the new attempt key it created, and the canonical identity that
//!    `ResetCtx::receive` checks) records `Completed` whether or not the snapshot carrying it was admitted: admission
//!    and provenance are separate decisions. A completed action never takes a later refusal or failure.
//! 3. **Unanswered-send obligation, U1** (`unanswered`, `read_after_ms`, [`AccountState::lock`]). Unchanged rules: a
//!    send stays open from `begin_send` until its own *admitted* receipt, its own admitted `sent:false` refusal, or an
//!    admitted successful read that started at or after its proof deadline. Provenance never lifts it.
//! 4. **Current text** ([`AccountState::current`]):
//!    - Current-tense numbers come only from current source readings; no receipt percentage is read here.
//!    - A durable receipt is current only as the answer to this context's own claim, and only while the admitted
//!      latest attempt is that same attempt; its admitted result is what is said. Otherwise it is dated history.
//!    - Any other answer (preparing, refused, not sent, unanswered) is current only while the admitted latest
//!      attempt/result is still exactly the action's starting frontier.
//!    - A newer local action replaces an older one; a genuinely newer server attempt/result supersedes both.
//!    - While U1 is open its sentence is said. A current definitive own answer (completed, `sent:false` refusal,
//!      not sent) is said first; when only that answer's own send is still open, U1 asks only to check again.
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

// ---- wording the projection uses (tests compare against these, never against copies) -----------------------------

/// U1's sentence while a send has no confirmed answer.
pub(super) const UNANSWERED_SEND: &str =
    "A reset request has no confirmed answer yet. The reset may have been used; check again.";
/// U1's sentence when the only open send already has this context's definitive answer.
pub(super) const OWN_SEND_WINDOW: &str = "Check again before using another reset.";
pub(super) const PREPARING: &str = "No reset has been sent for this confirmation.";
pub(super) const NEEDS_READ: &str = "AAC needs a current account read before a reset can be used.";
pub(super) const TOKENS_OUTSTANDING: &str =
    "A claim started on this or another page may still arrive.";
pub(super) const STILL_UNCONFIRMED: &str =
    "A reset claim on this account is still unconfirmed. Check it before using another.";
pub(super) const ACCOUNT_CHANGED_UNAVAILABLE: &str =
    "Account changed; usage unavailable. Check again before using a reset.";
pub(super) const USAGE_UNAVAILABLE: &str =
    "Usage is unavailable. Earlier claim results are in history; check again before using a reset.";
pub(super) fn unanswered_answer(reason: Option<&str>) -> String {
    format!(
        "{}AAC could not confirm the answer to your latest reset request. Earlier claim results are in history; check again.",
        reason.map(|s| format!("{s} ")).unwrap_or_default()
    )
}

// ---- 2. action provenance -----------------------------------------------------------------------------------------

/// One server attempt and its result, as far as ordering answers needs.
#[derive(Clone, Debug, PartialEq)]
struct Mark {
    order: u64,
    key: String,
    outcome: String,
    pending: bool,
}
impl Mark {
    /// Only validated results are marked, so their order always parses.
    fn of(result: Option<&ClaimResult>) -> Option<Self> {
        result.map(|r| Self {
            order: counter(&r.attempt_order).unwrap_or_default(),
            key: r.attempt_key.clone(),
            outcome: r.outcome.clone(),
            pending: r.pending,
        })
    }
    /// Server progress is monotonic: attempts are appended in order and settle once.
    fn progress(mark: Option<&Self>) -> (u64, bool) {
        mark.map_or((0, false), |m| (m.order, !m.pending))
    }
}

#[derive(Clone, Debug, PartialEq)]
enum ActionAnswer {
    Preparing,
    Refused(Refusal),
    NotSent(String),
    Unanswered(Option<String>),
    /// The claim's own receipt: the new attempt it created.
    Completed {
        attempt_key: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
struct LocalAction {
    id: u64,
    /// The admitted latest attempt/result when this action began. Written only by `begin_action`.
    frontier: Option<Mark>,
    answer: ActionAnswer,
}
impl LocalAction {
    /// An answer cannot predate its own question.
    fn answered_by(&self, evidence: Option<&Evidence>) -> bool {
        evidence.is_none_or(|e| {
            Mark::progress(Mark::of(e.latest_attempt.as_ref()).as_ref())
                >= Mark::progress(self.frontier.as_ref())
        })
    }
}

/// How a current own answer combines with an open U1 obligation.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Combine {
    /// It settles what happened to its own send (or that nothing was sent).
    Definitive,
    /// Said before the U1 sentence.
    Prefix,
    /// The U1 sentence says all there is.
    Hidden,
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
    /// The only place a frontier is written.
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
                frontier: Mark::of(self.latest()),
                answer: ActionAnswer::Preparing,
            });
        }
    }
    /// Records an answer for the action with this id. Whether it is current is decided only when projecting.
    fn answer(&mut self, id: u64, evidence: Option<&Evidence>, answer: ActionAnswer) {
        if let Some(action) = &mut self.action
            && action.id == id
            && !matches!(action.answer, ActionAnswer::Completed { .. })
            && action.answered_by(evidence)
        {
            action.answer = answer;
        }
    }
    pub fn action_not_sent(&mut self, id: u64, text: String) {
        self.answer(id, None, ActionAnswer::NotSent(text));
    }
    pub fn action_unanswered(&mut self, id: u64, text: Option<String>) {
        self.answer(id, None, ActionAnswer::Unanswered(text));
    }
    pub fn action_refused(&mut self, id: u64, refusal: &Refusal) {
        let evidence = refusal.evidence.as_deref();
        if evidence.is_some_and(|e| {
            !e.valid()
                || self
                    .snapshot
                    .as_ref()
                    .is_some_and(|s| s.canonical_account_key != e.canonical_account_key)
        }) {
            return;
        }
        let mut answer = refusal.clone();
        answer.evidence = None;
        self.answer(id, evidence, ActionAnswer::Refused(answer));
    }
    /// The action's own answer, when the admitted state says it is still current.
    fn own(&self, provider: &str) -> Option<(Said, Combine)> {
        let action = self.action.as_ref()?;
        let latest = self.latest();
        if let ActionAnswer::Completed { attempt_key } = &action.answer {
            return latest
                .filter(|r| &r.attempt_key == attempt_key)
                .map(|r| (result_said(r, provider), Combine::Definitive));
        }
        if Mark::of(latest) != action.frontier {
            return None;
        }
        Some(match &action.answer {
            ActionAnswer::Completed { .. } => return None,
            ActionAnswer::Preparing => (
                Said {
                    text: PREPARING.into(),
                    bad: false,
                    check: false,
                },
                Combine::Prefix,
            ),
            ActionAnswer::Refused(r) => (
                refusal_said(r, provider),
                if r.sent == Some(false) {
                    Combine::Definitive
                } else if self.unanswered.contains_key(&action.id) {
                    Combine::Hidden
                } else {
                    Combine::Prefix
                },
            ),
            ActionAnswer::NotSent(text) => (
                Said {
                    text: text.clone(),
                    bad: true,
                    check: true,
                },
                Combine::Definitive,
            ),
            ActionAnswer::Unanswered(reason) => (
                Said {
                    text: unanswered_answer(reason.as_deref()),
                    bad: true,
                    check: true,
                },
                if reason.is_some() {
                    Combine::Prefix
                } else {
                    Combine::Hidden
                },
            ),
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
    /// Admission, U1 and provenance are three separate decisions about one receipt.
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
        // 1. Admission: only a snapshot not older than the admitted one becomes current server state.
        let accepted = self.observe(evidence, read_started_ms);
        if let Some(id) = send {
            // 3. U1, unchanged: only an admitted own receipt acknowledges its send.
            if accepted {
                self.unanswered.remove(&id);
            }
            // 2. Provenance, independent of admission: a claim's own answer names the new attempt it created.
            if self
                .action
                .as_ref()
                .is_some_and(|a| a.frontier.as_ref().is_none_or(|f| order > f.order))
            {
                self.answer(
                    id,
                    Some(evidence),
                    ActionAnswer::Completed {
                        attempt_key: result.attempt_key.clone(),
                    },
                );
            }
        }
        accepted
    }
    pub fn not_sent(&mut self, id: u64) {
        self.unanswered.remove(&id);
    }
    /// 4. The current text: a pure function of the admitted snapshot, the action and U1.
    pub fn current(&self, provider: &str) -> Option<Said> {
        let own = self.own(provider);
        if !self.unanswered.is_empty() {
            let id = self.action.as_ref().map(|a| a.id);
            let only_own = self.unanswered.keys().all(|open| Some(*open) == id);
            return Some(match own {
                Some((said, Combine::Definitive)) if only_own => Said {
                    text: format!("{} {OWN_SEND_WINDOW}", said.text),
                    bad: said.bad,
                    check: true,
                },
                Some((said, Combine::Definitive | Combine::Prefix)) => Said {
                    text: format!("{} {UNANSWERED_SEND}", said.text),
                    bad: true,
                    check: true,
                },
                _ => Said {
                    text: UNANSWERED_SEND.into(),
                    bad: true,
                    check: true,
                },
            });
        }
        if let Some((mut said, _)) = own {
            said.check |= self.needs_refresh()
                || self.read_after_ms.is_some()
                || !self.attempts.is_empty()
                || self.token_deadline().is_some();
            return Some(said);
        }
        let plain_bad = |text: &str| {
            Some(Said {
                text: text.into(),
                bad: true,
                check: true,
            })
        };
        if let Some(changed) = self.source_failure {
            return plain_bad(if changed {
                ACCOUNT_CHANGED_UNAVAILABLE
            } else {
                USAGE_UNAVAILABLE
            });
        }
        if self.refresh_required || self.read_after_ms.is_some() || self.snapshot.is_none() {
            return plain_bad(NEEDS_READ);
        }
        if self.token_deadline().is_some() {
            return plain_bad(TOKENS_OUTSTANDING);
        }
        self.latest()
            .filter(|r| r.pending)
            .and_then(|_| plain_bad(STILL_UNCONFIRMED))
    }
    /// Every durable receipt is dated history, except the one currently said as this context's own answer.
    pub fn older_receipts(&self) -> Vec<String> {
        let own = self.action.as_ref().and_then(|a| match &a.answer {
            ActionAnswer::Completed { attempt_key }
                if self.latest().is_some_and(|r| &r.attempt_key == attempt_key) =>
            {
                Some(attempt_key)
            }
            _ => None,
        });
        let mut rows: BTreeMap<&String, &ClaimResult> =
            self.receipts.iter().map(|(k, (_, r))| (k, r)).collect();
        if let Some(r) = self.latest() {
            rows.insert(&r.attempt_key, r);
        }
        rows.into_iter()
            .filter(|(key, _)| Some(*key) != own)
            .map(|(_, r)| format!("Older claim: {}", super::history_word(&r.outcome)))
            .collect()
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
    /// Provenance and admission are separate: a stale own receipt completes its action without becoming the snapshot,
    /// without lifting U1, and is current only while the admitted state proves the same attempt.
    #[test]
    fn an_own_receipt_completes_its_action_without_admitting_its_snapshot_or_lifting_u1() {
        let t = trace();
        let ev = |name: &str| -> Evidence {
            serde_json::from_value(t[name]["evidence"].clone()).unwrap()
        };
        let retry: ClaimResult = serde_json::from_value(t["retry"].clone()).unwrap();
        let mut state = AccountState::default();
        state.observe(&ev("before"), Some(1.));
        state.begin_send(7, 1000.);
        assert!(state.observe(&ev("afterRetry"), Some(2.)));
        let admitted = state.snapshot.clone();
        assert!(
            !state.receipt(&retry, None, Some(7)),
            "older snapshot is not admitted"
        );
        assert_eq!(state.snapshot, admitted);
        assert!(
            state.unanswered.contains_key(&7),
            "provenance never lifts U1"
        );
        assert_eq!(
            state.action.as_ref().unwrap().answer,
            ActionAnswer::Completed {
                attempt_key: retry.attempt_key.clone()
            }
        );
        assert_eq!(
            state.current("codex").unwrap().text,
            format!("Reset applied and confirmed. {OWN_SEND_WINDOW}")
        );
        // A receipt for another action id, or one naming the frontier attempt itself, is not this action's answer.
        let mut other = AccountState::default();
        other.observe(&ev("before"), Some(1.));
        other.begin_send(8, 1000.);
        other.receipt(&retry, None, Some(9));
        assert_eq!(
            other.action.as_ref().unwrap().answer,
            ActionAnswer::Unanswered(None)
        );
        let mut replay: ClaimResult =
            serde_json::from_value(t["before"]["evidence"]["latestAttempt"].clone()).unwrap();
        replay.evidence = Some(Box::new(ev("recovered")));
        other.receipt(&replay, None, Some(8));
        assert_eq!(
            other.action.as_ref().unwrap().answer,
            ActionAnswer::Unanswered(None)
        );
    }
}
