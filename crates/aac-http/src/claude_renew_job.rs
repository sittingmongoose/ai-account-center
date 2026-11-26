//! The Claude sign-in renewal schedule (`aac-claude-renew`): every 10 minutes the server reads
//! every computer's Claude sign-ins and renews those within a day of their end, through the CLI's
//! own login and each account's plain Chrome profile on the browser computer. Dormant until
//! `<CCS dir>/claude-renew/config.json` exists; `"enabled": false` pauses it.
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

/// The schedule's period.
const EVERY: Duration = Duration::from_secs(600);
/// The first run waits this long after the service starts.
const FIRST: Duration = Duration::from_secs(120);

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

/// Runs the renewal on its schedule until the service stops.
pub(crate) async fn jobs(ccs_dir: PathBuf, stop: Arc<AtomicBool>) {
    if !pause(FIRST, &stop).await {
        return;
    }
    loop {
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
        if !pause(EVERY, &stop).await {
            return;
        }
    }
}
