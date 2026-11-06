//! redb tables (docs/store.md section 3). Keys are fixed-width big-endian byte strings, so byte
//! order is numeric order and every prefix is a range scan. Strings are interned per kind.

use crate::error::Result;
use crate::fail;
use redb::{ReadableTable, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};

pub type Bytes = TableDefinition<'static, &'static [u8], &'static [u8]>;
pub type Index = TableDefinition<'static, &'static [u8], ()>;

// Core.
pub const META: TableDefinition<'static, &'static str, &'static [u8]> = TableDefinition::new("meta");
pub const DICT: Bytes = TableDefinition::new("dict");
pub const DICT_REV: TableDefinition<'static, &'static [u8], u32> = TableDefinition::new("dict_rev");
pub const CHECKPOINT: TableDefinition<'static, &'static str, &'static [u8]> = TableDefinition::new("checkpoint");
pub const IDEM: TableDefinition<'static, &'static [u8], u64> = TableDefinition::new("idem");

// Usage (phase 1).
pub const ACCOUNT: Bytes = TableDefinition::new("account");
pub const WINDOW: Bytes = TableDefinition::new("window");
pub const SERIES: Bytes = TableDefinition::new("series");
pub const SERIES_BY_PROVIDER: Index = TableDefinition::new("series_by_provider");
pub const READING: Bytes = TableDefinition::new("reading");
pub const READING_BY_TIME: Index = TableDefinition::new("reading_by_time");
pub const LATEST: Bytes = TableDefinition::new("latest");
pub const ACCOUNT_LATEST: Bytes = TableDefinition::new("account_latest");
pub const ACCOUNT_STATE: Bytes = TableDefinition::new("account_state");
pub const PERIOD: Bytes = TableDefinition::new("period");
pub const ROLLUP_HOUR: Bytes = TableDefinition::new("rollup_hour");
pub const ROLLUP_DAY: Bytes = TableDefinition::new("rollup_day");
pub const DAY_CLOSED: Index = TableDefinition::new("day_closed");

pub const STORE_SCHEMA_VERSION: u64 = 1;

/// Dictionary kinds.
pub const KIND_ACCOUNT: u8 = 1;
pub const KIND_WINDOW: u8 = 2;
pub const KIND_PROVIDER: u8 = 3;
pub const KIND_SOURCE: u8 = 4;
pub const KIND_EVENT_TYPE: u8 = 5;

/// Creates every table so read transactions never meet a missing one.
pub fn create_tables(txn: &WriteTransaction) -> Result<()> {
    txn.open_table(META)?;
    txn.open_table(DICT)?;
    txn.open_table(DICT_REV)?;
    txn.open_table(CHECKPOINT)?;
    txn.open_table(IDEM)?;
    for def in [
        ACCOUNT,
        WINDOW,
        SERIES,
        READING,
        LATEST,
        ACCOUNT_LATEST,
        ACCOUNT_STATE,
        PERIOD,
        ROLLUP_HOUR,
        ROLLUP_DAY,
    ] {
        txn.open_table(def)?;
    }
    for def in [SERIES_BY_PROVIDER, READING_BY_TIME, DAY_CLOSED] {
        txn.open_table(def)?;
    }
    Ok(())
}

/// Big-endian key builder.
#[derive(Default, Clone)]
pub struct Key(pub Vec<u8>);

impl Key {
    pub fn new() -> Key {
        Key(Vec::with_capacity(24))
    }
    pub fn u8(mut self, v: u8) -> Key {
        self.0.push(v);
        self
    }
    pub fn u32(mut self, v: u32) -> Key {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn u64(mut self, v: u64) -> Key {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    pub fn bytes(mut self, v: &[u8]) -> Key {
        self.0.extend_from_slice(v);
        self
    }
    pub fn as_slice(&self) -> &[u8] {
        &self.0
    }
}

pub fn be_u32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
pub fn be_u64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

/// The smallest key above every key starting with `prefix` (for prefix range scans).
pub fn prefix_end(prefix: &[u8]) -> Vec<u8> {
    let mut end = prefix.to_vec();
    while let Some(last) = end.pop() {
        if last < 0xff {
            end.push(last + 1);
            return end;
        }
    }
    vec![0xff; prefix.len() + 1]
}

pub fn get_meta(txn: &WriteTransaction, key: &str) -> Result<Option<String>> {
    let table = txn.open_table(META)?;
    Ok(table.get(key)?.map(|v| String::from_utf8_lossy(v.value()).into_owned()))
}

pub fn set_meta(txn: &WriteTransaction, key: &str, value: &str) -> Result<()> {
    txn.open_table(META)?.insert(key, value.as_bytes())?;
    Ok(())
}

/// Interns `text` under `kind`, returning its id (ids start at 1 per kind).
pub fn intern(txn: &WriteTransaction, kind: u8, text: &str) -> Result<u32> {
    if text.len() > 512 {
        fail!("invalid", "dictionary text is over 512 bytes");
    }
    let rev_key = Key::new().u8(kind).bytes(text.as_bytes());
    let mut rev = txn.open_table(DICT_REV)?;
    if let Some(id) = rev.get(rev_key.as_slice())? {
        return Ok(id.value());
    }
    let counter = format!("dict_next:{kind}");
    let next: u32 = get_meta(txn, &counter)?.and_then(|v| v.parse().ok()).unwrap_or(1);
    set_meta(txn, &counter, &(next + 1).to_string())?;
    rev.insert(rev_key.as_slice(), next)?;
    txn.open_table(DICT)?
        .insert(Key::new().u8(kind).u32(next).as_slice(), text.as_bytes())?;
    Ok(next)
}

pub fn lookup_id<T: ReadableTable<&'static [u8], u32>>(rev: &T, kind: u8, text: &str) -> Result<Option<u32>> {
    Ok(rev
        .get(Key::new().u8(kind).bytes(text.as_bytes()).as_slice())?
        .map(|v| v.value()))
}

pub fn lookup_text<T: ReadableTable<&'static [u8], &'static [u8]>>(
    dict: &T,
    kind: u8,
    id: u32,
) -> Result<Option<String>> {
    Ok(dict
        .get(Key::new().u8(kind).u32(id).as_slice())?
        .map(|v| String::from_utf8_lossy(v.value()).into_owned()))
}

/// A projector's position in one log (docs/store.md 2.7), committed with its rows.
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct Checkpoint {
    pub log: String,
    pub segment_generation: u64,
    pub segment_name: String,
    pub byte_offset: u64,
    pub last_sequence_id: u64,
    pub last_event_id: String,
    pub survivor_prefix_sha256: String,
    pub projector_schema_version: u32,
}

impl Checkpoint {
    /// Folds one more event into the survivor-prefix digest chain.
    pub fn advance(&mut self, sequence: u64, event_id: &str) {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(self.survivor_prefix_sha256.as_bytes());
        hasher.update(sequence.to_be_bytes());
        hasher.update(event_id.as_bytes());
        let digest = hasher.finalize();
        self.survivor_prefix_sha256 = digest.iter().map(|b| format!("{b:02x}")).collect();
        self.last_sequence_id = sequence;
        self.last_event_id = event_id.to_string();
    }
}

pub fn read_checkpoint<T: ReadableTable<&'static str, &'static [u8]>>(
    table: &T,
    name: &str,
) -> Result<Option<Checkpoint>> {
    match table.get(name)? {
        Some(v) => Ok(Some(crate::msgpack::from_bytes(v.value())?)),
        None => Ok(None),
    }
}

pub fn write_checkpoint(txn: &WriteTransaction, name: &str, checkpoint: &Checkpoint) -> Result<()> {
    let bytes = crate::msgpack::to_bytes(checkpoint)?;
    txn.open_table(CHECKPOINT)?.insert(name, bytes.as_slice())?;
    Ok(())
}

/// A canonical MessagePack value with a leading version byte.
pub fn pack<T: Serialize>(value: &T) -> Result<Vec<u8>> {
    let mut out = vec![1u8];
    out.extend_from_slice(&crate::msgpack::to_bytes(value)?);
    Ok(out)
}

pub fn unpack<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T> {
    if bytes.first() != Some(&1) {
        fail!("unsupported", "value version {:?}", bytes.first());
    }
    crate::msgpack::from_bytes(&bytes[1..])
}
