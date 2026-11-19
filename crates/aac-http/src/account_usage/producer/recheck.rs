//! G7's per-account Re-check: one source through the native collector, its row
//! published into the shared generation cache under lane J's guards.
use super::Producer;
use crate::account_usage::producer::CompanionInputs;
use crate::account_usage::{AccountUsageCache, Desktop, sources, sources::Source};
use crate::lifecycle_recheck;
use aac_views::decline::Decline;
use serde_json::Value;
use std::collections::HashMap;

/// One account source resolved from the registry before a Re-check collects.
pub struct Resolved {
    /// The registered source, exactly as the whole-generation pass reads it.
    pub source: Source,
    /// The version 1 manifest supplied this source.
    pub legacy: bool,
}

/// The TS per-source forced-refresh debounce (`REFRESH_DEBOUNCE_MS`).
const REFRESH_DEBOUNCE_MS: i64 = 5_000;

/// The TS `force = refresh && now - cache.refreshedAt >= REFRESH_DEBOUNCE_MS`:
/// the first Re-check and one after the window force a read; a second inside it
/// keeps the collector's ordinary cache. The stamp moves only when it forces.
fn forced_refresh(forced: &mut HashMap<String, i64>, id: &str, now: i64) -> bool {
    let last = forced.get(id).copied().unwrap_or(i64::MIN);
    let force = now.saturating_sub(last) >= REFRESH_DEBOUNCE_MS;
    if force {
        forced.insert(id.to_owned(), now);
    }
    force
}

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
        self.lookup_recheck(id).await.ok().flatten()
    }

    pub(crate) async fn lookup_recheck(&self, id: &str) -> Result<Option<Resolved>, Decline> {
        let root = self.root.clone();
        let id = id.to_owned();
        tokio::task::spawn_blocking(move || match sources::read_registry(&root) {
            Ok(sources::Sources::V2(rows)) => Ok(rows
                .into_iter()
                .find(|source| source.id == id)
                .map(|source| Resolved {
                    source,
                    legacy: false,
                })),
            Ok(sources::Sources::Legacy) => {
                let (rows, valid) = sources::legacy(Some(&root));
                if !valid {
                    return Err(Decline::Shape);
                }
                Ok(rows
                    .into_iter()
                    .find(|source| source.id == id)
                    .map(|source| Resolved {
                        source,
                        legacy: true,
                    }))
            }
            Err(reason) => Err(reason),
        })
        .await
        .map_err(|_| Decline::Internal)?
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
            forced_refresh(&mut forced, &resolved.source.id, now)
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
        self.recheck_resolved(cache, companions, resolved).await
    }

    pub(crate) async fn recheck_resolved(
        &self,
        cache: &AccountUsageCache,
        companions: &CompanionInputs,
        resolved: Resolved,
    ) -> Result<Option<Value>, Decline> {
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
                self.publish_recheck_row(cache, companions, &expected.id, &expected, row)
                    .await
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_usage::{dashboard, source};
    use aac_files::registry::Owners;
    use serde_json::json;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    fn registry(root: &Path, accounts: Value) {
        let file = root.join("account-usage-accounts.json");
        fs::write(
            &file,
            serde_json::to_vec(&json!({"version":2,"accounts":accounts})).expect("registry"),
        )
        .expect("write registry");
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).expect("registry mode");
    }

    fn row(id: &str, provider: &str) -> Value {
        json!({
            "id":id,"provider":provider,"providerLabel":provider,"label":id,
            "email":null,"plan":null,"platform":"ubuntu","source":"fixture",
            "status":"ok","message":null,"fetchedAt":null,"sampledAt":null,
            "isActive":false,"windows":[],
            "capabilities":{"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]}
        })
    }

    fn publish(cache: &AccountUsageCache, rows: Vec<Value>) {
        let visibility = dashboard::empty_visibility();
        for desktop in [Desktop::Mac, Desktop::Windows] {
            for secure in [false, true] {
                let sample = dashboard::build(
                    desktop,
                    secure,
                    dashboard::Input {
                        rows: rows.clone(),
                        visibility: &visibility,
                        visibility_available: true,
                        refresh_seconds: 60,
                        lifecycle: &dashboard::Lifecycle::default(),
                        signins: None,
                        server: None,
                    },
                )
                .expect("fixture generation");
                cache
                    .publish(desktop, secure, 10, sample.dashboard, sample.facts)
                    .expect("fixture publish");
            }
        }
    }

    fn held(cache: &AccountUsageCache, desktop: &str, secure: bool) -> (String, Value) {
        let generation = cache
            .feeds
            .latest(&source(desktop, secure))
            .expect("held generation");
        (
            generation.id.clone(),
            serde_json::from_str(generation.data.get()).expect("dashboard"),
        )
    }

    #[test]
    fn a_recheck_forces_one_read_per_five_second_window() {
        let mut forced = HashMap::new();
        assert!(forced_refresh(&mut forced, "zai:usage", 1_000_000));
        assert!(!forced_refresh(&mut forced, "zai:usage", 1_004_999));
        assert!(forced_refresh(&mut forced, "zai:usage", 1_005_000));
        assert!(forced_refresh(&mut forced, "cursor:usage", 1_005_000));
    }

    #[test]
    fn the_publication_guard_ignores_only_the_label() {
        let source = Source {
            id: "zai:usage".into(),
            provider: "zai".into(),
            platform: "ubuntu".into(),
            ssh_host: None,
            label: Some("Z.ai".into()),
            credential: sources::Credential::Discover,
            created_at: None,
            created_by: None,
        };
        let mut relabelled = source.clone();
        relabelled.label = Some("Work".into());
        assert!(same_binding(&source, &relabelled));
        for changed in [
            Source {
                provider: "cursor".into(),
                ..source.clone()
            },
            Source {
                platform: "windows".into(),
                ..source.clone()
            },
            Source {
                ssh_host: Some("nas1".into()),
                ..source.clone()
            },
            Source {
                credential: sources::Credential::AacKey {
                    key_id: "0123abcd".into(),
                },
                ..source.clone()
            },
        ] {
            assert!(!same_binding(&source, &changed), "{changed:?}");
        }
    }

    /// The version 1 manifest keeps its existing policy: a valid one supplies
    /// the default source, an invalid one names nothing, and no account is 404.
    #[tokio::test]
    async fn a_legacy_manifest_resolves_only_its_valid_defaults() {
        let dir = tempfile::tempdir().expect("fixture root");
        let root = fs::canonicalize(dir.path()).expect("canonical root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("root mode");
        let producer = Producer::new(
            root.clone(),
            Owners::defaults(aac_files::registry::rows().expect("bundled private files")),
        );
        let resolved = producer
            .resolve_recheck("cursor:usage")
            .await
            .expect("default source");
        assert!(resolved.legacy);
        assert_eq!(resolved.source.provider, "cursor");
        assert_eq!(resolved.source.platform, "ubuntu");
        assert!(
            producer
                .resolve_recheck("cursor:acct:00000000")
                .await
                .is_none()
        );
        fs::write(
            root.join("account-usage-sources.json"),
            br#"{"version":1,"sources":[{"provider":"bogus","platform":"ubuntu"}]}"#.to_vec(),
        )
        .expect("manifest");
        fs::set_permissions(
            root.join("account-usage-sources.json"),
            fs::Permissions::from_mode(0o600),
        )
        .expect("manifest mode");
        assert!(producer.resolve_recheck("cursor:usage").await.is_none());
    }

    /// Lane J's PublishIfCurrent over the shared cache: one row is replaced in
    /// every held selector, unrelated rows of the latest concurrent generation
    /// stay, and a host or credential change publishes nothing.
    #[tokio::test]
    async fn a_recheck_replaces_one_row_only_while_its_binding_holds() {
        let dir = tempfile::tempdir().expect("fixture root");
        let root = fs::canonicalize(dir.path()).expect("canonical root");
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).expect("root mode");
        registry(
            &root,
            json!([
                json!({
                    "id":"zai:usage","provider":"zai","platform":"ubuntu",
                    "sshHost":null,"label":"Z.ai","credential":{"kind":"discover"},
                    "createdAt":null,"createdBy":null
                }),
                json!({
                    "id":"cursor:usage","provider":"cursor","platform":"ubuntu",
                    "sshHost":null,"label":null,"credential":{"kind":"discover"},
                    "createdAt":null,"createdBy":null
                })
            ]),
        );
        let producer = Producer::new(
            root.clone(),
            Owners::defaults(aac_files::registry::rows().expect("bundled private files")),
        );
        let expected = producer
            .resolve_recheck("zai:usage")
            .await
            .expect("registered account");
        assert!(!expected.legacy);

        let cache = AccountUsageCache::default();
        publish(
            &cache,
            vec![row("zai:usage", "zai"), row("cursor:usage", "cursor")],
        );
        let companions = CompanionInputs::default();
        let rechecked = json!({
            "id":"zai:usage","provider":"zai","providerLabel":"Z.ai coding plan","label":"Z.ai",
            "email":null,"plan":null,"platform":"ubuntu","source":"Z.ai verified quota on Ubuntu",
            "status":"cached","message":"The last reading is shown.","fetchedAt":null,
            "sampledAt":null,"isActive":false,"windows":[],
            "capabilities":{"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]}
        });
        assert_eq!(
            producer
                .publish_recheck_row(
                    &cache,
                    &companions,
                    "zai:usage",
                    &expected.source,
                    rechecked
                )
                .await,
            Ok(true)
        );
        for (desktop, secure) in [
            ("mac", false),
            ("mac", true),
            ("windows", false),
            ("windows", true),
        ] {
            let (generation, dashboard) = held(&cache, desktop, secure);
            assert_ne!(generation, "10");
            let accounts = dashboard["accounts"].as_array().expect("accounts");
            let target = accounts
                .iter()
                .find(|account| account["id"] == "zai:usage")
                .expect("rechecked row");
            assert_eq!(target["status"], "cached");
            assert_eq!(target["message"], "The last reading is shown.");
            assert_eq!(target["hidden"], false);
            assert_eq!(target["trayHidden"], false);
            assert_eq!(target["lifecycle"], json!({"state":"ready","jobId":null}));
            let untouched = accounts
                .iter()
                .find(|account| account["id"] == "cursor:usage")
                .expect("unrelated row");
            assert_eq!(untouched["status"], "ok");
        }
        let before = held(&cache, "mac", false).0;

        // The registered host changes after collection: nothing is published.
        registry(
            &root,
            json!([{
                "id":"zai:usage","provider":"zai","platform":"ubuntu","sshHost":"nas1",
                "label":"Z.ai","credential":{"kind":"discover"},"createdAt":null,"createdBy":null
            }]),
        );
        assert_eq!(
            producer
                .publish_recheck_row(
                    &cache,
                    &companions,
                    "zai:usage",
                    &expected.source,
                    json!({"id":"zai:usage"})
                )
                .await,
            Ok(false)
        );
        // The credential selector changes: nothing is published.
        registry(
            &root,
            json!([{
                "id":"zai:usage","provider":"zai","platform":"ubuntu","sshHost":null,
                "label":"Z.ai","credential":{"kind":"aac-key","keyId":"0123abcd"},
                "createdAt":null,"createdBy":null
            }]),
        );
        assert_eq!(
            producer
                .publish_recheck_row(
                    &cache,
                    &companions,
                    "zai:usage",
                    &expected.source,
                    json!({"id":"zai:usage"})
                )
                .await,
            Ok(false)
        );
        // The account is removed: the Re-check answers 404 and publishes nothing.
        registry(&root, json!([]));
        assert_eq!(
            producer
                .publish_recheck_row(
                    &cache,
                    &companions,
                    "zai:usage",
                    &expected.source,
                    json!({"id":"zai:usage"})
                )
                .await,
            Ok(false)
        );
        assert_eq!(held(&cache, "mac", false).0, before);
    }
}
