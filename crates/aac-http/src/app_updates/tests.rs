use super::*;

#[test]
fn helper_rows_match_typescript_bytes() {
    let examples: Value = serde_json::from_str(include_str!(
        "../../../../tests/spec/agent/app-update-rows.json"
    ))
    .expect("TS fixtures");
    for case in examples.as_array().expect("TS row table") {
        let host = hosts()
            .find(|host| case["host"] == host.id)
            .expect("fixture host");
        let app = case["app"].as_str().expect("fixture app");
        let actual = rows::normalize(host, app, &case["row"])
            .unwrap_or_else(|| rows::blank(host, app, "failed", "helper_invalid"));
        assert_eq!(
            actual.to_string(),
            case["expected"].to_string(),
            "{} {app}",
            host.id
        );
        let restored = rows::restore(host, app, &actual);
        assert_eq!(
            restored.to_string(),
            actual.to_string(),
            "{} {app}",
            host.id
        );
    }
}

#[test]
fn muse_quota_rows_complete_and_restore_with_the_helper_reset_time() {
    let host = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let reset = "2026-11-20 05:00 UTC";
    for (code, status, lead, version) in [
        ("muse_quota_exhausted", "updated", "Updated Muse.", "1.5.0"),
        ("muse_quota_current", "current", "Muse is current.", "1.4.2"),
    ] {
        let mut input = json!({
            "appId": "muse-code", "platform": host.os, "status": status, "messageCode": code,
            "previousVersion": "1.4.2", "version": version, "manager": "native",
            "updateAttempted": true,
        });
        let no_reset =
            rows::normalize(host, "muse-code", &input).expect("quota row without a time");
        assert!(no_reset.get("quotaReset").is_none());
        assert_eq!(
            no_reset["message"],
            format!(
                "{lead} Its quota is used up, so the new models appear after its next session."
            )
        );
        assert_eq!(rows::restore(host, "muse-code", &no_reset), no_reset);

        input["quotaReset"] = json!(reset);
        let mut job = Job::new("muse-quota-fixture".into(), clock());
        job.event(host, &json!({"event":"result","result":input}));
        job.finish();
        assert_eq!(job.state, "completed");
        let result = &job.results[0];
        assert_eq!(result["status"], status);
        assert_eq!(result["quotaReset"], reset);
        assert_eq!(
            result["message"],
            format!(
                "{lead} Its quota is used up until {reset}, so the new models appear after its next session."
            )
        );
        assert_eq!(rows::restore(host, "muse-code", result), *result);
    }
}

#[test]
fn muse_quota_notes_decline_outside_their_app_status_and_reset_shapes() {
    let host = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let mut input = json!({
        "appId": "muse-code", "platform": host.os, "status": "updated",
        "messageCode": "muse_quota_exhausted", "version": "1.5.0", "manager": "native",
    });
    for invalid in [
        Value::Null,
        json!(8),
        json!(""),
        json!("5 pm\nquota"),
        json!("5 pm\u{7f}"),
        json!("x".repeat(65)),
        json!("\u{10000}".repeat(33)),
    ] {
        input["quotaReset"] = invalid;
        assert!(rows::normalize(host, "muse-code", &input).is_none());
    }
    input["quotaReset"] = json!("\u{10000}".repeat(32));
    assert!(rows::normalize(host, "muse-code", &input).is_some());
    input["messageCode"] = json!("updated");
    assert!(rows::normalize(host, "muse-code", &input).is_none());
    input["messageCode"] = json!("muse_quota_current");
    assert!(rows::normalize(host, "muse-code", &input).is_none());
    input["appId"] = json!("omp");
    input["status"] = json!("current");
    assert!(rows::normalize(host, "omp", &input).is_none());
}

#[test]
fn antigravity_isolation_rows_preserve_the_verdict_and_restore() {
    let host = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let app = "antigravity-acp";
    let mut input = json!({
        "appId": app, "platform": host.os, "status": "updated", "version": "1.4.6",
        "manager": "official-download", "messageCode": "antigravity_acp_qualified",
    });
    let verified = rows::normalize(host, app, &input).expect("verified row");
    assert_eq!(
        verified["message"],
        "Updated Antigravity ACP to 1.4.6; account isolation verified"
    );
    assert!(verified.get("wrapperVersion").is_none());
    assert_eq!(rows::restore(host, app, &verified), verified);
    input["qualifyReason"] = json!("no private bus");
    assert!(rows::normalize(host, app, &input).is_none());

    input["messageCode"] = json!("antigravity_acp_isolation_unverified");
    input["wrapperVersion"] = json!("1.4.5");
    let unavailable = rows::normalize(host, app, &input).expect("unverified row");
    assert_eq!(unavailable["wrapperVersion"], "1.4.5");
    assert_eq!(unavailable["qualifyReason"], "no private bus");
    assert_eq!(
        unavailable["message"],
        "Updated Antigravity ACP to 1.4.6; account isolation is not verified yet, so T3 keeps using 1.4.5. The check could not test it: no private bus"
    );
    assert_eq!(rows::restore(host, app, &unavailable), unavailable);

    input["qualifyReason"] = Value::Null;
    input["wrapperVersion"] = Value::Null;
    let none = rows::normalize(host, app, &input).expect("null wrapper row");
    assert!(none.get("wrapperVersion").is_some_and(Value::is_null));
    assert!(none.get("qualifyReason").is_none());
    assert!(
        none["message"]
            .as_str()
            .unwrap_or_default()
            .ends_with("no qualified Antigravity ACP runtime is available to T3")
    );
    input.as_object_mut().expect("row").remove("wrapperVersion");
    let unread = rows::normalize(host, app, &input).expect("unread wrapper row");
    assert!(
        unread["message"]
            .as_str()
            .unwrap_or_default()
            .ends_with("T3 keeps using the release it ran before")
    );

    for bad in [json!("has space"), json!("x".repeat(65)), json!(true)] {
        input["wrapperVersion"] = bad;
        assert!(rows::normalize(host, app, &input).is_none());
    }
    input["wrapperVersion"] = json!("1.4.5");
    for bad in [json!(""), json!("line\nbreak"), json!("x".repeat(161))] {
        input["qualifyReason"] = bad;
        assert!(rows::normalize(host, app, &input).is_none());
    }
    input["qualifyReason"] = Value::Null;
    input["status"] = json!("current");
    assert!(rows::normalize(host, app, &input).is_none());
    input["status"] = json!("updated");
    input["appId"] = json!("muse-code");
    assert!(rows::normalize(host, "muse-code", &input).is_none());
}

#[test]
fn completed_rows_survive_connection_loss_and_malformed_terminal_replacements() {
    let host = hosts().find(|h| h.id == "windows").expect("Windows");
    let mut job = Job::new("abcdefgh".into(), clock());
    job.event(
        host,
        &json!({"event":"app","appId":"codex-desktop","phase":"checking"}),
    );
    let row = json!({"appId":"codex-desktop","platform":"windows","status":"updated",
        "messageCode":"updated","manager":"msix","version":"1.2.3","updateAttempted":true});
    job.event(host, &json!({"event":"result","result":row}));
    assert_eq!(job.results.len(), 1);
    let final_ = Event::Result {
        protocol: 1,
        id: "fixture".into(),
        value: json!({"results":[
            {"appId":"codex-desktop","platform":"windows","status":"unknown","messageCode":"host_timeout"}
        ]}),
    };
    job.finish_host(host, Some(&final_), false);
    assert_eq!(job.results.len(), 12);
    assert_eq!(job.results[0]["status"], "updated");
    job.finish();
    assert_eq!(job.state, "failed");
}

#[test]
fn effective_and_rollback_journals_keep_their_own_bounds_and_recover() {
    let dir = tempfile::tempdir().expect("fixture root");
    let root = dir
        .path()
        .canonicalize()
        .expect("canonical fixture root")
        .join("work");
    let writer = storage::Writer::claim(&root).expect("journal writer");
    assert!(storage::Writer::claim(&root).is_err());
    let mut job = Job::new("01234567-0123-4123-8123-0123456789ab".into(), clock());
    for host in hosts() {
        job.fill(host, "skipped", "skipped_cancelled");
    }
    writer.save(&job).expect("write both journals");
    let legacy =
        aac_files::read_bounded(&root.join("dashboard-job.json"), 65536).expect("legacy bytes");
    let legacy: Value = serde_json::from_slice(&legacy).expect("legacy JSON");
    assert_eq!(legacy["job"]["expectedResults"], 40);
    assert_eq!(
        legacy["job"]["results"]
            .as_array()
            .expect("legacy rows")
            .len(),
        40
    );
    let restored = storage::load(&root).expect("active job");
    assert!(restored.running());
    assert_eq!(restored.results.len(), 84);
    drop(writer);
    let recovered = storage::load(&root).expect("orphan display state");
    assert_eq!(recovered.state, "failed");
    assert!(recovered.hosts.is_none());
    assert_eq!(recovered.results.len(), 84);
}
