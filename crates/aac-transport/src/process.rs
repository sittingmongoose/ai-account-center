use aac_config::{ComputerConfig, TransportConfig};
use aac_core::{Cause, Failure, Retry};
use std::ffi::OsString;
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::{Semaphore, mpsc};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Pinned SSH policy and executable locations, injectable for offline fake-SSH tests.
#[derive(Clone, Debug)]
pub struct SshPolicy {
    /// Existing private known_hosts file; no automatic trust is performed.
    pub known_hosts: PathBuf,
    /// Owner-controlled 0700 directory for OpenSSH control sockets.
    pub run_dir: PathBuf,
    /// SSH executable, normally `ssh` resolved on PATH.
    pub ssh: PathBuf,
    /// SCP executable, normally `scp` resolved on PATH.
    pub scp: PathBuf,
    /// Explicit child PATH for test fakes; process-global environment is never changed.
    pub path: Option<OsString>,
}

impl SshPolicy {
    /// Use system OpenSSH and caller-supplied data/run paths.
    pub fn new(known_hosts: PathBuf, run_dir: PathBuf) -> Self {
        Self {
            known_hosts,
            run_dir,
            ssh: "ssh".into(),
            scp: "scp".into(),
            path: None,
        }
    }

    fn prepare_control_directory(&self) -> Result<(), Failure> {
        if !self.run_dir.exists() {
            let builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(&self.run_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let metadata = std::fs::symlink_metadata(&self.run_dir)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Failure::new(
                Cause::Refused,
                "SSH control directory must be private and real",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Failure::new(
                    Cause::Refused,
                    "SSH control directory permissions are too broad",
                ));
            }
        }
        Ok(())
    }

    /// Exact target-design 5.1 options, in stable order. Scan keepalive is six, short is two.
    /// The separate identity argv accepts spaces. OpenSSH -o paths accept only literal ASCII
    /// letters/digits and `/._-:`; unsupported quoting, whitespace and expansion are refused.
    pub fn argv(&self, key: &Path, scan: bool) -> Result<Vec<OsString>, Failure> {
        if !key.is_absolute()
            || !self.known_hosts.is_absolute()
            || !self.run_dir.is_absolute()
            || [&key, &self.known_hosts.as_path(), &self.run_dir.as_path()]
                .iter()
                .any(|p| p.to_string_lossy().chars().any(char::is_control))
        {
            return Err(Failure::new(
                Cause::Refused,
                "SSH paths must be explicit absolute paths",
            ));
        }
        let hosts_path = option_path(&self.known_hosts)?;
        let control_path = option_path(&self.run_dir)?;
        let mut argv: Vec<OsString> = vec!["-T".into(), "-F".into(), "none".into()];
        for value in [
            "BatchMode=yes",
            "ConnectTimeout=5",
            "ConnectionAttempts=1",
            "ServerAliveInterval=5",
            if scan {
                "ServerAliveCountMax=6"
            } else {
                "ServerAliveCountMax=2"
            },
            "StrictHostKeyChecking=yes",
            "UpdateHostKeys=no",
            "ForwardAgent=no",
            "GlobalKnownHostsFile=none",
            "KnownHostsCommand=none",
            "PermitLocalCommand=no",
            "ClearAllForwardings=yes",
        ] {
            argv.extend(["-o".into(), value.into()]);
        }
        let mut hosts = OsString::from("UserKnownHostsFile=");
        hosts.push(quote_option_path(&hosts_path));
        argv.extend([
            "-o".into(),
            hosts,
            "-o".into(),
            "IdentitiesOnly=yes".into(),
            "-i".into(),
            key.as_os_str().into(),
            "-o".into(),
            "ControlMaster=auto".into(),
        ]);
        let mut control = OsString::from("ControlPath=");
        control.push(quote_option_path(&format!(
            "{}/cm-%C",
            control_path.trim_end_matches('/')
        )));
        argv.extend([
            "-o".into(),
            control,
            "-o".into(),
            "ControlPersist=120".into(),
        ]);
        Ok(argv)
    }
}

fn option_path(path: &Path) -> Result<String, Failure> {
    let text = path
        .to_str()
        .ok_or_else(|| Failure::new(Cause::Refused, "SSH option path is not UTF-8"))?;
    #[cfg(windows)]
    let text = text.replace('\\', "/");
    if !text
        .bytes()
        .all(|c| c.is_ascii_alphanumeric() || b"/._-: ".contains(&c))
    {
        return Err(Failure::new(
            Cause::Refused,
            "unsupported OpenSSH option path",
        ));
    }
    Ok(text.to_owned())
}

fn quote_option_path(text: &str) -> String {
    if text.contains(' ') {
        format!("\"{text}\"")
    } else {
        text.to_owned()
    }
}

/// Per-call controls; the absolute deadline also covers waiting for a channel slot.
#[derive(Clone, Debug)]
pub struct CallOptions {
    /// Absolute monotonic deadline shared by every stage of an operation.
    pub deadline: Instant,
    /// Cancellation token shared with the caller.
    pub cancel: CancellationToken,
    /// Combined stdout and stderr byte cap.
    pub output_cap: usize,
    /// Whether the longer scan keepalive applies.
    pub scan: bool,
}

impl CallOptions {
    /// Bound an operation to a duration and total captured output, with a fresh cancel token.
    pub fn new(budget: Duration, output_cap: usize) -> Self {
        Self {
            deadline: Instant::now() + budget,
            cancel: CancellationToken::new(),
            output_cap,
            scan: false,
        }
    }
}

/// Captured bounded child output. Raw bytes may be private; they are never copied into Failure
/// diagnostics or Debug output. A nonzero application status is distinct from transport failure.
pub struct ProcessOutput {
    /// Child exit code, absent for termination by signal.
    pub code: Option<i32>,
    /// Bounded stdout bytes.
    pub stdout: Vec<u8>,
    /// Bounded stderr bytes, for in-memory classification only.
    pub stderr: Vec<u8>,
}

#[derive(Default)]
struct Breaker {
    failures: u8,
    until: Option<Instant>,
    probing: bool,
}
struct Shared {
    slots: Semaphore,
    breaker: Mutex<Breaker>,
    shipping: tokio::sync::Mutex<()>,
}

/// A computer handle whose clones share four channels, a circuit breaker and an SSH master.
/// Construct one handle per configured computer and clone it; independently constructed handles
/// deliberately do not share process-global state.
#[derive(Clone)]
pub struct Computer {
    pub(crate) config: ComputerConfig,
    policy: SshPolicy,
    shared: Arc<Shared>,
}

impl Computer {
    /// Validate configuration without contacting a host or trusting a key.
    pub fn new(config: ComputerConfig, policy: SshPolicy) -> Result<Self, Failure> {
        if let TransportConfig::Ssh {
            host,
            user,
            port,
            key,
        } = &config.transport
        {
            endpoint_host(host)?;
            if user.as_ref().is_some_and(|u| {
                u.is_empty()
                    || u.starts_with('-')
                    || !u
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || b"_.-".contains(&c))
            }) || *port == Some(0)
            {
                return Err(Failure::new(Cause::Refused, "invalid SSH endpoint"));
            }
            policy.argv(key, false)?;
        }
        Ok(Self {
            config,
            policy,
            shared: Arc::new(Shared {
                slots: Semaphore::new(4),
                breaker: Mutex::new(Breaker::default()),
                shipping: tokio::sync::Mutex::new(()),
            }),
        })
    }

    /// Borrow the immutable computer definition.
    pub fn config(&self) -> &ComputerConfig {
        &self.config
    }

    pub(crate) async fn shipping_lock(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.shared.shipping.lock().await
    }

    /// Run argv directly for Local or a fixed remote command through SSH. Remote argv is
    /// quoted as data for the peer's shell; no caller-controlled shell source is accepted.
    pub async fn execute(
        &self,
        argv: &[OsString],
        stdin: &[u8],
        options: &CallOptions,
    ) -> Result<ProcessOutput, Failure> {
        let command = self.command(argv, options)?;
        self.run_command(command, stdin, options, None).await
    }

    /// Execute with bounded later stdin frames and a bounded stdout chunk stream. The caller
    /// drains stdout concurrently. Completion includes the child's exit, never just a frame.
    pub async fn execute_controlled(
        &self,
        argv: &[OsString],
        stdin: &[u8],
        options: &CallOptions,
        controls: mpsc::Receiver<Vec<u8>>,
        output: mpsc::Sender<Vec<u8>>,
    ) -> Result<ProcessOutput, Failure> {
        let command = self.command(argv, options)?;
        self.run_command(command, stdin, options, Some(Pipes { controls, output }))
            .await
    }

    fn command(&self, argv: &[OsString], options: &CallOptions) -> Result<Command, Failure> {
        if argv.is_empty() {
            return Err(Failure::new(Cause::Refused, "empty command"));
        }
        let mut command = match &self.config.transport {
            TransportConfig::Local => {
                let mut cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                cmd
            }
            TransportConfig::Ssh {
                host,
                user,
                port,
                key,
            } => {
                self.policy.prepare_control_directory()?;
                let mut cmd = Command::new(&self.policy.ssh);
                cmd.args(self.policy.argv(key, options.scan)?);
                if let Some(port) = port {
                    cmd.args(["-p", &port.to_string()]);
                }
                if let Some(user) = user {
                    cmd.args(["-l", user]);
                }
                let ssh_host = endpoint_host(host)?;
                set_literal_hostname(&mut cmd, &ssh_host);
                cmd.arg("--").arg(ssh_host).arg(self.remote_command(argv)?);
                cmd
            }
        };
        self.set_path(&mut command);
        Ok(command)
    }

    pub(crate) async fn scp(
        &self,
        local: &Path,
        destination: &str,
        options: &CallOptions,
    ) -> Result<ProcessOutput, Failure> {
        let TransportConfig::Ssh {
            host,
            user,
            port,
            key,
        } = &self.config.transport
        else {
            return Err(Failure::new(Cause::Refused, "SCP needs SSH transport"));
        };
        let mut command = Command::new(&self.policy.scp);
        // SCP gets the identical SSH options, except SSH's -T (which has another meaning in scp).
        command
            .arg("-q")
            .args(self.policy.argv(key, options.scan)?.into_iter().skip(1));
        if let Some(port) = port {
            command.args(["-P", &port.to_string()]);
        }
        let scp_host = endpoint_host(host)?;
        set_literal_hostname(&mut command, &scp_host);
        let destination = format!(
            "{}{}:{destination}",
            user.as_ref().map(|u| format!("{u}@")).unwrap_or_default(),
            scp_host
        );
        command.arg("--").arg(local).arg(destination);
        self.set_path(&mut command);
        self.run_command(command, &[], options, None).await
    }

    fn set_path(&self, command: &mut Command) {
        if let Some(path) = &self.policy.path {
            command.env("PATH", path);
        }
    }

    fn remote_command(&self, argv: &[OsString]) -> Result<String, Failure> {
        if self.config.os == aac_config::OperatingSystem::Windows {
            // Windows OpenSSH hands the command to cmd.exe by default. Fixed PowerShell
            // source is UTF-16LE/Base64 encoded, so cmd never interprets argument metacharacters.
            let args = argv
                .iter()
                .map(|s| {
                    s.to_str()
                        .ok_or_else(|| Failure::new(Cause::Refused, "non-UTF8 remote argv"))
                        .and_then(|s| {
                            if s.chars().any(|c| ('\u{2018}'..='\u{201b}').contains(&c)) {
                                return Err(Failure::new(
                                    Cause::Refused,
                                    "unsupported PowerShell quote",
                                ));
                            }
                            Ok(format!("'{}'", s.replace('\'', "''")))
                        })
                })
                .collect::<Result<Vec<_>, _>>()?;
            let source = format!("& {}; exit $LASTEXITCODE", args.join(" "));
            Ok(format!(
                "powershell.exe -NoProfile -NonInteractive -EncodedCommand {}",
                encode_powershell(&source)
            ))
        } else {
            argv.iter()
                .map(|s| {
                    s.to_str()
                        .ok_or_else(|| Failure::new(Cause::Refused, "non-UTF8 remote argv"))
                        .map(|s| format!("'{}'", s.replace('\'', "'\\''")))
                })
                .collect::<Result<Vec<_>, _>>()
                .map(|v| v.join(" "))
        }
    }

    async fn run_command(
        &self,
        mut command: Command,
        stdin: &[u8],
        options: &CallOptions,
        pipes: Option<Pipes>,
    ) -> Result<ProcessOutput, Failure> {
        let _permit = tokio::select! {
            biased;
            _ = options.cancel.cancelled() => return Err(Failure::new(Cause::Cancelled, "request cancelled")),
            _ = tokio::time::sleep_until(options.deadline) => return Err(Failure::new(Cause::Timeout, "deadline expired waiting for channel")),
            slot = self.shared.slots.acquire() => slot.map_err(|_| Failure::new(Cause::Bug, "channel pool closed"))?,
        };
        let probing = self.before_call()?;
        let _probe = ProbeLease {
            shared: self.shared.clone(),
            probing,
        };
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        command.process_group(0);
        let result = self.capture(&mut command, stdin, options, pipes).await;
        self.after_call(&result, probing)?;
        result
    }

    fn before_call(&self) -> Result<bool, Failure> {
        let mut state = self
            .shared
            .breaker
            .lock()
            .map_err(|_| Failure::new(Cause::Bug, "breaker lock poisoned"))?;
        if let Some(until) = state.until {
            if until > Instant::now() || state.probing {
                let mut failure = Failure::new(Cause::SshUnreachable, "transport circuit open");
                failure.retry = Retry::After(
                    until.saturating_duration_since(Instant::now()).as_millis() as u64,
                );
                return Err(failure);
            }
            state.probing = true;
            return Ok(true);
        }
        Ok(false)
    }

    fn after_call(
        &self,
        result: &Result<ProcessOutput, Failure>,
        probing: bool,
    ) -> Result<(), Failure> {
        let mut state = self
            .shared
            .breaker
            .lock()
            .map_err(|_| Failure::new(Cause::Bug, "breaker lock poisoned"))?;
        if probing {
            state.probing = false;
        }
        match result {
            Err(failure)
                if matches!(
                    failure.cause,
                    Cause::SshUnreachable | Cause::Timeout | Cause::HostKeyMismatch
                ) =>
            {
                state.failures = state.failures.saturating_add(1);
                if state.failures >= 3 {
                    state.until = Some(Instant::now() + Duration::from_secs(30));
                }
            }
            Ok(_) => {
                state.failures = 0;
                state.until = None;
            }
            _ => {}
        }
        Ok(())
    }

    async fn capture(
        &self,
        command: &mut Command,
        stdin: &[u8],
        options: &CallOptions,
        pipes: Option<Pipes>,
    ) -> Result<ProcessOutput, Failure> {
        let mut child = command.spawn().map_err(|_| {
            Failure::new(Cause::SshUnreachable, "transport process could not start")
        })?;
        let pid = child.id();
        let mut guard = KillOnDrop(pid);
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| Failure::new(Cause::Bug, "missing child stdin"))?;
        let output = child
            .stdout
            .take()
            .ok_or_else(|| Failure::new(Cause::Bug, "missing child stdout"))?;
        let error = child
            .stderr
            .take()
            .ok_or_else(|| Failure::new(Cause::Bug, "missing child stderr"))?;
        let used = Arc::new(AtomicUsize::new(0));
        let result = {
            let work = async {
                let (mut controls, output_stream) = match pipes {
                    Some(pipes) => (Some(pipes.controls), Some(pipes.output)),
                    None => (None, None),
                };
                let send = async {
                    input.write_all(stdin).await?;
                    if let Some(controls) = controls.as_mut() {
                        let mut total = 0usize;
                        while let Some(frame) = controls.recv().await {
                            total = total.saturating_add(frame.len());
                            if frame.len() > 65536 || total > 1024 * 1024 {
                                return Err(std::io::Error::other("control cap exceeded"));
                            }
                            input.write_all(&frame).await?;
                        }
                    }
                    drop(input);
                    Ok::<_, std::io::Error>(())
                };
                tokio::pin!(send);
                // Exit takes priority over EPIPE. A controlled writer lives until exit and must
                // not keep a completed child alive waiting for another control.
                let wait = async {
                    tokio::select! {
                        status = child.wait() => status.map_err(Failure::from),
                        sent = &mut send => {
                            let status = child.wait().await?;
                            if status.success() && sent.is_err() {
                                return Err(Failure::new(Cause::SshUnreachable, "transport stdin closed"));
                            }
                            Ok(status)
                        }
                    }
                };
                let (stdout, stderr, status) = tokio::try_join!(
                    capture_bytes(output, options.output_cap, used.clone(), output_stream),
                    capture_bytes(error, options.output_cap, used, None),
                    wait
                )?;
                if status.code() == Some(255)
                    && matches!(self.config.transport, TransportConfig::Ssh { .. })
                {
                    let cause = if stderr
                        .windows(b"Host key verification failed".len())
                        .any(|s| s == b"Host key verification failed")
                        || stderr
                            .windows(b"REMOTE HOST IDENTIFICATION HAS CHANGED".len())
                            .any(|s| s == b"REMOTE HOST IDENTIFICATION HAS CHANGED")
                    {
                        Cause::HostKeyMismatch
                    } else {
                        Cause::SshUnreachable
                    };
                    return Err(Failure::new(cause, "SSH transport failed"));
                }
                Ok(ProcessOutput {
                    code: status.code(),
                    stdout,
                    stderr,
                })
            };
            tokio::select! {
                biased;
                _ = options.cancel.cancelled() => Err(Failure::new(Cause::Cancelled, "request cancelled")),
                _ = tokio::time::sleep_until(options.deadline) => Err(Failure::new(Cause::Timeout, "transport deadline exceeded")),
                output = work => output,
            }
        };
        if result.is_err() {
            kill_tree(pid);
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
        guard.0 = None;
        result
    }
}

// A single validator produces a bracketed literal for both tools. Hostname explicitly binds
// both invocations to the unbracketed address consumed by OpenSSH's resolver.
// Scoped literals, host:port strings, malformed brackets and other unsupported forms fail closed.
fn endpoint_host(host: &str) -> Result<String, Failure> {
    let refused = || Failure::new(Cause::Refused, "invalid or unsupported SSH host");
    if host.is_empty() || host.len() > 253 || host.starts_with('-') {
        return Err(refused());
    }
    let bracketed = host.starts_with('[');
    let literal = if bracketed {
        host.strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .ok_or_else(refused)?
    } else {
        host
    };
    if bracketed || literal.contains(':') {
        let ip: Ipv6Addr = literal.parse().map_err(|_| refused())?;
        return Ok(format!("[{ip}]"));
    }
    if !literal
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || b".-".contains(&byte))
    {
        return Err(refused());
    }
    Ok(literal.to_owned())
}

fn set_literal_hostname(command: &mut Command, host: &str) {
    if let Some(ip) = host
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        command.args(["-o", &format!("Hostname={ip}")]);
    }
}

async fn capture_bytes(
    mut input: impl AsyncRead + Unpin,
    max: usize,
    used: Arc<AtomicUsize>,
    stream: Option<mpsc::Sender<Vec<u8>>>,
) -> Result<Vec<u8>, Failure> {
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 8192];
    loop {
        let n = input.read(&mut buffer).await?;
        if n == 0 {
            return Ok(bytes);
        }
        let previous = used.fetch_add(n, Ordering::Relaxed);
        if previous > max || n > max.saturating_sub(previous) {
            return Err(Failure::new(
                Cause::OutputTooLarge,
                "transport output cap exceeded",
            ));
        }
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(stream) = &stream {
            stream
                .send(buffer[..n].to_vec())
                .await
                .map_err(|_| Failure::new(Cause::Cancelled, "agent receiver closed"))?;
        }
    }
}

struct Pipes {
    controls: mpsc::Receiver<Vec<u8>>,
    output: mpsc::Sender<Vec<u8>>,
}

fn kill_tree(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
    #[cfg(windows)]
    if let Some(pid) = pid {
        let _ = std::process::Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .output();
    }
}

struct KillOnDrop(Option<u32>);
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        kill_tree(self.0);
    }
}

struct ProbeLease {
    shared: Arc<Shared>,
    probing: bool,
}
impl Drop for ProbeLease {
    fn drop(&mut self) {
        if self.probing
            && let Ok(mut state) = self.shared.breaker.lock()
        {
            state.probing = false;
        }
    }
}

pub(crate) fn encode_powershell(source: &str) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes: Vec<u8> = source.encode_utf16().flat_map(u16::to_le_bytes).collect();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}
