//! Analytics usage history (docs/store.md section 12): dedupe, replace and raise, settled hours,
//! backfill, replay after day close, ranges across months, gaps and opt-in retention. Every store
//! lives in a temporary folder; never the real home.

mod common;

use aac_store::activity;
use aac_store::engine::Engine;
use aac_store::ops;
use common::temp_dir;
use serde_json::{Value as Json, json};
use std::path::Path;

const H: u64 = 3_600_000;
const D: u64 = 86_400_000;
/// 2025-08-01T00:00:00Z: every read below is in the past, as the store clamps read times to its
/// clock and leaves out what is dated after it.
const T0: u64 = 1_754_006_400_000;
const SESSION_A: &str = "0123456789abcdef";
const SESSION_B: &str = "fedcba9876543210";

fn iso(ms: u64) -> String {
    aac_store::usage::iso(ms as i64)
}

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, None).unwrap()
}

fn call(engine: &Engine, method: &str, args: Json) -> Json {
    ops::call(engine, method, &args.to_string()).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn cell(hour: u64, session: Option<&str>, model: &str, tokens: u64) -> Json {
    json!({
        "hour": iso(hour), "sessionKey": session, "route": "", "modelRaw": model,
        "inputTokens": tokens, "outputTokens": 0, "cacheCreationTokens": 0, "cacheReadTokens": 0,
        "costUsd": tokens as f64 / 1e6, "fallbackCostUsd": 0.0, "events": 1,
    })
}

fn report(
    computer: &str,
    read_at: u64,
    state: &str,
    complete: bool,
    cover_from: Option<u64>,
    cells: Vec<Json>,
) -> Json {
    json!({
        "schema": "aac.usage-report.v1", "computer": computer, "tool": "codex", "readAt": iso(read_at),
        "state": state, "complete": complete, "coverFrom": cover_from.map(iso), "cells": cells,
    })
}

/// A complete live read at `read_at` covering the 31 days before it.
fn complete(computer: &str, read_at: u64, cells: Vec<Json>) -> Json {
    let from = (read_at - 31 * D) / H * H;
    report(computer, read_at, "ok", true, Some(from), cells)
}

fn ingest(engine: &Engine, reports: Vec<Json>) -> Json {
    call(engine, "activityIngest", json!({ "reports": reports }))
}

/// Every stored cell as `computer tool hour session model tokens`.
fn dump(engine: &Engine) -> Vec<String> {
    activity::dump_cells(engine)
        .unwrap()
        .into_iter()
        .map(|(c, t, e)| format!("{c} {t} {} {:?} {} {}", e.h, e.s, e.m, e.i + e.o + e.cw + e.cr))
        .collect()
}

fn tokens_in(engine: &Engine, from: u64, to: u64) -> u64 {
    let answer = call(engine, "activityRange", json!({ "fromMs": from, "toMs": to }));
    answer["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[5].as_u64().unwrap())
        .sum()
}

#[test]
fn the_same_report_twice_writes_nothing() {
    let dir = temp_dir("act-dedupe");
    let engine = open(&dir);
    let read = T0 + 40 * D;
    let r = complete(
        "ubuntu",
        read,
        vec![
            cell(read - 5 * H, Some(SESSION_A), "gpt-6", 100),
            cell(read - 4 * H, Some(SESSION_A), "gpt-6", 50),
        ],
    );
    let first = ingest(&engine, vec![r.clone()]);
    assert_eq!(first["put"], 2);
    assert!(first["events"].as_u64().unwrap() >= 1);
    let again = ingest(&engine, vec![r]);
    assert_eq!(
        (
            again["put"].as_u64(),
            again["deleted"].as_u64(),
            again["events"].as_u64()
        ),
        (Some(0), Some(0), Some(0))
    );
    assert_eq!(tokens_in(&engine, read - D, read), 150);
}

#[test]
fn a_reread_replaces_and_never_adds() {
    let dir = temp_dir("act-replace");
    let engine = open(&dir);
    let read = T0 + 40 * D;
    let hour = read - 3 * H;
    ingest(
        &engine,
        vec![complete(
            "ubuntu",
            read,
            vec![cell(hour, Some(SESSION_A), "gpt-6", 100)],
        )],
    );
    ingest(
        &engine,
        vec![complete(
            "ubuntu",
            read + 60_000,
            vec![cell(hour, Some(SESSION_A), "gpt-6", 150)],
        )],
    );
    assert_eq!(
        tokens_in(&engine, hour, hour),
        150,
        "a re-read replaces the cell's totals"
    );
    // the same hour read again without the cell: a measured zero inside a complete read
    let after = ingest(&engine, vec![complete("ubuntu", read + 120_000, vec![])]);
    assert_eq!(after["deleted"], 1);
    assert_eq!(tokens_in(&engine, hour, hour), 0);
}

#[test]
fn a_partial_read_only_raises() {
    let dir = temp_dir("act-raise");
    let engine = open(&dir);
    let read = T0 + 40 * D;
    let hour = read - 3 * H;
    ingest(
        &engine,
        vec![complete(
            "ubuntu",
            read,
            vec![cell(hour, Some(SESSION_A), "gpt-6", 100)],
        )],
    );
    let lower = report(
        "ubuntu",
        read + 60_000,
        "partial",
        false,
        None,
        vec![cell(hour, Some(SESSION_A), "gpt-6", 40)],
    );
    assert_eq!(ingest(&engine, vec![lower])["put"], 0);
    assert_eq!(tokens_in(&engine, hour, hour), 100);
    let higher = report(
        "ubuntu",
        read + 120_000,
        "partial",
        false,
        None,
        vec![cell(hour, Some(SESSION_A), "gpt-6", 120)],
    );
    assert_eq!(ingest(&engine, vec![higher])["put"], 1);
    assert_eq!(tokens_in(&engine, hour, hour), 120);
    // a partial read never removes a cell
    let none = report("ubuntu", read + 180_000, "partial", false, None, vec![]);
    assert_eq!(ingest(&engine, vec![none])["deleted"], 0);
    assert_eq!(tokens_in(&engine, hour, hour), 120);
}

#[test]
fn settled_hours_survive_deleted_logs() {
    let dir = temp_dir("act-settle");
    let engine = open(&dir);
    let read = T0 + 40 * D;
    let old = read - 10 * D;
    ingest(
        &engine,
        vec![complete("ubuntu", read, vec![cell(old, Some(SESSION_A), "gpt-6", 100)])],
    );
    // the tool cleaned up its old log: the next complete read no longer holds the hour
    let later = ingest(&engine, vec![complete("ubuntu", read + 2 * H, vec![])]);
    assert_eq!(
        later["deleted"], 0,
        "an hour behind the 72-hour settle line is never lowered"
    );
    assert_eq!(tokens_in(&engine, old, old), 100);
    // nor raised by a live read
    let raise = complete("ubuntu", read + 4 * H, vec![cell(old, Some(SESSION_A), "gpt-6", 900)]);
    assert_eq!(ingest(&engine, vec![raise])["put"], 0);
    assert_eq!(tokens_in(&engine, old, old), 100);
}

#[test]
fn backfill_adds_old_history_and_never_lowers() {
    let dir = temp_dir("act-backfill");
    let engine = open(&dir);
    let read = T0 + 300 * D;
    let recent = read - 2 * D;
    let ancient = read - 200 * D;
    ingest(
        &engine,
        vec![complete(
            "ubuntu",
            read,
            vec![cell(recent, Some(SESSION_A), "gpt-6", 100)],
        )],
    );
    let mut deep = report(
        "ubuntu",
        read + 60_000,
        "ok",
        true,
        Some(ancient / H * H),
        vec![
            cell(ancient, Some(SESSION_B), "gpt-5", 70),
            cell(recent, Some(SESSION_A), "gpt-6", 60),
        ],
    );
    deep["kind"] = json!("backfill");
    let first = ingest(&engine, vec![deep.clone()]);
    assert_eq!(first["put"], 1, "only the missing old hour is added");
    assert_eq!(tokens_in(&engine, ancient, ancient), 70);
    assert_eq!(tokens_in(&engine, recent, recent), 100, "a backfill never lowers");
    let again = ingest(&engine, vec![deep]);
    assert_eq!((again["put"].as_u64(), again["events"].as_u64()), (Some(0), Some(0)));
    // the deep read's coverage reaches back to the oldest hour it found
    let sources = call(
        &engine,
        "activitySources",
        json!({ "fromMs": ancient - D, "toMs": read }),
    );
    let ubuntu = &sources["computers"][0];
    assert_eq!(ubuntu["oldestMs"].as_u64(), Some(ancient));
    assert_eq!(call(&engine, "activityOldest", json!({})).as_u64(), Some(ancient));
}

#[test]
fn a_rebuild_from_the_logs_gives_the_same_tables() {
    let dir = temp_dir("act-replay");
    {
        let engine = open(&dir);
        let start = T0 + 40 * D;
        // a complete read holds every hour of its window, as the collectors' reads do
        let mut cells = Vec::new();
        for day in 0..6u64 {
            let read = start + day * D + 12 * H;
            cells.push(cell(read - 2 * H, Some(SESSION_A), "gpt-6", 100 + day));
            cells.push(cell(read - H, Some(SESSION_B), "claude-opus-5-5", 10 * day + 1));
            ingest(
                &engine,
                vec![
                    complete("ubuntu", read, cells.clone()),
                    complete("mac", read, cells.clone()),
                ],
            );
        }
        let now = start + 9 * D;
        let maintained = call(&engine, "maintain", json!({ "nowMs": now }));
        assert!(
            !maintained["activity"]["closed"]["closed"]
                .as_array()
                .unwrap()
                .is_empty(),
            "{maintained}"
        );
        // a late change to a closed day (a computer that was off) goes to the ledger
        let mut late = report(
            "nas1",
            now,
            "partial",
            false,
            None,
            vec![cell(start + 12 * H, Some(SESSION_A), "gpt-6", 999)],
        );
        late["tool"] = json!("codex");
        assert_eq!(ingest(&engine, vec![late])["put"], 1);
        let before = dump(&engine);
        let range_before = call(&engine, "activityRange", json!({ "fromMs": start, "toMs": now }));
        engine.close().unwrap();
        drop(engine);
        std::fs::remove_file(dir.join("aac.redb")).unwrap();
        let engine = open(&dir);
        assert_eq!(dump(&engine), before);
        let range_after = call(&engine, "activityRange", json!({ "fromMs": start, "toMs": now }));
        assert_eq!(range_after["rows"], range_before["rows"]);
        assert_eq!(range_after["sessions"]["total"], range_before["sessions"]["total"]);
    }
    // the late change is a ledger event after the day's close
    let (ledger, _) = aac_store::log::Log::open(&dir.join("log/ledger"), "ledger", "l").unwrap();
    let mut kinds = Vec::new();
    ledger
        .read_after(None, |e| {
            if e.event_type.starts_with("activity.") {
                kinds.push(e.event_type.clone());
            }
            Ok(())
        })
        .unwrap();
    let close = kinds.iter().position(|k| k == "activity.day_closed").unwrap();
    assert!(kinds[close..].iter().any(|k| k == "activity.cells"), "{kinds:?}");
}

#[test]
fn ranges_cross_months_by_hour_and_by_day() {
    let dir = temp_dir("act-months");
    let engine = open(&dir);
    let days = [T0 + 29 * D, T0 + 45 * D, T0 + 62 * D, T0 + 101 * D]; // Aug 30, Sep 15, Oct 2, Nov 10
    let read = T0 + 104 * D;
    let mut deep = report(
        "ubuntu",
        read,
        "ok",
        true,
        Some(T0),
        days.iter()
            .enumerate()
            .map(|(i, d)| cell(d + 10 * H, Some(SESSION_A), "gpt-6", (i as u64 + 1) * 10))
            .collect(),
    );
    deep["kind"] = json!("backfill");
    ingest(&engine, vec![deep]);
    assert_eq!(tokens_in(&engine, T0, read), 100);
    let starts: Vec<u64> = (0..=104).map(|i| T0 + i * D).collect();
    let by_day = call(
        &engine,
        "activityRange",
        json!({ "fromMs": T0, "toMs": read, "resolution": "day", "dayStarts": starts }),
    );
    let buckets: Vec<(u64, u64)> = by_day["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r[0].as_u64().unwrap(), r[5].as_u64().unwrap()))
        .collect();
    assert_eq!(
        buckets,
        days.iter()
            .enumerate()
            .map(|(i, d)| (*d, (i as u64 + 1) * 10))
            .collect::<Vec<_>>()
    );
}

#[test]
fn an_unreachable_computer_is_a_gap_not_zero() {
    let dir = temp_dir("act-gaps");
    let engine = open(&dir);
    let day1 = T0 + 40 * D;
    // the Mac's logs only reach back one day each time it is read
    let mac = |read: u64, cells| report("mac", read, "ok", true, Some((read - D) / H * H), cells);
    ingest(
        &engine,
        vec![mac(
            day1 + 12 * H,
            vec![cell(day1 + 9 * H, Some(SESSION_A), "gpt-6", 5)],
        )],
    );
    for d in 2..=4u64 {
        let mut down = report("mac", day1 + (d - 1) * D + 12 * H, "unreadable", false, None, vec![]);
        down["cause"] = json!("ssh_unreachable");
        let result = ingest(&engine, vec![down]);
        assert_eq!(result["put"], 0);
        if d == 3 {
            let s = call(
                &engine,
                "activitySources",
                json!({ "fromMs": day1, "toMs": day1 + 3 * D }),
            );
            assert_eq!(s["sources"][0]["state"], "unreadable");
            assert_eq!(s["sources"][0]["badSinceMs"].as_u64(), Some(day1 + D + 12 * H));
        }
    }
    ingest(
        &engine,
        vec![mac(
            day1 + 4 * D + 12 * H,
            vec![cell(day1 + 4 * D + 9 * H, Some(SESSION_A), "gpt-6", 7)],
        )],
    );
    let s = call(
        &engine,
        "activitySources",
        json!({ "fromMs": day1, "toMs": day1 + 5 * D }),
    );
    let gaps = s["computers"][0]["gaps"].as_array().unwrap();
    assert_eq!(gaps.len(), 2, "{gaps:?}");
    assert_eq!(gaps[0][0].as_u64(), Some(day1 + 13 * H));
    assert_eq!(gaps[0][1].as_u64(), Some(day1 + 3 * D + 12 * H));
    assert!(s["sources"][0]["badSinceMs"].is_null(), "a good read ends the outage");
    // no row was invented for the days it could not be read
    let rows = call(
        &engine,
        "activityRange",
        json!({ "fromMs": day1 + D, "toMs": day1 + 3 * D }),
    );
    assert!(rows["rows"].as_array().unwrap().is_empty());
}

#[test]
fn retention_is_opt_in() {
    for (days, kept) in [(None, true), (Some(90), false)] {
        let dir = temp_dir("act-retention");
        let engine = ops::open_engine_with(dir.to_str().unwrap(), None, None, days).unwrap();
        let read = T0 + 400 * D;
        let mut deep = report(
            "ubuntu",
            read,
            "partial",
            false,
            None,
            vec![
                cell(T0, Some(SESSION_A), "gpt-6", 9),
                cell(read - D, Some(SESSION_A), "gpt-6", 1),
            ],
        );
        deep["kind"] = json!("backfill");
        ingest(&engine, vec![deep]);
        call(&engine, "maintain", json!({ "nowMs": read }));
        assert_eq!(tokens_in(&engine, T0, T0) == 9, kept);
        assert_eq!(tokens_in(&engine, read - D, read), 1);
    }
}

#[test]
fn sessions_merge_in_any_order() {
    let session = |_computer: &str, tokens: u64, first: u64, last: u64| {
        json!({ "sessionKey": SESSION_A, "first": iso(first), "last": iso(last), "target": "codex",
                "parts": [{ "route": "", "modelRaw": "gpt-6", "inputTokens": tokens, "outputTokens": 0,
                            "cacheCreationTokens": 0, "cacheReadTokens": 0, "costUsd": 0.5 }] })
    };
    let read = T0 + 40 * D;
    let mut a = report("ubuntu", read, "partial", false, None, vec![]);
    a["sessions"] = json!([session("ubuntu", 10, read - 9 * H, read - 2 * H)]);
    let mut b = report("mac", read, "partial", false, None, vec![]);
    b["sessions"] = json!([session("mac", 30, read - 5 * H, read - H)]);
    let mut answers = Vec::new();
    for order in [vec![a.clone(), b.clone()], vec![b, a]] {
        let dir = temp_dir("act-sessions");
        let engine = open(&dir);
        for r in order {
            ingest(&engine, vec![r]);
        }
        let range = call(&engine, "activityRange", json!({ "fromMs": read - D, "toMs": read }));
        assert_eq!(range["sessions"]["total"], 1, "a mirrored session is one session");
        answers.push(range["sessions"].clone());
    }
    assert_eq!(answers[0], answers[1]);
    let s = &answers[0]["sample"][0];
    assert_eq!(s["firstMs"].as_u64(), Some(read - 9 * H));
    assert_eq!(s["lastMs"].as_u64(), Some(read - H));
    assert_eq!(s["parts"][0]["inputTokens"], 30);
}

#[test]
fn bad_reports_are_refused_whole() {
    let dir = temp_dir("act-refuse");
    let engine = open(&dir);
    let read = T0 + 40 * D;
    let mut off_hour = cell(read - H, None, "gpt-6", 1);
    off_hour["hour"] = json!(iso(read - H + 60_000));
    let secret = cell(read - H, None, "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123456789", 1);
    let mut messages = report("ubuntu", read, "ok", false, None, vec![]);
    messages["messages"] = json!([{ "at": iso(read), "key": SESSION_A, "modelRaw": "gpt-6", "inputTokens": 1,
                                    "outputTokens": 0, "cacheCreationTokens": 0, "cacheReadTokens": 0, "costUsd": 0.0 }]);
    let failed_with_cells = report(
        "ubuntu",
        read,
        "unreadable",
        false,
        None,
        vec![cell(read - H, None, "gpt-6", 1)],
    );
    let good = cell(read - 2 * H, Some(SESSION_A), "gpt-6", 3);
    let answer = ingest(
        &engine,
        vec![
            report("ubuntu", read, "ok", false, None, vec![good.clone(), off_hour]),
            report("ubuntu", read, "ok", false, None, vec![good.clone(), secret]),
            messages,
            failed_with_cells,
            json!({ "schema": "aac.usage-report.v1" }),
        ],
    );
    let codes: Vec<(u64, String)> = answer["refused"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r["index"].as_u64().unwrap(), r["code"].as_str().unwrap().to_string()))
        .collect();
    assert_eq!(codes.len(), 5, "{answer}");
    assert!(codes.contains(&(2, "not_supported".to_string())));
    assert_eq!(
        tokens_in(&engine, read - D, read),
        0,
        "nothing of a refused report is stored"
    );
}

#[test]
fn a_failed_read_is_a_state_and_never_a_zero() {
    let dir = temp_dir("act-failed");
    let engine = open(&dir);
    let read = T0 + 40 * D;
    ingest(
        &engine,
        vec![complete(
            "windows",
            read,
            vec![cell(read - H, Some(SESSION_A), "gpt-6", 8)],
        )],
    );
    let mut failed = report("windows", read + H, "scan_failed", false, None, vec![]);
    failed["cause"] = json!("timeout");
    ingest(&engine, vec![failed]);
    assert_eq!(
        tokens_in(&engine, read - D, read + H),
        8,
        "a failed read removes nothing"
    );
    let s = call(
        &engine,
        "activitySources",
        json!({ "fromMs": read - D, "toMs": read + H }),
    );
    assert_eq!(s["sources"][0]["state"], "scan_failed");
    assert_eq!(s["sources"][0]["cause"], "timeout");
    // the scan history keeps both reads
    let counts = call(&engine, "activityCounts", json!({}));
    assert_eq!(counts["scans"], 2);
}

#[test]
fn the_retired_codex_cache_is_imported_once_with_cached_input_taken_out() {
    let dir = temp_dir("act-legacy");
    let engine = open(&dir);
    let file = dir.join("codex-native-usage-v1.json");
    let at = |h: u64| iso(T0 + h * H);
    let body = json!({
        "version": 1, "includeCliproxySessions": false, "generatedAt": 1,
        "files": {
            "/gone/rollout-a.jsonl": { "path": "/gone/rollout-a.jsonl", "size": 1, "mtimeMs": 1, "entries": [
                { "inputTokens": 1000, "outputTokens": 50, "cacheCreationTokens": 0, "cacheReadTokens": 900, "model": "gpt-5.6-sol",
                  "sessionId": "019a-raw-session-a", "timestamp": at(3), "projectPath": "/private/project", "version": "0.1", "target": "codex" },
                { "inputTokens": 400, "outputTokens": 10, "cacheCreationTokens": 0, "cacheReadTokens": 300, "model": "gpt-5.6-sol",
                  "sessionId": "019a-raw-session-a", "timestamp": at(3), "projectPath": "/private/project", "version": "0.1", "target": "codex" }
            ]},
            "/gone/rollout-b.jsonl": { "path": "/gone/rollout-b.jsonl", "size": 1, "mtimeMs": 1, "entries": [
                { "inputTokens": 70, "outputTokens": 7, "cacheCreationTokens": 0, "cacheReadTokens": 0, "model": "gpt-6-astra",
                  "sessionId": "019a-raw-session-b", "timestamp": at(30), "projectPath": "/private/other", "version": "0.1", "target": "codex" }
            ]}
        }
    });
    std::fs::write(&file, body.to_string()).unwrap();
    let args = json!({ "path": file.to_str().unwrap(), "computer": "ubuntu" });
    let first = call(&engine, "activityImportCodexV1", args.clone());
    assert_eq!(first["imported"], true, "{first}");
    assert_eq!(first["records"], 3);
    // uncached input: 1000 - 900 + 400 - 300 = 200 in hour 3; 70 in hour 30
    let range = call(&engine, "activityRange", json!({ "fromMs": T0, "toMs": T0 + 2 * D }));
    let rows: Vec<(u64, u64, u64)> = range["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| (r[0].as_u64().unwrap(), r[5].as_u64().unwrap(), r[8].as_u64().unwrap()))
        .collect();
    assert_eq!(rows, vec![(T0 + 3 * H, 200, 1200), (T0 + 30 * H, 70, 0)]);
    // output is imported as it is (it counts reasoning twice) and every cell keeps the flag
    let flags: Vec<u64> = range["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[13].as_u64().unwrap())
        .collect();
    assert_eq!(flags, vec![1, 1]);
    assert_eq!(range["rows"][0][6], 60);
    assert_eq!(
        range["quality"],
        json!([{ "tool": "codex", "flags": ["outputIncludesReasoningTwice"], "fromMs": T0 + 3 * H, "toMs": T0 + 31 * H }])
    );
    for s in range["sessions"]["sample"].as_array().unwrap() {
        assert_eq!(s["quality"], json!(["outputIncludesReasoningTwice"]));
    }
    assert!(
        range["rows"][0][10].is_null(),
        "no cost is invented; the server prices it"
    );
    assert_eq!(range["sessions"]["total"], 2);
    // no raw session id or path is stored
    let dump = format!("{:?}", activity::dump_cells(&engine).unwrap());
    assert!(!dump.contains("raw-session") && !dump.contains("/private"));
    // the scan history says where it came from: an import of a retired cache, two rollout files
    let sources = call(&engine, "activitySources", json!({ "fromMs": T0, "toMs": T0 + 2 * D }));
    assert_eq!(first["files"], 2);
    let counts = call(&engine, "activityCounts", json!({}));
    assert_eq!(counts["scans"], 1);
    assert!(
        sources["sources"][0]["oldestMs"].as_u64() == Some(T0 + 3 * H),
        "{sources}"
    );
    let again = call(&engine, "activityImportCodexV1", args);
    assert_eq!(again["imported"], false);
    // another version is refused
    let mut v2 = body.clone();
    v2["version"] = json!(2);
    let other = dir.join("v2.json");
    std::fs::write(&other, v2.to_string()).unwrap();
    assert!(
        ops::call(
            &engine,
            "activityImportCodexV1",
            &json!({ "path": other.to_str().unwrap(), "computer": "ubuntu" }).to_string()
        )
        .is_err()
    );
}

fn wall_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64
}

#[test]
fn a_future_dated_record_never_freezes_live_writes() {
    // The review's probe: a complete read dated ten days ahead carrying one 5-token cell, then a
    // normal complete read with 100 tokens in the last hour.
    let dir = temp_dir("act-future");
    let engine = open(&dir);
    let now = wall_now();
    let hour = now / H * H;
    let ahead = hour + 10 * D;
    let probe = ingest(
        &engine,
        vec![complete(
            "ubuntu",
            ahead,
            vec![cell(ahead, Some(SESSION_A), "gpt-6", 5)],
        )],
    );
    assert_eq!(probe["put"], 0, "a record dated after now is not written: {probe}");
    // Each later read is sent at a fresh clock reading: the store clamped the probe's read time to
    // its own clock during that ingest, after `now`, and a read sent at `now` would look older.
    let normal = ingest(
        &engine,
        vec![complete(
            "ubuntu",
            wall_now(),
            vec![cell(hour, Some(SESSION_A), "gpt-6", 100)],
        )],
    );
    assert_eq!(normal["put"], 1, "{normal}");
    assert_eq!(
        tokens_in(&engine, hour - H, hour + H),
        100,
        "the settle line stayed behind now"
    );
    // a later read with the future record still in the log: the rest is written, the record is
    // left out and counted with the reader's own count, never refused whole
    let mut mixed = complete(
        "ubuntu",
        wall_now(),
        vec![
            cell(hour, Some(SESSION_A), "gpt-6", 120),
            cell(ahead, Some(SESSION_B), "gpt-6", 5),
        ],
    );
    mixed["counts"] = json!({ "futureDated": 2 });
    mixed["sessions"] = json!([
        { "sessionKey": SESSION_B, "first": iso(ahead), "last": iso(ahead + H), "parts": [] },
        { "sessionKey": SESSION_A, "first": iso(hour), "last": iso(ahead), "parts": [] }
    ]);
    let third = ingest(&engine, vec![mixed]);
    assert!(third["refused"].as_array().unwrap().is_empty(), "{third}");
    assert_eq!(tokens_in(&engine, hour - H, hour + H), 120);
    assert_eq!(tokens_in(&engine, hour, ahead + D), 120, "nothing stored after now");
    let sources = call(&engine, "activitySources", json!({ "fromMs": hour - D, "toMs": hour }));
    let src = &sources["sources"][0];
    assert_eq!(src["futureDated"], 3, "{sources}");
    assert!(src["lastReadMs"].as_u64().unwrap() <= wall_now());
    // the session that starts in the future is left out; the other one ends at the slack
    let range = call(
        &engine,
        "activityRange",
        json!({ "fromMs": hour - D, "toMs": wall_now() }),
    );
    let sample = range["sessions"]["sample"].as_array().unwrap();
    assert_eq!(sample.len(), 1, "{range}");
    assert_eq!(sample[0]["sessionKey"], SESSION_A);
    assert!(sample[0]["lastMs"].as_u64().unwrap() <= wall_now() + 5 * 60_000);
}
