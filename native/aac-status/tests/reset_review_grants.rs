use aac_status::reset::*;
use serde_json::{Value, json};
use std::sync::{Arc, Barrier};
const NOW: i64 = 1_700_000_000_000;
fn nonce(n: u64) -> String {
    format!("{n:064x}")
}
fn snap(profile: &str, host: &str) -> Snapshot {
    Snapshot {
        identity: Identity {
            provider: "codex".into(),
            account: "fixture-account".into(),
            organization: "".into(),
            profile: profile.into(),
            host: host.into(),
        },
        observed_at_ms: NOW,
        grants: (0..2)
            .map(|n| Grant {
                id: Some(format!("fixture_credit_{n}")),
                kind: "banked".into(),
                clears: vec!["primary".into()],
                left: 1,
                expires_at_ms: None,
                usable: true,
                words: None,
            })
            .collect(),
        windows: std::collections::BTreeMap::from([("primary".into(), 100.)]),
        executor_ready: true,
        source_binding: format!("binding-{profile}-{host}"),
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
fn begin(store: &mut Store, route: &str, s: &Snapshot, n: u64, index: usize) -> String {
    let inv = store.inventory(route, s, s.observed_at_ms, &nonce(n)).unwrap();
    let key = inv["grants"][index]["grantKey"].as_str().unwrap();
    let who = format!("session:{}", nonce(90));
    let prep = store
        .prepare(route, key, &who, s, s.observed_at_ms, &nonce(n + 1))
        .unwrap();
    let attempt = nonce(n + 2);
    store
        .begin(
            route,
            key,
            &who,
            prep["confirmationToken"].as_str().unwrap(),
            s,
            s.observed_at_ms,
            &attempt,
        )
        .unwrap();
    attempt
}
#[test]
fn many_connections_restarts_and_alias_forms() {
    for round in 0..24 {
        let (dir, mut store) = open();
        let before = snap("profile-a", "host-a");
        let attempt = begin(&mut store, "route-a", &before, 1, 0);
        for (i, (p, h)) in [("profile-b", "host-a"), ("profile-a", "host-b")]
            .into_iter()
            .enumerate()
        {
            let alias = snap(p, h);
            let route = format!("route-{i}");
            let inv = store.inventory(&route, &alias, NOW, &nonce(20 + i as u64)).unwrap();
            assert_eq!(store.pending(&route, NOW).unwrap()["attemptKey"], attempt);
            assert_eq!(
                store.gate(&route, inv["grants"][1]["grantKey"].as_str().unwrap(), NOW, true),
                Err("check-again-required")
            );
        }
        let path = dir.path().join("claims.sqlite");
        let workers: Vec<_> = (0..8).map(|_| Store::open(&path).unwrap()).collect();
        let barrier = Arc::new(Barrier::new(9));
        let locker = rusqlite::Connection::open(&path).unwrap();
        locker.execute_batch("BEGIN IMMEDIATE").unwrap();
        let mut handles = vec![];
        for (i, mut s) in workers.into_iter().enumerate() {
            let b = barrier.clone();
            let a = attempt.clone();
            let mut after = before.clone();
            after.observed_at_ms += 1;
            if i == 1 {
                after.grants[0].left = 0;
                after.windows.insert("primary".into(), 0.);
            }
            handles.push(std::thread::spawn(move || {
                b.wait();
                if i == 0 {
                    s.finish("route-a", &a, Outcome::Reset, None, NOW + 2, None)
                } else {
                    s.check(if i % 2 == 0 { "route-0" } else { "route-a" }, &a, &after, NOW + 2)
                }
            }));
        }
        barrier.wait();
        std::thread::sleep(std::time::Duration::from_millis(100));
        locker.execute_batch("COMMIT").unwrap();
        let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap().unwrap()).collect();
        let confirmed = results[1].clone();
        assert_eq!(confirmed["outcome"], "claimed");
        let events = store.history("route-a").unwrap();
        let verification: Vec<_> = events
            .as_array()
            .unwrap()
            .iter()
            .filter(|v| v["event"] == "verification")
            .collect();
        assert_eq!(
            verification.iter().filter(|v| v["outcome"] == "claimed").count(),
            1,
            "round {round}"
        );
        assert_eq!(verification.last().unwrap()["outcome"], "claimed");
        for r in results.iter().filter(|r| r["outcome"] == "claimed") {
            assert_eq!(*r, confirmed);
        }
        drop(store);
        let mut reopened = Store::open(&path).unwrap();
        for route in ["route-a", "route-0", "route-1"] {
            assert_eq!(reopened.pending(route, NOW).unwrap()["attemptKey"], Value::Null);
            assert_eq!(reopened.check(route, &attempt, &before, NOW + 3).unwrap(), confirmed);
            assert_eq!(reopened.history(route).unwrap(), events);
        }
    }
    println!(
        "24 rounds: eight connections, mixed checks/provider finish, distinct profile and host aliases, reopen/replay all consistent"
    );
}
#[test]
fn two_distinct_grants_produce_ordered_attempts_for_ui_delivery() {
    let (_dir, mut store) = open();
    let mut before = snap("profile-a", "host-a");
    before.identity.provider = "claude".into();
    before.identity.organization = "11111111-1111-4111-8111-111111111111".into();
    before.grants[0].kind = "session".into();
    before.grants[0].clears = vec!["five_hour".into()];
    before.grants[1].kind = "weekly".into();
    before.grants[1].clears = vec!["seven_day".into()];
    before.windows = std::collections::BTreeMap::from([("five_hour".into(), 100.), ("seven_day".into(), 100.)]);
    let first = begin(&mut store, "route-a", &before, 1, 0);
    let mut after = before.clone();
    after.observed_at_ms = NOW + 1;
    after.grants[0].left = 0;
    after.windows.insert("five_hour".into(), 0.);
    let result1 = store
        .finish("route-a", &first, Outcome::Reset, Some(&after), NOW + 2, None)
        .unwrap();
    assert_eq!(result1["outcome"], "claimed");
    let mut next = after.clone();
    next.observed_at_ms = NOW + 3;
    next.source_binding = "post-first-claim-binding".into();
    let second = begin(&mut store, "route-a", &next, 11, 1);
    let inv = store.inventory("route-a", &next, NOW + 3, &nonce(21)).unwrap();
    let history = store.history("route-a").unwrap();
    let pending = store.pending("route-a", NOW).unwrap()["attemptKey"].clone();
    assert_eq!(pending, second);
    let old_check = store.check("route-a", &first, &next, NOW + 4).unwrap();
    assert_eq!(old_check["outcome"], result1["outcome"]);
    assert_eq!(old_check["windows"], result1["windows"]);
    assert_eq!(old_check["evidence"]["latestAttempt"]["attemptKey"], second);
    assert_eq!(store.pending("route-a", NOW).unwrap()["attemptKey"], second);
    assert_eq!(
        store.gate("route-a", inv["grants"][1]["grantKey"].as_str().unwrap(), NOW + 4, true),
        Err("check-again-required")
    );
    let mut response = inv;
    response["history"] = history;
    response["pendingAttempt"] = pending;
    let fixture = json!({"oldResult":result1,"replayedResult":old_check,"newInventory":response,"firstKey":first,"secondKey":second});
    if let Ok(dir) = std::env::var("AAC_RESET_FIXTURE_DIR") {
        std::fs::write(
            std::path::Path::new(&dir).join("reset-ordered-grants.json"),
            serde_json::to_string_pretty(&fixture).unwrap() + "\n",
        )
        .unwrap();
    } else {
        let saved: Value = serde_json::from_str(
            &std::fs::read_to_string(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../web-dashboard/leptos/tests/fixtures/reset-ordered-grants.json"),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(fixture, saved);
    }
    println!(
        "real Store response: attempt1 settled; inventory says attempt2 pending; replay of attempt1 remains a settled answer while attempt2 is durably pending"
    );
}
