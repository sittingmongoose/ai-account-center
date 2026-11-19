//! sb-R3 parity with the TypeScript oracle: reviews whose own runs are unscored (no catalog
//! model or task type) count toward no blind-review measure, in view or joining from outside
//! a filtered view. The fixture's `expected` is the TypeScript view; `envelope` is the same
//! input as the service receives.
use serde_json::Value;

#[test]
fn unscored_run_reviews_move_no_measure() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/scoreboard-unscored-reviews.json"
    ))
    .unwrap();
    let envelope = serde_json::from_value(fixture["envelope"].clone()).unwrap();
    let actual = aac_status::read_scoreboard::compose(envelope).expect("unscored reviews compose");
    // JSON numbers compare by value, just as a dashboard client compares 1 with 1.0.
    fn equal(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
            (Value::Array(a), Value::Array(b)) => {
                a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b))
            }
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|b| equal(v, b)))
            }
            _ => a == b,
        }
    }
    assert!(
        equal(&actual, &fixture["expected"]),
        "native/TS mismatch: {actual}"
    );
    // And the rule itself is visible, not just absent: item A stays failed, B and C stay
    // passed (the joining unscored review moved B nowhere), only A is overturned.
    let rows = actual["uses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["id"] == "building")
        .unwrap()["taskTypes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["id"] == "build")
        .unwrap()["rows"]
        .as_array()
        .unwrap();
    let opus = rows
        .iter()
        .find(|r| r["model"].as_str().is_some_and(|m| m.contains("opus")))
        .unwrap()["observed"]
        .clone();
    assert_eq!(opus["measures"]["quality"]["of"], 2);
    assert_eq!(opus["measures"]["quality"]["per"], 3);
    assert_eq!(opus["measures"]["quality"]["firstTry"], 2);
    assert_eq!(opus["measures"]["overclaim"]["value"], 0.3333);
    assert_eq!(opus["measures"]["overclaim"]["of"], 1);
    assert_eq!(opus["measures"]["overclaim"]["per"], 3);
    assert!(
        actual["sources"][0]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("2 reviews ignored")),
        "the two in-view unscored reviews are named as ignored: {}",
        actual["sources"][0]["detail"]
    );
}
