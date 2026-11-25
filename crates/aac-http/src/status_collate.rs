//! The Status facts' `localeCompare` ranks for every word, not only printable ASCII.
//!
//! `aac_status::produce::facts::ranks` ranks only printable ASCII (the order
//! `aac_num::intl::ascii_compare` proves against Node) and leaves every other word without a
//! rank, so a composer that compares one declines with `Unproven::Facts` (`date`). Real usage
//! window labels hold a middle dot ("Claude and GPT models · Weekly"), which the usage panel's
//! provider/label/window sort compares: every Status read declined. TS ranked every word with
//! `Intl`. This module completes the ranks in the service (aac-status, in the agent's source
//! closure, stays unchanged) with the one total collation in `aac_num::intl`
//! (`collation_key` / `collate`, shared with the Analytics projection): exactly
//! `ascii_compare` for printable ASCII, and any other character above every
//! printable-ASCII primary by code point.
use aac_num::intl;
use indexmap::IndexSet;
use serde_json::{Map, Value};

/// The facts text with `order` holding a rank for every word the sources' data carries
/// (`facts::words_of`, the producer's own word set), ranked by `intl::collation_key`. `None` when a text
/// does not parse (the caller keeps the facts it has).
pub(crate) fn complete_order(sources: &[&str], facts: &str) -> Option<String> {
    let mut values = Vec::with_capacity(sources.len());
    for text in sources {
        values.push(serde_json::from_str::<Value>(text).ok()?);
    }
    complete_order_of(&values.iter().collect::<Vec<_>>(), facts)
}

/// Rank a deduplicated word set into a JSON `{word: rank}` object using `intl::collation_key`.
pub(crate) fn rank_words<'a>(words: impl IntoIterator<Item = &'a String>) -> Map<String, Value> {
    let mut keyed: Vec<((Vec<u32>, Vec<u32>), &String)> = words
        .into_iter()
        .map(|word| (intl::collation_key(word), word))
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
    use std::cmp::Ordering;
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
                    Some(intl::collate(a, b)),
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
