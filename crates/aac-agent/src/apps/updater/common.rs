//! Shared bounded operations (Python `app_update_common.py`): the command runner,
//! the row diagnostics recorder, downloads and the private-folder helpers.
use super::{BoxFuture, FailureCause, Platform, ProcessTable, System, UpdateFailure};
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{LazyLock, Mutex},
    time::Duration,
};
use tokio::{io::AsyncReadExt, time::Instant};

/// Python `NO_AUTO_UPDATE`: every child runs with the apps' own auto-updaters off.
pub(crate) const NO_AUTO_UPDATE: [(&str, &str); 5] = [
    ("MUSE_NO_AUTO_UPDATE", "1"),
    ("AGY_CLI_DISABLE_AUTO_UPDATE", "true"),
    ("DISABLE_AUTOUPDATER", "1"),
    ("NO_UPDATE_NOTIFIER", "1"),
    ("CODEX_NON_INTERACTIVE", "1"),
];

/// Python `TOOLS`: the only words a row's `diagnostics.tool` can hold. These are executable
/// names (one space-separated vocabulary), not a provider table.
pub(crate) const TOOLS: &str = "agy antigravity apt apt-cache apt-get bun claude codesign codex cursor cursor-agent \
     ditto dpkg dpkg-query hdiutil launchctl lsof msiexec muse node npm npx omp open pnpm \
     powershell ps pwsh python spctl sudo systemctl systemd-run t3 winget wt zcode";

/// Python `REASON`: a fixed reason or note word.
pub(crate) static REASON: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\A[a-z0-9][a-z0-9_.:-]{0,63}\z").expect("reason word"));

/// How a child's environment is built.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) enum EnvMode {
    /// Python `environment(extra)`: this process's environment, the no-auto-update
    /// switches, then `extra`, without the dashboard's secrets.
    #[default]
    Inherit,
    /// Python `preserve_env=True`: exactly `extra` (minus the dashboard's secrets).
    Exact,
}

/// One fixed command (Python `command()` keyword arguments).
#[derive(Clone, Debug)]
pub(crate) struct CommandSpec {
    pub argv: Vec<OsString>,
    pub timeout: Duration,
    pub env: Vec<(OsString, OsString)>,
    /// Variables to remove after `env` is built (Python callers that `pop` a key).
    pub env_remove: Vec<OsString>,
    pub env_mode: EnvMode,
    /// Return stdout (`capture=True`); otherwise stdout goes to the null device.
    pub capture: bool,
    pub capture_limit: usize,
    /// Windows: give the child a console window.
    #[cfg(windows)]
    pub visible: bool,
    /// Python `errors=True`: stderr merged into stdout, kept in the failure's output.
    pub merge_errors: bool,
    pub cwd: Option<PathBuf>,
    /// Stop the child's owned process tree at the timeout, not only its group.
    pub kill_tree: bool,
    /// The diagnostics tool word's source (its first word), instead of argv[0].
    pub label: Option<&'static str>,
    /// Keep stderr's last 64 KiB in memory for classification (Python `stderr_tail`).
    pub stderr_tail: bool,
    /// Leave this run out of the row's record (read-only probes).
    pub quiet: bool,
    /// Decode as UTF-8, else the console's OEM code page (Python `console=True`).
    pub console: bool,
}

impl CommandSpec {
    /// Python `command(argv)` defaults: 30 s, inherited environment, stderr tail kept.
    pub(crate) fn new<I, S>(argv: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Self {
            argv: argv.into_iter().map(Into::into).collect(),
            timeout: Duration::from_secs(30),
            env: Vec::new(),
            env_remove: Vec::new(),
            env_mode: EnvMode::Inherit,
            capture: false,
            capture_limit: 65_536,
            #[cfg(windows)]
            visible: false,
            merge_errors: false,
            cwd: None,
            kill_tree: false,
            label: None,
            stderr_tail: true,
            quiet: false,
            console: false,
        }
    }

    /// Python `powershell(script)`: `-EncodedCommand` with UTF-8 output, captured.
    pub(crate) fn powershell(powershell: impl Into<OsString>, script: &str) -> Self {
        let mut argv = vec![powershell.into()];
        argv.extend(crate::apps::ps_command(script.to_owned()));
        Self::new(argv).capture()
    }

    pub(crate) fn timeout(mut self, seconds: Duration) -> Self {
        self.timeout = seconds;
        self
    }

    pub(crate) fn capture(mut self) -> Self {
        self.capture = true;
        self
    }

    pub(crate) fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub(crate) fn errors(mut self) -> Self {
        self.merge_errors = true;
        self
    }

    pub(crate) fn cwd(mut self, folder: impl Into<PathBuf>) -> Self {
        self.cwd = Some(folder.into());
        self
    }

    pub(crate) fn label(mut self, label: &'static str) -> Self {
        self.label = Some(label);
        self
    }

    pub(crate) fn quiet(mut self) -> Self {
        self.quiet = true;
        self
    }

    pub(crate) fn no_stderr(mut self) -> Self {
        self.stderr_tail = false;
        self
    }

    pub(crate) fn kill_tree(mut self) -> Self {
        self.kill_tree = true;
        self
    }

    #[cfg(windows)]
    pub(crate) fn visible(mut self) -> Self {
        self.visible = true;
        self
    }

    pub(crate) fn console(mut self) -> Self {
        self.console = true;
        self
    }
}

/// A command that exited 0. `stdout` is empty unless the spec captured it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CommandOutput {
    pub stdout: String,
}

/// A finished download (Python returns nothing; the size is for callers that check it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Downloaded {
    pub bytes: u64,
    /// The final URL after redirects; always https.
    pub url: String,
}

/// Python `remote_fingerprint`: the final URL, size and validator of a remote file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Fingerprint {
    pub url: String,
    pub size: u64,
    pub validator: String,
}

/// One recorded run: its tool word, exit code and whether it failed. Never output or argv.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Run {
    pub tool: &'static str,
    pub exit_code: Option<i64>,
    pub failed: bool,
}

/// One row's record (Python `Diagnostics`, owner decision U13).
#[derive(Debug, Default)]
pub(crate) struct Diagnostics {
    phases: Vec<(&'static str, u64)>,
    current: Option<(&'static str, Instant)>,
    runs: Vec<Run>,
    pub owner: Option<Value>,
    cause: Option<FailureCause>,
    reason: Option<String>,
    notes: Vec<String>,
}

impl Diagnostics {
    const RUNS: usize = 16;

    /// Start timing `name`, or stop timing with `None`.
    pub(crate) fn phase(&mut self, name: Option<&'static str>) {
        let now = Instant::now();
        if let Some((previous, since)) = self.current.take() {
            let spent = u64::try_from(now.duration_since(since).as_millis()).unwrap_or(u64::MAX);
            match self.phases.iter_mut().find(|(phase, _)| *phase == previous) {
                Some(entry) => entry.1 = entry.1.saturating_add(spent),
                None => self.phases.push((previous, spent)),
            }
        }
        self.current = name.map(|name| (name, now));
    }

    /// Record one run (Python `ran`). `failed` defaults to a non-zero exit code.
    pub(crate) fn ran(
        &mut self,
        argv: &[OsString],
        label: Option<&str>,
        exit_code: Option<i64>,
        failed: Option<bool>,
    ) {
        let failed = failed.unwrap_or(!matches!(exit_code, None | Some(0)));
        self.runs.push(Run {
            tool: tool_id(argv, label),
            exit_code,
            failed,
        });
        if self.runs.len() > Self::RUNS {
            self.runs.remove(0);
        }
    }

    /// The run that decided a row: the last success for a clean row, else the last failure.
    pub(crate) fn deciding(&self, success: bool) -> Option<&Run> {
        self.runs
            .iter()
            .rev()
            .find(|run| run.failed != success)
            .or_else(|| if success { None } else { self.runs.last() })
    }

    /// Name the row's cause and, optionally, a fixed reason word.
    pub(crate) fn blame(&mut self, cause: FailureCause, reason: Option<&str>) {
        self.cause = Some(cause);
        if let Some(reason) = reason {
            self.reason = Some(if REASON.is_match(reason) {
                reason.to_owned()
            } else {
                "unlisted".to_owned()
            });
        }
    }

    /// Add a fixed note word (at most six, no repeats).
    pub(crate) fn note(&mut self, word: &str) {
        if REASON.is_match(word)
            && !self.notes.iter().any(|note| note == word)
            && self.notes.len() < 6
        {
            self.notes.push(word.to_owned());
        }
    }

    /// The row's `diagnostics` object (Python `export`).
    pub(crate) fn export(&mut self, row: &Value) -> Value {
        self.phase(None);
        let failure = self.deciding(false).cloned();
        let (cause, reason) = match self.cause {
            Some(cause) => (cause.as_str(), self.reason.clone()),
            None => {
                let exit = failure
                    .as_ref()
                    .filter(|run| run.failed)
                    .and_then(|run| run.exit_code);
                let (cause, derived) = cause_for(row, exit);
                (cause, self.reason.clone().or(derived))
            }
        };
        let mut value = Map::new();
        value.insert("cause".into(), cause.into());
        if let Some(reason) = reason {
            value.insert("reason".into(), reason.into());
        }
        if let Some(run) = self.deciding(cause == "none") {
            value.insert("tool".into(), run.tool.into());
            if let Some(code) = run
                .exit_code
                .filter(|code| (-(1_i64 << 31)..(1_i64 << 32)).contains(code))
            {
                value.insert("exitCode".into(), code.into());
            }
        }
        let phases: Map<String, Value> = self
            .phases
            .iter()
            .take(8)
            .filter(|(name, _)| REASON.is_match(name))
            .map(|(name, ms)| ((*name).to_owned(), Value::from((*ms).min(86_400_000))))
            .collect();
        if !phases.is_empty() {
            value.insert("phases".into(), Value::Object(phases));
        }
        if let Some(owner) = &self.owner {
            value.insert("owner".into(), owner.clone());
        }
        if !self.notes.is_empty() {
            value.insert("notes".into(), json!(self.notes));
        }
        Value::Object(value)
    }
}

/// Python `_CODE_CAUSES` and `cause_for`: the cause of a row whose updater named none.
pub(crate) fn cause_for(row: &Value, exit_code: Option<i64>) -> (&'static str, Option<String>) {
    let status = row["status"].as_str().unwrap_or_default();
    let code = row["messageCode"].as_str();
    let mapped = code.and_then(code_cause);
    if matches!(
        status,
        "updated" | "current" | "not_installed" | "skipped" | "held"
    ) && mapped.is_none()
    {
        return ("none", None);
    }
    if let Some(cause) = mapped {
        return (cause, None);
    }
    if code == Some("update_failed") && exit_code.is_some_and(|code| code != 0) {
        return ("installer_failed", None);
    }
    let reason = code
        .filter(|code| REASON.is_match(code))
        .unwrap_or("unlisted");
    ("unknown", Some(reason.to_owned()))
}

fn code_cause(code: &str) -> Option<&'static str> {
    Some(match code {
        "signin_needed" | "antigravity_acp_signin_hold" => "signin_needed",
        "cursor_signin_needed" => "auth_required",
        "policy_blocked" => "policy_blocked",
        "wrong_prefix" => "wrong_prefix",
        "prefix_not_writable" => "not_writable",
        "t3_session_issue_unknown" | "t3_session_reply_lost" => "lost_session_reply",
        "t3_absent" => "t3_absent",
        "t3_unreachable" | "t3_not_ready" => "t3_not_ready",
        "t3_restart_pending" => "t3_restart_pending",
        "in_use"
        | "quit_first"
        | "busy"
        | "codex_busy"
        | "cursor_update_staged"
        | "package_in_use" => "in_use",
        "timeout" | "check_timeout" | "host_timeout" | "t3_install_timeout" => "timeout",
        "t3_install_failed:installer" | "version_went_down" => "installer_failed",
        _ => return None,
    })
}

/// Python `tool_id`: which tool a run used, one word from [`TOOLS`] or `unlisted`.
pub(crate) fn tool_id(argv: &[OsString], label: Option<&str>) -> &'static str {
    static SUFFIX: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?i)\.(?:exe|cmd|bat|ps1|app)$").expect("tool suffix")
    });
    static DIGITS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[0-9.]+$").expect("tool digits"));
    let word = match label {
        Some(label) if !label.is_empty() => {
            label.split_whitespace().next().unwrap_or("").to_owned()
        }
        _ => argv
            .first()
            .map(|program| {
                let text = program.to_string_lossy();
                text.rsplit(['\\', '/']).next().unwrap_or("").to_owned()
            })
            .unwrap_or_default(),
    };
    let word = DIGITS
        .replace(&SUFFIX.replace(&word, ""), "")
        .to_lowercase();
    TOOLS
        .split(' ')
        .find(|tool| *tool == word)
        .unwrap_or("unlisted")
}

/// Python `version_text`: the first version-looking run in a probe's output.
pub(crate) fn version_text(text: &str) -> Option<String> {
    crate::apps::version(Some(text))
}

/// Python `version_tuple`: the numeric parts of a version, for ordering.
pub(crate) fn version_tuple(value: &str) -> Vec<u64> {
    static NUMBERS: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"[0-9]+").expect("version numbers"));
    let head = value.split('-').next().unwrap_or_default();
    NUMBERS
        .find_iter(head)
        .filter_map(|part| part.as_str().parse().ok())
        .collect()
}

/// True for a Windows junction (a reparse point that is a mount point).
pub(crate) fn is_junction(info: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        info.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        let _ = info;
        false
    }
}

/// Create a folder (and parents) readable only by this user.
pub(crate) fn private_dir(path: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path)
}

/// Python `write_private_json`: an atomic 0600 JSON write.
pub(crate) fn write_private_json(path: &Path, value: &Value) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        private_dir(parent)?;
    }
    let temporary = path.with_file_name(format!(
        "{}.{}.tmp",
        path.file_name()
            .map(|name| name.to_string_lossy())
            .unwrap_or_default(),
        std::process::id()
    ));
    let result = (|| {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        serde_json::to_writer(&mut file, value)?;
        std::io::Write::flush(&mut file)?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    let _ = std::fs::remove_file(&temporary);
    result
}

/// The native host services: real processes, real network.
pub(crate) struct Native;

const USER_AGENT: &str = "CCS-Installed-App-Updater/1.0";

impl System for Native {
    fn run<'a>(
        &'a self,
        spec: &'a CommandSpec,
        diagnostics: &'a Mutex<Diagnostics>,
    ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>> {
        Box::pin(async move {
            let outcome = run_bounded(spec).await;
            let mut record = diagnostics
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let mut ran = |exit: Option<i64>, failed: Option<bool>, note: Option<&str>| {
                if !spec.quiet {
                    record.ran(&spec.argv, spec.label, exit, failed);
                    if let Some(note) = note {
                        record.note(note);
                    }
                }
            };
            match outcome {
                Bounded::Timeout => {
                    ran(None, Some(true), Some("timed_out"));
                    Err(UpdateFailure::new("timeout"))
                }
                Bounded::NotStarted => {
                    ran(None, Some(true), Some("not_started"));
                    Err(UpdateFailure {
                        reason: Some("not_started"),
                        ..UpdateFailure::failed()
                    })
                }
                Bounded::Exited {
                    code,
                    stdout,
                    stderr,
                } => {
                    if code != 0 {
                        ran(Some(code), None, None);
                        let text = if spec.merge_errors {
                            let text = decode(&stdout, spec.console);
                            let start = text.len().saturating_sub(262_144);
                            text[text.floor_char_boundary(start)..].to_owned()
                        } else {
                            decode(&stderr, spec.console)
                        };
                        return Err(UpdateFailure {
                            output: text,
                            exit_code: Some(code),
                            ..UpdateFailure::failed()
                        });
                    }
                    ran(Some(0), None, None);
                    if spec.capture && stdout.len() > spec.capture_limit.min(2 * 1024 * 1024) {
                        return Err(UpdateFailure::failed());
                    }
                    Ok(CommandOutput {
                        stdout: if spec.capture {
                            decode(&stdout, spec.console)
                        } else {
                            String::new()
                        },
                    })
                }
            }
        })
    }

    fn processes(&self, platform: Platform) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>> {
        Box::pin(super::processes::native_scan(platform))
    }

    fn download<'a>(
        &'a self,
        url: &'a str,
        destination: &'a Path,
        maximum: u64,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>> {
        Box::pin(download(url, destination, maximum, timeout))
    }

    fn fetch<'a>(
        &'a self,
        url: &'a str,
        prefix: &'a str,
        maximum: u64,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>> {
        Box::pin(fetch(url, prefix, maximum, timeout))
    }

    fn fingerprint<'a>(
        &'a self,
        url: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Option<Fingerprint>> {
        Box::pin(fingerprint(url, timeout))
    }

    fn range<'a>(
        &'a self,
        url: &'a str,
        range: &'a str,
        timeout: Duration,
    ) -> BoxFuture<'a, Result<super::shared::RangeReply, UpdateFailure>> {
        Box::pin(range_get(url, range, timeout))
    }
}

enum Bounded {
    Exited {
        code: i64,
        stdout: Vec<u8>,
        stderr: Vec<u8>,
    },
    Timeout,
    NotStarted,
}

fn child_environment(spec: &CommandSpec) -> Vec<(OsString, OsString)> {
    let mut env: Vec<(OsString, OsString)> = match spec.env_mode {
        EnvMode::Inherit => std::env::vars_os().collect(),
        EnvMode::Exact => Vec::new(),
    };
    let mut set = |key: OsString, value: OsString| {
        env.retain(|(existing, _)| !same_key(existing, &key));
        env.push((key, value));
    };
    if spec.env_mode == EnvMode::Inherit {
        for (key, value) in NO_AUTO_UPDATE {
            set(key.into(), value.into());
        }
    }
    for (key, value) in &spec.env {
        set(key.clone(), value.clone());
    }
    env.retain(|(key, _)| {
        let name = key.to_string_lossy().to_ascii_uppercase();
        !(name == "CCS_SESSION_SECRET"
            || name.starts_with("CCS_DASHBOARD_")
            || name.ends_with("_PASSWORD_HASH"))
            && !spec.env_remove.iter().any(|removed| same_key(key, removed))
    });
    env
}

fn same_key(left: &OsString, right: &OsString) -> bool {
    if cfg!(windows) {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    } else {
        left == right
    }
}

async fn run_bounded(spec: &CommandSpec) -> Bounded {
    let Some(program) = spec.argv.first() else {
        return Bounded::NotStarted;
    };
    let mut command = tokio::process::Command::new(program);
    command
        .args(&spec.argv[1..])
        .env_clear()
        .envs(child_environment(spec))
        .stdin(Stdio::null())
        .stdout(if spec.capture || spec.merge_errors {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if spec.merge_errors || spec.stderr_tail {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        command.current_dir(cwd);
    }
    // POSIX: its own process group, so a timeout stops the group. Windows: no
    // console window unless asked for.
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    if !spec.visible {
        command.creation_flags(0x08000000);
    }
    let Ok(mut child) = command.spawn() else {
        return Bounded::NotStarted;
    };
    let pid = child.id();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let merged = spec.merge_errors;
    let capture = async {
        let out = async {
            match stdout {
                Some(stream) => read_tail(stream, usize::MAX >> 1, false).await,
                None => Vec::new(),
            }
        };
        let err = async {
            match stderr {
                Some(stream) => read_tail(stream, 65_536, true).await,
                None => Vec::new(),
            }
        };
        let (mut out, err, status) = tokio::join!(out, err, child.wait());
        if merged {
            out.extend_from_slice(&err);
        }
        (out, err, status)
    };
    match tokio::time::timeout(spec.timeout, capture).await {
        Ok((stdout, stderr, Ok(status))) => Bounded::Exited {
            code: exit_code(&status),
            stdout,
            stderr,
        },
        Ok((_, _, Err(_))) => Bounded::NotStarted,
        Err(_) => {
            stop_group(pid, spec.kill_tree).await;
            Bounded::Timeout
        }
    }
}

/// Python `_stop_group`: kill a timed-out child's group (POSIX) or the child
/// itself (Windows, where `kill_on_drop` stops the root when the future drops).
async fn stop_group(pid: Option<u32>, tree: bool) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        if tree {
            super::processes::kill_tree(pid).await;
        }
        let Ok(pid) = i32::try_from(pid) else {
            return;
        };
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    #[cfg(windows)]
    if tree && let Some(pid) = pid {
        super::processes::kill_tree(pid).await;
    }
    #[cfg(not(any(unix, windows)))]
    let _ = (pid, tree);
}

fn exit_code(status: &std::process::ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        // Windows HRESULTs are unsigned 32-bit values; keep them as such.
        if cfg!(windows) {
            return i64::from(code as u32);
        }
        return i64::from(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -i64::from(signal);
        }
    }
    -1
}

/// Read a stream to its end, keeping at most `cap` bytes: the head, or with
/// `tail` the last bytes from their first whole line (Python's stderr sink).
async fn read_tail(
    mut stream: impl tokio::io::AsyncRead + Unpin,
    cap: usize,
    tail: bool,
) -> Vec<u8> {
    let mut kept = Vec::new();
    let mut chunk = vec![0; 64 * 1024];
    let mut cut = false;
    loop {
        match stream.read(&mut chunk).await {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                kept.extend_from_slice(&chunk[..read]);
                if kept.len() > cap {
                    cut = true;
                    if tail {
                        let start = kept.len() - cap;
                        kept.drain(..start);
                    } else {
                        // Over the cap: keep reading so the child never blocks; the
                        // caller sees an oversized capture and refuses it.
                        kept.truncate(cap + 1);
                    }
                }
            }
        }
    }
    if tail && cut {
        match kept.iter().position(|byte| *byte == b'\n') {
            Some(newline) => {
                kept.drain(..=newline);
            }
            None => kept.clear(),
        }
    }
    kept
}

/// Python `decode_console` / UTF-8 with replacement. Windows' OEM fallback is
/// approximated as Latin-1, which keeps every byte as a character.
pub(crate) fn decode(bytes: &[u8], console: bool) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) if console && cfg!(windows) => bytes.iter().map(|byte| char::from(*byte)).collect(),
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

fn client(timeout: Duration) -> Result<reqwest::Client, UpdateFailure> {
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(15).min(timeout))
        .https_only(true)
        .build()
        .map_err(|_| UpdateFailure::failed())
}

fn challenge_page(head: &[u8]) -> bool {
    let head = &head[..head.len().min(4096)];
    let start = head
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(head.len());
    let lead: Vec<u8> = head[start..]
        .iter()
        .take(64)
        .map(u8::to_ascii_lowercase)
        .collect();
    lead.starts_with(b"<!doctype html") || lead.starts_with(b"<html")
}

/// Python `download`.
pub(crate) async fn download(
    url: &str,
    destination: &Path,
    maximum: u64,
    timeout: Duration,
) -> Result<Downloaded, UpdateFailure> {
    use tokio::io::AsyncWriteExt;
    let deadline = Instant::now() + timeout;
    let response = tokio::time::timeout(Duration::from_secs(15), client(timeout)?.get(url).send())
        .await
        .map_err(|_| UpdateFailure::failed())?
        .map_err(|_| UpdateFailure::failed())?;
    if response.status() == reqwest::StatusCode::FORBIDDEN {
        return Err(UpdateFailure::new("download_blocked"));
    }
    if !response.status().is_success() || response.url().scheme() != "https" {
        return Err(UpdateFailure::failed());
    }
    let final_url = response.url().to_string();
    if response
        .content_length()
        .is_some_and(|length| length > maximum)
    {
        return Err(UpdateFailure::failed());
    }
    let html = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|kind| kind.trim().eq_ignore_ascii_case("text/html"));
    if html {
        return Err(UpdateFailure::new("download_blocked"));
    }
    let mut response = response;
    let mut total: u64 = 0;
    let mut target: Option<tokio::fs::File> = None;
    let outcome = async {
        loop {
            let chunk = match tokio::time::timeout_at(deadline, response.chunk()).await {
                Err(_) => return Err(UpdateFailure::new("timeout")),
                Ok(Err(_)) => return Err(UpdateFailure::failed()),
                Ok(Ok(None)) => break,
                Ok(Ok(Some(chunk))) => chunk,
            };
            if target.is_none() {
                if challenge_page(&chunk) {
                    return Err(UpdateFailure::new("download_blocked"));
                }
                target = Some(
                    tokio::fs::File::create(destination)
                        .await
                        .map_err(|_| UpdateFailure::failed())?,
                );
            }
            total += chunk.len() as u64;
            if total > maximum {
                return Err(UpdateFailure::failed());
            }
            if let Some(file) = target.as_mut() {
                file.write_all(&chunk)
                    .await
                    .map_err(|_| UpdateFailure::failed())?;
            }
        }
        match target.as_mut() {
            Some(file) => file.flush().await.map_err(|_| UpdateFailure::failed())?,
            None => {
                tokio::fs::File::create(destination)
                    .await
                    .map_err(|_| UpdateFailure::failed())?;
            }
        }
        Ok(())
    }
    .await;
    drop(target);
    outcome?;
    Ok(Downloaded {
        bytes: total,
        url: final_url,
    })
}

/// A small capped GET from a fixed publisher (Python `read_bounded`).
pub(crate) async fn fetch(
    url: &str,
    prefix: &str,
    maximum: u64,
    timeout: Duration,
) -> Result<Vec<u8>, UpdateFailure> {
    let deadline = Instant::now() + timeout;
    let mut response = tokio::time::timeout_at(deadline, client(timeout)?.get(url).send())
        .await
        .map_err(|_| UpdateFailure::new("timeout"))?
        .map_err(|_| UpdateFailure::failed())?;
    if response.status() != reqwest::StatusCode::OK
        || !response.url().as_str().starts_with(prefix)
        || response
            .content_length()
            .is_some_and(|length| length > maximum)
    {
        return Err(UpdateFailure::failed());
    }
    let mut body = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline, response.chunk()).await {
            Err(_) => return Err(UpdateFailure::new("timeout")),
            Ok(Err(_)) => return Err(UpdateFailure::failed()),
            Ok(Ok(None)) => return Ok(body),
            Ok(Ok(Some(chunk))) => {
                body.extend_from_slice(&chunk);
                if body.len() as u64 > maximum {
                    return Err(UpdateFailure::failed());
                }
            }
        }
    }
}

/// Python `remote_fingerprint`.
pub(crate) async fn fingerprint(url: &str, timeout: Duration) -> Option<Fingerprint> {
    let response = tokio::time::timeout(timeout, client(timeout).ok()?.head(url).send())
        .await
        .ok()?
        .ok()?;
    if response.status() != reqwest::StatusCode::OK || response.url().scheme() != "https" {
        return None;
    }
    let headers = response.headers();
    let size = headers
        .get(reqwest::header::CONTENT_LENGTH)?
        .to_str()
        .ok()
        .filter(|text| !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit()))?
        .parse()
        .ok()?;
    let validator = headers
        .get(reqwest::header::ETAG)
        .or_else(|| headers.get(reqwest::header::LAST_MODIFIED))?
        .to_str()
        .ok()?;
    let mut end = validator.len().min(200);
    while !validator.is_char_boundary(end) {
        end -= 1;
    }
    Some(Fingerprint {
        url: response.url().to_string(),
        size,
        validator: validator[..end].to_owned(),
    })
}

/// Python `RangeReader._get`.
pub(crate) async fn range_get(
    url: &str,
    range: &str,
    timeout: Duration,
) -> Result<super::shared::RangeReply, UpdateFailure> {
    static CONTENT_RANGE: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"\Abytes ([0-9]+)-([0-9]+)/([0-9]+)\z").expect("content range")
    });
    let deadline = Instant::now() + timeout;
    let mut response = tokio::time::timeout_at(
        deadline,
        client(timeout)?
            .get(url)
            .header(reqwest::header::RANGE, range)
            .send(),
    )
    .await
    .map_err(|_| UpdateFailure::new("timeout"))?
    .map_err(|_| UpdateFailure::failed())?;
    if response.status() != reqwest::StatusCode::PARTIAL_CONTENT
        || response.url().scheme() != "https"
    {
        return Err(UpdateFailure::failed());
    }
    let header = response
        .headers()
        .get(reqwest::header::CONTENT_RANGE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_owned();
    let found = CONTENT_RANGE
        .captures(&header)
        .ok_or_else(UpdateFailure::failed)?;
    let number = |index: usize| {
        found[index]
            .parse::<u64>()
            .map_err(|_| UpdateFailure::failed())
    };
    let (first, last, total) = (number(1)?, number(2)?, number(3)?);
    let expected = last
        .checked_sub(first)
        .map(|span| span + 1)
        .ok_or_else(UpdateFailure::failed)?;
    let final_url = response.url().to_string();
    let mut data = Vec::new();
    loop {
        match tokio::time::timeout_at(deadline, response.chunk()).await {
            Err(_) => return Err(UpdateFailure::new("timeout")),
            Ok(Err(_)) => return Err(UpdateFailure::failed()),
            Ok(Ok(None)) => break,
            Ok(Ok(Some(chunk))) => {
                data.extend_from_slice(&chunk);
                if data.len() as u64 > expected {
                    return Err(UpdateFailure::failed());
                }
            }
        }
    }
    if data.len() as u64 != expected {
        return Err(UpdateFailure::failed());
    }
    Ok(super::shared::RangeReply {
        start: first,
        data,
        total,
        url: final_url,
    })
}
