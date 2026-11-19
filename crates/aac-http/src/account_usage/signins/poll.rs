//! A bounded native sign-in poll across the configured L5 computers.
use crate::{
    agent_transport::{Agents, Selector},
    collection::{Collected, read_agent},
};
use aac_protocol::{MAX_FRAME_BYTES, PROTOCOL_VERSION, Request};
use aac_views::decline::Decline;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use std::{collections::HashSet, path::PathBuf, sync::Arc, time::Duration};
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

/// Read only the supported sign-in metadata. One instance reuses its L5 pool across polls.
#[derive(Clone)]
pub struct Reader {
    root: PathBuf,
    agents: Arc<Agents>,
}

impl Reader {
    /// Bind a native caller to its configured private root and L5 package configuration.
    pub fn new(root: PathBuf) -> Self {
        let agents = Arc::new(Agents::new(root.clone()));
        Self { root, agents }
    }

    pub(crate) fn shared(root: PathBuf, agents: Arc<Agents>) -> Self {
        Self { root, agents }
    }

    /// Collect the configured hosts concurrently with the M2 verb's existing deadlines.
    pub async fn read(&self, hosts: Vec<Host>) -> Result<Batch, Decline> {
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
            let selector = Selector::new(host.platform.clone(), host.ssh_host.clone());
            let args = if host.ssh_host.is_none() {
                json!({"ccsDir": self.root})
            } else {
                json!({})
            };
            let agents = self.agents.clone();
            let host = host.host.clone();
            reads.spawn(async move {
                let request = Request {
                    protocol: PROTOCOL_VERSION,
                    id: format!("signin-{index}"),
                    verb: "signin-expiry".into(),
                    deadline_ms: 15_000,
                    args,
                };
                let wire = serde_json::to_string(&request).ok()?;
                let result =
                    read_agent(&agents, selector, request, wire, deadline, MAX_FRAME_BYTES)
                        .await
                        .ok()?;
                let Collected::Value(value) = result.as_ref() else {
                    return None;
                };
                let profiles = super::profile_rows::profiles(value, &host)?;
                (!profiles.is_empty()).then_some((index, profiles))
            });
        }
        let mut results = vec![None; hosts.len()];
        while let Some(outcome) = reads.join_next().await {
            if let Ok(Some((index, rows))) = outcome {
                results[index] = Some(rows);
            }
        }
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
}
