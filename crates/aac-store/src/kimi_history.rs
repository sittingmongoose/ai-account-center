//! One-time Kimi Code history from OMP session logs.
//!
//! Lives in the store as `tool=omp` cells with `route=kimi-code` so Analytics
//! shows those days under Kimi Code and the remaining OMP cells stay OMP.
//! Idempotent: skip any `(hour, session, model)` already stored for this
//! computer's OMP tool, and a per-computer meta stamp so a second pass is a
//! no-op. Does not touch OMP's backfill mark.

use crate::activity::{self, IngestResult};
use crate::collect::{self, Pricing, PricingResolution, Rates};
use crate::contract::{self, HOUR_MS};
use crate::engine::Engine;
use crate::error::Result;
use crate::report::{self, RowsReportArgs};
use serde_json::{Value, json};

const META_PREFIX: &str = "kimi_omp_history:";
const TOOL: &str = "omp";
const KIND_BACKFILL: &str = "backfill";

/// Pricing that records tokens only (no list rates). History backfill uses this
/// when the host has not fed models.dev facts; Analytics still has the counts.
pub struct Unpriced;

impl Pricing for Unpriced {
    fn resolve(&self, _model: &str, _provider: Option<&str>) -> PricingResolution {
        PricingResolution {
            rates: Rates {
                input: 0.0,
                output: 0.0,
                cache_creation: 0.0,
                cache_read: 0.0,
            },
            source: "fallback".into(),
        }
    }
}

fn meta_key(computer: &str) -> String {
    format!("{META_PREFIX}{computer}")
}

/// True when this computer already ran the one-time Kimi OMP history backfill.
pub fn already_done(engine: &Engine, computer: &str) -> Result<bool> {
    if computer.trim().is_empty() {
        return Ok(true);
    }
    Ok(engine.get_meta(&meta_key(computer))?.is_some())
}

/// Stamp the one-time Kimi OMP history backfill so a later pass is a no-op.
pub fn mark_done(engine: &Engine, computer: &str, now_ms: i64) -> Result<()> {
    if computer.trim().is_empty() {
        return Ok(());
    }
    engine.set_meta(&meta_key(computer), &now_ms.to_string())
}

fn kimi_worker(scan: &Value, computer: &str, now_ms: i64, pricing: &dyn Pricing) -> Value {
    let empty = Vec::new();
    let rows = scan["rows"].as_array().unwrap_or(&empty);
    let srows = scan["srows"].as_array().unwrap_or(&empty);
    let omp_rows: Vec<Value> = rows
        .iter()
        .filter(|row| row["k"].as_str() == Some(TOOL))
        .cloned()
        .collect();
    let omp_srows: Vec<Value> = srows
        .iter()
        .filter(|row| row["k"].as_str() == Some(TOOL))
        .cloned()
        .collect();
    let mut worker = collect::worker_from_scan(&omp_rows, &omp_srows, TOOL, computer, now_ms, pricing);
    let cells = worker["cells"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|cell| collect::is_kimi_code_usage(cell["modelRaw"].as_str(), cell["route"].as_str()))
        .cloned()
        .collect::<Vec<_>>();
    worker["cells"] = json!(cells);
    worker
}

fn hour_index(hour: &str) -> Option<u32> {
    contract::parse_iso(hour).map(|ms| (ms / HOUR_MS) as u32)
}

fn already_stored(
    cells: &[(String, String, activity::CellEv)],
    computer: &str,
    hour: Option<u32>,
    session: Option<&str>,
    model: &str,
) -> bool {
    let Some(hour) = hour else {
        return false;
    };
    cells.iter().any(|(c, tool, cell)| {
        c == computer && tool == TOOL && cell.h == hour && cell.s.as_deref() == session && cell.m == model
    })
}

/// Load Kimi Code rows from an OMP scan into the store once. Skip any
/// `(hour, session, model)` already stored as OMP for this computer so live
/// and history never double-count. Does not set the stamp; the writer stamps
/// after a successful scan.
pub fn ingest_kimi_from_omp_scan(
    engine: &Engine,
    computer: &str,
    scan: &Value,
    now_ms: i64,
    pricing: &dyn Pricing,
) -> Result<IngestResult> {
    if computer.trim().is_empty() || already_done(engine, computer)? {
        return Ok(IngestResult::default());
    }
    let worker = kimi_worker(scan, computer, now_ms, pricing);
    let report = report::rows_report(&RowsReportArgs {
        computer,
        tool: TOOL,
        data: std::slice::from_ref(&worker),
        kind: KIND_BACKFILL,
        read_at: now_ms,
        complete: true,
        unreadable_records: None,
        cause: None,
        now: now_ms,
    });
    let stored = activity::dump_cells(engine)?;
    let cells = report["cells"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|cell| {
            !already_stored(
                &stored,
                computer,
                cell["hour"].as_str().and_then(hour_index),
                cell["sessionKey"].as_str(),
                cell["modelRaw"].as_str().unwrap_or(""),
            )
        })
        .cloned()
        .collect::<Vec<_>>();
    if cells.is_empty() {
        return Ok(IngestResult::default());
    }
    let mut report = report;
    report["cells"] = json!(cells);
    let typed = report::contract_reports(std::slice::from_ref(&report))
        .map_err(|error| crate::error::Error::new("invalid", error.to_string()))?;
    activity::ingest(engine, &typed)
}
