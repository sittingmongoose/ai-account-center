//! A bounded native sign-in poll across the configured L5 computers.
use crate::agent_transport::Agents;
use aac_protocol::MAX_FRAME_BYTES;
use aac_views::decline::Decline;
use axum::body::to_bytes;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::HashSet, path::Path, sync::Arc, time::Duration};
use tokio::{task::JoinSet, time::Instant};

/// A dashboard host and its configured L5 selector.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Host {
    /// Published computer id.
    pub host: String,
    /// Existing agent platform name.
    pub platform: String,
    /// Configured SSH name, or no name for the local host.
    pub ssh_host: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Poll {
    hosts: Vec<Host>,
}

pub(crate) fn parse(bytes: &[u8]) -> Result<Vec<Host>, Decline> {
    crate::json_object::from_slice::<Poll>(bytes, &["hosts"])
        .map(|poll| poll.hosts)
        .map_err(|_| Decline::Shape)
}

/// Rows proven by the native readers, in request order. A reference runner can fill missing hosts.
pub struct Batch {
    /// Host keys and sanitized profile arrays.
    pub machines: Map<String, Value>,
    /// Hosts whose agent could not produce rows.
    pub reference_hosts: Vec<String>,
}

/// One result frame's value from the agent's NDJSON answer.
fn result_frame(bytes: &[u8]) -> Option<Value> {
    for line in bytes.split(|byte| *byte == b'\n') {
        let Ok(value) = serde_json::from_slice::<Value>(line) else {
            continue;
        };
        if value["type"] == "result" {
            return Some(value["value"].clone());
        }
    }
    None
}

/// Collect the configured hosts concurrently; each call keeps the L5 transport's own deadline.
pub async fn read(agents: &Arc<Agents>, root: &Path, hosts: Vec<Host>) -> Result<Batch, Decline> {
    let mut seen = HashSet::new();
    if hosts.is_empty()
        || hosts.len() > 5
        || hosts.iter().any(|entry| {
            !["ubuntu", "mac", "windows", "nas1", "alexis"].contains(&entry.host.as_str())
                || !["ubuntu", "linux", "mac", "windows"].contains(&entry.platform.as_str())
                || !seen.insert(&entry.host)
        })
    {
        return Err(Decline::Shape);
    }
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut reads = JoinSet::new();
    for (index, host) in hosts.iter().enumerate() {
        let agents = agents.clone();
        let name = host.host.clone();
        let platform = host.platform.clone();
        let ssh_host = host.ssh_host.clone();
        let args = if ssh_host.is_none() {
            json!({"ccsDir": root})
        } else {
            json!({})
        };
        reads.spawn(async move {
            let payload = json!({
                "computer": {"platform": platform, "sshHost": ssh_host},
                "args": args,
                "deadlineMs": 15_000,
                "timeoutMs": 20_000,
                "maxBytes": MAX_FRAME_BYTES,
                "callId": format!("signin-{index}"),
            });
            let response = agents
                .call("signin-expiry", payload.to_string().as_bytes())
                .await;
            let bytes = to_bytes(response.into_body(), MAX_FRAME_BYTES * 2)
                .await
                .ok()?;
            let value = result_frame(&bytes)?;
            let profiles = super::profile_rows::profiles(&value, &name)?;
            (!profiles.is_empty()).then_some((index, profiles))
        });
    }
    let mut results = vec![None; hosts.len()];
    loop {
        match tokio::time::timeout_at(deadline, reads.join_next()).await {
            Ok(Some(Ok(Some((index, rows))))) => results[index] = Some(rows),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    reads.abort_all();
    let mut machines = Map::new();
    let mut reference_hosts = Vec::new();
    for (host, rows) in hosts.into_iter().zip(results) {
        if let Some(rows) = rows {
            machines.insert(host.host, Value::Array(rows));
        } else {
            reference_hosts.push(host.host);
        }
    }
    Ok(Batch {
        machines,
        reference_hosts,
    })
}
