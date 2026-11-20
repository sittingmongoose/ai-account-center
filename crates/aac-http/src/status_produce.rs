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
use crate::feed::Feeds;
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
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
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
const SOURCES: [&str; 6] = [REPORTS, QUOTA, PINNED, COLLECTED, ANSWERS, FACTS];

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

/// The display zone a request named, kept between requests.
#[derive(Clone)]
struct RequestZone {
    name: String,
    zone: Zone,
    offset_now: i64,
}

struct Inner {
    reports: Json,
    quota: Json,
    docs: Json,
    registry: Json,
    reports_rows: AgentReportsState,
    docs_rows: StatusDocsState,
    machines: BTreeMap<String, Collected<Sample>>,
    browsers: Collected<Vec<BrowserMachine>>,
    simlock: Collected<SimlockStatus>,
    simlock_workers: Option<Vec<&'static str>>,
    usage: Option<UsageInput>,
    answers: AnswersFeed,
    pins: Pins,
    pinned: Json,
    pinned_at: u64,
    pinned_offset: Option<i64>,
    pinned_dirty: bool,
    answers_dirty: bool,
    sources_dirty: bool,
    zone: Option<RequestZone>,
    held: HashMap<&'static str, String>,
    held_sources: HashMap<&'static str, (String, String)>,
    last_collect: u64,
    last_demand: u64,
    collecting: bool,
    hinted_at: u64,
}

impl Inner {
    fn new() -> Self {
        let empty = empty_reports();
        Self {
            reports: aac_status::share::reports::state_json(&empty),
            reports_rows: empty,
            quota: Json::Null,
            docs: Json::Null,
            registry: json!({"data": null, "asOf": null, "error": null}),
            docs_rows: StatusDocsState {
                state: "missing",
                scanned_at: None,
                docs: Vec::new(),
                problems: Vec::new(),
            },
            machines: aac_status::computers::machine_ids()
                .into_iter()
                .map(|machine| (machine.to_owned(), Collected::empty()))
                .collect(),
            browsers: Collected::empty(),
            simlock: Collected::empty(),
            simlock_workers: None,
            usage: None,
            answers: AnswersFeed {
                answers: Vec::new(),
                sends: Vec::new(),
                problem: None,
            },
            pins: Pins::default(),
            pinned: json!({"unavailable": "the pinned threads have not been read yet"}),
            pinned_at: 0,
            pinned_offset: None,
            pinned_dirty: true,
            answers_dirty: true,
            sources_dirty: true,
            zone: None,
            held: HashMap::new(),
            held_sources: HashMap::new(),
            last_collect: 0,
            last_demand: 0,
            collecting: false,
            hinted_at: 0,
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

pub(crate) struct Produce {
    feeds: Feeds,
    answers: Arc<AnswersOwned>,
    inner: Mutex<Inner>,
    clock: AtomicU64,
}

impl Produce {
    pub(crate) fn new(feeds: Feeds, answers: Arc<AnswersOwned>) -> Arc<Self> {
        Arc::new(Self {
            feeds,
            answers,
            inner: Mutex::new(Inner::new()),
            clock: AtomicU64::new(0),
        })
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
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

    /// A page read: demand for the process-starting collectors.
    pub(crate) fn touch(&self) {
        self.lock().last_demand = now_ms();
    }

    /// A new usage sample (`recordUsage`'s built input) or none.
    pub(crate) fn set_usage(&self, usage: Option<UsageInput>) {
        let mut inner = self.lock();
        inner.usage = usage;
        inner.sources_dirty = true;
    }

    /// The answer store recorded something (a forwarded send): the history member may change.
    pub(crate) fn mark_answers(&self) {
        let mut inner = self.lock();
        inner.answers_dirty = true;
        inner.pinned_dirty = true;
    }

    /// One producer pass every 5 s: collect when a page has been read recently and the minute
    /// is up (the TypeScript 60 s timer's rule).
    pub(crate) async fn jobs(self: Arc<Self>, stop: Arc<std::sync::atomic::AtomicBool>) {
        loop {
            if stop.load(Ordering::Acquire) {
                return;
            }
            let now = now_ms();
            let wanted = {
                let inner = self.lock();
                now.saturating_sub(inner.last_demand) < DEMAND_WINDOW_MS
                    && now.saturating_sub(inner.last_collect) >= COLLECT_EVERY_MS
                    && !inner.collecting
            };
            if wanted {
                let owner = Arc::clone(&self);
                tokio::spawn(async move { owner.collect().await });
            }
            tokio::time::sleep(Duration::from_millis(5_000)).await;
        }
    }

    /// Start a collect now when the page has been read recently and nothing is running.
    fn maybe_collect(self: &Arc<Self>) {
        let now = now_ms();
        let start = {
            let mut inner = self.lock();
            let wanted = now.saturating_sub(inner.last_demand) < DEMAND_WINDOW_MS;
            let due = now.saturating_sub(inner.last_collect) >= COLLECT_EVERY_MS;
            if wanted && due && !inner.collecting {
                inner.collecting = true;
                inner.last_collect = now;
                true
            } else {
                false
            }
        };
        if start {
            let owner = Arc::clone(self);
            tokio::spawn(async move { owner.collect_running().await });
        }
    }

    /// One collect pass; overlapping calls share the running one.
    pub(crate) async fn collect(self: &Arc<Self>) {
        {
            let mut inner = self.lock();
            if inner.collecting {
                return;
            }
            inner.collecting = true;
            inner.last_collect = now_ms();
        }
        self.collect_running().await;
    }

    async fn collect_running(self: &Arc<Self>) {
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
        let results: Vec<(String, Result<Sample, String>)> =
            futures_util::future::join_all(pending).await;
        let simlock = find_executable("simlock");
        let simlock_result = read_simlock(&simlock).await;
        let fresh_workers = simlock_result
            .as_ref()
            .ok()
            .and_then(|(_, _, workers)| workers.clone());
        let workers = fresh_workers
            .clone()
            .or_else(|| self.lock().simlock_workers.clone());
        let browsers_result = read_browsers(workers.as_deref()).await;
        let previous_pins = self.lock().pins.machines.clone();
        let pins = read_pins(&rows, local, previous_pins).await;
        let now_i64 = clamp_i64(now);
        {
            let mut inner = self.lock();
            for (machine, result) in results {
                let before = inner
                    .machines
                    .get(&machine)
                    .cloned()
                    .unwrap_or_else(Collected::empty);
                inner
                    .machines
                    .insert(machine, apply_machine(before, result, now_i64));
            }
            match browsers_result {
                Ok(rows) => {
                    let merged =
                        browsers::merge_rows(inner.browsers.data.as_deref(), rows, now_i64);
                    let problems = browsers::problems(&merged);
                    inner.browsers = Collected::succeeded(merged, now_i64).with_problems(problems);
                }
                Err((reason, missing)) => {
                    let before = inner.browsers.clone();
                    inner.browsers = before.failed(&reason, now_i64, missing);
                }
            }
            match simlock_result {
                Ok((status, worker_only, workers)) => {
                    if let Some(workers) = workers {
                        inner.simlock_workers = Some(workers);
                    }
                    let problems = simlock_problems(
                        &status,
                        worker_only,
                        inner.simlock.problems.as_deref(),
                        now_i64,
                    );
                    inner.simlock = Collected::succeeded(status, now_i64).with_problems(problems);
                }
                Err((reason, missing)) => {
                    let before = inner.simlock.clone();
                    inner.simlock = before.failed(&reason, now_i64, missing);
                }
            }
            inner.pins = pins;
            inner.pinned_dirty = true;
            inner.sources_dirty = true;
            inner.collecting = false;
            drop(inner);
        }
        let owner = Arc::clone(self);
        tokio::task::spawn_blocking(move || owner.publish(None))
            .await
            .ok();
        self.hint();
    }

    /// The 10 s report scan's snapshots: shared answers into the store, then the sources.
    pub(crate) async fn after_scan(self: &Arc<Self>, scan: Scan) {
        let cards: Vec<(Json, String)> = scan
            .reports
            .cards
            .iter()
            .map(|row| (row.doc.clone(), row.sha256.clone()))
            .collect();
        let answers = Arc::clone(&self.answers);
        let feed = tokio::task::spawn_blocking(move || answers.feed(&cards))
            .await
            .unwrap_or_else(|_| AnswersFeed::unavailable(Vec::new()));
        {
            let mut inner = self.lock();
            inner.reports_rows = scan.reports.clone();
            inner.docs_rows = scan.docs.clone();
            inner.reports = resolved_reports(&scan.reports, &feed);
            inner.quota = scan.quota;
            inner.docs = aac_status::share::status_docs::state_json(&scan.docs);
            inner.registry = scan.registry;
            inner.answers = feed;
            inner.answers_dirty = false;
            inner.pinned_dirty = true;
            inner.sources_dirty = true;
        }
        let owner = Arc::clone(self);
        tokio::task::spawn_blocking(move || owner.publish(None))
            .await
            .ok();
        self.hint();
    }

    /// The answer store's shared copies for these cards, or the fixed unavailable answer.
    fn refresh_answers(&self, inner: &mut Inner) {
        let cards: Vec<(Json, String)> = inner
            .reports_rows
            .cards
            .iter()
            .map(|row| (row.doc.clone(), row.sha256.clone()))
            .collect();
        inner.answers = self.answers.feed(&cards);
        inner.answers_dirty = false;
        inner.sources_dirty = true;
    }

    /// The pins the readers-check writer matches reports against (`owned-pins`).
    fn push_owned_pins(&self, inner: &Inner, now: u64) {
        let pins: Vec<Json> = inner
            .pins
            .merged
            .iter()
            .map(|pin| json!({"id": pin.id, "title": pin.title}))
            .collect();
        let generation = self.next_generation(now);
        let data = aac_views::js_json::write(&Json::Array(pins));
        let body = format!(
            "{{\"generation\":\"{generation}\",\"data\":{},\"facts\":null}}",
            String::from_utf8_lossy(&data)
        );
        let _ = self.feeds.put(OWNED_PINS, body.as_bytes());
    }

    /// Rebuild every source the current state makes, push what changed, and compose the panel
    /// when a zone is known. Called on the blocking pool or with no await inside.
    fn publish(&self, zone_override: Option<(&str, &ZoneFacts, &Zone)>) {
        let mut inner = self.lock();
        if inner.answers_dirty {
            self.refresh_answers(&mut inner);
        }
        let zone = match zone_override {
            Some((name, _facts, zone)) => {
                if inner
                    .zone
                    .as_ref()
                    .is_none_or(|held| held.name != name || held.zone != *zone)
                {
                    inner.zone = Some(RequestZone {
                        name: name.to_owned(),
                        zone: zone.clone(),
                        offset_now: zone.offset_minutes(clamp_i64(now_ms())).unwrap_or(0),
                    });
                    inner.pinned_dirty = true;
                    inner.sources_dirty = true;
                }
                inner.zone.clone()
            }
            None => inner.zone.clone(),
        };
        let now = now_ms();
        if zone.is_some() && inner.pinned_dirty {
            let offset = zone
                .as_ref()
                .map(|zone| zone.zone.offset_minutes(clamp_i64(now)).unwrap_or(0))
                .unwrap_or(0);
            if let Some(panel) = pinned_panel(&inner, clamp_i64(now)) {
                inner.pinned = panel;
                inner.pinned_at = now;
                inner.pinned_offset = Some(offset);
                inner.pinned_dirty = false;
                self.push_owned_pins(&inner, now);
            }
        }
        if inner.sources_dirty {
            self.push_sources(&mut inner, zone.as_ref(), now);
            inner.sources_dirty = false;
        }
    }

    /// Build and push the six fed generations (facts included) from the state and the zone.
    fn push_sources(&self, inner: &mut Inner, zone: Option<&RequestZone>, now: u64) {
        let Some(zone) = zone.filter(|_| inner.reports.as_object().is_some()) else {
            return;
        };
        let machines = machines_json(&inner.machines);
        let sources =
            aac_status::produce::sources::build(&aac_status::produce::sources::Readings {
                reports: &inner.reports,
                quota: &inner.quota,
                pinned: &inner.pinned,
                registry: &inner.registry,
                machines: &machines,
                browsers: &json!(inner.browsers),
                simlock: &json!(inner.simlock),
                usage: inner.usage.as_ref(),
                answers: &inner.answers,
                time_zone: &zone.name,
                now: clamp_i64(now),
                offset_minutes: zone.offset_now,
                zone: &zone.zone,
            });
        let Ok(sources) = sources else {
            return;
        };
        let pushed = [
            (REPORTS, sources.reports, None),
            (QUOTA, sources.quota, None),
            (PINNED, sources.pinned, None),
            (COLLECTED, sources.collected, None),
            (ANSWERS, sources.answers, None),
            (FACTS, sources.facts_data, Some(sources.facts)),
        ];
        for (source, text, facts) in pushed {
            let facts_text = facts.unwrap_or_else(|| "{}".to_owned());
            let unchanged = inner
                .held_sources
                .get(source)
                .is_some_and(|(held, held_facts)| held == &text && held_facts == &facts_text);
            if unchanged {
                continue;
            }
            let generation = self.next_generation(now);
            let body = format!(
                "{{\"generation\":\"{generation}\",\"data\":{text},\"facts\":{facts_text}}}"
            );
            if let Ok(value) = self.feeds.put(source, body.as_bytes()) {
                if let Some(held) = value.get("generation").and_then(Json::as_str) {
                    inner.held.insert(source, held.to_owned());
                }
                inner.held_sources.insert(source, (text, facts_text));
            }
        }
    }

    /// The request path: demand, a collect if due, the sources at this zone, and the feed header.
    pub(crate) fn prepare(
        self: &Arc<Self>,
        now: u64,
        time_zone: &str,
        zone_facts: &ZoneFacts,
    ) -> Result<String, Decline> {
        self.touch();
        self.maybe_collect();
        let zone = zone_facts.zone().ok_or(Decline::Locale)?;
        let mut inner = self.lock();
        if inner.answers_dirty {
            self.refresh_answers(&mut inner);
        }
        let offset_changed = inner
            .zone
            .as_ref()
            .is_none_or(|held| held.name != time_zone || held.zone != zone);
        if offset_changed {
            inner.zone = Some(RequestZone {
                name: time_zone.to_owned(),
                zone: zone.clone(),
                offset_now: zone.offset_minutes(clamp_i64(now)).unwrap_or(0),
            });
            inner.pinned_dirty = true;
            inner.sources_dirty = true;
        }
        if inner.pinned_dirty
            || inner.zone.as_ref().map(|zone| zone.offset_now) != inner.pinned_offset
        {
            let offset = zone.offset_minutes(clamp_i64(now)).unwrap_or(0);
            if let Some(panel) = pinned_panel(&inner, clamp_i64(now)) {
                inner.pinned = panel;
                inner.pinned_at = now;
                inner.pinned_offset = Some(offset);
                inner.pinned_dirty = false;
                inner.sources_dirty = true;
                self.push_owned_pins(&inner, now);
            }
        }
        if inner.sources_dirty
            || inner
                .zone
                .as_ref()
                .is_some_and(|held| held.name != time_zone || held.zone != zone)
        {
            let zone = inner.zone.clone();
            self.push_sources(&mut inner, zone.as_ref(), now);
            inner.sources_dirty = false;
        }
        let mut parts = Vec::with_capacity(SOURCES.len());
        for source in SOURCES {
            parts.push(
                inner
                    .held
                    .get(source)
                    .cloned()
                    .ok_or(Decline::Unsupported)?,
            );
        }
        let now_words = zone.words(clamp_i64(now)).ok_or(Decline::Locale)?;
        let header = json!({
            "now": clamp_i64(now),
            "clock": now_words.0,
            "day": now_words.1,
            "reports": parts[0],
            "quota": parts[1],
            "pinned": parts[2],
            "collected": parts[3],
            "answers": parts[4],
            "facts": parts[5],
        });
        Ok(header.to_string())
    }

    /// One owned Status request: the composed view's bytes and its ETag.
    pub(crate) fn view(
        self: &Arc<Self>,
        now: u64,
        time_zone: &str,
        zone_facts: &ZoneFacts,
    ) -> Result<(Vec<u8>, String), Decline> {
        let header = self.prepare(now, time_zone, zone_facts)?;
        crate::status_fed::answer(&self.feeds, &header)
    }

    /// `GET /api/status/answers`: the page's answer history from the owned feed (newest first,
    /// at most `limit`). A dirty feed is scanned here first, as a Status read would.
    pub(crate) fn answers_view(&self, limit: usize) -> Json {
        let mut inner = self.lock();
        if inner.answers_dirty {
            self.refresh_answers(&mut inner);
        }
        crate::status_answers_route::history_json(
            &inner.answers.answers,
            &inner.answers.sends,
            limit,
        )
    }

    /// The producers' hint: one `status-changed` per window, once anything changed.
    fn hint(&self) {
        let now = now_ms();
        let send = {
            let mut inner = self.lock();
            if now.saturating_sub(inner.hinted_at) < HINT_GAP_MS {
                false
            } else {
                inner.hinted_at = now;
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

/// `statfs('/')`: `(free bytes, total bytes)` as Node's `bavail*bsize` and `blocks*bsize`,
/// from `df -Pk /` (the same 1 KiB blocks and the same filesystem the probe reads).
#[cfg(not(windows))]
fn statfs_root(_path: &str) -> Option<(f64, f64)> {
    let output = std::process::Command::new("df")
        .args(["-Pk", "/"])
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
        statfs: &|_| {
            let root = [u16::from(b'\\'), 0];
            let (mut free, mut total) = (0u64, 0u64);
            // SAFETY: root is NUL-terminated and the two requested outputs are live u64s.
            let ok = unsafe {
                GetDiskFreeSpaceExW(root.as_ptr(), &mut free, &mut total, std::ptr::null_mut())
            };
            (ok != 0).then_some((free as f64, total as f64))
        },
        mac_memory: None,
    })
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
                statfs: &statfs_root,
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
            statfs: &statfs_root,
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
                local_pins()
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
fn pinned_panel(inner: &Inner, now: i64) -> Option<Json> {
    // Before the first collect there are no pins to compose over; the panel says it has not
    // been read yet, as the collector's view did.
    if !inner.pins.collected {
        return None;
    }
    let closed = closing_card_versions(&inner.answers.answers);
    let cards: Vec<aac_status::produce::answered::CardRow<'_>> = inner
        .reports_rows
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
        &inner.reports_rows,
        &inner.docs_rows,
        &inner.pins.merged,
        &inner.pins.problems,
        now,
        &closed,
        &waits,
    )?;
    let output = aac_status::pinned::compose(&input);
    serde_json::to_value(output).ok()
}
