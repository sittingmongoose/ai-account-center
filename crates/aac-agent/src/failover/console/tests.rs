use crate::failover_additional::{desktop, muse};
use crate::failover_special::wallet as opencode_console;
use serde_json::{Value, json};
use std::io::Write;
use std::path::Path;
fn oracle(module: &str, function: &str, arg: &Value) -> Value {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let home = tempfile::tempdir().expect("home");
    let mut child=std::process::Command::new("python3").arg("-c").arg("import sys,json,importlib;sys.path[:0]=sys.argv[1:3];m=importlib.import_module(sys.argv[3]);a=json.load(sys.stdin);print(json.dumps(getattr(m,sys.argv[4])(a)))").arg(root.join("scripts/account-usage")).arg(root.join("browser-bridge/opencode-muse/native-host")).arg(module).arg(function).env("HOME",home.path()).env("CCS_HOME",home.path().join(".ccs")).env("PYTHONDONTWRITEBYTECODE","1").stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().expect("oracle");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&serde_json::to_vec(arg).expect("arg"))
        .expect("write");
    let output = child.wait_with_output().expect("output");
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).expect("json")
}
fn numeric(v: Value) -> Value {
    match v {
        Value::Number(n) => json!(n.as_f64()),
        Value::Array(a) => json!(a.into_iter().map(numeric).collect::<Vec<_>>()),
        Value::Object(o) => Value::Object(o.into_iter().map(|(k, v)| (k, numeric(v))).collect()),
        v => v,
    }
}
#[test]
fn wallet_debt_matches_python() {
    for b in ["-150000000", "0", "99999999999999999999"] {
        let v = json!({"mode":"pay-as-you-go","billingMode":"prepaid","balanceMicroCents":b,"expiresAt":"2027-01-01T00:00:00Z"});
        assert_eq!(
            numeric(opencode_console::balance_window(&v).expect("wallet")),
            numeric(oracle("opencode_console", "balance_window", &v))
        );
    }
}
#[test]
fn go_meters_match_python() {
    for v in [
        json!({}),
        json!({"access":{"endsAt":"2027-01-01T00:00:00Z","meters":{"fiveHour":{"usedMicroCents":"120","limitMicroCents":"100"},"week":{"usedMicroCents":"3","limitMicroCents":"0"},"month":{"usedMicroCents":"2","limitMicroCents":"3"}}}}),
    ] {
        assert_eq!(
            numeric(json!(opencode_console::go_windows(&v))),
            numeric(oracle("opencode_console", "go_windows", &v))
        );
    }
}
#[test]
fn muse_windows_match_python() {
    for v in [
        json!({}),
        json!({"subscription_quota":{"window_weighted_used":"12.5","window_weighted_limit":"10","window_duration_secs":18000,"window_resets_at":"1800000000","weekly_weighted_used":0,"weekly_weighted_limit":500}}),
        json!({"subscription_quota":{"window_weighted_used":true,"window_resets_at":"2027-01-01T00:00:00Z"}}),
        json!({"subs_usage":{"window":{"used_percent":123.456789,"window_duration_mins":300,"unlimited":false},"weekly":{"used_percent":0,"enabled":true}}}),
    ] {
        assert_eq!(
            numeric(json!(desktop::normalize_muse(&v))),
            numeric(oracle("desktop_usage", "normalize_muse", &v)),
            "{v}"
        );
    }
}
fn cookie() -> Value {
    json!([{"name":"llm_sess","value":"synthetic-session","domain":"dev.meta.ai","path":"/","secure":true,"hostOnly":true}])
}
#[test]
fn cookie_validation_matches_python() {
    let v = cookie();
    assert_eq!(
        json!(muse::validate_cookies(&v, 1.0).expect("cookies")),
        oracle("muse_console", "validate_cookies", &v)
    );
}
