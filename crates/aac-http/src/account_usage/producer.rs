//! Read the installed account sources and publish a complete Accounts generation.
mod claude;
mod codex_local;
mod failover;
mod history;
pub mod recheck;
mod selection;
mod signin_state;

use super::coordinator::FailoverCoordinator;
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
    /// The service process home the renewal snapshot reads (`HOME` at boot).
    home: Option<PathBuf>,
    agents: Arc<Agents>,
    collection: Arc<Collection>,
    owners: Owners,
    cursor: Arc<super::cursor::Reader>,
    visibility: Arc<Mutex<Value>>,
    active_identities: Arc<Mutex<HashMap<String, Weak<Mutex<()>>>>>,
    /// Per-account forced refresh stamps for a single-row Re-check (the TS debounce).
    recheck_forced: Arc<std::sync::Mutex<HashMap<String, i64>>>,
    /// The renewal status the Codex rows read, cached for the TS status TTL (15 s).
    renewal_status: Arc<std::sync::Mutex<Option<RenewalSnapshot>>>,
    /// The one display-failover coordinator (G4) every provider's row goes through.
    failover: Arc<FailoverCoordinator>,
    /// The cached `claude-host` session answers (the TS five-minute probe).
    sign_in_state: signin_state::SignInCache,
    #[cfg(test)]
    mac_session_fixture: Option<signin_state::SessionFixture>,
}

type RenewalSnapshot = (
    i64,
    HashMap<String, aac_renewal::renewal::CodexProfileRenewalProfileStatus>,
);

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
        let failover = failover::coordinator(agents.clone(), collection.clone(), owners.clone());
        Self {
            root,
            home: std::env::var_os("HOME").map(PathBuf::from),
            agents,
            collection,
            owners,
            cursor,
            visibility: Arc::new(Mutex::new(dashboard::empty_visibility())),
            active_identities: Arc::default(),
            recheck_forced: Arc::default(),
            renewal_status: Arc::default(),
            failover,
            sign_in_state: Arc::default(),
            #[cfg(test)]
            mac_session_fixture: None,
        }
    }

    /// Point the renewal snapshot's homes at a fixture root (tests only).
    #[cfg(test)]
    pub(crate) fn with_home(mut self, home: Option<PathBuf>) -> Self {
        self.home = home;
        self
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

    /// Read the saved Codex profiles through the collector's existing quota and renewal readers.
    pub async fn codex_profile_rows(&self, now: i64) -> Vec<Value> {
        let root = self.root.clone();
        let Ok(Ok(profiles)) = tokio::task::spawn_blocking(move || codex_names(&root)).await else {
            return Vec::new();
        };
        let entries = self.renewal_entries(now).await;
        self.codex_rows(profiles, &entries, now).await
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
        let mut visibility = {
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
        let history = self.mac_history_rows(&disk.claude_private, now).await;
        let claude = self.claude_rows(&disk.claude, &disk.claude_private, &history, force);
        let entries = self.renewal_entries(now).await;
        let codex = self.codex_rows(disk.codex, &entries, now);
        let (additional, codex, claude) = tokio::join!(additional, codex, claude);

        if visibility_available {
            let migrations = dashboard::unique_discovered_opencode_ids(&additional);
            if dashboard::has_migratable_ids(&visibility, &migrations) {
                let root = self.root.clone();
                let owners = self.owners.clone();
                if let Ok(Ok(Some(saved))) = tokio::task::spawn_blocking(move || {
                    crate::visibility_write::migrate_account_ids(&root, &owners, &migrations)
                })
                .await
                {
                    let mut current = self.visibility.lock().await;
                    *current = saved.clone();
                    visibility = saved;
                }
            }
        }

        let mut samples = Vec::with_capacity(4);
        for (selected, desktop) in [Desktop::Mac, Desktop::Windows].into_iter().enumerate() {
            let mut rows = codex.clone();
            rows.extend(claude.iter().filter_map(|pair| pair[selected].clone()));
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
    pub async fn run<C, Inputs, P, Publish>(
        &self,
        cache: AccountUsageCache,
        cancel: CancellationToken,
        mut companions: C,
        published: P,
    ) where
        C: FnMut(bool) -> Inputs,
        Inputs: std::future::Future<Output = CompanionInputs>,
        P: FnMut(AccountUsageCache) -> Publish,
        Publish: std::future::Future<Output = ()>,
    {
        let root = self.root.clone();
        let initial = read_refresh(&root) * 1000;
        let (settings, interval) = watch::channel(initial);
        let monitor = async move {
            let mut last_mtime = std::fs::metadata(root.join("account-refresh-settings.json"))
                .and_then(|m| m.modified())
                .ok();
            loop {
                tokio::time::sleep(Duration::from_secs(10)).await;
                let mtime = std::fs::metadata(root.join("account-refresh-settings.json"))
                    .and_then(|m| m.modified())
                    .ok();
                if mtime == last_mtime {
                    continue;
                }
                last_mtime = mtime;
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
                let inputs = companions(force);
                async move {
                    self.collect_with_refresh(inputs.await, force)
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

    /// The renewal status of every saved Codex login from local files only,
    /// exactly as the TS dashboard reads it (a failed read yields no entries).
    /// The snapshot scans the shared native login, the saved instances and the
    /// custom `.codex*` and running-process homes, so notes derived from a
    /// custom active home match the TS planner.
    async fn renewal_entries(
        &self,
        now: i64,
    ) -> HashMap<String, aac_renewal::renewal::CodexProfileRenewalProfileStatus> {
        const STATUS_TTL_MS: i64 = 15_000;
        {
            let held = self
                .renewal_status
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some((at, entries)) = held.as_ref()
                && now.saturating_sub(*at) < STATUS_TTL_MS
            {
                return entries.clone();
            }
        }
        let root = self.root.clone();
        let home = self.home.clone();
        let entries: HashMap<String, aac_renewal::renewal::CodexProfileRenewalProfileStatus> =
            tokio::task::spawn_blocking(move || {
                let mut env = aac_renewal::planner::CodexRenewalEnvironment {
                    ccs_dir: root,
                    ..Default::default()
                };
                if let Some(home) = &home {
                    env.codex_home = Some(home.join(".codex"));
                    env.home_dir = Some(home.clone());
                }
                let options = aac_renewal::renewal::CodexProfileRenewalOptions {
                    env,
                    ..Default::default()
                };
                aac_renewal::renewal::get_codex_profile_renewal_status(&options)
                    .map(|status| {
                        status
                            .profiles
                            .into_iter()
                            .map(|entry| (entry.name.clone(), entry))
                            .collect()
                    })
                    .unwrap_or_default()
            })
            .await
            .unwrap_or_default();
        let mut held = self
            .renewal_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        held.replace((now, entries.clone()));
        entries
    }

    async fn codex_rows(
        &self,
        profiles: Vec<SavedCodex>,
        entries: &HashMap<String, aac_renewal::renewal::CodexProfileRenewalProfileStatus>,
        now: i64,
    ) -> Vec<Value> {
        use futures_util::{StreamExt, stream};
        if profiles.is_empty() {
            return vec![];
        }
        let platform = if cfg!(target_os = "macos") {
            "mac"
        } else if cfg!(windows) {
            "windows"
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
                let (sample, failure) = match &candidate {
                    Some(candidate) => {
                        self.read_primary_reply(platform, None, "codex", candidate, now)
                            .await
                    }
                    None => (None, None),
                };
                // A rejected login is the TS reauth row, never the bare profile's
                // local session fallback (that fallback is for an unreadable login).
                let sample = if name == "default"
                    && failure.as_deref() != Some("needs_sign_in")
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
                let needs_reauth =
                    row_needs_reauth(signed_out, failure.as_deref(), sample.as_ref());
                let note = renewal_row_note(entries, &name, active, needs_reauth, now);
                rows::codex(
                    &name,
                    active,
                    needs_reauth,
                    candidate.as_ref(),
                    sample.as_ref(),
                    note.as_ref(),
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
        self.read_primary_reply(platform, alias, provider, candidate, now)
            .await
            .0
    }

    /// The collect reply's sample and failure code for one candidate. A failed
    /// read returns no sample; the code lets the Codex rows keep the TS
    /// `needsReauth` status (`needs_sign_in`) that the helper reports without
    /// inventing a reading.
    async fn read_primary_reply(
        &self,
        platform: &str,
        alias: Option<&str>,
        provider: &str,
        candidate: &Value,
        now: i64,
    ) -> (Option<Value>, Option<String>) {
        let Some(reply) = self
            .attempt(platform, alias, provider, candidate, None, now)
            .await
        else {
            return (None, None);
        };
        let failure = reply["failureCode"].as_str().map(str::to_owned);
        let sample = confirmed(&reply, candidate)
            .then(|| {
                reply
                    .get("sample")
                    .filter(|sample| sample.is_object())
                    .cloned()
            })
            .flatten();
        (sample, failure)
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

/// The TS row's own `needsReauth` flag: a parked profile (no readable login), a
/// login OpenAI rejected (the helper reports a failure code with no sample), or
/// a sample that already says so.
fn row_needs_reauth(signed_out: bool, failure: Option<&str>, sample: Option<&Value>) -> bool {
    signed_out
        || failure == Some("needs_sign_in")
        || sample.is_some_and(|reading| reading["status"] == "needs_sign_in")
}

/// The TS note decision for one Codex row: the entry exists, this profile is not
/// the login Codex is using (Codex renews that itself), its source is not live,
/// and its renewal state says something the row should show.
fn renewal_row_note(
    entries: &HashMap<String, aac_renewal::renewal::CodexProfileRenewalProfileStatus>,
    name: &str,
    active: bool,
    needs_reauth: bool,
    now: i64,
) -> Option<aac_renewal::notes::CodexRenewalNote> {
    entries
        .get(name)
        .filter(|_| !active)
        .filter(|entry| entry.reason != aac_renewal::CodexRenewalReason::Live)
        .and_then(|entry| aac_renewal::notes::codex_renewal_note(needs_reauth, entry, now))
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

pub(crate) fn has_codex_profile(root: &Path, name: &str) -> bool {
    codex_names(root).is_ok_and(|profiles| profiles.iter().any(|profile| profile.name == name))
}

pub(crate) fn source_host(platform: &str, alias: Option<&str>) -> (String, String) {
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

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use aac_renewal::renewal::CodexProfileRenewalProfileStatus;
    use aac_renewal::{CodexRenewalReason, CodexRenewalState};
    use std::fs;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn set_mode(path: &Path, mode: u32) {
        #[cfg(unix)]
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("fixture mode");
        #[cfg(not(unix))]
        let _ = (path, mode);
    }

    fn private_bytes(file: &Path, bytes: &[u8]) {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent).expect("fixture folder");
            set_mode(parent, 0o700);
        }
        fs::write(file, bytes).expect("fixture file");
        set_mode(file, 0o600);
    }

    fn state(word: &str) -> CodexRenewalState {
        match word {
            "ok" => CodexRenewalState::Ok,
            "due" => CodexRenewalState::Due,
            "renewing" => CodexRenewalState::Renewing,
            "retrying" => CodexRenewalState::Retrying,
            "failed" => CodexRenewalState::Failed,
            _ => CodexRenewalState::Skipped,
        }
    }

    #[test]
    fn a_parked_or_rejected_login_keeps_the_ts_reauth_status() {
        assert!(row_needs_reauth(true, None, None));
        assert!(row_needs_reauth(false, Some("needs_sign_in"), None));
        assert!(row_needs_reauth(
            false,
            None,
            Some(&json!({"status":"needs_sign_in"}))
        ));
        assert!(!row_needs_reauth(false, Some("unavailable"), None));
        assert!(!row_needs_reauth(
            false,
            None,
            Some(&json!({"status":"cached"}))
        ));
    }

    /// The TS `codexAccount` goldens: the renewal note overlay, the active-login
    /// suppression and every status the native collector can produce. The TS
    /// side is `tests/spec/codex-renewal-row-parity.ts`.
    #[test]
    fn codex_rows_match_the_ts_renewal_overlay() {
        let fixture: Value =
            serde_json::from_str(include_str!("../../tests/fixtures/codex-renewal-rows.json"))
                .expect("renewal row fixture parses");
        assert_eq!(fixture["version"], 1);
        let now = fixture["now"].as_i64().expect("fixture clock");
        let cases = fixture["cases"].as_array().expect("fixture cases");
        assert!(!cases.is_empty());
        for case in cases {
            let input = &case["input"];
            let name = input["name"].as_str().expect("profile name");
            let active = input["active"].as_bool().expect("active flag");
            let needs_reauth = !input["authValid"].as_bool().expect("auth flag")
                || input["needsReauth"].as_bool().expect("reauth flag");
            let mut entries = HashMap::new();
            let note = match input["renewal"].as_object() {
                None => None,
                Some(entry) => {
                    let reason =
                        CodexRenewalReason::from_code(entry["reason"].as_str().expect("reason"))
                            .expect("known renewal reason");
                    entries.insert(
                        name.to_owned(),
                        CodexProfileRenewalProfileStatus {
                            name: name.to_owned(),
                            state: state(entry["state"].as_str().expect("state")),
                            reason,
                            message: reason.message(),
                            last_renewed_at: None,
                            last_attempt_at: None,
                            last_outcome: None,
                            next_attempt_at: None,
                            access_expires_at: entry
                                .get("accessExpiresAt")
                                .and_then(Value::as_str)
                                .map(str::to_owned),
                        },
                    );
                    renewal_row_note(&entries, name, active, needs_reauth, now)
                }
            };
            let sample = json!({
                "status": if needs_reauth {
                    "needs_sign_in"
                } else if input["cached"] == true {
                    "cached"
                } else {
                    "ok"
                },
                "usageSource": input["quotaSource"],
                "windows": input["windows"],
                "fetchedAt": input["fetchedAt"],
                "sampledAt": input["sampledAt"],
            });
            let candidate = json!({"email": input["email"], "plan": input["plan"]});
            let row = rows::codex(
                name,
                active,
                needs_reauth,
                Some(&candidate),
                Some(&sample),
                note.as_ref(),
            );
            assert_eq!(
                aac_views::js_json::write(&row),
                aac_views::js_json::write(&case["row"]),
                "{}",
                case["name"]
            );
        }
    }

    /// A saved login copied into a custom `.codex*` home is shared, exactly as the
    /// TS planner reports it, and the collected row says why renewal leaves it
    /// alone. The live login and an activated profile never get a note.
    #[tokio::test]
    async fn renewal_entries_read_custom_codex_homes_like_the_ts_planner() {
        use aac_renewal::parity::{Account, LoginSpec, login_buffer};

        let dir = tempfile::tempdir().expect("fixture root");
        let ccs = dir.path().join(".ccs");
        let home = dir.path().join("home");
        let now = aac_core::UtcMs::now().expect("clock").get() as i64;
        let day = 86_400_000;
        let login = |account: &str, refresh: &str| LoginSpec {
            account: Account::Word(account.into()),
            refresh: refresh.into(),
            label: account.into(),
            session: Some(Some(format!("sess-{account}"))),
            auth_time: None,
            access_exp: Some(Some(now + 2 * day)),
            last_refresh: None,
            access_account_id: None,
        };
        let alpha = login("alpha", "refresh-alpha");
        let bravo = login("bravo", "refresh-bravo");
        private_bytes(
            &ccs.join("codex-profiles.yaml"),
            b"version: '1.0'\ndefault: null\nprofiles:\n  gmail:\n    type: codex\n    created: '2026-10-01T00:00:00Z'\n    last_used: null\n  party:\n    type: codex\n    created: '2026-10-01T00:00:00Z'\n    last_used: null\n",
        );
        private_bytes(
            &ccs.join("codex-instances/gmail/auth.json"),
            &login_buffer(&alpha),
        );
        private_bytes(
            &ccs.join("codex-instances/party/auth.json"),
            &login_buffer(&bravo),
        );
        // Codex is using bravo; gmail's saved login also lives in a custom home.
        private_bytes(&home.join(".codex/auth.json"), &login_buffer(&bravo));
        private_bytes(
            &home.join(".codex-t3/gmail/auth.json"),
            &login_buffer(&alpha),
        );
        let owners = aac_files::registry::Owners::defaults(
            aac_files::registry::rows().expect("bundled private files"),
        );
        let producer = Producer::new(ccs, owners).with_home(Some(home));
        let entries = producer.renewal_entries(now).await;

        assert_eq!(
            entries["gmail"].reason,
            CodexRenewalReason::SharedFamily,
            "{entries:?}"
        );
        assert_eq!(entries["gmail"].state, CodexRenewalState::Skipped);
        assert_eq!(entries["party"].reason, CodexRenewalReason::Live);
        let note = renewal_row_note(&entries, "gmail", false, false, now)
            .expect("a shared saved login names itself on its row");
        assert_eq!(note.message, CodexRenewalReason::SharedFamily.message());
        assert!(
            renewal_row_note(&entries, "gmail", true, false, now).is_none(),
            "the login Codex uses never gets a note"
        );
        assert!(
            renewal_row_note(&entries, "party", false, false, now).is_none(),
            "a live source never gets a note"
        );
    }

    struct MutableOpencodeKeys {
        keys: std::sync::Mutex<Vec<(&'static str, &'static str, i64)>>,
        now: std::sync::atomic::AtomicI64,
    }

    impl crate::account_usage::coordinator::FailoverDeps for MutableOpencodeKeys {
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
                let keys = self.keys.lock().unwrap_or_else(|e| e.into_inner()).clone();
                if request["action"] == "inventory" {
                    let candidates: Vec<Value> = keys
                        .iter()
                        .map(|(locator, key, _)| {
                            json!({
                                "locator": locator.repeat(64),
                                "selectors": ["discover"],
                                "revision": "2".repeat(64),
                                "identity": {"kind": "opencode-go-credential", "key": key.repeat(64)}
                            })
                        })
                        .collect();
                    return Ok(json!({
                        "version": 1,
                        "action": "inventory",
                        "candidates": candidates
                    }));
                }
                let prefix = request["candidate"]["identity"]["key"]
                    .as_str()
                    .and_then(|s| s.get(..1))
                    .unwrap_or("");
                let used = keys
                    .iter()
                    .find(|(_, key, _)| *key == prefix)
                    .map(|(_, _, used)| *used)
                    .unwrap_or(99);
                let at = aac_num::ecma::to_iso_string(
                    self.now.load(std::sync::atomic::Ordering::Relaxed),
                )
                .unwrap_or_else(|| "2026-11-21T17:00:00.000Z".into());
                Ok(json!({
                    "version": 1,
                    "action": "collect",
                    "candidate": request["candidate"],
                    "failureCode": null,
                    "sample": {
                        "status": "ok",
                        "sampledAt": at,
                        "fetchedAt": at,
                        "windows": [{"key": "weekly", "label": "Weekly", "usedPercent": used}]
                    }
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

    fn visibility_and_collection_owners() -> Owners {
        let rows = aac_files::registry::rows().expect("bundled private files");
        let files: std::collections::BTreeMap<String, String> = rows
            .iter()
            .map(|row| {
                let owner = if matches!(row.moves_with.as_deref(), Some("collection" | "writes")) {
                    "rust-service"
                } else {
                    row.owner.word()
                };
                (row.id.clone(), owner.to_owned())
            })
            .collect();
        Owners::from_boot(rows, &files).expect("owners")
    }

    /// Reviewer Low fix round 1: migrating a legacy bare `opencode-go:usage` hide onto the sole
    /// digest row persists that digest into `account-visibility.json` (Home and tray
    /// independently), so adding key B keeps A hidden and removing A never hides B. A choice made
    /// directly on a digest is never modified.
    #[tokio::test]
    async fn opencode_go_bare_hide_persists_onto_key_a_and_never_moves_to_key_b() {
        let key_a = ("1", "a", 11);
        let key_b = ("3", "b", 22);
        let id_a = "opencode-go:usage:aaaaaaaa";
        let id_b = "opencode-go:usage:bbbbbbbb";

        let find_row = |sample: &sampler::DashboardSample, id: &str| -> Value {
            sample.dashboard["accounts"]
                .as_array()
                .and_then(|rows| rows.iter().find(|row| row["id"] == id))
                .cloned()
                .unwrap_or_else(|| panic!("row {id} missing from {:?}", sample.desktop))
        };

        // Home-only and tray-only: run the three-step sequence (only A -> A+B -> only B).
        for tray_only in [false, true] {
            let label = if tray_only { "tray-only" } else { "home-only" };
            let dir = tempfile::tempdir().expect("fixture root");
            let root = fs::canonicalize(dir.path()).expect("canonical root");
            set_mode(&root, 0o700);
            private_bytes(
                &root.join("account-usage-accounts.json"),
                &serde_json::to_vec(&json!({
                    "version": 2,
                    "accounts": [{
                        "id": "opencode-go:usage",
                        "provider": "opencode-go",
                        "platform": "ubuntu",
                        "sshHost": null,
                        "label": "OpenCode Go",
                        "credential": {"kind": "discover"},
                        "createdAt": null,
                        "createdBy": "migration"
                    }]
                }))
                .expect("registry"),
            );
            let initial_home = if tray_only {
                json!([])
            } else {
                json!(["codex:party", "opencode-go:usage"])
            };
            let initial_tray = if tray_only {
                json!(["codex:party", "opencode-go:usage"])
            } else {
                json!([])
            };
            private_bytes(
                &root.join("account-visibility.json"),
                &serde_json::to_vec(&json!({
                    "version": 1,
                    "hiddenProviders": [],
                    "hiddenAccountIds": initial_home,
                    "trayHiddenProviders": [],
                    "trayHiddenAccountIds": initial_tray
                }))
                .expect("visibility"),
            );

            let owners = visibility_and_collection_owners();
            let deps = Arc::new(MutableOpencodeKeys {
                keys: std::sync::Mutex::new(vec![key_a]),
                now: std::sync::atomic::AtomicI64::new(1_795_280_400_000),
            });
            let mut producer = Producer::new(root.clone(), owners.clone()).with_home(None);
            producer.failover = Arc::new(
                crate::account_usage::coordinator::FailoverCoordinator::new(deps.clone(), owners),
            );

            let expected_home = if tray_only {
                json!([])
            } else {
                json!(["codex:party", id_a])
            };
            let expected_tray = if tray_only {
                json!(["codex:party", id_a])
            } else {
                json!([])
            };

            // Step 1: only key A exists -> A is hidden in the configured list and persisted.
            let step1 = producer
                .collect(CompanionInputs::default())
                .await
                .expect("step 1 collect");
            assert_eq!(step1.len(), 4);
            for sample in &step1 {
                let row_a = find_row(sample, id_a);
                assert_eq!(row_a["hidden"], !tray_only, "{label} step 1 Mac/Win hidden");
                assert_eq!(row_a["trayHidden"], tray_only, "{label} step 1 trayHidden");
                assert_eq!(
                    sample.dashboard["settings"]["hiddenAccountIds"], expected_home,
                    "{label} step 1 settings home"
                );
                assert_eq!(
                    sample.dashboard["settings"]["trayHiddenAccountIds"], expected_tray,
                    "{label} step 1 settings tray"
                );
            }
            let saved_after_step1 =
                crate::account_reads::visibility(&root).expect("saved visibility step 1");
            assert_eq!(
                saved_after_step1["visibility"]["hiddenAccountIds"], expected_home,
                "{label} step 1 persisted home"
            );
            assert_eq!(
                saved_after_step1["visibility"]["trayHiddenAccountIds"], expected_tray,
                "{label} step 1 persisted tray"
            );

            // Step 2: add key B alongside A -> A stays hidden, B is visible.
            *deps.keys.lock().unwrap_or_else(|e| e.into_inner()) = vec![key_a, key_b];
            deps.now
                .fetch_add(120_000, std::sync::atomic::Ordering::Relaxed);
            let step2 = producer
                .collect(CompanionInputs::default())
                .await
                .expect("step 2 collect");
            for sample in &step2 {
                let row_a = find_row(sample, id_a);
                let row_b = find_row(sample, id_b);
                assert_eq!(row_a["hidden"], !tray_only, "{label} step 2 A hidden");
                assert_eq!(
                    row_a["trayHidden"], tray_only,
                    "{label} step 2 A trayHidden"
                );
                assert_eq!(row_b["hidden"], false, "{label} step 2 B hidden");
                assert_eq!(row_b["trayHidden"], false, "{label} step 2 B trayHidden");
            }

            // Step 3: remove A, leaving only B -> B stays visible, saved choice remains on A.
            *deps.keys.lock().unwrap_or_else(|e| e.into_inner()) = vec![key_b];
            deps.now
                .fetch_add(120_000, std::sync::atomic::Ordering::Relaxed);
            let step3 = producer
                .collect(CompanionInputs::default())
                .await
                .expect("step 3 collect");
            for sample in &step3 {
                let row_b = find_row(sample, id_b);
                assert_eq!(row_b["hidden"], false, "{label} step 3 B hidden");
                assert_eq!(row_b["trayHidden"], false, "{label} step 3 B trayHidden");
                assert_eq!(
                    sample.dashboard["settings"]["hiddenAccountIds"], expected_home,
                    "{label} step 3 settings home"
                );
                assert_eq!(
                    sample.dashboard["settings"]["trayHiddenAccountIds"], expected_tray,
                    "{label} step 3 settings tray"
                );
            }
            let saved_after_step3 =
                crate::account_reads::visibility(&root).expect("saved visibility step 3");
            assert_eq!(
                saved_after_step3["visibility"]["hiddenAccountIds"], expected_home,
                "{label} step 3 persisted home"
            );
            assert_eq!(
                saved_after_step3["visibility"]["trayHiddenAccountIds"], expected_tray,
                "{label} step 3 persisted tray"
            );
        }

        // Direct digest choice: never changed, even when the chosen key disappears and another
        // key is the sole row, and even when the other list (Home vs tray) migrates a bare ID.
        let dir = tempfile::tempdir().expect("direct digest root");
        let root = fs::canonicalize(dir.path()).expect("canonical root");
        set_mode(&root, 0o700);
        private_bytes(
            &root.join("account-usage-accounts.json"),
            &serde_json::to_vec(&json!({
                "version": 2,
                "accounts": [{
                    "id": "opencode-go:usage",
                    "provider": "opencode-go",
                    "platform": "ubuntu",
                    "sshHost": null,
                    "label": "OpenCode Go",
                    "credential": {"kind": "discover"},
                    "createdAt": null,
                    "createdBy": "migration"
                }]
            }))
            .expect("registry"),
        );
        let direct_bytes = serde_json::to_vec(&json!({
            "version": 1,
            "hiddenProviders": [],
            "hiddenAccountIds": [id_a],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": [id_b]
        }))
        .expect("direct visibility");
        private_bytes(&root.join("account-visibility.json"), &direct_bytes);

        let owners = visibility_and_collection_owners();
        let deps = Arc::new(MutableOpencodeKeys {
            keys: std::sync::Mutex::new(vec![key_a]),
            now: std::sync::atomic::AtomicI64::new(1_795_280_400_000),
        });
        let mut producer = Producer::new(root.clone(), owners.clone()).with_home(None);
        producer.failover = Arc::new(crate::account_usage::coordinator::FailoverCoordinator::new(
            deps.clone(),
            owners,
        ));

        let s1 = producer
            .collect(CompanionInputs::default())
            .await
            .expect("direct s1");
        assert_eq!(find_row(&s1[0], id_a)["hidden"], true);
        assert_eq!(find_row(&s1[0], id_a)["trayHidden"], false);

        *deps.keys.lock().unwrap_or_else(|e| e.into_inner()) = vec![key_b];
        deps.now
            .fetch_add(120_000, std::sync::atomic::Ordering::Relaxed);
        let s2 = producer
            .collect(CompanionInputs::default())
            .await
            .expect("direct s2");
        assert_eq!(find_row(&s2[0], id_b)["hidden"], false);
        assert_eq!(find_row(&s2[0], id_b)["trayHidden"], true);
        // Exact file bytes untouched because no bare ID was present to migrate.
        assert_eq!(
            fs::read(root.join("account-visibility.json")).expect("read direct file"),
            direct_bytes
        );

        // Migrating a bare ID on Home must not alter a direct digest choice on tray.
        private_bytes(
            &root.join("account-visibility.json"),
            &serde_json::to_vec(&json!({
                "version": 1,
                "hiddenProviders": [],
                "hiddenAccountIds": ["opencode-go:usage"],
                "trayHiddenProviders": [],
                "trayHiddenAccountIds": [id_b]
            }))
            .expect("mixed visibility"),
        );
        *deps.keys.lock().unwrap_or_else(|e| e.into_inner()) = vec![key_a];
        deps.now
            .fetch_add(120_000, std::sync::atomic::Ordering::Relaxed);
        let m1 = producer
            .collect(CompanionInputs::default())
            .await
            .expect("mixed m1");
        assert_eq!(find_row(&m1[0], id_a)["hidden"], true);
        assert_eq!(find_row(&m1[0], id_a)["trayHidden"], false);
        let saved_mixed = crate::account_reads::visibility(&root).expect("saved mixed");
        assert_eq!(saved_mixed["visibility"]["hiddenAccountIds"], json!([id_a]));
        assert_eq!(
            saved_mixed["visibility"]["trayHiddenAccountIds"],
            json!([id_b])
        );
        *deps.keys.lock().unwrap_or_else(|e| e.into_inner()) = vec![key_b];
        deps.now
            .fetch_add(120_000, std::sync::atomic::Ordering::Relaxed);
        let m2 = producer
            .collect(CompanionInputs::default())
            .await
            .expect("mixed m2");
        assert_eq!(find_row(&m2[0], id_b)["hidden"], false);
        assert_eq!(find_row(&m2[0], id_b)["trayHidden"], true);
    }
}
