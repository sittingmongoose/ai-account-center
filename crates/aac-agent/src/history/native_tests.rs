//! Synthetic native history fixtures, including the optional retained-source oracle.

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
#[ignore = "requires the retained Python oracle; run explicitly before legacy source deletion"]
fn python_snapshot_byte_parity() {
    use std::io::Write;
    for platform in ["mac", "windows"] {
        let args = fixture(platform);
        let native = run(&args).expect("native snapshot");
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/history-collection-oracle.py");
        let mut child =
            std::process::Command::new(if cfg!(windows) { "python" } else { "python3" })
                .arg(script)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("offline Python oracle");
        child
            .stdin
            .take()
            .expect("oracle input")
            .write_all(args.to_string().as_bytes())
            .expect("fixture request");
        let output = child.wait_with_output().expect("oracle result");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let expected: Value = serde_json::from_slice(&output.stdout).expect("oracle snapshot");
        assert_eq!(native, expected, "{platform}");
    }
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
