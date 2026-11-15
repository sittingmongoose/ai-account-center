//! A banked Codex credit can be used at any usage (owner, 2026-11-13). OpenAI chooses which limits it refills, so a
//! claim is confirmed by the exact credit being spent on a fresh same-identity read; windows that fell are evidence.
//! Offline only: synthetic snapshots, generated login material and a fake app-server. No real credentials, provider
//! refresh or reset writes.
#![cfg(unix)]
mod support;
use aac_status::reset::{Grant, Identity, Outcome, Snapshot, Store, Write};
use aac_status::reset_source::{ReadContext, codex_read_with_binary, codex_with_binary};
use base64::Engine;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use support::ReadFixtures;

const ROUTE: &str = "codex:fixture";
const NOW: i64 = 1_000_000;

fn nonce(n: u8) -> String {
    format!("{n:064x}")
}
fn caller() -> String {
    format!("session:{}", nonce(20))
}
fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A saved profile whose fake app-server reports one credit and the 5-hour and weekly limits in `state.json`.
/// `consume` answers `state.outcome`; a "reset" marks the credit redeemed and applies `state.refill`.
/// `state.status`, when set, overrides the credit's status (for example `redeeming`).
struct Fixture {
    _dir: tempfile::TempDir,
    home: PathBuf,
    binary: PathBuf,
    db: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let home = dir.path().join("profile");
        std::fs::create_dir(&home).unwrap();
        let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
            json!({"email":"synthetic-principal","https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}})
                .to_string(),
        );
        // Generated protocol material only: not a stored credential fixture.
        std::fs::write(
            home.join("auth.json"),
            json!({"tokens":{"account_id":"synthetic-account","id_token":format!("e30.{claims}.fixture")}}).to_string(),
        )
        .unwrap();
        let binary = dir.path().join("fake-app-server");
        std::fs::write(
            &binary,
            r##"#!/usr/bin/python3
import json,os,sys
home=os.environ['CODEX_HOME']
path=os.path.join(home,'state.json')
def window(used,mins):
 w={'usedPercent':used}
 if mins is not None: w['windowDurationMins']=mins
 return w
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 method=r['method']
 st=json.load(open(path))
 if method=='initialize': result={}
 elif method=='account/read': result={'account':{'type':'chatgpt','email':'synthetic-principal'}}
 elif method=='account/rateLimits/read':
  status=st.get('status') or ('redeemed' if st['spent'] else 'available')
  result={'accountId':'synthetic-account',
   'rateLimits':{'primary':window(st['five'],st.get('fiveMins')),'secondary':window(st['week'],st.get('weekMins'))},
   'rateLimitResetCredits':{'availableCount':1 if status=='available' else 0,
    'credits':[{'id':'credit_a','resetType':'codexRateLimits','status':status}]}}
 elif method=='account/rateLimitResetCredit/consume':
  with open(os.path.join(home,'sent.jsonl'),'a') as f: f.write(json.dumps(r['params'])+'\n')
  if st['outcome']=='reset':
   st['spent']=True
   st.update(st.get('refill',{}))
   json.dump(st,open(path,'w'))
  result={'outcome':st['outcome']}
 else: result={}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
"##,
        )
        .unwrap();
        std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
        let db = dir.path().join("claims.sqlite");
        Self {
            _dir: dir,
            home,
            binary,
            db,
        }
    }
    fn state(&self, state: Value) {
        std::fs::write(self.home.join("state.json"), state.to_string()).unwrap();
    }
    /// The production capture boundary: saved login, ticket and app-server read.
    fn read(&self) -> Snapshot {
        let context = ReadContext {
            db_path: &self.db,
            route_account: ROUTE,
        };
        let v = codex_read_with_binary(
            self.home.to_str().unwrap(),
            "exact-profile",
            "fixture-host",
            &self.binary,
            &context,
        )
        .unwrap();
        serde_json::from_value(v).unwrap()
    }
    /// The real transport: it re-reads, rechecks identity and binding, then consumes the exact credit.
    fn send(&self, w: &Write) -> Value {
        let write = json!({"identity":w.identity,"sourceBinding":w.source_binding,"method":w.method,"body":w.body});
        codex_with_binary(
            self.home.to_str().unwrap(),
            "exact-profile",
            "fixture-host",
            Some(&write),
            &self.binary,
        )
        .unwrap()
    }
    fn sent(&self) -> Vec<Value> {
        std::fs::read_to_string(self.home.join("sent.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
    fn store(&self) -> Store {
        Store::open(&self.db).unwrap()
    }
}

/// Inventory, prepare and begin on fresh reads. Returns the inventory, the reserved write and the read it began on.
fn reserve(f: &Fixture, s: &mut Store, attempt: &str) -> (Value, Write, Snapshot) {
    let inv = s.inventory(ROUTE, &f.read(), wall(), &nonce(1)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap().to_owned();
    s.gate(ROUTE, &key, wall(), true).unwrap();
    let prepared = s.prepare(ROUTE, &key, &caller(), &f.read(), wall(), &nonce(2)).unwrap();
    s.gate(ROUTE, &key, wall(), false).unwrap();
    let token = prepared["confirmationToken"].as_str().unwrap().to_owned();
    let began = f.read();
    let write = s
        .begin(ROUTE, &key, &caller(), &token, &began, wall(), attempt)
        .unwrap();
    (inv, write, began)
}
fn verification(s: &Store) -> Value {
    s.history(ROUTE)
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .rfind(|e| e["event"] == "verification")
        .unwrap()
        .clone()
}

#[test]
fn a_codex_credit_is_claimable_at_partial_zero_and_full_usage() {
    // The owner's part-used accounts (weekly 60% / 5-hour 2%, and 18%), an idle one, and an exhausted one.
    for (five, week) in [(2.0, 60.0), (18.0, 18.0), (0.0, 0.0), (100.0, 20.0), (100.0, 100.0)] {
        let f = Fixture::new();
        f.state(json!({"five":five,"week":week,"fiveMins":300,"weekMins":10080,"spent":false,"outcome":"reset"}));
        let snap = f.read();
        assert_eq!(snap.grants[0].clears, ["five_hour", "seven_day"], "{five}/{week}");
        let mut s = f.store();
        let inv = s.inventory(ROUTE, &snap, wall(), &nonce(1)).unwrap();
        let g = &inv["grants"][0];
        assert_eq!(g["claimable"], true, "{five}/{week}: {g}");
        assert_eq!(g["claimBlockedReason"], Value::Null);
        assert_eq!(g["resetsLeft"], 1);
        // The confirmation is told what is used now, from the same read it is bound to.
        let key = g["grantKey"].as_str().unwrap();
        let prepared = s.prepare(ROUTE, key, &caller(), &f.read(), wall(), &nonce(2)).unwrap();
        assert_eq!(
            prepared["usage"],
            json!([{"id":"five_hour","usedPercent":five},{"id":"seven_day","usedPercent":week}])
        );
        assert_eq!(prepared["grant"]["claimable"], true);
        assert!(f.sent().is_empty(), "prepare sends nothing");
    }
}

#[test]
fn codex_windows_are_named_by_their_length_or_keep_their_position() {
    let f = Fixture::new();
    for (five_mins, week_mins, names) in [
        (json!(300), json!(10080), ["five_hour", "seven_day"]),
        (Value::Null, Value::Null, ["primary", "secondary"]),
        // Two windows of one length cannot share a name: the second keeps its position.
        (json!(10080), json!(10080), ["secondary", "seven_day"]),
        (json!(43200), json!(10080), ["primary", "seven_day"]),
    ] {
        f.state(json!({"five":40,"week":60,"fiveMins":five_mins,"weekMins":week_mins,"spent":false,"outcome":"reset"}));
        let snap = f.read();
        let keys: Vec<&str> = snap.windows.keys().map(String::as_str).collect();
        assert_eq!(keys, names);
        assert_eq!(snap.grants[0].clears, names);
    }
}

#[test]
fn a_partial_usage_claim_is_confirmed_by_the_exact_credit_and_reports_only_windows_that_fell() {
    for (refill, fell) in [
        (json!({"week":0}), json!([{"id":"seven_day","usedPercent":0.0}])),
        // OpenAI may refill nothing the next read shows yet; the credit leaving is still the proof.
        (json!({}), json!([])),
        // Ordinary use can raise a limit at the same time; that never blocks the proof.
        (
            json!({"five":30,"week":0}),
            json!([{"id":"seven_day","usedPercent":0.0}]),
        ),
    ] {
        let f = Fixture::new();
        f.state(
            json!({"five":2,"week":60,"fiveMins":300,"weekMins":10080,"spent":false,"outcome":"reset","refill":refill}),
        );
        let mut s = f.store();
        let attempt = nonce(3);
        let (_, write, _) = reserve(&f, &mut s, &attempt);
        assert_eq!(write.body, json!({"creditId":"credit_a","idempotencyKey":attempt}));
        assert_eq!(f.send(&write)["outcome"], "reset");
        let r = s
            .finish(ROUTE, &attempt, Outcome::Reset, Some(&f.read()), wall(), None)
            .unwrap();
        assert_eq!(r["outcome"], "claimed", "{refill}");
        assert_eq!(r["verification"], "confirmed");
        assert_eq!(r["pending"], false);
        assert_eq!(r["windows"], fell, "{refill}");
        assert_eq!(f.sent(), [json!({"creditId":"credit_a","idempotencyKey":attempt})]);
        // The audit keeps the before/after counts and windows as evidence, and no raw credit id.
        let v = verification(&s);
        assert_eq!(v["outcome"], "claimed");
        assert_eq!(
            (v["details"]["beforeCount"].clone(), v["details"]["afterCount"].clone()),
            (json!(1), json!(0))
        );
        assert_eq!(v["details"]["beforeWindows"]["seven_day"], 60.0);
        assert!(!s.history(ROUTE).unwrap().to_string().contains("credit_a"));
    }
}

#[test]
fn nothing_to_reset_at_partial_usage_spends_nothing_and_says_so() {
    let f = Fixture::new();
    f.state(json!({"five":2,"week":60,"fiveMins":300,"weekMins":10080,"spent":false,"outcome":"nothingToReset"}));
    let mut s = f.store();
    let attempt = nonce(3);
    let (inv, write, _) = reserve(&f, &mut s, &attempt);
    assert_eq!(f.send(&write)["outcome"], "nothingToReset");
    let after = f.read();
    assert_eq!(after.grants[0].left, 1, "the credit is still there");
    let r = s
        .finish(ROUTE, &attempt, Outcome::Nothing, Some(&after), wall(), None)
        .unwrap();
    assert_eq!(r["outcome"], "nothing-to-reset");
    assert_eq!(r["pending"], false);
    assert_eq!(r["providerSuccess"], false);
    assert_eq!(r["windows"], json!([]));
    // Nothing is held: the same credit is listed, usable and claimable again.
    assert_eq!(s.pending(ROUTE, wall()).unwrap()["attemptKey"], Value::Null);
    let again = s.inventory(ROUTE, &f.read(), wall(), &nonce(4)).unwrap();
    assert_eq!(again["grants"][0]["claimable"], true);
    assert_eq!(again["grants"][0]["resetsLeft"], inv["grants"][0]["resetsLeft"]);
    let v = verification(&s);
    assert_eq!(v["outcome"], "nothing-to-reset");
    assert_eq!(v["details"]["providerSuccess"], false);
    assert_eq!(
        (v["details"]["beforeCount"].clone(), v["details"]["afterCount"].clone()),
        (json!(1), json!(1))
    );
}

#[test]
fn an_unconfirmed_partial_usage_spend_keeps_its_lock_and_is_never_retried() {
    let f = Fixture::new();
    f.state(
        json!({"five":2,"week":60,"fiveMins":300,"weekMins":10080,"spent":false,"outcome":"reset","refill":{"week":0}}),
    );
    let mut s = f.store();
    let attempt = nonce(3);
    let (_, write, began) = reserve(&f, &mut s, &attempt);
    let lagging = f.read();
    // The answer was lost; the provider spent the credit, but this read lags and still lists it.
    f.send(&write);
    let r = s
        .read_finish(ROUTE, &attempt, Outcome::Ambiguous, Some(&lagging), wall(), None)
        .unwrap();
    assert_eq!(r["outcome"], "claimed-unconfirmed");
    assert_eq!(r["pending"], true);
    assert_eq!(r["windows"], json!([]));
    // The unanswered-send lock holds every new spend on this account.
    let inv = s.inventory(ROUTE, &f.read(), wall(), &nonce(5)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap().to_owned();
    assert_eq!(s.gate(ROUTE, &key, wall(), true), Err("check-again-required"));
    // Check again only reads. These reads never confirm: one no newer than the claim (even showing the credit
    // spent), one that lost the credit entirely, and one of another account.
    let mut stale = began.clone();
    stale.grants[0].left = 0;
    stale.grants[0].usable = false;
    assert_eq!(
        s.check(ROUTE, &attempt, &stale, wall()).unwrap()["outcome"],
        "claimed-unconfirmed"
    );
    let mut missing = f.read();
    missing.grants.clear();
    assert_eq!(
        s.check(ROUTE, &attempt, &missing, wall()).unwrap()["outcome"],
        "claimed-unconfirmed"
    );
    let mut other = f.read();
    other.identity.account = "other-account".into();
    assert_eq!(
        s.check(ROUTE, &attempt, &other, wall()).unwrap()["outcome"],
        "claimed-unconfirmed"
    );
    assert_eq!(f.sent().len(), 1, "check never sends");
    // A fresh same-identity read that shows the exact credit spent settles it, without any new send.
    let r = s.check(ROUTE, &attempt, &f.read(), wall()).unwrap();
    assert_eq!(r["outcome"], "claimed");
    assert_eq!(
        r["providerSuccess"], false,
        "the lost answer never became provider success"
    );
    assert_eq!(r["windows"], json!([{"id":"seven_day","usedPercent":0.0}]));
    assert_eq!(f.sent().len(), 1);
    assert_eq!(s.pending(ROUTE, wall()).unwrap()["attemptKey"], Value::Null);
}

fn claude(week: f64) -> Snapshot {
    Snapshot {
        read_ticket: None,
        identity: Identity {
            provider: "claude".into(),
            account: "synthetic-account".into(),
            organization: "synthetic-org".into(),
            profile: "synthetic-profile".into(),
            host: "windows".into(),
        },
        observed_at_ms: NOW,
        grants: vec![Grant {
            id: Some("grant_a".into()),
            kind: "weekly".into(),
            clears: vec!["seven_day".into()],
            left: 2,
            expires_at_ms: None,
            usable: true,
            words: None,
        }],
        windows: std::collections::BTreeMap::from([("seven_day".into(), week), ("five_hour".into(), 10.0)]),
        executor_ready: true,
        source_binding: "synthetic-binding".into(),
        account_label: "synthetic-principal".into(),
    }
}

#[test]
fn claude_keeps_its_scopes_and_its_window_proof() {
    for (fell, outcome, windows) in [
        (false, "claimed-unconfirmed", json!([])),
        (true, "claimed", json!([{"id":"seven_day","usedPercent":0.0}])),
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut s = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        // Claude never gated on usage: a weekly grant at 50% was and is claimable, refilling only its own scope.
        let snap = claude(50.0);
        let inv = s.read_inventory("route", &snap, NOW, &nonce(1)).unwrap();
        let key = inv["grants"][0]["grantKey"].as_str().unwrap();
        assert_eq!(inv["grants"][0]["claimable"], true);
        assert_eq!(inv["grants"][0]["refillsText"], "Refills your weekly allowance.");
        let prepared = s.prepare("route", key, &caller(), &snap, NOW, &nonce(2)).unwrap();
        assert_eq!(prepared["usage"], json!([{"id":"seven_day","usedPercent":50.0}]));
        let token = prepared["confirmationToken"].as_str().unwrap();
        let attempt = nonce(3);
        // Usage moves while the dialog is open (50% -> 52% weekly, 10% -> 12% 5-hour); the claim is not refused.
        let mut fresh = claude(52.0);
        fresh.windows.insert("five_hour".into(), 12.0);
        fresh.read_ticket = Some(s.captured_ticket("route").unwrap());
        s.begin("route", key, &caller(), token, &fresh, NOW, &attempt).unwrap();
        // Claude still needs its refilled window to fall below the sent reading: a spent grant alone stays
        // unconfirmed.
        let mut after = fresh.clone();
        after.observed_at_ms += 1;
        after.grants[0].left = 1;
        if fell {
            after.windows.insert("seven_day".into(), 0.0);
        }
        let r = s
            .read_finish("route", &attempt, Outcome::Reset, Some(&after), NOW + 1, None)
            .unwrap();
        assert_eq!(r["outcome"], outcome);
        assert_eq!(r["windows"], windows);
        assert_eq!(r["sentUsage"], json!([{"id":"seven_day","usedPercent":52.0}]));
    }
}

/// Two Codex credits at weekly 60% / 5-hour 2%, as the source names them.
fn codex(five: f64, week: f64) -> Snapshot {
    let credit = |id: &str| Grant {
        id: Some(id.into()),
        kind: "banked".into(),
        clears: vec!["five_hour".into(), "seven_day".into()],
        left: 1,
        expires_at_ms: Some(NOW + 10_000),
        usable: true,
        words: None,
    };
    Snapshot {
        read_ticket: None,
        identity: Identity {
            provider: "codex".into(),
            account: "synthetic-account".into(),
            organization: String::new(),
            profile: "synthetic-profile".into(),
            host: "fixture-host".into(),
        },
        observed_at_ms: NOW,
        grants: vec![credit("credit_a"), credit("credit_b")],
        windows: std::collections::BTreeMap::from([("five_hour".into(), five), ("seven_day".into(), week)]),
        executor_ready: true,
        source_binding: "synthetic-binding".into(),
        account_label: "synthetic-principal".into(),
    }
}

#[test]
fn usage_moving_while_the_dialog_is_open_never_refuses_but_spend_safety_still_does() {
    for case in [
        "usage moved",
        "other grant",
        "grant spent",
        "grant expired",
        "identity changed",
        "refill keys changed",
        "binding changed",
    ] {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let mut s = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        let shown = codex(2.0, 60.0);
        let inv = s.read_inventory("route", &shown, NOW, &nonce(1)).unwrap();
        let (a, b) = (
            inv["grants"][0]["grantKey"].as_str().unwrap(),
            inv["grants"][1]["grantKey"].as_str().unwrap(),
        );
        let prepared = s.prepare("route", b, &caller(), &shown, NOW, &nonce(2)).unwrap();
        let token = prepared["confirmationToken"].as_str().unwrap();
        // Every case also has usage moving: it must never be what decides.
        let mut fresh = codex(5.0, 62.0);
        let mut key = b;
        let mut now = NOW;
        let refusal = match case {
            "usage moved" => None,
            "other grant" => {
                key = a;
                Some("confirmation-invalid")
            }
            "grant spent" => {
                fresh.grants[1].left = 0;
                fresh.grants[1].usable = false;
                Some("grant-changed")
            }
            "grant expired" => {
                now = NOW + 10_000;
                fresh.observed_at_ms = now;
                Some("grant-unavailable")
            }
            "identity changed" => {
                fresh.identity.account = "other-account".into();
                Some("identity-changed")
            }
            "refill keys changed" => {
                fresh.windows.remove("five_hour");
                for g in &mut fresh.grants {
                    g.clears = vec!["seven_day".into()];
                }
                Some("grant-changed")
            }
            _ => {
                fresh.source_binding = "re-saved-login".into();
                Some("grant-changed")
            }
        };
        let attempt = nonce(3);
        let begun = s.begin("route", key, &caller(), token, &fresh, now, &attempt);
        match refusal {
            Some(reason) => {
                assert_eq!(begun.unwrap_err(), reason, "{case}");
                assert_eq!(s.pending("route", now).unwrap()["attemptKey"], Value::Null, "{case}");
            }
            None => {
                let write = begun.unwrap();
                assert_eq!(write.body["creditId"], "credit_b");
                // The result says what usage the claim was sent at, not what the dialog showed.
                let r = s
                    .read_finish("route", &attempt, Outcome::Nothing, Some(&fresh), NOW + 1, None)
                    .unwrap();
                assert_eq!(r["outcome"], "nothing-to-reset");
                assert_eq!(
                    r["sentUsage"],
                    json!([{"id":"five_hour","usedPercent":5.0},{"id":"seven_day","usedPercent":62.0}])
                );
            }
        }
        // Either way the confirmation was one-shot: it cannot be used again.
        assert_eq!(
            s.begin("route", b, &caller(), token, &fresh, now, &nonce(4))
                .unwrap_err(),
            "confirmation-invalid",
            "{case}"
        );
    }
}

#[test]
fn a_redeeming_credit_stays_unconfirmed_and_locked_until_a_read_says_redeemed() {
    let f = Fixture::new();
    // The consume answer is lost; OpenAI is still redeeming the credit when AAC reads again.
    f.state(
        json!({"five":2,"week":60,"fiveMins":300,"weekMins":10080,"spent":false,"outcome":"reset",
        "refill":{"status":"redeeming"}}),
    );
    let mut s = f.store();
    let attempt = nonce(3);
    let (_, write, _) = reserve(&f, &mut s, &attempt);
    f.send(&write);
    let redeeming = f.read();
    assert_eq!(
        (redeeming.grants[0].left, redeeming.grants[0].usable),
        (1, false),
        "redeeming is in progress, not spent"
    );
    let r = s
        .finish(ROUTE, &attempt, Outcome::Ambiguous, Some(&redeeming), wall(), None)
        .unwrap();
    assert_eq!(r["outcome"], "claimed-unconfirmed");
    assert_eq!(r["verification"], "unconfirmed");
    assert_eq!(r["pending"], true);
    // Locked, and the redeeming credit itself can't be used or sent.
    let inv = s.inventory(ROUTE, &f.read(), wall(), &nonce(5)).unwrap();
    assert_eq!(inv["pendingAttempt"], attempt);
    assert_eq!(inv["grants"][0]["claimable"], false);
    assert_eq!(inv["grants"][0]["claimBlockedReason"], "grant-unavailable");
    let key = inv["grants"][0]["grantKey"].as_str().unwrap().to_owned();
    assert_eq!(s.gate(ROUTE, &key, wall(), true), Err("check-again-required"));
    // Check again while it is still redeeming: still unconfirmed, still locked, nothing sent.
    let r = s.check(ROUTE, &attempt, &f.read(), wall()).unwrap();
    assert_eq!(
        (r["outcome"].clone(), r["pending"].clone()),
        (json!("claimed-unconfirmed"), json!(true))
    );
    assert_eq!(f.sent().len(), 1, "check never sends");
    // A later read that says redeemed confirms it.
    f.state(json!({"five":2,"week":0,"fiveMins":300,"weekMins":10080,"spent":true,"outcome":"reset"}));
    let r = s.check(ROUTE, &attempt, &f.read(), wall()).unwrap();
    assert_eq!(r["outcome"], "claimed");
    assert_eq!(r["pending"], false);
    assert_eq!(r["windows"], json!([{"id":"seven_day","usedPercent":0.0}]));
    assert_eq!(s.pending(ROUTE, wall()).unwrap()["attemptKey"], Value::Null);
    assert_eq!(f.sent().len(), 1);
}

#[test]
fn a_redeeming_credit_confirms_only_when_openai_answered_this_attempt() {
    for answered_now in [true, false] {
        let f = Fixture::new();
        f.state(
            json!({"five":2,"week":60,"fiveMins":300,"weekMins":10080,"spent":false,"outcome":"reset",
            "refill":{"status":"redeeming"}}),
        );
        let mut s = f.store();
        let attempt = nonce(3);
        let (_, write, began) = reserve(&f, &mut s, &attempt);
        assert_eq!(f.send(&write)["outcome"], "reset");
        if answered_now {
            // OpenAI answered `reset` and the read shows the credit redeeming: today's handling, confirmed.
            let r = s
                .finish(ROUTE, &attempt, Outcome::Reset, Some(&f.read()), wall(), None)
                .unwrap();
            assert_eq!(r["outcome"], "claimed");
            assert_eq!(r["providerSuccess"], true);
        } else {
            // OpenAI answered `reset`, but the read lagged (still available): unconfirmed with provider success.
            let lagging = began.clone();
            let r = s
                .read_finish(ROUTE, &attempt, Outcome::Reset, Some(&lagging), wall(), None)
                .unwrap();
            assert_eq!(
                (r["outcome"].clone(), r["providerSuccess"].clone()),
                (json!("claimed-unconfirmed"), json!(true))
            );
            // Check again later sees it redeeming: the earlier `reset` answer for this attempt counts.
            let r = s.check(ROUTE, &attempt, &f.read(), wall()).unwrap();
            assert_eq!(r["outcome"], "claimed");
        }
        assert_eq!(s.pending(ROUTE, wall()).unwrap()["attemptKey"], Value::Null);
        assert_eq!(f.sent().len(), 1);
    }
}
