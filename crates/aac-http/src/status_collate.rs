//! The Status facts' `localeCompare` ranks for every word, not only printable ASCII.
//!
//! `aac_status::produce::facts::ranks` ranks only printable ASCII (the order
//! `aac_num::intl::ascii_compare` proves against Node) and leaves every other word without a
//! rank, so a composer that compares one declines with `Unproven::Facts` (`date`). Real usage
//! window labels hold a middle dot ("Claude and GPT models · Weekly"), which the usage panel's
//! provider/label/window sort compares: every Status read declined. TS ranked every word with
//! `Intl`. This module completes the ranks in the service (aac-status, in the agent's source
//! closure, stays unchanged) with the same total collation the Analytics projection uses
//! (`aac_views::analytics_activity::collate`): exactly `ascii_compare` for printable ASCII, and
//! any other character above every printable-ASCII primary by code point.
use aac_num::intl;
use indexmap::IndexSet;
use serde_json::{Map, Value};
#[cfg(test)]
use std::cmp::Ordering;

type CollationKey = (Vec<u32>, Vec<u32>);

/// Primary weight base for characters outside printable ASCII (above every ASCII primary).
const NON_ASCII_PRIMARY: u32 = 256;

/// Precomputed `(primary, tertiary)` table for printable ASCII bytes from `intl::ASCII_ORDER`:
/// one primary per letter pair (lowercase first), else its rank. The weights `intl::ascii_compare`
/// uses, resolved in O(1) instead of scanning `ASCII_ORDER` per character comparison.
const ASCII_WEIGHTS: [Option<(u32, u32)>; 128] = {
    let order = intl::ASCII_ORDER.as_bytes();
    let mut digits_end = 0u32;
    let mut i = 0;
    while i < order.len() {
        if order[i] == b'a' {
            digits_end = i as u32;
            break;
        }
        i += 1;
    }
    let mut table: [Option<(u32, u32)>; 128] = [None; 128];
    let mut at = 0;
    while at < order.len() {
        let c = order[at];
        if (c as usize) < 128 {
            let entry = if c.is_ascii_alphabetic() {
                (
                    digits_end + (at as u32 - digits_end) / 2,
                    if c.is_ascii_uppercase() { 1 } else { 0 },
                )
            } else {
                (at as u32, 0)
            };
            table[c as usize] = Some(entry);
        }
        at += 1;
    }
    table
};

/// `(primary, tertiary)` collation key for one string, computed once per word before sorting.
fn collation_key(s: &str) -> CollationKey {
    let mut primary = Vec::with_capacity(s.len());
    let mut tertiary = Vec::with_capacity(s.len());
    for c in s.chars() {
        let (p, t) = if ('\u{20}'..='\u{7e}').contains(&c) {
            ASCII_WEIGHTS[c as usize].unwrap_or((NON_ASCII_PRIMARY + c as u32, 0))
        } else {
            (NON_ASCII_PRIMARY + c as u32, 0)
        };
        primary.push(p);
        tertiary.push(t);
    }
    (primary, tertiary)
}

/// A total order for any two strings that is exactly `intl::ascii_compare` when both are
/// printable ASCII: primary weights left to right (a prefix first), then tertiary weights.
#[cfg(test)]
pub(crate) fn collate(a: &str, b: &str) -> Ordering {
    let (pa, ta) = collation_key(a);
    let (pb, tb) = collation_key(b);
    pa.cmp(&pb).then_with(|| ta.cmp(&tb))
}

/// The facts text with `order` holding a rank for every word the sources' data carries
/// (`facts::words_of`, the producer's own word set), ranked by `collation_key`. `None` when a text
/// does not parse (the caller keeps the facts it has).
pub(crate) fn complete_order(sources: &[&str], facts: &str) -> Option<String> {
    let mut values = Vec::with_capacity(sources.len());
    for text in sources {
        values.push(serde_json::from_str::<Value>(text).ok()?);
    }
    complete_order_of(&values.iter().collect::<Vec<_>>(), facts)
}

/// Rank a deduplicated word set into a JSON `{word: rank}` object using `collation_key`.
pub(crate) fn rank_words<'a>(words: impl IntoIterator<Item = &'a String>) -> Map<String, Value> {
    let mut keyed: Vec<(CollationKey, &String)> = words
        .into_iter()
        .map(|word| (collation_key(word), word))
        .collect();
    keyed.sort_by(|(a, _), (b, _)| a.cmp(b));
    keyed
        .into_iter()
        .enumerate()
        .map(|(rank, (_, word))| (word.clone(), Value::from(rank)))
        .collect()
}

/// [`complete_order`] over parsed values (the one data object of the Scoreboard facts).
pub(crate) fn complete_order_of(values: &[&Value], facts: &str) -> Option<String> {
    let mut words: IndexSet<String> = IndexSet::new();
    for value in values {
        words.extend(aac_status::produce::facts::words_of(value));
    }
    let mut facts: Value = serde_json::from_str(facts).ok()?;
    let order = rank_words(words.iter());
    *facts.as_object_mut()?.get_mut("order")? = Value::Object(order);
    serde_json::to_string(&facts).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use aac_status::produce::facts::Facts;
    use aac_status::read_context::{Context, Presentation};
    use serde_json::json;
    use std::sync::Arc;

    fn context(facts: &str) -> Context {
        let presentation: Presentation = serde_json::from_str(facts).expect("facts");
        Context::shared(Arc::new(presentation), None)
    }

    /// Live Nov 23: usage window labels with a middle dot had no rank, so the usage panel's
    /// sort marked a fact missing and every Status read declined `date`.
    #[test]
    fn a_non_ascii_label_gets_a_rank_and_ascii_keeps_its_order() {
        let collected = json!({"usage": {"accounts": [{"windows": [
            {"label": "Claude and GPT models \u{b7} Weekly \u{b7} In T3"},
            {"label": "Claude and GPT models \u{b7} 5-hour \u{b7} In T3"},
            {"label": "5 hours \u{b7} Tokens"},
        ]}], "providers": ["codex", "Claude", "a-b", "ab", "aB", "Ab", "a_1", "a-1"]}});
        let reports = json!({"reports": {"cards": [{"title": "Say \u{201c}computer\u{201d}"}]}});
        let texts = [reports.to_string(), collected.to_string()];
        let original = Facts::of(&[&reports, &collected], None).to_text();
        let weekly = "Claude and GPT models \u{b7} Weekly \u{b7} In T3";
        let hourly = "Claude and GPT models \u{b7} 5-hour \u{b7} In T3";
        // Red without the completion: the producer's facts leave the label unranked.
        let before = context(&original);
        let _ = before.compare(weekly, hourly);
        assert!(
            !before.valid(),
            "the producer's ranks miss the middle-dot label"
        );
        // Green: every word ranks, in the collation's order.
        let refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let completed = complete_order(&refs, &original).expect("completed");
        let after = context(&completed);
        assert_eq!(after.compare(hourly, weekly), Ordering::Less);
        assert_eq!(
            after.compare("5 hours \u{b7} Tokens", hourly),
            Ordering::Less
        );
        assert!(after.rank("Say \u{201c}computer\u{201d}").is_some());
        assert!(after.valid());
        // Printable ASCII keeps exactly the producer's (ascii_compare) order.
        let ascii = ["codex", "Claude", "a-b", "ab", "aB", "Ab", "a_1", "a-1"];
        for a in ascii {
            for b in ascii {
                assert_eq!(after.compare(a, b), before.compare(a, b), "{a} {b}");
                assert_eq!(
                    Some(collate(a, b)),
                    aac_num::intl::ascii_compare(a, b),
                    "{a} {b}"
                );
            }
        }
        // The Scoreboard path (parsed values) ranks the same.
        assert_eq!(
            complete_order_of(&[&reports, &collected], &original).as_deref(),
            Some(completed.as_str())
        );
        // Dates and epochs are carried over untouched.
        let o: Value = serde_json::from_str(&original).expect("json");
        let c: Value = serde_json::from_str(&completed).expect("json");
        assert_eq!(o["dates"], c["dates"]);
        assert_eq!(o["epochs"], c["epochs"]);
    }
}
