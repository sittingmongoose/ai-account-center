//! The models.dev registry cache (`models-dev/registry-cache.ts`) and the per-registry pricing
//! resolution (`models-dev/pricing-resolver.ts`): the cache file under the CCS dir with its 24 h
//! TTL, the payload normalization, the best-effort write and the live refresh flow with its stale
//! fallback. The HTTP fetch itself is injected (the service supplies one; tests supply recorded
//! fixtures — no live models.dev call), and the background starter coalesces concurrent callers
//! the way `startModelsDevRegistryRefresh` shares one promise.
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
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

/// The parsed cache as resolution reads it: per provider (payload order) its `id`, and per model
/// its `id` and resolved list rates, with the normalized model index `findModel` builds (the
/// normalized key and the normalized `id` both point at the model; a later entry wins a
/// collision, like the TS `Map.set`). The catalog's other fields (names, limits, modalities,
/// dates) and the JSON tree itself are not kept resident; the cache file and the refresh return
/// payload keep them, since the write path serializes the full normalized map.
pub struct CachedRegistry {
    /// `fetchedAt` as parsed.
    pub fetched_at: f64,
    providers: Vec<ProviderEntry>,
    /// Provider key to its position in `providers`.
    keys: HashMap<Box<str>, usize>,
}

struct ProviderEntry {
    key: Box<str>,
    id: Box<str>,
    models: Vec<ModelEntry>,
    /// Normalized candidate id to the model's position in `models`.
    index: HashMap<Box<str>, usize>,
}

struct ModelEntry {
    id: Option<Box<str>>,
    /// `toPricing` of the model's `cost`, None when it has no usable input and output rates.
    rates: Option<Rates>,
}

impl CachedRegistry {
    /// Index already-normalized providers (with their `fetchedAt`), keeping only what
    /// resolution reads.
    pub fn new(fetched_at: f64, providers: Map<String, Value>) -> Self {
        let mut entries = Vec::with_capacity(providers.len());
        let mut keys = HashMap::with_capacity(providers.len());
        for (key, provider) in &providers {
            let id = provider
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let mut models = Vec::new();
            let mut index = HashMap::new();
            if let Some(listed) = provider.get("models").and_then(Value::as_object) {
                for (model_key, model) in listed {
                    if !model.is_object() {
                        continue;
                    }
                    let id = model.get("id").and_then(Value::as_str);
                    index.insert(normalize_id(model_key).into_boxed_str(), models.len());
                    if let Some(id) = id {
                        index.insert(normalize_id(id).into_boxed_str(), models.len());
                    }
                    models.push(ModelEntry {
                        id: id.map(Box::from),
                        rates: to_pricing(model.get("cost")),
                    });
                }
            }
            keys.insert(Box::from(key.as_str()), entries.len());
            entries.push(ProviderEntry {
                key: Box::from(key.as_str()),
                id: Box::from(id),
                models,
                index,
            });
        }
        drop(providers);
        CachedRegistry {
            fetched_at,
            providers: entries,
            keys,
        }
    }

    /// The provider keys, in payload order.
    pub fn provider_keys(&self) -> impl Iterator<Item = &str> {
        self.providers.iter().map(|provider| &*provider.key)
    }

    /// Whether the registry lists the provider key.
    pub fn has_provider(&self, key: &str) -> bool {
        self.keys.contains_key(key)
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
    memo: Option<(StatKey, Arc<CachedRegistry>)>,
}

/// One parsed registry per cache file for the whole process: every `RegistryCache` (the route's
/// shared resolver, the writers' pricing seam, the refresh point) shares the one parse until the
/// file changes. Bounded: a service reads one cache file; the cap only evicts test fixtures' paths.
type SharedMap = HashMap<PathBuf, (StatKey, Arc<CachedRegistry>)>;
static SHARED: Mutex<Option<SharedMap>> = Mutex::new(None);
const SHARED_PATHS: usize = 16;

fn shared_lock() -> std::sync::MutexGuard<'static, Option<SharedMap>> {
    SHARED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
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
            let shared = shared_lock()
                .as_ref()
                .and_then(|held| held.get(&self.path))
                .filter(|(held_key, _)| *held_key == key)
                .map(|(_, cached)| Arc::clone(cached));
            let cached = match shared {
                Some(cached) => cached,
                None => {
                    let text = read_cache_text(&self.path)?;
                    let payload: Value = serde_json::from_str(&text).ok()?;
                    let (fetched_at, providers) = normalize_cache_payload(&payload)?;
                    let cached = Arc::new(CachedRegistry::new(fetched_at, providers));
                    let mut held = shared_lock();
                    let map = held.get_or_insert_with(HashMap::new);
                    if map.len() >= SHARED_PATHS {
                        map.clear();
                    }
                    map.insert(self.path.clone(), (key, Arc::clone(&cached)));
                    cached
                }
            };
            self.memo = Some((key, cached));
        }
        let (_, cached) = self.memo.as_ref().expect("memo just set");
        if !allow_stale && now - cached.fetched_at > CACHE_TTL_MS {
            return None;
        }
        Some(cached.as_ref())
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
        if let Some(map) = shared_lock().as_mut() {
            map.remove(&self.path);
        }
    }

    /// `clearModelsDevRegistryCache`: true when the file existed and was removed.
    pub fn clear(&mut self) -> bool {
        self.memo = None;
        if let Some(map) = shared_lock().as_mut() {
            map.remove(&self.path);
        }
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

/// The one read of the models.dev cache file (the memo refresh and the full refresh payload share
/// it, so the file keeps the single read the one-file policy admits).
fn read_cache_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// The full normalized payload re-read from the cache file: the refresh flow returns it (the
/// returned registry keeps every extra field), while the resident memo holds only the fields
/// resolution reads. Transient — the caller drops it.
fn full_providers(path: &Path) -> Option<Map<String, Value>> {
    let text = read_cache_text(path)?;
    let payload: Value = serde_json::from_str(&text).ok()?;
    normalize_cache_payload(&payload).map(|(_, providers)| providers)
}

fn stale(cache: &Mutex<RegistryCache>, now: f64) -> Option<Map<String, Value>> {
    let mut guard = lock(cache);
    guard.read(true, now)?;
    let path = guard.path().to_path_buf();
    drop(guard);
    full_providers(&path)
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
        if guard.read(false, now).is_some() {
            let path = guard.path().to_path_buf();
            drop(guard);
            return full_providers(&path);
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
            if guard.read(false, now).is_some() {
                let path = guard.path().to_path_buf();
                drop(guard);
                return full_providers(&path);
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

    /// The coalesced refresh for a caller that only needs the cache kept fresh (the Analytics
    /// activity pass): `true` when a registry (fresh or stale) is usable afterwards. The same
    /// fetches and cache writes as [`start`](Self::start), but the cache file is never re-read
    /// into a full payload nobody uses: freshness comes from the shared resident memo, so a
    /// 6.8 MB cache no longer costs a ~150 MiB JSON tree on every pass.
    pub fn ensure(
        &self,
        force: bool,
        now: f64,
        fetch: impl FnOnce() -> Result<Fetched, FetchError>,
    ) -> bool {
        let usable = |fresh: bool| lock(&self.cache).read(!fresh, now).is_some();
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
            drop(pending);
            return usable(false);
        }
        if !force && usable(true) {
            return true;
        }
        *pending = true;
        drop(pending);
        let written = match fetch() {
            Ok(Fetched { ok: true, body }) => {
                let parsed: Option<Value> = serde_json::from_slice(&body).ok();
                drop(body);
                match parsed.as_ref().and_then(normalize_registry_payload) {
                    Some(providers) => {
                        drop(parsed);
                        lock(&self.cache).write(&providers, now);
                        true
                    }
                    None => false,
                }
            }
            _ => false,
        };
        let result = written || usable(false);
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

/// `normalizeModelsDevProviderId`: the alias-mapped, trimmed and lowercased provider id.
pub fn normalize_models_dev_provider_id(provider: &str) -> Option<String> {
    if provider.is_empty() {
        return None;
    }
    let normalized = normalize_id(provider);
    // `PROVIDER_ALIASES`: an alias the provider table lists maps to its models.dev provider.
    Some(aac_providers::pricing_alias(&normalized).map_or(normalized, str::to_owned))
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
fn find_model(index: &HashMap<Box<str>, usize>, model: &str) -> Option<usize> {
    get_model_candidates(model)
        .iter()
        .find_map(|candidate| index.get(candidate.as_str()).copied())
}

/// `resolveProviderModel` for a provider already resolved to its map key: the provider's and
/// model's normalized ids with the resolved rates.
fn resolve_in_provider(
    cached: &CachedRegistry,
    provider_key: &str,
    model: &str,
) -> Option<(String, String, Rates)> {
    let provider = &cached.providers[*cached.keys.get(provider_key)?];
    let entry = &provider.models[find_model(&provider.index, model)?];
    let rates = entry.rates?;
    Some((
        provider.id.to_string(),
        entry.id.as_deref().unwrap_or_default().to_owned(),
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
    for provider_key in cached.provider_keys() {
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
    for provider in &cached.providers {
        for model in &provider.models {
            if let Some(id) = &model.id {
                ids.insert(format!("{}/{id}", provider.id));
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

    /// `PROVIDER_ALIASES` is the provider table's `pricingAliases`; a word it does not alias is
    /// its own models.dev id.
    #[test]
    fn provider_aliases_come_from_the_table() {
        for (alias, models_dev) in [
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
        ] {
            assert_eq!(
                normalize_models_dev_provider_id(alias).as_deref(),
                Some(models_dev),
                "{alias}"
            );
        }
        assert_eq!(
            normalize_models_dev_provider_id("OpenAI").as_deref(),
            Some("openai")
        );
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
        assert_eq!(cached.provider_keys().count(), 1);
        assert!(cached.has_provider("anthropic"));
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

    #[test]
    fn two_caches_over_one_file_share_one_parse() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join(CACHE_FILE_NAME);
        let mut first = RegistryCache::new(path.clone());
        let mut second = RegistryCache::new(path);
        first.write(&providers(), 1.0);
        let a = first.read(true, 0.0).expect("first read") as *const CachedRegistry;
        let b = second.read(true, 0.0).expect("second read") as *const CachedRegistry;
        assert!(
            std::ptr::eq(a, b),
            "a second cache over the same unchanged file must reuse the one parse"
        );
    }

    #[test]
    fn the_resident_registry_keeps_only_the_fields_resolution_reads() {
        let root = tempfile::tempdir().expect("tempdir");
        let mut cache = RegistryCache::new(root.path().join(CACHE_FILE_NAME));
        let full = normalize_registry_payload(&json!({
            "openai": {
                "id": "openai", "name": "OpenAI", "doc": "https://openai.com",
                "models": { "gpt-5.5": {
                    "id": "gpt-5.5", "name": "GPT-5.5", "knowledge": "2026-01",
                    "modalities": { "input": ["text"] },
                    "cost": { "input": 5, "output": 30 }
                } }
            }
        }))
        .expect("normalizes");
        cache.write(&full, 1.0);
        let cached = cache.read(true, 0.0).expect("read");
        assert_eq!(get_known_models_dev_models(cached), ["openai/gpt-5.5"]);
        // Resolution answers from the compact index alone.
        let (_, _, rates) = resolve_provider_model(cached, "openai", "gpt-5.5").expect("rates");
        assert_eq!(rates.input_per_million, 5.0);
        assert_eq!(rates.output_per_million, 30.0);
    }

    #[test]
    fn a_refresh_returns_the_full_payload_even_though_the_memo_is_pruned() {
        let root = tempfile::tempdir().expect("tempdir");
        let cache = Mutex::new(RegistryCache::new(root.path().join(CACHE_FILE_NAME)));
        let full = normalize_registry_payload(&json!({
            "openai": {
                "id": "openai", "name": "OpenAI",
                "models": { "gpt-5.5": { "id": "gpt-5.5", "cost": { "input": 5, "output": 30 } } }
            }
        }))
        .expect("normalizes");
        lock(&cache).write(&full, 1.0);
        let returned =
            refresh_models_dev_registry(&cache, false, 2.0, || unreachable!("fresh cache"))
                .expect("fresh answer");
        assert_eq!(returned["openai"]["name"], json!("OpenAI"));
    }

    #[test]
    fn a_write_invalidates_the_shared_parse_for_other_caches() {
        let root = tempfile::tempdir().expect("tempdir");
        let path = root.path().join(CACHE_FILE_NAME);
        let mut first = RegistryCache::new(path.clone());
        let mut second = RegistryCache::new(path);
        first.write(&providers(), 1.0);
        assert!(first.read(true, 0.0).is_some());
        let mut replacement = Map::new();
        replacement.insert(
            "anthropic".into(),
            json!({
                "id": "anthropic",
                "models": { "claude-x": { "id": "claude-x", "cost": { "input": 1, "output": 2 } } }
            }),
        );
        second.write(&replacement, 2.0);
        let cached = first
            .read(true, 0.0)
            .expect("re-read after the other cache wrote");
        assert!(cached.has_provider("anthropic"));
        assert!(!cached.has_provider("openai"));
    }
}
