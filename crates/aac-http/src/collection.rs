//! H2: a single bounded collection pass per matching request, on the shared L5 registry.
use crate::agent_transport::{Agents, Call, FRONT_DOOR_MARGIN_MS, fallback, line, response};
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

const VERBS: [&str; 3] = ["quota-read", "signin-expiry", "failover-inventory"];
const MAX_PENDING: usize = 16;
type Key = [u8; 32];
type Reply = Result<Arc<Value>, &'static str>;
type Pending = watch::Sender<Option<Reply>>;

pub(crate) struct Collection {
    pending: Mutex<HashMap<Key, Pending>>,
    timers: Semaphore,
    stop: CancellationToken,
}

impl Default for Collection {
    fn default() -> Self {
        Self {
            pending: Mutex::default(),
            timers: Semaphore::new(4),
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
            Ok(value) => line(&json!({
                "type": "result", "protocol": 1, "id": call.call_id, "value": value.as_ref()
            })),
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
                let wire = call.wire(&request.verb);
                tokio::spawn(async move {
                    // A lost dashboard connection does not spawn a second provider read.
                    // The original pass still owns its slot and absolute deadline.
                    let result = read_agent(&agents, computer, request, wire, deadline, cap).await;
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
        if !VERBS.contains(&verb)
            || !self.valid(verb)
            || self.deadline_ms > 15_000
            || self.timeout_ms <= FRONT_DOOR_MARGIN_MS
            || self.max_bytes < 256
            || self.max_bytes > MAX_FRAME_BYTES
        {
            return None;
        }
        let request = serde_json::from_str::<Request>(&self.wire(verb)).ok()?;
        request.validate().ok()?;
        if verb == "failover-inventory" && request.args["action"] != "inventory" {
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
        Some(Event::Result { value, .. }) => Ok(Arc::new(value)),
        _ => Err("reply"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
