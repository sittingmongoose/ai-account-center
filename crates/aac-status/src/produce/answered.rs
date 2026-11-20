//! G16c: the answered-card helpers the feed producers need.
//!
//! A port of `answered-cards.ts` (`resolvedCardWaits`, `activeBlocker`, `closingCardVersions`)
//! over the readers' rows: an owner wait explicitly linked to a resolved card stops being a
//! wait once that card is closed (answered, judged, objected to or sampled).

use crate::answers_engine::{HistoryEntry, card_version, closing_card_versions};
use serde_json::Value as Json;
use std::collections::{HashMap, HashSet};

/// One card row as the reports reader holds it.
pub struct CardRow<'a> {
    pub writer: &'a str,
    pub doc: &'a Json,
    pub sha256: &'a str,
}

/// `closingCardVersions(answers)` over the feed's history entries.
#[must_use]
pub fn closed_versions(answers: &[HistoryEntry]) -> HashSet<String> {
    closing_card_versions(answers)
}

/// `resolvedCardWaits`: the writers' card references, each true when every card carrying it
/// is resolved (a closed version, or not open).
pub struct ResolvedWaits {
    writers: HashMap<String, HashMap<String, bool>>,
}

impl ResolvedWaits {
    /// `true` when a writer's wait naming `reference` (or a `Card <ref>:` prefix in `words`) is
    /// explained by a resolved card.
    #[must_use]
    pub fn answered(&self, writer: &str, reference: Option<&Json>, words: Option<&Json>) -> bool {
        let named = reference.and_then(Json::as_str).map(str::trim).unwrap_or("");
        let prefix = words.and_then(Json::as_str).and_then(card_prefix);
        let reference = if named.is_empty() { prefix.unwrap_or("") } else { named };
        !reference.is_empty() && self.writers.get(writer).and_then(|refs| refs.get(reference)).copied() == Some(true)
    }
}

fn card_prefix(words: &str) -> Option<&str> {
    let rest = words.strip_prefix("Card ")?;
    let (name, _) = rest.split_once(':')?;
    Some(name)
}

/// `resolvedCardWaits(reports.cards, closed)`.
#[must_use]
pub fn resolved_card_waits(cards: &[CardRow<'_>], closed: &HashSet<String>) -> ResolvedWaits {
    let mut writers: HashMap<String, HashMap<String, bool>> = HashMap::new();
    for row in cards {
        let id = row.doc.get("id").and_then(Json::as_str).unwrap_or("");
        let resolved = row.doc.get("state").and_then(Json::as_str) != Some("open")
            || closed.contains(&card_version(id, row.doc.get("seq").unwrap_or(&Json::Null), row.sha256));
        let label = row.doc.get("label").and_then(Json::as_str);
        let refs = writers.entry(row.writer.to_owned()).or_default();
        for reference in [
            Some(id),
            id.find("--")
                .map(|at| &id[at + 2..])
                .or_else(|| Some(&id[1.min(id.len())..])),
            label,
        ] {
            let Some(reference) = reference.filter(|r| !r.is_empty()) else {
                continue;
            };
            let held = refs.get(reference).copied();
            // If two cards share a label, keep any wait an unresolved card could explain.
            refs.insert(reference.to_owned(), resolved && held != Some(false));
        }
    }
    ResolvedWaits { writers }
}

/// `activeBlocker`: an owner wait explained by a resolved card is no longer a wait.
#[must_use]
pub fn active_blocker(answered: &ResolvedWaits, writer: &str, blocker: Option<Json>) -> Option<Json> {
    let blocker = blocker?;
    if blocker.get("kind").and_then(Json::as_str) == Some("owner")
        && answered.answered(writer, blocker.get("card"), blocker.get("what"))
    {
        return None;
    }
    Some(blocker)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(kind: &str, card_id: &str, seq: u64, sha: &str) -> HistoryEntry {
        HistoryEntry {
            id: format!("e-{kind}"),
            send_id: None,
            shared_only: false,
            answer: json!({
                "kind": kind, "cardId": card_id, "cardSeq": seq, "cardSha256": sha,
                "answeredAt": "2026-11-19T06:00:00.000Z",
            }),
            card: json!({}),
            share: crate::answers_engine::StepState::pending(),
            doorbell: crate::answers_engine::StepState::pending(),
        }
    }

    #[test]
    fn a_closed_card_resolves_a_wait_naming_it_by_id_or_label() {
        let doc = json!({ "id": "vm.lead--Q1", "seq": 2, "state": "open", "label": "S-Q1" });
        let cards = vec![CardRow {
            writer: "vm.lead",
            doc: &doc,
            sha256: "abc",
        }];
        let open = resolved_card_waits(&cards, &HashSet::new());
        assert!(!open.answered("vm.lead", Some(&json!("Q1")), None));
        let closed = resolved_card_waits(&cards, &closed_versions(&[entry("answer", "vm.lead--Q1", 2, "abc")]));
        assert!(closed.answered("vm.lead", Some(&json!("Q1")), None));
        assert!(closed.answered("vm.lead", None, Some(&json!("Card Q1: please pick"))));
        // Another writer's wait is not explained by this writer's card.
        assert!(!closed.answered("mac.worker", Some(&json!("Q1")), None));
        // A wait on an unknown reference is never resolved.
        assert!(!closed.answered("vm.lead", Some(&json!("Q9")), None));
    }

    #[test]
    fn an_owner_wait_on_an_unresolved_card_stands() {
        let doc = json!({ "id": "vm.lead--Q1", "seq": 2, "state": "open" });
        let cards = vec![CardRow {
            writer: "vm.lead",
            doc: &doc,
            sha256: "abc",
        }];
        let waits = resolved_card_waits(&cards, &HashSet::new());
        let blocker = json!({ "kind": "owner", "what": "Choose", "card": "Q1" });
        assert_eq!(
            active_blocker(&waits, "vm.lead", Some(blocker.clone())),
            Some(blocker.clone())
        );
        let waits = resolved_card_waits(&cards, &closed_versions(&[entry("answer", "vm.lead--Q1", 2, "abc")]));
        assert_eq!(active_blocker(&waits, "vm.lead", Some(blocker.clone())), None);
        // Another kind of wait is never resolved by a card.
        assert!(active_blocker(&waits, "vm.lead", Some(json!({"kind":"review","what":"x"}))).is_some());
    }
}
