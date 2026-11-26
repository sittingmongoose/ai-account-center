//! Compact typed prior fingerprints for incremental and settled scans (R4).
//!
//! Replaces `serde_json::Map<String, Value>` trees for incoming fingerprints and `settledPacked`
//! checkpoints:
//! - 64-hex SHA-256 digests and `device:inode` identities are stored inline without heap allocation.
//! - Matching `appendState` (`head`/`tail` equal to the owning print) is block-compressed with
//!   zstd into 64 KiB chunks inside `PriorKindMap`, eliminating 40k+ per-entry heap allocations.
//! - Prints live in a chunked slab (`PriorKindMap`, 192 B/entry including key) indexed by a
//!   4-byte-per-slot open-addressed `CompactShaIndex`, avoiding `HashMap<[u8; 32], u32>` bloat.

use base64::engine::general_purpose::STANDARD_NO_PAD;
use serde::de::{self, DeserializeSeed, IgnoredAny, MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};
use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::io::{self, BufReader, Cursor, Read};

/// Maximum uncompressed JSON payload admitted from `settledPacked`.
pub const MAX_SETTLED_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_SHIFT: u32 = 12;
const CHUNK_SIZE: usize = 1 << CHUNK_SHIFT;
const CHUNK_MASK: usize = CHUNK_SIZE - 1;

const NO_APPEND_BLOCK: u16 = u16::MAX;
const MAX_APPEND_BLOCK: usize = (u16::MAX as usize) - 1;
const APPEND_BLOCK_CAP: usize = u16::MAX as usize;

const FLAG_HAS_SIZE: u32 = 1 << 0;
const FLAG_HAS_MTIME: u32 = 1 << 1;
const FLAG_HAS_NEWEST_USAGE_KEY: u32 = 1 << 2;
const FLAG_HAS_NEWEST_USAGE_VAL: u32 = 1 << 3;
const FLAG_HAS_SETTLED_SLOT: u32 = 1 << 4;
const FLAG_USAGE_SKIPPED: u32 = 1 << 5;
const FLAG_UNREAD_USAGE: u32 = 1 << 6;
const FLAG_ESTIMATED_CLOCK: u32 = 1 << 7;
const FLAG_OPTIONAL_ROOT: u32 = 1 << 8;
const FLAG_WAL_UNREAD: u32 = 1 << 9;
const FLAG_HAS_CLAUDE_MIRROR: u32 = 1 << 10;
const FLAG_CLAUDE_MIRROR_VAL: u32 = 1 << 11;
const FLAG_HAS_USAGE_SKIPPED: u32 = 1 << 12;
const FLAG_HAS_UNREAD_USAGE: u32 = 1 << 13;
const FLAG_HAS_ESTIMATED_CLOCK: u32 = 1 << 14;
const FLAG_HAS_OPTIONAL_ROOT: u32 = 1 << 15;
const FLAG_HAS_WAL_UNREAD: u32 = 1 << 16;
const FLAG_HEAD_SHA: u32 = 1 << 17;
const FLAG_TAIL_SHA: u32 = 1 << 18;
const FLAG_PATH_KEY_SHA: u32 = 1 << 19;
const FLAG_FILE_ID_PAIR: u32 = 1 << 20;
const FLAG_HAS_READ_THROUGH_KEY: u32 = 1 << 21;
const FLAG_KEY_OTHER: u32 = 1 << 22;
const FLAG_FILE_DEV_RARE: u32 = 1 << 23;
const FLAG_SETTLED_SLOT_RARE: u32 = 1 << 24;

/// Compact representation of a 64-char lowercase hex SHA-256 digest (32 bytes inline, zero heap
/// allocation), falling back to a boxed string for non-SHA-256 fixture/legacy values.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CompactHash {
    Sha256([u8; 32]),
    Other(Box<str>),
}

impl CompactHash {
    pub fn from_text(text: &str) -> Self {
        match decode_sha256_hex(text) {
            Some(bytes) => Self::Sha256(bytes),
            None => Self::Other(text.into()),
        }
    }

    pub fn as_sha256(&self) -> Option<&[u8; 32]> {
        match self {
            Self::Sha256(bytes) => Some(bytes),
            Self::Other(_) => None,
        }
    }

    pub fn eq_str(&self, text: &str) -> bool {
        match self {
            Self::Sha256(bytes) => sha256_eq_str(bytes, text),
            Self::Other(other) => other.as_ref() == text,
        }
    }

    pub fn is_empty(&self) -> bool {
        match self {
            Self::Sha256(_) => false,
            Self::Other(s) => s.is_empty(),
        }
    }

    pub fn to_hex_string(&self) -> String {
        match self {
            Self::Sha256(bytes) => encode_sha256_string(bytes),
            Self::Other(s) => s.to_string(),
        }
    }
}

impl PartialOrd for CompactHash {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for CompactHash {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        match (self, other) {
            (Self::Sha256(a), Self::Sha256(b)) => a.cmp(b),
            (Self::Other(a), Self::Other(b)) => a.cmp(b),
            (Self::Sha256(a), Self::Other(b)) => {
                let buf = encode_sha256_bytes(a);
                let text = std::str::from_utf8(&buf).unwrap_or("");
                text.cmp(b.as_ref())
            }
            (Self::Other(a), Self::Sha256(b)) => {
                let buf = encode_sha256_bytes(b);
                let text = std::str::from_utf8(&buf).unwrap_or("");
                a.as_ref().cmp(text)
            }
        }
    }
}

impl Serialize for CompactHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Sha256(bytes) => {
                let buf = encode_sha256_bytes(bytes);
                let text = std::str::from_utf8(&buf).unwrap_or("");
                serializer.serialize_str(text)
            }
            Self::Other(text) => serializer.serialize_str(text),
        }
    }
}

impl<'de> Deserialize<'de> for CompactHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = CompactHash;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a hash string")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(CompactHash::from_text(v))
            }
        }
        deserializer.deserialize_str(V)
    }
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn decode_sha256_hex(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (slot, pair) in out.iter_mut().zip(bytes.as_chunks::<2>().0) {
        let hi = hex_nibble(pair[0])?;
        let lo = hex_nibble(pair[1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

fn sha256_eq_str(bytes: &[u8; 32], text: &str) -> bool {
    let raw = text.as_bytes();
    if raw.len() != 64 {
        return false;
    }
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for (i, &b) in bytes.iter().enumerate() {
        if raw[i * 2] != HEX[(b >> 4) as usize] || raw[i * 2 + 1] != HEX[(b & 0x0f) as usize] {
            return false;
        }
    }
    true
}

fn encode_sha256_bytes(bytes: &[u8; 32]) -> [u8; 64] {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = [0u8; 64];
    for (i, &b) in bytes.iter().enumerate() {
        out[i * 2] = HEX[(b >> 4) as usize];
        out[i * 2 + 1] = HEX[(b & 0x0f) as usize];
    }
    out
}

fn encode_sha256_string(bytes: &[u8; 32]) -> String {
    let buf = encode_sha256_bytes(bytes);
    String::from_utf8(buf.to_vec()).unwrap_or_default()
}

fn parse_canonical_file_id(text: &str) -> Option<(u64, u64)> {
    let (dev_s, ino_s) = text.split_once(':')?;
    let dev = u64::from_str_radix(dev_s, 16).ok()?;
    let ino = u64::from_str_radix(ino_s, 16).ok()?;
    if format!("{dev:x}:{ino:x}") == text {
        Some((dev, ino))
    } else {
        None
    }
}

/// Compact open-addressed `[u8; 32] -> u32` index storing only 4-byte slab indices.
/// Keys live in the slab (`PriorKindMap`), cutting index RAM by ~10x versus `HashMap<[u8; 32], u32>`.
#[derive(Clone, Debug, Default)]
pub(crate) struct CompactShaIndex {
    slots: Vec<u32>,
    len: usize,
    used: usize,
}

impl CompactShaIndex {
    const EMPTY: u32 = u32::MAX;
    const TOMBSTONE: u32 = u32::MAX - 1;

    pub fn with_capacity(entries: usize) -> Self {
        if entries == 0 {
            return Self::default();
        }
        let needed = (entries.saturating_mul(10) / 7).saturating_add(1);
        let cap = needed.max(16).next_power_of_two();
        Self {
            slots: vec![Self::EMPTY; cap],
            len: 0,
            used: 0,
        }
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    fn hash(sha: &[u8; 32]) -> usize {
        let a = u64::from_ne_bytes(sha[0..8].try_into().unwrap_or([0; 8]));
        let b = u64::from_ne_bytes(sha[8..16].try_into().unwrap_or([0; 8]));
        let c = u64::from_ne_bytes(sha[16..24].try_into().unwrap_or([0; 8]));
        let d = u64::from_ne_bytes(sha[24..32].try_into().unwrap_or([0; 8]));
        let mut h = a ^ b.rotate_left(17) ^ c.rotate_left(31) ^ d.rotate_left(47);
        h = h.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        (h ^ (h >> 32)) as usize
    }

    pub fn get<'a>(&self, sha: &[u8; 32], key_at: impl Fn(u32) -> Option<&'a [u8; 32]>) -> Option<u32> {
        if self.slots.is_empty() {
            return None;
        }
        let mask = self.slots.len() - 1;
        let mut pos = Self::hash(sha) & mask;
        loop {
            let slot = self.slots[pos];
            if slot == Self::EMPTY {
                return None;
            }
            if slot != Self::TOMBSTONE && key_at(slot) == Some(sha) {
                return Some(slot);
            }
            pos = (pos + 1) & mask;
        }
    }

    fn grow<'a>(&mut self, key_at: impl Fn(u32) -> Option<&'a [u8; 32]>) {
        let next_cap = if self.slots.is_empty() {
            16
        } else if (self.len.saturating_add(1)).saturating_mul(10) > self.slots.len().saturating_mul(7) {
            self.slots.len().saturating_mul(2)
        } else {
            self.slots.len()
        };
        let old = std::mem::replace(&mut self.slots, vec![Self::EMPTY; next_cap]);
        let mask = next_cap - 1;
        let mut count = 0usize;
        for slot in old {
            if slot == Self::EMPTY || slot == Self::TOMBSTONE {
                continue;
            }
            let Some(sha) = key_at(slot) else {
                continue;
            };
            let mut pos = Self::hash(sha) & mask;
            while self.slots[pos] != Self::EMPTY {
                pos = (pos + 1) & mask;
            }
            self.slots[pos] = slot;
            count += 1;
        }
        self.len = count;
        self.used = count;
    }

    pub fn insert<'a>(&mut self, sha: &[u8; 32], idx: u32, key_at: impl Fn(u32) -> Option<&'a [u8; 32]>) {
        if self.slots.is_empty() || (self.used + 1) * 10 > self.slots.len() * 7 {
            self.grow(&key_at);
        }
        let mask = self.slots.len() - 1;
        let mut pos = Self::hash(sha) & mask;
        let mut first_tombstone: Option<usize> = None;
        loop {
            let slot = self.slots[pos];
            if slot == Self::EMPTY {
                let target = first_tombstone.unwrap_or(pos);
                self.slots[target] = idx;
                self.len += 1;
                if first_tombstone.is_none() {
                    self.used += 1;
                }
                return;
            }
            if slot == Self::TOMBSTONE {
                if first_tombstone.is_none() {
                    first_tombstone = Some(pos);
                }
            } else if key_at(slot) == Some(sha) {
                self.slots[pos] = idx;
                return;
            }
            pos = (pos + 1) & mask;
        }
    }

    pub fn remove<'a>(&mut self, sha: &[u8; 32], key_at: impl Fn(u32) -> Option<&'a [u8; 32]>) -> bool {
        if self.slots.is_empty() {
            return false;
        }
        let mask = self.slots.len() - 1;
        let mut pos = Self::hash(sha) & mask;
        loop {
            let slot = self.slots[pos];
            if slot == Self::EMPTY {
                return false;
            }
            if slot != Self::TOMBSTONE && key_at(slot) == Some(sha) {
                self.slots[pos] = Self::TOMBSTONE;
                self.len -= 1;
                return true;
            }
            pos = (pos + 1) & mask;
        }
    }
}

/// Block-compressed store for `SameEnds` `appendState.parser` JSON slices (64 KiB zstd blocks).
#[derive(Clone, Debug, Default)]
struct AppendBlobStore {
    blocks: Vec<Box<[u8]>>,
    staging: Vec<u8>,
}

impl AppendBlobStore {
    fn push(&mut self, bytes: &[u8]) -> Option<(u16, u16, u16)> {
        if bytes.len() > APPEND_BLOCK_CAP {
            return None;
        }
        if !self.staging.is_empty() && self.staging.len() + bytes.len() > APPEND_BLOCK_CAP {
            self.flush_staging()?;
        }
        if self.blocks.len() >= MAX_APPEND_BLOCK {
            return None;
        }
        let block = self.blocks.len() as u16;
        let off = self.staging.len() as u16;
        self.staging.extend_from_slice(bytes);
        Some((block, off, bytes.len() as u16))
    }

    fn flush_staging(&mut self) -> Option<()> {
        if self.staging.is_empty() {
            return Some(());
        }
        if self.blocks.len() >= MAX_APPEND_BLOCK {
            return None;
        }
        let compressed = zstd::bulk::compress(&self.staging, 1).ok()?;
        self.blocks.push(compressed.into_boxed_slice());
        self.staging.clear();
        Some(())
    }

    fn finish(&mut self) {
        if self.staging.len() > 1024 && self.flush_staging().is_some() {
            self.staging = Vec::new();
        } else {
            self.staging.shrink_to_fit();
        }
    }

    fn get(&self, block: u16, off: u16, len: u16) -> Option<Vec<u8>> {
        let start = usize::from(off);
        let end = start.checked_add(usize::from(len))?;
        let idx = usize::from(block);
        if idx == self.blocks.len() {
            return self.staging.get(start..end).map(ToOwned::to_owned);
        }
        let compressed = self.blocks.get(idx)?;
        let raw = zstd::bulk::decompress(compressed, APPEND_BLOCK_CAP + 1).ok()?;
        raw.get(start..end).map(ToOwned::to_owned)
    }
}

/// Zero-allocation lenient JSON scalar visitors matching `Value::as_*` semantics.
struct LenientU64(Option<u64>);
impl<'de> Deserialize<'de> for LenientU64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientU64;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(LenientU64(Some(v)))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(LenientU64(u64::try_from(v).ok()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientU64(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientU64(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientU64(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientU64(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientU64(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientU64(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientI64(Option<i64>);
impl<'de> Deserialize<'de> for LenientI64 {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientI64;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(LenientI64(Some(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(LenientI64(i64::try_from(v).ok()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientI64(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientI64(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientI64(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientBool(Option<bool>);
impl<'de> Deserialize<'de> for LenientBool {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientBool;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(LenientBool(Some(v)))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientBool(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientBool(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientBool(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientHash(Option<CompactHash>);
impl<'de> Deserialize<'de> for LenientHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientHash;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(LenientHash(Some(CompactHash::from_text(v))))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientHash(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientHash(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientHash(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientHash(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientHash(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientHash(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientHash(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientFileId(Option<Result<(u64, u64), Box<str>>>);
impl<'de> Deserialize<'de> for LenientFileId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientFileId;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(LenientFileId(Some(match parse_canonical_file_id(v) {
                    Some(pair) => Ok(pair),
                    None => Err(v.into()),
                })))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientFileId(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientFileId(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientFileId(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientFileId(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientFileId(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientFileId(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientFileId(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientStr(Option<Box<str>>);
impl<'de> Deserialize<'de> for LenientStr {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientStr;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(LenientStr(Some(v.into())))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientStr(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientStr(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientStr(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[derive(Clone, Debug)]
pub struct FullAppendState {
    pub head: CompactHash,
    pub tail: CompactHash,
    pub parser_raw: Box<RawValue>,
}

/// Rare or non-slab `appendState` representation stored in `PriorRare`.
#[derive(Clone, Debug)]
pub enum CompactAppendState {
    SameEnds(Box<RawValue>),
    Full(Box<FullAppendState>),
    Raw(Box<RawValue>),
}

impl CompactAppendState {
    pub fn from_value(value: &Value) -> Option<Self> {
        if let Some(obj) = value.as_object()
            && obj.len() == 3
            && let (Some(h), Some(t), Some(p)) = (
                obj.get("head").and_then(Value::as_str),
                obj.get("tail").and_then(Value::as_str),
                obj.get("parser"),
            )
            && let Ok(parser_raw) = serde_json::value::to_raw_value(p)
        {
            return Some(Self::Full(Box::new(FullAppendState {
                head: CompactHash::from_text(h),
                tail: CompactHash::from_text(t),
                parser_raw,
            })));
        }
        Some(Self::Raw(serde_json::value::to_raw_value(value).ok()?))
    }

    pub fn head_tail_match(&self, owner: &PriorPrint, head: &str, tail: &str) -> bool {
        match self {
            Self::SameEnds(_) => owner.head_eq(head) && owner.tail_eq(tail),
            Self::Full(full) => full.head.eq_str(head) && full.tail.eq_str(tail),
            Self::Raw(raw) => serde_json::from_str::<Value>(raw.get())
                .ok()
                .is_some_and(|v| v["head"].as_str() == Some(head) && v["tail"].as_str() == Some(tail)),
        }
    }

    pub fn has_head_and_tail(&self) -> bool {
        match self {
            Self::SameEnds(_) | Self::Full(_) => true,
            Self::Raw(raw) => serde_json::from_str::<Value>(raw.get())
                .ok()
                .is_some_and(|v| v["head"].is_string() && v["tail"].is_string()),
        }
    }

    pub fn parser_value(&self) -> Value {
        match self {
            Self::SameEnds(parser_raw) => serde_json::from_str(parser_raw.get()).unwrap_or(Value::Null),
            Self::Full(full) => serde_json::from_str(full.parser_raw.get()).unwrap_or(Value::Null),
            Self::Raw(raw) => serde_json::from_str::<Value>(raw.get())
                .ok()
                .and_then(|mut v| v.as_object_mut().and_then(|m| m.remove("parser")))
                .unwrap_or(Value::Null),
        }
    }

    pub fn to_json(&self, owner: &PriorPrint) -> Value {
        match self {
            Self::SameEnds(parser_raw) => {
                let parser: Value = serde_json::from_str(parser_raw.get()).unwrap_or(Value::Null);
                json!({
                    "head": owner.head_string().unwrap_or_default(),
                    "tail": owner.tail_string().unwrap_or_default(),
                    "parser": parser,
                })
            }
            Self::Full(full) => {
                let parser: Value = serde_json::from_str(full.parser_raw.get()).unwrap_or(Value::Null);
                json!({
                    "head": full.head.to_hex_string(),
                    "tail": full.tail.to_hex_string(),
                    "parser": parser,
                })
            }
            Self::Raw(raw) => serde_json::from_str(raw.get()).unwrap_or(Value::Null),
        }
    }
}

/// Streaming parser for `"appendState"` that avoids allocating an outer `Box<RawValue>` or
/// `Box<FullAppendState>` in the standard `{head, tail, parser}` case.
enum ParsedAppendState {
    Standard {
        head: CompactHash,
        tail: CompactHash,
        parser: Box<RawValue>,
    },
    Raw(Box<RawValue>),
}

impl<'de> Deserialize<'de> for ParsedAppendState {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        enum HashOrValue {
            Hash(CompactHash),
            Other(Value),
        }
        impl<'de> Deserialize<'de> for HashOrValue {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                struct V;
                impl<'de> Visitor<'de> for V {
                    type Value = HashOrValue;
                    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                        f.write_str("a hash string or JSON value")
                    }
                    fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                        Ok(HashOrValue::Hash(CompactHash::from_text(v)))
                    }
                    fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                        Ok(HashOrValue::Other(Value::Bool(v)))
                    }
                    fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                        Ok(HashOrValue::Other(Value::from(v)))
                    }
                    fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                        Ok(HashOrValue::Other(Value::from(v)))
                    }
                    fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                        Ok(HashOrValue::Other(json!(v)))
                    }
                    fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                        Ok(HashOrValue::Other(Value::Null))
                    }
                    fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                        let v = Value::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                        Ok(HashOrValue::Other(v))
                    }
                    fn visit_map<A: MapAccess<'de>>(self, map: A) -> Result<Self::Value, A::Error> {
                        let v = Value::deserialize(de::value::MapAccessDeserializer::new(map))?;
                        Ok(HashOrValue::Other(v))
                    }
                }
                d.deserialize_any(V)
            }
        }

        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ParsedAppendState;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an appendState JSON value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut head: Option<CompactHash> = None;
                let mut tail: Option<CompactHash> = None;
                let mut parser: Option<Box<RawValue>> = None;
                let mut fallback: Option<Map<String, Value>> = None;

                let promote = |fallback: &mut Option<Map<String, Value>>,
                               head: &mut Option<CompactHash>,
                               tail: &mut Option<CompactHash>,
                               parser: &mut Option<Box<RawValue>>| {
                    let m = fallback.get_or_insert_with(Map::new);
                    if let Some(h) = head.take() {
                        m.insert("head".into(), Value::from(h.to_hex_string()));
                    }
                    if let Some(t) = tail.take() {
                        m.insert("tail".into(), Value::from(t.to_hex_string()));
                    }
                    if let Some(p) = parser.take() {
                        let v: Value = serde_json::from_str(p.get()).unwrap_or(Value::Null);
                        m.insert("parser".into(), v);
                    }
                };

                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    if let Some(fb) = fallback.as_mut() {
                        let val: Value = map.next_value()?;
                        fb.insert(key.into_owned(), val);
                        continue;
                    }
                    match key.as_ref() {
                        "head" if head.is_none() => match map.next_value::<HashOrValue>()? {
                            HashOrValue::Hash(h) => head = Some(h),
                            HashOrValue::Other(v) => {
                                promote(&mut fallback, &mut head, &mut tail, &mut parser);
                                if let Some(fb) = fallback.as_mut() {
                                    fb.insert("head".into(), v);
                                }
                            }
                        },
                        "tail" if tail.is_none() => match map.next_value::<HashOrValue>()? {
                            HashOrValue::Hash(t) => tail = Some(t),
                            HashOrValue::Other(v) => {
                                promote(&mut fallback, &mut head, &mut tail, &mut parser);
                                if let Some(fb) = fallback.as_mut() {
                                    fb.insert("tail".into(), v);
                                }
                            }
                        },
                        "parser" if parser.is_none() => {
                            parser = Some(map.next_value::<Box<RawValue>>()?);
                        }
                        _ => {
                            let val: Value = map.next_value()?;
                            promote(&mut fallback, &mut head, &mut tail, &mut parser);
                            if let Some(fb) = fallback.as_mut() {
                                fb.insert(key.into_owned(), val);
                            }
                        }
                    }
                }
                if fallback.is_none()
                    && let (Some(head), Some(tail), Some(parser)) = (head.take(), tail.take(), parser.take())
                {
                    return Ok(ParsedAppendState::Standard { head, tail, parser });
                }
                promote(&mut fallback, &mut head, &mut tail, &mut parser);
                let obj = Value::Object(fallback.unwrap_or_default());
                let raw = serde_json::value::to_raw_value(&obj).map_err(de::Error::custom)?;
                Ok(ParsedAppendState::Raw(raw))
            }

            fn visit_bool<E: de::Error>(self, v: bool) -> Result<Self::Value, E> {
                Ok(ParsedAppendState::Raw(
                    serde_json::value::to_raw_value(&v).map_err(de::Error::custom)?,
                ))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Self::Value, E> {
                Ok(ParsedAppendState::Raw(
                    serde_json::value::to_raw_value(&v).map_err(de::Error::custom)?,
                ))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Self::Value, E> {
                Ok(ParsedAppendState::Raw(
                    serde_json::value::to_raw_value(&v).map_err(de::Error::custom)?,
                ))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Self::Value, E> {
                Ok(ParsedAppendState::Raw(
                    serde_json::value::to_raw_value(&v).map_err(de::Error::custom)?,
                ))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(ParsedAppendState::Raw(
                    serde_json::value::to_raw_value(&v).map_err(de::Error::custom)?,
                ))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(ParsedAppendState::Raw(
                    serde_json::value::to_raw_value(&Value::Null).map_err(de::Error::custom)?,
                ))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, seq: A) -> Result<Self::Value, A::Error> {
                let v = Value::deserialize(de::value::SeqAccessDeserializer::new(seq))?;
                Ok(ParsedAppendState::Raw(
                    serde_json::value::to_raw_value(&v).map_err(de::Error::custom)?,
                ))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Rarely populated fingerprint fields, heap-allocated only when present.
#[derive(Clone, Debug, Default)]
pub struct PriorRare {
    pub key_str: Option<Box<str>>,
    pub head_str: Option<Box<str>>,
    pub tail_str: Option<Box<str>>,
    pub path_key_str: Option<Box<str>>,
    pub file_id_str: Option<Box<str>>,
    pub file_id_dev_u64: u64,
    pub settled_checked_slot_u64: u64,
    pub append_state: Option<CompactAppendState>,
    pub claude_name: Option<CompactHash>,
    pub claude_group: Option<CompactHash>,
    pub wal_size: Option<u64>,
    pub wal_mtime_ms: Option<i64>,
    pub msg_through: Option<u64>,
    pub msg_head: Option<CompactHash>,
    pub read_through: Option<u64>,
    pub read_from: Option<u64>,
    pub read_state: Option<Box<RawValue>>,
    pub read_name: Option<Box<str>>,
    pub decoded_size: Option<u64>,
    pub unreadable_records: Option<u64>,
    pub oldest_unreadable_ms: Option<Option<i64>>,
    pub sqlite_min_date_ms: Option<i64>,
    pub unknown: Vec<(Box<str>, Box<RawValue>)>,
}

/// Compact typed prior fingerprint parsed directly from request JSON or `settledPacked` (160 B).
#[derive(Clone, Debug)]
pub struct PriorPrint {
    size: u64,
    mtime_ms: i64,
    newest_usage_ms: i64,
    file_id_ino: u64,
    rare: Option<Box<PriorRare>>,
    flags: u32,
    settled_checked_slot: u32,
    file_id_dev: u32,
    append_block: u16,
    append_off: u16,
    append_len: u16,
    head_sha: [u8; 32],
    tail_sha: [u8; 32],
    path_key_sha: [u8; 32],
}

impl Default for PriorPrint {
    fn default() -> Self {
        Self {
            size: 0,
            mtime_ms: 0,
            newest_usage_ms: 0,
            file_id_ino: 0,
            rare: None,
            flags: 0,
            settled_checked_slot: 0,
            file_id_dev: 0,
            append_block: NO_APPEND_BLOCK,
            append_off: 0,
            append_len: 0,
            head_sha: [0; 32],
            tail_sha: [0; 32],
            path_key_sha: [0; 32],
        }
    }
}

impl PriorPrint {
    fn rare_mut(&mut self) -> &mut PriorRare {
        self.rare.get_or_insert_with(|| Box::new(PriorRare::default()))
    }

    fn apply_settled_slot(&mut self, slot: u64) {
        self.flags |= FLAG_HAS_SETTLED_SLOT;
        if let Ok(small) = u32::try_from(slot) {
            self.settled_checked_slot = small;
            self.flags &= !FLAG_SETTLED_SLOT_RARE;
        } else {
            self.flags |= FLAG_SETTLED_SLOT_RARE;
            self.rare_mut().settled_checked_slot_u64 = slot;
        }
    }

    fn store_parsed_append(&mut self, parsed: ParsedAppendState, blobs: Option<&mut AppendBlobStore>) {
        match parsed {
            ParsedAppendState::Standard { head, tail, parser } => {
                if self.head_eq_hash(&head) && self.tail_eq_hash(&tail) {
                    if let Some(store) = blobs
                        && let Some((block, off, len)) = store.push(parser.get().as_bytes())
                    {
                        self.append_block = block;
                        self.append_off = off;
                        self.append_len = len;
                        return;
                    }
                    self.rare_mut().append_state = Some(CompactAppendState::SameEnds(parser));
                } else {
                    self.rare_mut().append_state = Some(CompactAppendState::Full(Box::new(FullAppendState {
                        head,
                        tail,
                        parser_raw: parser,
                    })));
                }
            }
            ParsedAppendState::Raw(raw) => {
                self.rare_mut().append_state = Some(CompactAppendState::Raw(raw));
            }
        }
    }

    fn head_eq_hash(&self, other: &CompactHash) -> bool {
        if self.flags & FLAG_HEAD_SHA != 0 {
            matches!(other, CompactHash::Sha256(b) if *b == self.head_sha)
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.head_str.as_deref())
                .is_some_and(|s| other.eq_str(s))
        }
    }

    fn tail_eq_hash(&self, other: &CompactHash) -> bool {
        if self.flags & FLAG_TAIL_SHA != 0 {
            matches!(other, CompactHash::Sha256(b) if *b == self.tail_sha)
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.tail_str.as_deref())
                .is_some_and(|s| other.eq_str(s))
        }
    }

    fn head_string(&self) -> Option<String> {
        if self.flags & FLAG_HEAD_SHA != 0 {
            Some(encode_sha256_string(&self.head_sha))
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.head_str.as_deref())
                .map(str::to_owned)
        }
    }

    fn tail_string(&self) -> Option<String> {
        if self.flags & FLAG_TAIL_SHA != 0 {
            Some(encode_sha256_string(&self.tail_sha))
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.tail_str.as_deref())
                .map(str::to_owned)
        }
    }

    pub fn size(&self) -> Option<u64> {
        (self.flags & FLAG_HAS_SIZE != 0).then_some(self.size)
    }

    pub fn mtime_ms(&self) -> Option<i64> {
        (self.flags & FLAG_HAS_MTIME != 0).then_some(self.mtime_ms)
    }

    pub fn has_newest_usage_key(&self) -> bool {
        self.flags & FLAG_HAS_NEWEST_USAGE_KEY != 0
    }

    pub fn newest_usage_ms(&self) -> Option<i64> {
        (self.flags & FLAG_HAS_NEWEST_USAGE_VAL != 0).then_some(self.newest_usage_ms)
    }

    pub fn settled_checked_slot(&self) -> Option<u64> {
        if self.flags & FLAG_HAS_SETTLED_SLOT == 0 {
            return None;
        }
        if self.flags & FLAG_SETTLED_SLOT_RARE != 0 {
            self.rare.as_deref().map(|r| r.settled_checked_slot_u64)
        } else {
            Some(u64::from(self.settled_checked_slot))
        }
    }

    pub fn head_eq(&self, value: &str) -> bool {
        if self.flags & FLAG_HEAD_SHA != 0 {
            sha256_eq_str(&self.head_sha, value)
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.head_str.as_deref())
                .is_some_and(|s| s == value)
        }
    }

    pub fn tail_eq(&self, value: &str) -> bool {
        if self.flags & FLAG_TAIL_SHA != 0 {
            sha256_eq_str(&self.tail_sha, value)
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.tail_str.as_deref())
                .is_some_and(|s| s == value)
        }
    }

    pub fn has_path_key(&self) -> bool {
        (self.flags & FLAG_PATH_KEY_SHA != 0) || self.rare.as_deref().is_some_and(|r| r.path_key_str.is_some())
    }

    pub(crate) fn path_key_sha(&self) -> Option<&[u8; 32]> {
        (self.flags & FLAG_PATH_KEY_SHA != 0).then_some(&self.path_key_sha)
    }

    pub fn path_key_hash(&self) -> Option<CompactHash> {
        if self.flags & FLAG_PATH_KEY_SHA != 0 {
            Some(CompactHash::Sha256(self.path_key_sha))
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.path_key_str.as_deref())
                .map(CompactHash::from_text)
        }
    }

    pub fn path_key_eq(&self, value: &str) -> bool {
        if self.flags & FLAG_PATH_KEY_SHA != 0 {
            sha256_eq_str(&self.path_key_sha, value)
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.path_key_str.as_deref())
                .is_some_and(|s| s == value)
        }
    }

    pub fn has_file_identity(&self) -> bool {
        (self.flags & FLAG_FILE_ID_PAIR != 0) || self.rare.as_deref().is_some_and(|r| r.file_id_str.is_some())
    }

    fn dev_u64(&self) -> u64 {
        if self.flags & FLAG_FILE_DEV_RARE != 0 {
            self.rare.as_deref().map_or(0, |r| r.file_id_dev_u64)
        } else {
            u64::from(self.file_id_dev)
        }
    }

    pub fn file_identity_pair(&self) -> Option<(u64, u64)> {
        if self.flags & FLAG_FILE_ID_PAIR != 0 {
            return Some((self.dev_u64(), self.file_id_ino));
        }
        let text = self.rare.as_deref()?.file_id_str.as_deref()?;
        let (dev, ino) = text.split_once(':')?;
        Some((u64::from_str_radix(dev, 16).ok()?, u64::from_str_radix(ino, 16).ok()?))
    }

    pub fn file_identity_eq_pair(&self, dev: u64, ino: u64) -> bool {
        if self.flags & FLAG_FILE_ID_PAIR != 0 {
            self.dev_u64() == dev && self.file_id_ino == ino
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.file_id_str.as_deref())
                .is_some_and(|s| s == format!("{dev:x}:{ino:x}"))
        }
    }

    pub fn file_identity_eq_str(&self, value: &str) -> bool {
        if self.flags & FLAG_FILE_ID_PAIR != 0 {
            parse_canonical_file_id(value) == Some((self.dev_u64(), self.file_id_ino))
        } else {
            self.rare
                .as_deref()
                .and_then(|r| r.file_id_str.as_deref())
                .is_some_and(|s| s == value)
        }
    }

    pub fn usage_skipped(&self) -> bool {
        self.flags & FLAG_USAGE_SKIPPED != 0
    }

    pub fn unread_usage(&self) -> bool {
        self.flags & FLAG_UNREAD_USAGE != 0
    }

    pub fn estimated_clock(&self) -> bool {
        self.flags & FLAG_ESTIMATED_CLOCK != 0
    }

    pub fn optional_root(&self) -> bool {
        self.flags & FLAG_OPTIONAL_ROOT != 0
    }

    pub fn wal_unread(&self) -> bool {
        self.flags & FLAG_WAL_UNREAD != 0
    }

    pub fn claude_group(&self) -> Option<&CompactHash> {
        self.rare
            .as_deref()
            .and_then(|r| r.claude_group.as_ref())
            .filter(|g| !g.is_empty())
    }

    pub fn wal_size(&self) -> Option<u64> {
        self.rare.as_deref().and_then(|r| r.wal_size)
    }

    pub fn wal_mtime_ms(&self) -> Option<i64> {
        self.rare.as_deref().and_then(|r| r.wal_mtime_ms)
    }

    pub fn msg_through(&self) -> Option<u64> {
        self.rare.as_deref().and_then(|r| r.msg_through)
    }

    pub fn msg_head(&self) -> Option<&CompactHash> {
        self.rare.as_deref().and_then(|r| r.msg_head.as_ref())
    }

    pub fn has_read_through_key(&self) -> bool {
        self.flags & FLAG_HAS_READ_THROUGH_KEY != 0
    }

    pub fn read_through(&self) -> Option<u64> {
        self.rare.as_deref().and_then(|r| r.read_through)
    }

    pub fn read_state_json(&self) -> Value {
        self.rare
            .as_deref()
            .and_then(|r| r.read_state.as_deref())
            .and_then(|raw| serde_json::from_str(raw.get()).ok())
            .unwrap_or(Value::Null)
    }

    pub fn read_name(&self) -> Option<&str> {
        self.rare.as_deref().and_then(|r| r.read_name.as_deref())
    }

    pub fn decoded_size(&self) -> Option<u64> {
        self.rare.as_deref().and_then(|r| r.decoded_size)
    }

    pub fn unreadable_records(&self) -> Option<u64> {
        self.rare.as_deref().and_then(|r| r.unreadable_records)
    }

    pub fn oldest_unreadable_ms_entry(&self) -> Option<Option<i64>> {
        self.rare.as_deref().and_then(|r| r.oldest_unreadable_ms)
    }

    pub fn sqlite_min_date_ms(&self) -> Option<i64> {
        self.rare.as_deref().and_then(|r| r.sqlite_min_date_ms)
    }

    /// Exact typed equivalent of `collector::settled_print`.
    pub fn is_settled(&self, cutoff: i64) -> bool {
        self.has_path_key()
            && self.mtime_ms().is_some_and(|mtime| mtime < cutoff)
            && self.newest_usage_ms().is_some_and(|usage| usage < cutoff)
            && !self.has_read_through_key()
            && !self.usage_skipped()
            && !self.unread_usage()
    }

    fn to_json_inner(&self, settled_slot_override: Option<u64>, append_override: Option<Value>) -> Value {
        let mut map = Map::new();
        if self.has_newest_usage_key() {
            map.insert(
                "newestUsageMs".into(),
                self.newest_usage_ms().map(Value::from).unwrap_or(Value::Null),
            );
        }
        if let Some(size) = self.size() {
            map.insert("size".into(), Value::from(size));
        }
        if let Some(mtime) = self.mtime_ms() {
            map.insert("mtimeMs".into(), Value::from(mtime));
        }
        if let Some(head) = self.head_string() {
            map.insert("head".into(), Value::from(head));
        }
        if let Some(tail) = self.tail_string() {
            map.insert("tail".into(), Value::from(tail));
        }
        if let Some(rare) = self.rare.as_deref()
            && let Some(name) = &rare.claude_name
        {
            map.insert("claudeName".into(), Value::from(name.to_hex_string()));
        }
        if self.flags & FLAG_HAS_CLAUDE_MIRROR != 0 {
            map.insert(
                "claudeMirror".into(),
                Value::from(self.flags & FLAG_CLAUDE_MIRROR_VAL != 0),
            );
        }
        if let Some(rare) = self.rare.as_deref()
            && let Some(group) = &rare.claude_group
        {
            map.insert("claudeGroup".into(), Value::from(group.to_hex_string()));
        }
        if self.flags & FLAG_HAS_USAGE_SKIPPED != 0 {
            map.insert("usageSkipped".into(), Value::from(self.usage_skipped()));
        }
        if let Some(rare) = self.rare.as_deref() {
            if let Some(size) = rare.wal_size {
                map.insert("walSize".into(), Value::from(size));
            }
            if let Some(mtime) = rare.wal_mtime_ms {
                map.insert("walMtimeMs".into(), Value::from(mtime));
            }
            if let Some(offset) = rare.msg_through {
                map.insert("msgThrough".into(), Value::from(offset));
            }
            if let Some(head) = &rare.msg_head {
                map.insert("msgHead".into(), Value::from(head.to_hex_string()));
            }
            if let Some(count) = rare.unreadable_records {
                map.insert("unreadableRecords".into(), Value::from(count));
            }
            if let Some(oldest) = rare.oldest_unreadable_ms {
                map.insert("oldestUnreadableMs".into(), json!(oldest));
            }
            if let Some(cutoff) = rare.sqlite_min_date_ms {
                map.insert("sqliteMinDateMs".into(), Value::from(cutoff));
            }
        }
        if self.flags & FLAG_HAS_WAL_UNREAD != 0 {
            map.insert("walUnread".into(), Value::from(self.wal_unread()));
        }
        if self.flags & FLAG_PATH_KEY_SHA != 0 {
            map.insert("pathKey".into(), Value::from(encode_sha256_string(&self.path_key_sha)));
        } else if let Some(s) = self.rare.as_deref().and_then(|r| r.path_key_str.as_deref()) {
            map.insert("pathKey".into(), Value::from(s));
        }
        if self.flags & FLAG_FILE_ID_PAIR != 0 {
            map.insert(
                "fileIdentity".into(),
                Value::from(format!("{:x}:{:x}", self.dev_u64(), self.file_id_ino)),
            );
        } else if let Some(s) = self.rare.as_deref().and_then(|r| r.file_id_str.as_deref()) {
            map.insert("fileIdentity".into(), Value::from(s));
        }
        if let Some(state) = append_override {
            map.insert("appendState".into(), state);
        } else if let Some(state) = self.rare.as_deref().and_then(|r| r.append_state.as_ref()) {
            map.insert("appendState".into(), state.to_json(self));
        }
        if self.flags & FLAG_HAS_OPTIONAL_ROOT != 0 {
            map.insert("optionalRoot".into(), Value::from(self.optional_root()));
        }
        if self.flags & FLAG_HAS_UNREAD_USAGE != 0 {
            map.insert("unreadUsage".into(), Value::from(self.unread_usage()));
        }
        if self.flags & FLAG_HAS_ESTIMATED_CLOCK != 0 {
            map.insert("estimatedClock".into(), Value::from(self.estimated_clock()));
        }
        if let Some(slot) = settled_slot_override.or_else(|| self.settled_checked_slot()) {
            map.insert("settledCheckedSlot".into(), Value::from(slot));
        }
        if let Some(rare) = self.rare.as_deref() {
            if let Some(through) = rare.read_through {
                map.insert("readThrough".into(), Value::from(through));
            } else if self.has_read_through_key() {
                map.insert("readThrough".into(), Value::Null);
            }
            if settled_slot_override.is_none()
                && let Some(from) = rare.read_from
            {
                map.insert("readFrom".into(), Value::from(from));
            }
            if let Some(state) = &rare.read_state {
                map.insert(
                    "readState".into(),
                    serde_json::from_str(state.get()).unwrap_or(Value::Null),
                );
            }
            if let Some(name) = &rare.read_name {
                map.insert("readName".into(), Value::from(name.as_ref()));
            }
            if let Some(decoded) = rare.decoded_size {
                map.insert("decodedSize".into(), Value::from(decoded));
            }
            for (k, v) in &rare.unknown {
                if settled_slot_override.is_some() && k.as_ref() == "readFrom" {
                    continue;
                }
                if let Ok(parsed) = serde_json::from_str(v.get()) {
                    map.insert(k.to_string(), parsed);
                }
            }
        }
        Value::Object(map)
    }

    pub fn from_value(value: &Value) -> Option<Self> {
        let map = value.as_object()?;
        let mut out = Self::default();
        for (k, v) in map {
            out.apply_value_field(k, v);
        }
        let same = out
            .rare
            .as_deref()
            .and_then(|r| r.append_state.as_ref())
            .is_some_and(|state| {
                matches!(
                    state,
                    CompactAppendState::Full(full)
                        if out.head_eq_hash(&full.head) && out.tail_eq_hash(&full.tail)
                )
            });
        if same && let Some(CompactAppendState::Full(full)) = out.rare.as_mut().and_then(|r| r.append_state.take()) {
            out.rare_mut().append_state = Some(CompactAppendState::SameEnds(full.parser_raw));
        }
        Some(out)
    }

    fn apply_head_hash(&mut self, hash: CompactHash) {
        match hash {
            CompactHash::Sha256(sha) => {
                self.head_sha = sha;
                self.flags |= FLAG_HEAD_SHA;
            }
            CompactHash::Other(s) => {
                self.flags &= !FLAG_HEAD_SHA;
                self.rare_mut().head_str = Some(s);
            }
        }
    }

    fn apply_tail_hash(&mut self, hash: CompactHash) {
        match hash {
            CompactHash::Sha256(sha) => {
                self.tail_sha = sha;
                self.flags |= FLAG_TAIL_SHA;
            }
            CompactHash::Other(s) => {
                self.flags &= !FLAG_TAIL_SHA;
                self.rare_mut().tail_str = Some(s);
            }
        }
    }

    fn apply_path_key_hash(&mut self, hash: CompactHash) {
        match hash {
            CompactHash::Sha256(sha) => {
                self.path_key_sha = sha;
                self.flags |= FLAG_PATH_KEY_SHA;
            }
            CompactHash::Other(s) => {
                self.flags &= !FLAG_PATH_KEY_SHA;
                self.rare_mut().path_key_str = Some(s);
            }
        }
    }

    fn apply_file_id_parsed(&mut self, parsed: Result<(u64, u64), Box<str>>) {
        match parsed {
            Ok((dev, ino)) => {
                self.file_id_ino = ino;
                self.flags |= FLAG_FILE_ID_PAIR;
                if let Ok(small) = u32::try_from(dev) {
                    self.file_id_dev = small;
                    self.flags &= !FLAG_FILE_DEV_RARE;
                } else {
                    self.flags |= FLAG_FILE_DEV_RARE;
                    self.rare_mut().file_id_dev_u64 = dev;
                }
            }
            Err(s) => {
                self.flags &= !(FLAG_FILE_ID_PAIR | FLAG_FILE_DEV_RARE);
                self.rare_mut().file_id_str = Some(s);
            }
        }
    }

    fn apply_value_field(&mut self, key: &str, v: &Value) {
        match key {
            "size" => {
                if let Some(n) = v.as_u64() {
                    self.size = n;
                    self.flags |= FLAG_HAS_SIZE;
                }
            }
            "mtimeMs" => {
                if let Some(n) = v.as_i64() {
                    self.mtime_ms = n;
                    self.flags |= FLAG_HAS_MTIME;
                }
            }
            "newestUsageMs" => {
                self.flags |= FLAG_HAS_NEWEST_USAGE_KEY;
                if let Some(n) = v.as_i64() {
                    self.newest_usage_ms = n;
                    self.flags |= FLAG_HAS_NEWEST_USAGE_VAL;
                } else {
                    self.flags &= !FLAG_HAS_NEWEST_USAGE_VAL;
                }
            }
            "settledCheckedSlot" => {
                if let Some(n) = v.as_u64() {
                    self.apply_settled_slot(n);
                }
            }
            "head" => {
                if let Some(s) = v.as_str() {
                    self.apply_head_hash(CompactHash::from_text(s));
                }
            }
            "tail" => {
                if let Some(s) = v.as_str() {
                    self.apply_tail_hash(CompactHash::from_text(s));
                }
            }
            "pathKey" => {
                if let Some(s) = v.as_str() {
                    self.apply_path_key_hash(CompactHash::from_text(s));
                }
            }
            "fileIdentity" => {
                if let Some(s) = v.as_str() {
                    self.apply_file_id_parsed(parse_canonical_file_id(s).ok_or_else(|| s.into()));
                }
            }
            "claudeName" => {
                if let Some(s) = v.as_str() {
                    self.rare_mut().claude_name = Some(CompactHash::from_text(s));
                }
            }
            "claudeMirror" => {
                if let Some(b) = v.as_bool() {
                    self.flags |= FLAG_HAS_CLAUDE_MIRROR;
                    if b {
                        self.flags |= FLAG_CLAUDE_MIRROR_VAL;
                    } else {
                        self.flags &= !FLAG_CLAUDE_MIRROR_VAL;
                    }
                }
            }
            "claudeGroup" => {
                if let Some(s) = v.as_str() {
                    self.rare_mut().claude_group = Some(CompactHash::from_text(s));
                }
            }
            "usageSkipped" => {
                if let Some(b) = v.as_bool() {
                    self.flags |= FLAG_HAS_USAGE_SKIPPED;
                    if b {
                        self.flags |= FLAG_USAGE_SKIPPED;
                    } else {
                        self.flags &= !FLAG_USAGE_SKIPPED;
                    }
                }
            }
            "unreadUsage" => {
                if let Some(b) = v.as_bool() {
                    self.flags |= FLAG_HAS_UNREAD_USAGE;
                    if b {
                        self.flags |= FLAG_UNREAD_USAGE;
                    } else {
                        self.flags &= !FLAG_UNREAD_USAGE;
                    }
                }
            }
            "estimatedClock" => {
                if let Some(b) = v.as_bool() {
                    self.flags |= FLAG_HAS_ESTIMATED_CLOCK;
                    if b {
                        self.flags |= FLAG_ESTIMATED_CLOCK;
                    } else {
                        self.flags &= !FLAG_ESTIMATED_CLOCK;
                    }
                }
            }
            "optionalRoot" => {
                if let Some(b) = v.as_bool() {
                    self.flags |= FLAG_HAS_OPTIONAL_ROOT;
                    if b {
                        self.flags |= FLAG_OPTIONAL_ROOT;
                    } else {
                        self.flags &= !FLAG_OPTIONAL_ROOT;
                    }
                }
            }
            "walUnread" => {
                if let Some(b) = v.as_bool() {
                    self.flags |= FLAG_HAS_WAL_UNREAD;
                    if b {
                        self.flags |= FLAG_WAL_UNREAD;
                    } else {
                        self.flags &= !FLAG_WAL_UNREAD;
                    }
                }
            }
            "walSize" => {
                if let Some(n) = v.as_u64() {
                    self.rare_mut().wal_size = Some(n);
                }
            }
            "walMtimeMs" => {
                if let Some(n) = v.as_i64() {
                    self.rare_mut().wal_mtime_ms = Some(n);
                }
            }
            "msgThrough" => {
                if let Some(n) = v.as_u64() {
                    self.rare_mut().msg_through = Some(n);
                }
            }
            "msgHead" => {
                if let Some(s) = v.as_str() {
                    self.rare_mut().msg_head = Some(CompactHash::from_text(s));
                }
            }
            "readThrough" => {
                self.flags |= FLAG_HAS_READ_THROUGH_KEY;
                if let Some(n) = v.as_u64() {
                    self.rare_mut().read_through = Some(n);
                }
            }
            "readFrom" => {
                if let Some(n) = v.as_u64() {
                    self.rare_mut().read_from = Some(n);
                }
            }
            "readState" => {
                if let Ok(raw) = serde_json::value::to_raw_value(v) {
                    self.rare_mut().read_state = Some(raw);
                }
            }
            "readName" => {
                if let Some(s) = v.as_str() {
                    self.rare_mut().read_name = Some(s.into());
                }
            }
            "decodedSize" => {
                if let Some(n) = v.as_u64() {
                    self.rare_mut().decoded_size = Some(n);
                }
            }
            "unreadableRecords" => {
                if let Some(n) = v.as_u64() {
                    self.rare_mut().unreadable_records = Some(n);
                }
            }
            "oldestUnreadableMs" => {
                self.rare_mut().oldest_unreadable_ms = Some(v.as_i64());
            }
            "sqliteMinDateMs" => {
                if let Some(n) = v.as_i64() {
                    self.rare_mut().sqlite_min_date_ms = Some(n);
                }
            }
            "appendState" => {
                if let Some(state) = CompactAppendState::from_value(v) {
                    self.rare_mut().append_state = Some(state);
                }
            }
            other => {
                if let Ok(raw) = serde_json::value::to_raw_value(v) {
                    self.rare_mut().unknown.push((other.into(), raw));
                }
            }
        }
    }
}

/// One slab entry: inline 32-byte key digest + 160-byte `PriorPrint` = 192 bytes exact.
#[derive(Clone, Debug)]
struct PriorEntry {
    key_sha: [u8; 32],
    print: PriorPrint,
}

impl PriorEntry {
    fn key_hash(&self) -> CompactHash {
        if self.print.flags & FLAG_KEY_OTHER == 0 {
            CompactHash::Sha256(self.key_sha)
        } else {
            let text = self
                .print
                .rare
                .as_deref()
                .and_then(|r| r.key_str.as_deref())
                .unwrap_or("");
            CompactHash::Other(text.into())
        }
    }
}

/// Chunked slab of `PriorEntry` indexed by a compact 4-byte-per-slot `CompactShaIndex`
/// plus block-compressed `SameEnds` `appendState.parser` storage.
#[derive(Clone, Debug, Default)]
pub struct PriorKindMap {
    chunks: Vec<Vec<PriorEntry>>,
    sha_index: CompactShaIndex,
    other_index: HashMap<Box<str>, u32>,
    append_blobs: AppendBlobStore,
}

impl PriorKindMap {
    pub fn len(&self) -> usize {
        self.sha_index.len() + self.other_index.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    #[inline]
    fn entry_at(&self, idx: u32) -> Option<&PriorEntry> {
        let i = idx as usize;
        self.chunks.get(i >> CHUNK_SHIFT)?.get(i & CHUNK_MASK)
    }

    #[inline]
    fn key_sha_at(chunks: &[Vec<PriorEntry>], idx: u32) -> Option<&[u8; 32]> {
        let i = idx as usize;
        let entry = chunks.get(i >> CHUNK_SHIFT)?.get(i & CHUNK_MASK)?;
        (entry.print.flags & FLAG_KEY_OTHER == 0).then_some(&entry.key_sha)
    }

    pub(crate) fn path_key_sha_at(&self, idx: u32) -> Option<&[u8; 32]> {
        self.entry_at(idx)?.print.path_key_sha()
    }

    pub fn finish_loading(&mut self) {
        self.append_blobs.finish();
    }

    pub fn insert(&mut self, key: CompactHash, mut value: PriorPrint) {
        let existing = match &key {
            CompactHash::Sha256(bytes) => {
                let chunks = &self.chunks;
                self.sha_index.get(bytes, |idx| Self::key_sha_at(chunks, idx))
            }
            CompactHash::Other(text) => self.other_index.get(text.as_ref()).copied(),
        };
        let key_sha = match key {
            CompactHash::Sha256(bytes) => {
                value.flags &= !FLAG_KEY_OTHER;
                bytes
            }
            CompactHash::Other(text) => {
                value.flags |= FLAG_KEY_OTHER;
                value.rare_mut().key_str = Some(text);
                [0; 32]
            }
        };
        if let Some(idx) = existing {
            let i = idx as usize;
            self.chunks[i >> CHUNK_SHIFT][i & CHUNK_MASK] = PriorEntry { key_sha, print: value };
            return;
        }
        let idx = self.chunks.iter().map(Vec::len).sum::<usize>() as u32;
        if self.chunks.last().is_none_or(|c| c.len() == CHUNK_SIZE) {
            let cap = if self.chunks.is_empty() { 16 } else { CHUNK_SIZE };
            self.chunks.push(Vec::with_capacity(cap));
        }
        let is_other = value.flags & FLAG_KEY_OTHER != 0;
        let other_key = if is_other {
            value.rare.as_deref().and_then(|r| r.key_str.clone())
        } else {
            None
        };
        if let Some(chunk) = self.chunks.last_mut() {
            chunk.push(PriorEntry { key_sha, print: value });
        }
        if is_other {
            if let Some(text) = other_key {
                self.other_index.insert(text, idx);
            }
        } else {
            let chunks = &self.chunks;
            self.sha_index
                .insert(&key_sha, idx, |slot| Self::key_sha_at(chunks, slot));
        }
    }

    pub fn get_index(&self, idx: u32) -> Option<(CompactHash, &PriorPrint)> {
        let entry = self.entry_at(idx)?;
        Some((entry.key_hash(), &entry.print))
    }

    pub fn get_str(&self, key: &str) -> Option<&PriorPrint> {
        let idx = match decode_sha256_hex(key) {
            Some(bytes) => {
                let chunks = &self.chunks;
                self.sha_index.get(&bytes, |slot| Self::key_sha_at(chunks, slot))?
            }
            None => self.other_index.get(key).copied()?,
        };
        self.entry_at(idx).map(|e| &e.print)
    }

    pub fn get_hash(&self, key: &CompactHash) -> Option<&PriorPrint> {
        let idx = match key {
            CompactHash::Sha256(bytes) => {
                let chunks = &self.chunks;
                self.sha_index.get(bytes, |slot| Self::key_sha_at(chunks, slot))?
            }
            CompactHash::Other(text) => self.other_index.get(text.as_ref()).copied()?,
        };
        self.entry_at(idx).map(|e| &e.print)
    }

    pub fn iter(&self) -> impl Iterator<Item = (CompactHash, &PriorPrint)> {
        self.chunks
            .iter()
            .flat_map(|c| c.iter().map(|e| (e.key_hash(), &e.print)))
    }

    pub fn iter_indexed_prints(&self) -> impl Iterator<Item = (u32, &PriorPrint)> {
        self.chunks.iter().enumerate().flat_map(|(ci, chunk)| {
            let base = (ci << CHUNK_SHIFT) as u32;
            chunk
                .iter()
                .enumerate()
                .map(move |(ei, e)| (base + ei as u32, &e.print))
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = CompactHash> + '_ {
        self.chunks.iter().flat_map(|c| c.iter().map(PriorEntry::key_hash))
    }

    pub fn has_append_state(&self, print: &PriorPrint) -> bool {
        print.append_block != NO_APPEND_BLOCK || print.rare.as_deref().is_some_and(|r| r.append_state.is_some())
    }

    pub fn append_state_has_ends(&self, print: &PriorPrint) -> bool {
        if print.append_block != NO_APPEND_BLOCK {
            return true;
        }
        print
            .rare
            .as_deref()
            .and_then(|r| r.append_state.as_ref())
            .is_some_and(CompactAppendState::has_head_and_tail)
    }

    pub fn append_state_head_tail_match(&self, print: &PriorPrint, head: &str, tail: &str) -> bool {
        if print.append_block != NO_APPEND_BLOCK {
            return print.head_eq(head) && print.tail_eq(tail);
        }
        print
            .rare
            .as_deref()
            .and_then(|r| r.append_state.as_ref())
            .is_some_and(|state| state.head_tail_match(print, head, tail))
    }

    pub fn append_state_parser_value(&self, print: &PriorPrint) -> Option<Value> {
        if print.append_block != NO_APPEND_BLOCK {
            let raw = self
                .append_blobs
                .get(print.append_block, print.append_off, print.append_len)?;
            return Some(serde_json::from_slice(&raw).unwrap_or(Value::Null));
        }
        print
            .rare
            .as_deref()
            .and_then(|r| r.append_state.as_ref())
            .map(CompactAppendState::parser_value)
    }

    pub fn append_state_json(&self, print: &PriorPrint) -> Option<Value> {
        if print.append_block != NO_APPEND_BLOCK {
            let parser = self.append_state_parser_value(print).unwrap_or(Value::Null);
            return Some(json!({
                "head": print.head_string().unwrap_or_default(),
                "tail": print.tail_string().unwrap_or_default(),
                "parser": parser,
            }));
        }
        print
            .rare
            .as_deref()
            .and_then(|r| r.append_state.as_ref())
            .map(|state| state.to_json(print))
    }

    pub fn print_to_json(&self, print: &PriorPrint) -> Value {
        let append = self.append_state_json(print);
        print.to_json_inner(None, append)
    }

    pub fn print_to_settled_json(&self, print: &PriorPrint, slot: u64) -> Value {
        let append = self.append_state_json(print);
        print.to_json_inner(Some(slot), append)
    }
}

/// Deserialize a JSON value into `Option<PriorPrint>`, storing `SameEnds` `appendState.parser`
/// bytes straight into the owning `AppendBlobStore`.
struct OptionPriorPrintSeed<'a>(&'a mut AppendBlobStore);

impl<'de> DeserializeSeed<'de> for OptionPriorPrintSeed<'_> {
    type Value = Option<PriorPrint>;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Self::Value, D::Error> {
        struct V<'a>(&'a mut AppendBlobStore);
        impl<'de> Visitor<'de> for V<'_> {
            type Value = Option<PriorPrint>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a fingerprint object or ignored value")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut print = PriorPrint::default();
                let mut pending_append: Option<ParsedAppendState> = None;
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "size" => {
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.size = n;
                                print.flags |= FLAG_HAS_SIZE;
                            }
                        }
                        "mtimeMs" => {
                            if let Some(n) = map.next_value::<LenientI64>()?.0 {
                                print.mtime_ms = n;
                                print.flags |= FLAG_HAS_MTIME;
                            }
                        }
                        "newestUsageMs" => {
                            print.flags |= FLAG_HAS_NEWEST_USAGE_KEY;
                            if let Some(n) = map.next_value::<LenientI64>()?.0 {
                                print.newest_usage_ms = n;
                                print.flags |= FLAG_HAS_NEWEST_USAGE_VAL;
                            } else {
                                print.flags &= !FLAG_HAS_NEWEST_USAGE_VAL;
                            }
                        }
                        "settledCheckedSlot" => {
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.apply_settled_slot(n);
                            }
                        }
                        "head" => {
                            if let Some(h) = map.next_value::<LenientHash>()?.0 {
                                print.apply_head_hash(h);
                            }
                        }
                        "tail" => {
                            if let Some(h) = map.next_value::<LenientHash>()?.0 {
                                print.apply_tail_hash(h);
                            }
                        }
                        "pathKey" => {
                            if let Some(h) = map.next_value::<LenientHash>()?.0 {
                                print.apply_path_key_hash(h);
                            }
                        }
                        "fileIdentity" => {
                            if let Some(id) = map.next_value::<LenientFileId>()?.0 {
                                print.apply_file_id_parsed(id);
                            }
                        }
                        "claudeName" => {
                            if let Some(h) = map.next_value::<LenientHash>()?.0 {
                                print.rare_mut().claude_name = Some(h);
                            }
                        }
                        "claudeMirror" => {
                            if let Some(b) = map.next_value::<LenientBool>()?.0 {
                                print.flags |= FLAG_HAS_CLAUDE_MIRROR;
                                if b {
                                    print.flags |= FLAG_CLAUDE_MIRROR_VAL;
                                } else {
                                    print.flags &= !FLAG_CLAUDE_MIRROR_VAL;
                                }
                            }
                        }
                        "claudeGroup" => {
                            if let Some(h) = map.next_value::<LenientHash>()?.0 {
                                print.rare_mut().claude_group = Some(h);
                            }
                        }
                        "usageSkipped" => {
                            if let Some(b) = map.next_value::<LenientBool>()?.0 {
                                print.flags |= FLAG_HAS_USAGE_SKIPPED;
                                if b {
                                    print.flags |= FLAG_USAGE_SKIPPED;
                                } else {
                                    print.flags &= !FLAG_USAGE_SKIPPED;
                                }
                            }
                        }
                        "unreadUsage" => {
                            if let Some(b) = map.next_value::<LenientBool>()?.0 {
                                print.flags |= FLAG_HAS_UNREAD_USAGE;
                                if b {
                                    print.flags |= FLAG_UNREAD_USAGE;
                                } else {
                                    print.flags &= !FLAG_UNREAD_USAGE;
                                }
                            }
                        }
                        "estimatedClock" => {
                            if let Some(b) = map.next_value::<LenientBool>()?.0 {
                                print.flags |= FLAG_HAS_ESTIMATED_CLOCK;
                                if b {
                                    print.flags |= FLAG_ESTIMATED_CLOCK;
                                } else {
                                    print.flags &= !FLAG_ESTIMATED_CLOCK;
                                }
                            }
                        }
                        "optionalRoot" => {
                            if let Some(b) = map.next_value::<LenientBool>()?.0 {
                                print.flags |= FLAG_HAS_OPTIONAL_ROOT;
                                if b {
                                    print.flags |= FLAG_OPTIONAL_ROOT;
                                } else {
                                    print.flags &= !FLAG_OPTIONAL_ROOT;
                                }
                            }
                        }
                        "walUnread" => {
                            if let Some(b) = map.next_value::<LenientBool>()?.0 {
                                print.flags |= FLAG_HAS_WAL_UNREAD;
                                if b {
                                    print.flags |= FLAG_WAL_UNREAD;
                                } else {
                                    print.flags &= !FLAG_WAL_UNREAD;
                                }
                            }
                        }
                        "walSize" => {
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.rare_mut().wal_size = Some(n);
                            }
                        }
                        "walMtimeMs" => {
                            if let Some(n) = map.next_value::<LenientI64>()?.0 {
                                print.rare_mut().wal_mtime_ms = Some(n);
                            }
                        }
                        "msgThrough" => {
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.rare_mut().msg_through = Some(n);
                            }
                        }
                        "msgHead" => {
                            if let Some(h) = map.next_value::<LenientHash>()?.0 {
                                print.rare_mut().msg_head = Some(h);
                            }
                        }
                        "readThrough" => {
                            print.flags |= FLAG_HAS_READ_THROUGH_KEY;
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.rare_mut().read_through = Some(n);
                            }
                        }
                        "readFrom" => {
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.rare_mut().read_from = Some(n);
                            }
                        }
                        "readState" => {
                            let raw: Box<RawValue> = map.next_value()?;
                            print.rare_mut().read_state = Some(raw);
                        }
                        "readName" => {
                            if let Some(s) = map.next_value::<LenientStr>()?.0 {
                                print.rare_mut().read_name = Some(s);
                            }
                        }
                        "decodedSize" => {
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.rare_mut().decoded_size = Some(n);
                            }
                        }
                        "unreadableRecords" => {
                            if let Some(n) = map.next_value::<LenientU64>()?.0 {
                                print.rare_mut().unreadable_records = Some(n);
                            }
                        }
                        "oldestUnreadableMs" => {
                            print.rare_mut().oldest_unreadable_ms = Some(map.next_value::<LenientI64>()?.0);
                        }
                        "sqliteMinDateMs" => {
                            if let Some(n) = map.next_value::<LenientI64>()?.0 {
                                print.rare_mut().sqlite_min_date_ms = Some(n);
                            }
                        }
                        "appendState" => {
                            pending_append = Some(map.next_value::<ParsedAppendState>()?);
                        }
                        _ => {
                            let raw: Box<RawValue> = map.next_value()?;
                            print.rare_mut().unknown.push((key.into_owned().into(), raw));
                        }
                    }
                }
                if let Some(parsed) = pending_append {
                    print.store_parsed_append(parsed, Some(self.0));
                }
                Ok(Some(print))
            }

            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(None)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(None)
            }
        }
        deserializer.deserialize_any(V(self.0))
    }
}

/// Typed prior fingerprint state parsed directly from request JSON + `settledPacked`.
#[derive(Clone, Debug, Default)]
pub struct PriorState {
    pub store_incremental: bool,
    pub settled_slot: Option<u64>,
    pub settled_before: HashMap<String, i64>,
    pub settled_packed_kinds: HashSet<String>,
    pub kinds: HashMap<String, PriorKindMap>,
}

impl PriorState {
    pub fn from_map(map: &Map<String, Value>) -> Self {
        let mut out = Self::default();
        out.overlay_map(map);
        out
    }

    pub fn overlay_map(&mut self, map: &Map<String, Value>) {
        if map.contains_key("_store") {
            self.store_incremental = true;
        }
        if let Some(store) = map.get("_store").and_then(Value::as_object) {
            if let Some(slot) = store.get("settledSlot").and_then(Value::as_u64) {
                self.settled_slot = Some(slot);
            }
            if let Some(before) = store.get("settledBefore").and_then(Value::as_object) {
                for (kind, cutoff) in before {
                    if let Some(cutoff) = cutoff.as_i64() {
                        self.settled_before.insert(kind.clone(), cutoff);
                    }
                }
            }
            if let Some(packed) = store.get("settledPacked").and_then(Value::as_object) {
                for (kind, val) in packed {
                    if val.as_str().is_some_and(|s| !s.is_empty()) {
                        self.settled_packed_kinds.insert(kind.clone());
                    }
                }
            }
        }
        for (kind, val) in map {
            if kind == "_store" {
                continue;
            }
            let Some(prints) = val.as_object() else {
                continue;
            };
            let entry = self.kinds.entry(kind.clone()).or_default();
            for (key, print_val) in prints {
                if let Some(print) = PriorPrint::from_value(print_val) {
                    entry.insert(CompactHash::from_text(key), print);
                }
            }
        }
    }

    pub fn get(&self, kind: &str, key: &str) -> Option<&PriorPrint> {
        self.kinds.get(kind)?.get_str(key)
    }

    pub fn get_hash(&self, kind: &str, key: &CompactHash) -> Option<&PriorPrint> {
        self.kinds.get(kind)?.get_hash(key)
    }
}

/// Zero-allocation streaming visitor for `_store.settledBefore`.
struct LenientI64Map(HashMap<String, i64>);
impl<'de> Deserialize<'de> for LenientI64Map {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientI64Map;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of kind to cutoff timestamp")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = HashMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if let Some(v) = map.next_value::<LenientI64>()?.0 {
                        out.insert(key, v);
                    }
                }
                Ok(LenientI64Map(out))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientI64Map(HashMap::new()))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientI64Map(HashMap::new()))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientI64Map(HashMap::new()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientI64Map(HashMap::new()))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientI64Map(HashMap::new()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientI64Map(HashMap::new()))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientI64Map(HashMap::new()))
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LenientBorrowedStr<'de>(Option<Cow<'de, str>>);
impl<'de> Deserialize<'de> for LenientBorrowedStr<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientBorrowedStr<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("any JSON value")
            }
            fn visit_borrowed_str<E: de::Error>(self, v: &'de str) -> Result<Self::Value, E> {
                Ok(LenientBorrowedStr(Some(Cow::Borrowed(v))))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Self::Value, E> {
                Ok(LenientBorrowedStr(Some(Cow::Owned(v.to_owned()))))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientBorrowedStr(None))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientBorrowedStr(None))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientBorrowedStr(None))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientBorrowedStr(None))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientBorrowedStr(None))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientBorrowedStr(None))
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
                Ok(LenientBorrowedStr(None))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Zero-copy streaming visitor for `_store.settledPacked` borrowing base64 strings from raw input.
struct LenientStrMap<'de>(HashMap<String, Cow<'de, str>>);
impl<'de> Deserialize<'de> for LenientStrMap<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = LenientStrMap<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a map of kind to packed settled string")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = HashMap::new();
                while let Some(key) = map.next_key::<String>()? {
                    if let Some(s) = map.next_value::<LenientBorrowedStr<'de>>()?.0 {
                        out.insert(key, s);
                    }
                }
                Ok(LenientStrMap(out))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(LenientStrMap(HashMap::new()))
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(LenientStrMap(HashMap::new()))
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(LenientStrMap(HashMap::new()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(LenientStrMap(HashMap::new()))
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(LenientStrMap(HashMap::new()))
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(LenientStrMap(HashMap::new()))
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(LenientStrMap(HashMap::new()))
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Zero-Value streaming visitor for `_store`.
#[derive(Default)]
struct StoreSettings<'de> {
    settled_slot: Option<u64>,
    settled_before: HashMap<String, i64>,
    settled_packed: HashMap<String, Cow<'de, str>>,
}

impl<'de> Deserialize<'de> for StoreSettings<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = StoreSettings<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a _store object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = StoreSettings::default();
                while let Some(key) = map.next_key::<Cow<'de, str>>()? {
                    match key.as_ref() {
                        "settledSlot" => {
                            out.settled_slot = map.next_value::<LenientU64>()?.0;
                        }
                        "settledBefore" => {
                            out.settled_before = map.next_value::<LenientI64Map>()?.0;
                        }
                        "settledPacked" => {
                            out.settled_packed = map.next_value::<LenientStrMap<'de>>()?.0;
                        }
                        _ => {
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(out)
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(StoreSettings::default())
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(StoreSettings::default())
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(StoreSettings::default())
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(StoreSettings::default())
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(StoreSettings::default())
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(StoreSettings::default())
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(StoreSettings::default())
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Result of deserializing `request.fingerprints` directly without building a `Value` tree.
#[derive(Default)]
pub struct RawFingerprints<'de> {
    pub state: PriorState,
    pub non_object_kinds: HashSet<String>,
    pub settled_packed_raw: HashMap<String, Cow<'de, str>>,
}

impl<'de> Deserialize<'de> for RawFingerprints<'de> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawFingerprints<'de>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a fingerprints object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = RawFingerprints::default();
                while let Some(kind) = map.next_key::<String>()? {
                    if kind == "_store" {
                        let store: StoreSettings<'de> = map.next_value()?;
                        out.state.store_incremental = true;
                        out.state.settled_slot = store.settled_slot;
                        out.state.settled_before = store.settled_before;
                        out.settled_packed_raw = store.settled_packed;
                    } else {
                        match map.next_value::<KindPrintsOrOther>()? {
                            KindPrintsOrOther::Map(mut prints) => {
                                prints.finish_loading();
                                out.state.kinds.insert(kind, prints);
                            }
                            KindPrintsOrOther::Other => {
                                out.non_object_kinds.insert(kind);
                            }
                        }
                    }
                }
                Ok(out)
            }
        }
        deserializer.deserialize_map(V)
    }
}

enum KindPrintsOrOther {
    Map(PriorKindMap),
    Other,
}

impl<'de> Deserialize<'de> for KindPrintsOrOther {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = KindPrintsOrOther;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a per-kind fingerprint map")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut out = PriorKindMap::default();
                while let Some(key) = map.next_key::<CompactHash>()? {
                    if let Some(p) = map.next_value_seed(OptionPriorPrintSeed(&mut out.append_blobs))? {
                        out.insert(key, p);
                    }
                }
                Ok(KindPrintsOrOther::Map(out))
            }
            fn visit_bool<E: de::Error>(self, _: bool) -> Result<Self::Value, E> {
                Ok(KindPrintsOrOther::Other)
            }
            fn visit_i64<E: de::Error>(self, _: i64) -> Result<Self::Value, E> {
                Ok(KindPrintsOrOther::Other)
            }
            fn visit_u64<E: de::Error>(self, _: u64) -> Result<Self::Value, E> {
                Ok(KindPrintsOrOther::Other)
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> Result<Self::Value, E> {
                Ok(KindPrintsOrOther::Other)
            }
            fn visit_str<E: de::Error>(self, _: &str) -> Result<Self::Value, E> {
                Ok(KindPrintsOrOther::Other)
            }
            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(KindPrintsOrOther::Other)
            }
            fn visit_seq<A: de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<IgnoredAny>()?.is_some() {}
                Ok(KindPrintsOrOther::Other)
            }
        }
        deserializer.deserialize_any(V)
    }
}

struct LimitedReader<R> {
    inner: R,
    remaining: usize,
}

impl<R: Read> Read for LimitedReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "settledPacked exceeds byte cap",
                )),
            };
        }
        let max = buf.len().min(self.remaining);
        let n = self.inner.read(&mut buf[..max])?;
        self.remaining -= n;
        Ok(n)
    }
}

struct InsertPriorMap<'a>(&'a mut PriorKindMap);

impl<'de> DeserializeSeed<'de> for InsertPriorMap<'_> {
    type Value = ();

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<(), D::Error> {
        struct V<'a>(&'a mut PriorKindMap);
        impl<'de> Visitor<'de> for V<'_> {
            type Value = ();
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a settled fingerprints JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<(), A::Error> {
                while let Some(key) = map.next_key::<CompactHash>()? {
                    if let Some(p) = map.next_value_seed(OptionPriorPrintSeed(&mut self.0.append_blobs))? {
                        self.0.insert(key, p);
                    }
                }
                Ok(())
            }
        }
        deserializer.deserialize_map(V(self.0))
    }
}

/// Stream-decompress `base64(zstd(JSON))` directly into compact `PriorPrint` structs without
/// buffering the decompressed JSON byte vector or building a `serde_json::Map<String, Value>`.
pub fn unpack_settled_into(encoded: &str, target: &mut PriorKindMap) -> Option<()> {
    if encoded.is_empty() {
        return None;
    }
    let b64 = base64::read::DecoderReader::new(Cursor::new(encoded.as_bytes()), &STANDARD_NO_PAD);
    let decoder = zstd::stream::read::Decoder::new(b64).ok()?;
    let limited = LimitedReader {
        inner: decoder,
        remaining: MAX_SETTLED_BYTES,
    };
    let mut reader = BufReader::with_capacity(64 * 1024, limited);
    let mut de = serde_json::Deserializer::from_reader(&mut reader);
    InsertPriorMap(target).deserialize(&mut de).ok()?;
    de.end().ok()?;
    let mut tail = [0u8; 64];
    loop {
        match reader.read(&mut tail) {
            Ok(0) => break,
            Ok(n) if tail[..n].iter().all(|b| b.is_ascii_whitespace()) => continue,
            _ => return None,
        }
    }
    target.finish_loading();
    Some(())
}

/// Serialize a settled-confirmed fingerprint entry directly without a `serde_json::Value` tree.
pub struct SettledConfirmed(pub u64);

impl Serialize for SettledConfirmed {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(2))?;
        map.serialize_entry("settledCheckedSlot", &self.0)?;
        map.serialize_entry("settledConfirmed", &true)?;
        map.end()
    }
}
