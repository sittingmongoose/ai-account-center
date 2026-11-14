//! Codex rollouts (`rollout-*.jsonl` and `.jsonl.zst` under `sessions` and `archived_sessions`):
//! cumulative `token_count` events differenced per file. The Python helper's `_parse_codex_line`.

use super::LineBox;
use crate::collector::{Collector, MAX_LINE_BYTES};
use crate::discover::{self, Env, T3Instance};
use crate::fsx;
use crate::util::{self, path_text};
use serde_json::Value;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};

/// The conversation id a rollout's file name ends with (`...-<uuid>.jsonl[.zst]`), lower case.
/// Checked on bytes, so a name with multi-byte characters is never cut inside one
/// (FIX-ROUND-BIG 8).
pub fn conversation_id(name: &str) -> Option<String> {
    let stem = name
        .strip_suffix(".jsonl.zst")
        .or_else(|| name.strip_suffix(".jsonl"))?
        .as_bytes();
    let tail = stem.get(stem.len().checked_sub(36)?..)?;
    let ok = tail.iter().enumerate().all(|(i, b)| match i {
        8 | 13 | 18 | 23 => *b == b'-',
        _ => b.is_ascii_hexdigit(),
    });
    // All ASCII when it passed, so the bytes are a valid string.
    ok.then(|| String::from_utf8_lossy(tail).to_lowercase())
}

/// A Codex log's key: its conversation (FIX-ROUND-BIG 1), so every copy of one conversation, in
/// any folder, and the log after Codex moves it to `archived_sessions`, keep the same rows.
pub fn file_key(path: &Path) -> String {
    util::filekey_text(
        "codex",
        &format!("aac-conversation-v1:{}", super::copy_key("codex", path)),
    )
}

/// The same key from a validated session id itself (RECHECK-BIG-1 M1): the identity is the
/// `session_meta` payload id, hashed here, never the file name.
pub fn file_key_for(session: &str) -> String {
    util::filekey_text("codex", &format!("aac-conversation-v1:{session}"))
}

/// What a bounded look at a log's head found: its session, no usable header, or no read at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionPeek {
    /// The validated `session_meta` payload id (UUID-shaped ids lower-cased, as file names spell
    /// them, so copies keep the key they had when names decided).
    Found(String),
    /// The head decoded but holds no valid session header.
    NoHeader,
    /// The file could not be opened or decoded far enough to tell.
    Unreadable,
}

fn validated_id(value: Option<&Value>) -> Option<String> {
    let id = util::clean_text(value, util::SESSION_MAX_LEN)?;
    // A UUID-shaped id is one conversation in any case; file names spell it lower-case.
    let bytes = id.as_bytes();
    let uuid = bytes.len() == 36
        && bytes.iter().enumerate().all(|(i, b)| match i {
            8 | 13 | 18 | 23 => *b == b'-',
            _ => b.is_ascii_hexdigit(),
        });
    Some(if uuid { id.to_lowercase() } else { id })
}

fn header_id<R: Read>(reader: R) -> SessionPeek {
    use std::io::BufRead;
    let mut reader = BufReader::with_capacity(1 << 16, reader);
    let mut spent: u64 = 0;
    for _ in 0..16 {
        if spent >= MAX_LINE_BYTES as u64 {
            break;
        }
        let mut line = Vec::new();
        let read = match reader
            .by_ref()
            .take(MAX_LINE_BYTES as u64 + 2 - spent.min(MAX_LINE_BYTES as u64))
            .read_until(b'\n', &mut line)
        {
            Ok(read) => read as u64,
            Err(_) => return SessionPeek::Unreadable,
        };
        if read == 0 {
            break;
        }
        spent += read;
        let Ok(text) = std::str::from_utf8(&line) else {
            continue;
        };
        if !text.contains("\"session_meta\"") {
            continue;
        }
        let Ok(record) = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}')) else {
            continue;
        };
        if record.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }
        return match record
            .get("payload")
            .filter(|p| p.is_object())
            .and_then(|p| validated_id(p.get("id")))
        {
            Some(id) => SessionPeek::Found(id),
            None => SessionPeek::NoHeader,
        };
    }
    SessionPeek::NoHeader
}

/// A log's session from its own header, without reading the log: at most 16 lines or one line's
/// worth of bytes, decoded the same way the read itself would be.
pub fn session_id_of(path: &Path) -> SessionPeek {
    let Ok(file) = fsx::open_regular(path) else {
        return SessionPeek::Unreadable;
    };
    if path_text(path).ends_with(".jsonl.zst") {
        let Ok(decoder) = zstd::stream::read::Decoder::new(file) else {
            return SessionPeek::Unreadable;
        };
        header_id(decoder)
    } else {
        header_id(file)
    }
}

fn number(value: Option<&Value>) -> f64 {
    match value {
        Some(Value::Number(n)) => match n.as_f64() {
            Some(v) if v.is_finite() && v > 0.0 => v,
            _ => 0.0,
        },
        _ => 0.0,
    }
}

/// Codex's TokenUsage: cached reads and cache writes are details of input_tokens.
/// Returns (uncached input, cache write, cache read, output).
fn snapshot(value: Option<&Value>) -> Option<[f64; 4]> {
    let value = value?.as_object()?;
    let cache_read = number(value.get("cached_input_tokens"));
    let cache_write = number(value.get("cache_write_input_tokens"));
    Some([
        (number(value.get("input_tokens")) - cache_read - cache_write).max(0.0),
        cache_write,
        cache_read,
        number(value.get("output_tokens")),
    ])
}

pub fn parse_line(line: &str, collector: &mut Collector, kind: &str, filekey: &str, line_box: &mut LineBox) {
    // Pre-filter before parsing, so prompts and tool output are never decoded.
    if !line.contains("\"type\"") {
        return;
    }
    let meta = line.contains("\"session_meta\"");
    let turn = line.contains("\"turn_context\"");
    if !meta && !turn && !line.contains("\"event_msg\"") {
        return;
    }
    if !line.contains("\"token_count\"") && !meta && !turn {
        return;
    }
    let Ok(record) = serde_json::from_str::<Value>(line) else {
        return;
    };
    let payload = record.get("payload").filter(|p| p.is_object());
    let kind_of = record.get("type").and_then(Value::as_str);
    if kind_of == Some("session_meta")
        && let Some(payload) = payload
    {
        if let Some(session) = payload
            .get("id")
            .and_then(Value::as_str)
            .filter(|s| !util::py_strip(s).is_empty())
        {
            line_box.session = Some(session.to_string());
        }
        if let Some(provider) = payload
            .get("model_provider")
            .and_then(Value::as_str)
            .filter(|s| !util::py_strip(s).is_empty())
        {
            line_box.provider = Some(provider.to_string());
        }
        if let Some(cwd) = payload.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            line_box.cwd = Some(cwd.to_string());
        }
        return;
    }
    if kind_of == Some("turn_context")
        && let Some(payload) = payload
    {
        if let Some(model) = payload
            .get("model")
            .and_then(Value::as_str)
            .filter(|s| !util::py_strip(s).is_empty())
        {
            line_box.model = Some(model.to_string());
        }
        if line_box.cwd.is_none()
            && let Some(cwd) = payload.get("cwd").and_then(Value::as_str).filter(|s| !s.is_empty())
        {
            line_box.cwd = Some(cwd.to_string());
        }
        return;
    }
    let Some(payload) = payload else {
        return;
    };
    if kind_of != Some("event_msg")
        || payload.get("type").and_then(Value::as_str) != Some("token_count")
        || line_box.session.is_none()
        || matches!(line_box.provider.as_deref(), Some("cliproxy") | Some("ccs_runtime"))
    {
        return;
    }
    let Some(info) = payload.get("info").filter(|i| i.is_object()) else {
        return;
    };
    let Some(total) = snapshot(info.get("total_token_usage")) else {
        return;
    };
    let last = snapshot(info.get("last_token_usage"));
    if line_box.previous == Some(total) {
        return;
    }
    let delta = match line_box.previous {
        None => match last {
            Some(last) if last.iter().any(|v| *v > 0.0) => last,
            _ => total,
        },
        Some(previous) => {
            let mut delta = [0.0; 4];
            for i in 0..4 {
                delta[i] = (total[i] - previous[i]).max(0.0);
            }
            delta
        }
    };
    line_box.previous = Some(total);
    if !delta.iter().any(|v| *v > 0.0) {
        return;
    }
    let Some(timestamp) = record
        .get("timestamp")
        .and_then(Value::as_str)
        .filter(|t| !util::py_strip(t).is_empty())
    else {
        return;
    };
    let Some(epoch_ms) = util::iso_ms(timestamp) else {
        return;
    };
    if epoch_ms < collector.min_date_ms {
        return;
    }
    let Some(hour) = util::hour_label(epoch_ms, collector.now_ms) else {
        return;
    };
    let raw_model = line_box
        .model
        .clone()
        .unwrap_or_else(|| "unknown-codex-model".to_string());
    let Some(model) = util::clean_model(Some(&Value::String(raw_model))) else {
        return;
    };
    let tokens = [
        util::int_of(delta[0]),
        util::int_of(delta[3]),
        util::int_of(delta[2]),
        util::int_of(delta[1]),
    ];
    let session = line_box.session.clone().unwrap_or_default();
    // Python: "c:%s|%s|%s|%s" % (session, timestamp, model, tokens) with tokens a list repr.
    let record_key = format!(
        "c:{session}|{timestamp}|{model}|[{}, {}, {}, {}]",
        tokens[0], tokens[1], tokens[2], tokens[3]
    );
    if !collector.keep_record(Some(record_key.clone())) {
        return;
    }
    collector.add(kind, filekey, &model, None, &hour, tokens, 0.0);
    let session_key = util::session_key(kind, Some(&session));
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
    collector.note_session(kind, &session, line_box.cwd.clone());
    if line_box.offset >= line_box.message_from {
        let key = util::sha256_hex(format!("aac-msg-v1:{kind}:{record_key}").as_bytes())[..16].to_string();
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

/// Python's `_collect_codex`: "not_installed" when no root exists, else "ok".
pub fn collect(
    collector: &mut Collector,
    home: &Path,
    env: &Env,
    extra: &[PathBuf],
    experiments: &[PathBuf],
    instances: &[T3Instance],
) -> &'static str {
    let mut plan = discover::codex_roots(home, env, extra, instances);
    // Home has more children than the signature search looks at: named, and the kind is partial
    // (FIX-ROUND-BIG 6).
    if let Some((read, total)) = plan.limit {
        collector.search_limit("codex", read, Some(total));
    }
    // An account base that could not be listed: named, and the kind is partial (RECHECK-BIG-1 M3).
    for base in &plan.failed_bases {
        collector.unreadable_folder("codex", base);
    }
    for (path, cause) in &plan.search_failures {
        collector.note_root("codex", path, "signature");
        collector.root_failure("codex", path, "unreadable", cause);
    }
    // Experiment log folders are read through the copy rule, like other homes' logs (DESIGN 16.5).
    plan.others
        .extend(experiments.iter().map(|p| (p.clone(), "experiment")));
    for (path, origin) in plan.defaults.iter().chain(plan.others.iter()) {
        collector.note_root("codex", path, origin);
    }
    // Every folder is probed first (DESIGN 16.4): a link onto a mount that hangs is reported, never
    // stat'ed again; one that answers is read through its real path.
    let defaults: Vec<PathBuf> = plan.defaults.iter().map(|(p, _)| p.clone()).collect();
    let dirs = crate::fsx::dedup_dirs(&collector.reachable_dirs("codex", &defaults));
    let candidates: Vec<PathBuf> = plan.others.iter().map(|(p, _)| p.clone()).collect();
    let others: Vec<PathBuf> = crate::fsx::dedup_dirs(&collector.reachable_dirs("codex", &candidates));
    if others.is_empty() && dirs.is_empty() {
        return "not_installed";
    }
    let accept =
        |name: &str| name.starts_with("rollout-") && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"));
    super::scan_kind_files(collector, "codex", &dirs, &accept, parse_line, None, &others);
    "ok"
}
