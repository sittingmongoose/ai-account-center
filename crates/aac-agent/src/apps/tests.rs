use super::*;

const HARNESS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/spec/agent/app-inventory.py"
);

struct FixtureProbe {
    replies: Value,
    packages: Value,
}

impl Probe for FixtureProbe {
    async fn run(&self, program: &Path, argv: &[OsString], _seconds: u64) -> ProbeResult {
        let program = program.to_str().expect("fixture program name");
        let name = Path::new(program)
            .file_name()
            .and_then(OsStr::to_str)
            .expect("fixture command");
        let last = argv
            .last()
            .and_then(|arg| arg.to_str())
            .expect("fixture argument");
        let key = match name {
            "plutil" | "dpkg-query" => last,
            "powershell.exe" if argv.iter().any(|item| item == "-EncodedCommand") => {
                let bytes = STANDARD.decode(last).expect("base64 PowerShell script");
                let text: Vec<_> = bytes
                    .chunks_exact(2)
                    .map(|bytes| u16::from_le_bytes([bytes[0], bytes[1]]))
                    .collect();
                let script = String::from_utf16(&text).expect("UTF-16 PowerShell script");
                assert!(script.starts_with("[Console]::OutputEncoding="));
                if !script.contains("Get-AppxPackage") {
                    assert!(script.contains("cursor-agent.cmd") && script.contains("--version"));
                    return fixture_reply(&self.replies["cursor"], program, argv);
                }
                return ProbeResult::Output(
                    self.packages
                        .as_object()
                        .expect("package table")
                        .iter()
                        .find(|(name, _)| script.contains(&format!("'{name}'")))
                        .map(|(_, data)| data.to_string())
                        .unwrap_or_default(),
                );
            }
            "powershell.exe" if last == "--version" => {
                if argv
                    .iter()
                    .any(|arg| arg.to_string_lossy().ends_with(".muse-launcher.ps1"))
                {
                    "muse"
                } else {
                    "cursor"
                }
            }
            _ => {
                assert_eq!(argv, args(&["--version"]));
                program
            }
        };
        let reply = self.replies.get(key).or_else(|| {
            self.replies
                .as_object()?
                .iter()
                .find(|(path, _)| Path::new(path) == Path::new(key))
                .map(|(_, value)| value)
        });
        fixture_reply(reply.unwrap_or(&Value::Null), program, argv)
    }
}

fn fixture_reply(reply: &Value, program: &str, argv: &[OsString]) -> ProbeResult {
    match reply {
        Value::String(text) => ProbeResult::Output(text.clone()),
        value if value["timeout"] == true => ProbeResult::Timeout,
        value if value.is_object() => {
            ProbeResult::Failed(value["failure"].as_str().unwrap_or_default().to_owned())
        }
        _ => panic!("unexpected probe {program} {argv:?}"),
    }
}

async fn parity(platform: Platform) {
    for case in ["empty", "populated", "degraded"] {
        let root = tempfile::tempdir().expect("temporary app fixture");
        let python = std::env::var_os("AAC_TEST_PYTHON").unwrap_or_else(|| {
            if cfg!(windows) {
                "python".into()
            } else {
                "python3".into()
            }
        });
        let result = std::process::Command::new(python)
            .args(["-B", HARNESS])
            .arg(root.path())
            .args([platform.name(), case])
            .output()
            .expect("run Python reference");
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
        let data: Value = serde_json::from_slice(&result.stdout).expect("reference payload");
        let root = Path::new(data["root"].as_str().expect("fixture root"));
        let host = Host {
            platform,
            home: root.join("home"),
            local: root.join("local"),
            roaming: root.join("roaming"),
            program_files: root.join("program-files"),
            path: data["path"]
                .as_array()
                .expect("fixture PATH")
                .iter()
                .map(|path| PathBuf::from(path.as_str().expect("path")))
                .collect(),
            path_ext: vec![".EXE".into(), ".CMD".into(), ".PS1".into()],
            powershell: Some(root.join("Windows/powershell.exe")),
            npm_prefix: None,
            arch: if platform == Platform::Mac {
                "aarch64"
            } else {
                "x86_64"
            },
            system_root: Some(root.join("system")),
        };
        let fake = FixtureProbe {
            replies: data["probes"].clone(),
            packages: data["packages"].clone(),
        };
        let actual = inventory(&host, &fake).await;
        let expected = data["expected"].as_str().expect("expected bytes");
        assert_eq!(actual.to_string(), expected, "{} {case}", platform.name());
    }
}

#[tokio::test]
async fn linux_inventory_matches_python() {
    parity(Platform::Ubuntu).await;
}

#[tokio::test]
async fn mac_inventory_matches_python() {
    parity(Platform::Mac).await;
}

#[tokio::test]
async fn windows_inventory_matches_python() {
    parity(Platform::Windows).await;
}

#[tokio::test]
async fn rejects_callers_selecting_a_path_or_foreign_platform() {
    for invalid in [
        json!({"action":"inventory","platform":"windows","path":"fixture"}),
        json!({"action":"inventory","platform":"nas1"}),
        json!({"action":"inventory","platform":null}),
        json!({"action":"inventory","platform":if cfg!(windows) { "mac" } else { "windows" }}),
    ] {
        assert_eq!(
            run_inventory(&invalid).await.expect_err("refused").cause,
            Cause::Refused
        );
    }
}

#[test]
fn job_ids_cannot_select_a_path() {
    assert!(valid_job("cdc2d248a04f48a894be80bd777d866d"));
    for invalid in ["", "../../fixture", "cDc2d248a04f48a894be80bd777d866d"] {
        assert!(!valid_job(invalid));
    }
}
