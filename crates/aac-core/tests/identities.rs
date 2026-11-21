//! Golden equality against executed current TS identity functions and the store id format.
#![allow(clippy::unwrap_used)]
use aac_core::*;
use serde_json::Value;

#[test]
fn golden_id_vectors_match_byte_for_byte() {
    let table: Value =
        serde_json::from_str(include_str!("../../../tests/spec/id-vectors.json")).unwrap();
    assert_eq!(table["schemaVersion"], 1);
    for row in table["sessions"].as_array().unwrap() {
        let tool = ToolId::new(row["tool"].as_str().unwrap()).unwrap();
        let raw = row["sessionId"].as_str().unwrap();
        let key = SessionKey::derive(&tool, raw);
        assert_eq!(key.as_ref().map(|k| k.as_str()), row["sessionKey"].as_str());
        assert_eq!(
            SessionHash::derive(&tool, raw).as_str(),
            row["sessionHash"].as_str().unwrap()
        );
        let published = SessionKey::published(&tool, raw);
        assert_eq!(
            published.as_ref().map(|k| k.as_str()),
            row["publishedSessionKey"].as_str()
        );
    }
    for row in table["accounts"].as_array().unwrap() {
        let provider = ProviderId::new(row["provider"].as_str().unwrap()).unwrap();
        assert_eq!(
            AccountKey::derive(
                &provider,
                row["id"].as_str().unwrap(),
                row.get("email").and_then(Value::as_str)
            )
            .unwrap()
            .as_str(),
            row["accountKey"].as_str().unwrap()
        );
    }
    for row in table["events"].as_array().unwrap() {
        let sequence = row["sequence"].as_str().unwrap().parse::<u64>().unwrap();
        assert_eq!(
            EventId::from_sequence(row["storeShortId"].as_str().unwrap(), sequence)
                .unwrap()
                .as_str(),
            row["eventId"].as_str().unwrap()
        );
    }
}

#[test]
fn empty_session_id_has_no_published_key() {
    // m0-spec-F1c: an empty raw id is no id. TS analyticsSessionKey/publishedSessionKey
    // return null and the remote helper returns None; the Rust key must agree.
    let tool = ToolId::new("claude").unwrap();
    assert!(SessionKey::derive(&tool, "").is_none());
    assert!(SessionKey::published(&tool, "").is_none());
    // A non-empty id still publishes a 16-hex digest, and a 16-hex id passes through.
    let key = SessionKey::derive(&tool, "fixture-session").unwrap();
    assert_eq!(key.as_str().len(), 16);
    assert_eq!(
        SessionKey::published(&tool, "0123456789abcdef")
            .unwrap()
            .as_str(),
        "0123456789abcdef"
    );
    // The full session hash is a different function (analyticsSessionHash) and still
    // derives for an empty id, exactly as the TS golden vector records.
    assert_eq!(SessionHash::derive(&tool, "").as_str().len(), 64);
}
