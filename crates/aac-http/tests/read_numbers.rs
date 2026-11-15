//! Presentation-envelope references of the read composers; these tests run only on the Rust build host. The numeric
//! forms (`read-numbers.json`) are checked against aac-num, their one implementation (`crates/aac-num/tests/corpus.rs`).
#[test]
fn locale_equivalent_strings_keep_a_stable_tie() {
    let envelope: aac_status::read_context::Envelope = serde_json::from_value(serde_json::json!({
        "data": null,
        "presentation": {"dates": {}, "epochs": {}, "order": {"é": 3, "e\u{301}": 3}}
    }))
    .expect("fixture envelope");
    let context = aac_status::read_context::Context::new(envelope.presentation);
    assert_eq!(context.compare("é", "e\u{301}"), std::cmp::Ordering::Equal);
    assert!(context.valid());
}

#[test]
fn outcome_trimming_uses_javascript_whitespace() {
    assert_eq!(
        aac_status::read_measures::outcome(&serde_json::json!({"outcome":"\u{feff}done\u{feff}"})),
        1
    );
    assert_eq!(
        aac_status::read_measures::outcome(&serde_json::json!({"outcome":"\u{85}done"})),
        0
    );
}
