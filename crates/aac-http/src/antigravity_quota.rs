//! G6: the Antigravity quota read path. The runtime composition's `collect` wrapper (identity
//! cooldown, request/response identity binding, throttle holds), `usage-service.ts`'s per-profile
//! cache and fallbacks, and the `cachedAccounts`/`invalidate` seam. The quota worker itself is an
//! input: the caller supplies `collectQuota(profileId)`, so a fixture can script every sample and
//! no native program or provider is reached here.
//!
//! Parity is against `tests/parity/g6/quota-cases.json` + `quota-oracle.mjs`, which drive the real
//! TypeScript runtime with the same fixtures and a fake collector.

use crate::antigravity_runtime::{
    LiveIdentity, ProfileRecord, RegistryState, js_date, map_profiles, read_credential,
    read_inventory, read_state,
};
use aac_views::antigravity::{
    display_text_value, email_value, plan_at, pool_window_value, public_profile, timestamp_word,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const TTL_MS: i64 = 120_000;
const FAILURE_BACKOFF_MS: i64 = 30_000;
const REFRESH_DEBOUNCE_MS: i64 = 5_000;
const DAY_MS: i64 = 86_400_000;
const MAX_WINDOWS: usize = 256;
const MAX_PROFILES: usize = 16;
const IDENTITY_UNBOUND_MESSAGE: &str =
    "Antigravity usage could not be matched to this saved account.";
const UNAVAILABLE_MESSAGE: &str = "Antigravity usage is temporarily unavailable.";
const SIGN_IN_MESSAGE: &str = "This Antigravity profile needs to sign in again.";
const CACHED_MESSAGE: &str = "Showing the last successful Antigravity usage reading.";
const LAST_SAVED_MESSAGE: &str =
    "Live Antigravity usage is temporarily unavailable; showing the last saved sample.";
const OLDER_SAMPLE_MESSAGE: &str = "The latest read returned an older sample; showing the last successful Antigravity usage reading.";

/// `usage-service.ts`'s runtime profile, the row `mapProfiles` produced.
#[derive(Clone, Debug)]
struct Profile {
    row: Value,
    id: String,
    email: String,
    identity_key: String,
    credential_revision: String,
    identity_verified: bool,
    available: bool,
    selected: bool,
}

impl Profile {
    fn from_row(row: &Value) -> Option<Self> {
        Some(Self {
            row: row.clone(),
            id: row.get("id")?.as_str()?.to_owned(),
            email: row.get("email")?.as_str()?.to_owned(),
            identity_key: row.get("identityKey")?.as_str()?.to_owned(),
            credential_revision: row.get("credentialRevision")?.as_str()?.to_owned(),
            identity_verified: row.get("identityVerified")?.as_bool()?,
            available: row.get("available")?.as_bool()?,
            selected: row.get("selected")?.as_bool()?,
        })
    }

    fn revision_key(&self) -> String {
        json!([self.identity_key, self.credential_revision]).to_string()
    }

    /// `publicProfile(profile)`.
    fn public_row(&self) -> Option<Value> {
        public_profile(&self.row)
    }
}

/// One `SavedQuotaSnapshot` from the worker.
#[derive(Clone, Debug, Default)]
struct Snapshot {
    profile_id: String,
    identity_key: String,
    credential_revision: String,
    email: Option<String>,
    plan: Option<String>,
    status: String,
    fetched_at: Option<String>,
    sampled_at: Option<String>,
    windows: Vec<Value>,
    retry_after_seconds: Option<f64>,
    reported_plan: Option<Value>,
    identity_verified: bool,
    identity_validation: String,
    source: String,
}

fn text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(str::to_owned)
}

impl Snapshot {
    pub fn from_value(value: &Value) -> Self {
        Self {
            profile_id: text(value.get("profileId")).unwrap_or_default(),
            identity_key: text(value.get("identityKey")).unwrap_or_default(),
            credential_revision: text(value.get("credentialRevision")).unwrap_or_default(),
            email: text(value.get("email")),
            plan: text(value.get("plan")),
            status: text(value.get("status")).unwrap_or_default(),
            fetched_at: text(value.get("fetchedAt")),
            sampled_at: text(value.get("sampledAt")),
            windows: value
                .get("windows")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default(),
            retry_after_seconds: value
                .get("retryAfterSeconds")
                .and_then(Value::as_f64)
                .filter(|n| n.is_finite()),
            reported_plan: value.get("reportedPlan").cloned(),
            identity_verified: value.get("identityVerified").and_then(Value::as_bool) == Some(true),
            identity_validation: text(value.get("identityValidation")).unwrap_or_default(),
            source: text(value.get("source")).unwrap_or_default(),
        }
    }

    fn integer_retry(&self) -> Option<i64> {
        self.retry_after_seconds
            .filter(|n| n.fract() == 0.0 && *n > 0.0)
            .map(|n| n as i64)
    }

    fn successful(&self) -> bool {
        matches!(self.status.as_str(), "ok" | "fresh" | "cached")
    }
}

/// `unavailableQuota(request)`.
fn unavailable(record: &ProfileRecord, status: &str, validation: &str) -> Value {
    json!({
        "profileId": record.id,
        "identityKey": record.identity_key,
        "credentialRevision": record.credential.fingerprint,
        "email": record.email,
        "plan": record.plan,
        "status": status,
        "fetchedAt": Value::Null,
        "sampledAt": Value::Null,
        "windows": [],
        "source": "native-consumer",
        "identityVerified": false,
        "identityValidation": validation,
    })
}

// ---- identity cooldowns (`usage-failover-cooldown.ts`) -------------------------------------------

/// The process's own cooldown records, beside the persisted file (`inProcessCooldowns`).
#[derive(Default)]
struct Cooldowns {
    entries: HashMap<String, i64>,
}

fn cooldown_key(ccs: &Path, identity_key: &str) -> String {
    let absolute = std::path::absolute(ccs).unwrap_or_else(|_| ccs.to_path_buf());
    let text = json!([
        path_word(&absolute),
        "antigravity",
        { "kind": "antigravity-subject", "key": identity_key }
    ])
    .to_string();
    crate::antigravity_runtime::sha256_hex(text.as_bytes())
}

fn path_word(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn cooldown_file(ccs: &Path, key: &str) -> PathBuf {
    ccs.join("account-usage-failover")
        .join(format!("cooldown-{key}.json"))
}

fn key_word(key: &str) -> bool {
    key.len() == 64
        && key
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

/// `readPersistedUsageCooldown`: a bounded, owner-held, version-1 record for this key only.
fn read_persisted(ccs: &Path, key: &str, now: i64) -> Option<i64> {
    let path = cooldown_file(ccs, key);
    let policy = aac_files::read::ReadPolicy::capped(1024)
        .leaf(aac_files::read::Leaf::Refuse)
        .owner(aac_files::read::Owner::Mine)
        .mode(aac_files::read::Mode::Private);
    let bytes = match aac_files::read::read(&path, &policy) {
        aac_files::read::Read::Bytes(bytes, _) => bytes,
        _ => return None,
    };
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let record = value.as_object()?;
    if record
        .keys()
        .any(|field| !["key", "until", "version", "claude429Streak"].contains(&field.as_str()))
        || record.get("version").and_then(Value::as_u64) != Some(1)
        || record.get("key").and_then(Value::as_str) != Some(key)
    {
        return None;
    }
    let until = record.get("until").and_then(Value::as_f64)?;
    if !until.is_finite() || until < 0.0 || until > (now + DAY_MS) as f64 {
        return None;
    }
    Some(until as i64)
}

impl Cooldowns {
    /// `readUsageCooldownState(...).until`, clamped to a live future.
    pub fn read(&mut self, ccs: &Path, identity_key: &str, now: i64) -> i64 {
        let key = cooldown_key(ccs, identity_key);
        if !key_word(&key) {
            return 0;
        }
        let memory = self.entries.get(&key).copied().unwrap_or(0);
        let saved = read_persisted(ccs, &key, now).unwrap_or(0);
        let until = memory.max(saved);
        if memory > 0 && memory <= now {
            self.entries.remove(&key);
        }
        if until > now { until } else { 0 }
    }

    /// `holdUsageCooldown`: keep the longer cooldown and persist it; a failed write keeps memory.
    pub fn hold(&mut self, ccs: &Path, identity_key: &str, requested_until: i64, now: i64) -> i64 {
        let key = cooldown_key(ccs, identity_key);
        if !key_word(&key) || requested_until <= 0 {
            return 0;
        }
        let previous = self.read(ccs, identity_key, now);
        let until = previous.max(now.max(requested_until.min(now + DAY_MS)));
        self.entries.insert(key.clone(), until);
        let path = cooldown_file(ccs, &key);
        let body = json!({ "version": 1, "key": key, "until": until }).to_string();
        if let Some(directory) = path.parent() {
            let _ = std::fs::create_dir_all(directory);
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(directory, std::fs::Permissions::from_mode(0o700));
            }
            let temporary = path.with_extension(format!(
                "{}.{}.tmp",
                path.extension().and_then(|e| e.to_str()).unwrap_or(""),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0)
            ));
            let written = std::fs::write(&temporary, body.as_bytes());
            #[cfg(unix)]
            if written.is_ok() {
                use std::os::unix::fs::PermissionsExt;
                let _ =
                    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600));
            }
            if written.is_ok() && std::fs::rename(&temporary, &path).is_ok() {
                self.entries.insert(key, until);
                return until;
            }
            let _ = std::fs::remove_file(&temporary);
        }
        until
    }
}

// ---- the runtime composition's `collect` ---------------------------------------------------------

/// `runtime-composition.ts` `collect(profileId)`: the worker snapshot bound to the immutable saved
/// credential, the identity cooldown and the throttle hold. `None` is the TS throw.
fn collect_quota(
    ccs: &Path,
    state: &RegistryState,
    profile_id: &str,
    cooldowns: &mut Cooldowns,
    worker: &mut dyn FnMut(&str) -> Option<Value>,
    now: i64,
) -> Option<Value> {
    let record = state.profile(profile_id)?;
    let saved: LiveIdentity = read_credential(ccs, record)?;
    if saved.identity_key() != record.identity_key {
        return None;
    }
    let until = cooldowns.read(ccs, &record.identity_key, now);
    if until > now {
        let mut limited = unavailable(record, "rate_limited", "unavailable");
        limited["retryAfterSeconds"] = json!(ceil_div(until - now, 1000));
        return Some(limited);
    }
    let snapshot = Snapshot::from_value(&worker(profile_id)?);
    let sample_email = snapshot
        .email
        .as_deref()
        .and_then(|value| email_value(Some(&json!(value))));
    let matches = snapshot.profile_id == record.id
        && snapshot.identity_key == record.identity_key
        && snapshot.credential_revision == record.credential.fingerprint
        && snapshot.source == "native-consumer"
        && snapshot.email.as_deref().is_none_or(|email| {
            sample_email.as_deref().map(str::to_lowercase) == Some(email.to_lowercase())
        });
    let fresh_throttled = snapshot.status == "fresh" && snapshot.integer_retry().is_some();
    if matches
        && (snapshot.status == "rate_limited" || fresh_throttled)
        && snapshot.identity_validation != "mismatch"
        && snapshot.identity_validation != "needs_sign_in"
    {
        let seconds = snapshot.integer_retry().unwrap_or(60).clamp(60, 86_400);
        cooldowns.hold(ccs, &record.identity_key, now + seconds * 1000, now);
    }
    let current = read_credential(ccs, record);
    let stale = current
        .as_ref()
        .is_none_or(|identity| identity.identity_key() != record.identity_key);
    if stale || !matches {
        let mut out = unavailable(record, "needs_sign_in", "mismatch");
        out["status"] = json!("needs_sign_in");
        return Some(out);
    }
    if snapshot.identity_validation == "mismatch" || snapshot.identity_validation == "needs_sign_in"
    {
        let mut out = unavailable(record, "needs_sign_in", &snapshot.identity_validation);
        out["status"] = json!("needs_sign_in");
        out["identityValidation"] = json!(snapshot.identity_validation);
        return Some(out);
    }
    if (snapshot.status == "fresh" || snapshot.status == "ok")
        && !(snapshot.identity_verified && snapshot.identity_validation == "verified")
    {
        return Some(unavailable(record, "unavailable", "unavailable"));
    }
    Some(worker_value(&snapshot))
}

fn ceil_div(value: i64, by: i64) -> i64 {
    (value + by - 1) / by
}

/// The worker's snapshot back as JSON, with every field the usage service reads.
fn worker_value(snapshot: &Snapshot) -> Value {
    let mut out = json!({
        "profileId": snapshot.profile_id,
        "identityKey": snapshot.identity_key,
        "credentialRevision": snapshot.credential_revision,
        "email": snapshot.email,
        "plan": snapshot.plan,
        "status": snapshot.status,
        "fetchedAt": snapshot.fetched_at,
        "sampledAt": snapshot.sampled_at,
        "windows": snapshot.windows,
        "source": snapshot.source,
        "identityVerified": snapshot.identity_verified,
        "identityValidation": snapshot.identity_validation,
    });
    if let Some(retry) = snapshot.retry_after_seconds {
        out["retryAfterSeconds"] = json!(retry);
    }
    if let Some(reported) = &snapshot.reported_plan {
        out["reportedPlan"] = reported.clone();
    }
    out
}

// ---- `usage-service.ts` --------------------------------------------------------------------------

#[derive(Clone)]
struct Cache {
    revision: String,
    identity_key: String,
    result: Option<Value>,
    last_good: Option<Value>,
    checked_at: i64,
    refreshed_at: i64,
    retry_at: i64,
    failed: bool,
}

impl Cache {
    fn fresh(revision: String, identity_key: String, last_good: Option<Value>) -> Self {
        Self {
            revision,
            identity_key,
            result: None,
            last_good,
            checked_at: i64::MIN / 4,
            refreshed_at: i64::MIN / 4,
            retry_at: i64::MIN / 4,
            failed: false,
        }
    }
}

fn plan_fields(
    plan: Option<&str>,
    sampled: Option<&Value>,
    reported: Option<&Value>,
    clock: i64,
) -> (Value, Option<Value>) {
    plan_at(plan, sampled, reported, clock as f64)
}

/// `fallback(profile)`.
fn fallback(profile: &Profile, clock: i64) -> Value {
    let Some(public) = profile.public_row() else {
        return json!({});
    };
    let email = public["email"].clone();
    let available = public["available"] == json!(true);
    let verified_at = public["verifiedAt"].clone();
    let plan = public["plan"].as_str();
    let (display_plan, plan_detail) = plan_fields(
        plan,
        Some(&verified_at).filter(|value| !value.is_null()),
        None,
        clock,
    );
    let mut out = json!({
        "id": format!("antigravity:profile:{}", profile.id),
        "provider": "antigravity",
        "providerLabel": "Antigravity",
        "label": email,
        "email": (profile.public_row().map(|row| row["email"].clone())).unwrap_or(Value::Null),
        "plan": display_plan,
        "platform": "ubuntu",
        "source": "Antigravity saved login on Ubuntu",
        "status": if available { "unavailable" } else { "needs_sign_in" },
        "message": if available { UNAVAILABLE_MESSAGE } else { "This Antigravity profile needs a verified sign-in." },
        "fetchedAt": Value::Null,
        "sampledAt": Value::Null,
        "isActive": profile.selected,
        "windows": [],
        "capabilities": {
            "codexProfile": Value::Null,
            "claudeProfileId": Value::Null,
            "claudePlatforms": [],
            "antigravityProfileId": profile.id,
            "antigravityHostIds": if available { json!(["ubuntu"]) } else { json!([]) },
        },
    });
    if let Some(detail) = plan_detail {
        out["antigravityPlan"] = detail;
    }
    out
}

/// The usage answer with one profile's sample applied to its fallback.
fn apply_sample(profile: &Profile, sample: Option<&Value>, cache: &mut Cache, now: i64) -> Value {
    let mut result = fallback(profile, now);
    let mut invalidate_last_good = false;
    let mut retry_after = FAILURE_BACKOFF_MS;
    let mut reported_backoff = false;
    let Some(sample) = sample else {
        // `collectQuota` threw: the TS catch keeps the fallback and lets the cache carry lastGood.
        result["status"] = json!("error");
        result["message"] = json!(UNAVAILABLE_MESSAGE);
        return settle_cache(
            profile,
            result,
            cache,
            now,
            retry_after,
            reported_backoff,
            invalidate_last_good,
        );
    };
    let snapshot = Snapshot::from_value(sample);
    let sample_email = snapshot
        .email
        .as_deref()
        .and_then(|value| email_value(Some(&json!(value))));
    let unbound = snapshot.profile_id != profile.id
        || snapshot.identity_key != profile.identity_key
        || snapshot.credential_revision != profile.credential_revision
        || (snapshot.email.is_some()
            && sample_email.as_deref().map(str::to_lowercase)
                != Some(profile.email.to_lowercase()))
        || (snapshot.successful() && sample_email.is_none());
    if unbound {
        invalidate_last_good = true;
        result["status"] = json!("error");
        result["statusReason"] = json!("identity_unbound");
        result["message"] = json!(IDENTITY_UNBOUND_MESSAGE);
    } else {
        let plan_text = display_text_value(Some(&json!(snapshot.plan)), 80)
            .or_else(|| result["plan"].as_str().map(str::to_owned));
        if let Some(seconds) = snapshot.integer_retry() {
            retry_after = seconds.min(86_400) * 1000;
            reported_backoff = true;
        }
        let status = match snapshot.status.as_str() {
            "fresh" | "ok" => "ok",
            "cached" | "needs_sign_in" | "unavailable" => snapshot.status.as_str(),
            _ => "error",
        };
        result["status"] = json!(status);
        result["fetchedAt"] = timestamp_value(snapshot.fetched_at.as_deref());
        result["sampledAt"] = timestamp_value(snapshot.sampled_at.as_deref());
        let sampled = result["sampledAt"].clone();
        let fetched = result["fetchedAt"].clone();
        let observed = if sampled.is_null() { fetched } else { sampled };
        let (display_plan, plan_detail) = plan_fields(
            plan_text.as_deref(),
            Some(&observed).filter(|value| !value.is_null()),
            snapshot.reported_plan.as_ref(),
            now,
        );
        result["plan"] = display_plan;
        result
            .as_object_mut()
            .map(|object| object.remove("antigravityPlan"));
        if let Some(detail) = plan_detail {
            result["antigravityPlan"] = detail;
        }
        let sampled_ms = result["sampledAt"].as_str().and_then(js_date);
        let fresh_enough = sampled_ms.is_some_and(|at| at <= now + 30_000);
        if matches!(status, "ok" | "cached") && sampled_ms.is_some() {
            if fresh_enough {
                let windows: Vec<Value> = snapshot
                    .windows
                    .iter()
                    .take(MAX_WINDOWS)
                    .filter_map(pool_window_value)
                    .collect();
                let policy = result["antigravityPlan"]["quotaPolicy"].as_str();
                if windows.is_empty() && policy != Some("none") {
                    result["status"] = json!("error");
                }
                result["windows"] = Value::Array(windows);
            } else {
                result["status"] = json!("error");
            }
        } else if matches!(status, "ok" | "cached") {
            result["status"] = json!("error");
        }
        if result["status"] == json!("needs_sign_in") {
            invalidate_last_good = true;
        }
        result["message"] = json!(match result["status"].as_str().unwrap_or("error") {
            "ok" => Value::Null,
            "cached" => json!(CACHED_MESSAGE),
            "needs_sign_in" => json!(SIGN_IN_MESSAGE),
            _ => json!(UNAVAILABLE_MESSAGE),
        });
    }
    settle_cache(
        profile,
        result,
        cache,
        now,
        retry_after,
        reported_backoff,
        invalidate_last_good,
    )
}

/// The cache update every collect path shares: last-good carry, retry timing and the failed flag.
fn settle_cache(
    profile: &Profile,
    result: Value,
    cache: &mut Cache,
    now: i64,
    retry_after: i64,
    reported_backoff: bool,
    invalidate_last_good: bool,
) -> Value {
    if invalidate_last_good {
        cache.last_good = None;
    }
    let valid = matches!(result["status"].as_str(), Some("ok" | "cached"));
    if valid {
        let older = match (
            cache
                .last_good
                .as_ref()
                .and_then(|good| good["sampledAt"].as_str())
                .and_then(js_date),
            result["sampledAt"].as_str().and_then(js_date),
        ) {
            (Some(good), Some(this)) => this < good,
            _ => false,
        };
        if older {
            let mut carried = cache.last_good.clone().unwrap_or(Value::Null);
            carried["isActive"] = json!(profile.selected);
            carried["status"] = json!("cached");
            carried["cacheReason"] = json!("read_failed");
            carried["message"] = json!(OLDER_SAMPLE_MESSAGE);
            cache.result = Some(carried);
        } else {
            cache.result = Some(result);
        }
        cache.last_good = cache.result.clone();
        cache.retry_at = if cache
            .result
            .as_ref()
            .is_some_and(|r| r["status"] == json!("cached"))
            && reported_backoff
        {
            now + retry_after
        } else {
            now
        };
    } else {
        cache.result = match cache.last_good.clone() {
            Some(mut good) => {
                good["isActive"] = json!(profile.selected);
                good["status"] = json!("cached");
                good["cacheReason"] = json!(if reported_backoff {
                    "rate_limited"
                } else {
                    "read_failed"
                });
                good["message"] = json!(LAST_SAVED_MESSAGE);
                Some(good)
            }
            None => Some(result),
        };
        cache.retry_at = now + retry_after;
    }
    cache.failed = !valid;
    cache.checked_at = now;
    cache.result.clone().unwrap_or(Value::Null)
}

fn timestamp_value(word: Option<&str>) -> Value {
    word.and_then(|text| timestamp_word(Some(&json!(text))))
        .map_or(Value::Null, Value::String)
}

/// The per-profile cache seam (`getProfile`).
#[derive(Default)]
struct QuotaService {
    caches: HashMap<String, Cache>,
}

/// One read's fixed inputs: the scope, its registry state, the live proof and the clock.
struct Read<'a> {
    ccs: &'a Path,
    state: &'a RegistryState,
    live: Option<&'a LiveIdentity>,
    now: i64,
}

impl QuotaService {
    /// `getAccounts({refresh})` for the registry's profiles. `worker` receives the profile id and
    /// returns the worker snapshot (or `None`, the TS throw). The second registry read rechecks
    /// identity and revision before a sample is published, exactly as TS does.
    fn get_accounts(
        &mut self,
        read: &Read<'_>,
        worker: &mut dyn FnMut(&str) -> Option<Value>,
        cooldowns: &mut Cooldowns,
        refresh: bool,
    ) -> Vec<Value> {
        let first = self.profiles(read);
        let accounts: Vec<Value> = first
            .iter()
            .map(|profile| self.get_profile(read, profile, worker, cooldowns, refresh))
            .collect();
        let current = self.profiles(read);
        current
            .iter()
            .map(|profile| {
                let previous = first.iter().find(|row| row.id == profile.id);
                let sampled = accounts.iter().find(|account| {
                    account["capabilities"]["antigravityProfileId"].as_str()
                        == Some(profile.id.as_str())
                });
                if previous.is_none()
                    || sampled.is_none()
                    || previous.is_some_and(|row| {
                        row.identity_key != profile.identity_key
                            || row.credential_revision != profile.credential_revision
                    })
                    || !profile.identity_verified
                    || !profile.available
                {
                    self.caches.remove(&profile.id);
                    let mut row = fallback(profile, read.now);
                    row["isActive"] = json!(profile.selected);
                    return row;
                }
                let mut row = sampled.cloned().unwrap_or(Value::Null);
                row["isActive"] = json!(profile.selected);
                row
            })
            .collect()
    }

    /// `cachedAccounts(profiles)`: bounded, synchronous, never starts a read.
    #[allow(
        dead_code,
        reason = "the synchronous tray seam; wired with the runtime cache"
    )]
    fn cached_accounts(&self, read: &Read<'_>) -> Vec<Value> {
        let now = read.now;
        self.profiles(read)
            .iter()
            .map(|profile| {
                if !profile.identity_verified || !profile.available {
                    return fallback(profile, now);
                }
                let cache = self.caches.get(&profile.id);
                let revision = profile.revision_key();
                if let Some(cache) = cache {
                    if cache.revision == revision
                        && let Some(result) = &cache.result
                    {
                        let mut row = result.clone();
                        row["isActive"] = json!(profile.selected);
                        return row;
                    }
                    if cache.identity_key == profile.identity_key
                        && let Some(carried) =
                            cache.result.clone().or_else(|| cache.last_good.clone())
                    {
                        let mut row = carried;
                        row["isActive"] = json!(profile.selected);
                        row["status"] = json!("cached");
                        row["cacheReason"] = json!("source_changed");
                        row["message"] =
                            json!("Showing the last saved sample while the new sign-in is read.");
                        return row;
                    }
                }
                fallback(profile, now)
            })
            .collect()
    }

    #[allow(
        dead_code,
        reason = "the TS `invalidate()` seam; wired with the runtime cache"
    )]
    fn invalidate(&mut self) {
        self.caches.clear();
    }

    /// `profiles()`: the mapped rows, parsed and admitted (16 at most, unique ids and identities).
    fn profiles(&self, read: &Read<'_>) -> Vec<Profile> {
        let overlay = read_inventory(read.state, read.live, read.now);
        let rows = map_profiles(read.ccs, read.state, &overlay);
        if rows.len() > MAX_PROFILES {
            return Vec::new();
        }
        let mut ids = std::collections::HashSet::new();
        let mut identities = std::collections::HashSet::new();
        let mut selected = 0;
        let mut profiles = Vec::new();
        for row in rows {
            let Some(profile) = Profile::from_row(&row) else {
                return Vec::new();
            };
            if profile.id.len() > 64
                || !ids.insert(profile.id.clone())
                || (profile.identity_verified && !identities.insert(profile.identity_key.clone()))
            {
                return Vec::new();
            }
            if profile.identity_verified && profile.selected {
                selected += 1;
            }
            profiles.push(profile);
        }
        if selected > 1 {
            return Vec::new();
        }
        profiles
    }

    fn get_profile(
        &mut self,
        read: &Read<'_>,
        profile: &Profile,
        worker: &mut dyn FnMut(&str) -> Option<Value>,
        cooldowns: &mut Cooldowns,
        refresh: bool,
    ) -> Value {
        let now = read.now;
        if !profile.identity_verified || !profile.available {
            self.caches.remove(&profile.id);
            let mut row = fallback(profile, now);
            row["isActive"] = json!(profile.selected);
            return row;
        }
        let revision = profile.revision_key();
        if self
            .caches
            .get(&profile.id)
            .is_none_or(|cache| cache.revision != revision)
        {
            let carried = self
                .caches
                .get(&profile.id)
                .filter(|cache| cache.identity_key == profile.identity_key)
                .and_then(|cache| cache.last_good.clone());
            self.caches.insert(
                profile.id.clone(),
                Cache::fresh(revision, profile.identity_key.clone(), carried),
            );
        }
        let cache = self
            .caches
            .get_mut(&profile.id)
            .expect("cache just ensured");
        let forced = refresh && now - cache.refreshed_at >= REFRESH_DEBOUNCE_MS;
        if forced {
            cache.refreshed_at = now;
        }
        let backoff = if cache.failed {
            FAILURE_BACKOFF_MS
        } else {
            TTL_MS
        };
        let expired = now - cache.checked_at >= backoff;
        let should_collect = now >= cache.retry_at && (forced || cache.result.is_none() || expired);
        let result = if should_collect {
            let collected =
                collect_quota(read.ccs, read.state, &profile.id, cooldowns, worker, now);
            apply_sample(profile, collected.as_ref(), cache, now)
        } else {
            cache
                .result
                .clone()
                .unwrap_or_else(|| fallback(profile, now))
        };
        let mut row = result;
        row["isActive"] = json!(profile.selected);
        row
    }
}

/// The route's `/quotas` answer: the runtime's own accounts through the already-ported
/// `publicQuotas` (`public-reads.ts`).
fn public_quotas(accounts: &[Value], now: i64, clock: i64) -> Option<Value> {
    let envelope = aac_status::read_context::Envelope {
        data: json!({ "accounts": accounts, "now": now as f64, "clock": clock as f64 }),
        presentation: aac_status::read_context::Presentation::empty(),
    };
    aac_views::antigravity::quotas(envelope).ok()
}

/// The wall clock for a route answer.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// The process's one Antigravity quota state (TS keeps the same caches module-wide).
fn shared() -> &'static std::sync::Mutex<(QuotaService, Cooldowns)> {
    static SHARED: std::sync::OnceLock<std::sync::Mutex<(QuotaService, Cooldowns)>> =
        std::sync::OnceLock::new();
    SHARED.get_or_init(|| std::sync::Mutex::new((QuotaService::default(), Cooldowns::default())))
}

/// `GET /api/antigravity/profiles/quotas` for a caller that sends no TS input: the registry's
/// profiles and the injected quota worker. `worker` returns one snapshot per profile id (`None`
/// is the worker's safe failure). No live proof is wired yet, so `isActive` is false.
pub fn quotas_now(
    ccs: &Path,
    live: Option<&LiveIdentity>,
    refresh: bool,
    worker: &mut dyn FnMut(&str) -> Option<Value>,
) -> Option<Value> {
    let now = now_ms();
    let state = read_state(ccs)?;
    let mut guard = shared().lock().ok()?;
    let (service, cooldowns) = &mut *guard;
    let read = Read {
        ccs,
        state: &state,
        live,
        now,
    };
    let accounts = service.get_accounts(&read, worker, cooldowns, refresh);
    public_quotas(&accounts, now, now)
}

// ---- the private quota worker (`quota-worker-transport.ts`) -------------------------------------

const MAX_WORKER_BYTES: usize = 65_536;
const MAX_CREDENTIAL_BYTES: usize = 16_384;
const WORKER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(25);

/// The one private helper both credential validation and quota collection use: fixed trusted
/// absolute paths, one JSON object on stdin, one bounded JSON object on stdout. Credentials travel
/// on stdin only; stderr is discarded so it can never become a channel.
pub struct Worker {
    python: PathBuf,
    script: PathBuf,
    collectors: PathBuf,
}

/// One `SavedQuotaRequest`: the immutable saved credential and the identity it was verified as.
pub struct QuotaRequest<'a> {
    /// The saved profile id the answer must name.
    pub profile_id: &'a str,
    /// The saved account email, compared lower-case.
    pub email: &'a str,
    /// The profile's identity key.
    pub identity_key: &'a str,
    /// The credential fingerprint the bytes must hash to.
    pub credential_revision: &'a str,
    /// The verified identity the credential represents.
    pub identity: &'a LiveIdentity,
    /// The saved credential's format (sent to no one).
    pub credential_format: &'a str,
    /// The saved credential bytes; they travel on stdin only.
    pub credential_bytes: &'a [u8],
}

impl Worker {
    /// `createAntigravityQuotaWorker`: both package paths must be absolute.
    pub fn new(python: &Path, script: &Path, collectors: &Path) -> Option<Self> {
        (python.is_absolute() && script.is_absolute() && collectors.is_absolute()).then(|| Self {
            python: python.to_path_buf(),
            script: script.to_path_buf(),
            collectors: collectors.to_path_buf(),
        })
    }

    fn invoke(&self, packet: &Value) -> Option<Value> {
        let request = serde_json::to_vec(packet).ok()?;
        if request.len() > MAX_WORKER_BYTES {
            return None;
        }
        use std::io::{Read, Write};
        use std::process::{Command, Stdio};
        let mut child = Command::new(&self.python)
            .arg("-I")
            .arg(&self.script)
            .arg("--collector-dir")
            .arg(&self.collectors)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        if let Some(mut stdin) = child.stdin.take()
            && (stdin.write_all(&request).is_err() || stdin.flush().is_err())
        {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        let mut stdout = child.stdout.take()?;
        let reader = std::thread::spawn(move || {
            let mut body = Vec::new();
            let mut chunk = [0u8; 8192];
            loop {
                match stdout.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        body.extend_from_slice(&chunk[..n]);
                        if body.len() > MAX_WORKER_BYTES + 1 {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
            body
        });
        let deadline = std::time::Instant::now() + WORKER_TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let body = reader.join().ok()?;
        let status = status?;
        if !status.success() || body.len() > MAX_WORKER_BYTES {
            return None;
        }
        let value: Value = serde_json::from_slice(&body).ok()?;
        value.as_object()?;
        Some(value)
    }

    /// `collectQuota(request)`: the saved credential bound to its revision, then one worker call.
    pub fn collect_quota(&self, request: &QuotaRequest<'_>) -> Option<Value> {
        let bytes = request.credential_bytes;
        if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_BYTES {
            return None;
        }
        if !profile_id_word(request.profile_id)
            || !crate::antigravity_runtime::hash_word(request.identity_key)
            || !crate::antigravity_runtime::hash_word(request.credential_revision)
            || request.email.to_lowercase() != request.identity.email
            || request.identity.identity_key() != request.identity_key
            || crate::antigravity_runtime::sha256_hex(bytes) != request.credential_revision
        {
            return None;
        }
        let _ = request.credential_format;
        self.invoke(&json!({
            "operation": "quota",
            "profileId": request.profile_id,
            "email": request.email,
            "identityKey": request.identity_key,
            "credentialRevision": request.credential_revision,
            "credentialBase64": encode_base64(bytes),
        }))
    }

    /// `validateCredential(credential)`: the worker's identity, checked and bound to the bytes.
    pub fn validate_credential(&self, bytes: &[u8]) -> Option<LiveIdentity> {
        if bytes.is_empty() || bytes.len() > MAX_CREDENTIAL_BYTES {
            return None;
        }
        let result = self.invoke(&json!({
            "operation": "identity",
            "credentialBase64": encode_base64(bytes),
        }))?;
        let identity = LiveIdentity::from_value(&json!({
            "email": result.get("email"),
            "subject": result.get("subject"),
            "plan": result.get("plan"),
            "verifiedAt": result.get("verifiedAt"),
            "source": result.get("source"),
        }))?;
        if result.get("credentialRevision").and_then(Value::as_str)
            != Some(crate::antigravity_runtime::sha256_hex(bytes).as_str())
            || result.get("identityKey").and_then(Value::as_str)
                != Some(identity.identity_key().as_str())
        {
            return None;
        }
        Some(identity)
    }
}

fn encode_base64(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn profile_id_word(id: &str) -> bool {
    let bytes = id.as_bytes();
    matches!(bytes.first(), Some(c) if c.is_ascii_alphanumeric())
        && id.len() <= 64
        && bytes
            .iter()
            .all(|c| c.is_ascii_alphanumeric() || *c == b'_' || *c == b'-')
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use base64::Engine as _;
    use serde_json::Value;

    const CASES: &str = include_str!("../../../tests/parity/g6/quota-cases.json");
    const EXPECTED: &str = include_str!("../../../tests/parity/g6/quota-expected.json");

    fn private(path: &Path, directory: bool) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = if directory { 0o700 } else { 0o600 };
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
        }
        let _ = (path, directory);
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
            private(parent, true);
        }
        std::fs::write(path, bytes).unwrap();
        private(path, false);
    }

    /// The registry fixture as `quota-cases.json` describes it.
    fn materialize(registry: &Value) -> (tempfile::TempDir, std::path::PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let ccs = temp.path().to_path_buf();
        private(&ccs, true);
        let mut records = Vec::new();
        for profile in registry["profiles"].as_array().unwrap() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(profile["credential"]["bytes"].as_str().unwrap())
                .unwrap();
            let fingerprint = crate::antigravity_runtime::sha256_hex(&bytes);
            write(
                &ccs.join("antigravity-instances")
                    .join(profile["id"].as_str().unwrap())
                    .join("ubuntu")
                    .join(format!("credential-{fingerprint}.bin")),
                &bytes,
            );
            let identity = profile["identity"].clone();
            records.push(json!({
                "id": profile["id"],
                "email": profile["email"],
                "identityKey": LiveIdentity::from_value(&identity).unwrap().identity_key(),
                "plan": profile["plan"],
                "credential": {
                    "fingerprint": fingerprint,
                    "format": profile["credential"]["format"],
                    "identity": identity,
                    "savedAt": profile["identity"]["verifiedAt"],
                },
            }));
        }
        write(
            &ccs.join("antigravity-profiles")
                .join("registry-000000000001.json"),
            json!({
                "version": 1,
                "revision": 1,
                "profiles": records,
                "active": registry["active"],
                "transaction": null,
            })
            .to_string()
            .as_bytes(),
        );
        (temp, ccs)
    }

    /// The worker snapshot the oracle's `snapshotFor` produced for one step and profile.
    fn snapshot(state: &RegistryState, profile_id: &str, sample: &Value, at: &str) -> Value {
        let record = state.profile(profile_id).unwrap();
        let identity_key = match sample.get("identityKey").and_then(Value::as_str) {
            Some("other") => "f".repeat(64),
            _ => record.identity_key.clone(),
        };
        let mut out = json!({
            "profileId": profile_id,
            "identityKey": identity_key,
            "credentialRevision": record.credential.fingerprint,
            "email": record.email,
            "plan": sample.get("plan").and_then(Value::as_str),
            "status": sample["status"],
            "fetchedAt": sample.get("sampledAt").and_then(Value::as_str).unwrap_or(at),
            "sampledAt": sample.get("sampledAt").and_then(Value::as_str).unwrap_or(at),
            "windows": sample.get("windows").cloned().unwrap_or_else(|| json!([])),
            "source": sample["source"],
            "identityVerified": sample["identityVerified"],
            "identityValidation": sample["identityValidation"],
        });
        if let Some(retry) = sample.get("retryAfterSeconds") {
            out["retryAfterSeconds"] = retry.clone();
        }
        if let Some(reported) = sample.get("reportedPlan") {
            out["reportedPlan"] = reported.clone();
        }
        out
    }

    #[test]
    fn the_accounts_and_public_quotas_match_the_typescript_oracle_every_step() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        let expected: Value = serde_json::from_str(EXPECTED).unwrap();
        let clock = expected["clock"].as_i64().unwrap();
        let expected = expected["cases"].as_array().unwrap();
        assert_eq!(expected.len(), cases["cases"].as_array().unwrap().len());
        for (case, oracle) in cases["cases"].as_array().unwrap().iter().zip(expected) {
            let registry = &cases["registry"];
            let (_temp, ccs) = materialize(registry);
            let live = registry
                .get("live")
                .filter(|value| !value.is_null())
                .and_then(LiveIdentity::from_value);
            let state = read_state(&ccs).unwrap();
            let mut service = QuotaService::default();
            let mut cooldowns = Cooldowns::default();
            let steps = case["steps"].as_array().unwrap();
            assert_eq!(steps.len(), oracle["steps"].as_array().unwrap().len());
            for (step, want) in steps.iter().zip(oracle["steps"].as_array().unwrap()) {
                let now = js_date(step["at"].as_str().unwrap()).unwrap();
                let samples = step["samples"].as_object().unwrap().clone();
                let mut worker = |profile_id: &str| {
                    samples.get(profile_id).map(|sample| {
                        snapshot(&state, profile_id, sample, step["at"].as_str().unwrap())
                    })
                };
                let read = Read {
                    ccs: &ccs,
                    state: &state,
                    live: live.as_ref(),
                    now,
                };
                let accounts = service.get_accounts(
                    &read,
                    &mut worker,
                    &mut cooldowns,
                    step["refresh"].as_bool().unwrap_or(false),
                );
                assert_eq!(
                    &json!(accounts),
                    &want["accounts"],
                    "{} {}",
                    case["name"],
                    step["at"]
                );
                assert_eq!(
                    &public_quotas(&accounts, now, clock).unwrap(),
                    &want["quotas"],
                    "{} quotas {}",
                    case["name"],
                    step["at"]
                );
            }
        }
    }

    /// An absolute interpreter path: the transport refuses a relative executable, like TS.
    fn absolute_python() -> Option<std::path::PathBuf> {
        if let Some(path) = std::env::var_os("AAC_TEST_PYTHON") {
            return Some(std::path::PathBuf::from(path));
        }
        let paths = std::env::var_os("PATH")?;
        for directory in std::env::split_paths(&paths) {
            for name in ["python3", "python"] {
                for candidate in [directory.join(name), directory.join(format!("{name}.exe"))] {
                    if candidate.is_file() {
                        return Some(candidate);
                    }
                }
            }
        }
        None
    }

    #[test]
    fn the_worker_protocol_is_bounded_and_binds_the_credential() {
        // A fixture worker: one JSON object in, one out. The identity operation computes the
        // fingerprint and identity key the way the TS worker does, so a success is provable.
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("worker.py");
        write(
            &script,
            br#"import hashlib,json,os,sys
request = json.load(sys.stdin)
credential = __import__("base64").b64decode(request["credentialBase64"])
fingerprint = hashlib.sha256(credential).hexdigest()
if request.get("operation") == "identity":
    identity = {"email":"fixture@example.com","subject":"fixture-subject","plan":None,
                "verifiedAt":"2026-10-01T17:00:00.000Z","source":"native-runtime"}
    key = hashlib.sha256(json.dumps(["antigravity", identity["email"], identity["subject"]],
                                    separators=(",", ":")).encode()).hexdigest()
    print(json.dumps({**identity, "credentialRevision": fingerprint,
                      "identityKey": key,
                      "source": identity["source"]}))
else:
    print(json.dumps({"status":"fresh","windows":[]}))
"#,
        );
        let Some(python) = absolute_python() else {
            return;
        };
        let collectors = temp.path().join("collectors");
        std::fs::create_dir_all(&collectors).unwrap();
        let worker = Worker::new(&python, &script, &collectors).unwrap();
        let bytes = b"fixture private credential bytes";
        // The identity comes back bound to the bytes it was given, and normalizes the email.
        let identity = worker.validate_credential(bytes).unwrap();
        assert_eq!(identity.email, "fixture@example.com");
        assert_eq!(identity.subject, "fixture-subject");
        assert_eq!(identity.source, "native-runtime");
        assert_eq!(identity.verified_at, "2026-10-01T17:00:00.000Z");
        // A guessed credential revision never seats a request.
        assert!(worker.validate_credential(b"").is_none());
        let key = identity.identity_key();
        let revision = crate::antigravity_runtime::sha256_hex(bytes);
        let request = QuotaRequest {
            profile_id: "gmail",
            email: "fixture@example.com",
            identity_key: &key,
            credential_revision: &revision,
            identity: &identity,
            credential_format: "fake-native-json",
            credential_bytes: bytes,
        };
        assert!(worker.collect_quota(&request).is_some());
        let wrong = "b".repeat(64);
        let request = QuotaRequest {
            credential_revision: &wrong,
            ..request
        };
        assert!(worker.collect_quota(&request).is_none());
        // The executable is fixed by absolute paths only.
        assert!(Worker::new(Path::new("python"), &script, &collectors).is_none());
    }
}
