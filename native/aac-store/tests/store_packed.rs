//! The packed day values (docs/store.md 3.1, 11.4): day-value queries against a full scan,
//! retention across a day value, crash cuts over the projection, late readings, the rebuild from
//! projector version 1, compaction and the size budget.

mod common;

use aac_store::db;
use aac_store::engine::Engine;
use aac_store::ops;
use common::{copy_dir, find, temp_dir};
use redb::{ReadableDatabase, ReadableTable};
use serde_json::{Value as Json, json};
use std::fs;
use std::path::Path;

const MIN: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
/// Midnight UTC, so fixtures control which days they cross.
const MIDNIGHT: i64 = 1_793_000_000_000 - 1_793_000_000_000 % DAY;

fn identity(n: u32) -> String {
    format!("{n:064x}")
}

fn window(key: &str, used: f64, reset: i64) -> Json {
    json!({
        "identity": format!("[\"{key}\",\"rate_limit\",null]"),
        "key": key, "label": key, "kind": "rate_limit", "unit": null,
        "used_pct": used, "remaining_pct": 100.0 - used, "reset_at_ms": reset, "window_minutes": 300.0,
    })
}

fn obs(account: u32, t: i64, windows: Vec<Json>) -> Json {
    json!({
        "account": {
            "identity": identity(account), "provider": "claude", "account_id": format!("claude:a{account}"),
            "plan": "max", "platform": "ubuntu", "source": "test source", "status": "ok", "is_active": false,
        },
        "sampled_at_ms": t, "observed_at_ms": t, "windows": windows,
    })
}

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, Some(90)).unwrap()
}

fn call(engine: &Engine, method: &str, args: Json) -> Json {
    ops::call(engine, method, &args.to_string()).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn record(engine: &Engine, observations: Vec<Json>) -> Json {
    call(engine, "record", json!({ "observations": observations }))
}

/// Every row of the usage projection.
fn dump(engine: &Engine) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    engine
        .with_db(|database| {
            let read = database.begin_read()?;
            let mut out = Vec::new();
            for (name, def) in [
                ("account", db::ACCOUNT),
                ("window", db::WINDOW),
                ("series", db::SERIES),
                ("reading_day", db::READING_DAY),
                ("latest", db::LATEST),
                ("account_latest", db::ACCOUNT_LATEST),
                ("account_state", db::ACCOUNT_STATE),
                ("period", db::PERIOD),
                ("rollup_hours", db::ROLLUP_HOURS),
                ("rollup_day", db::ROLLUP_DAY),
            ] {
                for item in read.open_table(def)?.iter()? {
                    let (k, v) = item?;
                    out.push((name.to_string(), k.value().to_vec(), v.value().to_vec()));
                }
            }
            Ok(out)
        })
        .unwrap()
}

/// Raw points of one series as (start, last seen, used percent).
fn raw(engine: &Engine, key: &str, from: i64, to: i64) -> Vec<(i64, i64, f64)> {
    let answer = call(
        engine,
        "seriesPoints",
        json!({ "windowKey": key, "fromMs": from, "toMs": to, "resolution": "raw" }),
    );
    answer["series"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["points"].as_array().unwrap().iter())
        .map(|p| (p[0].as_i64().unwrap(), p[1].as_i64().unwrap(), p[2].as_f64().unwrap()))
        .collect()
}

/// Five days over midnights: changes every few minutes, a span lasting past two midnights, a
/// silent stretch that splits a span, and one window changing every minute on one day.
fn varied(start: i64) -> Vec<Json> {
    let mut out = Vec::new();
    let mut used = 3.0;
    for minute in 0..5 * 1440 {
        let t = start + minute * MIN;
        if (2880..3000).contains(&minute) {
            continue; // the dashboard was off for two hours
        }
        if minute % 11 == 0 && !(1000..4000).contains(&minute) {
            used = (used + 1.25) % 90.0;
        }
        let busy = if (1440..2880).contains(&minute) {
            (minute % 97) as f64 / 3.0
        } else {
            5.0
        };
        out.push(obs(
            1,
            t,
            vec![
                window("five_hour", used, start + 9 * DAY),
                window("minutes", busy, start + 9 * DAY),
            ],
        ));
    }
    out
}

/// The raw list is what the old per-reading table held; every ranged answer must match a filter
/// of it, wherever the range falls against day values.
#[test]
fn day_values_answer_like_a_scan_of_every_span() {
    let dir = temp_dir("packed-scan");
    let engine = open(&dir);
    let start = MIDNIGHT + 13 * HOUR;
    record(&engine, varied(start));
    let (lo, hi) = (start - 2 * DAY, start + 9 * DAY);
    for key in ["five_hour", "minutes"] {
        let all = raw(&engine, key, lo, hi);
        assert!(all.len() > 5, "{key}");
        assert!(all.windows(2).all(|w| w[0].0 < w[1].0), "{key} is ordered");
        assert!(all.windows(2).all(|w| w[0].1 <= w[1].0), "{key} spans do not overlap");
        // Ranges on, just before and just after span starts and midnights, plus a stride.
        let mut edges: Vec<i64> = all.iter().step_by(all.len() / 40 + 1).map(|p| p.0).collect();
        edges.extend((0..6).map(|d| MIDNIGHT + d * DAY));
        let mut ranges = Vec::new();
        for (i, &edge) in edges.iter().enumerate() {
            for delta in [-1, 0, 1] {
                let from = edge + delta;
                ranges.push((from, from + HOUR));
                ranges.push((from, from));
                ranges.push((from, edges[(i * 7 + 3) % edges.len()].max(from)));
            }
        }
        for (from, to) in ranges {
            let expected: Vec<_> = all.iter().filter(|p| p.0 >= from && p.0 <= to).cloned().collect();
            assert_eq!(raw(&engine, key, from, to), expected, "{key} {from}..{to}");
        }
        // "At time T": the last span starting at or before T, valid 10 minutes past its last
        // sighting.
        for &edge in &edges {
            for t in [edge - 1, edge, edge + 1, edge + 30 * MIN] {
                let expected = all
                    .iter()
                    .rev()
                    .find(|p| p.0 <= t)
                    .filter(|p| t <= p.1 + 10 * MIN)
                    .map(|p| p.0);
                let at = call(&engine, "usageAt", json!({ "t": t }));
                let series = at["series"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|s| s["window"]["key"] == key)
                    .unwrap();
                assert_eq!(series["reading"]["from"].as_i64(), expected, "{key} at {t}");
            }
        }
    }
    // Hourly rollups: any hour range is a filter of the whole list, and the day rollups agree.
    let hours = |from: i64, to: i64| {
        call(
            &engine,
            "seriesPoints",
            json!({ "windowKey": "minutes", "fromMs": from, "toMs": to, "resolution": "hour" }),
        )["series"][0]["points"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    };
    let all_hours = hours(lo, hi);
    assert!(all_hours.len() > 100);
    for (from, to) in [
        (start, start + HOUR),
        (start + 40 * MIN, start + 90 * MIN),
        (MIDNIGHT + DAY - 1, MIDNIGHT + DAY),
        (MIDNIGHT + DAY, MIDNIGHT + 2 * DAY - 1),
        (start, start + 3 * DAY),
    ] {
        let expected: Vec<Json> = all_hours
            .iter()
            .filter(|p| {
                let h = p[0].as_i64().unwrap();
                h >= from / HOUR * HOUR && h <= to / HOUR * HOUR
            })
            .cloned()
            .collect();
        assert_eq!(hours(from, to), expected, "{from}..{to}");
    }
    // A limit stops at the same point as a scan and says it did.
    let limited = call(
        &engine,
        "seriesPoints",
        json!({ "windowKey": "minutes", "fromMs": lo, "toMs": hi, "resolution": "raw", "limit": 7 }),
    );
    assert_eq!(limited["truncated"], true);
    assert_eq!(limited["series"][0]["points"].as_array().unwrap().len(), 7);
    let oldest = call(&engine, "oldest", json!({}));
    assert_eq!(oldest.as_i64(), Some(start));
    // Times past every key day clamp instead of wrapping.
    assert!(!raw(&engine, "minutes", lo, i64::MAX).is_empty());
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn retention_removes_expired_spans_inside_a_day_value() {
    let dir = temp_dir("packed-retention");
    let engine = open(&dir);
    let start = MIDNIGHT;
    let mut observations = Vec::new();
    for minute in 0..3 * 1440 {
        let t = start + minute * MIN;
        // A change every 7 minutes for two days, then a long span that lasts into day three.
        let used = if minute < 2 * 1440 {
            (minute / 7) as f64 % 100.0
        } else {
            50.0
        };
        observations.push(obs(1, t, vec![window("five_hour", used, start + 9 * DAY)]));
    }
    record(&engine, observations);
    let before = raw(&engine, "five_hour", start, start + 9 * DAY);
    let spans_before = call(&engine, "counts", json!({}))["reading"].as_u64().unwrap();
    assert_eq!(spans_before as usize, before.len());
    // A cutoff in the middle of day one: 90-day retention, measured from this "now".
    let now = start + 90 * DAY + 12 * HOUR + 3 * MIN;
    let cutoff = now - 90 * DAY;
    let answer = call(&engine, "maintain", json!({ "nowMs": now }));
    let kept = raw(&engine, "five_hour", start, start + 9 * DAY);
    let expected: Vec<_> = before.iter().filter(|p| p.1 >= cutoff).cloned().collect();
    assert_eq!(kept, expected);
    assert!(
        kept.len() < before.len() && kept[0].0 < cutoff,
        "the day value was split"
    );
    assert_eq!(
        answer["pruned"]["removedRows"].as_u64().unwrap(),
        (before.len() - kept.len()) as u64
    );
    assert_eq!(
        call(&engine, "oldest", json!({})).as_i64(),
        Some(kept[0].0),
        "oldest follows retention"
    );
    // A second pass finds nothing more; a later one empties whole days and keeps the open span.
    let again = call(&engine, "maintain", json!({ "nowMs": now }));
    assert_eq!(again["pruned"]["removedRows"], 0);
    call(&engine, "maintain", json!({ "nowMs": start + 300 * DAY }));
    let counts = call(&engine, "counts", json!({}));
    assert_eq!(counts["reading"], 1, "{counts}");
    assert_eq!(counts["reading_days"], 0, "{counts}");
    // Rollups are kept for good.
    assert!(counts["rollup_hour"].as_u64().unwrap() >= 72, "{counts}");
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// A crash can cut the log's unacknowledged tail at any byte while redb holds the previous
/// commit. Every cut must give the projection of exactly the whole frames that survive, with the
/// tick's day values neither half-applied nor doubled.
#[test]
fn a_cut_at_every_byte_of_a_tick_leaves_whole_day_values() {
    let base = temp_dir("packed-cut-base");
    let start = MIDNIGHT + 20 * HOUR;
    let reset = start + 9 * DAY;
    let w = |used: f64| vec![window("five_hour", used, reset)];
    let before = {
        let engine = open(&base);
        for i in 0..20 {
            record(&engine, vec![obs(1, start + i * 5 * MIN, w(i as f64))]);
        }
        let d = dump(&engine);
        drop(engine);
        d
    };
    let snapshot = temp_dir("packed-cut-snapshot");
    copy_dir(&base, &snapshot);
    let active_dir = |root: &Path| root.join("log").join("readings");
    let prefix = fs::read(find(&active_dir(&base), ".active").pop().unwrap()).unwrap();
    let after = {
        let engine = open(&base);
        // This tick closes a span into its day value and starts a new one.
        record(&engine, vec![obs(1, start + 100 * MIN, w(44.5))]);
        let d = dump(&engine);
        drop(engine);
        d
    };
    assert_ne!(before, after);
    let active = find(&active_dir(&base), ".active").pop().unwrap();
    let full = fs::read(&active).unwrap();
    assert!(full.len() > prefix.len() && full[..prefix.len()] == prefix[..]);
    let name = active.file_name().unwrap().to_owned();
    for cut in prefix.len()..=full.len() {
        let rebuild = cut % 64 == 0;
        let dir = temp_dir("packed-cut");
        copy_dir(&snapshot, &dir);
        fs::write(active_dir(&dir).join(&name), &full[..cut]).unwrap();
        if rebuild {
            fs::remove_file(dir.join("aac.redb")).unwrap();
        }
        let engine = open(&dir);
        let expected = if cut == full.len() { &after } else { &before };
        assert!(dump(&engine) == *expected, "cut at byte {cut}, rebuild {rebuild}");
        // The store keeps working after the cut, and no sequence or span is reused.
        record(&engine, vec![obs(1, start + 110 * MIN, w(60.0))]);
        assert_eq!(raw(&engine, "five_hour", start, reset).last().unwrap().2, 60.0);
        drop(engine);
        fs::remove_dir_all(&dir).unwrap();
    }
    fs::remove_dir_all(&base).unwrap();
    fs::remove_dir_all(&snapshot).unwrap();
}

/// A day closed before its last samples arrive keeps its rollups as closed. A rebuild reads the
/// readings first, then the close; the close must win for the whole day.
#[test]
fn a_rebuild_matches_when_samples_arrive_after_their_day_closed() {
    let dir = temp_dir("packed-late");
    let engine = open(&dir);
    let reset = MIDNIGHT + 9 * DAY;
    let tick = |minute: i64| {
        obs(
            1,
            MIDNIGHT + minute * MIN,
            vec![window("five_hour", (minute / 5) as f64, reset)],
        )
    };
    record(&engine, (0..20 * 60).map(tick).collect());
    call(&engine, "maintain", json!({ "nowMs": MIDNIGHT + DAY + 3 * HOUR }));
    // Hours 20 to 23 of the closed day arrive late, then the next day goes on.
    record(&engine, (20 * 60..26 * 60).map(tick).collect());
    let args = json!({ "fromMs": MIDNIGHT, "toMs": MIDNIGHT + 2 * DAY, "resolution": "hour" });
    let hours = call(&engine, "seriesPoints", args.clone());
    let first_day = hours["series"][0]["points"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p[0].as_i64().unwrap() < MIDNIGHT + DAY)
        .count();
    assert_eq!(first_day, 20, "late samples leave the closed day's rollups alone");
    let spans = raw(&engine, "five_hour", MIDNIGHT, MIDNIGHT + 2 * DAY);
    assert!(
        spans
            .iter()
            .any(|p| p.0 >= MIDNIGHT + 20 * HOUR && p.0 < MIDNIGHT + DAY)
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = open(&dir);
    assert_eq!(call(&engine, "seriesPoints", args), hours);
    assert_eq!(raw(&engine, "five_hour", MIDNIGHT, MIDNIGHT + 2 * DAY), spans);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// A trimmed file doubles at the next commit, so compaction runs only when the file is over
/// twice its used pages.
#[test]
fn compaction_runs_only_when_it_saves_space() {
    let dir = temp_dir("packed-compact");
    let start = MIDNIGHT - 3 * DAY;
    let engine = open(&dir);
    let mut observations = Vec::new();
    for account in 0..8 {
        for minute in 0..3 * 1440 {
            let t = start + minute * MIN;
            let used = (minute % 300) as f64 / 3.7;
            observations.push(obs(account, t, vec![window("five_hour", used, t + DAY)]));
        }
    }
    for chunk in observations.chunks(2000) {
        record(&engine, chunk.to_vec());
    }
    let file = || fs::metadata(dir.join("aac.redb")).unwrap().len();
    let grown = file();
    assert_eq!(engine.get_meta("last_compact_ms").unwrap(), None);
    // Retention empties the readings, leaving most of the file free.
    let freed = call(
        &engine,
        "maintain",
        json!({ "nowMs": start + 300 * DAY, "compact": true }),
    );
    assert!(freed["pruned"]["removedRows"].as_u64().unwrap() > 1000, "{freed}");
    assert_eq!(freed["compacted"], true, "{freed}");
    let stamped = engine.get_meta("last_compact_ms").unwrap();
    assert!(stamped.is_some());
    assert!(file() < grown, "{} against {grown}", file());
    // Just compacted, the file is about twice its pages: asked again, it is left alone.
    let trimmed = file();
    let again = call(
        &engine,
        "maintain",
        json!({ "nowMs": start + 300 * DAY, "compact": true }),
    );
    assert_eq!(again["compacted"], false, "{again}");
    assert_eq!(file(), trimmed, "not trimmed");
    assert_eq!(engine.get_meta("last_compact_ms").unwrap(), stamped);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// The budget behind "bytes per reading below the JSON history's": a change-every-few-minutes
/// series stays near a dozen bytes per closed span.
#[test]
fn a_day_value_stays_small_per_span() {
    let dir = temp_dir("packed-budget");
    let engine = open(&dir);
    let start = MIDNIGHT;
    let observations: Vec<Json> = (0..1440)
        .map(|minute| {
            let used = (minute / 4) as f64 % 100.0;
            obs(
                1,
                start + minute * MIN,
                vec![window("five_hour", used, start + (minute / 300 + 1) * 300 * MIN)],
            )
        })
        .collect();
    record(&engine, observations);
    let counts = call(&engine, "counts", json!({}));
    let spans = counts["reading"].as_u64().unwrap();
    let stored = counts["tableBytes"]["reading_day"][0].as_u64().unwrap();
    assert!(spans >= 300, "{counts}");
    assert!(stored / spans <= 16, "{stored} bytes for {spans} spans");
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}
