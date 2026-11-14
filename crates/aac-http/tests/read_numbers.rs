//! Exact JavaScript numeric references; these tests run only on the Rust build host.
use aac_status::read_context::{fixed, math_round, round_word, significant};
use serde_json::Value;
#[test]
fn javascript_precision_and_fixed_forms_match_exact_binary_rounding()
-> Result<(), Box<dyn std::error::Error>> {
    let rows: Vec<Value> =
        serde_json::from_str(include_str!("../../../tests/spec/read-numbers.json"))?;
    for row in rows {
        if row["kind"] == "math-round" {
            let value = f64::from_bits(u64::from_str_radix(
                row["valueBits"].as_str().ok_or("value bits")?,
                16,
            )?);
            let (operand, divisor) = match row["op"].as_str().ok_or("operation")? {
                "integer" => (value, 1.0),
                "tenth" => (value * 10.0, 10.0),
                "hundredth" => (value * 100.0, 100.0),
                "percent" => (value * 100.0, 1.0),
                "gib" => ((value / 1_073_741_824.0) * 10.0, 10.0),
                "seconds" => (value / 1000.0, 1.0),
                "hours" => (value / 3_600_000.0, 1.0),
                _ => return Err("unknown operation".into()),
            };
            assert_eq!(
                format!("{:016x}", operand.to_bits()),
                row["operandBits"].as_str().ok_or("operand bits")?,
                "operand: {row}"
            );
            assert_eq!(
                format!("{:016x}", (math_round(operand) / divisor).to_bits()),
                row["expectedBits"].as_str().ok_or("expected bits")?,
                "Math.round: {row}"
            );
            continue;
        }
        let value = row["value"].as_f64().ok_or("number")?;
        let admitted = row["admitted"] == true;
        if row["kind"] == "significant" {
            let actual = significant(value, row["digits"].as_u64().ok_or("digits")? as u32);
            assert_eq!(actual.is_some(), admitted, "{row}");
            if admitted {
                assert_eq!(actual, row["expected"].as_f64(), "{row}");
            }
        } else if row["kind"] == "fixed" {
            let actual = fixed(value, row["places"].as_u64().ok_or("places")? as u32);
            assert_eq!(actual.is_some(), admitted, "{row}");
            if admitted {
                assert_eq!(actual.as_deref(), row["expected"].as_str(), "{row}");
            }
        } else {
            return Err("unknown numeric reference kind".into());
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

#[test]
fn integer_words_are_admitted_only_in_the_proven_js_format_domain() {
    assert_eq!(round_word(-0.49999999999999994).as_deref(), Some("0"));
    assert_eq!(round_word(-1.5).as_deref(), Some("-1"));
    assert_eq!(round_word(1e12).as_deref(), Some("1000000000000"));
    for value in [1e30, -1e30, f64::INFINITY, f64::NEG_INFINITY, f64::NAN] {
        assert!(round_word(value).is_none());
    }
    assert!(math_round(f64::NAN).is_nan());
    assert_eq!(math_round(f64::INFINITY), f64::INFINITY);
    assert_eq!(math_round(f64::NEG_INFINITY), f64::NEG_INFINITY);
}
