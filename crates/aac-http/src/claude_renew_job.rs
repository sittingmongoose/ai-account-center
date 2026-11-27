//! The Claude sign-in renewal schedule (`aac-claude-renew`): every 10 minutes the server reads
//! every computer's Claude sign-ins and renews those within a day of their end, through the CLI's
//! own login and each account's plain Chrome profile on the browser computer. Dormant until
//! `<CCS dir>/claude-renew/config.json` exists; `"enabled": false` pauses it.
//!
//! Each run holds the boot's single `local_work` slot (MEM7 lane 2), so a local
//! `claude auth status` never overlaps a local `python3 -c` / `aac-agent` helper and
//! renewal scans never overlap each other: max concurrency 1. At most one renew run
//! exists at a time (the schedule loop is sequential), so the slot is a memory gate,
//! not a liveness gate; a tick that cannot acquire it in time is skipped and the
//! schedule retries in ten minutes.
use crate::agent_transport::Agents;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The schedule's period.
const EVERY: Duration = Duration::from_secs(600);
/// The first run waits this long after the service starts.
const FIRST: Duration = Duration::from_secs(120);
/// How long a run waits for the single local-work slot before skipping the tick. A full
/// `activity_pass` holds the slot for minutes; waiting less than that would skip most
/// ticks whenever a pass is running, while waiting past the next tick risks a stall.
/// Five minutes covers the worst live pass (~162 s) with margin.
const GATE_WAIT: Duration = Duration::from_secs(300);

async fn pause(total: Duration, stop: &AtomicBool) -> bool {
    let mut left = total;
    while !left.is_zero() {
        if stop.load(Ordering::Acquire) {
            return false;
        }
        let step = left.min(Duration::from_secs(5));
        tokio::time::sleep(step).await;
        left -= step;
    }
    !stop.load(Ordering::Acquire)
}

/// Runs the renewal on its schedule until the service stops. `agents` is the boot's one
/// `Agents` pool; the run holds its `local_work` slot (max concurrency 1 for
/// `claude auth status` against local helpers, MEM7 lane 2).
pub(crate) async fn jobs(ccs_dir: PathBuf, stop: Arc<AtomicBool>, agents: Arc<Agents>) {
    if !pause(FIRST, &stop).await {
        return;
    }
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let deadline = tokio::time::Instant::now() + GATE_WAIT;
        let Ok(_local_work) = agents.local_work_acquire(deadline).await else {
            // A local helper held the slot past the gate wait (or the service is
            // stopping): skip this tick; the schedule retries in ten minutes.
            if !pause(EVERY, &stop).await {
                return;
            }
            continue;
        };
        let dir = ccs_dir.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let publish = aac_renewal::claude::snapshot_directory(&dir);
            aac_claude_renew::service::run(
                &dir,
                publish,
                aac_claude_renew::service::RunOptions {
                    scheduled: true,
                    ..Default::default()
                },
            )
        })
        .await;
        // `_local_work` is held across the whole run and released here, before the pause.
        drop(_local_work);
        if !pause(EVERY, &stop).await {
            return;
        }
    }
}
