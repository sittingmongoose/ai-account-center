mod support;
use aac_status::reset::*;
use support::ReadFixtures;
const NOW: i64 = 1_700_000_000_000;
fn nonce(n: u64) -> String {
    format!("{n:064x}")
}
fn snap(account: &str, profile: &str, host: &str) -> Snapshot {
    Snapshot {
        read_ticket: None,
        identity: Identity {
            provider: "codex".into(),
            account: account.into(),
            organization: "".into(),
            profile: profile.into(),
            host: host.into(),
        },
        observed_at_ms: NOW,
        grants: vec![Grant {
            id: Some("fixture_credit".into()),
            kind: "banked".into(),
            clears: vec!["primary".into()],
            left: 1,
            expires_at_ms: None,
            usable: true,
            words: None,
        }],
        windows: std::collections::BTreeMap::from([("primary".into(), 100.0)]),
        executor_ready: true,
        source_binding: format!("binding-{account}-{profile}-{host}"),
        account_label: "fixture-principal".into(),
    }
}
fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    (dir, store)
}
fn begin(store: &mut Store, route: &str, before: &Snapshot, n: u64) -> String {
    let inv = store.read_inventory(route, before, NOW, &nonce(n)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap();
    let caller = format!("session:{}", nonce(999));
    let prep = store.prepare(route, key, &caller, before, NOW, &nonce(n + 1)).unwrap();
    let attempt = nonce(n + 2);
    store
        .begin(
            route,
            key,
            &caller,
            prep["confirmationToken"].as_str().unwrap(),
            before,
            NOW,
            &attempt,
        )
        .unwrap();
    attempt
}
fn confirmed(before: &Snapshot) -> Snapshot {
    let mut after = before.clone();
    after.observed_at_ms += 1;
    after.grants[0].left -= 1;
    after.windows.insert("primary".into(), 0.0);
    after
}

fn revision(v: &serde_json::Value) -> u64 {
    v["evidence"]["revision"].as_str().unwrap().parse().unwrap()
}
fn golden(name: &str, value: &serde_json::Value) {
    if let Ok(dir) = std::env::var("AAC_RESET_FIXTURE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join(name),
            serde_json::to_string_pretty(value).unwrap() + "\n",
        )
        .unwrap();
    } else {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web-dashboard/leptos/tests/fixtures")
            .join(name);
        let saved: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(
            *value, saved,
            "committed browser fixtures are real transactional Store responses"
        );
    }
}
#[test]
fn recorded_three_attempt_snapshots_are_ordered_coherent_and_durable() {
    let (dir, mut store) = open();
    let route = "reused";
    let mut before = snap("account-x", "profile-a", "host-a");
    before.grants[0].left = 3;
    let empty = store.read_inventory(route, &before, NOW, &nonce(80)).unwrap();
    let a = begin(&mut store, route, &before, 1);
    let inventory1 = store.read_inventory(route, &before, NOW, &nonce(81)).unwrap();
    let settled1 = store
        .read_finish(route, &a, Outcome::Reset, Some(&confirmed(&before)), NOW + 2, None)
        .unwrap();
    before.grants[0].left = 2;
    let b = begin(&mut store, route, &before, 10);
    let inventory2 = store.read_inventory(route, &before, NOW, &nonce(60)).unwrap();
    let settled2 = store
        .read_finish(route, &b, Outcome::Reset, Some(&confirmed(&before)), NOW + 2, None)
        .unwrap();
    before.grants[0].left = 1;
    let c = begin(&mut store, route, &before, 20);
    let unconfirmed3 = store
        .read_finish(route, &c, Outcome::Ambiguous, None, NOW + 3, None)
        .unwrap();
    let inventory3 = store.read_inventory(route, &before, NOW, &nonce(61)).unwrap();
    let grant = inventory3["grants"][0]["grantKey"].as_str().unwrap();
    assert_eq!(
        store
            .prepare(route, grant, "local", &before, NOW, &nonce(62))
            .unwrap_err(),
        "check-again-required"
    );
    let recovery3 = store.pending(route, NOW).unwrap();
    let replay2 = store.read_check(route, &b, &before, NOW + 4).unwrap();
    assert_eq!(revision(&replay2), revision(&inventory3));
    assert_eq!(replay2["attemptKey"], b);
    assert_eq!(replay2["evidence"]["latestAttempt"]["attemptKey"], c);
    assert_eq!(recovery3["evidence"], inventory3["evidence"]);
    assert_eq!(recovery3["history"], inventory3["history"]);
    assert_eq!(replay2["windows"], settled2["windows"]);
    let settled3 = store
        .read_finish(route, &c, Outcome::Reset, Some(&confirmed(&before)), NOW + 5, None)
        .unwrap();
    let final_inventory = store.read_inventory(route, &before, NOW, &nonce(82)).unwrap();
    let ordered = [
        &empty,
        &inventory1,
        &settled1,
        &inventory2,
        &settled2,
        &unconfirmed3,
        &inventory3,
        &settled3,
        &final_inventory,
    ];
    assert!(ordered.windows(2).all(|w| revision(w[0]) < revision(w[1])));
    assert_eq!(inventory1["evidence"]["latestAttempt"]["attemptOrder"], "1");
    assert_eq!(inventory2["evidence"]["latestAttempt"]["attemptOrder"], "2");
    assert_eq!(inventory3["evidence"]["latestAttempt"]["attemptOrder"], "3");
    let fixture = serde_json::json!({"empty":empty,"inventory1":inventory1,"settled1":settled1,"inventory2":inventory2,"settled2":settled2,"unconfirmed3":unconfirmed3,"inventory3":inventory3,"recovery3":recovery3,"replay2":replay2,"settled3":settled3,"finalInventory":final_inventory});
    golden("reset-ordered-attempts.json", &fixture);
    drop(store);
    let store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    assert_eq!(
        store.pending(route, NOW).unwrap()["evidence"],
        fixture["finalInventory"]["evidence"]
    );
}
#[test]
fn recorded_reassignment_has_ordered_bindings_and_separate_accounts() {
    let (_dir, mut store) = open();
    let x = snap("account-x", "reused", "host-a");
    let mut y = snap("account-y", "reused", "host-a");
    y.observed_at_ms = NOW + 1;
    let a = begin(&mut store, "reused", &x, 1);
    let original = store.read_inventory("reused", &x, NOW, &nonce(90)).unwrap();
    let replacement = store.read_inventory("reused", &y, NOW + 1, &nonce(91)).unwrap();
    let sibling = store
        .read_inventory("x-sibling", &snap("account-x", "sibling", "host-b"), NOW, &nonce(92))
        .unwrap();
    assert_ne!(original["canonicalAccountKey"], replacement["canonicalAccountKey"]);
    assert_eq!(original["canonicalAccountKey"], sibling["canonicalAccountKey"]);
    assert_eq!(original["pendingAttempt"], a);
    assert!(replacement["pendingAttempt"].is_null());
    assert_eq!(sibling["pendingAttempt"], a);
    assert!(replacement["history"].as_array().unwrap().is_empty());
    golden(
        "reset-ordered-repoint.json",
        &serde_json::json!([original, replacement, sibling]),
    );
    let settled = store
        .read_finish("reused", &a, Outcome::Reset, Some(&confirmed(&x)), NOW + 2, None)
        .unwrap();
    golden("reset-ordered-repoint-settlement.json", &settled);
}
#[test]
fn revisions_survive_upgrade_from_the_old_schema_and_multiple_connections() {
    let (dir, store) = open();
    drop(store);
    let path = dir.path().join("claims.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(
        "DROP TABLE reset_clock; DROP TABLE reset_revisions; ALTER TABLE aliases DROP COLUMN binding_revision",
    )
    .unwrap();
    drop(db);
    let mut first = Store::open(&path).unwrap();
    let mut second = Store::open(&path).unwrap();
    let a = first
        .read_inventory("a", &snap("account-x", "p", "h"), NOW, &nonce(100))
        .unwrap();
    let b = second
        .read_inventory("b", &snap("account-x", "p2", "h2"), NOW, &nonce(101))
        .unwrap();
    assert!(revision(&b) > revision(&a));
    assert_eq!(first.pending("a", NOW).unwrap()["evidence"], b["evidence"]);
}

// RECHECK-3 M1: the inventory is newer and has no attempt, but begin can still arrive.
#[test]
fn recorded_outstanding_tokens_cover_delayed_begin_and_new_clients() {
    let (dir, mut store) = open();
    let before = snap("account-x", "profile-a", "host-a");
    let first = store.read_inventory("route", &before, NOW, &nonce(300)).unwrap();
    let key = first["grants"][0]["grantKey"].as_str().unwrap();
    let caller = format!("session:{}", nonce(999));
    let prepared = store.prepare("route", key, &caller, &before, NOW, &nonce(301)).unwrap();
    drop(store);
    let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    let reload = store.read_inventory("route", &before, NOW + 10, &nonce(302)).unwrap();
    assert!(revision(&reload) > revision(&prepared));
    assert!(reload["evidence"]["pendingAttempts"].as_array().unwrap().is_empty());
    assert_eq!(reload["evidence"]["claimTokens"]["untilMs"], NOW + 120_000);
    assert_eq!(
        reload["evidence"]["claimTokens"]["tokenKeys"],
        serde_json::json!([prepared["confirmationKey"]])
    );
    assert_ne!(prepared["confirmationKey"], prepared["confirmationToken"]);
    let recovery = store.pending("route", NOW + 11).unwrap();
    assert_eq!(recovery["evidence"], reload["evidence"]);
    store
        .begin(
            "route",
            key,
            &caller,
            prepared["confirmationToken"].as_str().unwrap(),
            &before,
            NOW + 20,
            &nonce(303),
        )
        .unwrap();
    let pending = store.pending("route", NOW + 21).unwrap();
    assert_eq!(pending["attemptKey"], nonce(303));
    assert!(
        pending["evidence"]["claimTokens"]["tokenKeys"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(revision(&pending) > revision(&reload));
    let settled = store
        .read_finish("route", &nonce(303), Outcome::Nothing, None, NOW + 22, None)
        .unwrap();
    let final_inventory = store.read_inventory("route", &before, NOW + 23, &nonce(304)).unwrap();
    let second = store
        .prepare("route", key, &caller, &before, NOW + 24, &nonce(305))
        .unwrap();
    let with_second = store.read_inventory("route", &before, NOW + 25, &nonce(306)).unwrap();
    let before_expiry = store.pending("route", NOW + 120_023).unwrap();
    assert_eq!(before_expiry["evidence"]["claimTokens"]["untilMs"], NOW + 120_024);
    let expired = store.pending("route", NOW + 120_024).unwrap();
    assert!(revision(&expired) > revision(&before_expiry));
    assert!(expired["evidence"]["claimTokens"]["untilMs"].is_null());
    assert_eq!(
        store
            .begin(
                "route",
                key,
                &caller,
                second["confirmationToken"].as_str().unwrap(),
                &before,
                NOW + 120_024,
                &nonce(307)
            )
            .unwrap_err(),
        "confirmation-invalid"
    );
    golden(
        "reset-outstanding-tokens.json",
        &serde_json::json!({"first":first,"prepared":prepared,"reload":reload,"recovery":recovery,"pending":pending,"settled":settled,"finalInventory":final_inventory,"second":second,"withSecond":with_second,"beforeExpiry":before_expiry,"expired":expired}),
    );
}

#[test]
fn outstanding_tokens_follow_original_accounts_in_both_reassignment_directions() {
    for (original, replacement) in [("x", "y"), ("y", "x")] {
        let (_dir, mut store) = open();
        let before = snap(original, "reused", "host-a");
        let inventory = store.read_inventory("reused", &before, NOW, &nonce(310)).unwrap();
        let key = inventory["grants"][0]["grantKey"].as_str().unwrap();
        let prepared = store
            .prepare("reused", key, "local", &before, NOW, &nonce(311))
            .unwrap();
        let mut sibling = before.clone();
        sibling.identity.profile = "sibling".into();
        sibling.identity.host = "host-b".into();
        let sibling_inventory = store.read_inventory("sibling", &sibling, NOW, &nonce(312)).unwrap();
        assert_eq!(
            sibling_inventory["evidence"]["claimTokens"],
            prepared["evidence"]["claimTokens"]
        );
        let mut replacement = snap(replacement, "reused", "host-a");
        replacement.observed_at_ms = NOW + 1;
        store
            .read_inventory("reused", &replacement, NOW + 1, &nonce(313))
            .unwrap();
        assert!(store.pending("reused", NOW).unwrap()["evidence"]["claimTokens"]["untilMs"].is_null());
        assert_eq!(
            store.pending("sibling", NOW).unwrap()["evidence"]["claimTokens"],
            prepared["evidence"]["claimTokens"]
        );
        store
            .begin(
                "reused",
                key,
                "local",
                prepared["confirmationToken"].as_str().unwrap(),
                &before,
                NOW,
                &nonce(314),
            )
            .unwrap();
        assert_eq!(store.pending("sibling", NOW).unwrap()["attemptKey"], nonce(314));
        assert!(store.pending("reused", NOW).unwrap()["attemptKey"].is_null());
    }
}

#[test]
fn migration_restores_unconsumed_tokens_from_immutable_targets() {
    let (dir, mut store) = open();
    let before = snap("x", "reused", "host-a");
    let inventory = store.read_inventory("reused", &before, NOW, &nonce(320)).unwrap();
    let key = inventory["grants"][0]["grantKey"].as_str().unwrap();
    let prepared = store
        .prepare("reused", key, "local", &before, NOW, &nonce(321))
        .unwrap();
    let sibling = store.read_inventory("sibling", &before, NOW, &nonce(322)).unwrap();
    let mut replacement = snap("y", "reused", "host-a");
    replacement.observed_at_ms = NOW + 1;
    store
        .read_inventory("reused", &replacement, NOW + 1, &nonce(323))
        .unwrap();
    drop(store);
    let db = rusqlite::Connection::open(dir.path().join("claims.sqlite")).unwrap();
    db.execute_batch("DROP TABLE claim_tokens").unwrap();
    drop(db);
    let store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    let recovered = store.pending("sibling", NOW).unwrap();
    assert_eq!(
        recovered["evidence"]["claimTokens"],
        prepared["evidence"]["claimTokens"]
    );
    assert!(revision(&recovered) > revision(&sibling));
    assert!(store.pending("reused", NOW).unwrap()["evidence"]["claimTokens"]["untilMs"].is_null());
}

#[test]
fn token_burn_and_other_token_expiry_are_independently_ordered() {
    let (_dir, mut store) = open();
    let before = snap("x", "a", "h");
    let inventory = store.read_inventory("route", &before, NOW, &nonce(330)).unwrap();
    let key = inventory["grants"][0]["grantKey"].as_str().unwrap();
    let first = store.prepare("route", key, "local", &before, NOW, &nonce(331)).unwrap();
    assert_eq!(
        store
            .prepare("route", key, "local", &before, NOW + 1, &nonce(332))
            .unwrap_err(),
        "claim-token-outstanding"
    );
    // Preserve independent burn coverage for a legacy database with two issued tokens.
    let db = rusqlite::Connection::open(_dir.path().join("claims.sqlite")).unwrap();
    use sha2::Digest;
    let token = nonce(332);
    let token_key: String = sha2::Sha256::digest(token.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    db.execute("INSERT INTO confirmations SELECT ?,caller,account,target,fingerprint,expires+1 FROM confirmations WHERE token=?", rusqlite::params![token_key, first["confirmationKey"].as_str().unwrap()]).unwrap();
    db.execute(
        "INSERT INTO claim_tokens SELECT ?,lock_key,expires+1 FROM claim_tokens WHERE token=?",
        rusqlite::params![token_key, first["confirmationKey"].as_str().unwrap()],
    )
    .unwrap();
    drop(db);
    let mut second = store.read_inventory("route", &before, NOW + 1, &nonce(339)).unwrap();
    second["confirmationKey"] = serde_json::json!(token_key);
    second["confirmationToken"] = serde_json::json!(token);
    golden(
        "reset-legacy-tokens.json",
        &serde_json::json!({"first":first,"two":second}),
    );
    assert_eq!(
        second["evidence"]["claimTokens"]["tokenKeys"].as_array().unwrap().len(),
        2
    );
    assert_eq!(
        store
            .begin(
                "route",
                key,
                &format!("session:{}", nonce(999)),
                first["confirmationToken"].as_str().unwrap(),
                &before,
                NOW + 2,
                &nonce(333)
            )
            .unwrap_err(),
        "confirmation-invalid"
    );
    let burned = store.pending("route", NOW + 3).unwrap();
    assert!(revision(&burned) > revision(&second));
    assert_eq!(
        burned["evidence"]["claimTokens"]["tokenKeys"],
        serde_json::json!([second["confirmationKey"]])
    );
    assert_eq!(
        store
            .begin(
                "route",
                key,
                "local",
                first["confirmationToken"].as_str().unwrap(),
                &before,
                NOW + 4,
                &nonce(334)
            )
            .unwrap_err(),
        "confirmation-invalid"
    );
    store
        .begin(
            "route",
            key,
            "local",
            second["confirmationToken"].as_str().unwrap(),
            &before,
            NOW + 4,
            &nonce(335),
        )
        .unwrap();
    assert_eq!(store.pending("route", NOW + 5).unwrap()["attemptKey"], nonce(335));
}

#[test]
fn concurrent_token_consumption_never_exposes_an_empty_reservation_gap() {
    for round in 0..16 {
        let (dir, mut store) = open();
        let before = snap("x", "a", "h");
        let inventory = store.read_inventory("route", &before, NOW, &nonce(340)).unwrap();
        let key = inventory["grants"][0]["grantKey"].as_str().unwrap().to_owned();
        let prepared = store
            .prepare("route", &key, "local", &before, NOW, &nonce(341))
            .unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let ready = barrier.clone();
        let path = dir.path().join("claims.sqlite");
        let reader = std::thread::spawn(move || {
            let store = Store::open(&path).unwrap();
            ready.wait();
            for _ in 0..16 {
                let read = store.pending("route", NOW).unwrap();
                let tokens = read["evidence"]["claimTokens"]["tokenKeys"].as_array().unwrap();
                assert!(
                    !tokens.is_empty() || read["attemptKey"] == nonce(342),
                    "round {round}: consumed token without reserved attempt"
                );
            }
        });
        barrier.wait();
        store
            .begin(
                "route",
                &key,
                "local",
                prepared["confirmationToken"].as_str().unwrap(),
                &before,
                NOW,
                &nonce(342),
            )
            .unwrap();
        reader.join().unwrap();
    }
}

#[test]
fn recorded_pre_prepare_read_requires_invalidation_and_second_prepare_is_refused() {
    let (_dir, mut store) = open();
    let before = snap("account-x", "profile-a", "host-a");
    let old = store.read_inventory("route", &before, NOW, &nonce(900)).unwrap();
    let key = old["grants"][0]["grantKey"].as_str().unwrap();
    let prepared = store
        .prepare("route", key, "local", &before, NOW + 1, &nonce(901))
        .unwrap();
    assert!(revision(&prepared) > revision(&old));
    assert_eq!(
        store
            .prepare("route", key, "local", &before, NOW + 2, &nonce(903))
            .unwrap_err(),
        "claim-token-outstanding"
    );
    assert_eq!(
        store.gate("route", key, NOW + 2, true).unwrap_err(),
        "claim-token-outstanding"
    );
    // The originating token remains actionable; a second page cannot issue another.
    let current = store.read_inventory("route", &before, NOW + 3, &nonce(904)).unwrap();
    store
        .begin(
            "route",
            key,
            "local",
            prepared["confirmationToken"].as_str().unwrap(),
            &before,
            NOW + 20,
            &nonce(902),
        )
        .unwrap();
    let pending = store.pending("route", NOW + 21).unwrap();
    let settled = store
        .read_finish("route", &nonce(902), Outcome::Nothing, None, NOW + 22, None)
        .unwrap();
    let final_read = store.read_inventory("route", &before, NOW + 23, &nonce(905)).unwrap();
    golden(
        "reset-live-fence.json",
        &serde_json::json!({"old":old,"prepared":prepared,"current":current,"pending":pending,"settled":settled,"final":final_read}),
    );
}
fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

// RECHECK-4 M2: unchanged 120-second TTL, real SQLite lock wait, production ABI.
#[test]
fn review_dispatch_must_recheck_expiry_after_database_lock_wait() {
    let (dir, mut store) = open();
    let issued = wall() - 119_000;
    let mut before = snap("account-x", "profile-a", "host-a");
    before.observed_at_ms = issued;
    let inv = store.read_inventory("route", &before, issued, &nonce(910)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap();
    let prep = store
        .prepare("route", key, "local", &before, issued, &nonce(911))
        .unwrap();
    let expiry = prep["expiresAtMs"].as_i64().unwrap();
    before.observed_at_ms = wall();
    let dbpath = dir.path().join("claims.sqlite");
    let args = serde_json::json!({"dbPath":dbpath,"op":"begin","account":"route","nowMs":wall(),"key":key,"caller":"local","token":prep["confirmationToken"],"snapshot":before,"entropy":nonce(912)}).to_string();
    let lock = rusqlite::Connection::open(&dbpath).unwrap();
    lock.execute_batch("BEGIN IMMEDIATE").unwrap();
    let worker = std::thread::spawn(move || dispatch(&args));
    std::thread::sleep(std::time::Duration::from_millis(1800));
    assert!(wall() >= expiry);
    lock.execute_batch("COMMIT").unwrap();
    let got = worker.join().unwrap();
    assert_eq!(
        got.unwrap_err(),
        "confirmation-invalid",
        "execution wall={} expiry={expiry}",
        wall()
    );
}
#[test]
fn execution_and_pending_sample_their_clock_after_begin_immediate_waits() {
    for recovery in [false, true] {
        let (dir, mut store) = open();
        let issued = wall() - 119_000;
        let mut before = snap("x", "a", "h");
        before.observed_at_ms = issued;
        let inv = store.read_inventory("route", &before, issued, &nonce(920)).unwrap();
        let key = inv["grants"][0]["grantKey"].as_str().unwrap().to_owned();
        let prep = store
            .prepare("route", &key, "local", &before, issued, &nonce(921))
            .unwrap();
        let path = dir.path().join("claims.sqlite");
        // Open first, so this wait occurs in execution's BEGIN, after migration has finished.
        let mut executor = Store::open_with_clock(&path, |_| Ok(wall())).unwrap();
        let lock = rusqlite::Connection::open(&path).unwrap();
        lock.execute_batch("BEGIN IMMEDIATE").unwrap();
        before.observed_at_ms = wall();
        let queued_at = wall();
        let worker = std::thread::spawn(move || {
            if recovery {
                let got = executor.pending("route", queued_at).unwrap();
                assert!(got["evidence"]["claimTokens"]["untilMs"].is_null());
            } else {
                assert_eq!(
                    executor
                        .begin(
                            "route",
                            &key,
                            "local",
                            prep["confirmationToken"].as_str().unwrap(),
                            &before,
                            queued_at,
                            &nonce(922)
                        )
                        .unwrap_err(),
                    "confirmation-invalid"
                );
            }
        });
        std::thread::sleep(std::time::Duration::from_millis(1800));
        lock.execute_batch("COMMIT").unwrap();
        worker.join().unwrap();
    }
}
#[test]
fn pending_abi_ignores_request_clock_and_direct_store_clocks_remain_injectable() {
    let (dir, mut store) = open();
    let issued = wall();
    let mut before = snap("x", "a", "h");
    before.observed_at_ms = issued;
    let inv = store.read_inventory("route", &before, issued, &nonce(930)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap();
    store
        .prepare("route", key, "local", &before, issued, &nonce(931))
        .unwrap();
    let path = dir.path().join("claims.sqlite");
    let got =
        dispatch(&serde_json::json!({"dbPath":path,"op":"pending","account":"route","nowMs":i64::MAX}).to_string())
            .unwrap();
    assert_eq!(got["evidence"]["claimTokens"]["untilMs"], issued + 120_000);
    let deterministic = Store::open_with_clock(&path, |given| Ok(given + 120_000)).unwrap();
    assert!(deterministic.pending("route", issued).unwrap()["evidence"]["claimTokens"]["untilMs"].is_null());
}
#[test]
fn concurrent_prepares_on_sibling_aliases_issue_exactly_one_confirmation() {
    let (dir, mut store) = open();
    let mut keys = vec![];
    for (route, profile) in [("a", "profile-a"), ("b", "profile-b")] {
        let s = snap("x", profile, "h");
        let inv = store
            .read_inventory(route, &s, NOW, &nonce(if route == "a" { 940 } else { 941 }))
            .unwrap();
        keys.push((
            route.to_owned(),
            s,
            inv["grants"][0]["grantKey"].as_str().unwrap().to_owned(),
        ));
    }
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = vec![];
    for (i, (route, s, key)) in keys.into_iter().enumerate() {
        let path = dir.path().join("claims.sqlite");
        let barrier = barrier.clone();
        workers.push(std::thread::spawn(move || {
            let mut store = Store::open(&path).unwrap();
            barrier.wait();
            store.prepare(&route, &key, "local", &s, NOW, &nonce(942 + i as u64))
        }));
    }
    let results: Vec<_> = workers.into_iter().map(|w| w.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results.iter().find(|r| r.is_err()).unwrap().as_ref().unwrap_err(),
        &"claim-token-outstanding"
    );
}

#[test]
fn source_tickets_refuse_old_same_or_different_identity_and_replay_without_mutation() {
    for empty in [false, true] {
        let (dir, mut store) = open();
        let mut x = snap("x", "reused", "h");
        x.read_ticket = Some(store.captured_ticket("route").unwrap());
        store.inventory("route", &x, NOW, &nonce(960)).unwrap();
        let mut old_same = snap("y", "reused", "h");
        old_same.read_ticket = Some(store.captured_ticket("route").unwrap());
        let mut y = old_same.clone();
        y.read_ticket = Some(store.captured_ticket("route").unwrap());
        if empty {
            y.grants.clear();
        }
        let newest = store.inventory("route", &y, NOW, &nonce(961)).unwrap();
        drop(store);
        let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        // Timestamp equality and apparent later wall times cannot replace ticket ordering.
        for (index, mut stale) in [x, old_same, y.clone()].into_iter().enumerate() {
            stale.observed_at_ms = NOW + 10;
            assert_eq!(
                store
                    .inventory("route", &stale, NOW + 10, &nonce(962 + index as u64))
                    .unwrap_err(),
                "stale-inventory"
            );
            let got = store.pending("route", NOW + 10).unwrap();
            assert_eq!(got["canonicalAccountKey"], newest["canonicalAccountKey"]);
            assert_eq!(got["bindingRevision"], newest["bindingRevision"]);
            assert_eq!(got["evidence"], newest["evidence"]);
            assert_eq!(got["history"], newest["history"]);
        }
        y.read_ticket = Some(store.captured_ticket("route").unwrap());
        assert!(store.inventory("route", &y, NOW, &nonce(966)).is_ok());
    }
}

#[test]
fn migration_retires_wall_clock_fences_and_rejects_unticketed_inflight_reads() {
    let (dir, mut store) = open();
    let mut x = snap("x", "reused", "h");
    x.observed_at_ms = NOW + 86_400_000;
    let before = store
        .read_inventory("route", &x, x.observed_at_ms, &nonce(970))
        .unwrap();
    drop(store);
    let db = rusqlite::Connection::open(dir.path().join("claims.sqlite")).unwrap();
    db.execute_batch(
        "CREATE TABLE route_sources(account TEXT PRIMARY KEY, identity TEXT NOT NULL, observed_at_ms INTEGER NOT NULL)",
    )
    .unwrap();
    db.execute(
        "INSERT INTO route_sources VALUES('route','legacy-identity',?)",
        [x.observed_at_ms],
    )
    .unwrap();
    drop(db);
    let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    let mut old = x.clone();
    old.read_ticket = None;
    old.observed_at_ms = NOW;
    assert_eq!(
        store.inventory("route", &old, NOW, &nonce(971)).unwrap_err(),
        "stale-inventory"
    );
    assert_eq!(store.pending("route", NOW).unwrap()["evidence"], before["evidence"]);
    let y = snap("y", "reused", "h");
    let recovered = store.read_inventory("route", &y, NOW, &nonce(972)).unwrap();
    assert_ne!(recovered["canonicalAccountKey"], before["canonicalAccountKey"]);
}

// RECHECK-6: every sample is fresh under its current injected clock, including after rollback.
#[test]
fn clock_correction_recovers_replacement_after_restart_without_waiting_for_old_wall_time() {
    for corrected_time in [NOW, NOW + 30_001, NOW + 3_600_000, NOW + 86_400_000] {
        let (dir, mut store) = open();
        let future = NOW + 86_400_000;
        let mut x = snap("x", "reused", "h");
        x.observed_at_ms = future;
        store.read_inventory("route", &x, future, &nonce(1100)).unwrap();
        drop(store);
        let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        let mut y = snap("y", "reused", "h");
        y.observed_at_ms = corrected_time;
        let recovered = store.read_inventory("route", &y, corrected_time, &nonce(1110)).unwrap();
        assert_eq!(
            store.pending("route", corrected_time).unwrap()["canonicalAccountKey"],
            recovered["canonicalAccountKey"]
        );
        x.observed_at_ms = corrected_time;
        store.read_inventory("route", &x, corrected_time, &nonce(1111)).unwrap();
        assert!(store.read_inventory("route", &y, corrected_time, &nonce(1112)).is_ok());
    }
}

#[test]
fn tickets_survive_restart_between_issuance_read_and_acceptance() {
    let (dir, mut store) = open();
    let mut x = snap("x", "reused", "h");
    x.read_ticket = Some(store.captured_ticket("route").unwrap());
    drop(store);
    let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    let mut y = snap("y", "reused", "h");
    y.read_ticket = Some(store.captured_ticket("route").unwrap());
    assert!(
        y.read_ticket.as_ref().unwrap().sequence.parse::<u64>().unwrap()
            > x.read_ticket.as_ref().unwrap().sequence.parse::<u64>().unwrap()
    );
    drop(store);
    let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    let newest = store.inventory("route", &y, NOW, &nonce(1120)).unwrap();
    drop(store);
    let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    assert_eq!(
        store.inventory("route", &x, NOW, &nonce(1121)).unwrap_err(),
        "stale-inventory"
    );
    assert_eq!(
        store.inventory("route", &y, NOW, &nonce(1122)).unwrap_err(),
        "stale-inventory"
    );
    assert_eq!(store.pending("route", NOW).unwrap()["evidence"], newest["evidence"]);
    assert!(store.read_inventory("route", &x, NOW, &nonce(1123)).is_ok());
}

#[test]
fn missing_wrong_route_and_unissued_tickets_do_not_mutate_account_evidence() {
    let (_dir, mut store) = open();
    let before = snap("x", "p", "h");
    let inv = store.read_inventory("route", &before, NOW, &nonce(1130)).unwrap();
    let mut wrong_route = before.clone();
    wrong_route.read_ticket = Some(store.captured_ticket("sibling").unwrap());
    let mut unissued = before.clone();
    unissued.read_ticket = Some(store.captured_ticket("route").unwrap());
    unissued.read_ticket.as_mut().unwrap().sequence = i64::MAX.to_string();
    for (i, bad) in [before, wrong_route, unissued].into_iter().enumerate() {
        assert_eq!(
            store
                .inventory("route", &bad, NOW, &nonce(1131 + i as u64))
                .unwrap_err(),
            "stale-inventory"
        );
        assert_eq!(store.pending("route", NOW).unwrap()["evidence"], inv["evidence"]);
    }
}

#[test]
fn concurrent_read_issuance_is_unique_and_does_not_advance_account_revisions() {
    let (dir, mut store) = open();
    let inv = store
        .read_inventory("route", &snap("x", "p", "h"), NOW, &nonce(1140))
        .unwrap();
    let workers: Vec<_> = (0..4)
        .map(|_| {
            let path = dir.path().join("claims.sqlite");
            std::thread::spawn(move || Store::open(&path).unwrap().captured_ticket("route").unwrap())
        })
        .collect();
    let mut orders: Vec<_> = workers
        .into_iter()
        .map(|w| w.join().unwrap().sequence.parse::<u64>().unwrap())
        .collect();
    orders.sort_unstable();
    orders.dedup();
    assert_eq!(orders.len(), 4);
    assert_eq!(store.pending("route", NOW).unwrap()["evidence"], inv["evidence"]);
}

#[test]
fn ticket_order_verifies_an_attempt_after_wall_clock_correction() {
    let (_dir, mut store) = open();
    let future = NOW + 86_400_000;
    let mut before = snap("x", "p", "h");
    before.observed_at_ms = future;
    before.read_ticket = Some(store.captured_ticket("route").unwrap());
    let inv = store.inventory("route", &before, future, &nonce(1150)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap();
    before.read_ticket = Some(store.captured_ticket("route").unwrap());
    let prep = store
        .prepare("route", key, "local", &before, future, &nonce(1151))
        .unwrap();
    before.read_ticket = Some(store.captured_ticket("route").unwrap());
    store
        .begin(
            "route",
            key,
            "local",
            prep["confirmationToken"].as_str().unwrap(),
            &before,
            future,
            &nonce(1152),
        )
        .unwrap();
    let mut after = confirmed(&before);
    after.observed_at_ms = NOW;
    after.read_ticket = Some(store.captured_ticket("route").unwrap());
    let checked = store.check("route", &nonce(1152), &after, NOW).unwrap();
    assert_eq!(checked["outcome"], "claimed");
    assert_eq!(checked["verification"], "confirmed");
}

#[test]
fn every_source_ticket_delivery_order_keeps_the_newest_identity_and_its_pending_attempt() {
    let mut count = 0;
    for a in 0..4 {
        for b in 0..4 {
            for c in 0..4 {
                for d in 0..4 {
                    let order = [a, b, c, d];
                    if (0..4).any(|i| (i + 1..4).any(|j| order[i] == order[j])) {
                        continue;
                    }
                    let (_dir, mut store) = open();
                    let pending = begin(&mut store, "y-sibling", &snap("y", "sibling", "h"), 1160);
                    let mut samples = vec![];
                    for who in ["x", "y", "x", "y"] {
                        let mut sample = snap(who, "reused", "h");
                        sample.read_ticket = Some(store.captured_ticket("route").unwrap());
                        samples.push(sample);
                    }
                    let mut latest = None;
                    for (step, index) in order.into_iter().enumerate() {
                        let result = store.inventory("route", &samples[index], NOW, &nonce(1170 + step as u64));
                        if latest.is_none_or(|old| index > old) {
                            assert!(result.is_ok());
                            latest = Some(index);
                        } else {
                            assert_eq!(result.unwrap_err(), "stale-inventory");
                        }
                    }
                    let route = store.pending("route", NOW).unwrap();
                    let sibling = store.pending("y-sibling", NOW).unwrap();
                    assert_eq!(route["canonicalAccountKey"], sibling["canonicalAccountKey"]);
                    assert_eq!(route["pendingAttempt"], pending);
                    count += 1;
                }
            }
        }
    }
    assert_eq!(count, 24);
}

#[test]
fn capture_commit_ignores_wall_clock_and_retires_admission_only_abi() {
    let (dir, _) = open();
    let mut store = Store::open_with_clock(&dir.path().join("claims.sqlite"), |_| Err("clock")).unwrap();
    let (first_capture, first) = store.capture_read("route", || Ok("first-identity")).unwrap();
    let (second_capture, second) = store.capture_read("route", || Ok("second-identity")).unwrap();
    assert_eq!(first_capture, "first-identity");
    assert_eq!(second_capture, "second-identity");
    assert_eq!(first.capture_version, 1);
    assert!(second.sequence.parse::<u64>().unwrap() > first.sequence.parse::<u64>().unwrap());
    let result = dispatch(&serde_json::json!({"dbPath":dir.path().join("claims.sqlite"),"account":"route","op":"readTicket","nowMs":i64::MAX}).to_string());
    assert_eq!(result.unwrap_err(), "unknown-method");
}

#[test]
fn admission_only_inflight_tickets_cannot_override_captured_identity_after_upgrade() {
    let (_dir, mut store) = open();
    let mut old = snap("x", "p", "h");
    old.read_ticket = Some(store.captured_ticket("route").unwrap());
    old.read_ticket.as_mut().unwrap().capture_version = 0;
    assert_eq!(
        store.inventory("route", &old, NOW, &nonce(1180)).unwrap_err(),
        "stale-inventory"
    );
    assert!(
        store
            .read_inventory("route", &snap("y", "p", "h"), NOW, &nonce(1181))
            .is_ok()
    );
}

#[test]
fn capture_and_allocation_serialize_across_connections_without_time_ordering() {
    let (dir, _) = open();
    let db = dir.path().join("claims.sqlite");
    let (captured, seen) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let first_db = db.clone();
    let first = std::thread::spawn(move || {
        Store::open(&first_db)
            .unwrap()
            .capture_read("route", || {
                captured.send(()).unwrap();
                wait.recv().unwrap();
                Ok("identity-x")
            })
            .unwrap()
    });
    seen.recv().unwrap();
    let (second_captured, second_seen) = std::sync::mpsc::channel();
    let second = std::thread::spawn(move || {
        Store::open(&db)
            .unwrap()
            .capture_read("route", || {
                second_captured.send(()).unwrap();
                Ok("identity-y")
            })
            .unwrap()
    });
    assert!(second_seen.recv_timeout(std::time::Duration::from_millis(100)).is_err());
    release.send(()).unwrap();
    let (x, xt) = first.join().unwrap();
    let (y, yt) = second.join().unwrap();
    assert_eq!(x, "identity-x");
    assert_eq!(y, "identity-y");
    assert!(yt.sequence.parse::<u64>().unwrap() > xt.sequence.parse::<u64>().unwrap());
}

#[test]
fn failed_capture_rolls_back_without_consuming_an_order_or_account_evidence() {
    let (_dir, mut store) = open();
    let first = store.captured_ticket("route").unwrap();
    let error: Result<((), ReadTicket)> = store.capture_read("route", || Err("signed-out"));
    assert_eq!(error.unwrap_err(), "signed-out");
    let next = store.captured_ticket("route").unwrap();
    assert_eq!(
        next.sequence.parse::<u64>().unwrap(),
        first.sequence.parse::<u64>().unwrap() + 1
    );
    assert!(store.pending("route", NOW).unwrap()["canonicalAccountKey"].is_null());
}

#[cfg(unix)]
#[test]
fn capture_crash_child_fixture() {
    let Ok(path) = std::env::var("AAC_CAPTURE_CRASH_DB") else {
        return;
    };
    let mut store = Store::open(std::path::Path::new(&path)).unwrap();
    let _: Result<((), ReadTicket)> = store.capture_read("route", || {
        std::fs::write(std::env::var("AAC_CAPTURE_CRASH_READY").unwrap(), "locked").unwrap();
        loop {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
}

#[cfg(unix)]
#[test]
fn a_crashed_capture_process_cannot_retain_the_critical_section() {
    let (dir, _) = open();
    let db = dir.path().join("claims.sqlite");
    let ready = dir.path().join("ready");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "capture_crash_child_fixture", "--nocapture"])
        .env("AAC_CAPTURE_CRASH_DB", &db)
        .env("AAC_CAPTURE_CRASH_READY", &ready)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..1000 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    if !ready.exists() {
        let _ = child.kill();
        let _ = child.wait();
        panic!("capture child did not reach its barrier");
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let mut store = Store::open(&db).unwrap();
    let (identity, ticket) = store.capture_read("route", || Ok("replacement")).unwrap();
    assert_eq!(identity, "replacement");
    assert_eq!(ticket.capture_version, 1);
    assert_eq!(ticket.sequence, "1");
}
