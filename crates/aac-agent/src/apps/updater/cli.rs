//! The five command-line updaters (W6): antigravity-cli, muse-code, omp, codex-cli
//! and claude-code. Python oracle: `app_updates.py` (`detect_cli`, `_strays`,
//! `_current_block`, `claude_current`, `check_readiness`, `update_cli`,
//! `perform_cli_update`, `npm_view_latest` and the Muse helpers).
use super::{
    AppRow, AppUpdater, BoxFuture, CommandSpec, Context, Detection, Installed, Platform, Probe,
    Status, UpdateFailure,
    common::{version_text, version_tuple, write_private_json},
    processes::{self, CliInstances, Process, ProcessTable},
    shared::{self, Probed},
    terminal,
};
use crate::apps::{Host, read_json, real, unique};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, SystemTime},
};

/// One CLI updater: its fixed app id and command name (Python `CLI_NAMES`).
pub(super) struct Cli {
    app: &'static str,
    name: &'static str,
}

pub(super) static ANTIGRAVITY_CLI: Cli = Cli {
    app: "antigravity-cli",
    name: "agy",
};
pub(super) static MUSE: Cli = Cli {
    app: "muse-code",
    name: "muse",
};
pub(super) static OMP: Cli = Cli {
    app: "omp",
    name: "omp",
};
pub(super) static CODEX: Cli = Cli {
    app: "codex-cli",
    name: "codex",
};
pub(super) static CLAUDE: Cli = Cli {
    app: "claude-code",
    name: "claude",
};

/// The managed release folders (Python `MANAGED_RELEASES`).
fn managed_release(name: &str) -> Option<&'static str> {
    match name {
        "codex" => Some(".codex/packages/standalone/releases"),
        "claude" => Some(".local/share/claude/versions"),
        _ => None,
    }
}
const MANAGED_AGY: &str = ".local/share/ai-account-center/antigravity-runtime/bin/agy";
const STRAY_LIMIT: usize = 4;
const CODEX_NPM_PACKAGE: &str = "@openai/codex";
const PACKAGE_JSON_LIMIT: u64 = 64 * 1024;
/// Python `CODEX_BRIDGE_SECONDS`.
const CODEX_BRIDGE_SECONDS: u64 = 420;
const CLAUDE_RELEASES: &str = "https://downloads.claude.ai/claude-code-releases/";
const MUSE_QUOTA_MARKER: &str = "subscription quota exhausted";
const MUSE_QUOTA_RESET_MAX: usize = 64;

static CLAUDE_VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9]{1,6}\.[0-9]{1,6}\.[0-9]{1,6}\z").expect("claude version")
});
static MUSE_QUOTA_RESET_AT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)usage window resets at[ \t]+([^\r\n]+)").expect("muse reset")
});
static MUSE_PID: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\A[1-9][0-9]{0,8}\z").expect("muse pid"));

impl AppUpdater for Cli {
    fn app_id(&self) -> &'static str {
        self.app
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            match detect_cli(cx, self.app, self.name).await {
                Some(install) => Detection::Found(Box::new(install)),
                None => Detection::Absent,
            }
        })
    }

    fn current<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Option<AppRow>> {
        Box::pin(async move {
            if self.app != "claude-code" || !claude_may_be_current(cx, install) {
                return None;
            }
            claude_current(cx, install, latest_claude_version(cx).await)
        })
    }

    fn readiness<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<(), (Status, &'static str)>> {
        Box::pin(check_readiness(cx, install))
    }

    fn update<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>> {
        Box::pin(async move { Ok(update_cli(cx, install, self.name).await) })
    }
}

// ---------------------------------------------------------------------------
// Detection (Python `_candidates`, `_managed_copy`, `detect_cli`, `_strays`).
// ---------------------------------------------------------------------------

fn candidates(host: &Host, name: &str) -> Vec<PathBuf> {
    let home = &host.home;
    let mut paths = if host.platform == Platform::Windows {
        let exe = format!("{name}.exe");
        let local = host.local.join(name);
        let programs = host.local.join("Programs").join(name);
        vec![
            home.join(".local/bin").join(&exe),
            local.join("bin").join(&exe),
            local.join(&exe),
            programs.join(&exe),
            programs.join(format!("{name}.ps1")),
            programs.join(format!("{name}.cmd")),
            host.roaming.join("npm").join(format!("{name}.cmd")),
        ]
    } else {
        vec![
            home.join(".local/bin").join(name),
            home.join(".bun/bin").join(name),
            host.system("/opt/homebrew/bin").join(name),
            host.system("/usr/local/bin").join(name),
        ]
    };
    if let Some(located) = host.which(name) {
        // Process census/update must name the native executable, never the PATH
        // launcher shim; native version/help/update controls remain pass-through.
        if !(name == "agy"
            && host.platform == Platform::Ubuntu
            && located == home.join(MANAGED_AGY))
        {
            paths.insert(0, located);
        }
    }
    if host.platform != Platform::Windows && managed_release(name).is_some() {
        // Every PATH copy is a candidate, so a stray ahead on PATH is seen beside the managed install.
        let mut copies: Vec<PathBuf> = host
            .path
            .iter()
            .filter(|folder| folder.is_absolute())
            .map(|folder| folder.join(name))
            .collect();
        copies.extend(paths);
        paths = unique(copies);
    }
    paths
}

/// Python `_within`: `path` is `root` or lies inside it (case-folded on Windows only).
fn within(path: &Path, root: &Path, platform: Platform) -> bool {
    if platform == Platform::Windows {
        let (path, root) = (
            path.to_string_lossy().to_lowercase(),
            root.to_string_lossy().to_lowercase(),
        );
        return path == root
            || path
                .strip_prefix(&root)
                .is_some_and(|rest| rest.starts_with(['\\', '/']));
    }
    path.starts_with(root)
}

fn managed_copy<'a>(host: &Host, name: &str, found: &'a [PathBuf]) -> Option<&'a PathBuf> {
    if host.platform == Platform::Windows {
        return None;
    }
    let release = real(&host.home.join(managed_release(name)?));
    found
        .iter()
        .find(|path| within(&real(path), &release, host.platform))
}

fn package_json(folder: &Path) -> Option<serde_json::Map<String, Value>> {
    match read_json(&folder.join("package.json"), PACKAGE_JSON_LIMIT) {
        Some(Value::Object(info)) => Some(info),
        _ => None,
    }
}

fn codex_package(folder: &Path) -> Option<serde_json::Map<String, Value>> {
    package_json(folder)
        .filter(|info| info.get("name").and_then(Value::as_str) == Some(CODEX_NPM_PACKAGE))
}

/// Python `_npm_codex_root`: the package folder of a POSIX npm-global Codex CLI.
fn npm_codex_root(path: &Path, resolved: &Path, platform: Platform) -> Option<PathBuf> {
    let bin = path.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let root = bin.parent()?.join("lib/node_modules/@openai/codex");
    if !within(resolved, &real(&root), platform) {
        return None;
    }
    codex_package(&root).map(|_| root)
}

/// Python `npm_prefix`.
fn npm_prefix(install: &Installed) -> PathBuf {
    let folder = install.path.parent().unwrap_or(Path::new(""));
    if install.platform == Platform::Windows {
        folder.to_path_buf()
    } else {
        folder.parent().unwrap_or(Path::new("")).to_path_buf()
    }
}

fn lower_name(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn powershell_file(cx: &Context, script: &Path) -> Vec<OsString> {
    vec![
        shared::powershell_path(&cx.host).into_os_string(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script.as_os_str().to_owned(),
    ]
}

/// Python `detect_cli`. `None` means not installed.
pub(super) async fn detect_cli(cx: &Context, app: &'static str, name: &str) -> Option<Installed> {
    let host = &cx.host;
    let platform = host.platform;
    let found: Vec<PathBuf> = candidates(host, name)
        .into_iter()
        .filter(|path| path.is_file())
        .collect();
    let managed = managed_copy(host, name, &found).cloned();
    let path = managed.clone().or_else(|| found.first().cloned())?;
    let installed = |path: PathBuf, version: Option<String>, manager| {
        Installed::new(app, platform, path, version, manager)
    };
    let unsupported = |path: PathBuf| installed(path, None, "unsupported");
    if platform == Platform::Windows && app == "muse-code" {
        // Meta's install.ps1 writes only muse.cmd (a hand-added `muse` shim is also
        // accepted). Probe its PowerShell launcher directly: CreateProcess cannot run .cmd.
        let expected = host.local.join("Programs/muse");
        let launcher = expected.join(".muse-launcher.ps1");
        if path
            .parent()
            .is_none_or(|parent| real(parent) != real(&expected))
            || !matches!(lower_name(&path).as_str(), "muse" | "muse.cmd")
            || !launcher.is_file()
        {
            return Some(unsupported(path));
        }
        let mut argv = powershell_file(cx, &launcher);
        argv.push("--version".into());
        let spec = CommandSpec::new(argv)
            .timeout(Duration::from_secs(20))
            .capture()
            .quiet();
        let mut install = installed(expected.join("muse.cmd"), None, "native");
        match cx.run(&spec).await {
            Ok(output) => {
                install.version =
                    version_text(output.stdout.rsplit('(').next().unwrap_or_default());
            }
            Err(failure) => {
                install.probe = if failure.code == "timeout" {
                    Probe::Timeout
                } else {
                    Probe::Failed
                };
            }
        }
        return Some(install);
    }
    if platform == Platform::Windows
        && path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
    {
        if app != "codex-cli" {
            return Some(unsupported(path));
        }
        let root = path
            .parent()
            .unwrap_or(Path::new(""))
            .join("node_modules/@openai/codex");
        let mut install = installed(path, None, "npm");
        match package_json(&root) {
            Some(info) if info.get("name").and_then(Value::as_str) == Some(CODEX_NPM_PACKAGE) => {
                install.version = info
                    .get("version")
                    .and_then(Value::as_str)
                    .and_then(version_text);
                install.package_root = Some(root);
                if install.version.is_none() {
                    install.probe = Probe::Failed;
                }
            }
            _ => install.probe = Probe::Failed,
        }
        return Some(install);
    }
    let version = match shared::cli_probe(cx, &path, &["--version"]).await {
        Probed::Version(Some(version)) => version,
        probed => {
            let mut install = installed(path, None, "native");
            install.probe = if probed == Probed::Timeout {
                Probe::Timeout
            } else {
                Probe::Failed
            };
            return Some(install);
        }
    };
    let resolved = real(&path);
    let home = &host.home;
    // Python: `str(home / root) in str(resolved)`, case-folded on Windows. The resolved
    // form of the root is accepted too, for a home reached through a link.
    let resolved_text = |root: &Path| {
        let text = resolved.to_string_lossy().into_owned();
        [root.to_path_buf(), real(root)].iter().any(|root| {
            let root = root.to_string_lossy();
            if platform == Platform::Windows {
                text.to_lowercase().contains(&root.to_lowercase())
            } else {
                text.contains(root.as_ref())
            }
        })
    };
    let npm_root = (app == "codex-cli" && platform != Platform::Windows)
        .then(|| npm_codex_root(&path, &resolved, platform))
        .flatten();
    let standalone = home.join(".codex/packages/standalone/releases");
    let claude = home.join(".local/share/claude/versions");
    let mut manager = "native";
    let package_root = if app == "codex-cli" && resolved_text(&standalone) {
        Some(standalone)
    } else if app == "claude-code" && resolved_text(&claude) {
        Some(claude)
    } else if let Some(root) = npm_root {
        manager = "npm";
        Some(root)
    } else {
        None
    };
    let strays = match &managed {
        Some(managed) => strays(cx, &found, managed).await,
        None => Vec::new(),
    };
    let mut install = installed(path, Some(version), manager);
    install.package_root = package_root;
    install.strays = strays;
    Some(install)
}

/// Python `_stray_location`: one fixed word, never the folder.
fn stray_location(host: &Host, path: &Path, resolved: &Path) -> &'static str {
    if path.starts_with(host.system("/usr/local")) {
        "usr-local"
    } else if path.starts_with(host.system("/opt/homebrew"))
        || path.starts_with(host.system("/home/linuxbrew"))
    {
        "homebrew"
    } else if path.starts_with(host.home.join(".bun")) {
        "bun"
    } else if path.starts_with(&host.home) && resolved.to_string_lossy().contains("node_modules") {
        "user-npm"
    } else {
        "other"
    }
}

/// Python `_stray_version`: an npm-script copy reports its package version and is never run.
async fn stray_version(cx: &Context, path: &Path, resolved: &Path) -> Option<String> {
    if resolved.file_name().is_some_and(|name| name == "codex.js")
        && let Some(bin) = resolved.parent()
        && bin.file_name().is_some_and(|name| name == "bin")
        && let Some(root) = bin.parent()
        && root.ends_with("node_modules/@openai/codex")
        && let Some(info) = codex_package(root)
    {
        return info
            .get("version")
            .and_then(Value::as_str)
            .and_then(version_text);
    }
    match shared::cli_probe(cx, path, &["--version"]).await {
        Probed::Version(version) => version,
        _ => None,
    }
}

/// Python `_effective_path_folders`: PATH, plus the Mac system folders, first occurrence wins.
fn effective_folders(host: &Host) -> Vec<PathBuf> {
    let mut folders: Vec<PathBuf> = host
        .path
        .iter()
        .filter(|path| path.is_absolute())
        .cloned()
        .collect();
    if host.platform == Platform::Mac {
        folders.extend(host.mac_paths(false).into_iter().take(64));
    }
    unique(folders)
}

/// Python `_strays`: other copies resolving to a different file, each probed once, side by side.
async fn strays(cx: &Context, found: &[PathBuf], managed: &Path) -> Vec<Value> {
    let host = &cx.host;
    let mut seen = std::collections::HashSet::from([real(managed)]);
    let mut copies = Vec::new();
    for path in found {
        let resolved = real(path);
        if copies.len() < STRAY_LIMIT && seen.insert(resolved.clone()) {
            copies.push((path.clone(), resolved));
        }
    }
    if copies.is_empty() {
        return Vec::new();
    }
    let folders: Vec<PathBuf> = effective_folders(host).iter().map(|f| real(f)).collect();
    let position = |path: &Path| {
        let parent = real(path.parent()?);
        folders.iter().position(|folder| *folder == parent)
    };
    let managed_at = position(managed);
    let versions = futures_join(
        copies
            .iter()
            .map(|(path, resolved)| stray_version(cx, path, resolved)),
    )
    .await;
    copies
        .iter()
        .zip(versions)
        .map(|((path, resolved), version)| {
            let at = position(path);
            json!({
                "location": stray_location(host, path, resolved),
                "version": version,
                "shadows": at.is_some_and(|at| managed_at.is_none_or(|managed_at| at < managed_at)),
            })
        })
        .collect()
}

/// Run up to four futures side by side, keeping their order.
async fn futures_join<F: std::future::Future>(items: impl Iterator<Item = F>) -> Vec<F::Output> {
    let mut handles: Vec<std::pin::Pin<Box<F>>> = items.map(Box::pin).collect();
    let mut outputs: Vec<Option<F::Output>> = handles.iter().map(|_| None).collect();
    std::future::poll_fn(|context| {
        let mut pending = false;
        for (handle, output) in handles.iter_mut().zip(outputs.iter_mut()) {
            if output.is_none() {
                match handle.as_mut().poll(context) {
                    std::task::Poll::Ready(value) => *output = Some(value),
                    std::task::Poll::Pending => pending = true,
                }
            }
        }
        if pending {
            std::task::Poll::Pending
        } else {
            std::task::Poll::Ready(())
        }
    })
    .await;
    outputs.into_iter().flatten().collect()
}

/// Python `_current_block`: why this install must not be called current.
fn current_block(install: &Installed) -> Option<&'static str> {
    for stray in &install.strays {
        if stray["shadows"] != true {
            continue;
        }
        let version = stray["version"].as_str().unwrap_or_default();
        let installed = install.version.as_deref().unwrap_or_default();
        if version.is_empty()
            || installed.is_empty()
            || version_tuple(version).is_empty()
            || version_tuple(installed).is_empty()
        {
            return Some("unknown");
        }
        if version_tuple(version) < version_tuple(installed) {
            return Some("older");
        }
    }
    None
}

/// Python `_blocked_current`.
fn blocked_current(
    install: &Installed,
    before: Option<&str>,
    attempted: bool,
    restarted: u32,
) -> Option<AppRow> {
    let (status, code) = match current_block(install)? {
        "older" => (Status::ActionRequired, "older_copy_first"),
        _ => (Status::Unknown, "readiness_unknown"),
    };
    let mut row = AppRow::of(install, status, before, install.version.as_deref(), code);
    row.update_attempted = attempted;
    row.restarted_processes = restarted;
    Some(row)
}

// ---------------------------------------------------------------------------
// Claude's official channel pointers (Python `claude_pointer`, `claude_current`).
// ---------------------------------------------------------------------------

async fn claude_pointer(cx: &Context, channel: &str) -> Option<String> {
    let url = format!("{CLAUDE_RELEASES}{channel}");
    let raw = cx
        .system
        .fetch(&url, CLAUDE_RELEASES, 64, Duration::from_secs(10))
        .await
        .ok()?;
    if raw.len() > 64 || !raw.is_ascii() {
        return None;
    }
    let text = String::from_utf8(raw).ok()?;
    let text = text.trim();
    CLAUDE_VERSION.is_match(text).then(|| text.to_owned())
}

/// Python `latest_claude_version`: the newer of `latest` and `stable`; `None` unless both answer.
async fn latest_claude_version(cx: &Context) -> Option<String> {
    let (latest, stable) = tokio::join!(claude_pointer(cx, "latest"), claude_pointer(cx, "stable"));
    let (latest, stable) = (latest?, stable?);
    Some(if version_tuple(&stable) > version_tuple(&latest) {
        stable
    } else {
        latest
    })
}

/// Python `claude_current`'s checks before it reads the pointers: native Claude Code
/// with a plain version, no older or unreadable copy running first, no pending restart.
fn claude_may_be_current(cx: &Context, install: &Installed) -> bool {
    install.app_id == "claude-code"
        && install.manager == "native"
        && install
            .version
            .as_deref()
            .is_some_and(|version| CLAUDE_VERSION.is_match(version))
        && current_block(install).is_none()
        && !cx
            .host
            .home
            .join(".ccs/app-updates/claude-code-pending-restart.json")
            .exists()
}

/// Python `claude_current`: `current` only when native Claude Code is at least the newest build.
fn claude_current(cx: &Context, install: &Installed, newest: Option<String>) -> Option<AppRow> {
    if !claude_may_be_current(cx, install) {
        return None;
    }
    let before = install.version.as_deref()?;
    let newest = newest?;
    if version_tuple(before) < version_tuple(&newest) {
        return None;
    }
    Some(AppRow::of(
        install,
        Status::Current,
        Some(before),
        Some(before),
        "current",
    ))
}

// ---------------------------------------------------------------------------
// Readiness (Python `check_readiness`, CLI part).
// ---------------------------------------------------------------------------

async fn check_readiness(cx: &Context, install: &Installed) -> Result<(), (Status, &'static str)> {
    if install.manager == "unsupported" {
        return Err((Status::Failed, "unsupported"));
    }
    if install.app_id == "muse-code" && install.platform == Platform::Windows {
        // Never stopped or restarted on Windows (see update_cli), so nothing to check.
        return Ok(());
    }
    let judged = async {
        let table = processes::scan(cx).await?;
        let found = processes::cli_instances(cx, install, &table).await?;
        terminal::check_terminal(cx, &found.contexts).await
    };
    match judged.await {
        Ok(()) => Ok(()),
        Err(failure) if failure.code == "readiness_unknown" => {
            Err((Status::Unknown, "readiness_unknown"))
        }
        Err(failure) => Err((Status::Failed, failure.code)),
    }
}

// ---------------------------------------------------------------------------
// Muse (Python `resolve_muse_shell`, `muse_update_busy`, the catalog warm).
// ---------------------------------------------------------------------------

fn resolve_muse_shell(host: &Host) -> Result<Option<PathBuf>, UpdateFailure> {
    if host.platform == Platform::Windows {
        return Ok(None);
    }
    let fallback = host.system("/bin/bash");
    host.which("bash")
        .or_else(|| fallback.is_file().then_some(fallback))
        .map(Some)
        .ok_or_else(|| UpdateFailure::new("unsupported"))
}

/// Python `_pid_alive`.
fn pid_alive(pid: u32) -> bool {
    #[cfg(unix)]
    {
        let Ok(pid) = i32::try_from(pid) else {
            return false;
        };
        match nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None) {
            Ok(()) => true,
            Err(nix::errno::Errno::EPERM) => true,
            Err(_) => false,
        }
    }
    #[cfg(windows)]
    {
        windows_alive::pid_alive(pid)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = pid;
        false
    }
}

#[cfg(windows)]
#[allow(unsafe_code)]
mod windows_alive {
    use windows_sys::Win32::{
        Foundation::CloseHandle,
        System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };

    /// Python `_pid_alive` on Windows: the process exists and has not exited (STILL_ACTIVE).
    pub(super) fn pid_alive(pid: u32) -> bool {
        // SAFETY: OpenProcess has no pointer arguments; a null result is checked.
        let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
        if handle.is_null() {
            return false;
        }
        let mut status = 0u32;
        // SAFETY: the handle is valid and `status` outlives the call.
        let running = unsafe { GetExitCodeProcess(handle, &mut status) } != 0 && status == 259;
        // SAFETY: the handle came from OpenProcess and is closed once.
        unsafe { CloseHandle(handle) };
        running
    }
}

/// Python `muse_update_busy`: the launcher's own updater holds its lock with a live PID.
fn muse_update_busy(directory: &Path) -> bool {
    let path = directory.join(".muse-update-lock/pid");
    let Some(text) = crate::apps::read_text(&path, 64) else {
        return false;
    };
    let text = text.trim();
    MUSE_PID.is_match(text) && text.parse().is_ok_and(pid_alive)
}

fn muse_catalog_dir(host: &Host) -> PathBuf {
    host.home.join(".local/share/muse/model-catalog")
}

/// Python `muse_catalog_fresh`: a catalog `*.json` written at or after `started`.
fn muse_catalog_fresh(host: &Host, started: SystemTime) -> bool {
    let Ok(entries) = std::fs::read_dir(muse_catalog_dir(host)) else {
        return false;
    };
    entries.filter_map(Result::ok).any(|entry| {
        let path = entry.path();
        path.extension().is_some_and(|ext| ext == "json")
            && entry
                .metadata()
                .is_ok_and(|info| info.is_file() && info.modified().is_ok_and(|at| at >= started))
    })
}

/// Python `muse_quota_reset`: (quota, reset) from a warm failure's own words.
fn muse_quota_reset(output: &str) -> (bool, Option<String>) {
    if !output.to_lowercase().contains(MUSE_QUOTA_MARKER) {
        return (false, None);
    }
    let reset = MUSE_QUOTA_RESET_AT
        .captures(output)
        .map(|found| found[1].trim().to_owned());
    match reset {
        Some(reset) if reset.is_empty() || reset.chars().count() > MUSE_QUOTA_RESET_MAX => {
            (true, None)
        }
        reset => (true, reset),
    }
}

/// Python `muse_warm_catalog`: one headless session so the new binary refreshes its catalog.
async fn muse_warm_catalog(
    cx: &Context,
    refreshed: &Installed,
    started: SystemTime,
) -> Result<bool, UpdateFailure> {
    let binary = &refreshed.path;
    if !binary.is_file() {
        return Ok(false);
    }
    let words = [
        "exec",
        "--contributor-mode",
        "off",
        super::desktop::MUSE_WARM_PROMPT,
    ];
    let argv: Vec<OsString> = if refreshed.platform == Platform::Windows {
        let launcher = binary.with_file_name(".muse-launcher.ps1");
        if !launcher.is_file() {
            return Ok(false);
        }
        let mut argv = powershell_file(cx, &launcher);
        argv.extend(words.iter().map(OsString::from));
        argv
    } else {
        let mut argv = vec![binary.as_os_str().to_owned()];
        argv.extend(words.iter().map(OsString::from));
        argv
    };
    let remaining = cx.remaining();
    if remaining.is_zero() {
        return Ok(false);
    }
    let warmed = async {
        let folder = shared::private_temporary(cx)?;
        let timeout = Duration::from_secs(60).min(cx.remaining());
        if timeout.is_zero() {
            return Ok(false);
        }
        if refreshed.platform == Platform::Mac {
            mac_muse_warm(cx, timeout).await?;
        } else {
            let spec = CommandSpec::new(argv)
                .timeout(timeout)
                .cwd(folder.path())
                .kill_tree();
            cx.run(&spec).await?;
        }
        Ok::<bool, UpdateFailure>(true)
    };
    match warmed.await {
        Ok(false) => Ok(false),
        Ok(true) => Ok(muse_catalog_fresh(&cx.host, started)),
        Err(failure) if failure.code.starts_with("mac_") => Err(failure),
        Err(failure) => {
            let (quota, reset) = muse_quota_reset(&failure.output);
            if quota {
                // Muse is installed and signed in; only its quota is used up.
                return Err(UpdateFailure {
                    quota_reset: reset,
                    ..UpdateFailure::new("muse_quota_exhausted")
                });
            }
            Ok(false)
        }
    }
}

/// The Mac warm runs in the user's GUI session (Python `app_update_macos.gui_command`,
/// W2's `desktop::mac_gui_command`).
async fn mac_muse_warm(cx: &Context, timeout: Duration) -> Result<(), UpdateFailure> {
    super::desktop::mac_gui_command(cx, super::desktop::GuiAction::MuseWarm, timeout).await
}

// ---------------------------------------------------------------------------
// The installers (Python `npm_view_latest`, `perform_cli_update`).
// ---------------------------------------------------------------------------

/// Python `_clamp(seconds, deadline)`: at most the deadline's remaining time, never under 30 s.
fn clamp30(cx: &Context, seconds: u64) -> Duration {
    Duration::from_secs(seconds.min(cx.remaining().as_secs()).max(30))
}

async fn npm_view_latest(cx: &Context, install: &Installed) -> Option<String> {
    let (node, cli) = shared::resolve_npm(&cx.host, install.path.parent()?)?;
    let spec = CommandSpec::new([
        node.into_os_string(),
        cli.into_os_string(),
        "view".into(),
        CODEX_NPM_PACKAGE.into(),
        "version".into(),
    ])
    .timeout(Duration::from_secs(60))
    .capture();
    let text = cx.run(&spec).await.ok()?.stdout;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

async fn perform_cli_update(cx: &Context, install: &Installed) -> Result<(), UpdateFailure> {
    let path = &install.path;
    let host = &cx.host;
    let windows = install.platform == Platform::Windows;
    if install.app_id == "muse-code" {
        let expected = if windows {
            host.local.join("Programs/muse/muse.cmd")
        } else {
            host.home.join(".local/bin/muse")
        };
        if real(path) != real(&expected)
            || windows && !path.with_file_name(".muse-launcher.ps1").is_file()
        {
            return Err(UpdateFailure::new("unsupported"));
        }
        let shell = resolve_muse_shell(host)?;
        let folder = shared::private_temporary(cx)?;
        let (url, target) = if windows {
            (
                "https://dev.meta.ai/install.ps1",
                folder.path().join("muse-install.ps1"),
            )
        } else {
            (
                "https://dev.meta.ai/install.sh",
                folder.path().join("muse-install.sh"),
            )
        };
        cx.system
            .download(url, &target, 512 * 1024, Duration::from_secs(60))
            .await?;
        let spec = if windows {
            CommandSpec::new(powershell_file(cx, &target))
                .timeout(clamp30(cx, 180))
                .env(
                    "MUSE_INSTALL_DIR",
                    path.parent().unwrap_or(Path::new("")).as_os_str(),
                )
        } else {
            let shell = shell.ok_or_else(|| UpdateFailure::new("unsupported"))?;
            CommandSpec::new([shell.into_os_string(), target.into_os_string()])
                .timeout(clamp30(cx, 600))
        }
        .env("MUSE_UPGRADE_MODE", "1")
        .env("MUSE_NO_MODIFY_PATH", "1");
        cx.run(&spec).await?;
        return Ok(());
    }
    if install.manager == "npm" {
        // npm itself, under the install's own prefix: never `codex update`, which runs whichever npm PATH finds.
        let prefix = npm_prefix(install);
        let (node, cli) =
            shared::resolve_npm(host, &prefix).ok_or_else(|| UpdateFailure::new("unsupported"))?;
        let spec = CommandSpec::new([
            node.into_os_string(),
            cli.into_os_string(),
            "install".into(),
            "--global".into(),
            "--prefix".into(),
            prefix.into_os_string(),
            "@openai/codex@latest".into(),
        ])
        .timeout(clamp30(cx, 300));
        cx.run(&spec).await?;
        return Ok(());
    }
    if install.manager != "native" {
        return Err(UpdateFailure::new("unsupported"));
    }
    let mut search = vec![path.parent().unwrap_or(Path::new("")).to_path_buf()];
    search.extend(std::env::split_paths(
        &std::env::var_os("PATH").unwrap_or_default(),
    ));
    let joined = std::env::join_paths(search).map_err(|_| UpdateFailure::failed())?;
    let spec = CommandSpec::new([path.as_os_str().to_owned(), "update".into()])
        .timeout(Duration::from_secs(180))
        .env("PATH", joined);
    cx.run(&spec).await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// The update itself (Python `update_cli`).
// ---------------------------------------------------------------------------

/// What `update_cli`'s exception handlers need to know about a run that stopped early.
#[derive(Default)]
struct Run {
    attempted: bool,
    changed: bool,
    pre_stopped: bool,
    pre_forced: u32,
    forced_background: bool,
    refreshed: Option<String>,
    contexts: Vec<Process>,
}

fn pending_path(host: &Host, app: &str) -> PathBuf {
    host.home
        .join(".ccs/app-updates")
        .join(format!("{app}-pending-restart.json"))
}

fn mark_pending(path: &Path, version: &str) {
    let _ = write_private_json(path, &json!({"version": version}));
}

fn unmark(path: &Path) {
    let _ = std::fs::remove_file(path);
}

fn row(
    install: &Installed,
    status: Status,
    before: Option<&str>,
    after: Option<&str>,
    code: Option<&str>,
    attempted: bool,
    restarted: u32,
) -> AppRow {
    let mut row = AppRow::of(
        install,
        status,
        before,
        after,
        code.unwrap_or(status.as_str()),
    );
    row.update_attempted = attempted;
    row.restarted_processes = restarted;
    row
}

/// Python `outcome(row)`: a forced Windows npm Codex stop names itself.
fn outcome(run: &Run, mut row: AppRow) -> AppRow {
    if run.forced_background {
        row.message_code = Some(
            if matches!(row.status, Status::Failed | Status::RestartFailed) {
                "codex_force_failed"
            } else {
                "codex_sessions_stopped"
            }
            .into(),
        );
        row.forced_stops = Some(row.forced_stops.unwrap_or(0).max(run.pre_forced));
    }
    row
}

/// Python `relaunch_after_failed_install`: best effort; the marker stays when it fails.
async fn relaunch_after_failed_install(
    cx: &Context,
    install: &Installed,
    run: &Run,
    pending: &Path,
) {
    if terminal::restart_cli(cx, install, &run.contexts)
        .await
        .is_ok()
    {
        unmark(pending);
    }
}

async fn redetect(
    cx: &Context,
    install: &Installed,
    name: &str,
) -> Result<Installed, UpdateFailure> {
    match detect_cli(cx, install.app_id, name).await {
        Some(refreshed) if refreshed.version.is_some() => Ok(refreshed),
        _ => Err(UpdateFailure::new("version_unknown")),
    }
}

async fn update_cli(cx: &Context, install: &Installed, name: &str) -> AppRow {
    let before = install.version.as_deref();
    let pending = pending_path(&cx.host, install.app_id);
    let mut run = Run::default();
    match update_steps(cx, install, name, &pending, &mut run).await {
        Ok(row) => outcome(&run, row),
        Err(failure) => {
            if run.changed {
                return outcome(
                    &run,
                    row(
                        install,
                        Status::RestartFailed,
                        before,
                        run.refreshed.as_deref(),
                        Some("restart_failed"),
                        run.attempted,
                        0,
                    ),
                );
            }
            if run.pre_stopped {
                relaunch_after_failed_install(cx, install, &run, &pending).await;
            }
            let code = if failure.code == "download_blocked" {
                "update_failed"
            } else {
                failure.code
            };
            if let Some(cause) = failure.cause {
                cx.record(|record| record.blame(cause, failure.reason));
            }
            outcome(
                &run,
                row(
                    install,
                    Status::Failed,
                    before,
                    before,
                    Some(code),
                    run.attempted,
                    0,
                ),
            )
        }
    }
}

async fn update_steps(
    cx: &Context,
    install: &Installed,
    name: &str,
    pending: &Path,
    run: &mut Run,
) -> Result<AppRow, UpdateFailure> {
    let Some(before) = install.version.as_deref() else {
        return Err(UpdateFailure::new("version_unknown"));
    };
    let platform = install.platform;
    let table = processes::scan(cx).await?;
    // Linux Codex's shared daemon needs the startup-lock and idle protocol, not an
    // idle TUI restart: the fixed bridge handles it. An npm-global Codex has no
    // AAC shared daemon and takes the ordinary CLI path below.
    if install.app_id == "codex-cli"
        && platform == Platform::Ubuntu
        && install.manager != "npm"
        && processes::family(&cx.host, install, &table, true)
            .iter()
            .any(|item| item.args.iter().any(|arg| arg == "app-server"))
    {
        return codex_daemon_update(cx, install, name, pending, &table).await;
    }
    let CliInstances {
        mut contexts,
        mut targets,
        mut background,
    } = if install.app_id == "muse-code" && platform == Platform::Windows {
        // T3's muse-acp adapter hosts `muse serve` from this folder: never stop or restart it.
        CliInstances::default()
    } else {
        // Terminal instances reopen idle. Background sessions keep running, except
        // the explicitly forced Windows npm Codex update below.
        processes::cli_instances(cx, install, &table).await?
    };
    terminal::check_terminal(cx, &contexts).await?;
    let mut newest = None;
    if install.manager == "npm" && platform == Platform::Windows {
        newest = if contexts.is_empty() && background.is_empty() {
            None
        } else {
            npm_view_latest(cx, install).await
        };
        if let Some(latest) = &newest
            && shared::npm_version_key(latest)? <= shared::npm_version_key(before)?
        {
            // Already current: never stop running sessions for a no-op.
            if !contexts.is_empty() {
                unmark(pending);
            }
            return Ok(row(
                install,
                Status::Current,
                Some(before),
                Some(before),
                None,
                false,
                0,
            ));
        }
        if !background.is_empty() && install.app_id == "codex-cli" && newest.is_none() {
            // Unknown registry metadata is no reason to interrupt work.
            return Ok(row(
                install,
                Status::Unknown,
                Some(before),
                Some(before),
                Some("readiness_unknown"),
                false,
                0,
            ));
        }
        if !background.is_empty() && install.app_id != "codex-cli" {
            // npm cannot replace files a running process holds open on Windows,
            // and a background session is never stopped: change nothing.
            return Ok(row(
                install,
                Status::ActionRequired,
                Some(before),
                Some(before),
                Some("in_use"),
                false,
                0,
            ));
        }
        if !contexts.is_empty() || !background.is_empty() {
            // Windows cannot replace a running npm tree, so mapped instances stop
            // before npm runs. Explicit Update apps forces only this installed Codex
            // npm family off Windows' locked files, never T3 itself.
            if install.app_id == "codex-cli" {
                let latest = processes::scan(cx).await?;
                let found = processes::cli_instances(cx, install, &latest).await?;
                (contexts, targets, background) = (found.contexts, found.targets, found.background);
                terminal::check_terminal(cx, &contexts).await?;
                if !background.is_empty() && newest.is_none() {
                    return Ok(row(
                        install,
                        Status::Unknown,
                        Some(before),
                        Some(before),
                        Some("readiness_unknown"),
                        false,
                        0,
                    ));
                }
                // Only direct package file holders qualify for a forced stop.
                let holders: std::collections::HashSet<u32> =
                    processes::family(&cx.host, install, &latest, false)
                        .iter()
                        .map(|item| item.pid)
                        .collect();
                let forced: Vec<Process> = background
                    .iter()
                    .filter(|item| holders.contains(&item.pid))
                    .cloned()
                    .collect();
                run.forced_background = !forced.is_empty();
                targets.extend(forced);
            }
            mark_pending(pending, before);
            run.contexts = contexts.clone();
            let stopped = processes::terminate_cli(
                cx,
                install,
                &targets,
                Duration::from_secs(5),
                !run.forced_background,
            )
            .await;
            match stopped {
                Ok(forced) => run.pre_forced = forced,
                Err(_) => {
                    if terminal::restart_cli(cx, install, &contexts).await.is_ok() {
                        unmark(pending);
                    }
                    return Ok(row(
                        install,
                        Status::Failed,
                        Some(before),
                        Some(before),
                        Some("restart_context"),
                        false,
                        0,
                    ));
                }
            }
            run.pre_stopped = true;
        }
    }
    run.contexts = contexts.clone();
    run.attempted = true;
    let started = SystemTime::now();
    perform_cli_update(cx, install).await?;
    let refreshed = redetect(cx, install, name).await?;
    let now = refreshed.version.clone().unwrap_or_default();
    if let Some(latest) = &newest
        && shared::npm_version_key(&now)? < shared::npm_version_key(latest)?
    {
        return Err(UpdateFailure::new("version_unknown"));
    }
    let marker_version = read_json(pending, 64 * 1024).and_then(|marker| {
        marker
            .get("version")
            .and_then(Value::as_str)
            .map(str::to_owned)
    });
    let needs_restart = marker_version.as_deref() == Some(now.as_str())
        && marker_version.as_deref() != Some(before);
    let mut catalog_ok = true;
    let mut catalog_code = "model_catalog_unrefreshed";
    let mut catalog_reset = None;
    let muse = install.app_id == "muse-code";
    let directory = install.path.parent().unwrap_or(Path::new(""));
    if muse && now != before {
        // Changed binary: never warm while the launcher's own updater holds the lock.
        if muse_update_busy(directory) {
            catalog_ok = false;
        } else {
            match muse_warm_catalog(cx, &refreshed, started).await {
                Ok(fresh) => catalog_ok = fresh,
                Err(failure) => {
                    catalog_ok = false;
                    catalog_code = failure.code;
                    catalog_reset = failure.quota_reset;
                }
            }
        }
    }
    if now == before && !needs_restart {
        if muse && muse_update_busy(directory) {
            // The launcher's own updater holds the lock, so Meta's installer skipped
            // this run. Report busy, never a false "current"; the lock stays.
            return Err(UpdateFailure::new("busy"));
        }
        if run.pre_stopped {
            // Stopped for the install; relaunch the same version. Nothing newer to retry towards.
            unmark(pending);
            let sessions = match terminal::restart_cli(cx, &refreshed, &contexts).await {
                Ok(sessions) => sessions,
                Err(_) => {
                    mark_pending(pending, &now);
                    return Ok(row(
                        install,
                        Status::RestartFailed,
                        Some(before),
                        Some(&now),
                        Some("restart_failed"),
                        true,
                        0,
                    ));
                }
            };
            let restarted = count(&contexts);
            let mut value = blocked_current(&refreshed, Some(before), true, restarted)
                .unwrap_or_else(|| {
                    row(
                        install,
                        Status::Current,
                        Some(before),
                        Some(before),
                        None,
                        true,
                        restarted,
                    )
                });
            value.restart_targets = Some(sessions);
            value.forced_stops = Some(run.pre_forced);
            return Ok(value);
        }
        if muse && !muse_catalog_fresh(&cx.host, started) {
            // Same version, stale catalog: heal it here without claiming an update.
            match muse_warm_catalog(cx, &refreshed, started).await {
                Ok(true) => {}
                Ok(false) => return Err(UpdateFailure::new("model_catalog_unrefreshed")),
                Err(failure) if failure.code == "muse_quota_exhausted" => {
                    let mut healed = row(
                        install,
                        Status::Current,
                        Some(before),
                        Some(before),
                        Some("muse_quota_current"),
                        true,
                        0,
                    );
                    set_quota_reset(&mut healed, failure.quota_reset);
                    return Ok(healed);
                }
                Err(failure) => return Err(failure),
            }
        }
        if marker_version.as_deref() == Some(before) {
            // Stale pre-stop bookkeeping from an earlier run.
            unmark(pending);
        }
        if let Some(blocked) = blocked_current(&refreshed, Some(before), true, 0) {
            return Ok(blocked);
        }
        return Ok(row(
            install,
            Status::Current,
            Some(before),
            Some(before),
            None,
            true,
            0,
        ));
    }
    run.changed = true;
    run.refreshed = Some(now.clone());
    mark_pending(pending, &now);
    let forced = if targets.is_empty() {
        0
    } else {
        processes::terminate_cli(cx, install, &targets, Duration::from_secs(5), true).await?
    } + run.pre_forced;
    let sessions = match terminal::restart_cli(cx, &refreshed, &contexts).await {
        Ok(sessions) => sessions,
        Err(_) => {
            return Ok(row(
                install,
                Status::RestartFailed,
                Some(before),
                Some(&now),
                Some("restart_failed"),
                true,
                0,
            ));
        }
    };
    // Background sessions keep the previous version until they are started again.
    let code = (!background.is_empty()).then_some("background_sessions_kept");
    let mut value = row(
        install,
        Status::Updated,
        Some(before),
        Some(&now),
        code,
        true,
        count(&contexts),
    );
    value.restart_targets = Some(sessions);
    value.forced_stops = Some(forced);
    unmark(pending);
    if !catalog_ok {
        if catalog_code == "muse_quota_exhausted" {
            // Muse updated and its sessions restarted; only the warm met a used-up quota.
            value.message_code = Some(catalog_code.into());
            set_quota_reset(&mut value, catalog_reset);
            return Ok(value);
        }
        // The binary changed but the warm did not refresh the catalog: never "updated".
        value.status = Status::RestartFailed;
        value.message_code = Some(catalog_code.into());
    }
    Ok(value)
}

fn count(contexts: &[Process]) -> u32 {
    u32::try_from(contexts.len()).unwrap_or(u32::MAX)
}

/// Muse's own printed reset time, verbatim (Python `quotaReset`).
fn set_quota_reset(row: &mut AppRow, reset: Option<String>) {
    row.quota_reset = reset;
}

/// Linux Codex with its shared app-server daemon (Python `update_cli`'s bridge branch).
async fn codex_daemon_update(
    cx: &Context,
    install: &Installed,
    name: &str,
    pending: &Path,
    table: &ProcessTable,
) -> Result<AppRow, UpdateFailure> {
    let clients = ProcessTable::new(
        table
            .items
            .iter()
            .filter(|item| !item.args.iter().any(|arg| arg == "app-server"))
            .cloned()
            .collect(),
    );
    let found = processes::cli_instances(cx, install, &clients).await?;
    terminal::check_terminal(cx, &found.contexts).await?;
    let seconds = cx.remaining().as_secs().clamp(30, CODEX_BRIDGE_SECONDS);
    let mut payload = codex_bridge(cx, install, seconds).await?;
    if payload.status == Status::Updated && !found.contexts.is_empty() {
        let refreshed = redetect(cx, install, name).await?;
        mark_pending(pending, refreshed.version.as_deref().unwrap_or_default());
        let restarted = async {
            let forced =
                processes::terminate_cli(cx, install, &found.targets, Duration::from_secs(5), true)
                    .await?;
            let sessions = terminal::restart_cli(cx, &refreshed, &found.contexts).await?;
            Ok::<_, UpdateFailure>((forced, sessions))
        };
        match restarted.await {
            Ok((forced, sessions)) => {
                payload.restarted_processes += count(&found.contexts);
                payload.restart_targets = Some(sessions);
                payload.forced_stops = Some(forced);
                unmark(pending);
            }
            Err(_) => {
                payload.status = Status::RestartFailed;
                payload.message_code = Some("restart_failed".into());
            }
        }
    }
    if payload.status == Status::Current {
        // The bridge judged only the managed copy; a shadowing copy is the one that runs.
        match current_block(install) {
            Some("older") => {
                payload.status = Status::ActionRequired;
                payload.message_code = Some("older_copy_first".into());
            }
            Some(_) => {
                payload.status = Status::Unknown;
                payload.message_code = Some("readiness_unknown".into());
            }
            None => {}
        }
    }
    Ok(payload)
}

/// Ubuntu Codex with its shared daemon: W2's port of `app_update_codex.cjs`
/// (`desktop::codex_bridge`). It takes `~/.codex/.ccs-activation.lock` itself.
async fn codex_bridge(
    cx: &Context,
    _install: &Installed,
    seconds: u64,
) -> Result<AppRow, UpdateFailure> {
    Ok(
        super::desktop::codex_bridge(cx, super::desktop::BridgeMode::Cli, None, false, seconds)
            .await,
    )
}

#[cfg(test)]
#[path = "cli_tests.rs"]
mod tests;
