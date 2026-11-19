//! The Status tab's answer routes at the service: `GET /api/status/answers` (the owner's
//! history as the page reads it) and `POST /api/status/answers` (one answer set). The public
//! front door keeps the HTTP admission (session, origin, JSON, body limit); this module owns
//! the entry contract and the exact JSON the routes write.
//!
//! The read is the owned feed's own answer history (the journal plus the verified share
//! copies, merged by [`crate::status_produce`]), so the page sees the writer's records. The
//! write is [`crate::answers_owned::AnswersOwned::submit`], the same record path the
//! front door's forwarded sends use.

use aac_status::answers_engine::{HistoryEntry, SendRecord, delivery_view};
use aac_views::decline::Decline;
use indexmap::IndexMap;
use serde_json::{Map, Value as Json, json};

/// The route's limit default and bounds (`limit must be a whole number from 1 to 500`).
pub(crate) const DEFAULT_LIMIT: usize = 50;
pub(crate) const MAX_LIMIT: usize = 500;
/// `MAX_ANSWER_ENTRIES`.
pub(crate) const MAX_ENTRIES: usize = 50;

/// `limit` for the read route: absent is the default, anything else is a four-digit number in
/// 1..=500. The front door answers `invalid_limit` before this sees a bad one.
pub(crate) fn limit(raw: Option<&str>) -> Option<usize> {
    match raw {
        None => Some(DEFAULT_LIMIT),
        Some(text) => {
            let ok = (1..=4).contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit());
            let value = text.parse::<usize>().ok()?;
            (ok && (1..=MAX_LIMIT).contains(&value)).then_some(value)
        }
    }
}

/// The read route's query: at most one `limit` key, no other key. The front door answers
/// `unexpected_query` and `invalid_limit` itself; a service that sees anything else declines.
pub(crate) fn read_limit(raw: Option<&str>) -> Option<usize> {
    let Some(raw) = raw.filter(|raw| !raw.is_empty()) else {
        return Some(DEFAULT_LIMIT);
    };
    let mut raw_limit = None;
    for part in raw.split('&') {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        if key != "limit" || raw_limit.is_some() {
            return None;
        }
        raw_limit = Some(value);
    }
    raw_limit.and_then(|value| limit(Some(value)))
}

/// `^[0-9a-f]{32}$`
fn entry_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
}

/// The route's `validEntryIds`: every present `entryId` is the page's id and none repeats.
fn valid_entry_ids(entries: &[Json]) -> bool {
    let mut seen: Vec<&str> = Vec::new();
    for entry in entries {
        let Some(id) = entry.get("entryId") else {
            continue;
        };
        let Some(id) = id.as_str() else {
            return false;
        };
        if !entry_id(id) || seen.contains(&id) {
            return false;
        }
        seen.push(id);
    }
    true
}

/// The body's `entryId` check; the route answers `invalid_body` for anything else.
pub(crate) fn valid_entries(entries: &[Json]) -> bool {
    !entries.is_empty() && entries.len() <= MAX_ENTRIES && valid_entry_ids(entries)
}

/// `answerDevice`: the device word from the browser's User-Agent.
pub(crate) fn device_of(user_agent: Option<&str>) -> &'static str {
    let ua = user_agent.unwrap_or("");
    if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") {
        "iPad"
    } else if ua.contains("Android") {
        "Android"
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        "Mac"
    } else if ua.contains("Windows") {
        "Windows"
    } else if ua.contains("Linux") || ua.contains("X11") {
        "Linux"
    } else {
        "browser"
    }
}

/// One history entry as the route's `entryView` writes it.
pub(crate) fn entry_view(entry: &HistoryEntry, sends: &IndexMap<String, SendRecord>) -> Json {
    let card = &entry.card;
    let answer = &entry.answer;
    let mut view = Map::new();
    view.insert("id".into(), Json::from(entry.id.clone()));
    view.insert(
        "card".into(),
        json!({
            "id": card.get("id").cloned().unwrap_or(Json::Null),
            "label": card.get("label").cloned().unwrap_or(Json::Null),
            "seq": card.get("seq").cloned().unwrap_or(Json::Null),
            "title": card.get("title").cloned().unwrap_or(Json::Null),
            "project": card.get("project").cloned().unwrap_or(Json::Null),
            "stream": card.get("stream").cloned().unwrap_or(Json::Null),
            "askedBy": card.get("askedBy").cloned().unwrap_or(Json::Null),
            "options": card.get("options").and_then(Json::as_array).map(|options| {
                options
                    .iter()
                    .map(|option| json!({
                        "key": option.get("key").cloned().unwrap_or(Json::Null),
                        "label": option.get("label").cloned().unwrap_or(Json::Null),
                    }))
                    .collect::<Vec<_>>()
            }).unwrap_or_default(),
            "recommendation": card.get("recommendation").cloned().unwrap_or(Json::Null),
            "cardSha256": answer.get("cardSha256").cloned().unwrap_or(Json::Null),
        }),
    );
    view.insert(
        "answer".into(),
        json!({
            "kind": answer.get("kind").cloned().unwrap_or(Json::Null),
            "choice": answer.get("choice").cloned().unwrap_or(Json::Null),
            "note": answer.get("note").cloned().unwrap_or(Json::Null),
            "via": answer.get("via").cloned().unwrap_or(Json::Null),
            "answeredAt": answer.get("answeredAt").cloned().unwrap_or(Json::Null),
            "device": answer.get("device").cloned().unwrap_or(Json::Null),
            "delivery": delivery_view(entry, sends),
        }),
    );
    Json::Object(view)
}

/// The route's answer list: newest first, at most `limit`, in the reference's shape.
pub(crate) fn history_json(
    answers: &[HistoryEntry],
    sends: &[SendRecord],
    limit: usize,
) -> Json {
    let index: IndexMap<String, SendRecord> = sends
        .iter()
        .map(|send| (send.send_id.clone(), send.clone()))
        .collect();
    let rows: Vec<Json> = answers
        .iter()
        .rev()
        .take(limit)
        .map(|entry| entry_view(entry, &index))
        .collect();
    json!({ "answers": rows })
}

/// The route's own body guard (`hasOnlyKeys`, the entry count, the entry ids, `sendId`): what
/// the front door answers `invalid_body` for before any owner sees it.
pub(crate) fn valid_send_body(body: &Json) -> bool {
    let Some(map) = body.as_object() else {
        return false;
    };
    if map.keys().any(|key| key != "answers" && key != "sendId") {
        return false;
    }
    let Some(answers) = map.get("answers").and_then(Json::as_array) else {
        return false;
    };
    if !valid_entries(answers) {
        return false;
    }
    match map.get("sendId") {
        None | Some(Json::Null) => true,
        Some(Json::String(id)) => {
            (16..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        }
        Some(_) => false,
    }
}

/// The route's body: `{answers, sendId?}` in the shape `AnswersOwned::submit` takes, or the
/// route's `invalid_body` when the set itself is unusable.
pub(crate) fn submit_body(body: &Json, device: &str) -> Result<Json, Decline> {
    let map = body.as_object().ok_or(Decline::Shape)?;
    let answers = map
        .get("answers")
        .and_then(Json::as_array)
        .cloned()
        .ok_or(Decline::Shape)?;
    if !valid_entries(&answers) {
        return Err(Decline::Shape);
    }
    if map
        .keys()
        .any(|key| key != "answers" && key != "sendId")
    {
        return Err(Decline::Shape);
    }
    let send_id = match map.get("sendId") {
        None | Some(Json::Null) => Json::Null,
        Some(Json::String(id)) => {
            let ok = (16..=64).contains(&id.len())
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
            if !ok {
                return Err(Decline::Shape);
            }
            Json::String(id.clone())
        }
        Some(_) => return Err(Decline::Shape),
    };
    Ok(json!({
        "entries": answers,
        "device": device,
        "clientSendId": send_id,
    }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    fn entry(card: Json, answer: Json) -> HistoryEntry {
        HistoryEntry {
            id: "0123456789abcdef0123456789abcdef".into(),
            send_id: None,
            shared_only: false,
            answer,
            card,
            share: aac_status::answers_engine::StepState::pending(),
            doorbell: aac_status::answers_engine::StepState::pending(),
        }
    }

    #[test]
    fn the_limit_is_the_routes_own_bounds() {
        assert_eq!(limit(None), Some(50));
        assert_eq!(limit(Some("1")), Some(1));
        assert_eq!(limit(Some("500")), Some(500));
        for bad in ["0", "501", "1000", "1.5", "-1", "abc", ""] {
            assert_eq!(limit(Some(bad)), None, "{bad}");
        }
    }

    #[test]
    fn entry_ids_are_lower_hex_and_unique_only_when_present() {
        let ok = json!([{"cardId": "a"}, {"cardId": "b", "entryId": "0123456789abcdef0123456789abcdef"}]);
        assert!(valid_entries(ok.as_array().unwrap()));
        for bad in [
            json!([{"entryId": "XYZ"}]),
            json!([{"entryId": "0123456789ABCDEF0123456789abcdef"}]),
            json!([{"entryId": "0123456789abcdef0123456789abcdef"},
                   {"entryId": "0123456789abcdef0123456789abcdef"}]),
            json!([{"entryId": 7}]),
        ] {
            assert!(!valid_entries(bad.as_array().unwrap()), "{bad}");
        }
        assert!(!valid_entries(&[]));
    }

    #[test]
    fn the_device_words_are_the_reference_ones() {
        assert_eq!(device_of(Some("Mozilla/5.0 (iPhone; CPU iPhone OS 17_0)")), "iPhone");
        assert_eq!(device_of(Some("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15)")), "Mac");
        assert_eq!(device_of(Some("Mozilla/5.0 (Windows NT 10.0)")), "Windows");
        assert_eq!(device_of(Some("Mozilla/5.0 (X11; Linux x86_64)")), "Linux");
        assert_eq!(device_of(None), "browser");
    }

    #[test]
    fn the_entry_view_is_the_routes_projection() {
        let card = json!({
            "id": "vm.lead--Q1", "label": "S-Q1", "seq": 1, "title": "Question",
            "project": "proj", "stream": "S", "askedBy": "lead",
            "options": [{"key": "A", "label": "First", "gets": "x"}],
            "recommendation": {"key": "A", "why": "why"},
        });
        let answer = json!({
            "kind": "choice", "choice": ["A"], "note": null, "via": "deck",
            "answeredAt": "2026-11-19T06:00:00.000Z", "device": "Mac",
            "cardSha256": "ab",
        });
        let row = entry_view(&entry(card, answer), &IndexMap::new());
        assert_eq!(row["id"], "0123456789abcdef0123456789abcdef");
        assert_eq!(row["card"]["cardSha256"], "ab");
        assert_eq!(row["card"]["options"], json!([{"key": "A", "label": "First"}]));
        assert!(row["answer"]["delivery"]["state"].is_string());
    }

    #[test]
    fn the_body_is_the_owned_set_or_a_decline() {
        let id = "0123456789abcdef0123456789abcdef";
        let body = json!({"answers": [{"cardId": "c", "entryId": id}], "sendId": "abcdefghijklmnop"});
        let submit = submit_body(&body, "Mac").unwrap();
        assert_eq!(submit["device"], "Mac");
        assert_eq!(submit["clientSendId"], "abcdefghijklmnop");
        for bad in [
            json!({}),
            json!({"answers": []}),
            json!({"answers": [{"entryId": id}], "extra": 1}),
            json!({"answers": [{"entryId": id}], "sendId": "short"}),
            json!({"answers": [{"entryId": id}], "sendId": 4}),
        ] {
            assert!(submit_body(&bad, "Mac").is_err(), "{bad}");
        }
    }
}
