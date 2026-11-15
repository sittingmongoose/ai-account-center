//! Packed day values (docs/store.md 3.1): one redb value per series and UTC day holds that day's
//! closed reading spans, or its hourly rollups. Every field round-trips bit for bit.

use crate::engine::DAY_MS;
use crate::error::Result;
use crate::fail;
use crate::usage::{Rollup, Row, Vals};

const VERSION: u8 = 1;
/// Whole and hundredth numbers below this are stored as integers.
const SMALL_LIMIT: i64 = 1 << 40;

pub fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push((v as u8) | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn zigzag(v: i64) -> u64 {
    ((v << 1) ^ (v >> 63)) as u64
}

fn unzigzag(v: u64) -> i64 {
    ((v >> 1) as i64) ^ -((v & 1) as i64)
}

pub fn put_svarint(out: &mut Vec<u8>, v: i64) {
    put_varint(out, zigzag(v));
}

/// A strict reader: running off the end is corruption, never a short answer.
pub struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Cursor<'a> {
    pub fn new(bytes: &'a [u8]) -> Cursor<'a> {
        Cursor { bytes, at: 0 }
    }

    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let Some(end) = self.at.checked_add(n).filter(|end| *end <= self.bytes.len()) else {
            fail!("corrupt", "packed value is short");
        };
        let slice = &self.bytes[self.at..end];
        self.at = end;
        Ok(slice)
    }

    pub fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    pub fn varint(&mut self) -> Result<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = self.u8()?;
            let bits = (byte & 0x7f) as u64;
            if shift == 63 && bits > 1 {
                fail!("corrupt", "packed integer overflows");
            }
            value |= bits << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
        }
        fail!("corrupt", "packed integer is too long")
    }

    pub fn svarint(&mut self) -> Result<i64> {
        Ok(unzigzag(self.varint()?))
    }

    pub fn f32(&mut self) -> Result<f32> {
        Ok(f32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn f64(&mut self) -> Result<f64> {
        Ok(f64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn finish(&self) -> Result<()> {
        if self.at != self.bytes.len() {
            fail!("corrupt", "packed value has trailing bytes");
        }
        Ok(())
    }
}

const F_NONE: u64 = 0;
const F_RAW: u64 = 1;
const F_COMPLEMENT: u64 = 2;
const F_FIRST_NUMBER: u64 = 3;

/// An optional f64: absent, raw, `100 - used`, or a whole or hundredth number in one varint.
fn put_float(out: &mut Vec<u8>, value: Option<f64>, complement_of: Option<f64>) {
    let Some(v) = value else {
        put_varint(out, F_NONE);
        return;
    };
    if complement_of.is_some_and(|u| (100.0 - u).to_bits() == v.to_bits()) {
        put_varint(out, F_COMPLEMENT);
        return;
    }
    if v.abs() < SMALL_LIMIT as f64 {
        let whole = v as i64;
        if (whole as f64).to_bits() == v.to_bits() {
            put_varint(out, F_FIRST_NUMBER + (zigzag(whole) << 1));
            return;
        }
        let cents = (v * 100.0).round() as i64;
        if (cents as f64 / 100.0).to_bits() == v.to_bits() {
            put_varint(out, F_FIRST_NUMBER + ((zigzag(cents) << 1) | 1));
            return;
        }
    }
    put_varint(out, F_RAW);
    out.extend_from_slice(&v.to_be_bytes());
}

fn read_float(c: &mut Cursor<'_>, complement_of: Option<f64>) -> Result<Option<f64>> {
    let tag = c.varint()?;
    Ok(match tag {
        F_NONE => None,
        F_RAW => Some(c.f64()?),
        F_COMPLEMENT => match complement_of {
            Some(u) => Some(100.0 - u),
            None => fail!("corrupt", "packed complement without its value"),
        },
        _ => {
            let n = tag - F_FIRST_NUMBER;
            let number = unzigzag(n >> 1) as f64;
            Some(if n & 1 == 1 { number / 100.0 } else { number })
        }
    })
}

/// An optional time: a presence byte, then its distance from `base`.
fn put_time(out: &mut Vec<u8>, value: Option<i64>, base: i64) {
    match value {
        None => out.push(0),
        Some(v) => {
            out.push(1);
            put_svarint(out, v.wrapping_sub(base));
        }
    }
}

fn read_time(c: &mut Cursor<'_>, base: i64) -> Result<Option<i64>> {
    Ok(match c.u8()? {
        0 => None,
        1 => Some(base.wrapping_add(c.svarint()?)),
        _ => fail!("corrupt", "packed time marker"),
    })
}

fn flags_of(v: &Vals) -> u8 {
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
    flags
}

fn same_f(a: Option<f64>, b: Option<f64>) -> bool {
    a.map(f64::to_bits) == b.map(f64::to_bits)
}

const C_FLAGS: u64 = 1 << 0;
const C_USED_PCT: u64 = 1 << 1;
const C_REMAINING_PCT: u64 = 1 << 2;
const C_USED: u64 = 1 << 3;
const C_LIMIT: u64 = 1 << 4;
const C_REMAINING: u64 = 1 << 5;
const C_RESET: u64 = 1 << 6;
const C_EXPIRES: u64 = 1 << 7;
const C_WINDOW_MINUTES: u64 = 1 << 8;
const C_CACHED_AT: u64 = 1 << 9;

/// Which fields differ from the span before.
fn changes(prev: &Vals, v: &Vals) -> u64 {
    let mut c = 0;
    let mut on = |bit: u64, differs: bool| {
        if differs {
            c |= bit;
        }
    };
    on(C_FLAGS, flags_of(prev) != flags_of(v));
    on(C_USED_PCT, !same_f(prev.used_pct, v.used_pct));
    on(C_REMAINING_PCT, !same_f(prev.remaining_pct, v.remaining_pct));
    on(C_USED, !same_f(prev.used, v.used));
    on(C_LIMIT, !same_f(prev.limit, v.limit));
    on(C_REMAINING, !same_f(prev.remaining, v.remaining));
    on(C_RESET, prev.reset_at != v.reset_at);
    on(C_EXPIRES, prev.expires_at != v.expires_at);
    on(C_WINDOW_MINUTES, !same_f(prev.window_minutes, v.window_minutes));
    on(C_CACHED_AT, prev.cached_sampled_at != v.cached_sampled_at);
    c
}

/// The closed reading spans of one series and day, oldest first. Each span stores its distance
/// from the one before, its length, and only the fields that changed.
pub fn encode_spans(day: u32, spans: &[(u64, Row)]) -> Result<Vec<u8>> {
    let mut out = Vec::with_capacity(16 + spans.len() * 16);
    out.push(VERSION);
    put_varint(&mut out, spans.len() as u64);
    let mut prev_t = day as u64 * DAY_MS;
    let mut prev = Vals::default();
    for (t, row) in spans {
        let (Some(step), Some(length)) = (t.checked_sub(prev_t), row.last_seen.checked_sub(*t)) else {
            fail!("invalid", "reading spans out of order");
        };
        put_varint(&mut out, step);
        put_varint(&mut out, length);
        let v = &row.vals;
        let c = changes(&prev, v);
        put_varint(&mut out, c);
        if c & C_FLAGS != 0 {
            out.push(flags_of(v));
        }
        if c & C_USED_PCT != 0 {
            put_float(&mut out, v.used_pct, None);
        }
        if c & C_REMAINING_PCT != 0 {
            put_float(&mut out, v.remaining_pct, v.used_pct);
        }
        if c & C_USED != 0 {
            put_float(&mut out, v.used, None);
        }
        if c & C_LIMIT != 0 {
            put_float(&mut out, v.limit, None);
        }
        if c & C_REMAINING != 0 {
            put_float(&mut out, v.remaining, None);
        }
        let start = *t as i64;
        if c & C_RESET != 0 {
            put_time(&mut out, v.reset_at, prev.reset_at.unwrap_or(start));
        }
        if c & C_EXPIRES != 0 {
            put_time(&mut out, v.expires_at, prev.expires_at.unwrap_or(start));
        }
        if c & C_WINDOW_MINUTES != 0 {
            put_float(&mut out, v.window_minutes, None);
        }
        if c & C_CACHED_AT != 0 {
            put_time(&mut out, v.cached_sampled_at, prev.cached_sampled_at.unwrap_or(start));
        }
        prev_t = *t;
        prev = v.clone();
    }
    Ok(out)
}

pub fn decode_spans(day: u32, bytes: &[u8]) -> Result<Vec<(u64, Row)>> {
    let mut c = Cursor::new(bytes);
    if c.u8()? != VERSION {
        fail!("corrupt", "reading day version");
    }
    let count = c.varint()?;
    // Every span takes at least three bytes, so a damaged count cannot reserve much.
    let mut spans = Vec::with_capacity((count as usize).min(bytes.len() / 3));
    let mut t = day as u64 * DAY_MS;
    let mut v = Vals::default();
    for _ in 0..count {
        let Some(next) = t.checked_add(c.varint()?) else {
            fail!("corrupt", "reading day time");
        };
        t = next;
        let Some(last_seen) = t.checked_add(c.varint()?) else {
            fail!("corrupt", "reading day time");
        };
        let changed = c.varint()?;
        if changed & C_FLAGS != 0 {
            let flags = c.u8()?;
            v.cached = flags & 1 != 0;
            v.unlimited = (flags & 2 != 0).then_some(flags & 4 != 0);
            v.enabled = (flags & 8 != 0).then_some(flags & 16 != 0);
        }
        if changed & C_USED_PCT != 0 {
            v.used_pct = read_float(&mut c, None)?;
        }
        if changed & C_REMAINING_PCT != 0 {
            v.remaining_pct = read_float(&mut c, v.used_pct)?;
        }
        if changed & C_USED != 0 {
            v.used = read_float(&mut c, None)?;
        }
        if changed & C_LIMIT != 0 {
            v.limit = read_float(&mut c, None)?;
        }
        if changed & C_REMAINING != 0 {
            v.remaining = read_float(&mut c, None)?;
        }
        let start = t as i64;
        if changed & C_RESET != 0 {
            v.reset_at = read_time(&mut c, v.reset_at.unwrap_or(start))?;
        }
        if changed & C_EXPIRES != 0 {
            v.expires_at = read_time(&mut c, v.expires_at.unwrap_or(start))?;
        }
        if changed & C_WINDOW_MINUTES != 0 {
            v.window_minutes = read_float(&mut c, None)?;
        }
        if changed & C_CACHED_AT != 0 {
            v.cached_sampled_at = read_time(&mut c, v.cached_sampled_at.unwrap_or(start))?;
        }
        spans.push((
            t,
            Row {
                vals: v.clone(),
                last_seen,
            },
        ));
    }
    c.finish()?;
    Ok(spans)
}

/// How many spans a day value holds, without decoding them.
pub fn span_count(bytes: &[u8]) -> Result<u64> {
    let mut c = Cursor::new(bytes);
    if c.u8()? != VERSION {
        fail!("corrupt", "reading day version");
    }
    c.varint()
}

/// One series' hourly rollups of a day: a 24-bit mask of the hours present, then their rollups.
/// `hours` holds (hour of the day, rollup), in order.
pub fn encode_hours(hours: &[(u8, Rollup)]) -> Vec<u8> {
    let mut mask = 0u32;
    for (hour, _) in hours {
        mask |= 1 << hour;
    }
    let mut out = Vec::with_capacity(4 + hours.len() * 16);
    out.push(VERSION);
    out.extend_from_slice(&mask.to_be_bytes()[1..]);
    for (_, rollup) in hours {
        rollup.encode_into(&mut out);
    }
    out
}

pub fn decode_hours(bytes: &[u8]) -> Result<Vec<(u8, Rollup)>> {
    let mut c = Cursor::new(bytes);
    if c.u8()? != VERSION {
        fail!("corrupt", "rollup day version");
    }
    let m = c.take(3)?;
    let mask = u32::from_be_bytes([0, m[0], m[1], m[2]]);
    if mask >> 24 != 0 {
        fail!("corrupt", "rollup day mask");
    }
    let mut hours = Vec::with_capacity(mask.count_ones() as usize);
    for hour in 0..24u8 {
        if mask & (1 << hour) != 0 {
            hours.push((hour, Rollup::decode_from(&mut c)?));
        }
    }
    c.finish()?;
    Ok(hours)
}

/// How many hours a rollup day value holds.
pub fn hour_count(bytes: &[u8]) -> Result<u32> {
    let mut c = Cursor::new(bytes);
    if c.u8()? != VERSION {
        fail!("corrupt", "rollup day version");
    }
    let m = c.take(3)?;
    Ok(u32::from_be_bytes([0, m[0], m[1], m[2]]).count_ones())
}

/// An f32 percentage: a whole number in one varint, or a marker and its four bytes.
pub fn put_small_f32(out: &mut Vec<u8>, value: f64) {
    let v = value as f32;
    if (0.0..16_777_216.0).contains(&v) {
        let whole = v as u32;
        if (whole as f32).to_bits() == v.to_bits() {
            put_varint(out, (whole as u64) << 1);
            return;
        }
    }
    put_varint(out, 1);
    out.extend_from_slice(&v.to_be_bytes());
}

pub fn read_small_f32(c: &mut Cursor<'_>) -> Result<f64> {
    let tag = c.varint()?;
    Ok(match tag {
        1 => c.f32()? as f64,
        _ if tag & 1 == 0 => (tag >> 1) as f32 as f64,
        _ => fail!("corrupt", "packed percentage marker"),
    })
}

pub fn put_plain_float(out: &mut Vec<u8>, value: f64) {
    put_float(out, Some(value), None);
}

pub fn read_plain_float(c: &mut Cursor<'_>) -> Result<f64> {
    match read_float(c, None)? {
        Some(v) => Ok(v),
        None => fail!("corrupt", "packed number is absent"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A small deterministic generator; the crate takes no random-number dependency.
    struct Xorshift(u64);

    impl Xorshift {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    fn float(g: &mut Xorshift) -> Option<f64> {
        match g.below(10) {
            0 => None,
            1 => Some(f64::from_bits(g.next())),
            2 => Some(-0.0),
            3 => Some(g.below(10_000) as f64 / 100.0),
            4 => Some(g.below(1000) as f64 / 7.0),
            5 => Some(-(g.below(5000) as f64)),
            6 => Some(1e300),
            7 => Some(2f64.powi(41) + 1.0),
            _ => Some(g.below(101) as f64),
        }
    }

    fn time(g: &mut Xorshift) -> Option<i64> {
        match g.below(6) {
            0 => None,
            1 => Some(g.next() as i64),
            2 => Some(i64::MIN),
            3 => Some(i64::MAX),
            _ => Some(1_790_000_000_000 + g.below(1_000_000_000) as i64),
        }
    }

    fn vals(g: &mut Xorshift) -> Vals {
        let used_pct = float(g);
        Vals {
            cached: g.below(2) == 0,
            unlimited: [None, Some(false), Some(true)][g.below(3) as usize],
            enabled: [None, Some(false), Some(true)][g.below(3) as usize],
            used_pct,
            remaining_pct: if g.below(2) == 0 {
                used_pct.map(|u| 100.0 - u)
            } else {
                float(g)
            },
            used: float(g),
            limit: float(g),
            remaining: float(g),
            reset_at: time(g),
            expires_at: time(g),
            window_minutes: float(g),
            cached_sampled_at: time(g),
        }
    }

    /// `Row` equality that treats NaN bits as equal, as `Vals::same` does.
    fn bits(row: &Row) -> Vec<Option<u64>> {
        let v = &row.vals;
        [
            v.used_pct,
            v.remaining_pct,
            v.used,
            v.limit,
            v.remaining,
            v.window_minutes,
        ]
        .iter()
        .map(|f| f.map(f64::to_bits))
        .chain(
            [v.reset_at, v.expires_at, v.cached_sampled_at]
                .iter()
                .map(|t| t.map(|x| x as u64)),
        )
        .chain([Some(flags_of(v) as u64), Some(row.last_seen)])
        .collect()
    }

    #[test]
    fn spans_round_trip_bit_for_bit() {
        let mut g = Xorshift(0x9e37_79b9_7f4a_7c15);
        let day = 20_000u32;
        for _ in 0..400 {
            let mut t = day as u64 * DAY_MS;
            let mut spans = Vec::new();
            let mut v = vals(&mut g);
            for _ in 0..g.below(40) {
                t += g.below(4_000_000);
                if g.below(3) == 0 {
                    v = vals(&mut g);
                }
                spans.push((
                    t,
                    Row {
                        vals: v.clone(),
                        last_seen: t + g.below(600_000),
                    },
                ));
            }
            let bytes = encode_spans(day, &spans).unwrap();
            assert_eq!(span_count(&bytes).unwrap(), spans.len() as u64);
            let back = decode_spans(day, &bytes).unwrap();
            assert_eq!(back.len(), spans.len());
            for (a, b) in spans.iter().zip(&back) {
                assert_eq!(a.0, b.0);
                assert_eq!(bits(&a.1), bits(&b.1));
            }
        }
    }

    #[test]
    fn a_damaged_value_is_refused_or_read_without_panicking() {
        let mut g = Xorshift(77);
        let day = 20_000u32;
        let t = day as u64 * DAY_MS + 5;
        let spans = vec![
            (
                t,
                Row {
                    vals: vals(&mut g),
                    last_seen: t + 9,
                },
            ),
            (
                t + 60_000,
                Row {
                    vals: vals(&mut g),
                    last_seen: t + 70_000,
                },
            ),
        ];
        let bytes = encode_spans(day, &spans).unwrap();
        for cut in 0..bytes.len() {
            assert!(decode_spans(day, &bytes[..cut]).is_err(), "cut at {cut}");
        }
        for i in 0..bytes.len() {
            for bit in 0..8 {
                let mut bad = bytes.clone();
                bad[i] ^= 1 << bit;
                let _ = decode_spans(day, &bad);
            }
        }
        let mut long = bytes.clone();
        long.push(0);
        assert!(decode_spans(day, &long).is_err());
    }

    #[test]
    fn out_of_order_spans_are_refused() {
        let day = 20_000u32;
        let t = day as u64 * DAY_MS + 1000;
        let row = |last_seen| Row {
            vals: Vals::default(),
            last_seen,
        };
        assert!(encode_spans(day, &[(t, row(t)), (t - 1, row(t))]).is_err());
        assert!(encode_spans(day, &[(t, row(t - 1))]).is_err());
        assert!(encode_spans(day, &[(t - DAY_MS, row(t))]).is_err());
    }

    #[test]
    fn hours_round_trip_with_f32_percentages() {
        let mut g = Xorshift(4242);
        for _ in 0..300 {
            let mut hours = Vec::new();
            for hour in 0..24u8 {
                if g.below(3) == 0 {
                    continue;
                }
                let used = g.below(1000) as f32 as f64 / 8.0;
                let mut r = Rollup {
                    consumed: (g.below(1000) as f32 / 3.0) as f64,
                    credits_spent: if g.below(4) == 0 {
                        g.below(100_000) as f64 / 7.0
                    } else {
                        0.0
                    },
                    credits_added: if g.below(9) == 0 { 12.5 } else { 0.0 },
                    covered_ms: g.below(3_600_001),
                    rows: g.below(70) as u32,
                    resets: g.below(2) as u32,
                    late_rows: g.below(2) as u32,
                    last_reset_ms: if g.below(5) == 0 { Some(g.next() as i64) } else { None },
                    ..Default::default()
                };
                if g.below(4) != 0 {
                    r.first_used = Some(used);
                    r.last_used = Some(100.0);
                    r.min_used = Some(0.0);
                    r.max_used = Some(33.25);
                }
                hours.push((hour, r));
            }
            let bytes = encode_hours(&hours);
            assert_eq!(hour_count(&bytes).unwrap(), hours.len() as u32);
            assert_eq!(decode_hours(&bytes).unwrap(), hours);
        }
    }
}
