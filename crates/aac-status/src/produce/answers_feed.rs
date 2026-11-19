//! G16c: the answer history the feed carries.
//!
//! A port of `mergeSharedAnswers` (`answers.ts`): the journal's entries, plus a verified share
//! copy for a card version the journal has not closed, in `answeredAt` order. The journal's own
//! order is oldest first; a stable sort by `Date.parse(answeredAt)` matches the reference (a
//! NaN epoch sorts as 0, as the comparator does).

use crate::answers_engine::{HistoryEntry, SendRecord, closing_card_versions};
use aac_num::ecma;

/// `mergeSharedAnswers(local, shared)`.
#[must_use]
pub fn merge_shared(local: &[HistoryEntry], shared: &[HistoryEntry]) -> Vec<HistoryEntry> {
    if shared.is_empty() {
        return local.to_vec();
    }
    let closed = closing_card_versions(local);
    let recovered: Vec<&HistoryEntry> = shared
        .iter()
        .filter(|entry| {
            let answer = &entry.answer;
            let id = answer.get("cardId").and_then(serde_json::Value::as_str).unwrap_or("");
            let seq = answer.get("cardSeq").unwrap_or(&serde_json::Value::Null);
            let sha = answer
                .get("cardSha256")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            !closed.contains(&crate::answers_engine::card_version(id, seq, sha))
        })
        .collect();
    if recovered.is_empty() {
        return local.to_vec();
    }
    let mut out: Vec<HistoryEntry> = local.iter().cloned().chain(recovered.into_iter().cloned()).collect();
    out.sort_by_key(answered_at);
    out
}

fn answered_at(entry: &HistoryEntry) -> i64 {
    entry
        .answer
        .get("answeredAt")
        .and_then(serde_json::Value::as_str)
        .and_then(ecma::parse_instant)
        .unwrap_or(0)
}

/// The feed's three answer members.
#[derive(Debug, Clone)]
pub struct AnswersFeed {
    pub answers: Vec<HistoryEntry>,
    pub sends: Vec<SendRecord>,
    pub problem: Option<String>,
}

impl AnswersFeed {
    /// The `status-answers` data object (`{answers, sends, answerHistoryProblem}`).
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "answers": self.answers.iter().map(HistoryEntry::to_json).collect::<Vec<_>>(),
            "sends": self.sends.iter().map(SendRecord::to_json).collect::<Vec<_>>(),
            "answerHistoryProblem": self.problem,
        })
    }

    /// The history could not be read: the shared copies stand, no sends, the fixed sentence.
    #[must_use]
    pub fn unavailable(shared: Vec<HistoryEntry>) -> Self {
        Self {
            answers: shared,
            sends: Vec::new(),
            problem: Some("The answer history can't be read right now.".to_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::answers_engine::StepState;
    use serde_json::json;

    fn entry(id: &str, kind: &str, card: &str, seq: u64, sha: &str, at: &str, shared: bool) -> HistoryEntry {
        HistoryEntry {
            id: id.to_owned(),
            send_id: None,
            shared_only: shared,
            answer: json!({
                "kind": kind, "cardId": card, "cardSeq": seq, "cardSha256": sha, "answeredAt": at,
            }),
            card: json!({"id": card, "seq": seq}),
            share: StepState::pending(),
            doorbell: StepState::pending(),
        }
    }

    #[test]
    fn a_shared_copy_fills_a_version_the_journal_has_not_closed() {
        let local = vec![entry(
            "a",
            "answer",
            "vm.lead--Q1",
            1,
            "s1",
            "2026-11-19T06:00:00.000Z",
            false,
        )];
        let shared = vec![
            entry("b", "answer", "vm.lead--Q1", 1, "s1", "2026-11-19T06:00:00.000Z", true),
            entry("c", "answer", "vm.lead--Q2", 1, "s2", "2026-11-19T05:00:00.000Z", true),
        ];
        let merged = merge_shared(&local, &shared);
        assert_eq!(merged.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["c", "a"]);
        // Equal epochs keep the journal entry first (stable).
        let equal = merge_shared(&local, &shared[..1]);
        assert_eq!(equal.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), ["a"]);
    }

    #[test]
    fn no_shared_copy_returns_the_journal_as_it_is() {
        let local = vec![entry(
            "a",
            "note",
            "vm.lead--Q1",
            1,
            "s1",
            "2026-11-19T06:00:00.000Z",
            false,
        )];
        assert_eq!(merge_shared(&local, &[]).len(), 1);
    }

    #[test]
    fn the_feed_data_names_the_three_members() {
        let feed = AnswersFeed {
            answers: Vec::new(),
            sends: Vec::new(),
            problem: None,
        };
        let value = feed.to_json();
        assert_eq!(value["answers"], json!([]));
        assert_eq!(value["sends"], json!([]));
        assert_eq!(value["answerHistoryProblem"], json!(null));
        let unavailable = AnswersFeed::unavailable(Vec::new());
        assert_eq!(
            unavailable.to_json()["answerHistoryProblem"],
            json!("The answer history can't be read right now.")
        );
    }
}
