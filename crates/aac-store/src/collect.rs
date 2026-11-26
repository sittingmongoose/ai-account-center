//! G14 activity collection aggregation, ported from `analytics-remote-sources.ts` (`toCompact`,
//! `toSessionAggregates`, `toWorkerResult`), `usage/activity-cells.ts` (`cellsFromCompact`) and
//! `usage/account-activity-collector.ts` (`aggregateSessionAggregates`).
//!
//! The usage scanner's (`aac-usage-scan`) compact rows become one tool's worker result: the priced
//! hour/session/route/model cells the report builder shapes and the store ingests, plus the session
//! rows. Pricing resolution stays with the front door (the models.dev registry, CCS static prices
//! and policy overrides it owns); this module applies the resolved facts with the collector's exact
//! arithmetic — a logged cost wins, the rest prices at list rates, and the fallback part is kept
//! apart. [`Pricing`] is the fed-facts seam the host supplies and the parity vectors pin.
use crate::scan_row::{Interner, ScanRow};
use aac_num::ecma::{parse_instant, to_iso_string};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::Arc;

const HOUR: i64 = 3_600_000;
/// `FUTURE_SLACK_MS`: records dated more than this after the read are counted, not written.
pub const FUTURE_SLACK_MS: i64 = 5 * 60_000;
const MAX_ROUTE: usize = 64;
const MAX_MODEL: usize = 200;
/// `MAX_SESSION_ROWS`: one read keeps at most this many sessions.
const MAX_SESSION_ROWS: usize = 10_000;

/// The provider aliases `normalizeModelsDevProviderId` maps (trim + lower case first).
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

/// `normalizeUsageProvider`: the routing provider's canonical id, or None for an empty one.
pub fn normalize_provider(provider: Option<&str>) -> Option<String> {
    let provider = provider?;
    let normalized = provider.trim().to_lowercase();
    if normalized.is_empty() {
        return None;
    }
    Some(
        PROVIDER_ALIASES
            .iter()
            .find(|(from, _)| *from == normalized)
            .map(|(_, to)| (*to).to_string())
            .unwrap_or(normalized),
    )
}

/// True when a usage row is Kimi Code (OMP logs only). Keep in sync with
/// `aac_usage::read::kimi::is_kimi_code_usage` and Analytics attribution.
pub fn is_kimi_code_usage(model: Option<&str>, route: Option<&str>) -> bool {
    let route = route.unwrap_or("").trim().to_ascii_lowercase();
    let route = route.strip_prefix("builtin:").unwrap_or(&route);
    if matches!(route, "kimi-code" | "kimi") {
        return true;
    }
    let model = model.unwrap_or("").trim().to_ascii_lowercase();
    let name = model.rsplit('/').next().unwrap_or(&model);
    name == "k3" || name == "kimi-k3" || name == "kimi-for-coding" || name == "kimi-code" || name.starts_with("kimi-")
}

/// The compact/session provider id: Kimi Code rows keep the dashboard route.
fn scan_provider(kind: &str, model: Option<&str>, logged: Option<&str>) -> Option<String> {
    if is_kimi_code_usage(model, logged) {
        Some("kimi-code".to_string())
    } else {
        remote_row_provider(kind, logged)
    }
}

fn sha256_hex(text: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    hasher.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// `analyticsSessionKey`: the published key, first 16 hex of sha256("aac-session-v1:tool:id").
pub fn session_key(tool: &str, session_id: &str) -> String {
    sha256_hex(&format!("aac-session-v1:{tool}:{session_id}"))
        .chars()
        .take(16)
        .collect()
}

/// `analyticsSessionHash`: the full 64-hex digest of the same string.
pub fn session_hash(tool: &str, session_id: &str) -> String {
    sha256_hex(&format!("aac-session-v1:{tool}:{session_id}"))
}

fn is_hex16(text: &str) -> bool {
    text.len() == 16 && text.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The resolved list rates for one model (per million tokens).
#[derive(Clone, Copy, Debug)]
pub struct Rates {
    /// Input tokens per million.
    pub input: f64,
    /// Output tokens per million.
    pub output: f64,
    /// Cache creation tokens per million.
    pub cache_creation: f64,
    /// Cache read tokens per million.
    pub cache_read: f64,
}

/// One model's resolved pricing: its list rates and where they came from (`fallback` marks the
/// unknown-model rate, whose part is kept apart as `fallbackCostUsd`).
#[derive(Clone, Debug)]
pub struct PricingResolution {
    /// The list rates.
    pub rates: Rates,
    /// The resolution source word.
    pub source: String,
}

/// The fed pricing facts: the front door resolves a model (with its routing provider) to list
/// rates. The host implements this from the models.dev registry it owns; tests pin a fixture map.
pub trait Pricing {
    /// Resolve `model` under `provider` (None when the row names no routing provider).
    fn resolve(&self, model: &str, provider: Option<&str>) -> PricingResolution;
}

/// A label the store accepts: no control characters, trimmed, bounded; never empty for a model.
fn label(value: &str, max: usize) -> String {
    let cleaned: String = value.chars().filter(|c| !(*c <= '\u{1f}' || *c == '\u{7f}')).collect();
    cleaned.trim().chars().take(max).collect()
}

fn iso(ms: i64) -> String {
    to_iso_string(ms).expect("collection time within the ISO domain")
}

/// `remoteRowProvider`: the routing provider prices the row; codex carries none of its own.
fn remote_row_provider(kind: &str, route: Option<&str>) -> Option<String> {
    match route {
        Some(route) if !route.is_empty() => Some(route.to_string()),
        _ => {
            if kind == "codex" {
                None
            } else {
                Some(String::new())
            }
        }
    }
}

/// The compact entry's provider (`toCompact`), or the row's tool where it names none: the
/// routing word `cellsFromCompact` and `aggregateSessionAggregates` read.
fn routing<'a>(row: &'a ScanRow, provider: &'a Option<String>) -> &'a str {
    provider.as_deref().unwrap_or(&row.kind)
}

/// List rates applied to the unlogged token counts (input, output, cache write, cache read).
fn listed(unlogged: [f64; 4], rates: Rates) -> f64 {
    let [input, output, write, read] = unlogged;
    (input / 1_000_000.0) * rates.input
        + (output / 1_000_000.0) * rates.output
        + (write / 1_000_000.0) * rates.cache_creation
        + (read / 1_000_000.0) * rates.cache_read
}

/// The row's tokens in the collector's order: input, output, cache creation, cache read.
fn entry_tokens(row: &ScanRow) -> [u64; 4] {
    let [input, output, read, write] = row.tokens;
    [input, output, write, read]
}

const TOKEN_FIELDS: [&str; 4] = ["inputTokens", "outputTokens", "cacheCreationTokens", "cacheReadTokens"];

struct Slot {
    hour: String,
    session_key: Option<String>,
    route: usize,
    tokens: [u64; 4],
    events: u64,
    logged: f64,
    unlogged: [f64; 4],
}

/// `toCompact` then `cellsFromCompact` (keyed): the absolute totals of each UTC hour, session,
/// route and model, priced with the fed facts. A row's session is the published key its file's
/// session row names. Returns the cells and the count of future-dated records left out.
fn cells_from_rows(
    rows: &[&ScanRow],
    session_of: &HashMap<&str, &str>,
    now: i64,
    pricing: &dyn Pricing,
) -> (Vec<Value>, u64) {
    let limit = now + FUTURE_SLACK_MS;
    let mut hours: HashMap<&str, Option<i64>> = HashMap::new();
    // (tool, model, logged provider) to the (route, modelRaw) pair's index.
    let mut labels: HashMap<(&str, &str, Option<&str>), usize> = HashMap::new();
    let mut routes: Vec<(String, String)> = Vec::new();
    let mut route_index: HashMap<(String, String), usize> = HashMap::new();
    let mut order: Vec<Slot> = Vec::new();
    let mut slots: HashMap<(i64, Option<&str>, usize), usize> = HashMap::new();
    let mut future = 0u64;
    for row in rows {
        let hour = row.hour.as_deref().unwrap_or("");
        let at = *hours
            .entry(hour)
            .or_insert_with(|| parse_instant(&format!("{}:00Z", hour.replace(' ', "T"))));
        let Some(at) = at else {
            continue;
        };
        if at > limit {
            future += row.events.max(1);
            continue;
        }
        let route = *labels
            .entry((&row.kind, &row.model, row.provider.as_deref()))
            .or_insert_with(|| {
                let provider = scan_provider(&row.kind, Some(&row.model), row.provider.as_deref());
                let provider = routing(row, &provider);
                let model_raw = {
                    let raw = label(&row.model, MAX_MODEL);
                    if raw.is_empty() { "unknown".to_string() } else { raw }
                };
                let route = if is_kimi_code_usage(Some(&model_raw), Some(provider)) {
                    "kimi-code".to_string()
                } else {
                    label(&normalize_provider(Some(provider)).unwrap_or_default(), MAX_ROUTE).to_lowercase()
                };
                // Different spellings can share one label pair; they share its cell.
                *route_index
                    .entry((route.clone(), model_raw.clone()))
                    .or_insert_with(|| {
                        routes.push((route, model_raw));
                        routes.len() - 1
                    })
            });
        let session = session_of.get(&*row.file).copied().filter(|key| is_hex16(key));
        let hour_ms = at.div_euclid(HOUR) * HOUR;
        let index = *slots.entry((hour_ms, session, route)).or_insert_with(|| {
            order.push(Slot {
                hour: iso(hour_ms),
                session_key: session.map(str::to_string),
                route,
                tokens: [0; 4],
                events: 0,
                logged: 0.0,
                unlogged: [0.0; 4],
            });
            order.len() - 1
        });
        let slot = &mut order[index];
        let tokens = entry_tokens(row);
        for (total, value) in slot.tokens.iter_mut().zip(tokens) {
            *total += value;
        }
        slot.events += row.events;
        if row.cost > 0.0 && row.cost_basis.as_deref() != Some("unknown") {
            slot.logged += row.cost;
        } else {
            for (pending, value) in slot.unlogged.iter_mut().zip(tokens) {
                *pending += value as f64;
            }
        }
    }
    let mut resolved: HashMap<usize, PricingResolution> = HashMap::new();
    let mut cells = Vec::new();
    for slot in order {
        let (route, model_raw) = &routes[slot.route];
        let resolution = resolved.entry(slot.route).or_insert_with(|| {
            let provider = if route.is_empty() { None } else { Some(route.as_str()) };
            pricing.resolve(model_raw, provider)
        });
        let listed = listed(slot.unlogged, resolution.rates);
        if slot.tokens.iter().sum::<u64>() == 0 && slot.logged == 0.0 {
            continue;
        }
        let mut cell = Map::new();
        cell.insert("hour".into(), json!(slot.hour));
        cell.insert("sessionKey".into(), json!(slot.session_key));
        cell.insert("route".into(), json!(route));
        cell.insert("modelRaw".into(), json!(model_raw));
        for (field, value) in TOKEN_FIELDS.iter().zip(slot.tokens) {
            cell.insert((*field).into(), json!(value));
        }
        cell.insert("events".into(), json!(slot.events));
        cell.insert(
            "loggedCostUsd".into(),
            if slot.logged > 0.0 {
                json!(slot.logged)
            } else {
                Value::Null
            },
        );
        cell.insert("costUsd".into(), json!(slot.logged + listed));
        cell.insert(
            "fallbackCostUsd".into(),
            json!(if resolution.source == "fallback" { listed } else { 0.0 }),
        );
        cells.push(Value::Object(cell));
    }
    (cells, future)
}

struct ModelSlot {
    key: (Option<String>, Arc<str>),
    tokens: [u64; 4],
    logged: f64,
    unlogged: [f64; 4],
}

struct SessionBucket {
    /// The model breakdowns, in first-seen order.
    models: Vec<ModelSlot>,
    first_ms: i64,
    last_ms: i64,
    target: Arc<str>,
}

/// `toSessionAggregates` then `aggregateSessionAggregates`: the session rows the report builder's
/// `sessionsOf` reads, priced with the fed facts. Only the fields `sessionsOf` consumes are shaped
/// (key, first/last, target, model breakdowns); the projection's extra labels stay with the front
/// door.
fn sessions_from_rows(srows: &[&ScanRow], pricing: &dyn Pricing) -> Vec<Value> {
    let mut order: Vec<SessionBucket> = Vec::new();
    let mut ids: Vec<&str> = Vec::new();
    let mut sessions: HashMap<&str, usize> = HashMap::new();
    for row in srows {
        let session_id = row.session.as_deref().unwrap_or("");
        if session_id.is_empty() {
            continue;
        }
        let (first, last) = (row.first.unwrap_or(0), row.last.unwrap_or(0));
        if last < first {
            continue;
        }
        let index = match sessions.get(session_id) {
            Some(index) => *index,
            None => {
                if sessions.len() >= MAX_SESSION_ROWS {
                    continue;
                }
                sessions.insert(session_id, order.len());
                ids.push(session_id);
                order.push(SessionBucket {
                    models: Vec::new(),
                    first_ms: first,
                    last_ms: last,
                    target: Arc::clone(&row.kind),
                });
                order.len() - 1
            }
        };
        let bucket = &mut order[index];
        bucket.first_ms = bucket.first_ms.min(first);
        bucket.last_ms = bucket.last_ms.max(last);
        bucket.target = Arc::clone(&row.kind);
        let provider = scan_provider(&row.kind, Some(&row.model), row.provider.as_deref());
        let provider = normalize_provider(Some(routing(row, &provider)));
        let key = (provider, Arc::clone(&row.model));
        let model = match bucket.models.iter().position(|model| model.key == key) {
            Some(position) => &mut bucket.models[position],
            None => {
                bucket.models.push(ModelSlot {
                    key,
                    tokens: [0; 4],
                    logged: 0.0,
                    unlogged: [0.0; 4],
                });
                bucket.models.last_mut().expect("model just pushed")
            }
        };
        let tokens = entry_tokens(row);
        for (total, value) in model.tokens.iter_mut().zip(tokens) {
            *total += value;
        }
        if row.cost > 0.0 {
            model.logged += row.cost;
        } else {
            for (pending, value) in model.unlogged.iter_mut().zip(tokens) {
                *pending += value as f64;
            }
        }
    }
    let mut resolved: HashMap<(Option<String>, Arc<str>), PricingResolution> = HashMap::new();
    let mut out: Vec<(String, Value)> = Vec::new();
    for (session_id, bucket) in ids.into_iter().zip(order) {
        let mut breakdowns: Vec<(f64, Value)> = Vec::new();
        for model in bucket.models {
            let (provider, name) = &model.key;
            let resolution = resolved
                .entry(model.key.clone())
                .or_insert_with(|| pricing.resolve(name, provider.as_deref()));
            let listed = listed(model.unlogged, resolution.rates);
            let cost = model.logged + listed;
            let mut breakdown = Map::new();
            breakdown.insert("modelName".into(), json!(&**name));
            if let Some(provider) = provider {
                breakdown.insert("provider".into(), json!(provider));
            }
            for (field, value) in TOKEN_FIELDS.iter().zip(model.tokens) {
                breakdown.insert((*field).into(), json!(value));
            }
            breakdown.insert("cost".into(), json!(cost));
            if resolution.source == "fallback" && listed > 0.0 {
                breakdown.insert("fallbackCost".into(), json!(listed));
            }
            breakdowns.push((cost, Value::Object(breakdown)));
        }
        // The collector sorts breakdowns by cost descending; sessionsOf keeps that order.
        breakdowns.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        let last = iso(bucket.last_ms);
        let mut session = Map::new();
        session.insert("sessionId".into(), json!(session_id));
        session.insert("firstActivity".into(), json!(iso(bucket.first_ms)));
        session.insert("lastActivity".into(), json!(last));
        session.insert(
            "modelBreakdowns".into(),
            Value::Array(breakdowns.into_iter().map(|(_, b)| b).collect()),
        );
        session.insert("target".into(), json!(&*bucket.target));
        out.push((last, Value::Object(session)));
    }
    // Newest session first (the collector's lastActivity localeCompare descending).
    out.sort_by(|a, b| b.0.cmp(&a.0));
    out.into_iter().map(|(_, session)| session).collect()
}

/// `toWorkerResult` for the scanner's answer: one tool's cells and session rows read on `host`,
/// priced with the fed facts. The report builder shapes these into the store's usage reports.
pub fn worker_from_rows(rows: &[&ScanRow], srows: &[&ScanRow], host: &str, now: i64, pricing: &dyn Pricing) -> Value {
    // A row's file names its session through the session rows (keyed on the host that read them).
    let mut session_of: HashMap<&str, &str> = HashMap::new();
    for row in srows {
        if let Some(session) = row.session.as_deref()
            && !row.file.is_empty()
            && !session.is_empty()
        {
            session_of.insert(&row.file, session);
        }
    }
    let (cells, future) = cells_from_rows(rows, &session_of, now, pricing);
    let session = sessions_from_rows(srows, pricing);
    let mut result = Map::new();
    result.insert("hourly".into(), json!([]));
    result.insert("cells".into(), Value::Array(cells));
    if future > 0 {
        result.insert("futureDated".into(), json!(future));
    }
    result.insert("host".into(), json!(host));
    result.insert("session".into(), Value::Array(session));
    Value::Object(result)
}

/// [`worker_from_rows`] for rows held as the scanner's JSON objects.
pub fn worker_from_scan(
    rows: &[Value],
    srows: &[Value],
    _kind: &str,
    host: &str,
    now: i64,
    pricing: &dyn Pricing,
) -> Value {
    let mut interner = Interner::default();
    let mut typed =
        |rows: &[Value]| -> Vec<ScanRow> { rows.iter().map(|row| ScanRow::from_value(row, &mut interner)).collect() };
    let (rows, srows) = (typed(rows), typed(srows));
    worker_from_rows(
        &rows.iter().collect::<Vec<_>>(),
        &srows.iter().collect::<Vec<_>>(),
        host,
        now,
        pricing,
    )
}

#[cfg(test)]
#[path = "collect_value_oracle.rs"]
mod oracle;
