//! Shared bounded command runner, owned Linux socket probe and sign-in preflight checks.
//!
//! Nothing here starts, stops or changes a service, and no provider is reached.

use std::io::Read;
use std::path::Path;

/// Every probe's stdout/line cap.
pub const OUTPUT_LIMIT: usize = 64 * 1024;

/// Bounded command runner: `Command`, scrubbed Python environment, 64 KiB stdout, a deadline.
pub fn bounded_run(file: &str, args: &[&str], timeout_ms: u64) -> Option<String> {
    use std::process::{Command, Stdio};
    let mut child = Command::new(file)
        .args(args)
        .env_remove("PYTHONPATH")
        .env_remove("PYTHONHOME")
        .env_remove("PYTHONSTARTUP")
        .env_remove("PYTHONUSERBASE")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut body = Vec::new();
        let mut chunk = [0u8; 8192];
        loop {
            match stdout.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    body.extend_from_slice(&chunk[..n]);
                    if body.len() > OUTPUT_LIMIT {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
        body
    });
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let body = reader.join().ok()?;
    if !status?.success() || body.len() > OUTPUT_LIMIT {
        return None;
    }
    String::from_utf8(body).ok()
}

/// Owned socket connect state: only absence or a refused connect is `Stopped`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SocketState {
    /// Nothing can be listening: the socket or its folder is absent, or a leftover file refused.
    Stopped,
    /// A listener accepted the connection.
    Listening,
    /// Unsafe or unclear: never reported as stopped.
    Unknown,
}

/// Check an owned Linux Unix domain socket with a bounded connect probe.
#[cfg(target_os = "linux")]
pub fn socket_state(socket_path: &Path, timeout_ms: u64) -> SocketState {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let uid = aac_files::path::uid();
    let directory = std::fs::symlink_metadata(socket_path.parent().unwrap_or(Path::new(".")));
    match directory {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SocketState::Stopped,
        Ok(meta) if meta.is_dir() && meta.uid() == uid => {}
        _ => return SocketState::Unknown,
    }
    match std::fs::symlink_metadata(socket_path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return SocketState::Stopped,
        Ok(meta) if meta.file_type().is_socket() && meta.uid() == uid => {}
        _ => return SocketState::Unknown,
    }
    use std::sync::mpsc;
    let path = socket_path.to_path_buf();
    let (sender, receiver) = mpsc::channel();
    std::thread::spawn(move || {
        let result = std::os::unix::net::UnixStream::connect(&path);
        let _ = sender.send(result);
    });
    match receiver.recv_timeout(std::time::Duration::from_millis(timeout_ms)) {
        Ok(Ok(stream)) => {
            drop(stream);
            SocketState::Listening
        }
        Ok(Err(error)) => {
            if error.raw_os_error() == Some(libc::ECONNREFUSED) {
                SocketState::Stopped
            } else if error.kind() == std::io::ErrorKind::NotFound {
                // ENOENT is proof only while the file is really gone.
                if std::fs::symlink_metadata(socket_path).is_err() {
                    SocketState::Stopped
                } else {
                    SocketState::Unknown
                }
            } else {
                SocketState::Unknown
            }
        }
        Err(_) => SocketState::Unknown,
    }
}

/// Return `Unknown` outside Linux.
#[cfg(not(target_os = "linux"))]
pub fn socket_state(_socket_path: &Path, _timeout_ms: u64) -> SocketState {
    SocketState::Unknown
}

/// `nativeBinaryProblem(home, uid)`: false when `~/.local/bin/agy` is a safe owned executable.
fn native_binary_problem(home: &Path, uid: Option<u32>) -> bool {
    let _ = uid;
    let binary = home.join(".local").join("bin").join("agy");
    let Ok(meta) = std::fs::symlink_metadata(&binary) else {
        return true;
    };
    if !meta.is_file() || meta.file_type().is_symlink() {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if uid.is_some_and(|uid| meta.uid() != uid) {
            return true;
        }
        if meta.mode() & 0o022 != 0 || meta.mode() & 0o100 == 0 {
            return true;
        }
    }
    false
}

/// Owned-binary check for Antigravity sign-in preflight.
pub(crate) fn native_binary_problem_public(home: &Path, uid: Option<u32>) -> bool {
    native_binary_problem(home, uid)
}

pub(crate) fn current_uid() -> Option<u32> {
    #[cfg(unix)]
    {
        Some(aac_files::path::uid())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn socket_probe_distinguishes_listening_and_stopped() {
        let temp = tempfile::tempdir().unwrap();
        let socket = temp.path().join("daemon.sock");
        assert_eq!(socket_state(&socket, 500), SocketState::Stopped);
        let _server = std::os::unix::net::UnixListener::bind(&socket).unwrap();
        assert_eq!(socket_state(&socket, 500), SocketState::Listening);
        std::fs::remove_file(&socket).unwrap();
        assert_eq!(socket_state(&socket, 500), SocketState::Stopped);
    }
}
