//! Exact JavaScript numeric references; these tests run only on the Rust build host.
use aac_status::read_context::{fixed, significant};
use serde_json::Value;
#[test]
fn javascript_precision_and_fixed_forms_match_exact_binary_rounding()
-> Result<(), Box<dyn std::error::Error>> {
    let rows: Vec<Value> =
        serde_json::from_str(include_str!("../../../tests/spec/read-numbers.json"))?;
    for row in rows {
        let value = row["value"].as_f64().ok_or("number")?;
        let admitted = row["admitted"] == true;
        if row["kind"] == "significant" {
            let actual = significant(value, row["digits"].as_u64().ok_or("digits")? as u32);
            assert_eq!(actual.is_some(), admitted, "{row}");
            if admitted {
                assert_eq!(actual, row["expected"].as_f64(), "{row}");
            }
        } else {
            let actual = fixed(value, row["places"].as_u64().ok_or("places")? as u32);
            assert_eq!(actual.is_some(), admitted, "{row}");
            if admitted {
                assert_eq!(actual.as_deref(), row["expected"].as_str(), "{row}");
            }
        }
    }
    Ok(())
}

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
