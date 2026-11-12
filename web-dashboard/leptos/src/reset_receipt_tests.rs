use super::*;

#[cfg(test)]
mod recheck10_extra {
    use super::*;
    fn t() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/reset-reload-refusal.json")).unwrap()
    }
    fn fresh_page(label: &str) {
        let t = t();
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show("codex:fixture", "codex", false);
            rc.take(
                "codex:fixture",
                serde_json::from_value(t["recovered"].clone()).unwrap(),
                200.,
            );
            let shown = rc.said.get_untracked().unwrap_or(Said {
                text: String::new(),
                bad: false,
                check: false,
            });
            println!(
                "{label}: {} blocked={} current-primary={} provider-sends={}",
                shown.text,
                rc.unresolved.get_untracked().blocks(),
                t["actualWindows"]["primary"],
                t["providerSends"]
            );
            assert!(
                !shown.text.starts_with("Reset applied"),
                "fresh page must not present A's stale refill as current"
            );
        });
    }
    #[test]
    fn reload_must_not_restore_old_success() {
        fresh_page("reload")
    }
    #[test]
    fn second_page_must_not_restore_old_success() {
        fresh_page("second page")
    }
}
#[cfg(test)]
mod recheck10_actual_consumed {
    use super::*;
    fn run(settled: bool) {
        let t: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/reset-refusal-after-success.json"
        ))
        .unwrap();
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            let route = "codex:fixture";
            rc.show(route, "codex", false);
            let before: Inventory = serde_json::from_value(t["before"].clone()).unwrap();
            let canonical = before.canonical_account_key.clone().unwrap();
            rc.take(route, before, 100.);
            let send = rc.begin_send(&canonical, 102.);
            let r: Refusal = serde_json::from_value(
                t[if settled {
                    "consumedAfterSuccess"
                } else {
                    "consumedWhilePending"
                }]
                .clone(),
            )
            .unwrap();
            assert_eq!(r.sent, Some(false));
            assert_eq!(
                r.reason.as_deref(),
                Some(if settled {
                    "confirmation-invalid"
                } else {
                    "check-again-required"
                })
            );
            let inv: Inventory = serde_json::from_value(
                t[if settled { "afterRetry" } else { "duringRetry" }].clone(),
            )
            .unwrap();
            // Other page B has already become current before old request A's no-send answer arrives.
            rc.take(route, inv.clone(), 120103.);
            let previous = rc.said.get_untracked();
            rc.claim_answer(&canonical, send, Answer::Refused(r), 120104.);
            rc.take(route, inv, 120105.);
            println!(
                "settled={settled} before={previous:?} after={:?} blocked={}",
                rc.said.get_untracked(),
                rc.unresolved.get_untracked().blocks()
            );
            assert_eq!(rc.unresolved.get_untracked().blocks(), !settled);
            if settled {
                assert_eq!(
                    rc.said.get_untracked(),
                    previous,
                    "old request must not hide newer server success"
                );
                let rows = history_rows(&rc.history_of(route));
                assert_eq!(rows.len(), 2);
                assert_eq!(rows[0].result, "Used, confirmed");
                assert!(rows[0].at_ms > 0.);
            } else {
                assert!(
                    rc.said
                        .get_untracked()
                        .unwrap()
                        .text
                        .contains("unconfirmed")
                );
            }
        });
    }
    #[test]
    fn old_consumed_request_with_pending_evidence() {
        run(false)
    }
    #[test]
    fn old_consumed_request_with_success_evidence() {
        run(true)
    }
}

#[cfg(test)]
mod recheck10_matrix {
    use super::*;
    #[test]
    fn all_requested_refusals_then_real_retry_no_prior_and_newer_attempts() {
        for prior in [false, true] {
            for (label, reason, prepare) in [
                ("grant changed", "grant-changed", false),
                ("expired", "confirmation-invalid", false),
                ("consumed", "confirmation-invalid", false),
                ("prepare exclusion", "claim-token-outstanding", true),
                ("unavailable", "credential-host-unavailable", true),
            ] {
                let t: serde_json::Value = serde_json::from_str(if prior {
                    include_str!("../tests/fixtures/reset-refusal-after-success.json")
                } else {
                    include_str!("../tests/fixtures/reset-refusal-first-action.json")
                })
                .unwrap();
                Owner::new().with(|| {
 let route="codex:fixture";let rc=ResetCtx::new();rc.show(route,"codex",false);
 let before:Inventory=serde_json::from_value(t["before"].clone()).unwrap();let canonical=before.canonical_account_key.clone().unwrap();rc.take(route,before,100.);
 let old=if prepare {rc.begin_action(&canonical)}else{rc.begin_send(&canonical,102.)};
 let mut r:Refusal=serde_json::from_value(t["refused"].clone()).unwrap();r.reason=Some(reason.into());
 rc.claim_answer(&canonical,old,Answer::Refused(r.clone()),103.);
 rc.take(route,serde_json::from_value(t["recovered"].clone()).unwrap(),104.);
 let said=rc.said.get_untracked().unwrap();assert!(!said.text.contains("Reset applied"));assert!(said.text.contains("Nothing was sent"));assert!(!said.text.contains("0%"));assert!(!rc.unresolved.get_untracked().blocks());
 let new=rc.begin_send(&canonical,105.);rc.claim_answer(&canonical,old,Answer::Refused(r.clone()),106.);assert!(rc.unresolved.get_untracked().blocks());assert!(!rc.said.get_untracked().unwrap().text.contains("Reset applied"));
 let result:ClaimResult=serde_json::from_value(t["retry"].clone()).unwrap();rc.claim_answer(&canonical,new,Answer::Ok(result),107.);rc.take(route,serde_json::from_value(t["afterRetry"].clone()).unwrap(),108.);let success=rc.said.get_untracked();assert!(success.as_ref().unwrap().text.contains("Reset applied"));rc.claim_answer(&canonical,old,Answer::Refused(r),109.);assert_eq!(rc.said.get_untracked(),success);assert!(!rc.unresolved.get_untracked().blocks());
 println!("prior={prior} {label}: refusal, newer send lock, genuine retry, delayed old refusal PASS");
 });
            }
        }
    }
}

#[cfg(test)]
mod historical_projection {
    use super::*;
    fn trace() -> serde_json::Value {
        serde_json::from_str(include_str!("../tests/fixtures/reset-reload-refusal.json")).unwrap()
    }
    fn recovered(t: &serde_json::Value) -> Inventory {
        serde_json::from_value(t["recovered"].clone()).unwrap()
    }
    fn assert_historical(rc: ResetCtx, account: &str, t: &serde_json::Value) {
        assert!(rc.said.get_untracked().is_none());
        let rows = history_rows(&rc.history_of(account));
        assert_eq!(rows.len(), 1);
        assert!(rows[0].at_ms > 0.);
        let recorded = t["recovered"]["history"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["event"] == "attempt")
            .unwrap()["atMs"]
            .as_f64()
            .unwrap();
        assert_eq!(rows[0].at_ms, recorded);
        let summary = history_summary(&rows[0], |at| format!("audit-time {at}"));
        assert!(summary.starts_with("Last claim at audit-time "));
        assert!(!summary.contains("is back") && !summary.contains('%'));
        assert!(!rc.unresolved.get_untracked().blocks());
        assert_eq!(t["actualWindows"]["primary"], 100.);
    }
    #[test]
    fn fresh_navigation_sibling_hint_and_recovery_use_dated_history_not_current_receipt_limits() {
        let t = trace();
        for path in ["fresh", "navigation", "sibling", "hint", "recovery"] {
            Owner::new().with(|| {
                let rc = ResetCtx::new();
                let route = "codex:fixture";
                rc.show(route, "codex", false);
                rc.take(route, recovered(&t), 1.);
                match path {
                    "navigation" => {
                        rc.show("other", "codex", false);
                        rc.show(route, "codex", false);
                    }
                    "sibling" => {
                        let canonical = rc.lock_key(route);
                        rc.bind_alias("sibling", &canonical, 1000);
                        rc.show("sibling", "codex", false);
                    }
                    "hint" => {
                        rc.invalidate();
                        rc.take(route, recovered(&t), 2.);
                    }
                    "recovery" => {
                        let mut failed = recovered(&t);
                        failed.status = "unavailable".into();
                        rc.take(route, failed, 2.);
                        assert!(rc.unresolved.get_untracked().blocks());
                        rc.take(route, recovered(&t), 3.);
                    }
                    _ => {}
                }
                assert_historical(rc, if path == "sibling" { "sibling" } else { route }, &t);
            });
        }
    }
    #[test]
    fn local_success_has_no_receipt_percentage_and_check_recovery_retains_its_provenance() {
        let t: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/reset-refusal-after-success.json"
        ))
        .unwrap();
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            let route = "codex:fixture";
            rc.show(route, "codex", false);
            rc.take(
                route,
                serde_json::from_value(t["before"].clone()).unwrap(),
                1.,
            );
            let canonical = rc.lock_key(route);
            let send = rc.begin_send(&canonical, 2.);
            let mut result: ClaimResult = serde_json::from_value(t["retry"].clone()).unwrap();
            for value in [0., 99.] {
                result.windows[0].used_percent = Some(value);
                assert_eq!(
                    result_said(&result, "codex").text,
                    "Reset applied and confirmed."
                );
            }
            let pending: Inventory = serde_json::from_value(t["duringRetry"].clone()).unwrap();
            let mut pending_result = pending
                .evidence
                .as_ref()
                .unwrap()
                .latest_attempt
                .as_ref()
                .unwrap()
                .clone();
            pending_result.evidence = pending.evidence.clone();
            rc.claim_answer(&canonical, send, Answer::Ok(pending_result), 3.);
            assert!(rc.unresolved.get_untracked().blocks());
            let settled: ClaimResult = serde_json::from_value(t["retry"].clone()).unwrap();
            rc.receive(&canonical, &settled, None);
            rc.take(
                route,
                serde_json::from_value(t["afterRetry"].clone()).unwrap(),
                4.,
            );
            let said = rc.said.get_untracked().unwrap();
            assert_eq!(said.text, "Reset applied and confirmed.");
            assert!(!said.text.contains('%'));
        });
    }
    #[test]
    fn old_frontier_refusal_is_current_but_newer_pending_and_confirmed_evidence_cannot_rebase_it() {
        let t: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/reset-refusal-after-success.json"
        ))
        .unwrap();
        for body in ["refused", "consumedWhilePending", "consumedAfterSuccess"] {
            Owner::new().with(|| {
                let rc = ResetCtx::new();
                let route = "codex:fixture";
                rc.show(route, "codex", false);
                rc.take(
                    route,
                    serde_json::from_value(t["before"].clone()).unwrap(),
                    1.,
                );
                let canonical = rc.lock_key(route);
                let send = rc.begin_send(&canonical, 2.);
                let r = serde_json::from_value(t[body].clone()).unwrap();
                rc.claim_answer(&canonical, send, Answer::Refused(r), 3.);
                if body == "refused" {
                    assert!(
                        rc.said
                            .get_untracked()
                            .unwrap()
                            .text
                            .starts_with("Not used:")
                    );
                } else if body == "consumedWhilePending" {
                    let said = rc.said.get_untracked().unwrap();
                    assert!(said.text.contains("unconfirmed") && !said.text.contains("Not used:"));
                    assert!(rc.unresolved.get_untracked().blocks());
                } else {
                    assert!(rc.said.get_untracked().is_none());
                    assert!(!rc.unresolved.get_untracked().blocks());
                }
            });
        }
    }
}

#[test]
fn a_read_only_check_without_a_local_claim_keeps_its_success_in_dated_history() {
    let t: serde_json::Value = serde_json::from_str(include_str!(
        "../tests/fixtures/reset-refusal-after-success.json"
    ))
    .unwrap();
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        let route = "codex:fixture";
        rc.show(route, "codex", false);
        rc.take(
            route,
            serde_json::from_value(t["duringRetry"].clone()).unwrap(),
            1.,
        );
        let canonical = rc.lock_key(route);
        let settled = serde_json::from_value(t["retry"].clone()).unwrap();
        rc.receive(&canonical, &settled, None);
        rc.take(
            route,
            serde_json::from_value(t["afterRetry"].clone()).unwrap(),
            2.,
        );
        assert!(rc.said.get_untracked().is_none());
        assert!(!rc.unresolved.get_untracked().blocks());
        let rows = history_rows(&rc.history_of(route));
        assert_eq!(rows.len(), 2);
        assert!(rows[0].at_ms > 0.);
    });
}
