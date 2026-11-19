//! A single-account Re-check publishes a reading only if its source still matches.
use std::future::Future;
use tokio::io;

/// The guarded collector and publication effects for a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Effect {
    /// Collect one resolved source, with normal collector debounce and failure backoff.
    Collect,
    /// Revalidate the source binding immediately before publishing the collected row.
    PublishIfCurrent,
}

/// Dispatch one Re-check. A missing collection or a changed source returns false.
/// Callers bind publication to the same account, provider, host and credential
/// selector captured before collection.
pub async fn run<E, F>(mut perform: impl FnMut(Effect) -> F) -> Result<bool, E>
where
    F: Future<Output = Result<bool, E>>,
{
    if !perform(Effect::Collect).await? {
        return Ok(false);
    }
    perform(Effect::PublishIfCurrent).await
}

/// Compatibility dispatch protocol; only effect names and result flags cross the pipe.
#[doc(hidden)]
pub async fn compatibility_pipe() -> io::Result<()> {
    let pipe = crate::lifecycle_effect_pipe::Pipe::new();
    let result = run(|effect| {
        pipe.perform(match effect {
            Effect::Collect => "collect",
            Effect::PublishIfCurrent => "publish-if-current",
        })
    })
    .await;
    crate::lifecycle_effect_pipe::finish(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_collection_does_not_reach_the_publish_effect() {
        let mut effects = Vec::new();
        let found = run(|effect| {
            effects.push(effect);
            std::future::ready(Ok::<_, ()>(false))
        })
        .await
        .expect("fixture effect");
        assert!(!found);
        assert_eq!(effects, [Effect::Collect]);
    }
}
