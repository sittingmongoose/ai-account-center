//! Ordered Claude host effects and compensation after front-door admission.
use std::future::Future;
use tokio::io;

/// A workflow chosen after provider, identity and confirmation checks.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Workflow {
    /// Create launchers and register a pending profile.
    Add,
    /// Move the launchers to trash before committing registry changes.
    Remove,
    /// Restore the launchers before publishing the account.
    Restore,
    /// Purge every host of a reserved trash entry before removing its row.
    Purge,
    /// Inspect the pending launcher and promote it after verified sign-in.
    Recheck,
}

impl Workflow {
    fn from_wire(value: &str) -> Option<Self> {
        match value {
            "add" => Some(Self::Add),
            "remove" => Some(Self::Remove),
            "restore" => Some(Self::Restore),
            "purge" => Some(Self::Purge),
            "recheck" => Some(Self::Recheck),
            _ => None,
        }
    }
}

/// One host or registry operation. Inputs stay in the caller's guarded state.
#[derive(Clone, Copy)]
pub enum Effect {
    /// Apply the workflow to a macOS profile.
    ApplyMac,
    /// Apply the workflow to a Windows profile.
    ApplyWindows,
    /// Undo an applied macOS effect.
    UndoMac,
    /// Undo an applied Windows effect.
    UndoWindows,
    /// Settle the registry after the host effects succeed.
    Commit,
    /// Verify that the pending launcher has a session eligible for promotion.
    Session,
}

impl Effect {
    fn wire_name(self) -> &'static str {
        match self {
            Self::ApplyMac => "apply-mac",
            Self::ApplyWindows => "apply-windows",
            Self::UndoMac => "undo-mac",
            Self::UndoWindows => "undo-windows",
            Self::Commit => "commit",
            Self::Session => "session",
        }
    }
}

/// Run an admitted workflow. An apply effect returns true when that host's
/// state changed, false when the profile has no launcher there. Preserve the
/// original failure while attempting every applicable compensation.
pub async fn run<E, F>(workflow: Workflow, mut perform: impl FnMut(Effect) -> F) -> Result<(), E>
where
    F: Future<Output = Result<bool, E>>,
{
    if workflow == Workflow::Recheck {
        if perform(Effect::Session).await? {
            perform(Effect::Commit).await?;
        }
        return Ok(());
    }
    let mut undo = Vec::new();
    let mut failed = None;
    for (apply, compensate) in [
        (Effect::ApplyMac, Effect::UndoMac),
        (Effect::ApplyWindows, Effect::UndoWindows),
    ] {
        match perform(apply).await {
            Ok(true) => undo.push(compensate),
            Ok(false) => {}
            Err(error) => {
                failed.get_or_insert(error);
                if workflow != Workflow::Purge {
                    break;
                }
            }
        }
    }
    let result = match failed {
        Some(error) => Err(error),
        None => perform(Effect::Commit).await.map(|_| ()),
    };
    if result.is_err()
        && matches!(
            workflow,
            Workflow::Add | Workflow::Remove | Workflow::Restore
        )
    {
        for effect in undo.into_iter().rev() {
            let _ = perform(effect).await;
        }
    }
    result
}

/// Compatibility pipe transmits effect names and outcome flags only.
#[doc(hidden)]
pub async fn compatibility_pipe(mode: &str) -> io::Result<()> {
    let workflow = Workflow::from_wire(mode).ok_or(io::ErrorKind::InvalidInput)?;
    let pipe = crate::lifecycle_effect_pipe::Pipe::new();
    let result = run(workflow, |effect| pipe.perform(effect.wire_name())).await;
    crate::lifecycle_effect_pipe::finish(result.map(|_| true))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn restore_inventory_failure_undoes_both_host_moves() {
        let mut steps = Vec::new();
        let result = run(Workflow::Restore, |effect| {
            steps.push(effect.wire_name());
            std::future::ready(if matches!(effect, Effect::Commit) {
                Err("inventory write")
            } else {
                Ok(true)
            })
        })
        .await;
        assert_eq!(result, Err("inventory write"));
        assert_eq!(
            steps,
            [
                "apply-mac",
                "apply-windows",
                "commit",
                "undo-windows",
                "undo-mac"
            ]
        );
    }
}
