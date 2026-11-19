use aac_config::{ComputerConfig, TransportConfig};
use aac_core::{Cause, Failure, Retry};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::net::Ipv6Addr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{
    Arc, Mutex, OnceLock,
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
    /// SCP executable, normally `scp` resolved on PATH. It always runs the probed SSH via `-S`.
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

    fn search_path(&self) -> OsString {
        self.path
            .clone()
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default()
    }

    /// Capabilities belong to one executable file, not its pathname: a same-path upgrade,
    /// PATH change or symlink retarget re-probes. Returns the probed file and its identity.
    async fn capabilities(&self) -> Result<(PathBuf, String, Vec<OsString>), Failure> {
        let path = self.search_path();
        // A file replaced while its probe runs is probed again; results match the file now there.
        for _ in 0..3 {
            let (file, identity) = executable_identity(&self.ssh, &path).await?;
            let args = self.probe(&file, &identity).await?;
            if executable_identity(&self.ssh, &path).await? == (file.clone(), identity.clone()) {
                return Ok((file, identity, args));
            }
        }
        Err(Failure::new(
            Cause::Refused,
            "SSH executable changed during capability probe",
        ))
    }

    async fn probe(&self, file: &Path, identity: &str) -> Result<Vec<OsString>, Failure> {
        /// A failed probe is retried after this, so a recovered client is not refused until restart.
        const FAILURE_TTL: Duration = Duration::from_secs(30);
        type Probe = Arc<tokio::sync::OnceCell<(Result<Vec<OsString>, Failure>, Instant)>>;
        static PROBES: OnceLock<Mutex<std::collections::HashMap<PathBuf, (String, Probe)>>> =
            OnceLock::new();
        let probe = {
            let mut probes = PROBES
                .get_or_init(Mutex::default)
                .lock()
                .map_err(|_| Failure::new(Cause::Bug, "SSH capability cache unavailable"))?;
            let entry = probes
                .entry(file.to_path_buf())
                .or_insert_with(|| (identity.to_owned(), Probe::default()));
            // Success is cached per identity; a failure only for FAILURE_TTL.
            let expired = entry
                .1
                .get()
                .is_some_and(|(result, at)| result.is_err() && at.elapsed() >= FAILURE_TTL);
            if entry.0 != identity || expired {
                *entry = (identity.to_owned(), Probe::default());
            }
            entry.1.clone()
        };
        probe
            .get_or_init(|| async {
                let result: Result<Vec<OsString>, Failure> = async {
                    let mut args = Vec::new();
                    for option in ["KnownHostsCommand=none", "SetEnv=AAC_SSH_POLICY=1"] {
                        let mut command = Command::new(file);
                        ssh_environment(&mut command);
                        if let Some(path) = &self.path {
                            command.env("PATH", path);
                        }
                        command
                            .args(["-G", "-F", "none", "-o", option, "--", "localhost"])
                            .stdin(Stdio::null())
                            .stdout(Stdio::null())
                            .stderr(Stdio::piped())
                            .kill_on_drop(true);
                        let mut child = command.spawn()?;
                        let stderr = child
                            .stderr
                            .take()
                            .ok_or_else(|| Failure::new(Cause::Bug, "SSH probe stderr absent"))?;
                        let result = tokio::time::timeout(Duration::from_secs(2), async {
                            let mut output = Vec::new();
                            stderr.take(8192).read_to_end(&mut output).await?;
                            let status = child.wait().await?;
                            Ok::<_, std::io::Error>((status, output))
                        })
                        .await;
                        match result {
                            Ok(Ok((status, _))) if status.success() => {
                                args.extend(["-o".into(), option.into()])
                            }
                            Ok(Ok((_, output)))
                                if String::from_utf8_lossy(&output)
                                    .to_ascii_lowercase()
                                    .contains("bad configuration option:") =>
                            {
                                eprintln!(
                                    "[AAC SSH] {} unsupported; compatibility downgrade",
                                    option.split('=').next().unwrap_or("option")
                                );
                            }
                            _ => {
                                let _ = child.start_kill();
                                let _ =
                                    tokio::time::timeout(Duration::from_millis(150), child.wait())
                                        .await;
                                return Err(Failure::new(
                                    Cause::Refused,
                                    "SSH capability probe failed",
                                ));
                            }
                        }
                    }
                    Ok(args)
                }
                .await;
                (result, Instant::now())
            })
            .await
            .0
            .clone()
    }

    fn control_directory(&self) -> PathBuf {
        // %C expands to 40 hex characters; OpenSSH also appends a 17-byte temporary suffix.
        if self.run_dir.as_os_str().len() <= 24 {
            return self.run_dir.clone();
        }
        let identity = aac_core::sha256_hex(self.run_dir.to_string_lossy().as_bytes());
        #[cfg(unix)]
        let root = PathBuf::from("/tmp");
        #[cfg(not(unix))]
        let root = std::env::temp_dir();
        #[cfg(unix)]
        let owner = aac_files::path::uid();
        #[cfg(not(unix))]
        let owner = std::process::id();
        root.join(format!("aac-{owner}-{}", &identity[..8]))
    }

    fn prepare_control_directory(&self) -> Result<(), Failure> {
        Self::prepare_directory(&self.run_dir)?;
        Self::prepare_directory(&self.control_directory())
    }

    fn prepare_directory(directory: &Path) -> Result<(), Failure> {
        if !directory.exists() {
            let builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            let builder = {
                use std::os::unix::fs::DirBuilderExt;
                let mut builder = builder;
                builder.mode(0o700);
                builder
            };
            match builder.create(directory) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
        }
        let metadata = std::fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Failure::new(
                Cause::Refused,
                "SSH control directory must be private and real",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::{MetadataExt, PermissionsExt};
            if metadata.permissions().mode() & 0o077 != 0
                || metadata.uid() != aac_files::path::uid()
            {
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
    pub async fn argv(&self, key: &Path, scan: bool) -> Result<Vec<OsString>, Failure> {
        Ok(self.invocation(key, scan).await?.argv)
    }

    /// The probed SSH file and the policy argv computed for exactly that file.
    async fn invocation(&self, key: &Path, scan: bool) -> Result<Invocation, Failure> {
        let mut argv = self.base_argv(key, scan)?;
        let (program, identity, capabilities) = self.capabilities().await?;
        argv.extend(capabilities);
        let policy_hash = self.policy_hash(&argv, &identity).await?;
        let control = format!(
            "ControlPath={}",
            quote_option_path(&format!(
                "{}/cm-{policy_hash}-%C",
                option_path(&self.control_directory())?.trim_end_matches('/')
            ))
        );
        argv.extend(["-o".into(), control.into()]);
        Ok(Invocation {
            program,
            identity,
            argv,
        })
    }

    /// Re-check, just before spawning, that the probed file is still the one SSH resolves to.
    async fn verify(&self, invocation: &Invocation) -> Result<(), Failure> {
        let current = executable_identity(&self.ssh, &self.search_path()).await?;
        if current.0 != invocation.program || current.1 != invocation.identity {
            return Err(Failure::new(
                Cause::Refused,
                "SSH executable changed before launch",
            ));
        }
        Ok(())
    }

    async fn policy_hash(&self, argv: &[OsString], executable: &str) -> Result<String, Failure> {
        let pin = self.known_hosts.clone();
        let metadata = tokio::task::spawn_blocking(move || match std::fs::metadata(&pin) {
            Ok(meta) => {
                // The Unix fields name a pin's inode and ctime; Windows has no such pair to add.
                let suffix = {
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::MetadataExt;
                        format!(
                            ":{}:{}:{}:{}",
                            meta.dev(),
                            meta.ino(),
                            meta.ctime(),
                            meta.ctime_nsec()
                        )
                    }
                    #[cfg(not(unix))]
                    {
                        String::new()
                    }
                };
                Ok(format!("{}:{:?}{suffix}", meta.len(), meta.modified()))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("absent".to_owned()),
            Err(error) => Err(Failure::from(error)),
        })
        .await
        .map_err(|_| Failure::new(Cause::Bug, "SSH pin identity unavailable"))??;
        let mut hash = Sha256::new();
        hash.update(b"aac-ssh-policy-v2");
        hash.update(self.known_hosts.to_string_lossy().as_bytes());
        hash.update(metadata.as_bytes());
        // A replaced client gets its own master, never one opened under another client's policy.
        hash.update([0]);
        hash.update(executable.as_bytes());
        for arg in argv {
            hash.update([0]);
            hash.update(arg.to_string_lossy().as_bytes());
        }
        Ok(hash.finalize()[..8]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect())
    }

    fn base_argv(&self, key: &Path, scan: bool) -> Result<Vec<OsString>, Failure> {
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
        option_path(&self.run_dir)?;
        let mut argv: Vec<OsString> = vec!["-T".into()];
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
            "VerifyHostKeyDNS=no",
            "UpdateHostKeys=no",
            "ForwardAgent=no",
            "ForwardX11=no",
            "GlobalKnownHostsFile=none",
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
        argv.extend(["-o".into(), "ControlPersist=120".into()]);
        Ok(argv)
    }
}

/// Resolve SSH as the child's PATH lookup would (that path is what the probe runs), then name
/// the file by its identity: canonical path, size and modification time, plus device, inode and
/// change time on Unix (creation time on Windows, whose stable std API has no file index).
async fn executable_identity(
    program: &Path,
    path: &std::ffi::OsStr,
) -> Result<(PathBuf, String), Failure> {
    let program = program.to_path_buf();
    let path = path.to_os_string();
    tokio::task::spawn_blocking(move || {
        let unavailable = || Failure::new(Cause::Refused, "SSH executable unavailable");
        let candidates: Vec<PathBuf> = if program.components().count() > 1 {
            vec![program]
        } else {
            std::env::split_paths(&path)
                .filter(|directory| directory.is_absolute())
                .flat_map(|directory| {
                    let plain = directory.join(&program);
                    let exe = plain.with_extension("exe");
                    if cfg!(windows) && program.extension().is_none() {
                        vec![exe, plain]
                    } else {
                        vec![plain]
                    }
                })
                .collect()
        };
        for candidate in candidates {
            let Ok(file) = std::fs::canonicalize(&candidate) else {
                continue;
            };
            let Ok(meta) = std::fs::metadata(&file) else {
                continue;
            };
            #[cfg(unix)]
            let runnable = {
                use std::os::unix::fs::PermissionsExt;
                meta.is_file() && meta.permissions().mode() & 0o111 != 0
            };
            #[cfg(not(unix))]
            let runnable = meta.is_file();
            if !runnable {
                continue;
            }
            let mut identity = format!(
                "{}\0{}\0{:?}",
                file.to_string_lossy(),
                meta.len(),
                meta.modified()
            );
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                identity.push_str(&format!(
                    "\0{}\0{}\0{}\0{}",
                    meta.dev(),
                    meta.ino(),
                    meta.ctime(),
                    meta.ctime_nsec()
                ));
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::MetadataExt;
                identity.push_str(&format!("\0{}", meta.creation_time()));
            }
            return Ok((candidate, identity));
        }
        Err(unavailable())
    })
    .await
    .map_err(|_| Failure::new(Cause::Bug, "SSH executable identity unavailable"))?
}

/// One SSH launch: the probed file, its identity and the policy argv bound to them.
struct Invocation {
    program: PathBuf,
    identity: String,
    argv: Vec<OsString>,
}

/// Only connection/authentication environment reaches SSH or SCP; Local verbs keep theirs.
fn ssh_environment(command: &mut Command) {
    command.env_clear();
    for (name, value) in std::env::vars_os() {
        let key = name.to_string_lossy();
        let key = if cfg!(windows) {
            key.to_ascii_uppercase()
        } else {
            key.into_owned()
        };
        if ["PATH", "HOME", "USER", "LOGNAME", "LANG", "SSH_AUTH_SOCK"].contains(&key.as_str())
            || (cfg!(windows) && ["SYSTEMROOT", "USERPROFILE", "WINDIR"].contains(&key.as_str()))
        {
            command.env(name, value);
        }
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
    /// Trusted dashboard scope for a local app-update agent. SSH never inherits it.
    pub local_app_update_state_dir: Option<PathBuf>,
}

impl CallOptions {
    /// Bound an operation to a duration and total captured output, with a fresh cancel token.
    pub fn new(budget: Duration, output_cap: usize) -> Self {
        Self {
            deadline: Instant::now() + budget,
            cancel: CancellationToken::new(),
            output_cap,
            scan: false,
            local_app_update_state_dir: None,
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
            policy.base_argv(key, false)?;
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
        let (command, launch) = self.command(argv, options).await?;
        self.run_command(command, launch, stdin, options, None)
            .await
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
        let (command, launch) = self.command(argv, options).await?;
        self.run_command(
            command,
            launch,
            stdin,
            options,
            Some(Pipes { controls, output }),
        )
        .await
    }

    async fn command(
        &self,
        argv: &[OsString],
        options: &CallOptions,
    ) -> Result<(Command, Option<Invocation>), Failure> {
        if argv.is_empty() {
            return Err(Failure::new(Cause::Refused, "empty command"));
        }
        let (mut command, launch) = match &self.config.transport {
            TransportConfig::Local => {
                let mut cmd = Command::new(&argv[0]);
                cmd.args(&argv[1..]);
                if let Some(directory) = &options.local_app_update_state_dir {
                    if !directory.is_absolute() {
                        return Err(Failure::new(
                            Cause::Refused,
                            "invalid local app update state directory",
                        ));
                    }
                    cmd.env("AAC_APP_UPDATE_STATE_DIR", directory);
                }
                (cmd, None)
            }
            TransportConfig::Ssh {
                host,
                user,
                port,
                key,
            } => {
                self.policy.prepare_control_directory()?;
                let invocation = self.policy.invocation(key, options.scan).await?;
                let mut cmd = Command::new(&invocation.program);
                ssh_environment(&mut cmd);
                cmd.args(&invocation.argv);
                if let Some(port) = port {
                    cmd.args(["-p", &port.to_string()]);
                }
                if let Some(user) = user {
                    cmd.args(["-l", user]);
                }
                let ssh_host = endpoint_host(host)?;
                set_literal_hostname(&mut cmd, &ssh_host);
                cmd.arg("--").arg(ssh_host).arg(self.remote_command(argv)?);
                (cmd, Some(invocation))
            }
        };
        self.set_path(&mut command);
        Ok((command, launch))
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
        let invocation = self.policy.invocation(key, options.scan).await?;
        let mut command = Command::new(&self.policy.scp);
        ssh_environment(&mut command);
        // SCP runs the probed SSH file (-S), never its built-in or PATH ssh, with the identical
        // SSH options except SSH's -T (which has another meaning in scp).
        command
            .arg("-q")
            .arg("-S")
            .arg(&invocation.program)
            .args(invocation.argv.iter().skip(1));
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
        self.run_command(command, Some(invocation), &[], options, None)
            .await
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
        command: Command,
        launch: Option<Invocation>,
        stdin: &[u8],
        options: &CallOptions,
        pipes: Option<Pipes>,
    ) -> Result<ProcessOutput, Failure> {
        // This task owns completion and breaker accounting even if the HTTP receiver goes away.
        let computer = self.clone();
        let input = stdin.to_vec();
        let options = options.clone();
        let mut completion = CancelOnDrop(Some(options.cancel.clone()));
        let result = tokio::spawn(async move {
            computer
                .run_command_owned(command, launch, &input, &options, pipes)
                .await
        })
        .await
        .map_err(|_| Failure::new(Cause::Bug, "transport completion task failed"))?;
        completion.0 = None;
        result
    }

    async fn run_command_owned(
        &self,
        mut command: Command,
        launch: Option<Invocation>,
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
        // Immediately before the spawn, after any wait for a channel: the probed ssh file is
        // still the one argv was built for. Residual: a swap between this check and exec, by
        // someone who can already replace the user's ssh, is not detected.
        if let Some(invocation) = &launch {
            self.policy.verify(invocation).await?;
        }
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
                _ = tokio::time::sleep_until(options.deadline) => Err(Failure::new(Cause::Timeout, "transport deadline exceeded")),
                _ = options.cancel.cancelled() => Err(Failure::new(Cause::Cancelled, "request cancelled")),
                output = work => output,
            }
        };
        if result.is_err() {
            kill_tree(pid);
            let _ = child.start_kill();
            let _ = tokio::time::timeout(Duration::from_millis(150), child.wait()).await;
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

// Dropping a caller requests cleanup; the owned task still reaps and accounts the outcome.
struct CancelOnDrop(Option<CancellationToken>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if let Some(cancel) = self.0.take() {
            cancel.cancel();
        }
    }
}

/// Production `Computer::scp` against an owned loopback sshd (tests/spec/agent/scp_loopback.py).
#[cfg(all(test, target_os = "linux"))]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod scp_loopback {
    use super::{CallOptions, Computer, SshPolicy};
    use aac_config::{ComputerConfig, OperatingSystem, TransportConfig};
    use aac_core::ComputerId;
    use std::path::PathBuf;
    use std::time::Duration;

    /// Runs only as the fixture script's child, inside its namespace; otherwise a no-op.
    #[tokio::test]
    async fn child() {
        let Some(spec) = std::env::var_os("AAC_SCP_LOOPBACK_CHILD") else {
            return;
        };
        let spec: serde_json::Value = serde_json::from_str(&spec.to_string_lossy()).unwrap();
        let text = |name: &str| PathBuf::from(spec[name].as_str().unwrap());
        let root = text("root");
        let mut policy = SshPolicy::new(text("pin"), text("run"));
        policy.path = Some(spec["path"].as_str().unwrap().into());
        let computer = Computer::new(
            ComputerConfig {
                id: ComputerId::new("fixture").unwrap(),
                label: "Fixture".into(),
                os: OperatingSystem::Linux,
                transport: TransportConfig::Ssh {
                    host: "127.0.0.1".into(),
                    user: Some("root".into()),
                    port: Some(u16::try_from(spec["port"].as_u64().unwrap()).unwrap()),
                    key: root.join("key"),
                },
                roles: Vec::new(),
                metadata: Default::default(),
            },
            policy,
        )
        .unwrap();
        let result = computer
            .scp(
                &text("payload"),
                spec["dest"].as_str().unwrap(),
                &CallOptions::new(Duration::from_secs(15), 64 * 1024),
            )
            .await;
        let report = match &result {
            Ok(output) => serde_json::json!({"ok": true, "code": output.code}),
            Err(failure) => {
                serde_json::json!({"ok": false, "cause": format!("{:?}", failure.cause)})
            }
        };
        println!("AAC_SCP_RESULT {report}");
    }

    #[test]
    #[ignore = "requires AAC_TEST_OPENSSH_81, sudo, mount namespaces and an owned loopback sshd"]
    fn scp_runs_the_probed_ssh_and_refuses_an_empty_pin() {
        let old = std::env::var_os("AAC_TEST_OPENSSH_81").expect("AAC_TEST_OPENSSH_81");
        // A short root keeps AAC's control directory inside the fixture.
        let root = tempfile::Builder::new()
            .prefix("s")
            .tempdir_in("/tmp")
            .unwrap();
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/spec/agent/scp_loopback.py");
        let output = std::process::Command::new("sudo")
            .args(["-n", "unshare", "--mount", "--fork", "python3"])
            .arg(script)
            .arg(root.path())
            .arg(std::env::current_exe().unwrap())
            .arg(old)
            .output()
            .unwrap();
        println!("{}", String::from_utf8_lossy(&output.stdout));
        assert!(
            output.status.success(),
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
