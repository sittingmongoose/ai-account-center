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
use std::sync::{Arc, Condvar, Mutex};
use tokio::sync::Semaphore;

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
    last_share_failed_ms: Option<u64>,
}

struct State {
    /// The boot open finished (successfully or not); socket calls wait for it.
    ready: bool,
    boot_error: Option<String>,
    jobs: Jobs,
}

/// The hosted store: one Engine slot behind the NAPI method contract, plus its jobs.
pub(crate) struct StoreHost {
    boot: StoreBoot,
    slot: Slot,
    permits: Arc<Semaphore>,
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
        Arc::new(Self {
            boot,
            slot: Slot::new(None),
            permits: Arc::new(Semaphore::new(STORE_PERMITS)),
            state: Mutex::new(State {
                ready: false,
                boot_error: None,
                jobs: Jobs::default(),
            }),
            ready_changed: Condvar::new(),
        })
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

    fn engine(&self) -> Option<Arc<Engine>> {
        self.slot
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(Arc::clone)
    }

    /// One store call from the front door. Queued on the store permits, run on the blocking
    /// pool, with no deadline: `backup` and `maintain` take as long as they take, as they
    /// did in process. The answer is the dispatch's JSON text; an error is its
    /// `<code>: <message>`, exactly what the NAPI binding rejects with.
    pub(crate) async fn call(
        self: &Arc<Self>,
        method: String,
        args: String,
    ) -> Result<String, String> {
        let permit = self
            .permits
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| "service_down: the store host is stopping".to_string())?;
        let host = Arc::clone(self);
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            host.call_blocking(&method, &args)
        })
        .await
        .unwrap_or_else(|_| Err("panic: the store stopped this call".to_string()))
    }

    fn call_blocking(&self, method: &str, args: &str) -> Result<String, String> {
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            self.wait_ready();
            match method {
                "open" => self.open_idempotent(args),
                "hostJobs" => Ok(self.jobs_json()),
                "hostIdentity" => Ok(aac_store::build_identity().to_string()),
                _ => ops::dispatch(&self.slot, method, args).map_err(|e| e.to_string()),
            }
        }));
        outcome.unwrap_or_else(|_| Err("panic: the store stopped this call".to_string()))
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
        json!({
            "open": self.engine().is_some(),
            "bootError": state.boot_error,
            "jobs": {
                "passes": state.jobs.passes,
                "lastPassMs": state.jobs.last_pass_ms,
                "lastError": state.jobs.last_error,
                "lastShareFailedMs": state.jobs.last_share_failed_ms,
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
        if error.is_some() {
            state.jobs.last_error = error;
        }
        if share_failed {
            state.jobs.last_share_failed_ms = Some(now);
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
