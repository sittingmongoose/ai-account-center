//! Usage readings (docs/store.md phase 1): the sampler's observations become `usage.sampled`
//! (something changed) or `usage.seen` (unchanged, at most once per five-minute bucket), and the
//! usage projector keeps readings as spans, account states, reset periods and rollups.

use crate::db::{self, Bytes, Index, Key};
use crate::engine::{Batch, DAY_MS, Engine, LogId, Projector, begin, civil_from_days, now_ms};
use crate::error::Result;
use crate::event::Event;
use crate::fail;
use crate::log::Finding;
use crate::msgpack;
use crate::pack;
use redb::{ReadableDatabase, ReadableTable, ReadableTableMetadata, WriteTransaction};
use serde::{Deserialize, Serialize};
use serde_json::{Value as Json, json};

pub const HOUR_MS: u64 = 3_600_000;
const BUCKET_MS: u64 = 5 * 60_000;
/// Readings further apart than this are a gap, not coverage.
const COVER_GAP_MS: u64 = 10 * 60_000;
const RESET_SHIFT_MS: i64 = 10 * 60_000;
const RESET_DROP_POINTS: f64 = 1.0;
const MAX_WINDOWS: usize = 256;
/// A day closes this long after it ends, so late samples of that day still count.
const CLOSE_GRACE_MS: u64 = 2 * HOUR_MS;
const PRUNE_BATCH: usize = 10_000;
const DAY_CLOSED_SERIES_PER_EVENT: usize = 128;
const STATUSES: [&str; 5] = ["ok", "cached", "unavailable", "error", "needs_sign_in"];
const PLATFORMS: [&str; 3] = ["ubuntu", "mac", "windows"];

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct AccountObs {
    pub identity: String,
    pub provider: String,
    pub account_id: String,
    pub plan: Option<String>,
    pub platform: String,
    pub source: String,
    pub status: String,
    pub is_active: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct WindowObs {
    pub identity: String,
    pub key: String,
    pub label: String,
    pub kind: Option<String>,
    pub unit: Option<String>,
    pub used_pct: Option<f64>,
    pub remaining_pct: Option<f64>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub reset_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
    pub window_minutes: Option<f64>,
    pub unlimited: Option<bool>,
    pub enabled: Option<bool>,
    pub cached_sampled_at_ms: Option<i64>,
    pub pool_id: Option<String>,
    pub pool_id_source: Option<String>,
    pub pool_label: Option<String>,
    pub model_ids: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Sampled {
    pub account: AccountObs,
    pub sampled_at_ms: i64,
    pub observed_at_ms: i64,
    pub windows: Vec<WindowObs>,
    pub import_ref: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct Seen {
    pub account_identity: String,
    pub sampled_at_ms: i64,
    pub observed_at_ms: i64,
    pub import_ref: Option<String>,
}

/// One `usage.sampled` event: every changed account of one sampling tick.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SampledBatch {
    pub observations: Vec<Sampled>,
}

/// One `usage.seen` event: every unchanged account of one sampling tick.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default, deny_unknown_fields)]
pub struct SeenBatch {
    pub accounts: Vec<Seen>,
}

/// At most this many accounts share one event.
const ACCOUNTS_PER_EVENT: usize = 64;

/// A persisted string: bounded, no control characters, nothing shaped like a credential.
fn text_ok(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && !s.chars().any(|c| c.is_control()) && !crate::secret::looks_secret(s)
}

fn opt_text_ok(s: &Option<String>, max: usize) -> bool {
    s.as_ref().map(|v| text_ok(v, max)).unwrap_or(true)
}

impl Sampled {
    pub fn validate(&self) -> Result<()> {
        let a = &self.account;
        let hex = a.identity.len() == 64
            && a.identity
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase());
        if !hex
            || !text_ok(&a.identity, 64)
            || !text_ok(&a.provider, 64)
            || !text_ok(&a.account_id, 128)
            || !opt_text_ok(&a.plan, 80)
            || !PLATFORMS.contains(&a.platform.as_str())
            || !STATUSES.contains(&a.status.as_str())
            || !text_ok(&a.source, 120)
            || self.sampled_at_ms < 0
            || self.observed_at_ms < 0
            || self.windows.len() > MAX_WINDOWS
            || !opt_text_ok(&self.import_ref, 512)
        {
            fail!("invalid", "observation fails validation");
        }
        if !self.windows.iter().all(window_ok) {
            fail!("invalid", "window fails validation");
        }
        Ok(())
    }

    /// The fingerprints of this observation's series.
    fn fingerprints(&self) -> Vec<String> {
        self.windows
            .iter()
            .map(|w| series_fingerprint(&self.account.identity, &w.identity))
            .collect()
    }

    /// What decides "unchanged": the windows (every field, in order) and the account's source,
    /// platform and active flag, as the JSON history compared them.
    fn digest(&self) -> Result<String> {
        let basis = json!({
            "windows": serde_json::to_value(&self.windows)?,
            "source": self.account.source,
            "platform": self.account.platform,
            "active": self.account.is_active,
        });
        Ok(crate::fsx::sha256_hex(&msgpack::encode(&msgpack::from_json(&basis)?)?))
    }
}

/// A series named without its text: the first 16 bytes of `sha256(account identity, 0, window
/// identity)` as hex. Refusal records carry this, never the refused strings.
pub fn series_fingerprint(account_identity: &str, window_identity: &str) -> String {
    let mut basis = account_identity.as_bytes().to_vec();
    basis.push(0);
    basis.extend_from_slice(window_identity.as_bytes());
    crate::fsx::sha256_hex(&basis)[..32].to_string()
}

/// Every persisted string of a window passes the text checks.
fn window_ok(w: &WindowObs) -> bool {
    let models_ok = w
        .model_ids
        .as_ref()
        .map(|m| m.len() <= 64 && m.iter().all(|x| text_ok(x, 160)))
        .unwrap_or(true);
    text_ok(&w.identity, 512)
        && text_ok(&w.key, 64)
        && text_ok(&w.label, 80)
        && opt_text_ok(&w.kind, 32)
        && opt_text_ok(&w.unit, 40)
        && opt_text_ok(&w.pool_id, 160)
        && opt_text_ok(&w.pool_id_source, 64)
        && opt_text_ok(&w.pool_label, 80)
        && models_ok
}

/// The account and window text a closed-day summary would persist passes the same checks as an
/// observation's (a summary carries no source or status).
fn summary_text_ok(a: &AccountObs, w: &WindowObs) -> bool {
    a.identity.len() == 64
        && a.identity
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        && text_ok(&a.provider, 64)
        && text_ok(&a.account_id, 128)
        && opt_text_ok(&a.plan, 80)
        && PLATFORMS.contains(&a.platform.as_str())
        && window_ok(w)
}

fn refused_key(day: u32, fingerprint: &str) -> Option<Vec<u8>> {
    let bytes: Vec<u8> = (0..fingerprint.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(fingerprint.get(i..i + 2)?, 16).ok())
        .collect();
    (bytes.len() == 16).then(|| Key::new().u32(day).bytes(&bytes).0)
}

/// Records that a refused observation touched these series on `day`, and withdraws what older
/// closed-day summaries wrote for them that depends on that day.
fn note_refused(txn: &WriteTransaction, day: u32, fingerprints: &[String]) -> Result<Vec<String>> {
    let mut withdrawn = Vec::new();
    for fp in fingerprints {
        let Some(key) = refused_key(day, fp) else { continue };
        put_index(txn, db::REFUSED, &key)?;
        put_index(
            txn,
            db::REFUSED_BY_SERIES,
            Key::new().bytes(&key[4..]).u32(day).as_slice(),
        )?;
        let mut rows: Vec<(Bytes, Vec<u8>, Vec<u8>)> = Vec::new();
        if let Some(value) = get(txn, db::SUMMARY_UNMARKED, &key)? {
            rows.push((db::SUMMARY_UNMARKED, key.clone(), value));
        }
        // Unbounded rows of this series whose last day is on or after the refusal.
        let fp_bytes = &key[4..];
        let lo = Key::new().bytes(fp_bytes).u32(day);
        let hi = Key::new().bytes(fp_bytes).u32(u32::MAX);
        for item in txn
            .open_table(db::SUMMARY_UNBOUNDED)?
            .range(lo.as_slice()..=hi.as_slice())?
        {
            let (k, v) = item?;
            rows.push((db::SUMMARY_UNBOUNDED, k.value().to_vec(), v.value().to_vec()));
        }
        if rows.is_empty() {
            continue;
        }
        for (table, row_key, value) in rows {
            withdraw(txn, &value)?;
            txn.open_table(table)?.remove(row_key.as_slice())?;
        }
        withdrawn.push(fp.clone());
    }
    Ok(withdrawn)
}

/// Removes what a provenance row lists: a day's rollups or a reset period.
fn withdraw(txn: &WriteTransaction, value: &[u8]) -> Result<()> {
    if value.len() < 8 {
        return Ok(());
    }
    let (a, w) = (db::be_u32(value, 0), db::be_u32(value, 4));
    let mut at = 8;
    while at + 9 <= value.len() {
        let n = db::be_u64(value, at + 1);
        match value[at] {
            1 => {
                let rollup = Key::new().u32(a).u32(w).u32(n.min(u32::MAX as u64) as u32);
                txn.open_table(db::ROLLUP_HOURS)?.remove(rollup.as_slice())?;
                txn.open_table(db::ROLLUP_DAY)?.remove(rollup.as_slice())?;
            }
            2 => {
                txn.open_table(db::PERIOD)?
                    .remove(Key::new().u32(a).u32(w).u64(n).as_slice())?;
            }
            _ => {}
        }
        at += 9;
    }
    Ok(())
}

/// Adds an entry (`1`: a day's rollups, `2`: a period start) to a provenance row.
fn remember(txn: &WriteTransaction, table: Bytes, key: &[u8], (a, w): (u32, u32), entry: (u8, u64)) -> Result<()> {
    let mut value = get(txn, table, key)?.filter(|v| v.len() >= 8).unwrap_or_else(|| {
        let mut v = a.to_be_bytes().to_vec();
        v.extend_from_slice(&w.to_be_bytes());
        v
    });
    let known = (8..value.len())
        .step_by(9)
        .any(|at| at + 9 <= value.len() && value[at] == entry.0 && db::be_u64(&value, at + 1) == entry.1);
    if !known {
        value.push(entry.0);
        value.extend_from_slice(&entry.1.to_be_bytes());
    }
    put(txn, table, key, &value)
}

/// The UTC days an older summary's numbers for a series can depend on.
enum Reach {
    /// From the first day through the last.
    Days(u32, u32),
    /// Every day up to the last: a window of unknown length with no earlier surviving reading.
    Back(u32),
}

/// What numbers covering `[first_ms, last_ms]` depend on: the days they cover and, before them,
/// the reading that came last before `first_ms`, since it decides the first consumption and
/// whether a reset happened. That reading is found as the series' previous surviving reading
/// when the readings still replay, whatever the window. Without one: one window length back when
/// the length and the provider's reset time are both known (a longer silence passes the reset
/// time, which resets the window whatever that reading was); otherwise every earlier day, since
/// without a reset time nothing ends a period by time.
fn reach(
    txn: &WriteTransaction,
    ids: Option<(u32, u32)>,
    first_ms: u64,
    last_ms: i64,
    window_minutes: Option<f64>,
    reset_known: bool,
) -> Result<Reach> {
    let last = day_of(last_ms.max(first_ms as i64) as u64);
    if let (Some((a, w)), Some(before)) = (ids, first_ms.checked_sub(1)) {
        let readings = txn.open_table(db::READING_DAY)?;
        let latest = txn.open_table(db::LATEST)?;
        let open = open_spans_for(&latest, [(a, w)])?;
        if let Some((_, row)) = span_at(&readings, &open, (a, w), before)? {
            return Ok(Reach::Days(day_of(row.last_seen.min(before)), last));
        }
    }
    // A silence longer than the window resets it only through the provider's reset time.
    if reset_known && let Some(m) = window_minutes.filter(|m| m.is_finite() && *m > 0.0) {
        let lookback = ((m * 60_000.0) / DAY_MS as f64).ceil().clamp(1.0, 366.0) as u32;
        return Ok(Reach::Days(day_of(first_ms).saturating_sub(lookback), last));
    }
    Ok(Reach::Back(last))
}

/// Whether a refused observation of this series falls on a day the numbers depend on.
fn reach_refused(txn: &WriteTransaction, fingerprint: &str, reach: &Reach) -> Result<bool> {
    match *reach {
        Reach::Days(first, last) => {
            for d in first..=last.min(first.saturating_add(3660)) {
                if let Some(key) = refused_key(d, fingerprint)
                    && has_index(txn, db::REFUSED, &key)?
                {
                    return Ok(true);
                }
            }
            Ok(last > first.saturating_add(3660))
        }
        Reach::Back(last) => {
            let Some(probe) = refused_key(0, fingerprint) else {
                return Ok(false);
            };
            // One seek: the series' first refused day, if it is not after the last day.
            let lo = Key::new().bytes(&probe[4..]).u32(0);
            let hi = Key::new().bytes(&probe[4..]).u32(last);
            Ok(txn
                .open_table(db::REFUSED_BY_SERIES)?
                .range(lo.as_slice()..=hi.as_slice())?
                .next()
                .is_some())
        }
    }
}

/// Records an applied entry under every day of its reach, so a refusal there withdraws it.
fn remember_reach(
    txn: &WriteTransaction,
    fingerprint: &str,
    reach: &Reach,
    ids: (u32, u32),
    entry: (u8, u64),
) -> Result<()> {
    match *reach {
        Reach::Days(first, last) => {
            for d in first..=last.min(first.saturating_add(3660)) {
                if let Some(key) = refused_key(d, fingerprint) {
                    remember(txn, db::SUMMARY_UNMARKED, &key, ids, entry)?;
                }
            }
        }
        Reach::Back(last) => {
            if let Some(key) = refused_key(0, fingerprint) {
                let row = Key::new().bytes(&key[4..]).u32(last);
                remember(txn, db::SUMMARY_UNBOUNDED, row.as_slice(), ids, entry)?;
            }
        }
    }
    Ok(())
}

fn summary_finding(event: &Event, why: &str) -> Finding {
    Finding {
        kind: "summary_quarantined".into(),
        detail: format!("a closed-day summary in this event {why}; its rollups were left out of the usage tables"),
        start_seq: Some(event.sequence_id),
        end_seq: Some(event.sequence_id),
        refused: None,
    }
}

// ----------------------------------------------------------------------------- stored values

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Vals {
    pub cached: bool,
    pub unlimited: Option<bool>,
    pub enabled: Option<bool>,
    pub used_pct: Option<f64>,
    pub remaining_pct: Option<f64>,
    pub used: Option<f64>,
    pub limit: Option<f64>,
    pub remaining: Option<f64>,
    pub reset_at: Option<i64>,
    pub expires_at: Option<i64>,
    pub window_minutes: Option<f64>,
    pub cached_sampled_at: Option<i64>,
}

impl Vals {
    fn from_obs(w: &WindowObs) -> Vals {
        Vals {
            cached: w.cached_sampled_at_ms.is_some(),
            unlimited: w.unlimited,
            enabled: w.enabled,
            used_pct: w.used_pct,
            remaining_pct: w.remaining_pct,
            used: w.used,
            limit: w.limit,
            remaining: w.remaining,
            reset_at: w.reset_at_ms,
            expires_at: w.expires_at_ms,
            window_minutes: w.window_minutes,
            cached_sampled_at: w.cached_sampled_at_ms,
        }
    }

    fn same(&self, other: &Vals) -> bool {
        let f = |a: Option<f64>, b: Option<f64>| a.map(f64::to_bits) == b.map(f64::to_bits);
        self.cached == other.cached
            && self.unlimited == other.unlimited
            && self.enabled == other.enabled
            && f(self.used_pct, other.used_pct)
            && f(self.remaining_pct, other.remaining_pct)
            && f(self.used, other.used)
            && f(self.limit, other.limit)
            && f(self.remaining, other.remaining)
            && self.reset_at == other.reset_at
            && self.expires_at == other.expires_at
            && f(self.window_minutes, other.window_minutes)
            && self.cached_sampled_at == other.cached_sampled_at
    }
}

/// A reading span: the same values observed from its start time through `last_seen`. The open
/// span of each series lives only in `latest`; it enters its `reading_day` value when it closes,
/// so an unchanged sighting rewrites one small row and a change rewrites one day value.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Row {
    pub vals: Vals,
    pub last_seen: u64,
}

const ROW_HEADER: usize = 12;

impl Row {
    pub fn encode(&self) -> Vec<u8> {
        let v = &self.vals;
        let mut out = Vec::with_capacity(64);
        out.push(1);
        let mut flags = 0u8;
        if v.cached {
            flags |= 1;
        }
        if let Some(u) = v.unlimited {
            flags |= 2 | if u { 4 } else { 0 };
        }
        if let Some(e) = v.enabled {
            flags |= 8 | if e { 16 } else { 0 };
        }
        out.push(flags);
        let floats = [v.used_pct, v.remaining_pct, v.used, v.limit, v.remaining];
        let times = [v.reset_at, v.expires_at];
        let mut mask = 0u16;
        for (i, f) in floats.iter().enumerate() {
            if f.is_some() {
                mask |= 1 << i;
            }
        }
        for (i, t) in times.iter().enumerate() {
            if t.is_some() {
                mask |= 1 << (5 + i);
            }
        }
        if v.window_minutes.is_some() {
            mask |= 1 << 7;
        }
        if v.cached_sampled_at.is_some() {
            mask |= 1 << 8;
        }
        out.extend_from_slice(&mask.to_be_bytes());
        out.extend_from_slice(&self.last_seen.to_be_bytes());
        for f in floats.iter().flatten() {
            out.extend_from_slice(&f.to_be_bytes());
        }
        for t in times.iter().flatten() {
            out.extend_from_slice(&t.to_be_bytes());
        }
        if let Some(m) = v.window_minutes {
            out.extend_from_slice(&m.to_be_bytes());
        }
        if let Some(c) = v.cached_sampled_at {
            out.extend_from_slice(&c.to_be_bytes());
        }
        out
    }

    pub fn decode(b: &[u8]) -> Result<Row> {
        if b.len() < ROW_HEADER || b[0] != 1 {
            fail!("corrupt", "reading value");
        }
        let flags = b[1];
        let mask = u16::from_be_bytes([b[2], b[3]]);
        let mut at = ROW_HEADER;
        let mut next8 = || -> Result<[u8; 8]> {
            if at + 8 > b.len() {
                fail!("corrupt", "reading value is short");
            }
            let v: [u8; 8] = b[at..at + 8].try_into().unwrap();
            at += 8;
            Ok(v)
        };
        let mut floats = [None; 5];
        for (i, slot) in floats.iter_mut().enumerate() {
            if mask & (1 << i) != 0 {
                *slot = Some(f64::from_be_bytes(next8()?));
            }
        }
        let mut times = [None; 2];
        for (i, slot) in times.iter_mut().enumerate() {
            if mask & (1 << (5 + i)) != 0 {
                *slot = Some(i64::from_be_bytes(next8()?));
            }
        }
        let window_minutes = if mask & (1 << 7) != 0 {
            Some(f64::from_be_bytes(next8()?))
        } else {
            None
        };
        let cached_sampled_at = if mask & (1 << 8) != 0 {
            Some(i64::from_be_bytes(next8()?))
        } else {
            None
        };
        Ok(Row {
            vals: Vals {
                cached: flags & 1 != 0,
                unlimited: if flags & 2 != 0 { Some(flags & 4 != 0) } else { None },
                enabled: if flags & 8 != 0 { Some(flags & 16 != 0) } else { None },
                used_pct: floats[0],
                remaining_pct: floats[1],
                used: floats[2],
                limit: floats[3],
                remaining: floats[4],
                reset_at: times[0],
                expires_at: times[1],
                window_minutes,
                cached_sampled_at,
            },
            last_seen: db::be_u64(b, 4),
        })
    }
}

/// Hourly and daily rollup (docs/store.md 3.1).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Rollup {
    pub consumed: f64,
    pub credits_spent: f64,
    pub credits_added: f64,
    pub first_used: Option<f64>,
    pub last_used: Option<f64>,
    pub min_used: Option<f64>,
    pub max_used: Option<f64>,
    pub covered_ms: u64,
    pub rows: u32,
    pub resets: u32,
    pub late_rows: u32,
    pub last_reset_ms: Option<i64>,
}

impl Rollup {
    /// Version 2: a presence mask, then only the present fields. Percentages keep f32 precision
    /// (rollups are summaries shown to two decimals), whole ones in a byte or two; credits keep
    /// f64 precision. Self-delimiting, so a day's hours follow each other in one value.
    pub fn encode_into(&self, out: &mut Vec<u8>) {
        let mut mask = 0u64;
        let bits = [
            self.consumed != 0.0,
            self.credits_spent != 0.0,
            self.credits_added != 0.0,
            self.first_used.is_some(),
            self.covered_ms != 0,
            self.rows != 0,
            self.resets != 0,
            self.late_rows != 0,
            self.last_reset_ms.is_some(),
        ];
        for (i, on) in bits.iter().enumerate() {
            if *on {
                mask |= 1 << i;
            }
        }
        pack::put_varint(out, mask);
        if bits[0] {
            pack::put_small_f32(out, self.consumed);
        }
        if bits[1] {
            pack::put_plain_float(out, self.credits_spent);
        }
        if bits[2] {
            pack::put_plain_float(out, self.credits_added);
        }
        if bits[3] {
            for f in [self.first_used, self.last_used, self.min_used, self.max_used] {
                pack::put_small_f32(out, f.unwrap_or(0.0));
            }
        }
        if bits[4] {
            pack::put_varint(out, self.covered_ms.min(u32::MAX as u64));
        }
        for (on, n) in [(bits[5], self.rows), (bits[6], self.resets), (bits[7], self.late_rows)] {
            if on {
                pack::put_varint(out, n as u64);
            }
        }
        if let Some(t) = self.last_reset_ms {
            pack::put_svarint(out, t);
        }
    }

    pub fn decode_from(c: &mut pack::Cursor<'_>) -> Result<Rollup> {
        let mask = c.varint()?;
        if mask >> 9 != 0 {
            fail!("corrupt", "rollup value");
        }
        let on = |i: u64| mask & (1 << i) != 0;
        let count = |c: &mut pack::Cursor<'_>| -> Result<u32> {
            match u32::try_from(c.varint()?) {
                Ok(n) => Ok(n),
                Err(_) => fail!("corrupt", "rollup count"),
            }
        };
        let mut r = Rollup::default();
        if on(0) {
            r.consumed = pack::read_small_f32(c)?;
        }
        if on(1) {
            r.credits_spent = pack::read_plain_float(c)?;
        }
        if on(2) {
            r.credits_added = pack::read_plain_float(c)?;
        }
        if on(3) {
            r.first_used = Some(pack::read_small_f32(c)?);
            r.last_used = Some(pack::read_small_f32(c)?);
            r.min_used = Some(pack::read_small_f32(c)?);
            r.max_used = Some(pack::read_small_f32(c)?);
        }
        if on(4) {
            r.covered_ms = c.varint()?;
        }
        if on(5) {
            r.rows = count(c)?;
        }
        if on(6) {
            r.resets = count(c)?;
        }
        if on(7) {
            r.late_rows = count(c)?;
        }
        if on(8) {
            r.last_reset_ms = Some(c.svarint()?);
        }
        Ok(r)
    }

    /// One `rollup_day` value.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32);
        self.encode_into(&mut out);
        out
    }

    pub fn decode(b: &[u8]) -> Result<Rollup> {
        let mut c = pack::Cursor::new(b);
        let rollup = Rollup::decode_from(&mut c)?;
        c.finish()?;
        Ok(rollup)
    }

    fn note_used(&mut self, used: f64) {
        if self.first_used.is_none() {
            self.first_used = Some(used);
        }
        self.last_used = Some(used);
        self.min_used = Some(self.min_used.map(|m| m.min(used)).unwrap_or(used));
        self.max_used = Some(self.max_used.map(|m| m.max(used)).unwrap_or(used));
    }
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AccountMeta {
    pub identity: String,
    pub provider: String,
    pub account_id: String,
    pub plan: Option<String>,
    pub platform: String,
    pub first_ms: i64,
    pub last_ms: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct WindowMeta {
    pub identity: String,
    pub key: String,
    pub kind: Option<String>,
    pub unit: Option<String>,
    pub pool_id: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct SeriesMeta {
    pub first_ms: i64,
    pub last_ms: i64,
    pub rows: u64,
    pub label: String,
    pub window_minutes: Option<f64>,
    pub pool_id_source: Option<String>,
    pub pool_label: Option<String>,
    pub model_ids: Option<Vec<String>>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AccountLatest {
    pub state_t: i64,
    pub sampled_ms: i64,
    pub digest: String,
    pub status: String,
    pub window_ids: Vec<u32>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct AccountState {
    pub status: String,
    pub is_active: bool,
    pub source: String,
    pub platform: String,
    pub observed_ms: i64,
    pub last_seen: i64,
    pub last_observed: i64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(default)]
pub struct Period {
    pub reset_at_ms: Option<i64>,
    pub end_ms: i64,
    pub start_used: f64,
    pub end_used: f64,
    pub max_used: f64,
    pub consumed: f64,
    pub hit_limit_ms: Option<i64>,
    pub rows: u32,
    #[serde(rename = "final")]
    pub is_final: bool,
    pub emitted: bool,
}

// ----------------------------------------------------------------------------- table helpers

fn get(txn: &WriteTransaction, def: Bytes, key: &[u8]) -> Result<Option<Vec<u8>>> {
    let table = txn.open_table(def)?;
    let value = table.get(key)?.map(|v| v.value().to_vec());
    Ok(value)
}

fn put(txn: &WriteTransaction, def: Bytes, key: &[u8], value: &[u8]) -> Result<()> {
    txn.open_table(def)?.insert(key, value)?;
    Ok(())
}

fn put_index(txn: &WriteTransaction, def: Index, key: &[u8]) -> Result<()> {
    txn.open_table(def)?.insert(key, ())?;
    Ok(())
}

fn has_index(txn: &WriteTransaction, def: Index, key: &[u8]) -> Result<bool> {
    Ok(txn.open_table(def)?.get(key)?.is_some())
}

fn hour_of(t: u64) -> u32 {
    (t / HOUR_MS) as u32
}

fn day_of(t: u64) -> u32 {
    (t / DAY_MS) as u32
}

fn day_closed(txn: &WriteTransaction, t: u64) -> Result<bool> {
    has_index(txn, db::DAY_CLOSED, Key::new().u32(day_of(t)).as_slice())
}

/// Applies `f` to the hour and day rollups holding `t`, unless that day is closed.
fn with_rollups(txn: &WriteTransaction, a: u32, w: u32, t: u64, f: impl Fn(&mut Rollup)) -> Result<bool> {
    if day_closed(txn, t)? {
        return Ok(false);
    }
    let key = Key::new().u32(a).u32(w).u32(day_of(t));
    let mut hours = match get(txn, db::ROLLUP_HOURS, key.as_slice())? {
        Some(b) => pack::decode_hours(&b)?,
        None => Vec::new(),
    };
    let hour = (hour_of(t) % 24) as u8;
    let slot = match hours.binary_search_by_key(&hour, |(h, _)| *h) {
        Ok(i) => i,
        Err(i) => {
            hours.insert(i, (hour, Rollup::default()));
            i
        }
    };
    f(&mut hours[slot].1);
    put(txn, db::ROLLUP_HOURS, key.as_slice(), &pack::encode_hours(&hours))?;
    let mut rollup = match get(txn, db::ROLLUP_DAY, key.as_slice())? {
        Some(b) => Rollup::decode(&b)?,
        None => Rollup::default(),
    };
    f(&mut rollup);
    put(txn, db::ROLLUP_DAY, key.as_slice(), &rollup.encode())?;
    Ok(true)
}

fn note_late(txn: &WriteTransaction, a: u32, w: u32, t: u64) -> Result<()> {
    // A closed day's rollups are final; the late reading is only counted.
    let key = Key::new().u32(a).u32(w).u32(day_of(t));
    if let Some(b) = get(txn, db::ROLLUP_DAY, key.as_slice())? {
        let mut rollup = Rollup::decode(&b)?;
        rollup.late_rows += 1;
        put(txn, db::ROLLUP_DAY, key.as_slice(), &rollup.encode())?;
    }
    Ok(())
}

/// Adds the observed span `[from, to]` to coverage when the readings are close enough.
fn cover(txn: &WriteTransaction, a: u32, w: u32, from: u64, to: u64) -> Result<()> {
    if to <= from || to - from > COVER_GAP_MS {
        return Ok(());
    }
    let mut start = from;
    while start < to {
        let hour_end = (start / HOUR_MS + 1) * HOUR_MS;
        let end = hour_end.min(to);
        let span = end - start;
        if span > 0 {
            with_rollups(txn, a, w, end - 1, |r| r.covered_ms += span)?;
        }
        start = end;
    }
    Ok(())
}

fn upcoming(reset: Option<i64>, at: i64) -> bool {
    reset.map(|r| r > at).unwrap_or(false)
}

/// Whether a reset happened between two readings (the rules of agent-usage-history.ts).
pub fn is_reset(prev_t: i64, prev: &Vals, t: i64, now: &Vals) -> bool {
    let (Some(up), Some(u)) = (prev.used_pct, now.used_pct) else {
        return false;
    };
    if [prev.reset_at, now.reset_at]
        .iter()
        .flatten()
        .any(|&r| prev_t < r && r <= t)
    {
        return true;
    }
    let same_upcoming = upcoming(prev.reset_at, prev_t) && upcoming(now.reset_at, t);
    if same_upcoming && (prev.reset_at.unwrap() - now.reset_at.unwrap()).abs() > RESET_SHIFT_MS {
        return true;
    }
    up - u > if same_upcoming { RESET_DROP_POINTS } else { 0.0 }
}

// ----------------------------------------------------------------------------- the projector

pub struct UsageProjector;

/// Bump when projection rules change: the usage tables are then rebuilt from the logs.
/// Version 3: observations refused by today's checks are set aside, and closed-day summaries
/// built from them are not trusted; earlier projections are rebuilt from the logs. Version 4: an
/// older summary's reset periods are checked against every day they cover. Version 6: a window
/// length bounds that dependence only when the reset time is known; `refused_by_series`.
/// Version 5: its day and
/// hour rollups and its periods are checked against every day they depend on, back to the series'
/// previous surviving reading when the window length is unknown.
pub const USAGE_PROJECTOR_VERSION: u32 = 6;

impl Projector for UsageProjector {
    fn name(&self) -> &'static str {
        "usage"
    }
    fn version(&self) -> u32 {
        USAGE_PROJECTOR_VERSION
    }
    fn reads(&self, _log: LogId) -> bool {
        true
    }
    fn apply(&self, txn: &WriteTransaction, _log: LogId, event: &Event) -> Result<Vec<Finding>> {
        let mut set_aside = Vec::new();
        match event.event_type.as_str() {
            "usage.sampled" => {
                let batch: SampledBatch = serde_json::from_value(msgpack::to_json(&event.payload))?;
                for sampled in &batch.observations {
                    // New observations are checked before the append (`record`); one an older
                    // build accepted is set aside here, so the rest of the history still opens.
                    // The finding names the event, never the refused text.
                    if let Err(e) = sampled.validate() {
                        let day = day_of(sampled.sampled_at_ms.max(0) as u64);
                        let series = sampled.fingerprints();
                        note_refused(txn, day, &series)?;
                        set_aside.push(Finding {
                            kind: "event_quarantined".into(),
                            detail: format!(
                                "an observation in this usage.sampled event fails today's checks ({}) and was left out of the usage tables; the log keeps the event",
                                e.message
                            ),
                            start_seq: Some(event.sequence_id),
                            end_seq: Some(event.sequence_id),
                            refused: Some(json!({ "day": day, "series": series })),
                        });
                        continue;
                    }
                    project_sampled(txn, sampled)?;
                }
            }
            "usage.seen" => {
                let batch: SeenBatch = serde_json::from_value(msgpack::to_json(&event.payload))?;
                for seen in &batch.accounts {
                    project_seen(txn, seen)?;
                }
            }
            "usage.day_closed" => set_aside = project_day_closed(txn, event, &msgpack::to_json(&event.payload))?,
            // Disclosed findings come back with a rebuild; a refusal withdraws older summaries.
            "store.integrity_detected" => {
                let found = crate::engine::finding_from_json(&msgpack::to_json(&event.payload));
                if let Some(refused) = &found.refused {
                    let day = refused["day"].as_u64().unwrap_or(u64::MAX).min(u32::MAX as u64) as u32;
                    let series: Vec<String> = refused["series"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|v| v.as_str().map(String::from))
                        .collect();
                    if !note_refused(txn, day, &series)?.is_empty() {
                        set_aside.push(summary_finding(event, "was built from an observation refused since"));
                    }
                }
                set_aside.push(found);
            }
            _ => {}
        }
        Ok(set_aside)
    }
    fn reset(&self, txn: &WriteTransaction) -> Result<()> {
        for def in [
            db::ACCOUNT,
            db::WINDOW,
            db::SERIES,
            db::READING_DAY,
            db::LATEST,
            db::ACCOUNT_LATEST,
            db::ACCOUNT_STATE,
            db::PERIOD,
            db::ROLLUP_HOURS,
            db::ROLLUP_DAY,
            db::SUMMARY_UNMARKED,
            db::SUMMARY_UNBOUNDED,
            db::LEGACY_READING,
            db::LEGACY_ROLLUP_HOUR,
        ] {
            txn.delete_table(def)?;
        }
        for def in [
            db::SERIES_BY_PROVIDER,
            db::LEGACY_READING_BY_TIME,
            db::DAY_CLOSED,
            db::REFUSED,
            db::REFUSED_BY_SERIES,
        ] {
            txn.delete_table(def)?;
        }
        db::set_meta(txn, "closed_through_day", "")?;
        // The dictionaries hold only this projector's strings: a rebuild starts them again, so
        // nothing an older projection interned (a refused string included) outlives it.
        txn.delete_table(db::DICT)?;
        txn.delete_table(db::DICT_REV)?;
        for kind in [
            db::KIND_ACCOUNT,
            db::KIND_WINDOW,
            db::KIND_PROVIDER,
            db::KIND_SOURCE,
            db::KIND_EVENT_TYPE,
        ] {
            txn.open_table(db::META)?.remove(format!("dict_next:{kind}").as_str())?;
        }
        Ok(())
    }
}

fn upsert_account(txn: &WriteTransaction, a: u32, obs: &AccountObs, t: i64, only_if_absent: bool) -> Result<()> {
    let key = Key::new().u32(a);
    let existing: Option<AccountMeta> = match get(txn, db::ACCOUNT, key.as_slice())? {
        Some(b) => Some(db::unpack(&b)?),
        None => None,
    };
    if only_if_absent && existing.is_some() {
        return Ok(());
    }
    let first = existing.as_ref().map(|m| m.first_ms.min(t)).unwrap_or(t);
    let last = existing.as_ref().map(|m| m.last_ms.max(t)).unwrap_or(t);
    let meta = AccountMeta {
        identity: obs.identity.clone(),
        provider: obs.provider.clone(),
        account_id: obs.account_id.clone(),
        plan: obs.plan.clone(),
        platform: obs.platform.clone(),
        first_ms: first,
        last_ms: last,
    };
    if existing.as_ref() != Some(&meta) {
        put(txn, db::ACCOUNT, key.as_slice(), &db::pack(&meta)?)?;
    }
    Ok(())
}

fn upsert_window(
    txn: &WriteTransaction,
    a: u32,
    p: u32,
    w: u32,
    obs: &WindowObs,
    t: i64,
    only_if_absent: bool,
) -> Result<SeriesMeta> {
    let wkey = Key::new().u32(w);
    if get(txn, db::WINDOW, wkey.as_slice())?.is_none() {
        let meta = WindowMeta {
            identity: obs.identity.clone(),
            key: obs.key.clone(),
            kind: obs.kind.clone(),
            unit: obs.unit.clone(),
            pool_id: obs.pool_id.clone(),
        };
        put(txn, db::WINDOW, wkey.as_slice(), &db::pack(&meta)?)?;
    }
    let skey = Key::new().u32(a).u32(w);
    let existing: Option<SeriesMeta> = match get(txn, db::SERIES, skey.as_slice())? {
        Some(b) => Some(db::unpack(&b)?),
        None => None,
    };
    if existing.is_none() {
        put_index(txn, db::SERIES_BY_PROVIDER, Key::new().u32(p).u32(a).u32(w).as_slice())?;
    }
    if let (true, Some(meta)) = (only_if_absent, existing.clone()) {
        return Ok(meta);
    }
    let mut meta = existing.unwrap_or(SeriesMeta {
        first_ms: t,
        last_ms: t,
        ..Default::default()
    });
    meta.first_ms = meta.first_ms.min(t);
    meta.last_ms = meta.last_ms.max(t);
    meta.label = obs.label.clone();
    meta.window_minutes = obs.window_minutes;
    meta.pool_id_source = obs.pool_id_source.clone();
    meta.pool_label = obs.pool_label.clone();
    meta.model_ids = obs.model_ids.clone();
    Ok(meta)
}

fn put_series(txn: &WriteTransaction, a: u32, w: u32, meta: &SeriesMeta) -> Result<()> {
    put(txn, db::SERIES, Key::new().u32(a).u32(w).as_slice(), &db::pack(meta)?)
}

struct Latest {
    t_first: u64,
    period_start: u64,
    row: Row,
}

fn read_latest(txn: &WriteTransaction, a: u32, w: u32) -> Result<Option<Latest>> {
    Ok(match get(txn, db::LATEST, Key::new().u32(a).u32(w).as_slice())? {
        Some(b) if b.len() >= 16 => Some(Latest {
            t_first: db::be_u64(&b, 0),
            period_start: db::be_u64(&b, 8),
            row: Row::decode(&b[16..])?,
        }),
        _ => None,
    })
}

/// Writes the series' open span to `latest`.
fn write_latest(txn: &WriteTransaction, a: u32, w: u32, latest: &Latest) -> Result<()> {
    let mut b = Vec::with_capacity(80);
    b.extend_from_slice(&latest.t_first.to_be_bytes());
    b.extend_from_slice(&latest.period_start.to_be_bytes());
    b.extend_from_slice(&latest.row.encode());
    put(txn, db::LATEST, Key::new().u32(a).u32(w).as_slice(), &b)
}

/// Adds a closed span to its day value (a span starting at the same time is replaced).
fn close_span(txn: &WriteTransaction, a: u32, w: u32, t_first: u64, row: &Row) -> Result<()> {
    let day = day_of(t_first);
    let key = Key::new().u32(a).u32(w).u32(day);
    let mut spans = match get(txn, db::READING_DAY, key.as_slice())? {
        Some(b) => pack::decode_spans(day, &b)?,
        None => Vec::new(),
    };
    match spans.binary_search_by_key(&t_first, |(t, _)| *t) {
        Ok(i) => spans[i].1 = row.clone(),
        Err(i) => spans.insert(i, (t_first, row.clone())),
    }
    put(txn, db::READING_DAY, key.as_slice(), &pack::encode_spans(day, &spans)?)
}

fn read_period(txn: &WriteTransaction, a: u32, w: u32, start: u64) -> Result<Option<Period>> {
    match get(txn, db::PERIOD, Key::new().u32(a).u32(w).u64(start).as_slice())? {
        Some(b) => Ok(Some(db::unpack(&b)?)),
        None => Ok(None),
    }
}

fn write_period(txn: &WriteTransaction, a: u32, w: u32, start: u64, period: &Period) -> Result<()> {
    put(
        txn,
        db::PERIOD,
        Key::new().u32(a).u32(w).u64(start).as_slice(),
        &db::pack(period)?,
    )
}

fn project_sampled(txn: &WriteTransaction, s: &Sampled) -> Result<()> {
    s.validate()?;
    let t = s.sampled_at_ms as u64;
    let a = db::intern(txn, db::KIND_ACCOUNT, &s.account.identity)?;
    let p = db::intern(txn, db::KIND_PROVIDER, &s.account.provider)?;
    upsert_account(txn, a, &s.account, s.sampled_at_ms, false)?;

    // The account's state span.
    let al_key = Key::new().u32(a);
    let previous: Option<AccountLatest> = match get(txn, db::ACCOUNT_LATEST, al_key.as_slice())? {
        Some(b) => Some(db::unpack(&b)?),
        None => None,
    };
    let state = AccountState {
        status: s.account.status.clone(),
        is_active: s.account.is_active,
        source: s.account.source.clone(),
        platform: s.account.platform.clone(),
        observed_ms: s.observed_at_ms,
        last_seen: s.sampled_at_ms,
        last_observed: s.observed_at_ms,
    };
    let mut state_t = s.sampled_at_ms;
    let mut extended = false;
    if let Some(prev) = &previous {
        let key = Key::new().u32(a).u64(prev.state_t as u64);
        if let Some(b) = get(txn, db::ACCOUNT_STATE, key.as_slice())? {
            let mut old: AccountState = db::unpack(&b)?;
            let same = old.status == state.status
                && old.is_active == state.is_active
                && old.source == state.source
                && old.platform == state.platform;
            // An unchanged state continues its span only while sightings are close together:
            // a longer silence is a gap, and a gap starts a new span.
            let close = s.sampled_at_ms - old.last_seen <= COVER_GAP_MS as i64;
            if same && close && s.sampled_at_ms >= prev.state_t {
                old.last_seen = old.last_seen.max(s.sampled_at_ms);
                old.last_observed = s.observed_at_ms;
                put(txn, db::ACCOUNT_STATE, key.as_slice(), &db::pack(&old)?)?;
                state_t = prev.state_t;
                extended = true;
            }
        }
    }
    if !extended {
        // A new state span (on the same key as the previous one, the newer observation wins).
        put(
            txn,
            db::ACCOUNT_STATE,
            Key::new().u32(a).u64(t).as_slice(),
            &db::pack(&state)?,
        )?;
    }

    let mut window_ids = Vec::with_capacity(s.windows.len());
    let closed = day_closed(txn, t)?;
    for win in &s.windows {
        let w = db::intern(txn, db::KIND_WINDOW, &win.identity)?;
        window_ids.push(w);
        let mut series = upsert_window(txn, a, p, w, win, s.sampled_at_ms, false)?;
        let vals = Vals::from_obs(win);
        let latest = read_latest(txn, a, w)?;
        match latest {
            Some(mut l)
                if l.row.vals.same(&vals) && t >= l.t_first && t.saturating_sub(l.row.last_seen) <= COVER_GAP_MS =>
            {
                cover(txn, a, w, l.row.last_seen, t)?;
                l.row.last_seen = l.row.last_seen.max(t);
                write_latest(txn, a, w, &l)?;
            }
            prev => {
                let mut period_start = prev.as_ref().map(|l| l.period_start).unwrap_or(0);
                if let Some(l) = &prev {
                    cover(txn, a, w, l.row.last_seen, t)?;
                }
                // Consumption and reset periods use measured, fresh percentages only.
                if let (Some(u), false) = (vals.used_pct, vals.cached) {
                    let prev_used = prev
                        .as_ref()
                        .filter(|l| !l.row.vals.cached && l.row.vals.used_pct.is_some());
                    match prev_used {
                        Some(l) => {
                            let prev_t = l.row.last_seen as i64;
                            let up = l.row.vals.used_pct.unwrap();
                            let reset = is_reset(prev_t, &l.row.vals, s.sampled_at_ms, &vals);
                            let added = if reset { u.max(0.0) } else { (u - up).max(0.0) };
                            let mut period_changed = true;
                            if let Some(mut period) = read_period(txn, a, w, period_start)? {
                                if period.is_final && s.sampled_at_ms <= period.end_ms {
                                    period_changed = false;
                                } else if reset {
                                    period.is_final = true;
                                    write_period(txn, a, w, period_start, &period)?;
                                }
                            }
                            if reset {
                                period_start = t;
                                write_period(
                                    txn,
                                    a,
                                    w,
                                    t,
                                    &Period {
                                        reset_at_ms: vals.reset_at,
                                        end_ms: s.sampled_at_ms,
                                        start_used: u,
                                        end_used: u,
                                        max_used: u,
                                        consumed: added,
                                        hit_limit_ms: if u >= 100.0 { Some(s.sampled_at_ms) } else { None },
                                        rows: 1,
                                        is_final: false,
                                        emitted: false,
                                    },
                                )?;
                            } else if period_changed && let Some(mut period) = read_period(txn, a, w, period_start)? {
                                period.consumed += added;
                                period.end_used = u;
                                period.end_ms = period.end_ms.max(s.sampled_at_ms);
                                period.max_used = period.max_used.max(u);
                                period.rows += 1;
                                if vals.reset_at.is_some() {
                                    period.reset_at_ms = vals.reset_at;
                                }
                                if u >= 100.0 && period.hit_limit_ms.is_none() {
                                    period.hit_limit_ms = Some(s.sampled_at_ms);
                                }
                                write_period(txn, a, w, period_start, &period)?;
                            }
                            if !closed {
                                with_rollups(txn, a, w, t, |r| {
                                    r.consumed += added;
                                    if reset {
                                        r.resets += 1;
                                        r.last_reset_ms = Some(t as i64);
                                    }
                                })?;
                            }
                        }
                        None => {
                            period_start = t;
                            if read_period(txn, a, w, t)?.is_none() {
                                write_period(
                                    txn,
                                    a,
                                    w,
                                    t,
                                    &Period {
                                        reset_at_ms: vals.reset_at,
                                        end_ms: s.sampled_at_ms,
                                        start_used: u,
                                        end_used: u,
                                        max_used: u,
                                        consumed: 0.0,
                                        hit_limit_ms: if u >= 100.0 { Some(s.sampled_at_ms) } else { None },
                                        rows: 1,
                                        is_final: false,
                                        emitted: false,
                                    },
                                )?;
                            }
                        }
                    }
                    if !closed {
                        with_rollups(txn, a, w, t, |r| r.note_used(u))?;
                    }
                } else if let (Some(rem), false, Some(l)) = (vals.remaining, vals.cached, prev.as_ref()) {
                    // Balances: decreases are spending, increases are credit added.
                    if let (Some(old), false) = (l.row.vals.remaining, l.row.vals.cached) {
                        let delta = rem - old;
                        if !closed {
                            with_rollups(txn, a, w, t, |r| {
                                if delta < 0.0 {
                                    r.credits_spent += -delta;
                                } else {
                                    r.credits_added += delta;
                                }
                            })?;
                        }
                    }
                }
                if closed {
                    note_late(txn, a, w, t)?;
                } else {
                    with_rollups(txn, a, w, t, |r| r.rows += 1)?;
                }
                let row = Row { vals, last_seen: t };
                if let Some(l) = prev.as_ref().filter(|l| l.t_first != t) {
                    // The previous span closes, with its final sighting.
                    close_span(txn, a, w, l.t_first, &l.row)?;
                }
                if prev.as_ref().map(|l| l.t_first != t).unwrap_or(true) {
                    series.rows += 1;
                }
                write_latest(
                    txn,
                    a,
                    w,
                    &Latest {
                        t_first: t,
                        period_start,
                        row,
                    },
                )?;
            }
        }
        put_series(txn, a, w, &series)?;
    }

    let latest = AccountLatest {
        state_t,
        sampled_ms: s.sampled_at_ms,
        digest: s.digest()?,
        status: s.account.status.clone(),
        window_ids,
    };
    put(txn, db::ACCOUNT_LATEST, al_key.as_slice(), &db::pack(&latest)?)
}

fn project_seen(txn: &WriteTransaction, seen: &Seen) -> Result<()> {
    let Some(a) = db::lookup_id(&txn.open_table(db::DICT_REV)?, db::KIND_ACCOUNT, &seen.account_identity)? else {
        return Ok(());
    };
    let al_key = Key::new().u32(a);
    let Some(b) = get(txn, db::ACCOUNT_LATEST, al_key.as_slice())? else {
        return Ok(());
    };
    let mut latest: AccountLatest = db::unpack(&b)?;
    if seen.sampled_at_ms < latest.sampled_ms {
        return Ok(());
    }
    let t = seen.sampled_at_ms as u64;
    let closed = day_closed(txn, t)?;
    let state_key = Key::new().u32(a).u64(latest.state_t as u64);
    if let Some(b) = get(txn, db::ACCOUNT_STATE, state_key.as_slice())? {
        let mut state: AccountState = db::unpack(&b)?;
        if seen.sampled_at_ms - state.last_seen <= COVER_GAP_MS as i64 {
            state.last_seen = state.last_seen.max(seen.sampled_at_ms);
            state.last_observed = seen.observed_at_ms;
            put(txn, db::ACCOUNT_STATE, state_key.as_slice(), &db::pack(&state)?)?;
        } else {
            // After a gap the same state starts a new span, so the gap stays visible.
            state.observed_ms = seen.observed_at_ms;
            state.last_seen = seen.sampled_at_ms;
            state.last_observed = seen.observed_at_ms;
            put(
                txn,
                db::ACCOUNT_STATE,
                Key::new().u32(a).u64(t).as_slice(),
                &db::pack(&state)?,
            )?;
            latest.state_t = seen.sampled_at_ms;
        }
    }
    for &w in &latest.window_ids {
        if let Some(mut l) = read_latest(txn, a, w)? {
            if t.saturating_sub(l.row.last_seen) <= COVER_GAP_MS {
                cover(txn, a, w, l.row.last_seen, t)?;
                l.row.last_seen = l.row.last_seen.max(t);
                write_latest(txn, a, w, &l)?;
            } else if t > l.t_first {
                let row = Row {
                    vals: l.row.vals.clone(),
                    last_seen: t,
                };
                close_span(txn, a, w, l.t_first, &l.row)?;
                write_latest(
                    txn,
                    a,
                    w,
                    &Latest {
                        t_first: t,
                        period_start: l.period_start,
                        row,
                    },
                )?;
                if closed {
                    note_late(txn, a, w, t)?;
                } else {
                    with_rollups(txn, a, w, t, |r| r.rows += 1)?;
                }
                if let Some(b) = get(txn, db::SERIES, Key::new().u32(a).u32(w).as_slice())? {
                    let mut meta: SeriesMeta = db::unpack(&b)?;
                    meta.rows += 1;
                    put_series(txn, a, w, &meta)?;
                }
            }
        }
        if let Some(b) = get(txn, db::SERIES, Key::new().u32(a).u32(w).as_slice())? {
            let mut meta: SeriesMeta = db::unpack(&b)?;
            if meta.last_ms < seen.sampled_at_ms {
                meta.last_ms = seen.sampled_at_ms;
                put_series(txn, a, w, &meta)?;
            }
        }
    }
    latest.sampled_ms = seen.sampled_at_ms;
    put(txn, db::ACCOUNT_LATEST, al_key.as_slice(), &db::pack(&latest)?)
}

/// A rollup in `usage.day_closed`: a positional array (consumed, credits spent, credits added,
/// first used, last used, lowest used, highest used, covered ms, rows, resets, late rows, last
/// reset), so a day of hourly rows stays small in the ledger kept for good.
fn rollup_to_json(r: &Rollup) -> Json {
    json!([
        r.consumed,
        r.credits_spent,
        r.credits_added,
        r.first_used,
        r.last_used,
        r.min_used,
        r.max_used,
        r.covered_ms,
        r.rows,
        r.resets,
        r.late_rows,
        r.last_reset_ms
    ])
}

fn rollup_from_json(v: &Json) -> Result<Rollup> {
    let Some(a) = v.as_array().filter(|a| a.len() == 12) else {
        fail!("corrupt", "day_closed rollup is not a 12-element array");
    };
    let f = |i: usize| a[i].as_f64().unwrap_or(0.0);
    let o = |i: usize| a[i].as_f64();
    Ok(Rollup {
        consumed: f(0),
        credits_spent: f(1),
        credits_added: f(2),
        first_used: o(3),
        last_used: o(4),
        min_used: o(5),
        max_used: o(6),
        covered_ms: a[7].as_u64().unwrap_or(0),
        rows: a[8].as_u64().unwrap_or(0) as u32,
        resets: a[9].as_u64().unwrap_or(0) as u32,
        late_rows: a[10].as_u64().unwrap_or(0) as u32,
        last_reset_ms: a[11].as_i64(),
    })
}

fn project_day_closed(txn: &WriteTransaction, event: &Event, payload: &Json) -> Result<Vec<Finding>> {
    let day = payload["day"].as_u64().unwrap_or(u64::MAX) as u32;
    if day == u32::MAX {
        fail!("corrupt", "day_closed without a day");
    }
    // Closes from this build were computed with refused observations already left out.
    let marked = payload["refused_excluded"] == true;
    let mut set_aside = Vec::new();
    for series in payload["series"].as_array().into_iter().flatten() {
        let account: AccountObs = serde_json::from_value(series["account"].clone())?;
        let window: WindowObs = serde_json::from_value(series["window"].clone())?;
        // The series as this projection knows it from clean readings, without interning anything.
        let known = {
            let rev = txn.open_table(db::DICT_REV)?;
            match (
                db::lookup_id(&rev, db::KIND_ACCOUNT, &account.identity)?,
                db::lookup_id(&rev, db::KIND_WINDOW, &window.identity)?,
            ) {
                (Some(a), Some(w)) => Some((a, w)),
                _ => None,
            }
        };
        // An older close checked against clean readings that still give its day is re-stated by
        // a close from this build (trusted or not), so a rebuild after the readings retire
        // serves what the check found; without those readings nothing is re-stated.
        if !marked
            && let Some((a, w)) = known
            && get(txn, db::ROLLUP_DAY, Key::new().u32(a).u32(w).u32(day).as_slice())?.is_some()
        {
            put_index(txn, db::RECLOSE, Key::new().u32(day).u32(a).u32(w).as_slice())?;
        }
        if !summary_text_ok(&account, &window) {
            set_aside.push(summary_finding(event, "holds text that today's checks refuse"));
            continue;
        }
        let fingerprint = series_fingerprint(&account.identity, &window.identity);
        // The day's rollups depend on the day and on the reading before its first one.
        let day_start = day as u64 * DAY_MS;
        let rollup_reach = reach(
            txn,
            known,
            day_start,
            (day_start + DAY_MS - 1) as i64,
            window.window_minutes,
            false,
        )?;
        let rollups_ok = marked || !reach_refused(txn, &fingerprint, &rollup_reach)?;
        // Each reset period separately, against every day it depends on.
        let mut periods: Vec<(u64, Period, Reach)> = Vec::new();
        let mut periods_refused = false;
        for period in series["periods"].as_array().into_iter().flatten() {
            let start = period["start_ms"].as_u64().unwrap_or(0);
            let mut value: Period = serde_json::from_value(period["period"].clone())?;
            let period_reach = reach(
                txn,
                known,
                start,
                value.end_ms,
                window.window_minutes,
                value.reset_at_ms.is_some(),
            )?;
            if !marked && reach_refused(txn, &fingerprint, &period_reach)? {
                periods_refused = true;
                continue;
            }
            value.is_final = true;
            value.emitted = true;
            periods.push((start, value, period_reach));
        }
        if !rollups_ok {
            set_aside.push(summary_finding(event, "was built from an observation refused since"));
        } else if periods_refused {
            set_aside.push(summary_finding(
                event,
                "holds a reset period over an observation refused since",
            ));
        }
        if !rollups_ok && periods.is_empty() {
            continue;
        }
        let a = db::intern(txn, db::KIND_ACCOUNT, &account.identity)?;
        let p = db::intern(txn, db::KIND_PROVIDER, &account.provider)?;
        let w = db::intern(txn, db::KIND_WINDOW, &window.identity)?;
        let t = day as i64 * DAY_MS as i64;
        upsert_account(txn, a, &account, t, true)?;
        let meta = upsert_window(txn, a, p, w, &window, t, true)?;
        if get(txn, db::SERIES, Key::new().u32(a).u32(w).as_slice())?.is_none() {
            put_series(txn, a, w, &meta)?;
        }
        if marked {
            txn.open_table(db::RECLOSE)?
                .remove(Key::new().u32(day).u32(a).u32(w).as_slice())?;
        }
        if rollups_ok {
            // The close replaces the whole day: an hour it does not list has no rollup, even if
            // a late reading reached it during a rebuild.
            let mut hours = Vec::new();
            for hour in series["hours"].as_array().into_iter().flatten() {
                let h = hour["hour"].as_u64().unwrap_or(u64::MAX);
                if h / 24 != day as u64 {
                    fail!("corrupt", "day_closed hour outside its day");
                }
                hours.push(((h % 24) as u8, rollup_from_json(&hour["rollup"])?));
            }
            hours.sort_by_key(|(h, _)| *h);
            let key = Key::new().u32(a).u32(w).u32(day);
            if hours.is_empty() {
                txn.open_table(db::ROLLUP_HOURS)?.remove(key.as_slice())?;
            } else {
                put(txn, db::ROLLUP_HOURS, key.as_slice(), &pack::encode_hours(&hours))?;
            }
            put(
                txn,
                db::ROLLUP_DAY,
                key.as_slice(),
                &rollup_from_json(&series["day_rollup"])?.encode(),
            )?;
            // An unmarked summary may still meet a refusal later in the ledger.
            if !marked {
                remember_reach(txn, &fingerprint, &rollup_reach, (a, w), (1, day as u64))?;
            }
        }
        for (start, value, period_reach) in &periods {
            write_period(txn, a, w, *start, value)?;
            if !marked {
                remember_reach(txn, &fingerprint, period_reach, (a, w), (2, *start))?;
            }
        }
    }
    put_index(txn, db::DAY_CLOSED, Key::new().u32(day).as_slice())?;
    let through: Option<u32> = db::get_meta(txn, "closed_through_day")?.and_then(|v| v.parse().ok());
    if through.map(|d| day > d).unwrap_or(true) {
        db::set_meta(txn, "closed_through_day", &day.to_string())?;
    }
    Ok(set_aside)
}

// ----------------------------------------------------------------------------- engine operations

/// What the store did with each observation.
#[derive(Serialize, Default)]
pub struct RecordResult {
    pub sampled: usize,
    pub seen: usize,
    pub skipped: usize,
    pub rejected: usize,
}

/// Decides and records observations, oldest first (docs/store.md 2.2, the JSON history's rules):
/// an unchanged account is `usage.seen` at most once per five-minute bucket; a change is
/// `usage.sampled`; an older or duplicate sample is skipped.
pub fn record(engine: &Engine, observations: Vec<Sampled>) -> Result<RecordResult> {
    let mut result = RecordResult::default();
    let mut pending: Vec<Sampled> = Vec::new();
    for obs in observations {
        if obs.validate().is_err() {
            result.rejected += 1;
        } else {
            pending.push(obs);
        }
    }
    pending.sort_by_key(|o| (o.sampled_at_ms, o.observed_at_ms));
    for chunk in pending.chunks(crate::engine::PROJECTOR_BATCH / 2) {
        let counts = engine.commit(LogId::Readings, |batch| {
            let mut counts = RecordResult::default();
            let mut sampled: Vec<Sampled> = Vec::new();
            let mut seen: Vec<Seen> = Vec::new();
            let mut waiting: std::collections::HashSet<String> = std::collections::HashSet::new();
            for obs in chunk {
                // An account already waiting in this tick's events is decided against its
                // projected state, so the waiting events go first.
                if waiting.contains(&obs.account.identity) || waiting.len() >= ACCOUNTS_PER_EVENT {
                    flush(batch, &mut sampled, &mut seen)?;
                    waiting.clear();
                }
                match decide(batch, obs)? {
                    Decision::Skip => counts.skipped += 1,
                    Decision::Seen => {
                        seen.push(Seen {
                            account_identity: obs.account.identity.clone(),
                            sampled_at_ms: obs.sampled_at_ms,
                            observed_at_ms: obs.observed_at_ms,
                            import_ref: obs.import_ref.clone(),
                        });
                        waiting.insert(obs.account.identity.clone());
                        counts.seen += 1;
                    }
                    Decision::Sampled => {
                        sampled.push(obs.clone());
                        waiting.insert(obs.account.identity.clone());
                        counts.sampled += 1;
                    }
                }
            }
            flush(batch, &mut sampled, &mut seen)?;
            Ok(counts)
        })?;
        result.sampled += counts.sampled;
        result.seen += counts.seen;
        result.skipped += counts.skipped;
    }
    Ok(result)
}

/// Appends one tick's waiting observations: one `usage.sampled`, then one `usage.seen`.
fn flush(batch: &mut Batch<'_>, sampled: &mut Vec<Sampled>, seen: &mut Vec<Seen>) -> Result<()> {
    if !sampled.is_empty() {
        let first = sampled.iter().map(|o| o.sampled_at_ms).min().unwrap_or(0);
        let payload = SampledBatch {
            observations: std::mem::take(sampled),
        };
        let mut event = Event::new(
            "usage.sampled",
            now_ms(),
            msgpack::from_json(&serde_json::to_value(&payload)?)?,
        );
        event.source_at_ms = Some(first.max(0) as u64);
        batch.push(event)?;
    }
    if !seen.is_empty() {
        let first = seen.iter().map(|o| o.sampled_at_ms).min().unwrap_or(0);
        let payload = SeenBatch {
            accounts: std::mem::take(seen),
        };
        let mut event = Event::new(
            "usage.seen",
            now_ms(),
            msgpack::from_json(&serde_json::to_value(&payload)?)?,
        );
        event.source_at_ms = Some(first.max(0) as u64);
        batch.push(event)?;
    }
    Ok(())
}

enum Decision {
    Skip,
    Seen,
    Sampled,
}

fn decide(batch: &Batch<'_>, obs: &Sampled) -> Result<Decision> {
    let txn = batch.txn;
    let a = db::lookup_id(&txn.open_table(db::DICT_REV)?, db::KIND_ACCOUNT, &obs.account.identity)?;
    let previous: Option<AccountLatest> = match a {
        Some(a) => match get(txn, db::ACCOUNT_LATEST, Key::new().u32(a).as_slice())? {
            Some(b) => Some(db::unpack(&b)?),
            None => None,
        },
        None => None,
    };
    let Some(prev) = previous else {
        // An initial unavailable row is not a measured zero.
        return Ok(if obs.windows.is_empty() {
            Decision::Skip
        } else {
            Decision::Sampled
        });
    };
    if obs.sampled_at_ms < prev.sampled_ms {
        return Ok(Decision::Skip);
    }
    let same = obs.digest()? == prev.digest;
    if same && obs.sampled_at_ms == prev.sampled_ms {
        return Ok(Decision::Skip);
    }
    if obs.windows.is_empty() && prev.window_ids.is_empty() && obs.account.status == prev.status {
        return Ok(Decision::Skip);
    }
    if same {
        let bucket = |t: i64| t.div_euclid(BUCKET_MS as i64);
        return Ok(if bucket(prev.sampled_ms) == bucket(obs.sampled_at_ms) {
            Decision::Skip
        } else {
            Decision::Seen
        });
    }
    Ok(Decision::Sampled)
}

/// Appends `usage.day_closed` for every finished UTC day not yet closed (2 hours after it ends).
pub fn close_days(engine: &Engine, now: u64) -> Result<Json> {
    let mut closed = Vec::new();
    loop {
        let day = engine.commit(LogId::Ledger, |batch| {
            let txn = batch.txn;
            let through: Option<u32> = db::get_meta(txn, "closed_through_day")?.and_then(|v| v.parse().ok());
            let next = match through {
                Some(d) => d + 1,
                None => {
                    let readings = txn.open_table(db::READING_DAY)?;
                    let latest = txn.open_table(db::LATEST)?;
                    match oldest_reading(&readings, &latest)? {
                        Some(t) => day_of(t),
                        None => return Ok(None),
                    }
                }
            };
            let day_end = (next as u64 + 1) * DAY_MS;
            if now < day_end + CLOSE_GRACE_MS {
                return Ok(None);
            }
            let series = day_series(txn, next)?;
            let chunks: Vec<Vec<Json>> = if series.is_empty() {
                vec![Vec::new()]
            } else {
                series.chunks(DAY_CLOSED_SERIES_PER_EVENT).map(|c| c.to_vec()).collect()
            };
            let parts = chunks.len();
            for (i, chunk) in chunks.into_iter().enumerate() {
                let payload =
                    json!({ "day": next, "part": i, "parts": parts, "series": chunk, "refused_excluded": true });
                let mut event = Event::new("usage.day_closed", now_ms(), msgpack::from_json(&payload)?);
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

fn day_series(txn: &WriteTransaction, day: u32) -> Result<Vec<Json>> {
    let mut keys = Vec::new();
    {
        let table = txn.open_table(db::SERIES)?;
        for item in table.iter()? {
            let (k, _) = item?;
            keys.push((db::be_u32(k.value(), 0), db::be_u32(k.value(), 4)));
        }
    }
    let mut out = Vec::new();
    for (a, w) in keys {
        if let Some(entry) = series_close(txn, (a, w), day, Periods::Unemitted)? {
            out.push(entry);
        }
    }
    Ok(out)
}

/// One series' entry in a day's close: its metadata, hourly and day rollups, and the reset
/// periods `include` selects. `None` when the series has no rollup that day.
/// Which reset periods a day's close entry carries.
#[derive(Clone, Copy)]
enum Periods {
    /// Final periods not yet emitted by a close (the regular close).
    Unemitted,
    /// Final periods that overlap the day (a replacement close): read by start key, the one
    /// open at the day's start and those starting in it, not the series' whole history.
    OverlappingDay,
}

fn series_close(txn: &WriteTransaction, (a, w): (u32, u32), day: u32, pick: Periods) -> Result<Option<Json>> {
    let Some(day_rollup) = get(txn, db::ROLLUP_DAY, Key::new().u32(a).u32(w).u32(day).as_slice())? else {
        return Ok(None);
    };
    let Some(value) = get(txn, db::SERIES, Key::new().u32(a).u32(w).as_slice())? else {
        return Ok(None);
    };
    let series: SeriesMeta = db::unpack(&value)?;
    let account: AccountMeta = db::unpack(&get(txn, db::ACCOUNT, Key::new().u32(a).as_slice())?.unwrap_or_default())?;
    let window: WindowMeta = db::unpack(&get(txn, db::WINDOW, Key::new().u32(w).as_slice())?.unwrap_or_default())?;
    let mut hours = Vec::new();
    if let Some(b) = get(txn, db::ROLLUP_HOURS, Key::new().u32(a).u32(w).u32(day).as_slice())? {
        for (hour, rollup) in pack::decode_hours(&b)? {
            hours.push(json!({ "hour": day * 24 + hour as u32, "rollup": rollup_to_json(&rollup) }));
        }
    }
    let mut periods = Vec::new();
    {
        let table = txn.open_table(db::PERIOD)?;
        let prefix = Key::new().u32(a).u32(w);
        let mut push = |k: &[u8], v: &[u8], include: &dyn Fn(&Period) -> bool| -> Result<()> {
            let period: Period = db::unpack(v)?;
            if include(&period) {
                periods.push(json!({ "start_ms": db::be_u64(k, 8), "period": period }));
            }
            Ok(())
        };
        match pick {
            Periods::Unemitted => {
                let end = db::prefix_end(prefix.as_slice());
                for item in table.range(prefix.as_slice()..end.as_slice())? {
                    let (k, v) = item?;
                    push(k.value(), v.value(), &|p| p.is_final && !p.emitted)?;
                }
            }
            Periods::OverlappingDay => {
                let day_start = day as u64 * DAY_MS;
                let start_key = prefix.clone().u64(day_start);
                let end_key = prefix.clone().u64(day_start + DAY_MS);
                let final_since = |p: &Period| p.is_final && p.end_ms >= day_start as i64;
                if let Some(item) = table.range(prefix.as_slice()..start_key.as_slice())?.next_back() {
                    let (k, v) = item?;
                    push(k.value(), v.value(), &final_since)?;
                }
                for item in table.range(start_key.as_slice()..end_key.as_slice())? {
                    let (k, v) = item?;
                    push(k.value(), v.value(), &final_since)?;
                }
            }
        }
    }
    Ok(Some(json!({
        "account": AccountObs {
            identity: account.identity, provider: account.provider, account_id: account.account_id,
            plan: account.plan, platform: account.platform, source: String::new(), status: String::new(), is_active: false,
        },
        "window": WindowObs {
            identity: window.identity, key: window.key, label: series.label, kind: window.kind, unit: window.unit,
            window_minutes: series.window_minutes, pool_id: window.pool_id, pool_id_source: series.pool_id_source,
            pool_label: series.pool_label, model_ids: series.model_ids, ..Default::default()
        },
        "hours": hours,
        "day_rollup": rollup_to_json(&Rollup::decode(&day_rollup)?),
        "periods": periods,
    })))
}

/// Appends a close from this build for every series and day whose older summary was set aside
/// while its clean readings still gave the day (`reclose`), with the final reset periods that
/// overlap that day. A later rebuild then finds the clean day in the ledger, as a reopen finds
/// it in redb, after the readings retire.
pub fn write_replacement_closes(engine: &Engine) -> Result<usize> {
    let pending: Vec<(u32, u32, u32)> = engine.with_db(|db| {
        let read = db.begin_read()?;
        let mut out = Vec::new();
        for item in read.open_table(db::RECLOSE)?.iter()? {
            let (k, _) = item?;
            out.push((
                db::be_u32(k.value(), 0),
                db::be_u32(k.value(), 4),
                db::be_u32(k.value(), 8),
            ));
        }
        Ok(out)
    })?;
    if pending.is_empty() {
        return Ok(0);
    }
    engine.commit(LogId::Ledger, |batch| {
        let mut written = 0;
        let mut days: Vec<u32> = pending.iter().map(|(d, _, _)| *d).collect();
        days.dedup();
        for day in days {
            let (start, end) = (day as i64 * DAY_MS as i64, (day as i64 + 1) * DAY_MS as i64);
            let mut series = Vec::new();
            for (_, a, w) in pending.iter().filter(|(d, _, _)| *d == day) {
                if let Some(entry) = series_close(batch.txn, (*a, *w), day, Periods::OverlappingDay)? {
                    series.push(filter_periods_to_day(entry, start, end));
                }
            }
            let parts = series.len().div_ceil(DAY_CLOSED_SERIES_PER_EVENT);
            for (i, chunk) in series.chunks(DAY_CLOSED_SERIES_PER_EVENT).enumerate() {
                let payload = json!({ "day": day, "part": i, "parts": parts, "series": chunk, "refused_excluded": true, "replaces_set_aside": true });
                let mut event = Event::new("usage.day_closed", now_ms(), msgpack::from_json(&payload)?);
                event.idempotency_key = Some(format!("{day}:replacement:{i}"));
                batch.push(event)?;
                written += 1;
            }
        }
        // A series whose day no longer has a rollup needs no replacement.
        let mut table = batch.txn.open_table(db::RECLOSE)?;
        for (day, a, w) in &pending {
            table.remove(Key::new().u32(*day).u32(*a).u32(*w).as_slice())?;
        }
        Ok(written)
    })
}

/// Keeps the periods of a close entry that overlap `[start, end)`: they start before its end and
/// end at or after its start.
fn filter_periods_to_day(mut entry: Json, start: i64, end: i64) -> Json {
    if let Some(periods) = entry["periods"].as_array_mut() {
        periods.retain(|p| {
            let begins = p["start_ms"].as_i64().unwrap_or(i64::MAX);
            let ends = p["period"]["end_ms"].as_i64().unwrap_or(i64::MIN);
            begins < end && ends >= start
        });
    }
    entry
}

/// Deletes readings and account states older than the retention (docs/store.md 5). Rollups,
/// periods and each series' open span stay. Returns how many spans and states went.
pub fn prune(engine: &Engine, now: u64) -> Result<Json> {
    let cutoff = now.saturating_sub(engine.config.retention_days * DAY_MS);
    let mut removed = 0usize;
    loop {
        let _logs = engine.lock_logs();
        let n = engine.with_db(|db| {
            let txn = begin(db)?;
            // Day values that can hold an expired span: per series, every day through the
            // cutoff's. A span still seen after the cutoff stays, so a day value is rewritten or
            // left as it is unless all its spans have expired.
            let mut victims: Vec<(Vec<u8>, Option<Vec<u8>>)> = Vec::new();
            let mut deleted = 0;
            {
                let series = txn.open_table(db::SERIES)?;
                let readings = txn.open_table(db::READING_DAY)?;
                'scan: for item in series.iter()? {
                    let (k, _) = item?;
                    let base = Key::new().bytes(k.value());
                    let last = base.clone().u32(day_key(cutoff));
                    for item in readings.range(base.as_slice()..=last.as_slice())? {
                        let (key, value) = item?;
                        let day = db::be_u32(key.value(), 8);
                        let spans = pack::decode_spans(day, value.value())?;
                        let before = spans.len();
                        let kept: Vec<(u64, Row)> = spans.into_iter().filter(|(_, r)| r.last_seen >= cutoff).collect();
                        if kept.len() == before {
                            continue;
                        }
                        deleted += before - kept.len();
                        let rewritten = if kept.is_empty() {
                            None
                        } else {
                            Some(pack::encode_spans(day, &kept)?)
                        };
                        victims.push((key.value().to_vec(), rewritten));
                        if victims.len() >= PRUNE_BATCH {
                            break 'scan;
                        }
                    }
                }
            }
            {
                let mut readings = txn.open_table(db::READING_DAY)?;
                for (key, rewritten) in &victims {
                    match rewritten {
                        Some(value) => readings.insert(key.as_slice(), value.as_slice())?.is_some(),
                        None => readings.remove(key.as_slice())?.is_some(),
                    };
                }
            }
            // Account states: every row that ended before the cutoff, except the current one.
            let mut states = Vec::new();
            {
                let latest = txn.open_table(db::ACCOUNT_LATEST)?;
                let mut current = std::collections::HashSet::new();
                for item in latest.iter()? {
                    let (k, v) = item?;
                    let al: AccountLatest = db::unpack(v.value())?;
                    current.insert((db::be_u32(k.value(), 0), al.state_t as u64));
                }
                let table = txn.open_table(db::ACCOUNT_STATE)?;
                for item in table.iter()? {
                    let (k, v) = item?;
                    let (a, t) = (db::be_u32(k.value(), 0), db::be_u64(k.value(), 4));
                    let state: AccountState = db::unpack(v.value())?;
                    if (state.last_seen as u64) < cutoff && !current.contains(&(a, t)) {
                        states.push(k.value().to_vec());
                    }
                    if states.len() >= PRUNE_BATCH {
                        break;
                    }
                }
            }
            for key in &states {
                txn.open_table(db::ACCOUNT_STATE)?.remove(key.as_slice())?;
            }
            txn.commit()?;
            Ok((victims.len(), deleted + states.len()))
        })?;
        removed += n.1;
        if n.0 < PRUNE_BATCH {
            break;
        }
    }
    Ok(json!({ "removedRows": removed, "cutoffMs": cutoff }))
}

/// Retires sealed readings segments past retention once every projector has consumed them, the
/// days they hold are closed in the ledger, and (when backups run) a backup holds them.
pub fn retire_segments(engine: &Engine, now: u64, backed_up_through: Option<u64>) -> Result<Json> {
    let cutoff = now.saturating_sub(engine.config.retention_days * DAY_MS);
    let (consumed, closed_through) = engine.with_db(|db| {
        let read = db.begin_read()?;
        let table = read.open_table(db::CHECKPOINT)?;
        let cp = db::read_checkpoint(&table, "usage/readings")?
            .map(|c| c.last_sequence_id)
            .unwrap_or(0);
        let meta = read.open_table(db::META)?;
        let closed: Option<u64> = meta
            .get("closed_through_day")?
            .and_then(|v| String::from_utf8_lossy(v.value()).parse().ok());
        Ok((cp, closed))
    })?;
    let mut logs = engine.lock_logs();
    let log = logs.get(LogId::Readings);
    let mut through = None;
    for s in &log.manifest().segments {
        let ok = s.end_ms < cutoff
            && s.end_seq <= consumed
            && closed_through.map(|d| s.end_ms / DAY_MS <= d).unwrap_or(false)
            && backed_up_through.map(|b| s.end_seq <= b).unwrap_or(true);
        if !ok {
            break;
        }
        through = Some(s.start_seq);
    }
    let gone = match through {
        Some(start) => log.retire_through(start)?,
        None => Vec::new(),
    };
    drop(logs);
    if !gone.is_empty() {
        let payload = json!({ "log": "readings", "segments": gone.iter().map(|s| json!({"start_seq": s.start_seq, "end_seq": s.end_seq})).collect::<Vec<_>>() });
        let event = Event::new("store.segment_retired", now_ms(), msgpack::from_json(&payload)?);
        engine.commit(LogId::Ledger, |batch| batch.push(event).map(|_| ()))?;
    }
    Ok(json!({ "retired": gone.len() }))
}

// ----------------------------------------------------------------------------- queries

pub fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let (y, m, d) = civil_from_days(secs.div_euclid(86_400));
    let rem = secs.rem_euclid(86_400);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{millis:03}Z",
        rem / 3600,
        rem / 60 % 60,
        rem % 60
    )
}

fn window_json(window: &WindowMeta, series: &SeriesMeta, row: &Row) -> Json {
    let v = &row.vals;
    let mut out = serde_json::Map::new();
    out.insert("key".into(), json!(window.key));
    out.insert("label".into(), json!(series.label));
    out.insert("usedPercent".into(), json!(v.used_pct));
    out.insert("remainingPercent".into(), json!(v.remaining_pct));
    out.insert("resetAt".into(), json!(v.reset_at.map(iso)));
    out.insert("windowMinutes".into(), json!(v.window_minutes));
    out.insert("used".into(), json!(v.used));
    out.insert("limit".into(), json!(v.limit));
    out.insert("unit".into(), json!(window.unit));
    if let Some(kind) = &window.kind {
        out.insert("kind".into(), json!(kind));
    }
    out.insert("remaining".into(), json!(v.remaining));
    out.insert("expiresAt".into(), json!(v.expires_at.map(iso)));
    if let Some(u) = v.unlimited {
        out.insert("unlimited".into(), json!(u));
    }
    if let Some(e) = v.enabled {
        out.insert("enabled".into(), json!(e));
    }
    if let Some(c) = v.cached_sampled_at {
        out.insert("status".into(), json!("cached"));
        out.insert("sampledAt".into(), json!(iso(c)));
    }
    if let Some(p) = &window.pool_id {
        out.insert("poolId".into(), json!(p));
    }
    if let Some(p) = &series.pool_id_source {
        out.insert("poolIdSource".into(), json!(p));
    }
    if let Some(p) = &series.pool_label {
        out.insert("poolLabel".into(), json!(p));
    }
    if let Some(m) = &series.model_ids {
        out.insert("modelIds".into(), json!(m));
    }
    Json::Object(out)
}

type OpenSpans = std::collections::HashMap<(u32, u32), (u64, Row)>;

/// Each series' open span, from `latest`. An open span is newer than every span of its series
/// in `reading_day`.
fn open_spans(read: &redb::ReadTransaction) -> Result<OpenSpans> {
    let mut out = OpenSpans::new();
    for item in read.open_table(db::LATEST)?.iter()? {
        let (k, v) = item?;
        let b = v.value();
        if b.len() >= 16 {
            out.insert(
                (db::be_u32(k.value(), 0), db::be_u32(k.value(), 4)),
                (db::be_u64(b, 0), Row::decode(&b[16..])?),
            );
        }
    }
    Ok(out)
}

/// The open spans of the given series only: one `latest` read each.
fn open_spans_for<T: ReadableTable<&'static [u8], &'static [u8]>>(
    latest: &T,
    series: impl IntoIterator<Item = (u32, u32)>,
) -> Result<OpenSpans> {
    let mut out = OpenSpans::new();
    for (a, w) in series {
        if let Some(v) = latest.get(Key::new().u32(a).u32(w).as_slice())? {
            let b = v.value();
            if b.len() >= 16 {
                out.insert((a, w), (db::be_u64(b, 0), Row::decode(&b[16..])?));
            }
        }
    }
    Ok(out)
}

/// The accounts a query names by identity, through `dict_rev`: no account row is read.
fn accounts_named(read: &redb::ReadTransaction, identities: &[String]) -> Result<Vec<u32>> {
    let rev = read.open_table(db::DICT_REV)?;
    let mut ids = Vec::new();
    for identity in identities {
        if let Some(a) = db::lookup_id(&rev, db::KIND_ACCOUNT, identity)? {
            ids.push(a);
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// The windows with this key: the window table is read (one small row per window), no series.
fn windows_keyed(read: &redb::ReadTransaction, key: &str) -> Result<Vec<u32>> {
    let mut ids = Vec::new();
    for item in read.open_table(db::WINDOW)?.iter()? {
        let (k, v) = item?;
        let meta: WindowMeta = db::unpack(v.value())?;
        if meta.key == key {
            ids.push(db::be_u32(k.value(), 0));
        }
    }
    Ok(ids)
}

/// The series a filtered query reads, in key order, with their `series` rows, chosen through the
/// indexes (docs/store.md 3.2): identities through `dict_rev` and an account prefix, a provider
/// through `series_by_provider`, a window key through the window table and then the keys of
/// `series_by_provider`. Only an `account_ids` filter reads account rows (accounts are few, and
/// `account_id` is not interned). Without filters, every series.
fn select_series(
    read: &redb::ReadTransaction,
    identities: Option<&[String]>,
    account_ids: Option<&[String]>,
    provider: Option<&str>,
    window_key: Option<&str>,
) -> Result<Vec<((u32, u32), Vec<u8>)>> {
    let series_table = read.open_table(db::SERIES)?;
    let windows: Option<Vec<u32>> = window_key.map(|k| windows_keyed(read, k)).transpose()?;
    let window_ok = |w: u32| windows.as_ref().is_none_or(|ids| ids.contains(&w));
    let mut accounts: Option<Vec<u32>> = identities.map(|ids| accounts_named(read, ids)).transpose()?;
    if let Some(wanted) = account_ids {
        let table = read.open_table(db::ACCOUNT)?;
        let mut matched = Vec::new();
        for item in table.iter()? {
            let (k, v) = item?;
            let a = db::be_u32(k.value(), 0);
            if accounts.as_ref().is_some_and(|ids| !ids.contains(&a)) {
                continue;
            }
            let meta: AccountMeta = db::unpack(v.value())?;
            if wanted.contains(&meta.account_id) {
                matched.push(a);
            }
        }
        accounts = Some(matched);
    }
    let mut keys: Vec<(u32, u32)> = Vec::new();
    if let Some(provider) = provider {
        let rev = read.open_table(db::DICT_REV)?;
        if let Some(p) = db::lookup_id(&rev, db::KIND_PROVIDER, provider)? {
            let index = read.open_table(db::SERIES_BY_PROVIDER)?;
            let prefix = Key::new().u32(p);
            let end = db::prefix_end(prefix.as_slice());
            for item in index.range(prefix.as_slice()..end.as_slice())? {
                let (k, _) = item?;
                let (a, w) = (db::be_u32(k.value(), 4), db::be_u32(k.value(), 8));
                if accounts.as_ref().is_none_or(|ids| ids.contains(&a)) && window_ok(w) {
                    keys.push((a, w));
                }
            }
        }
    } else if let Some(ids) = &accounts {
        for &a in ids {
            let prefix = Key::new().u32(a);
            let end = db::prefix_end(prefix.as_slice());
            for item in series_table.range(prefix.as_slice()..end.as_slice())? {
                let (k, _) = item?;
                let w = db::be_u32(k.value(), 4);
                if window_ok(w) {
                    keys.push((a, w));
                }
            }
        }
    } else if windows.is_some() {
        // Every series of these windows, from the provider index's keys alone.
        for item in read.open_table(db::SERIES_BY_PROVIDER)?.iter()? {
            let (k, _) = item?;
            let (a, w) = (db::be_u32(k.value(), 4), db::be_u32(k.value(), 8));
            if window_ok(w) {
                keys.push((a, w));
            }
        }
    } else {
        let mut out = Vec::new();
        for item in series_table.iter()? {
            let (k, v) = item?;
            out.push(((db::be_u32(k.value(), 0), db::be_u32(k.value(), 4)), v.value().to_vec()));
        }
        return Ok(out);
    }
    keys.sort_unstable();
    keys.dedup();
    let mut out = Vec::new();
    for (a, w) in keys {
        if let Some(v) = series_table.get(Key::new().u32(a).u32(w).as_slice())? {
            out.push(((a, w), v.value().to_vec()));
        }
    }
    Ok(out)
}

/// A time's UTC day as a key field; times past the last key day clamp to it.
fn day_key(t: u64) -> u32 {
    (t / DAY_MS).min(u32::MAX as u64) as u32
}

/// Visits a series' spans starting in `[from, to]`, oldest first, until `visit` says stop. Reads
/// one `reading_day` value per day in the range (a seek per day, then the whole day's value).
fn visit_spans<T: ReadableTable<&'static [u8], &'static [u8]>>(
    readings: &T,
    open: &OpenSpans,
    (a, w): (u32, u32),
    (from, to): (u64, u64),
    mut visit: impl FnMut(u64, Row) -> Result<bool>,
) -> Result<()> {
    let base = Key::new().u32(a).u32(w);
    let lo = base.clone().u32(day_key(from));
    let hi = base.clone().u32(day_key(to));
    for item in readings.range(lo.as_slice()..=hi.as_slice())? {
        let (k, v) = item?;
        for (t, row) in pack::decode_spans(db::be_u32(k.value(), 8), v.value())? {
            if t > to {
                return Ok(());
            }
            if t >= from && !visit(t, row)? {
                return Ok(());
            }
        }
    }
    if let Some((t, row)) = open.get(&(a, w))
        && *t >= from
        && *t <= to
    {
        visit(*t, row.clone())?;
    }
    Ok(())
}

/// The series' last span starting at or before `t`: a reverse seek by day.
fn span_at<T: ReadableTable<&'static [u8], &'static [u8]>>(
    readings: &T,
    open: &OpenSpans,
    (a, w): (u32, u32),
    t: u64,
) -> Result<Option<(u64, Row)>> {
    if let Some((first, row)) = open.get(&(a, w))
        && *first <= t
    {
        return Ok(Some((*first, row.clone())));
    }
    let base = Key::new().u32(a).u32(w);
    let hi = base.clone().u32(day_key(t));
    for item in readings.range(base.as_slice()..=hi.as_slice())?.rev() {
        let (k, v) = item?;
        let spans = pack::decode_spans(db::be_u32(k.value(), 8), v.value())?;
        if let Some(span) = spans.into_iter().rev().find(|(first, _)| *first <= t) {
            return Ok(Some(span));
        }
    }
    Ok(None)
}

/// The oldest reading's start time: one seek per series (every series has a `latest` row).
fn oldest_reading<R, L>(readings: &R, latest: &L) -> Result<Option<u64>>
where
    R: ReadableTable<&'static [u8], &'static [u8]>,
    L: ReadableTable<&'static [u8], &'static [u8]>,
{
    let mut oldest: Option<u64> = None;
    for item in latest.iter()? {
        let (k, v) = item?;
        let end = db::prefix_end(k.value());
        let first = match readings.range(k.value()..end.as_slice())?.next() {
            Some(item) => {
                let (key, value) = item?;
                pack::decode_spans(db::be_u32(key.value(), 8), value.value())?
                    .first()
                    .map(|(t, _)| *t)
            }
            None => None,
        };
        let open = (v.value().len() >= 8).then(|| db::be_u64(v.value(), 0));
        if let Some(t) = first.or(open) {
            oldest = Some(oldest.map_or(t, |o| o.min(t)));
        }
    }
    Ok(oldest)
}

/// Visits a series' hourly rollups for hours in `[from, to]` (hour indexes): one value per day.
fn visit_hours<T: ReadableTable<&'static [u8], &'static [u8]>>(
    hours: &T,
    (a, w): (u32, u32),
    (from, to): (u64, u64),
    mut visit: impl FnMut(u64, Rollup) -> Result<bool>,
) -> Result<()> {
    let base = Key::new().u32(a).u32(w);
    let lo = base.clone().u32((from / 24).min(u32::MAX as u64) as u32);
    let hi = base.clone().u32((to / 24).min(u32::MAX as u64) as u32);
    for item in hours.range(lo.as_slice()..=hi.as_slice())? {
        let (k, v) = item?;
        let day = db::be_u32(k.value(), 8) as u64;
        for (hour, rollup) in pack::decode_hours(v.value())? {
            let index = day * 24 + hour as u64;
            if index > to {
                return Ok(());
            }
            if index >= from && !visit(index, rollup)? {
                return Ok(());
            }
        }
    }
    Ok(())
}

/// How `history_records` thins: the last record per account and bucket, or the JSON history's
/// own rule (every record in the last hour, 5-minute buckets to a day, hourly beyond).
#[derive(Deserialize, Clone, Copy)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum Thinning {
    Bucket {
        #[serde(rename = "bucketMs")]
        bucket_ms: u64,
    },
    History,
    None,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryQuery {
    pub from_ms: u64,
    pub to_ms: u64,
    pub now_ms: u64,
    pub thinning: Thinning,
    pub identities: Option<Vec<String>>,
}

/// Rebuilds the JSON history's account observations (`AccountAnalyticsObservation`) for a time
/// range from the spans: one full account record at every time any of its spans starts or ends.
pub fn history_records(engine: &Engine, query: &HistoryQuery) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let accounts = read.open_table(db::ACCOUNT)?;
        let windows = read.open_table(db::WINDOW)?;
        let series_table = read.open_table(db::SERIES)?;
        let readings = read.open_table(db::READING_DAY)?;
        let states = read.open_table(db::ACCOUNT_STATE)?;
        let latest_table = read.open_table(db::ACCOUNT_LATEST)?;
        let latest_rows = read.open_table(db::LATEST)?;
        let (from, to) = (query.from_ms, query.to_ms);
        let mut records: Vec<(i64, Json)> = Vec::new();
        // Named accounts through `dict_rev`; otherwise every account.
        let selected: Vec<(u32, AccountMeta)> = match &query.identities {
            Some(ids) => {
                let mut out = Vec::new();
                for a in accounts_named(&read, ids)? {
                    if let Some(v) = accounts.get(Key::new().u32(a).as_slice())? {
                        out.push((a, db::unpack(v.value())?));
                    }
                }
                out
            }
            None => {
                let mut out = Vec::new();
                for item in accounts.iter()? {
                    let (k, v) = item?;
                    out.push((db::be_u32(k.value(), 0), db::unpack(v.value())?));
                }
                out
            }
        };
        for (a, meta) in selected {
            // Account state spans overlapping the range: the span open at `from` (one reverse
            // seek; spans do not overlap, so no earlier one reaches it), then those starting in it.
            let mut spans: Vec<(u64, AccountState)> = Vec::new();
            let prefix = Key::new().u32(a);
            let at_from = Key::new().u32(a).u64(from);
            if let Some(item) = states.range(prefix.as_slice()..=at_from.as_slice())?.next_back() {
                let (k, v) = item?;
                let state: AccountState = db::unpack(v.value())?;
                if state.last_seen as u64 >= from {
                    spans.push((db::be_u64(k.value(), 4), state));
                }
            }
            let after = Key::new().u32(a).u64(from.saturating_add(1));
            let end = Key::new().u32(a).u64(to.saturating_add(1));
            if from < u64::MAX && after.as_slice() < end.as_slice() {
                for item in states.range(after.as_slice()..end.as_slice())? {
                    let (k, v) = item?;
                    spans.push((db::be_u64(k.value(), 4), db::unpack(v.value())?));
                }
            }
            if spans.is_empty() {
                continue;
            }
            // Window order: the latest observation's order first, then the rest by id.
            let mut order: Vec<u32> = match latest_table.get(Key::new().u32(a).as_slice())? {
                Some(v) => db::unpack::<AccountLatest>(v.value())?.window_ids,
                None => Vec::new(),
            };
            let sprefix = Key::new().u32(a);
            let send = db::prefix_end(sprefix.as_slice());
            let mut series_meta = Vec::new();
            for item in series_table.range(sprefix.as_slice()..send.as_slice())? {
                let (k, v) = item?;
                let w = db::be_u32(k.value(), 4);
                if !order.contains(&w) {
                    order.push(w);
                }
                series_meta.push((w, db::unpack::<SeriesMeta>(v.value())?));
            }
            let open = open_spans_for(&latest_rows, series_meta.iter().map(|(w, _)| (a, *w)))?;
            let mut window_rows: Vec<(u32, WindowMeta, SeriesMeta, Vec<(u64, Row)>)> = Vec::new();
            for w in order {
                let Some((_, smeta)) = series_meta.iter().find(|(id, _)| *id == w) else {
                    continue;
                };
                let wmeta: WindowMeta = match windows.get(Key::new().u32(w).as_slice())? {
                    Some(v) => db::unpack(v.value())?,
                    None => continue,
                };
                let mut rows = Vec::new();
                if let Some(before) = from.checked_sub(1)
                    && let Some((t, row)) = span_at(&readings, &open, (a, w), before)?
                    && row.last_seen >= from
                {
                    rows.push((t, row));
                }
                visit_spans(&readings, &open, (a, w), (from, to), |t, row| {
                    rows.push((t, row));
                    Ok(true)
                })?;
                if !rows.is_empty() {
                    window_rows.push((w, wmeta, smeta.clone(), rows));
                }
            }
            let mut times: Vec<u64> = Vec::new();
            for (t, s) in &spans {
                times.push(*t);
                times.push(s.last_seen as u64);
            }
            for (_, _, _, rows) in &window_rows {
                for (t, r) in rows {
                    times.push(*t);
                    times.push(r.last_seen);
                }
            }
            // Inside each continuously observed span, readings on the thinning grid: the JSON
            // history kept an unchanged reading every five minutes (hourly beyond a day), and
            // burn rates need a reading near each horizon's start.
            for (t0, s) in &spans {
                let last = s.last_seen as u64;
                let mut tau = *t0;
                loop {
                    let grid = match query.thinning {
                        Thinning::None => break,
                        Thinning::Bucket { bucket_ms } => bucket_ms.max(60_000),
                        Thinning::History => {
                            // An unchanged account was recorded once per five-minute bucket.
                            let age = query.now_ms.saturating_sub(tau);
                            if age <= DAY_MS { BUCKET_MS } else { HOUR_MS }
                        }
                    };
                    tau = (tau / grid + 1) * grid;
                    if tau >= last || tau > to {
                        break;
                    }
                    if tau >= from {
                        times.push(tau);
                    }
                }
            }
            times.retain(|t| *t >= from && *t <= to);
            times.sort_unstable();
            times.dedup();
            let mut kept: Vec<(i64, Json)> = Vec::new();
            for tau in times {
                let Some((state_t, state)) = spans.iter().rev().find(|(t, s)| *t <= tau && tau <= s.last_seen as u64)
                else {
                    continue;
                };
                // Inside a span only its first and last observation times are kept: a rebuilt
                // reading takes the span's first offset from its sample time, within the two.
                let observed = if *state_t == tau {
                    state.observed_ms
                } else if tau == state.last_seen as u64 {
                    state.last_observed
                } else {
                    let offset = state.observed_ms - *state_t as i64;
                    let (low, high) = (
                        state.observed_ms.min(state.last_observed),
                        state.observed_ms.max(state.last_observed),
                    );
                    (tau as i64 + offset).clamp(low, high)
                };
                let mut wins = Vec::new();
                for (_, wmeta, smeta, rows) in &window_rows {
                    if let Some((_, row)) = rows.iter().rev().find(|(t, r)| *t <= tau && tau <= r.last_seen) {
                        wins.push(window_json(wmeta, smeta, row));
                    }
                }
                kept.push((
                    tau as i64,
                    json!({
                        "identity": meta.identity,
                        "accountId": meta.account_id,
                        "sampledAt": iso(tau as i64),
                        "observedAt": iso(observed),
                        "platform": state.platform,
                        "source": state.source,
                        "status": state.status,
                        "isActive": state.is_active,
                        "windows": wins,
                    }),
                ));
            }
            // Thinning, newest record per bucket wins.
            let mut buckets: std::collections::BTreeMap<(u64, i64), (i64, Json)> = std::collections::BTreeMap::new();
            for (i, (t, record)) in kept.into_iter().enumerate() {
                let key = match query.thinning {
                    Thinning::None => (0, i as i64),
                    Thinning::Bucket { bucket_ms } => (bucket_ms, t.div_euclid(bucket_ms.max(1) as i64)),
                    Thinning::History => {
                        let age = query.now_ms as i64 - t;
                        let interval: i64 = if age <= HOUR_MS as i64 {
                            0
                        } else if age <= DAY_MS as i64 {
                            BUCKET_MS as i64
                        } else {
                            HOUR_MS as i64
                        };
                        if interval == 0 {
                            (0, i as i64)
                        } else {
                            (interval as u64, t.div_euclid(interval))
                        }
                    }
                };
                buckets.insert(key, (t, record));
            }
            records.extend(buckets.into_values());
        }
        records.sort_by_key(|(t, _)| *t);
        Ok(Json::Array(records.into_iter().map(|(_, r)| r).collect()))
    })
}

/// The oldest reading time, or null.
pub fn oldest(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let readings = read.open_table(db::READING_DAY)?;
        let latest = read.open_table(db::LATEST)?;
        Ok(match oldest_reading(&readings, &latest)? {
            Some(t) => json!(t),
            None => Json::Null,
        })
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesRef {
    pub identity: String,
    pub window: String,
}

/// Consumed points, rate and coverage over each span ending `now`, from hourly rollups.
pub fn burn_long(engine: &Engine, series: &[SeriesRef], spans_ms: &[u64], now: u64) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let rev = read.open_table(db::DICT_REV)?;
        let hours = read.open_table(db::ROLLUP_HOURS)?;
        let mut out = Vec::new();
        for s in series {
            let ids = (
                db::lookup_id(&rev, db::KIND_ACCOUNT, &s.identity)?,
                db::lookup_id(&rev, db::KIND_WINDOW, &s.window)?,
            );
            let mut spans = Vec::new();
            for &span in spans_ms {
                let (Some(a), Some(w)) = ids else {
                    spans.push(json!({ "spanMs": span, "consumed": null, "rate": null, "coverage": 0.0 }));
                    continue;
                };
                let from = now.saturating_sub(span);
                let (mut consumed, mut covered, mut any) = (0.0, 0u64, false);
                visit_hours(&hours, (a, w), (from / HOUR_MS, now / HOUR_MS), |_, r| {
                    consumed += r.consumed;
                    covered += r.covered_ms;
                    any |= r.first_used.is_some();
                    Ok(true)
                })?;
                let coverage = (covered as f64 / span.max(1) as f64).min(1.0);
                // Without a reading in the span, consumption is unknown, not zero (7.2).
                let (consumed, rate) = if any {
                    (
                        Some(round2(consumed)),
                        Some(round2(consumed / (span as f64 / HOUR_MS as f64))),
                    )
                } else {
                    (None, None)
                };
                spans.push(json!({ "spanMs": span, "consumed": consumed, "rate": rate, "coverage": round2(coverage) }));
            }
            out.push(json!({ "identity": s.identity, "window": s.window, "spans": spans }));
        }
        Ok(Json::Array(out))
    })
}

fn round2(v: f64) -> f64 {
    (v * 100.0).round() / 100.0
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesQuery {
    pub identities: Option<Vec<String>>,
    pub account_ids: Option<Vec<String>>,
    pub provider: Option<String>,
    pub window_key: Option<String>,
    pub from_ms: u64,
    pub to_ms: u64,
    /// `raw`, `hour` or `day`.
    pub resolution: String,
    pub limit: Option<usize>,
}

/// Points or rollups per series (the `/api/usage/history` route).
pub fn series_points(engine: &Engine, query: &SeriesQuery) -> Result<Json> {
    let limit = query.limit.unwrap_or(20_000).min(20_000);
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let accounts = read.open_table(db::ACCOUNT)?;
        let windows = read.open_table(db::WINDOW)?;
        let readings = read.open_table(db::READING_DAY)?;
        let hours = read.open_table(db::ROLLUP_HOURS)?;
        let days = read.open_table(db::ROLLUP_DAY)?;
        let selected = select_series(
            &read,
            query.identities.as_deref(),
            query.account_ids.as_deref(),
            query.provider.as_deref(),
            query.window_key.as_deref(),
        )?;
        let open = open_spans_for(&read.open_table(db::LATEST)?, selected.iter().map(|(key, _)| *key))?;
        let mut out = Vec::new();
        let mut total = 0usize;
        let mut truncated = false;
        for ((a, w), value) in selected {
            let smeta: SeriesMeta = db::unpack(&value)?;
            let Some(av) = accounts.get(Key::new().u32(a).as_slice())? else { continue };
            let ameta: AccountMeta = db::unpack(av.value())?;
            let Some(wv) = windows.get(Key::new().u32(w).as_slice())? else { continue };
            let wmeta: WindowMeta = db::unpack(wv.value())?;
            if query.identities.as_ref().map(|ids| !ids.contains(&ameta.identity)).unwrap_or(false)
                || query.account_ids.as_ref().map(|ids| !ids.contains(&ameta.account_id)).unwrap_or(false)
                || query.provider.as_ref().map(|p| *p != ameta.provider).unwrap_or(false)
                || query.window_key.as_ref().map(|k| *k != wmeta.key).unwrap_or(false)
            {
                continue;
            }
            let mut points = Vec::new();
            let rollup_point = |start: u64, r: &Rollup| {
                json!([start, round2(r.consumed), r.last_used, r.max_used, r.covered_ms / 60_000, r.rows, r.resets, round2(r.credits_spent), round2(r.credits_added)])
            };
            match query.resolution.as_str() {
                "raw" => {
                    visit_spans(&readings, &open, (a, w), (query.from_ms, query.to_ms), |t, r| {
                        if total >= limit {
                            truncated = true;
                            return Ok(false);
                        }
                        points.push(json!([t, r.last_seen, r.vals.used_pct, r.vals.remaining_pct, r.vals.used, r.vals.limit, r.vals.remaining, r.vals.reset_at, r.vals.cached]));
                        total += 1;
                        Ok(true)
                    })?;
                }
                "day" => {
                    let base = Key::new().u32(a).u32(w);
                    let lo = base.clone().u32((query.from_ms / DAY_MS) as u32);
                    let hi = base.clone().u32((query.to_ms / DAY_MS) as u32 + 1);
                    for item in days.range(lo.as_slice()..hi.as_slice())? {
                        if total >= limit {
                            truncated = true;
                            break;
                        }
                        let (k, v) = item?;
                        points.push(rollup_point(db::be_u32(k.value(), 8) as u64 * DAY_MS, &Rollup::decode(v.value())?));
                        total += 1;
                    }
                }
                _ => {
                    visit_hours(&hours, (a, w), (query.from_ms / HOUR_MS, query.to_ms / HOUR_MS), |h, r| {
                        if total >= limit {
                            truncated = true;
                            return Ok(false);
                        }
                        points.push(rollup_point(h * HOUR_MS, &r));
                        total += 1;
                        Ok(true)
                    })?;
                }
            }
            if points.is_empty() {
                continue;
            }
            out.push(json!({
                "identity": ameta.identity, "accountId": ameta.account_id, "provider": ameta.provider, "plan": ameta.plan,
                "window": { "identity": wmeta.identity, "key": wmeta.key, "label": smeta.label, "kind": wmeta.kind, "unit": wmeta.unit, "poolId": wmeta.pool_id, "poolLabel": smeta.pool_label },
                "points": points,
            }));
        }
        let columns = if query.resolution == "raw" {
            json!(["from", "lastSeen", "usedPercent", "remainingPercent", "used", "limit", "remaining", "resetAt", "cached"])
        } else {
            json!(["start", "consumed", "lastUsed", "maxUsed", "coveredMinutes", "readings", "resets", "creditsSpent", "creditsAdded"])
        };
        Ok(json!({ "resolution": query.resolution, "columns": columns, "series": out, "truncated": truncated }))
    })
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SeriesListQuery {
    pub identities: Option<Vec<String>>,
    pub account_ids: Option<Vec<String>>,
    pub provider: Option<String>,
    pub window_key: Option<String>,
    pub from_ms: u64,
    pub to_ms: u64,
}

/// The series seen between two times, from series metadata alone: no point limit applies.
pub fn series_list(engine: &Engine, query: &SeriesListQuery) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let accounts = read.open_table(db::ACCOUNT)?;
        let windows = read.open_table(db::WINDOW)?;
        let mut out = Vec::new();
        let selected = select_series(
            &read,
            query.identities.as_deref(),
            query.account_ids.as_deref(),
            query.provider.as_deref(),
            query.window_key.as_deref(),
        )?;
        for ((a, w), value) in selected {
            let smeta: SeriesMeta = db::unpack(&value)?;
            if (smeta.last_ms as u64) < query.from_ms || (smeta.first_ms.max(0) as u64) > query.to_ms {
                continue;
            }
            let Some(av) = accounts.get(Key::new().u32(a).as_slice())? else { continue };
            let ameta: AccountMeta = db::unpack(av.value())?;
            let Some(wv) = windows.get(Key::new().u32(w).as_slice())? else { continue };
            let wmeta: WindowMeta = db::unpack(wv.value())?;
            if query.identities.as_ref().map(|ids| !ids.contains(&ameta.identity)).unwrap_or(false)
                || query.account_ids.as_ref().map(|ids| !ids.contains(&ameta.account_id)).unwrap_or(false)
                || query.provider.as_ref().map(|p| *p != ameta.provider).unwrap_or(false)
                || query.window_key.as_ref().map(|k| *k != wmeta.key).unwrap_or(false)
            {
                continue;
            }
            out.push(json!({
                "identity": ameta.identity, "accountId": ameta.account_id, "provider": ameta.provider, "plan": ameta.plan,
                "window": { "identity": wmeta.identity, "key": wmeta.key, "label": smeta.label, "kind": wmeta.kind, "unit": wmeta.unit, "poolId": wmeta.pool_id, "poolLabel": smeta.pool_label },
            }));
        }
        Ok(Json::Array(out))
    })
}

/// Every series' reading at time `t` (a reading stays valid 10 minutes past its last sighting).
pub fn usage_at(engine: &Engine, t: u64) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let accounts = read.open_table(db::ACCOUNT)?;
        let windows = read.open_table(db::WINDOW)?;
        let series_table = read.open_table(db::SERIES)?;
        let readings = read.open_table(db::READING_DAY)?;
        let open = open_spans(&read)?;
        let mut out = Vec::new();
        for item in series_table.iter()? {
            let (k, v) = item?;
            let (a, w) = (db::be_u32(k.value(), 0), db::be_u32(k.value(), 4));
            let smeta: SeriesMeta = db::unpack(v.value())?;
            let Some(av) = accounts.get(Key::new().u32(a).as_slice())? else { continue };
            let ameta: AccountMeta = db::unpack(av.value())?;
            let Some(wv) = windows.get(Key::new().u32(w).as_slice())? else { continue };
            let wmeta: WindowMeta = db::unpack(wv.value())?;
            let reading = span_at(&readings, &open, (a, w), t)?.filter(|(_, r)| t <= r.last_seen + COVER_GAP_MS);
            out.push(json!({
                "identity": ameta.identity, "accountId": ameta.account_id, "provider": ameta.provider,
                "window": { "key": wmeta.key, "label": smeta.label, "kind": wmeta.kind, "unit": wmeta.unit, "poolId": wmeta.pool_id },
                "reading": reading.map(|(from, r)| json!({
                    "from": from, "lastSeen": r.last_seen, "usedPercent": r.vals.used_pct, "remainingPercent": r.vals.remaining_pct,
                    "used": r.vals.used, "limit": r.vals.limit, "remaining": r.vals.remaining, "resetAt": r.vals.reset_at, "cached": r.vals.cached,
                })),
            }));
        }
        Ok(json!({ "at": t, "series": out }))
    })
}

/// Reset periods of the series in a time range (newest last).
pub fn periods(engine: &Engine, series: &[SeriesRef], from: u64, to: u64) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let rev = read.open_table(db::DICT_REV)?;
        let table = read.open_table(db::PERIOD)?;
        let mut out = Vec::new();
        for s in series {
            let (Some(a), Some(w)) = (db::lookup_id(&rev, db::KIND_ACCOUNT, &s.identity)?, db::lookup_id(&rev, db::KIND_WINDOW, &s.window)?) else {
                out.push(json!({ "identity": s.identity, "window": s.window, "periods": [] }));
                continue;
            };
            let base = Key::new().u32(a).u32(w);
            let hi = base.clone().u64(to.saturating_add(1));
            let mut list = Vec::new();
            for item in table.range(base.as_slice()..hi.as_slice())? {
                let (k, v) = item?;
                let p: Period = db::unpack(v.value())?;
                if (p.end_ms as u64) >= from {
                    list.push(json!({ "start": db::be_u64(k.value(), 8), "end": p.end_ms, "resetAt": p.reset_at_ms, "startUsed": p.start_used, "endUsed": p.end_used, "maxUsed": p.max_used, "consumed": round2(p.consumed), "hitLimitAt": p.hit_limit_ms, "final": p.is_final }));
                }
            }
            out.push(json!({ "identity": s.identity, "window": s.window, "periods": list }));
        }
        Ok(Json::Array(out))
    })
}

/// Size figures for budgets: rows per table.
pub fn counts(engine: &Engine) -> Result<Json> {
    engine.with_db(|db| {
        let read = db.begin_read()?;
        let mut out = serde_json::Map::new();
        for (name, def) in [
            ("account_state", db::ACCOUNT_STATE),
            ("period", db::PERIOD),
            ("rollup_day", db::ROLLUP_DAY),
            ("series", db::SERIES),
        ] {
            out.insert(name.into(), json!(read.open_table(def)?.len()?));
        }
        // Spans and hourly rollups are counted inside their day values; every open span is one
        // reading too.
        let days = read.open_table(db::READING_DAY)?;
        let mut spans = read.open_table(db::LATEST)?.len()?;
        for item in days.iter()? {
            spans += pack::span_count(item?.1.value())?;
        }
        out.insert("reading".into(), json!(spans));
        out.insert("reading_days".into(), json!(days.len()?));
        let hours = read.open_table(db::ROLLUP_HOURS)?;
        let mut hour_rows = 0u64;
        for item in hours.iter()? {
            hour_rows += pack::hour_count(item?.1.value())? as u64;
        }
        out.insert("rollup_hour".into(), json!(hour_rows));
        out.insert("rollup_hour_days".into(), json!(hours.len()?));
        // Where the file's bytes go: stored row bytes, B-tree metadata, unused space in pages.
        let mut tables = serde_json::Map::new();
        for (name, def) in [
            ("reading_day", db::READING_DAY),
            ("latest", db::LATEST),
            ("account_state", db::ACCOUNT_STATE),
            ("period", db::PERIOD),
            ("rollup_hours", db::ROLLUP_HOURS),
            ("rollup_day", db::ROLLUP_DAY),
        ] {
            let stats = read.open_table(def)?.stats()?;
            tables.insert(
                name.into(),
                json!([
                    stats.stored_bytes(),
                    stats.metadata_bytes(),
                    stats.fragmented_bytes(),
                    stats.leaf_pages()
                ]),
            );
        }
        out.insert("tableBytes".into(), Json::Object(tables));
        Ok(Json::Object(out))
    })
}
