#![cfg(unix)]
use aac_status::{
    reset::Store,
    reset_claude_source::{self, HelperCommand},
};
use serde_json::json;
use std::{path::Path, time::Duration};
const X: &str = "11111111-1111-1111-1111-111111111111";
const Y: &str = "44444444-4444-4444-4444-444444444444";
fn login(root: &Path, account: &str) {
    std::fs::write(root.join("next"), account).unwrap();
    std::fs::rename(root.join("next"), root.join("login")).unwrap();
}
fn setup() -> (tempfile::TempDir, HelperCommand) {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    login(dir.path(), X);
    let source = include_str!("../../../scripts/account-usage/claude_usage.py");
    let program = r#"
import json,sys,time
from pathlib import Path
scope={'__name__':'capture_fixture','__file__':'managed-claude.py'}
exec(compile(AAC_SOURCE_JSON,'managed-claude','exec'),scope)
root=Path(AAC_ROOT_JSON)
request=json.load(sys.stdin)
scope['sys'].platform='win32'
scope['resolve_profile_path']=lambda *args,**kwargs: root/'profile'
ORG='33333333-3333-3333-3333-333333333333'
CLIENT='22222222-2222-2222-2222-222222222222'
def decrypt(*args):
 account=(root/'login').read_text()
 key='acct:'+account+'|'+CLIENT+':'+ORG+':https://api.anthropic.com:user:inference user:profile'
 return account,{key:{'token':'fixture-token-'+account,'refreshToken':'fixture-refresh','expiresAt':4102444800000}}
scope['decrypt_cache']=decrypt
original_web=scope['verified_web_client']
def no_web(*args,**kwargs):
 if (root/'web-usage').exists():return original_web(*args,**kwargs)
 raise scope['UsageUnavailable']()
scope['verified_web_client']=no_web
scope['live_session_cookies']=lambda *args,**kwargs:[{'name':'sessionKey','value':'fixture-cookie'}]
class WebClient:
 def __init__(self,*args):pass
 def get(self,route):
  with (root/'web-reads').open('a') as f:f.write(route+'\n')
  if route=='/api/account':
   account='44444444-4444-4444-4444-444444444444' if (root/'web-other-account').exists() else (root/'login').read_text()
   org='44444444-4444-4444-4444-444444444444' if (root/'web-other-org').exists() else ORG
   return {'uuid':account,'email_address':'fixture@example.com','memberships':[{'organization':{'uuid':org}}]}
  if (root/'refill-usage.json').exists():return json.loads((root/'refill-usage.json').read_text())
  return {'five_hour':{'utilization':100},'seven_day':{'utilization':100},'limits':[{'kind':'weekly_scoped','group':'weekly','percent':37,'scope':{'model':{'display_name':'Fable'},'surface':None}}],'cedar_ember':{'eligible':True,'grants':[{'id':'fixture_grant','resets_left':1,'resets_total':1,'paused':False,'usable_now':True,'clears':['five_hour','seven_day','seven_day_overage_included']}]}}
scope['WebClient']=WebClient
class Client:
 def get(self,route,token):
  with (root/'provider-reads').open('a') as f:f.write(route+'\n')
  if (root/'signed-out').exists():raise scope['UsageUnavailable']('needs_sign_in')
  account=token[len('fixture-token-'):]
  if route=='/api/oauth/profile':return {'account':{'uuid':account,'email':'fixture@example.com','has_claude_pro':True},'organization':{'uuid':ORG}}
  if (root/'rate-limit').exists():raise scope['UsageUnavailable']('rate_limited',1800)
  if (root/'fail-usage').exists():raise scope['UsageUnavailable']()
  if (root/'hold-usage').exists():
   (root/'waiting').write_text('waiting')
   for _ in range(1000):
    if (root/'release').exists():break
    time.sleep(.005)
  return {'five_hour':{'utilization':100},'cedar_ember':{'eligible':True,'grants':[{'id':'fixture_grant','resets_left':1,'resets_total':1,'paused':False,'usable_now':True,'clears':['five_hour']}]}}
scope['Client']=Client
if request['action']=='read' and (root/'swap-before-read').exists():
 (root/'login').write_text('44444444-4444-4444-4444-444444444444')
print(json.dumps(scope['reset_host'](request),allow_nan=False))
"#.replace("AAC_SOURCE_JSON", &serde_json::to_string(source).unwrap())
    .replace("AAC_ROOT_JSON", &serde_json::to_string(dir.path().to_str().unwrap()).unwrap());
    let bootstrap = "import sys,json,io;p=json.loads(sys.stdin.buffer.read().decode('utf-8'));sys.stdin=io.StringIO(json.dumps(p['request']));exec(compile(p['helperSource'],'managed-fixture','exec'))";
    let command = HelperCommand {
        executable: "/usr/bin/python3".into(),
        args: vec!["-c".into(), bootstrap.into()],
        payload: json!({"helperSource":program,"request":{"profile":"reused","expectedEmail":"fixture@example.com","host":"windows:fixture","action":"read"}}),
    };
    (dir, command)
}
fn read(db: &Path, command: &HelperCommand) -> aac_status::reset::Snapshot {
    serde_json::from_value(reset_claude_source::read(db, "claude:reused", command).unwrap()).unwrap()
}
fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn nonce(n: u64) -> String {
    format!("{n:064x}")
}
#[test]
fn native_claude_capture_order_survives_both_response_delivery_orders() {
    for newer_first in [false, true] {
        let (dir, command) = setup();
        let db = dir.path().join("claims.sqlite");
        let mut store = Store::open(&db).unwrap();
        let (go, wait) = std::sync::mpsc::channel();
        let c = command.clone();
        let d = db.clone();
        // A was admitted earlier, but has not yet entered its native capture.
        let a = std::thread::spawn(move || {
            wait.recv().unwrap();
            read(&d, &c)
        });
        let older = read(&db, &command);
        login(dir.path(), Y);
        go.send(()).unwrap();
        let newer = a.join().unwrap();
        assert_eq!(older.identity.account, X);
        assert_eq!(newer.identity.account, Y);
        assert!(
            newer.read_ticket.as_ref().unwrap().sequence.parse::<u64>().unwrap()
                > older.read_ticket.as_ref().unwrap().sequence.parse::<u64>().unwrap()
        );
        if !newer_first {
            assert_eq!(
                store.inventory("claude:reused", &older, now(), &nonce(1)).unwrap_err(),
                "stale-inventory"
            );
        }
        let current = store.inventory("claude:reused", &newer, now(), &nonce(2)).unwrap();
        assert_eq!(
            store.inventory("claude:reused", &older, now(), &nonce(3)).unwrap_err(),
            "stale-inventory"
        );
        assert_eq!(
            store.pending("claude:reused", now()).unwrap()["evidence"],
            current["evidence"]
        );
    }
}
#[test]
fn changed_login_between_native_capture_and_pinned_read_makes_no_provider_call() {
    let (dir, command) = setup();
    let db = dir.path().join("claims.sqlite");
    Store::open(&db).unwrap();
    std::fs::write(dir.path().join("swap-before-read"), "").unwrap();
    assert_eq!(
        reset_claude_source::read(&db, "claude:reused", &command).unwrap_err(),
        "stale-inventory"
    );
    assert!(!dir.path().join("provider-reads").exists());
}
#[test]
fn claude_provider_io_releases_the_capture_lock_and_final_login_recheck_remains() {
    let (dir, command) = setup();
    let db = dir.path().join("claims.sqlite");
    Store::open(&db).unwrap();
    std::fs::write(dir.path().join("hold-usage"), "").unwrap();
    let c = command.clone();
    let d = db.clone();
    let worker = std::thread::spawn(move || reset_claude_source::read(&d, "claude:reused", &c));
    for _ in 0..1000 {
        if dir.path().join("waiting").exists() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    assert!(dir.path().join("waiting").exists());
    let mut store = Store::open(&db).unwrap();
    let (identity, _) = store
        .capture_read("claude:reused", || Ok("independent-capture"))
        .unwrap();
    assert_eq!(identity, "independent-capture");
    login(dir.path(), Y);
    std::fs::write(dir.path().join("release"), "").unwrap();
    assert_eq!(worker.join().unwrap().unwrap_err(), "stale-inventory");
}
#[test]
fn a_hung_capture_helper_times_out_without_leaving_a_lock_or_allocating_a_ticket() {
    let (dir, mut command) = setup();
    let db = dir.path().join("claims.sqlite");
    Store::open(&db).unwrap();
    command.payload["helperSource"] = json!("import time;time.sleep(20)");
    assert_eq!(
        reset_claude_source::read(&db, "claude:reused", &command).unwrap_err(),
        "credential-host-unavailable"
    );
    let mut store = Store::open(&db).unwrap();
    let (_, ticket) = store.capture_read("claude:reused", || Ok(())).unwrap();
    assert_eq!(ticket.sequence, "1");
}

#[test]
fn claude_failed_quota_read_rebinds_to_captured_account_and_recovers_after_restart() {
    let (dir, command) = setup();
    let db = dir.path().join("claims.sqlite");
    let mut store = Store::open(&db).unwrap();
    let x = read(&db, &command);
    let xi = store.inventory("claude:reused", &x, now(), &nonce(21)).unwrap();
    login(dir.path(), Y);
    std::fs::write(dir.path().join("fail-usage"), "").unwrap();
    assert!(reset_claude_source::read(&db, "claude:reused", &command).is_err());
    drop(store);
    let mut store = Store::open(&db).unwrap();
    let failed = store.pending("claude:reused", now()).unwrap();
    assert_ne!(failed["canonicalAccountKey"], xi["canonicalAccountKey"]);
    assert_eq!(failed["reason"], "account-changed-usage-unavailable");
    assert_eq!(failed["history"], json!([]));
    assert_eq!(
        store.inventory("claude:reused", &x, now(), &nonce(22)).unwrap_err(),
        "stale-inventory"
    );
    std::fs::remove_file(dir.path().join("fail-usage")).unwrap();
    let y = read(&db, &command);
    let yi = store.inventory("claude:reused", &y, now(), &nonce(23)).unwrap();
    assert_eq!(yi["canonicalAccountKey"], failed["canonicalAccountKey"]);
    assert!(store.pending("claude:reused", now()).unwrap()["reason"].is_null());
    std::fs::write(dir.path().join("fail-usage"), "").unwrap();
    assert!(reset_claude_source::read(&db, "claude:reused", &command).is_err());
    let same = store.pending("claude:reused", now()).unwrap();
    assert_eq!(same["canonicalAccountKey"], yi["canonicalAccountKey"]);
    assert!(same["reason"].is_null());
}

#[test]
fn web_inventory_reaches_claimable_exact_grants_without_oauth_usage() {
    let (dir, command) = setup();
    let db = dir.path().join("claims.sqlite");
    std::fs::write(dir.path().join("web-usage"), "").unwrap();
    std::fs::write(dir.path().join("rate-limit"), "").unwrap();
    let snapshot = read(&db, &command);
    let mut store = Store::open(&db).unwrap();
    let inventory = store.inventory("claude:reused", &snapshot, now(), &nonce(30)).unwrap();
    assert_eq!(inventory["grants"][0]["claimable"], true);
    assert_eq!(inventory["grants"][0]["kind"], "full");
    assert_eq!(snapshot.windows["seven_day_overage_included"], 37.0);
    assert_eq!(snapshot.grants[0].id.as_deref(), Some("fixture_grant"));
    assert_eq!(
        std::fs::read_to_string(dir.path().join("provider-reads")).unwrap(),
        "/api/oauth/profile\n"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("web-reads"))
            .unwrap()
            .lines()
            .count(),
        2
    );
}

#[test]
fn throttled_inventory_preserves_the_safe_reason_instead_of_stale_identity() {
    let (dir, command) = setup();
    std::fs::write(dir.path().join("rate-limit"), "").unwrap();
    assert_eq!(
        reset_claude_source::read(&dir.path().join("claims.sqlite"), "claude:reused", &command).unwrap_err(),
        "rate-limited"
    );
}

#[test]
fn signed_out_oauth_does_not_turn_web_cookies_into_a_claim_executor() {
    let (dir, command) = setup();
    std::fs::write(dir.path().join("signed-out"), "").unwrap();
    std::fs::write(dir.path().join("web-usage"), "").unwrap();
    assert_eq!(
        reset_claude_source::read(&dir.path().join("claims.sqlite"), "claude:reused", &command).unwrap_err(),
        "signed-out"
    );
    assert!(!dir.path().join("web-reads").exists());
}

#[test]
fn mismatched_web_identity_is_unconfirmed_even_when_oauth_usage_would_succeed() {
    for marker in ["web-other-account", "web-other-org"] {
        let (dir, command) = setup();
        std::fs::write(dir.path().join("web-usage"), "").unwrap();
        std::fs::write(dir.path().join(marker), "").unwrap();
        assert_eq!(
            reset_claude_source::read(&dir.path().join("claims.sqlite"), "claude:reused", &command).unwrap_err(),
            "identity-unconfirmed"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("provider-reads")).unwrap(),
            "/api/oauth/profile\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("web-reads")).unwrap(),
            "/api/account\n"
        );
    }
}

fn assert_refill_scope(name: &str) {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/reset-source/claude-refill-scopes.json"
    ))
    .unwrap();
    let case = cases
        .as_array()
        .unwrap()
        .iter()
        .find(|case| case["name"] == name)
        .unwrap();
    let expected = case["claimable"].as_bool().unwrap();
    let (dir, command) = setup();
    std::fs::write(dir.path().join("web-usage"), "").unwrap();
    let usage = json!({"five_hour":{"utilization":100}, "limits":[case["limit"].clone()],
        "cedar_ember":{"eligible":true,"grants":[{"id":"fixture_grant","resets_left":1,"resets_total":1,
            "paused":false,"usable_now":true,"clears":["seven_day_overage_included"],"gives_back_weekly":true}]}});
    std::fs::write(dir.path().join("refill-usage.json"), usage.to_string()).unwrap();
    let db = dir.path().join("claims.sqlite");
    let snapshot = read(&db, &command);
    let mut store = Store::open(&db).unwrap();
    let inventory = store.inventory("claude:reused", &snapshot, now(), &nonce(51)).unwrap();
    let grant = &inventory["grants"][0];
    let prepared = store.prepare(
        "claude:reused",
        grant["grantKey"].as_str().unwrap(),
        "local",
        &snapshot,
        now(),
        &nonce(52),
    );
    assert_eq!(grant["claimable"], expected, "{name}");
    assert_eq!(prepared.is_ok(), expected, "{name}");
    if !expected {
        assert_eq!(grant["claimBlockedReason"], "refill-windows-unconfirmed");
        assert_eq!(prepared.unwrap_err(), "refill-windows-unconfirmed");
    }
}
#[test]
fn astra_exact_model_control() {
    assert_refill_scope("exact-model");
}
#[test]
fn astra_known_surface_control() {
    assert_refill_scope("known-surface");
}
#[test]
fn astra_surface_only_is_not_fable_model() {
    assert_refill_scope("surface-only");
}
#[test]
fn astra_unknown_surface_is_not_unscoped() {
    assert_refill_scope("unknown-surface");
}
