use super::*;
use fs2::FileExt;
use futures_util::{SinkExt, StreamExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
    process::Stdio,
    time::{Duration, Instant},
};
use tokio::{io::AsyncReadExt, net::UnixStream};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::WebSocketConfig},
};

/// Linux process operations for the Codex update runtime.
/// Construct only for an explicitly authorized update job, never for collection.
pub struct Native {
    home: PathBuf,
    executable: PathBuf,
    locks: Vec<File>,
    native_lock: Option<PathBuf>,
    epoch: Instant,
}
impl Native {
    /// Use this Codex home and updated CLI executable for relaunches.
    pub fn new(home: PathBuf, executable: PathBuf) -> Self {
        Self {
            home,
            executable,
            locks: vec![],
            native_lock: None,
            epoch: Instant::now(),
        }
    }
    fn socket(&self) -> PathBuf {
        self.home.join("app-server-control/app-server-control.sock")
    }
    async fn acquire(&mut self, path: &Path) -> Result<()> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(path)
            .map_err(|_| Error::RestartFailed)?;
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match file.try_lock_exclusive() {
                Ok(()) => {
                    self.locks.push(file);
                    return Ok(());
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => return Err(Error::RestartFailed),
            }
            if Instant::now() >= deadline {
                return Err(Error::Busy);
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}
impl Backend for Native {
    async fn scan(&mut self) -> Result<Vec<Process>> {
        read_processes()
    }
    async fn lock(&mut self) -> Result<()> {
        if !self.locks.is_empty() {
            return Ok(());
        }
        let directory = self.home.join("app-server-control");
        fs::create_dir_all(&directory).map_err(|_| Error::RestartFailed)?;
        self.acquire(&directory.join("app-server-startup.lock"))
            .await?;
        let result = async {
            if self.native_lock.is_none() {
                match fs::symlink_metadata(self.socket()) {
                    Ok(m) if m.file_type().is_symlink() => {
                        let target =
                            fs::canonicalize(self.socket()).map_err(|_| Error::RestartFailed)?;
                        self.native_lock =
                            Some(PathBuf::from(format!("{}.lock", target.display())));
                    }
                    Ok(_) => {}
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(Error::RestartFailed),
                }
            }
            if let Some(path) = self.native_lock.clone() {
                self.acquire(&path).await?;
            }
            Ok(())
        }
        .await;
        if result.is_err() {
            self.locks.clear();
        }
        result
    }
    async fn release(&mut self) {
        self.locks.clear();
    }
    async fn assert_idle(&mut self, all: &[Process]) -> Result<()> {
        for p in all.iter().filter(|p| p.daemon() && p.on_home(&self.home)) {
            daemon_idle(p, &self.home).await?;
        }
        for p in all.iter().filter(|p| {
            p.desktop()
                && descendants(p, all)
                    .iter()
                    .any(|n| n.codex() && n.on_home(&self.home) && !n.proxy())
        }) {
            desktop_idle(p, all, &self.home)?;
        }
        Ok(())
    }
    async fn signal(&mut self, p: &Process, kill: bool) -> Result<()> {
        use nix::{
            sys::signal::{Signal, kill as send},
            unistd::Pid,
        };
        let current = read_processes()?
            .into_iter()
            .find(|n| n.same(p) && n.alive());
        if let Some(n) = current {
            if n.fingerprint() != p.fingerprint() {
                return Err(Error::RestartFailed);
            }
            match send(
                Pid::from_raw(i32::try_from(p.pid).map_err(|_| Error::RestartFailed)?),
                if kill {
                    Signal::SIGKILL
                } else {
                    Signal::SIGTERM
                },
            ) {
                Ok(()) | Err(nix::errno::Errno::ESRCH) => {}
                Err(_) => return Err(Error::RestartFailed),
            }
        }
        Ok(())
    }
    async fn launch(&mut self, p: &Process, desktop: bool) -> Result<Launch> {
        let exe = if desktop {
            PathBuf::from(&p.exe)
        } else {
            self.executable.clone()
        };
        let env: Vec<_> = p
            .env
            .iter()
            .filter(|(k, _)| {
                let k = k.to_ascii_uppercase();
                k != "CCS_SESSION_SECRET"
                    && !k.starts_with("CCS_DASHBOARD_")
                    && !k.ends_with("_PASSWORD_HASH")
            })
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let log = if desktop {
            None
        } else {
            let path = self.home.join("app-server-control/app-server.log");
            prepare_log(&path)?;
            Some(path.to_string_lossy().into_owned())
        };
        let spec = crate::launch::LaunchSpec {
            app: if desktop {
                "codex-desktop"
            } else {
                "codex-app-server"
            }
            .into(),
            exe: exe.to_string_lossy().into_owned(),
            args: p.args.iter().skip(1).cloned().collect(),
            cwd: Some(p.cwd.clone()),
            env: Some(env),
            log_file: log,
        };
        if !inside_service_cgroup(&fs::read_to_string("/proc/self/cgroup").unwrap_or_default()) {
            let mut command = tokio::process::Command::new(&spec.exe);
            command
                .args(&spec.args)
                .current_dir(&p.cwd)
                .env_clear()
                .envs(
                    spec.env
                        .as_ref()
                        .expect("captured environment")
                        .iter()
                        .map(|(k, v)| (k, v)),
                )
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .process_group(0);
            let child = command.spawn().map_err(|_| Error::RestartFailed)?;
            let identity = child.id().and_then(|pid| {
                fs::read_to_string(format!("/proc/{pid}/stat"))
                    .ok()
                    .and_then(|stat| {
                        stat.rfind(')').and_then(|at| {
                            stat[at + 2..].split(' ').nth(19).map(|s| Identity {
                                pid,
                                start_time: s.into(),
                            })
                        })
                    })
            });
            return Ok(Launch {
                identity,
                unit: None,
            });
        }
        let unit = format!(
            "aac-launch-{}-{}-{}",
            spec.app,
            std::process::id(),
            self.epoch.elapsed().as_nanos()
        );
        let args = crate::launch::build_systemd_run_args(&spec, &unit);
        let mut command = tokio::process::Command::new("/usr/bin/systemd-run");
        command
            .args(args)
            .env_clear()
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(env) = crate::launch::systemd_run_environment(
            &spec,
            std::env::var("XDG_RUNTIME_DIR").ok().as_deref(),
        ) {
            command.envs(env);
        }
        let status = tokio::time::timeout(Duration::from_secs(10), command.status())
            .await
            .map_err(|_| Error::RestartFailed)?
            .map_err(|_| Error::RestartFailed)?;
        if !status.success() {
            return Err(Error::RestartFailed);
        }
        Ok(Launch {
            identity: None,
            unit: Some(unit),
        })
    }
    async fn verify_daemon(&mut self, p: &Process) -> Result<()> {
        let end = Instant::now() + Duration::from_secs(30);
        loop {
            let mut command = tokio::process::Command::new(&self.executable);
            command
                .args(["app-server", "daemon", "version"])
                .current_dir(&p.cwd)
                .env_clear()
                .envs(&p.env)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .kill_on_drop(true);
            let check = async {
                let mut child = command.spawn().map_err(|_| Error::RestartFailed)?;
                let mut out = vec![];
                child
                    .stdout
                    .take()
                    .ok_or(Error::RestartFailed)?
                    .take(1_048_577)
                    .read_to_end(&mut out)
                    .await
                    .map_err(|_| Error::RestartFailed)?;
                let status = child.wait().await.map_err(|_| Error::RestartFailed)?;
                if out.len() > 1_048_576 || !status.success() {
                    return Err(Error::RestartFailed);
                }
                let v: serde_json::Value =
                    serde_json::from_slice(&out).map_err(|_| Error::RestartFailed)?;
                if v["status"] == "running" {
                    Ok(())
                } else {
                    Err(Error::RestartFailed)
                }
            };
            let remaining = end.saturating_duration_since(Instant::now());
            if tokio::time::timeout(remaining.min(Duration::from_secs(10)), check)
                .await
                .is_ok_and(|v| v.is_ok())
            {
                return Ok(());
            }
            if Instant::now() >= end {
                return Err(Error::RestartFailed);
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }
    async fn remove_socket(&mut self) -> Result<()> {
        let path = self.socket();
        let target = match fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => fs::read_link(&path)
                .map(|p| {
                    if p.is_absolute() {
                        p
                    } else {
                        path.parent().expect("socket parent").join(p)
                    }
                })
                .map_err(|_| Error::RestartFailed)?,
            Ok(_) => path,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(Error::RestartFailed),
        };
        match fs::symlink_metadata(&target) {
            Ok(m) if m.file_type().is_socket() => {
                fs::remove_file(target).map_err(|_| Error::RestartFailed)
            }
            Ok(_) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(Error::RestartFailed),
        }
    }
    fn now(&self) -> u64 {
        self.epoch
            .elapsed()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
    async fn sleep(&mut self, d: Duration) {
        tokio::time::sleep(d).await;
    }
}
fn prepare_log(path: &Path) -> Result<()> {
    fs::create_dir_all(path.parent().ok_or(Error::RestartFailed)?)
        .map_err(|_| Error::RestartFailed)?;
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.len() > 5 * 1024 * 1024) {
        let _ = fs::rename(path, path.with_extension("log.1"));
    }
    OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .map_err(|_| Error::RestartFailed)?;
    Ok(())
}
/// Same-user Linux inventory. Errors on unreadable relevant context; never returns partial success.
pub fn read_processes() -> Result<Vec<Process>> {
    let mut all = vec![];
    for entry in fs::read_dir("/proc").map_err(|_| Error::RestartFailed)? {
        let entry = entry.map_err(|_| Error::RestartFailed)?;
        let name = entry.file_name();
        let Some(pid) = name.to_str().and_then(|s| s.parse::<u32>().ok()) else {
            continue;
        };
        let path = entry.path();
        let capture = || -> std::io::Result<Option<Process>> {
            if fs::metadata(&path)?.uid() != nix::unistd::getuid().as_raw() {
                return Ok(None);
            }
            let stat = fs::read_to_string(path.join("stat"))?;
            let fields: Vec<_> = stat[stat
                .rfind(')')
                .ok_or_else(|| std::io::Error::other("stat"))?
                + 2..]
                .split(' ')
                .collect();
            if fields.len() < 20 {
                return Err(std::io::Error::other("stat"));
            }
            let args: Vec<String> = fs::read_to_string(path.join("cmdline"))?
                .split('\0')
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect();
            let exe = match fs::read_link(path.join("exe")) {
                Ok(p) => p.to_string_lossy().trim_end_matches(" (deleted)").into(),
                Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                    args.first().cloned().unwrap_or_default()
                }
                Err(e) => return Err(e),
            };
            let relevant = Path::new(&exe).file_name().is_some_and(|n| n == "codex")
                || exe == "/usr/lib/chatgpt/ChatGPT";
            let mut env = BTreeMap::new();
            if relevant {
                let mut raw = String::new();
                File::open(path.join("environ"))?
                    .take(1_048_577)
                    .read_to_string(&mut raw)?;
                if raw.len() > 1_048_576 {
                    return Err(std::io::Error::other("context"));
                }
                for item in raw.split('\0') {
                    if let Some((k, v)) = item.split_once('=')
                        && !k.is_empty()
                    {
                        env.insert(k.into(), v.into());
                    }
                }
            }
            let cwd = if relevant {
                fs::read_link(path.join("cwd"))?
                    .to_string_lossy()
                    .into_owned()
            } else {
                String::new()
            };
            let unit = fs::read_to_string(path.join("cgroup"))
                .ok()
                .and_then(|s| systemd_unit_of(&s));
            Ok(Some(Process {
                pid,
                ppid: fields[1]
                    .parse()
                    .map_err(|_| std::io::Error::other("stat"))?,
                start_time: fields[19].into(),
                state: fields[0].into(),
                args,
                exe,
                cwd,
                env,
                unit,
            }))
        };
        match capture() {
            Ok(Some(p)) => all.push(p),
            Ok(None) => {}
            Err(e)
                if e.kind() == std::io::ErrorKind::NotFound
                    || e.raw_os_error() == Some(libc::ESRCH) => {}
            Err(_) => return Err(Error::RestartFailed),
        }
    }
    Ok(all)
}

async fn rpc(
    ws: &mut WebSocketStream<UnixStream>,
    id: u64,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value> {
    let call = async {
        ws.send(Message::Text(
            serde_json::json!({"id":id,"method":method,"params":params})
                .to_string()
                .into(),
        ))
        .await
        .map_err(|_| Error::Busy)?;
        while let Some(message) = ws.next().await {
            let message = message.map_err(|_| Error::Busy)?;
            if message.is_close() {
                return Err(Error::Busy);
            }
            if let Message::Ping(bytes) = message {
                ws.send(Message::Pong(bytes))
                    .await
                    .map_err(|_| Error::Busy)?;
                continue;
            }
            if !message.is_text() {
                continue;
            }
            let Ok(v) = serde_json::from_str::<serde_json::Value>(
                message.to_text().map_err(|_| Error::Busy)?,
            ) else {
                continue;
            };
            if v["id"].as_u64() == Some(id) {
                if !v["error"].is_null() {
                    return Err(Error::Busy);
                }
                return v.get("result").cloned().ok_or(Error::Busy);
            }
        }
        Err(Error::Busy)
    };
    tokio::time::timeout(Duration::from_secs(5), call)
        .await
        .map_err(|_| Error::Busy)?
}
async fn daemon_idle(p: &Process, home: &Path) -> Result<()> {
    let index = p
        .args
        .iter()
        .position(|s| s == "--listen")
        .ok_or(Error::Busy)?;
    let endpoint = p.args.get(index + 1).ok_or(Error::Busy)?;
    let socket = if endpoint == "unix://" {
        home.join("app-server-control/app-server-control.sock")
    } else {
        PathBuf::from(endpoint.strip_prefix("unix://").ok_or(Error::Busy)?)
    };
    let connect = async {
        let stream = UnixStream::connect(socket).await.map_err(|_| Error::Busy)?;
        tokio_tungstenite::client_async_with_config(
            "ws://localhost/",
            stream,
            Some(
                WebSocketConfig::default()
                    .max_message_size(Some(1024 * 1024))
                    .max_frame_size(Some(1024 * 1024)),
            ),
        )
        .await
        .map(|v| v.0)
        .map_err(|_| Error::Busy)
    };
    let mut ws = tokio::time::timeout(Duration::from_secs(5), connect)
        .await
        .map_err(|_| Error::Busy)??;
    rpc(&mut ws,1,"initialize",serde_json::json!({"clientInfo":{"name":"ccs-activation","version":"1.0.0"},"capabilities":{"experimentalApi":true}})).await?;
    ws.send(Message::Text(
        serde_json::json!({"method":"initialized","params":{}})
            .to_string()
            .into(),
    ))
    .await
    .map_err(|_| Error::Busy)?;
    let mut cursor: Option<String> = None;
    let mut seen = BTreeSet::new();
    let mut id = 1;
    loop {
        id += 1;
        let mut params = serde_json::json!({"limit":100});
        if let Some(c) = &cursor {
            params["cursor"] = serde_json::Value::String(c.clone());
        }
        let loaded = rpc(&mut ws, id, "thread/loaded/list", params).await?;
        for thread in loaded["data"].as_array().ok_or(Error::Busy)? {
            let thread = thread.as_str().ok_or(Error::Busy)?;
            id += 1;
            let read = rpc(
                &mut ws,
                id,
                "thread/read",
                serde_json::json!({"threadId":thread,"includeTurns":false}),
            )
            .await?;
            if !matches!(
                read["thread"]["status"]["type"].as_str(),
                Some("idle" | "notLoaded" | "systemError")
            ) {
                return Err(Error::Busy);
            }
        }
        cursor = loaded["nextCursor"]
            .as_str()
            .filter(|s| !s.is_empty())
            .map(str::to_owned);
        let Some(c) = &cursor else {
            break;
        };
        if !seen.insert(c.clone()) {
            return Err(Error::Busy);
        }
    }
    Ok(())
}
/// Last recognized rollout lifecycle event, scanning backwards without loading the rollout.
pub fn rollout_lifecycle(file: &mut File) -> Result<&'static str> {
    let mut position = file.metadata().map_err(|_| Error::Busy)?.len();
    let mut carry = vec![];
    while position > 0 {
        let len = position.min(65_536) as usize;
        position -= len as u64;
        file.seek(SeekFrom::Start(position))
            .map_err(|_| Error::Busy)?;
        let mut bytes = vec![0; len];
        file.read_exact(&mut bytes).map_err(|_| Error::Busy)?;
        bytes.extend_from_slice(&carry);
        let mut lines: Vec<_> = bytes.split(|b| *b == b'\n').collect();
        carry = if position > 0 {
            lines.remove(0).to_vec()
        } else {
            vec![]
        };
        for line in lines.into_iter().rev() {
            let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
                continue;
            };
            if v["type"] != "event_msg" {
                continue;
            }
            match v["payload"]["type"].as_str() {
                Some("task_complete" | "task_aborted" | "turn_aborted") => return Ok("idle"),
                Some("task_started") => return Ok("busy"),
                _ => {}
            }
        }
        if carry.len() > 1024 * 1024 {
            return Ok("unknown");
        }
    }
    Ok("unknown")
}
fn desktop_idle(p: &Process, all: &[Process], home: &Path) -> Result<()> {
    let servers: Vec<_> = descendants(p, all)
        .into_iter()
        .filter(|p| p.codex() && p.args.iter().any(|s| s == "app-server") && !p.proxy())
        .collect();
    if servers.is_empty() {
        return Err(Error::Busy);
    }
    let roots: Vec<_> = ["sessions", "archived_sessions"]
        .map(|s| {
            let p = home.join(s);
            fs::canonicalize(&p).unwrap_or(p)
        })
        .into();
    for server in servers {
        let directory = PathBuf::from(format!("/proc/{}/fd", server.pid));
        for fd in fs::read_dir(directory).map_err(|_| Error::Busy)? {
            let fd = fd.map_err(|_| Error::Busy)?;
            let target = match fs::read_link(fd.path()) {
                Ok(p) => PathBuf::from(p.to_string_lossy().trim_end_matches(" (deleted)")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return Err(Error::Busy),
            };
            if roots.iter().any(|r| target.starts_with(r))
                && target.extension().is_some_and(|s| s == "jsonl")
                && rollout_lifecycle(&mut File::open(fd.path()).map_err(|_| Error::Busy)?)?
                    != "idle"
            {
                return Err(Error::Busy);
            }
        }
    }
    Ok(())
}

fn inside_service_cgroup(text: &str) -> bool {
    text.lines().any(|line| {
        let parts: Vec<_> = line.trim().split('/').collect();
        parts.iter().enumerate().any(|(i, p)| {
            p.strip_prefix("user@")
                .and_then(|s| s.strip_suffix(".service"))
                .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
                && parts.iter().skip(i + 1).any(|s| s.ends_with(".service"))
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn daemon_idle_rpc_rejects_busy_unknown_and_repeating_pages() {
        for (status, repeat, expected) in [
            ("idle", false, Ok(())),
            ("active", false, Err(Error::Busy)),
            ("missing", false, Err(Error::Busy)),
            ("idle", true, Err(Error::Busy)),
        ] {
            let dir = tempfile::tempdir().expect("dir");
            let socket = dir.path().join("socket");
            let listener = tokio::net::UnixListener::bind(&socket).expect("listen");
            let server = tokio::spawn(async move {
                let (stream, _) = listener.accept().await.expect("accept");
                let mut ws = tokio_tungstenite::accept_async(stream)
                    .await
                    .expect("websocket");
                while let Some(Ok(Message::Text(text))) = ws.next().await {
                    let v: serde_json::Value = serde_json::from_str(&text).expect("request");
                    let result = match v["method"].as_str().expect("method") {
                        "initialized" => continue,
                        "initialize" => serde_json::json!({}),
                        "thread/loaded/list" => {
                            serde_json::json!({"data":["fixture"],"nextCursor":if repeat{Some("again")}else{None}})
                        }
                        "thread/read" => {
                            assert_eq!(v["params"]["includeTurns"], false);
                            serde_json::json!({"thread":{"status":{"type":status}}})
                        }
                        _ => panic!("unexpected method"),
                    };
                    if ws
                        .send(Message::Text(
                            serde_json::json!({"id":v["id"],"result":result})
                                .to_string()
                                .into(),
                        ))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
            });
            let p = Process {
                args: vec![
                    "codex".into(),
                    "app-server".into(),
                    "--listen".into(),
                    format!("unix://{}", socket.display()),
                ],
                ..Default::default()
            };
            assert_eq!(daemon_idle(&p, dir.path()).await, expected);
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn startup_locks_cover_native_socket_and_release_on_drop() {
        let dir = tempfile::tempdir().expect("dir");
        let control = dir.path().join("app-server-control");
        fs::create_dir(&control).expect("control");
        let socket = dir.path().join("native.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("socket");
        std::os::unix::fs::symlink(&socket, control.join("app-server-control.sock")).expect("link");
        let mut n = Native::new(dir.path().into(), "/fixture/codex".into());
        n.lock().await.expect("locks");
        let native = File::open(dir.path().join("native.sock.lock")).expect("native lock");
        let legacy = File::open(control.join("app-server-startup.lock")).expect("legacy");
        assert!(native.try_lock_exclusive().is_err());
        assert!(legacy.try_lock_exclusive().is_err());
        drop(n);
        assert!(native.try_lock_exclusive().is_ok());
        assert!(legacy.try_lock_exclusive().is_ok());
    }
}
