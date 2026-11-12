mod support;
use aac_status::reset::*;
use serde_json::json;
use support::ReadFixtures;
const NOW: i64 = 1_000_000;
fn entropy(n: u8) -> String {
    format!("{n:064x}")
}
fn caller() -> String {
    format!("session:{}", entropy(20))
}
fn snapshot(provider: &str) -> Snapshot {
    Snapshot {
        read_ticket: None,
        identity: Identity {
            provider: provider.into(),
            account: "synthetic-account".into(),
            organization: "synthetic-org".into(),
            profile: "synthetic-profile".into(),
            host: "windows".into(),
        },
        observed_at_ms: NOW,
        grants: vec![
            Grant {
                id: Some("grant_a".into()),
                kind: if provider == "codex" { "banked" } else { "weekly" }.into(),
                clears: vec!["seven_day".into()],
                left: 2,
                expires_at_ms: None,
                usable: true,
                words: None,
            },
            Grant {
                id: Some("grant_b".into()),
                kind: if provider == "codex" { "banked" } else { "weekly" }.into(),
                clears: vec!["seven_day".into()],
                left: 1,
                expires_at_ms: None,
                usable: true,
                words: None,
            },
        ],
        windows: std::collections::BTreeMap::from([("seven_day".into(), 100.0)]),
        executor_ready: true,
        source_binding: "synthetic-binding".into(),
        account_label: "synthetic-principal".into(),
    }
}
fn inventory(s: &mut Store, snap: &Snapshot) -> String {
    static NONCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1000);
    let nonce = NONCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    s.read_inventory("route-account", snap, NOW, &format!("{nonce:064x}"))
        .unwrap()["grants"][1]["grantKey"]
        .as_str()
        .unwrap()
        .into()
}
fn prepare(s: &mut Store, k: &str, snap: &Snapshot) -> String {
    s.prepare("route-account", k, &caller(), snap, NOW, &entropy(2))
        .unwrap()["confirmationToken"]
        .as_str()
        .unwrap()
        .into()
}
fn open() -> (tempfile::TempDir, Store) {
    let dir = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let s = Store::open(&dir.path().join("reset.sqlite")).unwrap();
    (dir, s)
}
fn begin(s: &mut Store, snap: &Snapshot) -> (String, String) {
    let k = inventory(s, snap);
    let t = prepare(s, &k, snap);
    let mut fresh = snap.clone();
    fresh.read_ticket = Some(s.read_ticket("route-account").unwrap());
    s.begin("route-account", &k, &caller(), &t, &fresh, NOW, &entropy(3))
        .unwrap();
    (k, entropy(3))
}
#[test]
fn exact_selected_grant_and_transport() {
    for provider in ["claude", "codex"] {
        let (_d, mut s) = open();
        let snap = snapshot(provider);
        let k = inventory(&mut s, &snap);
        let t = prepare(&mut s, &k, &snap);
        let w = s
            .begin("route-account", &k, &caller(), &t, &snap, NOW, &entropy(3))
            .unwrap();
        assert_eq!(
            w.body[if provider == "claude" { "grant_id" } else { "creditId" }],
            "grant_b"
        );
        assert_eq!(w.identity, snap.identity);
        assert!(
            s.begin("route-account", &k, &caller(), &t, &snap, NOW, &entropy(4))
                .is_err()
        );
    }
}
#[test]
fn public_inventory_and_audit_have_no_private_identity() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let out = s
        .read_inventory("route-account", &snap, NOW, &entropy(1))
        .unwrap()
        .to_string();
    for secret in [
        "synthetic-account",
        "synthetic-org",
        "synthetic-profile",
        "windows",
        "grant_a",
        "grant_b",
    ] {
        assert!(!out.contains(secret));
    }
    begin(&mut s, &snap);
    let h = s.history("route-account").unwrap().to_string();
    assert!(!h.contains("grant_b"));
    assert!(!h.contains("synthetic"));
}
#[test]
fn wrong_caller_burns_confirmation() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let k = inventory(&mut s, &snap);
    let t = prepare(&mut s, &k, &snap);
    assert!(
        s.begin(
            "route-account",
            &k,
            &format!("session:{}", entropy(21)),
            &t,
            &snap,
            NOW,
            &entropy(3)
        )
        .is_err()
    );
    assert!(
        s.begin("route-account", &k, &caller(), &t, &snap, NOW, &entropy(3))
            .is_err()
    );
}
#[test]
fn device_tokens_view_only() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let k = inventory(&mut s, &snap);
    assert_eq!(
        s.prepare("route-account", &k, "device:synthetic", &snap, NOW, &entropy(2))
            .unwrap_err(),
        "browser-session-required"
    );
}
#[test]
fn expired_token_rejected_at_boundary() {
    let (_d, mut s) = open();
    let mut snap = snapshot("claude");
    let k = inventory(&mut s, &snap);
    let t = prepare(&mut s, &k, &snap);
    snap.observed_at_ms = NOW + 120_000;
    assert!(
        s.begin("route-account", &k, &caller(), &t, &snap, NOW + 120_000, &entropy(3))
            .is_err()
    );
}
#[test]
fn identity_and_grant_drift_never_substituted() {
    for field in 0..7 {
        let (_d, mut s) = open();
        let snap = snapshot("claude");
        let k = inventory(&mut s, &snap);
        let t = prepare(&mut s, &k, &snap);
        let mut fresh = snap.clone();
        match field {
            0 => fresh.identity.account.push('x'),
            1 => fresh.identity.organization.push('x'),
            2 => fresh.identity.profile.push('x'),
            3 => fresh.identity.host.push('x'),
            4 => fresh.grants[1].left = 0,
            5 => {
                fresh.grants.remove(1);
            }
            _ => fresh.grants[1].kind = "full".into(),
        };
        assert!(
            s.begin("route-account", &k, &caller(), &t, &fresh, NOW, &entropy(3))
                .is_err()
        );
    }
}
#[test]
fn no_id_unknown_kind_and_mac_only_are_read_only() {
    for mode in 0..5 {
        let (_d, mut s) = open();
        let mut snap = snapshot("claude");
        match mode {
            0 => snap.grants[1].id = None,
            1 => snap.grants[1].kind = "future".into(),
            2 => snap.executor_ready = false,
            3 => snap.identity.provider = "zai".into(),
            _ => snap.identity.provider = "qwen".into(),
        };
        let k = inventory(&mut s, &snap);
        assert!(
            s.prepare("route-account", &k, &caller(), &snap, NOW, &entropy(2))
                .is_err()
        );
    }
}
#[test]
fn ambiguous_survives_restart_and_blocks_new_spend() {
    let (d, mut s) = open();
    let snap = snapshot("codex");
    let (k, a) = begin(&mut s, &snap);
    assert_eq!(
        s.read_finish("route-account", &a, Outcome::Ambiguous, None, NOW, None)
            .unwrap()["outcome"],
        "claimed-unconfirmed"
    );
    drop(s);
    let mut s = Store::open(&d.path().join("reset.sqlite")).unwrap();
    assert_eq!(
        s.prepare("route-account", &k, &caller(), &snap, NOW, &entropy(8))
            .unwrap_err(),
        "check-again-required"
    );
}
#[test]
fn account_aliases_cannot_bypass_pending_lock() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    begin(&mut s, &snap);
    let k = s.read_inventory("alias", &snap, NOW, &entropy(10)).unwrap()["grants"][0]["grantKey"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        s.prepare("alias", &k, &caller(), &snap, NOW, &entropy(11)).unwrap_err(),
        "check-again-required"
    );
}
#[test]
fn fresh_verification_requires_exact_decrement_and_refill() {
    for mode in 0..6 {
        let (_d, mut s) = open();
        let mut snap = snapshot("claude");
        snap.read_ticket = Some(s.read_ticket("route-account").unwrap());
        let (_, a) = begin(&mut s, &snap);
        let mut after = snap.clone();
        after.observed_at_ms += 1;
        after.grants[1].left = 0;
        after.windows.insert("seven_day".into(), 0.0);
        match mode {
            1 => after.observed_at_ms = NOW,
            2 => after.identity.account.push('x'),
            3 => after.grants[1].left = 1,
            4 => {
                after.windows.insert("seven_day".into(), 100.0);
            }
            5 => {
                after.grants.remove(1);
            }
            _ => {}
        };
        let r = if mode == 1 {
            // This read started before begin; equal wall timestamps cannot make it post-claim proof.
            s.finish("route-account", &a, Outcome::Reset, Some(&after), NOW + 1, None)
        } else {
            s.read_finish("route-account", &a, Outcome::Reset, Some(&after), NOW + 1, None)
        }
        .unwrap();
        assert_eq!(r["outcome"], if mode == 0 { "claimed" } else { "claimed-unconfirmed" });
    }
}
#[test]
fn check_again_only_verifies() {
    let (_d, mut s) = open();
    let snap = snapshot("codex");
    let (_, a) = begin(&mut s, &snap);
    let r = s.read_check("route-account", &a, &snap, NOW).unwrap();
    assert_eq!(r["outcome"], "claimed-unconfirmed");
    assert_eq!(r["providerSuccess"], false);
}
#[test]
fn malformed_and_stale_inventory_are_not_zero() {
    let (_d, mut s) = open();
    let mut snap = snapshot("claude");
    snap.observed_at_ms = NOW - 30_001;
    assert_eq!(
        s.read_inventory("r", &snap, NOW, &entropy(1)).unwrap_err(),
        "stale-inventory"
    );
    snap.observed_at_ms = NOW;
    snap.grants[1].id = snap.grants[0].id.clone();
    assert!(s.read_inventory("r", &snap, NOW, &entropy(1)).is_err());
}
#[test]
fn cooldown_persists() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let (k, a) = begin(&mut s, &snap);
    assert_eq!(
        s.read_finish("route-account", &a, Outcome::Cooldown, None, NOW, Some(60_000))
            .unwrap()["outcome"],
        "cooldown"
    );
    let t = prepare(&mut s, &k, &snap);
    assert_eq!(
        s.begin("route-account", &k, &caller(), &t, &snap, NOW, &entropy(9))
            .unwrap_err(),
        "cooldown"
    );
}
#[test]
fn native_dispatch_never_echoes_provider_bodies() {
    assert!(aac_status::reset::dispatch(&json!({"providerBody":"synthetic-untrusted"}).to_string()).is_err());
}
#[test]
fn normalizers_preserve_ids_and_unknowns() {
    let cedar = json!({"eligible":true,"grants":[{"id":"exact_a","resets_left":1,"paused":false,"usable_now":true,"type":"future","clears":["seven_day"]}]});
    let g = aac_status::reset_inventory::claude(&cedar, &[None]).unwrap();
    assert_eq!(g[0].id.as_deref(), Some("exact_a"));
    assert_eq!(g[0].kind, "unknown");
    let details = json!({"credits":[{"id":"exact_b","status":"available","reset_type":"future"}]});
    let g = aac_status::reset_inventory::codex(&details, &[None], "", &[]).unwrap();
    assert_eq!(g[0].kind, "unknown");
    assert_eq!(g[0].words.as_deref(), Some("future"));
}

#[test]
fn unknown_kinds_keep_bounded_provider_words_and_known_kinds_have_none() {
    use aac_status::reset_inventory::{claude, provider_words};
    let cedar = json!({"eligible":true,"grants":[
        {"id":"g_typed","resets_left":1,"paused":false,"usable_now":true,"type":"opus\u{0} <b>boost</b>","clears":["seven_day"]},
        {"id":"g_window","resets_left":1,"paused":false,"usable_now":true,"clears":["seven_day_opus"]},
        {"id":"g_weekly","resets_left":1,"paused":false,"usable_now":true,"type":"weekly","clears":["seven_day"]}]});
    let g = claude(&cedar, &[None, None, None]).unwrap();
    assert_eq!(g[0].kind, "unknown");
    assert_eq!(g[0].words.as_deref(), Some("opus bboost/b"));
    assert_eq!(g[1].words.as_deref(), Some("seven_day_opus"));
    assert_eq!(g[2].kind, "weekly");
    assert_eq!(g[2].words, None);
    assert_eq!(provider_words(&"x".repeat(80)).unwrap().len(), 48);
    assert_eq!(provider_words("  \u{7}  "), None);
    // the public row carries the words for the unknown grant only, and never makes it claimable
    let (_d, mut s) = open();
    let mut snap = snapshot("claude");
    snap.grants[0].kind = "unknown".into();
    snap.grants[0].words = Some("seven_day_opus".into());
    snap.grants[1].words = Some("must-not-show".into());
    let out = s.read_inventory("route-account", &snap, NOW, &entropy(30)).unwrap();
    assert_eq!(out["grants"][0]["providerWords"], "seven_day_opus");
    assert_eq!(out["grants"][0]["claimable"], false);
    assert_eq!(out["grants"][0]["claimBlockedReason"], "unknown-grant-kind");
    assert!(out["grants"][1]["providerWords"].is_null());
    assert_eq!(out["grants"][1]["claimable"], true);
}

#[test]
fn a_confirmed_claim_reports_the_refilled_windows_from_the_verification_read() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let (_, a) = begin(&mut s, &snap);
    let mut after = snap.clone();
    after.observed_at_ms += 1;
    after.grants[1].left = 0;
    after.windows.insert("seven_day".into(), 12.5);
    let r = s
        .read_finish("route-account", &a, Outcome::Reset, Some(&after), NOW + 1, None)
        .unwrap();
    assert_eq!(r["outcome"], "claimed");
    assert_eq!(r["windows"], json!([{"id":"seven_day","usedPercent":12.5}]));
    // an unconfirmed result never reports windows
    let (_d, mut s) = open();
    let (_, a) = begin(&mut s, &snap);
    let r = s
        .read_finish("route-account", &a, Outcome::Reset, Some(&snap), NOW, None)
        .unwrap();
    assert_eq!(r["outcome"], "claimed-unconfirmed");
    assert_eq!(r["windows"], json!([]));
}

#[test]
fn the_pending_attempt_is_readable_for_check_again_and_clears_once_settled() {
    let (d, mut s) = open();
    let snap = snapshot("codex");
    assert!(s.pending("route-account", NOW).unwrap()["attemptKey"].is_null());
    let (_, a) = begin(&mut s, &snap);
    s.read_finish("route-account", &a, Outcome::Ambiguous, None, NOW, None)
        .unwrap();
    drop(s);
    let mut s = Store::open(&d.path().join("reset.sqlite")).unwrap();
    assert_eq!(s.pending("route-account", NOW).unwrap()["attemptKey"], a);
    assert!(s.pending("other-account", NOW).unwrap()["attemptKey"].is_null());
    let mut after = snap.clone();
    after.observed_at_ms += 1;
    after.grants[1].left = 0;
    after.windows.insert("seven_day".into(), 0.0);
    assert_eq!(
        s.read_check("route-account", &a, &after, NOW + 1).unwrap()["outcome"],
        "claimed"
    );
    assert!(s.pending("route-account", NOW).unwrap()["attemptKey"].is_null());
}

#[test]
fn known_rejection_does_not_become_provider_success() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let (_, a) = begin(&mut s, &snap);
    let mut after = snap.clone();
    after.observed_at_ms += 1;
    after.grants[1].left = 0;
    after.windows.insert("seven_day".into(), 0.0);
    let result = s
        .read_finish("route-account", &a, Outcome::Nothing, Some(&after), NOW + 1, None)
        .unwrap();
    assert_eq!(result["outcome"], "nothing-to-reset");
    assert_eq!(result["providerSuccess"], false);
}
#[test]
fn provider_success_is_retained_across_read_only_check() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let (_, a) = begin(&mut s, &snap);
    s.read_finish("route-account", &a, Outcome::Reset, None, NOW, None)
        .unwrap();
    let result = s.read_check("route-account", &a, &snap, NOW).unwrap();
    assert_eq!(result["providerSuccess"], true);
    assert_eq!(result["outcome"], "claimed-unconfirmed");
}

#[test]
fn simultaneous_confirmations_produce_only_one_write_permission() {
    let (d, mut s) = open();
    let snap = snapshot("claude");
    let k = inventory(&mut s, &snap);
    let t1 = prepare(&mut s, &k, &snap);
    assert_eq!(
        s.prepare("route-account", &k, &caller(), &snap, NOW, &entropy(6))
            .unwrap_err(),
        "claim-token-outstanding"
    );
    // Racing replays of the originating token still yield exactly one Write.
    let t2 = t1.clone();
    drop(s);
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let mut workers = vec![];
    for (token, n) in [(t1, 7), (t2, 8)] {
        let db = d.path().join("reset.sqlite");
        let barrier = barrier.clone();
        let k = k.clone();
        let snap = snap.clone();
        workers.push(std::thread::spawn(move || {
            let mut s = Store::open(&db).unwrap();
            barrier.wait();
            s.begin("route-account", &k, &caller(), &token, &snap, NOW, &entropy(n))
                .is_ok()
        }));
    }
    assert_eq!(
        workers
            .into_iter()
            .map(|w| usize::from(w.join().unwrap()))
            .sum::<usize>(),
        1
    );
}
#[test]
fn send_limit_is_durable_and_audit_cannot_be_pruned() {
    let (d, mut s) = open();
    let snap = snapshot("claude");
    let k = inventory(&mut s, &snap);
    for i in 0..4 {
        let token = s
            .prepare("route-account", &k, &caller(), &snap, NOW, &entropy(30 + i))
            .unwrap()["confirmationToken"]
            .as_str()
            .unwrap()
            .to_owned();
        let attempt = entropy(40 + i);
        let result = s.begin("route-account", &k, &caller(), &token, &snap, NOW, &attempt);
        if i == 3 {
            assert_eq!(result.unwrap_err(), "rate-limited");
        } else {
            result.unwrap();
            s.read_finish("route-account", &attempt, Outcome::Nothing, None, NOW, None)
                .unwrap();
        }
    }
    drop(s);
    let db = rusqlite::Connection::open(d.path().join("reset.sqlite")).unwrap();
    assert!(db.execute("DELETE FROM history", []).is_err());
    assert!(db.execute("UPDATE history SET outcome='changed'", []).is_err());
}

#[test]
fn full_reset_can_confirm_when_an_unused_window_stays_zero() {
    let (_d, mut s) = open();
    let mut snap = snapshot("claude");
    snap.windows.insert("five_hour".into(), 0.0);
    snap.grants[1].clears.push("five_hour".into());
    let (_, a) = begin(&mut s, &snap);
    let mut after = snap.clone();
    after.observed_at_ms += 1;
    after.grants[1].left = 0;
    after.windows.insert("seven_day".into(), 0.0);
    assert_eq!(
        s.read_finish("route-account", &a, Outcome::Reset, Some(&after), NOW + 1, None)
            .unwrap()["outcome"],
        "claimed"
    );
}

#[cfg(unix)]
#[test]
fn storage_refuses_a_shared_parent_directory() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Store::open(&dir.path().join("reset.sqlite")).is_err());
}

#[test]
fn preflight_rejects_holds_and_pending_before_source_io() {
    let (_d, mut s) = open();
    let snap = snapshot("claude");
    let (key, a) = begin(&mut s, &snap);
    assert_eq!(
        s.gate("route-account", &key, NOW, true).unwrap_err(),
        "check-again-required"
    );
    s.read_finish("route-account", &a, Outcome::Cooldown, None, NOW, Some(60_000))
        .unwrap();
    assert_eq!(s.gate("route-account", &key, NOW, false).unwrap_err(), "cooldown");
    assert!(s.gate("route-account", &key, NOW + 60_001, true).is_ok());
}
