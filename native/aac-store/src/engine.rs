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
/// The store's release, written when this build creates a store: `1`. Stores written by the
/// branch's unreleased test builds have logs or a redb file but no marker.
const RELEASE_FILE: &str = "release";
pub const STORE_RELEASE: u32 = 1;
const UNRELEASED: &str =
    "This store was written by an unreleased test build; move it aside and AAC will start a new one.";

/// Admits a store of this release, or creates the marker for a new one; refuses anything else
/// before it is changed.
fn admit_release(dir: &Path) -> Result<()> {
    let marker = dir.join(RELEASE_FILE);
    match fs::read_to_string(&marker) {
        Ok(text) => match text.trim().parse::<u32>() {
            Ok(STORE_RELEASE) => Ok(()),
            Ok(n) if n > STORE_RELEASE => fail!(
                "newer_store",
                "store release {n} is newer than this build ({STORE_RELEASE})"
            ),
            _ => fail!("unreleased_store", "{UNRELEASED}"),
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let has_data = dir.join("log").exists() || dir.join(REDB_FILE).exists();
            if has_data {
                fail!("unreleased_store", "{UNRELEASED}");
            }
            fsx::write_atomic(&marker, STORE_RELEASE.to_string().as_bytes())
        }
        Err(e) => Err(e.into()),
    }
}
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
    /// Projects one event. What it had to set aside (an event an older build accepted that
    /// today's checks refuse) comes back as findings, and replay goes on.
    fn apply(&self, txn: &WriteTransaction, log: LogId, event: &Event) -> Result<Vec<Finding>>;
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

/// Opens the existing redb file with the bounded cache; it never creates one. Below the size
/// limit every open verifies every page checksum (`set_check_integrity_on_open`), so an altered
/// file fails with `Corrupted` instead of being trusted.
fn open_database(path: &Path, config: &Config) -> std::result::Result<Database, redb::DatabaseError> {
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut builder = Database::builder();
    builder.set_cache_size(config.cache_bytes);
    builder.set_check_integrity_on_open(size < config.integrity_max_bytes);
    builder.open(path)
}

/// Prefix of a redb file being made under `tmp/`.
const REDB_MAKING: &str = "aac.redb.";

/// Makes a new, empty redb under `tmp/` and renames it into place only after its first commit.
/// redb writes its magic number last, so a kill mid-creation would otherwise leave an
/// `aac.redb` that never opens; here it leaves a temporary file the next open removes.
fn create_database(dir: &Path, config: &Config) -> Result<()> {
    let making = dir.join("tmp").join(format!("{REDB_MAKING}{}", fsx::random_hex(8)));
    {
        let mut builder = Database::builder();
        builder.set_cache_size(config.cache_bytes);
        let database = builder
            .create(&making)
            .map_err(|e| Error::new("db_unavailable", e.to_string()))?;
        let txn = begin(&database)?;
        db::create_tables(&txn)?;
        txn.commit()?;
    }
    fs::rename(&making, dir.join(REDB_FILE))?;
    fsx::sync_dir(dir)
}

/// Removes redb files a killed creation left under `tmp/` (the store lock is held).
fn remove_unfinished_redb(dir: &Path) -> Result<()> {
    for entry in fs::read_dir(dir.join("tmp"))?.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(REDB_MAKING) && entry.file_type()?.is_file() {
            fs::remove_file(entry.path())?;
        }
    }
    Ok(())
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
        admit_release(dir)?;
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
        remove_unfinished_redb(dir)?;
        if !db_path.exists() {
            create_database(dir, &config)?;
        }
        // Every table is derived from the logs, so a redb that fails to open for any reason but
        // another holder (corrupt, never finished, empty, unreadable) is moved to quarantine and
        // rebuilt; it is never deleted.
        let database = match open_database(&db_path, &config) {
            Ok(db) => db,
            Err(redb::DatabaseError::DatabaseAlreadyOpen) => fail!("lock_held", "the database is open elsewhere"),
            Err(e) => {
                let reason = match e {
                    redb::DatabaseError::Storage(redb::StorageError::Corrupted(_)) => "redb_corrupt",
                    _ => "redb_unreadable",
                };
                quarantine(dir, &db_path, reason)?;
                findings.push(finding("redb_quarantined", &e.to_string()));
                create_database(dir, &config)?;
                open_database(&db_path, &config).map_err(|e| Error::new("db_unavailable", e.to_string()))?
            }
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
        let replay_findings = engine.replay()?;
        merge_findings(&mut findings, replay_findings);
        // What earlier opens disclosed stays disclosed until it is resolved (2.6), whatever the
        // checkpoint: kept in redb, and rebuilt from the ledger's `store.integrity_detected`.
        let kept = engine.with_db(kept_findings)?;
        let ledgered: Vec<String> = kept.iter().filter(|(_, l)| *l).map(|(f, _)| finding_key(f)).collect();
        merge_findings(&mut findings, kept.into_iter().map(|(f, _)| f));
        let loss: Vec<_> = findings
            .iter()
            .filter(|f| is_loss(&f.kind) && !ledgered.contains(&finding_key(f)))
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
        engine.commit(LogId::Ledger, |batch| batch.push(opened).map(|_| ()))?;
        // Each disclosed finding is already kept in redb with the rows it explains, marked not yet
        // ledgered; each commit below ledgers a group and marks it, so a kill between groups
        // leaves the rest to the next open, and none is written twice. One group holds at most
        // a projector batch.
        for group in loss.chunks(PROJECTOR_BATCH - 1) {
            engine.commit(LogId::Ledger, |batch| {
                for f in group {
                    let event = Event::new(
                        "store.integrity_detected",
                        now_ms(),
                        crate::msgpack::from_json(&serde_json::to_value(f)?)?,
                    );
                    batch.push(event)?;
                }
                Ok(())
            })?;
        }
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

    /// A projector's first checkpoint in a log: its chain starts where the log's retired events end.
    fn fresh_checkpoint(projector: &dyn Projector, log_id: LogId, log: &Log) -> Checkpoint {
        Checkpoint {
            log: log_id.name().into(),
            segment_generation: crate::log::GENERATION,
            projector_schema_version: projector.version(),
            survivor_prefix_sha256: log.base_digest().unwrap_or_default(),
            ..Default::default()
        }
    }

    /// Checks every checkpoint against the events each log still holds (docs/store.md 2.6): a
    /// projection built from events the log has lost, or whose chain cannot be checked, is stale.
    /// Returns the stale projectors and the findings, the logs' own included.
    fn check_checkpoints(&self, logs: &mut Logs, db: &Database) -> Result<(Vec<usize>, Vec<Finding>)> {
        let mut stale = Vec::new();
        let mut findings = Vec::new();
        for log_id in LogId::ALL {
            let mut checkpoints: Vec<(usize, Checkpoint)> = Vec::new();
            {
                let read = db.begin_read()?;
                let table = read.open_table(db::CHECKPOINT)?;
                for (i, projector) in self.projectors.iter().enumerate() {
                    if projector.reads(log_id)
                        && let Some(cp) =
                            db::read_checkpoint(&table, &Self::checkpoint_name(projector.as_ref(), log_id))?
                    {
                        checkpoints.push((i, cp));
                    }
                }
            }
            let log = logs.peek(log_id);
            let base = log.base_digest();
            let mut chain = base.clone();
            // For each checkpoint: the last surviving event at or below it, and the chain there.
            let mut reached: Vec<Option<(u64, Option<String>)>> = vec![None; checkpoints.len()];
            let log_findings = log.read_ids(|sequence, event_id| {
                chain = chain.as_deref().map(|c| db::chain_digest(c, sequence, event_id));
                for (k, (_, cp)) in checkpoints.iter().enumerate() {
                    if sequence <= cp.last_sequence_id {
                        reached[k] = Some((sequence, chain.clone()));
                    }
                }
            })?;
            findings.extend(log_findings.iter().cloned());
            let retired_below = log.manifest().retired_below;
            let mut reanchor = false;
            for (k, (i, cp)) in checkpoints.iter().enumerate() {
                if cp.last_sequence_id == 0 {
                    continue;
                }
                let matches = match &reached[k] {
                    Some((seq, Some(digest))) => *seq == cp.last_sequence_id && *digest == cp.survivor_prefix_sha256,
                    Some((_, None)) => false,
                    // Everything it was built from has retired: the retirement chain must end there.
                    None => {
                        base.as_deref() == Some(cp.survivor_prefix_sha256.as_str())
                            && retired_below == cp.last_sequence_id + 1
                    }
                };
                if matches {
                    continue;
                }
                let projector = self.projectors[*i].name();
                let (kind, detail) = if base.is_none() {
                    reanchor = true;
                    (
                        "projection_unverifiable_missing",
                        format!(
                            "the {} log's retired events are not recorded, so the {projector} tables could not be checked against it; they were rebuilt from the surviving events",
                            log_id.name()
                        ),
                    )
                } else {
                    (
                        "projection_missing_events",
                        format!(
                            "the {} log no longer holds every event the {projector} tables were built from; they were rebuilt from the surviving events",
                            log_id.name()
                        ),
                    )
                };
                // Where the loss is: past the last surviving event when the tail is gone, and from
                // the first damaged or missing segment at or below the checkpoint. When neither
                // says, the whole range the checkpoint covers. Always ordered.
                let end = cp.last_sequence_id;
                let lower = retired_below.max(1).min(end);
                let mut start = match &reached[k] {
                    Some((seq, _)) if *seq < end => seq + 1,
                    _ => lower,
                };
                if let Some(damaged) = log_findings
                    .iter()
                    .filter_map(|f| f.start_seq)
                    .filter(|s| *s <= end)
                    .min()
                {
                    start = start.min(damaged.max(lower));
                }
                findings.push(Finding {
                    kind: kind.into(),
                    detail,
                    start_seq: Some(start.clamp(lower, end)),
                    end_seq: Some(end),
                });
                if !stale.contains(i) {
                    stale.push(*i);
                }
            }
            if reanchor {
                // Rebuilt projections start their chain at the first surviving event.
                logs.get(log_id).set_base_digest("")?;
            }
        }
        Ok((stale, findings))
    }

    /// Brings every projector up to the end of both logs, rebuilding any whose version changed or
    /// whose checkpoint does not match the surviving events.
    fn replay(&self) -> Result<Vec<Finding>> {
        let mut logs = self.lock_logs();
        self.with_db(|db| {
            let (lost, mut findings) = self.check_checkpoints(&mut logs, db)?;
            let txn = begin(db)?;
            // Kept in the same transaction as the rebuild they cause, so a kill before the ledger
            // records them cannot leave a rebuilt store that no longer says why.
            for f in findings.iter().filter(|f| is_loss(&f.kind)) {
                keep_finding(&txn, f, false)?;
            }
            for (index, projector) in self.projectors.iter().enumerate() {
                let mut stale = lost.contains(&index);
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
                                .unwrap_or_else(|| Self::fresh_checkpoint(projector.as_ref(), log_id, log));
                            checkpoints.push((i, cp));
                        }
                    }
                }
                if checkpoints.is_empty() {
                    continue;
                }
                let floor = checkpoints.iter().map(|(_, cp)| cp.last_sequence_id).min().unwrap_or(0);
                let mut pending: Vec<Event> = Vec::new();
                let flush = |pending: &mut Vec<Event>,
                             checkpoints: &mut Vec<(usize, Checkpoint)>,
                             found: &mut Vec<Finding>|
                 -> Result<()> {
                    if pending.is_empty() {
                        return Ok(());
                    }
                    let txn = begin(db)?;
                    for event in pending.iter() {
                        for (i, cp) in checkpoints.iter_mut() {
                            if event.sequence_id > cp.last_sequence_id {
                                let ledgered = event.event_type == "store.integrity_detected";
                                for f in self.projectors[*i].apply(&txn, log_id, event)? {
                                    keep_finding(&txn, &f, ledgered)?;
                                    found.push(f);
                                }
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
                // The check above already reported what the logs hold; this pass only projects.
                log.read_after(if floor == 0 { None } else { Some(floor) }, |event| {
                    pending.push(event);
                    if pending.len() >= PROJECTOR_BATCH {
                        flush(&mut pending, &mut checkpoints, &mut findings)?;
                    }
                    Ok(())
                })?;
                flush(&mut pending, &mut checkpoints, &mut findings)?;
            }
            Ok(findings)
        })
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
                let log = logs.peek(log_id);
                for (i, projector) in self.projectors.iter().enumerate() {
                    if projector.reads(log_id) {
                        let cp = db::read_checkpoint(&table, &Self::checkpoint_name(projector.as_ref(), log_id))?
                            .unwrap_or_else(|| Self::fresh_checkpoint(projector.as_ref(), log_id, log));
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
            merge_findings(&mut self.findings.lock().unwrap(), log.length_loss_findings());
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
        let persisted_loss = [&logs.readings, &logs.ledger]
            .iter()
            .any(|log| log.manifest().segments.iter().any(|s| s.state != "sealed"));
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
            "state": if persisted_loss || findings.iter().any(|f| is_loss(&f.kind)) {
                "degraded"
            } else {
                "ready"
            },
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
        let mut noted = Vec::new();
        {
            let mut logs = self.lock_logs();
            for id in LogId::ALL {
                let log = logs.get(id);
                if let Some(active) = &log.manifest().active
                    && active.start_ms / DAY_MS < now / DAY_MS
                {
                    log.seal()?;
                }
                noted.extend(log.length_loss_findings());
                for c in log.compress_candidates() {
                    candidates.push((id, c));
                }
            }
        }
        merge_findings(&mut self.findings.lock().unwrap(), noted);
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
        // Log each detection once: a read-path loss already in findings must stay disclosed
        // but must not be re-ledgered by its first verification.
        let known: Vec<String> = self
            .findings
            .lock()
            .unwrap()
            .iter()
            .filter(|f| is_loss(&f.kind))
            .map(finding_key)
            .collect();
        let fresh: Vec<Finding> = found
            .iter()
            .filter(|f| !known.contains(&finding_key(f)))
            .cloned()
            .collect();
        if !fresh.is_empty() {
            let events = fresh.clone();
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
            merge_findings(&mut self.findings.lock().unwrap(), fresh);
        }
        self.set_meta("last_verify_ms", &now_ms().to_string())?;
        // A later check must still disclose previously detected loss.
        for known in self.findings.lock().unwrap().iter().filter(|f| is_loss(&f.kind)) {
            if !found.iter().any(|f| finding_key(f) == finding_key(known)) {
                found.push(known.clone());
            }
        }
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
        let mut noted = logs.readings.length_loss_findings();
        noted.extend(logs.ledger.length_loss_findings());
        merge_findings(&mut self.findings.lock().unwrap(), noted);
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
                // Verify decoded bytes against the sealed manifest, even for degraded logs.
                for entry in &log.manifest().segments {
                    if entry.state == "missing" {
                        fail!("backup_unverified", "live segment is missing");
                    }
                    let valid = crate::log::read_segment_bytes(&self.dir.join("log").join(log_id.name()), entry)
                        .map(|bytes| fsx::sha256_hex(&bytes) == entry.raw_sha256)
                        .unwrap_or(false);
                    if !valid {
                        fail!("backup_unverified", "live segment does not match its sealed hash");
                    }
                }
                let (sealed, active) = log.backup_files();
                let mut segments = Vec::new();
                for (name, path, _raw_sha, _) in sealed {
                    let dest = pool.join(&name);
                    let (source_sha, bytes) = fsx::sha256_file(&path)?;
                    if dest.exists() {
                        if fsx::sha256_file(&dest)?.0 != source_sha {
                            fail!(
                                "backup_unverified",
                                "pooled segment differs; existing backup left intact"
                            );
                        }
                    } else {
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
            "storeRelease": STORE_RELEASE,
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
            let set_aside = self.engine.projectors[*i].apply(self.txn, self.log_id, &event)?;
            let ledgered = event.event_type == "store.integrity_detected";
            for f in &set_aside {
                keep_finding(self.txn, f, ledgered)?;
            }
            if !set_aside.is_empty() {
                merge_findings(&mut self.engine.findings.lock().unwrap(), set_aside);
            }
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

/// A finding that means history is missing: disclosed in the ledger, and the store
/// reports itself degraded.
pub fn is_loss(kind: &str) -> bool {
    kind.contains("degraded") || kind.contains("missing") || kind == "segment_unreadable"
}

pub fn finding(kind: &str, detail: &str) -> Finding {
    Finding {
        kind: kind.into(),
        detail: detail.into(),
        start_seq: None,
        end_seq: None,
    }
}

/// The key a finding is kept under: one entry per kind and range.
fn finding_key(f: &Finding) -> String {
    format!("{}:{}:{}", f.kind, f.start_seq.unwrap_or(0), f.end_seq.unwrap_or(0))
}

/// Keeps a disclosed finding in redb, in the projection's own transaction. `ledgered` says the
/// ledger already holds it as `store.integrity_detected`.
fn keep_finding(txn: &WriteTransaction, f: &Finding, ledgered: bool) -> Result<()> {
    let key = finding_key(f);
    let mut table = txn.open_table(db::FINDING)?;
    let already = table
        .get(key.as_str())?
        .and_then(|v| crate::msgpack::from_bytes::<Json>(v.value()).ok())
        .is_some_and(|v| v["ledgered"] == true);
    let value = json!({ "finding": serde_json::to_value(f)?, "ledgered": ledgered || already });
    table.insert(
        key.as_str(),
        crate::msgpack::encode(&crate::msgpack::from_json(&value)?)?.as_slice(),
    )?;
    Ok(())
}

/// The findings kept by earlier opens, with whether the ledger holds each.
fn kept_findings(db: &Database) -> Result<Vec<(Finding, bool)>> {
    let read = db.begin_read()?;
    let table = read.open_table(db::FINDING)?;
    let mut out = Vec::new();
    for item in table.iter()? {
        let (_, v) = item?;
        let value: Json = crate::msgpack::from_bytes(v.value())?;
        out.push((finding_from_json(&value["finding"]), value["ledgered"] == true));
    }
    Ok(out)
}

/// A finding read back from the ledger or from redb.
pub fn finding_from_json(v: &Json) -> Finding {
    Finding {
        kind: v["kind"].as_str().unwrap_or_default().to_string(),
        detail: v["detail"].as_str().unwrap_or_default().to_string(),
        start_seq: v["start_seq"].as_u64(),
        end_seq: v["end_seq"].as_u64(),
    }
}

/// Adds findings not already listed (by kind and range).
fn merge_findings(into: &mut Vec<Finding>, more: impl IntoIterator<Item = Finding>) {
    for f in more {
        let key = finding_key(&f);
        if !into.iter().any(|g| finding_key(g) == key) {
            into.push(f);
        }
    }
}

fn quarantine(dir: &Path, path: &Path, reason: &str) -> Result<()> {
    let parent = dir.join("quarantine");
    fsx::private_dir(&parent)?;
    let base = format!("{}-{reason}", iso_compact(now_ms()));
    // Each recovery gets its own folder, created exclusively: a second
    // recovery in the same second must never reuse (and overwrite) the
    // first quarantined copy, so retry with a fresh suffix on collision.
    let mut attempt = 0u32;
    let target = loop {
        let name = if attempt == 0 {
            base.clone()
        } else {
            format!("{base}-{attempt}-{}", fsx::random_hex(4))
        };
        let candidate = parent.join(name);
        match fs::create_dir(&candidate) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    fs::set_permissions(&candidate, fs::Permissions::from_mode(0o700))?;
                }
                break candidate;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                attempt += 1;
                if attempt >= 100 {
                    return Err(e.into());
                }
            }
            Err(e) => return Err(e.into()),
        }
    };
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
        if !backup_rel_ok(rel, id) {
            fail!("invalid", "a backup file path is outside the backup");
        }
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
            if dest.exists() {
                fail!(
                    "backup_unverified",
                    "mirror segment differs; existing backup left intact"
                );
            }
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

/// A name a backup may use for a file or folder: one path component of safe characters.
fn backup_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-' || b == b'_')
}

/// A backup file path: `pool/<log>/<name>` or `sets/<id>/<name>`, nothing else.
fn backup_rel_ok(rel: &str, id: &str) -> bool {
    let parts: Vec<&str> = rel.split('/').collect();
    match parts.as_slice() {
        ["pool", log, name] => ["readings", "ledger"].contains(log) && backup_name_ok(name),
        ["sets", set, name] => *set == id && backup_name_ok(name),
        _ => false,
    }
}

/// `path` is a regular file (not a link) whose folders up to `root` are not links either.
fn plain_file_under(root: &Path, path: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return false;
    };
    if !meta.file_type().is_file() {
        return false;
    }
    let mut at = path.parent();
    while let Some(dir) = at {
        if dir == root {
            break;
        }
        match fs::symlink_metadata(dir) {
            Ok(m) if m.file_type().is_dir() => at = dir.parent(),
            _ => return false,
        }
    }
    match (fs::canonicalize(root), fs::canonicalize(path)) {
        (Ok(r), Ok(f)) => f.starts_with(r),
        _ => false,
    }
}

/// What a validated backup restores: each file's source, its place in the staged store, its hash.
struct RestorePlan {
    id: String,
    instance: Option<String>,
    copies: Vec<(PathBuf, PathBuf, String)>,
}

/// Validates a whole backup manifest before anything is changed: names, paths, hashes, and that
/// every file it restores is listed with its hash, inside the backup root and not a link.
fn plan_restore(backup_dir: &Path) -> Result<RestorePlan> {
    let refuse = |why: &str| -> Result<RestorePlan> { fail!("invalid", "the backup manifest {why}") };
    let text = fs::read_to_string(backup_dir.join("backup.json"))?;
    let manifest: Json = serde_json::from_str(&text)?;
    if manifest["schema"] != "aac-store-backup@1" {
        fail!("invalid", "not an AAC store backup");
    }
    if manifest["storeSchemaVersion"].as_u64().unwrap_or(u64::MAX) > db::STORE_SCHEMA_VERSION {
        fail!("newer_store", "the backup is from a newer store");
    }
    match manifest["storeRelease"].as_u64() {
        Some(n) if n == STORE_RELEASE as u64 => {}
        Some(n) if n > STORE_RELEASE as u64 => fail!("newer_store", "the backup is from a newer store"),
        _ => fail!("unreleased_store", "{UNRELEASED}"),
    }
    let Some(id) = manifest["id"].as_str().filter(|id| backup_name_ok(id)) else {
        return refuse("has an invalid id");
    };
    // The backup folder is `<root>/sets/<id>`; the pool sits beside `sets`.
    let sets = backup_dir.parent();
    if backup_dir.file_name().and_then(|n| n.to_str()) != Some(id)
        || sets.and_then(|p| p.file_name()).and_then(|n| n.to_str()) != Some("sets")
    {
        return refuse("does not match its folder");
    }
    let root = sets.and_then(|p| p.parent()).unwrap().to_path_buf();
    let instance = match &manifest["instanceId"] {
        Json::Null => None,
        Json::String(v) if v.len() == 32 && v.bytes().all(|b| b.is_ascii_hexdigit()) => Some(v.clone()),
        _ => return refuse("has an invalid instance id"),
    };
    let mut hashes: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    for file in manifest["files"].as_array().into_iter().flatten() {
        let (Some(rel), Some(sha)) = (file["path"].as_str(), file["sha256"].as_str()) else {
            return refuse("lists a file without a path or a hash");
        };
        if !backup_rel_ok(rel, id) || sha.len() != 64 || !sha.bytes().all(|b| b.is_ascii_hexdigit()) {
            return refuse("lists a file outside the backup");
        }
        hashes.insert(rel.to_string(), sha.to_ascii_lowercase());
    }
    let mut copies = Vec::new();
    let mut want = |rel: String, dest: PathBuf| -> Result<()> {
        let Some(sha) = hashes.get(&rel) else {
            fail!(
                "invalid",
                "the backup manifest restores a file it does not list with a hash"
            );
        };
        copies.push((
            root.join(rel.replace('/', std::path::MAIN_SEPARATOR_STR)),
            dest,
            sha.clone(),
        ));
        Ok(())
    };
    for log in ["readings", "ledger"] {
        let info = &manifest["logs"][log];
        let log_dir = PathBuf::from("log").join(log);
        for seg in info["segments"].as_array().into_iter().flatten() {
            let Some(name) = seg.as_str().filter(|n| backup_name_ok(n) && n.starts_with("seg-")) else {
                return refuse("names an invalid segment");
            };
            want(format!("pool/{log}/{name}"), log_dir.join(name))?;
        }
        let Some(name) = info["manifest"].as_str().filter(|n| backup_name_ok(n)) else {
            return refuse("names an invalid log manifest");
        };
        want(format!("sets/{id}/{name}"), log_dir.join("manifest.v1.msgpack"))?;
        match &info["active"] {
            Json::Null => {}
            Json::Object(active) => {
                let file = active
                    .get("file")
                    .and_then(|v| v.as_str())
                    .filter(|n| backup_name_ok(n));
                let name = active
                    .get("name")
                    .and_then(|v| v.as_str())
                    .filter(|n| backup_name_ok(n) && n.starts_with("seg-") && n.ends_with(".active"));
                let (Some(file), Some(name)) = (file, name) else {
                    return refuse("names an invalid active segment");
                };
                want(format!("sets/{id}/{file}"), log_dir.join(name))?;
            }
            _ => return refuse("names an invalid active segment"),
        }
    }
    match &manifest["withRedb"] {
        Json::Bool(true) => want(format!("sets/{id}/{REDB_FILE}"), PathBuf::from(REDB_FILE))?,
        Json::Bool(false) => {}
        _ => return refuse("has an invalid redb flag"),
    }
    for (source, _, _) in &copies {
        if !plain_file_under(&root, source) {
            return refuse("restores a file that is missing, a link, or outside the backup");
        }
    }
    Ok(RestorePlan {
        id: id.to_string(),
        instance,
        copies,
    })
}

/// Restores a verified backup with the dashboard stopped (docs/store.md 4.7). The whole manifest
/// is validated and every file hashed before anything changes; current files go to
/// `quarantine/`; an interrupted restore is finished by the next open from its intent.
pub fn restore(dir: &Path, backup_dir: &Path) -> Result<Json> {
    let plan = plan_restore(backup_dir)?;
    for (source, _, sha) in &plan.copies {
        let (actual, _) = fsx::sha256_file(source)?;
        if actual != *sha {
            fail!("backup_unverified", "a backup file does not match its hash");
        }
    }
    fsx::private_dir(dir)?;
    let _lock = fsx::lock_store(dir)?;
    finish_restore(dir)?;
    // A store of this release, or a new one, receives the restore; a dev-build store is refused.
    admit_release(dir)?;
    // A fresh staging folder, named here and never from the manifest.
    fsx::private_dir(&dir.join("tmp"))?;
    let staging = dir.join("tmp").join(format!("restore-{}", fsx::random_hex(8)));
    fs::create_dir(&staging)?;
    for (source, dest, sha) in &plan.copies {
        let target = staging.join(dest);
        if let Some(parent) = target.parent() {
            fsx::private_dir(parent)?;
        }
        fsx::copy_atomic(source, &target)?;
        // The copy is what gets promoted, so it is the copy that must match.
        if fsx::sha256_file(&target)?.0 != *sha {
            fs::remove_dir_all(&staging)?;
            fail!("backup_unverified", "a backup file changed while it was copied");
        }
    }
    for log in ["readings", "ledger"] {
        fsx::private_dir(&staging.join("log").join(log))?;
    }
    if let Some(instance) = &plan.instance {
        fsx::write_atomic(&staging.join("log").join("instance-id"), instance.as_bytes())?;
    }
    let quarantine_dir =
        dir.join("quarantine")
            .join(format!("{}-pre-restore-{}", iso_compact(now_ms()), fsx::random_hex(2)));
    let intent = json!({ "staging": staging.display().to_string(), "quarantine": quarantine_dir.display().to_string(), "backup": plan.id });
    fsx::write_atomic(&dir.join(RESTORE_INTENT), intent.to_string().as_bytes())?;
    finish_restore(dir)?;
    Ok(json!({ "restored": plan.id, "quarantine": quarantine_dir.display().to_string() }))
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
    // The intent names folders this store created: anything else is refused, never moved.
    let inside = |path: &Path, parent: &str, prefix: &str| {
        path.parent() == Some(dir.join(parent).as_path())
            && path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(prefix) && backup_name_ok(n))
    };
    if !inside(&staging, "tmp", "restore-") || !inside(&quarantine_dir, "quarantine", "") {
        fail!("invalid", "the restore intent names folders outside the store");
    }
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
