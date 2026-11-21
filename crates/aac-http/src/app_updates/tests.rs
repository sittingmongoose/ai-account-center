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
fn retired_antigravity_isolation_rows_remain_updated() {
    let host = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let app = "antigravity-acp";
    for code in [
        "antigravity_acp_qualified",
        "antigravity_acp_isolation_unverified",
    ] {
        let mut input = json!({
            "appId": app, "platform": host.os, "status": "updated", "version": "1.4.6",
            "manager": "official-download", "messageCode": code,
        });
        let row = rows::normalize(host, app, &input).expect("legacy reply");
        assert_eq!(row["status"], "updated");
        assert!(row.get("wrapperVersion").is_none());
        assert!(row.get("qualifyReason").is_none());
        let mut archived = row;
        archived["message"] = json!(if code == "antigravity_acp_qualified" {
            "Updated Antigravity ACP to 1.4.6; account isolation verified"
        } else {
            "Updated Antigravity ACP to 1.4.6; account isolation is not verified yet, so T3 keeps using the release it ran before"
        });
        assert_eq!(rows::restore(host, app, &archived)["status"], "updated");
        input["status"] = json!("current");
        assert!(rows::normalize(host, app, &input).is_none());
    }
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

#[test]
fn older_agents_that_refuse_acp_follow_up_are_reported_as_needing_an_update() {
    let host = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let mut job = Job::new("acp-follow-up-fixture".into(), clock());
    job.results.push(rows::blank(
        host,
        ACP,
        "action_required",
        "t3_restart_pending",
    ));
    let call = HostCall {
        admitted: false,
        terminal: Ok(vec![Event::Failure {
            protocol: 1,
            id: job.id.clone(),
            failure: aac_core::Failure::new(aac_core::Cause::Refused, "invalid apps action"),
        }]),
        until: Instant::now() + Duration::from_secs(30),
    };

    job.finish_acp(host, &call);
    assert_eq!(
        job.results[0],
        rows::blank(host, ACP, "failed", "agent_update_required")
    );
}

#[test]
fn saved_acp_deferral_restores_with_current_words() {
    let host = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let old = "T3 was updated in this run and its server restarts after the job, so Antigravity ACP is checked against the new T3 on the next Update apps.";
    let saved = json!({"platform":"ubuntu", "appId":"antigravity-acp",
                       "status":"action_required", "message":old});
    let row = rows::restore(host, "antigravity-acp", &saved);
    assert_eq!(row["status"], "action_required");
    assert!(
        row["message"]
            .as_str()
            .expect("public words")
            .contains("by itself")
    );
}
