//! The Scoreboard facts' word ranks: the compared words the service extracts per row,
//! ranked once with the one total collation in `aac_num::intl` (`collation_key` /
//! `collate`, shared with the Analytics projection): exactly `ascii_compare` for printable
//! ASCII, and any other character above every printable-ASCII primary by code point. The
//! Status facts need no completing pass here: `aac_status::produce::facts::ranks` ranks
//! every word with the same collation at the producer.
use aac_num::intl;
use serde_json::{Map, Value};

/// Rank a deduplicated word set into a JSON `{word: rank}` object using
/// `intl::collation_key` (each word weighed once, then sorted by its key).
pub(crate) fn rank_words<'a>(words: impl IntoIterator<Item = &'a String>) -> Map<String, Value> {
    let mut keyed: Vec<(intl::CollationKey, &String)> = words
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
    /// sort marked a fact missing and every Status read declined `date`. The producer now
    /// ranks every word itself; `rank_words` (the Scoreboard facts' ranking) agrees with it.
    #[test]
    fn the_producer_ranks_every_word_and_rank_words_agrees() {
        let collected = json!({"usage": {"accounts": [{"windows": [
            {"label": "Claude and GPT models \u{b7} Weekly \u{b7} In T3"},
            {"label": "Claude and GPT models \u{b7} 5-hour \u{b7} In T3"},
            {"label": "5 hours \u{b7} Tokens"},
        ]}], "providers": ["codex", "Claude", "a-b", "ab", "aB", "Ab", "a_1", "a-1"]}});
        let reports = json!({"reports": {"cards": [{"title": "Say \u{201c}computer\u{201d}"}]}});
        let facts = Facts::of(&[&reports, &collected], None);
        let weekly = "Claude and GPT models \u{b7} Weekly \u{b7} In T3";
        let hourly = "Claude and GPT models \u{b7} 5-hour \u{b7} In T3";
        let ctx = context(&facts.to_text());
        assert_eq!(ctx.compare(hourly, weekly), Ordering::Less);
        assert_eq!(ctx.compare("5 hours \u{b7} Tokens", hourly), Ordering::Less);
        assert!(ctx.rank("Say \u{201c}computer\u{201d}").is_some());
        assert!(ctx.valid(), "no completing pass is needed any more");
        // Printable ASCII keeps exactly the proved (ascii_compare) order.
        let ascii = ["codex", "Claude", "a-b", "ab", "aB", "Ab", "a_1", "a-1"];
        for a in ascii {
            for b in ascii {
                assert_eq!(
                    Some(ctx.compare(a, b)),
                    aac_num::intl::ascii_compare(a, b),
                    "{a} {b}"
                );
            }
        }
        assert!(ctx.valid());
        // rank_words over the same words gives the producer's ranks.
        let ranked = rank_words(facts.order.keys());
        for (word, rank) in &facts.order {
            assert_eq!(ranked.get(word), Some(&Value::from(*rank)), "{word}");
        }
    }
}
