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

/// Write the hourly and daily rollup files for one sample, streaming each file's JSON text
/// straight to `write` (the share writer). R2: the base built two full `serde_json::Value`
/// trees per sample (`complete` + `to_file`) and the publisher discarded them 59/60 times;
/// this is called only when the hourly write is due and holds one series' points at a time,
/// never the whole 90-day tree. The bytes are `JSON.stringify` of the same file value the
/// base produced (the golden test pins them). The hourly file is written before the daily
/// is built, so a daily failure leaves the hourly landed, exactly as the base publisher did.
pub fn write_rollup_exports(
    engine: &Engine,
    rows: &[Value],
    projected: &[ProjectedAccount],
    now: i64,
    mut write: impl FnMut(&str, &str) -> Result<(), ()>,
) -> Result<(), ExportError> {
    let hourly = complete_text(engine, "hour", HOURLY_DAYS, rows, projected, now)?;
    write("hour", &hourly).map_err(|_| ExportError)?;
    let daily = complete_text(engine, "day", DAILY_DAYS, rows, projected, now)?;
    write("day", &daily).map_err(|_| ExportError)?;
    Ok(())
}

/// One rollup file's `JSON.stringify` text, streamed series by series. Each series' points
/// are read page by page and written immediately, so only one page of points is held at a
/// time; the file's envelope and each series' metadata are written around them.
fn complete_text(
    engine: &Engine,
    resolution: &str,
    days: i64,
    rows: &[Value],
    projected: &[ProjectedAccount],
    now: i64,
) -> Result<String, ExportError> {
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
    let by_identity = identity_map(rows, projected);
    let mut out = aac_views::js_json::Writer::plain(4096);
    // The file envelope, written as raw `JSON.stringify` bytes (the keys are fixed words and
    // the columns a fixed array, so no object-member ordering applies). The series array is
    // streamed below; only the closing bracket follows the loop.
    out.raw(b"{\"version\":1,\"generatedAt\":");
    out.string(&crate::js::iso_of(now).unwrap_or_default());
    out.raw(b",\"resolution\":");
    out.string(resolution);
    out.raw(b",\"columns\":[\"start\",\"consumed\",\"lastUsed\",\"maxUsed\",\"coveredMinutes\",\"readings\",\"resets\",\"creditsSpent\",\"creditsAdded\"],\"series\":[");
    let mut emitted = false;
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
        // Pages contain at most 1,000 buckets for the selected account/window. The first
        // non-empty page opens the series; later pages append. A series with no points is
        // absent, exactly as the base's `if !points.is_empty()` guard.
        let mut opened = false;
        let mut from = from_ms;
        while from <= now {
            let to = now.min(from + 1000 * step - 1);
            let mut points: Vec<Value> = Vec::new();
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
            if !points.is_empty() {
                if !opened {
                    if emitted {
                        out.raw(b",");
                    }
                    emitted = true;
                    opened = true;
                    write_series_head(&mut out, meta, by_identity.get(&identity).copied());
                } else {
                    out.raw(b",");
                }
                write_points(&mut out, &points);
            }
            from += 1000 * step;
        }
        if opened {
            out.raw(b"]}");
        }
    }
    out.raw(b"]}");
    Ok(String::from_utf8(out.into_bytes()).unwrap_or_default())
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
            if member(member(item, "window"), "identity").as_str() == Some(window_identity)
                && let Some(item_points) = member(item, "points").as_array()
            {
                points.extend(item_points.iter().cloned());
            }
        }
    }
    Ok(())
}

/// The projected account per history identity, exactly as the base `to_file` built it: a row
/// joins the projection by its dashboard id, keyed by the row's analytics identity.
fn identity_map<'a>(
    rows: &[Value],
    projected: &'a [ProjectedAccount],
) -> HashMap<String, &'a ProjectedAccount> {
    let mut by_identity: HashMap<String, &ProjectedAccount> = HashMap::new();
    for row in rows {
        let id = member(row, "id").as_str().unwrap_or("");
        if let Some(account) = projected.iter().find(|candidate| candidate.id == id) {
            by_identity.insert(account_identity(row), account);
        }
    }
    by_identity
}

/// One series' fixed members and the opening of its `points` array. The point values follow
/// through [`write_points`]; the caller closes the array and the object. Field order and
/// values are the base `to_file`'s, so the bytes are identical.
fn write_series_head(
    w: &mut aac_views::js_json::Writer,
    meta: &Value,
    account: Option<&ProjectedAccount>,
) {
    let window = member(meta, "window");
    let mut series = w.object();
    series.member("accountId", |w| w.value(member(meta, "accountId")));
    series.member("provider", |w| w.value(member(meta, "provider")));
    series.member("label", |w| {
        w.value(&account.map(|a| json!(a.label)).unwrap_or(Value::Null));
    });
    series.member("email", |w| {
        w.value(
            &account
                .and_then(|a| a.email.as_ref().map(|e| json!(e)))
                .unwrap_or(Value::Null),
        );
    });
    series.member("profile", |w| {
        w.value(
            &account
                .and_then(|a| a.profile.as_ref().map(|p| json!(p)))
                .unwrap_or(Value::Null),
        );
    });
    series.member("hidden", |w| {
        w.value(&json!(account.map(|a| a.hidden).unwrap_or(false)));
    });
    series.member("window", |w| w.value(member(window, "key")));
    series.member("windowLabel", |w| w.value(member(window, "label")));
    series.member("pool", |w| w.value(member(window, "poolLabel")));
    series.member("kind", |w| w.value(member(window, "kind")));
    series.member("unit", |w| w.value(member(window, "unit")));
    series.member("points", |w| w.raw(b"["));
}

/// A page of points, each written as `[isoStart, ...rest]` — the base `to_file`'s point
/// transform (the first element becomes its ISO string; the rest are copied verbatim).
fn write_points(w: &mut aac_views::js_json::Writer, points: &[Value]) {
    for (index, point) in points.iter().enumerate() {
        if index > 0 {
            w.raw(b",");
        }
        w.raw(b"[");
        let parts = point.as_array();
        let start = parts.and_then(|p| p.first()).and_then(Value::as_f64);
        w.string(
            &start
                .and_then(|ms| crate::js::iso_of(ms as i64))
                .unwrap_or_default(),
        );
        if let Some(parts) = parts {
            for part in parts.iter().skip(1) {
                w.raw(b",");
                w.value(part);
            }
        }
        w.raw(b"]");
    }
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
