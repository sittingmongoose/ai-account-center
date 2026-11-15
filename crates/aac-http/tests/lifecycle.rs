//! L1 lifecycle checks on the real binary and the real Unix socket; all data is temporary.
//! Each one failed against the M3 service (7bc46ca90): no deadline or pool, no named declines,
//! no build header, no short socket folder, no feeds or events.
#![cfg(unix)]
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    error::Error,
    fs,
    io::{BufRead, BufReader, Read, Write},
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

fn admin() -> String {
    principal("session", "fixture-admin", "admin")
}

fn front_door() -> String {
    principal("session", "front-door", "service")
}

struct Service {
    child: Child,
    socket: PathBuf,
    root: PathBuf,
    _keep: Vec<tempfile::TempDir>,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}
impl Reply {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn json(&self) -> Result<Value> {
        Ok(serde_json::from_str(&self.body)?)
    }
}

fn parse(raw: &str) -> Result<Reply> {
    let (head, body) = raw.split_once("\r\n\r\n").ok_or("no head")?;
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .ok_or("no status")?
        .parse()?;
    let headers = lines
        .filter_map(|l| l.split_once(": "))
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();
    Ok(Reply {
        status,
        headers,
        body: body.to_owned(),
    })
}

impl Service {
    /// Start under `root` (canonical, 0700) with the socket at `socket`, whose folders the test
    /// made as the front door does. `env` reaches only the child.
    fn start_at(
        root: PathBuf,
        socket: PathBuf,
        env: &[(&str, String)],
        keep: Vec<tempfile::TempDir>,
    ) -> Result<Self> {
        let mut command = Command::new(env!("CARGO_BIN_EXE_aac-http"));
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .env_clear();
        for (k, v) in env {
            command.env(k, v);
        }
        let mut child = command.spawn()?;
        let boot = json!({"ccsDir":root,"socketPath":socket,"secret":SECRET,"bootId":BOOT_ID});
        writeln!(child.stdin.as_mut().ok_or("stdin missing")?, "{boot}")?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut service = Self {
            child,
            socket,
            root,
            _keep: keep,
        };
        loop {
            if let Some(status) = service.child.try_wait()? {
                return Err(format!("service exited: {status}").into());
            }
            if service.socket.exists()
                && service
                    .get(
                        "/api/health",
                        &principal("session", "public-health", "public"),
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

    fn start(env: &[(&str, String)]) -> Result<Self> {
        let dir = tempfile::tempdir()?;
        let root = fs::canonicalize(dir.path())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let run = root.join("run");
        let boot = run.join(format!("aac-http-{BOOT_ID}"));
        private(&run)?;
        private(&boot)?;
        Self::start_at(root, boot.join("http.sock"), env, vec![dir])
    }

    fn call(&self, method: &str, path: &str, who: &str, body: Option<&[u8]>) -> Result<Reply> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
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

    fn get(&self, path: &str, who: &str) -> Result<Reply> {
        self.call("GET", path, who, None)
    }
}

#[test]
fn health_carries_the_build_identity_only_in_a_private_header() -> Result<()> {
    let s = Service::start(&[])?;
    let reply = s.get(
        "/api/health",
        &principal("session", "public-health", "public"),
    )?;
    assert_eq!(reply.status, 200);
    assert_eq!(
        reply.body, r#"{"status":"ok"}"#,
        "the public body is unchanged"
    );
    let build: Value = serde_json::from_str(
        reply
            .header("x-aac-service-build")
            .ok_or("no build header")?,
    )?;
    let binary = fs::read(env!("CARGO_BIN_EXE_aac-http"))?;
    let sha: String = Sha256::digest(&binary)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(build["sha256"], json!(sha), "the running image's own hash");
    assert_eq!(build["protocol"], json!(aac_http::PROTOCOL));
    assert_eq!(build["storeSchema"], json!(2));
    assert!(build["commit"].as_str().is_some_and(|c| !c.is_empty()));
    assert_eq!(build.as_object().map(|o| o.len()), Some(4));
    // Only health carries it.
    let diag = s.get("/api/diag", &admin())?;
    assert_eq!(diag.status, 200);
    assert!(diag.header("x-aac-service-build").is_none());
    Ok(())
}

#[test]
fn every_decline_names_its_reason() -> Result<()> {
    let s = Service::start(&[])?;
    let settings = s.root.join("account-refresh-settings.json");
    let decline = |path: &str, body: Option<&[u8]>| -> Result<(u16, String, Value)> {
        let r = s.call("GET", path, &admin(), body)?;
        Ok((
            r.status,
            r.header("x-aac-decline").unwrap_or("").to_owned(),
            r.json()?,
        ))
    };
    // An absent file, a shape the DTO refuses, a number outside the proved domain.
    assert_eq!(
        decline("/api/accounts/settings", None)?,
        (
            503,
            "unsupported".into(),
            json!({"error":"TypeScript settings fallback required"})
        )
    );
    fs::write(&settings, r#"{"refreshIntervalSeconds":60,"extra":1}"#)?;
    assert_eq!(decline("/api/accounts/settings", None)?.1, "shape");
    fs::write(&settings, r#"{"refreshIntervalSeconds":5}"#)?;
    assert_eq!(decline("/api/accounts/settings", None)?.1, "number");
    fs::write(&settings, r#"{"refreshIntervalSeconds":90}"#)?;
    let ok = s.get("/api/accounts/settings", &admin())?;
    assert_eq!((ok.status, ok.header("x-aac-decline")), (200, None));
    // Read routes: a refused envelope, a composer's own reason, an unsupported snapshot.
    let read = json!({"error":"TypeScript read fallback required"});
    assert_eq!(
        decline("/api/views/home", Some(b"[]"))?,
        (503, "shape".into(), read.clone())
    );
    let table: Value =
        serde_json::from_str(include_str!("../../../tests/spec/views/home/cases.json"))?;
    let case = table["cases"]
        .as_array()
        .ok_or("cases")?
        .iter()
        .find(|c| c["name"] == "decline/zone-Africa/Casablanca")
        .ok_or("case")?;
    let envelope = json!({
        "data": {"ctx": case["ctx"], "dashboard": case["dashboard"], "profiles": case["profiles"], "antigravity": case["antigravity"]},
        "presentation": case["presentation"],
    });
    assert_eq!(
        decline("/api/views/home", Some(&serde_json::to_vec(&envelope)?))?.1,
        "zone"
    );
    assert_eq!(
        decline(
            "/api/store/status",
            Some(br#"{"snapshot":{"state":"ready","fixture":true}}"#)
        )?,
        (503, "unsupported".into(), read)
    );
    assert_eq!(
        decline("/api/status", Some(br#"{"snapshot":1,"more":2}"#))?.1,
        "shape"
    );
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn a_stuck_read_declines_at_its_deadline_and_never_runs_more_than_eight_blocking_reads()
-> Result<()> {
    let scratch = tempfile::tempdir()?;
    let shim = scratch.path().join("slow-read.so");
    let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/slow-read.c");
    let built = Command::new("cc")
        .args(["-shared", "-fPIC", "-O1", "-o"])
        .arg(&shim)
        .arg(&source)
        .arg("-ldl")
        .status()?;
    assert!(built.success(), "the slow-read shim needs a C compiler");
    let receipt = scratch.path().join("receipt");
    // The root is made first so the shim can name the settings file's real path.
    let dir = tempfile::tempdir()?;
    let root = fs::canonicalize(dir.path())?;
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    let settings = root.join("account-refresh-settings.json");
    fs::write(&settings, r#"{"refreshIntervalSeconds":90}"#)?;
    let run = root.join("run");
    let boot = run.join(format!("aac-http-{BOOT_ID}"));
    private(&run)?;
    private(&boot)?;
    let s = Service::start_at(
        root,
        boot.join("http.sock"),
        &[
            ("LD_PRELOAD", shim.display().to_string()),
            ("AAC_SLOW_READ_PATH", settings.display().to_string()),
            ("AAC_SLOW_READ_RECEIPT", receipt.display().to_string()),
            ("AAC_SLOW_READ_MS", "1500".into()),
        ],
        vec![dir],
    )?;
    let socket = s.socket.clone();
    let started = Instant::now();
    let calls: Vec<_> = (0..20)
        .map(|_| {
            let socket = socket.clone();
            std::thread::spawn(move || -> Option<(u16, String, Duration)> {
                let began = Instant::now();
                let mut stream = UnixStream::connect(&socket).ok()?;
                stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
                write!(
                    stream,
                    "GET /api/accounts/settings HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {}\r\n\r\n",
                    admin()
                )
                .ok()?;
                let mut raw = String::new();
                stream.read_to_string(&mut raw).ok()?;
                let reply = parse(&raw).ok()?;
                Some((
                    reply.status,
                    reply.header("x-aac-decline").unwrap_or("").to_owned(),
                    began.elapsed(),
                ))
            })
        })
        .collect();
    let mut reasons = Vec::new();
    for call in calls {
        let (status, reason, elapsed) = call.join().map_err(|_| "thread")?.ok_or("call")?;
        assert_eq!(status, 503, "a stuck read is never answered late");
        assert!(
            elapsed < Duration::from_millis(1000),
            "answered within the 400 ms budget, not after the 1.5 s read: {elapsed:?}"
        );
        reasons.push(reason);
    }
    let deadline = reasons.iter().filter(|r| *r == "deadline").count();
    let busy = reasons.iter().filter(|r| *r == "busy").count();
    assert_eq!(deadline + busy, 20, "{reasons:?}");
    assert!((1..=8).contains(&deadline), "{reasons:?}");
    assert!(started.elapsed() < Duration::from_millis(1400));
    // The stuck reads still finish in the background (each read sleeps, so a task takes a few
    // seconds); the pool frees again once they end.
    let freed = Instant::now() + Duration::from_secs(15);
    let again = loop {
        let reply = s.get("/api/accounts/settings", &admin())?;
        if reply.header("x-aac-decline") != Some("busy") || Instant::now() >= freed {
            break reply;
        }
        std::thread::sleep(Duration::from_millis(200));
    };
    // Still a slow read, so: its deadline, not busy.
    assert_eq!(
        (again.status, again.header("x-aac-decline")),
        (503, Some("deadline"))
    );
    // The pool never held more than 8 blocking reads.
    let peak = fs::read_to_string(&receipt)?
        .lines()
        .filter_map(|l| l.strip_prefix("inflight ")?.parse::<usize>().ok())
        .max()
        .ok_or("the shim never delayed a read")?;
    assert!(
        (1..=8).contains(&peak),
        "at most 8 blocking reads at once, saw {peak}"
    );
    Ok(())
}

/// The short folder the front door makes when `<root>/run/aac-http-<id>/http.sock` would be too
/// long: `<base>/aac-<hash12>/aac-http-<id>/http.sock`.
fn short_socket(base: &Path, root: &Path) -> Result<PathBuf> {
    let folder = base.join(format!("aac-{}", aac_http::short_name(root)));
    let boot = folder.join(format!("aac-http-{BOOT_ID}"));
    private(&folder)?;
    private(&boot)?;
    Ok(boot.join("http.sock"))
}

#[test]
fn an_over_long_root_starts_on_a_short_private_socket_folder() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let mut long = fs::canonicalize(dir.path())?;
    // Roots of 63 to 200 bytes (Fable's longpath.py stopped at 94).
    while long.as_os_str().len() < 200 {
        long.push("u".repeat(30));
    }
    fs::create_dir_all(&long)?;
    fs::set_permissions(&long, fs::Permissions::from_mode(0o700))?;
    let root = fs::canonicalize(&long)?;
    private(&root.join("run"))?;
    let classic = root.join(format!("run/aac-http-{BOOT_ID}/http.sock"));
    assert!(
        classic.as_os_str().len() > 108,
        "the classic path cannot bind"
    );
    let base = tempfile::Builder::new()
        .prefix("aac-l1-")
        .tempdir_in("/tmp")?;
    let socket = short_socket(base.path(), &root)?;
    assert!(socket.as_os_str().len() <= 100);
    let s = Service::start_at(root.clone(), socket, &[], vec![dir, base])?;
    assert_eq!(s.get("/api/diag", &admin())?.status, 200);
    drop(s);

    // A short folder made for another root is refused: the service exits without binding.
    let other = tempfile::tempdir()?;
    let base = tempfile::Builder::new()
        .prefix("aac-l1-")
        .tempdir_in("/tmp")?;
    let wrong = short_socket(base.path(), &fs::canonicalize(other.path())?)?;
    assert!(Service::start_at(root.clone(), wrong.clone(), &[], vec![]).is_err());
    assert!(!wrong.exists());
    // So is a group-readable one.
    let base = tempfile::Builder::new()
        .prefix("aac-l1-")
        .tempdir_in("/tmp")?;
    let open = short_socket(base.path(), &root)?;
    fs::set_permissions(
        open.parent().and_then(Path::parent).ok_or("folder")?,
        fs::Permissions::from_mode(0o750),
    )?;
    assert!(Service::start_at(root, open, &[], vec![]).is_err());
    Ok(())
}

#[test]
fn feeds_and_events_reach_only_the_front_door() -> Result<()> {
    let s = Service::start(&[])?;
    let feed = br#"{"generation":"g1","data":{"rows":[1,2]},"facts":null}"#;
    // Sessions, devices and the public envelope never reach the internal routes.
    for who in [
        admin(),
        principal("device", "dev_fixture", "device"),
        principal("session", "public-health", "public"),
        principal("session", "fixture-admin", "service"),
    ] {
        assert_eq!(
            s.call("PUT", "/internal/feed/home", &who, Some(feed))?
                .status,
            401
        );
        assert_eq!(s.get("/internal/events", &who)?.status, 401);
    }
    // And the front door never reaches the public read routes as itself.
    assert_eq!(s.get("/api/accounts/settings", &front_door())?.status, 401);
    // Open the event stream, then push a generation without facts.
    let mut events = UnixStream::connect(&s.socket)?;
    events.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        events,
        "GET /internal/events HTTP/1.1\r\nHost: private\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {}\r\n\r\n",
        front_door()
    )?;
    let mut reader = BufReader::new(events);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    assert!(line.starts_with("HTTP/1.1 200"), "{line}");
    loop {
        line.clear();
        reader.read_line(&mut line)?;
        if line == "\r\n" {
            break;
        }
    }
    let put = s.call("PUT", "/internal/feed/home", &front_door(), Some(feed))?;
    assert_eq!(put.status, 200);
    assert_eq!(
        put.json()?,
        json!({"source":"home","generation":"g1","facts":false})
    );
    // Chunked NDJSON: a size line, then the event.
    let mut event = String::new();
    while !event.contains("facts-needed") {
        line.clear();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        event.push_str(&line);
    }
    let text = event
        .lines()
        .find(|l| l.contains("facts-needed"))
        .ok_or("no event")?;
    assert_eq!(
        serde_json::from_str::<Value>(text)?,
        json!({"event":"facts-needed","source":"home","generation":"g1"})
    );
    let held = s.get("/internal/feed/home", &front_door())?;
    assert_eq!(held.json()?["generation"], json!("g1"));
    let none = s.get("/internal/feed/status", &front_door())?;
    assert_eq!(
        (none.status, none.header("x-aac-decline")),
        (503, Some("stale"))
    );
    let bad = s.call(
        "PUT",
        "/internal/feed/home",
        &front_door(),
        Some(br#"{"generation":"g2","data":1}"#),
    )?;
    assert_eq!(
        (bad.status, bad.header("x-aac-decline")),
        (503, Some("shape"))
    );
    Ok(())
}
