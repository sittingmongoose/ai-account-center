//! Fresh idle CLI relaunches in real terminal contexts; no prompt replay
//! (Python `app_update_terminal.py`, `app_update_terminal_child.py`, `app_update_pipe.py`).
use super::{
    CommandSpec, Context, Installed, Platform, Process, UpdateFailure,
    processes::{scan, user_family},
    shared::{self, Env},
};
use serde_json::{Value, json};
use std::{collections::HashSet, path::Path, sync::LazyLock, time::Duration};

static UUID: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}\z").expect("uuid")
});

/// Python `resume_arguments`: an explicit, already selected session UUID may be reopened, never a prompt.
pub(crate) fn resume_arguments(app_id: &str, old_args: &[String]) -> Vec<String> {
    let forms: &[&str] = match app_id {
        "antigravity-cli" => &["--conversation"],
        "muse-code" | "codex-cli" => &["resume"],
        "omp" | "claude-code" => &["--resume"],
        _ => &[],
    };
    for (index, value) in old_args.iter().enumerate() {
        if forms.contains(&value.as_str())
            && let Some(next) = old_args.get(index + 1)
            && UUID.is_match(next)
        {
            return vec![value.clone(), next.clone()];
        }
    }
    Vec::new()
}

/// Python `restart_environment`.
pub(crate) fn restart_environment(context: &Process) -> Env {
    let mut values: Env = if context.env.is_empty() {
        std::env::vars_os()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value.to_string_lossy().into_owned(),
                )
            })
            .collect()
    } else {
        context.env.clone()
    };
    for (key, value) in super::common::NO_AUTO_UPDATE {
        if key == "CODEX_NON_INTERACTIVE" {
            continue;
        }
        match values.iter_mut().find(|(name, _)| name == key) {
            Some(entry) => entry.1 = value.to_owned(),
            None => values.push((key.to_owned(), value.to_owned())),
        }
    }
    values.retain(|(key, _)| key != "MUSE_LAUNCHER_INSTALL" && key != "MUSE_UPGRADE_MODE");
    shared::strip_dashboard_secrets(values)
}

/// Python `mac_terminal_environment`: [`restart_environment`] without the SSH session.
pub(crate) fn mac_terminal_environment(context: &Process) -> Env {
    restart_environment(context)
        .into_iter()
        .filter(|(key, _)| {
            !shared::SSH_SESSION_ENV
                .iter()
                .any(|name| name.eq_ignore_ascii_case(key))
        })
        .collect()
}

/// Python `shlex.quote`.
pub(crate) fn shell_quote(word: &str) -> String {
    if word.is_empty() {
        return "''".into();
    }
    if word
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || "_@%+=:,./-".contains(ch))
    {
        return word.into();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

/// Python `shlex.join`.
pub(crate) fn shell_join(words: &[String]) -> String {
    words
        .iter()
        .map(|word| shell_quote(word))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Python `check_terminal(platform, contexts)`: refuse when a terminal instance
/// cannot be reopened on this host (`restart_context`).
pub(crate) async fn check_terminal(
    cx: &Context,
    contexts: &[Process],
) -> Result<(), UpdateFailure> {
    if contexts.is_empty() {
        return Ok(());
    }
    let refused = || Err(UpdateFailure::new("restart_context"));
    match cx.platform {
        Platform::Ubuntu => {
            if cx.host.which("tmux").is_none() || cx.host.which("systemd-run").is_none() {
                return refused();
            }
            Ok(())
        }
        Platform::Mac => {
            if !cx
                .host
                .system("/System/Applications/Utilities/Terminal.app")
                .is_dir()
            {
                return refused();
            }
            Ok(())
        }
        Platform::Windows => {
            let session = super::processes::own_session();
            if session.is_none() || contexts.iter().any(|item| item.session != session) {
                return refused();
            }
            if cx.host.which("wt").is_none() {
                return refused();
            }
            Ok(())
        }
    }
}

/// Python `restart_cli(install, contexts)`: reopen each stopped terminal instance
/// idle in a new terminal, then wait until each one runs. Returns `restartTargets`.
pub(crate) async fn restart_cli(
    cx: &Context,
    install: &Installed,
    contexts: &[Process],
) -> Result<Vec<Value>, UpdateFailure> {
    check_terminal(cx, contexts).await?;
    if contexts.is_empty() {
        return Ok(Vec::new());
    }
    let before: HashSet<(u32, String)> = user_family(&cx.host, install, &scan(cx).await?)
        .into_iter()
        .map(|item| (item.pid, item.identity))
        .collect();
    let mut sessions = Vec::new();
    for (index, context) in contexts.iter().enumerate() {
        let args = resume_arguments(install.app_id, &context.args);
        match cx.platform {
            Platform::Ubuntu => {
                let server = format!("ccs-updates-{}", super::processes::unique_suffix());
                let session = format!("ccs-updated-{}-{}", install.app_id, index + 1);
                let unit = format!(
                    "aac-launch-{}-{}",
                    shared::sanitize_unit_fragment(install.app_id),
                    super::processes::unique_suffix()
                );
                let tmux = cx
                    .host
                    .which("tmux")
                    .ok_or_else(|| UpdateFailure::new("restart_context"))?;
                let cwd = context.cwd.clone().unwrap_or_default();
                let mut command_words = vec![install.path.to_string_lossy().into_owned()];
                command_words.extend(args);
                let tmux_argv = vec![
                    tmux.to_string_lossy().into_owned(),
                    "-L".into(),
                    server.clone(),
                    "new-session".into(),
                    "-d".into(),
                    "-s".into(),
                    session.clone(),
                    "-c".into(),
                    cwd.clone(),
                    shell_join(&command_words),
                ];
                let environment = restart_environment(context);
                // forking: tmux daemonizes, so a simple unit would lose track of the server.
                let argv = shared::systemd_user_service_argv(
                    &cx.host,
                    &unit,
                    Some(&cwd),
                    &environment,
                    &tmux_argv,
                    "forking",
                    &[],
                )?;
                let mut spec = CommandSpec::new(argv).timeout(Duration::from_secs(15));
                spec.env_mode = super::common::EnvMode::Exact;
                spec.env = shared::systemd_run_environment(&environment)
                    .into_iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect();
                cx.run(&spec).await?;
                sessions.push(json!({"kind": "tmux", "server": server, "session": session}));
            }
            Platform::Mac => {
                #[cfg(unix)]
                {
                    broker_mac(cx, install, context, args).await?;
                    sessions.push(json!({"kind": "terminal"}));
                }
                #[cfg(not(unix))]
                return Err(UpdateFailure::new("restart_context"));
            }
            Platform::Windows => {
                #[cfg(windows)]
                {
                    broker_windows(cx, install, context, args).await?;
                    sessions.push(json!({"kind": "windows-terminal"}));
                }
                #[cfg(not(windows))]
                return Err(UpdateFailure::new("restart_context"));
            }
        }
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while tokio::time::Instant::now() < deadline {
        let live = user_family(&cx.host, install, &scan(cx).await?);
        let ids: HashSet<u32> = live.iter().map(|item| item.pid).collect();
        let fresh = live
            .iter()
            .filter(|item| {
                !ids.contains(&item.ppid) && !before.contains(&(item.pid, item.identity.clone()))
            })
            .count();
        if fresh >= contexts.len() {
            return Ok(sessions);
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(UpdateFailure::new("restart_failed"))
}

static ENDPOINT: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\Aterminal-[0-9a-f]{32}\.sock\z").expect("endpoint"));
#[cfg(windows)]
const PIPE_PREFIX: &str = r"\\.\pipe\ccs-update-terminal-";

fn nonce() -> String {
    format!(
        "{}{}{}",
        super::processes::unique_suffix(),
        super::processes::unique_suffix(),
        &super::processes::unique_suffix()[..8]
    )
}

/// The relaunch packet the terminal child receives (Python `_broker_terminal`'s packet).
fn packet(install: &Installed, context: &Process, args: Vec<String>, env: Env) -> Vec<u8> {
    let mut argv = vec![install.path.to_string_lossy().into_owned()];
    argv.extend(args);
    let env: serde_json::Map<String, Value> = env
        .into_iter()
        .map(|(key, value)| (key, Value::String(value)))
        .collect();
    let mut bytes = serde_json::to_vec(&json!({"argv": argv, "cwd": context.cwd, "env": env}))
        .unwrap_or_default();
    bytes.push(b'\n');
    bytes
}

/// Python `_broker_terminal` (Mac): a private socket, a `.command` script that runs
/// `aac-agent --terminal-child --socket <endpoint>` in Terminal, and the packet handed over.
#[cfg(unix)]
async fn broker_mac(
    cx: &Context,
    install: &Installed,
    context: &Process,
    args: Vec<String>,
) -> Result<(), UpdateFailure> {
    use std::os::unix::fs::PermissionsExt as _;
    let failed = || UpdateFailure::new("restart_failed");
    let root = cx.host.home.join(".ccs/app-updates");
    super::common::private_dir(&root).map_err(|_| failed())?;
    let nonce = nonce();
    let endpoint = root.join(format!("terminal-{nonce}.sock"));
    let script = root.join(format!("terminal-{nonce}.command"));
    let listener = std::os::unix::net::UnixListener::bind(&endpoint).map_err(|_| failed())?;
    let cleanup = scopeguard(endpoint.clone(), script.clone());
    std::fs::set_permissions(&endpoint, std::fs::Permissions::from_mode(0o600))
        .map_err(|_| failed())?;
    let data = packet(install, context, args, mac_terminal_environment(context));
    let server = tokio::task::spawn_blocking(move || -> bool {
        use std::io::Write as _;
        if listener.set_nonblocking(false).is_err() {
            return false;
        }
        // accept() has no timeout; the caller stops waiting after 25 s and the socket is removed.
        let Ok((mut connection, _)) = listener.accept() else {
            return false;
        };
        connection.write_all(&data).is_ok()
            && connection.shutdown(std::net::Shutdown::Write).is_ok()
    });
    let agent = std::env::current_exe().map_err(|_| failed())?;
    let line = shell_join(&[
        agent.to_string_lossy().into_owned(),
        "--terminal-child".into(),
        "--socket".into(),
        endpoint.to_string_lossy().into_owned(),
    ]);
    std::fs::write(&script, format!("#!/bin/sh\nexec {line}\n")).map_err(|_| failed())?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))
        .map_err(|_| failed())?;
    let mut spec = CommandSpec::new([
        "/usr/bin/open".into(),
        "-a".into(),
        "Terminal".into(),
        script.as_os_str().to_owned(),
    ])
    .timeout(Duration::from_secs(15))
    .no_stderr();
    spec.env_mode = super::common::EnvMode::Exact;
    spec.env = shared::gui_launch_environment(&[])
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect();
    cx.run(&spec).await?;
    let delivered = tokio::time::timeout(Duration::from_secs(25), server).await;
    drop(cleanup);
    match delivered {
        Ok(Ok(true)) => Ok(()),
        _ => Err(failed()),
    }
}

/// Removes the broker's socket and script however the broker ends.
#[cfg(unix)]
struct Cleanup(std::path::PathBuf, std::path::PathBuf);

#[cfg(unix)]
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        let _ = std::fs::remove_file(&self.1);
    }
}

#[cfg(unix)]
fn scopeguard(endpoint: std::path::PathBuf, script: std::path::PathBuf) -> Cleanup {
    Cleanup(endpoint, script)
}

/// Python `_broker_terminal` (Windows): a pipe only this user can open, and a new
/// Windows Terminal tab running `aac-agent --terminal-child --pipe <endpoint>`.
#[cfg(windows)]
async fn broker_windows(
    cx: &Context,
    install: &Installed,
    context: &Process,
    args: Vec<String>,
) -> Result<(), UpdateFailure> {
    let failed = || UpdateFailure::new("restart_failed");
    let endpoint = format!("{PIPE_PREFIX}{}", nonce());
    let pipe = super::os_windows::PrivatePipe::create(&endpoint).ok_or_else(failed)?;
    let data = packet(install, context, args, restart_environment(context));
    let server = tokio::task::spawn_blocking(move || pipe.send(&data));
    let wt = cx
        .host
        .which("wt")
        .ok_or_else(|| UpdateFailure::new("restart_context"))?;
    let agent = std::env::current_exe().map_err(|_| failed())?;
    let cwd = context.cwd.clone().unwrap_or_default();
    let spec = CommandSpec::new([
        wt.into_os_string(),
        "-w".into(),
        "0".into(),
        "new-tab".into(),
        "-d".into(),
        cwd.into(),
        "--".into(),
        agent.into_os_string(),
        "--terminal-child".into(),
        "--pipe".into(),
        endpoint.into(),
    ])
    .timeout(Duration::from_secs(15))
    .visible()
    .no_stderr();
    cx.run(&spec).await?;
    match tokio::time::timeout(Duration::from_secs(25), server).await {
        Ok(Ok(true)) => Ok(()),
        _ => Err(failed()),
    }
}

/// `aac-agent --terminal-child --socket <path>` / `--pipe <endpoint>` (Python
/// `app_update_terminal_child.py`): receive one relaunch packet and become the idle CLI.
/// Returns an exit code only when it could not start the CLI.
pub fn terminal_child(args: &[std::ffi::OsString]) -> i32 {
    let run = || -> Option<i32> {
        let [mode, value] = args else { return None };
        let value = value.to_str()?;
        let raw = match mode.to_str()? {
            "--socket" => receive_socket(value)?,
            #[cfg(windows)]
            "--pipe" => super::os_windows::receive_pipe(value, PIPE_PREFIX)?,
            _ => return None,
        };
        let packet: Value = serde_json::from_slice(&raw).ok()?;
        let argv: Vec<String> = packet["argv"]
            .as_array()?
            .iter()
            .map(|item| item.as_str().map(str::to_owned))
            .collect::<Option<_>>()?;
        let cwd = packet["cwd"].as_str()?;
        let env = packet["env"].as_object()?;
        if !(1..=3).contains(&argv.len()) {
            return None;
        }
        std::env::set_current_dir(cwd).ok()?;
        #[cfg_attr(not(windows), allow(unused_mut))]
        let mut argv = argv;
        #[cfg(windows)]
        if Path::new(&argv[0])
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd"))
        {
            let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
            let line = super::os_windows::list2cmdline(&argv);
            argv = vec![
                format!("{root}\\System32\\cmd.exe"),
                "/d".into(),
                "/s".into(),
                "/c".into(),
                line,
            ];
        }
        let mut command = std::process::Command::new(&argv[0]);
        command.args(&argv[1..]).env_clear().envs(
            env.iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key, value))),
        );
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            let _ = command.exec();
            None
        }
        #[cfg(not(unix))]
        {
            // Windows has no exec: run the CLI in this console and pass its exit code on.
            command.status().ok()?.code()
        }
    };
    run().unwrap_or(1)
}

fn receive_socket(path: &str) -> Option<Vec<u8>> {
    #[cfg(unix)]
    {
        use std::io::Read as _;
        let endpoint = Path::new(path);
        let root = std::env::home_dir()?.join(".ccs/app-updates");
        let name = endpoint.file_name()?.to_str()?;
        if crate::apps::real(endpoint.parent()?) != crate::apps::real(&root)
            || !ENDPOINT.is_match(name)
        {
            return None;
        }
        let connection = std::os::unix::net::UnixStream::connect(endpoint).ok()?;
        connection
            .set_read_timeout(Some(Duration::from_secs(10)))
            .ok()?;
        let mut raw = Vec::new();
        connection
            .take(2 * 1024 * 1024 + 1)
            .read_to_end(&mut raw)
            .ok()?;
        (raw.len() <= 2 * 1024 * 1024).then_some(raw)
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}
