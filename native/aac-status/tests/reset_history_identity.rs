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
    let now = before.observed_at_ms;
    let inv = store.read_inventory(route, before, now, &nonce(n)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap();
    let caller = format!("session:{}", nonce(999));
    let prep = store.prepare(route, key, &caller, before, now, &nonce(n + 1)).unwrap();
    let attempt = nonce(n + 2);
    store
        .begin(
            route,
            key,
            &caller,
            prep["confirmationToken"].as_str().unwrap(),
            before,
            now,
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
fn unrelated_account_does_not_inherit_a_repointed_profiles_claim() {
    let (_dir, mut store) = open();
    let x = snap("account-x", "reusable-profile", "host-a");
    let y = snap("account-y", "stable-y-profile", "host-a");
    store
        .read_inventory("codex:stable-y-profile", &y, NOW, &nonce(50))
        .unwrap();
    let attempt_x = begin(&mut store, "codex:reusable-profile", &x, 1);
    let result = store
        .read_finish(
            "codex:reusable-profile",
            &attempt_x,
            Outcome::Reset,
            Some(&confirmed(&x)),
            NOW + 2,
            None,
        )
        .unwrap();
    assert_eq!(result["outcome"], "claimed");
    assert!(
        store
            .history("codex:stable-y-profile")
            .unwrap()
            .as_array()
            .unwrap()
            .is_empty()
    );
    // Supported operation: import-default --force overwrites this saved login with account Y.
    let mut replacement = y.clone();
    replacement.identity.profile = x.identity.profile.clone();
    replacement.source_binding = "new-login-y".into();
    replacement.observed_at_ms = NOW + 1;
    store
        .read_inventory("codex:reusable-profile", &replacement, NOW + 1, &nonce(51))
        .unwrap();
    let unrelated_history = store.history("codex:stable-y-profile").unwrap();
    println!("UNRELATED_ACCOUNT_HISTORY={unrelated_history}");
    assert!(store.pending("codex:stable-y-profile", NOW).unwrap()["attemptKey"].is_null());
    assert!(
        store
            .read_check("codex:stable-y-profile", &attempt_x, &y, NOW + 3)
            .is_err()
    );
    assert!(
        unrelated_history.as_array().unwrap().is_empty(),
        "account Y never claimed, but inherits account X's Used, confirmed record"
    );
}

#[test]
fn canonical_history_does_not_inherit_future_claims_of_a_reused_route() {
    let (_dir, mut store) = open();
    let x = snap("account-x", "reusable-profile", "host-a");
    let alias_x = snap("account-x", "stable-x-profile", "host-a");
    store
        .read_inventory("codex:stable-x-profile", &alias_x, NOW, &nonce(60))
        .unwrap();
    let attempt_x = begin(&mut store, "codex:reusable-profile", &x, 1);
    store
        .read_finish(
            "codex:reusable-profile",
            &attempt_x,
            Outcome::Reset,
            Some(&confirmed(&x)),
            NOW + 2,
            None,
        )
        .unwrap();
    let mut y = snap("account-y", "reusable-profile", "host-a");
    y.observed_at_ms = NOW + 1;
    let attempt_y = begin(&mut store, "codex:reusable-profile", &y, 10);
    store
        .read_finish(
            "codex:reusable-profile",
            &attempt_y,
            Outcome::Nothing,
            None,
            NOW + 3,
            None,
        )
        .unwrap();
    let events = store.history("codex:stable-x-profile").unwrap();
    println!("OLD_ACCOUNT_HISTORY={events}");
    assert_eq!(
        events
            .as_array()
            .unwrap()
            .iter()
            .filter(|e| e["event"] == "attempt")
            .count(),
        1,
        "account X must not inherit the later attempt from unrelated Y"
    );
}

#[test]
fn reassignment_keeps_both_sibling_histories_and_unclaimed_prepares_after_restart() {
    for (original, replacement) in [("account-x", "account-y"), ("account-y", "account-x")] {
        let (dir, mut store) = open();
        let before = snap(original, "reusable-profile", "host-a");
        let old_alias = snap(original, "stable-original-profile", "host-b");
        let new_alias = snap(replacement, "stable-replacement-profile", "host-b");
        store
            .read_inventory("old-sibling", &old_alias, NOW, &nonce(30))
            .unwrap();
        store
            .read_inventory("new-sibling", &new_alias, NOW, &nonce(31))
            .unwrap();
        // A prepare with no attempt still belongs to its immutable inventory target.
        let inventory = store.read_inventory("reused", &before, NOW, &nonce(32)).unwrap();
        let prepared = store
            .prepare(
                "reused",
                inventory["grants"][0]["grantKey"].as_str().unwrap(),
                "local",
                &before,
                NOW,
                &nonce(33),
            )
            .unwrap();
        // Burn without an attempt: its permanent prepare audit must retain original ownership.
        assert_eq!(
            store
                .begin(
                    "reused",
                    inventory["grants"][0]["grantKey"].as_str().unwrap(),
                    &format!("session:{}", nonce(998)),
                    prepared["confirmationToken"].as_str().unwrap(),
                    &before,
                    NOW,
                    &nonce(35)
                )
                .unwrap_err(),
            "confirmation-invalid"
        );
        let attempt = begin(&mut store, "reused", &before, 1);
        store
            .read_finish(
                "reused",
                &attempt,
                Outcome::Reset,
                Some(&confirmed(&before)),
                NOW + 2,
                None,
            )
            .unwrap();
        let original_history = store.history("old-sibling").unwrap();
        assert_eq!(original_history.as_array().unwrap().len(), 4);
        let mut after = snap(replacement, "reusable-profile", "host-a");
        after.observed_at_ms = NOW + 1;
        store.read_inventory("reused", &after, NOW + 1, &nonce(34)).unwrap();
        assert!(store.history("new-sibling").unwrap().as_array().unwrap().is_empty());
        let attempt = begin(&mut store, "reused", &after, 10);
        store
            .read_finish("reused", &attempt, Outcome::Nothing, None, NOW + 3, None)
            .unwrap();
        let replacement_history = store.history("reused").unwrap();
        assert_eq!(replacement_history.as_array().unwrap().len(), 3);
        drop(store);
        let store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        assert_eq!(store.history("old-sibling").unwrap(), original_history);
        assert_eq!(store.history("new-sibling").unwrap(), replacement_history);
        // The audit remains append-only, including records excluded from the new view.
        let db = rusqlite::Connection::open(dir.path().join("claims.sqlite")).unwrap();
        assert_eq!(
            db.query_row("SELECT count(*) FROM history", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            7
        );
    }
}

#[test]
fn x_y_x_reassignment_and_hosts_do_not_move_canonical_history() {
    let (_d, mut s) = open();
    let mut x = snap("account-x", "reused", "host-a");
    let mut y = snap("account-y", "reused", "host-a");
    y.observed_at_ms = NOW + 1;
    let xa = snap("account-x", "x-sibling", "host-b");
    let ya = snap("account-y", "y-sibling", "host-c");
    s.read_inventory("x-sibling", &xa, NOW, &nonce(71)).unwrap();
    s.read_inventory("y-sibling", &ya, NOW, &nonce(72)).unwrap();
    let a = begin(&mut s, "reused", &x, 1);
    s.read_finish("reused", &a, Outcome::Nothing, None, NOW + 2, None)
        .unwrap();
    let hx = s.history("x-sibling").unwrap();
    let b = begin(&mut s, "reused", &y, 10);
    s.read_finish("reused", &b, Outcome::Nothing, None, NOW + 3, None)
        .unwrap();
    let hy = s.history("y-sibling").unwrap();
    x.observed_at_ms = NOW + 2;
    s.read_inventory("reused", &x, NOW + 2, &nonce(73)).unwrap();
    assert_eq!(s.history("reused").unwrap(), hx);
    assert_eq!(s.history("y-sibling").unwrap(), hy);
    y.observed_at_ms = NOW + 3;
    s.read_inventory("reused", &y, NOW + 3, &nonce(74)).unwrap();
    assert_eq!(s.history("reused").unwrap(), hy);
    assert_eq!(s.history("x-sibling").unwrap(), hx);
}
