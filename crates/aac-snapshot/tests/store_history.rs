//! MEM5 B #2: the snapshot reads the store's 7-day quota history straight into typed records
//! (`QuotaHistory::from_store`) instead of the `historyRecords` JSON tree. On a store filled with
//! a week of live-shaped samples (resets, cached windows, pools, amount windows, a gap), and on
//! the TS sampler vectors, the typed history equals the JSON answer read back, and the published
//! usage.json and history.json are the same bytes either way.

use aac_snapshot::history::{QuotaHistory, RecentReadings};
use aac_snapshot::js::{stringify, stringify_pretty};
use aac_snapshot::snapshot::{SampleEvent, build_snapshot_files};
use aac_store::engine::Engine;
use aac_store::usage::{HistoryQuery, Thinning};
use aac_store::{ops, sampler};
use serde_json::{Value, json};

const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;
const DAY: i64 = 24 * HOUR;
/// 2026-11-20T12:00:00.000Z.
const NOW: i64 = 1_795_176_000_000;

fn iso(ms: i64) -> String {
    aac_num::ecma::to_iso_string(ms).expect("iso")
}

/// The dashboard rows of one sample at `at`: a rising 5-hour window that resets, a weekly
/// window, a pooled window, an amount window and an account that turns cached.
fn rows(at: i64) -> Vec<Value> {
    let step = (at - (NOW - 8 * DAY)) / MINUTE;
    let five_reset = (at / (5 * HOUR) + 1) * 5 * HOUR;
    let five_used = ((at % (5 * HOUR)) as f64 / (5 * HOUR) as f64 * 90.0).floor() + 0.5;
    let week_reset = (at / (7 * DAY) + 1) * 7 * DAY;
    let week_used = ((at % (7 * DAY)) as f64 / (7 * DAY) as f64 * 100.0 * 10.0).round() / 10.0;
    let mut alpha_windows = vec![
        json!({"key": "five_hour", "label": "5-hour limit", "kind": "rate_limit",
               "usedPercent": five_used, "resetAt": iso(five_reset), "windowMinutes": 300}),
        json!({"key": "weekly", "label": "Weekly", "kind": "rate_limit",
               "usedPercent": week_used, "resetAt": iso(week_reset), "windowMinutes": 10080}),
    ];
    if step % 3 != 0 {
        alpha_windows.push(json!({
            "key": "weekly", "label": "Weekly (Opus)", "kind": "rate_limit", "poolId": "opus",
            "poolLabel": "Opus", "usedPercent": (week_used / 2.0).floor(),
            "resetAt": iso(week_reset)
        }));
    }
    // A cached account row keeps its own older time; a retained window inside it an older one
    // still (`observedAt` takes the window's own time).
    let cached = step % 50 < 10;
    let beta_sampled = if cached { at - 3 * MINUTE } else { at };
    let window_sampled = at - 7 * MINUTE;
    let mut beta_window = json!({
        "key": "credits", "label": "Credits", "kind": "balance", "unit": "usd",
        "remaining": 50.0 - (step % 400) as f64 * 0.1, "limit": 50.0,
        "resetAt": Value::Null
    });
    if cached {
        beta_window["status"] = json!("cached");
        beta_window["sampledAt"] = json!(iso(window_sampled));
    }
    let mut out = vec![json!({
        "id": "alpha", "provider": "claude", "providerLabel": "Claude",
        "email": "Alpha@Example.test", "label": "Alpha",
        "status": "ok", "source": "cli", "host": "ubuntu", "platform": "ubuntu",
        "isActive": true, "sampledAt": iso(at), "fetchedAt": iso(at), "windows": alpha_windows
    })];
    // A gap of six hours two days back: no beta readings at all.
    if !(NOW - 2 * DAY..NOW - 2 * DAY + 6 * HOUR).contains(&at) {
        out.push(json!({
            "id": "beta", "provider": "opencode-go", "providerLabel": "OpenCode Go",
            "email": "beta@example.test", "label": "Beta",
            "status": if cached { "cached" } else { "ok" }, "source": "api", "host": "ubuntu",
            "platform": "ubuntu", "isActive": false, "sampledAt": iso(beta_sampled),
            "fetchedAt": iso(at), "windows": [beta_window]
        }));
    }
    out
}

fn temp_engine(name: &str) -> (tempfile::TempDir, Engine) {
    let dir = tempfile::Builder::new()
        .prefix(name)
        .tempdir()
        .expect("temp dir");
    let engine = ops::open_engine(dir.path().to_str().expect("utf-8 path"), None, None)
        .expect("open engine");
    (dir, engine)
}

fn query(now: i64) -> HistoryQuery {
    HistoryQuery {
        from_ms: (now - 7 * DAY).max(0) as u64,
        to_ms: now as u64,
        now_ms: now as u64,
        thinning: Thinning::History,
        identities: None,
    }
}

/// The JSON answer the snapshot used to read, as the value it used to keep.
fn json_history(engine: &Engine, now: i64) -> Value {
    let records = ops::call(
        engine,
        "historyRecords",
        &json!({
            "fromMs": (now - 7 * DAY).max(0),
            "toMs": now,
            "nowMs": now,
            "thinning": { "kind": "history" },
        })
        .to_string(),
    )
    .expect("historyRecords");
    assert!(
        records.as_array().is_some_and(|list| !list.is_empty()),
        "the store holds records"
    );
    json!({ "schemaVersion": 1, "records": records })
}

fn published(event: &SampleEvent, recent: &RecentReadings) -> (String, String) {
    let (snapshot, history) = build_snapshot_files(event, 60.0, Some(recent), &[], None);
    let text = history.to_json();
    assert_eq!(text, stringify(&history.value()), "streamed history text");
    (stringify_pretty(&snapshot), text)
}

fn assert_same(engine: &Engine, accounts: Vec<Value>, now: i64, recent: &RecentReadings) {
    let typed = QuotaHistory::from_store(engine, &query(now)).expect("typed history");
    let value = QuotaHistory::from_value(&json_history(engine, now));
    assert_eq!(
        typed, value,
        "typed history equals the JSON answer read back"
    );
    let event = |history| SampleEvent {
        accounts: accounts.clone(),
        history: Some(history),
        sampled_at: now,
        long_burn: None,
    };
    let (typed_json, typed_history) = published(&event(typed), recent);
    let (value_json, value_history) = published(&event(value), recent);
    assert!(
        typed_history.matches("\"points\"").count() >= 2,
        "series published"
    );
    assert_eq!(typed_json, value_json, "usage.json");
    assert_eq!(typed_history, value_history, "history.json");
}

#[test]
fn typed_store_history_matches_the_json_answer_over_a_week() {
    let (_dir, engine) = temp_engine("mem5-history");
    let mut recent = RecentReadings::new();
    let mut at = NOW - 8 * DAY;
    let mut last = Vec::new();
    while at <= NOW {
        let sample = rows(at);
        sampler::record_sample(&engine, &sample, at).expect("record sample");
        if at >= NOW - HOUR {
            recent.add(&sample, at);
        }
        last = sample;
        // Every 7 minutes for the week, every minute for the last hour.
        at += if at >= NOW - HOUR { MINUTE } else { 7 * MINUTE };
    }
    assert_same(&engine, last.clone(), NOW, &recent);
    // Mid-history clocks: the thinning tiers fall on other records.
    for back in [37 * MINUTE, 5 * HOUR + 3 * MINUTE, 3 * DAY + 11 * MINUTE] {
        let now = NOW - back;
        assert_same(&engine, rows(now), now, &RecentReadings::new());
    }
    // Without the ring: the store's history alone.
    assert_same(&engine, last, NOW, &RecentReadings::new());
}

#[test]
fn typed_store_history_matches_the_json_answer_on_the_sampler_vectors() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../tests/spec/analytics-writers/sampler-vectors.json"
    );
    let doc: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("sampler vectors"))
        .expect("vectors parse");
    let now = doc["now"].as_i64().expect("now");
    let (_dir, engine) = temp_engine("mem5-vectors");
    let merged = sampler::merge_accounts(doc["dashboards"].as_array().expect("dashboards"));
    let sources: Vec<sampler::BackfillSource> = doc["sources"]
        .as_array()
        .expect("sources")
        .iter()
        .map(|source| {
            let text = source["text"].as_str().expect("source text");
            let (records, parsed) = sampler::records_from_text(text);
            assert!(parsed, "source parsed");
            sampler::BackfillSource {
                label: source["label"].as_str().expect("label").to_string(),
                sha256: source["sha256"].as_str().expect("sha").to_string(),
                records,
            }
        })
        .collect();
    let providers = sampler::providers_of(&merged);
    sampler::backfill(&engine, &sources, &providers, now, 0).expect("backfill");
    sampler::record_sample(&engine, &merged, now).expect("record live");
    // The projection names each account: the vectors' rows carry only what the store keeps.
    let named: Vec<Value> = merged
        .iter()
        .map(|account| {
            let mut account = account.clone();
            account["label"] = account["id"].clone();
            account["providerLabel"] = account["provider"].clone();
            account["platform"] = json!("mac");
            account
        })
        .collect();
    let later = now + 5 * MINUTE;
    assert_same(&engine, named, later, &RecentReadings::new());
}
