//! M5 lane K: the private service's claims-file call is the native ABI with a server-owned path.
//! The same request sequence on twin ledgers gives the same answers through `service_call` and
//! through `dispatch` (the napi `resetClaims`), and the service refuses a path from the wire.
mod support;
use aac_status::reset::*;
use serde_json::{Value, json};
use std::path::Path;
use support::ReadFixtures;

const ACCOUNT: &str = "codex:route-account";

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn entropy(n: u8) -> String {
    format!("{n:064x}")
}
fn grant(id: &str, left: u32) -> Grant {
    Grant {
        id: Some(id.into()),
        kind: "banked".into(),
        clears: vec!["seven_day".into()],
        left,
        expires_at_ms: None,
        usable: true,
        words: None,
    }
}
/// A snapshot carrying a fresh read ticket from `db`, as the capture transaction issues one.
fn snapshot(db: &Path, left_b: u32, used: f64) -> Value {
    let mut store = Store::open(db).unwrap();
    let ticket = store.captured_ticket(ACCOUNT).unwrap();
    serde_json::to_value(Snapshot {
        read_ticket: Some(ticket),
        identity: Identity {
            provider: "codex".into(),
            account: "synthetic-account".into(),
            organization: String::new(),
            profile: "synthetic-profile".into(),
            host: "fixture-host".into(),
        },
        observed_at_ms: wall(),
        grants: vec![grant("grant_a", 2), grant("grant_b", left_b)],
        windows: std::collections::BTreeMap::from([("seven_day".into(), used)]),
        executor_ready: true,
        source_binding: "synthetic-binding".into(),
        account_label: "synthetic-principal".into(),
    })
    .unwrap()
}

/// One ledger. `service` sends the service body; otherwise the native ABI with `dbPath`.
struct Ledger {
    _dir: tempfile::TempDir,
    db: std::path::PathBuf,
    service: bool,
}
impl Ledger {
    fn new(service: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let db = dir.path().join("claims.sqlite");
        Self { _dir: dir, db, service }
    }
    fn call(&self, op: &str, n: u8, args: Value) -> std::result::Result<Value, &'static str> {
        let mut body = json!({"op": op, "account": ACCOUNT, "nowMs": wall(), "entropy": entropy(n)});
        for (k, v) in args.as_object().unwrap() {
            body[k] = v.clone();
        }
        if self.service {
            service_call(&self.db, body.to_string().as_bytes())
        } else {
            body["dbPath"] = json!(self.db.to_str().unwrap());
            dispatch(&body.to_string())
        }
    }
}

/// Clock readings differ between the twins by the time between calls; everything else is equal.
fn timeless(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, item) in map.iter_mut() {
                if key.ends_with("Ms") && item.is_number() {
                    *item = json!(0);
                } else {
                    timeless(item);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(timeless),
        _ => {}
    }
}

fn run(ledger: &Ledger) -> Vec<std::result::Result<Value, &'static str>> {
    let caller = format!("session:{}", entropy(20));
    let mut out = vec![ledger.call("pending", 1, json!({}))];
    let inventory = ledger.call("inventory", 2, json!({"snapshot": snapshot(&ledger.db, 1, 100.0)}));
    let key = inventory.as_ref().unwrap()["grants"][1]["grantKey"]
        .as_str()
        .unwrap()
        .to_owned();
    out.push(inventory);
    out.push(ledger.call("prepareGate", 3, json!({"key": key})));
    let prepared = ledger.call(
        "prepare",
        4,
        json!({"key": key, "caller": caller, "snapshot": snapshot(&ledger.db, 1, 100.0)}),
    );
    let token = prepared.as_ref().unwrap()["confirmationToken"]
        .as_str()
        .unwrap()
        .to_owned();
    out.push(prepared);
    out.push(ledger.call("claimGate", 5, json!({"key": key})));
    // A wrong token is refused with the same fixed code on both sides.
    out.push(ledger.call(
        "begin",
        6,
        json!({"key": key, "caller": caller, "token": entropy(99), "snapshot": snapshot(&ledger.db, 1, 100.0)}),
    ));
    let prepared = ledger.call(
        "prepare",
        7,
        json!({"key": key, "caller": caller, "snapshot": snapshot(&ledger.db, 1, 100.0)}),
    );
    let token = prepared
        .as_ref()
        .map(|p| p["confirmationToken"].as_str().unwrap_or(&token).to_owned())
        .unwrap_or(token);
    out.push(prepared);
    let write = ledger.call(
        "begin",
        8,
        json!({"key": key, "caller": caller, "token": token, "snapshot": snapshot(&ledger.db, 1, 100.0)}),
    );
    let attempt = write.as_ref().unwrap()["attemptKey"].as_str().unwrap().to_owned();
    out.push(write);
    out.push(ledger.call("pending", 9, json!({})));
    out.push(ledger.call(
        "finish",
        10,
        json!({"key": attempt, "outcome": "reset", "retryAfterMs": null, "snapshot": snapshot(&ledger.db, 0, 0.0)}),
    ));
    out.push(ledger.call(
        "check",
        11,
        json!({"key": attempt, "snapshot": snapshot(&ledger.db, 0, 0.0)}),
    ));
    out.push(ledger.call("pending", 12, json!({})));
    out.push(ledger.call("inventory", 13, json!({"snapshot": snapshot(&ledger.db, 0, 0.0)})));
    // A snapshot without a read ticket never establishes inventory authority.
    let mut unticketed = snapshot(&ledger.db, 0, 0.0);
    unticketed["readTicket"] = Value::Null;
    out.push(ledger.call("inventory", 14, json!({"snapshot": unticketed})));
    out
}

#[test]
fn service_answers_equal_the_native_abi_through_a_whole_claim() {
    let native = run(&Ledger::new(false));
    let service = run(&Ledger::new(true));
    assert_eq!(native.len(), service.len());
    for (step, (mut a, mut b)) in native.into_iter().zip(service).enumerate() {
        if let (Ok(a), Ok(b)) = (&mut a, &mut b) {
            timeless(a);
            timeless(b);
        }
        assert_eq!(a, b, "step {step}");
    }
}

#[test]
fn the_service_refuses_a_wire_path_unknown_ops_and_unbound_entropy() {
    let ledger = Ledger::new(true);
    let call = |body: Value| service_call(&ledger.db, body.to_string().as_bytes());
    let ok = json!({"op":"pending","account":ACCOUNT,"nowMs":1,"entropy":entropy(1)});
    assert!(call(ok.clone()).is_ok());
    let mut with_path = ok.clone();
    with_path["dbPath"] = json!("/elsewhere/claims.sqlite");
    assert_eq!(call(with_path), Err("shape"));
    for op in ["history", "resetClaims", "sourceCodexWrite", ""] {
        let mut body = ok.clone();
        body["op"] = json!(op);
        assert_eq!(call(body), Err("shape"), "{op}");
    }
    for bad in [json!("short"), json!(null), json!(7)] {
        let mut body = ok.clone();
        body["entropy"] = bad;
        assert_eq!(call(body), Err("shape"));
    }
    assert_eq!(service_call(&ledger.db, b"[]"), Err("shape"));
    assert_eq!(service_call(&ledger.db, b"not json"), Err("shape"));
    // The native ABI rejects unknown fields; the service passes them through to that same check.
    let mut extra = ok;
    extra["unexpected"] = json!(true);
    assert_eq!(call(extra), Err("shape"));
}

#[test]
fn ticketed_source_reads_take_exact_fields_and_reach_the_native_source() {
    let ledger = Ledger::new(true);
    let call = |body: Value| service_call(&ledger.db, body.to_string().as_bytes());
    let codex = json!({"op":"sourceCodexRead","routeAccount":ACCOUNT,"home":"/nonexistent/aac-fixture",
        "expectedAccount":"account-x","profile":"fixture","host":"fixture-host"});
    // A write never goes through the service; neither do extra or missing fields.
    for edit in ["write", "dbPath", "extra"] {
        let mut body = codex.clone();
        body[edit] = json!(null);
        assert_eq!(call(body), Err("shape"), "{edit}");
    }
    let mut missing = codex.clone();
    missing.as_object_mut().unwrap().remove("host");
    assert_eq!(call(missing), Err("shape"));
    // Same input, same outcome as the native `resetCodexSource` with this ledger's path.
    let native = |mut body: Value| {
        body.as_object_mut().unwrap().remove("op");
        body["write"] = Value::Null;
        body["dbPath"] = json!(ledger.db.to_str().unwrap());
        aac_status::reset_source::dispatch(&body.to_string())
    };
    let mut unconfirmed = codex.clone();
    unconfirmed["expectedAccount"] = json!("");
    assert_eq!(call(unconfirmed.clone()), Err("identity-unconfirmed"));
    assert_eq!(native(unconfirmed), Err("identity-unconfirmed"));
    assert_eq!(call(codex.clone()), native(codex));

    let claude = json!({"op":"sourceClaudeRead","routeAccount":"claude:fixture",
        "command":{"executable":"/nonexistent/aac-fixture-ssh","args":[],"payload":{"request":"not-an-object"}}});
    let mut extra = claude.clone();
    extra["write"] = json!(null);
    assert_eq!(call(extra), Err("shape"));
    let native_claude = |mut body: Value| {
        body.as_object_mut().unwrap().remove("op");
        body["dbPath"] = json!(ledger.db.to_str().unwrap());
        aac_status::reset_claude_source::dispatch(&body.to_string())
    };
    assert_eq!(call(claude.clone()), Err("shape"));
    assert_eq!(call(claude.clone()), native_claude(claude));
}
