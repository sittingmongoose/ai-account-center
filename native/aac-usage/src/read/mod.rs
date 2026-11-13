//! Reading logs: whole files, line by line, with the Python helper's caps and copy rules.

pub mod antigravity;
pub mod claude;
pub mod codex;
pub mod muse;
pub mod omp;
pub mod zcode;

use crate::collector::{Collector, MAX_FILES, MAX_LINE_BYTES};
use crate::fsx;
use crate::util::{self, path_text};
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};

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
    mut line_box: LineBox,
) -> Outcome {
    let mut reader = BufReader::with_capacity(1 << 16, reader);
    let mut offset: u64 = 0;
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
            continue;
        }
        let Ok(line) = std::str::from_utf8(&chunk) else {
            continue;
        };
        line_box.offset = start;
        parse(
            line.trim_start_matches('\u{feff}'),
            collector,
            kind,
            filekey,
            &mut line_box,
        );
        if collector.row_cap {
            return Outcome::Cut;
        }
    }
    if let Some(flush) = flush {
        flush(collector, kind, filekey, &mut line_box);
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
    let message_from = collector
        .pending
        .get(kind)
        .and_then(|m| m.get(filekey))
        .and_then(|p| p.msg_through)
        .unwrap_or(0);
    let Ok(file) = File::open(path) else {
        return Outcome::Unreadable;
    };
    if kind == "codex" && path_text(path).ends_with(".jsonl.zst") {
        let Ok(decoder) = zstd::stream::read::Decoder::new(file) else {
            collector.codex_compressed_unread.insert(path_text(path));
            return Outcome::Unreadable;
        };
        line_box.message_from = message_from;
        let outcome = read_lines(collector, kind, decoder, filekey, parse, flush, line_box);
        if outcome == Outcome::Unreadable {
            collector.codex_compressed_unread.insert(path_text(path));
        }
        return outcome;
    }
    line_box.message_from = message_from;
    read_lines(collector, kind, file, filekey, parse, flush, line_box)
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

/// Python's `_deps_print`: a digest of each copy reference's key and print.
pub fn deps_print(kind: &str, copies: &[PathBuf]) -> String {
    let mut sorted: Vec<&PathBuf> = copies.iter().collect();
    sorted.sort_by_key(|p| path_text(p));
    let lines: Vec<String> = sorted
        .iter()
        .map(|path| {
            let print = util::fingerprint(path);
            format!(
                "{}:{}:{}:{}:{}",
                util::filekey(kind, path),
                print.as_ref().map_or("None".to_string(), |p| p.size.to_string()),
                print.as_ref().map_or("None".to_string(), |p| p.mtime_ms.to_string()),
                print.as_ref().map_or("None".to_string(), |p| p.head.clone()),
                print.as_ref().map_or("None".to_string(), |p| p.tail.clone()),
            )
        })
        .collect();
    util::sha256_hex(lines.join("\n").as_bytes())
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
            Some(deps_print(kind, &copies))
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
        let outcome = read_file(collector, kind, &path, &filekey, parse, flush);
        collector.skip = None;
        match outcome {
            Outcome::Cut => return true,
            Outcome::Ok => {
                collector.sent_through(kind, &filekey, &path);
                collector.confirm_file(kind, &filekey);
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
