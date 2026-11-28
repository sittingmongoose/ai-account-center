//! Analytics scanner state and verified copies of the files retired by the Rust cut.
use crate::engine::{self, Engine};
use crate::error::Result;
use redb::{ReadableDatabase, ReadableTable, TableDefinition};
use serde_json::Value;

pub(crate) const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("analytics_cache");

/// Durable input fingerprint; cache writes themselves never advance these checkpoints.
pub fn answer_generation(engine: &Engine) -> Result<String> {
    engine.with_db(|db| {
        let txn = db.begin_read()?;
        let table = txn.open_table(crate::db::CHECKPOINT)?;
        let mut bytes = Vec::new();
        for row in table.iter()? {
            let (key, value) = row?;
            bytes.extend_from_slice(&(key.value().len() as u64).to_le_bytes());
            bytes.extend_from_slice(key.value().as_bytes());
            let cp: crate::db::Checkpoint = crate::msgpack::from_bytes(value.value())?;
            // Sealing/reopening a segment may move its physical cursor without changing
            // any projected facts. The logical position and survivor chain name the data.
            let logical = crate::msgpack::to_bytes(&(
                cp.last_sequence_id,
                cp.last_event_id,
                cp.survivor_prefix_sha256,
                cp.projector_schema_version,
            ))?;
            bytes.extend_from_slice(&(logical.len() as u64).to_le_bytes());
            bytes.extend_from_slice(&logical);
        }
        bytes.extend_from_slice(b"meta");
        // Includes repair epochs and retention/schema changes, without table statistics or
        // a write transaction. Conservative invalidation is preferable to stale attribution.
        let meta = txn.open_table(crate::db::META)?;
        for row in meta.iter()? {
            let (key, value) = row?;
            if matches!(
                key.value(),
                "clean_shutdown"
                    | "last_verify_ms"
                    | "last_compact_ms"
                    | "last_backup_ms"
                    | "last_share_backup_ms"
                    | "backup_readings_seq"
                    | crate::engine::STATS_META
                    | crate::engine::VERIFIED_GENERATION_META
            ) {
                continue;
            }
            bytes.extend_from_slice(&(key.value().len() as u64).to_le_bytes());
            bytes.extend_from_slice(key.value().as_bytes());
            bytes.extend_from_slice(&(value.value().len() as u64).to_le_bytes());
            bytes.extend_from_slice(value.value());
        }
        Ok(aac_core::sha256_hex(&bytes))
    })
}

pub fn get(engine: &Engine, key: &str) -> Result<Option<Vec<u8>>> {
    engine.with_db(|db| {
        let txn = db.begin_read()?;
        let table = txn.open_table(TABLE)?;
        Ok(table.get(key)?.map(|value| value.value().to_vec()))
    })
}

pub fn put(engine: &Engine, key: &str, bytes: &[u8]) -> Result<()> {
    engine.with_db(|db| {
        let txn = engine::begin(db)?;
        txn.open_table(TABLE)?.insert(key, bytes)?;
        txn.commit()?;
        Ok(())
    })
}

/// Save one complete checkpoint before its corresponding usage is published.
pub fn put_many(engine: &Engine, values: &[(String, Vec<u8>)]) -> Result<()> {
    if values.is_empty() {
        return Ok(());
    }
    engine.with_db(|db| {
        let txn = engine::begin(db)?;
        {
            let mut table = txn.open_table(TABLE)?;
            for (key, bytes) in values {
                table.insert(key.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    })
}

/// Read the present keys' bytes in one read transaction, handing each to `each` in turn (one
/// value held at a time).
pub fn read_each(engine: &Engine, keys: &[String], mut each: impl FnMut(&str, &[u8]) -> Result<()>) -> Result<()> {
    engine.with_db(|db| {
        let txn = db.begin_read()?;
        let table = txn.open_table(TABLE)?;
        for key in keys {
            if let Some(value) = table.get(key.as_str())? {
                each(key, value.value())?;
            }
        }
        Ok(())
    })
}

/// Write `values` and remove `removed` in one transaction: a checkpoint saved in parts changes
/// all at once.
pub fn replace_many(engine: &Engine, values: &[(String, Vec<u8>)], removed: &[String]) -> Result<()> {
    if values.is_empty() && removed.is_empty() {
        return Ok(());
    }
    engine.with_db(|db| {
        let txn = engine::begin(db)?;
        {
            let mut table = txn.open_table(TABLE)?;
            for key in removed {
                table.remove(key.as_str())?;
            }
            for (key, bytes) in values {
                table.insert(key.as_str(), bytes.as_slice())?;
            }
        }
        txn.commit()?;
        Ok(())
    })
}

/// One write transaction over the cache: what [`rewrite`] hands its caller. Reads see the
/// transaction's own writes.
pub struct Rewrite<'t> {
    table: redb::Table<'t, &'static str, &'static [u8]>,
    commit: bool,
}

impl Rewrite<'_> {
    /// The value at `key`, handed to `read` without a copy.
    pub fn read<T>(&self, key: &str, read: impl FnOnce(&[u8]) -> T) -> Result<Option<T>> {
        Ok(self.table.get(key)?.map(|value| read(value.value())))
    }

    pub fn insert(&mut self, key: &str, bytes: &[u8]) -> Result<()> {
        self.table.insert(key, bytes)?;
        Ok(())
    }

    pub fn remove(&mut self, key: &str) -> Result<()> {
        self.table.remove(key)?;
        Ok(())
    }

    /// Commit the transaction when the caller returns (otherwise every write is dropped).
    pub fn commit(&mut self) {
        self.commit = true;
    }
}

/// Run `change` in one write transaction: a checkpoint saved in parts changes all at once while
/// its parts are read, changed and written one at a time. It commits only when `change` asked
/// for it ([`Rewrite::commit`]) and returned `Ok`; the inner result is the commit's. Other
/// writers wait while it runs, so `change` holds no await and no other store call.
pub fn rewrite<T>(engine: &Engine, change: impl FnOnce(&mut Rewrite<'_>) -> Result<T>) -> Result<(T, Result<()>)> {
    engine.with_db(|db| {
        let txn = engine::begin(db)?;
        let (out, commit) = {
            let mut rewrite = Rewrite {
                table: txn.open_table(TABLE)?,
                commit: false,
            };
            let out = change(&mut rewrite)?;
            (out, rewrite.commit)
        };
        if commit {
            Ok((out, txn.commit().map_err(Into::into)))
        } else {
            txn.abort()?;
            Ok((out, Ok(())))
        }
    })
}

/// Drop a consumed scanner checkpoint after its completion receipt commits.
pub fn remove(engine: &Engine, key: &str) -> Result<()> {
    engine.with_db(|db| {
        let txn = engine::begin(db)?;
        txn.open_table(TABLE)?.remove(key)?;
        txn.commit()?;
        Ok(())
    })
}

/// A new value at a retired path keeps its own copy if migration was interrupted and the
/// old application wrote the file again. The path index and its bytes commit together.
pub fn archive(engine: &Engine, files: &[(String, Vec<u8>)]) -> Result<()> {
    engine.with_db(|db| {
        let txn = engine::begin(db)?;
        {
            let mut table = txn.open_table(TABLE)?;
            for (path, bytes) in files {
                let digest = aac_core::sha256_hex(bytes);
                let blob = format!("blob/{path}/{digest}");
                if table.get(blob.as_str())?.is_none() {
                    table.insert(blob.as_str(), bytes.as_slice())?;
                }
                table.insert(path.as_str(), digest.as_bytes())?;
            }
        }
        txn.commit()?;
        Ok(())
    })
}

pub fn archived(engine: &Engine, path: &str) -> Result<Option<Vec<u8>>> {
    engine.with_db(|db| {
        let txn = db.begin_read()?;
        let table = txn.open_table(TABLE)?;
        let Some(digest) = table.get(path)? else {
            return Ok(None);
        };
        let digest = std::str::from_utf8(digest.value())?;
        let key = format!("blob/{path}/{digest}");
        let bytes = table
            .get(key.as_str())?
            .ok_or_else(|| crate::error::Error::new("io", "missing Analytics archive bytes"))?;
        if aac_core::sha256_hex(bytes.value()) != digest {
            crate::fail!("io", "Analytics archive hash mismatch");
        }
        Ok(Some(bytes.value().to_vec()))
    })
}

/// The indexed legacy paths already staged by migration.
pub fn archived_index(engine: &Engine, prefix: &str) -> Result<Vec<(String, String)>> {
    engine.with_db(|db| {
        let txn = db.begin_read()?;
        let table = txn.open_table(TABLE)?;
        let mut paths = Vec::new();
        for entry in table.range(prefix..)? {
            let (path, digest) = entry?;
            if !path.value().starts_with(prefix) {
                break;
            }
            paths.push((path.value().to_owned(), std::str::from_utf8(digest.value())?.to_owned()));
        }
        Ok(paths)
    })
}

/// Apply the capture opt-out to every retained version of each imported computer cache.
pub fn clear_remote_folders(engine: &Engine) -> Result<u64> {
    const PREFIX: &str = "blob/legacy/cache/analytics-remote-v1/";
    engine.with_db(|db| {
        let txn = engine::begin(db)?;
        let mut cleared = 0;
        {
            let mut table = txn.open_table(TABLE)?;
            let mut scrubbed = Vec::new();
            for item in table.range(PREFIX.."blob/legacy/cache/analytics-remote-v10")? {
                let (key, bytes) = item?;
                let Ok(mut value) = serde_json::from_slice::<Value>(bytes.value()) else {
                    continue;
                };
                let Some(sessions) = value["sessions"].as_object_mut() else {
                    continue;
                };
                let mut changed = 0;
                for session in sessions.values_mut() {
                    if session.get("cwd").is_some_and(|cwd| !cwd.is_null()) {
                        session["cwd"] = Value::Null;
                        changed += 1;
                    }
                }
                if changed > 0 {
                    cleared += changed;
                    scrubbed.push((key.value().to_owned(), serde_json::to_vec(&value)?));
                }
            }
            for (key, bytes) in scrubbed {
                let Some((path, old_digest)) = key.strip_prefix("blob/").and_then(|key| key.rsplit_once('/')) else {
                    continue;
                };
                let digest = aac_core::sha256_hex(&bytes);
                let latest = table
                    .get(path)?
                    .is_some_and(|value| value.value() == old_digest.as_bytes());
                table.insert(format!("blob/{path}/{digest}").as_str(), bytes.as_slice())?;
                if latest {
                    table.insert(path, digest.as_bytes())?;
                }
                table.remove(key.as_str())?;
            }
        }
        txn.commit()?;
        Ok(cleared)
    })
}
