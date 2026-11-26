//! G7's per-account Re-check: one source through the native collector, its row
//! published into the shared generation cache under lane J's guards.
use super::Producer;
use crate::account_usage::coordinator::UsageIdentity;
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
    /// The stored key a per-key OpenCode Go row reads (card OCG1); None for a registry entry.
    pub pin: Option<UsageIdentity>,
}

/// A per-key OpenCode Go row id (`<discover entry id>:<identity digest>`, card OCG1): the
/// entry id it expands from, or None for any other id.
pub(crate) fn discovered_key_id(id: &str) -> Option<&str> {
    let (base, digest) = id.rsplit_once(':')?;
    (base.starts_with("opencode-go:")
        && sources::is_additional_account_id(base)
        && digest.len() == 8
        && digest
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then_some(base)
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

pub(crate) async fn lookup(
    root: std::path::PathBuf,
    id: &str,
) -> Result<Option<Resolved>, Decline> {
    let id = id.to_owned();
    tokio::task::spawn_blocking(move || match sources::read_registry(&root) {
        Ok(sources::Sources::V2(rows)) => {
            Ok(rows
                .into_iter()
                .find(|source| source.id == id)
                .map(|source| Resolved {
                    source,
                    legacy: false,
                    pin: None,
                }))
        }
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
                    pin: None,
                }))
        }
        Err(reason) => Err(reason),
    })
    .await
    .map_err(|_| Decline::Internal)?
}

impl Producer {
    /// The account's current source binding, or None when the list is unreadable,
    /// invalid, or does not name the account (the TS refreshAdditional lookup).
    pub async fn resolve_recheck(&self, id: &str) -> Option<Resolved> {
        self.lookup_recheck(id).await.ok().flatten()
    }

    pub(crate) async fn lookup_recheck(&self, id: &str) -> Result<Option<Resolved>, Decline> {
        let base = match lookup(self.root.clone(), id).await? {
            // A discover entry that may now stand for per-key rows resolves through the expansion
            // below: once its stored keys have rows (or are all named) it is not read unpinned.
            Some(found)
                if found.source.provider == "opencode-go"
                    && matches!(found.source.credential, sources::Credential::Discover) =>
            {
                found.source.id
            }
            Some(found) => return Ok(Some(found)),
            None => match discovered_key_id(id) {
                Some(base) => base.to_owned(),
                None => return Ok(None),
            },
        };
        let root = self.root.clone();
        let (rows, legacy) =
            tokio::task::spawn_blocking(move || match sources::read_registry(&root) {
                Ok(sources::Sources::V2(rows)) => Ok((rows, false)),
                Ok(sources::Sources::Legacy) => {
                    let (rows, valid) = sources::legacy(Some(&root));
                    if !valid {
                        return Err(Decline::Shape);
                    }
                    Ok((rows, true))
                }
                Err(reason) => Err(reason),
            })
            .await
            .map_err(|_| Decline::Internal)??;
        if !rows.iter().any(|source| source.id == base) {
            return Ok(None);
        }
        // The same expansion and pinning as sampling: only a stored key that no other entry
        // names has a row, so a digest of a named key (a duplicate) stays unknown.
        Ok(self
            .expand_discovery(rows, legacy)
            .await
            .into_iter()
            .find(|(source, _)| source.id == id)
            .map(|(source, pin)| Resolved {
                source,
                legacy,
                pin,
            }))
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
        self.pinned_row(
            &resolved.source,
            resolved.pin.as_ref(),
            resolved.legacy,
            now,
            force,
        )
        .await
    }

    /// Probe a tentative key through the same provider collector as a dashboard re-check.
    pub(crate) async fn probe_key(&self, source: &Source, now: i64) -> Value {
        self.additional_row(source, false, now, true).await
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
        if !same_binding(&current.source, expected) || current.source.id != expected.id {
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

    /// Force-refresh a pasted-session OpenCode console wallet row (`plan-opencode-go-console-web-*`).
    pub(crate) async fn recheck_web_wallet(
        &self,
        cache: &AccountUsageCache,
        companions: &CompanionInputs,
        id: &str,
    ) -> Result<Option<Value>, Decline> {
        let Ok(now) = aac_core::UtcMs::now() else {
            return Err(Decline::Date);
        };
        let now_ms = now.get() as i64;
        let force = {
            let mut forced = self
                .recheck_forced
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            forced_refresh(&mut forced, id, now_ms)
        };
        let Some(row) = self.wallet(now_ms, force).await else {
            return Ok(None);
        };
        if row["id"].as_str() != Some(id) {
            return Ok(None);
        }
        for desktop in [Desktop::Mac, Desktop::Windows] {
            for secure in [false, true] {
                let _ = cache.replace_row(desktop, secure, id, row.clone(), &companions.lifecycle);
            }
        }
        Ok(Some(row))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_usage::{dashboard, source};
    use aac_files::registry::Owners;
    use serde_json::json;
    use std::fs;
    use std::path::Path;

    fn set_mode(path: &Path, mode: u32) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("fixture mode");
        }
        #[cfg(not(unix))]
        let _ = (path, mode);
    }

    fn registry(root: &Path, accounts: Value) {
        let file = root.join("account-usage-accounts.json");
        fs::write(
            &file,
            serde_json::to_vec(&json!({"version":2,"accounts":accounts})).expect("registry"),
        )
        .expect("write registry");
        set_mode(&file, 0o600);
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
        set_mode(&root, 0o700);
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
            br#"{"version":1,"sources":[{"provider":"bogus","platform":"ubuntu"}]}"#,
        )
        .expect("manifest");
        set_mode(&root.join("account-usage-sources.json"), 0o600);
        assert!(producer.resolve_recheck("cursor:usage").await.is_none());
    }

    /// Lane J's PublishIfCurrent over the shared cache: one row is replaced in
    /// every held selector, unrelated rows of the latest concurrent generation
    /// stay, and a host or credential change publishes nothing.
    #[tokio::test]
    async fn a_recheck_replaces_one_row_only_while_its_binding_holds() {
        let dir = tempfile::tempdir().expect("fixture root");
        let root = fs::canonicalize(dir.path()).expect("canonical root");
        set_mode(&root, 0o700);
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

    /// Fixture helper transport: OpenCode Go stores on Ubuntu with three keys (a, b, c), two of
    /// them also held by the registry's key slots; a collect answers the asked key's own reading.
    struct StoredKeys {
        collected: std::sync::Mutex<Vec<String>>,
    }

    impl crate::account_usage::coordinator::FailoverDeps for StoredKeys {
        fn transport<'a>(
            &'a self,
            _host: &'a crate::account_usage::coordinator::UsageHost,
            request: &'a Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, ()>> + Send + 'a>>
        {
            Box::pin(async move {
                let stored = |locator: &str, selector: &str, key: &str| {
                    json!({
                        "locator":locator.repeat(64), "selectors":[selector], "revision":"2".repeat(64),
                        "identity":{"kind":"opencode-go-credential","key":key.repeat(64)}
                    })
                };
                if request["action"] == "inventory" {
                    return Ok(json!({"version":1,"action":"inventory","candidates":[
                        stored("1", "discover", "a"), stored("3", "discover", "b"),
                        stored("5", "discover", "c"), stored("7", "aac-key:2e034ec2", "a"),
                        stored("9", "aac-key:52f3251b", "b")
                    ]}));
                }
                let key = request["candidate"]["identity"]["key"]
                    .as_str()
                    .unwrap_or("")
                    .get(..1)
                    .unwrap_or("")
                    .to_owned();
                self.collected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(key.clone());
                let used = match key.as_str() {
                    "a" => 11,
                    "b" => 22,
                    "c" => 33,
                    _ => 99,
                };
                let at = "2026-11-21T17:00:00.000Z";
                Ok(json!({
                    "version":1,"action":"collect","candidate":request["candidate"],"failureCode":null,
                    "sample":{"status":"ok","sampledAt":at,"fetchedAt":at,
                        "windows":[{"key":"weekly","label":"Weekly","usedPercent":used}]}
                }))
            })
        }

        fn hosts<'a>(
            &'a self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Vec<crate::account_usage::coordinator::UsageHost>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Vec::new() })
        }

        fn now(&self) -> f64 {
            1_795_280_400_000.0
        }
    }

    /// Card OCG1 follow-up: Re-check finds each per-key OpenCode Go row and reads that one key,
    /// with the same pinning and no-duplicate rules as sampling; an unknown id stays unknown.
    #[tokio::test]
    async fn a_recheck_reads_each_stored_opencode_go_key_of_its_own_row() {
        let dir = tempfile::tempdir().expect("fixture root");
        let root = fs::canonicalize(dir.path()).expect("canonical root");
        set_mode(&root, 0o700);
        let entry = |id: &str, credential: Value| {
            json!({
                "id":id,"provider":"opencode-go","platform":"ubuntu","sshHost":null,
                "label":"OpenCode Go","credential":credential,"createdAt":null,"createdBy":null
            })
        };
        registry(
            &root,
            json!([
                entry("opencode-go:usage", json!({"kind":"discover"})),
                entry(
                    "opencode-go:acct:52f3251b",
                    json!({"kind":"aac-key","keyId":"52f3251b"})
                ),
                entry(
                    "opencode-go:acct:2e034ec2",
                    json!({"kind":"aac-key","keyId":"2e034ec2"})
                ),
            ]),
        );
        let owners = Owners::defaults(aac_files::registry::rows().expect("bundled private files"));
        let deps = std::sync::Arc::new(StoredKeys {
            collected: std::sync::Mutex::default(),
        });
        let mut producer = Producer::new(root.clone(), owners.clone());
        producer.failover = std::sync::Arc::new(
            crate::account_usage::coordinator::FailoverCoordinator::new(deps.clone(), owners),
        );
        let rows = [
            ("opencode-go:usage:cccccccc", "c", 33.0),
            ("opencode-go:acct:52f3251b", "b", 22.0),
            ("opencode-go:acct:2e034ec2", "a", 11.0),
        ];
        let cache = AccountUsageCache::default();
        publish(
            &cache,
            rows.iter()
                .map(|(id, _, _)| row(id, "opencode-go"))
                .collect(),
        );
        let companions = CompanionInputs::default();
        for (id, key, used) in rows {
            deps.collected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            let answer = producer
                .recheck(&cache, &companions, id)
                .await
                .expect("recheck")
                .unwrap_or_else(|| panic!("{id} must resolve"));
            assert_eq!(answer["id"], id);
            assert_eq!(answer["status"], "ok", "{id}: {answer}");
            assert_eq!(
                answer["windows"][0]["usedPercent"].as_f64(),
                Some(used),
                "{id}"
            );
            assert_eq!(
                *deps.collected.lock().unwrap_or_else(|e| e.into_inner()),
                [key],
                "{id} reads its own key, once"
            );
            let (_, dashboard) = held(&cache, "mac", false);
            let published = dashboard["accounts"]
                .as_array()
                .expect("accounts")
                .iter()
                .find(|account| account["id"] == id)
                .cloned()
                .expect("published row");
            assert_eq!(
                published["windows"][0]["usedPercent"].as_f64(),
                Some(used),
                "{id}"
            );
        }
        // Unknown digests, and the digest of a key another entry already names (no duplicate
        // row), stay "not found" and read nothing.
        deps.collected
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        for unknown in [
            "opencode-go:usage",
            "opencode-go:usage:dddddddd",
            "opencode-go:usage:aaaaaaaa",
            "opencode-go:acct:99999999",
            "zai:usage:cccccccc",
        ] {
            assert!(
                producer
                    .recheck(&cache, &companions, unknown)
                    .await
                    .expect("recheck")
                    .is_none(),
                "{unknown}"
            );
        }
        assert!(
            deps.collected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        );
    }

    #[test]
    fn only_a_digest_after_an_opencode_go_entry_id_is_a_per_key_row() {
        assert_eq!(
            discovered_key_id("opencode-go:usage:cccccccc"),
            Some("opencode-go:usage")
        );
        for other in [
            "opencode-go:usage",
            "opencode-go:acct:52f3251b",
            "opencode-go:usage:CCCCCCCC",
            "opencode-go:usage:ccccccc",
            "zai:usage:cccccccc",
            "codex:work:cccccccc",
        ] {
            assert_eq!(discovered_key_id(other), None, "{other}");
        }
    }

    /// Three-key inventory without named registry entries: each key returns its own reading.
    struct ThreeStoredKeys {
        now: std::sync::atomic::AtomicI64,
        unreadable: std::sync::atomic::AtomicBool,
        collected: std::sync::Mutex<Vec<String>>,
    }

    impl crate::account_usage::coordinator::FailoverDeps for ThreeStoredKeys {
        fn transport<'a>(
            &'a self,
            _host: &'a crate::account_usage::coordinator::UsageHost,
            request: &'a Value,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Value, ()>> + Send + 'a>>
        {
            Box::pin(async move {
                if request["provider"] != "opencode-go" {
                    return Ok(json!({"version":1,"action":"inventory","candidates":[]}));
                }
                let stored = |locator: &str, key: &str| {
                    json!({
                        "locator":locator.repeat(64), "selectors":["discover"], "revision":"2".repeat(64),
                        "identity":{"kind":"opencode-go-credential","key":key.repeat(64)}
                    })
                };
                if request["action"] == "inventory" {
                    if self.unreadable.load(std::sync::atomic::Ordering::Relaxed) {
                        return Ok(json!({
                            "version":1,"action":"inventory","candidates":[],
                            "incompleteSelectors":["*"]
                        }));
                    }
                    return Ok(json!({"version":1,"action":"inventory","candidates":[
                        stored("1", "a"), stored("3", "b"), stored("5", "c")
                    ]}));
                }
                let key = request["candidate"]["identity"]["key"]
                    .as_str()
                    .unwrap_or("")
                    .get(..1)
                    .unwrap_or("")
                    .to_owned();
                self.collected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(key.clone());
                let used = match key.as_str() {
                    "a" => 11,
                    "b" => 22,
                    "c" => 33,
                    _ => 99,
                };
                let at = aac_num::ecma::to_iso_string(
                    self.now.load(std::sync::atomic::Ordering::Relaxed),
                )
                .unwrap_or_else(|| "2026-11-21T17:00:00.000Z".into());
                Ok(json!({
                    "version":1,"action":"collect","candidate":request["candidate"],"failureCode":null,
                    "sample":{"status":"ok","sampledAt":at,"fetchedAt":at,
                        "windows":[{"key":"weekly","label":"Weekly","usedPercent":used}]}
                }))
            })
        }

        fn hosts<'a>(
            &'a self,
        ) -> std::pin::Pin<
            Box<
                dyn std::future::Future<Output = Vec<crate::account_usage::coordinator::UsageHost>>
                    + Send
                    + 'a,
            >,
        > {
            Box::pin(async { Vec::new() })
        }

        fn now(&self) -> f64 {
            self.now.load(std::sync::atomic::Ordering::Relaxed) as f64
        }
    }

    /// Astra Medium 1: the same three-key inventory expands to three pinned requests and three
    /// rows for sampling and single-row Re-check under default, version 1 and version 2.
    #[tokio::test]
    async fn three_stored_opencode_go_keys_expand_for_default_v1_and_v2() {
        let expected = [
            ("opencode-go:usage:aaaaaaaa", "Key aaaaaaaa", "a", 11.0),
            ("opencode-go:usage:bbbbbbbb", "Key bbbbbbbb", "b", 22.0),
            ("opencode-go:usage:cccccccc", "Key cccccccc", "c", 33.0),
        ];
        for mode in ["default", "v1", "v2"] {
            let dir = tempfile::tempdir().expect("fixture root");
            let root = fs::canonicalize(dir.path()).expect("canonical root");
            set_mode(&root, 0o700);
            match mode {
                "default" => {}
                "v1" => {
                    let manifest = root.join("account-usage-sources.json");
                    fs::write(
                        &manifest,
                        serde_json::to_vec(&json!({
                            "version": 1,
                            "sources": [{"provider": "opencode-go", "platform": "ubuntu"}]
                        }))
                        .expect("manifest"),
                    )
                    .expect("write manifest");
                    set_mode(&manifest, 0o600);
                }
                "v2" => {
                    registry(
                        &root,
                        json!([{
                            "id":"opencode-go:usage","provider":"opencode-go","platform":"ubuntu",
                            "sshHost":null,"label":"OpenCode Go","credential":{"kind":"discover"},
                            "createdAt":null,"createdBy":"migration"
                        }]),
                    );
                }
                _ => unreachable!(),
            }
            let owners =
                Owners::defaults(aac_files::registry::rows().expect("bundled private files"));
            let deps = std::sync::Arc::new(ThreeStoredKeys {
                now: std::sync::atomic::AtomicI64::new(1_795_280_400_000),
                unreadable: std::sync::atomic::AtomicBool::new(false),
                collected: std::sync::Mutex::default(),
            });
            let mut producer = Producer::new(root.clone(), owners.clone());
            producer.failover = std::sync::Arc::new(
                crate::account_usage::coordinator::FailoverCoordinator::new(deps.clone(), owners),
            );
            let disk = super::super::Disk::load(&root).expect("load disk");
            let sources: Vec<Source> = disk
                .sources
                .into_iter()
                .filter(|source| source.provider == "opencode-go")
                .collect();
            assert_eq!(
                sources.len(),
                1,
                "{mode}: one implicit or v2 discover entry"
            );
            let rows = producer
                .additional(
                    sources,
                    disk.legacy,
                    disk.source_problem,
                    false,
                    1_795_280_400_000,
                    false,
                )
                .await;
            let got: Vec<(String, String, Option<f64>)> = rows
                .iter()
                .map(|row| {
                    (
                        row["id"].as_str().unwrap_or("").to_owned(),
                        row["label"].as_str().unwrap_or("").to_owned(),
                        row["windows"][0]["usedPercent"].as_f64(),
                    )
                })
                .collect();
            let want: Vec<(String, String, Option<f64>)> = expected
                .iter()
                .map(|(id, label, _, used)| ((*id).to_owned(), (*label).to_owned(), Some(*used)))
                .collect();
            assert_eq!(got, want, "{mode}: sampling rows");
            let mut sampled = deps
                .collected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            sampled.sort();
            assert_eq!(sampled, ["a", "b", "c"], "{mode}: three pinned requests");

            let cache = AccountUsageCache::default();
            publish(&cache, rows);
            let companions = CompanionInputs::default();
            deps.now
                .fetch_add(120_000, std::sync::atomic::Ordering::Relaxed);
            for (id, _, key, used) in expected {
                deps.collected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clear();
                let answer = producer
                    .recheck(&cache, &companions, id)
                    .await
                    .expect("recheck")
                    .unwrap_or_else(|| panic!("{mode}: {id} must resolve"));
                assert_eq!(answer["id"], id, "{mode}");
                assert_eq!(answer["status"], "ok", "{mode}: {answer}");
                assert_eq!(
                    answer["windows"][0]["usedPercent"].as_f64(),
                    Some(used),
                    "{mode}: {id}"
                );
                assert_eq!(
                    *deps.collected.lock().unwrap_or_else(|e| e.into_inner()),
                    [key],
                    "{mode}: {id} reads only its own key"
                );
            }
            // Bare discover id and unknown digests stay 404 and read nothing once expanded.
            deps.collected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clear();
            for unknown in ["opencode-go:usage", "opencode-go:usage:dddddddd"] {
                assert!(
                    producer
                        .recheck(&cache, &companions, unknown)
                        .await
                        .expect("recheck")
                        .is_none(),
                    "{mode}: {unknown}"
                );
            }
            assert!(
                deps.collected
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_empty(),
                "{mode}: unknown ids must not collect"
            );

            // Unreadable store keeps the bare discover entry as it was.
            deps.unreadable
                .store(true, std::sync::atomic::Ordering::Relaxed);
            deps.now
                .fetch_add(120_000, std::sync::atomic::Ordering::Relaxed);
            let fallback_disk = super::super::Disk::load(&root).expect("load disk");
            let fallback_sources: Vec<Source> = fallback_disk
                .sources
                .into_iter()
                .filter(|source| source.provider == "opencode-go")
                .collect();
            let fallback_rows = producer
                .additional(
                    fallback_sources,
                    fallback_disk.legacy,
                    fallback_disk.source_problem,
                    false,
                    deps.now.load(std::sync::atomic::Ordering::Relaxed),
                    false,
                )
                .await;
            assert_eq!(
                fallback_rows
                    .iter()
                    .filter_map(|row| row["id"].as_str())
                    .collect::<Vec<_>>(),
                ["opencode-go:usage"],
                "{mode}: unreadable store keeps bare entry"
            );
            assert!(
                producer
                    .resolve_recheck("opencode-go:usage")
                    .await
                    .is_some(),
                "{mode}: unreadable store keeps bare recheck"
            );
        }

        // An invalid v1 manifest still refuses expansion and single-row recheck.
        let dir = tempfile::tempdir().expect("invalid v1 root");
        let root = fs::canonicalize(dir.path()).expect("canonical root");
        set_mode(&root, 0o700);
        let manifest = root.join("account-usage-sources.json");
        fs::write(
            &manifest,
            br#"{"version":1,"sources":[{"provider":"bogus","platform":"ubuntu"}]}"#,
        )
        .expect("write invalid manifest");
        set_mode(&manifest, 0o600);
        let owners = Owners::defaults(aac_files::registry::rows().expect("bundled private files"));
        let deps = std::sync::Arc::new(ThreeStoredKeys {
            now: std::sync::atomic::AtomicI64::new(1_795_280_400_000),
            unreadable: std::sync::atomic::AtomicBool::new(false),
            collected: std::sync::Mutex::default(),
        });
        let mut producer = Producer::new(root.clone(), owners.clone());
        producer.failover = std::sync::Arc::new(
            crate::account_usage::coordinator::FailoverCoordinator::new(deps.clone(), owners),
        );
        let disk = super::super::Disk::load(&root).expect("load invalid v1");
        assert!(disk.source_problem.is_some());
        let rows = producer
            .additional(
                disk.sources
                    .into_iter()
                    .filter(|s| s.provider == "opencode-go")
                    .collect(),
                disk.legacy,
                disk.source_problem,
                false,
                1_795_280_400_000,
                false,
            )
            .await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["id"], "opencode-go:usage");
        assert_eq!(rows[0]["status"], "error");
        assert!(
            producer
                .resolve_recheck("opencode-go:usage:aaaaaaaa")
                .await
                .is_none()
        );
        assert!(
            deps.collected
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
        );
    }
}
