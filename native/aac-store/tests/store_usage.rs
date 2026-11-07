//! Store-level suites: usage decisions and projections, replay determinism, day closing and
//! segment retirement, backup and restore, and redb corruption (docs/store.md 9.3, 10).

mod common;

use aac_store::db;
use aac_store::engine::{Engine, now_ms};
use aac_store::ops;
use common::{copy_dir, temp_dir};
use redb::{ReadableDatabase, ReadableTable};
use serde_json::{Value as Json, json};
use std::fs;
use std::path::Path;

const MIN: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;

fn identity(n: u32) -> String {
    format!("{n:064x}")
}

fn window(key: &str, used: Option<f64>, reset: Option<i64>) -> Json {
    json!({
        "identity": format!("[\"{key}\",\"rate_limit\",null]"),
        "key": key, "label": key, "kind": "rate_limit", "unit": null,
        "used_pct": used, "remaining_pct": used.map(|u| 100.0 - u),
        "reset_at_ms": reset, "window_minutes": 300.0,
    })
}

fn obs(account: u32, t: i64, windows: Vec<Json>) -> Json {
    obs_status(account, t, "ok", windows)
}

fn obs_status(account: u32, t: i64, status: &str, windows: Vec<Json>) -> Json {
    json!({
        "account": {
            "identity": identity(account), "provider": "claude", "account_id": format!("claude:a{account}"),
            "plan": "max", "platform": "ubuntu", "source": "test source", "status": status, "is_active": account == 1,
        },
        "sampled_at_ms": t, "observed_at_ms": t + 500, "windows": windows,
    })
}

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, None).unwrap()
}

fn call(engine: &Engine, method: &str, args: Json) -> Json {
    ops::call(engine, method, &args.to_string()).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn record(engine: &Engine, observations: Vec<Json>) -> Json {
    call(engine, "record", json!({ "observations": observations }))
}

/// Every row of the usage tables, for equality checks.
fn dump(engine: &Engine, tables: &[&str]) -> Vec<(String, Vec<u8>, Vec<u8>)> {
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
                if !tables.contains(&name) {
                    continue;
                }
                for item in read.open_table(def)?.iter()? {
                    let (k, v) = item?;
                    out.push((name.to_string(), k.value().to_vec(), v.value().to_vec()));
                }
            }
            for (name, def) in [
                ("series_by_provider", db::SERIES_BY_PROVIDER),
                ("day_closed", db::DAY_CLOSED),
            ] {
                if !tables.contains(&name) {
                    continue;
                }
                for item in read.open_table(def)?.iter()? {
                    let (k, _) = item?;
                    out.push((name.to_string(), k.value().to_vec(), Vec::new()));
                }
            }
            Ok(out)
        })
        .unwrap()
}

const ALL: &[&str] = &[
    "account",
    "window",
    "series",
    "reading_day",
    "latest",
    "account_latest",
    "account_state",
    "period",
    "rollup_hours",
    "rollup_day",
    "series_by_provider",
    "day_closed",
];

/// A morning of use on two accounts, with one reset.
fn morning(start: i64) -> Vec<Json> {
    let reset1 = start + 2 * HOUR;
    let reset2 = start + 7 * HOUR;
    let mut out = Vec::new();
    let mut used = 10.0;
    for minute in 0..240 {
        let t = start + minute * MIN;
        let reset = if t < reset1 { reset1 } else { reset2 };
        if t == reset1 {
            used = 4.0;
        } else if minute % 7 == 0 {
            used += 1.0;
        }
        out.push(obs(
            1,
            t,
            vec![
                window("five_hour", Some(used), Some(reset)),
                window("seven_day", Some(40.0), Some(start + 5 * DAY)),
            ],
        ));
        if minute % 3 == 0 {
            out.push(obs(
                2,
                t + 1000,
                vec![window(
                    "five_hour",
                    Some(50.0 + (minute / 30) as f64),
                    Some(start + 3 * HOUR),
                )],
            ));
        }
    }
    out
}

#[test]
fn a_second_writer_is_refused() {
    let dir = temp_dir("lock");
    let engine = open(&dir);
    let second = ops::open_engine(dir.to_str().unwrap(), None, None);
    assert_eq!(second.err().map(|e| e.code), Some("lock_held"));
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn decisions_follow_the_json_history_rules() {
    let dir = temp_dir("decide");
    let engine = open(&dir);
    let t = 1_793_000_000_000 - 1_793_000_000_000 % (5 * MIN); // a five-minute boundary
    let w = |u: f64| vec![window("five_hour", Some(u), Some(t + 4 * HOUR))];
    // An initial unavailable row is not a measured zero.
    assert_eq!(
        record(&engine, vec![obs_status(1, t, "unavailable", vec![])])["skipped"],
        1
    );
    assert_eq!(record(&engine, vec![obs(1, t, w(10.0))])["sampled"], 1);
    assert_eq!(record(&engine, vec![obs(1, t, w(10.0))])["skipped"], 1, "same sample");
    assert_eq!(
        record(&engine, vec![obs(1, t + MIN, w(10.0))])["skipped"],
        1,
        "unchanged in the same bucket"
    );
    assert_eq!(
        record(&engine, vec![obs(1, t + 5 * MIN, w(10.0))])["seen"],
        1,
        "unchanged in a new bucket"
    );
    assert_eq!(
        record(&engine, vec![obs(1, t + 6 * MIN, w(12.0))])["sampled"],
        1,
        "a change"
    );
    assert_eq!(
        record(&engine, vec![obs(1, t + 2 * MIN, w(11.0))])["skipped"],
        1,
        "older than the latest"
    );
    assert_eq!(
        record(&engine, vec![obs_status(1, t + 7 * MIN, "error", vec![])])["sampled"],
        1,
        "a gap"
    );
    assert_eq!(
        record(&engine, vec![obs_status(1, t + 15 * MIN, "error", vec![])])["skipped"],
        1,
        "the same gap"
    );
    let bad = json!({ "account": { "identity": "nope" }, "sampled_at_ms": t, "observed_at_ms": t, "windows": [] });
    assert_eq!(record(&engine, vec![bad])["rejected"], 1);

    let records = call(
        &engine,
        "historyRecords",
        json!({ "fromMs": t, "toMs": t + HOUR, "nowMs": t + HOUR, "thinning": { "kind": "none" } }),
    );
    let times: Vec<&str> = records
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["sampledAt"].as_str().unwrap())
        .collect();
    assert_eq!(times.len(), 4, "{records:#}");
    let used: Vec<Json> = records
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["windows"]
                .get(0)
                .map(|w| w["usedPercent"].clone())
                .unwrap_or(Json::Null)
        })
        .collect();
    assert_eq!(used, vec![json!(10.0), json!(10.0), json!(12.0), Json::Null]);
    assert_eq!(records[3]["status"], "error");
    assert_eq!(records[0]["windows"][0]["resetAt"], aac_store::usage::iso(t + 4 * HOUR));
    assert_eq!(records[0]["isActive"], true);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn consumption_periods_and_coverage() {
    let dir = temp_dir("periods");
    let engine = open(&dir);
    let start = 1_793_000_000_000 - 1_793_000_000_000 % HOUR;
    let result = record(&engine, morning(start));
    assert!(result["sampled"].as_u64().unwrap() > 30, "{result}");
    let series = json!([{ "identity": identity(1), "window": "[\"five_hour\",\"rate_limit\",null]" }]);
    let periods = call(
        &engine,
        "periods",
        json!({ "series": series, "fromMs": start, "toMs": start + DAY }),
    );
    let list = periods[0]["periods"].as_array().unwrap();
    assert_eq!(list.len(), 2, "{periods:#}");
    assert_eq!(list[0]["final"], true);
    assert_eq!(list[0]["startUsed"], 11.0);
    // 10% to 28% before the reset (18 one-point steps in 120 minutes), 4% after it, then steps.
    let before = list[0]["endUsed"].as_f64().unwrap();
    assert_eq!(list[0]["consumed"].as_f64().unwrap(), before - 11.0);
    let after = list[1]["endUsed"].as_f64().unwrap();
    assert_eq!(list[1]["consumed"].as_f64().unwrap(), after);
    let hours = call(
        &engine,
        "seriesPoints",
        json!({ "identities": [identity(1)], "windowKey": "five_hour", "fromMs": start, "toMs": start + DAY, "resolution": "hour" }),
    );
    let points = hours["series"][0]["points"].as_array().unwrap();
    let consumed: f64 = points.iter().map(|p| p[1].as_f64().unwrap()).sum();
    assert!((consumed - (before - 11.0 + after)).abs() < 1e-9, "{consumed}");
    let covered: u64 = points.iter().map(|p| p[4].as_u64().unwrap()).sum();
    assert!((235..=240).contains(&covered), "covered {covered} minutes");
    let burn = call(
        &engine,
        "burnLong",
        json!({ "series": series, "spansMs": [4 * HOUR, 7 * DAY], "nowMs": start + 4 * HOUR }),
    );
    assert!(burn[0]["spans"][0]["coverage"].as_f64().unwrap() > 0.95);
    assert!(burn[0]["spans"][1]["coverage"].as_f64().unwrap() < 0.05);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn replay_rebuilds_identical_tables_from_the_logs() {
    let dir = temp_dir("replay");
    let start = 1_793_000_000_000 - 1_793_000_000_000 % HOUR;
    let all = morning(start);
    let (first, rest) = all.split_at(all.len() / 3);
    let snapshot = temp_dir("replay-snap");
    {
        let engine = open(&dir);
        record(&engine, first.to_vec());
        engine.close().unwrap();
    }
    fs::copy(dir.join("aac.redb"), snapshot.join("aac.redb")).unwrap();
    let full = {
        let engine = open(&dir);
        record(&engine, rest.to_vec());
        let d = dump(&engine, ALL);
        engine.close().unwrap();
        d
    };
    // From nothing.
    fs::remove_file(dir.join("aac.redb")).unwrap();
    {
        let engine = open(&dir);
        assert_eq!(dump(&engine, ALL), full);
        engine.close().unwrap();
    }
    // From a mid-way checkpoint.
    fs::copy(snapshot.join("aac.redb"), dir.join("aac.redb")).unwrap();
    {
        let engine = open(&dir);
        assert_eq!(dump(&engine, ALL), full);
        engine.close().unwrap();
    }
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&snapshot).unwrap();
}

#[test]
fn a_corrupt_redb_is_quarantined_and_rebuilt() {
    let dir = temp_dir("redb-corrupt");
    let start = 1_793_000_000_000 - 1_793_000_000_000 % HOUR;
    let before = {
        let engine = open(&dir);
        record(&engine, morning(start));
        let d = dump(&engine, ALL);
        engine.close().unwrap();
        d
    };
    let path = dir.join("aac.redb");
    let mut bytes = fs::read(&path).unwrap();
    let len = bytes.len();
    for i in (len / 4..len).step_by(97) {
        bytes[i] ^= 0x5a;
    }
    fs::write(&path, &bytes).unwrap();
    let engine = open(&dir);
    let status = engine.status().unwrap();
    let kinds: Vec<String> = status["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap().to_string())
        .collect();
    assert!(
        kinds.iter().any(|k| k == "redb_quarantined" || k == "redb_repaired"),
        "{kinds:?}"
    );
    assert_eq!(dump(&engine, ALL), before);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn closed_days_keep_rollups_after_their_readings_retire() {
    let dir = temp_dir("retire");
    let backups = temp_dir("retire-backups");
    let now = now_ms() as i64;
    let start = now - 3 * DAY - now % HOUR;
    let engine = open(&dir);
    let mut observations = morning(start);
    observations.extend(morning(start + DAY));
    record(&engine, observations);
    let later = now + 100 * DAY;
    // Before a backup holds the segments, nothing retires.
    let first = call(&engine, "maintain", json!({ "nowMs": later }));
    assert!(first["closed"]["closed"].as_array().unwrap().len() >= 2, "{first:#}");
    assert_eq!(first["retired"]["retired"], 0);
    let rollups = dump(&engine, &["rollup_hours", "rollup_day", "day_closed"]);
    let finals: Vec<_> = dump(&engine, &["period"]);
    call(
        &engine,
        "backup",
        json!({ "roots": [backups.to_str().unwrap()], "withRedb": false }),
    );
    let second = call(&engine, "maintain", json!({ "nowMs": later }));
    assert!(second["retired"]["retired"].as_u64().unwrap() >= 1, "{second:#}");
    // Readings past retention are gone except each series' latest.
    let counts = call(&engine, "counts", json!({}));
    assert_eq!(counts["reading"], counts["series"], "{counts}");
    engine.close().unwrap();
    drop(engine);
    // A rebuild from the logs alone restores every closed day's rollups and final periods.
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = open(&dir);
    assert_eq!(dump(&engine, &["rollup_hours", "rollup_day", "day_closed"]), rollups);
    let rebuilt_periods = dump(&engine, &["period"]);
    for row in &rebuilt_periods {
        let original = finals.iter().find(|r| r.1 == row.1).expect("period kept");
        let a: aac_store::usage::Period = db::unpack(&original.2).unwrap();
        let b: aac_store::usage::Period = db::unpack(&row.2).unwrap();
        assert_eq!(
            (a.consumed, a.end_ms, a.start_used),
            (b.consumed, b.end_ms, b.start_used)
        );
        assert!(b.is_final);
    }
    assert!(!rebuilt_periods.is_empty());
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&backups).unwrap();
}

#[test]
fn a_backup_restores_the_same_tables() {
    let dir = temp_dir("backup");
    let roots = [temp_dir("backup-local"), temp_dir("backup-share")];
    let start = 1_793_000_000_000 - 1_793_000_000_000 % HOUR;
    let all = morning(start);
    let (first, rest) = all.split_at(all.len() / 2);
    let engine = open(&dir);
    record(&engine, first.to_vec());
    call(&engine, "maintain", json!({ "nowMs": now_ms() + 2 * DAY as u64 }));
    let at_backup = dump(&engine, ALL);
    let backup = call(
        &engine,
        "backup",
        json!({ "roots": [roots[0].to_str().unwrap(), roots[1].to_str().unwrap()], "withRedb": true }),
    );
    assert_eq!(backup["mirrors"][0]["ok"], true, "{backup:#}");
    record(&engine, rest.to_vec());
    assert_ne!(dump(&engine, ALL), at_backup);
    engine.close().unwrap();
    drop(engine);
    // Restore from the share copy, as after a lost disk.
    let id = backup["id"].as_str().unwrap();
    let restored = ops::dispatch(
        &ops::Slot::new(None),
        "restore",
        &json!({ "dir": dir.to_str().unwrap(), "backupDir": roots[1].join("sets").join(id).to_str().unwrap() })
            .to_string(),
    )
    .unwrap();
    assert!(restored.contains(id));
    let engine = open(&dir);
    assert_eq!(dump(&engine, ALL), at_backup);
    engine.close().unwrap();
    drop(engine);
    // A tampered backup is refused before anything changes.
    let backup_dir = roots[0].join("sets").join(id);
    let manifest = backup_dir.join("readings.manifest.v1.msgpack");
    let mut bytes = fs::read(&manifest).unwrap();
    bytes[20] ^= 1;
    fs::write(&manifest, bytes).unwrap();
    let refused = ops::dispatch(
        &ops::Slot::new(None),
        "restore",
        &json!({ "dir": dir.to_str().unwrap(), "backupDir": backup_dir.to_str().unwrap() }).to_string(),
    );
    assert_eq!(refused.err().map(|e| e.code), Some("backup_unverified"));
    fs::remove_dir_all(&dir).unwrap();
    for root in roots {
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn a_store_from_a_newer_build_is_left_untouched() {
    let dir = temp_dir("newer");
    {
        let engine = open(&dir);
        engine.close().unwrap();
    }
    {
        let database = redb::Database::create(dir.join("aac.redb")).unwrap();
        let txn = database.begin_write().unwrap();
        txn.open_table(db::META)
            .unwrap()
            .insert("store_schema_version", "99".as_bytes())
            .unwrap();
        txn.commit().unwrap();
    }
    let before: Vec<_> = walk(&dir);
    let result = ops::open_engine(dir.to_str().unwrap(), None, None);
    assert_eq!(result.err().map(|e| e.code), Some("newer_store"));
    assert_eq!(walk(&dir), before);
    fs::remove_dir_all(&dir).unwrap();
}

fn walk(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let copy = temp_dir("walk");
    copy_dir(dir, &copy);
    fn rec(base: &Path, path: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(path).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                rec(base, &entry.path(), out);
            } else {
                out.push((
                    entry.path().strip_prefix(base).unwrap().display().to_string(),
                    fs::read(entry.path()).unwrap(),
                ));
            }
        }
    }
    rec(&copy, &copy, &mut out);
    out.sort();
    fs::remove_dir_all(copy).unwrap();
    out
}

#[test]
fn a_gap_in_sampling_starts_a_new_span_and_stays_visible() {
    let dir = temp_dir("gap");
    let engine = open(&dir);
    let t = 1_793_000_000_000 - 1_793_000_000_000 % (5 * MIN);
    let w = || vec![window("five_hour", Some(30.0), Some(t + 4 * HOUR))];
    record(&engine, vec![obs(1, t, w()), obs(1, t + 5 * MIN, w())]);
    // The dashboard was off for 55 minutes; the reading is unchanged when it comes back.
    assert_eq!(record(&engine, vec![obs(1, t + HOUR, w())])["seen"], 1);
    let records = call(
        &engine,
        "historyRecords",
        json!({ "fromMs": t, "toMs": t + 2 * HOUR, "nowMs": t + 2 * HOUR, "thinning": { "kind": "none" } }),
    );
    let times: Vec<i64> = records
        .as_array()
        .unwrap()
        .iter()
        .map(|r| chrono_ms(r["sampledAt"].as_str().unwrap()))
        .collect();
    assert_eq!(times, vec![t, t + 5 * MIN, t + HOUR], "{records:#}");
    let hours = call(
        &engine,
        "seriesPoints",
        json!({ "fromMs": t, "toMs": t + 2 * HOUR, "resolution": "hour" }),
    );
    let covered: u64 = hours["series"][0]["points"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p[4].as_u64().unwrap())
        .sum();
    assert_eq!(covered, 5, "only the five observed minutes count as covered");
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn history_thinning_fills_observed_spans_on_the_json_grid() {
    let dir = temp_dir("grid");
    let engine = open(&dir);
    let t = 1_793_000_000_000 - 1_793_000_000_000 % HOUR;
    let w = || vec![window("seven_day", Some(12.0), Some(t + 5 * DAY))];
    let observations: Vec<Json> = (0..=120).map(|m| obs(1, t + m * MIN, w())).collect();
    record(&engine, observations);
    let now = t + 3 * HOUR;
    let records = call(
        &engine,
        "historyRecords",
        json!({ "fromMs": t, "toMs": now, "nowMs": now, "thinning": { "kind": "history" } }),
    );
    let times: Vec<i64> = records
        .as_array()
        .unwrap()
        .iter()
        .map(|r| chrono_ms(r["sampledAt"].as_str().unwrap()))
        .collect();
    // One reading per five minutes from the span's start to its last sighting.
    let expected: Vec<i64> = (0..=24).map(|k| t + k * 5 * MIN).collect();
    assert_eq!(times, expected);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// Parses the store's own ISO output (`YYYY-MM-DDTHH:MM:SS.mmmZ`) back to milliseconds.
fn chrono_ms(iso: &str) -> i64 {
    let n = |a: usize, b: usize| iso[a..b].parse::<i64>().unwrap();
    let (y, m, d) = (n(0, 4), n(5, 7), n(8, 10));
    let (y2, m2) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y2.div_euclid(400);
    let yoe = y2 - era * 400;
    let doy = (153 * m2 + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    ((days * 24 + n(11, 13)) * 60 + n(14, 16)) * 60_000 + n(17, 19) * 1000 + n(20, 23)
}
