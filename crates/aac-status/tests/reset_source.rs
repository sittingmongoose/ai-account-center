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

/// The saved login read (L2 one file policy) keeps its old answers: missing or corrupt is signed
/// out, a link, FIFO, folder or oversized file is an unsafe profile, and a FIFO never blocks.
#[cfg(unix)]
#[test]
fn codex_saved_login_read_keeps_absence_and_corruption_answers() {
    use base64::Engine;
    use serde_json::json;
    let dir = tempfile::tempdir().unwrap();
    // No app server is ever started: every case below is answered by the saved-login read, and a
    // good login reaches the (missing) binary.
    let binary = dir.path().join("no-such-app-server");
    let read = |home: &std::path::Path| {
        aac_status::reset_source::codex_with_binary(
            home.to_str().unwrap(),
            "exact-profile",
            "fixture-host",
            None,
            &binary,
        )
        .unwrap_err()
    };
    let profile = |name: &str| {
        let home = dir.path().join(name);
        std::fs::create_dir(&home).unwrap();
        home
    };
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        json!({"email":"synthetic-principal","https://api.openai.com/auth":{"chatgpt_account_id":"synthetic-account"}})
            .to_string(),
    );
    // Generated protocol material only: not a stored credential fixture.
    let login =
        json!({"tokens":{"account_id":"synthetic-account","id_token":format!("e30.{claims}.fixture")}}).to_string();

    let good = profile("good");
    std::fs::write(good.join("auth.json"), &login).unwrap();
    assert_eq!(read(&good), "app-server-unavailable");

    assert_eq!(read(&profile("absent")), "signed-out");

    let corrupt = profile("corrupt");
    std::fs::write(corrupt.join("auth.json"), b"{\"tokens\":").unwrap();
    assert_eq!(read(&corrupt), "signed-out");

    let linked = profile("linked");
    std::os::unix::fs::symlink(good.join("auth.json"), linked.join("auth.json")).unwrap();
    assert_eq!(read(&linked), "unsafe-profile");

    let folder = profile("folder");
    std::fs::create_dir(folder.join("auth.json")).unwrap();
    assert_eq!(read(&folder), "unsafe-profile");

    let large = profile("large");
    std::fs::write(large.join("auth.json"), vec![b' '; 1024 * 1024 + 1]).unwrap();
    assert_eq!(read(&large), "unsafe-profile");
    let full = profile("full");
    let mut padded = login.clone().into_bytes();
    padded.resize(1024 * 1024, b' ');
    std::fs::write(full.join("auth.json"), padded).unwrap();
    assert_eq!(read(&full), "app-server-unavailable");

    let fifo = profile("fifo");
    let made = std::process::Command::new("mkfifo")
        .arg(fifo.join("auth.json"))
        .status()
        .unwrap();
    assert!(made.success());
    let (tx, rx) = std::sync::mpsc::channel();
    let fifo_home = fifo.clone();
    let fifo_binary = binary.clone();
    std::thread::spawn(move || {
        let answer = aac_status::reset_source::codex_with_binary(
            fifo_home.to_str().unwrap(),
            "exact-profile",
            "fixture-host",
            None,
            &fifo_binary,
        )
        .unwrap_err();
        let _ = tx.send(answer);
    });
    let answer = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("a FIFO at the saved login must not block the read");
    assert_eq!(answer, "unsafe-profile");
}
