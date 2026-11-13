//! Claude Code transcripts (`projects/**/*.jsonl`): assistant messages with usage, one count per API
//! response, copies of one transcript in several folders counted once (copy groups). The Python
//! helper's `_parse_claude_line`, `_emit_claude_response` and `_scan_claude_groups`.

use super::{Flush, LineBox, Outcome, Parse, newest_first, read_file};
use crate::collector::{Collector, MAX_CLAUDE_KEYS, MAX_FILES, Tokens};
use crate::discover::{self, Env, T3Instance};
use crate::util::{self, path_text};
use serde_json::Value;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// A response in progress: (message+request key, model, hour, tokens, epoch ms, session key, raw-key).
#[derive(Clone, Debug)]
pub struct Pending {
    pub key: Option<String>,
    pub model: String,
    pub hour: String,
    pub tokens: Tokens,
    pub epoch_ms: i64,
    pub session: Option<String>,
    /// Per-message key source: the message id, else the line offset.
    pub message_key: String,
    /// Byte offset of the response's latest line (an updated response is sent again, same key).
    pub offset: u64,
}

/// An SSH mirror transcript: a path part named `ssh-<uuid>`.
pub fn is_mirror(path: &Path) -> bool {
    let normal = util::normpath(path);
    let parts: Vec<String> = normal.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    parts[..parts.len().saturating_sub(1)].iter().any(|part| {
        part.len() == 40
            && part[..4].eq_ignore_ascii_case("ssh-")
            && part[4..].chars().all(|c| c.is_ascii_hexdigit() || c == '-')
    })
}

pub fn parse_line(line: &str, collector: &mut Collector, kind: &str, filekey: &str, line_box: &mut LineBox) {
    // Pre-filter before parsing, so conversation content is never decoded.
    if !line.contains("\"type\"") || !line.contains("\"assistant\"") || !line.contains("\"usage\"") {
        return;
    }
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if record.get("type").and_then(Value::as_str) != Some("assistant") {
        return;
    }
    let Some(message) = record.get("message").filter(|m| m.is_object()) else {
        return;
    };
    let Some(model) = util::clean_model(message.get("model")) else {
        return;
    };
    let Some(usage) = message.get("usage").filter(|u| u.is_object()) else {
        return;
    };
    let (Some(input), Some(output)) = (
        util::non_negative(usage.get("input_tokens")),
        util::non_negative(usage.get("output_tokens")),
    ) else {
        return;
    };
    let cache_read = util::non_negative(usage.get("cache_read_input_tokens")).unwrap_or(0.0);
    let cache_write = util::non_negative(usage.get("cache_creation_input_tokens")).unwrap_or(0.0);
    let tokens = [
        util::int_of(input),
        util::int_of(output),
        util::int_of(cache_read),
        util::int_of(cache_write),
    ];
    let Some(epoch_ms) = record.get("timestamp").and_then(Value::as_str).and_then(util::iso_ms) else {
        return;
    };
    if epoch_ms < collector.min_date_ms {
        return;
    }
    let Some(hour) = util::hour_label(epoch_ms, collector.now_ms) else {
        return;
    };
    let raw_session = record
        .get("sessionId")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty());
    let session = util::session_key(kind, raw_session);
    if let Some(raw) = raw_session
        && collector.capture.is_none()
    {
        let cwd = record.get("cwd").and_then(Value::as_str).map(str::to_string);
        collector.note_session(kind, raw, cwd);
    }
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.chars().count() <= 200);
    let request_id = match record.get("requestId").or_else(|| message.get("requestId")) {
        None | Some(Value::Null) => Some(None),
        Some(Value::String(text)) if text.chars().count() <= 200 => Some(Some(text.as_str())),
        _ => None,
    };
    let message_key = match message_id {
        Some(id) => id.to_string(),
        None => format!("@{filekey}:{}", line_box.offset),
    };
    let pending = Pending {
        key: None,
        model,
        hour,
        tokens,
        epoch_ms,
        session,
        message_key,
        offset: line_box.offset,
    };
    if let (Some(id), Some(request)) = (message_id, request_id) {
        let key = format!("{id}\n{}", request.unwrap_or(""));
        let pending = Pending {
            key: Some(key.clone()),
            ..pending
        };
        match line_box.claude_pending.take() {
            Some(previous) if previous.key.as_deref() == Some(key.as_str()) => {}
            Some(previous) => emit(collector, kind, filekey, previous, line_box.message_from),
            None => {}
        }
        line_box.claude_pending = Some(pending);
        return;
    }
    if let Some(previous) = line_box.claude_pending.take() {
        emit(collector, kind, filekey, previous, line_box.message_from);
    }
    emit(collector, kind, filekey, pending, line_box.message_from);
}

/// Identity of a response when copies are reconciled (Python: "m:<message id>", else the tuple).
fn identity(pending: &Pending) -> String {
    match &pending.key {
        Some(key) => format!("m:{}", key.split('\n').next().unwrap_or("")),
        None => format!(
            "t:{}|{}|{}|{:?}",
            pending.session.as_deref().unwrap_or(""),
            pending.epoch_ms,
            pending.model,
            pending.tokens
        ),
    }
}

/// Copy groups being reconciled collect the best (most output) record per response here.
pub struct Chosen(pub HashMap<String, Pending>);

thread_local! {
    static CHOSEN: std::cell::RefCell<Option<HashMap<String, Pending>>> = const { std::cell::RefCell::new(None) };
}

fn emit(collector: &mut Collector, kind: &str, filekey: &str, pending: Pending, message_from: u64) {
    let chosen = CHOSEN.with(|cell| cell.borrow().is_some());
    if chosen {
        let id = identity(&pending);
        let mut capped = false;
        CHOSEN.with(|cell| {
            let mut borrow = cell.borrow_mut();
            let map = borrow.as_mut().expect("chosen map");
            let prior = map.get(&id);
            if prior.is_none() && map.len() >= MAX_CLAUDE_KEYS {
                capped = true;
                return;
            }
            if prior.is_none_or(|p| pending.tokens[1] > p.tokens[1]) {
                map.insert(id, pending);
            }
        });
        if capped {
            collector.row_cap = true;
            collector.truncated = true;
        }
        return;
    }
    // A copy is the same API response: its message id, as the server's experiment dedup keys it.
    let record_key = pending
        .key
        .as_ref()
        .map(|k| format!("m:{}", k.split('\n').next().unwrap_or("")));
    if !collector.keep_record(record_key) {
        return;
    }
    count(collector, kind, filekey, &pending, message_from);
}

fn count(collector: &mut Collector, kind: &str, filekey: &str, pending: &Pending, message_from: u64) {
    collector.add(kind, filekey, &pending.model, None, &pending.hour, pending.tokens, 0.0);
    collector.sadd(
        kind,
        filekey,
        pending.session.as_deref(),
        &pending.model,
        None,
        pending.epoch_ms,
        pending.tokens,
        0.0,
    );
    let past = pending.offset >= message_from;
    if past {
        let key = util::sha256_hex(format!("aac-msg-v1:{kind}:{}", pending.message_key).as_bytes())[..16].to_string();
        collector.message(
            kind,
            filekey,
            key,
            pending.epoch_ms,
            pending.session.as_deref(),
            &pending.model,
            None,
            pending.tokens,
            0.0,
        );
    }
}

pub fn flush(collector: &mut Collector, kind: &str, filekey: &str, line_box: &mut LineBox) {
    if let Some(pending) = line_box.claude_pending.take() {
        emit(collector, kind, filekey, pending, line_box.message_from);
    }
}

/// Every byte of `small` is a prefix of `large` (bounded; a timeout keeps both for keyed dedupe).
fn is_prefix(small: &Path, large: &Path, deadline: Instant) -> bool {
    let (Ok(a), Ok(b)) = (std::fs::metadata(small), std::fs::metadata(large)) else {
        return false;
    };
    if a.len() > b.len() {
        return false;
    }
    let (Ok(mut left), Ok(mut right)) = (File::open(small), File::open(large)) else {
        return false;
    };
    let mut x = vec![0u8; 1024 * 1024];
    let mut y = vec![0u8; 1024 * 1024];
    while Instant::now() < deadline {
        let read = match read_full(&mut left, &mut x) {
            Ok(0) => return true,
            Ok(read) => read,
            Err(_) => return false,
        };
        match read_full(&mut right, &mut y[..read]) {
            Ok(got) if got == read && x[..read] == y[..read] => {}
            _ => return false,
        }
    }
    false
}

fn read_full(file: &mut File, buffer: &mut [u8]) -> std::io::Result<usize> {
    let mut total = 0;
    while total < buffer.len() {
        let read = file.read(&mut buffer[total..])?;
        if read == 0 {
            break;
        }
        total += read;
    }
    Ok(total)
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum GroupKey {
    Name(bool, String),
    Prior(bool, String),
    Bytes(bool, u64, String, String),
}

/// Python's `_scan_claude_groups`. True when the scan stopped early.
pub fn scan_groups(collector: &mut Collector, paths: &[PathBuf], parse: Parse, flush_fn: Option<Flush>) -> bool {
    let mut unique: Vec<PathBuf> = Vec::new();
    let mut seen = HashSet::new();
    for path in paths {
        if seen.insert(path.clone()) {
            unique.push(path.clone());
        }
    }
    let paths = unique;
    let mut parents: Vec<usize> = (0..paths.len()).collect();
    fn root(parents: &mut [usize], mut index: usize) -> usize {
        while parents[index] != index {
            parents[index] = parents[parents[index]];
            index = parents[index];
        }
        index
    }
    let prior: serde_json::Map<String, Value> = collector.prior_kind("claude").cloned().unwrap_or_default();
    let mut prior_groups: HashMap<String, BTreeSet<String>> = HashMap::new();
    for (key, stamp) in &prior {
        if let Some(group) = stamp
            .get("claudeGroup")
            .and_then(Value::as_str)
            .filter(|g| !g.is_empty())
        {
            prior_groups.entry(group.to_string()).or_default().insert(key.clone());
        }
    }
    let mut aliases: HashMap<GroupKey, usize> = HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        // Each print below opens the file; the budget holds in this pass too.
        if collector.expired() {
            collector.truncated = true;
            return true;
        }
        let mirror = is_mirror(path);
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let mut keys = vec![GroupKey::Name(mirror, name)];
        if let Some(previous) = prior
            .get(&util::filekey("claude", path))
            .and_then(|p| p.get("claudeGroup"))
            .and_then(Value::as_str)
            .filter(|g| !g.is_empty())
        {
            keys.push(GroupKey::Prior(mirror, previous.to_string()));
        }
        if let Some(stamp) = util::fingerprint(path) {
            keys.push(GroupKey::Bytes(mirror, stamp.size, stamp.head, stamp.tail));
        }
        for key in keys {
            let other = *aliases.entry(key).or_insert(index);
            let (a, b) = (root(&mut parents, index), root(&mut parents, other));
            parents[a] = b;
        }
    }
    let mut groups: Vec<(usize, Vec<PathBuf>)> = Vec::new();
    let mut by_root: HashMap<usize, usize> = HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        let r = root(&mut parents, index);
        let slot = *by_root.entry(r).or_insert_with(|| {
            groups.push((r, Vec::new()));
            groups.len() - 1
        });
        groups[slot].1.push(path.clone());
    }
    let order: HashMap<PathBuf, usize> = newest_first(&paths)
        .into_iter()
        .enumerate()
        .map(|(i, p)| (p, i))
        .collect();
    let mut groups: Vec<Vec<PathBuf>> = groups.into_iter().map(|(_, g)| g).collect();
    groups.sort_by_key(|group| {
        group
            .iter()
            .map(|p| order.get(p).copied().unwrap_or(MAX_FILES))
            .min()
            .unwrap_or(MAX_FILES)
    });
    for group in groups {
        if collector.expired() {
            collector.truncated = true;
            return true;
        }
        let Some(group_print) = super::deps_print("claude", &group, collector.deadline()) else {
            collector.truncated = true;
            return true;
        };
        let deps = format!("{}:{}", group_print, util::filekey("claude", &group[0]));
        let members: BTreeSet<String> = group.iter().map(|p| util::filekey("claude", p)).collect();
        let previous: BTreeSet<String> = members
            .iter()
            .filter_map(|key| {
                prior
                    .get(key)
                    .and_then(|p| p.get("claudeGroup"))
                    .and_then(Value::as_str)
            })
            .filter(|g| !g.is_empty())
            .map(str::to_string)
            .collect();
        let identity = members.iter().chain(previous.iter()).min().cloned().unwrap_or_default();
        let mut changed = false;
        let mut usable = Vec::new();
        for path in &group {
            changed = collector.note_file("claude", path, Some(&deps)).is_some() || changed;
            let key = util::filekey("claude", path);
            if collector.is_pending("claude", &key) {
                let group_id = identity.clone();
                collector.set_pending("claude", &key, |p| p.claude_group = Some(group_id));
                usable.push(path.clone());
            } else {
                collector.usage_partial.insert("claude".into());
                collector.truncated = true;
            }
        }
        // A subset cannot replace the coherent cached group: leave every member unconfirmed.
        if usable.len() != group.len() {
            continue;
        }
        if !changed {
            for path in &usable {
                collector.confirm_file("claude", &util::filekey("claude", path));
            }
            continue;
        }
        let mut stamped: Vec<(u64, PathBuf)> = Vec::new();
        for path in &usable {
            match std::fs::metadata(path) {
                Ok(meta) => stamped.push((meta.len(), path.clone())),
                Err(_) => {
                    collector.read_failure("claude", util::filekey("claude", path));
                    collector.truncated = true;
                }
            }
        }
        if stamped.len() != group.len() {
            continue;
        }
        let member_set: HashSet<String> = members.iter().cloned().collect();
        let mut kept: Vec<PathBuf> = Vec::new();
        let mut covered: Vec<(PathBuf, PathBuf)> = Vec::new();
        let prefix_deadline = collector.deadline().min(Instant::now() + Duration::from_millis(100));
        stamped.sort_by(|a, b| b.0.cmp(&a.0).then(path_text(&a.1).cmp(&path_text(&b.1))));
        for (_, path) in &stamped {
            match kept.iter().find(|larger| is_prefix(path, larger, prefix_deadline)) {
                Some(larger) => covered.push((path.clone(), larger.clone())),
                None => kept.push(path.clone()),
            }
        }
        let covered_by = |p: &PathBuf| covered.iter().find(|(small, _)| small == p).map(|(_, l)| l.clone());
        let dedupe = kept.len() > 1;
        let mut filekey = util::filekey("claude", &usable[0]);
        let mut parsed: Vec<PathBuf> = Vec::new();
        let mut chosen: HashMap<String, Pending> = HashMap::new();
        let mut cut = false;
        let mut readers: Vec<PathBuf> = kept.clone();
        readers.extend(covered.iter().map(|(small, _)| small.clone()));
        for path in readers {
            if covered_by(&path).is_some_and(|larger| parsed.contains(&larger)) {
                continue;
            }
            if !dedupe {
                let owner = usable
                    .iter()
                    .find(|p| **p == path || covered_by(p).as_ref() == Some(&path))
                    .cloned()
                    .unwrap_or_else(|| path.clone());
                filekey = util::filekey("claude", &owner);
            }
            CHOSEN.with(|cell| *cell.borrow_mut() = if dedupe { Some(HashMap::new()) } else { None });
            let outcome = read_file(collector, "claude", &path, &filekey, parse, flush_fn);
            let records = CHOSEN.with(|cell| cell.borrow_mut().take());
            if outcome == Outcome::Cut {
                cut = true;
                break;
            }
            if outcome == Outcome::Unreadable {
                collector.read_failure("claude", util::filekey("claude", &path));
                collector.drop_file_rows(&HashSet::from([filekey.clone()]));
                collector.truncated = true;
                continue;
            }
            if dedupe && let Some(records) = records {
                // Merge this copy's responses into the group's choice (more output wins).
                for (id, pending) in records {
                    match chosen.get(&id) {
                        Some(prior) if pending.tokens[1] <= prior.tokens[1] => {}
                        _ => {
                            if !chosen.contains_key(&id) && chosen.len() >= MAX_CLAUDE_KEYS {
                                collector.row_cap = true;
                                collector.truncated = true;
                                continue;
                            }
                            chosen.insert(id, pending);
                        }
                    }
                }
            }
            parsed.push(path.clone());
            if !dedupe {
                break;
            }
        }
        CHOSEN.with(|cell| *cell.borrow_mut() = None);
        if cut || collector.row_cap {
            collector.drop_file_rows(&member_set);
            return true;
        }
        if parsed.is_empty() {
            continue;
        }
        // Only parsed files and byte-verified prefixes of parsed files are fresh.
        let confirmed: HashSet<PathBuf> = parsed
            .iter()
            .cloned()
            .chain(
                covered
                    .iter()
                    .filter(|(_, larger)| parsed.contains(larger))
                    .map(|(small, _)| small.clone()),
            )
            .collect();
        if confirmed.len() != group.len() {
            collector.drop_file_rows(&member_set);
            continue;
        }
        let owner = usable
            .iter()
            .find(|p| confirmed.contains(*p))
            .cloned()
            .unwrap_or_else(|| parsed[0].clone());
        let filekey = util::filekey("claude", &owner);
        let mut chosen: Vec<Pending> = chosen.into_values().collect();
        chosen.sort_by(|a, b| a.epoch_ms.cmp(&b.epoch_ms).then(a.message_key.cmp(&b.message_key)));
        for pending in chosen {
            count(collector, "claude", &filekey, &pending, 0);
            if collector.row_cap {
                collector.drop_file_rows(&member_set);
                return true;
            }
        }
        for path in &group {
            if confirmed.contains(path) {
                let key = util::filekey("claude", path);
                collector.sent_through("claude", &key, path);
                collector.confirm_file("claude", &key);
            }
        }
        // A fully read group also retires previous owners no longer present.
        collector.replaced_files.extend(members.iter().cloned());
        for old in previous {
            if let Some(keys) = prior_groups.get(&old) {
                collector.replaced_files.extend(keys.iter().cloned());
            }
        }
    }
    false
}

/// Python's `_collect_claude`: "not_installed" when no root exists, else "ok".
pub fn collect(
    collector: &mut Collector,
    home: &Path,
    env: &Env,
    extra: &[PathBuf],
    instances: &[T3Instance],
) -> &'static str {
    let plan = discover::claude_roots(home, env, extra, instances);
    for (path, origin) in plan.defaults.iter().chain(plan.others.iter()) {
        collector.note_root("claude", path, origin);
    }
    let dirs = crate::fsx::dedup_dirs(&plan.defaults.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>());
    let others: Vec<PathBuf> = crate::fsx::dedup_dirs(&plan.others.iter().map(|(p, _)| p.clone()).collect::<Vec<_>>())
        .into_iter()
        .filter(|d| crate::fsx::is_dir(d))
        .collect();
    if others.is_empty() && !dirs.iter().any(|d| crate::fsx::is_dir(d) && !crate::fsx::is_link(d)) {
        return "not_installed";
    }
    super::scan_kind_files(
        collector,
        "claude",
        &dirs,
        &|name: &str| name.ends_with(".jsonl"),
        parse_line,
        Some(flush),
        &others,
    );
    "ok"
}
