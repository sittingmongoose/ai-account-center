//! R3: the store host. With `rustService.store: on` the service opens the history store's
//! Engine itself (the boot's `store` config, resolved by TS) and serves
//! `POST /internal/store/<method>` with the same JSON method contract as the NAPI module's
//! `NativeStore.call` (`aac_store::ops::dispatch`): one method name and a JSON argument in,
//! the JSON answer text or `<code>: <message>` out. TS's `AacStore` keeps its API and swaps
//! only its handle. The kernel lock (`store.lock`) stays the single-writer proof: this
//! process holds it, so a NAPI open elsewhere is refused, and a crash releases it for the
//! restart (or, once the restart budget is used up, TS's in-process fallback on the same
//! files). Maintenance, the daily backup with its share copy and the weekly verify,
//! compaction and redb snapshot run here with TS's intervals and `last_*` meta keys.
use aac_store::engine::Engine;
use aac_store::ops::{self, Slot};
use aac_views::decline::Decline;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{
    Arc, Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

/// Store calls in flight at most (L1's store pool size). Unlike the read pools, a full pool
/// queues: the NAPI binding queues on libuv's pool too, and a producer must not lose a
/// write to a `busy` decline the in-process store never gave.
const STORE_PERMITS: usize = 4;
/// The longest store request body (`captureReports` already caps its batch at 8 MiB in ops).
#[cfg(unix)]
pub(crate) const STORE_INPUT_CAP: usize = 16 * 1024 * 1024;

const MINUTE_MS: u64 = 60_000;
const DAY_MS: u64 = 86_400_000;
/// TS's maintenance cadence (`store-service.ts`): first pass minutes after start, then
/// every 15 minutes; backup daily; verify, compaction and the redb snapshot weekly.
const FIRST_PASS_MS: u64 = 3 * MINUTE_MS;
const MAINTAIN_EVERY_MS: u64 = 15 * MINUTE_MS;
const BACKUP_EVERY_MS: u64 = DAY_MS;
const REDB_BACKUP_EVERY_MS: u64 = 7 * DAY_MS;
const VERIFY_EVERY_MS: u64 = 7 * DAY_MS;

/// The boot's store config, resolved once by TS at start: the open arguments it would have
/// given the NAPI module, plus the backup job's share inputs. Absent in the boot JSON, the
/// store stays in the TS process exactly as before.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StoreBoot {
    /// The Node module's compiled identity: checked before opening or migrating any files.
    #[serde(default)]
    pub binding_identity: Value,
    /// `<ccsDir>/store`, checked in `Boot::validate`.
    pub dir: PathBuf,
    /// `OpenArgs.retentionDays` (TS's raw-retention constant).
    pub retention_days: u64,
    /// `OpenArgs.cacheBytes`, when `AAC_STORE_CACHE_MB` named one.
    #[serde(default)]
    pub cache_bytes: Option<usize>,
    /// `OpenArgs.activityRetentionDays`, the opt-in Analytics retention.
    #[serde(default)]
    pub activity_retention_days: Option<u64>,
    /// The agent-usage snapshot folder the share backup derives from, when set.
    #[serde(default)]
    pub snapshot_directory: Option<PathBuf>,
    /// `AAC_STORE_SHARE_BACKUP_DIR` as a folder, when it named one.
    #[serde(default)]
    pub share_backup_dir: Option<PathBuf>,
    /// `AAC_STORE_SHARE_BACKUP_DIR=off`: no share copy, and no derivation either.
    #[serde(default)]
    pub share_backup_off: bool,
}

impl StoreBoot {
    /// Use the existing store settings when the public listener hosts the engine itself.
    pub(crate) fn for_public(root: &std::path::Path) -> Option<Self> {
        if disabled_for_public(root) {
            return None;
        }
        let positive = |name| {
            let value = std::env::var(name).ok()?.trim().parse::<f64>().ok()?;
            (value.is_finite() && value > 0.0).then_some(value.round() as u64)
        };
        let share = std::env::var("AAC_STORE_SHARE_BACKUP_DIR").unwrap_or_default();
        let share = share.trim();
        let share_backup_off = share.eq_ignore_ascii_case("off");
        Some(Self {
            binding_identity: aac_store::build_identity(),
            dir: root.join("store"),
            retention_days: 0,
            cache_bytes: positive("AAC_STORE_CACHE_MB")
                .and_then(|mb| usize::try_from(mb).ok()?.checked_mul(1024 * 1024)),
            activity_retention_days: positive("AAC_ANALYTICS_RETENTION_DAYS"),
            snapshot_directory: crate::registered::snapshot_settings(root)
                .map(|settings| settings.directory.into()),
            share_backup_dir: (!share.is_empty() && !share_backup_off)
                .then(|| std::path::absolute(share).ok())
                .flatten(),
            share_backup_off,
        })
    }

    fn open_args(&self) -> String {
        let mut args = json!({
            "dir": self.dir.to_string_lossy(),
            "retentionDays": self.retention_days,
        });
        if let Some(bytes) = self.cache_bytes {
            args["cacheBytes"] = json!(bytes);
        }
        if let Some(days) = self.activity_retention_days {
            args["activityRetentionDays"] = json!(days);
        }
        args.to_string()
    }

    /// Where backups are copied off this computer: the port of TS's `shareBackupRoot`.
    /// Only an existing `share/ai-account-center` folder is used: an unmounted share never
    /// grows a local folder.
    fn share_root(&self, instance_id: &str) -> Option<PathBuf> {
        if let Some(dir) = &self.share_backup_dir {
            return Some(dir.clone());
        }
        if self.share_backup_off {
            return None;
        }
        let snapshot = self.snapshot_directory.as_ref()?;
        if instance_id.len() != 32
            || !instance_id
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return None;
        }
        let project = snapshot.parent()?.join("share").join("ai-account-center");
        if !project.is_dir() {
            return None;
        }
        Some(project.join("store-backups").join(instance_id))
    }
}

/// The explicit store feature switch, independently of its host.
pub(crate) fn disabled() -> bool {
    std::env::var("AAC_STORE").is_ok_and(|value| value.trim().eq_ignore_ascii_case("off"))
}

pub(crate) fn disabled_for_public(root: &std::path::Path) -> bool {
    if disabled() {
        return true;
    }
    if let Ok(value) = std::env::var("AAC_RUST_STORE") {
        return value != "on";
    }
    use aac_files::read::{Ancestors, Read, ReadPolicy, read};
    let policy = ReadPolicy::capped(4 * 1024 * 1024).ancestors(Ancestors::NoLinkBelow(root));
    let Read::Bytes(bytes, _) = read(&root.join("config.yaml"), &policy) else {
        return false;
    };
    std::str::from_utf8(&bytes)
        .ok()
        .and_then(|text| aac_config::split_active(text).ok())
        .is_some_and(|(config, _)| {
            config["rustService"]
                .get("store")
                .is_some_and(|value| value != "on")
        })
}

/// In-memory mirror of a job failure record: keeps the monotonic sequence and fixed code
/// visible through `hostJobs` even when `aac.redb` refuses `set_meta`.
#[derive(Clone)]
struct JobFailure {
    seq: u64,
    code: Option<String>,
    ms: u64,
    persisted: bool,
}

/// The job loop's record, readable through the `hostJobs` method (never in `status`, whose
/// shape the goldens pin).
#[derive(Default)]
struct Jobs {
    passes: u64,
    last_pass_ms: Option<u64>,
    last_error: Option<String>,
    last_error_record: Option<JobFailure>,
    last_share_record: Option<JobFailure>,
    known_error_seq: u64,
    known_share_seq: u64,
}

struct State {
    /// The boot open finished (successfully or not); socket calls wait for it.
    ready: bool,
    boot_error: Option<String>,
    /// This host's identity: a supervised restart is a new process (new id), so the dashboard
    /// watch can tell a reopened store's persisted record from a failure it already reported.
    instance: String,
    jobs: Jobs,
}

/// One queued store call: the method contract plus the channel that carries its answer back
/// to the async handler. The reply is dropped (the caller hung up) without disturbing the run.
enum StoreCall {
    Json {
        method: String,
        args: String,
        reply: tokio::sync::oneshot::Sender<Result<String, String>>,
    },
    /// The Scoreboard's typed snapshot read: the store's resident resolved history,
    /// shared by `Arc` instead of serialized through `runsQuery` text.
    RunsHistory {
        reply: tokio::sync::oneshot::Sender<Result<Arc<aac_store::runs::ResolvedHistory>, String>>,
    },
}

/// The hosted store: one Engine slot behind the NAPI method contract, plus its jobs. Calls run
/// on a dedicated pool of [`STORE_PERMITS`] threads fed by one queue, not the shared blocking
/// pool: a store call costs one handoff to a store thread and one wake back, and never contends
/// with the read routes' blocking tasks. The queue is work-conserving — whichever store thread
/// is free takes the next call, so a long backup on one thread does not queue reads behind it.
pub(crate) struct StoreHost {
    boot: StoreBoot,
    slot: Slot,
    sender: mpsc::Sender<StoreCall>,
    queue: Arc<Mutex<mpsc::Receiver<StoreCall>>>,
    state: Mutex<State>,
    ready_changed: Condvar,
}

fn meta_ms(status: &Value, key: &str) -> i64 {
    status["meta"][key]
        .as_str()
        .and_then(|text| text.parse::<i64>().ok())
        .unwrap_or(0)
}

/// One host's identity (never persisted): a supervised restart opens a new process, so a new
/// id; the counter keeps two hosts in one process (the tests) apart.
fn host_instance() -> String {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let count = NEXT.fetch_add(1, Ordering::Relaxed);
    let started = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{}-{started:x}-{count:x}", std::process::id())
}

impl StoreHost {
    #[cfg(all(test, windows))]
    pub(crate) fn boot_for_test(&self) -> &StoreBoot {
        &self.boot
    }

    pub(crate) fn new(boot: StoreBoot) -> Arc<Self> {
        let (sender, receiver) = mpsc::channel();
        let host = Arc::new(Self {
            boot,
            slot: Slot::new(None),
            sender,
            queue: Arc::new(Mutex::new(receiver)),
            state: Mutex::new(State {
                ready: false,
                boot_error: None,
                instance: host_instance(),
                jobs: Jobs::default(),
            }),
            ready_changed: Condvar::new(),
        });
        if let Some(root) = host.boot.dir.parent() {
            crate::analytics_storage::attach(root, &host);
        }
        // One queue, STORE_PERMITS takers: a thread parks in recv holding the queue lock, and
        // releases it before running the call, so the next free thread takes the next call.
        // Workers hold a Weak: when the last Arc drops, the sender goes, recv ends, they exit.
        for _ in 0..STORE_PERMITS {
            let queue = Arc::clone(&host.queue);
            let weak = Arc::downgrade(&host);
            let spawned = std::thread::Builder::new()
                .name("aac-store".to_string())
                .spawn(move || {
                    loop {
                        let call = {
                            let guard = queue.lock().unwrap_or_else(|p| p.into_inner());
                            match guard.recv() {
                                Ok(call) => call,
                                Err(_) => return,
                            }
                        };
                        let Some(host) = weak.upgrade() else { return };
                        match call {
                            StoreCall::Json {
                                method,
                                args,
                                reply,
                            } => {
                                let result = host.call_blocking(&method, &args);
                                let _ = reply.send(result);
                            }
                            StoreCall::RunsHistory { reply } => {
                                let result = host.runs_history_blocking();
                                let _ = reply.send(result);
                            }
                        }
                    }
                });
            if spawned.is_err() {
                tracing::warn!(target: "aac_http", "store host could not start a worker thread");
            }
        }
        host
    }

    /// The boot open, on a blocking thread at service start: the same `open` TS would have
    /// sent the NAPI module, with the boot's arguments. A failure (another holder, a newer
    /// store) is kept and named by `hostJobs`; the service still serves its other routes,
    /// and TS's `open` retries the dispatch and gets the same code.
    pub(crate) fn open_at_boot(self: &Arc<Self>) {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            ops::dispatch(&self.slot, "open", &self.boot.open_args())
        }));
        let error = match outcome {
            Ok(Ok(_)) => None,
            Ok(Err(error)) => Some(error.to_string()),
            Err(_) => Some("panic: the store stopped this call".to_string()),
        };
        let (error_seq, share_seq) = if error.is_none() {
            self.engine()
                .as_ref()
                .map(|engine| {
                    (
                        persisted_failure_seq(engine, "ts:host_job_last_error"),
                        persisted_failure_seq(engine, "ts:host_job_last_share_failure"),
                    )
                })
                .unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.ready = true;
        state.boot_error = error.clone();
        state.jobs.known_error_seq = state.jobs.known_error_seq.max(error_seq);
        state.jobs.known_share_seq = state.jobs.known_share_seq.max(share_seq);
        drop(state);
        self.ready_changed.notify_all();
        if let Some(error) = &error {
            tracing::warn!(target: "aac_http", "store host open failed: {error}");
        }
    }

    fn seed_failure_floors(&self, engine: &Engine) {
        let error_seq = persisted_failure_seq(engine, "ts:host_job_last_error");
        let share_seq = persisted_failure_seq(engine, "ts:host_job_last_share_failure");
        if error_seq > 0 || share_seq > 0 {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.jobs.known_error_seq = state.jobs.known_error_seq.max(error_seq);
            state.jobs.known_share_seq = state.jobs.known_share_seq.max(share_seq);
        }
    }

    pub(crate) fn wait_ready(&self) {
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        while !state.ready {
            state = self
                .ready_changed
                .wait(state)
                .unwrap_or_else(|p| p.into_inner());
        }
    }

    /// R5/A2: the owned Analytics read composes from this Engine in process.
    pub(crate) fn analytics_engine(&self) -> Option<Arc<Engine>> {
        self.engine()
    }

    /// Why [`Self::analytics_engine`] has no engine, as a reader's fixed cause: still opening or
    /// closed for a restore is a read in progress; a failed boot open is an unsafe store.
    pub(crate) fn engine_unavailable_cause(&self) -> &'static str {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        if state.ready && state.boot_error.is_some() {
            "store_unsafe"
        } else {
            "read_in_progress"
        }
    }

    fn engine(&self) -> Option<Arc<Engine>> {
        self.slot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    /// One store call from the front door. Queued on the store pool, run on a store thread,
    /// with no deadline: `backup` and `maintain` take as long as they take, as they did in
    /// process. The answer is the dispatch's JSON text; an error is its `<code>: <message>`,
    /// exactly what the NAPI binding rejects with. A dropped caller (a hung-up request) does
    /// not cancel the run: an admitted write may still commit, as it did on the blocking pool.
    pub(crate) async fn call(&self, method: String, args: String) -> Result<String, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.sender
            .send(StoreCall::Json {
                method,
                args,
                reply,
            })
            .map_err(|_| "service_down: the store host is stopping".to_string())?;
        answer
            .await
            .map_err(|_| "service_down: the store host is stopping".to_string())?
    }

    /// The store's resident resolved run history, shared by `Arc` with the Scoreboard
    /// owner: queued on the store pool like a method call, but the answer never becomes
    /// `runsQuery` text for the owner to re-parse into a second resident copy.
    pub(crate) async fn runs_history(
        &self,
    ) -> Result<Arc<aac_store::runs::ResolvedHistory>, String> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.sender
            .send(StoreCall::RunsHistory { reply })
            .map_err(|_| "service_down: the store host is stopping".to_string())?;
        answer
            .await
            .map_err(|_| "service_down: the store host is stopping".to_string())?
    }

    fn runs_history_blocking(&self) -> Result<Arc<aac_store::runs::ResolvedHistory>, String> {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.wait_ready();
            let engine = self
                .engine()
                .ok_or_else(|| "service_down: the store host is closed".to_string())?;
            aac_store::runs::resolved_history_snapshot(&engine).map_err(|e| e.to_string())
        }));
        outcome.unwrap_or_else(|_| Err("panic: the store stopped this call".to_string()))
    }

    fn record_dashboard(&self, args: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Tick {
            now: i64,
            accounts: Vec<Value>,
        }
        let tick: Tick = crate::json_object::from_slice(args.as_bytes(), &["now", "accounts"])
            .map_err(|_| "invalid: invalid dashboard sample".to_string())?;
        let observations =
            crate::dashboard_observations::observations(&tick.accounts, tick.now, i64::MIN)
                .map_err(|_| "invalid: unproved dashboard observation".to_string())?;
        let engine = self
            .engine()
            .ok_or_else(|| "service_down: the store host is closed".to_string())?;
        let result = aac_store::usage::record(&engine, observations).map_err(|e| e.to_string())?;
        serde_json::to_string(&result).map_err(|_| "invalid: dashboard sample reply".to_string())
    }

    fn call_blocking(&self, method: &str, args: &str) -> Result<String, String> {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.wait_ready();
            match method {
                "open" => self.open_idempotent(args),
                "recordDashboard" => self.record_dashboard(args),
                "hostJobs" => Ok(self.jobs_json()),
                "hostIdentity" => Ok(aac_store::build_identity().to_string()),
                "activityPricingFacts" => self.pricing_facts_json(),
                _ => ops::dispatch(&self.slot, method, args).map_err(|e| e.to_string()),
            }
        }));
        outcome.unwrap_or_else(|_| Err("panic: the store stopped this call".to_string()))
    }

    /// G14/G15: the owned read's pricing facts, resolved in Rust over the store dictionary
    /// (`Resolver::facts_table`, byte-equal to the TS bridge's table). Null when the dictionary
    /// exceeds the pair cap, as the TS bridge declines. The resolver reads the same
    /// `<ccsDir>/models-dev-registry-cache.json` the TS refresh writes.
    fn pricing_facts_json(&self) -> Result<String, String> {
        let engine = self
            .engine()
            .ok_or_else(|| "invalid: the store is not open".to_string())?;
        let dict = aac_store::activity::models(&engine).map_err(|e| e.to_string())?;
        let strings = |key: &str| -> Vec<String> {
            dict[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default()
        };
        let models = strings("models");
        let routes = strings("routes");
        // The bridge's MAX_PRICING_PAIRS cap, counting the empty route facts_table adds.
        if models.len() * (routes.len() + 1) > 8_000 {
            return Ok("null".to_string());
        }
        let ccs_dir = self.boot.dir.parent().unwrap_or(&self.boot.dir);
        let mut resolver = aac_views::pricing::Resolver::new(ccs_dir);
        let table = resolver.facts_table(&models, &routes);
        Ok(Value::Object(table).to_string())
    }

    /// TS's `open` once per boot: the engine the boot opened is the answer (its status, as
    /// dispatch's `open` returns), never a second open. Another folder is refused, and a
    /// slot left empty (a close, a failed boot open) opens through dispatch as usual.
    fn open_idempotent(&self, args: &str) -> Result<String, String> {
        #[derive(Deserialize)]
        struct Dir {
            dir: String,
        }
        let dir: Dir = serde_json::from_str(args)
            .map_err(|e| aac_store::error::Error::new("invalid", e.to_string()).to_string())?;
        let canonical = |path: &std::path::Path| {
            std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
        };
        if canonical(std::path::Path::new(&dir.dir)) != canonical(&self.boot.dir) {
            return Err("invalid: the store dir differs from the boot's store dir".to_string());
        }
        match self.engine() {
            Some(engine) => {
                self.seed_failure_floors(&engine);
                ops::call(&engine, "status", "{}")
                    .map(|value| value.to_string())
                    .map_err(|e| e.to_string())
            }
            None => {
                let opened = ops::dispatch(&self.slot, "open", args).map_err(|e| e.to_string())?;
                if let Some(engine) = self.engine() {
                    self.seed_failure_floors(&engine);
                }
                Ok(opened)
            }
        }
    }

    fn jobs_json(&self) -> String {
        let engine = self.engine();
        // The persisted failure records survive a host restart; each is one meta write
        // holding a monotonic sequence plus code/time, so a backward wall clock cannot
        // hide a new failure (round 3 L1). When a meta write fails, the newer in-memory
        // record keeps the failure visible with `last*Persisted: false`.
        let (error, share) = engine
            .as_ref()
            .map(|engine| {
                (
                    failure_record(engine, "ts:host_job_last_error"),
                    failure_record(engine, "ts:host_job_last_share_failure"),
                )
            })
            .unwrap_or((Value::Null, Value::Null));
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        let persisted_error_seq = error["seq"].as_u64().unwrap_or(0);
        state.jobs.known_error_seq = state.jobs.known_error_seq.max(persisted_error_seq);
        let (error_code, error_ms, error_seq, error_persisted) = match &state.jobs.last_error_record
        {
            Some(mem) if mem.seq > persisted_error_seq => (
                mem.code.as_deref().map_or(Value::Null, |c| json!(c)),
                json!(mem.ms),
                json!(mem.seq),
                json!(mem.persisted),
            ),
            _ => (
                error["code"].clone(),
                error["ms"].clone(),
                error["seq"].clone(),
                error["seq"].as_u64().map_or(Value::Null, |_| json!(true)),
            ),
        };
        let persisted_share_seq = share["seq"].as_u64().unwrap_or(0);
        state.jobs.known_share_seq = state.jobs.known_share_seq.max(persisted_share_seq);
        let (share_ms, share_seq, share_persisted) = match &state.jobs.last_share_record {
            Some(mem) if mem.seq > persisted_share_seq => {
                (json!(mem.ms), json!(mem.seq), json!(mem.persisted))
            }
            _ => (
                share["ms"].clone(),
                share["seq"].clone(),
                share["seq"].as_u64().map_or(Value::Null, |_| json!(true)),
            ),
        };
        json!({
            "open": engine.is_some(),
            "bootError": state.boot_error,
            "instance": state.instance,
            "jobs": {
                "passes": state.jobs.passes,
                "lastPassMs": state.jobs.last_pass_ms,
                "lastError": state.jobs.last_error,
                "lastErrorCode": error_code,
                "lastErrorMs": error_ms,
                "lastErrorSeq": error_seq,
                "lastErrorPersisted": error_persisted,
                "lastShareFailedMs": share_ms,
                "lastShareSeq": share_seq,
                "lastSharePersisted": share_persisted,
            },
        })
        .to_string()
    }

    /// `/api/store/status` from the Engine in process (R3): the writer's own `status`,
    /// admitted by the same shape check the snapshot path uses. No engine, no answer.
    pub(crate) fn status_value(&self) -> Result<Value, Decline> {
        self.wait_ready();
        let Some(engine) = self.engine() else {
            let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            let error = state.boot_error.as_deref().ok_or(Decline::Unsupported)?;
            let reason = error.split_once(": ").map_or("unknown", |(code, _)| code);
            return Ok(json!({"state": "unavailable", "reason": reason}));
        };
        let status = ops::call(&engine, "status", "{}").map_err(|_| Decline::Internal)?;
        aac_store::read_status::from_writer(&status).ok_or(Decline::Unsupported)
    }

    /// Run the same periodic maintenance and backups for either transport.
    pub(crate) async fn jobs(self: Arc<Self>, stop: Arc<AtomicBool>) {
        tokio::time::sleep(Duration::from_millis(FIRST_PASS_MS)).await;
        while !stop.load(Ordering::Acquire) {
            let host = Arc::clone(&self);
            let _ = tokio::task::spawn_blocking(move || host.job_pass(aac_store::engine::now_ms()))
                .await;
            tokio::time::sleep(Duration::from_millis(MAINTAIN_EVERY_MS)).await;
        }
    }

    /// One maintenance pass, TS's loop body with its intervals and meta keys: maintain
    /// (weekly verify and compaction by `last_verify_ms`), then a daily backup to
    /// `store/backups` and the share copy, with a redb snapshot weekly by
    /// `ts:last_redb_backup_ms`. Runs on a blocking thread; a closed store is a no-op.
    pub(crate) fn job_pass(&self, now: u64) {
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.job_pass_inner(now)));
        let (error, share_failed) = match outcome {
            Ok(Ok(share_failed)) => (None, share_failed),
            Ok(Err(error)) => (Some(error), false),
            Err(_) => (
                Some("panic: the store stopped this call".to_string()),
                false,
            ),
        };
        self.record_job_outcome(now, error, share_failed);
    }

    fn record_job_outcome(&self, now: u64, error: Option<String>, share_failed: bool) {
        let (prior_error_seq, prior_share_seq) = {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            state.jobs.passes += 1;
            state.jobs.last_pass_ms = Some(now);
            if let Some(error) = &error {
                state.jobs.last_error = Some(error.clone());
            }
            (
                state
                    .jobs
                    .last_error_record
                    .as_ref()
                    .map_or(0, |r| r.seq)
                    .max(state.jobs.known_error_seq),
                state
                    .jobs
                    .last_share_record
                    .as_ref()
                    .map_or(0, |r| r.seq)
                    .max(state.jobs.known_share_seq),
            )
        };
        // R3 review L1: the operator warning must survive a host restart. Persist one
        // record per failure kind — a monotonic sequence with the fixed code and time
        // (never the message, which can name private paths). TS's hosted-jobs watch reads
        // them back through `hostJobs` and warns as it does for the in-process loop; the
        // sequence, not the wall time, identifies a new failure (round 3 L1).
        // If `set_meta` fails, keep the failure record in memory so `hostJobs` still names
        // it with a monotonic sequence and `last*Persisted: false`.
        let engine = self.engine();
        let stamp = |key: &str, record: Value| -> bool {
            let Some(engine) = engine.as_ref() else {
                return false;
            };
            match engine.set_meta(key, &record.to_string()) {
                Ok(()) => true,
                Err(error) => {
                    tracing::warn!(target: "aac_http", "store job diagnostic not persisted: {error}");
                    false
                }
            }
        };
        let next_error = error.as_ref().map(|error| {
            let persisted_seq = engine
                .as_ref()
                .map_or(0, |e| persisted_failure_seq(e, "ts:host_job_last_error"));
            let seq = persisted_seq.max(prior_error_seq) + 1;
            let code = job_error_code(error);
            let persisted = stamp(
                "ts:host_job_last_error",
                json!({"seq": seq, "code": &code, "ms": now}),
            );
            JobFailure {
                seq,
                code: Some(code),
                ms: now,
                persisted,
            }
        });
        let next_share = share_failed.then(|| {
            let persisted_seq = engine.as_ref().map_or(0, |e| {
                persisted_failure_seq(e, "ts:host_job_last_share_failure")
            });
            let seq = persisted_seq.max(prior_share_seq) + 1;
            let persisted = stamp(
                "ts:host_job_last_share_failure",
                json!({"seq": seq, "ms": now}),
            );
            JobFailure {
                seq,
                code: None,
                ms: now,
                persisted,
            }
        });
        if next_error.is_some() || next_share.is_some() {
            let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(record) = next_error {
                state.jobs.known_error_seq = state.jobs.known_error_seq.max(record.seq);
                state.jobs.last_error_record = Some(record);
            }
            if let Some(record) = next_share {
                state.jobs.known_share_seq = state.jobs.known_share_seq.max(record.seq);
                state.jobs.last_share_record = Some(record);
            }
        }
    }

    fn job_pass_inner(&self, now: u64) -> Result<bool, String> {
        let Some(engine) = self.engine() else {
            return Ok(false);
        };
        let at = now as i64;
        let status = ops::call(&engine, "status", "{}").map_err(|e| e.to_string())?;
        let weekly = at - meta_ms(&status, "last_verify_ms") >= VERIFY_EVERY_MS as i64;
        ops::call(
            &engine,
            "maintain",
            &json!({ "nowMs": now, "verify": weekly, "compact": weekly }).to_string(),
        )
        .map_err(|e| e.to_string())?;
        if at - meta_ms(&status, "last_backup_ms") < BACKUP_EVERY_MS as i64 {
            return Ok(false);
        }
        let last_redb: i64 = engine
            .get_meta("ts:last_redb_backup_ms")
            .map_err(|e| e.to_string())?
            .and_then(|text| text.parse().ok())
            .unwrap_or(0);
        let with_redb = at - last_redb >= REDB_BACKUP_EVERY_MS as i64;
        let share = self.boot.share_root(&engine.instance_id);
        let mut roots = vec![engine.dir.join("backups")];
        if let Some(share) = &share {
            roots.push(share.clone());
        }
        let result = ops::call(
            &engine,
            "backup",
            &json!({ "roots": roots, "withRedb": with_redb }).to_string(),
        )
        .map_err(|e| e.to_string())?;
        if with_redb {
            engine
                .set_meta("ts:last_redb_backup_ms", &now.to_string())
                .map_err(|e| e.to_string())?;
        }
        let share_failed = share.is_some()
            && !result["mirrors"]
                .as_array()
                .is_some_and(|mirrors| mirrors.iter().all(|m| m["ok"] == true));
        Ok(share_failed)
    }

    /// A clean close when the service ends, whatever TS managed to ask first: producers may
    /// still be stopping, but the logs seal and the shutdown is marked clean (dispatch's
    /// `close` on an empty slot is already a no-op).
    pub(crate) fn close_engine(&self) {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let engine = self.slot.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(engine) = engine {
                let _ = engine.close();
            }
        }));
    }
}

/// A job failure's fixed code, as TS's `storeErrorCode` reads it: the `<code>:` prefix when
/// it is one, else `unknown`. The message itself can name private paths and never persists.
fn job_error_code(error: &str) -> String {
    let code = error.split(": ").next().unwrap_or("");
    if !code.is_empty() && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') {
        code.to_string()
    } else {
        "unknown".to_string()
    }
}

/// A persisted failure record, or null when none exists: one meta value holding the
/// monotonic sequence and the code/time, written as one record so a crash cannot split them.
fn failure_record(engine: &Engine, key: &str) -> Value {
    engine
        .get_meta(key)
        .ok()
        .flatten()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The last persisted failure sequence for a record key, or 0 when none exists.
fn persisted_failure_seq(engine: &Engine, key: &str) -> u64 {
    failure_record(engine, key)["seq"].as_u64().unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boot(dir: PathBuf) -> StoreBoot {
        StoreBoot {
            binding_identity: aac_store::build_identity(),
            dir,
            retention_days: 0,
            cache_bytes: None,
            activity_retention_days: None,
            snapshot_directory: None,
            share_backup_dir: None,
            share_backup_off: false,
        }
    }

    fn observation(t: u64) -> String {
        json!({ "observations": [{
            "account": { "identity": "a".repeat(64), "provider": "codex", "account_id": "codex:first",
                         "plan": null, "platform": "ubuntu", "source": "host test", "status": "ok", "is_active": true },
            "sampled_at_ms": t, "observed_at_ms": t, "import_ref": null,
            "windows": [{ "identity": "[\"five_hour\",null,null]", "key": "five_hour", "label": "5 hour",
                          "used_pct": 10, "remaining_pct": 90, "reset_at_ms": t + 3_600_000 }],
        }]})
        .to_string()
    }

    #[test]
    fn the_host_opens_once_and_answers_the_napi_contract() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let dir = root.path().join("store");
        let host = StoreHost::new(boot(dir.clone()));
        host.open_at_boot();
        // TS's open on the opened boot is its status, never already_open.
        let opened = host.call_blocking("open", &json!({ "dir": dir }).to_string())?;
        assert!(opened.contains("\"state\":\"ready\""), "{opened}");
        // Another folder is refused before dispatch can open it.
        let other = root.path().join("other");
        let refused = host.call_blocking("open", &json!({ "dir": other }).to_string());
        assert!(
            matches!(&refused, Err(e) if e.starts_with("invalid:")),
            "{refused:?}"
        );
        // While the host holds the store, the kernel lock refuses a second writer.
        let second = ops::open_engine(&dir.to_string_lossy(), None, None);
        assert!(second.is_err_and(|e| e.code == "lock_held"));
        // The write/read contract round-trips, NAPI-shaped errors included.
        let t = 1_790_000_000_000u64;
        host.call_blocking("record", &observation(t))?;
        let history = host.call_blocking(
            "historyRecords",
            &json!({ "fromMs": 0, "toMs": t + 3_600_000, "nowMs": t, "thinning": { "kind": "none" } })
                .to_string(),
        )?;
        assert!(history.contains("codex:first"), "{history}");
        let unknown = host.call_blocking("noSuchMethod", "{}");
        assert!(
            matches!(&unknown, Err(e) if e.starts_with("invalid:")),
            "{unknown:?}"
        );
        // Status from the engine in process is admitted, as the writer's snapshot is.
        assert!(host.status_value().is_ok());
        host.close_engine();
        assert!(host.status_value().is_err());
        // The lock went with the engine: the same folder opens again.
        assert!(ops::open_engine(&dir.to_string_lossy(), None, None).is_ok());
        Ok(())
    }

    #[test]
    fn a_job_pass_backs_up_and_stamps_the_ts_meta_keys() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let dir = root.path().join("store");
        let share = root.path().join("share-copy");
        let mut config = boot(dir.clone());
        config.share_backup_dir = Some(share.clone());
        let host = StoreHost::new(config);
        host.open_at_boot();
        let now = aac_store::engine::now_ms();
        host.job_pass(now);
        let engine = host.engine().ok_or("engine")?;
        assert!(
            engine
                .get_meta("last_backup_ms")
                .map_err(|e| e.to_string())?
                .is_some_and(|v| v.parse::<u64>().is_ok_and(|ms| ms > 0)),
            "the daily backup stamped last_backup_ms"
        );
        assert_eq!(
            engine
                .get_meta("ts:last_redb_backup_ms")
                .map_err(|e| e.to_string())?,
            Some(now.to_string()),
            "the weekly redb stamp is TS's key, so a fallback resumes on it"
        );
        assert!(dir.join("backups").join("sets").is_dir());
        assert!(share.join("sets").is_dir(), "the share copy was mirrored");
        let jobs = host.jobs_json();
        assert!(jobs.contains("\"passes\":1"), "{jobs}");
        let jobs: Value = serde_json::from_str(&jobs).map_err(|e| e.to_string())?;
        assert_eq!(
            jobs["jobs"]["lastErrorMs"],
            Value::Null,
            "a healthy pass names no failure"
        );
        assert_eq!(jobs["jobs"]["lastShareFailedMs"], Value::Null);
        assert_eq!(jobs["jobs"]["lastErrorSeq"], Value::Null);
        assert_eq!(jobs["jobs"]["lastShareSeq"], Value::Null);
        host.close_engine();
        Ok(())
    }

    #[test]
    fn a_failed_share_backup_keeps_its_warning_across_a_host_restart() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let dir = root.path().join("store");
        // A file where the share folder should be: the local backup succeeds, the mirror fails.
        let share = root.path().join("share-copy");
        std::fs::write(&share, b"not a folder").map_err(|e| e.to_string())?;
        let mut config = boot(dir.clone());
        config.share_backup_dir = Some(share.clone());
        let host = StoreHost::new(config);
        host.open_at_boot();
        let now = aac_store::engine::now_ms();
        host.job_pass(now);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareFailedMs"], json!(now));
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(1));
        assert_eq!(
            jobs["jobs"]["lastErrorMs"],
            Value::Null,
            "the pass itself finished"
        );
        assert!(
            dir.join("backups").join("sets").is_dir(),
            "the local backup is intact"
        );
        // Restart: the failure record survives in the store, not only this boot's memory.
        host.close_engine();
        drop(host);
        let reopened = StoreHost::new(boot(dir.clone()));
        reopened.open_at_boot();
        let jobs: Value = serde_json::from_str(&reopened.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["passes"], json!(0), "a fresh boot ran no pass");
        assert_eq!(jobs["jobs"]["lastShareFailedMs"], json!(now));
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(1));
        reopened.close_engine();
        Ok(())
    }

    #[test]
    fn a_failed_job_pass_persists_its_fixed_code_not_the_message() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let dir = root.path().join("store");
        // A file where the backup root should be: the pass fails before any mirror.
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("backups"), b"not a folder").map_err(|e| e.to_string())?;
        let host = StoreHost::new(boot(dir.clone()));
        host.open_at_boot();
        let now = aac_store::engine::now_ms();
        host.job_pass(now);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now));
        let code = jobs["jobs"]["lastErrorCode"].as_str().ok_or("code")?;
        assert!(
            !code.is_empty() && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'),
            "{code}"
        );
        assert_eq!(jobs["jobs"]["lastShareFailedMs"], Value::Null);
        // The persisted record is one meta value: sequence, fixed code and time — no
        // private path reaches it.
        let engine = host.engine().ok_or("engine")?;
        let persisted = engine
            .get_meta("ts:host_job_last_error")
            .map_err(|e| e.to_string())?
            .ok_or("persisted")?;
        let record: Value = serde_json::from_str(&persisted).map_err(|e| e.to_string())?;
        assert_eq!(record["code"], json!(code));
        assert_eq!(record["ms"], json!(now));
        assert_eq!(record["seq"], json!(1));
        assert!(
            !persisted.contains(root.path().to_str().ok_or("root")?),
            "no private path: {persisted}"
        );
        // The kernel lock lives until every Arc drops: release this one before the restart.
        drop(engine);
        // Restart keeps the code and time.
        host.close_engine();
        drop(host);
        let reopened = StoreHost::new(boot(dir.clone()));
        reopened.open_at_boot();
        let jobs: Value = serde_json::from_str(&reopened.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now));
        assert_eq!(jobs["jobs"]["lastErrorCode"], json!(code));
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(1));
        reopened.close_engine();
        Ok(())
    }

    #[test]
    fn a_backward_clock_still_names_a_new_failure() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        // The error record: a file where the backup root should be fails every pass,
        // whatever the clock says. The second pass is stamped earlier — a real backward
        // step, not a 1970 fixture time the seal path would read as a different era.
        let now = aac_store::engine::now_ms();
        let dir = root.path().join("store");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("backups"), b"not a folder").map_err(|e| e.to_string())?;
        let host = StoreHost::new(boot(dir.clone()));
        host.open_at_boot();
        host.job_pass(now);
        host.job_pass(now - 1_000);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(2));
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now - 1_000));
        // The sequence survives the restart too.
        host.close_engine();
        drop(host);
        let reopened = StoreHost::new(boot(dir.clone()));
        reopened.open_at_boot();
        let jobs: Value = serde_json::from_str(&reopened.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(2));
        reopened.close_engine();
        // The share record: mirror failures on due passes, the second stamped earlier.
        let dir = root.path().join("store-share");
        let share = root.path().join("share-copy");
        std::fs::write(&share, b"not a folder").map_err(|e| e.to_string())?;
        let mut config = boot(dir.clone());
        config.share_backup_dir = Some(share.clone());
        let host = StoreHost::new(config);
        host.open_at_boot();
        host.job_pass(now + DAY_MS);
        let engine = host.engine().ok_or("engine")?;
        engine
            .set_meta("last_backup_ms", "0")
            .map_err(|e| e.to_string())?;
        drop(engine);
        host.job_pass(now + DAY_MS - 1_000);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(2));
        assert_eq!(
            jobs["jobs"]["lastShareFailedMs"],
            json!(now + DAY_MS - 1_000)
        );
        host.close_engine();
        Ok(())
    }

    #[test]
    fn a_failed_meta_write_still_publishes_the_job_failure_with_a_monotonic_sequence()
    -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let now = aac_store::engine::now_ms();
        // Fresh host: closing the engine's db while the host slot keeps it makes both the
        // pass and set_meta fail, as when aac.redb refuses writes.
        let dir = root.path().join("store-unpersisted");
        let host = StoreHost::new(boot(dir));
        host.open_at_boot();
        let engine = host.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);

        host.job_pass(now);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(1), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorCode"], json!("closed"), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(false), "{jobs}");

        // A second unpersisted failure advances the sequence monotonically.
        host.job_pass(now + 1_000);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(2), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now + 1_000), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(false), "{jobs}");
        host.close_engine();

        // After a persisted failure (seq 1), an unpersisted failure advances to seq 2.
        let dir = root.path().join("store-after-persisted");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("backups"), b"not a folder").map_err(|e| e.to_string())?;
        let host = StoreHost::new(boot(dir));
        host.open_at_boot();
        host.job_pass(now);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(1), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(true), "{jobs}");

        let engine = host.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);
        host.job_pass(now + 2_000);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(2), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now + 2_000), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorCode"], json!("closed"), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(false), "{jobs}");
        host.close_engine();
        Ok(())
    }

    #[test]
    fn a_reopened_host_names_a_new_instance_and_keeps_its_persisted_failure() -> Result<(), String>
    {
        // M3: the watch resets its cursors when this identity changes, so a reopened host's
        // persisted failure 1 is a baseline, not a failure already reported.
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let now = aac_store::engine::now_ms();
        let dir = root.path().join("store-instance");
        std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
        std::fs::write(dir.join("backups"), b"not a folder").map_err(|e| e.to_string())?;
        let host = StoreHost::new(boot(dir.clone()));
        host.open_at_boot();
        host.job_pass(now);
        let first: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        let instance = first["instance"].as_str().ok_or("instance")?.to_string();
        assert!(!instance.is_empty(), "{first}");
        let again: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(again["instance"], json!(instance), "{again}");
        host.close_engine();
        drop(host);

        let reopened = StoreHost::new(boot(dir));
        reopened.open_at_boot();
        let restarted: Value =
            serde_json::from_str(&reopened.jobs_json()).map_err(|e| e.to_string())?;
        assert_ne!(restarted["instance"], json!(instance), "{restarted}");
        assert_eq!(restarted["jobs"]["lastErrorSeq"], json!(1), "{restarted}");
        assert_eq!(restarted["jobs"]["lastErrorPersisted"], json!(true));
        reopened.close_engine();
        Ok(())
    }

    #[test]
    fn a_restarted_host_keeps_unpersisted_failure_sequences_monotonic() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let now = aac_store::engine::now_ms();

        // Case 1 (seq 1 -> 2): Boot 1 persists seq 1; Boot 2 restarts via open_at_boot,
        // then the engine closes before the next failing pass.
        let dir1 = root.path().join("store-restart-seq1");
        std::fs::create_dir_all(&dir1).map_err(|e| e.to_string())?;
        std::fs::write(dir1.join("backups"), b"not a folder").map_err(|e| e.to_string())?;
        let host1 = StoreHost::new(boot(dir1.clone()));
        host1.open_at_boot();
        host1.job_pass(now);
        let jobs: Value = serde_json::from_str(&host1.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(1), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(true), "{jobs}");
        host1.close_engine();
        drop(host1);

        let host2 = StoreHost::new(boot(dir1));
        host2.open_at_boot();
        let jobs: Value = serde_json::from_str(&host2.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(1), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(true), "{jobs}");
        let engine = host2.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);
        host2.job_pass(now + 2_000);
        let jobs: Value = serde_json::from_str(&host2.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(2), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorCode"], json!("closed"), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now + 2_000), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(false), "{jobs}");
        host2.close_engine();

        // Case 2 (seq 2 -> 3): Boot 1 persists seq 1 and seq 2; Boot 2 opens via
        // open_idempotent (after a locked open_at_boot), then the engine closes before
        // the next failing pass — sequence must advance to 3, never regress to 1.
        let dir2 = root.path().join("store-restart-seq2");
        std::fs::create_dir_all(&dir2).map_err(|e| e.to_string())?;
        std::fs::write(dir2.join("backups"), b"not a folder").map_err(|e| e.to_string())?;
        let host1 = StoreHost::new(boot(dir2.clone()));
        host1.open_at_boot();
        host1.job_pass(now);
        host1.job_pass(now + 1_000);
        let jobs: Value = serde_json::from_str(&host1.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(2), "{jobs}");
        host1.close_engine();
        drop(host1);

        let holder =
            ops::open_engine(&dir2.to_string_lossy(), None, None).map_err(|e| e.to_string())?;
        let host2 = StoreHost::new(boot(dir2.clone()));
        host2.open_at_boot();
        assert!(host2.engine().is_none(), "boot open fails while lock held");
        holder.close().map_err(|e| e.to_string())?;
        drop(holder);
        host2.call_blocking("open", &json!({ "dir": dir2 }).to_string())?;
        // Close immediately after open_idempotent without calling jobs_json() first,
        // proving open_idempotent itself seeded the monotonic floor.
        let engine = host2.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);
        host2.job_pass(now + 3_000);
        let jobs: Value = serde_json::from_str(&host2.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(3), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorCode"], json!("closed"), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorMs"], json!(now + 3_000), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(false), "{jobs}");
        host2.close_engine();

        // Case 3 (jobs_json ratchets the floor): a persisted failure written after boot
        // is observed via jobs_json(), raising the floor so a later closed-engine pass
        // still advances monotonically.
        let dir3 = root.path().join("store-ratchet");
        let host3 = StoreHost::new(boot(dir3));
        host3.open_at_boot();
        let engine = host3.engine().ok_or("engine")?;
        engine
            .set_meta(
                "ts:host_job_last_error",
                &json!({"seq": 5, "code": "io", "ms": now}).to_string(),
            )
            .map_err(|e| e.to_string())?;
        drop(engine);
        let jobs: Value = serde_json::from_str(&host3.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(5), "{jobs}");
        let engine = host3.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);
        host3.job_pass(now + 4_000);
        let jobs: Value = serde_json::from_str(&host3.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastErrorSeq"], json!(6), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorPersisted"], json!(false), "{jobs}");
        host3.close_engine();
        Ok(())
    }

    #[test]
    fn a_failed_meta_write_still_publishes_the_share_failure_with_a_monotonic_sequence()
    -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let now = aac_store::engine::now_ms();

        // Fresh host: closed engine makes set_meta fail on a share failure; sequence
        // starts at 1 and advances monotonically with lastSharePersisted: false.
        let dir_fresh = root.path().join("store-share-unpersisted");
        let host = StoreHost::new(boot(dir_fresh));
        host.open_at_boot();
        let engine = host.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);

        host.record_job_outcome(now, None, true);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(1), "{jobs}");
        assert_eq!(jobs["jobs"]["lastShareFailedMs"], json!(now), "{jobs}");
        assert_eq!(jobs["jobs"]["lastSharePersisted"], json!(false), "{jobs}");
        assert_eq!(jobs["jobs"]["lastErrorSeq"], Value::Null, "{jobs}");

        host.record_job_outcome(now + 1_000, None, true);
        let jobs: Value = serde_json::from_str(&host.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(2), "{jobs}");
        assert_eq!(
            jobs["jobs"]["lastShareFailedMs"],
            json!(now + 1_000),
            "{jobs}"
        );
        assert_eq!(jobs["jobs"]["lastSharePersisted"], json!(false), "{jobs}");
        host.close_engine();

        // Restarted host after persisted share failures (seq 1 -> 2, and seq 2 -> 3):
        // Boot 1 persists seq 1, Boot 2 seeds floor and advances to seq 2 when closed.
        let dir_restart = root.path().join("store-share-restart");
        let share = root.path().join("share-copy-file");
        std::fs::write(&share, b"not a folder").map_err(|e| e.to_string())?;
        let mut config = boot(dir_restart.clone());
        config.share_backup_dir = Some(share.clone());
        let host1 = StoreHost::new(config);
        host1.open_at_boot();
        host1.job_pass(now);
        let jobs: Value = serde_json::from_str(&host1.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(1), "{jobs}");
        assert_eq!(jobs["jobs"]["lastSharePersisted"], json!(true), "{jobs}");
        host1.close_engine();
        drop(host1);

        let host2 = StoreHost::new(boot(dir_restart.clone()));
        host2.open_at_boot();
        let engine = host2.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);
        host2.record_job_outcome(now + 2_000, None, true);
        let jobs: Value = serde_json::from_str(&host2.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(2), "{jobs}");
        assert_eq!(
            jobs["jobs"]["lastShareFailedMs"],
            json!(now + 2_000),
            "{jobs}"
        );
        assert_eq!(jobs["jobs"]["lastSharePersisted"], json!(false), "{jobs}");
        host2.close_engine();
        drop(host2);

        // Second persisted share failure on Boot 3 (seq 2), then Boot 4 closed -> seq 3.
        let mut config3 = boot(dir_restart.clone());
        config3.share_backup_dir = Some(share);
        let host3 = StoreHost::new(config3);
        host3.open_at_boot();
        let engine = host3.engine().ok_or("engine")?;
        engine
            .set_meta("last_backup_ms", "0")
            .map_err(|e| e.to_string())?;
        drop(engine);
        host3.job_pass(now + DAY_MS);
        let jobs: Value = serde_json::from_str(&host3.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(2), "{jobs}");
        assert_eq!(jobs["jobs"]["lastSharePersisted"], json!(true), "{jobs}");
        host3.close_engine();
        drop(host3);

        let host4 = StoreHost::new(boot(dir_restart));
        host4.open_at_boot();
        let engine = host4.engine().ok_or("engine")?;
        engine.close().map_err(|e| e.to_string())?;
        drop(engine);
        host4.record_job_outcome(now + DAY_MS + 1_000, None, true);
        let jobs: Value = serde_json::from_str(&host4.jobs_json()).map_err(|e| e.to_string())?;
        assert_eq!(jobs["jobs"]["lastShareSeq"], json!(3), "{jobs}");
        assert_eq!(
            jobs["jobs"]["lastShareFailedMs"],
            json!(now + DAY_MS + 1_000),
            "{jobs}"
        );
        assert_eq!(jobs["jobs"]["lastSharePersisted"], json!(false), "{jobs}");
        host4.close_engine();
        Ok(())
    }

    #[test]
    fn the_share_root_derives_like_ts_or_stays_off() -> Result<(), String> {
        let root = tempfile::tempdir().map_err(|e| e.to_string())?;
        let snapshot = root.path().join("snapshots");
        let mut config = boot(root.path().join("store"));
        config.snapshot_directory = Some(snapshot.clone());
        let instance = "0123456789abcdef0123456789abcdef";
        // No share/ai-account-center folder yet: no derivation, nothing created.
        assert_eq!(config.share_root(instance), None);
        let project = root.path().join("share").join("ai-account-center");
        std::fs::create_dir_all(&project).map_err(|e| e.to_string())?;
        assert_eq!(
            config.share_root(instance),
            Some(project.join("store-backups").join(instance))
        );
        config.share_backup_off = true;
        assert_eq!(config.share_root(instance), None);
        config.share_backup_off = false;
        assert_eq!(config.share_root("not-an-instance"), None);
        Ok(())
    }
}
