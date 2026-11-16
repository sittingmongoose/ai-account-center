//! Search permission is enough to discover a configured profile through the real NDJSON verb.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{fs, os::unix::fs::PermissionsExt};

async fn signin_with_userdata_mode(mode: u32) -> Value {
    let home = tempfile::tempdir().expect("fixture home");
    let userdata = home.path().join(".t3/userdata");
    fs::create_dir_all(&userdata).expect("fixture settings directory");
    let custom = home.path().join("custom-home");
    fs::create_dir(&custom).expect("fixture configured profile");
    let settings = userdata.join("settings.json");
    fs::write(
        &settings,
        json!({"providerInstances":{"fixture":{"driver":"claudeAgent","config":{"homePath":custom}}}})
            .to_string(),
    )
    .expect("fixture settings");
    fs::set_permissions(&userdata, fs::Permissions::from_mode(mode)).expect("fixture mode");
    // Confirm the ordinary named read still works for search-only directories. Restore before
    // asserting, including an agent error, so the private fixture can always be removed.
    let reference_can_read = fs::read(&settings).is_ok();
    let payload = json!({"protocol":1,"verb":"signin-expiry","id":"review","deadlineMs":5000,
        "args":{"home":home.path(),"nowMs":1800000000123_i64}})
    .to_string()
        + "\n";
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output).await;
    fs::set_permissions(&userdata, fs::Permissions::from_mode(0o700)).expect("restore fixture");
    assert_eq!(reference_can_read, mode != 0, "fixture permission control");
    assert_eq!(code.expect("NDJSON dispatch"), 0);
    let event: Value = serde_json::from_slice(&output).expect("one terminal NDJSON event");
    assert_eq!(event["type"], "result");
    assert_eq!(event["id"], "review");
    event["value"].clone()
}

#[tokio::test]
async fn search_only_ancestors_keep_the_configured_signin_profile() {
    if aac_files::path::uid() == 0 {
        return; // Root bypasses the fixture's directory permission control.
    }
    for mode in [0o300, 0o100] {
        let value = signin_with_userdata_mode(mode).await;
        // Platforms without a search-capable descriptor must label a whole-verb decline.
        // Linux and the supported O_SEARCH platforms must actually read the fixture.
        if !cfg!(any(
            target_os = "linux",
            target_os = "android",
            target_vendor = "apple",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "solaris",
            target_os = "illumos"
        )) && value == json!({"fallback":"python"})
        {
            continue;
        }
        let profiles = value["profiles"].as_array().expect("proven inventory");
        assert!(
            profiles
                .iter()
                .any(|p| p["name"] == "custom-home" && p["configDir"] == "~/custom-home"),
            "mode {mode:o}: {value}"
        );
        assert!(!profiles.iter().any(|p| p["name"] == "saved-profiles"));
    }
}

#[tokio::test]
async fn permission_denied_in_the_chain_declines_the_whole_signin_verb() {
    if aac_files::path::uid() == 0 {
        return;
    }
    assert_eq!(
        signin_with_userdata_mode(0).await,
        json!({"fallback":"python"})
    );
}
