//! Compact value primitives shared across the compact share readers and check writer:
//! binary 32-byte SHA-256 digests, allocation-free stat stamps, compact problems,
//! transient miss tracking, and prototype/wire-order checks.

use aac_files::read::Stat;
use aac_status::read_context::PROTO_NAMES;
use serde_json::{Map, Value as Json};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;

/// A SHA-256 digest stored inline as 32 bytes when it is canonical 64-char lowercase hex,
/// falling back to a boxed string for non-hex synthetic test fixtures so round-trips are
/// 100% lossless.
#[derive(Clone, PartialEq, Eq, Hash)]
pub(crate) enum CompactSha {
    Hex32([u8; 32]),
    Other(Box<str>),
}

impl CompactSha {
    pub(crate) fn digest_bytes(bytes: &[u8]) -> Self {
        Self::Hex32(Sha256::digest(bytes).into())
    }

    pub(crate) fn from_hex_or_str(text: &str) -> Self {
        if let Some(raw) = decode_lower_hex32(text) {
            Self::Hex32(raw)
        } else {
            Self::Other(text.into())
        }
    }

    pub(crate) fn to_hex_string(&self) -> String {
        match self {
            Self::Hex32(bytes) => encode_lower_hex32(bytes),
            Self::Other(text) => text.to_string(),
        }
    }

    pub(crate) fn write_hex(&self, out: &mut String) {
        match self {
            Self::Hex32(bytes) => write_lower_hex32(bytes, out),
            Self::Other(text) => out.push_str(text),
        }
    }

    pub(crate) fn eq_str(&self, other: &str) -> bool {
        match self {
            Self::Hex32(bytes) => decode_lower_hex32(other).is_some_and(|raw| raw == *bytes),
            Self::Other(text) => text.as_ref() == other,
        }
    }

    pub(crate) fn update_hasher(&self, hasher: &mut Sha256) {
        match self {
            Self::Hex32(bytes) => {
                hasher.update([0u8]);
                hasher.update(bytes);
            }
            Self::Other(text) => {
                hasher.update([1u8]);
                hasher.update((text.len() as u64).to_le_bytes());
                hasher.update(text.as_bytes());
            }
        }
    }

    pub(crate) fn proto_unsafe(&self) -> bool {
        match self {
            Self::Hex32(_) => false,
            Self::Other(text) => proto_named_str(text),
        }
    }
}

impl fmt::Debug for CompactSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex_string())
    }
}

fn hex_nibble_lower(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

pub(crate) fn decode_lower_hex32(text: &str) -> Option<[u8; 32]> {
    let bytes = text.as_bytes();
    if bytes.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, slot) in out.iter_mut().enumerate() {
        let hi = hex_nibble_lower(bytes[i * 2])?;
        let lo = hex_nibble_lower(bytes[i * 2 + 1])?;
        *slot = (hi << 4) | lo;
    }
    Some(out)
}

const HEX_CHARS: &[u8; 16] = b"0123456789abcdef";

pub(crate) fn write_lower_hex32(bytes: &[u8; 32], out: &mut String) {
    let mut buf = [0u8; 64];
    for (i, &byte) in bytes.iter().enumerate() {
        buf[i * 2] = HEX_CHARS[(byte >> 4) as usize];
        buf[i * 2 + 1] = HEX_CHARS[(byte & 0x0f) as usize];
    }
    // SAFETY: HEX_CHARS are strictly ASCII.
    let text = std::str::from_utf8(&buf).unwrap_or_default();
    out.push_str(text);
}

pub(crate) fn encode_lower_hex32(bytes: &[u8; 32]) -> String {
    let mut out = String::with_capacity(64);
    write_lower_hex32(bytes, &mut out);
    out
}

/// Canonicalize an IEEE-754 f64 to match `aac_num::ecma::to_string` equality:
/// `-0.0` and `+0.0` both format as `"0"`, and any `NaN` formats as `"NaN"`.
#[inline]
fn canon_f64_bits(value: f64) -> u64 {
    if value == 0.0 {
        0.0_f64.to_bits()
    } else if value.is_nan() {
        f64::NAN.to_bits()
    } else {
        value.to_bits()
    }
}

/// Allocation-free equivalent of `aac_status::share::stat_key(&stat)`
/// (`size:mtime_ms:inode:ctime_ms`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StatStamp {
    size: u64,
    mtime_bits: u64,
    inode: u64,
    ctime_bits: u64,
}

impl StatStamp {
    #[inline]
    pub(crate) fn from_stat(stat: &Stat) -> Self {
        Self {
            size: stat.size,
            mtime_bits: canon_f64_bits(stat.mtime_ms),
            inode: stat.inode,
            ctime_bits: canon_f64_bits(stat.ctime_ms),
        }
    }
}

/// Allocation-free equivalent of status_docs' `format!("{}:{}", to_string(mtime_ms), size)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MtimeSizeStamp {
    mtime_bits: u64,
    size: u64,
}

impl MtimeSizeStamp {
    #[inline]
    pub(crate) fn from_stat(stat: &Stat) -> Self {
        Self {
            mtime_bits: canon_f64_bits(stat.mtime_ms),
            size: stat.size,
        }
    }
}

/// Compact named problem (relative file path + reason + first-seen epoch ms).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactProblem {
    pub(crate) file: Box<str>,
    pub(crate) reason: Box<str>,
    pub(crate) since: u64,
}

impl CompactProblem {
    pub(crate) fn new(file: &str, reason: &str, since: u64) -> Self {
        Self {
            file: file.into(),
            reason: reason.into(),
            since,
        }
    }

    pub(crate) fn to_json(&self) -> Json {
        serde_json::json!({
            "file": self.file.as_ref(),
            "reason": self.reason.as_ref(),
            "since": self.since,
        })
    }

    pub(crate) fn to_share_problem(&self) -> aac_status::share::Problem {
        aac_status::share::Problem {
            file: self.file.to_string(),
            reason: self.reason.to_string(),
            since: self.since,
        }
    }

    pub(crate) fn proto_unsafe(&self) -> bool {
        proto_named_str(&self.file) || proto_named_str(&self.reason)
    }
}

/// Compact registry problem (`item`, `reason`, `since`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CompactRegistryProblem {
    pub(crate) item: Box<str>,
    pub(crate) reason: Box<str>,
    pub(crate) since: u64,
}

impl CompactRegistryProblem {
    pub(crate) fn to_json(&self) -> Json {
        serde_json::json!({
            "item": self.item.as_ref(),
            "reason": self.reason.as_ref(),
            "since": self.since,
        })
    }

    pub(crate) fn proto_unsafe(&self) -> bool {
        proto_named_str(&self.item) || proto_named_str(&self.reason)
    }
}

/// Consecutive transient miss tracker using compact `Box<str>` keys.
#[derive(Default)]
pub(crate) struct CompactTransientMisses {
    first: HashMap<Box<str>, u64>,
}

impl CompactTransientMisses {
    pub(crate) fn miss(&mut self, file: &str, now: u64) -> Option<u64> {
        match self.first.get(file) {
            Some(since) => Some(*since),
            None => {
                self.first.insert(file.into(), now);
                None
            }
        }
    }

    pub(crate) fn clear(&mut self, file: &str) {
        self.first.remove(file);
    }

    pub(crate) fn keep_only(&mut self, seen: &HashSet<Box<str>>) {
        self.first.retain(|file, _| seen.contains(file));
    }

    pub(crate) fn keep_only_strings(&mut self, seen: &HashSet<String>) {
        self.first.retain(|file, _| seen.contains(file.as_ref()));
    }
}

/// Exact `Object.prototype` member name check matching `aac_status::read_context::proto_named`.
#[inline]
pub(crate) fn proto_named_str(text: &str) -> bool {
    PROTO_NAMES.contains(&text)
}

#[inline]
pub(crate) fn proto_named_opt(text: Option<&str>) -> bool {
    text.is_some_and(proto_named_str)
}

/// Same key-order proof as `aac_status::read_status_reports::wire_order`:
/// ECMA-262 array-index keys (0..u32::MAX-1 in canonical spelling) must appear first in
/// strictly ascending numeric order, before any non-index key.
pub(crate) fn wire_order(value: &Json) -> bool {
    match value {
        Json::Array(items) => items.iter().all(wire_order),
        Json::Object(map) => wire_order_map(map),
        _ => true,
    }
}

pub(crate) fn wire_order_map(map: &Map<String, Json>) -> bool {
    let mut last = None;
    let mut saw_name = false;
    for (key, member) in map {
        let index = key
            .as_bytes()
            .first()
            .filter(|c| c.is_ascii_digit())
            .and_then(|_| key.parse::<u32>().ok())
            .filter(|i| *i != u32::MAX && i.to_string() == *key);
        match index {
            Some(index) if !saw_name && last.is_none_or(|old| old < index) => last = Some(index),
            Some(_) => return false,
            None => saw_name = true,
        }
        if !wire_order(member) {
            return false;
        }
    }
    true
}

/// Write a JSON string escaped identically to `serde_json::to_string`.
pub(crate) fn write_json_str(out: &mut String, text: &str) {
    let escaped = serde_json::to_string(text).unwrap_or_else(|_| "\"\"".to_owned());
    out.push_str(&escaped);
}
