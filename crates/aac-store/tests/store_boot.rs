//! Boot and Store status costs (docs/store.md 2.6, 4.3): a clean close lets the next open trust
//! the logs it recorded instead of re-reading every sealed segment, and a status read never opens
//! a write transaction or walks the file.
mod common;
use aac_store::engine::{Engine, LogId, VERIFIED_GENERATION_META};
use aac_store::event::Event;
use aac_store::ops;
use common::{find, temp_dir};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, None).unwrap()
}

fn write_events(engine: &Engine, count: usize) {
    engine
        .commit(LogId::Ledger, |batch| {
            for i in 0..count {
                let payload = aac_store::msgpack::from_json(&json!({ "n": i, "pad": "x".repeat(64) }))?;
                let mut event = Event::new("test.boot", 1_794_564_000_000 + i as u64, payload);
                event.idempotency_key = None;
                batch.push(event)?;
            }
            Ok(())
        })
        .unwrap();
}

fn findings(engine: &Engine) -> Vec<Value> {
    engine.status().unwrap()["findings"].as_array().unwrap().clone()
}

fn kinds(engine: &Engine) -> Vec<String> {
    findings(engine)
        .iter()
        .map(|f| f["kind"].as_str().unwrap().to_string())
        .collect()
}

/// The ledger's sealed segment files, oldest first.
fn sealed(dir: &Path) -> Vec<std::path::PathBuf> {
    let mut files = find(&dir.join("log").join("ledger"), ".seglog");
    files.sort();
    files
}

/// Flips one payload byte of the newest sealed ledger segment, keeping its length.
fn damage_same_size(dir: &Path) {
    let path = sealed(dir).pop().expect("a sealed ledger segment");
    let mut bytes = fs::read(&path).unwrap();
    let at = bytes.len() - 20;
    bytes[at] ^= 0x40;
    fs::write(&path, &bytes).unwrap();
}

fn meta(engine: &Engine, key: &str) -> Option<String> {
    engine.get_meta(key).unwrap()
}

fn open_check(engine: &Engine) -> Value {
    engine.status().unwrap()["integrity"]["openCheck"].clone()
}

#[test]
fn a_clean_close_lets_the_next_open_trust_the_recorded_logs() {
    let dir = temp_dir("boot-clean-fast");
    let engine = open(&dir);
    write_events(&engine, 200);
    // The first open of a store has nothing recorded: everything is read.
    assert_eq!(open_check(&engine), json!({ "readings": "full", "ledger": "full" }));
    engine.close().unwrap();
    drop(engine);
    let engine = open(&dir);
    assert_eq!(
        open_check(&engine),
        json!({ "readings": "clean_close", "ledger": "clean_close" })
    );
    assert!(
        !kinds(&engine).iter().any(|k| aac_store::engine::is_loss(k)),
        "{:#?}",
        findings(&engine)
    );
    // Used once: the record is gone as soon as this run starts.
    assert_eq!(meta(&engine, VERIFIED_GENERATION_META), None);
    engine.close().unwrap();
    drop(engine);
    // A segment rewritten while the store was closed is read again and its damage found.
    damage_same_size(&dir);
    let engine = open(&dir);
    assert_eq!(open_check(&engine)["ledger"], "full");
    assert!(
        kinds(&engine).iter().any(|k| k == "segment_degraded"),
        "{:#?}",
        findings(&engine)
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_same_size_rewrite_with_its_old_modified_time_is_still_read_in_full() {
    let dir = temp_dir("boot-mtime-full");
    let engine = open(&dir);
    write_events(&engine, 200);
    engine.close().unwrap();
    drop(engine);
    let path = sealed(&dir).pop().unwrap();
    let modified = fs::metadata(&path).unwrap().modified().unwrap();
    damage_same_size(&dir);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(modified)
        .unwrap();
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), modified);
    let engine = open(&dir);
    assert_eq!(open_check(&engine)["ledger"], "full");
    assert!(
        kinds(&engine).iter().any(|k| k == "segment_degraded"),
        "{:#?}",
        findings(&engine)
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn an_unclean_stop_is_checked_in_full_even_after_an_earlier_clean_close() {
    let dir = temp_dir("boot-unclean-full");
    let engine = open(&dir);
    write_events(&engine, 200);
    engine.close().unwrap();
    drop(engine);
    // This run consumes the clean close's record and then stops without closing.
    let engine = open(&dir);
    write_events(&engine, 10);
    drop(engine);
    damage_same_size(&dir);
    let engine = open(&dir);
    let seen = kinds(&engine);
    assert!(seen.iter().any(|k| k == "unclean_shutdown"), "{seen:?}");
    assert!(seen.iter().any(|k| k == "segment_degraded"), "{:#?}", findings(&engine));
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_segment_file_changed_in_size_after_a_clean_close_is_checked_in_full() {
    let dir = temp_dir("boot-size-full");
    let engine = open(&dir);
    write_events(&engine, 200);
    engine.close().unwrap();
    drop(engine);
    let path = sealed(&dir).pop().unwrap();
    let mut bytes = fs::read(&path).unwrap();
    bytes.extend_from_slice(b"tail");
    fs::write(&path, &bytes).unwrap();
    let engine = open(&dir);
    assert!(
        kinds(&engine).iter().any(|k| k == "segment_degraded"),
        "{:#?}",
        findings(&engine)
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(dir).unwrap();
}

/// The clean close's record, read from the closed redb file without opening the store.
fn recorded(dir: &Path) -> Option<Value> {
    use redb::ReadableDatabase;
    let db = redb::Database::open(dir.join("aac.redb")).unwrap();
    let read = db.begin_read().unwrap();
    let table = read.open_table(aac_store::db::META).unwrap();
    let value = table.get(VERIFIED_GENERATION_META).unwrap()?;
    Some(serde_json::from_slice(value.value()).unwrap())
}

#[test]
fn a_store_closed_with_a_degraded_log_records_no_generation_for_it() {
    let dir = temp_dir("boot-degraded-full");
    let engine = open(&dir);
    write_events(&engine, 200);
    engine.close().unwrap();
    drop(engine);
    let first = recorded(&dir).expect("a clean close records its logs");
    assert!(
        first["logs"]["ledger"].is_string() && first["checkpoints"].is_string(),
        "{first:#}"
    );
    let engine = open(&dir);
    damage_same_size(&dir);
    engine.verify().unwrap();
    engine.close().unwrap();
    drop(engine);
    let second = recorded(&dir).expect("a clean close records the plain logs");
    assert!(second["logs"]["ledger"].is_null(), "{second:#}");
    assert!(second["logs"]["readings"].is_string(), "{second:#}");
    let engine = open(&dir);
    assert!(
        kinds(&engine).iter().any(|k| k == "segment_degraded"),
        "{:#?}",
        findings(&engine)
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_status_read_never_waits_for_a_writer() {
    let dir = temp_dir("status-no-write-txn");
    let engine = std::sync::Arc::new(open(&dir));
    write_events(&engine, 50);
    let (held, release) = (std::sync::mpsc::channel::<()>(), std::sync::mpsc::channel::<()>());
    let holder = {
        let engine = std::sync::Arc::clone(&engine);
        let (held_tx, release_rx) = (held.0, release.1);
        std::thread::spawn(move || {
            engine
                .with_db(|db| {
                    let txn = db.begin_write()?;
                    held_tx.send(()).unwrap();
                    let _ = release_rx.recv_timeout(Duration::from_secs(10));
                    txn.abort()?;
                    Ok(())
                })
                .unwrap();
        })
    };
    held.1.recv().unwrap();
    let started = Instant::now();
    let status = ops::call(&engine, "status", "{}").unwrap();
    let took = started.elapsed();
    let _ = release.0.send(());
    holder.join().unwrap();
    assert!(took < Duration::from_secs(2), "status waited {took:?} for the writer");
    assert!(status["redbStats"].is_object());
    assert!(status["capture"].is_object());
    std::sync::Arc::try_unwrap(engine).ok().unwrap().close().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn measured_stats_are_kept_across_a_restart_and_named_by_their_time() {
    let dir = temp_dir("status-measured");
    let engine = open(&dir);
    write_events(&engine, 50);
    let before = ops::call(&engine, "status", "{}").unwrap();
    assert_eq!(before["redbStats"], json!({ "measuredAt": null }), "{before:#}");
    assert!(before["capture"]["contentBytes"].is_null());
    assert!(engine.stats_due(aac_store::engine::now_ms()));
    engine.refresh_stats().unwrap();
    assert!(!engine.stats_due(aac_store::engine::now_ms()));
    let after = ops::call(&engine, "status", "{}").unwrap();
    let measured_at = after["redbStats"]["measuredAt"].as_u64().expect("measured");
    assert!(after["redbStats"]["allocatedPages"].as_u64().unwrap() > 0);
    assert!(after["redbStats"]["pageSize"].as_u64().unwrap() > 0);
    assert_eq!(after["capture"]["bytesMeasuredAt"], measured_at);
    assert!(after["capture"]["contentBytes"].as_u64().is_some());
    // Writes after the measurement keep it, with its time, until the next refresh is due.
    write_events(&engine, 50);
    assert_eq!(
        ops::call(&engine, "status", "{}").unwrap()["redbStats"],
        after["redbStats"]
    );
    assert!(aac_store::read_status::from_writer(&after).is_some());
    engine.close().unwrap();
    drop(engine);
    let engine = open(&dir);
    assert_eq!(
        ops::call(&engine, "status", "{}").unwrap()["redbStats"],
        after["redbStats"]
    );
    assert!(!engine.stats_due(aac_store::engine::now_ms()));
    assert!(!engine.stats_due(measured_at + aac_store::engine::STATS_REFRESH_MS - 1));
    assert!(engine.stats_due(measured_at + aac_store::engine::STATS_REFRESH_MS));
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn housekeeping_meta_does_not_change_the_analytics_answer_generation() {
    let dir = temp_dir("status-generation");
    let engine = open(&dir);
    write_events(&engine, 20);
    let generation = aac_store::analytics_cache::answer_generation(&engine).unwrap();
    engine.refresh_stats().unwrap();
    assert_eq!(
        aac_store::analytics_cache::answer_generation(&engine).unwrap(),
        generation
    );
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(dir).unwrap();
}
