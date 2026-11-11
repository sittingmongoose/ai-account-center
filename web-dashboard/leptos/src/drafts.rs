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
    /// Entry ids already recorded from this nonce while the draft was kept (another tab changed it mid-send). An edit
    /// that would make one of them again takes a fresh nonce, so the same words are a new answer, not a dropped retry.
    pub recorded: Vec<String>,
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

/// Whether an entry of this kind closes its card once recorded: an answer, "done", and words sent without a choice
/// (the wire kind stays "note"; the owner's written response is the answer). An objection leaves the card open.
pub fn closes(kind: &str) -> bool {
    matches!(kind, "answer" | "done" | "note")
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
    refresh_nonce(drafts, card);
}

pub fn set_note(drafts: &mut Drafts, card: &Card, note: String) {
    let d = draft_for(drafts, card);
    d.note = note;
    d.note_version = card.version_key();
    if d.via.is_empty() {
        d.via = "deck".into();
    }
    refresh_nonce(drafts, card);
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

/// After an entry was recorded: drop its card's draft, unless the draft has changed since that entry was made from it
/// (another tab edited it while the send was on its way), in which case the newer draft stays.
pub fn forget_sent(drafts: &mut Drafts, cards: &[Card], sent: &AnswerEntry) {
    let Some(card) = cards.iter().find(|c| c.id == sent.card_id) else {
        return;
    };
    let unchanged = entries(drafts, std::slice::from_ref(card))
        .first()
        .is_some_and(|now| now.entry_id == sent.entry_id);
    let Some(d) = drafts.get_mut(&sent.card_id) else {
        return;
    };
    if unchanged {
        // Recorded: the words go, but the draft stays as an empty marker with its nonce and the recorded id, so a tab
        // whose newer words win the merge still knows that id, and the same words typed again are a new answer.
        d.choice.clear();
        d.note.clear();
        d.via.clear();
    }
    // Keep the nonce (another tab's send of this very draft must still match its own result), and remember the
    // recorded id: an edit that would make it again takes a fresh nonce. Past RECORDED_KEPT ids the nonce does
    // rotate; another tab's send in flight on the old nonce then stays a ready draft once recorded (a known limit:
    // it takes nine sends of one card and a race between tabs).
    join_recorded(d, std::slice::from_ref(&sent.entry_id));
}

/// How many recorded ids a draft keeps for its nonce.
const RECORDED_KEPT: usize = 8;

/// Add recorded ids to a draft. Past the limit it starts a fresh nonce with none, rather than forget the oldest: a
/// forgotten id could be made again and dropped as a retry.
fn join_recorded(d: &mut Draft, ids: &[String]) {
    for id in ids {
        if !d.recorded.contains(id) {
            d.recorded.push(id.clone());
        }
    }
    if d.recorded.len() > RECORDED_KEPT {
        d.nonce = fresh_nonce();
        d.recorded.clear();
    }
}

/// After an edit: if the draft would now make an entry already recorded from its nonce, start a new nonce.
fn refresh_nonce(drafts: &mut Drafts, card: &Card) {
    let Some(d) = drafts.get_mut(&card.id) else {
        return;
    };
    if !d.recorded.is_empty() && d.recorded.contains(&entry_from(card, d).entry_id) {
        d.nonce = fresh_nonce();
        d.recorded.clear();
    }
}

/// Drop drafts for cards that are no longer open.
pub fn prune(drafts: &mut Drafts, cards: &[Card]) {
    drafts.retain(|id, _| cards.iter().any(|c| &c.id == id));
}

/// Before a send: a draft whose entry would remake an id already recorded from its nonce (an edit back to recorded
/// words, or ids joined from another tab) takes a fresh nonce, so it goes as the new answer it is.
pub fn refresh_recorded(drafts: &mut Drafts, cards: &[Card]) {
    for card in cards {
        refresh_nonce(drafts, card);
    }
}

/// What the review screen sends: every card whose draft is ready for this exact version, in deck order.
pub fn entries(drafts: &Drafts, cards: &[Card]) -> Vec<AnswerEntry> {
    cards
        .iter()
        .filter(|c| c.can_answer() && standing(drafts, c) == Standing::Ready)
        .map(|c| entry_from(c, &drafts[&c.id]))
        .collect()
}

/// The entry a draft makes for its card, with its id.
fn entry_from(c: &Card, d: &Draft) -> AnswerEntry {
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

/// Merge this tab's changes into what storage holds now. Each card this tab changed since `base` (its last read of
/// storage) takes this tab's draft, or goes if this tab dropped it; every other card keeps what storage holds, so two
/// tabs editing different cards both keep their work, and a send or a prune in one tab removes only its own cards. A
/// drop never removes a card another tab changed meanwhile (storage's copy differs from `base`): that newer edit stays.
/// When both keep the same draft (same nonce), the entry ids each has recorded are joined.
pub fn merge(stored: &Drafts, base: &Drafts, current: &Drafts) -> Drafts {
    let mut out = stored.clone();
    for id in base.keys().chain(current.keys()) {
        if base.get(id) == current.get(id) {
            continue;
        }
        // A send's result is not an edit of the words: a change of recorded ids alone, or this tab's recorded draft
        // emptied while another tab wrote newer words, keeps storage's words and joins the ids.
        if let (Some(b), Some(c), Some(theirs)) = (base.get(id), current.get(id), stored.get(id)) {
            let words = |d: &Draft| Draft {
                recorded: Vec::new(),
                ..d.clone()
            };
            // emptied by a send's result (its ids grew), not cleared by the owner, while another tab wrote newer
            // words: those words stay, whatever their nonce
            let emptied_by_send =
                !c.has_answer() && c.recorded != b.recorded && words(theirs) != words(b);
            // this tab's change was ids only: the same words, maybe under a nonce its send just rotated
            let without_nonce = |d: &Draft| Draft {
                nonce: String::new(),
                ..words(d)
            };
            let ids_only = words(b) == words(c)
                || (c.nonce != b.nonce && without_nonce(b) == without_nonce(c));
            if ids_only || emptied_by_send {
                let mut kept = theirs.clone();
                if kept.nonce == c.nonce {
                    // ids only mean something to the nonce they were made from
                    join_recorded(&mut kept, &c.recorded);
                } else if c.nonce != b.nonce && kept.nonce == b.nonce {
                    // this tab's send rotated the nonce (too many ids): the kept words move to the new nonce with it,
                    // or the id that overflowed would be lost and those words remade as a dropped retry
                    kept.nonce = c.nonce.clone();
                    kept.recorded = c.recorded.clone();
                }
                out.insert(id.clone(), kept);
                continue;
            }
        }
        match current.get(id) {
            Some(d) => {
                let mut mine = d.clone();
                if let Some(theirs) = stored.get(id).filter(|t| t.nonce == mine.nonce) {
                    join_recorded(&mut mine, &theirs.recorded);
                }
                out.insert(id.clone(), mine);
            }
            None => {
                if stored.get(id) == base.get(id) {
                    out.remove(id);
                }
            }
        }
    }
    out
}

/// What storage holds, or `None` when storage is missing or refuses the read (never read as "no drafts").
pub fn load_checked() -> Option<Drafts> {
    match storage()?.get_item(STORAGE_KEY) {
        Ok(Some(text)) => Some(parse(&text)),
        Ok(None) => Some(Drafts::new()),
        Err(_) => None,
    }
}

/// Save this tab's drafts merged with storage's (read and written in one go, so no other tab's write lands between).
/// `Some(what storage now holds)` once written; `None` when storage is missing or refused the read or the write, and
/// then nothing is assumed stored: the caller keeps its drafts and its base as they were.
pub fn save_merged(base: &Drafts, current: &Drafts) -> Option<Drafts> {
    let s = storage()?;
    let stored = match s.get_item(STORAGE_KEY) {
        Ok(Some(text)) => parse(&text),
        Ok(None) => Drafts::new(),
        Err(_) => return None,
    };
    let merged = merge(&stored, base, current);
    let text = serde_json::to_string(&merged).ok()?;
    s.set_item(STORAGE_KEY, &text).ok()?;
    Some(merged)
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
    fn a_recorded_send_keeps_a_draft_another_tab_changed_meanwhile() {
        let n = card("n", 1, None);
        let mut d = Drafts::new();
        set_note(&mut d, &n, "first words".into());
        let sent = entries(&d, &[n.clone()]).remove(0);
        // unchanged: the recorded draft goes
        let mut same = d.clone();
        forget_sent(&mut same, &[n.clone()], &sent);
        assert!(!same["n"].has_answer(), "the recorded words go");
        assert_eq!(same["n"].recorded, vec![sent.entry_id.clone()]);
        // another tab edited it while the send was on its way: the newer words stay
        set_note(&mut d, &n, "second thoughts".into());
        forget_sent(&mut d, &[n.clone()], &sent);
        assert_eq!(d["n"].note, "second thoughts");
        // the owner later types the recorded words again: a new entry, not a retry of the recorded one
        let mut retyped = d.clone();
        set_note(&mut retyped, &n, "first words".into());
        assert_ne!(entries(&retyped, &[n.clone()])[0].entry_id, sent.entry_id);
        // the other tab sent its words before this tab's reply came back: its own reply still matches and clears it
        let theirs = entries(&d, &[n.clone()]).remove(0);
        forget_sent(&mut d, &[n], &theirs);
        assert!(
            !d["n"].has_answer(),
            "the other tab's recorded words go too"
        );
    }

    #[test]
    fn two_tabs_editing_different_cards_both_keep_their_drafts() {
        let a = card("a", 1, None);
        let b = card("b", 1, None);
        let c = card("c", 1, None);
        let base = Drafts::new();
        // tab 1 writes a; tab 2, which read storage before that, writes b
        let mut one = base.clone();
        set_note(&mut one, &a, "from tab one".into());
        let stored = merge(&Drafts::new(), &base, &one);
        let mut two = base.clone();
        set_note(&mut two, &b, "from tab two".into());
        let stored = merge(&stored, &base, &two);
        assert_eq!(stored["a"].note, "from tab one");
        assert_eq!(stored["b"].note, "from tab two");
        // a send in tab 2 removes only its own card; a third card another tab added meanwhile stays
        let mut later = stored.clone();
        set_note(&mut later, &c, "from tab three".into());
        let base_two = stored.clone();
        let mut sent = base_two.clone();
        sent.remove("b");
        let stored = merge(&later, &base_two, &sent);
        assert_eq!(stored.keys().cloned().collect::<Vec<_>>(), vec!["a", "c"]);
    }

    #[test]
    fn a_drop_keeps_another_tabs_newer_edit_and_recorded_ids_join() {
        let x = card("x", 1, None);
        let mut base = Drafts::new();
        set_note(&mut base, &x, "first".into());
        // another tab edited x; this tab, not yet told, sends it and drops its draft
        let mut stored = base.clone();
        set_note(&mut stored, &x, "their newer words".into());
        let mut current = base.clone();
        current.remove("x");
        assert_eq!(
            merge(&stored, &base, &current)["x"].note,
            "their newer words"
        );
        // both keep x with the same nonce: their recorded ids and mine are joined
        let mut theirs = base.clone();
        theirs.get_mut("x").unwrap().recorded = vec!["r1".into()];
        let mut mine = base.clone();
        mine.get_mut("x").unwrap().note = "mine".into();
        mine.get_mut("x").unwrap().recorded = vec!["r2".into()];
        let joined = merge(&theirs, &base, &mine);
        assert_eq!(joined["x"].recorded, vec!["r2", "r1"]);
    }

    #[test]
    fn a_result_alone_keeps_another_tabs_words_and_a_resend_never_remakes_a_recorded_id() {
        let x = card("x", 1, None);
        let mut base = Drafts::new();
        set_note(&mut base, &x, "W1".into());
        // tab A wrote W2; tab B, not yet told, only notes a recorded id on its W1
        let mut stored = base.clone();
        set_note(&mut stored, &x, "W2".into());
        let mut current = base.clone();
        current.get_mut("x").unwrap().recorded = vec!["r0".into()];
        let merged = merge(&stored, &base, &current);
        assert_eq!(merged["x"].note, "W2");
        assert_eq!(merged["x"].recorded, vec!["r0"]);
        // ids joined onto a draft that already makes one of them: the send takes a fresh nonce
        let mut d = Drafts::new();
        set_note(&mut d, &x, "W0".into());
        let made = entries(&d, &[x.clone()]).remove(0).entry_id;
        d.get_mut("x").unwrap().recorded = vec![made.clone()];
        refresh_recorded(&mut d, &[x.clone()]);
        assert_ne!(entries(&d, &[x])[0].entry_id, made);
    }

    #[test]
    fn a_tab_that_wrote_newer_words_learns_the_recorded_id_of_the_send_it_raced() {
        let x = card("x", 1, None);
        // tab A sends W0; tab B, with A's nonce, writes W1 before A's result lands
        let mut base = Drafts::new();
        set_note(&mut base, &x, "W0".into());
        let sent = entries(&base, &[x.clone()]).remove(0);
        let mut stored = base.clone();
        set_note(&mut stored, &x, "W1".into());
        let mut a = base.clone();
        forget_sent(&mut a, &[x.clone()], &sent);
        let merged = merge(&stored, &base, &a);
        assert_eq!(merged["x"].note, "W1", "B's newer words stay");
        assert!(merged["x"].recorded.contains(&sent.entry_id));
        // B later types W0 again: a new answer, not a retry of the recorded one
        let mut b = merged.clone();
        set_note(&mut b, &x, "W0".into());
        assert_ne!(entries(&b, &[x])[0].entry_id, sent.entry_id);
    }

    #[test]
    fn a_send_result_never_wipes_another_tabs_words_and_old_ids_never_return() {
        let x = card("x", 1, None);
        // F2: A's send of W1 is in flight; B retypes the recorded W0 and takes a fresh nonce; A's result lands
        let mut base = Drafts::new();
        set_note(&mut base, &x, "W1".into());
        let sent = entries(&base, &[x.clone()]).remove(0);
        let mut stored = base.clone();
        set_note(&mut stored, &x, "W0".into());
        stored.get_mut("x").unwrap().nonce = "other".into();
        let mut a = base.clone();
        forget_sent(&mut a, &[x.clone()], &sent);
        let merged = merge(&stored, &base, &a);
        assert_eq!(merged["x"].note, "W0", "B's unsent words stay");
        assert!(
            merged["x"].recorded.is_empty(),
            "A's id means nothing to B's nonce"
        );
        // F4: the owner clears a note while another tab only noted a recorded id: the clear stands
        let mut cleared = base.clone();
        set_note(&mut cleared, &x, String::new());
        let mut theirs = base.clone();
        theirs.get_mut("x").unwrap().recorded = vec!["r".into()];
        assert_eq!(merge(&theirs, &base, &cleared)["x"].note, "");
        // F3: past the limit a draft starts a fresh nonce instead of forgetting its oldest id
        let mut d = Drafts::new();
        set_note(&mut d, &x, "first".into());
        let first = entries(&d, &[x.clone()]).remove(0);
        forget_sent(&mut d, &[x.clone()], &first);
        for i in 0..RECORDED_KEPT {
            set_note(&mut d, &x, format!("note {i}"));
            let e = entries(&d, &[x.clone()]).remove(0);
            forget_sent(&mut d, &[x.clone()], &e);
        }
        set_note(&mut d, &x, "first".into());
        assert_ne!(entries(&d, &[x])[0].entry_id, first.entry_id);
    }

    #[test]
    fn a_rotation_or_an_ids_only_change_in_storage_never_brings_sent_words_back() {
        let x = card("x", 1, None);
        // N3: storage differs from base only in recorded ids; this tab's send empties the draft: it stays empty
        let mut base = Drafts::new();
        set_note(&mut base, &x, "W1".into());
        let sent = entries(&base, &[x.clone()]).remove(0);
        let mut stored = base.clone();
        stored.get_mut("x").unwrap().recorded = vec!["other".into()];
        let mut a = base.clone();
        forget_sent(&mut a, &[x.clone()], &sent);
        assert!(!merge(&stored, &base, &a)["x"].has_answer());
        // N2: the 9th id rotates this tab's nonce while another tab wrote newer words on the old one
        let mut full = Drafts::new();
        set_note(&mut full, &x, "W".into());
        full.get_mut("x").unwrap().recorded = (0..RECORDED_KEPT).map(|i| format!("r{i}")).collect();
        let ninth = entries(&full, &[x.clone()]).remove(0);
        let mut theirs = full.clone();
        set_note(&mut theirs, &x, "newer".into());
        let mut mine = full.clone();
        forget_sent(&mut mine, &[x.clone()], &ninth);
        assert_ne!(mine["x"].nonce, full["x"].nonce, "the overflow rotated");
        let merged = merge(&theirs, &full, &mine);
        assert_eq!(merged["x"].note, "newer");
        assert_eq!(
            merged["x"].nonce, mine["x"].nonce,
            "the kept words move to the new nonce"
        );
        // the ninth sent words typed again are a new answer
        let mut again = merged.clone();
        set_note(&mut again, &x, "W".into());
        assert_ne!(entries(&again, &[x])[0].entry_id, ninth.entry_id);
    }

    #[test]
    fn a_send_that_rotates_the_nonce_never_writes_over_another_tabs_words() {
        let x = card("x", 1, None);
        // this tab holds W2 with a full set of ids; a send of its older W1 lands and rotates the nonce
        let mut base = Drafts::new();
        set_note(&mut base, &x, "W2".into());
        base.get_mut("x").unwrap().recorded = (0..RECORDED_KEPT).map(|i| format!("r{i}")).collect();
        let older = AnswerEntry {
            entry_id: "w1".into(),
            ..entries(&base, &[x.clone()]).remove(0)
        };
        let mut mine = base.clone();
        forget_sent(&mut mine, &[x.clone()], &older);
        assert_ne!(mine["x"].nonce, base["x"].nonce);
        // another tab wrote W3 meanwhile: it stays, on the new nonce
        let mut theirs = base.clone();
        set_note(&mut theirs, &x, "W3".into());
        let merged = merge(&theirs, &base, &mine);
        assert_eq!(merged["x"].note, "W3");
        assert_eq!(merged["x"].nonce, mine["x"].nonce);
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

    #[test]
    fn a_written_response_closes_its_card_and_keeps_the_note_kind() {
        let card = Card { id: "c".into(), ..Default::default() };
        let draft = Draft { note: "Use the other approach instead".into(), ..Default::default() };
        // no option is invented: the wire kind is "note" with no choice
        assert_eq!(entry_kind(&card, &draft), "note");
        assert!(closes("note") && closes("answer") && closes("done"));
        assert!(!closes("object"));
    }
}
