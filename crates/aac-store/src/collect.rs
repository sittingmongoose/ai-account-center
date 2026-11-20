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
use aac_num::ecma::{parse_instant, to_iso_string};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

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

fn as_f64(value: &Value) -> f64 {
    value.as_f64().unwrap_or(0.0)
}

/// `Math.floor(v)` for a finite number, else 0 (the collector's token counts are non-negative).
fn floor_count(value: &Value) -> i64 {
    let n = as_f64(value);
    if n.is_finite() { n.floor() as i64 } else { 0 }
}

/// `count`: a finite positive number floored, else 0.
fn count(value: &Value) -> u64 {
    let n = as_f64(value);
    if n.is_finite() && n > 0.0 { n.floor() as u64 } else { 0 }
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

/// `toCompact`: the scanner's compact rows as the collector's compact entries.
pub fn to_compact(rows: &[Value]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let kind = row["k"].as_str().unwrap_or("");
            let provider = scan_provider(kind, row["m"].as_str(), row["p"].as_str());
            let hour = row["h"].as_str().unwrap_or("").replace(' ', "T");
            let mut entry = Map::new();
            entry.insert("inputTokens".into(), json!(floor_count(&row["i"])));
            entry.insert("outputTokens".into(), json!(floor_count(&row["o"])));
            entry.insert("cacheCreationTokens".into(), json!(floor_count(&row["cw"])));
            entry.insert("cacheReadTokens".into(), json!(floor_count(&row["cr"])));
            entry.insert("model".into(), json!(row["m"].as_str().unwrap_or("")));
            entry.insert("sessionId".into(), json!(""));
            entry.insert("timestamp".into(), json!(format!("{hour}:00Z")));
            entry.insert("projectPath".into(), json!(""));
            entry.insert("target".into(), json!(kind));
            if let Some(provider) = provider {
                entry.insert("provider".into(), json!(provider));
            }
            let cost = as_f64(&row["c"]);
            match row["costBasis"].as_str() {
                Some("unknown") => {
                    entry.insert("costBasis".into(), json!("unknown"));
                }
                Some("reported") => {
                    entry.insert("costUsd".into(), json!(cost));
                    entry.insert("costBasis".into(), json!("reported"));
                }
                _ => {
                    if cost > 0.0 {
                        entry.insert("costUsd".into(), json!(cost));
                    }
                }
            }
            json!({ "entry": Value::Object(entry), "events": floor_count(&row["n"]) })
        })
        .collect()
}

/// `toSessionAggregates`: the scanner's session rows as the collector's session aggregates.
pub fn to_session_aggregates(srows: &[Value]) -> Vec<Value> {
    srows
        .iter()
        .map(|row| {
            let kind = row["k"].as_str().unwrap_or("");
            let provider = scan_provider(kind, row["m"].as_str(), row["p"].as_str());
            let mut out = Map::new();
            out.insert("sessionId".into(), json!(row["s"].as_str().unwrap_or("")));
            out.insert("model".into(), json!(row["m"].as_str().unwrap_or("")));
            if let Some(provider) = provider {
                out.insert("provider".into(), json!(provider));
            }
            out.insert("target".into(), json!(kind));
            out.insert("firstMs".into(), json!(floor_count(&row["a"])));
            out.insert("lastMs".into(), json!(floor_count(&row["z"])));
            out.insert("inputTokens".into(), json!(floor_count(&row["i"])));
            out.insert("outputTokens".into(), json!(floor_count(&row["o"])));
            out.insert("cacheCreationTokens".into(), json!(floor_count(&row["cw"])));
            out.insert("cacheReadTokens".into(), json!(floor_count(&row["cr"])));
            let cost = as_f64(&row["c"]);
            if cost > 0.0 {
                out.insert("cost".into(), json!(cost));
            }
            out.insert("events".into(), json!(floor_count(&row["n"])));
            Value::Object(out)
        })
        .collect()
}

struct Slot {
    cell: Map<String, Value>,
    logged: f64,
    unlogged: [f64; 4],
}

/// `cellsFromCompact`: the absolute totals of each UTC hour, session, route and model, priced with
/// the fed facts. `keyed`: the rows' session ids are already published keys. Returns the cells and
/// the count of future-dated records left out.
pub fn cells_from_compact(
    rows: &[Value],
    tool: &str,
    keyed: bool,
    now: i64,
    pricing: &dyn Pricing,
) -> (Vec<Value>, u64) {
    let limit = now + FUTURE_SLACK_MS;
    let mut order: Vec<String> = Vec::new();
    let mut slots: HashMap<String, Slot> = HashMap::new();
    let mut future = 0u64;
    for row in rows {
        let entry = &row["entry"];
        let Some(at) = entry["timestamp"].as_str().and_then(parse_instant) else {
            continue;
        };
        if at > limit {
            future += count(&row["events"]).max(1);
            continue;
        }
        let hour = iso(at.div_euclid(HOUR) * HOUR);
        let provider = entry["provider"].as_str().or_else(|| entry["target"].as_str());
        let model_raw = {
            let raw = label(entry["model"].as_str().unwrap_or(""), MAX_MODEL);
            if raw.is_empty() { "unknown".to_string() } else { raw }
        };
        let route = if is_kimi_code_usage(Some(&model_raw), provider) {
            "kimi-code".to_string()
        } else {
            label(&normalize_provider(provider).unwrap_or_default(), MAX_ROUTE).to_lowercase()
        };
        let raw = entry["sessionId"].as_str().unwrap_or("");
        let session_key = if raw.is_empty() {
            None
        } else if keyed {
            is_hex16(raw).then(|| raw.to_string())
        } else {
            Some(session_key(tool, raw))
        };
        let session_hash = if !raw.is_empty() && !keyed {
            Some(session_hash(tool, raw))
        } else {
            None
        };
        let id = format!(
            "{hour}\u{0}{}\u{0}{route}\u{0}{model_raw}",
            session_key.as_deref().unwrap_or("")
        );
        if !slots.contains_key(&id) {
            let mut cell = Map::new();
            cell.insert("hour".into(), json!(hour));
            cell.insert("sessionKey".into(), json!(session_key));
            if let Some(hash) = session_hash {
                cell.insert("sessionHash".into(), json!(hash));
            }
            cell.insert("route".into(), json!(route));
            cell.insert("modelRaw".into(), json!(model_raw));
            cell.insert("inputTokens".into(), json!(0));
            cell.insert("outputTokens".into(), json!(0));
            cell.insert("cacheCreationTokens".into(), json!(0));
            cell.insert("cacheReadTokens".into(), json!(0));
            cell.insert("events".into(), json!(0));
            order.push(id.clone());
            slots.insert(
                id.clone(),
                Slot {
                    cell,
                    logged: 0.0,
                    unlogged: [0.0; 4],
                },
            );
        }
        let slot = slots.get_mut(&id).expect("slot just inserted");
        let tokens = [
            count(&entry["inputTokens"]),
            count(&entry["outputTokens"]),
            count(&entry["cacheCreationTokens"]),
            count(&entry["cacheReadTokens"]),
        ];
        for (field, value) in ["inputTokens", "outputTokens", "cacheCreationTokens", "cacheReadTokens"]
            .iter()
            .zip(tokens)
        {
            let current = slot.cell[*field].as_u64().unwrap_or(0);
            slot.cell.insert((*field).into(), json!(current + value));
        }
        let events = slot.cell["events"].as_u64().unwrap_or(0) + count(&row["events"]);
        slot.cell.insert("events".into(), json!(events));
        let cost_usd = entry["costUsd"].as_f64();
        if cost_usd.is_some_and(|c| c > 0.0) {
            slot.logged += cost_usd.unwrap_or(0.0);
        } else {
            for (i, value) in tokens.iter().enumerate() {
                slot.unlogged[i] += *value as f64;
            }
        }
    }
    let mut cache: HashMap<String, PricingResolution> = HashMap::new();
    let mut cells = Vec::new();
    for id in order {
        let slot = slots.remove(&id).expect("ordered slot");
        let route = slot.cell["route"].as_str().unwrap_or("").to_string();
        let model_raw = slot.cell["modelRaw"].as_str().unwrap_or("").to_string();
        let key = format!("{route}\u{0}{model_raw}");
        let resolved = cache
            .entry(key)
            .or_insert_with(|| {
                let provider = if route.is_empty() { None } else { Some(route.as_str()) };
                pricing.resolve(&model_raw, provider)
            })
            .clone();
        let rates = resolved.rates;
        let [input, output, write, read] = slot.unlogged;
        let listed = (input / 1_000_000.0) * rates.input
            + (output / 1_000_000.0) * rates.output
            + (write / 1_000_000.0) * rates.cache_creation
            + (read / 1_000_000.0) * rates.cache_read;
        let tokens = slot.cell["inputTokens"].as_u64().unwrap_or(0)
            + slot.cell["outputTokens"].as_u64().unwrap_or(0)
            + slot.cell["cacheCreationTokens"].as_u64().unwrap_or(0)
            + slot.cell["cacheReadTokens"].as_u64().unwrap_or(0);
        if tokens == 0 && slot.logged == 0.0 {
            continue;
        }
        let mut cell = slot.cell;
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
            json!(if resolved.source == "fallback" { listed } else { 0.0 }),
        );
        cells.push(Value::Object(cell));
    }
    (cells, future)
}

struct SessionBucket {
    /// Model key to its breakdown (insertion order kept).
    models: Vec<(String, Map<String, Value>)>,
    logged: HashMap<String, f64>,
    unlogged: HashMap<String, [f64; 4]>,
    first_ms: i64,
    last_ms: i64,
    target: Option<String>,
}

/// `aggregateSessionAggregates`: the session rows the report builder's `sessionsOf` reads, priced
/// with the fed facts. Only the fields `sessionsOf` consumes are shaped (key, first/last, target,
/// model breakdowns); the projection's extra labels stay with the front door.
pub fn aggregate_sessions(rows: &[Value], pricing: &dyn Pricing) -> Vec<Value> {
    let mut order: Vec<String> = Vec::new();
    let mut sessions: HashMap<String, SessionBucket> = HashMap::new();
    for row in rows {
        let Some(session_id) = row["sessionId"].as_str() else {
            continue;
        };
        if session_id.is_empty() {
            continue;
        }
        let first = row["firstMs"].as_f64();
        let last = row["lastMs"].as_f64();
        let (Some(first), Some(last)) = (first, last) else {
            continue;
        };
        if !first.is_finite() || !last.is_finite() || last < first {
            continue;
        }
        let first = first as i64;
        let last = last as i64;
        if !sessions.contains_key(session_id) {
            if sessions.len() >= MAX_SESSION_ROWS {
                continue;
            }
            order.push(session_id.to_string());
            sessions.insert(
                session_id.to_string(),
                SessionBucket {
                    models: Vec::new(),
                    logged: HashMap::new(),
                    unlogged: HashMap::new(),
                    first_ms: first,
                    last_ms: last,
                    target: row["target"].as_str().map(str::to_string),
                },
            );
        }
        let bucket = sessions.get_mut(session_id).expect("bucket");
        bucket.first_ms = bucket.first_ms.min(first);
        bucket.last_ms = bucket.last_ms.max(last);
        if let Some(target) = row["target"].as_str() {
            bucket.target = Some(target.to_string());
        }
        let provider = normalize_provider(row["provider"].as_str().or_else(|| row["target"].as_str()));
        let model_name = row["model"].as_str().unwrap_or("").to_string();
        let model_key = format!("{}\u{0}{model_name}", provider.as_deref().unwrap_or(""));
        if !bucket.models.iter().any(|(k, _)| k == &model_key) {
            let mut model = Map::new();
            model.insert("modelName".into(), json!(model_name));
            if let Some(provider) = &provider {
                model.insert("provider".into(), json!(provider));
            }
            model.insert("inputTokens".into(), json!(0));
            model.insert("outputTokens".into(), json!(0));
            model.insert("cacheCreationTokens".into(), json!(0));
            model.insert("cacheReadTokens".into(), json!(0));
            model.insert("cost".into(), json!(0.0));
            bucket.models.push((model_key.clone(), model));
        }
        let model = &mut bucket
            .models
            .iter_mut()
            .find(|(k, _)| k == &model_key)
            .expect("model")
            .1;
        for field in ["inputTokens", "outputTokens", "cacheCreationTokens", "cacheReadTokens"] {
            let current = model[field].as_u64().unwrap_or(0);
            model.insert(field.into(), json!(current + count(&row[field])));
        }
        let cost = row["cost"].as_f64();
        if cost.is_some_and(|c| c > 0.0) {
            *bucket.logged.entry(model_key.clone()).or_insert(0.0) += cost.unwrap_or(0.0);
        } else {
            let pending = bucket.unlogged.entry(model_key.clone()).or_insert([0.0; 4]);
            for (i, field) in ["inputTokens", "outputTokens", "cacheCreationTokens", "cacheReadTokens"]
                .iter()
                .enumerate()
            {
                pending[i] += count(&row[field]) as f64;
            }
        }
    }
    let mut cache: HashMap<String, PricingResolution> = HashMap::new();
    let mut out: Vec<Value> = Vec::new();
    for session_id in order {
        let bucket = sessions.remove(&session_id).expect("ordered bucket");
        let mut breakdowns: Vec<Value> = Vec::new();
        for (model_key, mut model) in bucket.models {
            let model_name = model["modelName"].as_str().unwrap_or("").to_string();
            let provider = model["provider"].as_str().map(str::to_string);
            let resolved = cache
                .entry(model_key.clone())
                .or_insert_with(|| pricing.resolve(&model_name, provider.as_deref()))
                .clone();
            let rates = resolved.rates;
            let [input, output, write, read] = bucket.unlogged.get(&model_key).copied().unwrap_or([0.0; 4]);
            let listed = (input / 1_000_000.0) * rates.input
                + (output / 1_000_000.0) * rates.output
                + (write / 1_000_000.0) * rates.cache_creation
                + (read / 1_000_000.0) * rates.cache_read;
            let cost = bucket.logged.get(&model_key).copied().unwrap_or(0.0) + listed;
            model.insert("cost".into(), json!(cost));
            if resolved.source == "fallback" && listed > 0.0 {
                model.insert("fallbackCost".into(), json!(listed));
            }
            breakdowns.push(Value::Object(model));
        }
        // The collector sorts breakdowns by cost descending; sessionsOf keeps that order.
        breakdowns.sort_by(|a, b| {
            as_f64(&b["cost"])
                .partial_cmp(&as_f64(&a["cost"]))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let mut session = Map::new();
        session.insert("sessionId".into(), json!(session_id));
        session.insert("firstActivity".into(), json!(iso(bucket.first_ms)));
        session.insert("lastActivity".into(), json!(iso(bucket.last_ms)));
        session.insert("modelBreakdowns".into(), Value::Array(breakdowns));
        if let Some(target) = bucket.target {
            session.insert("target".into(), json!(target));
        }
        out.push(Value::Object(session));
    }
    // Newest session first (the collector's lastActivity localeCompare descending).
    out.sort_by(|a, b| {
        b["lastActivity"]
            .as_str()
            .unwrap_or("")
            .cmp(a["lastActivity"].as_str().unwrap_or(""))
    });
    out
}

/// `toWorkerResult` for the scanner's answer: one tool's cells and session rows read on `host`,
/// priced with the fed facts. The report builder shapes these into the store's usage reports.
pub fn worker_from_scan(
    rows: &[Value],
    srows: &[Value],
    kind: &str,
    host: &str,
    now: i64,
    pricing: &dyn Pricing,
) -> Value {
    let compact = to_compact(rows);
    // A row's file names its session through the session rows (keyed on the host that read them).
    let mut session_of: HashMap<String, String> = HashMap::new();
    for row in srows {
        if let (Some(file), Some(key)) = (row["f"].as_str(), row["s"].as_str())
            && !file.is_empty()
            && !key.is_empty()
        {
            session_of.insert(file.to_string(), key.to_string());
        }
    }
    let keyed_compact: Vec<Value> = compact
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let file = rows[index]["f"].as_str().unwrap_or("");
            let session_id = session_of.get(file).cloned().unwrap_or_default();
            let mut entry = entry.clone();
            entry["entry"]["sessionId"] = json!(session_id);
            entry
        })
        .collect();
    let (cells, future) = cells_from_compact(&keyed_compact, kind, true, now, pricing);
    let session = aggregate_sessions(&to_session_aggregates(srows), pricing);
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
