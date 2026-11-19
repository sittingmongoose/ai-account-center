//! Read the installed account sources and publish a complete Accounts generation.
mod claude;
mod codex_local;
mod history;
mod peers;
pub mod recheck;
mod selection;

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
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, Weak},
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
    cursor: Arc<super::cursor::Reader>,
    visibility: Arc<Mutex<Value>>,
    active_identities: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    /// Per-account forced refresh stamps for a single-row Re-check (the TS debounce).
    recheck_forced: Arc<std::sync::Mutex<HashMap<String, i64>>>,
}

impl Producer {
    /// Open the production readers for the given canonical service root and file owners.
    pub fn new(root: PathBuf, owners: Owners) -> Self {
        let agents = Arc::new(Agents::new(root.clone()));
        Self::shared(root, owners, agents, Arc::default(), Arc::default())
    }

    pub(crate) fn shared(
        root: PathBuf,
        owners: Owners,
        agents: Arc<Agents>,
        collection: Arc<Collection>,
        cursor: Arc<super::cursor::Reader>,
    ) -> Self {
        Self {
            root,
            agents,
            collection,
            owners,
            cursor,
            visibility: Arc::new(Mutex::new(dashboard::empty_visibility())),
            active_identities: Arc::default(),
            recheck_forced: Arc::default(),
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
        self.collect_with_refresh(companions, false).await
    }

    async fn collect_with_refresh(
        &self,
        companions: CompanionInputs,
        force: bool,
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

        let additional = self.additional(
            disk.sources,
            disk.legacy,
            disk.source_problem,
            companions.antigravity.is_some(),
            now,
            force,
        );
        let codex = self.codex_rows(disk.codex, now);
        let claude = self.claude_rows(&disk.claude_private, disk.refresh, now);
        let history = self.mac_history_rows(&disk.claude_private, now);
        let (additional, codex, claude, history) = tokio::join!(additional, codex, claude, history);

        let mut samples = Vec::with_capacity(4);
        for desktop in [Desktop::Mac, Desktop::Windows] {
            let mut rows = codex.clone();
            rows.extend(
                disk.claude
                    .iter()
                    .zip(claude.iter().zip(&history))
                    .filter_map(|(profile, (raw, history))| {
                        rows::claude(profile, desktop, raw.as_ref(), history.as_ref())
                    }),
            );
            for profile in &disk.claude_pending {
                let pending = rows::pending_claude(profile, desktop);
                if !rows.iter().any(|row| row["id"] == pending["id"]) {
                    rows.push(pending);
                }
            }
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
            |force| {
                let inputs = companions();
                async move {
                    self.collect_with_refresh(inputs, force)
                        .await
                        .unwrap_or_default()
                }
            },
            published,
        );
        tokio::select! {
            () = sampling => {},
            () = monitor => {},
        }
    }

    async fn codex_rows(&self, profiles: Vec<SavedCodex>, now: i64) -> Vec<Value> {
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
            .filter(|profile| {
                candidate(&profile.name)
                    .is_some_and(|row| Some(&row["identity"]) == active.as_ref())
            })
            .reduce(|best, next| {
                if next.last_used > best.last_used {
                    next
                } else {
                    best
                }
            })
            .map(|profile| profile.name.clone());

        let requests = profiles.into_iter().map(|profile| {
            let candidate = candidate(&profile.name).cloned();
            let active = active.as_deref() == Some(&profile.name);
            (profile.name, candidate, active)
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
                let sample = if name == "default"
                    && candidate.is_some()
                    && sample.as_ref().is_none_or(|reading| {
                        !matches!(
                            reading["status"].as_str(),
                            Some("needs_sign_in" | "ok" | "cached")
                        ) || (reading["status"] != "needs_sign_in"
                            && rows::windows(&reading["windows"], 256).is_empty())
                    }) {
                    let root = self.root.clone();
                    tokio::task::spawn_blocking(move || codex_local::read(&root, now))
                        .await
                        .ok()
                        .flatten()
                        .or(sample)
                } else {
                    sample
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
        let reply = self
            .attempt(platform, alias, provider, candidate, None, now)
            .await?;
        if confirmed(&reply, candidate) {
            reply
                .get("sample")
                .filter(|sample| sample.is_object())
                .cloned()
        } else {
            None
        }
    }

    async fn lock_identities(
        &self,
        key: &str,
        peer: Option<&str>,
    ) -> Vec<tokio::sync::OwnedMutexGuard<()>> {
        let mut keys = std::iter::once(key).chain(peer).collect::<Vec<_>>();
        keys.sort_unstable();
        keys.dedup();
        let locks = {
            let mut active = self.active_identities.lock().await;
            active.retain(|_, lock| lock.strong_count() > 0);
            keys.into_iter()
                .map(|key| {
                    let slot = active.entry(key.to_owned()).or_default();
                    slot.upgrade().unwrap_or_else(|| {
                        let lock = Arc::new(Mutex::new(()));
                        *slot = Arc::downgrade(&lock);
                        lock
                    })
                })
                .collect::<Vec<_>>()
        };
        // Consistent order also covers two linked accounts used as one another's fallback.
        let mut held = Vec::with_capacity(locks.len());
        for lock in locks {
            held.push(lock.lock_owned().await);
        }
        held
    }

    async fn attempt(
        &self,
        platform: &str,
        alias: Option<&str>,
        provider: &str,
        candidate: &Value,
        anchor: Option<&str>,
        now: i64,
    ) -> Option<Value> {
        let key = identity_key(&self.root, provider, &candidate["identity"]);
        let _active = self.lock_identities(&key, anchor).await;
        let now = now_at_least(now);
        for key in std::iter::once(key.as_str()).chain(anchor) {
            let held = super::failover::persisted(&self.root, key, now as f64)
                .ok()?
                .unwrap_or_default();
            if held.until > now as f64 {
                return None;
            }
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
        if limited(&reply) {
            let now = now_at_least(now);
            let seconds = reply["retryAfterSeconds"]
                .as_f64()
                .unwrap_or(60.0)
                .clamp(60.0, 86_400.0);
            let hold = super::failover::Cooldown {
                until: now as f64 + seconds * 1000.0,
                claude_429_streak: None,
            };
            save_hold(&self.owners, &self.root, &key, now as f64, hold);
            if let Some(anchor) = anchor {
                save_hold(&self.owners, &self.root, anchor, now as f64, hold);
            }
        }
        Some(reply)
    }
}

fn now_at_least(started: i64) -> i64 {
    UtcMs::now()
        .ok()
        .map_or(started, |now| started.max(now.get() as i64))
}

fn identity_key(root: &Path, provider: &str, identity: &Value) -> String {
    aac_core::sha256_hex(aac_views::js_json::write(&json!([
        root, provider, {"kind":identity["kind"],"key":identity["key"]}
    ])))
}

fn limited(reply: &Value) -> bool {
    reply["failureCode"] == "rate_limited"
        || (reply["failureCode"] == "source_changed" && reply["rateLimited"] == true)
}

fn confirmed(reply: &Value, candidate: &Value) -> bool {
    let final_candidate = &reply["candidate"];
    reply["version"] == 1
        && reply["action"] == "collect"
        && ["identity", "revision", "locator"]
            .iter()
            .all(|field| final_candidate[*field] == candidate[*field])
}

fn save_hold(owners: &Owners, root: &Path, key: &str, now: f64, hold: super::failover::Cooldown) {
    let mut record = json!({"now":now,"until":hold.until});
    if let Some(streak) = hold.claude_429_streak {
        record["claude429Streak"] = json!(streak);
    }
    let _ = super::failover::save(owners, root, key, &aac_views::js_json::write(&record));
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
    let mut matches = inventory.as_ref()?["candidates"]
        .as_array()?
        .iter()
        .filter(|row| {
            row["selectors"]
                .as_array()
                .is_some_and(|items| items.iter().any(|item| item == selector))
                && valid_candidate(row, provider)
        });
    let first = matches.next()?;
    matches.next().is_none().then_some(first)
}

fn valid_candidate(row: &Value, provider: &str) -> bool {
    let Some(kinds) = aac_providers::provider(provider).map(|config| config.failover.kinds) else {
        return false;
    };
    row["identity"]
        .as_object()
        .is_some_and(|identity| identity.len() == 2)
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
}

struct Disk {
    visibility: Option<Value>,
    refresh: u64,
    sources: Vec<sources::Source>,
    legacy: bool,
    source_problem: Option<(&'static str, &'static str)>,
    claude: Vec<Value>,
    claude_private: Vec<Value>,
    claude_pending: Vec<Value>,
    codex: Vec<SavedCodex>,
}

impl Disk {
    fn load(root: &Path) -> Result<Self, Decline> {
        let visibility =
            crate::account_reads::visibility(root).map(|mut result| result["visibility"].take());
        let (sources, legacy, source_problem) = match sources::read_registry(root) {
            Ok(sources::Sources::Legacy) => {
                let (sources, valid) = sources::legacy(Some(root));
                (
                    sources,
                    true,
                    (!valid).then_some(("error", "Account usage source configuration is invalid.")),
                )
            }
            Ok(sources::Sources::V2(rows)) => (rows, false, None),
            Err(_) => (
                sources::legacy(None).0,
                false,
                Some(("unavailable", "Account list could not be read safely.")),
            ),
        };
        let manifest = crate::profile_reads::desktop_manifest(root).ok_or(Decline::Unsupported)?;
        let claude = crate::profile_reads::compose_manifest(manifest.as_deref())
            .ok_or(Decline::Unsupported)?;
        let claude_private = manifest
            .as_deref()
            .and_then(|bytes| serde_json::from_slice::<Value>(bytes).ok())
            .and_then(|mut value| value["profiles"].take().as_array().cloned())
            .unwrap_or_default();
        Ok(Self {
            visibility,
            refresh: read_refresh(root),
            sources,
            legacy,
            source_problem,
            claude: claude["profiles"].as_array().ok_or(Decline::Shape)?.clone(),
            claude_private,
            claude_pending: claude::pending(root).unwrap_or_default(),
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

struct SavedCodex {
    name: String,
    last_used: String,
}

fn codex_names(root: &Path) -> Result<Vec<SavedCodex>, Decline> {
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
            Ok(profiles
                .iter()
                .map(|(name, metadata)| SavedCodex {
                    name: name.clone(),
                    last_used: metadata["last_used"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned(),
                })
                .collect())
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
