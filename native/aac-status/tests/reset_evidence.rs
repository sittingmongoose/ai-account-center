use aac_status::reset::*;
const NOW: i64 = 1_700_000_000_000;
fn nonce(n: u64) -> String {
    format!("{n:064x}")
}
fn snap(account: &str, profile: &str, host: &str) -> Snapshot {
    Snapshot {
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
    let inv = store.inventory(route, before, NOW, &nonce(n)).unwrap();
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
    let empty = store.inventory(route, &before, NOW, &nonce(80)).unwrap();
    let a = begin(&mut store, route, &before, 1);
    let inventory1 = store.inventory(route, &before, NOW, &nonce(81)).unwrap();
    let settled1 = store
        .finish(route, &a, Outcome::Reset, Some(&confirmed(&before)), NOW + 2, None)
        .unwrap();
    before.grants[0].left = 2;
    let b = begin(&mut store, route, &before, 10);
    let inventory2 = store.inventory(route, &before, NOW, &nonce(60)).unwrap();
    let settled2 = store
        .finish(route, &b, Outcome::Reset, Some(&confirmed(&before)), NOW + 2, None)
        .unwrap();
    before.grants[0].left = 1;
    let c = begin(&mut store, route, &before, 20);
    let unconfirmed3 = store
        .finish(route, &c, Outcome::Ambiguous, None, NOW + 3, None)
        .unwrap();
    let inventory3 = store.inventory(route, &before, NOW, &nonce(61)).unwrap();
    let grant = inventory3["grants"][0]["grantKey"].as_str().unwrap();
    assert_eq!(
        store
            .prepare(route, grant, "local", &before, NOW, &nonce(62))
            .unwrap_err(),
        "check-again-required"
    );
    let recovery3 = store.pending(route).unwrap();
    let replay2 = store.check(route, &b, &before, NOW + 4).unwrap();
    assert_eq!(revision(&replay2), revision(&inventory3));
    assert_eq!(replay2["attemptKey"], b);
    assert_eq!(replay2["evidence"]["latestAttempt"]["attemptKey"], c);
    assert_eq!(recovery3["evidence"], inventory3["evidence"]);
    assert_eq!(recovery3["history"], inventory3["history"]);
    assert_eq!(replay2["windows"], settled2["windows"]);
    let settled3 = store
        .finish(route, &c, Outcome::Reset, Some(&confirmed(&before)), NOW + 5, None)
        .unwrap();
    let final_inventory = store.inventory(route, &before, NOW, &nonce(82)).unwrap();
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
        store.pending(route).unwrap()["evidence"],
        fixture["finalInventory"]["evidence"]
    );
}
#[test]
fn recorded_reassignment_has_ordered_bindings_and_separate_accounts() {
    let (_dir, mut store) = open();
    let x = snap("account-x", "reused", "host-a");
    let y = snap("account-y", "reused", "host-a");
    let a = begin(&mut store, "reused", &x, 1);
    let original = store.inventory("reused", &x, NOW, &nonce(90)).unwrap();
    let replacement = store.inventory("reused", &y, NOW, &nonce(91)).unwrap();
    let sibling = store
        .inventory("x-sibling", &snap("account-x", "sibling", "host-b"), NOW, &nonce(92))
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
        .finish("reused", &a, Outcome::Reset, Some(&confirmed(&x)), NOW + 2, None)
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
        .inventory("a", &snap("account-x", "p", "h"), NOW, &nonce(100))
        .unwrap();
    let b = second
        .inventory("b", &snap("account-x", "p2", "h2"), NOW, &nonce(101))
        .unwrap();
    assert!(revision(&b) > revision(&a));
    assert_eq!(first.pending("a").unwrap()["evidence"], b["evidence"]);
}
