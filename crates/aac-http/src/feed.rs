//! L1: the front door's feeds and the service's event stream (the "fed" level of the M3 design).
//! `PUT /internal/feed/<source>` stores a source's latest generation `{generation, data, facts}`;
//! `GET /internal/feed/<source>` reports which generation is held; `GET /internal/events` streams
//! NDJSON events to the front door. Only the reserved front-door principal reaches these routes
//! (the table's `service` role, plus the boot secret). Route groups that compose from feeds (R1
//! onward) read them here; until then a feed is held and reported, never served.
use aac_views::decline::Decline;
use axum::body::{Body, Bytes};
use serde::Deserialize;
use serde_json::{json, value::RawValue};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
};
use tokio::sync::broadcast;

/// At most this many sources are held.
const SOURCES: usize = 16;

struct Feed {
    generation: String,
    bytes: usize,
    facts: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Put {
    generation: String,
    data: Box<RawValue>,
    facts: Box<RawValue>,
}

/// The held feeds and the event channel.
#[derive(Clone)]
pub(crate) struct Feeds {
    held: Arc<Mutex<HashMap<String, Feed>>>,
    events: broadcast::Sender<Bytes>,
}

fn source_word(source: &str) -> bool {
    let mut bytes = source.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && source.len() <= 32
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn generation_word(generation: &str) -> bool {
    (1..=64).contains(&generation.len())
        && generation
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:-".contains(&b))
}

impl Feeds {
    pub(crate) fn new() -> Self {
        Self {
            held: Arc::new(Mutex::new(HashMap::new())),
            events: broadcast::channel(64).0,
        }
    }

    fn publish(&self, event: serde_json::Value) {
        let mut line = event.to_string().into_bytes();
        line.push(b'\n');
        // No subscriber is not an error: the front door opens the stream when it needs it.
        let _ = self.events.send(Bytes::from(line));
    }

    /// Store a generation. A generation without facts asks the front door for them
    /// (`facts-needed`): composers serve only generations whose facts are present.
    pub(crate) fn put(&self, source: &str, body: &[u8]) -> Result<serde_json::Value, Decline> {
        if !source_word(source) {
            return Err(Decline::Unsupported);
        }
        let put: Put = crate::json_object::from_slice(body, &["generation", "data", "facts"])
            .map_err(|_| Decline::Shape)?;
        if !generation_word(&put.generation) {
            return Err(Decline::Shape);
        }
        let facts = put.facts.get().trim() != "null";
        {
            let mut held = self.held.lock().map_err(|_| Decline::Internal)?;
            if held.len() >= SOURCES && !held.contains_key(source) {
                return Err(Decline::Busy);
            }
            held.insert(
                source.to_owned(),
                Feed {
                    generation: put.generation.clone(),
                    bytes: put.data.get().len(),
                    facts,
                },
            );
        }
        if !facts {
            self.publish(json!({
                "event": "facts-needed",
                "source": source,
                "generation": put.generation,
            }));
        }
        Ok(json!({"source": source, "generation": put.generation, "facts": facts}))
    }

    /// The generation held for a source, or `stale` when none is.
    pub(crate) fn status(&self, source: &str) -> Result<serde_json::Value, Decline> {
        let held = self.held.lock().map_err(|_| Decline::Internal)?;
        let feed = held.get(source).ok_or(Decline::Stale)?;
        Ok(json!({
            "source": source,
            "generation": feed.generation,
            "facts": feed.facts,
            "bytes": feed.bytes,
        }))
    }

    /// An NDJSON body that streams every event published after it opened.
    pub(crate) fn stream(&self) -> Body {
        let receiver = self.events.subscribe();
        let stream = futures_util::stream::unfold(receiver, |mut receiver| async move {
            let line = match receiver.recv().await {
                Ok(line) => line,
                Err(broadcast::error::RecvError::Lagged(missed)) => {
                    Bytes::from(format!("{{\"event\":\"lagged\",\"missed\":{missed}}}\n"))
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            };
            Some((Ok::<Bytes, std::io::Error>(line), receiver))
        });
        Body::from_stream(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn feeds_accept_only_exact_envelopes_and_named_sources() {
        let feeds = Feeds::new();
        assert!(
            feeds
                .put("home", br#"{"generation":"g1","data":{"a":1},"facts":{}}"#)
                .is_ok()
        );
        assert_eq!(
            feeds.status("home").map(|v| v["generation"].clone()),
            Ok(json!("g1"))
        );
        assert_eq!(feeds.status("status"), Err(Decline::Stale));
        for (source, body, reason) in [
            (
                "Home",
                r#"{"generation":"g","data":1,"facts":null}"#,
                Decline::Unsupported,
            ),
            (
                "home/x",
                r#"{"generation":"g","data":1,"facts":null}"#,
                Decline::Unsupported,
            ),
            ("home", r#"{"generation":"g","data":1}"#, Decline::Shape),
            (
                "home",
                r#"{"generation":"g","data":1,"facts":null,"x":0}"#,
                Decline::Shape,
            ),
            (
                "home",
                r#"{"generation":"","data":1,"facts":null}"#,
                Decline::Shape,
            ),
            (
                "home",
                r#"{"generation":"a b","data":1,"facts":null}"#,
                Decline::Shape,
            ),
            ("home", r#"[1,2,3]"#, Decline::Shape),
        ] {
            assert_eq!(
                feeds.put(source, body.as_bytes()).err(),
                Some(reason),
                "{source} {body}"
            );
        }
        for i in 0..SOURCES - 1 {
            assert!(
                feeds
                    .put(
                        &format!("s{i}"),
                        br#"{"generation":"g","data":1,"facts":{}}"#
                    )
                    .is_ok()
            );
        }
        assert_eq!(
            feeds
                .put("one-too-many", br#"{"generation":"g","data":1,"facts":{}}"#)
                .err(),
            Some(Decline::Busy)
        );
    }
}
