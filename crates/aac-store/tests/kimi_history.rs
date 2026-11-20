//! One-time Kimi Code history from OMP scan rows: load once, skip existing
//! (hour, session, model), never double-count OMP totals. Temporary store only.
mod common;

use aac_store::activity;
use aac_store::collect;
use aac_store::engine::Engine;
use aac_store::kimi_history::{self, Unpriced};
use aac_store::ops;
use aac_store::usage;
use common::temp_dir;
use serde_json::{Value as Json, json};
use std::path::Path;

const H: u64 = 3_600_000;
const D: u64 = 86_400_000;
/// 2026-11-20T00:00:00Z.
const NOW: i64 = 1_795_132_800_000;
/// 2026-10-01T15:00:00Z, outside the live 31-day window.
const OLD: u64 = 1_790_866_800_000;
/// 2026-11-18T12:00:00Z, inside the live window.
const RECENT: u64 = 1_795_003_200_000;
const KIMI_SESS: &str = "kimi-history-session";
const QWEN_SESS: &str = "qwen-history-session";

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, None).unwrap()
}

fn iso(ms: u64) -> String {
    usage::iso(ms as i64)
}

fn hour_label(ms: u64) -> String {
    let iso = iso(ms);
    format!("{} {}", &iso[..10], &iso[11..16])
}

fn session_key(raw: &str) -> String {
    collect::session_key("omp", raw)
}

fn cell(hour: u64, session: Option<&str>, route: &str, model: &str, tokens: u64) -> Json {
    json!({
        "hour": iso(hour),
        "sessionKey": session,
        "route": route,
        "modelRaw": model,
        "inputTokens": tokens,
        "outputTokens": 0,
        "cacheCreationTokens": 0,
        "cacheReadTokens": 0,
        "costUsd": 0.0,
        "fallbackCostUsd": 0.0,
        "events": 1,
    })
}

fn live_omp(read_at: u64, cells: Vec<Json>) -> Json {
    let from = (read_at - 31 * D) / H * H;
    json!({
        "schema": "aac.usage-report.v1",
        "computer": "ubuntu",
        "tool": "omp",
        "readAt": iso(read_at),
        "state": "ok",
        "complete": true,
        "coverFrom": iso(from),
        "cells": cells,
    })
}

fn ingest(engine: &Engine, reports: Vec<Json>) -> Json {
    ops::call(engine, "activityIngest", &json!({ "reports": reports }).to_string()).unwrap()
}

fn dump(engine: &Engine) -> Vec<String> {
    activity::dump_cells(engine)
        .unwrap()
        .into_iter()
        .map(|(c, t, e)| {
            format!(
                "{c} {t} {} {} {} {} {}",
                e.h,
                e.s.as_deref().unwrap_or("-"),
                e.r,
                e.m,
                e.i + e.o + e.cw + e.cr
            )
        })
        .collect()
}

fn tokens_in(engine: &Engine, from: u64, to: u64) -> u64 {
    let answer = ops::call(
        engine,
        "activityRange",
        &json!({ "fromMs": from, "toMs": to }).to_string(),
    )
    .unwrap();
    answer["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r[5].as_u64().unwrap())
        .sum()
}

fn scan_row(file: &str, model: &str, provider: &str, hour_ms: u64, input: u64) -> Json {
    json!({
        "k": "omp",
        "f": file,
        "m": model,
        "p": provider,
        "h": hour_label(hour_ms),
        "i": input,
        "o": 0,
        "cr": 0,
        "cw": 0,
        "c": 0,
        "n": 1
    })
}

fn scan_srow(file: &str, raw_session: &str, model: &str, provider: &str, hour_ms: u64, input: u64) -> Json {
    json!({
        "k": "omp",
        "f": file,
        "s": session_key(raw_session),
        "m": model,
        "p": provider,
        "a": hour_ms,
        "z": hour_ms,
        "i": input,
        "o": 0,
        "cr": 0,
        "cw": 0,
        "c": 0,
        "n": 1
    })
}

fn mixed_scan() -> Json {
    json!({
        "version": 2,
        "reader": { "name": "aac-usage-scan" },
        "kinds": { "omp": { "state": "ok" } },
        "rows": [
            scan_row("old-kimi", "kimi-k3", "moonshotai", OLD, 200),
            scan_row("recent-kimi", "kimi-k3", "moonshotai", RECENT, 400),
            scan_row("recent-qwen", "qwen3-max", "alibaba", RECENT, 1000),
        ],
        "srows": [
            scan_srow("old-kimi", KIMI_SESS, "kimi-k3", "moonshotai", OLD, 200),
            scan_srow("recent-kimi", KIMI_SESS, "kimi-k3", "moonshotai", RECENT, 400),
            scan_srow("recent-qwen", QWEN_SESS, "qwen3-max", "alibaba", RECENT, 1000),
        ],
    })
}

#[test]
fn kimi_history_backfill_is_one_time_and_skips_existing_omp_cells() {
    let dir = temp_dir("kimi-hist");
    let engine = open(&dir);
    let kimi_key = session_key(KIMI_SESS);
    let qwen_key = session_key(QWEN_SESS);
    ingest(
        &engine,
        vec![live_omp(
            NOW as u64,
            vec![
                cell(RECENT, Some(&kimi_key), "moonshotai", "kimi-k3", 400),
                cell(RECENT, Some(&qwen_key), "alibaba", "qwen3-max", 1000),
            ],
        )],
    );
    assert_eq!(tokens_in(&engine, RECENT, RECENT), 1400);

    let first = kimi_history::ingest_kimi_from_omp_scan(&engine, "ubuntu", &mixed_scan(), NOW, &Unpriced).unwrap();
    assert!(
        first.put >= 1,
        "old Kimi hour is inserted: put={} refused={}",
        first.put,
        first.refused.len()
    );
    assert_eq!(tokens_in(&engine, OLD, OLD), 200, "old Kimi lands on 2026-10-01");
    assert_eq!(
        tokens_in(&engine, RECENT, RECENT),
        1400,
        "existing (hour, session, model) is not counted twice"
    );

    let kimi_routes: Vec<String> = dump(&engine)
        .into_iter()
        .filter(|row| row.contains("kimi-k3"))
        .collect();
    assert_eq!(
        kimi_routes.len(),
        2,
        "one stored recent cell plus the old history cell: {kimi_routes:?}"
    );
    assert!(
        dump(&engine)
            .iter()
            .any(|row| row.contains("kimi-code") && row.contains("kimi-k3")),
        "history cell uses the Kimi Code route: {:?}",
        dump(&engine)
    );

    let again = kimi_history::ingest_kimi_from_omp_scan(&engine, "ubuntu", &mixed_scan(), NOW, &Unpriced).unwrap();
    assert_eq!(again.put, 0, "skip-existing is idempotent without the stamp");
    assert_eq!(tokens_in(&engine, OLD, NOW as u64), 1600);

    kimi_history::mark_done(&engine, "ubuntu", NOW).unwrap();
    assert!(kimi_history::already_done(&engine, "ubuntu").unwrap());
    let stamped = kimi_history::ingest_kimi_from_omp_scan(&engine, "ubuntu", &mixed_scan(), NOW, &Unpriced).unwrap();
    assert_eq!(stamped.put, 0);
    assert_eq!(stamped.accepted, 0);
    assert_eq!(tokens_in(&engine, OLD, NOW as u64), 1600);
}

#[test]
fn kimi_history_does_not_retag_qwen_or_invent_kimi_on_empty_scan() {
    let dir = temp_dir("kimi-empty");
    let engine = open(&dir);
    let qwen_key = session_key(QWEN_SESS);
    ingest(
        &engine,
        vec![live_omp(
            NOW as u64,
            vec![cell(RECENT, Some(&qwen_key), "alibaba", "qwen3-max", 50)],
        )],
    );
    let scan = json!({
        "version": 2,
        "reader": { "name": "aac-usage-scan" },
        "kinds": { "omp": { "state": "ok" } },
        "rows": [scan_row("recent-qwen", "qwen3-max", "alibaba", RECENT, 50)],
        "srows": [scan_srow("recent-qwen", QWEN_SESS, "qwen3-max", "alibaba", RECENT, 50)],
    });
    let result = kimi_history::ingest_kimi_from_omp_scan(&engine, "ubuntu", &scan, NOW, &Unpriced).unwrap();
    assert_eq!(result.put, 0);
    assert_eq!(tokens_in(&engine, RECENT, RECENT), 50);
    assert!(!dump(&engine).iter().any(|row| row.contains("kimi-code")));
}
