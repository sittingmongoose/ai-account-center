use super::*;
fn inventory(canonical: &str, pending: Option<&str>) -> Inventory {
    serde_json::from_value(serde_json::json!({
        "status":"ok", "canonicalAccountKey": canonical, "pendingAttempt": pending,
        "grants":[], "history":[]
    }))
    .unwrap()
}
#[test]
fn a_late_alias_read_preserves_a_newer_unknown_send_latch() {
    Owner::new().with(|| {
        let rc = ResetCtx::new();
        rc.show("route-a", "codex", false);
        rc.take("route-a", inventory("canonical-x", None), 1.0);
        rc.show("route-b", "codex", false);
        // A claim response for A arrives while B's first inventory is in flight.
        rc.update_lock("route-a", |u| {
            u.apply(Lock::Latch {
                key: None,
                proof_after_ms: 120.0,
            })
        });
        rc.take("route-b", inventory("canonical-x", None), 10.0);
        assert!(rc.unresolved.get_untracked().blocks());
        assert!(!use_enabled(
            true,
            &rc.unresolved.get_untracked(),
            &Dialog::Closed
        ));
        rc.take("route-b", inventory("canonical-x", Some("attempt")), 121.0);
        assert_eq!(rc.lock_of("route-a").key.as_deref(), Some("attempt"));
        rc.take("route-b", inventory("canonical-x", None), 122.0);
        assert!(!rc.lock_of("route-a").blocks());
    });
}
#[test]
fn profile_repoint_does_not_falsely_say_the_old_pending_claim_settled() {
    Owner::new().with(|| {
        let responses: Vec<Inventory> =
            serde_json::from_str(include_str!("../tests/fixtures/reset-profile-repoint.json"))
                .unwrap();
        let rc = ResetCtx::new();
        rc.show("codex:reusable-profile", "codex", false);
        rc.take("codex:reusable-profile", responses[0].clone(), 10.0);
        rc.say(Said {
            text: "Claim on X is unconfirmed".into(),
            bad: true,
            check: true,
        });
        // The same saved profile is replaced with Y. Y has never made a claim.
        rc.take("codex:reusable-profile", responses[1].clone(), 20.0);
        let said = rc.said.get_untracked().unwrap();
        println!("REPOINT_TEXT={}", said.text);
        println!(
            "NEW_ACCOUNT_BLOCKED={}",
            rc.unresolved.get_untracked().blocks()
        );
        rc.show("codex:stable-x-profile", "codex", false);
        rc.take("codex:stable-x-profile", responses[2].clone(), 21.0);
        println!(
            "ORIGINAL_ACCOUNT_PENDING={:?}",
            rc.unresolved.get_untracked().key
        );
        assert!(
            rc.unresolved.get_untracked().blocks(),
            "X still owns its unresolved attempt"
        );
        assert_ne!(
            said.text, SETTLED_ELSEWHERE,
            "a profile changing accounts is not settlement of its old account's claim"
        );
    });
}
#[test]
fn reassignment_in_both_directions_keeps_results_and_locks_with_canonical_accounts() {
    for (x, y) in [
        ("canonical-x", "canonical-y"),
        ("canonical-y", "canonical-x"),
    ] {
        Owner::new().with(|| {
            let rc = ResetCtx::new();
            rc.show("reused", "codex", false);
            rc.take("reused", inventory(x, Some("attempt-x")), 10.);
            rc.bind_alias("old-sibling", x);
            rc.bind_alias("new-sibling", y);
            let result_x = Said {
                text: "Claim on the original account is unconfirmed".into(),
                bad: true,
                check: true,
            };
            rc.say(result_x.clone());
            // Capture the same identity a running claim/check callback captures.
            let request_identity = rc.lock_key("reused");
            rc.take("reused", inventory(y, None), 20.);
            assert!(!rc.unresolved.get_untracked().blocks());
            assert_eq!(rc.said.get_untracked().unwrap().text, ACCOUNT_CHANGED);
            assert_eq!(rc.lock_of("old-sibling").key.as_deref(), Some("attempt-x"));
            assert_eq!(rc.said_of("old-sibling"), Some(result_x.clone()));
            assert!(!rc.lock_of("new-sibling").blocks());
            // Another account's fresh empty read cannot prove anything about X.
            rc.take("new-sibling", inventory(y, None), 21.);
            assert!(rc.lock_of("old-sibling").blocks());
            rc.show("old-sibling", "codex", false);
            assert_eq!(rc.said.get_untracked(), Some(result_x));
            assert!(rc.unresolved.get_untracked().blocks());
            rc.show("reused", "codex", false);
            let shown_y = rc.said.get_untracked();
            let final_x = Said {
                text: "Original claim confirmed".into(),
                bad: false,
                check: false,
            };
            // A delayed receipt still updates X, even after this route represents Y.
            rc.update_lock(&request_identity, |u| {
                u.apply(Lock::Settle {
                    key: "attempt-x".into(),
                })
            });
            rc.say_for(&request_identity, final_x.clone());
            assert_eq!(rc.said.get_untracked(), shown_y);
            rc.show("old-sibling", "codex", false);
            assert_eq!(rc.said.get_untracked(), Some(final_x.clone()));
            assert!(!rc.unresolved.get_untracked().blocks());
            // Reassigning back must not overwrite X's retained result with the view notice.
            rc.show("reused", "codex", false);
            rc.take("reused", inventory(x, None), 22.);
            assert_eq!(rc.said.get_untracked().unwrap().text, ACCOUNT_CHANGED);
            assert_eq!(rc.said_of("old-sibling"), Some(final_x));
        });
    }
}
