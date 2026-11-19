//! The failover coordinator's peer and account arbitration (cut gap G4), ported from
//! `usage-failover-service.ts`. It chooses the account and peers, applies the exact wave 67
//! Claude Desktop cadence (target offset, stepped delay, Retry-After floor, verified-success
//! reset), keeps the longer identity hold, and caches the verified reading. Helper scheduling
//! and the cooldown primitives are lane I's (`collection`, `account_usage::failover`); this is
//! the arbitration that removes the TypeScript coordinator. The TS path stays until the cut.

use super::contract::{
    self, UsageCandidate, UsageIdentity, UsageReading, hex_digest, is_failure_code, parse_candidate,
    parse_identity, parse_reading, same_identity, safe_usage_text, usage_timestamp,
};
use super::failover::{self, Cooldown};
use aac_files::registry::Owners;
use aac_views::js_json;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex};

const DAY_MS: f64 = 86_400_000.0;
const MAX_CLAUDE_429_STREAK: u8 = 5;

/// A computer the coordinator may read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageHost {
    /// The dashboard host id.
    pub host: String,
    /// The OS platform.
    pub platform: String,
    /// The private SSH alias, or None for the dashboard computer.
    pub ssh_host: Option<String>,
}

impl UsageHost {
    /// The JSON value the contract hashes.
    pub fn value(&self) -> Value {
        json!({"host": self.host, "platform": self.platform, "sshHost": self.ssh_host.clone().map(Value::String).unwrap_or(Value::Null)})
    }
    fn key(&self) -> String {
        js_json::write(&json!([self.platform, self.ssh_host]))
    }
}

/// A configured primary source.
#[derive(Clone, Debug)]
pub struct PrimarySource {
    /// The computer.
    pub host: UsageHost,
    /// The selector to read.
    pub selector: String,
    /// The private Claude launcher context.
    pub claude_profile: Option<Value>,
}

impl PrimarySource {
    fn value(&self) -> Value {
        let mut out = self.host.value();
        out["selector"] = json!(self.selector);
        if let Some(profile) = &self.claude_profile {
            out["claudeProfile"] = profile.clone();
        }
        out
    }
}

/// The account/source the coordinator reads.
#[derive(Clone, Debug)]
pub struct UsageFailoverTarget {
    /// The dashboard account id.
    pub account_id: String,
    /// The provider.
    pub provider: String,
    /// The private CCS directory.
    pub ccs_dir: PathBuf,
    /// The full source/credential binding.
    pub binding: String,
    /// The configured primary sources.
    pub primary_sources: Vec<PrimarySource>,
    /// Windows Desktop quota uses the slower staggered cadence.
    pub claude_desktop_cadence: bool,
    /// The expected email, when bound.
    pub expected_email: Option<String>,
    /// The expected identity, when bound.
    pub expected_identity: Option<UsageIdentity>,
}

/// The transport/host/clock effects, injectable for fixtures.
pub trait FailoverDeps: Send + Sync {
    /// Run one helper request on a computer.
    fn transport<'a>(
        &'a self,
        host: &'a UsageHost,
        request: &'a Value,
    ) -> Pin<Box<dyn Future<Output = Result<Value, ()>> + Send + 'a>>;
    /// The configured remote computers.
    fn hosts<'a>(&'a self) -> Pin<Box<dyn Future<Output = Vec<UsageHost>> + Send + 'a>>;
    /// The wall clock in milliseconds.
    fn now(&self) -> f64;
    /// The request deadline override (tests need not wait for a real hang).
    fn request_deadline_ms(&self, _action: &str) -> Option<f64> {
        None
    }
}

#[derive(Clone, Debug)]
struct SavedReading {
    reading: UsageReading,
    host: String,
    candidate: UsageCandidate,
    fallback: bool,
}

#[derive(Clone, Debug, Default)]
struct State {
    identity: Option<UsageIdentity>,
    primary: Option<UsageCandidate>,
    saved: Option<SavedReading>,
    next_attempt: f64,
    fresh: Option<bool>,
    fresh_until: f64,
    primary_sign_in: Vec<String>,
    primary_failure: Option<String>,
    observed_plan: Option<String>,
    observed_plan_at: Option<String>,
    observed_plan_peer: Option<contract::MuseUsagePeer>,
}

/// The native failover coordinator.
pub struct FailoverCoordinator {
    deps: Arc<dyn FailoverDeps>,
    owners: Owners,
    dashboard_hosts: Vec<&'static str>,
    states: Mutex<HashMap<String, State>>,
    cooldowns: Mutex<HashMap<String, Cooldown>>,
    inventories: Mutex<HashMap<String, (f64, Vec<UsageCandidate>)>>,
    sign_in_selectors: Mutex<HashMap<String, Vec<String>>>,
    unread_selectors: Mutex<HashMap<String, Vec<String>>>,
    slots: tokio::sync::Semaphore,
}

fn computers() -> &'static Value {
    static COMPUTERS: std::sync::LazyLock<Value> = std::sync::LazyLock::new(|| {
        serde_json::from_str(include_str!("../../../../web-dashboard/public/computers.json"))
            .unwrap_or(Value::Null)
    });
    &COMPUTERS
}

fn host_label(host: &str) -> String {
    computers().get(host).and_then(|c| c.get("label")).and_then(Value::as_str).unwrap_or(host).to_owned()
}

fn helper_platform(host: &str) -> String {
    let os = computers().get(host).and_then(|c| c.get("os")).and_then(Value::as_str).unwrap_or("");
    if os == "linux" { "ubuntu".to_owned() } else { os.to_owned() }
}

fn dashboard_hosts() -> Vec<&'static str> {
    computers().as_object().map(|m| m.keys().map(String::as_str).collect()).unwrap_or_default()
}

fn email_matches(expected: Option<&str>, actual: Option<&str>) -> bool {
    match expected {
        None => true,
        Some(expected) => actual.is_some_and(|actual| expected.to_lowercase() == actual.to_lowercase()),
    }
}

fn wallet_matches(target: &UsageFailoverTarget, candidate: &UsageCandidate) -> bool {
    if target.provider != "opencode-wallet" {
        return true;
    }
    let suffix = target.account_id.strip_prefix("plan-opencode-go-console-mac-");
    match suffix {
        Some(s) if s.len() == 12 && s.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) => {
            candidate.identity.kind == "opencode-workspace" && candidate.identity.key.starts_with(s)
        }
        _ => false,
    }
}

fn candidate_matches_target(target: &UsageFailoverTarget, candidate: &UsageCandidate) -> bool {
    wallet_matches(target, candidate)
        && target.expected_identity.as_ref().map_or(true, |expected| same_identity(Some(expected), Some(&candidate.identity)))
        && candidate.email.as_deref().map_or(true, |email| email_matches(target.expected_email.as_deref(), Some(email)))
}

fn response_is_rate_limited(response: &Value) -> bool {
    response.get("action").and_then(Value::as_str) == Some("collect")
        && (response.get("failureCode").and_then(Value::as_str) == Some("rate_limited")
            || (response.get("failureCode").and_then(Value::as_str) == Some("source_changed")
                && response.get("rateLimited") == Some(&json!(true))))
}

fn tracks_verified_plan(target: &UsageFailoverTarget) -> bool {
    target.provider == "muse" || target.provider == "cursor"
}

impl FailoverCoordinator {
    /// A coordinator with the given effects and file owners.
    pub fn new(deps: Arc<dyn FailoverDeps>, owners: Owners) -> Self {
        Self {
            deps,
            owners,
            dashboard_hosts: dashboard_hosts(),
            states: Mutex::new(HashMap::new()),
            cooldowns: Mutex::new(HashMap::new()),
            inventories: Mutex::new(HashMap::new()),
            sign_in_selectors: Mutex::new(HashMap::new()),
            unread_selectors: Mutex::new(HashMap::new()),
            slots: tokio::sync::Semaphore::new(3),
        }
    }

    fn now(&self) -> f64 {
        self.deps.now()
    }

    /// The target's observation/cooldown key (the digest the TS coordinator hashes).
    pub fn key(&self, target: &UsageFailoverTarget) -> String {
        let ccs = aac_files::path::node::resolve(&target.ccs_dir, &target.ccs_dir)
            .unwrap_or_else(|_| target.ccs_dir.clone());
        let sources: Vec<Value> = target.primary_sources.iter().map(|s| s.value()).collect();
        let expected_email = target.expected_email.as_deref().map(|e| e.to_lowercase());
        let array = json!([
            ccs.to_string_lossy(),
            target.provider,
            target.account_id,
            target.binding,
            sources,
            expected_email.map(Value::String).unwrap_or(Value::Null),
            target.expected_identity.as_ref().map(|i| i.value()).unwrap_or(Value::Null),
        ]);
        aac_core::sha256_hex(js_json::write(&array))
    }

    fn file(&self, target: &UsageFailoverTarget) -> PathBuf {
        target.ccs_dir.join("account-usage-failover").join(format!("{}.json", self.key(target)))
    }

    fn matches_identity(&self, target: &UsageFailoverTarget, identity: Option<&UsageIdentity>, candidate: &UsageCandidate, host: &str) -> bool {
        same_identity(identity, Some(&candidate.identity))
            || (target.provider == "muse"
                && contract::is_bound_muse_peer(&target.ccs_dir, &self.key(target), identity, &candidate.identity, host, &self.dashboard_hosts))
    }

    fn validate_saved(&self, target: &UsageFailoverTarget, state: &mut State) {
        if let Some(peer) = state.observed_plan_peer.clone() {
            if !self.matches_identity(target, state.identity.as_ref(), &UsageCandidate {
                locator: String::new(), selectors: vec![], identity: peer.identity.clone(), revision: String::new(), email: None, plan: None,
            }, &peer.host) {
                state.observed_plan = None;
                state.observed_plan_at = None;
                state.observed_plan_peer = None;
            }
        }
        if let Some(saved) = state.saved.clone() {
            if !self.matches_identity(target, state.identity.as_ref(), &saved.candidate, &saved.host) {
                state.saved = None;
            }
        }
    }

    fn state(&self, target: &UsageFailoverTarget) -> State {
        let key = self.key(target);
        let mut states = self.states.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = states.get_mut(&key) {
            self.validate_saved(target, existing);
            return existing.clone();
        }
        let mut state = State { identity: target.expected_identity.clone(), ..Default::default() };
        let value = contract::read_private_cache(&self.file(target));
        if let Some(obj) = value.as_object() {
            if obj.get("version").and_then(Value::as_f64) == Some(1.0) && obj.get("key").and_then(Value::as_str) == Some(key.as_str()) {
                if let Some(sign_in) = obj.get("primarySignIn").and_then(Value::as_array) {
                    if sign_in.len() <= 2 && sign_in.iter().all(|h| matches!(h.as_str(), Some("mac") | Some("windows"))) {
                        state.primary_sign_in = sign_in.iter().filter_map(|h| h.as_str().map(str::to_owned)).collect();
                    }
                }
                if let Some(failure) = obj.get("primaryFailure").and_then(Value::as_str) {
                    if is_failure_code(failure) {
                        state.primary_failure = Some(failure.to_owned());
                    }
                }
                let identity = parse_identity(obj.get("identity").unwrap_or(&Value::Null));
                let identity_matches_target = identity.as_ref().is_some_and(|id| {
                    target.expected_identity.as_ref().map_or(true, |expected| same_identity(Some(expected), Some(id)))
                });
                if identity_matches_target {
                    state.identity = identity;
                    state.observed_plan = if tracks_verified_plan(target) {
                        safe_usage_text(obj.get("observedPlan").unwrap_or(&Value::Null), 160)
                    } else {
                        None
                    };
                    state.observed_plan_at = usage_timestamp(obj.get("observedPlanAt").unwrap_or(&Value::Null), self.now());
                    if state.observed_plan_at.is_none() {
                        state.observed_plan = None;
                    }
                    if let Some(peer) = obj.get("observedPlanPeer") {
                        if !peer.is_null() {
                            state.observed_plan_peer = if target.provider == "muse" {
                                contract::parse_muse_peer(peer, &self.dashboard_hosts)
                            } else {
                                None
                            };
                            if state.observed_plan_peer.is_none() {
                                state.observed_plan = None;
                                state.observed_plan_at = None;
                            }
                        }
                    }
                    state.primary = parse_candidate(obj.get("primary").unwrap_or(&Value::Null));
                    if let Some(saved) = obj.get("saved").and_then(Value::as_object) {
                        let host = saved.get("host").and_then(Value::as_str).unwrap_or("");
                        if self.dashboard_hosts.iter().any(|h| *h == host) {
                            let reading = parse_reading(saved.get("reading").unwrap_or(&Value::Null), self.now());
                            let candidate = parse_candidate(saved.get("candidate").unwrap_or(&Value::Null));
                            if let (Some(candidate), Some(reading)) = (candidate, reading) {
                                if reading.has_usage() && email_matches(target.expected_email.as_deref(), reading.email.as_deref()) {
                                    state.saved = Some(SavedReading {
                                        reading,
                                        candidate,
                                        host: host.to_owned(),
                                        fallback: saved.get("fallback") == Some(&json!(true)),
                                    });
                                }
                            }
                        }
                    }
                    state.fresh = match obj.get("fresh") {
                        Some(&json!(true)) if state.saved.as_ref().is_some_and(|s| s.reading.status == "ok") => Some(true),
                        Some(&json!(false)) => Some(false),
                        _ => None,
                    };
                    if let Some(until) = obj.get("freshUntil").and_then(Value::as_f64) {
                        if until.is_finite() {
                            state.fresh_until = until.min(self.now() + DAY_MS);
                        }
                    }
                }
                if (identity_matches_target || (target.claude_desktop_cadence && state.primary_failure.as_deref() == Some("source_changed")))
                    && let Some(next) = obj.get("nextAttempt").and_then(Value::as_f64)
                {
                    if next.is_finite() {
                        state.next_attempt = next.min(self.now() + DAY_MS);
                    }
                }
            }
        }
        self.validate_saved(target, &mut state);
        states.insert(key.clone(), state.clone());
        if states.len() > 256 {
            let evict = states.keys().find(|k| *k != &key).cloned();
            if let Some(old) = evict {
                states.remove(&old);
            }
        }
        state
    }

    fn persist(&self, target: &UsageFailoverTarget, state: &mut State) {
        self.validate_saved(target, state);
        let key = self.key(target);
        let value = json!({
            "version": 1,
            "key": key,
            "identity": state.identity.as_ref().map(|i| i.value()).unwrap_or(Value::Null),
            "primary": state.primary.as_ref().map(|c| c.value()).unwrap_or(Value::Null),
            "saved": state.saved.as_ref().map(|s| json!({
                "reading": s.reading.value(), "host": s.host, "candidate": s.candidate.value(), "fallback": s.fallback,
            })).unwrap_or(Value::Null),
            "nextAttempt": state.next_attempt,
            "fresh": state.fresh.map(Value::Bool).unwrap_or(Value::Null),
            "freshUntil": state.fresh_until,
            "primaryFailure": state.primary_failure.clone().map(Value::String).unwrap_or(Value::Null),
            "primarySignIn": state.primary_sign_in,
            "observedPlan": state.observed_plan.clone().map(Value::String).unwrap_or(Value::Null),
            "observedPlanAt": state.observed_plan_at.clone().map(Value::String).unwrap_or(Value::Null),
            "observedPlanPeer": state.observed_plan_peer.as_ref().map(|p| json!({"host": p.host, "identity": p.identity.value()})).unwrap_or(Value::Null),
        });
        let bytes = js_json::write(&value);
        if bytes.len() > 256 * 1024 {
            return;
        }
        let file = format!("{}.json", key);
        let Ok(rows) = aac_files::registry::rows() else { return };
        if let Ok(store) = self.owners.claim(rows, "account-usage-observations", &target.ccs_dir, Some(&file)) {
            let _ = store.write_private_cache(bytes.as_bytes());
        }
    }

    fn fresh_until(&self, target: &UsageFailoverTarget, refresh_after_seconds: f64) -> f64 {
        let seconds = if refresh_after_seconds > 0.0 {
            refresh_after_seconds.min(86_400.0)
        } else {
            read_refresh_seconds(&target.ccs_dir)
        };
        self.now() + seconds * 1000.0
    }

    fn cooldown_state(&self, ccs_dir: &Path, key: &str) -> Cooldown {
        let memory = self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).get(key).copied().unwrap_or_default();
        let saved = failover::persisted(ccs_dir, key, self.now()).ok().flatten().unwrap_or_default();
        Cooldown::merge(memory, saved, self.now())
    }

    fn hold_cooldown_inner(&self, ccs_dir: &Path, key: &str, requested_until: f64, streak: Option<u8>) -> f64 {
        if !hex_digest(key) || !requested_until.is_finite() {
            return 0.0;
        }
        let now = self.now();
        let previous = self.cooldown_state(ccs_dir, key);
        let merged = previous.hold(now, requested_until, streak);
        self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).insert(key.to_owned(), merged);
        let mut record = json!({"now": now, "until": merged.until});
        if let Some(streak) = merged.claude_429_streak {
            record["claude429Streak"] = json!(streak);
        }
        let _ = failover::save(&self.owners, ccs_dir, key, js_json::write(&record).as_bytes());
        merged.until
    }

    /// The identity hold a native collector must respect before its own request.
    pub fn cooldown_until(&self, target: &UsageFailoverTarget) -> f64 {
        let state = self.state(target);
        let Some(identity) = target.expected_identity.clone().or(state.identity) else { return 0.0 };
        let key = identity_cooldown_key(&target.ccs_dir, &target.provider, &identity);
        let until = self.cooldown_state(&target.ccs_dir, &key).until;
        if until > self.now() { until } else { 0.0 }
    }

    /// Apply an external hold (a native collector's own 429).
    pub fn hold_cooldown(&self, target: &UsageFailoverTarget, requested_until: f64) -> f64 {
        let mut state = self.state(target);
        let Some(identity) = target.expected_identity.clone().or(state.identity.clone()) else { return 0.0 };
        let key = identity_cooldown_key(&target.ccs_dir, &target.provider, &identity);
        let until = self.hold_cooldown_inner(&target.ccs_dir, &key, requested_until, None);
        self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), Cooldown { until, claude_429_streak: self.cooldown_state(&target.ccs_dir, &key).claude_429_streak });
        state.next_attempt = state.next_attempt.max(until);
        state.fresh = Some(false);
        state.primary_failure = Some("rate_limited".into());
        self.persist(target, &mut state);
        until
    }
}

/// `identityUsageCooldownKey`.
pub fn identity_cooldown_key(ccs_dir: &Path, provider: &str, identity: &UsageIdentity) -> String {
    let ccs = aac_files::path::node::resolve(ccs_dir, ccs_dir).unwrap_or_else(|_| ccs_dir.to_path_buf());
    aac_core::sha256_hex(js_json::write(&json!([ccs.to_string_lossy(), provider, identity.value()])))
}

fn read_refresh_seconds(ccs_dir: &Path) -> f64 {
    let file = ccs_dir.join("account-refresh-settings.json");
    let value = contract::read_private_cache(&file);
    value.get("refreshIntervalSeconds").and_then(Value::as_f64).filter(|n| n.is_finite() && (30.0..=3600.0).contains(n)).unwrap_or(60.0)
}

fn has_usage_value(account: &Value) -> bool {
    let status = account.get("status").and_then(Value::as_str).unwrap_or("");
    (status == "ok" || status == "cached")
        && account.get("windows").and_then(Value::as_array).is_some_and(|w| !w.is_empty())
        && account.get("sampledAt").is_some_and(|s| !s.is_null())
}

fn parse_opt(s: Option<&str>) -> f64 {
    s.and_then(contract::parse_ms).unwrap_or(f64::NAN)
}

impl FailoverCoordinator {
    fn inventory_key(&self, target: &UsageFailoverTarget, host: &UsageHost) -> String {
        js_json::write(&json!([
            target.ccs_dir.to_string_lossy(),
            target.provider,
            host.key(),
            self.claude_profile(target, host).unwrap_or(Value::Null),
        ]))
    }

    fn claude_profile(&self, target: &UsageFailoverTarget, host: &UsageHost) -> Option<Value> {
        if target.provider != "claude" {
            return None;
        }
        target.primary_sources.iter().find(|s| s.host.key() == host.key()).and_then(|s| s.claude_profile.clone())
    }

    fn apply_inventory_sign_in(&self, target: &UsageFailoverTarget, state: &mut State, host: &UsageHost) {
        if target.provider != "claude" || (host.host != "mac" && host.host != "windows") {
            return;
        }
        let selectors = self.sign_in_selectors.lock().unwrap_or_else(|e| e.into_inner()).get(&self.inventory_key(target, host)).cloned().unwrap_or_default();
        if target.primary_sources.iter().any(|s| s.host.key() == host.key() && selectors.contains(&s.selector)) {
            if !state.primary_sign_in.contains(&host.host) {
                state.primary_sign_in.push(host.host.clone());
            }
        }
    }

    async fn inventory(&self, target: &UsageFailoverTarget, host: &UsageHost) -> Option<Vec<UsageCandidate>> {
        let key = self.inventory_key(target, host);
        {
            let cached = self.inventories.lock().unwrap_or_else(|e| e.into_inner());
            if let Some((expires, candidates)) = cached.get(&key) {
                if *expires > self.now() {
                    return Some(candidates.clone());
                }
            }
        }
        let mut request = json!({"version":1,"action":"inventory","provider":target.provider});
        if let Some(profile) = self.claude_profile(target, host) {
            request["claudeProfile"] = profile;
        }
        if host.ssh_host.is_none() {
            request["ccsDir"] = json!(target.ccs_dir.to_string_lossy());
        }
        let response = self.request(host, &request).await.ok();
        let result = match response {
            Some(value) if value.get("action").and_then(Value::as_str) == Some("inventory") => {
                if target.provider == "claude" {
                    let sign_in = value.get("signInNeededSelectors").and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default();
                    self.sign_in_selectors.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), sign_in);
                }
                let incomplete = value.get("incompleteSelectors").and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect::<Vec<_>>()).unwrap_or_default();
                if !incomplete.is_empty() {
                    self.unread_selectors.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), incomplete);
                } else {
                    self.unread_selectors.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
                }
                value.get("candidates").and_then(Value::as_array).map(|a| a.iter().filter_map(parse_candidate).collect())
            }
            _ => None,
        };
        self.inventories.lock().unwrap_or_else(|e| e.into_inner()).insert(key, (self.now() + 15_000.0, result.clone().unwrap_or_default()));
        result
    }

    async fn request(&self, host: &UsageHost, request: &Value) -> Result<Value, ()> {
        let _permit = self.slots.acquire().await.map_err(|_| ())?;
        self.deps.transport(host, request).await
    }

    fn apply_cooldown(&self, target: &UsageFailoverTarget, state: &mut State, cooldown_key: &str) -> bool {
        let merged = self.cooldown_state(&target.ccs_dir, cooldown_key);
        if merged.until <= self.now() {
            self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).remove(cooldown_key);
            return false;
        }
        self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).insert(cooldown_key.to_owned(), merged);
        state.next_attempt = state.next_attempt.max(merged.until);
        true
    }

    async fn cursor_session(&self, target: &UsageFailoverTarget, state: &mut State) -> (Option<(UsageHost, UsageCandidate)>, bool, bool) {
        let mut hosts: Vec<UsageHost> = target.primary_sources.iter().map(|s| s.host.clone()).collect();
        hosts.extend(self.deps.hosts().await);
        let mut dedup: Vec<UsageHost> = Vec::new();
        for host in hosts {
            if !dedup.iter().any(|h| h.key() == host.key()) {
                dedup.push(host);
            }
        }
        let selector = target.primary_sources.first().map(|s| s.selector.clone()).unwrap_or_else(|| "discover".into());
        let mut found: Vec<(UsageHost, UsageCandidate)> = Vec::new();
        let mut configured_unreadable = false;
        for host in &dedup {
            let candidates = self.inventory(target, host).await;
            self.apply_inventory_sign_in(target, state, host);
            let configured = target.primary_sources.iter().any(|s| s.host.key() == host.key());
            let gaps = self.unread_selectors.lock().unwrap_or_else(|e| e.into_inner()).get(&self.inventory_key(target, host)).cloned().unwrap_or_default();
            if configured && (candidates.is_none() || gaps.iter().any(|g| g == "*" || g == &selector)) {
                configured_unreadable = true;
            }
            let Some(candidates) = candidates else { continue };
            for candidate in candidates {
                if !candidate.selectors.contains(&selector) {
                    continue;
                }
                if candidate_matches_target(target, &candidate) {
                    found.push((host.clone(), candidate));
                }
            }
        }
        let mut by_identity: Vec<(String, (UsageHost, UsageCandidate))> = Vec::new();
        for item in found {
            let id = js_json::write(&json!([item.1.identity.kind, item.1.identity.key]));
            if !by_identity.iter().any(|(k, _)| *k == id) {
                by_identity.push((id, item));
            }
        }
        if by_identity.len() > 1 {
            return (None, true, configured_unreadable);
        }
        if by_identity.len() == 1 {
            return (Some(by_identity.into_iter().next().unwrap().1), false, configured_unreadable);
        }
        (None, false, configured_unreadable)
    }

    async fn peer_lists(&self, target: &UsageFailoverTarget, state: &mut State) -> Vec<(UsageHost, Option<Vec<UsageCandidate>>)> {
        let mut hosts: Vec<UsageHost> = target.primary_sources.iter().map(|s| s.host.clone()).collect();
        hosts.extend(self.deps.hosts().await);
        let mut dedup: Vec<UsageHost> = Vec::new();
        for host in hosts {
            if !dedup.iter().any(|h| h.key() == host.key()) {
                dedup.push(host);
            }
        }
        let mut lists = Vec::new();
        for host in dedup {
            let candidates = self.inventory(target, &host).await;
            lists.push((host, candidates));
        }
        for (host, _) in &lists {
            self.apply_inventory_sign_in(target, state, host);
        }
        lists
    }

    fn retain_extras(&self, target: &UsageFailoverTarget, state: &State, reading: UsageReading, candidate: &UsageCandidate, host: &str) -> UsageReading {
        if target.provider != "claude" {
            return reading;
        }
        let previous = state.saved.as_ref();
        let same_source = previous.is_some_and(|p| {
            p.host == host && p.candidate.locator == candidate.locator && p.candidate.revision == candidate.revision && same_identity(Some(&p.candidate.identity), Some(&candidate.identity))
        });
        contract::retain_claude_extras(previous.map(|p| &p.reading), &reading, same_source, self.now())
    }

    /// Display-only quota recovery; never changes which account a program uses.
    pub fn peek(&self, target: &UsageFailoverTarget, template: &Value) -> Value {
        let state = self.state(target);
        let newer = parse_opt(template.get("sampledAt").and_then(Value::as_str))
            > parse_opt(state.saved.as_ref().and_then(|s| s.reading.sampled_at.as_deref()));
        let saved_sampled_value = state.saved.as_ref().map(|s| json!(s.reading.sampled_at));
        let same_cached = template.get("status").and_then(Value::as_str) == Some("cached")
            && template.get("sampledAt") == saved_sampled_value.as_ref();
        if has_usage_value(template) && (state.saved.is_none() || newer || same_cached) {
            return self.with_sign_in(template.clone(), &state);
        }
        self.project(target, template, &state, state.fresh != Some(true))
    }

    /// Collect a reading for the account.
    pub async fn collect(&self, target: &UsageFailoverTarget, template: &Value) -> Value {
        self.run(target, template, false).await
    }

    /// Recover using an already-read primary.
    pub async fn recover(&self, target: &UsageFailoverTarget, primary: &Value) -> Value {
        self.run(target, primary, true).await
    }

    async fn run(&self, target: &UsageFailoverTarget, template: &Value, primary_already_read: bool) -> Value {
        if template.get("statusReason").and_then(Value::as_str) == Some("identity_unbound") {
            return template.clone();
        }
        let key = self.key(target);
        let mut state = self.state(target);
        if self.now() >= state.next_attempt {
            let outcome = self.refresh(target, template, &mut state, primary_already_read).await;
            if outcome.is_err() {
                state.next_attempt = self.now() + 30_000.0;
            }
            self.states.lock().unwrap_or_else(|e| e.into_inner()).insert(key.clone(), state.clone());
            self.persist(target, &mut state);
        }
        let sampled = parse_opt(template.get("sampledAt").and_then(Value::as_str));
        let saved_sampled = parse_opt(state.saved.as_ref().and_then(|s| s.reading.sampled_at.as_deref()));
        let saved_sampled_value = state.saved.as_ref().map(|s| json!(s.reading.sampled_at));
        let keep_template = has_usage_value(template)
            && template.get("status").and_then(Value::as_str) == Some("ok")
            && (state.saved.is_none()
                || sampled > saved_sampled
                || (state.fresh == Some(true) && self.now() < state.fresh_until && template.get("sampledAt") == saved_sampled_value.as_ref()));
        if keep_template {
            return self.with_sign_in(template.clone(), &state);
        }
        self.project(target, template, &state, state.fresh != Some(true))
    }
}

impl FailoverCoordinator {
    async fn refresh(&self, target: &UsageFailoverTarget, template: &Value, state: &mut State, primary_already_read: bool) -> Result<(), ()> {
        state.next_attempt = self.now() + 15_000.0;
        state.fresh = Some(false);
        state.primary_failure = if template.get("status").and_then(Value::as_str) == Some("needs_sign_in") { Some("needs_sign_in".into()) } else { None };
        self.persist(target, state);
        let mut primaries: Vec<(UsageHost, UsageCandidate)> = Vec::new();
        let mut missing_primary = false;
        for source in &target.primary_sources {
            let candidates = self.inventory(target, &source.host).await;
            self.apply_inventory_sign_in(target, state, &source.host);
            let selected: Vec<UsageCandidate> = candidates.as_ref().map(|c| c.iter().filter(|item| item.selectors.contains(&source.selector)).cloned().collect()).unwrap_or_default();
            let eligible: Vec<UsageCandidate> = selected.iter().filter(|item| candidate_matches_target(target, item)).cloned().collect();
            let candidate = eligible.iter().find(|item| email_matches(target.expected_email.as_deref(), item.email.as_deref())).or_else(|| eligible.first()).cloned();
            if !selected.is_empty() && candidate.is_none() {
                state.identity = target.expected_identity.clone();
                state.primary = None;
                state.saved = None;
                state.observed_plan = None;
                state.observed_plan_at = None;
                state.observed_plan_peer = None;
                return Ok(());
            }
            let Some(candidate) = candidate else {
                let unread = self.unread_selectors.lock().unwrap_or_else(|e| e.into_inner()).get(&self.inventory_key(target, &source.host)).cloned().unwrap_or_default();
                let family = source.selector.find(':').map(|i| format!("{}:*", &source.selector[..i]));
                if candidates.is_some()
                    && !unread.iter().any(|u| u == "*")
                    && !unread.contains(&source.selector)
                    && !family.as_ref().is_some_and(|f| unread.contains(f))
                {
                    missing_primary = true;
                }
                continue;
            };
            if state.identity.as_ref().map_or(true, |id| !same_identity(Some(id), Some(&candidate.identity))) {
                state.identity = Some(candidate.identity.clone());
                state.saved = None;
                state.observed_plan = None;
                state.observed_plan_at = None;
                state.observed_plan_peer = None;
                state.primary = Some(candidate.clone());
            } else if let Some(primary) = &state.primary {
                if primary.locator == candidate.locator && primary.revision != candidate.revision && target.provider == "claude" {
                    if let Some(saved) = &mut state.saved {
                        saved.reading = contract::strip_claude_extras(&saved.reading);
                    }
                }
            }
            state.primary = Some(candidate.clone());
            primaries.push((source.host.clone(), candidate));
            break;
        }
        if primaries.is_empty() && target.provider == "cursor" && target.expected_identity.is_none() {
            let (chosen, conflict, configured_unreadable) = self.cursor_session(target, state).await;
            let cached = state.identity.clone();
            let different = chosen.is_some() && cached.is_some() && !same_identity(cached.as_ref(), chosen.as_ref().map(|c| &c.1.identity));
            if let Some((host, candidate)) = chosen {
                if !(configured_unreadable && different) {
                    if !same_identity(cached.as_ref(), Some(&candidate.identity)) {
                        state.saved = None;
                        state.observed_plan = None;
                        state.observed_plan_at = None;
                        state.observed_plan_peer = None;
                    }
                    state.identity = Some(candidate.identity.clone());
                    state.primary = Some(candidate.clone());
                    primaries.push((host, candidate));
                }
            } else if configured_unreadable && cached.is_some() {
                // Keep the verified reading; the known account refreshes below.
            } else if conflict || !configured_unreadable {
                state.identity = None;
                state.primary = None;
                state.saved = None;
                state.observed_plan = None;
                state.observed_plan_at = None;
                state.observed_plan_peer = None;
                return Ok(());
            } else if cached.is_none() {
                return Ok(());
            }
        } else if primaries.is_empty() && missing_primary && target.expected_identity.is_none() {
            state.identity = None;
            state.primary = None;
            state.saved = None;
            state.observed_plan = None;
            state.observed_plan_at = None;
            state.observed_plan_peer = None;
            return Ok(());
        }
        if state.identity.is_none() {
            return Ok(());
        }
        let cooldown_key = identity_cooldown_key(&target.ccs_dir, &target.provider, state.identity.as_ref().unwrap());
        if primary_already_read && has_usage_value(template) && !primaries.is_empty() {
            if let Some(reading) = parse_reading(template, self.now()) {
                if reading.has_usage() && email_matches(target.expected_email.as_deref(), reading.email.as_deref()) {
                    let newer = state.saved.as_ref().map_or(true, |s| parse_opt(reading.sampled_at.as_deref()) >= parse_opt(s.reading.sampled_at.as_deref()));
                    if newer {
                        let retained = self.retain_extras(target, state, reading, &primaries[0].1, &primaries[0].0.host);
                        state.saved = Some(SavedReading { reading: retained, candidate: primaries[0].1.clone(), host: primaries[0].0.host.clone(), fallback: false });
                        state.fresh = Some(template.get("status").and_then(Value::as_str) == Some("ok"));
                        if state.fresh == Some(true) {
                            state.fresh_until = self.fresh_until(target, 0.0);
                        }
                    }
                    if template.get("status").and_then(Value::as_str) == Some("ok") {
                        state.primary_failure = None;
                        if primaries[0].0.host == "mac" || primaries[0].0.host == "windows" {
                            state.primary_sign_in.retain(|h| h != &primaries[0].0.host);
                        }
                        return Ok(());
                    }
                }
            }
        }
        if self.apply_cooldown(target, state, &cooldown_key) {
            return Ok(());
        }
        if primary_already_read {
            let message = template.get("message").and_then(Value::as_str).unwrap_or("");
            if rate_limit_message(message) {
                let until = self.hold_cooldown_inner(&target.ccs_dir, &cooldown_key, self.now() + 60_000.0, None);
                state.next_attempt = until;
                self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).insert(cooldown_key.clone(), Cooldown { until, claude_429_streak: self.cooldown_state(&target.ccs_dir, &cooldown_key).claude_429_streak });
                return Ok(());
            }
        }
        let mut tried: Vec<String> = Vec::new();
        for (host, candidate) in &primaries {
            if !primary_already_read {
                if self.attempt(target, state, host, candidate, false, &mut tried, &cooldown_key, &primaries).await == "done" {
                    return Ok(());
                }
            } else {
                tried.push(js_json::write(&json!([host.key(), candidate.locator])));
            }
        }
        for (host, candidates) in self.peer_lists(target, state).await {
            for candidate in candidates.unwrap_or_default() {
                if self.attempt(target, state, &host, &candidate, true, &mut tried, &cooldown_key, &primaries).await == "done" {
                    return Ok(());
                }
            }
        }
        state.next_attempt = self.now() + 30_000.0;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn attempt(&self, target: &UsageFailoverTarget, state: &mut State, host: &UsageHost, candidate: &UsageCandidate, fallback: bool, tried: &mut Vec<String>, cooldown_key: &str, primaries: &[(UsageHost, UsageCandidate)]) -> &'static str {
        if !self.matches_identity(target, state.identity.as_ref(), candidate, &host.host)
            || !wallet_matches(target, candidate)
            || candidate.email.as_deref().is_some_and(|email| !email_matches(target.expected_email.as_deref(), Some(email)))
        {
            return "next";
        }
        let peer_key = if same_identity(state.identity.as_ref(), Some(&candidate.identity)) {
            None
        } else {
            Some(identity_cooldown_key(&target.ccs_dir, &target.provider, &candidate.identity))
        };
        let peer_held = |s: &Self| peer_key.as_ref().is_some_and(|k| s.cooldown_state(&target.ccs_dir, k).until > s.now());
        if peer_held(self) {
            return "next";
        }
        let key = js_json::write(&json!([host.key(), candidate.locator]));
        if tried.contains(&key) {
            return "next";
        }
        tried.push(key);
        if self.apply_cooldown(target, state, cooldown_key) || peer_held(self) {
            return if self.apply_cooldown(target, state, cooldown_key) { "done" } else { "next" };
        }
        let mut request = json!({"version":1,"action":"collect","provider":target.provider,"candidate":candidate.value()});
        if let Some(profile) = self.claude_profile(target, host) {
            request["claudeProfile"] = profile;
        }
        if host.ssh_host.is_none() {
            request["ccsDir"] = json!(target.ccs_dir.to_string_lossy());
        }
        let response = self.request(host, &request).await;
        if let Ok(value) = &response {
            if response_is_rate_limited(value) {
                let now = self.now();
                let retry_after = value.get("retryAfterSeconds").and_then(Value::as_f64);
                let provider_until = now + retry_after.unwrap_or(60.0).clamp(60.0, 86_400.0) * 1000.0;
                let mut until = provider_until;
                let mut streak: Option<u8> = None;
                if target.claude_desktop_cadence {
                    let previous = self.cooldown_state(&target.ccs_dir, cooldown_key);
                    let s = (previous.claude_429_streak.unwrap_or(0) + 1).min(MAX_CLAUDE_429_STREAK);
                    streak = Some(s);
                    let floor = (provider_until - now) / 1000.0;
                    if let Ok(delay) = failover::claude_delay_ms(&self.key(target), floor, s) {
                        until = now + delay;
                    }
                    state.primary_failure = Some("rate_limited".into());
                }
                state.next_attempt = self.hold_cooldown_inner(&target.ccs_dir, cooldown_key, until, streak);
                self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).insert(cooldown_key.to_owned(), Cooldown { until: state.next_attempt, claude_429_streak: streak.or(self.cooldown_state(&target.ccs_dir, cooldown_key).claude_429_streak) });
                if let Some(peer) = &peer_key {
                    let peer_until = self.hold_cooldown_inner(&target.ccs_dir, peer, until, None);
                    self.cooldowns.lock().unwrap_or_else(|e| e.into_inner()).insert(peer.clone(), Cooldown { until: peer_until, claude_429_streak: self.cooldown_state(&target.ccs_dir, peer).claude_429_streak });
                }
            }
        }
        let response = match response {
            Ok(value) => value,
            Err(_) => return "next",
        };
        if response.get("action").and_then(Value::as_str) != Some("collect") {
            return "next";
        }
        let adopted_cursor = target.provider == "cursor"
            && primaries.iter().any(|(h, c)| h.key() == host.key() && c.locator == candidate.locator && same_identity(Some(&c.identity), Some(&candidate.identity)));
        let primary_source = adopted_cursor
            || target.primary_sources.iter().any(|s| s.host.key() == host.key() && candidate.selectors.contains(&s.selector));
        let failure_code = response.get("failureCode").and_then(Value::as_str).map(str::to_owned);
        if primary_source {
            state.primary_failure = failure_code.clone();
        }
        if target.provider == "claude" && primary_source && failure_code.as_deref() == Some("needs_sign_in") && (host.host == "mac" || host.host == "windows") {
            if !state.primary_sign_in.contains(&host.host) {
                state.primary_sign_in.push(host.host.clone());
            }
        }
        if matches!(failure_code.as_deref(), Some("needs_sign_in") | Some("identity_mismatch") | Some("account_mismatch") | Some("identity_unavailable") | Some("source_changed") | Some("plan_mismatch") | Some("team_mismatch"))
            && state.saved.as_ref().is_some_and(|s| s.candidate.locator == candidate.locator && s.host == host.host)
        {
            state.saved = None;
        }
        let blocked = response_is_rate_limited(&response);
        if primary_source && matches!(failure_code.as_deref(), Some("identity_mismatch") | Some("account_mismatch") | Some("source_changed") | Some("plan_mismatch") | Some("team_mismatch")) {
            state.identity = target.expected_identity.clone();
            state.primary = None;
            state.saved = None;
            state.observed_plan = None;
            state.observed_plan_at = None;
            state.observed_plan_peer = None;
            return "done";
        }
        let resp_candidate = response.get("candidate").and_then(parse_candidate);
        let candidate_ok = resp_candidate.as_ref().is_some_and(|c| c.locator == candidate.locator && c.revision == candidate.revision && same_identity(Some(&candidate.identity), Some(&c.identity)));
        if !candidate_ok {
            return if blocked { "done" } else { "next" };
        }
        if !self.matches_identity(target, state.identity.as_ref(), candidate, &host.host) {
            return "next";
        }
        let observed_plan = response.get("observedPlan").and_then(|v| safe_usage_text(v, 160));
        let observed_plan_at = response.get("observedPlanAt").and_then(|v| usage_timestamp(v, self.now()));
        if tracks_verified_plan(target) {
            if let (Some(plan), Some(at)) = (&observed_plan, &observed_plan_at) {
                let bound = parse_opt(state.observed_plan_at.as_deref()).max(parse_opt(state.saved.as_ref().and_then(|s| s.reading.sampled_at.as_deref())));
                if parse_opt(Some(at)) >= bound {
                    state.observed_plan = Some(plan.clone());
                    state.observed_plan_at = Some(at.clone());
                    state.observed_plan_peer = if same_identity(state.identity.as_ref(), Some(&candidate.identity)) {
                        None
                    } else {
                        Some(contract::MuseUsagePeer { host: host.host.clone(), identity: candidate.identity.clone() })
                    };
                    if state.saved.as_ref().is_some_and(|s| s.reading.plan.as_deref() != Some(plan)) {
                        state.saved = None;
                    }
                }
            }
        }
        let reading = response.get("sample").and_then(|s| parse_reading(s, self.now()));
        let Some(reading) = reading else { return if blocked { "done" } else { "next" } };
        if !reading.has_usage() || !email_matches(target.expected_email.as_deref(), reading.email.as_deref()) {
            return if blocked { "done" } else { "next" };
        }
        if tracks_verified_plan(target) {
            let sampled = parse_opt(reading.sampled_at.as_deref());
            let known_plan_at = parse_opt(state.observed_plan_at.as_deref());
            if reading.status == "ok" && sampled >= known_plan_at && sampled >= parse_opt(state.saved.as_ref().and_then(|s| s.reading.sampled_at.as_deref())) {
                state.observed_plan = reading.plan.clone();
                state.observed_plan_at = reading.sampled_at.clone();
                state.observed_plan_peer = if same_identity(state.identity.as_ref(), Some(&candidate.identity)) { None } else { Some(contract::MuseUsagePeer { host: host.host.clone(), identity: candidate.identity.clone() }) };
            } else if state.observed_plan.is_some() && reading.plan != state.observed_plan {
                return if blocked { "done" } else { "next" };
            }
        }
        if target.provider == "claude" && primary_source && reading.status == "ok" && (host.host == "mac" || host.host == "windows") {
            state.primary_sign_in.retain(|h| h != &host.host);
        }
        let newer = state.saved.as_ref().map_or(true, |s| parse_opt(reading.sampled_at.as_deref()) >= parse_opt(s.reading.sampled_at.as_deref()));
        if newer {
            let retained = self.retain_extras(target, state, reading.clone(), candidate, &host.host);
            state.saved = Some(SavedReading { reading: retained, candidate: candidate.clone(), host: host.host.clone(), fallback });
            state.fresh = Some(reading.status == "ok");
            if state.fresh == Some(true) {
                let refresh_after = if failure_code.is_none() { response.get("refreshAfterSeconds").and_then(Value::as_f64).unwrap_or(0.0) } else { 0.0 };
                state.fresh_until = self.fresh_until(target, refresh_after);
            }
        }
        if blocked {
            return "done";
        }
        if target.claude_desktop_cadence && reading.status == "ok" && failure_code.is_none() {
            let now = self.now();
            let cooldown = self.cooldown_state(&target.ccs_dir, cooldown_key);
            if cooldown.until == 0.0 && cooldown.claude_429_streak.is_some_and(|n| n > 0) {
                self.hold_cooldown_inner(&target.ccs_dir, cooldown_key, now, Some(0));
            }
            let floor = read_refresh_seconds(&target.ccs_dir).max(response.get("refreshAfterSeconds").and_then(Value::as_f64).unwrap_or(0.0));
            if let Ok(delay) = failover::claude_delay_ms(&self.key(target), floor, 0) {
                state.next_attempt = now + delay;
            }
            if state.fresh == Some(true) {
                state.fresh_until = state.next_attempt;
            }
            return "done";
        }
        if failure_code.is_none() {
            if let Some(refresh_after) = response.get("refreshAfterSeconds").and_then(Value::as_f64) {
                if refresh_after > 0.0 {
                    state.next_attempt = state.next_attempt.max(self.now() + refresh_after.min(86_400.0) * 1000.0);
                    return "done";
                }
            }
        }
        if reading.status == "ok" { "done" } else { "next" }
    }
}

fn rate_limit_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    if lower.contains("too many requests") {
        return true;
    }
    let bytes = lower.as_bytes();
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    for i in 0..lower.len() {
        if lower[i..].starts_with("rate") {
            let after = &lower[i + 4..];
            if after.starts_with("limit") {
                return true;
            }
            if let Some(&c) = after.as_bytes().first() {
                if c != b'\n' && after[1..].starts_with("limit") {
                    return true;
                }
            }
        }
        if lower[i..].starts_with("429") {
            let before_ok = i == 0 || !word(bytes[i - 1]);
            let after = i + 3;
            let after_ok = after >= bytes.len() || !word(bytes[after]);
            if before_ok && after_ok {
                return true;
            }
        }
    }
    false
}

impl FailoverCoordinator {
    fn project(&self, target: &UsageFailoverTarget, template: &Value, state: &State, cached: bool) -> Value {
        let mut state = state.clone();
        self.validate_saved(target, &mut state);
        let mut template = template.clone();
        let identity_unbound = template.get("statusReason").and_then(Value::as_str) == Some("identity_unbound");
        let Some(saved) = state.saved.clone() else {
            if target.provider == "cursor" && state.observed_plan.is_some() && state.identity.is_some() && !identity_unbound {
                template["plan"] = json!(state.observed_plan);
            }
            return self.with_sign_in(template, &state);
        };
        if identity_unbound {
            return self.with_sign_in(template, &state);
        }
        let reading = &saved.reading;
        if !email_matches(target.expected_email.as_deref(), reading.email.as_deref()) {
            return self.with_sign_in(template, &state);
        }
        let now = self.now();
        let refresh_due = state.fresh == Some(true) && now >= state.fresh_until;
        let held = state.primary_failure.as_deref() == Some("rate_limited") && state.next_attempt > now;
        let stale = cached || reading.status == "cached" || refresh_due;
        let native_muse_peer = target.provider == "muse" && !same_identity(state.identity.as_ref(), Some(&saved.candidate.identity));
        let mut result = template.clone();
        if template.get("email").is_none() || template.get("email") == Some(&Value::Null) {
            result["email"] = json!(reading.email);
        }
        result["plan"] = json!(reading.plan);
        match &reading.plan_class {
            Some(class) => result["planClass"] = json!(class),
            None => { result.as_object_mut().map(|m| m.remove("planClass")); }
        }
        result["sampledAt"] = json!(reading.sampled_at);
        result["fetchedAt"] = json!(reading.fetched_at);
        result["windows"] = Value::Array(reading.windows.clone());
        match &reading.antigravity_plan {
            Some(detail) => result["antigravityPlan"] = detail.clone(),
            None => { result.as_object_mut().map(|m| m.remove("antigravityPlan")); }
        }
        result["status"] = json!(if stale { "cached" } else { "ok" });
        result["source"] = json!(format!(
            "Verified {}quota on {}{}",
            if native_muse_peer { "native subscription " } else { "" },
            host_label(&saved.host),
            if saved.fallback { " (fallback)" } else { "" }
        ));
        if stale {
            let reason = if refresh_due {
                if held { "rate_limited" } else { "refresh_pending" }
            } else if state.fresh.is_none() {
                "outcome_unknown"
            } else if state.primary_failure.as_deref() == Some("rate_limited") {
                "rate_limited"
            } else {
                "read_failed"
            };
            result["cacheReason"] = json!(reason);
            let message = if refresh_due {
                if held { "Usage refresh is rate limited; showing the last successful reading." } else { "The next usage read is pending; showing the last successful reading." }
            } else if state.fresh.is_none() {
                "The previous usage read outcome was not recorded; showing the saved reading."
            } else if state.primary_failure.as_deref() == Some("rate_limited") {
                "Usage refresh is rate limited; showing the last successful reading."
            } else {
                "The usage read failed or was skipped; showing the last successful reading."
            };
            result["message"] = json!(message);
        } else {
            result.as_object_mut().map(|m| m.remove("cacheReason"));
            result["message"] = Value::Null;
        }
        if template.get("status").and_then(Value::as_str) == Some("needs_sign_in") {
            result["status"] = json!("needs_sign_in");
            result["message"] = template.get("message").cloned().unwrap_or(Value::Null);
            if stale {
                let windows: Vec<Value> = reading.windows.iter().map(|w| {
                    let mut w = w.clone();
                    w["status"] = json!("cached");
                    if w.get("sampledAt").is_none() || w.get("sampledAt") == Some(&Value::Null) {
                        if let Some(sampled) = &reading.sampled_at {
                            w["sampledAt"] = json!(sampled);
                        }
                    }
                    w
                }).collect();
                result["windows"] = Value::Array(windows);
            }
        }
        if template.get("provider").and_then(Value::as_str) != Some("claude") {
            result["platform"] = json!(helper_platform(&saved.host));
        }
        if saved.host != result.get("platform").and_then(Value::as_str).unwrap_or("") {
            result["host"] = json!(saved.host);
        } else {
            result.as_object_mut().map(|m| m.remove("host"));
        }
        self.with_sign_in(result, &state)
    }

    fn with_sign_in(&self, account: Value, state: &State) -> Value {
        let mut account = account;
        let message_for = |code: &str| -> Option<&'static str> {
            match code {
                "needs_sign_in" => Some("Sign in again on the configured source to refresh usage."),
                "no_browser_cookie" => Some("Connect a browser quota session for the configured account."),
                "choose_team" => Some("Choose a team in the configured browser quota session."),
                "inactive_subscription" => Some("The configured account subscription is inactive."),
                "rate_limited" => Some("Usage refresh is rate limited. The provider cooldown is being respected."),
                "identity_mismatch" | "account_mismatch" => Some("The configured source no longer matches this account."),
                "source_changed" => Some("The configured sign-in changed; usage will refresh shortly."),
                "identity_unavailable" => Some("The configured account identity could not be verified."),
                "plan_mismatch" => Some("The configured quota session no longer matches this subscription."),
                "team_mismatch" => Some("The configured quota session no longer matches this team."),
                _ => None,
            }
        };
        let code = state.primary_failure.clone();
        let requires_sign_in = code.as_deref().is_some_and(|c| ["needs_sign_in", "no_browser_cookie", "choose_team"].contains(&c));
        if let Some(code) = &code {
            if let Some(message) = message_for(code) {
                let status = account.get("status").and_then(Value::as_str).unwrap_or("");
                let provider = account.get("provider").and_then(Value::as_str).unwrap_or("");
                if status != "needs_sign_in" && (!has_usage_value(&account) || (requires_sign_in && provider != "claude")) {
                    account["status"] = json!(if requires_sign_in { "needs_sign_in" } else { "unavailable" });
                    account["message"] = json!(message);
                    if status == "cached" {
                        let sampled = account.get("sampledAt").cloned().unwrap_or(Value::Null);
                        let windows: Vec<Value> = account.get("windows").and_then(Value::as_array).cloned().unwrap_or_default().iter().map(|w| {
                            let mut w = w.clone();
                            w["status"] = json!("cached");
                            if w.get("sampledAt").is_none() || w.get("sampledAt") == Some(&Value::Null) {
                                if !sampled.is_null() {
                                    w["sampledAt"] = sampled.clone();
                                }
                            }
                            w
                        }).collect();
                        account["windows"] = Value::Array(windows);
                    }
                }
            }
        }
        if account.get("provider").and_then(Value::as_str) != Some("claude") {
            return account;
        }
        let mut sign_in_needed: Vec<String> = account.get("signInNeeded").and_then(Value::as_array).map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_default();
        for host in &state.primary_sign_in {
            if !sign_in_needed.contains(host) {
                sign_in_needed.push(host.clone());
            }
        }
        let reading = UsageReading {
            status: account.get("status").and_then(Value::as_str).unwrap_or("").to_owned(),
            email: None, plan: None, plan_class: None,
            sampled_at: account.get("sampledAt").and_then(Value::as_str).map(str::to_owned),
            fetched_at: account.get("fetchedAt").and_then(Value::as_str).map(str::to_owned),
            windows: account.get("windows").and_then(Value::as_array).cloned().unwrap_or_default(),
            optional_extras_context: None, optional_extras: None, antigravity_plan: None,
        };
        let projected = contract::project_claude_extras(&reading, self.now());
        account["windows"] = Value::Array(projected.windows);
        if !sign_in_needed.is_empty() {
            account["signInNeeded"] = json!(sign_in_needed);
        } else {
            account.as_object_mut().map(|m| m.remove("signInNeeded"));
        }
        account
    }
}
