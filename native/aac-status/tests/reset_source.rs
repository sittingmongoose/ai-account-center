//! No real credentials, network, refresh or installed provider is used here.
#[cfg(unix)]
#[test]
fn codex_appserver_fixture_binds_profile_account_and_exact_credit() {
    use base64::Engine;
    use serde_json::json;
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
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
    let script = r##"#!/usr/bin/python3
import sys,json,os
for line in sys.stdin:
 r=json.loads(line)
 if 'id' not in r: continue
 method=r['method']
 if method=='initialize': result={}
 elif method=='account/read': result={'account':{'type':'chatgpt','email':'synthetic-principal'}}
 elif method=='account/rateLimits/read':
  who=os.path.join(os.environ['CODEX_HOME'],'usage-account.json')
  usage_account=json.load(open(who)) if os.path.exists(who) else {'accountId':'synthetic-account'}
  result={**usage_account,'rateLimits':{'primary':{'usedPercent':100}},'rateLimitResetCredits':{'availableCount':2,'credits':[{'id':'individual_a','resetType':'codexRateLimits','status':'available'},{'id':'individual_b','resetType':'codexRateLimits','status':'available'}]}}
 elif method=='account/rateLimitResetCredit/consume':
  with open(os.path.join(os.environ['CODEX_HOME'],'sent.json'),'w') as f: json.dump(r['params'],f)
  result={'outcome':'reset'}
 else: result={}
 print(json.dumps({'id':r['id'],'result':result}),flush=True)
"##;
    std::fs::write(&binary, script).unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o700)).unwrap();
    let read = aac_status::reset_source::codex_with_binary(
        home.to_str().unwrap(),
        "exact-profile",
        "fixture-host",
        None,
        &binary,
    )
    .unwrap();
    assert!(!home.join("sent.json").exists());
    assert_eq!(read["grants"][1]["id"], "individual_b");
    let mut write = json!({"identity":read["identity"],"sourceBinding":read["sourceBinding"],"method":"account/rateLimitResetCredit/consume","body":{"creditId":"individual_b","idempotencyKey":"a".repeat(64)}});
    write["identity"]["profile"] = json!("other-profile");
    assert!(
        aac_status::reset_source::codex_with_binary(
            home.to_str().unwrap(),
            "exact-profile",
            "fixture-host",
            Some(&write),
            &binary
        )
        .is_err()
    );
    assert!(!home.join("sent.json").exists());
    write["identity"]["profile"] = json!("exact-profile");
    let result = aac_status::reset_source::codex_with_binary(
        home.to_str().unwrap(),
        "exact-profile",
        "fixture-host",
        Some(&write),
        &binary,
    )
    .unwrap();
    assert_eq!(result["outcome"], "reset");
    let sent: serde_json::Value = serde_json::from_slice(&std::fs::read(home.join("sent.json")).unwrap()).unwrap();
    assert_eq!(sent["creditId"], "individual_b");
    assert_eq!(sent["idempotencyKey"], "a".repeat(64));

    // The usage read must name the saved account. A different or missing accountId refuses the read and the claim
    // before any consume request, even though the saved login and account/read still agree.
    std::fs::remove_file(home.join("sent.json")).unwrap();
    for usage_account in [
        json!({"accountId":"other-account"}),
        json!({}),
        json!({"accountId":null}),
    ] {
        std::fs::write(home.join("usage-account.json"), usage_account.to_string()).unwrap();
        let read = aac_status::reset_source::codex_with_binary(
            home.to_str().unwrap(),
            "exact-profile",
            "fixture-host",
            None,
            &binary,
        );
        assert_eq!(read.unwrap_err(), "identity-unconfirmed", "usage {usage_account}");
        let claim = aac_status::reset_source::codex_with_binary(
            home.to_str().unwrap(),
            "exact-profile",
            "fixture-host",
            Some(&write),
            &binary,
        );
        assert_eq!(claim.unwrap_err(), "identity-unconfirmed", "usage {usage_account}");
        assert!(!home.join("sent.json").exists(), "no consume for usage {usage_account}");
    }
}
