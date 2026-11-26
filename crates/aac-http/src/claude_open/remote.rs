//! Native history and host operations over the authenticated agent pool. Mutations never replay.
use super::open::{AgentOpen, Remote, SshError, SshOutput};
use super::sync_service::HelperCall;
use crate::agent_transport::{Agents, Selector};
use aac_core::Cause;
use serde_json::Value;
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
/// The Open orchestrator's remote boundary over the service's agent pool.
pub struct AgentRemote {
    agents: Arc<Agents>,
}

impl AgentRemote {
    /// A boundary for one boot; the package root is the trusted front door's, never a request's.
    pub(crate) fn new(agents: Arc<Agents>, _package_root: PathBuf) -> Self {
        Self { agents }
    }

    fn selector(platform: &str, ssh_host: &str) -> Selector {
        Selector {
            platform: platform.to_string(),
            ssh_host: Some(ssh_host.to_string()),
        }
    }
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
            let mode = request["mode"].as_str().ok_or(())?.to_owned();
            if !super::manifest::profile_id_pattern(profile_id)
                || request["profileId"].as_str() != Some(profile_id)
                || request["platform"].as_str() != Some(platform)
                || !HISTORY_HELPER_MODES.contains(&mode.as_str())
            {
                return Err(());
            }
            if matches!(mode.as_str(), "append" | "snapshot-cleanup") {
                return match self
                    .agents
                    .call_mutation(
                        "claude-history",
                        &Self::selector(platform, ssh_host),
                        request,
                        30000,
                    )
                    .await
                {
                    crate::agent_transport::MutationReply::Result(value)
                        if history_reply_valid(&mode, profile_id, platform, &value) =>
                    {
                        serde_json::to_vec(&value).map_err(|_| ())
                    }
                    _ => Err(()),
                };
            }
            let value = self
                .agents
                .call_value(
                    "claude-history",
                    Self::selector(platform, ssh_host),
                    request,
                    25000,
                    30000,
                    MAX_PRIVATE_INDEX_BYTES,
                )
                .await
                .ok_or(())?;
            if !history_reply_valid(&mode, profile_id, platform, &value) {
                return Err(());
            }
            serde_json::to_vec(&value).map_err(|_| ())
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
        "verify-transcripts" => map.len() == 1 && value["verified"].is_boolean(),
        "snapshot-cleanup" => {
            map.len() == 3
                && ["kept", "deleted", "skipped"].iter().all(|k| {
                    value[*k]
                        .as_array()
                        .is_some_and(|rows| rows.iter().all(Value::is_string))
                })
        }
        "append" => append_receipt_valid(value),
        _ => false,
    }
}

fn append_receipt_valid(value: &Value) -> bool {
    const REQUIRED: [&str; 11] = [
        "schemaVersion",
        "status",
        "createdCount",
        "recoveryRequired",
        "ownedFilesRolledBack",
        "snapshotBindingVerified",
        "privateSnapshotsPreserved",
        "profileApplyEndpointImplemented",
        "vendorResumeImplemented",
        "distributedLeaseImplemented",
        "writerQuiescent",
    ];
    let Some(map) = value.as_object() else {
        return false;
    };
    if REQUIRED.iter().any(|k| !map.contains_key(*k))
        || map.keys().any(|k| {
            !REQUIRED.contains(&k.as_str())
                && !["createdCountBeforeRefusal", "protectedBytesUnchanged"].contains(&k.as_str())
        })
        || value["schemaVersion"] != 2
        || value["writerQuiescent"] != true
    {
        return false;
    }
    for k in ["createdCount", "createdCountBeforeRefusal"] {
        if (k == "createdCount" || map.contains_key(k))
            && !value[k].as_u64().is_some_and(|n| n <= 200)
        {
            return false;
        }
    }
    if REQUIRED[3..].iter().any(|k| !value[*k].is_boolean())
        || [
            "profileApplyEndpointImplemented",
            "vendorResumeImplemented",
            "distributedLeaseImplemented",
        ]
        .iter()
        .any(|k| value[*k] != false)
    {
        return false;
    }
    match value["status"].as_str() {
        Some("created_metadata") => {
            value["createdCount"].as_u64().is_some_and(|n| n >= 1)
                && value["protectedBytesUnchanged"] == true
                && value["recoveryRequired"] == false
                && value["snapshotBindingVerified"] == true
                && value["privateSnapshotsPreserved"] == true
        }
        Some("refused") => value["createdCount"] == 0,
        _ => false,
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn append_receipts_keep_the_frozen_helpers_strict_contract() {
        let good = json!({"schemaVersion":2,"status":"created_metadata","createdCount":1,"protectedBytesUnchanged":true,"recoveryRequired":false,"ownedFilesRolledBack":false,"snapshotBindingVerified":true,"privateSnapshotsPreserved":true,"profileApplyEndpointImplemented":false,"vendorResumeImplemented":false,"distributedLeaseImplemented":false,"writerQuiescent":true});
        assert!(append_receipt_valid(&good));
        for (key, value) in [
            ("createdCount", json!(0)),
            ("recoveryRequired", json!(true)),
            ("privateSnapshotsPreserved", json!(false)),
            ("vendorResumeImplemented", json!(true)),
            ("extra", json!(true)),
        ] {
            let mut bad = good.clone();
            bad[key] = value;
            assert!(!append_receipt_valid(&bad), "{bad}");
        }
    }
}
