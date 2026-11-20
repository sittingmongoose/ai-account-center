//! G16 producer gate: the Rust readers-check writer (lane C's `share::check`, used through its
//! public types only) writes the same `<reports>/v1/readers/aac/<writer>.json` bytes as the TS
//! ReadersCheckWriter after one Status scan of the same share and the same runs scan, at the same
//! clock (tests/spec/readers-check-vectors.json, from tests/spec/readers-check-vectors.ts).
use aac_files::registry::{self, Owners};
use aac_status::share::check::{ReadersCheckWriter, RunRow};
use aac_status::share::reports::{AgentReportsReader, Limits};
use aac_status::share::status_docs::{STATUS_DOCS_LIMITS, StatusDocsReader};
use aac_status::share::{NodeFs, Problem};
use serde_json::Value;
use std::collections::BTreeMap;

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../tests/spec/readers-check-vectors.json")).unwrap()
}

/// The service's boot owners with the readers-check row given to the Rust service, as the
/// supervisor hands it over once the R4 groups own it. The row's move is declared here (the
/// bundled registry still says `movesWith: null`), so the owners this test builds are the ones
/// the handoff produces.
fn rust_owns_readers_check() -> (Vec<registry::Row>, Owners) {
    let source = include_str!("../../../config/private-files.json").replace(
        r#""id": "readers-check", "root": "share", "path": "v1/readers/aac/*.json", "class": "state", "maxBytes": 1048576, "mode": "0644", "owner": "ts", "agent": false, "movesWith": null"#,
        r#""id": "readers-check", "root": "share", "path": "v1/readers/aac/*.json", "class": "state", "maxBytes": 1048576, "mode": "0644", "owner": "ts", "agent": false, "movesWith": "status""#,
    );
    let rows = registry::parse(&source).unwrap();
    let files: BTreeMap<String, String> = rows
        .iter()
        .map(|row| {
            let word = if row.id == "readers-check" {
                "rust-service"
            } else {
                row.owner.word()
            };
            (row.id.clone(), word.to_owned())
        })
        .collect();
    let owners = Owners::from_boot(&rows, &files).unwrap();
    (rows, owners)
}

#[test]
fn the_rust_check_files_equal_the_ts_ones() {
    let v = vectors();
    let now = v["now"].as_u64().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (rel, content) in v["share"].as_object().unwrap() {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, content.as_str().unwrap()).unwrap();
    }
    let mut reports = AgentReportsReader::new(root.clone(), Limits::default());
    reports.scan(&NodeFs, now);
    let docs = StatusDocsReader::new(root.clone(), STATUS_DOCS_LIMITS);
    let pins: Vec<(String, String)> = v["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p[0].as_str().unwrap().to_owned(), p[1].as_str().unwrap().to_owned()))
        .collect();
    let runs: Vec<RunRow> = v["runs"]["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| RunRow {
            file: r["file"].as_str().unwrap().into(),
            seq: r["doc"]["seq"].as_u64().unwrap(),
            sha256: r["sha256"].as_str().unwrap().into(),
            first_seen_at: r["firstSeenAt"].as_u64().unwrap(),
        })
        .collect();
    let problems: Vec<Problem> = v["runs"]["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| Problem {
            file: p["file"].as_str().unwrap().into(),
            reason: p["reason"].as_str().unwrap().into(),
            since: p["since"].as_u64().unwrap(),
        })
        .collect();
    let mut writer = ReadersCheckWriter::new(root.clone());
    writer.record_status_scan(&reports.snapshot(), &docs.snapshot(), &pins);
    writer.record_runs(&runs, &problems);
    let (_rows, owners) = rust_owns_readers_check();
    writer.flush(&owners, now);

    let check = root.join("v1").join("readers").join("aac");
    let mut got = BTreeMap::new();
    for entry in std::fs::read_dir(&check).unwrap().flatten() {
        let name = entry.file_name().into_string().unwrap();
        // The registry's file lease keeps `<name>.lock` beside each file; the TS writer has none.
        if name.ends_with(".json") {
            got.insert(
                format!("v1/readers/aac/{name}"),
                std::fs::read_to_string(entry.path()).unwrap(),
            );
        }
    }
    let want: BTreeMap<String, String> = v["check"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_owned()))
        .collect();
    assert_eq!(got.keys().collect::<Vec<_>>(), want.keys().collect::<Vec<_>>());
    for (name, body) in &want {
        assert_eq!(&got[name], body, "{name}");
    }
}
