//! Read the installed account sources and publish a complete Accounts generation.
use super::{AccountUsageCache, Desktop, dashboard, rows, sampler, sources};
use crate::{agent_transport::Agents, collection::Collection};
use aac_core::UtcMs;
use aac_files::{
    read::{Ancestors, Read, ReadPolicy},
    registry::Owners,
};
use aac_transport::CancellationToken;
use aac_views::decline::Decline;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::{Mutex, watch};

/// Inputs from the adjacent Rust owners. This contains no normalized dashboard
/// or provider rows collected by TypeScript.
#[derive(Clone, Default)]
pub struct CompanionInputs {
    /// Active sign-in jobs and available lifecycle routes.
    pub lifecycle: dashboard::Lifecycle,
    /// The native Antigravity bridge's public account rows.
    pub antigravity: Option<Vec<Value>>,
    /// Native sign-in health aggregation.
    pub signins: Option<Value>,
    /// Public listener information.
    pub server: Option<Value>,
}

/// The standalone collection, using the same L5 computer pool as the private service.
#[derive(Clone)]
pub struct Producer {
    root: PathBuf,
    agents: Arc<Agents>,
    collection: Arc<Collection>,
    owners: Owners,
    visibility: Arc<Mutex<Value>>,
}

impl Producer {
    /// Open the production readers for the given canonical service root and file owners.
    pub fn new(root: PathBuf, owners: Owners) -> Self {
        let agents = Arc::new(Agents::new(root.clone()));
        Self::shared(root, owners, agents, Arc::default())
    }

    pub(crate) fn shared(
        root: PathBuf,
        owners: Owners,
        agents: Arc<Agents>,
        collection: Arc<Collection>,
    ) -> Self {
        Self {
            root,
            agents,
            collection,
            owners,
            visibility: Arc::new(Mutex::new(dashboard::empty_visibility())),
        }
    }

    async fn ask(
        &self,
        platform: &str,
        alias: Option<&str>,
        verb: &str,
        args: &Value,
    ) -> Result<Value, &'static str> {
        self.collection
            .read_value(self.agents.clone(), platform, alias, verb, args)
            .await
    }

    /// Build all desktop/transport variants in one pass. Only the presentation
    /// differs between variants, so a provider is read once per generation.
    pub async fn collect(
        &self,
        companions: CompanionInputs,
    ) -> Result<Vec<sampler::DashboardSample>, Decline> {
        let root = self.root.clone();
        let disk = tokio::task::spawn_blocking(move || Disk::load(&root))
            .await
            .map_err(|_| Decline::Internal)??;
        let now = UtcMs::now().map_err(|_| Decline::Date)?.get() as i64;
        let visibility_available = disk.visibility.is_some();
        let visibility = {
            let mut current = self.visibility.lock().await;
            if let Some(visibility) = disk.visibility {
                *current = visibility;
            }
            current.clone()
        };

        let additional = self.additional(disk.sources, companions.antigravity.is_some());
        let codex = self.codex_rows(disk.codex, now);
        let claude = self.claude_rows(&disk.claude, now);
        let (additional, codex, claude) = tokio::join!(additional, codex, claude);

        let mut samples = Vec::with_capacity(4);
        for desktop in [Desktop::Mac, Desktop::Windows] {
            let mut rows = codex.clone();
            rows.extend(
                disk.claude
                    .iter()
                    .zip(&claude)
                    .filter_map(|(profile, raw)| rows::claude(profile, desktop, raw.as_ref())),
            );
            rows.extend(additional.iter().cloned());
            if let Some(antigravity) = &companions.antigravity {
                rows.extend(antigravity.iter().cloned());
            }
            for secure in [false, true] {
                samples.push(dashboard::build(
                    desktop,
                    secure,
                    dashboard::Input {
                        rows: rows.clone(),
                        visibility: &visibility,
                        visibility_available,
                        refresh_seconds: disk.refresh,
                        lifecycle: &companions.lifecycle,
                        signins: companions.signins.as_ref(),
                        server: companions.server.as_ref(),
                    },
                )?);
            }
        }
        Ok(samples)
    }

    /// Run production sampling until cancellation. A settings edit replaces the
    /// pending wait and publication hands the same cache to the listener and store.
    pub async fn run<C, P, Publish>(
        &self,
        cache: AccountUsageCache,
        cancel: CancellationToken,
        mut companions: C,
        published: P,
    ) where
        C: FnMut() -> CompanionInputs,
        P: FnMut(AccountUsageCache) -> Publish,
        Publish: std::future::Future<Output = ()>,
    {
        let root = self.root.clone();
        let initial = read_refresh(&root) * 1000;
        let (settings, interval) = watch::channel(initial);
        let monitor = async move {
            loop {
                tokio::time::sleep(Duration::from_secs(1)).await;
                let scope = root.clone();
                let next = tokio::task::spawn_blocking(move || read_refresh(&scope) * 1000).await;
                if let Ok(next) = next {
                    settings.send_if_modified(|previous| {
                        let changed = *previous != next;
                        *previous = next;
                        changed
                    });
                }
            }
        };
        let sampling = sampler::run(
            cache,
            interval,
            cancel,
            || {
                let inputs = companions();
                async move { self.collect(inputs).await.unwrap_or_default() }
            },
            published,
        );
        tokio::select! {
            () = sampling => {},
            () = monitor => {},
        }
    }

    async fn codex_rows(&self, profiles: Vec<String>, now: i64) -> Vec<Value> {
        use futures_util::{StreamExt, stream};
        if profiles.is_empty() {
            return vec![];
        }
        let platform = if cfg!(target_os = "macos") {
            "mac"
        } else {
            "ubuntu"
        };
        let args = json!({"version":1,"action":"inventory","provider":"codex","ccsDir":self.root});
        let inventory = self
            .ask(platform, None, "failover-inventory", &args)
            .await
            .ok();
        let candidate = |name: &str| pick(&inventory, "codex", &format!("codex-profile:{name}"));
        let active = candidate("default").map(|row| row["identity"].clone());
        let active = profiles
            .iter()
            .find(|name| {
                candidate(name).is_some_and(|row| Some(&row["identity"]) == active.as_ref())
            })
            .cloned();
        let requests = profiles.into_iter().map(|name| {
            let candidate = candidate(&name).cloned();
            let active = active.as_deref() == Some(&name);
            (name, candidate, active)
        });
        let inventory = &inventory;
        stream::iter(requests)
            .map(move |(name, candidate, active)| async move {
                let sample = match &candidate {
                    Some(candidate) => {
                        self.read_primary(platform, None, "codex", candidate, now)
                            .await
                    }
                    None => None,
                };
                let missing = format!("codex-profile:{name}");
                let signed_out = inventory_complete(inventory, &missing) && candidate.is_none();
                rows::codex(
                    &name,
                    active,
                    signed_out,
                    candidate.as_ref(),
                    sample.as_ref(),
                )
            })
            .buffered(3)
            .collect()
            .await
    }

    async fn read_primary(
        &self,
        platform: &str,
        alias: Option<&str>,
        provider: &str,
        candidate: &Value,
        now: i64,
    ) -> Option<Value> {
        let key = aac_core::sha256_hex(aac_views::js_json::write(&json!([
            self.root,
            provider,
            candidate["identity"]
        ])));
        let held = super::failover::persisted(&self.root, &key, now as f64)
            .ok()?
            .unwrap_or_default();
        if held.until > now as f64 {
            return None;
        }
        let mut args =
            json!({"version":1,"action":"collect","provider":provider,"candidate":candidate});
        if alias.is_none() {
            args["ccsDir"] = json!(self.root);
        }
        let reply = self
            .ask(platform, alias, "failover-collect", &args)
            .await
            .ok()?;
        if reply["failureCode"] == "rate_limited" || reply["rateLimited"] == true {
            let seconds = reply["retryAfterSeconds"]
                .as_f64()
                .unwrap_or(60.0)
                .clamp(60.0, 86_400.0);
            let hold = json!({"now":now,"until":now as f64 + seconds * 1000.0});
            let _ = super::failover::save(
                &self.owners,
                &self.root,
                &key,
                &aac_views::js_json::write(&hold),
            );
        }
        let final_candidate = &reply["candidate"];
        if ["identity", "revision", "locator"]
            .iter()
            .any(|field| final_candidate[*field] != candidate[*field])
        {
            return None;
        }
        reply
            .get("sample")
            .filter(|sample| sample.is_object())
            .cloned()
    }

    async fn claude_rows(&self, profiles: &[Value], now: i64) -> Vec<Option<Value>> {
        let root = self.root.clone();
        let profiles = profiles.to_vec();
        let count = profiles.len();
        tokio::task::spawn_blocking(move || {
            let raw = crate::profile_reads::desktop_manifest(&root)
                .flatten()
                .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
                .unwrap_or_default();
            profiles
                .iter()
                .map(|profile| {
                    let id = profile["id"].as_str()?;
                    let original = raw["profiles"]
                        .as_array()?
                        .iter()
                        .find(|entry| entry["id"] == id)?;
                    let hash = aac_core::sha256_hex(aac_views::js_json::write(original));
                    let mut sample =
                        super::provider_caches::claude_snapshot(&root, id, &hash).ok()??;
                    let stamp = rows::stamp(&sample["fetchedAt"])?;
                    let sampled = aac_views::js::parse_date(&stamp).ok()??;
                    if sampled > now
                        || sample["profileId"] != id
                        || sample["email"] != profile["email"]
                    {
                        return None;
                    }
                    sample["status"] = json!("cached");
                    Some(sample)
                })
                .collect()
        })
        .await
        .unwrap_or_else(|_| vec![None; count])
    }

    async fn additional(&self, sources: Vec<sources::Source>, antigravity: bool) -> Vec<Value> {
        // Keep stable account order and cap simultaneous local and remote dispatch.
        use futures_util::{StreamExt, stream};
        let mut sources = sources;
        let order = [
            "antigravity",
            "muse",
            "cursor",
            "kimi-code",
            "qwen",
            "zai",
            "opencode-go",
        ];
        sources.sort_by_key(|source| {
            order
                .iter()
                .position(|id| *id == source.provider)
                .unwrap_or(order.len())
        });
        stream::iter(sources)
            .filter(|source| std::future::ready(!antigravity || source.provider != "antigravity"))
            .map(|source| async move {
                let (host, name) = source_host(&source.platform, source.ssh_host.as_deref());
                let raw = if aac_providers::provider(&source.provider).is_some_and(|row| row.quota)
                    && matches!(
                        source.credential,
                        sources::Credential::Discover
                            | sources::Credential::AacKey { .. }
                            | sources::Credential::BrowserCapsule { .. }
                    ) {
                    let mut args = json!({"provider":source.provider,"platform":source.platform});
                    let explicit = !matches!(source.credential, sources::Credential::Discover);
                    if explicit {
                        args["account"] = json!(source.id);
                        args["credential"] = json!(source.credential);
                    }
                    let expected = if explicit {
                        source.id.clone()
                    } else {
                        format!("plan-{}-{}", source.provider, source.platform)
                    };
                    self.ask(
                        &source.platform,
                        source.ssh_host.as_deref(),
                        "quota-read",
                        &args,
                    )
                    .await
                    .ok()
                    .filter(|raw| {
                        raw["id"] == expected
                            && raw["provider"] == source.provider
                            && raw["platform"] == source.platform
                    })
                } else {
                    None
                };
                rows::additional(&source, &host, &name, raw.as_ref())
            })
            .buffered(3)
            .collect()
            .await
    }
}

fn inventory_complete(inventory: &Option<Value>, selector: &str) -> bool {
    let Some(raw) = inventory else { return false };
    raw["version"] == 1
        && raw["action"] == "inventory"
        && raw["failed"] != true
        && raw["candidates"].is_array()
        && !raw["incompleteSelectors"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                item == selector
                    || item
                        .as_str()
                        .and_then(|s| s.strip_suffix('*'))
                        .is_some_and(|prefix| selector.starts_with(prefix))
            })
        })
}

fn pick<'a>(inventory: &'a Option<Value>, provider: &str, selector: &str) -> Option<&'a Value> {
    if !inventory_complete(inventory, selector) {
        return None;
    }
    let kinds = aac_providers::provider(provider)?.failover.kinds;
    let mut matches = inventory.as_ref()?["candidates"]
        .as_array()?
        .iter()
        .filter(|row| {
            row["selectors"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == selector))
                && row["identity"]["kind"]
                    .as_str()
                    .is_some_and(|kind| kinds.contains(&kind))
                && ["locator", "revision"]
                    .iter()
                    .map(|key| &row[*key])
                    .chain([&row["identity"]["key"]])
                    .all(|value| {
                        value.as_str().is_some_and(|text| {
                            text.len() == 64
                                && text
                                    .bytes()
                                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        })
                    })
        });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

struct Disk {
    visibility: Option<Value>,
    refresh: u64,
    sources: Vec<sources::Source>,
    claude: Vec<Value>,
    codex: Vec<String>,
}

impl Disk {
    fn load(root: &Path) -> Result<Self, Decline> {
        let visibility =
            crate::account_reads::visibility(root).map(|mut result| result["visibility"].take());
        let sources = match sources::read_registry(root)? {
            sources::Sources::Legacy => sources::legacy(root),
            sources::Sources::V2(rows) => rows,
        };
        let claude = crate::profile_reads::desktop_profiles(root).ok_or(Decline::Unsupported)?;
        Ok(Self {
            visibility,
            refresh: read_refresh(root),
            sources,
            claude: claude["profiles"].as_array().ok_or(Decline::Shape)?.clone(),
            codex: codex_names(root)?,
        })
    }
}

fn read_refresh(root: &Path) -> u64 {
    let valid = |bytes: Vec<u8>| -> Option<u64> {
        let value: Value = serde_json::from_slice(&bytes).ok()?;
        if value.as_object()?.len() != 1 {
            return None;
        }
        value["refreshIntervalSeconds"]
            .as_u64()
            .filter(|seconds| (30..=3600).contains(seconds))
    };
    aac_files::read_bounded(&root.join("account-refresh-settings.json"), 4096)
        .ok()
        .and_then(valid)
        .unwrap_or(60)
}

fn codex_names(root: &Path) -> Result<Vec<String>, Decline> {
    let file = root.join("codex-profiles.yaml");
    let policy = ReadPolicy::capped(256 * 1024).ancestors(Ancestors::NoLinkBelow(root));
    match aac_files::read::read(&file, &policy) {
        Read::Absent => Ok(vec![]),
        Read::Bytes(bytes, _) => {
            let value: Value = serde_yaml::from_slice(&bytes).map_err(|_| Decline::Shape)?;
            let profiles = value["profiles"].as_object().ok_or(Decline::Shape)?;
            if profiles.len() > 64
                || profiles.keys().any(|name| {
                    !(1..=64).contains(&name.len())
                        || !name.as_bytes()[0].is_ascii_alphanumeric()
                        || !name
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
                })
            {
                return Err(Decline::Shape);
            }
            Ok(profiles.keys().cloned().collect())
        }
        _ => Err(Decline::Unsupported),
    }
}

fn source_host(platform: &str, alias: Option<&str>) -> (String, String) {
    let table: Value =
        serde_json::from_str(crate::account_reads::COMPUTERS).expect("bundled computers");
    let known = table.as_object().and_then(|entries| {
        entries.iter().find(|(_, row)| {
            alias.is_some_and(|alias| row["sshAlias"] == alias)
                && (row["os"] == platform || (platform == "ubuntu" && row["os"] == "linux"))
        })
    });
    let host = known.map(|(id, _)| id.as_str()).unwrap_or(platform);
    let name = table[host]["label"].as_str().unwrap_or(platform);
    (host.into(), name.into())
}
