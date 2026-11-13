mod common;
use aac_store::{capture, fsx, ops};
use common::temp_dir;
use serde_json::{Value as Json, json};
use std::fs;

fn report(seq: u64, state: &str) -> capture::Report {
    let doc = json!({"schema":"agent-reports/thread@1","writer":"vm.example","seq":seq,
        "writtenAt":"2026-11-13T10:00:00Z","state":state,"doing":"bounded accepted metadata ".repeat(40),
        "agent":{"t3Thread":"thread:example","model":"openai/example","effort":"high"}});
    capture::Report {
        kind: "thread".into(),
        writer: "vm.example".into(),
        object_id: "vm.example".into(),
        seq,
        written_at_ms: 1794564000000 + seq,
        observed_at_ms: 1794564001000 + seq,
        source_sha256: fsx::sha256_hex(&serde_json::to_vec(&doc).unwrap()),
        ids: json!({"computer":"ubuntu","t3ThreadId":"thread:example","writer":"vm.example"}),
        doc,
    }
}
fn history(engine: &aac_store::engine::Engine) -> Json {
    capture::history(
        engine,
        capture::Query {
            kind: "thread".into(),
            writer: "vm.example".into(),
            object_id: "vm.example".into(),
            limit: None,
        },
    )
    .unwrap()
}

#[test]
fn report_revisions_are_permanent_idempotent_compressed_and_replayable() {
    let dir = temp_dir("capture-revisions");
    let engine = ops::open_engine(dir.to_str().unwrap(), None, None).unwrap();
    let a = report(1, "working");
    let b = report(2, "done");
    let c = report(3, "working");
    assert_eq!(
        capture::record(&engine, vec![a.clone(), b.clone(), c.clone()]).unwrap()["stored"],
        3
    );
    assert_eq!(
        capture::record(&engine, vec![a.clone(), b, c]).unwrap()["duplicates"],
        3
    );
    let before = history(&engine);
    let states: Vec<_> = before["revisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["doc"]["state"].clone())
        .collect();
    assert_eq!(states, vec![json!("working"), json!("done"), json!("working")]);
    assert_eq!(before["revisions"][0]["ids"]["t3ThreadId"], "thread:example");
    let health = capture::health(&engine).unwrap();
    assert_eq!(health["revisions"], 3);
    assert!(health["contentBytes"].as_u64().unwrap() < serde_json::to_vec(&a.doc).unwrap().len() as u64 * 3);
    let mut bad = report(4, "done");
    bad.doc["access_token"] = json!("must-not-persist");
    assert!(capture::record(&engine, vec![bad]).is_err());
    let mut mismatch = report(4, "done");
    mismatch.doc["seq"] = json!(7);
    assert!(capture::record(&engine, vec![mismatch]).is_err());
    assert_eq!(history(&engine), before);
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = ops::open_engine(dir.to_str().unwrap(), None, None).unwrap();
    assert_eq!(history(&engine), before);
    assert_eq!(capture::record(&engine, vec![a]).unwrap()["duplicates"], 1);
    engine.close().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn normalized_large_task_boards_fit_without_losing_items() {
    let dir = temp_dir("capture-large-board");
    let engine = ops::open_engine(dir.to_str().unwrap(), None, None).unwrap();
    let mut value = report(1, "working");
    value.kind = "tasks".into();
    value.doc = json!({"schema":"agent-reports/tasks@1","writer":"vm.example","seq":1,
        "writtenAt":"2026-11-13T10:00:00Z","project":"example","stream":"main",
        "items":(0..2000).map(|i|json!({"id":format!("task{i}"),"title":"x".repeat(170),"state":"done",
            "owner":null,"branch":null,"head":null,"reviews":[],"landed":null,"blockedOn":null,"why":null,"updatedAt":null})).collect::<Vec<_>>()});
    assert!(serde_json::to_vec(&value.doc).unwrap().len() > 512 * 1024);
    value.source_sha256 = fsx::sha256_hex(&serde_json::to_vec(&value.doc).unwrap());
    assert_eq!(capture::record(&engine, vec![value]).unwrap()["stored"], 1);
    let stored = capture::history(
        &engine,
        capture::Query {
            kind: "tasks".into(),
            writer: "vm.example".into(),
            object_id: "vm.example".into(),
            limit: None,
        },
    )
    .unwrap();
    assert_eq!(stored["revisions"][0]["doc"]["items"].as_array().unwrap().len(), 2000);
    engine.close().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

/// Both language boundaries read these same synthetic fixtures. Rejected batches must leave
/// authoritative ledger bytes, compressed content and query history unchanged.
#[test]
fn report_reader_secret_parity_refuses_before_any_persistence() {
    use aac_store::report_secret;
    use common::find;
    let cases: Vec<Json> =
        serde_json::from_str(include_str!("../../../tests/fixtures/secret-policy-cases.json")).unwrap();
    let dir = temp_dir("capture-secret-parity");
    let engine = ops::open_engine(dir.to_str().unwrap(), None, None).unwrap();
    let ledger = || {
        find(&dir.join("log/ledger"), "")
            .into_iter()
            .filter(|path| path.is_file())
            .collect::<Vec<_>>()
            .into_iter()
            .map(|path| (path.clone(), fs::read(path).unwrap()))
            .collect::<Vec<_>>()
    };
    capture::record(&engine, vec![report(0, "working")]).unwrap();
    let content = || {
        use redb::{ReadableDatabase, ReadableTable};
        engine
            .with_db(|database| {
                let read = database.begin_read()?;
                let table = read.open_table(capture::CONTENT)?;
                let mut rows = Vec::new();
                for row in table.iter()? {
                    let (key, value) = row?;
                    rows.push((key.value().to_vec(), value.value().to_vec()));
                }
                Ok(rows)
            })
            .unwrap()
    };
    let mut seq = 1;
    for case in cases {
        let fields = &case["doc"];
        let expected = case.get("rustSecret").unwrap_or(&case["secret"]).as_str();
        assert!(
            case["secret"].is_null() || expected.is_some(),
            "native must never be looser"
        );
        assert_eq!(report_secret::find_secret_in("", fields), expected, "{}", case["name"]);
        let mut candidate = report(seq, "working");
        for (key, value) in fields.as_object().unwrap() {
            candidate.doc[key] = value.clone();
        }
        candidate.source_sha256 = fsx::sha256_hex(&serde_json::to_vec(&candidate.doc).unwrap());
        if expected.is_some() {
            let before_log = ledger();
            assert!(
                !before_log.is_empty(),
                "ledger snapshot must inspect actual segment bytes"
            );
            let before_health = capture::health(&engine).unwrap();
            let before_content = content();
            let before_history = history(&engine);
            // A valid report preceding the bad report in a batch must not append either.
            assert!(capture::record(&engine, vec![report(seq + 1, "done"), candidate]).is_err());
            assert_eq!(ledger(), before_log, "{}: ledger untouched", case["name"]);
            assert_eq!(capture::health(&engine).unwrap(), before_health, "content untouched");
            assert_eq!(history(&engine), before_history, "no rejected history");
            assert_eq!(content(), before_content, "compressed content table untouched");
            let mut secret_ids = report(seq, "done");
            secret_ids.ids = fields.clone();
            assert!(capture::record(&engine, vec![secret_ids]).is_err());
            assert_eq!(ledger(), before_log, "ids are checked before append too");
            assert_eq!(capture::health(&engine).unwrap(), before_health);
            assert_eq!(history(&engine), before_history);
            assert_eq!(content(), before_content);
        } else {
            assert_eq!(capture::record(&engine, vec![candidate]).unwrap()["stored"], 1);
        }
        seq += 2;
    }
    let before = history(&engine);
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = ops::open_engine(dir.to_str().unwrap(), None, None).unwrap();
    assert_eq!(
        history(&engine),
        before,
        "ledger replay contains only accepted fixture revisions"
    );
    engine.close().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn shared_policy_keeps_the_legacy_native_credential_shapes() {
    let mut recognized = 0;
    for prefix in [
        "aacd_",
        "sk-ant-",
        "sk-",
        "glpat-",
        "github_pat_",
        "ghp_",
        "gho_",
        "ghs_",
        "ghu_",
        "AIza",
        "AKIA",
        "xoxb-",
        "Bearer ",
        "Basic ",
        "Token ",
        "api_key=",
        "api-key%3D",
        "access_token=",
        "refresh-token=",
        "secret%3D",
    ] {
        for length in 0..=48 {
            for tail in ["A".repeat(length), "a".repeat(length), "Ab9".repeat(length)] {
                let text = format!("prefix: {prefix}{tail}");
                if aac_store::secret::looks_secret(&text) {
                    recognized += 1;
                    assert!(
                        aac_store::report_secret::find_secret(&text).is_some(),
                        "legacy credential shape was lost: {prefix}, length {length}"
                    );
                }
            }
        }
    }
    assert!(recognized > 1000, "union check must exercise legacy-positive shapes");
}

#[test]
fn shared_loader_exposes_the_packaged_policy_without_a_status_dependency() {
    let policy = aac_store::secret_policy::load_secret_policy();
    assert_eq!(
        aac_store::secret_policy::SECRET_POLICY_PATH,
        "src/shared/secret-policy.json"
    );
    assert_eq!(policy["version"], 3);
    assert!(policy["patterns"].as_array().unwrap().len() > 10);
    assert_eq!(aac_store::report_secret::load_secret_policy(), policy);
    assert!(aac_store::secret_policy::find_secret(&format!("sk-{}", "A".repeat(20))).is_some());
    assert!(aac_store::secret_policy::find_secret(&format!("sk-{}", "A".repeat(19))).is_none());
}
