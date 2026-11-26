//! The JSON-object aggregation the typed scan path replaced, kept as the test oracle.
#![allow(dead_code)]
use super::*;

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
            let provider = model.get("provider").and_then(Value::as_str);
            let resolved = cache
                .entry(model_key.clone())
                .or_insert_with(|| pricing.resolve(&model_name, provider))
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

#[cfg(test)]
mod parity {
    use super::super::{Pricing, PricingResolution, Rates};
    use serde_json::{Value, json};

    struct Fixture;
    impl Pricing for Fixture {
        fn resolve(&self, model: &str, provider: Option<&str>) -> PricingResolution {
            let known = model.starts_with("gpt") || provider == Some("anthropic");
            PricingResolution {
                rates: Rates {
                    input: 3.0 + model.len() as f64,
                    output: 15.0,
                    cache_creation: 3.75,
                    cache_read: if provider.is_some() { 0.3 } else { 0.1 },
                },
                source: if known { "models.dev" } else { "fallback" }.into(),
            }
        }
    }

    /// Rows that cover each branch: spellings sharing a label pair, Kimi Code routes, codex
    /// without a provider, logged/unknown/reported costs, future hours, session files, odd numbers.
    fn rows(seed: u64, now: i64) -> (Vec<Value>, Vec<Value>) {
        let mut state = seed;
        let mut next = |n: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % n
        };
        let kinds = ["codex", "claude", "omp", "muse"];
        let models = ["gpt-5", " gpt-5", "opus", "kimi-k3", "", "k3", "gpt-5\u{1}"];
        let providers = [
            None,
            Some(""),
            Some("OpenAI"),
            Some("openai"),
            Some("kimi"),
            Some("claude"),
        ];
        let bases = [None, Some("reported"), Some("unknown")];
        let hour = |ms: i64| {
            let iso = aac_num::ecma::to_iso_string(ms.div_euclid(3_600_000) * 3_600_000).unwrap();
            format!("{} {}", &iso[..10], &iso[11..13])
        };
        let mut rows = Vec::new();
        let mut srows = Vec::new();
        for index in 0..400 {
            let kind = kinds[next(4) as usize];
            let file = format!("file-{}", next(30));
            let model = models[next(models.len() as u64) as usize];
            let at = now - (next(72) as i64) * 3_600_000 + if next(20) == 0 { 7_200_000 } else { 0 };
            let mut row = json!({
                "k": kind, "f": file, "m": model, "h": hour(at),
                "i": next(5000), "o": next(900), "cr": next(3) * 1000, "cw": next(2) * 77,
                "c": if next(3) == 0 { (next(1000) as f64) / 100.0 } else { 0.0 },
                "n": next(4),
            });
            if let Some(provider) = providers[next(providers.len() as u64) as usize] {
                row["p"] = json!(provider);
            }
            if let Some(basis) = bases[next(3) as usize] {
                row["costBasis"] = json!(basis);
            }
            if index % 97 == 0 {
                row["i"] = json!(12.7);
                row["h"] = json!("not an hour");
            }
            rows.push(row);
            if next(2) == 0 {
                let first = at - next(3_000_000) as i64;
                let session = if next(4) == 0 {
                    "not-hex".to_string()
                } else {
                    format!("{:016x}", next(12))
                };
                let mut srow = json!({
                    "k": kind, "f": format!("file-{}", next(30)), "s": session, "m": model,
                    "a": first, "z": if next(25) == 0 { first - 1 } else { at },
                    "i": next(5000), "o": next(900), "cr": next(3) * 1000, "cw": next(2) * 77,
                    "c": if next(3) == 0 { (next(1000) as f64) / 100.0 } else { 0.0 },
                    "n": next(4),
                });
                if let Some(provider) = providers[next(providers.len() as u64) as usize] {
                    srow["p"] = json!(provider);
                }
                srows.push(srow);
            }
        }
        (rows, srows)
    }

    #[test]
    fn typed_scan_aggregation_matches_the_json_object_aggregation() {
        let now = 1_795_000_000_000;
        for seed in 1..=40 {
            let (rows, srows) = rows(seed, now);
            let typed = super::super::worker_from_scan(&rows, &srows, "codex", "ubuntu", now, &Fixture);
            let oracle = super::worker_from_scan(&rows, &srows, "codex", "ubuntu", now, &Fixture);
            assert_eq!(
                serde_json::to_string(&typed).unwrap(),
                serde_json::to_string(&oracle).unwrap(),
                "seed {seed}"
            );
        }
    }
}
