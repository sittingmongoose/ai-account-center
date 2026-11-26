//! Bind the shared maintenance clock to the lifecycle stores in a worker-free boot.
use super::lifecycle_trash::Trash;
use crate::{
    account_usage::sources,
    lifecycle_maintenance::{self, Step},
    lifecycle_queue::LifecycleQueues,
    public_auth::{now, storage},
};
use aac_files::{
    key_sweep, process, profile_staging, profile_writes::AntigravityMaintenanceLease,
    registry::Owners,
};
use aac_transport::CancellationToken;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};
use tokio::task::JoinHandle;

struct Maintenance {
    root: PathBuf,
    owners: Owners,
    queues: LifecycleQueues,
    trash: Arc<Trash>,
}

pub(super) fn start(
    root: PathBuf,
    owners: Owners,
    trash: Arc<Trash>,
    cancel: CancellationToken,
) -> JoinHandle<()> {
    let owner = Arc::new(Maintenance {
        queues: LifecycleQueues::new(root.clone()),
        root,
        owners,
        trash,
    });
    tokio::spawn(async move {
        lifecycle_maintenance::run(
            move |step| {
                let owner = owner.clone();
                async move {
                    owner.step(step).await;
                    Ok::<_, ()>(())
                }
            },
            cancel.cancelled(),
        )
        .await;
    })
}

impl Maintenance {
    async fn step(&self, step: Step) {
        match step {
            Step::CodexStaging => {
                let root = self.root.clone();
                let _ = tokio::task::spawn_blocking(move || profile_staging::sweep(&root, clock()))
                    .await;
            }
            Step::OrphanKeys => self.sweep_keys().await,
            Step::AntigravityStaging => {
                let root = self.root.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let now = clock();
                    let _ = profile_staging::sweep_antigravity(&root, now);
                    let boot = boot_id();
                    profile_staging::sweep_antigravity_markers(&root, now, |bytes, mtime| {
                        holder_stale(bytes, mtime, now, boot.as_deref())
                    })
                })
                .await;
            }
            Step::AntigravityOrphans => {
                let root = self.root.clone();
                let _ = tokio::task::spawn_blocking(move || sweep_antigravity_orphans(&root)).await;
            }
            Step::ClaudeTrash => self.trash.sweep_due().await,
        }
    }

    async fn sweep_keys(&self) {
        for provider in aac_providers::dashboard_providers().filter(|provider| provider.key_store) {
            let provider = provider.id.as_str();
            let root = self.root.clone();
            let owners = self.owners.clone();
            let _ = self
                .queues
                .run(&format!("key:{provider}"), async move {
                    tokio::task::spawn_blocking(move || {
                        let Ok(sources::Sources::V2(entries)) = sources::read_registry(&root)
                        else {
                            return;
                        };
                        let named: Vec<String> = entries
                            .into_iter()
                            .filter_map(|entry| match entry.credential {
                                sources::Credential::AacKey { key_id }
                                    if entry.provider == provider =>
                                {
                                    Some(key_id)
                                }
                                _ => None,
                            })
                            .collect();
                        let _ = key_sweep::sweep_owned(&root, &owners, provider, &named, clock());
                    })
                    .await
                })
                .await;
        }
    }
}

fn clock() -> u64 {
    u64::try_from(now()).unwrap_or_default()
}

pub(super) fn antigravity_signin_running(root: &Path, profile: &str) -> bool {
    let file = root
        .join("antigravity-signin")
        .join(format!("{profile}.running"));
    let Ok(stat) = aac_files::read::stat(&file) else {
        return aac_files::read::entry_exists(&file).unwrap_or(true);
    };
    let mtime = stat.mtime_ms as u64;
    let now = clock();
    let stale = aac_files::read_bounded(&file, 512)
        .ok()
        .and_then(|bytes| holder_stale(&bytes, mtime, now, boot_id().as_deref()))
        .unwrap_or_else(|| now.saturating_sub(mtime) > 30 * 60_000);
    !stale
}

fn sweep_antigravity_orphans(root: &Path) -> Option<(usize, usize)> {
    let marker = holder_record()?;
    let lease = AntigravityMaintenanceLease::claim(root, marker.as_bytes()).ok()?;
    let state = crate::antigravity_runtime::read_state(root)?;
    let saved = state
        .profiles
        .into_iter()
        .map(|profile| profile.id)
        .collect::<Vec<_>>();
    lease.writer().antigravity_sweep(root, &saved).ok()
}

pub(crate) fn holder_record() -> Option<String> {
    let pid = std::process::id();
    let marker = json!({
        "pid":pid,"startedAt":storage::iso(now())?,"processStart":process_start(pid),"bootId":boot_id()
    });
    Some(marker.to_string())
}

fn process_start(pid: u32) -> Option<String> {
    let bytes = aac_files::read_bounded(Path::new(&format!("/proc/{pid}/stat")), 8192).ok()?;
    let stat = std::str::from_utf8(&bytes).ok()?;
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    digits(start).then(|| start.to_owned())
}

pub(crate) fn boot_id() -> Option<String> {
    let bytes = aac_files::read_bounded(Path::new("/proc/sys/kernel/random/boot_id"), 64).ok()?;
    let id = std::str::from_utf8(&bytes).ok()?.trim();
    boot_id_shape(id).then(|| id.to_owned())
}

fn digits(value: &str) -> bool {
    (1..=20).contains(&value.len()) && value.bytes().all(|byte| byte.is_ascii_digit())
}

fn boot_id_shape(value: &str) -> bool {
    value.len() == 36
        && value
            .bytes()
            .all(|byte| byte == b'-' || byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn optional(value: &Value, predicate: impl Fn(&str) -> bool) -> Option<Option<&str>> {
    if value.is_null() {
        return Some(None);
    }
    value.as_str().filter(|text| predicate(text)).map(Some)
}

pub(crate) fn holder_stale(bytes: &[u8], mtime: u64, now: u64, boot: Option<&str>) -> Option<bool> {
    holder_stale_after(bytes, mtime, now, boot, 30 * 60_000)
}

pub(crate) fn holder_stale_after(
    bytes: &[u8],
    mtime: u64,
    now: u64,
    boot: Option<&str>,
    silent_after_ms: u64,
) -> Option<bool> {
    let record: Value = serde_json::from_slice(bytes).ok()?;
    let pid = u32::try_from(record["pid"].as_u64()?).ok()?;
    if pid == 0 {
        return None;
    }
    let started = u64::try_from(storage::timestamp(record["startedAt"].as_str()?)?).ok()?;
    let start = optional(&record["processStart"], digits)?;
    let recorded_boot = optional(&record["bootId"], boot_id_shape)?;
    if recorded_boot
        .zip(boot)
        .is_some_and(|(recorded, current)| recorded != current)
        || process::is_dead(pid)
        || start
            .zip(process_start(pid))
            .is_some_and(|(recorded, current)| recorded != current)
    {
        return Some(true);
    }
    Some(now.saturating_sub(mtime.max(started)) > silent_after_ms)
}
