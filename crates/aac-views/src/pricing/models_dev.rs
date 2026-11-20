//! The models.dev registry cache (`models-dev/registry-cache.ts`) and the per-registry pricing
//! resolution (`models-dev/pricing-resolver.ts`): the cache file under the CCS dir with its 24 h
//! TTL, the payload normalization, the best-effort write and the live refresh flow with its stale
//! fallback. The HTTP fetch itself is injected (the service supplies one; tests supply recorded
//! fixtures — no live models.dev call), and the background starter coalesces concurrent callers
//! the way `startModelsDevRegistryRefresh` shares one promise.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::resolver::{Rates, normalize_id};

/// `MODELS_DEV_API_URL`: the registry the refresh fetches.
pub const MODELS_DEV_API_URL: &str = "https://models.dev/api.json";
/// `CACHE_FILE_NAME`: the cache file inside the CCS dir.
pub const CACHE_FILE_NAME: &str = "models-dev-registry-cache.json";
/// `CACHE_TTL_MS`: 24 hours; a cache older than this reads as absent unless stale reads are allowed.
pub const CACHE_TTL_MS: f64 = 24.0 * 60.0 * 60.0 * 1000.0;
/// `LIVE_FETCH_TIMEOUT_MS`: the live fetch aborts after 3 seconds; the fetch implementation the
/// service supplies enforces it.
pub const LIVE_FETCH_TIMEOUT_MS: u64 = 3000;

/// `<ccsDir>/models-dev-registry-cache.json`.
pub fn cache_path(ccs_dir: &Path) -> PathBuf {
    ccs_dir.join(CACHE_FILE_NAME)
}

/// One fetch outcome: the response status and body. `Err` is any transport failure (the refresh
/// flow treats both the same: the stale cache answers).
pub struct Fetched {
    /// The response's `ok` (`status` in 200..300).
    pub ok: bool,
    /// The response body bytes.
    pub body: Vec<u8>,
}

/// A transport failure. The flow never inspects it, matching the TS `catch`.
pub type FetchError = ();

/// `normalizeRegistryPayload`: providers keyed by their normalized id, each keeping its extra
/// fields, with malformed model entries dropped and ids normalized in place. `None` when nothing
/// survives (the caller then keeps the stale cache).
pub fn normalize_registry_payload(payload: &Value) -> Option<Map<String, Value>> {
    let object = payload.as_object()?;
    let mut providers = Map::new();
    for (key, value) in object {
        let Some(entry) = value.as_object() else {
            continue;
        };
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.trim().is_empty())
            .map(|id| id.trim())
            .unwrap_or(key);
        let Some(models_payload) = entry.get("models").and_then(Value::as_object) else {
            continue;
        };
        let mut models = Map::new();
        for (model_key, model_value) in models_payload {
            let Some(model_entry) = model_value.as_object() else {
                continue;
            };
            let model_id = model_entry
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.trim().is_empty())
                .map(|id| id.trim())
                .unwrap_or(model_key);
            let mut model = model_entry.clone();
            model.insert("id".into(), Value::String(model_id.to_owned()));
            models.insert(model_key.clone(), Value::Object(model));
        }
        if models.is_empty() {
            continue;
        }
        let mut provider = entry.clone();
        provider.insert("id".into(), Value::String(id.to_owned()));
        provider.insert("models".into(), Value::Object(models));
        providers.insert(id.to_owned(), Value::Object(provider));
    }
    (!providers.is_empty()).then_some(providers)
}

/// `normalizeCachePayload`: `{version: 1, fetchedAt: number, providers}` with a usable provider
/// map. `fetchedAt` stays the parsed double (JSON cannot spell NaN, but `1e999` reads Infinity
/// exactly like JavaScript's `JSON.parse`).
pub fn normalize_cache_payload(payload: &Value) -> Option<(f64, Map<String, Value>)> {
    let object = payload.as_object()?;
    if object.get("version").and_then(Value::as_f64) != Some(1.0) {
        return None;
    }
    let fetched_at = object.get("fetchedAt").and_then(Value::as_f64)?;
    let providers = object
        .get("providers")
        .and_then(normalize_registry_payload)?;
    Some((fetched_at, providers))
}

/// The parsed cache plus the normalized model index `findModel` builds per provider models map:
/// normalized key and normalized `id` both point at the model's key in the map (a later entry
/// wins a collision, like the TS `Map.set`).
pub struct CachedRegistry {
    /// `fetchedAt` as parsed.
    pub fetched_at: f64,
    /// The normalized providers, in payload order.
    pub providers: Map<String, Value>,
    /// Provider key → (normalized candidate id → model key).
    index: HashMap<String, HashMap<String, String>>,
}

fn build_index(providers: &Map<String, Value>) -> HashMap<String, HashMap<String, String>> {
    let mut index = HashMap::new();
    for (provider_key, provider) in providers {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        let mut entries = HashMap::new();
        for (model_key, model) in models {
            if !model.is_object() {
                continue;
            }
            entries.insert(normalize_id(model_key), model_key.clone());
            if let Some(id) = model.get("id").and_then(Value::as_str) {
                entries.insert(normalize_id(id), model_key.clone());
            }
        }
        index.insert(provider_key.clone(), entries);
    }
    index
}

impl CachedRegistry {
    /// Wrap already-normalized providers (with their `fetchedAt`).
    pub fn new(fetched_at: f64, providers: Map<String, Value>) -> Self {
        let index = build_index(&providers);
        CachedRegistry {
            fetched_at,
            providers,
            index,
        }
    }
}

/// The parsed identity of one cache file (size and mtime; the TS key also has the inode, which is
/// not observable through it).
type StatKey = (u64, Option<SystemTime>);

fn stat_key(path: &Path) -> Option<StatKey> {
    let metadata = fs::metadata(path).ok()?;
    Some((metadata.len(), metadata.modified().ok()))
}

/// The cache file's parsed state, memoised on its stat identity like the TS module state: repeated
/// reads share one parsed registry until the file changes or a write clears the memo.
pub struct RegistryCache {
    path: PathBuf,
    memo: Option<(StatKey, CachedRegistry)>,
}

impl RegistryCache {
    /// A cache bound to one file path.
    pub fn new(path: PathBuf) -> Self {
        RegistryCache { path, memo: None }
    }

    /// The bound file path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `getCachedModelsDevRegistry`: the parsed registry, or `None` when the file is missing,
    /// malformed or (without `allow_stale`) past its TTL.
    pub fn read(&mut self, allow_stale: bool, now: f64) -> Option<&CachedRegistry> {
        let key = stat_key(&self.path)?;
        if !self.memo.as_ref().is_some_and(|(k, _)| *k == key) {
            let text = fs::read_to_string(&self.path).ok()?;
            let payload: Value = serde_json::from_str(&text).ok()?;
            let (fetched_at, providers) = normalize_cache_payload(&payload)?;
            self.memo = Some((key, CachedRegistry::new(fetched_at, providers)));
        }
        let (_, cached) = self.memo.as_ref().expect("memo just set");
        if !allow_stale && now - cached.fetched_at > CACHE_TTL_MS {
            return None;
        }
        Some(cached)
    }

    /// `setCachedModelsDevRegistry`: best-effort write of the cache bytes `JSON.stringify` would
    /// produce, then the memo drops.
    pub fn write(&mut self, providers: &Map<String, Value>, fetched_at: f64) {
        let mut cache = Map::new();
        cache.insert("version".into(), Value::Number(1u64.into()));
        cache.insert(
            "fetchedAt".into(),
            serde_json::Number::from_f64(fetched_at)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        );
        cache.insert("providers".into(), Value::Object(providers.clone()));
        let bytes = crate::js_json::write(&Value::Object(cache));
        if let Some(parent) = self.path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::write(&self.path, bytes);
        self.memo = None;
    }

    /// `clearModelsDevRegistryCache`: true when the file existed and was removed.
    pub fn clear(&mut self) -> bool {
        self.memo = None;
        match fs::remove_file(&self.path) {
            Ok(()) => true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
            Err(_) => false,
        }
    }

    /// `modelsDevRegistryIdentity`: `size:mtimeMs:ctimeMs:ino`, or `none`. An internal change
    /// token (the collector reuses an unchanged pass); on hosts without a ctime it repeats the
    /// mtime, and on Windows the file index stands in for the inode.
    pub fn identity(&self) -> String {
        let Ok(metadata) = fs::metadata(&self.path) else {
            return "none".into();
        };
        let mtime = modified_ms(&metadata);
        let ctime = metadata.created().ok().map(system_ms).unwrap_or(mtime);
        format!(
            "{}:{}:{}:{}",
            metadata.len(),
            mtime,
            ctime,
            inode(&metadata)
        )
    }
}

fn system_ms(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64() * 1000.0)
        .unwrap_or(f64::NAN)
}

fn modified_ms(metadata: &fs::Metadata) -> f64 {
    metadata.modified().ok().map(system_ms).unwrap_or(f64::NAN)
}

#[cfg(unix)]
fn inode(metadata: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(windows)]
fn inode(_metadata: &fs::Metadata) -> u64 {
    // The Windows file index is still unstable (`windows_by_handle`); the identity token only has
    // to change when the file does, and size + times already do.
    0
}

fn stale(cache: &Mutex<RegistryCache>, now: f64) -> Option<Map<String, Value>> {
    let mut guard = lock(cache);
    guard.read(true, now).map(|cached| cached.providers.clone())
}

fn lock(cache: &Mutex<RegistryCache>) -> std::sync::MutexGuard<'_, RegistryCache> {
    cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// `refreshModelsDevRegistry`: serve a fresh cache when one exists (unless `force`), else fetch,
/// normalize and store the payload, falling back to the stale cache on every failure.
pub fn refresh_models_dev_registry(
    cache: &Mutex<RegistryCache>,
    force: bool,
    now: f64,
    fetch: impl FnOnce() -> Result<Fetched, FetchError>,
) -> Option<Map<String, Value>> {
    if !force {
        let mut guard = lock(cache);
        if let Some(fresh) = guard.read(false, now) {
            return Some(fresh.providers.clone());
        }
    }
    match fetch() {
        Ok(Fetched { ok: true, body }) => {
            let parsed: Option<Value> = serde_json::from_slice(&body).ok();
            match parsed.as_ref().and_then(normalize_registry_payload) {
                Some(providers) => {
                    lock(cache).write(&providers, now);
                    Some(providers)
                }
                None => stale(cache, now),
            }
        }
        _ => stale(cache, now),
    }
}

/// `startModelsDevRegistryRefresh`: the coalescing refresh point. The TS module keeps one pending
/// promise, so callers that arrive while a refresh is running receive its result instead of
/// starting another fetch; a caller that arrives after it completes runs its own (a `force` one
/// refetches). The pending flag plus its condition variable give the same split per cache file
/// (the TS state is module-wide for the one CCS dir): a caller that finds a refresh in progress
/// parks until it finishes and is then answered from the cache it wrote (fresh, else stale — the
/// same value the finished refresh returned), and only a caller that finds none in progress
/// fetches. The fetch runs without the cache lock, so cache readers never wait on a refresh.
pub struct RegistryRefresh {
    cache: Mutex<RegistryCache>,
    pending: Mutex<bool>,
    done: std::sync::Condvar,
}

impl RegistryRefresh {
    /// A refresh point over `<ccs_dir>/models-dev-registry-cache.json`.
    pub fn new(ccs_dir: &Path) -> Self {
        RegistryRefresh {
            cache: Mutex::new(RegistryCache::new(cache_path(ccs_dir))),
            pending: Mutex::new(false),
            done: std::sync::Condvar::new(),
        }
    }

    /// A refresh point over one cache file path.
    pub fn over(path: impl Into<PathBuf>) -> Self {
        RegistryRefresh {
            cache: Mutex::new(RegistryCache::new(path.into())),
            pending: Mutex::new(false),
            done: std::sync::Condvar::new(),
        }
    }

    /// The bound cache, for direct reads and `Resolver` construction.
    pub fn cache(&self) -> &Mutex<RegistryCache> {
        &self.cache
    }

    /// The coalesced starter.
    pub fn start(
        &self,
        force: bool,
        now: f64,
        fetch: impl FnOnce() -> Result<Fetched, FetchError>,
    ) -> Option<Map<String, Value>> {
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if *pending {
            while *pending {
                pending = self
                    .done
                    .wait(pending)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
            }
            let mut guard = lock(&self.cache);
            if let Some(fresh) = guard.read(false, now) {
                return Some(fresh.providers.clone());
            }
            drop(guard);
            return stale(&self.cache, now);
        }
        *pending = true;
        drop(pending);
        let result = refresh_models_dev_registry(&self.cache, force, now, fetch);
        let mut pending = self
            .pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *pending = false;
        self.done.notify_all();
        result
    }
}

// ------------------------------------------------------------- pricing-resolver.ts

/// `PROVIDER_ALIASES`.
const PROVIDER_ALIASES: [(&str, &str); 11] = [
    ("agy", "google"),
    ("antigravity", "google"),
    ("claude", "anthropic"),
    ("codex", "openai"),
    ("copilot", "github-copilot"),
    ("gemini", "google"),
    ("ghcp", "github-copilot"),
    ("github", "github-copilot"),
    ("kimi", "moonshotai"),
    ("moonshot", "moonshotai"),
    ("qwen", "alibaba"),
];

/// `normalizeModelsDevProviderId`: the alias-mapped, trimmed and lowercased provider id.
pub fn normalize_models_dev_provider_id(provider: &str) -> Option<String> {
    if provider.is_empty() {
        return None;
    }
    let normalized = normalize_id(provider);
    Some(
        PROVIDER_ALIASES
            .iter()
            .find(|(alias, _)| *alias == normalized)
            .map(|(_, mapped)| (*mapped).to_owned())
            .unwrap_or(normalized),
    )
}

/// `splitProviderPrefix`: everything before the first `/` (when non-empty) is the provider.
pub fn split_provider_prefix(model: &str) -> (Option<String>, String) {
    let trimmed = model.trim();
    match trimmed.find('/') {
        Some(index) if index > 0 => (
            Some(trimmed[..index].to_owned()),
            trimmed[index + 1..].to_owned(),
        ),
        _ => (None, trimmed.to_owned()),
    }
}

/// `getModelCandidates` (the resolver's variant): normalized, base-before-`:` and their
/// Claude-date-stripped forms.
pub fn get_model_candidates(model: &str) -> Vec<String> {
    let normalized = normalize_id(model);
    let base = normalized.split(':').next().unwrap_or_default().to_owned();
    let mut candidates = vec![normalized.clone()];
    if base != normalized {
        candidates.push(base.clone());
    }
    for candidate in [normalized, base] {
        let stripped = super::resolver::strip_claude_date_suffix(&candidate);
        if stripped != candidate && !candidates.contains(&stripped) {
            candidates.push(stripped);
        }
    }
    candidates
}

/// `toPricing`: input and output must be finite numbers; the cache rates default to 0.
fn to_pricing(cost: Option<&Value>) -> Option<Rates> {
    let cost = cost?.as_object()?;
    let to_number = |field: &str| {
        cost.get(field)
            .and_then(Value::as_f64)
            .filter(|value| value.is_finite())
    };
    Some(Rates {
        input_per_million: to_number("input")?,
        output_per_million: to_number("output")?,
        cache_creation_per_million: to_number("cache_write").unwrap_or(0.0),
        cache_read_per_million: to_number("cache_read").unwrap_or(0.0),
    })
}

/// `findModel` over the index.
fn find_model<'a>(index: &'a HashMap<String, String>, model: &str) -> Option<&'a str> {
    get_model_candidates(model)
        .iter()
        .find_map(|candidate| index.get(candidate).map(String::as_str))
}

/// `resolveProviderModel` for a provider already resolved to its map key: the provider's and
/// model's normalized ids with the resolved rates.
fn resolve_in_provider(
    cached: &CachedRegistry,
    provider_key: &str,
    model: &str,
) -> Option<(String, String, Rates)> {
    let provider = cached.providers.get(provider_key)?;
    let entry_key = find_model(cached.index.get(provider_key)?, model)?;
    let entry = provider.get("models")?.get(entry_key)?;
    let rates = to_pricing(entry.get("cost"))?;
    Some((
        provider
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        entry
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        rates,
    ))
}

/// `resolveProviderModel`: the normalized provider id selects the provider.
pub fn resolve_provider_model(
    cached: &CachedRegistry,
    provider: &str,
    model: &str,
) -> Option<(String, String, Rates)> {
    let provider_key = normalize_models_dev_provider_id(provider)?;
    resolve_in_provider(cached, &provider_key, model)
}

fn same_rates(left: &Rates, right: &Rates) -> bool {
    left.input_per_million == right.input_per_million
        && left.output_per_million == right.output_per_million
        && left.cache_creation_per_million == right.cache_creation_per_million
        && left.cache_read_per_million == right.cache_read_per_million
}

/// `resolveUnambiguousModel`: the first provider match when every provider that has the model
/// prices it the same; `None` on disagreement or no match.
pub fn resolve_unambiguous_model(
    cached: &CachedRegistry,
    model: &str,
) -> Option<(String, String, Rates)> {
    let mut first: Option<(String, String, Rates)> = None;
    for provider_key in cached.providers.keys() {
        if let Some(found) = resolve_in_provider(cached, provider_key, model) {
            if first.as_ref().is_some_and(|f| !same_rates(&f.2, &found.2)) {
                return None;
            }
            first.get_or_insert(found);
        }
    }
    first
}

/// `getKnownModelsDevModels`: `provider/model` ids. TS sorts with `localeCompare`; this sorts by
/// bytes, which is the same order for the ASCII ids real registries use.
pub fn get_known_models_dev_models(cached: &CachedRegistry) -> Vec<String> {
    let mut ids = std::collections::BTreeSet::new();
    for provider in cached.providers.values() {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for model in models.values() {
            if let Some(id) = model.get("id").and_then(Value::as_str) {
                ids.insert(format!(
                    "{}/{id}",
                    provider
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                ));
            }
        }
    }
    ids.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn providers() -> Map<String, Value> {
        normalize_registry_payload(&json!({
            "openai": { "id": "openai", "models": { "gpt-5.5": { "id": "gpt-5.5", "cost": { "input": 5, "output": 30 } } } }
        })).expect("fixture normalizes")
    }

    #[test]
    fn clear_reports_whether_the_file_existed() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut cache = RegistryCache::new(root.path().join(CACHE_FILE_NAME));
        assert!(!cache.clear());
        cache.write(&providers(), 1.0);
        assert!(cache.read(true, 0.0).is_some());
        assert!(cache.clear());
        assert!(!cache.read(true, 0.0).is_some());
    }

    #[test]
    fn the_identity_tracks_the_file_and_recovers_after_removal() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut cache = RegistryCache::new(root.path().join(CACHE_FILE_NAME));
        assert_eq!(cache.identity(), "none");
        cache.write(&providers(), 1.0);
        let written = cache.identity();
        assert_ne!(written, "none");
        assert_eq!(written.split(':').count(), 4, "size:mtime:ctime:ino");
        assert_eq!(cache.identity(), written, "stable while the file stands");
        // A differently-sized cache (the same-mtime rewrite the token cannot see) changes it.
        let mut larger = providers();
        larger.insert(
            "anthropic".into(),
            json!({ "id": "anthropic", "models": { "claude-x": { "id": "claude-x", "cost": { "input": 1, "output": 2 } } } }),
        );
        cache.write(&larger, 2.0);
        assert_ne!(cache.identity(), written);
        assert!(cache.clear());
        assert_eq!(cache.identity(), "none");
    }

    #[test]
    fn a_write_drops_the_parse_memo_so_reads_see_new_data() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut cache = RegistryCache::new(root.path().join(CACHE_FILE_NAME));
        cache.write(&providers(), 1.0);
        assert!(cache.read(false, 2.0).is_some());
        let mut other = providers();
        other.remove("openai").expect("provider present");
        let mut replacement = Map::new();
        replacement.insert(
            "anthropic".into(),
            json!({
                "id": "anthropic",
                "models": { "claude-x": { "id": "claude-x", "cost": { "input": 1, "output": 2 } } }
            }),
        );
        cache.write(&replacement, 3.0);
        let cached = cache.read(true, 0.0).expect("re-read");
        assert_eq!(cached.providers.len(), 1);
        assert!(cached.providers.contains_key("anthropic"));
    }

    #[test]
    fn the_coalesced_starter_runs_one_fetch_for_concurrent_callers() {
        let root = tempfile::tempdir().expect("tempdir");
        let refresh = std::sync::Arc::new(RegistryRefresh::over(root.path().join(CACHE_FILE_NAME)));
        let body = json!({
            "openai": { "id": "openai", "models": { "gpt-5.5": { "id": "gpt-5.5", "cost": { "input": 5, "output": 30 } } } }
        })
        .to_string();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let arrived = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut joins = Vec::new();
        for _ in 0..4 {
            let refresh = refresh.clone();
            let calls = calls.clone();
            let arrived = arrived.clone();
            let body = body.clone();
            joins.push(std::thread::spawn(move || {
                arrived.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                refresh.start(true, 7.0, || {
                    calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    // Hold the batch open until every caller has entered the starter, and then a
                    // settle beat for them to park on the pending flag — the role the TS test's
                    // unresolved response promise plays there.
                    while arrived.load(std::sync::atomic::Ordering::SeqCst) < 4 {
                        std::thread::yield_now();
                    }
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    Ok(Fetched {
                        ok: true,
                        body: body.clone().into_bytes(),
                    })
                })
            }));
        }
        for join in joins {
            assert!(join.join().expect("thread").is_some());
        }
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        // A later non-forced caller is served by the fresh cache without any fetch.
        assert!(
            refresh_models_dev_registry(refresh.cache(), false, 8.0, || unreachable!("no fetch"))
                .is_some()
        );
    }
}
