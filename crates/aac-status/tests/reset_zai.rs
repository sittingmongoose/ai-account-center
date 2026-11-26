use aac_status::reset::{Identity, Snapshot};
use aac_status::reset_rules::rules;
use aac_status::reset_zai::{PackList, apply_quota};
use serde_json::{Value, json};

const NOW: i64 = 1_795_098_400_000;
const RESET: i64 = 1_795_700_000_000;

fn list() -> Value {
    json!({"success":true,"code":200,"data":{
        "targetType":"PERSONAL","customerId":123,"organizationId":"org-1","projectId":"project-1",
        "weekResets":[{"recordId":9_007_199_254_740_993u64,"grantType":"GLOBAL",
            "expireTime":"2026-12-22 08:00:00","available":true}],
        "fiveHourResets":[{"recordId":42,"grantType":"GLOBAL","available":false,"resetType":"HOUR"}]
    }})
}

fn snapshot(packs: &Value, weekly: f64) -> Snapshot {
    let list = PackList::parse(packs).unwrap();
    let mut snap = list.snapshot("slot", "nas1", "hash".into(), NOW);
    apply_quota(
        &mut snap,
        &json!({"success":true,"data":{"limits":[
            {"type":"TOKENS_LIMIT","unit":3,"number":5,"percentage":1},
            {"type":"TOKENS_LIMIT","unit":6,"number":1,"percentage":weekly,"nextResetTime":RESET},
            {"type":"TIME_LIMIT","unit":5,"number":1,"percentage":44}
        ]}}),
    )
    .unwrap();
    snap
}

#[test]
fn list_is_exact_and_complete_and_expiry_is_china_time() {
    let mut packs = list();
    let snap = snapshot(&packs, 97.);
    assert_eq!(
        snap.identity,
        Identity {
            provider: "zai".into(),
            account: "123".into(),
            organization: "org-1".into(),
            profile: "slot".into(),
            host: "nas1".into()
        }
    );
    assert_eq!(snap.grants[0].expires_at_ms, Some(1_797_897_600_000));
    assert_eq!(snap.grants[0].left, 1);
    assert!(snap.grants[0].usable);
    assert_eq!(snap.grants[1].kind, "unknown");
    assert_eq!(snap.grants[1].words.as_deref(), Some("HOUR"));
    assert!(!snap.grants[1].usable);
    assert_eq!(snap.windows["five_hour"], 1.);
    assert_eq!(snap.windows["seven_day"], 97.);
    assert_eq!(snap.windows["monthly_time"], 44.);
    assert_eq!(snap.weekly_reset_at_ms, Some(RESET));
    assert!(!snap.grants[0].clears.iter().any(|key| key == "monthly_time"));

    packs["data"]["weekResets"][0]["expireTime"] = json!("2026-11-01 08:00:00");
    assert_eq!(snapshot(&packs, 97.).grants[0].left, 0);
    packs["data"]["weekResets"][0]["resetType"] = json!("FUTURE");
    assert_eq!(snapshot(&packs, 97.).grants[0].kind, "unknown");
    packs["data"]["weekResets"][0]["available"] = Value::Null;
    assert!(PackList::parse(&packs).is_err());
    packs["data"]["weekResets"] = json!(vec![list()["data"]["weekResets"][0].clone(); 101]);
    assert!(PackList::parse(&packs).is_err());
    packs = list();
    packs["data"].as_object_mut().unwrap().remove("fiveHourResets");
    assert!(PackList::parse(&packs).is_err());
    packs = list();
    packs["success"] = json!(false);
    assert!(PackList::parse(&packs).is_err());
}

#[test]
fn rules_preserve_numeric_id_and_confirm_with_exact_spend_and_weekly_drop() {
    let before = snapshot(&list(), 97.);
    let g = &before.grants[0];
    let rule = rules("zai").unwrap();
    let attempt = "0123456789abcdef".repeat(4);
    let (_, body) = rule.write(&before, g, &attempt).unwrap();
    assert_eq!(
        body,
        json!({"targetType":"PERSONAL","resetType":"WEEK","recordId":9007199254740993u64,
        "grantType":"GLOBAL","requestId":"01234567-89ab-4def-8123-456789abcdef"})
    );
    assert_eq!(rule.blocked(&before, g), Some("claim-unproven"));
    let zero = snapshot(&list(), 0.);
    assert_eq!(rule.blocked(&zero, &zero.grants[0]), Some("no-weekly-usage"));
    let mut gone = list();
    gone["data"]["weekResets"] = json!([]);
    assert!(rule.verify(&before, g, &snapshot(&gone, 0.), false));
    assert!(!rule.verify(&before, g, &snapshot(&gone, 97.), true));
    assert!(!rule.verify(&before, g, &zero, true));
    let mut unavailable = list();
    unavailable["data"]["weekResets"][0]["available"] = json!(false);
    assert!(rule.verify(&before, g, &snapshot(&unavailable, 0.), false));
    assert!(!rule.unspent(&before, g, &before, 119_999));
    assert!(rule.unspent(&before, g, &before, 120_000));
    assert!(!rule.unspent(&before, g, &snapshot(&gone, 97.), 120_000));
}

fn private_root() -> tempfile::TempDir {
    let root = tempfile::tempdir().unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    root
}

const ROUTE: &str = "zai:acct:00000001";
const SENT: i64 = NOW + 500;

fn capture(store: &mut aac_status::reset::Store, at: i64, mut snap: Snapshot) -> Snapshot {
    let (_, ticket) = store.capture_read(ROUTE, || Ok(())).unwrap();
    store.bind_zai_read(ROUTE, &ticket, &snap.identity).unwrap();
    snap.read_ticket = Some(ticket);
    snap.observed_at_ms = at;
    snap
}

fn fixture_attempt(store: &mut aac_status::reset::Store, db: &std::path::Path) -> (Snapshot, String, String) {
    let before = capture(store, NOW, snapshot(&list(), 97.));
    let inv = store.inventory(ROUTE, &before, NOW, &"a".repeat(64)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap();
    assert_eq!(inv["grants"][0]["claimBlockedReason"], "claim-unproven");
    assert_eq!(
        store.prepare(ROUTE, key, "local", &before, NOW, &"a".repeat(64)),
        Err("claim-unproven")
    );
    let g = &before.grants[0];
    let attempt = "c".repeat(64);
    // Seed a reserved attempt directly. The production gate is closed, including in this test.
    rusqlite::Connection::open(db)
        .unwrap()
        .execute(
            "INSERT INTO attempts VALUES(?,?,?,?,?,'claimed-unconfirmed',1,?,?,0)",
            rusqlite::params![
                attempt,
                ROUTE,
                key,
                serde_json::to_string(&before).unwrap(),
                serde_json::to_string(g).unwrap(),
                NOW,
                inv["canonicalAccountKey"].as_str().unwrap()
            ],
        )
        .unwrap();
    let (method, body) = rules("zai").unwrap().write(&before, g, &attempt).unwrap();
    let write = json!({"method":method,"body":body,"attemptKey":attempt,"routeAccount":ROUTE,
        "identity":before.identity,"sourceBinding":before.source_binding});
    let mut wrong = write.clone();
    wrong["body"]["grantType"] = json!("OTHER");
    assert_eq!(store.mark_zai_send(&wrong, SENT), Err("grant-changed"));
    store.mark_zai_send(&write, SENT).unwrap();
    assert!(store.mark_zai_send(&write, SENT).is_err());
    (before, key.to_string(), attempt)
}

#[test]
fn applying_a_lost_answer_unlock_requires_a_current_same_key_read_started_two_minutes_later() {
    use aac_status::reset::{Outcome, Store};
    let root = private_root();
    let db = root.path().join("claims.sqlite");
    let mut store = Store::open(&db).unwrap();
    let (before, key, attempt) = fixture_attempt(&mut store, &db);
    let route2 = "zai:acct:00000002";
    let mut alias = before.clone();
    alias.identity.profile = "slot2".into();
    alias.source_binding = "second-key".into();
    let (_, ticket) = store.capture_read(route2, || Ok(())).unwrap();
    store.bind_zai_read(route2, &ticket, &alias.identity).unwrap();
    alias.read_ticket = Some(ticket);
    let inv = store.inventory(route2, &alias, NOW, &"d".repeat(64)).unwrap();
    let alias_key = inv["grants"][0]["grantKey"].as_str().unwrap();
    assert_eq!(store.gate(route2, alias_key, NOW, false), Err("check-again-required"));
    let pending = |result: Value| {
        assert_eq!(result["pending"], true);
        assert_eq!(result["outcome"], "claimed-unconfirmed");
    };
    pending(
        store
            .finish(ROUTE, &attempt, Outcome::Ambiguous, None, SENT + 100, None)
            .unwrap(),
    );

    let early = capture(&mut store, SENT + 119_999, before.clone());
    // Completion after 2 min does not turn a read that started too early into proof.
    pending(store.check(ROUTE, &attempt, &early, SENT + 120_050).unwrap());
    let mut different_pack = before.clone();
    different_pack.grants[0].id = Some("55".into());
    let wrong = capture(&mut store, SENT + 120_000, different_pack);
    pending(store.check(ROUTE, &attempt, &wrong, SENT + 120_050).unwrap());
    let mut changed_week = before.clone();
    changed_week.weekly_reset_at_ms = Some(RESET + 1);
    let wrong = capture(&mut store, SENT + 120_000, changed_week);
    pending(store.check(ROUTE, &attempt, &wrong, SENT + 120_050).unwrap());
    let mut wrong_binding = before.clone();
    wrong_binding.source_binding = "replacement".into();
    let wrong = capture(&mut store, SENT + 120_000, wrong_binding);
    pending(store.check(ROUTE, &attempt, &wrong, SENT + 120_050).unwrap());
    let stale = capture(&mut store, SENT + 120_000, before.clone());
    let valid = capture(&mut store, SENT + 120_000, before);
    pending(store.check(ROUTE, &attempt, &stale, SENT + 120_050).unwrap());
    assert_eq!(
        store.gate(ROUTE, &key, SENT + 120_050, false),
        Err("check-again-required")
    );
    let result = store.check(ROUTE, &attempt, &valid, SENT + 120_050).unwrap();
    assert_eq!(result["outcome"], "not-applied");
    assert_eq!(result["pending"], false);
    assert!(store.gate(ROUTE, &key, SENT + 120_050, false).is_ok());
}

#[test]
fn settle_requires_new_ticket_same_customer_and_key_and_weekly_drop() {
    use aac_status::reset::{Outcome, Store};
    let root = private_root();
    let db = root.path().join("claims.sqlite");
    let mut store = Store::open(&db).unwrap();
    let (before, _, attempt) = fixture_attempt(&mut store, &db);
    let mut gone = list();
    gone["data"]["weekResets"] = json!([]);
    let mut drop = snapshot(&gone, 0.);
    drop.observed_at_ms = SENT + 1;
    drop.read_ticket.clone_from(&before.read_ticket);
    let attempt_finish = |s: &mut Store, after: &Snapshot| {
        s.finish(ROUTE, &attempt, Outcome::Ambiguous, Some(after), SENT + 60_000, None)
            .unwrap()
    };
    assert_eq!(attempt_finish(&mut store, &drop)["outcome"], "claimed-unconfirmed");
    let mut wrong_key = drop.clone();
    wrong_key.source_binding = "replacement".into();
    let wrong_key = capture(&mut store, SENT + 55_000, wrong_key);
    assert_eq!(attempt_finish(&mut store, &wrong_key)["outcome"], "claimed-unconfirmed");
    let stale = capture(&mut store, SENT + 1, drop.clone());
    assert_eq!(attempt_finish(&mut store, &stale)["outcome"], "claimed-unconfirmed");
    let mut wrong_customer = drop.clone();
    wrong_customer.identity.account = "other".into();
    let wrong_customer = capture(&mut store, SENT + 55_000, wrong_customer);
    assert_eq!(
        attempt_finish(&mut store, &wrong_customer)["outcome"],
        "claimed-unconfirmed"
    );
    let exact = capture(&mut store, SENT + 55_001, drop);
    let result = attempt_finish(&mut store, &exact);
    assert_eq!(result["outcome"], "claimed");
    assert_eq!(result["weeklyResetAtMs"], RESET);
    assert_eq!(result["providerSuccess"], false);
}

#[test]
fn provider_admission_matches_the_table() {
    use aac_status::reset::Store;
    let root = private_root();
    let mut store = Store::open(&root.path().join("claims.sqlite")).unwrap();
    for (i, (provider, kind, claimable)) in [
        ("claude", "weekly", true),
        ("codex", "banked", true),
        ("zai", "weekly", false),
        ("qwen", "weekly", false),
        ("antigravity", "weekly", false),
    ]
    .into_iter()
    .enumerate()
    {
        let route = format!("{provider}:fixture");
        let mut snap = snapshot(&list(), 97.);
        snap.identity.provider = provider.into();
        snap.grants[0].kind = kind.into();
        snap.grants.truncate(1);
        let (_, ticket) = store.capture_read(&route, || Ok(())).unwrap();
        snap.read_ticket = Some(ticket);
        let entropy = format!("{i:064x}");
        let inv = store.inventory(&route, &snap, NOW, &entropy);
        if provider == "antigravity" {
            assert_eq!(inv, Err("shape"));
            continue;
        }
        assert_eq!(inv.unwrap()["grants"][0]["claimable"], claimable);
        assert_eq!(
            rules(provider).is_some_and(|r| r.write(&snap, &snap.grants[0], &entropy).is_ok()),
            provider != "qwen"
        );
    }
}

#[test]
fn provider_answers_keep_their_holds_and_success() {
    use aac_status::reset::{Outcome, Store};
    for (outcome, expected) in [
        (Outcome::RateLimited, "rate-limited"),
        (Outcome::Reset, "claimed-unconfirmed"),
    ] {
        let root = private_root();
        let db = root.path().join("claims.sqlite");
        let mut store = Store::open(&db).unwrap();
        let (before, key, attempt) = fixture_attempt(&mut store, &db);
        let result = store
            .finish(ROUTE, &attempt, outcome, None, SENT + 1, Some(18_000))
            .unwrap();
        assert_eq!(result["outcome"], expected);
        if matches!(outcome, Outcome::RateLimited) {
            assert_eq!(store.gate(ROUTE, &key, SENT + 18_000, false), Err("cooldown"));
            assert!(store.gate(ROUTE, &key, SENT + 18_001, false).is_ok());
        } else {
            let late = capture(&mut store, SENT + 120_000, before);
            let result = store.check(ROUTE, &attempt, &late, SENT + 120_001).unwrap();
            assert_eq!(result["outcome"], "claimed-unconfirmed");
            assert_eq!(result["providerSuccess"], true);
        }
    }
}
