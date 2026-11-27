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
fn a_scoped_retry_job_round_trips_through_storage() {
    let dir = tempfile::tempdir().expect("fixture root");
    let root = dir.path().join("work");
    let writer = storage::Writer::claim(&root).expect("job lease");
    let ubuntu = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let mut scope = Map::new();
    scope.insert("ubuntu".into(), json!(["antigravity-cli", "zcode"]));
    scope.insert("alexis".into(), json!(["zcode"]));
    let mut job = Job::retry(
        "01234567-0123-4123-8123-0123456789ab".into(),
        clock(),
        scope.clone(),
    );
    job.results
        .push(rows::blank(ubuntu, "zcode", "current", "current"));
    job.finish();
    writer.save(&job).expect("save scoped job");
    drop(writer);
    let loaded = storage::load(&root).expect("scoped job");
    assert_eq!(loaded.scope, Some(scope));
    assert_eq!(loaded.expected_results, Some(3));
    assert_eq!(loaded.results.len(), 1);
    let hosts = loaded.hosts.expect("scoped hosts");
    assert_eq!(hosts.len(), 2);
    assert!(hosts.contains_key("alexis"));
}

#[test]
fn the_legacy_view_drops_non_legacy_scopes() {
    let dir = tempfile::tempdir().expect("fixture root");
    let root = dir.path().join("work");
    let writer = storage::Writer::claim(&root).expect("job lease");
    let mut scope = Map::new();
    for host in ["alexis", "odroid", "1070ti"] {
        scope.insert(host.into(), json!(["zcode"]));
    }
    let job = Job::retry(
        "01234567-0123-4123-8123-0123456789ab".into(),
        clock(),
        scope,
    );
    writer.save(&job).expect("save scoped job");
    drop(writer);
    let legacy: Value = serde_json::from_slice(
        &aac_files::read_bounded(&root.join("dashboard-job.json"), 65_536).expect("legacy bytes"),
    )
    .expect("legacy JSON");
    assert!(legacy["job"].get("scope").is_none());
    assert_eq!(legacy["job"]["expectedResults"], 0);
    assert_eq!(legacy["job"]["hosts"], json!({}));
    std::fs::remove_file(root.join("dashboard-job-expanded.json")).expect("drop expanded job");
    let loaded = storage::load(&root).expect("legacy retry");
    assert!(loaded.results.is_empty());
    assert_eq!(loaded.hosts.as_ref().map_or(0, |hosts| hosts.len()), 0);
    assert!(loaded.scope.as_ref().is_some_and(|scope| scope.is_empty()));
}

#[test]
fn a_crashed_retry_job_recovers_its_missing_scoped_rows() {
    let dir = tempfile::tempdir().expect("fixture root");
    let root = dir.path().join("work");
    let writer = storage::Writer::claim(&root).expect("job lease");
    let mut scope = Map::new();
    scope.insert("ubuntu".into(), json!(["antigravity-cli", "zcode"]));
    let job = Job::retry(
        "01234567-0123-4123-8123-0123456789ab".into(),
        clock(),
        scope,
    );
    writer.save(&job).expect("save running retry");
    drop(writer);
    let loaded = storage::load(&root).expect("orphaned retry");
    assert_eq!(loaded.state, "failed");
    assert_eq!(loaded.results.len(), 2);
    for row in &loaded.results {
        assert_eq!(row["status"], "unknown");
    }
    let scope = retry_scope(&loaded).expect("the missing rows are retryable");
    assert_eq!(scope["ubuntu"], json!(["antigravity-cli", "zcode"]));
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
        scoped: false,
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
fn an_old_agent_that_admits_then_refuses_the_scope_needs_an_update() {
    let host = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let mut scope = Map::new();
    scope.insert("ubuntu".into(), json!(["zcode"]));
    let mut job = Job::retry("retry-fixture-1".into(), clock(), scope);
    let refusal = |id: &str| Event::Failure {
        protocol: 1,
        id: id.to_owned(),
        failure: aac_core::Failure::new(aac_core::Cause::Refused, "invalid app update request"),
    };
    // The real 54f17f2f4 agent admits prepare-update first, then refuses the
    // fourth key, so admitted is true by the time the failure arrives.
    let call = HostCall {
        admitted: true,
        scoped: true,
        terminal: Ok(vec![refusal(&job.id)]),
        until: Instant::now() + Duration::from_secs(30),
    };
    assert_eq!(call.setup_message(), Some("agent_update_required"));
    job.fill(host, "failed", call.setup_message().expect("setup message"));
    assert_eq!(
        job.results[0],
        rows::blank(host, "zcode", "failed", "agent_update_required")
    );
    // A post-admission follow-up refusal is not evidence of an old update agent.
    let follow_up = HostCall {
        admitted: true,
        scoped: false,
        terminal: Ok(vec![refusal("follow-up")]),
        until: Instant::now() + Duration::from_secs(30),
    };
    assert_eq!(follow_up.setup_message(), None);
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

#[test]
fn retired_switching_runtime_codes_are_rejected() {
    let ubuntu = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    for (code, attempted) in [
        ("runtime_unavailable", false),
        ("runtime_paused", true),
        ("runtime_paused_blocked", false),
    ] {
        let row = rows::normalize(
            ubuntu,
            "antigravity-cli",
            &json!({
                "appId": "antigravity-cli",
                "platform": ubuntu.os,
                "status": "failed",
                "messageCode": code,
                "updateAttempted": attempted,
            }),
        );
        assert!(
            row.is_none(),
            "retired switching runtime code {code} must not normalize"
        );
    }
}

#[test]
fn a_retry_scope_is_exactly_the_failed_and_unknown_rows() {
    let ubuntu = hosts().find(|host| host.id == "ubuntu").expect("Ubuntu");
    let mac = hosts().find(|host| host.id == "mac").expect("Mac");
    let windows = hosts().find(|host| host.id == "windows").expect("Windows");
    let mut job = Job::new("01234567-0123-4123-8123-0123456789ab".into(), clock());
    for host in hosts() {
        job.fill(host, "current", "current");
    }
    for (host, app, status) in [
        (ubuntu, "zcode", "failed"),
        (ubuntu, "antigravity-cli", "unknown"),
        (mac, "omp", "restart_failed"),
    ] {
        let index = job.has(host, app).expect("filled row");
        job.results[index]["status"] = json!(status);
    }
    let scope = retry_scope(&job).expect("retry scope");
    assert_eq!(
        Value::Object(scope.clone()),
        json!({"mac":["omp"],"ubuntu":["antigravity-cli","zcode"]})
    );
    let retry = Job::retry(
        "01234567-0123-4123-8123-0123456789ab".into(),
        clock(),
        scope,
    );
    assert_eq!(retry.expected_results, Some(3));
    assert_eq!(retry.hosts.as_ref().expect("retry hosts").len(), 2);
    assert_eq!(retry.apps(ubuntu), ["antigravity-cli", "zcode"]);
    assert!(retry.owns(mac, "omp"));
    assert!(!retry.owns(mac, "zcode"));
    assert!(!retry.in_scope(windows));
    let clean = {
        let mut job = Job::new("01234567-0123-4123-8123-0123456789ab".into(), clock());
        for host in hosts() {
            job.fill(host, "current", "current");
        }
        job.finish();
        job
    };
    assert!(retry_scope(&clean).is_none());
}

#[tokio::test]
async fn retry_refuses_while_a_job_runs() {
    let dir = tempfile::tempdir().expect("fixture root");
    let root = dir.path().join("work");
    let lease = storage::Writer::claim(&root).expect("job lease");
    let service = AppUpdates::new(root);
    *service.active.lock().await = Some(Active {
        job: Job::new("01234567-0123-4123-8123-0123456789ab".into(), clock()),
        cancel: CancellationToken::new(),
        lease,
    });
    let reply = service.retry().await;
    assert_eq!(reply.status, 409);
    assert_eq!(reply.body["error"], "An app update is already running.");
}

#[tokio::test]
async fn retry_refuses_when_the_last_job_has_nothing_to_retry() {
    let dir = tempfile::tempdir().expect("fixture root");
    let root = dir.path().join("work");
    // AppUpdates owns `<root>/app-updates`; the direct writer must use that folder.
    let writer = storage::Writer::claim(&root.join("app-updates")).expect("job lease");
    let mut job = Job::new("01234567-0123-4123-8123-0123456789ab".into(), clock());
    for host in hosts() {
        job.fill(host, "current", "current");
    }
    job.finish();
    writer.save(&job).expect("save completed job");
    drop(writer);
    let reply = AppUpdates::new(root).retry().await;
    assert_eq!(reply.status, 409);
    assert_eq!(reply.body["error"], "There is nothing to retry.");
    assert_eq!(reply.body["job"]["id"], job.id);
}

/// Update apps runs on the dashboard's own computer in place and reaches the VM over its
/// packaged alias when the dashboard runs elsewhere.
#[test]
fn update_apps_reaches_the_vm_over_ssh_only_when_the_dashboard_is_elsewhere() {
    let alias = |here: Option<&'static str>, id: &str| {
        hosts_at(here)
            .find(|host| host.id == id)
            .expect("registered host")
            .alias
    };
    assert_eq!(alias(Some("ubuntu"), "ubuntu"), None);
    assert_eq!(alias(Some("ubuntu"), "nas1"), Some("nas1-agent"));
    assert_eq!(alias(None, "ubuntu"), Some("ubuntu-vm"));
    assert_eq!(hosts_at(None).count(), 7);
}
