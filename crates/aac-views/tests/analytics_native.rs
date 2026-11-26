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
            "sessions": { "sample": [], "signatures": [], "total": 0, "truncated": false },
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

fn input(tz: &str) -> NativeInput {
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
        pricing: json!({}),
        table: table(),
    }
}

#[test]
fn the_native_input_composes_a_complete_answer() {
    let answer = analytics_owned::compose_native(input("UTC"), &mut EmptyStore, |_, _| None)
        .expect("composed");
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
    // No CLI activity in an empty store: the projection's fixed unavailable block, not an
    // error — the store is read and the answer is complete.
    assert_eq!(answer["activity"]["status"], json!("unavailable"));
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
    let failed =
        analytics_owned::compose_native(input("America/New_York"), &mut EmptyStore, |_, _| None);
    assert_eq!(failed, Err(Decline::Zone));
}

// --- Collation hotfix: a non-ASCII activity label in an old (>30d) bucket ---

const OLD_BUCKET: u64 = NOW - 40 * DAY; // outside the 7d/30d windows, inside 90d and all
const RECENT_BUCKET: u64 = NOW - 2 * DAY; // inside the 7d window

/// A store with one activity bucket holding a row per model. `costs` sets each row's logged cost in
/// nano-dollars: equal costs force the projection's ranking tie-breaks onto the name/key collation,
/// unequal costs leave the ranking on cost alone. The bucket's age selects which ranges see it.
struct ActivityStore {
    bucket: u64,
    models: Vec<&'static str>,
    costs: Vec<u64>,
}

impl analytics_owned::AnalyticsStore for ActivityStore {
    fn activity_oldest(&mut self) -> Result<Option<u64>, Decline> {
        Ok(Some(NOW - 100 * DAY))
    }
    fn quota_oldest(&mut self) -> Result<Option<u64>, Decline> {
        Ok(Some(NOW - 100 * DAY))
    }
    fn activity_range(&mut self, _query: Value) -> Result<Value, Decline> {
        let rows: Vec<Value> = self
            .models
            .iter()
            .enumerate()
            .map(|(index, _)| {
                json!([
                    self.bucket,
                    0,
                    0,
                    0,
                    index,
                    1000,
                    500,
                    0,
                    0,
                    Value::Null,
                    self.costs[index],
                    0,
                    1,
                    0
                ])
            })
            .collect();
        Ok(json!({
            "tools": ["claude"],
            "routes": ["anthropic"],
            "models": self.models,
            "rows": rows,
            "sessions": { "sample": [], "signatures": [], "total": 0, "truncated": false },
            "quality": [],
        }))
    }
    fn activity_sources(&mut self, _from_ms: u64, _to_ms: u64) -> Result<Value, Decline> {
        Ok(json!({
            "computers": [{
                "computer": "ubuntu", "oldestMs": NOW - 100 * DAY, "newestMs": NOW,
                "lastReadMs": NOW, "gaps": [],
            }],
            "sources": [],
        }))
    }
    fn history_records(&mut self, _query: Value) -> Result<Value, Decline> {
        Ok(json!([]))
    }
}

fn fixture_input(range: &str, models: &[&str]) -> NativeInput {
    let mut native = input("UTC");
    native.query.insert("range".into(), json!(range));
    // A pricing key per model on the anthropic route: null is a missing fact (no rate), which the
    // projection accepts; an absent key would decline Unsupported before the collation sort runs.
    let mut pricing = Map::new();
    for model in models {
        pricing.insert(format!("{model}\0anthropic"), Value::Null);
    }
    native.pricing = Value::Object(pricing);
    native
}

#[test]
fn a_non_ascii_activity_label_answers_90d_and_all() {
    // On 7d41daa85 the projection sort declined Collation on the non-ASCII model name, turning the
    // 90d and all reads into 500s. The total collation answers them. The old bucket sits outside the
    // 7d window, so the 7d answer never sees the label and is untouched.
    for range in ["90d", "all"] {
        let mut store = ActivityStore {
            bucket: OLD_BUCKET,
            models: vec!["ascii-model", "café"],
            costs: vec![1_000_000_000, 1_000_000_000],
        };
        let answer = analytics_owned::compose_native(
            fixture_input(range, &store.models),
            &mut store,
            |_, _| None,
        )
        .unwrap_or_else(|err| panic!("{range} declined: {err:?}"));
        assert_eq!(answer["range"]["preset"], json!(range));
        let activity =
            String::from_utf8(js_json::write(&answer["activity"])).expect("utf-8 activity");
        assert!(
            activity.contains("café"),
            "{range} answer keeps the non-ASCII model"
        );
    }
}

#[test]
fn the_7d_ascii_activity_bytes_are_unchanged() {
    // Parity guard: an all-ASCII 7d fixture must compose to the exact bytes the ASCII-only collation
    // produced on 7d41daa85 (captured below), so the total collation reorders nothing ASCII.
    let mut store = ActivityStore {
        bucket: RECENT_BUCKET,
        models: vec!["ascii-model", "gpt-4o"],
        costs: vec![1_000_000_000, 2_000_000_000],
    };
    let answer =
        analytics_owned::compose_native(fixture_input("7d", &store.models), &mut store, |_, _| {
            None
        })
        .expect("7d composes");
    let text = String::from_utf8(js_json::write(&answer["activity"])).expect("utf-8 activity");
    assert_eq!(
        text, GOLDEN_7D_ACTIVITY,
        "the 7d ASCII activity bytes changed"
    );
}

const GOLDEN_7D_ACTIVITY: &str = r#"{"status":"ok","refreshing":false,"refreshingRemote":[],"scope":"multi-host-cli","timezone":"UTC","accountAttribution":"unavailable","costBasis":"estimated-api-equivalent","fetchedAt":"2026-11-19T00:00:00.000Z","message":"CLI activity kept in AAC's history (UTC hourly buckets; days are grouped in UTC). Cost is an API-equivalent estimate, not a subscription charge.","totals":{"inputTokens":2000,"outputTokens":1000,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":3,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false},"providers":[{"provider":"claude","label":"Claude","totals":{"inputTokens":2000,"outputTokens":1000,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":3,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false},"usageEvents":2,"sessionCount":0,"tools":["claude"]}],"byDay":[{"date":"2026-11-17","provider":"claude","inputTokens":2000,"outputTokens":1000,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":3,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false,"requestCount":2}],"byHour":[{"hour":"2026-11-17T00:00:00Z","provider":"claude","inputTokens":2000,"outputTokens":1000,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":3,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false,"requestCount":2}],"models":[{"model":"gpt-4o","provider":"claude","inputTokens":1000,"outputTokens":500,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":2,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false,"rates":null,"tools":["claude"]},{"model":"ascii-model","provider":"claude","inputTokens":1000,"outputTokens":500,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":1,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false,"rates":null,"tools":["claude"]}],"byDayModel":[{"date":"2026-11-17","provider":"claude","model":"gpt-4o","inputTokens":1000,"outputTokens":500,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":2,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false},{"date":"2026-11-17","provider":"claude","model":"ascii-model","inputTokens":1000,"outputTokens":500,"cacheCreationTokens":0,"cacheReadTokens":0,"estimatedCostUsd":1,"fallbackCostUsd":0,"costByType":null,"costByTypeReconciled":false}],"sessions":{"total":0,"sample":[],"truncated":false},"anomalies":{"thresholds":{"costSpikeMultiplier":2,"highInputTokens":10000000,"highIoRatio":100,"highCacheReadTokens":1000000000},"items":[],"summary":{"totalAnomalies":0,"highInputDays":0,"highIoRatioDays":0,"costSpikeDays":0,"highCacheReadDays":0}},"sources":[],"history":{"computers":[{"host":"ubuntu","oldestAt":"2026-08-11T00:00:00.000Z","newestAt":"2026-11-19T00:00:00.000Z","lastReadAt":"2026-11-19T00:00:00.000Z","gaps":[]}],"sources":[],"backfill":{"running":false,"sources":[]},"quality":[]},"resolution":"hour"}"#;

#[test]
fn analytics_deep_all_keeps_one_bound_when_older_activity_arrives() {
    struct AdvancingStore {
        reads: usize,
    }
    impl analytics_owned::AnalyticsStore for AdvancingStore {
        fn activity_oldest(&mut self) -> Result<Option<u64>, Decline> {
            self.reads += 1;
            Ok(Some(NOW - if self.reads == 1 { 31 } else { 117 } * DAY))
        }
        fn quota_oldest(&mut self) -> Result<Option<u64>, Decline> {
            Ok(None)
        }
        fn activity_range(&mut self, query: Value) -> Result<Value, Decline> {
            EmptyStore.activity_range(query)
        }
        fn activity_sources(&mut self, from: u64, to: u64) -> Result<Value, Decline> {
            EmptyStore.activity_sources(from, to)
        }
        fn history_records(&mut self, query: Value) -> Result<Value, Decline> {
            EmptyStore.history_records(query)
        }
    }

    let mut store = AdvancingStore { reads: 0 };
    let zone_for = |oldest: f64, _: &str| {
        let from = oldest as i64 - DAY as i64;
        aac_num::intl::Zone::proven(from, (NOW + DAY) as i64, &[[from, -300]])
    };
    let mut args = input("America/New_York");
    args.query.insert("range".into(), json!("all"));
    let answer =
        analytics_owned::compose_native(args, &mut store, zone_for).expect("warm All composes");
    assert_eq!(answer["range"]["preset"], "all");
    assert_eq!(store.reads, 1);
}

// Fixed-clock quota fixtures pin the complete response bytes across the typed pipeline change.
struct QuotaStore(Value);
impl analytics_owned::AnalyticsStore for QuotaStore {
    fn activity_oldest(&mut self) -> Result<Option<u64>, Decline> {
        EmptyStore.activity_oldest()
    }
    fn quota_oldest(&mut self) -> Result<Option<u64>, Decline> {
        EmptyStore.quota_oldest()
    }
    fn activity_range(&mut self, query: Value) -> Result<Value, Decline> {
        EmptyStore.activity_range(query)
    }
    fn activity_sources(&mut self, from: u64, to: u64) -> Result<Value, Decline> {
        EmptyStore.activity_sources(from, to)
    }
    fn history_records(&mut self, _: Value) -> Result<Value, Decline> {
        Ok(self.0.clone())
    }
}

fn quota_fixture() -> Value {
    let stamp =
        |offset: u64| aac_num::ecma::to_iso_string((NOW - offset) as i64).expect("quota fixture");
    let rows: Vec<_> = (0..12).map(|i| json!({
        "identity":aac_core::AccountKey::derive(&aac_core::ProviderId::new("codex").expect("quota fixture"), "fixture", None).expect("quota fixture").as_str(), "accountId":"fixture", "provider":"codex",
        "source":"fixture", "platform":"mac", "status":"ok", "isActive":true,
        "sampledAt":stamp(DAY - i * 300_000), "observedAt":stamp(DAY - i * 300_000),
        "windows": if i == 4 { json!([]) } else { json!([
            {"key":"weekly", "unit":"percent", "label":"Weekly é", "usedPercent":i,
             "status":if i == 5 { "cached" } else { "ok" },
             "sampledAt":stamp(DAY + if i == 5 { 300_000 } else { 0 })},
            {"key":"weekly", "unit":"percent", "label":"Duplicate", "usedPercent":99},
            {"key":"pool", "unit":"tokens", "poolId":"shared", "used": i * 10}
        ]) }
    })).collect();
    json!(rows)
}

#[test]
fn quota_pipeline_sha256_parity() {
    use sha2::{Digest, Sha256};
    for historical in [false, true] {
        for preset in ["24h", "7d", "30d", "90d", "all"] {
            let mut native = input("UTC");
            native.query.insert("range".into(), json!(preset));
            native.dashboard.insert("accounts".into(), if historical { Value::Null } else {
                json!([[ ["provider","codex"], ["id","fixture"], ["status","ok"],
                    ["windows", [{"key":"weekly", "unit":"percent", "label":"Weekly", "usedPercent":50}]] ]])
            });
            let answer = analytics_owned::compose_native(
                native,
                &mut QuotaStore(quota_fixture()),
                |_, _| None,
            )
            .expect("quota fixture");
            let bytes = js_json::write(&answer);
            let mut plain = js_json::Writer::plain(1024);
            plain.value(&answer);
            assert_eq!(plain.into_bytes(), bytes);
            let hash: String = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            let expected = match (historical, preset) {
                (false, "24h") => {
                    "f815403298f00ed10c92c60d905c52ba98d56e300833c75ca38c54e3895c9dc2"
                }
                (false, "7d") => "3535ef3cb83fcd2b4c3d8b0ebdbc69849e6dfce745420f066489991c80af1e1d",
                (false, "30d") => {
                    "de27ab38a6afdf87dd70536d2f40d975cbd91af707cb2d06a72863236784ff18"
                }
                (false, "90d") => {
                    "e3d23ac1968df77b0f55a362e5808f5407ba85650774798bdf002f4c8f395f7d"
                }
                (false, "all") => {
                    "ec24d0cbb2ccab543022a5a10e41cd24acaee289c63b1bd999a8be130a205943"
                }
                (true, "24h") => "043b32859553ac29925c57148692d2dcdcdd55c7f7a4aef4fc1f04251a9fcda2",
                (true, "7d") => "0192e901699ea38d9215aa7e6363a59fb49a8a044751cb8dc1fbc9b9b42b01f8",
                (true, "30d") => "c3055124baf7cbe352555919592c27d112fe320569c1d8bd8aeb038f09d43be5",
                (true, "90d") => "892e346bcc1fc65fe5b304030266e278804a6a971634f3889ff9c4fa40f9ae50",
                (true, "all") => "f7d1ee21c9b0c821d292a71a86063ac59b6473d7cc3e0d50a29f6e28e5008e30",
                _ => unreachable!(),
            };
            assert_eq!(hash, expected, "{historical} {preset}");
            assert_eq!(
                answer["accounts"].as_array().expect("quota fixture").len(),
                1
            );
            assert_eq!(answer["accounts"][0]["sampleCount"], 12);
        }
    }
}
