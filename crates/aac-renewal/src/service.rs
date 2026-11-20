//! The saved-login renewal timer (`src/web-server/services/codex-profile-renewal-service.ts`):
//! first cycle 3-5 minutes after start, then every 6 hours +/- 30 minutes,
//! sooner when a backoff or a busy activation lock asks for a retry (never
//! under 5 minutes). One cycle at a time; stop() lets a refresh in flight
//! finish but starts no further profile.

use crate::jsnum::{iso, js_round, parse_date_ms};
use crate::planner::insert_opt;
use crate::renewal::{
    CodexProfileRenewalOptions, CodexProfileRenewalProfileStatus, CodexRenewalCycle,
    get_codex_profile_renewal_status, renew_due_codex_profiles, renewal_enabled, system_now_ms,
    system_random,
};
use crate::types::{CODEX_RENEWAL_MESSAGES, CodexRenewalReason, CodexRenewalState};
use serde_json::{Map, Value};
use std::collections::HashMap;
use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::time::Duration;

const FIRST_DELAY_MS: i64 = 3 * 60_000;
const FIRST_JITTER_MS: i64 = 2 * 60_000;
const CYCLE_MS: i64 = 6 * 60 * 60_000;
const CYCLE_JITTER_MS: i64 = 30 * 60_000;
const MIN_RETRY_DELAY_MS: i64 = 5 * 60_000;
const STATUS_TTL_MS: i64 = 15_000;

/// The fixed line a failed cycle keeps (no cause detail).
pub const CYCLE_FAILED_MESSAGE: &str =
    "The last renewal check could not read the saved Codex profiles.";
/// The fixed line a failed status read keeps.
pub const STATUS_READ_FAILED_MESSAGE: &str =
    "Saved Codex profiles could not be read for renewal status.";

/// The injectable cycle runner (test seam).
pub type CycleSeam = Arc<
    dyn Fn(
            CodexProfileRenewalOptions,
        ) -> Pin<Box<dyn Future<Output = Result<CodexRenewalCycle, ()>> + Send>>
        + Send
        + Sync,
>;

/// Everything the service needs; the clock, jitter and cycle are injectable.
#[derive(Clone)]
pub struct CodexProfileRenewalServiceDeps {
    /// The CCS root this service renews (one service per scope).
    pub ccs_dir: PathBuf,
    /// The environment for the disable switch; None falls through to the
    /// renewal options' env, then the process env.
    pub env_map: Option<HashMap<String, String>>,
    /// Epoch ms clock.
    pub now: Arc<dyn Fn() -> i64 + Send + Sync>,
    /// Jitter source in [0, 1).
    pub random: Arc<dyn Fn() -> f64 + Send + Sync>,
    /// Passed to every cycle and status read (test homes, fetch seam, log sink).
    pub renewal: CodexProfileRenewalOptions,
    /// Test seam for the cycle itself.
    pub run_cycle: Option<CycleSeam>,
}

impl Default for CodexProfileRenewalServiceDeps {
    fn default() -> Self {
        Self {
            ccs_dir: PathBuf::new(),
            env_map: None,
            now: Arc::new(system_now_ms),
            random: Arc::new(system_random),
            renewal: CodexProfileRenewalOptions::default(),
            run_cycle: None,
        }
    }
}

/// The dashboard DTO; local reads only, cached for 15 seconds.
#[derive(Debug, Clone)]
pub struct CodexProfileRenewalServiceStatus {
    /// The disable switch.
    pub enabled: bool,
    /// Whether the timer is running.
    pub running: bool,
    /// Profile being renewed right now, if any.
    pub renewing: Option<String>,
    /// Last cycle finish, ISO.
    pub last_cycle_at: Option<String>,
    /// Next scheduled cycle, ISO.
    pub next_cycle_at: Option<String>,
    /// Fixed error line, when the last cycle or read failed.
    pub message: Option<String>,
    /// Per-profile status.
    pub profiles: Vec<CodexProfileRenewalProfileStatus>,
}

impl CodexProfileRenewalServiceStatus {
    /// The DTO in the TS key order.
    pub fn to_json(&self) -> Value {
        let mut out = Map::new();
        out.insert("enabled".into(), Value::Bool(self.enabled));
        out.insert("running".into(), Value::Bool(self.running));
        insert_opt(&mut out, "renewing", &self.renewing);
        insert_opt(&mut out, "lastCycleAt", &self.last_cycle_at);
        insert_opt(&mut out, "nextCycleAt", &self.next_cycle_at);
        insert_opt(&mut out, "message", &self.message);
        out.insert(
            "profiles".into(),
            Value::Array(self.profiles.iter().map(|p| p.to_json()).collect()),
        );
        Value::Object(out)
    }
}

type CycleResult = Result<CodexRenewalCycle, ()>;

struct Inner {
    running: bool,
    generation: u64,
    renewing: Option<String>,
    last_cycle_at: Option<String>,
    next_cycle_at: Option<String>,
    last_error: Option<String>,
    status_cache: Option<(i64, CodexProfileRenewalServiceStatus)>,
    in_flight: Option<tokio::sync::watch::Receiver<CycleResult>>,
    timer: Option<Pin<Box<tokio::time::Sleep>>>,
}

/// One renewal timer per CCS scope. Create inside a tokio runtime (the timer
/// uses tokio time); drive it with `run_until`.
pub struct CodexProfileRenewalService {
    /// The CCS root this service renews.
    pub ccs_dir: PathBuf,
    deps: CodexProfileRenewalServiceDeps,
    inner: Arc<Mutex<Inner>>,
}

impl CodexProfileRenewalService {
    /// A stopped service for one scope.
    pub fn new(deps: CodexProfileRenewalServiceDeps) -> Self {
        let ccs_dir = crate::procscan::normalize(&deps.ccs_dir);
        Self {
            ccs_dir,
            deps,
            inner: Arc::new(Mutex::new(Inner {
                running: false,
                generation: 0,
                renewing: None,
                last_cycle_at: None,
                next_cycle_at: None,
                last_error: None,
                status_cache: None,
                in_flight: None,
                timer: None,
            })),
        }
    }

    fn now(&self) -> i64 {
        (self.deps.now)()
    }

    fn random(&self) -> f64 {
        (self.deps.random)()
    }

    fn enabled(&self) -> bool {
        match &self.deps.env_map {
            Some(map) => crate::types::is_codex_profile_renewal_enabled(map),
            None => renewal_enabled(&self.deps.renewal),
        }
    }

    /// Start the timer: first cycle 3-5 minutes out. A disabled switch logs
    /// the fixed line and schedules nothing.
    pub fn start(&self) {
        let mut inner = self.inner.lock().expect("renewal state");
        if inner.running {
            return;
        }
        if !self.enabled() {
            tracing::info!(
                target: "aac_renewal",
                event = "codex.renewal",
                outcome = "disabled",
                "{}",
                CodexRenewalReason::Disabled.message()
            );
            return;
        }
        inner.running = true;
        let delay = FIRST_DELAY_MS + js_round(FIRST_JITTER_MS as f64 * self.random());
        Self::schedule_locked(&mut inner, delay, self.now());
    }

    /// Stop the timer; a refresh in flight finishes but no further profile starts.
    pub fn stop(&self) {
        let mut inner = self.inner.lock().expect("renewal state");
        inner.running = false;
        inner.generation += 1;
        inner.timer = None;
        inner.next_cycle_at = None;
    }

    fn schedule_locked(inner: &mut Inner, delay: i64, now: i64) {
        inner.next_cycle_at = iso(now + delay);
        inner.timer = Some(Box::pin(tokio::time::sleep(Duration::from_millis(
            delay.max(0) as u64,
        ))));
    }

    /// The next cycle's delay: the regular 6 h +/- 30 m, sooner for a backoff
    /// or busy retry, never under 5 minutes.
    pub fn next_delay(&self, cycle: Option<&CodexRenewalCycle>) -> i64 {
        let regular = CYCLE_MS + js_round((self.random() * 2.0 - 1.0) * CYCLE_JITTER_MS as f64);
        let retry_at = cycle
            .and_then(|c| c.retry_at.as_deref())
            .and_then(parse_date_ms);
        match retry_at {
            Some(retry) => regular.min(MIN_RETRY_DELAY_MS.max(retry - self.now())),
            None => regular,
        }
    }

    fn cycle_options(&self, generation: u64) -> CodexProfileRenewalOptions {
        let mut options = self.deps.renewal.clone();
        if let Some(map) = &self.deps.env_map {
            options.env_map = Some(map.clone());
        }
        options.now = Arc::clone(&self.deps.now);
        options.random = Arc::clone(&self.deps.random);
        let inner = Arc::clone(&self.inner);
        let deps = self.deps.clone();
        options.should_continue = Some(Arc::new(move || {
            let guard = inner.lock().expect("renewal state");
            let enabled = match &deps.env_map {
                Some(map) => crate::types::is_codex_profile_renewal_enabled(map),
                None => renewal_enabled(&deps.renewal),
            };
            guard.generation == generation && enabled
        }));
        let inner = Arc::clone(&self.inner);
        options.on_renewing = Some(Arc::new(move |name: Option<&str>| {
            let mut guard = inner.lock().expect("renewal state");
            guard.renewing = name.map(str::to_owned);
            guard.status_cache = None;
        }));
        options
    }

    /// Coalesced: concurrent callers share one cycle. Never panics.
    pub async fn run_cycle(&self) -> Option<CodexRenewalCycle> {
        let receiver = {
            let mut inner = self.inner.lock().expect("renewal state");
            if let Some(receiver) = &inner.in_flight {
                receiver.clone()
            } else {
                let generation = inner.generation;
                let options = self.cycle_options(generation);
                let runner: CycleSeam = self.deps.run_cycle.clone().unwrap_or_else(|| {
                    Arc::new(|options| {
                        Box::pin(async move { renew_due_codex_profiles(&options).await })
                    })
                });
                let (tx, rx) = tokio::sync::watch::channel::<CycleResult>(Err(()));
                inner.in_flight = Some(rx.clone());
                let inner = Arc::clone(&self.inner);
                tokio::spawn(async move {
                    let result = runner(options).await;
                    {
                        let mut guard = inner.lock().expect("renewal state");
                        match &result {
                            Ok(cycle) => {
                                guard.last_cycle_at = Some(cycle.finished_at.clone());
                                guard.last_error = None;
                            }
                            Err(()) => {
                                guard.last_error = Some(CYCLE_FAILED_MESSAGE.to_owned());
                                tracing::warn!(
                                    target: "aac_renewal",
                                    event = "codex.renewal",
                                    outcome = "cycle_failed",
                                    "{CYCLE_FAILED_MESSAGE}"
                                );
                            }
                        }
                        guard.in_flight = None;
                        guard.renewing = None;
                        guard.status_cache = None;
                    }
                    let _ = tx.send(result);
                });
                rx
            }
        };
        let mut receiver = receiver;
        // The sender drops after the state update; a changed() error means the
        // cycle task ended without a value (never happens: it always sends).
        let _ = receiver.changed().await;
        let result = receiver.borrow().clone();
        result.ok()
    }

    /// Dashboard DTO; local reads only, cached for 15 seconds.
    pub async fn get_status(&self) -> CodexProfileRenewalServiceStatus {
        let now = self.now();
        let cached = {
            let inner = self.inner.lock().expect("renewal state");
            inner
                .status_cache
                .as_ref()
                .filter(|(at, _)| now - at < STATUS_TTL_MS)
                .map(|(_, value)| value.clone())
        };
        if let Some(value) = cached {
            return self.with_live(value);
        }
        let mut options = self.deps.renewal.clone();
        if let Some(map) = &self.deps.env_map {
            options.env_map = Some(map.clone());
        }
        options.now = Arc::clone(&self.deps.now);
        let (mut message, last_cycle_at, next_cycle_at) = {
            let inner = self.inner.lock().expect("renewal state");
            (
                inner.last_error.clone(),
                inner.last_cycle_at.clone(),
                inner.next_cycle_at.clone(),
            )
        };
        let profiles = match get_codex_profile_renewal_status(&options) {
            Ok(status) => status.profiles,
            Err(()) => {
                message = Some(STATUS_READ_FAILED_MESSAGE.to_owned());
                Vec::new()
            }
        };
        let value = CodexProfileRenewalServiceStatus {
            enabled: self.enabled(),
            running: false,
            renewing: None,
            last_cycle_at,
            next_cycle_at,
            message,
            profiles,
        };
        {
            let mut inner = self.inner.lock().expect("renewal state");
            inner.status_cache = Some((now, value.clone()));
        }
        self.with_live(value)
    }

    fn with_live(
        &self,
        value: CodexProfileRenewalServiceStatus,
    ) -> CodexProfileRenewalServiceStatus {
        let inner = self.inner.lock().expect("renewal state");
        let renewing = inner.renewing.clone();
        let mut live = value;
        live.running = inner.running;
        live.renewing = renewing.clone();
        live.last_cycle_at = inner.last_cycle_at.clone();
        live.next_cycle_at = inner.next_cycle_at.clone();
        for profile in &mut live.profiles {
            if renewing.as_deref() == Some(profile.name.as_str()) {
                profile.state = CodexRenewalState::Renewing;
                profile.reason = CodexRenewalReason::Renewing;
                profile.message = CodexRenewalReason::Renewing.message();
            }
        }
        live
    }

    /// Drive the timer until the stop signal flips true. The stop lets a cycle
    /// in flight finish (its shouldContinue sees the new generation) and
    /// schedules nothing further.
    pub async fn run_until(&self, mut stop: tokio::sync::watch::Receiver<bool>) {
        self.start();
        loop {
            if *stop.borrow() {
                self.stop();
                return;
            }
            let timer = {
                let mut inner = self.inner.lock().expect("renewal state");
                if !inner.running {
                    return;
                }
                inner.timer.take()
            };
            let Some(mut timer) = timer else {
                // Running without a timer only happens between stop and start.
                tokio::task::yield_now().await;
                continue;
            };
            tokio::select! {
                _ = &mut timer => {}
                changed = stop.changed() => {
                    if changed.is_err() || *stop.borrow() {
                        self.stop();
                        return;
                    }
                    // A false flip leaves the timer armed.
                    let mut inner = self.inner.lock().expect("renewal state");
                    inner.timer = Some(timer);
                    continue;
                }
            }
            let cycle = self.run_cycle().await;
            let running = self.inner.lock().expect("renewal state").running;
            if running {
                let delay = self.next_delay(cycle.as_ref());
                let mut inner = self.inner.lock().expect("renewal state");
                Self::schedule_locked(&mut inner, delay, self.now());
            }
        }
    }
}

/// The fixed disabled message, for parity exports.
pub fn disabled_message() -> &'static str {
    CODEX_RENEWAL_MESSAGES
        .iter()
        .find(|(code, _)| *code == "disabled")
        .map(|(_, message)| *message)
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planner::CodexRenewalEnvironment;
    use crate::renewal::{CodexProfileRenewalOptions, CodexRenewalCycle};

    fn cycle(retry_at: Option<String>) -> CodexRenewalCycle {
        CodexRenewalCycle {
            started_at: "2026-10-08T12:00:00.000Z".into(),
            finished_at: "2026-10-08T12:00:00.000Z".into(),
            enabled: true,
            plans: Vec::new(),
            attempts: Vec::new(),
            retry_at,
        }
    }

    /// A sandboxed root: the status read never touches a real home.
    fn temp_deps() -> (tempfile::TempDir, CodexProfileRenewalServiceDeps) {
        let root = tempfile::tempdir().expect("tempdir");
        let ccs_dir = root.path().join(".ccs");
        let deps = CodexProfileRenewalServiceDeps {
            ccs_dir: ccs_dir.clone(),
            renewal: CodexProfileRenewalOptions {
                env: CodexRenewalEnvironment {
                    ccs_dir,
                    codex_home: Some(root.path().join("shared-codex")),
                    home_dir: Some(root.path().join("home")),
                    ..Default::default()
                },
                ..Default::default()
            },
            ..Default::default()
        };
        (root, deps)
    }

    #[tokio::test]
    async fn first_delay_and_stop() {
        let now = 1_791_460_800_000;
        let (_root, deps) = temp_deps();
        let service = CodexProfileRenewalService::new(CodexProfileRenewalServiceDeps {
            now: Arc::new(move || now),
            random: Arc::new(|| 0.5),
            ..deps
        });
        service.start();
        let status = service.get_status().await;
        assert!(status.running);
        assert_eq!(
            status.next_cycle_at.as_deref().and_then(parse_date_ms),
            Some(now + 4 * 60_000)
        );
        service.stop();
        assert!(service.get_status().await.next_cycle_at.is_none());
    }

    #[tokio::test]
    async fn disabled_schedules_nothing() {
        let mut env = HashMap::new();
        env.insert("CCS_CODEX_RENEWAL".to_owned(), "0".to_owned());
        let (_root, deps) = temp_deps();
        let service = CodexProfileRenewalService::new(CodexProfileRenewalServiceDeps {
            env_map: Some(env),
            ..deps
        });
        service.start();
        let status = service.get_status().await;
        assert!(!status.enabled);
        assert!(!status.running);
        assert!(status.next_cycle_at.is_none());
    }

    #[tokio::test]
    async fn retry_shortens_the_next_cycle_but_never_under_five_minutes() {
        let now = 1_791_460_800_000;
        let (_root, deps) = temp_deps();
        let service = CodexProfileRenewalService::new(CodexProfileRenewalServiceDeps {
            now: Arc::new(move || now),
            random: Arc::new(|| 0.5),
            ..deps
        });
        // random 0.5 -> regular delay exactly 6 h.
        assert_eq!(service.next_delay(None), 6 * 60 * 60_000);
        let soon = cycle(Some(iso(now + 10 * 60_000).expect("iso")));
        assert_eq!(service.next_delay(Some(&soon)), 10 * 60_000);
        let too_soon = cycle(Some(iso(now + 60_000).expect("iso")));
        assert_eq!(service.next_delay(Some(&too_soon)), MIN_RETRY_DELAY_MS);
        let past = cycle(Some(iso(now - 60_000).expect("iso")));
        assert_eq!(service.next_delay(Some(&past)), MIN_RETRY_DELAY_MS);
    }

    #[tokio::test]
    async fn coalesced_cycles_share_one_run() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runner_gate = Arc::clone(&gate);
        let runner_calls = Arc::clone(&calls);
        let (_root, deps) = temp_deps();
        let service = Arc::new(CodexProfileRenewalService::new(
            CodexProfileRenewalServiceDeps {
                run_cycle: Some(Arc::new(move |options| {
                    let gate = Arc::clone(&runner_gate);
                    let calls = Arc::clone(&runner_calls);
                    Box::pin(async move {
                        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if let Some(on_renewing) = &options.on_renewing {
                            on_renewing(Some("alpha"));
                        }
                        gate.notified().await;
                        if let Some(on_renewing) = &options.on_renewing {
                            on_renewing(None);
                        }
                        Ok(cycle(None))
                    })
                })),
                ..deps
            },
        ));
        let first = tokio::spawn({
            let service = Arc::clone(&service);
            async move { service.run_cycle().await }
        });
        // Wait until the cycle marks the profile renewing.
        loop {
            if service.get_status().await.renewing.is_some() {
                break;
            }
            tokio::task::yield_now().await;
        }
        assert_eq!(
            service.get_status().await.renewing.as_deref(),
            Some("alpha")
        );
        // A second caller attaches to the in-flight cycle before it finishes.
        let second = service.run_cycle();
        tokio::pin!(second);
        std::future::poll_fn(|cx| {
            let _ = second.as_mut().poll(cx);
            std::task::Poll::Ready(())
        })
        .await;
        gate.notify_one();
        let a = first.await.expect("cycle task");
        let b = second.await;
        assert!(a.is_some());
        assert!(b.is_some());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        let status = service.get_status().await;
        assert!(status.renewing.is_none());
        assert_eq!(
            status.last_cycle_at.as_deref(),
            Some("2026-10-08T12:00:00.000Z")
        );
    }

    #[tokio::test]
    async fn stopped_cycle_starts_no_further_profile() {
        let gate = Arc::new(tokio::sync::Notify::new());
        let runner_gate = Arc::clone(&gate);
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let runner_started = Arc::clone(&started);
        let (_root, deps) = temp_deps();
        let service = Arc::new(CodexProfileRenewalService::new(
            CodexProfileRenewalServiceDeps {
                run_cycle: Some(Arc::new(move |options| {
                    let gate = Arc::clone(&runner_gate);
                    let started = Arc::clone(&runner_started);
                    Box::pin(async move {
                        started.store(true, std::sync::atomic::Ordering::SeqCst);
                        gate.notified().await;
                        // The service stopped mid-cycle: the generation moved on.
                        let check = options.should_continue.expect("shouldContinue");
                        assert!(!check());
                        Ok(cycle(None))
                    })
                })),
                ..deps
            },
        ));
        service.start();
        let cycle_task = tokio::spawn({
            let service = Arc::clone(&service);
            async move { service.run_cycle().await }
        });
        while !started.load(std::sync::atomic::Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        service.stop();
        gate.notify_one();
        assert!(cycle_task.await.expect("cycle task").is_some());
    }

    #[tokio::test]
    async fn failed_cycle_keeps_the_fixed_message() {
        let (_root, deps) = temp_deps();
        let service = CodexProfileRenewalService::new(CodexProfileRenewalServiceDeps {
            run_cycle: Some(Arc::new(|_options| Box::pin(async move { Err(()) }))),
            ..deps
        });
        assert!(service.run_cycle().await.is_none());
        let status = service.get_status().await;
        assert_eq!(status.message.as_deref(), Some(CYCLE_FAILED_MESSAGE));
    }

    #[tokio::test(start_paused = true)]
    async fn the_timer_runs_cycles_on_cadence_and_stops() {
        let runs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let runner_runs = Arc::clone(&runs);
        let (_root, deps) = temp_deps();
        let service = Arc::new(CodexProfileRenewalService::new(
            CodexProfileRenewalServiceDeps {
                random: Arc::new(|| 0.5),
                run_cycle: Some(Arc::new(move |_options| {
                    let runs = Arc::clone(&runner_runs);
                    Box::pin(async move {
                        runs.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        Ok(cycle(None))
                    })
                })),
                ..deps
            },
        ));
        let (tx, rx) = tokio::sync::watch::channel(false);
        let driver = tokio::spawn({
            let service = Arc::clone(&service);
            async move { service.run_until(rx).await }
        });
        // First cycle 4 minutes out (random 0.5), then every 6 hours.
        // Let the driver arm its first timer before time moves.
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(4 * 60_000)).await;
        tokio::task::yield_now().await;
        tokio::time::advance(Duration::from_millis(6 * 60 * 60_000)).await;
        tokio::task::yield_now().await;
        let _ = tx.send(true);
        let _ = driver.await;
        assert!(runs.load(std::sync::atomic::Ordering::SeqCst) >= 2);
        assert!(service.get_status().await.next_cycle_at.is_none());
    }
}
