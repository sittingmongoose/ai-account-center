//! The owner's unsent choices and notes. Drafts are never answers: they live on this device (browser storage) so
//! leaving the page loses nothing, and they go to the server only from the review screen.
//!
//! A draft remembers the exact card version it was made on. If the agent changes the question afterwards the draft
//! is kept but marked, and it is not sent until the owner looks at the new version (touching it re-dates it).

use crate::api::{AnswerEntry, Card};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

const STORAGE_KEY: &str = "aac.status.drafts.v1";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub choice: Vec<String>,
    pub note: String,
    /// "deck", or "recommended" when "Use the recommendations" filled it.
    pub via: String,
    /// The card version (`Card::version_key`) the draft was made on.
    pub version: String,
}

impl Draft {
    pub fn has_answer(&self) -> bool {
        !self.choice.is_empty() || !self.note.trim().is_empty()
    }
}

/// Drafts by card id.
pub type Drafts = BTreeMap<String, Draft>;

/// How a card stands in the deck.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Standing {
    Empty,
    Ready,
    /// The question changed after the owner chose; not sent until looked at again.
    Changed,
    /// An objection with no words yet: an objection must say why.
    NeedsWords,
}

pub fn standing(drafts: &Drafts, card: &Card) -> Standing {
    match drafts.get(&card.id) {
        Some(d) if d.has_answer() && d.version != card.version_key() => Standing::Changed,
        Some(d)
            if card.kind == "objection"
                && d.choice.iter().any(|k| k == "object")
                && d.note.trim().is_empty() =>
        {
            Standing::NeedsWords
        }
        Some(d) if d.has_answer() => Standing::Ready,
        _ => Standing::Empty,
    }
}

/// The answer kind the server records: a choice is an answer (or "done" / "object" on those cards), words alone a note.
pub fn entry_kind(card: &Card, draft: &Draft) -> &'static str {
    if draft.choice.is_empty() {
        return "note";
    }
    match card.kind.as_str() {
        "action" => "done",
        "objection" => "object",
        _ => "answer",
    }
}

/// Tap an option: one choice unless the card takes several; tapping the chosen one clears it.
pub fn toggle(drafts: &mut Drafts, card: &Card, key: &str) {
    let d = drafts.entry(card.id.clone()).or_default();
    if card.multiple {
        if let Some(i) = d.choice.iter().position(|k| k == key) {
            d.choice.remove(i);
        } else {
            d.choice.push(key.to_string());
        }
    } else if d.choice.first().map(String::as_str) == Some(key) {
        d.choice.clear();
    } else {
        d.choice = vec![key.to_string()];
    }
    d.via = "deck".into();
    d.version = card.version_key();
}

pub fn set_note(drafts: &mut Drafts, card: &Card, note: String) {
    let d = drafts.entry(card.id.clone()).or_default();
    d.note = note;
    d.version = card.version_key();
    if d.via.is_empty() {
        d.via = "deck".into();
    }
}

/// Fill every card that has no choice yet with its recommendation (notes are kept).
pub fn use_recommendations(drafts: &mut Drafts, cards: &[Card]) -> usize {
    let mut filled = 0;
    for card in cards.iter().filter(|c| c.can_answer() && !c.is_judgement()) {
        let Some(rec) = card.recommendation.as_ref() else {
            continue;
        };
        let d = drafts.entry(card.id.clone()).or_default();
        if d.choice.is_empty() && card.options.iter().any(|o| o.key == rec.key) {
            d.choice = vec![rec.key.clone()];
            d.via = "recommended".into();
            d.version = card.version_key();
            filled += 1;
        }
    }
    filled
}

/// Drop drafts for cards that are no longer open.
pub fn prune(drafts: &mut Drafts, cards: &[Card]) {
    drafts.retain(|id, _| cards.iter().any(|c| &c.id == id));
}

/// What the review screen sends: every card whose draft is ready for this exact version, in deck order.
pub fn entries(drafts: &Drafts, cards: &[Card]) -> Vec<AnswerEntry> {
    cards
        .iter()
        .filter(|c| c.can_answer() && standing(drafts, c) == Standing::Ready)
        .map(|c| {
            let d = &drafts[&c.id];
            let kind = entry_kind(c, d);
            AnswerEntry {
                kind: kind.to_string(),
                card_id: c.id.clone(),
                card_seq: c.seq,
                card_sha256: c.card_sha256.clone(),
                choice: if kind == "answer" {
                    d.choice.clone()
                } else {
                    Vec::new()
                },
                note: d.note.trim().to_string(),
                via: if d.via == "recommended" {
                    "recommended".into()
                } else {
                    "deck".into()
                },
            }
        })
        .collect()
}

fn storage() -> Option<web_sys::Storage> {
    web_sys::window()?.local_storage().ok().flatten()
}

/// Browser storage can be missing or refuse (private mode); drafts then live only for this visit.
pub fn load() -> Drafts {
    storage()
        .and_then(|s| s.get_item(STORAGE_KEY).ok().flatten())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(drafts: &Drafts) {
    if let (Some(s), Ok(text)) = (storage(), serde_json::to_string(drafts)) {
        let _ = s.set_item(STORAGE_KEY, &text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{CardOption, Recommendation};

    fn card(id: &str, seq: u64, rec: Option<&str>) -> Card {
        Card {
            id: id.into(),
            seq,
            card_sha256: format!("h{seq}"),
            options: ["A", "B"]
                .iter()
                .map(|k| CardOption {
                    key: (*k).into(),
                    ..Default::default()
                })
                .collect(),
            recommendation: rec.map(|k| Recommendation {
                key: k.into(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn samples_and_judgements_take_no_recommendation_and_samples_send_nothing() {
        let mut sample = card("s", 1, Some("A"));
        sample.kind = "sample".into();
        let mut judged = card("j", 1, Some("A"));
        judged.kind = "judgement".into();
        let cards = vec![sample.clone(), judged.clone()];
        let mut d = Drafts::new();
        assert_eq!(use_recommendations(&mut d, &cards), 0);
        toggle(&mut d, &sample, "A");
        toggle(&mut d, &judged, "B");
        let sent = entries(&d, &cards);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].card_id, "j");
        assert_eq!(sent[0].kind, "answer");
    }

    #[test]
    fn toggling_picks_one_and_clears_on_a_second_tap() {
        let c = card("x", 1, None);
        let mut d = Drafts::new();
        toggle(&mut d, &c, "A");
        assert_eq!(d["x"].choice, vec!["A"]);
        toggle(&mut d, &c, "B");
        assert_eq!(d["x"].choice, vec!["B"]);
        toggle(&mut d, &c, "B");
        assert!(d["x"].choice.is_empty());
        let mut m = card("m", 1, None);
        m.multiple = true;
        toggle(&mut d, &m, "A");
        toggle(&mut d, &m, "B");
        assert_eq!(d["m"].choice, vec!["A", "B"]);
    }

    #[test]
    fn recommendations_fill_only_empty_cards_and_keep_notes() {
        let cards = vec![
            card("a", 1, Some("A")),
            card("b", 1, Some("B")),
            card("c", 1, None),
        ];
        let mut d = Drafts::new();
        toggle(&mut d, &cards[0], "B");
        set_note(&mut d, &cards[1], "why B?".into());
        assert_eq!(use_recommendations(&mut d, &cards), 1);
        assert_eq!(d["a"].choice, vec!["B"]);
        assert_eq!(d["a"].via, "deck");
        assert_eq!(d["b"].choice, vec!["B"]);
        assert_eq!(d["b"].via, "recommended");
        assert_eq!(d["b"].note, "why B?");
        assert!(!d.contains_key("c") || d["c"].choice.is_empty());
    }

    #[test]
    fn a_changed_question_holds_its_draft_back_until_looked_at() {
        let old = card("a", 1, Some("A"));
        let new = card("a", 2, Some("A"));
        let mut d = Drafts::new();
        toggle(&mut d, &old, "A");
        assert_eq!(standing(&d, &new), Standing::Changed);
        assert!(entries(&d, &[new.clone()]).is_empty());
        set_note(&mut d, &new, String::new());
        assert_eq!(standing(&d, &new), Standing::Ready);
        let sent = entries(&d, &[new]);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].card_seq, sent[0].card_sha256.as_str()), (2, "h2"));
    }

    #[test]
    fn notes_alone_are_sent_and_empty_drafts_are_not() {
        let cards = vec![card("a", 1, None), card("b", 1, None)];
        let mut d = Drafts::new();
        set_note(&mut d, &cards[0], "  a question  ".into());
        set_note(&mut d, &cards[1], "   ".into());
        let sent = entries(&d, &cards);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].note, "a question");
        assert!(sent[0].choice.is_empty());
    }

    #[test]
    fn objections_need_words_and_actions_send_done() {
        let mut objection = card("o", 1, None);
        objection.kind = "objection".into();
        let mut action = card("t", 1, None);
        action.kind = "action".into();
        let mut d = Drafts::new();
        toggle(&mut d, &objection, "object");
        assert_eq!(standing(&d, &objection), Standing::NeedsWords);
        assert!(entries(&d, &[objection.clone()]).is_empty());
        set_note(&mut d, &objection, "Keep it opt-in".into());
        toggle(&mut d, &action, "done");
        let sent = entries(&d, &[objection.clone(), action.clone()]);
        assert_eq!(
            sent.iter().map(|e| e.kind.as_str()).collect::<Vec<_>>(),
            vec!["object", "done"]
        );
        assert!(sent.iter().all(|e| e.choice.is_empty()));
        // Recommendations never fill objections or actions.
        let mut d2 = Drafts::new();
        objection.recommendation = Some(Recommendation {
            key: "object".into(),
            ..Default::default()
        });
        assert_eq!(use_recommendations(&mut d2, &[objection, action]), 0);
    }

    #[test]
    fn pruning_drops_closed_cards() {
        let cards = vec![card("a", 1, None)];
        let mut d = Drafts::new();
        toggle(&mut d, &card("gone", 1, None), "A");
        toggle(&mut d, &cards[0], "A");
        prune(&mut d, &cards);
        assert_eq!(d.keys().collect::<Vec<_>>(), vec!["a"]);
    }
}
