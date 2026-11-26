use super::*;
use sha2::{Digest, Sha256};

/// Private process context. Deliberately neither Debug nor Serialize.
#[derive(Clone, Default)]
pub struct Process {
    /// Kernel process id.
    pub pid: u32,
    /// Parent process id.
    pub ppid: u32,
    /// Kernel start ticks, paired with pid to defeat reuse.
    pub start_time: String,
    /// Kernel state; zombies and dead processes are excluded.
    pub state: String,
    /// NUL-separated argv, kept as separate arguments.
    pub args: Vec<String>,
    /// Executable without Linux's deleted suffix.
    pub exe: String,
    /// Captured working directory.
    pub cwd: String,
    /// Credentials may be present. Never persist or log this field.
    pub env: BTreeMap<String, String>,
    /// Systemd service from the unified cgroup.
    pub unit: Option<String>,
}
impl Process {
    pub(super) fn alive(&self) -> bool {
        self.state != "Z" && self.state != "X"
    }
    pub(super) fn codex(&self) -> bool {
        Path::new(self.exe.strip_suffix(" (deleted)").unwrap_or(&self.exe))
            .file_name()
            .is_some_and(|n| n == "codex")
    }
    pub(super) fn desktop(&self) -> bool {
        self.exe.trim_end_matches(" (deleted)") == "/usr/lib/chatgpt/ChatGPT"
            && self
                .env
                .get("ELECTRON_RUN_AS_NODE")
                .is_none_or(String::is_empty)
            && chromium_switches(&self.argv())
                .get("type")
                .is_none_or(String::is_empty)
    }
    pub(super) fn proxy(&self) -> bool {
        self.args.windows(2).any(|v| v == ["app-server", "proxy"])
    }
    pub(super) fn daemon(&self) -> bool {
        self.codex()
            && self.args.iter().any(|s| s == "app-server")
            && self
                .args
                .windows(2)
                .any(|v| v[0] == "--listen" && v[1].starts_with("unix://"))
    }
    pub(super) fn on_home(&self, home: &Path) -> bool {
        let path = self
            .env
            .get("CODEX_HOME")
            .filter(|s| !s.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(self.env.get("HOME").map(String::as_str).unwrap_or("/")).join(".codex")
            });
        normalize(&path, Path::new(&self.cwd)) == normalize(home, Path::new(&self.cwd))
    }
    pub(super) fn same(&self, other: &Self) -> bool {
        self.pid == other.pid && self.start_time == other.start_time
    }
    // Read only by the Linux backend (`native.rs`).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub(super) fn fingerprint(&self) -> String {
        let bytes =
            serde_json::to_vec(&(&self.exe, &self.args, &self.cwd)).expect("strings serialize");
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
    pub(super) fn argv(&self) -> Vec<String> {
        if self.args.len() != 1 {
            return self
                .args
                .iter()
                .filter(|s| !s.is_empty())
                .cloned()
                .collect();
        }
        let text = &self.args[0];
        let mut starts = vec![0];
        for (i, c) in text.char_indices() {
            if c == '-'
                && text[i..].starts_with("--")
                && text[..i].chars().next_back().is_some_and(js_space)
            {
                starts.push(i);
            }
        }
        if starts.len() == 1 {
            return self.args.clone();
        }
        starts.push(text.len());
        let mut out = Vec::new();
        let mut ended = false;
        for (i, w) in starts.windows(2).enumerate() {
            let word = text[w[0]..w[1]].trim_end_matches(js_space);
            if i == 0 || ended {
                out.push(word.to_owned());
                continue;
            }
            if word == "--" || word[2..].chars().next().is_some_and(js_space) {
                out.push("--".into());
                let rest = word[2..].trim_matches(js_space);
                if !rest.is_empty() {
                    out.push(rest.into());
                }
                ended = true;
                continue;
            }
            if let Some(at) = word.find(js_space).filter(|at| !word[..*at].contains('=')) {
                out.push(word[..at].into());
                out.push(word[at..].trim_matches(js_space).into());
            } else {
                out.push(word.into());
            }
        }
        out
    }
}
pub(super) fn normalize(path: &Path, cwd: &Path) -> PathBuf {
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    };
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            std::path::Component::ParentDir => {
                out.pop();
            }
            std::path::Component::CurDir => {}
            _ => out.push(c.as_os_str()),
        }
    }
    out
}
pub(super) fn descendants(root: &Process, all: &[Process]) -> Vec<Process> {
    let mut ids = BTreeSet::from([root.pid]);
    loop {
        let n = ids.len();
        for p in all {
            if ids.contains(&p.ppid) {
                ids.insert(p.pid);
            }
        }
        if ids.len() == n {
            break;
        }
    }
    all.iter()
        .filter(|p| ids.contains(&p.pid))
        .cloned()
        .collect()
}
pub(super) fn desktop_launcher(
    main: &Process,
    all: &[Process],
    user_home: &Path,
) -> Result<Process> {
    let children = descendants(main, all);
    let mut launch = main.clone();
    launch.env = children
        .iter()
        .find(|p| {
            p.env.get("DISPLAY").is_some_and(|s| !s.is_empty())
                || p.env.get("WAYLAND_DISPLAY").is_some_and(|s| !s.is_empty())
        })
        .unwrap_or(main)
        .env
        .clone();
    launch.env.retain(|k, _| {
        !k.starts_with("CODEX_")
            && !k.starts_with("BROWSER_USE_")
            && k != "RUST_LOG"
            && k != "LOG_FORMAT"
    });
    if !["DISPLAY", "WAYLAND_DISPLAY"]
        .iter()
        .any(|k| launch.env.get(*k).is_some_and(|s| !s.is_empty()))
    {
        return Err(Error::RestartFailed);
    }
    launch.args = main.argv();
    if !chromium_switches(&launch.args).contains_key("user-data-dir") {
        let found = std::iter::once(main).chain(children.iter()).find_map(|p| {
            chromium_switches(&p.argv())
                .get("user-data-dir")
                .map(|s| (s.clone(), p))
        });
        let folder = match found {
            Some((s, p)) if !s.is_empty() => {
                if !Path::new(&s).is_absolute() && Path::new(&p.cwd).is_absolute() {
                    format!("{}/{}", p.cwd.trim_end_matches('/'), s)
                } else {
                    s
                }
            }
            _ => user_home
                .join(".config/Codex")
                .to_string_lossy()
                .into_owned(),
        };
        let at = launch
            .args
            .iter()
            .enumerate()
            .find(|(i, s)| *i > 0 && s.trim_matches(|c: char| c.is_ascii_whitespace()) == "--")
            .map(|(i, _)| i)
            .unwrap_or(launch.args.len());
        launch.args.insert(at, format!("--user-data-dir={folder}"));
    }
    Ok(launch)
}
/// The unified cgroup's leaf service, if present.
pub fn systemd_unit_of(text: &str) -> Option<String> {
    text.lines()
        .find_map(|s| s.strip_prefix("0::"))
        .and_then(|s| s.rsplit('/').next())
        .filter(|s| s.ends_with(".service"))
        .map(str::to_owned)
}

fn js_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}
