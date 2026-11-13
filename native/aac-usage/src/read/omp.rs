//! OMP (and Pi) sessions: `<ts>_<uuid>.jsonl` files whose assistant messages carry `usage`.
//! The Python helper's `_omp_roots`, `_scan_session_roots`, `_drop_resume_copies` and `_parse_omp_line`.

use super::{LineBox, Outcome, newest_first, read_file_with};
use crate::collector::{Collector, MAX_FILES};
use crate::discover::{self, Env, T3Instance};
use crate::fsx::{self, skipped};
use crate::util::{self, abspath, path_text};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

const SCAN_MAX_DEPTH: usize = 12;
const SCAN_MAX_DIRS: usize = 100_000;
const SCAN_MAX_ENTRIES: usize = 2_000_000;
const SCAN_BUDGET_SHARE: f64 = 0.4;
const SCAN_MAX_ROOTS: usize = 512;
const SANDBOX_MARKER: &str = ".aac-synthetic";

fn is_ts_prefix(text: &str) -> bool {
    // ^\d{4}-\d{2}-\d{2}T\d{2}[:-]\d{2}
    let b = text.as_bytes();
    b.len() >= 16
        && b[..4].iter().all(u8::is_ascii_digit)
        && b[4] == b'-'
        && b[5..7].iter().all(u8::is_ascii_digit)
        && b[7] == b'-'
        && b[8..10].iter().all(u8::is_ascii_digit)
        && b[10] == b'T'
        && b[11..13].iter().all(u8::is_ascii_digit)
        && (b[13] == b':' || b[13] == b'-')
        && b[14..16].iter().all(u8::is_ascii_digit)
}

/// Python's `_is_session_filename`.
pub fn is_session_filename(name: &str) -> bool {
    if name == "__advisor.jsonl" {
        return true;
    }
    if let Some(stem) = name.strip_suffix(".jsonl") {
        return is_ts_prefix(stem) || is_ts_prefix(name);
    }
    is_ts_prefix(name) && !name.contains('.') && name.contains('_')
}

/// Python's `_omp_session_id`: the file's `<ts>_<uuid>` stem, else its folder's name.
fn session_id(path: &Path) -> Option<String> {
    let base = path.file_name()?.to_string_lossy().into_owned();
    if base != "__advisor.jsonl" && is_session_filename(&base) {
        let stem = base.strip_suffix(".jsonl").unwrap_or(&base);
        return Some(util::prefix_chars(stem, util::SESSION_MAX_LEN));
    }
    let parent = path.parent()?.file_name()?.to_string_lossy().into_owned();
    if parent.is_empty() || parent == "." {
        return None;
    }
    Some(util::prefix_chars(&parent, util::SESSION_MAX_LEN))
}

fn is_sandbox_tree(directory: &Path, entries: &[fs::DirEntry]) -> bool {
    let names: HashSet<String> = entries
        .iter()
        .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    if names.contains(SANDBOX_MARKER) || names.contains("SANDBOX.md") {
        return true;
    }
    if !names.contains("MANIFEST.json") {
        return false;
    }
    let Ok(mut file) = File::open(directory.join("MANIFEST.json")) else {
        return false;
    };
    let mut text = Vec::new();
    if file.by_ref().take(64 * 1024 + 1).read_to_end(&mut text).is_err() || text.len() > 64 * 1024 {
        return false;
    }
    serde_json::from_slice::<Value>(&text)
        .ok()
        .and_then(|v| v.get("trees").cloned())
        .is_some_and(|t| t.is_object())
}

fn sessions_dir_has_marker(directory: &Path) -> bool {
    let mut pending = vec![(directory.to_path_buf(), 0usize)];
    let mut checked = 0;
    while let Some((current, depth)) = pending.pop() {
        let Ok(list) = fs::read_dir(&current) else {
            return false;
        };
        for entry in list.flatten().take(200) {
            checked += 1;
            if checked > 400 {
                return false;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let path = entry.path();
            if fsx::is_file(&path) {
                if is_session_filename(&name) {
                    return true;
                }
            } else if depth < 1 && fsx::is_dir(&path) && !fsx::is_link(&path) {
                pending.push((path, depth + 1));
            }
        }
    }
    false
}

/// Python's `_scan_session_roots`: breadth-first marker scan for custom OMP session roots under base.
fn scan_session_roots(base: &Path, collector: &mut Collector) -> Vec<PathBuf> {
    let mut found = Vec::new();
    if !fsx::is_dir(base) {
        return found;
    }
    let deadline = collector
        .deadline()
        .min(Instant::now() + Duration::from_secs_f64(collector.budget * SCAN_BUDGET_SHARE));
    let mut pending: VecDeque<(PathBuf, usize)> = VecDeque::from([(base.to_path_buf(), 0)]);
    let (mut visited, mut examined) = (0usize, 0usize);
    let read = |dir: &Path| -> Option<Vec<fs::DirEntry>> { fs::read_dir(dir).ok().map(|l| l.flatten().collect()) };
    let session_file = |entries: &[fs::DirEntry]| {
        entries.iter().any(|e| {
            e.file_type().map(|t| t.is_file()).unwrap_or(false) && is_session_filename(&e.file_name().to_string_lossy())
        })
    };
    while let Some((directory, depth)) = pending.pop_front() {
        if found.len() >= SCAN_MAX_ROOTS {
            pending.push_front((directory, depth));
            break;
        }
        if visited >= SCAN_MAX_DIRS || examined > SCAN_MAX_ENTRIES || Instant::now() >= deadline {
            collector.discovery_truncated = true;
            break;
        }
        if depth > SCAN_MAX_DEPTH {
            continue;
        }
        visited += 1;
        let Some(entries) = read(&directory) else {
            continue;
        };
        examined += entries.len();
        if examined > SCAN_MAX_ENTRIES {
            collector.discovery_truncated = true;
            break;
        }
        if is_sandbox_tree(&directory, &entries) {
            continue;
        }
        if directory != base {
            if directory.file_name().is_some_and(|n| n == "sessions") {
                if sessions_dir_has_marker(&directory) {
                    found.push(directory);
                }
                continue;
            }
            if session_file(&entries) {
                found.push(directory);
                continue;
            }
        }
        if depth >= SCAN_MAX_DEPTH {
            if entries.iter().any(|e| {
                e.file_type().map(|t| t.is_dir()).unwrap_or(false) && !skipped(&e.file_name().to_string_lossy())
            }) {
                collector.discovery_truncated = true;
            }
            if depth == SCAN_MAX_DEPTH {
                for entry in &entries {
                    if found.len() >= SCAN_MAX_ROOTS
                        || visited >= SCAN_MAX_DIRS
                        || examined > SCAN_MAX_ENTRIES
                        || Instant::now() >= deadline
                    {
                        break;
                    }
                    let name = entry.file_name().to_string_lossy().into_owned();
                    if !entry.file_type().map(|t| t.is_dir()).unwrap_or(false) || !name.contains("sessions") {
                        continue;
                    }
                    visited += 1;
                    let Some(children) = read(&entry.path()) else {
                        continue;
                    };
                    examined += children.len() + 1;
                    if is_sandbox_tree(&entry.path(), &children) {
                        continue;
                    }
                    if name == "sessions" {
                        if sessions_dir_has_marker(&entry.path()) {
                            found.push(entry.path());
                        }
                    } else if session_file(&children) {
                        found.push(entry.path());
                    }
                }
            }
            continue;
        }
        let mut dirs: Vec<(String, PathBuf)> = entries
            .iter()
            .filter(|e| {
                !skipped(&e.file_name().to_string_lossy()) && e.file_type().map(|t| t.is_dir()).unwrap_or(false)
            })
            .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
            .collect();
        // os.scandir order is the directory's own; sort for a stable scan.
        dirs.sort();
        for (_, path) in dirs {
            pending.push_back((path, depth + 1));
        }
    }
    if !pending.is_empty() && found.len() >= SCAN_MAX_ROOTS {
        collector.discovery_truncated = true;
    }
    found
}

/// Python's `_omp_roots`, each root with its origin.
fn roots(
    home: &Path,
    env: &Env,
    collector: &mut Collector,
    extra: &[PathBuf],
    instances: &[T3Instance],
) -> Vec<PathBuf> {
    let mut list: Vec<PathBuf> = Vec::new();
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut add = |path: PathBuf, origin: &str, collector: &mut Collector, list: &mut Vec<PathBuf>| {
        let absolute = abspath(&path);
        if seen.insert(absolute.clone()) {
            collector.note_root("omp", &absolute, origin);
            list.push(absolute);
        }
    };
    let agent = home.join(".omp").join("agent");
    add(agent.join("sessions"), "default", collector, &mut list);
    let profiles = home.join(".omp").join("profiles");
    if let Some(names) = fsx::sorted_names(&profiles) {
        for name in names.into_iter().take(SCAN_MAX_ROOTS) {
            let candidate = profiles.join(name).join("agent").join("sessions");
            if fsx::is_dir(&candidate) {
                add(candidate, "default", collector, &mut list);
            }
        }
    }
    if let Some(dir) = discover::env_dir(env, "PI_CODING_AGENT_DIR", home) {
        add(dir.join("sessions"), "env", collector, &mut list);
    }
    if let Some(extra_dirs) = env.get("OMP_SESSION_DIRS") {
        let separator = if cfg!(windows) { ';' } else { ':' };
        for piece in extra_dirs.split(separator) {
            let piece = util::py_strip(piece);
            if piece.is_empty() {
                continue;
            }
            let expanded = if let Some(rest) = piece.strip_prefix("~/").or_else(|| piece.strip_prefix("~\\")) {
                home.join(rest)
            } else if piece == "~" {
                home.to_path_buf()
            } else {
                PathBuf::from(piece)
            };
            if expanded.is_absolute() {
                add(expanded, "env", collector, &mut list);
            }
        }
    }
    for found in discover::omp_config_roots(home, instances) {
        add(found, "tool_config", collector, &mut list);
    }
    for found in scan_session_roots(&home.join("PM-Experiments"), collector) {
        add(found, "signature", collector, &mut list);
    }
    if list.len() > SCAN_MAX_ROOTS {
        collector.discovery_truncated = true;
    }
    list.truncate(SCAN_MAX_ROOTS);
    // Saved extras are configuration, not discovery: they join after the cap, never cut by it.
    for path in extra {
        let absolute = abspath(path);
        if seen.insert(absolute.clone()) {
            collector.note_root("omp", &absolute, "extra");
            list.push(absolute);
        }
    }
    list
}

/// The session-named part of a path (Python's `_session_copy_key`).
fn session_copy_key(path: &Path) -> Option<String> {
    let parts: Vec<String> = abspath(path).iter().map(|p| p.to_string_lossy().into_owned()).collect();
    for index in (0..parts.len()).rev() {
        let part = &parts[index];
        if part != "__advisor.jsonl" && is_session_filename(part) {
            return Some(parts[index..].join("/"));
        }
    }
    None
}

fn is_prefix_copy(small: &(PathBuf, u64), large: &(PathBuf, u64)) -> bool {
    if small.1 > large.1 {
        return false;
    }
    let head = small.1.min(256) as usize;
    let tail = small.1.saturating_sub(256);
    let (Ok(mut left), Ok(mut right)) = (File::open(&small.0), File::open(&large.0)) else {
        return false;
    };
    let mut a = vec![0u8; head];
    let mut b = vec![0u8; head];
    if left.read_exact(&mut a).is_err() || right.read_exact(&mut b).is_err() || a != b {
        return false;
    }
    let length = (small.1 - tail) as usize;
    let mut a = vec![0u8; length];
    let mut b = vec![0u8; length];
    left.seek(SeekFrom::Start(tail)).is_ok()
        && right.seek(SeekFrom::Start(tail)).is_ok()
        && left.read_exact(&mut a).is_ok()
        && right.read_exact(&mut b).is_ok()
        && a == b
}

/// Python's `_drop_resume_copies`: the longest file per session; a byte-prefix copy is dropped.
fn drop_resume_copies(paths: &[PathBuf]) -> Vec<PathBuf> {
    let mut groups: HashMap<String, Vec<(PathBuf, u64)>> = HashMap::new();
    for path in paths {
        let Some(key) = session_copy_key(path) else {
            continue;
        };
        if let Ok(meta) = fs::metadata(path) {
            groups.entry(key).or_default().push((path.clone(), meta.len()));
        }
    }
    let mut dropped: HashSet<PathBuf> = HashSet::new();
    for group in groups.values_mut() {
        if group.len() < 2 {
            continue;
        }
        group.sort_by(|a, b| b.1.cmp(&a.1).then(path_text(&a.0).cmp(&path_text(&b.0))));
        let mut kept: Vec<(PathBuf, u64)> = Vec::new();
        for item in group.iter() {
            if kept.iter().any(|larger| is_prefix_copy(item, larger)) {
                dropped.insert(item.0.clone());
            } else {
                kept.push(item.clone());
            }
        }
    }
    paths.iter().filter(|p| !dropped.contains(*p)).cloned().collect()
}

/// Python's `_parse_omp_line` (also Pi's format).
pub fn parse_line(line: &str, collector: &mut Collector, kind: &str, filekey: &str, line_box: &mut LineBox) {
    if line_box.offset == 0 && line.contains("\"cwd\"") && collector.capture.is_none() {
        // The session header names its working folder: identity evidence only.
        if let Ok(header) = serde_json::from_str::<Value>(line)
            && let Some(cwd) = header.get("cwd").and_then(Value::as_str)
        {
            line_box.cwd = Some(cwd.to_string());
        }
    }
    if !line.contains("\"type\"") || !line.contains("\"message\"") || !line.contains("\"usage\"") {
        return;
    }
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if record.get("type").and_then(Value::as_str) != Some("message") {
        return;
    }
    let Some(message) = record.get("message").filter(|m| m.is_object()) else {
        return;
    };
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(model) = util::clean_model(message.get("model")) else {
        return;
    };
    let Some(usage) = message.get("usage").filter(|u| u.is_object()) else {
        return;
    };
    let values = [
        util::non_negative(usage.get("input")),
        util::non_negative(usage.get("output")),
        util::non_negative(usage.get("cacheRead")),
        util::non_negative(usage.get("cacheWrite")),
    ];
    if values.iter().any(Option::is_none) {
        return;
    }
    let tokens = values.map(|v| util::int_of(v.unwrap_or(0.0)));
    let cost = usage
        .get("cost")
        .and_then(|c| util::non_negative(c.get("total")))
        .unwrap_or(0.0);
    // Zero-usage pings carry nothing; only usage.* is read (never cumulative counters).
    if tokens.iter().sum::<u64>() == 0 && cost == 0.0 {
        return;
    }
    let epoch_ms = match record.get("timestamp") {
        Some(Value::String(text)) => match util::iso_ms(text) {
            Some(ms) => ms,
            None => return,
        },
        Some(Value::Number(number)) => {
            let Some(mut seconds) = number.as_f64().map(|f| f as i64) else {
                return;
            };
            while seconds > 4_102_444_800 {
                seconds /= 1000;
            }
            seconds * 1000
        }
        _ => return,
    };
    if epoch_ms < collector.min_date_ms {
        return;
    }
    let Some(hour) = util::hour_label(epoch_ms, collector.now_ms) else {
        return;
    };
    let provider = util::clean_provider(message.get("provider"));
    collector.add(kind, filekey, &model, provider.as_deref(), &hour, tokens, cost);
    let session = line_box.session.clone();
    let session_key = util::session_key(kind, session.as_deref());
    collector.sadd(
        kind,
        filekey,
        session_key.as_deref(),
        &model,
        provider.as_deref(),
        epoch_ms,
        tokens,
        cost,
    );
    if let Some(raw) = session.as_deref() {
        collector.note_session(kind, raw, line_box.cwd.clone());
    }
    if line_box.offset >= line_box.message_from {
        let id = record.get("id").and_then(Value::as_str).map(str::to_string);
        let unit = id.map_or_else(
            || format!("@{filekey}:{}", line_box.offset),
            |id| format!("{}:{id}", session.as_deref().unwrap_or("")),
        );
        let key = util::sha256_hex(format!("aac-msg-v1:{kind}:{unit}").as_bytes())[..16].to_string();
        collector.message(
            kind,
            filekey,
            key,
            epoch_ms,
            session_key.as_deref(),
            &model,
            provider.as_deref(),
            tokens,
            cost,
        );
    }
}

/// Python's `_collect_omp`.
pub fn collect(
    collector: &mut Collector,
    home: &Path,
    env: &Env,
    extra: &[PathBuf],
    experiments: &[PathBuf],
    instances: &[T3Instance],
) -> &'static str {
    let mut listed = roots(home, env, collector, extra, instances);
    // Experiment log folders (DESIGN 16.5); a copy of a session counts once by its file identity.
    for path in experiments {
        collector.note_root("omp", path, "experiment");
        listed.push(path.clone());
    }
    let roots: Vec<PathBuf> = fsx::dedup_dirs(&listed)
        .into_iter()
        .filter(|r| fsx::is_dir(r))
        .collect();
    if roots.is_empty() {
        return "not_installed";
    }
    let accept = |name: &str| name.ends_with(".jsonl") || is_session_filename(name);
    let mut paths: Vec<PathBuf> = Vec::new();
    for root in &roots {
        let start = paths.len();
        let mut capped = false;
        for path in fsx::walk_files(root, &accept, collector, None) {
            paths.push(path);
            if paths.len() > MAX_FILES {
                collector.truncated = true;
                capped = true;
                break;
            }
        }
        let added = paths[start..].to_vec();
        collector.count_root("omp", root, &added);
        if collector.expired() || capped {
            break;
        }
    }
    for path in drop_resume_copies(&newest_first(&paths)) {
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
        let Some(filekey) = collector.note_file("omp", &path, None) else {
            let staged = util::filekey("omp", &path);
            if collector.is_pending("omp", &staged) {
                collector.confirm_file("omp", &staged);
            }
            continue;
        };
        let line_box = LineBox {
            session: session_id(&path),
            ..LineBox::default()
        };
        match read_file_with(collector, "omp", &path, &filekey, parse_line, None, line_box) {
            Outcome::Cut => return "ok",
            Outcome::Unreadable => continue,
            Outcome::Ok => {
                collector.sent_through("omp", &filekey, &path);
                collector.confirm_file("omp", &filekey);
            }
        }
    }
    "ok"
}
