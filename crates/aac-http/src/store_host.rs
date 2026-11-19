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
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::{Arc, Condvar, Mutex, mpsc};

/// Store calls in flight at most (L1's store pool size). Unlike the read pools, a full pool
/// queues: the NAPI binding queues on libuv's pool too, and a producer must not lose a
/// write to a `busy` decline the in-process store never gave.
const STORE_PERMITS: usize = 4;
/// The longest store request body (`captureReports` already caps its batch at 8 MiB in ops).
pub(crate) const STORE_INPUT_CAP: usize = 16 * 1024 * 1024;

const MINUTE_MS: u64 = 60_000;
const DAY_MS: u64 = 86_400_000;
/// TS's maintenance cadence (`store-service.ts`): first pass minutes after start, then
/// every 15 minutes; backup daily; verify, compaction and the redb snapshot weekly.
pub(crate) const FIRST_PASS_MS: u64 = 3 * MINUTE_MS;
pub(crate) const MAINTAIN_EVERY_MS: u64 = 15 * MINUTE_MS;
const BACKUP_EVERY_MS: u64 = DAY_MS;
const REDB_BACKUP_EVERY_MS: u64 = 7 * DAY_MS;
const VERIFY_EVERY_MS: u64 = 7 * DAY_MS;

/// The boot's store config, resolved once by TS at start: the open arguments it would have
/// given the NAPI module, plus the backup job's share inputs. Absent in the boot JSON, the
/// store stays in the TS process exactly as before.
#[derive(Clone, Debug, Deserialize)]
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

/// The job loop's record, readable through the `hostJobs` method (never in `status`, whose
/// shape the goldens pin).
#[derive(Default)]
struct Jobs {
    passes: u64,
    last_pass_ms: Option<u64>,
    last_error: Option<String>,
}

struct State {
    /// The boot open finished (successfully or not); socket calls wait for it.
    ready: bool,
    boot_error: Option<String>,
    jobs: Jobs,
}

/// One queued store call: the method contract plus the channel that carries its answer back
/// to the async handler. The reply is dropped (the caller hung up) without disturbing the run.
struct StoreCall {
    method: String,
    args: String,
    reply: tokio::sync::oneshot::Sender<Result<String, String>>,
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

impl StoreHost {
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
                jobs: Jobs::default(),
            }),
            ready_changed: Condvar::new(),
        });
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
                        let result = host.call_blocking(&call.method, &call.args);
                        let _ = call.reply.send(result);
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
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.ready = true;
        state.boot_error = error.clone();
        drop(state);
        self.ready_changed.notify_all();
        if let Some(error) = &error {
            tracing::warn!(target: "aac_http", "store host open failed: {error}");
        }
    }

    fn wait_ready(&self) {
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
            .send(StoreCall {
                method,
                args,
                reply,
            })
            .map_err(|_| "service_down: the store host is stopping".to_string())?;
        answer
            .await
            .map_err(|_| "service_down: the store host is stopping".to_string())?
    }

    pub(crate) fn record_native_account_usage(
        &self,
        cache: &crate::account_usage::AccountUsageCache,
    ) {
        self.wait_ready();
        let now = aac_store::engine::now_ms() as i64;
        if let (Some(engine), Some(updated_at)) = (self.engine(), aac_num::ecma::to_iso_string(now))
            && let Ok(observations) = cache.store_observations(&updated_at, now, i64::MIN)
        {
            let _ = aac_store::usage::record(&engine, observations);
        }
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
            Some(engine) => ops::call(&engine, "status", "{}")
                .map(|value| value.to_string())
                .map_err(|e| e.to_string()),
            None => ops::dispatch(&self.slot, "open", args).map_err(|e| e.to_string()),
        }
    }

    fn jobs_json(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        // The persisted failure records survive a host restart; each is one meta write
        // holding a monotonic sequence plus code/time, so a backward wall clock cannot
        // hide a new failure (round 3 L1).
        let (error, share) = self
            .engine()
            .map(|engine| {
                (
                    failure_record(&engine, "ts:host_job_last_error"),
                    failure_record(&engine, "ts:host_job_last_share_failure"),
                )
            })
            .unwrap_or((Value::Null, Value::Null));
        json!({
            "open": self.engine().is_some(),
            "bootError": state.boot_error,
            "jobs": {
                "passes": state.jobs.passes,
                "lastPassMs": state.jobs.last_pass_ms,
                "lastError": state.jobs.last_error,
                "lastErrorCode": error["code"],
                "lastErrorMs": error["ms"],
                "lastErrorSeq": error["seq"],
                "lastShareFailedMs": share["ms"],
                "lastShareSeq": share["seq"],
            },
        })
        .to_string()
    }

    /// `/api/store/status` from the Engine in process (R3): the writer's own `status`,
    /// admitted by the same shape check the snapshot path uses. No engine, no answer.
    pub(crate) fn status_value(&self) -> Result<Value, Decline> {
        let engine = self.engine().ok_or(Decline::Unsupported)?;
        let status = ops::call(&engine, "status", "{}").map_err(|_| Decline::Internal)?;
        aac_store::read_status::from_writer(&status).ok_or(Decline::Unsupported)
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
        let mut state = self.state.lock().unwrap_or_else(|p| p.into_inner());
        state.jobs.passes += 1;
        state.jobs.last_pass_ms = Some(now);
        if let Some(error) = &error {
            state.jobs.last_error = Some(error.clone());
        }
        drop(state);
        // R3 review L1: the operator warning must survive a host restart. Persist one
        // record per failure kind — a monotonic sequence with the fixed code and time
        // (never the message, which can name private paths). TS's hosted-jobs watch reads
        // them back through `hostJobs` and warns as it does for the in-process loop; the
        // sequence, not the wall time, identifies a new failure (round 3 L1).
        if let Some(engine) = self.engine() {
            let stamp = |key: &str, record: Value| {
                if let Err(error) = engine.set_meta(key, &record.to_string()) {
                    tracing::warn!(target: "aac_http", "store job diagnostic not persisted: {error}");
                }
            };
            if let Some(error) = &error {
                let seq = next_failure_seq(&engine, "ts:host_job_last_error");
                stamp(
                    "ts:host_job_last_error",
                    json!({"seq": seq, "code": job_error_code(error), "ms": now}),
                );
            }
            if share_failed {
                let seq = next_failure_seq(&engine, "ts:host_job_last_share_failure");
                stamp(
                    "ts:host_job_last_share_failure",
                    json!({"seq": seq, "ms": now}),
                );
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

/// The next failure sequence for a record key: monotonic across passes and restarts,
/// independent of the wall clock (round 3 L1).
fn next_failure_seq(engine: &Engine, key: &str) -> u64 {
    failure_record(engine, key)["seq"].as_u64().unwrap_or(0) + 1
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
