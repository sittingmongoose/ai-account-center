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

#[test]
fn nanosecond_newer_correction_keeps_sol_in_native_scoreboard() {
    let envelope = serde_json::from_str(include_str!(
        "../../../tests/fixtures/run-corrections-precision-scoreboard.json"
    ))
    .unwrap();
    let actual = aac_status::read_scoreboard::compose(envelope).expect("precision fixture composes");
    assert_eq!(actual["recent"][0]["id"], "vm.probe--a-newer");
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
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["model"], "openai/gpt-6.1-sol");
    assert_eq!(rows[0]["observed"]["n"], 1);
}

#[test]
fn full_timestamp_domain_agrees_across_readers() {
    let source: Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/run-corrections-precision-scoreboard.json"
    ))
    .unwrap();
    let cases: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/run-corrections-precision.json")).unwrap();
    for case in cases["cases"].as_array().unwrap() {
        let mut input = source.clone();
        for field in ["runs", "docs"] {
            let docs = if field == "runs" {
                input["data"]["runs"]["runs"].as_array_mut().unwrap()
            } else {
                input["data"]["docs"].as_array_mut().unwrap()
            };
            for row in docs {
                let doc = if field == "runs" { &mut row["doc"] } else { row };
                if doc["id"] == "vm.probe--z-older" {
                    doc["writtenAt"] = case["older"].clone();
                }
                if doc["id"] == "vm.probe--a-newer" {
                    doc["writtenAt"] = case["newer"].clone();
                }
            }
        }
        input["presentation"]["epochs"]
            .as_object_mut()
            .unwrap()
            .extend(case["epochs"].as_object().unwrap().clone());
        let actual = aac_status::read_scoreboard::compose(serde_json::from_value(input).unwrap())
            .expect("accepted precision case");
        assert_eq!(actual["recent"][0]["id"], case["keep"], "{}", case["name"]);
    }
}
