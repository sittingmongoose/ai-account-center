//! The store-fed Analytics activity answer (DESIGN-M3-COMPLETE 1.5, A2): the port of
//! `src/store/activity-answer.ts` (`rangeToSources`, `sessionCounts`, `storeActivityAnswer`),
//! `src/store/activity-coverage.ts` (`coverageOf`, `gridFromStore`),
//! `src/web-server/services/account-analytics-attribution.ts`,
//! `src/web-server/services/account-analytics-projection.ts` and
//! `src/web-server/services/account-analytics-anomalies.ts`.
//!
//! Inputs are the store's own `activityRange`/`activitySources` answers plus the live collector
//! state and the resolved pricing facts the TS front door supplies. The projection collates with
//! the one total collation in `aac_num::intl`: the CLDR root order `ascii_compare` proves against Node for printable
//! ASCII, extended to a total order for any UTF-8 so non-ASCII activity labels answer instead of
//! declining; every zone word comes from the envelope's proven offset segments. Anything else is a
//! named [`Decline`], and the TS computation answers.
use crate::decline::Decline;
use aac_num::{civil, ecma, intl};
use serde_json::{Map, Value, json};
use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};

type R<T> = Result<T, Decline>;
type Object = Map<String, Value>;

const HOUR_MS: f64 = 3_600_000.0;
const DAY_MS: f64 = 86_400_000.0;
/// Ranges up to this long are answered hourly; longer ones in local days (`HOURLY_RANGE_MS`).
pub(crate) const HOURLY_RANGE_MS: f64 = 92.0 * DAY_MS;

// The tools `rangeToSources` publishes (`activitySources`, seven; the projection's grid has
// nine), every tool the activity collector reads (`activityTools`, published as `tools` on
// provider and model rows) and the session targets it accepts (`cliTargets`) come from the
// tool table, in its order, so the port cannot drift from the scanner's kinds.
const MAX_SESSION_SAMPLE: usize = 50;
const MAX_NAMED_DAY_MODELS: usize = 12;
const MAX_PUBLISHED_MODELS: usize = 500;
const OTHER_MODELS: &str = "Other models";
const SYNTHETIC_MODEL_NAME: &str = "<synthetic>";

const TOKEN_FIELDS: [&str; 4] = [
    "inputTokens",
    "outputTokens",
    "cacheCreationTokens",
    "cacheReadTokens",
];
const FIELDS: [&str; 6] = [
    "inputTokens",
    "outputTokens",
    "cacheCreationTokens",
    "cacheReadTokens",
    "estimatedCostUsd",
    "fallbackCostUsd",
];
const RATE_KEYS: [&str; 5] = [
    "inputPerMillion",
    "outputPerMillion",
    "cacheCreationPerMillion",
    "cacheReadPerMillion",
    "source",
];

/// `USAGE_PROVIDER_ORDER`: the shared providers table's analytics order, then "other".
fn usage_provider_order() -> Vec<&'static str> {
    let mut rows: Vec<(u32, &'static str)> = aac_providers::dashboard_providers()
        .filter_map(|row| {
            row.analytics
                .as_ref()
                .map(|analytics| (analytics.order, row.id.as_str()))
        })
        .collect();
    rows.sort_by_key(|(order, _)| *order);
    let mut out: Vec<&'static str> = rows.into_iter().map(|(_, id)| id).collect();
    out.push("other");
    out
}

/// `USAGE_PROVIDER_LABELS`: the dashboard's provider labels.
fn provider_label(provider: &str) -> String {
    if provider == "other" {
        return "Other".to_owned();
    }
    aac_providers::provider(provider)
        .and_then(|row| row.analytics.as_ref())
        .map(|analytics| analytics.label.to_owned())
        .unwrap_or_else(|| provider.to_owned())
}

// ---------------------------------------------------------------- helpers

fn obj(value: &Value) -> R<&Object> {
    value.as_object().ok_or(Decline::Shape)
}
fn arr(value: Option<&Value>) -> R<&Vec<Value>> {
    value.and_then(Value::as_array).ok_or(Decline::Shape)
}
fn text(value: Option<&Value>) -> R<&str> {
    value.and_then(Value::as_str).ok_or(Decline::Shape)
}
fn num(value: Option<&Value>) -> R<f64> {
    value
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite())
        .ok_or(Decline::Number)
}
fn opt_num(value: Option<&Value>) -> R<Option<f64>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(_) => Ok(Some(num(value)?)),
    }
}
/// The projection's `finite`: a finite number at least zero, else zero.
fn finite(value: Option<&Value>) -> f64 {
    value
        .and_then(Value::as_f64)
        .filter(|n| n.is_finite() && *n >= 0.0)
        .unwrap_or(0.0)
}
/// A JavaScript number as JSON: integral safe values keep the integer spelling.
fn js_num(x: f64) -> Value {
    if x.fract() == 0.0 && x.abs() <= ecma::MAX_SAFE_INTEGER as f64 {
        #[allow(clippy::cast_possible_truncation)]
        let integral = x as i64;
        json!(integral)
    } else {
        json!(x)
    }
}
/// `nanoDollars`: `Math.round(value * 1e9) / 1e9`.
fn nano_dollars(value: f64) -> f64 {
    ecma::math_round(value * 1e9) / 1e9
}
/// An integral JavaScript time value (|t| <= 8.64e15).
fn time(value: f64) -> R<i64> {
    if value.is_finite() && value.fract() == 0.0 && value.abs() <= 8.64e15 {
        #[allow(clippy::cast_possible_truncation)]
        Ok(value as i64)
    } else {
        Err(Decline::Date)
    }
}
fn iso(ms: f64) -> R<String> {
    ecma::to_iso_string(time(ms)?).ok_or(Decline::Date)
}
/// `a.localeCompare(b)` for printable ASCII, extended to the one total deterministic order
/// in `aac_num::intl` ([`intl::collate`]): exactly `intl::ascii_compare` whenever both are
/// printable ASCII, so ASCII answers and parity goldens are unchanged, and any other
/// character above every printable-ASCII primary by code point. Non-ASCII labels and the
/// NUL-separated ranking keys no longer decline the projection sort.
fn cmp_str(a: &str, b: &str) -> R<Ordering> {
    Ok(intl::collate(a, b))
}
/// `hourEpoch`: `YYYY-MM-DD HH:00` as the UTC instant, else NaN (`None`).
fn hour_epoch(hour: &str) -> Option<f64> {
    let bytes = hour.as_bytes();
    if bytes.len() != 16
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b' '
        || bytes[13] != b':'
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
        || !bytes[11..13].iter().all(u8::is_ascii_digit)
        || &bytes[14..16] != b"00"
    {
        return None;
    }
    let spelled = format!("{}T{}:00Z", &hour[..10], &hour[11..13]);
    #[allow(clippy::cast_precision_loss)]
    ecma::parse_instant(&spelled).map(|ms| ms as f64)
}
/// `hourText`: the store's hour label of an instant.
fn hour_text(ms: f64) -> R<String> {
    Ok(iso(ms)?[..13].replace('T', " ") + ":00")
}
/// `validModelName`: at most 160 UTF-16 units, no control character.
fn valid_model_name(value: Option<&Value>) -> Option<&str> {
    let name = value.and_then(Value::as_str)?;
    if name.encode_utf16().count() > 160 || name.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}') {
        return None;
    }
    Some(name)
}

/// JavaScript `Map` with insertion order: `set` keeps an existing key's position.
struct OrderedMap<T> {
    keys: Vec<String>,
    values: HashMap<String, T>,
}
impl<T> OrderedMap<T> {
    fn new() -> Self {
        Self {
            keys: Vec::new(),
            values: HashMap::new(),
        }
    }
    fn get(&self, key: &str) -> Option<&T> {
        self.values.get(key)
    }
    fn get_mut(&mut self, key: &str) -> Option<&mut T> {
        self.values.get_mut(key)
    }
    fn set(&mut self, key: String, value: T) {
        if !self.values.contains_key(&key) {
            self.keys.push(key.clone());
        }
        self.values.insert(key, value);
    }
    fn entries(&self) -> Vec<(&str, &T)> {
        self.keys
            .iter()
            .filter_map(|key| self.values.get(key).map(|v| (key.as_str(), v)))
            .collect()
    }
    fn into_entries(self) -> Vec<(String, T)> {
        let mut values = self.values;
        self.keys
            .into_iter()
            .filter_map(|key| values.remove(&key).map(|v| (key, v)))
            .collect()
    }
}

// ---------------------------------------------------------------- attribution

/// `routeProvider`: the dashboard provider a route names, after trim/lowercase and `builtin:`.
fn route_provider(route: Option<&str>) -> Option<&'static str> {
    let mut key = route?.trim().to_lowercase();
    if let Some(rest) = key.strip_prefix("builtin:") {
        key = rest.to_owned();
    }
    aac_providers::usage_route_provider(&key)
}

/// True when a usage row is Kimi Code (OMP logs only). Keep in sync with
/// `aac_store::collect::is_kimi_code_usage` and the OMP scanner retag.
fn is_kimi_code_usage(model: Option<&str>, route: Option<&str>) -> bool {
    let route = route.unwrap_or("").trim().to_ascii_lowercase();
    let route = route.strip_prefix("builtin:").unwrap_or(&route);
    if matches!(route, "kimi-code" | "kimi") {
        return true;
    }
    let model = model.unwrap_or("").trim().to_ascii_lowercase();
    let name = model.rsplit('/').next().unwrap_or(&model);
    name == "k3"
        || name == "kimi-k3"
        || name == "kimi-for-coding"
        || name == "kimi-code"
        || name.starts_with("kimi-")
}

/// `usageProviderFor`: the tool's own provider, else Kimi Code, else the route, else the model's prefix, else other.
fn usage_provider_for(tool: &str, route: Option<&str>, model: Option<&str>) -> &'static str {
    if let Some(own) = aac_providers::tool_provider(tool) {
        return own;
    }
    if is_kimi_code_usage(model, route) {
        return "kimi-code";
    }
    if let Some(route) = route.filter(|r| !r.trim().is_empty()) {
        return route_provider(Some(route)).unwrap_or("other");
    }
    let name = model.unwrap_or("").trim();
    if let Some(slash) = name.find('/')
        && slash > 0
    {
        return route_provider(Some(&name[..slash])).unwrap_or("other");
    }
    "other"
}

fn tokens_of(row: &Object) -> f64 {
    TOKEN_FIELDS
        .iter()
        .map(|field| finite(row.get(*field)))
        .sum()
}

/// `splitRow`: divide one hourly or session row by the providers its breakdowns count under.
fn split_row(row: &Object, tool: &str) -> R<Vec<(&'static str, Object)>> {
    let mut groups: OrderedMap<Vec<Object>> = OrderedMap::new();
    for breakdown in row
        .get("modelBreakdowns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(breakdown) = breakdown.as_object() else {
            continue;
        };
        let provider = usage_provider_for(
            tool,
            breakdown.get("provider").and_then(Value::as_str),
            breakdown.get("modelName").and_then(Value::as_str),
        );
        let mut list = groups.get(provider).cloned().unwrap_or_default();
        list.push(breakdown.clone());
        groups.set(provider.to_owned(), list);
    }
    let home = aac_providers::tool_provider(tool).unwrap_or("other");
    let entries = groups.into_entries();
    if entries.is_empty() {
        return Ok(vec![(home, row.clone())]);
    }
    if entries.len() == 1 {
        let (provider, _) = entries.into_iter().next().ok_or(Decline::Shape)?;
        // The single provider is a static table word; a non-table provider cannot appear here
        // because usage_provider_for only returns table words.
        let provider = leak_provider(provider);
        return Ok(vec![(provider, row.clone())]);
    }
    let mut parts: Vec<(&'static str, Object)> = Vec::new();
    let mut covered = [0.0f64; 4];
    let mut covered_cost = 0.0;
    let mut covered_fallback = 0.0;
    for (provider, list) in &entries {
        let sum = |field: &str| -> f64 { list.iter().map(|item| finite(item.get(field))).sum() };
        let cost: f64 = sum("cost");
        let fallback_cost: f64 = sum("fallbackCost");
        let counted = list
            .iter()
            .all(|item| item.get("requestCount").is_some_and(Value::is_number));
        let mut part = row.clone();
        for (index, field) in TOKEN_FIELDS.iter().enumerate() {
            let value = sum(field);
            covered[index] += value;
            part.insert((*field).to_owned(), js_num(value));
        }
        part.insert("cost".into(), js_num(cost));
        part.insert("totalCost".into(), js_num(cost));
        let mut used: Vec<Value> = Vec::new();
        for item in list {
            let name = item.get("modelName").cloned().unwrap_or(Value::Null);
            if !used.contains(&name) {
                used.push(name);
            }
        }
        part.insert("modelsUsed".into(), Value::Array(used));
        part.insert(
            "modelBreakdowns".into(),
            Value::Array(
                list.iter()
                    .map(|item| Value::Object(item.clone()))
                    .collect(),
            ),
        );
        if fallback_cost > 0.0 {
            part.insert("fallbackCost".into(), js_num(fallback_cost));
        } else {
            part.remove("fallbackCost");
        }
        if part.contains_key("hour") {
            if counted {
                let requests: f64 = list
                    .iter()
                    .map(|item| finite(item.get("requestCount")))
                    .sum();
                part.insert("requestCount".into(), js_num(requests));
            } else {
                part.remove("requestCount");
            }
        }
        covered_cost += cost;
        covered_fallback += fallback_cost;
        parts.push((leak_provider(provider.clone()), part));
    }
    let residual: Vec<f64> = TOKEN_FIELDS
        .iter()
        .enumerate()
        .map(|(index, field)| (finite(row.get(*field)) - covered[index]).max(0.0))
        .collect();
    if residual.iter().sum::<f64>() > 0.0 {
        let cost =
            (finite(row.get("totalCost").or_else(|| row.get("cost"))) - covered_cost).max(0.0);
        let mut part = row.clone();
        for (index, field) in TOKEN_FIELDS.iter().enumerate() {
            part.insert((*field).to_owned(), js_num(residual[index]));
        }
        part.insert("cost".into(), js_num(cost));
        part.insert("totalCost".into(), js_num(cost));
        part.insert("modelsUsed".into(), json!([]));
        part.insert("modelBreakdowns".into(), json!([]));
        let fallback = (finite(row.get("fallbackCost")) - covered_fallback).max(0.0);
        if fallback > 0.0 {
            part.insert("fallbackCost".into(), js_num(fallback));
        } else {
            part.remove("fallbackCost");
        }
        part.remove("requestCount");
        parts.push((home, part));
    }
    Ok(parts)
}

/// The providers `usage_provider_for` returns are static table words; keep them `'static`.
fn leak_provider(provider: String) -> &'static str {
    // Every word a route, a tool or a model can name is a dashboard provider or an analytics
    // tool (or "other"); match it back to the table's static id.
    aac_providers::dashboard_providers()
        .map(|row| row.id.as_str())
        .chain(aac_providers::ANALYTICS_TOOLS.iter().map(|tool| tool.id))
        .find(|word| *word == provider)
        .unwrap_or("other")
}

/// One tool's rows for one provider (`AttributedResult`).
struct AttributedResult {
    tool: String,
    hourly: Vec<Object>,
    session: Vec<(Object, Object, bool)>,
}
/// All retained usage one provider served (`AttributedSource`).
struct AttributedSource {
    provider: String,
    fetched_at: String,
    data: Vec<AttributedResult>,
}

/// `attributeActivitySources`: regroup the tool snapshots by provider, in the dashboard order.
fn attribute_activity_sources(sources: &[SourceData]) -> R<Vec<AttributedSource>> {
    let mut by_provider: OrderedMap<AttributedSource> = OrderedMap::new();
    for source in sources {
        for data in &source.data {
            // One result per provider per data item, in first-appearance order.
            let mut seen: OrderedMap<usize> = OrderedMap::new();
            let mut results: Vec<AttributedResult> = Vec::new();
            let mut result_for = |provider: &str,
                                  results: &mut Vec<AttributedResult>,
                                  seen: &mut OrderedMap<usize>|
             -> R<usize> {
                if let Some(index) = seen.get(provider) {
                    return Ok(*index);
                }
                let index = results.len();
                results.push(AttributedResult {
                    tool: source.provider.clone(),
                    hourly: Vec::new(),
                    session: Vec::new(),
                });
                seen.set(provider.to_owned(), index);
                let fetched_at = source.fetched_at.clone();
                match by_provider.get_mut(provider) {
                    Some(existing) => {
                        if fetched_at < existing.fetched_at {
                            existing.fetched_at = fetched_at;
                        }
                        existing.data.push(AttributedResult {
                            tool: source.provider.clone(),
                            hourly: Vec::new(),
                            session: Vec::new(),
                        });
                    }
                    None => {
                        by_provider.set(
                            provider.to_owned(),
                            AttributedSource {
                                provider: provider.to_owned(),
                                fetched_at,
                                data: vec![AttributedResult {
                                    tool: source.provider.clone(),
                                    hourly: Vec::new(),
                                    session: Vec::new(),
                                }],
                            },
                        );
                    }
                }
                Ok(index)
            };
            for hour in &data.hourly {
                for (provider, part) in split_row(hour, &source.provider)? {
                    let index = result_for(provider, &mut results, &mut seen)?;
                    results[index].hourly.push(part);
                }
            }
            for session in &data.session {
                let parts = split_row(session, &source.provider)?;
                let mut best = 0usize;
                let mut best_tokens = tokens_of(&parts[0].1);
                for (index, (_, part)) in parts.iter().enumerate().skip(1) {
                    let tokens = tokens_of(part);
                    if tokens > best_tokens {
                        best = index;
                        best_tokens = tokens;
                    }
                }
                for (index, (provider, part)) in parts.into_iter().enumerate() {
                    let at = result_for(provider, &mut results, &mut seen)?;
                    results[at]
                        .session
                        .push((part, session.clone(), index == best));
                }
            }
            // Append this data item's results to their providers, in first-appearance order (move, no clone).
            for (provider, index) in seen.entries() {
                let result = results.get_mut(*index).ok_or(Decline::Shape)?;
                let source_entry = by_provider.get_mut(provider).ok_or(Decline::Shape)?;
                let slot = source_entry.data.last_mut().ok_or(Decline::Shape)?;
                *slot = AttributedResult {
                    tool: std::mem::take(&mut result.tool),
                    hourly: std::mem::take(&mut result.hourly),
                    session: std::mem::take(&mut result.session),
                };
            }
        }
    }
    let order = usage_provider_order();
    let mut out: Vec<AttributedSource> = Vec::new();
    for provider in &order {
        if let Some(source) = by_provider.get_mut(provider) {
            out.push(AttributedSource {
                provider: std::mem::take(&mut source.provider),
                fetched_at: std::mem::take(&mut source.fetched_at),
                data: std::mem::take(&mut source.data),
            });
        }
    }
    let mut rest: Vec<String> = by_provider
        .entries()
        .into_iter()
        .map(|(provider, _)| provider.to_owned())
        .filter(|provider| !order.contains(&provider.as_str()))
        .collect();
    rest.sort();
    for provider in rest {
        if let Some(source) = by_provider.get_mut(&provider) {
            out.push(AttributedSource {
                provider: std::mem::take(&mut source.provider),
                fetched_at: std::mem::take(&mut source.fetched_at),
                data: std::mem::take(&mut source.data),
            });
        }
    }
    Ok(out)
}

#[allow(dead_code)]
fn clone_source(source: &AttributedSource) -> AttributedSource {
    AttributedSource {
        provider: source.provider.clone(),
        fetched_at: source.fetched_at.clone(),
        data: source
            .data
            .iter()
            .map(|result| AttributedResult {
                tool: result.tool.clone(),
                hourly: result.hourly.clone(),
                session: result.session.clone(),
            })
            .collect(),
    }
}

// ---------------------------------------------------------------- pricing facts

/// One resolved rate set the front door's lookup answered (`AccountAnalyticsModelRates`).
#[derive(Clone, Debug)]
struct Rates {
    input: f64,
    output: f64,
    cache_creation: f64,
    cache_read: f64,
    source: String,
}
/// The resolved pricing facts, keyed `model\0route` (empty route is the undefined provider).
pub(crate) struct Pricing {
    rates: HashMap<String, Option<Rates>>,
}
impl Pricing {
    pub(crate) fn decode(value: Option<&Value>) -> R<Self> {
        let table = obj(value.ok_or(Decline::Shape)?)?;
        let mut rates = HashMap::new();
        for (key, value) in table {
            if value.is_null() {
                rates.insert(key.clone(), None);
                continue;
            }
            let row = obj(value)?;
            let rate = |field: &str| -> R<f64> {
                let n = num(row.get(field))?;
                if n >= 0.0 {
                    Ok(n)
                } else {
                    Err(Decline::Number)
                }
            };
            rates.insert(
                key.clone(),
                Some(Rates {
                    input: rate("inputPerMillion")?,
                    output: rate("outputPerMillion")?,
                    cache_creation: rate("cacheCreationPerMillion")?,
                    cache_read: rate("cacheReadPerMillion")?,
                    source: text(row.get("source"))?.to_owned(),
                }),
            );
        }
        Ok(Self { rates })
    }
    /// `pricing(model, provider)`: a missing pair is a missing fact, not a fallback rate.
    fn lookup(&self, model: &str, provider: Option<&str>) -> R<Option<Rates>> {
        let key = format!("{model}\0{}", provider.unwrap_or(""));
        self.rates.get(&key).cloned().ok_or(Decline::Unsupported)
    }
    fn value(rates: &Rates) -> Value {
        json!({
            "inputPerMillion": rates.input,
            "outputPerMillion": rates.output,
            "cacheCreationPerMillion": rates.cache_creation,
            "cacheReadPerMillion": rates.cache_read,
            "source": rates.source,
        })
    }
}

// ---------------------------------------------------------------- rangeToSources

/// One tool's merged rows (`SourceData`): hourly and session objects in the worker shape.
struct SourceData {
    provider: String,
    data: Vec<WorkerResult>,
    fetched_at: String,
}
struct WorkerResult {
    hourly: Vec<Object>,
    session: Vec<Object>,
}

/// `listed`: price a reader's own logged cost at list rates when the store has no estimate.
fn listed(pricing: &Pricing, route: &str, model: &str, tokens: [f64; 4]) -> R<(f64, f64)> {
    let resolved = pricing
        .lookup(model, if route.is_empty() { None } else { Some(route) })?
        .ok_or(Decline::Unsupported)?;
    let cost = (tokens[0] / 1e6) * resolved.input
        + (tokens[1] / 1e6) * resolved.output
        + (tokens[2] / 1e6) * resolved.cache_creation
        + (tokens[3] / 1e6) * resolved.cache_read;
    Ok((
        cost,
        if resolved.source == "fallback" {
            cost
        } else {
            0.0
        },
    ))
}

/// `rangeToSources`: the store's bucketed rows as the per-tool sources the projection reads.
fn range_to_sources(
    range: &Value,
    daily: bool,
    fetched_at: &str,
    pricing: &Pricing,
) -> R<Vec<SourceData>> {
    let range = obj(range)?;
    let tools = arr(Some(range.get("tools").ok_or(Decline::Shape)?))?;
    let routes = arr(Some(range.get("routes").ok_or(Decline::Shape)?))?;
    let models = arr(Some(range.get("models").ok_or(Decline::Shape)?))?;
    let rows = arr(Some(range.get("rows").ok_or(Decline::Shape)?))?;
    let word = |list: &Vec<Value>, index: Option<&Value>| -> R<String> {
        let index = index.and_then(Value::as_u64).ok_or(Decline::Shape)?;
        Ok(text(list.get(index as usize))?.to_owned())
    };
    let mut by_tool: OrderedMap<OrderedMap<Object>> = OrderedMap::new();
    let mut known_counts: HashMap<String, bool> = HashMap::new();
    for row in rows {
        let row = arr(Some(row))?;
        if row.len() != 14 {
            return Err(Decline::Shape);
        }
        let bucket = num(row.first())?;
        let tool = word(tools, row.get(2))?;
        let route = match row.get(3) {
            None | Some(Value::Null) => String::new(),
            Some(_) => word(routes, row.get(3))?,
        };
        let model = match row.get(4) {
            None | Some(Value::Null) => "unknown".to_owned(),
            Some(_) => word(models, row.get(4))?,
        };
        let tokens = [
            num(row.get(5))?,
            num(row.get(6))?,
            num(row.get(7))?,
            num(row.get(8))?,
        ];
        let logged_nano = opt_num(row.get(9))?;
        let cost_nano = opt_num(row.get(10))?;
        let fallback_nano = opt_num(row.get(11))?;
        let events = opt_num(row.get(12))?;
        let quality = match row.get(13) {
            Some(value) if value.is_number() => num(Some(value))?,
            _ => 0.0,
        };
        let (cost, fallback) = match cost_nano {
            None => {
                let priced = listed(pricing, &route, &model, tokens)?;
                let logged = logged_nano.unwrap_or(0.0) / 1e9;
                if logged > 0.0 { (logged, 0.0) } else { priced }
            }
            Some(cost) => (cost / 1e9, fallback_nano.unwrap_or(0.0) / 1e9),
        };
        // A day bucket is labelled by the first whole hour of its local day.
        let at = if daily {
            (bucket / HOUR_MS).ceil() * HOUR_MS
        } else {
            bucket
        };
        if by_tool.get(&tool).is_none() {
            by_tool.set(tool.clone(), OrderedMap::new());
        }
        let hours = by_tool.get_mut(&tool).ok_or(Decline::Shape)?;
        let at_key = format!("{at}");
        if hours.get(&at_key).is_none() {
            let mut hour = Object::new();
            hour.insert("hour".into(), json!(hour_text(at)?));
            hour.insert("source".into(), json!(format!("{tool}-store")));
            for field in TOKEN_FIELDS {
                hour.insert(field.into(), json!(0));
            }
            hour.insert("cost".into(), json!(0));
            hour.insert("totalCost".into(), json!(0));
            hour.insert("modelsUsed".into(), json!([]));
            hour.insert("modelBreakdowns".into(), json!([]));
            hour.insert("requestCount".into(), json!(0));
            hours.set(at_key.clone(), hour);
        }
        let hour = hours.get_mut(&at_key).ok_or(Decline::Shape)?;
        let mut breakdown = Object::new();
        breakdown.insert("modelName".into(), json!(model));
        if !route.is_empty() {
            breakdown.insert("provider".into(), json!(route));
        }
        breakdown.insert("inputTokens".into(), js_num(tokens[0]));
        breakdown.insert("outputTokens".into(), js_num(tokens[1]));
        breakdown.insert("cacheCreationTokens".into(), js_num(tokens[2]));
        breakdown.insert("cacheReadTokens".into(), js_num(tokens[3]));
        breakdown.insert("cost".into(), js_num(cost));
        if fallback > 0.0 {
            breakdown.insert("fallbackCost".into(), js_num(fallback));
        }
        if let Some(events) = events {
            breakdown.insert("requestCount".into(), js_num(events));
        }
        if quality != 0.0 {
            breakdown.insert("quality".into(), js_num(quality));
        }
        let breakdowns = hour
            .get_mut("modelBreakdowns")
            .and_then(Value::as_array_mut)
            .ok_or(Decline::Shape)?;
        let prior = breakdowns.iter_mut().find(|candidate| {
            candidate.get("modelName").and_then(Value::as_str) == Some(model.as_str())
                && candidate
                    .get("provider")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    == route
                && candidate
                    .get("quality")
                    .and_then(Value::as_f64)
                    .unwrap_or(0.0)
                    == quality
        });
        match prior {
            Some(prior) => {
                let prior = prior.as_object_mut().ok_or(Decline::Shape)?;
                let mut add = |field: &str, amount: f64| {
                    let next = finite(prior.get(field)) + amount;
                    prior.insert(field.to_owned(), js_num(next));
                };
                add("inputTokens", tokens[0]);
                add("outputTokens", tokens[1]);
                add("cacheCreationTokens", tokens[2]);
                add("cacheReadTokens", tokens[3]);
                add("cost", cost);
                if fallback > 0.0 {
                    let next = finite(prior.get("fallbackCost")) + fallback;
                    prior.insert("fallbackCost".into(), js_num(next));
                }
                match events {
                    None => {
                        prior.remove("requestCount");
                    }
                    Some(events) => {
                        if prior.get("requestCount").is_none() {
                            prior.remove("requestCount");
                        } else {
                            let next = finite(prior.get("requestCount")) + events;
                            prior.insert("requestCount".into(), js_num(next));
                        }
                    }
                }
            }
            None => breakdowns.push(Value::Object(breakdown)),
        }
        let models_used = hour
            .get_mut("modelsUsed")
            .and_then(Value::as_array_mut)
            .ok_or(Decline::Shape)?;
        if !models_used
            .iter()
            .any(|used| used.as_str() == Some(model.as_str()))
        {
            models_used.push(json!(model));
        }
        for (index, field) in TOKEN_FIELDS.iter().enumerate() {
            let next = finite(hour.get(*field)) + tokens[index];
            hour.insert((*field).to_owned(), js_num(next));
        }
        let next = finite(hour.get("cost")) + cost;
        hour.insert("cost".into(), js_num(next));
        let next = finite(hour.get("totalCost")) + cost;
        hour.insert("totalCost".into(), js_num(next));
        if fallback > 0.0 {
            let next = finite(hour.get("fallbackCost")) + fallback;
            hour.insert("fallbackCost".into(), js_num(next));
        }
        let count_key = format!("{tool}\0{at}");
        match events {
            None => {
                known_counts.insert(count_key, false);
            }
            Some(events) => {
                if known_counts.get(&count_key) != Some(&false) {
                    known_counts.insert(count_key.clone(), true);
                    let next = finite(hour.get("requestCount")) + events;
                    hour.insert("requestCount".into(), js_num(next));
                }
            }
        }
    }
    // Sessions: one row per sampled session, its parts priced at ingest already.
    let sessions = obj(range.get("sessions").ok_or(Decline::Shape)?)?;
    let sample = arr(Some(sessions.get("sample").ok_or(Decline::Shape)?))?;
    let mut sessions_by_tool: OrderedMap<Vec<Object>> = OrderedMap::new();
    for session in sample {
        let session = obj(session)?;
        let session_key = match session.get("sessionKey") {
            Some(Value::String(key)) if !key.is_empty() => key.clone(),
            _ => continue,
        };
        let tool = text(session.get("tool"))?.to_owned();
        let parts = arr(Some(session.get("parts").ok_or(Decline::Shape)?))?;
        let mut breakdowns: Vec<Value> = Vec::new();
        let mut sums = [0.0f64; 5];
        let mut used: Vec<Value> = Vec::new();
        for part in parts {
            let part = obj(part)?;
            let route = text(part.get("route")).unwrap_or("");
            let model = text(part.get("modelRaw"))?.to_owned();
            let tokens = [
                finite(part.get("inputTokens")),
                finite(part.get("outputTokens")),
                finite(part.get("cacheCreationTokens")),
                finite(part.get("cacheReadTokens")),
            ];
            let cost = opt_num(part.get("costUsd"))?
                .or(opt_num(part.get("loggedCostUsd"))?)
                .unwrap_or(0.0);
            let fallback = opt_num(part.get("fallbackCostUsd"))?.unwrap_or(0.0);
            let mut breakdown = Object::new();
            breakdown.insert("modelName".into(), json!(model));
            if !route.is_empty() {
                breakdown.insert("provider".into(), json!(route));
            }
            breakdown.insert("inputTokens".into(), js_num(tokens[0]));
            breakdown.insert("outputTokens".into(), js_num(tokens[1]));
            breakdown.insert("cacheCreationTokens".into(), js_num(tokens[2]));
            breakdown.insert("cacheReadTokens".into(), js_num(tokens[3]));
            breakdown.insert("cost".into(), js_num(cost));
            if fallback != 0.0 {
                breakdown.insert("fallbackCost".into(), js_num(fallback));
            }
            for (index, value) in tokens.iter().enumerate() {
                sums[index] += value;
            }
            sums[4] += cost;
            if !used
                .iter()
                .any(|name| name.as_str() == Some(model.as_str()))
            {
                used.push(json!(model));
            }
            breakdowns.push(Value::Object(breakdown));
        }
        let mut row = Object::new();
        row.insert("sessionId".into(), json!(session_key));
        row.insert("projectPath".into(), json!(""));
        row.insert("source".into(), json!(format!("{tool}-store")));
        row.insert("inputTokens".into(), js_num(sums[0]));
        row.insert("outputTokens".into(), js_num(sums[1]));
        row.insert("cacheCreationTokens".into(), js_num(sums[2]));
        row.insert("cacheReadTokens".into(), js_num(sums[3]));
        row.insert("cost".into(), js_num(sums[4]));
        row.insert("totalCost".into(), js_num(sums[4]));
        row.insert(
            "firstActivity".into(),
            json!(iso(num(session.get("firstMs"))?)?),
        );
        row.insert(
            "lastActivity".into(),
            json!(iso(num(session.get("lastMs"))?)?),
        );
        row.insert("versions".into(), json!([]));
        row.insert("modelsUsed".into(), Value::Array(used));
        row.insert("modelBreakdowns".into(), Value::Array(breakdowns));
        if let Some(target) = session
            .get("target")
            .filter(|v| crate::spread::truthy(Some(v)))
        {
            row.insert("target".into(), target.clone());
        }
        if sessions_by_tool.get(&tool).is_none() {
            sessions_by_tool.set(tool.clone(), Vec::new());
        }
        let list = sessions_by_tool.get_mut(&tool).ok_or(Decline::Shape)?;
        list.push(row);
    }
    let mut out: Vec<SourceData> = Vec::new();
    for tool in aac_providers::activity_sources() {
        let hours = by_tool.get(tool);
        let sessions = sessions_by_tool.get(tool).map(Vec::as_slice).unwrap_or(&[]);
        if hours.is_none() && sessions.is_empty() {
            continue;
        }
        let mut hourly: Vec<Object> = hours
            .map(|hours| {
                hours
                    .entries()
                    .into_iter()
                    .map(|(_, hour)| hour.clone())
                    .collect()
            })
            .unwrap_or_default();
        for hour in &mut hourly {
            let label = text(hour.get("hour"))?;
            let epoch = hour_epoch(label).ok_or(Decline::Date)?;
            if known_counts.get(&format!("{tool}\0{epoch}")) == Some(&false) {
                hour.remove("requestCount");
            }
        }
        let event_count: f64 = hourly
            .iter()
            .map(|hour| finite(hour.get("requestCount")))
            .sum();
        let mut result = Object::new();
        result.insert("daily".into(), json!([]));
        result.insert("monthly".into(), json!([]));
        result.insert(
            "hourly".into(),
            Value::Array(hourly.into_iter().map(Value::Object).collect()),
        );
        result.insert(
            "session".into(),
            Value::Array(sessions.iter().cloned().map(Value::Object).collect()),
        );
        result.insert("eventCount".into(), js_num(event_count));
        out.push(SourceData {
            provider: tool.to_owned(),
            data: vec![WorkerResult {
                hourly: result
                    .get("hourly")
                    .and_then(Value::as_array)
                    .ok_or(Decline::Shape)?
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect(),
                session: result
                    .get("session")
                    .and_then(Value::as_array)
                    .ok_or(Decline::Shape)?
                    .iter()
                    .filter_map(|v| v.as_object().cloned())
                    .collect(),
            }],
            fetched_at: fetched_at.to_owned(),
        });
    }
    Ok(out)
}

/// `sessionCounts`: per-provider session counts from the store's signatures.
fn session_counts(range: &Value) -> R<HashMap<String, f64>> {
    let sessions = obj(obj(range)?.get("sessions").ok_or(Decline::Shape)?)?;
    let signatures = arr(Some(sessions.get("signatures").ok_or(Decline::Shape)?))?;
    let mut counts: HashMap<String, f64> = HashMap::new();
    for signature in signatures {
        let signature = obj(signature)?;
        let tool = text(signature.get("tool"))?;
        let pairs = arr(Some(signature.get("pairs").ok_or(Decline::Shape)?))?;
        let mut providers: Vec<&'static str> = Vec::new();
        for pair in pairs {
            let pair = arr(Some(pair))?;
            let route = pair.first().and_then(Value::as_str);
            let model = pair.get(1).and_then(Value::as_str);
            let provider = usage_provider_for(tool, route, model);
            if !providers.contains(&provider) {
                providers.push(provider);
            }
        }
        if providers.is_empty() {
            providers.push(usage_provider_for(tool, None, None));
        }
        let count = num(signature.get("count"))?;
        for provider in providers {
            *counts.entry(provider.to_owned()).or_insert(0.0) += count;
        }
    }
    Ok(counts)
}

// ---------------------------------------------------------------- coverage

/// `scanFailureText`: the fixed sentences the grid shows for a read's cause.
fn scan_failure_text(cause: &str) -> &'static str {
    match cause {
        "agent_missing" => "agent missing",
        "source_limit" => "some usage log sources exceeded the scan limit",
        "freshness_unverified" => {
            "the independent activity check is incomplete; current usage coverage is unverified"
        }
        "ssh_unreachable" => "the computer did not answer over ssh",
        "ssh_auth" => "the computer refused this account's ssh key",
        "ssh_host_key" => {
            "the computer's ssh host key is not the one on record (unknown or changed), so ssh would not connect"
        }
        "runtime_missing:store_alias" => {
            "no working Python 3.9 or later on this computer (only the Microsoft Store alias, which is not Python)"
        }
        "runtime_missing" => "no working Python 3.9 or later on this computer",
        "helper_failed" => "the usage reader stopped with an error on this computer",
        "unreadable_folder" => "a folder searched for logs could not be read",
        "read_failed" => "its logs were found but could not be read",
        "unsafe_root" => "a saved usage log source has an unsafe root",
        "bad_mapping" => "a saved generic JSONL source has an invalid field mapping",
        "no_local_usage_records" => "the tool keeps no local usage records",
        "timeout" => "the scan took longer than its time limit",
        "bad_response" => "the computer's answer was not a usage report",
        "helper_damaged" => {
            "the packaged analytics helper failed its integrity check, so no computer was scanned; reinstall AI Account Center"
        }
        _ => "the scan failed",
    }
}

/// `gridFromStore`: the "Included usage" grid from the store's last reads.
pub(crate) fn grid_from_store(history: &Value) -> R<Value> {
    let history = obj(history)?;
    let sources = arr(Some(history.get("sources").ok_or(Decline::Shape)?))?;
    let mut grid: Vec<Value> = Vec::new();
    for source in sources {
        let source = obj(source)?;
        let tool = text(source.get("tool"))?;
        if !aac_providers::activity_tools().any(|id| id == tool) {
            continue;
        }
        let state_word = text(source.get("state")).unwrap_or("");
        let cause = source.get("cause").filter(|v| !v.is_null());
        let state = match state_word {
            _ if cause.and_then(Value::as_str) == Some("agent_missing") => "unavailable",
            "partial" if cause.and_then(Value::as_str) == Some("newer_activity") => "stale",
            "ok" => "ok",
            "no_usage" => "no_usage",
            "partial" | "cached" => "cached",
            "scanning" => "scanning",
            "not_installed" | "installed_no_logs" | "no_reader" | "unreadable" | "scan_failed" => {
                state_word
            }
            _ => "unavailable",
        };
        let last_scan = match source.get("lastFullReadAt").filter(|v| !v.is_null()) {
            Some(value) => value.clone(),
            None => {
                if matches!(
                    state,
                    "ok" | "no_usage" | "not_installed" | "installed_no_logs"
                ) {
                    source.get("lastReadAt").cloned().unwrap_or(Value::Null)
                } else {
                    Value::Null
                }
            }
        };
        let mut detail = match cause.and_then(Value::as_str) {
            Some("newer_activity") => {
                let clock = source.get("freshness");
                let date = |key: &str, fallback: &str| -> R<String> {
                    match opt_num(clock.and_then(|f| f.get(key)))? {
                        Some(ms) => iso(ms),
                        None => Ok(fallback.to_owned()),
                    }
                };
                let measured = date("newestDataMs", "no measured usage date")?;
                let activity = date("newestActivityMs", "unknown")?;
                Some(format!(
                    "usage is stale: newest measured data {measured}; newer activity {activity}; activity does not establish token counts or cost"
                ))
            }
            Some(cause) => Some(scan_failure_text(cause).to_owned()),
            None if state == "unavailable" => Some("the last read failed".into()),
            None => None,
        };
        let unmeasured = source
            .get("unmeasuredFiles")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0);
        // The scanner's own disclosures (wave 82): measured numbers, unverified coverage.
        let disclosed = |key: &str, needle: &str, note: &str| -> Option<String> {
            if source.get(key).and_then(Value::as_bool) != Some(true) {
                return None;
            }
            (!detail.as_deref().is_some_and(|text| text.contains(needle))).then(|| note.to_owned())
        };
        let notes = [
            disclosed(
                "unreadUsage",
                "usage-shaped record",
                "a usage-shaped record could not be read; its tokens are not counted until a later scan reads it",
            ),
            disclosed(
                "estimatedClock",
                "no event timestamp",
                "some usage records carry no event timestamp; their file times are not used as usage dates",
            ),
        ];
        for note in notes.into_iter().flatten() {
            detail = Some(match detail {
                Some(detail) => format!("{detail}; {note}"),
                None => note,
            });
        }
        if let Some(count) = unmeasured {
            let files = if count == 1 {
                "file exposes"
            } else {
                "files expose"
            };
            let note = format!(
                "{count} local chat data {files} no numeric usage; their tokens and cost are not counted"
            );
            detail = Some(match detail {
                Some(detail) => format!("{detail}; {note}"),
                None => note,
            });
        }
        // Records the reader could not read beside readable ones: the readable numbers stand
        // and the page names the count (the Devin case). Whole-source failures carry their own
        // failure text already, so they keep it alone.
        let unreadable_records = source
            .get("unreadableRecords")
            .and_then(Value::as_u64)
            .filter(|n| *n > 0);
        // A read failure keeps its failure text alone (r1 Medium): the mixed-record note never
        // appears on a cause of `read_failed`, even when verified cells already existed.
        if let Some(count) = unreadable_records
            && !matches!(state, "unreadable" | "scan_failed" | "unavailable")
            && cause.and_then(Value::as_str) != Some("read_failed")
        {
            let records = if count == 1 {
                "record unreadable"
            } else {
                "records unreadable"
            };
            let note = format!("{count} {records}");
            detail = Some(match detail {
                Some(detail) => format!("{detail}; {note}"),
                None => note,
            });
        }
        let mut row = json!({
            "tool": tool,
            "host": text(source.get("host"))?,
            "state": state,
            "lastScanAt": last_scan,
            "rowCount": 0,
            "detail": detail,
        });
        if let Some(cause) = cause {
            row.as_object_mut()
                .ok_or(Decline::Shape)?
                .insert("cause".into(), cause.clone());
        }
        if let Some(count) = unmeasured {
            row["unmeasuredFiles"] = json!(count);
        }
        if let Some(count) = unreadable_records {
            row["unreadableRecords"] = json!(count);
        }
        grid.push(row);
    }
    Ok(Value::Array(grid))
}

/// `coverageOf`: what the store holds per computer and source, for the page.
fn coverage_of(sources_state: &Value) -> R<Value> {
    let sources_state = obj(sources_state)?;
    let sources = arr(Some(sources_state.get("sources").ok_or(Decline::Shape)?))?;
    let computers = arr(Some(sources_state.get("computers").ok_or(Decline::Shape)?))?;
    let at = |value: Option<&Value>| -> R<Value> {
        match opt_num(value)? {
            None => Ok(Value::Null),
            Some(ms) => Ok(json!(iso(ms)?)),
        }
    };
    let mut backfill_rows: Vec<Value> = Vec::new();
    for source in sources {
        let source = obj(source)?;
        let Some(backfill) = source.get("backfill").filter(|v| !v.is_null()) else {
            continue;
        };
        let backfill = obj(backfill)?;
        backfill_rows.push(json!({
            "host": text(source.get("computer"))?,
            "tool": text(source.get("tool"))?,
            "state": backfill.get("state").and_then(Value::as_str).unwrap_or("pending"),
            "filesRead": backfill.get("files_read").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
            "filesTotal": backfill.get("files_total").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
        }));
    }
    let running = backfill_rows.iter().any(|row| {
        row.get("state").and_then(Value::as_str) == Some("pending")
            || row.get("state").and_then(Value::as_str) == Some("running")
    });
    let mut computer_rows: Vec<Value> = Vec::new();
    for computer in computers {
        let computer = obj(computer)?;
        let gaps = arr(Some(computer.get("gaps").ok_or(Decline::Shape)?))?;
        let mut gap_rows: Vec<Value> = Vec::new();
        for gap in gaps {
            let gap = arr(Some(gap))?;
            gap_rows.push(json!([iso(num(gap.first())?)?, iso(num(gap.get(1))?)?]));
        }
        computer_rows.push(json!({
            "host": text(computer.get("computer"))?,
            "oldestAt": at(computer.get("oldestMs"))?,
            "newestAt": at(computer.get("newestMs"))?,
            "lastReadAt": at(computer.get("lastReadMs"))?,
            "gaps": gap_rows,
        }));
    }
    let mut source_rows: Vec<Value> = Vec::new();
    for source in sources {
        let source = obj(source)?;
        let future = opt_num(source.get("futureDated"))?;
        let mut row = json!({
            "host": text(source.get("computer"))?,
            "tool": text(source.get("tool"))?,
            "state": source.get("state").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
            "cause": source.get("cause").filter(|v| !v.is_null()).cloned().unwrap_or(Value::Null),
            "lastReadAt": at(source.get("lastReadMs"))?,
            "lastFullReadAt": at(source.get("lastCompleteMs"))?,
            "badSince": at(source.get("badSinceMs"))?,
            "oldestAt": at(source.get("oldestMs"))?,
            "futureDated": match future {
                Some(n) if n > 0.0 => js_num(n),
                _ => Value::Null,
            },
        });
        if let Some(counts) = source.get("counts") {
            if let Some(freshness) = counts.get("freshness") {
                row["freshness"] = freshness.clone();
            }
            if let Some(count) = counts.get("unmeasured_files") {
                row["unmeasuredFiles"] = count.clone();
            }
            if let Some(count) = counts.get("unreadable_records") {
                row["unreadableRecords"] = count.clone();
            }
            // Wave 82's coverage disclosures: a usage-shaped record the reader could not decode,
            // and a date it estimated from a file time. Only true is evidence.
            for (from, to) in [
                ("unread_usage", "unreadUsage"),
                ("estimated_clock", "estimatedClock"),
            ] {
                if counts.get(from).and_then(Value::as_bool) == Some(true) {
                    row[to] = json!(true);
                }
            }
        }
        source_rows.push(row);
    }
    Ok(json!({
        "computers": computer_rows,
        "sources": source_rows,
        "backfill": { "running": running, "sources": backfill_rows },
        "quality": [],
    }))
}

// ---------------------------------------------------------------- projection

/// `totals`: the six published amounts of a row, each clamped finite and non-negative.
fn totals(row: &Object) -> [f64; 6] {
    let cost = match row.get("totalCost") {
        None | Some(Value::Null) => finite(row.get("cost")),
        Some(value) => finite(Some(value)),
    };
    [
        finite(row.get("inputTokens")),
        finite(row.get("outputTokens")),
        finite(row.get("cacheCreationTokens")),
        finite(row.get("cacheReadTokens")),
        cost,
        finite(row.get("fallbackCost")),
    ]
}

/// Totals plus per-type cost parts; parts become unknown once any contribution has no rate.
#[derive(Clone)]
struct Accumulator {
    values: [f64; 6],
    parts: [f64; 4],
    parts_known: bool,
}
impl Accumulator {
    fn new() -> Self {
        Self {
            values: [0.0; 6],
            parts: [0.0; 4],
            parts_known: true,
        }
    }
    fn add_values(&mut self, values: &[f64; 6]) {
        for (index, field) in FIELDS.iter().enumerate() {
            let _ = field;
            let next = self.values[index] + values[index];
            if next.is_finite() {
                self.values[index] = next;
            }
        }
    }
    fn add_parts(&mut self, parts: Option<&[f64; 4]>) {
        match parts {
            None => self.parts_known = false,
            Some(parts) => {
                for (total, part) in self.parts.iter_mut().zip(parts) {
                    let next = *total + part;
                    if next.is_finite() {
                        *total = next;
                    } else {
                        self.parts_known = false;
                    }
                }
            }
        }
    }
    fn merge(&mut self, source: &Accumulator) {
        self.add_values(&source.values);
        self.add_parts(if source.parts_known {
            Some(&source.parts)
        } else {
            None
        });
    }
    /// `publish`: the totals object, with the cost parts when every contribution had a rate.
    fn publish(&self) -> Value {
        let mut out = json!({
            "inputTokens": js_num(self.values[0]),
            "outputTokens": js_num(self.values[1]),
            "cacheCreationTokens": js_num(self.values[2]),
            "cacheReadTokens": js_num(self.values[3]),
            "estimatedCostUsd": js_num(self.values[4]),
            "fallbackCostUsd": js_num(nano_dollars(self.values[5])),
        });
        let map = out.as_object_mut().ok_or(Decline::Shape);
        let map = match map {
            Ok(map) => map,
            Err(_) => return out,
        };
        if !self.parts_known {
            map.insert("costByType".into(), Value::Null);
            map.insert("costByTypeReconciled".into(), json!(false));
            return out;
        }
        let sum: f64 = self.parts.iter().sum();
        map.insert(
            "costByType".into(),
            json!({
                "input": js_num(nano_dollars(self.parts[0])),
                "output": js_num(nano_dollars(self.parts[1])),
                "cacheWrite": js_num(nano_dollars(self.parts[2])),
                "cacheRead": js_num(nano_dollars(self.parts[3])),
            }),
        );
        map.insert(
            "costByTypeReconciled".into(),
            json!((sum - self.values[4]).abs() <= (0.01f64).max(0.005 * self.values[4])),
        );
        out
    }
}

/// One priced model breakdown of a row (`PricedBreakdowns.models`).
struct PricedModel {
    name: Option<String>,
    values: [f64; 6],
    parts: Option<[f64; 4]>,
    rates: Option<Rates>,
    quality: f64,
}
struct PricedBreakdowns {
    parts: Option<[f64; 4]>,
    residual: Option<[f64; 6]>,
    models: Vec<PricedModel>,
}

/// `priceBreakdowns`: price each model breakdown of a worker row at its list rates.
fn price_breakdowns(row: &Object, pricing: &Pricing) -> R<PricedBreakdowns> {
    let row_values = totals(row);
    let mut covered = Accumulator::new();
    let mut sum = [0.0f64; 4];
    let mut known = true;
    let mut models: Vec<PricedModel> = Vec::new();
    for breakdown in row
        .get("modelBreakdowns")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        let Some(breakdown) = breakdown.as_object() else {
            continue;
        };
        let values = totals(breakdown);
        covered.add_values(&values);
        let name = valid_model_name(breakdown.get("modelName")).map(str::to_owned);
        let rates = match &name {
            None => None,
            Some(name) => {
                pricing.lookup(name, breakdown.get("provider").and_then(Value::as_str))?
            }
        };
        let list_rate = rates.as_ref().filter(|rates| rates.source != "fallback");
        let parts = list_rate.map(|rate| {
            [
                (values[0] / 1_000_000.0) * rate.input,
                (values[1] / 1_000_000.0) * rate.output,
                (values[2] / 1_000_000.0) * rate.cache_creation,
                (values[3] / 1_000_000.0) * rate.cache_read,
            ]
        });
        let quality = breakdown
            .get("quality")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        models.push(PricedModel {
            name,
            values,
            parts,
            rates,
            quality,
        });
        match parts {
            None => known = false,
            Some(parts) => {
                for index in 0..4 {
                    sum[index] += parts[index];
                }
            }
        }
    }
    let uncovered = (0..4).any(|index| row_values[index] > covered.values[index]);
    let residual = if uncovered {
        let mut out = [0.0f64; 6];
        for index in 0..6 {
            out[index] = (row_values[index] - covered.values[index]).max(0.0);
        }
        Some(out)
    } else {
        None
    };
    Ok(PricedBreakdowns {
        parts: if known && !uncovered { Some(sum) } else { None },
        residual,
        models,
    })
}

/// `sessionSpan`: compacted rows keep each hour's last event, so first activity counts from its UTC hour.
fn session_span(session: &Object) -> R<(f64, f64)> {
    // Date.parse of the ISO strings the store wrote; a missing or unreadable time is NaN in TS.
    let parse = |value: Option<&Value>| -> f64 {
        value
            .and_then(Value::as_str)
            .and_then(ecma::parse_instant)
            .map(|ms| ms as f64)
            .unwrap_or(f64::NAN)
    };
    let last = parse(session.get("lastActivity"));
    let first = parse(session.get("firstActivity"));
    Ok(if first <= last {
        (first - first.rem_euclid(HOUR_MS), last)
    } else {
        (last, last)
    })
}

/// The projection's bucket accumulators.
struct Bucket {
    totals: Accumulator,
    requests: f64,
    requests_known: bool,
}
impl Bucket {
    fn new() -> Self {
        Self {
            totals: Accumulator::new(),
            requests: 0.0,
            requests_known: true,
        }
    }
    fn add(&mut self, values: &Accumulator, request_count: Option<&Value>) {
        self.totals.merge(values);
        match request_count.and_then(Value::as_f64) {
            Some(count) if count.is_finite() && count >= 0.0 => self.requests += count,
            _ => self.requests_known = false,
        }
    }
    fn row(&self) -> Value {
        let mut out = self.totals.publish();
        if let Some(map) = out.as_object_mut() {
            map.insert(
                "requestCount".into(),
                if self.requests_known {
                    js_num(self.requests)
                } else {
                    Value::Null
                },
            );
        }
        out
    }
}

/// One per-day model row before the named-model fold.
#[derive(Clone)]
struct DayModelRow {
    date: String,
    provider: String,
    model: Option<String>,
    totals: Accumulator,
    quality: f64,
}

/// The live collector state and zone facts the projection reads. The store-fed paths never
/// carry a `cursorAccount` (only the in-memory snapshot path does, which stays in TS).
struct ProjectionOptions<'a> {
    tz: &'a str,
    zone: Option<&'a intl::Zone>,
    pricing: &'a Pricing,
    sources: Value,
    refreshing: bool,
    refreshing_remote: Value,
}

/// `localDate` as text, from the proven zone offsets (UTC is the ISO prefix).
pub(crate) fn local_date_text(epoch: f64, tz: &str, zone: Option<&intl::Zone>) -> R<String> {
    if tz == "UTC" {
        return Ok(iso(epoch)?[..10].to_owned());
    }
    let zone = zone.ok_or(Decline::Zone)?;
    let (year, month, day) = zone.local_date(time(epoch)?).ok_or(Decline::Zone)?;
    Ok(format!("{year:04}-{month:02}-{day:02}"))
}

/// `zoneOffsetMs` in minutes, from the proven offsets.
fn offset_minutes(epoch: f64, tz: &str, zone: Option<&intl::Zone>) -> R<i64> {
    if tz == "UTC" {
        return Ok(0);
    }
    let zone = zone.ok_or(Decline::Zone)?;
    zone.offset_minutes(time(epoch)?).ok_or(Decline::Zone)
}

pub(crate) fn day_number(date: &str) -> R<i64> {
    let bytes = date.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return Err(Decline::Date);
    }
    let part = |range: std::ops::Range<usize>| -> R<i64> {
        date[range].parse::<i64>().map_err(|_| Decline::Date)
    };
    Ok(civil::days_from_civil(
        part(0..4)?,
        part(5..7)?,
        part(8..10)?,
    ))
}

/// `localMidnight`: the first instant of a local calendar day, DST skips included.
pub(crate) fn local_midnight(date: &str, tz: &str, zone: Option<&intl::Zone>) -> R<f64> {
    if tz == "UTC" {
        #[allow(clippy::cast_precision_loss)]
        return Ok(day_number(date)? as f64 * DAY_MS);
    }
    let guess = day_number(date)? as f64 * DAY_MS;
    let offset_at = |epoch: f64| -> R<f64> {
        #[allow(clippy::cast_precision_loss)]
        Ok(offset_minutes(epoch, tz, zone)? as f64 * 60_000.0)
    };
    let mut epoch = guess - offset_at(guess)?;
    epoch = guess - offset_at(epoch)?;
    let step = 15.0 * 60_000.0;
    for _ in 0..8 {
        if local_date_text(epoch - step, tz, zone)? == date {
            epoch -= step;
        } else {
            break;
        }
    }
    for _ in 0..8 {
        if local_date_text(epoch, tz, zone)?.as_str() < date {
            epoch += step;
        } else {
            break;
        }
    }
    Ok(epoch)
}

/// `hasPartialHourOffset`: the zone is not a whole number of hours from UTC anywhere in the range.
fn has_partial_hour_offset(tz: &str, from: f64, to: f64, zone: Option<&intl::Zone>) -> R<bool> {
    if tz == "UTC" {
        return Ok(false);
    }
    // JavaScript's Date and `zoneOffsetMs` truncate fractional milliseconds. The midpoint
    // of an odd-millisecond range can be fractional; truncate it to the same instant instead
    // of refusing the whole Analytics answer as an invalid date.
    let middle = (from + (to - from) / 2.0).floor();
    for epoch in [from, to, middle] {
        if offset_minutes(epoch, tz, zone)?.rem_euclid(60) != 0 {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `localDayStarts`: the local midnights from the day of `from` through the one after `to`.
pub(crate) fn local_day_starts(
    from: f64,
    to: f64,
    tz: &str,
    zone: Option<&intl::Zone>,
) -> R<Vec<f64>> {
    let mut starts: Vec<f64> = Vec::new();
    let mut date = local_date_text(from, tz, zone)?;
    let last = local_date_text(to, tz, zone)?;
    for _ in 0..20_000 {
        starts.push(local_midnight(&date, tz, zone)?);
        if date > last {
            break;
        }
        let next = day_number(&date)? + 1;
        date = ecma::to_iso_string(next * 86_400_000).ok_or(Decline::Date)?[..10].to_owned();
    }
    Ok(starts)
}

/// `detectAccountAnalyticsAnomalies`: the original CCS insights rules on local days.
fn detect_anomalies(days: &[(String, f64)], models: &[DayModelRow]) -> R<Value> {
    const MAX_ANOMALIES: usize = 100;
    const HIGH_INPUT: f64 = 10_000_000.0;
    const HIGH_IO: f64 = 100.0;
    const HIGH_CACHE: f64 = 1_000_000_000.0;
    let format_tokens = |tokens: f64| -> R<String> {
        let (value, suffix) = if tokens >= 1_000_000_000.0 {
            (tokens / 1_000_000_000.0, "B")
        } else if tokens >= 1_000_000.0 {
            (tokens / 1_000_000.0, "M")
        } else if tokens >= 1_000.0 {
            (tokens / 1_000.0, "K")
        } else {
            return Ok(ecma::to_string(tokens));
        };
        Ok(format!(
            "{}{suffix}",
            ecma::to_fixed(value, 1).ok_or(Decline::Number)?
        ))
    };
    let mut items: Vec<Value> = Vec::new();
    let total_cost: f64 = days.iter().map(|(_, cost)| cost).sum();
    let average = if days.is_empty() {
        0.0
    } else {
        total_cost / days.len() as f64
    };
    let spike_threshold = average * 2.0;
    let mut models_by_date: OrderedMap<Vec<&DayModelRow>> = OrderedMap::new();
    for row in models {
        let mut list = models_by_date.get(&row.date).cloned().unwrap_or_default();
        list.push(row);
        models_by_date.set(row.date.clone(), list);
    }
    let mut dates: Vec<String> = days.iter().map(|(date, _)| date.clone()).collect();
    for (date, _) in models_by_date.entries() {
        if !dates.iter().any(|d| d == date) {
            dates.push(date.to_owned());
        }
    }
    dates.sort();
    let costs: HashMap<&str, f64> = days
        .iter()
        .map(|(date, cost)| (date.as_str(), *cost))
        .collect();
    for date in &dates {
        if let Some(cost) = costs.get(date.as_str())
            && average > 0.0
            && *cost > spike_threshold
        {
            let multiplier = ecma::math_round((cost / average) * 10.0) / 10.0;
            items.push(json!({
                "date": date,
                "type": "cost_spike",
                "provider": Value::Null,
                "model": Value::Null,
                "value": js_num(*cost),
                "threshold": js_num(average),
                "message": format!(
                    "Cost {}x above daily average (${} vs ${})",
                    ecma::to_string(multiplier),
                    ecma::to_string(ecma::math_round(*cost)),
                    ecma::to_string(ecma::math_round(average))
                ),
            }));
        }
        for row in models_by_date.get(date).map(Vec::as_slice).unwrap_or(&[]) {
            let input = row.totals.values[0];
            let output = row.totals.values[1];
            let cache_read = row.totals.values[3];
            let model = row.model.clone().unwrap_or_default();
            if input > HIGH_INPUT {
                let multiplier = ecma::math_round((input / HIGH_INPUT) * 10.0) / 10.0;
                items.push(json!({
                    "date": date,
                    "type": "high_input",
                    "provider": row.provider,
                    "model": model,
                    "value": js_num(input),
                    "threshold": js_num(HIGH_INPUT),
                    "message": format!(
                        "Input tokens {}x above threshold ({})",
                        ecma::to_string(multiplier),
                        format_tokens(input)?
                    ),
                }));
            }
            if output > 0.0 {
                let ratio = input / output;
                if ratio > HIGH_IO {
                    let multiplier = ecma::math_round((ratio / HIGH_IO) * 10.0) / 10.0;
                    items.push(json!({
                        "date": date,
                        "type": "high_io_ratio",
                        "provider": row.provider,
                        "model": model,
                        "value": js_num(ratio),
                        "threshold": js_num(HIGH_IO),
                        "message": format!(
                            "I/O ratio {}x above threshold ({}:1)",
                            ecma::to_string(multiplier),
                            ecma::to_string(ecma::math_round(ratio))
                        ),
                    }));
                }
            }
            if cache_read > HIGH_CACHE {
                let multiplier = ecma::math_round((cache_read / HIGH_CACHE) * 10.0) / 10.0;
                items.push(json!({
                    "date": date,
                    "type": "high_cache_read",
                    "provider": row.provider,
                    "model": model,
                    "value": js_num(cache_read),
                    "threshold": js_num(HIGH_CACHE),
                    "message": format!(
                        "Cache reads {}x above threshold ({})",
                        ecma::to_string(multiplier),
                        format_tokens(cache_read)?
                    ),
                }));
            }
        }
    }
    // Descending date; JS sort is stable, and so is sort_by.
    let mut sorted = items;
    let mut fallible: R<()> = Ok(());
    sorted.sort_by(|a, b| {
        let da = a.get("date").and_then(Value::as_str).unwrap_or("");
        let db = b.get("date").and_then(Value::as_str).unwrap_or("");
        match cmp_str(db, da) {
            Ok(order) => order,
            Err(err) => {
                fallible = Err(err);
                Ordering::Equal
            }
        }
    });
    fallible?;
    let days_of = |kind: &str| -> usize {
        let mut dates: Vec<&str> = Vec::new();
        for item in &sorted {
            if item.get("type").and_then(Value::as_str) == Some(kind) {
                let date = item.get("date").and_then(Value::as_str).unwrap_or("");
                if !dates.contains(&date) {
                    dates.push(date);
                }
            }
        }
        dates.len()
    };
    Ok(json!({
        "thresholds": {
            "costSpikeMultiplier": 2,
            "highInputTokens": 10_000_000,
            "highIoRatio": 100,
            "highCacheReadTokens": 1_000_000_000,
        },
        "items": sorted.iter().take(MAX_ANOMALIES).collect::<Vec<_>>(),
        "summary": {
            "totalAnomalies": sorted.len(),
            "highInputDays": days_of("high_input"),
            "highIoRatioDays": days_of("high_io_ratio"),
            "costSpikeDays": days_of("cost_spike"),
            "highCacheReadDays": days_of("high_cache_read"),
        },
    }))
}

/// `projectAccountAnalyticsActivity`: the activity block from the attributed sources.
fn project(
    sources: &[SourceData],
    query: &Object,
    from: f64,
    to: f64,
    status: &str,
    message: &str,
    options: &ProjectionOptions<'_>,
) -> R<Value> {
    let tz = options.tz;
    let provider_filter = text(query.get("provider"))?;
    let account_filter = text(query.get("account"))?;
    let mut base = json!({
        "status": status,
        "refreshing": options.refreshing,
        "refreshingRemote": options.refreshing_remote,
        "scope": "multi-host-cli",
        "timezone": tz,
        "accountAttribution": "unavailable",
        "costBasis": "estimated-api-equivalent",
        "fetchedAt": Value::Null,
        "message": message,
        "totals": Value::Null,
        "providers": [],
        "byDay": [],
        "byHour": [],
        "models": [],
        "byDayModel": [],
        "sessions": Value::Null,
        "anomalies": Value::Null,
        "sources": options.sources,
    });
    let fetched_at_of = |sources: &[SourceData]| -> R<Value> {
        let mut stamps: Vec<String> = sources.iter().map(|s| s.fetched_at.clone()).collect();
        stamps.sort();
        Ok(match stamps.first() {
            Some(stamp) => json!(stamp),
            None => Value::Null,
        })
    };
    base.as_object_mut()
        .ok_or(Decline::Shape)?
        .insert("fetchedAt".into(), fetched_at_of(sources)?);
    let set_base = |base: &mut Value, key: &str, value: Value| -> R<()> {
        base.as_object_mut()
            .ok_or(Decline::Shape)?
            .insert(key.into(), value);
        Ok(())
    };
    if account_filter != "all" {
        set_base(&mut base, "status", json!("unavailable"))?;
        set_base(
            &mut base,
            "message",
            json!(
                "Local CLI logs do not reliably identify a subscription account. Select all accounts to view local activity; account quota history remains available."
            ),
        )?;
        return Ok(base);
    }
    let attributed = attribute_activity_sources(sources)?;
    let selected: Vec<&AttributedSource> = attributed
        .iter()
        .filter(|source| provider_filter == "all" || provider_filter == source.provider)
        .collect();
    if selected.is_empty() {
        set_base(
            &mut base,
            "status",
            json!(if status == "loading" {
                "loading"
            } else {
                "unavailable"
            }),
        )?;
        set_base(
            &mut base,
            "message",
            json!(if provider_filter == "all" {
                message
            } else {
                "No CLI usage log in this range was served by this provider; its quota and balance observations remain available."
            }),
        )?;
        return Ok(base);
    }
    let mut selected_stamps: Vec<String> = selected
        .iter()
        .map(|source| source.fetched_at.clone())
        .collect();
    selected_stamps.sort();
    set_base(
        &mut base,
        "fetchedAt",
        match selected_stamps.first() {
            Some(stamp) => json!(stamp),
            None => Value::Null,
        },
    )?;
    let mut message = message.to_owned();
    if has_partial_hour_offset(tz, from, to, options.zone)? {
        message = format!(
            "{message} Days are built from UTC hours, so a day boundary in this time zone can be off by up to 45 minutes."
        );
    }
    let mut dates: HashMap<String, String> = HashMap::new();
    let date_of = |epoch: f64, dates: &mut HashMap<String, String>| -> R<String> {
        let key = format!("{epoch}");
        if let Some(date) = dates.get(&key) {
            return Ok(date.clone());
        }
        let date = local_date_text(epoch, tz, options.zone)?;
        dates.insert(key, date.clone());
        Ok(date)
    };
    let mut day_buckets: OrderedMap<(String, String, Bucket)> = OrderedMap::new();
    let mut hour_buckets: OrderedMap<(String, String, Bucket)> = OrderedMap::new();
    struct ModelBucket {
        model: String,
        provider: String,
        totals: Accumulator,
        rates: Option<Rates>,
        rates_seen: bool,
        tools: Vec<String>,
    }
    let mut model_buckets: OrderedMap<ModelBucket> = OrderedMap::new();
    let mut day_models: OrderedMap<DayModelRow> = OrderedMap::new();
    struct SessionCandidate {
        provider: String,
        tool: String,
        last_activity: f64,
        session: Object,
    }
    let mut session_candidates: OrderedMap<SessionCandidate> = OrderedMap::new();
    let mut distinct_sessions: HashSet<String> = HashSet::new();
    let mut combined = Accumulator::new();
    let add_day_model = |day_models: &mut OrderedMap<DayModelRow>,
                         date: &str,
                         provider: &str,
                         model: Option<&str>,
                         values: &[f64; 6],
                         parts: Option<&[f64; 4]>,
                         quality: f64| {
        let key = format!(
            "{date}\0{provider}\0{}\0{}\0{quality}",
            model.unwrap_or(""),
            if model.is_none() { "unnamed" } else { "named" }
        );
        let mut row = day_models.get(&key).cloned().unwrap_or(DayModelRow {
            date: date.to_owned(),
            provider: provider.to_owned(),
            model: model.map(str::to_owned),
            totals: Accumulator::new(),
            quality,
        });
        row.totals.add_values(values);
        row.totals.add_parts(parts);
        day_models.set(key, row);
    };
    let mut providers: Vec<Value> = Vec::new();
    for source in &selected {
        let mut source_totals = Accumulator::new();
        let mut usage_events = 0.0f64;
        let mut sessions: HashSet<String> = HashSet::new();
        let mut tools: Vec<String> = Vec::new();
        for result in &source.data {
            for hour in &result.hourly {
                let label = text(hour.get("hour"))?;
                let Some(epoch) = hour_epoch(label) else {
                    continue;
                };
                if epoch < from || epoch > to {
                    continue;
                }
                if !tools.contains(&result.tool) {
                    tools.push(result.tool.clone());
                }
                let date = date_of(epoch, &mut dates)?;
                let priced = price_breakdowns(hour, options.pricing)?;
                let mut values = Accumulator::new();
                values.add_values(&totals(hour));
                values.add_parts(priced.parts.as_ref());
                source_totals.merge(&values);
                let day_key = format!("{}:{date}", source.provider);
                let mut day = day_buckets
                    .get(&day_key)
                    .map(|(date, provider, bucket)| {
                        (date.clone(), provider.clone(), clone_bucket(bucket))
                    })
                    .unwrap_or((date.clone(), source.provider.clone(), Bucket::new()));
                day.2.add(&values, hour.get("requestCount"));
                day_buckets.set(day_key, day);
                let hour_key = format!("{}:{label}", source.provider);
                let mut bucket = hour_buckets
                    .get(&hour_key)
                    .map(|(hour, provider, bucket)| {
                        (hour.clone(), provider.clone(), clone_bucket(bucket))
                    })
                    .unwrap_or((
                        format!("{}:00Z", label.replace(' ', "T")),
                        source.provider.clone(),
                        Bucket::new(),
                    ));
                bucket.2.add(&values, hour.get("requestCount"));
                hour_buckets.set(hour_key, bucket);
                usage_events += finite(hour.get("requestCount"));
                for model in &priced.models {
                    add_day_model(
                        &mut day_models,
                        &date,
                        &source.provider,
                        model.name.as_deref(),
                        &model.values,
                        model.parts.as_ref(),
                        model.quality,
                    );
                    let Some(name) = &model.name else {
                        continue;
                    };
                    if model.quality != 0.0 {
                        continue;
                    }
                    let key = format!("{}:{name}", source.provider);
                    let mut existing = model_buckets.get(&key).map(|bucket| ModelBucket {
                        model: bucket.model.clone(),
                        provider: bucket.provider.clone(),
                        totals: clone_accumulator(&bucket.totals),
                        rates: bucket.rates.clone(),
                        rates_seen: bucket.rates_seen,
                        tools: bucket.tools.clone(),
                    });
                    let bucket = existing.get_or_insert(ModelBucket {
                        model: name.clone(),
                        provider: source.provider.clone(),
                        totals: Accumulator::new(),
                        rates: None,
                        rates_seen: false,
                        tools: Vec::new(),
                    });
                    if !bucket.tools.contains(&result.tool) {
                        bucket.tools.push(result.tool.clone());
                    }
                    bucket.totals.add_values(&model.values);
                    bucket.totals.add_parts(model.parts.as_ref());
                    if !bucket.rates_seen {
                        bucket.rates = model.rates.clone();
                    } else if bucket.rates.is_some()
                        && (model.rates.is_none()
                            || !same_rates(bucket.rates.as_ref(), model.rates.as_ref()))
                    {
                        bucket.rates = None;
                    }
                    bucket.rates_seen = true;
                    if let Some(bucket) = existing {
                        model_buckets.set(key, bucket);
                    }
                }
                if let Some(residual) = &priced.residual {
                    add_day_model(
                        &mut day_models,
                        &date,
                        &source.provider,
                        None,
                        residual,
                        None,
                        0.0,
                    );
                }
            }
            for (part, whole, dominant) in &result.session {
                let (first, last_activity) = session_span(part)?;
                let Some(session_id) = part.get("sessionId").and_then(Value::as_str) else {
                    continue;
                };
                if first <= to && last_activity >= from {
                    let key = format!("{}\0{session_id}", result.tool);
                    sessions.insert(key.clone());
                    distinct_sessions.insert(key.clone());
                    let previous = session_candidates.get(&key);
                    if *dominant
                        && previous.is_none_or(|previous| last_activity > previous.last_activity)
                    {
                        session_candidates.set(
                            key,
                            SessionCandidate {
                                provider: source.provider.clone(),
                                tool: result.tool.clone(),
                                last_activity,
                                session: whole.clone(),
                            },
                        );
                    }
                }
            }
        }
        combined.merge(&source_totals);
        providers.push(json!({
            "provider": source.provider,
            "label": provider_label(&source.provider),
            "totals": source_totals.publish(),
            "usageEvents": js_num(usage_events),
            "sessionCount": sessions.len(),
            "tools": aac_providers::activity_tools().filter(|tool| tools.iter().any(|t| t.as_str() == *tool)).collect::<Vec<_>>(),
        }));
    }
    // byDay / byHour, in their sort orders.
    let mut by_day: Vec<(String, String, Bucket)> = day_buckets
        .into_entries()
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    let mut sort_err: R<()> = Ok(());
    by_day.sort_by(|a, b| {
        match cmp_str(&a.0, &b.0).and_then(|order| match order {
            Ordering::Equal => cmp_str(&a.1, &b.1),
            order => Ok(order),
        }) {
            Ok(order) => order,
            Err(err) => {
                sort_err = Err(err);
                Ordering::Equal
            }
        }
    });
    sort_err?;
    let by_day: Vec<Value> = by_day
        .iter()
        .map(|(date, provider, bucket)| {
            let mut row = json!({ "date": date, "provider": provider });
            if let (Some(target), Some(extra)) = (row.as_object_mut(), bucket.row().as_object()) {
                for (key, value) in extra {
                    target.insert(key.clone(), value.clone());
                }
            }
            row
        })
        .collect();
    let mut by_hour: Vec<(String, String, Bucket)> = hour_buckets
        .into_entries()
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    by_hour.sort_by(|a, b| {
        match cmp_str(&a.0, &b.0).and_then(|order| match order {
            Ordering::Equal => cmp_str(&a.1, &b.1),
            order => Ok(order),
        }) {
            Ok(order) => order,
            Err(err) => {
                sort_err = Err(err);
                Ordering::Equal
            }
        }
    });
    sort_err?;
    let by_hour: Vec<Value> = by_hour
        .iter()
        .map(|(hour, provider, bucket)| {
            let mut row = json!({ "hour": hour, "provider": provider });
            if let (Some(target), Some(extra)) = (row.as_object_mut(), bucket.row().as_object()) {
                for (key, value) in extra {
                    target.insert(key.clone(), value.clone());
                }
            }
            row
        })
        .collect();
    // Per-day model rows: the twelve costliest named models keep their name.
    let mut model_cost: HashMap<String, f64> = HashMap::new();
    for (_, row) in day_models.entries() {
        if let Some(model) = &row.model
            && model != OTHER_MODELS
            && row.quality == 0.0
        {
            let key = format!("{}\0{model}", row.provider);
            *model_cost.entry(key).or_insert(0.0) += row.totals.values[4];
        }
    }
    let mut cost_entries: Vec<(String, f64)> = model_cost.into_iter().collect();
    cost_entries.sort_by(
        |a, b| match b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal) {
            Ordering::Equal => match cmp_str(&a.0, &b.0) {
                Ok(order) => order,
                Err(err) => {
                    sort_err = Err(err);
                    Ordering::Equal
                }
            },
            order => order,
        },
    );
    sort_err?;
    let named: HashSet<String> = cost_entries
        .iter()
        .take(MAX_NAMED_DAY_MODELS)
        .map(|(key, _)| key.clone())
        .collect();
    let mut folded: OrderedMap<DayModelRow> = OrderedMap::new();
    for (_, row) in day_models.entries() {
        let model = match &row.model {
            Some(model) if named.contains(&format!("{}\0{model}", row.provider)) => model.clone(),
            _ => OTHER_MODELS.to_owned(),
        };
        let key = format!("{}\0{}\0{model}\0{}", row.date, row.provider, row.quality);
        let mut target = folded.get(&key).cloned().unwrap_or(DayModelRow {
            date: row.date.clone(),
            provider: row.provider.clone(),
            model: Some(model.clone()),
            totals: Accumulator::new(),
            quality: row.quality,
        });
        target.totals.merge(&row.totals);
        folded.set(key, target);
    }
    let mut folded_rows: Vec<DayModelRow> = folded
        .into_entries()
        .into_iter()
        .map(|(_, row)| row)
        .collect();
    folded_rows.sort_by(|a, b| {
        let order = cmp_str(&a.date, &b.date)
            .and_then(|order| match order {
                Ordering::Equal => cmp_str(&a.provider, &b.provider),
                order => Ok(order),
            })
            .map(|order| match order {
                Ordering::Equal => b.totals.values[4]
                    .partial_cmp(&a.totals.values[4])
                    .unwrap_or(Ordering::Equal),
                order => order,
            })
            .and_then(|order| match order {
                Ordering::Equal => cmp_str(
                    a.model.as_deref().unwrap_or(""),
                    b.model.as_deref().unwrap_or(""),
                ),
                order => Ok(order),
            });
        match order {
            Ok(order) => order,
            Err(err) => {
                sort_err = Err(err);
                Ordering::Equal
            }
        }
    });
    sort_err?;
    let by_day_model: Vec<Value> = folded_rows
        .iter()
        .map(|row| {
            let mut out = json!({
                "date": row.date,
                "provider": row.provider,
                "model": row.model,
            });
            if let (Some(target), Some(extra)) =
                (out.as_object_mut(), row.totals.publish().as_object())
            {
                for (key, value) in extra {
                    target.insert(key.clone(), value.clone());
                }
            }
            if row.quality != 0.0
                && let Some(target) = out.as_object_mut()
            {
                target.insert("quality".into(), json!(quality_words(row.quality)));
            }
            out
        })
        .collect();
    let usable = status == "ok" || status == "cached";
    let mut candidates: Vec<(String, SessionCandidate)> = session_candidates.into_entries();
    candidates.sort_by(|a, b| {
        match b
            .1
            .last_activity
            .partial_cmp(&a.1.last_activity)
            .unwrap_or(Ordering::Equal)
        {
            Ordering::Equal => match cmp_str(&a.0, &b.0) {
                Ok(order) => order,
                Err(err) => {
                    sort_err = Err(err);
                    Ordering::Equal
                }
            },
            order => order,
        }
    });
    sort_err?;
    let mut sample: Vec<Value> = Vec::new();
    for (_, candidate) in candidates.iter().take(MAX_SESSION_SAMPLE) {
        let session = &candidate.session;
        let session_id = text(session.get("sessionId"))?;
        let tool = aac_core::ToolId::new(candidate.tool.clone()).map_err(|_| Decline::Shape)?;
        // An empty raw id publishes no key; skip the sample row as the TS projection does.
        let Some(key) = aac_core::SessionKey::published(&tool, session_id) else {
            continue;
        };
        let priced = price_breakdowns(session, options.pricing)?;
        let mut values = Accumulator::new();
        values.add_values(&totals(session));
        values.add_parts(priced.parts.as_ref());
        let mut from_breakdowns: Vec<(String, f64)> = priced
            .models
            .iter()
            .filter_map(|model| {
                let name = model.name.as_ref()?;
                if name.is_empty() || name == SYNTHETIC_MODEL_NAME {
                    return None;
                }
                Some((name.clone(), model.values[..4].iter().sum()))
            })
            .collect();
        from_breakdowns.sort_by(
            |a, b| match b.1.partial_cmp(&a.1).unwrap_or(Ordering::Equal) {
                Ordering::Equal => match cmp_str(&a.0, &b.0) {
                    Ok(order) => order,
                    Err(err) => {
                        sort_err = Err(err);
                        Ordering::Equal
                    }
                },
                order => order,
            },
        );
        sort_err?;
        let mut models: Vec<String> = Vec::new();
        if from_breakdowns.is_empty() {
            for name in session
                .get("modelsUsed")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                if let Some(name) = valid_model_name(Some(name))
                    && !name.is_empty()
                    && name != SYNTHETIC_MODEL_NAME
                    && !models.iter().any(|m| m == name)
                {
                    models.push(name.to_owned());
                }
            }
        } else {
            for (name, _) in from_breakdowns {
                if !models.contains(&name) {
                    models.push(name);
                }
            }
        }
        models.truncate(5);
        let target = session
            .get("target")
            .and_then(Value::as_str)
            .filter(|target| aac_providers::is_cli_target(target));
        let mut row = json!({
            "key": key.as_str(),
            "provider": candidate.provider,
            "lastActivity": iso(candidate.last_activity)?,
            "models": models,
            "target": target,
        });
        if let (Some(target), Some(extra)) = (row.as_object_mut(), values.publish().as_object()) {
            for (key, value) in extra {
                target.insert(key.clone(), value.clone());
            }
        }
        sample.push(row);
    }
    let session_total = distinct_sessions.len();
    if usable && !sample.is_empty() {
        message = format!(
            "{message} Session rows show each session's whole retained totals, which can include activity outside this range."
        );
    }
    // The day-level cost spike leaves out days holding flawed counts.
    let flawed_days: HashSet<String> = day_models
        .entries()
        .into_iter()
        .filter(|(_, row)| row.quality != 0.0)
        .map(|(_, row)| row.date.clone())
        .collect();
    let mut day_costs: OrderedMap<f64> = OrderedMap::new();
    for row in &by_day {
        let date = text(row.get("date"))?.to_owned();
        if !flawed_days.contains(&date) {
            let cost = finite(row.get("estimatedCostUsd"));
            let next = day_costs.get(&date).copied().unwrap_or(0.0) + cost;
            day_costs.set(date, next);
        }
    }
    let anomalies = if usable {
        let mut anomaly_models: Vec<DayModelRow> = day_models
            .entries()
            .into_iter()
            .filter(|(_, row)| {
                row.model.as_ref().is_some_and(|model| !model.is_empty()) && row.quality == 0.0
            })
            .map(|(_, row)| clone_day_model(row))
            .collect();
        anomaly_models.sort_by(|a, b| {
            let order = cmp_str(&a.date, &b.date)
                .and_then(|order| match order {
                    Ordering::Equal => cmp_str(&a.provider, &b.provider),
                    order => Ok(order),
                })
                .map(|order| match order {
                    Ordering::Equal => b.totals.values[4]
                        .partial_cmp(&a.totals.values[4])
                        .unwrap_or(Ordering::Equal),
                    order => order,
                })
                .and_then(|order| match order {
                    Ordering::Equal => cmp_str(
                        a.model.as_deref().unwrap_or(""),
                        b.model.as_deref().unwrap_or(""),
                    ),
                    order => Ok(order),
                });
            match order {
                Ok(order) => order,
                Err(err) => {
                    sort_err = Err(err);
                    Ordering::Equal
                }
            }
        });
        sort_err?;
        let days: Vec<(String, f64)> = day_costs.into_entries();
        detect_anomalies(&days, &anomaly_models)?
    } else {
        Value::Null
    };
    // The model rows, ranked by cost then tokens.
    let mut model_rows: Vec<ModelBucket> = model_buckets
        .into_entries()
        .into_iter()
        .map(|(_, bucket)| bucket)
        .collect();
    model_rows.sort_by(|a, b| {
        let token_total = |totals: &Accumulator| -> f64 { totals.values[..4].iter().sum() };

        b.totals.values[4]
            .partial_cmp(&a.totals.values[4])
            .unwrap_or(Ordering::Equal)
            .then_with(|| {
                token_total(&b.totals)
                    .partial_cmp(&token_total(&a.totals))
                    .unwrap_or(Ordering::Equal)
            })
            .then_with(|| match cmp_str(&a.provider, &b.provider) {
                Ok(order) => order,
                Err(err) => {
                    sort_err = Err(err);
                    Ordering::Equal
                }
            })
            .then_with(|| match cmp_str(&a.model, &b.model) {
                Ok(order) => order,
                Err(err) => {
                    sort_err = Err(err);
                    Ordering::Equal
                }
            })
    });
    sort_err?;
    let models: Vec<Value> = model_rows
        .iter()
        .take(MAX_PUBLISHED_MODELS)
        .map(|row| {
            let mut out = json!({
                "model": row.model,
                "provider": row.provider,
            });
            if let (Some(target), Some(extra)) =
                (out.as_object_mut(), row.totals.publish().as_object())
            {
                for (key, value) in extra {
                    target.insert(key.clone(), value.clone());
                }
            }
            if let Some(target) = out.as_object_mut() {
                target.insert(
                    "rates".into(),
                    row.rates
                        .as_ref()
                        .map(Pricing::value)
                        .unwrap_or(Value::Null),
                );
                target.insert(
                    "tools".into(),
                    json!(
                        aac_providers::activity_tools()
                            .filter(|tool| row.tools.iter().any(|t| t.as_str() == *tool))
                            .collect::<Vec<_>>()
                    ),
                );
            }
            out
        })
        .collect();
    set_base(&mut base, "message", json!(message))?;
    set_base(&mut base, "totals", combined.publish())?;
    set_base(&mut base, "providers", Value::Array(providers))?;
    set_base(&mut base, "byDay", Value::Array(by_day))?;
    set_base(&mut base, "byHour", Value::Array(by_hour))?;
    set_base(&mut base, "models", Value::Array(models))?;
    set_base(&mut base, "byDayModel", Value::Array(by_day_model))?;
    set_base(
        &mut base,
        "sessions",
        if usable {
            json!({
                "total": session_total,
                "sample": sample,
                "truncated": session_total > sample.len(),
            })
        } else {
            Value::Null
        },
    )?;
    set_base(&mut base, "anomalies", anomalies)?;
    Ok(base)
}

fn clone_bucket(bucket: &Bucket) -> Bucket {
    Bucket {
        totals: clone_accumulator(&bucket.totals),
        requests: bucket.requests,
        requests_known: bucket.requests_known,
    }
}
fn clone_accumulator(value: &Accumulator) -> Accumulator {
    Accumulator {
        values: value.values,
        parts: value.parts,
        parts_known: value.parts_known,
    }
}
fn clone_day_model(row: &DayModelRow) -> DayModelRow {
    DayModelRow {
        date: row.date.clone(),
        provider: row.provider.clone(),
        model: row.model.clone(),
        totals: clone_accumulator(&row.totals),
        quality: row.quality,
    }
}
fn same_rates(left: Option<&Rates>, right: Option<&Rates>) -> bool {
    match (left, right) {
        (Some(left), Some(right)) => RATE_KEYS.iter().all(|key| match *key {
            "inputPerMillion" => left.input == right.input,
            "outputPerMillion" => left.output == right.output,
            "cacheCreationPerMillion" => left.cache_creation == right.cache_creation,
            "cacheReadPerMillion" => left.cache_read == right.cache_read,
            _ => left.source == right.source,
        }),
        _ => false,
    }
}
/// `qualityWords`: the contract's flag words of the quality bits.
fn quality_words(bits: f64) -> Vec<&'static str> {
    let mut out = Vec::new();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let bits = bits as u32;
    if bits & 1 != 0 {
        out.push("outputIncludesReasoningTwice");
    }
    out
}

// ---------------------------------------------------------------- storeActivityAnswer

/// The live collector state the front door supplies (`storeActivityAnswer`'s `live`).
pub(crate) struct LiveState {
    pub(crate) tz: String,
    pub(crate) sources: Value,
    pub(crate) refreshing: bool,
    pub(crate) refreshing_remote: Value,
}

/// The store-fed activity answer: the block, the oldest hour and the providers with activity.
pub(crate) struct StoreAnswer {
    pub(crate) activity: Value,
    pub(crate) oldest_hour_at: Option<f64>,
    pub(crate) providers_with_activity: Vec<String>,
}

/// `storeActivityAnswer`: the page's activity from the store's own range and sources answers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn store_activity_answer(
    range: &Value,
    sources_state: &Value,
    query: &Object,
    from: f64,
    to: f64,
    live: &LiveState,
    zone: Option<&intl::Zone>,
    pricing: &Pricing,
    clock: Option<f64>,
) -> R<StoreAnswer> {
    let daily = to - from > HOURLY_RANGE_MS;
    let day_starts = if daily {
        local_day_starts(from, to, &live.tz, zone)?
    } else {
        Vec::new()
    };
    let coverage = coverage_of(sources_state)?;
    let computers = arr(Some(
        obj(sources_state)?.get("computers").ok_or(Decline::Shape)?,
    ))?;
    let mut last_read = 0.0f64;
    let mut oldest: Option<f64> = None;
    let mut stored = false;
    for computer in computers {
        let computer = obj(computer)?;
        let read = opt_num(computer.get("lastReadMs"))?.unwrap_or(0.0);
        last_read = last_read.max(read);
        let oldest_ms = opt_num(computer.get("oldestMs"))?;
        if oldest_ms.is_some() || opt_num(computer.get("lastReadMs"))?.is_some() {
            stored = true;
        }
        if let Some(ms) = oldest_ms {
            oldest = Some(oldest.map_or(ms, |min: f64| min.min(ms)));
        }
    }
    // The store's last read stamps the answer; without one TS read its own clock. The native
    // read passes the request clock (it is the only reader); the envelope path passes none,
    // because it cannot prove its clock equal to the TS computation's.
    let last_read = if last_read == 0.0 {
        clock.ok_or(Decline::Stale)?
    } else {
        last_read
    };
    let fetched_at = iso(last_read)?;
    let sources = range_to_sources(range, daily, &fetched_at, pricing)?;
    let status = if !sources.is_empty() {
        if live.refreshing { "cached" } else { "ok" }
    } else if stored {
        "ok"
    } else if live.refreshing {
        "loading"
    } else {
        "unavailable"
    };
    let project_from = if daily {
        from.min(day_starts.first().copied().unwrap_or(from))
    } else {
        from
    };
    let project_to = if daily {
        let last_start = day_starts
            .get(day_starts.len().saturating_sub(2))
            .copied()
            .unwrap_or(to);
        let stamp = (last_start / HOUR_MS).ceil() * HOUR_MS;
        to.max(stamp)
    } else {
        to
    };
    let message = if status == "loading" {
        "Usage history is being read for the first time. Quota history is available immediately."
            .to_owned()
    } else {
        format!(
            "CLI activity kept in AAC's history ({}). Cost is an API-equivalent estimate, not a subscription charge.",
            if daily {
                format!("daily buckets in {}", live.tz)
            } else {
                format!("UTC hourly buckets; days are grouped in {}", live.tz)
            }
        )
    };
    let options = ProjectionOptions {
        tz: &live.tz,
        zone,
        pricing,
        sources: live.sources.clone(),
        refreshing: live.refreshing,
        refreshing_remote: live.refreshing_remote.clone(),
    };
    let mut projected = project(
        &sources,
        query,
        project_from,
        project_to,
        status,
        &message,
        &options,
    )?;
    // Session counts cover every session in the range, not only the sample the projection saw.
    let counts = session_counts(range)?;
    if let Some(providers) = projected.get_mut("providers").and_then(Value::as_array_mut) {
        for provider in providers {
            let id = text(provider.get("provider"))?.to_owned();
            let count = counts.get(&id).copied().unwrap_or(0.0);
            if let Some(map) = provider.as_object_mut() {
                map.insert("sessionCount".into(), js_num(count));
            }
        }
    }
    let range_sessions = obj(obj(range)?.get("sessions").ok_or(Decline::Shape)?)?;
    let range_total = num(range_sessions.get("total"))?;
    let range_truncated = range_sessions
        .get("truncated")
        .and_then(Value::as_bool)
        .ok_or(Decline::Shape)?;
    if let Some(sessions) = projected.get_mut("sessions").filter(|v| !v.is_null()) {
        let provider_filter = text(query.get("provider"))?;
        let total = if provider_filter == "all" {
            range_total
        } else {
            counts.get(provider_filter).copied().unwrap_or(0.0)
        };
        let projected_truncated = sessions
            .get("truncated")
            .and_then(Value::as_bool)
            .ok_or(Decline::Shape)?;
        if let Some(map) = sessions.as_object_mut() {
            map.insert("total".into(), js_num(total));
            map.insert(
                "truncated".into(),
                json!(range_truncated || projected_truncated),
            );
        }
    }
    // The quality notes of the range, and the store coverage as the block's history.
    let quality = arr(Some(obj(range)?.get("quality").ok_or(Decline::Shape)?))?;
    let mut quality_rows: Vec<Value> = Vec::new();
    for row in quality {
        let row = obj(row)?;
        let flags = arr(Some(row.get("flags").ok_or(Decline::Shape)?))?;
        quality_rows.push(json!({
            "tool": text(row.get("tool"))?,
            "flags": flags,
            "from": iso(num(row.get("fromMs"))?)?,
            "to": iso(num(row.get("toMs"))?)?,
        }));
    }
    let mut history = coverage;
    if let Some(map) = history.as_object_mut() {
        map.insert("quality".into(), Value::Array(quality_rows));
    }
    let providers_with_activity: Vec<String> = projected
        .get("providers")
        .and_then(Value::as_array)
        .ok_or(Decline::Shape)?
        .iter()
        .filter_map(|provider| provider.get("provider").and_then(Value::as_str))
        .filter(|provider| *provider != "other")
        .map(str::to_owned)
        .collect();
    if let Some(map) = projected.as_object_mut() {
        map.insert("history".into(), history);
        map.insert(
            "resolution".into(),
            json!(if daily { "day" } else { "hour" }),
        );
    }
    Ok(StoreAnswer {
        activity: projected,
        oldest_hour_at: oldest,
        providers_with_activity,
    })
}

/// The guarded selection's activity: no store read, the fixed unavailable projection.
pub(crate) fn guarded_activity(
    query: &Object,
    from: f64,
    to: f64,
    live: &LiveState,
    zone: Option<&intl::Zone>,
    pricing: &Pricing,
) -> R<Value> {
    let options = ProjectionOptions {
        tz: &live.tz,
        zone,
        pricing,
        sources: live.sources.clone(),
        refreshing: live.refreshing,
        refreshing_remote: live.refreshing_remote.clone(),
    };
    project(
        &[],
        query,
        from,
        to,
        "unavailable",
        "Local activity is unavailable for this selection.",
        &options,
    )
}

/// Whether the selection is one the activity reader answers from the store (the guard's inverse).
pub(crate) fn activity_selection(query: &Object) -> R<bool> {
    let account = text(query.get("account"))?;
    let provider = text(query.get("provider"))?;
    let refresh = query
        .get("refresh")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let order = usage_provider_order();
    Ok(refresh || (account == "all" && (provider == "all" || order.contains(&provider))))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The port reads the tool table's own lists: the order it publishes tools in, the targets
    /// it accepts, the tools whose logs are one provider's usage, and the route words.
    #[test]
    fn the_tool_tables_keep_their_old_lists() {
        assert_eq!(
            aac_providers::activity_sources().collect::<Vec<_>>(),
            [
                "claude",
                "codex",
                "omp",
                "muse",
                "zcode",
                "jsonl",
                "antigravity"
            ]
        );
        assert_eq!(
            aac_providers::activity_tools().collect::<Vec<_>>(),
            [
                "claude",
                "codex",
                "omp",
                "muse",
                "zcode",
                "jsonl",
                "antigravity",
                "cursor",
                "devin"
            ]
        );
        assert!(aac_providers::is_cli_target("droid"));
        assert!(!aac_providers::is_cli_target("codex-cli"));
        assert_eq!(route_provider(Some("opencode-zen")), Some("opencode-go"));
        assert_eq!(route_provider(None), None);
        assert_eq!(usage_provider_for("muse", None, None), "muse");
        assert_eq!(usage_provider_for("omp", None, None), "other");
        assert_eq!(leak_provider("qwen".to_owned()), "qwen");
        assert_eq!(leak_provider("omp".to_owned()), "omp");
        assert_eq!(leak_provider("opencode-wallet".to_owned()), "other");
    }

    #[test]
    fn collate_is_total_and_keeps_the_ascii_order() {
        // Printable ASCII pairs keep exactly the ascii_compare (ICU root) order.
        for (a, b) in [
            ("a_1", "a-1"),
            ("a-b", "ab"),
            ("aB", "Ab"),
            ("A", "a b"),
            ("x", "x"),
            ("", ""),
            ("", "a"),
            ("a", ""),
            ("Z", "a"),
            ("claude-3", "claude-3-5"),
            ("gpt-4o", "gpt-4"),
        ] {
            assert_eq!(
                intl::collate(a, b),
                intl::ascii_compare(a, b).expect("a printable ASCII pair"),
                "{a:?} vs {b:?}"
            );
        }
        // Non-ASCII and control characters no longer decline; the order is deterministic.
        assert_eq!(intl::collate("é", "e"), Ordering::Greater);
        assert_eq!(intl::collate("e", "é"), Ordering::Less);
        assert_eq!(intl::collate("模型", "model"), Ordering::Greater);
        assert_eq!(intl::collate("café", "cafe"), Ordering::Greater);
        assert_eq!(intl::collate("a\tb", "ab"), Ordering::Greater);
        assert_eq!(intl::collate("x", "x"), Ordering::Equal);
        // A byte-order fallback for the declined pairs would cycle here: the ICU ranks put "a" < "A",
        // while bytes put "A" < "Bé" < "a". collate must stay a total order, so sorting is consistent.
        let mut words = ["a", "A", "Bé", "é", "ab", "abé", "az", "Z", "模型", "a\tb"];
        words.sort_by(|a, b| intl::collate(a, b));
        for pair in words.windows(2) {
            assert_ne!(
                intl::collate(pair[0], pair[1]),
                Ordering::Greater,
                "{:?} sorted before {:?}",
                pair[0],
                pair[1]
            );
        }
        for a in words {
            for b in words {
                assert_eq!(
                    intl::collate(a, b),
                    intl::collate(b, a).reverse(),
                    "antisymmetry {a:?} {b:?}"
                );
                for c in words {
                    if intl::collate(a, b) != Ordering::Greater
                        && intl::collate(b, c) != Ordering::Greater
                    {
                        assert_ne!(
                            intl::collate(a, c),
                            Ordering::Greater,
                            "transitivity {a:?} {b:?} {c:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_empty_session_id_is_counted_but_not_sampled() {
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../tests/fixtures/analytics-empty-session.json"
        ))
        .expect("shared projection fixture");
        let activity = fixture["sessions"][0]["lastActivity"]
            .as_str()
            .expect("activity time");
        let sessions = serde_json::from_value(fixture["sessions"].clone()).expect("session rows");
        let sources = [SourceData {
            provider: "claude".into(),
            fetched_at: activity.into(),
            data: vec![WorkerResult {
                hourly: vec![],
                session: sessions,
            }],
        }];
        let query = json!({"provider": "all", "account": "all"});
        let pricing = Pricing {
            rates: HashMap::new(),
        };
        let options = ProjectionOptions {
            tz: "UTC",
            zone: None,
            pricing: &pricing,
            sources: json!([]),
            refreshing: false,
            refreshing_remote: json!([]),
        };
        let at = ecma::parse_instant(activity).expect("fixture time") as f64;
        let projected = project(
            &sources,
            query.as_object().expect("query"),
            at - 3_600_000.0,
            at + 3_600_000.0,
            "ok",
            "Fixture",
            &options,
        )
        .expect("projected activity");
        let sessions = &projected["sessions"];
        assert_eq!(sessions["total"], fixture["expected"]["total"]);
        let keys: Vec<_> = sessions["sample"]
            .as_array()
            .expect("session sample")
            .iter()
            .map(|row| &row["key"])
            .collect();
        assert_eq!(json!(keys), fixture["expected"]["sampleKeys"]);
        assert_eq!(sessions["truncated"], fixture["expected"]["truncated"]);
    }

    #[test]
    fn analytics_grid_shows_missing_agents_as_unavailable_after_an_ok_read() {
        let last_full_read = "2026-11-15T12:00:00.000Z";
        for host in ["mac", "windows", "alexis"] {
            let previous = grid_from_store(&json!({"sources": [{
                "host": host, "tool": "codex", "state": "ok", "lastFullReadAt": last_full_read,
            }]}))
            .expect("previous grid");
            assert_eq!(previous[0]["state"], "ok", "{host}");

            let current = grid_from_store(&json!({"sources": [{
                "host": host, "tool": "codex", "state": "unreadable", "cause": "agent_missing",
                "lastFullReadAt": last_full_read, "lastReadAt": "2026-11-15T13:00:00.000Z",
            }]}))
            .expect("current grid");
            assert_eq!(
                current,
                json!([{
                    "host": host, "tool": "codex", "state": "unavailable", "cause": "agent_missing",
                    "detail": "agent missing", "lastScanAt": last_full_read, "rowCount": 0,
                }]),
                "{host}"
            );
        }
    }

    #[test]
    fn a_mixed_reads_detail_is_exactly_the_unreadable_count() {
        let grid = grid_from_store(&json!({"sources": [{
            "host": "ubuntu", "tool": "codex", "state": "partial",
            "unreadableRecords": 1,
        }]}))
        .expect("mixed grid");
        assert_eq!(
            grid,
            json!([{
                "host": "ubuntu", "tool": "codex", "state": "cached",
                "detail": "1 record unreadable", "lastScanAt": Value::Null,
                "rowCount": 0, "unreadableRecords": 1,
            }])
        );
    }

    #[test]
    fn kimi_code_rows_from_omp_logs_are_kimi_code_not_other() {
        assert_eq!(
            usage_provider_for("omp", Some("kimi-code"), Some("kimi-k3")),
            "kimi-code"
        );
        assert_eq!(
            usage_provider_for("omp", Some("moonshotai"), Some("kimi-k3")),
            "kimi-code"
        );
        assert_eq!(
            usage_provider_for("omp", Some("kimi"), Some("k3")),
            "kimi-code"
        );
        assert_eq!(
            usage_provider_for("omp", Some("moonshotai"), Some("kimi-for-coding")),
            "kimi-code"
        );
        assert_eq!(
            usage_provider_for("omp", Some("alibaba"), Some("qwen3-max")),
            "other"
        );
        assert_eq!(
            usage_provider_for("omp", Some("moonshotai"), Some("moonshot-v1-8k")),
            "other"
        );
    }

    #[test]
    fn a_fractional_midpoint_does_not_decline_the_partial_hour_offset_check() {
        let zone = intl::Zone::proven(0, 1, &[[0, -300]]).expect("whole-hour zone");
        assert_eq!(
            has_partial_hour_offset("America/New_York", 0.0, 1.0, Some(&zone)),
            Ok(false)
        );
        let partial = intl::Zone::proven(0, 1, &[[0, 330]]).expect("half-hour zone");
        assert_eq!(
            has_partial_hour_offset("Asia/Kolkata", 0.0, 1.0, Some(&partial)),
            Ok(true)
        );
    }
}
