//! Compatibility, bounded-read and secret-boundary regression tests.
#![allow(clippy::unwrap_used)]
use aac_core::*;
use serde_json::{Value, json};

#[test]
fn secret_policy_is_shared_and_scrubs_on_deserialization() {
    let cases: Vec<Value> =
        serde_json::from_str(include_str!("../../../tests/spec/secret-policy.json")).unwrap();
    for case in cases {
        let text = case["text"].as_str().unwrap();
        assert_eq!(
            scrub(text).as_str() == "[redacted]",
            case["secret"].as_bool().unwrap(),
            "fixture {}",
            case["id"]
        );
        let decoded: Scrubbed = serde_json::from_value(json!(text)).unwrap();
        assert_eq!(decoded, scrub(text));
    }
    assert_eq!(scrub("safe\u{001b}text").as_str(), "safetext");
    assert_eq!(scrub(&"x".repeat(4097)).as_str(), "[detail exceeded limit]");
}

#[test]
fn unavailable_cannot_carry_a_value() {
    let reading: Reading<u64> = Reading::Unavailable {
        failure: Failure::new(Cause::Timeout, "no reading"),
    };
    let value = serde_json::to_value(&reading).unwrap();
    assert!(value.get("value").is_none());
    let mut fabricated = value.clone();
    fabricated["value"] = json!(0);
    assert!(serde_json::from_value::<Reading<u64>>(fabricated).is_err());
    let measured: Reading<u64> = Reading::Live {
        value: 0,
        observed_at: UtcMs::new(5).unwrap(),
    };
    assert_eq!(serde_json::to_value(measured).unwrap()["value"], 0);
    assert!(serde_json::from_value::<Reading<u64>>(json!({"state":"live","value":0})).is_err());
}

#[test]
fn timestamps_and_identifiers_validate_at_deserialization() {
    for bad in ["", "../escape", "two words", "x\u{0000}", "a:b"] {
        assert!(ComputerId::new(bad).is_err());
        assert!(serde_json::from_value::<ComputerId>(json!(bad)).is_err());
    }
    assert_eq!(
        ComputerId::new("legacy_ID-1").unwrap().as_str(),
        "legacy_ID-1"
    );
    assert!(UtcMs::new(9_007_199_254_740_992).is_err());
    assert!(serde_json::from_value::<UtcMs>(json!(-1)).is_err());
    assert!(SessionKey::new("0123456789ABCDEf").is_err());
    assert!(UtcMs::now().unwrap().get() > 0);
}

#[test]
fn json_caps_invalid_documents_and_safe_errors() {
    let bytes = b"{\"value\":1}";
    let value: Value = bounded_json(&bytes[..], bytes.len()).unwrap();
    assert_eq!(value["value"], 1);
    assert_eq!(
        bounded_read(&bytes[..], bytes.len() - 1).unwrap_err().cause,
        Cause::OutputTooLarge
    );
    assert!(bounded_read(&bytes[..], usize::MAX).is_err());
    assert_eq!(
        bounded_json::<Value>(&b"{}{}"[..], 10).unwrap_err().cause,
        Cause::Damaged
    );
    assert!(
        bounded_json::<Value>(&b"private malformed source"[..], 100)
            .unwrap_err()
            .detail
            .as_str()
            .find("private")
            .is_none()
    );
}

#[test]
fn every_generated_wire_word_roundtrips_and_keeps_provenance() {
    let table = wire_words().unwrap();
    assert!(table.len() > 100);
    let mut seen = std::collections::BTreeSet::new();
    for entry in table {
        assert!(seen.insert(&entry.word));
        assert!(!entry.sources.is_empty());
        assert_eq!(wire_word(&entry.word).unwrap(), *entry);
        assert_eq!(
            serde_json::from_value::<WireWord>(serde_json::to_value(entry).unwrap()).unwrap(),
            *entry
        );
        for source in &entry.sources {
            let path = source.split_once(':').unwrap().0;
            assert!(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../..")
                    .join(path)
                    .is_file(),
                "{source}"
            );
        }
    }
    assert_eq!(wire_word("ok").unwrap().cause, None);
    assert_eq!(
        wire_word("not_writable").unwrap().cause,
        Some(Cause::NotWritable)
    );
    assert!(wire_word("unknown-future-wire-word").is_err());
}

#[test]
fn wire_vocabulary_sources_are_pinned_to_the_generated_table() {
    let hashes: std::collections::BTreeMap<String, String> =
        serde_json::from_str(include_str!("../../../tests/spec/wire-source-hashes.json")).unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for (source, hash) in hashes {
        assert_eq!(
            sha256_hex(std::fs::read(root.join(&source)).unwrap()),
            hash,
            "wire vocabulary source changed: {source}; regenerate and review"
        );
    }
}

#[test]
fn regression_m1_scrub_checks_the_text_it_returns() {
    let address = "fixture@\0example.invalid".to_owned();
    let prefix = format!("sk-\0{}", "A".repeat(24));
    let body = format!("sk-{}\u{001b}{}", "A".repeat(12), "B".repeat(12));
    for text in [address, prefix, body] {
        let scrubbed = scrub(&text);
        assert_eq!(
            scrub(scrubbed.as_str()),
            scrubbed,
            "scrub must be idempotent"
        );
        assert_eq!(scrubbed.as_str(), "[redacted]");
        let decoded: Scrubbed = serde_json::from_value(json!(text)).unwrap();
        assert_eq!(decoded, scrubbed);
        let failure = Failure::new(Cause::ProviderError, &text);
        assert_eq!(failure.detail, scrubbed);
        assert_eq!(format!("{failure}"), "ProviderError: [redacted]");
    }
}
