//! Owned Analytics (DESIGN-M3-COMPLETE 1.5, R5 + A2): Rust reads the quota history and the
//! activity range from the hosted store itself and composes the whole `/api/accounts/analytics`
//! answer, activity block included. The front door supplies only what stays in TS until H2: the
//! dashboard accounts, the live collector state, the resolved pricing facts and the proven zone
//! offsets, plus its own bounds and provisional range, which this module re-reads and re-resolves
//! and refuses on any disagreement (`stale`). Every unproven value is a named [`Decline`]; the TS
//! computation then answers exactly as before.
//!
//! The quota composition is the proven part-3 port ([`crate::analytics`]); the activity block is
//! the store-fed port ([`crate::analytics_activity`]). The engine facts the composition reads
//! (`Date.parse` epochs, `toISOString` words, provider-id collation ranks) are built here from the
//! store's own ISO strings and the CLDR root order, the same answers the TS envelope carries.
use crate::analytics::{self, Range};
use crate::analytics_activity::{self as activity, LiveState, Pricing};
use crate::decline::Decline;
use aac_num::{ecma, intl};
use aac_status::read_context::{Context, Envelope, Presentation};
use serde_json::{Map, Value, json};

type R<T> = Result<T, Decline>;
/// An ordered JSON object; insertion order is the answer's own key order.
pub type Object = Map<String, Value>;

const DAY_MS: f64 = 86_400_000.0;
const RETAINED_MS: f64 = 31.0 * DAY_MS;
const STORE_RETENTION_MS: f64 = 90.0 * DAY_MS;

/// The hosted store reads the owned answer needs. `aac-http` implements it over its one Engine;
/// the spec tests implement it over fixture answers. Read failures of the two bounds are `None`
/// candidates, as the TS reader's per-call catches are; a failed history or activity read declines.
pub trait AnalyticsStore {
    /// The oldest stored activity hour (ms), or none.
    fn activity_oldest(&mut self) -> R<Option<u64>>;
    /// The oldest stored quota sample (ms), or none.
    fn quota_oldest(&mut self) -> R<Option<u64>>;
    /// The bucketed activity rows and sessions of a range (`activityRange`).
    fn activity_range(&mut self, query: Value) -> R<Value>;
    /// The per-computer and per-source coverage of a range (`activitySources`).
    fn activity_sources(&mut self, from_ms: u64, to_ms: u64) -> R<Value>;
    /// The thinned quota observations of a range (`historyRecords`).
    fn history_records(&mut self, query: Value) -> R<Value>;
}

fn obj(value: Option<&Value>) -> R<&Object> {
    value.and_then(Value::as_object).ok_or(Decline::Shape)
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
fn ms_u64(value: f64) -> R<u64> {
    let ms = time(value)?;
    if ms < 0 {
        return Err(Decline::Date);
    }
    Ok(ms as u64)
}

/// `resolveAccountAnalyticsRange` for every preset, from the proven zone offsets. A custom range
/// outside retention declines here; the TS computation raises the same query error itself.
fn resolve_range(
    query: &Object,
    now: f64,
    available_from: f64,
    bounds: Option<f64>,
    tz: &str,
    zone: Option<&intl::Zone>,
) -> R<Range> {
    let preset = text(query.get("range"))?.to_owned();
    let retained_from = match bounds {
        Some(oldest) => analytics::js_min(now, oldest),
        None => now - RETAINED_MS,
    };
    let mut to = now;
    let from = match preset.as_str() {
        "24h" => now - DAY_MS,
        "7d" => now - 7.0 * DAY_MS,
        "30d" => now - 30.0 * DAY_MS,
        "90d" => {
            let from = now - 90.0 * DAY_MS;
            if bounds.is_none() {
                analytics::js_max(from, retained_from)
            } else {
                from
            }
        }
        "month" => {
            let month_first = format!("{}-01", &activity::local_date_text(now, tz, zone)?[..7]);
            activity::local_midnight(&month_first, tz, zone)?
        }
        "all" => analytics::js_min(now, analytics::js_max(retained_from, available_from)),
        "custom" => {
            let first = text(query.get("from"))?.to_owned();
            let last = text(query.get("to"))?.to_owned();
            if !is_calendar_date(&first) || !is_calendar_date(&last) || first > last {
                return Err(Decline::Unsupported);
            }
            let days = activity::day_number(&last)? - activity::day_number(&first)? + 1;
            if days > 36_525 {
                return Err(Decline::Unsupported);
            }
            if bounds.is_none() && days > 31 {
                return Err(Decline::Unsupported);
            }
            if last > activity::local_date_text(now, tz, zone)?
                || first < activity::local_date_text(retained_from, tz, zone)?
            {
                return Err(Decline::Unsupported);
            }
            let next = activity::day_number(&last)? + 1;
            let next_text =
                ecma::to_iso_string(next * 86_400_000).ok_or(Decline::Date)?[..10].to_owned();
            to = analytics::js_min(
                now,
                activity::local_midnight(&next_text, tz, zone)? - 1000.0,
            );
            let from =
                analytics::js_max(activity::local_midnight(&first, tz, zone)?, retained_from);
            if to < from {
                return Err(Decline::Unsupported);
            }
            from
        }
        _ => return Err(Decline::Unsupported),
    };
    // The shape checks the route already ran; a from/to on a non-custom preset never reaches here.
    if preset != "custom" && (query.contains_key("from") || query.contains_key("to")) {
        return Err(Decline::Unsupported);
    }
    #[allow(clippy::cast_precision_loss)]
    let minutes = analytics::bucket_minutes(to - from) as f64;
    Ok(Range {
        preset,
        from,
        to,
        tz: tz.to_owned(),
        bucket_minutes: minutes,
    })
}

/// `isCalendarDate`: a real `YYYY-MM-DD` day, 1970 or later.
fn is_calendar_date(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || !bytes[..4].iter().all(u8::is_ascii_digit)
        || !bytes[5..7].iter().all(u8::is_ascii_digit)
        || !bytes[8..10].iter().all(u8::is_ascii_digit)
    {
        return false;
    }
    let Ok(year) = value[..4].parse::<i64>() else {
        return false;
    };
    let Ok(month) = value[5..7].parse::<i64>() else {
        return false;
    };
    let Ok(day) = value[8..10].parse::<i64>() else {
        return false;
    };
    if year < 1970 || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return false;
    }
    let days = aac_num::civil::days_from_civil(year, month, day);
    let (y, m, d) = aac_num::civil::civil_from_days(days);
    y == year && m == month && d == day
}

/// The presentation facts of the owned read: the ISO words and epochs of the store's own strings,
/// and the CLDR root ranks of the provider table's ids.
fn presentation(records: &[Value], table: &[Value], instants: &[f64]) -> R<Presentation> {
    let mut epochs: Map<String, Value> = Map::new();
    let mut add_epoch = |text: &str| -> R<()> {
        if text.is_empty() {
            return Ok(());
        }
        if !ecma::iso_prefixed(text) {
            return Err(Decline::Date);
        }
        match ecma::parse_instant(text) {
            Some(ms) => {
                epochs.insert(text.to_owned(), json!(ms));
                Ok(())
            }
            None => Err(Decline::Date),
        }
    };
    for record in records {
        let record = obj(Some(record))?;
        add_epoch(text(record.get("sampledAt"))?)?;
        for window in record
            .get("windows")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            if let Some(sampled) = window.get("sampledAt").and_then(Value::as_str) {
                add_epoch(sampled)?;
            }
        }
    }
    let mut dates: Map<String, Value> = Map::new();
    for instant in instants {
        let ms = time(*instant)?;
        dates.insert(ms.to_string(), json!({ "iso": iso(*instant)? }));
    }
    let mut ids: Vec<&str> = Vec::new();
    for row in table {
        ids.push(text(obj(Some(row))?.get("id"))?);
    }
    let mut sorted = ids.clone();
    let mut err: R<()> = Ok(());
    sorted.sort_by(|a, b| match intl::ascii_compare(a, b) {
        Some(order) => order,
        None => {
            err = Err(Decline::Collation);
            std::cmp::Ordering::Equal
        }
    });
    err?;
    let mut order: Map<String, Value> = Map::new();
    let mut rank = 0usize;
    for (index, word) in sorted.iter().enumerate() {
        if index > 0
            && intl::ascii_compare(sorted[index - 1], word) != Some(std::cmp::Ordering::Equal)
        {
            rank += 1;
        }
        order.insert((*word).to_owned(), json!(rank));
    }
    serde_json::from_value(json!({ "dates": dates, "epochs": epochs, "order": order }))
        .map_err(|_| Decline::Shape)
}

/// The store's plain window objects as the entry lists the composition port spreads. JSON carries
/// no undefined members, so the entries reproduce each object exactly.
#[allow(dead_code)]
fn record_entries(records: &Value) -> R<Vec<Value>> {
    let mut out = Vec::new();
    for record in arr(records)? {
        let record = obj(Some(record))?;
        let mut copy = record.clone();
        let windows = record
            .get("windows")
            .and_then(Value::as_array)
            .ok_or(Decline::Shape)?;
        let mut entries: Vec<Value> = Vec::new();
        for window in windows {
            let window = obj(Some(window))?;
            let pairs: Vec<Value> = window
                .iter()
                .map(|(key, value)| json!([key, value]))
                .collect();
            entries.push(Value::Array(pairs));
        }
        copy.insert("windows".into(), Value::Array(entries));
        out.push(Value::Object(copy));
    }
    Ok(out)
}

fn arr(value: &Value) -> R<&Vec<Value>> {
    value.as_array().ok_or(Decline::Shape)
}

/// The owned answer: the whole `/api/accounts/analytics` body, activity included, or a decline.
pub fn compose(envelope: Envelope, store: &mut dyn AnalyticsStore) -> R<Value> {
    let input = obj(Some(&envelope.data))?.clone();
    let query = obj(input.get("query"))?.clone();
    let now = num(input.get("now"))?;
    let tz = text(input.get("tz"))?.to_owned();
    let available = input
        .get("available")
        .and_then(Value::as_bool)
        .ok_or(Decline::Shape)?;
    let live_value = obj(input.get("live"))?;
    let live = LiveState {
        tz: tz.clone(),
        sources: live_value.get("sources").cloned().ok_or(Decline::Shape)?,
        refreshing: live_value
            .get("refreshing")
            .and_then(Value::as_bool)
            .ok_or(Decline::Shape)?,
        refreshing_remote: live_value
            .get("refreshingRemote")
            .cloned()
            .ok_or(Decline::Shape)?,
    };
    let zone = match input.get("zone") {
        None | Some(Value::Null) => None,
        Some(value) => {
            let zone = obj(Some(value))?;
            let from = time(num(zone.get("from"))?)?;
            let to = time(num(zone.get("to"))?)?;
            let rows = arr(zone.get("offsets").ok_or(Decline::Shape)?)?;
            let mut segments: Vec<[i64; 2]> = Vec::new();
            for row in rows {
                let row = arr(row)?;
                segments.push([time(num(row.first())?)?, time(num(row.get(1))?)?]);
            }
            Some(intl::Zone::proven(from, to, &segments).ok_or(Decline::Zone)?)
        }
    };
    if tz != "UTC" && zone.is_none() {
        return Err(Decline::Zone);
    }
    let pricing = Pricing::decode(input.get("pricing"))?;
    let table = arr(input.get("providerTable").ok_or(Decline::Shape)?)?.clone();
    let dashboard = obj(input.get("dashboard"))?.clone();

    // The bounds the TS reader read, re-read here: the oldest stored activity hour or quota
    // sample, never later than the retained window. A failed read is a null candidate, as there.
    let (oldest, quota_oldest) = bounds(store, now);
    // Numeric comparison: the envelope's JSON integer and this float are the same time.
    let sent = opt_num(obj(input.get("bounds"))?.get("oldest"))?;
    if sent != Some(oldest) {
        return Err(Decline::Stale);
    }
    let retained_from = oldest;

    // The provisional range, re-resolved: any disagreement with the TS read is stale.
    let provisional = resolve_range(&query, now, retained_from, Some(oldest), &tz, zone.as_ref())?;
    let expected = Range::decode(input.get("provisional")).map_err(|_| Decline::Stale)?;
    if provisional != expected {
        return Err(Decline::Stale);
    }

    answer(
        Owned {
            query,
            now,
            tz,
            available,
            live,
            zone,
            pricing,
            table,
            dashboard,
            oldest,
            quota_oldest,
            guard_coverage: input.get("guardCoverage").cloned(),
            clock: None,
            provisional,
        },
        store,
    )
}

/// What the live service knows before the store reads: every value the TS envelope carried, built
/// here instead of forwarded (R5/A2 live hookup). `None` declines exactly like the envelope path.
pub struct NativeInput {
    /// The query object (validated by the route: platform, range, provider, account, refresh, tz).
    pub query: Object,
    /// The request clock.
    pub now: f64,
    /// The display zone name.
    pub tz: String,
    /// The platform's dashboard (`accounts` entry lists, `providers`, `settings`).
    /// A null `accounts` value means the current inventory could not be established.
    pub dashboard: Object,
    /// The activity collector grid; empty lets the store's own reads fill it.
    pub live_sources: Value,
    /// A collection pass is running.
    pub refreshing: bool,
    /// `Resolver::facts_table` over the store dictionary, as a JSON object.
    pub pricing: Value,
    /// `analytics_provider_table`, as `{id,label,order}` rows.
    pub table: Vec<Value>,
}

/// The native service's owned answer. Use the same store bounds for the display zone
/// and provisional range, even if a backfill adds older data during this request.
pub fn compose_native(
    input: NativeInput,
    store: &mut dyn AnalyticsStore,
    zone_for: impl FnOnce(f64, &str) -> Option<intl::Zone>,
) -> R<Value> {
    let query = input.query;
    let now = input.now;
    let tz = input.tz;
    let (oldest, quota_oldest) = bounds(store, now);
    let zone = zone_for(oldest, &tz);
    if tz != "UTC" && zone.is_none() {
        return Err(Decline::Zone);
    }
    let live = LiveState {
        tz: tz.clone(),
        sources: input.live_sources,
        refreshing: input.refreshing,
        refreshing_remote: Value::Array(Vec::new()),
    };
    let pricing = Pricing::decode(Some(&input.pricing))?;
    let provisional = resolve_range(&query, now, oldest, Some(oldest), &tz, zone.as_ref())?;
    answer(
        Owned {
            query,
            now,
            tz,
            // The store answered its bounds and history; a failed history read declines below.
            available: true,
            live,
            zone,
            pricing,
            table: input.table,
            dashboard: input.dashboard,
            oldest,
            quota_oldest,
            guard_coverage: None,
            clock: Some(now),
            provisional,
        },
        store,
    )
}

/// The bounds the TS reader read: the oldest stored activity hour or quota sample, never later
/// than the retained window, and the quota oldest itself. A failed bound read is a null candidate,
/// as there.
fn bounds(store: &mut dyn AnalyticsStore, now: f64) -> (f64, Option<u64>) {
    let activity_oldest = store.activity_oldest().unwrap_or(None);
    let quota_oldest = store.quota_oldest().unwrap_or(None);
    let mut candidates = vec![now - RETAINED_MS];
    #[allow(clippy::cast_precision_loss)]
    for ms in activity_oldest.into_iter().chain(quota_oldest) {
        candidates.push(ms as f64);
    }
    let oldest = candidates
        .iter()
        .copied()
        .fold(f64::INFINITY, analytics::js_min);
    (oldest, quota_oldest)
}

/// The validated owned facts, before the activity and quota reads.
struct Owned {
    query: Object,
    now: f64,
    tz: String,
    available: bool,
    live: LiveState,
    zone: Option<intl::Zone>,
    pricing: Pricing,
    table: Vec<Value>,
    dashboard: Object,
    oldest: f64,
    /// The oldest stored quota sample (`store.quotaOldest`), for the coverage bound.
    quota_oldest: Option<u64>,
    /// The guarded selection's coverage the envelope carried; `None` reads it from the store.
    guard_coverage: Option<Value>,
    /// The request clock, for an activity answer whose store has no read yet (the native read
    /// is the only reader; the envelope path has none, as it cannot prove the clock equal).
    clock: Option<f64>,
    provisional: Range,
}

/// The one answer builder both entries share: the activity block, the range, the throttled history
/// and the part-3 composition of the whole analytics body.
fn answer(owned: Owned, store: &mut dyn AnalyticsStore) -> R<Value> {
    let Owned {
        query,
        now,
        tz,
        available,
        live,
        zone,
        pricing,
        table,
        dashboard,
        oldest,
        quota_oldest,
        guard_coverage,
        clock,
        provisional,
    } = owned;
    let mut read_activity = || {
        let from = ms_u64(provisional.from)?;
        let to = ms_u64(provisional.to)?;
        let daily = provisional.to - provisional.from > activity::HOURLY_RANGE_MS;
        let mut range_query = json!({
            "fromMs": from,
            "toMs": to,
            "resolution": if daily { "day" } else { "hour" },
            "sessionSample": 200,
        });
        if daily {
            let starts =
                activity::local_day_starts(provisional.from, provisional.to, &tz, zone.as_ref())?;
            let starts_ms = starts.into_iter().map(ms_u64).collect::<R<Vec<_>>>()?;
            range_query
                .as_object_mut()
                .ok_or(Decline::Shape)?
                .insert("dayStarts".into(), json!(starts_ms));
        }
        let range = store.activity_range(range_query)?;
        let sources_state = store.activity_sources(from, to)?;
        activity::store_activity_answer(
            &range,
            &sources_state,
            &query,
            provisional.from,
            provisional.to,
            &live,
            zone.as_ref(),
            &pricing,
            clock,
        )
    };
    let retained_from = oldest;
    // The activity block: the guarded selection's fixed projection, or the store's own range.
    let (activity_block, coverage_oldest, providers_with_activity, resolution) =
        if activity::activity_selection(&query)? {
            let mut answered = read_activity()?;
            // Before this run's first pass the grid comes from the store's last reads.
            if live.sources.as_array().is_some_and(Vec::is_empty) {
                let history = answered.activity.get("history").ok_or(Decline::Shape)?;
                let grid = activity::grid_from_store(history)?;
                answered
                    .activity
                    .as_object_mut()
                    .ok_or(Decline::Shape)?
                    .insert("sources".into(), grid);
            }
            let resolution = text(answered.activity.get("resolution"))?.to_owned();
            (
                answered.activity,
                answered.oldest_hour_at,
                answered.providers_with_activity,
                Some(resolution),
            )
        } else {
            let (coverage_oldest, providers) = match guard_coverage.as_ref() {
                Some(guard) => {
                    let guard = obj(Some(guard))?;
                    let coverage = opt_num(guard.get("oldestHourAt"))?;
                    let providers: Vec<String> =
                        arr(guard.get("providersWithActivity").ok_or(Decline::Shape)?)?
                            .iter()
                            .map(|value| text(Some(value)).map(str::to_owned))
                            .collect::<R<_>>()?;
                    (coverage, providers)
                }
                // The native read: the store's own range and attribution answer the coverage.
                None => {
                    let answered = read_activity()?;
                    (answered.oldest_hour_at, answered.providers_with_activity)
                }
            };
            let block = activity::guarded_activity(
                &query,
                provisional.from,
                provisional.to,
                &live,
                zone.as_ref(),
                &pricing,
            )?;
            (block, coverage_oldest, providers, None)
        };

    #[allow(clippy::cast_precision_loss)]
    let oldest_quota = quota_oldest.map(|ms| ms as f64).unwrap_or(f64::INFINITY);
    let coverage = coverage_oldest.unwrap_or(f64::INFINITY);
    let oldest_available = analytics::js_min(oldest_quota, coverage);
    let today_midnight = if oldest_available.is_finite() {
        None
    } else {
        let today = activity::local_date_text(now, &tz, zone.as_ref())?;
        Some(activity::local_midnight(&today, &tz, zone.as_ref())?)
    };
    let available_from = if oldest_available.is_finite() {
        analytics::js_min(now, analytics::js_max(retained_from, oldest_available))
    } else {
        analytics::js_max(retained_from, today_midnight.ok_or(Decline::Date)?)
    };
    let range = if query.get("range").and_then(Value::as_str) == Some("all") {
        resolve_range(
            &query,
            now,
            available_from,
            Some(oldest),
            &tz,
            zone.as_ref(),
        )?
    } else {
        provisional.clone()
    };

    // The quota history, read here for the final range (90 days of it, thinned to the buckets).
    let quota_from = analytics::js_max(range.from, now - STORE_RETENTION_MS);
    let bucket_ms = ms_u64(range.bucket_minutes * 60_000.0)?;
    let history_query = json!({
        "fromMs": ms_u64(analytics::js_max(0.0, quota_from.floor()))?,
        "toMs": ms_u64(analytics::js_max(0.0, range.to.ceil()))?,
        "nowMs": ms_u64(now)?,
        "thinning": { "kind": "bucket", "bucketMs": bucket_ms },
    });
    let records_value = store.history_records(history_query)?;
    let records = arr(&records_value)?;
    let collected_since = match records.first().and_then(|r| r.get("sampledAt")) {
        Some(value @ Value::String(_)) => value.clone(),
        _ => json!(iso(now)?),
    };

    // The composition input, in the part-3 envelope's shape, with the facts built from the store.
    let mut instants = vec![now, range.from, range.to, available_from];
    if oldest_quota.is_finite() {
        instants.push(oldest_quota);
    }
    let facts = presentation(records, &table, &instants)?;
    let ctx = Context::new(facts);
    let finite_or_null = |value: f64| -> Value {
        if value.is_finite() {
            json!(value)
        } else {
            Value::Null
        }
    };
    let mut history_map = Object::new();
    history_map.insert("collectedSince".into(), collected_since);
    history_map.insert("records".into(), records_value);
    let mut data_map = Object::new();
    data_map.insert("query".into(), Value::Object(query));
    data_map.insert("now".into(), json!(now));
    data_map.insert("retainedFrom".into(), json!(retained_from));
    data_map.insert("bounds".into(), json!({ "oldest": oldest }));
    data_map.insert("oldestQuota".into(), finite_or_null(oldest_quota));
    data_map.insert("coverageOldestHourAt".into(), finite_or_null(coverage));
    data_map.insert(
        "provisional".into(),
        json!({
            "preset": provisional.preset,
            "from": provisional.from,
            "to": provisional.to,
            "tz": provisional.tz,
            "bucketMinutes": provisional.bucket_minutes,
        }),
    );
    data_map.insert(
        "todayMidnight".into(),
        today_midnight.map(|ms| json!(ms)).unwrap_or(Value::Null),
    );
    data_map.insert(
        "expected".into(),
        json!({
            "availableFrom": available_from,
            "range": {
                "preset": range.preset,
                "from": range.from,
                "to": range.to,
                "tz": range.tz,
                "bucketMinutes": range.bucket_minutes,
            },
        }),
    );
    data_map.insert("tz".into(), Value::String(tz));
    data_map.insert("resolution".into(), json!(resolution));
    data_map.insert("available".into(), Value::Bool(available));
    data_map.insert("stored".into(), Value::Bool(true));
    data_map.insert("history".into(), Value::Object(history_map));
    data_map.insert("dashboard".into(), Value::Object(dashboard));
    data_map.insert("providerTable".into(), Value::Array(table));
    data_map.insert(
        "providersWithActivity".into(),
        json!(providers_with_activity),
    );
    let mut composed =
        analytics::compose_data(&ctx, &data_map).map_err(|_| Decline::Unsupported)?;
    drop(data_map);
    if !ctx.valid() {
        return Err(Decline::Locale);
    }
    let map = composed.as_object_mut().ok_or(Decline::Shape)?;
    map.insert("activity".into(), activity_block);
    Ok(composed)
}
