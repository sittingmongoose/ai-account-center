//! sb-R3 parity with the TypeScript oracle: reviews whose own runs are unscored (no catalog
//! model or task type) count toward no blind-review measure, in view or joining from outside
//! a filtered view. The fixture's `expected` is the TypeScript view; `envelope` is the same
//! input as the service receives.
use aac_status::scoreboard::{ViewIn, by_model};
use serde_json::{Value, json};

#[test]
fn unscored_run_reviews_move_no_measure() {
    let fixture: Value =
        serde_json::from_str(include_str!("../../../tests/fixtures/scoreboard-unscored-reviews.json")).unwrap();
    let envelope = serde_json::from_value(fixture["envelope"].clone()).unwrap();
    let actual = aac_status::read_scoreboard::compose(envelope).expect("unscored reviews compose");
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
    // And the rule itself is visible, not just absent: item A stays failed, B and C stay
    // passed (the joining unscored review moved B nowhere), only A is overturned. Counts are
    // compared as numbers: the native view emits 2 where the oracle's JSON says 2.
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
    let number = |at: &str| opus.pointer(at).and_then(Value::as_f64);
    assert_eq!(number("/measures/quality/of"), Some(2.0));
    assert_eq!(number("/measures/quality/per"), Some(3.0));
    assert_eq!(number("/measures/quality/firstTry"), Some(2.0));
    assert_eq!(number("/measures/overclaim/value"), Some(0.3333));
    assert_eq!(number("/measures/overclaim/of"), Some(1.0));
    assert_eq!(number("/measures/overclaim/per"), Some(3.0));
    assert!(
        actual["sources"][0]["detail"]
            .as_str()
            .is_some_and(|d| d.contains("2 reviews ignored")),
        "the two in-view unscored reviews are named as ignored: {}",
        actual["sources"][0]["detail"]
    );
}

fn process_row(id: &str, reviews: i64) -> Value {
    json!({
        "model": "x/one", "label": "One", "runtime": "rt", "effort": "e",
        "process": { "id": id, "version": "1.0" },
        "n": { "observed": 5, "benchmark": 0 },
        "basis": "observed", "ranked": true, "rank": 1, "verdict": "fine",
        "why": "Fine row",
        "observed": {
            "n": 5, "reviews": reviews, "ranked": true, "why": "",
            "measures": {
                "quality": { "value": 1, "n": 5, "low": 0.5, "high": 1, "of": 5, "per": 5 }
            }
        },
        "benchmarks": []
    })
}

fn building_view(rows: Vec<Value>) -> ViewIn {
    serde_json::from_value(json!({
        "catalog": {
            "minRuns": 5,
            "models": [{ "model": "x/one", "label": "One" }],
            "taskTypes": [{ "id": "worknode-build", "label": "WorkNode build" }]
        },
        "uses": [
            { "id": "planning", "label": "Planning", "taskTypes": [] },
            {
                "id": "building",
                "label": "Building",
                "taskTypes": [{
                    "id": "worknode-build",
                    "label": "WorkNode build",
                    "unit": "task",
                    "rows": rows
                }]
            },
            { "id": "pm-users", "label": "PM users", "taskTypes": [] }
        ],
        "sources": []
    }))
    .unwrap()
}

#[test]
fn two_processes_on_the_same_version_keep_both_review_totals() {
    // Four rows / 20 runs: scout-a and scout-b, each split across two files, same version.
    // Runs already key by process id (10). Reviews must too (base keys version only → 5).
    let view = by_model(&building_view(vec![
        process_row("scout-a", 5),
        process_row("scout-a", 5),
        process_row("scout-b", 5),
        process_row("scout-b", 5),
    ]))
    .unwrap();
    let model = &view.views[0].models[0];
    assert_eq!(model.runs, 10);
    assert!(
        model.sheet_head.contains("10 blind reviews"),
        "headline/sheet must keep both processes' reviews: {}",
        model.sheet_head
    );
    let head: String = model.head_runs.iter().map(|run| run.text.as_str()).collect();
    assert!(head.contains("10 blind reviews"), "head was: {head}");
}

#[test]
fn the_same_process_split_across_files_still_totals_reviews_once() {
    let view = by_model(&building_view(vec![
        process_row("scout-a", 5),
        process_row("scout-a", 5),
    ]))
    .unwrap();
    let model = &view.views[0].models[0];
    assert_eq!(model.runs, 5);
    assert!(
        model.sheet_head.contains("5 blind reviews"),
        "the same process must not double-count: {}",
        model.sheet_head
    );
}
