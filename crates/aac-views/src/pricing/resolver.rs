//! Rate resolution (`model-pricing.ts`) and the Analytics pricing wrapper
//! (`account-analytics-pricing.ts`): the CCS static table, the models.dev cache and the
//! unknown-model fallback, in that order, with the service-tier overlay. The [`Resolver`] mirrors
//! the TS module surface (`getModelPricingWithSource`, `defaultAccountAnalyticsPricing`, the
//! memoised variant and the `model\0route` facts table lane D's owned read consumes).
use std::collections::HashMap;
use std::path::Path;

use serde_json::{Map, Value};

use super::models_dev::{
    CachedRegistry, RegistryCache, resolve_provider_model, resolve_unambiguous_model,
};
use super::table::Table;

/// `PricingRates`: USD per million tokens.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rates {
    /// `inputPerMillion`.
    pub input_per_million: f64,
    /// `outputPerMillion`.
    pub output_per_million: f64,
    /// `cacheCreationPerMillion`.
    pub cache_creation_per_million: f64,
    /// `cacheReadPerMillion`.
    pub cache_read_per_million: f64,
}

/// `ModelPricing`: rates plus the optional per-service-tier overrides.
#[derive(Clone, Debug)]
pub struct Pricing {
    /// The base rates.
    pub rates: Rates,
    /// `serviceTiers`, in table order.
    pub service_tiers: Option<Vec<(String, Rates)>>,
}

/// Which table supplied the rates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// The CCS static table.
    Builtin,
    /// The models.dev cache.
    ModelsDev,
    /// `UNKNOWN_MODEL_PRICING`.
    Fallback,
}

impl Source {
    /// The `source` field's text.
    pub fn as_str(&self) -> &'static str {
        match self {
            Source::Builtin => "builtin",
            Source::ModelsDev => "models-dev",
            Source::Fallback => "fallback",
        }
    }
}

/// `ModelPricingResolution` (`getModelPricingWithSource`).
#[derive(Clone, Debug)]
pub struct Resolution {
    /// The resolved pricing (with the service tier applied).
    pub pricing: Pricing,
    /// Where the rates came from.
    pub source: Source,
}

/// `AccountAnalyticsModelRates` (`defaultAccountAnalyticsPricing`).
#[derive(Clone, Debug, PartialEq)]
pub struct AnalyticsRates {
    /// `inputPerMillion`.
    pub input_per_million: f64,
    /// `outputPerMillion`.
    pub output_per_million: f64,
    /// `cacheCreationPerMillion`.
    pub cache_creation_per_million: f64,
    /// `cacheReadPerMillion`.
    pub cache_read_per_million: f64,
    /// `source`.
    pub source: String,
}

/// A JSON number (`JSON.stringify` writes NaN and Infinity as `null`, as it does).
fn number(x: f64) -> Value {
    serde_json::Number::from_f64(x)
        .map(Value::Number)
        .unwrap_or(Value::Null)
}

fn rates_value(rates: &Rates) -> Value {
    let mut value = Map::new();
    value.insert("inputPerMillion".into(), number(rates.input_per_million));
    value.insert("outputPerMillion".into(), number(rates.output_per_million));
    value.insert(
        "cacheCreationPerMillion".into(),
        number(rates.cache_creation_per_million),
    );
    value.insert(
        "cacheReadPerMillion".into(),
        number(rates.cache_read_per_million),
    );
    Value::Object(value)
}

impl Pricing {
    /// The pricing object in the field order the TS table writes.
    pub fn to_value(&self) -> Value {
        let mut value = match rates_value(&self.rates) {
            Value::Object(value) => value,
            _ => unreachable!("rates_value builds an object"),
        };
        if let Some(tiers) = &self.service_tiers {
            let mut map = Map::new();
            for (name, rates) in tiers {
                map.insert(name.clone(), rates_value(rates));
            }
            value.insert("serviceTiers".into(), Value::Object(map));
        }
        Value::Object(value)
    }
}

impl Resolution {
    /// `{pricing, source}` as `getModelPricingWithSource` returns it.
    pub fn to_value(&self) -> Value {
        let mut value = Map::new();
        value.insert("pricing".into(), self.pricing.to_value());
        value.insert(
            "source".into(),
            Value::String(self.source.as_str().to_owned()),
        );
        Value::Object(value)
    }
}

impl AnalyticsRates {
    /// The 5-field row `defaultAccountAnalyticsPricing` returns.
    pub fn to_value(&self) -> Value {
        let mut value = Map::new();
        value.insert("inputPerMillion".into(), number(self.input_per_million));
        value.insert("outputPerMillion".into(), number(self.output_per_million));
        value.insert(
            "cacheCreationPerMillion".into(),
            number(self.cache_creation_per_million),
        );
        value.insert(
            "cacheReadPerMillion".into(),
            number(self.cache_read_per_million),
        );
        value.insert("source".into(), Value::String(self.source.clone()));
        Value::Object(value)
    }
}

/// `UNKNOWN_MODEL_PRICING`: $3 / $15 with the standard Anthropic cache multipliers.
fn unknown_model_pricing() -> Pricing {
    Pricing {
        rates: Rates {
            input_per_million: 3.0,
            output_per_million: 15.0,
            cache_creation_per_million: 3.75,
            cache_read_per_million: 0.3,
        },
        service_tiers: None,
    }
}

// --------------------------------------------------------------- normalization

/// `stripProviderPrefix` (`model-pricing.ts`): everything after the first `/`.
pub fn strip_provider_prefix(model: &str) -> String {
    let trimmed = model.trim();
    match trimmed.find('/') {
        Some(index) if index > 0 => trimmed[index + 1..].to_owned(),
        _ => trimmed.to_owned(),
    }
}

/// `normalizeModelName`: the provider prefix stripped and lowercased.
pub fn normalize_model_name(model: &str) -> String {
    strip_provider_prefix(model).to_lowercase()
}

/// One JS-style global replace of `-\d{8}` occurrences: `require_thinking` selects the
/// `(?=-thinking(?:$|:))` lookahead, else `(?=$|:)`.
fn remove_date_segments(text: &str, require_thinking: bool) -> String {
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < bytes.len() {
        let matched = bytes[i] == b'-'
            && i + 9 <= bytes.len()
            && bytes[i + 1..i + 9].iter().all(u8::is_ascii_digit)
            && {
                let after = i + 9;
                if require_thinking {
                    text[after..].starts_with("-thinking")
                        && (after + "-thinking".len() == bytes.len()
                            || bytes[after + "-thinking".len()] == b':')
                } else {
                    after == bytes.len() || bytes[after] == b':'
                }
            };
        if matched {
            i += 9;
            continue;
        }
        let piece = text[i..].chars().next().expect("index at a char boundary");
        out.push(piece);
        i += piece.len_utf8();
    }
    out
}

/// `stripClaudeDateSuffix` / `stripDateSuffix`: drop `-\d{8}` before `-thinking`, the end or a
/// `:` (Claude session ids can carry the date before a `-thinking` segment). Claude ids only —
/// other families keep their date-like segments.
pub fn strip_claude_date_suffix(model: &str) -> String {
    if !model.starts_with("claude-") {
        return model.to_owned();
    }
    remove_date_segments(&remove_date_segments(model, true), false)
}

/// The effort words `stripEffortSuffix` removes, in the regex's alternation order.
const EFFORT_SUFFIXES: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];

/// `stripEffortSuffix`: drop a trailing `-(low|medium|high|xhigh|max)` reasoning-effort segment
/// (Claude ids only; effort is a request setting, not a price).
pub fn strip_effort_suffix(model: &str) -> String {
    if !model.starts_with("claude-") {
        return model.to_owned();
    }
    let bytes = model.as_bytes();
    let mut out = String::with_capacity(model.len());
    let mut i = 0;
    while i < bytes.len() {
        let mut matched = None;
        if bytes[i] == b'-' {
            for effort in EFFORT_SUFFIXES {
                let end = i + 1 + effort.len();
                if end <= bytes.len()
                    && &model[i + 1..end] == effort
                    && (end == bytes.len()
                        || bytes[end] == b':'
                        || (model[end..].starts_with("-thinking")
                            && (end + "-thinking".len() == bytes.len()
                                || bytes[end + "-thinking".len()] == b':')))
                {
                    matched = Some(end);
                    break;
                }
            }
        }
        if let Some(end) = matched {
            i = end;
            continue;
        }
        let piece = model[i..].chars().next().expect("index at a char boundary");
        out.push(piece);
        i += piece.len_utf8();
    }
    out
}

/// `getLookupCandidates` (`model-pricing.ts`): normalized, base-before-`:`, then their
/// date-stripped and effort-stripped variants, first occurrence kept.
pub fn lookup_candidates(model: &str) -> Vec<String> {
    let normalized = normalize_model_name(model);
    let base = normalized.split(':').next().unwrap_or_default().to_owned();
    let mut candidates = vec![normalized.clone()];
    if base != normalized {
        candidates.push(base.clone());
    }
    for value in [normalized, base] {
        let date_stripped = strip_claude_date_suffix(&value);
        for variant in [date_stripped.clone(), strip_effort_suffix(&date_stripped)] {
            if !candidates.contains(&variant) {
                candidates.push(variant);
            }
        }
    }
    candidates
}

/// `normalizeId` (`pricing-resolver.ts`): trimmed and lowercased.
pub fn normalize_id(value: &str) -> String {
    value.trim().to_lowercase()
}

/// `hasProviderContext`: an explicit provider option (non-empty, as JavaScript truthiness reads
/// it) or a `provider/…` prefix on the model.
fn has_provider_context(model: &str, provider: Option<&str>) -> bool {
    provider.is_some_and(|p| !p.is_empty()) || model.trim().find('/').is_some_and(|index| index > 0)
}

/// `getDirectOrAliasPricing`.
fn direct_or_alias_pricing(table: &Table, model: &str) -> Option<Pricing> {
    if let Some(direct) = table.direct(model) {
        return Some(direct.clone());
    }
    for candidate in lookup_candidates(model) {
        if let Some(pricing) = table.normalized(&candidate) {
            return Some(pricing.clone());
        }
        if let Some(alias) = table.alias(&candidate)
            && let Some(pricing) = table.normalized(alias)
        {
            return Some(pricing.clone());
        }
    }
    None
}

/// `getCcsStaticPricing`.
fn ccs_static_pricing(table: &Table, model: &str) -> Option<Pricing> {
    if let Some(pricing) = direct_or_alias_pricing(table, model) {
        return Some(pricing);
    }
    let providerless = strip_provider_prefix(model);
    if providerless != model.trim() {
        return direct_or_alias_pricing(table, &providerless);
    }
    None
}

/// `getCcsPolicyOverridePricing`: aliases (and non-exact variants) only, so a CCS policy price
/// beats a provider route's models.dev rate.
fn ccs_policy_override_pricing(table: &Table, model: &str) -> Option<Pricing> {
    let providerless = strip_provider_prefix(model);
    let normalized = normalize_model_name(&providerless);
    for candidate in lookup_candidates(&providerless) {
        if let Some(alias) = table.alias(&candidate)
            && let Some(pricing) = table.normalized(alias)
        {
            return Some(pricing.clone());
        }
        if candidate != normalized
            && let Some(pricing) = table.normalized(&candidate)
        {
            return Some(pricing.clone());
        }
    }
    None
}

/// `applyServiceTier`.
fn apply_service_tier(pricing: Pricing, tier: Option<&str>) -> Pricing {
    let Some(tier) = tier.filter(|tier| !tier.is_empty()) else {
        return pricing;
    };
    match pricing.service_tiers.as_ref().and_then(|tiers| {
        tiers
            .iter()
            .find(|(name, _)| name == tier)
            .map(|(_, rates)| *rates)
    }) {
        Some(rates) => Pricing {
            rates,
            service_tiers: pricing.service_tiers,
        },
        None => pricing,
    }
}

/// The resolved provider/model/rates from the models.dev cache.
struct ModelsDevMatch {
    rates: Rates,
}

/// The TS module surface over one cache file: every lookup reads the cache (memoised on the
/// file's identity) exactly as `getCachedModelsDevRegistry` does.
pub struct Resolver {
    cache: RegistryCache,
    /// `memoiseAccountAnalyticsPricing`'s map, once the memoised entrypoint is used.
    analytics_memo: HashMap<String, Option<AnalyticsRates>>,
}

impl Resolver {
    /// A resolver over `<ccs_dir>/models-dev-registry-cache.json`.
    pub fn new(ccs_dir: &Path) -> Self {
        Resolver::over(super::models_dev::cache_path(ccs_dir))
    }

    /// A resolver over one cache file path.
    pub fn over(path: impl Into<std::path::PathBuf>) -> Self {
        Resolver {
            cache: RegistryCache::new(path.into()),
            analytics_memo: HashMap::new(),
        }
    }

    /// The bound cache (for refresh/clear/identity).
    pub fn cache(&mut self) -> &mut RegistryCache {
        &mut self.cache
    }

    fn cached(&mut self) -> Option<&CachedRegistry> {
        self.cache.read(true, 0.0)
    }

    /// `resolveModelsDevPricing` over the bound cache (stale reads allowed, as every TS
    /// resolution path allows them).
    fn resolve_models_dev(
        &mut self,
        model: &str,
        provider: Option<&str>,
    ) -> Option<ModelsDevMatch> {
        let (prefixed_provider, model_id) = super::models_dev::split_provider_prefix(model);
        let provider = provider.or(prefixed_provider.as_deref());
        let cached = self.cached()?;
        let found = match provider {
            Some(provider) => resolve_provider_model(cached, provider, &model_id),
            None => resolve_unambiguous_model(cached, &model_id),
        };
        found.map(|(_, _, rates)| ModelsDevMatch { rates })
    }

    /// `resolveBasePricing`.
    fn resolve_base_pricing(&mut self, model: &str, provider: Option<&str>) -> Resolution {
        let table = Table::get();
        if has_provider_context(model, provider) {
            if let Some(pricing) = ccs_policy_override_pricing(table, model) {
                return Resolution {
                    pricing,
                    source: Source::Builtin,
                };
            }
            if let Some(found) = self.resolve_models_dev(model, provider) {
                return Resolution {
                    pricing: Pricing {
                        rates: found.rates,
                        service_tiers: None,
                    },
                    source: Source::ModelsDev,
                };
            }
        }
        if let Some(pricing) = ccs_static_pricing(table, model) {
            return Resolution {
                pricing,
                source: Source::Builtin,
            };
        }
        if let Some(found) = self.resolve_models_dev(model, provider) {
            return Resolution {
                pricing: Pricing {
                    rates: found.rates,
                    service_tiers: None,
                },
                source: Source::ModelsDev,
            };
        }
        for candidate in lookup_candidates(model) {
            for (key, pricing) in table.normalized_entries() {
                if candidate.ends_with(key.as_str()) {
                    return Resolution {
                        pricing: pricing.clone(),
                        source: Source::Builtin,
                    };
                }
            }
        }
        Resolution {
            pricing: unknown_model_pricing(),
            source: Source::Fallback,
        }
    }

    /// `getModelPricingWithSource`.
    pub fn get_model_pricing_with_source(
        &mut self,
        model: &str,
        provider: Option<&str>,
        service_tier: Option<&str>,
    ) -> Resolution {
        let resolved = self.resolve_base_pricing(model, provider);
        Resolution {
            pricing: apply_service_tier(resolved.pricing, service_tier),
            source: resolved.source,
        }
    }

    /// `defaultAccountAnalyticsPricing`: the rates plus their source, or `None` unless all four
    /// are finite and non-negative.
    pub fn default_account_analytics_pricing(
        &mut self,
        model: &str,
        provider: Option<&str>,
    ) -> Option<AnalyticsRates> {
        let resolution = self.get_model_pricing_with_source(model, provider, None);
        let rates = AnalyticsRates {
            input_per_million: resolution.pricing.rates.input_per_million,
            output_per_million: resolution.pricing.rates.output_per_million,
            cache_creation_per_million: resolution.pricing.rates.cache_creation_per_million,
            cache_read_per_million: resolution.pricing.rates.cache_read_per_million,
            source: resolution.source.as_str().to_owned(),
        };
        [
            rates.input_per_million,
            rates.output_per_million,
            rates.cache_creation_per_million,
            rates.cache_read_per_million,
        ]
        .iter()
        .all(|rate| rate.is_finite() && *rate >= 0.0)
        .then_some(rates)
    }

    /// `memoiseAccountAnalyticsPricing(defaultAccountAnalyticsPricing)`: per `provider\0model`
    /// memoisation, cleared whole at 4096 entries, returning a fresh copy like the TS spread.
    pub fn memoised_default_account_analytics_pricing(
        &mut self,
        model: &str,
        provider: Option<&str>,
    ) -> Option<AnalyticsRates> {
        let key = format!("{}\0{model}", provider.unwrap_or(""));
        if !self.analytics_memo.contains_key(&key) {
            if self.analytics_memo.len() >= 4096 {
                self.analytics_memo.clear();
            }
            let value = self.default_account_analytics_pricing(model, provider);
            self.analytics_memo.insert(key.clone(), value);
        }
        self.analytics_memo.get(&key).cloned().flatten()
    }

    /// The pricing facts table the owned Analytics read consumes: `model\0route` → the analytics
    /// rates row (or null), built over the store dictionary's models and routes the way
    /// `rust-analytics-owned-input.ts` forwards them today.
    pub fn facts_table(&mut self, models: &[String], routes: &[String]) -> Map<String, Value> {
        let mut table = Map::new();
        for model in models {
            for route in routes
                .iter()
                .map(|route| Some(route.as_str()))
                .chain(std::iter::once(None))
            {
                let key = format!("{model}\0{}", route.unwrap_or(""));
                let value = self
                    .default_account_analytics_pricing(model, route)
                    .map(|rates| rates.to_value())
                    .unwrap_or(Value::Null);
                table.insert(key, value);
            }
        }
        table
    }

    /// `calculateCost`: the four token terms in the TS evaluation order.
    pub fn calculate_cost(
        &mut self,
        usage: &[f64; 4],
        model: &str,
        provider: Option<&str>,
        service_tier: Option<&str>,
    ) -> f64 {
        let rates = self
            .get_model_pricing_with_source(model, provider, service_tier)
            .pricing
            .rates;
        (usage[0] / 1_000_000.0) * rates.input_per_million
            + (usage[1] / 1_000_000.0) * rates.output_per_million
            + (usage[2] / 1_000_000.0) * rates.cache_creation_per_million
            + (usage[3] / 1_000_000.0) * rates.cache_read_per_million
    }

    /// `getKnownModels`: table ids then models.dev ids, first occurrence kept.
    pub fn get_known_models(&mut self) -> Vec<String> {
        let mut seen = std::collections::HashSet::new();
        let mut models = Vec::new();
        for key in super::table::known_table_models() {
            if seen.insert(key.clone()) {
                models.push(key);
            }
        }
        if let Some(cached) = self.cached() {
            for id in super::models_dev::get_known_models_dev_models(cached) {
                if seen.insert(id.clone()) {
                    models.push(id);
                }
            }
        }
        models
    }

    /// `hasCustomPricing`.
    pub fn has_custom_pricing(&mut self, model: &str, provider: Option<&str>) -> bool {
        ccs_static_pricing(Table::get(), model).is_some()
            || self.resolve_models_dev(model, provider).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cacheless() -> Resolver {
        let root = tempfile::tempdir().expect("tempdir");
        Resolver::over(root.path().join("no-cache.json"))
    }

    #[test]
    fn the_analytics_memo_repeats_answers_and_survives_the_cap() {
        let mut resolver = cacheless();
        let first = resolver.memoised_default_account_analytics_pricing("claude-opus-4-6", None);
        let second = resolver.memoised_default_account_analytics_pricing("claude-opus-4-6", None);
        assert_eq!(first, second);
        assert!(first.is_some());
        // Overfill past the cap: the map clears whole, and answers stay correct after it.
        for i in 0..5000 {
            resolver.memoised_default_account_analytics_pricing(&format!("model-{i}"), None);
        }
        let again = resolver.memoised_default_account_analytics_pricing("claude-opus-4-6", None);
        assert_eq!(again, first);
    }

    #[test]
    fn provider_context_gates_the_policy_and_models_dev_paths() {
        let mut resolver = cacheless();
        // No provider and no prefix: the static table answers directly.
        assert_eq!(
            resolver
                .get_model_pricing_with_source("claude-sonnet-4-5", None, None)
                .source,
            Source::Builtin
        );
        // An empty provider option is falsy in JavaScript: it must not open the provider paths.
        assert_eq!(
            resolver
                .get_model_pricing_with_source("claude-sonnet-4-5", Some(""), None)
                .source,
            Source::Builtin
        );
        // A provider/ prefix on the model opens them even without an explicit option.
        assert_eq!(
            resolver
                .get_model_pricing_with_source("anthropic/claude-sonnet-4-5", None, None)
                .source,
            Source::Builtin
        );
    }

    #[test]
    fn the_candidates_strip_only_claude_date_and_effort_segments() {
        assert_eq!(
            strip_claude_date_suffix("claude-opus-4-6-20260101-thinking"),
            "claude-opus-4-6-thinking"
        );
        assert_eq!(
            strip_claude_date_suffix("claude-opus-4-6-20260101:session"),
            "claude-opus-4-6:session"
        );
        assert_eq!(
            strip_claude_date_suffix("claude-opus-4-6-20260101"),
            "claude-opus-4-6"
        );
        assert_eq!(
            strip_claude_date_suffix("minimax-m2-20260101"),
            "minimax-m2-20260101"
        );
        assert_eq!(
            strip_claude_date_suffix("claude-20260101-x"),
            "claude-20260101-x"
        );
        assert_eq!(strip_effort_suffix("claude-opus-5-high"), "claude-opus-5");
        assert_eq!(
            strip_effort_suffix("claude-opus-4-6-medium-thinking"),
            "claude-opus-4-6-thinking"
        );
        assert_eq!(
            strip_effort_suffix("claude-opus-5-thinking-high"),
            "claude-opus-5-thinking"
        );
        assert_eq!(
            strip_effort_suffix("gemini-3-pro-high"),
            "gemini-3-pro-high"
        );
        assert_eq!(
            strip_effort_suffix("claude-opus-5-maximum"),
            "claude-opus-5-maximum"
        );
        assert_eq!(strip_effort_suffix("claude-opus-5-max:"), "claude-opus-5:");
    }
}
