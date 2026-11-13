//! The wire format: today's helper request and response (version 2), with `discovery`, `reader`,
//! `sessions` (identity evidence) and `messages` added. Rows, session rows and fingerprints are the
//! Python helper's, so the server's merge, cache and incremental rules are unchanged.

use crate::collector::Collector;
use crate::discover::{self, Env};
use crate::read;
use crate::util;
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const VERSION: u64 = 2;
pub const PROTOCOL: u64 = 3;
pub const NAME: &str = "aac-usage-scan";
pub const KINDS: [&str; 6] = ["claude", "codex", "omp", "muse", "zcode", "antigravity"];
const MAX_REQUEST_BYTES: usize = 8 * 1024 * 1024;
const EXTRA_ROOTS_MAX: usize = 16;
const EXTRA_ZCODE_DBS_MAX: usize = 2048;
const EXPERIMENT_ROOTS_MAX: usize = 256;

pub struct Request {
    pub kinds: Vec<String>,
    pub min_date_ms: i64,
    pub immutable: bool,
    pub fingerprints: Map<String, Value>,
    pub extra_roots: Map<String, Value>,
    /// Log folders an experiment harness kept (DESIGN 16.5): read through the copy rule, origin
    /// `experiment`. Unlike `extraRoots`, which are tool homes read as defaults.
    pub experiment_roots: Map<String, Value>,
    pub budget: f64,
    pub presence: bool,
    pub messages: bool,
    pub cwd: bool,
    /// Tests only, never read from a request: cut the scan after this many log lines, so a resumed
    /// read is tested at fixed points instead of wall-clock ones.
    pub line_budget: Option<u64>,
}

fn valid_extra_root(value: &Value) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    if text.is_empty() || text.chars().count() > 1024 || text.contains('\0') {
        return false;
    }
    if text.replace('\\', "/").split('/').any(|part| part == "..") {
        return false;
    }
    crate::fsx::absolute_text(text)
}

/// The request, validated exactly as the Python helper validates it (unknown fields refused).
pub fn parse_request(raw: &[u8]) -> Result<Request, String> {
    if raw.is_empty() || raw.len() > MAX_REQUEST_BYTES {
        return Err("request is missing or too large".into());
    }
    let value: Value = serde_json::from_slice(raw).map_err(|_| "request is not valid JSON".to_string())?;
    let request = value.as_object().ok_or("request must be an object")?;
    let allowed = [
        "kinds",
        "minDateMs",
        "immutableSqlite",
        "fingerprints",
        "extraRoots",
        "experimentRoots",
        "deadlineMs",
        "zcodeRecords",
        "presence",
        "messages",
        "cwd",
    ];
    if request.keys().any(|key| !allowed.contains(&key.as_str())) {
        return Err("request has unknown fields".into());
    }
    let kinds = request
        .get("kinds")
        .and_then(Value::as_array)
        .ok_or("request.kinds must list kinds")?;
    if kinds.is_empty() || kinds.iter().any(|k| !k.as_str().is_some_and(|k| KINDS.contains(&k))) {
        return Err("request.kinds must list claude, codex, omp, muse, zcode and/or antigravity".into());
    }
    let mut ordered: Vec<String> = Vec::new();
    for kind in kinds.iter().filter_map(Value::as_str) {
        if !ordered.iter().any(|k| k == kind) {
            ordered.push(kind.to_string());
        }
    }
    let min_date = match request.get("minDateMs") {
        Some(Value::Number(n)) if n.as_f64().is_some_and(|v| v >= 0.0) => n.as_f64().unwrap_or(0.0) as i64,
        _ => return Err("request.minDateMs must be a non-negative number".into()),
    };
    let flag = |name: &str, default: bool| -> Result<bool, String> {
        match request.get(name) {
            None => Ok(default),
            Some(Value::Bool(value)) => Ok(*value),
            _ => Err(format!("request.{name} must be a boolean")),
        }
    };
    let immutable = flag("immutableSqlite", true)?;
    if flag("zcodeRecords", false)? {
        // zcodeRecords is the dashboard computer's experiment mode, never sent to this scanner.
        return Err("request.zcodeRecords is not supported by this reader".into());
    }
    let fingerprints = match request.get("fingerprints") {
        None => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        _ => return Err("request.fingerprints must be an object".into()),
    };
    let extra_roots = match request.get("extraRoots") {
        None => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        _ => return Err("request.extraRoots must map kinds to absolute paths".into()),
    };
    for (kind, paths) in &extra_roots {
        let max = if kind == "zcode" {
            EXTRA_ZCODE_DBS_MAX
        } else {
            EXTRA_ROOTS_MAX
        };
        let ok = ["claude", "codex", "omp", "muse", "zcode"].contains(&kind.as_str())
            && paths
                .as_array()
                .is_some_and(|list| list.len() <= max && list.iter().all(valid_extra_root));
        if !ok {
            return Err("request.extraRoots must map kinds to absolute paths".into());
        }
    }
    let experiment_roots = match request.get("experimentRoots") {
        None => Map::new(),
        Some(Value::Object(map)) => map.clone(),
        _ => return Err("request.experimentRoots must map kinds to absolute paths".into()),
    };
    for (kind, paths) in &experiment_roots {
        let ok = ["claude", "codex", "omp", "muse"].contains(&kind.as_str())
            && paths.as_array().is_some_and(|list| {
                list.len() <= EXPERIMENT_ROOTS_MAX && list.iter().all(valid_extra_root)
            });
        if !ok {
            return Err("request.experimentRoots must map kinds to absolute paths".into());
        }
    }
    let budget = match request.get("deadlineMs") {
        None => 20.0,
        Some(Value::Number(n)) => match n.as_f64() {
            Some(v) if (1000.0..=120_000.0).contains(&v) => v / 1000.0,
            _ => return Err("request.deadlineMs must be within 1s and 120s".into()),
        },
        _ => return Err("request.deadlineMs must be within 1s and 120s".into()),
    };
    Ok(Request {
        kinds: ordered,
        min_date_ms: min_date,
        immutable,
        fingerprints,
        extra_roots,
        experiment_roots,
        budget,
        presence: flag("presence", false)?,
        messages: flag("messages", false)?,
        cwd: flag("cwd", true)?,
        line_budget: None,
    })
}


fn extras(request: &Request, kind: &str) -> Vec<PathBuf> {
    paths_of(&request.extra_roots, kind)
}

/// The experiment log folders of a kind: the request's (DESIGN 16.0 B) and, later, discovery's.
fn experiments(request: &Request, kind: &str) -> Vec<PathBuf> {
    paths_of(&request.experiment_roots, kind)
}

fn paths_of(map: &Map<String, Value>, kind: &str) -> Vec<PathBuf> {
    map.get(kind)
        .and_then(Value::as_array)
        .map(|list| list.iter().filter_map(Value::as_str).map(PathBuf::from).collect())
        .unwrap_or_default()
}

fn row_json(row: &crate::collector::Row) -> Value {
    let mut map = Map::new();
    map.insert("k".into(), json!(row.kind));
    map.insert("f".into(), json!(row.filekey));
    map.insert("m".into(), json!(row.model));
    if let Some(provider) = &row.provider {
        map.insert("p".into(), json!(provider));
    }
    map.insert("h".into(), json!(row.hour));
    map.insert("i".into(), json!(row.tokens[0]));
    map.insert("o".into(), json!(row.tokens[1]));
    map.insert("cr".into(), json!(row.tokens[2]));
    map.insert("cw".into(), json!(row.tokens[3]));
    map.insert("c".into(), json!(row.cost));
    map.insert("n".into(), json!(row.events));
    Value::Object(map)
}

fn srow_json(row: &crate::collector::SessionRow) -> Value {
    let mut map = Map::new();
    map.insert("k".into(), json!(row.kind));
    map.insert("f".into(), json!(row.filekey));
    map.insert("s".into(), json!(row.session));
    map.insert("m".into(), json!(row.model));
    if let Some(provider) = &row.provider {
        map.insert("p".into(), json!(provider));
    }
    map.insert("a".into(), json!(row.first));
    map.insert("z".into(), json!(row.last));
    map.insert("i".into(), json!(row.tokens[0]));
    map.insert("o".into(), json!(row.tokens[1]));
    map.insert("cr".into(), json!(row.tokens[2]));
    map.insert("cw".into(), json!(row.tokens[3]));
    map.insert("c".into(), json!(row.cost));
    map.insert("n".into(), json!(row.events));
    Value::Object(map)
}

/// This binary's identity, for the version handshake.
pub fn version_json() -> Value {
    json!({
        "name": NAME,
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": PROTOCOL,
        "target": target(),
    })
}

pub fn target() -> &'static str {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        "aarch64-apple-darwin"
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        "x86_64-apple-darwin"
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        "x86_64-pc-windows-msvc"
    } else {
        "unknown"
    }
}

/// One scan: every requested kind in order, then the response.
pub fn scan(request: &Request, home: &Path, env: &Env) -> Value {
    let mut collector = Collector::new(request.min_date_ms, request.budget, request.fingerprints.clone());
    collector.capture_cwd = request.cwd;
    collector.messages_wanted = request.messages;
    collector.lines_left = request.line_budget;
    let instances = discover::t3_instances(home);
    let mut states: Map<String, Value> = Map::new();
    let mut partials: Vec<String> = Vec::new();
    for kind in &request.kinds {
        collector.fresh.entry(kind.clone()).or_default();
        let cut_before = collector.truncated;
        let state = match kind.as_str() {
            "claude" => read::claude::collect(&mut collector, home, env, &extras(request, "claude"), &instances),
            "codex" => read::codex::collect(
                &mut collector,
                home,
                env,
                &extras(request, "codex"),
                &experiments(request, "codex"),
                &instances,
            ),
            "omp" => read::omp::collect(&mut collector, home, env, &extras(request, "omp"), &instances),
            "muse" => read::muse::collect(&mut collector, home, env, &extras(request, "muse")),
            "zcode" => read::zcode::collect(&mut collector, home, env, request.immutable, &extras(request, "zcode")),
            _ => read::antigravity::collect(&mut collector, home, env),
        };
        states.insert(kind.clone(), json!(state));
        if collector.usage_partial.contains(kind)
            || (collector.truncated && !cut_before)
            || collector.row_cap
            || collector.expired()
        {
            partials.push(kind.clone());
        }
        if collector.expired() {
            collector.truncated = true;
            break;
        }
    }
    if collector.truncated {
        collector.keep_confirmed();
    }
    let mut kinds = Map::new();
    for kind in &request.kinds {
        let prints: Map<String, Value> = collector
            .fresh
            .get(kind)
            .map(|m| m.iter().map(|(k, p)| (k.clone(), p.to_json())).collect())
            .unwrap_or_default();
        let mut entry = Map::new();
        entry.insert("state".into(), states.get(kind).cloned().unwrap_or(json!("pending")));
        entry.insert("fingerprints".into(), Value::Object(prints));
        if partials.contains(kind) {
            entry.insert("partial".into(), json!(true));
        }
        if let Some(failures) = collector.read_failures.get(kind).filter(|f| !f.is_empty()) {
            entry.insert("partial".into(), json!(true));
            entry.insert("readFailures".into(), json!(failures.len()));
        }
        if kind == "claude" && !collector.replaced_files.is_empty() {
            entry.insert(
                "replacedFiles".into(),
                json!(collector.replaced_files.iter().collect::<Vec<_>>()),
            );
        }
        if kind == "codex" && !collector.codex_compressed_unread.is_empty() {
            entry.insert("partial".into(), json!(true));
            entry.insert(
                "compressedUnread".into(),
                json!(collector.codex_compressed_unread.len()),
            );
        }
        if crate::collector::resumable(kind) {
            let (unfinished, count) = unfinished_logs(&collector, kind);
            if count > 0 {
                entry.insert("partial".into(), json!(true));
                entry.insert("unfinished".into(), Value::Array(unfinished));
                entry.insert("unfinishedCount".into(), json!(count));
            }
        }
        if kind == "zcode" && collector.wal_unread {
            entry.insert("walUnread".into(), json!(true));
        }
        if kind == "antigravity" && collector.agy_unreadable > 0 {
            entry.insert("unreadable".into(), json!(true));
        }
        kinds.insert(kind.clone(), Value::Object(entry));
    }
    let mut rows = collector.row_list.clone();
    rows.sort_by(|a, b| {
        (&a.hour, &a.kind, a.provider.as_deref().unwrap_or(""), &a.model).cmp(&(
            &b.hour,
            &b.kind,
            b.provider.as_deref().unwrap_or(""),
            &b.model,
        ))
    });
    let mut srows = collector.srow_list.clone();
    srows.sort_by(|a, b| (a.last, &a.kind, &a.session, &a.model).cmp(&(b.last, &b.kind, &b.session, &b.model)));
    let mut discovery_kinds = Map::new();
    for kind in &request.kinds {
        if states.contains_key(kind) {
            discovery_kinds.insert(
                kind.clone(),
                discover::discovery_kind(&collector, home, env, kind, &instances),
            );
        }
    }
    let mut discovery = json!({"kinds": discovery_kinds});
    // Presence runs after the readers and only while the budget lasts, so a slow disk cannot push an
    // answer past ssh's timeout; without it the server keeps the last presence it saw.
    if request.presence && !collector.expired() {
        discovery["tools"] = Value::Object(discover::presence(home, env, &instances, Duration::from_secs(3)));
    }
    // Identity evidence of every session this answer names: its full hash and, when the log states
    // one, its working folder with home as ~ (DESIGN 11.1a).
    let named: std::collections::HashSet<&str> = srows.iter().map(|r| r.session.as_str()).collect();
    let sessions: Map<String, Value> = collector
        .sessions
        .iter()
        .filter(|(key, _)| named.contains(key.as_str()))
        .map(|(key, evidence)| {
            let cwd = evidence.cwd.as_ref().map(|cwd| {
                let path = discover::tilde(Path::new(cwd), home);
                json!({
                    "key": util::sha256_hex(format!("aac-cwd-v1:{path}").as_bytes())[..16].to_string(),
                    "path": path,
                    // An inferred join only, never a measured attribution (history DESIGN 1.3,
                    // DATA-ARCHITECTURE 2.1).
                    "identitySource": "inferred_cwd",
                    "confidence": "inferred",
                })
            });
            (key.clone(), json!({"hash": evidence.hash, "cwd": cwd}))
        })
        .collect();
    let mut response = json!({
        "version": VERSION,
        "truncated": collector.truncated,
        "discoveryTruncated": collector.discovery_truncated,
        "kinds": kinds,
        "rows": rows.iter().map(row_json).collect::<Vec<_>>(),
        "srows": srows.iter().map(srow_json).collect::<Vec<_>>(),
        "discovery": discovery,
        "reader": version_json(),
        "sessions": sessions,
    });
    if request.messages {
        response["messages"] = Value::Array(
            collector
                .messages
                .iter()
                .map(|m| {
                    json!({
                        "k": m.kind, "f": m.filekey, "key": m.key, "at": m.at, "s": m.session,
                        "m": m.model, "p": m.provider, "i": m.tokens[0], "o": m.tokens[1],
                        "cr": m.tokens[2], "cw": m.tokens[3], "c": m.cost,
                    })
                })
                .collect(),
        );
        if collector.messages_cut {
            response["messagesCut"] = json!(true);
        }
    }
    response
}

/// The logs of a resumable kind not read to their end, named so the page can say which (DESIGN
/// 16.1): those this call left part-read, those an earlier call did that this one did not reach
/// (only while the kind was cut, else a file the scan did not find is gone), and those the decoder
/// could not read. At most 20, largest first; the count is of all of them.
pub fn unfinished_logs(collector: &Collector, kind: &str) -> (Vec<Value>, usize) {
    let mut found: Vec<(u8, u64, Value)> = Vec::new();
    let fresh = collector.fresh.get(kind);
    if let Some(prints) = fresh {
        for print in prints.values() {
            if let Some(progress) = &print.progress
                && let Some(through) = progress.through
            {
                found.push((
                    0,
                    print.size,
                    json!({"name": progress.name, "sizeBytes": print.size, "readThroughBytes": through,
                           "decodedBytes": progress.decoded, "reason": "deadline"}),
                ));
            }
        }
    }
    if collector.truncated
        && let Some(prior) = collector.prior_kind(kind)
    {
        for (key, print) in prior {
            if fresh.is_some_and(|m| m.contains_key(key)) {
                continue;
            }
            let Some(through) = print.get("readThrough").and_then(Value::as_u64) else {
                continue;
            };
            let size = print.get("size").and_then(Value::as_u64).unwrap_or(0);
            let name = print
                .get("readName")
                .and_then(Value::as_str)
                .map(|n| util::prefix_chars(n, 120));
            found.push((
                0,
                size,
                json!({"name": name, "sizeBytes": size, "readThroughBytes": through,
                       "decodedBytes": print.get("decodedSize").and_then(Value::as_u64), "reason": "deadline"}),
            ));
        }
    }
    let mut seen = std::collections::HashSet::new();
    for (path, size) in &collector.unreadable_logs {
        if seen.insert(path.clone()) {
            found.push((
                1,
                *size,
                json!({"name": util::display_name(path), "sizeBytes": size, "readThroughBytes": null,
                       "decodedBytes": null, "reason": "unreadable"}),
            ));
        }
    }
    let count = found.len();
    found.sort_by(|a, b| a.0.cmp(&b.0).then(b.1.cmp(&a.1)));
    (found.into_iter().take(20).map(|(_, _, v)| v).collect(), count)
}
