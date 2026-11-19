//! The native Analytics entry (`compose_native`): the live service's own inputs, no envelope.
//! The store is an empty fixture whose one computer read just happened, so the answer must be a
//! complete composed body — the range it resolved, the activity block and the provider table —
//! never a decline. This is the wiring the route depends on; the full byte parity is the M3
//! table and the recorded M0 contracts.
use aac_views::analytics_owned::{self, NativeInput};
use aac_views::decline::Decline;
use aac_views::js_json;
use serde_json::{Map, Value, json};

const NOW: u64 = 1_795_046_400_000; // 2026-11-19T00:00:00Z
const DAY: u64 = 86_400_000;

struct EmptyStore;

impl analytics_owned::AnalyticsStore for EmptyStore {
    fn activity_oldest(&mut self) -> Result<Option<u64>, Decline> {
        Ok(Some(NOW - 2 * DAY))
    }
    fn quota_oldest(&mut self) -> Result<Option<u64>, Decline> {
        Ok(Some(NOW - 3 * DAY))
    }
    fn activity_range(&mut self, _query: Value) -> Result<Value, Decline> {
        Ok(json!({
            "tools": [], "routes": [], "models": [], "rows": [],
            "sessions": { "signatures": [], "total": 0, "truncated": false },
            "quality": [],
        }))
    }
    fn activity_sources(&mut self, _from_ms: u64, _to_ms: u64) -> Result<Value, Decline> {
        Ok(json!({
            "computers": [{
                "computer": "ubuntu", "oldestMs": NOW - 2 * DAY, "newestMs": NOW,
                "lastReadMs": NOW, "gaps": [],
            }],
            "sources": [],
        }))
    }
    fn history_records(&mut self, _query: Value) -> Result<Value, Decline> {
        Ok(json!([]))
    }
}

fn query() -> Map<String, Value> {
    let mut query = Map::new();
    query.insert("platform".into(), json!("mac"));
    query.insert("range".into(), json!("7d"));
    query.insert("provider".into(), json!("all"));
    query.insert("account".into(), json!("all"));
    query.insert("tz".into(), json!("UTC"));
    query
}

fn table() -> Vec<Value> {
    aac_providers::analytics_provider_table()
        .into_iter()
        .map(|(id, label, order)| json!({ "id": id, "label": label, "order": order }))
        .collect()
}

fn input(tz: &str, zone: Option<aac_num::intl::Zone>) -> NativeInput {
    let mut query = query();
    query.insert("tz".into(), json!(tz));
    NativeInput {
        query,
        now: NOW as f64,
        tz: tz.to_owned(),
        dashboard: json!({ "accounts": [], "providers": [], "settings": {} })
            .as_object()
            .cloned()
            .expect("object"),
        live_sources: json!([]),
        refreshing: false,
        zone,
        pricing: json!({}),
        table: table(),
    }
}

#[test]
fn the_native_input_composes_a_complete_answer() {
    let answer =
        analytics_owned::compose_native(input("UTC", None), &mut EmptyStore).expect("composed");
    let mut expected_keys = vec![
        "schemaVersion",
        "updatedAt",
        "range",
        "filters",
        "history",
        "summary",
        "providers",
        "accounts",
        "activity",
    ];
    let keys: Vec<&str> = answer
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect();
    expected_keys.sort_unstable();
    let mut keys_sorted = keys.clone();
    keys_sorted.sort_unstable();
    assert_eq!(keys_sorted, expected_keys);
    assert_eq!(answer["schemaVersion"], json!(1));
    assert_eq!(answer["range"]["preset"], json!("7d"));
    assert_eq!(answer["range"]["tz"], json!("UTC"));
    assert_eq!(answer["activity"]["status"], json!("ok"));
    assert!(
        answer["activity"]["sources"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );
    assert!(answer["accounts"].as_array().is_some_and(Vec::is_empty));
    // The whole body is JSON text the HTTP layer can send unchanged.
    assert!(js_json::write(&answer).starts_with(b"{\"schemaVersion\":1,"));
}

#[test]
fn a_non_utc_zone_without_proven_offsets_declines() {
    let failed = analytics_owned::compose_native(input("America/New_York", None), &mut EmptyStore);
    assert_eq!(failed, Err(Decline::Zone));
}
