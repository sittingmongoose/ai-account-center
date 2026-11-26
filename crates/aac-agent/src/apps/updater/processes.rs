//! Exact app-family process inventory, stops and desktop restart contexts
//! (Python `app_update_processes.py`). Arguments and environments stay in memory.
//! Only same-user executables of the detected installation qualify; a generic
//! node/python/process-name match is never a control target.
use super::{Context, Host, Installed, Platform, UpdateFailure};
use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};

/// T3's standalone runtime, `<versions>/<version>/t3` (Ubuntu, Nas1, the Mac).
pub(crate) const T3_RUNTIME_VERSIONS: &str = ".t3/runtime/versions";
pub(crate) const T3_MAC_BUNDLE: &str = "/Applications/T3 Code (Nightly).app";
pub(crate) const T3_WINDOWS_FOLDER: &str = "Programs/t3code";
/// Windows keeps the whole process table, at most this many entries, for ancestry only.
pub(crate) const LINEAGE_LIMIT: usize = 16_384;

static TTY_DEVICE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A/dev/(?:pts/[0-9]+|tty[A-Za-z0-9]*|console)\z").expect("tty pattern")
});

/// One scanned process (Python `Process`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Process {
    pub pid: u32,
    pub ppid: u32,
    pub uid: u32,
    /// The resolved executable path.
    pub exe: String,
    /// A start-time identity (Linux: /proc stat field 22; Mac: ps lstart; Windows: creation FILETIME).
    pub identity: String,
    pub args: Vec<String>,
    /// Filled by a context read ([`cli_context`], [`main_contexts`]).
    pub cwd: Option<String>,
    pub env: Vec<(String, String)>,
    /// Windows session id.
    pub session: Option<u32>,
}

impl Process {
    pub(crate) fn env_var(&self, key: &str) -> Option<&str> {
        self.env
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value.as_str())
    }
}

/// Scanned processes (Python `ProcessTable`). On Windows `lineage` covers every
/// process (pid -> (parent pid, start FILETIME or `None`)), whatever its name.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ProcessTable {
    pub items: Vec<Process>,
    pub lineage: Option<HashMap<u32, (u32, Option<u64>)>>,
}

impl ProcessTable {
    pub(crate) fn new(items: Vec<Process>) -> Self {
        Self {
            items,
            lineage: None,
        }
    }
}

/// The user's terminal CLI instances (Python `CliInstances`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CliInstances {
    /// Main processes to relaunch in a new terminal.
    pub contexts: Vec<Process>,
    /// Processes to stop.
    pub targets: Vec<Process>,
    /// Background sessions, never stopped or relaunched.
    pub background: Vec<Process>,
}

/// Python `scan(platform)` through the context's [`super::System`] (fixtures answer it in tests).
pub(crate) async fn scan(cx: &Context) -> Result<ProcessTable, UpdateFailure> {
    cx.system.processes(cx.platform).await
}

/// The native scan behind [`super::common::Native`].
pub(crate) async fn native_scan(platform: Platform) -> Result<ProcessTable, UpdateFailure> {
    match platform {
        Platform::Ubuntu => tokio::task::spawn_blocking(linux_processes)
            .await
            .unwrap_or_else(|_| Err(UpdateFailure::new("restart_context"))),
        #[cfg(target_os = "macos")]
        Platform::Mac => super::os_mac::processes()
            .await
            .map_err(|_| UpdateFailure::new("restart_context")),
        #[cfg(windows)]
        Platform::Windows => {
            let powershell = std::env::var_os("SystemRoot")
                .map(|root| {
                    PathBuf::from(root).join("System32/WindowsPowerShell/v1.0/powershell.exe")
                })
                .unwrap_or_else(|| PathBuf::from("powershell.exe"));
            super::os_windows::processes(powershell).await
        }
        // A host scans only its own platform.
        #[allow(unreachable_patterns)]
        _ => Err(UpdateFailure::new("restart_context")),
    }
}

/// Fields after the `comm` field of `/proc/<pid>/stat`.
fn stat_fields(pid: u32) -> std::io::Result<Vec<String>> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let rest = stat
        .rfind(')')
        .and_then(|end| stat.get(end + 2..))
        .ok_or_else(|| std::io::Error::other("stat"))?;
    Ok(rest.split_whitespace().map(str::to_owned).collect())
}

fn own_uid() -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        std::fs::metadata("/proc/self").map_or(u32::MAX, |info| info.uid())
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Python `linux_processes`: this user's live processes from /proc.
pub(crate) fn linux_processes() -> Result<ProcessTable, UpdateFailure> {
    let uid = own_uid();
    let mut values = Vec::new();
    let entries = std::fs::read_dir("/proc").map_err(|_| UpdateFailure::new("restart_context"))?;
    for entry in entries.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let one = || -> std::io::Result<Option<Process>> {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if entry.path().metadata()?.uid() != uid {
                    return Ok(None);
                }
            }
            let parts = stat_fields(pid)?;
            if parts.len() < 20 || matches!(parts[0].as_str(), "Z" | "X") {
                return Ok(None);
            }
            let exe = std::fs::read_link(format!("/proc/{pid}/exe"))?;
            let exe = exe.to_string_lossy();
            let exe = exe.strip_suffix(" (deleted)").unwrap_or(&exe).to_owned();
            let raw = std::fs::read(format!("/proc/{pid}/cmdline"))?;
            if raw.len() > 65_536 {
                return Ok(None);
            }
            let args = raw
                .split(|byte| *byte == 0)
                .filter(|item| !item.is_empty())
                .map(|item| String::from_utf8_lossy(item).into_owned())
                .collect();
            let ppid = parts[1].parse().map_err(std::io::Error::other)?;
            Ok(Some(Process {
                pid,
                ppid,
                uid,
                exe,
                identity: parts[19].clone(),
                args,
                ..Process::default()
            }))
        };
        if let Ok(Some(process)) = one() {
            values.push(process);
        }
    }
    Ok(ProcessTable::new(values))
}

/// Python `os.path.normcase(os.path.realpath(path))`.
pub(crate) fn norm(path: &Path, platform: Platform) -> PathBuf {
    let real = crate::apps::real(path);
    if platform == Platform::Windows {
        PathBuf::from(real.to_string_lossy().to_lowercase().replace('/', "\\"))
    } else {
        real
    }
}

/// Python `is_under(exe, root)`.
pub(crate) fn is_under(exe: &Path, root: &Path, platform: Platform) -> bool {
    norm(exe, platform).starts_with(norm(root, platform))
}

fn file_name_lower(path: &str) -> String {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

/// Python `family(install, processes)`: the processes running this installation.
pub(crate) fn family(
    host: &Host,
    install: &Installed,
    table: &ProcessTable,
    include_descendants: bool,
) -> Vec<Process> {
    let platform = install.platform;
    let active = norm(&install.path, platform);
    let root = install.package_root.as_deref();
    let mut entrypoints: HashSet<PathBuf> = HashSet::new();
    if install.manager == "npm"
        && let Some(root) = root
    {
        if let Some(package) = crate::apps::read_json(&root.join("package.json"), 65_536) {
            let mut names: Vec<String> = match &package["bin"] {
                serde_json::Value::Object(bins) => bins
                    .values()
                    .filter_map(|v| v.as_str().map(str::to_owned))
                    .collect(),
                serde_json::Value::String(bin) => vec![bin.clone()],
                _ => Vec::new(),
            };
            if let Some(main) = package["main"].as_str() {
                names.push(main.to_owned());
            }
            entrypoints.extend(
                names
                    .iter()
                    .map(|name| root.join(name))
                    .filter(|path| is_under(path, root, platform))
                    .map(|path| norm(&path, platform)),
            );
        }
        if install.app_id == "codex-cli" {
            entrypoints.insert(norm(&root.join("bin/codex.js"), platform));
        }
    }
    let install_name = install
        .path
        .file_name()
        .map(|name| name.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut values: Vec<Process> = Vec::new();
    for process in &table.items {
        let exe = Path::new(&process.exe);
        let exact = norm(exe, platform) == active;
        let owned_package = root.is_some_and(|root| is_under(exe, root, platform));
        let name = file_name_lower(&process.exe);
        let runtime = matches!(name.as_str(), "node" | "node.exe" | "bun" | "bun.exe")
            || name == install_name;
        let node_module = runtime
            && process.args.len() >= 2
            && entrypoints.contains(&norm(Path::new(&process.args[1]), platform));
        let muse_binary = install.app_id == "muse-code"
            && name.starts_with("muse-bin-")
            && exe.parent() == install.path.parent();
        if exact || owned_package || node_module || muse_binary {
            values.push(process.clone());
        }
    }
    if install.manager == "npm" && include_descendants {
        let mut pids: HashSet<u32> = values.iter().map(|item| item.pid).collect();
        loop {
            let children: Vec<Process> = table
                .items
                .iter()
                .filter(|item| !pids.contains(&item.pid) && pids.contains(&item.ppid))
                .cloned()
                .collect();
            if children.is_empty() {
                break;
            }
            pids.extend(children.iter().map(|item| item.pid));
            values.extend(children);
        }
    }
    // Signed Codex desktop children in its per-user staged runtime, by fixed path AND ancestry.
    if platform == Platform::Windows && install.app_id == "codex-desktop" {
        let runtime = host.local.join("OpenAI/Codex/bin");
        let mut pids: HashSet<u32> = values.iter().map(|item| item.pid).collect();
        let mut changed = true;
        while changed {
            changed = false;
            for process in &table.items {
                if !pids.contains(&process.pid)
                    && pids.contains(&process.ppid)
                    && is_under(Path::new(&process.exe), &runtime, platform)
                    && file_name_lower(&process.exe) == "codex.exe"
                {
                    values.push(process.clone());
                    pids.insert(process.pid);
                    changed = true;
                }
            }
        }
    }
    values
}

/// Python `electron_runs_as_node`: an unreadable environ counts as Node mode.
pub(crate) fn electron_runs_as_node(pid: u32) -> bool {
    let Ok(raw) = std::fs::read(format!("/proc/{pid}/environ")) else {
        return true;
    };
    let name = b"ELECTRON_RUN_AS_NODE=";
    raw.split(|byte| *byte == 0)
        .any(|entry| entry.starts_with(name) && entry.len() > name.len())
}

/// Python `desktop_roots`: the app's main processes; on Ubuntu none in Node mode.
pub(crate) fn desktop_roots(install: &Installed, processes: &[Process]) -> Vec<Process> {
    let pids: HashSet<u32> = processes.iter().map(|item| item.pid).collect();
    processes
        .iter()
        .filter(|item| !pids.contains(&item.ppid))
        .filter(|item| install.platform != Platform::Ubuntu || !electron_runs_as_node(item.pid))
        .cloned()
        .collect()
}

fn read_environ(pid: u32) -> Result<Vec<(String, String)>, UpdateFailure> {
    let raw = std::fs::read(format!("/proc/{pid}/environ"))
        .map_err(|_| UpdateFailure::new("restart_context"))?;
    if raw.len() > 1024 * 1024 {
        return Err(UpdateFailure::new("restart_context"));
    }
    Ok(raw
        .split(|byte| *byte == 0)
        .filter_map(|entry| {
            let text = String::from_utf8_lossy(entry);
            text.split_once('=')
                .map(|(key, value)| (key.to_owned(), value.to_owned()))
        })
        .collect())
}

/// Python `main_contexts`: the family's main processes with what a relaunch needs.
pub(crate) async fn main_contexts(
    cx: &Context,
    install: &Installed,
) -> Result<Vec<Process>, UpdateFailure> {
    let table = scan(cx).await?;
    let selected = family(&cx.host, install, &table, true);
    let pids: HashSet<u32> = selected.iter().map(|item| item.pid).collect();
    let mut mains: Vec<Process> = selected
        .iter()
        .filter(|item| !pids.contains(&item.ppid))
        .cloned()
        .collect();
    let context = || UpdateFailure::new("restart_context");
    if !selected.is_empty() && mains.is_empty() {
        return Err(context());
    }
    for item in &mut mains {
        if install.platform == Platform::Mac {
            let (args, env) = mac_context(cx, item.pid).await;
            item.args = args;
            item.env = env;
        }
        if item.args.is_empty() {
            return Err(context());
        }
        if install.platform == Platform::Ubuntu {
            item.env = read_environ(item.pid)?;
            if item
                .env_var("ELECTRON_RUN_AS_NODE")
                .is_some_and(|value| !value.is_empty())
            {
                continue;
            }
            item.cwd = Some(
                std::fs::read_link(format!("/proc/{}/cwd", item.pid))
                    .map_err(|_| context())?
                    .to_string_lossy()
                    .into_owned(),
            );
            let display = |key| item.env_var(key).is_some_and(|value| !value.is_empty());
            if !display("DISPLAY") && !display("WAYLAND_DISPLAY") {
                return Err(context());
            }
        }
    }
    if install.platform == Platform::Ubuntu {
        mains.retain(|item| {
            item.env_var("ELECTRON_RUN_AS_NODE")
                .is_none_or(str::is_empty)
        });
        if !selected.is_empty() && mains.is_empty() {
            return Err(context());
        }
    }
    Ok(mains)
}

/// Python `mac_context(pid)`: (arguments, environment) of a Mac process; empty when unreadable.
pub(crate) async fn mac_context(cx: &Context, pid: u32) -> (Vec<String>, Vec<(String, String)>) {
    let _ = cx;
    #[cfg(target_os = "macos")]
    {
        tokio::task::spawn_blocking(move || super::os_mac::procargs(pid))
            .await
            .unwrap_or_default()
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = pid;
        (Vec::new(), Vec::new())
    }
}

/// Python `mac_arguments(pid)`.
pub(crate) async fn mac_arguments(cx: &Context, pid: u32) -> Vec<String> {
    mac_context(cx, pid).await.0
}

/// Python `windows_context(pid)`: (cwd, environment) of this user's Windows process.
pub(crate) async fn windows_context(
    cx: &Context,
    pid: u32,
) -> Result<(String, Vec<(String, String)>), UpdateFailure> {
    let _ = cx;
    #[cfg(windows)]
    {
        tokio::task::spawn_blocking(move || super::os_windows::context(pid))
            .await
            .unwrap_or_else(|_| Err(UpdateFailure::new("restart_context")))
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Err(UpdateFailure::new("restart_context"))
    }
}

/// This process's Windows desktop session id (`None` elsewhere or when unreadable).
pub(crate) fn own_session() -> Option<u32> {
    #[cfg(windows)]
    {
        super::os_windows::own_session()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// Python `t3_servers`: PIDs running from T3's own server or desktop location.
pub(crate) fn t3_servers(host: &Host, platform: Platform, table: &ProcessTable) -> HashSet<u32> {
    if platform == Platform::Windows {
        let folder = host.local.join(T3_WINDOWS_FOLDER);
        return table
            .items
            .iter()
            .filter(|item| is_under(Path::new(&item.exe), &folder, platform))
            .map(|item| item.pid)
            .collect();
    }
    let versions = crate::apps::real(&host.home.join(T3_RUNTIME_VERSIONS));
    let bundle = host.system(T3_MAC_BUNDLE);
    table
        .items
        .iter()
        .filter(|item| {
            let executable = crate::apps::real(Path::new(&item.exe));
            (executable.file_name().is_some_and(|name| name == "t3")
                && executable.parent().and_then(Path::parent) == Some(versions.as_path()))
                || (platform == Platform::Mac && is_under(Path::new(&item.exe), &bundle, platform))
        })
        .map(|item| item.pid)
        .collect()
}

/// Python `ancestors`: item's ancestors' PIDs, nearest first. A parent that
/// started after its child, or a PID cycle, ends the chain.
pub(crate) fn ancestors(table: &ProcessTable, item: &Process) -> Vec<u32> {
    let fallback: HashMap<u32, (u32, Option<u64>)>;
    let lineage = match &table.lineage {
        Some(lineage) if !lineage.is_empty() => lineage,
        _ => {
            fallback = table
                .items
                .iter()
                .map(|row| (row.pid, (row.ppid, None)))
                .collect();
            &fallback
        }
    };
    let mut chain = Vec::new();
    let mut seen = HashSet::from([item.pid]);
    let mut current = item.ppid;
    let mut started = lineage.get(&item.pid).and_then(|entry| entry.1);
    while let Some(&(parent, created)) = lineage.get(&current) {
        if seen.contains(&current) {
            break;
        }
        if let (Some(started), Some(created)) = (started, created)
            && created > started
        {
            break;
        }
        chain.push(current);
        seen.insert(current);
        current = parent;
        started = created;
    }
    chain
}

/// Python `t3_owned`: family processes with T3's server or desktop as an ancestor.
pub(crate) fn t3_owned(host: &Host, install: &Installed, table: &ProcessTable) -> Vec<Process> {
    let selected = family(host, install, table, true);
    if selected.is_empty() {
        return Vec::new();
    }
    let servers = t3_servers(host, install.platform, table);
    if servers.is_empty() {
        return Vec::new();
    }
    selected
        .into_iter()
        .filter(|item| {
            ancestors(table, item)
                .iter()
                .any(|pid| servers.contains(pid))
        })
        .collect()
}

/// Python `user_family`: the family without T3-owned processes.
pub(crate) fn user_family(host: &Host, install: &Installed, table: &ProcessTable) -> Vec<Process> {
    let owned: HashSet<u32> = t3_owned(host, install, table)
        .iter()
        .map(|item| item.pid)
        .collect();
    family(host, install, table, true)
        .into_iter()
        .filter(|item| !owned.contains(&item.pid))
        .collect()
}

/// Python `vanished`: true once item exited or its PID names another process; false when unsure.
pub(crate) async fn vanished(cx: &Context, item: &Process) -> bool {
    if cx.platform != Platform::Ubuntu {
        return live_contexts(cx, std::slice::from_ref(item))
            .await
            .is_empty();
    }
    match stat_fields(item.pid) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => true,
        Err(_) => false,
        Ok(parts) => {
            parts.len() < 20 || matches!(parts[0].as_str(), "Z" | "X") || parts[19] != item.identity
        }
    }
}

/// Python `cli_context`: read one main process's cwd and environment (and Mac arguments).
pub(crate) async fn cli_context(cx: &Context, item: &mut Process) -> Result<(), UpdateFailure> {
    match cx.platform {
        Platform::Ubuntu => {
            item.cwd = Some(
                std::fs::read_link(format!("/proc/{}/cwd", item.pid))
                    .map_err(|_| UpdateFailure::new("restart_context"))?
                    .to_string_lossy()
                    .into_owned(),
            );
            item.env = read_environ(item.pid)?;
        }
        Platform::Mac => {
            let (args, env) = mac_context(cx, item.pid).await;
            item.args = args;
            item.env = env;
            let spec = super::CommandSpec::new([
                "/usr/sbin/lsof".to_owned(),
                "-a".into(),
                "-p".into(),
                item.pid.to_string(),
                "-d".into(),
                "cwd".into(),
                "-Fn".into(),
            ])
            .timeout(Duration::from_secs(10))
            .capture();
            let output = cx.run(&spec).await?.stdout;
            item.cwd = output
                .lines()
                .find_map(|line| line.strip_prefix('n').filter(|path| path.starts_with('/')))
                .map(str::to_owned);
        }
        Platform::Windows => {
            let (cwd, env) = windows_context(cx, item.pid).await?;
            item.cwd = Some(cwd);
            item.env = env;
        }
    }
    Ok(())
}

/// Python `terminal_attached`: PIDs of items attached to a terminal the user has.
pub(crate) async fn terminal_attached(cx: &Context, items: &[Process]) -> HashSet<u32> {
    let mut attached = HashSet::new();
    match cx.platform {
        Platform::Ubuntu => {
            for item in items {
                let fresh =
                    stat_fields(item.pid).is_ok_and(|parts| parts.get(19) == Some(&item.identity));
                let tty = |fd: u8| {
                    std::fs::read_link(format!("/proc/{}/fd/{fd}", item.pid))
                        .is_ok_and(|target| TTY_DEVICE.is_match(&target.to_string_lossy()))
                };
                if fresh && tty(0) && tty(1) {
                    attached.insert(item.pid);
                }
            }
        }
        Platform::Mac => {
            for item in items {
                let spec = super::CommandSpec::new([
                    "/usr/sbin/lsof".to_owned(),
                    "-a".into(),
                    "-p".into(),
                    item.pid.to_string(),
                    "-d".into(),
                    "0,1".into(),
                    "-F".into(),
                    "n".into(),
                ])
                .timeout(Duration::from_secs(10))
                .capture();
                let Ok(output) = cx.run(&spec).await else {
                    continue;
                };
                let mut names: HashMap<String, String> = HashMap::new();
                let mut descriptor = None;
                for line in output.stdout.lines() {
                    if let Some(fd) = line.strip_prefix('f') {
                        descriptor = Some(fd.to_owned());
                    } else if let (Some(name), Some(fd)) =
                        (line.strip_prefix('n'), descriptor.as_deref())
                        && matches!(fd, "0" | "1")
                    {
                        names.insert(fd.to_owned(), name.to_owned());
                    }
                }
                if ["0", "1"]
                    .iter()
                    .all(|fd| names.get(*fd).is_some_and(|name| TTY_DEVICE.is_match(name)))
                {
                    attached.insert(item.pid);
                }
            }
        }
        // W1 ports the Windows console-window probe next; until then nothing counts as attached,
        // so no Windows CLI instance is stopped (the Python default for an unreadable probe).
        Platform::Windows => {}
    }
    attached
}

/// Python `descendants(roots, items)`: roots plus everything below them among items.
pub(crate) fn descendants(roots: &HashSet<u32>, items: &[Process]) -> HashSet<u32> {
    let mut found = roots.clone();
    let mut grown = !found.is_empty();
    while grown {
        grown = false;
        for item in items {
            if !found.contains(&item.pid) && found.contains(&item.ppid) {
                found.insert(item.pid);
                grown = true;
            }
        }
    }
    found
}

/// Python `cli_instances(install, processes)`.
pub(crate) async fn cli_instances(
    cx: &Context,
    install: &Installed,
    table: &ProcessTable,
) -> Result<CliInstances, UpdateFailure> {
    let selected = family(&cx.host, install, table, true);
    let owned: HashSet<u32> = t3_owned(&cx.host, install, table)
        .iter()
        .map(|item| item.pid)
        .collect();
    let user: Vec<Process> = selected
        .iter()
        .filter(|item| !owned.contains(&item.pid))
        .cloned()
        .collect();
    let pids: HashSet<u32> = user.iter().map(|item| item.pid).collect();
    let mains: Vec<Process> = user
        .iter()
        .filter(|item| !pids.contains(&item.ppid))
        .cloned()
        .collect();
    let attached = terminal_attached(cx, &mains).await;
    let detached: HashSet<u32> = mains
        .iter()
        .filter(|item| !attached.contains(&item.pid))
        .map(|item| item.pid)
        .collect();
    let background: HashSet<u32> = owned
        .union(&descendants(&detached, &user))
        .copied()
        .collect();
    let mut contexts = Vec::new();
    let mut gone = HashSet::new();
    for item in mains.iter().filter(|item| attached.contains(&item.pid)) {
        let mut item = item.clone();
        let readable = cli_context(cx, &mut item).await.is_ok()
            && item
                .cwd
                .as_deref()
                .is_some_and(|cwd| !cwd.is_empty() && Path::new(cwd).is_dir());
        if readable {
            contexts.push(item);
        } else if vanished(cx, &item).await {
            gone.insert(item.pid);
        } else {
            return Err(UpdateFailure::new("restart_context"));
        }
    }
    let gone = descendants(&gone, &user);
    Ok(CliInstances {
        contexts,
        targets: user
            .iter()
            .filter(|item| !gone.contains(&item.pid) && !background.contains(&item.pid))
            .cloned()
            .collect(),
        background: selected
            .into_iter()
            .filter(|item| background.contains(&item.pid))
            .collect(),
    })
}

/// Python `same_process`.
pub(crate) fn same_process(left: &Process, right: &Process) -> bool {
    left.pid == right.pid && left.identity == right.identity && left.exe == right.exe
}

/// Python `live_contexts`: the contexts still running as the same process.
pub(crate) async fn live_contexts(cx: &Context, contexts: &[Process]) -> Vec<Process> {
    #[cfg(windows)]
    if cx.platform == Platform::Windows {
        let contexts = contexts.to_vec();
        return tokio::task::spawn_blocking(move || super::os_windows::live(&contexts))
            .await
            .unwrap_or_default();
    }
    let Ok(current) = scan(cx).await else {
        // Python lets a failed rescan raise; a context that cannot be confirmed
        // live is never signalled, so it is treated as gone here.
        return Vec::new();
    };
    contexts
        .iter()
        .filter(|item| current.items.iter().any(|other| same_process(item, other)))
        .cloned()
        .collect()
}

/// Python `safe_signal`: signal a process only while it is still the same one.
pub(crate) async fn safe_signal(cx: &Context, item: &Process, signal: i32) {
    #[cfg(unix)]
    {
        if !live_contexts(cx, std::slice::from_ref(item))
            .await
            .is_empty()
            && let (Ok(pid), Ok(signal)) = (
                i32::try_from(item.pid),
                nix::sys::signal::Signal::try_from(signal),
            )
        {
            let _ = nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), signal);
        }
    }
    #[cfg(not(unix))]
    let _ = (cx, item, signal);
}

/// Python `terminate_cli`: stop targets gracefully, then the survivors; the survivor count.
pub(crate) async fn terminate_cli(
    cx: &Context,
    install: &Installed,
    targets: &[Process],
    grace: Duration,
    graceful: bool,
) -> Result<u32, UpdateFailure> {
    let mut grace = grace;
    if install.platform == Platform::Windows {
        if graceful {
            #[cfg(windows)]
            {
                let pids: Vec<u32> = live_contexts(cx, targets)
                    .await
                    .iter()
                    .map(|item| item.pid)
                    .collect();
                let _ =
                    tokio::task::spawn_blocking(move || super::os_windows::ctrl_break(&pids)).await;
            }
        } else {
            // A background Codex can share a console group with other tools: force only checked handles.
            grace = Duration::ZERO;
        }
    } else {
        for item in live_contexts(cx, targets).await {
            safe_signal(cx, &item, 15).await;
        }
    }
    let deadline = tokio::time::Instant::now() + grace;
    while !live_contexts(cx, targets).await.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let survivors = live_contexts(cx, targets).await;
    for item in &survivors {
        if install.platform == Platform::Windows {
            #[cfg(windows)]
            super::os_windows::terminate(item)?;
            #[cfg(not(windows))]
            return Err(UpdateFailure::new("restart_failed"));
        } else {
            safe_signal(cx, item, 9).await;
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while !live_contexts(cx, targets).await.is_empty() {
        if tokio::time::Instant::now() >= deadline {
            return Err(UpdateFailure::new("restart_failed"));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(u32::try_from(survivors.len()).unwrap_or(u32::MAX))
}

/// Python `desktop_data_folder`: (the `--user-data-dir` value given, relative?).
pub(crate) fn desktop_data_folder(context: &Process, platform: Platform) -> (Option<String>, bool) {
    let windows = platform == Platform::Windows;
    let prefixes: &[&str] = if windows {
        &["--", "-", "/"]
    } else {
        &["--", "-"]
    };
    let mut value: Option<String> = None;
    for arg in context.args.iter().skip(1) {
        let arg = arg.trim_matches([' ', '\t', '\n', '\x0b', '\x0c', '\r']);
        if arg == "--" {
            break;
        }
        if let Some(prefix) = prefixes
            .iter()
            .find(|prefix| arg.starts_with(**prefix) && arg.len() > prefix.len())
        {
            let rest = &arg[prefix.len()..];
            let (name, separator, value_part) = match rest.split_once('=') {
                Some((name, value)) => (name, true, value),
                None => (rest, false, ""),
            };
            let matches = if windows {
                name.eq_ignore_ascii_case("user-data-dir")
            } else {
                name == "user-data-dir"
            };
            if matches {
                value = Some(if separator {
                    value_part.to_owned()
                } else {
                    String::new()
                });
            }
        }
    }
    match value.filter(|value| !value.is_empty()) {
        None => (None, false),
        Some(value) => {
            let relative = !is_absolute(&value, platform);
            (Some(value), relative)
        }
    }
}

fn is_absolute(value: &str, platform: Platform) -> bool {
    if platform == Platform::Windows {
        let bytes = value.as_bytes();
        (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/'))
            || value.starts_with("\\\\")
            || value.starts_with("//")
    } else {
        value.starts_with('/')
    }
}

static WINDOWS_DRIVE_RELATIVE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\A[A-Za-z]:([^\\/].*)?\z").expect("drive relative"));

/// Python `desktop_profile`: the relaunch argv and whether a given data folder can be reproduced.
pub(crate) fn desktop_profile(context: &Process, platform: Platform) -> (Vec<String>, bool) {
    let (value, relative) = desktop_data_folder(context, platform);
    let Some(mut value) = value else {
        return (Vec::new(), true);
    };
    if relative {
        let Some(cwd) = context
            .cwd
            .as_deref()
            .filter(|cwd| !cwd.is_empty() && is_absolute(cwd, platform))
        else {
            return (Vec::new(), false);
        };
        if platform == Platform::Windows && WINDOWS_DRIVE_RELATIVE.is_match(&value) {
            return (Vec::new(), false);
        }
        let separator = if platform == Platform::Windows {
            '\\'
        } else {
            '/'
        };
        value = if cwd.ends_with(['/', '\\']) {
            format!("{cwd}{value}")
        } else {
            format!("{cwd}{separator}{value}")
        };
        if !is_absolute(&value, platform) {
            return (Vec::new(), false);
        }
    }
    (vec![format!("--user-data-dir={value}")], true)
}

/// Python `desktop_arguments`.
pub(crate) fn desktop_arguments(context: &Process, platform: Platform) -> Vec<String> {
    desktop_profile(context, platform).0
}

/// Python `terminate_desktops`: close the app, then stop survivors of its family.
/// `ready` runs right before the first close request; an error there closes nothing.
pub(crate) async fn terminate_desktops(
    cx: &Context,
    install: &Installed,
    contexts: &[Process],
    grace: Duration,
    ready: impl FnOnce() -> Result<(), UpdateFailure> + Send,
) -> Result<u32, UpdateFailure> {
    let table = scan(cx).await?;
    let targets = family(&cx.host, install, &table, true);
    match install.platform {
        Platform::Ubuntu => {
            let live = live_contexts(cx, contexts).await;
            ready()?;
            for item in &live {
                safe_signal(cx, item, 15).await;
            }
        }
        Platform::Mac => {
            let live = live_contexts(cx, contexts).await;
            ready()?;
            for item in &live {
                #[cfg(target_os = "macos")]
                let closed = super::os_mac::terminate_application(item.pid);
                #[cfg(not(target_os = "macos"))]
                let closed = {
                    let _ = item;
                    false
                };
                if !closed {
                    return Err(UpdateFailure::new("restart_context"));
                }
            }
        }
        Platform::Windows => {
            let session = own_session();
            if session.is_none() || contexts.iter().any(|item| item.session != session) {
                return Err(UpdateFailure::new("restart_context"));
            }
            let live: HashSet<u32> = live_contexts(cx, contexts)
                .await
                .iter()
                .map(|item| item.pid)
                .collect();
            ready()?;
            #[cfg(windows)]
            {
                let _ =
                    tokio::task::spawn_blocking(move || super::os_windows::close_windows(&live))
                        .await;
            }
            #[cfg(not(windows))]
            let _ = live;
        }
    }
    let deadline = tokio::time::Instant::now() + grace;
    while !live_contexts(cx, &targets).await.is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    terminate_cli(cx, install, &targets, Duration::from_secs(2), true).await
}

/// Python `restart_desktops`: reopen each captured desktop; the count restarted.
pub(crate) async fn restart_desktops(
    cx: &Context,
    install: &Installed,
    contexts: &[Process],
) -> Result<u32, UpdateFailure> {
    use super::shared;
    let mut restarted = 0;
    let path = install.path.to_string_lossy().into_owned();
    for item in contexts {
        let args = desktop_arguments(item, install.platform);
        match install.platform {
            Platform::Mac => {
                let mut argv = vec![
                    "/usr/bin/open".to_owned(),
                    "-n".into(),
                    "-a".into(),
                    path.clone(),
                    "--args".into(),
                ];
                argv.extend(args);
                let mut spec = super::CommandSpec::new(argv)
                    .timeout(Duration::from_secs(15))
                    .no_stderr();
                spec.env_mode = super::common::EnvMode::Exact;
                spec.env = shared::gui_launch_environment(&item.env)
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect();
                cx.run(&spec).await?;
            }
            Platform::Windows => {
                let mut command = std::process::Command::new(&install.path);
                command
                    .args(&args)
                    .env_clear()
                    .envs(shared::environment(&item.env))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                #[cfg(windows)]
                {
                    use std::os::windows::process::CommandExt as _;
                    // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
                    command.creation_flags(0x0000_0008 | 0x0000_0200);
                }
                command
                    .spawn()
                    .map_err(|_| UpdateFailure::new("restart_failed"))?;
            }
            Platform::Ubuntu if shared::in_service_cgroup() => {
                // A plain start would stay in the dashboard's cgroup and count against its limit.
                let unit = format!(
                    "aac-launch-{}-{}",
                    shared::sanitize_unit_fragment(install.app_id),
                    unique_suffix()
                );
                let values = shared::environment(&item.env);
                let mut argv = vec![path.clone()];
                argv.extend(args);
                let wrapped = shared::systemd_user_service_argv(
                    &cx.host,
                    &unit,
                    item.cwd.as_deref(),
                    &values,
                    &argv,
                    "simple",
                    &[
                        "KillMode=process",
                        "StandardOutput=null",
                        "StandardError=null",
                    ],
                )?;
                let mut spec = super::CommandSpec::new(wrapped).timeout(Duration::from_secs(15));
                spec.env_mode = super::common::EnvMode::Exact;
                spec.env = shared::systemd_run_environment(&values)
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect();
                cx.run(&spec).await?;
            }
            Platform::Ubuntu => {
                let mut command = std::process::Command::new(&install.path);
                command
                    .args(&args)
                    .env_clear()
                    .envs(shared::environment(&item.env))
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null());
                if let Some(cwd) = &item.cwd {
                    command.current_dir(cwd);
                }
                #[cfg(unix)]
                {
                    use std::os::unix::process::CommandExt as _;
                    command.process_group(0);
                }
                command
                    .spawn()
                    .map_err(|_| UpdateFailure::new("restart_failed"))?;
            }
        }
        restarted += 1;
    }
    if contexts.is_empty() {
        return Ok(restarted);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let table = scan(cx).await?;
        if desktop_roots(install, &family(&cx.host, install, &table, true)).len() >= contexts.len()
        {
            return Ok(restarted);
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    Err(UpdateFailure::new("restart_failed"))
}

/// A short unique word for unit and session names (Python `uuid4().hex[:12]`).
pub(crate) fn unique_suffix() -> String {
    use sha2::Digest as _;
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let seed = format!(
        "{:?}{}{}",
        std::time::SystemTime::now(),
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    sha2::Sha256::digest(seed.as_bytes())
        .iter()
        .take(6)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Stop a timed-out command's owned process tree (Python `_kill_posix_tree` / `_kill_windows_tree`):
/// only descendants started at or after their parent, pinned by identity.
pub(crate) async fn kill_tree(pid: u32) {
    #[cfg(target_os = "linux")]
    {
        let Ok(table) = linux_processes() else {
            return;
        };
        let Some(root) = table.items.iter().find(|item| item.pid == pid) else {
            return;
        };
        let start = |item: &Process| item.identity.parse::<u64>().unwrap_or(0);
        let mut found = vec![root.clone()];
        let mut grown = true;
        while grown {
            grown = false;
            for item in &table.items {
                if !found.iter().any(|seen| seen.pid == item.pid)
                    && let Some(parent) = found.iter().find(|seen| seen.pid == item.ppid)
                    && start(item) >= start(parent)
                {
                    found.push(item.clone());
                    grown = true;
                }
            }
        }
        for item in found.iter().skip(1).rev() {
            let still =
                stat_fields(item.pid).is_ok_and(|parts| parts.get(19) == Some(&item.identity));
            if still && let Ok(raw) = i32::try_from(item.pid) {
                let _ = nix::sys::signal::kill(
                    nix::unistd::Pid::from_raw(raw),
                    nix::sys::signal::Signal::SIGKILL,
                );
            }
        }
    }
    #[cfg(not(target_os = "linux"))]
    let _ = pid;
}
