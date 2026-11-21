//! The Cursor billing reader's checkpoint, kept in the store rather than a side file.
//!
//! The packaged reader (`aac-cursor-account read`) keeps no file. Its checkpoint and hourly
//! cells live in the store's `analytics_cache` family under [`KEY`], one document holding each
//! signed-in account's checkpoint by its one-way hash: `{"version":1,"accounts":{..}}`. The
//! Cursor [`Reader`](super::cursor::Reader) is the one writer: it holds its read lock from the
//! load, through the reader child, to the save, so two passes cannot regress the watermark.
//!
//! A main-branch install may have left the reader's old `cache/cursor-billing-v1.sqlite`. It is
//! archived (verified, content-addressed) and imported once; the file is removed only after the
//! store holds its data or a refill from Cursor. A failed import keeps the file.
use aac_files::read::{Ancestors, Read, ReadPolicy};
use aac_store::analytics_cache;
use aac_store::engine::Engine;
use aac_store::error::{Error, Result};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::sync::Arc;

/// The store key of the billing document (beside `live/cursor-account-v1.json`).
pub(crate) const KEY: &str = "live/cursor-billing-v1.json";
/// The retired side file of the main-branch reader, under `<ccs>/cache`.
pub(crate) const LEGACY_FILE: &str = "cursor-billing-v1.sqlite";
const LEGACY_KEY: &str = "legacy/cache/cursor-billing-v1.sqlite";
/// A rollback journal the old reader may have left beside its file when it was stopped.
const LEGACY_JOURNAL: &str = "cursor-billing-v1.sqlite-journal";
const LEGACY_JOURNAL_KEY: &str = "legacy/cache/cursor-billing-v1.sqlite-journal";
/// The reader's own cap on one saved checkpoint, and so on the document handed to it.
const MAX_DOCUMENT: usize = 16 * 1024 * 1024;
const MAX_LEGACY: u64 = 64 * 1024 * 1024;

/// Where this pass's billing checkpoint comes from and goes to.
pub(crate) enum BillingStore {
    /// The store owner's engine and the document it holds now.
    Saved(Arc<Engine>, Value),
    /// No store in this process (a caller without one): the pass starts fresh, nothing persists.
    None,
    /// The store exists but cannot be used now: the fixed cause the answer carries.
    Unavailable(&'static str),
}

impl BillingStore {
    /// The document handed to the reader child, if there is a store.
    pub(crate) fn document(&self) -> Option<&Value> {
        match self {
            Self::Saved(_, document) => Some(document),
            _ => None,
        }
    }
}

fn empty() -> Value {
    json!({"version": 1, "accounts": {}})
}

/// A store error as the reader's fixed causes: a closed store (a restore or a restart in
/// progress) is a read in progress; anything else is an unsafe store.
pub(crate) fn cause(error: &Error) -> &'static str {
    if error.code == "closed" {
        "read_in_progress"
    } else {
        "store_unsafe"
    }
}

fn account_ok(account: &str) -> bool {
    account.len() == 16
        && account
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// A checkpoint the reader wrote: an object with its coverage bounds and hour cells. The reader
/// checks the rest itself and starts fresh from anything it cannot read.
fn checkpoint_ok(checkpoint: &Value) -> bool {
    checkpoint["from"].is_i64() && checkpoint["through"].is_i64() && checkpoint["hours"].is_object()
}

/// The saved document; an unreadable one is replaced by the next save (the reader refills).
pub(crate) fn load(engine: &Engine) -> Result<Value> {
    let Some(bytes) = analytics_cache::get(engine, KEY)? else {
        return Ok(empty());
    };
    let document = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(|document| document["version"] == 1 && document["accounts"].is_object());
    if document.is_none() {
        tracing::warn!("Cursor billing state unreadable; the next pass refills it");
    }
    Ok(document.unwrap_or_else(empty))
}

fn accounts(document: &mut Value) -> &mut Map<String, Value> {
    if !document["accounts"].is_object() {
        *document = empty();
    }
    match document["accounts"].as_object_mut() {
        Some(accounts) => accounts,
        None => unreachable!("the document was just reset"),
    }
}

/// Write the document, dropping the least recently read other accounts if it outgrew the cap.
fn put(engine: &Engine, mut document: Value, keep: &str) -> Result<()> {
    loop {
        let bytes = serde_json::to_vec(&document)?;
        if bytes.len() <= MAX_DOCUMENT {
            return analytics_cache::put(engine, KEY, &bytes);
        }
        let accounts = accounts(&mut document);
        let oldest = accounts
            .iter()
            .filter(|(account, _)| account.as_str() != keep)
            .min_by_key(|(_, checkpoint)| checkpoint["read_at"].as_i64().unwrap_or(i64::MIN))
            .map(|(account, _)| account.clone());
        match oldest {
            Some(account) => {
                accounts.remove(&account);
            }
            None => return Err(Error::new("source_limit", "Cursor billing state too large")),
        }
    }
}

/// Save one pass's checkpoint (`{"account":..,"checkpoint":..}` from the reader child).
pub(crate) fn save(engine: &Engine, state: &Value) -> Result<()> {
    let account = state["account"].as_str().filter(|a| account_ok(a));
    let checkpoint = &state["checkpoint"];
    let (Some(account), true) = (account, checkpoint_ok(checkpoint)) else {
        return Err(Error::new(
            "invalid",
            "Cursor billing state has no account checkpoint",
        ));
    };
    let mut document = load(engine)?;
    accounts(&mut document).insert(account.to_owned(), checkpoint.clone());
    put(engine, document, account)?;
    // Verify the write before the reader's answer is served as saved.
    match load(engine)?["accounts"].get(account) {
        Some(saved) if saved == checkpoint => Ok(()),
        _ => Err(Error::new("io", "Cursor billing state verification failed")),
    }
}

/// The checkpoints a legacy SQLite file holds, read from its bytes in memory (never the file).
fn legacy_checkpoints(bytes: &[u8]) -> rusqlite::Result<Vec<(String, Value)>> {
    let mut db = rusqlite::Connection::open_in_memory()?;
    db.deserialize_read_exact(rusqlite::MAIN_DB, bytes, bytes.len(), true)?;
    let mut statement = db.prepare(
        "SELECT account, state FROM observations WHERE length(CAST(state AS BLOB)) <= ?1",
    )?;
    let rows = statement.query_map([MAX_DOCUMENT as i64], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
    })?;
    let mut found = Vec::new();
    for row in rows {
        let (account, state) = row?;
        if let Some(checkpoint) = serde_json::from_str::<Value>(&state)
            .ok()
            .filter(|checkpoint| account_ok(&account) && checkpoint_ok(checkpoint))
        {
            found.push((account, checkpoint));
        }
    }
    Ok(found)
}

fn read_legacy(root: &Path, file: &Path) -> Result<Option<Vec<u8>>> {
    let policy = ReadPolicy::capped(MAX_LEGACY).ancestors(Ancestors::NoLinkBelow(root));
    match aac_files::read::read(file, &policy) {
        Read::Bytes(bytes, _) => Ok(Some(bytes)),
        Read::Absent => Ok(None),
        Read::TooLarge => Err(Error::new(
            "source_limit",
            "Cursor billing file exceeds 64 MiB",
        )),
        other => Err(Error::new(
            "io",
            format!("Cursor billing file read: {other:?}"),
        )),
    }
}

/// Archive a retired file's bytes and check the store returns them unchanged.
fn archive(engine: &Engine, key: &str, bytes: &[u8]) -> Result<()> {
    analytics_cache::archive(engine, &[(key.to_owned(), bytes.to_vec())])?;
    if analytics_cache::archived(engine, key)?.as_deref() != Some(bytes) {
        return Err(Error::new(
            "io",
            "Cursor billing archive verification failed",
        ));
    }
    Ok(())
}

/// Remove a retired file only while it still holds the bytes the store verified.
fn remove(root: &Path, file: &Path, bytes: &[u8]) -> Result<()> {
    match read_legacy(root, file)? {
        Some(current) if current == bytes => Ok(std::fs::remove_file(file)?),
        Some(_) => Err(Error::new(
            "io",
            "Cursor billing file changed during import",
        )),
        None => Ok(()),
    }
}

/// What [`retire_legacy`] did.
#[derive(Debug, PartialEq)]
pub(crate) enum Legacy {
    /// No legacy file.
    Absent,
    /// The file's checkpoints are in the store (this many new accounts) and the file is gone.
    Imported(usize),
    /// The file could not be read as checkpoints, but the store has refilled; the file is gone.
    Refilled,
}

/// Import the main-branch reader's SQLite file once, then remove it (and a journal it left).
/// Checkpoints already in the store are newer and win. Any failure keeps the file.
pub(crate) fn retire_legacy(engine: &Engine, root: &Path) -> Result<Legacy> {
    let cache = root.join("cache");
    let file = cache.join(LEGACY_FILE);
    let Some(bytes) = read_legacy(root, &file)? else {
        return Ok(Legacy::Absent);
    };
    archive(engine, LEGACY_KEY, &bytes)?;
    let outcome = match legacy_checkpoints(&bytes) {
        Ok(found) => {
            let mut document = load(engine)?;
            let saved = accounts(&mut document);
            let mut added = Vec::new();
            for (account, checkpoint) in found {
                if !saved.contains_key(&account) {
                    saved.insert(account.clone(), checkpoint);
                    added.push(account);
                }
            }
            if !added.is_empty() {
                let keep = added[0].clone();
                put(engine, document, &keep)?;
                let stored = load(engine)?;
                if added
                    .iter()
                    .any(|account| stored["accounts"].get(account).is_none())
                {
                    return Err(Error::new(
                        "io",
                        "Cursor billing import verification failed",
                    ));
                }
            }
            Legacy::Imported(added.len())
        }
        // Unreadable checkpoints: the verified archive keeps the bytes, and the file goes once
        // the store has refilled from Cursor.
        Err(_)
            if !load(engine)?["accounts"]
                .as_object()
                .is_none_or(Map::is_empty) =>
        {
            Legacy::Refilled
        }
        Err(_) => {
            return Err(Error::new(
                "invalid",
                "Cursor billing file kept: not importable and not refilled yet",
            ));
        }
    };
    remove(root, &file, &bytes)?;
    let journal = cache.join(LEGACY_JOURNAL);
    if let Some(journal_bytes) = read_legacy(root, &journal)? {
        archive(engine, LEGACY_JOURNAL_KEY, &journal_bytes)?;
        remove(root, &journal, &journal_bytes)?;
    }
    Ok(outcome)
}

/// The billing store for one pass, from the store owner attached for this root.
pub(crate) fn open(root: &Path) -> BillingStore {
    let Some(host) = crate::analytics_storage::host(root) else {
        return BillingStore::None;
    };
    let engine = match host.analytics_engine() {
        Some(engine) => engine,
        None => return BillingStore::Unavailable(host.engine_unavailable_cause()),
    };
    open_with(&engine, root)
}

pub(crate) fn open_with(engine: &Arc<Engine>, root: &Path) -> BillingStore {
    match retire_legacy(engine, root) {
        Ok(Legacy::Absent) => {}
        Ok(outcome) => tracing::info!(?outcome, "Cursor billing file moved into the store"),
        Err(error) => {
            tracing::warn!(code = error.code, "Cursor billing file kept");
        }
    }
    match load(engine) {
        Ok(document) => BillingStore::Saved(Arc::clone(engine), document),
        Err(error) => BillingStore::Unavailable(cause(&error)),
    }
}

/// Take the reader child's `billingState` out of its answer and save it. A failed save turns the
/// answer into the store's fixed cause, so the last saved rows stay what the page shows.
pub(crate) fn settle(store: &BillingStore, answer: &mut Value) {
    let state = answer
        .as_object_mut()
        .and_then(|answer| answer.remove("billingState"));
    let (Some(state), BillingStore::Saved(engine, _)) = (state, store) else {
        return;
    };
    if let Err(error) = save(engine, &state) {
        tracing::warn!(code = error.code, "Cursor billing state not saved");
        let cause = if error.code == "invalid" {
            "bad_response"
        } else {
            cause(&error)
        };
        *answer = json!({"state": "unavailable", "cause": cause});
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const ACCOUNT: &str = "0123456789abcdef";
    const OTHER: &str = "fedcba9876543210";

    fn open_store(dir: &Path) -> Arc<Engine> {
        Arc::new(
            aac_store::ops::open_engine(&dir.join("store").to_string_lossy(), None, None).unwrap(),
        )
    }

    fn checkpoint(through: i64) -> Value {
        json!({"from": through - 86_400_000, "through": through, "read_at": through,
            "newest": null, "forward_span": 86_400_000, "history_span": 86_400_000,
            "hours": {"1794036000000": {"rows": [{"h":"2026-11-07 08:00","m":"claude-sonnet-4-5",
            "i":17,"o":5,"cr":90,"cw":11,"n":1,"cents":0.0,"uncosted":1}], "unmeasured": 0,
            "unrecognized_models": 0}}})
    }

    fn legacy_file(root: &Path, rows: &[(&str, Value)]) -> Vec<u8> {
        let file = root.join("cache").join(LEGACY_FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        let db = rusqlite::Connection::open(&file).unwrap();
        db.execute_batch(
            "CREATE TABLE observations (account TEXT PRIMARY KEY, state TEXT NOT NULL);",
        )
        .unwrap();
        for (account, state) in rows {
            db.execute(
                "INSERT INTO observations VALUES (?1, ?2)",
                (*account, state.to_string()),
            )
            .unwrap();
        }
        drop(db);
        std::fs::read(&file).unwrap()
    }

    #[test]
    fn billing_state_survives_a_restart_from_the_store() {
        let home = tempfile::tempdir().unwrap();
        let engine = open_store(home.path());
        let BillingStore::Saved(_, document) = open_with(&engine, home.path()) else {
            panic!("store");
        };
        assert_eq!(document, empty());
        let mut answer = json!({"state": "read", "rows": [],
            "billingState": {"account": ACCOUNT, "checkpoint": checkpoint(1_794_067_200_000)}});
        settle(
            &BillingStore::Saved(Arc::clone(&engine), document),
            &mut answer,
        );
        assert_eq!(answer, json!({"state": "read", "rows": []}));
        engine.close().unwrap();
        drop(engine);

        // The restarted service hands the reader the same checkpoint, and no side file exists.
        let engine = open_store(home.path());
        let store = open_with(&engine, home.path());
        assert_eq!(
            store.document().unwrap()["accounts"][ACCOUNT],
            checkpoint(1_794_067_200_000)
        );
        assert!(!home.path().join("cache").join(LEGACY_FILE).exists());
        assert!(!home.path().join("cache").exists());
        engine.close().unwrap();
    }

    #[test]
    fn a_failed_save_keeps_the_last_rows_with_a_fixed_cause() {
        let home = tempfile::tempdir().unwrap();
        let engine = open_store(home.path());
        let store = open_with(&engine, home.path());
        let mut bad =
            json!({"state": "read", "billingState": {"account": "../x", "checkpoint": {}}});
        settle(&store, &mut bad);
        assert_eq!(
            bad,
            json!({"state": "unavailable", "cause": "bad_response"})
        );
        engine.close().unwrap();
        let mut closed = json!({"state": "read",
            "billingState": {"account": ACCOUNT, "checkpoint": checkpoint(1_794_067_200_000)}});
        settle(&store, &mut closed);
        assert_eq!(
            closed,
            json!({"state": "unavailable", "cause": "read_in_progress"})
        );
        // Without a store the state is dropped and the answer stands.
        let mut plain = json!({"state": "read",
            "billingState": {"account": ACCOUNT, "checkpoint": checkpoint(1)}});
        settle(&BillingStore::None, &mut plain);
        assert_eq!(plain, json!({"state": "read"}));
    }

    #[test]
    fn a_legacy_billing_file_is_imported_once_then_retired() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let engine = open_store(root);
        // The store's own, newer checkpoint wins over the file's copy of the same account.
        save(
            &engine,
            &json!({"account": ACCOUNT, "checkpoint": checkpoint(1_794_067_200_000)}),
        )
        .unwrap();
        let bytes = legacy_file(
            root,
            &[
                (ACCOUNT, checkpoint(1_794_000_000_000)),
                (OTHER, checkpoint(1_794_060_000_000)),
            ],
        );
        std::fs::write(
            root.join("cache").join(LEGACY_JOURNAL),
            b"left by a stopped reader",
        )
        .unwrap();
        assert_eq!(retire_legacy(&engine, root).unwrap(), Legacy::Imported(1));
        let document = load(&engine).unwrap();
        assert_eq!(document["accounts"][ACCOUNT], checkpoint(1_794_067_200_000));
        assert_eq!(document["accounts"][OTHER], checkpoint(1_794_060_000_000));
        assert!(!root.join("cache").join(LEGACY_FILE).exists());
        assert!(!root.join("cache").join(LEGACY_JOURNAL).exists());
        assert_eq!(
            analytics_cache::archived(&engine, LEGACY_KEY).unwrap(),
            Some(bytes)
        );
        assert_eq!(retire_legacy(&engine, root).unwrap(), Legacy::Absent);
        engine.close().unwrap();
    }

    #[test]
    fn a_legacy_file_that_cannot_be_imported_is_kept_until_the_store_refills() {
        let home = tempfile::tempdir().unwrap();
        let root = home.path();
        let engine = open_store(root);
        let file = root.join("cache").join(LEGACY_FILE);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, b"not a database").unwrap();
        assert!(retire_legacy(&engine, root).is_err());
        assert!(file.is_file());
        // The pass still reads: the store opens empty and the reader refills from Cursor.
        let store = open_with(&engine, root);
        assert_eq!(store.document(), Some(&empty()));
        assert!(file.is_file());
        save(
            &engine,
            &json!({"account": ACCOUNT, "checkpoint": checkpoint(1_794_067_200_000)}),
        )
        .unwrap();
        assert_eq!(retire_legacy(&engine, root).unwrap(), Legacy::Refilled);
        assert!(!file.exists());
        assert_eq!(
            analytics_cache::archived(&engine, LEGACY_KEY)
                .unwrap()
                .as_deref(),
            Some(&b"not a database"[..])
        );
        engine.close().unwrap();
    }

    #[test]
    fn a_full_document_drops_the_least_recently_read_other_account() {
        let home = tempfile::tempdir().unwrap();
        let engine = open_store(home.path());
        let mut big = checkpoint(1_000);
        big["hours"]["0"] = json!({"rows": [{"pad": "x".repeat(MAX_DOCUMENT / 2)}]});
        save(&engine, &json!({"account": OTHER, "checkpoint": big})).unwrap();
        let mut current = checkpoint(2_000);
        current["hours"]["0"] = json!({"rows": [{"pad": "y".repeat(MAX_DOCUMENT / 2 + 1024)}]});
        save(&engine, &json!({"account": ACCOUNT, "checkpoint": current})).unwrap();
        let document = load(&engine).unwrap();
        assert!(document["accounts"].get(OTHER).is_none());
        assert!(document["accounts"].get(ACCOUNT).is_some());
        engine.close().unwrap();
    }
}
