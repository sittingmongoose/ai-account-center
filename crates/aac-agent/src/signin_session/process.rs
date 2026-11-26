//! The sign-in CLI process layer, ported from `signin-process.ts` (CONTRACT-registry-lifecycle
//! 6.6). Fixed argv, no shell, the complete environment and nothing else, stdio never inherited.
//!
//! With `pty: true` the CLI runs on the same fixed Python relay the TypeScript server uses:
//! it allocates the pseudo-terminal, turns echo off, relays bytes and forwards SIGTERM to the
//! CLI's own session, SIGKILL one second later. The child is the leader of a new process
//! group (`detached: true` / `process_group(0)`), and `kill` signals the whole group: SIGTERM,
//! then SIGKILL after the two-second grace.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

// The relay's own grace before it SIGKILLs the CLI is one second, inside the fixed
// program text below; it is shorter than [`KILL_GRACE_MS`].
/// SIGTERM to the process group, then SIGKILL after this grace (`KILL_GRACE_MS`).
const KILL_GRACE_MS: u64 = 2_000;
/// The relay interpreter (`PTY_PYTHON`).
pub const PTY_PYTHON: &str = "/usr/bin/python3";

/// Fixed relay program (`PTY_RELAY_PROGRAM`); its argv after `-c <program>` is the CLI argv.
/// Byte-for-byte the TypeScript server's relay, so signal and echo semantics are the proven ones.
pub const PTY_RELAY_PROGRAM: &str = concat!(
    "import os, pty, select, signal, sys, termios\n",
    "argv = sys.argv[1:]\n",
    "pid, fd = pty.fork()\n",
    "if pid == 0:\n",
    "    try:\n",
    "        attrs = termios.tcgetattr(0)\n",
    "        attrs[3] &= ~termios.ECHO\n",
    "        termios.tcsetattr(0, termios.TCSANOW, attrs)\n",
    "    except Exception:\n",
    "        pass\n",
    "    try:\n",
    "        os.execvp(argv[0], argv)\n",
    "    except Exception:\n",
    "        os._exit(127)\n",
    "try:\n",
    "    import fcntl, struct\n",
    "    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 160, 0, 0))\n",
    "except Exception:\n",
    "    pass\n",
    "def force(signum, frame):\n",
    "    try:\n",
    "        os.killpg(pid, signal.SIGKILL)\n",
    "    except Exception:\n",
    "        pass\n",
    "stopping = []\n",
    "def stop(signum, frame):\n",
    "    try:\n",
    "        os.killpg(pid, signal.SIGTERM)\n",
    "    except Exception:\n",
    "        pass\n",
    "    if not stopping:\n",
    "        stopping.append(True)\n",
    "        signal.signal(signal.SIGALRM, force)\n",
    "        signal.setitimer(signal.ITIMER_REAL, 1)\n",
    "for name in (signal.SIGTERM, signal.SIGINT, signal.SIGHUP):\n",
    "    signal.signal(name, stop)\n",
    "source = sys.stdin.fileno()\n",
    "watch = [fd, source]\n",
    "while True:\n",
    "    ready, _, _ = select.select(watch, [], [])\n",
    "    if fd in ready:\n",
    "        try:\n",
    "            data = os.read(fd, 4096)\n",
    "        except OSError:\n",
    "            data = b''\n",
    "        if not data:\n",
    "            break\n",
    "        os.write(1, data)\n",
    "    if source in ready:\n",
    "        data = os.read(source, 4096)\n",
    "        if data:\n",
    "            os.write(fd, data)\n",
    "        else:\n",
    "            watch.remove(source)\n",
    "_, status = os.waitpid(pid, 0)\n",
    "sys.exit(os.waitstatus_to_exitcode(status))\n",
);

/// One sign-in CLI launch (`SignInCommand`).
#[derive(Clone, Debug)]
pub struct Command {
    /// The executable, never a shell.
    pub file: String,
    /// The fixed argv after the executable.
    pub args: Vec<String>,
    /// The complete environment; nothing else is inherited.
    pub env: BTreeMap<String, String>,
    /// Run the CLI on a pseudo-terminal through the fixed relay.
    pub pty: bool,
    /// When set, drive the Antigravity sandboxed CLI on a native Rust PTY without Python.
    pub antigravity_driver: Option<crate::signin_session::antigravity_driver::DriverSpec>,
}

/// Why a spawn failed (`SignInSpawnFailure`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpawnFailure {
    /// The executable is absent (ENOENT).
    Missing,
    /// Any other spawn error.
    Failed,
}

/// One child event: decoded output or the exit (`onData` / `onExit`).
#[derive(Clone, Debug)]
pub enum ProcessEvent {
    /// A decoded output chunk; stdout and stderr share the listener, in arrival order.
    Data(String),
    /// The child ended: its exit code, or the spawn failure.
    Exit(Option<i32>, Option<SpawnFailure>),
}

/// The group-kill side of a handle (`kill`). Implementations must be idempotent.
pub trait Killer: Send + Sync {
    /// SIGTERM to the process group, then SIGKILL after the grace; no-op once exited.
    fn kill(&self);
}

/// A spawned sign-in process (`SignInProcessHandle`): output events, input writes and kill.
pub struct Handle {
    /// Output and exit events, in order; the exit event is last.
    pub events: mpsc::UnboundedReceiver<ProcessEvent>,
    /// Input writes; a send error means the input is no longer open (`write` returns false).
    pub input: mpsc::UnboundedSender<String>,
    killer: Arc<dyn Killer>,
    killed: AtomicBool,
    pid: Option<u32>,
}

impl Handle {
    /// Write to the CLI's input; false when it is no longer open.
    pub fn write(&self, text: &str) -> bool {
        self.input.send(text.to_string()).is_ok()
    }

    /// The child's pid (the process-group leader), when a child exists.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// SIGTERM to the process group, then SIGKILL after a short grace period.
    pub fn kill(&self) {
        if !self.killed.swap(true, Ordering::SeqCst) {
            self.killer.kill();
        }
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        // An abandoned session (cancel, deadline, front-door death) still stops the CLI.
        self.kill();
    }
}

/// A fake handle's kill counter (the unit-test seam, like the TS tests' FakeProcess).
#[cfg(test)]
pub struct CountingKiller {
    /// How many times kill was requested.
    pub kills: Arc<std::sync::atomic::AtomicUsize>,
}

#[cfg(test)]
impl Killer for CountingKiller {
    fn kill(&self) {
        self.kills.fetch_add(1, Ordering::SeqCst);
    }
}

/// A fake handle for the runner's unit tests (the TS tests' FakeProcess shape).
#[cfg(test)]
pub fn test_handle(
    events: mpsc::UnboundedReceiver<ProcessEvent>,
    input: mpsc::UnboundedSender<String>,
    killer: Arc<dyn Killer>,
) -> Handle {
    Handle {
        events,
        input,
        killer,
        killed: AtomicBool::new(false),
        pid: None,
    }
}

/// Incremental UTF-8 decoder with Node `StringDecoder` semantics: an incomplete trailing
/// sequence waits for the next chunk; invalid bytes become U+FFFD (WHATWG replacement).
#[derive(Default)]
pub struct Utf8Decoder {
    pending: Vec<u8>,
}

impl Utf8Decoder {
    /// Decode one chunk, buffering only a valid incomplete trailing sequence.
    pub fn push(&mut self, bytes: &[u8]) -> String {
        self.pending.extend_from_slice(bytes);
        let buf = std::mem::take(&mut self.pending);
        match std::str::from_utf8(&buf) {
            Ok(text) => text.to_string(),
            Err(error) => {
                let valid = error.valid_up_to();
                if error.error_len().is_none() {
                    // Incomplete tail: keep it for the next chunk.
                    self.pending = buf[valid..].to_vec();
                    String::from_utf8_lossy(&buf[..valid]).into_owned()
                } else {
                    String::from_utf8_lossy(&buf).into_owned()
                }
            }
        }
    }

    /// The stream ended: flush any buffered bytes with replacement.
    pub fn flush(&mut self) -> String {
        let buf = std::mem::take(&mut self.pending);
        String::from_utf8_lossy(&buf).into_owned()
    }
}

/// Spawn one sign-in CLI. Unix only: the relay and the group signals are the proven Unix
/// path; the TypeScript runner never starts sign-in jobs on Windows either.
#[cfg(unix)]
pub fn spawn(command: &Command) -> Handle {
    if let Some(driver) = &command.antigravity_driver {
        return crate::signin_session::antigravity_driver::spawn(command, driver);
    }
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (event_tx, events) = mpsc::unbounded_channel();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();
    let exited = Arc::new(AtomicBool::new(false));

    let mut build = tokio::process::Command::new(if command.pty {
        PTY_PYTHON
    } else {
        &command.file
    });
    if command.pty {
        build.arg("-c").arg(PTY_RELAY_PROGRAM).arg(&command.file);
    }
    build.args(&command.args);
    build.env_clear();
    build.envs(&command.env);
    build
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .process_group(0)
        .kill_on_drop(true);

    let child = match build.spawn() {
        Ok(child) => child,
        Err(error) => {
            let failure = if error.kind() == std::io::ErrorKind::NotFound {
                SpawnFailure::Missing
            } else {
                SpawnFailure::Failed
            };
            let _ = event_tx.send(ProcessEvent::Exit(None, Some(failure)));
            return Handle {
                events,
                input: input_tx,
                killer: Arc::new(NoChild),
                killed: AtomicBool::new(false),
                pid: None,
            };
        }
    };
    let pid = child.id();
    let killer = Arc::new(GroupKiller {
        pid,
        exited: exited.clone(),
    });
    let mut child = child;
    let stdin = child.stdin.take();
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    // Input relay: writes go to the child's stdin; a closed input drops the sender.
    if let Some(mut stdin) = stdin {
        tokio::spawn(async move {
            while let Some(text) = input_rx.recv().await {
                if stdin.write_all(text.as_bytes()).await.is_err() {
                    break;
                }
                let _ = stdin.flush().await;
            }
        });
    } else {
        drop(input_rx);
    }

    // Output readers: one decoder per stream, both forwarding to the same listener.
    // Their joins gate the exit event, the way Node's `close` follows the last `data`.
    let out_tx = event_tx.clone();
    let out_reader = stdout.map(|mut stdout| {
        tokio::spawn(async move {
            let mut decoder = Utf8Decoder::default();
            let mut buf = [0u8; 4096];
            loop {
                match stdout.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = decoder.push(&buf[..n]);
                        if !text.is_empty() && out_tx.send(ProcessEvent::Data(text)).is_err() {
                            break;
                        }
                    }
                }
            }
            let text = decoder.flush();
            if !text.is_empty() {
                let _ = out_tx.send(ProcessEvent::Data(text));
            }
        })
    });
    let err_tx = event_tx.clone();
    let err_reader = stderr.map(|mut stderr| {
        tokio::spawn(async move {
            let mut decoder = Utf8Decoder::default();
            let mut buf = [0u8; 4096];
            loop {
                match stderr.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        let text = decoder.push(&buf[..n]);
                        if !text.is_empty() && err_tx.send(ProcessEvent::Data(text)).is_err() {
                            break;
                        }
                    }
                }
            }
            let text = decoder.flush();
            if !text.is_empty() {
                let _ = err_tx.send(ProcessEvent::Data(text));
            }
        })
    });

    // Exit: wait for the child, then report once both readers have drained.
    tokio::spawn(async move {
        let status = child.wait().await;
        if let Some(reader) = out_reader {
            let _ = reader.await;
        }
        if let Some(reader) = err_reader {
            let _ = reader.await;
        }
        exited.store(true, Ordering::SeqCst);
        let code = status.ok().and_then(|s| s.code());
        let _ = event_tx.send(ProcessEvent::Exit(code, None));
    });

    Handle {
        events,
        input: input_tx,
        killer,
        killed: AtomicBool::new(false),
        pid,
    }
}

/// The spawn refusal on platforms without the relay (the TS runner never runs there).
#[cfg(not(unix))]
pub fn spawn(_command: &Command) -> Handle {
    let (event_tx, events) = mpsc::unbounded_channel();
    let (input_tx, input_rx) = mpsc::unbounded_channel::<String>();
    drop(input_rx);
    let _ = event_tx.send(ProcessEvent::Exit(None, Some(SpawnFailure::Missing)));
    Handle {
        events,
        input: input_tx,
        killer: Arc::new(NoChild),
        killed: AtomicBool::new(false),
        pid: None,
    }
}

/// A handle for a spawn that failed before creating a child (used by the Antigravity driver).
pub(crate) fn unspawned_handle(
    events: mpsc::UnboundedReceiver<ProcessEvent>,
    input: mpsc::UnboundedSender<String>,
) -> Handle {
    Handle {
        events,
        input,
        killer: Arc::new(NoChild),
        killed: AtomicBool::new(false),
        pid: None,
    }
}

/// Construct a spawned handle from a driver-managed child process.
#[cfg(unix)]
pub(crate) fn spawned_handle(
    events: mpsc::UnboundedReceiver<ProcessEvent>,
    input: mpsc::UnboundedSender<String>,
    killer: Arc<dyn Killer>,
    pid: Option<u32>,
) -> Handle {
    Handle {
        events,
        input,
        killer,
        killed: AtomicBool::new(false),
        pid,
    }
}

/// A handle with no child (spawn already failed).
struct NoChild;
impl Killer for NoChild {
    fn kill(&self) {}
}

/// SIGTERM to the child's process group, then SIGKILL after [`KILL_GRACE_MS`].
#[cfg(unix)]
struct GroupKiller {
    pid: Option<u32>,
    exited: Arc<AtomicBool>,
}

#[cfg(unix)]
impl Killer for GroupKiller {
    fn kill(&self) {
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        let Some(pid) = self.pid.and_then(|p| i32::try_from(p).ok()) else {
            return;
        };
        let group = nix::unistd::Pid::from_raw(pid);
        let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGTERM);
        let exited = self.exited.clone();
        // A plain thread: the grace must fire even when the runtime is shutting down.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(KILL_GRACE_MS));
            if !exited.load(Ordering::SeqCst) {
                let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
            }
        });
    }
}

/// Signal a process group by id (the journal's cancel and stray-cleanup path).
#[cfg(unix)]
pub fn signal_group(pgid: u32, signal: nix::sys::signal::Signal) -> bool {
    match i32::try_from(pgid) {
        Ok(pid) => nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), signal).is_ok(),
        Err(_) => false,
    }
}

/// Whether a process group still exists (`killpg` with no signal).
#[cfg(unix)]
pub fn group_alive(pgid: u32) -> bool {
    match i32::try_from(pgid) {
        Ok(pid) => nix::sys::signal::killpg(nix::unistd::Pid::from_raw(pid), None).is_ok(),
        Err(_) => false,
    }
}

/// Whether one process still exists (`kill` with no signal).
#[cfg(unix)]
pub fn process_alive(pid: u32) -> bool {
    match i32::try_from(pid) {
        Ok(pid) => nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid), None).is_ok(),
        Err(_) => false,
    }
}

/// Sessions never start on this platform (the TS runner's platforms are ubuntu and mac);
/// a journaled entry is left alone rather than recovered by a guess.
#[cfg(not(unix))]
pub fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn decoder_buffers_split_code_points() {
        let mut decoder = Utf8Decoder::default();
        let euro = "€".as_bytes();
        assert_eq!(decoder.push(&euro[..1]), "");
        assert_eq!(decoder.push(&euro[1..2]), "");
        assert_eq!(decoder.push(&euro[2..]), "€");
        assert_eq!(decoder.flush(), "");
    }

    #[test]
    fn decoder_replaces_invalid_bytes() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.push(b"a\xffb"), "a\u{fffd}b");
    }

    #[test]
    fn decoder_flushes_incomplete_tail() {
        let mut decoder = Utf8Decoder::default();
        assert_eq!(decoder.push(&"€".as_bytes()[..1]), "");
        assert_eq!(decoder.flush(), "\u{fffd}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn relay_runs_cli_and_reports_exit() {
        let mut env = BTreeMap::new();
        env.insert("PATH".into(), std::env::var("PATH").unwrap_or_default());
        let mut handle = spawn(&Command {
            file: "/bin/sh".into(),
            args: vec!["-c".into(), "printf 'hello\\n'; exit 3".into()],
            env,
            pty: true,
            antigravity_driver: None,
        });
        let mut data = String::new();
        let mut exit = None;
        while let Some(event) = handle.events.recv().await {
            match event {
                ProcessEvent::Data(text) => data.push_str(&text),
                ProcessEvent::Exit(code, failure) => {
                    exit = Some((code, failure));
                    break;
                }
            }
        }
        assert!(data.contains("hello"), "{data:?}");
        assert_eq!(exit, Some((Some(3), None)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_executable_reports_missing() {
        let mut handle = spawn(&Command {
            file: "/nonexistent/aac-signin-fixture-cli".into(),
            args: vec![],
            env: BTreeMap::new(),
            pty: false,
            antigravity_driver: None,
        });
        let event = handle.events.recv().await;
        assert!(matches!(
            event,
            Some(ProcessEvent::Exit(None, Some(SpawnFailure::Missing)))
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn kill_stops_the_group() {
        let mut env = BTreeMap::new();
        env.insert("PATH".into(), std::env::var("PATH").unwrap_or_default());
        let mut handle = spawn(&Command {
            file: "/bin/sh".into(),
            args: vec!["-c".into(), "sleep 30".into()],
            env,
            pty: true,
            antigravity_driver: None,
        });
        // Give the relay a moment to fork the CLI.
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        handle.kill();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut exit = None;
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_secs(1), handle.events.recv())
                .await
            {
                Ok(Some(ProcessEvent::Exit(code, failure))) => {
                    exit = Some((code, failure));
                    break;
                }
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => break,
            }
        }
        assert!(exit.is_some(), "relay did not exit after kill");
    }
}
