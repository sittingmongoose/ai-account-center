use aac_status::reset::*;
use serde_json::Value;
use std::sync::{Arc, Barrier};
const NOW: i64 = 1_700_000_000_000;
fn nonce(n: u64) -> String {
    format!("{n:064x}")
}
fn snap() -> Snapshot {
    Snapshot {
        identity: Identity {
            provider: "codex".into(),
            account: "fixture-account".into(),
            organization: "".into(),
            profile: "fixture-profile".into(),
            host: "fixture-host".into(),
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
        source_binding: "fixture-binding".into(),
        account_label: "fixture-principal".into(),
    }
}
#[test]
fn concurrent_checks_must_not_append_unconfirmed_after_a_confirmed_settlement() {
    let mut regressions = 0;
    for round in 0..16 {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let db_path = dir.path().join("claims.sqlite");
        let mut store = Store::open(&db_path).unwrap();
        let before = snap();
        let caller = format!("session:{}", nonce(90));
        let inv = store.inventory("route", &before, NOW, &nonce(1)).unwrap();
        let key = inv["grants"][0]["grantKey"].as_str().unwrap();
        let prep = store.prepare("route", key, &caller, &before, NOW, &nonce(2)).unwrap();
        let attempt = nonce(3);
        store
            .begin(
                "route",
                key,
                &caller,
                prep["confirmationToken"].as_str().unwrap(),
                &before,
                NOW,
                &attempt,
            )
            .unwrap();
        let mut confirmed = before.clone();
        confirmed.observed_at_ms += 1;
        confirmed.grants[0].left = 0;
        confirmed.windows.insert("primary".into(), 0.0);
        let mut unconfirmed = before.clone();
        unconfirmed.observed_at_ms += 1;
        let mut a = Store::open(&db_path).unwrap();
        let mut b = Store::open(&db_path).unwrap();
        let locker = rusqlite::Connection::open(&db_path).unwrap();
        locker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let barrier = Arc::new(Barrier::new(3));
        let ba = barrier.clone();
        let aa = attempt.clone();
        let wa = std::thread::spawn(move || {
            ba.wait();
            a.check("route", &aa, &confirmed, NOW + 2)
        });
        let bb = barrier.clone();
        let ab = attempt.clone();
        let wb = std::thread::spawn(move || {
            bb.wait();
            b.check("route", &ab, &unconfirmed, NOW + 2)
        });
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(250));
        locker.execute_batch("COMMIT").unwrap();
        let ra = wa.join().unwrap().unwrap();
        let rb = wb.join().unwrap().unwrap();
        let history = store.history("route").unwrap();
        let events = history.as_array().unwrap();
        let outcomes: Vec<_> = events
            .iter()
            .filter(|e| e["event"] == "verification")
            .map(|e| e["outcome"].as_str().unwrap())
            .collect();
        let pending = store.pending("route", NOW).unwrap()["attemptKey"].clone();
        assert_eq!(ra["outcome"], "claimed");
        assert_eq!(pending, Value::Null);
        if outcomes == ["claimed", "claimed-unconfirmed"] && pending == Value::Null {
            regressions += 1;
            eprintln!(
                "round {round}: confirmed check={ra:?}; stale check={rb:?}; audit={outcomes:?}; pending={pending}"
            );
        }
        let settled = store.check("route", &attempt, &before, NOW + 3).unwrap();
        assert_eq!(settled, ra, "a settled replay returns the committed evidence");
        assert_eq!(
            store.history("route").unwrap(),
            history,
            "a settled replay appends nothing"
        );
    }
    assert_eq!(
        regressions, 0,
        "confirmed settlement regressed in permanent audit on real Store::check calls"
    );
}
