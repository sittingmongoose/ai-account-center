//! Running fixed scripts on the covered computers: this one directly, the others over plain
//! OpenSSH (`BatchMode`, a shared control connection). Output stays in memory; it may carry a
//! link or a code and is never logged.

use crate::config::Host;
use base64::Engine;
use std::io::{Read, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// A fixed script for one shell.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Script {
    /// A POSIX `sh` script (Linux and the Mac).
    Sh(String),
    /// One `cmd` line (Windows), the way Windows OpenSSH runs a command.
    Cmd(String),
    /// A PowerShell script (Windows), sent encoded so no quoting reaches `cmd`.
    Pwsh(String),
}

/// A finished command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Output {
    /// Its exit code (`None` when it was stopped).
    pub code: Option<i32>,
    /// What it printed (stdout, capped).
    pub stdout: String,
}

/// Why a command could not run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RemoteError {
    /// It could not be started.
    Spawn,
    /// It ran past its time.
    Timeout,
}

impl RemoteError {
    /// The fixed word.
    pub fn word(&self) -> &'static str {
        match self {
            RemoteError::Spawn => "spawn_failed",
            RemoteError::Timeout => "timeout",
        }
    }
}

/// A running interactive command: the login prompt.
pub trait Session: Send {
    /// Sends one line (the authorization code) followed by Enter.
    fn write_line(&mut self, line: &str) -> std::io::Result<()>;
    /// Closes its input.
    fn close_input(&mut self);
    /// Waits up to `wait` for more output; false once the command has ended and all is read.
    fn read(&mut self, wait: Duration) -> bool;
    /// Everything printed so far (terminal codes removed), kept in memory only.
    fn text(&self) -> String;
    /// Waits up to `wait` for the end; its exit code, or `None` after stopping it.
    fn wait(&mut self, wait: Duration) -> Option<i32>;
}

/// Runs scripts on computers.
pub trait Remote: Send + Sync {
    /// Runs a script to its end with this input.
    fn run(
        &self,
        host: &Host,
        script: &Script,
        stdin: &[u8],
        timeout: Duration,
    ) -> Result<Output, RemoteError>;
    /// Starts an interactive script; `tty` asks ssh for a terminal on the far side.
    fn start(
        &self,
        host: &Host,
        script: &Script,
        tty: bool,
    ) -> Result<Box<dyn Session>, RemoteError>;
}

/// Quotes a word for a POSIX shell.
pub fn sh_quote(word: &str) -> String {
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

fn encoded(script: &str) -> String {
    let utf16: Vec<u8> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
    base64::engine::general_purpose::STANDARD.encode(utf16)
}

#[cfg(unix)]
fn private_control_dir() -> Option<std::path::PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    let uid = std::fs::metadata("/proc/self")
        .map(|m| m.uid())
        .ok()
        .or_else(|| {
            std::env::var_os("HOME")
                .and_then(|h| std::fs::metadata(h).ok())
                .map(|m| m.uid())
        })?;
    let base = std::env::var_os("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .map(|p| p.join("aac-claude-renew"))
        .unwrap_or_else(|| std::path::PathBuf::from(format!("/tmp/aac-claude-renew-{uid}")));
    let _ = std::fs::DirBuilder::new().mode(0o700).create(&base);
    let meta = std::fs::symlink_metadata(&base).ok()?;
    if !meta.is_dir() || meta.uid() != uid || meta.permissions().mode() & 0o077 != 0 {
        return None;
    }
    (base.as_os_str().len() <= 48).then_some(base)
}

#[cfg(not(unix))]
fn private_control_dir() -> Option<std::path::PathBuf> {
    None
}

/// Plain OpenSSH, as the operator's own ssh config resolves the aliases.
#[derive(Clone, Debug)]
pub struct SshRemote {
    /// A private folder for the shared control sockets, if any.
    pub control_dir: Option<std::path::PathBuf>,
}

impl SshRemote {
    /// A remote with shared control connections in a short private folder (`$XDG_RUNTIME_DIR`
    /// or `/tmp/aac-claude-renew-<uid>`): a socket path must stay under about 100 bytes. Without
    /// a folder that is provably this user's own and private, connections are not shared.
    pub fn with_private_control() -> Self {
        SshRemote {
            control_dir: private_control_dir(),
        }
    }

    /// The argv that runs this script on this host.
    pub fn argv(&self, host: &Host, script: &Script, tty: bool) -> Vec<String> {
        let Some(alias) = &host.ssh else {
            return match script {
                Script::Sh(s) => vec!["sh".into(), "-c".into(), s.clone()],
                Script::Cmd(s) => vec!["cmd".into(), "/c".into(), s.clone()],
                Script::Pwsh(s) => vec![
                    "powershell".into(),
                    "-NoProfile".into(),
                    "-NonInteractive".into(),
                    "-EncodedCommand".into(),
                    encoded(s),
                ],
            };
        };
        let mut argv: Vec<String> = vec!["ssh".into(), if tty { "-tt" } else { "-T" }.into()];
        for option in [
            "BatchMode=yes",
            "ConnectTimeout=8",
            "ServerAliveInterval=15",
            "UpdateHostKeys=no",
        ] {
            argv.push("-o".into());
            argv.push(option.into());
        }
        if let Some(dir) = &self.control_dir {
            argv.push("-o".into());
            argv.push("ControlMaster=auto".into());
            argv.push("-o".into());
            argv.push(format!("ControlPath={}/%C", dir.display()));
            argv.push("-o".into());
            argv.push("ControlPersist=120".into());
        }
        argv.push("--".into());
        argv.push(alias.clone());
        argv.push(match script {
            Script::Sh(s) => format!("sh -c {}", sh_quote(s)),
            Script::Cmd(s) => s.clone(),
            Script::Pwsh(s) => format!(
                "powershell -NoProfile -NonInteractive -EncodedCommand {}",
                encoded(s)
            ),
        });
        argv
    }

    fn spawn(&self, host: &Host, script: &Script, tty: bool) -> Result<Child, RemoteError> {
        let argv = self.argv(host, script, tty);
        Command::new(&argv[0])
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| RemoteError::Spawn)
    }
}

const OUTPUT_CAP: usize = 8 * 1024 * 1024;

fn reader(mut out: impl Read + Send + 'static) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match out.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    rx
}

impl Remote for SshRemote {
    fn run(
        &self,
        host: &Host,
        script: &Script,
        stdin: &[u8],
        timeout: Duration,
    ) -> Result<Output, RemoteError> {
        let mut child = self.spawn(host, script, false)?;
        if let Some(mut input) = child.stdin.take() {
            let data = stdin.to_vec();
            std::thread::spawn(move || {
                let _ = input.write_all(&data);
            });
        }
        let rx = child.stdout.take().map(reader).ok_or(RemoteError::Spawn)?;
        let deadline = Instant::now() + timeout;
        let mut out = Vec::new();
        loop {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok(chunk) => {
                    if out.len() < OUTPUT_CAP {
                        out.extend_from_slice(&chunk);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if Instant::now() > deadline {
                let _ = child.kill();
                let _ = child.wait();
                return Err(RemoteError::Timeout);
            }
        }
        let code = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status.code(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        Ok(Output {
            code,
            stdout: String::from_utf8_lossy(&out).into_owned(),
        })
    }

    fn start(
        &self,
        host: &Host,
        script: &Script,
        tty: bool,
    ) -> Result<Box<dyn Session>, RemoteError> {
        let mut child = self.spawn(host, script, tty)?;
        let rx = child.stdout.take().map(reader).ok_or(RemoteError::Spawn)?;
        let input = child.stdin.take();
        Ok(Box::new(ChildSession {
            child,
            input,
            rx,
            buf: Vec::new(),
            ended: false,
            newline: if tty { "\r" } else { "\n" },
        }))
    }
}

struct ChildSession {
    child: Child,
    input: Option<std::process::ChildStdin>,
    rx: mpsc::Receiver<Vec<u8>>,
    buf: Vec<u8>,
    ended: bool,
    newline: &'static str,
}

const SESSION_CAP: usize = 64 * 1024;

impl Session for ChildSession {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        let input = self
            .input
            .as_mut()
            .ok_or_else(|| std::io::Error::other("input closed"))?;
        input.write_all(line.as_bytes())?;
        input.write_all(self.newline.as_bytes())?;
        input.flush()
    }

    fn close_input(&mut self) {
        self.input = None;
    }

    fn read(&mut self, wait: Duration) -> bool {
        if self.ended {
            return false;
        }
        match self.rx.recv_timeout(wait) {
            Ok(chunk) => {
                self.buf.extend_from_slice(&chunk);
                while let Ok(more) = self.rx.try_recv() {
                    self.buf.extend_from_slice(&more);
                }
                if self.buf.len() > SESSION_CAP {
                    let cut = self.buf.len() - SESSION_CAP;
                    self.buf.drain(..cut);
                }
                true
            }
            Err(mpsc::RecvTimeoutError::Timeout) => true,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                self.ended = true;
                false
            }
        }
    }

    fn text(&self) -> String {
        clean_terminal(&String::from_utf8_lossy(&self.buf))
    }

    fn wait(&mut self, wait: Duration) -> Option<i32> {
        let deadline = Instant::now() + wait;
        while Instant::now() < deadline {
            if !self.read(Duration::from_millis(200)) {
                break;
            }
        }
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return status.code(),
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    return None;
                }
            }
        }
    }
}

impl Drop for ChildSession {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// Removes terminal control sequences and turns carriage returns into line ends.
pub fn clean_terminal(text: &str) -> String {
    static ANSI: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)|\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[()][A-Za-z0-9]|\x1b[=>]")
            .expect("static regex")
    });
    ANSI.replace_all(text, "").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Os;

    #[cfg_attr(windows, allow(dead_code))]
    fn local() -> Host {
        Host {
            name: "here".into(),
            os: Os::Linux,
            ssh: None,
            claude: None,
        }
    }

    #[test]
    fn ssh_argv_quotes_the_script_and_never_takes_options_from_the_alias() {
        let remote = SshRemote { control_dir: None };
        let host = Host {
            name: "nas1".into(),
            os: Os::Linux,
            ssh: Some("nas1-agent".into()),
            claude: None,
        };
        let argv = remote.argv(&host, &Script::Sh("echo 'hi'; ls".into()), false);
        assert_eq!(argv[0], "ssh");
        assert_eq!(argv[1], "-T");
        let dashdash = argv.iter().position(|a| a == "--").expect("--");
        assert_eq!(argv[dashdash + 1], "nas1-agent");
        assert_eq!(argv[dashdash + 2], "sh -c 'echo '\"'\"'hi'\"'\"'; ls'");
        let tty = remote.argv(&host, &Script::Cmd("dir".into()), true);
        assert_eq!(tty[1], "-tt");
        assert_eq!(tty.last().map(String::as_str), Some("dir"));
    }

    #[cfg(unix)]
    #[test]
    fn runs_with_input_and_times_out() {
        let remote = SshRemote { control_dir: None };
        let out = remote
            .run(
                &local(),
                &Script::Sh("cat; echo done".into()),
                b"in\n",
                Duration::from_secs(10),
            )
            .expect("run");
        assert_eq!(out.code, Some(0));
        assert_eq!(out.stdout, "in\ndone\n");
        let slow = remote.run(
            &local(),
            &Script::Sh("sleep 5".into()),
            b"",
            Duration::from_millis(300),
        );
        assert_eq!(slow, Err(RemoteError::Timeout));
    }

    #[cfg(unix)]
    #[test]
    fn session_reads_and_takes_a_line() {
        let remote = SshRemote { control_dir: None };
        let mut s = remote
            .start(
                &local(),
                &Script::Sh("printf 'prompt > '; read x; echo \"got:$x\"".into()),
                false,
            )
            .expect("start");
        let mut seen = false;
        for _ in 0..50 {
            s.read(Duration::from_millis(100));
            if s.text().contains("prompt >") {
                seen = true;
                break;
            }
        }
        assert!(seen);
        s.write_line("abc").expect("write");
        assert_eq!(s.wait(Duration::from_secs(5)), Some(0));
        assert!(s.text().contains("got:abc"));
    }

    #[test]
    fn terminal_codes_are_removed() {
        assert_eq!(clean_terminal("\x1b[2Ka\x1b]0;t\x07b\r\nc"), "ab\n\nc");
    }
}
