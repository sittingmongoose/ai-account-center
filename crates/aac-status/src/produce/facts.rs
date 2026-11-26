//! G16c: the Status presentation facts, computed here instead of the TypeScript envelope.
//!
//! The fed Status generations carry one `status-facts` generation whose facts are
//! `{dates, epochs, order}`: the clock and day words of every instant, the `Date.parse`
//! epochs of every ISO-shaped string, and the `localeCompare` ranks of every word. The
//! TypeScript `status-feed.ts` computed all three with this engine's `Intl`
//! (`rust-read-input.ts` `readFacts` / `localeRanks` / `dateWordsIn`). This module computes
//! them from the same values, with one boundary: the display zone's UTC offsets (an engine
//! fact no server can reproduce from a zone name alone) come in as rows, and every word is
//! then written from them (`aac_num::intl::Zone`, the layer Home's fed path already uses).
//!
//! Domains, as everywhere in this crate: a time whose ISO text or zone word is outside the
//! proved window keeps no date fact, and a composer that then reads that fact declines, and
//! the TypeScript handler answers. Every word ranks, by the one total collation
//! (`aac_num::intl::collate`): printable ASCII in its proved `localeCompare` order, any other
//! word past it by code point. No fact is guessed and none is defaulted.

use aac_num::ecma;
use aac_num::intl::Zone;
use indexmap::{IndexMap, IndexSet};
use serde::Deserialize;
use serde_json::Value as Json;

/// One instant's date facts: `new Date(ms).toISOString()`, and the display zone's
/// `{hour:'numeric', minute:'2-digit'}` and `{month:'short', day:'numeric'}` words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DateWords {
    pub iso: String,
    pub clock: String,
    pub day: String,
}

/// The display zone's offsets as the front door's `Intl` found them (`serverZoneFacts`): a
/// window `[from, to]` and `(start ms, offset minutes)` rows over it, starts increasing.
#[derive(Debug, Clone, Deserialize)]
pub struct ZoneFacts {
    pub from: i64,
    pub to: i64,
    pub offsets: Vec<[i64; 2]>,
}

impl ZoneFacts {
    /// The zone, or `None` when the rows are outside the proved shape (then the request
    /// declines `fact` and TypeScript answers).
    pub fn zone(&self) -> Option<Zone> {
        Zone::proven(self.from, self.to, &self.offsets)
    }
}

/// What one parsed value contributes: the words it ranks, the instants it names, and the
/// epochs of its ISO strings. Insertion order is the order facts are written in (the
/// TypeScript envelope's first-seen order; this crate's JSON objects are key-sorted, so the
/// order can differ from Node's without changing any fact's value).
#[derive(Debug, Default)]
pub struct Acc {
    words: IndexSet<String>,
    times: IndexSet<i64>,
    epochs: IndexMap<String, i64>,
}

impl Acc {
    /// `readFacts`: every string as a word (and its part after the first `.`), every
    /// ISO-shaped string's finite `Date.parse`, and every safe-integer millisecond instant.
    /// Traversal walks objects in key order and arrays in order.
    pub fn read(&mut self, value: &Json, rank_all: bool) {
        match value {
            Json::String(text) => self.read_text(text, rank_all),
            Json::Number(number) => {
                if let Some(n) = number.as_f64() {
                    self.read_number(n);
                }
            }
            Json::Array(items) => {
                for item in items {
                    self.read(item, rank_all);
                }
            }
            Json::Object(map) => {
                for item in map.values() {
                    self.read(item, rank_all);
                }
            }
            Json::Null | Json::Bool(_) => {}
        }
    }
    pub(crate) fn read_text(&mut self, text: &str, rank_all: bool) {
        if rank_all {
            if !self.words.contains(text) {
                self.words.insert(text.to_owned());
            }
            if let Some((_, rest)) = text.split_once('.')
                && !self.words.contains(rest)
            {
                self.words.insert(rest.to_owned());
            }
        }
        if ecma::iso_prefixed(text)
            && !self.epochs.contains_key(text)
            && let Some(ms) = ecma::parse_instant(text)
        {
            self.epochs.insert(text.to_owned(), ms);
            self.times.insert(ms);
        }
    }

    pub(crate) fn read_number(&mut self, n: f64) {
        if n.fract() == 0.0 && (100_000_000_000.0..=8_640_000_000_000_000.0).contains(&n) {
            #[allow(clippy::cast_possible_truncation)]
            self.times.insert(n as i64);
        }
    }
}

/// The facts one set of generations contributes: `readFacts`' union plus the words' ranks.
#[derive(Debug, Default, Clone)]
pub struct Facts {
    pub dates: IndexMap<i64, DateWords>,
    pub epochs: IndexMap<String, i64>,
    pub order: IndexMap<String, usize>,
}

impl Facts {
    /// The facts of `values` (the feed sources' data, in the envelope's source order) in
    /// `zone`.
    pub fn of(values: &[&Json], zone: Option<&Zone>) -> Facts {
        let mut acc = Acc::default();
        for value in values {
            acc.read(value, true);
        }
        Facts::build(&acc, zone)
    }

    /// The facts of one accumulator. A time the engine cannot write (`toISOString` outside
    /// its domain, or a zone word outside the offsets' window) keeps no date fact.
    #[must_use]
    pub fn build(acc: &Acc, zone: Option<&Zone>) -> Facts {
        let (dates, epochs) = Self::dates_epochs(acc, zone);
        Facts {
            dates,
            epochs,
            order: ranks(acc.words.iter()),
        }
    }

    /// The date and epoch facts alone, for a caller that ranks its own word set (the
    /// Scoreboard facts rank the compared words extracted per row, not every word the
    /// accumulator read, so ranking here too would sort twice and keep neither).
    #[must_use]
    pub fn dates_epochs(acc: &Acc, zone: Option<&Zone>) -> (IndexMap<i64, DateWords>, IndexMap<String, i64>) {
        let mut dates = IndexMap::new();
        for ms in &acc.times {
            let (Some(iso), Some((clock, day))) = (ecma::to_iso_string(*ms), zone.and_then(|z| z.words(*ms))) else {
                continue;
            };
            dates.insert(*ms, DateWords { iso, clock, day });
        }
        (dates, acc.epochs.clone())
    }

    pub(crate) fn into_presentation(self) -> crate::read_context::Presentation {
        crate::read_context::Presentation::from_facts(self)
    }

    /// The facts object (`status-facts`' facts text). Written here, in this order, so the
    /// service's feed body is one function's output: `JSON.stringify` of the reference.
    #[must_use]
    pub fn to_json(&self) -> Json {
        let dates: serde_json::Map<String, Json> = self
            .dates
            .iter()
            .map(|(ms, date)| (ms.to_string(), json_date(date)))
            .collect();
        let epochs: serde_json::Map<String, Json> = self
            .epochs
            .iter()
            .map(|(text, ms)| (text.clone(), Json::from(*ms)))
            .collect();
        let order: serde_json::Map<String, Json> = self
            .order
            .iter()
            .map(|(word, rank)| (word.clone(), Json::from(*rank)))
            .collect();
        let mut out = serde_json::Map::new();
        out.insert("dates".into(), Json::Object(dates));
        out.insert("epochs".into(), Json::Object(epochs));
        out.insert("order".into(), Json::Object(order));
        Json::Object(out)
    }

    /// The facts text `JSON.stringify` writes, in Node's member order (`dates`, `epochs`,
    /// `order`) and each map in first-seen order.
    #[must_use]
    pub fn to_text(&self) -> String {
        let dates: Vec<String> = self
            .dates
            .iter()
            .map(|(ms, date)| {
                format!(
                    "{}:{{\"iso\":{},\"clock\":{},\"day\":{}}}",
                    quote(&ms.to_string()),
                    quote(&date.iso),
                    quote(&date.clock),
                    quote(&date.day)
                )
            })
            .collect();
        let epochs: Vec<String> = self
            .epochs
            .iter()
            .map(|(text, ms)| format!("{}:{}", quote(text), ms))
            .collect();
        let order: Vec<String> = self
            .order
            .iter()
            .map(|(word, rank)| format!("{}:{}", quote(word), rank))
            .collect();
        format!(
            "{{\"dates\":{{{}}},\"epochs\":{{{}}},\"order\":{{{}}}}}",
            dates.join(","),
            epochs.join(","),
            order.join(",")
        )
    }
}

fn json_date(date: &DateWords) -> Json {
    let mut map = serde_json::Map::new();
    map.insert("iso".into(), Json::from(date.iso.clone()));
    map.insert("clock".into(), Json::from(date.clock.clone()));
    map.insert("day".into(), Json::from(date.day.clone()));
    Json::Object(map)
}

/// `JSON.stringify`'s text for one string (the escaping serde_json uses is Node's for every
/// character these words carry; a character neither admits is not a word that reaches here).
fn quote(text: &str) -> String {
    Json::String(text.to_owned()).to_string()
}

/// `localeRanks`: every word ranked once, by the one total collation
/// (`aac_num::intl::collation_key`): printable ASCII in its proved `localeCompare` order
/// (`aac_num::intl::ascii_compare`, checked against Node), any other word past it by code
/// point. Each word is weighed once (its key), not twice per comparison, and no second
/// ranking overwrites this one: the service publishes these ranks as they are.
#[must_use]
pub fn ranks<'a>(words: impl IntoIterator<Item = &'a String>) -> IndexMap<String, usize> {
    let mut seen = IndexSet::new();
    let mut keyed: Vec<(aac_num::intl::CollationKey, &String)> = Vec::new();
    for word in words {
        if seen.insert(word.as_str()) {
            keyed.push((aac_num::intl::collation_key(word), word));
        }
    }
    keyed.sort_by(|a, b| a.0.cmp(&b.0));
    keyed
        .into_iter()
        .enumerate()
        .map(|(rank, (_, word))| (word.clone(), rank))
        .collect()
}

/// The words one value contributes, for a caller that ranks a smaller set (the envelope's
/// `extras`); Status ranks every word, so the producers use [`Facts::of`].
#[must_use]
pub fn words_of(value: &Json) -> IndexSet<String> {
    let mut acc = Acc::default();
    acc.read(value, true);
    acc.words
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn zone() -> Zone {
        // UTC, one row, a wide window.
        ZoneFacts {
            from: 1_700_000_000_000,
            to: 1_900_000_000_000,
            offsets: vec![[1_700_000_000_000, 0]],
        }
        .zone()
        .unwrap()
    }

    #[test]
    fn facts_read_words_times_and_epochs_like_the_envelope() {
        let data = json!({
            "value": "2026-11-14T12:00:00.000Z",
            "name": "vm.lead",
            "at": 1_790_000_000_000_i64,
            "nested": ["alpha", {"inner": "a.b"}],
            "skip": 1.5,
        });
        let facts = Facts::of(&[&data], Some(&zone()));
        assert_eq!(facts.epochs.get("2026-11-14T12:00:00.000Z"), Some(&1_794_657_600_000));
        // The epoch and the safe-integer instant both become date facts.
        assert!(facts.dates.contains_key(&1_794_657_600_000));
        assert!(facts.dates.contains_key(&1_790_000_000_000));
        let date = &facts.dates[&1_790_000_000_000];
        assert_eq!(date.iso, "2026-09-21T14:13:20.000Z");
        assert_eq!(date.clock, "2:13 PM");
        assert_eq!(date.day, "Sep 21");
        // Words: the full string and the part after its first dot.
        assert!(facts.order.contains_key("vm.lead"));
        assert!(facts.order.contains_key("lead"));
        assert!(facts.order.contains_key("a.b"));
        assert!(facts.order.contains_key("b"));
        assert!(!facts.order.contains_key("nested"));
    }

    #[test]
    fn ranks_order_like_locale_compare_and_rank_every_word() {
        let weekly = "Claude and GPT models \u{b7} Weekly \u{b7} In T3";
        let hourly = "Claude and GPT models \u{b7} 5-hour \u{b7} In T3";
        let data = json!(["b", "a-b", "a_1", "A", "é", weekly, hourly]);
        let facts = Facts::of(&[&data], Some(&zone()));
        let order = &facts.order;
        // A < a_1 < a-b < b in the reference's ASCII order (`_` precedes `-`, a prefix first).
        assert!(order["A"] < order["a_1"]);
        assert!(order["a_1"] < order["a-b"]);
        assert!(order["a-b"] < order["b"]);
        // Every word ranks: past printable ASCII the total collation orders by code point,
        // so a composer comparing a middle-dot usage label no longer declines.
        assert!(order["b"] < order["é"], "{order:?}");
        assert!(order[hourly] < order[weekly], "{order:?}");
        // The dates/epochs split the Scoreboard facts use carries the same facts.
        let mut acc = Acc::default();
        acc.read(&data, true);
        let (dates, epochs) = Facts::dates_epochs(&acc, Some(&zone()));
        assert_eq!(dates, facts.dates);
        assert_eq!(epochs, facts.epochs);
    }

    #[test]
    fn text_is_node_order_and_stringify_escaping() {
        let data = json!(["2026-11-14T12:00:00.000Z", "b", "a"]);
        let facts = Facts::of(&[&data], Some(&zone()));
        let text = facts.to_text();
        assert!(text.starts_with("{\"dates\":{\"1794657600000\":{\"iso\":\"2026-11-14T12:00:00.000Z\",\"clock\":\"12:00 PM\",\"day\":\"Nov 14\"}},"));
        // The ISO word's part after its first dot is a word too, and sorts before it.
        assert!(text.ends_with("\"order\":{\"000Z\":0,\"2026-11-14T12:00:00.000Z\":1,\"a\":2,\"b\":3}}"));
        // The parsed value is the same facts.
        let parsed: Facts = Facts {
            dates: facts.dates.clone(),
            epochs: facts.epochs.clone(),
            order: facts.order.clone(),
        };
        assert_eq!(parsed.to_text(), text);
    }

    #[test]
    fn a_missing_zone_keeps_epochs_and_ranks_but_no_dates() {
        let facts = Facts::of(&[&json!([1790000000000_i64, "x"])], None);
        assert!(facts.dates.is_empty());
        assert_eq!(facts.order.get("x"), Some(&0));
    }

    #[test]
    fn a_time_outside_the_zone_window_keeps_no_date() {
        let narrow = ZoneFacts {
            from: 1_700_000_000_000,
            to: 1_700_100_000_000,
            offsets: vec![[1_700_000_000_000, 0]],
        }
        .zone()
        .unwrap();
        let facts = Facts::of(&[&json!([1790000000000_i64])], Some(&narrow));
        assert!(facts.dates.is_empty());
    }
}
