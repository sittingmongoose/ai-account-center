//! Native `ai-account-center antigravity ...`: terminal sign-in on Ubuntu and retired-verb guard.
//!
//! Ported from `src/antigravity/terminal-signin.ts`, `src/antigravity/signin-sandbox.ts`,
//! and `src/antigravity/account-lifecycle.ts` (`importSignIn`). Runs zero Node or Python
//! processes: the official Antigravity CLI executes inside bubblewrap on the user's own
//! terminal, credential completion is polled via `aac_files::antigravity_store`, and the
//! verified identity + optional live-login renewal commits directly under `.transaction-lock`.

use crate::antigravity_host::Host;
use crate::antigravity_probes::{bounded_run, current_uid};
use crate::antigravity_renewal::{self, Effect, Outcome};
use crate::antigravity_runtime::{
    self as runtime, LiveIdentity, ProfileRecord, RegistryRead, RegistryState, sha256_hex,
};
use crate::signin_callbacks;
use aac_files::antigravity_store::{
    CREDENTIAL_FORMAT, MaskedCredentialState, masked_credential_state, read_masked_credential,
};
use aac_files::profile_writes::AntigravityMaintenanceLease;
use aac_files::{process, profile_staging, read, read_bounded};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const BWRAP: &str = "/usr/bin/bwrap";
const DBUS_RUN_SESSION: &str = "/usr/bin/dbus-run-session";
const APPARMOR_QUERY_LEAF: &str = "/sys/kernel/security/apparmor/.access";
const MAX_PROFILES: usize = 16;
const POLL_MS: u64 = 250;
const TIMEOUT_MS: u64 = 15 * 60_000;
const STOP_GRACE_MS: u64 = 2_000;
const STALE_LOCK_TIMEOUT_MS: u64 = 10 * 60_000;
const STALE_MARKER_TIMEOUT_MS: u64 = 30 * 60_000;

/// Leave the alternate screen, show the cursor, reset colours, mouse, paste and keyboard modes.
const TERMINAL_RESET: &str =
    "\x1b[?1049l\x1b[?25h\x1b[0m\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?2004l\x1b[<u\r\n";

const PRIVATE_SESSION_BUS_CONFIG: &str = r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig>
  <type>session</type>
  <listen>unix:tmpdir=/tmp</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
"#;

/// Usage text for `ai-account-center antigravity`.
pub fn help() -> &'static str {
    "AI Account Center Antigravity profiles

Usage
  ai-account-center antigravity signin <profile>

Commands
  signin <profile>   Add a new saved profile, or sign in again to a saved one

Notes
  Runs on Ubuntu in an interactive terminal (over SSH is fine). The official
  Antigravity CLI starts in a private sign-in home: choose Google OAuth, open the
  link in a browser, sign in to the Google account for this profile and paste
  the code back. History and settings are not touched. Sign in again keeps the
  same Google account; when the profile is the live Antigravity login, the live
  login is renewed too. Remove profiles in the dashboard. AI Account Center no
  longer switches the live login (activate is retired); it reads each profile's
  usage.
"
}

/// Execute `ai-account-center antigravity <args...>`.
pub fn run(args: &[String], ccs: &Path) -> i32 {
    let Some((first, rest)) = args.split_first() else {
        print!("{}", help());
        return 0;
    };
    match first.as_str() {
        "help" | "--help" | "-h" => {
            print!("{}", help());
            0
        }
        "activate" => {
            println!("[X] antigravity activate is retired in AI Account Center.");
            println!(
                "    Account switching was removed; AI Account Center only reads usage and limits."
            );
            1
        }
        "signin" => {
            if rest.len() != 1 || rest[0].starts_with('-') {
                println!("[X] Usage: ai-account-center antigravity signin <profile>");
                return 1;
            }
            let Some(home) = std::env::home_dir() else {
                println!(
                    "[X] The Antigravity CLI is not installed at ~/.local/bin/agy. Nothing was started."
                );
                return 1;
            };
            let env: BTreeMap<String, String> = std::env::vars().collect();
            let mut io = DefaultIo;
            let host = Host::from_root(ccs);
            let mut backend = HostBackend {
                host: host.as_ref(),
            };
            let mut runner = RealRunner;
            let mut deps = TerminalSignInDeps {
                ccs_dir: ccs,
                real_home: &home,
                env: &env,
                io: &mut io,
                backend: &mut backend,
                runner: &mut runner,
                timeout_ms: TIMEOUT_MS,
                poll_ms: POLL_MS,
                stop_grace_ms: STOP_GRACE_MS,
                guard_signals: true,
                abort_signal: None,
            };
            run_terminal_signin(&rest[0], &mut deps)
        }
        other => {
            println!(
                "[X] Unknown antigravity command: {other}. Run: ai-account-center antigravity help"
            );
            1
        }
    }
}

/// Terminal sign-in mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignInMode {
    /// Adding a brand-new profile.
    Add,
    /// Renewing an existing saved profile.
    SignInAgain,
}

/// Preflight check result for isolated Antigravity sign-in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PreflightResult {
    /// Host is ready for isolated bubblewrap sign-in.
    Ok {
        /// Resolved path to `~/.local/bin/agy`.
        native_binary: PathBuf,
        /// Optional AppArmor policy query leaf path.
        apparmor_leaf: Option<String>,
        /// Optional `/run/user/<uid>` path to mask with tmpfs.
        runtime_dir: Option<String>,
    },
    /// Host failed preflight with a fixed user-facing reason sentence.
    Failed {
        /// Machine reason (`"tool_missing"` or `"preflight_failed"`).
        reason: &'static str,
        /// User-facing detail sentence (without trailing `" Nothing was started."`).
        detail: String,
    },
}

/// Terminal I/O abstraction (injectable for deterministic tests).
pub trait TerminalIo {
    /// Whether stdin and stdout are both interactive TTYs.
    fn is_interactive(&self) -> bool;
    /// Write text to stdout.
    fn write(&mut self, text: &str);
    /// Save terminal mode string (`stty -g`), if available.
    fn save_terminal(&mut self) -> Option<String>;
    /// Restore terminal mode string (or `stty sane` when `None`).
    fn restore_terminal(&mut self, saved: Option<&str>);
}

/// Spawned sandbox child abstraction (injectable for deterministic tests).
pub trait SandboxChild {
    /// Non-blocking exit check; returns `Some(exit_code)` once exited (`None` while running).
    fn try_wait(&mut self) -> Option<Option<i32>>;
    /// Deliver `SIGTERM` to the child.
    fn send_term(&mut self);
    /// Deliver `SIGKILL` to the child.
    fn send_kill(&mut self);
}

/// Preflight, spawn, clock, and sleep abstraction (injectable for deterministic tests).
pub trait SandboxRunner {
    /// Check Linux/CLI/bwrap/dbus-run-session preflight.
    fn preflight(&self, real_home: &Path) -> PreflightResult;
    /// Spawn the sandboxed CLI with inherited terminal I/O.
    fn spawn(
        &mut self,
        file: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Box<dyn SandboxChild>;
    /// Current UTC epoch milliseconds.
    fn now_ms(&self) -> i64;
    /// Sleep for `ms` milliseconds.
    fn sleep_ms(&mut self, ms: u64);
}

/// Native credential verification and live-login backend (injectable for offline tests).
pub trait SignInBackend {
    /// Verify staged credential bytes and return a fresh authoritative [`LiveIdentity`].
    fn validate_credential(&mut self, bytes: &[u8]) -> Option<LiveIdentity>;
    /// Read the current live native login bytes and SHA-256 fingerprint, if present.
    fn read_live_credential(&mut self) -> Option<(Vec<u8>, String)>;
    /// Atomically replace the live native login if its revision matches `expected_fingerprint`.
    fn install_live_credential(&mut self, bytes: &[u8], expected_fingerprint: &str) -> bool;
}

/// Complete dependencies for [`run_terminal_signin`].
pub struct TerminalSignInDeps<'a> {
    /// CCS configuration directory (`<ccs>`).
    pub ccs_dir: &'a Path,
    /// Real user home directory (`$HOME`).
    pub real_home: &'a Path,
    /// Caller environment snapshot (allowlist-filtered before launch).
    pub env: &'a BTreeMap<String, String>,
    /// Terminal I/O seam.
    pub io: &'a mut dyn TerminalIo,
    /// Credential verification and live-login renewal seam.
    pub backend: &'a mut dyn SignInBackend,
    /// Preflight, process spawn, clock, and wait seam.
    pub runner: &'a mut dyn SandboxRunner,
    /// Maximum wait before stopping the sandboxed CLI (default 15 minutes).
    pub timeout_ms: u64,
    /// Poll interval in milliseconds (default 250ms).
    pub poll_ms: u64,
    /// SIGTERM-to-SIGKILL grace in milliseconds (default 2000ms).
    pub stop_grace_ms: u64,
    /// Whether to install real POSIX signal handlers (`SIGINT`/`SIGQUIT`/`SIGTSTP`/`SIGHUP`/`SIGTERM`).
    pub guard_signals: bool,
    /// Optional external abort signal slot (tests store `1` for SIGHUP / `15` for SIGTERM).
    pub abort_signal: Option<Arc<AtomicI32>>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LifecycleErrorCode {
    ActivationRunning,
    SignInRunning,
    UnknownAccount,
    IdInUse,
    TooManyAccounts,
    DuplicateIdentity,
    IdentityMismatch,
    WriteFailed,
    LiveLoginNotUpdated,
}

impl LifecycleErrorCode {
    fn message(self) -> &'static str {
        match self {
            Self::ActivationRunning => {
                "Another change to the saved Antigravity profiles is running. Wait for it to finish, then try again."
            }
            Self::SignInRunning => "A sign-in for this profile is already running.",
            Self::UnknownAccount => "That Antigravity profile no longer exists. Nothing was saved.",
            Self::IdInUse => {
                "Another sign-in saved a profile with this name meanwhile. Nothing was saved."
            }
            Self::TooManyAccounts => "The most Antigravity profiles allowed are already saved.",
            Self::DuplicateIdentity => {
                "That Google account is already saved in another profile. Nothing was saved."
            }
            Self::IdentityMismatch => {
                "You signed in to a different Google account than this profile has. Nothing was saved."
            }
            Self::WriteFailed => {
                "The sign-in could not be verified or saved safely. Nothing was saved."
            }
            Self::LiveLoginNotUpdated => {
                "The new sign-in was saved to its profile, but the live Antigravity login could not be updated. Sign in again."
            }
        }
    }
}

struct DefaultIo;

impl TerminalIo for DefaultIo {
    fn is_interactive(&self) -> bool {
        #[cfg(unix)]
        {
            #[allow(unsafe_code)]
            // SAFETY: isatty queries kernel tty status of fds 0 and 1 without touching memory.
            unsafe {
                libc::isatty(0) == 1 && libc::isatty(1) == 1
            }
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    fn write(&mut self, text: &str) {
        print!("{text}");
    }

    fn save_terminal(&mut self) -> Option<String> {
        let out = Command::new("/bin/stty")
            .arg("-g")
            .stdin(Stdio::inherit())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let trimmed = String::from_utf8(out.stdout).ok()?.trim().to_owned();
        (!trimmed.is_empty() && trimmed.bytes().all(|b| b.is_ascii_hexdigit() || b == b':'))
            .then_some(trimmed)
    }

    fn restore_terminal(&mut self, saved: Option<&str>) {
        let arg = saved.unwrap_or("sane");
        let _ = Command::new("/bin/stty")
            .arg(arg)
            .stdin(Stdio::inherit())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

struct HostBackend<'a> {
    host: Option<&'a Host>,
}

impl SignInBackend for HostBackend<'_> {
    fn validate_credential(&mut self, bytes: &[u8]) -> Option<LiveIdentity> {
        self.host?.validate_credential(bytes)
    }

    fn read_live_credential(&mut self) -> Option<(Vec<u8>, String)> {
        self.host?.read_native_credential()
    }

    fn install_live_credential(&mut self, bytes: &[u8], expected_fingerprint: &str) -> bool {
        self.host
            .is_some_and(|h| h.install_native_credential(bytes, expected_fingerprint))
    }
}

struct RealChild {
    child: Option<Child>,
    exited: Option<Option<i32>>,
}

impl SandboxChild for RealChild {
    fn try_wait(&mut self) -> Option<Option<i32>> {
        if let Some(code) = self.exited {
            return Some(code);
        }
        let Some(child) = self.child.as_mut() else {
            return Some(Some(127));
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                let code = status.code();
                self.exited = Some(code);
                Some(code)
            }
            Ok(None) => None,
            Err(_) => {
                self.exited = Some(Some(127));
                Some(Some(127))
            }
        }
    }

    fn send_term(&mut self) {
        #[cfg(unix)]
        if let Some(child) = &self.child
            && let Ok(pid) = i32::try_from(child.id())
        {
            #[allow(unsafe_code)]
            // SAFETY: libc::kill sends SIGTERM to the child pid we own.
            unsafe {
                libc::kill(pid, libc::SIGTERM);
            }
        }
    }

    fn send_kill(&mut self) {
        if let Some(child) = self.child.as_mut() {
            let _ = child.kill();
        }
    }
}

struct RealRunner;

impl SandboxRunner for RealRunner {
    fn preflight(&self, real_home: &Path) -> PreflightResult {
        antigravity_signin_preflight(real_home)
    }

    fn spawn(
        &mut self,
        file: &str,
        args: &[String],
        env: &BTreeMap<String, String>,
    ) -> Box<dyn SandboxChild> {
        let mut cmd = Command::new(file);
        cmd.args(args)
            .env_clear()
            .envs(env)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit());
        match cmd.spawn() {
            Ok(child) => Box::new(RealChild {
                child: Some(child),
                exited: None,
            }),
            Err(_) => Box::new(RealChild {
                child: None,
                exited: Some(Some(127)),
            }),
        }
    }

    fn now_ms(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0)
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(Duration::from_millis(ms));
    }
}

/// Detailed preflight matching `src/antigravity/signin-sandbox.ts:antigravitySignInPreflight`.
pub fn antigravity_signin_preflight(real_home: &Path) -> PreflightResult {
    if !cfg!(target_os = "linux") {
        return PreflightResult::Failed {
            reason: "preflight_failed",
            detail: "Antigravity sign-in runs on Ubuntu.".to_owned(),
        };
    }
    let abs_home = std::path::absolute(real_home).unwrap_or_else(|_| real_home.to_path_buf());
    let uid = current_uid();
    let binary = abs_home.join(".local/bin/agy");
    match std::fs::symlink_metadata(&binary) {
        Err(_) => {
            return PreflightResult::Failed {
                reason: "tool_missing",
                detail: "The Antigravity CLI is not installed at ~/.local/bin/agy.".to_owned(),
            };
        }
        Ok(meta) => {
            if !meta.is_file() || meta.file_type().is_symlink() {
                return PreflightResult::Failed {
                    reason: "tool_missing",
                    detail: "The Antigravity CLI is not a regular file.".to_owned(),
                };
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if uid.is_some_and(|u| meta.uid() != u) {
                    return PreflightResult::Failed {
                        reason: "tool_missing",
                        detail: "The Antigravity CLI is owned by another user.".to_owned(),
                    };
                }
                if meta.mode() & 0o022 != 0 {
                    return PreflightResult::Failed {
                        reason: "tool_missing",
                        detail: "The Antigravity CLI is writable by other users.".to_owned(),
                    };
                }
                if meta.mode() & 0o100 == 0 {
                    return PreflightResult::Failed {
                        reason: "tool_missing",
                        detail: "The Antigravity CLI is not executable.".to_owned(),
                    };
                }
            }
        }
    }
    for tool in [BWRAP, DBUS_RUN_SESSION] {
        let ok =
            read::stat(Path::new(tool)).is_ok_and(|s| s.file) && is_executable(Path::new(tool));
        if !ok {
            return PreflightResult::Failed {
                reason: "preflight_failed",
                detail: format!("{tool} is needed for the isolated sign-in."),
            };
        }
    }
    if bounded_run(
        BWRAP,
        &[
            "--die-with-parent",
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/tmp",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "/bin/true",
        ],
        10_000,
    )
    .is_none()
    {
        return PreflightResult::Failed {
            reason: "preflight_failed",
            detail: "This computer does not allow the isolated namespaces the sign-in needs."
                .to_owned(),
        };
    }
    let apparmor_leaf = read::stat(Path::new(APPARMOR_QUERY_LEAF))
        .is_ok_and(|s| s.file)
        .then(|| APPARMOR_QUERY_LEAF.to_owned());
    let runtime_dir = uid.and_then(|u| {
        let candidate = format!("/run/user/{u}");
        read::stat(Path::new(&candidate))
            .is_ok_and(|s| s.dir)
            .then_some(candidate)
    });
    PreflightResult::Ok {
        native_binary: binary,
        apparmor_leaf,
        runtime_dir,
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && (m.mode() & 0o111 != 0))
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

// ---------------------------------------------------------------------------
// Process-wide POSIX signal guard (SIGINT/SIGQUIT/SIGTSTP ignored; SIGHUP/SIGTERM abort)
// ---------------------------------------------------------------------------

#[cfg(unix)]
static GLOBAL_ABORT_SIGNAL: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
extern "C" fn record_abort_signal(sig: libc::c_int) {
    GLOBAL_ABORT_SIGNAL.store(sig, Ordering::SeqCst);
}

#[cfg(unix)]
struct AbortSignalGuard {
    old_hup: libc::sighandler_t,
    old_term: libc::sighandler_t,
    tty_ignored: bool,
    old_int: libc::sighandler_t,
    old_quit: libc::sighandler_t,
    old_tstp: libc::sighandler_t,
}

#[cfg(unix)]
impl AbortSignalGuard {
    fn install_abort() -> Self {
        GLOBAL_ABORT_SIGNAL.store(0, Ordering::SeqCst);
        #[allow(unsafe_code)]
        // SAFETY: record_abort_signal only performs a lock-free atomic store.
        let (old_hup, old_term) = unsafe {
            let handler = record_abort_signal as *const () as libc::sighandler_t;
            (
                libc::signal(libc::SIGHUP, handler),
                libc::signal(libc::SIGTERM, handler),
            )
        };
        Self {
            old_hup,
            old_term,
            tty_ignored: false,
            old_int: 0,
            old_quit: 0,
            old_tstp: 0,
        }
    }

    fn ignore_tty_signals(&mut self) {
        if self.tty_ignored {
            return;
        }
        #[allow(unsafe_code)]
        // SAFETY: SIG_IGN is a standard POSIX signal disposition.
        unsafe {
            self.old_int = libc::signal(libc::SIGINT, libc::SIG_IGN);
            self.old_quit = libc::signal(libc::SIGQUIT, libc::SIG_IGN);
            self.old_tstp = libc::signal(libc::SIGTSTP, libc::SIG_IGN);
        }
        self.tty_ignored = true;
    }

    fn triggered() -> i32 {
        GLOBAL_ABORT_SIGNAL.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
impl Drop for AbortSignalGuard {
    fn drop(&mut self) {
        #[allow(unsafe_code)]
        // SAFETY: restoring previously saved signal dispositions.
        unsafe {
            if self.tty_ignored {
                libc::signal(libc::SIGINT, self.old_int);
                libc::signal(libc::SIGQUIT, self.old_quit);
                libc::signal(libc::SIGTSTP, self.old_tstp);
            }
            libc::signal(libc::SIGHUP, self.old_hup);
            libc::signal(libc::SIGTERM, self.old_term);
        }
    }
}

// ---------------------------------------------------------------------------
// Holder record / staleness helpers (shared rules with holder-record.ts)
// ---------------------------------------------------------------------------

fn holder_record(now_ms: i64) -> Option<String> {
    let pid = std::process::id();
    let started = aac_num::py::iso_millis(now_ms)?;
    Some(
        json!({
            "pid": pid,
            "startedAt": started,
            "processStart": process_start(pid),
            "bootId": boot_id(),
        })
        .to_string(),
    )
}

fn process_start(pid: u32) -> Option<String> {
    let bytes = read_bounded(Path::new(&format!("/proc/{pid}/stat")), 8192).ok()?;
    let stat = std::str::from_utf8(&bytes).ok()?;
    let start = stat.rsplit_once(')')?.1.split_whitespace().nth(19)?;
    ((1..=20).contains(&start.len()) && start.bytes().all(|b| b.is_ascii_digit()))
        .then(|| start.to_owned())
}

fn boot_id() -> Option<String> {
    let bytes = read_bounded(Path::new("/proc/sys/kernel/random/boot_id"), 64).ok()?;
    let id = std::str::from_utf8(&bytes).ok()?.trim();
    (id.len() == 36
        && id
            .bytes()
            .all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
    .then(|| id.to_owned())
}

fn holder_stale(
    bytes: &[u8],
    mtime_ms: u64,
    now_ms: u64,
    current_boot: Option<&str>,
    silent_after_ms: u64,
) -> Option<bool> {
    let record: Value = serde_json::from_slice(bytes).ok()?;
    let pid = u32::try_from(record.get("pid")?.as_u64()?).ok()?;
    if pid == 0 {
        return None;
    }
    let started = u64::try_from(runtime::js_date(record.get("startedAt")?.as_str()?)?).ok()?;
    let start = match record.get("processStart") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if (1..=20).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_digit()) =>
        {
            Some(s.as_str())
        }
        _ => return None,
    };
    let recorded_boot = match record.get("bootId") {
        None | Some(Value::Null) => None,
        Some(Value::String(s))
            if s.len() == 36
                && s.bytes()
                    .all(|b| b == b'-' || b.is_ascii_digit() || (b'a'..=b'f').contains(&b)) =>
        {
            Some(s.as_str())
        }
        _ => return None,
    };
    if recorded_boot
        .zip(current_boot)
        .is_some_and(|(rec, cur)| rec != cur)
        || process::is_dead(pid)
        || start
            .zip(process_start(pid))
            .is_some_and(|(rec, cur)| rec != cur)
    {
        return Some(true);
    }
    Some(now_ms.saturating_sub(mtime_ms.max(started)) > silent_after_ms)
}

fn signin_marker_running(root: &Path, profile: &str, now_ms: u64) -> bool {
    let file = root
        .join("antigravity-signin")
        .join(format!("{profile}.running"));
    let Ok(stat) = read::stat(&file) else {
        return read::entry_exists(&file).unwrap_or(true);
    };
    let mtime = stat.mtime_ms as u64;
    let boot = boot_id();
    let stale = read_bounded(&file, 512)
        .ok()
        .and_then(|bytes| {
            holder_stale(
                &bytes,
                mtime,
                now_ms,
                boot.as_deref(),
                STALE_MARKER_TIMEOUT_MS,
            )
        })
        .unwrap_or_else(|| now_ms.saturating_sub(mtime) > STALE_MARKER_TIMEOUT_MS);
    !stale
}

fn random_hex_8() -> Option<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).ok()?;
    let mut out = String::with_capacity(16);
    for b in bytes {
        use std::fmt::Write as _;
        let _ = write!(out, "{b:02x}");
    }
    Some(out)
}

fn ensure_ccs_dir(ccs: &Path) -> std::io::Result<PathBuf> {
    if !ccs.exists() {
        std::fs::create_dir_all(ccs)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(ccs, std::fs::Permissions::from_mode(0o700))?;
        }
    }
    std::fs::canonicalize(ccs)
}

// ---------------------------------------------------------------------------
// Core terminal sign-in workflow (`terminal-signin.ts:runAntigravityTerminalSignIn`)
// ---------------------------------------------------------------------------

/// Run one terminal Antigravity sign-in; returns the CLI exit code.
pub fn run_terminal_signin(profile_id: &str, deps: &mut TerminalSignInDeps<'_>) -> i32 {
    let fail = |io: &mut dyn TerminalIo, msg: &str| -> i32 {
        io.write(&format!("[X] {msg}\n"));
        1
    };

    if !runtime::profile_id_ok(profile_id) {
        return fail(
            deps.io,
            "Profile names start with a lowercase letter and use lowercase letters, digits, - or _ (at most 48).",
        );
    }

    let now_u64 = u64::try_from(deps.runner.now_ms()).unwrap_or(0);
    let boot = boot_id();

    let mode = match runtime::read_registry(deps.ccs_dir) {
        RegistryRead::Unreadable => {
            return fail(
                deps.io,
                "The saved Antigravity profiles could not be read safely. Nothing was started.",
            );
        }
        RegistryRead::Absent => SignInMode::Add,
        RegistryRead::Available(state) => {
            match AntigravityMaintenanceLease::lock_held_live(
                deps.ccs_dir,
                now_u64,
                STALE_LOCK_TIMEOUT_MS,
                |bytes, mtime| {
                    holder_stale(
                        bytes,
                        mtime,
                        now_u64,
                        boot.as_deref(),
                        STALE_LOCK_TIMEOUT_MS,
                    )
                },
            ) {
                Ok(true) => return fail(deps.io, LifecycleErrorCode::ActivationRunning.message()),
                Ok(false) => {}
                Err(_) => {
                    return fail(
                        deps.io,
                        "The saved Antigravity profiles could not be read safely. Nothing was started.",
                    );
                }
            }
            if state.profile(profile_id).is_some() {
                SignInMode::SignInAgain
            } else {
                if state.profiles.len() >= MAX_PROFILES {
                    return fail(deps.io, LifecycleErrorCode::TooManyAccounts.message());
                }
                SignInMode::Add
            }
        }
    };
    // Also check lock when registry is Absent (antigravity-profiles folder might exist with a lock).
    if matches!(mode, SignInMode::Add) {
        match AntigravityMaintenanceLease::lock_held_live(
            deps.ccs_dir,
            now_u64,
            STALE_LOCK_TIMEOUT_MS,
            |bytes, mtime| {
                holder_stale(
                    bytes,
                    mtime,
                    now_u64,
                    boot.as_deref(),
                    STALE_LOCK_TIMEOUT_MS,
                )
            },
        ) {
            Ok(true) => return fail(deps.io, LifecycleErrorCode::ActivationRunning.message()),
            Ok(false) => {}
            Err(_) => {
                return fail(
                    deps.io,
                    "The saved Antigravity profiles could not be read safely. Nothing was started.",
                );
            }
        }
    }

    if !deps.io.is_interactive() {
        return fail(
            deps.io,
            "Run this in an interactive terminal: the sign-in needs your keyboard.",
        );
    }

    let (native_binary, apparmor_leaf, runtime_dir) = match deps.runner.preflight(deps.real_home) {
        PreflightResult::Ok {
            native_binary,
            apparmor_leaf,
            runtime_dir,
        } => (native_binary, apparmor_leaf, runtime_dir),
        PreflightResult::Failed { detail, .. } => {
            return fail(deps.io, &format!("{detail} Nothing was started."));
        }
    };

    let Ok(ccs_root) = ensure_ccs_dir(deps.ccs_dir) else {
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    };
    if profile_staging::antigravity_root(&ccs_root).is_err() {
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    }

    // Claim <profile>.running marker (reclaiming stale marker if needed).
    let marker_name = format!("{profile_id}.running");
    let marker_path = ccs_root.join("antigravity-signin").join(&marker_name);
    let Some(holder_bytes) = holder_record(deps.runner.now_ms()) else {
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    };
    let Some(marker_nonce) = random_hex_8() else {
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    };
    let marker_temp = format!(".marker-{marker_nonce}");
    if let Ok((dev, ino)) = profile_staging::marker_identity(&marker_path)
        && !signin_marker_running(&ccs_root, profile_id, now_u64)
    {
        let _ = profile_staging::remove_antigravity_marker(&ccs_root, &marker_name, dev, ino);
    }
    match profile_staging::claim_antigravity_marker(
        &ccs_root,
        &marker_name,
        &marker_temp,
        holder_bytes.as_bytes(),
    ) {
        Ok(true) => {}
        Ok(false) => return fail(deps.io, LifecycleErrorCode::SignInRunning.message()),
        Err(_) => {
            return fail(
                deps.io,
                "The private sign-in folder could not be prepared. Nothing was started.",
            );
        }
    }
    let Ok((marker_dev, marker_ino)) = profile_staging::marker_identity(&marker_path) else {
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    };
    let release_marker = |root: &Path| {
        let _ =
            profile_staging::remove_antigravity_marker(root, &marker_name, marker_dev, marker_ino);
    };

    // Prepare isolated staging directory.
    let Some(stage_nonce) = random_hex_8() else {
        release_marker(&ccs_root);
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    };
    let staging_name = format!(".staging-{stage_nonce}");
    if profile_staging::prepare_antigravity(
        &ccs_root,
        &staging_name,
        PRIVATE_SESSION_BUS_CONFIG.as_bytes(),
    )
    .is_err()
    {
        release_marker(&ccs_root);
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    }

    let stage_dir = ccs_root.join("antigravity-signin").join(&staging_name);
    let mask_dir = stage_dir.join("home/.gemini");
    let token_path = mask_dir.join("antigravity-cli/antigravity-oauth-token");
    let stubs_dir = stage_dir.join("browser-stubs");
    let bus_config = stage_dir.join("session-bus.conf");

    let env_json = Value::Object(
        deps.env
            .iter()
            .map(|(k, v)| (k.clone(), Value::String(v.clone())))
            .collect(),
    );
    let abs_home =
        std::path::absolute(deps.real_home).unwrap_or_else(|_| deps.real_home.to_path_buf());
    let Some(sandbox_cfg) = signin_callbacks::antigravity_sandbox(
        &bus_config.to_string_lossy(),
        &mask_dir.to_string_lossy(),
        &stubs_dir.to_string_lossy(),
        &abs_home.to_string_lossy(),
        &native_binary.to_string_lossy(),
        apparmor_leaf.as_deref(),
        runtime_dir.as_deref(),
        &env_json,
    ) else {
        let _ = profile_staging::cleanup_antigravity(&ccs_root, &staging_name);
        release_marker(&ccs_root);
        return fail(
            deps.io,
            "The private sign-in folder could not be prepared. Nothing was started.",
        );
    };
    let sandbox_args: Vec<String> = sandbox_cfg["sandboxArgs"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default();
    let sandbox_env: BTreeMap<String, String> = sandbox_cfg["env"]
        .as_object()
        .map(|obj| {
            obj.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned())))
                .collect()
        })
        .unwrap_or_default();

    #[cfg(unix)]
    let mut sig_guard = deps.guard_signals.then(AbortSignalGuard::install_abort);

    let check_abort = |deps_abort: &Option<Arc<AtomicI32>>, guard_real: bool| -> Option<i32> {
        if let Some(slot) = deps_abort {
            let s = slot.load(Ordering::SeqCst);
            if s > 0 {
                return Some(128 + s);
            }
        }
        #[cfg(unix)]
        if guard_real {
            let s = AbortSignalGuard::triggered();
            if s > 0 {
                return Some(128 + s);
            }
        }
        let _ = guard_real;
        None
    };

    let intro = match mode {
        SignInMode::Add => format!("Adding Antigravity profile \"{profile_id}\"."),
        SignInMode::SignInAgain => {
            format!("Signing in again to Antigravity profile \"{profile_id}\".")
        }
    };
    deps.io.write(&format!(
        "{intro}\n\
         Antigravity starts in a private sign-in home on this computer. Your history and\n\
         settings are not touched; if this profile is the live login, it is renewed too.\n\
         1. Choose Google OAuth.\n\
         2. Open the link it shows in your browser and sign in to the Google account for\n\
            \"{profile_id}\".\n\
         3. Paste the code back here and press Enter.\n\
         Antigravity closes by itself as soon as the sign-in is saved. Ctrl+C cancels.\n\
         \n"
    ));

    let saved_tty = deps.io.save_terminal();
    let mut terminal_saved = true;

    #[cfg(unix)]
    if let Some(guard) = sig_guard.as_mut() {
        guard.ignore_tty_signals();
    }

    let mut child = deps.runner.spawn(BWRAP, &sandbox_args, &sandbox_env);
    let deadline = deps.runner.now_ms().saturating_add(deps.timeout_ms as i64);
    let mut complete = false;
    let mut timed_out = false;
    let mut stable_checks = 0u32;
    let mut aborted_code: Option<i32> = None;

    let finished = |path: &Path| {
        matches!(
            masked_credential_state(path),
            MaskedCredentialState::Complete | MaskedCredentialState::Unsafe
        )
    };

    while child.try_wait().is_none() {
        if let Some(code) = check_abort(&deps.abort_signal, deps.guard_signals) {
            aborted_code = Some(code);
            break;
        }
        deps.runner.sleep_ms(deps.poll_ms);
        if let Some(code) = check_abort(&deps.abort_signal, deps.guard_signals) {
            aborted_code = Some(code);
            break;
        }
        if finished(&token_path) {
            stable_checks += 1;
            if stable_checks >= 2 {
                complete = true;
                break;
            }
        } else {
            stable_checks = 0;
        }
        if deps.runner.now_ms() >= deadline {
            timed_out = true;
            break;
        }
    }

    if let Some(code) = aborted_code {
        child.send_kill();
        let _ = profile_staging::cleanup_antigravity(&ccs_root, &staging_name);
        release_marker(&ccs_root);
        if terminal_saved {
            deps.io.restore_terminal(saved_tty.as_deref());
            deps.io.write(TERMINAL_RESET);
        }
        #[cfg(unix)]
        drop(sig_guard);
        return code;
    }

    if child.try_wait().is_none() {
        child.send_term();
        let stop_by = deps
            .runner
            .now_ms()
            .saturating_add(deps.stop_grace_ms as i64);
        while child.try_wait().is_none() && deps.runner.now_ms() < stop_by {
            deps.runner.sleep_ms(50);
        }
        if child.try_wait().is_none() {
            child.send_kill();
        }
        let mut hard_spins = 0u32;
        while child.try_wait().is_none() && hard_spins < 200 {
            deps.runner.sleep_ms(50);
            hard_spins += 1;
        }
    }

    terminal_saved = false;
    deps.io.restore_terminal(saved_tty.as_deref());
    deps.io.write(TERMINAL_RESET);

    let exit_code = (|| -> i32 {
        if !complete {
            complete = finished(&token_path);
        }
        if !complete {
            return fail(
                deps.io,
                if timed_out {
                    "The sign-in took longer than 15 minutes and was stopped. Nothing was saved."
                } else {
                    "The sign-in did not finish. Nothing was saved."
                },
            );
        }
        deps.io.write("Checking the new sign-in with Google...\n");
        let Ok(credential_bytes) = read_masked_credential(&token_path) else {
            return fail(deps.io, LifecycleErrorCode::WriteFailed.message());
        };
        match import_signin(
            &ccs_root,
            profile_id,
            mode,
            &credential_bytes,
            deps.backend,
            deps.runner.now_ms(),
        ) {
            Ok((email, live_renewed)) => {
                let done = match mode {
                    SignInMode::Add => format!(
                        "Saved Antigravity profile \"{profile_id}\" ({email}). It now appears in the dashboard.\n"
                    ),
                    SignInMode::SignInAgain => format!(
                        "Signed in again: Antigravity profile \"{profile_id}\" ({email}) is saved.\n"
                    ),
                };
                deps.io.write(&done);
                if live_renewed {
                    deps.io.write(
                        "The live Antigravity login was renewed too. Restart an open Antigravity CLI.\n",
                    );
                }
                0
            }
            Err(code) => fail(deps.io, code.message()),
        }
    })();

    if terminal_saved {
        deps.io.restore_terminal(saved_tty.as_deref());
        deps.io.write(TERMINAL_RESET);
    }
    #[cfg(unix)]
    drop(sig_guard);
    if profile_staging::cleanup_antigravity(&ccs_root, &staging_name).is_err() {
        deps.io.write(
            "[!] The private sign-in folder could not be removed; it is swept within a day.\n",
        );
    }
    release_marker(&ccs_root);
    exit_code
}

fn current_registry(root: &Path) -> Result<(Value, Option<RegistryState>), LifecycleErrorCode> {
    if let Some((document, parsed, _)) = runtime::read_document(root) {
        return Ok((document, Some(parsed)));
    }
    match runtime::newest_generation(&root.join("antigravity-profiles")) {
        Ok(None) => Ok((
            json!({"version":1,"revision":0,"profiles":[],"active":null,"transaction":null}),
            None,
        )),
        _ => Err(LifecycleErrorCode::WriteFailed),
    }
}

fn publish_registry(
    root: &Path,
    lease: &AntigravityMaintenanceLease,
    mut document: Value,
) -> Option<()> {
    let generation = document["revision"].as_i64()?.checked_add(1)?;
    document["revision"] = json!(generation);
    document["transaction"] = Value::Null;
    runtime::validate_state(&document)?;
    lease
        .writer()
        .antigravity_index(
            root,
            &format!("{generation:012}"),
            &aac_views::js_json::write(&document),
        )
        .ok()
}

fn grant_digest(bytes: &[u8]) -> Option<String> {
    let json: Value = serde_json::from_slice(bytes).ok()?;
    let token = json["token"]["refresh_token"].as_str()?;
    (!token.is_empty()).then(|| sha256_hex(token.as_bytes()))
}

fn live_to_renew(
    root: &Path,
    backend: &mut dyn SignInBackend,
    profile: &ProfileRecord,
) -> Option<String> {
    let (bytes, fingerprint) = backend.read_live_credential()?;
    if fingerprint == profile.credential.fingerprint {
        return Some(fingerprint);
    }
    if let Some(identity) = backend.validate_credential(&bytes) {
        return (identity.identity_key() == profile.identity_key).then_some(fingerprint);
    }
    let saved = runtime::read_credential_bytes(root, profile)?;
    let a = grant_digest(&bytes)?;
    let b = grant_digest(&saved)?;
    (a == b).then_some(fingerprint)
}

fn import_signin(
    root: &Path,
    profile_id: &str,
    mode: SignInMode,
    bytes: &[u8],
    backend: &mut dyn SignInBackend,
    now_ms: i64,
) -> Result<(String, bool), LifecycleErrorCode> {
    AntigravityMaintenanceLease::initialize(root).map_err(|_| LifecycleErrorCode::WriteFailed)?;
    let holder = holder_record(now_ms).ok_or(LifecycleErrorCode::WriteFailed)?;
    let now_u64 = u64::try_from(now_ms).unwrap_or(0);
    let boot = boot_id();
    let (lease, recovered) = AntigravityMaintenanceLease::claim_or_recover(
        root,
        holder.as_bytes(),
        now_u64,
        STALE_LOCK_TIMEOUT_MS,
        |b, mtime| holder_stale(b, mtime, now_u64, boot.as_deref(), STALE_LOCK_TIMEOUT_MS),
    )
    .map_err(|err| {
        if err.kind() == std::io::ErrorKind::WouldBlock {
            LifecycleErrorCode::ActivationRunning
        } else {
            LifecycleErrorCode::WriteFailed
        }
    })?;
    if let Some(reason) = recovered {
        eprintln!("antigravity: recovered a stale transaction lock ({reason})");
    }

    let (previous, parsed) = current_registry(root)?;
    let profiles = parsed
        .as_ref()
        .map_or(&[][..], |parsed| parsed.profiles.as_slice());
    let existing = profiles.iter().find(|record| record.id == profile_id);
    match mode {
        SignInMode::Add => {
            if existing.is_some() {
                return Err(LifecycleErrorCode::IdInUse);
            }
            if profiles.len() >= MAX_PROFILES {
                return Err(LifecycleErrorCode::TooManyAccounts);
            }
        }
        SignInMode::SignInAgain => {
            if existing.is_none() {
                return Err(LifecycleErrorCode::UnknownAccount);
            }
        }
    }

    let identity = backend
        .validate_credential(bytes)
        .ok_or(LifecycleErrorCode::WriteFailed)?;
    if identity
        .age_ms(now_ms)
        .is_none_or(|age| !(-60_000..=300_000).contains(&age))
    {
        return Err(LifecycleErrorCode::WriteFailed);
    }
    let key = identity.identity_key();
    if existing.is_some_and(|profile| profile.identity_key != key) {
        return Err(LifecycleErrorCode::IdentityMismatch);
    }
    if profiles.iter().any(|record| {
        record.id != profile_id && (record.identity_key == key || record.email == identity.email)
    }) {
        return Err(LifecycleErrorCode::DuplicateIdentity);
    }

    let live_expected = existing.and_then(|record| live_to_renew(root, backend, record));
    let started = aac_num::py::iso_millis(now_ms).ok_or(LifecycleErrorCode::WriteFailed)?;
    let fingerprint = sha256_hex(bytes);
    let mut next = previous.clone();
    let rows = next["profiles"]
        .as_array_mut()
        .ok_or(LifecycleErrorCode::WriteFailed)?;
    crate::antigravity_runtime::replace_profile_row(
        rows,
        profile_id,
        json!({
            "id": profile_id,
            "email": identity.email,
            "identityKey": key,
            "plan": identity.plan,
            "credential": {
                "fingerprint": fingerprint,
                "format": CREDENTIAL_FORMAT,
                "identity": {
                    "email": identity.email,
                    "subject": identity.subject,
                    "plan": identity.plan,
                    "verifiedAt": identity.verified_at,
                    "source": identity.source,
                },
                "savedAt": started,
            }
        }),
    );

    let save = |lease: &AntigravityMaintenanceLease| -> Result<bool, LifecycleErrorCode> {
        lease
            .writer()
            .antigravity_credential(root, profile_id, &fingerprint, bytes)
            .map_err(|_| LifecycleErrorCode::WriteFailed)?;
        publish_registry(root, lease, next.clone()).ok_or(LifecycleErrorCode::WriteFailed)?;
        let prev_fp = existing.map(|r| r.credential.fingerprint.as_str());
        if let Ok(left) = lease
            .writer()
            .antigravity_prune(root, profile_id, &fingerprint, prev_fp)
            && left > 0
        {
            eprintln!("accounts.antigravity.credentials.left_in_place count={left}");
        }
        Ok(true)
    };

    if let Some(expected) = live_expected {
        // Run the renewal state machine synchronously via a tiny single-step future runner
        // (antigravity_renewal::run only awaits the immediate Ready futures we return).
        let outcome = poll_ready(antigravity_renewal::run(|step| {
            let res: Result<bool, LifecycleErrorCode> = match step {
                Effect::Save => save(&lease),
                Effect::Install => Ok(backend.install_live_credential(bytes, &expected)),
                Effect::Verify => Ok(backend
                    .read_live_credential()
                    .is_some_and(|(_, fp)| fp == fingerprint)),
                Effect::Restore => {
                    if existing
                        .and_then(|record| runtime::read_credential_bytes(root, record))
                        .is_none()
                    {
                        Ok(false)
                    } else {
                        let mut restore = previous.clone();
                        restore["revision"] =
                            json!(next["revision"].as_i64().unwrap_or_default() + 1);
                        Ok(publish_registry(root, &lease, restore).is_some())
                    }
                }
            };
            std::future::ready(res)
        }))?;
        match outcome {
            Outcome::Renewed => Ok((identity.email, true)),
            Outcome::Restored => Err(LifecycleErrorCode::WriteFailed),
            Outcome::ProfileOnly => Err(LifecycleErrorCode::LiveLoginNotUpdated),
        }
    } else {
        save(&lease)?;
        Ok((identity.email, false))
    }
}

fn poll_ready<F: std::future::Future>(fut: F) -> F::Output {
    use std::task::{Context, Poll, Waker};
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    let mut fut = std::pin::pin!(fut);
    match fut.as_mut().poll(&mut cx) {
        Poll::Ready(val) => val,
        Poll::Pending => unreachable!("renewal steps are synchronous"),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::cell::Cell;

    struct FakeIo {
        interactive: bool,
        output: String,
        saved: Option<String>,
        restored: Vec<Option<String>>,
    }

    impl TerminalIo for FakeIo {
        fn is_interactive(&self) -> bool {
            self.interactive
        }
        fn write(&mut self, text: &str) {
            self.output.push_str(text);
        }
        fn save_terminal(&mut self) -> Option<String> {
            self.saved.clone()
        }
        fn restore_terminal(&mut self, saved: Option<&str>) {
            self.restored.push(saved.map(str::to_owned));
        }
    }

    struct FakeChild {
        polls_before_exit: Option<u32>,
        polls: u32,
        termed: bool,
        killed: bool,
        on_poll: Option<Box<dyn FnMut(u32)>>,
    }

    impl SandboxChild for FakeChild {
        fn try_wait(&mut self) -> Option<Option<i32>> {
            if self.killed || self.termed {
                return Some(Some(0));
            }
            self.polls += 1;
            if let Some(cb) = self.on_poll.as_mut() {
                cb(self.polls);
            }
            if let Some(limit) = self.polls_before_exit
                && self.polls >= limit
            {
                return Some(Some(0));
            }
            None
        }
        fn send_term(&mut self) {
            self.termed = true;
        }
        fn send_kill(&mut self) {
            self.killed = true;
        }
    }

    struct FakeRunner {
        now: Cell<i64>,
        token_to_write: Option<Vec<u8>>,
        exit_without_token: bool,
        never_exit: bool,
    }

    impl SandboxRunner for FakeRunner {
        fn preflight(&self, real_home: &Path) -> PreflightResult {
            PreflightResult::Ok {
                native_binary: real_home.join(".local/bin/agy"),
                apparmor_leaf: None,
                runtime_dir: None,
            }
        }

        fn spawn(
            &mut self,
            _file: &str,
            args: &[String],
            _env: &BTreeMap<String, String>,
        ) -> Box<dyn SandboxChild> {
            // Find the bound mask path (`--bind <mask> <real>/.gemini`) and write the token there.
            let mask_idx = args.iter().position(|a| a == "--bind").unwrap();
            let token_path =
                PathBuf::from(&args[mask_idx + 1]).join("antigravity-cli/antigravity-oauth-token");
            let payload = self.token_to_write.clone();
            let on_poll: Option<Box<dyn FnMut(u32)>> = payload.map(|bytes| {
                let mut written = false;
                Box::new(move |n: u32| {
                    if !written && n >= 2 {
                        std::fs::write(&token_path, &bytes).unwrap();
                        #[cfg(unix)]
                        {
                            use std::os::unix::fs::PermissionsExt;
                            std::fs::set_permissions(
                                &token_path,
                                std::fs::Permissions::from_mode(0o600),
                            )
                            .unwrap();
                        }
                        written = true;
                    }
                }) as Box<dyn FnMut(u32)>
            });
            Box::new(FakeChild {
                polls_before_exit: if self.exit_without_token {
                    Some(2)
                } else if self.never_exit {
                    None
                } else {
                    Some(50)
                },
                polls: 0,
                termed: false,
                killed: false,
                on_poll,
            })
        }

        fn now_ms(&self) -> i64 {
            self.now.get()
        }

        fn sleep_ms(&mut self, ms: u64) {
            self.now.set(self.now.get() + ms as i64);
        }
    }

    struct FakeBackend {
        identity: Option<LiveIdentity>,
        live: Option<(Vec<u8>, String)>,
        install_ok: bool,
    }

    impl SignInBackend for FakeBackend {
        fn validate_credential(&mut self, _bytes: &[u8]) -> Option<LiveIdentity> {
            self.identity.clone()
        }
        fn read_live_credential(&mut self) -> Option<(Vec<u8>, String)> {
            self.live.clone()
        }
        fn install_live_credential(&mut self, bytes: &[u8], expected_fingerprint: &str) -> bool {
            if !self.install_ok {
                return false;
            }
            if self
                .live
                .as_ref()
                .is_some_and(|(_, fp)| fp == expected_fingerprint)
            {
                self.live = Some((bytes.to_vec(), sha256_hex(bytes)));
                true
            } else {
                false
            }
        }
    }

    fn cred_bytes(access: &str, refresh: &str) -> Vec<u8> {
        json!({
            "auth_method": "consumer",
            "token": {
                "access_token": access,
                "refresh_token": refresh,
                "token_type": "Bearer",
                "expiry": "2026-11-26T23:00:00Z",
            }
        })
        .to_string()
        .into_bytes()
    }

    #[test]
    fn retired_runtime_verbs_are_rejected_and_omitted_from_help() {
        let dir = tempfile::tempdir().unwrap();
        let text = help();
        assert!(text.contains("ai-account-center antigravity signin <profile>"));
        assert!(!text.contains("antigravity status"));
        assert!(!text.contains("antigravity runtime refresh"));
        assert!(!text.contains("recover"));
        assert_eq!(run(&["status".into()], dir.path()), 1);
        assert_eq!(run(&["recover".into()], dir.path()), 1);
        assert_eq!(run(&["runtime".into(), "refresh".into()], dir.path()), 1);
        assert_eq!(run(&["activate".into()], dir.path()), 1);
    }

    #[cfg(unix)]
    #[test]
    fn terminal_signin_adds_renews_live_and_rejects_wrong_account() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let ccs = dir.path().join(".ccs");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&ccs).unwrap();
        std::fs::create_dir_all(&home).unwrap();
        std::fs::set_permissions(&ccs, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();

        let now_ms = 1_795_728_000_000i64;
        let verified_at = aac_num::py::iso_millis(now_ms).unwrap();
        let cred1 = cred_bytes("acc-1", "ref-1");
        let cred2 = cred_bytes("acc-2", "ref-2");
        let env = BTreeMap::new();

        // 1. Add new profile "work"
        let mut io = FakeIo {
            interactive: true,
            output: String::new(),
            saved: Some("0:5:bf".into()),
            restored: Vec::new(),
        };
        let mut runner = FakeRunner {
            now: Cell::new(now_ms),
            token_to_write: Some(cred1.clone()),
            exit_without_token: false,
            never_exit: false,
        };
        let mut backend = FakeBackend {
            identity: Some(LiveIdentity {
                email: "work@example.com".into(),
                subject: "1001".into(),
                plan: None,
                verified_at: verified_at.clone(),
                source: "provider-userinfo".into(),
            }),
            live: None,
            install_ok: true,
        };
        let mut deps = TerminalSignInDeps {
            ccs_dir: &ccs,
            real_home: &home,
            env: &env,
            io: &mut io,
            backend: &mut backend,
            runner: &mut runner,
            timeout_ms: TIMEOUT_MS,
            poll_ms: POLL_MS,
            stop_grace_ms: STOP_GRACE_MS,
            guard_signals: false,
            abort_signal: None,
        };
        assert_eq!(run_terminal_signin("work", &mut deps), 0, "{}", io.output);
        assert!(
            io.output
                .contains("Saved Antigravity profile \"work\" (work@example.com).")
        );
        assert_eq!(io.restored, vec![Some("0:5:bf".into())]);
        // Staging folder and running marker cleaned up
        assert_eq!(
            std::fs::read_dir(ccs.join("antigravity-signin"))
                .unwrap()
                .count(),
            0
        );

        // 2. Sign in again with wrong Google account -> IdentityMismatch, nothing changed
        let mut io2 = FakeIo {
            interactive: true,
            output: String::new(),
            saved: None,
            restored: Vec::new(),
        };
        let mut runner2 = FakeRunner {
            now: Cell::new(now_ms + 1000),
            token_to_write: Some(cred2.clone()),
            exit_without_token: false,
            never_exit: false,
        };
        let mut backend2 = FakeBackend {
            identity: Some(LiveIdentity {
                email: "other@example.com".into(),
                subject: "9999".into(),
                plan: None,
                verified_at: verified_at.clone(),
                source: "provider-userinfo".into(),
            }),
            live: Some((cred1.clone(), sha256_hex(&cred1))),
            install_ok: true,
        };
        let mut deps2 = TerminalSignInDeps {
            ccs_dir: &ccs,
            real_home: &home,
            env: &env,
            io: &mut io2,
            backend: &mut backend2,
            runner: &mut runner2,
            timeout_ms: TIMEOUT_MS,
            poll_ms: POLL_MS,
            stop_grace_ms: STOP_GRACE_MS,
            guard_signals: false,
            abort_signal: None,
        };
        assert_eq!(run_terminal_signin("work", &mut deps2), 1);
        assert!(io2.output.contains("different Google account"));

        // 3. Sign in again with same Google account while live -> renews saved AND live login
        let mut io3 = FakeIo {
            interactive: true,
            output: String::new(),
            saved: None,
            restored: Vec::new(),
        };
        let mut runner3 = FakeRunner {
            now: Cell::new(now_ms + 2000),
            token_to_write: Some(cred2.clone()),
            exit_without_token: false,
            never_exit: false,
        };
        let mut backend3 = FakeBackend {
            identity: Some(LiveIdentity {
                email: "work@example.com".into(),
                subject: "1001".into(),
                plan: None,
                verified_at: verified_at.clone(),
                source: "provider-userinfo".into(),
            }),
            live: Some((cred1.clone(), sha256_hex(&cred1))),
            install_ok: true,
        };
        let mut deps3 = TerminalSignInDeps {
            ccs_dir: &ccs,
            real_home: &home,
            env: &env,
            io: &mut io3,
            backend: &mut backend3,
            runner: &mut runner3,
            timeout_ms: TIMEOUT_MS,
            poll_ms: POLL_MS,
            stop_grace_ms: STOP_GRACE_MS,
            guard_signals: false,
            abort_signal: None,
        };
        assert_eq!(run_terminal_signin("work", &mut deps3), 0, "{}", io3.output);
        assert!(io3.output.contains(
            "Signed in again: Antigravity profile \"work\" (work@example.com) is saved."
        ));
        assert!(
            io3.output
                .contains("The live Antigravity login was renewed too.")
        );
        assert_eq!(backend3.live.as_ref().unwrap().1, sha256_hex(&cred2));

        // 4. SIGHUP abort removes staging & marker and returns 129
        let abort = Arc::new(AtomicI32::new(1)); // SIGHUP
        let mut io4 = FakeIo {
            interactive: true,
            output: String::new(),
            saved: Some("0:5:bf".into()),
            restored: Vec::new(),
        };
        let mut runner4 = FakeRunner {
            now: Cell::new(now_ms + 3000),
            token_to_write: None,
            exit_without_token: false,
            never_exit: true,
        };
        let mut deps4 = TerminalSignInDeps {
            ccs_dir: &ccs,
            real_home: &home,
            env: &env,
            io: &mut io4,
            backend: &mut backend3,
            runner: &mut runner4,
            timeout_ms: TIMEOUT_MS,
            poll_ms: POLL_MS,
            stop_grace_ms: STOP_GRACE_MS,
            guard_signals: false,
            abort_signal: Some(abort),
        };
        assert_eq!(run_terminal_signin("work", &mut deps4), 129);
        assert_eq!(
            std::fs::read_dir(ccs.join("antigravity-signin"))
                .unwrap()
                .count(),
            0
        );
    }
}
