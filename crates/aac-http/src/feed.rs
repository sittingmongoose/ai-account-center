//! L1: the front door's feeds and the service's event stream (the "fed" level of the M3 design).
//! `PUT /internal/feed/<source>` takes a source's latest generation `{generation, data, facts}`;
//! `GET /internal/feed/<source>` reports which generation is held; `GET /internal/events` streams
//! NDJSON events to the front door. Only the reserved front-door principal reaches these routes
//! (the table's `service` role, plus the boot secret).
//!
//! R1: each source holds its latest generation's immutable bounded payload and facts (the request
//! body is capped at 4 MiB, and at most 16 sources are held). A prepared Home dashboard holds its
//! raw text and facts, its parsed value and its byte template (the fixed parts once, each flippable
//! window twice), about eight times the text for a string-heavy dashboard (measured; R1b: the
//! template keeps no second identity copy), plus its view memos (bounded per generation,
//! `aac_views::home::memo`). The bound is therefore a small multiple of 4 MiB per dashboard
//! source, not 4 MiB, and up to four dashboard sources are possible (`mac|windows` x `0|1`):
//! four 3.69 MiB fixture dashboards retained +118.6 MiB RSS over a 5.8 MiB idle release service
//! (Nas1, MEASURE-qwen-lane-2-feed-rss.json), so four sources at the cap can retain on the order
//! of 128 MiB, beside the other sources' own bounded payloads. Home's sources are prepared once,
//! when their generation arrives (`aac_views::home::fed`): `home-dashboard-<platform>-<0|1>` (the
//! dashboard answer per platform and secure transport), `home-opens` (the Claude Open progress) and
//! `home-antigravity` (the composed Antigravity inventory). A source that names a numeric
//! generation only moves forward: an older one arriving late (two pushes racing) is not taken, and
//! the reply names the generation held, so the front door knows. A generation is immutable: the
//! same id with other bytes is refused. A request names the generations its front door read; any
//! other held generation, or none, is `stale`, and TS answers.
//!
//! R2: Status's six sources (`status-*`, crate::status_fed) are parsed once when they arrive, like Home's, and
//! need numeric generations too.
use aac_views::decline::Decline;
use aac_views::home::fed;
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
/// The dashboard sources' prefix; the rest of the name is `<platform>-<0|1>`.
pub(crate) const HOME_DASHBOARD: &str = "home-dashboard-";
pub(crate) const HOME_OPENS: &str = "home-opens";
pub(crate) const HOME_ANTIGRAVITY: &str = "home-antigravity";

/// What a generation was prepared into when it arrived. Every request for it reads this.
#[derive(Debug)]
pub(crate) enum Prepared {
    /// A source no composer reads yet: held as sent.
    Held,
    Dashboard(Result<fed::Dashboard, Decline>),
    Opens(Result<fed::Opens, Decline>),
    Antigravity(Result<serde_json::Value, Decline>),
    /// R2: a Status source's data object (and, for `status-facts`, its presentation facts).
    Status(Result<crate::status_fed::Part, Decline>),
}

/// One held generation: immutable.
#[derive(Debug)]
pub(crate) struct Generation {
    pub(crate) id: String,
    pub(crate) data: Box<RawValue>,
    pub(crate) facts: Box<RawValue>,
    pub(crate) prepared: Prepared,
    /// R1b: SHA-256 of the generation's data and facts, once: what an answer built from it names in its
    /// `ETag` (Home's tag digests its inputs, not its 200 KB of bytes).
    pub(crate) digest: [u8; 32],
}

impl Generation {
    fn has_facts(&self) -> bool {
        self.facts.get().trim() != "null"
    }
}

fn digest(data: &RawValue, facts: &RawValue) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(data.get().len().to_le_bytes());
    hash.update(data.get().as_bytes());
    hash.update(facts.get().as_bytes());
    hash.finalize().into()
}

/// Sources whose generations never carry facts: no `facts-needed` for them (review L3).
fn without_facts(source: &str) -> bool {
    source == HOME_OPENS || source == HOME_ANTIGRAVITY
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
    held: Arc<Mutex<HashMap<String, Arc<Generation>>>>,
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

/// A numeric generation (digits, no leading zero), which orders.
fn number(generation: &str) -> Option<u64> {
    (generation == "0" || !generation.starts_with('0'))
        .then(|| generation.parse().ok())
        .flatten()
}

/// Home's sources need a numeric generation, so a late push never replaces a newer one.
fn home_source(source: &str) -> bool {
    source == HOME_OPENS
        || source == HOME_ANTIGRAVITY
        || source
            .strip_prefix(HOME_DASHBOARD)
            .is_some_and(|rest| ["mac-0", "mac-1", "windows-0", "windows-1"].contains(&rest))
}

fn prepare(source: &str, data: &RawValue, facts: &RawValue) -> Prepared {
    let bytes = data.get().as_bytes();
    if source.starts_with(HOME_DASHBOARD) {
        Prepared::Dashboard(fed::prepare_dashboard(bytes, facts.get().as_bytes()))
    } else if source == HOME_OPENS {
        Prepared::Opens(fed::prepare_opens(bytes))
    } else if source == HOME_ANTIGRAVITY {
        Prepared::Antigravity(fed::prepare_antigravity(bytes))
    } else if crate::status_fed::source(source) {
        Prepared::Status(crate::status_fed::prepare(source, data, facts))
    } else {
        Prepared::Held
    }
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

    /// Whether generation `id` (with these bytes) may replace what is held for a source:
    /// `Ok(true)` to take it, `Ok(false)` when it or a newer one is held (the reply names it), an
    /// error when the same id comes with other bytes (a generation is immutable).
    fn newer(
        held: Option<&Arc<Generation>>,
        id: &str,
        data: &str,
        facts: &str,
    ) -> Result<bool, Decline> {
        let Some(held) = held else {
            return Ok(true);
        };
        if held.id == id {
            return if held.data.get() == data && held.facts.get() == facts {
                Ok(false)
            } else {
                Err(Decline::Shape)
            };
        }
        Ok(match (number(&held.id), number(id)) {
            (Some(old), Some(new)) => new > old,
            // A source that does not name numbers: the latest push wins (L1).
            _ => true,
        })
    }

    /// Store a generation. A generation without facts asks the front door for them
    /// (`facts-needed`): composers serve only generations whose facts are present.
    pub(crate) fn put(&self, source: &str, body: &[u8]) -> Result<serde_json::Value, Decline> {
        if !source_word(source) {
            return Err(Decline::Unsupported);
        }
        let put: Put = crate::json_object::from_slice(body, &["generation", "data", "facts"])
            .map_err(|_| Decline::Shape)?;
        let ordered = home_source(source) || crate::status_fed::source(source);
        if !generation_word(&put.generation)
            || (source.starts_with("home-") && !home_source(source))
            || (source.starts_with("status-") && !crate::status_fed::source(source))
            || (ordered && number(&put.generation).is_none())
        {
            return Err(Decline::Shape);
        }
        {
            let held = self.held.lock().map_err(|_| Decline::Internal)?;
            if held.len() >= SOURCES && !held.contains_key(source) {
                return Err(Decline::Busy);
            }
            if !Self::newer(
                held.get(source),
                &put.generation,
                put.data.get(),
                put.facts.get(),
            )? {
                let current = held.get(source).ok_or(Decline::Internal)?;
                return Ok(
                    json!({"source": source, "generation": current.id, "facts": current.has_facts()}),
                );
            }
        }
        // Prepared outside the lock (a large dashboard takes a moment); the order is checked again.
        let generation = Arc::new(Generation {
            prepared: prepare(source, &put.data, &put.facts),
            digest: digest(&put.data, &put.facts),
            id: put.generation.clone(),
            data: put.data,
            facts: put.facts,
        });
        let facts = generation.has_facts();
        let reply = {
            let mut held = self.held.lock().map_err(|_| Decline::Internal)?;
            if Self::newer(
                held.get(source),
                &generation.id,
                generation.data.get(),
                generation.facts.get(),
            )? {
                if held.len() >= SOURCES && !held.contains_key(source) {
                    return Err(Decline::Busy);
                }
                held.insert(source.to_owned(), generation.clone());
                json!({"source": source, "generation": generation.id, "facts": facts})
            } else {
                let current = held.get(source).ok_or(Decline::Internal)?;
                return Ok(
                    json!({"source": source, "generation": current.id, "facts": current.has_facts()}),
                );
            }
        };
        if !facts && !without_facts(source) {
            self.publish(json!({
                "event": "facts-needed",
                "source": source,
                "generation": generation.id,
            }));
        }
        Ok(reply)
    }

    /// The generation held for a source, if it is the one named.
    pub(crate) fn current(
        &self,
        source: &str,
        generation: &str,
    ) -> Result<Arc<Generation>, Decline> {
        let held = self.held.lock().map_err(|_| Decline::Internal)?;
        match held.get(source) {
            Some(g) if g.id == generation => Ok(g.clone()),
            _ => Err(Decline::Stale),
        }
    }

    /// The generation held for a source, or `stale` when none is.
    pub(crate) fn status(&self, source: &str) -> Result<serde_json::Value, Decline> {
        let held = self.held.lock().map_err(|_| Decline::Internal)?;
        let feed = held.get(source).ok_or(Decline::Stale)?;
        let declined = match &feed.prepared {
            Prepared::Held => None,
            Prepared::Dashboard(r) => r.as_ref().err().copied(),
            Prepared::Opens(r) => r.as_ref().err().copied(),
            Prepared::Antigravity(r) => r.as_ref().err().copied(),
            Prepared::Status(r) => r.as_ref().err().copied(),
        };
        Ok(json!({
            "source": source,
            "generation": feed.id,
            "facts": feed.has_facts(),
            "bytes": feed.data.get().len(),
            "declined": declined.map(Decline::as_str),
        }))
    }

    /// The latest generation held for a source, whatever its id (R4: the owned readers'
    /// flushes read the front door's last pins without naming a generation).
    pub(crate) fn latest(&self, source: &str) -> Option<Arc<Generation>> {
        self.held.lock().ok()?.get(source).cloned()
    }

    /// Publish one event on the front door's stream (R4: `status-changed`, `scoreboard-changed`).
    pub(crate) fn publish_event(&self, event: serde_json::Value) {
        self.publish(event);
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

    fn held(feeds: &Feeds, source: &str) -> Option<String> {
        feeds
            .status(source)
            .ok()
            .and_then(|v| v["generation"].as_str().map(str::to_owned))
    }

    #[test]
    fn sources_without_facts_ask_for_none() {
        // Review L3: Opens and Antigravity generations never carry facts, so they publish no `facts-needed`.
        let feeds = Feeds::new();
        let mut events = feeds.events.subscribe();
        let with = |g: &str, data: &str| {
            format!(r#"{{"generation":"{g}","data":{data},"facts":null}}"#).into_bytes()
        };
        let put = |g: &str| with(g, "[]");
        assert!(feeds.put(HOME_OPENS, &put("1")).is_ok());
        assert!(feeds.put(HOME_ANTIGRAVITY, &put("1")).is_ok());
        assert!(
            events.try_recv().is_err(),
            "no event for a source without facts"
        );
        // A source that takes facts still asks for them (L1).
        assert!(feeds.put("home", &put("g1")).is_ok());
        let line = events
            .try_recv()
            .map(|b| String::from_utf8_lossy(&b).into_owned());
        assert!(line.is_ok_and(|l| l.contains("facts-needed")));
        // Each generation carries the digest of its own bytes.
        let a = feeds.current(HOME_OPENS, "1").map(|g| g.digest);
        assert!(feeds.put(HOME_OPENS, &with("2", r#"[["p",{}]]"#)).is_ok());
        let b = feeds.current(HOME_OPENS, "2").map(|g| g.digest);
        assert!(matches!((a, b), (Ok(a), Ok(b)) if a != b));
    }

    #[test]
    fn a_generation_keeps_its_payload_and_only_a_newer_one_replaces_it() {
        let feeds = Feeds::new();
        let opens = |g: &str, data: &str| {
            format!(r#"{{"generation":"{g}","data":{data},"facts":null}}"#).into_bytes()
        };
        // R1 (review L2 of L1): the payload is held and prepared, not just its size.
        assert!(
            feeds
                .put(HOME_OPENS, &opens("2", r#"[["p",{"state":"copying"}]]"#))
                .is_ok()
        );
        let g = feeds.current(HOME_OPENS, "2").expect("held");
        assert_eq!(g.data.get(), r#"[["p",{"state":"copying"}]]"#);
        assert!(matches!(g.prepared, Prepared::Opens(Ok(_))));
        // Two pushes racing: generation 1 arrives after 2 and is not taken; the reply names 2.
        let late = feeds.put(HOME_OPENS, &opens("1", "[]")).expect("reply");
        assert_eq!(late["generation"], json!("2"));
        assert_eq!(held(&feeds, HOME_OPENS).as_deref(), Some("2"));
        assert_eq!(feeds.current(HOME_OPENS, "1").err(), Some(Decline::Stale));
        // The same generation again is idempotent; with other bytes it is refused (immutable).
        assert!(
            feeds
                .put(HOME_OPENS, &opens("2", r#"[["p",{"state":"copying"}]]"#))
                .is_ok()
        );
        assert_eq!(
            feeds.put(HOME_OPENS, &opens("2", "[]")).err(),
            Some(Decline::Shape)
        );
        assert!(feeds.put(HOME_OPENS, &opens("3", "[]")).is_ok());
        assert_eq!(held(&feeds, HOME_OPENS).as_deref(), Some("3"));
        assert_eq!(feeds.current(HOME_OPENS, "2").err(), Some(Decline::Stale));
        // Home sources need numeric generations and known names; a prepared decline is held and reported.
        assert_eq!(
            feeds.put(HOME_OPENS, &opens("g4", "[]")).err(),
            Some(Decline::Shape)
        );
        assert_eq!(
            feeds.put("home-dashboard-linux-0", &opens("1", "{}")).err(),
            Some(Decline::Shape)
        );
        assert!(feeds.put(HOME_ANTIGRAVITY, &opens("1", "null")).is_ok());
        assert_eq!(
            feeds
                .status(HOME_ANTIGRAVITY)
                .map(|v| v["declined"].clone()),
            Ok(json!("shape"))
        );
        assert_eq!(
            feeds.current("home-dashboard-mac-0", "1").err(),
            Some(Decline::Stale)
        );
    }
}
