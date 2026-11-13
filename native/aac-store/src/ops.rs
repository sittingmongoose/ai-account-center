//! JSON method dispatch shared by the Node binding and the tests: one method name and a JSON
//! argument in, one JSON answer out. No method takes or returns a secret.

use crate::engine::{self, Config, Engine};
use crate::error::Result;
use crate::fail;
use crate::{capture, runs, usage};
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// The longest meta value TypeScript may store (its markers and reports are small JSON).
const META_VALUE_MAX: usize = 16 * 1024;

pub type Slot = Mutex<Option<Arc<Engine>>>;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct OpenArgs {
    dir: String,
    cache_bytes: Option<usize>,
    retention_days: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MaintainArgs {
    now_ms: Option<u64>,
    verify: Option<bool>,
    compact: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BackupArgs {
    roots: Vec<String>,
    with_redb: bool,
    keep_daily: Option<usize>,
    keep_with_redb: Option<usize>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BurnArgs {
    series: Vec<usage::SeriesRef>,
    spans_ms: Vec<u64>,
    now_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PeriodArgs {
    series: Vec<usage::SeriesRef>,
    from_ms: u64,
    to_ms: u64,
}

fn parse<T: serde::de::DeserializeOwned>(args: &str) -> Result<T> {
    serde_json::from_str(args).map_err(|e| crate::error::Error::new("invalid", e.to_string()))
}

pub fn open_engine(dir: &str, cache_bytes: Option<usize>, retention_days: Option<u64>) -> Result<Engine> {
    let mut config = Config::default();
    if let Some(bytes) = cache_bytes {
        config.cache_bytes = bytes.clamp(8 << 20, 256 << 20);
    }
    if let Some(days) = retention_days {
        config.retention_days = if days == 0 { 0 } else { days.clamp(7, 3650) };
    }
    Engine::open(
        &PathBuf::from(dir),
        config,
        vec![
            Box::new(usage::UsageProjector),
            Box::new(capture::CaptureProjector),
            Box::new(runs::RunsProjector),
        ],
    )
}

pub fn dispatch(slot: &Slot, method: &str, args: &str) -> Result<String> {
    let answer = match method {
        "open" => {
            let a: OpenArgs = parse(args)?;
            let mut guard = slot.lock().unwrap_or_else(|p| p.into_inner());
            if guard.is_some() {
                fail!("already_open", "the store is already open");
            }
            let engine = open_engine(&a.dir, a.cache_bytes, a.retention_days)?;
            let status = engine.status()?;
            *guard = Some(Arc::new(engine));
            status
        }
        "close" => {
            let engine = slot.lock().unwrap_or_else(|p| p.into_inner()).take();
            if let Some(engine) = engine {
                engine.close()?;
            }
            json!({ "closed": true })
        }
        "restore" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct RestoreArgs {
                dir: String,
                backup_dir: String,
            }
            let a: RestoreArgs = parse(args)?;
            if slot.lock().unwrap_or_else(|p| p.into_inner()).is_some() {
                fail!("already_open", "close the store before a restore");
            }
            engine::restore(&PathBuf::from(a.dir), &PathBuf::from(a.backup_dir))?
        }
        _ => {
            let engine = match slot.lock().unwrap_or_else(|p| p.into_inner()).as_ref() {
                Some(e) => Arc::clone(e),
                None => fail!("closed", "the store is not open"),
            };
            call(&engine, method, args)?
        }
    };
    Ok(answer.to_string())
}

pub fn call(engine: &Engine, method: &str, args: &str) -> Result<Json> {
    Ok(match method {
        "status" => {
            let mut status = engine.status()?;
            status["capture"] = capture::health(engine)?;
            let bytes = status["redbBytes"].as_u64().unwrap_or(0);
            status["storageHealth"] = json!({
                "rawPolicy": if engine.config.retention_days == 0 { "forever" } else { "explicit_retention" },
                "hotBytes": bytes, "hotBudgetBytes": 2u64 << 30,
                "budgetWarning": bytes >= (2u64 << 30) * 4 / 5,
                "budgetExceeded": bytes >= 2u64 << 30,
                "automaticDeletion": false,
                "projectedYearBytes": null,
                "quotaTemporaries": crate::storage_health::quota_temporaries(&engine.dir),
            });
            status
        }
        "captureReports" => {
            if args.len() > 8 * 1024 * 1024 {
                fail!("invalid", "capture batch too large");
            }
            #[derive(Deserialize)]
            struct CaptureArgs {
                reports: Vec<capture::Report>,
            }
            capture::record(engine, parse::<CaptureArgs>(args)?.reports)?
        }
        "captureHistory" => capture::history(engine, parse(args)?)?,
        "counts" => usage::counts(engine)?,
        "record" => {
            #[derive(Deserialize)]
            struct RecordArgs {
                observations: Vec<Json>,
            }
            let a: RecordArgs = parse(args)?;
            let mut valid = Vec::with_capacity(a.observations.len());
            let mut rejected = 0;
            for o in a.observations {
                match serde_json::from_value::<usage::Sampled>(o) {
                    Ok(s) => valid.push(s),
                    Err(_) => rejected += 1,
                }
            }
            let mut result = serde_json::to_value(usage::record(engine, valid)?)?;
            result["rejected"] = json!(result["rejected"].as_u64().unwrap_or(0) + rejected);
            result
        }
        "historyRecords" => usage::history_records(engine, &parse(args)?)?,
        "oldest" => usage::oldest(engine)?,
        "burnLong" => {
            let a: BurnArgs = parse(args)?;
            usage::burn_long(engine, &a.series, &a.spans_ms, a.now_ms)?
        }
        "periods" => {
            let a: PeriodArgs = parse(args)?;
            usage::periods(engine, &a.series, a.from_ms, a.to_ms)?
        }
        "seriesPoints" => usage::series_points(engine, &parse(args)?)?,
        "seriesList" => usage::series_list(engine, &parse(args)?)?,
        "usageAt" => {
            #[derive(Deserialize)]
            struct AtArgs {
                t: u64,
            }
            usage::usage_at(engine, parse::<AtArgs>(args)?.t)?
        }
        "maintain" => {
            let a: MaintainArgs = parse(args)?;
            let now = a.now_ms.unwrap_or_else(engine::now_ms);
            let sealed = engine.seal_and_compress(now)?;
            let closed = usage::close_days(engine, now)?;
            let pruned = usage::prune(engine, now)?;
            let migrated = usage::migrate_account_states(engine, false)?;
            let backed: Option<u64> = engine.get_meta("backup_readings_seq")?.and_then(|v| v.parse().ok());
            let retired = usage::retire_segments(engine, now, Some(backed.unwrap_or(0)))?;
            let verified = if a.verify.unwrap_or(false) {
                engine.verify()?
            } else {
                Json::Null
            };
            let compacted = if a.compact.unwrap_or(false) {
                json!(engine.compact()?)
            } else {
                Json::Null
            };
            json!({ "sealed": sealed, "closed": closed, "pruned": pruned, "migrated": migrated, "retired": retired, "verified": verified, "compacted": compacted })
        }
        "backup" => {
            let a: BackupArgs = parse(args)?;
            let roots: Vec<PathBuf> = a.roots.iter().map(PathBuf::from).collect();
            engine.backup(
                &roots,
                a.with_redb,
                a.keep_daily.unwrap_or(7),
                a.keep_with_redb.unwrap_or(4),
            )?
        }
        "setMeta" => {
            #[derive(Deserialize)]
            struct MetaArgs {
                key: String,
                value: String,
            }
            let a: MetaArgs = parse(args)?;
            if !a.key.starts_with("ts:") {
                fail!("invalid", "TypeScript meta keys start with ts:");
            }
            // A meta value is a persisted string like any other: bounded, no control
            // characters, nothing shaped like a credential.
            if !usage::text_ok(&a.value, META_VALUE_MAX) {
                fail!("invalid", "the meta value is empty, too long or not plain text");
            }
            engine.set_meta(&a.key, &a.value)?;
            json!({ "ok": true })
        }
        "getMeta" => {
            #[derive(Deserialize)]
            struct MetaKey {
                key: String,
            }
            json!(engine.get_meta(&parse::<MetaKey>(args)?.key)?)
        }
        // Agent-report runs (Scoreboard v2): scan, record, queries.
        "runsScan" => runs::scan(engine, &parse(args)?)?,
        "runsRecord" => runs::record(engine, parse(args)?)?,
        "runsQuery" => runs::query(engine, &parse(args)?)?,
        "runsIdentity" => runs::identity_table(engine)?,
        "runsAudit" => runs::audit(engine)?,
        "runsStatus" => runs::status(engine)?,
        "runsRefused" => runs::refused(engine)?,
        "runOriginal" | "runHistory" => {
            #[derive(Deserialize)]
            #[serde(rename_all = "camelCase")]
            struct RunArgs {
                run_id: String,
                sha256: Option<String>,
            }
            let a: RunArgs = parse(args)?;
            if method == "runOriginal" {
                runs::original(engine, &a.run_id, a.sha256.as_deref())?
            } else {
                runs::history(engine, &a.run_id)?
            }
        }
        _ => fail!("invalid", "unknown method {method}"),
    })
}
