#![cfg(test)]
use super::*;
use aac_config::{ComputerConfig, TransportConfig};
use aac_core::ComputerId;
use aac_transport::SshPolicy;
use std::path::PathBuf;

const STAND_IN: &str = "import argparse\nparser = argparse.ArgumentParser()\nparser.add_argument('--platform', required=True)\nparser.add_argument('--apply', action='store_true')\nparser.parse_args()\n";

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A package with the checkout's helpers and a stand-in for the generated Codex runtime.
fn package(root: &Path) -> PathBuf {
    let package = root.join("package");
    let scripts = package.join("scripts/app-updates");
    let dist = package.join("dist/app-updates");
    std::fs::create_dir_all(&scripts).expect("package helpers");
    std::fs::create_dir_all(&dist).expect("package runtime");
    for name in HELPER_FILES.iter().filter(|name| **name != RUNTIME) {
        std::fs::copy(
            repo().join("scripts/app-updates").join(name),
            scripts.join(name),
        )
        .expect("copy helper");
    }
    std::fs::write(dist.join(RUNTIME), "'use strict';\n// fixture runtime\n").expect("runtime");
    package
}

fn output(code: i32, stdout: &str) -> ProcessOutput {
    ProcessOutput {
        code: Some(code),
        stdout: stdout.as_bytes().to_vec(),
        stderr: Vec::new(),
    }
}

#[test]
fn payload_carries_the_script_manifest_and_files_in_order() {
    let dir = tempfile::tempdir().expect("fixture");
    let set = HelperSet::load(&package(dir.path())).expect("helper set");
    let payload = set.payload("mac");
    let mut lines = payload.splitn(3, |byte| *byte == b'\n');
    let script = STANDARD
        .decode(lines.next().expect("script"))
        .expect("base64");
    assert_eq!(script, SCRIPT.as_bytes());
    let header: Value = serde_json::from_slice(lines.next().expect("manifest")).expect("json");
    assert_eq!(header["platform"], "mac");
    let mut body = lines.next().expect("files");
    let files = header["files"].as_array().expect("files");
    assert_eq!(files.len(), HELPER_FILES.len());
    for (entry, name) in files.iter().zip(HELPER_FILES) {
        assert_eq!(entry["name"], name);
        let size = entry["size"].as_u64().expect("size") as usize;
        assert_eq!(aac_core::sha256_hex(&body[..size]), entry["sha256"]);
        body = &body[size..];
    }
    assert!(body.is_empty());

    std::fs::remove_file(dir.path().join("package/dist/app-updates").join(RUNTIME))
        .expect("drop runtime");
    assert_eq!(
        HelperSet::load(&dir.path().join("package")).err(),
        Some(failed("helper_set_missing"))
    );
}

#[test]
fn replies_map_to_honest_causes() {
    assert_eq!(
        interpret(&output(0, "{\"ok\":true,\"changed\":false}\n")),
        Ok(())
    );
    for (reason, cause) in [
        ("helper_upload_damaged", "unknown"),
        ("helper_capability_missing", "unknown"),
        ("helper_not_writable", "not_writable"),
    ] {
        let reply = format!("{{\"ok\":false,\"reason\":\"{reason}\"}}\n");
        assert_eq!(
            interpret(&output(0, &reply)),
            Err(SyncError::Failed { reason, cause })
        );
    }
    assert_eq!(
        interpret(&output(0, "{\"ok\":false,\"reason\":\"PRIVATE\"}")),
        Err(failed("helper_reply_invalid"))
    );
    assert_eq!(
        interpret(&output(127, "")),
        Err(failed("helper_python_missing"))
    );
    assert_eq!(
        interpret(&output(1, "{\"ok\":true}")),
        Err(failed("helper_reply_invalid"))
    );
}

/// The real script on this computer in an isolated home, through the local transport the
/// dashboard uses for its own computer. A child process owns the isolated HOME.
#[cfg(unix)]
#[test]
fn local_sync_publishes_a_capable_generation_and_keeps_the_previous_one() {
    let dir = tempfile::tempdir().expect("fixture");
    let root = dir.path().canonicalize().expect("canonical root");
    std::fs::create_dir(root.join("home")).expect("home");
    let mut command = std::process::Command::new(std::env::current_exe().expect("test binary"));
    command
        .args([
            "--exact",
            "app_updates::helpers::tests::local_sync_child",
            "--nocapture",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", root.join("home"))
        .env("AAC_TEST_HELPER_SYNC_ROOT", &root);
    for name in ["AAC_TEST_PACKAGED_AGENT", "AAC_TEST_WAVE84_HELPERS"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let child = command.output().expect("run isolated child");
    print!("{}", String::from_utf8_lossy(&child.stdout));
    assert!(
        child.status.success(),
        "{}",
        String::from_utf8_lossy(&child.stderr)
    );
}

#[cfg(unix)]
fn native() -> &'static str {
    if cfg!(target_os = "macos") {
        "mac"
    } else {
        "ubuntu"
    }
}

#[cfg(unix)]
fn probe(launcher: &Path) -> bool {
    std::process::Command::new("/usr/bin/python3")
        .arg("-B")
        .arg(launcher)
        .args(["--platform", native(), "--agent-capabilities"])
        .output()
        .is_ok_and(|done| {
            done.status.success()
                && serde_json::from_slice::<Value>(
                    done.stdout
                        .split(|b| *b == b'\n')
                        .next()
                        .unwrap_or_default(),
                )
                .ok()
                    == Some(json!({"appWorker":1}))
        })
}

#[cfg(unix)]
fn generations(helpers: &Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(helpers)
        .expect("helper root")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .filter(|name| name.starts_with("helpers-") || name.starts_with(".helpers-stage-"))
        .collect();
    names.sort();
    names
}

#[cfg(unix)]
fn launcher_line(helpers: &Path, prefix: &str) -> String {
    std::fs::read_to_string(helpers.join("app_updates.py"))
        .expect("launcher")
        .lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::to_owned))
        .expect("launcher line")
}

/// The Wave 84 layout: a format-2 launcher naming an immutable generation. Its helper exits 2
/// on the capability probe, as the exact Wave 84 helper did. AAC_TEST_WAVE84_HELPERS supplies
/// the real Wave 84 files instead of the stand-in.
#[cfg(unix)]
fn seed_wave84(helpers: &Path) -> String {
    let generation = format!("helpers-{}", "8".repeat(32));
    let folder = helpers.join(&generation);
    std::fs::create_dir_all(&folder).expect("Wave 84 generation");
    match std::env::var_os("AAC_TEST_WAVE84_HELPERS") {
        Some(source) => {
            for name in HELPER_FILES {
                std::fs::copy(Path::new(&source).join(name), folder.join(name))
                    .expect("copy Wave 84 helper");
            }
        }
        None => {
            for name in HELPER_FILES {
                std::fs::write(folder.join(name), "# Wave 84 stand-in\n").expect("stand-in");
            }
            std::fs::write(folder.join("app_updates.py"), STAND_IN).expect("stand-in helper");
        }
    }
    std::fs::write(
        helpers.join("app_updates.py"),
        format!("# aac-helpers: {generation}\n# aac-helpers-format: 2\nimport pathlib, runpy, sys\nroot = pathlib.Path(__file__).resolve().parent / {generation:?}\nsys.path.insert(0, str(root))\nrunpy.run_path(str(root / \"app_updates.py\"), run_name=\"__main__\")\n"),
    )
    .expect("Wave 84 launcher");
    generation
}

/// The packaged agent's own decision. A Python stand-in answers each installer with
/// `not_installed`, so no real installer runs; the capability probe runs the synced helper.
#[cfg(target_os = "linux")]
fn agent(root: &Path, home: &Path) -> Option<Value> {
    use std::io::{BufRead, Write};
    let binary = std::env::var_os("AAC_TEST_PACKAGED_AGENT")?;
    let python = root.join("python-stand-in");
    if !python.exists() {
        std::fs::write(
            &python,
            "#!/bin/sh\nfor arg in \"$@\"; do [ \"$arg\" = --agent-capabilities ] && exec /usr/bin/python3 \"$@\"; done\napp=; last=\nfor arg in \"$@\"; do [ \"$last\" = --app ] && app=$arg; last=$arg; done\nprintf '{\"results\":[{\"appId\":\"%s\",\"platform\":\"ubuntu\",\"status\":\"not_installed\",\"messageCode\":\"not_installed\"}]}\\n' \"$app\"\n",
        )
        .expect("python stand-in");
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o700))
            .expect("stand-in mode");
    }
    let mut child = std::process::Command::new(binary)
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("HOME", home)
        .env("AAC_APP_UPDATE_PYTHON", &python)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("start packaged agent");
    // A fresh job each time: a repeated job id would only replay the saved journal.
    static CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let job =
        aac_core::sha256_hex(format!("{}:{call}", home.display()).as_bytes())[..32].to_owned();
    let request = json!({"protocol":1,"verb":"apps","id":job,"deadlineMs":120000,
        "args":{"action":"update","platform":"ubuntu","jobId":job}});
    writeln!(child.stdin.take().expect("stdin"), "{request}").expect("request");
    let reader = std::io::BufReader::new(child.stdout.take().expect("stdout"));
    let result = reader
        .lines()
        .map_while(Result::ok)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|event| event["type"] == "result" || event["type"] == "failure")
        .expect("agent terminal event");
    child.wait().expect("reap agent");
    Some(result["value"].clone())
}

#[cfg(not(target_os = "linux"))]
fn agent(_: &Path, _: &Path) -> Option<Value> {
    None
}

#[cfg(unix)]
fn assert_agent(root: &Path, home: &Path, capable: bool) {
    let Some(value) = agent(root, home) else {
        println!("packaged agent not supplied; the launcher probe stands for the agent's");
        return;
    };
    if capable {
        let rows = value["results"].as_array().expect("agent rows");
        assert_eq!(rows.len(), 12, "{value}");
        assert!(
            rows.iter().all(|row| row["status"] == "not_installed"),
            "{value}"
        );
        println!("packaged agent: {} rows after sync", rows.len());
    } else {
        assert_eq!(value, json!({"fallback":"python"}));
        println!("packaged agent: fallback before sync");
    }
}

#[cfg(unix)]
#[tokio::test]
async fn local_sync_child() {
    let Some(root) = std::env::var_os("AAC_TEST_HELPER_SYNC_ROOT").map(PathBuf::from) else {
        return;
    };
    let set = HelperSet::load(&package(&root)).expect("helper set");
    let computer = Computer::new(
        ComputerConfig {
            id: ComputerId::new("ubuntu").expect("id"),
            label: "Fixture".into(),
            os: if native() == "mac" {
                OperatingSystem::Mac
            } else {
                OperatingSystem::Linux
            },
            transport: TransportConfig::Local,
            roles: Vec::new(),
            metadata: Default::default(),
        },
        SshPolicy::new(root.join("known_hosts"), root.join("run")),
    )
    .expect("local computer");
    let cancel = CancellationToken::new();
    let home = root.join("home");
    let helpers = home.join(".ccs/app-updates");

    // Absent helper: the agent falls back, the sync publishes, the probe and agent pass.
    assert_agent(&root, &home, false);
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    assert!(probe(&helpers.join("app_updates.py")));
    assert_eq!(launcher_line(&helpers, "# aac-helpers-previous: "), "none");
    assert_agent(&root, &home, true);
    let first = generations(&helpers);
    assert_eq!(first.len(), 1, "{first:?}");
    let launcher = std::fs::read(helpers.join("app_updates.py")).expect("launcher");

    // A current computer pays one round trip and changes nothing.
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    assert_eq!(generations(&helpers), first);
    assert_eq!(
        std::fs::read(helpers.join("app_updates.py")).expect("same"),
        launcher
    );

    // A damaged upload is refused before anything is staged or swapped.
    let mut damaged = set.payload(native());
    let last = damaged.len() - 3;
    damaged[last] ^= 0x20;
    let options = CallOptions::new(Duration::from_secs(60), 64 * 1024);
    let reply = computer
        .execute(&argv(computer.config().os), &damaged, &options)
        .await
        .expect("damaged upload reply");
    assert_eq!(interpret(&reply), Err(failed("helper_upload_damaged")));
    assert_eq!(generations(&helpers), first);
    assert_eq!(
        std::fs::read(helpers.join("app_updates.py")).expect("kept"),
        launcher
    );

    // The Wave 84 helper: refused by the agent until the sync, then rollback stays possible.
    std::fs::remove_dir_all(&helpers).expect("reset helpers");
    std::fs::create_dir_all(&helpers).expect("helpers");
    let wave84 = seed_wave84(&helpers);
    let before: Vec<_> = HELPER_FILES
        .iter()
        .map(|name| std::fs::read(helpers.join(&wave84).join(name)).expect("Wave 84 bytes"))
        .collect();
    assert!(!probe(&helpers.join("app_updates.py")));
    assert_agent(&root, &home, false);
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    assert!(probe(&helpers.join("app_updates.py")));
    assert_agent(&root, &home, true);
    assert_eq!(launcher_line(&helpers, "# aac-helpers-previous: "), wave84);
    let current = launcher_line(&helpers, "# aac-helpers: ");
    assert_ne!(current, wave84);
    assert_eq!(generations(&helpers), {
        let mut both = vec![current.clone(), wave84.clone()];
        both.sort();
        both
    });
    for (name, bytes) in HELPER_FILES.iter().zip(&before) {
        assert_eq!(
            &std::fs::read(helpers.join(&wave84).join(name)).expect("kept"),
            bytes
        );
    }
    // The documented rollback selects the previous generation again.
    let rollback = std::process::Command::new("/usr/bin/python3")
        .args(["-c", include_str!("helper_rollback.py")])
        .env("HOME", &home)
        .output()
        .expect("rollback");
    assert!(rollback.status.success(), "{rollback:?}");
    assert_eq!(launcher_line(&helpers, "# aac-helpers: "), wave84);
    assert_eq!(launcher_line(&helpers, "# aac-helpers-previous: "), current);
    assert!(!probe(&helpers.join("app_updates.py")));

    // Flat helpers from before generations are kept as a generation too.
    std::fs::remove_dir_all(&helpers).expect("reset helpers");
    std::fs::create_dir_all(&helpers).expect("helpers");
    std::fs::write(helpers.join("app_updates.py"), STAND_IN).expect("flat helper");
    assert_eq!(sync(&computer, &set, native(), &cancel).await, Ok(()));
    let previous = launcher_line(&helpers, "# aac-helpers-previous: ");
    assert_eq!(
        std::fs::read_to_string(helpers.join(&previous).join("app_updates.py")).expect("flat"),
        STAND_IN
    );
    assert!(probe(&helpers.join("app_updates.py")));
}
