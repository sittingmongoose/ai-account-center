//! G17: the Home feed producer, in Rust. The production work `views/home-feed.ts` (`answerHomeFed`) does today
//! — the dashboard's change-key text, its facts (the `localeCompare` ranks of `homeSortWords` and the
//! `Date.parse` epochs of `resetEpochs`), the Antigravity ISO-only envelope, the Opens text, and the per-source
//! change keys and generations — computed here instead of in TypeScript. Lane I's collection feeds it in
//! process once it owns the dashboard read (G2); `PUT /internal/home-inputs` is the transitional socket form,
//! and the TS push path stays available until the cut.
//!
//! Every output is the very text the TS producer writes (`JSON.stringify`), or a named decline:
//! - sort words: printable ASCII only (`aac_num::intl::ascii_compare`, proven against Node's `localeCompare`);
//!   another word declines `collation` (TS's `localeRanks` answers there until the cut);
//! - instants: the ES Date Time String Format with an explicit zone (`aac_num::ecma::parse_instant`); a string
//!   `Date.parse` reads outside that domain declines `date` (the empty string is its proven NaN);
//! - `toISOString`: years 1970-9999 (`aac_num::ecma::to_iso_string`);
//! - the envelope's times: safe-integer milliseconds in `readFacts`'s window;
//! - `String(profile)` of the sign-in rows: absent/null/boolean/string/number only; an object or array declines
//!   `unsupported` (its coercion is not a proved domain);
//! - `jsonExact`'s gate: depth at most 64 and no `__proto__` key (a parsed JSON value is otherwise exact).
//!
//! The generation rules are `ensure`'s: a source keeps its latest key; a changed key builds once, takes the
//! next generation (the clock in ms x 1000, only forward, moving past a newer held one), and a push that finds
//! a newer generation held declines `stale` for this request. A body over the transport's 4 MiB cap is refused
//! once per key (`unsupported` here; the bridge's own word for the same refusal is `cap`).

use crate::feed::{Feeds, HOME_ANTIGRAVITY, HOME_DASHBOARD, HOME_OPENS};
use aac_num::ecma::{iso_prefixed, parse_instant, to_iso_string};
use aac_num::intl::ascii_compare;
use aac_views::decline::Decline;
use aac_views::js_json;
use serde::Deserialize;
use serde_json::{Map, Value};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

/// `requestRust`'s request cap (`home-feed.ts` `RUST_FEED_CAP`): a generation above it is never sent.
const FEED_CAP: usize = 4 * 1024 * 1024;
/// `jsonExact`'s depth cap (`rust-profile-input.ts`).
const EXACT_DEPTH: usize = 64;
/// `readFacts`'s safe-integer millisecond window (`rust-read-input.ts`).
const TIME_MIN: f64 = 100_000_000_000.0;
/// `Number.isSafeInteger`'s bound.
const SAFE_INTEGER: f64 = 9_007_199_254_740_991.0;

/// One producer request: the raw Home inputs, as the TS front door reads them (the dashboard with its
/// `updatedAt` and `server.views`, this process's Open progress, the runtime's inventory).
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Inputs {
    pub(crate) platform: String,
    pub(crate) secure: bool,
    pub(crate) dashboard: Value,
    pub(crate) opens: Value,
    pub(crate) antigravity: Value,
}

/// `jsonExact` for a parsed JSON value: the depth cap and the `__proto__` refusal. Numbers, strings, booleans,
/// null, arrays and plain objects are exact by construction (a non-finite number never parses).
fn json_exact(value: &Value, depth: usize) -> bool {
    if depth > EXACT_DEPTH {
        return false;
    }
    match value {
        Value::Object(map) => map
            .iter()
            .all(|(key, member)| key != "__proto__" && json_exact(member, depth + 1)),
        Value::Array(items) => items.iter().all(|member| json_exact(member, depth + 1)),
        _ => true,
    }
}

/// `homeSortWords`: the account ids (a non-string id is `''`) and `String(profile)` of the Claude sign-in rows.
fn sort_words(dashboard: &Value) -> Result<Vec<String>, Decline> {
    let mut words = Vec::new();
    if let Some(accounts) = dashboard.get("accounts").and_then(Value::as_array) {
        for account in accounts {
            words.push(match account.get("id") {
                Some(Value::String(id)) => id.clone(),
                _ => String::new(),
            });
        }
    }
    if let Some(rows) = dashboard
        .get("claudeSignIns")
        .and_then(|renewal| renewal.get("signIns"))
        .and_then(Value::as_array)
    {
        for row in rows {
            words.push(string_coercion(row.get("profile"))?);
        }
    }
    Ok(words)
}

/// `String(value)` for the sign-in profile: the proved coercions only.
fn string_coercion(value: Option<&Value>) -> Result<String, Decline> {
    match value {
        None => Ok("undefined".into()),
        Some(Value::Null) => Ok("null".into()),
        Some(Value::Bool(flag)) => Ok(flag.to_string()),
        Some(Value::String(text)) => Ok(text.clone()),
        Some(Value::Number(number)) => Ok(aac_num::ecma::to_string(
            number.as_f64().ok_or(Decline::Number)?,
        )),
        Some(Value::Object(_) | Value::Array(_)) => Err(Decline::Unsupported),
    }
}

/// `localeRanks`: the words deduplicated in first-seen order, sorted by `localeCompare`, equal words sharing a
/// rank. Outside the printable-ASCII domain the engine's order is not proved, so the producer declines.
fn ranks(words: &[String]) -> Result<Map<String, Value>, Decline> {
    let mut seen = HashSet::new();
    let mut unique: Vec<&String> = words.iter().filter(|w| seen.insert(w.as_str())).collect();
    for word in &unique {
        // A word compares with itself exactly when every byte is in the proved domain.
        if ascii_compare(word, word).is_none() {
            return Err(Decline::Collation);
        }
    }
    unique.sort_by(|a, b| ascii_compare(a, b).unwrap_or(Ordering::Equal));
    let mut order = Map::new();
    let mut rank = 0usize;
    for (index, word) in unique.iter().enumerate() {
        if index > 0 && ascii_compare(unique[index - 1], word) != Some(Ordering::Equal) {
            rank += 1;
        }
        order.insert((*word).clone(), Value::from(rank));
    }
    Ok(order)
}

/// `resetEpochs`: every string `markPassedResets` hands to `Date.parse` (the account's `sampledAt`, each
/// window's `resetAt` and `sampledAt`), first-seen order, the finite epoch or null. A string the engine reads
/// outside the proved instant domain declines the generation (TS writes its `Date.parse` answer there).
fn reset_epochs(dashboard: &Value) -> Result<Map<String, Value>, Decline> {
    let accounts = dashboard
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or(Decline::Shape)?;
    let mut seen = HashSet::new();
    let mut epochs = Map::new();
    let mut add = |text: &str, epochs: &mut Map<String, Value>| -> Result<(), Decline> {
        if !seen.insert(text.to_owned()) {
            return Ok(());
        }
        let ms = if text.is_empty() {
            // `Date.parse('')` is NaN, proven.
            None
        } else if iso_prefixed(text) {
            Some(parse_instant(text).ok_or(Decline::Date)?)
        } else {
            return Err(Decline::Date);
        };
        epochs.insert(
            text.to_owned(),
            ms.map(Value::from).unwrap_or(Value::Null),
        );
        Ok(())
    };
    for account in accounts {
        // A non-object account contributes nothing (its `sampledAt`/`windows` read as absent).
        let Some(row) = account.as_object() else {
            continue;
        };
        if let Some(Value::String(text)) = row.get("sampledAt") {
            add(text, &mut epochs)?;
        }
        if let Some(windows) = row.get("windows").and_then(Value::as_array) {
            for window in windows {
                let Some(window) = window.as_object() else {
                    continue;
                };
                for key in ["resetAt", "sampledAt"] {
                    if let Some(Value::String(text)) = window.get(key) {
                        add(text, &mut epochs)?;
                    }
                }
            }
        }
    }
    Ok(epochs)
}

/// The dashboard generation's facts: `{"order": localeRanks(homeSortWords(dashboard)), "epochs":
/// resetEpochs(dashboard)}`, the text `home-feed.ts` writes.
pub(crate) fn dashboard_facts(dashboard: &Value) -> Result<Vec<u8>, Decline> {
    let mut facts = Map::new();
    facts.insert("order".into(), Value::Object(ranks(&sort_words(dashboard)?)?));
    facts.insert("epochs".into(), Value::Object(reset_epochs(dashboard)?));
    Ok(js_json::write(&Value::Object(facts)))
}

/// The dashboard's change key: `JSON.stringify({...dashboard, updatedAt: ''})` — `updatedAt` blanked in its
/// own position (appended when absent, as the spread does).
pub(crate) fn dashboard_text(dashboard: &Value) -> Result<Vec<u8>, Decline> {
    let mut top = dashboard.as_object().ok_or(Decline::Shape)?.clone();
    top.insert("updatedAt".into(), Value::String(String::new()));
    Ok(js_json::write(&Value::Object(top)))
}

/// `readFacts` over the inventory (ISO epochs and safe-integer times, first-seen order), then the ISO-only
/// presentation envelope `rustAntigravityInventoryInput` writes: `{data, presentation: {dates, epochs, order}}`
/// with `order` empty (`rankAll: false`, no extras) and ISO words only (`clockAndDay: false`, UTC).
pub(crate) fn antigravity_envelope(inventory: &Value) -> Result<Vec<u8>, Decline> {
    if !json_exact(inventory, 0) {
        return Err(Decline::Shape);
    }
    let mut epochs: Map<String, Value> = Map::new();
    let mut times: Vec<i64> = Vec::new();
    let mut times_seen: HashSet<i64> = HashSet::new();
    fn visit(
        value: &Value,
        epochs: &mut Map<String, Value>,
        times: &mut Vec<i64>,
        times_seen: &mut HashSet<i64>,
    ) -> Result<(), Decline> {
        match value {
            Value::String(text) if iso_prefixed(text) => {
                let ms = parse_instant(text).ok_or(Decline::Date)?;
                epochs.entry(text.clone()).or_insert(Value::from(ms));
                if times_seen.insert(ms) {
                    times.push(ms);
                }
            }
            Value::Number(number) => {
                let ms = number.as_f64().ok_or(Decline::Number)?;
                if ms.is_finite()
                    && ms.fract() == 0.0
                    && ms.abs() <= SAFE_INTEGER
                    && (TIME_MIN..=TIME_MAX).contains(&ms)
                {
                    let ms = ms as i64;
                    if times_seen.insert(ms) {
                        times.push(ms);
                    }
                }
            }
            Value::Array(items) => {
                for member in items {
                    visit(member, epochs, times, times_seen)?;
                }
            }
            Value::Object(map) => {
                for member in map.values() {
                    visit(member, epochs, times, times_seen)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    visit(inventory, &mut epochs, &mut times, &mut times_seen)?;
    let mut dates = Map::new();
    for ms in &times {
        let mut words = Map::new();
        words.insert(
            "iso".into(),
            Value::String(to_iso_string(*ms).ok_or(Decline::Date)?),
        );
        dates.insert(ms.to_string(), Value::Object(words));
    }
    let mut presentation = Map::new();
    presentation.insert("dates".into(), Value::Object(dates));
    presentation.insert("epochs".into(), Value::Object(epochs));
    presentation.insert("order".into(), Value::Object(Map::new()));
    let mut envelope = Map::new();
    envelope.insert("data".into(), inventory.clone());
    envelope.insert("presentation".into(), Value::Object(presentation));
    Ok(js_json::write(&Value::Object(envelope)))
}

/// What one source holds: its latest change key and the generation the service holds for it (or the refusal).
struct Held {
    key: String,
    generation: Option<String>,
    refused: Option<Decline>,
}

#[derive(Default)]
struct State {
    /// The last generation number this producer issued (the clock in ms x 1000, only forward).
    last: u64,
    sources: HashMap<String, Held>,
}

/// The Home feed producer of one boot: the per-source change keys and generations, in process.
#[derive(Default)]
pub(crate) struct HomeProducer {
    state: Mutex<State>,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(0))
}

/// `seen`: a newer held generation moves the counter past it.
fn seen(last: &mut u64, held: &str) {
    if let Ok(number) = held.parse::<u64>()
        && number > *last
    {
        *last = number;
    }
}

impl HomeProducer {
    /// Produce the three Home generations for one request's raw inputs, pushing only what changed, and name
    /// the generations the service holds. A decline leaves the sources it did not record as they were; the
    /// next request produces again (as `ensure` does).
    pub(crate) fn produce(&self, feeds: &Feeds, inputs: &Inputs) -> Result<Value, Decline> {
        if inputs.platform != "mac" && inputs.platform != "windows" {
            return Err(Decline::Shape);
        }
        if !json_exact(&inputs.dashboard, 0) {
            return Err(Decline::Shape);
        }
        let dashboard_data = dashboard_text(&inputs.dashboard)?;
        let dashboard_key =
            String::from_utf8(dashboard_data.clone()).map_err(|_| Decline::Internal)?;
        let opens_key =
            String::from_utf8(js_json::write(&inputs.opens)).map_err(|_| Decline::Internal)?;
        let inventory_key = format!(
            "inventory:{}",
            String::from_utf8(js_json::write(&inputs.antigravity))
                .map_err(|_| Decline::Internal)?
        );
        let source = format!(
            "{HOME_DASHBOARD}{}-{}",
            inputs.platform,
            u8::from(inputs.secure)
        );
        let mut state = self.state.lock().map_err(|_| Decline::Internal)?;
        let dashboard = ensure_source(
            &mut state,
            feeds,
            &source,
            dashboard_key,
            || {
                let facts = dashboard_facts(&inputs.dashboard)?;
                Ok((dashboard_data.clone(), facts))
            },
        )?;
        let opens = ensure_source(&mut state, feeds, HOME_OPENS, opens_key, || {
            Ok((js_json::write(&inputs.opens), b"null".to_vec()))
        })?;
        let antigravity = ensure_source(
            &mut state,
            feeds,
            HOME_ANTIGRAVITY,
            inventory_key,
            || Ok((antigravity_envelope(&inputs.antigravity)?, b"null".to_vec())),
        )?;
        Ok(serde_json::json!({
            "dashboard": dashboard,
            "opens": opens,
            "antigravity": antigravity,
        }))
    }
}

/// `ensure` for one source, in process: the held key answers at once; a changed key builds, takes the next
/// generation and puts it; a newer held generation declines `stale`; an over-cap body is refused once per key.
fn ensure_source(
    state: &mut State,
    feeds: &Feeds,
    source: &str,
    key: String,
    build: impl FnOnce() -> Result<(Vec<u8>, Vec<u8>), Decline>,
) -> Result<String, Decline> {
    if let Some(held) = state.sources.get(source)
        && held.key == key
    {
        return match (&held.generation, held.refused) {
            (Some(generation), _) => Ok(generation.clone()),
            (None, Some(refused)) => Err(refused),
            (None, None) => Err(Decline::Shape),
        };
    }
    let (data, facts) = build()?;
    let generation = next_generation(&mut state.last);
    if data.len() + facts.len() + generation.len() + 64 > FEED_CAP {
        state.sources.insert(
            source.to_owned(),
            Held {
                key,
                generation: None,
                refused: Some(Decline::Unsupported),
            },
        );
        return Err(Decline::Unsupported);
    }
    let mut body = Vec::with_capacity(data.len() + facts.len() + generation.len() + 48);
    body.extend_from_slice(br#"{"generation":""#);
    body.extend_from_slice(generation.as_bytes());
    body.extend_from_slice(br#"","data":"#);
    body.extend_from_slice(&data);
    body.extend_from_slice(br#","facts":"#);
    body.extend_from_slice(&facts);
    body.push(b'}');
    let reply = feeds.put(source, &body)?;
    let held = reply
        .get("generation")
        .and_then(Value::as_str)
        .ok_or(Decline::Internal)?;
    if held != generation {
        // Another push holds a newer generation: this request declines; the next one produces past it.
        seen(&mut state.last, held);
        return Err(Decline::Stale);
    }
    state.sources.insert(
        source.to_owned(),
        Held {
            key,
            generation: Some(held.to_owned()),
            refused: None,
        },
    );
    Ok(held.to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_change_key_blanks_updated_at_in_its_own_position() {
        let dashboard = json!({"schemaVersion": 1, "updatedAt": "2026-11-19T00:00:00.000Z", "accounts": []});
        assert_eq!(
            dashboard_text(&dashboard).unwrap(),
            br#"{"schemaVersion":1,"updatedAt":"","accounts":[]}"#
        );
        // Absent updatedAt appends, as the spread does.
        let dashboard = json!({"accounts": []});
        assert_eq!(
            dashboard_text(&dashboard).unwrap(),
            br#"{"accounts":[],"updatedAt":""}"#
        );
    }

    #[test]
    fn ranks_and_epochs_are_the_ts_facts() {
        // Node: localeRanks(['b','A','a','B']) and resetEpochs of the dashboard below.
        let dashboard = json!({
            "accounts": [
                {"id": "b", "sampledAt": "2026-11-14T10:00:00.000Z",
                 "windows": [{"resetAt": "2026-11-14T12:00:00.000Z", "sampledAt": ""}]},
                {"id": "A", "windows": [{"resetAt": "2026-11-14T10:00:00.000Z"}]},
                {"id": "a"}, {"id": "B"}
            ],
            "claudeSignIns": {"signIns": [{"profile": "z"}, {"profile": null}, {}]}
        });
        let facts: Value = serde_json::from_slice(&dashboard_facts(&dashboard).unwrap()).unwrap();
        // localeCompare order: A B a b z (primary letter order, lowercase first per ICU root: a A b B ...
        // actually the proved ASCII order ranks 'A' before 'a'? ascii_compare: lowercase precedes uppercase).
        let order = facts["order"].as_object().unwrap();
        let words: Vec<&str> = order.keys().map(String::as_str).collect();
        assert_eq!(words, vec!["a", "A", "b", "B", "null", "undefined", "z"]);
        assert_eq!(order["a"], json!(0));
        assert_eq!(order["A"], json!(1));
        assert_eq!(order["null"], json!(4));
        let epochs = facts["epochs"].as_object().unwrap();
        assert_eq!(
            epochs.keys().map(String::as_str).collect::<Vec<_>>(),
            vec![
                "2026-11-14T10:00:00.000Z",
                "2026-11-14T12:00:00.000Z",
                ""
            ]
        );
        assert_eq!(epochs["2026-11-14T10:00:00.000Z"], json!(1_794_650_400_000i64));
        assert_eq!(epochs[""], Value::Null);
    }

    #[test]
    fn a_non_ascii_sort_word_declines_collation() {
        let dashboard = json!({"accounts": [{"id": "é"}]});
        assert_eq!(dashboard_facts(&dashboard), Err(Decline::Collation));
    }

    #[test]
    fn a_legacy_date_string_declines_date() {
        let dashboard = json!({"accounts": [{"sampledAt": "Nov 14, 2026"}]});
        assert_eq!(dashboard_facts(&dashboard), Err(Decline::Date));
    }

    #[test]
    fn the_envelope_is_the_iso_only_presentation() {
        // Node: rustAntigravityInventoryInput({schemaVersion:1,hostId:'ubuntu',profiles:[{at:'2026-11-14T10:00:00.000Z',ms:1794650400000}]})
        let inventory = json!({
            "schemaVersion": 1,
            "hostId": "ubuntu",
            "profiles": [{"at": "2026-11-14T10:00:00.000Z", "ms": 1_794_650_400_000i64}]
        });
        let text = antigravity_envelope(&inventory).unwrap();
        let value: Value = serde_json::from_slice(&text).unwrap();
        assert_eq!(value["data"], inventory);
        let presentation = value["presentation"].as_object().unwrap();
        assert_eq!(
            presentation.keys().map(String::as_str).collect::<Vec<_>>(),
            vec!["dates", "epochs", "order"]
        );
        assert_eq!(
            presentation["dates"]["1794650400000"]["iso"],
            json!("2026-11-14T10:00:00.000Z")
        );
        assert_eq!(
            presentation["epochs"]["2026-11-14T10:00:00.000Z"],
            json!(1_794_650_400_000i64)
        );
        assert_eq!(presentation["order"], json!({}));
        // The text is the very JSON.stringify output (key order included).
        assert_eq!(
            text,
            br#"{"data":{"schemaVersion":1,"hostId":"ubuntu","profiles":[{"at":"2026-11-14T10:00:00.000Z","ms":1794650400000}]},"presentation":{"dates":{"1794650400000":{"iso":"2026-11-14T10:00:00.000Z"}},"epochs":{"2026-11-14T10:00:00.000Z":1794650400000},"order":{}}}"#
        );
    }

    #[test]
    fn a_proto_key_or_excess_depth_declines_shape() {
        let inventory = json!({"__proto__": {}});
        assert_eq!(antigravity_envelope(&inventory), Err(Decline::Shape));
        let mut deep = json!({});
        for _ in 0..70 {
            deep = json!({"nested": deep});
        }
        assert_eq!(antigravity_envelope(&deep), Err(Decline::Shape));
    }

    #[test]
    fn the_producer_pushes_once_per_change_and_names_the_held_generations() {
        let feeds = Feeds::new();
        let producer = HomeProducer::default();
        let inputs = Inputs {
            platform: "mac".into(),
            secure: false,
            dashboard: json!({"schemaVersion": 1, "updatedAt": "x", "accounts": []}),
            opens: json!([]),
            antigravity: json!({"schemaVersion": 1, "hostId": "ubuntu", "profiles": []}),
        };
        let first = producer.produce(&feeds, &inputs).unwrap();
        let second = producer.produce(&feeds, &inputs).unwrap();
        assert_eq!(first, second, "an unchanged input names the same generations");
        let mut changed = inputs.dashboard.clone();
        changed["updatedAt"] = json!("y");
        // updatedAt is blanked in the change key: the same key, the same generations.
        let mut same_key = inputs.clone();
        same_key.dashboard = changed;
        assert_eq!(producer.produce(&feeds, &same_key).unwrap(), first);
        let mut new_content = inputs.clone();
        new_content.dashboard["accounts"] = json!([{"id": "a", "windows": []}]);
        let third = producer.produce(&feeds, &new_content).unwrap();
        assert_ne!(third["dashboard"], first["dashboard"]);
        assert_eq!(third["opens"], first["opens"]);
        // The held dashboard generation parses and round-trips (the fed path's own gate).
        let held = feeds
            .current("home-dashboard-mac-0", third["dashboard"].as_str().unwrap())
            .unwrap();
        assert!(matches!(held.prepared, crate::feed::Prepared::Dashboard(Ok(_))));
    }

    /// The TS producer's own outputs (the Home corpus and the large fixture, generated by
    /// `tests/spec` from the dist modules): every proved input's texts are byte-equal; an input outside the
    /// proved domains declines. The push gates (`prepare_*`) are the fed path's own, shared with the TS push.
    #[test]
    fn the_producer_texts_are_the_ts_producer_texts() {
        let goldens: Value = serde_json::from_str(include_str!(
            "../../../tests/spec/home-producer-goldens.json"
        ))
        .unwrap();
        let entries = goldens["entries"].as_array().unwrap();
        let (mut equal, mut declined) = (0, 0);
        for entry in entries {
            let name = entry["name"].as_str().unwrap();
            let dashboard = &entry["dashboard"];
            let antigravity = &entry["antigravity"];
            if entry.get("expectDecline") == Some(&Value::Bool(true)) {
                let refused = !json_exact(dashboard, 0)
                    || dashboard_text(dashboard).is_err()
                    || dashboard_facts(dashboard).is_err()
                    || !json_exact(antigravity, 0)
                    || antigravity_envelope(antigravity).is_err();
                assert!(refused, "{name}: the producer must decline this input");
                declined += 1;
                continue;
            }
            let expected = &entry["expected"];
            assert_eq!(
                dashboard_text(dashboard).unwrap(),
                expected["dashboardText"].as_str().unwrap().as_bytes(),
                "{name}: change-key text"
            );
            assert_eq!(
                dashboard_facts(dashboard).unwrap(),
                expected["facts"].as_str().unwrap().as_bytes(),
                "{name}: facts"
            );
            assert_eq!(
                js_json::write(&entry["opens"]),
                expected["opensText"].as_str().unwrap().as_bytes(),
                "{name}: opens text"
            );
            assert_eq!(
                antigravity_envelope(antigravity).unwrap(),
                expected["antigravityEnvelope"].as_str().unwrap().as_bytes(),
                "{name}: ISO envelope"
            );
            equal += 1;
        }
        assert!(equal > 0 && declined > 0);
        eprintln!("home producer goldens: {equal} byte-equal, {declined} declined");
    }
}