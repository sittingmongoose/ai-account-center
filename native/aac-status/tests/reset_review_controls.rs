use aac_status::reset::*;
use serde_json::Value;
use std::sync::{Arc, Barrier};
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
    after.grants[0].left = 0;
    after.windows.insert("primary".into(), 0.0);
    after
}

#[test]
fn mixed_finish_and_alias_checks_remain_monotone_after_restart() {
    for _ in 0..12 {
        let (dir, mut store) = open();
        let before = snap("account-x", "profile-a", "host-a");
        let alias = snap("account-x", "profile-b", "host-b");
        let attempt = begin(&mut store, "route-a", &before, 1);
        store.inventory("route-b", &alias, NOW, &nonce(10)).unwrap();
        let barrier = Arc::new(Barrier::new(7));
        let mut connections = Vec::new();
        for _ in 0..6 {
            connections.push(Store::open(&dir.path().join("claims.sqlite")).unwrap());
        }
        let locker = rusqlite::Connection::open(dir.path().join("claims.sqlite")).unwrap();
        locker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut workers = Vec::new();
        for (i, mut connection) in connections.into_iter().enumerate() {
            let barrier = barrier.clone();
            let attempt = attempt.clone();
            let good = confirmed(&before);
            let wrong_alias = confirmed(&alias);
            let mut stale = before.clone();
            stale.observed_at_ms += 1;
            workers.push(std::thread::spawn(move || {
                barrier.wait();
                match i {
                    0 => connection.finish("route-a", &attempt, Outcome::Reset, None, NOW + 2, None),
                    1 | 2 => connection.check("route-a", &attempt, &good, NOW + 2),
                    3 => connection.check("route-b", &attempt, &wrong_alias, NOW + 2),
                    _ => connection.check("route-a", &attempt, &stale, NOW + 2),
                }
                .unwrap()
            }));
        }
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(80));
        locker.execute_batch("COMMIT").unwrap();
        let answers: Vec<Value> = workers.into_iter().map(|w| w.join().unwrap()).collect();
        let history = store.history("route-b").unwrap();
        let mut settled = false;
        for e in history
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["event"] == "verification")
        {
            assert!(!settled, "verification must never append after settlement");
            settled = e["outcome"] == "claimed";
        }
        assert!(settled);
        assert!(store.pending("route-b", NOW).unwrap()["attemptKey"].is_null());
        drop(store);
        let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        let replay = store.check("route-b", &attempt, &alias, NOW + 3).unwrap();
        for answer in answers.iter().filter(|a| a["outcome"] == "claimed") {
            assert_eq!(*answer, replay);
        }
        assert_eq!(store.history("route-b").unwrap(), history);
    }
}

#[test]
fn alias_matrix_and_legacy_discovery_preserve_exact_identity() {
    for legacy in [false, true] {
        for variant in 0..3 {
            let (dir, mut store) = open();
            let before = snap("account-x", "profile-a", "host-a");
            let mut alias = before.clone();
            if variant != 1 {
                alias.identity.profile = "profile-b".into();
            }
            if variant != 0 {
                alias.identity.host = "host-b".into();
            }
            alias.source_binding = "alias-binding".into();
            let attempt = begin(&mut store, "route-a", &before, 1);
            store.inventory("route-b", &alias, NOW, &nonce(10)).unwrap();
            drop(store);
            if legacy {
                let db = rusqlite::Connection::open(dir.path().join("claims.sqlite")).unwrap();
                db.execute_batch("DROP TABLE aliases").unwrap();
            }
            let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
            let recovery = store.pending("route-b", NOW).unwrap();
            assert_eq!(recovery["attemptKey"], attempt);
            assert!(recovery["bindingRevision"].is_string());
            assert!(recovery["evidence"]["revision"].is_string());
            assert_eq!(store.history("route-b").unwrap(), store.history("route-a").unwrap());
            assert_eq!(
                store.check("route-b", &attempt, &confirmed(&alias), NOW + 2).unwrap()["outcome"],
                "claimed-unconfirmed"
            );
            assert_eq!(store.pending("route-a", NOW).unwrap()["attemptKey"], attempt);
            let mut different_org = alias.clone();
            different_org.identity.organization = "other-org".into();
            store
                .inventory("different-org", &different_org, NOW, &nonce(11))
                .unwrap();
            assert!(store.pending("different-org", NOW).unwrap()["attemptKey"].is_null());
            assert!(
                store
                    .check("different-org", &attempt, &confirmed(&before), NOW + 2)
                    .is_err()
            );
            let expected = store.check("route-a", &attempt, &confirmed(&before), NOW + 2).unwrap();
            assert_eq!(expected["outcome"], "claimed");
            let mut empty = alias.clone();
            empty.grants.clear();
            store.inventory("empty-alias", &empty, NOW, &nonce(12)).unwrap();
            let mut replay = store.check("empty-alias", &attempt, &empty, NOW + 3).unwrap();
            let newer = replay.as_object_mut().unwrap().remove("evidence").unwrap();
            let mut expected = expected;
            let previous = expected.as_object_mut().unwrap().remove("evidence").unwrap();
            assert_eq!(replay, expected);
            assert!(
                newer["revision"].as_str().unwrap().parse::<u64>().unwrap()
                    > previous["revision"].as_str().unwrap().parse::<u64>().unwrap()
            );
            assert_eq!(store.history("empty-alias").unwrap(), store.history("route-a").unwrap());
        }
    }
}
