//! Bounded content probes for the PM-Experiments discovery walk.
use super::experiment_roots::Kind;
use aac_files::read::{self, Ancestors, ReadPolicy};
use regex::Regex;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

const SNIFF_BYTES: usize = 64 * 1024;
const SNIFF_LINES: usize = 40;
static SESSION_META: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""type"\s*:\s*"session_meta""#).expect("fixed regex"));
static CODEX_IDENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#""(originator|cli_version)"\s*:\s*""#).expect("fixed regex"));

fn open(base: &Path, path: &Path) -> Option<File> {
    let policy = ReadPolicy::capped(u64::MAX).ancestors(Ancestors::NoLinkBelow(base));
    read::open(path, &policy).ok().map(|(file, _)| file)
}

fn text(value: &Value) -> bool {
    value.as_str().is_some_and(|text| !text.is_empty())
}

fn codex_meta(line: &str) -> bool {
    let bytes = line.as_bytes();
    let end = bytes.len().min(4096);
    SESSION_META.is_match(&String::from_utf8_lossy(&bytes[..end]))
}

fn classify(text_bytes: &[u8], name: &str) -> Option<Kind> {
    let text_bytes = String::from_utf8_lossy(text_bytes);
    let mut parsed = 0;
    for line in text_bytes.lines().map(str::trim) {
        if parsed == SNIFF_LINES {
            break;
        }
        if !line.starts_with('{') || !line.ends_with('}') {
            continue;
        }
        if name.starts_with("rollout-") && codex_meta(line) {
            return CODEX_IDENTITY.is_match(line).then_some(Kind::Codex);
        }
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        parsed += 1;
        let get = |name: &str| record.get(name).unwrap_or(&Value::Null);
        let kind = get("type").as_str();
        if matches!(kind, Some("user" | "assistant" | "system"))
            && ["sessionId", "version", "cwd"]
                .iter()
                .all(|key| text(get(key)))
        {
            return Some(Kind::Claude);
        }
        if kind == Some("system") && get("subtype") == "init" && text(get("claude_code_version")) {
            return text(get("session_id")).then_some(Kind::Claude);
        }
        if kind == Some("assistant")
            && ["session_id", "uuid", "timestamp"]
                .iter()
                .all(|key| text(get(key)))
            && get("message")["usage"].is_object()
        {
            return Some(Kind::Claude);
        }
        if text(get("record_type"))
            && record.contains_key("recorded_at")
            && get("payload").is_object()
        {
            return (name == "session.jsonl").then_some(Kind::Muse);
        }
    }
    None
}

fn read_at(file: &mut File, at: u64, size: usize) -> std::io::Result<Vec<u8>> {
    file.seek(SeekFrom::Start(at))?;
    let mut bytes = Vec::with_capacity(size);
    file.take(size as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn usage_log(
    base: &Path,
    path: &Path,
    name: &str,
    compressed: bool,
    deadline: Instant,
) -> Option<Kind> {
    let mut file = open(base, path)?;
    if compressed {
        let mut decoder = zstd::stream::read::Decoder::new(file).ok()?;
        decoder.window_log_max(27).ok()?;
        let mut decoder = decoder.take(SNIFF_BYTES as u64);
        let mut bytes = Vec::with_capacity(SNIFF_BYTES);
        let mut chunk = [0; 8192];
        while Instant::now() < deadline {
            let size = decoder.read(&mut chunk).ok()?;
            if size == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..size]);
        }
        if Instant::now() >= deadline {
            return None;
        }
        let head = String::from_utf8_lossy(&bytes);
        let first = head.trim_start().lines().next()?;
        if first.starts_with('{') && codex_meta(first) && CODEX_IDENTITY.is_match(first) {
            return Some(Kind::Codex);
        }
        return classify(&bytes, name);
    }
    let size = file.metadata().ok()?.len();
    let head = read_at(&mut file, 0, SNIFF_BYTES).ok()?;
    let matched = classify(&head, name);
    if matched.is_some() || size <= SNIFF_BYTES as u64 {
        return matched;
    }
    let tail = read_at(&mut file, size - SNIFF_BYTES as u64, SNIFF_BYTES).ok()?;
    let start = tail
        .iter()
        .position(|&byte| byte == b'\n')
        .map_or(0, |pos| pos + 1);
    classify(&tail[start..], name)
}

pub(super) fn root_for(kind: Kind, path: &Path) -> Option<&Path> {
    if kind == Kind::Zcode {
        return Some(path);
    }
    let parent = path.parent()?;
    if kind == Kind::Claude {
        return Some(parent);
    }
    let directory = if kind == Kind::Muse {
        parent.parent()?
    } else {
        parent
    };
    let mut date = directory;
    for digits in [2, 2, 4] {
        let Some(name) = date.file_name().and_then(|name| name.to_str()) else {
            return Some(directory);
        };
        if name.len() != digits || !name.bytes().all(|b| b.is_ascii_digit()) {
            return Some(directory);
        }
        date = date.parent()?;
    }
    Some(date)
}

pub(super) fn generator_manifest(base: &Path, dir: &Path) -> bool {
    let policy = ReadPolicy::capped(64 * 1024).ancestors(Ancestors::NoLinkBelow(base));
    let read::Read::Bytes(bytes, _) = read::read(&dir.join("MANIFEST.json"), &policy) else {
        return false;
    };
    serde_json::from_slice::<Value>(&bytes).is_ok_and(|value| value["trees"].is_object())
}

fn word_in(sql: &str, word: &str) -> bool {
    sql.split(|ch: char| !ch.is_ascii_alphanumeric() && ch != '_')
        .any(|part| part == word)
}

fn varint(bytes: &[u8], offset: &mut usize) -> Option<usize> {
    let mut value = 0usize;
    for index in 0..9 {
        let byte = *bytes.get(*offset)?;
        *offset += 1;
        value = if index == 8 {
            value.checked_mul(256)?.checked_add(usize::from(byte))?
        } else {
            value
                .checked_mul(128)?
                .checked_add(usize::from(byte & 0x7f))?
        };
        if byte < 0x80 || index == 8 {
            return Some(value);
        }
    }
    None
}

fn schema_columns(page: &[u8], start: usize) -> Option<Vec<&str>> {
    let mut cursor = start;
    varint(page, &mut cursor)?; // payload length
    varint(page, &mut cursor)?; // rowid
    let header_start = cursor;
    let header_end = header_start.checked_add(varint(page, &mut cursor)?)?;
    let mut value_at = header_end;
    let mut columns = Vec::new();
    while cursor < header_end && columns.len() < 5 {
        let serial = varint(page, &mut cursor)?;
        let size = if serial >= 12 {
            (serial - 12) / 2
        } else {
            [0, 1, 2, 3, 4, 6, 8, 8, 0, 0, 0, 0][serial]
        };
        let end = value_at.checked_add(size)?;
        let column = if serial >= 13 && serial % 2 == 1 {
            std::str::from_utf8(page.get(value_at..end.min(page.len()))?).ok()?
        } else {
            ""
        };
        columns.push(column);
        value_at = end;
    }
    Some(columns)
}

fn be16(bytes: &[u8], offset: usize) -> Option<usize> {
    Some(usize::from(u16::from_be_bytes(
        bytes.get(offset..offset + 2)?.try_into().ok()?,
    )))
}

fn child_page(bytes: &[u8], offset: usize) -> Option<u32> {
    let number = u32::from_be_bytes(bytes.get(offset..offset + 4)?.try_into().ok()?);
    (number > 0).then_some(number)
}

/// Match the zcode schema from at most 64 SQLite pages. Discovery never opens the DB with SQLite.
pub(super) fn is_zcode(base: &Path, path: &Path) -> Option<bool> {
    let mut file = open(base, path)?;
    let header = read_at(&mut file, 0, 100).ok()?;
    if header.len() < 100 || !header.starts_with(b"SQLite format 3\0") {
        return Some(false);
    }
    let page_size = match be16(&header, 16)? {
        1 => 65536,
        size => size,
    };
    if page_size < 512 || !page_size.is_power_of_two() {
        return Some(false);
    }
    let mut pending = vec![(1u32, 0)];
    for _ in 0..64 {
        let Some((number, depth)) = pending.pop() else {
            break;
        };
        let start = if number == 1 { 100 } else { 0 };
        let page = read_at(
            &mut file,
            u64::from(number - 1) * page_size as u64,
            page_size,
        )
        .ok()?;
        if page.len() != page_size {
            continue;
        }
        let node = page[start];
        let cells = be16(&page, start + 3)?.min(1000);
        let internal = node == 5;
        let pointers = start + if internal { 12 } else { 8 };
        if internal
            && depth < 4
            && let Some(right) = child_page(&page, start + 8)
        {
            pending.push((right, depth + 1));
        }
        for i in (0..cells).rev() {
            let Some(at) = be16(&page, pointers + 2 * i).filter(|at| *at < page_size) else {
                continue;
            };
            if internal
                && depth < 4
                && let Some(child) = child_page(&page, at)
            {
                pending.push((child, depth + 1));
            }
            if node == 13
                && let Some(columns) = schema_columns(&page, at)
                && let ["table", "model_usage", _, _, sql] = columns.as_slice()
                && [
                    "model_id",
                    "provider_id",
                    "started_at",
                    "input_tokens",
                    "output_tokens",
                    "cache_read_input_tokens",
                    "cache_creation_input_tokens",
                ]
                .iter()
                .all(|col| word_in(sql, col))
            {
                return Some(true);
            }
        }
    }
    Some(false)
}

pub(super) fn zcode_digest(base: &Path, path: &Path) -> Option<String> {
    let mut hash = Sha256::new();
    let mut wal = path.as_os_str().to_owned();
    wal.push("-wal");
    let wal = PathBuf::from(wal);
    for (part, name) in [(path, "db"), (wal.as_path(), "wal")] {
        let Some(mut file) = open(base, part) else {
            continue;
        };
        let size = file.metadata().ok()?.len();
        if size == 0 {
            continue;
        }
        hash.update(format!("{name}:{size}:"));
        hash.update(read_at(&mut file, 0, SNIFF_BYTES).ok()?);
        hash.update(
            read_at(
                &mut file,
                size.saturating_sub(SNIFF_BYTES as u64),
                SNIFF_BYTES,
            )
            .ok()?,
        );
        for index in 1..=16 {
            let at = size / 17 * index + size % 17 * index / 17;
            hash.update(read_at(&mut file, at, 4096).ok()?);
        }
    }
    Some(
        hash.finalize()
            .iter()
            .take(16)
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}
