//! Model pricing (cut gap G15): the models.dev registry cache and the rate resolution the Analytics
//! read needs, ported from `src/web-server/model-pricing.ts`, `model-pricing-data.ts`,
//! `models-dev/registry-cache.ts`, `models-dev/pricing-resolver.ts` and
//! `services/account-analytics-pricing.ts` so the cut can delete them.
//!
//! Parity is byte equality on the spec corpus (`tests/spec/pricing/cases.json`, generated from the
//! TS modules by `tests/spec/pricing/parity.ts`) over `JSON.stringify`-shaped values: every
//! resolution `{pricing, source}`, the analytics rate rows lane D's owned read consumes, and the
//! cache file bytes a refresh writes. Rates are JSON doubles end to end; [`crate::js_json`] writes
//! them exactly as JavaScript would. The one deliberate divergence is Unicode case folding outside
//! ASCII ids (JS `toLowerCase`), which real model ids never use.
//!
//! The static table is `data.json`, generated from the TS table by
//! `tests/spec/pricing/generate-table.ts`; editing rates after the cut means editing that file.
pub mod models_dev;
pub mod resolver;
mod table;

pub use models_dev::{
    CACHE_FILE_NAME, CACHE_TTL_MS, CachedRegistry, Fetched, LIVE_FETCH_TIMEOUT_MS,
    MODELS_DEV_API_URL, RegistryCache, RegistryRefresh, cache_path, normalize_cache_payload,
    normalize_registry_payload, refresh_models_dev_registry,
};
pub use resolver::{AnalyticsRates, Pricing, Rates, Resolution, Resolver, Source};
pub use table::known_table_models;
