//! A single-account Re-check publishes a reading only if its source still matches.
use serde_json::{Value, json};
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

/// The resolved v2 account source captured before a Re-check.
#[derive(Clone, PartialEq, Eq)]
pub struct Source {
    /// Registry account id.
    pub id: String,
    /// Provider whose collector owns the reading.
    pub provider: String,
    /// The platform of the registered account.
    pub platform: String,
    /// The registered SSH alias, when configured.
    pub ssh_host: Option<String>,
    /// The registered credential reference, without any credential bytes.
    pub credential: Value,
}

/// The native route's collect-and-publish path. `collect` returns one normalized
/// public dashboard row and keeps the collector's debounce and backoff.
/// `publish_if_current` compares the complete registry source under its publication
/// guard before replacing the row. False means the source changed during the read.
pub async fn account<E, C, P>(
    source: Source,
    collect: impl FnOnce(Source) -> C,
    publish_if_current: impl FnOnce(Source, Value) -> P,
) -> Result<Option<Value>, E>
where
    C: Future<Output = Result<Option<Value>, E>>,
    P: Future<Output = Result<bool, E>>,
{
    let Some(mut row) = collect(source.clone()).await? else {
        return Ok(None);
    };
    if row["id"].as_str() != Some(source.id.as_str())
        || row["provider"].as_str() != Some(source.provider.as_str())
        || row["platform"].as_str() != Some(source.platform.as_str())
        || !publish_if_current(source, row.clone()).await?
    {
        return Ok(None);
    }
    row["lifecycle"] = json!({"state":"ready","jobId":null});
    Ok(Some(row))
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
    async fn an_unbound_or_replaced_source_never_produces_a_recheck_reply() {
        let source = Source {
            id: "zai:usage".into(),
            provider: "zai".into(),
            platform: "ubuntu".into(),
            ssh_host: None,
            credential: json!({"kind":"discover"}),
        };
        let row = |provider: &str| json!({"id":"zai:usage","provider":provider,"platform":"ubuntu","status":"cached"});
        let reply = account(
            source.clone(),
            |_| std::future::ready(Ok::<_, ()>(Some(row("cursor")))),
            |_, _| async {
                panic!("a foreign provider reading reached publication");
            },
        )
        .await;
        assert_eq!(reply, Ok(None));
        let reply = account(
            source.clone(),
            |_| std::future::ready(Ok::<_, ()>(Some(row("zai")))),
            |_, _| std::future::ready(Ok(false)),
        )
        .await;
        assert_eq!(reply, Ok(None));
        let reply = account(
            source,
            |_| std::future::ready(Ok::<_, ()>(Some(row("zai")))),
            |_, _| std::future::ready(Ok(true)),
        )
        .await
        .expect("fixture collection")
        .expect("current matching reading");
        assert_eq!(reply["lifecycle"], json!({"state":"ready","jobId":null}));
    }

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

#[cfg(test)]
mod golden_tests {
    use super::*;

    /// The TS route's one-account reply (`tests/spec/recheck-reply-parity.ts`): the
    /// collected row is stored without a lifecycle, the reply adds the ready one, and
    /// an account the registry no longer names publishes nothing. The route's 10 s
    /// admission stays with the route and is recorded in the fixture only.
    #[tokio::test]
    async fn recheck_replies_match_the_ts_fixture() {
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/recheck-replies.json"))
                .expect("recheck fixture parses");
        assert_eq!(fixture["version"], 1);
        for case in fixture["cases"].as_array().expect("fixture cases") {
            let id = case["id"].as_str().expect("account id");
            if case["name"] == "unknown-account" {
                let source = Source {
                    id: id.into(),
                    provider: "zai".into(),
                    platform: "ubuntu".into(),
                    ssh_host: None,
                    credential: json!({"kind":"discover"}),
                };
                let reply = account(
                    source,
                    |_| std::future::ready(Ok::<_, ()>(None)),
                    |_, _| async {
                        panic!("an unknown account reached publication");
                    },
                )
                .await;
                assert_eq!(reply, Ok(None));
                continue;
            }
            if case["name"] == "route-limit" {
                assert_eq!(case["outcome"]["status"], 429);
                continue;
            }
            let reply = &case["reply"]["account"];
            let source = Source {
                id: id.into(),
                provider: reply["provider"].as_str().expect("provider").into(),
                platform: reply["platform"].as_str().expect("platform").into(),
                ssh_host: None,
                credential: json!({"kind":"discover"}),
            };
            let expected = reply.clone();
            let stored = case["stored"].clone();
            let reply = account(
                source,
                move |_| {
                    let mut row = expected.clone();
                    let _ = row.as_object_mut().expect("reply row").remove("lifecycle");
                    std::future::ready(Ok::<_, ()>(Some(row)))
                },
                move |_, row| {
                    assert_eq!(
                        aac_views::js_json::write(&row),
                        aac_views::js_json::write(&stored),
                        "the stored row is the collected row, before the ready lifecycle"
                    );
                    std::future::ready(Ok::<_, ()>(true))
                },
            )
            .await
            .expect("fixture effects")
            .expect("current account");
            assert_eq!(
                aac_views::js_json::write(&reply),
                aac_views::js_json::write(&case["reply"]["account"]),
                "{}",
                case["name"]
            );
        }
    }
}
