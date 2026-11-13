//! The account's usage outside the scanned computers (DESIGN 17.4). Cursor's events are every
//! request on every device, T3's runs included; the local runs reader places T3's runs on a
//! computer. Never add the two: local hours cover the account's tokens, the rest is outside.
//!
//! An event carries no run id, so the match is by model and time, per token kind. A local run
//! counts in the hour it finished, its requests are billed through its run: a local hour covers
//! the account's same hour first, then the hour before, then the hour after (clock skew), never
//! further, greedily, never below zero. Models compare without a `cursor-` prefix and effort,
//! `-thinking` or `-fast` suffix; rows keep their own spelling.

use crate::util;
use serde_json::{Value, json};
use std::collections::BTreeMap;

const HOUR_MS: i64 = 3_600_000;

#[derive(Clone, Debug, Default)]
struct Cell {
    tokens: [u64; 4],
    original: [u64; 4],
    events: u64,
    cents: f64,
    uncosted: u64,
}

fn tokens_of(row: &Value) -> Option<[u64; 4]> {
    let get = |key: &str| row.get(key).and_then(Value::as_u64);
    Some([get("i")?, get("o")?, get("cr")?, get("cw")?])
}

/// Rows `{h, m, i, o, cr, cw[, n, cents, uncosted]}`; a malformed row is skipped.
fn rows(value: Option<&Value>) -> Vec<(i64, String, [u64; 4], u64, f64, u64)> {
    let Some(Value::Array(items)) = value else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|row| {
            let hour = util::hour_epoch(row.get("h")?.as_str()?)?;
            let model = row.get("m")?.as_str()?.to_string();
            let tokens = tokens_of(row)?;
            let events = row.get("n").and_then(Value::as_u64).unwrap_or(0);
            let cents = row
                .get("cents")
                .and_then(Value::as_f64)
                .filter(|c| c.is_finite() && *c >= 0.0)
                .unwrap_or(0.0);
            let uncosted = row.get("uncosted").and_then(Value::as_u64).unwrap_or(0);
            Some((hour, model, tokens, events, cents, uncosted))
        })
        .collect()
}

/// `{account: rows, local: rows}` in, `{rows, summary}` out: the account's rows minus what the
/// local runs account for, and the four totals that say how much was matched.
pub fn dedupe(request: &Value) -> Value {
    // (match model, hour) -> raw model -> cell
    let mut account: BTreeMap<(String, i64), BTreeMap<String, Cell>> = BTreeMap::new();
    for (hour, model, tokens, events, cents, uncosted) in rows(request.get("account")) {
        let cell = account
            .entry((util::match_model(&model), hour))
            .or_default()
            .entry(model)
            .or_default();
        for ((now, original), value) in cell.tokens.iter_mut().zip(cell.original.iter_mut()).zip(tokens) {
            *now += value;
            *original += value;
        }
        cell.events += events;
        cell.cents += cents;
        cell.uncosted += uncosted;
    }
    let mut local: BTreeMap<(String, i64), [u64; 4]> = BTreeMap::new();
    for (hour, model, tokens, ..) in rows(request.get("local")) {
        let cell = local.entry((util::match_model(&model), hour)).or_default();
        for (total, value) in cell.iter_mut().zip(tokens) {
            *total += value;
        }
    }
    let mut unmatched = [0u64; 4];
    for ((model, hour), mut left) in local {
        for target in [hour, hour - HOUR_MS, hour + HOUR_MS] {
            let Some(cells) = account.get_mut(&(model.clone(), target)) else {
                continue;
            };
            for cell in cells.values_mut() {
                for (want, have) in left.iter_mut().zip(cell.tokens.iter_mut()) {
                    let take = (*want).min(*have);
                    *have -= take;
                    *want -= take;
                }
            }
        }
        for (total, value) in unmatched.iter_mut().zip(left) {
            *total += value;
        }
    }
    let mut out = Vec::new();
    let (mut total, mut outside) = ([0u64; 4], [0u64; 4]);
    for ((_, hour), cells) in account {
        for (model, cell) in cells {
            for i in 0..4 {
                total[i] += cell.original[i];
                outside[i] += cell.tokens[i];
            }
            let left: u64 = cell.tokens.iter().sum();
            if left == 0 {
                continue;
            }
            let whole: u64 = cell.original.iter().sum();
            let share = if whole == 0 { 1.0 } else { left as f64 / whole as f64 };
            out.push(json!({
                "h": util::hour_label(hour),
                "m": model,
                "i": cell.tokens[0],
                "o": cell.tokens[1],
                "cr": cell.tokens[2],
                "cw": cell.tokens[3],
                "n": ((cell.events as f64 * share).round() as u64).max(1),
                "cents": cell.cents * share,
                "uncosted": ((cell.uncosted as f64 * share).round() as u64),
            }));
        }
    }
    let sum = |t: [u64; 4]| t.iter().sum::<u64>();
    json!({
        "rows": out,
        "summary": {
            "accountTokens": sum(total),
            "matchedTokens": sum(total) - sum(outside),
            "outsideTokens": sum(outside),
            "localUnmatchedTokens": sum(unmatched),
        },
    })
}
