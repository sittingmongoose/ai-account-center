//! The production remote boundary for the Open orchestrator: lane E's `claude-history` verb
//! and lane F's `claude-host` open verb through the service's L5 pool, with the packaged
//! reference helpers and the fixed Open commands over the identical SSH policy as the
//! fallback — exactly the attempts the TS transports make, in the same order.
use super::open::{AgentOpen, Remote, SshError, SshOutput};
use super::sync_service::HelperCall;
use crate::agent_transport::{Agents, Selector};
use aac_core::Cause;
use serde_json::{Value, json};
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

const HISTORY_HELPER_MODES: [&str; 6] = [
    "closed-check",
    "collect",
    "protected-check",
    "verify-transcripts",
    "append",
    "snapshot-cleanup",
];
const MAX_PRIVATE_INDEX_BYTES: usize = 24 * 1024 * 1024;
const WRITER_SHA256: &str = "0b3b4cbb14db144457684bf7683d53bbbc38529d4b92c9c6590ab089eff0c132";
const MAX_HELPER_SOURCE_BYTES: usize = 128 * 1024;

/// The fixed bootstrap the TS transport sends over SSH: it execs the packaged helper with the
/// request on stdin and the packaged sources in globals. Byte for byte the reference's.
const BOOTSTRAP_CODE: &str = "import sys,json,io;_p=json.loads(sys.stdin.buffer.read().decode('utf-8'));_s=_p['helperSource'];globals()['__ccs_history_helper_source__']=_s;globals()['__ccs_history_node_bridge_source__']=_p.get('nodeBridgeSource');sys.stdin=io.TextIOWrapper(io.BytesIO(json.dumps(_p['request']).encode('utf-8')),encoding='utf-8');exec(compile(_s,'managed-history-helper','exec'))";

/// The Open orchestrator's remote boundary over the service's agent pool.
pub struct AgentRemote {
    agents: Arc<Agents>,
    package_root: PathBuf,
}

impl AgentRemote {
    /// A boundary for one boot; the package root is the trusted front door's, never a request's.
    pub(crate) fn new(agents: Arc<Agents>, package_root: PathBuf) -> Self {
        Self {
            agents,
            package_root,
        }
    }

    /// `readPackagedSource`: the canonical LF bytes of one packaged helper.
    fn packaged_source(&self, relative: &str) -> Result<Vec<u8>, ()> {
        let bytes = std::fs::read(self.package_root.join(relative)).map_err(|_| ())?;
        Ok(with_lf_line_endings(bytes))
    }

    fn selector(platform: &str, ssh_host: &str) -> Selector {
        Selector {
            platform: platform.to_string(),
            ssh_host: Some(ssh_host.to_string()),
        }
    }
}

/// `withLfLineEndings`: only CR LF pairs change; a lone CR stays, so any other edit fails its pin.
fn with_lf_line_endings(bytes: Vec<u8>) -> Vec<u8> {
    if !bytes.contains(&b'\r') {
        return bytes;
    }
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            out.push(b'\n');
            index += 2;
        } else {
            out.push(bytes[index]);
            index += 1;
        }
    }
    out
}

impl HelperCall for AgentRemote {
    fn call<'a>(
        &'a self,
        ssh_host: &'a str,
        platform: &'a str,
        profile_id: &'a str,
        request: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ()>> + Send + 'a>> {
        Box::pin(async move {
            // runClaudeHistoryHelper's admission: fixed modes, bound profile and platform.
            let mode = request["mode"].as_str().ok_or(())?;
            if !super::manifest::profile_id_pattern(profile_id)
                || request["profileId"].as_str() != Some(profile_id)
                || request["platform"].as_str() != Some(platform)
                || !HISTORY_HELPER_MODES.contains(&mode)
            {
                return Err(());
            }
            let helper =
                self.packaged_source("scripts/claude-history/history_index_remote_v2.py")?;
            if helper.len() > MAX_HELPER_SOURCE_BYTES {
                return Err(());
            }
            let mut private_request = request.clone();
            let mut bridge: Option<String> = None;
            if mode == "append" {
                let writer = self
                    .packaged_source("scripts/claude-history/history_index_transaction_v1.cjs")?;
                if super::js::sha(&writer) != WRITER_SHA256 {
                    return Err(());
                }
                private_request["transactionSource"] = json!(String::from_utf8_lossy(&writer));
                let bridge_bytes = self
                    .packaged_source("scripts/claude-history/history_index_node_bridge_v2.cjs")?;
                if bridge_bytes.len() > MAX_HELPER_SOURCE_BYTES {
                    return Err(());
                }
                bridge = Some(String::from_utf8_lossy(&bridge_bytes).into_owned());
            }
            let input = serde_json::to_vec(&json!({
                "helperSource": String::from_utf8_lossy(&helper),
                "nodeBridgeSource": bridge,
                "request": private_request
            }))
            .map_err(|_| ())?;
            if input.len() > MAX_PRIVATE_INDEX_BYTES {
                return Err(());
            }
            // The verb attempt covers exactly the three read modes the TS adapter attempts;
            // every other mode keeps the packaged Python/Node writer on the reference path.
            if matches!(mode, "collect" | "closed-check" | "protected-check")
                && let Some(value) = self
                    .agents
                    .call_value(
                        "claude-history",
                        Self::selector(platform, ssh_host),
                        request.clone(),
                        25000,
                        30000,
                        1024 * 1024,
                    )
                    .await
                && history_reply_valid(mode, profile_id, platform, &value)
            {
                return serde_json::to_vec(&value).map_err(|_| ());
            }
            // The reference bootstrap: private data and sources travel in the SSH stdin pipe.
            let argv = reference_helper_argv(platform, &input)?;
            let output = self
                .agents
                .reference(
                    Self::selector(platform, ssh_host),
                    argv,
                    input,
                    30000,
                    MAX_PRIVATE_INDEX_BYTES,
                )
                .await
                .map_err(|_| ())?;
            if output.code != Some(0) {
                return Err(());
            }
            Ok(output.stdout)
        })
    }
}

/// `runHistoryAgent`'s reply DTO checks, mode by mode.
fn history_reply_valid(mode: &str, profile_id: &str, platform: &str, value: &Value) -> bool {
    let Some(map) = value.as_object() else {
        return false;
    };
    match mode {
        "closed-check" => map.len() == 1 && value["closed"].is_boolean(),
        "protected-check" => map.len() == 1 && value["unchanged"].is_boolean(),
        "collect" => {
            value["profileId"].as_str() == Some(profile_id)
                && value["platform"].as_str() == Some(platform)
                && value["records"].is_array()
                && value["revision"].is_string()
        }
        _ => false,
    }
}

/// The fixed interpreter argv for the reference bootstrap on each platform.
fn reference_helper_argv(platform: &str, _input: &[u8]) -> Result<Vec<std::ffi::OsString>, ()> {
    if platform == "mac" {
        return Ok(vec![
            "/usr/bin/python3".into(),
            "-c".into(),
            BOOTSTRAP_CODE.into(),
        ]);
    }
    // The Windows bootstrap finds the migration venv's fixed python, then runs the same code.
    let script = [
        "$ErrorActionPreference = 'Stop'",
        "$python = [IO.Path]::Combine($env:USERPROFILE, '.ccs', 'claude-session-migration', 'venv', 'Scripts', 'python.exe')",
        "if (-not [IO.File]::Exists($python)) { exit 1 }",
        &format!("& $python -c '{}'", BOOTSTRAP_CODE.replace('\'', "''")),
        "exit $LASTEXITCODE",
    ]
    .join("; ");
    use base64::{Engine, engine::general_purpose::STANDARD};
    let units: Vec<u8> = script
        .encode_utf16()
        .flat_map(|unit| unit.to_le_bytes())
        .collect();
    Ok(vec![
        "powershell.exe".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-EncodedCommand".into(),
        STANDARD.encode(units).into(),
    ])
}

impl Remote for AgentRemote {
    fn host_open<'a>(
        &'a self,
        platform: &'a str,
        ssh_host: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Result<AgentOpen, SshError>> + Send + 'a>> {
        use crate::agent_transport::MutationReply;
        Box::pin(async move {
            match self
                .agents
                .call_mutation(
                    "claude-host",
                    &Self::selector(platform, ssh_host),
                    args,
                    60_000,
                )
                .await
            {
                MutationReply::Result(receipt) if receipt["ok"] == true => Ok(AgentOpen::Opened),
                MutationReply::NotDispatched => Ok(AgentOpen::NotDispatched),
                _ => Err(SshError::Failed),
            }
        })
    }

    fn ssh_exec<'a>(
        &'a self,
        platform: &'a str,
        ssh_host: &'a str,
        argv: Vec<String>,
        stdin: Option<Vec<u8>>,
        timeout_ms: u64,
        max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<SshOutput, SshError>> + Send + 'a>> {
        Box::pin(async move {
            let argv: Vec<std::ffi::OsString> = argv.into_iter().map(Into::into).collect();
            let output = self
                .agents
                .reference(
                    Self::selector(platform, ssh_host),
                    argv,
                    stdin.unwrap_or_default(),
                    timeout_ms,
                    max_bytes,
                )
                .await
                .map_err(|cause| {
                    if cause == Cause::Timeout {
                        SshError::Timeout
                    } else {
                        SshError::Failed
                    }
                })?;
            Ok(SshOutput {
                code: output.code,
                stdout: output.stdout,
            })
        })
    }
}
