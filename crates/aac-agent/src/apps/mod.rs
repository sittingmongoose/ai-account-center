//! Read-only app discovery, with the legacy helper payload as the wire contract.
mod adapters;
mod cli;
mod packages;
mod python;
pub(crate) mod update;

use aac_core::{Cause, Failure};
use aac_files::read::{self, Read, ReadPolicy};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    process::Stdio,
    sync::LazyLock,
    time::Duration,
};
use tokio::io::AsyncReadExt;

const APPS: [&str; 12] = [
    "antigravity-cli",
    "muse-code",
    "omp",
    "codex-cli",
    "codex-desktop",
    "claude-code",
    "claude-desktop",
    "zcode",
    "t3-acp-adapters",
    "t3-code",
    "cursor-cli",
    "antigravity-acp",
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    Ubuntu,
    Mac,
    Windows,
}

impl Platform {
    fn native() -> Option<Self> {
        match std::env::consts::OS {
            "linux" => Some(Self::Ubuntu),
            "macos" => Some(Self::Mac),
            "windows" => Some(Self::Windows),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Ubuntu => "ubuntu",
            Self::Mac => "mac",
            Self::Windows => "windows",
        }
    }
}

struct Host {
    platform: Platform,
    home: PathBuf,
    local: PathBuf,
    roaming: PathBuf,
    program_files: PathBuf,
    path: Vec<PathBuf>,
    path_ext: Vec<String>,
    powershell: Option<PathBuf>,
    npm_prefix: Option<PathBuf>,
    arch: &'static str,
    // Tests present the three platforms' paths under one temporary directory.
    #[cfg(test)]
    system_root: Option<PathBuf>,
}

impl Host {
    fn native(platform: Platform) -> Option<Self> {
        let home = std::env::home_dir().filter(|path| path.is_absolute())?;
        let folder = |name, default| {
            std::env::var_os(name)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or(default)
        };
        Some(Self {
            platform,
            local: folder("LOCALAPPDATA", home.join("AppData/Local")),
            roaming: folder("APPDATA", home.join("AppData/Roaming")),
            program_files: folder("ProgramFiles", PathBuf::from("C:/Program Files")),
            path: std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).collect(),
            path_ext: std::env::var("PATHEXT")
                .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD;.VBS;.JS;.WS;.MSC".into())
                .split(';')
                .filter(|ext| !ext.is_empty())
                .map(str::to_owned)
                .collect(),
            powershell: std::env::var_os("SystemRoot").map(|root| {
                PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe")
            }),
            npm_prefix: std::env::var_os("npm_config_prefix")
                .or_else(|| std::env::var_os("NPM_CONFIG_PREFIX"))
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
            arch: std::env::consts::ARCH,
            home,
            #[cfg(test)]
            system_root: None,
        })
    }

    fn system(&self, path: &str) -> PathBuf {
        #[cfg(test)]
        if let Some(root) = &self.system_root {
            return root.join(path.trim_start_matches('/'));
        }
        PathBuf::from(path)
    }

    fn which(&self, name: &str) -> Option<PathBuf> {
        for folder in self.path.iter().filter(|path| path.is_absolute()) {
            if self.platform == Platform::Windows {
                if let Some(path) = self.path_ext.iter().find_map(|ext| {
                    let path = folder.join(format!("{name}{}", ext.to_ascii_lowercase()));
                    path.is_file().then_some(path)
                }) {
                    return Some(path);
                }
            } else {
                let path = folder.join(name);
                if path.is_file() && executable(&path) {
                    return Some(path);
                }
            }
        }
        None
    }

    fn mac_paths(&self, words: bool) -> Vec<PathBuf> {
        let mut files = vec![self.system("/etc/paths")];
        if let Ok(list) = read::entries(&self.system("/etc/paths.d"), 256) {
            let mut extras: Vec<_> = list
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .collect();
            extras.sort();
            files.extend(extras.into_iter().take(32));
        }
        let mut folders = Vec::new();
        for text in files.iter().filter_map(|file| read_text(file, 64 * 1024)) {
            if words {
                folders.extend(text.split_whitespace().map(|path| self.system(path)));
            } else {
                folders.extend(
                    text.lines()
                        .take(64)
                        .map(str::trim)
                        .filter(|path| path.starts_with('/'))
                        .map(|path| self.system(path)),
                );
            }
        }
        folders
    }

    fn effective_path(&self) -> Vec<PathBuf> {
        let mut folders = self.path.clone();
        if self.platform == Platform::Mac {
            folders.extend(self.mac_paths(true));
            folders.push(self.system("/opt/homebrew/bin"));
            folders.push(self.system("/usr/local/bin"));
        }
        folders.retain(|folder| folder.is_absolute());
        unique(folders)
    }
}

fn unique<T: std::cmp::Eq + std::hash::Hash + Clone>(mut values: Vec<T>) -> Vec<T> {
    let mut seen = std::collections::HashSet::new();
    values.retain(|value| seen.insert(value.clone()));
    values
}

fn real(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        path.metadata()
            .is_ok_and(|info| info.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

fn read_text(path: &Path, cap: u64) -> Option<String> {
    let Read::Bytes(bytes, _) = read::read(path, &ReadPolicy::capped(cap)) else {
        return None;
    };
    String::from_utf8(bytes).ok()
}

fn read_json(path: &Path, cap: u64) -> Option<Value> {
    serde_json::from_str(&read_text(path, cap)?).ok()
}

static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"[0-9]+\.[0-9]+(?:\.[0-9]+){0,3}(?:[-+][0-9A-Za-z.+-]+)?")
        .expect("app version pattern")
});

fn version(text: Option<&str>) -> Option<String> {
    let text = text.filter(|text| text.len() <= 65_536)?;
    // A greedy match already consumes the trailing ASCII digits. It starts at the
    // beginning of the leftmost run that can satisfy the Python version expression.
    // The regex crate has no look-around, so Python's `(?<!\d)` / `(?![0-9])`
    // guards are applied on the match bounds.
    let value = VERSION.find(text)?;
    let bytes = text.as_bytes();
    if value.start() > 0 && bytes[value.start() - 1].is_ascii_digit() {
        return None;
    }
    if value.end() < bytes.len() && bytes[value.end()].is_ascii_digit() {
        return None;
    }
    Some(value.as_str().to_owned())
}

fn checked_version(value: Option<&str>, pattern: &regex::Regex, max: usize) -> Option<String> {
    value
        .filter(|value| value.len() <= max && pattern.is_match(value))
        .map(str::to_owned)
}

#[derive(Default)]
struct Install {
    manager: Option<&'static str>,
    version: Option<String>,
    parts: Option<Value>,
    strays: Vec<Value>,
}

impl Install {
    fn new(manager: &'static str, version: Option<String>) -> Self {
        Self {
            manager: Some(manager),
            version,
            ..Default::default()
        }
    }
}

fn row(app_id: &str, install: Option<Install>) -> Value {
    let installed = install.is_some();
    let install = install.unwrap_or_default();
    let mut value = json!({
        "appId": app_id, "installed": installed, "version": install.version, "manager": install.manager
    });
    if let Some(parts) = install.parts {
        value["parts"] = parts;
    }
    if !install.strays.is_empty() {
        value["strays"] = Value::Array(install.strays);
    }
    value
}

#[derive(Debug)]
enum ProbeResult {
    Output(String),
    Failed(String),
    Timeout,
}

impl ProbeResult {
    fn output(&self) -> Option<&str> {
        match self {
            Self::Output(text) => Some(text),
            _ => None,
        }
    }

    fn version(&self) -> Option<String> {
        version(self.output())
    }
}

trait Probe {
    async fn run(&self, program: &Path, args: &[OsString], seconds: u64) -> ProbeResult;

    async fn version(&self, program: &Path) -> Option<String> {
        self.run(program, &["--version".into()], 20).await.version()
    }
}

struct NativeProbe;
impl Probe for NativeProbe {
    async fn run(&self, program: &Path, args: &[OsString], seconds: u64) -> ProbeResult {
        use ProbeResult::{Failed, Output, Timeout};
        let mut command = tokio::process::Command::new(program);
        command
            .args(args)
            .kill_on_drop(true)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("MUSE_NO_AUTO_UPDATE", "1")
            .env("AGY_CLI_DISABLE_AUTO_UPDATE", "true")
            .env("DISABLE_AUTOUPDATER", "1")
            .env("NO_UPDATE_NOTIFIER", "1")
            .env("CODEX_NON_INTERACTIVE", "1");
        strip_dashboard_secrets(&mut command);
        #[cfg(windows)]
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        if !program.is_absolute() {
            return Failed(String::new());
        }
        let Ok(mut child) = command.spawn() else {
            return Failed(String::new());
        };
        let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Failed(String::new());
        };
        let capture = async {
            let (out, err, status) =
                tokio::try_join!(capped(stdout), capped(stderr), child.wait())?;
            Ok::<_, std::io::Error>(if status.success() {
                Output(out)
            } else {
                Failed(err)
            })
        };
        match tokio::time::timeout(Duration::from_secs(seconds), capture).await {
            Ok(Ok(result)) => result,
            Ok(Err(_)) => Failed(String::new()),
            Err(_) => Timeout,
        }
    }
}

fn strip_dashboard_secrets(command: &mut tokio::process::Command) {
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name == "CCS_SESSION_SECRET"
            || name.starts_with("CCS_DASHBOARD_")
            || name.ends_with("_PASSWORD_HASH")
        {
            command.env_remove(key);
        }
    }
}

async fn capped(stream: impl tokio::io::AsyncRead + Unpin) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    stream.take(65_537).read_to_end(&mut bytes).await?;
    if bytes.len() > 65_536 {
        return Err(std::io::Error::other("inventory probe output too large"));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn args(values: &[&str]) -> Vec<OsString> {
    values.iter().map(OsString::from).collect()
}

fn ps_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

fn ps_command(script: String) -> Vec<OsString> {
    // Windows PowerShell 5.1 otherwise writes to a pipe using the OEM code page.
    let script =
        format!("[Console]::OutputEncoding=(New-Object System.Text.UTF8Encoding $false); {script}");
    let utf16: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut argv = args(&["-NoProfile", "-NonInteractive", "-EncodedCommand"]);
    argv.push(STANDARD.encode(utf16).into());
    argv
}

async fn inventory(host: &Host, probe: &impl Probe) -> Value {
    let (
        agy,
        muse,
        omp,
        codex,
        codex_desktop,
        claude,
        claude_desktop,
        zcode,
        adapters,
        t3,
        cursor,
        acp,
    ) = tokio::join!(
        cli::detect(host, probe, "agy"),
        cli::detect(host, probe, "muse"),
        cli::detect(host, probe, "omp"),
        cli::detect(host, probe, "codex"),
        packages::desktop(host, probe, true),
        cli::detect(host, probe, "claude"),
        packages::desktop(host, probe, false),
        packages::zcode(host, probe),
        async { adapters::detect(host) },
        packages::t3(host, probe),
        cli::cursor(host, probe),
        async { packages::antigravity_acp(host) },
    );
    let apps: Vec<_> = APPS
        .into_iter()
        .zip([
            agy,
            muse,
            omp,
            codex,
            codex_desktop,
            claude,
            claude_desktop,
            zcode,
            adapters,
            t3,
            cursor,
            acp,
        ])
        .map(|(id, install)| row(id, install))
        .collect();
    json!({"inventory": true, "apps": apps})
}

pub(crate) async fn run_inventory(value: &Value) -> Result<Value, Failure> {
    let input = value
        .as_object()
        .ok_or_else(|| Failure::new(Cause::Refused, "invalid apps request"))?;
    if !matches!(
        input.get("platform").and_then(Value::as_str),
        Some("ubuntu" | "mac" | "windows")
    ) {
        return Err(Failure::new(Cause::Refused, "invalid apps request"));
    }
    match input.get("action").and_then(Value::as_str) {
        Some("inventory") if input.len() == 2 => {
            let Some(host) = Platform::native().and_then(Host::native) else {
                return Ok(crate::proof::fallback());
            };
            if input["platform"].as_str() != Some(host.platform.name()) {
                return Err(Failure::new(
                    Cause::Refused,
                    "app inventory platform mismatch",
                ));
            }
            Ok(inventory(&host, &NativeProbe).await)
        }
        _ => Err(Failure::new(Cause::Refused, "invalid apps action")),
    }
}

fn valid_job(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|ch| ch.is_ascii_digit() || (b'a'..=b'f').contains(&ch))
}

#[cfg(test)]
mod tests;
