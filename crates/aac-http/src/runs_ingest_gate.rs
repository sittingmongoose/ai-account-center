#![cfg(test)]

//! G16b gate: the service's owned run ingest (`scoreboard_owned::ingest_request`) builds the same
//! `runsRecord` request and the same pass problems as the TS ingest (`RunsStore.pass`) from the
//! same `runsScan` output, for every pass in `tests/spec/runs-ingest-vectors.json` (made by the
//! real TS ingest over fixture runs and the real store module).
use crate::scoreboard_owned::{SafePaths, ingest_request};
use serde_json::Value as Json;

#[test]
fn the_rust_ingest_builds_the_ts_record_request_for_every_pass() {
    let file = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tests/spec/runs-ingest-vectors.json");
    let vectors: Json =
        serde_json::from_slice(&std::fs::read(file).expect("vectors")).expect("json");
    let passes = vectors["passes"].as_array().expect("passes");
    assert!(passes.len() >= 3);
    // One owner across the passes, as the service keeps its remembered safe paths.
    let mut safe_paths = SafePaths::new();
    let mut refusals = 0;
    for pass in passes {
        let name = pass["name"].as_str().unwrap_or_default();
        let now = pass["now"].as_u64().expect("now");
        let request = ingest_request(&pass["scan"], now, &mut safe_paths);
        assert_eq!(
            request.record, pass["record"],
            "{name}: the runsRecord request"
        );
        assert_eq!(
            Json::Array(request.problems),
            pass["problems"],
            "{name}: the problems"
        );
        assert_eq!(
            request.more,
            pass["more"].as_bool().unwrap_or(false),
            "{name}: more"
        );
        refusals += request.record["refused"].as_array().map_or(0, Vec::len);
    }
    // Not vacuous: the fixture's refusals (cap, secret text, secret name, torn JSON) are compared.
    assert!(refusals >= 6, "{refusals} refusals compared");
    // No secret-shaped path is remembered for display.
    assert!(safe_paths.values().all(|p| !p.contains("sk-ant-")));
}
