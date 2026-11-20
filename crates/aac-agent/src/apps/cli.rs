use super::*;
use std::cmp::Reverse;

const MANAGED_AGY: &str = ".local/share/ai-account-center/antigravity-runtime/bin/agy";
const CODEX_PACKAGE: &str = "@openai/codex";

pub(super) async fn detect(host: &Host, probe: &impl Probe, name: &str) -> Option<Install> {
    let home = &host.home;
    let managed_root = match name {
        "codex" => Some(home.join(".codex/packages/standalone/releases")),
        "claude" => Some(home.join(".local/share/claude/versions")),
        _ => None,
    };
    let located = host.which(name).filter(|path| {
        name != "agy" || host.platform != Platform::Ubuntu || *path != home.join(MANAGED_AGY)
    });
    let mut candidates: Vec<PathBuf> = located.into_iter().collect();
    if host.platform == Platform::Windows {
        let exe = format!("{name}.exe");
        let local_name = host.local.join(name);
        let program = host.local.join("Programs").join(name);
        candidates.extend([
            home.join(".local/bin").join(&exe),
            local_name.join("bin").join(&exe),
            local_name.join(&exe),
            program.join(&exe),
            program.join(format!("{name}.ps1")),
            program.join(format!("{name}.cmd")),
            host.roaming.join("npm").join(format!("{name}.cmd")),
        ]);
    } else {
        if managed_root.is_some() {
            candidates.splice(
                0..0,
                host.path
                    .iter()
                    .filter(|path| path.is_absolute())
                    .map(|path| path.join(name)),
            );
        }
        candidates.extend([
            home.join(".local/bin").join(name),
            home.join(".bun/bin").join(name),
            host.system("/opt/homebrew/bin").join(name),
            host.system("/usr/local/bin").join(name),
        ]);
    }
    let found: Vec<_> = unique(candidates)
        .into_iter()
        .filter(|path| path.is_file())
        .collect();
    let managed = managed_root
        .filter(|_| host.platform != Platform::Windows)
        .and_then(|root| {
            let root = real(&root);
            found.iter().find(|path| real(path).starts_with(&root))
        });
    let path = managed.or_else(|| found.first())?;
    if host.platform == Platform::Windows {
        if name == "muse" {
            return Some(windows_muse(host, probe, path).await);
        }
        if extension(path) == "cmd" {
            return Some(if name == "codex" {
                Install::new(
                    "npm",
                    package_version(&path.parent()?.join("node_modules/@openai/codex")),
                )
            } else {
                Install::new("unsupported", None)
            });
        }
        // CreateProcess cannot run PowerShell scripts directly. Never route a
        // stray batch or PowerShell file through a shell.
        if matches!(extension(path).as_str(), "bat" | "ps1") {
            return Some(Install::new("native", None));
        }
    }
    let Some(current) = probe.version(path).await else {
        return Some(Install::new("native", None));
    };
    let manager = if name == "codex"
        && host.platform != Platform::Windows
        && managed.is_none()
        && let Some(bin) = path
            .parent()
            .filter(|folder| folder.file_name() == Some(OsStr::new("bin")))
        && let Some(prefix) = bin.parent()
        && let root = prefix.join("lib/node_modules/@openai/codex")
        && real(path).starts_with(real(&root))
        && is_codex_package(&root)
    {
        "npm"
    } else {
        "native"
    };
    let mut install = Install::new(manager, Some(current));
    if let Some(managed) = managed {
        install.strays = strays(host, probe, found.iter(), managed).await;
    }
    Some(install)
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
}

fn codex_package(root: &Path) -> Option<Value> {
    read_json(&root.join("package.json"), 65_536).filter(|info| info["name"] == CODEX_PACKAGE)
}

fn is_codex_package(root: &Path) -> bool {
    codex_package(root).is_some()
}

fn package_version(root: &Path) -> Option<String> {
    version(codex_package(root)?.get("version").and_then(Value::as_str))
}

async fn windows_muse(host: &Host, probe: &impl Probe, path: &Path) -> Install {
    let expected = host.local.join("Programs/muse");
    let launcher = expected.join(".muse-launcher.ps1");
    let name = path
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !matches!(name.as_str(), "muse" | "muse.cmd")
        || path
            .parent()
            .is_none_or(|parent| real(parent) != real(&expected))
        || !launcher.is_file()
    {
        return Install::new("unsupported", None);
    }
    let current = if let Some(shell) = host.powershell.as_deref() {
        let mut argv = args(&[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ]);
        argv.push(launcher.into_os_string());
        argv.push("--version".into());
        let result = probe.run(shell, &argv, 20).await;
        version(result.output().and_then(|text| text.rsplit('(').next()))
    } else {
        None
    };
    Install::new("native", current)
}

async fn strays<'a>(
    host: &Host,
    probe: &impl Probe,
    paths: impl Iterator<Item = &'a PathBuf>,
    managed: &Path,
) -> Vec<Value> {
    let mut seen = std::collections::HashSet::from([real(managed)]);
    let mut folders: Vec<_> = host
        .path
        .iter()
        .filter(|path| path.is_absolute())
        .cloned()
        .collect();
    if host.platform == Platform::Mac {
        folders.extend(host.mac_paths(false).into_iter().take(64));
    }
    let folders = unique(folders)
        .iter()
        .map(|folder| real(folder))
        .collect::<Vec<_>>();
    let position = |path: &Path| {
        let parent = real(path.parent()?);
        folders.iter().position(|folder| *folder == parent)
    };
    let managed_at = position(managed);
    let mut copies = paths
        .filter_map(|path| {
            let target = real(path);
            seen.insert(target.clone())
                .then_some((path.as_path(), target))
        })
        .take(4);
    let probe_copy = async |copy: Option<(&Path, PathBuf)>| {
        let (path, target) = copy?;
        let location = if path.starts_with(host.system("/usr/local")) {
            "usr-local"
        } else if path.starts_with(host.system("/opt/homebrew"))
            || path.starts_with(host.system("/home/linuxbrew"))
        {
            "homebrew"
        } else if path.starts_with(host.home.join(".bun")) {
            "bun"
        } else if path.starts_with(&host.home) && target.to_string_lossy().contains("node_modules")
        {
            "user-npm"
        } else {
            "other"
        };
        let mut version = None;
        if target.ends_with("node_modules/@openai/codex/bin/codex.js")
            && let Some(root) = target.parent().and_then(Path::parent)
            && let Some(info) = codex_package(root)
        {
            version = Some(super::version(info["version"].as_str()));
        }
        let version = match version {
            Some(version) => version,
            None => probe.version(path).await,
        };
        Some(json!({
            "location": location, "version": version,
            "shadows": position(path).is_some_and(|at| managed_at.is_none_or(|managed_at| at < managed_at))
        }))
    };
    let (first, second, third, fourth) = tokio::join!(
        probe_copy(copies.next()),
        probe_copy(copies.next()),
        probe_copy(copies.next()),
        probe_copy(copies.next()),
    );
    [first, second, third, fourth]
        .into_iter()
        .flatten()
        .collect()
}

static CURSOR_VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A20[0-9]{2}\.[0-9]{2}\.[0-9]{2}-[A-Za-z0-9.-]{1,80}\z")
        .expect("Cursor release pattern")
});
static VERSION_DIR: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(
        r"\A([0-9]{4})\.([0-9]{1,2})\.([0-9]{1,2})(?:-[0-9]{2}-[0-9]{2}-[0-9]{2})?-[a-f0-9]+\z",
    )
    .expect("Cursor folder pattern")
});
static POLICY: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"(?i)PSSecurityException|FullyQualifiedErrorId(?-u:\s*:\s*)UnauthorizedAccess(?-u:\b)|running scripts is disabled on this system|is not digitally signed")
        .expect("Cursor policy pattern")
});

pub(super) async fn cursor(host: &Host, probe: &impl Probe) -> Option<Install> {
    let windows = host.platform == Platform::Windows;
    let root = host.local.join("cursor-agent");
    let mut launcher = host.home.join(".local/bin/cursor-agent");
    let names: &[_] = if windows {
        launcher = root.join("cursor-agent.ps1");
        if !launcher.is_file() {
            launcher = root.join("cursor-agent.cmd");
        }
        &["cursor-agent.cmd", "cursor-agent.ps1", "cursor-agent.exe"]
    } else {
        &["cursor-agent"]
    };
    if !launcher.is_file() {
        return host
            .effective_path()
            .iter()
            .any(|folder| names.iter().any(|name| folder.join(name).is_file()))
            .then(|| Install::new("unsupported", None));
    }
    let current = if windows {
        if let Some(folder) = cursor_folder(&root, false) {
            Some(folder)
        } else {
            windows_cursor(host, probe, &root, &launcher).await
        }
    } else {
        let target = real(&launcher);
        if !target.starts_with(real(&host.home.join(".local/share/cursor-agent/versions")))
            || target.file_name() != Some(OsStr::new("cursor-agent"))
        {
            return Some(Install::new("unsupported", None));
        }
        let folder = target
            .parent()
            .and_then(Path::file_name)
            .and_then(OsStr::to_str);
        if let Some(version) = checked_version(folder, &CURSOR_VERSION, 100)
            && host.platform == Platform::Mac
        {
            Some(version)
        } else {
            probe.version(&launcher).await
        }
    };
    Some(Install::new("native", current))
}

fn cursor_folder(root: &Path, resolve_tie: bool) -> Option<String> {
    let list = read::entries(&root.join("versions"), 4096).ok()?;
    let mut folders: Vec<_> = list
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            let match_ = VERSION_DIR.captures(&name)?;
            let date = (
                match_[1].parse::<u16>().ok()?,
                match_[2].parse::<u8>().ok()?,
                match_[3].parse::<u8>().ok()?,
            );
            let modified = resolve_tie
                .then(|| path.metadata().and_then(|meta| meta.modified()).ok())
                .flatten();
            Some((date, modified, path, name))
        })
        .collect();
    folders.sort_by_key(|(day, modified, _, _)| Reverse((*day, *modified)));
    let (day, _, path, name) = folders.first()?;
    if !resolve_tie && folders.get(1).is_some_and(|(next, ..)| next == day) {
        return None;
    }
    (path.join("node.exe").is_file() && path.join("index.js").is_file()).then(|| name.clone())
}

async fn windows_cursor(
    host: &Host,
    probe: &impl Probe,
    root: &Path,
    launcher: &Path,
) -> Option<String> {
    let shell = host.powershell.as_deref()?;
    let argv = if extension(launcher) == "ps1" {
        let mut argv = args(&[
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ]);
        argv.extend([launcher.as_os_str().to_owned(), "--version".into()]);
        argv
    } else {
        ps_command(format!(
            "$ErrorActionPreference='Stop'; & {} --version; if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}",
            ps_quote(launcher),
        ))
    };
    match probe.run(shell, &argv, 20).await {
        ProbeResult::Failed(output) if POLICY.is_match(&output) => cursor_folder(root, true),
        result => result.version(),
    }
}
