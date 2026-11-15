//! R2: `GET /api/status` from fed generations (DESIGN-M3-COMPLETE 1.2). StatusService's sources are pushed by
//! generation, each a JSON object of some of the composer's top-level input members:
//!
//! | Source | Members |
//! |---|---|
//! | `status-reports` | `reports` |
//! | `status-quota` | `quota`, `spendByStream` |
//! | `status-pinned` | `pinned` |
//! | `status-collected` | `registry`, `machines`, `browsers`, `simlock`, `usage` |
//! | `status-answers` | `answers`, `sends`, `answerHistoryProblem` |
//! | `status-facts` | `timeZone`, `hiddenThreadNames`, `machineIds`, `machineLabels`, and the facts |
//!
//! `status-facts` carries the presentation facts the TS envelope gives (`dates`, `epochs`, `order`: the clock and
//! day words of every time value, the `Date.parse` epochs, the `localeCompare` ranks), computed once by TS over the
//! whole set its id belongs to; the other sources carry `{}` facts. Each generation is parsed once, when it
//! arrives, and its round trip through `JSON.stringify`'s rules is proved then (the composer copies parts of it
//! into the view). The request carries no body: `x-aac-feed` names the six ids, the clock (`now`) and now's clock
//! and day words. The view is the existing composer (`aac_status::read_status`) over the borrowed generations,
//! written by `aac_views::js_json` as Node writes it, with the revalidated-view ETag. Anything unproven declines
//! with a named reason, and TS answers.
use crate::feed::{Feeds, Prepared};
use aac_status::read_context::{Date, Presentation};
use aac_status::read_status::{self, Unproven};
use aac_views::decline::Decline;
use aac_views::js_json::Writer;
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex};

/// The view's name and version (`x-aac-view: status@1`).
pub(crate) const VIEW: &str = "status";
pub(crate) const VERSION: u32 = 1;
const REPORTS: &str = "status-reports";
const QUOTA: &str = "status-quota";
const PINNED: &str = "status-pinned";
const COLLECTED: &str = "status-collected";
const ANSWERS: &str = "status-answers";
const FACTS: &str = "status-facts";
const SOURCES: [&str; 6] = [REPORTS, QUOTA, PINNED, COLLECTED, ANSWERS, FACTS];

/// Each source's members, in the order TS writes them. A generation with any other member set declines (`shape`).
fn members(source: &str) -> &'static [&'static str] {
    match source {
        REPORTS => &["reports"],
        QUOTA => &["quota", "spendByStream"],
        PINNED => &["pinned"],
        COLLECTED => &["registry", "machines", "browsers", "simlock", "usage"],
        ANSWERS => &["answers", "sends", "answerHistoryProblem"],
        FACTS => &[
            "timeZone",
            "hiddenThreadNames",
            "machineIds",
            "machineLabels",
        ],
        _ => &[],
    }
}

/// True for a Status source name.
pub(crate) fn source(name: &str) -> bool {
    SOURCES.contains(&name)
}

/// One prepared generation: its data object, and for `status-facts` the presentation facts.
pub(crate) struct Part {
    data: Value,
    facts: Option<Arc<Presentation>>,
}

impl std::fmt::Debug for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Part")
            .field("members", &self.data.as_object().map(serde_json::Map::len))
            .field("facts", &self.facts.is_some())
            .finish()
    }
}

/// Parse one generation when it arrives. The data is an object of exactly the source's members (absent members
/// are members TS did not write, as `JSON.stringify` drops `undefined`), passes the envelope's prototype-name gate,
/// and re-writes to the very text TS sent (so every part the composer copies is written as Node wrote it).
pub(crate) fn prepare(source: &str, data: &RawValue, facts: &RawValue) -> Result<Part, Decline> {
    let text = data.get();
    let value: Value = serde_json::from_str(text).map_err(|_| Decline::Shape)?;
    let object = value.as_object().ok_or(Decline::Shape)?;
    let allowed = members(source);
    let mut at = 0;
    for key in object.keys() {
        // In TS's order, each at most once (a JSON object may repeat a key; serde keeps the last).
        let found = allowed[at..]
            .iter()
            .position(|m| *m == key.as_str())
            .ok_or(Decline::Shape)?;
        at += found + 1;
    }
    read_status::fed_part(&value).ok_or(Decline::Shape)?;
    if aac_views::js_json::write(&value) != text.as_bytes() {
        return Err(Decline::Shape);
    }
    let facts = if source == FACTS {
        let presentation: Presentation =
            serde_json::from_str(facts.get()).map_err(|_| Decline::Shape)?;
        Some(Arc::new(presentation))
    } else {
        if facts.get().trim() != "{}" {
            return Err(Decline::Shape);
        }
        None
    };
    Ok(Part { data: value, facts })
}

/// The front door's per-request facts: the clock, now's two words (formatted by TS in the facts' time zone) and
/// the generation it read for each source.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Feed {
    now: i64,
    clock: String,
    day: String,
    reports: String,
    quota: String,
    pinned: String,
    collected: String,
    answers: String,
    facts: String,
}

/// The last ETag, keyed by the generations and the second of `now`; served again only for the same identity bytes.
type Tagged = (String, Vec<u8>, String);
static ETAG: Mutex<Option<Tagged>> = Mutex::new(None);

fn etag(key: String, identity: &[u8]) -> String {
    if let Ok(held) = ETAG.lock()
        && let Some((k, i, tag)) = held.as_ref()
        && *k == key
        && i.as_slice() == identity
    {
        return tag.clone();
    }
    let digest = Sha256::digest(identity);
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    let tag = format!("W/\"{hex}\"");
    if let Ok(mut held) = ETAG.lock() {
        *held = Some((key, identity.to_vec(), tag.clone()));
    }
    tag
}

/// The answer of one fed Status request: the view's bytes and its ETag. Runs on the blocking pool.
pub(crate) fn answer(feeds: &Feeds, feed: &str) -> Result<(Vec<u8>, String), Decline> {
    let feed: Feed = crate::json_object::from_slice(
        feed.as_bytes(),
        &[
            "now",
            "clock",
            "day",
            "reports",
            "quota",
            "pinned",
            "collected",
            "answers",
            "facts",
        ],
    )
    .map_err(|_| Decline::Shape)?;
    let ids = [
        (REPORTS, &feed.reports),
        (QUOTA, &feed.quota),
        (PINNED, &feed.pinned),
        (COLLECTED, &feed.collected),
        (ANSWERS, &feed.answers),
        (FACTS, &feed.facts),
    ];
    let mut held = Vec::with_capacity(ids.len());
    for (source, id) in ids {
        held.push(feeds.current(source, id)?);
    }
    let mut parts: Vec<(&str, &Value)> = Vec::with_capacity(20);
    let now = Value::from(feed.now);
    parts.push(("now", &now));
    let mut presentation = None;
    for generation in &held {
        let Prepared::Status(part) = &generation.prepared else {
            return Err(Decline::Internal);
        };
        let part = part.as_ref().map_err(|d| *d)?;
        if let Some(facts) = &part.facts {
            presentation = Some(facts.clone());
        }
        let Some(object) = part.data.as_object() else {
            return Err(Decline::Internal);
        };
        for (key, value) in object {
            parts.push((key.as_str(), value));
        }
    }
    let presentation = presentation.ok_or(Decline::Internal)?;
    let date = Date::of_now(feed.now, feed.clock, feed.day).ok_or(Decline::Date)?;
    let view =
        read_status::compose_fed(parts, presentation, (feed.now, date)).map_err(|reason| {
            match reason {
                Unproven::Facts => Decline::Date,
                Unproven::Input => Decline::Unsupported,
            }
        })?;
    let mut writer = Writer::with_capacity(64 * 1024);
    writer.value(&view);
    let (bytes, identity) = writer.finish();
    let key = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        feed.reports,
        feed.quota,
        feed.pinned,
        feed.collected,
        feed.answers,
        feed.facts,
        feed.now.div_euclid(1000)
    );
    Ok((bytes, etag(key, &identity)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_owned()).expect("raw")
    }

    #[test]
    fn a_generation_is_exactly_its_members_written_as_node_writes_them() {
        assert!(prepare(REPORTS, &raw(r#"{"reports":{"a":[1,2.5]}}"#), &raw("{}")).is_ok());
        assert!(
            prepare(
                COLLECTED,
                &raw(r#"{"registry":null,"usage":null}"#),
                &raw("{}")
            )
            .is_ok()
        );
        for (source, data, facts, reason) in [
            // Another member, a member out of order, a repeated member, not an object.
            (REPORTS, r#"{"reports":{},"x":1}"#, "{}", Decline::Shape),
            (
                QUOTA,
                r#"{"spendByStream":{},"quota":{}}"#,
                "{}",
                Decline::Shape,
            ),
            (
                REPORTS,
                r#"{"reports":{},"reports":{}}"#,
                "{}",
                Decline::Shape,
            ),
            (REPORTS, "[1]", "{}", Decline::Shape),
            // An Object.prototype name anywhere declines, as the envelope does.
            (
                REPORTS,
                r#"{"reports":{"constructor":1}}"#,
                "{}",
                Decline::Shape,
            ),
            // Text Node would not write (an escape it does not use, a number it spells otherwise).
            (REPORTS, r#"{"reports":"a\/b"}"#, "{}", Decline::Shape),
            (REPORTS, r#"{"reports":1.0}"#, "{}", Decline::Shape),
            (REPORTS, r#"{"reports":1E21}"#, "{}", Decline::Shape),
            (REPORTS, r#"{ "reports":1}"#, "{}", Decline::Shape),
            // Data sources carry no facts; the facts source carries exactly the envelope's three maps.
            (REPORTS, r#"{"reports":{}}"#, "null", Decline::Shape),
            (FACTS, r#"{"timeZone":"UTC"}"#, "{}", Decline::Shape),
            (
                FACTS,
                r#"{"timeZone":"UTC"}"#,
                r#"{"dates":{},"epochs":{},"order":{},"x":1}"#,
                Decline::Shape,
            ),
        ] {
            assert_eq!(
                prepare(source, &raw(data), &raw(facts)).err(),
                Some(reason),
                "{source} {data} {facts}"
            );
        }
        let facts = prepare(
            FACTS,
            &raw(r#"{"timeZone":"UTC","machineIds":["vm"]}"#),
            &raw(
                r#"{"dates":{"1":{"iso":"x","clock":"c","day":"d"}},"epochs":{},"order":{"a":0}}"#,
            ),
        )
        .expect("facts");
        assert!(facts.facts.is_some());
    }

    #[test]
    fn the_etag_is_reused_only_for_the_same_identity() {
        let a = etag("k".into(), b"{\"a\":1}");
        // Node: createHash('sha256').update('{"a":1}').digest('hex')
        assert_eq!(
            a,
            "W/\"015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862\""
        );
        assert_eq!(etag("k".into(), b"{\"a\":1}"), a);
        assert_ne!(etag("k".into(), b"{\"a\":2}"), a);
        assert_eq!(etag("other".into(), b"{\"a\":1}"), a);
    }
}
