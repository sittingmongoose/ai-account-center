//! L5: the private front door delegates to one pool/client per configured computer. No shipping.
use aac_config::{Config, LoadOptions, OperatingSystem, TransportConfig};
use aac_core::{Cause, Failure};
use aac_protocol::{Control, Event, Handshake, PROTOCOL_VERSION, Request, frame_cap};
use aac_transport::{
    AgentArtifact, AgentClient, CallOptions, CancellationToken, Computer, ProcessOutput, SshPolicy,
};
use axum::{
    Json,
    body::{Body, Bytes},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
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
#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Selector {
    pub(crate) platform: String,
    pub(crate) ssh_host: Option<String>,
}
impl Selector {
    pub(crate) fn usage(platform: &str, alias: Option<&str>) -> Self {
        Self {
            platform: platform.into(),
            ssh_host: alias.map(str::to_owned),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Call {
    pub(crate) computer: Selector,
    pub(crate) args: Box<RawValue>,
    pub(crate) deadline_ms: u64,
    pub(crate) timeout_ms: u64,
    pub(crate) max_bytes: usize,
    pub(crate) call_id: String,
}
struct Entry {
    config: aac_config::ComputerConfig,
    client: Option<AgentClient>,
    /// The same checked computer without the agent artifact: the Open orchestrator's
    /// reference commands run over the identical SSH policy when no verb can prove a step.
    computer: Option<Computer>,
}
pub(crate) struct UsageHost {
    pub(crate) id: String,
    pub(crate) platform: &'static str,
    pub(crate) alias: Option<String>,
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
    enabled_by_default: bool,
    registry: watch::Sender<Option<Arc<Result<Registry, Failure>>>>,
    loading: AtomicBool,
    slots: Arc<Semaphore>,
    active: Mutex<BTreeMap<String, Active>>,
}

/// The service's deadline is this much earlier than the caller's `timeoutMs`.
pub(crate) const FRONT_DOOR_MARGIN_MS: u64 = 250;

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

/// A random correlation id for an in-process orchestrator call (hex, as the front door's).
fn random_call_id() -> Option<String> {
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut file| std::io::Read::read_exact(&mut file, &mut bytes))
        .ok()?;
    Some(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub(crate) fn line(value: &Value) -> Bytes {
    let mut bytes = value.to_string().into_bytes();
    bytes.push(b'\n');
    bytes.into()
}
pub(crate) fn fallback(id: &str, reason: &str) -> Bytes {
    line(&json!({"type":"fallback","protocol":1,"id":id,"fallback":"python","reason":reason}))
}
pub(crate) fn response(body: Body) -> Response {
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

/// Resolve the installed package from the trusted boot override or the packaged binary.
pub(crate) fn package_root() -> Option<PathBuf> {
    if let Some(root) = std::env::var_os("AAC_AGENT_PACKAGE_ROOT") {
        let root = PathBuf::from(root);
        return root.is_absolute().then_some(root);
    }
    let exe = std::env::current_exe().ok()?;
    let native = exe.parent()?;
    let dist = native.parent()?;
    if native.file_name()? != "native" || dist.file_name()? != "dist" {
        return None;
    }
    Some(dist.parent()?.to_owned())
}

impl Registry {
    fn load(root: &Path, enabled_by_default: bool) -> Result<Self, Failure> {
        // No real-home discovery. The package paths are supplied by the trusted front door.
        if std::env::var("AAC_AGENT_TRANSPORT").is_ok_and(|v| v != "on") {
            return Err(refused());
        }
        let package = package_root().ok_or_else(refused)?;
        let mut options = LoadOptions::new(
            root.into(),
            package.join("web-dashboard/public/computers.json"),
        );
        options.cli = json!({"config_dir":root});
        let config = Config::load(&options)?;
        let enabled = std::env::var("AAC_AGENT_TRANSPORT").ok().map_or_else(
            || match config.values()["agentTransport"].as_str() {
                Some("on") => true,
                Some("off") => false,
                _ => enabled_by_default,
            },
            |v| v == "on",
        );
        if !enabled {
            return Err(refused());
        }
        let catalog_path = std::env::var_os("AAC_AGENT_CATALOG")
            .map(PathBuf::from)
            .unwrap_or_else(|| package.join("config/agent-artifacts.json"));
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
            let computer = if admitted {
                let policy = SshPolicy::new(
                    hosts.clone().unwrap_or_default(),
                    root.join("run/agent-ssh"),
                );
                Computer::new(definition.clone(), policy).ok()
            } else {
                None
            };
            let client = computer.clone().and_then(|computer| {
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
                    .map(|artifact| AgentClient::existing(computer, artifact))
            });
            // Missing trust configuration disables this computer, never another computer.
            entries.push(Entry {
                config: definition,
                client,
                computer,
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
    fn computer(&self, selector: &Selector) -> Option<Computer> {
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
        found[0].computer.clone()
    }
}

impl Call {
    pub(crate) fn parse(bytes: &[u8]) -> Option<Self> {
        crate::json_object::from_slice::<Call>(
            bytes,
            &[
                "computer",
                "args",
                "deadlineMs",
                "timeoutMs",
                "maxBytes",
                "callId",
            ],
        )
        .ok()
    }

    pub(crate) fn valid(&self, verb: &str) -> bool {
        [
            "health",
            "cursor-open",
            "muse-login-identity",
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
            && self.bounded(verb)
    }

    pub(crate) fn bounded(&self, verb: &str) -> bool {
        word(&self.call_id)
            && [
                "health",
                "cursor-open",
                "muse-login-identity",
                "inventory",
                "apps",
                "signin-expiry",
                "quota-read",
                "scan-usage",
                "t3-pins",
                "key-store",
                "claude-history",
                "failover-inventory",
                "failover-collect",
                "launch",
                "claude-host",
                "signin-session",
            ]
            .contains(&verb)
            && self.deadline_ms != 0
            && self.deadline_ms
                <= if verb == "apps" {
                    aac_protocol::APP_UPDATE_DEADLINE_MS
                } else {
                    aac_protocol::max_deadline_ms(verb)
                }
            && self.timeout_ms != 0
            && self.timeout_ms <= self.deadline_ms + 5000
            && self.max_bytes != 0
            && self.max_bytes <= frame_cap(verb) * 2
            && self.args.get().trim_start().starts_with('{')
    }

    pub(crate) fn wire(&self, verb: &str) -> String {
        format!(
            "{{\"protocol\":1,\"verb\":\"{verb}\",\"id\":\"{}\",\"deadlineMs\":{},\"args\":{}}}",
            self.call_id,
            self.deadline_ms,
            self.args.get()
        )
    }
}

impl Agents {
    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn permit(&self) -> Option<tokio::sync::OwnedSemaphorePermit> {
        self.slots.clone().try_acquire_owned().ok()
    }

    pub(crate) fn new(root: PathBuf) -> Self {
        Self::with_default(root, false)
    }

    pub(crate) fn with_default(root: PathBuf, enabled_by_default: bool) -> Self {
        Self {
            root,
            enabled_by_default,
            registry: watch::channel(None).0,
            loading: AtomicBool::new(false),
            slots: Arc::new(Semaphore::new(16)),
            active: Mutex::new(BTreeMap::new()),
        }
    }
    async fn registered(
        &self,
        deadline: tokio::time::Instant,
    ) -> Option<Arc<Result<Registry, Failure>>> {
        let mut loaded = self.registry.subscribe();
        if !self.loading.swap(true, Ordering::AcqRel) {
            let root = self.root.clone();
            let enabled_by_default = self.enabled_by_default;
            let ready = self.registry.clone();
            tokio::spawn(async move {
                // Exactly one initialization job per boot, even if its waiting HTTP request
                // expires. A missing catalog, a known_hosts file added later, and computer or
                // identity edits stay unavailable until the service restarts (L5 review L5).
                // A slow mount cannot accumulate abandoned config/hash workers.
                let result =
                    tokio::task::spawn_blocking(move || Registry::load(&root, enabled_by_default))
                        .await
                        .unwrap_or_else(|_| Err(refused()));
                ready.send_replace(Some(Arc::new(result)));
            });
        }
        tokio::time::timeout_at(deadline, async {
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
        .flatten()
    }

    pub(crate) async fn client(
        &self,
        selector: &Selector,
        deadline: tokio::time::Instant,
    ) -> Option<AgentClient> {
        self.registered(deadline)
            .await?
            .as_ref()
            .as_ref()
            .ok()?
            .client(selector)
    }

    pub(crate) async fn usage_hosts(&self) -> Vec<UsageHost> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        let Some(registered) = self.registered(deadline).await else {
            return Vec::new();
        };
        let Ok(registry) = registered.as_ref() else {
            return Vec::new();
        };
        registry
            .entries
            .iter()
            .filter(|entry| entry.client.is_some())
            .map(|entry| {
                let platform = match entry.config.os {
                    OperatingSystem::Linux => "ubuntu",
                    OperatingSystem::Mac => "mac",
                    OperatingSystem::Windows => "windows",
                };
                let remote = matches!(entry.config.transport, TransportConfig::Ssh { .. });
                UsageHost {
                    id: entry.config.id.as_str().to_owned(),
                    platform,
                    alias: remote.then(|| entry.config.id.as_str().to_owned()),
                }
            })
            .collect()
    }

    pub(crate) async fn app_call(
        &self,
        platform: &str,
        alias: Option<&str>,
        id: String,
        args: Value,
        budget: Duration,
        progress: mpsc::Sender<Event>,
    ) -> Result<Vec<Event>, Failure> {
        let started = tokio::time::Instant::now();
        let deadline = started + budget;
        let _permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Failure::new(Cause::Busy, "agent transport saturated"))?;
        let client = self
            .client(
                &Selector {
                    platform: platform.into(),
                    ssh_host: alias.map(str::to_owned),
                },
                deadline,
            )
            .await
            .ok_or_else(refused)?;
        let deadline_ms = aac_protocol::deadline_cap("apps", &args).min(
            u64::try_from(
                deadline
                    .saturating_duration_since(tokio::time::Instant::now())
                    .as_millis(),
            )
            .unwrap_or(0),
        );
        let request = Request {
            protocol: PROTOCOL_VERSION,
            verb: "apps".into(),
            id,
            deadline_ms,
            args,
        };
        let mut options = CallOptions::new(budget, aac_protocol::MAX_FRAME_BYTES * 2);
        options.deadline = deadline;
        if alias.is_none() {
            options.local_app_update_state_dir = Some(self.root.join("app-updates"));
        }
        let (_input, controls) = mpsc::channel(1);
        let wire = serde_json::to_vec(&request).map_err(|_| refused())?;
        client
            .call_controlled_encoded(&request, &wire, &options, controls, progress)
            .await
    }

    pub(crate) async fn call(self: &Arc<Self>, verb: &str, bytes: &[u8]) -> Response {
        let started = tokio::time::Instant::now();
        let Some(mut call) = Call::parse(bytes) else {
            return unavailable("", "shape");
        };
        let verb = if verb == "lifecycle-signin" {
            let Some((deadline, args)) =
                crate::lifecycle_signin::agent_start(&self.root, call.args.get())
            else {
                return unavailable(&call.call_id, "shape");
            };
            if args["jobId"].as_str() != Some(&call.call_id) {
                return unavailable(&call.call_id, "shape");
            }
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
        if !call.valid(verb) {
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
        let Some(permit) = self.permit() else {
            return unavailable(&call.call_id, "busy");
        };
        let deadline = started + Duration::from_millis(budget);
        let client = self.client(&call.computer, deadline).await;
        let Some(client) = client else {
            return unavailable(&call.call_id, "unavailable");
        };
        let wire = call.wire(verb);
        let Ok(request) = serde_json::from_str::<Request>(&wire) else {
            return unavailable(&call.call_id, "shape");
        };
        let mut options = CallOptions::new(Duration::ZERO, call.max_bytes);
        options.deadline = deadline;
        options.scan = verb == "scan-usage";
        if verb == "apps"
            && call.computer.ssh_host.is_none()
            && matches!(request.args["action"].as_str(), Some("update" | "cancel"))
        {
            options.local_app_update_state_dir = Some(self.root.join("app-updates"));
        }
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
                        if matches!(
                            request.verb.as_str(),
                            "cursor-open" | "key-store" | "muse-login-identity" | "signin-session"
                        ) && failure.cause == Cause::Refused
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

    /// In-process verb call for the Open orchestrator: the terminal result value, or None
    /// when the agent falls back, fails or cannot be reached. The same admission as `call`,
    /// without the streaming control channel these verbs do not use.
    pub(crate) async fn call_value(
        &self,
        verb: &str,
        computer: Selector,
        args: Value,
        deadline_ms: u64,
        timeout_ms: u64,
        max_bytes: usize,
    ) -> Option<Value> {
        if !matches!(
            verb,
            "claude-host" | "claude-history" | "signin-session" | "muse-login-identity"
        ) || deadline_ms == 0
            || deadline_ms > aac_protocol::MAX_DEADLINE_MS
            || timeout_ms == 0
            || timeout_ms > deadline_ms + 5000
            || max_bytes == 0
            || max_bytes > frame_cap(verb) * 2
        {
            return None;
        }
        let budget = timeout_ms
            .checked_sub(FRONT_DOOR_MARGIN_MS)
            .filter(|ms| *ms > 0)?;
        let permit = self.slots.clone().try_acquire_owned().ok()?;
        let deadline = tokio::time::Instant::now() + Duration::from_millis(budget);
        let registry = self.registered(deadline).await?;
        let client = registry.as_ref().as_ref().ok()?.client(&computer)?;
        let call_id = random_call_id()?;
        let args_text = serde_json::to_string(&args).ok()?;
        let wire = format!(
            "{{\"protocol\":1,\"verb\":\"{verb}\",\"id\":\"{call_id}\",\"deadlineMs\":{deadline_ms},\"args\":{args_text}}}"
        );
        let request = serde_json::from_str::<Request>(&wire).ok()?;
        let mut options = CallOptions::new(Duration::ZERO, max_bytes);
        options.deadline = deadline;
        let events = client.call(&request, &options).await.ok()?;
        drop(permit);
        match events.last() {
            Some(Event::Result { value, .. }) if *value != json!({"fallback":"python"}) => {
                Some(value.clone())
            }
            _ => None,
        }
    }

    pub(crate) async fn interactive_command(
        &self,
        selector: &Selector,
        argv: &[std::ffi::OsString],
    ) -> Option<aac_transport::InteractiveCommand> {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let registry = self.registered(deadline).await?;
        let computer = registry.as_ref().as_ref().ok()?.computer(selector)?;
        tokio::time::timeout_at(deadline, computer.interactive_command(argv, deadline))
            .await
            .ok()?
            .ok()
    }

    /// One fixed reference command over the identical SSH policy, without the agent: the
    /// Open orchestrator's fallback when no verb proves a step. The cause distinguishes a
    /// deadline kill from any other transport failure.
    pub(crate) async fn reference(
        &self,
        computer: Selector,
        argv: Vec<std::ffi::OsString>,
        stdin: Vec<u8>,
        timeout_ms: u64,
        max_bytes: usize,
    ) -> Result<ProcessOutput, Cause> {
        let deadline = tokio::time::Instant::now() + Duration::from_millis(timeout_ms);
        let registry = self.registered(deadline).await.ok_or(Cause::Refused)?;
        let handle = registry
            .as_ref()
            .as_ref()
            .ok()
            .ok_or(Cause::Refused)?
            .computer(&computer)
            .ok_or(Cause::Refused)?;
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Cause::Refused)?;
        let mut options = CallOptions::new(
            Duration::from_millis(timeout_ms),
            max_bytes.saturating_add(65536),
        );
        options.deadline = deadline;
        let result = handle.execute(&argv, &stdin, &options).await;
        drop(permit);
        result.map_err(|failure| failure.cause)
    }

    /// G14: an in-process `scan-usage` call for the activity scheduler. Returns the scanner's
    /// answer value (the v2 response) or a named error. Unlike [`Self::call`], it collects the
    /// terminal frame instead of streaming NDJSON to the front door: the scheduler owns the
    /// cadence and reads the rows itself, so no front-door control channel is registered.
    pub(crate) async fn scan_usage(
        self: &Arc<Self>,
        platform: &str,
        ssh_host: Option<&str>,
        args: &Value,
        deadline_ms: u64,
        max_bytes: usize,
        call_id: &str,
    ) -> Result<Value, String> {
        let selector = Selector {
            platform: platform.to_string(),
            ssh_host: ssh_host.map(str::to_string),
        };
        let started = tokio::time::Instant::now();
        let budget = deadline_ms
            .checked_sub(FRONT_DOOR_MARGIN_MS)
            .filter(|ms| *ms > 0)
            .ok_or_else(|| "deadline".to_string())?;
        let permit = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| "busy".to_string())?;
        let deadline = started + Duration::from_millis(budget);
        let registry = self.registered(deadline).await;
        let client = registry
            .as_deref()
            .and_then(|result| result.as_ref().ok())
            .and_then(|registry| registry.client(&selector))
            .ok_or_else(|| "unavailable".to_string())?;
        let args_text = serde_json::to_string(args).map_err(|_| "shape".to_string())?;
        let wire = format!(
            "{{\"protocol\":1,\"verb\":\"scan-usage\",\"id\":\"{call_id}\",\"deadlineMs\":{deadline_ms},\"args\":{args_text}}}"
        );
        let request = serde_json::from_str::<Request>(&wire).map_err(|_| "shape".to_string())?;
        let mut options = CallOptions::new(Duration::ZERO, max_bytes);
        options.deadline = deadline;
        options.scan = true;
        let (_input, controls) = mpsc::channel(8);
        let (progress, _events) = mpsc::channel(4);
        let _permit = permit;
        let events = client
            .call_controlled_encoded(&request, wire.as_bytes(), &options, controls, progress)
            .await
            .map_err(|_| "transport".to_string())?;
        match events.last() {
            Some(Event::Result { value, .. }) if value != &json!({"fallback":"python"}) => {
                Ok(value.clone())
            }
            Some(Event::Failure { failure, .. }) => Err(format!("{:?}", failure.cause)),
            _ => Err("reply".to_string()),
        }
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

pub(crate) enum MutationReply {
    Result(Value),
    NotDispatched,
    Uncertain,
}

impl Agents {
    /// Decide whether the caller can run a reference mutation without repeating an uncertain write.
    pub(crate) async fn call_mutation(
        self: &Arc<Self>,
        verb: &str,
        computer: &Selector,
        args: Value,
        deadline_ms: u64,
    ) -> MutationReply {
        let Some(id) = random_call_id() else {
            return MutationReply::Uncertain;
        };
        let call = json!({
            "computer":computer,"args":args,"deadlineMs":deadline_ms,"timeoutMs":deadline_ms+2000,
            "maxBytes":4096,"callId":id
        });
        let response = self.call(verb, call.to_string().as_bytes()).await;
        let Ok(bytes) = axum::body::to_bytes(response.into_body(), 4096).await else {
            return MutationReply::Uncertain;
        };
        let Ok(mut frame) = serde_json::from_slice::<Value>(&bytes) else {
            return MutationReply::Uncertain;
        };
        if frame["protocol"] != 1 || frame["id"] != id {
            return MutationReply::Uncertain;
        }
        match frame["type"].as_str() {
            Some("result") => MutationReply::Result(frame["value"].take()),
            Some("fallback")
                if matches!(
                    frame["reason"].as_str(),
                    Some("shape" | "unavailable" | "busy" | "unsupported")
                ) =>
            {
                MutationReply::NotDispatched
            }
            _ => MutationReply::Uncertain,
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
