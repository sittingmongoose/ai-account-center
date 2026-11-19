//! L5: the private front door delegates to one pool/client per configured computer. No shipping.
use aac_config::{Config, LoadOptions, OperatingSystem, TransportConfig};
use aac_core::{Cause, Failure};
use aac_protocol::{Control, Event, Handshake, PROTOCOL_VERSION, Request, frame_cap};
use aac_transport::{
    AgentArtifact, AgentClient, CallOptions, CancellationToken, Computer, SshPolicy,
};
use axum::{
    Json,
    body::{Body, Bytes},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json, value::RawValue};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Semaphore, mpsc, watch};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    artifacts: Vec<Artifact>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Artifact {
    target: String,
    version: String,
    sha256: String,
    file: PathBuf,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Selector {
    platform: String,
    ssh_host: Option<String>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Call {
    computer: Selector,
    args: Box<RawValue>,
    deadline_ms: u64,
    timeout_ms: u64,
    max_bytes: usize,
    call_id: String,
}
struct Entry {
    config: aac_config::ComputerConfig,
    client: Option<AgentClient>,
}
struct Registry {
    entries: Vec<Entry>,
}
struct Active {
    input: mpsc::Sender<Vec<u8>>,
    done: watch::Receiver<Option<bool>>,
    abort: CancellationToken,
}

pub(crate) struct Agents {
    root: PathBuf,
    registry: watch::Sender<Option<Arc<Result<Registry, Failure>>>>,
    loading: AtomicBool,
    slots: Arc<Semaphore>,
    active: Mutex<BTreeMap<String, Active>>,
}

/// The service's deadline is this much earlier than the caller's `timeoutMs`.
const FRONT_DOOR_MARGIN_MS: u64 = 250;

fn refused() -> Failure {
    Failure::new(Cause::Refused, "agent transport unavailable")
}
fn compatible_target(os: OperatingSystem, target: &str) -> bool {
    match os {
        OperatingSystem::Linux => matches!(
            target,
            "x86_64-unknown-linux-musl" | "x86_64-unknown-linux-gnu"
        ),
        OperatingSystem::Mac => matches!(target, "aarch64-apple-darwin" | "x86_64-apple-darwin"),
        OperatingSystem::Windows => target == "x86_64-pc-windows-msvc",
    }
}
fn word(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}
fn line(value: &Value) -> Bytes {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes.into()
}
fn fallback(id: &str, reason: &str) -> Bytes {
    line(&json!({"type":"fallback","protocol":1,"id":id,"fallback":"python","reason":reason}))
}
fn response(body: Body) -> Response {
    (
        [
            ("content-type", "application/x-ndjson"),
            ("cache-control", "no-store"),
        ],
        body,
    )
        .into_response()
}
fn unavailable(id: &str, reason: &str) -> Response {
    response(Body::from(fallback(id, reason)))
}

impl Registry {
    fn load(root: &Path) -> Result<Self, Failure> {
        // No real-home discovery. The package paths are supplied by the trusted front door.
        if std::env::var("AAC_AGENT_TRANSPORT").is_ok_and(|v| v != "on") {
            return Err(refused());
        }
        let package =
            PathBuf::from(std::env::var_os("AAC_AGENT_PACKAGE_ROOT").ok_or_else(refused)?);
        if !package.is_absolute() {
            return Err(refused());
        }
        let mut options = LoadOptions::new(
            root.into(),
            package.join("web-dashboard/public/computers.json"),
        );
        options.cli = json!({"config_dir":root});
        let config = Config::load(&options)?;
        let enabled = std::env::var("AAC_AGENT_TRANSPORT")
            .ok()
            .map_or_else(|| config.values()["agentTransport"] == "on", |v| v == "on");
        if !enabled {
            return Err(refused());
        }
        let catalog_path = std::env::var_os("AAC_AGENT_CATALOG")
            .map(PathBuf::from)
            .unwrap_or_else(|| package.join("dist/web-server/services/agent-artifacts.json"));
        let catalog: Catalog =
            serde_json::from_slice(&aac_files::read_bounded(&catalog_path, 65536)?)
                .map_err(|_| refused())?;
        if catalog.artifacts.len() > 16 {
            return Err(refused());
        }
        let settings = &config.values()["agentTransportSsh"];
        let identity = std::env::var_os("AAC_AGENT_IDENTITY_FILE")
            .map(PathBuf::from)
            .or_else(|| settings["identityFile"].as_str().map(PathBuf::from));
        let hosts = std::env::var_os("AAC_AGENT_KNOWN_HOSTS_FILE")
            .map(PathBuf::from)
            .or_else(|| settings["knownHostsFile"].as_str().map(PathBuf::from));
        if config.computers().len() > 128 {
            return Err(refused());
        }
        let mut entries = Vec::new();
        for definition in config.computers() {
            let mut definition = definition.clone();
            let mut admitted = true;
            if let TransportConfig::Ssh { key, .. } = &mut definition.transport {
                if key.as_os_str().is_empty() {
                    *key = identity.clone().unwrap_or_default();
                }
                admitted = key.is_absolute() && hosts.as_ref().is_some_and(|p| p.is_absolute());
            }
            let client = if admitted {
                let policy = SshPolicy::new(
                    hosts.clone().unwrap_or_default(),
                    root.join("run/agent-ssh"),
                );
                let artifact = catalog
                    .artifacts
                    .iter()
                    .find(|a| compatible_target(definition.os, &a.target));
                artifact
                    .and_then(|a| {
                        let path = package.join(&a.file);
                        AgentArtifact::load(
                            path,
                            a.sha256.clone(),
                            Handshake {
                                name: "aac-agent".into(),
                                version: a.version.clone(),
                                protocol: PROTOCOL_VERSION,
                                target: a.target.clone(),
                            },
                        )
                        .ok()
                    })
                    .and_then(|artifact| {
                        Computer::new(definition.clone(), policy)
                            .ok()
                            .map(|c| AgentClient::existing(c, artifact))
                    })
            } else {
                None
            };
            // Missing trust configuration disables this computer, never another computer.
            entries.push(Entry {
                config: definition,
                client,
            });
        }
        Ok(Self { entries })
    }
    fn client(&self, selector: &Selector) -> Option<AgentClient> {
        let os = match selector.platform.as_str() {
            "linux" | "ubuntu" => OperatingSystem::Linux,
            "mac" => OperatingSystem::Mac,
            "windows" => OperatingSystem::Windows,
            _ => return None,
        };
        let found = self
            .entries
            .iter()
            .filter(|e| {
                e.config.os == os
                    && match &selector.ssh_host {
                        Some(alias) => {
                            matches!(e.config.transport, TransportConfig::Ssh { .. })
                                && (e.config.id.as_str() == alias
                                    || e.config.metadata.get("sshAlias").and_then(Value::as_str)
                                        == Some(alias))
                        }
                        None => matches!(e.config.transport, TransportConfig::Local),
                    }
            })
            .collect::<Vec<_>>();
        if found.len() != 1 {
            return None;
        }
        found[0].client.clone()
    }
}

impl Agents {
    pub(crate) fn new(root: PathBuf) -> Self {
        Self {
            root,
            registry: watch::channel(None).0,
            loading: AtomicBool::new(false),
            slots: Arc::new(Semaphore::new(16)),
            active: Mutex::new(BTreeMap::new()),
        }
    }
    pub(crate) async fn call(self: &Arc<Self>, verb: &str, bytes: &[u8]) -> Response {
        let started = tokio::time::Instant::now();
        let Ok(mut call) = crate::json_object::from_slice::<Call>(
            bytes,
            &[
                "computer",
                "args",
                "deadlineMs",
                "timeoutMs",
                "maxBytes",
                "callId",
            ],
        ) else {
            return unavailable("", "shape");
        };
        let verb = if verb == "lifecycle-signin" {
            let Some((deadline, args)) =
                crate::lifecycle_signin::agent_start(&self.root, call.args.get())
            else {
                return unavailable(&call.call_id, "shape");
            };
            let Ok(args) = serde_json::value::to_raw_value(&args) else {
                return unavailable(&call.call_id, "shape");
            };
            call.computer = Selector {
                platform: "ubuntu".into(),
                ssh_host: None,
            };
            call.args = args;
            call.deadline_ms = deadline;
            "signin-session"
        } else {
            verb
        };
        if !word(&call.call_id)
            || ![
                "health",
                "cursor-open",
                "inventory",
                "signin-expiry",
                "quota-read",
                "scan-usage",
                "t3-pins",
                "key-store",
                "claude-history",
                "failover-inventory",
                "signin-session",
            ]
            .contains(&verb)
            || call.deadline_ms == 0
            || call.deadline_ms > aac_protocol::max_deadline_ms(verb)
            || call.timeout_ms == 0
            || call.timeout_ms > call.deadline_ms + 5000
            || call.max_bytes == 0
            || call.max_bytes > frame_cap(verb) * 2
            || !call.args.get().trim_start().starts_with('{')
        {
            return unavailable(&call.call_id, "shape");
        }
        // End before the front door's own `timeoutMs` timer, leaving time to reap the owned child
        // and deliver an ordinary fallback frame. A slow agent must not look like an unresponsive
        // service to L1's health supervisor (L5 review L2).
        let Some(budget) = call
            .timeout_ms
            .checked_sub(FRONT_DOOR_MARGIN_MS)
            .filter(|ms| *ms > 0)
        else {
            return unavailable(&call.call_id, "deadline");
        };
        let Ok(permit) = self.slots.clone().try_acquire_owned() else {
            return unavailable(&call.call_id, "busy");
        };
        let deadline = started + Duration::from_millis(budget);
        let mut loaded = self.registry.subscribe();
        if !self.loading.swap(true, Ordering::AcqRel) {
            let root = self.root.clone();
            let ready = self.registry.clone();
            tokio::spawn(async move {
                // Exactly one initialization job per boot, even if its waiting HTTP request
                // expires. A missing catalog, a known_hosts file added later, and computer or
                // identity edits stay unavailable until the service restarts (L5 review L5).
                // A slow mount cannot accumulate abandoned config/hash workers.
                let result = tokio::task::spawn_blocking(move || Registry::load(&root))
                    .await
                    .unwrap_or_else(|_| Err(refused()));
                ready.send_replace(Some(Arc::new(result)));
            });
        }
        let registry = tokio::time::timeout_at(deadline, async {
            loop {
                if let Some(registry) = loaded.borrow_and_update().clone() {
                    break Some(registry);
                }
                if loaded.changed().await.is_err() {
                    break None;
                }
            }
        })
        .await
        .ok()
        .flatten();
        let client = registry
            .as_deref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|registry| registry.client(&call.computer));
        let Some(client) = client else {
            return unavailable(&call.call_id, "unavailable");
        };
        let wire = format!(
            "{{\"protocol\":1,\"verb\":\"{verb}\",\"id\":\"{}\",\"deadlineMs\":{},\"args\":{}}}",
            call.call_id,
            call.deadline_ms,
            call.args.get()
        );
        let Ok(request) = serde_json::from_str::<Request>(&wire) else {
            return unavailable(&call.call_id, "shape");
        };
        let mut options = CallOptions::new(Duration::ZERO, call.max_bytes);
        options.deadline = deadline;
        options.scan = verb == "scan-usage";
        let (input, controls) = mpsc::channel(8);
        let (done, finished) = watch::channel(None);
        let (output, receiver) = mpsc::channel(8);
        let abort = options.cancel.clone();
        {
            let Ok(mut active) = self.active.lock() else {
                return unavailable(&call.call_id, "internal");
            };
            if active.contains_key(&call.call_id) {
                return unavailable(&call.call_id, "busy");
            }
            active.insert(
                call.call_id.clone(),
                Active {
                    input,
                    done: finished,
                    abort: abort.clone(),
                },
            );
        }
        let agents = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let (progress, mut events) = mpsc::channel(4);
            let result = {
                let work = client.call_controlled_encoded(
                    &request,
                    wire.as_bytes(),
                    &options,
                    controls,
                    progress,
                );
                tokio::pin!(work);
                loop {
                    tokio::select! {
                        result=&mut work=>break result,
                        Some(event)=events.recv()=>{
                            let sent = tokio::select! {
                                _ = options.cancel.cancelled() => false,
                                _ = tokio::time::sleep_until(options.deadline) => false,
                                sent = output.send(line(&serde_json::to_value(event).unwrap_or(Value::Null))) => sent.is_ok(),
                            };
                            if !sent { options.cancel.cancel(); }
                        }
                    }
                }
            };
            // Completion can win select while earlier validated progress is still queued.
            // Drain that queue before the terminal frame, under the same backpressure budget.
            while let Ok(event) = events.try_recv() {
                let bytes = line(&serde_json::to_value(event).unwrap_or(Value::Null));
                if !matches!(
                    tokio::time::timeout_at(options.deadline, output.send(bytes)).await,
                    Ok(Ok(()))
                ) {
                    options.cancel.cancel();
                    break;
                }
            }
            let mut cancelled = false;
            let terminal = match result {
                Ok(events) => match events.last() {
                    Some(Event::Result { value, .. }) if value == &json!({"fallback":"python"}) => {
                        fallback(&request.id, "unsupported")
                    }
                    Some(Event::Failure { failure, .. })
                        if matches!(request.verb.as_str(), "cursor-open" | "signin-session")
                            && failure.cause == Cause::Refused
                            && failure.detail.as_str() == "unsupported agent verb" =>
                    {
                        fallback(&request.id, "unsupported")
                    }
                    Some(event) => {
                        cancelled = matches!(event,Event::Failure{failure,..} if failure.cause==Cause::Cancelled);
                        line(&serde_json::to_value(event).unwrap_or(Value::Null))
                    }
                    None => fallback(&request.id, "reply"),
                },
                Err(_) => fallback(&request.id, "transport"),
            };
            // call_controlled has waited for the far agent's terminal frame AND SSH child exit.
            let _ = done.send(Some(cancelled));
            if let Ok(mut active) = agents.active.lock() {
                active.remove(&request.id);
            }
            let _ = tokio::time::timeout_at(
                options.deadline + Duration::from_millis(200),
                output.send(terminal),
            )
            .await;
        });
        let stream = futures_util::stream::unfold(
            (receiver, AbortOnDrop(abort)),
            |(mut receiver, guard)| async move {
                receiver
                    .recv()
                    .await
                    .map(|bytes| (Ok::<_, std::io::Error>(bytes), (receiver, guard)))
            },
        );
        response(Body::from_stream(stream))
    }

    pub(crate) async fn control(&self, id: &str, bytes: &[u8]) -> Response {
        let Some((control, frame)) = control_line(bytes) else {
            return Json(json!({"accepted":false})).into_response();
        };
        let active = self.active.lock().ok().and_then(|active| {
            active
                .get(id)
                .map(|a| (a.input.clone(), a.done.clone(), a.abort.clone()))
        });
        let Some((input, mut done, abort)) = active else {
            return Json(json!({"accepted":false})).into_response();
        };
        if input.try_send(frame).is_err() {
            return Json(json!({"accepted":false})).into_response();
        }
        if matches!(control, Control::Cancel { cancel: true }) {
            let acknowledged = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    if let Some(cancelled) = *done.borrow_and_update() {
                        return cancelled;
                    }
                    if done.changed().await.is_err() {
                        return false;
                    }
                }
            })
            .await
            .unwrap_or(false);
            if !acknowledged {
                abort.cancel();
            }
            Json(json!({"cancelled":acknowledged})).into_response()
        } else {
            Json(json!({"accepted":true})).into_response()
        }
    }
}
/// The agent's control line: the parsed [`Control`], written compactly with one trailing newline.
/// A body such as `{"cancel":\ntrue}` is valid here and must not be forwarded raw, or the agent
/// reads `{"cancel":` and ends the call as `invalid agent control` (L5 review L3).
pub(crate) fn control_line(bytes: &[u8]) -> Option<(Control, Vec<u8>)> {
    if bytes.len() > 65536 {
        return None;
    }
    let control = crate::json_object::from_slice::<Control>(bytes, &["cancel"])
        .or_else(|_| crate::json_object::from_slice::<Control>(bytes, &["input"]))
        .ok()?;
    let mut frame = serde_json::to_vec(&control).ok()?;
    frame.push(b'\n');
    Some((control, frame))
}

struct AbortOnDrop(CancellationToken);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

#[cfg(test)]
mod tests {
    use super::control_line;
    use aac_protocol::Control;

    #[test]
    fn a_cancel_with_a_newline_is_one_compact_line() {
        let (control, frame) = control_line(b"{\"cancel\":\ntrue}").expect("control");
        assert!(matches!(control, Control::Cancel { cancel: true }));
        assert_eq!(frame, b"{\"cancel\":true}\n");
    }
}
