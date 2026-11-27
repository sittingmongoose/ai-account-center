//! H2: a single bounded collection pass per matching request, on the shared L5 registry.
use crate::agent_transport::{Agents, Call, FRONT_DOOR_MARGIN_MS, fallback, line, response};
use aac_core::Failure;
use aac_protocol::{Event, MAX_FRAME_BYTES, Request};
use aac_transport::{CallOptions, CancellationToken};
use aac_views::decline::Decline;
use axum::{body::Body, response::Response};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{Semaphore, watch},
    time::Instant,
};

const VERBS: [&str; 4] = [
    "quota-read",
    "signin-expiry",
    "failover-inventory",
    "failover-collect",
];
const MAX_PENDING: usize = 16;

/// The agent verb that answers one collection request, or `None` for the managed Python
/// helper. Claude inventory and collection are native in the agent on every platform (its
/// `failover-inventory` verb serves both actions), so Claude never reaches the helper.
fn agent_verb(verb: &str, args: &Value) -> Option<&'static str> {
    let verb = VERBS.into_iter().find(|v| *v == verb)?;
    if verb.starts_with("failover-") && args["provider"] == "claude" {
        return Some("failover-inventory");
    }
    // Wallet inventory uses the existing capsule parser, which is not an M2 agent reader.
    let wallet = verb == "failover-inventory" && args["provider"] == "opencode-wallet";
    (verb != "failover-collect" && !wallet).then_some(verb)
}

/// Whether an agent that declines a request (an older agent's `{"fallback":"python"}`) may
/// hand it to the helper. Claude may not: its answer is the agent's, or honestly none.
fn helper_may_answer(verb: &str, args: &Value) -> bool {
    verb == "failover-inventory" && args["provider"] != "claude"
}
type Key = [u8; 32];
enum Collected {
    Value(Value),
    Failure(Failure),
}
type Reply = Result<Arc<Collected>, &'static str>;
type Pending = watch::Sender<Option<Reply>>;

pub(crate) struct Collection {
    pending: Mutex<HashMap<Key, Pending>>,
    timers: Semaphore,
    helpers: Semaphore,
    stop: CancellationToken,
}

impl Default for Collection {
    fn default() -> Self {
        Self {
            pending: Mutex::default(),
            timers: Semaphore::new(4),
            // R5b: one python3 failover helper at a time. Three concurrent interpreters
            // stacked ~100 MiB of helper RSS on the main process; the deadline-bounded
            // acquire below keeps the wait honest (a helper that cannot start in time is
            // "busy", never a stalled pass).
            helpers: Semaphore::new(1),
            stop: CancellationToken::new(),
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Tick {
    interval_ms: u64,
}

impl Collection {
    pub(crate) async fn next_tick(&self, bytes: &[u8]) -> Result<(), Decline> {
        let tick = crate::json_object::from_slice::<Tick>(bytes, &["intervalMs"])
            .map_err(|_| Decline::Shape)?;
        if !(30_000..=3_600_000).contains(&tick.interval_ms) {
            return Err(Decline::Shape);
        }
        let _permit = self.timers.try_acquire().map_err(|_| Decline::Busy)?;
        tokio::select! {
            () = self.stop.cancelled() => Err(Decline::Unsupported),
            () = tokio::time::sleep(Duration::from_millis(tick.interval_ms)) => Ok(()),
        }
    }

    pub(crate) fn stop(&self) {
        self.stop.cancel();
    }

    async fn read_helper(
        &self,
        agents: &Agents,
        computer: &crate::agent_transport::Selector,
        args: &Value,
        deadline: Instant,
        cap: usize,
    ) -> Reply {
        let client = agents
            .client(computer, deadline)
            .await
            .ok_or("unavailable")?;
        let _permit = tokio::time::timeout_at(deadline, self.helpers.acquire())
            .await
            .map_err(|_| "busy")?
            .map_err(|_| "busy")?;
        // MEM5 helpers (d): a local helper holds the single local-work slot, so it never
        // overlaps a local scan; a remote helper runs on the remote machine and holds none.
        let _local_work = agents
            .local_work_permit(computer, deadline)
            .await
            .map_err(|_| "busy")?;
        let result = crate::account_usage::helper::collect(
            client.computer(),
            agents.root(),
            args,
            deadline,
            cap,
        )
        .await?;
        Ok(Arc::new(Collected::Value(result)))
    }

    /// The standalone producer uses the same registry, deadline and coalescing as the bridge.
    pub(crate) async fn read_value(
        self: &Arc<Self>,
        agents: Arc<Agents>,
        platform: &str,
        ssh_host: Option<&str>,
        verb: &str,
        args: &Value,
    ) -> Result<Value, &'static str> {
        let deadline = if verb == "failover-collect" {
            35_000
        } else {
            15_000
        };
        let bytes = serde_json::to_vec(&json!({
            "computer":{"platform":platform,"sshHost":ssh_host},
            "args":args,"deadlineMs":deadline,"timeoutMs":deadline+1000,
            "maxBytes":MAX_FRAME_BYTES,"callId":"native-account-usage",
        }))
        .map_err(|_| "shape")?;
        let call = Call::parse(&bytes).ok_or("shape")?;
        let request = call.collection_request(verb).ok_or("shape")?;
        let reply = self
            .collect(agents.clone(), &call, request, Instant::now())
            .await;
        let result = match reply {
            Err("unsupported") if helper_may_answer(verb, args) => {
                self.read_helper(
                    &agents,
                    &call.computer,
                    args,
                    Instant::now() + Duration::from_secs(12),
                    call.max_bytes,
                )
                .await?
            }
            other => other?,
        };
        match result.as_ref() {
            Collected::Value(value) => Ok(value.clone()),
            Collected::Failure(_) => Err("reply"),
        }
    }

    pub(crate) async fn read(
        self: &Arc<Self>,
        agents: Arc<Agents>,
        verb: &str,
        bytes: &[u8],
    ) -> Response {
        let started = Instant::now();
        let Some(call) = Call::parse(bytes) else {
            return response(Body::from(fallback("", "shape")));
        };
        let reply = match call.collection_request(verb) {
            Some(request) => self.collect(agents, &call, request, started).await,
            None => Err("shape"),
        };
        let bytes = match reply {
            Ok(outcome) => match outcome.as_ref() {
                Collected::Value(value) => line(&json!({
                    "type": "result", "protocol": 1, "id": call.call_id, "value": value
                })),
                Collected::Failure(failure) => line(&json!({
                    "type": "failure", "protocol": 1, "id": call.call_id, "failure": failure
                })),
            },
            Err(reason) => fallback(&call.call_id, reason),
        };
        if bytes.len() <= call.max_bytes {
            response(Body::from(bytes))
        } else {
            response(Body::from(fallback(&call.call_id, "reply")))
        }
    }

    async fn collect(
        self: &Arc<Self>,
        agents: Arc<Agents>,
        call: &Call,
        request: Request,
        started: Instant,
    ) -> Reply {
        // The caller's remaining allowance is part of the key. A shorter failover
        // inventory pass cannot take over a longer one or borrow its budget.
        let key: Key = Sha256::digest(
            serde_json::to_vec(&(
                &request.verb,
                &call.computer,
                call.args.get(),
                call.deadline_ms,
                call.timeout_ms,
                call.max_bytes,
            ))
            .map_err(|_| "shape")?,
        )
        .into();
        let deadline = started + Duration::from_millis(call.timeout_ms - FRONT_DOOR_MARGIN_MS);
        let mut receiver = {
            let mut pending = self.pending.lock().map_err(|_| "internal")?;
            if let Some(entry) = pending.get(&key) {
                entry.subscribe()
            } else {
                if pending.len() >= MAX_PENDING {
                    return Err("busy");
                }
                let (sender, receiver) = watch::channel(None);
                pending.insert(key, sender.clone());
                let collection = self.clone();
                let computer = call.computer.clone();
                let cap = call.max_bytes;
                let route = agent_verb(&request.verb, &request.args);
                let wire = route.map(|verb| call.wire(verb));
                tokio::spawn(async move {
                    // A lost dashboard connection does not spawn a second provider read.
                    // The original pass still owns its slot and absolute deadline.
                    let result = match (route, wire) {
                        (Some(verb), Some(wire)) => {
                            let mut request = request;
                            request.verb = verb.into();
                            read_agent(&agents, computer, request, wire, deadline, cap).await
                        }
                        _ => {
                            collection
                                .read_helper(&agents, &computer, &request.args, deadline, cap)
                                .await
                        }
                    };
                    let mut pending = collection.pending.lock().unwrap_or_else(|e| e.into_inner());
                    sender.send_replace(Some(result));
                    pending.remove(&key);
                });
                receiver
            }
        };
        tokio::time::timeout_at(deadline + Duration::from_millis(100), async {
            receiver
                .wait_for(Option::is_some)
                .await
                .map_err(|_| "transport")?
                .clone()
                .ok_or("transport")?
        })
        .await
        .unwrap_or(Err("transport"))
    }
}

impl Call {
    fn collection_request(&self, verb: &str) -> Option<Request> {
        let helper = verb == "failover-collect";
        let (deadline, cap) = if helper {
            (35_000, MAX_FRAME_BYTES * 2)
        } else {
            (15_000, MAX_FRAME_BYTES)
        };
        if !VERBS.contains(&verb)
            || !self.bounded(verb)
            || self.deadline_ms > deadline
            || self.timeout_ms <= FRONT_DOOR_MARGIN_MS
            || self.max_bytes < 256
            || self.max_bytes > cap
        {
            return None;
        }
        let request = serde_json::from_str::<Request>(&self.wire(verb)).ok()?;
        request.validate().ok()?;
        if (verb == "failover-inventory" && request.args["action"] != "inventory")
            || (helper && request.args["action"] != "collect")
        {
            return None;
        }
        Some(request)
    }
}

async fn read_agent(
    agents: &Agents,
    computer: crate::agent_transport::Selector,
    request: Request,
    wire: String,
    deadline: Instant,
    cap: usize,
) -> Reply {
    let _permit = agents.permit().ok_or("busy")?;
    // A local quota/sign-in agent waits for the local-agent gate within its deadline.
    let _local_permit = agents
        .local_agent_permit(&computer, deadline)
        .await
        .map_err(|_| "busy")?;
    let client = agents
        .client(&computer, deadline)
        .await
        .ok_or("unavailable")?;
    let mut options = CallOptions::new(Duration::ZERO, cap);
    options.deadline = deadline;
    let (input, controls) = tokio::sync::mpsc::channel(1);
    let (progress, mut events) = tokio::sync::mpsc::channel(4);
    let result =
        client.call_controlled_encoded(&request, wire.as_bytes(), &options, controls, progress);
    tokio::pin!(result);
    let events = loop {
        tokio::select! {
            result = &mut result => break result.map_err(|_| "transport")?,
            Some(_) = events.recv() => {}
        }
    };
    drop(input);
    match events.into_iter().last() {
        Some(Event::Result { value, .. }) if value == json!({"fallback": "python"}) => {
            Err("unsupported")
        }
        Some(Event::Result { value, .. }) => Ok(Arc::new(Collected::Value(value))),
        Some(Event::Failure { failure, .. }) => Ok(Arc::new(Collected::Failure(failure))),
        _ => Err("reply"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_collection_runs_in_the_agent_and_never_in_the_python_helper() {
        let claude = json!({"version":1,"action":"collect","provider":"claude"});
        assert_eq!(
            agent_verb("failover-collect", &claude),
            Some("failover-inventory")
        );
        let inventory = json!({"version":1,"action":"inventory","provider":"claude"});
        assert_eq!(
            agent_verb("failover-inventory", &inventory),
            Some("failover-inventory")
        );
        assert!(!helper_may_answer("failover-inventory", &inventory));
        assert!(!helper_may_answer("failover-collect", &claude));
        // Providers without a native collector keep their existing routes.
        let codex = json!({"version":1,"action":"collect","provider":"codex"});
        assert_eq!(agent_verb("failover-collect", &codex), None);
        let wallet = json!({"version":1,"action":"inventory","provider":"opencode-wallet"});
        assert_eq!(agent_verb("failover-inventory", &wallet), None);
        let codex = json!({"version":1,"action":"inventory","provider":"codex"});
        assert_eq!(
            agent_verb("failover-inventory", &codex),
            Some("failover-inventory")
        );
        assert!(helper_may_answer("failover-inventory", &codex));
        assert_eq!(agent_verb("quota-read", &json!({})), Some("quota-read"));
        assert_eq!(agent_verb("unknown", &json!({})), None);
    }

    #[tokio::test(start_paused = true)]
    async fn schedules_the_exact_configured_interval_and_releases_waiters_on_stop() {
        let collection = Arc::new(Collection::default());
        let schedule = || {
            let collection = collection.clone();
            tokio::spawn(async move { collection.next_tick(br#"{"intervalMs":45000}"#).await })
        };
        let tick = schedule();
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(44_999)).await;
        assert!(!tick.is_finished());
        tokio::time::advance(Duration::from_millis(1)).await;
        assert_eq!(tick.await.expect("timer"), Ok(()));
        let tick = schedule();
        collection.stop();
        assert_eq!(tick.await.expect("timer"), Err(Decline::Unsupported));
        assert_eq!(
            collection.next_tick(br#"{"intervalMs":29999}"#).await,
            Err(Decline::Shape)
        );
    }
}
