//! Analytics scanner state and verified copies of the files retired by the Rust cut.
use crate::engine::{self, Engine};
use crate::error::Result;
use redb::{ReadableDatabase, ReadableTable, TableDefinition};
use serde_json::Value;

pub(crate) const TABLE: TableDefinition<&str, &[u8]> = TableDefinition::new("analytics_cache");

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
