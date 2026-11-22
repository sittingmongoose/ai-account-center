use super::*;

const ROUTE: &str = "codex:fixture";

#[test]
fn own_no_send_receipt_clears_only_its_own_unanswered_obligation() {
    for another_send in [false, true] {
        let t = trace(false);
        Owner::new().with(|| {
            let (rc, canonical) = start(&t);
            let send = rc.begin_send(&canonical, 102.);
            if another_send {
                rc.begin_send(&canonical, 103.);
            }
            // Reuse real native receipt ordering/evidence; only the settled
            // outcome is the new no-send outcome (native-tested separately).
            let mut result: ClaimResult = serde_json::from_value(t["retry"].clone()).unwrap();
            result.outcome = "not-sent".into();
            result.provider_success = false;
            let latest = result
                .evidence
                .as_mut()
                .unwrap()
                .latest_attempt
                .as_mut()
                .unwrap();
            latest.outcome = "not-sent".into();
            latest.provider_success = false;
            rc.claim_answer(&canonical, send, Answer::Ok(result), 104.);
            assert_eq!(rc.unresolved.get_untracked().blocks(), another_send);
            let said = rc.said.get_untracked().unwrap();
            if !another_send {
                assert!(said.text.contains("did not send"));
                assert!(!said.check);
                assert!(!said.text.contains("may have been used"));
            }
        });
    }
}
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

fn start_on(t: &serde_json::Value, provider: &str, fetch: bool) -> (ResetCtx, String) {
    let rc = ResetCtx::new();
    rc.show(ROUTE, provider, fetch);
    let before = inventory(t, "before");
    let canonical = before.canonical_account_key.clone().unwrap();
    rc.take(ROUTE, before, 100.);
    (rc, canonical)
}

fn expired_token_inventory(t: &serde_json::Value, grant_kind: &str) -> Inventory {
    let mut inv = inventory(t, "withToken");
    if let Some(tokens) = inv.evidence.as_mut().and_then(|e| e.claim_tokens.as_mut()) {
        // Client clock is past expiry; the snapshot still lists the key.
        tokens.until_ms = Some(1.0);
    }
    if grant_kind != "banked" {
        for grant in &mut inv.grants {
            grant.kind = grant_kind.into();
            grant.label = "Weekly reset".into();
            grant.refills_text = "Refills your weekly allowance.".into();
        }
    }
    inv
}

/// Spec resets-token-fence: Open it again must be allowed to read while the
/// retained lock still blocks spending. Client time must not clear tokens.
fn assert_open_again_may_read(rc: ResetCtx) {
    assert!(!rc.dialog_open(), "dialog is closed before open()");
    assert!(
        rc.unresolved.get_untracked().blocks(),
        "retained claimTokens still lock spending"
    );
    assert!(
        !use_enabled(true, &rc.unresolved.get_untracked(), &Dialog::Closed),
        "Use reset stays fenced by the retained lock"
    );
    assert!(
        rc.may_read_for_open(),
        "read gate must allow a server read while the retained lock still blocks"
    );
    rc.dialog.set(Dialog::Opening {
        grant_key: "held-confirmation".into(),
    });
    assert!(rc.dialog_open());
    assert!(
        !rc.may_read_for_open(),
        "an already-open dialog still skips a second read"
    );
}

#[test]
fn expired_claude_weekly_confirmation_may_read_for_open() {
    // resets-Astra-4-L6
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, _) = start_on(&t, "claude", false);
        rc.take(ROUTE, expired_token_inventory(&t, "weekly"), 101.);
        assert_open_again_may_read(rc);
    });
}

#[test]
fn expired_claude_weekly_after_reopening_details_may_read_for_open() {
    // resets-Sol-5-L6
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, _) = start_on(&t, "claude", false);
        rc.take(ROUTE, expired_token_inventory(&t, "weekly"), 101.);
        rc.show("", "claude", false);
        rc.show(ROUTE, "claude", false);
        assert_open_again_may_read(rc);
    });
}

#[test]
fn expired_codex_credit_confirmation_may_read_for_open() {
    // resets-Astra-5-L6 — native state test named in the spec
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, _) = start_on(&t, "codex", false);
        let held = expired_token_inventory(&t, "banked");
        assert!(
            held.evidence
                .as_ref()
                .and_then(|e| e.claim_tokens.as_ref())
                .and_then(|tokens| tokens.until_ms)
                < Some(100.)
        );
        rc.take(ROUTE, held, 101.);
        assert_open_again_may_read(rc);
    });
}

#[test]
fn expired_confirmation_after_successful_inventory_may_read_for_open() {
    // resets-Astra-6-L6 — expiry control adapted to fix9: inventory must be ok
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, _) = start_on(&t, "codex", false);
        let held = expired_token_inventory(&t, "banked");
        assert_eq!(held.status, "ok");
        rc.take(ROUTE, held, 101.);
        assert!(matches!(rc.load.get_untracked(), Load::Ready(_)));
        assert_open_again_may_read(rc);
    });
}

#[test]
fn expired_confirmation_after_a_hint_may_read_for_open() {
    // resets-Sol-6-L6
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, _) = start_on(&t, "codex", false);
        rc.take(ROUTE, expired_token_inventory(&t, "banked"), 101.);
        rc.invalidate();
        assert_open_again_may_read(rc);
    });
}

#[test]
fn expired_confirmation_after_navigation_back_may_read_for_open() {
    // resets-Astra-7-L6
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, _) = start_on(&t, "claude", false);
        rc.take(ROUTE, expired_token_inventory(&t, "weekly"), 101.);
        rc.show("other", "claude", false);
        rc.show(ROUTE, "claude", false);
        assert_open_again_may_read(rc);
    });
}

#[test]
fn expired_confirmation_with_fresh_page_load_suppressed_may_read_for_open() {
    // resets-Sol-7-L6
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, _) = start_on(&t, "codex", false);
        rc.take(ROUTE, expired_token_inventory(&t, "banked"), 101.);
        assert!(
            rc.cache
                .with_value(|cache| cache.iter().any(|(account, _, inv)| {
                    account == ROUTE
                        && inv.evidence.as_ref().is_some_and(|e| {
                            e.claim_tokens.as_ref().is_some_and(|tokens| {
                                !tokens.token_keys.is_empty() && tokens.until_ms == Some(1.0)
                            })
                        })
                })),
            "suppressed fresh load still holds the expired snapshot"
        );
        rc.show(ROUTE, "codex", false);
        assert_open_again_may_read(rc);
    });
}

/// Spec resets-owner-liveness: a claim or Check again is in flight, then the
/// owner is cleaned up (sign-out or frame disposal) and the answer arrives
/// afterwards. `spawn_local` tasks outlive owner cleanup, so the completion
/// must drop the answer instead of touching a disposed signal.
fn started_then_disposed(t: &serde_json::Value) -> (ResetCtx, String, u64) {
    Owner::new().with(|| {
        let (rc, canonical) = start(t);
        let send = rc.begin_send(&canonical, 102.);
        (rc, canonical, send)
    })
}

fn retry_result(t: &serde_json::Value) -> ClaimResult {
    serde_json::from_value(t["retry"].clone()).unwrap()
}

#[test]
fn a_receipt_after_owner_cleanup_is_dropped_not_a_panic() {
    // resets-Sol-3-L4
    let t = trace(false);
    let (rc, canonical, send) = Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        let send = rc.begin_send(&canonical, 102.);
        (rc, canonical, send)
    }); // owner dropped: ResetCtx signals are disposed
    let result: ClaimResult = serde_json::from_value(t["retry"].clone()).unwrap();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Ok(result), 104.);
    }));
    assert!(
        outcome.is_ok(),
        "a disposed owner must not be touched by a late receipt"
    );
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn a_refusal_with_evidence_after_owner_cleanup_is_dropped_not_a_panic() {
    // resets-Astra-4-L5
    let t = trace(false);
    let (rc, canonical, send) = started_then_disposed(&t);
    let r = refusal(&t);
    assert!(r.evidence.is_some(), "fixture refusal carries evidence");
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Refused(r), 104.);
    }));
    assert!(
        outcome.is_ok(),
        "a disposed owner must not be touched by a late refusal"
    );
    assert!(rc.said.try_get_untracked().is_none());
    assert!(rc.unresolved.try_get_untracked().is_none());
}

#[test]
fn a_claim_ok_after_sign_out_is_dropped_not_a_panic() {
    // resets-Sol-4-L4: sign-out disposes the frame owner while the claim is in flight.
    let t = trace(true);
    let (rc, canonical, send) = started_then_disposed(&t);
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Ok(retry_result(&t)), 104.);
    }));
    assert!(
        outcome.is_ok(),
        "a claim settling after sign-out must not revive the disposed page"
    );
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn a_check_again_ok_after_owner_cleanup_is_dropped_not_a_panic() {
    // resets-Sol-5-L4: Check again completes through receive with no send id.
    let t = trace(false);
    let (rc, canonical, _send, attempt) = Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        // The server lists the pending attempt, so Check again has a key to ask about.
        rc.take(ROUTE, inventory(&t, "duringRetry"), 101.);
        let send = rc.begin_send(&canonical, 102.);
        let attempt = rc.unresolved.get_untracked().key.unwrap();
        (rc, canonical, send, attempt)
    });
    let mut result = retry_result(&t);
    result.attempt_key = attempt;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.receive(&canonical, &result, None);
    }));
    assert!(
        outcome.is_ok(),
        "a late Check again answer must not touch the disposed owner"
    );
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn a_late_receipt_and_history_after_disposed_aliases_are_dropped_not_a_panic() {
    // resets-Astra-5-L5: the alias lookup and the history read must resolve, not panic.
    let t = trace(false);
    let (rc, canonical, send) = started_then_disposed(&t);
    let late = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Ok(retry_result(&t)), 104.);
    }));
    assert!(late.is_ok(), "the late receipt must be dropped");
    let history =
        std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rc.history_of(&canonical)));
    assert!(
        history.is_ok(),
        "history of a disposed owner must resolve to the route key, not panic"
    );
    assert!(history.unwrap().is_empty());
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn claim_and_check_ok_after_owner_cleanup_are_both_dropped_not_a_panic() {
    // resets-Astra-6-L5: both completion bodies (confirm and check_again).
    let t = trace(false);
    let (rc, canonical, send) = started_then_disposed(&t);
    let claimed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Ok(retry_result(&t)), 104.);
    }));
    let checked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.receive(&canonical, &retry_result(&t), None);
    }));
    assert!(claimed.is_ok(), "the late claim answer must be dropped");
    assert!(checked.is_ok(), "the late check answer must be dropped");
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn a_refund_and_refusal_completion_after_owner_cleanup_are_dropped_not_a_panic() {
    // resets-Sol-6-L4: a not-sent refund and a refusal without evidence.
    let t = trace(false);
    let (rc, canonical, send) = started_then_disposed(&t);
    let mut refund = retry_result(&t);
    refund.outcome = "not-sent".into();
    refund.provider_success = false;
    let latest = refund
        .evidence
        .as_mut()
        .unwrap()
        .latest_attempt
        .as_mut()
        .unwrap();
    latest.outcome = "not-sent".into();
    latest.provider_success = false;
    let refunded = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Ok(refund), 104.);
    }));
    let mut bare = refusal(&t);
    bare.evidence = None;
    let refused = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Refused(bare), 104.);
    }));
    assert!(refunded.is_ok(), "the late refund must be dropped");
    assert!(
        refused.is_ok(),
        "the late evidence-less refusal must be dropped"
    );
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn failed_and_lost_receipt_reconciliation_after_owner_cleanup_is_dropped_not_a_panic() {
    // resets-Astra-7-L5: the Failed/Lost arms and the evidence reconciliation.
    let t = trace(false);
    let (rc, canonical, send) = started_then_disposed(&t);
    let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(
            &canonical,
            send,
            Answer::Failed("dropped connection".into()),
            104.,
        );
    }));
    let lost = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.claim_answer(&canonical, send, Answer::Lost, 104.);
    }));
    assert!(failed.is_ok(), "the late Failed answer must be dropped");
    assert!(lost.is_ok(), "the late Lost answer must be dropped");
    let mut mismatched = retry_result(&t);
    mismatched.evidence.as_mut().unwrap().canonical_account_key = "another-canonical".into();
    let rejected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.receive(&canonical, &mismatched, Some(send))
    }));
    assert!(rejected.is_ok(), "the evidence guard must not panic");
    assert!(
        !rejected.unwrap(),
        "a receipt for another canonical account stays rejected"
    );
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn update_state_and_project_after_owner_cleanup_are_dropped_not_a_panic() {
    // resets-Sol-7-L4: the shared state write and the view projection.
    let t = trace(false);
    let (rc, canonical, send) = started_then_disposed(&t);
    let updated = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.update_state(&canonical, |s| s.action_unanswered(send, None));
    }));
    let projected = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| rc.project()));
    assert!(updated.is_ok(), "a disposed state write must be dropped");
    assert!(projected.is_ok(), "a disposed projection must be dropped");
    assert!(rc.said.try_get_untracked().is_none());
}

#[test]
fn a_live_owner_still_receives_and_shows_the_receipt() {
    // Control for every row above: with the owner alive the receipt is stored and shown.
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        let send = rc.begin_send(&canonical, 102.);
        assert!(
            rc.unresolved.get_untracked().blocks(),
            "the unanswered send locks spending"
        );
        rc.claim_answer(&canonical, send, Answer::Ok(retry_result(&t)), 104.);
        assert!(
            !rc.unresolved.get_untracked().blocks(),
            "the settled receipt releases the lock"
        );
        let said = rc.said.get_untracked().unwrap();
        assert!(said.text.contains("Reset applied"), "{}", said.text);
    });
}

/// Spec resets-checking-flag: the busy state belongs to the check's own identity, not the page.
/// Retiring A's ticket must never set or clear B's, and a stale answer must not clear a newer check.
#[test]
fn a_check_ticket_is_scoped_to_its_identity_and_retires_only_itself() {
    // resets-Astra-L1 .. resets-Sol-7-L2: one shared boolean kept every account busy until reload.
    let t = trace(false);
    Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        let a = rc.begin_check(&canonical);
        assert_eq!(rc.check_ticket(), Some((canonical.clone(), a)));
        // Navigation to B: A's live ticket must not make B busy.
        rc.show("codex:other", "codex", false);
        assert_eq!(rc.check_ticket(), None);
        // A's answer lands after navigation and retires only A's ticket.
        rc.retire_check(&canonical, a);
        assert_eq!(rc.check_ticket(), None);
        assert!(rc.checking.get_untracked().is_none());
        // B's own check stays live when a stale A answer arrives.
        let b = rc.begin_check("codex:other");
        assert_eq!(rc.check_ticket(), Some(("codex:other".into(), b)));
        rc.retire_check(&canonical, a);
        assert_eq!(rc.check_ticket(), Some(("codex:other".into(), b)));
        // Back on A, a newer check is not cleared by the old ticket either.
        rc.show(ROUTE, "codex", false);
        let a2 = rc.begin_check(&canonical);
        assert!(a2 > a, "check ids are monotonic");
        rc.retire_check(&canonical, a);
        assert_eq!(rc.check_ticket(), Some((canonical.clone(), a2)));
        rc.retire_check(&canonical, a2);
        assert_eq!(rc.check_ticket(), None);
    });
}

#[test]
fn a_check_ticket_retiring_after_owner_cleanup_is_dropped_not_a_panic() {
    // The owner-liveness guarantee extends to the ticket: a late completion after sign-out
    // or frame disposal must not touch a disposed signal.
    let t = trace(false);
    let (rc, canonical, id) = Owner::new().with(|| {
        let (rc, canonical) = start(&t);
        let id = rc.begin_check(&canonical);
        (rc, canonical, id)
    });
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        rc.retire_check(&canonical, id);
    }));
    assert!(
        outcome.is_ok(),
        "a disposed owner must not be touched by a late ticket retirement"
    );
    assert!(rc.checking.try_get_untracked().is_none());
}
