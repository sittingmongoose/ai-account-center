//! R3 store host checks on the real binary and the real Unix socket; all stores are
//! fixtures in temporary folders. Each fails against a service without the store host
//! (no `/internal/store/<method>` route, no engine behind `/api/store/status`).
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::PermissionsExt,
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

const SECRET: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const BOOT_ID: &str = "abcdefabcdefabcdefabcdef";

fn private(path: &Path) -> Result<()> {
    fs::create_dir_all(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

fn principal(kind: &str, id: &str, role: &str) -> String {
    json!({"kind":kind,"id":id,"role":role}).to_string()
}

fn front_door() -> String {
    principal("session", "front-door", "service")
}

fn admin() -> String {
    principal("session", "fixture-admin", "admin")
}

struct Reply {
    status: u16,
    body: String,
}
impl Reply {
    fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.body)?)
    }
}

fn parse(raw: &str) -> Result<Reply> {
    let (head, body) = raw.split_once("\r\n\r\n").ok_or("no head")?;
    let status = head
        .lines()
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .ok_or("no status")?
        .parse()?;
    Ok(Reply {
        status,
        body: body.to_owned(),
    })
}

struct Service {
    child: Child,
    socket: PathBuf,
    root: PathBuf,
    _keep: tempfile::TempDir,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Service {
    fn start(store: bool) -> Result<Self> {
        // Under gate load a child can die before its first accept (runtime start, boot
        // read): start over within the same deadline instead of failing the contract.
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            match Self::try_start(store, deadline) {
                Ok(service) => return Ok(service),
                Err(error) => {
                    if Instant::now() >= deadline {
                        return Err(error);
                    }
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }

    fn try_start(store: bool, deadline: Instant) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let root = fs::canonicalize(dir.path())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let run = root.join("run");
        let boot_dir = run.join(format!("aac-http-{BOOT_ID}"));
        private(&run)?;
        private(&boot_dir)?;
        let socket = boot_dir.join("http.sock");
        let mut child = Command::new(env!("CARGO_BIN_EXE_aac-http"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear()
            .spawn()?;
        let ready = (|| -> Result<()> {
            let mut boot =
                json!({"ccsDir":root,"socketPath":socket,"secret":SECRET,"bootId":BOOT_ID});
            if store {
                boot["store"] = json!({
                    "dir": root.join("store"),
                    "retentionDays": 0,
                    "bindingIdentity": aac_store::build_identity(),
                    "snapshotDirectory": null,
                    "shareBackupDir": null,
                    "shareBackupOff": false,
                });
            }
            writeln!(child.stdin.as_mut().ok_or("stdin missing")?, "{boot}")?;
            loop {
                if child.try_wait()?.is_some() {
                    return Err("the service exited before it was ready".into());
                }
                if socket.exists()
                    && exchange(
                        &socket,
                        "GET",
                        "/api/health",
                        &principal("session", "public-health", "public"),
                        None,
                    )
                    .is_ok_and(|r| r.status == 200)
                {
                    return Ok(());
                }
                if Instant::now() >= deadline {
                    return Err("service failed to start".into());
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        })();
        if let Err(error) = ready {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
        Ok(Self {
            child,
            socket,
            root,
            _keep: dir,
        })
    }

    fn store_dir(&self) -> PathBuf {
        self.root.join("store")
    }

    fn call(&self, method: &str, path: &str, who: &str, body: Option<&[u8]>) -> Result<Reply> {
        exchange(&self.socket, method, path, who, body)
    }

    fn store(&self, method: &str, args: &str) -> Result<Reply> {
        self.call(
            "POST",
            &format!("/internal/store/{method}"),
            &front_door(),
            Some(args.as_bytes()),
        )
    }

    /// End the service as a front door that went away does: its stdin closes. The fixture
    /// folder stays (this guard holds it) for the caller's reopen checks.
    fn stop(&mut self) -> Result<()> {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(10);
        while self.child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                return Err("service did not end".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
    }
}

fn connect_with_retry(
    socket: &Path,
    first_failure: impl FnOnce(&io::Error) -> io::Result<()>,
) -> io::Result<UnixStream> {
    let connect_deadline = Instant::now() + Duration::from_secs(2);
    let mut first_failure = Some(first_failure);
    loop {
        match UnixStream::connect(socket) {
            Ok(stream) => return Ok(stream),
            Err(error) => {
                if Instant::now() >= connect_deadline {
                    return Err(error);
                }
                if let Some(notify) = first_failure.take() {
                    notify(&error)?;
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        }
    }
}

/// One request/response exchange on the fixture socket. A reply that arrives before the
/// body is written is still read: the service may answer and close early (the wave-56
/// BrokenPipe), and the contract assertions apply to that reply exactly as to any other.
fn exchange(
    socket: &Path,
    method: &str,
    path: &str,
    who: &str,
    body: Option<&[u8]>,
) -> Result<Reply> {
    let stream = connect_with_retry(socket, |_| Ok(()))?;
    exchange_on(stream, method, path, who, body)
}

fn exchange_on(
    mut stream: UnixStream,
    method: &str,
    path: &str,
    who: &str,
    body: Option<&[u8]>,
) -> Result<Reply> {
    stream.set_read_timeout(Some(Duration::from_secs(15)))?;
    let write = (|| -> io::Result<()> {
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {who}\r\n"
        )?;
        match body {
            Some(body) => {
                write!(
                    stream,
                    "content-type: application/json\r\ncontent-length: {}\r\n\r\n",
                    body.len()
                )?;
                stream.write_all(body)?;
            }
            None => write!(stream, "\r\n")?,
        }
        Ok(())
    })();
    let mut raw = String::new();
    match stream.read_to_string(&mut raw) {
        Ok(_) => parse(&raw),
        Err(read_error) => match write {
            Err(write_error) => Err(write_error.into()),
            Ok(()) => Err(read_error.into()),
        },
    }
}

fn observation(t: u64) -> String {
    json!({ "observations": [{
        "account": { "identity": "a".repeat(64), "provider": "codex", "account_id": "codex:first",
                     "plan": null, "platform": "ubuntu", "source": "host test", "status": "ok", "is_active": true },
        "sampled_at_ms": t, "observed_at_ms": t, "import_ref": null,
        "windows": [{ "identity": "[\"five_hour\",null,null]", "key": "five_hour", "label": "5 hour",
                      "used_pct": 10, "remaining_pct": 90, "reset_at_ms": t + 3_600_000 }],
    }]})
    .to_string()
}

#[test]
fn the_hosted_store_answers_the_napi_contract_and_the_status_route() -> Result<()> {
    let service = Service::start(true)?;
    let dir = service.store_dir();
    // TS's open over the socket: the boot already opened the engine; its status answers.
    let opened = service.store(
        "open",
        &json!({ "dir": dir, "retentionDays": 0 }).to_string(),
    )?;
    assert_eq!(opened.status, 200, "{}", opened.body);
    assert_eq!(opened.json()?["state"], "ready");
    // Only the front door reaches the store: a dashboard session is refused at the table.
    let refused = service.call("POST", "/internal/store/status", &admin(), Some(b"{}"))?;
    assert_eq!(refused.status, 401);
    // A write and its read, the same answers the NAPI module gives.
    let t = 1_790_000_000_000u64;
    let recorded = service.store("record", &observation(t))?;
    assert_eq!(recorded.status, 200, "{}", recorded.body);
    let history = service.store(
        "historyRecords",
        &json!({ "fromMs": 0, "toMs": t + 3_600_000, "nowMs": t, "thinning": { "kind": "none" } })
            .to_string(),
    )?;
    assert_eq!(history.status, 200);
    assert!(history.body.contains("codex:first"), "{}", history.body);
    // Digits in a real binding method name must reach dispatch (R3 socket-suite regression).
    let legacy = service.store(
        "activityImportCodexV1",
        &json!({
            "path": service.root.join("absent-codex-v1.json"), "computer": "ubuntu"
        })
        .to_string(),
    )?;
    assert!(
        legacy.json()?["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("not_found:")),
        "{}",
        legacy.body
    );
    // An unknown method is the binding's invalid, in its error shape.
    let unknown = service.store("noSuchMethod", "{}")?;
    assert_eq!(unknown.status, 500);
    assert!(
        unknown.json()?["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("invalid:")),
        "{}",
        unknown.body
    );
    // While the service holds the store, no second writer opens it (the kernel lock).
    let second = aac_store::ops::open_engine(&dir.to_string_lossy(), None, None);
    assert!(
        second.is_err_and(|e| e.code == "lock_held"),
        "the host is the one writer"
    );
    // /api/store/status with no snapshot is answered from the Engine in process.
    let status = service.call("GET", "/api/store/status", &admin(), Some(b"{}"))?;
    assert_eq!(status.status, 200, "{}", status.body);
    let status = status.json()?;
    assert_eq!(status["state"], "ready");
    assert_eq!(status["instanceId"], opened.json()?["instanceId"]);
    // The jobs record is readable, with no pass yet (the first waits minutes).
    let jobs = service.store("hostJobs", "{}")?;
    assert_eq!(jobs.json()?["open"], true);
    assert_eq!(jobs.json()?["jobs"]["passes"], 0);
    // A clean service end closes the engine: the same files open in process afterwards,
    // with the acknowledged write intact.
    let mut service = service;
    service.stop()?;
    let engine = aac_store::ops::open_engine(&dir.to_string_lossy(), None, None)
        .map_err(|e| e.to_string())?;
    let status = engine.status().map_err(|e| e.to_string())?;
    assert_eq!(status["findings"], json!([]), "{status}");
    Ok(())
}

#[test]
fn a_boot_without_the_store_config_never_opens_it() -> Result<()> {
    let service = Service::start(false)?;
    let reply = service.store("status", "{}")?;
    assert_eq!(reply.status, 500);
    assert!(
        reply.json()?["error"]
            .as_str()
            .is_some_and(|e| e.starts_with("service_down:")),
        "{}",
        reply.body
    );
    assert!(!service.store_dir().exists(), "no store folder was made");
    // The hosted status read declines too, so TS's snapshot path answers as before.
    let status = service.call("GET", "/api/store/status", &admin(), Some(b"{}"))?;
    assert_eq!(status.status, 503, "{}", status.body);
    Ok(())
}

/// The wave-56 BrokenPipe shape, deterministic: the service answers and closes before the
/// request body is written (under load the boundary can reply 401 early). The exchange must
/// still read that reply; the contract assertions apply to it unchanged.
#[test]
fn an_early_reply_is_read_even_when_the_body_write_hits_broken_pipe() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("early.sock");
    let listener = UnixListener::bind(&socket)?;
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("fixture connect");
        // Answer, then refuse the body: SHUT_RD makes the client's write hit EPIPE while
        // the reply stays readable. The body queued before the shutdown must be drained
        // before the close: a close with unread data resets the connection, and the reset
        // destroys the reply read (the r4 gate run caught that flake). Drained, the pattern
        // is deterministic — 30/30 in a syscall probe with the strict read criterion.
        let _ = stream
            .write_all(b"HTTP/1.1 401 Unauthorized\r\ncontent-length: 12\r\n\r\n{\"ok\":false}");
        let _ = stream.shutdown(Shutdown::Read);
        let mut sink = [0u8; 65536];
        while let Ok(n) = stream.read(&mut sink) {
            if n == 0 {
                break;
            }
        }
    });
    let body = vec![b'x'; 1_048_576];
    let reply = exchange(
        &socket,
        "POST",
        "/internal/store/status",
        &admin(),
        Some(&body),
    )?;
    server.join().expect("fixture server");
    assert_eq!(reply.status, 401);
    assert_eq!(reply.json()?["ok"], json!(false));
    Ok(())
}

/// A socket file whose listener is not accepting yet refuses the first connect; the
/// exchange retries within its deadline instead of failing the contract test.
#[test]
fn the_first_connect_retries_until_the_listener_answers() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let socket = dir.path().join("late.sock");
    // Bind and drop: the file stays, but connect reads ECONNREFUSED until the real bind.
    drop(UnixListener::bind(&socket)?);
    let mut server = None;
    let stream = connect_with_retry(&socket, |error| {
        assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
        fs::remove_file(&socket)?;
        let listener = UnixListener::bind(&socket)?;
        server = Some(std::thread::spawn(move || -> io::Result<()> {
            let (stream, _) = listener.accept()?;
            stream.set_read_timeout(Some(Duration::from_secs(5)))?;
            let mut request = BufReader::new(stream);
            let mut line = Vec::new();
            loop {
                line.clear();
                if request.read_until(b'\n', &mut line)? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "request headers",
                    ));
                }
                if line == b"\r\n" {
                    break;
                }
            }
            // Closing with unread request bytes can reset the connection before the reply is read.
            request
                .into_inner()
                .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}")
        }));
        Ok(())
    })?;
    let reply = exchange_on(
        stream,
        "GET",
        "/api/health",
        &principal("session", "public-health", "public"),
        None,
    );
    server
        .expect("first connect failed")
        .join()
        .expect("fixture server")?;
    assert_eq!(reply?.status, 200);
    Ok(())
}
