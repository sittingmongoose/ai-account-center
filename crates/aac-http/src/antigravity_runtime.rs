//! G6: the Antigravity runtime bridge's own reads. Rust reads the saved-profile registry and the
//! live-identity overlay itself, so `runtime-service.ts`, the `switch-service.ts` inventory and
//! `runtime-composition.ts`'s profile mapping can leave the service. Ported from:
//!
//! - `registry.ts` `listProfiles` / `readCredential` / `readState` (read-only: TS stays the writer);
//! - `switch-service.ts` `readInventory` (the live proof, its 5-minute window and the
//!   selected/active overlay);
//! - `runtime-composition.ts` `mapProfiles` (identityVerified, credentialRevision, runtimeVerified);
//! - `usage-service.ts` `profiles` admission and `usage-normalization.ts` `publicProfile`
//!   (the public inventory members, `hostId: "ubuntu"`).
//!
//! No native program runs here and nothing writes: the live identity and the quota worker are
//! inputs. A registry that cannot be read safely declines, so TS answers until the cut.

use aac_files::read::{Ancestors, Leaf, Mode, Owner, Read, ReadPolicy};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// `registry.ts` caps, unchanged.
const MAX_REGISTRY_BYTES: u64 = 1024 * 1024;
const MAX_NATIVE_BYTES: u64 = 1024 * 1024;
const MAX_PROFILES: usize = 64;
/// `usage-service.ts` `MAX_PROFILES` refuses a wider public inventory.
const MAX_PUBLIC_PROFILES: usize = 16;
/// `switch-service.ts` `IDENTITY_MAX_AGE`: a proof older than this is stale.
pub const IDENTITY_MAX_AGE_MS: i64 = 5 * 60_000;
/// The same proof may be this far ahead of a clock before it reads as not yet valid.
pub const IDENTITY_FUTURE_TOLERANCE_MS: i64 = 60_000;
/// `runtime-service.ts` `ANTIGRAVITY_RUNTIME_RETRY_MS`: an empty scope is asked again this often.
pub const RUNTIME_RETRY_MS: i64 = 15_000;

pub(crate) fn profile_id_ok(id: &str) -> bool {
    let bytes = id.as_bytes();
    matches!(bytes.first(), Some(c) if c.is_ascii_lowercase())
        && id.len() <= 48
        && bytes
            .iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'_' || *c == b'-')
}

pub(crate) fn hash_word(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn format_ok(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=64).contains(&bytes.len())
        && matches!(bytes.first(), Some(c) if c.is_ascii_lowercase())
        && bytes.iter().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'_' | b'.' | b'-')
        })
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// `canonicalEmail`: at most 254 characters, one `@`, a dot in the host, no whitespace or control.
fn canonical_email(value: &str) -> Option<String> {
    let (local, host) = value.split_once('@')?;
    if value.len() > 254
        || local.is_empty()
        || host.is_empty()
        || !host.contains('.')
        || value.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return None;
    }
    Some(value.to_lowercase())
}

/// A registry identity or a driver proof, checked as `checkedIdentity` checks it.
#[derive(Clone, Debug)]
pub struct LiveIdentity {
    /// The canonical (lower-case) email.
    pub email: String,
    /// The provider's stable subject.
    pub subject: String,
    /// The reported plan, when the proof names one.
    pub plan: Option<String>,
    /// When the proof was taken, as the stored ISO word.
    pub verified_at: String,
    /// `provider-userinfo` or `native-runtime`.
    pub source: String,
}

impl LiveIdentity {
    /// `checkedIdentity(value)`: the canonical email, the bounded subject and plan, a parseable
    /// verification time and one of the two sources.
    pub fn from_value(value: &Value) -> Option<Self> {
        let email = canonical_email(value.get("email")?.as_str()?)?;
        let subject = value.get("subject")?.as_str()?;
        if subject.is_empty() || subject.len() > 256 || subject.chars().any(char::is_control) {
            return None;
        }
        let plan = match value.get("plan") {
            None | Some(Value::Null) => None,
            Some(Value::String(plan))
                if plan.len() <= 128 && !plan.chars().any(char::is_control) =>
            {
                Some(plan.clone())
            }
            Some(_) => return None,
        };
        let verified_at = value.get("verifiedAt")?.as_str()?;
        js_date(verified_at)?;
        let source = value.get("source")?.as_str()?;
        if !matches!(source, "provider-userinfo" | "native-runtime") {
            return None;
        }
        Some(Self {
            email,
            subject: subject.to_owned(),
            plan,
            verified_at: verified_at.to_owned(),
            source: source.to_owned(),
        })
    }

    /// `identityKey`: sha256 of `JSON.stringify(["antigravity", email, subject])`.
    pub fn identity_key(&self) -> String {
        let text = json!(["antigravity", self.email, self.subject]).to_string();
        sha256_hex(text.as_bytes())
    }

    /// `now - Date.parse(verifiedAt)`, or `None` when the word does not parse.
    pub fn age_ms(&self, now_ms: i64) -> Option<i64> {
        Some(now_ms - js_date(&self.verified_at)?)
    }
}

pub(crate) struct CredentialRecord {
    pub(crate) fingerprint: String,
    pub(crate) identity: LiveIdentity,
}

pub(crate) struct ProfileRecord {
    pub(crate) id: String,
    pub(crate) email: String,
    pub(crate) identity_key: String,
    pub(crate) plan: Option<String>,
    pub(crate) credential: CredentialRecord,
}

pub(crate) struct ActiveRecord {
    profile_id: String,
    verified_at: String,
}

pub(crate) struct RegistryState {
    pub(crate) profiles: Vec<ProfileRecord>,
    active: Option<ActiveRecord>,
}

impl RegistryState {
    pub(crate) fn profile(&self, id: &str) -> Option<&ProfileRecord> {
        self.profiles.iter().find(|profile| profile.id == id)
    }

    pub(crate) fn active_for(&self, id: &str) -> Option<&ActiveRecord> {
        self.active
            .as_ref()
            .filter(|active| active.profile_id == id)
    }
}

/// One private file below `base`: the shared hardened read with the owner and mode rules.
fn read_private(base: &Path, path: &Path, cap: u64) -> Option<Vec<u8>> {
    let policy = ReadPolicy::capped(cap)
        .leaf(Leaf::Refuse)
        .ancestors(Ancestors::NoLinkBelow(base))
        .owner(Owner::Mine)
        .mode(Mode::Private);
    match aac_files::read::read(path, &policy) {
        Read::Bytes(bytes, _) => Some(bytes),
        _ => None,
    }
}

/// The newest `registry-<12 digits>.json` by name, checked as `validateState` checks it.
pub(crate) fn read_state(ccs: &Path) -> Option<RegistryState> {
    let profiles_directory = ccs.join("antigravity-profiles");
    let entries = std::fs::read_dir(&profiles_directory).ok()?;
    let mut names: Vec<String> = entries
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| {
            name.len() == "registry-000000000000.json".len()
                && name.starts_with("registry-")
                && name.ends_with(".json")
                && name["registry-".len().."registry-".len() + 12]
                    .bytes()
                    .all(|c| c.is_ascii_digit())
        })
        .collect();
    names.sort();
    let name = names.last()?;
    let bytes = read_private(ccs, &profiles_directory.join(name), MAX_REGISTRY_BYTES)?;
    let value: Value = serde_json::from_slice(&bytes).ok()?;
    let state = validate_state(&value)?;
    if name["registry-".len().."registry-".len() + 12]
        .parse::<i64>()
        .ok()?
        != state.1
    {
        return None;
    }
    Some(state.0)
}

fn checked_identity(value: Option<&Value>) -> Option<LiveIdentity> {
    LiveIdentity::from_value(value?)
}

fn validate_state(value: &Value) -> Option<(RegistryState, i64)> {
    let state = value.as_object()?;
    if state.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    let revision = state.get("revision").and_then(Value::as_i64)?;
    let rows = state.get("profiles")?.as_array()?;
    if revision < 1 || rows.len() > MAX_PROFILES {
        return None;
    }
    let mut profiles = Vec::with_capacity(rows.len());
    let mut ids = HashSet::new();
    let mut emails = HashSet::new();
    for row in rows {
        let row = row.as_object()?;
        let id = row.get("id")?.as_str()?;
        if !profile_id_ok(id) || !ids.insert(id.to_owned()) {
            return None;
        }
        let email = row.get("email")?.as_str()?;
        if !emails.insert(canonical_email(email)?) {
            return None;
        }
        let credential = row.get("credential")?.as_object()?;
        let identity = checked_identity(credential.get("identity"))?;
        if email != identity.email
            || row.get("identityKey")?.as_str()? != identity.identity_key()
            || row.get("plan").cloned().unwrap_or(Value::Null) != plan_value(&identity.plan)
        {
            return None;
        }
        let fingerprint = credential.get("fingerprint")?.as_str()?;
        let format = credential.get("format")?.as_str()?;
        if !hash_word(fingerprint) || !format_ok(format) {
            return None;
        }
        js_date(credential.get("savedAt")?.as_str()?)?;
        profiles.push(ProfileRecord {
            id: id.to_owned(),
            email: identity.email.clone(),
            identity_key: identity.identity_key(),
            plan: identity.plan.clone(),
            credential: CredentialRecord {
                fingerprint: fingerprint.to_owned(),
                identity,
            },
        });
    }
    let active = match state.get("active") {
        None | Some(Value::Null) => None,
        Some(Value::Object(active)) => {
            let profile_id = active.get("profileId")?.as_str()?;
            if !ids.contains(profile_id) {
                return None;
            }
            let verified_at = active.get("verifiedAt")?.as_str()?.to_owned();
            js_date(&verified_at)?;
            Some(ActiveRecord {
                profile_id: profile_id.to_owned(),
                verified_at,
            })
        }
        Some(_) => return None,
    };
    match state.get("transaction") {
        None | Some(Value::Null) => {}
        Some(Value::Object(tx)) => {
            let id = tx.get("id")?.as_str()?;
            let state_word = tx.get("state")?.as_str()?;
            let previous = tx.get("previousId")?.as_str()?;
            let target = tx.get("targetId")?.as_str()?;
            if id.len() != 32
                || !id
                    .bytes()
                    .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                || !matches!(state_word, "pending" | "recovery-required")
                || !ids.contains(previous)
                || !ids.contains(target)
            {
                return None;
            }
            js_date(tx.get("startedAt")?.as_str()?)?;
        }
        Some(_) => return None,
    }
    Some((RegistryState { profiles, active }, revision))
}

fn plan_value(plan: &Option<String>) -> Value {
    plan.as_ref().map_or(Value::Null, |plan| json!(plan))
}

/// `listProfiles()`: one host row per profile, from the stored active record.
fn list_profiles(state: &RegistryState) -> Vec<Value> {
    state
        .profiles
        .iter()
        .map(|profile| {
            let active = state.active_for(&profile.id);
            json!({
                "id": profile.id,
                "email": profile.email,
                "plan": plan_value(&profile.plan),
                "identityKey": profile.identity_key,
                "hosts": [{
                    "hostId": "ubuntu",
                    "available": true,
                    "active": active.is_some(),
                    "verifiedAt": active.map_or(Value::Null, |row| json!(row.verified_at)),
                    "verification": if active.is_some() { "runtime" } else { "stored-only" },
                }],
            })
        })
        .collect()
}

/// `readInventory()`: the live proof's overlay, or every host unselected when the proof is absent,
/// stale (older than five minutes) or ahead of the clock by more than a minute.
pub(crate) fn read_inventory(
    state: &RegistryState,
    live: Option<&LiveIdentity>,
    now_ms: i64,
) -> Value {
    let rows = list_profiles(state);
    if rows.is_empty() {
        return Value::Array(rows);
    }
    let selected_key = live.and_then(|identity| {
        let age = identity.age_ms(now_ms)?;
        (-IDENTITY_FUTURE_TOLERANCE_MS..=IDENTITY_MAX_AGE_MS)
            .contains(&age)
            .then(|| identity.identity_key())
    });
    Value::Array(
        rows.into_iter()
            .map(|mut row| {
                let key_ok = selected_key
                    .as_deref()
                    .is_some_and(|key| row["identityKey"].as_str() == Some(key));
                if let Some(host) = row["hosts"].get_mut(0).and_then(Value::as_object_mut) {
                    let was_active = host.get("active").and_then(Value::as_bool) == Some(true);
                    host.insert("selected".into(), json!(key_ok));
                    host.insert("active".into(), json!(was_active && key_ok));
                }
                row
            })
            .collect(),
    )
}

pub(crate) fn read_credential(ccs: &Path, profile: &ProfileRecord) -> Option<LiveIdentity> {
    let directory = ccs
        .join("antigravity-instances")
        .join(&profile.id)
        .join("ubuntu");
    let file = directory.join(format!("credential-{}.bin", profile.credential.fingerprint));
    let bytes = read_private(ccs, &file, MAX_NATIVE_BYTES)?;
    if sha256_hex(&bytes) != profile.credential.fingerprint {
        return None;
    }
    if profile.identity_key != profile.credential.identity.identity_key() {
        return None;
    }
    Some(profile.credential.identity.clone())
}

/// `runtime-composition.ts` `mapProfiles`: the runtime rows the usage service consumes. A saved
/// credential that is missing, changed or no longer the record's identity makes the row unverified.
pub(crate) fn map_profiles(ccs: &Path, state: &RegistryState, rows: &Value) -> Vec<Value> {
    let Some(rows) = rows.as_array() else {
        return Vec::new();
    };
    rows.iter()
        .map(|row| {
            let profile = row
                .get("id")
                .and_then(Value::as_str)
                .and_then(|id| state.profile(id));
            let host = row.get("hosts").and_then(|hosts| hosts.get(0));
            let saved = profile.and_then(|profile| {
                let identity = read_credential(ccs, profile)?;
                let email_ok = identity.email.to_lowercase() == profile.email.to_lowercase();
                (identity.identity_key() == profile.identity_key && email_ok).then_some(identity)
            });
            let Some(profile) = profile else {
                return Value::Null;
            };
            let (id, email) = (profile.id.clone(), profile.email.clone());
            let plan = plan_value(&profile.plan);
            let verified_at = host
                .and_then(|host| host.get("verifiedAt"))
                .filter(|value| !value.is_null())
                .cloned();
            match saved {
                Some(identity) => json!({
                    "id": id,
                    "email": email,
                    "plan": plan,
                    "identityKey": profile.identity_key,
                    "credentialRevision": profile.credential.fingerprint,
                    "identityVerified": true,
                    "available": host.and_then(|h| h.get("available")) == Some(&json!(true)),
                    "selected": host.and_then(|h| h.get("selected")) == Some(&json!(true)),
                    "runtimeVerified": host.and_then(|h| h.get("active")) == Some(&json!(true))
                        && host.and_then(|h| h.get("verification")).and_then(Value::as_str) == Some("runtime"),
                    "verifiedAt": verified_at.unwrap_or_else(|| json!(identity.verified_at)),
                }),
                None => json!({
                    "id": id,
                    "email": email,
                    "plan": plan,
                    "identityKey": profile.identity_key,
                    "credentialRevision": "unavailable",
                    "identityVerified": false,
                    "available": false,
                    "selected": false,
                    "runtimeVerified": false,
                    "verifiedAt": Value::Null,
                }),
            }
        })
        .filter(|row| !row.is_null())
        .collect()
}

/// `usage-service.ts` `profiles()` admission plus `publicProfile` mapping. `None` is the row TS
/// would throw on: the route declines and TS answers until the cut.
fn public_inventory(rows: &[Value]) -> Option<Value> {
    if rows.len() > MAX_PUBLIC_PROFILES {
        return None;
    }
    let mut ids = HashSet::new();
    let mut identities = HashSet::new();
    let mut selected = 0;
    let mut profiles = Vec::with_capacity(rows.len());
    for row in rows {
        let id = row.get("id")?.as_str()?;
        let identity_key = row.get("identityKey")?.as_str()?;
        let revision = row.get("credentialRevision")?.as_str()?;
        let verified = row.get("identityVerified").and_then(Value::as_bool)?;
        for key in ["available", "selected", "runtimeVerified"] {
            row.get(key)?.as_bool()?;
        }
        if !profile_id_ok(id)
            || id.len() > 64
            || !ids.insert(id.to_owned())
            || identity_key.is_empty()
            || identity_key.len() > 512
            || revision.is_empty()
            || revision.len() > 128
            || (verified && !identities.insert(identity_key.to_owned()))
        {
            return None;
        }
        if verified && row.get("selected").and_then(Value::as_bool)? {
            selected += 1;
        }
        profiles.push(aac_views::antigravity::public_profile(row)?);
    }
    if selected > 1 {
        return None;
    }
    Some(json!({ "schemaVersion": 1, "hostId": "ubuntu", "profiles": profiles }))
}

/// [`inventory`] plus the two read-only probes `runtime-composition.ts` attaches: a paused managed
/// update (`isNativeVersion`) and a runtime-service problem (`publicRuntimeServiceProblem`).
pub fn inventory_with_probes(
    ccs: &Path,
    live: Option<&LiveIdentity>,
    now_ms: i64,
    paused: Option<&Value>,
    problem: Option<&Value>,
) -> Option<Value> {
    let mut out = inventory(ccs, live, now_ms)?;
    if let Some(problem) = aac_views::antigravity::runtime_problem_value(problem) {
        out["runtimeServiceProblem"] = problem;
    }
    if let Some(paused) = paused {
        out["nativeUpdatePaused"] = json!({
            "installedVersion": aac_views::antigravity::native_version_word(paused.get("installedVersion")),
        });
    }
    Some(out)
}

/// [`inventory`] at the wall clock, for a route answer that has no clock of its own.
pub fn inventory_now(ccs: &Path) -> Option<Value> {
    let now = i64::try_from(aac_core::UtcMs::now().ok()?.get()).ok()?;
    inventory(ccs, None, now)
}

/// The runtime's `getInventory()` for one saved-profile scope, from the registry and the live
/// proof alone. `None` declines (unreadable or invalid storage).
pub fn inventory(ccs: &Path, live: Option<&LiveIdentity>, now_ms: i64) -> Option<Value> {
    let state = read_state(ccs)?;
    let overlay = read_inventory(&state, live, now_ms);
    public_inventory(&map_profiles(ccs, &state, &overlay))
}

/// `readSelectedProfileId()`.
pub fn selected_profile_id(ccs: &Path, live: Option<&LiveIdentity>, now_ms: i64) -> Option<String> {
    let state = read_state(ccs)?;
    let overlay = read_inventory(&state, live, now_ms);
    map_profiles(ccs, &state, &overlay)
        .into_iter()
        .find(|row| {
            row.get("identityVerified") == Some(&json!(true))
                && row.get("selected") == Some(&json!(true))
        })
        .and_then(|row| row.get("id").and_then(Value::as_str).map(str::to_owned))
}

/// `hasProfiles()`: storage holding at least one saved profile.
pub fn has_profiles(ccs: &Path) -> bool {
    read_state(ccs).is_some_and(|state| !state.profiles.is_empty())
}

/// `runtime-service.ts`'s factory signature.
pub type RuntimeFactory<V> = Box<dyn Fn(&Path) -> Option<V> + Send + Sync>;

/// `runtime-service.ts`'s process-wide seam: one runtime per resolved scope, and an empty scope is
/// asked again only after [`RUNTIME_RETRY_MS`], so a first profile appears without a restart.
pub struct RuntimeCache<V> {
    factory: Option<RuntimeFactory<V>>,
    runtimes: HashMap<PathBuf, Option<V>>,
    empty_checks: HashMap<PathBuf, i64>,
}

impl<V> Default for RuntimeCache<V> {
    fn default() -> Self {
        Self {
            factory: None,
            runtimes: HashMap::new(),
            empty_checks: HashMap::new(),
        }
    }
}

impl<V> RuntimeCache<V> {
    /// `configureAntigravityRuntimeFactory`: explicit composition, exactly once.
    pub fn configure(&mut self, factory: RuntimeFactory<V>) -> bool {
        if self.factory.is_some() {
            return false;
        }
        self.factory = Some(factory);
        true
    }

    /// `getAntigravityRuntime(scope, now)`. The returned runtime is cached; `None` is cached for
    /// [`RUNTIME_RETRY_MS`] before the factory is asked again.
    pub fn get(&mut self, scope: &Path, now: i64) -> Option<V>
    where
        V: Clone,
    {
        let key = std::path::absolute(scope).unwrap_or_else(|_| scope.to_path_buf());
        if let Some(Some(runtime)) = self.runtimes.get(&key) {
            return Some(runtime.clone());
        }
        let empty_age = self.empty_checks.get(&key).map(|at| now - at);
        if self.runtimes.contains_key(&key) && empty_age.is_some_and(|age| age < RUNTIME_RETRY_MS) {
            return None;
        }
        let created = self.factory.as_ref().map(|factory| factory(&key))?;
        self.empty_checks.insert(key.clone(), now);
        self.runtimes.insert(key.clone(), created);
        self.runtimes.get(&key).and_then(Option::as_ref).cloned()
    }

    /// `startAntigravityRuntime`: the server's start builds its scope's runtime now.
    pub fn start(&mut self, scope: &Path, now: i64) -> Option<V>
    where
        V: Clone,
    {
        self.get(scope, now)
    }

    /// `stopAntigravityRuntime`: the runtime holds no timers, so there is nothing to stop.
    pub fn stop(&mut self) {}
}

/// `Date.parse` for a stored word: the same ECMAScript instant parse the views use.
pub(crate) fn js_date(word: &str) -> Option<i64> {
    aac_views::js::parse_date(word).ok().flatten()
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use base64::Engine;
    use serde_json::Value;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    const CASES: &str = include_str!("../../../tests/parity/g6/cases.json");
    const EXPECTED: &str = include_str!("../../../tests/parity/g6/expected.json");

    #[cfg(unix)]
    fn private(path: &Path, directory: bool) {
        use std::os::unix::fs::PermissionsExt;
        let mode = if directory { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
    #[cfg(not(unix))]
    fn private(_path: &Path, _directory: bool) {}

    fn directory(path: &Path) {
        std::fs::create_dir_all(path).unwrap();
        private(path, true);
    }

    fn write(path: &Path, bytes: &[u8]) {
        if let Some(parent) = path.parent() {
            directory(parent);
        }
        std::fs::write(path, bytes).unwrap();
        private(path, false);
    }

    /// The fixture as `cases.json` describes it: a read-only registry and its credential files.
    fn materialize(case: &Value) -> (tempfile::TempDir, PathBuf) {
        let temp = tempfile::tempdir().unwrap();
        let ccs = temp.path().to_path_buf();
        private(&ccs, true);
        let mut records = Vec::new();
        for profile in case["profiles"].as_array().unwrap() {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(profile["credential"]["bytes"].as_str().unwrap())
                .unwrap();
            let fingerprint = sha256_hex(&bytes);
            if case.get("omitCredentials") != Some(&Value::Bool(true)) {
                write(
                    &ccs.join("antigravity-instances")
                        .join(profile["id"].as_str().unwrap())
                        .join("ubuntu")
                        .join(format!("credential-{fingerprint}.bin")),
                    &bytes,
                );
            }
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
                "active": case["active"],
                "transaction": null,
            })
            .to_string()
            .as_bytes(),
        );
        (temp, ccs)
    }

    fn case_report(case: &Value, now: i64) -> Value {
        let (_temp, ccs) = materialize(case);
        let live = case
            .get("live")
            .filter(|value| !value.is_null())
            .and_then(LiveIdentity::from_value);
        let inventory = inventory_with_probes(
            &ccs,
            live.as_ref(),
            now,
            case.get("paused"),
            case.get("problem"),
        )
        .unwrap_or_else(|| panic!("state unreadable for {}", case["name"]));
        let state = read_state(&ccs).unwrap();
        let overlay = read_inventory(&state, live.as_ref(), now);
        let selected = selected_profile_id(&ccs, live.as_ref(), now);
        json!({
            "name": case["name"],
            "profiles": overlay,
            "inventory": inventory,
            "selectedProfileId": selected.map_or(Value::Null, Value::String),
        })
    }

    /// The parity gate: the same fixture answers exactly what the TypeScript runtime answered
    /// (`tests/parity/g6/oracle.mjs` recorded `expected.json`).
    #[test]
    fn the_inventory_matches_the_typescript_oracle_on_every_fixture() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        let expected: Value = serde_json::from_str(EXPECTED).unwrap();
        let now = js_date(cases["now"].as_str().unwrap()).unwrap();
        let expected = expected.as_array().unwrap();
        assert_eq!(expected.len(), cases["cases"].as_array().unwrap().len());
        for (case, oracle) in cases["cases"].as_array().unwrap().iter().zip(expected) {
            assert_eq!(&case_report(case, now), oracle, "{}", case["name"]);
        }
    }

    /// The route's own path once TS stops sending the input: the wall-clock answer is the
    /// registry's public inventory (no live proof, so the same rows the stale fixture recorded).
    #[test]
    fn the_route_answer_reads_the_registry_without_ts_input() {
        let cases: Value = serde_json::from_str(CASES).unwrap();
        let expected: Value = serde_json::from_str(EXPECTED).unwrap();
        // The empty registry has no probes to attach, so the wall-clock route answer is exact.
        let case = &cases["cases"][3];
        let (_temp, ccs) = materialize(case);
        let answered = inventory_now(&ccs).expect("the fixture registry reads");
        assert_eq!(answered, expected[3]["inventory"], "no-profiles");
    }

    #[test]
    fn a_registry_that_cannot_be_read_safely_declines() {
        let temp = tempfile::tempdir().unwrap();
        let ccs = temp.path();
        private(ccs, true);
        assert_eq!(inventory(ccs, None, 0), None);
        directory(&ccs.join("antigravity-profiles"));
        assert!(read_state(ccs).is_none());
        assert!(!has_profiles(ccs));
        // A registry whose filename revision disagrees with its body is corrupt, not newer.
        write(
            &ccs.join("antigravity-profiles")
                .join("registry-000000000002.json"),
            br#"{"version":1,"revision":1,"profiles":[],"active":null,"transaction":null}"#,
        );
        assert!(read_state(ccs).is_none());
    }

    #[test]
    fn the_runtime_cache_retries_an_empty_scope_after_fifteen_seconds() {
        let mut cache = RuntimeCache::<u32>::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        assert!(cache.configure(Box::new(move |_| {
            counted.fetch_add(1, Ordering::SeqCst);
            None
        })));
        // A second configuration is refused, as the TS factory guard refuses it.
        assert!(!cache.configure(Box::new(|_| Some(1))));
        let scope = Path::new("/fixture/antigravity-scope");
        assert_eq!(cache.get(scope, 1_000), None);
        assert_eq!(cache.get(scope, 1_000 + RUNTIME_RETRY_MS - 1), None);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "an empty scope is not re-asked inside 15 s"
        );
        assert_eq!(cache.get(scope, 1_000 + RUNTIME_RETRY_MS), None);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "the retry asks the factory again"
        );
    }

    #[test]
    fn a_created_runtime_is_cached_per_resolved_scope() {
        let mut cache = RuntimeCache::<u32>::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        assert!(cache.configure(Box::new(move |scope| {
            counted.fetch_add(1, Ordering::SeqCst);
            Some(u32::try_from(scope.to_string_lossy().len()).unwrap_or(0))
        })));
        let first = cache.get(Path::new("/fixture/one"), 5_000).unwrap();
        assert_eq!(cache.get(Path::new("/fixture/one"), 9_000), Some(first));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(cache.start(Path::new("/fixture/two"), 5_000).is_some());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        cache.stop();
        assert_eq!(
            cache.get(Path::new("/fixture/one"), 99_000),
            Some(first),
            "a built runtime survives the retry window"
        );
    }
}
