//! Cursor's supported native updater, independently of T3's own update
//! (Python `app_update_cursor.py`): where the installation is and who owns
//! it, whether another copy comes first on PATH, the Windows launcher with its
//! execution-policy fallback, the Mac GUI step, and what the updater's own
//! words mean.
use super::{
    AppRow, AppUpdater, BoxFuture, CommandOutput, CommandSpec, Context, Detection, FailureCause,
    Installed, Platform, Probe, Status, UpdateFailure, common, shared,
};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    path::{Path, PathBuf},
    sync::LazyLock,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

const APP: &str = "cursor-cli";
/// Python `VERSION_PROBE_TIMEOUT` and `windows_version(timeout=20)`.
const PROBE: Duration = Duration::from_secs(20);
/// Python `update_cursor`: the updater's own bound.
const UPDATE: Duration = Duration::from_secs(180);

/// Python `\s*` for ASCII text (the workspace regex has no `unicode-perl`).
macro_rules! blanks {
    () => {
        r"[ \t\r\n\x0b\x0c]*"
    };
}

static CURSOR_VERSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\A20[0-9]{2}\.[0-9]{2}\.[0-9]{2}-[A-Za-z0-9.-]{1,80}\z")
        .expect("Cursor release pattern")
});
static VERSION_DIR: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"\A([0-9]{4})\.([0-9]{1,2})\.([0-9]{1,2})(?:-[0-9]{2}-[0-9]{2}-[0-9]{2})?-[a-f0-9]+\z",
    )
    .expect("Cursor version folder pattern")
});
static POLICY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        "(?i)PSSecurityException|FullyQualifiedErrorId",
        blanks!(),
        ":",
        blanks!(),
        r"UnauthorizedAccess(?-u:\b)|running scripts is disabled on this system|is not digitally signed",
    ))
    .expect("Cursor policy pattern")
});
static AUTH: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r#"(?i)\[unauthenticated\]|ERROR_NOT_LOGGED_IN|not logged in|"status""#,
        blanks!(),
        ":",
        blanks!(),
        r"401(?-u:\b)|(?-u:\b)401(?-u:\b)[^\n]{0,40}unauthori[sz]ed|please (?:log|sign) in",
    ))
    .expect("Cursor sign-in pattern")
});
static NAMED_CAUSE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(concat!(
        r"(?i)(?-u:\b)E(?:NOSPC|PERM|ACCES|BUSY|NOENT|NOTFOUND|CONNREFUSED|CONNRESET|TIMEDOUT|AI_AGAIN|NETUNREACH|HOSTUNREACH|ROFS|MFILE|EXIST|NOTEMPTY|IO)(?-u:\b)",
        r"|no space left on device|permission denied|operation not permitted|read-only file system",
        r"|(?-u:\b)TLS(?-u:\b)|(?-u:\b)SSL(?-u:\b)|certificate|CERT_[A-Z_]+|SELF_SIGNED|UNABLE_TO_VERIFY",
    ))
    .expect("Cursor named cause pattern")
});
static SIGNIN: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)401|unauthenticated").expect("Cursor sign-in reason"));

pub(super) static CURSOR: Cursor = Cursor;

pub(super) struct Cursor;

impl AppUpdater for Cursor {
    fn app_id(&self) -> &'static str {
        APP
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            match detect(cx).await {
                Some(install) => Detection::Found(Box::new(install)),
                None => Detection::Absent,
            }
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>> {
        Box::pin(async move { Ok(update(cx, install).await) })
    }
}

fn launcher(cx: &Context) -> PathBuf {
    let root = cx.host.local.join("cursor-agent");
    if cx.platform == Platform::Windows {
        let script = root.join("cursor-agent.ps1");
        return if script.is_file() {
            script
        } else {
            root.join("cursor-agent.cmd")
        };
    }
    cx.host.home.join(".local/bin/cursor-agent")
}

fn written(folder: &Path) -> SystemTime {
    std::fs::metadata(folder)
        .and_then(|meta| meta.modified())
        .unwrap_or(UNIX_EPOCH)
}

/// Python `windows_version_dir`: the version folder Cursor's launcher would
/// run now, read without running it. Same-day folders cannot be ordered the
/// way PowerShell would (`None`: the launcher decides) unless `newest_on_tie`.
fn windows_version_dir(root: &Path, newest_on_tie: bool) -> Option<PathBuf> {
    let list = std::fs::read_dir(root.join("versions")).ok()?;
    let mut folders: Vec<(u32, PathBuf)> = list
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                return None;
            }
            let name = entry.file_name().into_string().ok()?;
            let found = VERSION_DIR.captures(&name)?;
            let part = |at: usize| found[at].parse::<u32>().ok();
            Some((part(1)? * 10_000 + part(2)? * 100 + part(3)?, path))
        })
        .collect();
    folders.sort_by_key(|(day, _)| std::cmp::Reverse(*day));
    let top = folders.first()?.0;
    if folders.get(1).is_some_and(|(day, _)| *day == top) {
        if !newest_on_tie {
            return None;
        }
        folders.retain(|(day, _)| *day == top);
        folders.sort_by_key(|(_, path)| std::cmp::Reverse(written(path)));
    }
    let newest = &folders[0].1;
    (newest.join("node.exe").is_file() && newest.join("index.js").is_file()).then(|| newest.clone())
}

/// Python `windows_command`'s label: `cursor-agent <argument>`.
fn launcher_label(argument: &str, direct: bool) -> &'static str {
    match (argument, direct) {
        ("--version", false) => "cursor-agent --version",
        ("--version", true) => "cursor-agent --version (node direct)",
        (_, false) => "cursor-agent update",
        (_, true) => "cursor-agent update (node direct)",
    }
}

fn hushed(mut spec: CommandSpec, quiet: bool) -> CommandSpec {
    spec.quiet = quiet;
    spec
}

/// Python `windows_command`: Cursor's own launcher, the way its
/// `cursor-agent.cmd` runs it.
async fn windows_command(
    cx: &Context,
    path: &Path,
    argument: &str,
    timeout: Duration,
    quiet: bool,
) -> Result<CommandOutput, UpdateFailure> {
    if path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("ps1"))
    {
        let mut argv: Vec<OsString> = [
            "powershell.exe",
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ]
        .map(OsString::from)
        .to_vec();
        argv.extend([path.as_os_str().to_owned(), argument.into()]);
        let spec = hushed(
            CommandSpec::new(argv)
                .timeout(timeout)
                .capture()
                .errors()
                .label(launcher_label(argument, false))
                .console(),
            quiet,
        );
        return cx.run(&spec).await;
    }
    let script = format!(
        "$ErrorActionPreference='Stop'; & {} {argument}; if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}",
        shared::ps_quote(&path.to_string_lossy())
    );
    shared::powershell(
        cx,
        &script,
        timeout,
        Some(launcher_label(argument, false)),
        quiet,
    )
    .await
    .map(|stdout| CommandOutput { stdout })
}

/// Python `windows_direct`: the launcher's last step without PowerShell, for a
/// policy that refuses every script.
async fn windows_direct(
    cx: &Context,
    root: &Path,
    argument: &str,
    timeout: Duration,
    quiet: bool,
) -> Result<CommandOutput, UpdateFailure> {
    let Some(folder) = windows_version_dir(root, true) else {
        return Err(UpdateFailure::new("policy_blocked")
            .with_cause(FailureCause::PolicyBlocked, Some("no_direct_entry")));
    };
    let extra = [
        (
            "CURSOR_INVOKED_AS".to_owned(),
            "cursor-agent.ps1".to_owned(),
        ),
        (
            "NODE_COMPILE_CACHE".to_owned(),
            cx.host
                .local
                .join("cursor-compile-cache")
                .to_string_lossy()
                .into_owned(),
        ),
    ];
    let mut argv = vec![folder.join("node.exe"), folder.join("index.js")];
    argv.push(argument.into());
    let mut spec = hushed(
        CommandSpec::new(argv)
            .timeout(timeout)
            .capture()
            .errors()
            .label(launcher_label(argument, true)),
        quiet,
    );
    spec.env_mode = common::EnvMode::Exact;
    spec.env = shared::environment(&extra)
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    cx.run(&spec).await
}

/// Python `windows_version`: `(version, probe word, reason)` for a Windows
/// install whose folder cannot decide it.
async fn windows_version(
    cx: &Context,
    path: &Path,
) -> (Option<String>, Option<&'static str>, Option<&'static str>) {
    match windows_command(cx, path, "--version", PROBE, true).await {
        Ok(output) => return (common::version_text(&output.stdout), None, None),
        Err(error) if error.code == "timeout" => {
            return (None, Some("timeout"), Some("version_probe_timeout"));
        }
        Err(error) if !POLICY.is_match(&error.output) => {
            return (None, Some("failed"), Some("version_probe_failed"));
        }
        Err(_) => {}
    }
    match windows_version_dir(path.parent().unwrap_or(Path::new("")), true) {
        Some(folder) => {
            cx.record(|record| record.note("policy_version_from_folder"));
            (
                folder
                    .file_name()
                    .and_then(OsStr::to_str)
                    .map(str::to_owned),
                None,
                None,
            )
        }
        None => (None, Some("failed"), Some("policy_blocked")),
    }
}

/// Python `path_copies`: every `cursor-agent` on the effective PATH, in PATH
/// order, one per folder (a `.cmd` and a `.ps1` side by side are one copy).
fn path_copies(cx: &Context) -> Vec<PathBuf> {
    let windows = cx.platform == Platform::Windows;
    let names: &[&str] = if windows {
        &["cursor-agent.cmd", "cursor-agent.ps1", "cursor-agent.exe"]
    } else {
        &["cursor-agent"]
    };
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    for folder in cx.host.effective_path() {
        for name in names {
            let candidate = folder.join(name);
            let Ok(key) = (if windows {
                folder.canonicalize()
            } else {
                candidate.canonicalize()
            }) else {
                continue;
            };
            if candidate.is_file() && seen.insert(key) {
                found.push(candidate);
            }
        }
    }
    found
}

/// Python `same_copy`.
fn same_copy(cx: &Context, left: &Path, right: &Path) -> bool {
    let resolve = |path: &Path| {
        if cx.platform == Platform::Windows {
            path.parent().and_then(|parent| parent.canonicalize().ok())
        } else {
            path.canonicalize().ok()
        }
    };
    resolve(left)
        .zip(resolve(right))
        .is_some_and(|(left, right)| left == right)
}

/// Python `owner_for`: which copy detection chose and how.
fn owner_for(cx: &Context, path: &Path, copies: &[PathBuf]) -> Value {
    let mut value = json!({
        "manager": "native",
        "executable": shared::display_path(&cx.host, path),
        "onPath": copies.iter().any(|copy| same_copy(cx, copy, path)),
        "copies": copies.len().min(32),
    });
    if let Some(first) = copies.first().filter(|first| !same_copy(cx, first, path)) {
        value["firstOnPath"] = Value::String(shared::display_path(&cx.host, first));
    }
    value
}

/// Python `shadow_of`: the copy that runs instead of the managed one.
async fn shadow_of(cx: &Context, path: &Path, copies: &[PathBuf]) -> Option<Value> {
    let first = copies.first()?;
    if same_copy(cx, first, path) {
        return None;
    }
    // Reading another installer's version runs it; only POSIX binaries are
    // probed, never a Windows script.
    let version = if cx.platform == Platform::Windows {
        None
    } else {
        match shared::cli_probe(cx, first, &["--version"]).await {
            shared::Probed::Version(version) => version,
            _ => None,
        }
    };
    Some(json!({
        "path": shared::display_path(&cx.host, first),
        "version": version,
    }))
}

/// Python `probe or (None if version else "failed")` and its reason twin.
fn probed(
    version: bool,
    probe: Option<&'static str>,
    reason: Option<&'static str>,
    unparsed: &'static str,
) -> (Probe, Option<&'static str>) {
    let probe = probe.or(if version { None } else { Some("failed") });
    let reason = reason.or(if version { None } else { Some(unparsed) });
    let probe = match probe {
        Some("timeout") => Probe::Timeout,
        Some("failed") => Probe::Failed,
        _ => Probe::Ok,
    };
    (probe, reason)
}

/// Python `detect_cursor`.
pub(super) async fn detect(cx: &Context) -> Option<Installed> {
    let platform = cx.platform;
    let path = launcher(cx);
    let copies = path_copies(cx);
    if !path.is_file() {
        // Another installer's copy (Homebrew, a manual copy): it runs, but has
        // no supported updater here.
        let first = copies.first()?;
        let mut install = Installed::new(APP, platform, first.clone(), None, "unsupported");
        install.probe_reason = Some("unmanaged_location");
        install.owner = Some(json!({
            "manager": "other",
            "executable": shared::display_path(&cx.host, first),
            "onPath": true,
            "copies": copies.len().min(32),
        }));
        return Some(install);
    }
    let mut owner = owner_for(cx, &path, &copies);
    let shadow = shadow_of(cx, &path, &copies).await;
    if let Some(version) = shadow
        .as_ref()
        .and_then(|shadow| shadow["version"].as_str())
        .filter(|version| !version.is_empty())
    {
        owner["firstOnPathVersion"] = Value::String(version.to_owned());
    }
    let mut install = Installed::new(APP, platform, path.clone(), None, "native");
    install.owner = Some(owner.clone());
    install.shadow = shadow;
    if platform != Platform::Windows {
        let target = crate::apps::real(&path);
        let versions = crate::apps::real(&cx.host.home.join(".local/share/cursor-agent/versions"));
        if !target.starts_with(&versions) || target.file_name() != Some(OsStr::new("cursor-agent"))
        {
            install.manager = "unsupported";
            install.probe_reason = Some("unmanaged_location");
            install.shadow = None;
            return Some(install);
        }
        // Older Mac launchers consult Keychain even for --version. The managed
        // target's release directory is the installer's version, without auth.
        let release = target
            .parent()
            .and_then(|parent| parent.file_name())
            .and_then(OsStr::to_str);
        if platform == Platform::Mac
            && let Some(release) = release.filter(|release| CURSOR_VERSION.is_match(release))
        {
            install.version = Some(release.to_owned());
            return Some(install);
        }
        let probe = match shared::cli_probe(cx, &path, &["--version"]).await {
            shared::Probed::Version(version) => {
                install.version = version;
                None
            }
            shared::Probed::Timeout => Some("timeout"),
            shared::Probed::Failed => None,
        };
        let (probe, reason) = probed(
            install.version.is_some(),
            probe,
            None,
            "version_probe_failed",
        );
        install.probe = probe;
        install.probe_reason = reason;
        return Some(install);
    }
    // Windows: the folder the launcher would run is the version, read without
    // PowerShell (its staged promotion and execution policy stay out of a check).
    let parent = path.parent().unwrap_or(Path::new("")).to_path_buf();
    if let Some(folder) = windows_version_dir(&parent, false) {
        install.version = folder
            .file_name()
            .and_then(OsStr::to_str)
            .map(str::to_owned);
        owner["source"] = Value::String("versions-folder".into());
        install.owner = Some(owner);
        return Some(install);
    }
    let (version, probe, reason) = windows_version(cx, &path).await;
    install.version = version;
    let (probe, reason) = probed(install.version.is_some(), probe, reason, "version_unparsed");
    install.probe = probe;
    install.probe_reason = reason;
    Some(install)
}

/// Python `cursor_staged`: the Windows updater left a staged version or
/// launcher behind.
fn staged(path: &Path) -> bool {
    let suffix = |entry: &std::fs::DirEntry, end: &str| {
        let name = entry.file_name().to_string_lossy().into_owned();
        !name.starts_with('.')
            && (name.ends_with(end) || (cfg!(windows) && name.to_lowercase().ends_with(end)))
    };
    let versions = path.parent().unwrap_or(path).join("versions");
    let any = |folder: PathBuf, end: &'static str| {
        std::fs::read_dir(folder)
            .is_ok_and(|list| list.filter_map(Result::ok).any(|entry| suffix(&entry, end)))
    };
    any(versions.clone(), ".new") || any(versions.join("pending-launchers"), ".pending")
}

/// Python `classify`: what the updater's own output means.
fn classify(cx: &Context, error: &UpdateFailure) -> UpdateFailure {
    if !matches!(error.code, "update_failed" | "policy_blocked") {
        return error.clone();
    }
    let text = error.output.as_str();
    // The updater's own "Update failed:" line decides first: a TLS warning
    // elsewhere in the output must not hide the refusal it names, and a
    // refusal there must not hide a named errno either.
    let failed = text
        .split('\n')
        .filter(|line| line.to_lowercase().contains("update failed"))
        .collect::<Vec<_>>()
        .join("\n");
    if !failed.is_empty() {
        if AUTH.is_match(&failed) {
            return signin(error, &failed);
        }
        if NAMED_CAUSE.is_match(&failed) {
            cx.record(|record| record.note("signin_undetermined"));
            return error.clone();
        }
    }
    if NAMED_CAUSE.is_match(text) {
        cx.record(|record| record.note("signin_undetermined"));
        return error.clone();
    }
    if AUTH.is_match(text) {
        return signin(error, text);
    }
    if POLICY.is_match(text) {
        return UpdateFailure {
            output: error.output.clone(),
            exit_code: error.exit_code,
            cause: Some(FailureCause::PolicyBlocked),
            reason: Some("execution_policy"),
            ..UpdateFailure::new("policy_blocked")
        };
    }
    if error.code == "update_failed" {
        cx.record(|record| record.note("signin_undetermined"));
    }
    error.clone()
}

/// Python `_signin_failure`.
fn signin(error: &UpdateFailure, words: &str) -> UpdateFailure {
    UpdateFailure {
        output: error.output.clone(),
        exit_code: error.exit_code,
        cause: Some(FailureCause::AuthRequired),
        reason: Some(if SIGNIN.is_match(words) {
            "http_401"
        } else {
            "not_logged_in"
        }),
        ..UpdateFailure::new("cursor_signin_needed")
    }
}

/// Python `run_update`.
async fn run_update(
    cx: &Context,
    install: &Installed,
    remaining: Duration,
) -> Result<(), UpdateFailure> {
    match install.platform {
        Platform::Windows => {
            match windows_command(cx, &install.path, "update", remaining, false).await {
                Ok(_) => Ok(()),
                Err(error) => {
                    let failure = classify(cx, &error);
                    if failure.code != "policy_blocked" {
                        return Err(failure);
                    }
                    // Group Policy overrides the process-scoped Bypass: run the
                    // launcher's last step directly.
                    cx.record(|record| record.note("policy_fallback_direct"));
                    if staged(&install.path) {
                        return Err(UpdateFailure::new("cursor_update_staged"));
                    }
                    let direct = windows_direct(
                        cx,
                        install.path.parent().unwrap_or(Path::new("")),
                        "update",
                        remaining,
                        false,
                    )
                    .await;
                    match direct {
                        Ok(_) => Ok(()),
                        Err(error) => Err(direct_failure(cx, error)),
                    }
                }
            }
        }
        Platform::Mac => {
            super::desktop::mac_gui_command(cx, super::desktop::GuiAction::CursorUpdate, remaining)
                .await
                .map_err(|error| classify(cx, &error))
        }
        Platform::Ubuntu => {
            // stdout too: Cursor may print its refusal there.
            let mut argv = vec![install.path.as_os_str().to_owned()];
            argv.push("update".into());
            let spec = CommandSpec::new(argv)
                .timeout(remaining)
                .capture()
                .errors()
                .label("cursor-agent update");
            cx.run(&spec)
                .await
                .map(|_| ())
                .map_err(|error| classify(cx, &error))
        }
    }
}

/// Python `run_update`'s last step: the script was refused and its direct step
/// failed too, still a policy outcome (a sign-in refusal or a timeout is not).
fn direct_failure(cx: &Context, error: UpdateFailure) -> UpdateFailure {
    let failure = classify(cx, &error);
    if matches!(failure.code, "cursor_signin_needed" | "timeout") {
        return failure;
    }
    UpdateFailure {
        output: error.output,
        exit_code: error.exit_code,
        cause: Some(FailureCause::PolicyBlocked),
        reason: if error.code != "policy_blocked" {
            Some("direct_failed")
        } else {
            error.reason
        },
        ..UpdateFailure::new("policy_blocked")
    }
}

/// Python `result(..., attempted)`.
fn row(
    install: &Installed,
    status: Status,
    before: Option<&str>,
    after: Option<&str>,
    code: &str,
    attempted: bool,
) -> AppRow {
    let row = AppRow::of(install, status, before, after, code);
    if attempted { row.attempted(0) } else { row }
}

fn newer(left: &str, right: &str) -> bool {
    common::version_tuple(left) > common::version_tuple(right)
}

/// Python `update_cursor`. Always a row: a failure is re-detected and reported
/// with what is on disk now.
pub(super) async fn update(cx: &Context, install: &Installed) -> AppRow {
    let before = install.version.clone();
    let mut attempted = false;
    cx.record(|record| record.owner = install.owner.clone());
    match run(cx, install, &mut attempted).await {
        Ok(row) => row,
        Err(error) => failed(cx, install, before, attempted, error).await,
    }
}

async fn run(
    cx: &Context,
    install: &Installed,
    attempted: &mut bool,
) -> Result<AppRow, UpdateFailure> {
    let remaining = UPDATE.min(cx.remaining());
    if remaining.is_zero() {
        return Err(UpdateFailure::new("timeout"));
    }
    *attempted = true;
    cx.phase("update");
    run_update(cx, install, remaining).await?;
    cx.phase("verify");
    let refreshed = detect(cx).await;
    let before = install.version.as_deref();
    let after = refreshed
        .as_ref()
        .and_then(|found| found.version.as_deref());
    let Some(after) = after else {
        return Err(UpdateFailure::new("version_unknown"));
    };
    if before.is_some_and(|before| newer(before, after)) {
        return Err(UpdateFailure::new("version_went_down"));
    }
    let same =
        before.is_some_and(|before| common::version_tuple(after) == common::version_tuple(before));
    if before.is_none() || (Some(after) != before && same) {
        // Commit hashes from the same date are not an ordering: fail honestly.
        return Err(UpdateFailure::new("version_unknown"));
    }
    if install.platform == Platform::Windows && staged(&install.path) {
        return Ok(row(
            install,
            Status::ActionRequired,
            before,
            Some(after),
            "cursor_update_staged",
            true,
        ));
    }
    let shadow = refreshed
        .as_ref()
        .and_then(|found| found.shadow.clone())
        .or_else(|| install.shadow.clone());
    if let Some(shadow) = shadow {
        // The managed copy is updated, but a bare cursor-agent runs another
        // copy: never "updated".
        cx.record(|record| record.blame(FailureCause::Shadowed, Some("other_copy_first")));
        if let Some(version) = shadow["version"].as_str().filter(|v| !v.is_empty()) {
            let word = match version.cmp(after) {
                std::cmp::Ordering::Less => "first_on_path_older",
                std::cmp::Ordering::Equal => "first_on_path_same",
                std::cmp::Ordering::Greater => "first_on_path_newer",
            };
            cx.record(|record| record.note(word));
        }
        return Ok(row(
            install,
            Status::ActionRequired,
            before,
            Some(after),
            "cursor_shadowed",
            true,
        ));
    }
    let rose = before.is_none_or(|before| newer(after, before));
    Ok(row(
        install,
        if rose {
            Status::Updated
        } else {
            Status::Current
        },
        before,
        Some(after),
        if rose { "cursor_updated" } else { "current" },
        true,
    ))
}

/// Python `update_cursor`'s failure row.
async fn failed(
    cx: &Context,
    install: &Installed,
    before: Option<String>,
    attempted: bool,
    error: UpdateFailure,
) -> AppRow {
    // A native Windows updater may safely stage its files while handles are
    // open and exit nonzero. Never stop those agents or delete their trees.
    let refreshed = detect(cx).await;
    let before = before.as_deref();
    let after = refreshed
        .as_ref()
        .and_then(|found| found.version.as_deref());
    if let (Some(before), Some(after)) = (before, after)
        && newer(before, after)
    {
        return row(
            install,
            Status::Failed,
            Some(before),
            Some(after),
            "version_went_down",
            attempted,
        );
    }
    if error.cause.is_some() || error.reason.is_some() {
        cx.record(|record| {
            record.blame(error.cause.unwrap_or(FailureCause::Unknown), error.reason)
        });
    }
    let special = match error.code {
        "cursor_signin_needed" => Some(("cursor_signin_needed", Status::ActionRequired)),
        _ if install.platform == Platform::Windows && staged(&install.path) => {
            Some(("cursor_update_staged", Status::ActionRequired))
        }
        "policy_blocked" => Some(("policy_blocked", Status::ActionRequired)),
        _ => None,
    };
    let (code, status) = special.unwrap_or((error.code, Status::Failed));
    row(install, status, before, after, code, attempted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::updater::tests::{FakeSystem, host};
    use crate::apps::updater::{Shared, System, attempt};
    use base64::Engine as _;
    use std::sync::Arc;
    #[cfg(unix)]
    use std::sync::atomic::AtomicBool;
    use std::sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    const OLD: &str = "2026.11.05-old";
    /// The official `cursor-agent.cmd` runs `powershell.exe -NoProfile
    /// -ExecutionPolicy Bypass -File cursor-agent.ps1`.
    const OFFICIAL_LAUNCHER: [&str; 6] = [
        "powershell.exe",
        "-NoProfile",
        "-NonInteractive",
        "-ExecutionPolicy",
        "Bypass",
        "-File",
    ];
    const NEW: &str = "2026.11.11-new";
    /// tests/unit/app-updates/fixtures/update-all-2026-11-13/cursor-alexis-policy.txt
    const REFUSED: &str = r"& : File C:\Users\<user>\AppData\Local\cursor-agent\cursor-agent.ps1 cannot be loaded because running scripts is disabled on this system. For more information, see about_Execution_Policies at https:/go.microsoft.com/fwlink/?LinkID=135170.
At line:1 char:28
+ $ErrorActionPreference='Stop'; & 'C:\Users\<user>\AppData\Local\cursor-agent\cursor-agent.ps1 --version; if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
+                                ~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~~
    + CategoryInfo          : SecurityError: (:) [], PSSecurityException
    + FullyQualifiedErrorId : UnauthorizedAccess
";
    /// tests/unit/app-updates/fixtures/update-all-2026-11-13/cursor-owner-pc-stderr.txt
    const REFUSED_SIGNIN: &str = r#"{"event":"global.main.promiseCatch","level":"ERROR","data":{"message":"Update failed: [unauthenticated] Error","stack":"Error: Update failed: [unauthenticated] Error\n    at y (C:\\Users\\<user>\\AppData\\Local\\cursor-agent\\versions\\2026.11.11-b912975\\4261.index.js:1:24891)"}}"#;

    fn ok(_: &CommandSpec) -> Result<CommandOutput, UpdateFailure> {
        Ok(CommandOutput::default())
    }

    fn refused(code: &'static str) -> Result<CommandOutput, UpdateFailure> {
        Err(UpdateFailure::new(code))
    }

    /// A context whose host sees this PATH, under a temporary home.
    fn context(
        root: &Path,
        platform: Platform,
        path: Vec<PathBuf>,
        system: Arc<dyn System>,
    ) -> Context {
        let mut owner = Arc::into_inner(host(root, platform)).expect("host");
        owner.path = path;
        Context::new(
            platform,
            Arc::new(owner),
            system,
            Arc::new(Shared::default()),
            tokio::time::Instant::now() + Duration::from_secs(60),
            root.join("stage"),
            root.join("state"),
            None,
            || false,
            |_| {},
        )
    }

    fn executable(path: &Path, text: &str) {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder).expect("folder");
        }
        std::fs::write(path, text).expect("write");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("mode");
        }
    }

    /// Python `Fixtures.cursor`: the official release layout.
    #[cfg(unix)]
    fn binary(root: &Path, version: &str) -> PathBuf {
        let path = root
            .join("home/.local/share/cursor-agent/versions")
            .join(version)
            .join("cursor-agent");
        executable(&path, &format!("#!/bin/sh\necho {version}\n"));
        path
    }

    #[cfg(unix)]
    fn link(target: &Path, root: &Path) -> PathBuf {
        let link = root.join("home/.local/bin/cursor-agent");
        std::fs::create_dir_all(link.parent().expect("bin")).expect("bin");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(target, &link).expect("link");
        link
    }

    /// Python `B13CursorFallback.layout` / `CursorWindowsRegressions.layout`:
    /// Alexis PC's real `%LOCALAPPDATA%\cursor-agent` layout.
    fn windows_layout(root: &Path, launchers: &[&str], versions: &[&str]) -> PathBuf {
        let root = root.join("home/AppData/Local").join("cursor-agent");
        for name in launchers {
            executable(&root.join(name), "launcher fixture\n");
        }
        for version in versions {
            for name in ["node.exe", "index.js"] {
                executable(&root.join("versions").join(version).join(name), "fixture\n");
            }
        }
        root
    }

    /// Python `FakeWindows`: PowerShell refuses a script file unless the
    /// process runs with `-ExecutionPolicy Bypass`.
    #[derive(Clone, Copy)]
    struct Windows {
        restricted: bool,
        update_stderr: Option<&'static str>,
    }

    fn script_of(argv: &[OsString]) -> String {
        let Some(at) = argv.iter().position(|arg| arg == "-EncodedCommand") else {
            return argv
                .iter()
                .map(|arg| arg.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ");
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(argv[at + 1].to_string_lossy().as_bytes())
            .expect("encoded command");
        let units: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        String::from_utf16(&units).expect("script")
    }

    fn windows(spec: &CommandSpec, fake: &Windows) -> Result<CommandOutput, UpdateFailure> {
        let argv: Vec<String> = spec
            .argv
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let script = script_of(&spec.argv);
        let finish = |code: i64, out: &str, err: &str| {
            if code == 0 {
                Ok(CommandOutput {
                    stdout: out.to_owned(),
                })
            } else {
                Err(UpdateFailure {
                    output: if spec.merge_errors {
                        format!("{out}{err}")
                    } else {
                        err.to_owned()
                    },
                    exit_code: Some(code),
                    ..UpdateFailure::failed()
                })
            }
        };
        let bypass = argv
            .iter()
            .any(|arg| arg.eq_ignore_ascii_case("-executionpolicy"))
            && argv.iter().any(|arg| arg == "Bypass");
        if script.contains(".ps1") && fake.restricted && !bypass {
            return finish(1, "", REFUSED);
        }
        if script.trim_end().ends_with("--version") || script.contains(" --version") {
            return finish(0, "2026.11.11-b912975\n", "");
        }
        if script.contains(" update") || argv.last().is_some_and(|arg| arg == "update") {
            return match fake.update_stderr {
                Some(stderr) => finish(1, "", stderr),
                None => finish(0, "Already up to date\n", ""),
            };
        }
        finish(0, "", "")
    }

    fn pair(row: &Value, left: &str, right: &str) -> (Value, Value) {
        (row[left].clone(), row[right].clone())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn detects_mac_from_official_release_link_without_accessing_keychain() {
        // CursorTests.test_detects_mac_from_official_release_link_without_accessing_keychain
        let root = tempfile::tempdir().expect("root");
        let path = link(&binary(root.path(), OLD), root.path());
        let system = FakeSystem::answering(|_| panic!("no launcher may run"));
        let cx = context(root.path(), Platform::Mac, Vec::new(), system.clone());
        let found = detect(&cx).await.expect("found");
        assert_eq!(
            (found.path, found.version.as_deref(), found.probe),
            (path, Some(OLD), Probe::Ok)
        );
        assert!(system.runs.lock().expect("runs").is_empty());
    }

    #[test]
    fn windows_prefers_ps1_and_accepts_the_older_cmd_only_layout() {
        // CursorTests.test_windows_prefers_ps1_and_accepts_the_older_cmd_only_layout
        let root = tempfile::tempdir().expect("root");
        let cx = context(
            root.path(),
            Platform::Windows,
            Vec::new(),
            FakeSystem::answering(ok),
        );
        let folder = windows_layout(root.path(), &["cursor-agent.cmd"], &[]);
        assert_eq!(launcher(&cx), folder.join("cursor-agent.cmd"));
        executable(&folder.join("cursor-agent.ps1"), "launcher");
        assert_eq!(launcher(&cx), folder.join("cursor-agent.ps1"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn absent_unmanaged_and_failed_probes_do_not_claim_installed() {
        // CursorTests.test_absent_unmanaged_and_failed_probes_do_not_claim_installed
        let root = tempfile::tempdir().expect("root");
        let cx = context(
            root.path(),
            Platform::Ubuntu,
            Vec::new(),
            FakeSystem::answering(ok),
        );
        assert!(detect(&cx).await.is_none());
        assert_eq!(
            attempt(&CURSOR, &cx).await.to_value()["status"],
            "not_installed"
        );

        // A launcher outside the managed versions folder has no supported updater here.
        let outside = cx.host.home.join(".local/bin/cursor-agent");
        executable(&outside, "outside");
        let found = detect(&cx).await.expect("found");
        assert_eq!(
            (found.manager, found.probe_reason),
            ("unsupported", Some("unmanaged_location"))
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("failed".into(), "unsupported".into())
        );

        // A managed install whose probe times out, or whose output has no version.
        std::fs::remove_file(&outside).expect("remove the outside launcher");
        link(&binary(root.path(), OLD), root.path());
        let timed_out = context(
            root.path(),
            Platform::Ubuntu,
            Vec::new(),
            FakeSystem::answering(|_| refused("timeout")),
        );
        assert_eq!(
            detect(&timed_out).await.expect("found").probe,
            Probe::Timeout
        );
        let unparsed = context(
            root.path(),
            Platform::Ubuntu,
            Vec::new(),
            FakeSystem::answering(|_| {
                Ok(CommandOutput {
                    stdout: "nothing here".into(),
                })
            }),
        );
        let found = detect(&unparsed).await.expect("found");
        assert_eq!(
            (found.probe, found.probe_reason),
            (Probe::Failed, Some("version_probe_failed"))
        );
        let row = attempt(&CURSOR, &timed_out).await.to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("unknown".into(), "check_timeout".into())
        );
        let row = attempt(&CURSOR, &unparsed).await.to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("unknown".into(), "readiness_unknown".into())
        );
        assert_eq!(row["diagnostics"]["reason"], "version_probe_failed");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ownership_names_the_managed_copy_and_any_copy_first_on_path() {
        // CursorOwnershipTests.test_no_path_entry_still_finds_the_managed_install_and_says_so
        // CursorOwnershipTests.test_a_second_copy_first_on_path_is_named
        // CursorOwnershipTests.test_a_copy_from_another_installer_is_unsupported_not_absent
        let root = tempfile::tempdir().expect("root");
        let version = "2026.11.11-b912975";
        let path = link(&binary(root.path(), version), root.path());
        let system = FakeSystem::answering(move |_| {
            Ok(CommandOutput {
                stdout: format!("{version}\n"),
            })
        });
        let cx = context(root.path(), Platform::Ubuntu, Vec::new(), system.clone());
        let owner = detect(&cx).await.expect("found").owner.expect("owner");
        assert_eq!(
            (owner["onPath"].clone(), owner["executable"].clone()),
            (json!(false), json!("~/.local/bin/cursor-agent"))
        );

        let other = root.path().join("home/opt/bin/cursor-agent");
        executable(&other, "#!/bin/sh\necho 2026.01.01-old\n");
        let first = other.clone();
        let system = FakeSystem::answering(move |spec| {
            let version = if spec.argv[0] == first.as_os_str() {
                "2026.01.01-old"
            } else {
                version
            };
            Ok(CommandOutput {
                stdout: format!("{version}\n"),
            })
        });
        let cx = context(
            root.path(),
            Platform::Ubuntu,
            vec![
                other.parent().expect("bin").to_path_buf(),
                path.parent().expect("bin").to_path_buf(),
            ],
            system,
        );
        let found = detect(&cx).await.expect("found");
        let owner = found.owner.expect("owner");
        assert_eq!(
            (
                owner["copies"].clone(),
                owner["firstOnPath"].clone(),
                owner["onPath"].clone()
            ),
            (json!(2), json!("~/opt/bin/cursor-agent"), json!(true))
        );
        assert_eq!(
            found.shadow,
            Some(json!({"path": "~/opt/bin/cursor-agent", "version": "2026.01.01-old"}))
        );
        assert_eq!(owner["firstOnPathVersion"], "2026.01.01-old");

        let _ = std::fs::remove_file(&path);
        let cx = context(
            root.path(),
            Platform::Ubuntu,
            vec![other.parent().expect("bin").to_path_buf()],
            FakeSystem::answering(ok),
        );
        let found = detect(&cx).await.expect("found");
        assert_eq!(
            (found.manager, found.probe_reason, found.path),
            ("unsupported", Some("unmanaged_location"), other)
        );
        assert_eq!(found.owner.expect("owner")["manager"], "other");
    }

    #[tokio::test]
    async fn a_restricted_policy_no_longer_blocks_detection_or_update() {
        // CursorWindowsRegressions.test_alexis_restricted_policy_no_longer_blocks_detection_or_update
        let root = tempfile::tempdir().expect("root");
        let folder = windows_layout(
            root.path(),
            &[
                "agent.cmd",
                "agent.ps1",
                "cursor-agent.cmd",
                "cursor-agent.ps1",
            ],
            &["2026.03.20-44cb435", "2026.11.11-b912975"],
        );
        executable(&folder.join("versions/promote-staged.cmd"), "@echo off\n");
        let fake = Windows {
            restricted: true,
            update_stderr: None,
        };
        let system = FakeSystem::answering(move |spec| windows(spec, &fake));
        let cx = context(root.path(), Platform::Windows, Vec::new(), system.clone());
        let found = detect(&cx).await.expect("found");
        assert_eq!(
            (found.version.as_deref(), found.probe),
            (Some("2026.11.11-b912975"), Probe::Ok)
        );
        assert!(
            system.runs.lock().expect("runs").is_empty(),
            "the readiness probe was refused: the row would read unknown"
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            pair(&row, "status", "version"),
            ("current".into(), "2026.11.11-b912975".into())
        );
        let runs = system.runs.lock().expect("runs").clone();
        let update = runs
            .iter()
            .find(|argv| argv.last() == Some(&OsString::from("update")))
            .expect("update run");
        let mut expected: Vec<OsString> = OFFICIAL_LAUNCHER.map(OsString::from).to_vec();
        expected.push(folder.join("cursor-agent.ps1").into_os_string());
        assert_eq!(update[..7], expected[..]);
        assert_eq!(row["diagnostics"]["tool"], "cursor-agent");
    }

    #[tokio::test]
    async fn windows_reads_the_version_through_the_official_launcher() {
        // CursorOemOutput.test_a_path_with_a_typographic_apostrophe_is_one_file_argument
        let root = tempfile::tempdir().expect("root");
        let folder = windows_layout(root.path(), &["cursor-agent.cmd", "cursor-agent.ps1"], &[]);
        let seen = Arc::new(Mutex::new(Vec::new()));
        let system = {
            let seen = seen.clone();
            FakeSystem::answering(move |spec| {
                seen.lock().expect("seen").push((
                    spec.argv.clone(),
                    spec.console,
                    spec.merge_errors,
                    spec.quiet,
                    spec.label,
                    spec.timeout,
                ));
                windows(
                    spec,
                    &Windows {
                        restricted: false,
                        update_stderr: None,
                    },
                )
            })
        };
        let cx = context(root.path(), Platform::Windows, Vec::new(), system);
        let found = detect(&cx).await.expect("found");
        assert_eq!(found.version.as_deref(), Some("2026.11.11-b912975"));
        let (argv, console, errors, quiet, label, timeout) = seen.lock().expect("seen")[0].clone();
        let mut expected: Vec<OsString> = OFFICIAL_LAUNCHER.map(OsString::from).to_vec();
        expected.push(folder.join("cursor-agent.ps1").into_os_string());
        expected.push("--version".into());
        assert_eq!(argv, expected);
        assert_eq!(
            (console, errors, quiet, label, timeout),
            (true, true, true, Some("cursor-agent --version"), PROBE)
        );

        // An older `.cmd`-only install: PowerShell's call operator, no script file, no policy.
        let _ = std::fs::remove_file(folder.join("cursor-agent.ps1"));
        let seen = Arc::new(Mutex::new(Vec::new()));
        let system = {
            let seen = seen.clone();
            FakeSystem::answering(move |spec| {
                seen.lock()
                    .expect("seen")
                    .push((spec.argv.clone(), spec.console, spec.label));
                windows(
                    spec,
                    &Windows {
                        restricted: false,
                        update_stderr: None,
                    },
                )
            })
        };
        let cx = context(root.path(), Platform::Windows, Vec::new(), system);
        assert_eq!(
            detect(&cx).await.expect("found").version.as_deref(),
            Some("2026.11.11-b912975")
        );
        let (argv, console, label) = seen.lock().expect("seen")[0].clone();
        assert_eq!(argv.len(), 5);
        assert_eq!(argv[3], "-EncodedCommand");
        assert!(!console);
        assert_eq!(label, Some("cursor-agent --version"));
        let script = script_of(&argv);
        assert!(script.ends_with(&format!(
            "& '{}' --version; if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}",
            folder.join("cursor-agent.cmd").display()
        )));
    }

    // --- what the updater's own words mean (Python `classify`) -------------------------------

    /// `classify` on a failed run with this output, with the notes it left.
    fn classified(
        output: String,
        code: &'static str,
        exit_code: Option<i64>,
    ) -> (UpdateFailure, Vec<String>) {
        let root = tempfile::tempdir().expect("root");
        let cx = context(
            root.path(),
            Platform::Ubuntu,
            Vec::new(),
            FakeSystem::answering(ok),
        );
        let failure = classify(
            &cx,
            &UpdateFailure {
                output,
                exit_code,
                ..UpdateFailure::new(code)
            },
        );
        let row = json!({"status": "failed", "messageCode": failure.code});
        let notes = cx
            .record(|record| record.export(&row))
            .get("notes")
            .and_then(Value::as_array)
            .map(|notes| {
                notes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        (failure, notes)
    }

    #[test]
    fn a_refusal_for_sign_in_is_sign_in_needed_with_its_reason() {
        // robustness row 1, fix_round1 B4, lane_d R2-1
        let cases: Vec<(String, &str)> = [
            (
                format!("Checking for updates...\n{REFUSED_SIGNIN}\n"),
                "http_401",
            ),
            ("Update failed: [unauthenticated] Error".into(), "http_401"),
            ("ERROR_NOT_LOGGED_IN".into(), "not_logged_in"),
            ("Error: not logged in".into(), "not_logged_in"),
            (r#"{"status": 401, "error": "x"}"#.into(), "http_401"),
            (
                "Request failed with status code 401 (Unauthorized)".into(),
                "http_401",
            ),
            ("please log in to continue".into(), "not_logged_in"),
            ("Please sign in".into(), "not_logged_in"),
        ]
        .into();
        for (output, reason) in cases {
            let (failure, _) = classified(output.clone(), "update_failed", Some(1));
            assert_eq!(
                (
                    failure.code,
                    failure.cause,
                    failure.reason,
                    failure.exit_code
                ),
                (
                    "cursor_signin_needed",
                    Some(FailureCause::AuthRequired),
                    Some(reason),
                    Some(1)
                ),
                "{output}"
            );
        }
    }

    #[test]
    fn a_tls_warning_never_hides_the_401_on_the_failure_line() {
        // lane_d R2-1
        let output = "Warning: Setting the NODE_TLS_REJECT_UNAUTHORIZED environment variable to '0' makes TLS \
                      connections and HTTPS requests insecure. Please think twice.\n\
                      Error: Update failed: [unauthenticated] Error: Request failed with status code 401";
        let (failure, _) = classified(output.into(), "update_failed", None);
        assert_eq!(
            (failure.code, failure.cause, failure.reason),
            (
                "cursor_signin_needed",
                Some(FailureCause::AuthRequired),
                Some("http_401")
            )
        );
    }

    #[test]
    fn a_real_tls_failure_on_the_failure_line_stays_undetermined() {
        // lane_d R2-1
        let output = "Warning: NODE_TLS_REJECT_UNAUTHORIZED is set to '0'\nError: Update failed: unable to \
                      verify the first certificate";
        let (failure, notes) = classified(output.into(), "update_failed", None);
        assert_eq!(failure.code, "update_failed");
        assert_eq!(notes, ["signin_undetermined"]);
    }

    #[test]
    fn named_causes_and_policy_words_decide_on_their_own() {
        // fix_round1 B3, AlsoFixed.test_unauthorized_access_exception_is_not_a_policy_refusal
        for output in [
            "Error: ENOSPC: no space left on device, write",
            "Error: getaddrinfo ENOTFOUND api2.cursor.sh\n",
            "npm error code EACCES",
        ] {
            let (failure, notes) = classified(output.into(), "update_failed", Some(1));
            assert_eq!(failure.code, "update_failed", "{output}");
            assert_eq!(notes, ["signin_undetermined"], "{output}");
        }
        for output in [
            "running scripts is disabled on this system",
            "the file is not digitally signed",
            "PSSecurityException",
        ] {
            let (failure, _) = classified(output.into(), "update_failed", Some(1));
            assert_eq!(failure.code, "policy_blocked", "{output}");
        }
        let (failure, _) = classified(
            "System.UnauthorizedAccessException: Access to the path 'versions\\x' is denied."
                .into(),
            "update_failed",
            Some(1),
        );
        assert_eq!(failure.code, "update_failed");
        let (failure, _) = classified(REFUSED.into(), "update_failed", Some(1));
        assert_eq!(
            (
                failure.code,
                failure.cause,
                failure.reason,
                failure.exit_code
            ),
            (
                "policy_blocked",
                Some(FailureCause::PolicyBlocked),
                Some("execution_policy"),
                Some(1)
            )
        );
        // Any other code is not the updater's answer and stays as it is.
        let (failure, notes) = classified(REFUSED.into(), "timeout", None);
        assert_eq!((failure.code, notes), ("timeout", Vec::new()));
        let (failure, _) = classified("plain refusal".into(), "update_failed", None);
        assert_eq!((failure.code, failure.cause), ("update_failed", None));
    }

    // --- the update ------------------------------------------------------------------------------

    fn notes(row: &Value) -> Vec<String> {
        row["diagnostics"]["notes"]
            .as_array()
            .map(|notes| {
                notes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn windows_uses_powershell_launcher_for_probe_and_supported_update() {
        // CursorTests.test_windows_uses_powershell_launcher_for_probe_and_supported_update
        let root = tempfile::tempdir().expect("root");
        let folder = windows_layout(root.path(), &["cursor-agent.cmd"], &[]);
        let probes = AtomicUsize::new(0);
        let system = FakeSystem::answering(move |spec| {
            if script_of(&spec.argv).contains(" --version") {
                let answer = match probes.fetch_add(1, Ordering::SeqCst) {
                    0 => OLD,
                    _ => NEW,
                };
                return Ok(CommandOutput {
                    stdout: format!("{answer}\n"),
                });
            }
            Ok(CommandOutput::default())
        });
        let cx = context(root.path(), Platform::Windows, Vec::new(), system.clone());
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            (
                row["status"].clone(),
                row["previousVersion"].clone(),
                row["version"].clone(),
            ),
            ("updated".into(), OLD.into(), NEW.into())
        );
        let runs = system.runs.lock().expect("runs");
        let update = script_of(&runs[1]);
        assert!(
            update.contains(
                &folder
                    .join("cursor-agent.cmd")
                    .to_string_lossy()
                    .to_string()
            ) && update.contains(" update;"),
            "{update}"
        );
        assert!(
            runs.iter()
                .all(|argv| !script_of(argv).contains("Remove-Item")),
            "an update never removes anything"
        );
    }

    #[tokio::test]
    async fn windows_in_use_staging_is_action_required_on_success_and_failure() {
        // CursorTests.test_windows_in_use_staging_is_action_required_on_success_and_failure
        let root = tempfile::tempdir().expect("root");
        let folder = windows_layout(
            root.path(),
            &["cursor-agent.ps1", "cursor-agent.cmd"],
            &["2026.11.11-b912975"],
        );
        let staged_folder = folder.join("versions").join(format!("{NEW}.new"));
        std::fs::create_dir_all(&staged_folder).expect("staged");
        for stderr in [None, Some("Error: EPERM: another run holds the files")] {
            let fake = Windows {
                restricted: false,
                update_stderr: stderr,
            };
            let cx = context(
                root.path(),
                Platform::Windows,
                Vec::new(),
                FakeSystem::answering(move |spec| windows(spec, &fake)),
            );
            let row = attempt(&CURSOR, &cx).await.to_value();
            assert_eq!(
                (
                    row["status"].clone(),
                    row["messageCode"].clone(),
                    row["version"].clone(),
                    row["updateAttempted"].clone(),
                ),
                (
                    "action_required".into(),
                    "cursor_update_staged".into(),
                    "2026.11.11-b912975".into(),
                    true.into()
                )
            );
        }
        assert!(staged_folder.is_dir(), "a staged tree is never removed");
        // A staged launcher counts too.
        let _ = std::fs::remove_dir(&staged_folder);
        std::fs::create_dir_all(folder.join("versions/pending-launchers")).expect("folder");
        executable(
            &folder.join("versions/pending-launchers/cursor-agent.ps1.pending"),
            "x",
        );
        assert!(staged(&folder.join("cursor-agent.ps1")));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_mac_update_runs_in_the_gui_session_and_reports_both_versions() {
        // CursorTests.test_mac_update_runs_in_gui_and_reports_both_versions
        let root = tempfile::tempdir().expect("root");
        let old = binary(root.path(), OLD);
        let newer = binary(root.path(), NEW);
        let path = link(&old, root.path());
        let home = root.path().join("home");
        let plist = Arc::new(Mutex::new(Vec::new()));
        let bootouts = Arc::new(AtomicUsize::new(0));
        let system = {
            let (plist, bootouts) = (plist.clone(), bootouts.clone());
            FakeSystem::answering(move |spec| {
                let argv: Vec<String> = spec
                    .argv
                    .iter()
                    .map(|arg| arg.to_string_lossy().into_owned())
                    .collect();
                match argv[1].as_str() {
                    "bootstrap" => {
                        *plist.lock().expect("plist") =
                            std::fs::read(&spec.argv[3]).expect("the agent's plist");
                    }
                    "print" if argv[2].contains("/com.ccs.") => {
                        std::fs::remove_file(&path).expect("unlink");
                        std::os::unix::fs::symlink(&newer, &path).expect("relink");
                        return Ok(CommandOutput {
                            stdout: "state = not running\nlast exit code = 0\n".into(),
                        });
                    }
                    "bootout" => {
                        bootouts.fetch_add(1, Ordering::SeqCst);
                    }
                    _ => {}
                }
                Ok(CommandOutput::default())
            })
        };
        let cx = context(root.path(), Platform::Mac, Vec::new(), system.clone());
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            (
                row["status"].clone(),
                row["previousVersion"].clone(),
                row["version"].clone()
            ),
            ("updated".into(), OLD.into(), NEW.into())
        );
        // The app's own command is what ran; the launchctl plumbing is not recorded.
        assert_eq!(
            (
                row["diagnostics"]["tool"].clone(),
                row["diagnostics"]["exitCode"].clone()
            ),
            ("cursor-agent".into(), 0.into())
        );
        let agent = String::from_utf8(plist.lock().expect("plist").clone()).expect("plist");
        assert!(agent.contains(&format!(
            "<string>{}</string>",
            home.join(".local/bin/cursor-agent").display()
        )));
        assert!(agent.contains("<string>update</string>"));
        assert!(
            agent.contains("<key>ProcessType</key>")
                && agent.contains("<string>Interactive</string>")
        );
        assert!(agent.contains("<key>EnvironmentVariables</key>") && agent.contains("<dict>"));
        assert_eq!(bootouts.load(Ordering::SeqCst), 1);
        assert!(
            system
                .runs
                .lock()
                .expect("runs")
                .iter()
                .all(|argv| argv[0] == "/bin/launchctl")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_401_from_the_gui_session_is_sign_in_needed_with_its_command_and_exit() {
        // B4MacGui.test_a_401_from_the_gui_session_is_sign_in_needed_with_its_command_and_exit
        let root = tempfile::tempdir().expect("root");
        let path = link(&binary(root.path(), OLD), root.path());
        let system = FakeSystem::answering(move |spec| {
            let argv: Vec<String> = spec
                .argv
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect();
            match argv[1].as_str() {
                "bootstrap" => {
                    let folder = Path::new(&spec.argv[3]).parent().expect("folder");
                    std::fs::write(folder.join("stderr"), REFUSED_SIGNIN).expect("stderr");
                }
                "print" if argv[2].contains("/com.ccs.") => {
                    return Ok(CommandOutput {
                        stdout: "state = not running\nlast exit code = 1\n".into(),
                    });
                }
                _ => {}
            }
            Ok(CommandOutput::default())
        });
        let cx = context(root.path(), Platform::Mac, Vec::new(), system);
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("action_required".into(), "cursor_signin_needed".into())
        );
        assert_eq!(
            (
                row["diagnostics"]["tool"].clone(),
                row["diagnostics"]["exitCode"].clone(),
            ),
            ("cursor-agent".into(), 1.into())
        );
        assert_eq!(row["diagnostics"]["cause"], "auth_required");
        assert!(row["diagnostics"].get("tail").is_none());
        assert!(!row.to_string().contains("unauthenticated"));
        assert_eq!(path, root.path().join("home/.local/bin/cursor-agent"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_linux_native_update_keeps_the_running_process_and_the_login_files() {
        // CursorTests.test_linux_native_update_keeps_running_process_and_login_files
        let root = tempfile::tempdir().expect("root");
        let versions = root.path().join("home/.local/share/cursor-agent/versions");
        let (old, newer) = (
            versions.join(OLD).join("cursor-agent"),
            versions.join(NEW).join("cursor-agent"),
        );
        let path = root.path().join("home/.local/bin/cursor-agent");
        let quote =
            |path: &Path| format!("'{}'", path.display().to_string().replace('\'', "'\\''"));
        executable(
            &old,
            &format!(
                "#!/bin/sh\ncase \"$1\" in\n  --version) echo {OLD} ;;\n  update) n={}; mkdir -p \"$(dirname \"$n\")\"; printf '#!/bin/sh\\necho {NEW}\\n' > \"$n\"; chmod 755 \"$n\"; rm -f {link}; ln -s \"$n\" {link} ;;\n  *) exec sleep 60 ;;\nesac\n",
                quote(&newer),
                link = quote(&path)
            ),
        );
        let link = link(&old, root.path());
        let login = root.path().join("home/.cursor/fixture-login");
        std::fs::create_dir_all(login.parent().expect("folder")).expect("folder");
        std::fs::write(&login, "unchanged").expect("login");
        let mut held = std::process::Command::new(&old)
            .arg("hold")
            .spawn()
            .expect("a running Cursor session");
        let cx = context(
            root.path(),
            Platform::Ubuntu,
            Vec::new(),
            Arc::new(common::Native),
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        let running = held.try_wait().expect("wait").is_none();
        let _ = held.kill();
        let _ = held.wait();
        assert_eq!(
            (
                row["status"].clone(),
                row["previousVersion"].clone(),
                row["version"].clone()
            ),
            ("updated".into(), OLD.into(), NEW.into())
        );
        assert!(running, "the updater never stops a running Cursor session");
        assert!(old.is_file());
        assert_eq!(link, path);
        assert_eq!(std::fs::read_to_string(&login).expect("login"), "unchanged");
    }

    #[tokio::test]
    async fn a_deadline_never_starts_the_updater() {
        // CursorTests.test_deadline_never_starts_updater
        let root = tempfile::tempdir().expect("root");
        windows_layout(root.path(), &["cursor-agent.ps1"], &["2026.11.11-b912975"]);
        let system = FakeSystem::answering(|_| panic!("the updater ran"));
        let mut cx = context(root.path(), Platform::Windows, Vec::new(), system.clone());
        cx.deadline = tokio::time::Instant::now();
        let install = detect(&cx).await.expect("found");
        let row = update(&cx, &install).await.to_value();
        assert_eq!(
            (
                row["status"].clone(),
                row["messageCode"].clone(),
                row["updateAttempted"].clone()
            ),
            ("failed".into(), "timeout".into(), false.into())
        );
        assert!(system.runs.lock().expect("runs").is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_copy_first_on_path_makes_the_row_say_so_never_updated() {
        // B2CursorShadow.test_a_copy_first_on_path_makes_the_row_say_so_never_updated
        let root = tempfile::tempdir().expect("root");
        let version = "2026.11.11-b912975";
        let old = binary(root.path(), version);
        let newer = root
            .path()
            .join("home/.local/share/cursor-agent/versions/2026.11.13-abcd/cursor-agent");
        let path = link(&old, root.path());
        let other = root.path().join("home/opt/bin/cursor-agent");
        executable(&other, "#!/bin/sh\necho 2026.01.01-old\n");
        let updated = AtomicBool::new(false);
        let (first, relink) = (other.clone(), path.clone());
        let system = FakeSystem::answering(move |spec| {
            let answer = if spec.argv.last() == Some(&OsString::from("update")) {
                executable(&newer, "#!/bin/sh\necho 2026.11.13-abcd\n");
                std::fs::remove_file(&relink).expect("unlink");
                std::os::unix::fs::symlink(&newer, &relink).expect("relink");
                updated.store(true, Ordering::SeqCst);
                return Ok(CommandOutput::default());
            } else if spec.argv[0] == first.as_os_str() {
                "2026.01.01-old"
            } else if updated.load(Ordering::SeqCst) {
                "2026.11.13-abcd"
            } else {
                version
            };
            Ok(CommandOutput {
                stdout: format!("{answer}\n"),
            })
        });
        let cx = context(
            root.path(),
            Platform::Ubuntu,
            vec![
                other.parent().expect("bin").to_path_buf(),
                path.parent().expect("bin").to_path_buf(),
            ],
            system,
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_ne!(row["status"], "updated");
        assert_eq!(
            (
                row["status"].clone(),
                row["messageCode"].clone(),
                row["version"].clone()
            ),
            (
                "action_required".into(),
                "cursor_shadowed".into(),
                "2026.11.13-abcd".into()
            )
        );
        assert_eq!(row["diagnostics"]["cause"], "shadowed");
        assert_eq!(
            row["diagnostics"]["owner"]["firstOnPath"],
            "~/opt/bin/cursor-agent"
        );
        assert_eq!(notes(&row), ["first_on_path_older"]);
    }

    /// A managed Ubuntu install whose launcher answers `--version` with the
    /// shared text, which the update changes from `before` to `after`.
    #[cfg(unix)]
    async fn version_rows(before: &'static str, after: &'static str) -> Value {
        let root = tempfile::tempdir().expect("root");
        let old = binary(root.path(), before);
        let path = link(&old, root.path());
        let current = Mutex::new(before);
        let system = FakeSystem::answering(move |spec| {
            if spec.argv.last() == Some(&OsString::from("update")) {
                *current.lock().expect("version") = after;
            } else if spec.argv[0] != path.as_os_str() {
                panic!("only the managed launcher may run");
            }
            Ok(CommandOutput {
                stdout: format!("{}\n", current.lock().expect("version")),
            })
        });
        let cx = context(root.path(), Platform::Ubuntu, Vec::new(), system);
        attempt(&CURSOR, &cx).await.to_value()
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cursor_compares_versions_numerically_and_needs_a_strict_increase() {
        // lane_d R2-2, fix_round2 C3
        let row = version_rows("2026.9.30-abcdef1", "2026.10.01-abcdef2").await;
        assert_eq!(
            (
                row["status"].clone(),
                row["messageCode"].clone(),
                row["previousVersion"].clone(),
                row["version"].clone()
            ),
            (
                "updated".into(),
                "cursor_updated".into(),
                "2026.9.30-abcdef1".into(),
                "2026.10.01-abcdef2".into()
            ),
            "as text 2026.10.01 sorts before 2026.9.30"
        );
        let row = version_rows("2026.11.13-abcdef1", "2026.11.11-abcdef2").await;
        assert_eq!(
            (
                row["status"].clone(),
                row["messageCode"].clone(),
                row["previousVersion"].clone(),
                row["version"].clone()
            ),
            (
                "failed".into(),
                "version_went_down".into(),
                "2026.11.13-abcdef1".into(),
                "2026.11.11-abcdef2".into()
            )
        );
        let row = version_rows("2026.11.11-aaaaaaa", "2026.11.11-bbbbbbb").await;
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("failed".into(), "version_unknown".into())
        );
        let row = version_rows("2026.11.11-aaaaaaa", "2026.11.11-aaaaaaa").await;
        assert_eq!(
            (
                row["status"].clone(),
                row["messageCode"].clone(),
                row["updateAttempted"].clone()
            ),
            ("current".into(), "current".into(), true.into())
        );
    }

    #[tokio::test]
    async fn a_401_from_the_owner_pc_is_sign_in_needed_not_update_failed() {
        // CursorWindowsRegressions.test_owner_pc_401_is_sign_in_needed_not_update_failed
        let root = tempfile::tempdir().expect("root");
        windows_layout(
            root.path(),
            &[
                "agent.cmd",
                "agent.ps1",
                "cursor-agent.cmd",
                "cursor-agent.ps1",
            ],
            &["2026.03.20-44cb435", "2026.11.11-b912975"],
        );
        let fake = Windows {
            restricted: false,
            update_stderr: Some(REFUSED_SIGNIN),
        };
        let cx = context(
            root.path(),
            Platform::Windows,
            Vec::new(),
            FakeSystem::answering(move |spec| windows(spec, &fake)),
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            (
                row["status"].clone(),
                row["messageCode"].clone(),
                row["updateAttempted"].clone()
            ),
            (
                "action_required".into(),
                "cursor_signin_needed".into(),
                true.into()
            )
        );
        assert_eq!(
            (
                row["diagnostics"]["cause"].clone(),
                row["diagnostics"]["exitCode"].clone(),
            ),
            ("auth_required".into(), 1.into())
        );
        // Classified from the output in memory; the output itself is not kept.
        assert!(row["diagnostics"].get("tail").is_none());
        assert!(!row.to_string().contains("unauthenticated"));
    }

    #[tokio::test]
    async fn a_401_in_an_unbound_session_log_leaves_signin_undetermined() {
        // CursorWindowsRegressions.test_401_in_an_unbound_session_log_leaves_signin_undetermined
        let root = tempfile::tempdir().expect("root");
        windows_layout(
            root.path(),
            &["cursor-agent.cmd", "cursor-agent.ps1"],
            &["2026.11.11-b912975"],
        );
        let fake = Windows {
            restricted: false,
            update_stderr: Some(""),
        };
        let cx = context(
            root.path(),
            Platform::Windows,
            Vec::new(),
            FakeSystem::answering(move |spec| windows(spec, &fake)),
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("failed".into(), "update_failed".into())
        );
        assert!(notes(&row).contains(&"signin_undetermined".to_owned()));
    }

    /// Python `B13CursorFallback.group_policy`: Group Policy ignores the
    /// process-scoped Bypass; the launcher's own node step answers `direct`.
    fn group_policy(
        spec: &CommandSpec,
        direct: Option<Result<CommandOutput, UpdateFailure>>,
    ) -> Result<CommandOutput, UpdateFailure> {
        if spec.argv.iter().any(|arg| arg == "-File") {
            let mut failure = UpdateFailure::failed();
            failure.exit_code = Some(1);
            if spec.merge_errors {
                failure.output = REFUSED.into();
            }
            return Err(failure);
        }
        if spec
            .argv
            .first()
            .is_some_and(|arg| arg.to_string_lossy().ends_with("node.exe"))
        {
            return direct
                .clone()
                .unwrap_or_else(|| Ok(CommandOutput::default()));
        }
        windows(
            spec,
            &Windows {
                restricted: true,
                update_stderr: None,
            },
        )
    }

    #[tokio::test]
    async fn a_group_policy_that_overrides_bypass_falls_back_to_the_direct_step() {
        // CursorWindowsRegressions.test_group_policy_that_overrides_bypass_is_reported_as_policy_blocked_after_a_direct_try
        // B13CursorFallback.test_a_fallback_that_worked_is_what_ran
        let root = tempfile::tempdir().expect("root");
        let folder = windows_layout(
            root.path(),
            &["cursor-agent.ps1", "cursor-agent.cmd"],
            &["2026.11.11-b912975"],
        );
        let system = FakeSystem::answering(move |spec| group_policy(spec, None));
        let cx = context(root.path(), Platform::Windows, Vec::new(), system.clone());
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(row["status"], "current");
        let runs = system.runs.lock().expect("runs");
        assert!(
            runs.iter().any(|argv| {
                argv.first()
                    .is_some_and(|arg| arg.to_string_lossy().ends_with("node.exe"))
                    && argv.len() == 3
                    && argv[1] == folder.join("versions/2026.11.11-b912975/index.js")
                    && argv.last() == Some(&OsString::from("update"))
            }),
            "the launcher's last step ran directly: {runs:?}"
        );
        drop(runs);
        assert!(notes(&row).contains(&"policy_fallback_direct".to_owned()));
        assert_eq!(
            (
                row["diagnostics"]["tool"].clone(),
                row["diagnostics"]["exitCode"].clone()
            ),
            ("cursor-agent".into(), 0.into())
        );
    }

    #[tokio::test]
    async fn a_group_policy_plus_a_failed_direct_run_is_policy_blocked() {
        // B13CursorFallback.test_group_policy_plus_a_failed_direct_run_is_policy_blocked
        let root = tempfile::tempdir().expect("root");
        windows_layout(
            root.path(),
            &["cursor-agent.ps1", "cursor-agent.cmd"],
            &["2026.11.11-b912975"],
        );
        let system = FakeSystem::answering(move |spec| {
            group_policy(
                spec,
                Some(Err(UpdateFailure {
                    output: "Error: getaddrinfo ENOTFOUND api2.cursor.sh\n".into(),
                    exit_code: Some(1),
                    ..UpdateFailure::failed()
                })),
            )
        });
        let cx = context(root.path(), Platform::Windows, Vec::new(), system);
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("action_required".into(), "policy_blocked".into())
        );
        assert_eq!(
            (
                row["diagnostics"]["cause"].clone(),
                row["diagnostics"]["reason"].clone(),
            ),
            ("policy_blocked".into(), "direct_failed".into())
        );
        assert!(row["diagnostics"].get("tail").is_none());
        assert!(!row.to_string().contains("ENOTFOUND"));
    }

    #[tokio::test]
    async fn same_day_folders_under_group_policy_still_give_a_version() {
        // B13CursorFallback.test_same_day_folders_under_group_policy_still_give_a_version
        let root = tempfile::tempdir().expect("root");
        let folder = windows_layout(
            root.path(),
            &["cursor-agent.ps1", "cursor-agent.cmd"],
            &["2026.11.13-aaaaaaa", "2026.11.13-bbbbbbb"],
        );
        let later = folder.join("versions/2026.11.13-bbbbbbb");
        // Windows opens a folder only with backup semantics, and setting its time needs
        // write-attributes access.
        #[cfg(windows)]
        let handle = {
            use std::os::windows::fs::OpenOptionsExt;
            std::fs::OpenOptions::new()
                .access_mode(0x0100)
                .custom_flags(0x0200_0000)
                .open(&later)
        };
        #[cfg(not(windows))]
        let handle = std::fs::File::open(&later);
        handle
            .expect("folder")
            .set_modified(SystemTime::now() + Duration::from_secs(60))
            .expect("mtime");
        // Same-day folders cannot be ordered the way PowerShell would: the launcher decides.
        let cx = context(
            root.path(),
            Platform::Windows,
            Vec::new(),
            FakeSystem::answering(|_| refused("timeout")),
        );
        assert_eq!(detect(&cx).await.expect("found").probe, Probe::Timeout);
        // When no launcher can run, the most recently written one is the version.
        let cx = context(
            root.path(),
            Platform::Windows,
            Vec::new(),
            FakeSystem::answering(move |spec| group_policy(spec, None)),
        );
        let found = detect(&cx).await.expect("found");
        assert_eq!(
            (found.version.as_deref(), found.probe),
            (Some("2026.11.13-bbbbbbb"), Probe::Ok)
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert!(notes(&row).contains(&"policy_version_from_folder".to_owned()));
    }

    #[tokio::test]
    async fn the_updates_own_error_wins_over_another_sessions_401() {
        // B3CursorLogs
        let root = tempfile::tempdir().expect("root");
        windows_layout(
            root.path(),
            &["cursor-agent.ps1", "cursor-agent.cmd"],
            &["2026.11.11-b912975"],
        );
        let fake = Windows {
            restricted: false,
            update_stderr: Some("Error: ENOSPC: no space left on device, write"),
        };
        let cx = context(
            root.path(),
            Platform::Windows,
            Vec::new(),
            FakeSystem::answering(move |spec| windows(spec, &fake)),
        );
        let row = attempt(&CURSOR, &cx).await.to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("failed".into(), "update_failed".into())
        );
        assert_ne!(row["diagnostics"]["cause"], "auth_required");
        assert_eq!(row["diagnostics"]["cause"], "installer_failed");
        assert_eq!(notes(&row), ["signin_undetermined"]);
    }

    #[test]
    fn policy_and_auth_patterns_do_not_false_match_unrelated_errors() {
        for output in [
            "FullyQualifiedErrorId : CommandNotFoundException",
            "FullyQualifiedErrorId : UnauthorizedAccessException",
            "UnauthorizedAccess without error id",
        ] {
            let (failure, _) = classified(output.into(), "update_failed", Some(1));
            assert_eq!(failure.code, "update_failed", "{output}");
        }
        let (failure, _) = classified(
            "FullyQualifiedErrorId : UnauthorizedAccess".into(),
            "update_failed",
            Some(1),
        );
        assert_eq!(failure.code, "policy_blocked");

        for output in [
            r#"{"status": 500, "error": "server"}"#,
            "retry at port 401 failed",
        ] {
            let (failure, _) = classified(output.into(), "update_failed", Some(1));
            assert_eq!(failure.code, "update_failed", "{output}");
        }
        let (failure, _) = classified(
            r#"{"status": 401, "error": "nope"}"#.into(),
            "update_failed",
            Some(1),
        );
        assert_eq!(
            (failure.code, failure.reason),
            ("cursor_signin_needed", Some("http_401"))
        );
    }

    #[tokio::test]
    async fn windows_staged_files_take_precedence_over_policy_blocked() {
        // app_update_cursor.py:322-327 checks cursor_staged before policy_blocked.
        let root = tempfile::tempdir().expect("root");
        let folder = windows_layout(
            root.path(),
            &["cursor-agent.ps1", "cursor-agent.cmd"],
            &["2026.11.11-b912975"],
        );
        let staged_folder = folder.join("versions").join(format!("{NEW}.new"));
        std::fs::create_dir_all(&staged_folder).expect("staged");
        let cx = context(
            root.path(),
            Platform::Windows,
            Vec::new(),
            FakeSystem::answering(ok),
        );
        let install = detect(&cx).await.expect("found");
        let row = failed(
            &cx,
            &install,
            install.version.clone(),
            true,
            UpdateFailure::new("policy_blocked")
                .with_cause(FailureCause::PolicyBlocked, Some("direct_failed")),
        )
        .await
        .to_value();
        assert_eq!(
            pair(&row, "status", "messageCode"),
            ("action_required".into(), "cursor_update_staged".into())
        );
    }

    fn kept(mut row: Value) -> Value {
        if let Some(names) = row
            .get("diagnostics")
            .and_then(Value::as_object)
            .and_then(|d| d.get("phases"))
            .and_then(Value::as_object)
            .map(|phases| {
                phases
                    .keys()
                    .cloned()
                    .map(Value::String)
                    .collect::<Vec<_>>()
            })
        {
            row["diagnostics"]["phases"] = Value::Array(names);
        }
        row
    }

    fn finish(mut row: AppRow, cx: &Context) -> Value {
        if row.diagnostics.is_none() {
            let value = row.to_value();
            row.diagnostics = Some(cx.record(|record| record.export(&value)));
        }
        kept(row.to_value())
    }

    #[tokio::test]
    async fn matches_python_oracle_rows() {
        let mut rows = serde_json::Map::new();
        let mut add = |name: &str, row: Value| {
            rows.insert(name.into(), row);
        };

        // Windows regression cases
        for (name, fake) in [
            (
                "cursor_restricted_policy",
                Windows {
                    restricted: true,
                    update_stderr: None,
                },
            ),
            (
                "cursor_owner_pc_401",
                Windows {
                    restricted: false,
                    update_stderr: Some(REFUSED_SIGNIN),
                },
            ),
            (
                "cursor_401_undetermined",
                Windows {
                    restricted: false,
                    update_stderr: Some(""),
                },
            ),
            (
                "cursor_b3_enospc",
                Windows {
                    restricted: false,
                    update_stderr: Some("Error: ENOSPC: no space left on device, write"),
                },
            ),
        ] {
            let root = tempfile::tempdir().expect("root");
            windows_layout(
                root.path(),
                &[
                    "agent.cmd",
                    "agent.ps1",
                    "cursor-agent.cmd",
                    "cursor-agent.ps1",
                ],
                &["2026.03.20-44cb435", "2026.11.11-b912975"],
            );
            let cx = context(
                root.path(),
                Platform::Windows,
                Vec::new(),
                FakeSystem::answering(move |spec| windows(spec, &fake)),
            );
            let install = detect(&cx).await.expect("found");
            add(name, finish(update(&cx, &install).await, &cx));
        }

        // Group policy direct fallback (ok, failed, and staged over policy)
        for (name, direct, stage_on_direct) in [
            ("cursor_group_policy_direct", None, false),
            ("cursor_b13_direct_ok", None, false),
            (
                "cursor_b13_direct_failed",
                Some(Err(UpdateFailure {
                    output: "Error: getaddrinfo ENOTFOUND api2.cursor.sh\n".into(),
                    exit_code: Some(1),
                    ..UpdateFailure::failed()
                })),
                false,
            ),
            (
                "cursor_staged_over_policy",
                Some(Err(UpdateFailure {
                    output: "Error: getaddrinfo ENOTFOUND api2.cursor.sh\n".into(),
                    exit_code: Some(1),
                    ..UpdateFailure::failed()
                })),
                true,
            ),
        ] {
            let root = tempfile::tempdir().expect("root");
            let folder = windows_layout(
                root.path(),
                &["cursor-agent.ps1", "cursor-agent.cmd"],
                &["2026.11.11-b912975"],
            );
            let staged_dir = folder.join("versions").join(format!("{NEW}.new"));
            let cx = context(
                root.path(),
                Platform::Windows,
                Vec::new(),
                FakeSystem::answering(move |spec| {
                    if stage_on_direct
                        && spec
                            .argv
                            .first()
                            .is_some_and(|arg| arg.to_string_lossy().ends_with("node.exe"))
                    {
                        std::fs::create_dir_all(&staged_dir).expect("staged");
                    }
                    group_policy(spec, direct.clone())
                }),
            );
            let install = detect(&cx).await.expect("found");
            add(name, finish(update(&cx, &install).await, &cx));
        }

        // Windows .cmd-only update
        {
            let root = tempfile::tempdir().expect("root");
            windows_layout(root.path(), &["cursor-agent.cmd"], &[]);
            let probes = AtomicUsize::new(0);
            let cx = context(
                root.path(),
                Platform::Windows,
                Vec::new(),
                FakeSystem::answering(move |spec| {
                    if script_of(&spec.argv).contains(" --version") {
                        let answer = match probes.fetch_add(1, Ordering::SeqCst) {
                            0 => OLD,
                            _ => NEW,
                        };
                        return Ok(CommandOutput {
                            stdout: format!("{answer}\n"),
                        });
                    }
                    Ok(CommandOutput::default())
                }),
            );
            let install = detect(&cx).await.expect("found");
            add(
                "cursor_cmd_update",
                finish(update(&cx, &install).await, &cx),
            );
        }

        // Windows staged (success + failure)
        for (name, stderr) in [
            ("cursor_staged_success", None),
            (
                "cursor_staged_failure",
                Some("Error: EPERM: another run holds the files"),
            ),
        ] {
            let root = tempfile::tempdir().expect("root");
            let folder = windows_layout(
                root.path(),
                &["cursor-agent.ps1", "cursor-agent.cmd"],
                &["2026.11.11-b912975"],
            );
            std::fs::create_dir_all(folder.join("versions").join(format!("{NEW}.new")))
                .expect("staged");
            let fake = Windows {
                restricted: false,
                update_stderr: stderr,
            };
            let cx = context(
                root.path(),
                Platform::Windows,
                Vec::new(),
                FakeSystem::answering(move |spec| windows(spec, &fake)),
            );
            let install = detect(&cx).await.expect("found");
            add(name, finish(update(&cx, &install).await, &cx));
        }

        // Unix-only fixtures (B2 shadow, Mac GUI update + 401, Linux native, numeric versions, attempt_one)
        #[cfg(unix)]
        {
            // B2 shadow
            let root = tempfile::tempdir().expect("root");
            let version = "2026.11.11-b912975";
            let old = binary(root.path(), version);
            let newer = root
                .path()
                .join("home/.local/share/cursor-agent/versions/2026.11.13-abcd/cursor-agent");
            let path = link(&old, root.path());
            let other = root.path().join("home/opt/bin/cursor-agent");
            executable(&other, "#!/bin/sh\necho 2026.01.01-old\n");
            let updated = AtomicBool::new(false);
            let (first, relink) = (other.clone(), path.clone());
            let cx = context(
                root.path(),
                Platform::Ubuntu,
                vec![
                    other.parent().expect("bin").to_path_buf(),
                    path.parent().expect("bin").to_path_buf(),
                ],
                FakeSystem::answering(move |spec| {
                    let answer = if spec.argv.last() == Some(&OsString::from("update")) {
                        executable(&newer, "#!/bin/sh\necho 2026.11.13-abcd\n");
                        std::fs::remove_file(&relink).expect("unlink");
                        std::os::unix::fs::symlink(&newer, &relink).expect("relink");
                        updated.store(true, Ordering::SeqCst);
                        return Ok(CommandOutput::default());
                    } else if spec.argv[0] == first.as_os_str() {
                        "2026.01.01-old"
                    } else if updated.load(Ordering::SeqCst) {
                        "2026.11.13-abcd"
                    } else {
                        version
                    };
                    Ok(CommandOutput {
                        stdout: format!("{answer}\n"),
                    })
                }),
            );
            let install = detect(&cx).await.expect("found");
            add("cursor_b2_shadow", finish(update(&cx, &install).await, &cx));

            // B4 Mac GUI 401
            let root = tempfile::tempdir().expect("root");
            link(&binary(root.path(), "2026.11.11-b912975"), root.path());
            let cx = context(
                root.path(),
                Platform::Mac,
                Vec::new(),
                FakeSystem::answering(move |spec| {
                    let argv: Vec<String> = spec
                        .argv
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect();
                    match argv[1].as_str() {
                        "bootstrap" => {
                            let folder = Path::new(&spec.argv[3]).parent().expect("folder");
                            std::fs::write(folder.join("stderr"), REFUSED_SIGNIN).expect("stderr");
                        }
                        "print" if argv[2].contains("/com.ccs.") => {
                            return Ok(CommandOutput {
                                stdout: "state = not running\nlast exit code = 1\n".into(),
                            });
                        }
                        _ => {}
                    }
                    Ok(CommandOutput::default())
                }),
            );
            let install = detect(&cx).await.expect("found");
            add(
                "cursor_b4_gui_401",
                finish(update(&cx, &install).await, &cx),
            );

            // Mac GUI update success
            let root = tempfile::tempdir().expect("root");
            let old = binary(root.path(), OLD);
            let newer = binary(root.path(), NEW);
            let path = link(&old, root.path());
            let cx = context(
                root.path(),
                Platform::Mac,
                Vec::new(),
                FakeSystem::answering(move |spec| {
                    let argv: Vec<String> = spec
                        .argv
                        .iter()
                        .map(|arg| arg.to_string_lossy().into_owned())
                        .collect();
                    if argv[1] == "print" && argv[2].contains("/com.ccs.") {
                        std::fs::remove_file(&path).expect("unlink");
                        std::os::unix::fs::symlink(&newer, &path).expect("relink");
                        return Ok(CommandOutput {
                            stdout: "state = not running\nlast exit code = 0\n".into(),
                        });
                    }
                    Ok(CommandOutput::default())
                }),
            );
            let install = detect(&cx).await.expect("found");
            add(
                "cursor_mac_update",
                finish(update(&cx, &install).await, &cx),
            );

            // Linux native update
            let root = tempfile::tempdir().expect("root");
            let versions = root.path().join("home/.local/share/cursor-agent/versions");
            let (old, newer) = (
                versions.join(OLD).join("cursor-agent"),
                versions.join(NEW).join("cursor-agent"),
            );
            let path = root.path().join("home/.local/bin/cursor-agent");
            let quote = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', "'\\''"));
            executable(
                &old,
                &format!(
                    "#!/bin/sh\ncase \"$1\" in\n  --version) echo {OLD} ;;\n  update) n={}; mkdir -p \"$(dirname \"$n\")\"; printf '#!/bin/sh\\necho {NEW}\\n' > \"$n\"; chmod 755 \"$n\"; rm -f {link}; ln -s \"$n\" {link} ;;\nesac\n",
                    quote(&newer),
                    link = quote(&path)
                ),
            );
            link(&old, root.path());
            let cx = context(
                root.path(),
                Platform::Ubuntu,
                Vec::new(),
                Arc::new(common::Native),
            );
            let install = detect(&cx).await.expect("found");
            add(
                "cursor_linux_native",
                finish(update(&cx, &install).await, &cx),
            );

            // Numeric version comparison cases
            for (name, before, after) in [
                (
                    "cursor_ver_numeric_up",
                    "2026.9.30-abcdef1",
                    "2026.10.01-abcdef2",
                ),
                (
                    "cursor_ver_went_down",
                    "2026.11.13-abcdef1",
                    "2026.11.11-abcdef2",
                ),
                (
                    "cursor_ver_same_day_diff_hash",
                    "2026.11.11-aaaaaaa",
                    "2026.11.11-bbbbbbb",
                ),
                (
                    "cursor_ver_same_current",
                    "2026.11.11-aaaaaaa",
                    "2026.11.11-aaaaaaa",
                ),
            ] {
                let root = tempfile::tempdir().expect("root");
                let old = binary(root.path(), before);
                link(&old, root.path());
                let current = Mutex::new(before);
                let cx = context(
                    root.path(),
                    Platform::Ubuntu,
                    Vec::new(),
                    FakeSystem::answering(move |spec| {
                        if spec.argv.last() == Some(&OsString::from("update")) {
                            *current.lock().expect("version") = after;
                            return Ok(CommandOutput::default());
                        }
                        Ok(CommandOutput {
                            stdout: format!("{}\n", current.lock().expect("version")),
                        })
                    }),
                );
                let install = detect(&cx).await.expect("found");
                add(name, finish(update(&cx, &install).await, &cx));
            }

            // attempt_one edge cases
            let root = tempfile::tempdir().expect("root");
            let cx = context(
                root.path(),
                Platform::Ubuntu,
                Vec::new(),
                FakeSystem::answering(ok),
            );
            add(
                "cursor_absent",
                kept(attempt(&CURSOR, &cx).await.to_value()),
            );
            let outside = cx.host.home.join(".local/bin/cursor-agent");
            executable(&outside, "outside");
            let cx = context(
                root.path(),
                Platform::Ubuntu,
                Vec::new(),
                FakeSystem::answering(ok),
            );
            add(
                "cursor_unmanaged",
                kept(attempt(&CURSOR, &cx).await.to_value()),
            );
            std::fs::remove_file(&outside).expect("remove");
            link(&binary(root.path(), OLD), root.path());
            let cx = context(
                root.path(),
                Platform::Ubuntu,
                Vec::new(),
                FakeSystem::answering(|_| refused("timeout")),
            );
            add(
                "cursor_probe_timeout",
                kept(attempt(&CURSOR, &cx).await.to_value()),
            );
            let cx = context(
                root.path(),
                Platform::Ubuntu,
                Vec::new(),
                FakeSystem::answering(|_| {
                    Ok(CommandOutput {
                        stdout: "nothing here".into(),
                    })
                }),
            );
            add(
                "cursor_probe_unparsed",
                kept(attempt(&CURSOR, &cx).await.to_value()),
            );
        }

        assert_eq!(rows.len(), if cfg!(unix) { 23 } else { 11 });
        if let Ok(path) = std::env::var("AAC_W4_ORACLE_JSON") {
            let oracle: serde_json::Map<String, Value> =
                serde_json::from_str(&std::fs::read_to_string(&path).expect("oracle json"))
                    .expect("oracle map");
            let mut diffs = Vec::new();
            let count = rows.len();
            for (name, got) in rows {
                let Some(want) = oracle.get(&name) else {
                    diffs.push(format!("{name}: missing in oracle"));
                    continue;
                };
                if &got != want {
                    diffs.push(format!("{name}:\n  rust:   {got}\n  python: {want}"));
                }
            }
            assert!(
                diffs.is_empty(),
                "Cursor oracle row diffs ({}):\n{}",
                diffs.len(),
                diffs.join("\n")
            );
            eprintln!("cursor oracle diff: {count}/{count} exact row matches");
        }
    }
}
