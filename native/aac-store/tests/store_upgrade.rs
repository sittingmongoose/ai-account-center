//! Stores written by an older build (docs/store.md 4.8): the `000a0ac7` fixtures hold a redb
//! projection and checkpoints that build made from an observation it accepted and this build
//! refuses (a synthetic, credential-shaped label). Opening them must serve exactly what a
//! replay of the logs alone serves, with the refused observation set aside and disclosed.

mod common;

use aac_store::engine::Engine;
use aac_store::ops;
use common::{copy_dir, temp_dir};
use serde_json::{Value as Json, json};
use std::fs;
use std::path::{Path, PathBuf};

const T0: i64 = 1_793_000_000_000;
const DAY: i64 = 86_400_000;
const SECRET: &str = "SYNTHETIC";

/// Unpacks a fixture store (its redb file is zstd-compressed) into a temporary folder.
fn unpack(kind: &str) -> PathBuf {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/store-v2-000a0ac7")
        .join(kind);
    let dir = temp_dir(&format!("upgrade-{kind}"));
    copy_dir(&source.join("log"), &dir.join("log"));
    let packed = fs::read(source.join("aac.redb.zst")).unwrap();
    fs::write(dir.join("aac.redb"), zstd::decode_all(packed.as_slice()).unwrap()).unwrap();
    dir
}

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, None).unwrap()
}

fn call(engine: &Engine, method: &str, args: Json) -> Json {
    ops::call(engine, method, &args.to_string()).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn kinds(engine: &Engine) -> Vec<String> {
    engine.status().unwrap()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap().to_string())
        .collect()
}

/// Every answer a route can give for the fixture's day.
fn answers(engine: &Engine) -> Json {
    let range = json!({ "fromMs": T0 - DAY, "toMs": T0 + 2 * DAY });
    let series = |account: char| json!({ "identity": account.to_string().repeat(64), "window": "five" });
    let mut out = serde_json::Map::new();
    for resolution in ["raw", "hour", "day"] {
        let mut args = range.clone();
        args["resolution"] = json!(resolution);
        out.insert(resolution.into(), call(engine, "seriesPoints", args));
    }
    out.insert("list".into(), call(engine, "seriesList", range.clone()));
    out.insert("at".into(), call(engine, "usageAt", json!({ "t": T0 + 60_000 })));
    out.insert(
        "history".into(),
        call(
            engine,
            "historyRecords",
            json!({ "fromMs": T0 - DAY, "toMs": T0 + 2 * DAY, "nowMs": T0 + 2 * DAY, "thinning": { "kind": "none" } }),
        ),
    );
    out.insert(
        "burn".into(),
        call(
            engine,
            "burnLong",
            json!({ "series": [series('a'), series('b')], "spansMs": [DAY, 7 * DAY], "nowMs": T0 + DAY }),
        ),
    );
    out.insert(
        "periods".into(),
        call(
            engine,
            "periods",
            json!({ "series": [series('a'), series('b')], "fromMs": T0 - DAY, "toMs": T0 + 2 * DAY }),
        ),
    );
    Json::Object(out)
}

/// The same store with its redb removed: what the logs alone say.
fn log_only(dir: &Path) -> (Json, Vec<String>) {
    let copy = temp_dir("upgrade-log-only");
    copy_dir(dir, &copy);
    fs::remove_file(copy.join("aac.redb")).unwrap();
    let engine = open(&copy);
    let out = (answers(&engine), kinds(&engine));
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&copy).unwrap();
    out
}

/// The refused observation is left out wherever a route reads.
fn assert_clean(answers: &Json) {
    let text = answers.to_string();
    assert!(!text.contains(SECRET), "a refused label reached an answer: {text}");
    let raw = &answers["raw"]["series"];
    let accounts: Vec<&str> = raw
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["accountId"].as_str().unwrap())
        .collect();
    assert_eq!(accounts, vec!["codex:b"], "only the accepted account has readings");
}

#[test]
fn an_old_projection_is_rebuilt_and_its_refused_observation_set_aside() {
    let dir = unpack("open");
    let (expected, log_kinds) = log_only(&dir);
    assert!(log_kinds.iter().any(|k| k == "event_quarantined"), "{log_kinds:?}");
    assert_clean(&expected);
    let engine = open(&dir);
    let found = kinds(&engine);
    assert!(found.iter().any(|k| k == "projector_rebuilt"), "{found:?}");
    assert!(found.iter().any(|k| k == "event_quarantined"), "{found:?}");
    assert_eq!(engine.status().unwrap()["state"], "degraded");
    assert_eq!(answers(&engine), expected, "the upgraded store answers as its logs do");
    engine.close().unwrap();
    drop(engine);
    // A plain reopen still says so.
    let engine = open(&dir);
    assert!(kinds(&engine).iter().any(|k| k == "event_quarantined"));
    assert_eq!(engine.status().unwrap()["state"], "degraded");
    assert_eq!(answers(&engine), expected);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn closed_day_summaries_built_from_refused_observations_are_not_trusted() {
    let dir = unpack("closed");
    for pass in ["upgrade", "reopen", "rebuild"] {
        if pass == "rebuild" {
            fs::remove_file(dir.join("aac.redb")).unwrap();
        }
        let engine = open(&dir);
        let got = answers(&engine);
        assert_clean(&got);
        // The refused series has no rollups at all: an honest gap, not the old summary's numbers.
        for resolution in ["hour", "day"] {
            let series = got[resolution]["series"].as_array().unwrap();
            assert!(
                series.iter().all(|s| s["accountId"] == "codex:b"),
                "{pass} {resolution}: {series:?}"
            );
            assert!(
                !series.is_empty(),
                "{pass} {resolution}: the accepted account keeps its rollups"
            );
        }
        assert!(
            got["burn"][0]["spans"][0]["consumed"].is_null(),
            "{pass}: {}",
            got["burn"]
        );
        assert!(kinds(&engine).iter().any(|k| k == "event_quarantined"), "{pass}");
        assert!(
            kinds(&engine).iter().any(|k| k == "summary_quarantined"),
            "{pass}: {:?}",
            kinds(&engine)
        );
        assert_eq!(engine.status().unwrap()["state"], "degraded", "{pass}");
        engine.close().unwrap();
        drop(engine);
    }
    let (expected, _) = log_only(&dir);
    let engine = open(&dir);
    assert_eq!(answers(&engine), expected);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// A close written by this build excludes refused observations, so its summary is trusted for
/// that series even after the readings are gone.
#[test]
fn a_close_from_this_build_keeps_a_refused_series_clean_summary() {
    let dir = unpack("open");
    let backups = temp_dir("upgrade-close-backups");
    let reset = T0 + 4 * 3_600_000;
    let clean = |t: i64, used: f64| {
        json!({ "account": { "identity": "a".repeat(64), "provider": "codex", "account_id": "codex:a",
                "platform": "ubuntu", "source": "fixture", "status": "ok", "is_active": true },
                "sampled_at_ms": t, "observed_at_ms": t,
                "windows": [{ "identity": "five", "key": "five", "label": "5 hour", "used_pct": used, "reset_at_ms": reset }] })
    };
    let engine = open(&dir);
    call(
        &engine,
        "record",
        json!({ "observations": [clean(T0 + 600_000, 40.0), clean(T0 + 900_000, 45.0)] }),
    );
    // Close the day, then let the readings expire and their segments retire.
    call(&engine, "maintain", json!({ "nowMs": T0 + 3 * DAY }));
    let before = call(
        &engine,
        "seriesPoints",
        json!({ "identities": ["a".repeat(64)], "fromMs": T0 - DAY, "toMs": T0 + DAY, "resolution": "hour" }),
    );
    assert_eq!(before["series"][0]["points"][0][1], 5.0, "{before}");
    call(
        &engine,
        "backup",
        json!({ "roots": [backups.to_str().unwrap()], "withRedb": false }),
    );
    let far = aac_store::engine::now_ms() + 200 * DAY as u64;
    call(&engine, "maintain", json!({ "nowMs": far }));
    call(&engine, "maintain", json!({ "nowMs": far }));
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = open(&dir);
    let after = call(
        &engine,
        "seriesPoints",
        json!({ "identities": ["a".repeat(64)], "fromMs": T0 - DAY, "toMs": T0 + DAY, "resolution": "hour" }),
    );
    assert_eq!(after, before, "the trusted close restores the clean summary");
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&backups).unwrap();
}

const MIN: i64 = 60_000;
const HOUR: i64 = 3_600_000;

/// The fixtures' observation shape (the 000a0ac7 writer's), for the clean reference stores.
fn reading(account: char, t: i64, window: &str, used: f64, reset_at: i64) -> Json {
    json!({ "account": { "identity": account.to_string().repeat(64), "provider": "codex", "account_id": format!("codex:{account}"),
            "platform": "ubuntu", "source": "probe", "status": "ok", "is_active": true },
            "sampled_at_ms": t, "observed_at_ms": t,
            "windows": [{ "identity": window, "key": window, "label": if window == "seven" { "weekly" } else { "5 hour" },
                          "used_pct": used, "reset_at_ms": reset_at }] })
}

fn periods_of(engine: &Engine, account: char, window: &str) -> Json {
    let answer = call(
        engine,
        "periods",
        json!({ "series": [{ "identity": account.to_string().repeat(64), "window": window }], "fromMs": 0, "toMs": T0 + 30 * DAY }),
    );
    answer[0]["periods"].clone()
}

/// A store this build writes from the clean readings alone, with the same maintenance.
fn clean_store(name: &str, steps: Vec<(Vec<Json>, Option<i64>)>) -> (PathBuf, Engine) {
    let dir = temp_dir(name);
    let engine = open(&dir);
    for (observations, maintain) in steps {
        if !observations.is_empty() {
            call(&engine, "record", json!({ "observations": observations }));
        }
        if let Some(now) = maintain {
            call(&engine, "maintain", json!({ "nowMs": now }));
        }
    }
    (dir, engine)
}

/// No live row of any table holds the refused text.
fn assert_no_refused_text(engine: &Engine) {
    engine
        .with_db(|database| {
            use redb::{ReadableDatabase, ReadableTable};
            let read = database.begin_read()?;
            let bytes = SECRET.as_bytes();
            let holds = |b: &[u8]| b.windows(bytes.len()).any(|w| w == bytes);
            for def in [
                aac_store::db::DICT,
                aac_store::db::ACCOUNT,
                aac_store::db::WINDOW,
                aac_store::db::SERIES,
                aac_store::db::READING_DAY,
                aac_store::db::LATEST,
                aac_store::db::ACCOUNT_LATEST,
                aac_store::db::ACCOUNT_STATE,
                aac_store::db::PERIOD,
                aac_store::db::ROLLUP_HOURS,
                aac_store::db::ROLLUP_DAY,
                aac_store::db::SUMMARY_UNMARKED,
            ] {
                for item in read.open_table(def)?.iter()? {
                    let (k, v) = item?;
                    assert!(
                        !holds(k.value()) && !holds(v.value()),
                        "a live row holds the refused text"
                    );
                }
            }
            for item in read.open_table(aac_store::db::DICT_REV)?.iter()? {
                let (k, _) = item?;
                assert!(!holds(k.value()), "dict_rev holds the refused text");
            }
            Ok(())
        })
        .unwrap();
}

/// The multi-day fixture: day 1's series a/five holds a refused 100 between 30 and 55; the close
/// of day 0 (computed by 000a0ac7 in one maintenance pass) carries day 1's reset periods, built
/// over it. Account c's window identity itself is credential-shaped.
#[test]
fn old_periods_over_a_refused_reading_on_another_day_are_not_served() {
    let dir = unpack("multi");
    let d1 = T0 + DAY;
    let d2 = T0 + 2 * DAY;
    let day_reset = |t: i64| t - t.rem_euclid(DAY) + DAY;
    let r = |acct: char, t: i64, used: f64| reading(acct, t, "five", used, day_reset(t));
    let (reference_dir, reference) = clean_store(
        "upgrade-multi-clean",
        vec![(
            vec![
                r('a', T0, 10.0),
                r('a', T0 + 5 * MIN, 20.0),
                r('b', T0, 10.0),
                r('b', T0 + 5 * MIN, 20.0),
                r('a', d1, 30.0),
                r('a', d1 + 10 * MIN, 55.0),
                r('a', d1 + 15 * MIN, 60.0),
                r('b', d1, 10.0),
                r('b', d1 + 5 * MIN, 25.0),
                r('a', d2, 70.0),
                r('a', d2 + 5 * MIN, 75.0),
                r('b', d2, 5.0),
                r('b', d2 + 5 * MIN, 6.0),
            ],
            Some(T0 + 6 * DAY),
        )],
    );
    let expected_a = periods_of(&reference, 'a', "five");
    let expected_b = periods_of(&reference, 'b', "five");
    assert!(expected_a.as_array().unwrap().iter().all(|p| p["hitLimitAt"].is_null()));
    reference.close().unwrap();
    drop(reference);
    fs::remove_dir_all(&reference_dir).unwrap();
    for pass in ["upgrade", "reopen", "rebuild", "rebuild-reopen"] {
        if pass == "rebuild" {
            fs::remove_file(dir.join("aac.redb")).unwrap();
        }
        let engine = open(&dir);
        assert_eq!(periods_of(&engine, 'a', "five"), expected_a, "{pass}: a/five periods");
        assert_eq!(periods_of(&engine, 'b', "five"), expected_b, "{pass}: b/five periods");
        assert!(!answers(&engine).to_string().contains(SECRET), "{pass}");
        assert_no_refused_text(&engine);
        engine.close().unwrap();
        drop(engine);
    }
    fs::remove_dir_all(&dir).unwrap();
}

/// The weekly fixture: one reading a day, maintenance after each midnight; day 2 is refused (100),
/// so 000a0ac7 saw 100 then 40 as a reset and closed that invented period on day 6.
#[test]
fn an_old_weekly_period_with_an_invented_reset_is_not_served() {
    let dir = unpack("weekly");
    let start = T0 - T0.rem_euclid(DAY) + 6 * HOUR;
    let reset = start + 7 * DAY - HOUR;
    let mut steps = Vec::new();
    for d in 0..9i64 {
        let t = start + d * DAY;
        let (used, r) = if d < 7 {
            (10.0 * (d as f64 + 1.0), reset)
        } else {
            ((d - 6) as f64, reset + 7 * DAY)
        };
        let observations = if d == 2 {
            Vec::new()
        } else {
            vec![reading('a', t, "seven", used, r)]
        };
        steps.push((observations, Some(start - 6 * HOUR + (d + 1) * DAY + HOUR)));
    }
    let (reference_dir, reference) = clean_store("upgrade-weekly-clean", steps);
    let expected = periods_of(&reference, 'a', "seven");
    reference.close().unwrap();
    drop(reference);
    fs::remove_dir_all(&reference_dir).unwrap();
    for pass in ["upgrade", "reopen", "rebuild"] {
        if pass == "rebuild" {
            fs::remove_file(dir.join("aac.redb")).unwrap();
        }
        let engine = open(&dir);
        let got = periods_of(&engine, 'a', "seven");
        assert_eq!(got, expected, "{pass}");
        // No two periods overlap, and none starts at the invented reset (40%).
        let list = got.as_array().unwrap();
        assert!(
            list.windows(2).all(|p| p[0]["end"].as_i64() < p[1]["start"].as_i64()),
            "{pass}: {got}"
        );
        assert!(list.iter().all(|p| p["startUsed"] != 40.0), "{pass}: {got}");
        engine.close().unwrap();
        drop(engine);
    }
    fs::remove_dir_all(&dir).unwrap();
}

/// After this build retires the readings, a reopen and a rebuild from the logs serve the same
/// closed days: the clean day of a series whose older close was set aside reaches the ledger.
#[test]
fn reopen_and_log_only_rebuild_agree_after_retirement() {
    let dir = unpack("multi");
    let backups = temp_dir("upgrade-retire-backups");
    let engine = open(&dir);
    let far = aac_store::engine::now_ms() + 200 * DAY as u64;
    for _ in 0..3 {
        call(
            &engine,
            "backup",
            json!({ "roots": [backups.to_str().unwrap()], "withRedb": false }),
        );
        call(&engine, "maintain", json!({ "nowMs": far }));
    }
    assert!(
        common::find(&dir.join("log").join("readings"), ".zst").is_empty()
            && common::find(&dir.join("log").join("readings"), ".seglog").is_empty(),
        "the readings retired"
    );
    engine.close().unwrap();
    drop(engine);
    // Closed days only: an open span and an unfinished period live in redb alone (a separate,
    // known limit), so they are not compared.
    let closed = |engine: &Engine| {
        let range = json!({ "fromMs": T0 - DAY, "toMs": T0 + 3 * DAY });
        let mut out = serde_json::Map::new();
        for resolution in ["hour", "day"] {
            let mut args = range.clone();
            args["resolution"] = json!(resolution);
            out.insert(resolution.into(), call(engine, "seriesPoints", args));
        }
        for (account, window) in [('a', "five"), ('b', "five")] {
            let finals: Vec<Json> = periods_of(engine, account, window)
                .as_array()
                .unwrap()
                .iter()
                .filter(|p| p["final"] == true)
                .cloned()
                .collect();
            out.insert(format!("{account}/{window}"), json!(finals));
        }
        Json::Object(out)
    };
    let engine = open(&dir);
    let reopened = closed(&engine);
    assert!(
        reopened["day"]["series"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["accountId"] == "codex:a")
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = open(&dir);
    assert_eq!(
        closed(&engine),
        reopened,
        "a rebuild from the logs serves what the reopen served"
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&backups).unwrap();
}
