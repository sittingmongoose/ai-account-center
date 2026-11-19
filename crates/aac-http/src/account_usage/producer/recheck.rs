//! G7's per-account Re-check: one source through the native collector, its row
//! published into the shared generation cache under lane J's guards.
use super::Producer;
use crate::account_usage::{AccountUsageCache, Desktop, sources, sources::Source};
use crate::account_usage::producer::CompanionInputs;
use crate::lifecycle_recheck;
use aac_views::decline::Decline;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// One account source resolved from the registry before a Re-check collects.
pub struct Resolved {
    /// The registered source, exactly as the whole-generation pass reads it.
    pub source: Source,
    /// The version 1 manifest supplied this source.
    pub legacy: bool,
}

/// The TS per-source forced-refresh debounce (`REFRESH_DEBOUNCE_MS`).
const REFRESH_DEBOUNCE_MS: i64 = 5_000;

/// The publication guard compares the same fields as the TS Re-check binding:
/// provider, platform, SSH alias and credential selector, never the label.
fn same_binding(current: &Source, expected: &Source) -> bool {
    current.provider == expected.provider
        && current.platform == expected.platform
        && current.ssh_host == expected.ssh_host
        && current.credential == expected.credential
}

impl Producer {
    /// The account's current source binding, or None when the list is unreadable,
    /// invalid, or does not name the account (the TS refreshAdditional lookup).
    pub async fn resolve_recheck(&self, id: &str) -> Option<Resolved> {
        let root = self.root.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || {
            match sources::read_registry(&root) {
                Ok(sources::Sources::V2(rows)) => rows
                    .into_iter()
                    .find(|source| source.id == id)
                    .map(|source| Resolved {
                        source,
                        legacy: false,
                    }),
                Ok(sources::Sources::Legacy) => {
                    let (rows, valid) = sources::legacy(Some(&root));
                    if !valid {
                        return None;
                    }
                    rows.into_iter()
                        .find(|source| source.id == id)
                        .map(|source| Resolved {
                            source,
                            legacy: true,
                        })
                }
                Err(_) => None,
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// Collect exactly one source now: the reading caches' TTL is skipped while
    /// the collector's own cooldowns, and the five-second forced refresh
    /// debounce, still apply (the TS `getSource(source, refresh = true)` path).
    pub async fn collect_recheck_row(&self, resolved: &Resolved, now: i64) -> Value {
        let force = {
            let mut forced = self
                .recheck_forced
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let last = forced.get(&resolved.source.id).copied().unwrap_or(i64::MIN);
            let force = now.saturating_sub(last) >= REFRESH_DEBOUNCE_MS;
            if force {
                forced.insert(resolved.source.id.clone(), now);
            }
            force
        };
        self.additional_row(&resolved.source, resolved.legacy, now, force)
            .await
    }

    /// Lane J's PublishIfCurrent: re-resolve the account and, only when the
    /// provider, platform, SSH alias and credential are unchanged, replace the
    /// row in every held dashboard generation. Unrelated rows of a concurrent
    /// generation are kept; a source that changed mid-read publishes nothing.
    pub async fn publish_recheck_row(
        &self,
        cache: &AccountUsageCache,
        companions: &CompanionInputs,
        id: &str,
        expected: &Source,
        row: Value,
    ) -> Result<bool, Decline> {
        let Some(current) = self.resolve_recheck(id).await else {
            return Ok(false);
        };
        if !same_binding(&current.source, expected) {
            return Ok(false);
        }
        for desktop in [Desktop::Mac, Desktop::Windows] {
            for secure in [false, true] {
                cache.replace_row(desktop, secure, id, row.clone(), &companions.lifecycle)?;
            }
        }
        Ok(true)
    }

    /// The full guarded sequence for one account: resolve, collect, revalidate,
    /// publish, and answer the row the route shows. None is the Re-check 404
    /// (unknown or replaced account); a decline hands the route back to TS.
    pub async fn recheck(
        &self,
        cache: &AccountUsageCache,
        companions: &CompanionInputs,
        id: &str,
    ) -> Result<Option<Value>, Decline> {
        let Some(resolved) = self.resolve_recheck(id).await else {
            return Ok(None);
        };
        let expected = resolved.source.clone();
        let guard = lifecycle_recheck::Source {
            id: expected.id.clone(),
            provider: expected.provider.clone(),
            platform: expected.platform.clone(),
            ssh_host: expected.ssh_host.clone(),
            credential: serde_json::to_value(&expected.credential)
                .map_err(|_| Decline::Internal)?,
        };
        let Ok(now) = aac_core::UtcMs::now() else {
            return Err(Decline::Date);
        };
        lifecycle_recheck::account(
            guard,
            |_source| async move {
                let reading = self.collect_recheck_row(&resolved, now.get() as i64).await;
                Ok::<_, Decline>(Some(reading))
            },
            |_source, row| async move {
                self.publish_recheck_row(cache, companions, id, &expected, row)
                    .await
            },
        )
        .await
    }
}
