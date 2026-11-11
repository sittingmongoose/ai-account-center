use super::*;

fn sequence() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../tests/fixtures/reset-successive-attempts.json"
    ))
    .unwrap()
}

fn result(v: &serde_json::Value) -> Answer<ClaimResult> {
    Answer::Ok(serde_json::from_value(v["oldResult"].clone()).unwrap())
}

fn inventory(v: &serde_json::Value) -> Inventory {
    serde_json::from_value(v["newInventory"].clone()).unwrap()
}

fn aliases(rc: ResetCtx, v: &serde_json::Value) {
    let canonical = v["newInventory"]["canonicalAccountKey"].as_str().unwrap();
    rc.show("route-a", "claude", false);
    rc.bind_alias("route-a", canonical);
    rc.bind_alias("route-b", canonical);
}

fn assert_pending_blocks(rc: ResetCtx, v: &serde_json::Value) {
    let pending = rc.unresolved.get_untracked();
    assert_eq!(pending.key.as_deref(), v["secondKey"].as_str());
    assert!(pending.blocks());
    assert!(rc.lock_of("route-a").blocks());
    assert!(rc.lock_of("route-b").blocks());
    let inv = inventory(v);
    let rows = rows_from_inventory(&inv, "claude", 1_700_000_000_004.);
    assert_eq!(history_rows(&inv.history)[0].result, "Unconfirmed");
    assert_eq!(rows.iter().filter(|r| r.claimable).count(), 1);
    assert_eq!(
        rows.iter()
            .filter(|r| use_enabled(r.claimable, &pending, &Dialog::Closed))
            .count(),
        0
    );
}

#[test]
fn older_started_read_must_not_hide_a_different_pending_attempt() {
    Owner::new().with(|| {
        let v = sequence();
        let rc = ResetCtx::new();
        aliases(rc, &v);
        // The old check arrives at 300. A GET started at 200 names a new attempt at 400.
        rc.update_lock("route-a", |u| u.apply(check_lock(&result(&v), 300.)));
        rc.show("route-b", "claude", false);
        rc.take("route-b", inventory(&v), 200.);
        assert_pending_blocks(rc, &v);
    });
}

#[test]
fn late_old_check_must_not_settle_the_new_attempt_lock() {
    Owner::new().with(|| {
        let v = sequence();
        let rc = ResetCtx::new();
        aliases(rc, &v);
        rc.show("route-b", "claude", false);
        rc.take("route-b", inventory(&v), 200.);
        assert_pending_blocks(rc, &v);
        // The delayed receipt names only the old attempt, through a canonical sibling alias.
        rc.update_lock("route-a", |u| u.apply(check_lock(&result(&v), 300.)));
        assert_pending_blocks(rc, &v);
    });
}

#[test]
fn settled_attempts_cannot_revive_or_clear_a_newer_unknown_send() {
    let v = sequence();
    let old = result(&v);
    let mut pending = Unresolved::default();
    pending.apply(check_lock(&old, 300.));
    pending.apply(claim_lock(&Answer::Lost, 400., 450.));
    pending.apply(check_lock(&old, 500.));
    assert!(pending.blocks());
    assert!(!pending.read(None, 501.));
    assert!(pending.read(None, 400. + CONFIRM_LIFE_MS));
    pending.read(
        Some(v["firstKey"].as_str().unwrap().into()),
        600. + CONFIRM_LIFE_MS,
    );
    assert!(!pending.blocks());
}

#[test]
fn claim_receipt_only_settles_its_own_attempt_across_aliases() {
    Owner::new().with(|| {
        let v = sequence();
        let rc = ResetCtx::new();
        aliases(rc, &v);
        rc.take("route-a", inventory(&v), 200.);
        rc.update_lock("route-b", |u| u.apply(claim_lock(&result(&v), 100., 300.)));
        assert_pending_blocks(rc, &v);
    });
}
