//! The Scoreboard composer's partial-view marker and provisional count must match the TS sources
//! (RECHECK-lane-c M1/M2): both compositors carry the same words on the same branches, so the Rust
//! route never serves a partial first view the TS path would mark, nor an unqualified count.
use serde_json::{Value, json};

const PARTIAL: &str = " The scan is still running; this view is partial.";

fn envelope() -> Value {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/run-corrections-precision-scoreboard.json"
    ))
    .unwrap()
}

fn run(env: Value) -> Value {
    aac_status::read_scoreboard::compose(serde_json::from_value(env).unwrap()).expect("composes")
}

#[test]
fn a_partial_scan_marks_both_sources_like_the_ts_compositor() {
    let mut env = envelope();
    // Without the flag neither source carries the marker.
    let plain = run(env.clone());
    assert!(!plain["sources"][0]["detail"].as_str().unwrap().ends_with(PARTIAL));
    assert!(!plain["sources"][1]["detail"].as_str().unwrap().ends_with(PARTIAL));
    // With both readers in flight both sources carry it; the catalog path was missed before.
    env["data"]["runs"]["partial"] = json!(true);
    env["data"]["catalog"]["partial"] = json!(true);
    let view = run(env);
    assert!(view["sources"][0]["detail"].as_str().unwrap().ends_with(PARTIAL));
    assert!(view["sources"][1]["detail"].as_str().unwrap().ends_with(PARTIAL));
}

#[test]
fn a_partial_first_scan_keeps_the_marker_on_the_waiting_branch() {
    let mut env = envelope();
    // A cold run: no scan has answered yet, but one is in flight. The TS wrapper appends the marker
    // to "Waiting for the first scan." too; the Rust source must not drop it.
    env["data"]["runs"]["scannedAt"] = Value::Null;
    env["data"]["runs"]["partial"] = json!(true);
    let view = run(env);
    assert_eq!(
        view["sources"][0]["detail"],
        json!("Waiting for the first scan. The scan is still running; this view is partial.")
    );
}

#[test]
fn a_provisional_last_known_count_is_qualified_like_the_ts_source() {
    let mut env = envelope();
    env["data"]["runs"]["provisionalRunFiles"] = json!(2);
    env["data"]["runs"]["skipped"]["runFiles"] = json!(2);
    let view = run(env);
    let detail = view["sources"][0]["detail"].as_str().unwrap();
    assert!(detail.contains("2 run files not read under folders that could not be listed"));
    assert!(detail.contains(
        "a provisional last-known count (files added or removed since are unknown), not a current count or a guaranteed lower bound."
    ));
}
