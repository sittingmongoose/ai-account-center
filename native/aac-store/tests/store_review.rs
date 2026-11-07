//! Regressions from the first code review of the store: projections over a log that lost
//! acknowledged frames, restore paths outside the store, and credentials in persisted strings.

mod common;

use aac_store::engine::{Engine, now_ms};
use aac_store::ops;
use common::{copy_dir, find, temp_dir};
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
    // The loss is recorded in the ledger, and the next open does not rebuild again.
    engine.close().unwrap();
    drop(engine);
    let engine = open(&dir);
    assert_eq!(raw_starts(&engine), rebuilt);
    assert!(
        !kinds(&engine)
            .iter()
            .any(|k| k == "projection_missing_events" || k == "projector_rebuilt")
    );
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
        json!({ "schema": "aac-store-backup@1", "id": id, "storeSchemaVersion": 1, "withRedb": false,
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

/// Writes events straight into the readings log in the log format, as an older build did.
fn append_raw(dir: &Path, events: Vec<(&str, Json)>) -> Vec<u64> {
    let (mut log, _) = aac_store::log::Log::open(&dir.join("log").join("readings"), "readings", "r").unwrap();
    let mut seqs = Vec::new();
    for (event_type, payload) in events {
        let mut event =
            aac_store::event::Event::new(event_type, now_ms(), aac_store::msgpack::from_json(&payload).unwrap());
        log.assign(&mut event).unwrap();
        seqs.push(event.sequence_id);
        log.write(&[event]).unwrap();
    }
    seqs
}

/// An event an older build accepted, which today's checks refuse, is set aside and disclosed;
/// the rest of the history is still served, and new appends are still refused.
#[test]
fn an_old_event_refused_today_is_set_aside_not_fatal() {
    let dir = temp_dir("review-old-event");
    let bad = obs_with(T0, 10.0, |o| {
        o["windows"][0]["label"] = json!("sk-ant-SYNTHETIC_SECRET_abcdefgh")
    });
    let other = obs_with(T0, 30.0, |o| {
        o["account"]["identity"] = json!(format!("{:064x}", 2));
        o["account"]["account_id"] = json!("claude:a2");
    });
    let seqs = append_raw(
        &dir,
        vec![
            ("usage.sampled", json!({ "observations": [bad] })),
            ("usage.sampled", json!({ "observations": [obs(T0 + 5 * MIN, 20.0)] })),
            ("usage.sampled", json!({ "observations": [other] })),
        ],
    );
    let engine = open(&dir);
    let starts = raw_starts(&engine);
    assert_eq!(
        starts,
        vec![T0 + 5 * MIN, T0],
        "every event but the refused one is served"
    );
    let findings = engine.status().unwrap()["findings"].clone();
    let set_aside: Vec<&Json> = findings
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "event_quarantined")
        .collect();
    assert_eq!(set_aside.len(), 1, "{findings:#}");
    assert_eq!(set_aside[0]["start_seq"], seqs[0]);
    assert_eq!(set_aside[0]["end_seq"], seqs[0]);
    assert!(
        !set_aside[0]["detail"].as_str().unwrap().contains("SYNTHETIC"),
        "the finding never repeats it"
    );
    assert_eq!(engine.status().unwrap()["state"], "degraded");
    // New appends with the same text are still refused.
    let again = obs_with(T0 + 10 * MIN, 40.0, |o| {
        o["windows"][0]["label"] = json!("sk-ant-SYNTHETIC_SECRET_abcdefgh")
    });
    assert_eq!(record(&engine, vec![again])["rejected"], 1);
    engine.close().unwrap();
    drop(engine);
    // A rebuild sets it aside the same way.
    fs::remove_file(dir.join("aac.redb")).unwrap();
    let engine = open(&dir);
    assert_eq!(raw_starts(&engine), starts);
    engine.close().unwrap();
    drop(engine);
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
