//! Dashboard snapshot publication shared by Accounts, Home, the usage hub and store sampling.
pub(crate) mod cursor;
pub mod dashboard;
pub mod failover;
pub(crate) mod helper;
pub mod producer;
pub mod provider_caches;
mod rows;
pub mod sampler;
pub mod sources;

use crate::feed::{Feeds, Generation, HOME_DASHBOARD, Prepared};
use aac_views::decline::Decline;
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc};

/// The dashboard's desktop selector. It names the view, even when a reading came from another host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Desktop {
    /// The Mac dashboard and tray.
    Mac,
    /// The Windows dashboard and tray.
    Windows,
}

impl Desktop {
    fn word(self) -> &'static str {
        match self {
            Self::Mac => "mac",
            Self::Windows => "windows",
        }
    }
}

/// The exact same immutable dashboard generations the private Home and Accounts routes render.
/// Standalone producers publish a complete public dashboard DTO for each selector and transport.
#[derive(Clone)]
pub struct AccountUsageCache {
    pub(crate) feeds: Feeds,
    refresh: Arc<tokio::sync::Notify>,
    last_refresh: Arc<std::sync::atomic::AtomicU64>,
}

impl Default for AccountUsageCache {
    fn default() -> Self {
        Self {
            feeds: Feeds::new(),
            refresh: Arc::default(),
            last_refresh: Arc::default(),
        }
    }
}

impl AccountUsageCache {
    /// Publish one dashboard. Generation IDs must increase per selector, and a late generation
    /// cannot replace a more recent one. Date epochs and sort ranks use the existing Home contract.
    pub fn publish(
        &self,
        desktop: Desktop,
        secure: bool,
        generation: u64,
        mut dashboard: Value,
        facts: Value,
    ) -> Result<bool, Decline> {
        let row = dashboard.as_object_mut().ok_or(Decline::Shape)?;
        if row.get("schemaVersion") != Some(&json!(1))
            || !row.get("accounts").is_some_and(Value::is_array)
            || !row.get("updatedAt").is_some_and(Value::is_string)
        {
            return Err(Decline::Shape);
        }
        row.insert("updatedAt".into(), json!(""));
        let generation = generation.to_string();
        let source = source(desktop.word(), secure);
        let body = aac_views::js_json::write(&json!({
            "generation": generation,
            "data": dashboard,
            "facts": facts,
        }));
        let held = self.feeds.put_dashboard(&source, &body)?;
        Ok(held["generation"] == generation)
    }

    /// Ask the native sampler to start a fresh generation, with the dashboard's
    /// five-second debounce. Provider cooldowns still apply.
    pub fn request_refresh(&self) -> bool {
        use std::sync::atomic::Ordering;
        let Ok(now) = aac_core::UtcMs::now() else {
            return false;
        };
        let now = now.get();
        if self
            .last_refresh
            .try_update(Ordering::AcqRel, Ordering::Acquire, |last| {
                (now.saturating_sub(last) >= 5000).then_some(now)
            })
            .is_err()
        {
            return false;
        }
        self.refresh.notify_one();
        true
    }

    /// Replace one account row inside the latest held generation for a selector
    /// (a per-account Re-check), keeping every unrelated row of a concurrently
    /// published generation. The stored visibility settings redecorate the row
    /// and the Home facts are rebuilt from the final rows, so the patched
    /// generation is byte-equivalent to a full pass that collected this row.
    /// A selector without a held generation, or one that does not list the id,
    /// keeps its generation unchanged, like the TS row replacement.
    pub fn replace_row(
        &self,
        desktop: Desktop,
        secure: bool,
        id: &str,
        row: Value,
        lifecycle: &dashboard::Lifecycle,
    ) -> Result<bool, Decline> {
        let word = desktop.word();
        let held = match self.feeds.latest(&source(word, secure)) {
            Ok(held) => held,
            Err(Decline::Stale) => return Ok(false),
            Err(reason) => return Err(reason),
        };
        let mut dashboard: Value =
            serde_json::from_str(held.data.get()).map_err(|_| Decline::Shape)?;
        let settings = dashboard["settings"].clone();
        let accounts = dashboard["accounts"].as_array_mut().ok_or(Decline::Shape)?;
        let replaced = accounts.iter_mut().find(|row| row["id"] == id);
        let Some(target) = replaced else {
            return Ok(false);
        };
        *target = dashboard::decorate(row, &settings, lifecycle)?;
        let accounts = dashboard["accounts"].as_array().ok_or(Decline::Shape)?;
        let signins = dashboard.get("claudeSignIns");
        let facts = dashboard::facts(accounts, signins)?;
        let Ok(now) = aac_core::UtcMs::now() else {
            return Err(Decline::Date);
        };
        // The sampler's own numbering: one above the held generation, and never
        // below the wall clock, so a concurrent pass and a Re-check cannot collide.
        let generation = held
            .id
            .parse::<u64>()
            .map_or(now.get(), |held| held.saturating_add(1))
            .max(now.get());
        self.publish(desktop, secure, generation, dashboard, facts)
    }

    /// Generation to send to the private Home renderer. Both listeners share
    /// the same cache, so this needs no intermediate TypeScript feed.
    pub fn generation(&self, desktop: Desktop, secure: bool) -> Option<String> {
        Some(
            self.feeds
                .latest(&source(desktop.word(), secure))
                .ok()?
                .id
                .clone(),
        )
    }

    /// Render the latest prepared dashboard. A missing selector declines so the caller can
    /// keep serving its existing source while the first collection is pending.
    pub fn render(
        &self,
        desktop: Desktop,
        secure: bool,
        updated_at: &str,
        now: i64,
    ) -> Result<Vec<u8>, Decline> {
        let generation = self.feeds.latest(&source(desktop.word(), secure))?;
        Ok(prepared(&generation)?.render(updated_at, now))
    }

    /// Cache-only dashboards in Mac then Windows order for the hub. The transport-neutral
    /// publications never offer secure sign-in actions to a different consumer.
    pub fn hub_dashboards(&self, updated_at: &str, now: i64) -> Vec<Value> {
        [Desktop::Mac, Desktop::Windows]
            .into_iter()
            .filter_map(|desktop| {
                let bytes = self.render(desktop, false, updated_at, now).ok()?;
                serde_json::from_slice(&bytes).ok()
            })
            .collect()
    }

    /// Choose the same unique accounts as the TS sampler before projecting store observations.
    pub fn store_observations(
        &self,
        updated_at: &str,
        now: i64,
        cutoff: i64,
    ) -> Result<Vec<aac_store::usage::Sampled>, Decline> {
        let dashboards = self.hub_dashboards(updated_at, now);
        let mut indices = HashMap::new();
        let mut rows: Vec<(Value, Option<i64>, bool)> = Vec::new();
        for dashboard in dashboards {
            let Some(accounts) = dashboard["accounts"].as_array() else {
                return Err(Decline::Shape);
            };
            for account in accounts {
                let key = crate::dashboard_observations::identity(account)?;
                let measured = account["sampledAt"]
                    .as_str()
                    .or_else(|| account["fetchedAt"].as_str());
                let measured = measured
                    .map(aac_views::js::parse_date)
                    .transpose()
                    .map_err(|_| Decline::Date)?
                    .flatten();
                let available = account["status"] == "ok" || account["status"] == "cached";
                match indices.entry(key) {
                    std::collections::hash_map::Entry::Vacant(slot) => {
                        slot.insert(rows.len());
                        rows.push((account.clone(), measured, available));
                    }
                    std::collections::hash_map::Entry::Occupied(slot) => {
                        let prior = &mut rows[*slot.get()];
                        if (available && !prior.2)
                            || (available == prior.2 && measured.is_some() && measured >= prior.1)
                        {
                            *prior = (account.clone(), measured, available);
                        }
                    }
                }
            }
        }
        let rows: Vec<_> = rows.into_iter().map(|row| row.0).collect();
        crate::dashboard_observations::observations(&rows, now, cutoff)
    }
}

pub(crate) fn source(platform: &str, secure: bool) -> String {
    format!("{HOME_DASHBOARD}{platform}-{}", u8::from(secure))
}

pub(crate) fn prepared(
    generation: &Arc<Generation>,
) -> Result<&aac_views::home::fed::Dashboard, Decline> {
    match &generation.prepared {
        Prepared::Dashboard(dashboard) => dashboard.as_ref().map_err(|reason| *reason),
        _ => Err(Decline::Internal),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TS: i64 = 1_767_225_600_000;
    const ISO: &str = "2026-01-01T00:00:00.000Z";

    fn row(status: &str, source: &str, pct: u32) -> Value {
        json!({
            "id": "codex:fixture", "provider": "codex", "email": "User@example.test",
            "source": source, "platform": "mac", "host": "alexis",
            "plan": "pro", "status": status, "isActive": true, "sampledAt": ISO,
            "windows": [{
                "key":"five_hour", "kind":"rate_limit", "label":"Five hours", "unit":"tokens",
                "usedPercent":pct, "resetAt": ISO, "poolId":"pool:fixture",
                "poolIdSource":"provider-id", "poolLabel":" Example pool ",
                "modelIds":["gpt-5", "https://fixture.invalid", "sk-fixture"]
            }]
        })
    }

    fn dashboard(account: Value) -> Value {
        json!({"schemaVersion":1, "updatedAt":ISO, "accounts":[account]})
    }

    fn facts() -> Value {
        json!({"order":{}, "epochs":{ISO: TS}})
    }

    #[test]
    fn latest_snapshot_keeps_the_transport_scope_and_store_prefers_usable_desktop_quota() {
        let cache = AccountUsageCache::default();
        let publish = |desktop, secure, generation, row| {
            cache
                .publish(desktop, secure, generation, dashboard(row), facts())
                .expect("fixture dashboard")
        };
        assert!(publish(
            Desktop::Mac,
            false,
            2,
            row("ok", "Fixture Mac", 25)
        ));
        assert!(!publish(Desktop::Mac, false, 1, row("ok", "Old Mac", 33)));
        assert!(publish(Desktop::Mac, true, 3, row("ok", "Secure Mac", 30)));
        assert!(publish(
            Desktop::Windows,
            false,
            3,
            row("unavailable", "Fixture Windows", 75)
        ));

        let pair = cache.hub_dashboards(ISO, TS);
        assert_eq!(pair.len(), 2);
        assert_eq!(pair[0]["accounts"][0]["source"], "Fixture Mac");
        assert_eq!(pair[1]["accounts"][0]["source"], "Fixture Windows");
        let observations = cache
            .store_observations(ISO, TS, TS - 1000)
            .expect("observations");
        assert_eq!(observations.len(), 1);
        let observation = &observations[0];
        assert_eq!(observation.account.source, "Fixture Mac");
        assert_eq!(observation.account.platform, "windows");
        assert_eq!(observation.sampled_at_ms, TS);
        assert_eq!(observation.windows[0].used_pct, Some(25.0));
        assert_eq!(
            observation.windows[0].identity,
            r#"["five_hour","rate_limit","tokens","pool:fixture"]"#
        );
        assert_eq!(
            observation.windows[0].pool_label.as_deref(),
            Some("Example pool")
        );
        assert_eq!(observation.windows[0].model_ids, Some(vec!["gpt-5".into()]));
    }
}
