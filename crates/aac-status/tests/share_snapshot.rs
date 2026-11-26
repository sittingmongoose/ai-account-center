//! The reports snapshot shares its documents: two snapshots of an unchanged scan hold
//! the same `Arc` per document, so a Status pass over thousands of retained reports
//! clones no document tree (row status-scan-cpu). On the base, where `SeenReport::doc`
//! is a plain `Value` deep-cloned per snapshot, this does not compile.
use aac_status::share::NodeFs;
use aac_status::share::reports::{AgentReportsReader, Limits};
use serde_json::Value;

fn vectors() -> Value {
    serde_json::from_str(include_str!("../../../tests/spec/readers-check-vectors.json")).unwrap()
}

#[test]
fn snapshots_share_the_retained_documents() {
    let v = vectors();
    let now = v["now"].as_u64().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (rel, content) in v["share"].as_object().unwrap() {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, content.as_str().unwrap()).unwrap();
    }
    let mut reader = AgentReportsReader::new(root, Limits::default());
    reader.scan(&NodeFs, now);
    let first = reader.snapshot();
    let second = reader.snapshot();
    assert!(!first.threads.is_empty(), "the fixture scan reads threads");
    assert_eq!(first.threads.len(), second.threads.len());
    for (a, b) in first.threads.iter().zip(&second.threads) {
        assert!(
            std::sync::Arc::ptr_eq(&a.doc, &b.doc),
            "{}: snapshots must share the retained document",
            a.file
        );
    }
    for (a, b) in first.cards.iter().zip(&second.cards) {
        assert!(std::sync::Arc::ptr_eq(&a.doc, &b.doc), "{}", a.file);
    }
    let changed = &first.threads[0];
    let path = reader.root().join("v1").join(&changed.file);
    let mut new_doc: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    new_doc["seq"] = Value::from(new_doc["seq"].as_u64().unwrap() + 1);
    std::fs::write(&path, serde_json::to_vec(&new_doc).unwrap()).unwrap();
    assert!(reader.scan(&NodeFs, now + 5000));
    let third = reader.snapshot();
    assert!(!std::sync::Arc::ptr_eq(changed, &third.threads[0]));
    for (a, b) in first.threads.iter().skip(1).zip(third.threads.iter().skip(1)) {
        assert!(std::sync::Arc::ptr_eq(a, b), "quiet writer row must stay shared");
    }
    for (a, b) in first.cards.iter().zip(&third.cards) {
        assert!(std::sync::Arc::ptr_eq(a, b), "quiet card row must stay shared");
    }
    // The state text is unchanged by the sharing.
    assert_eq!(
        aac_status::share::reports::state_json(&first),
        aac_status::share::reports::state_json(&second)
    );
}
