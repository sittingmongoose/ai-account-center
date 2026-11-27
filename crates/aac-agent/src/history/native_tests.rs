#![cfg(test)]
//! Synthetic native history fixtures for collection, append and candidate guards.

use super::*;
fn fixture(platform: &str) -> Value {
    let home = tempfile::tempdir().expect("fixture home").keep();
    let profile = if platform == "mac" { "platyr" } else { "gmail" };
    let account = "12345678-1234-1234-1234-123456789abc";
    let org = "abcdefab-abcd-abcd-abcd-abcdefabcdef";
    let root = if platform == "mac" {
        home.join("Library/Application Support/Claude")
    } else {
        home.join("AppData/Local/Packages/Claude_pzs8sxrjxfjjc/LocalCache/Roaming/Claude")
    };
    fs::create_dir_all(root.join("claude-code-sessions").join(account).join(org))
        .expect("registry");
    fs::create_dir_all(home.join(".ssh")).expect("ssh config folder");
    fs::write(
        home.join(".ssh/config"),
        "Host fixture\n HostName fixture.invalid\n User fixture\n Port 22\n",
    )
    .expect("fixed endpoint");
    fs::write(
        root.join("config.json"),
        json!({"lastKnownAccountUuid":account}).to_string(),
    )
    .expect("account bind");
    fs::write(
        root.join("ssh_configs.json"),
        json!({"configs":[{"sshHost":"fixture"}]}).to_string(),
    )
    .expect("app endpoint");
    let endpoint =
        json!({"alias":"fixture","hostname":"fixture.invalid","username":"fixture","port":22});
    json!({"mode":"collect","profileId":profile,"platform":platform,"policy":{"version":1,"enabled":true,"sourcePlatform":if profile=="platyr"{"mac"}else{"windows"},"identity":{"accountUuid":account,"organizationUuid":org},"project":{"cwd":"/mnt/Cursor/PuppetMaster","originCwd":"/mnt/Cursor/PuppetMaster","transcriptRoot":"/fixture/transcripts"},"ssh":{"mac":endpoint,"windows":endpoint}},"_historyFixture":{"home":home,"closed":true,"native":{"warmGuardVerified":true,"autoResumeGuardVerified":true},"transcriptsVerified":true}})
}
#[test]
fn native_append_and_candidate_guard_run_on_both_platform_fixtures() {
    for platform in ["mac", "windows"] {
        let mut args = fixture(platform);
        let expected = run(&args).expect("bound collection");
        assert!(expected["records"].is_array(), "{expected}");
        let id = "11111111-1111-1111-1111-111111111111";
        let value = json!({"sessionId":format!("local_{id}"),"cliSessionId":id,"cwd":"/mnt/Cursor/PuppetMaster","originCwd":"/mnt/Cursor/PuppetMaster","createdAt":0.125,"lastActivityAt":0.125,"lastFocusedAt":0,"title":"Synthetic café","isArchived":false,"permissionMode":"default","importedFrom":"local-1p-code","resumeConfirmed":false,"remoteControlAutoEligible":false,"sshConfig":{"sshHost":"fixture"},"sshRemoteTranscriptPath":format!("/fixture/transcripts/project/{id}.jsonl")});
        let raw = serde_json::to_vec(&value).expect("record JSON");
        args["expectedTarget"] = expected;
        args["records"] = json!([{"name":format!("local_{id}.json"),"base64":STANDARD.encode(&raw),"sha256":hash(&raw)}]);
        args["mode"] = "append".into();
        let receipt = run(&args).expect("native append receipt");
        assert_eq!(
            receipt["status"], "created_metadata",
            "{platform}: {receipt}"
        );
        assert_eq!(receipt["createdCount"], 1);
        assert_eq!(receipt["recoveryRequired"], false);
        assert_eq!(receipt["writerQuiescent"], true);
        args["mode"] = "protected-check".into();
        assert_eq!(
            run(&args).expect("candidate-aware recheck"),
            json!({"unchanged":true})
        );
        args["mode"] = "append".into();
        let second = run(&args).expect("create-only refusal receipt");
        assert_eq!(second["status"], "refused");
        assert_eq!(second["createdCount"], 0);
    }
}
