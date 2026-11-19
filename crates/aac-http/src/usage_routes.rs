//! The history store's public reads at the service: `GET /api/usage/history`, `/api/usage/at`
//! and `/api/usage/burn` (`src/web-server/routes/store-routes.ts`). The guard and the HTTP
//! encoding stay the public front door's; this module owns the query contract, the store calls
//! and the exact JSON the route writes.
//!
//! The store methods are the binding's own (`seriesPoints`, `seriesList`, `burnLong`, `periods`,
//! `usageAt`), so the values are the ones the TypeScript route read from the same Engine.

use crate::store_host::StoreHost;
use aac_num::ecma;
use aac_views::decline::Decline;
use serde_json::{Map, Value as Json, json};

const HOUR: u64 = 3_600_000;
const DAY: u64 = 86_400_000;
const MAX_RANGE_MS: u64 = 3650 * DAY;
/// The burn route's spans, in the reference's own order (its `Object.keys(SPANS)`).
pub(crate) const SPANS: [(&str, u64); 6] = [
    ("1h", HOUR),
    ("6h", 6 * HOUR),
    ("24h", DAY),
    ("7d", 7 * DAY),
    ("30d", 30 * DAY),
    ("90d", 90 * DAY),
];
/// The route's own rule text, sent with every burn answer.
pub(crate) const RULE: &str = "Points used per hour across resets, from hourly rollups. A rate under 50% coverage is shown with its coverage only.";

/// One parsed query: the service rejects anything else before it reads the store.
struct Query(Vec<(String, String)>);

fn unescape(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                let hi = char::from(*bytes.get(i + 1)?).to_digit(16)? as u8;
                let lo = char::from(*bytes.get(i + 2)?).to_digit(16)? as u8;
                out.push(hi * 16 + lo);
                i += 3;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// `req.query` for these routes: plain string values, at most 200 characters, no repeated key
/// and no key outside `keys`. Anything else is the route's fixed 400.
fn parse(raw: Option<&str>, keys: &[&str]) -> Option<Query> {
    let mut out: Vec<(String, String)> = Vec::new();
    for part in raw.unwrap_or_default().split('&') {
        if part.is_empty() {
            continue;
        }
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = unescape(key)?;
        if !keys.contains(&key.as_str()) || out.iter().any(|(held, _)| *held == key) {
            return None;
        }
        let value = unescape(value)?;
        if value.encode_utf16().count() > 200 {
            return None;
        }
        out.push((key, value));
    }
    Some(Query(out))
}

impl Query {
    fn get(&self, key: &str) -> Option<&str> {
        self.0
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value.as_str())
    }
}

/// `^[a-zA-Z0-9][a-zA-Z0-9:@._+-]{0,159}$`
fn account(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() <= 160
        && bytes.first().is_some_and(|b| b.is_ascii_alphanumeric())
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || b"@:._+-".contains(b))
}

/// `^[A-Za-z0-9_.:-]{1,64}$`
fn window(text: &str) -> bool {
    (1..=64).contains(&text.len())
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
}

/// `^[a-z][a-z0-9-]{0,31}$`
fn provider(text: &str) -> bool {
    (1..=32).contains(&text.len())
        && text.bytes().next().is_some_and(|b| b.is_ascii_lowercase())
        && text
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

struct Filter {
    account_ids: Option<Vec<String>>,
    provider: Option<String>,
    window: Option<String>,
}

fn filter(query: &Query) -> Option<Filter> {
    let account_value = query.get("account").unwrap_or("all");
    let window_key = query.get("window");
    let provider_name = query.get("provider");
    if account_value != "all" && !account(account_value) {
        return None;
    }
    if window_key.is_some_and(|value| !window(value)) {
        return None;
    }
    if provider_name.is_some_and(|value| !provider(value)) {
        return None;
    }
    Some(Filter {
        account_ids: (account_value != "all").then(|| vec![account_value.to_owned()]),
        provider: provider_name.map(str::to_owned),
        window: window_key.map(str::to_owned),
    })
}

impl Filter {
    fn insert(&self, args: &mut Map<String, Json>) {
        if let Some(ids) = &self.account_ids {
            args.insert("accountIds".into(), json!(ids));
        }
        if let Some(provider) = &self.provider {
            args.insert("provider".into(), json!(provider));
        }
        if let Some(window) = &self.window {
            args.insert("windowKey".into(), json!(window));
        }
    }
}

fn instant(value: &str) -> Option<u64> {
    if !ecma::iso_prefixed(value) {
        return None;
    }
    let ms = ecma::parse_instant(value)?;
    u64::try_from(ms).ok()
}

fn iso(ms: u64) -> Result<String, Decline> {
    let ms = i64::try_from(ms).map_err(|_| Decline::Internal)?;
    ecma::to_iso_string(ms).ok_or(Decline::Internal)
}

async fn ask(host: &StoreHost, method: &str, args: Json) -> Result<Json, Decline> {
    let text = host
        .call(method.to_owned(), args.to_string())
        .await
        .map_err(|_| Decline::Internal)?;
    serde_json::from_str(&text).map_err(|_| Decline::Internal)
}

struct History {
    from: u64,
    to: u64,
    resolution: &'static str,
    filter: Filter,
}

fn history_query(raw: Option<&str>, now: u64) -> Option<History> {
    let query = parse(
        raw,
        &["account", "window", "provider", "from", "to", "resolution"],
    )?;
    let filter = filter(&query)?;
    let to = match query.get("to") {
        Some(value) => instant(value)?,
        None => now,
    };
    let from = match query.get("from") {
        Some(value) => instant(value)?,
        None => to.saturating_sub(7 * DAY),
    };
    let resolution = match query.get("resolution").unwrap_or("hour") {
        "raw" => "raw",
        "hour" => "hour",
        "day" => "day",
        _ => return None,
    };
    (from <= to && to - from <= MAX_RANGE_MS).then_some(History {
        from,
        to,
        resolution,
        filter,
    })
}

/// The front door's own 400 guard: the same query contract the service re-checks.
pub(crate) fn valid_history(raw: Option<&str>, now: u64) -> bool {
    history_query(raw, now).is_some()
}

/// `GET /api/usage/history`.
pub(crate) async fn history(
    host: &StoreHost,
    raw: Option<&str>,
    now: u64,
) -> Result<Json, Decline> {
    let History {
        from,
        to,
        resolution,
        filter,
    } = history_query(raw, now).ok_or(Decline::Shape)?;
    let mut args = Map::new();
    args.insert("fromMs".into(), json!(from));
    args.insert("toMs".into(), json!(to));
    args.insert("resolution".into(), json!(resolution));
    filter.insert(&mut args);
    let points = ask(host, "seriesPoints", Json::Object(args)).await?;
    let mut answer = Map::new();
    answer.insert("from".into(), json!(iso(from)?));
    answer.insert("to".into(), json!(iso(to)?));
    for key in ["resolution", "columns", "series", "truncated"] {
        answer.insert(key.into(), points.get(key).cloned().unwrap_or(Json::Null));
    }
    Ok(Json::Object(answer))
}

fn at_query(raw: Option<&str>, now: u64) -> Option<u64> {
    let query = parse(raw, &["t"])?;
    match query.get("t") {
        Some(value) => instant(value),
        None => Some(now),
    }
}

/// The front door's own 400 guard.
pub(crate) fn valid_at(raw: Option<&str>) -> bool {
    at_query(raw, u64::MAX / 2).is_some()
}

/// `GET /api/usage/at`.
pub(crate) async fn at(host: &StoreHost, raw: Option<&str>, now: u64) -> Result<Json, Decline> {
    let t = at_query(raw, now).ok_or(Decline::Shape)?;
    ask(host, "usageAt", json!({ "t": t })).await
}

fn string(series: &Json, field: &str) -> Json {
    series.get(field).cloned().unwrap_or(Json::Null)
}

struct Burn {
    filter: Filter,
    names: Vec<String>,
    spans: Vec<u64>,
}

fn burn_query(raw: Option<&str>) -> Option<Burn> {
    let query = parse(raw, &["account", "window", "provider", "span"])?;
    let filter = filter(&query)?;
    let names: Vec<String> = match query.get("span") {
        Some(value) => value.split(',').map(str::to_owned).collect(),
        None => SPANS.iter().map(|(name, _)| (*name).to_owned()).collect(),
    };
    let mut spans: Vec<u64> = Vec::with_capacity(names.len());
    for name in &names {
        let Some((_, span)) = SPANS.iter().find(|(held, _)| *held == name.as_str()) else {
            return None;
        };
        spans.push(*span);
    }
    Some(Burn {
        filter,
        names,
        spans,
    })
}

/// The front door's own 400 guard.
pub(crate) fn valid_burn(raw: Option<&str>) -> bool {
    burn_query(raw).is_some()
}

/// `GET /api/usage/burn`: the series seen in the longest span (from metadata, never a capped
/// point query), then the consumed/rate/coverage of every requested span and its reset periods.
pub(crate) async fn burn(host: &StoreHost, raw: Option<&str>, now: u64) -> Result<Json, Decline> {
    let Burn {
        filter,
        names,
        spans,
    } = burn_query(raw).ok_or(Decline::Shape)?;
    let longest = spans.iter().copied().max().unwrap_or(0);
    let mut list_args = Map::new();
    list_args.insert("fromMs".into(), json!(now.saturating_sub(longest)));
    list_args.insert("toMs".into(), json!(now));
    filter.insert(&mut list_args);
    let series = ask(host, "seriesList", Json::Object(list_args)).await?;
    let series = series.as_array().cloned().ok_or(Decline::Internal)?;
    let refs: Vec<Json> = series
        .iter()
        .map(|row| {
            json!({
                "identity": string(row, "identity"),
                "window": row.get("window").and_then(|w| w.get("identity")).cloned().unwrap_or(Json::Null),
            })
        })
        .collect();
    let burn = ask(
        host,
        "burnLong",
        json!({ "series": refs, "spansMs": spans, "nowMs": now }),
    )
    .await?;
    let periods = ask(
        host,
        "periods",
        json!({ "series": refs, "fromMs": now.saturating_sub(longest), "toMs": now }),
    )
    .await?;
    let (burn, periods) = (
        burn.as_array().cloned().ok_or(Decline::Internal)?,
        periods.as_array().cloned().ok_or(Decline::Internal)?,
    );
    if burn.len() < series.len() || periods.len() < series.len() {
        return Err(Decline::Internal);
    }
    let mut rows = Vec::with_capacity(series.len());
    for (index, row) in series.iter().enumerate() {
        let window = row.get("window").cloned().unwrap_or(Json::Null);
        let span_rows: Vec<Json> = burn[index]
            .get("spans")
            .and_then(Json::as_array)
            .map(|rows| {
                rows.iter()
                    .enumerate()
                    .map(|(position, span)| {
                        let coverage = span.get("coverage").cloned().unwrap_or(Json::Null);
                        let rate = coverage
                            .as_f64()
                            .is_some_and(|value| value >= 0.5)
                            .then(|| span.get("rate").cloned().unwrap_or(Json::Null));
                        json!({
                            "span": names.get(position).map(String::as_str).unwrap_or(""),
                            "consumed": span.get("consumed").cloned().unwrap_or(Json::Null),
                            "rate": rate.unwrap_or(Json::Null),
                            "coverage": coverage,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        rows.push(json!({
            "accountId": string(row, "accountId"),
            "provider": string(row, "provider"),
            "plan": string(row, "plan"),
            "window": {
                "key": window.get("key").cloned().unwrap_or(Json::Null),
                "label": window.get("label").cloned().unwrap_or(Json::Null),
                "pool": window.get("poolLabel").cloned().unwrap_or(Json::Null),
            },
            "spans": span_rows,
            "periods": periods[index].get("periods").cloned().unwrap_or_else(|| json!([])),
        }));
    }
    Ok(json!({ "at": iso(now)?, "rule": RULE, "series": rows }))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn the_query_is_plain_keys_and_strings() {
        let keys = ["account", "window", "provider", "from", "to", "resolution"];
        for raw in [
            "account=codex%3Afirst&window=five_hour&from=2026-11-01T00%3A00%3A00Z",
            "",
            "resolution=raw&to=2026-11-06T12%3A00%3A00Z",
        ] {
            let query = parse(Some(raw), &keys).unwrap_or_else(|| panic!("{raw}"));
            assert!(filter(&query).is_some(), "{raw}");
        }
        assert!(parse(Some("bogus=1"), &keys).is_none());
        assert!(parse(Some("account=x&account=y"), &keys).is_none());
        let bad_filter =
            |raw: &str| parse(Some(raw), &keys).is_some_and(|query| filter(&query).is_none());
        assert!(bad_filter("account=%3Cscript%3E"));
        assert!(bad_filter("window="));
        assert!(bad_filter("provider=Codex"));
    }

    #[test]
    fn the_route_guards_match_the_query_contract() {
        assert!(valid_history(None, 1_767_225_600_000));
        assert!(valid_history(Some("resolution=raw"), 1_767_225_600_000));
        for bad in [
            "resolution=minute",
            "from=yesterday",
            "account=%3Cscript%3E",
            "bogus=1",
            "from=2026-11-06T00:00:00Z&to=2026-11-01T00:00:00Z",
        ] {
            assert!(!valid_history(Some(bad), 1_767_225_600_000), "{bad}");
        }
        assert!(valid_history(
            Some("from=2026-11-01T00:00:00Z"),
            1_767_225_600_000
        ));
        assert!(valid_at(None));
        assert!(valid_at(Some("t=2026-11-01T00:00:00Z")));
        assert!(!valid_at(Some("t=nope")));
        assert!(!valid_at(Some("bogus=1")));
        assert!(valid_burn(None));
        assert!(valid_burn(Some("span=7d,1h")));
        assert!(!valid_burn(Some("span=2y")));
    }

    #[test]
    fn the_filter_words_are_the_reference_patterns() {
        assert!(account("codex:first"));
        assert!(!account("-lead"));
        assert!(!account(&"a".repeat(161)));
        assert!(window("five_hour"));
        assert!(!window(""));
        assert!(provider("codex"));
        assert!(!provider("Codex"));
        assert!(!provider(&"a".repeat(33)));
    }

    #[test]
    fn spans_are_the_six_the_route_knows() {
        assert_eq!(
            SPANS.iter().map(|(name, _)| *name).collect::<Vec<_>>(),
            ["1h", "6h", "24h", "7d", "30d", "90d"]
        );
    }
}
