//! Startup and daily scheduling for account lifecycle maintenance.
use std::{future::Future, time::Duration};
use tokio::{
    io::{self, AsyncReadExt},
    time::{self, MissedTickBehavior},
};

const DAILY: Duration = Duration::from_secs(86_400);

/// One independently retried maintenance action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Step {
    /// Remove expired Codex sign-in staging.
    CodexStaging,
    /// Sweep old, unreferenced dashboard-host keys.
    OrphanKeys,
    /// Remove expired sign-in staging and markers.
    AntigravityStaging,
    /// Sweep Antigravity profiles whose ownership can be proved.
    AntigravityOrphans,
    /// Purge due, unprotected Claude trash through the host permit.
    ClaudeTrash,
}

impl Step {
    const ALL: [Self; 5] = [
        Self::CodexStaging,
        Self::OrphanKeys,
        Self::AntigravityStaging,
        Self::AntigravityOrphans,
        Self::ClaudeTrash,
    ];

    fn wire_name(self) -> &'static str {
        match self {
            Self::CodexStaging => "codex-staging",
            Self::OrphanKeys => "orphan-keys",
            Self::AntigravityStaging => "antigravity-staging",
            Self::AntigravityOrphans => "antigravity-orphans",
            Self::ClaudeTrash => "claude-trash",
        }
    }
}

fn clock(period: Duration) -> time::Interval {
    let mut clock = time::interval(period);
    clock.set_missed_tick_behavior(MissedTickBehavior::Skip);
    clock
}

/// Start once for the active dashboard boot. Each effect settles before the
/// next one starts. A failed sweep leaves its work for the next day.
pub async fn run<E, F>(mut effects: impl FnMut(Step) -> F, shutdown: impl Future<Output = ()>)
where
    F: Future<Output = Result<(), E>>,
{
    let mut clock = clock(DAILY);
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return,
            _ = clock.tick() => {}
        }
        for step in Step::ALL {
            // Let the active effect finish on shutdown. In particular, a host
            // purge may have reserved its row and already reached a host.
            tokio::select! {
                biased;
                _ = &mut shutdown => return,
                _ = std::future::ready(()) => {}
            }
            let _ = effects(step).await;
        }
    }
}

/// Compatibility adapter for the TS front door. The child drives scheduling
/// and dispatch. Each acknowledgement settles one step, regardless of outcome.
#[doc(hidden)]
pub async fn compatibility_pipe() -> io::Result<()> {
    use std::io::Write;

    let mut clock = clock(DAILY);
    let mut input = io::stdin();
    let mut byte = [0];
    let mut output = std::io::stdout().lock();
    loop {
        tokio::select! {
            biased;
            _ = input.read(&mut byte) => return Ok(()),
            _ = clock.tick() => {}
        }
        for step in Step::ALL {
            writeln!(output, "{}", step.wire_name())?;
            output.flush()?;
            if input.read(&mut byte).await? != 1 || byte != [b'\n'] {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;

    #[tokio::test(start_paused = true)]
    async fn runs_at_boot_and_each_day_even_when_a_step_fails() {
        let effects = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&effects);
        let (stop, stopped) = oneshot::channel();
        let task = tokio::spawn(run(
            move |step| {
                effects.lock().expect("fixture records effects").push(step);
                std::future::ready(if step == Step::OrphanKeys {
                    Err(())
                } else {
                    Ok(())
                })
            },
            async {
                let _ = stopped.await;
            },
        ));
        time::advance(Duration::from_millis(1)).await;
        tokio::task::yield_now().await;
        assert_eq!(
            *observed.lock().expect("fixture records effects"),
            Step::ALL
        );
        time::advance(DAILY).await;
        tokio::task::yield_now().await;
        assert_eq!(
            *observed.lock().expect("fixture records effects"),
            Step::ALL.repeat(2)
        );
        stop.send(()).expect("dispatcher is running");
        task.await.expect("dispatcher joined");
    }
}
