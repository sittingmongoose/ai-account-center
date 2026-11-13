//! Reading logs: whole files, line by line, with the Python helper's caps and copy rules.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod cursor;
pub mod devin;
pub mod muse;
pub mod omp;
pub mod sqlite;
pub mod zcode;

use crate::collector::{self, Collector, MAX_FILES, MAX_LINE_BYTES};
use crate::fsx;
use crate::util::{self, path_text};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::Instant;

/// Per-file parser state (the Python helper's `box`).
#[derive(Default)]
pub struct LineBox {
    // Codex
    pub session: Option<String>,
    pub provider: Option<String>,
    pub model: Option<String>,
    pub cwd: Option<String>,
    pub previous: Option<[f64; 4]>,
    // Claude: the response in progress (key, model, hour, tokens, epoch ms, session, offset).
    pub claude_pending: Option<claude::Pending>,
    /// Byte offset of the line being parsed (per-message keys and `msgThrough`).
    pub offset: u64,
    /// Lines before this offset were sent as messages by an earlier scan.
    pub message_from: u64,
    /// Decoded byte offset after the last complete line: where a read starts (a resumed one past
    /// 0) and, after a cut, where the next one resumes (DESIGN 16.1).
    pub through: u64,
    /// Set when a resumed read could not trust its saved place and read the file from 0 instead.
    pub restarted: bool,
}

impl LineBox {
    /// Codex's carry-over between lines, for a read the deadline cut. No identifier: the session
    /// travels as a digest only, checked against the log's own first line on resume.
    pub fn codex_state(&self) -> Value {
        json!({
            "model": self.model,
            "provider": self.provider,
            "previous": self.previous.map(|p| p.to_vec()),
            "sessionDigest": self.session.as_deref().map(session_digest),
        })
    }

    /// Restore the model, provider and running totals saved by `codex_state`.
    pub fn restore_codex(&mut self, state: &Value) {
        let text = |key: &str, limit: usize| {
            state
                .get(key)
                .and_then(Value::as_str)
                .map(|t| util::prefix_chars(t, limit))
        };
        self.model = text("model", 200);
        self.provider = text("provider", 64);
        self.previous = state.get("previous").and_then(Value::as_array).and_then(|values| {
            let numbers: Vec<f64> = values.iter().filter_map(Value::as_f64).collect();
            (numbers.len() == 4 && numbers.iter().all(|n| n.is_finite() && *n >= 0.0))
                .then(|| [numbers[0], numbers[1], numbers[2], numbers[3]])
        });
    }
}

/// The session as a resume check: 16 hex of sha256("aac-resume-v1:" + id), never the id.
pub fn session_digest(session: &str) -> String {
    util::sha256_hex(format!("aac-resume-v1:{session}").as_bytes())[..16].to_string()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Outcome {
    Ok,
    Cut,
    Unreadable,
}

pub type Parse = fn(&str, &mut Collector, &str, &str, &mut LineBox);
pub type Flush = fn(&mut Collector, &str, &str, &mut LineBox);

/// Python's `_newest_first`: newest mtime first, ties by path; unstattable files dropped.
pub fn newest_first(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut stamped: Vec<(i128, String, PathBuf)> = paths
        .iter()
        .filter_map(|p| {
            std::fs::metadata(p)
                .ok()
                .map(|m| (util::mtime_ns(&m), path_text(p), p.clone()))
        })
        .collect();
    stamped.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    stamped.into_iter().map(|(_, _, p)| p).collect()
}

/// Python's `_note_oversized`: the same 512-byte envelope rules as the server.
pub fn note_oversized(collector: &mut Collector, kind: &str, filekey: &str, chunk: &[u8]) {
    let prefix = String::from_utf8_lossy(&chunk[..chunk.len().min(512)]);
    let prefix = prefix.trim_start_matches('\u{feff}');
    let record_type = envelope_type(prefix);
    let carries = match kind {
        "muse" => {
            let compact: String = prefix.chars().filter(|c| !c.is_whitespace()).collect();
            compact.contains("\"children\":")
        }
        "codex" => record_type.as_deref() != Some("response_item"),
        "claude" => record_type.is_none() || record_type.as_deref() == Some("assistant"),
        _ => true,
    };
    if carries {
        collector.usage_partial.insert(kind.to_string());
        collector.set_pending(kind, filekey, |print| print.usage_skipped = Some(true));
    }
}

/// `^\s*\{\s*("timestamp"\s*:\s*"[^"\\]*"\s*,\s*)?("ordinal"\s*:\s*\d+\s*,\s*)?"type"\s*:\s*"([^"\\]+)"`
fn envelope_type(text: &str) -> Option<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut i = 0usize;
    let skip_ws = |i: &mut usize| {
        while *i < chars.len() && chars[*i].is_whitespace() {
            *i += 1;
        }
    };
    let literal = |i: &mut usize, word: &str| -> bool {
        let w: Vec<char> = word.chars().collect();
        if chars.len() >= *i + w.len() && chars[*i..*i + w.len()] == w[..] {
            *i += w.len();
            true
        } else {
            false
        }
    };
    // "key" \s* : \s*
    let key = |i: &mut usize, name: &str| -> bool {
        let mut j = *i;
        if !literal(&mut j, &format!("\"{name}\"")) {
            return false;
        }
        skip_ws(&mut j);
        if !literal(&mut j, ":") {
            return false;
        }
        skip_ws(&mut j);
        *i = j;
        true
    };
    // "[^"\\]*" ; returns the content when `nonempty` allows it
    let string = |i: &mut usize, nonempty: bool| -> Option<String> {
        let mut j = *i;
        if !literal(&mut j, "\"") {
            return None;
        }
        let begin = j;
        while j < chars.len() && chars[j] != '"' && chars[j] != '\\' {
            j += 1;
        }
        if j >= chars.len() || chars[j] != '"' || (nonempty && j == begin) {
            return None;
        }
        let value: String = chars[begin..j].iter().collect();
        *i = j + 1;
        Some(value)
    };
    skip_ws(&mut i);
    if !literal(&mut i, "{") {
        return None;
    }
    skip_ws(&mut i);
    let mut j = i;
    if key(&mut j, "timestamp") && string(&mut j, false).is_some() {
        skip_ws(&mut j);
        if literal(&mut j, ",") {
            skip_ws(&mut j);
            i = j;
        }
    }
    let mut j = i;
    if key(&mut j, "ordinal") {
        let begin = j;
        while j < chars.len() && chars[j].is_ascii_digit() {
            j += 1;
        }
        if j > begin {
            skip_ws(&mut j);
            if literal(&mut j, ",") {
                skip_ws(&mut j);
                i = j;
            }
        }
    }
    if !key(&mut i, "type") {
        return None;
    }
    string(&mut i, true)
}

/// Python's `_read_open_file`: one file whole; "ok", "cut" (deadline or row cap) or "unreadable".
pub fn read_lines<R: Read>(
    collector: &mut Collector,
    kind: &str,
    reader: R,
    filekey: &str,
    parse: Parse,
    flush: Option<Flush>,
    line_box: &mut LineBox,
) -> Outcome {
    let mut reader = BufReader::with_capacity(1 << 16, reader);
    let mut offset: u64 = line_box.through;
    let mut chunk = Vec::with_capacity(4096);
    loop {
        if collector.expired() {
            collector.truncated = true;
            return Outcome::Cut;
        }
        chunk.clear();
        let read = match (&mut reader)
            .take((MAX_LINE_BYTES + 2) as u64)
            .read_until(b'\n', &mut chunk)
        {
            Ok(read) => read,
            Err(_) => return Outcome::Unreadable,
        };
        if read == 0 {
            break;
        }
        let start = offset;
        offset += read as u64;
        let body = if chunk.ends_with(b"\n") {
            &chunk[..chunk.len() - 1]
        } else {
            &chunk[..]
        };
        if body.len() > MAX_LINE_BYTES {
            note_oversized(collector, kind, filekey, &chunk);
            // Skip the oversized line without decoding content.
            while !chunk.ends_with(b"\n") {
                if collector.expired() {
                    collector.truncated = true;
                    return Outcome::Cut;
                }
                chunk.clear();
                match (&mut reader)
                    .take((MAX_LINE_BYTES + 2) as u64)
                    .read_until(b'\n', &mut chunk)
                {
                    Ok(0) => break,
                    Ok(read) => offset += read as u64,
                    Err(_) => return Outcome::Unreadable,
                }
            }
            line_box.through = offset;
            continue;
        }
        let Ok(line) = std::str::from_utf8(&chunk) else {
            line_box.through = offset;
            continue;
        };
        line_box.offset = start;
        parse(line.trim_start_matches('\u{feff}'), collector, kind, filekey, line_box);
        line_box.through = offset;
        collector.count_line();
        if collector.row_cap {
            return Outcome::Cut;
        }
    }
    if let Some(flush) = flush {
        flush(collector, kind, filekey, line_box);
    }
    Outcome::Ok
}

/// Python's `_read_file`: a compressed Codex rollout is decoded in-process (no external tool needed).
pub fn read_file(
    collector: &mut Collector,
    kind: &str,
    path: &Path,
    filekey: &str,
    parse: Parse,
    flush: Option<Flush>,
) -> Outcome {
    read_file_with(collector, kind, path, filekey, parse, flush, LineBox::default())
}

/// `read_file` with a prepared per-file state (OMP and Muse carry the file's session).
pub fn read_file_with(
    collector: &mut Collector,
    kind: &str,
    path: &Path,
    filekey: &str,
    parse: Parse,
    flush: Option<Flush>,
    mut line_box: LineBox,
) -> Outcome {
    read_into(collector, kind, path, filekey, parse, flush, &mut line_box)
}

/// Read one file from `line_box.through` (0, or where an earlier call stopped), leaving in
/// `line_box.through` the offset after the last complete line. A compressed log is decoded from
/// its start and the bytes before `through` discarded without parsing (a single zstd frame cannot
/// be entered in the middle); a plain file is entered by seeking. A resumed Codex read first takes
/// the session and cwd from the log's own first line and checks them against the saved digest; a
/// mismatch or a place past the end reads the file from 0 instead (`restarted`). DESIGN 16.1.
pub fn read_into(
    collector: &mut Collector,
    kind: &str,
    path: &Path,
    filekey: &str,
    parse: Parse,
    flush: Option<Flush>,
    line_box: &mut LineBox,
) -> Outcome {
    let message_from = collector
        .pending
        .get(kind)
        .and_then(|m| m.get(filekey))
        .and_then(|p| p.msg_through)
        .unwrap_or(0);
    line_box.message_from = message_from;
    let Ok(file) = File::open(path) else {
        return Outcome::Unreadable;
    };
    let compressed = kind == "codex" && path_text(path).ends_with(".jsonl.zst");
    let skip = line_box.through;
    if skip == 0 {
        if compressed {
            let Ok(decoder) = zstd::stream::read::Decoder::new(file) else {
                note_unreadable_log(collector, path);
                return Outcome::Unreadable;
            };
            let outcome = read_lines(collector, kind, decoder, filekey, parse, flush, line_box);
            if outcome == Outcome::Unreadable {
                note_unreadable_log(collector, path);
            }
            return outcome;
        }
        return read_lines(collector, kind, file, filekey, parse, flush, line_box);
    }
    let expected = collector
        .resume
        .get(&(kind.to_string(), filekey.to_string()))
        .and_then(|(_, state)| state.get("sessionDigest").and_then(Value::as_str).map(str::to_string));
    let restart = |collector: &mut Collector, line_box: &mut LineBox| {
        *line_box = LineBox {
            message_from,
            restarted: true,
            ..LineBox::default()
        };
        read_into(collector, kind, path, filekey, parse, flush, line_box)
    };
    if compressed {
        let Ok(decoder) = zstd::stream::read::Decoder::new(file) else {
            note_unreadable_log(collector, path);
            return Outcome::Unreadable;
        };
        let mut reader = BufReader::with_capacity(1 << 16, decoder);
        let Some(head) = head_line(collector, kind, filekey, &mut reader, line_box, skip) else {
            return restart(collector, line_box);
        };
        if expected.as_deref() != line_box.session.as_deref().map(session_digest).as_deref() {
            return restart(collector, line_box);
        }
        match discard(collector, &mut reader, skip - head) {
            Discarded::Done => {}
            Discarded::Cut => {
                collector.truncated = true;
                line_box.through = skip;
                return Outcome::Cut;
            }
            Discarded::Short => return restart(collector, line_box),
            Discarded::Failed => {
                note_unreadable_log(collector, path);
                return Outcome::Unreadable;
            }
        }
        line_box.through = skip;
        let outcome = read_lines(collector, kind, reader, filekey, parse, flush, line_box);
        if outcome == Outcome::Unreadable {
            note_unreadable_log(collector, path);
        }
        return outcome;
    }
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut reader = BufReader::with_capacity(1 << 16, file);
    let Some(_) = head_line(collector, kind, filekey, &mut reader, line_box, skip) else {
        return restart(collector, line_box);
    };
    if skip > size
        || expected.as_deref() != line_box.session.as_deref().map(session_digest).as_deref()
        || reader.seek(SeekFrom::Start(skip)).is_err()
    {
        return restart(collector, line_box);
    }
    line_box.through = skip;
    read_lines(collector, kind, reader, filekey, parse, flush, line_box)
}

/// A resumed read's first line: the session and cwd come from the log's own `session_meta`, never
/// from saved state. Returns its length, or None when it is not a whole line before `skip`.
fn head_line<R: BufRead>(
    collector: &mut Collector,
    kind: &str,
    filekey: &str,
    reader: &mut R,
    line_box: &mut LineBox,
    skip: u64,
) -> Option<u64> {
    let mut line = Vec::new();
    let read = reader
        .by_ref()
        .take(MAX_LINE_BYTES as u64 + 2)
        .read_until(b'\n', &mut line)
        .ok()? as u64;
    if read == 0 || read > skip || !line.ends_with(b"\n") {
        return None;
    }
    let text = std::str::from_utf8(&line).ok()?;
    if text.contains("\"session_meta\"") {
        let (model, provider, previous) = (line_box.model.take(), line_box.provider.take(), line_box.previous);
        codex::parse_line(text.trim_start_matches('\u{feff}'), collector, kind, filekey, line_box);
        // The saved model and totals stand; the head line only names the session and its folder.
        line_box.model = model.or(line_box.model.take());
        line_box.provider = provider.or(line_box.provider.take());
        line_box.previous = previous;
    }
    Some(read)
}

fn note_unreadable_log(collector: &mut Collector, path: &Path) {
    collector.codex_compressed_unread.insert(path_text(path));
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    collector.unreadable_logs.push((path.to_path_buf(), size));
}

enum Discarded {
    Done,
    Cut,
    Short,
    Failed,
}

/// Decode and drop `count` bytes, 8 MiB at a time, stopping at the deadline.
fn discard<R: Read>(collector: &Collector, reader: &mut R, count: u64) -> Discarded {
    let mut left = count;
    while left > 0 {
        if collector.expired() {
            return Discarded::Cut;
        }
        let step = left.min(8 << 20);
        match std::io::copy(&mut reader.by_ref().take(step), &mut std::io::sink()) {
            Ok(0) => return Discarded::Short,
            Ok(read) => left -= read,
            Err(_) => return Discarded::Failed,
        }
    }
    Discarded::Done
}

/// Where copies of a log's records can be: a log of the same name, or (Codex) the same conversation id.
pub fn copy_key(kind: &str, path: &Path) -> String {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if kind == "codex"
        && let Some(id) = codex::conversation_id(&name)
    {
        return id;
    }
    if kind == "codex" && name.ends_with(".zst") {
        return name[..name.len() - 4].to_string();
    }
    name
}

/// Python's `_deps_print`: a digest of each copy reference's key and print. None when the deadline
/// passes before every copy is printed (each print opens the file and reads its head and tail).
pub fn deps_print(kind: &str, copies: &[PathBuf], deadline: Instant) -> Option<String> {
    let mut sorted: Vec<&PathBuf> = copies.iter().collect();
    sorted.sort_by_key(|p| path_text(p));
    let mut lines: Vec<String> = Vec::with_capacity(sorted.len());
    for path in sorted {
        if Instant::now() >= deadline {
            return None;
        }
        let print = util::fingerprint(path);
        lines.push(format!(
            "{}:{}:{}:{}:{}",
            util::filekey(kind, path),
            print.as_ref().map_or("None".to_string(), |p| p.size.to_string()),
            print.as_ref().map_or("None".to_string(), |p| p.mtime_ms.to_string()),
            print.as_ref().map_or("None".to_string(), |p| p.head.clone()),
            print.as_ref().map_or("None".to_string(), |p| p.tail.clone()),
        ));
    }
    Some(util::sha256_hex(lines.join("\n").as_bytes()))
}

/// Python's `_t3_files`: non-default files to read, each mapped to the default and earlier non-default
/// files that may hold copies of its records. A file that is a default file (a link or hard link to
/// it) or an earlier non-default file is not listed.
pub fn other_files(
    collector: &mut Collector,
    kind: &str,
    dirs: &[PathBuf],
    default_paths: &[PathBuf],
    other_dirs: &[PathBuf],
    accept: &dyn Fn(&str) -> bool,
) -> Vec<(PathBuf, Vec<PathBuf>)> {
    let exclude: Vec<PathBuf> = dirs
        .iter()
        .filter(|d| fsx::is_dir(d))
        .map(|d| util::realpath(d))
        .collect();
    let mut identities: HashSet<(u64, u64)> = default_paths.iter().filter_map(|p| fsx::identity(p)).collect();
    let mut found = Vec::new();
    for root in other_dirs {
        let start = found.len();
        for (path, identity) in fsx::walk_t3_files(root, accept, collector, &exclude) {
            if !identities.insert(identity) {
                continue;
            }
            found.push(path);
            if default_paths.len() + found.len() > MAX_FILES {
                collector.truncated = true;
                break;
            }
        }
        let added: Vec<PathBuf> = found[start..].to_vec();
        collector.count_root(kind, &util::realpath(root), &added);
    }
    let mut by_key: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for path in default_paths {
        by_key.entry(copy_key(kind, path)).or_default().push(path.clone());
    }
    found.sort_by_key(|p| path_text(p));
    let mut copies = Vec::new();
    for path in found {
        let key = copy_key(kind, &path);
        let refs = by_key.get(&key).cloned().unwrap_or_default();
        by_key.entry(key).or_default().push(path.clone());
        copies.push((path, refs));
    }
    copies
}

/// The record keys the copy references hold, or None when the deadline cut the read.
pub fn record_keys(
    collector: &mut Collector,
    kind: &str,
    copies: &[PathBuf],
    parse: Parse,
    flush: Option<Flush>,
) -> Option<HashSet<String>> {
    collector.capture = Some(HashSet::new());
    for path in copies {
        if read_file(collector, kind, path, "", parse, flush) == Outcome::Cut {
            collector.capture = None;
            return None;
        }
    }
    collector.capture.take()
}

/// Python's `_scan_kind_files` (Codex and Claude): default dirs, then non-default dirs through the
/// copy rule, newest first. True when the scan stopped early.
pub fn scan_kind_files(
    collector: &mut Collector,
    kind: &str,
    dirs: &[PathBuf],
    accept: &dyn Fn(&str) -> bool,
    parse: Parse,
    flush: Option<Flush>,
    other_dirs: &[PathBuf],
) -> bool {
    let mut paths: Vec<PathBuf> = Vec::new();
    for directory in dirs {
        if !fsx::is_dir(directory) || fsx::is_link(directory) {
            continue;
        }
        let start = paths.len();
        let mut capped = false;
        for path in fsx::walk_files(directory, accept, collector, None) {
            paths.push(path);
            if paths.len() > MAX_FILES {
                collector.truncated = true;
                capped = true;
                break;
            }
        }
        let added: Vec<PathBuf> = paths[start..].to_vec();
        collector.count_root(kind, &util::realpath(directory), &added);
        if collector.expired() || capped {
            break;
        }
    }
    let mut refs: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    if !other_dirs.is_empty() {
        for (path, copies) in other_files(collector, kind, dirs, &paths.clone(), other_dirs, accept) {
            paths.push(path.clone());
            refs.insert(path, copies);
        }
    }
    if kind == "codex" {
        let recent: Vec<PathBuf> = paths
            .iter()
            .filter(|p| {
                std::fs::metadata(p)
                    .map(|m| util::mtime_seconds(&m) * 1000.0 >= (collector.min_date_ms - 86_400_000) as f64)
                    .unwrap_or(false)
            })
            .cloned()
            .collect();
        let plain: HashSet<String> = recent
            .iter()
            .filter(|p| !path_text(p).ends_with(".zst"))
            .map(|p| copy_key(kind, p))
            .collect();
        let mut compressed: HashSet<String> = HashSet::new();
        paths = Vec::new();
        for path in recent {
            let key = copy_key(kind, &path);
            if path_text(&path).ends_with(".zst") {
                if plain.contains(&key) || compressed.contains(&key) {
                    continue;
                }
                compressed.insert(key);
            }
            paths.push(path);
        }
        let allowed: HashSet<PathBuf> = paths.iter().cloned().collect();
        for copies in refs.values_mut() {
            copies.retain(|c| allowed.contains(c));
        }
    }
    if kind == "claude" {
        return claude::scan_groups(collector, &paths, parse, flush);
    }
    // The server's exact-copy rule: same size, head and tail. Unreadable files go to the read path.
    let mut seen: HashSet<Option<(u64, String, String)>> = HashSet::new();
    let mut distinct = Vec::new();
    for path in &paths {
        // Each print opens the file: on a slow disk with thousands of files this pass alone can
        // outlast the budget, and ssh would then time the whole computer out.
        if collector.expired() {
            collector.truncated = true;
            return true;
        }
        let stamp = util::fingerprint(path).map(|p| (p.size, p.head, p.tail));
        if stamp.is_some() && seen.contains(&stamp) {
            continue;
        }
        seen.insert(stamp);
        distinct.push(path.clone());
    }
    for path in newest_first(&distinct) {
        if collector.expired() {
            collector.truncated = true;
            return true;
        }
        let copies = refs.get(&path).cloned().unwrap_or_default();
        let deps = if copies.is_empty() {
            None
        } else {
            let Some(print) = deps_print(kind, &copies, collector.deadline()) else {
                collector.truncated = true;
                return true;
            };
            Some(print)
        };
        let Some(filekey) = collector.note_file(kind, &path, deps.as_deref()) else {
            let staged = util::filekey(kind, &path);
            if collector.is_pending(kind, &staged) {
                collector.confirm_file(kind, &staged);
            }
            continue;
        };
        if !copies.is_empty() {
            let Some(known) = record_keys(collector, kind, &copies, parse, flush) else {
                return true;
            };
            collector.skip = Some(known);
        }
        // A resumable kind reads on from where an earlier call stopped (DESIGN 16.1).
        let (from, state) = if collector::resumable(kind) {
            collector.resume_point(kind, &filekey)
        } else {
            (0, Value::Null)
        };
        let mut line_box = LineBox {
            through: from,
            ..LineBox::default()
        };
        if from > 0 {
            line_box.restore_codex(&state);
        }
        let outcome = read_into(collector, kind, &path, &filekey, parse, flush, &mut line_box);
        // A saved place that could not be trusted was read from 0: its rows replace, not add.
        let from = if line_box.restarted { 0 } else { from };
        collector.skip = None;
        match outcome {
            Outcome::Cut => {
                if collector::resumable(kind) && !collector.row_cap {
                    collector.cut_file(kind, &filekey, from, line_box.through, line_box.codex_state(), &path);
                }
                return true;
            }
            Outcome::Ok => {
                collector.sent_through(kind, &filekey, &path);
                collector.confirm_file(kind, &filekey);
                collector.finish_resumed(kind, &filekey, from);
            }
            Outcome::Unreadable => {
                collector.read_failure(kind, filekey.clone());
                // A failed decoder may have emitted a prefix. None of it is fresh.
                collector.drop_file_rows(&HashSet::from([filekey]));
            }
        }
    }
    false
}
