//! G16c: the Status feed's producers in the service, under the owned Status group.
//!
//! The TypeScript `StatusService` used to build every fed generation itself: its collectors
//! (machines, browser-check, Simlock, pinned threads, the usage sample), its answer history and
//! its `Intl` facts. This module runs them here, at the same cadence and under the same rules:
//!
//! - the 10 s report scan is [`crate::status_owned`]'s; after each pass the six sources are
//!   rebuilt from its snapshots (the answers' shared copies fed into the answer store first);
//! - the 60 s collect ([`Produce::maybe_collect`], and at once after a quiet spell) reads this
//!   computer and the other machines over ssh, `browser-check slots --json` and
//!   `simlock status --json`; a read that fails keeps the last good reading and names the part;
//! - the pinned threads are re-read with every collect and composed over the reports;
//! - the answer history is read from the store (G16b's engine) and merged with verified share
//!   copies, as `readAnswerHistory` did;
//! - the facts are built here ([`aac_status::produce::facts`]) from the display zone's offsets,
//!   which the front door supplies with each request (its `Intl` is the one engine fact a
//!   server cannot reproduce); every word is written from them.
//!
//! A source whose data text did not change keeps its generation, so a request is not disturbed
//! by a producer pass that found the same readings. Any change publishes `status-changed` (at
//! most one every 5 s), which the front door rebroadcasts to `/ws`.
//!
//! Anything this side cannot prove declines: a request with no zone facts, an answer store it
//! cannot read, a pinned composition whose documents fail their projection. The front door then
//! answers with its own producers, which it restarts on the first decline.

use crate::answers_owned::AnswersOwned;
use crate::feed::{Feeds, Generation};
use crate::limits::Limits;
use crate::status_fed::StatusCompose;
#[cfg(test)]
use crate::status_owned::Scan;
use crate::status_run::{find_executable, run, ssh_argv};
use aac_num::intl::Zone;
use aac_status::answers_engine::closing_card_versions;
use aac_status::produce::answered::{ResolvedWaits, resolved_card_waits};
use aac_status::produce::answers_feed::AnswersFeed;
use aac_status::produce::browsers::{self, BrowserMachine};
use aac_status::produce::facts::ZoneFacts;
use aac_status::produce::machines::{self as machines_produce, HostOs, Platform, Sample};
use aac_status::produce::pinned_input;
use aac_status::produce::simlock::{self, Status as SimlockStatus};
pub(crate) use aac_status::produce::usage::UsageInput;
use sha2::Digest;

/// The Status usage input uses the same snapshot projection as the share publisher.
#[derive(Default)]
pub(crate) struct UsageSamples(aac_snapshot::history::RecentReadings);

impl UsageSamples {
    pub(crate) fn project(
        &mut self,
        root: &Path,
        cache: &crate::account_usage::AccountUsageCache,
    ) -> Option<UsageInput> {
        let now = i64::try_from(now_ms()).ok()?;
        let timestamp = chrono::DateTime::from_timestamp_millis(now)?
            .to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
        let accounts = cache.merged_accounts(&timestamp, now).ok()?;
        self.0.add(&accounts, now);
        let event = aac_snapshot::SampleEvent {
            accounts,
            sampled_at: now,
            history: None,
            long_burn: None,
        };
        let refresh = crate::account_usage::coordinator::read_refresh_seconds(root);
        let (mut snapshot, _) =
            aac_snapshot::build_snapshot_files(&event, refresh, Some(&self.0), &[], None);
        let accounts = std::mem::take(snapshot["accounts"].as_array_mut()?);
        (!accounts.is_empty()).then_some(UsageInput {
            accounts,
            sampled_at: now,
            stale_after_seconds: aac_snapshot::snapshot::stale_after_seconds(refresh) as i64,
        })
    }
}
use aac_status::produce::{Collected, ReadProblem, next_problem};
use aac_status::share::reports::AgentReportsState;
use aac_status::share::status_docs::StatusDocsState;
use aac_views::decline::Decline;
use serde_json::{Map, Value as Json, json};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// The collect cadence (`COLLECT_EVERY_MS`).
const COLLECT_EVERY_MS: u64 = 60_000;
/// A page read within this window keeps the process-starting collectors running.
const DEMAND_WINDOW_MS: u64 = 15 * 60_000;
/// The hint throttle (`HINT_GAP_MS`): one `status-changed` per window.
const HINT_GAP_MS: u64 = 5_000;

const REPORTS: &str = "status-reports";
const QUOTA: &str = "status-quota";
const PINNED: &str = "status-pinned";
const COLLECTED: &str = "status-collected";
const ANSWERS: &str = "status-answers";
const FACTS: &str = "status-facts";
const OWNED_PINS: &str = "owned-pins";

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn clamp_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// One machine's pin state across collects (`PinnedThreadsCollector`'s `MachineState`).
#[derive(Clone, Default)]
struct PinMachine {
    last_good: Option<aac_status::pins::PinsRead>,
    not_shown: usize,
    failures: u32,
    problem: Option<(String, u64)>,
}

/// Every machine's pins, the merged list and the panel's notes.
#[derive(Default)]
struct Pins {
    machines: BTreeMap<String, PinMachine>,
    merged: Vec<aac_status::pins::MergedPin>,
    problems: Vec<aac_status::pinned::MachineProblemIn>,
    collected: bool,
}

/// The display zone a request (or boot preference) named, kept between requests.
#[derive(Clone)]
struct RequestZone {
    name: String,
    zone: Zone,
    /// Fallback instant inside `zone`'s proved window when a caller/test sends a synthetic
    /// historical clock whose window does not contain wall-clock `now_ms()`.
    sample_now: u64,
}

/// One immutable, atomically swapped generation set of all six fed Status sources.
/// Readers clone this `Arc` in nanoseconds and never wait on a producer pass.
#[derive(Clone)]
struct Published {
    zone_name: String,
    zone: Zone,
    generations: [Arc<Generation>; 6],
}

#[derive(Clone)]
enum PublishedState {
    Ready(Arc<Published>),
    /// A finished producer pass whose inputs could not be proved: decline honestly by name.
    Declined {
        zone_name: String,
        zone: Zone,
        reason: Decline,
    },
}

impl PublishedState {
    fn matches_zone(&self, time_zone: &str, zone: &Zone) -> bool {
        match self {
            PublishedState::Ready(published) => {
                published.zone_name == time_zone && published.zone == *zone
            }
            PublishedState::Declined {
                zone_name,
                zone: held,
                ..
            } => zone_name == time_zone && held == zone,
        }
    }
}

/// Mutable producer inputs, held under `Produce::state` ONLY for quick Arc clones/swaps.
/// Never held across I/O, answers refresh, pinned composition, source building, or feed puts.
struct State {
    quota: Arc<Json>,
    registry: Arc<Json>,
    reports_rows: Arc<AgentReportsState>,
    docs_rows: Arc<StatusDocsState>,
    machines: BTreeMap<String, Collected<Sample>>,
    browsers: Collected<Vec<BrowserMachine>>,
    simlock: Collected<SimlockStatus>,
    simlock_workers: Option<Vec<&'static str>>,
    usage: Option<Arc<UsageInput>>,
    answers: Arc<AnswersFeed>,
    pins: Arc<Pins>,
    pinned: Arc<Json>,
    pinned_at: u64,
    pinned_offset: Option<i64>,
    pinned_dirty: bool,
    answers_dirty: bool,
    sources_dirty: bool,
    answers_gen: u64,
    pinned_gen: u64,
    sources_gen: u64,
    zone: Option<RequestZone>,
    hinted_at: u64,
}

/// Cheap snapshot taken from `State` under a microsecond lock so `publish_once` can compute
/// completely outside `Produce::state` and `Produce::published`.
struct PublishSnapshot {
    quota: Arc<Json>,
    registry: Arc<Json>,
    reports_rows: Arc<AgentReportsState>,
    docs_rows: Arc<StatusDocsState>,
    machines: BTreeMap<String, Collected<Sample>>,
    browsers: Collected<Vec<BrowserMachine>>,
    simlock: Collected<SimlockStatus>,
    usage: Option<Arc<UsageInput>>,
    answers: Arc<AnswersFeed>,
    pins: Arc<Pins>,
    pinned: Arc<Json>,
    pinned_offset: Option<i64>,
    pinned_dirty: bool,
    answers_dirty: bool,
    sources_dirty: bool,
    answers_gen: u64,
    pinned_gen: u64,
    sources_gen: u64,
    zone: Option<RequestZone>,
}

type HeldSource = ([u8; 32], [u8; 32], Arc<Generation>);
type ChangedSource = (usize, &'static str, [u8; 32], [u8; 32], Arc<Generation>);

/// Owned exclusively by the serialized publisher (`Produce::publisher`); readers never touch it.
/// Stores SHA-256 digests of the last published text & facts instead of raw multi-MB Strings.
#[derive(Default)]
struct PublisherCache {
    held: HashMap<&'static str, HeldSource>,
}

struct CollectingGuard<'a>(&'a AtomicBool);
impl Drop for CollectingGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Clears `publish_running` when the publish loop exits, including on panic/unwind, so a
/// panicked publish cannot wedge every later `trigger_publish` (review L1). Mirrors
/// `CollectingGuard`.
struct PublishRunningGuard<'a>(&'a AtomicBool);
impl Drop for PublishRunningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

impl State {
    fn new(default_zone: Option<RequestZone>) -> Self {
        let empty = empty_reports();
        Self {
            reports_rows: Arc::new(empty),
            quota: Arc::new(Json::Null),
            registry: Arc::new(json!({"data": null, "asOf": null, "error": null})),
            docs_rows: Arc::new(StatusDocsState {
                state: "missing",
                scanned_at: None,
                docs: Vec::new(),
                problems: Vec::new(),
            }),
            machines: aac_status::computers::machine_ids()
                .into_iter()
                .map(|machine| (machine.to_owned(), Collected::empty()))
                .collect(),
            browsers: Collected::empty(),
            simlock: Collected::empty(),
            simlock_workers: None,
            usage: None,
            answers: Arc::new(AnswersFeed {
                answers: Vec::new(),
                sends: Vec::new(),
                problem: None,
            }),
            pins: Arc::new(Pins::default()),
            pinned: Arc::new(json!({"unavailable": "the pinned threads have not been read yet"})),
            pinned_at: 0,
            pinned_offset: None,
            pinned_dirty: true,
            answers_dirty: true,
            sources_dirty: true,
            answers_gen: 0,
            pinned_gen: 0,
            sources_gen: 0,
            zone: default_zone,
            hinted_at: 0,
        }
    }

    fn snapshot(&self) -> PublishSnapshot {
        PublishSnapshot {
            quota: Arc::clone(&self.quota),
            registry: Arc::clone(&self.registry),
            reports_rows: Arc::clone(&self.reports_rows),
            docs_rows: Arc::clone(&self.docs_rows),
            machines: self.machines.clone(),
            browsers: self.browsers.clone(),
            simlock: self.simlock.clone(),
            usage: self.usage.clone(),
            answers: Arc::clone(&self.answers),
            pins: Arc::clone(&self.pins),
            pinned: Arc::clone(&self.pinned),
            pinned_offset: self.pinned_offset,
            pinned_dirty: self.pinned_dirty,
            answers_dirty: self.answers_dirty,
            sources_dirty: self.sources_dirty,
            answers_gen: self.answers_gen,
            pinned_gen: self.pinned_gen,
            sources_gen: self.sources_gen,
            zone: self.zone.clone(),
        }
    }
}

fn empty_reports() -> AgentReportsState {
    AgentReportsState {
        root: String::new(),
        state: "missing",
        scanned_at: None,
        complete: false,
        threads: Vec::new(),
        tasks: Vec::new(),
        cards: Vec::new(),
        problems: Vec::new(),
        boards: serde_json::Map::new(),
    }
}

fn default_request_zone(ccs_dir: &Path, now: u64) -> Option<RequestZone> {
    #[cfg(test)]
    return default_request_zone_inner(ccs_dir, now, None);
    #[cfg(not(test))]
    return default_request_zone_inner(ccs_dir, now);
}

#[cfg(test)]
fn default_request_zone_inner(
    ccs_dir: &Path,
    now: u64,
    record: Option<&Arc<Mutex<Vec<std::thread::ThreadId>>>>,
) -> Option<RequestZone> {
    if let Some(record) = record
        && let Ok(mut threads) = record.lock()
    {
        threads.push(std::thread::current().id());
    }
    zone_request(ccs_dir, now)
}

#[cfg(not(test))]
fn default_request_zone_inner(ccs_dir: &Path, now: u64) -> Option<RequestZone> {
    zone_request(ccs_dir, now)
}

fn zone_request(ccs_dir: &Path, now: u64) -> Option<RequestZone> {
    let now_i64 = clamp_i64(now);
    let (name, offsets) = crate::zone_facts::status_zone(ccs_dir, now_i64)?;
    Some(RequestZone {
        name,
        zone: offsets.zone,
        sample_now: now,
    })
}

pub(crate) struct Produce {
    feeds: Feeds,
    answers: Arc<AnswersOwned>,
    ccs_dir: PathBuf,
    state: Mutex<State>,
    publisher: Mutex<PublisherCache>,
    published: Mutex<Option<PublishedState>>,
    published_tx: tokio::sync::watch::Sender<u64>,
    published_rx: tokio::sync::watch::Receiver<u64>,
    compose: StatusCompose,
    clock: AtomicU64,
    last_demand: AtomicU64,
    last_collect: AtomicU64,
    collecting: AtomicBool,
    publish_requested: AtomicBool,
    publish_running: AtomicBool,
    zone_checked_day: AtomicU64,
    #[cfg(test)]
    publish_delay_ms: AtomicU64,
    #[cfg(test)]
    publish_panic_once: AtomicBool,
    #[cfg(test)]
    publish_entered: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    zone_checks: Arc<std::sync::atomic::AtomicU64>,
    #[cfg(test)]
    before_commit: Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>,
    #[cfg(test)]
    jobs_now_override: Arc<AtomicU64>,
    #[cfg(test)]
    zone_walk_threads: Arc<Mutex<Vec<std::thread::ThreadId>>>,
}

impl Produce {
    pub(crate) fn new(feeds: Feeds, answers: Arc<AnswersOwned>, ccs_dir: PathBuf) -> Arc<Self> {
        let default_zone = default_request_zone(&ccs_dir, now_ms());
        let (published_tx, published_rx) = tokio::sync::watch::channel(0u64);
        let produce = Arc::new(Self {
            feeds,
            answers,
            ccs_dir,
            state: Mutex::new(State::new(default_zone)),
            publisher: Mutex::new(PublisherCache::default()),
            published: Mutex::new(None),
            published_tx,
            published_rx,
            compose: StatusCompose::default(),
            clock: AtomicU64::new(0),
            last_demand: AtomicU64::new(0),
            last_collect: AtomicU64::new(0),
            collecting: AtomicBool::new(false),
            publish_requested: AtomicBool::new(false),
            publish_running: AtomicBool::new(false),
            #[cfg(test)]
            publish_delay_ms: AtomicU64::new(0),
            #[cfg(test)]
            publish_panic_once: AtomicBool::new(false),
            #[cfg(test)]
            publish_entered: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            zone_checked_day: AtomicU64::new(0),
            #[cfg(test)]
            zone_checks: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            #[cfg(test)]
            before_commit: Mutex::new(None),
            #[cfg(test)]
            jobs_now_override: Arc::new(AtomicU64::new(0)),
            #[cfg(test)]
            zone_walk_threads: Arc::new(Mutex::new(Vec::new())),
        });
        // Publish the honest empty/starting state now, without touching the answers store. A
        // producer is readable from construction; startup cannot lose this invariant by
        // forgetting a separate warm-up call before the first poll.
        produce.state().answers_dirty = false;
        produce.publish(None, true);
        produce
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn next_generation(&self, now: u64) -> u64 {
        let mut last = self.clock.load(Ordering::Relaxed);
        loop {
            let next = (last + 1).max(now.saturating_mul(1000));
            match self
                .clock
                .compare_exchange(last, next, Ordering::Relaxed, Ordering::Relaxed)
            {
                Ok(_) => return next,
                Err(held) => last = held,
            }
        }
    }

    /// A page read: demand for the process-starting collectors (lock-free).
    pub(crate) fn touch(&self) {
        self.last_demand.store(now_ms(), Ordering::Relaxed);
    }

    /// A new usage sample (`recordUsage`'s built input) or none. Stages the sample and schedules
    /// a background publish so the next Status read never pays `push_sources` inline.
    pub(crate) fn set_usage(self: &Arc<Self>, usage: Option<UsageInput>) {
        {
            let mut state = self.state();
            state.usage = usage.map(Arc::new);
            state.sources_dirty = true;
            state.sources_gen = state.sources_gen.wrapping_add(1);
        }
        self.trigger_publish();
    }

    /// The answer store recorded something (a forwarded send): stage dirty flags and schedule a
    /// background publish outside the request path.
    pub(crate) fn mark_answers(self: &Arc<Self>) {
        {
            let mut state = self.state();
            state.answers_dirty = true;
            state.pinned_dirty = true;
            state.sources_dirty = true;
            state.answers_gen = state.answers_gen.wrapping_add(1);
            state.pinned_gen = state.pinned_gen.wrapping_add(1);
            state.sources_gen = state.sources_gen.wrapping_add(1);
        }
        self.trigger_publish();
    }

    /// Schedule one coalesced background publish pass on the blocking pool.
    fn trigger_publish(self: &Arc<Self>) {
        self.publish_requested.store(true, Ordering::SeqCst);
        if self
            .publish_running
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let owner = Arc::clone(self);
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn_blocking(move || owner.run_publish_loop());
            } else {
                std::thread::spawn(move || owner.run_publish_loop());
            }
        }
    }

    fn run_publish_loop(self: &Arc<Self>) {
        let _guard = PublishRunningGuard(&self.publish_running);
        loop {
            self.publish_requested.store(false, Ordering::SeqCst);
            let prewarm = self.current_published().is_some();
            self.publish_once_and_hint(prewarm);
            if self.publish_requested.load(Ordering::SeqCst) {
                continue;
            }
            return;
        }
    }

    /// One producer pass every 5 s: collect when a page has been read recently and the minute
    /// is up (the TypeScript 60 s timer's rule), and keep the display zone window rolled across
    /// UTC midnight.
    pub(crate) async fn jobs(self: Arc<Self>, stop: Arc<std::sync::atomic::AtomicBool>) {
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let now = now_ms();
            #[cfg(test)]
            let now = {
                let override_now = self.jobs_now_override.load(Ordering::SeqCst);
                if override_now > 0 { override_now } else { now }
            };
            // The preferences read + 520-day zone walk is synchronous file I/O and ~2,080
            // chrono-tz offset evaluations, but `server_zone_facts` only rolls once per UTC
            // day. Re-walk only when the day advances, on the blocking pool, never on a Tokio
            // worker every 5 s tick (review L2).
            let day = now.div_euclid(86_400_000);
            if day != self.zone_checked_day.load(Ordering::Acquire) {
                self.zone_checked_day.store(day, Ordering::Release);
                let ccs_dir = self.ccs_dir.clone();
                #[cfg(test)]
                let record = Arc::clone(&self.zone_walk_threads);
                let zone_walk = tokio::task::spawn_blocking(move || {
                    #[cfg(test)]
                    {
                        default_request_zone_inner(&ccs_dir, now, Some(&record))
                    }
                    #[cfg(not(test))]
                    {
                        default_request_zone(&ccs_dir, now)
                    }
                })
                .await
                .unwrap_or(None);
                if let Some(next_zone) = zone_walk {
                    #[cfg(test)]
                    self.zone_checks.fetch_add(1, Ordering::SeqCst);
                    let rolled = {
                        let mut state = self.state();
                        if state.zone.as_ref().is_some_and(|held| {
                            held.name == next_zone.name && held.zone != next_zone.zone
                        }) {
                            state.zone = Some(next_zone);
                            state.pinned_dirty = true;
                            state.sources_dirty = true;
                            state.pinned_gen = state.pinned_gen.wrapping_add(1);
                            state.sources_gen = state.sources_gen.wrapping_add(1);
                            true
                        } else {
                            false
                        }
                    };
                    if rolled {
                        self.trigger_publish();
                    }
                }
            }
            let demand = self.last_demand.load(Ordering::Relaxed);
            let collected = self.last_collect.load(Ordering::Relaxed);
            let wanted = now.saturating_sub(demand) < DEMAND_WINDOW_MS
                && now.saturating_sub(collected) >= COLLECT_EVERY_MS
                && !self.collecting.load(Ordering::Acquire);
            if wanted {
                let owner = Arc::clone(&self);
                tokio::spawn(async move { owner.collect().await });
            }
            tokio::time::sleep(Duration::from_millis(5_000)).await;
        }
    }

    /// Start a collect now when the page has been read recently and nothing is running (lock-free).
    fn maybe_collect(self: &Arc<Self>) {
        let now = now_ms();
        let demand = self.last_demand.load(Ordering::Relaxed);
        let collected = self.last_collect.load(Ordering::Relaxed);
        if now.saturating_sub(demand) >= DEMAND_WINDOW_MS
            || now.saturating_sub(collected) < COLLECT_EVERY_MS
        {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            self.last_collect.store(now, Ordering::Relaxed);
            let owner = Arc::clone(self);
            handle.spawn(async move { owner.collect_running().await });
        }
    }

    /// One collect pass; overlapping calls share the running one.
    pub(crate) async fn collect(self: &Arc<Self>) {
        if self
            .collecting
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return;
        }
        self.last_collect.store(now_ms(), Ordering::Relaxed);
        self.collect_running().await;
    }

    async fn collect_running(self: &Arc<Self>) {
        let _guard = CollectingGuard(&self.collecting);
        let rows = computers_rows();
        let local = local_machine();
        let now = now_ms();
        // Read every machine at once; each failure is that machine's, never the pass's.
        let mut reads = Vec::new();
        for (machine, os, alias) in &rows {
            reads.push((
                machine.clone(),
                HostOs::of(os),
                alias.clone(),
                *machine == local,
            ));
        }
        // Every machine at once, as the collector's `Promise.all` did.
        let mut pending = Vec::with_capacity(reads.len());
        for (machine, host, alias, is_local) in reads {
            pending.push(async move {
                let sample = if is_local {
                    tokio::task::spawn_blocking(local_sample)
                        .await
                        .unwrap_or_else(|_| Err("The machine did not answer.".to_owned()))
                } else {
                    match (host, alias.clone()) {
                        (Some(host), Some(alias)) => read_remote(host, &alias).await,
                        _ => Err("No ssh host is configured for this machine.".to_owned()),
                    }
                };
                (machine, sample)
            });
        }
        let cached_workers = self.state().simlock_workers.clone();
        let previous_pins = self.state().pins.machines.clone();
        // Machines, simlock+browsers, and pins are independent I/O groups; run them together.
        let machines_fut = futures_util::future::join_all(pending);
        let simlock_and_browsers_fut = async move {
            let simlock = find_executable("simlock");
            let simlock_result = read_simlock(&simlock).await;
            let fresh_workers = simlock_result
                .as_ref()
                .ok()
                .and_then(|(_, _, workers)| workers.clone());
            let workers = fresh_workers.or(cached_workers);
            let browsers_result = read_browsers(workers.as_deref()).await;
            (simlock_result, browsers_result)
        };
        let pins_fut = read_pins(&rows, local, previous_pins);
        let (results, (simlock_result, browsers_result), pins) =
            tokio::join!(machines_fut, simlock_and_browsers_fut, pins_fut);
        let now_i64 = clamp_i64(now);
        {
            let mut state = self.state();
            for (machine, result) in results {
                let before = state
                    .machines
                    .get(&machine)
                    .cloned()
                    .unwrap_or_else(Collected::empty);
                state
                    .machines
                    .insert(machine, apply_machine(before, result, now_i64));
            }
            match browsers_result {
                Ok(rows) => {
                    let merged =
                        browsers::merge_rows(state.browsers.data.as_deref(), rows, now_i64);
                    let problems = browsers::problems(&merged);
                    state.browsers = Collected::succeeded(merged, now_i64).with_problems(problems);
                }
                Err((reason, missing)) => {
                    let before = state.browsers.clone();
                    state.browsers = before.failed(&reason, now_i64, missing);
                }
            }
            match simlock_result {
                Ok((status, worker_only, workers)) => {
                    if let Some(workers) = workers {
                        state.simlock_workers = Some(workers);
                    }
                    let problems = simlock_problems(
                        &status,
                        worker_only,
                        state.simlock.problems.as_deref(),
                        now_i64,
                    );
                    state.simlock = Collected::succeeded(status, now_i64).with_problems(problems);
                }
                Err((reason, missing)) => {
                    let before = state.simlock.clone();
                    state.simlock = before.failed(&reason, now_i64, missing);
                }
            }
            state.pins = Arc::new(pins);
            state.pinned_dirty = true;
            state.sources_dirty = true;
            state.pinned_gen = state.pinned_gen.wrapping_add(1);
            state.sources_gen = state.sources_gen.wrapping_add(1);
        }
        let owner = Arc::clone(self);
        let _ = tokio::task::spawn_blocking(move || owner.publish_once_and_hint(true)).await;
    }

    /// The 10 s report scan's snapshots: shared answers into the store outside the state lock,
    /// then an atomic swap and publish (without cloning report docs or keeping redundant JSON).
    #[cfg(test)]
    pub(crate) async fn after_scan(self: &Arc<Self>, scan: Scan) {
        self.after_scan_shared(
            Arc::new(scan.reports),
            Arc::new(scan.docs),
            Arc::new(scan.quota),
            Arc::new(scan.registry),
            true,
        )
        .await;
    }

    pub(crate) async fn after_scan_shared(
        self: &Arc<Self>,
        reports: Arc<AgentReportsState>,
        docs: Arc<StatusDocsState>,
        quota: Arc<Json>,
        registry: Arc<Json>,
        emit_hint: bool,
    ) {
        let answers = Arc::clone(&self.answers);
        let reports_for_feed = Arc::clone(&reports);
        let feed = tokio::task::spawn_blocking(move || {
            let cards: Vec<(Json, String)> = reports_for_feed
                .cards
                .iter()
                .map(|row| (row.doc.clone(), row.sha256.clone()))
                .collect();
            answers.feed(&cards)
        })
        .await
        .unwrap_or_else(|_| AnswersFeed::unavailable(Vec::new()));
        {
            let mut state = self.state();
            state.reports_rows = reports;
            state.docs_rows = docs;
            state.quota = quota;
            state.registry = registry;
            state.answers = Arc::new(feed);
            state.answers_dirty = false;
            state.pinned_dirty = true;
            state.sources_dirty = true;
            state.answers_gen = state.answers_gen.wrapping_add(1);
            state.pinned_gen = state.pinned_gen.wrapping_add(1);
            state.sources_gen = state.sources_gen.wrapping_add(1);
        }
        let owner = Arc::clone(self);
        let _ = tokio::task::spawn_blocking(move || {
            if owner.publish(None, true) && emit_hint {
                owner.hint();
            }
        })
        .await;
    }

    /// The pins the readers-check writer matches reports against (`owned-pins`).
    fn push_owned_pins(&self, pins: &Pins, now: u64) {
        let rows: Vec<Json> = pins
            .merged
            .iter()
            .map(|pin| json!({"id": pin.id, "title": pin.title}))
            .collect();
        let generation = self.next_generation(now);
        let data = aac_views::js_json::write(&Json::Array(rows));
        let body = format!(
            "{{\"generation\":\"{generation}\",\"data\":{},\"facts\":null}}",
            String::from_utf8_lossy(&data)
        );
        let _ = self.feeds.put(OWNED_PINS, body.as_bytes());
    }

    fn current_published(&self) -> Option<PublishedState> {
        self.published
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
    }

    fn set_published(&self, next: PublishedState) {
        *self.published.lock().unwrap_or_else(|p| p.into_inner()) = Some(next);
        self.published_tx.send_modify(|seq| {
            *seq = seq.wrapping_add(1);
        });
    }

    /// Rebuild every source from a snapshot of the state completely outside `self.state` and
    /// `self.published`, then commit to `Feeds` and swap `self.published` atomically.
    /// Readers keep answering from the last good generation throughout this pass.
    fn publish(&self, zone_override: Option<(&str, &Zone, u64)>, prewarm_compose: bool) -> bool {
        let mut cache = self.publisher.lock().unwrap_or_else(|p| p.into_inner());
        #[cfg(test)]
        self.publish_entered.fetch_add(1, Ordering::SeqCst);
        #[cfg(test)]
        if self.publish_panic_once.swap(false, Ordering::SeqCst) {
            panic!("flicker-lows: injected publish panic");
        }
        let snap = {
            let mut state = self.state();
            if let Some((name, zone, sample_now)) = zone_override
                && state
                    .zone
                    .as_ref()
                    .is_none_or(|held| held.name != name || held.zone != *zone)
            {
                state.zone = Some(RequestZone {
                    name: name.to_owned(),
                    zone: zone.clone(),
                    sample_now,
                });
                state.pinned_dirty = true;
                state.sources_dirty = true;
                state.pinned_gen = state.pinned_gen.wrapping_add(1);
                state.sources_gen = state.sources_gen.wrapping_add(1);
            }
            state.snapshot()
        };
        let mut answers = snap.answers;
        let mut need_sources = snap.sources_dirty;
        if snap.answers_dirty {
            let cards: Vec<(Json, String)> = snap
                .reports_rows
                .cards
                .iter()
                .map(|row| (row.doc.clone(), row.sha256.clone()))
                .collect();
            let fresh = Arc::new(self.answers.feed(&cards));
            {
                let mut state = self.state();
                if state.answers_gen == snap.answers_gen {
                    state.answers = Arc::clone(&fresh);
                    state.answers_dirty = false;
                }
            }
            answers = fresh;
            need_sources = true;
        }
        let Some(zone) = snap.zone else {
            return false;
        };
        let wall_now = now_ms();
        let now = if zone.zone.words(clamp_i64(wall_now)).is_some() {
            wall_now
        } else {
            zone.sample_now
        };
        let offset = zone.zone.offset_minutes(clamp_i64(now)).unwrap_or(0);
        let mut pinned = snap.pinned;
        if snap.pins.collected
            && (snap.pinned_dirty || snap.pinned_offset != Some(offset))
            && let Some(panel) = pinned_panel(
                &snap.reports_rows,
                &snap.docs_rows,
                &snap.pins,
                &answers,
                clamp_i64(now),
            )
        {
            let panel = Arc::new(panel);
            self.push_owned_pins(&snap.pins, now);
            {
                let mut state = self.state();
                if state.pinned_gen == snap.pinned_gen {
                    state.pinned = Arc::clone(&panel);
                    state.pinned_at = now;
                    state.pinned_offset = Some(offset);
                    state.pinned_dirty = false;
                }
            }
            pinned = panel;
            need_sources = true;
        }
        let matches_published = self
            .current_published()
            .as_ref()
            .is_some_and(|held| held.matches_zone(&zone.name, &zone.zone));
        if !need_sources && matches_published {
            return false;
        }
        let reports = resolved_reports(&snap.reports_rows, &answers);
        if reports.as_object().is_none() {
            return false;
        }
        #[cfg(test)]
        {
            let delay = self.publish_delay_ms.load(Ordering::SeqCst);
            if delay > 0 {
                std::thread::sleep(Duration::from_millis(delay));
            }
        }
        let machines = machines_json(&snap.machines);
        let built = aac_status::produce::sources::build(&aac_status::produce::sources::Readings {
            reports: &reports,
            quota: &snap.quota,
            pinned: &pinned,
            registry: &snap.registry,
            machines: &machines,
            browsers: &json!(snap.browsers),
            simlock: &json!(snap.simlock),
            usage: snap.usage.as_deref(),
            answers: &answers,
            time_zone: &zone.name,
            now: clamp_i64(now),
            offset_minutes: offset,
            zone: &zone.zone,
        });
        drop(reports);
        drop(machines);
        let mut sources = match built {
            Ok(sources) => sources,
            Err(why) => {
                let reason = match why {
                    "zone" => Decline::Locale,
                    "date" | "facts" => Decline::Date,
                    _ => Decline::Unsupported,
                };
                self.set_published(PublishedState::Declined {
                    zone_name: zone.name,
                    zone: zone.zone,
                    reason,
                });
                let mut state = self.state();
                if state.sources_gen == snap.sources_gen {
                    state.sources_dirty = false;
                }
                return false;
            }
        };
        // Every word ranked, not only printable ASCII (a middle-dot usage label is compared).
        if let Some(facts) = crate::status_collate::complete_order(
            &[
                &sources.reports,
                &sources.quota,
                &sources.pinned,
                &sources.collected,
                &sources.answers,
                &sources.facts_data,
            ],
            &sources.facts,
        ) {
            sources.facts = facts;
        }
        let pushed = [
            (REPORTS, sources.reports, None),
            (QUOTA, sources.quota, None),
            (PINNED, sources.pinned, None),
            (COLLECTED, sources.collected, None),
            (ANSWERS, sources.answers, None),
            (FACTS, sources.facts_data, Some(sources.facts)),
        ];
        // Prepare every changed source completely outside `Feeds::held`, then commit atomically.
        let mut existing: [Option<Arc<Generation>>; 6] = [None, None, None, None, None, None];
        let mut changed_meta: Vec<ChangedSource> = Vec::new();
        for (index, (source, text, facts)) in pushed.into_iter().enumerate() {
            let facts_text = facts.unwrap_or_else(|| "{}".to_owned());
            let text_hash: [u8; 32] = sha2::Sha256::digest(text.as_bytes()).into();
            let facts_hash: [u8; 32] = sha2::Sha256::digest(facts_text.as_bytes()).into();
            if let Some((held_text, held_facts, held_gen)) = cache.held.get(source)
                && *held_text == text_hash
                && *held_facts == facts_hash
            {
                existing[index] = Some(Arc::clone(held_gen));
                continue;
            }
            let generation_id = self.next_generation(now);
            let body = format!(
                "{{\"generation\":\"{generation_id}\",\"data\":{text},\"facts\":{facts_text}}}"
            );
            drop(text);
            drop(facts_text);
            let prepared = match self.feeds.prepare_generation(source, body.as_bytes()) {
                Ok(generation) => generation,
                Err(reason) => {
                    self.set_published(PublishedState::Declined {
                        zone_name: zone.name,
                        zone: zone.zone,
                        reason,
                    });
                    let mut state = self.state();
                    if state.sources_gen == snap.sources_gen {
                        state.sources_dirty = false;
                    }
                    return false;
                }
            };
            changed_meta.push((index, source, text_hash, facts_hash, prepared));
        }
        let any_changed = !changed_meta.is_empty();
        if any_changed {
            let commit_input: Vec<(&str, Arc<Generation>)> = changed_meta
                .iter()
                .map(|(_, source, _, _, generation)| (*source, Arc::clone(generation)))
                .collect();
            #[cfg(test)]
            {
                let mut hook = self.before_commit.lock().unwrap_or_else(|p| p.into_inner());
                if let Some((ready, resume)) = hook.take() {
                    let _ = ready.send(());
                    let _ = resume.recv();
                }
            }
            let committed = match self.feeds.commit_generations(&commit_input) {
                Ok(committed) => committed,
                Err(reason) => {
                    self.set_published(PublishedState::Declined {
                        zone_name: zone.name,
                        zone: zone.zone,
                        reason,
                    });
                    let mut state = self.state();
                    if state.sources_gen == snap.sources_gen {
                        state.sources_dirty = false;
                    }
                    return false;
                }
            };
            for ((index, source, text_hash, facts_hash, prepared), held_gen) in
                changed_meta.into_iter().zip(committed)
            {
                // Keep the digests paired atomically with the committed generation: if an external
                // PUT outran this commit (a newer generation kept), hash `held_gen`'s own data and
                // facts instead of recording Produce's digests (review L4).
                let (committed_text_hash, committed_facts_hash) =
                    if Arc::ptr_eq(&held_gen, &prepared) {
                        (text_hash, facts_hash)
                    } else {
                        (
                            sha2::Sha256::digest(held_gen.data.get().as_bytes()).into(),
                            sha2::Sha256::digest(held_gen.facts.get().as_bytes()).into(),
                        )
                    };
                cache.held.insert(
                    source,
                    (
                        committed_text_hash,
                        committed_facts_hash,
                        Arc::clone(&held_gen),
                    ),
                );
                existing[index] = Some(held_gen);
            }
        }
        let generations: Option<[Arc<Generation>; 6]> = existing
            .into_iter()
            .collect::<Option<Vec<_>>>()
            .and_then(|vec| vec.try_into().ok());
        if let Some(generations) = generations {
            if prewarm_compose
                && (any_changed || !matches_published)
                && let Some((clock, day)) = zone.zone.words(clamp_i64(now))
            {
                crate::status_fed::prewarm_generations(&generations, clamp_i64(now), clock, day);
            }
            self.set_published(PublishedState::Ready(Arc::new(Published {
                zone_name: zone.name,
                zone: zone.zone,
                generations,
            })));
        }
        {
            let mut state = self.state();
            if state.sources_gen == snap.sources_gen {
                state.sources_dirty = false;
            }
        }
        any_changed
    }

    fn publish_once_and_hint(&self, prewarm_compose: bool) {
        if self.publish(None, prewarm_compose) {
            self.hint();
        }
    }

    fn set_request_zone(&self, now: u64, time_zone: &str, zone: &Zone) {
        let mut state = self.state();
        if state
            .zone
            .as_ref()
            .is_none_or(|held| held.name != time_zone || held.zone != *zone)
        {
            state.zone = Some(RequestZone {
                name: time_zone.to_owned(),
                zone: zone.clone(),
                sample_now: now,
            });
            state.pinned_dirty = true;
            state.sources_dirty = true;
            state.pinned_gen = state.pinned_gen.wrapping_add(1);
            state.sources_gen = state.sources_gen.wrapping_add(1);
        }
    }

    async fn published_for_async(
        self: &Arc<Self>,
        now: u64,
        time_zone: &str,
        zone: &Zone,
    ) -> Result<Arc<Published>, Decline> {
        // Fast path: 10 ns lock, never waits on a producer pass.
        if let Some(current) = self.current_published()
            && current.matches_zone(time_zone, zone)
        {
            return match current {
                PublishedState::Ready(published) => Ok(published),
                PublishedState::Declined { reason, .. } => Err(reason),
            };
        }
        // Cold start or zone change: share one background publish without holding any pool permit.
        let mut rx = self.published_rx.clone();
        loop {
            let _ = rx.borrow_and_update();
            if let Some(current) = self.current_published()
                && current.matches_zone(time_zone, zone)
            {
                return match current {
                    PublishedState::Ready(published) => Ok(published),
                    PublishedState::Declined { reason, .. } => Err(reason),
                };
            }
            self.set_request_zone(now, time_zone, zone);
            self.trigger_publish();
            if rx.changed().await.is_err() {
                return Err(Decline::Internal);
            }
        }
    }

    /// One owned Status request (async, singleflight): reads from the last good generation while
    /// producers recompute outside the lock, and shares one compose across concurrent reads.
    pub(crate) async fn view_async(
        self: &Arc<Self>,
        limits: Option<&Limits>,
        now: u64,
        time_zone: &str,
        zone_facts: &ZoneFacts,
    ) -> Result<(Vec<u8>, String), Decline> {
        self.touch();
        self.maybe_collect();
        let zone = zone_facts.zone().ok_or(Decline::Locale)?;
        let now_i64 = clamp_i64(now);
        let (clock, day) = zone.words(now_i64).ok_or(Decline::Locale)?;
        let published = self.published_for_async(now, time_zone, &zone).await?;
        self.compose
            .answer_generations_async(
                limits,
                published.generations.clone(),
                now_i64,
                clock,
                day,
                true,
            )
            .await
    }

    /// Synchronous owned Status read: answers from the last good generation without waiting on
    /// an in-flight producer pass; builds synchronously only when cold or on a zone mismatch.
    #[cfg(test)]
    pub(crate) fn view(
        self: &Arc<Self>,
        now: u64,
        time_zone: &str,
        zone_facts: &ZoneFacts,
    ) -> Result<(Vec<u8>, String), Decline> {
        self.touch();
        self.maybe_collect();
        let zone = zone_facts.zone().ok_or(Decline::Locale)?;
        let now_i64 = clamp_i64(now);
        let (clock, day) = zone.words(now_i64).ok_or(Decline::Locale)?;
        let published = match self.current_published() {
            Some(PublishedState::Ready(published))
                if published.zone_name == time_zone && published.zone == zone =>
            {
                published
            }
            Some(PublishedState::Declined {
                zone_name,
                zone: held,
                reason,
            }) if zone_name == time_zone && held == zone => return Err(reason),
            _ => {
                self.publish(Some((time_zone, &zone, now)), false);
                match self.current_published() {
                    Some(PublishedState::Ready(published))
                        if published.zone_name == time_zone && published.zone == zone =>
                    {
                        published
                    }
                    Some(PublishedState::Declined { reason, .. }) => return Err(reason),
                    _ => return Err(Decline::Unsupported),
                }
            }
        };
        crate::status_fed::answer_generations(&published.generations, now_i64, clock, day)
    }

    /// `GET /api/status/answers`: the page's answer history from the owned feed (newest first,
    /// at most `limit`). Refreshes outside the state lock when dirty.
    pub(crate) fn answers_view(&self, limit: usize) -> Json {
        let (answers, dirty_inputs) = {
            let state = self.state();
            let dirty = state
                .answers_dirty
                .then(|| (Arc::clone(&state.reports_rows), state.answers_gen));
            (Arc::clone(&state.answers), dirty)
        };
        let answers = if let Some((reports_rows, answers_gen)) = dirty_inputs {
            let cards: Vec<(Json, String)> = reports_rows
                .cards
                .iter()
                .map(|row| (row.doc.clone(), row.sha256.clone()))
                .collect();
            let fresh = Arc::new(self.answers.feed(&cards));
            let mut state = self.state();
            if state.answers_gen == answers_gen {
                state.answers = Arc::clone(&fresh);
                state.answers_dirty = false;
            }
            fresh
        } else {
            answers
        };
        crate::status_answers_route::history_json(&answers.answers, &answers.sends, limit)
    }

    /// The producers' hint: one `status-changed` per window, once anything changed.
    fn hint(&self) {
        let now = now_ms();
        let send = {
            let mut state = self.state();
            if now.saturating_sub(state.hinted_at) < HINT_GAP_MS {
                false
            } else {
                state.hinted_at = now;
                true
            }
        };
        if send {
            let generation = self.next_generation(now);
            self.feeds.publish_event(json!({
                "event": "status-changed",
                "generation": generation,
            }));
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Readings

/// The registry's computers in shipped order: `(machineId, os, sshAlias)`.
fn computers_rows() -> Vec<(String, String, Option<String>)> {
    aac_status::computers::machine_rows()
        .into_iter()
        .map(|(machine, os, alias)| (machine.to_owned(), os.to_owned(), alias.map(str::to_owned)))
        .collect()
}

fn hostname() -> String {
    if cfg!(windows) {
        return std::env::var("COMPUTERNAME").unwrap_or_default();
    }
    // Linux keeps the name in procfs; macOS has `uname -n`. (No unsafe call for one word.)
    if let Some(name) = read_file("/proc/sys/kernel/hostname") {
        return name.trim().to_owned();
    }
    std::process::Command::new("uname")
        .arg("-n")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
        .unwrap_or_default()
}

fn local_machine() -> String {
    let computer = aac_status::computers::local_computer(&hostname(), std::env::consts::OS);
    aac_status::computers::computer(computer)
        .and_then(|row| row["machineId"].as_str())
        .unwrap_or(computer)
        .to_owned()
}

fn read_file(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `statfs(home)`: `(free bytes, total bytes)` as Node's `bavail*bsize` and `blocks*bsize`,
/// from `df -Pk <home>` (the same 1 KiB blocks and the same filesystem the probe reads).
#[cfg(not(windows))]
fn statfs_at(path: &str) -> Option<(f64, f64)> {
    let output = std::process::Command::new("df")
        .args(["-Pk", path])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let row = text.lines().nth(1)?;
    let fields: Vec<&str> = row.split_whitespace().collect();
    let total: f64 = fields.get(1)?.parse().ok()?;
    let available: f64 = fields.get(3)?.parse().ok()?;
    Some((available * 1024.0, total * 1024.0))
}

/// `os.homedir()`.
#[cfg(not(windows))]
fn home_dir() -> Option<String> {
    std::env::home_dir().map(|home| home.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn linux_cpus() -> usize {
    read_file("/proc/cpuinfo")
        .map(|text| {
            text.lines()
                .filter(|line| line.starts_with("processor"))
                .count()
        })
        .filter(|count| *count > 0)
        .unwrap_or(0)
}

#[cfg(not(windows))]
fn linux_total_memory() -> (Option<f64>, Option<f64>) {
    let Some(text) = read_file("/proc/meminfo") else {
        return (None, None);
    };
    let field = |name: &str| -> Option<f64> {
        for line in text.lines() {
            let Some(rest) = line.strip_prefix(name) else {
                continue;
            };
            let digits: String = rest
                .trim_start()
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            if digits.is_empty() {
                return None;
            }
            return digits.parse::<f64>().ok().map(|kb| kb * 1024.0);
        }
        None
    };
    (
        field("MemTotal:"),
        machines_produce::mem_available_from(&text),
    )
}

/// A synchronous Mac probe with the same 5 s deadline the reference gives it.
#[cfg(not(windows))]
fn mac_probe(command: &str, timeout: Duration) -> Option<String> {
    use std::io::Read;
    let mut child = std::process::Command::new("/bin/sh")
        .args(["-c", command])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match child.try_wait().ok()? {
            Some(status) if status.success() => {
                let mut out = String::new();
                child.stdout.take()?.read_to_string(&mut out).ok()?;
                return Some(out);
            }
            Some(_) => return None,
            None if std::time::Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            None => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// `readLocalMachine`: this computer, from the host facts the way Node read them.
fn local_sample() -> Result<Sample, String> {
    #[cfg(windows)]
    {
        Ok(windows_sample())
    }
    #[cfg(not(windows))]
    {
        unix_sample()
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
fn windows_sample() -> Sample {
    use windows_sys::Win32::{
        Storage::FileSystem::GetDiskFreeSpaceExW,
        System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
    };
    let mut memory = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    // SAFETY: the writable structure has the size required by GlobalMemoryStatusEx.
    let has_memory = unsafe { GlobalMemoryStatusEx(&mut memory) } != 0;
    machines_produce::local_sample(&machines_produce::LocalReads {
        platform: Platform::Windows,
        cpus: std::thread::available_parallelism().map_or(0, usize::from),
        total_mem: has_memory.then_some(memory.ullTotalPhys as f64),
        free_mem: has_memory.then_some(memory.ullAvailPhys as f64),
        load1: None,
        read_file: &|_| None,
        home: &home_dir,
        statfs: &|path| {
            use std::os::windows::ffi::OsStrExt;
            let wide: Vec<u16> = std::ffi::OsStr::new(path)
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            let (mut free, mut total) = (0u64, 0u64);
            // SAFETY: wide is NUL-terminated and the two requested outputs are live u64s.
            let ok = unsafe {
                GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
            };
            (ok != 0).then_some((free as f64, total as f64))
        },
        mac_memory: None,
    })
}

/// `os.homedir()`.
#[cfg(windows)]
fn home_dir() -> Option<String> {
    std::env::home_dir().map(|home| home.to_string_lossy().into_owned())
}

#[cfg(not(windows))]
fn unix_sample() -> Result<Sample, String> {
    let platform = if cfg!(target_os = "macos") {
        Platform::Mac
    } else {
        Platform::Linux
    };
    if platform == Platform::Mac {
        let probe = mac_probe(
            &format!("export LC_ALL=C; {}", machines_produce::MAC_MEMORY_PROBE),
            Duration::from_millis(5_000),
        )
        .and_then(|out| machines_produce::parse_probe(HostOs::Mac, &out));
        let cpus = probe.as_ref().and_then(|sample| sample.cores).unwrap_or(0);
        let load1 = probe.as_ref().and_then(|sample| sample.load1);
        return Ok(machines_produce::local_sample(
            &machines_produce::LocalReads {
                platform,
                cpus: usize::try_from(cpus).unwrap_or(0),
                total_mem: None,
                free_mem: None,
                load1,
                read_file: &read_file,
                home: &home_dir,
                statfs: &statfs_at,
                mac_memory: probe,
            },
        ));
    }
    let (total, available) = linux_total_memory();
    let load1 = read_file("/proc/loadavg")
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok());
    Ok(machines_produce::local_sample(
        &machines_produce::LocalReads {
            platform,
            cpus: linux_cpus(),
            total_mem: total,
            free_mem: available,
            load1,
            read_file: &read_file,
            home: &home_dir,
            statfs: &statfs_at,
            mac_memory: None,
        },
    ))
}

/// `commandFailure`: the fixed words for a failed read.
fn command_failure(tool: &str, result: &crate::status_run::Run) -> String {
    if result.missing {
        return format!("{tool} is not installed here.");
    }
    if result.timed_out {
        return format!("{tool} did not answer in time.");
    }
    format!("{tool} failed.")
}

/// One remote machine over the dashboard's ssh alias.
async fn read_remote(host: HostOs, alias: &str) -> Result<Sample, String> {
    let args = ssh_argv(alias, &machines_produce::probe_command(host), Some((5, 2)));
    let result = run(Path::new("ssh"), &args, 20_000, 64 * 1024).await;
    if result.code != Some(0) {
        return Err(command_failure("ssh", &result));
    }
    machines_produce::parse_probe(host, &result.stdout)
        .ok_or_else(|| "The machine answered without a reading.".to_owned())
}

/// `collectMachines`' per-machine step: the sample with its read times, a failed part named and
/// its earlier reading kept.
fn apply_machine(
    before: Collected<Sample>,
    result: Result<Sample, String>,
    now: i64,
) -> Collected<Sample> {
    let sample = match result {
        Ok(sample) => sample,
        Err(reason) => return before.failed(&reason, now, false),
    };
    let earlier = before.data.clone();
    let busy = machines_produce::busy_between(
        earlier.as_ref().and_then(|sample| sample.cpu_times),
        sample.cpu_times,
    );
    let busy_pct = busy.or(sample.busy_pct);
    let mut data = sample.clone();
    data.busy_pct = busy_pct;
    data.cpu_read_at = if busy_pct.is_none() {
        earlier.as_ref().and_then(|held| held.cpu_read_at)
    } else {
        Some(now)
    };
    data.disk_read_at = sample.disk.is_some().then_some(now);
    data.memory_read_at = sample.memory_error.is_none().then_some(now);
    let mut problems = Vec::new();
    if sample.disk.is_none() && sample.disk_error.is_some() {
        if let Some(held) = &earlier {
            data.disk = held.disk.clone();
            data.disk_read_at = if held.disk.is_some() {
                held.disk_read_at.or(before.as_of)
            } else {
                None
            };
        }
        problems.push(next_problem(
            before
                .problems
                .as_ref()
                .and_then(|list| list.iter().find(|p| p.item == "disk")),
            "disk",
            sample.disk_error.as_deref().unwrap_or(""),
            now,
        ));
    }
    if sample.cpu_error.is_some() {
        if let Some(held) = &earlier {
            data.busy_pct = held.busy_pct;
            data.cpu_times = held.cpu_times;
            data.cpu_read_at = held.cpu_read_at;
        }
        problems.push(next_problem(
            before
                .problems
                .as_ref()
                .and_then(|list| list.iter().find(|p| p.item == "cpu")),
            "cpu",
            sample.cpu_error.as_deref().unwrap_or(""),
            now,
        ));
    }
    if sample.memory_error.is_some() {
        if let Some(held) = &earlier {
            data.mem_available_bytes = held.mem_available_bytes;
            data.mem_total_bytes = held.mem_total_bytes;
            data.memory_read_at = held.memory_read_at;
        }
        problems.push(next_problem(
            before
                .problems
                .as_ref()
                .and_then(|list| list.iter().find(|p| p.item == "memory")),
            "memory",
            sample.memory_error.as_deref().unwrap_or(""),
            now,
        ));
    }
    if problems.is_empty() {
        Collected::succeeded(data, now)
    } else {
        Collected::succeeded(data, now).with_problems(problems)
    }
}

async fn read_browsers(workers: Option<&[&str]>) -> Result<Vec<BrowserMachine>, (String, bool)> {
    let program = find_executable("browser-check");
    let result = run(
        &program,
        &["slots".to_owned(), "--json".to_owned()],
        45_000,
        4 * 1024 * 1024,
    )
    .await;
    if result.code != Some(0) {
        return Err((command_failure("browser-check", &result), result.missing));
    }
    let raw: Json = serde_json::from_str(&result.stdout).map_err(|_| {
        (
            "browser-check answered in an unknown shape.".to_owned(),
            false,
        )
    })?;
    browsers::parse_slots_with_workers(&raw, workers).map_err(|reason| (reason.to_owned(), false))
}

type SimlockReading = (SimlockStatus, bool, Option<Vec<&'static str>>);

async fn read_simlock(program: &Path) -> Result<SimlockReading, (String, bool)> {
    let mut args = vec![
        "status".to_owned(),
        "--json".to_owned(),
        "--gateway".to_owned(),
    ];
    let mut result = run(program, &args, 20_000, 4 * 1024 * 1024).await;
    let worker_only = result.code != Some(0);
    if worker_only {
        args.pop();
        result = run(program, &args, 20_000, 4 * 1024 * 1024).await;
    }
    if result.code != Some(0) {
        return Err((command_failure("simlock", &result), result.missing));
    }
    let raw: Json = serde_json::from_str(&result.stdout)
        .map_err(|_| ("simlock answered in an unknown shape.".to_owned(), false))?;
    let status =
        simlock::parse_status(&raw).map_err(|error| (error.message().to_owned(), false))?;
    let workers = raw["workers"]
        .as_array()
        .filter(|_| !worker_only)
        .map(|workers| {
            workers
                .iter()
                .filter_map(|worker| {
                    aac_status::computers::exact_computer(worker["label"].as_str()?)
                })
                .collect()
        });
    Ok((status, worker_only, workers))
}

fn simlock_problems(
    status: &SimlockStatus,
    worker_only: bool,
    before: Option<&[ReadProblem]>,
    now: i64,
) -> Vec<ReadProblem> {
    let mut problems = Vec::with_capacity(status.problems.len() + usize::from(worker_only));
    if worker_only {
        problems.push(next_problem(
            before.and_then(|list| list.iter().find(|held| held.item == "Simlock")),
            "Simlock",
            "Read this computer's Simlock worker only; the fleet gateway did not answer",
            now,
        ));
    }
    problems.extend(status.problems.iter().map(|problem| {
        if let Some(seen) = problem
            .last_seen_at
            .as_deref()
            .and_then(aac_num::ecma::parse_instant)
        {
            return ReadProblem {
                item: problem.item.clone(),
                reason: problem.reason.clone(),
                since: seen,
            };
        }
        next_problem(
            before.and_then(|list| list.iter().find(|held| held.item == problem.item)),
            &problem.item,
            &problem.reason,
            now,
        )
    }));
    problems
}

#[cfg(all(test, unix))]
mod simlock_tests {
    use super::{read_simlock, simlock_problems};
    use std::os::unix::fs::PermissionsExt;

    #[tokio::test]
    async fn simlock_prefers_the_fleet_and_labels_a_worker_only_answer() {
        let temp = tempfile::tempdir().expect("fixture directory");
        for (gateway, worker_only) in [
            (
                "echo '{\"capacity\":{\"ios\":{\"running\":3}}}'; exit 0",
                false,
            ),
            ("exit 1", true),
        ] {
            let program = temp
                .path()
                .join(if worker_only { "local" } else { "fleet" });
            std::fs::write(
                &program,
                format!(
                    "#!/bin/sh\nif [ \"${{3-}}\" = --gateway ]; then {gateway}; fi\necho '{{\"capacity\":{{\"ios\":{{\"running\":1}}}}}}'\n"
                ),
            )
            .expect("fixture script");
            std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700))
                .expect("fixture executable");
            let (status, local, _) = read_simlock(&program)
                .await
                .expect("Simlock fixture answer");
            assert_eq!(local, worker_only);
            assert_eq!(
                status.capacity[0].running,
                Some(if worker_only { 1 } else { 3 })
            );
            let problems = simlock_problems(&status, local, None, 100);
            if worker_only {
                assert_eq!(problems.len(), 1);
                assert_eq!(
                    problems[0].reason,
                    "Read this computer's Simlock worker only; the fleet gateway did not answer"
                );
                assert_eq!(
                    simlock_problems(&status, local, Some(&problems), 200)[0].since,
                    100
                );
            } else {
                assert!(problems.is_empty());
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Pinned threads

/// The machines in panel order, with the local database read here and the others over ssh.
async fn read_pins(
    rows: &[(String, String, Option<String>)],
    local: String,
    machines: BTreeMap<String, PinMachine>,
) -> Pins {
    let mut pins = Pins {
        machines,
        ..Pins::default()
    };
    let now = now_ms();
    let mut pending = Vec::with_capacity(rows.len());
    for (machine, os, alias) in rows {
        let machine = machine.clone();
        let os = HostOs::of(os);
        let alias = alias.clone();
        let local_machine = machine == local;
        pending.push(async move {
            let outcome = if local_machine {
                tokio::task::spawn_blocking(local_pins)
                    .await
                    .unwrap_or_else(|_| Err("io".to_owned()))
            } else {
                match (os, alias) {
                    (Some(host), Some(alias)) => remote_pins(&machine, host, &alias).await,
                    _ => Err("did_not_answer".to_owned()),
                }
            };
            (machine, outcome)
        });
    }
    let outcomes: Vec<(String, Result<aac_status::pins::PinsRead, String>)> =
        futures_util::future::join_all(pending).await;
    let by_machine: HashMap<String, Result<aac_status::pins::PinsRead, String>> =
        outcomes.into_iter().collect();
    let mut lists = Vec::new();
    for (machine, _, _) in rows {
        let Some(outcome) = by_machine.get(machine) else {
            continue;
        };
        let state = pins.machines.entry(machine.clone()).or_default();
        match outcome {
            Ok(read) => {
                state.last_good = Some(read.clone());
                state.not_shown = read.not_shown;
                state.failures = 0;
                state.problem = None;
            }
            Err(code) => {
                if state.last_good.is_some() && state.failures == 0 {
                    state.failures = 1;
                } else {
                    state.failures += 1;
                    state.last_good = None;
                    state.not_shown = 0;
                    let since = state
                        .problem
                        .as_ref()
                        .filter(|(held, _)| held == code)
                        .map_or(now, |(_, since)| *since);
                    state.problem = Some((code.clone(), since));
                }
            }
        }
        if let Some(read) = &state.last_good {
            lists.push(aac_status::pins::MachinePins {
                name: machine.clone(),
                pinned: read.pinned.clone(),
            });
        }
    }
    pins.merged = aac_status::pins::merge_pins(&lists);
    // The panel names its machines in the registry's order (`PINS_MACHINES`), not this map's.
    pins.problems = rows
        .iter()
        .filter_map(|(machine, _, _)| {
            let state = pins.machines.get(machine)?;
            if let Some((code, _)) = &state.problem {
                return Some(aac_status::pinned::MachineProblemIn {
                    machine: machine.clone(),
                    code: code.clone(),
                    not_shown: None,
                });
            }
            (state.last_good.is_some() && state.not_shown > 0).then(|| {
                aac_status::pinned::MachineProblemIn {
                    machine: machine.clone(),
                    code: "truncated".to_owned(),
                    not_shown: Some(u64::try_from(state.not_shown).unwrap_or(u64::MAX)),
                }
            })
        })
        .collect();
    pins.collected = true;
    pins
}

fn local_pins() -> Result<aac_status::pins::PinsRead, String> {
    let Some(path) = aac_status::pins::default_db_path(None) else {
        return Err("missing".to_owned());
    };
    aac_status::pins::read_pinned_threads(&path).map_err(|error| match error.code {
        "missing" | "shape" | "limit" | "io" => error.code.to_owned(),
        _ => "io".to_owned(),
    })
}

async fn remote_pins(
    machine: &str,
    host: HostOs,
    alias: &str,
) -> Result<aac_status::pins::PinsRead, String> {
    let args = ssh_argv(alias, &remote_pins_command(host), Some((5, 2)));
    let result = run(Path::new("ssh"), &args, 30_000, 1024 * 1024).await;
    if result.code != Some(0) {
        return Err("did_not_answer".to_owned());
    }
    aac_status::pins::parse_remote_pins(machine, &result.stdout).map_err(|_| "shape".to_owned())
}

/// `PINNED_THREADS_QUERY`, the same predicate and order the local reader uses.
const PINNED_THREADS_QUERY: &str = "SELECT thread_id, title, json_extract(payload_json, '$.pinnedAt') AS pinnedAt, json_type(payload_json, '$.pinnedAt') AS pinnedType, json_extract(payload_json, '$.pinOrderKey') AS pinOrderKey, json_type(payload_json, '$.pinOrderKey') AS orderType, json_type(payload_json) AS rootType, json_type(payload_json, '$.lineage') AS lineageType, json_type(payload_json, '$.lineage.parentThreadId') AS parentType, json_extract(payload_json, '$.createdAt') AS createdAt, json_type(payload_json, '$.createdAt') AS createdType FROM orchestration_v2_projection_threads WHERE archived_at IS NULL AND deleted_at IS NULL AND json_extract(payload_json, '$.pinnedAt') IS NOT NULL AND (json_type(payload_json, '$.lineage') IS NOT 'object' OR json_type(payload_json, '$.lineage.parentThreadId') IS NOT 'text' OR (json_type(payload_json, '$.pinnedAt') = 'text' AND json_type(payload_json, '$.createdAt') = 'text' AND (julianday(json_extract(payload_json, '$.pinnedAt')) - julianday(json_extract(payload_json, '$.createdAt'))) * 86400.0 > 59.999)) ORDER BY (json_extract(payload_json, '$.pinOrderKey') IS NULL) ASC, json_extract(payload_json, '$.pinOrderKey') ASC, json_extract(payload_json, '$.pinnedAt') ASC, thread_id ASC";

/// The one read-only pins command a host runs (the reference's fixed sqlite3 paths).
fn remote_pins_command(host: HostOs) -> String {
    if host == HostOs::Windows {
        let script = format!(
            "& 'C:\\ProgramData\\chocolatey\\bin\\sqlite3.exe' -readonly -json (Join-Path $env:USERPROFILE '.t3\\userdata\\statev2.sqlite') '{}'",
            PINNED_THREADS_QUERY.replace('\'', "''")
        );
        let encoded = {
            use base64::Engine as _;
            let bytes: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
            base64::engine::general_purpose::STANDARD.encode(bytes)
        };
        return format!("powershell.exe -NoProfile -NonInteractive -EncodedCommand {encoded}");
    }
    format!(
        "for s in /usr/bin/sqlite3 /usr/local/bin/sqlite3 /opt/homebrew/bin/sqlite3 \"$(command -v sqlite3)\"; do [ -n \"$s\" ] && [ -x \"$s\" ] && exec \"$s\" -readonly -json \"file:$HOME/.t3/userdata/statev2.sqlite?mode=ro\" \"{PINNED_THREADS_QUERY}\"; done; exit 127"
    )
}

// ---------------------------------------------------------------------------------------------
// Feed values

/// `reports::state_json` with each thread's owner wait resolved against closed cards.
fn resolved_reports(reports: &AgentReportsState, answers: &AnswersFeed) -> Json {
    let mut value = aac_status::share::reports::state_json(reports);
    let closed = closing_card_versions(&answers.answers);
    let cards: Vec<aac_status::produce::answered::CardRow<'_>> = reports
        .cards
        .iter()
        .map(|row| aac_status::produce::answered::CardRow {
            writer: &row.writer,
            doc: &row.doc,
            sha256: &row.sha256,
        })
        .collect();
    let waits = resolved_card_waits(&cards, &closed);
    let Some(threads) = value.get_mut("threads").and_then(Json::as_array_mut) else {
        return value;
    };
    for thread in threads {
        let writer = thread
            .get("writer")
            .and_then(Json::as_str)
            .unwrap_or("")
            .to_owned();
        let Some(doc) = thread.get_mut("doc").and_then(Json::as_object_mut) else {
            continue;
        };
        // The reference always writes the resolved member: a doc without `blockedOn` gains
        // `blockedOn: null`, and a resolved owner wait becomes null.
        let active = aac_status::produce::answered::active_blocker(
            &waits,
            &writer,
            doc.get("blockedOn").cloned(),
        );
        doc.insert("blockedOn".to_owned(), active.unwrap_or(Json::Null));
    }
    value
}

/// The `machines` member: one collected wrapper per registered machine, in registry order.
fn machines_json(machines: &BTreeMap<String, Collected<Sample>>) -> Json {
    let mut out = Map::new();
    for machine in aac_status::computers::machine_ids() {
        let Some(collected) = machines.get(machine) else {
            continue;
        };
        let mut value = Map::new();
        value.insert(
            "data".into(),
            collected
                .data
                .as_ref()
                .map_or(Json::Null, Sample::to_collected_json),
        );
        value.insert(
            "asOf".into(),
            collected.as_of.map_or(Json::Null, Json::from),
        );
        value.insert(
            "error".into(),
            collected.error.as_ref().map_or(
                Json::Null,
                |error| json!({"reason": error.reason, "since": error.since}),
            ),
        );
        if let Some(missing) = collected.missing {
            value.insert("missing".into(), Json::Bool(missing));
        }
        if let Some(problems) = &collected.problems {
            value.insert("problems".into(), json!(problems));
        }
        out.insert(machine.to_owned(), Json::Object(value));
    }
    Json::Object(out)
}

/// The pinned panel for these readings, or `None` when the projection refuses a document.
fn pinned_panel(
    reports_rows: &AgentReportsState,
    docs_rows: &StatusDocsState,
    pins: &Pins,
    answers: &AnswersFeed,
    now: i64,
) -> Option<Json> {
    // Before the first collect there are no pins to compose over; the panel says it has not
    // been read yet, as the collector's view did.
    if !pins.collected {
        return None;
    }
    let closed = closing_card_versions(&answers.answers);
    let cards: Vec<aac_status::produce::answered::CardRow<'_>> = reports_rows
        .cards
        .iter()
        .map(|row| aac_status::produce::answered::CardRow {
            writer: &row.writer,
            doc: &row.doc,
            sha256: &row.sha256,
        })
        .collect();
    let waits: ResolvedWaits = resolved_card_waits(&cards, &closed);
    let input = pinned_input::compose_input(
        reports_rows,
        docs_rows,
        &pins.merged,
        &pins.problems,
        now,
        &closed,
        &waits,
    )?;
    let output = aac_status::pinned::compose(&input);
    serde_json::to_value(output).ok()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod flicker_tests {
    use super::*;
    use crate::limits::Limits;
    use aac_status::share::reports::SeenReport;

    /// Wait for a real event (a counter, a flag) with a deadline guard. The condition is the
    /// barrier; no fixed sleep assumes how long the event takes under load.
    async fn wait_until(mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "condition never became true"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn utc_zone(now: u64) -> ZoneFacts {
        let now = clamp_i64(now);
        let span = 30 * 86_400_000;
        ZoneFacts {
            from: now - span,
            to: now + span,
            offsets: vec![[now - span, 0]],
        }
    }

    pub(super) fn scan_with_thread(writer: &str, now: u64) -> Scan {
        let mut reports = empty_reports();
        reports.state = "ok";
        reports.scanned_at = Some(now);
        reports.complete = true;
        reports.threads.push(SeenReport {
            file: format!("writers/{writer}/thread.json"),
            writer: writer.to_owned(),
            doc: json!({
                "schema": "agent-reports/thread@1",
                "writer": writer,
                "seq": 1,
                "writtenAt": "2026-11-24T00:00:00.000Z",
                "project": "aac",
                "stream": "status",
                "state": "active",
                "summary": format!("Thread {writer}"),
                "blockedOn": null,
                "ttlSeconds": 3600,
                "links": [],
            }),
            sha256: "a".repeat(64),
            first_seen_at: now,
            written_at_ms: clamp_i64(now),
            problem: None,
        });
        Scan {
            reports,
            docs: StatusDocsState {
                state: "ok",
                scanned_at: Some(now),
                docs: Vec::new(),
                problems: Vec::new(),
            },
            quota: Json::Null,
            registry: json!({"data": null, "asOf": null, "error": null}),
        }
    }

    /// Reproduces the live 503 flicker: a slow producer pass (`publish`) must not block
    /// Status reads past the 400 ms route budget or fill the status pool.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_read_does_not_decline_during_a_slow_producer_pass() {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let reports = scratch.path().join("reports");
        std::fs::create_dir_all(&ccs).unwrap();
        std::fs::create_dir_all(&reports).unwrap();

        let feeds = Feeds::new();
        let answers = AnswersOwned::new(ccs.clone(), reports);
        let produce = Produce::new(feeds, Arc::clone(&answers), ccs);
        let limits = Limits::new();
        let now = 1_795_000_000_000_u64;
        let zone = utc_zone(now);

        // Establish a last good generation.
        let initial = {
            let produce = Arc::clone(&produce);
            let zone = zone.clone();
            limits
                .blocking_status(move || produce.view(now, "UTC", &zone))
                .await
        };
        assert!(initial.is_ok(), "initial read failed: {initial:?}");

        // Hold the answers engine lock until the test releases it, so the background producer
        // pass is slow for exactly as long as the reads below need (matching
        // `answers_owned::jobs` / slow share I/O during `publish`). No wall-clock assumption.
        let (hold_ready_tx, hold_ready_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let hold_answers = Arc::clone(&answers);
        let holder = std::thread::spawn(move || {
            let _guard = hold_answers.lock_engine_for_test();
            let _ = hold_ready_tx.send(());
            let _ = release_rx.recv();
        });
        hold_ready_rx.recv().unwrap();

        // Trigger a background producer pass that must refresh answers and republish.
        let entered = Arc::clone(&produce.publish_entered);
        let before = entered.load(Ordering::SeqCst);
        produce.mark_answers();
        let publisher = {
            let produce = Arc::clone(&produce);
            tokio::task::spawn_blocking(move || produce.publish(None, true))
        };
        // Barrier: the pass has entered and must block on the held engine lock before it can
        // commit, so every read below runs during a genuinely slow pass.
        wait_until(|| entered.load(Ordering::SeqCst) > before).await;

        // While the slow producer pass is running, Status reads must answer within the
        // 400 ms route budget from the last good generation (never `deadline` or `busy`).
        for attempt in 0..5 {
            let produce = Arc::clone(&produce);
            let zone = zone.clone();
            let read_now = now + attempt * 10;
            let outcome = tokio::time::timeout(
                Duration::from_millis(400),
                limits.blocking_status(move || produce.view(read_now, "UTC", &zone)),
            )
            .await;
            match outcome {
                Ok(Ok((bytes, etag))) => {
                    assert!(!bytes.is_empty());
                    assert!(etag.starts_with("W/\""));
                }
                Ok(Err(reason)) => panic!("attempt {attempt} declined with {reason:?}"),
                Err(_) => panic!("attempt {attempt} hit 400ms deadline during slow producer pass"),
            }
        }

        let _ = release_tx.send(());
        let _ = holder.join();
        let _ = publisher.await;
    }

    /// Reads answer from the last good generation while `after_scan` recomputes sources outside
    /// the lock, then see the swapped generation once the producer pass completes.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn status_read_answers_from_last_good_generation_during_slow_source_build_then_swaps() {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let reports = scratch.path().join("reports");
        std::fs::create_dir_all(&ccs).unwrap();
        std::fs::create_dir_all(&reports).unwrap();

        let feeds = Feeds::new();
        let answers = AnswersOwned::new(ccs.clone(), reports);
        let produce = Produce::new(feeds, Arc::clone(&answers), ccs);
        let limits = Limits::new();
        let now = 1_795_000_000_000_u64;
        let zone = utc_zone(now);

        produce.after_scan(scan_with_thread("vm.first", now)).await;
        let (first_bytes, _) = produce
            .view_async(Some(&limits), now, "UTC", &zone)
            .await
            .expect("first view");
        let first_text = String::from_utf8(first_bytes).unwrap();
        assert!(
            first_text.contains("\"writer\":\"vm.first\""),
            "{first_text}"
        );

        // Make the next source build take 600 ms (longer than the 400 ms route budget).
        produce.publish_delay_ms.store(600, Ordering::SeqCst);
        let entered = Arc::clone(&produce.publish_entered);
        let before = entered.load(Ordering::SeqCst);
        let scan_task = {
            let produce = Arc::clone(&produce);
            tokio::spawn(async move {
                produce
                    .after_scan(scan_with_thread("vm.second", now + 1000))
                    .await;
            })
        };
        // Barrier: the pass has entered; its injected 600 ms delay (longer than the 400 ms
        // route budget) keeps it from committing while the reads below run.
        wait_until(|| entered.load(Ordering::SeqCst) > before).await;

        // Concurrent reads during the slow producer pass answer from `vm.first` within 400 ms.
        for i in 0..6 {
            let read = tokio::time::timeout(
                Duration::from_millis(400),
                produce.view_async(Some(&limits), now + 100 + i * 5, "UTC", &zone),
            )
            .await
            .expect("read must not hit 400ms deadline")
            .expect("read must not decline");
            let text = String::from_utf8(read.0).unwrap();
            assert!(
                text.contains("\"writer\":\"vm.first\""),
                "expected last good generation: {text}"
            );
        }

        scan_task.await.unwrap();
        produce.publish_delay_ms.store(0, Ordering::SeqCst);

        // Once swapped, reads immediately reflect `vm.second`.
        let (swapped_bytes, _) = produce
            .view_async(Some(&limits), now + 2000, "UTC", &zone)
            .await
            .expect("swapped view");
        let swapped_text = String::from_utf8(swapped_bytes).unwrap();
        assert!(
            swapped_text.contains("\"writer\":\"vm.second\""),
            "{swapped_text}"
        );
    }

    /// Concurrent Status reads beyond `STATUS_PERMITS` (4) share one singleflight compose instead
    /// of filling the pool and declining `busy`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_status_reads_share_one_compose_singleflight() {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let reports = scratch.path().join("reports");
        std::fs::create_dir_all(&ccs).unwrap();
        std::fs::create_dir_all(&reports).unwrap();

        let feeds = Feeds::new();
        let answers = AnswersOwned::new(ccs.clone(), reports);
        let produce = Produce::new(feeds, Arc::clone(&answers), ccs);
        let limits = Limits::new();
        let now = 1_795_000_000_000_u64;
        let zone = utc_zone(now);

        // Warm the generation first so all 12 concurrent reads hit compose together.
        produce.after_scan(scan_with_thread("vm.sf", now)).await;
        produce
            .view_async(Some(&limits), now, "UTC", &zone)
            .await
            .expect("prime zone");
        let before = produce.compose.compose_count();

        // One leader enters the flight first; the barrier proves it is in flight before the
        // other 11 readers start, so all 12 share one compose regardless of machine load.
        // The 1 s injected delay keeps the flight open; the reads carry a hang guard, not a
        // wall-clock budget (the 400 ms route budget during a slow pass is the barrier
        // tests' assertion above).
        produce.compose.set_compose_delay_ms(1000);
        let base_now = now + 5_000;
        let mut tasks = Vec::new();
        let mut spawn_reader = |offset: u64| {
            let produce = Arc::clone(&produce);
            let limits = limits.clone();
            let zone = zone.clone();
            tasks.push(tokio::spawn(async move {
                tokio::time::timeout(
                    Duration::from_secs(5),
                    produce.view_async(Some(&limits), base_now + offset, "UTC", &zone),
                )
                .await
            }));
        };
        spawn_reader(0);
        wait_until(|| produce.compose.compose_count() > before).await;
        for offset in 1..12_u64 {
            spawn_reader(offset);
        }
        for (idx, task) in tasks.into_iter().enumerate() {
            let result = task.await.unwrap();
            match result {
                Ok(Ok((bytes, etag))) => {
                    assert!(!bytes.is_empty(), "reader {idx}");
                    assert!(etag.starts_with("W/\""), "reader {idx}");
                }
                Ok(Err(reason)) => panic!("reader {idx} declined with {reason:?}"),
                Err(_) => panic!("reader {idx} hung"),
            }
        }
        produce.compose.set_compose_delay_ms(0);
        assert_eq!(
            produce.compose.compose_count() - before,
            1,
            "all 12 concurrent reads must share one singleflight compose"
        );
    }

    /// A producer is safe to read from the moment it is constructed: the first poll gets the
    /// honest "waiting for the first scan" view, never a route deadline while a slow first
    /// publish runs. Startup must not depend on a separate warm-up call staying in place.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_new_producer_answers_the_first_poll_with_a_starting_state() {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let reports = scratch.path().join("reports");
        std::fs::create_dir_all(&ccs).unwrap();
        std::fs::create_dir_all(&reports).unwrap();

        let now = now_ms();
        let (tz_name, offsets) =
            crate::zone_facts::status_zone(&ccs, clamp_i64(now)).expect("default zone");
        let zone = ZoneFacts {
            from: offsets.from,
            to: offsets.to,
            offsets: offsets.offsets,
        };
        let produce = Produce::new(Feeds::new(), AnswersOwned::new(ccs.clone(), reports), ccs);
        produce.publish_delay_ms.store(600, Ordering::SeqCst);
        let limits = Limits::new();
        let first = tokio::time::timeout(
            Duration::from_secs(5),
            produce.view_async(Some(&limits), now, &tz_name, &zone),
        )
        .await
        .expect("the first poll must not wait for a slow publish")
        .expect("the first poll must answer");
        let text = String::from_utf8(first.0).unwrap();
        assert!(text.contains("\"complete\":false"), "{text}");
        assert!(
            text.contains("Waiting for the first scan."),
            "the starting view must name the missing scan honestly: {text}"
        );
    }

    /// Real failures are never hidden: invalid zones and unprovable inputs still decline by name.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn real_failures_still_decline_by_name() {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let reports = scratch.path().join("reports");
        std::fs::create_dir_all(&ccs).unwrap();
        std::fs::create_dir_all(&reports).unwrap();

        let produce = Produce::new(Feeds::new(), AnswersOwned::new(ccs.clone(), reports), ccs);
        let limits = Limits::new();
        let now = 1_795_000_000_000_u64;
        let zone = utc_zone(now);

        // Invalid zone declines `locale`.
        let bad_zone = ZoneFacts {
            from: 100,
            to: 50,
            offsets: vec![],
        };
        assert_eq!(
            produce
                .view_async(Some(&limits), now, "UTC", &bad_zone)
                .await
                .unwrap_err(),
            Decline::Locale
        );

        // Start from a valid generation.
        produce.after_scan(scan_with_thread("vm.good", now)).await;
        assert!(
            produce
                .view_async(Some(&limits), now, "UTC", &zone)
                .await
                .is_ok()
        );

        // An unprovable report containing an exact `Object.prototype` member name declines `shape`
        // once published (never silently masked by the earlier good generation).
        let mut bad_scan = scan_with_thread("vm.bad", now + 1000);
        bad_scan.reports.threads[0].doc["summary"] = json!("constructor");
        produce.after_scan(bad_scan).await;
        assert_eq!(
            produce
                .view_async(Some(&limits), now + 1000, "UTC", &zone)
                .await
                .unwrap_err(),
            Decline::Shape
        );

        // Recovering with valid inputs clears the decline.
        produce
            .after_scan(scan_with_thread("vm.recovered", now + 2000))
            .await;
        let (bytes, _) = produce
            .view_async(Some(&limits), now + 2000, "UTC", &zone)
            .await
            .expect("recovered view");
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("\"writer\":\"vm.recovered\"")
        );
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod low_tests {
    use super::flicker_tests::scan_with_thread;
    use super::*;

    async fn wait_for(mut cond: impl FnMut() -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while !cond() {
            assert!(
                tokio::time::Instant::now() < deadline,
                "condition never became true"
            );
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    }

    fn scratch() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let reports = scratch.path().join("reports");
        std::fs::create_dir_all(&ccs).unwrap();
        std::fs::create_dir_all(&reports).unwrap();
        (scratch, ccs, reports)
    }

    /// L1: a panic inside a coalesced publish pass must not wedge `publish_running`. The RAII
    /// guard clears it on unwind, so the next `trigger_publish` still runs a real pass.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publish_running_is_cleared_when_a_publish_panics() {
        let (_scratch, ccs, reports) = scratch();
        let produce = Produce::new(Feeds::new(), AnswersOwned::new(ccs.clone(), reports), ccs);
        let now = 1_795_000_000_000_u64;
        produce.after_scan(scan_with_thread("vm.l1", now)).await;
        assert!(produce.current_published().is_some(), "seed generation");

        // Arm a panic inside the next coalesced publish pass.
        produce.publish_panic_once.store(true, Ordering::SeqCst);
        let entered = Arc::clone(&produce.publish_entered);
        let before = entered.load(Ordering::SeqCst);
        produce.mark_answers();
        wait_for(|| entered.load(Ordering::SeqCst) > before).await;
        // Wait for the panicking pass to unwind (the RAII guard clears publish_running)
        // before the next trigger; a stuck flag would time out here, not pass silently.
        wait_for(|| !produce.publish_running.load(Ordering::SeqCst)).await;

        // The next coalesced publish must still run, not be a no-op behind a stuck flag.
        let before_next = entered.load(Ordering::SeqCst);
        produce.mark_answers();
        let ran = tokio::time::timeout(Duration::from_secs(3), async {
            wait_for(|| entered.load(Ordering::SeqCst) > before_next).await;
        })
        .await;
        assert!(
            ran.is_ok(),
            "publish_running stayed set after a publish panic; later publishes are no-ops"
        );
        wait_for(|| !produce.publish_running.load(Ordering::SeqCst)).await;
    }

    /// L2: the 5 s producer job must not run the synchronous `default_request_zone` (preferences
    /// read + 520-day zone walk) on a Tokio worker every tick; it re-walks only when the UTC day
    /// advances, on the blocking pool.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn jobs_zone_walk_only_once_per_utc_day_off_the_workers() {
        let (_scratch, ccs, reports) = scratch();
        let produce = Produce::new(Feeds::new(), AnswersOwned::new(ccs.clone(), reports), ccs);
        let checks = Arc::clone(&produce.zone_checks);
        // Capture this runtime's worker thread ids. Each probe records its worker's id, then
        // blocks that worker on a std barrier; with both workers blocked at once, the two ids
        // are necessarily distinct. (The blocking pool shares the worker thread *name*, so only
        // the id tells a blocking-pool thread apart from a Tokio worker.)
        let worker_ids = Arc::new(Mutex::new(std::collections::HashSet::new()));
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let mut probes = Vec::new();
        for _ in 0..2 {
            let ids = Arc::clone(&worker_ids);
            let barrier = Arc::clone(&barrier);
            probes.push(tokio::spawn(async move {
                ids.lock().unwrap().insert(std::thread::current().id());
                barrier.wait();
            }));
        }
        for probe in probes {
            probe.await.unwrap();
        }
        let worker_ids = worker_ids.lock().unwrap().clone();
        assert_eq!(worker_ids.len(), 2, "expected both runtime workers");
        // A fixed clock inside one UTC day: three ticks, no day change, so one walk.
        produce
            .jobs_now_override
            .store(1_795_000_000_000, Ordering::SeqCst);
        let stop = Arc::new(AtomicBool::new(false));
        let jobs = tokio::spawn(Produce::jobs(Arc::clone(&produce), Arc::clone(&stop)));
        // Let several 5 s ticks pass. The base walks the zone on a Tokio worker every tick
        // (>= 2); the fix walks once (the UTC day does not advance) and off the workers.
        tokio::time::sleep(Duration::from_millis(11_500)).await;
        stop.store(true, Ordering::Release);
        jobs.await.unwrap();
        assert_eq!(
            checks.load(Ordering::SeqCst),
            1,
            "the zone walk must run once per UTC day, not on every 5 s Tokio-worker tick"
        );
        // The one walk must have run on the blocking pool, never on a Tokio worker.
        let threads = produce.zone_walk_threads.lock().unwrap().clone();
        assert_eq!(threads.len(), 1, "exactly one zone walk: {threads:?}");
        assert!(
            !worker_ids.contains(&threads[0]),
            "the zone walk ran on a Tokio worker: {threads:?} (workers {worker_ids:?})"
        );
    }

    /// L4: an external PUT that outruns Produce's commit must not leave `PublisherCache::held`
    /// pairing Produce's text with the external generation. The text and generation stay paired.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn publisher_cache_pairs_text_with_the_committed_generation() {
        let (_scratch, ccs, reports) = scratch();
        let feeds = Feeds::new();
        let produce = Produce::new(feeds.clone(), AnswersOwned::new(ccs.clone(), reports), ccs);
        let now = 1_795_000_000_000_u64;
        produce.after_scan(scan_with_thread("vm.l4", now)).await;
        let seed_data = feeds.latest(REPORTS).unwrap().data.get().to_owned();

        // Pause the next publish just before it commits so an external PUT lands a higher
        // generation first (the PUT outruns Produce's commit).
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (resume_tx, resume_rx) = std::sync::mpsc::channel();
        *produce.before_commit.lock().unwrap() = Some((ready_tx, resume_rx));
        let scan_task = {
            let produce = Arc::clone(&produce);
            tokio::spawn(async move {
                produce
                    .after_scan(scan_with_thread("vm.l4b", now + 1000))
                    .await;
            })
        };
        ready_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let external = format!(
            "{{\"generation\":\"9000000000000000000\",\"data\":{seed_data},\"facts\":{{}}}}"
        );
        feeds.put(REPORTS, external.as_bytes()).unwrap();
        resume_tx.send(()).unwrap();
        scan_task.await.unwrap();

        let held_gen = feeds.latest(REPORTS).unwrap();
        assert_eq!(
            held_gen.id, "9000000000000000000",
            "the external generation stays"
        );
        let cache = produce.publisher.lock().unwrap_or_else(|p| p.into_inner());
        let (text_hash, _facts_hash, arc) = cache.held.get(REPORTS).unwrap();
        assert!(
            Arc::ptr_eq(arc, &held_gen),
            "cache holds the committed generation"
        );
        let expected_hash: [u8; 32] = sha2::Sha256::digest(held_gen.data.get().as_bytes()).into();
        assert_eq!(
            *text_hash, expected_hash,
            "L4: cache digest must match the committed generation's data, not Produce's text"
        );
    }

    /// L3: the owned singleflight window must be one-sided. A follower whose `x-aac-now` is in
    /// the leader's past may not reuse the leader's bytes: their `generatedAt` would be in the
    /// follower's future.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_past_follower_never_receives_a_future_generated_at() {
        let (_scratch, ccs, reports) = scratch();
        let feeds = Feeds::new();
        let produce = Produce::new(feeds.clone(), AnswersOwned::new(ccs.clone(), reports), ccs);
        let now = 1_795_000_000_000_u64;
        produce.after_scan(scan_with_thread("vm.l3", now)).await;
        let gens = match produce.current_published() {
            Some(PublishedState::Ready(published)) => published.generations.clone(),
            _ => panic!("expected a published generation"),
        };
        // A wide UTC window proves the words of both instants (the same minute, one flight).
        let now_i64 = clamp_i64(now);
        let span = 30 * 86_400_000;
        let zone =
            Zone::proven(now_i64 - span, now_i64 + span, &[[now_i64 - span, 0]]).expect("utc zone");
        // The leader composes at the staleness edge's far side; the follower asks 600 ms in the
        // leader's past (still inside the old two-sided 1 s window).
        let leader_now = clamp_i64(now + 60_500);
        let follower_now = clamp_i64(now + 59_900);
        let words = |ms: i64| zone.words(ms).expect("zone words");
        let (leader_clock, leader_day) = words(leader_now);
        let (follower_clock, follower_day) = words(follower_now);
        assert_eq!(
            (leader_clock.clone(), leader_day.clone()),
            (follower_clock.clone(), follower_day.clone()),
            "both instants must share one (clock, day) so they share a flight"
        );
        // Make the leader's compose slow enough for the follower to join its flight.
        produce.compose.set_compose_delay_ms(150);
        let compose = produce.compose.clone();
        let compose_before = compose.compose_count();
        let leader_gens = gens.clone();
        let leader = tokio::spawn(async move {
            compose
                .answer_generations_async(
                    None,
                    leader_gens,
                    leader_now,
                    leader_clock,
                    leader_day,
                    true,
                )
                .await
        });
        // Barrier: the leader's compose has started (inside its 150 ms injected delay), so
        // the follower joins the same flight (same six ids, same clock/day) instead of racing it.
        wait_for(|| produce.compose.compose_count() > compose_before).await;
        let follower = produce
            .compose
            .answer_generations_async(None, gens, follower_now, follower_clock, follower_day, true)
            .await
            .expect("follower read");
        let _ = leader.await.unwrap();
        let follower_iso = aac_num::ecma::to_iso_string(follower_now).expect("follower iso");
        let leader_iso = aac_num::ecma::to_iso_string(leader_now).expect("leader iso");
        let text = String::from_utf8(follower.0).expect("utf8");
        assert!(
            text.contains(&format!("\"generatedAt\":\"{follower_iso}\"")),
            "L3: a past follower must get its own generatedAt, not the leader's future instant ({leader_iso}): {text}"
        );
    }

    /// The producer holds the one shared copy of each scan input: the very Arcs the scan handed
    /// it, not a second wrapped or parsed copy of the same quota/registry JSON.
    #[tokio::test]
    async fn after_scan_shared_holds_the_one_shared_copy() {
        let (_scratch, ccs, reports) = scratch();
        let produce = Produce::new(Feeds::new(), AnswersOwned::new(ccs.clone(), reports), ccs);
        let reports_rows = Arc::new(empty_reports());
        let docs_rows = Arc::new(StatusDocsState {
            state: "missing",
            scanned_at: None,
            docs: Vec::new(),
            problems: Vec::new(),
        });
        let quota = Arc::new(json!({"state": "ok"}));
        let registry = Arc::new(json!({"data": null, "asOf": null, "error": null}));
        produce
            .after_scan_shared(
                Arc::clone(&reports_rows),
                Arc::clone(&docs_rows),
                Arc::clone(&quota),
                Arc::clone(&registry),
                false,
            )
            .await;
        let state = produce.state();
        assert!(Arc::ptr_eq(&state.reports_rows, &reports_rows));
        assert!(Arc::ptr_eq(&state.docs_rows, &docs_rows));
        assert!(Arc::ptr_eq(&state.quota, &quota));
        assert!(Arc::ptr_eq(&state.registry, &registry));
    }

    /// A failed compose propagates its named decline to every follower sharing the flight,
    /// and the failed flight is cleared: the same inputs compose again, and a healthy
    /// generation set recovers at once. Blind-check coverage for StatusCompose failure
    /// propagation (flicker-T1): a leader that served bytes despite a failed compose, or a
    /// flight that stayed in the map after failing, fails this test.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_failed_compose_propagates_to_every_follower_and_clears() {
        let (_scratch, ccs, reports) = scratch();
        let feeds = Feeds::new();
        let produce = Produce::new(feeds.clone(), AnswersOwned::new(ccs.clone(), reports), ccs);
        let now = 1_795_000_000_000_u64;
        produce.after_scan(scan_with_thread("vm.prop", now)).await;
        let gens = match produce.current_published() {
            Some(PublishedState::Ready(published)) => published.generations.clone(),
            _ => panic!("expected a published generation"),
        };
        let now_i64 = clamp_i64(now);
        let span = 30 * 86_400_000;
        let zone =
            Zone::proven(now_i64 - span, now_i64 + span, &[[now_i64 - span, 0]]).expect("utc zone");
        let (clock, day) = zone.words(now_i64).expect("zone words");

        // A facts generation whose presentation proves nothing: it prepares and passes
        // validate_prepared, but the compose cannot build the view from it.
        let bad_facts = feeds
            .prepare_generation(
                FACTS,
                br#"{"generation":"9000000000000000001","data":{"timeZone":"UTC"},"facts":{"dates":{},"epochs":{},"order":{}}}"#,
            )
            .expect("bad facts prepare");
        let mut bad = gens.clone();
        bad[5] = bad_facts;

        // Slow the compose so followers join the leader's flight instead of racing it.
        produce.compose.set_compose_delay_ms(300);
        let compose_before = produce.compose.compose_count();
        let compose = produce.compose.clone();
        let bad_leader = bad.clone();
        let leader_clock = clock.clone();
        let leader_day = day.clone();
        let leader = tokio::spawn(async move {
            compose
                .answer_generations_async(None, bad_leader, now_i64, leader_clock, leader_day, true)
                .await
        });
        wait_for(|| produce.compose.compose_count() > compose_before).await;
        let mut followers = Vec::new();
        for _ in 0..5 {
            let compose = produce.compose.clone();
            let bad = bad.clone();
            let clock = clock.clone();
            let day = day.clone();
            followers.push(tokio::spawn(async move {
                compose
                    .answer_generations_async(None, bad, now_i64, clock, day, true)
                    .await
            }));
        }
        let lead = leader.await.unwrap();
        assert!(
            lead.is_err(),
            "a failed compose must decline by name, not serve bytes: {lead:?}"
        );
        for follower in followers {
            assert_eq!(
                follower.await.unwrap(),
                lead,
                "every follower must receive the leader's decline"
            );
        }

        // The failed flight is gone: the same bad set composes again (and declines again),
        // never joining a dead flight, and a healthy set recovers.
        produce.compose.set_compose_delay_ms(0);
        let again = produce
            .compose
            .answer_generations_async(None, bad.clone(), now_i64, clock.clone(), day.clone(), true)
            .await;
        assert_eq!(again, lead, "the same inputs must decline the same way");
        assert_eq!(
            produce.compose.compose_count() - compose_before,
            2,
            "the failed flight must not stay in the inflight map"
        );
        let (bytes, _) = produce
            .compose
            .answer_generations_async(None, gens, now_i64, clock, day, true)
            .await
            .expect("a healthy generation set recovers");
        assert!(!bytes.is_empty());
    }
}
