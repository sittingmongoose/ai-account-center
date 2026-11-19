//! Transaction order for API-key lifecycle operations.
use std::future::Future;
use tokio::io;

/// An API-key workflow admitted under the matching provider queue.
#[derive(Clone, Copy)]
pub enum Workflow {
    /// Save a new key and add its account.
    Add,
    /// Probe a trial key before updating the existing account's credential.
    Replace,
    /// Unpublish an account and delete its credential.
    Remove,
}

/// A storage or provider effect performed against the guarded account state.
#[derive(Clone, Copy)]
pub enum Effect {
    /// Save the new or temporary credential.
    Store,
    /// Add the account, publish the accepted replacement, or restore a removed account.
    Publish,
    /// Ask the provider to verify the new or trial key.
    Probe,
    /// Remove the typed account from the registry.
    Unpublish,
    /// Delete the credential associated with the current workflow.
    Discard,
}

impl Effect {
    fn wire_name(self) -> &'static str {
        match self {
            Self::Store => "store",
            Self::Publish => "publish",
            Self::Probe => "probe",
            Self::Unpublish => "unpublish",
            Self::Discard => "discard",
        }
    }
}

/// Execute the workflow through its provider and persistence effects. A
/// declined probe returns false after cleaning up any trial state.
pub async fn run<E, F>(workflow: Workflow, mut perform: impl FnMut(Effect) -> F) -> Result<bool, E>
where
    F: Future<Output = Result<bool, E>>,
{
    use Effect::{Discard, Probe, Publish, Store, Unpublish};
    match workflow {
        Workflow::Add => {
            perform(Store).await?;
            if let Err(error) = perform(Publish).await {
                let _ = perform(Discard).await;
                return Err(error);
            }
            let accepted = perform(Probe).await?;
            if !accepted {
                let _ = perform(Unpublish).await;
                let _ = perform(Discard).await;
            }
            Ok(accepted)
        }
        Workflow::Replace => {
            perform(Store).await?;
            let result = match perform(Probe).await {
                Ok(true) => perform(Publish).await.map(|_| true),
                other => other,
            };
            let _ = perform(Discard).await;
            result
        }
        Workflow::Remove => {
            perform(Unpublish).await?;
            if let Err(error) = perform(Discard).await {
                let _ = perform(Publish).await;
                return Err(error);
            }
            Ok(true)
        }
    }
}

/// Compatibility pipe. Key material and provider receipts stay with the caller.
#[doc(hidden)]
pub async fn compatibility_pipe(mode: &str) -> io::Result<()> {
    let workflow = match mode {
        "add" => Workflow::Add,
        "replace" => Workflow::Replace,
        "remove" => Workflow::Remove,
        _ => return Err(io::ErrorKind::InvalidInput.into()),
    };
    let pipe = crate::lifecycle_effect_pipe::Pipe::new();
    let result = run(workflow, |effect| pipe.perform(effect.wire_name())).await;
    crate::lifecycle_effect_pipe::finish(result)
}
