//! `ai-account-center bar ...`: port of src/commands/bar (dispatcher, help, version, launch,
//! install, uninstall). The macOS paths run behind the same injectable boundaries the TS
//! commands use, so per-OS fixture tests invent app bundles, runners and process tables on any
//! build host; the real install/launch stays a lead-run step on the Mac. Non-macOS refusals are
//! byte-equal with the TS command.

use super::native_app_paths::{
    self as paths, MAC_APP_NAME, MAC_BUNDLE_ID, MAC_EXECUTABLE, NativeResult, PRODUCT_NAME,
};
use serde_json::Value;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const MAC_REQUIRED_FILES: &[&str] = &[
    "Package.swift",
    "VERSION",
    "LICENSE",
    "Resources/Info.plist",
    "Scripts/package_app.sh",
    "Scripts/install_user.sh",
    "Scripts/install_bundle.py",
];
const MAC_SOURCE_PATHS: &[&str] = &[
    "Package.swift",
    "VERSION",
    "LICENSE",
    "Sources",
    "Scripts",
    "Resources",
];

pub struct Proc {
    pub pid: u32,
    pub executable: String,
}

type Runner = Box<dyn Fn(&str, &[&str]) -> Result<NativeResult, String>>;
type PlistReader = Box<dyn Fn(&Path) -> Result<Value, String>>;
type ProcessList = Box<dyn Fn(&[String]) -> Result<Vec<Proc>, String>>;
type IdentityReader = Box<dyn Fn(u32) -> Option<String>>;
type AgentDisable = Box<dyn Fn(&Path, &[String]) -> Result<bool, String>>;
type AgentRestore = Box<dyn Fn(&Path) -> Result<(), String>>;
type MovePath = Box<dyn Fn(&Path, &Path) -> Result<(), String>>;
type OpenApp = Box<dyn Fn(&Path) -> Result<(), String>>;
type PromptLaunch = Box<dyn Fn() -> bool>;
type InstallCommand = Box<dyn Fn(&str, &[&str], &Path) -> Result<(), String>>;

/// The injectable boundaries (the TS commands' deps); production fills the real ones.
pub struct Deps {
    pub platform: String,
    pub home: PathBuf,
    pub ccs: PathBuf,
    pub apps_dir: PathBuf,
    pub package_root: PathBuf,
    pub runner: Option<Runner>,
    pub read_plist: Option<PlistReader>,
    pub processes: Option<ProcessList>,
    pub identity: Option<IdentityReader>,
    pub disable_agent: Option<AgentDisable>,
    pub restore_agent: Option<AgentRestore>,
    pub move_path: Option<MovePath>,
    pub open_app: Option<OpenApp>,
    pub prompt_launch: Option<PromptLaunch>,
    pub install_command: Option<InstallCommand>,
}

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

pub fn production_deps(ccs: &Path) -> Deps {
    let platform = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    }
    .to_string();
    let home = home_dir();
    let package_root = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            exe.parent()
                .and_then(Path::parent)
                .and_then(Path::parent)
                .map(Path::to_path_buf)
        })
        .unwrap_or_default();
    Deps {
        platform,
        apps_dir: home.join("Applications"),
        home,
        ccs: ccs.to_path_buf(),
        package_root,
        runner: None,
        read_plist: None,
        processes: None,
        identity: None,
        disable_agent: None,
        restore_agent: None,
        move_path: None,
        open_app: None,
        prompt_launch: None,
        install_command: None,
    }
}

fn out(text: &str) {
    print!("{text}");
}

fn err(text: &str) {
    eprint!("{text}");
}

fn run_native(deps: &Deps, command: &str, args: &[&str]) -> Result<NativeResult, String> {
    match &deps.runner {
        Some(runner) => runner(command, args),
        None => paths::run_native_command(command, args),
    }
}

fn read_plist(deps: &Deps, file: &Path) -> Result<Value, String> {
    match &deps.read_plist {
        Some(reader) => reader(file),
        None => paths::read_mac_plist(file, &|c, a| run_native(deps, c, a), None),
    }
}

fn resolve_app(deps: &Deps) -> Result<Option<paths::OwnedMacApp>, String> {
    paths::resolve_owned_mac_app(
        &deps.apps_dir,
        &|c, a| run_native(deps, c, a),
        deps.read_plist.as_ref().map(|reader| {
            let reader: &dyn Fn(&Path) -> Result<Value, String> = reader.as_ref();
            reader
        }),
    )
}

pub fn help() -> String {
    let sections: [(&str, Vec<(&str, &str)>); 4] = [
        (
            "Commands:",
            vec![
                (
                    "install",
                    "Build and install the macOS app from the native sources included in this package",
                ),
                (
                    "uninstall",
                    "Remove the owned app while preserving connection and account settings",
                ),
                ("version", "Show CLI and installed app versions"),
            ],
        ),
        (
            "Options:",
            vec![
                ("--help, -h", "Show this help message"),
                ("--version", "Show CLI and installed app versions"),
            ],
        ),
        (
            "Install options:",
            vec![
                ("--launch", "Launch the app after installation"),
                ("--no-launch", "Install without launching"),
            ],
        ),
        (
            "Examples:",
            vec![
                ("ai-account-center bar", "Open the paired native app"),
                (
                    "ai-account-center bar install --launch",
                    "Build, install and launch the packaged native app",
                ),
                (
                    "ai-account-center bar install --no-launch",
                    "Build and install without launching",
                ),
                (
                    "ai-account-center bar version",
                    "Show CLI and installed native app versions",
                ),
            ],
        ),
    ];
    let mut text = String::from(
        "\nAI Account Center - macOS bar\n\nUsage:\n  ai-account-center bar [command] [options]\n\n",
    );
    for (title, rows) in sections {
        text.push_str(title);
        text.push('\n');
        let width = rows
            .iter()
            .map(|(command, _)| command.len())
            .max()
            .unwrap_or(0);
        for (command, description) in rows {
            text.push_str(&format!(
                "  {:<pad$} {description}\n",
                command,
                pad = width + 2
            ));
        }
        text.push('\n');
    }
    text.push_str("  macOS builds require Xcode or Command Line Tools with Swift.\n");
    text.push_str(
        "  Installation preserves the saved connection, startup preference and legacy app path.\n",
    );
    text.push_str("  The native app uses its saved dashboard address and login.\n");
    text.push_str("  The compatible `ccs bar` command uses the same installer.\n");
    text.push_str("  On Windows, use the installed AI Account Center tray app.\n\n");
    text
}

pub fn run(args: &[String], deps: &Deps) -> i32 {
    let subcommand = args.first().map(String::as_str);
    if args.iter().any(|arg| arg == "--help" || arg == "-h") || subcommand == Some("help") {
        out(&help());
        return 0;
    }
    if matches!(subcommand, Some("--version" | "version")) {
        return version(deps);
    }
    match subcommand {
        None => launch(&[], deps),
        Some("install") => install(&args[1..], deps),
        Some("uninstall") => uninstall(&args[1..], deps),
        Some("launch" | "serve" | "stop" | "status") => {
            err(
                "[X] Local bar server mode was retired. Run ai-account-center dashboard, then open the paired native app with ai-account-center bar.\n",
            );
            1
        }
        Some(other) => {
            err(&format!("[X] Unknown bar subcommand: {other}\n"));
            err("[i] Usage: ai-account-center bar [install|uninstall|version|--help]\n");
            1
        }
    }
}

// ── version ──────────────────────────────────────────────────────────────────

fn read_recorded_version(ccs: &Path) -> Option<String> {
    let file = ccs.join("bar").join(".version");
    let stat = fs::symlink_metadata(&file).ok()?;
    if !stat.is_file() {
        return None;
    }
    let value = fs::read_to_string(&file).ok()?.trim().to_string();
    let semver = {
        let core = value.split(['-', '+']).next().unwrap_or("");
        let parts: Vec<&str> = core.split('.').collect();
        parts.len() == 3
            && parts
                .iter()
                .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
    };
    if value.len() < 100 && semver {
        Some(value)
    } else {
        None
    }
}

fn version(deps: &Deps) -> i32 {
    out(&format!(
        "[i] AI Account Center CLI v{}\n",
        env!("AAC_VERSION")
    ));
    let result = (|| -> Result<(), String> {
        let installed = resolve_app(deps)?;
        if let Some(app) = installed {
            out(&format!(
                "[i] AI Account Center macOS app: v{}\n",
                app.version.as_deref().unwrap_or("unknown")
            ));
            return Ok(());
        }
        out(
            "[i] AI Account Center macOS app: not installed (run `ai-account-center bar install`)\n",
        );
        if let Some(recorded) = read_recorded_version(&deps.ccs) {
            out(&format!("[i] Last recorded bar version: v{recorded}\n"));
        }
        Ok(())
    })();
    if let Err(message) = result {
        out(&format!("[!] Native app metadata unavailable: {message}\n"));
    }
    0
}

// ── launch ───────────────────────────────────────────────────────────────────

fn launch(args: &[String], deps: &Deps) -> i32 {
    if !args.is_empty() {
        err("[X] Usage: ai-account-center bar\n");
        return 1;
    }
    if deps.platform != "darwin" {
        err("[X] Open the installed native tray app on macOS or Windows.\n");
        return 1;
    }
    let result = (|| -> Result<(), String> {
        let app = resolve_app(deps)?
            .ok_or_else(|| "App not installed. Run ai-account-center bar install.".to_string())?;
        match &deps.open_app {
            Some(open) => open(&app.path)?,
            None => {
                let status = std::process::Command::new("open")
                    .arg(&app.path)
                    .status()
                    .map_err(|e| e.to_string())?;
                if !status.success() {
                    return Err(format!("open exited with {status}"));
                }
            }
        }
        out("[OK] AI Account Center opened.\n");
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(message) => {
            err(&format!("[X] {message}\n"));
            1
        }
    }
}

// ── install ──────────────────────────────────────────────────────────────────

fn validate_source_entry(entry: &Path) -> Result<(), String> {
    let stat = fs::symlink_metadata(entry).map_err(|e| e.to_string())?;
    if stat.file_type().is_symlink() || (!stat.is_dir() && !stat.is_file()) {
        return Err(
            "Packaged native source contains a symlink or unsupported file. Reinstall this CLI."
                .into(),
        );
    }
    if stat.is_dir() {
        for child in fs::read_dir(entry).map_err(|e| e.to_string())? {
            let child = child.map_err(|e| e.to_string())?;
            validate_source_entry(&child.path())?;
        }
    }
    Ok(())
}

fn validate_packaged_mac_source(source: &Path) -> Result<(), String> {
    let stat = fs::symlink_metadata(source).ok();
    if !stat.is_some_and(|s| s.is_dir()) {
        return Err("Packaged macOS app source is missing. Reinstall this CLI.".into());
    }
    for required in MAC_REQUIRED_FILES {
        let file = source.join(required);
        if !fs::symlink_metadata(&file).is_ok_and(|s| s.is_file()) {
            return Err(format!(
                "Packaged macOS app source is incomplete: {required}. Reinstall this CLI."
            ));
        }
    }
    if !fs::symlink_metadata(source.join("Sources")).is_ok_and(|s| s.is_dir()) {
        return Err("Packaged macOS app source is incomplete: Sources. Reinstall this CLI.".into());
    }
    for entry in MAC_SOURCE_PATHS {
        validate_source_entry(&source.join(entry))?;
    }
    Ok(())
}

fn pin_installed_version(ccs: &Path, version: Option<&str>) {
    let Some(version) = version else {
        out("[!] The installed app version could not be read.\n");
        return;
    };
    let result = (|| -> Result<(), String> {
        let directory = ccs.join("bar");
        let version_path = directory.join(".version");
        fs::create_dir_all(&directory).map_err(|e| e.to_string())?;
        if !fs::symlink_metadata(&directory).is_ok_and(|s| s.is_dir()) {
            return Err("Unsafe version directory.".into());
        }
        match fs::symlink_metadata(&version_path) {
            Ok(stat) if !stat.is_file() => return Err("Unsafe version metadata.".into()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
        let temporary = directory.join(format!(
            ".version-{}-{}.tmp",
            std::process::id(),
            super::codex_auth::random_suffix_public()
        ));
        let write = (|| -> Result<(), String> {
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut handle = options.open(&temporary).map_err(|e| e.to_string())?;
            handle
                .write_all(version.as_bytes())
                .map_err(|e| e.to_string())?;
            drop(handle);
            fs::rename(&temporary, &version_path).map_err(|e| e.to_string())?;
            Ok(())
        })();
        let _ = fs::remove_file(&temporary);
        write
    })();
    if result.is_err() {
        out("[!] The app was installed, but its version metadata could not be saved.\n");
    }
}

fn copy_dir_recursive(src: &Path, dest: &Path) -> Result<(), String> {
    fs::create_dir_all(dest).map_err(|e| e.to_string())?;
    for entry in fs::read_dir(src).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let stat = fs::symlink_metadata(&from).map_err(|e| e.to_string())?;
        if stat.is_dir() {
            copy_dir_recursive(&from, &to)?;
        } else {
            fs::copy(&from, &to).map_err(|e| e.to_string())?;
        }
    }
    Ok(())
}

/// cpSync semantics for one packaged source entry: a file copies as a file.
fn copy_source_entry(src: &Path, dest: &Path) -> Result<(), String> {
    let stat = fs::symlink_metadata(src).map_err(|e| e.to_string())?;
    if stat.is_dir() {
        return copy_dir_recursive(src, dest);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    fs::copy(src, dest).map_err(|e| e.to_string())?;
    Ok(())
}

fn install(args: &[String], deps: &Deps) -> i32 {
    let allowed = ["--launch", "--no-launch", "--await-quit"];
    if let Some(unknown) = args.iter().find(|arg| !allowed.contains(&arg.as_str())) {
        err(&format!("[X] Unknown option: {unknown}\n"));
        return 1;
    }
    if let Some(duplicate) = args
        .iter()
        .enumerate()
        .find(|(index, arg)| args[..*index].contains(arg))
        .map(|(_, arg)| arg)
    {
        err(&format!("[X] Duplicate option: {duplicate}\n"));
        return 1;
    }
    let force_launch = args.iter().any(|arg| arg == "--launch");
    let no_launch = args.iter().any(|arg| arg == "--no-launch");
    if force_launch && no_launch {
        err("[X] Use either --launch or --no-launch.\n");
        return 1;
    }
    if deps.platform != "darwin" {
        err("[X] This native bar installer requires macOS 14+ and Swift Command Line Tools.\n");
        if deps.platform == "win32" {
            err(
                "[i] Use the packaged windows-bar/scripts/Build.ps1 and Install.ps1 for the Windows app.\n",
            );
        }
        return 1;
    }
    if args.iter().any(|arg| arg == "--await-quit") {
        err(
            "[X] --await-quit is obsolete. The owned installer safely stops only this app after verifying the new build.\n",
        );
        return 1;
    }
    let source = deps.package_root.join("macos-bar");
    let mut workspace: Option<PathBuf> = None;
    let result = (|| -> Result<bool, String> {
        validate_packaged_mac_source(&source)?;
        let base = std::env::temp_dir().join(format!(
            "ai-account-center-native-build-{}",
            super::codex_auth::random_suffix_public()
        ));
        fs::create_dir(&base).map_err(|e| e.to_string())?;
        workspace = Some(base.clone());
        let build_source = base.join("macos-bar");
        fs::create_dir(&build_source).map_err(|e| e.to_string())?;
        for entry in MAC_SOURCE_PATHS {
            copy_source_entry(&source.join(entry), &build_source.join(entry))?;
        }
        out("[i] Building AI Account Center from the native source packaged with this CLI...\n");
        let run_install = |file: &str, args: &[&str], cwd: &Path| -> Result<(), String> {
            match &deps.install_command {
                Some(run) => run(file, args, cwd),
                None => {
                    let status = std::process::Command::new(file)
                        .args(args)
                        .current_dir(cwd)
                        .status()
                        .map_err(|_| {
                            format!(
                                "Could not run {}.",
                                Path::new(file)
                                    .file_name()
                                    .and_then(|n| n.to_str())
                                    .unwrap_or(file)
                            )
                        })?;
                    if status.success() {
                        Ok(())
                    } else {
                        Err(format!(
                            "{} failed ({}).",
                            Path::new(file)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or(file),
                            status
                                .code()
                                .map(|c| c.to_string())
                                .unwrap_or_else(|| "unknown".into())
                        ))
                    }
                }
            }
        };
        run_install(
            "/bin/bash",
            &[&build_source
                .join("Scripts/package_app.sh")
                .to_string_lossy()],
            &build_source,
        )?;
        let should_launch = force_launch
            || (!no_launch
                && match &deps.prompt_launch {
                    Some(prompt) => prompt(),
                    None => {
                        use std::io::IsTerminal;
                        if !std::io::stdin().is_terminal() {
                            false
                        } else {
                            out("Launch AI Account Center now? [Y/n] ");
                            let mut answer = String::new();
                            std::io::stdin().read_line(&mut answer).ok();
                            let value = answer.trim().to_lowercase();
                            value.is_empty() || value == "y" || value == "yes"
                        }
                    }
                });
        let script = build_source
            .join("Scripts/install_user.sh")
            .to_string_lossy()
            .to_string();
        let mut install_args: Vec<&str> = vec![&script];
        if should_launch {
            install_args.push("--launch");
        }
        run_install("/bin/bash", &install_args, &build_source)?;
        let app_path = deps.apps_dir.join(MAC_APP_NAME);
        let installed_version = paths::inspect_owned_mac_app(
            &app_path,
            &|c, a| run_native(deps, c, a),
            deps.read_plist.as_ref().map(|reader| {
                let reader: &dyn Fn(&Path) -> Result<Value, String> = reader.as_ref();
                reader
            }),
        )
        .ok()
        .flatten()
        .and_then(|app| app.version);
        pin_installed_version(&deps.ccs, installed_version.as_deref());
        out(&format!(
            "[OK] AI Account Center{} installed.\n",
            installed_version
                .as_deref()
                .map(|v| format!(" v{v}"))
                .unwrap_or_default()
        ));
        Ok(should_launch)
    })();
    if let Some(base) = &workspace {
        let _ = fs::remove_dir_all(base);
    }
    match result {
        Ok(should_launch) => {
            if !should_launch {
                out("[i] Run `ai-account-center bar` to launch later.\n");
            }
            0
        }
        Err(message) => {
            err(&format!("[X] {message}\n"));
            err(
                "[i] The native installer preserves connection settings and backs up existing owned apps.\n",
            );
            1
        }
    }
}

// ── uninstall ────────────────────────────────────────────────────────────────

struct PreservedTarget {
    source: PathBuf,
    name: String,
    #[cfg(unix)]
    identity: (u64, u64, u32, u64, i64, i64),
    #[cfg(not(unix))]
    identity: (u64, i64, i64),
}

fn stat_identity(stat: &fs::Metadata) -> PreservedTargetIdentity {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        (
            stat.dev(),
            stat.ino(),
            stat.mode(),
            stat.size(),
            stat.mtime(),
            stat.mtime_nsec(),
        )
    }
    #[cfg(not(unix))]
    {
        let mtime = stat
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| (d.as_secs() as i64, d.subsec_nanos() as i64))
            .unwrap_or((0, 0));
        (stat.len(), mtime.0, mtime.1)
    }
}

#[cfg(unix)]
type PreservedTargetIdentity = (u64, u64, u32, u64, i64, i64);
#[cfg(not(unix))]
type PreservedTargetIdentity = (u64, i64, i64);

fn same_file(source: &Path, expected: &PreservedTargetIdentity) -> bool {
    match fs::symlink_metadata(source) {
        Ok(stat) => &stat_identity(&stat) == expected,
        Err(_) => false,
    }
}

fn stat_if_present(file: &Path) -> Result<Option<fs::Metadata>, String> {
    match fs::symlink_metadata(file) {
        Ok(stat) => Ok(Some(stat)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.to_string()),
    }
}

fn require_safe_file(file: &Path) -> Result<fs::Metadata, String> {
    let stat = stat_if_present(file)?;
    let refuse = || {
        format!(
            "Refusing an unexpected uninstall target: {}",
            file.display()
        )
    };
    let Some(stat) = stat else {
        return Err(refuse());
    };
    if !stat.is_file() || stat.file_type().is_symlink() {
        return Err(refuse());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        #[allow(unsafe_code)] // libc::getuid reads the caller's uid; no memory is touched.
        let uid = unsafe { libc::getuid() };
        if stat.uid() != uid {
            return Err(refuse());
        }
    }
    Ok(stat)
}

fn preflight_parents(base: &Path, home: &Path) -> Result<(), String> {
    let mut current = base.to_path_buf();
    while current != home {
        if let Some(stat) = stat_if_present(&current)?
            && (!stat.is_dir() || stat.file_type().is_symlink())
        {
            return Err(format!(
                "Refusing an unexpected native app parent path: {}",
                current.display()
            ));
        }
        let Some(parent) = current.parent().map(Path::to_path_buf) else {
            return Err("Invalid native app parent location.".into());
        };
        if parent == current {
            return Err("Invalid native app parent location.".into());
        }
        current = parent;
    }
    Ok(())
}

fn list_processes(deps: &Deps, executables: &[String]) -> Result<Vec<Proc>, String> {
    if let Some(list) = &deps.processes {
        return list(executables);
    }
    let result = run_native(deps, "/bin/ps", &["-axww", "-o", "pid=,comm="])?;
    if result.status != Some(0) {
        return Err("Cannot verify whether the native app is running.".into());
    }
    let mut processes = Vec::new();
    for line in result.stdout.split('\n') {
        let trimmed = line.trim();
        let mut parts = trimmed.splitn(2, char::is_whitespace);
        let (Some(pid), Some(comm)) = (parts.next(), parts.next()) else {
            continue;
        };
        let comm = comm.trim();
        if comm.is_empty() {
            continue;
        }
        if comm == MAC_EXECUTABLE {
            return Err(
                "An unidentified native app process is running; quit it before uninstalling."
                    .into(),
            );
        }
        if executables.iter().any(|e| e == comm) {
            let pid: u32 = pid
                .parse()
                .map_err(|_| "Cannot verify a native app process identity.".to_string())?;
            if pid == 0 {
                return Err("Cannot verify a native app process identity.".into());
            }
            processes.push(Proc {
                pid,
                executable: comm.to_string(),
            });
        }
    }
    Ok(processes)
}

fn process_identity(deps: &Deps, pid: u32) -> Option<String> {
    if let Some(reader) = &deps.identity {
        return reader(pid);
    }
    let result = run_native(deps, "/bin/ps", &["-p", &pid.to_string(), "-o", "lstart="]).ok()?;
    if result.status != Some(0) {
        return None;
    }
    let trimmed = result.stdout.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn current_uid() -> Result<u32, String> {
    #[cfg(unix)]
    {
        #[allow(unsafe_code)] // libc::getuid reads the caller's uid; no memory is touched.
        Ok(unsafe { libc::getuid() })
    }
    #[cfg(not(unix))]
    {
        Err("Cannot identify the graphical user.".into())
    }
}

fn disable_agent(deps: &Deps, agent: &Path, executables: &[String]) -> Result<bool, String> {
    if let Some(disable) = &deps.disable_agent {
        return disable(agent, executables);
    }
    let uid = current_uid()?;
    let target = format!("gui/{uid}/{MAC_BUNDLE_ID}");
    let current = run_native(deps, "/bin/launchctl", &["print", &target])?;
    if current.status != Some(0) {
        let lower = current.stderr.to_lowercase();
        if lower.contains("could not find service")
            || lower.contains("could not find specified service")
            || lower.contains("service not found")
        {
            return Ok(false);
        }
        return Err("Cannot safely verify the native app launch agent.".into());
    }
    let program = current
        .stdout
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("program = "))
        .map(str::trim);
    let Some(program) = program else {
        return Err("Refusing to stop a foreign launch agent.".into());
    };
    if !executables.iter().any(|e| e == program) {
        return Err("Refusing to stop a foreign launch agent.".into());
    }
    if current.stdout.lines().any(|line| {
        let trimmed = line.trim_start();
        trimmed
            .strip_prefix("pid = ")
            .is_some_and(|pid| pid.starts_with(|c: char| c.is_ascii_digit() && c != '0'))
    }) {
        return Err(format!(
            "Quit {PRODUCT_NAME} before uninstalling; the launch agent has a running process."
        ));
    }
    let result = run_native(deps, "/bin/launchctl", &["bootout", &target])?;
    if result.status != Some(0) {
        return Err("Unable to disable the native app launch agent.".into());
    }
    Ok(true)
}

fn uninstall(args: &[String], deps: &Deps) -> i32 {
    let _ = args;
    if deps.platform != "darwin" {
        err(&format!(
            "[X] {PRODUCT_NAME} native uninstall is supported on macOS only.\n"
        ));
        return 1;
    }
    let result = (|| -> Result<(), String> {
        let apps_dir = std::path::absolute(&deps.apps_dir).map_err(|e| e.to_string())?;
        let home = std::path::absolute(&deps.home).map_err(|e| e.to_string())?;
        let ccs = std::path::absolute(&deps.ccs).map_err(|e| e.to_string())?;
        let paths = paths::mac_app_paths(&home, Some(&apps_dir));
        preflight_parents(&apps_dir, &home)?;
        resolve_app(deps)?;
        let candidates = [paths.canonical.clone(), paths.legacy.clone()];
        let mut targets: Vec<PreservedTarget> = Vec::new();
        for source in &candidates {
            if let Some(stat) = stat_if_present(source)? {
                targets.push(PreservedTarget {
                    source: source.clone(),
                    name: source
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or("")
                        .to_string(),
                    identity: stat_identity(&stat),
                });
            }
        }
        let executables: Vec<String> = candidates
            .iter()
            .map(|app| {
                app.join("Contents")
                    .join("MacOS")
                    .join(MAC_EXECUTABLE)
                    .to_string_lossy()
                    .to_string()
            })
            .collect();
        let agent_dir = home.join("Library").join("LaunchAgents");
        preflight_parents(&agent_dir, &home)?;
        let agent = agent_dir.join(format!("{MAC_BUNDLE_ID}.plist"));
        let agent_stat = stat_if_present(&agent)?;
        if let Some(stat) = &agent_stat {
            require_safe_file(&agent)?;
            let document = read_plist(deps, &agent)?;
            let program_args = document.get("ProgramArguments").and_then(Value::as_array);
            let first = program_args
                .and_then(|args| args.first())
                .and_then(Value::as_str);
            if document.get("Label").and_then(Value::as_str) != Some(MAC_BUNDLE_ID)
                || first.is_none()
                || !executables.iter().any(|e| Some(e.as_str()) == first)
                || document
                    .get("Program")
                    .is_some_and(|program| program.as_str() != first)
            {
                return Err("Refusing a foreign native app launch agent.".into());
            }
            targets.push(PreservedTarget {
                source: agent.clone(),
                name: agent
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("")
                    .to_string(),
                identity: stat_identity(stat),
            });
        }
        let bar_dir = ccs.join("bar");
        if let Some(stat) = stat_if_present(&bar_dir)?
            && (!stat.is_dir() || stat.file_type().is_symlink())
        {
            return Err("Refusing an unexpected version pin directory.".into());
        }
        let pin = bar_dir.join(".version");
        if stat_if_present(&pin)?.is_some() {
            let stat = require_safe_file(&pin)?;
            targets.push(PreservedTarget {
                source: pin.clone(),
                name: "version-pin".into(),
                identity: stat_identity(&stat),
            });
        }
        if targets.is_empty() {
            out(&format!(
                "[i] {PRODUCT_NAME} is not installed — nothing to remove.\n"
            ));
            return Ok(());
        }
        let ensure_not_running = || -> Result<(), String> {
            let Some(process) = list_processes(deps, &executables)?.into_iter().next() else {
                return Ok(());
            };
            if process.pid == 0 || !executables.contains(&process.executable) {
                return Err("Cannot verify a reported native app process.".into());
            }
            let identity = process_identity(deps, process.pid);
            Err(match identity {
                Some(_) => format!(
                    "Quit {PRODUCT_NAME} (PID {}) before uninstalling.",
                    process.pid
                ),
                None => {
                    "Cannot verify that the native app has stopped; no uninstall changes were made."
                        .into()
                }
            })
        };
        ensure_not_running()?;
        let base = home
            .join("Library")
            .join("Application Support")
            .join("CCS Bar")
            .join("Backups");
        preflight_parents(&base, &home)?;
        fs::create_dir_all(&base).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&base, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let backup = base.join(format!(
            "uninstall-{}",
            super::codex_auth::random_suffix_public()
        ));
        fs::create_dir(&backup).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&backup, fs::Permissions::from_mode(0o700))
                .map_err(|e| e.to_string())?;
        }
        let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
        let mut unloaded = false;
        let attempt = (|| -> Result<(), String> {
            for target in &targets {
                if !same_file(&target.source, &target.identity) {
                    return Err(
                        "Native uninstall targets changed during preflight; refusing to continue."
                            .into(),
                    );
                }
            }
            resolve_app(deps)?;
            if agent_stat.is_some() {
                unloaded = disable_agent(deps, &agent, &executables)?;
            }
            ensure_not_running()?;
            for target in &targets {
                if !same_file(&target.source, &target.identity) {
                    return Err(
                        "Native uninstall targets changed during preflight; refusing to continue."
                            .into(),
                    );
                }
                let is_app = target.source == paths.canonical
                    || (!fs::symlink_metadata(&target.source)
                        .is_ok_and(|s| s.file_type().is_symlink())
                        && target.source == paths.legacy);
                if is_app {
                    paths::inspect_owned_mac_app(
                        &target.source,
                        &|c, a| run_native(deps, c, a),
                        deps.read_plist.as_ref().map(|reader| {
                            let reader: &dyn Fn(&Path) -> Result<Value, String> = reader.as_ref();
                            reader
                        }),
                    )?;
                }
                let saved = backup.join(&target.name);
                match &deps.move_path {
                    Some(move_path) => move_path(&target.source, &saved)?,
                    None => fs::rename(&target.source, &saved).map_err(|e| e.to_string())?,
                }
                moved.push((target.source.clone(), saved));
            }
            Ok(())
        })();
        if let Err(error) = attempt {
            let mut restored = true;
            for (source, saved) in moved.iter().rev() {
                let rollback = (|| -> Result<(), String> {
                    if stat_if_present(source)?.is_some() {
                        return Err("A replacement uninstall target appeared.".into());
                    }
                    fs::rename(saved, source).map_err(|e| e.to_string())?;
                    Ok(())
                })();
                if rollback.is_err() {
                    restored = false;
                }
            }
            if unloaded && restored {
                let restore = match &deps.restore_agent {
                    Some(restore) => restore(&agent),
                    None => (|| -> Result<(), String> {
                        let uid = current_uid()?;
                        let result = run_native(
                            deps,
                            "/bin/launchctl",
                            &["bootstrap", &format!("gui/{uid}"), &agent.to_string_lossy()],
                        )?;
                        if result.status != Some(0) {
                            return Err("bootstrap failed".into());
                        }
                        Ok(())
                    })(),
                };
                if restore.is_err() {
                    restored = false;
                }
            }
            if restored && fs::remove_dir(&backup).is_err() {
                restored = false;
            }
            if !restored {
                err(&format!(
                    "[X] Uninstall rollback needs recovery from {}; account data was preserved.\n",
                    backup.display()
                ));
            }
            return Err(error);
        }
        out(&format!(
            "[OK] {PRODUCT_NAME} uninstalled. Preserved app backup: {}\n",
            backup.display()
        ));
        out(
            "[i] Account configuration and authentication were preserved. Run `ai-account-center bar install` to reinstall.\n",
        );
        Ok(())
    })();
    match result {
        Ok(()) => 0,
        Err(message) => {
            err(&format!("[X] {message}\n"));
            1
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn fixture_deps(platform: &str) -> (tempfile::TempDir, Deps) {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        let ccs = dir.path().join("ccs");
        fs::create_dir_all(&home).unwrap();
        fs::create_dir_all(&ccs).unwrap();
        let deps = Deps {
            platform: platform.into(),
            apps_dir: home.join("Applications"),
            home: home.clone(),
            ccs,
            package_root: dir.path().join("package"),
            runner: None,
            read_plist: None,
            processes: None,
            identity: None,
            disable_agent: None,
            restore_agent: None,
            move_path: None,
            open_app: None,
            prompt_launch: None,
            install_command: None,
        };
        (dir, deps)
    }

    fn make_fixture_app(apps: &Path, name: &str) -> PathBuf {
        let app = apps.join(name);
        let macos = app.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(
            app.join("Contents/Info.plist"),
            format!(
                "<plist><dict><key>CFBundleIdentifier</key><string>{MAC_BUNDLE_ID}</string><key>CFBundleExecutable</key><string>{MAC_EXECUTABLE}</string><key>CFBundleShortVersionString</key><string>3.1.0</string></dict></plist>"
            ),
        )
        .unwrap();
        let exe = macos.join(MAC_EXECUTABLE);
        fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        }
        app
    }

    fn ok_runner(_c: &str, _a: &[&str]) -> Result<NativeResult, String> {
        Ok(NativeResult {
            status: Some(0),
            stdout: String::new(),
            stderr: String::new(),
        })
    }

    #[test]
    fn launch_refuses_off_macos_and_opens_the_owned_app() {
        let (_dir, mut deps) = fixture_deps("linux");
        assert_eq!(launch(&[], &deps), 1);
        assert_eq!(launch(&["extra".into()], &deps), 1);
        deps.platform = "darwin".into();
        assert_eq!(launch(&[], &deps), 1); // not installed
        fs::create_dir_all(&deps.apps_dir).unwrap();
        make_fixture_app(&deps.apps_dir, MAC_APP_NAME);
        let opened = std::rc::Rc::new(Cell::new(false));
        let opened_in = opened.clone();
        deps.open_app = Some(Box::new(move |_path| {
            opened_in.set(true);
            Ok(())
        }));
        assert_eq!(launch(&[], &deps), 0);
        assert!(opened.get());
    }

    #[test]
    fn install_validates_flags_platform_and_source() {
        let (_dir, mut deps) = fixture_deps("linux");
        assert_eq!(install(&["--wat".into()], &deps), 1);
        assert_eq!(install(&["--launch".into(), "--launch".into()], &deps), 1);
        assert_eq!(
            install(&["--launch".into(), "--no-launch".into()], &deps),
            1
        );
        assert_eq!(install(&[], &deps), 1); // linux refusal
        deps.platform = "win32".into();
        assert_eq!(install(&[], &deps), 1);
        deps.platform = "darwin".into();
        assert_eq!(install(&["--await-quit".into()], &deps), 1);
        assert_eq!(install(&[], &deps), 1); // missing packaged source
        // A complete packaged source plus a fake installer and installed app succeeds.
        let source = deps.package_root.join("macos-bar");
        for file in MAC_REQUIRED_FILES {
            let path = source.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, "fixture\n").unwrap();
        }
        fs::create_dir_all(source.join("Sources")).unwrap();
        fs::write(source.join("Sources/main.swift"), "fixture\n").unwrap();
        fs::create_dir_all(&deps.apps_dir).unwrap();
        make_fixture_app(&deps.apps_dir, MAC_APP_NAME);
        let ran = std::rc::Rc::new(Cell::new(0));
        let ran_in = ran.clone();
        deps.install_command = Some(Box::new(move |_file, _args, _cwd| {
            ran_in.set(ran_in.get() + 1);
            Ok(())
        }));
        deps.prompt_launch = Some(Box::new(|| false));
        assert_eq!(install(&["--no-launch".into()], &deps), 0);
        assert_eq!(ran.get(), 2); // package_app.sh + install_user.sh
        assert_eq!(
            fs::read_to_string(deps.ccs.join("bar/.version")).unwrap(),
            "3.1.0"
        );
    }

    fn uninstall_fixture(deps: &mut Deps, with_agent: bool) {
        fs::create_dir_all(&deps.apps_dir).unwrap();
        make_fixture_app(&deps.apps_dir, MAC_APP_NAME);
        if with_agent {
            let agents = deps.home.join("Library/LaunchAgents");
            fs::create_dir_all(&agents).unwrap();
            let exe = deps
                .apps_dir
                .join(MAC_APP_NAME)
                .join("Contents/MacOS")
                .join(MAC_EXECUTABLE);
            fs::write(
                agents.join(format!("{MAC_BUNDLE_ID}.plist")),
                format!(
                    "<plist><dict><key>Label</key><string>{MAC_BUNDLE_ID}</string><key>ProgramArguments</key><array><string>{}</string></array></dict></plist>",
                    exe.display()
                ),
            )
            .unwrap();
        }
        fs::create_dir_all(deps.ccs.join("bar")).unwrap();
        fs::write(deps.ccs.join("bar/.version"), "3.1.0").unwrap();
        deps.runner = Some(Box::new(|c, a| {
            if c == "/bin/ps" {
                return Ok(NativeResult {
                    status: Some(0),
                    stdout: String::new(),
                    stderr: String::new(),
                });
            }
            if c == "/bin/launchctl" && a.first() == Some(&"print") {
                return Ok(NativeResult {
                    status: Some(1),
                    stdout: String::new(),
                    stderr: "Could not find service".into(),
                });
            }
            ok_runner(c, a)
        }));
    }

    #[test]
    fn uninstall_preserves_targets_in_a_backup() {
        let (_dir, mut deps) = fixture_deps("linux");
        assert_eq!(uninstall(&[], &deps), 1); // macOS only
        deps.platform = "darwin".into();
        assert_eq!(uninstall(&[], &deps), 0); // nothing installed
        uninstall_fixture(&mut deps, true);
        assert_eq!(uninstall(&[], &deps), 0);
        assert!(!deps.apps_dir.join(MAC_APP_NAME).exists());
        assert!(!deps.ccs.join("bar/.version").exists());
        let backups = deps
            .home
            .join("Library/Application Support/CCS Bar/Backups");
        let entries: Vec<_> = fs::read_dir(&backups).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let backup = entries[0].as_ref().unwrap().path();
        assert!(backup.join(MAC_APP_NAME).exists());
        assert!(backup.join("version-pin").exists());
        assert!(backup.join(format!("{MAC_BUNDLE_ID}.plist")).exists());
    }

    #[test]
    fn uninstall_refuses_while_running_or_foreign() {
        let (_dir, mut deps) = fixture_deps("darwin");
        uninstall_fixture(&mut deps, false);
        let exe = deps
            .apps_dir
            .join(MAC_APP_NAME)
            .join("Contents/MacOS")
            .join(MAC_EXECUTABLE)
            .to_string_lossy()
            .to_string();
        deps.processes = Some(Box::new(move |_| {
            Ok(vec![Proc {
                pid: 4242,
                executable: exe.clone(),
            }])
        }));
        deps.identity = Some(Box::new(|_| Some("Mon Jan  1 00:00:00 2026".into())));
        assert_eq!(uninstall(&[], &deps), 1);
        assert!(deps.apps_dir.join(MAC_APP_NAME).exists());
        // A foreign launch agent refuses before anything moves.
        let (_dir2, mut deps2) = fixture_deps("darwin");
        uninstall_fixture(&mut deps2, false);
        let agents = deps2.home.join("Library/LaunchAgents");
        fs::create_dir_all(&agents).unwrap();
        fs::write(
            agents.join(format!("{MAC_BUNDLE_ID}.plist")),
            format!(
                "<plist><dict><key>Label</key><string>{MAC_BUNDLE_ID}</string><key>ProgramArguments</key><array><string>/usr/bin/evil</string></array></dict></plist>"
            ),
        )
        .unwrap();
        assert_eq!(uninstall(&[], &deps2), 1);
        assert!(deps2.apps_dir.join(MAC_APP_NAME).exists());
    }

    #[test]
    fn recorded_version_follows_the_semver_rule() {
        let (dir, deps) = fixture_deps("linux");
        assert!(read_recorded_version(&deps.ccs).is_none());
        fs::create_dir_all(deps.ccs.join("bar")).unwrap();
        fs::write(deps.ccs.join("bar/.version"), " 1.2.3-beta.1 \n").unwrap();
        assert_eq!(
            read_recorded_version(&deps.ccs).as_deref(),
            Some("1.2.3-beta.1")
        );
        fs::write(deps.ccs.join("bar/.version"), "not-a-version").unwrap();
        assert!(read_recorded_version(&deps.ccs).is_none());
        let _ = dir;
    }
}
