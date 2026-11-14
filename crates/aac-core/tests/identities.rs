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
        assert_eq!(
            SessionKey::derive(&tool, raw).as_str(),
            row["sessionKey"].as_str().unwrap()
        );
        assert_eq!(
            SessionHash::derive(&tool, raw).as_str(),
            row["sessionHash"].as_str().unwrap()
        );
        assert_eq!(
            SessionKey::published(&tool, raw).as_str(),
            row["publishedSessionKey"].as_str().unwrap()
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
