//! The publish loop (`createSnapshotPublisher` in `agent-usage-snapshot.ts`): one write at a
//! time, off until a folder is set, nothing to publish without accounts. A stalled write holds
//! the publisher and is reported once; later samples still count toward the short-span pace.
//! The sign-in fetch waits at most 60 seconds inside a publish slot; a rejected or timed-out
//! fetch publishes a named collection-failed row. `usage.json` is the commit point of each
//! pass, and the sign-in deadline recheck converges both files before it lands.

use crate::history::RecentReadings;
use crate::markdown::render_snapshot_markdown;
use crate::settings::{SnapshotSettings, render_snapshot_locations};
use crate::snapshot::{
    SampleEvent, build_snapshot_files, downgrade_crossed_sign_in_rows,
    sign_in_collection_failed_row,
};
use crate::{SNAPSHOT_HISTORY, SNAPSHOT_JSON, SNAPSHOT_LOCATIONS, SNAPSHOT_MARKDOWN};
use serde_json::Value;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, TryLockError};
use std::time::Duration;

/// A write still running after this long is reported once in the dashboard log.
const STALL_LOG_MS: i64 = 5 * 60_000;
/// The sign-in fetch waits at most this long inside a publish slot.
const SIGNIN_FETCH_LIMIT_MS: u64 = 60_000;

type SerialWrite = dyn Fn(&str, &str, &str) -> io::Result<()> + Send + Sync;
type DirectoryCheck = dyn Fn(&str) -> io::Result<bool> + Send + Sync;
type ClientSync = dyn Fn(&str) -> io::Result<()> + Send + Sync;
type RollupWrite = dyn Fn(&SampleEvent, &str, &Arc<SerialWrite>) -> Result<(), ()> + Send + Sync;
type LogWarning = dyn Fn(&str, &str) + Send + Sync;

/// The publisher's injectable effects, the TypeScript `SnapshotPublisherDeps` one for one.
/// Every closure is synchronous; the sign-in fetch runs on its own thread under the 60-second
/// limit, exactly as the TS promise race releases a fetch that never returns.
#[derive(Clone)]
pub struct Deps {
    /// The saved setting, or None while no folder is configured.
    pub settings: Arc<dyn Fn() -> Option<SnapshotSettings> + Send + Sync>,
    /// The atomic share write.
    pub write: Arc<SerialWrite>,
    /// Whether the configured folder is a folder (an error fails the pass).
    pub is_directory: Arc<DirectoryCheck>,
    /// The dashboard's refresh interval in seconds.
    pub refresh_seconds: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// Keeps the reader and runner files current in the folder.
    pub sync_client: Arc<ClientSync>,
    /// The sign-in expiry rows; Err is a collection failure, not an empty result.
    pub sign_in: Arc<dyn Fn() -> Result<Vec<Value>, ()> + Send + Sync>,
    /// The renewal runner's summary, best effort.
    pub sign_in_renewal: Arc<dyn Fn() -> Option<Value> + Send + Sync>,
    /// Write the store's hourly and daily rollup files for one sample, streaming each file's
    /// JSON text straight to the share writer (no full `Value` tree is held). Called only when
    /// the hourly write is due. Ok(()) wrote both (or there is no store); Err retries next sample.
    pub rollup_write: Arc<RollupWrite>,
    /// The wall clock (`Date.now`).
    pub now_ms: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// The machine name a failed collection row names (`os.hostname`).
    pub hostname: Arc<dyn Fn() -> String + Send + Sync>,
    /// The dashboard log's warn lines: `write_stalled` and `write_failed`.
    pub log: Arc<LogWarning>,
}

/// The per-folder caches the pass keeps between samples. The service holds the guard opaquely
/// between [`SnapshotPublisher::try_begin`] and [`SnapshotPublisher::pass`].
#[derive(Default)]
pub struct Inner {
    rollups_written: Option<String>,
    warned: Option<String>,
    client_synced: Option<String>,
    locations_written: Option<String>,
}

/// The snapshot publisher: one per dashboard boot, fed every analytics sample.
pub struct SnapshotPublisher {
    deps: Deps,
    recent: Mutex<RecentReadings>,
    inner: Mutex<Inner>,
    busy_since: AtomicI64,
    stall_logged: AtomicBool,
}

impl SnapshotPublisher {
    /// A publisher with the given effects.
    pub fn new(deps: Deps) -> Self {
        Self {
            deps,
            recent: Mutex::new(RecentReadings::new()),
            inner: Mutex::new(Inner::default()),
            busy_since: AtomicI64::new(0),
            stall_logged: AtomicBool::new(false),
        }
    }

    /// Every sample's readings count toward short-span pace, even one whose write is skipped.
    pub fn note(&self, event: &SampleEvent) {
        let mut recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        recent.add(&event.accounts, event.sampled_at);
    }

    /// The busy guard: claim the publish slot, or report a stalled write once and skip.
    pub fn try_begin(&self) -> Option<MutexGuard<'_, Inner>> {
        let guard = match self.inner.try_lock() {
            Ok(guard) => guard,
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner(),
            Err(TryLockError::WouldBlock) => {
                self.note_stall();
                return None;
            }
        };
        self.busy_since
            .store((self.deps.now_ms)(), Ordering::Release);
        self.stall_logged.store(false, Ordering::Release);
        Some(guard)
    }

    /// A skipped sample's stall check: a write that never returns is reported once.
    pub fn note_stall(&self) {
        let busy_since = self.busy_since.load(Ordering::Acquire);
        if busy_since != 0
            && (self.deps.now_ms)() - busy_since > STALL_LOG_MS
            && !self.stall_logged.swap(true, Ordering::AcqRel)
        {
            (self.deps.log)("agent_usage_snapshot.write_stalled", "");
        }
    }

    /// One full pass over a claimed slot; releases the busy flag when it ends.
    pub fn pass(&self, inner: &mut MutexGuard<'_, Inner>, event: &SampleEvent) -> bool {
        self.pass_since(inner, event, None)
    }

    /// One full pass whose sign-in fetch started at `sign_in_since` (the front door's clock
    /// when it pre-fetched the rows); None reads the start here, as the in-process writer does.
    pub fn pass_since(
        &self,
        inner: &mut MutexGuard<'_, Inner>,
        event: &SampleEvent,
        sign_in_since: Option<i64>,
    ) -> bool {
        let written = self.run_pass(inner, event, sign_in_since);
        self.busy_since.store(0, Ordering::Release);
        written
    }

    /// The synchronous whole publish: note, claim, pass. The service splits these across its
    /// endpoint and worker so a slow write never holds the socket; tests drive them together.
    pub fn publish(&self, event: &SampleEvent) -> bool {
        self.note(event);
        match self.try_begin() {
            Some(mut inner) => self.pass(&mut inner, event),
            None => false,
        }
    }

    fn run_pass(&self, inner: &mut Inner, event: &SampleEvent, sign_in_since: Option<i64>) -> bool {
        let deps = &self.deps;
        let mut directory = String::new();
        let result = self.publish_files(inner, event, &mut directory, sign_in_since);
        match result {
            Ok(written) => written,
            Err(()) => {
                if inner.warned.as_deref() != Some(directory.as_str()) {
                    inner.warned = Some(directory.clone());
                    (deps.log)("agent_usage_snapshot.write_failed", &directory);
                }
                false
            }
        }
    }

    fn publish_files(
        &self,
        inner: &mut Inner,
        event: &SampleEvent,
        directory: &mut String,
        sign_in_since: Option<i64>,
    ) -> Result<bool, ()> {
        let deps = &self.deps;
        // Off until a folder is set, and nothing to publish without accounts: neither case
        // may start the sign-in reads.
        let Some(chosen) = (deps.settings)() else {
            return Ok(false);
        };
        *directory = chosen.directory.clone();
        if event.accounts.is_empty() {
            return Ok(false);
        }
        let sign_in_started_at = sign_in_since.unwrap_or_else(|| (deps.now_ms)());
        let (rows, failed) = fetch_sign_in(&deps.sign_in);
        let renewal = (deps.sign_in_renewal)();
        if !(deps.is_directory)(directory).map_err(|_| ())? {
            return Err(());
        }
        let sign_in_profiles = if failed {
            vec![sign_in_collection_failed_row(&(deps.hostname)())]
        } else {
            rows
        };
        let recent = self.recent.lock().unwrap_or_else(|e| e.into_inner());
        let (mut snapshot, history) = build_snapshot_files(
            event,
            (deps.refresh_seconds)(),
            Some(&recent),
            &sign_in_profiles,
            renewal.as_ref(),
        );
        drop(recent);
        let mut history_text = history.to_json();
        drop(history);
        history_text.push('\n');
        (deps.write)(directory, SNAPSHOT_HISTORY, &history_text).map_err(|_| ())?;
        drop(history_text);
        // Rechecked against publication time, not the sample's, as late as the writes allow.
        if let Some(sign_in) = snapshot.get("signIn").and_then(Value::as_array) {
            let downgraded =
                downgrade_crossed_sign_in_rows(sign_in, sign_in_started_at, (deps.now_ms)());
            snapshot["signIn"] = Value::Array(downgraded);
        }
        // usage.json is the commit point of each pass. Downgrades only accumulate, so
        // rechecking after each write converges: the pass ends only when a full markdown+JSON
        // pair lands with the rows stable across both writes.
        let mut json_committed = false;
        let loop_result = self.commit_loop(
            &mut snapshot,
            directory,
            sign_in_started_at,
            &mut json_committed,
        );
        if let Err(error) = loop_result {
            if json_committed {
                // One best-effort JSON rewrite still carries the latest downgraded rows:
                // machines read the JSON, and it must not keep an expired login published.
                if let Some(sign_in) = snapshot.get("signIn").and_then(Value::as_array) {
                    let downgraded = downgrade_crossed_sign_in_rows(
                        sign_in,
                        sign_in_started_at,
                        (deps.now_ms)(),
                    );
                    snapshot["signIn"] = Value::Array(downgraded);
                }
                let _ = (deps.write)(
                    directory,
                    SNAPSHOT_JSON,
                    &format!("{}\n", crate::js::stringify_pretty(&snapshot)),
                );
            }
            return Err(error);
        }
        inner.warned = None;
        // The store's hourly and daily files, once an hour (after usage.json commits a pass).
        let hour_key = format!(
            "{}\n{}",
            directory,
            crate::exports::export_hour(event.sampled_at)
        );
        if inner.rollups_written.as_deref() != Some(hour_key.as_str())
            && (deps.rollup_write)(event, directory, &deps.write).is_ok()
        {
            inner.rollups_written = Some(hour_key);
        }
        // The setting can change without a restart, so this follows it; unchanged, it is not rewritten.
        let locations = render_snapshot_locations(&chosen.locations);
        let locations_key = format!("{}\n{}", directory, locations);
        if inner.locations_written.as_deref() != Some(locations_key.as_str())
            && (deps.write)(directory, SNAPSHOT_LOCATIONS, &locations).is_ok()
        {
            inner.locations_written = Some(locations_key);
        }
        // Once per folder and dashboard start; a failed copy is retried after the next sample.
        if inner.client_synced.as_deref() != Some(directory.as_str())
            && (deps.sync_client)(directory).is_ok()
        {
            inner.client_synced = Some(directory.clone());
        }
        Ok(true)
    }

    fn commit_loop(
        &self,
        snapshot: &mut Value,
        directory: &str,
        sign_in_started_at: i64,
        json_committed: &mut bool,
    ) -> Result<(), ()> {
        let deps = &self.deps;
        loop {
            let markdown = render_snapshot_markdown(snapshot, (deps.now_ms)());
            (deps.write)(directory, SNAPSHOT_MARKDOWN, &markdown).map_err(|_| ())?;
            if let Some(sign_in) = snapshot.get("signIn").and_then(Value::as_array) {
                let rechecked =
                    downgrade_crossed_sign_in_rows(sign_in, sign_in_started_at, (deps.now_ms)());
                if rechecked != *sign_in {
                    snapshot["signIn"] = Value::Array(rechecked);
                    continue;
                }
            }
            (deps.write)(
                directory,
                SNAPSHOT_JSON,
                &format!("{}\n", crate::js::stringify_pretty(snapshot)),
            )
            .map_err(|_| ())?;
            *json_committed = true;
            let Some(sign_in) = snapshot.get("signIn").and_then(Value::as_array) else {
                break;
            };
            let rechecked =
                downgrade_crossed_sign_in_rows(sign_in, sign_in_started_at, (deps.now_ms)());
            if rechecked == *sign_in {
                break;
            }
            snapshot["signIn"] = Value::Array(rechecked);
        }
        Ok(())
    }
}

/// The sign-in fetch under its 60-second limit: a fetch that never returns is released, and a
/// rejected or timed-out fetch is a collection failure, not an empty result.
fn fetch_sign_in(
    sign_in: &Arc<dyn Fn() -> Result<Vec<Value>, ()> + Send + Sync>,
) -> (Vec<Value>, bool) {
    let fetch = Arc::clone(sign_in);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(fetch());
    });
    match receiver.recv_timeout(Duration::from_millis(SIGNIN_FETCH_LIMIT_MS)) {
        Ok(Ok(rows)) => (rows, false),
        _ => (Vec::new(), true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::js::{self, member};
    use crate::{SNAPSHOT_HISTORY_DAILY, SNAPSHOT_HISTORY_HOURLY};
    use serde_json::json;
    use std::sync::atomic::AtomicUsize;

    const NOW: i64 = 1_792_065_600_000;

    fn event() -> SampleEvent {
        SampleEvent {
            accounts: vec![json!({
                "id": "codex:gmail", "provider": "codex", "providerLabel": "Codex",
                "label": "Gmail", "email": "person@example.com", "plan": "Pro",
                "platform": "ubuntu", "source": "Codex usage on Ubuntu", "status": "ok",
                "sampledAt": "2026-10-15T12:00:00.000Z", "isActive": true,
                "windows": [{"key": "five_hour", "label": "5-hour", "usedPercent": 40, "remainingPercent": 60,
                              "resetAt": "2026-10-15T14:00:00.000Z", "windowMinutes": 300}],
                "capabilities": {"codexProfile": "gmail"}
            })],
            history: None,
            sampled_at: NOW,
            long_burn: None,
        }
    }

    fn deps(writes: Arc<Mutex<Vec<(String, String)>>>) -> Deps {
        let sink = Arc::clone(&writes);
        Deps {
            settings: Arc::new(|| {
                Some(SnapshotSettings {
                    directory: "/share/aac-usage".into(),
                    locations: vec![],
                })
            }),
            write: Arc::new(move |_directory, name, body| {
                sink.lock()
                    .expect("writes")
                    .push((name.to_owned(), body.to_owned()));
                Ok(())
            }),
            is_directory: Arc::new(|_| Ok(true)),
            refresh_seconds: Arc::new(|| 60.0),
            sync_client: Arc::new(|_| Ok(())),
            sign_in: Arc::new(|| Ok(vec![])),
            sign_in_renewal: Arc::new(|| None),
            rollup_write: Arc::new(|_, _, _| Ok(())),
            now_ms: Arc::new(|| NOW),
            hostname: Arc::new(|| "fixture".into()),
            log: Arc::new(|_, _| {}),
        }
    }

    #[test]
    fn never_starts_sign_in_probes_without_a_folder() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let mut d = deps(Arc::new(Mutex::new(Vec::new())));
        d.settings = Arc::new(|| None);
        d.sign_in = Arc::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(vec![])
        });
        let publisher = SnapshotPublisher::new(d);
        assert!(!publisher.publish(&event()));
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn writes_history_markdown_json_in_order() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let publisher = SnapshotPublisher::new(deps(Arc::clone(&writes)));
        assert!(publisher.publish(&event()));
        let names: Vec<String> = writes
            .lock()
            .expect("writes")
            .iter()
            .map(|(name, _)| name.clone())
            .collect();
        assert_eq!(
            names,
            vec![
                SNAPSHOT_HISTORY,
                SNAPSHOT_MARKDOWN,
                SNAPSHOT_JSON,
                SNAPSHOT_LOCATIONS
            ]
        );
    }

    #[test]
    fn empty_accounts_publish_nothing() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let publisher = SnapshotPublisher::new(deps(Arc::clone(&writes)));
        let mut empty = event();
        empty.accounts = vec![];
        assert!(!publisher.publish(&empty));
        assert!(writes.lock().expect("writes").is_empty());
    }

    #[test]
    fn failed_sign_in_fetch_publishes_a_named_row() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut d = deps(Arc::clone(&writes));
        d.sign_in = Arc::new(|| Err(()));
        let publisher = SnapshotPublisher::new(d);
        assert!(publisher.publish(&event()));
        let guard = writes.lock().expect("writes");
        let json_body = guard
            .iter()
            .find(|(name, _)| name == SNAPSHOT_JSON)
            .map(|(_, body)| body.clone())
            .expect("json");
        let json: Value = serde_json::from_str(&json_body).expect("parse");
        let sign_in = member(&json, "signIn").as_array().expect("signIn");
        assert_eq!(sign_in.len(), 1);
        assert_eq!(sign_in[0]["state"], json!("unavailable"));
        assert_eq!(sign_in[0]["error"], json!("sign-in collection failed"));
        let markdown = guard
            .iter()
            .find(|(name, _)| name == SNAPSHOT_MARKDOWN)
            .map(|(_, body)| body.clone())
            .expect("markdown");
        assert!(markdown.contains("sign-in collection failed"));
    }

    #[test]
    fn failing_folder_survives_and_warns_once() {
        let logs = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&logs);
        let mut d = deps(Arc::new(Mutex::new(Vec::new())));
        d.is_directory = Arc::new(|_| Err(io::Error::other("stale file handle")));
        d.log = Arc::new(move |event, directory| {
            sink.lock()
                .expect("logs")
                .push((event.to_owned(), directory.to_owned()))
        });
        let publisher = SnapshotPublisher::new(d);
        assert!(!publisher.publish(&event()));
        assert!(!publisher.publish(&event()));
        assert_eq!(logs.lock().expect("logs").len(), 1);
    }

    #[test]
    fn busy_slot_skips_and_counts_the_sample() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let publisher = SnapshotPublisher::new(deps(Arc::clone(&writes)));
        let guard = publisher.try_begin().expect("first slot");
        // A second sample while the first holds the slot: skipped, but noted.
        assert!(publisher.try_begin().is_none());
        publisher.note(&event());
        let mut guard = guard;
        assert!(publisher.pass(&mut guard, &event()));
        drop(guard);
        assert!(publisher.try_begin().is_some());
    }

    #[test]
    fn crossed_deadline_during_a_slow_write_converges() {
        // The row's deadline crosses while the markdown write is in flight; the published
        // JSON must not present it as a future end, and the collected row is never mutated.
        let clock = Arc::new(AtomicI64::new(NOW));
        let writes = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&writes);
        let tick = Arc::clone(&clock);
        let mut d = deps(Arc::clone(&writes));
        let live_row = json!({
            "name": "soon", "provider": "claude", "platform": "ubuntu", "machine": "ubuntu",
            "state": "critical", "loggedOutAt": js::iso_of(NOW + 80), "fixCommand": "claude auth login"
        });
        let row = live_row.clone();
        d.sign_in = Arc::new(move || Ok(vec![row.clone()]));
        d.now_ms = Arc::new(move || clock.load(Ordering::SeqCst));
        d.write = Arc::new(move |_directory, name, body| {
            if name == SNAPSHOT_MARKDOWN {
                tick.store(NOW + 200, Ordering::SeqCst);
            }
            sink.lock()
                .expect("writes")
                .push((name.to_owned(), body.to_owned()));
            Ok(())
        });
        let publisher = SnapshotPublisher::new(d);
        let mut sample = event();
        sample.sampled_at = NOW;
        assert!(publisher.publish(&sample));
        let guard = writes.lock().expect("writes");
        let json_body = guard
            .iter()
            .rev()
            .find(|(name, _)| name == SNAPSHOT_JSON)
            .map(|(_, body)| body.clone())
            .expect("json");
        let json: Value = serde_json::from_str(&json_body).expect("parse");
        assert_eq!(json["signIn"][0]["state"], json!("unavailable"));
        let markdown = guard
            .iter()
            .rev()
            .find(|(name, _)| name == SNAPSHOT_MARKDOWN)
            .map(|(_, body)| body.clone())
            .expect("markdown");
        assert!(!markdown.contains("re-login before then"));
        assert_eq!(live_row["state"], json!("critical"));
    }

    #[test]
    fn rollups_write_once_an_hour() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let mut d = deps(Arc::clone(&writes));
        d.rollup_write = Arc::new(|_event, directory, write| {
            write(
                directory,
                SNAPSHOT_HISTORY_HOURLY,
                "{\"version\":1,\"resolution\":\"hour\"}\n",
            )
            .map_err(|_| ())?;
            write(
                directory,
                SNAPSHOT_HISTORY_DAILY,
                "{\"version\":1,\"resolution\":\"day\"}\n",
            )
            .map_err(|_| ())?;
            Ok(())
        });
        let publisher = SnapshotPublisher::new(d);
        assert!(publisher.publish(&event()));
        let mut next = event();
        next.sampled_at = NOW + 60_000;
        assert!(publisher.publish(&next));
        let guard = writes.lock().expect("writes");
        assert_eq!(
            guard
                .iter()
                .filter(|(name, _)| name == SNAPSHOT_HISTORY_HOURLY)
                .count(),
            1
        );
        let mut next_hour = event();
        next_hour.sampled_at = NOW + 3_600_000;
        drop(guard);
        assert!(publisher.publish(&next_hour));
        assert_eq!(
            writes
                .lock()
                .expect("writes")
                .iter()
                .filter(|(name, _)| name == SNAPSHOT_HISTORY_HOURLY)
                .count(),
            2
        );
    }

    /// R2: the rollup build/write callback runs only when the hourly write is due, never on
    /// the samples between hours. The base built the rollups on every sample (the pre-fetch
    /// ran before this gate) and discarded them 59/60 times; this pins that the expensive
    /// callback is invoked exactly once per hour, not once per sample.
    #[test]
    fn the_rollup_callback_runs_only_when_the_hourly_write_is_due() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&calls);
        let mut d = deps(Arc::new(Mutex::new(Vec::new())));
        d.rollup_write = Arc::new(move |_event, _directory, _write| {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(())
        });
        let publisher = SnapshotPublisher::new(d);
        // Ten samples across one hour: the callback runs once (the first, due sample).
        for step in 0..10 {
            let mut sample = event();
            sample.sampled_at = NOW + step * 60_000;
            assert!(publisher.publish(&sample));
        }
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "one build per hour, not per sample"
        );
        // The next hour is due again: exactly one more build.
        let mut next_hour = event();
        next_hour.sampled_at = NOW + 3_600_000;
        assert!(publisher.publish(&next_hour));
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
