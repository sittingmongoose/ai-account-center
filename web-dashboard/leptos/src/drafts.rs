//! The owner's unsent choices and notes. Drafts are never answers: they live on this device (browser storage) so
//! leaving the page loses nothing, and they go to the server only from the review screen.
//!
//! A choice remembers the exact card version it was made on, and a note the version it was written on. If the agent
//! changes the question afterwards the draft is kept but marked, and nothing is sent until the owner acts on the new
//! version: the first tap on a changed card starts its choice afresh, and editing the note re-dates only the note. A
//! note never re-dates a choice, so an old choice can't ride along on a changed question.

use crate::api::{AnswerEntry, Card};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const STORAGE_KEY: &str = "aac.status.drafts.v1";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub choice: Vec<String>,
    pub note: String,
    /// "deck", or "recommended" when "Use the recommendations" filled it.
    pub via: String,
    /// The card version (`Card::version_key`) the choice was made on.
    pub version: String,
    /// The card version the note was last written on.
    pub note_version: String,
    /// Random, made with the draft and kept until it is recorded: with the content it makes the entry's `entryId`, so a
    /// retry of this draft is recorded once and a new draft with the same words is a new entry.
    pub nonce: String,
}

/// A new draft's nonce: 128 random bits from the browser (a counter in native tests).
fn fresh_nonce() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        let mut bytes = [0u8; 16];
        let filled = web_sys::window()
            .and_then(|w| w.crypto().ok())
            .and_then(|c| c.get_random_values_with_u8_array(&mut bytes).ok())
            .is_some();
        if !filled {
            for chunk in bytes.chunks_mut(4) {
                let r = (js_sys::Math::random() * f64::from(u32::MAX)) as u32;
                chunk.copy_from_slice(&r.to_le_bytes());
            }
        }
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        format!("{:032x}", NEXT.fetch_add(1, Ordering::Relaxed))
    }
}

/// The draft for a card, made (with its nonce) on first use.
fn draft_for<'a>(drafts: &'a mut Drafts, card: &Card) -> &'a mut Draft {
    let d = drafts.entry(card.id.clone()).or_default();
    if d.nonce.is_empty() {
        d.nonce = fresh_nonce();
    }
    d
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

/// Which parts of a draft were made on an older version of the card: (choice, note).
pub fn stale_parts(drafts: &Drafts, card: &Card) -> (bool, bool) {
    drafts.get(&card.id).map_or((false, false), |d| {
        (
            !d.choice.is_empty() && d.version != card.version_key(),
            !d.note.trim().is_empty() && d.note_version != card.version_key(),
        )
    })
}

pub fn standing(drafts: &Drafts, card: &Card) -> Standing {
    match drafts.get(&card.id) {
        Some(d) if !d.choice.is_empty() && d.version != card.version_key() => Standing::Changed,
        Some(d) if !d.note.trim().is_empty() && d.note_version != card.version_key() => {
            Standing::Changed
        }
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
    let d = draft_for(drafts, card);
    // A choice made on an older version of the question is not carried over: the first tap starts afresh.
    if d.version != card.version_key() {
        d.choice.clear();
    }
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
    let d = draft_for(drafts, card);
    d.note = note;
    d.note_version = card.version_key();
    if d.via.is_empty() {
        d.via = "deck".into();
    }
}

/// Fill every card that has no choice yet with its recommendation (notes are kept).
pub fn use_recommendations(drafts: &mut Drafts, cards: &[Card]) -> usize {
    let mut filled = 0;
    // Only plain questions: never a judgement (the owner's own call), an objection (the owner's own words) or an action.
    for card in cards.iter().filter(|c| c.can_answer() && c.is_question()) {
        let Some(rec) = card.recommendation.as_ref() else {
            continue;
        };
        let d = draft_for(drafts, card);
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
            let mut entry = AnswerEntry {
                entry_id: String::new(),
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
            };
            entry.entry_id = crate::api::entry_id(&d.nonce, &entry);
            entry
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
        .map(|text| parse(&text))
        .unwrap_or_default()
}

/// Read stored drafts. A note saved before notes kept their own version was written on the draft's version.
fn parse(text: &str) -> Drafts {
    let mut drafts: Drafts = serde_json::from_str(text).unwrap_or_default();
    for d in drafts.values_mut() {
        if d.note_version.is_empty() && !d.note.is_empty() {
            d.note_version = d.version.clone();
        }
        if d.nonce.is_empty() {
            d.nonce = fresh_nonce();
        }
    }
    drafts
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
        let mut objection = card("o", 1, Some("A"));
        objection.kind = "objection".into();
        let mut action = card("a", 1, Some("A"));
        action.kind = "action".into();
        let mut d = Drafts::new();
        assert_eq!(use_recommendations(&mut d, &[objection, action]), 0);
        assert!(d.is_empty());
        let cards = vec![sample.clone(), judged.clone()];
        assert_eq!(use_recommendations(&mut d, &cards), 0);
        toggle(&mut d, &sample, "A");
        toggle(&mut d, &judged, "B");
        let sent = entries(&d, &cards);
        assert_eq!(sent.len(), 1);
        assert_eq!(sent[0].card_id, "j");
        assert_eq!(sent[0].kind, "answer");
    }

    #[test]
    fn stale_parts_name_what_changed_and_old_saved_notes_keep_their_version() {
        let old = card("x", 1, None);
        let new = card("x", 2, None);
        let mut d = Drafts::new();
        toggle(&mut d, &old, "A");
        assert_eq!(stale_parts(&d, &new), (true, false));
        set_note(&mut d, &old, "why".into());
        assert_eq!(stale_parts(&d, &new), (true, true));
        set_note(&mut d, &new, "why, still".into());
        assert_eq!(stale_parts(&d, &new), (true, false));
        assert_eq!(standing(&d, &new), Standing::Changed);
        // a draft saved before notes had their own version: the note was written on the draft's version
        let saved = r##"{"x":{"choice":["A"],"note":"why","via":"deck","version":"x#1#h1"}}"##;
        let loaded = parse(saved);
        assert_eq!(loaded["x"].note_version, loaded["x"].version);
        assert_eq!(standing(&loaded, &old), Standing::Ready);
    }

    #[test]
    fn a_draft_keeps_its_entry_id_until_recorded_and_a_new_draft_gets_a_new_one() {
        let a = card("a", 1, None);
        let n = card("n", 1, None);
        let mut d = Drafts::new();
        toggle(&mut d, &a, "A");
        set_note(&mut d, &n, "check the logs".into());
        let both = entries(&d, &[a.clone(), n.clone()]);
        assert_eq!(both.len(), 2);
        // the answered card closes while the response is lost: the note alone goes again with its id
        d.remove("a");
        let alone = entries(&d, &[n.clone()]);
        assert_eq!(alone[0].entry_id, both[1].entry_id);
        // once recorded the draft goes; the same words typed later are a new entry
        d.remove("n");
        set_note(&mut d, &n, "check the logs".into());
        assert_ne!(entries(&d, &[n.clone()])[0].entry_id, both[1].entry_id);
        // an edit is a new entry too
        set_note(&mut d, &n, "check the logs again".into());
        assert_ne!(entries(&d, &[n])[0].entry_id, both[1].entry_id);
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
        // Typing a note on the new version must not re-date the old choice.
        set_note(&mut d, &new, "why?".into());
        assert_eq!(standing(&d, &new), Standing::Changed);
        assert!(entries(&d, &[new.clone()]).is_empty());
        // The first tap on the changed card starts the choice afresh, on the new version.
        toggle(&mut d, &new, "B");
        assert_eq!(d["a"].choice, vec!["B"]);
        assert_eq!(standing(&d, &new), Standing::Ready);
        let sent = entries(&d, &[new]);
        assert_eq!(sent.len(), 1);
        assert_eq!((sent[0].card_seq, sent[0].card_sha256.as_str()), (2, "h2"));
        assert_eq!(sent[0].choice, vec!["B"]);
    }

    #[test]
    fn a_note_written_on_an_older_version_waits_until_edited() {
        let old = card("a", 1, None);
        let new = card("a", 2, None);
        let mut d = Drafts::new();
        set_note(&mut d, &old, "keep it small".into());
        assert_eq!(standing(&d, &new), Standing::Changed);
        assert!(entries(&d, &[new.clone()]).is_empty());
        // "Use recommendations" on the new version neither sends nor re-dates the old note.
        use_recommendations(&mut d, &[new.clone()]);
        assert_eq!(standing(&d, &new), Standing::Changed);
        set_note(&mut d, &new, "keep it small".into());
        assert_eq!(standing(&d, &new), Standing::Ready);
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
