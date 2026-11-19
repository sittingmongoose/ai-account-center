//! G16c: the read-only command runner the Status collectors use.
//!
//! One spawn without a shell, never rejects, always settles: a child that ignores the
//! deadline's kill is waited for; output past the cap is a failure, never a truncated reading.
//! The environment is the dashboard's allowlist (no session or T3 thread variables leak into
//! a probe) with this process's own PATH plus `~/.local/bin`; ssh gets the isolation options
//! the front door's policy sets (BatchMode, host-key checking, no forwarding) and the fixed
//! keepalives the collectors ask for.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

/// One command's outcome, as the collectors read it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Run {
    /// The exit code, or null when the child did not finish (missing, killed, overflow).
    pub code: Option<i32>,
    pub stdout: String,
    pub timed_out: bool,
    /// The program was not found.
    pub missing: bool,
}

/// The allowlist of the environment a child sees; `CLAUDE_*`/`T3*` session variables never
/// reach it.
fn child_env() -> Vec<(String, String)> {
    let allowed = ["PATH", "HOME", "USER", "LOGNAME", "LANG", "SSH_AUTH_SOCK"];
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| allowed.contains(&name.as_str()))
        .collect();
    let mut dirs: Vec<String> = std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .filter(|dir| !dir.is_empty())
        .map(str::to_owned)
        .collect();
    if let Some(home) = std::env::var_os("HOME") {
        let local = Path::new(&home).join(".local/bin");
        let text = local.to_string_lossy().into_owned();
        if !dirs.contains(&text) {
            dirs.push(text);
        }
    }
    env.retain(|(name, _)| name != "PATH");
    env.push(("PATH".to_owned(), dirs.join(":")));
    env
}

/// `findExecutable`: the first PATH folder (then `~/.local/bin`, already on the PATH) that
/// holds an executable `name`; the bare name when none does.
#[must_use]
pub(crate) fn find_executable(name: &str) -> PathBuf {
    if name.contains('/') {
        return PathBuf::from(name);
    }
    let path = child_env()
        .into_iter()
        .find(|(key, _)| key == "PATH")
        .map(|(_, value)| value)
        .unwrap_or_default();
    let separator = if cfg!(windows) { ';' } else { ':' };
    for dir in path.split(separator).filter(|dir| !dir.is_empty()) {
        let candidate = Path::new(dir).join(name);
        if is_executable(&candidate) {
            return candidate;
        }
    }
    PathBuf::from(name)
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path)
            .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        // Windows resolves an executable by its extension (PATHEXT), not by a mode bit.
        let name = path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let extensions = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into());
        std::fs::metadata(path).is_ok_and(|meta| meta.is_file())
            && extensions
                .split(';')
                .any(|extension| !extension.is_empty() && name.ends_with(&extension.to_ascii_lowercase()))
    }
}

/// One read-only command with a deadline and an output cap.
pub(crate) async fn run(file: &Path, args: &[String], timeout_ms: u64, max_bytes: usize) -> Run {
    let mut command = tokio::process::Command::new(file);
    command
        .args(args)
        .env_clear()
        .envs(child_env())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    let child = match command.spawn() {
        Ok(child) => child,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Run {
                missing: true,
                ..Run::default()
            };
        }
        Err(_) => {
            return Run {
                code: None,
                ..Run::default()
            };
        }
    };
    match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait_with_output()).await {
        Ok(Ok(output)) => {
            if output.stdout.len() > max_bytes {
                return Run::default();
            }
            Run {
                code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
                timed_out: false,
                missing: false,
            }
        }
        Ok(Err(_)) => Run::default(),
        Err(_) => Run {
            timed_out: true,
            ..Run::default()
        },
    }
}

/// The single-value SSH policy the front door applies to every read-only probe.
const SSH_ISOLATION: [&str; 11] = [
    "BatchMode=yes",
    "ConnectTimeout=5",
    "ConnectionAttempts=1",
    "StrictHostKeyChecking=yes",
    "VerifyHostKeyDNS=no",
    "UpdateHostKeys=no",
    "ForwardAgent=no",
    "ForwardX11=no",
    "GlobalKnownHostsFile=none",
    "PermitLocalCommand=no",
    "ClearAllForwardings=yes",
];

/// `sshArgv(host, command, {interval, countMax})`: the isolation options, the keepalives, the
/// fixed alias and the one remote command.
#[must_use]
pub(crate) fn ssh_argv(host: &str, command: &str, keepalive: Option<(u64, u64)>) -> Vec<String> {
    let mut args = vec!["-T".to_owned()];
    for option in SSH_ISOLATION {
        args.push("-o".to_owned());
        args.push(option.to_owned());
    }
    if let Some((interval, count_max)) = keepalive {
        args.push("-o".to_owned());
        args.push(format!("ServerAliveInterval={interval}"));
        args.push("-o".to_owned());
        args.push(format!("ServerAliveCountMax={count_max}"));
    }
    args.push("--".to_owned());
    args.push(host.to_owned());
    args.push(command.to_owned());
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ssh_isolates_the_connection_and_ends_with_the_alias_and_command() {
        let args = ssh_argv("nas1-agent", "true", Some((5, 2)));
        assert!(args.contains(&"BatchMode=yes".to_owned()));
        assert!(args.contains(&"ServerAliveInterval=5".to_owned()));
        assert_eq!(&args[args.len() - 3..], ["--", "nas1-agent", "true"]);
    }

    #[tokio::test]
    async fn a_missing_program_is_named_and_a_slow_one_times_out() {
        let missing = run(Path::new("/definitely/not/a/program"), &[], 1_000, 1024).await;
        assert!(missing.missing);
        let slow = run(
            Path::new("/bin/sh"),
            &["-c".to_owned(), "sleep 5".to_owned()],
            100,
            1024,
        )
        .await;
        assert!(slow.timed_out);
    }

    #[tokio::test]
    async fn output_past_the_cap_is_a_failure() {
        let over = run(
            Path::new("/bin/sh"),
            &["-c".to_owned(), "printf 'abcdefghij'".to_owned()],
            1_000,
            4,
        )
        .await;
        assert_eq!(over.code, None);
        let ok = run(
            Path::new("/bin/sh"),
            &["-c".to_owned(), "printf 'ok'".to_owned()],
            1_000,
            4,
        )
        .await;
        assert_eq!(ok.code, Some(0));
        assert_eq!(ok.stdout, "ok");
    }
}
