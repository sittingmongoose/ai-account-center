//! The helper set each computer's agent runs, synced before dispatch as Wave 84's
//! `syncRemoteHelpers` did. The agent runs `~/.ccs/app-updates/app_updates.py` under its own
//! account and needs a helper that answers `--agent-capabilities`. The dashboard sends the
//! package's checksum-verified set through the agent's own transport and account. The fixed
//! script publishes it as one immutable generation, registers the Windows task, swaps the
//! launcher last and keeps the previous generation for rollback. A failure dispatches nothing.
use aac_config::OperatingSystem;
use aac_core::Cause;
use aac_transport::{CallOptions, CancellationToken, Computer, ProcessOutput};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{ffi::OsString, path::Path, time::Duration};

/// Every file the agent's helper imports or starts. The Codex runtime is generated into
/// `dist/app-updates` by the package build; the rest are the package's source helpers.
pub(super) const HELPER_FILES: [&str; 17] = [
    "app_updates.py",
    "app_update_common.py",
    "app_update_desktop.py",
    "app_update_t3.py",
    "app_update_t3_rpc.py",
    "app_update_cursor.py",
    "app_update_antigravity.py",
    "app_update_antigravity_release.py",
    "app_update_macos.py",
    "app_update_processes.py",
    "app_update_terminal.py",
    "app_update_terminal_child.py",
    "app_update_pipe.py",
    "app_update_confirmed_codex.py",
    "app_update_zcode.py",
    "app_update_codex.cjs",
    RUNTIME,
];
const RUNTIME: &str = "app_update_codex_runtime.cjs";
const SCRIPT: &str = include_str!("helper_sync.py");
/// No quote characters: Windows PowerShell strips embedded double quotes from native arguments.
const BOOTSTRAP: &str = "import sys,base64;exec(base64.b64decode(sys.stdin.buffer.readline()))";
const MAX_FILE: usize = 16 * 1024 * 1024;
/// Upload, two capability probes of up to 30 seconds each and the swap.
const SYNC_BUDGET: Duration = Duration::from_secs(150);

/// Why a computer did not get the helper set. `reason` is a diagnostic word on each row;
/// `cause` is one of the contract's diagnostic causes.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum SyncError {
    Cancelled,
    Failed {
        reason: &'static str,
        cause: &'static str,
    },
}

fn failed(reason: &'static str) -> SyncError {
    SyncError::Failed {
        reason,
        cause: match reason {
            "helper_not_writable" => "not_writable",
            "helper_sync_timeout" => "timeout",
            _ => "unknown",
        },
    }
}

/// The package's helper files with their SHA-256, read once per job.
pub(super) struct HelperSet {
    files: Vec<(&'static str, Vec<u8>, String)>,
}

impl HelperSet {
    /// A missing or oversized file fails every computer instead of deploying a partial set.
    pub(super) fn load(package: &Path) -> Result<Self, SyncError> {
        let files = HELPER_FILES
            .iter()
            .map(|&name| {
                let folder = if name == RUNTIME {
                    package.join("dist/app-updates")
                } else {
                    package.join("scripts/app-updates")
                };
                let bytes = aac_files::read_bounded(&folder.join(name), MAX_FILE)
                    .map_err(|_| failed("helper_set_missing"))?;
                let hash = aac_core::sha256_hex(&bytes);
                Ok((name, bytes, hash))
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { files })
    }

    /// Script line, manifest line, then each file's bytes in manifest order.
    pub(super) fn payload(&self, platform: &str) -> Vec<u8> {
        let header = json!({
            "platform": platform,
            "files": self.files.iter().map(|(name, bytes, hash)| {
                json!({"name": name, "size": bytes.len(), "sha256": hash})
            }).collect::<Vec<_>>(),
        });
        let mut payload = STANDARD.encode(SCRIPT).into_bytes();
        payload.push(b'\n');
        payload.extend(header.to_string().into_bytes());
        payload.push(b'\n');
        for (_, bytes, _) in &self.files {
            payload.extend_from_slice(bytes);
        }
        payload
    }
}

/// The interpreter the agent itself uses: `/usr/bin/python3` on Linux and Mac, `python` on
/// Windows. The sync runs as the same account, so its probe is the agent's probe.
pub(super) fn argv(os: OperatingSystem) -> Vec<OsString> {
    let python = if os == OperatingSystem::Windows {
        "python.exe"
    } else {
        "/usr/bin/python3"
    };
    [python, "-B", "-c", BOOTSTRAP]
        .into_iter()
        .map(OsString::from)
        .collect()
}

/// Publish the set on one computer through its admitted transport.
pub(super) async fn sync(
    computer: &Computer,
    set: &HelperSet,
    platform: &str,
    cancel: &CancellationToken,
) -> Result<(), SyncError> {
    let mut options = CallOptions::new(SYNC_BUDGET, 64 * 1024);
    options.cancel = cancel.child_token();
    match computer
        .execute(
            &argv(computer.config().os),
            &set.payload(platform),
            &options,
        )
        .await
    {
        Ok(output) => interpret(&output),
        Err(_) if cancel.is_cancelled() => Err(SyncError::Cancelled),
        Err(failure) if failure.cause == Cause::Timeout => Err(failed("helper_sync_timeout")),
        Err(_) => Err(failed("helper_transport_failed")),
    }
}

pub(super) fn interpret(output: &ProcessOutput) -> Result<(), SyncError> {
    let reply = output
        .stdout
        .split(|byte| *byte == b'\n')
        .next()
        .and_then(|line| serde_json::from_slice::<Value>(line).ok());
    match reply {
        Some(reply) if reply["ok"] == true && output.code == Some(0) => Ok(()),
        Some(reply) if reply["ok"] == false => Err(failed(match reply["reason"].as_str() {
            Some("helper_upload_damaged") => "helper_upload_damaged",
            Some("helper_capability_missing") => "helper_capability_missing",
            Some("helper_not_writable") => "helper_not_writable",
            Some("helper_sync_timeout") => "helper_sync_timeout",
            Some("windows_task_missing") => "windows_task_missing",
            _ => "helper_reply_invalid",
        })),
        _ if matches!(output.code, Some(127 | 9009)) => Err(failed("helper_python_missing")),
        _ => Err(failed("helper_reply_invalid")),
    }
}

#[cfg(test)]
#[path = "helper_tests.rs"]
mod tests;
