//! Running Codex process homes for the renewal guards
//! (`src/codex-auth/codex-activation-runtime.ts`: `readProcesses`,
//! `codexProcessHome`, `isCodex`, `isDesktop`, the Chromium switch parse).
//! Linux reads `/proc`; elsewhere the scan is unavailable unless a seam injects
//! a process list. Env stays in memory; a snapshot is never serialized.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One same-user process, as the renewal guards read it. The systemd unit field
/// the activation runtime also captures is not part of any renewal guard.
#[derive(Debug, Clone)]
pub struct CodexProcessSnapshot {
    /// Process id.
    pub pid: u32,
    /// Parent process id.
    pub ppid: u32,
    /// `/proc` stat field 22 (ticks since boot), as text.
    pub start_time: String,
    /// `/proc` stat state letter.
    pub state: String,
    /// Whole argv from cmdline.
    pub args: Vec<String>,
    /// Executable path (readlink of exe, argv[0] when unreadable).
    pub exe: String,
    /// Working directory for relevant processes; empty otherwise.
    pub cwd: String,
    /// Environment of relevant processes only.
    pub env: HashMap<String, String>,
}

/// Lexical `path.resolve`/`path.normalize`: no symlink resolution, `.` and `..`
/// folded, separators native. A rooted path without a drive on Windows takes
/// the current drive, as Node does.
pub fn node_resolve(base: &Path, selected: &str) -> PathBuf {
    let candidate = Path::new(selected);
    let joined = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else if cfg!(windows)
        && (selected.starts_with('/') || selected.starts_with('\\'))
        && !selected.contains(':')
    {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("C:\\"));
        let mut root: PathBuf = cwd.components().take(1).collect();
        root.push(std::path::MAIN_SEPARATOR.to_string());
        root.join(selected.trim_start_matches(['/', '\\']))
    } else {
        base.join(candidate)
    };
    normalize(&joined)
}

/// Lexical normalization (Node `path.normalize`), keeping the root intact.
pub fn normalize(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

/// `os.homedir()`: HOME on Unix, USERPROFILE on Windows.
pub fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let key = "USERPROFILE";
    #[cfg(not(windows))]
    let key = "HOME";
    std::env::var_os(key)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn strip_deleted(exe: &str) -> &str {
    exe.strip_suffix(" (deleted)").unwrap_or(exe)
}

fn base_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(index) => &trimmed[index + 1..],
        None => trimmed,
    }
}

fn is_codex(process: &CodexProcessSnapshot) -> bool {
    let exe = if process.exe.is_empty() {
        process.args.first().map(String::as_str).unwrap_or("")
    } else {
        &process.exe
    };
    base_name(strip_deleted(exe)) == "codex"
}

fn electron_runs_as_node(process: &CodexProcessSnapshot) -> bool {
    process
        .env
        .get("ELECTRON_RUN_AS_NODE")
        .is_some_and(|v| !v.is_empty())
}

/// Electron and its zygotes can rewrite a cmdline into one space-joined string.
fn rewritten_cmdline(process: &CodexProcessSnapshot) -> bool {
    static RE: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\s--").expect("static pattern"));
    process.args.len() == 1 && RE.is_match(&process.args[0])
}

fn trim_whitespace_ascii(text: &str) -> &str {
    text.trim_matches(['\t', '\n', '\u{0b}', '\u{0c}', '\r', ' '])
}

/// The argv a desktop process really carries: a rewritten zygote cmdline is
/// split back before each `--` word, flags join their value, and a bare `--`
/// terminates the switches.
fn process_argv(process: &CodexProcessSnapshot) -> Vec<String> {
    if !rewritten_cmdline(process) {
        return process
            .args
            .iter()
            .filter(|a| !a.is_empty())
            .cloned()
            .collect();
    }
    // split(/\s+(?=--)/): whitespace runs that look ahead to `--`.
    let text = &process.args[0];
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    let bytes: Vec<char> = text.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_whitespace() {
            let mut j = i;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            if bytes.get(j) == Some(&'-') && bytes.get(j + 1) == Some(&'-') {
                if !current.is_empty() {
                    words.push(std::mem::take(&mut current));
                }
                i = j;
                continue;
            }
            current.push(bytes[i]);
            i += 1;
        } else {
            current.push(bytes[i]);
            i += 1;
        }
    }
    if !current.is_empty() {
        words.push(current);
    }
    let mut out: Vec<String> = Vec::new();
    let mut ended = false;
    for (index, word) in words.iter().filter(|w| !w.is_empty()).enumerate() {
        if index == 0 || ended {
            out.push(word.clone());
            continue;
        }
        // /^--(?:\s+([\s\S]*))?$/ : a bare `--`, or `--` plus words after it.
        if word == "--" {
            ended = true;
            out.push("--".into());
        } else if let Some(rest) = word.strip_prefix("--") {
            if rest.starts_with(char::is_whitespace) {
                ended = true;
                out.push("--".into());
                let tail = rest.trim();
                if !tail.is_empty() {
                    out.push(tail.to_owned());
                }
            } else if let Some(ws) = rest.find(char::is_whitespace) {
                // /^(--[^\s=]+)\s+([\s\S]+)$/ : a flag joined to its value.
                let flag = &rest[..ws];
                if !flag.is_empty() && !flag.contains('=') && ws < rest.len() {
                    out.push(format!("--{flag}"));
                    out.push(rest[ws..].trim().to_owned());
                } else {
                    out.push(word.clone());
                }
            } else {
                out.push(word.clone());
            }
        } else {
            out.push(word.clone());
        }
    }
    out
}

/// Chromium's switch parse (base::CommandLine): after argv[0], ASCII-trimmed
/// words; parsing stops at a bare `--`; a switch is `-`/`--` with a name up to
/// the first `=` and the rest as value; later duplicates replace earlier ones.
fn chromium_switches(argv: &[String]) -> HashMap<String, String> {
    let mut switches = HashMap::new();
    let mut parse = true;
    for arg in argv.iter().skip(1).map(|a| trim_whitespace_ascii(a)) {
        parse = parse && arg != "--";
        if !parse {
            continue;
        }
        let prefix = if arg.starts_with("--") {
            2
        } else if arg.starts_with('-') {
            1
        } else {
            0
        };
        if prefix == 0 || prefix == arg.len() {
            continue;
        }
        let body = &arg[prefix..];
        match body.find('=') {
            Some(equals) => {
                switches.insert(body[..equals].to_owned(), body[equals + 1..].to_owned());
            }
            None => {
                switches.insert(body.to_owned(), String::new());
            }
        }
    }
    switches
}

fn is_desktop(process: &CodexProcessSnapshot) -> bool {
    strip_deleted(&process.exe) == "/usr/lib/chatgpt/ChatGPT"
        && !electron_runs_as_node(process)
        && !chromium_switches(&process_argv(process)).contains_key("type")
}

/// `codexProcessHome`: the home a Codex CLI or desktop process reads and
/// refreshes; null for other processes.
pub fn codex_process_home(process: &CodexProcessSnapshot) -> Option<PathBuf> {
    if !is_codex(process) && !is_desktop(process) {
        return None;
    }
    if let Some(configured) = process.env.get("CODEX_HOME").filter(|v| !v.is_empty()) {
        let cwd = if process.cwd.is_empty() {
            "/"
        } else {
            &process.cwd
        };
        return Some(node_resolve(Path::new(cwd), configured));
    }
    let home = process
        .env
        .get("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(home_dir);
    Some(node_resolve(&home, ".codex"))
}

/// The scan's outcome: homes of running Codex processes, or why there are none.
#[derive(Debug, Clone)]
pub struct CodexProcessScan {
    /// `ok`, `unavailable` (no scanner on this platform) or `failed`.
    pub state: ScanState,
    /// Deduplicated homes with their real paths.
    pub homes: Vec<ProcessHome>,
}

/// The scan state word.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanState {
    /// Processes were read.
    Ok,
    /// No scanner on this platform and none injected.
    Unavailable,
    /// The scanner threw; every profile skips.
    Failed,
}

impl ScanState {
    /// The wire word.
    pub fn word(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Unavailable => "unavailable",
            Self::Failed => "failed",
        }
    }
}

/// One running home and its resolved path.
#[derive(Debug, Clone)]
pub struct ProcessHome {
    /// The home as the process names it.
    pub home: PathBuf,
    /// Its realpath, when resolvable.
    pub real_home: Option<PathBuf>,
}

/// The injected scanner: `None` means unavailable, an error means failed.
pub type ScanProcesses =
    std::sync::Arc<dyn Fn() -> Result<Option<Vec<CodexProcessSnapshot>>, ()> + Send + Sync>;

/// `scanCodexProcessHomes`: without a seam, only Linux scans; a throwing scan
/// fails the whole guard, a null scan leaves the file sources standing alone.
pub fn scan_codex_process_homes(scan: Option<&ScanProcesses>) -> CodexProcessScan {
    let processes = match scan {
        Some(seam) => match seam() {
            Ok(list) => list,
            Err(()) => {
                return CodexProcessScan {
                    state: ScanState::Failed,
                    homes: Vec::new(),
                };
            }
        },
        None => {
            #[cfg(target_os = "linux")]
            {
                match read_processes() {
                    Ok(list) => Some(list),
                    Err(()) => {
                        return CodexProcessScan {
                            state: ScanState::Failed,
                            homes: Vec::new(),
                        };
                    }
                }
            }
            #[cfg(not(target_os = "linux"))]
            {
                None
            }
        }
    };
    let Some(processes) = processes else {
        return CodexProcessScan {
            state: ScanState::Unavailable,
            homes: Vec::new(),
        };
    };
    let mut seen: Vec<PathBuf> = Vec::new();
    let mut homes = Vec::new();
    for snapshot in &processes {
        let Some(home) = codex_process_home(snapshot) else {
            continue;
        };
        if seen.contains(&home) {
            continue;
        }
        seen.push(home.clone());
        let real_home = std::fs::canonicalize(&home).ok();
        homes.push(ProcessHome { home, real_home });
    }
    CodexProcessScan {
        state: ScanState::Ok,
        homes,
    }
}

/// Same-user processes from Linux `/proc`. Any inspection failure beyond a
/// vanished process fails the whole scan, as the TS reader does.
#[cfg(target_os = "linux")]
pub fn read_processes() -> Result<Vec<CodexProcessSnapshot>, ()> {
    use std::os::unix::fs::MetadataExt;
    let uid = unsafe { libc::getuid() };
    let entries = std::fs::read_dir("/proc").map_err(|_| ())?;
    let mut processes = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|_| ())?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_digit()) {
            continue;
        }
        let directory = entry.path();
        let result = (|| -> Result<Option<CodexProcessSnapshot>, ProcError> {
            let meta = std::fs::metadata(&directory).map_err(ProcError::from)?;
            if meta.uid() != uid {
                return Ok(None);
            }
            let stat = read_lossy(&directory.join("stat"))?;
            let Some(close) = stat.rfind(')') else {
                return Err(ProcError::Fatal);
            };
            let fields: Vec<&str> = stat[close + 2..].split(' ').collect();
            if fields.len() < 20 {
                return Err(ProcError::Fatal);
            }
            let cmdline = read_lossy(&directory.join("cmdline"))?;
            let args: Vec<String> = cmdline
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            if args.is_empty() {
                return Ok(None);
            }
            let mut exe = args[0].clone();
            match std::fs::read_link(directory.join("exe")) {
                Ok(target) => {
                    exe = target
                        .to_string_lossy()
                        .trim_end_matches(" (deleted)")
                        .to_owned();
                }
                Err(error) => {
                    let candidate = base_name(&exe) == "codex" || exe == "/usr/lib/chatgpt/ChatGPT";
                    if error.kind() != std::io::ErrorKind::PermissionDenied || candidate {
                        return Err(ProcError::from(error));
                    }
                }
            }
            let relevant = base_name(&exe) == "codex" || exe == "/usr/lib/chatgpt/ChatGPT";
            let mut env = HashMap::new();
            if relevant {
                let environ = read_lossy(&directory.join("environ"))?;
                for item in environ.split('\0') {
                    if let Some(separator) = item.find('=')
                        && separator > 0
                    {
                        env.insert(
                            item[..separator].to_owned(),
                            item[separator + 1..].to_owned(),
                        );
                    }
                }
            }
            let cwd = if relevant {
                std::fs::read_link(directory.join("cwd"))
                    .map_err(ProcError::from)?
                    .to_string_lossy()
                    .into_owned()
            } else {
                String::new()
            };
            Ok(Some(CodexProcessSnapshot {
                pid: name.parse().map_err(|_| ProcError::Fatal)?,
                ppid: fields[1].parse().map_err(|_| ProcError::Fatal)?,
                state: fields[0].to_owned(),
                start_time: fields[19].to_owned(),
                args,
                exe,
                cwd,
                env,
            }))
        })();
        match result {
            Ok(Some(snapshot)) => processes.push(snapshot),
            Ok(None) => {}
            Err(ProcError::Gone) => {}
            Err(ProcError::Fatal) => return Err(()),
        }
    }
    Ok(processes)
}

#[cfg(target_os = "linux")]
enum ProcError {
    Gone,
    Fatal,
}

#[cfg(target_os = "linux")]
impl From<std::io::Error> for ProcError {
    fn from(error: std::io::Error) -> Self {
        match error.kind() {
            std::io::ErrorKind::NotFound => Self::Gone,
            _ => Self::Fatal,
        }
    }
}

#[cfg(target_os = "linux")]
fn read_lossy(path: &Path) -> Result<String, ProcError> {
    let bytes = std::fs::read(path).map_err(ProcError::from)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// The non-Linux stub: the scan is unavailable without a seam.
#[cfg(not(target_os = "linux"))]
pub fn read_processes() -> Result<Vec<CodexProcessSnapshot>, ()> {
    Err(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codex_process(home: Option<&str>) -> CodexProcessSnapshot {
        let mut env = HashMap::new();
        match home {
            Some(home) => {
                env.insert("CODEX_HOME".into(), home.into());
            }
            None => {
                env.insert("HOME".into(), "/nonexistent-home".into());
            }
        }
        CodexProcessSnapshot {
            pid: 4242,
            ppid: 1,
            start_time: "100".into(),
            state: "S".into(),
            args: vec!["/usr/local/bin/codex".into(), "app-server".into()],
            exe: "/usr/local/bin/codex".into(),
            cwd: "/".into(),
            env,
        }
    }

    #[test]
    fn homes_follow_codex_home_and_default() {
        let process = codex_process(Some("/elsewhere/home"));
        assert_eq!(
            codex_process_home(&process),
            Some(node_resolve(Path::new("/"), "/elsewhere/home"))
        );
        let process = codex_process(None);
        assert_eq!(
            codex_process_home(&process),
            Some(node_resolve(Path::new("/nonexistent-home"), ".codex"))
        );
        let mut other = codex_process(Some("/x"));
        other.exe = "/usr/bin/python3".into();
        other.args = vec!["python3".into()];
        assert_eq!(codex_process_home(&other), None);
    }

    #[test]
    fn desktop_children_are_not_desktop() {
        let mut main = codex_process(None);
        main.exe = "/usr/lib/chatgpt/ChatGPT".into();
        main.args = vec!["/usr/lib/chatgpt/ChatGPT".into()];
        assert!(is_desktop(&main));
        let mut child = main.clone();
        child.args = vec!["/usr/lib/chatgpt/ChatGPT".into(), "--type=zygote".into()];
        assert!(!is_desktop(&child));
        let mut rewritten = main.clone();
        rewritten.args = vec!["/usr/lib/chatgpt/ChatGPT --type=renderer --foo bar".into()];
        assert!(!is_desktop(&rewritten));
        let mut node_mode = main.clone();
        node_mode
            .env
            .insert("ELECTRON_RUN_AS_NODE".into(), "1".into());
        assert!(!is_desktop(&node_mode));
    }

    #[test]
    fn chromium_switches_stop_at_the_terminator() {
        let argv: Vec<String> = ["chrome", "--type=zygote", "--", "--type=fake"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let switches = chromium_switches(&argv);
        assert_eq!(switches.get("type").map(String::as_str), Some("zygote"));
        let argv: Vec<String> = ["chrome", " --flag value ", "--x=1", "--x=2"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let switches = chromium_switches(&argv);
        assert_eq!(switches.get("x").map(String::as_str), Some("2"));
        assert!(switches.contains_key("flag value"));
    }
}
