#![cfg(unix)]
mod support;
use aac_status::reset::*;
use aac_status::reset_source::{ReadContext, codex_read_with_binary, codex_with_binary};
use base64::Engine;
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::{
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use support::ReadFixtures;
fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as i64
}
fn nonce(n: u64) -> String {
    format!("{n:064x}")
}
fn login(home: &Path, who: &str) {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({"email":format!("fixture-{who}"),"https://api.openai.com/auth":{"chatgpt_account_id":who}}).to_string(),
    );
    let v = json!({"tokens":{"account_id":who,"id_token":format!("e30.{claims}.fixture")}});
    std::fs::write(home.join("auth.next"), v.to_string()).unwrap();
    std::fs::rename(home.join("auth.next"), home.join("auth.json")).unwrap();
}
#[test]
fn overlapping_production_reads_discard_the_replaced_login_and_preserve_pending_identity() {
    let (dir, home, binary) = setup();
    let db_path = dir.path().join("claims.sqlite");
    let mut store = Store::open(&db_path).unwrap();
    let call = |write: Option<&Value>| {
        if write.is_some() {
            codex_with_binary(home.to_str().unwrap(), "reused", "fixture-host", write, &binary)
        } else {
            codex_read_with_binary(
                home.to_str().unwrap(),
                "reused",
                "fixture-host",
                &binary,
                &ReadContext {
                    db_path: &db_path,
                    route_account: "codex:reused",
                },
            )
        }
    };
    let before: Snapshot = serde_json::from_value(call(None).unwrap()).unwrap();
    let inv = store.inventory("codex:reused", &before, now(), &nonce(1)).unwrap();
    let key = inv["grants"][0]["grantKey"].as_str().unwrap();
    let prep = store
        .prepare("codex:reused", key, "local", &before, now(), &nonce(2))
        .unwrap();
    let write = store
        .begin(
            "codex:reused",
            key,
            "local",
            prep["confirmationToken"].as_str().unwrap(),
            &before,
            now(),
            &nonce(3),
        )
        .unwrap();
    let mut after = before.clone();
    after.observed_at_ms += 1;
    after.read_ticket = Some(store.captured_ticket("codex:reused").unwrap());
    after.grants[0].left = 0;
    after.windows.insert("primary".into(), 0.);
    store
        .finish("codex:reused", &nonce(3), Outcome::Reset, Some(&after), now() + 2, None)
        .unwrap();
    std::fs::write(home.join("spent-x"), "").unwrap();
    // A prior unresolved attempt through Y's other saved profile, before the delayed read starts.
    let mut yp = before.clone();
    yp.identity.account = "account-y".into();
    yp.identity.profile = "y-sibling".into();
    yp.grants[0].id = Some("credit-account-y".into());
    yp.observed_at_ms = now();
    let yi = store.read_inventory("codex:y-sibling", &yp, now(), &nonce(40)).unwrap();
    let yk = yi["grants"][0]["grantKey"].as_str().unwrap();
    let yt = store
        .prepare("codex:y-sibling", yk, "local", &yp, now(), &nonce(41))
        .unwrap();
    store
        .begin(
            "codex:y-sibling",
            yk,
            "local",
            yt["confirmationToken"].as_str().unwrap(),
            &yp,
            now(),
            &nonce(42),
        )
        .unwrap();
    // A complete, formerly valid source read can also be delayed before Store acceptance.
    let old_preview: Snapshot = serde_json::from_value(call(None).unwrap()).unwrap();
    std::fs::write(home.join("hold-x"), "").unwrap();
    let old_home = home.clone();
    let old_binary = binary.clone();
    let old_db = db_path.clone();
    let pending = std::thread::spawn(move || {
        codex_read_with_binary(
            old_home.to_str().unwrap(),
            "reused",
            "fixture-host",
            &old_binary,
            &ReadContext {
                db_path: &old_db,
                route_account: "codex:reused",
            },
        )
    });
    for _ in 0..1000 {
        if home.join("waiting").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(home.join("waiting").exists());
    login(&home, "account-y");
    let y: Snapshot = serde_json::from_value(call(None).unwrap()).unwrap();
    assert_eq!(y.identity.account, "account-y");
    let newer = store.inventory("codex:reused", &y, now(), &nonce(90)).unwrap();
    std::fs::write(home.join("release"), "").unwrap();
    let got = pending.join().unwrap();
    assert_eq!(got.unwrap_err(), "stale-inventory");
    assert!(old_preview.observed_at_ms < y.observed_at_ms);
    assert_eq!(
        store
            .inventory("codex:reused", &old_preview, now(), &nonce(91))
            .unwrap_err(),
        "stale-inventory"
    );
    let recovery = store.pending("codex:reused", now()).unwrap();
    assert_eq!(recovery["bindingRevision"], newer["bindingRevision"]);
    assert_eq!(recovery["evidence"], newer["evidence"]);
    // This is the production router's source-failure recovery shape: no old account snapshot.
    let mut delayed = recovery.clone();
    delayed["status"] = json!("unavailable");
    delayed["reason"] = json!("stale-inventory");
    delayed["grants"] = json!([]);
    let refused = call(Some(
        &json!({"method":write.method,"body":write.body,"identity":write.identity,"sourceBinding":write.source_binding}),
    ));
    assert!(refused.is_err());
    assert!(!home.join("sent").exists());
    let saved: Value = serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
    let record = json!({"newer":newer,"delayed":delayed,"recovery":recovery,"currentSavedAccount":saved["tokens"]["account_id"],"writeGuard":refused.unwrap_err(),"providerSends":0,"yRecovery":store.pending("codex:y-sibling",now()).unwrap()});
    assert_eq!(
        record["newer"]["canonicalAccountKey"],
        record["recovery"]["canonicalAccountKey"]
    );
    assert_eq!(record["yRecovery"]["pendingAttempt"], nonce(42));
    golden("reset-source-rebind.json", &record);
}

// Only sample/audit clock values vary. All identity, revision, attempt and result fields are golden checked.
fn stable(mut v: Value) -> Value {
    match &mut v {
        Value::Object(map) => {
            for key in ["observedAtMs", "sourceAgeMs", "atMs"] {
                map.remove(key);
            }
            for value in map.values_mut() {
                *value = stable(value.take());
            }
        }
        Value::Array(items) => {
            for value in items {
                *value = stable(value.take());
            }
        }
        _ => {}
    }
    v
}
fn golden(name: &str, value: &Value) {
    if let Ok(dir) = std::env::var("AAC_RESET_FIXTURE_DIR") {
        std::fs::write(
            Path::new(&dir).join(name),
            serde_json::to_string_pretty(value).unwrap() + "\n",
        )
        .unwrap();
    } else {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web-dashboard/leptos/tests/fixtures")
            .join(name);
        let saved: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        assert_eq!(stable(value.clone()), stable(saved));
    }
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = dir.path().join("profile");
    std::fs::create_dir(&home).unwrap();
    login(&home, "account-x");
    let binary = dir.path().join("fake-app-server");
    std::fs::write(&binary,r##"#!/usr/bin/python3
import json,os,sys,time
home=os.environ['CODEX_HOME']
who=json.load(open(os.path.join(home,'auth.json')))['tokens']['account_id']
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 method=r['method']
 if method=='initialize': result={}
 elif method=='account/read': result={'account':{'type':'chatgpt','email':'fixture-'+who}}
 elif method=='account/rateLimits/read':
  spent=who=='account-x' and os.path.exists(os.path.join(home,'spent-x'))
  result={'accountId':who,'rateLimits':{'primary':{'usedPercent':0 if spent else 100}},'rateLimitResetCredits':{'availableCount':0 if spent else 1,'credits':[{'id':'credit-'+who,'resetType':'codexRateLimits','status':'redeemed' if spent else 'available'}]}}
  if who=='account-x' and os.path.exists(os.path.join(home,'hold-x')):
   open(os.path.join(home,'waiting'),'w').close()
   for _ in range(1000):
    if os.path.exists(os.path.join(home,'release')): break
    time.sleep(.005)
   else: raise RuntimeError('test coordination expired')
 elif method=='account/rateLimitResetCredit/consume':
  open(os.path.join(home,'sent'),'w').close()
  result={'outcome':'reset'}
 else: result={}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
"##).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    (dir, home, binary)
}

fn wait_for_read(home: &Path) {
    for _ in 0..1000 {
        if home.join("waiting").exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("fixture RPC did not reach its read barrier");
}
#[test]
fn verified_read_time_does_not_advance_with_rpc_completion() {
    let (_dir, home, binary) = setup();
    std::fs::write(home.join("hold-x"), "").unwrap();
    let worker_home = home.clone();
    let worker = std::thread::spawn(move || {
        codex_with_binary(worker_home.to_str().unwrap(), "reused", "fixture-host", None, &binary)
    });
    wait_for_read(&home);
    let verified_before = now();
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(home.join("release"), "").unwrap();
    let snapshot: Snapshot = serde_json::from_value(worker.join().unwrap().unwrap()).unwrap();
    assert!(snapshot.observed_at_ms <= verified_before);
    assert!(snapshot.observed_at_ms < now());
    assert!(!home.join("sent").exists());
}
#[test]
fn replaced_hash_or_signed_out_login_during_rpc_discards_the_read() {
    for logout in [false, true] {
        let (_dir, home, binary) = setup();
        std::fs::write(home.join("hold-x"), "").unwrap();
        let worker_home = home.clone();
        let worker = std::thread::spawn(move || {
            codex_with_binary(worker_home.to_str().unwrap(), "reused", "fixture-host", None, &binary)
        });
        wait_for_read(&home);
        if logout {
            std::fs::remove_file(home.join("auth.json")).unwrap();
        } else {
            let mut auth: Value = serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
            auth["fixtureRevision"] = json!(2);
            std::fs::write(home.join("auth.next"), auth.to_string()).unwrap();
            std::fs::rename(home.join("auth.next"), home.join("auth.json")).unwrap();
        }
        std::fs::write(home.join("release"), "").unwrap();
        assert_eq!(worker.join().unwrap().unwrap_err(), "stale-inventory");
        assert!(!home.join("sent").exists());
    }
}

fn ticketed_read(db: &Path, home: &Path, binary: &Path) -> Result<Snapshot> {
    let value = codex_read_with_binary(
        home.to_str().unwrap(),
        "reused",
        "fixture-host",
        binary,
        &ReadContext {
            db_path: db,
            route_account: "route",
        },
    )?;
    Ok(serde_json::from_value(value).unwrap())
}
#[test]
fn completed_reads_keep_ticket_order_in_both_acceptance_orders() {
    for reverse in [false, true] {
        let (dir, home, binary) = setup();
        let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        let x = ticketed_read(&dir.path().join("claims.sqlite"), &home, &binary).unwrap();
        login(&home, "account-y");
        let y = ticketed_read(&dir.path().join("claims.sqlite"), &home, &binary).unwrap();
        if !reverse {
            store.inventory("route", &x, now(), &nonce(1200)).unwrap();
        }
        let newest = store.inventory("route", &y, now(), &nonce(1201)).unwrap();
        assert_eq!(
            store.inventory("route", &x, now(), &nonce(1202)).unwrap_err(),
            "stale-inventory"
        );
        assert_eq!(store.pending("route", now()).unwrap()["evidence"], newest["evidence"]);
    }
}
#[test]
fn aba_or_double_replacement_cannot_promote_a_held_old_read() {
    for final_account in ["account-x", "account-z"] {
        let (dir, home, binary) = setup();
        let mut store = Store::open(&dir.path().join("claims.sqlite")).unwrap();
        std::fs::write(home.join("hold-x"), "").unwrap();
        let h = home.clone();
        let b = binary.clone();
        let worker =
            std::thread::spawn(move || codex_with_binary(h.to_str().unwrap(), "reused", "fixture-host", None, &b));
        wait_for_read(&home);
        login(&home, "account-y");
        let y = ticketed_read(&dir.path().join("claims.sqlite"), &home, &binary).unwrap();
        let newest = store.inventory("route", &y, now(), &nonce(1210)).unwrap();
        login(&home, final_account);
        std::fs::write(home.join("release"), "").unwrap();
        let got = worker.join().unwrap();
        if final_account == "account-x" {
            let old: Snapshot = serde_json::from_value(got.unwrap()).unwrap();
            assert_eq!(
                store.inventory("route", &old, now(), &nonce(1211)).unwrap_err(),
                "stale-inventory"
            );
        } else {
            assert_eq!(got.unwrap_err(), "stale-inventory");
        }
        assert_eq!(store.pending("route", now()).unwrap()["evidence"], newest["evidence"]);
        let fresh = ticketed_read(&dir.path().join("claims.sqlite"), &home, &binary).unwrap();
        assert!(store.inventory("route", &fresh, now(), &nonce(1212)).is_ok());
        assert!(!home.join("sent").exists());
    }
}

#[test]
fn request_admission_cannot_reorder_actual_captures_in_either_delivery_order() {
    for newer_first in [false, true] {
        let (dir, home, binary) = setup();
        let db = dir.path().join("claims.sqlite");
        let mut store = Store::open(&db).unwrap();
        let read = |profile: &str, route: &str| -> Snapshot {
            serde_json::from_value(
                codex_read_with_binary(
                    home.to_str().unwrap(),
                    profile,
                    "fixture-host",
                    &binary,
                    &ReadContext {
                        db_path: &db,
                        route_account: route,
                    },
                )
                .unwrap(),
            )
            .unwrap()
        };
        let x = read("reused", "codex:reused");
        let xi = store.inventory("codex:reused", &x, now(), &nonce(2001)).unwrap();
        let key = xi["grants"][0]["grantKey"].as_str().unwrap();
        let token = store
            .prepare("codex:reused", key, "local", &x, now(), &nonce(2002))
            .unwrap();
        let write = store
            .begin(
                "codex:reused",
                key,
                "local",
                token["confirmationToken"].as_str().unwrap(),
                &x,
                now(),
                &nonce(2003),
            )
            .unwrap();
        std::fs::write(home.join("spent-x"), "").unwrap();
        assert_eq!(
            store
                .finish(
                    "codex:reused",
                    &nonce(2003),
                    Outcome::Reset,
                    Some(&read("reused", "codex:reused")),
                    now(),
                    None
                )
                .unwrap()["outcome"],
            "claimed"
        );
        login(&home, "account-y");
        let y = read("y-sibling", "codex:y-sibling");
        let yi = store.inventory("codex:y-sibling", &y, now(), &nonce(2010)).unwrap();
        let key = yi["grants"][0]["grantKey"].as_str().unwrap();
        let token = store
            .prepare("codex:y-sibling", key, "local", &y, now(), &nonce(2011))
            .unwrap();
        store
            .begin(
                "codex:y-sibling",
                key,
                "local",
                token["confirmationToken"].as_str().unwrap(),
                &y,
                now(),
                &nonce(2012),
            )
            .unwrap();
        login(&home, "account-x");
        // A entered its async adapter first, but has not yet reached the native capture.
        let (go, wait) = std::sync::mpsc::channel();
        let h = home.clone();
        let b = binary.clone();
        let d = db.clone();
        let a = std::thread::spawn(move || {
            wait.recv().unwrap();
            codex_read_with_binary(
                h.to_str().unwrap(),
                "reused",
                "fixture-host",
                &b,
                &ReadContext {
                    db_path: &d,
                    route_account: "codex:reused",
                },
            )
            .unwrap()
        });
        // B actually reads X first, passes the final hash recheck, then waits before acceptance.
        let older = read("reused", "codex:reused");
        login(&home, "account-y");
        go.send(()).unwrap();
        let newer: Snapshot = serde_json::from_value(a.join().unwrap()).unwrap();
        assert!(
            newer.read_ticket.as_ref().unwrap().sequence.parse::<u64>().unwrap()
                > older.read_ticket.as_ref().unwrap().sequence.parse::<u64>().unwrap()
        );
        if !newer_first {
            store.inventory("codex:reused", &older, now(), &nonce(2020)).unwrap();
        }
        let accepted = store.inventory("codex:reused", &newer, now(), &nonce(2021)).unwrap();
        assert_eq!(accepted["pendingAttempt"], nonce(2012));
        assert_eq!(
            store
                .inventory("codex:reused", &older, now(), &nonce(2022))
                .unwrap_err(),
            "stale-inventory"
        );
        let recovery = store.pending("codex:reused", now()).unwrap();
        assert_eq!(recovery["evidence"], accepted["evidence"]);
        assert_eq!(
            store.pending("codex:y-sibling", now()).unwrap()["pendingAttempt"],
            nonce(2012)
        );
        let saved: Value = serde_json::from_slice(&std::fs::read(home.join("auth.json")).unwrap()).unwrap();
        assert_eq!(saved["tokens"]["account_id"], "account-y");
        assert!(codex_with_binary(home.to_str().unwrap(), "reused", "fixture-host",
            Some(&json!({"method":write.method,"body":write.body,"identity":write.identity,"sourceBinding":write.source_binding})), &binary).is_err());
        assert!(!home.join("sent").exists());
    }
}

#[test]
fn provider_rpc_does_not_hold_the_capture_transaction() {
    let (dir, home, binary) = setup();
    let db = dir.path().join("claims.sqlite");
    Store::open(&db).unwrap();
    std::fs::write(home.join("hold-x"), "").unwrap();
    let h = home.clone();
    let b = binary.clone();
    let d = db.clone();
    let a = std::thread::spawn(move || {
        codex_read_with_binary(
            h.to_str().unwrap(),
            "reused",
            "fixture-host",
            &b,
            &ReadContext {
                db_path: &d,
                route_account: "route",
            },
        )
    });
    wait_for_read(&home);
    // This second real read must complete while the first provider RPC is still blocked.
    login(&home, "account-y");
    let y = ticketed_read(&db, &home, &binary).unwrap();
    assert_eq!(y.identity.account, "account-y");
    std::fs::write(home.join("release"), "").unwrap();
    assert_eq!(a.join().unwrap().unwrap_err(), "stale-inventory");
}
