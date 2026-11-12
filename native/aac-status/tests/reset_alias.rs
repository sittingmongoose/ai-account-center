use aac_status::reset::*;
use serde_json::Value;
const NOW: i64 = 1_700_000_000_000;
fn nonce(n: u64) -> String {
    format!("{n:064x}")
}
fn snapshot(profile: &str) -> Snapshot {
    Snapshot {
        identity: Identity {
            provider: "codex".into(),
            account: "fixture-account".into(),
            organization: "".into(),
            profile: profile.into(),
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
        source_binding: format!("binding-{profile}"),
        account_label: "fixture-principal".into(),
    }
}
#[test]
fn aliases_discover_the_canonical_pending_attempt_and_history_without_weakening_verification() {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    let a = snapshot("profile-a");
    let b = snapshot("profile-b");
    let ia = store.inventory("route-a", &a, NOW, &nonce(1)).unwrap();
    let key = ia["grants"][0]["grantKey"].as_str().unwrap();
    let caller = format!("session:{}", nonce(90));
    let prep = store.prepare("route-a", key, &caller, &a, NOW, &nonce(2)).unwrap();
    let attempt = nonce(3);
    store
        .begin(
            "route-a",
            key,
            &caller,
            prep["confirmationToken"].as_str().unwrap(),
            &a,
            NOW,
            &attempt,
        )
        .unwrap();
    let ib = store.inventory("route-b", &b, NOW, &nonce(4)).unwrap();
    let kb = ib["grants"][0]["grantKey"].as_str().unwrap();
    assert_eq!(store.gate("route-b", kb, NOW, true), Err("check-again-required"));
    assert_eq!(store.pending("route-b", NOW).unwrap()["attemptKey"], attempt);
    assert_eq!(store.history("route-b").unwrap(), store.history("route-a").unwrap());
    assert_eq!(ia["canonicalAccountKey"], ib["canonicalAccountKey"]);
    assert!(ia["canonicalAccountKey"].is_string());
    let mut after = b.clone();
    after.observed_at_ms += 1;
    after.grants[0].left = 0;
    after.windows.insert("primary".into(), 0.0);
    // Alias discovery does not authorize evidence from another saved profile/host binding.
    assert_eq!(
        store.check("route-b", &attempt, &after, NOW + 2).unwrap()["outcome"],
        "claimed-unconfirmed"
    );
    assert_eq!(store.pending("route-a", NOW).unwrap()["attemptKey"], attempt);
    let mut correct = a.clone();
    correct.observed_at_ms += 1;
    correct.grants[0].left = 0;
    correct.windows.insert("primary".into(), 0.0);
    store.check("route-a", &attempt, &correct, NOW + 2).unwrap();
    assert_eq!(store.pending("route-b", NOW).unwrap()["attemptKey"], Value::Null);
    assert_eq!(store.history("route-b").unwrap(), store.history("route-a").unwrap());
    // Even an inventory with no grants retains canonical discovery after restart.
    drop(store);
    let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
    let mut empty = b;
    empty.grants.clear();
    store.inventory("route-c", &empty, NOW, &nonce(5)).unwrap();
    assert_eq!(store.history("route-c").unwrap(), store.history("route-a").unwrap());
    let mut other = snapshot("other");
    other.identity.account = "different-account".into();
    store.inventory("unrelated", &other, NOW, &nonce(6)).unwrap();
    assert!(store.history("unrelated").unwrap().as_array().unwrap().is_empty());
    assert!(store.check("unrelated", &attempt, &other, NOW + 2).is_err());
}
