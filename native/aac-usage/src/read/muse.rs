//! Muse Code sessions (`sessions/**/session.jsonl`): `model_completed` events only (the goal usage
//! attribution repeats the same numbers). The Python helper's `_scan_muse_dir` and `_parse_muse_line`.

use super::{LineBox, Outcome, read_file_with};
use crate::collector::Collector;
use crate::discover::Env;
use crate::fsx;
use crate::util;
use serde_json::Value;
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

/// One sessions folder; true when the scan stopped early.
fn scan_dir(collector: &mut Collector, directory: &Path) -> bool {
    let listed = fsx::walk_files(
        directory,
        &|name: &str| name == "session.jsonl",
        collector,
        Some(&holds_no_logs),
    );
    collector.count_root("muse", &util::realpath(directory), &listed);
    for path in listed {
        if collector.expired() {
            collector.truncated = true;
            return true;
        }
        let Some(filekey) = collector.note_file("muse", &path, None) else {
            let staged = util::filekey("muse", &path);
            if collector.is_pending("muse", &staged) {
                collector.confirm_file("muse", &staged);
            }
            continue;
        };
        let session = path
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| util::prefix_chars(&n.to_string_lossy(), util::SESSION_MAX_LEN));
        let line_box = LineBox {
            session,
            ..LineBox::default()
        };
        match read_file_with(collector, "muse", &path, &filekey, parse_line, None, line_box) {
            Outcome::Cut => return true,
            Outcome::Unreadable => continue,
            Outcome::Ok => {
                collector.sent_through("muse", &filekey, &path);
                collector.confirm_file("muse", &filekey);
            }
        }
    }
    false
}

/// Python's `_collect_muse`.
pub fn collect(collector: &mut Collector, home: &Path, env: &Env, extra: &[PathBuf]) -> &'static str {
    let default = home.join(".local").join("share").join("muse").join("sessions");
    let sessions = crate::discover::env_dir(env, "MUSE_SESSIONS_DIR", home);
    let origin = if sessions.is_some() { "env" } else { "default" };
    let sessions = sessions.unwrap_or(default);
    collector.note_root("muse", &sessions, origin);
    for path in extra {
        collector.note_root("muse", path, "extra");
    }
    let mut candidates = vec![sessions];
    candidates.extend(extra.iter().cloned());
    let mut scanned = false;
    for directory in fsx::dedup_dirs(&candidates) {
        if !fsx::is_dir(&directory) || fsx::is_link(&directory) {
            continue;
        }
        scanned = true;
        if scan_dir(collector, &directory) {
            return "ok";
        }
    }
    if scanned { "ok" } else { "not_installed" }
}
