//! The store engine (docs/store.md sections 2 and 4): one writer process, two logs, redb
//! projections with checkpoints, maintenance, backups and restore.

use crate::db::{self, Checkpoint};
use crate::error::{Error, Result};
use crate::event::Event;
use crate::fail;
use crate::fsx;
use crate::log::{Finding, Log};
use redb::{Database, Durability, ReadableDatabase, ReadableTable, WriteTransaction};
use serde_json::{Value as Json, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, RwLock};

pub const PROJECTOR_BATCH: usize = 4096;
pub const DAY_MS: u64 = 86_400_000;
const REDB_FILE: &str = "aac.redb";
const RESTORE_INTENT: &str = "restore.intent.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogId {
    Readings,
    Ledger,
}

impl LogId {
    pub fn name(self) -> &'static str {
        match self {
            LogId::Readings => "readings",
            LogId::Ledger => "ledger",
        }
    }
    pub const ALL: [LogId; 2] = [LogId::Readings, LogId::Ledger];
}

#[derive(Clone, Debug)]
pub struct Config {
    pub cache_bytes: usize,
    /// redb `check_integrity()` runs at every start while the file is smaller than this.
    pub integrity_max_bytes: u64,
    pub retention_days: u64,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            cache_bytes: 32 << 20,
            integrity_max_bytes: 512 << 20,
            retention_days: 90,
        }
    }
}

/// A projection of one or both logs. Its tables are derived: a version change drops and rebuilds them.
pub trait Projector: Send + Sync {
    fn name(&self) -> &'static str;
    fn version(&self) -> u32;
    fn reads(&self, log: LogId) -> bool;
    fn apply(&self, txn: &WriteTransaction, log: LogId, event: &Event) -> Result<()>;
    /// Drops this projector's tables before a rebuild.
    fn reset(&self, txn: &WriteTransaction) -> Result<()>;
}

pub struct Logs {
    pub readings: Log,
    pub ledger: Log,
}

impl Logs {
    pub fn get(&mut self, id: LogId) -> &mut Log {
        match id {
            LogId::Readings => &mut self.readings,
            LogId::Ledger => &mut self.ledger,
        }
    }
    pub fn peek(&self, id: LogId) -> &Log {
        match id {
            LogId::Readings => &self.readings,
            LogId::Ledger => &self.ledger,
        }
    }
}

pub struct Engine {
    pub dir: PathBuf,
    pub config: Config,
    _lock: fsx::StoreLock,
    pub logs: Mutex<Logs>,
    pub db: RwLock<Option<Database>>,
    pub projectors: Vec<Box<dyn Projector>>,
    pub findings: Mutex<Vec<Finding>>,
    pub instance_id: String,
    pub opened_ms: u64,
    pub integrity: Mutex<Json>,
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Opens redb with the bounded cache. Below the size limit every open verifies every page
/// checksum (`set_check_integrity_on_open`), so an altered file fails with `Corrupted` instead
/// of being trusted.
fn open_database(path: &Path, config: &Config) -> std::result::Result<Database, redb::DatabaseError> {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut builder = Database::builder();
    builder.set_cache_size(config.cache_bytes);
    builder.set_check_integrity_on_open(size < config.integrity_max_bytes);
    builder.create(path)
}

fn is_corruption(error: &redb::DatabaseError) -> bool {
    matches!(error, redb::DatabaseError::Storage(redb::StorageError::Corrupted(_)))
}

/// A write transaction with the store's durability settings.
pub fn begin(db: &Database) -> Result<WriteTransaction> {
    let mut txn = db.begin_write()?;
    txn.set_durability(Durability::Immediate)?;
    txn.set_quick_repair(true);
    Ok(txn)
}

impl Engine {
    pub fn open(dir: &Path, config: Config, projectors: Vec<Box<dyn Projector>>) -> Result<Engine> {
        fsx::private_dir(dir)?;
        let lock = fsx::lock_store(dir)?;
        finish_restore(dir)?;
        for sub in ["log", "quarantine", "tmp"] {
            fsx::private_dir(&dir.join(sub))?;
        }
        fsx::remove_temporaries(dir)?;
        let mut findings: Vec<Finding> = Vec::new();
        let db_path = dir.join(REDB_FILE);

        // A newer store is refused before anything is written: a read-only look at its version
        // first (opening for write, or the integrity check, would change the file).
        if db_path.exists() {
            // A read-only open skips the page checks, and redb may panic on a damaged page; a
            // panic here only means "not readable", and the checked open below decides.
            let probe = std::panic::catch_unwind(|| -> u64 {
                let Ok(read_only) = redb::Builder::new().open_read_only(&db_path) else {
                    return 0;
                };
                read_only
                    .begin_read()
                    .ok()
                    .and_then(|txn| {
                        txn.open_table(db::META).ok().and_then(|t| {
                            t.get("store_schema_version")
                                .ok()
                                .flatten()
                                .map(|v| String::from_utf8_lossy(v.value()).into_owned())
                        })
                    })
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0)
            });
            let version = probe.unwrap_or(0);
            if version > db::STORE_SCHEMA_VERSION {
                fail!(
                    "newer_store",
                    "store schema {version} is newer than this build ({})",
                    db::STORE_SCHEMA_VERSION
                );
            }
        }
        let database = match open_database(&db_path, &config) {
            Ok(db) => db,
            Err(e) if is_corruption(&e) => {
                quarantine(dir, &db_path, "redb_corrupt")?;
                findings.push(finding("redb_quarantined", &e.to_string()));
                open_database(&db_path, &config).map_err(|e| Error::new("db_unavailable", e.to_string()))?
            }
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => fail!("lock_held", "the database is open elsewhere"),
            Err(e) => fail!("db_unavailable", "{e}"),
        };
        let size = fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0);
        let checked = size < config.integrity_max_bytes;
        let integrity = json!({ "checkedAt": if checked { Some(now_ms()) } else { None }, "ok": if checked { Some(true) } else { None }, "bytes": size });
        let instance_id;
        {
            let txn = begin(&database)?;
            db::create_tables(&txn)?;
            let version: u64 = db::get_meta(&txn, "store_schema_version")?
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            if version > db::STORE_SCHEMA_VERSION {
                drop(txn);
                fail!(
                    "newer_store",
                    "store schema {version} is newer than this build ({})",
                    db::STORE_SCHEMA_VERSION
                );
            }
            if version == 0 {
                db::set_meta(&txn, "store_schema_version", &db::STORE_SCHEMA_VERSION.to_string())?;
                db::set_meta(&txn, "created_ms", &now_ms().to_string())?;
            }
            instance_id = match db::get_meta(&txn, "instance_id")? {
                Some(id) => id,
                None => {
                    // A rebuilt redb keeps the instance recorded by the logs' folder.
                    let id = read_instance_file(dir)?.unwrap_or_else(|| fsx::random_hex(16));
                    db::set_meta(&txn, "instance_id", &id)?;
                    id
                }
            };
            let clean = db::get_meta(&txn, "clean_shutdown")?;
            if clean.as_deref() == Some("false") {
                findings.push(finding("unclean_shutdown", "the previous run did not close the store"));
            }
            db::set_meta(&txn, "clean_shutdown", "false")?;
            txn.commit()?;
        }
        write_instance_file(dir, &instance_id)?;

        let (readings, mut found) = Log::open(&dir.join("log").join("readings"), "readings", "r")?;
        findings.append(&mut found);
        let (ledger, mut found) = Log::open(&dir.join("log").join("ledger"), "ledger", "l")?;
        findings.append(&mut found);

        let engine = Engine {
            dir: dir.to_path_buf(),
            config,
            _lock: lock,
            logs: Mutex::new(Logs { readings, ledger }),
            db: RwLock::new(Some(database)),
            projectors,
            findings: Mutex::new(Vec::new()),
            instance_id,
            opened_ms: now_ms(),
            integrity: Mutex::new(integrity),
        };
        let mut replay_findings = engine.replay()?;
        findings.append(&mut replay_findings);
        let loss: Vec<_> = findings
            .iter()
            .filter(|f| f.kind.contains("degraded") || f.kind.contains("missing"))
            .cloned()
            .collect();
        *engine.findings.lock().unwrap() = findings;
        let mut opened = Event::new(
            "store.opened",
            now_ms(),
            crate::msgpack::from_json(&json!({
                "build": env!("CARGO_PKG_VERSION"),
                "store_schema_version": db::STORE_SCHEMA_VERSION,
            }))?,
        );
        opened.idempotency_key = None;
        engine.commit(LogId::Ledger, |batch| {
            batch.push(opened)?;
            for f in &loss {
                let event = Event::new(
                    "store.integrity_detected",
                    now_ms(),
                    crate::msgpack::from_json(&serde_json::to_value(f)?)?,
                );
                batch.push(event)?;
            }
            Ok(())
        })?;
        Ok(engine)
    }

    pub fn lock_logs(&self) -> MutexGuard<'_, Logs> {
        self.logs.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn with_db<T>(&self, f: impl FnOnce(&Database) -> Result<T>) -> Result<T> {
        let guard = self.db.read().unwrap_or_else(|p| p.into_inner());
        match guard.as_ref() {
            Some(db) => f(db),
            None => fail!("closed", "the store is closed"),
        }
    }

    fn checkpoint_name(projector: &dyn Projector, log: LogId) -> String {
        format!("{}/{}", projector.name(), log.name())
    }

    /// Brings every projector up to the end of both logs, rebuilding any whose version changed.
    fn replay(&self) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let logs = self.lock_logs();
        self.with_db(|db| {
            // Version changes drop and rebuild.
            let txn = begin(db)?;
            for projector in &self.projectors {
                let mut stale = false;
                {
                    let table = txn.open_table(db::CHECKPOINT)?;
                    for log in LogId::ALL {
                        if !projector.reads(log) {
                            continue;
                        }
                        if let Some(cp) = db::read_checkpoint(&table, &Self::checkpoint_name(projector.as_ref(), log))?
                            && cp.projector_schema_version != projector.version()
                        {
                            stale = true;
                        }
                    }
                }
                if stale {
                    projector.reset(&txn)?;
                    let mut table = txn.open_table(db::CHECKPOINT)?;
                    for log in LogId::ALL {
                        table.remove(Self::checkpoint_name(projector.as_ref(), log).as_str())?;
                    }
                    findings.push(finding("projector_rebuilt", projector.name()));
                }
            }
            db::create_tables(&txn)?;
            txn.commit()?;

            for log_id in LogId::ALL {
                let log = logs.peek(log_id);
                let mut checkpoints: Vec<(usize, Checkpoint)> = Vec::new();
                {
                    let read = db.begin_read()?;
                    let table = read.open_table(db::CHECKPOINT)?;
                    for (i, projector) in self.projectors.iter().enumerate() {
                        if projector.reads(log_id) {
                            let cp = db::read_checkpoint(&table, &Self::checkpoint_name(projector.as_ref(), log_id))?
                                .unwrap_or_else(|| Checkpoint {
                                    log: log_id.name().into(),
                                    segment_generation: crate::log::GENERATION,
                                    projector_schema_version: projector.version(),
                                    ..Default::default()
                                });
                            checkpoints.push((i, cp));
                        }
                    }
                }
                if checkpoints.is_empty() {
                    continue;
                }
                let floor = checkpoints.iter().map(|(_, cp)| cp.last_sequence_id).min().unwrap_or(0);
                let mut pending: Vec<Event> = Vec::new();
                let flush = |pending: &mut Vec<Event>, checkpoints: &mut Vec<(usize, Checkpoint)>| -> Result<()> {
                    if pending.is_empty() {
                        return Ok(());
                    }
                    let txn = begin(db)?;
                    for event in pending.iter() {
                        for (i, cp) in checkpoints.iter_mut() {
                            if event.sequence_id > cp.last_sequence_id {
                                self.projectors[*i].apply(&txn, log_id, event)?;
                                cp.advance(event.sequence_id, &event.event_id);
                            }
                        }
                    }
                    for (i, cp) in checkpoints.iter() {
                        db::write_checkpoint(&txn, &Self::checkpoint_name(self.projectors[*i].as_ref(), log_id), cp)?;
                    }
                    txn.commit()?;
                    pending.clear();
                    Ok(())
                };
                let mut read_findings = log.read_after(if floor == 0 { None } else { Some(floor) }, |event| {
                    pending.push(event);
                    if pending.len() >= PROJECTOR_BATCH {
                        flush(&mut pending, &mut checkpoints)?;
                    }
                    Ok(())
                })?;
                flush(&mut pending, &mut checkpoints)?;
                findings.append(&mut read_findings);
            }
            Ok(())
        })?;
        Ok(findings)
    }

    /// Runs `build` inside one redb write transaction: each pushed event gets its sequence,
    /// is projected, then all are written durably to the log before the transaction (with the
    /// advanced checkpoints) commits. A failed log write aborts the transaction.
    pub fn commit<T>(&self, log_id: LogId, build: impl FnOnce(&mut Batch<'_>) -> Result<T>) -> Result<T> {
        let mut logs = self.lock_logs();
        self.with_db(|db| {
            let txn = begin(db)?;
            let mut checkpoints = Vec::new();
            {
                let table = txn.open_table(db::CHECKPOINT)?;
                for (i, projector) in self.projectors.iter().enumerate() {
                    if projector.reads(log_id) {
                        let cp = db::read_checkpoint(&table, &Self::checkpoint_name(projector.as_ref(), log_id))?
                            .unwrap_or_else(|| Checkpoint {
                                log: log_id.name().into(),
                                segment_generation: crate::log::GENERATION,
                                projector_schema_version: projector.version(),
                                ..Default::default()
                            });
                        checkpoints.push((i, cp));
                    }
                }
            }
            let log = logs.get(log_id);
            let mut batch = Batch {
                txn: &txn,
                log,
                log_id,
                engine: self,
                checkpoints,
                events: Vec::new(),
            };
            let result = build(&mut batch)?;
            let Batch {
                events,
                checkpoints,
                log,
                ..
            } = batch;
            if events.is_empty() {
                drop(txn);
                return Ok(result);
            }
            log.write(&events)?;
            for (i, cp) in &checkpoints {
                db::write_checkpoint(&txn, &Self::checkpoint_name(self.projectors[*i].as_ref(), log_id), cp)?;
            }
            txn.commit()?;
            Ok(result)
        })
    }

    pub fn status(&self) -> Result<Json> {
        let logs = self.lock_logs();
        let log_info = json!({ "readings": logs.readings.info(), "ledger": logs.ledger.info() });
        drop(logs);
        let db_bytes = fs::metadata(self.dir.join(REDB_FILE)).map(|m| m.len()).unwrap_or(0);
        let db_stats = self.with_db(|db| {
            let txn = db.begin_write()?;
            let stats = txn.stats()?;
            let out = json!({
                "allocatedPages": stats.allocated_pages(),
                "pageSize": stats.page_size(),
                "storedBytes": stats.stored_bytes(),
                "metadataBytes": stats.metadata_bytes(),
                "fragmentedBytes": stats.fragmented_bytes(),
            });
            txn.abort()?;
            Ok(out)
        })?;
        let (checkpoints, meta) = self.with_db(|db| {
            let read = db.begin_read()?;
            let table = read.open_table(db::CHECKPOINT)?;
            let mut checkpoints = serde_json::Map::new();
            for item in table.iter()? {
                let (k, v) = item?;
                let cp: Checkpoint = crate::msgpack::from_bytes(v.value())?;
                checkpoints.insert(
                    k.value().to_string(),
                    json!({
                        "lastSequence": cp.last_sequence_id,
                        "projectorVersion": cp.projector_schema_version,
                    }),
                );
            }
            let meta_table = read.open_table(db::META)?;
            let mut meta = serde_json::Map::new();
            for key in [
                "created_ms",
                "last_backup_ms",
                "last_share_backup_ms",
                "last_verify_ms",
                "last_compact_ms",
                "closed_through_day",
                "backfill",
            ] {
                if let Some(v) = meta_table.get(key)? {
                    meta.insert(
                        key.to_string(),
                        Json::String(String::from_utf8_lossy(v.value()).into_owned()),
                    );
                }
            }
            Ok((checkpoints, meta))
        })?;
        let findings = self.findings.lock().unwrap().clone();
        Ok(json!({
            "state": if findings.iter().any(|f| f.kind.contains("degraded") || f.kind.contains("missing")) { "degraded" } else { "ready" },
            "storeSchemaVersion": db::STORE_SCHEMA_VERSION,
            "instanceId": self.instance_id,
            "openedAt": self.opened_ms,
            "cacheBytes": self.config.cache_bytes,
            "retentionDays": self.config.retention_days,
            "redbBytes": db_bytes,
            "redbStats": db_stats,
            "integrity": *self.integrity.lock().unwrap(),
            "logs": log_info,
            "checkpoints": checkpoints,
            "meta": meta,
            "findings": findings,
        }))
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        let _logs = self.lock_logs();
        self.with_db(|db| {
            let txn = begin(db)?;
            db::set_meta(&txn, key, value)?;
            txn.commit()?;
            Ok(())
        })
    }

    pub fn get_meta(&self, key: &str) -> Result<Option<String>> {
        self.with_db(|db| {
            let read = db.begin_read()?;
            let table = read.open_table(db::META)?;
            Ok(table.get(key)?.map(|v| String::from_utf8_lossy(v.value()).into_owned()))
        })
    }

    /// Seals segments whose day has ended, then compresses sealed segments outside the lock.
    pub fn seal_and_compress(&self, now: u64) -> Result<Json> {
        let mut candidates = Vec::new();
        {
            let mut logs = self.lock_logs();
            for id in LogId::ALL {
                let log = logs.get(id);
                if let Some(active) = &log.manifest().active
                    && active.start_ms / DAY_MS < now / DAY_MS
                {
                    log.seal()?;
                }
                for c in log.compress_candidates() {
                    candidates.push((id, c));
                }
            }
        }
        let mut compressed = 0;
        for (id, (start, raw, sha)) in candidates {
            match Log::compress_file(&raw, &sha) {
                Ok((temp, bytes)) => {
                    let mut logs = self.lock_logs();
                    logs.get(id).finish_compress(start, &temp, bytes)?;
                    compressed += 1;
                }
                Err(e) => self
                    .findings
                    .lock()
                    .unwrap()
                    .push(finding("compress_failed", &e.message)),
            }
        }
        Ok(json!({ "compressed": compressed }))
    }

    /// Re-hashes sealed segments (weekly) and records any loss.
    pub fn verify(&self) -> Result<Json> {
        let mut found = Vec::new();
        {
            let mut logs = self.lock_logs();
            for id in LogId::ALL {
                found.append(&mut logs.get(id).verify_sealed()?);
            }
        }
        if !found.is_empty() {
            let events = found.clone();
            self.commit(LogId::Ledger, |batch| {
                for f in &events {
                    batch.push(Event::new(
                        "store.integrity_detected",
                        now_ms(),
                        crate::msgpack::from_json(&serde_json::to_value(f)?)?,
                    ))?;
                }
                Ok(())
            })?;
            self.findings.lock().unwrap().extend(found.iter().cloned());
        }
        self.set_meta("last_verify_ms", &now_ms().to_string())?;
        Ok(json!({ "findings": found }))
    }

    /// redb `compact()`, with readers and the writer held off, when it can save space: a trimmed
    /// file has no free pages, and below one 4 GiB region redb doubles the file at the next
    /// commit that needs one, so a file up to twice its used pages is left alone.
    pub fn compact(&self) -> Result<bool> {
        let _logs = self.lock_logs();
        let mut guard = self.db.write().unwrap_or_else(|p| p.into_inner());
        let Some(db) = guard.as_mut() else {
            fail!("closed", "the store is closed")
        };
        let used = {
            let txn = db.begin_write()?;
            let stats = txn.stats()?;
            let bytes = stats.allocated_pages() * stats.page_size() as u64;
            txn.abort()?;
            bytes
        };
        if fs::metadata(self.dir.join(REDB_FILE))?.len() <= used * 2 {
            return Ok(false);
        }
        // Each pass moves pages toward the start; repeat until nothing more moves.
        let mut done = false;
        for _ in 0..16 {
            if !db.compact().map_err(|e| Error::new("io", e.to_string()))? {
                break;
            }
            done = true;
        }
        drop(guard);
        drop(_logs);
        self.set_meta("last_compact_ms", &now_ms().to_string())?;
        Ok(done)
    }

    /// Seals both logs and marks a clean shutdown. The engine is unusable afterwards.
    pub fn close(&self) -> Result<()> {
        let mut logs = self.lock_logs();
        logs.readings.seal()?;
        logs.ledger.seal()?;
        let mut guard = self.db.write().unwrap_or_else(|p| p.into_inner());
        if let Some(db) = guard.as_ref() {
            let txn = begin(db)?;
            db::set_meta(&txn, "clean_shutdown", "true")?;
            txn.commit()?;
        }
        *guard = None;
        Ok(())
    }

    /// Writes a verified backup into the first root, then mirrors it into the others
    /// (docs/store.md 4.7). Sealed segments are pooled once per root; each backup lists them.
    pub fn backup(&self, roots: &[PathBuf], with_redb: bool, keep_daily: usize, keep_with_redb: usize) -> Result<Json> {
        let Some(primary) = roots.first() else {
            fail!("invalid", "no backup root")
        };
        let id = format!("{}-{}", iso_compact(now_ms()), fsx::random_hex(2));
        fsx::private_dir(&primary.join("sets"))?;
        let target = primary.join("sets").join(&id);
        let staging = primary.join("sets").join(format!(".{id}.partial"));
        fsx::private_dir(&staging)?;
        let mut files: Vec<Json> = Vec::new();
        let mut logs_json = serde_json::Map::new();
        let readings_sealed_through;
        {
            let logs = self.lock_logs();
            readings_sealed_through = logs
                .readings
                .manifest()
                .segments
                .iter()
                .map(|s| s.end_seq)
                .max()
                .unwrap_or(0);
            for log_id in LogId::ALL {
                let log = logs.peek(log_id);
                let pool = primary.join("pool").join(log_id.name());
                fsx::private_dir(&pool)?;
                let (sealed, active) = log.backup_files();
                let mut segments = Vec::new();
                for (name, path, _raw_sha, _) in sealed {
                    let dest = pool.join(&name);
                    if !dest.exists() {
                        fsx::copy_atomic(&path, &dest)?;
                    }
                    let (sha, bytes) = fsx::sha256_file(&dest)?;
                    let (source_sha, _) = fsx::sha256_file(&path)?;
                    if sha != source_sha {
                        // A pooled copy that no longer matches is replaced.
                        fsx::copy_atomic(&path, &dest)?;
                    }
                    let rel = format!("pool/{}/{}", log_id.name(), name);
                    files.push(json!({ "path": rel, "sha256": source_sha, "bytes": bytes }));
                    segments.push(Json::String(name));
                }
                let manifest_name = format!("{}.manifest.v1.msgpack", log_id.name());
                let manifest_bytes = log.manifest_bytes()?;
                fsx::write_atomic(&staging.join(&manifest_name), &manifest_bytes)?;
                files.push(json!({ "path": format!("sets/{id}/{manifest_name}"), "sha256": fsx::sha256_hex(&manifest_bytes), "bytes": manifest_bytes.len() }));
                let mut active_json = Json::Null;
                if let Some((name, path, len)) = active {
                    let mut bytes = fs::read(&path)?;
                    bytes.truncate(len as usize);
                    let dest_name = format!("{}.{}", log_id.name(), name);
                    fsx::write_atomic(&staging.join(&dest_name), &bytes)?;
                    files.push(json!({ "path": format!("sets/{id}/{dest_name}"), "sha256": fsx::sha256_hex(&bytes), "bytes": bytes.len() }));
                    active_json = json!({ "name": name, "file": dest_name, "bytes": len });
                }
                logs_json.insert(log_id.name().into(), json!({ "manifest": manifest_name, "segments": segments, "active": active_json, "lastSequence": log.last_seq() }));
            }
            if with_redb {
                let mut guard = self.db.write().unwrap_or_else(|p| p.into_inner());
                let db = guard.take();
                drop(db);
                let result = fsx::copy_atomic(&self.dir.join(REDB_FILE), &staging.join(REDB_FILE));
                let reopened = open_database(&self.dir.join(REDB_FILE), &self.config)
                    .map_err(|e| Error::new("db_unavailable", e.to_string()))?;
                *guard = Some(reopened);
                result?;
                let (sha, bytes) = fsx::sha256_file(&staging.join(REDB_FILE))?;
                files.push(json!({ "path": format!("sets/{id}/{REDB_FILE}"), "sha256": sha, "bytes": bytes }));
            }
        }
        let manifest = json!({
            "schema": "aac-store-backup@1",
            "id": id,
            "createdAt": now_ms(),
            "instanceId": self.instance_id,
            "storeSchemaVersion": db::STORE_SCHEMA_VERSION,
            "withRedb": with_redb,
            "logs": logs_json,
            "files": files,
        });
        // Verify every listed file before the backup is published.
        for file in manifest["files"].as_array().unwrap() {
            let rel = file["path"].as_str().unwrap();
            let path = if let Some(rest) = rel.strip_prefix(&format!("sets/{id}/")) {
                staging.join(rest)
            } else {
                primary.join(rel)
            };
            let (sha, _) = fsx::sha256_file(&path)?;
            if Some(sha.as_str()) != file["sha256"].as_str() {
                fail!("backup_unverified", "{rel} does not match its hash");
            }
        }
        fsx::write_atomic(
            &staging.join("backup.json"),
            serde_json::to_string_pretty(&manifest)?.as_bytes(),
        )?;
        fs::rename(&staging, &target)?;
        fsx::sync_dir(&primary.join("sets"))?;
        prune_backups(primary, keep_daily, keep_with_redb)?;

        let mut mirrors = Vec::new();
        for root in &roots[1..] {
            match mirror_backup(primary, root, &id, &manifest) {
                Ok(()) => {
                    prune_backups(root, keep_daily, keep_with_redb)?;
                    mirrors.push(json!({ "root": root.display().to_string(), "ok": true }));
                }
                Err(e) => {
                    mirrors.push(json!({ "root": root.display().to_string(), "ok": false, "error": e.to_string() }))
                }
            }
        }
        let event = Event::new(
            "store.backup_verified",
            now_ms(),
            crate::msgpack::from_json(&json!({
                "backup_id": id, "with_redb": with_redb, "files": manifest["files"].as_array().unwrap().len(),
                "mirrors": mirrors.iter().filter(|m| m["ok"] == true).count(),
            }))?,
        );
        self.commit(LogId::Ledger, |batch| batch.push(event).map(|_| ()))?;
        self.set_meta("last_backup_ms", &now_ms().to_string())?;
        // Readings segments up to here may retire: a verified backup holds them.
        self.set_meta("backup_readings_seq", &readings_sealed_through.to_string())?;
        if mirrors.iter().any(|m| m["ok"] == true) {
            self.set_meta("last_share_backup_ms", &now_ms().to_string())?;
        }
        let readings_last = manifest["logs"]["readings"]["segments"]
            .as_array()
            .map(|s| s.len())
            .unwrap_or(0);
        Ok(
            json!({ "id": id, "files": manifest["files"].as_array().unwrap().len(), "readingsSegments": readings_last, "mirrors": mirrors }),
        )
    }
}

pub struct Batch<'a> {
    pub txn: &'a WriteTransaction,
    log: &'a mut Log,
    log_id: LogId,
    engine: &'a Engine,
    checkpoints: Vec<(usize, Checkpoint)>,
    events: Vec<Event>,
}

impl Batch<'_> {
    /// Assigns the event its sequence, projects it into this transaction, and queues it.
    pub fn push(&mut self, mut event: Event) -> Result<u64> {
        if self.events.len() >= PROJECTOR_BATCH {
            fail!("too_large", "a batch holds at most {PROJECTOR_BATCH} events");
        }
        self.log.assign(&mut event)?;
        for (i, cp) in self.checkpoints.iter_mut() {
            self.engine.projectors[*i].apply(self.txn, self.log_id, &event)?;
            cp.advance(event.sequence_id, &event.event_id);
        }
        let seq = event.sequence_id;
        self.events.push(event);
        Ok(seq)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }
}

pub fn finding(kind: &str, detail: &str) -> Finding {
    Finding {
        kind: kind.into(),
        detail: detail.into(),
        start_seq: None,
        end_seq: None,
    }
}

fn quarantine(dir: &Path, path: &Path, reason: &str) -> Result<()> {
    let target = dir
        .join("quarantine")
        .join(format!("{}-{reason}", iso_compact(now_ms())));
    fsx::private_dir(&target)?;
    if path.exists() {
        fs::rename(path, target.join(path.file_name().unwrap()))?;
    }
    fsx::sync_dir(dir)
}

fn read_instance_file(dir: &Path) -> Result<Option<String>> {
    match fs::read_to_string(dir.join("log").join("instance-id")) {
        Ok(s) if s.trim().len() == 32 => Ok(Some(s.trim().to_string())),
        _ => Ok(None),
    }
}

fn write_instance_file(dir: &Path, id: &str) -> Result<()> {
    fsx::private_dir(&dir.join("log"))?;
    let path = dir.join("log").join("instance-id");
    if fs::read_to_string(&path).map(|s| s.trim() == id).unwrap_or(false) {
        return Ok(());
    }
    fsx::write_atomic(&path, id.as_bytes())
}

/// `YYYYMMDDTHHMMSSZ` for ids and folder names (UTC, no clock crate).
pub fn iso_compact(ms: u64) -> String {
    let secs = ms / 1000;
    let (y, m, d) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

/// Howard Hinnant's days-to-civil algorithm.
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn mirror_backup(primary: &Path, root: &Path, id: &str, manifest: &Json) -> Result<()> {
    fsx::private_dir(&root.join("sets"))?;
    let staging = root.join("sets").join(format!(".{id}.partial"));
    fsx::private_dir(&staging)?;
    for file in manifest["files"].as_array().unwrap() {
        let rel = file["path"].as_str().unwrap();
        let sha = file["sha256"].as_str().unwrap();
        let (source, dest) = if let Some(rest) = rel.strip_prefix(&format!("sets/{id}/")) {
            (primary.join("sets").join(id).join(rest), staging.join(rest))
        } else {
            (primary.join(rel), root.join(rel))
        };
        if let Some(parent) = dest.parent() {
            fsx::private_dir(parent)?;
        }
        let present = dest.exists() && fsx::sha256_file(&dest).map(|(s, _)| s == sha).unwrap_or(false);
        if !present {
            fsx::copy_atomic(&source, &dest)?;
            if fsx::sha256_file(&dest)?.0 != sha {
                fail!("backup_unverified", "mirror copy of {rel} does not match");
            }
        }
    }
    fsx::copy_atomic(
        &primary.join("sets").join(id).join("backup.json"),
        &staging.join("backup.json"),
    )?;
    fs::rename(&staging, root.join("sets").join(id))?;
    fsx::sync_dir(&root.join("sets"))
}

/// Keeps the newest `keep_daily` backups plus the newest `keep_with_redb` holding a redb
/// snapshot, then removes pooled segments no kept backup lists.
pub fn prune_backups(root: &Path, keep_daily: usize, keep_with_redb: usize) -> Result<()> {
    let dir = root.join("sets");
    let mut backups: Vec<(String, Json)> = Vec::new();
    for entry in fs::read_dir(&dir)?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            if name.ends_with(".partial") {
                let _ = fs::remove_dir_all(entry.path());
            }
            continue;
        }
        if let Ok(text) = fs::read_to_string(entry.path().join("backup.json"))
            && let Ok(json) = serde_json::from_str::<Json>(&text)
        {
            backups.push((name, json));
        }
    }
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    let mut keep = std::collections::HashSet::new();
    for (name, _) in backups.iter().take(keep_daily) {
        keep.insert(name.clone());
    }
    for (name, _) in backups
        .iter()
        .filter(|(_, j)| j["withRedb"] == true)
        .take(keep_with_redb)
    {
        keep.insert(name.clone());
    }
    let mut referenced = std::collections::HashSet::new();
    for (name, json) in &backups {
        if keep.contains(name) {
            for file in json["files"].as_array().into_iter().flatten() {
                if let Some(path) = file["path"].as_str() {
                    referenced.insert(path.to_string());
                }
            }
        } else {
            fs::remove_dir_all(dir.join(name))?;
        }
    }
    for log in ["readings", "ledger"] {
        let pool = root.join("pool").join(log);
        if let Ok(entries) = fs::read_dir(&pool) {
            for entry in entries.flatten() {
                let rel = format!("pool/{log}/{}", entry.file_name().to_string_lossy());
                if !referenced.contains(&rel) {
                    let _ = fs::remove_file(entry.path());
                }
            }
        }
    }
    Ok(())
}

/// Restores a verified backup with the dashboard stopped (docs/store.md 4.7). Current files go
/// to `quarantine/`; an interrupted restore is finished by the next open from its intent.
pub fn restore(dir: &Path, backup_dir: &Path) -> Result<Json> {
    fsx::private_dir(dir)?;
    let _lock = fsx::lock_store(dir)?;
    finish_restore(dir)?;
    let text = fs::read_to_string(backup_dir.join("backup.json"))?;
    let manifest: Json = serde_json::from_str(&text)?;
    if manifest["schema"] != "aac-store-backup@1" {
        fail!("invalid", "not an AAC store backup");
    }
    let id = manifest["id"].as_str().unwrap_or_default().to_string();
    if manifest["storeSchemaVersion"].as_u64().unwrap_or(u64::MAX) > db::STORE_SCHEMA_VERSION {
        fail!("newer_store", "the backup is from a newer store");
    }
    let root = backup_dir
        .parent()
        .and_then(|p| p.parent())
        .ok_or_else(|| Error::new("invalid", "backup folder layout"))?;
    let resolve = |rel: &str| -> PathBuf {
        match rel.strip_prefix(&format!("sets/{id}/")) {
            Some(rest) => backup_dir.join(rest),
            None => root.join(rel),
        }
    };
    for file in manifest["files"].as_array().into_iter().flatten() {
        let rel = file["path"].as_str().unwrap_or_default();
        let (sha, _) = fsx::sha256_file(&resolve(rel))?;
        if Some(sha.as_str()) != file["sha256"].as_str() {
            fail!("backup_unverified", "{rel} does not match its hash");
        }
    }
    let staging = dir.join("tmp").join(format!("restore-{id}"));
    if staging.exists() {
        fs::remove_dir_all(&staging)?;
    }
    for log in ["readings", "ledger"] {
        let log_dir = staging.join("log").join(log);
        fsx::private_dir(&log_dir)?;
        let info = &manifest["logs"][log];
        for seg in info["segments"].as_array().into_iter().flatten() {
            let name = seg.as_str().unwrap_or_default();
            fsx::copy_atomic(&root.join("pool").join(log).join(name), &log_dir.join(name))?;
        }
        let manifest_name = info["manifest"].as_str().unwrap_or_default();
        fsx::copy_atomic(&backup_dir.join(manifest_name), &log_dir.join("manifest.v1.msgpack"))?;
        if let Some(active) = info["active"].as_object() {
            let file = active["file"].as_str().unwrap_or_default();
            let name = active["name"].as_str().unwrap_or_default();
            fsx::copy_atomic(&backup_dir.join(file), &log_dir.join(name))?;
        }
    }
    if let Some(instance) = manifest["instanceId"].as_str() {
        fsx::write_atomic(&staging.join("log").join("instance-id"), instance.as_bytes())?;
    }
    if manifest["withRedb"] == true {
        fsx::copy_atomic(&backup_dir.join(REDB_FILE), &staging.join(REDB_FILE))?;
    }
    let quarantine_dir = dir
        .join("quarantine")
        .join(format!("{}-pre-restore", iso_compact(now_ms())));
    let intent = json!({ "staging": staging.display().to_string(), "quarantine": quarantine_dir.display().to_string(), "backup": id });
    fsx::write_atomic(&dir.join(RESTORE_INTENT), intent.to_string().as_bytes())?;
    finish_restore(dir)?;
    Ok(json!({ "restored": id, "quarantine": quarantine_dir.display().to_string() }))
}

/// Completes a restore from its intent: move current files aside, promote the staged ones.
fn finish_restore(dir: &Path) -> Result<()> {
    let intent_path = dir.join(RESTORE_INTENT);
    let Ok(text) = fs::read_to_string(&intent_path) else {
        return Ok(());
    };
    let intent: Json = serde_json::from_str(&text)?;
    let staging = PathBuf::from(intent["staging"].as_str().unwrap_or_default());
    let quarantine_dir = PathBuf::from(intent["quarantine"].as_str().unwrap_or_default());
    fsx::private_dir(&quarantine_dir)?;
    if staging.join("log").exists() {
        if dir.join("log").exists() {
            fs::rename(dir.join("log"), quarantine_dir.join("log"))?;
        }
        fs::rename(staging.join("log"), dir.join("log"))?;
    }
    if dir.join(REDB_FILE).exists() && !quarantine_dir.join(REDB_FILE).exists() {
        fs::rename(dir.join(REDB_FILE), quarantine_dir.join(REDB_FILE))?;
    }
    if staging.join(REDB_FILE).exists() {
        fs::rename(staging.join(REDB_FILE), dir.join(REDB_FILE))?;
    }
    fsx::sync_dir(dir)?;
    let _ = fs::remove_dir_all(&staging);
    fs::remove_file(&intent_path)?;
    fsx::sync_dir(dir)
}
