//! Regressions from the first code review of the store: projections over a log that lost
//! acknowledged frames, restore paths outside the store, and credentials in persisted strings.

mod common;

use aac_store::engine::{Engine, now_ms};
use aac_store::ops;
use common::{copy_dir, find, temp_dir};
use redb::{ReadableTable, TableHandle};
use serde_json::{Value as Json, json};
use std::fs;
use std::path::Path;

const MIN: i64 = 60_000;
const HOUR: i64 = 3_600_000;
const DAY: i64 = 86_400_000;
const T0: i64 = 1_793_000_000_000 - 1_793_000_000_000 % DAY + 9 * HOUR;

fn obs(t: i64, used: f64) -> Json {
    obs_with(t, used, |_| {})
}

fn obs_with(t: i64, used: f64, edit: impl FnOnce(&mut Json)) -> Json {
    let mut o = json!({
        "account": {
            "identity": format!("{:064x}", 1), "provider": "claude", "account_id": "claude:a1",
            "plan": "max", "platform": "ubuntu", "source": "test source", "status": "ok", "is_active": true,
        },
        "sampled_at_ms": t, "observed_at_ms": t,
        "windows": [{
            "identity": "[\"five_hour\",\"rate_limit\",null]", "key": "five_hour", "label": "5 hour",
            "kind": "rate_limit", "used_pct": used, "remaining_pct": 100.0 - used,
            "reset_at_ms": T0 + 4 * HOUR, "window_minutes": 300.0,
        }],
    });
    edit(&mut o);
    o
}

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, None).unwrap()
}

fn call(engine: &Engine, method: &str, args: Json) -> Json {
    ops::call(engine, method, &args.to_string()).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn raw_starts(engine: &Engine) -> Vec<i64> {
    let answer = call(
        engine,
        "seriesPoints",
        json!({ "fromMs": T0 - DAY, "toMs": T0 + DAY, "resolution": "raw" }),
    );
    answer["series"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|s| s["points"].as_array().unwrap().iter())
        .map(|p| p[0].as_i64().unwrap())
        .collect()
}

fn kinds(engine: &Engine) -> Vec<String> {
    engine.status().unwrap()["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["kind"].as_str().unwrap().to_string())
        .collect()
}

/// A sealed segment loses its last (acknowledged) frame: the projection must not keep it.
#[test]
fn projections_follow_the_log_when_acknowledged_frames_are_lost() {
    let dir = temp_dir("review-loss");
    {
        let engine = open(&dir);
        record(&engine, vec![obs(T0, 10.0), obs(T0 + 5 * MIN, 20.0)]);
        assert_eq!(raw_starts(&engine), vec![T0, T0 + 5 * MIN]);
        engine.close().unwrap();
    }
    let sealed = find(&dir.join("log").join("readings"), ".seglog")
        .pop()
        .expect("a sealed segment");
    let mut bytes = fs::read(&sealed).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    fs::write(&sealed, &bytes).unwrap();

    let rebuilt = {
        let copy = temp_dir("review-loss-rebuild");
        copy_dir(&dir, &copy);
        fs::remove_file(copy.join("aac.redb")).unwrap();
        let engine = open(&copy);
        let starts = raw_starts(&engine);
        engine.close().unwrap();
        drop(engine);
        fs::remove_dir_all(&copy).unwrap();
        starts
    };
    assert_eq!(rebuilt, vec![T0], "the log keeps only the first reading");

    let engine = open(&dir);
    assert_eq!(
        raw_starts(&engine),
        rebuilt,
        "the projection matches a rebuild from the log"
    );
    let found = kinds(&engine);
    assert!(found.iter().any(|k| k == "segment_degraded"), "{found:?}");
    assert!(found.iter().any(|k| k == "projection_missing_events"), "{found:?}");
    assert_eq!(engine.status().unwrap()["state"], "degraded");
    // The loss is recorded in the ledger: a plain reopen still discloses it, without rebuilding
    // again, and so does a rebuild.
    engine.close().unwrap();
    drop(engine);
    let engine = open(&dir);
    assert_eq!(raw_starts(&engine), rebuilt);
    let found = kinds(&engine);
    assert!(found.iter().any(|k| k == "projection_missing_events"), "{found:?}");
    assert!(!found.iter().any(|k| k == "projector_rebuilt"), "{found:?}");
    assert_eq!(engine.status().unwrap()["state"], "degraded");
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = open(&dir);
    assert!(kinds(&engine).iter().any(|k| k == "projection_missing_events"));
    assert_eq!(engine.status().unwrap()["state"], "degraded");
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// Retired segments are a legal gap: reopening after retirement keeps the projection.
#[test]
fn retirement_is_not_mistaken_for_loss() {
    let dir = temp_dir("review-retire");
    let backups = temp_dir("review-retire-backups");
    let start = now_ms() as i64 - 3 * DAY;
    {
        let engine = open(&dir);
        let mut observations = Vec::new();
        for minute in 0..2 * 1440 {
            observations.push(obs(start + minute * MIN, (minute / 9) as f64 % 90.0));
        }
        record(&engine, observations);
        engine.close().unwrap();
    }
    let engine = open(&dir);
    call(&engine, "maintain", json!({ "nowMs": now_ms() + 100 * DAY as u64 }));
    call(
        &engine,
        "backup",
        json!({ "roots": [backups.to_str().unwrap()], "withRedb": false }),
    );
    let retired = call(&engine, "maintain", json!({ "nowMs": now_ms() + 100 * DAY as u64 }));
    assert!(retired["retired"]["retired"].as_u64().unwrap() >= 1, "{retired:#}");
    let before = call(&engine, "counts", json!({}));
    engine.close().unwrap();
    drop(engine);
    let engine = open(&dir);
    let found = kinds(&engine);
    assert!(
        !found.iter().any(|k| k.contains("missing") || k == "projector_rebuilt"),
        "{found:?}"
    );
    assert_eq!(call(&engine, "counts", json!({}))["rollup_hour"], before["rollup_hour"]);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
    fs::remove_dir_all(&backups).unwrap();
}

fn record(engine: &Engine, observations: Vec<Json>) -> Json {
    call(engine, "record", json!({ "observations": observations }))
}

fn restore(dir: &Path, backup: &Path) -> Result<String, String> {
    ops::dispatch(
        &ops::Slot::new(None),
        "restore",
        &json!({ "dir": dir.to_str().unwrap(), "backupDir": backup.to_str().unwrap() }).to_string(),
    )
    .map_err(|e| format!("{}: {}", e.code, e.message))
}

fn write_backup(root: &Path, name: &str, manifest: &Json) -> std::path::PathBuf {
    let set = root.join("sets").join(name);
    fs::create_dir_all(&set).unwrap();
    fs::write(set.join("backup.json"), manifest.to_string()).unwrap();
    set
}

fn walk(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    fn rec(base: &Path, path: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in fs::read_dir(path).unwrap().flatten() {
            if entry.file_type().unwrap().is_dir() {
                rec(base, &entry.path(), out);
            } else if entry.file_name() != "store.lock" {
                out.push((
                    entry.path().strip_prefix(base).unwrap().display().to_string(),
                    fs::read(entry.path()).unwrap(),
                ));
            }
        }
    }
    rec(dir, dir, &mut out);
    out.sort();
    out
}

/// A backup manifest names nothing outside the store and its backup root.
#[test]
fn restore_refuses_manifests_that_reach_outside() {
    let base = temp_dir("review-restore");
    let store = base.join("store");
    {
        let engine = open(&store);
        record(&engine, vec![obs(T0, 10.0)]);
        engine.close().unwrap();
    }
    let victim = base.join("victim");
    fs::create_dir_all(&victim).unwrap();
    fs::write(victim.join("keep"), b"keep me").unwrap();
    fs::create_dir_all(store.join("tmp").join("restore-x")).unwrap();
    let root = base.join("backups");
    let sha = "0".repeat(64);
    let good_logs = json!({
        "readings": { "manifest": "readings.manifest.v1.msgpack", "segments": [], "active": null },
        "ledger": { "manifest": "ledger.manifest.v1.msgpack", "segments": [], "active": null },
    });
    let manifest = |id: &str, files: Json, logs: Json| {
        json!({ "schema": "aac-store-backup@1", "id": id, "storeSchemaVersion": 1, "storeRelease": 1, "withRedb": false,
                "instanceId": "0".repeat(32), "logs": logs, "files": files })
    };
    // (what, the backup's folder, the guard's words, manifest): each folder matches its id unless
    // the id itself is the case, so every case reaches the guard it names.
    let cases = [
        (
            "the reported id",
            "case0",
            "has an invalid id",
            manifest("x/../../../victim", json!([]), json!({})),
        ),
        (
            "an absolute id",
            "case1",
            "has an invalid id",
            manifest(victim.to_str().unwrap(), json!([]), json!({})),
        ),
        (
            "a folder that is not its id",
            "case2",
            "does not match its folder",
            manifest("b2", json!([]), good_logs.clone()),
        ),
        (
            "a file path with ..",
            "b3",
            "lists a file outside the backup",
            manifest(
                "b3",
                json!([{ "path": "pool/readings/../../../victim/keep", "sha256": sha, "bytes": 7 }]),
                good_logs.clone(),
            ),
        ),
        (
            "an absolute file path",
            "b4",
            "lists a file outside the backup",
            manifest(
                "b4",
                json!([{ "path": victim.join("keep").to_str().unwrap(), "sha256": sha, "bytes": 7 }]),
                good_logs.clone(),
            ),
        ),
        (
            "a segment name with ..",
            "b5",
            "names an invalid segment",
            manifest(
                "b5",
                json!([]),
                json!({
                    "readings": { "manifest": "readings.manifest.v1.msgpack", "segments": ["../../victim/keep"], "active": null },
                    "ledger": { "manifest": "ledger.manifest.v1.msgpack", "segments": [], "active": null },
                }),
            ),
        ),
        (
            "a restored file without a hash",
            "b6",
            "does not list with a hash",
            manifest("b6", json!([]), good_logs.clone()),
        ),
    ];
    let before = walk(&store);
    for (what, folder, guard, m) in cases.iter() {
        let set = write_backup(&root, folder, m);
        let answer = restore(&store, &set);
        let Err(error) = answer else {
            panic!("{what}: restored {answer:?}");
        };
        assert!(error.contains(guard), "{what}: refused by another check: {error}");
        assert_eq!(fs::read(victim.join("keep")).unwrap(), b"keep me", "{what}");
        assert_eq!(walk(&store), before, "{what}: the store is unchanged");
    }
    assert!(
        store.join("tmp").join("restore-x").exists(),
        "unrelated folders are left alone"
    );
    fs::remove_dir_all(&base).unwrap();
}

/// A restored file must be a regular file inside the backup root, not a link out of it.
#[test]
fn restore_refuses_a_linked_file() {
    let base = temp_dir("review-link");
    let store = base.join("store");
    let backups = base.join("backups");
    {
        let engine = open(&store);
        record(&engine, vec![obs(T0, 10.0)]);
        call(&engine, "maintain", json!({ "nowMs": now_ms() + 2 * DAY as u64 }));
        let made = call(
            &engine,
            "backup",
            json!({ "roots": [backups.to_str().unwrap()], "withRedb": false }),
        );
        assert!(made["id"].is_string());
        engine.close().unwrap();
    }
    let set = fs::read_dir(backups.join("sets"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let pooled = fs::read_dir(backups.join("pool").join("readings"))
        .unwrap()
        .flatten()
        .next()
        .map(|e| e.path());
    let outside = base.join("outside.bin");
    let Some(pooled) = pooled else {
        panic!("the backup pooled no segment");
    };
    fs::copy(&pooled, &outside).unwrap();
    fs::remove_file(&pooled).unwrap();
    #[cfg(unix)]
    let linked = std::os::unix::fs::symlink(&outside, &pooled).is_ok();
    #[cfg(windows)]
    let linked = std::os::windows::fs::symlink_file(&outside, &pooled).is_ok();
    if !linked {
        eprintln!("[!] this account cannot create symbolic links: the link case is not checked here");
        fs::remove_dir_all(&base).unwrap();
        return;
    }
    let before = walk(&store);
    let error = restore(&store, &set).expect_err("a linked file is refused");
    assert!(error.contains("a link"), "refused by another check: {error}");
    assert_eq!(walk(&store), before);
    fs::remove_dir_all(&base).unwrap();
}

/// No credential reaches the log, whichever persisted string carries it.
#[test]
fn credentials_are_refused_before_the_log() {
    let dir = temp_dir("review-secret");
    let engine = open(&dir);
    // Synthetic values only: shaped like credentials, valid for nothing.
    let secrets = [
        "Bearer sk-ant-api03-SYNTHETIC_SECRET_1234567890",
        "sk-ant-SYNTHETIC_SECRET_abcdefgh",
        "sk-SYNTHETICSECRETabcdefghijklmnopqrstuvwxyz0123",
        "xoxb-SYNTHETIC-SECRET-1234",
        "ghp_SYNTHETICSECRETabcdefghijklmnopqrstuvwxyz",
        "github_pat_SYNTHETIC_SECRET_abcdefghij",
        "glpat-SYNTHETIC_SECRET_abcdef",
        "AIzaSYNTHETIC_SECRET_abcdefghijklmnopqrstuvw",
        "eyJSYNTHETICxx.eyJSYNTHETICyy.signature",
        "api_key=SYNTHETIC_SECRET",
        "aacd_SYNTHETIC_SECRET_abcdef",
        "-----BEGIN PRIVATE KEY----- SYNTHETIC_SECRET",
        "Token SYNTHETIC_SECRET_12345",
    ];
    type Setter = fn(&mut Json, &str);
    let fields: [(&str, Setter); 8] = [
        ("source", |o, s| o["account"]["source"] = json!(s)),
        ("account_id", |o, s| o["account"]["account_id"] = json!(s)),
        ("plan", |o, s| o["account"]["plan"] = json!(s)),
        ("label", |o, s| o["windows"][0]["label"] = json!(s)),
        ("pool_label", |o, s| o["windows"][0]["pool_label"] = json!(s)),
        ("pool_id", |o, s| o["windows"][0]["pool_id"] = json!(s)),
        ("model_ids", |o, s| o["windows"][0]["model_ids"] = json!([s])),
        ("import_ref", |o, s| o["import_ref"] = json!(s)),
    ];
    let mut t = T0;
    for secret in secrets {
        for (name, set) in fields.iter() {
            t += MIN;
            let o = obs_with(t, 10.0, |o| set(o, secret));
            let answer = record(&engine, vec![o]);
            assert_eq!(answer["rejected"], 1, "{name} with {secret}: {answer}");
        }
    }
    // Ordinary text that merely mentions tokens, or holds a prefix inside a word, is stored.
    for (i, label) in [
        "Tokens per day",
        "Token requests",
        "Token budget for the week",
        "task-0123456789abcdef0123456789abcdef",
        "risk-0123456789abcdef0123456789abcdef",
        "Basic plan",
        "Bearer of the weekly quota",
    ]
    .iter()
    .enumerate()
    {
        let ordinary = obs_with(t + (i as i64 + 1) * MIN, 11.0 + i as f64, |o| {
            o["windows"][0]["label"] = json!(label);
            o["account"]["source"] = json!("Codex token quota on Mac");
        });
        assert_eq!(record(&engine, vec![ordinary])["sampled"], 1, "{label}");
    }
    engine.close().unwrap();
    drop(engine);
    for (path, bytes) in walk(&dir) {
        assert!(
            !bytes.windows(b"SYNTHETIC".len()).any(|w| w == b"SYNTHETIC"),
            "{path} holds a synthetic credential"
        );
    }
    fs::remove_dir_all(&dir).unwrap();
}

/// A lost frame at the start of a segment is reported with ordered bounds that hold it.
#[test]
fn loss_bounds_are_ordered_and_hold_the_lost_event() {
    let dir = temp_dir("review-bounds");
    {
        let engine = open(&dir);
        record(&engine, vec![obs(T0, 10.0)]);
        record(&engine, vec![obs(T0 + 5 * MIN, 20.0)]);
        engine.close().unwrap();
    }
    let sealed = find(&dir.join("log").join("readings"), ".seglog").pop().unwrap();
    let mut bytes = fs::read(&sealed).unwrap();
    let frames = aac_store::log::scan(&bytes).frames;
    assert_eq!(frames.iter().map(|f| f.seq).collect::<Vec<_>>(), vec![1, 2]);
    let end_of_first = frames[0].offset + frames[0].len - 1;
    bytes[end_of_first] ^= 0x01;
    fs::write(&sealed, &bytes).unwrap();
    let engine = open(&dir);
    assert_eq!(raw_starts(&engine), vec![T0 + 5 * MIN]);
    let findings = engine.status().unwrap()["findings"].clone();
    let loss = findings
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["kind"] == "projection_missing_events")
        .cloned()
        .unwrap_or_else(|| panic!("{findings:#}"));
    let (start, end) = (loss["start_seq"].as_u64().unwrap(), loss["end_seq"].as_u64().unwrap());
    assert!(start <= end, "{loss}");
    assert!(start <= 1 && 1 <= end, "the lost event 1 is inside {start}..={end}");
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// Burn over lost history is unknown, not zero; a measured zero stays zero (docs/store.md 7.2).
#[test]
fn burn_over_lost_history_is_null_and_a_measured_zero_is_zero() {
    let dir = temp_dir("review-burn");
    let series = |n: u32| json!({ "identity": format!("{n:064x}"), "window": "[\"five_hour\",\"rate_limit\",null]" });
    let burn = |engine: &Engine| {
        call(
            engine,
            "burnLong",
            json!({ "series": [series(1), series(9)], "spansMs": [HOUR, DAY], "nowMs": T0 + HOUR }),
        )
    };
    {
        let engine = open(&dir);
        // Flat at 30%: a measured zero.
        record(
            &engine,
            vec![obs(T0, 30.0), obs(T0 + 5 * MIN, 30.0), obs(T0 + 10 * MIN, 30.0)],
        );
        let measured = burn(&engine);
        assert_eq!(measured[0]["spans"][0]["consumed"], 0.0, "{measured}");
        // Known, but no reading in the span: ten days on, the last hour and day are unknown.
        let later = call(
            &engine,
            "burnLong",
            json!({ "series": [series(1)], "spansMs": [HOUR, DAY], "nowMs": T0 + 10 * DAY }),
        );
        for span in later[0]["spans"].as_array().unwrap() {
            assert!(
                span["consumed"].is_null(),
                "known series, no reading in the span: {later}"
            );
            assert!(span["rate"].is_null());
        }
        assert!(
            measured[1]["spans"][0]["consumed"].is_null(),
            "unknown series: {measured}"
        );
        engine.close().unwrap();
    }
    // Lose the only readings segment.
    for segment in find(&dir.join("log").join("readings"), ".seglog") {
        fs::remove_file(segment).unwrap();
    }
    let engine = open(&dir);
    let lost = burn(&engine);
    for span in lost[0]["spans"].as_array().unwrap() {
        assert!(span["consumed"].is_null(), "known but lost: {lost}");
        assert!(span["rate"].is_null());
    }
    assert!(lost[1]["spans"][0]["consumed"].is_null());
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// Filtered queries read only the selected series' rows: with unrelated metadata, open spans and
/// old account states made unreadable, they still answer.
#[test]
fn filtered_queries_read_only_the_selected_rows() {
    let dir = temp_dir("review-index");
    let other = |t: i64, used: f64| {
        obs_with(t, used, |o| {
            o["account"]["identity"] = json!(format!("{:064x}", 2));
            o["account"]["account_id"] = json!("codex:b");
            o["account"]["provider"] = json!("codex");
        })
    };
    let engine = open(&dir);
    // Account 1 has an old error state, then readings; account 2 is another provider.
    let failing = obs_with(T0 - 2 * DAY, 10.0, |o| {
        o["account"]["status"] = json!("error");
        o["windows"] = json!([]);
    });
    record(&engine, vec![obs(T0 - 3 * DAY, 5.0), failing]);
    record(
        &engine,
        vec![
            obs(T0, 10.0),
            other(T0, 50.0),
            obs(T0 + 5 * MIN, 20.0),
            other(T0 + 5 * MIN, 60.0),
        ],
    );
    let id1 = format!("{:064x}", 1);
    let queries = [
        (
            "seriesPoints",
            json!({ "identities": [id1], "fromMs": T0, "toMs": T0 + HOUR, "resolution": "raw" }),
        ),
        (
            "seriesPoints",
            json!({ "provider": "claude", "fromMs": T0, "toMs": T0 + HOUR, "resolution": "hour" }),
        ),
        (
            "seriesList",
            json!({ "identities": [id1], "fromMs": T0, "toMs": T0 + HOUR }),
        ),
        (
            "seriesList",
            json!({ "provider": "claude", "fromMs": T0, "toMs": T0 + HOUR }),
        ),
        (
            "historyRecords",
            json!({ "identities": [id1], "fromMs": T0, "toMs": T0 + HOUR, "nowMs": T0 + HOUR, "thinning": { "kind": "none" } }),
        ),
    ];
    let expected: Vec<Json> = queries.iter().map(|(m, a)| call(&engine, m, a.clone())).collect();
    assert!(expected[0]["series"].as_array().unwrap().len() == 1);
    engine.close().unwrap();
    drop(engine);
    // Unrelated rows a filtered query has no reason to read: account 2's series, open span and
    // account; account 1's state from three days before the range.
    {
        let database = redb::Database::create(dir.join("aac.redb")).unwrap();
        let txn = database.begin_write().unwrap();
        let bad: &[u8] = &[105, 0, 0];
        let a2 = {
            let rev = txn.open_table(aac_store::db::DICT_REV).unwrap();
            aac_store::db::lookup_id(&rev, aac_store::db::KIND_ACCOUNT, &format!("{:064x}", 2))
                .unwrap()
                .unwrap()
        };
        let a1 = {
            let rev = txn.open_table(aac_store::db::DICT_REV).unwrap();
            aac_store::db::lookup_id(&rev, aac_store::db::KIND_ACCOUNT, &id1)
                .unwrap()
                .unwrap()
        };
        // A `latest` value long enough to be decoded as a span, with a bad row version.
        let mut bad_latest = vec![0u8; 16];
        bad_latest.extend_from_slice(&[105u8; 16]);
        for def in [aac_store::db::SERIES, aac_store::db::LATEST] {
            let keys: Vec<Vec<u8>> = {
                let table = txn.open_table(def).unwrap();
                let prefix = a2.to_be_bytes();
                table
                    .range(prefix.as_slice()..)
                    .unwrap()
                    .map(|i| i.unwrap().0.value().to_vec())
                    .filter(|k| k.starts_with(&prefix))
                    .collect()
            };
            assert!(!keys.is_empty());
            let mut table = txn.open_table(def).unwrap();
            let value: &[u8] = if def.name() == "latest" { &bad_latest } else { bad };
            for key in keys {
                table.insert(key.as_slice(), value).unwrap();
            }
        }
        txn.open_table(aac_store::db::ACCOUNT)
            .unwrap()
            .insert(a2.to_be_bytes().as_slice(), bad)
            .unwrap();
        let first_state: Vec<u8> = {
            let table = txn.open_table(aac_store::db::ACCOUNT_STATE).unwrap();
            let prefix = a1.to_be_bytes();
            let key = table
                .range(prefix.as_slice()..)
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .0
                .value()
                .to_vec();
            assert!(key.starts_with(&prefix));
            key
        };
        txn.open_table(aac_store::db::ACCOUNT_STATE)
            .unwrap()
            .insert(first_state.as_slice(), bad)
            .unwrap();
        txn.commit().unwrap();
    }
    let engine = open(&dir);
    for ((method, args), want) in queries.iter().zip(&expected) {
        let got = ops::call(&engine, method, &args.to_string());
        assert_eq!(got.as_ref().ok(), Some(want), "{method} {args}: {got:?}");
    }
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// A window filter reads only that window's series: another window's metadata and open spans
/// are unreadable here.
#[test]
fn a_window_filter_reads_only_that_windows_series() {
    let dir = temp_dir("review-window");
    let engine = open(&dir);
    let two = |t: i64, used: f64| {
        obs_with(t, used, |o| {
            let mut week = o["windows"][0].clone();
            week["identity"] = json!("[\"seven_day\",\"rate_limit\",null]");
            week["key"] = json!("seven_day");
            o["windows"].as_array_mut().unwrap().push(week);
        })
    };
    record(&engine, vec![two(T0, 10.0), two(T0 + 5 * MIN, 20.0)]);
    let id1 = format!("{:064x}", 1);
    let queries = [
        (
            "seriesPoints",
            json!({ "windowKey": "five_hour", "fromMs": T0, "toMs": T0 + HOUR, "resolution": "raw" }),
        ),
        (
            "seriesList",
            json!({ "windowKey": "five_hour", "fromMs": T0, "toMs": T0 + HOUR }),
        ),
        (
            "seriesPoints",
            json!({ "identities": [id1], "windowKey": "five_hour", "fromMs": T0, "toMs": T0 + HOUR, "resolution": "hour" }),
        ),
        (
            "seriesList",
            json!({ "provider": "claude", "windowKey": "five_hour", "fromMs": T0, "toMs": T0 + HOUR }),
        ),
    ];
    let expected: Vec<Json> = queries.iter().map(|(m, a)| call(&engine, m, a.clone())).collect();
    assert_eq!(expected[1].as_array().unwrap().len(), 1);
    engine.close().unwrap();
    drop(engine);
    {
        let database = redb::Database::create(dir.join("aac.redb")).unwrap();
        let txn = database.begin_write().unwrap();
        let (a1, week) = {
            let rev = txn.open_table(aac_store::db::DICT_REV).unwrap();
            (
                aac_store::db::lookup_id(&rev, aac_store::db::KIND_ACCOUNT, &id1)
                    .unwrap()
                    .unwrap(),
                aac_store::db::lookup_id(&rev, aac_store::db::KIND_WINDOW, "[\"seven_day\",\"rate_limit\",null]")
                    .unwrap()
                    .unwrap(),
            )
        };
        let key = aac_store::db::Key::new().u32(a1).u32(week);
        let mut bad_latest = vec![0u8; 16];
        bad_latest.extend_from_slice(&[105u8; 16]);
        txn.open_table(aac_store::db::SERIES)
            .unwrap()
            .insert(key.as_slice(), [105u8, 0, 0].as_slice())
            .unwrap();
        txn.open_table(aac_store::db::LATEST)
            .unwrap()
            .insert(key.as_slice(), bad_latest.as_slice())
            .unwrap();
        txn.commit().unwrap();
    }
    let engine = open(&dir);
    for ((method, args), want) in queries.iter().zip(&expected) {
        let got = ops::call(&engine, method, &args.to_string());
        assert_eq!(got.as_ref().ok(), Some(want), "{method} {args}: {got:?}");
    }
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// Opens the store named by `AAC_CHILD_DIR` and closes it: run in a child process that the kill
/// test stops part way.
#[test]
#[ignore]
fn child_opens_the_store() {
    if let Ok(dir) = std::env::var("AAC_CHILD_DIR") {
        let engine = open(Path::new(&dir));
        engine.close().unwrap();
    }
}

const MANY: usize = 20_000;

/// A version-1 store whose redb keeps `MANY` loss findings not yet in the ledger: what a kill
/// after the projection committed them, and before the open ledgered them, leaves behind.
fn many_unledgered(dir: &Path) {
    {
        let engine = open(dir);
        record(&engine, vec![obs(T0, 10.0)]);
        engine.close().unwrap();
    }
    let database = redb::Database::create(dir.join("aac.redb")).unwrap();
    let txn = database.begin_write().unwrap();
    {
        let mut table = txn.open_table(aac_store::db::FINDING).unwrap();
        for i in 0..MANY as u64 {
            let finding =
                json!({ "kind": "segment_missing", "detail": "skipped", "start_seq": 1000 + i, "end_seq": 1000 + i });
            let value = json!({ "finding": finding, "ledgered": false });
            let bytes = aac_store::msgpack::encode(&aac_store::msgpack::from_json(&value).unwrap()).unwrap();
            table
                .insert(
                    format!("segment_missing:{}:{}", 1000 + i, 1000 + i).as_str(),
                    bytes.as_slice(),
                )
                .unwrap();
        }
    }
    txn.commit().unwrap();
}

/// The ledger's `store.integrity_detected` events for those findings, by sequence.
fn ledgered_findings(dir: &Path) -> Vec<u64> {
    let (log, _) = aac_store::log::Log::open(&dir.join("log").join("ledger"), "ledger", "l").unwrap();
    let mut out = Vec::new();
    log.read_after(None, |e| {
        if e.event_type == "store.integrity_detected" {
            let payload = aac_store::msgpack::to_json(&e.payload);
            if payload["kind"] == "segment_missing" {
                out.push(payload["start_seq"].as_u64().unwrap());
            }
        }
        Ok(())
    })
    .unwrap();
    out
}

fn missing(engine: &Engine) -> usize {
    kinds(engine).iter().filter(|k| *k == "segment_missing").count()
}

/// More findings than one batch holds: the store opens, ledgers each once, and a reopen adds
/// nothing to the ledger.
#[test]
fn a_store_with_twenty_thousand_unledgered_findings_opens() {
    let dir = temp_dir("review-many");
    many_unledgered(&dir);
    let engine = open(&dir);
    assert_eq!(missing(&engine), MANY);
    assert_eq!(engine.status().unwrap()["state"], "degraded");
    engine.close().unwrap();
    drop(engine);
    assert_eq!(ledgered_findings(&dir).len(), MANY);
    let engine = open(&dir);
    assert_eq!(missing(&engine), MANY);
    engine.close().unwrap();
    drop(engine);
    assert_eq!(ledgered_findings(&dir).len(), MANY, "a reopen ledgers nothing again");
    // A rebuild from the logs restores them all from the ledger.
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = open(&dir);
    assert_eq!(missing(&engine), MANY);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&dir).unwrap();
}

/// A kill while the open ledgers its findings leaves nothing half written: the next open
/// discloses every finding, each ledgered exactly once.
#[test]
fn a_kill_while_ledgering_many_findings_recovers() {
    let dir = temp_dir("review-many-kill");
    many_unledgered(&dir);
    let exe = std::env::current_exe().unwrap();
    let mut stopped = 0;
    for wait_ms in [300u64, 800, 1_500, 2_500, 4_000] {
        let mut child = std::process::Command::new(&exe)
            .args([
                "--ignored",
                "--exact",
                "child_opens_the_store",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("AAC_CHILD_DIR", &dir)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(wait_ms));
        if child.try_wait().unwrap().is_none() {
            child.kill().unwrap();
            stopped += 1;
        }
        child.wait().unwrap();
        let mut seen = ledgered_findings(&dir);
        let all = seen.len();
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(
            seen.len(),
            all,
            "after a kill at {wait_ms} ms: no finding is ledgered twice"
        );
    }
    assert!(stopped > 0, "at least one open was killed part way");
    let engine = open(&dir);
    assert_eq!(missing(&engine), MANY, "every finding is disclosed");
    engine.close().unwrap();
    drop(engine);
    let mut seen = ledgered_findings(&dir);
    assert_eq!(seen.len(), MANY);
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), MANY, "each exactly once");
    fs::remove_dir_all(&dir).unwrap();
}

/// A store written by one of the branch's unreleased test builds (logs or a redb file, but no
/// release marker), or one marked with another release, is refused with the owner's message and
/// left exactly as it was; nothing is deleted. A new folder starts a version-1 store.
#[test]
fn a_store_from_an_unreleased_test_build_is_refused_untouched() {
    let base = temp_dir("review-unreleased");
    // A version-1 store, then the same files as a test build left them: no marker.
    let dev = base.join("dev");
    {
        let engine = open(&dev);
        record(&engine, vec![obs(T0, 10.0)]);
        engine.close().unwrap();
    }
    assert_eq!(fs::read_to_string(dev.join("release")).unwrap(), "1");
    fs::remove_file(dev.join("release")).unwrap();
    let only_log = base.join("only-log");
    common::copy_dir(&dev.join("log"), &only_log.join("log"));
    let other = base.join("other");
    common::copy_dir(&dev, &other);
    fs::write(other.join("release"), "0").unwrap();
    for store in [&dev, &only_log, &other] {
        let before = walk(store);
        let refused = ops::open_engine(store.to_str().unwrap(), None, None);
        let error = refused.err().expect("a test-build store is refused");
        assert_eq!(error.code, "unreleased_store");
        assert_eq!(
            error.message,
            "This store was written by an unreleased test build; move it aside and AAC will start a new one."
        );
        assert_eq!(walk(store), before, "{}: left untouched", store.display());
    }
    // A newer release is reported as newer, not unreleased.
    let newer = base.join("newer");
    common::copy_dir(&dev, &newer);
    fs::write(newer.join("release"), "2").unwrap();
    let error = ops::open_engine(newer.to_str().unwrap(), None, None).err().unwrap();
    assert_eq!(error.code, "newer_store");
    // Moved aside, a new store starts in its place.
    let fresh = base.join("fresh");
    let engine = open(&fresh);
    assert_eq!(fs::read_to_string(fresh.join("release")).unwrap(), "1");
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(&base).unwrap();
}

/// A backup that does not say it holds a version-1 store is refused before anything changes.
#[test]
fn a_backup_from_an_unreleased_test_build_is_refused() {
    let base = temp_dir("review-unreleased-backup");
    let store = base.join("store");
    let backups = base.join("backups");
    {
        let engine = open(&store);
        record(&engine, vec![obs(T0, 10.0)]);
        call(&engine, "maintain", json!({ "nowMs": now_ms() + 2 * DAY as u64 }));
        let made = call(
            &engine,
            "backup",
            json!({ "roots": [backups.to_str().unwrap()], "withRedb": true }),
        );
        assert!(made["id"].is_string());
        engine.close().unwrap();
    }
    let set = fs::read_dir(backups.join("sets"))
        .unwrap()
        .flatten()
        .next()
        .unwrap()
        .path();
    let mut manifest: Json = serde_json::from_str(&fs::read_to_string(set.join("backup.json")).unwrap()).unwrap();
    assert_eq!(manifest["storeRelease"], 1);
    manifest.as_object_mut().unwrap().remove("storeRelease");
    fs::write(set.join("backup.json"), manifest.to_string()).unwrap();
    let before = walk(&store);
    let error = restore(&store, &set).expect_err("a backup without its release is refused");
    assert!(error.starts_with("unreleased_store: "), "{error}");
    assert_eq!(walk(&store), before);
    fs::remove_dir_all(&base).unwrap();
}
