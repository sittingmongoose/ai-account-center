//! The sign-in job journal: the agent-owned record that survives a TypeScript restart.
//!
//! Every state change is written atomically (0600, the one-file registry's `signin-jobs` row).
//! A later verb call reads it: `get` answers the journaled job, a running entry whose owner
//! died is recovered as `failed/server_restarted` and its stray CLI group is killed, and
//! finished entries are pruned after the runner's ten-minute retention. A second call's
//! `cancel` sets the journal flag the owning session polls, then kills and verifies the CLI.

use crate::signin_session::jobs::{ErrorCode, State, restarted_placeholder, valid_job_id};
use aac_core::{Cause, Failure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Finished jobs stay readable this long (`finishedRetentionMs`).
pub const RETENTION_MS: i64 = 10 * 60_000;
/// The issued-id ledger bound (`MAX_ISSUED_IDS`).
const MAX_ISSUED_IDS: usize = 512;
/// The journal's listing bound: the ledger plus room for running and retained jobs.
const LIST_CAP: usize = 2048;

/// One journaled job and the process facts recovery needs.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Entry {
    /// The job DTO in memory, projected to recovery fields before persistence.
    pub job: Value,
    /// The relay (or plain-pipe child) process id; the group leader the kill signals target.
    pub relay_pid: Option<u32>,
    /// The CLI's own pid once known (Linux reads the relay's children; the harness's fake
    /// CLI may also record it). Its session group is the CLI's pid.
    pub cli_pid: Option<u32>,
    /// The agent process that owns the running session.
    pub owner_pid: u32,
    /// The front-door process installing the login, when the CLI exited 0. The agent
    /// exits at the install boundary by design; only the front door's death ends it.
    #[serde(default)]
    pub front_door_pid: Option<u32>,
    /// The CLI exited 0 and the front door is installing the login.
    pub install_pending: bool,
    /// A second call asked this job to cancel; the owner polls the flag.
    pub cancel_requested: bool,
    /// When the job finished, in UTC milliseconds; null while it runs.
    pub finished_at: Option<i64>,
}

impl Entry {
    /// Whether the job has finished.
    pub fn finished(&self) -> bool {
        self.finished_at.is_some()
    }
}

// Preserve the DTO shape for recovery callers. The live event stream and front door
// keep the complete verification and result separately in memory.
fn recovery_job(job: &Value) -> Value {
    let result = job["result"]["accountId"].as_str().map(
        |account_id| json!({"accountId": account_id, "email": Value::Null, "plan": Value::Null}),
    );
    json!({
        "id": job["id"],
        "provider": job["provider"],
        "kind": job["kind"],
        "mode": job["mode"],
        "accountId": job["accountId"],
        "profileName": job["profileName"],
        "platform": job["platform"],
        "state": job["state"],
        "startedAt": job["startedAt"],
        "updatedAt": job["updatedAt"],
        "expiresAt": job["expiresAt"],
        "error": job["error"],
        "verification": Value::Null,
        "result": result,
    })
}

/// The journal under one CCS root.
pub struct Journal {
    root: PathBuf,
}

impl Journal {
    /// Open the journal under a CCS root; the ledger's claim proves the row and folders.
    pub fn open(root: &Path) -> Result<Self, Failure> {
        aac_files::registry::claim_as_agent("signin-jobs", root, Some("issued.json"))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn store(&self, name: &str) -> Result<aac_files::OwnedStore, Failure> {
        // The row's wildcard name is the whole file name, extension included.
        aac_files::registry::claim_as_agent(
            "signin-jobs",
            &self.root,
            Some(&format!("{name}.json")),
        )
    }

    fn dir(&self) -> PathBuf {
        self.root.join("signin-jobs")
    }

    /// Write one entry atomically. The owner's writes and a second call's cancel share the
    /// row's lease, so a busy claim retries briefly. The cancel flag and the discovered pids
    /// are monotonic: a write never clobbers a newer flag or pid another call recorded.
    /// Journal failures never fail the session itself.
    pub fn record(&self, entry: &Entry) -> Result<(), Failure> {
        let id = entry.job["id"].as_str().unwrap_or_default();
        if !valid_job_id(id) {
            return Err(Failure::new(Cause::Refused, "invalid journal job id"));
        }
        let mut merged = entry.clone();
        merged.job = recovery_job(&entry.job);
        if let Some(existing) = self.read(id) {
            merged.cancel_requested |= existing.cancel_requested;
            merged.relay_pid = merged.relay_pid.or(existing.relay_pid);
            merged.cli_pid = merged.cli_pid.or(existing.cli_pid);
        }
        let bytes = serde_json::to_vec(&merged)
            .map_err(|_| Failure::new(Cause::Bug, "journal encoding failed"))?;
        for attempt in 0..10 {
            match self.store(id).and_then(|store| store.write(&bytes)) {
                Ok(()) => return Ok(()),
                Err(failure) if failure.cause == Cause::Busy && attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(failure) => return Err(failure),
            }
        }
        Err(Failure::new(Cause::Busy, "journal writer busy"))
    }

    /// Read one entry; a damaged or absent file reads as absent.
    pub fn read(&self, job_id: &str) -> Option<Entry> {
        if !valid_job_id(job_id) {
            return None;
        }
        let bytes = self.store(job_id).ok()?.read().ok()?;
        let mut entry: Entry = serde_json::from_slice(&bytes).ok()?;
        entry.job = recovery_job(&entry.job);
        Some(entry)
    }

    /// Every readable entry, bounded; damaged files are skipped.
    pub fn entries(&self) -> Vec<Entry> {
        let mut out = Vec::new();
        let Ok(listing) = aac_files::read::entries(&self.dir(), LIST_CAP) else {
            return out;
        };
        for item in listing {
            let Ok(item) = item else { continue };
            let name = item.file_name();
            let Some(name) = name.to_str() else { continue };
            let Some(id) = name.strip_suffix(".json") else {
                continue;
            };
            if !valid_job_id(id) {
                continue;
            }
            if let Some(entry) = self.read(id) {
                out.push(entry);
            }
        }
        out
    }

    /// The issued-id ledger: ids this journal has started, newest last, bounded.
    fn issued(&self) -> Vec<String> {
        let Ok(store) = self.store("issued") else {
            return Vec::new();
        };
        let Ok(bytes) = store.read() else {
            return Vec::new();
        };
        serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| value["ids"].as_array().cloned())
            .map(|ids| {
                ids.into_iter()
                    .filter_map(|id| id.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Record a newly issued id.
    pub fn issue(&self, job_id: &str) {
        let mut ids = self.issued();
        ids.retain(|id| id != job_id);
        ids.push(job_id.to_string());
        while ids.len() > MAX_ISSUED_IDS {
            ids.remove(0);
        }
        if let Ok(store) = self.store("issued") {
            let _ = store.write(&serde_json::to_vec(&json!({ "ids": ids })).unwrap_or_default());
        }
    }

    /// Whether this journal ever issued the id.
    pub fn was_issued(&self, job_id: &str) -> bool {
        self.issued().iter().any(|id| id == job_id)
    }

    /// Running entries (not finished), after recovery.
    pub fn running(&self, now_ms: i64) -> Vec<Entry> {
        self.recover(now_ms);
        self.entries()
            .into_iter()
            .filter(|entry| !entry.finished())
            .collect()
    }

    /// Recover entries whose owner died: the job ends `failed/server_restarted` (or
    /// `cancelled` when a cancel was already requested) and any stray CLI group is killed.
    /// An installing entry is owned by the front door, not the exited agent.
    pub fn recover(&self, now_ms: i64) {
        for mut entry in self.entries() {
            if entry.finished() {
                continue;
            }
            let owner_alive = if entry.install_pending {
                entry
                    .front_door_pid
                    .is_some_and(crate::signin_session::process::process_alive)
            } else {
                crate::signin_session::process::process_alive(entry.owner_pid)
            };
            if owner_alive {
                continue;
            }
            kill_stray(&entry);
            let (state, code) = if entry.cancel_requested {
                (State::Cancelled, None)
            } else {
                (State::Failed, Some(ErrorCode::ServerRestarted))
            };
            entry.job["state"] = json!(state.word());
            entry.job["verification"] = Value::Null;
            entry.job["error"] = code
                .map(|code| json!({"code": code.word(), "message": code.message()}))
                .unwrap_or(Value::Null);
            entry.job["updatedAt"] = json!(crate::signin_session::jobs::iso(now_ms));
            entry.finished_at = Some(now_ms);
            entry.install_pending = false;
            let _ = self.record(&entry);
        }
    }

    /// Prune finished entries past the retention (the runner's ten minutes).
    pub fn prune(&self, now_ms: i64) {
        for entry in self.entries() {
            if let Some(finished_at) = entry.finished_at
                && now_ms - finished_at >= RETENTION_MS
            {
                let id = entry.job["id"].as_str().unwrap_or_default();
                if valid_job_id(id)
                    && let Ok(store) = self.store(id)
                {
                    let _ = store.trash();
                }
            }
        }
    }

    /// `get`: the journaled job, null for an issued id past retention, or the restarted
    /// placeholder for an id this journal never issued.
    pub fn get(&self, job_id: &str, now_ms: i64) -> Value {
        self.recover(now_ms);
        self.prune(now_ms);
        if let Some(entry) = self.read(job_id) {
            return json!({ "job": entry.job });
        }
        if self.was_issued(job_id) {
            return json!({ "job": Value::Null });
        }
        json!({ "job": restarted_placeholder(job_id, now_ms) })
    }

    /// `list`: the running jobs.
    pub fn list(&self, now_ms: i64) -> Value {
        let jobs: Vec<Value> = self
            .running(now_ms)
            .into_iter()
            .map(|entry| entry.job)
            .collect();
        json!({ "jobs": jobs })
    }

    /// Set the cancel flag the owning session polls. The owner's journal writes hold the
    /// same lease, so a busy claim retries briefly instead of failing the cancel.
    pub fn request_cancel(&self, job_id: &str) -> Result<Option<Entry>, Failure> {
        let Some(mut entry) = self.read(job_id) else {
            return Ok(None);
        };
        if entry.finished() {
            return Ok(Some(entry));
        }
        entry.cancel_requested = true;
        for attempt in 0..10 {
            match self.record(&entry) {
                Ok(()) => return Ok(Some(entry)),
                Err(failure) if failure.cause == Cause::Busy && attempt < 9 => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(failure) => return Err(failure),
            }
        }
        Err(Failure::new(Cause::Busy, "journal writer busy"))
    }

    /// `settle`: the front door's install outcome for an install-pending job.
    pub fn settle(
        &self,
        job_id: &str,
        state: State,
        result: Option<Value>,
        error: Option<ErrorCode>,
        now_ms: i64,
    ) -> Result<Value, Failure> {
        let Some(mut entry) = self.read(job_id) else {
            return Err(Failure::new(Cause::Refused, "unknown sign-in job"));
        };
        if !entry.install_pending || entry.finished() {
            return Err(Failure::new(
                Cause::Refused,
                "sign-in job is not installing",
            ));
        }
        if !state.finished() {
            return Err(Failure::new(Cause::Refused, "invalid settle state"));
        }
        entry.job["state"] = json!(state.word());
        entry.job["verification"] = Value::Null;
        entry.job["result"] = result.unwrap_or(Value::Null);
        entry.job["error"] = error
            .map(|code| json!({"code": code.word(), "message": code.message()}))
            .unwrap_or(Value::Null);
        entry.job["updatedAt"] = json!(crate::signin_session::jobs::iso(now_ms));
        entry.finished_at = Some(now_ms);
        entry.install_pending = false;
        self.record(&entry)?;
        Ok(json!({ "job": entry.job }))
    }
}

/// Kill a recovered entry's stray processes: the relay group, then the CLI's own session.
fn kill_stray(entry: &Entry) {
    #[cfg(unix)]
    {
        use crate::signin_session::process::{group_alive, signal_group};
        if let Some(pgid) = entry.relay_pid
            && group_alive(pgid)
        {
            signal_group(pgid, nix::sys::signal::Signal::SIGKILL);
        }
        if let Some(pid) = entry.cli_pid
            && crate::signin_session::process::process_alive(pid)
        {
            signal_group(pid, nix::sys::signal::Signal::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    {
        let _ = entry;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn entry(id: &str, state: &str, owner_pid: u32, finished_at: Option<i64>) -> Entry {
        Entry {
            job: json!({
                "id": id,
                "provider": "codex",
                "kind": "device-code",
                "mode": "add",
                "accountId": Value::Null,
                "profileName": "work",
                "platform": "ubuntu",
                "state": state,
                "verification": Value::Null,
                "result": Value::Null,
                "error": Value::Null,
                "startedAt": "2026-11-18T00:00:00.000Z",
                "updatedAt": "2026-11-18T00:00:00.000Z",
                "expiresAt": "2026-11-18T00:15:00.000Z",
            }),
            relay_pid: None,
            cli_pid: None,
            owner_pid,
            front_door_pid: None,
            install_pending: false,
            cancel_requested: false,
            finished_at,
        }
    }

    #[test]
    fn journal_round_trip_and_get_semantics() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let now = 1_763_424_000_000i64;
        let id = "job_0123456789abcdef";
        journal.issue(id);
        journal
            .record(&entry(id, "starting", std::process::id(), None))
            .unwrap();
        let got = journal.get(id, now);
        assert_eq!(got["job"]["state"], "starting");
        // An unknown id reads as the restarted placeholder.
        let unknown = journal.get("job_ffffffffffffffff", now);
        assert_eq!(unknown["job"]["error"]["code"], "server_restarted");
        assert_eq!(unknown["job"]["startedAt"], Value::Null);
        // A finished entry past retention reads as null (issued, then pruned).
        let mut finished = entry(
            id,
            "succeeded",
            std::process::id(),
            Some(now - RETENTION_MS),
        );
        finished.install_pending = false;
        journal.record(&finished).unwrap();
        assert_eq!(journal.get(id, now)["job"], Value::Null);
    }

    #[test]
    #[cfg(unix)]
    fn dead_owner_recovers_server_restarted() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let now = 1_763_424_000_000i64;
        let id = "job_0123456789abcdef";
        // Pid 1 exists but is not this process; use a pid that cannot exist: u32::MAX.
        journal
            .record(&entry(id, "waiting", u32::MAX, None))
            .unwrap();
        let got = journal.get(id, now);
        assert_eq!(got["job"]["state"], "failed");
        assert_eq!(got["job"]["error"]["code"], "server_restarted");
        assert_eq!(got["job"]["verification"], Value::Null);
    }

    #[test]
    fn settle_finalizes_install_pending() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let now = 1_763_424_000_000i64;
        let id = "job_0123456789abcdef";
        let mut installing = entry(id, "verifying", std::process::id(), None);
        installing.install_pending = true;
        journal.record(&installing).unwrap();
        let settled = journal
            .settle(
                id,
                State::Succeeded,
                Some(json!({"accountId": "codex:work", "email": Value::Null, "plan": Value::Null})),
                None,
                now,
            )
            .unwrap();
        assert_eq!(settled["job"]["state"], "succeeded");
        assert_eq!(settled["job"]["result"]["accountId"], "codex:work");
        // A second settle is refused.
        assert!(
            journal
                .settle(id, State::Succeeded, None, None, now)
                .is_err()
        );
    }

    #[test]
    fn device_code_and_settled_email_stay_in_the_live_job_only() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let id = "job_0123456789abcdef";
        let mut waiting = entry(id, "waiting", std::process::id(), None);
        waiting.job["verification"] = json!({
            "url": "https://auth.openai.com/codex/device",
            "userCode": "ABCD-EFGH",
            "expiresAt": waiting.job["expiresAt"],
        });
        journal.record(&waiting).unwrap();
        let file = dir.path().join(format!("signin-jobs/{id}.json"));
        let persisted: Entry = serde_json::from_slice(&std::fs::read(&file).unwrap()).unwrap();
        assert_eq!(persisted.job["state"], "waiting");
        assert_eq!(persisted.job["verification"], Value::Null);
        assert_eq!(waiting.job["verification"]["userCode"], "ABCD-EFGH");

        waiting.install_pending = true;
        journal.record(&waiting).unwrap();
        let settled = journal
            .settle(
                id,
                State::Succeeded,
                Some(json!({"accountId": "codex:work", "email": "person@example.test", "plan": "plus"})),
                None,
                1_763_424_000_000,
            )
            .unwrap();
        assert_eq!(settled["job"]["result"]["email"], "person@example.test");
        let persisted: Entry = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
        assert_eq!(persisted.job["state"], "succeeded");
        assert_eq!(persisted.job["result"]["accountId"], "codex:work");
        assert_eq!(persisted.job["result"]["email"], Value::Null);
    }

    #[test]
    fn cancel_flag_is_recorded() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Journal::open(dir.path()).unwrap();
        let id = "job_0123456789abcdef";
        journal
            .record(&entry(id, "waiting", std::process::id(), None))
            .unwrap();
        let entry = journal.request_cancel(id).unwrap().unwrap();
        assert!(entry.cancel_requested);
        assert!(journal.read(id).unwrap().cancel_requested);
    }
}
