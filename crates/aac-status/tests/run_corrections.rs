//! Full native composition of the shared correction history, with the TypeScript view as oracle.
use serde_json::Value;

#[test]
fn native_scoreboard_matches_typescript_correction_view() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/run-corrections-scoreboard.json")).unwrap();
    let envelope = serde_json::from_value(fixture["envelope"].clone()).unwrap();
    let actual = aac_status::read_scoreboard::compose(envelope).expect("native correction composition");
    // JSON numbers compare by value, just as a dashboard client compares 1 with 1.0.
    fn equal(a: &Value, b: &Value) -> bool {
        match (a, b) {
            (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
            (Value::Array(a), Value::Array(b)) => a.len() == b.len() && a.iter().zip(b).all(|(a, b)| equal(a, b)),
            (Value::Object(a), Value::Object(b)) => {
                a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|b| equal(v, b)))
            }
            _ => a == b,
        }
    }
    assert!(equal(&actual, &fixture["expected"]), "native/TS mismatch: {actual}");
}
