//! The CCS static price table (`model-pricing-data.ts`): rates in USD per million tokens plus the
//! alias ids, loaded once from the generated `data.json`. Two lookup orders matter for parity:
//! the registry keeps its file order (the direct lookup and the suffix-match scan iterate it), and
//! the normalized index reproduces JavaScript object assignment — a later key that normalizes onto
//! an earlier one replaces the value but keeps the earlier position.
use std::sync::OnceLock;

use serde_json::{Map, Value};

use super::resolver::{Pricing, Rates, normalize_model_name};

/// `data.json`: `{registry: {model: rates[, serviceTiers]}}, {aliases: {id: canonical}}`,
/// generated from the TS table so the doubles are identical by construction.
const DATA: &str = include_str!("data.json");

fn rate_of(row: &Value, field: &str) -> Option<f64> {
    row.get(field).and_then(Value::as_f64)
}

fn rates_of(row: &Value) -> Option<Rates> {
    Some(Rates {
        input_per_million: rate_of(row, "inputPerMillion")?,
        output_per_million: rate_of(row, "outputPerMillion")?,
        cache_creation_per_million: rate_of(row, "cacheCreationPerMillion")?,
        cache_read_per_million: rate_of(row, "cacheReadPerMillion")?,
    })
}

/// The parsed table. Entry parse failures (impossible for the generated file) are skipped the way
/// a TS reader would skip a malformed row, and the parity corpus catches any real drift.
pub struct Table {
    /// Registry entries in file order.
    entries: Vec<(String, Pricing)>,
    /// `NORMALIZED_PRICING_REGISTRY`: normalized id → rates, JavaScript assignment order.
    normalized: Vec<(String, Pricing)>,
    /// `MODEL_PRICING_ALIASES` (lookup only; order is never observable).
    aliases: Map<String, Value>,
}

fn parse_pricing(value: &Value) -> Option<Pricing> {
    let row = value.as_object()?;
    let service_tiers = row
        .get("serviceTiers")
        .and_then(Value::as_object)
        .map(|tiers| {
            tiers
                .iter()
                .filter_map(|(name, rates)| Some((name.clone(), rates_of(rates)?)))
                .collect::<Vec<(String, Rates)>>()
        });
    Some(Pricing {
        rates: rates_of(value)?,
        service_tiers,
    })
}

/// Set a Vec-backed ordered map the way JavaScript assigns an object member: a new key appends, an
/// existing key keeps its position and takes the new value.
fn js_assign<V>(map: &mut Vec<(String, V)>, key: String, value: V) {
    match map.iter_mut().find(|(k, _)| *k == key) {
        Some(slot) => slot.1 = value,
        None => map.push((key, value)),
    }
}

impl Table {
    fn load() -> Self {
        let data: Value = serde_json::from_str(DATA).expect("generated pricing table parses");
        let registry = data
            .get("registry")
            .and_then(Value::as_object)
            .expect("table registry");
        let entries: Vec<(String, Pricing)> = registry
            .iter()
            .filter_map(|(key, value)| parse_pricing(value).map(|pricing| (key.clone(), pricing)))
            .collect();
        let mut normalized = Vec::with_capacity(entries.len());
        for (key, pricing) in &entries {
            js_assign(&mut normalized, normalize_model_name(key), pricing.clone());
        }
        let aliases = data
            .get("aliases")
            .and_then(Value::as_object)
            .expect("table aliases")
            .clone();
        Table {
            entries,
            normalized,
            aliases,
        }
    }

    /// The one process-wide table.
    pub fn get() -> &'static Table {
        static TABLE: OnceLock<Table> = OnceLock::new();
        TABLE.get_or_init(Table::load)
    }

    /// `PRICING_REGISTRY[model]`: the exact (un-normalized) key.
    pub fn direct(&self, model: &str) -> Option<&Pricing> {
        self.entries
            .iter()
            .find(|(k, _)| k == model)
            .map(|(_, p)| p)
    }

    /// `NORMALIZED_PRICING_REGISTRY[key]`.
    pub fn normalized(&self, key: &str) -> Option<&Pricing> {
        self.normalized
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, p)| p)
    }

    /// `Object.entries(NORMALIZED_PRICING_REGISTRY)` in order (the suffix-match scan).
    pub fn normalized_entries(&self) -> &[(String, Pricing)] {
        &self.normalized
    }

    /// `MODEL_PRICING_ALIASES[candidate]`.
    pub fn alias(&self, candidate: &str) -> Option<&str> {
        self.aliases.get(candidate).and_then(Value::as_str)
    }
}

/// `Object.keys(PRICING_REGISTRY)` in table order (`getKnownModels` keeps that order).
pub fn known_table_models() -> Vec<String> {
    Table::get()
        .entries
        .iter()
        .map(|(k, _)| k.clone())
        .collect()
}
