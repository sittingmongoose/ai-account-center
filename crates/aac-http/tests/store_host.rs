//! R3 store host checks on the real binary and the real Unix socket; all stores are
//! fixtures in temporary folders. Each fails against a service without the store host
//! (no `/internal/store/<method>` route, no engine behind `/api/store/status`).
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
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
        let mut boot = json!({"ccsDir":root,"socketPath":socket,"secret":SECRET,"bootId":BOOT_ID});
        if store {
            boot["store"] = json!({
                "dir": root.join("store"),
                "retentionDays": 0,
                "snapshotDirectory": null,
                "shareBackupDir": null,
                "shareBackupOff": false,
            });
        }
        writeln!(child.stdin.as_mut().ok_or("stdin missing")?, "{boot}")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let service = Self {
            child,
            socket,
            root,
            _keep: dir,
        };
        loop {
            if service.socket.exists()
                && service
                    .call(
                        "GET",
                        "/api/health",
                        &principal("session", "public-health", "public"),
                        None,
                    )
                    .is_ok_and(|r| r.status == 200)
            {
                return Ok(service);
            }
            if Instant::now() >= deadline {
                return Err("service failed to start".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn store_dir(&self) -> PathBuf {
        self.root.join("store")
    }

    fn call(&self, method: &str, path: &str, who: &str, body: Option<&[u8]>) -> Result<Reply> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(15)))?;
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
        let mut raw = String::new();
        stream.read_to_string(&mut raw)?;
        parse(&raw)
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
