//! The store's share exports (`src/store/usage-exports.ts`) and the sample's long burn
//! (`longBurnFor` in `account-analytics-service.ts`), computed from the hosted store engine:
//! `history-hourly.json` (90 days of hourly rollups) and `history-daily.json` (every day kept),
//! written once an hour beside `usage.json`. Measured numbers only: a missing hour is absent,
//! never zero. Without a hosted store the sample payload carries these facts instead.

use crate::js::{account_identity, member, window_identity};
use crate::projection::ProjectedAccount;
use crate::snapshot::{LongBurnEntry, LongBurnSpan};
use aac_store::engine::Engine;
use aac_store::usage::{SeriesListQuery, SeriesQuery, SeriesRef};
use serde_json::{Value, json};
use std::collections::HashMap;

const HOUR_MS: i64 = 3_600_000;
const DAY_MS: i64 = 86_400_000;
const HOURLY_DAYS: i64 = 90;
const DAILY_DAYS: i64 = 3650;

/// `exportHour`: the hour a file set belongs to; exports are rewritten once per hour.
pub fn export_hour(now_ms: i64) -> i64 {
    now_ms.div_euclid(HOUR_MS)
}

/// An export failure: the publisher retries on the next sample.
#[derive(Debug)]
pub struct ExportError;

/// `buildRollupExports`: the hourly and daily files for one sample.
pub fn build_rollup_exports(
    engine: &Engine,
    rows: &[Value],
    projected: &[ProjectedAccount],
    now: i64,
) -> Result<(Value, Value), ExportError> {
    let hourly = complete(engine, "hour", HOURLY_DAYS, rows, projected, now)?;
    let daily = complete(engine, "day", DAILY_DAYS, rows, projected, now)?;
    Ok((hourly, daily))
}

fn complete(
    engine: &Engine,
    resolution: &str,
    days: i64,
    rows: &[Value],
    projected: &[ProjectedAccount],
    now: i64,
) -> Result<Value, ExportError> {
    let step = if resolution == "hour" {
        HOUR_MS
    } else {
        DAY_MS
    };
    let from_ms = ((now - days * DAY_MS) as f64 / step as f64).floor() as i64 * step;
    let now_u = now.max(0) as u64;
    let list = aac_store::usage::series_list(
        engine,
        &SeriesListQuery {
            identities: None,
            account_ids: None,
            provider: None,
            window_key: None,
            from_ms: from_ms.max(0) as u64,
            to_ms: now_u,
        },
    )
    .map_err(|_| ExportError)?;
    let metas = list.as_array().ok_or(ExportError)?;
    let mut series_out: Vec<Value> = Vec::new();
    for meta in metas {
        let identity = member(meta, "identity")
            .as_str()
            .ok_or(ExportError)?
            .to_owned();
        let window_key = member(member(meta, "window"), "key")
            .as_str()
            .ok_or(ExportError)?
            .to_owned();
        let window_identity = member(member(meta, "window"), "identity")
            .as_str()
            .ok_or(ExportError)?
            .to_owned();
        let mut points: Vec<Value> = Vec::new();
        // Pages contain at most 1,000 buckets for the selected account/window.
        let mut from = from_ms;
        while from <= now {
            let to = now.min(from + 1000 * step - 1);
            page(
                engine,
                resolution,
                &identity,
                &window_key,
                &window_identity,
                from,
                to,
                step,
                &mut points,
            )?;
            from += 1000 * step;
        }
        if !points.is_empty() {
            let mut entry = meta.clone();
            entry["points"] = Value::Array(points);
            series_out.push(entry);
        }
    }
    Ok(to_file(resolution, &series_out, rows, projected, now))
}

#[allow(clippy::too_many_arguments)]
fn page(
    engine: &Engine,
    resolution: &str,
    identity: &str,
    window_key: &str,
    window_identity: &str,
    from: i64,
    to: i64,
    step: i64,
    points: &mut Vec<Value>,
) -> Result<(), ExportError> {
    let result = aac_store::usage::series_points(
        engine,
        &SeriesQuery {
            identities: Some(vec![identity.to_owned()]),
            account_ids: None,
            provider: None,
            window_key: Some(window_key.to_owned()),
            from_ms: from.max(0) as u64,
            to_ms: to.max(0) as u64,
            resolution: resolution.to_owned(),
            limit: None,
        },
    )
    .map_err(|_| ExportError)?;
    if member(&result, "truncated") == &json!(true) {
        let buckets = (to as f64 / step as f64).floor() as i64
            - (from as f64 / step as f64).floor() as i64
            + 1;
        if buckets <= 1 {
            // A single bucket exceeds the query cap: the export is incomplete.
            return Err(ExportError);
        }
        let middle = ((from as f64 / step as f64).floor() as i64 + buckets / 2) * step;
        page(
            engine,
            resolution,
            identity,
            window_key,
            window_identity,
            from,
            middle - 1,
            step,
            points,
        )?;
        page(
            engine,
            resolution,
            identity,
            window_key,
            window_identity,
            middle,
            to,
            step,
            points,
        )?;
        return Ok(());
    }
    if let Some(items) = member(&result, "series").as_array() {
        for item in items {
            if member(member(item, "window"), "identity").as_str() == Some(window_identity) {
                if let Some(item_points) = member(item, "points").as_array() {
                    points.extend(item_points.iter().cloned());
                }
            }
        }
    }
    Ok(())
}

fn to_file(
    resolution: &str,
    series: &[Value],
    rows: &[Value],
    projected: &[ProjectedAccount],
    now: i64,
) -> Value {
    let mut by_identity: HashMap<String, &ProjectedAccount> = HashMap::new();
    for row in rows {
        let id = member(row, "id").as_str().unwrap_or("");
        if let Some(account) = projected.iter().find(|candidate| candidate.id == id) {
            by_identity.insert(account_identity(row), account);
        }
    }
    let series_out: Vec<Value> = series
        .iter()
        .map(|entry| {
            let account = by_identity.get(member(entry, "identity").as_str().unwrap_or(""));
            let window = member(entry, "window");
            let points: Vec<Value> = member(entry, "points")
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .map(|point| {
                            let mut out = Vec::new();
                            let start = point.as_array().and_then(|parts| parts.first()).and_then(Value::as_f64);
                            out.push(json!(start.and_then(|ms| crate::js::iso_of(ms as i64)).unwrap_or_default()));
                            if let Some(parts) = point.as_array() {
                                out.extend(parts.iter().skip(1).cloned());
                            }
                            Value::Array(out)
                        })
                        .collect()
                })
                .unwrap_or_default();
            json!({
                "accountId": member(entry, "accountId"),
                "provider": member(entry, "provider"),
                "label": account.map(|a| json!(a.label)).unwrap_or(Value::Null),
                "email": account.and_then(|a| a.email.as_ref().map(|e| json!(e))).unwrap_or(Value::Null),
                "profile": account.and_then(|a| a.profile.as_ref().map(|p| json!(p))).unwrap_or(Value::Null),
                "hidden": account.map(|a| a.hidden).unwrap_or(false),
                "window": member(window, "key"),
                "windowLabel": member(window, "label"),
                "pool": member(window, "poolLabel"),
                "kind": member(window, "kind"),
                "unit": member(window, "unit"),
                "points": points,
            })
        })
        .collect();
    json!({
        "version": 1,
        "generatedAt": crate::js::iso_of(now).unwrap_or_default(),
        "resolution": resolution,
        "columns": ["start", "consumed", "lastUsed", "maxUsed", "coveredMinutes", "readings", "resets", "creditsSpent", "creditsAdded"],
        "series": series_out,
    })
}

/// `longBurnFor`: 7- and 30-day burn for every live percent window, from the store's hourly
/// rollups, keyed by `longBurnKey`.
pub fn long_burn_for(
    engine: &Engine,
    accounts: &[Value],
    now: i64,
) -> Result<HashMap<String, LongBurnEntry>, ExportError> {
    let mut refs: Vec<(String, String, String)> = Vec::new();
    for account in accounts {
        let Some(windows) = member(account, "windows").as_array() else {
            continue;
        };
        for window in windows {
            let kind = member(window, "kind");
            let kind = if kind.is_null() {
                "rate_limit"
            } else {
                kind.as_str().unwrap_or("")
            };
            if member(window, "usedPercent").as_f64().is_some() && kind == "rate_limit" {
                refs.push((
                    account_identity(account),
                    window_identity(window),
                    crate::js::long_burn_key(account, window),
                ));
            }
        }
    }
    let mut result = HashMap::new();
    if refs.is_empty() {
        return Ok(result);
    }
    let series: Vec<SeriesRef> = refs
        .iter()
        .map(|(identity, window, _)| SeriesRef {
            identity: identity.clone(),
            window: window.clone(),
        })
        .collect();
    let answer = aac_store::usage::burn_long(
        engine,
        &series,
        &[7 * DAY_MS as u64, 30 * DAY_MS as u64],
        now.max(0) as u64,
    )
    .map_err(|_| ExportError)?;
    if let Some(entries) = answer.as_array() {
        for (index, entry) in entries.iter().enumerate() {
            let Some(spans) = member(entry, "spans").as_array() else {
                continue;
            };
            let (Some(last7), Some(last30)) = (spans.first(), spans.get(1)) else {
                continue;
            };
            let Some(key) = refs.get(index).map(|(_, _, key)| key.clone()) else {
                continue;
            };
            result.insert(
                key,
                LongBurnEntry {
                    last7_days: span(last7),
                    last30_days: span(last30),
                },
            );
        }
    }
    Ok(result)
}

fn span(value: &Value) -> LongBurnSpan {
    LongBurnSpan {
        span_ms: member(value, "spanMs").as_f64().unwrap_or(0.0) as i64,
        consumed: member(value, "consumed").as_f64(),
        rate: member(value, "rate").as_f64(),
        coverage: member(value, "coverage").as_f64().unwrap_or(0.0),
    }
}
