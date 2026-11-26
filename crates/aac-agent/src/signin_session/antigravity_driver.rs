//! Native Rust port of `scripts/antigravity/signin_driver.py`:
//! allocates a PTY with ECHO disabled and 40x160 window size, drives the sandboxed
//! Antigravity CLI, auto-selects Google OAuth once when the menu appears, extracts
//! the verified authorization URL (OSC 8 or wrapped plain text across completed lines),
//! forwards submitted codes with `\n` -> `\r`, polls the masked OAuth token file every
//! 250ms until two consecutive `Complete | Unsafe` observations, and stops the child
//! process group cleanly.

use crate::signin_session::process::{Command, Handle, Killer, ProcessEvent, SpawnFailure};
use aac_files::antigravity_store::{MaskedCredentialState, masked_credential_state};
use regex::bytes::Regex;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use tokio::sync::mpsc;

const POLL_MS: u16 = 250;
const STOP_GRACE_MS: u64 = 2_000;
const KILL_GRACE_MS: u64 = 1_000;
const MAX_BUFFER: usize = 524_288;

/// Antigravity native PTY sign-in driver configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DriverSpec {
    /// Absolute path to the staged masked OAuth token file inside the staging directory.
    pub token_path: PathBuf,
    /// Exact allowed HTTPS origin (e.g. `https://accounts.google.com`).
    pub origin: String,
}

fn osc8_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        Regex::new(r"(?-u)\x1b\]8;[^;\x07\x1b]*;(https?://[^\x07\x1b]*)(?:\x07|\x1b\\)")
            .expect("valid OSC 8 regex")
    })
}

fn strip_res() -> &'static [Regex; 4] {
    static RES: OnceLock<[Regex; 4]> = OnceLock::new();
    RES.get_or_init(|| {
        [
            Regex::new(r"(?-u)\x1b\][^\x07\x1b]*(?:\x07|\x1b\\)?").expect("valid OSC regex"),
            Regex::new(r"(?-u)\x1b\[[0-?]*[ -/]*[@-~]").expect("valid CSI regex"),
            Regex::new(r"(?-u)\x1b[@-Z\\-_]").expect("valid ESC regex"),
            Regex::new(r"(?-u)[\x00-\x08\x0b-\x1f\x7f]").expect("valid ctrl regex"),
        ]
    })
}

fn menu_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(?-u)>\s*1\.\s*Google\s+OAuth").expect("valid menu regex"))
}

fn is_url_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b"%&=+:/.?_~-".contains(&b)
}

fn trim_ascii_whitespace(slice: &[u8]) -> &[u8] {
    let is_ws = |b: &u8| matches!(b, b' ' | b'\t' | b'\n' | b'\r' | 0x0b | 0x0c);
    let start = slice.iter().position(|b| !is_ws(b)).unwrap_or(slice.len());
    let end = slice
        .iter()
        .rposition(|b| !is_ws(b))
        .map_or(start, |i| i + 1);
    &slice[start..end]
}

/// Remove ANSI/OSC/CSI/control sequences from raw PTY output (`signin_driver.py:strip_controls`).
pub fn strip_controls(data: &[u8]) -> Vec<u8> {
    let mut cur = data.to_vec();
    for re in strip_res() {
        cur = re.replace_all(&cur, &b""[..]).into_owned();
    }
    cur
}

/// Extract `<scheme>://<netloc>` from a URL byte slice (`signin_driver.py:url_origin`).
pub fn url_origin(url: &[u8]) -> Vec<u8> {
    let Some(sep) = url.windows(3).position(|w| w == b"://") else {
        return Vec::new();
    };
    let scheme = &url[..sep];
    let rest = &url[sep + 3..];
    let netloc_end = rest.iter().position(|&b| b == b'/').unwrap_or(rest.len());
    let mut out = Vec::with_capacity(sep + 3 + netloc_end);
    out.extend_from_slice(scheme);
    out.extend_from_slice(b"://");
    out.extend_from_slice(&rest[..netloc_end]);
    out
}

/// Whether stripped terminal output shows the `Select login method` menu with `> 1. Google OAuth`.
pub fn menu_ready(plain: &[u8]) -> bool {
    let needle = b"Select login method";
    plain.windows(needle.len()).any(|w| w == needle) && menu_re().is_match(plain)
}

/// Extract a complete authorization URL matching `origin` from buffered PTY output:
/// prefers OSC 8 hyperlinks, then wrapped plain-text URLs strictly across newline-ended lines
/// followed by at least one completed blank or non-URL line (`signin_driver.py:extract_url`).
pub fn extract_url(buf: &[u8], origin: &[u8]) -> Option<Vec<u8>> {
    if origin.is_empty() {
        return None;
    }
    if let Some(caps) = osc8_re().captures(buf)
        && let Some(m) = caps.get(1)
    {
        let url = m.as_bytes();
        if url_origin(url) == origin {
            return Some(url.to_vec());
        }
    }
    let stripped = strip_controls(buf);
    let mut all_lines: Vec<&[u8]> = stripped.split(|&b| b == b'\n').collect();
    if all_lines.len() <= 1 {
        return None;
    }
    all_lines.pop(); // drop trailing unterminated line: split(b"\n")[:-1]
    for (index, line) in all_lines.iter().enumerate() {
        let Some(at) = line.windows(origin.len()).position(|w| w == origin) else {
            continue;
        };
        let tail = &line[at..];
        let head_len = tail.iter().take_while(|&&b| is_url_char(b)).count();
        if head_len == 0 {
            continue;
        }
        let mut parts: Vec<&[u8]> = vec![&tail[..head_len]];
        for follow in &all_lines[index + 1..] {
            let chunk = trim_ascii_whitespace(follow);
            if chunk.is_empty() || !chunk.iter().all(|&b| is_url_char(b)) {
                break;
            }
            parts.push(chunk);
        }
        if index + parts.len() >= all_lines.len() {
            continue;
        }
        let mut url = Vec::new();
        for part in parts {
            url.extend_from_slice(part);
        }
        if url_origin(&url) == origin {
            return Some(url);
        }
    }
    None
}

fn token_finished(token_path: &std::path::Path) -> bool {
    matches!(
        masked_credential_state(token_path),
        MaskedCredentialState::Complete | MaskedCredentialState::Unsafe
    )
}

#[cfg(unix)]
struct DriverKiller {
    pid: u32,
    exited: Arc<AtomicBool>,
    stop_requested: Arc<AtomicBool>,
}

#[cfg(unix)]
impl Killer for DriverKiller {
    fn kill(&self) {
        self.stop_requested.store(true, Ordering::SeqCst);
        if self.exited.load(Ordering::SeqCst) {
            return;
        }
        let Ok(raw_pid) = i32::try_from(self.pid) else {
            return;
        };
        let group = nix::unistd::Pid::from_raw(raw_pid);
        let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGTERM);
        let exited = self.exited.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(KILL_GRACE_MS));
            if !exited.load(Ordering::SeqCst) {
                let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
            }
        });
    }
}

#[cfg(unix)]
fn stop_child_group(child: &mut std::process::Child, pid: u32) {
    let Ok(raw_pid) = i32::try_from(pid) else {
        return;
    };
    let group = nix::unistd::Pid::from_raw(raw_pid);
    if nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGTERM).is_err() {
        return;
    }
    let until = std::time::Instant::now() + std::time::Duration::from_millis(STOP_GRACE_MS);
    while std::time::Instant::now() < until {
        if let Ok(Some(_)) = child.try_wait() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = nix::sys::signal::killpg(group, nix::sys::signal::Signal::SIGKILL);
}

/// Spawn the Antigravity sign-in child on a native Rust PTY without invoking Python.
#[cfg(unix)]
pub fn spawn(command: &Command, driver: &DriverSpec) -> Handle {
    use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
    use nix::pty::{Winsize, openpty};
    use nix::sys::termios::{LocalFlags, SetArg, tcgetattr, tcsetattr};
    use std::io::{Read, Write};
    use std::os::fd::{AsFd, AsRawFd};
    use std::os::unix::net::UnixStream;
    use std::os::unix::process::CommandExt;
    use std::process::Stdio;

    let (event_tx, events) = mpsc::unbounded_channel();
    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<String>();

    let winsize = Winsize {
        ws_row: 40,
        ws_col: 160,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let Ok(pty_fds) = openpty(Some(&winsize), None) else {
        let _ = event_tx.send(ProcessEvent::Exit(None, Some(SpawnFailure::Failed)));
        return crate::signin_session::process::unspawned_handle(events, input_tx);
    };
    let master = pty_fds.master;
    let slave = pty_fds.slave;

    if let Ok(mut attrs) = tcgetattr(&slave) {
        attrs.local_flags.remove(LocalFlags::ECHO);
        let _ = tcsetattr(&slave, SetArg::TCSANOW, &attrs);
    }

    let Ok(slave_out) = slave.try_clone() else {
        let _ = event_tx.send(ProcessEvent::Exit(None, Some(SpawnFailure::Failed)));
        return crate::signin_session::process::unspawned_handle(events, input_tx);
    };
    let Ok(slave_err) = slave.try_clone() else {
        let _ = event_tx.send(ProcessEvent::Exit(None, Some(SpawnFailure::Failed)));
        return crate::signin_session::process::unspawned_handle(events, input_tx);
    };

    let Ok((mut ctrl_read, mut ctrl_write)) = UnixStream::pair() else {
        let _ = event_tx.send(ProcessEvent::Exit(None, Some(SpawnFailure::Failed)));
        return crate::signin_session::process::unspawned_handle(events, input_tx);
    };
    let _ = ctrl_read.set_nonblocking(true);

    let master_raw = master.as_raw_fd();
    let mut build = std::process::Command::new(&command.file);
    build.args(&command.args);
    build.env_clear();
    build.envs(&command.env);
    build
        .stdin(Stdio::from(slave))
        .stdout(Stdio::from(slave_out))
        .stderr(Stdio::from(slave_err));

    #[allow(unsafe_code)]
    // SAFETY: pre_exec invokes only async-signal-safe POSIX syscalls (setsid, ioctl TIOCSCTTY,
    // close) to detach into a new session with the PTY slave as controlling terminal and prevent
    // the child from inheriting the PTY master descriptor.
    unsafe {
        build.pre_exec(move || {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            let _ = libc::ioctl(0, libc::TIOCSCTTY as _, 0);
            let _ = libc::close(master_raw);
            Ok(())
        });
    }

    let mut child = match build.spawn() {
        Ok(child) => child,
        Err(error) => {
            let failure = if error.kind() == std::io::ErrorKind::NotFound {
                SpawnFailure::Missing
            } else {
                SpawnFailure::Failed
            };
            let _ = event_tx.send(ProcessEvent::Exit(None, Some(failure)));
            return crate::signin_session::process::unspawned_handle(events, input_tx);
        }
    };
    // Drop command so any remaining slave handles are closed in the parent.
    drop(build);

    let pid = child.id();
    let exited = Arc::new(AtomicBool::new(false));
    let stop_requested = Arc::new(AtomicBool::new(false));
    let killer = Arc::new(DriverKiller {
        pid,
        exited: exited.clone(),
        stop_requested: stop_requested.clone(),
    });

    // Forward async Handle::write inputs to the driver loop's control socket with \n -> \r.
    tokio::spawn(async move {
        while let Some(text) = input_rx.recv().await {
            let mapped = text.replace('\n', "\r");
            if ctrl_write.write_all(mapped.as_bytes()).is_err() {
                break;
            }
        }
        drop(ctrl_write);
    });

    let token_path = driver.token_path.clone();
    let origin_bytes = driver.origin.as_bytes().to_vec();
    let exited_worker = exited.clone();
    let _ = std::thread::Builder::new()
        .name("aac-agy-driver".into())
        .spawn(move || {
            let mut master_file = std::fs::File::from(master);
            let mut buf: Vec<u8> = Vec::new();
            let mut entered = false;
            let mut url_sent = false;
            let mut stable = 0u32;
            let mut ctrl_open = true;
            let mut chunk = vec![0u8; 65_536];

            loop {
                let mut fds = Vec::with_capacity(2);
                fds.push(PollFd::new(master_file.as_fd(), PollFlags::POLLIN));
                if ctrl_open {
                    fds.push(PollFd::new(ctrl_read.as_fd(), PollFlags::POLLIN));
                }
                let ready = match poll(&mut fds, PollTimeout::from(POLL_MS)) {
                    Ok(n) => n,
                    Err(nix::errno::Errno::EINTR) => continue,
                    Err(_) => break,
                };
                let master_revents = fds[0].revents().unwrap_or(PollFlags::empty());
                let ctrl_revents = if ctrl_open {
                    fds[1].revents().unwrap_or(PollFlags::empty())
                } else {
                    PollFlags::empty()
                };

                if ready > 0 && master_revents.intersects(PollFlags::POLLIN | PollFlags::POLLHUP) {
                    let read_len = match master_file.read(&mut chunk) {
                        Ok(n) => n,
                        Err(e) if e.raw_os_error() == Some(libc::EIO) => 0,
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => 0,
                    };
                    if read_len == 0 {
                        break;
                    }
                    if !url_sent {
                        buf.extend_from_slice(&chunk[..read_len]);
                        if buf.len() > MAX_BUFFER {
                            let drop_n = buf.len() - MAX_BUFFER;
                            buf.drain(..drop_n);
                        }
                        let plain = strip_controls(&buf);
                        if !entered && menu_ready(&plain) {
                            let _ = master_file.write_all(b"\r");
                            entered = true;
                        }
                        if let Some(url) = extract_url(&buf, &origin_bytes) {
                            let mut line = String::from_utf8_lossy(&url).into_owned();
                            line.push('\n');
                            let _ = event_tx.send(ProcessEvent::Data(line));
                            url_sent = true;
                        }
                    }
                }

                if ctrl_open
                    && ctrl_revents
                        .intersects(PollFlags::POLLIN | PollFlags::POLLHUP | PollFlags::POLLERR)
                {
                    match ctrl_read.read(&mut chunk) {
                        Ok(0) => ctrl_open = false,
                        Ok(n) => {
                            let _ = master_file.write_all(&chunk[..n]);
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                        Err(_) => ctrl_open = false,
                    }
                }

                if token_finished(&token_path) {
                    stable += 1;
                    if stable >= 2 {
                        stop_child_group(&mut child, pid);
                        break;
                    }
                } else {
                    stable = 0;
                }

                if stop_requested.load(Ordering::SeqCst)
                    && child.try_wait().ok().flatten().is_some()
                {
                    break;
                }
            }

            let _ = child.wait();
            exited_worker.store(true, Ordering::SeqCst);
            let code = if token_finished(&token_path) { 0 } else { 3 };
            let _ = event_tx.send(ProcessEvent::Exit(Some(code), None));
        });

    crate::signin_session::process::spawned_handle(events, input_tx, killer, Some(pid))
}
