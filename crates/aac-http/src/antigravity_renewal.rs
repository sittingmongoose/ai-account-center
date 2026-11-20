//! Antigravity saved-profile and live-login settlement under the provider lock.
use std::future::Future;
use tokio::io;

/// Provider effects ordered after the saved and live identities have been checked.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Save the verified new credential to the profile.
    Save,
    /// Compare and exchange the old live login.
    Install,
    /// Reread the live login after an unconfirmed exchange.
    Verify,
    /// Restore the previous saved profile when the live login did not change.
    Restore,
}

/// Outcome of the paired install.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Saved and live credentials both use the new login.
    Renewed,
    /// The previous profile was restored.
    Restored,
    /// The profile saved but the live login and rollback could not be completed.
    ProfileOnly,
}

/// Settle one verified sign-in while the caller holds the provider transaction
/// lock. An uncertain exchange is checked before compensating the saved profile.
pub async fn run<E, F>(mut perform: impl FnMut(Effect) -> F) -> Result<Outcome, E>
where
    F: Future<Output = Result<bool, E>>,
{
    perform(Effect::Save).await?;
    if perform(Effect::Install).await.unwrap_or(false)
        || perform(Effect::Verify).await.unwrap_or(false)
    {
        return Ok(Outcome::Renewed);
    }
    if perform(Effect::Restore).await.unwrap_or(false) {
        Ok(Outcome::Restored)
    } else {
        Ok(Outcome::ProfileOnly)
    }
}

/// Compatibility bridge. The phase flags carry no credential bytes.
#[doc(hidden)]
pub async fn compatibility_pipe() -> io::Result<()> {
    let pipe = crate::lifecycle_effect_pipe::Pipe::new();
    let result = run(|effect| {
        pipe.perform(match effect {
            Effect::Save => "save",
            Effect::Install => "install",
            Effect::Verify => "verify",
            Effect::Restore => "restore",
        })
    })
    .await;
    crate::lifecycle_effect_pipe::finish(result.map(|outcome| outcome == Outcome::Renewed))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn checks_an_uncertain_exchange_before_rolling_back_the_profile() {
        for (verified, restored, want) in [
            (true, false, Outcome::Renewed),
            (false, true, Outcome::Restored),
            (false, false, Outcome::ProfileOnly),
        ] {
            let mut effects = Vec::new();
            let result = run(|effect| {
                effects.push(effect);
                std::future::ready(Ok::<_, ()>(match effect {
                    Effect::Save => true,
                    Effect::Install => false,
                    Effect::Verify => verified,
                    Effect::Restore => restored,
                }))
            })
            .await;
            assert_eq!(result, Ok(want));
            assert_eq!(
                &effects[..3],
                &[Effect::Save, Effect::Install, Effect::Verify]
            );
            assert_eq!(effects.contains(&Effect::Restore), !verified);
        }
    }
}
