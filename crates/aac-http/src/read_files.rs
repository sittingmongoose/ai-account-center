//! Bounded read-only Status files. The source handle is confined before any bytes are read.
use aac_files::read::{ReadPolicy, opened_path};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Input {
    root: PathBuf,
    writer: Option<String>,
    path: String,
    start: u64,
    length: u64,
    head: bool,
}

fn plain(path: &str) -> bool {
    !path.is_empty()
        && path.encode_utf16().count() <= 2048
        && !path
            .bytes()
            .any(|b| b < 32 || b == 127 || b"\\:%".contains(&b))
        && path
            .split('/')
            .all(|p| !p.is_empty() && !p.starts_with('.'))
}

fn confined(root: &Path, base: &str, relative: &str) -> Option<(File, u64)> {
    if !root.is_absolute() {
        return None;
    }
    let root = fs::canonicalize(root).ok()?;
    let base = root.join(base);
    if fs::canonicalize(&base).ok()? != base {
        return None;
    }
    let target = fs::canonicalize(root.join(relative)).ok()?;
    if !target.starts_with(&base)
        || target == base
        || !target
            .strip_prefix(&base)
            .ok()?
            .iter()
            .all(|part| part.to_str().is_some_and(plain))
    {
        return None;
    }
    let (file, size) = open_resolved(&target)?;
    // Like the share route, require the opened descriptor's exact path. Unsupported hosts
    // delegate to TS rather than substituting a path-only race check.
    if opened_path(&file)? != target {
        return None;
    }
    Some((file, size))
}

/// The resolved target through the one hardened open: a regular file, never a link, opened
/// without blocking, the descriptor proved to be the checked inode. The callers apply their own
/// size rules (a media type's cap is known only after its header is read).
fn open_resolved(target: &Path) -> Option<(File, u64)> {
    let (file, meta) = aac_files::read::open(target, &ReadPolicy::capped(u64::MAX)).ok()?;
    Some((file, meta.len()))
}

/// TS `isWriterName`: the machine is one of computers.json's `machineId`s, never a copied list
/// (big review Wonder L6).
fn writer_ok(writer: &str) -> bool {
    crate::account_reads::machine_ids().is_some_and(|machines| writer_in(writer, &machines))
}

fn writer_in(writer: &str, machines: &[String]) -> bool {
    let Some((machine, name)) = writer.split_once('.') else {
        return false;
    };
    machines.iter().any(|m| m == machine)
        && writer.len() <= 64
        && !writer.contains("--")
        && !name.is_empty()
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-".contains(&b))
}

fn media_path(path: &str) -> bool {
    path.len() <= 300
        && path.split('/').count() <= 6
        && path.split('/').all(|p| {
            !p.is_empty()
                && p.len() <= 100
                && (p.as_bytes()[0].is_ascii_alphanumeric() || b"_-".contains(&p.as_bytes()[0]))
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
}

fn legacy(route: &str, bytes: &[u8]) -> Option<Value> {
    let input: Input = crate::json_object::from_slice(
        bytes,
        &["root", "writer", "path", "start", "length", "head"],
    )
    .ok()?;
    if !plain(&input.path) {
        return None;
    }
    let (mut file, size) = if route == "/api/status/deliverable" {
        if input.writer.is_some() {
            return None;
        }
        let parts: Vec<_> = input.path.split('/').collect();
        let base = if parts.first() == Some(&"share") && parts.len() > 1 {
            "share".to_owned()
        } else if parts.len() > 5
            && parts[..3] == ["agent-reports", "v1", "writers"]
            && parts[4] == "media"
            && parts[3].len() <= 128
            && parts[3].as_bytes()[0].is_ascii_alphanumeric()
            && parts[3]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            parts[..5].join("/")
        } else {
            return None;
        };
        let (file, size) = confined(&input.root, &base, &input.path)?;
        if size > 100 * 1024 * 1024 {
            return None;
        }
        (file, size)
    } else {
        let writer = input.writer.as_deref()?;
        if !writer_ok(writer) || !media_path(&input.path) {
            return None;
        }
        let base = format!("v1/writers/{writer}");
        let relative = format!("{base}/{}", input.path);
        let (file, size) = confined(&input.root, &base, &relative)?;
        if size > 50 * 1024 * 1024 {
            return None;
        }
        (file, size)
    };
    if input.start > size
        || input.length > size - input.start
        || (!input.head && input.length > 40 * 1024)
    {
        return None;
    }
    let mut bytes = Vec::new();
    if !input.head {
        file.seek(SeekFrom::Start(input.start)).ok()?;
        let mut limited = file.take(input.length);
        limited.read_to_end(&mut bytes).ok()?;
        if bytes.len() as u64 != input.length {
            return None;
        }
    }
    Some(json!({"size":size,"bytes":STANDARD.encode(bytes)}))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileInput {
    kind: String,
    root: PathBuf,
    writer: Option<String>,
    path: String,
    range: Option<String>,
    head: bool,
}
fn header(map: &mut Value, key: &str, value: impl Into<String>) {
    map[key] = json!(value.into());
}
fn refusal(media: bool, status: u16, code: &str, error: &str, headers: Value) -> Value {
    json!({"status":status,"headers":headers,"json":if media{json!({"error":error,"code":code})}else{json!({"code":code,"error":error})},"bytes":null})
}
fn media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(&[137, 80, 78, 71, 13, 10, 26, 10]) {
        return Some("image/png");
    }
    if bytes.starts_with(&[255, 216, 255]) {
        return Some("image/jpeg");
    }
    if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if bytes.len() >= 12 && &bytes[4..8] == b"ftyp" {
        if [
            b"avif", b"avis", b"heic", b"heix", b"heim", b"heis", b"mif1", b"msf1", b"qt  ",
        ]
        .iter()
        .any(|b| b.as_slice() == &bytes[8..12])
        {
            return None;
        }
        return Some("video/mp4");
    }
    if bytes.starts_with(&[26, 69, 223, 163]) && bytes[4..].windows(4).any(|w| w == b"webm") {
        return Some("video/webm");
    }
    None
}
fn share_type(path: &str) -> Option<&'static str> {
    let name = Path::new(path).extension()?.to_str()?.to_ascii_lowercase();
    Some(match name.as_str() {
        "html" => "text/html; charset=utf-8",
        "md" | "txt" => "text/plain; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "pdf" => "application/pdf",
        _ => return None,
    })
}
enum Range {
    Whole,
    Part(u64, u64),
    Unsatisfiable,
}
fn js_space(c: char) -> bool {
    matches!(c,'\u{0009}'..='\u{000d}'|'\u{0020}'|'\u{00a0}'|'\u{1680}'|'\u{2000}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{202f}'|'\u{205f}'|'\u{3000}'|'\u{feff}')
}
fn range(raw: Option<&str>, size: u64) -> Range {
    let Some(raw) = raw else {
        return Range::Whole;
    };
    let Some(spec) = raw.trim_matches(js_space).strip_prefix("bytes=") else {
        return Range::Whole;
    };
    let Some((a, b)) = spec.split_once('-') else {
        return Range::Whole;
    };
    if a.is_empty() && b.is_empty() || !a.bytes().chain(b.bytes()).all(|b| b.is_ascii_digit()) {
        return Range::Whole;
    }
    let parse = |s: &str| s.parse::<f64>().unwrap_or(f64::INFINITY);
    let safe = |n: f64| n.is_finite() && n.fract() == 0.0 && n <= 9_007_199_254_740_991.0;
    if a.is_empty() {
        let n = parse(b);
        if !safe(n) || n == 0.0 || size == 0 {
            return Range::Unsatisfiable;
        }
        return Range::Part(size.saturating_sub(n as u64), size - 1);
    }
    let start = parse(a);
    let end = if b.is_empty() {
        size as f64 - 1.0
    } else {
        parse(b).min(size as f64 - 1.0)
    };
    if !safe(start) || !safe(end) {
        return Range::Whole;
    }
    if start >= size as f64 || start > end {
        return Range::Unsatisfiable;
    }
    Range::Part(start as u64, end as u64)
}
pub fn answer(route: &str, bytes: &[u8]) -> Option<Value> {
    if serde_json::from_slice::<Value>(bytes)
        .ok()?
        .get("kind")
        .is_none()
    {
        return legacy(route, bytes);
    }
    let (reply, body) = prepare(route, bytes)?;
    let Some((file, length)) = body else {
        return Some(reply);
    };
    // The compatibility JSON transport is bounded. The public route streams the same handle.
    if length > 1024 * 1024 {
        return None;
    }
    let mut body = Vec::new();
    file.take(length).read_to_end(&mut body).ok()?;
    if body.len() as u64 != length {
        return None;
    }
    let mut reply = reply;
    reply["bytes"] = json!(STANDARD.encode(body));
    Some(reply)
}

type FileReply = (Value, Option<(File, u64)>);

/// Prepare a public file reply and, for GET, its verified handle positioned at the range start.
pub(crate) fn prepare(route: &str, bytes: &[u8]) -> Option<FileReply> {
    let input: FileInput =
        crate::json_object::from_slice(bytes, &["kind", "root", "writer", "path", "range", "head"])
            .ok()?;
    match open_file(route, input)? {
        Ok((headers, status, body)) => Some((
            json!({"status":status,"headers":headers,"json":null,"bytes":""}),
            body,
        )),
        Err(refusal) => Some((refusal, None)),
    }
}

type Opened = (Value, u16, Option<(File, u64)>);

fn open_file(route: &str, input: FileInput) -> Option<Result<Opened, Value>> {
    let media = match input.kind.as_str() {
        "media" if route.starts_with("/api/status/files/") => true,
        "deliverable" if route == "/api/status/deliverable" => false,
        _ => return None,
    };
    let mut headers = json!({});
    let not_found = || {
        refusal(
            media,
            404,
            "not_found",
            "This file is not available.",
            json!({}),
        )
    };
    let (base, relative) = if media {
        let writer = input.writer.as_deref()?;
        if !writer_ok(writer) || !media_path(&input.path) {
            return Some(Err(not_found()));
        }
        let base = format!("v1/writers/{writer}");
        (base.clone(), format!("{base}/{}", input.path))
    } else {
        if !plain(&input.path) {
            return Some(Err(refusal(
                false,
                400,
                "invalid_path",
                "Use a share-relative file path.",
                headers,
            )));
        }
        let parts: Vec<_> = input.path.split('/').collect();
        let base = if parts[0] == "share" && parts.len() > 1 {
            "share".to_owned()
        } else if parts.len() > 5
            && parts[..3] == ["agent-reports", "v1", "writers"]
            && parts[4] == "media"
            && parts[3].len() <= 128
            && parts[3].as_bytes()[0].is_ascii_alphanumeric()
            && parts[3]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        {
            parts[..5].join("/")
        } else {
            return Some(Err(refusal(
                false,
                400,
                "invalid_path",
                "Use a share-relative file path.",
                headers,
            )));
        };
        (base, input.path.clone())
    };
    let deliverable_type = if media {
        None
    } else {
        match share_type(&input.path) {
            Some(t) => Some(t),
            None => {
                return Some(Err(refusal(
                    false,
                    415,
                    "unsupported_type",
                    "This file type is not supported.",
                    headers,
                )));
            }
        }
    };
    let opened = if media {
        confined_media(&input.root, input.writer.as_deref()?, &input.path)
    } else {
        confined(&input.root, &base, &relative)
    };
    let Some((mut file, size)) = opened else {
        return Some(Err(not_found()));
    };
    let content_type = if media {
        let mut head = [0u8; 64];
        let read = file.read(&mut head).ok()?;
        let Some(t) = media_type(&head[..read]) else {
            return Some(Err(refusal(
                true,
                415,
                "not_media",
                "This file is not a PNG, JPEG or WebP image or an MP4 or WebM video.",
                headers,
            )));
        };
        let cap = if t.starts_with("video/") {
            50 * 1024 * 1024
        } else {
            2 * 1024 * 1024
        };
        if size > cap {
            return Some(Err(refusal(
                true,
                413,
                "too_large",
                "This file is larger than its limit (images 2 MB, videos 50 MB).",
                headers,
            )));
        }
        t
    } else {
        if size > 100 * 1024 * 1024 {
            return Some(Err(refusal(
                false,
                413,
                "too_large",
                "This file exceeds the 100 MB limit.",
                headers,
            )));
        }
        deliverable_type?
    };
    if media {
        header(&mut headers, "x-content-type-options", "nosniff");
        header(
            &mut headers,
            "content-security-policy",
            "default-src 'none'; sandbox",
        );
        header(&mut headers, "accept-ranges", "bytes");
    }
    let r = if media || content_type.starts_with("video/") {
        range(input.range.as_deref(), size)
    } else {
        Range::Whole
    };
    let (start, length, status) = match r {
        Range::Whole => (0, size, 200),
        Range::Part(a, b) => (a, b - a + 1, 206),
        Range::Unsatisfiable => {
            header(&mut headers, "content-range", format!("bytes */{size}"));
            return Some(Err(if media {
                refusal(
                    true,
                    416,
                    "range_not_satisfiable",
                    "The requested range is outside this file.",
                    headers,
                )
            } else {
                refusal(
                    false,
                    416,
                    "invalid_range",
                    "The requested range is outside this file.",
                    headers,
                )
            }));
        }
    };
    if media {
        header(&mut headers, "cache-control", "private, max-age=300");
    }
    header(&mut headers, "content-type", content_type);
    if !media && input.path.to_ascii_lowercase().ends_with(".svg") {
        header(&mut headers, "content-disposition", "attachment");
    }
    if content_type.starts_with("video/") {
        header(&mut headers, "accept-ranges", "bytes");
    }
    if status == 206 {
        header(
            &mut headers,
            "content-range",
            format!("bytes {start}-{}/{size}", start + length - 1),
        );
    }
    header(&mut headers, "content-length", length.to_string());
    let body = if input.head {
        None
    } else {
        file.seek(SeekFrom::Start(start)).ok()?;
        Some((file, length))
    };
    Some(Ok((headers, status, body)))
}

fn confined_media(root: &Path, writer: &str, path: &str) -> Option<(File, u64)> {
    let writers = fs::canonicalize(root.join("v1/writers")).ok()?;
    let base = writers.join(writer);
    if fs::canonicalize(&base).ok()? != base {
        return None;
    }
    let target = fs::canonicalize(base.join(path)).ok()?;
    if !target.starts_with(&base) || target == base {
        return None;
    }
    let (file, size) = open_resolved(&target)?;
    let opened = opened_path(&file)?;
    if !opened.starts_with(&base) || opened == base {
        return None;
    }
    Some((file, size))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_writers_are_the_computer_tables_machines() {
        let machines = crate::account_reads::machine_ids().expect("computers.json machine ids");
        assert!(machines.len() >= 2, "{machines:?}");
        for machine in &machines {
            assert!(writer_ok(&format!("{machine}.lead")), "{machine}");
            assert!(!writer_ok(&format!("{machine}.Lead")), "{machine}");
        }
        assert!(!writer_ok("lab.lead"));
        assert!(!writer_ok("vm"));
        // A computer added to the table is a writer machine without editing this module.
        let mut table: Value =
            serde_json::from_str(crate::account_reads::COMPUTERS).expect("computers.json");
        table["lab"] = json!({"machineId": "lab"});
        let machines =
            crate::account_reads::machine_ids_in(&table.to_string()).expect("machine ids");
        assert!(writer_in("lab.lead", &machines));
        assert!(!writer_in("lab2.lead", &machines));
    }
}
