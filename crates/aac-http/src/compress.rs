//! API JSON gzip compression, tag-keyed compressed caching, and zero-recompression
//! spliceable gzip streams for views whose cached body patches one clock field.
use axum::{
    body::{Body, Bytes, to_bytes},
    http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header},
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};
use flate2::{
    Compress, Compression, Crc, FlushCompress, Status as FlateStatus,
    read::GzDecoder,
    write::{DeflateEncoder, GzEncoder},
};
use sha1::{Digest, Sha1};
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::{
    collections::{HashMap, VecDeque},
    io::{self, Read, Write},
    sync::Mutex,
};

/// JSON responses smaller than this are left uncompressed (matches `api-compression.ts`).
pub(crate) const MIN_COMPRESS_BYTES: usize = 1024;
/// Upper bound on bodies buffered by the fallback compression layer.
pub(crate) const MAX_COMPRESS_BYTES: usize = 16 * 1024 * 1024;
/// Balanced gzip level (near-default ratio with `zlib-rs` at roughly half the encode cost).
pub(crate) const GZIP_LEVEL: Compression = Compression::new(4);

/// Internal private-hop request header: `"1"` when the public client accepts gzip and wants a body.
pub(crate) const ACCEPT_GZIP_HEADER: &str = "x-aac-accept-gzip";
/// Internal private-hop response header carrying the uncompressed length of a pre-gzipped body
/// so `HEAD` can advertise identity length without decompressing. Always stripped before sending.
pub(crate) const RAW_LENGTH_HEADER: &str = "x-aac-raw-length";

/// Standard 10-byte RFC 1952 gzip header (DEFLATE, no extra fields, unknown OS).
const GZIP_HEADER: [u8; 10] = [0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xff];
/// Minimum prefix or suffix byte length worth pre-deflating in [`SpliceGzip`].
const SPLICE_DEFLATE_MIN: usize = 256;
/// Maximum payload bytes per RFC 1951 stored DEFLATE block.
const MAX_STORED_BLOCK: usize = 65_535;

/// Bounded tag-keyed compressed body cache for tagged API JSON replies.
const TAG_CACHE_ENTRIES: usize = 32;
const TAG_CACHE_BYTES: usize = 4 << 20;

#[cfg(test)]
static TAG_CACHE_COMPRESSIONS: AtomicUsize = AtomicUsize::new(0);

/// Express-compatible weak ETag `W/"<hex-len>-<sha1-b64>"` for uncompressed JSON bytes.
pub(crate) fn weak_etag_sha1(bytes: &[u8]) -> String {
    let digest = STANDARD_NO_PAD.encode(Sha1::digest(bytes));
    format!(r#"W/"{:x}-{digest}""#, bytes.len())
}

/// RFC 9110 weak `If-None-Match` comparison against an `ETag`.
pub(crate) fn not_modified(if_none_match: Option<&str>, etag: &str) -> bool {
    let Some(validators) = if_none_match else {
        return false;
    };
    let current = etag.trim().strip_prefix("W/").unwrap_or(etag.trim());
    validators.split(',').any(|raw| {
        let candidate = raw.trim();
        if candidate == "*" {
            return true;
        }
        let candidate = candidate.strip_prefix("W/").unwrap_or(candidate);
        !candidate.is_empty() && candidate == current
    })
}

/// `true` when any `Cache-Control` header contains the `no-cache` directive.
pub(crate) fn has_no_cache(headers: &HeaderMap) -> bool {
    headers.get_all(header::CACHE_CONTROL).iter().any(|value| {
        value.to_str().is_ok_and(|text| {
            text.split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("no-cache"))
        })
    })
}

fn parse_quality(parameters: impl Iterator<Item = impl AsRef<str>>) -> u16 {
    let mut quality = 1000;
    for parameter in parameters {
        let parameter = parameter.as_ref().trim();
        let (name, raw) = parameter.split_once('=').unwrap_or((parameter, ""));
        if !name.trim().eq_ignore_ascii_case("q") {
            continue;
        }
        let raw = raw.trim();
        let (whole, fraction) = raw.split_once('.').unwrap_or((raw, ""));
        quality = if fraction.len() <= 3 && fraction.bytes().all(|ch| ch.is_ascii_digit()) {
            match whole {
                "0" => {
                    fraction.parse::<u16>().unwrap_or(0) * 10_u16.pow((3 - fraction.len()) as u32)
                }
                "1" if fraction.bytes().all(|ch| ch == b'0') => 1000,
                _ => 0,
            }
        } else {
            0
        };
    }
    quality
}

/// `true` when `Accept-Encoding` negotiates `gzip` (including `x-gzip` or `*`) with `q > 0`.
pub(crate) fn accepts_gzip(headers: &HeaderMap) -> bool {
    let mut combined = String::new();
    for value in headers.get_all(header::ACCEPT_ENCODING) {
        let Ok(text) = value.to_str() else {
            return false;
        };
        if !combined.is_empty() {
            combined.push(',');
        }
        combined.push_str(text);
        if combined.len() > 2048 {
            return false;
        }
    }
    if combined.is_empty() {
        return false;
    }
    accepts_gzip_value(&combined)
}

/// Parse one combined `Accept-Encoding` header string.
pub(crate) fn accepts_gzip_value(raw: &str) -> bool {
    if raw.is_empty() || raw.len() > 2048 {
        return false;
    }
    let mut qualities = HashMap::new();
    for part in raw.split(',') {
        let mut parts = part.split(';');
        let Some(coding) = parts.next() else {
            continue;
        };
        let coding = coding.trim().to_ascii_lowercase();
        if coding.is_empty() {
            continue;
        }
        qualities
            .entry(coding)
            .or_insert_with(|| parse_quality(parts));
    }
    let score = qualities
        .get("gzip")
        .or_else(|| qualities.get("x-gzip"))
        .or_else(|| qualities.get("*"))
        .copied()
        .unwrap_or(0);
    score > 0
}

/// Add `token` to `Vary` without duplicating existing tokens or overriding `Vary: *`.
pub(crate) fn append_vary(headers: &mut HeaderMap, token: &str) {
    let mut existing = Vec::new();
    for value in headers.get_all(header::VARY) {
        if let Ok(text) = value.to_str() {
            for item in text.split(',') {
                let item = item.trim();
                if item == "*" {
                    return;
                }
                if item.eq_ignore_ascii_case(token) {
                    return;
                }
                if !item.is_empty() {
                    existing.push(item.to_owned());
                }
            }
        }
    }
    existing.push(token.to_owned());
    if let Ok(value) = HeaderValue::from_str(&existing.join(", ")) {
        headers.insert(header::VARY, value);
    }
}

/// Compress `raw` into a single-member RFC 1952 gzip stream.
pub(crate) fn gzip_compress(raw: &[u8]) -> Option<Bytes> {
    let mut encoder = GzEncoder::new(Vec::with_capacity((raw.len() / 4).max(128)), GZIP_LEVEL);
    encoder.write_all(raw).ok()?;
    let out = encoder.finish().ok()?;
    Some(Bytes::from(out))
}

/// Decompress a gzip stream, using `hint_len` as initial capacity.
pub(crate) fn gzip_decompress(gz: &[u8], hint_len: usize) -> io::Result<Bytes> {
    let mut decoder = GzDecoder::new(gz);
    let mut out = Vec::with_capacity(hint_len);
    decoder.read_to_end(&mut out)?;
    Ok(Bytes::from(out))
}

/// IEEE CRC-32 matching RFC 1952 gzip trailers.
pub(crate) fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = Crc::new();
    crc.update(bytes);
    crc.sum()
}

fn gf2_matrix_times(mat: &[u32; 32], mut vec: u32) -> u32 {
    let mut sum = 0u32;
    let mut idx = 0usize;
    while vec != 0 {
        if (vec & 1) != 0 {
            sum ^= mat[idx];
        }
        vec >>= 1;
        idx += 1;
    }
    sum
}

fn gf2_matrix_square(square: &mut [u32; 32], mat: &[u32; 32]) {
    for n in 0..32 {
        square[n] = gf2_matrix_times(mat, mat[n]);
    }
}

/// Standard zlib `crc32_combine`: returns `crc32(a || b)` from `crc32(a)`, `crc32(b)`, and `b.len()`.
pub(crate) fn crc32_combine(mut crc1: u32, crc2: u32, mut len2: usize) -> u32 {
    if len2 == 0 {
        return crc1;
    }
    let mut odd = [0u32; 32];
    let mut even = [0u32; 32];
    // IEEE CRC-32 reversed polynomial.
    odd[0] = 0xedb8_8320;
    let mut row = 1u32;
    for slot in odd.iter_mut().skip(1) {
        *slot = row;
        row <<= 1;
    }
    gf2_matrix_square(&mut even, &odd);
    gf2_matrix_square(&mut odd, &even);

    loop {
        gf2_matrix_square(&mut even, &odd);
        if (len2 & 1) != 0 {
            crc1 = gf2_matrix_times(&even, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
        gf2_matrix_square(&mut odd, &even);
        if (len2 & 1) != 0 {
            crc1 = gf2_matrix_times(&odd, crc1);
        }
        len2 >>= 1;
        if len2 == 0 {
            break;
        }
    }
    crc1 ^ crc2
}

/// One cached JSON response stored in compressed form (when compressible) alongside its ETag.
#[derive(Clone)]
pub(crate) struct CachedBody {
    data: Bytes,
    raw_len: usize,
    gzipped: bool,
    pub(crate) etag: String,
}

impl CachedBody {
    /// Build from `raw` while also returning the uncompressed `Bytes` when the caller needs
    /// identity bytes on a cache miss (avoiding a redundant gunzip on the miss).
    pub(crate) fn from_raw_with_identity(raw: Vec<u8>, etag: String) -> (Self, Bytes) {
        let raw_bytes = Bytes::from(raw);
        let raw_len = raw_bytes.len();
        if raw_len >= MIN_COMPRESS_BYTES
            && let Some(gz) = gzip_compress(&raw_bytes)
            && gz.len() < raw_len
        {
            let cached = Self {
                data: gz,
                raw_len,
                gzipped: true,
                etag,
            };
            return (cached, raw_bytes);
        }
        let cached = Self {
            data: raw_bytes.clone(),
            raw_len,
            gzipped: false,
            etag,
        };
        (cached, raw_bytes)
    }

    /// Return `(bytes, is_gzip)`: O(1) shared `Bytes::clone()` when serving the stored encoding,
    /// or decompressed `Bytes` when an identity client reads a gzipped cache entry.
    pub(crate) fn serve(&self, accept_gzip: bool) -> io::Result<(Bytes, bool)> {
        if !self.gzipped {
            return Ok((self.data.clone(), false));
        }
        if accept_gzip {
            return Ok((self.data.clone(), true));
        }
        Ok((gzip_decompress(&self.data, self.raw_len)?, false))
    }

    pub(crate) fn raw_len(&self) -> usize {
        self.raw_len
    }

    pub(crate) fn stored_len(&self) -> usize {
        self.data.len()
    }

    pub(crate) fn is_gzipped(&self) -> bool {
        self.gzipped
    }

    #[cfg(test)]
    pub(crate) fn from_uncompressed(raw: Vec<u8>, etag: String) -> Self {
        let raw_len = raw.len();
        Self {
            data: Bytes::from(raw),
            raw_len,
            gzipped: false,
            etag,
        }
    }
}

#[derive(Clone)]
enum PrefixPart {
    Raw(Bytes),
    /// Byte-aligned raw DEFLATE stream terminated by `Z_SYNC_FLUSH` (`BFINAL = 0`).
    DeflateSync(Bytes),
}

#[derive(Clone)]
enum SuffixPart {
    Raw(Bytes),
    /// Standalone complete raw DEFLATE stream (`BFINAL = 1`, no back-references before byte 0).
    DeflateFinal(Bytes),
}

/// Pre-compressed spliceable single-member gzip stream for cached views that patch one small
/// clock field per request (`generatedAt` on Status, `cacheAgeSeconds` on Scoreboard).
///
/// The large static prefix and/or suffix are deflated once at cache insertion time; each warm hit
/// stitches the pre-deflated bytes with a tiny RFC 1951 stored block for the patched field and
/// combines the cached CRC-32 values in O(log N) (~50 ns) with zero recompression.
#[derive(Clone)]
pub(crate) struct SpliceGzip {
    prefix: PrefixPart,
    suffix: SuffixPart,
    prefix_len: usize,
    suffix_len: usize,
    crc_prefix: u32,
    crc_suffix: u32,
}

fn deflate_prefix_sync(prefix: &[u8]) -> Option<Bytes> {
    let mut compressor = Compress::new(GZIP_LEVEL, false);
    let mut out = Vec::with_capacity((prefix.len() / 4).max(256));
    let mut buf = [0u8; 16 * 1024];
    let mut in_pos = 0usize;
    while in_pos < prefix.len() {
        let before_in = compressor.total_in();
        let before_out = compressor.total_out();
        let status = compressor
            .compress(&prefix[in_pos..], &mut buf, FlushCompress::None)
            .ok()?;
        let consumed = (compressor.total_in() - before_in) as usize;
        let produced = (compressor.total_out() - before_out) as usize;
        in_pos += consumed;
        out.extend_from_slice(&buf[..produced]);
        if consumed == 0 && produced == 0 && status != FlateStatus::Ok {
            return None;
        }
    }
    loop {
        let before_out = compressor.total_out();
        let status = compressor
            .compress(&[], &mut buf, FlushCompress::Sync)
            .ok()?;
        let produced = (compressor.total_out() - before_out) as usize;
        out.extend_from_slice(&buf[..produced]);
        match status {
            FlateStatus::Ok if produced < buf.len() => break,
            FlateStatus::BufError if produced == 0 => break,
            FlateStatus::StreamEnd => return None,
            _ => {}
        }
    }
    // `Z_SYNC_FLUSH` always emits the 4-byte empty stored-block terminator on a byte boundary.
    if out.len() < 4 || &out[out.len() - 4..] != b"\x00\x00\xff\xff" {
        return None;
    }
    Some(Bytes::from(out))
}

fn deflate_suffix_final(suffix: &[u8]) -> Option<Bytes> {
    let mut encoder =
        DeflateEncoder::new(Vec::with_capacity((suffix.len() / 4).max(256)), GZIP_LEVEL);
    encoder.write_all(suffix).ok()?;
    let out = encoder.finish().ok()?;
    Some(Bytes::from(out))
}

fn write_stored_blocks(out: &mut Vec<u8>, data: &[u8], is_final: bool) {
    if data.is_empty() {
        if is_final {
            out.push(0x01);
            out.extend_from_slice(&0u16.to_le_bytes());
            out.extend_from_slice(&(!0u16).to_le_bytes());
        }
        return;
    }
    let mut offset = 0usize;
    while offset < data.len() {
        let end = (offset + MAX_STORED_BLOCK).min(data.len());
        let last = end == data.len();
        let chunk = &data[offset..end];
        let len = chunk.len() as u16;
        out.push(u8::from(is_final && last));
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(chunk);
        offset = end;
    }
}

impl SpliceGzip {
    /// Pre-compress `raw` split around the patchable byte range `start..end`.
    pub(crate) fn build(raw: &[u8], start: usize, end: usize) -> Option<Self> {
        if start > end || end > raw.len() {
            return None;
        }
        let prefix_bytes = &raw[..start];
        let suffix_bytes = &raw[end..];
        let compressible = raw.len() >= MIN_COMPRESS_BYTES;

        let prefix = if compressible && prefix_bytes.len() >= SPLICE_DEFLATE_MIN {
            PrefixPart::DeflateSync(deflate_prefix_sync(prefix_bytes)?)
        } else {
            PrefixPart::Raw(Bytes::copy_from_slice(prefix_bytes))
        };
        let suffix = if compressible && suffix_bytes.len() >= SPLICE_DEFLATE_MIN {
            SuffixPart::DeflateFinal(deflate_suffix_final(suffix_bytes)?)
        } else {
            SuffixPart::Raw(Bytes::copy_from_slice(suffix_bytes))
        };

        Some(Self {
            prefix,
            suffix,
            prefix_len: prefix_bytes.len(),
            suffix_len: suffix_bytes.len(),
            crc_prefix: crc32(prefix_bytes),
            crc_suffix: crc32(suffix_bytes),
        })
    }

    pub(crate) fn raw_len(&self, replacement_len: usize) -> usize {
        self.prefix_len + replacement_len + self.suffix_len
    }

    pub(crate) fn stored_len(&self) -> usize {
        let p = match &self.prefix {
            PrefixPart::Raw(b) | PrefixPart::DeflateSync(b) => b.len(),
        };
        let s = match &self.suffix {
            SuffixPart::Raw(b) | SuffixPart::DeflateFinal(b) => b.len(),
        };
        p + s
    }

    /// Assemble a single-member RFC 1952 gzip stream with `replacement` spliced in, without
    /// recompressing the static prefix or suffix.
    pub(crate) fn render_gzip(&self, replacement: &[u8]) -> Bytes {
        let mut out = Vec::with_capacity(self.stored_len() + replacement.len() + 48);
        out.extend_from_slice(&GZIP_HEADER);
        match (&self.prefix, &self.suffix) {
            (PrefixPart::Raw(prefix), SuffixPart::DeflateFinal(suffix)) => {
                // Combine small prefix + replacement into one non-final stored block, then
                // append the pre-deflated standalone suffix.
                let mut head = Vec::with_capacity(prefix.len() + replacement.len());
                head.extend_from_slice(prefix);
                head.extend_from_slice(replacement);
                write_stored_blocks(&mut out, &head, false);
                out.extend_from_slice(suffix);
            }
            (PrefixPart::DeflateSync(prefix), SuffixPart::Raw(suffix)) => {
                out.extend_from_slice(prefix);
                let mut tail = Vec::with_capacity(replacement.len() + suffix.len());
                tail.extend_from_slice(replacement);
                tail.extend_from_slice(suffix);
                write_stored_blocks(&mut out, &tail, true);
            }
            (PrefixPart::DeflateSync(prefix), SuffixPart::DeflateFinal(suffix)) => {
                out.extend_from_slice(prefix);
                write_stored_blocks(&mut out, replacement, false);
                out.extend_from_slice(suffix);
            }
            (PrefixPart::Raw(prefix), SuffixPart::Raw(suffix)) => {
                let mut whole = Vec::with_capacity(prefix.len() + replacement.len() + suffix.len());
                whole.extend_from_slice(prefix);
                whole.extend_from_slice(replacement);
                whole.extend_from_slice(suffix);
                write_stored_blocks(&mut out, &whole, true);
            }
        }
        let crc = crc32_combine(self.crc_prefix, crc32(replacement), replacement.len());
        let crc = crc32_combine(crc, self.crc_suffix, self.suffix_len);
        let isize = self.raw_len(replacement.len()) as u32;
        out.extend_from_slice(&crc.to_le_bytes());
        out.extend_from_slice(&isize.to_le_bytes());
        Bytes::from(out)
    }

    /// Assemble the uncompressed bytes with `replacement` spliced in.
    pub(crate) fn render_raw(&self, replacement: &[u8]) -> io::Result<Bytes> {
        if let (PrefixPart::Raw(prefix), SuffixPart::Raw(suffix)) = (&self.prefix, &self.suffix) {
            let mut out = Vec::with_capacity(prefix.len() + replacement.len() + suffix.len());
            out.extend_from_slice(prefix);
            out.extend_from_slice(replacement);
            out.extend_from_slice(suffix);
            return Ok(Bytes::from(out));
        }
        let gz = self.render_gzip(replacement);
        gzip_decompress(&gz, self.raw_len(replacement.len()))
    }
}

struct TagEntry {
    etag: String,
    gzip: Bytes,
}

#[derive(Default)]
struct TagCache {
    entries: VecDeque<TagEntry>,
    total_bytes: usize,
}

impl TagCache {
    fn get(&mut self, etag: &str) -> Option<Bytes> {
        let pos = self.entries.iter().position(|entry| entry.etag == etag)?;
        let entry = self.entries.remove(pos)?;
        let gzip = entry.gzip.clone();
        self.entries.push_back(entry);
        Some(gzip)
    }

    fn insert(&mut self, etag: String, gzip: Bytes) {
        if gzip.len() > TAG_CACHE_BYTES {
            return;
        }
        if let Some(pos) = self.entries.iter().position(|entry| entry.etag == etag)
            && let Some(old) = self.entries.remove(pos)
        {
            self.total_bytes = self.total_bytes.saturating_sub(old.gzip.len());
        }
        while self.entries.len() >= TAG_CACHE_ENTRIES
            || (self.total_bytes + gzip.len() > TAG_CACHE_BYTES && !self.entries.is_empty())
        {
            if let Some(old) = self.entries.pop_front() {
                self.total_bytes = self.total_bytes.saturating_sub(old.gzip.len());
            }
        }
        self.total_bytes += gzip.len();
        self.entries.push_back(TagEntry { etag, gzip });
    }
}

static TAG_CACHE: Mutex<TagCache> = Mutex::new(TagCache {
    entries: VecDeque::new(),
    total_bytes: 0,
});

/// Compress `raw` once per `etag` (sharing cached `Bytes` across warm hits) when gzip shrinks it.
pub(crate) fn compress_for_tag(etag: Option<&str>, raw: &[u8]) -> Option<Bytes> {
    if raw.len() < MIN_COMPRESS_BYTES {
        return None;
    }
    if let Some(tag) = etag
        && let Ok(mut cache) = TAG_CACHE.lock()
        && let Some(hit) = cache.get(tag)
    {
        return Some(hit);
    }
    #[cfg(test)]
    TAG_CACHE_COMPRESSIONS.fetch_add(1, Ordering::Relaxed);
    let gz = gzip_compress(raw)?;
    if gz.len() >= raw.len() {
        return None;
    }
    if let Some(tag) = etag
        && let Ok(mut cache) = TAG_CACHE.lock()
    {
        cache.insert(tag.to_owned(), gz.clone());
    }
    Some(gz)
}

fn is_json_content_type(headers: &HeaderMap) -> bool {
    headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

/// Apply gzip compression to eligible `/api/*` JSON responses that were not already pre-compressed
/// by their route owner, caching compressed bytes by `ETag`. Always strips internal hop headers.
pub(crate) async fn maybe_compress_api_json(
    mut reply: Response,
    accept_gzip: bool,
    head: bool,
) -> Response {
    let raw_len_hint = reply
        .headers_mut()
        .remove(HeaderName::from_static(RAW_LENGTH_HEADER))
        .and_then(|v| v.to_str().ok()?.parse::<usize>().ok());

    let status = reply.status();
    let already_encoded = reply.headers().contains_key(header::CONTENT_ENCODING);

    if head {
        if already_encoded {
            reply.headers_mut().remove(header::CONTENT_ENCODING);
            if let Some(raw_len) = raw_len_hint
                && let Ok(val) = HeaderValue::from_str(&raw_len.to_string())
            {
                reply.headers_mut().insert(header::CONTENT_LENGTH, val);
            }
        }
        *reply.body_mut() = Body::empty();
        return reply;
    }

    if already_encoded
        || status.as_u16() < 200
        || status == StatusCode::NO_CONTENT
        || status == StatusCode::NOT_MODIFIED
        || !is_json_content_type(reply.headers())
    {
        return reply;
    }

    if let Some(len) = reply
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok()?.parse::<usize>().ok())
        && !(MIN_COMPRESS_BYTES..=MAX_COMPRESS_BYTES).contains(&len)
    {
        return reply;
    }

    let (mut parts, body) = reply.into_parts();
    let Ok(bytes) = to_bytes(body, MAX_COMPRESS_BYTES).await else {
        return Response::from_parts(parts, Body::empty());
    };

    if bytes.len() < MIN_COMPRESS_BYTES {
        return Response::from_parts(parts, Body::from(bytes));
    }

    append_vary(&mut parts.headers, "Accept-Encoding");

    if !accept_gzip {
        return Response::from_parts(parts, Body::from(bytes));
    }

    let etag = parts
        .headers
        .get(header::ETAG)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);

    if let Some(gz) = compress_for_tag(etag.as_deref(), &bytes) {
        if let Ok(len) = HeaderValue::from_str(&gz.len().to_string()) {
            parts.headers.insert(header::CONTENT_LENGTH, len);
        }
        parts
            .headers
            .insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
        Response::from_parts(parts, Body::from(gz))
    } else {
        Response::from_parts(parts, Body::from(bytes))
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn accept_encoding_negotiation_follows_q_values() {
        for yes in [
            "gzip",
            "GZIP",
            "br, gzip, deflate",
            "br;q=1.0, gzip;q=0.5",
            "x-gzip",
            "*",
            "br, *;q=0.1",
            "deflate, gzip;q=0.001",
        ] {
            assert!(accepts_gzip_value(yes), "expected true for {yes}");
        }
        for no in [
            "",
            "identity",
            "br",
            "deflate",
            "gzip;q=0",
            "gzip;q=0.000",
            "gzip;q=0, *;q=1",
            "x-gzip;q=0",
            "*;q=0",
        ] {
            assert!(!accepts_gzip_value(no), "expected false for {no}");
        }
    }

    #[test]
    fn crc32_combine_matches_full_crc32_across_boundaries() {
        let samples: &[(&[u8], &[u8])] = &[
            (b"", b""),
            (b"hello", b""),
            (b"", b"world"),
            (b"hello ", b"world!"),
            (&vec![b'A'; 4097], &vec![b'B'; 12_345]),
            (&vec![0x5a; 100_000], &vec![0xa5; 250_000]),
        ];
        for (a, b) in samples {
            let mut both = Vec::with_capacity(a.len() + b.len());
            both.extend_from_slice(a);
            both.extend_from_slice(b);
            assert_eq!(crc32_combine(crc32(a), crc32(b), b.len()), crc32(&both));
        }
    }

    #[test]
    fn splice_gzip_round_trips_prefix_middle_and_suffix_splits() {
        // 1. Near-start splice (Status `generatedAt`).
        let mut status =
            String::from(r#"{"version":1,"generatedAt":"2026-11-25T10:00:00.000Z","rows":["#);
        for i in 0..800 {
            if i > 0 {
                status.push(',');
            }
            status.push_str(&format!(
                r#"{{"id":"w-{i:04}","state":"working","text":"payload repeat {i}"}}"#
            ));
        }
        status.push_str("]}");
        let start = status.find("2026-11-25T10:00:00.000Z").unwrap();
        let splice = SpliceGzip::build(status.as_bytes(), start, start + 24).unwrap();
        assert!(splice.stored_len() < status.len() / 5);
        for iso in ["2026-11-25T10:00:00.000Z", "2026-11-25T10:00:09.123Z"] {
            let gz = splice.render_gzip(iso.as_bytes());
            let decoded = gzip_decompress(&gz, status.len()).unwrap();
            let want = status.replacen("2026-11-25T10:00:00.000Z", iso, 1);
            assert_eq!(decoded.as_ref(), want.as_bytes());
            assert_eq!(
                splice.render_raw(iso.as_bytes()).unwrap().as_ref(),
                want.as_bytes()
            );
        }

        // 2. Near-end splice (Scoreboard `cacheAgeSeconds`).
        let mut sb = String::from(r#"{"version":1,"models":["#);
        for i in 0..1500 {
            if i > 0 {
                sb.push(',');
            }
            sb.push_str(&format!(r#"{{"model":"model-{i:04}","wins":12,"runs":20,"note":"repeated scoreboard row text"}}"#));
        }
        sb.push_str(r#"],"cacheAgeSeconds":0,"refreshing":false}"#);
        let pos = sb.rfind("\"cacheAgeSeconds\":0").unwrap() + "\"cacheAgeSeconds\":".len();
        let splice = SpliceGzip::build(sb.as_bytes(), pos, pos + 1).unwrap();
        assert!(splice.stored_len() < sb.len() / 5);
        for age in ["0", "12.345", "3600.5"] {
            let gz = splice.render_gzip(age.as_bytes());
            let decoded = gzip_decompress(&gz, sb.len() + 16).unwrap();
            let want = format!("{}{age}{}", &sb[..pos], &sb[pos + 1..]);
            assert_eq!(decoded.as_ref(), want.as_bytes());
            assert_eq!(
                splice.render_raw(age.as_bytes()).unwrap().as_ref(),
                want.as_bytes()
            );
        }

        // 3. Middle splice (both prefix and suffix >= 256 bytes).
        let mid_raw = format!(
            "{}{}{}",
            "prefix-repeat-".repeat(200),
            "SLOT",
            "suffix-repeat-".repeat(200)
        );
        let mid_pos = mid_raw.find("SLOT").unwrap();
        let splice = SpliceGzip::build(mid_raw.as_bytes(), mid_pos, mid_pos + 4).unwrap();
        let gz = splice.render_gzip(b"REPLACED-VALUE");
        let decoded = gzip_decompress(&gz, mid_raw.len() + 16).unwrap();
        assert_eq!(
            decoded.as_ref(),
            mid_raw.replace("SLOT", "REPLACED-VALUE").as_bytes()
        );
    }

    #[test]
    fn tag_cache_compresses_a_warm_answer_once_not_per_request() {
        let payload = format!(r#"{{"items":{:?}}}"#, vec!["repeated-api-json-entry"; 200]);
        let tag = r#"W/"unit-test-compress-once-tag""#;
        let before = TAG_CACHE_COMPRESSIONS.load(Ordering::Relaxed);
        let first = compress_for_tag(Some(tag), payload.as_bytes()).unwrap();
        let after_first = TAG_CACHE_COMPRESSIONS.load(Ordering::Relaxed);
        let second = compress_for_tag(Some(tag), payload.as_bytes()).unwrap();
        let after_second = TAG_CACHE_COMPRESSIONS.load(Ordering::Relaxed);

        assert_eq!(after_first, before + 1, "cold tag compresses once");
        assert_eq!(after_second, after_first, "warm tag must not recompress");
        assert_eq!(
            first.as_ptr(),
            second.as_ptr(),
            "warm tag shares Bytes buffer"
        );
        assert_eq!(
            gzip_decompress(&second, payload.len()).unwrap().as_ref(),
            payload.as_bytes()
        );
    }

    #[test]
    fn not_modified_matches_weak_strong_list_and_wildcard() {
        let etag = r#"W/"abc123""#;
        assert!(not_modified(Some(r#"W/"abc123""#), etag));
        assert!(not_modified(Some(r#""abc123""#), etag));
        assert!(not_modified(Some(r#"W/"other", W/"abc123""#), etag));
        assert!(not_modified(Some("*"), etag));
        assert!(!not_modified(Some(r#"W/"other""#), etag));
        assert!(!not_modified(None, etag));
    }

    #[test]
    fn append_vary_deduplicates_and_preserves_existing_tokens() {
        let mut headers = HeaderMap::new();
        append_vary(&mut headers, "Accept-Encoding");
        assert_eq!(headers[header::VARY], "Accept-Encoding");
        append_vary(&mut headers, "accept-encoding");
        assert_eq!(headers[header::VARY], "Accept-Encoding");

        let mut cookie = HeaderMap::new();
        cookie.insert(header::VARY, HeaderValue::from_static("Cookie"));
        append_vary(&mut cookie, "Accept-Encoding");
        assert_eq!(cookie[header::VARY], "Cookie, Accept-Encoding");
    }
}
