//! One append-only log (docs/store.md 2.4 to 2.6): an active segment, sealed segments that are
//! never edited, zstd copies of sealed segments, and a manifest holding the durable watermark
//! and the sequence lease. Recovery decides from bytes and the manifest, never from file times.

use crate::error::{Error, Result};
use crate::event::Event;
use crate::fail;
use crate::frame::{self, Problem};
use crate::fsx;
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

pub const GENERATION: u64 = 1;
pub const SEGMENT_MAX_BYTES: u64 = 8 << 20;
pub const LEASE_BLOCK: u64 = 4096;
pub const ZSTD_LEVEL: i32 = 9;
const MANIFEST_MAGIC: [u8; 8] = *b"AACMAN1\0";
const MANIFEST_FILE: &str = "manifest.v1.msgpack";
const DAY_MS: u64 = 86_400_000;

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct SegmentEntry {
    pub start_seq: u64,
    pub end_seq: u64,
    pub start_ms: u64,
    pub end_ms: u64,
    pub frames: u64,
    pub raw_bytes: u64,
    pub raw_sha256: String,
    pub zst_bytes: u64,
    pub compressed: bool,
    /// `sealed`, `degraded` (holds a disclosed hole) or `missing`.
    pub state: String,
    pub loss: Option<String>,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ActiveEntry {
    pub start_seq: u64,
    pub start_ms: u64,
    pub watermark_offset: u64,
    pub watermark_seq: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Manifest {
    pub version: u32,
    pub log: String,
    pub generation: u64,
    pub segments: Vec<SegmentEntry>,
    pub active: Option<ActiveEntry>,
    pub lease_high: u64,
    pub retired_below: u64,
    /// The survivor-prefix chain over every retired event (`""` when none retired). Absent in a
    /// manifest written before it was kept, or rebuilt after loss: the chain is then unknown.
    #[serde(default)]
    pub retired_digest: Option<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Finding {
    pub kind: String,
    pub detail: String,
    pub start_seq: Option<u64>,
    pub end_seq: Option<u64>,
}

impl Finding {
    fn new(kind: &str, detail: impl Into<String>, range: Option<(u64, u64)>) -> Finding {
        Finding {
            kind: kind.into(),
            detail: detail.into(),
            start_seq: range.map(|r| r.0),
            end_seq: range.map(|r| r.1),
        }
    }
}

struct Active {
    file: File,
    start_seq: u64,
    start_ms: u64,
    len: u64,
    last_seq: Option<u64>,
}

pub struct Log {
    pub name: String,
    short: String,
    dir: PathBuf,
    manifest: Manifest,
    active: Option<Active>,
    next_seq: u64,
    failed: bool,
}

/// One frame found by a scan.
pub struct Scanned {
    pub offset: usize,
    pub len: usize,
    pub seq: u64,
    pub observed_ms: u64,
}

/// A contiguous scan from offset 0: every valid frame, then where and why it stopped.
pub struct Scan {
    pub frames: Vec<Scanned>,
    pub valid_end: usize,
    pub stop: Option<(usize, Problem)>,
}

/// The frames of a sealed segment whose SHA-256 matches its manifest: every byte is what was
/// validated when it sealed, so only the frame prefixes and CRCs are checked, not each event.
/// `None` at the first frame that does not decode.
fn walk(buf: &[u8]) -> Option<Vec<Scanned>> {
    let mut frames = Vec::new();
    let mut offset = 0usize;
    let mut previous = None;
    while offset < buf.len() {
        let f = frame::decode(&buf[offset..], GENERATION, previous).ok()?;
        frames.push(Scanned {
            offset,
            len: f.len,
            seq: f.sequence,
            observed_ms: 0,
        });
        previous = Some(f.sequence);
        offset += f.len;
    }
    Some(frames)
}

pub fn scan(buf: &[u8]) -> Scan {
    let mut frames = Vec::new();
    let mut offset = 0usize;
    let mut previous = None;
    while offset < buf.len() {
        match frame::decode(&buf[offset..], GENERATION, previous) {
            Ok(f) => match Event::decode(&f) {
                Ok(event) => {
                    frames.push(Scanned {
                        offset,
                        len: f.len,
                        seq: f.sequence,
                        observed_ms: event.observed_at_ms,
                    });
                    previous = Some(f.sequence);
                    offset += f.len;
                }
                Err(_) => {
                    return Scan {
                        frames,
                        valid_end: offset,
                        stop: Some((offset, Problem::Corrupt("event"))),
                    };
                }
            },
            Err(problem) => {
                return Scan {
                    frames,
                    valid_end: offset,
                    stop: Some((offset, problem)),
                };
            }
        }
    }
    Scan {
        frames,
        valid_end: offset,
        stop: None,
    }
}

/// Every fully validated frame, resynchronizing past holes at the next frame that passes every
/// check (docs/store.md 2.6). Returns the frames and the byte ranges that held no valid frame.
pub fn salvage(buf: &[u8]) -> (Vec<Scanned>, Vec<(usize, usize)>) {
    let mut frames: Vec<Scanned> = Vec::new();
    let mut holes = Vec::new();
    let mut offset = 0usize;
    while offset < buf.len() {
        let previous = frames.last().map(|f| f.seq);
        let ok = frame::decode(&buf[offset..], GENERATION, previous)
            .ok()
            .and_then(|f| Event::decode(&f).ok().map(|e| (f, e)));
        match ok {
            Some((f, event)) => {
                frames.push(Scanned {
                    offset,
                    len: f.len,
                    seq: f.sequence,
                    observed_ms: event.observed_at_ms,
                });
                offset += f.len;
            }
            None => {
                let hole_start = offset;
                let mut next = offset + 1;
                loop {
                    match find_magic(buf, next) {
                        None => {
                            next = buf.len();
                            break;
                        }
                        Some(at) => {
                            let valid = frame::decode(&buf[at..], GENERATION, previous)
                                .ok()
                                .map(|f| Event::decode(&f).is_ok())
                                .unwrap_or(false);
                            if valid {
                                next = at;
                                break;
                            }
                            next = at + 1;
                        }
                    }
                }
                holes.push((hole_start, next));
                offset = next;
            }
        }
    }
    (frames, holes)
}

fn find_magic(buf: &[u8], from: usize) -> Option<usize> {
    if from >= buf.len() {
        return None;
    }
    buf[from..]
        .windows(frame::MAGIC.len())
        .position(|w| w == frame::MAGIC)
        .map(|p| p + from)
}

fn active_name(start: u64) -> String {
    format!("seg-{GENERATION:06}-{start:020}.active")
}
fn sealed_name(start: u64, end: u64) -> String {
    format!("seg-{GENERATION:06}-{start:020}-{end:020}.seglog")
}
fn zst_name(start: u64, end: u64) -> String {
    format!("{}.zst", sealed_name(start, end))
}

/// (start, end, kind) of a segment file name.
fn parse_name(name: &str) -> Option<(u64, Option<u64>, &'static str)> {
    let rest = name.strip_prefix("seg-")?;
    let (generation, rest) = rest.split_once('-')?;
    if generation.parse::<u64>().ok()? != GENERATION {
        return None;
    }
    if let Some(start) = rest.strip_suffix(".active") {
        return Some((start.parse().ok()?, None, "active"));
    }
    let (body, kind) = if let Some(b) = rest.strip_suffix(".seglog.zst") {
        (b, "zst")
    } else {
        let b = rest.strip_suffix(".seglog")?;
        (b, "seglog")
    };
    let (start, end) = body.split_once('-')?;
    Some((start.parse().ok()?, Some(end.parse().ok()?), kind))
}

pub fn read_manifest(dir: &Path) -> Result<Option<Manifest>> {
    let path = dir.join(MANIFEST_FILE);
    let bytes = match fs::read(&path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    decode_manifest(&bytes).map(Some)
}

fn decode_manifest(bytes: &[u8]) -> Result<Manifest> {
    if bytes.len() < 12 || bytes[..8] != MANIFEST_MAGIC {
        fail!("corrupt", "manifest magic");
    }
    let crc = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    if frame::crc32(&bytes[12..]) != crc {
        fail!("corrupt", "manifest checksum");
    }
    let manifest: Manifest = crate::msgpack::from_bytes(&bytes[12..])?;
    if manifest.version != 1 {
        fail!("newer_store", "manifest version {}", manifest.version);
    }
    if manifest.generation != GENERATION {
        fail!("newer_store", "segment generation {}", manifest.generation);
    }
    Ok(manifest)
}

fn encode_manifest(manifest: &Manifest) -> Result<Vec<u8>> {
    let body = crate::msgpack::to_bytes(manifest)?;
    let mut out = Vec::with_capacity(body.len() + 12);
    out.extend_from_slice(&MANIFEST_MAGIC);
    out.extend_from_slice(&frame::crc32(&body).to_le_bytes());
    out.extend_from_slice(&body);
    Ok(out)
}

/// Raw bytes of a sealed segment, decompressed when needed, with its SHA-256.
pub fn read_segment_bytes(dir: &Path, entry: &SegmentEntry) -> Result<Vec<u8>> {
    if entry.compressed {
        let file = File::open(dir.join(zst_name(entry.start_seq, entry.end_seq)))?;
        let mut decoder = zstd::stream::read::Decoder::new(file)?;
        let mut out = Vec::with_capacity(entry.raw_bytes as usize);
        // At most the segment size plus a frame: a bomb cannot grow past it.
        (&mut decoder)
            .take(SEGMENT_MAX_BYTES + 2 * frame::MAX_PAYLOAD as u64)
            .read_to_end(&mut out)?;
        Ok(out)
    } else {
        Ok(fs::read(dir.join(sealed_name(entry.start_seq, entry.end_seq)))?)
    }
}

impl Log {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn last_seq(&self) -> Option<u64> {
        self.active
            .as_ref()
            .and_then(|a| a.last_seq)
            .or_else(|| self.manifest.segments.iter().map(|s| s.end_seq).max())
    }

    /// Opens (or creates) the log in `dir`, recovering it first. `short` prefixes event ids.
    pub fn open(dir: &Path, name: &str, short: &str) -> Result<(Log, Vec<Finding>)> {
        fsx::private_dir(dir)?;
        fsx::remove_temporaries(dir)?;
        let mut findings = Vec::new();
        let manifest = match read_manifest(dir) {
            Ok(Some(m)) => {
                if m.log != name {
                    fail!("corrupt", "manifest belongs to log {}", m.log);
                }
                m
            }
            Ok(None) => Manifest {
                version: 1,
                log: name.to_string(),
                generation: GENERATION,
                segments: Vec::new(),
                active: None,
                lease_high: 1,
                retired_below: 0,
                retired_digest: Some(String::new()),
            },
            Err(e) if e.code == "newer_store" => return Err(e),
            Err(e) => {
                findings.push(Finding::new(
                    "manifest_rebuilt",
                    format!("manifest unreadable: {}", e.message),
                    None,
                ));
                Manifest {
                    version: 1,
                    log: name.to_string(),
                    generation: GENERATION,
                    segments: Vec::new(),
                    active: None,
                    lease_high: 1,
                    retired_below: 0,
                    retired_digest: None,
                }
            }
        };
        let mut log = Log {
            name: name.to_string(),
            short: short.to_string(),
            dir: dir.to_path_buf(),
            manifest,
            active: None,
            next_seq: 1,
            failed: false,
        };
        log.reconcile(&mut findings)?;
        // Abandon the rest of the previous lease: a sequence is never issued twice.
        let seen = log.last_seq().map(|s| s + 1).unwrap_or(1);
        log.next_seq = log.manifest.lease_high.max(seen);
        log.manifest.lease_high = log.next_seq + LEASE_BLOCK;
        log.write_manifest()?;
        Ok((log, findings))
    }

    fn reconcile(&mut self, findings: &mut Vec<Finding>) -> Result<()> {
        let mut files: Vec<(String, u64, Option<u64>, &'static str)> = Vec::new();
        for entry in fs::read_dir(&self.dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if let Some((start, end, kind)) = parse_name(&name) {
                files.push((name, start, end, kind));
            }
        }
        let exists = |name: &str| files.iter().any(|f| f.0 == name);

        // Sealed segments the manifest lists.
        let mut segments = std::mem::take(&mut self.manifest.segments);
        for entry in segments.iter_mut() {
            let raw = sealed_name(entry.start_seq, entry.end_seq);
            let zst = zst_name(entry.start_seq, entry.end_seq);
            if entry.compressed {
                if !exists(&zst) {
                    entry.state = "missing".into();
                    findings.push(Finding::new(
                        "segment_missing",
                        zst.clone(),
                        Some((entry.start_seq, entry.end_seq)),
                    ));
                } else if exists(&raw) {
                    // Compression finished but the raw copy was not removed yet.
                    fs::remove_file(self.dir.join(&raw))?;
                }
            } else if exists(&raw) {
                if exists(&zst) {
                    // A compressed copy the manifest never published.
                    fs::remove_file(self.dir.join(&zst))?;
                }
            } else {
                if exists(&zst) {
                    let mut probe = entry.clone();
                    probe.compressed = true;
                    match read_segment_bytes(&self.dir, &probe) {
                        Ok(bytes) if fsx::sha256_hex(&bytes) == entry.raw_sha256 => {
                            entry.compressed = true;
                            entry.zst_bytes = fs::metadata(self.dir.join(&zst))?.len();
                        }
                        _ => {
                            entry.state = "missing".into();
                            findings.push(Finding::new(
                                "segment_missing",
                                raw.clone(),
                                Some((entry.start_seq, entry.end_seq)),
                            ));
                        }
                    }
                } else {
                    entry.state = "missing".into();
                    findings.push(Finding::new(
                        "segment_missing",
                        raw.clone(),
                        Some((entry.start_seq, entry.end_seq)),
                    ));
                }
            }
        }
        self.manifest.segments = segments;

        // Files the manifest does not list: retired leftovers, a seal that renamed before the
        // manifest was published, or segments an unreadable manifest lost.
        let listed: Vec<u64> = self.manifest.segments.iter().map(|s| s.start_seq).collect();
        let active_start = self.manifest.active.as_ref().map(|a| a.start_seq);
        for (name, start, end, kind) in files.iter().cloned() {
            if kind == "active" || listed.contains(&start) {
                continue;
            }
            let end = end.unwrap_or(start);
            if end < self.manifest.retired_below {
                fs::remove_file(self.dir.join(&name))?;
                continue;
            }
            if kind == "zst" && files.iter().any(|f| f.1 == start && f.3 == "seglog") {
                // Prefer the raw file; the compressed copy was never published.
                fs::remove_file(self.dir.join(&name))?;
                continue;
            }
            let bytes = if kind == "zst" {
                let probe = SegmentEntry {
                    start_seq: start,
                    end_seq: end,
                    start_ms: 0,
                    end_ms: 0,
                    frames: 0,
                    raw_bytes: SEGMENT_MAX_BYTES,
                    raw_sha256: String::new(),
                    zst_bytes: 0,
                    compressed: true,
                    state: "sealed".into(),
                    loss: None,
                };
                read_segment_bytes(&self.dir, &probe)?
            } else {
                fs::read(self.dir.join(&name))?
            };
            let entry = self.entry_from_bytes(start, end, &bytes, kind == "zst", findings)?;
            if Some(start) == active_start {
                self.manifest.active = None;
            } else {
                findings.push(Finding::new(
                    "segment_adopted",
                    name.clone(),
                    Some((entry.start_seq, entry.end_seq)),
                ));
            }
            self.manifest.segments.push(entry);
        }
        self.manifest.segments.sort_by_key(|s| s.start_seq);

        // The active segment.
        let active_file = files.iter().find(|f| f.3 == "active").map(|f| (f.0.clone(), f.1));
        let others: Vec<_> = files.iter().filter(|f| f.3 == "active").skip(1).cloned().collect();
        if let Some((name, ..)) = others.into_iter().next() {
            fail!("corrupt", "two active segments ({name})");
        }
        match active_file {
            None => {
                if let Some(a) = self.manifest.active.take()
                    && a.watermark_offset > 0
                {
                    findings.push(Finding::new(
                        "active_missing",
                        "the active segment is gone",
                        Some((a.start_seq, a.watermark_seq)),
                    ));
                }
            }
            Some((name, start)) => {
                let watermark = match &self.manifest.active {
                    Some(a) if a.start_seq == start => a.watermark_offset,
                    // Adopted without a manifest entry: every valid frame is kept.
                    _ => 0,
                };
                self.recover_active(&name, start, watermark, findings)?;
            }
        }
        Ok(())
    }

    fn entry_from_bytes(
        &self,
        start: u64,
        end_seq: u64,
        bytes: &[u8],
        compressed: bool,
        findings: &mut Vec<Finding>,
    ) -> Result<SegmentEntry> {
        let (frames, holes) = salvage(bytes);
        let first = frames.first();
        let last = frames.last();
        let mut entry = SegmentEntry {
            start_seq: start,
            end_seq,
            start_ms: first.map(|f| f.observed_ms).unwrap_or(0),
            end_ms: last.map(|f| f.observed_ms).unwrap_or(0),
            frames: frames.len() as u64,
            raw_bytes: bytes.len() as u64,
            raw_sha256: fsx::sha256_hex(bytes),
            zst_bytes: 0,
            compressed,
            state: "sealed".into(),
            loss: None,
        };
        let named_wrong = last.map(|f| f.seq) != Some(end_seq) || first.map(|f| f.seq) != Some(start);
        if !holes.is_empty() || named_wrong {
            entry.state = "degraded".into();
            entry.loss = Some(if holes.is_empty() {
                "frames do not match the segment name".to_string()
            } else {
                format!("{} hole(s), first at byte {}", holes.len(), holes[0].0)
            });
            findings.push(Finding::new(
                "segment_degraded",
                entry.loss.clone().unwrap(),
                Some((start, end_seq)),
            ));
        }
        if compressed {
            entry.zst_bytes = fs::metadata(self.dir.join(zst_name(start, end_seq)))?.len();
        }
        Ok(entry)
    }

    fn recover_active(&mut self, name: &str, start: u64, watermark: u64, findings: &mut Vec<Finding>) -> Result<()> {
        let path = self.dir.join(name);
        let mut file = fsx::open_private(&path, false)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let mut result = scan(&bytes);
        if result.stop.is_none() && (bytes.len() as u64) < watermark {
            // A clean cut at a frame boundary, but below the durable watermark: acknowledged
            // frames are gone, which is loss, never a torn tail.
            result.stop = Some((bytes.len(), Problem::Incomplete));
        }
        if let Some((offset, problem)) = &result.stop {
            if *offset as u64 >= watermark {
                // A torn or unacknowledged tail: keep the valid frames, cut the rest.
                file.set_len(*offset as u64)?;
                file.sync_all()?;
                findings.push(Finding::new(
                    "torn_tail_truncated",
                    format!(
                        "{} bytes after the last valid frame ({problem:?})",
                        bytes.len() - offset
                    ),
                    None,
                ));
                bytes.truncate(*offset);
            } else {
                // Loss at or below the durable watermark: never repaired, sealed degraded.
                drop(file);
                let (frames, holes) = salvage(&bytes);
                let end = frames.last().map(|f| f.seq).unwrap_or(start);
                let sealed = sealed_name(start, end);
                fs::rename(&path, self.dir.join(&sealed))?;
                fsx::sync_dir(&self.dir)?;
                let mut entry = SegmentEntry {
                    start_seq: start,
                    end_seq: end,
                    start_ms: frames.first().map(|f| f.observed_ms).unwrap_or(0),
                    end_ms: frames.last().map(|f| f.observed_ms).unwrap_or(0),
                    frames: frames.len() as u64,
                    raw_bytes: bytes.len() as u64,
                    raw_sha256: fsx::sha256_hex(&bytes),
                    zst_bytes: 0,
                    compressed: false,
                    state: "degraded".into(),
                    loss: Some(format!(
                        "{problem:?} at byte {offset}, below the durable watermark {watermark}; {} hole(s)",
                        holes.len()
                    )),
                };
                if holes.is_empty() {
                    entry.loss = Some(format!("{problem:?} at byte {offset}"));
                }
                findings.push(Finding::new(
                    "active_degraded",
                    entry.loss.clone().unwrap(),
                    Some((start, end)),
                ));
                self.manifest.segments.push(entry);
                self.manifest.segments.sort_by_key(|s| s.start_seq);
                self.manifest.active = None;
                return Ok(());
            }
        }
        if result.frames.is_empty() {
            drop(file);
            fs::remove_file(&path)?;
            self.manifest.active = None;
            return Ok(());
        }
        let len = bytes.len() as u64;
        let last = result.frames.last().unwrap();
        let start_ms = result.frames[0].observed_ms;
        self.active = Some(Active {
            file,
            start_seq: start,
            start_ms,
            len,
            last_seq: Some(last.seq),
        });
        self.manifest.active = Some(ActiveEntry {
            start_seq: start,
            start_ms,
            watermark_offset: len,
            watermark_seq: last.seq,
        });
        Ok(())
    }

    pub fn write_manifest(&mut self) -> Result<()> {
        let bytes = encode_manifest(&self.manifest)?;
        fsx::write_atomic(&self.dir.join(MANIFEST_FILE), &bytes)
    }

    /// Reserves the next sequence for `event`, extending the durable lease first when needed.
    pub fn assign(&mut self, event: &mut Event) -> Result<()> {
        if self.failed {
            fail!("writer_closed", "the {} log stopped after a failed write", self.name);
        }
        if self.next_seq >= self.manifest.lease_high {
            self.manifest.lease_high = self.next_seq + LEASE_BLOCK;
            self.write_manifest()?;
        }
        event.sequence_id = self.next_seq;
        event.event_id = format!("{}-{:016x}", self.short, self.next_seq);
        self.next_seq += 1;
        Ok(())
    }

    /// Writes assigned events durably: frames, `sync_all`, then the manifest watermark.
    /// Only when this returns are the events acknowledged.
    pub fn write(&mut self, events: &[Event]) -> Result<()> {
        if self.failed {
            fail!("writer_closed", "the {} log stopped after a failed write", self.name);
        }
        if events.is_empty() {
            return Ok(());
        }
        let result = self.write_inner(events);
        if result.is_err() {
            self.failed = true;
            if let Some(active) = self.active.as_mut() {
                let _ = active
                    .file
                    .set_len(self.manifest.active.as_ref().map(|a| a.watermark_offset).unwrap_or(0));
            }
        }
        result
    }

    fn write_inner(&mut self, events: &[Event]) -> Result<()> {
        for event in events {
            let header = event.header_bytes()?;
            let payload = event.payload_bytes()?;
            if header.len() > frame::MAX_HEADER || payload.len() > frame::MAX_PAYLOAD {
                fail!("too_large", "event {} is over the frame caps", event.event_type);
            }
            let bytes = frame::encode(GENERATION, event.sequence_id, &header, &payload);
            if let Some(active) = &self.active {
                let new_day = event.observed_at_ms / DAY_MS != active.start_ms / DAY_MS;
                if active.len + bytes.len() as u64 > SEGMENT_MAX_BYTES || new_day {
                    self.seal()?;
                }
            }
            if self.active.is_none() {
                let path = self.dir.join(active_name(event.sequence_id));
                let file = fsx::open_private(&path, true)?;
                file.set_len(0)?;
                fsx::sync_dir(&self.dir)?;
                self.active = Some(Active {
                    file,
                    start_seq: event.sequence_id,
                    start_ms: event.observed_at_ms,
                    len: 0,
                    last_seq: None,
                });
            }
            let active = self.active.as_mut().unwrap();
            active.file.seek(SeekFrom::Start(active.len))?;
            active.file.write_all(&bytes)?;
            active.len += bytes.len() as u64;
            active.last_seq = Some(event.sequence_id);
        }
        let active = self.active.as_mut().unwrap();
        active.file.sync_all()?;
        self.manifest.active = Some(ActiveEntry {
            start_seq: active.start_seq,
            start_ms: active.start_ms,
            watermark_offset: active.len,
            watermark_seq: active.last_seq.unwrap_or(0),
        });
        self.write_manifest()
    }

    /// Closes the active segment: fsync, rename to its sealed name, publish the manifest.
    pub fn seal(&mut self) -> Result<()> {
        let Some(active) = self.active.take() else {
            return Ok(());
        };
        let Some(last) = active.last_seq else {
            drop(active.file);
            let _ = fs::remove_file(self.dir.join(active_name(active.start_seq)));
            self.manifest.active = None;
            return self.write_manifest();
        };
        active.file.sync_all()?;
        let mut file = active.file;
        file.seek(SeekFrom::Start(0))?;
        let mut bytes = Vec::with_capacity(active.len as usize);
        file.read_to_end(&mut bytes)?;
        drop(file);
        let scanned = scan(&bytes);
        fs::rename(
            self.dir.join(active_name(active.start_seq)),
            self.dir.join(sealed_name(active.start_seq, last)),
        )?;
        fsx::sync_dir(&self.dir)?;
        self.manifest.segments.push(SegmentEntry {
            start_seq: active.start_seq,
            end_seq: last,
            start_ms: active.start_ms,
            end_ms: scanned.frames.last().map(|f| f.observed_ms).unwrap_or(active.start_ms),
            frames: scanned.frames.len() as u64,
            raw_bytes: bytes.len() as u64,
            raw_sha256: fsx::sha256_hex(&bytes),
            zst_bytes: 0,
            compressed: false,
            state: "sealed".into(),
            loss: None,
        });
        self.manifest.active = None;
        self.write_manifest()
    }

    /// Sealed raw segments waiting for compression: (start, raw path, expected SHA-256).
    pub fn compress_candidates(&self) -> Vec<(u64, PathBuf, String)> {
        self.manifest
            .segments
            .iter()
            .filter(|s| !s.compressed && s.state == "sealed")
            .map(|s| {
                (
                    s.start_seq,
                    self.dir.join(sealed_name(s.start_seq, s.end_seq)),
                    s.raw_sha256.clone(),
                )
            })
            .collect()
    }

    /// Compresses a sealed file to a temporary file and proves it decodes to the same bytes.
    /// Needs no lock: sealed files never change.
    pub fn compress_file(raw: &Path, sha: &str) -> Result<(PathBuf, u64)> {
        let bytes = fs::read(raw)?;
        if fsx::sha256_hex(&bytes) != sha {
            fail!("corrupt", "sealed segment does not match its manifest hash");
        }
        let temp = fsx::temp_path(&raw.with_extension("seglog.zst"));
        let mut encoder = zstd::stream::write::Encoder::new(fsx::open_private(&temp, true)?, ZSTD_LEVEL)?;
        encoder.include_checksum(true)?;
        encoder.set_pledged_src_size(Some(bytes.len() as u64))?;
        encoder.write_all(&bytes)?;
        let file = encoder.finish()?;
        file.sync_all()?;
        drop(file);
        let mut decoded = Vec::with_capacity(bytes.len());
        zstd::stream::read::Decoder::new(File::open(&temp)?)?.read_to_end(&mut decoded)?;
        if fsx::sha256_hex(&decoded) != sha {
            let _ = fs::remove_file(&temp);
            fail!("corrupt", "compressed copy does not decode to the sealed bytes");
        }
        let len = fs::metadata(&temp)?.len();
        Ok((temp, len))
    }

    /// Publishes a verified compressed copy, then removes the raw file.
    pub fn finish_compress(&mut self, start: u64, temp: &Path, zst_bytes: u64) -> Result<()> {
        let Some(entry) = self
            .manifest
            .segments
            .iter_mut()
            .find(|s| s.start_seq == start && !s.compressed)
        else {
            let _ = fs::remove_file(temp);
            return Ok(());
        };
        let (s, e) = (entry.start_seq, entry.end_seq);
        fs::rename(temp, self.dir.join(zst_name(s, e)))?;
        fsx::sync_dir(&self.dir)?;
        entry.compressed = true;
        entry.zst_bytes = zst_bytes;
        self.write_manifest()?;
        fs::remove_file(self.dir.join(sealed_name(s, e)))?;
        fsx::sync_dir(&self.dir)
    }

    /// Retires the oldest sealed segments up to and including `through_start`: the manifest
    /// first, then the files (an interrupted retire finishes at the next open).
    pub fn retire_through(&mut self, through_start: u64) -> Result<Vec<SegmentEntry>> {
        let keep: Vec<SegmentEntry> = self
            .manifest
            .segments
            .iter()
            .filter(|s| s.start_seq > through_start)
            .cloned()
            .collect();
        let gone: Vec<SegmentEntry> = self
            .manifest
            .segments
            .iter()
            .filter(|s| s.start_seq <= through_start)
            .cloned()
            .collect();
        if gone.is_empty() {
            return Ok(gone);
        }
        // The chain moves past the retired events, so projections built from them still match.
        let mut digest = self.base_digest();
        for entry in &gone {
            let (bytes, frames, _) = self.segment_frames(entry);
            for f in frames {
                let event = Event::decode(
                    &frame::decode(&bytes[f.offset..], GENERATION, None)
                        .map_err(|p| Error::new("corrupt", format!("{p:?}")))?,
                )?;
                digest = digest.map(|d| crate::db::chain_digest(&d, event.sequence_id, &event.event_id));
            }
        }
        self.manifest.retired_digest = digest;
        self.manifest.retired_below = gone
            .iter()
            .map(|s| s.end_seq + 1)
            .max()
            .unwrap_or(0)
            .max(self.manifest.retired_below);
        self.manifest.segments = keep;
        self.write_manifest()?;
        for s in &gone {
            let _ = fs::remove_file(self.dir.join(sealed_name(s.start_seq, s.end_seq)));
            let _ = fs::remove_file(self.dir.join(zst_name(s.start_seq, s.end_seq)));
        }
        fsx::sync_dir(&self.dir)?;
        Ok(gone)
    }

    /// Re-hashes every sealed segment; a mismatch marks it degraded (bytes untouched).
    pub fn verify_sealed(&mut self) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let mut changed = false;
        for i in 0..self.manifest.segments.len() {
            let entry = self.manifest.segments[i].clone();
            if entry.state != "sealed" {
                continue;
            }
            let ok = read_segment_bytes(&self.dir, &entry)
                .map(|b| fsx::sha256_hex(&b) == entry.raw_sha256)
                .unwrap_or(false);
            if !ok {
                self.manifest.segments[i].state = "degraded".into();
                self.manifest.segments[i].loss = Some("hash mismatch".into());
                findings.push(Finding::new(
                    "segment_degraded",
                    "hash mismatch",
                    Some((entry.start_seq, entry.end_seq)),
                ));
                changed = true;
            }
        }
        if changed {
            self.write_manifest()?;
        }
        Ok(findings)
    }

    /// Calls `visit` for every event with a sequence above `after`, in order: sealed segments
    /// (decompressed, hash-checked, degraded ones salvaged), then the active segment.
    /// The survivor-prefix chain over the retired events, or `None` when it is not known.
    pub fn base_digest(&self) -> Option<String> {
        if self.manifest.retired_below == 0 {
            return Some(String::new());
        }
        self.manifest.retired_digest.clone()
    }

    /// Records that projections rebuilt from the surviving events start their chain at `digest`.
    pub fn set_base_digest(&mut self, digest: &str) -> Result<()> {
        if self.base_digest().as_deref() == Some(digest) {
            return Ok(());
        }
        self.manifest.retired_digest = Some(digest.to_string());
        self.write_manifest()
    }

    /// A sealed segment's bytes and its valid frames, with what was wrong with it.
    fn segment_frames(&self, entry: &SegmentEntry) -> (Vec<u8>, Vec<Scanned>, Option<Finding>) {
        let range = Some((entry.start_seq, entry.end_seq));
        if entry.state == "missing" {
            return (
                Vec::new(),
                Vec::new(),
                Some(Finding::new("segment_missing", "skipped", range)),
            );
        }
        let bytes = match read_segment_bytes(&self.dir, entry) {
            Ok(b) => b,
            Err(e) => {
                return (
                    Vec::new(),
                    Vec::new(),
                    Some(Finding::new("segment_unreadable", e.message, range)),
                );
            }
        };
        if entry.state == "sealed" && fsx::sha256_hex(&bytes) == entry.raw_sha256 {
            let frames = walk(&bytes).unwrap_or_else(|| salvage(&bytes).0);
            return (bytes, frames, None);
        }
        let finding =
            (entry.state == "sealed").then(|| Finding::new("segment_degraded", "hash mismatch on read", range));
        let frames = salvage(&bytes).0;
        (bytes, frames, finding)
    }

    /// Visits every surviving frame after `after` (all of them with `None`), oldest first.
    fn each_frame(
        &self,
        after: Option<u64>,
        mut visit: impl FnMut(&frame::FrameRef<'_>) -> Result<()>,
    ) -> Result<Vec<Finding>> {
        let mut findings = Vec::new();
        let floor = after.unwrap_or(0);
        fn decode<'b>(bytes: &'b [u8], f: &Scanned) -> Result<frame::FrameRef<'b>> {
            frame::decode(&bytes[f.offset..], GENERATION, None).map_err(|p| Error::new("corrupt", format!("{p:?}")))
        }
        for entry in &self.manifest.segments {
            if after.is_some() && entry.end_seq <= floor {
                continue;
            }
            let (bytes, frames, finding) = self.segment_frames(entry);
            findings.extend(finding);
            for f in frames.iter().filter(|f| after.is_none() || f.seq > floor) {
                visit(&decode(&bytes, f)?)?;
            }
        }
        if let Some(active) = &self.active {
            let mut bytes = Vec::with_capacity(active.len as usize);
            File::open(self.dir.join(active_name(active.start_seq)))?
                .take(active.len)
                .read_to_end(&mut bytes)?;
            for f in scan(&bytes).frames.iter().filter(|f| after.is_none() || f.seq > floor) {
                visit(&decode(&bytes, f)?)?;
            }
        }
        Ok(findings)
    }

    /// Visits every surviving event after `after` (all of them with `None`), oldest first.
    pub fn read_after(&self, after: Option<u64>, mut visit: impl FnMut(Event) -> Result<()>) -> Result<Vec<Finding>> {
        self.each_frame(after, |f| visit(Event::decode(f)?))
    }

    /// Visits every surviving event's sequence and id, reading only the header of each frame the
    /// scan has already checked (its CRCs included).
    pub fn read_ids(&self, mut visit: impl FnMut(u64, &str)) -> Result<Vec<Finding>> {
        fn event_id(bytes: &[u8], f: &Scanned) -> Result<String> {
            let at = f.offset;
            let header_len = u16::from_le_bytes([bytes[at + 9], bytes[at + 10]]) as usize;
            let header = &bytes[at + frame::PREFIX_LEN..at + frame::PREFIX_LEN + header_len];
            match crate::msgpack::decode(header)? {
                crate::msgpack::Value::Array(mut items) if items.len() == 8 => match items.swap_remove(1) {
                    crate::msgpack::Value::Str(id) => Ok(id),
                    _ => fail!("corrupt", "header event id"),
                },
                _ => fail!("corrupt", "header is not an eight-element tuple"),
            }
        }
        let mut findings = Vec::new();
        for entry in &self.manifest.segments {
            let (bytes, frames, finding) = self.segment_frames(entry);
            findings.extend(finding);
            for f in &frames {
                visit(f.seq, &event_id(&bytes, f)?);
            }
        }
        if let Some(active) = &self.active {
            let mut bytes = Vec::with_capacity(active.len as usize);
            File::open(self.dir.join(active_name(active.start_seq)))?
                .take(active.len)
                .read_to_end(&mut bytes)?;
            for f in &scan(&bytes).frames {
                visit(f.seq, &event_id(&bytes, f)?);
            }
        }
        Ok(findings)
    }

    pub fn info(&self) -> serde_json::Value {
        let sealed_raw: u64 = self.manifest.segments.iter().map(|s| s.raw_bytes).sum();
        let on_disk: u64 = self
            .manifest
            .segments
            .iter()
            .map(|s| if s.compressed { s.zst_bytes } else { s.raw_bytes })
            .sum::<u64>()
            + self.active.as_ref().map(|a| a.len).unwrap_or(0);
        serde_json::json!({
            "name": self.name,
            "segments": self.manifest.segments.len(),
            "sealedRawBytes": sealed_raw,
            "bytesOnDisk": on_disk,
            "activeBytes": self.active.as_ref().map(|a| a.len).unwrap_or(0),
            "lastSequence": self.last_seq(),
            "watermark": self.manifest.active.as_ref().map(|a| a.watermark_seq),
            "retiredBelow": self.manifest.retired_below,
            "oldestMs": self.manifest.segments.first().map(|s| s.start_ms).or(self.active.as_ref().map(|a| a.start_ms)),
            "degraded": self.manifest.segments.iter().filter(|s| s.state != "sealed").map(|s| serde_json::json!({
                "startSeq": s.start_seq, "endSeq": s.end_seq, "state": s.state, "loss": s.loss,
            })).collect::<Vec<_>>(),
            "writerClosed": self.failed,
        })
    }

    /// Paths of every file a backup needs: sealed segment files (immutable) and the active
    /// segment's acknowledged length.
    pub fn backup_files(&self) -> (Vec<(String, PathBuf, String, u64)>, Option<(String, PathBuf, u64)>) {
        let sealed = self
            .manifest
            .segments
            .iter()
            .filter(|s| s.state != "missing")
            .map(|s| {
                let name = if s.compressed {
                    zst_name(s.start_seq, s.end_seq)
                } else {
                    sealed_name(s.start_seq, s.end_seq)
                };
                (name.clone(), self.dir.join(&name), s.raw_sha256.clone(), s.raw_bytes)
            })
            .collect();
        let active = self.manifest.active.as_ref().map(|a| {
            let name = active_name(a.start_seq);
            (name.clone(), self.dir.join(name), a.watermark_offset)
        });
        (sealed, active)
    }

    pub fn manifest_bytes(&self) -> Result<Vec<u8>> {
        encode_manifest(&self.manifest)
    }
}
