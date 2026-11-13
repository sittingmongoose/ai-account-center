//! Where each tool keeps its logs on this computer, from what is configured here and never from a
//! per-computer list (DESIGN section 2): the tool's documented default for this OS, its env var, its
//! own config, T3 Code's provider instances, AAC's own profile homes, folders under home that carry
//! the tool's home signature (any name), and the extra roots saved in Settings. Each root has one
//! origin; its path never leaves this computer (`root_id` is a digest with home written as `~`).

use crate::collector::Collector;
use crate::fsx::{self, child_dirs, home_path, is_dir, is_link, link_inside, skipped, sorted_names};
use crate::util::{self, path_text, realpath};
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub const SIGNATURE_MAX_CHILDREN: usize = 512;
const T3_SETTINGS_MAX_BYTES: u64 = 1024 * 1024;
const CODEX_HOME_MARKERS: [&str; 2] = ["config.toml", "auth.json"];
/// Claude Code's own files beside `projects` (DESIGN 16.6). `settings.json`, `session-env`,
/// `todos` and `statsig` matched Qoder (`~/.qoder`) and Qwen Code (`~/.qwen`) too; each Claude home
/// on the five computers has one of these four.
const CLAUDE_DIR_MARKERS: [&str; 4] = [".credentials.json", ".claude.json", "shell-snapshots", "file-history"];
const PRESENCE_MAX_ENTRIES: usize = 20_000;
const PRESENCE_MAX_DEPTH: usize = 8;
pub const AGY_DIRS: [&str; 4] = [
    "antigravity",
    "antigravity-cli",
    "antigravity-ide",
    "antigravity-backup",
];

/// The environment the scan runs with (tests pass their own).
pub type Env = HashMap<String, String>;

/// One T3 Code provider instance: id, driver, enabled, config.
#[derive(Clone, Debug)]
pub struct T3Instance {
    pub id: String,
    pub driver: String,
    pub enabled: bool,
    pub config: Map<String, Value>,
}

/// T3 Code's provider instances on this computer, at most 64. An unreadable settings file means
/// none and never fails the scan. Only `driver`, `enabled` and `config` are kept.
pub fn t3_instances(home: &Path) -> Vec<T3Instance> {
    let settings = home.join(".t3").join("userdata").join("settings.json");
    let Ok(meta) = fs::metadata(&settings) else {
        return Vec::new();
    };
    if meta.len() > T3_SETTINGS_MAX_BYTES {
        return Vec::new();
    }
    let Ok(text) = fs::read(&settings) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<Value>(&text) else {
        return Vec::new();
    };
    let Some(instances) = value.get("providerInstances").and_then(Value::as_object) else {
        return Vec::new();
    };
    instances
        .iter()
        .take(fsx::T3_MAX_HOMES)
        .filter_map(|(id, instance)| {
            let instance = instance.as_object()?;
            Some(T3Instance {
                id: id.clone(),
                driver: instance.get("driver").and_then(Value::as_str).unwrap_or("").to_string(),
                enabled: instance.get("enabled") != Some(&Value::Bool(false)),
                config: instance
                    .get("config")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default(),
            })
        })
        .collect()
}

/// An env var naming an absolute folder, `~` expanded.
pub fn env_dir(env: &Env, name: &str, home: &Path) -> Option<PathBuf> {
    let value = util::py_strip(env.get(name)?);
    if value.is_empty() {
        return None;
    }
    let expanded = if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/").or_else(|| value.strip_prefix("~\\")) {
        home.join(rest)
    } else {
        PathBuf::from(value)
    };
    if fsx::absolute_text(&path_text(&expanded)) {
        Some(expanded)
    } else {
        None
    }
}

fn has_state_db(path: &Path) -> bool {
    sorted_names(path).is_some_and(|names| names.iter().any(|n| n.starts_with("state_") && n.ends_with(".sqlite")))
}

/// Direct children of home with a Codex home's signature (`sessions` or `archived_sessions`, with
/// `config.toml`, `auth.json` or a `state_*.sqlite`) or a Claude config dir's (`projects`, with one of
/// CLAUDE_DIR_MARKERS), whatever their name. A link counts only when its target stays inside home.
pub fn signature_homes(home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let (mut codex, mut claude) = (Vec::new(), Vec::new());
    let Some(names) = sorted_names(home) else {
        return (codex, claude);
    };
    for name in names.into_iter().take(SIGNATURE_MAX_CHILDREN) {
        if skipped(&name) || name == "PM-Experiments" {
            continue;
        }
        let path = home.join(&name);
        if is_link(&path) && link_inside(&path, home).is_none() {
            continue;
        }
        if !is_dir(&path) {
            continue;
        }
        let has = |entry: &str| fsx::exists(&path.join(entry));
        if (has("sessions") || has("archived_sessions"))
            && (CODEX_HOME_MARKERS.iter().any(|m| has(m)) || has_state_db(&path))
        {
            codex.push(path.clone());
        }
        if has("projects") && CLAUDE_DIR_MARKERS.iter().any(|m| has(m)) {
            claude.push(path);
        }
    }
    (codex, claude)
}

/// The log folders of tool homes: Claude's `projects`; Codex's `sessions` and `archived_sessions`
/// (Codex moves a thread's rollout there when the thread is archived).
pub fn log_dirs(kind: &str, homes: &[PathBuf]) -> Vec<PathBuf> {
    let subs: &[&str] = if kind == "claude" {
        &["projects"]
    } else {
        &["sessions", "archived_sessions"]
    };
    homes
        .iter()
        .flat_map(|home| subs.iter().map(move |sub| home.join(sub)))
        .collect()
}

/// T3 Code's homes of kind: account bases, managed Codex shadows, and settings `homePath` (and
/// `shadowHomePath` for Codex) of every instance of the driver, enabled or not (a disabled
/// instance's home still holds the logs it wrote).
pub fn t3_root_origins(home: &Path, kind: &str, instances: &[T3Instance]) -> Vec<(PathBuf, &'static str)> {
    let mut homes = Vec::new();
    if kind == "claude" {
        homes.extend(child_dirs(&home.join(".claude-t3")));
    } else {
        homes.extend(child_dirs(&home.join(".codex-t3")));
        let managed = home.join(".t3").join("userdata").join("providers").join("codex");
        homes.extend(child_dirs(&managed).into_iter().map(|dir| dir.join("shadow")));
    }
    let (driver, keys): (&str, &[&str]) = if kind == "claude" {
        ("claudeAgent", &["homePath"])
    } else {
        ("codex", &["homePath", "shadowHomePath"])
    };
    for instance in instances.iter().filter(|i| i.driver == driver) {
        for key in keys {
            if let Some(path) = home_path(instance.config.get(*key), home) {
                homes.push(path);
            }
        }
    }
    log_dirs(kind, &homes).into_iter().map(|p| (p, "t3_instance")).collect()
}

pub struct RootPlan {
    /// Read as they are (the tool's own defaults, env, extras).
    pub defaults: Vec<(PathBuf, &'static str)>,
    /// Read through the copy rule: a record a default log already holds counts once.
    pub others: Vec<(PathBuf, &'static str)>,
}

/// Claude Code: `$CLAUDE_CONFIG_DIR/projects`, `~/.claude/projects`, `$XDG_CONFIG_HOME/claude/projects`,
/// extras; then T3 homes, AAC instances and signature homes through the copy rule.
pub fn claude_roots(home: &Path, env: &Env, extra: &[PathBuf], instances: &[T3Instance]) -> RootPlan {
    let mut defaults = Vec::new();
    if let Some(dir) = env_dir(env, "CLAUDE_CONFIG_DIR", home) {
        defaults.push((dir.join("projects"), "env"));
    }
    defaults.push((home.join(".claude").join("projects"), "default"));
    let config = env_dir(env, "XDG_CONFIG_HOME", home).unwrap_or_else(|| home.join(".config"));
    defaults.push((config.join("claude").join("projects"), "default"));
    defaults.extend(extra.iter().map(|p| (p.clone(), "extra")));
    let mut others = t3_root_origins(home, "claude", instances);
    others.extend(
        log_dirs("claude", &child_dirs(&home.join(".ccs").join("instances")))
            .into_iter()
            .map(|p| (p, "ccs_instance")),
    );
    others.extend(
        log_dirs("claude", &signature_homes(home).1)
            .into_iter()
            .map(|p| (p, "signature")),
    );
    RootPlan { defaults, others }
}

/// Codex: `sessions` and `archived_sessions` of `$CODEX_HOME`, `~/.codex` and each extra home; then
/// T3 homes, AAC's Codex profile homes and signature homes through the copy rule.
pub fn codex_roots(home: &Path, env: &Env, extra: &[PathBuf], instances: &[T3Instance]) -> RootPlan {
    let mut homes: Vec<(PathBuf, &'static str)> = Vec::new();
    if let Some(dir) = env_dir(env, "CODEX_HOME", home) {
        homes.push((dir, "env"));
    }
    homes.push((home.join(".codex"), "default"));
    homes.extend(extra.iter().map(|p| (p.clone(), "extra")));
    let defaults = homes
        .iter()
        .flat_map(|(base, origin)| {
            log_dirs("codex", std::slice::from_ref(base))
                .into_iter()
                .map(move |p| (p, *origin))
        })
        .collect();
    let mut others = t3_root_origins(home, "codex", instances);
    others.extend(
        log_dirs("codex", &child_dirs(&home.join(".ccs").join("codex-instances")))
            .into_iter()
            .map(|p| (p, "ccs_instance")),
    );
    others.extend(
        log_dirs("codex", &signature_homes(home).0)
            .into_iter()
            .map(|p| (p, "signature")),
    );
    RootPlan { defaults, others }
}

/// A T3 instance's launch arguments as a bounded list.
pub fn instance_args(config: &Map<String, Value>) -> Vec<String> {
    match config.get("commandArgs").or_else(|| config.get("launchArgs")) {
        Some(Value::String(text)) => text.split_whitespace().take(64).map(str::to_string).collect(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(Value::as_str)
            .take(64)
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

fn arg_values(args: &[String], flag: &str) -> Vec<String> {
    let mut values = Vec::new();
    for (index, arg) in args.iter().enumerate() {
        if arg == flag {
            if let Some(next) = args.get(index + 1) {
                values.push(next.clone());
            }
        } else if let Some(rest) = arg.strip_prefix(&format!("{flag}=")) {
            values.push(rest.to_string());
        }
    }
    values
}

/// True when a T3 instance's command or arguments name the tool (word-bounded, any case).
pub fn names_tool(config: &Map<String, Value>, word: &str) -> bool {
    let bounded = |text: &str| {
        let lower = text.to_lowercase();
        let bytes = lower.as_bytes();
        let sep = |b: u8| matches!(b, b'\\' | b'/' | b'.' | b'_' | b' ' | b'-');
        let mut start = 0;
        while let Some(found) = lower[start..].find(word) {
            let at = start + found;
            let before = at == 0 || sep(bytes[at - 1]);
            let mut end = at + word.len();
            while end < bytes.len() && bytes[end].is_ascii_alphanumeric() {
                end += 1;
            }
            let after = end == bytes.len() || sep(bytes[end]);
            if before && after {
                return true;
            }
            start = at + 1;
        }
        false
    };
    let mut texts: Vec<String> = ["commandPath", "registryAgentId", "agentId", "source"]
        .iter()
        .filter_map(|key| config.get(*key).and_then(Value::as_str).map(str::to_string))
        .collect();
    texts.extend(instance_args(config));
    texts.iter().any(|text| bounded(text))
}

/// OMP session folders a T3 instance's own arguments name (`--session-dir`).
pub fn omp_config_roots(home: &Path, instances: &[T3Instance]) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for instance in instances {
        if !matches!(instance.driver.as_str(), "acpRegistry" | "omp" | "pi") || !names_tool(&instance.config, "omp") {
            continue;
        }
        for value in arg_values(&instance_args(&instance.config), "--session-dir") {
            if let Some(path) = home_path(Some(&Value::String(value)), home) {
                roots.push(path);
            }
        }
    }
    roots
}

fn has_binary(home: &Path, env: &Env, names: &[&str]) -> bool {
    let mut dirs: Vec<PathBuf> = env
        .get("PATH")
        .map(|path| std::env::split_paths(path).collect())
        .unwrap_or_default();
    dirs.push(home.join(".local").join("bin"));
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ""] } else { &[""] };
    for name in names {
        for dir in &dirs {
            for ext in exts {
                let file = dir.join(format!("{name}{ext}"));
                if let Ok(meta) = fs::metadata(&file) {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::PermissionsExt;
                        if meta.is_file() && meta.permissions().mode() & 0o111 != 0 {
                            return true;
                        }
                    }
                    #[cfg(not(unix))]
                    if meta.is_file() {
                        return true;
                    }
                }
            }
        }
    }
    false
}

fn driver_enabled(instances: &[T3Instance], drivers: &[&str], word: Option<&str>) -> bool {
    instances
        .iter()
        .any(|i| i.enabled && drivers.contains(&i.driver.as_str()) && word.is_none_or(|w| names_tool(&i.config, w)))
}

/// Why this computer has a tool Analytics reads, apart from its logs.
pub fn installed_signals(home: &Path, env: &Env, kind: &str, instances: &[T3Instance]) -> Vec<&'static str> {
    let mut homes: Vec<PathBuf> = match kind {
        "claude" => vec![home.join(".claude"), home.join(".claude.json")],
        "codex" => vec![home.join(".codex")],
        "omp" => vec![home.join(".omp")],
        "muse" => vec![home.join(".local").join("share").join("muse")],
        "zcode" => vec![home.join(".zcode")],
        "antigravity" => AGY_DIRS.iter().map(|n| home.join(".gemini").join(n)).collect(),
        "cursor" => vec![home.join(".cursor")],
        "devin" => vec![
            home.join(".devin"),
            home.join(".config").join("devin"),
            crate::read::devin::database(home, env)
                .parent()
                .map(Path::to_path_buf)
                .unwrap_or_default(),
        ],
        _ => Vec::new(),
    };
    if kind == "claude"
        && let Some(dir) = env_dir(env, "CLAUDE_CONFIG_DIR", home)
    {
        homes.push(dir);
    }
    if kind == "codex"
        && let Some(dir) = env_dir(env, "CODEX_HOME", home)
    {
        homes.push(dir);
    }
    let binaries: &[&str] = match kind {
        "antigravity" => &["agy", "antigravity"],
        other => match other {
            "claude" => &["claude"],
            "codex" => &["codex"],
            "omp" => &["omp"],
            "muse" => &["muse"],
            "zcode" => &["zcode"],
            "cursor" => &["cursor-agent"],
            "devin" => &["devin"],
            _ => &[],
        },
    };
    let (drivers, word): (&[&str], Option<&str>) = match kind {
        "claude" => (&["claudeAgent"], None),
        "codex" => (&["codex"], None),
        "omp" => (&["acpRegistry", "omp"], Some("omp")),
        "muse" => (&["muse"], None),
        "zcode" => (&["acpRegistry", "zcode"], Some("zcode")),
        "antigravity" => (&["antigravity"], None),
        "cursor" => (&["cursor"], None),
        _ => (&[], None),
    };
    let mut signals = Vec::new();
    if homes.iter().any(|path| path.exists()) {
        signals.push("home");
    }
    if has_binary(home, env, binaries) {
        signals.push("binary");
    }
    if driver_enabled(instances, drivers, word) {
        signals.push("t3_instance");
    }
    signals
}

fn count_files(root: &Path, accept: &dyn Fn(&str) -> bool, deadline: Instant) -> (usize, i64, bool) {
    let (mut files, mut newest, mut entries) = (0usize, 0i64, 0usize);
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        if Instant::now() >= deadline {
            return (files, newest, true);
        }
        let Ok(list) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in list.flatten() {
            entries += 1;
            if entries > PRESENCE_MAX_ENTRIES {
                return (files, newest, true);
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < PRESENCE_MAX_DEPTH && !skipped(&name) {
                    pending.push((entry.path(), depth + 1));
                }
            } else if kind.is_file() && accept(&name) {
                files += 1;
                if let Ok(meta) = entry.metadata() {
                    newest = newest.max(util::mtime_ms(&meta));
                }
            }
        }
    }
    (files, newest, false)
}

fn app_dir(home: &Path, env: &Env, name: &str) -> PathBuf {
    if cfg!(windows) {
        env.get("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join("AppData").join("Roaming"))
            .join(name)
    } else if cfg!(target_os = "macos") {
        home.join("Library").join("Application Support").join(name)
    } else {
        env_dir(env, "XDG_CONFIG_HOME", home)
            .unwrap_or_else(|| home.join(".config"))
            .join(name)
    }
}

type Accept = Box<dyn Fn(&str) -> bool>;

struct Spec {
    tool: &'static str,
    dirs: Vec<(PathBuf, Accept)>,
    homes: Vec<PathBuf>,
    binaries: &'static [&'static str],
    drivers: &'static [&'static str],
    cause: &'static str,
}

/// Tools Analytics has no usage reader for, where this computer has them: install signals and how
/// many of their own session files exist (names and mtimes only; nothing is opened). Cursor keeps no
/// token usage in its local stores (messages without usage fields), so its cause is fixed.
pub fn presence(home: &Path, env: &Env, instances: &[T3Instance], budget: Duration) -> Map<String, Value> {
    let deadline = Instant::now() + budget;
    let data = env_dir(env, "XDG_DATA_HOME", home).unwrap_or_else(|| home.join(".local").join("share"));
    let specs = vec![
        Spec {
            tool: "cursor",
            dirs: vec![
                (
                    home.join(".cursor").join("chats"),
                    Box::new(|n: &str| n == "store.db") as Accept,
                ),
                (
                    home.join(".cursor").join("projects"),
                    Box::new(|n: &str| n == "store.db"),
                ),
            ],
            homes: vec![home.join(".cursor"), app_dir(home, env, "Cursor")],
            binaries: &["cursor-agent"],
            drivers: &["cursor"],
            cause: "no_local_usage_records",
        },
        Spec {
            tool: "gemini-cli",
            dirs: vec![(
                home.join(".gemini").join("tmp"),
                Box::new(|n: &str| n.starts_with("session-") && (n.ends_with(".json") || n.ends_with(".jsonl"))),
            )],
            homes: vec![home.join(".gemini").join("tmp")],
            binaries: &["gemini"],
            drivers: &[],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "opencode",
            dirs: vec![
                (
                    data.join("opencode").join("storage").join("message"),
                    Box::new(|n: &str| n.ends_with(".json")),
                ),
                (data.join("opencode"), Box::new(|n: &str| n == "opencode.db")),
            ],
            homes: vec![data.join("opencode")],
            binaries: &["opencode"],
            drivers: &["opencode"],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "qwen",
            dirs: vec![
                (
                    home.join(".qwen").join("usage"),
                    Box::new(|n: &str| n.ends_with(".jsonl")),
                ),
                (
                    home.join(".qwen").join("tmp"),
                    Box::new(|n: &str| n.ends_with(".json") || n.ends_with(".jsonl")),
                ),
            ],
            homes: vec![home.join(".qwen")],
            binaries: &["qwen"],
            drivers: &[],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "kimi",
            dirs: vec![(
                home.join(".kimi"),
                Box::new(|n: &str| n.ends_with(".jsonl") || n.ends_with(".json")),
            )],
            homes: vec![home.join(".kimi")],
            binaries: &["kimi"],
            drivers: &[],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "grok",
            dirs: vec![(
                home.join(".grok"),
                Box::new(|n: &str| n.ends_with(".jsonl") || n.ends_with(".json") || n.ends_with(".db")),
            )],
            homes: vec![home.join(".grok")],
            binaries: &["grok"],
            drivers: &["grok"],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "kiro",
            dirs: vec![
                (
                    home.join(".kiro"),
                    Box::new(|n: &str| n.ends_with(".jsonl") || n.ends_with(".json") || n.ends_with(".sqlite3")),
                ),
                (
                    data.join("kiro-cli"),
                    Box::new(|n: &str| n.ends_with(".sqlite3") || n.ends_with(".db")),
                ),
            ],
            homes: vec![home.join(".kiro"), data.join("kiro-cli")],
            binaries: &["kiro-cli"],
            drivers: &["kiro"],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "pi",
            dirs: vec![(
                home.join(".pi").join("agent").join("sessions"),
                Box::new(|n: &str| n.ends_with(".jsonl")),
            )],
            homes: vec![home.join(".pi")],
            binaries: &["pi"],
            drivers: &["pi"],
            cause: "no_reader_yet",
        },
        // `fx` is also a common JSON viewer: only a T3 fx instance says it is here.
        Spec {
            tool: "fx",
            dirs: vec![],
            homes: vec![],
            binaries: &[],
            drivers: &["fx"],
            cause: "no_reader_yet",
        },
    ];
    let mut result = Map::new();
    for spec in specs {
        let mut signals = Vec::new();
        if spec.homes.iter().any(|path| is_dir(path)) {
            signals.push("home");
        }
        if !spec.binaries.is_empty() && has_binary(home, env, spec.binaries) {
            signals.push("binary");
        }
        if !spec.drivers.is_empty() && driver_enabled(instances, spec.drivers, None) {
            signals.push("t3_instance");
        }
        let (mut files, mut newest, mut cut) = (0usize, 0i64, false);
        for (directory, accept) in &spec.dirs {
            if !is_dir(directory) {
                continue;
            }
            let (count, last, truncated) = count_files(directory, accept.as_ref(), deadline);
            files += count;
            newest = newest.max(last);
            cut |= truncated;
        }
        if signals.is_empty() && files == 0 {
            continue;
        }
        let mut entry = json!({
            "installed": true,
            "signals": signals,
            "cause": spec.cause,
            "counts": {"filesFound": files, "newestMtime": if newest > 0 { Value::from(newest) } else { Value::Null }},
        });
        if cut {
            entry["partial"] = Value::Bool(true);
        }
        result.insert(spec.tool.to_string(), entry);
    }
    result
}

/// The report contract's root id: never the path itself.
pub fn root_id(tool: &str, origin: &str, path: &Path, home: &Path) -> String {
    let text = format!("aac-root-v1:{tool}:{origin}:{}", tilde(path, home));
    util::sha256_hex(text.as_bytes())[..16].to_string()
}

/// A path with the home folder written as `~` (case-insensitive on Windows).
pub fn tilde(path: &Path, home: &Path) -> String {
    let absolute = path_text(&util::abspath(path));
    let home_text = path_text(&util::abspath(home));
    let home_text = home_text.trim_end_matches(['\\', '/']);
    let (a, h) = if cfg!(windows) {
        (absolute.to_lowercase(), home_text.to_lowercase())
    } else {
        (absolute.clone(), home_text.to_string())
    };
    if a == h {
        return "~".to_string();
    }
    let sep = std::path::MAIN_SEPARATOR;
    if a.starts_with(&format!("{h}{sep}")) {
        return format!("~{}", &absolute[home_text.len()..]);
    }
    absolute
}

/// The report contract's read half for one kind: install signals, each existing root (id, origin,
/// files, newest) and the kind's counts.
pub fn discovery_kind(collector: &Collector, home: &Path, env: &Env, kind: &str, instances: &[T3Instance]) -> Value {
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (path, origin) in collector.root_origins.get(kind).cloned().unwrap_or_default() {
        // A link that could not be listed or did not answer is reported as such, never stat'ed again.
        if let Some(state) = collector.link_states.get(&(kind.to_string(), path.clone())) {
            if seen.insert(path.clone()) {
                roots.push(json!({
                    "root": root_id(kind, &origin, &path, home),
                    "origin": origin,
                    "state": state,
                    "counts": {"filesFound": 0, "newestMtime": Value::Null},
                }));
            }
            continue;
        }
        let real = realpath(&path);
        if !fsx::exists(&real) || !seen.insert(real.clone()) {
            continue;
        }
        let (files, newest) = collector
            .root_counts
            .get(&(kind.to_string(), real.clone()))
            .copied()
            .unwrap_or((0, 0));
        roots.push(json!({
            "root": root_id(kind, &origin, &path, home),
            "origin": origin,
            "state": if files > 0 { "ok" } else { "installed_no_logs" },
            "counts": {"filesFound": files, "newestMtime": if newest > 0 { Value::from(newest) } else { Value::Null }},
        }));
        if roots.len() >= 64 {
            break;
        }
    }
    let found =
        collector.pending.get(kind).map_or(0, |m| m.len()) + collector.read_failures.get(kind).map_or(0, |s| s.len());
    let newest = roots.iter().filter_map(|r| r["counts"]["newestMtime"].as_i64()).max();
    let signals = installed_signals(home, env, kind, instances);
    json!({
        "installed": !roots.is_empty() || !signals.is_empty(),
        "signals": signals,
        "roots": roots,
        "counts": {
            "roots": roots.len(),
            "filesFound": found,
            "filesRead": collector.fresh.get(kind).map_or(0, |m| m.len()),
            "filesFailed": collector.read_failures.get(kind).map_or(0, |s| s.len()),
            "newestMtime": newest.map(Value::from).unwrap_or(Value::Null),
        },
    })
}

pub const EXPERIMENT_MAX_DEPTH: usize = 4;
pub const EXPERIMENT_MAX_ENTRIES: usize = 20_000;
pub const EXPERIMENT_MAX_ROOTS: usize = 64;
const EXPERIMENT_KINDS: [&str; 4] = ["codex", "claude", "omp", "muse"];

/// Whether a folder named after a kind holds that kind's logs, within 3 levels and 400 entries:
/// Codex `rollout-*.jsonl[.zst]`, OMP `<time>_<id>.jsonl`, Muse `<id>/session.jsonl`, Claude any
/// `.jsonl`.
fn holds_logs(kind: &str, dir: &Path) -> bool {
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    while let Some((folder, depth)) = pending.pop() {
        let Some(names) = fsx::sorted_names(&folder) else {
            continue;
        };
        for name in names {
            seen += 1;
            if seen > 400 {
                return false;
            }
            let path = folder.join(&name);
            if fsx::is_link(&path) {
                continue;
            }
            if fsx::is_dir(&path) {
                if depth < 3 && !fsx::skipped(&name) {
                    pending.push((path, depth + 1));
                }
                continue;
            }
            let log = match kind {
                "codex" => name.starts_with("rollout-") && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst")),
                "omp" => name.ends_with(".jsonl") && name.contains('_'),
                "muse" => name == "session.jsonl",
                _ => name.ends_with(".jsonl"),
            };
            if log {
                return true;
            }
        }
    }
    false
}

/// Experiment log folders outside `~/PM-Experiments` (DESIGN 16.5), for example
/// `~/t3setup-codex-v6/retained-usage/<run>/<kind>/`: a folder named after a kind (codex, claude,
/// omp, muse) that holds that kind's logs, at most 4 levels below home. Bounded by entries and time;
/// hidden folders (tool homes and caches), `node_modules`, `PM-Experiments` (its own walk) and git
/// checkouts (a `.git` inside: fixtures, not logs) are never entered, nor is any link. A Muse
/// folder's sessions sit one level down, so each of its sub-folders is the root.
pub fn experiment_roots(home: &Path, budget: Duration) -> (HashMap<&'static str, Vec<PathBuf>>, bool) {
    let deadline = Instant::now() + budget;
    let mut found: HashMap<&'static str, Vec<PathBuf>> = HashMap::new();
    let mut total = 0usize;
    let mut entries = 0usize;
    let mut pending: Vec<(PathBuf, usize)> = vec![(home.to_path_buf(), 0)];
    while let Some((folder, depth)) = pending.pop() {
        if Instant::now() >= deadline || entries >= EXPERIMENT_MAX_ENTRIES {
            return (found, true);
        }
        if depth > 0 && fsx::exists(&folder.join(".git")) {
            continue;
        }
        let Some(names) = fsx::sorted_names(&folder) else {
            continue;
        };
        for name in names {
            entries += 1;
            if entries >= EXPERIMENT_MAX_ENTRIES {
                return (found, true);
            }
            if name.starts_with('.') || fsx::skipped(&name) || name == "node_modules" || name == "PM-Experiments" {
                continue;
            }
            let path = folder.join(&name);
            if fsx::is_link(&path) || !fsx::is_dir(&path) {
                continue;
            }
            if depth >= 1
                && let Some(kind) = EXPERIMENT_KINDS.iter().find(|k| **k == name)
            {
                if holds_logs(kind, &path) {
                    let roots = if *kind == "muse" {
                        fsx::child_dirs(&path)
                    } else {
                        vec![path.clone()]
                    };
                    for root in roots {
                        if total >= EXPERIMENT_MAX_ROOTS {
                            return (found, true);
                        }
                        total += 1;
                        found.entry(kind).or_default().push(root);
                    }
                }
                continue;
            }
            if depth + 1 < EXPERIMENT_MAX_DEPTH {
                pending.push((path, depth + 1));
            }
        }
    }
    (found, false)
}
