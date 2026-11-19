//! GET /api/accounts/analytics without its activity block: the port of
//! `src/web-server/services/account-analytics-compose.ts` (`analyticsAvailableRange` and
//! `composeAccountAnalytics`), plus `dashboardRegistryFacts`/`analyticsProviderRows` and the two
//! identities. Inputs are what the TS computation already read, in the M3 presentation envelope
//! (`rustReadInput`): `Date.parse`, `toISOString` and `localeCompare` answers come from its facts,
//! never from host formatting. This module reads nothing; every unproven value is a [`Refuse`].
use crate::spread::{Admit, Refuse, Spread, req, truthy, utf16_len};
use aac_core::{AccountKey, ProviderId};
use aac_status::read_context::{Context, Envelope};
use serde_json::{Map, Value, json};
use std::{cmp::Ordering, collections::HashMap};

const HOUR_MS: f64 = 3_600_000.0;
const DAY_MS: f64 = 86_400_000.0;
const RETAINED_MS: f64 = 31.0 * DAY_MS;
const STORE_RETENTION_DAYS: i64 = 90;
const MAX_POINTS_PER_WINDOW: usize = 249;
const MAX_WINDOWS: usize = 256;

type Object = Map<String, Value>;

fn num(object: &Object, key: &str) -> Admit<f64> {
    req(object.get(key).and_then(Value::as_f64))
}

fn text<'a>(object: &'a Object, key: &str) -> Admit<&'a str> {
    req(object.get(key).and_then(Value::as_str))
}

fn nullable_num(object: &Object, key: &str) -> Admit<Option<f64>> {
    match object.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => Ok(Some(req(value.as_f64())?)),
    }
}

/// `Math.max` / `Math.min` for non-NaN doubles, including the signed-zero rules.
pub(crate) fn js_max(a: f64, b: f64) -> f64 {
    if a == b {
        if a.is_sign_positive() { a } else { b }
    } else if a > b {
        a
    } else {
        b
    }
}
pub(crate) fn js_min(a: f64, b: f64) -> f64 {
    if a == b {
        if a.is_sign_negative() { a } else { b }
    } else if a < b {
        a
    } else {
        b
    }
}

/// An integral JavaScript time value (|t| <= 8.64e15, exact in i64).
fn time(value: f64) -> Admit<i64> {
    if value.is_finite() && value.fract() == 0.0 && value.abs() <= 8.64e15 {
        #[allow(clippy::cast_possible_truncation)]
        Ok(value as i64)
    } else {
        Err(Refuse)
    }
}

/// `new Date(t).toISOString()` from the presentation facts (missing marks the context invalid).
fn iso(ctx: &Context, value: f64) -> Admit<Value> {
    Ok(ctx.iso(Some(time(value)?)))
}

/// A string `Date.parse`; `None` is NaN. The facts hold every finite parse of a string shaped
/// `YYYY-MM-DDT…`, so such a string without one is NaN; the empty string is NaN; any other
/// spelling is refused (engines differ on non-standard forms).
fn parse_time(ctx: &Context, value: Option<&Value>) -> Admit<Option<f64>> {
    let Some(Value::String(text)) = value else {
        return Err(Refuse);
    };
    match crate::js::date_parse(ctx, text) {
        #[allow(clippy::cast_precision_loss)]
        Some(ms) => Ok(ms.map(|ms| ms as f64)),
        None => Err(Refuse),
    }
}

pub(crate) fn bucket_minutes(span: f64) -> i64 {
    if span <= DAY_MS + 2.0 * HOUR_MS {
        10
    } else if span <= 7.0 * DAY_MS + 2.0 * HOUR_MS {
        60
    } else if span <= 31.0 * DAY_MS + 2.0 * HOUR_MS {
        180
    } else {
        1440
    }
}

/// One resolved range: `ResolvedAccountAnalyticsRange`.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Range {
    pub(crate) preset: String,
    pub(crate) from: f64,
    pub(crate) to: f64,
    pub(crate) tz: String,
    pub(crate) bucket_minutes: f64,
}

impl Range {
    pub(crate) fn decode(value: Option<&Value>) -> Admit<Self> {
        let object = req(value.and_then(Value::as_object))?;
        Ok(Self {
            preset: text(object, "preset")?.to_owned(),
            from: num(object, "from")?,
            to: num(object, "to")?,
            tz: text(object, "tz")?.to_owned(),
            bucket_minutes: num(object, "bucketMinutes")?,
        })
    }
}

/// `accountAnalyticsIdentity` (`aac_core::AccountKey`). Only ASCII addresses are admitted, so
/// JavaScript and Rust lowercase them identically.
fn account_identity(account: &Spread) -> Admit<String> {
    let provider = ProviderId::new(req(account.get("provider").and_then(Value::as_str))?)
        .map_err(|_| Refuse)?;
    let id = req(account.get("id").and_then(Value::as_str))?;
    let email = match account.get("email") {
        None | Some(Value::Null) => None,
        Some(Value::String(email)) if email.is_ascii() => Some(email.as_str()),
        Some(_) => return Err(Refuse),
    };
    AccountKey::derive(&provider, id, email)
        .map(|key| key.as_str().to_owned())
        .map_err(|_| Refuse)
}

/// `analyticsWindowIdentity`: `[key, kind ?? null, unit]` plus a truthy pool id. Only equality
/// of identities is used, so any injective encoding of those text-or-null parts is exact.
fn window_identity(window: &Spread) -> Admit<String> {
    let part = |key: &str| -> Admit<Value> {
        match window.get(key) {
            None | Some(Value::Null) => Ok(Value::Null),
            Some(value @ Value::String(_)) => Ok(value.clone()),
            Some(_) => Err(Refuse),
        }
    };
    let mut parts = vec![part("key")?, part("kind")?, part("unit")?];
    match window.get("poolId") {
        None | Some(Value::Null) => {}
        Some(Value::String(pool)) if pool.is_empty() => {}
        Some(pool @ Value::String(_)) => parts.push(pool.clone()),
        Some(_) => return Err(Refuse),
    }
    Ok(Value::Array(parts).to_string())
}

/// A dashboard account (entries) and its windows (each entries).
struct Account {
    value: Spread,
    windows: Vec<Spread>,
}

fn decode_account(value: &Value) -> Admit<Account> {
    let value = Spread::decode(value)?;
    let windows = req(value.get("windows").and_then(Value::as_array))?
        .iter()
        .map(Spread::decode)
        .collect::<Admit<Vec<_>>>()?;
    req(value.get("provider").and_then(Value::as_str))?;
    req(value.get("id").and_then(Value::as_str))?;
    Ok(Account { value, windows })
}

/// One retained quota observation, with its windows decoded.
struct Record {
    value: Object,
    identity: String,
    sampled_at: String,
    windows: Vec<Spread>,
}

fn decode_record(value: &Value) -> Admit<Record> {
    let object = req(value.as_object())?;
    let windows = req(object.get("windows").and_then(Value::as_array))?
        .iter()
        .map(Spread::decode)
        .collect::<Admit<Vec<_>>>()?;
    text(object, "source")?;
    Ok(Record {
        identity: text(object, "identity")?.to_owned(),
        sampled_at: text(object, "sampledAt")?.to_owned(),
        value: object.clone(),
        windows,
    })
}

/// `dashboardRegistryFacts`.
struct Registry {
    /// Sorted rows: id, label, order.
    table: Vec<(String, Value, Value)>,
    visible: HashMap<String, bool>,
    hidden_providers: Vec<String>,
    hidden_accounts: Vec<String>,
}

fn string_set(settings: Option<&Object>, key: &str) -> Vec<String> {
    settings
        .and_then(|s| s.get(key))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(|value| value.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

impl Registry {
    fn new(ctx: &Context, dashboard: &Object, table: &[Value]) -> Admit<Self> {
        let entries: Vec<&Object> = match dashboard.get("providers") {
            Some(Value::Array(values)) => values.iter().filter_map(Value::as_object).collect(),
            _ => Vec::new(),
        };
        let settings = dashboard.get("settings").and_then(Value::as_object);
        let hidden_providers = string_set(settings, "hiddenProviders");
        let hidden_accounts = string_set(settings, "hiddenAccountIds");
        let mut merged: Vec<(String, Value, Value, f64)> = Vec::new();
        let mut visible: HashMap<String, bool> = HashMap::new();
        for row in table {
            let row = req(row.as_object())?;
            let id = text(row, "id")?.to_owned();
            let row_label = req(row.get("label"))?.clone();
            let row_order = req(row.get("order"))?.clone();
            let entry = entries
                .iter()
                .find(|candidate| candidate.get("id").and_then(Value::as_str) == Some(id.as_str()));
            let label = match entry.and_then(|e| e.get("label")) {
                Some(Value::String(label)) if !label.is_empty() && utf16_len(label) <= 80 => {
                    json!(label)
                }
                _ => row_label,
            };
            let order = match entry.and_then(|e| e.get("order")) {
                Some(order @ Value::Number(_)) => order.clone(),
                _ => row_order,
            };
            let shown = match entry.and_then(|e| e.get("visible")) {
                Some(Value::Bool(shown)) => *shown,
                _ => !hidden_providers.contains(&id),
            };
            visible.insert(id.clone(), shown);
            let sort = req(order.as_f64())?;
            merged.push((id, label, order, sort));
        }
        // `a.order - b.order || a.id.localeCompare(b.id)`, with the TS collation ranks.
        merged.sort_by(|a, b| match a.3.partial_cmp(&b.3) {
            Some(Ordering::Equal) | None => ctx.compare(&a.0, &b.0),
            Some(order) => order,
        });
        Ok(Self {
            table: merged
                .into_iter()
                .map(|(id, label, order, _)| (id, label, order))
                .collect(),
            visible,
            hidden_providers,
            hidden_accounts,
        })
    }

    fn visible(&self, provider: &str) -> bool {
        self.visible
            .get(provider)
            .copied()
            .unwrap_or_else(|| !self.hidden_providers.iter().any(|id| id == provider))
    }

    fn hidden(&self, account: &Spread) -> Admit<bool> {
        if let Some(Value::Bool(own)) = account.get("hidden") {
            return Ok(*own);
        }
        let provider = req(account.get("provider").and_then(Value::as_str))?;
        let id = req(account.get("id").and_then(Value::as_str))?;
        Ok(!self.visible.get(provider).copied().unwrap_or(true)
            || self.hidden_accounts.iter().any(|hidden| hidden == id))
    }
}

const AMOUNTS: [&str; 7] = [
    "usedPercent",
    "remainingPercent",
    "used",
    "limit",
    "remaining",
    "resetAt",
    "expiresAt",
];

/// `point(record, window)`.
fn point(record: &Record, window: Option<&Spread>) -> Spread {
    let cached = window.and_then(|w| w.get("status")).and_then(Value::as_str) == Some("cached");
    let window_value = |key: &str| {
        window
            .and_then(|w| w.get(key))
            .filter(|value| !value.is_null())
            .cloned()
            .unwrap_or(Value::Null)
    };
    let mut point = Spread::default();
    let sampled = if cached {
        match window.and_then(|w| w.get("sampledAt")) {
            None | Some(Value::Null) => json!(""),
            Some(value) => value.clone(),
        }
    } else {
        json!(record.sampled_at)
    };
    point.set("sampledAt", Some(sampled));
    point.set("observedAt", record.value.get("observedAt").cloned());
    for key in AMOUNTS {
        point.set(key, Some(window_value(key)));
    }
    let source = record
        .value
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("");
    point.set(
        "source",
        Some(json!(if cached {
            format!("{source} (cached window)")
        } else {
            source.to_owned()
        })),
    );
    point.set("platform", record.value.get("platform").cloned());
    point.set(
        "status",
        if cached {
            Some(json!("cached"))
        } else {
            record.value.get("status").cloned()
        },
    );
    point.set("isActive", record.value.get("isActive").cloned());
    point
}

/// JavaScript `Map` with insertion order: `set` keeps an existing key's position.
struct OrderedMap<T> {
    keys: Vec<String>,
    values: HashMap<String, T>,
}
impl<T: Clone> OrderedMap<T> {
    fn new() -> Self {
        Self {
            keys: Vec::new(),
            values: HashMap::new(),
        }
    }
    fn set(&mut self, key: String, value: T) {
        if !self.values.contains_key(&key) {
            self.keys.push(key.clone());
        }
        self.values.insert(key, value);
    }
    fn has(&self, key: &str) -> bool {
        self.values.contains_key(key)
    }
    fn entries(&self) -> Vec<(String, T)> {
        self.keys
            .iter()
            .filter_map(|key| self.values.get(key).map(|v| (key.clone(), v.clone())))
            .collect()
    }
}

struct Series {
    value: Value,
    count: usize,
    first: Option<String>,
    last: Option<String>,
}

fn account_series(
    ctx: &Context,
    account: &Account,
    records: &[Record],
    range: &Range,
    hidden: bool,
) -> Admit<Series> {
    let identity = account_identity(&account.value)?;
    let mut selected: Vec<&Record> = Vec::new();
    for record in records {
        if record.identity != identity {
            continue;
        }
        let Some(sampled) = parse_time(ctx, Some(&json!(record.sampled_at)))? else {
            continue;
        };
        if sampled >= range.from && sampled <= range.to {
            selected.push(record);
        }
    }
    let mut windows: OrderedMap<Spread> = OrderedMap::new();
    for record in &selected {
        for window in &record.windows {
            windows.set(window_identity(window)?, window.clone());
        }
    }
    for window in &account.windows {
        windows.set(window_identity(window)?, window.clone());
    }
    let mut current: OrderedMap<Spread> = OrderedMap::new();
    for window in &account.windows {
        current.set(window_identity(window)?, window.clone());
    }
    let mut preferred = OrderedMap::new();
    for (key, window) in current.entries() {
        preferred.set(key, window);
    }
    for (key, window) in windows.entries().into_iter().rev() {
        if !preferred.has(&key) {
            preferred.set(key, window);
        }
    }
    let bucket_ms = range.bucket_minutes * 60_000.0;
    let mut out_windows = Vec::new();
    for (identity_key, window) in preferred.entries().into_iter().take(MAX_WINDOWS) {
        let mut buckets: OrderedMap<(f64, Spread)> = OrderedMap::new();
        for record in &selected {
            let mut matching = None;
            for candidate in &record.windows {
                if window_identity(candidate)? == identity_key {
                    matching = Some(candidate);
                    break;
                }
            }
            let actual = point(record, matching);
            let Some(at) = parse_time(ctx, actual.get("sampledAt"))? else {
                continue;
            };
            if at < range.from || at > range.to {
                continue;
            }
            let bucket = (at / bucket_ms).floor();
            buckets.set(format!("{bucket}"), (at, actual));
        }
        let mut points: Vec<(f64, Spread)> =
            buckets.entries().into_iter().map(|(_, v)| v).collect();
        points.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(Ordering::Equal));
        let skip = points.len().saturating_sub(MAX_POINTS_PER_WINDOW);
        let mut out = window.clone();
        if !current.has(&identity_key) {
            for key in AMOUNTS {
                out.set(key, Some(Value::Null));
            }
            out.set("unlimited", None);
            out.set("enabled", None);
        }
        out.set(
            "points",
            Some(Value::Array(
                points
                    .into_iter()
                    .skip(skip)
                    .map(|(_, point)| point.into_value())
                    .collect(),
            )),
        );
        out_windows.push(out.into_value());
    }
    let first = selected.first().map(|r| r.sampled_at.clone());
    let last = selected.last().map(|r| r.sampled_at.clone());
    let mut out = account.value.clone();
    out.set("hidden", Some(json!(hidden)));
    out.set("firstSampleAt", Some(json!(first)));
    out.set("lastSampleAt", Some(json!(last)));
    out.set("sampleCount", Some(json!(selected.len())));
    out.set("windows", Some(Value::Array(out_windows)));
    Ok(Series {
        value: out.into_value(),
        count: selected.len(),
        first,
        last,
    })
}

fn available(account: &Spread) -> bool {
    matches!(
        account.get("status").and_then(Value::as_str),
        Some("ok" | "cached")
    )
}

/// The answer without `activity`, or a refusal (TS then composes it).
pub fn compose(envelope: Envelope) -> Admit<Value> {
    let ctx = Context::new(envelope.presentation);
    let input = req(envelope.data.as_object())?;
    let answer = compose_data(&ctx, input)?;
    // Every Date.parse/toISOString/localeCompare answer came from a supplied fact.
    if ctx.valid() { Ok(answer) } else { Err(Refuse) }
}

pub(crate) fn compose_data(ctx: &Context, input: &Object) -> Admit<Value> {
    let query = req(input.get("query").and_then(Value::as_object))?;
    let range_preset = text(query, "range")?;
    let now = num(input, "now")?;
    let retained_from = num(input, "retainedFrom")?;
    let bounds = match input.get("bounds") {
        None | Some(Value::Null) => None,
        Some(Value::Object(bounds)) => Some(num(bounds, "oldest")?),
        Some(_) => return Err(Refuse),
    };
    let oldest_quota = nullable_num(input, "oldestQuota")?.unwrap_or(f64::INFINITY);
    let coverage = nullable_num(input, "coverageOldestHourAt")?.unwrap_or(f64::INFINITY);
    let provisional = Range::decode(input.get("provisional"))?;
    let expected = req(input.get("expected").and_then(Value::as_object))?;

    // analyticsAvailableRange
    let oldest = js_min(oldest_quota, coverage);
    let available_from = if oldest.is_finite() {
        js_min(now, js_max(retained_from, oldest))
    } else {
        js_max(retained_from, req(nullable_num(input, "todayMidnight")?)?)
    };
    let range = if range_preset == "all" {
        if query.contains_key("from") || query.contains_key("to") {
            return Err(Refuse);
        }
        let tz = match query.get("tz") {
            None => "UTC".to_owned(),
            Some(Value::String(tz)) => tz.clone(),
            Some(_) => return Err(Refuse),
        };
        let retained = match bounds {
            Some(oldest) => js_min(now, oldest),
            None => now - RETAINED_MS,
        };
        let from = js_min(now, js_max(retained, available_from));
        #[allow(clippy::cast_precision_loss)]
        let minutes = bucket_minutes(now - from) as f64;
        Range {
            preset: "all".into(),
            from,
            to: now,
            tz,
            bucket_minutes: minutes,
        }
    } else {
        provisional
    };
    // The TS computation read the quota history for its own range; any disagreement refuses.
    if num(expected, "availableFrom")? != available_from
        || Range::decode(expected.get("range"))? != range
    {
        return Err(Refuse);
    }

    let tz = text(input, "tz")?;
    let dashboard = req(input.get("dashboard").and_then(Value::as_object))?;
    let all_accounts = req(dashboard.get("accounts").and_then(Value::as_array))?
        .iter()
        .map(decode_account)
        .collect::<Admit<Vec<_>>>()?;
    let records = match input.get("history") {
        None | Some(Value::Null) => None,
        Some(Value::Object(history)) => Some((
            history.get("collectedSince").cloned(),
            req(history.get("records").and_then(Value::as_array))?
                .iter()
                .map(decode_record)
                .collect::<Admit<Vec<_>>>()?,
        )),
        Some(_) => return Err(Refuse),
    };
    let empty = Vec::new();
    let record_list = records.as_ref().map_or(&empty, |(_, list)| list);
    let table = req(input.get("providerTable").and_then(Value::as_array))?;
    let registry = Registry::new(ctx, dashboard, table)?;
    let provider_filter = text(query, "provider")?;
    let account_filter = text(query, "account")?;
    let field = |account: &Account, key: &str| -> Option<String> {
        account
            .value
            .get(key)
            .and_then(Value::as_str)
            .map(str::to_owned)
    };

    let accounts: Vec<&Account> = all_accounts
        .iter()
        .filter(|account| {
            (provider_filter == "all"
                || field(account, "provider").as_deref() == Some(provider_filter))
                && (account_filter == "all"
                    || field(account, "id").as_deref() == Some(account_filter))
        })
        .collect();
    let mut series = Vec::new();
    let mut sample_count = 0usize;
    let mut stamps = Vec::new();
    for account in &accounts {
        let hidden = registry.hidden(&account.value)?;
        let one = account_series(ctx, account, record_list, &range, hidden)?;
        sample_count += one.count;
        stamps.extend(one.first);
        stamps.extend(one.last);
        series.push(one.value);
    }
    // Default sort: UTF-16 order, which is byte order for the ASCII ISO stamps admitted here.
    stamps.sort();

    // analyticsProviderRows
    let mut latest: HashMap<String, (String, f64)> = HashMap::new();
    for record in record_list {
        let Some(sampled) = parse_time(ctx, Some(&json!(record.sampled_at)))? else {
            continue;
        };
        if sampled < range.from || sampled > range.to {
            continue;
        }
        let newer = latest
            .get(&record.identity)
            .is_none_or(|(_, previous)| *previous < sampled);
        if newer {
            latest.insert(
                record.identity.clone(),
                (record.sampled_at.clone(), sampled),
            );
        }
    }
    let activity: Vec<String> = req(input.get("providersWithActivity").and_then(Value::as_array))?
        .iter()
        .map(|value| req(value.as_str()).map(str::to_owned))
        .collect::<Admit<_>>()?;
    let mut providers = Vec::new();
    for (id, label, order) in &registry.table {
        let all: Vec<&Account> = all_accounts
            .iter()
            .filter(|account| field(account, "provider").as_deref() == Some(id.as_str()))
            .collect();
        let has_activity = activity.iter().any(|provider| provider == id);
        if all.is_empty() && !has_activity {
            continue;
        }
        let matching: Vec<&Account> = all
            .into_iter()
            .filter(|account| {
                account_filter == "all" || field(account, "id").as_deref() == Some(account_filter)
            })
            .collect();
        let mut sampled = Vec::new();
        for account in &matching {
            if let Some(found) = latest.get(&account_identity(&account.value)?) {
                sampled.push(found.clone());
            }
        }
        sampled.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(Ordering::Equal));
        providers.push(json!({
            "provider": id,
            "label": label,
            "order": order,
            "visible": registry.visible(id),
            "accountCount": matching.len(),
            "availableAccounts": matching.iter().filter(|a| available(&a.value)).count(),
            "latestSampleAt": sampled.last().map(|(stamp, _)| stamp),
            "hasQuotaHistory": !sampled.is_empty(),
            "hasActivity": has_activity,
        }));
    }

    let active = |provider: &str| -> Value {
        all_accounts
            .iter()
            .find(|account| {
                field(account, "provider").as_deref() == Some(provider)
                    && truthy(account.value.get("isActive"))
            })
            .and_then(|account| account.value.get("id").cloned())
            .unwrap_or(Value::Null)
    };
    let mut active_ids = Object::new();
    for (id, _, _) in &registry.table {
        if id == "codex" || id == "antigravity" {
            active_ids.insert(id.clone(), active(id));
        }
    }

    let stored = req(input.get("stored").and_then(Value::as_bool))?;
    let available_history = req(input.get("available").and_then(Value::as_bool))?;
    let resolution = match input.get("resolution") {
        None | Some(Value::Null) => json!("hour"),
        Some(value @ Value::String(_)) => value.clone(),
        Some(_) => return Err(Refuse),
    };
    let mut range_out = json!({
        "preset": range.preset,
        "from": iso(ctx, range.from)?,
        "to": iso(ctx, range.to)?,
        "bucketMinutes": time(range.bucket_minutes)?,
        "tz": tz,
        "dayBucketTz": tz,
        "availableFrom": iso(ctx, available_from)?,
        "resolution": resolution,
    });
    if range_preset == "custom" {
        let day = |key: &str| {
            query
                .get(key)
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        };
        if let (Some(first), Some(last), Some(out)) =
            (day("from"), day("to"), range_out.as_object_mut())
        {
            out.insert("days".into(), json!({ "from": first, "to": last }));
        }
    }
    let collected_since = if stored && oldest_quota.is_finite() {
        iso(ctx, oldest_quota)?
    } else {
        match records.as_ref().and_then(|(since, _)| since.clone()) {
            None | Some(Value::Null) => Value::Null,
            Some(value) => value,
        }
    };
    let message = if !available_history {
        "Quota history could not be saved or read. Current account usage remains available."
    } else if sample_count == 0 {
        "No measured quota history is available for this selection yet."
    } else if stored {
        "Every changed quota reading is kept for 90 days, and hourly and daily summaries are kept after that. Missing readings remain gaps; percentages and balances are not added or averaged."
    } else {
        "Quota observations are retained for up to 30 days, with older readings downsampled hourly. Missing readings remain gaps; percentages and balances are not added or averaged."
    };
    Ok(json!({
        "schemaVersion": 1,
        "updatedAt": iso(ctx, now)?,
        "range": range_out,
        "filters": {
            "platform": req(query.get("platform"))?,
            "provider": provider_filter,
            "account": account_filter,
        },
        "history": {
            "status": if available_history { "ok" } else { "unavailable" },
            "retentionDays": if stored { STORE_RETENTION_DAYS } else { 30 },
            "collectedSince": collected_since,
            "oldestSampleAt": stamps.first(),
            "newestSampleAt": stamps.last(),
            "sampleCount": sample_count,
            "message": message,
        },
        "summary": {
            "accountCount": accounts.len(),
            "availableAccounts": accounts.iter().filter(|a| available(&a.value)).count(),
            "activeCodexAccountId": active("codex"),
            "activeAccountIds": active_ids,
            "sampleCount": sample_count,
        },
        "providers": providers,
        "accounts": series,
    }))
}
