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
