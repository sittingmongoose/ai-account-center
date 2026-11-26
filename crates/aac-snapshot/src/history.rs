//! Burn rates and the rolling history (`agent-usage-history.ts`), from the dashboard's own
//! quota history plus every one-minute reading this dashboard saw in the last hour. Only
//! measured readings are used: a rate needs two real readings of the same window in the same
//! reset period, and nothing is extrapolated across a reset.

use crate::js::{
    self, boundary_identity, boundary_key, burn_key, date_parse, finite, long_burn_key, member,
    minute_iso,
};
use crate::projection::ProjectedAccount;
use aac_num::ecma;
use indexmap::IndexMap;
use serde_json::{Value, json};

const HOUR_MS: i64 = 3_600_000;
/// A larger fall than this between two readings is a reset, not noise.
const RESET_DROP_POINTS: f64 = 1.0;
/// Readings whose reset times differ by more than this belong to different periods.
const RESET_SHIFT_MS: i64 = 10 * 60_000;
/// Below this pace (percentage points per hour) a window is treated as not rising.
const MIN_PACE: f64 = 0.05;
/// A span under an hour sets the pace only when use moved at least this much.
const PACE_MIN_POINTS: f64 = 2.0;
/// Fractional percentages subtract inexactly: 32.3 - 30.3 is 1.9999999999999964.
const POINT_TOLERANCE: f64 = 1e-9;
/// How long the one-minute readings are kept in memory.
pub const RECENT_READINGS_MS: i64 = 65 * 60_000;
const MAX_RECENT_READINGS: usize = 180;

/// The five pace spans, in the order the pace picks them.
const HORIZONS: [(&str, f64, i64); 5] = [
    ("last5Minutes", 5.0 / 60.0, 2 * 60_000),
    ("last15Minutes", 15.0 / 60.0, 5 * 60_000),
    ("lastHour", 1.0, 15 * 60_000),
    ("last6Hours", 6.0, HOUR_MS),
    ("last24Hours", 24.0, 4 * HOUR_MS),
];

/// One reading of one window.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reading {
    /// The observation time, epoch milliseconds.
    pub at: i64,
    /// The used percentage, when reported.
    pub used: Option<f64>,
    /// The remaining amount, when reported.
    pub remaining: Option<f64>,
    /// The reset time the reading named, epoch milliseconds.
    pub reset_at: Option<i64>,
}

/// The measured burn of one window within its current reset period.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BurnRate {
    /// Percentage points per hour over the last five minutes.
    pub last5_minutes: Option<f64>,
    /// Over the last fifteen minutes.
    pub last15_minutes: Option<f64>,
    /// Over the last hour.
    pub last_hour: Option<f64>,
    /// Over the last six hours.
    pub last6_hours: Option<f64>,
    /// Over the last twenty-four hours.
    pub last24_hours: Option<f64>,
    /// The pace the projection uses: the shortest span with enough readings and move.
    pub pace: Option<f64>,
    /// That span in minutes.
    pub pace_minutes: Option<i64>,
    /// When the window reaches 100% at that pace; null when it is not rising.
    pub projected_full_at: Option<String>,
    /// Whether that comes before the window resets; null when either time is unknown.
    pub runs_out_before_reset: Option<bool>,
    /// When the current reset period's first recorded reading was taken.
    pub period_started_at: Option<String>,
    /// With the history store only: the 7- and 30-day burn across resets.
    pub long: Option<LongBurn>,
}

/// The store's long-span burn merged into a current pace.
#[derive(Clone, Debug, PartialEq)]
pub struct LongBurn {
    /// Points per hour over the last 7 days; null under half coverage.
    pub last7_days: Option<f64>,
    /// Points per hour over the last 30 days; null under half coverage.
    pub last30_days: Option<f64>,
    /// The share of the 7-day span the readings cover.
    pub coverage7_days: f64,
    /// The share of the 30-day span the readings cover.
    pub coverage30_days: f64,
}

impl BurnRate {
    /// The burn as `usage.json` writes it: the ten base fields, then the store's long fields.
    pub fn value(&self) -> Value {
        let mut members = vec![
            ("last5Minutes", js::opt_number(self.last5_minutes)),
            ("last15Minutes", js::opt_number(self.last15_minutes)),
            ("lastHour", js::opt_number(self.last_hour)),
            ("last6Hours", js::opt_number(self.last6_hours)),
            ("last24Hours", js::opt_number(self.last24_hours)),
            ("pace", js::opt_number(self.pace)),
            (
                "paceMinutes",
                self.pace_minutes.map(|m| json!(m)).unwrap_or(Value::Null),
            ),
            (
                "projectedFullAt",
                js::opt_str(self.projected_full_at.as_deref()),
            ),
            (
                "runsOutBeforeReset",
                self.runs_out_before_reset
                    .map(Value::Bool)
                    .unwrap_or(Value::Null),
            ),
            (
                "periodStartedAt",
                js::opt_str(self.period_started_at.as_deref()),
            ),
        ];
        if let Some(long) = &self.long {
            members.extend([
                ("last7Days", js::opt_number(long.last7_days)),
                ("last30Days", js::opt_number(long.last30_days)),
                ("coverage7Days", js::number(long.coverage7_days)),
                ("coverage30Days", js::number(long.coverage30_days)),
            ]);
        }
        js::object(members)
    }
}

/// One published history series: a window's measured readings over the last 7 days.
#[derive(Clone, Debug, PartialEq)]
pub struct HistorySeries {
    /// The projected account id.
    pub account_id: String,
    /// The provider id.
    pub provider: String,
    /// The projected account label.
    pub label: String,
    /// The projected email.
    pub email: Option<String>,
    /// The projected profile name.
    pub profile: Option<String>,
    /// Hidden on the dashboard by its owner.
    pub hidden: bool,
    /// The raw window key.
    pub window: String,
    /// The raw window label.
    pub window_label: String,
    /// The raw pool label.
    pub pool: Option<String>,
    /// The window kind, defaulting to rate_limit.
    pub kind: String,
    /// The amount unit.
    pub unit: Option<String>,
    /// Each point: [minute ISO, used percent, remaining amount, reset ISO or null].
    pub points: Vec<HistoryPoint>,
}

/// A history point's time, used percentage, remaining amount and reset time.
pub type HistoryPoint = (String, Option<f64>, Option<f64>, Option<String>);

impl HistorySeries {
    /// The series as `history.json` writes it.
    pub fn value(&self) -> Value {
        let points: Vec<Value> = self
            .points
            .iter()
            .map(|(at, used, remaining, reset)| {
                json!([
                    at,
                    js::opt_number(*used),
                    js::opt_number(*remaining),
                    js::opt_str(reset.as_deref())
                ])
            })
            .collect();
        js::object(vec![
            ("accountId", json!(self.account_id)),
            ("provider", json!(self.provider)),
            ("label", json!(self.label)),
            ("email", js::opt_str(self.email.as_deref())),
            ("profile", js::opt_str(self.profile.as_deref())),
            ("hidden", json!(self.hidden)),
            ("window", json!(self.window)),
            ("windowLabel", json!(self.window_label)),
            ("pool", js::opt_str(self.pool.as_deref())),
            ("kind", json!(self.kind)),
            ("unit", js::opt_str(self.unit.as_deref())),
            ("points", Value::Array(points)),
        ])
    }
}

/// `recordsByIdentity`: each account's records, oldest first, keyed by the history identity.
pub fn records_by_identity(history: Option<&Value>) -> IndexMap<String, Vec<&Value>> {
    let mut result: IndexMap<String, Vec<&Value>> = IndexMap::new();
    if let Some(records) = history.and_then(|h| member(h, "records").as_array()) {
        for record in records {
            let identity = member(record, "identity").as_str().unwrap_or("").to_owned();
            result.entry(identity).or_default().push(record);
        }
    }
    for list in result.values_mut() {
        list.sort_by(|a, b| {
            let left = date_parse(member(a, "sampledAt")).unwrap_or(0);
            let right = date_parse(member(b, "sampledAt")).unwrap_or(0);
            left.cmp(&right)
        });
    }
    result
}

/// `observedAt`: a retained optional window keeps its own original sample time.
fn observed_at(window: &Value, record_time: &Value) -> Option<i64> {
    let cached = member(window, "status").as_str() == Some("cached");
    let sampled = member(window, "sampledAt");
    let source = if cached && !sampled.is_null() {
        sampled
    } else {
        record_time
    };
    date_parse(source)
}

/// `windowReadings`: the window's readings, oldest first, one per observation time.
pub fn window_readings(records: &[&Value], window: &Value, since: i64) -> Vec<Reading> {
    let identity = crate::js::window_identity(window);
    let mut readings: Vec<Reading> = Vec::new();
    for record in records {
        let Some(entries) = member(record, "windows").as_array() else {
            continue;
        };
        let Some(matched) = entries
            .iter()
            .find(|entry| crate::js::window_identity(entry) == identity)
        else {
            continue;
        };
        let Some(at) = observed_at(matched, member(record, "sampledAt")) else {
            continue;
        };
        if at < since {
            continue;
        }
        let used = finite(member(matched, "usedPercent"));
        let remaining = finite(member(matched, "remaining"));
        let reset = date_parse(member(matched, "resetAt"));
        if used.is_none() && remaining.is_none() && reset.is_none() {
            continue;
        }
        readings.push(Reading {
            at,
            used,
            remaining,
            reset_at: reset,
        });
    }
    let mut by_time: IndexMap<i64, Reading> = IndexMap::new();
    for reading in readings {
        by_time.entry(reading.at).or_insert(reading);
    }
    let mut merged: Vec<Reading> = by_time.into_values().collect();
    merged.sort_by_key(|reading| reading.at);
    merged
}

/// `mergeReadings`: both lists merged, oldest first, one reading per time (the later wins a tie).
pub fn merge_readings(stored: Vec<Reading>, recent: Vec<Reading>) -> Vec<Reading> {
    let mut by_time: IndexMap<i64, Reading> = IndexMap::new();
    for reading in stored.into_iter().chain(recent) {
        by_time.insert(reading.at, reading);
    }
    let mut merged: Vec<Reading> = by_time.into_values().collect();
    merged.sort_by_key(|reading| reading.at);
    merged
}

/// `rowReading`: the reading a dashboard row carries for one window, at its own sample time.
pub fn row_reading(row: &Value, window: &Value) -> Option<Reading> {
    let status = member(row, "status").as_str().unwrap_or("");
    if status != "ok" && status != "cached" {
        return None;
    }
    let at = observed_at(window, member(row, "sampledAt"))?;
    let used = finite(member(window, "usedPercent"));
    let remaining = finite(member(window, "remaining"));
    let reset = date_parse(member(window, "resetAt"));
    if used.is_none() && remaining.is_none() && reset.is_none() {
        return None;
    }
    Some(Reading {
        at,
        used,
        remaining,
        reset_at: reset,
    })
}

/// `storedBoundaries`: every reset time the stored history reports for this window, whatever
/// unit each reading had.
pub fn stored_boundaries(records: &[&Value], window: &Value, since: i64) -> Vec<i64> {
    let identity = boundary_identity(window);
    let mut found: Vec<i64> = Vec::new();
    for record in records {
        let Some(entries) = member(record, "windows").as_array() else {
            continue;
        };
        for entry in entries {
            if boundary_identity(entry) != identity {
                continue;
            }
            let Some(at) = observed_at(entry, member(record, "sampledAt")) else {
                continue;
            };
            if at < since {
                continue;
            }
            if let Some(reset) = date_parse(member(entry, "resetAt"))
                && !found.contains(&reset)
            {
                found.push(reset);
            }
        }
    }
    found
}

/// The one-minute readings of the last hour, kept in memory by the snapshot writer.
#[derive(Default)]
pub struct RecentReadings {
    series: IndexMap<String, Vec<Reading>>,
    resets: IndexMap<String, IndexMap<i64, i64>>,
}

impl RecentReadings {
    /// An empty ring.
    pub fn new() -> Self {
        Self::default()
    }

    /// `add`: every sample's readings count, even one whose write is skipped.
    pub fn add(&mut self, rows: &[Value], now: i64) {
        for row in rows {
            let Some(windows) = member(row, "windows").as_array() else {
                continue;
            };
            for window in windows {
                let Some(reading) = row_reading(row, window) else {
                    continue;
                };
                if let Some(reset) = reading.reset_at {
                    let key = boundary_key(row, window);
                    self.resets
                        .entry(key)
                        .or_default()
                        .insert(reset, reading.at);
                }
                let key = long_burn_key(row, window);
                let list = self.series.entry(key).or_default();
                if !list.iter().any(|entry| entry.at == reading.at) {
                    list.push(reading);
                }
                list.sort_by_key(|entry| entry.at);
                if list.len() > MAX_RECENT_READINGS {
                    list.drain(..list.len() - MAX_RECENT_READINGS);
                }
            }
        }
        self.series.retain(|_, list| {
            list.retain(|reading| reading.at >= now - RECENT_READINGS_MS);
            !list.is_empty()
        });
        self.resets.retain(|_, seen| {
            seen.retain(|_, at| *at >= now - RECENT_READINGS_MS);
            !seen.is_empty()
        });
    }

    /// `boundaries`: reset times seen for this window in the last hour, any unit.
    pub fn boundaries(&self, row: &Value, window: &Value) -> Vec<i64> {
        self.resets
            .get(&boundary_key(row, window))
            .map(|seen| seen.keys().copied().collect())
            .unwrap_or_default()
    }

    /// `get`: the window's kept readings.
    pub fn get(&self, row: &Value, window: &Value) -> Vec<Reading> {
        self.series
            .get(&long_burn_key(row, window))
            .cloned()
            .unwrap_or_default()
    }
}

/// The reading names a reset still ahead of it.
fn upcoming(reading: &Reading) -> bool {
    reading.reset_at.is_some_and(|reset| reset > reading.at)
}

/// `currentPeriod`: the readings since the last reset.
fn current_period(readings: &[Reading], extra_boundaries: &[i64]) -> Vec<Reading> {
    let mut boundaries: Vec<i64> = Vec::new();
    let mut previous_reset: Option<(i64, i64)> = None;
    let mut period_start = i64::MIN;
    for reading in readings {
        let Some(reset) = reading.reset_at else {
            continue;
        };
        // A missing reset timestamp must not hide a transition between announcements.
        // If the old reset was still ahead when last reported, its known boundary can
        // retain intervening post-reset readings. Otherwise only the new announcement
        // establishes the start of the new series.
        if let Some((earlier_at, earlier_reset)) = previous_reset
            && (reset - earlier_reset).abs() > RESET_SHIFT_MS
        {
            period_start = if earlier_at < earlier_reset && earlier_reset <= reading.at {
                earlier_reset
            } else {
                reading.at
            };
        }
        previous_reset = Some((reading.at, reset));
        if !boundaries.contains(&reset) {
            boundaries.push(reset);
        }
    }
    for reset in extra_boundaries {
        if !boundaries.contains(reset) {
            boundaries.push(*reset);
        }
    }
    let usable: Vec<&Reading> = readings
        .iter()
        .filter(|reading| reading.at >= period_start && reading.used.is_some())
        .collect();
    if usable.is_empty() {
        return Vec::new();
    }
    let mut start = usable.len() - 1;
    while start > 0 {
        let earlier = usable[start - 1];
        let later = usable[start];
        if boundaries
            .iter()
            .any(|reset| earlier.at < *reset && *reset <= later.at)
        {
            break;
        }
        let same_upcoming = upcoming(earlier) && upcoming(later);
        let fall = earlier.used.unwrap_or(0.0) - later.used.unwrap_or(0.0);
        if fall
            > if same_upcoming {
                RESET_DROP_POINTS
            } else {
                0.0
            }
        {
            break;
        }
        start -= 1;
    }
    usable[start..].iter().map(|reading| **reading).collect()
}

/// `round`: Math.round to two fraction digits.
fn round(value: f64) -> f64 {
    ecma::math_round(value * 100.0) / 100.0
}

/// `burnRate`: one window's measured pace within its current reset period.
pub fn burn_rate(
    readings: &[Reading],
    reset_at: Option<&str>,
    extra_boundaries: &[i64],
) -> BurnRate {
    let period = current_period(readings, extra_boundaries);
    let mut rates = BurnRate::default();
    let mut pace: Option<f64> = None;
    let mut pace_minutes: Option<i64> = None;
    if let Some(latest) = period.last() {
        let latest_used = latest.used.unwrap_or(0.0);
        for (name, hours, minimum_span) in HORIZONS {
            let horizon_ms = (hours * HOUR_MS as f64) as i64;
            let Some(earliest) = period
                .iter()
                .find(|reading| reading.at >= latest.at - horizon_ms)
            else {
                continue;
            };
            if latest.at - earliest.at < minimum_span {
                continue;
            }
            let earliest_used = earliest.used.unwrap_or(0.0);
            let rate = round(
                (latest_used - earliest_used) / ((latest.at - earliest.at) as f64 / HOUR_MS as f64),
            );
            match name {
                "last5Minutes" => rates.last5_minutes = Some(rate),
                "last15Minutes" => rates.last15_minutes = Some(rate),
                "lastHour" => rates.last_hour = Some(rate),
                "last6Hours" => rates.last6_hours = Some(rate),
                "last24Hours" => rates.last24_hours = Some(rate),
                _ => {}
            }
            let moved = (latest_used - earliest_used).abs();
            if pace.is_none()
                && (moved >= PACE_MIN_POINTS - POINT_TOLERANCE
                    || latest.at - earliest.at >= HOUR_MS)
            {
                pace = Some(rate);
                pace_minutes = Some(ecma::math_round(hours * 60.0) as i64);
            }
        }
    }
    let mut projected_full_at: Option<String> = None;
    if let Some(latest) = period.last() {
        let latest_used = latest.used.unwrap_or(0.0);
        if latest_used >= 100.0 {
            projected_full_at = js::iso_of(latest.at);
        } else if let Some(pace) = pace
            && pace >= MIN_PACE
        {
            let full = latest.at as f64 + ((100.0 - latest_used) / pace) * HOUR_MS as f64;
            projected_full_at = js::iso_of(full as i64);
        }
    }
    let reset = reset_at.and_then(js::date_parse_str);
    rates.pace = pace;
    rates.pace_minutes = pace_minutes;
    rates.runs_out_before_reset = match (&projected_full_at, reset) {
        (Some(full), Some(reset)) => js::date_parse_str(full).map(|at| at < reset),
        _ => None,
    };
    rates.projected_full_at = projected_full_at;
    rates.period_started_at = period.first().and_then(|first| js::iso_of(first.at));
    rates
}

/// `usageHistory`: burn rates for every percent window of the projected accounts, plus the
/// rolling history series for every window (percent or amount).
pub fn usage_history(
    rows: &[Value],
    accounts: &[ProjectedAccount],
    history: Option<&Value>,
    now: i64,
    recent: Option<&RecentReadings>,
) -> (
    IndexMap<String, IndexMap<String, BurnRate>>,
    Vec<HistorySeries>,
) {
    let by_identity = records_by_identity(history);
    let since = now - crate::HISTORY_HOURS * HOUR_MS;
    let mut row_by_id: IndexMap<String, &Value> = IndexMap::new();
    for row in rows {
        let id = member(row, "id").as_str().unwrap_or("").to_owned();
        let newer = match row_by_id.get(&id) {
            Some(existing) => {
                let left = date_parse(member(row, "sampledAt"));
                let right = date_parse(member(existing, "sampledAt"));
                match (left, right) {
                    (Some(l), Some(r)) => l > r,
                    _ => false,
                }
            }
            None => true,
        };
        if newer {
            row_by_id.insert(id, row);
        }
    }
    let mut rates: IndexMap<String, IndexMap<String, BurnRate>> = IndexMap::new();
    let mut series: Vec<HistorySeries> = Vec::new();
    for account in accounts {
        let Some(row) = row_by_id.get(&account.id).copied() else {
            continue;
        };
        let empty: Vec<&Value> = Vec::new();
        let records = by_identity
            .get(&crate::js::account_identity(row))
            .unwrap_or(&empty);
        let mut per_window: IndexMap<String, BurnRate> = IndexMap::new();
        // Every window measured in the last 7 days keeps its history; only windows in the
        // current reading get a reading and a pace.
        let mut windows: IndexMap<String, Value> = IndexMap::new();
        for record in records {
            if date_parse(member(record, "sampledAt")).is_some_and(|at| at >= since)
                && let Some(entries) = member(record, "windows").as_array()
            {
                for window in entries {
                    windows.insert(crate::js::window_identity(window), window.clone());
                }
            }
        }
        let mut current_ids: Vec<String> = Vec::new();
        if let Some(entries) = member(row, "windows").as_array() {
            for window in entries {
                let id = crate::js::window_identity(window);
                current_ids.push(id.clone());
                windows.insert(id, window.clone());
            }
        }
        for (id, window) in &windows {
            let live = current_ids.contains(id);
            let current = if live { row_reading(row, window) } else { None };
            let mut recent_readings = recent.map(|ring| ring.get(row, window)).unwrap_or_default();
            if let Some(reading) = current {
                recent_readings.push(reading);
            }
            let readings: Vec<Reading> =
                merge_readings(window_readings(records, window, since), recent_readings)
                    .into_iter()
                    .filter(|reading| reading.at >= since && reading.at <= now)
                    .collect();
            let kind = member(window, "kind").as_str().unwrap_or("rate_limit");
            if live && finite(member(window, "usedPercent")).is_some() && kind == "rate_limit" {
                let mut boundaries = stored_boundaries(records, window, since);
                if let Some(ring) = recent {
                    boundaries.extend(ring.boundaries(row, window));
                }
                per_window.insert(
                    burn_key(
                        member(window, "key").as_str().unwrap_or(""),
                        member(window, "unit").as_str(),
                        member(window, "poolId").as_str(),
                    ),
                    burn_rate(&readings, member(window, "resetAt").as_str(), &boundaries),
                );
            }
            let measured: Vec<&Reading> = readings
                .iter()
                .filter(|reading| reading.used.is_some() || reading.remaining.is_some())
                .collect();
            if measured.is_empty() {
                continue;
            }
            let points = measured
                .iter()
                .filter_map(|reading| {
                    Some((
                        minute_iso(reading.at)?,
                        reading.used,
                        reading.remaining,
                        reading.reset_at.and_then(js::iso_of),
                    ))
                })
                .collect();
            series.push(HistorySeries {
                account_id: account.id.clone(),
                provider: account.provider.clone(),
                label: account.label.clone(),
                email: account.email.clone(),
                profile: account.profile.clone(),
                hidden: account.hidden,
                window: member(window, "key").as_str().unwrap_or("").to_owned(),
                window_label: member(window, "label").as_str().unwrap_or("").to_owned(),
                pool: member(window, "poolLabel").as_str().map(str::to_owned),
                kind: kind.to_owned(),
                unit: member(window, "unit").as_str().map(str::to_owned),
                points,
            });
        }
        rates.insert(account.id.clone(), per_window);
    }
    (rates, series)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_792_065_600_000;
    const MINUTE: i64 = 60_000;

    fn reading(minutes_ago: i64, used: f64, reset_at: Option<i64>) -> Reading {
        Reading {
            at: NOW - minutes_ago * MINUTE,
            used: Some(used),
            remaining: None,
            reset_at,
        }
    }

    #[test]
    fn burst_shows_at_once() {
        let readings = [
            reading(60, 10.0, None),
            reading(30, 12.0, None),
            reading(5, 15.0, None),
            reading(0, 25.0, None),
        ];
        let burn = burn_rate(&readings, Some("2026-10-15T14:00:00.000Z"), &[]);
        assert_eq!(burn.last5_minutes, Some(120.0));
        assert_eq!(burn.pace, Some(120.0));
        assert_eq!(burn.pace_minutes, Some(5));
        assert_eq!(burn.last_hour, Some(15.0));
        assert_eq!(
            burn.projected_full_at.as_deref(),
            Some("2026-10-15T12:37:30.000Z")
        );
        assert_eq!(burn.runs_out_before_reset, Some(true));
    }

    #[test]
    fn never_measures_across_a_reset() {
        let readings = [
            reading(50, 90.0, None),
            reading(40, 95.0, None),
            reading(10, 2.0, None),
            reading(0, 4.0, None),
        ];
        let burn = burn_rate(&readings, Some("2026-10-15T17:00:00.000Z"), &[]);
        assert_eq!(burn.period_started_at, js::iso_of(NOW - 10 * MINUTE));
        assert_eq!(burn.last_hour, None);
        assert_eq!(burn.last15_minutes, Some(12.0));
    }

    #[test]
    fn a_period_restart_is_not_burn() {
        // OpenCode Go weekly, 2026-11-19: 21:32 still named the weekly reset that had just
        // passed and read 0%; 21:35 named the next week's reset and read 39%. Not 300%/h.
        let restart = burn_rate(
            &[
                reading(3, 0.0, Some(NOW - 5 * MINUTE)),
                reading(0, 39.0, Some(NOW + 7 * 24 * 60 * MINUTE)),
            ],
            None,
            &[],
        );
        assert_eq!(restart.period_started_at, js::iso_of(NOW));
        assert_eq!(restart.last5_minutes, None);
        assert_eq!(restart.pace, None);
        assert_eq!(restart.projected_full_at, None);
        assert_eq!(restart.runs_out_before_reset, None);
        // A reset drop still splits, and the 24-hour rate is not measured across it.
        let reset = burn_rate(
            &[
                reading(10, 90.0, None),
                reading(4, 2.0, None),
                reading(0, 4.0, None),
            ],
            None,
            &[],
        );
        assert_eq!(reset.period_started_at, js::iso_of(NOW - 4 * MINUTE));
        assert_eq!(reset.last24_hours, None);
        // Within one reported period an ordinary series keeps its rate.
        let period = NOW + 60 * MINUTE;
        let steady = [
            reading(10, 20.0, Some(period)),
            reading(5, 25.0, Some(period)),
            reading(0, 30.0, Some(period)),
        ];
        let burn = burn_rate(&steady, None, &[]);
        assert_eq!(burn.last5_minutes, Some(60.0));
        assert_eq!(burn.pace, Some(60.0));
        assert_eq!(burn.pace_minutes, Some(5));
    }

    #[test]
    fn a_reset_only_reading_keeps_the_old_period_out_of_the_pace() {
        let new_reset = NOW + 60 * MINUTE;
        let readings = [
            reading(4, 0.0, Some(NOW - 5 * MINUTE)),
            Reading {
                used: None,
                ..reading(3, 0.0, Some(new_reset))
            },
            reading(2, 39.0, None),
            reading(0, 41.0, Some(new_reset)),
        ];
        let first = burn_rate(&readings[..3], None, &[]);
        assert_eq!(first.period_started_at, js::iso_of(NOW - 2 * MINUTE));
        assert_eq!(first.last5_minutes, None);
        assert_eq!(first.pace, None);
        assert_eq!(first.projected_full_at, None);

        let second = burn_rate(&readings, None, &[]);
        assert_eq!(second.period_started_at, first.period_started_at);
        assert_eq!(second.pace, Some(60.0));
    }

    #[test]
    fn fractional_two_point_move_is_a_burst() {
        let burst = burn_rate(
            &[
                reading(60, 20.3, None),
                reading(5, 30.3, None),
                reading(0, 32.3, None),
            ],
            None,
            &[],
        );
        assert_eq!(burst.pace, Some(24.0));
        assert_eq!(burst.pace_minutes, Some(5));
        let below = burn_rate(
            &[
                reading(60, 20.3, None),
                reading(5, 30.3, None),
                reading(0, 31.8, None),
            ],
            None,
            &[],
        );
        assert_eq!(below.pace_minutes, Some(60));
    }

    #[test]
    fn one_point_reset_without_reported_time_splits() {
        let burn = burn_rate(
            &[
                reading(5, 1.0, None),
                reading(4, 0.0, None),
                reading(0, 2.0, None),
            ],
            None,
            &[],
        );
        assert_eq!(burn.period_started_at, js::iso_of(NOW - 4 * MINUTE));
        assert_eq!(burn.last5_minutes, Some(30.0));
        let reset = NOW + 120 * MINUTE;
        let eased = burn_rate(
            &[
                reading(5, 20.0, Some(reset)),
                reading(4, 19.0, Some(reset)),
                reading(0, 25.0, Some(reset)),
            ],
            None,
            &[],
        );
        assert_eq!(eased.period_started_at, js::iso_of(NOW - 5 * MINUTE));
    }

    #[test]
    fn moved_reset_time_starts_a_new_period() {
        let moved = [
            reading(30, 50.0, Some(NOW + 60 * MINUTE)),
            reading(5, 50.0, Some(NOW + 300 * MINUTE)),
            reading(0, 51.0, Some(NOW + 300 * MINUTE)),
        ];
        assert_eq!(
            burn_rate(&moved, None, &[]).period_started_at,
            js::iso_of(NOW - 5 * MINUTE)
        );
        let flat = burn_rate(
            &[reading(10, 30.0, None), reading(0, 30.0, None)],
            None,
            &[],
        );
        assert_eq!(flat.pace, None);
        assert_eq!(flat.projected_full_at, None);
        assert_eq!(flat.runs_out_before_reset, None);
    }

    #[test]
    fn reported_reset_between_readings_splits_even_when_omitted_later() {
        let burn = burn_rate(
            &[
                reading(10, 50.0, Some(NOW - 5 * MINUTE)),
                reading(0, 60.0, None),
            ],
            None,
            &[],
        );
        assert_eq!(burn.period_started_at, js::iso_of(NOW));
        assert_eq!(burn.pace, None);
        let late = burn_rate(
            &[
                reading(15, 40.0, Some(NOW - 5 * MINUTE)),
                reading(4, 10.0, None),
                reading(0, 20.0, Some(NOW + 295 * MINUTE)),
            ],
            None,
            &[],
        );
        assert_eq!(late.period_started_at, js::iso_of(NOW - 4 * MINUTE));
        assert_eq!(late.pace, Some(150.0));
        let reported_later = burn_rate(
            &[
                reading(10, 45.0, None),
                reading(0, 60.0, Some(NOW - 5 * MINUTE)),
            ],
            None,
            &[],
        );
        assert_eq!(reported_later.pace, None);
    }

    #[test]
    fn no_rate_without_two_readings_far_enough_apart() {
        let burn = burn_rate(&[reading(1, 30.0, None), reading(0, 31.0, None)], None, &[]);
        assert_eq!(burn.pace, None);
        assert_eq!(burn.last5_minutes, None);
    }

    #[test]
    fn rounding_step_within_minutes_is_not_the_pace() {
        let mut readings: Vec<Reading> = (0..25)
            .map(|step| reading(125 - step * 5, 20.0, None))
            .collect();
        readings.push(reading(0, 21.0, None));
        let burn = burn_rate(
            &readings,
            js::iso_of(NOW + 2 * 24 * 60 * MINUTE).as_deref(),
            &[],
        );
        assert_eq!(burn.last5_minutes, Some(12.0));
        assert_eq!(burn.pace, Some(1.0));
        assert_eq!(burn.pace_minutes, Some(60));
        assert_eq!(burn.runs_out_before_reset, Some(false));
        let burst = burn_rate(
            &[
                reading(60, 20.0, None),
                reading(5, 20.0, None),
                reading(0, 22.0, None),
            ],
            None,
            &[],
        );
        assert_eq!(burst.pace, Some(24.0));
        assert_eq!(burst.pace_minutes, Some(5));
    }

    #[test]
    fn recent_ring_keeps_every_minute_for_an_hour() {
        let mut ring = RecentReadings::new();
        let window = json!({"key": "five_hour", "label": "5-hour", "usedPercent": 40, "resetAt": "2026-10-15T14:00:00.000Z"});
        for minute in (0..=70).rev() {
            let mut row = json!({
                "id": "codex:gmail", "provider": "codex", "email": "person@example.com",
                "status": "ok", "sampledAt": js::iso_of(NOW - minute * MINUTE),
                "windows": [window.clone()]
            });
            row["windows"][0]["usedPercent"] = json!(if minute == 0 { 45 } else { 40 });
            ring.add(&[row], NOW - minute * MINUTE);
        }
        let row = json!({
            "id": "codex:gmail", "provider": "codex", "email": "person@example.com",
            "status": "ok", "sampledAt": js::iso_of(NOW), "windows": [window.clone()]
        });
        let readings = ring.get(&row, &window);
        assert!(readings[0].at >= NOW - 65 * MINUTE);
        assert_eq!(readings.last().and_then(|r| r.used), Some(45.0));
    }
}
