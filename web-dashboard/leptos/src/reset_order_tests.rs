use super::*;
use reset_state::AccountState;

fn fixture() -> serde_json::Value {
    serde_json::from_str(include_str!(
        "../tests/fixtures/reset-ordered-attempts.json"
    ))
    .unwrap()
}
fn inventory(name: &str) -> Inventory {
    serde_json::from_value(fixture()[name].clone()).unwrap()
}
fn result(name: &str) -> ClaimResult {
    serde_json::from_value(fixture()[name].clone()).unwrap()
}
fn evidence(name: &str) -> Evidence {
    serde_json::from_value(fixture()[name]["evidence"].clone()).unwrap()
}
fn canonical() -> String {
    evidence("inventory3").canonical_account_key
}
fn enabled(rc: ResetCtx) -> bool {
    use_enabled(true, &rc.unresolved.get_untracked(), &Dialog::Closed)
}
fn pending(rc: ResetCtx, key: &str) {
    assert!(!enabled(rc));
    assert_eq!(rc.unresolved.get_untracked().key.as_deref(), Some(key));
    let said = rc.said.get_untracked().unwrap();
    assert!(said.check && said.bad);
    assert!(!said.text.starts_with("Reset applied:"));
}

// SOL-MAX-2 M3: both transport orders, including a positively named newer attempt from an earlier-started GET.
#[test]
fn older_started_read_must_not_hide_a_different_pending_attempt() {
    for old_first in [true, false] {
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show("route-a", "codex", false);
            if old_first {
                rc.receive(&canonical(), &result("settled1"), None);
            }
            rc.take("route-a", inventory("inventory2"), 200.);
            if !old_first {
                rc.receive(&canonical(), &result("settled1"), None);
            }
            pending(rc, &result("settled2").attempt_key);
        });
    }
}
#[test]
fn late_old_check_must_not_settle_the_new_attempt_lock() {
    let mut state = AccountState::default();
    state.observe(&evidence("inventory2"), Some(200.));
    assert!(!state.receipt(&result("settled1"), None, None));
    assert!(state.lock().blocks());
    assert_eq!(state.lock().key, inventory("inventory2").pending_attempt);
}

// RECHECK-2 R1: a stale intermediate identity and its legitimate settlement cannot replace claim 3.
#[test]
fn stale_intermediate_pending_must_not_allow_settlement_to_clear_third_attempt() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("route-a", "codex", false);
        rc.take("route-a", inventory("inventory1"), 10.);
        rc.receive(&canonical(), &result("settled1"), None);
        rc.take("route-a", inventory("inventory3"), 100.);
        rc.take("route-a", inventory("inventory2"), 50.);
        rc.receive(&canonical(), &result("settled2"), None);
        pending(rc, &result("unconfirmed3").attempt_key);
        // A fresh replay of claim 2 also carries the complete current server snapshot for claim 3.
        assert!(rc.receive(&canonical(), &result("replay2"), None));
        pending(rc, &result("unconfirmed3").attempt_key);
    });
}
#[test]
fn stale_named_pending_and_its_check_must_not_shorten_unknown_send_proof_interval() {
    let mut state = AccountState::default();
    state.receipt(&result("settled1"), None, None);
    state.begin_send(1, 120100.);
    state.observe(&evidence("inventory2"), Some(50.));
    state.receipt(&result("settled2"), None, None);
    assert!(state.lock().blocks());
    // Neither a stale nor a current empty proof before the send deadline suffices.
    assert!(!state.observe(&evidence("empty"), Some(121.)));
    assert!(!state.observe(&evidence("empty"), Some(120101.)));
    assert!(state.lock().blocks());
    state.observe(&evidence("finalInventory"), Some(120099.));
    assert!(state.lock().blocks());
    state.observe(&evidence("finalInventory"), Some(120100.));
    assert!(!state.lock().blocks());
}
#[test]
fn three_successive_observed_settlements_and_old_empty_reads_are_safe() {
    let mut state = AccountState::default();
    for (inv, settled) in [
        ("inventory1", "settled1"),
        ("inventory2", "settled2"),
        ("inventory3", "settled3"),
    ] {
        state.observe(&evidence(inv), Some(100.));
        assert!(state.lock().blocks());
        assert!(!state.observe(&evidence("empty"), Some(1_000_000.)));
        assert!(state.lock().blocks());
        state.receipt(&result(settled), None, None);
        assert!(!state.lock().blocks());
    }
    for inv in ["inventory1", "inventory2", "inventory3"] {
        state.observe(&evidence(inv), Some(1_000_000.));
        assert!(!state.lock().blocks());
    }
}
#[test]
fn delayed_check_after_navigation_must_not_replace_newer_pending_result_with_confirmed() {
    for old in ["settled1", "settled2", "replay2"] {
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show("route-a", "codex", false);
            rc.take("route-a", inventory("inventory3"), 100.);
            rc.bind_alias("sibling", &canonical(), 100);
            let newer = rc.said.get_untracked();
            rc.show("other", "codex", false);
            rc.receive(&canonical(), &result(old), None);
            rc.show("sibling", "codex", false);
            assert_eq!(rc.said.get_untracked(), newer);
            pending(rc, &result("unconfirmed3").attempt_key);
            assert!(
                rc.older
                    .get_untracked()
                    .iter()
                    .any(|line| line.starts_with("Older claim:"))
            );
            assert!(
                rc.older
                    .get_untracked()
                    .iter()
                    .all(|line| !line.contains("0%"))
            );
        });
    }
}
#[test]
fn production_three_attempt_response_sequence_must_keep_latest_pending_blocked() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("a", "codex", false);
        rc.take("a", inventory("inventory3"), 100.);
        rc.take("a", inventory("inventory2"), 50.);
        rc.receive(&canonical(), &result("settled2"), None);
        let rows = rows_from_inventory(&inventory("inventory3"), "codex", 1_700_000_000_004.);
        assert_eq!(rows.iter().filter(|r| r.claimable).count(), 1);
        assert_eq!(
            rows.iter()
                .filter(|r| use_enabled(
                    r.claimable,
                    &rc.unresolved.get_untracked(),
                    &Dialog::Closed
                ))
                .count(),
            0
        );
    });
}
#[test]
fn claim_receipt_only_settles_its_own_attempt_across_aliases() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("a", "codex", false);
        rc.take("a", inventory("inventory3"), 100.);
        rc.bind_alias("b", &canonical(), 100);
        rc.show("b", "codex", false);
        let id = rc.begin_send(&canonical(), 100.);
        assert!(!rc.receive(&canonical(), &result("settled2"), Some(id)));
        assert!(rc.receive(&canonical(), &result("replay2"), None));
        assert!(rc.lock_of("a").blocks());
        assert!(
            rc.said
                .get_untracked()
                .unwrap()
                .text
                .contains("no confirmed answer")
        );
        // Only the originating request's accepted receipt can acknowledge its own send.
        assert!(rc.receive(&canonical(), &result("settled3"), Some(id)));
        assert!(enabled(rc));
    });
}

// M2 and U1: local sends exist before the response and survive alias discovery, navigation and old empty reads.
#[test]
fn an_ambiguous_claim_answer_blocks_every_use_reset_until_it_is_settled() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("a", "codex", false);
        rc.take("a", inventory("empty"), 1.);
        assert!(enabled(rc));
        let id = rc.begin_send(&canonical(), 100.);
        assert!(!enabled(rc));
        rc.take("a", inventory("empty"), 101.);
        assert!(!enabled(rc));
        rc.take("a", inventory("inventory2"), 200.);
        rc.receive(&canonical(), &result("settled2"), None);
        assert!(!enabled(rc));
        rc.take("a", inventory("finalInventory"), 120099.);
        assert!(!enabled(rc));
        rc.take("a", inventory("finalInventory"), 120100.);
        assert!(enabled(rc));
        // A different, still unanswered send is not retired by another send's acknowledgement.
        let second = rc.begin_send(&canonical(), 120101.);
        rc.receive(&canonical(), &result("settled3"), Some(id));
        assert!(!enabled(rc));
        rc.update_state(&canonical(), |s| s.not_sent(second));
        assert!(enabled(rc));
    });
}
#[test]
fn canonical_aliases_share_the_spend_lock_and_pending_recovery() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("a", "codex", false);
        rc.take("a", inventory("empty"), 1.);
        rc.show("b", "codex", false);
        rc.take("b", inventory("empty"), 2.);
        rc.begin_send(&canonical(), 100.);
        assert!(!enabled(rc));
        assert!(rc.lock_of("a").blocks());
        rc.take("b", inventory("inventory3"), 101.);
        pending(rc, &result("unconfirmed3").attempt_key);
        rc.show("a", "codex", false);
        assert!(!enabled(rc));
    });
}
#[test]
fn a_late_alias_read_preserves_a_newer_unknown_send_latch() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("a", "codex", false);
        rc.take("a", inventory("empty"), 1.);
        rc.show("b", "codex", false);
        rc.begin_send(&canonical(), 100.);
        rc.take("b", inventory("empty"), 10.);
        assert!(!enabled(rc));
        rc.take("b", inventory("inventory3"), 120101.);
        pending(rc, &result("unconfirmed3").attempt_key);
    });
}

fn repoint() -> Vec<Inventory> {
    serde_json::from_str(include_str!("../tests/fixtures/reset-ordered-repoint.json")).unwrap()
}
// ASTRA-MAX-2 M4 and RECHECK-2 control: both directions, unchanged siblings, delayed receipt, X -> Y -> X.
#[test]
fn reassignment_in_both_directions_keeps_results_and_locks_with_canonical_accounts() {
    for reverse in [false, true] {
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            let all = repoint();
            let mut x = all[0].clone();
            let mut y = all[1].clone();
            if reverse {
                let key = x.canonical_account_key.clone();
                x.canonical_account_key = y.canonical_account_key.clone();
                y.canonical_account_key = key;
                x.evidence.as_mut().unwrap().canonical_account_key =
                    x.canonical_account_key.clone().unwrap();
                y.evidence.as_mut().unwrap().canonical_account_key =
                    y.canonical_account_key.clone().unwrap();
            }
            let original = x.canonical_account_key.as_ref().unwrap().clone();
            let replacement = y.canonical_account_key.as_ref().unwrap().clone();
            rc.show("reused", "codex", false);
            rc.take("reused", x.clone(), 10.);
            rc.bind_alias("old-sibling", &original, 10);
            rc.bind_alias("new-sibling", &replacement, 10);
            let result_x = rc.said_of("old-sibling");
            assert!(result_x.as_ref().unwrap().check);
            rc.take("reused", y.clone(), 20.);
            assert!(enabled(rc));
            assert!(
                rc.said
                    .get_untracked()
                    .unwrap()
                    .text
                    .contains(ACCOUNT_CHANGED)
            );
            assert_eq!(rc.said_of("old-sibling"), result_x);
            assert!(rc.lock_of("old-sibling").blocks());
            rc.take("new-sibling", y, 21.);
            assert!(rc.lock_of("old-sibling").blocks());
            let mut settled: ClaimResult = serde_json::from_str(include_str!(
                "../tests/fixtures/reset-ordered-repoint-settlement.json"
            ))
            .unwrap();
            if reverse {
                settled.evidence.as_mut().unwrap().canonical_account_key = original.clone();
            }
            let shown = rc.said.get_untracked();
            assert!(rc.receive(&original, &settled, None));
            assert_eq!(rc.said.get_untracked(), shown);
            assert!(!rc.lock_of("old-sibling").blocks());
            assert!(!rc.said_of("old-sibling").unwrap().check);
            // Delayed original binding cannot redirect the current view back to X.
            rc.take("reused", x.clone(), 5.);
            assert_eq!(rc.lock_key("reused"), replacement);
            x.binding_revision = Some("1000".into());
            x.evidence = settled.evidence.clone();
            x.pending_attempt = None;
            rc.take("reused", x, 30.);
            assert_eq!(rc.lock_key("reused"), original);
            assert!(
                rc.said
                    .get_untracked()
                    .unwrap()
                    .text
                    .contains(ACCOUNT_CHANGED)
            );
            rc.show("old-sibling", "codex", false);
            assert_eq!(
                rc.said.get_untracked(),
                Some(result_said(&settled, "codex"))
            );
        });
    }
}
#[test]
fn profile_repoint_does_not_falsely_say_the_old_pending_claim_settled() {
    Owner::new().with(|| {
        let all = repoint();
        let rc = ResetCtx::new();
        rc.show("reused", "codex", false);
        rc.take("reused", all[0].clone(), 10.);
        rc.take("reused", all[1].clone(), 20.);
        assert!(
            rc.said
                .get_untracked()
                .unwrap()
                .text
                .contains(ACCOUNT_CHANGED)
        );
        rc.show("x-sibling", "codex", false);
        rc.take("x-sibling", all[2].clone(), 21.);
        assert!(!enabled(rc));
        assert!(rc.said.get_untracked().unwrap().check);
    });
}
#[test]
fn x_y_x_reassignment_retains_both_unresolved_latches_and_results() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        let all = repoint();
        let x = all[0].clone();
        let mut y = all[0].clone();
        let ykey = all[1].canonical_account_key.clone().unwrap();
        y.canonical_account_key = Some(ykey.clone());
        y.evidence.as_mut().unwrap().canonical_account_key = ykey.clone();
        y.binding_revision = Some("100".into());
        rc.show("reused", "codex", false);
        rc.take("reused", x.clone(), 10.);
        let xkey = rc.lock_key("reused");
        rc.bind_alias("x-sibling", &xkey, 10);
        rc.take("reused", y, 20.);
        rc.bind_alias("y-sibling", &ykey, 100);
        let mut back = x;
        back.binding_revision = Some("101".into());
        rc.take("reused", back, 30.);
        assert!(rc.lock_of("x-sibling").blocks() && rc.lock_of("y-sibling").blocks());
        assert!(rc.said_of("x-sibling").unwrap().check && rc.said_of("y-sibling").unwrap().check);
    });
}
#[test]
fn missing_or_incoherent_metadata_cannot_enable_use_reset() {
    let mut state = AccountState::default();
    assert!(state.lock().blocks());
    let mut e = evidence("inventory3");
    e.pending_attempts.clear();
    assert!(!state.observe(&e, Some(1.)));
    e = evidence("inventory3");
    e.revision = "9007199254740993".into();
    assert!(state.observe(&e, None));
    let mut older = evidence("settled3");
    older.revision = "9007199254740992".into();
    assert!(!state.observe(&older, None));
    assert!(state.lock().blocks());
}

// Every delivery order of eight real Store responses; the oracle follows server revisions, never request time.
#[test]
fn every_permutation_of_recorded_responses_keeps_lock_result_and_unknown_send_consistent() {
    let names = [
        "inventory1",
        "settled1",
        "inventory2",
        "settled2",
        "unconfirmed3",
        "inventory3",
        "replay2",
        "settled3",
    ];
    let recorded: Vec<_> = names
        .iter()
        .map(|name| {
            (
                evidence(name),
                if fixture()[name]["attemptKey"].is_string() {
                    Some(result(name))
                } else {
                    None
                },
            )
        })
        .collect();
    fn visit(
        order: &mut [usize],
        at: usize,
        recorded: &[(Evidence, Option<ClaimResult>)],
        count: &mut usize,
    ) {
        if at < order.len() {
            for i in at..order.len() {
                order.swap(at, i);
                visit(order, at + 1, recorded, count);
                order.swap(at, i);
            }
            return;
        }
        *count += 1;
        for unknown in [false, true] {
            let mut state = AccountState::default();
            if unknown {
                state.begin_send(1, 120100.);
            }
            let mut newest: Option<&Evidence> = None;
            for index in order.iter().copied() {
                let (e, r) = &recorded[index];
                if newest.is_none_or(|n| {
                    e.revision.parse::<u64>().unwrap() > n.revision.parse::<u64>().unwrap()
                }) {
                    newest = Some(e);
                }
                if let Some(r) = r {
                    state.receipt(r, None, None);
                } else {
                    state.observe(e, Some(50.));
                }
                let expected = newest.unwrap();
                let latest = expected.latest_attempt.as_ref().unwrap();
                assert_eq!(
                    state.lock().blocks(),
                    unknown || latest.pending,
                    "delivery {order:?}, prefix ends {index}"
                );
                assert_eq!(
                    state.lock().key.as_deref(),
                    if latest.pending {
                        Some(latest.attempt_key.as_str())
                    } else {
                        None
                    }
                );
                if unknown {
                    assert!(
                        state
                            .current("codex")
                            .unwrap()
                            .text
                            .contains("no confirmed answer")
                    );
                } else {
                    assert_eq!(state.current("codex"), Some(result_said(latest, "codex")));
                }
            }
        }
    }
    let mut count = 0;
    visit(&mut [0, 1, 2, 3, 4, 5, 6, 7], 0, &recorded, &mut count);
    assert_eq!(count, 40320);
}

#[test]
fn distinct_grant_witness_keeps_new_claim_pending_in_both_delivery_orders() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/reset-ordered-grants.json")).unwrap();
    let inv: Inventory = serde_json::from_value(fixture["newInventory"].clone()).unwrap();
    let old: ClaimResult = serde_json::from_value(fixture["oldResult"].clone()).unwrap();
    for old_first in [false, true] {
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show("route-a", "claude", false);
            let key = inv.canonical_account_key.as_ref().unwrap();
            if old_first {
                rc.receive(key, &old, None);
            }
            rc.take("route-a", inv.clone(), 200.);
            if !old_first {
                rc.receive(key, &old, None);
            }
            pending(rc, fixture["secondKey"].as_str().unwrap());
            let rows = rows_from_inventory(&inv, "claude", 1_700_000_000_004.);
            assert_eq!(rows.iter().filter(|r| r.claimable).count(), 1);
            assert_eq!(
                rows.iter()
                    .filter(|r| use_enabled(
                        r.claimable,
                        &rc.unresolved.get_untracked(),
                        &Dialog::Closed
                    ))
                    .count(),
                0
            );
        });
    }
}

#[test]
fn first_alias_resolution_preserves_a_route_local_unanswered_send() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("unknown-route", "codex", false);
        rc.begin_send("unknown-route", 100.);
        rc.take("unknown-route", inventory("empty"), 50.);
        rc.bind_alias("sibling", &canonical(), 100);
        rc.show("sibling", "codex", false);
        assert!(!enabled(rc));
        assert!(
            rc.said
                .get_untracked()
                .unwrap()
                .text
                .contains("no confirmed answer")
        );
    });
}

#[test]
fn cache_reuse_requires_the_accounts_current_server_snapshot() {
    let mut state = AccountState::default();
    let cached = evidence("empty");
    state.observe(&cached, None);
    assert!(state.matches(&cached));
    state.observe(&evidence("finalInventory"), None);
    assert!(!state.lock().blocks());
    assert!(!state.matches(&cached));
    assert!(state.matches(&evidence("finalInventory")));
}

#[test]
fn recovery_required_without_evidence_needs_a_read_started_after_the_refusal() {
    let mut state = AccountState::default();
    state.observe(&evidence("empty"), Some(1.));
    state.unknown(100.);
    state.observe(&evidence("empty"), Some(50.));
    assert!(state.lock().blocks());
    state.receipt(&result("settled3"), None, None);
    assert!(
        state.lock().blocks(),
        "a check cannot acknowledge a local recovery obligation"
    );
    state.observe(&evidence("finalInventory"), Some(99.));
    assert!(state.lock().blocks());
    state.observe(&evidence("finalInventory"), Some(100.));
    assert!(!state.lock().blocks());
}
#[test]
fn ordered_refusals_cannot_replace_newer_pending_or_acknowledge_another_send() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("a", "codex", false);
        rc.take("a", inventory("inventory3"), 100.);
        let id = rc.begin_send(&canonical(), 100.);
        let mut refusal = Refusal {
            code: "reset-state-unavailable".into(),
            reason: Some("check-again-required".into()),
            sent: Some(false),
            evidence: Some(Box::new(evidence("empty"))),
        };
        rc.refused(&canonical(), &refusal, Some(id), 110.);
        assert!(
            rc.said
                .get_untracked()
                .unwrap()
                .text
                .contains("no confirmed answer")
        );
        assert_eq!(rc.lock_of("a").key, inventory("inventory3").pending_attempt);
        refusal.evidence = Some(Box::new(evidence("inventory3")));
        rc.refused(&canonical(), &refusal, None, 111.);
        assert!(
            rc.said
                .get_untracked()
                .unwrap()
                .text
                .contains("no confirmed answer")
        );
        rc.refused(&canonical(), &refusal, Some(id), 112.);
        pending(rc, &result("unconfirmed3").attempt_key);
    });
}
#[test]
fn a_refusal_without_an_explicit_no_send_proof_preserves_the_full_unknown_interval() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("a", "codex", false);
        rc.take("a", inventory("empty"), 1.);
        let id = rc.begin_send(&canonical(), 100.);
        let refusal: Refusal = serde_json::from_str("{}").unwrap();
        rc.refused(&canonical(), &refusal, Some(id), 110.);
        rc.take("a", inventory("finalInventory"), 111.);
        assert!(!enabled(rc));
        rc.take("a", inventory("finalInventory"), 120100.);
        assert!(enabled(rc));
    });
}
