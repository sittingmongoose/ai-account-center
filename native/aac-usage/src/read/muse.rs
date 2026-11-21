//! Muse Code sessions (`sessions/**/session.jsonl`): `model_completed` events only (the goal usage
//! attribution repeats the same numbers). The Python helper's `_scan_muse_dir` and `_parse_muse_line`.

use super::{LineBox, Outcome, read_file_with};
use crate::collector::Collector;
use crate::discover::Env;
use crate::fsx;
use crate::util::{self, path_text};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

fn holds_no_logs(name: &str) -> bool {
    name == "tool-outputs" || name.starts_with('.')
}

fn normalize_recorded_at(value: Option<&Value>) -> Option<i64> {
    let number = match value? {
        Value::Number(n) => n.as_f64()?,
        _ => return None,
    };
    let epoch = number as i64;
    if epoch <= 0 {
        return None;
    }
    let mut seconds = epoch;
    while seconds > 4_102_444_800 {
        seconds /= 1000;
    }
    if seconds <= 0 { None } else { Some(seconds * 1000) }
}

fn event_of(record: &Value) -> Option<(Option<&Value>, &Value)> {
    let event = record.get("payload")?.as_object()?.get("event")?;
    if event.get("kind").and_then(Value::as_str) == Some("model_completed") {
        Some((record.get("recorded_at"), event))
    } else {
        None
    }
}

pub fn parse_line(line: &str, collector: &mut Collector, kind: &str, filekey: &str, line_box: &mut LineBox) {
    if !line.contains("model_completed") {
        return;
    }
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return;
    };
    if !record.is_object() {
        return;
    }
    let mut found: Option<(Option<Value>, Value)> = event_of(&record).map(|(a, e)| (a.cloned(), e.clone()));
    if found.is_none()
        && let Some(children) = record.get("children").and_then(Value::as_array)
    {
        for child in children {
            let Some(child) = child.as_object() else {
                continue;
            };
            let embedded = match child.get("record_json") {
                Some(Value::String(text)) => match serde_json::from_str::<Value>(text) {
                    Ok(value) => value,
                    Err(_) => continue,
                },
                Some(other) => other.clone(),
                None => continue,
            };
            // One level only; deeper nesting is not a usage record.
            if let Some((_, event)) = event_of(&embedded) {
                found = Some((record.get("recorded_at").cloned(), event.clone()));
                break;
            }
        }
    }
    let Some((recorded_at, event)) = found else {
        return;
    };
    let Some(epoch_ms) = normalize_recorded_at(recorded_at.as_ref()) else {
        return;
    };
    if epoch_ms < collector.min_date_ms {
        return;
    }
    let Some(model) = util::clean_model(event.get("model")) else {
        return;
    };
    let Some(usage) = event.get("usage").filter(|u| u.is_object()) else {
        return;
    };
    let values = [
        util::non_negative(usage.get("input_tokens")),
        util::non_negative(usage.get("output_tokens")),
        util::non_negative(usage.get("cache_read_tokens")),
        util::non_negative(usage.get("cache_write_tokens")),
    ];
    if values.iter().any(Option::is_none) {
        return;
    }
    let v = values.map(|v| util::int_of(v.unwrap_or(0.0)));
    // input_tokens includes the cache reads (as zcode's does): net them out.
    let tokens = [v[0].saturating_sub(v[2]), v[1], v[2], v[3]];
    let Some(hour) = util::hour_label(epoch_ms, collector.now_ms) else {
        return;
    };
    collector.add(kind, filekey, &model, None, &hour, tokens, 0.0);
    let session = line_box.session.clone();
    let session_key = util::session_key(kind, session.as_deref());
    collector.sadd(
        kind,
        filekey,
        session_key.as_deref(),
        &model,
        None,
        epoch_ms,
        tokens,
        0.0,
    );
    if let Some(raw) = session.as_deref() {
        collector.note_session(kind, raw, None);
    }
    if line_box.offset >= line_box.message_from {
        let unit = format!("@{filekey}:{}", line_box.offset);
        let key = util::sha256_hex(format!("aac-msg-v1:{kind}:{unit}").as_bytes())[..16].to_string();
        collector.message(
            kind,
            filekey,
            key,
            epoch_ms,
            session_key.as_deref(),
            &model,
            None,
            tokens,
            0.0,
        );
    }
}

/// A Muse log's key: its session (the folder `session.jsonl` sits in), so every copy of a session
/// in any folder keeps the same rows (FIX-ROUND-BIG 2).
fn session_of(path: &Path) -> String {
    path.parent()
        .and_then(|p| p.file_name())
        .map(|n| util::prefix_chars(&n.to_string_lossy(), util::SESSION_MAX_LEN))
        .unwrap_or_default()
}

fn file_key(path: &Path) -> String {
    util::filekey_text("muse", &format!("aac-session-file-v1:{}", session_of(path)))
}

/// What trying one copy of a session did (RECHECK-BIG-1 M4: a failed copy is not the end).
enum Attempt {
    /// Read whole, or confirmed unchanged: this copy owns the session.
    Done,
    /// The scan stopped early.
    Cut,
    /// This copy could not be read; try the next one.
    Failed,
}

/// Try one copy of a session under the session's key.
fn read_owner(collector: &mut Collector, path: &Path, key: &str) -> Attempt {
    // A copy that cannot even be printed is rejected by name; the next copy is tried.
    if util::fingerprint(path).is_none() {
        if !super::gone(path) {
            collector.read_failure("muse", key.to_string());
        }
        return Attempt::Failed;
    }
    let Some(filekey) = collector.note_file_as("muse", path, key.to_string(), None) else {
        if collector.is_pending("muse", key) {
            collector.confirm_file("muse", key);
            return Attempt::Done;
        }
        // It printed above, so only the file cap refuses it here.
        collector.truncated = true;
        return Attempt::Cut;
    };
    let line_box = LineBox {
        session: Some(session_of(path)).filter(|s| !s.is_empty()),
        ..LineBox::default()
    };
    match read_file_with(collector, "muse", path, &filekey, parse_line, None, line_box) {
        Outcome::Cut => Attempt::Cut,
        Outcome::Unreadable => {
            // Named as failed (FIX-ROUND-BIG 3); a prefix it may have emitted is not kept.
            collector.read_failure("muse", filekey.clone());
            collector.drop_file_rows(&HashSet::from([filekey]));
            Attempt::Failed
        }
        Outcome::Ok => {
            collector.sent_through("muse", &filekey, path);
            collector.confirm_file("muse", &filekey);
            Attempt::Done
        }
    }
}

/// A file's whole content digest, read in blocks; None when it cannot be read.
fn content_digest(path: &Path) -> Option<String> {
    let mut file = fsx::open_regular(path).ok()?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 1 << 16];
    loop {
        match file.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => digest.update(&buffer[..read]),
            Err(_) => return None,
        }
    }
    let mut text = String::with_capacity(64);
    for byte in digest.finalize().iter() {
        text.push_str(&format!("{byte:02x}"));
    }
    Some(text)
}

/// Sessions with a settled copy (FINAL-NEON M1): while that copy still ranks first among its
/// session's copies it stays settled and none of them is read; otherwise it is read again with
/// the others. Returns the paths still to read.
fn settle_sessions(collector: &mut Collector, found: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut by_session: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for path in &found {
        by_session.entry(session_of(path)).or_default().push(path.clone());
    }
    let mut settled_sessions: HashSet<String> = HashSet::new();
    for (session, members) in by_session {
        let Some(settled) = members
            .iter()
            .find(|p| collector.settled_key("muse", p).is_some())
            .cloned()
        else {
            continue;
        };
        let leads = members.len() == 1
            || super::rank_candidates(members, &|p| std::fs::metadata(p).ok().map(|m| m.len())).first()
                == Some(&settled);
        if leads {
            settled_sessions.insert(session);
        } else {
            collector.unsettle("muse", &settled);
        }
    }
    found
        .into_iter()
        .filter(|path| !settled_sessions.contains(&session_of(path)))
        .collect()
}

/// Every copy once, before any owner is elected (RECHECK-BIG-1 M2): first by the file itself
/// (device and inode, so a hard link under any folder name is the same file), then by content
/// for plain copies. Keep a published owner while it is present; choose by path on a cold read.
///
/// Settled copies take part too (RECHECK-FINAL-ASTRA M1): their identity and size come from their
/// prints, and their content is hashed only when an unsettled copy has the same size.
fn dedupe_copies(collector: &mut Collector, mut found: Vec<PathBuf>) -> Vec<PathBuf> {
    let settled: HashSet<PathBuf> = found
        .iter()
        .filter(|p| collector.settled_key("muse", p).is_some())
        .cloned()
        .collect();
    found.sort_by_cached_key(|path| {
        let key = util::filekey("muse", path);
        let published = collector
            .prior
            .get("muse")
            .and_then(|prints| prints.get(&key))
            .is_some_and(|prior| prior.is_object() && prior["readThrough"].is_null());
        (!settled.contains(path), !published, path_text(path))
    });
    let mut identities: HashSet<(u64, u64)> = HashSet::new();
    let mut unique: Vec<PathBuf> = Vec::new();
    for path in found {
        match collector.known_identity("muse", &path) {
            Some(id) if !identities.insert(id) => {
                collector.unsettle("muse", &path);
                continue;
            }
            _ => unique.push(path),
        }
    }
    // Content decides only where sizes already agree; files stat cannot size stay as they are.
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    let mut order: Vec<Option<u64>> = Vec::new();
    let mut no_size: Vec<PathBuf> = Vec::new();
    for path in unique {
        let size = match collector
            .settled_print_of("muse", &path)
            .and_then(|print| print["size"].as_u64())
        {
            Some(size) => Ok(size),
            None => std::fs::metadata(&path).map(|meta| meta.len()),
        };
        match size {
            Ok(size) => {
                if !by_size.contains_key(&size) {
                    order.push(Some(size));
                }
                by_size.entry(size).or_default().push(path);
            }
            Err(_) => {
                order.push(None);
                no_size.push(path);
            }
        }
    }
    let mut kept: Vec<PathBuf> = Vec::new();
    let mut no_size = no_size.into_iter();
    for slot in order {
        match slot {
            None => kept.extend(no_size.next()),
            Some(size) => {
                let group = by_size.remove(&size).unwrap_or_default();
                // Settled copies were told apart when they were read; only a new copy is compared.
                if group.len() == 1 || group.iter().all(|p| settled.contains(p)) {
                    kept.extend(group);
                    continue;
                }
                let mut digests: HashSet<String> = HashSet::new();
                for path in group {
                    if collector.expired() {
                        collector.truncated = true;
                        kept.push(path);
                        continue;
                    }
                    match content_digest(&path) {
                        Some(digest) if !digests.insert(digest.clone()) => {
                            collector.unsettle("muse", &path);
                            continue;
                        }
                        _ => kept.push(path),
                    }
                }
            }
        }
    }
    kept
}

/// Python's `_collect_muse`, with one rule for copies (FIX-ROUND-BIG 2): every `session.jsonl`
/// under every root is found first, and of each session only its longest copy is read (a retained
/// hard link, an experiment copy or a backup never adds to it).
pub fn collect(
    collector: &mut Collector,
    home: &Path,
    env: &Env,
    extra: &[PathBuf],
    experiments: &[PathBuf],
) -> &'static str {
    let default = home.join(".local").join("share").join("muse").join("sessions");
    let sessions = crate::discover::env_dir(env, "MUSE_SESSIONS_DIR", home);
    let origin = if sessions.is_some() { "env" } else { "default" };
    let sessions = sessions.unwrap_or(default);
    collector.note_root("muse", &sessions, origin);
    for path in extra {
        collector.note_root("muse", path, "extra");
    }
    // Experiment session folders (DESIGN 16.5): each holds `<id>/session.jsonl` like the default.
    for path in experiments {
        collector.note_root("muse", path, "experiment");
    }
    let mut candidates = vec![sessions];
    candidates.extend(extra.iter().cloned());
    candidates.extend(experiments.iter().cloned());
    // Probed first, before any other touch (FIX-ROUND-BIG 9): a folder (or a link) that is there
    // but cannot be read, or does not answer, is named; one that answers is read by its real path.
    let directories = fsx::dedup_dirs(&collector.reachable_dirs("muse", &candidates));
    if directories.is_empty() {
        return "not_installed";
    }
    let mut found = Vec::new();
    for directory in &directories {
        let listed = fsx::walk_files(
            "muse",
            directory,
            &|name: &str| name == "session.jsonl",
            collector,
            Some(&holds_no_logs),
        );
        collector.count_root("muse", &util::realpath(directory), &listed);
        found.extend(listed);
        if collector.expired() {
            collector.truncated = true;
            return "ok";
        }
    }
    // Copies of one file count once whatever folder names them (M2), settled copies included,
    // then each session's owner is its longest copy that actually reads (M4): candidates are
    // tried best first, and a session whose settled copy still leads is not read.
    let candidates = dedupe_copies(collector, found);
    if collector.expired() {
        collector.truncated = true;
        return "ok";
    }
    let candidates = settle_sessions(collector, candidates);
    for group in super::ranked_groups(&candidates, &|p| session_of(p), &|p| {
        std::fs::metadata(p).ok().map(|m| m.len())
    }) {
        let Some(first) = group.first() else {
            continue;
        };
        let key = file_key(first);
        for path in &group {
            if collector.expired() {
                collector.truncated = true;
                return "ok";
            }
            match read_owner(collector, path, &key) {
                Attempt::Done => break,
                Attempt::Cut => return "ok",
                Attempt::Failed => continue,
            }
        }
    }
    "ok"
}
