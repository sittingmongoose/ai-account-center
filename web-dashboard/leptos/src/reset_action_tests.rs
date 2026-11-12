use super::*;

const ROUTE: &str = "codex:fixture";
fn trace(prior: bool) -> serde_json::Value {
    serde_json::from_str(if prior {
        include_str!("../tests/fixtures/reset-refusal-after-success.json")
    } else {
        include_str!("../tests/fixtures/reset-refusal-first-action.json")
    })
    .unwrap()
}
fn inventory(t: &serde_json::Value, name: &str) -> Inventory {
    serde_json::from_value(t[name].clone()).unwrap()
}
fn refusal(t: &serde_json::Value) -> Refusal {
    serde_json::from_value(t["refused"].clone()).unwrap()
}
fn start(t: &serde_json::Value) -> (ResetCtx, String) {
    let rc = ResetCtx::new();
    rc.show(ROUTE, "codex", false);
    let before = inventory(t, "before");
    let canonical = before.canonical_account_key.clone().unwrap();
    rc.take(ROUTE, before, 100.);
    (rc, canonical)
}
fn no_old_success(rc: ResetCtx) -> String {
    let said = rc.said.get_untracked().unwrap();
    assert!(!said.text.contains("Reset applied"), "{}", said.text);
    said.text
}
fn answer_refusal(rc: ResetCtx, canonical: &str, id: u64, r: Refusal) {
    // The same completion function is used by confirm(), not a parallel test projection.
    rc.claim_answer(canonical, id, Answer::Refused(r), 103.);
}

// Max-6 M1: the real router/native quota-drift witness, with and without a hint or earlier claim.
#[test]
fn real_no_send_refusal_survives_refresh_hints_navigation_and_keeps_success_only_in_history() {
    for prior in [false, true] {
        for hint in [false, true] {
            let t = trace(prior);
            Owner::new().with(|| {
                let (rc, canonical) = start(&t);
                if prior {
                    assert!(rc.said.get_untracked().is_none());
                }
                rc.take(ROUTE, inventory(&t, "withToken"), 101.);
                let p: Prepared = serde_json::from_value(t["prepared"].clone()).unwrap();
                assert!(
                    rc.unresolved
                        .get_untracked()
                        .can_confirm(&p.confirmation_key)
                );
                let id = rc.begin_send(&canonical, 102.);
                if hint {
                    rc.invalidate();
                }
                answer_refusal(rc, &canonical, id, refusal(&t));
                rc.dialog.set(Dialog::Closed);
                rc.take(ROUTE, inventory(&t, "recovered"), 104.);
                let shown = no_old_success(rc);
                assert!(shown.starts_with("Not used:") && shown.contains("Nothing was sent"));
                assert!(!rc.unresolved.get_untracked().blocks());
                assert!(
                    rc.older
                        .get_untracked()
                        .iter()
                        .all(|s| s.starts_with("Older claim:") && !s.contains("0%"))
                );
                assert_eq!(!rc.older.get_untracked().is_empty(), prior);
                assert_eq!(
                    history_rows(&inventory(&t, "recovered").history).len(),
                    usize::from(prior)
                );
                rc.show("other", "codex", false);
                rc.show(ROUTE, "codex", false);
                assert_eq!(no_old_success(rc), shown);
                assert_eq!(t["actualWindows"]["primary"], 100.);
                assert_eq!(t["actualWindows"]["secondary"], 21.);
                assert_eq!(t["providerSends"], 0);
            });
        }
    }
}

#[test]
fn every_pre_send_refusal_is_current_after_refresh_with_or_without_an_earlier_attempt() {
    // Expiry and a consumed token deliberately share the native confirmation-invalid code.
    let cases = [
        ("quota drift", "grant-changed", "grant changed"),
        ("expired token", "confirmation-invalid", "expired"),
        ("consumed token", "confirmation-invalid", "already used"),
        (
            "prepare exclusion",
            "claim-token-outstanding",
            "another page",
        ),
        ("pending exclusion", "check-again-required", "unconfirmed"),
        (
            "unavailable",
            "credential-host-unavailable",
            "saved profile",
        ),
        ("grant unavailable", "grant-unavailable", "can't be used"),
        ("identity changed", "identity-changed", "which account"),
        ("stale read", "stale-inventory", "too old"),
        ("cooldown", "cooldown", "wait"),
        ("rate limited", "rate-limited", "Too many"),
        ("signed out", "signed-out", "signed out"),
    ];
    for prior in [false, true] {
        for (label, reason, words) in cases {
            let t = trace(prior);
            Owner::new().with(|| {
                let (rc, canonical) = start(&t);
                let id = rc.begin_send(&canonical, 102.);
                let mut r = refusal(&t);
                r.reason = Some(reason.into());
                answer_refusal(rc, &canonical, id, r);
                rc.take(ROUTE, inventory(&t, "recovered"), 104.);
                let shown = no_old_success(rc);
                assert!(shown.contains(words), "{label}: {shown}");
                assert!(shown.contains("Nothing was sent"), "{label}: {shown}");
                assert!(!rc.unresolved.get_untracked().blocks());
            });
        }
    }
}

#[test]
fn prepare_exclusion_and_unavailable_answers_are_account_results_not_view_notices() {
    let t = trace(true);
    for unavailable in [false, true] {
        Owner::new().with(|| {
            let (rc, canonical) = start(&t);
            let id = rc.begin_action(&canonical);
            if unavailable {
                rc.action_not_sent(
                    &canonical,
                    id,
                    "Not used: usage is unavailable. Nothing was sent; check again.".into(),
                );
            } else {
                let mut r = refusal(&t);
                r.reason = Some("claim-token-outstanding".into());
                r.evidence = inventory(&t, "withToken").evidence;
                rc.refused(&canonical, &r, Some(id), 103.);
            }
            rc.take(ROUTE, inventory(&t, "recovered"), 104.);
            let shown = no_old_success(rc);
            assert!(shown.contains("Nothing was sent"));
            rc.show("other", "codex", false);
            rc.show(ROUTE, "codex", false);
            assert_eq!(no_old_success(rc), shown);
        });
    }
}

#[test]
fn refusal_then_successful_retry_supersedes_the_action_and_late_answers_cannot_replace_it() {
    for prior in [false, true] {
        for late_first in [false, true] {
            let t = trace(prior);
            Owner::new().with(|| {
                let (rc, canonical) = start(&t);
                let old = rc.begin_send(&canonical, 102.);
                answer_refusal(rc, &canonical, old, refusal(&t));
                rc.take(ROUTE, inventory(&t, "recovered"), 104.);
                no_old_success(rc);
                let new = rc.begin_send(&canonical, 105.);
                rc.take(ROUTE, inventory(&t, "duringRetry"), 106.);
                let result: ClaimResult = serde_json::from_value(t["retry"].clone()).unwrap();
                assert_eq!(
                    rc.unresolved.get_untracked().key.as_deref(),
                    Some(result.attempt_key.as_str())
                );
                if late_first {
                    answer_refusal(rc, &canonical, old, refusal(&t));
                    assert!(rc.unresolved.get_untracked().blocks());
                    no_old_success(rc);
                }
                rc.claim_answer(&canonical, new, Answer::Ok(result.clone()), 107.);
                rc.take(ROUTE, inventory(&t, "afterRetry"), 108.);
                let shown = rc.said.get_untracked().unwrap();
                assert!(shown.text.starts_with("Reset applied"));
                if !late_first {
                    answer_refusal(rc, &canonical, old, refusal(&t));
                }
                rc.claim_answer(
                    &canonical,
                    old,
                    Answer::Failed("Delayed old transport error".into()),
                    109.,
                );
                assert_eq!(rc.said.get_untracked(), Some(shown));
                assert!(!rc.unresolved.get_untracked().blocks());
                assert_eq!(!rc.older.get_untracked().is_empty(), prior);
                assert_eq!(t["syntheticRetrySends"], 1);
            });
        }
    }
}

#[test]
fn a_refusal_after_a_newer_inventory_still_supersedes_the_same_old_attempts_success() {
    let t = trace(true);
    Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        let id = rc.begin_send(&canonical, 102.);
        // Successful proof after the full local send interval arrives before the refusal callback.
        rc.take(ROUTE, inventory(&t, "recovered"), 120103.);
        no_old_success(rc);
        answer_refusal(rc, &canonical, id, refusal(&t));
        assert!(no_old_success(rc).contains("grant changed"));
        rc.take(ROUTE, inventory(&t, "recovered"), 120104.);
        assert!(no_old_success(rc).contains("grant changed"));
        assert!(!rc.unresolved.get_untracked().blocks());
    });
}

#[test]
fn an_unrelated_newer_attempt_and_its_settlement_supersede_a_delayed_refusal() {
    let t = trace(true);
    for settled in [false, true] {
        Owner::new().with(|| {
            let (rc, canonical) = start(&t);
            let old = rc.begin_send(&canonical, 102.);
            rc.take(
                ROUTE,
                inventory(&t, if settled { "afterRetry" } else { "duringRetry" }),
                120103.,
            );
            let before = rc.said.get_untracked();
            let lock = rc.unresolved.get_untracked();
            answer_refusal(rc, &canonical, old, refusal(&t));
            assert_eq!(rc.said.get_untracked(), before);
            assert_eq!(rc.unresolved.get_untracked(), lock);
            if settled {
                assert!(before.is_none());
            } else {
                no_old_success(rc);
            }
        });
    }
}

#[test]
fn a_refusal_arriving_after_navigation_stays_with_its_canonical_account_and_sibling() {
    let t = trace(true);
    Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        let id = rc.begin_send(&canonical, 102.);
        rc.show("other", "claude", false);
        let other = rc.said.get_untracked();
        answer_refusal(rc, &canonical, id, refusal(&t));
        assert_eq!(rc.said.get_untracked(), other);
        rc.bind_alias("sibling", &canonical, 1000);
        rc.show("sibling", "codex", false);
        rc.take(ROUTE, inventory(&t, "recovered"), 104.);
        assert!(no_old_success(rc).contains("grant changed"));
        rc.show(ROUTE, "codex", false);
        assert!(no_old_success(rc).contains("grant changed"));
    });
}

#[test]
fn lost_failed_and_unproved_refusals_keep_u1_and_never_restore_the_old_success_after_recovery() {
    let t = trace(true);
    for which in 0..4 {
        Owner::new().with(|| {
            let (rc, canonical) = start(&t);
            let id = rc.begin_send(&canonical, 102.);
            let answer = match which {
                0 => Answer::Lost,
                1 => Answer::Failed("The server answered 503.".into()),
                2 => Answer::Refused(Refusal {
                    sent: Some(true),
                    ..refusal(&t)
                }),
                _ => Answer::Ok(ClaimResult::default()),
            };
            rc.claim_answer(&canonical, id, answer, 103.);
            rc.take(ROUTE, inventory(&t, "recovered"), 104.);
            assert!(rc.unresolved.get_untracked().blocks());
            assert!(no_old_success(rc).contains("no confirmed answer"));
            rc.take(ROUTE, inventory(&t, "recovered"), 120102.);
            assert!(!rc.unresolved.get_untracked().blocks());
            no_old_success(rc);
            assert!(
                !rc.said
                    .get_untracked()
                    .unwrap()
                    .text
                    .contains("Nothing was sent")
            );
        });
    }
}

#[test]
fn every_refusal_refresh_pending_and_retry_delivery_order_keeps_the_newest_action_current() {
    fn permutations(left: &mut [u8], at: usize, out: &mut Vec<Vec<u8>>) {
        if at == left.len() {
            out.push(left.to_vec());
            return;
        }
        for i in at..left.len() {
            left.swap(at, i);
            permutations(left, at + 1, out);
            left.swap(at, i);
        }
    }
    let mut orders = vec![];
    permutations(&mut [0, 1, 2, 3], 0, &mut orders);
    assert_eq!(orders.len(), 24);
    let t = trace(true);
    for order in orders {
        Owner::new().with(|| {
            let (rc, canonical) = start(&t);
            let old = rc.begin_send(&canonical, 102.);
            answer_refusal(rc, &canonical, old, refusal(&t));
            let new = rc.begin_send(&canonical, 105.);
            let mut settled = false;
            for event in &order {
                match event {
                    0 => answer_refusal(rc, &canonical, old, refusal(&t)),
                    1 => rc.take(ROUTE, inventory(&t, "recovered"), 104.),
                    2 => rc.take(ROUTE, inventory(&t, "duringRetry"), 106.),
                    _ => {
                        let result = serde_json::from_value(t["retry"].clone()).unwrap();
                        rc.claim_answer(&canonical, new, Answer::Ok(result), 107.);
                        settled = true;
                    }
                }
                assert_eq!(
                    !rc.unresolved.get_untracked().blocks(),
                    settled,
                    "{order:?} at {event}"
                );
                assert_eq!(
                    rc.said
                        .get_untracked()
                        .unwrap()
                        .text
                        .starts_with("Reset applied"),
                    settled,
                    "{order:?} at {event}"
                );
            }
        });
    }
}

#[test]
fn unavailable_recovery_keeps_the_refusal_current_and_offers_check_again_until_fresh() {
    let t = trace(true);
    Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        let id = rc.begin_send(&canonical, 102.);
        answer_refusal(rc, &canonical, id, refusal(&t));
        let mut unavailable = inventory(&t, "recovered");
        unavailable.status = "unavailable".into();
        unavailable.reason = "credential-host-unavailable".into();
        rc.take(ROUTE, unavailable, 104.);
        assert!(no_old_success(rc).contains("grant changed"));
        assert!(rc.said.get_untracked().unwrap().check);
        assert!(rc.unresolved.get_untracked().blocks());
        rc.take(ROUTE, inventory(&t, "recovered"), 105.);
        assert!(no_old_success(rc).contains("grant changed"));
        assert!(!rc.unresolved.get_untracked().blocks());
    });
}
