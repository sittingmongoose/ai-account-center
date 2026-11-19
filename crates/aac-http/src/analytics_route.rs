//! R5/A2 live: the native Analytics read. The public front door forwards the admitted query; the
//! service builds the whole owned envelope itself — the platform's dashboard from the same
//! generation Accounts and Home render, the activity collector's live flag, the display zone's
//! offsets from the host zoneinfo, the pricing facts over the store dictionary and the provider
//! table — then composes through [`aac_views::analytics_owned::compose_native`]. Every input is an
//! existing owner's answer: no second collector, cache or table.
use crate::account_reads::EngineStore;
use crate::feed::Feeds;
use aac_store::engine::Engine;
use aac_views::decline::Decline;
use serde_json::{Map, Value, json};
use std::path::Path;
use std::sync::Arc;

/// The fed analytics dashboards: `{dashboards: [{platform, dashboard}, ...]}` (older feeds carry
/// bare dashboards in mac-then-windows order; a bare single dashboard cannot name its platform).
const DASHBOARD: &str = "analytics-dashboard";
/// The display zone's default when no preference is saved (`DEFAULT_DASHBOARD_TIME_ZONE`).
const DEFAULT_ZONE: &str = "America/New_York";
const DAY_MS: f64 = 86_400_000.0;
const RETAINED_MS: f64 = 31.0 * DAY_MS;

/// The free answer: the whole body, or a named decline.
pub(crate) type Answer = Result<Value, Decline>;

/// One parsed route query with the service defaults, in the route's own object order.
fn query_object(raw: Option<&str>) -> Result<Map<String, Value>, Decline> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (key, value) in url::form_urlencoded::parse(raw.unwrap_or("").as_bytes()) {
        let key = key.into_owned();
        if pairs.iter().any(|(seen, _)| *seen == key) {
            return Err(Decline::Shape);
        }
        pairs.push((key, value.into_owned()));
    }
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.clone())
    };
    let mut query = Map::new();
    query.insert(
        "platform".into(),
        json!(get("platform").unwrap_or("mac".into())),
    );
    query.insert("range".into(), json!(get("range").unwrap_or("7d".into())));
    query.insert(
        "provider".into(),
        json!(get("provider").unwrap_or("all".into())),
    );
    query.insert(
        "account".into(),
        json!(get("account").unwrap_or("all".into())),
    );
    if let Some(refresh) = get("refresh") {
        query.insert("refresh".into(), json!(refresh == "true"));
    }
    // The route's literal order, then the service's `{...requested, tz}`: a tz the request
    // named keeps its place; a resolved default is appended, exactly as the spread does.
    for key in ["tz", "from", "to"] {
        if let Some(value) = get(key) {
            query.insert(key.into(), json!(value));
        }
    }
    Ok(query)
}

/// The saved display zone (`readDashboardPreferences(scope).timeZone`), default America/New_York.
fn saved_zone(root: &Path) -> String {
    crate::account_reads::preferences(root)
        .and_then(|value| {
            value
                .get("timeZone")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| DEFAULT_ZONE.to_owned())
}

/// `rustAnalyticsOwnedDashboard`: the dashboard's accounts as ordered entry lists.
fn dashboard_entries(dashboard: &Value) -> Result<Map<String, Value>, Decline> {
    let mut out = Map::new();
    let accounts = dashboard
        .get("accounts")
        .and_then(Value::as_array)
        .ok_or(Decline::Shape)?;
    let mut rows = Vec::with_capacity(accounts.len());
    for account in accounts {
        let account = account.as_object().ok_or(Decline::Shape)?;
        let provider = account.get("provider").and_then(Value::as_str);
        let id = account.get("id").and_then(Value::as_str);
        let email_ok = match account.get("email") {
            None | Some(Value::Null) => true,
            Some(Value::String(_)) => true,
            Some(_) => false,
        };
        if provider.is_none() || id.is_none() || !email_ok {
            return Err(Decline::Shape);
        }
        let Some(windows) = account.get("windows").and_then(Value::as_array) else {
            return Err(Decline::Shape);
        };
        let mut entries: Vec<Value> = Vec::with_capacity(account.len());
        for (key, value) in account {
            if key == "windows" {
                let mut lists = Vec::with_capacity(windows.len());
                for window in windows {
                    let window = window.as_object().ok_or(Decline::Shape)?;
                    lists.push(Value::Array(
                        window
                            .iter()
                            .map(|(key, value)| json!([key, value]))
                            .collect(),
                    ));
                }
                entries.push(json!(["windows", lists]));
            } else {
                entries.push(json!([key, value]));
            }
        }
        rows.push(Value::Array(entries));
    }
    out.insert("accounts".into(), Value::Array(rows));
    for key in ["providers", "settings"] {
        if let Some(value) = dashboard.get(key) {
            out.insert(key.into(), value.clone());
        }
    }
    Ok(out)
}

/// The platform a fed dashboard entry names, when it carries the newer labeled form.
fn platform_label(entry: &Value) -> Option<&str> {
    entry.get("platform").and_then(Value::as_str)
}

/// The platform's dashboard from the fed generation.
fn dashboard_of(feeds: &Feeds, platform: &str) -> Result<Value, Decline> {
    let Some(generation) = feeds.latest(DASHBOARD).ok() else {
        return Ok(json!({ "accounts": [] }));
    };
    let doc: Value = serde_json::from_str(generation.data.get()).map_err(|_| Decline::Shape)?;
    let list = doc
        .get("dashboards")
        .and_then(Value::as_array)
        .ok_or(Decline::Shape)?;
    let found = if list.iter().any(|entry| platform_label(entry).is_some()) {
        list.iter()
            .find(|entry| platform_label(entry) == Some(platform))
            .and_then(|entry| entry.get("dashboard"))
            .cloned()
    } else {
        // The older bare form: mac then windows, both present or neither proven.
        match (list.len(), platform) {
            (2, "mac") => list.first().cloned(),
            (2, _) => list.get(1).cloned(),
            _ => None,
        }
    };
    Ok(found.unwrap_or_else(|| json!({ "accounts": [] })))
}

/// The native Analytics answer for one admitted query.
#[allow(clippy::cast_precision_loss)]
pub(crate) fn compose(
    engine: Arc<Engine>,
    root: &Path,
    feeds: &Feeds,
    live: &crate::analytics_live::Live,
    raw_query: Option<&str>,
    now: i64,
) -> Answer {
    let mut query = query_object(raw_query)?;
    // The page's Refresh: the writers' next tick collects activity at once, and this answer
    // already says it is refreshing (the page polls while it runs).
    let refresh = query.get("refresh").and_then(Value::as_bool) == Some(true);
    if refresh {
        live.request_refresh();
    }
    let scanning = refresh || live.scanning();
    let platform = query
        .get("platform")
        .and_then(Value::as_str)
        .ok_or(Decline::Shape)?
        .to_owned();
    if !matches!(platform.as_str(), "mac" | "windows") {
        return Err(Decline::Shape);
    }
    // The route's tz is the request's or the saved display zone (`canonicalAccountAnalyticsTimeZone`).
    let tz = match query.get("tz").and_then(Value::as_str) {
        Some(tz) => tz.to_owned(),
        None => saved_zone(root),
    };
    if tz != "UTC" && !aac_files::zoneinfo::known(&tz) {
        return Err(Decline::Zone);
    }
    // The query object the answer carries: the route object, with the resolved tz in its place.
    query.insert("tz".into(), json!(tz.clone()));

    let dashboard = dashboard_of(feeds, &platform)?;
    let dashboard = dashboard_entries(&dashboard)?;
    #[allow(clippy::cast_sign_loss)]
    let clock_ms = now.max(0) as u64;
    let now = now as f64;
    let store_engine = Arc::clone(&engine);
    let with_store = |store: &mut EngineStore| {
        let oldest = aac_views::analytics_owned::oldest_bound(store, now);
        // The proven zone window: floor(oldest/DAY)*DAY - DAY .. floor(now/DAY)*DAY + 2*DAY.
        #[allow(clippy::cast_possible_truncation)]
        let (window_from, window_now) = (oldest.min(now - RETAINED_MS).max(0.0) as i64, now as i64);
        let zone = (tz != "UTC")
            .then(|| aac_files::zoneinfo::zone_for_window(&tz, window_from, window_now))
            .flatten();
        let models = aac_store::activity::models(&store_engine).unwrap_or_else(|_| json!({}));
        let names = |key: &str| -> Vec<String> {
            models
                .get(key)
                .and_then(Value::as_array)
                .map(|rows| {
                    rows.iter()
                        .filter_map(|row| row.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default()
        };
        let mut resolver = aac_views::pricing::Resolver::new(root);
        let pricing = resolver.facts_table(&names("models"), &names("routes"));
        let table = aac_providers::analytics_provider_table()
            .into_iter()
            .map(|(id, label, order)| json!({ "id": id, "label": label, "order": order }))
            .collect();
        aac_views::analytics_owned::compose_native(
            aac_views::analytics_owned::NativeInput {
                query,
                now,
                tz,
                dashboard,
                live_sources: Value::Array(Vec::new()),
                refreshing: scanning,
                zone,
                pricing: Value::Object(pricing),
                table,
            },
            store,
        )
    };
    let mut store = EngineStore::with_clock(engine, clock_ms);
    with_store(&mut store)
}
