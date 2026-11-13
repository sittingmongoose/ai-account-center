//! Analytics usage history (docs/store.md section 12): the `activity` projector, its ingest
//! decisions, day close, retention and range queries. Readers hand the store contract reports
//! (contract.rs); the store keeps every session-hour cell, session row and scan state.
//!
//! Two logs, as the quota readings: raw changes (the current hour moves on every pass) go to the
//! readings log; the final cells of a UTC day go to the ledger once the day closes (74 hours after
//! it ends), and a later change to a closed day goes straight to the ledger. Replay is exact because
//! a day is touched only by readings events before its close, by its close, and by ledger events
//! after it. Session rows merge commutatively, so their order across the logs does not matter.

use crate::contract::{self, HOUR_MS, Report};
use crate::db::{self, Key};
use crate::engine::{self, DAY_MS, Engine, LogId, Projector, begin, now_ms};
use crate::error::Result;
use crate::event::Event;
use crate::fail;
use crate::log::Finding;
use crate::msgpack;
use crate::pack::{Cursor, put_varint};
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, TableDefinition, WriteTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet, HashMap};

/// Bump when projection rules change: the activity tables are then rebuilt from the logs.
pub const ACTIVITY_PROJECTOR_VERSION: u32 = 1;
/// A source's hours more than this far behind its last complete read are settled: live reads
/// no longer change them (a tool that deletes old logs can never erase history).
pub const SETTLE_HOURS: u32 = 72;
/// A UTC day closes this long after it ends (the settle line plus two hours).
pub const CLOSE_AFTER_MS: u64 = 74 * HOUR_MS;
const CELLS_PER_EVENT: usize = 2000;
const SESSIONS_PER_EVENT: usize = 1000;
const READ_LOG_EVERY_MS: u64 = HOUR_MS;
const PRUNE_BATCH: usize = 10_000;
const DEFAULT_SAMPLE: usize = 200;
const MAX_SAMPLE: usize = 2000;
const MAX_ROWS: usize = 400_000;

type Bytes = TableDefinition<'static, &'static [u8], &'static [u8]>;
type Index = TableDefinition<'static, &'static [u8], ()>;

pub const ACT_DICT: Bytes = TableDefinition::new("act_dict");
pub const ACT_DICT_REV: TableDefinition<'static, &'static [u8], u32> = TableDefinition::new("act_dict_rev");
/// `day u32 | computer u32 | tool u32` to the packed cells of that day (3.2).
pub const ACT_HOUR: Bytes = TableDefinition::new("act_hour");
/// `tool u32 | session key [u8; 8]` to a MessagePack `SessionRow`.
pub const ACT_SESSION: Bytes = TableDefinition::new("act_session");
/// `last hour u32 | tool u32 | session key [u8; 8]` to `first ms u64 | last ms u64 | signature u32`:
/// a covering index, so counting the sessions of a range reads no session row.
pub const ACT_SESSION_BY_LAST: Bytes = TableDefinition::new("act_session_by_last");
/// `computer u32 | tool u32 | start hour u32` to `end hour u32` (exclusive).
pub const ACT_COVER: Bytes = TableDefinition::new("act_cover");
/// `computer u32 | tool u32` to a MessagePack `SourceRow`.
pub const ACT_SOURCE: Bytes = TableDefinition::new("act_source");
/// `day u32`: days whose cells are final in the ledger.
pub const ACT_DAY_CLOSED: Index = TableDefinition::new("act_day_closed");
/// `computer u32 | tool u32 | at ms u64` to a MessagePack `ReadRow`: scan-state history.
pub const ACT_SCAN: Bytes = TableDefinition::new("act_scan");
/// Volatile: `computer text | 0 | tool text` to a MessagePack `ReadRow`, written on every ingest
/// without a log event (minute-precision "last read"). Not rebuilt from the logs.
pub const ACT_LIVE: Bytes = TableDefinition::new("act_live");
/// `computer u32 | tool u32` to a MessagePack `BackfillRow`.
pub const ACT_BACKFILL: Bytes = TableDefinition::new("act_backfill");

const K_COMPUTER: u8 = 1;
const K_TOOL: u8 = 2;
const K_ROUTE: u8 = 3;
const K_MODEL: u8 = 4;
const K_ACCOUNT: u8 = 5;
const K_EFFORT: u8 = 6;
/// A session's tool and its distinct (route, model) pairs, as ids: `t|r:m;r:m`.
const K_SIGNATURE: u8 = 7;
const KINDS: [u8; 7] = [K_COMPUTER, K_TOOL, K_ROUTE, K_MODEL, K_ACCOUNT, K_EFFORT, K_SIGNATURE];

/// States that mean the computer or its logs could not be read.
const BAD_STATES: [&str; 2] = ["unreadable", "scan_failed"];

pub fn create_tables(txn: &WriteTransaction) -> Result<()> {
    for def in [
        ACT_DICT,
        ACT_HOUR,
        ACT_SESSION,
        ACT_COVER,
        ACT_SOURCE,
        ACT_SCAN,
        ACT_LIVE,
        ACT_BACKFILL,
    ] {
        txn.open_table(def)?;
    }
    txn.open_table(ACT_DICT_REV)?;
    txn.open_table(ACT_SESSION_BY_LAST)?;
    txn.open_table(ACT_DAY_CLOSED)?;
    Ok(())
}

// ----------------------------------------------------------------------------- dictionary

fn intern(txn: &WriteTransaction, kind: u8, text: &str) -> Result<u32> {
    if text.len() > 512 {
        fail!("invalid", "dictionary text is over 512 bytes");
    }
    let rev_key = Key::new().u8(kind).bytes(text.as_bytes());
    let mut rev = txn.open_table(ACT_DICT_REV)?;
    if let Some(id) = rev.get(rev_key.as_slice())? {
        return Ok(id.value());
    }
    let counter = format!("act_dict_next:{kind}");
    let next: u32 = db::get_meta(txn, &counter)?.and_then(|v| v.parse().ok()).unwrap_or(1);
    db::set_meta(txn, &counter, &(next + 1).to_string())?;
    rev.insert(rev_key.as_slice(), next)?;
    txn.open_table(ACT_DICT)?
        .insert(Key::new().u8(kind).u32(next).as_slice(), text.as_bytes())?;
    Ok(next)
}

fn intern_opt(txn: &WriteTransaction, kind: u8, text: Option<&str>) -> Result<u32> {
    match text {
        Some(t) => intern(txn, kind, t),
        None => Ok(0),
    }
}

/// Id to text, cached per call.
struct Names<'t, T: ReadableTable<&'static [u8], &'static [u8]>> {
    dict: &'t T,
    cache: HashMap<(u8, u32), String>,
}

impl<'t, T: ReadableTable<&'static [u8], &'static [u8]>> Names<'t, T> {
    fn new(dict: &'t T) -> Self {
        Names {
            dict,
            cache: HashMap::new(),
        }
    }
    fn get(&mut self, kind: u8, id: u32) -> Result<String> {
        if let Some(t) = self.cache.get(&(kind, id)) {
            return Ok(t.clone());
        }
        let text = match self.dict.get(Key::new().u8(kind).u32(id).as_slice())? {
            Some(v) => String::from_utf8_lossy(v.value()).into_owned(),
            None => fail!("corrupt", "activity dictionary id {kind}:{id} is missing"),
        };
        self.cache.insert((kind, id), text.clone());
        Ok(text)
    }
    fn opt(&mut self, kind: u8, id: u32) -> Result<Option<String>> {
        if id == 0 {
            Ok(None)
        } else {
            self.get(kind, id).map(Some)
        }
    }
}

// ----------------------------------------------------------------------------- cells

/// One cell of a day value. Identity: `(hour, session, route, model, effort, account)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cell {
    pub hour: u8,
    pub session: [u8; 8],
    pub route: u32,
    pub model: u32,
    pub effort: u32,
    pub account: u32,
    pub i: u64,
    pub o: u64,
    pub cw: u64,
    pub cr: u64,
    pub reasoning: Option<u64>,
    /// Nano-dollars.
    pub logged: Option<u64>,
    pub cost: Option<u64>,
    pub fallback: Option<u64>,
    pub events: Option<u64>,
    /// Quality bits (`contract::QUALITY_FLAGS`).
    pub quality: u32,
}

type CellId = (u8, [u8; 8], u32, u32, u32, u32);

impl Cell {
    fn id(&self) -> CellId {
        (
            self.hour,
            self.session,
            self.route,
            self.model,
            self.effort,
            self.account,
        )
    }
    fn tokens(&self) -> u64 {
        self.i + self.o + self.cw + self.cr
    }
}

const NO_SESSION: [u8; 8] = [0; 8];

fn session_bytes(hex: Option<&str>) -> [u8; 8] {
    let mut out = NO_SESSION;
    if let Some(h) = hex {
        for (i, chunk) in h.as_bytes().chunks(2).take(8).enumerate() {
            let s = std::str::from_utf8(chunk).unwrap_or("00");
            out[i] = u8::from_str_radix(s, 16).unwrap_or(0);
        }
    }
    out
}

fn session_hex(b: &[u8; 8]) -> Option<String> {
    if *b == NO_SESSION {
        None
    } else {
        Some(b.iter().map(|x| format!("{x:02x}")).collect())
    }
}

fn put_opt(out: &mut Vec<u8>, v: Option<u64>) {
    put_varint(out, v.map(|x| x + 1).unwrap_or(0));
}

fn read_opt(c: &mut Cursor<'_>) -> Result<Option<u64>> {
    let v = c.varint()?;
    Ok(if v == 0 { None } else { Some(v - 1) })
}

/// 2 adds each cell's quality bits after its events; 1 is read as quality 0.
const CELLS_VERSION: u8 = 2;

/// Packed day value (3.2): version, count, then each cell in identity order.
pub fn encode_cells(cells: &[Cell]) -> Vec<u8> {
    let mut sorted: Vec<&Cell> = cells.iter().collect();
    sorted.sort_by_key(|c| c.id());
    let mut out = Vec::with_capacity(4 + cells.len() * 24);
    out.push(CELLS_VERSION);
    put_varint(&mut out, sorted.len() as u64);
    for c in sorted {
        out.push(c.hour);
        out.extend_from_slice(&c.session);
        for v in [c.route, c.model, c.effort, c.account] {
            put_varint(&mut out, v as u64);
        }
        for v in [c.i, c.o, c.cw, c.cr] {
            put_varint(&mut out, v);
        }
        for v in [c.reasoning, c.logged, c.cost, c.fallback, c.events] {
            put_opt(&mut out, v);
        }
        put_varint(&mut out, c.quality as u64);
    }
    out
}

pub fn decode_cells(bytes: &[u8]) -> Result<Vec<Cell>> {
    let mut c = Cursor::new(bytes);
    let version = c.u8()?;
    if version != 1 && version != CELLS_VERSION {
        fail!("unsupported", "activity day value version");
    }
    let n = c.varint()? as usize;
    if n > 1_000_000 {
        fail!("corrupt", "activity day value count");
    }
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let hour = c.u8()?;
        if hour > 23 {
            fail!("corrupt", "activity hour of day");
        }
        let session: [u8; 8] = c.take(8)?.try_into().unwrap();
        let mut ids = [0u32; 4];
        for id in ids.iter_mut() {
            let v = c.varint()?;
            if v > u32::MAX as u64 {
                fail!("corrupt", "activity dictionary id");
            }
            *id = v as u32;
        }
        let (i, o, cw, cr) = (c.varint()?, c.varint()?, c.varint()?, c.varint()?);
        out.push(Cell {
            hour,
            session,
            route: ids[0],
            model: ids[1],
            effort: ids[2],
            account: ids[3],
            i,
            o,
            cw,
            cr,
            reasoning: read_opt(&mut c)?,
            logged: read_opt(&mut c)?,
            cost: read_opt(&mut c)?,
            fallback: read_opt(&mut c)?,
            events: read_opt(&mut c)?,
            quality: if version >= 2 {
                let q = c.varint()?;
                if q > u32::MAX as u64 {
                    fail!("corrupt", "activity cell quality");
                }
                q as u32
            } else {
                0
            },
        });
    }
    c.finish()?;
    Ok(out)
}

fn day_key(day: u32, c: u32, t: u32) -> Key {
    Key::new().u32(day).u32(c).u32(t)
}

fn load_day(txn: &WriteTransaction, day: u32, c: u32, t: u32) -> Result<Vec<Cell>> {
    let table = txn.open_table(ACT_HOUR)?;
    let value = table.get(day_key(day, c, t).as_slice())?.map(|v| v.value().to_vec());
    match value {
        Some(b) => decode_cells(&b),
        None => Ok(Vec::new()),
    }
}

fn store_day(txn: &WriteTransaction, day: u32, c: u32, t: u32, cells: &[Cell]) -> Result<()> {
    let mut table = txn.open_table(ACT_HOUR)?;
    let key = day_key(day, c, t);
    if cells.is_empty() {
        table.remove(key.as_slice())?;
    } else {
        table.insert(key.as_slice(), encode_cells(cells).as_slice())?;
    }
    Ok(())
}

// ----------------------------------------------------------------------------- event payloads

fn is_zero(v: &u64) -> bool {
    *v == 0
}

fn is_zero32(v: &u32) -> bool {
    *v == 0
}

/// A cell as it travels in the logs: text, never dictionary ids; the hour as an absolute index.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct CellEv {
    pub h: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    #[serde(default)]
    pub r: String,
    pub m: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<String>,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub i: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub o: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cw: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cr: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ln: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fb: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ev: Option<u64>,
    /// Quality bits (`contract::QUALITY_FLAGS`).
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub q: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct DelEv {
    pub h: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<String>,
    #[serde(default)]
    pub r: String,
    pub m: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub e: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub a: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct PartEv {
    #[serde(default)]
    pub r: String,
    pub m: String,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub i: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub o: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cw: u64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cr: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ln: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fb: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct SessEv {
    /// Session key, 16 hex.
    pub k: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hs: Option<String>,
    /// The computer that observed it.
    pub c: String,
    pub f: u64,
    pub l: u64,
    #[serde(default)]
    pub p: Vec<PartEv>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwp: Option<String>,
    /// Quality bits of the parts (`contract::QUALITY_FLAGS`).
    #[serde(default, skip_serializing_if = "is_zero32")]
    pub q: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct CellsEv {
    c: String,
    t: String,
    #[serde(default)]
    put: Vec<CellEv>,
    #[serde(default)]
    del: Vec<DelEv>,
    #[serde(default)]
    s: Vec<SessEv>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct CountsRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub roots: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failed: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest_ms: Option<u64>,
    /// Records dated after the read was made, left out (`contract::FUTURE_SLACK_MS`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub future_dated: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct RootRow {
    pub root: String,
    pub origin: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<CountsRow>,
}

/// One read's header (scan history, `act_live`, `act_source.read`).
#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct ReadRow {
    pub at: u64,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    #[serde(default)]
    pub complete: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_from_h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cover_to_h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counts: Option<CountsRow>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<RootRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
}

impl ReadRow {
    /// The parts that make a scan-history row worth keeping when they change.
    fn signature(&self) -> (String, Option<String>, bool, bool) {
        (
            self.state.clone(),
            self.cause.clone(),
            self.complete,
            self.counts.as_ref().and_then(|c| c.future_dated).is_some(),
        )
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct ReadEvEntry {
    c: String,
    t: String,
    #[serde(flatten)]
    read: ReadRow,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct ReadEv {
    reads: Vec<ReadEvEntry>,
}

/// One part of a closed day: cells per source, and sessions inside those entries' `s`.
#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct DayClosedEv {
    day: u32,
    part: u32,
    parts: u32,
    #[serde(default)]
    src: Vec<CellsEv>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
pub struct BackfillRow {
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cause: Option<String>,
    pub at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
struct BackfillEv {
    c: String,
    t: String,
    #[serde(flatten)]
    row: BackfillRow,
}

// ----------------------------------------------------------------------------- stored rows

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
struct PartRow {
    r: u32,
    m: u32,
    i: u64,
    o: u64,
    cw: u64,
    cr: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    ln: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cn: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    fb: Option<u64>,
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
struct SessionRow {
    c: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hs: Option<String>,
    f: u64,
    l: u64,
    #[serde(default)]
    p: Vec<PartRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    tg: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwk: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwp: Option<String>,
    /// Quality bits of `p`.
    #[serde(default, skip_serializing_if = "is_zero32")]
    q: u32,
}

impl SessionRow {
    fn tokens(&self) -> u64 {
        self.p.iter().map(|p| p.i + p.o + p.cw + p.cr).sum()
    }
    fn cost(&self) -> u64 {
        self.p.iter().map(|p| p.cn.or(p.ln).unwrap_or(0)).sum()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, Default, PartialEq)]
struct SourceRow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    first_h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    settle_h: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    last_complete: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    read: Option<ReadRow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    bad_since: Option<u64>,
}

fn get_bytes(txn: &WriteTransaction, def: Bytes, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let table = txn.open_table(def)?;
    let value = table.get(key)?.map(|v| v.value().to_vec());
    Ok(value)
}

fn put_bytes(txn: &WriteTransaction, def: Bytes, key: &[u8], value: &[u8]) -> Result<()> {
    txn.open_table(def)?.insert(key, value)?;
    Ok(())
}

fn read_source(txn: &WriteTransaction, c: u32, t: u32) -> Result<SourceRow> {
    match get_bytes(txn, ACT_SOURCE, Key::new().u32(c).u32(t).as_slice())? {
        Some(b) => db::unpack(&b),
        None => Ok(SourceRow::default()),
    }
}

fn write_source(txn: &WriteTransaction, c: u32, t: u32, row: &SourceRow) -> Result<()> {
    put_bytes(txn, ACT_SOURCE, Key::new().u32(c).u32(t).as_slice(), &db::pack(row)?)
}

/// The newest closed day: every day at or before it is final in the ledger.
fn closed_through<T: ReadableTable<&'static [u8], ()>>(table: &T) -> Result<Option<u32>> {
    Ok(table.last()?.map(|(k, _)| db::be_u32(k.value(), 0)))
}

// ----------------------------------------------------------------------------- projector

pub struct ActivityProjector;

impl Projector for ActivityProjector {
    fn name(&self) -> &'static str {
        "activity"
    }
    fn version(&self) -> u32 {
        ACTIVITY_PROJECTOR_VERSION
    }
    fn reads(&self, _log: LogId) -> bool {
        true
    }
    fn apply(&self, txn: &WriteTransaction, _log: LogId, event: &Event) -> Result<Vec<Finding>> {
        match event.event_type.as_str() {
            "activity.cells" => {
                let ev: CellsEv = serde_json::from_value(msgpack::to_json(&event.payload))?;
                project_cells(txn, &ev)?;
            }
            "activity.read" => {
                let ev: ReadEv = serde_json::from_value(msgpack::to_json(&event.payload))?;
                for entry in &ev.reads {
                    project_read(txn, entry)?;
                }
            }
            "activity.day_closed" => {
                let ev: DayClosedEv = serde_json::from_value(msgpack::to_json(&event.payload))?;
                project_day_closed(txn, &ev)?;
            }
            "activity.backfill" => {
                let ev: BackfillEv = serde_json::from_value(msgpack::to_json(&event.payload))?;
                let c = intern(txn, K_COMPUTER, &ev.c)?;
                let t = intern(txn, K_TOOL, &ev.t)?;
                put_bytes(
                    txn,
                    ACT_BACKFILL,
                    Key::new().u32(c).u32(t).as_slice(),
                    &db::pack(&ev.row)?,
                )?;
            }
            _ => {}
        }
        Ok(Vec::new())
    }
    fn reset(&self, txn: &WriteTransaction) -> Result<()> {
        for def in [
            ACT_DICT,
            ACT_HOUR,
            ACT_SESSION,
            ACT_COVER,
            ACT_SOURCE,
            ACT_SCAN,
            ACT_LIVE,
            ACT_BACKFILL,
        ] {
            txn.delete_table(def)?;
        }
        txn.delete_table(ACT_DICT_REV)?;
        txn.delete_table(ACT_SESSION_BY_LAST)?;
        txn.delete_table(ACT_DAY_CLOSED)?;
        for kind in KINDS {
            txn.open_table(db::META)?
                .remove(format!("act_dict_next:{kind}").as_str())?;
        }
        create_tables(txn)
    }
}

fn cell_from_ev(txn: &WriteTransaction, ev: &CellEv) -> Result<(u32, Cell)> {
    Ok((
        ev.h / 24,
        Cell {
            hour: (ev.h % 24) as u8,
            session: session_bytes(ev.s.as_deref()),
            route: intern(txn, K_ROUTE, &ev.r)?,
            model: intern(txn, K_MODEL, &ev.m)?,
            effort: intern_opt(txn, K_EFFORT, ev.e.as_deref())?,
            account: intern_opt(txn, K_ACCOUNT, ev.a.as_deref())?,
            i: ev.i,
            o: ev.o,
            cw: ev.cw,
            cr: ev.cr,
            reasoning: ev.rs,
            logged: ev.ln,
            cost: ev.cn,
            fallback: ev.fb,
            events: ev.ev,
            quality: ev.q,
        },
    ))
}

fn del_id(txn: &WriteTransaction, ev: &DelEv) -> Result<(u32, CellId)> {
    Ok((
        ev.h / 24,
        (
            (ev.h % 24) as u8,
            session_bytes(ev.s.as_deref()),
            intern(txn, K_ROUTE, &ev.r)?,
            intern(txn, K_MODEL, &ev.m)?,
            intern_opt(txn, K_EFFORT, ev.e.as_deref())?,
            intern_opt(txn, K_ACCOUNT, ev.a.as_deref())?,
        ),
    ))
}

fn project_cells(txn: &WriteTransaction, ev: &CellsEv) -> Result<()> {
    let c = intern(txn, K_COMPUTER, &ev.c)?;
    let t = intern(txn, K_TOOL, &ev.t)?;
    let mut days: BTreeMap<u32, (Vec<CellId>, Vec<Cell>)> = BTreeMap::new();
    for d in &ev.del {
        let (day, id) = del_id(txn, d)?;
        days.entry(day).or_default().0.push(id);
    }
    let mut first: Option<u32> = None;
    let mut last: Option<u32> = None;
    for p in &ev.put {
        let (day, cell) = cell_from_ev(txn, p)?;
        first = Some(first.map_or(p.h, |f| f.min(p.h)));
        last = Some(last.map_or(p.h, |l| l.max(p.h)));
        days.entry(day).or_default().1.push(cell);
    }
    for (day, (dels, puts)) in days {
        let mut cells: BTreeMap<CellId, Cell> = load_day(txn, day, c, t)?.into_iter().map(|x| (x.id(), x)).collect();
        for id in dels {
            cells.remove(&id);
        }
        for cell in puts {
            cells.insert(cell.id(), cell);
        }
        let list: Vec<Cell> = cells.into_values().collect();
        store_day(txn, day, c, t, &list)?;
    }
    if first.is_some() {
        let mut src = read_source(txn, c, t)?;
        src.first_h = match (src.first_h, first) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        src.last_h = match (src.last_h, last) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
        write_source(txn, c, t, &src)?;
    }
    for s in &ev.s {
        merge_session(txn, t, s)?;
    }
    Ok(())
}

fn part_row(txn: &WriteTransaction, p: &PartEv) -> Result<PartRow> {
    Ok(PartRow {
        r: intern(txn, K_ROUTE, &p.r)?,
        m: intern(txn, K_MODEL, &p.m)?,
        i: p.i,
        o: p.o,
        cw: p.cw,
        cr: p.cr,
        ln: p.ln,
        cn: p.cn,
        fb: p.fb,
    })
}

/// The merged row: `first` only earlier, `last` only later, parts from the row with more tokens
/// (then more cost, then the larger encoding), attributes kept once known. Commutative.
fn merged_session(stored: Option<&SessionRow>, incoming: &SessionRow) -> SessionRow {
    let Some(old) = stored else {
        return incoming.clone();
    };
    let mut out = old.clone();
    out.f = old.f.min(incoming.f);
    out.l = old.l.max(incoming.l);
    // The row with more tokens (then more cost) carries the parts and the observing computer. An
    // exact tie keeps what is stored: the same totals either way.
    let rank = |s: &SessionRow| (s.tokens(), s.cost());
    if rank(incoming) > rank(old) || old.c == 0 {
        out.p = incoming.p.clone();
        out.c = incoming.c;
        out.q = incoming.q;
    }
    out.hs = old.hs.clone().max(incoming.hs.clone());
    out.tg = old.tg.clone().max(incoming.tg.clone());
    // the smaller folder key wins, so the result does not depend on order
    if incoming.cwk.is_some() && (old.cwk.is_none() || incoming.cwk < old.cwk) {
        out.cwk = incoming.cwk.clone();
        out.cwp = incoming.cwp.clone();
    }
    out
}

fn session_row(txn: &WriteTransaction, s: &SessEv) -> Result<SessionRow> {
    Ok(SessionRow {
        c: intern(txn, K_COMPUTER, &s.c)?,
        hs: s.hs.clone(),
        f: s.f,
        l: s.l,
        p: s.p.iter().map(|p| part_row(txn, p)).collect::<Result<Vec<_>>>()?,
        tg: s.tg.clone(),
        cwk: s.cwk.clone(),
        cwp: s.cwp.clone(),
        q: s.q,
    })
}

fn session_key(t: u32, key: &[u8; 8]) -> Key {
    Key::new().u32(t).bytes(key)
}

fn load_session(txn: &WriteTransaction, t: u32, key: &[u8; 8]) -> Result<Option<SessionRow>> {
    match get_bytes(txn, ACT_SESSION, session_key(t, key).as_slice())? {
        Some(b) => Ok(Some(db::unpack(&b)?)),
        None => Ok(None),
    }
}

fn merge_session(txn: &WriteTransaction, t: u32, s: &SessEv) -> Result<()> {
    let key = session_bytes(Some(&s.k));
    let incoming = session_row(txn, s)?;
    let stored = load_session(txn, t, &key)?;
    let merged = merged_session(stored.as_ref(), &incoming);
    if stored.as_ref() == Some(&merged) {
        return Ok(());
    }
    let mut pairs: Vec<(u32, u32)> = merged.p.iter().map(|p| (p.r, p.m)).collect();
    pairs.sort();
    pairs.dedup();
    let text = format!(
        "{t}|{}",
        pairs
            .iter()
            .map(|(r, m)| format!("{r}:{m}"))
            .collect::<Vec<_>>()
            .join(";")
    );
    let sig = intern(txn, K_SIGNATURE, &text)?;
    let mut entry = Vec::with_capacity(20);
    entry.extend_from_slice(&merged.f.to_be_bytes());
    entry.extend_from_slice(&merged.l.to_be_bytes());
    entry.extend_from_slice(&sig.to_be_bytes());
    let mut index = txn.open_table(ACT_SESSION_BY_LAST)?;
    if let Some(old) = &stored {
        index.remove(Key::new().u32((old.l / HOUR_MS) as u32).u32(t).bytes(&key).as_slice())?;
    }
    index.insert(
        Key::new()
            .u32((merged.l / HOUR_MS) as u32)
            .u32(t)
            .bytes(&key)
            .as_slice(),
        entry.as_slice(),
    )?;
    drop(index);
    put_bytes(txn, ACT_SESSION, session_key(t, &key).as_slice(), &db::pack(&merged)?)
}

fn project_read(txn: &WriteTransaction, entry: &ReadEvEntry) -> Result<()> {
    let c = intern(txn, K_COMPUTER, &entry.c)?;
    let t = intern(txn, K_TOOL, &entry.t)?;
    let r = &entry.read;
    let mut src = read_source(txn, c, t)?;
    let live = r.kind.is_none();
    // Only live reads are the source's current state; backfill reads add history and coverage.
    let newest = live && src.read.as_ref().map(|old| r.at >= old.at).unwrap_or(true);
    if newest {
        if BAD_STATES.contains(&r.state.as_str()) {
            src.bad_since = src.bad_since.or(Some(r.at));
        } else {
            src.bad_since = None;
        }
        src.read = Some(r.clone());
    }
    if r.complete && live {
        src.last_complete = Some(src.last_complete.unwrap_or(0).max(r.at));
        let settle = ((r.at / HOUR_MS) as u32).saturating_sub(SETTLE_HOURS);
        src.settle_h = Some(src.settle_h.unwrap_or(0).max(settle));
    }
    write_source(txn, c, t, &src)?;
    if r.complete
        && let (Some(from), Some(to)) = (r.cover_from_h, r.cover_to_h)
        && from < to
    {
        add_cover(txn, c, t, from, to)?;
    }
    put_bytes(
        txn,
        ACT_SCAN,
        Key::new().u32(c).u32(t).u64(r.at).as_slice(),
        &db::pack(r)?,
    )
}

/// Adds `[from, to)` to the source's covered spans, merging touching and overlapping ones.
fn add_cover(txn: &WriteTransaction, c: u32, t: u32, from: u32, to: u32) -> Result<()> {
    let prefix = Key::new().u32(c).u32(t);
    let lo = prefix.clone().u32(0);
    let hi = prefix.clone().u32(u32::MAX);
    let mut table = txn.open_table(ACT_COVER)?;
    let (mut start, mut end) = (from, to);
    let mut remove = Vec::new();
    for item in table.range(lo.as_slice()..=hi.as_slice())? {
        let (k, v) = item?;
        let s = db::be_u32(k.value(), 8);
        let e = db::be_u32(v.value(), 0);
        if e < start || s > end {
            continue;
        }
        start = start.min(s);
        end = end.max(e);
        remove.push(k.value().to_vec());
    }
    for k in remove {
        table.remove(k.as_slice())?;
    }
    table.insert(prefix.u32(start).as_slice(), end.to_be_bytes().as_slice())?;
    Ok(())
}

fn project_day_closed(txn: &WriteTransaction, ev: &DayClosedEv) -> Result<()> {
    if ev.part == 0 {
        // The close is the whole truth of its day: every source's cells are replaced.
        let lo = Key::new().u32(ev.day);
        let hi = Key::new().u32(ev.day).u32(u32::MAX).u32(u32::MAX);
        let mut keys = Vec::new();
        {
            let table = txn.open_table(ACT_HOUR)?;
            for item in table.range(lo.as_slice()..=hi.as_slice())? {
                keys.push(item?.0.value().to_vec());
            }
        }
        let mut table = txn.open_table(ACT_HOUR)?;
        for k in keys {
            table.remove(k.as_slice())?;
        }
        txn.open_table(ACT_DAY_CLOSED)?
            .insert(Key::new().u32(ev.day).as_slice(), ())?;
    }
    for src in &ev.src {
        project_cells(txn, src)?;
    }
    Ok(())
}

// ----------------------------------------------------------------------------- ingest

#[derive(Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestResult {
    pub accepted: usize,
    pub refused: Vec<Json>,
    pub put: usize,
    pub deleted: usize,
    pub sessions: usize,
    pub events: usize,
    pub reads_logged: usize,
}

struct Prepared<'a> {
    checked: contract::Checked<'a>,
    /// Cells by absolute hour, summed by identity.
    hours: BTreeMap<u32, Vec<CellEv>>,
    same: BTreeSet<u32>,
}

fn nano_opt(v: Option<f64>) -> Option<u64> {
    v.map(contract::nano)
}

fn prepare(report: &Report) -> Result<Prepared<'_>> {
    let checked = report.check()?;
    let mut hours: BTreeMap<u32, BTreeMap<(Option<String>, String, String, Option<String>, Option<String>), CellEv>> =
        BTreeMap::new();
    for ((cell, hour_ms), quality) in report
        .cells
        .iter()
        .zip(checked.cell_hours.iter())
        .zip(checked.cell_quality.iter())
    {
        // dated after the store's clock: left out (counted in `checked.future_dated`)
        let Some(hour_ms) = hour_ms else { continue };
        let tokens = cell.input_tokens + cell.output_tokens + cell.cache_creation_tokens + cell.cache_read_tokens;
        let cost = cell.cost_usd.unwrap_or(0.0) + cell.logged_cost_usd.unwrap_or(0.0);
        if tokens == 0 && cost == 0.0 {
            continue;
        }
        let h = (hour_ms / HOUR_MS) as u32;
        let ev = CellEv {
            h,
            s: cell.session_key.clone(),
            r: cell.route.clone(),
            m: cell.model_raw.clone(),
            e: cell.effort.clone(),
            a: cell.account_key.clone(),
            i: cell.input_tokens,
            o: cell.output_tokens,
            cw: cell.cache_creation_tokens,
            cr: cell.cache_read_tokens,
            rs: cell.reasoning_output_tokens,
            ln: nano_opt(cell.logged_cost_usd),
            cn: nano_opt(cell.cost_usd),
            fb: nano_opt(cell.fallback_cost_usd),
            ev: cell.events,
            q: *quality,
        };
        let id = (ev.s.clone(), ev.r.clone(), ev.m.clone(), ev.e.clone(), ev.a.clone());
        let slot = hours.entry(h).or_default();
        match slot.get_mut(&id) {
            Some(existing) => {
                // A report is a multiset: duplicate identities add up.
                let sum = |a: Option<u64>, b: Option<u64>| match (a, b) {
                    (Some(x), Some(y)) => Some(x + y),
                    _ => None,
                };
                existing.i += ev.i;
                existing.o += ev.o;
                existing.cw += ev.cw;
                existing.cr += ev.cr;
                existing.rs = sum(existing.rs, ev.rs);
                existing.ln = match (existing.ln, ev.ln) {
                    (None, None) => None,
                    (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
                };
                existing.cn = sum(existing.cn, ev.cn);
                existing.fb = sum(existing.fb, ev.fb);
                existing.ev = sum(existing.ev, ev.ev);
                existing.q |= ev.q;
            }
            None => {
                slot.insert(id, ev);
            }
        }
    }
    Ok(Prepared {
        checked,
        hours: hours.into_iter().map(|(h, m)| (h, m.into_values().collect())).collect(),
        same: report.same_days.iter().copied().collect(),
    })
}

fn sess_ev(s: &contract::Session, computer: &str, (f, l): (u64, u64)) -> SessEv {
    SessEv {
        k: s.session_key.clone(),
        hs: s.session_hash.clone(),
        c: computer.to_string(),
        f,
        l,
        p: s.parts
            .iter()
            .map(|p| PartEv {
                r: p.route.clone(),
                m: p.model_raw.clone(),
                i: p.input_tokens,
                o: p.output_tokens,
                cw: p.cache_creation_tokens,
                cr: p.cache_read_tokens,
                ln: nano_opt(p.logged_cost_usd),
                cn: nano_opt(p.cost_usd),
                fb: nano_opt(p.fallback_cost_usd),
            })
            .collect(),
        tg: s.target.clone(),
        cwk: s.cwd.as_ref().map(|c| c.key.clone()),
        cwp: s.cwd.as_ref().map(|c| c.path.clone()),
        q: s.quality_bits().unwrap_or(0),
    }
}

fn ev_from_cell(names: &mut dyn FnMut(u8, u32) -> Result<Option<String>>, day: u32, c: &Cell) -> Result<CellEv> {
    Ok(CellEv {
        h: day * 24 + c.hour as u32,
        s: session_hex(&c.session),
        r: names(K_ROUTE, c.route)?.unwrap_or_default(),
        m: names(K_MODEL, c.model)?.unwrap_or_default(),
        e: if c.effort == 0 {
            None
        } else {
            names(K_EFFORT, c.effort)?
        },
        a: if c.account == 0 {
            None
        } else {
            names(K_ACCOUNT, c.account)?
        },
        i: c.i,
        o: c.o,
        cw: c.cw,
        cr: c.cr,
        rs: c.reasoning,
        ln: c.logged,
        cn: c.cost,
        fb: c.fallback,
        ev: c.events,
        q: c.quality,
    })
}

fn dict_text(txn: &WriteTransaction, kind: u8, id: u32) -> Result<Option<String>> {
    if id == 0 && kind != K_ROUTE {
        return Ok(None);
    }
    let table = txn.open_table(ACT_DICT)?;
    let text = table
        .get(Key::new().u8(kind).u32(id).as_slice())?
        .map(|v| String::from_utf8_lossy(v.value()).into_owned());
    Ok(text)
}

/// Decides one report's changes for the days `wanted` accepts (2.4.2 of the design): per
/// computer, tool and hour, replace, raise or leave. Returns the puts, deletions and sessions.
fn decide(
    txn: &WriteTransaction,
    p: &Prepared<'_>,
    wanted: &dyn Fn(u32) -> bool,
) -> Result<(Vec<CellEv>, Vec<DelEv>, Vec<SessEv>)> {
    let r = p.checked.report;
    let kind = p.checked.kind;
    let live = kind == "live";
    let c = intern(txn, K_COMPUTER, &r.computer)?;
    let t = intern(txn, K_TOOL, &r.tool)?;
    let src = read_source(txn, c, t)?;
    let settle = src.settle_h;
    let read_h = (p.checked.read_at_ms / HOUR_MS) as u32;
    let cover_from = p.checked.cover_from_ms.map(|m| (m / HOUR_MS) as u32);
    let replace_span = if live && r.complete {
        cover_from.map(|f| (f.max(settle.unwrap_or(0)), read_h))
    } else {
        None
    };
    // Every hour to decide: the report's, and with a complete live report every hour of the
    // replace span (stored cells there that the report no longer has are removed).
    let mut days: BTreeSet<u32> = p.hours.keys().map(|h| h / 24).collect();
    if let Some((a, b)) = replace_span {
        for d in a / 24..=b / 24 {
            days.insert(d);
        }
    }
    days.retain(|d| wanted(*d) && !p.same.contains(d));
    let mut puts = Vec::new();
    let mut dels = Vec::new();
    for day in days {
        let stored = load_day(txn, day, c, t)?;
        let mut by_hour: BTreeMap<u8, Vec<Cell>> = BTreeMap::new();
        for cell in stored {
            by_hour.entry(cell.hour).or_default().push(cell);
        }
        let mut hours: BTreeSet<u32> = (day * 24..day * 24 + 24).filter(|h| p.hours.contains_key(h)).collect();
        if let Some((a, b)) = replace_span {
            for hour in by_hour.keys() {
                let h = day * 24 + *hour as u32;
                if h >= a && h <= b {
                    hours.insert(h);
                }
            }
        }
        for h in hours {
            if live && settle.map(|s| h < s).unwrap_or(false) {
                continue;
            }
            let incoming: Vec<Cell> = match p.hours.get(&h) {
                Some(list) => list
                    .iter()
                    .map(|e| cell_from_ev(txn, e).map(|x| x.1))
                    .collect::<Result<_>>()?,
                None => Vec::new(),
            };
            let current = by_hour.remove(&((h % 24) as u8)).unwrap_or_default();
            let replace = replace_span.map(|(a, b)| h >= a && h <= b).unwrap_or(false);
            if !replace {
                // raise: the report's hour wins only with more tokens, or as many tokens tied to
                // more sessions
                let tokens = |cells: &[Cell]| cells.iter().map(Cell::tokens).sum::<u64>();
                let tied = |cells: &[Cell]| cells.iter().filter(|x| x.session != NO_SESSION).count();
                let (a, b) = (tokens(&incoming), tokens(&current));
                if !(a > b || (a == b && a > 0 && tied(&incoming) > tied(&current))) {
                    continue;
                }
            }
            let now: BTreeMap<CellId, &Cell> = incoming.iter().map(|x| (x.id(), x)).collect();
            let was: BTreeMap<CellId, &Cell> = current.iter().map(|x| (x.id(), x)).collect();
            let mut text = |kind: u8, id: u32| dict_text(txn, kind, id);
            for (id, cell) in &was {
                if !now.contains_key(id) {
                    let ev = ev_from_cell(&mut text, day, cell)?;
                    dels.push(DelEv {
                        h: ev.h,
                        s: ev.s,
                        r: ev.r,
                        m: ev.m,
                        e: ev.e,
                        a: ev.a,
                    });
                }
            }
            for (id, cell) in &now {
                if was.get(id) != Some(cell) {
                    puts.push(ev_from_cell(&mut text, day, cell)?);
                }
            }
        }
    }
    let mut sessions = Vec::new();
    for (s, span) in r.sessions.iter().zip(p.checked.session_spans.iter()) {
        // a session that starts after the store's clock is left out with its cells
        let Some(span) = span else { continue };
        let ev = sess_ev(s, &r.computer, *span);
        if !wanted((ev.l / DAY_MS) as u32) {
            continue;
        }
        let key = session_bytes(Some(&ev.k));
        let stored = load_session(txn, t, &key)?;
        let incoming = session_row(txn, &ev)?;
        if stored.as_ref() != Some(&merged_session(stored.as_ref(), &incoming)) {
            sessions.push(ev);
        }
    }
    Ok((puts, dels, sessions))
}

fn push_cells(
    batch: &mut engine::Batch<'_>,
    c: &str,
    t: &str,
    puts: Vec<CellEv>,
    dels: Vec<DelEv>,
    sessions: Vec<SessEv>,
) -> Result<usize> {
    let mut events = 0;
    let mut puts = puts.into_iter().peekable();
    let mut dels = dels.into_iter().peekable();
    let mut sessions = sessions.into_iter().peekable();
    while puts.peek().is_some() || dels.peek().is_some() || sessions.peek().is_some() {
        let ev = CellsEv {
            c: c.to_string(),
            t: t.to_string(),
            del: dels.by_ref().take(CELLS_PER_EVENT).collect(),
            put: puts.by_ref().take(CELLS_PER_EVENT).collect(),
            s: sessions.by_ref().take(SESSIONS_PER_EVENT).collect(),
        };
        let mut event = Event::new(
            "activity.cells",
            now_ms(),
            msgpack::from_json(&serde_json::to_value(&ev)?)?,
        );
        event.source_at_ms = ev.put.iter().map(|x| x.h as u64 * HOUR_MS).min();
        batch.push(event)?;
        events += 1;
    }
    Ok(events)
}

fn counts_row(c: &contract::Counts) -> CountsRow {
    CountsRow {
        roots: c.roots,
        found: c.files_found,
        read: c.files_read,
        failed: c.files_failed,
        newest_ms: c.newest_mtime.as_ref().and_then(contract::Mtime::ms),
        future_dated: c.future_dated.filter(|n| *n > 0),
    }
}

fn read_row(p: &Prepared<'_>) -> ReadRow {
    let r = p.checked.report;
    let read_h = (p.checked.read_at_ms / HOUR_MS) as u32;
    let (cover_from_h, cover_to_h) = if r.complete {
        let from = if p.checked.kind == "live" {
            p.checked.cover_from_ms.map(|m| (m / HOUR_MS) as u32)
        } else {
            // a complete deep read covers from the oldest hour it found
            p.hours
                .keys()
                .next()
                .copied()
                .or(p.checked.cover_from_ms.map(|m| (m / HOUR_MS) as u32))
        };
        (from, from.map(|_| read_h + 1))
    } else {
        (None, None)
    };
    ReadRow {
        at: p.checked.read_at_ms,
        state: r.state.clone(),
        cause: r.cause.clone(),
        complete: r.complete,
        cover_from_h,
        cover_to_h,
        counts: match (r.counts.as_ref().map(counts_row), p.checked.future_dated) {
            (counts, 0) => counts,
            (counts, n) => Some(CountsRow {
                future_dated: Some(n),
                ..counts.unwrap_or_default()
            }),
        },
        roots: r
            .roots
            .iter()
            .map(|x| RootRow {
                root: x.root.clone(),
                origin: x.origin.clone(),
                state: x.state.clone(),
                cause: x.cause.clone(),
                counts: x.counts.as_ref().map(counts_row),
            })
            .collect(),
        kind: if p.checked.kind == "live" {
            None
        } else {
            Some(p.checked.kind.to_string())
        },
    }
}

/// Records reports (design 4.2): cells and sessions by replace, raise or leave; read headers as
/// scan history when they change or hourly; the live read time always.
pub fn ingest(engine: &Engine, reports: &[Report]) -> Result<IngestResult> {
    let mut result = IngestResult::default();
    let mut prepared = Vec::new();
    for (index, report) in reports.iter().enumerate() {
        match prepare(report) {
            Ok(p) => prepared.push(p),
            Err(e) => result
                .refused
                .push(json!({ "index": index, "code": e.code, "message": e.message })),
        }
    }
    result.accepted = prepared.len();
    if prepared.is_empty() {
        return Ok(result);
    }
    // Readings first: every day after the newest closed one. The ledger then takes the closed
    // days as that commit saw them; days only ever close, so each day is decided once.
    let mut closed: Option<u32> = None;
    for log in [LogId::Readings, LogId::Ledger] {
        let (put, del, sess, events) = engine.commit(log, |batch| {
            if log == LogId::Readings {
                closed = closed_through(&batch.txn.open_table(ACT_DAY_CLOSED)?)?;
            }
            let ct = closed;
            let wanted = move |d: u32| match (log, ct) {
                (LogId::Readings, Some(c)) => d > c,
                (LogId::Readings, None) => true,
                (LogId::Ledger, Some(c)) => d <= c,
                (LogId::Ledger, None) => false,
            };
            let (mut put, mut del, mut sess, mut events) = (0, 0, 0, 0);
            for p in &prepared {
                let (puts, dels, sessions) = decide(batch.txn, p, &wanted)?;
                put += puts.len();
                del += dels.len();
                sess += sessions.len();
                let r = p.checked.report;
                events += push_cells(batch, &r.computer, &r.tool, puts, dels, sessions)?;
            }
            Ok((put, del, sess, events))
        })?;
        result.put += put;
        result.deleted += del;
        result.sessions += sess;
        result.events += events;
    }
    // Scan history: a read is logged when its state, cause or completeness changed, when its
    // covered span reaches a new hour, or when it has not been logged for an hour.
    let reads: Vec<ReadEvEntry> = prepared
        .iter()
        .map(|p| ReadEvEntry {
            c: p.checked.report.computer.clone(),
            t: p.checked.report.tool.clone(),
            read: read_row(p),
        })
        .collect();
    let logged = engine.commit(LogId::Ledger, |batch| {
        let mut due = Vec::new();
        for entry in &reads {
            let c = intern(batch.txn, K_COMPUTER, &entry.c)?;
            let t = intern(batch.txn, K_TOOL, &entry.t)?;
            let src = read_source(batch.txn, c, t)?;
            let log_it = match &src.read {
                // a backfill read is history when it covers something; an import always is, so the
                // scan history says where imported usage came from
                _ if entry.read.kind.is_some() => entry.read.complete || entry.read.kind.as_deref() == Some("import"),
                None => true,
                Some(old) => {
                    entry.read.at > old.at
                        && (old.signature() != entry.read.signature()
                            || entry.read.at >= old.at + READ_LOG_EVERY_MS
                            || entry.read.cover_to_h > old.cover_to_h
                            || entry.read.kind != old.kind)
                }
            };
            if log_it {
                due.push(entry.clone());
            }
        }
        if due.is_empty() {
            return Ok(0);
        }
        let n = due.len();
        let ev = ReadEv { reads: due };
        let event = Event::new(
            "activity.read",
            now_ms(),
            msgpack::from_json(&serde_json::to_value(&ev)?)?,
        );
        batch.push(event)?;
        Ok(n)
    })?;
    result.reads_logged = logged;
    // The live read time: not an event, refreshed on every ingest.
    engine.with_db(|db| {
        let txn = begin(db)?;
        {
            let mut live = txn.open_table(ACT_LIVE)?;
            for entry in &reads {
                let key = Key::new().bytes(entry.c.as_bytes()).u8(0).bytes(entry.t.as_bytes());
                let newer = match live.get(key.as_slice())? {
                    Some(v) => db::unpack::<ReadRow>(v.value())
                        .map(|old| entry.read.at >= old.at)
                        .unwrap_or(true),
                    None => true,
                };
                if newer && entry.read.kind.is_none() {
                    live.insert(key.as_slice(), db::pack(&entry.read)?.as_slice())?;
                }
            }
        }
        txn.commit()?;
        Ok(())
    })?;
    Ok(result)
}

/// Records a backfill's state for one source (ledger, kept for good).
pub fn backfill_mark(engine: &Engine, computer: &str, tool: &str, row: BackfillRow) -> Result<()> {
    if !contract::ident_ok(computer) || !contract::ident_ok(tool) {
        fail!("invalid", "computer and tool are lower-case identifiers");
    }
    if !["pending", "running", "done", "failed"].contains(&row.state.as_str())
        || row.cause.as_deref().map(|c| !contract::cause_ok(c)).unwrap_or(false)
    {
        fail!("invalid", "unknown backfill state");
    }
    let ev = BackfillEv {
        c: computer.to_string(),
        t: tool.to_string(),
        row,
    };
    engine.commit(LogId::Ledger, |batch| {
        let event = Event::new(
            "activity.backfill",
            now_ms(),
            msgpack::from_json(&serde_json::to_value(&ev)?)?,
        );
        batch.push(event).map(|_| ())
    })
}

// ----------------------------------------------------------------------------- maintenance

/// Closes every finished UTC day (74 hours after it ends) that has cells: its whole truth goes to
/// the ledger, with the sessions that last moved in it or in the empty days before it.
pub fn close_days(engine: &Engine, now: u64) -> Result<Json> {
    let mut closed = Vec::new();
    loop {
        let day = engine.commit(LogId::Ledger, |batch| {
            let txn = batch.txn;
            let ct = closed_through(&txn.open_table(ACT_DAY_CLOSED)?)?;
            let start = ct.map(|d| d + 1).unwrap_or(0);
            let next = {
                let table = txn.open_table(ACT_HOUR)?;
                let lo = Key::new().u32(start);
                let found = table.range(lo.as_slice()..)?.next();
                match found {
                    Some(item) => db::be_u32(item?.0.value(), 0),
                    None => return Ok(None),
                }
            };
            if now < (next as u64 + 1) * DAY_MS + CLOSE_AFTER_MS {
                return Ok(None);
            }
            let mut sources: Vec<CellsEv> = Vec::new();
            {
                let table = txn.open_table(ACT_HOUR)?;
                let lo = Key::new().u32(next);
                let hi = Key::new().u32(next).u32(u32::MAX).u32(u32::MAX);
                let rows: Vec<(Vec<u8>, Vec<u8>)> = table
                    .range(lo.as_slice()..=hi.as_slice())?
                    .map(|item| item.map(|(k, v)| (k.value().to_vec(), v.value().to_vec())))
                    .collect::<std::result::Result<_, _>>()?;
                drop(table);
                let mut text = |kind: u8, id: u32| dict_text(txn, kind, id);
                for (k, v) in rows {
                    let (c, t) = (db::be_u32(&k, 4), db::be_u32(&k, 8));
                    let cells = decode_cells(&v)?;
                    let mut ev = CellsEv {
                        c: text(K_COMPUTER, c)?.unwrap_or_default(),
                        t: text(K_TOOL, t)?.unwrap_or_default(),
                        ..Default::default()
                    };
                    for cell in &cells {
                        ev.put.push(ev_from_cell(&mut text, next, cell)?);
                    }
                    sources.push(ev);
                }
            }
            let mut sessions: Vec<(String, SessEv)> = Vec::new();
            {
                let index = txn.open_table(ACT_SESSION_BY_LAST)?;
                let lo = Key::new().u32(start * 24);
                let hi = Key::new().u32(next * 24 + 23).u32(u32::MAX).bytes(&[0xff; 8]);
                let keys: Vec<Vec<u8>> = index
                    .range(lo.as_slice()..=hi.as_slice())?
                    .map(|item| item.map(|(k, _)| k.value().to_vec()))
                    .collect::<std::result::Result<_, _>>()?;
                drop(index);
                for k in keys {
                    let t = db::be_u32(&k, 4);
                    let key: [u8; 8] = k[8..16].try_into().unwrap();
                    if let Some(row) = load_session(txn, t, &key)? {
                        let tool = dict_text(txn, K_TOOL, t)?.unwrap_or_default();
                        sessions.push((tool, session_ev_from_row(txn, &key, &row)?));
                    }
                }
            }
            // Parts: at most CELLS_PER_EVENT cells and SESSIONS_PER_EVENT sessions each; a source's
            // cells may span parts. Sessions ride in their own per-tool groups.
            let mut parts: Vec<DayClosedEv> = Vec::new();
            let mut current = DayClosedEv {
                day: next,
                ..Default::default()
            };
            let mut size = 0;
            for src in sources {
                let mut rest = src.put;
                while !rest.is_empty() {
                    let room = CELLS_PER_EVENT - size;
                    let take: Vec<CellEv> = rest.drain(..room.min(rest.len())).collect();
                    size += take.len();
                    current.src.push(CellsEv {
                        c: src.c.clone(),
                        t: src.t.clone(),
                        put: take,
                        ..Default::default()
                    });
                    if size >= CELLS_PER_EVENT {
                        parts.push(std::mem::take(&mut current));
                        current.day = next;
                        size = 0;
                    }
                }
            }
            let mut by_tool: BTreeMap<String, Vec<SessEv>> = BTreeMap::new();
            for (tool, s) in sessions {
                by_tool.entry(tool).or_default().push(s);
            }
            for (tool, list) in by_tool {
                for chunk in list.chunks(SESSIONS_PER_EVENT) {
                    current.src.push(CellsEv {
                        c: chunk[0].c.clone(),
                        t: tool.clone(),
                        s: chunk.to_vec(),
                        ..Default::default()
                    });
                    parts.push(std::mem::take(&mut current));
                    current.day = next;
                }
            }
            if !current.src.is_empty() || parts.is_empty() {
                parts.push(current);
            }
            let total = parts.len() as u32;
            for (i, mut part) in parts.into_iter().enumerate() {
                part.part = i as u32;
                part.parts = total;
                let mut event = Event::new(
                    "activity.day_closed",
                    now_ms(),
                    msgpack::from_json(&serde_json::to_value(&part)?)?,
                );
                event.idempotency_key = Some(format!("{next}:{i}"));
                batch.push(event)?;
            }
            Ok(Some(next))
        })?;
        match day {
            Some(d) => closed.push(d),
            None => break,
        }
    }
    Ok(json!({ "closed": closed }))
}

fn session_ev_from_row(txn: &WriteTransaction, key: &[u8; 8], row: &SessionRow) -> Result<SessEv> {
    Ok(SessEv {
        k: session_hex(key).unwrap_or_default(),
        hs: row.hs.clone(),
        c: dict_text(txn, K_COMPUTER, row.c)?.unwrap_or_default(),
        f: row.f,
        l: row.l,
        p: row
            .p
            .iter()
            .map(|p| {
                Ok(PartEv {
                    r: dict_text(txn, K_ROUTE, p.r)?.unwrap_or_default(),
                    m: dict_text(txn, K_MODEL, p.m)?.unwrap_or_default(),
                    i: p.i,
                    o: p.o,
                    cw: p.cw,
                    cr: p.cr,
                    ln: p.ln,
                    cn: p.cn,
                    fb: p.fb,
                })
            })
            .collect::<Result<Vec<_>>>()?,
        tg: row.tg.clone(),
        cwk: row.cwk.clone(),
        cwp: row.cwp.clone(),
        q: row.q,
    })
}

/// The oldest day that is not yet closed in the ledger (cells or sessions), for segment retirement:
/// a readings segment holding events of an open day must stay.
pub fn oldest_open_day(engine: &Engine) -> Result<Option<u32>> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let ct = closed_through(&read.open_table(ACT_DAY_CLOSED)?)?;
        let start = ct.map(|d| d + 1).unwrap_or(0);
        let hours = read.open_table(ACT_HOUR)?;
        let cell_day = hours
            .range(Key::new().u32(start).as_slice()..)?
            .next()
            .transpose()?
            .map(|(k, _)| db::be_u32(k.value(), 0));
        let index = read.open_table(ACT_SESSION_BY_LAST)?;
        let session_day = index
            .range(Key::new().u32(start.saturating_mul(24)).as_slice()..)?
            .next()
            .transpose()?
            .map(|(k, _)| db::be_u32(k.value(), 0) / 24);
        Ok(match (cell_day, session_day) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        })
    })
}

/// Opt-in retention (design 3.4): with `retention_days` set, deletes day values, sessions, cover
/// spans and scan rows older than the cutoff. Without it nothing is deleted.
pub fn prune(engine: &Engine, now: u64, retention_days: Option<u64>) -> Result<Json> {
    let Some(days) = retention_days else {
        return Ok(json!({ "removedRows": 0, "cutoffMs": null }));
    };
    let cutoff = now.saturating_sub(days * DAY_MS);
    let cutoff_day = (cutoff / DAY_MS) as u32;
    let cutoff_h = (cutoff / HOUR_MS) as u32;
    let mut removed = 0usize;
    loop {
        let _logs = engine.lock_logs();
        let n = engine.with_db(|db| {
            let txn = begin(db)?;
            let mut n = 0usize;
            let hour_keys: Vec<Vec<u8>> = {
                let t = txn.open_table(ACT_HOUR)?;
                t.range(..Key::new().u32(cutoff_day).as_slice())?
                    .take(PRUNE_BATCH)
                    .map(|i| i.map(|(k, _)| k.value().to_vec()))
                    .collect::<std::result::Result<_, _>>()?
            };
            {
                let mut t = txn.open_table(ACT_HOUR)?;
                for k in &hour_keys {
                    t.remove(k.as_slice())?;
                }
            }
            n += hour_keys.len();
            let session_keys: Vec<Vec<u8>> = {
                let t = txn.open_table(ACT_SESSION_BY_LAST)?;
                t.range(..Key::new().u32(cutoff_h).as_slice())?
                    .take(PRUNE_BATCH)
                    .map(|i| i.map(|(k, _)| k.value().to_vec()))
                    .collect::<std::result::Result<_, _>>()?
            };
            {
                let mut index = txn.open_table(ACT_SESSION_BY_LAST)?;
                let mut rows = txn.open_table(ACT_SESSION)?;
                for k in &session_keys {
                    index.remove(k.as_slice())?;
                    rows.remove(&k[4..16])?;
                }
            }
            n += session_keys.len();
            let (covers, scans): (Vec<Vec<u8>>, Vec<Vec<u8>>) = {
                let cover = txn.open_table(ACT_COVER)?;
                let mut covers = Vec::new();
                for item in cover.iter()? {
                    let (k, v) = item?;
                    if db::be_u32(v.value(), 0) <= cutoff_h {
                        covers.push(k.value().to_vec());
                    }
                    if covers.len() >= PRUNE_BATCH {
                        break;
                    }
                }
                let scan = txn.open_table(ACT_SCAN)?;
                let mut scans = Vec::new();
                for item in scan.iter()? {
                    let (k, _) = item?;
                    if db::be_u64(k.value(), 8) < cutoff {
                        scans.push(k.value().to_vec());
                    }
                    if scans.len() >= PRUNE_BATCH {
                        break;
                    }
                }
                (covers, scans)
            };
            {
                let mut t = txn.open_table(ACT_COVER)?;
                for k in &covers {
                    t.remove(k.as_slice())?;
                }
                let mut t = txn.open_table(ACT_SCAN)?;
                for k in &scans {
                    t.remove(k.as_slice())?;
                }
            }
            n += covers.len() + scans.len();
            txn.commit()?;
            Ok((
                n,
                [hour_keys.len(), session_keys.len(), covers.len(), scans.len()]
                    .iter()
                    .any(|x| *x >= PRUNE_BATCH),
            ))
        })?;
        removed += n.0;
        if !n.1 {
            break;
        }
    }
    Ok(json!({ "removedRows": removed, "cutoffMs": cutoff }))
}

// ----------------------------------------------------------------------------- queries

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RangeQuery {
    pub from_ms: u64,
    pub to_ms: u64,
    /// `hour` (default) or `day`.
    #[serde(default)]
    pub resolution: Option<String>,
    /// With `day`: the buckets' starts as UTC ms, ascending; the last one ends the range.
    #[serde(default)]
    pub day_starts: Vec<u64>,
    /// Split rows by computer.
    #[serde(default)]
    pub by_computer: bool,
    #[serde(default)]
    pub session_sample: Option<usize>,
}

fn names_list(map: &mut BTreeMap<String, usize>, list: &mut Vec<String>, text: String) -> usize {
    if let Some(i) = map.get(&text) {
        return *i;
    }
    let i = list.len();
    map.insert(text.clone(), i);
    list.push(text);
    i
}

/// A bucket's sums. A cost, fallback cost or event count that any cell lacks is unknown for the
/// bucket (null), never a partial sum; the logged cost is the sum of the cells that logged one.
#[derive(Default, Clone)]
struct Sum {
    i: u64,
    o: u64,
    cw: u64,
    cr: u64,
    logged: Option<u64>,
    cost: u64,
    cost_missing: bool,
    fallback: u64,
    fallback_missing: bool,
    events: u64,
    events_missing: bool,
}

impl Sum {
    fn add(&mut self, c: &Cell) {
        self.i += c.i;
        self.o += c.o;
        self.cw += c.cw;
        self.cr += c.cr;
        if let Some(l) = c.logged {
            self.logged = Some(self.logged.unwrap_or(0) + l);
        }
        match c.cost {
            Some(v) => self.cost += v,
            None => self.cost_missing = true,
        }
        match c.fallback {
            Some(v) => self.fallback += v,
            None => self.fallback_missing = true,
        }
        match c.events {
            Some(v) => self.events += v,
            None => self.events_missing = true,
        }
    }
    fn known(v: u64, missing: bool) -> Json {
        if missing { Json::Null } else { json!(v) }
    }
}

fn opt_json(v: Option<u64>) -> Json {
    v.map(|x| json!(x)).unwrap_or(Json::Null)
}

/// Cells in `[from, to]` (hour starts), summed per bucket, tool, route and model (and computer
/// when asked), with the sessions active in the range.
pub fn range(engine: &Engine, q: &RangeQuery) -> Result<Json> {
    let day_res = q.resolution.as_deref() == Some("day");
    if q.to_ms < q.from_ms {
        fail!("invalid", "the range ends before it starts");
    }
    if day_res && (q.day_starts.len() < 2 || q.day_starts.windows(2).any(|w| w[0] >= w[1])) {
        fail!("invalid", "day buckets need ascending starts");
    }
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let dict = read.open_table(ACT_DICT)?;
        let mut names = Names::new(&dict);
        let hours = read.open_table(ACT_HOUR)?;
        let (mut tools_m, mut routes_m, mut models_m, mut comps_m) =
            (BTreeMap::new(), BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
        let (mut tools, mut routes, mut models, mut comps) = (Vec::new(), Vec::new(), Vec::new(), Vec::new());
        let mut sums: BTreeMap<(u64, u32, u32, u32, u32, u32), Sum> = BTreeMap::new();
        // per tool and quality bits: the first and last flagged hour in the range
        let mut flagged: BTreeMap<(u32, u32), (u64, u64)> = BTreeMap::new();
        let lo = Key::new().u32((q.from_ms / DAY_MS) as u32);
        let hi = Key::new().u32((q.to_ms / DAY_MS) as u32).u32(u32::MAX).u32(u32::MAX);
        for item in hours.range(lo.as_slice()..=hi.as_slice())? {
            let (k, v) = item?;
            let day = db::be_u32(k.value(), 0);
            let (c, t) = (db::be_u32(k.value(), 4), db::be_u32(k.value(), 8));
            for cell in decode_cells(v.value())? {
                let at = (day as u64 * 24 + cell.hour as u64) * HOUR_MS;
                if at < q.from_ms || at > q.to_ms {
                    continue;
                }
                let bucket = if day_res {
                    let i = q.day_starts.partition_point(|s| *s <= at);
                    if i == 0 || i >= q.day_starts.len() {
                        continue;
                    }
                    q.day_starts[i - 1]
                } else {
                    at
                };
                let key = (
                    bucket,
                    if q.by_computer { c } else { 0 },
                    t,
                    cell.route,
                    cell.model,
                    cell.quality,
                );
                sums.entry(key).or_default().add(&cell);
                if cell.quality != 0 {
                    let span = flagged.entry((t, cell.quality)).or_insert((at, at));
                    span.0 = span.0.min(at);
                    span.1 = span.1.max(at);
                }
                if sums.len() > MAX_ROWS {
                    fail!("too_large", "the range holds too many rows; ask for day buckets");
                }
            }
        }
        let mut rows = Vec::with_capacity(sums.len());
        for ((bucket, c, t, r, m, quality), s) in sums {
            let ci = if q.by_computer {
                json!(names_list(&mut comps_m, &mut comps, names.get(K_COMPUTER, c)?))
            } else {
                Json::Null
            };
            let ti = names_list(&mut tools_m, &mut tools, names.get(K_TOOL, t)?);
            let ri = names_list(&mut routes_m, &mut routes, names.get(K_ROUTE, r)?);
            let mi = names_list(&mut models_m, &mut models, names.get(K_MODEL, m)?);
            rows.push(json!([
                bucket,
                ci,
                ti,
                ri,
                mi,
                s.i,
                s.o,
                s.cw,
                s.cr,
                opt_json(s.logged),
                Sum::known(s.cost, s.cost_missing),
                Sum::known(s.fallback, s.fallback_missing),
                Sum::known(s.events, s.events_missing),
                quality
            ]));
        }
        let mut quality_out = Vec::with_capacity(flagged.len());
        for ((t, bits), (first, last)) in flagged {
            quality_out.push(json!({
                "tool": names.get(K_TOOL, t)?, "flags": contract::quality_words(bits),
                "fromMs": first, "toMs": last + HOUR_MS,
            }));
        }
        // Sessions active in the range, from the covering index alone: `last >= from` by the key,
        // `first <= to` and the signature from its value. Only the newest `limit` rows are read.
        let index = read.open_table(ACT_SESSION_BY_LAST)?;
        let sessions_t = read.open_table(ACT_SESSION)?;
        let limit = q.session_sample.unwrap_or(DEFAULT_SAMPLE).min(MAX_SAMPLE);
        let mut total = 0u64;
        let mut by_signature: BTreeMap<u32, u64> = BTreeMap::new();
        let mut sample = Vec::new();
        let from_h = (q.from_ms / HOUR_MS) as u32;
        for item in index.range(Key::new().u32(from_h).as_slice()..)?.rev() {
            let (k, v) = item?;
            let entry = v.value();
            if entry.len() != 20 {
                fail!("corrupt", "activity session index entry");
            }
            let (first, last, sig) = (db::be_u64(entry, 0), db::be_u64(entry, 8), db::be_u32(entry, 16));
            if first > q.to_ms || last < q.from_ms {
                continue;
            }
            total += 1;
            *by_signature.entry(sig).or_default() += 1;
            let t = db::be_u32(k.value(), 4);
            let key: [u8; 8] = k.value()[8..16].try_into().unwrap();
            if sample.len() < limit {
                let Some(v) = sessions_t.get(session_key(t, &key).as_slice())? else {
                    continue;
                };
                let row: SessionRow = db::unpack(v.value())?;
                let parts = row
                    .p
                    .iter()
                    .map(|p| {
                        Ok(json!({
                            "route": names.get(K_ROUTE, p.r)?, "modelRaw": names.get(K_MODEL, p.m)?,
                            "inputTokens": p.i, "outputTokens": p.o, "cacheCreationTokens": p.cw, "cacheReadTokens": p.cr,
                            "loggedCostUsd": p.ln.map(|x| x as f64 / 1e9), "costUsd": p.cn.map(|x| x as f64 / 1e9),
                            "fallbackCostUsd": p.fb.map(|x| x as f64 / 1e9),
                        }))
                    })
                    .collect::<Result<Vec<_>>>()?;
                sample.push(json!({
                    "tool": names.get(K_TOOL, t)?, "sessionKey": session_hex(&key), "computer": names.opt(K_COMPUTER, row.c)?,
                    "firstMs": row.f, "lastMs": row.l, "target": row.tg, "parts": parts,
                    "quality": contract::quality_words(row.q),
                }));
            }
        }
        let mut sig_out = Vec::with_capacity(by_signature.len());
        for (sig, count) in by_signature {
            let text = names.get(K_SIGNATURE, sig)?;
            let (t, rest) = text.split_once('|').unwrap_or(("0", ""));
            let id = |s: &str| s.parse::<u32>().unwrap_or(0);
            let mut pairs = Vec::new();
            for pair in rest.split(';').filter(|p| !p.is_empty()) {
                let (r, m) = pair.split_once(':').unwrap_or(("0", "0"));
                pairs.push(json!([names.get(K_ROUTE, id(r))?, names.get(K_MODEL, id(m))?]));
            }
            sig_out.push(json!({ "tool": names.get(K_TOOL, id(t))?, "pairs": pairs, "count": count }));
        }
        Ok(json!({
            "columns": ["bucketMs", "computer", "tool", "route", "modelRaw", "inputTokens", "outputTokens",
                        "cacheCreationTokens", "cacheReadTokens", "loggedCostNano", "costNano", "fallbackCostNano", "events",
                        "quality"],
            "computers": comps, "tools": tools, "routes": routes, "models": models, "rows": rows,
            "quality": quality_out,
            "sessions": { "total": total, "signatures": sig_out, "sample": sample, "truncated": total > sample.len() as u64 },
        }))
    })
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SourcesQuery {
    pub from_ms: u64,
    pub to_ms: u64,
}

/// Per computer and tool: stored span, last read (live and logged), state, backfill, and covered
/// spans; per computer: oldest and newest stored hour, last read and the gaps in the range (hours
/// after its history starts that no complete read of any of its tools covered).
pub fn sources(engine: &Engine, q: &SourcesQuery) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let dict = read.open_table(ACT_DICT)?;
        let mut names = Names::new(&dict);
        let source_t = read.open_table(ACT_SOURCE)?;
        let live_t = read.open_table(ACT_LIVE)?;
        let backfill_t = read.open_table(ACT_BACKFILL)?;
        let cover_t = read.open_table(ACT_COVER)?;
        let mut out = Vec::new();
        let mut per_computer: BTreeMap<String, (Option<u32>, Option<u32>, Option<u64>, Vec<(u32, u32)>)> = BTreeMap::new();
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        for item in source_t.iter()? {
            let (k, v) = item?;
            let (c, t) = (db::be_u32(k.value(), 0), db::be_u32(k.value(), 4));
            let src: SourceRow = db::unpack(v.value())?;
            let (cn, tn) = (names.get(K_COMPUTER, c)?, names.get(K_TOOL, t)?);
            seen.insert((cn.clone(), tn.clone()));
            let live: Option<ReadRow> = live_t
                .get(Key::new().bytes(cn.as_bytes()).u8(0).bytes(tn.as_bytes()).as_slice())?
                .map(|v| db::unpack(v.value()))
                .transpose()?;
            let backfill: Option<BackfillRow> = backfill_t
                .get(Key::new().u32(c).u32(t).as_slice())?
                .map(|v| db::unpack(v.value()))
                .transpose()?;
            let mut covers = Vec::new();
            let lo = Key::new().u32(c).u32(t).u32(0);
            let hi = Key::new().u32(c).u32(t).u32(u32::MAX);
            for item in cover_t.range(lo.as_slice()..=hi.as_slice())? {
                let (k, v) = item?;
                covers.push((db::be_u32(k.value(), 8), db::be_u32(v.value(), 0)));
            }
            let last_read = live.as_ref().map(|l| l.at).max(src.read.as_ref().map(|r| r.at));
            let entry = per_computer.entry(cn.clone()).or_default();
            entry.0 = match (entry.0, src.first_h) {
                (Some(a), Some(b)) => Some(a.min(b)),
                (a, b) => a.or(b),
            };
            entry.1 = match (entry.1, src.last_h) {
                (Some(a), Some(b)) => Some(a.max(b)),
                (a, b) => a.or(b),
            };
            entry.2 = entry.2.max(last_read);
            entry.3.extend(covers.iter().copied());
            let current = live.clone().or(src.read.clone());
            out.push(json!({
                "computer": cn, "tool": tn,
                "oldestMs": src.first_h.map(|h| h as u64 * HOUR_MS), "newestMs": src.last_h.map(|h| h as u64 * HOUR_MS),
                "lastReadMs": last_read, "lastCompleteMs": src.last_complete,
                "state": current.as_ref().map(|r| r.state.clone()), "cause": current.as_ref().and_then(|r| r.cause.clone()),
                "complete": current.as_ref().map(|r| r.complete), "counts": current.as_ref().and_then(|r| r.counts.clone()),
                "roots": current.as_ref().map(|r| r.roots.clone()).unwrap_or_default(),
                "badSinceMs": src.bad_since, "backfill": backfill,
                "futureDated": current.as_ref().and_then(|r| r.counts.as_ref()).and_then(|c| c.future_dated),
                "covered": covers.iter().map(|(a, b)| json!([*a as u64 * HOUR_MS, *b as u64 * HOUR_MS])).collect::<Vec<_>>(),
            }));
        }
        // Sources only the live table knows (read, never stored a cell or a logged read yet).
        for item in live_t.iter()? {
            let (k, v) = item?;
            let raw = k.value();
            let Some(split) = raw.iter().position(|b| *b == 0) else { continue };
            let (cn, tn) = (String::from_utf8_lossy(&raw[..split]).into_owned(), String::from_utf8_lossy(&raw[split + 1..]).into_owned());
            if seen.contains(&(cn.clone(), tn.clone())) {
                continue;
            }
            let live: ReadRow = db::unpack(v.value())?;
            let entry = per_computer.entry(cn.clone()).or_default();
            entry.2 = entry.2.max(Some(live.at));
            out.push(json!({
                "computer": cn, "tool": tn, "oldestMs": null, "newestMs": null, "lastReadMs": live.at,
                "lastCompleteMs": null, "state": live.state, "cause": live.cause, "complete": live.complete,
                "futureDated": live.counts.as_ref().and_then(|c| c.future_dated),
                "counts": live.counts, "roots": live.roots, "badSinceMs": null, "backfill": null, "covered": [],
            }));
        }
        let (from_h, to_h) = ((q.from_ms / HOUR_MS) as u32, (q.to_ms / HOUR_MS) as u32 + 1);
        let mut computers = Vec::new();
        for (cn, (first, last, last_read, mut covers)) in per_computer {
            covers.sort();
            let mut merged: Vec<(u32, u32)> = Vec::new();
            for (a, b) in covers {
                match merged.last_mut() {
                    Some(m) if a <= m.1 => m.1 = m.1.max(b),
                    _ => merged.push((a, b)),
                }
            }
            // History starts at the oldest stored hour or covered span; nothing before it is a gap.
            let start = [first, merged.first().map(|m| m.0)].into_iter().flatten().min();
            let mut gaps = Vec::new();
            if let Some(start) = start {
                let mut at = from_h.max(start);
                for (a, b) in &merged {
                    if *b <= at {
                        continue;
                    }
                    if *a > at {
                        gaps.push((at, (*a).min(to_h)));
                    }
                    at = at.max(*b);
                    if at >= to_h {
                        break;
                    }
                }
                if at < to_h {
                    gaps.push((at, to_h));
                }
                gaps.retain(|(a, b)| a < b);
            }
            computers.push(json!({
                "computer": cn,
                "oldestMs": start.map(|h| h as u64 * HOUR_MS),
                "newestMs": last.map(|h| h as u64 * HOUR_MS),
                "lastReadMs": last_read,
                "covered": !merged.is_empty(),
                "gaps": gaps.iter().map(|(a, b)| json!([*a as u64 * HOUR_MS, *b as u64 * HOUR_MS])).collect::<Vec<_>>(),
            }));
        }
        Ok(json!({ "sources": out, "computers": computers }))
    })
}

/// The oldest stored hour (ms), or null: bounds "All" and the picker.
pub fn oldest(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let table = read.open_table(ACT_SOURCE)?;
        let mut oldest: Option<u32> = None;
        for item in table.iter()? {
            let (_, v) = item?;
            let src: SourceRow = db::unpack(v.value())?;
            if let Some(h) = src.first_h {
                oldest = Some(oldest.map_or(h, |o| o.min(h)));
            }
        }
        Ok(json!(oldest.map(|h| h as u64 * HOUR_MS)))
    })
}

/// Row counts of the activity tables (tests and status).
pub fn counts(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let mut cells = 0u64;
        let mut bytes = 0u64;
        for item in read.open_table(ACT_HOUR)?.iter()? {
            let (_, v) = item?;
            bytes += v.value().len() as u64;
            cells += decode_cells(v.value())?.len() as u64;
        }
        Ok(json!({
            "dayValues": read.open_table(ACT_HOUR)?.len()?,
            "cells": cells,
            "cellBytes": bytes,
            "sessions": read.open_table(ACT_SESSION)?.len()?,
            "covers": read.open_table(ACT_COVER)?.len()?,
            "scans": read.open_table(ACT_SCAN)?.len()?,
            "closedDays": read.open_table(ACT_DAY_CLOSED)?.len()?,
            "sources": read.open_table(ACT_SOURCE)?.len()?,
        }))
    })
}

/// Every stored cell, as event text, for tests and exports.
pub fn dump_cells(engine: &Engine) -> Result<Vec<(String, String, CellEv)>> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let dict = read.open_table(ACT_DICT)?;
        let mut names = Names::new(&dict);
        let mut out = Vec::new();
        for item in read.open_table(ACT_HOUR)?.iter()? {
            let (k, v) = item?;
            let day = db::be_u32(k.value(), 0);
            let (c, t) = (db::be_u32(k.value(), 4), db::be_u32(k.value(), 8));
            let (cn, tn) = (names.get(K_COMPUTER, c)?, names.get(K_TOOL, t)?);
            for cell in decode_cells(v.value())? {
                let mut text = |kind: u8, id: u32| -> Result<Option<String>> {
                    if id == 0 && kind != K_ROUTE {
                        Ok(None)
                    } else {
                        names.get(kind, id).map(Some)
                    }
                };
                out.push((cn.clone(), tn.clone(), ev_from_cell(&mut text, day, &cell)?));
            }
        }
        out.sort_by(|a, b| {
            (
                a.0.clone(),
                a.1.clone(),
                a.2.h,
                a.2.s.clone(),
                a.2.r.clone(),
                a.2.m.clone(),
            )
                .cmp(&(
                    b.0.clone(),
                    b.1.clone(),
                    b.2.h,
                    b.2.s.clone(),
                    b.2.r.clone(),
                    b.2.m.clone(),
                ))
        });
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_round_trip() {
        let cells = vec![
            Cell {
                hour: 3,
                session: [1, 2, 3, 4, 5, 6, 7, 8],
                route: 1,
                model: 2,
                effort: 0,
                account: 0,
                i: 5,
                o: 6,
                cw: 0,
                cr: 900,
                reasoning: None,
                logged: Some(0),
                cost: Some(1234),
                fallback: None,
                events: Some(3),
                quality: 0,
            },
            Cell {
                hour: 0,
                session: NO_SESSION,
                route: 1,
                model: 3,
                effort: 2,
                account: 7,
                i: 0,
                o: 1,
                cw: 2,
                cr: 3,
                reasoning: Some(1),
                logged: None,
                cost: None,
                fallback: Some(9),
                events: None,
                quality: 1,
            },
        ];
        let bytes = encode_cells(&cells);
        let mut back = decode_cells(&bytes).unwrap();
        back.sort_by_key(|c| c.id());
        let mut want = cells.clone();
        want.sort_by_key(|c| c.id());
        assert_eq!(back, want);
        assert!(decode_cells(&bytes[..bytes.len() - 1]).is_err());
    }

    #[test]
    fn version_one_values_read_as_quality_zero() {
        let cell = Cell {
            hour: 1,
            session: NO_SESSION,
            route: 0,
            model: 1,
            effort: 0,
            account: 0,
            i: 1,
            o: 2,
            cw: 0,
            cr: 0,
            reasoning: None,
            logged: None,
            cost: None,
            fallback: None,
            events: None,
            quality: 0,
        };
        let mut v1 = encode_cells(std::slice::from_ref(&cell));
        v1[0] = 1;
        v1.pop(); // version 1 has no quality varint
        assert_eq!(decode_cells(&v1).unwrap(), vec![cell]);
    }
}
