//! The packaged standalone reader, run locally or through the registered, pinned SSH transport.
use crate::agent_transport::{self, Agents, Selector, UsageHost};
use aac_core::Cause;
use serde_json::Value;
use std::{ffi::OsString, path::Path};

const SCANNER_MISSING: i32 = 87;
const NAME: &str = "aac-usage-scan";
const PROTOCOL: u64 = 3;
const LINUX_TARGET: &str = "x86_64-unknown-linux-gnu";

fn scanner_hash<'a>(manifest: &'a Value, target: &str) -> Option<&'a str> {
    if manifest["name"] != NAME || manifest["protocol"] != PROTOCOL {
        return None;
    }
    let hash = manifest["targets"][target]["sha256"].as_str()?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return None;
    }
    Some(hash)
}

fn local_launch(directory: &Path, manifest: &Value) -> Option<Vec<OsString>> {
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => LINUX_TARGET,
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        _ => return None,
    };
    let hash = scanner_hash(manifest, target)?;
    let name = format!("{NAME}{}", std::env::consts::EXE_SUFFIX);
    let path = directory.join(target).join(&name);
    let entry = &manifest["targets"][target];
    let expected = format!("{target}/{name}");
    if entry["file"].as_str() != Some(expected.as_str()) {
        return None;
    }
    let bytes = aac_files::read_bounded(&path, aac_transport::artifact_cap()).ok()?;
    (entry["size"] == bytes.len() && aac_core::sha256_hex(&bytes) == hash)
        .then(|| vec![path.into_os_string()])
}

fn linux_launch(manifest: &Value) -> Option<Vec<OsString>> {
    let hash = scanner_hash(manifest, LINUX_TARGET)?;
    let script = format!(
        r#"case "$(uname -s)-$(uname -m)" in Linux-x86_64) ;; *) exit {SCANNER_MISSING} ;; esac
p="$HOME/.cache/ai-account-center/bin/aac-usage-scan-{prefix}"
[ -x "$p" ] || exit {SCANNER_MISSING}
got=$( (sha256sum "$p" 2>/dev/null || shasum -a 256 "$p" 2>/dev/null) | cut -c1-64)
[ "$got" = "{hash}" ] || exit {SCANNER_MISSING}
exec "$p""#,
        prefix = &hash[..12],
    );
    Some(["/bin/sh".into(), "-c".into(), script.into()].into())
}

pub(super) fn eligible(host: &UsageHost) -> bool {
    (host.alias.is_none() || (host.fixed_ssh_available && host.platform == "ubuntu"))
        && std::env::var("AAC_AGENT_VERBS").as_deref() != Ok("off")
}

pub(super) async fn scan(
    agents: &Agents,
    host: &UsageHost,
    args: &Value,
    timeout_ms: u64,
    max_bytes: usize,
) -> Result<Value, Cause> {
    if !eligible(host) {
        return Err(Cause::AgentMissing);
    }
    let package = agent_transport::package_root().ok_or(Cause::AgentMissing)?;
    let directory = package.join("dist/native/aac-usage");
    let local = host.alias.is_none();
    let argv = tokio::task::spawn_blocking(move || {
        let bytes = aac_files::read_bounded(&directory.join("manifest.json"), 65536).ok()?;
        let manifest = serde_json::from_slice(&bytes).ok()?;
        if local {
            local_launch(&directory, &manifest)
        } else {
            linux_launch(&manifest)
        }
    })
    .await
    .map_err(|_| Cause::AgentMissing)?
    .ok_or(Cause::AgentMissing)?;
    let mut input = serde_json::to_vec(args).map_err(|_| Cause::Refused)?;
    input.push(b'\n');
    let output = agents
        .reference(
            Selector::usage(host.platform, host.alias.as_deref()),
            argv,
            input,
            timeout_ms,
            max_bytes,
        )
        .await?;
    match output.code {
        Some(0) if output.stdout.len() <= max_bytes => {
            serde_json::from_slice(&output.stdout).map_err(|_| Cause::Damaged)
        }
        Some(0) => Err(Cause::OutputTooLarge),
        Some(SCANNER_MISSING) => Err(Cause::AgentMissing),
        _ => Err(Cause::Damaged),
    }
}

#[cfg(all(test, target_os = "linux", target_arch = "x86_64"))]
mod tests {
    use super::*;
    use std::{
        fs,
        io::Write,
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
    };

    #[test]
    fn analytics_fixed_ssh_launch_requires_the_known_scanner_and_preserves_stdin() {
        let home = tempfile::tempdir().expect("fixture home");
        let body = b"#!/bin/sh\nexec cat\n";
        let hash = super::super::sha256_hex(body);
        let mut manifest = serde_json::json!({
            "name": "aac-usage-scan", "protocol": aac_usage::protocol::PROTOCOL,
            "targets": {"x86_64-unknown-linux-gnu": {"sha256": hash}},
        });
        let argv = linux_launch(&manifest).expect("fixed command");
        manifest["targets"]["x86_64-unknown-linux-gnu"]["sha256"] = serde_json::json!("invalid");
        assert!(linux_launch(&manifest).is_none());
        let directory = home.path().join(".cache/ai-account-center/bin");
        fs::create_dir_all(&directory).expect("scanner cache");
        let scanner = directory.join(format!("aac-usage-scan-{}", &hash[..12]));
        fs::write(&scanner, body).expect("scanner fixture");
        fs::set_permissions(&scanner, fs::Permissions::from_mode(0o700)).expect("executable");
        let mut child = Command::new(&argv[0])
            .args(&argv[1..])
            .env("HOME", home.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("launch fixture");
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(b"fixture request\n")
            .expect("stdin write");
        let output = child.wait_with_output().expect("fixture read");
        assert!(output.status.success());
        assert_eq!(output.stdout, b"fixture request\n");
        fs::write(scanner, b"#!/bin/sh\nexit 99\n").expect("mismatched fixture");
        let status = Command::new(&argv[0])
            .args(&argv[1..])
            .env("HOME", home.path())
            .status()
            .expect("checked launch");
        assert_eq!(status.code(), Some(SCANNER_MISSING));
    }
}
