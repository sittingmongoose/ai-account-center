//! R2: `GET /api/status` from fed generations (DESIGN-M3-COMPLETE 1.2). StatusService's sources are pushed by
//! generation, each a JSON object of some of the composer's top-level input members:
//!
//! | Source | Members |
//! |---|---|
//! | `status-reports` | `reports` |
//! | `status-quota` | `quota`, `spendByStream` |
//! | `status-pinned` | `pinned` |
//! | `status-collected` | `registry`, `machines`, `browsers`, `simlock`, `usage` |
//! | `status-answers` | `answers`, `sends`, `answerHistoryProblem` |
//! | `status-facts` | `timeZone`, `hiddenThreadNames`, `machineIds`, `machineLabels`, and the facts |
//!
//! `status-facts` carries the presentation facts the TS envelope gives (`dates`, `epochs`, `order`: the clock and
//! day words of every time value, the `Date.parse` epochs, the `localeCompare` ranks), computed once by TS over the
//! whole set its id belongs to; the other sources carry `{}` facts. Each generation is parsed once, when it
//! arrives, and its round trip through `JSON.stringify`'s rules is proved then (the composer copies parts of it
//! into the view). The request carries no body: `x-aac-feed` names the six ids, the clock (`now`) and now's clock
//! and day words. The view is the existing composer (`aac_status::read_status`) over the borrowed generations,
//! written by `aac_views::js_json` as Node writes it, with the revalidated-view ETag. Anything unproven declines
//! with a named reason, and TS answers.
use crate::feed::{Feeds, Generation, Prepared};
use crate::status_cache::CachedStatusBody;
use crate::view_reply::EncodedView;
use aac_status::read_context::{Date, Presentation};
use aac_status::read_status::{self, FedTrace, Unproven};
use aac_views::decline::Decline;
use aac_views::js_json::Writer;
use axum::body::Bytes;
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// The view's name and version (`x-aac-view: status@1`).
pub(crate) const VIEW: &str = "status";
pub(crate) const VERSION: u32 = 1;
const REPORTS: &str = "status-reports";
const QUOTA: &str = "status-quota";
const PINNED: &str = "status-pinned";
const COLLECTED: &str = "status-collected";
const ANSWERS: &str = "status-answers";
const FACTS: &str = "status-facts";
const SOURCES: [&str; 6] = [REPORTS, QUOTA, PINNED, COLLECTED, ANSWERS, FACTS];

/// Each source's members, in the order TS writes them. A generation with any other member set declines (`shape`).
fn members(source: &str) -> &'static [&'static str] {
    match source {
        REPORTS => &["reports"],
        QUOTA => &["quota", "spendByStream"],
        PINNED => &["pinned"],
        COLLECTED => &["registry", "machines", "browsers", "simlock", "usage"],
        ANSWERS => &["answers", "sends", "answerHistoryProblem"],
        FACTS => &[
            "timeZone",
            "hiddenThreadNames",
            "machineIds",
            "machineLabels",
        ],
        _ => &[],
    }
}

/// True for a Status source name.
pub(crate) fn source(name: &str) -> bool {
    SOURCES.contains(&name)
}

/// One prepared generation: its data object, and for `status-facts` the presentation facts.
pub(crate) type ScanClock = (Arc<AtomicU64>, aac_num::intl::Zone);

/// The `status-collected` members as a typed shared view (MEM5 Lane C): the same five
/// members the wire `data` object carries, each held once behind an `Arc` the
/// producer and the composer share, instead of a fresh `Value` tree parsed from the
/// wire bytes beside the producer's own snapshots. `None` is a member TS did not
/// write (`JSON.stringify` drops `undefined`); a present `null` stays `Some`.
#[derive(Debug)]
pub(crate) struct CollectedView {
    pub(crate) registry: Option<Arc<Value>>,
    pub(crate) machines: Option<Arc<Value>>,
    pub(crate) browsers: Option<Arc<Value>>,
    pub(crate) simlock: Option<Arc<Value>>,
    pub(crate) usage: Option<Arc<Value>>,
}

pub(crate) struct Part {
    pub(crate) scan: Option<ScanClock>,
    data: Value,
    reports: Option<Arc<read_status::ReportView>>,
    collected: Option<Arc<CollectedView>>,
    facts: Option<Arc<Presentation>>,
}

impl std::fmt::Debug for Part {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Part")
            .field("members", &self.data.as_object().map(serde_json::Map::len))
            .field("facts", &self.facts.is_some())
            .finish()
    }
}

/// Parse one generation when it arrives. The data is an object of exactly the source's members (absent members
/// are members TS did not write, as `JSON.stringify` drops `undefined`), passes the envelope's prototype-name gate,
/// and re-writes to the very text TS sent (so every part the composer copies is written as Node wrote it).
pub(crate) fn prepare(source: &str, data: &RawValue, facts: &RawValue) -> Result<Part, Decline> {
    let text = data.get();
    let value: Value = serde_json::from_str(text).map_err(|_| Decline::Shape)?;
    let object = value.as_object().ok_or(Decline::Shape)?;
    let allowed = members(source);
    let mut at = 0;
    for key in object.keys() {
        // In TS's order, each at most once (a JSON object may repeat a key; serde keeps the last).
        let found = allowed[at..]
            .iter()
            .position(|m| *m == key.as_str())
            .ok_or(Decline::Shape)?;
        at += found + 1;
    }
    fed_part(&value).ok_or(Decline::Shape)?;
    if aac_views::js_json::write(&value) != text.as_bytes() {
        return Err(Decline::Shape);
    }
    let facts = if source == FACTS {
        let presentation: Presentation =
            serde_json::from_str(facts.get()).map_err(|_| Decline::Shape)?;
        Some(Arc::new(presentation))
    } else {
        if facts.get().trim() != "{}" {
            return Err(Decline::Shape);
        }
        None
    };
    if source == COLLECTED {
        // Move the members out of the parsed tree into the typed share; the tree
        // itself is dropped, so the generation holds one copy, not two.
        let Value::Object(mut object) = value else {
            return Err(Decline::Shape);
        };
        let mut take = |key: &str| object.remove(key).map(Arc::new);
        let view = Arc::new(CollectedView {
            registry: take("registry"),
            machines: take("machines"),
            browsers: take("browsers"),
            simlock: take("simlock"),
            usage: take("usage"),
        });
        return Ok(Part {
            data: Value::Object(serde_json::Map::new()),
            reports: None,
            collected: Some(view),
            facts,
            scan: None,
        });
    }
    Ok(Part {
        data: value,
        reports: None,
        collected: None,
        facts,
        scan: None,
    })
}

pub(crate) fn prepared_reports(reports: Arc<read_status::ReportView>, scan: ScanClock) -> Part {
    Part {
        data: Value::Object(serde_json::Map::new()),
        reports: Some(reports),
        collected: None,
        scan: Some(scan),
        facts: None,
    }
}

pub(crate) fn prepared_collected(view: Arc<CollectedView>, scan: ScanClock) -> Part {
    Part {
        data: Value::Object(serde_json::Map::new()),
        reports: None,
        collected: Some(view),
        scan: Some(scan),
        facts: None,
    }
}

/// Whether an owned `status-collected` view proves: no key or string in any member is
/// exactly an `Object.prototype` member name (`fed_part`'s one exact-name rule). The
/// member names are fixed by the envelope, so checking the member values proves the
/// same thing as checking the wire object the wire path checks.
pub(crate) fn collected_proven(view: &CollectedView) -> bool {
    [
        &view.registry,
        &view.machines,
        &view.browsers,
        &view.simlock,
        &view.usage,
    ]
    .iter()
    .all(|member| {
        member
            .as_deref()
            .is_none_or(|value| !aac_status::read_context::proto_named(value))
    })
}

/// `read_status::fed_part`: the data is an object and no key or string in it is exactly an
/// `Object.prototype` member name (`read_context::proto_named`, the one exact-name rule).
/// Prose that merely holds such a name as a word ("constructor refusals" in a card)
/// answers; a plain object lookup `{...}[name]` reads an inherited member only for the
/// exact name, so an exact name still declines.
pub(crate) fn fed_part(data: &Value) -> Option<()> {
    (data.is_object() && !aac_status::read_context::proto_named(data)).then_some(())
}

/// The front door's per-request facts: the clock, now's two words (formatted by TS in the facts' time zone) and
/// the generation it read for each source.
#[cfg(unix)]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Feed {
    now: i64,
    clock: String,
    day: String,
    reports: String,
    quota: String,
    pinned: String,
    collected: String,
    answers: String,
    facts: String,
}

/// The last ETag, keyed by the generations and the second of `now`; verified by SHA-256 digest
/// instead of retaining a full copy of the 450 KB identity bytes.
type Tagged = (String, [u8; 32], String);
static ETAG: Mutex<Option<Tagged>> = Mutex::new(None);

fn etag(key: String, identity: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(identity).into();
    if let Ok(held) = ETAG.lock()
        && let Some((k, d, tag)) = held.as_ref()
        && *k == key
        && *d == digest
    {
        return tag.clone();
    }
    let tag = format!("W/\"{}\"", crate::view_reply::hex_digest(&digest));
    if let Ok(mut held) = ETAG.lock() {
        *held = Some((key, digest, tag.clone()));
    }
    tag
}

/// Answer one Status request from six already-resolved immutable generations. Used by
/// synchronous test reads directly from an atomic published snapshot.
#[cfg(test)]
pub(crate) fn answer_generations(
    held: &[Arc<Generation>; 6],
    now_ms: i64,
    clock: String,
    day: String,
) -> Result<(Vec<u8>, String), Decline> {
    let composed = compose_generations(held, scan_stamps(held), now_ms, clock, day)?;
    Ok((composed.raw.to_vec(), composed.etag))
}

#[cfg(unix)]
fn resolve_fed(feeds: &Feeds, feed: &str) -> Result<(Feed, [Arc<Generation>; 6]), Decline> {
    let feed: Feed = crate::json_object::from_slice(
        feed.as_bytes(),
        &[
            "now",
            "clock",
            "day",
            "reports",
            "quota",
            "pinned",
            "collected",
            "answers",
            "facts",
        ],
    )
    .map_err(|_| Decline::Shape)?;
    let held = [
        feeds.current(REPORTS, &feed.reports)?,
        feeds.current(QUOTA, &feed.quota)?,
        feeds.current(PINNED, &feed.pinned)?,
        feeds.current(COLLECTED, &feed.collected)?,
        feeds.current(ANSWERS, &feed.answers)?,
        feeds.current(FACTS, &feed.facts)?,
    ];
    Ok((feed, held))
}

#[derive(Clone)]
struct Composed {
    body: Option<Arc<CachedStatusBody>>,
    raw: Bytes,
    composed_iso: String,
    etag: String,
    at: Option<usize>,
    trace: Option<FedTrace>,
    composed_now: i64,
}

impl Composed {
    fn encode_for(
        &self,
        iso: &str,
        exact_or_shared: bool,
        accept_gzip: bool,
        if_none_match: Option<&str>,
    ) -> Option<EncodedView> {
        if crate::compress::not_modified(if_none_match, &self.etag) {
            return Some(EncodedView::NotModified {
                etag: self.etag.clone(),
            });
        }
        if exact_or_shared {
            if accept_gzip {
                if let Some(body) = &self.body
                    && let Some(view) =
                        body.render_encoded(&self.composed_iso, true, self.etag.clone())
                {
                    return Some(view);
                }
                if let Some(gz) = crate::compress::compress_for_tag(Some(&self.etag), &self.raw) {
                    return Some(EncodedView::Payload {
                        bytes: gz,
                        raw_len: self.raw.len(),
                        gzip: true,
                        etag: self.etag.clone(),
                    });
                }
            }
            return Some(EncodedView::Payload {
                bytes: self.raw.clone(),
                raw_len: self.raw.len(),
                gzip: false,
                etag: self.etag.clone(),
            });
        }
        if let Some(body) = &self.body {
            return body.render_encoded(iso, accept_gzip, self.etag.clone());
        }
        let at = self.at?;
        let filled = crate::status_cache::fill(&self.raw, at, iso)?;
        let raw_len = filled.len();
        if accept_gzip
            && let Some(gz) = crate::compress::compress_for_tag(Some(&self.etag), &filled)
        {
            return Some(EncodedView::Payload {
                bytes: gz,
                raw_len,
                gzip: true,
                etag: self.etag.clone(),
            });
        }
        Some(EncodedView::Payload {
            bytes: Bytes::from(filled),
            raw_len,
            gzip: false,
            etag: self.etag.clone(),
        })
    }
}

fn validate_prepared(held: &[Arc<Generation>; 6]) -> Result<(), Decline> {
    let mut has_facts = false;
    for generation in held {
        let Prepared::Status(part) = &generation.prepared else {
            return Err(Decline::Internal);
        };
        let part = part.as_ref().map_err(|d| *d)?;
        if part.facts.is_some() {
            has_facts = true;
        }
        if !part.data.is_object() {
            return Err(Decline::Internal);
        }
    }
    if !has_facts {
        return Err(Decline::Internal);
    }
    Ok(())
}

/// Best-effort cache pre-warm for a newly prepared generation set on a background producer pass.
/// Populates `status_cache` on success; errors are left for the request path to report by name.
pub(crate) fn prewarm_generations(
    held: &[Arc<Generation>; 6],
    now_ms: i64,
    clock: String,
    day: String,
) {
    let _ = compose_generations(held, scan_stamps(held), now_ms, clock, day);
}

fn scan_clock(generation: &Generation) -> Option<&ScanClock> {
    match &generation.prepared {
        Prepared::Status(Ok(part)) => part.scan.as_ref(),
        _ => None,
    }
}

const CLOCK_PARTS: [usize; 2] = [0, 3];

fn scan_stamps(held: &[Arc<Generation>; 6]) -> [u64; 2] {
    CLOCK_PARTS
        .map(|index| scan_clock(&held[index]).map_or(0, |(clock, _)| clock.load(Ordering::Acquire)))
}

fn cache_ids(held: &[Arc<Generation>; 6], stamps: [u64; 2]) -> [String; 6] {
    let mut ids = std::array::from_fn(|i| held[i].id.clone());
    for (index, stamp) in CLOCK_PARTS.into_iter().zip(stamps) {
        if stamp != 0 {
            ids[index].push('/');
            ids[index].push_str(&stamp.to_string());
        }
    }
    ids
}

fn compose_generations(
    held: &[Arc<Generation>; 6],
    stamps: [u64; 2],
    now_ms: i64,
    clock: String,
    day: String,
) -> Result<Composed, Decline> {
    let key_ids = cache_ids(held, stamps);
    // Named generations only: `current` already refused an id the service does not hold. The date
    // is checked before a hit so a clock `toISOString` cannot write still declines.
    let date = Date::of_now(now_ms, clock.clone(), day.clone()).ok_or(Decline::Date)?;
    if let Some(hit) = crate::status_cache::hit(&key_ids, &clock, &day, now_ms)
        && let Some(bytes) = hit.body.render_raw(date.iso_text())
    {
        return Ok(Composed {
            body: Some(hit.body),
            raw: Bytes::from(bytes),
            composed_iso: date.iso_text().to_owned(),
            etag: hit.etag,
            at: None,
            trace: None,
            composed_now: now_ms,
        });
    }
    let mut parts: Vec<(&str, &Value)> = Vec::with_capacity(20);
    let now = Value::from(now_ms);
    parts.push(("now", &now));
    let mut presentation = None;
    let mut reports = None;
    for generation in held {
        let Prepared::Status(part) = &generation.prepared else {
            return Err(Decline::Internal);
        };
        let part = part.as_ref().map_err(|d| *d)?;
        if let Some(facts) = &part.facts {
            presentation = Some(facts.clone());
        }
        if let Some(native) = &part.reports {
            reports = Some(native.as_ref());
        }
        if let Some(view) = &part.collected {
            // The typed share, in the envelope's member order (`members(COLLECTED)`).
            if let Some(value) = &view.registry {
                parts.push(("registry", value.as_ref()));
            }
            if let Some(value) = &view.machines {
                parts.push(("machines", value.as_ref()));
            }
            if let Some(value) = &view.browsers {
                parts.push(("browsers", value.as_ref()));
            }
            if let Some(value) = &view.simlock {
                parts.push(("simlock", value.as_ref()));
            }
            if let Some(value) = &view.usage {
                parts.push(("usage", value.as_ref()));
            }
            continue;
        }
        let Some(object) = part.data.as_object() else {
            return Err(Decline::Internal);
        };
        for (key, value) in object {
            parts.push((key.as_str(), value));
        }
    }
    let presentation = presentation.ok_or(Decline::Internal)?;
    let iso = date.iso_text().to_owned();
    let scan_dates = std::array::from_fn(|slot| {
        let index = CLOCK_PARTS[slot];
        let stamp = stamps[slot];
        if stamp == 0 {
            return None;
        }
        let stamp = i64::try_from(stamp).ok()?;
        let (_, zone) = scan_clock(&held[index])?;
        let (clock, day) = zone.words(stamp)?;
        Some((stamp, Date::of_now(stamp, clock, day)?))
    });
    let (view, traced) =
        read_status::compose_with_reports(parts, reports, presentation, (now_ms, date), scan_dates)
            .map_err(|reason| match reason {
                Unproven::Facts => Decline::Date,
                Unproven::Input => Decline::Unsupported,
            })?;
    let mut writer = Writer::with_capacity(256 * 1024);
    writer.value(&view);
    let (bytes, identity) = writer.finish();
    let key = format!(
        "{}|{}|{}|{}|{}|{}|{}",
        key_ids[0],
        key_ids[1],
        key_ids[2],
        key_ids[3],
        key_ids[4],
        key_ids[5],
        now_ms.div_euclid(1000)
    );
    let tag = etag(key, &identity);
    let at = crate::status_cache::generated_at(&bytes, &iso);
    let (body, raw) = if let Some(at) = at {
        let (cached_body, raw) = CachedStatusBody::from_raw_and_keep(bytes, at);
        let body = Arc::new(cached_body);
        crate::status_cache::remember_body(
            key_ids,
            clock,
            day,
            traced.clone(),
            Arc::clone(&body),
            tag.clone(),
        );
        (Some(body), raw)
    } else {
        (None, Bytes::from(bytes))
    };
    Ok(Composed {
        body,
        raw,
        composed_iso: iso,
        etag: tag,
        at,
        trace: Some(traced),
        composed_now: now_ms,
    })
}

#[derive(Clone, Hash, PartialEq, Eq)]
struct ComposeKey {
    ids: [String; 6],
    clock: String,
    day: String,
}

struct Flight {
    id: u64,
    rx: tokio::sync::watch::Receiver<Option<Result<Composed, Decline>>>,
}

struct FlightGuard {
    inflight: Arc<Mutex<HashMap<ComposeKey, Flight>>>,
    key: ComposeKey,
    id: u64,
}

impl Drop for FlightGuard {
    fn drop(&mut self) {
        let mut map = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
        if map
            .get(&self.key)
            .is_some_and(|flight| flight.id == self.id)
        {
            map.remove(&self.key);
        }
    }
}

/// Async singleflight for Status view composition. Concurrent reads for the same six generations
/// and `(clock, day)` share one compose (one `Pool::Status` permit) instead of filling the pool;
/// the compose runs in its own task so a caller timeout still completes and fills `status_cache`
/// for the immediate retry.
#[derive(Clone, Default)]
pub(crate) struct StatusCompose {
    inflight: Arc<Mutex<HashMap<ComposeKey, Flight>>>,
    next_id: Arc<AtomicU64>,
    #[cfg(test)]
    compose_count: Arc<std::sync::atomic::AtomicUsize>,
    #[cfg(test)]
    compose_delay_ms: Arc<AtomicU64>,
}

impl StatusCompose {
    #[cfg(test)]
    pub(crate) fn compose_count(&self) -> usize {
        self.compose_count.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(crate) fn set_compose_delay_ms(&self, ms: u64) {
        self.compose_delay_ms.store(ms, Ordering::SeqCst);
    }

    /// Singleflight `x-aac-feed` Status read returning pre-compressed gzip or `304 Not Modified`.
    #[cfg(unix)]
    pub(crate) async fn answer_fed_encoded(
        &self,
        feeds: &Feeds,
        limits: &crate::limits::Limits,
        feed: &str,
        accept_gzip: bool,
        if_none_match: Option<&str>,
    ) -> Result<EncodedView, Decline> {
        let (feed, held) = resolve_fed(feeds, feed)?;
        self.answer_generations_encoded(
            Some(limits),
            held,
            feed.now,
            feed.clock,
            feed.day,
            false,
            accept_gzip,
            if_none_match,
        )
        .await
    }

    /// Singleflight Status compose over six resolved generations (uncompressed compatibility wrapper).
    #[cfg(test)]
    pub(crate) async fn answer_generations_async(
        &self,
        limits: Option<&crate::limits::Limits>,
        held: [Arc<Generation>; 6],
        now_ms: i64,
        clock: String,
        day: String,
        share_concurrent_window: bool,
    ) -> Result<(Vec<u8>, String), Decline> {
        match self
            .answer_generations_encoded(
                limits,
                held,
                now_ms,
                clock,
                day,
                share_concurrent_window,
                false,
                None,
            )
            .await?
        {
            EncodedView::Payload { bytes, etag, .. } => Ok((bytes.to_vec(), etag)),
            EncodedView::NotModified { .. } => Err(Decline::Internal),
        }
    }

    /// Singleflight Status compose over six resolved generations. Short-circuits to `304` on
    /// matching `If-None-Match` before splicing or rendering, and serves pre-compressed gzip
    /// `Bytes` when `accept_gzip` is true. Waiters hold zero pool permits.
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn answer_generations_encoded(
        &self,
        limits: Option<&crate::limits::Limits>,
        held: [Arc<Generation>; 6],
        now_ms: i64,
        clock: String,
        day: String,
        share_concurrent_window: bool,
        accept_gzip: bool,
        if_none_match: Option<&str>,
    ) -> Result<EncodedView, Decline> {
        let date = Date::of_now(now_ms, clock.clone(), day.clone()).ok_or(Decline::Date)?;
        let stamps = scan_stamps(&held);
        let ids = cache_ids(&held, stamps);
        if let Some(hit) = crate::status_cache::hit(&ids, &clock, &day, now_ms) {
            if crate::compress::not_modified(if_none_match, &hit.etag) {
                return Ok(EncodedView::NotModified { etag: hit.etag });
            }
            if let Some(view) = hit
                .body
                .render_encoded(date.iso_text(), accept_gzip, hit.etag)
            {
                return Ok(view);
            }
        }
        validate_prepared(&held)?;
        let key = ComposeKey {
            ids: ids.clone(),
            clock: clock.clone(),
            day: day.clone(),
        };
        for _ in 0..2 {
            enum Role {
                Follow(tokio::sync::watch::Receiver<Option<Result<Composed, Decline>>>),
                Lead {
                    id: u64,
                    tx: tokio::sync::watch::Sender<Option<Result<Composed, Decline>>>,
                    rx: tokio::sync::watch::Receiver<Option<Result<Composed, Decline>>>,
                },
            }
            let role = {
                let mut map = self.inflight.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(existing) = map.get(&key) {
                    Role::Follow(existing.rx.clone())
                } else {
                    let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
                    let (tx, rx) = tokio::sync::watch::channel(None);
                    map.insert(key.clone(), Flight { id, rx: rx.clone() });
                    Role::Lead { id, tx, rx }
                }
            };
            let mut rx = match role {
                Role::Follow(rx) => rx,
                Role::Lead { id, tx, rx } => {
                    let inflight = Arc::clone(&self.inflight);
                    let flight_key = key.clone();
                    let limits = limits.cloned();
                    let held_clone = held.clone();
                    let clock_clone = clock.clone();
                    let day_clone = day.clone();
                    #[cfg(test)]
                    let compose_count = Arc::clone(&self.compose_count);
                    #[cfg(test)]
                    let compose_delay_ms = Arc::clone(&self.compose_delay_ms);
                    tokio::spawn(async move {
                        let guard = FlightGuard {
                            inflight,
                            key: flight_key,
                            id,
                        };
                        let work = move || {
                            #[cfg(test)]
                            {
                                compose_count.fetch_add(1, Ordering::SeqCst);
                                let delay = compose_delay_ms.load(Ordering::SeqCst);
                                if delay > 0 {
                                    std::thread::sleep(std::time::Duration::from_millis(delay));
                                }
                            }
                            compose_generations(&held_clone, stamps, now_ms, clock_clone, day_clone)
                        };
                        let outcome = match limits {
                            Some(limits) => limits.blocking_status(work).await,
                            None => tokio::task::spawn_blocking(work)
                                .await
                                .unwrap_or(Err(Decline::Internal)),
                        };
                        // Remove before waking waiters so a waiter that needs a fresh compose
                        // for a different millisecond cannot re-join this completed flight.
                        drop(guard);
                        let _ = tx.send(Some(outcome));
                    });
                    rx
                }
            };
            let outcome = loop {
                if let Some(result) = rx.borrow_and_update().clone() {
                    break result;
                }
                if rx.changed().await.is_err() {
                    break Err(Decline::Internal);
                }
            };
            let composed = outcome?;
            if composed.composed_now == now_ms
                && let Some(view) =
                    composed.encode_for(date.iso_text(), true, accept_gzip, if_none_match)
            {
                return Ok(view);
            }
            if composed
                .trace
                .as_ref()
                .is_some_and(|trace| trace.keep() && trace.matches(now_ms))
                && let Some(view) =
                    composed.encode_for(date.iso_text(), false, accept_gzip, if_none_match)
            {
                return Ok(view);
            }
            // One-sided: a follower may reuse the leader's bytes only when its own clock is at
            // or after the leader's instant, so `generatedAt` is never in the follower's future
            // (review L3).
            if share_concurrent_window
                && now_ms >= composed.composed_now
                && now_ms - composed.composed_now <= 1_000
                && let Some(view) =
                    composed.encode_for(date.iso_text(), true, accept_gzip, if_none_match)
            {
                return Ok(view);
            }
            if let Some(hit) = crate::status_cache::hit(&ids, &clock, &day, now_ms) {
                if crate::compress::not_modified(if_none_match, &hit.etag) {
                    return Ok(EncodedView::NotModified { etag: hit.etag });
                }
                if let Some(view) = hit
                    .body
                    .render_encoded(date.iso_text(), accept_gzip, hit.etag)
                {
                    return Ok(view);
                }
            }
        }
        // Final fallback if a second-boundary race required a fresh compose directly.
        let iso = date.iso_text().to_owned();
        let work = move || compose_generations(&held, stamps, now_ms, clock, day);
        let composed = match limits {
            Some(limits) => limits.blocking_status(work).await?,
            None => tokio::task::spawn_blocking(work)
                .await
                .unwrap_or(Err(Decline::Internal))?,
        };
        composed
            .encode_for(&iso, true, accept_gzip, if_none_match)
            .ok_or(Decline::Internal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(text: &str) -> Box<RawValue> {
        RawValue::from_string(text.to_owned()).expect("raw")
    }

    #[test]
    fn a_generation_is_exactly_its_members_written_as_node_writes_them() {
        assert!(prepare(REPORTS, &raw(r#"{"reports":{"a":[1,2.5]}}"#), &raw("{}")).is_ok());
        assert!(
            prepare(
                COLLECTED,
                &raw(r#"{"registry":null,"usage":null}"#),
                &raw("{}")
            )
            .is_ok()
        );
        for (source, data, facts, reason) in [
            // Another member, a member out of order, a repeated member, not an object.
            (REPORTS, r#"{"reports":{},"x":1}"#, "{}", Decline::Shape),
            (
                QUOTA,
                r#"{"spendByStream":{},"quota":{}}"#,
                "{}",
                Decline::Shape,
            ),
            (
                REPORTS,
                r#"{"reports":{},"reports":{}}"#,
                "{}",
                Decline::Shape,
            ),
            (REPORTS, "[1]", "{}", Decline::Shape),
            // An exact Object.prototype name as a key or a string anywhere declines.
            (
                REPORTS,
                r#"{"reports":{"constructor":1}}"#,
                "{}",
                Decline::Shape,
            ),
            (
                REPORTS,
                r#"{"reports":{"cards":[{"label":"toString"}]}}"#,
                "{}",
                Decline::Shape,
            ),
            (
                REPORTS,
                r#"{"reports":{"cards":[{"__proto__":{}}]}}"#,
                "{}",
                Decline::Shape,
            ),
            // Text Node would not write (an escape it does not use, a number it spells otherwise).
            (REPORTS, r#"{"reports":"a\/b"}"#, "{}", Decline::Shape),
            (REPORTS, r#"{"reports":1.0}"#, "{}", Decline::Shape),
            (REPORTS, r#"{"reports":1E21}"#, "{}", Decline::Shape),
            (REPORTS, r#"{ "reports":1}"#, "{}", Decline::Shape),
            // Data sources carry no facts; the facts source carries exactly the envelope's three maps.
            (REPORTS, r#"{"reports":{}}"#, "null", Decline::Shape),
            (FACTS, r#"{"timeZone":"UTC"}"#, "{}", Decline::Shape),
            (
                FACTS,
                r#"{"timeZone":"UTC"}"#,
                r#"{"dates":{},"epochs":{},"order":{},"x":1}"#,
                Decline::Shape,
            ),
        ] {
            assert_eq!(
                prepare(source, &raw(data), &raw(facts)).err(),
                Some(reason),
                "{source} {data} {facts}"
            );
        }
        let facts = prepare(
            FACTS,
            &raw(r#"{"timeZone":"UTC","machineIds":["vm"]}"#),
            &raw(
                r#"{"dates":{"1":{"iso":"x","clock":"c","day":"d"}},"epochs":{},"order":{"a":0}}"#,
            ),
        )
        .expect("facts");
        assert!(facts.facts.is_some());
    }

    /// Live Nov 23: a withdrawn card's prose ("original_scope constructor refusals") declined
    /// every Status read (`shape`). Prose that holds a prototype name as a word answers; the
    /// other checks still hold for it.
    #[test]
    fn prose_holding_a_prototype_name_as_a_word_is_prepared() {
        let card = r#"{"reports":{"threads":[],"tasks":[],"cards":[{"id":"vm.lead--E5-1","state":"withdrawn","title":"Entailment proof","blocks":"recipe_tuple / original_scope constructor refusals reach it first","notes":["C8-B1 constructor, cuts","a toString() call","valueOf.x and hasOwnProperty-check"]}],"problems":[]}}"#;
        assert!(prepare(REPORTS, &raw(card), &raw("{}")).is_ok());
        assert!(fed_part(&serde_json::from_str(card).expect("json")).is_some());
        // The one exact-name rule the composer keeps everywhere agrees: prose is not a name.
        assert!(!aac_status::read_context::proto_named(
            &serde_json::from_str(card).expect("json")
        ));
        // Round trip and member checks still apply to such prose.
        let spaced = card.replacen("{\"reports\"", "{ \"reports\"", 1);
        assert_eq!(
            prepare(REPORTS, &raw(&spaced), &raw("{}")).err(),
            Some(Decline::Shape)
        );
        let extra = card.replacen("]}}", "]},\"x\":1}", 1);
        assert_eq!(
            prepare(REPORTS, &raw(&extra), &raw("{}")).err(),
            Some(Decline::Shape)
        );
        // Every exact name still declines, as a key or as a string.
        for name in aac_status::read_context::PROTO_NAMES {
            let as_key = format!(r#"{{"reports":{{"{name}":1}}}}"#);
            let as_text = format!(r#"{{"reports":["{name}"]}}"#);
            assert_eq!(
                prepare(REPORTS, &raw(&as_key), &raw("{}")).err(),
                Some(Decline::Shape),
                "{as_key}"
            );
            assert_eq!(
                prepare(REPORTS, &raw(&as_text), &raw("{}")).err(),
                Some(Decline::Shape),
                "{as_text}"
            );
        }
    }

    #[test]
    fn the_etag_is_reused_only_for_the_same_identity() {
        let a = etag("k".into(), b"{\"a\":1}");
        // Node: createHash('sha256').update('{"a":1}').digest('hex')
        assert_eq!(
            a,
            "W/\"015abd7f5cc57a2dd94b7590f04ad8084273905ee33ec5cebeae62276a97f862\""
        );
        assert_eq!(etag("k".into(), b"{\"a\":1}"), a);
        assert_ne!(etag("k".into(), b"{\"a\":2}"), a);
        assert_eq!(etag("other".into(), b"{\"a\":1}"), a);
        // One changed identity byte is a different tag. The cache stores that tag with the bytes
        // it was hashed from and will not serve it for another generation.
        let mut flipped = b"{\"a\":1}".to_vec();
        flipped[5] ^= 1;
        assert_ne!(etag("k".into(), &flipped), a);
    }
}
