//! Where each tool keeps its logs on this computer, from what is configured here and never from a
//! per-computer list (DESIGN section 2): the tool's documented default for this OS, its env var, its
//! own config, T3 Code's provider instances, AAC's own profile homes, folders under home that carry
//! the tool's home signature (any name), and the extra roots saved in Settings. Each root has one
//! origin; its path never leaves this computer (`root_id` is a digest with home written as `~`).

use crate::collector::Collector;
use crate::fsx::{self, home_path, is_dir, is_link, link_inside, skipped};
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

/// The scan's environment from the process's variables. Windows names variables in any case (the
/// system's `Path`, say): there every key is upper-cased, and every lookup here uses upper case
/// (FIX-ROUND-BIG 12).
pub fn env_from(vars: impl IntoIterator<Item = (String, String)>) -> Env {
    vars.into_iter()
        .map(|(key, value)| (if cfg!(windows) { key.to_uppercase() } else { key }, value))
        .collect()
}

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
    // A regular file of at most 1 MiB, read without blocking (FIX-ROUND-BIG 9): a FIFO or a device
    // there is no settings file.
    let Some(text) = fsx::read_regular(&settings, T3_SETTINGS_MAX_BYTES) else {
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

/// Direct children of home with a Codex home's signature (`sessions` or `archived_sessions`, with
/// `config.toml`, `auth.json` or a `state_*.sqlite`) or a Claude config dir's (`projects`, with one of
/// CLAUDE_DIR_MARKERS), whatever their name. A link counts only when its target stays inside home.
/// Cloud-synced and personal folders are not looked in (FIX-ROUND-BIG 15). The third value is
/// `(looked at, children)` when home has more children than the search takes (FIX-ROUND-BIG 6).
pub fn signature_homes(home: &Path) -> (Vec<PathBuf>, Vec<PathBuf>, Option<(usize, usize)>) {
    signature_homes_checked(home, &mut Vec::new())
}

fn signature_homes_checked(
    home: &Path,
    failures: &mut Vec<(PathBuf, &'static str)>,
) -> (Vec<PathBuf>, Vec<PathBuf>, Option<(usize, usize)>) {
    let (mut codex, mut claude) = (Vec::new(), Vec::new());
    let names = match fsx::list_dir(home) {
        Ok(names) => names,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                failures.push((home.to_path_buf(), fsx::error_cause(&error)));
            }
            return (codex, claude, None);
        }
    };
    let total = names.len();
    let limit = (total > SIGNATURE_MAX_CHILDREN).then_some((SIGNATURE_MAX_CHILDREN, total));
    for name in names.into_iter().take(SIGNATURE_MAX_CHILDREN) {
        if skipped(&name) || name == "PM-Experiments" || fsx::personal_or_cloud(&name) {
            continue;
        }
        let path = home.join(&name);
        if is_link(&path) && link_inside(&path, home).is_none() {
            continue;
        }
        match fsx::metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                failures.push((path, fsx::error_cause(&error)));
                continue;
            }
        }
        // Discover the names once before probing children. A protected, unrelated home child
        // cannot establish that any of sessions/archived_sessions/projects even exists.
        // Report the actual failed directory once, never three imaginary log folders.
        let entries = match fsx::list_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                if !matches!(
                    error.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) {
                    failures.push((path, fsx::error_cause(&error)));
                }
                continue;
            }
        };
        let has = |entry: &str| entries.iter().any(|name| name == entry);
        let codex_logs = has("sessions") || has("archived_sessions");
        let codex_marker = codex_logs && CODEX_HOME_MARKERS.iter().any(|m| has(m));
        let claude_home = has("projects") && CLAUDE_DIR_MARKERS.iter().any(|m| has(m));
        if codex_logs
            && (codex_marker
                || entries
                    .iter()
                    .any(|n| n.starts_with("state_") && n.ends_with(".sqlite")))
        {
            codex.push(path.clone());
        }
        if claude_home {
            claude.push(path);
        }
    }
    (codex, claude, limit)
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
pub fn t3_root_origins(
    home: &Path,
    kind: &str,
    instances: &[T3Instance],
) -> (Vec<(PathBuf, &'static str)>, Vec<PathBuf>) {
    let mut homes = Vec::new();
    let mut failed: Vec<PathBuf> = Vec::new();
    let mut add_base = |base: PathBuf, homes: &mut Vec<PathBuf>| {
        let (dirs, unreadable) = fsx::child_dirs_checked(&base);
        if unreadable {
            failed.push(base);
        }
        homes.extend(dirs);
    };
    if kind == "claude" {
        add_base(home.join(".claude-t3"), &mut homes);
    } else {
        add_base(home.join(".codex-t3"), &mut homes);
        let managed = home.join(".t3").join("userdata").join("providers").join("codex");
        let (dirs, unreadable) = fsx::child_dirs_checked(&managed);
        if unreadable {
            failed.push(managed);
        }
        homes.extend(dirs.into_iter().map(|dir| dir.join("shadow")));
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
    (
        log_dirs(kind, &homes).into_iter().map(|p| (p, "t3_instance")).collect(),
        failed,
    )
}

pub struct RootPlan {
    /// Read as they are (the tool's own defaults, env, extras).
    pub defaults: Vec<(PathBuf, &'static str)>,
    /// Read through the copy rule: a record a default log already holds counts once.
    pub others: Vec<(PathBuf, &'static str)>,
    /// The signature search's bound, when home has more children than it looks at.
    pub limit: Option<(usize, usize)>,
    /// Account bases (T3, AAC instances) that could not be listed: named by the reader, so the
    /// kind is partial instead of silently missing their homes (RECHECK-BIG-1 M3).
    pub failed_bases: Vec<PathBuf>,
    /// A candidate whose signature could not be checked; not an established log folder.
    pub search_failures: Vec<(PathBuf, &'static str)>,
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
    let (mut others, mut failed_bases) = t3_root_origins(home, "claude", instances);
    let instances_base = home.join(".ccs").join("instances");
    let (instance_homes, unreadable) = fsx::child_dirs_checked(&instances_base);
    if unreadable {
        failed_bases.push(instances_base);
    }
    others.extend(
        log_dirs("claude", &instance_homes)
            .into_iter()
            .map(|p| (p, "ccs_instance")),
    );
    let mut search_failures = Vec::new();
    let (_, signed, limit) = signature_homes_checked(home, &mut search_failures);
    others.extend(log_dirs("claude", &signed).into_iter().map(|p| (p, "signature")));
    RootPlan {
        defaults,
        others,
        limit,
        failed_bases,
        search_failures,
    }
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
    let (mut others, mut failed_bases) = t3_root_origins(home, "codex", instances);
    let instances_base = home.join(".ccs").join("codex-instances");
    let (instance_homes, unreadable) = fsx::child_dirs_checked(&instances_base);
    if unreadable {
        failed_bases.push(instances_base);
    }
    others.extend(
        log_dirs("codex", &instance_homes)
            .into_iter()
            .map(|p| (p, "ccs_instance")),
    );
    let mut search_failures = Vec::new();
    let (signed, _, limit) = signature_homes_checked(home, &mut search_failures);
    others.extend(log_dirs("codex", &signed).into_iter().map(|p| (p, "signature")));
    RootPlan {
        defaults,
        others,
        limit,
        failed_bases,
        search_failures,
    }
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
    let exts: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat", ""]
    } else {
        &[""]
    };
    for name in names {
        for dir in &dirs {
            for ext in exts {
                let file = dir.join(format!("{name}{ext}"));
                if fsx::avoided(&file) {
                    continue;
                }
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

fn tool_binaries(kind: &str) -> &'static [&'static str] {
    match kind {
        "antigravity" => &["agy", "antigravity"],
        "claude" => &["claude"],
        "codex" => &["codex"],
        "omp" => &["omp"],
        "muse" => &["muse"],
        "zcode" => &["zcode"],
        "cursor" => &["cursor-agent"],
        "devin" => &["devin"],
        _ => &[],
    }
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
    let binaries = tool_binaries(kind);
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
    // The home is there (a link counts, never followed: its target may be a mount that hangs).
    if homes.iter().any(|path| fsx::exists(path)) {
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
    let mut incomplete = false;
    let mut pending = vec![(root.to_path_buf(), 0usize)];
    while let Some((directory, depth)) = pending.pop() {
        if Instant::now() >= deadline {
            return (files, newest, true);
        }
        if fsx::avoided(&directory) {
            incomplete = true;
            continue;
        }
        let list = match fs::read_dir(&directory) {
            Ok(list) => list,
            Err(error) => {
                incomplete |= error.kind() != std::io::ErrorKind::NotFound;
                continue;
            }
        };
        for entry in list {
            entries += 1;
            if entries > PRESENCE_MAX_ENTRIES {
                return (files, newest, true);
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    incomplete |= error.kind() != std::io::ErrorKind::NotFound;
                    continue;
                }
            };
            let name = entry.file_name().to_string_lossy().into_owned();
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    incomplete |= error.kind() != std::io::ErrorKind::NotFound;
                    continue;
                }
            };
            if kind.is_dir() {
                if !skipped(&name) {
                    if depth < PRESENCE_MAX_DEPTH {
                        pending.push((entry.path(), depth + 1));
                    } else {
                        incomplete = true;
                    }
                }
            } else if kind.is_file() && accept(&name) {
                files += 1;
                match entry.metadata() {
                    Ok(meta) => newest = newest.max(util::mtime_ms(&meta)),
                    Err(error) => incomplete |= error.kind() != std::io::ErrorKind::NotFound,
                }
            }
        }
    }
    (files, newest, incomplete)
}

/// Current CLI chat locations in addition to the older `.cursor/chats`. These stores contain
/// blobs and metadata, not exposed token counters. Discover their activity without decoding any
/// conversation blobs or treating a file as a numeric usage event.
pub fn cursor_chat_roots(home: &Path, env: &Env) -> Vec<PathBuf> {
    let config = env_dir(env, "XDG_CONFIG_HOME", home).unwrap_or_else(|| home.join(".config"));
    let mut roots = vec![home.join(".cursor/chats"), config.join("cursor/chats")];
    roots.push(app_dir(home, env, "cursor").join("chats"));
    roots.sort();
    roots.dedup();
    roots
}

/// One normal Analytics scan cycle. A source is dated only by entries or files that actually carry
/// usage: installation, login, thread and chat-store touches are never usage dates.
pub const FRESHNESS_CYCLE_MS: i64 = 5 * 60 * 1000;

pub fn freshness(
    newest_data: Option<i64>,
    newest_activity: Option<i64>,
    min_date: i64,
    activity_complete: bool,
) -> Value {
    let state = match (newest_data, newest_activity) {
        (Some(data), Some(activity)) if activity.saturating_sub(data) > FRESHNESS_CYCLE_MS => "stale",
        (None, Some(activity)) if activity >= min_date => "stale",
        (Some(_), _) if activity_complete => "current",
        _ => "unknown",
    };
    json!({"state": state, "newestDataMs": newest_data, "newestActivityMs": newest_activity, "activityComplete": activity_complete})
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
            tool: "claude-desktop",
            dirs: vec![(
                app_dir(home, env, "Claude"),
                Box::new(|n: &str| n.ends_with(".log") || n.ends_with(".jsonl")),
            )],
            homes: vec![app_dir(home, env, "Claude")],
            binaries: &[],
            drivers: &[],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "codex-app",
            dirs: vec![(
                app_dir(home, env, "Codex"),
                Box::new(|n: &str| n.ends_with(".log") || n.ends_with(".jsonl")),
            )],
            homes: vec![app_dir(home, env, "Codex")],
            binaries: &[],
            drivers: &[],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "cursor-ide",
            dirs: vec![(
                app_dir(home, env, "Cursor"),
                Box::new(|n: &str| n.ends_with(".vscdb") || n.ends_with(".log")),
            )],
            homes: vec![app_dir(home, env, "Cursor")],
            binaries: &[],
            drivers: &[],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "windsurf",
            dirs: vec![
                (
                    app_dir(home, env, "Windsurf"),
                    Box::new(|n: &str| n.ends_with(".vscdb") || n.ends_with(".log")),
                ),
                (
                    home.join(".codeium/windsurf"),
                    Box::new(|n: &str| n.ends_with(".pb") || n.ends_with(".json")),
                ),
            ],
            homes: vec![app_dir(home, env, "Windsurf"), home.join(".codeium/windsurf")],
            binaries: &["windsurf"],
            drivers: &[],
            cause: "no_reader_yet",
        },
        Spec {
            tool: "cursor",
            dirs: cursor_chat_roots(home, env)
                .into_iter()
                .map(|root| (root, Box::new(|n: &str| n == "store.db") as Accept))
                .chain(vec![(
                    home.join(".cursor").join("projects"),
                    Box::new(|n: &str| n == "store.db") as Accept,
                )])
                .collect(),
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
        if spec.homes.iter().any(|path| fsx::exists(path)) {
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
            // A linked folder is not followed: its target may be a mount that hangs.
            if is_link(directory) || !is_dir(directory) {
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
        // Lower-casing can change byte lengths (Windows): never cut inside a character.
        if let Some(rest) = absolute.get(home_text.len()..) {
            return format!("~{rest}");
        }
    }
    absolute
}

/// The report contract's read half for one kind: install signals, each existing root (id, origin,
/// files, newest) and the kind's counts.
pub fn discovery_kind(collector: &Collector, home: &Path, env: &Env, kind: &str, instances: &[T3Instance]) -> Value {
    let mut roots = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for (path, origin) in collector.root_origins.get(kind).cloned().unwrap_or_default() {
        // A root that is there but could not be read is reported as such (with why), never stat'ed
        // again (DESIGN 16.4, FIX-ROUND-BIG 4).
        if let Some((state, cause)) = collector.root_failures.get(&(kind.to_string(), path.clone())) {
            if seen.insert(path.clone()) {
                roots.push(json!({
                    "root": root_id(kind, &origin, &path, home),
                    "origin": origin,
                    "state": state,
                    "cause": cause,
                    "counts": {"filesFound": 0, "newestMtime": Value::Null},
                }));
            }
            continue;
        }
        // A root its probe found absent is not resolved or stat'ed again: past a link, that could be
        // a mount that does not answer (FIX-ROUND-BIG 9).
        if collector.absent_roots.contains(&(kind.to_string(), path.clone())) {
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
    // Each log once: a file that failed is counted as failed, not also as found pending.
    let failed = collector.read_failures.get(kind);
    // Settled files are found and read like unchanged ones, from their prints (FINAL-NEON H1).
    let settled: Vec<&Value> = settled_prints(collector, kind).collect();
    let found = collector.pending.get(kind).map_or(0, |m| {
        m.keys().filter(|k| !failed.is_some_and(|f| f.contains(*k))).count()
    }) + failed.map_or(0, |s| s.len())
        + settled.len();
    let newest = roots.iter().filter_map(|r| r["counts"]["newestMtime"].as_i64()).max();
    // Activity is dated only by entries that carry usage: the newest measured usage event. Tool
    // homes, binaries, login rewrites, T3 thread records, token-less Cursor chat stores and logs
    // touched without a usage entry are installation or metadata signals, never usage dates, so
    // they never make a source look stale. A source with no measured usage has no activity date.
    let newest_data = collector
        .fresh
        .get(kind)
        .into_iter()
        .flat_map(|prints| prints.values().filter_map(|p| p.newest_usage_ms))
        .chain(settled.iter().filter_map(|p| p["newestUsageMs"].as_i64()))
        .max();
    let activity = newest_data;
    // Coverage is the usage read's own completeness, not a file listing: a chat store the bounded
    // Cursor listing cannot reach, a usage-shaped record the reader could not decode and a
    // write-ahead log it could not see all leave current coverage unverified.
    let unread_usage = collector
        .fresh
        .get(kind)
        .is_some_and(|prints| prints.values().any(|p| p.unread_usage == Some(true)))
        || settled_flag(collector, kind, "unreadUsage");
    let wal_unread = collector.kind_wal_unread.contains(kind) || (kind == "zcode" && collector.wal_unread);
    let mut activity_limited = collector.usage_partial.contains(kind) || unread_usage || wal_unread;
    let mut unmeasured_files = 0usize;
    if kind == "cursor" {
        let deadline = collector.deadline().min(Instant::now() + Duration::from_millis(750));
        for root in cursor_chat_roots(home, env) {
            let (files, _, cut) = count_files(&root, &|n| n == "store.db" || n == "store.db-wal", deadline);
            activity_limited |= cut;
            unmeasured_files += files;
        }
    }
    let signals = installed_signals(home, env, kind, instances);
    let coverage = freshness(newest_data, activity, collector.min_date_ms, !activity_limited);
    let mut discovered = json!({
        "installed": !roots.is_empty() || !signals.is_empty(),
        "signals": signals,
        "freshness": coverage,
        "unmeasuredFiles": unmeasured_files,
        "roots": roots,
        "counts": {
            "roots": roots.len(),
            "filesFound": found,
            "filesRead": collector.fresh.get(kind).map_or(0, |m| m.len()) + settled.len(),
            "filesFailed": collector.read_failures.get(kind).map_or(0, |s| s.len()),
            "newestMtime": newest.map(Value::from).unwrap_or(Value::Null),
        },
    });
    if unread_usage {
        discovered["unreadUsage"] = Value::Bool(true);
    }
    if collector.estimated_clocks.contains(kind)
        || collector
            .fresh
            .get(kind)
            .is_some_and(|prints| prints.values().any(|p| p.estimated_clock == Some(true)))
        || settled_flag(collector, kind, "estimatedClock")
    {
        // Disclosed: the numbers are counted, their date is a file-time estimate and never a
        // measured usage event. The flag on the file's print keeps it across unchanged rescans.
        discovered["estimatedClock"] = Value::Bool(true);
    }
    discovered
}

/// Prints of this kind's settled files, confirmed unread this scan, that no read replaced.
fn settled_prints<'a>(collector: &'a Collector, kind: &'a str) -> impl Iterator<Item = &'a Value> + 'a {
    collector
        .settled
        .get(kind)
        .into_iter()
        .flat_map(|prints| prints.iter())
        .filter(move |(key, _)| {
            !collector.pending.get(kind).is_some_and(|p| p.contains_key(*key))
                && !collector.read_failures.get(kind).is_some_and(|f| f.contains(*key))
        })
        .map(|(_, print)| print)
}

/// Whether a settled file of this kind carries a disclosure flag (`unreadUsage`, `estimatedClock`).
pub fn settled_flag(collector: &Collector, kind: &str, flag: &str) -> bool {
    settled_prints(collector, kind).any(|print| print[flag] == true)
}

pub const EXPERIMENT_MAX_DEPTH: usize = 4;
pub const EXPERIMENT_MAX_ENTRIES: usize = 20_000;
/// Per kind (FIX-ROUND-BIG 6): one kind's many folders never use up another's.
pub const EXPERIMENT_MAX_ROOTS: usize = 64;
const EXPERIMENT_KINDS: [&str; 4] = ["codex", "claude", "omp", "muse"];

/// Whether a folder named after a kind holds that kind's logs, within 3 levels and 400 entries:
/// Codex `rollout-*.jsonl[.zst]`, OMP `<time>_<id>.jsonl`, Muse `<id>/session.jsonl`, Claude any
/// `.jsonl`. The second value says a listing inside it failed, so logs may hide there
/// (RECHECK-BIG-1 M3).
fn holds_logs(kind: &str, dir: &Path) -> (bool, bool) {
    let mut pending = vec![(dir.to_path_buf(), 0usize)];
    let mut seen = 0usize;
    let mut failed = false;
    while let Some((folder, depth)) = pending.pop() {
        let names = match fsx::list_dir(&folder) {
            Ok(names) => names,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                failed = true;
                continue;
            }
        };
        for name in names {
            seen += 1;
            if seen > 400 {
                // A real kind-named candidate exists. Let its reader finish the search instead
                // of treating this bounded preflight as proof that the folder holds no logs.
                return (true, failed);
            }
            let path = folder.join(&name);
            if fsx::is_link(&path) {
                continue;
            }
            let meta = match fsx::metadata(&path) {
                Ok(meta) => meta,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    failed = true;
                    continue;
                }
            };
            if meta.is_dir() {
                if !fsx::skipped(&name) {
                    if depth >= 3 {
                        return (true, failed);
                    }
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
            if log && meta.is_file() {
                return (true, failed);
            }
        }
    }
    (false, failed)
}

/// What the optional experiment walk found, and per-kind caps on real candidate roots.
pub struct ExperimentRoots {
    /// Only the optional walk was cut; this does not establish an unread log root.
    pub optional_limited: bool,
    pub found: HashMap<&'static str, Vec<PathBuf>>,
    pub limits: HashMap<&'static str, (usize, Option<usize>)>,
    /// Kind-named folders whose look inside failed: named by their kind's reader (M3).
    pub failed: HashMap<&'static str, Vec<PathBuf>>,
}

/// Experiment log folders outside `~/PM-Experiments` (DESIGN 16.5), for example
/// `~/t3setup-codex-v6/retained-usage/<run>/<kind>/`: a folder named after a kind (codex, claude,
/// omp, muse) that holds that kind's logs, at most 4 levels below home, at most 64 per kind. Bounded
/// by entries and time; hidden folders (tool homes and caches), `node_modules`, `PM-Experiments`
/// (its own walk), cloud-synced and personal folders, and git checkouts (a `.git` inside: fixtures,
/// not logs) are never entered, nor is any link. A Muse folder is one root: its sessions are found
/// under it at any depth. A cut of this optional search is advisory; a cap that leaves known
/// log roots unread remains a per-kind limit.
pub fn experiment_roots(home: &Path, budget: Duration) -> ExperimentRoots {
    let deadline = Instant::now() + budget;
    let mut result = ExperimentRoots {
        optional_limited: false,
        found: HashMap::new(),
        limits: HashMap::new(),
        failed: HashMap::new(),
    };
    let mut seen: HashMap<&'static str, usize> = HashMap::new();
    let mut entries = 0usize;
    let mut pending: Vec<(PathBuf, usize)> = vec![(home.to_path_buf(), 0)];
    let mut cut = false;
    'walk: while let Some((folder, depth)) = pending.pop() {
        if Instant::now() >= deadline || entries >= EXPERIMENT_MAX_ENTRIES {
            cut = true;
            break;
        }
        if depth > 0 && fsx::exists(&folder.join(".git")) {
            continue;
        }
        let names = match fsx::list_dir(&folder) {
            Ok(names) => names,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                for kind in EXPERIMENT_KINDS {
                    result.failed.entry(kind).or_default().push(folder.clone());
                }
                continue;
            }
        };
        for name in names {
            entries += 1;
            if entries >= EXPERIMENT_MAX_ENTRIES {
                cut = true;
                break 'walk;
            }
            if name.starts_with('.')
                || fsx::skipped(&name)
                || name == "node_modules"
                || name == "PM-Experiments"
                || fsx::personal_or_cloud(&name)
            {
                continue;
            }
            let path = folder.join(&name);
            if fsx::is_link(&path) {
                continue;
            }
            match fsx::metadata(&path) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_) => continue,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => {
                    for kind in EXPERIMENT_KINDS {
                        result.failed.entry(kind).or_default().push(path.clone());
                    }
                    continue;
                }
            }
            if depth >= 1
                && let Some(kind) = EXPERIMENT_KINDS.iter().find(|k| **k == name)
            {
                let (holds, failed) = holds_logs(kind, &path);
                if failed {
                    result.failed.entry(kind).or_default().push(path.clone());
                }
                if holds {
                    let count = seen.entry(kind).or_default();
                    *count += 1;
                    if *count <= EXPERIMENT_MAX_ROOTS {
                        result.found.entry(kind).or_default().push(path);
                    }
                }
                continue;
            }
            if depth + 1 < EXPERIMENT_MAX_DEPTH {
                pending.push((path, depth + 1));
            }
        }
    }
    for kind in EXPERIMENT_KINDS {
        let count = seen.get(kind).copied().unwrap_or(0);
        if count > EXPERIMENT_MAX_ROOTS {
            result.limits.insert(kind, (EXPERIMENT_MAX_ROOTS, Some(count)));
        }
    }
    result.optional_limited = cut;
    result
}
