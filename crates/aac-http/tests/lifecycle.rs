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

    /// End the service as a front door that went away does: its stdin closes.
    fn finish(mut self) -> Result<()> {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + Duration::from_secs(5);
        while self.child.try_wait()?.is_none() {
            if Instant::now() >= deadline {
                return Err("service did not end on stdin EOF".into());
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        Ok(())
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
    let folder = socket
        .parent()
        .and_then(Path::parent)
        .ok_or("folder")?
        .to_path_buf();
    let s = Service::start_at(root.clone(), socket.clone(), &[], vec![dir])?;
    assert_eq!(s.get("/api/diag", &admin())?.status, 200);
    // When the service ends, its socket, boot folder and empty short folder go with it, even if
    // the front door that made them is gone.
    s.finish()?;
    assert!(!socket.exists());
    assert!(!folder.exists(), "the empty short folder is removed");
    drop(base);

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

// ---------------------------------------------------------------- R1: Home from fed generations
// Each check failed before R1: the feed store kept no payload and Home read only the envelope body.

const FED_DASHBOARD: &str = r#"{"schemaVersion":1,"updatedAt":"","settings":{"refreshIntervalSeconds":60},"accounts":[{"id":"claude:a","provider":"claude","sampledAt":"2026-11-14T10:00:00.000Z","windows":[{"key":"five_hour","resetAt":"2026-11-14T12:00:00.000Z","usedPercent":42.5},{"key":"seven_day","resetAt":"2026-11-20T12:00:00.000Z","usedPercent":1e-7}]}],"server":{"version":"0.0.0","commit":null,"views":["home"]}}"#;
const FED_FACTS: &str = r#"{"order":{"claude:a":0},"epochs":{"2026-11-14T12:00:00.000Z":1794650400000,"2026-11-20T12:00:00.000Z":1795168800000,"2026-11-14T10:00:00.000Z":1794643200000}}"#;
const FED_OPENS: &str = r#"[["p1",{"id":"op_1","platform":"mac","state":"opened","confirmedCount":null,"totalCount":null,"message":null}]]"#;
const FED_ANTIGRAVITY: &str = r#"{"data":{"schemaVersion":1,"hostId":"ubuntu","profiles":[]},"presentation":{"dates":{},"epochs":{},"order":{}}}"#;
const FED_MANIFEST: &str = r#"{"version":1,"profiles":[{"id":"p1","email":"one@example.invalid","mac":{"launcherName":"Claude 1","launcherPath":"/Applications/Claude 1.app","profilePath":"/Users/fixture/Library/Application Support/Claude-1","sshHost":"mac"}},{"id":"p2","email":"two@example.invalid","mac":{"launcherName":"Claude 2","launcherPath":"/Applications/Claude 2.app","profilePath":"/Users/fixture/Library/Application Support/Claude-2","sshHost":"mac"}}]}"#;

fn push(s: &Service, source: &str, generation: &str, data: &str, facts: &str) -> Result<Reply> {
    let body = format!(r#"{{"generation":"{generation}","data":{data},"facts":{facts}}}"#);
    s.call(
        "PUT",
        &format!("/internal/feed/{source}"),
        &front_door(),
        Some(body.as_bytes()),
    )
}

fn push_home(s: &Service, generation: &str) -> Result<()> {
    for (source, data, facts) in [
        ("home-dashboard-mac-0", FED_DASHBOARD, FED_FACTS),
        ("home-opens", FED_OPENS, "null"),
        ("home-antigravity", FED_ANTIGRAVITY, "null"),
    ] {
        let reply = push(s, source, generation, data, facts)?;
        assert_eq!(
            (reply.status, reply.json()?["generation"].clone()),
            (200, json!(generation)),
            "{source}"
        );
    }
    Ok(())
}

fn feed(generations: [&str; 3]) -> String {
    json!({
        "platform": "mac", "secure": false, "now": 1_794_650_400_000_i64,
        "updatedAt": "2026-11-14T12:00:00.500Z",
        "dashboard": generations[0], "opens": generations[1], "antigravity": generations[2],
    })
    .to_string()
}

/// GET /api/views/home with the fed headers (no body).
fn fed_home(s: &Service, feed: &str, extra: &str) -> Result<Reply> {
    let mut stream = UnixStream::connect(&s.socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /api/views/home HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {}\r\nx-aac-feed: {feed}\r\n{extra}\r\n",
        admin()
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    parse(&raw)
}

fn write_manifest(s: &Service) -> Result<()> {
    let file = s.root.join("claude-desktop-profiles.json");
    fs::write(&file, FED_MANIFEST)?;
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[test]
fn home_is_answered_from_the_fed_generations_with_its_etag() -> Result<()> {
    let s = Service::start(&[])?;
    write_manifest(&s)?;
    push_home(&s, "1")?;
    let reply = fed_home(&s, &feed(["1", "1", "1"]), "")?;
    assert_eq!(reply.status, 200, "{}", reply.body);
    assert_eq!(reply.header("x-aac-view"), Some("home@3"));
    // The profiles are the desktop-profiles read's rows, each with its Open's progress (p2 has none).
    let rows = s.get("/api/claude/desktop-profiles", &admin())?.json()?["profiles"].clone();
    let mut profiles = rows.as_array().ok_or("rows")?.clone();
    profiles[0]["openOperation"] = serde_json::from_str::<Value>(FED_OPENS)?[0][1].clone();
    profiles[1]["openOperation"] = Value::Null;
    // The request's clock: its updatedAt, and the 5-hour reset passed at `now` (sampled before it), as
    // markPassedResets marks it; the weekly one has not. 1e-7 is written as Node writes it.
    let dashboard = FED_DASHBOARD
        .replace(
            r#""updatedAt":"""#,
            r#""updatedAt":"2026-11-14T12:00:00.500Z""#,
        )
        .replace(
            r#""usedPercent":42.5}"#,
            r#""usedPercent":42.5,"resetPassed":true}"#,
        );
    let want = format!(
        r#"{{"schemaVersion":1,"dashboard":{dashboard},"profiles":{},"antigravity":{{"schemaVersion":1,"hostId":"ubuntu","profiles":[]}},"view":null}}"#,
        Value::Array(profiles)
    );
    assert_eq!(reply.body, want);
    // R1b (review L2): the tag digests everything the bytes are a function of: the same inputs give the same tag
    // (and the same bytes); another clock, page context or manifest gives another.
    let tag = reply.header("etag").map(str::to_owned).ok_or("no etag")?;
    assert!(
        tag.len() == 68 && tag.starts_with("W/\"") && tag.ends_with('"'),
        "{tag}"
    );
    let again = fed_home(&s, &feed(["1", "1", "1"]), "")?;
    assert_eq!(
        (again.body.as_str(), again.header("etag")),
        (want.as_str(), Some(tag.as_str()))
    );
    // Before the reset, nothing is marked.
    let early = feed(["1", "1", "1"]).replace("1794650400000", "1794650399999");
    let reply = fed_home(&s, &early, "")?;
    assert!(!reply.body.contains("resetPassed"), "{}", reply.body);
    assert_ne!(reply.header("etag"), Some(tag.as_str()));
    // A changed manifest (same generations) is read, its rows served, and the tag moves.
    let file = s.root.join("claude-desktop-profiles.json");
    fs::write(&file, FED_MANIFEST.replace("Claude 2", "Claude Two"))?;
    let changed = fed_home(&s, &feed(["1", "1", "1"]), "")?;
    assert!(changed.body.contains("Claude Two"), "{}", changed.body);
    assert_ne!(changed.header("etag"), Some(tag.as_str()));
    Ok(())
}

#[test]
fn home_declines_a_missing_stale_or_unproven_generation() -> Result<()> {
    let s = Service::start(&[])?;
    write_manifest(&s)?;
    let named = |reply: Reply| {
        (
            reply.status,
            reply.header("x-aac-decline").map(str::to_owned),
        )
    };
    let stale = (503, Some("stale".to_owned()));
    // A missing feed declines: nothing was pushed to this boot.
    assert_eq!(named(fed_home(&s, &feed(["1", "1", "1"]), "")?), stale);
    push_home(&s, "1")?;
    assert_eq!(fed_home(&s, &feed(["1", "1", "1"]), "")?.status, 200);
    // A push during a request: the request names generation 1, the service now holds 2.
    assert_eq!(
        push(&s, "home-dashboard-mac-0", "2", FED_DASHBOARD, FED_FACTS)?.json()?["generation"],
        json!("2")
    );
    assert_eq!(named(fed_home(&s, &feed(["1", "1", "1"]), "")?), stale);
    // Concurrent pushes: generation 1 arriving after 2 is not taken, and the reply says 2 is held.
    assert_eq!(
        push(&s, "home-dashboard-mac-0", "1", FED_DASHBOARD, FED_FACTS)?.json()?["generation"],
        json!("2")
    );
    assert_eq!(fed_home(&s, &feed(["2", "1", "1"]), "")?.status, 200);
    // Another transport's dashboard is another source: not pushed, so stale.
    let secure = feed(["2", "1", "1"]).replace("\"secure\":false", "\"secure\":true");
    assert_eq!(named(fed_home(&s, &secure, "")?), stale);
    // A generation that cannot be served exactly declines with its reason (a missing epoch fact).
    push(
        &s,
        "home-dashboard-mac-0",
        "3",
        FED_DASHBOARD,
        r#"{"order":{},"epochs":{}}"#,
    )?;
    assert_eq!(
        named(fed_home(&s, &feed(["3", "1", "1"]), "")?),
        (503, Some("locale".to_owned()))
    );
    // A query, a body or an unreadable feed is not the fed read.
    assert_eq!(
        named(fed_home(&s, "{\"platform\":\"mac\"}", "")?),
        (503, Some("shape".to_owned()))
    );
    // Sessions only: the front door's own principal never reads Home.
    let mut stream = UnixStream::connect(&s.socket)?;
    write!(
        stream,
        "GET /api/views/home HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {}\r\nx-aac-feed: {}\r\n\r\n",
        front_door(),
        feed(["3", "1", "1"])
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    assert_eq!(parse(&raw)?.status, 401);
    Ok(())
}

#[test]
fn a_restarted_service_holds_no_feed_until_the_front_door_pushes_again() -> Result<()> {
    let s = Service::start(&[])?;
    write_manifest(&s)?;
    push_home(&s, "1")?;
    assert_eq!(fed_home(&s, &feed(["1", "1", "1"]), "")?.status, 200);
    // The service dies mid-feed (one source pushed of three); a new boot holds nothing.
    drop(s);
    let s = Service::start(&[])?;
    write_manifest(&s)?;
    assert_eq!(
        push(&s, "home-dashboard-mac-0", "1", FED_DASHBOARD, FED_FACTS)?.status,
        200
    );
    let reply = fed_home(&s, &feed(["1", "1", "1"]), "")?;
    assert_eq!(
        (reply.status, reply.header("x-aac-decline")),
        (503, Some("stale"))
    );
    push_home(&s, "1")?;
    assert_eq!(fed_home(&s, &feed(["1", "1", "1"]), "")?.status, 200);
    Ok(())
}

// R2: Status from fed generations. Six sources, each a JSON object of some of the composer's input members; the
// facts source carries the presentation facts of the whole set. The oracle is the envelope read of the same
// members and facts (the M3 path TS's handler uses today), written as Node writes it.
const STATUS_NOW: i64 = 1_794_650_400_500;
const STATUS_REPORTS: &str = r#"{"reports":{"root":"/fixture/share","state":"ok","scannedAt":1794650390000,"complete":true,"threads":[],"tasks":[],"cards":[],"problems":[],"boards":{}}}"#;
const STATUS_COLLECTED: &str = r#"{"registry":{"data":[{"name":"mail-worker","kind":"codex","machine":"vm","about":"Fixture task","lastSeen":"2026-11-14T11:59:00.000Z","readAt":1794650370000,"problem":null}],"asOf":1794650400000,"error":null},"machines":{"vm":{"data":null,"asOf":1794650370000,"error":{"reason":"Fixture host unavailable.","since":1794650350000}},"mac":{"data":null,"asOf":null,"error":null}},"browsers":{"data":null,"asOf":null,"error":null},"simlock":{"data":null,"asOf":null,"error":null},"usage":null}"#;
const STATUS_ANSWERS: &str = r#"{"answers":[],"sends":[],"answerHistoryProblem":null}"#;
const STATUS_CONTEXT: &str = r#"{"timeZone":"UTC","hiddenThreadNames":["aac-doorbell"],"machineIds":["vm","mac"],"machineLabels":{"vm":"VM","mac":"Mac"}}"#;

/// The envelope's facts of these parts, as rustReadInput computes them (a byte-order ranking stands in for
/// `localeCompare`: both paths read the same facts). `extra` adds the request's own clock.
fn status_facts(parts: &[&str], extra: Option<i64>) -> Result<Value> {
    fn walk(v: &Value, words: &mut Vec<String>, times: &mut Vec<i64>) {
        match v {
            Value::String(s) => {
                words.push(s.clone());
                if let Some(i) = s.find('.') {
                    words.push(s[i + 1..].to_owned());
                }
                if s == "2026-11-14T11:59:00.000Z" {
                    times.push(1_794_650_340_000);
                }
            }
            Value::Number(n) => {
                if let Some(t) = n.as_i64().filter(|t| *t >= 100_000_000_000) {
                    times.push(t);
                }
            }
            Value::Array(items) => items.iter().for_each(|i| walk(i, words, times)),
            Value::Object(map) => map.values().for_each(|i| walk(i, words, times)),
            _ => {}
        }
    }
    let (mut words, mut times) = (Vec::new(), Vec::new());
    for part in parts {
        walk(&serde_json::from_str(part)?, &mut words, &mut times);
    }
    times.extend(extra);
    words.sort();
    words.dedup();
    let order: serde_json::Map<String, Value> = words
        .into_iter()
        .enumerate()
        .map(|(i, w)| (w, json!(i)))
        .collect();
    // UTC words: the fixture's times all fall on Nov 14 between 11 AM and noon.
    let mut dates = serde_json::Map::new();
    for t in times {
        let seconds = t.div_euclid(1000) % 86_400;
        let (h, m) = (seconds / 3600, seconds % 3600 / 60);
        let clock = format!(
            "{}:{m:02} {}",
            if h % 12 == 0 { 12 } else { h % 12 },
            if h < 12 { "AM" } else { "PM" }
        );
        let iso = format!(
            "2026-11-14T{h:02}:{m:02}:{:02}.{:03}Z",
            seconds % 60,
            t.rem_euclid(1000)
        );
        dates.insert(
            t.to_string(),
            json!({"iso": iso, "clock": clock, "day": "Nov 14"}),
        );
    }
    Ok(
        json!({"dates": dates, "epochs": {"2026-11-14T11:59:00.000Z": 1_794_650_340_000_i64}, "order": order}),
    )
}

fn status_sources() -> [(&'static str, &'static str); 6] {
    [
        ("status-reports", STATUS_REPORTS),
        ("status-quota", "{}"),
        ("status-pinned", "{}"),
        ("status-collected", STATUS_COLLECTED),
        ("status-answers", STATUS_ANSWERS),
        ("status-facts", STATUS_CONTEXT),
    ]
}

fn push_status(s: &Service, generation: &str) -> Result<()> {
    let parts: Vec<&str> = status_sources().iter().map(|(_, d)| *d).collect();
    let facts = status_facts(&parts, None)?.to_string();
    for (source, data) in status_sources() {
        let facts = if source == "status-facts" {
            facts.as_str()
        } else {
            "{}"
        };
        let reply = push(s, source, generation, data, facts)?;
        assert_eq!(
            (reply.status, reply.json()?["generation"].clone()),
            (200, json!(generation)),
            "{source}: {}",
            reply.body
        );
    }
    Ok(())
}

fn status_feed(ids: [&str; 6], now: i64) -> String {
    json!({
        "now": now, "clock": "12:00 PM", "day": "Nov 14",
        "reports": ids[0], "quota": ids[1], "pinned": ids[2], "collected": ids[3], "answers": ids[4],
        "facts": ids[5],
    })
    .to_string()
}

/// GET /api/status with the fed header (no body).
fn fed_status(s: &Service, feed: &str) -> Result<Reply> {
    let mut stream = UnixStream::connect(&s.socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    write!(
        stream,
        "GET /api/status HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {}\r\nx-aac-feed: {feed}\r\n\r\n",
        admin()
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    parse(&raw)
}

/// The envelope read of the same members and facts, with now's date: the bytes Node writes for it and its ETag.
fn status_envelope(s: &Service, now: i64) -> Result<(String, String)> {
    let mut data = serde_json::Map::new();
    data.insert("now".into(), json!(now));
    for (_, part) in status_sources() {
        let Value::Object(members) = serde_json::from_str(part)? else {
            return Err("part".into());
        };
        data.extend(members);
    }
    let parts: Vec<&str> = status_sources().iter().map(|(_, d)| *d).collect();
    let presentation = status_facts(&parts, Some(now))?;
    let body = json!({"data": data, "presentation": presentation}).to_string();
    let reply = s.call("GET", "/api/status", &admin(), Some(body.as_bytes()))?;
    assert_eq!(reply.status, 200, "envelope: {}", reply.body);
    let mut writer = aac_views::js_json::Writer::default();
    writer.value(&reply.json()?);
    let (full, identity) = writer.finish();
    let digest: String = Sha256::digest(&identity)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((String::from_utf8(full)?, format!("W/\"{digest}\"")))
}

#[test]
fn status_is_answered_from_the_fed_generations_byte_equal_to_the_envelope_read() -> Result<()> {
    let s = Service::start(&[])?;
    push_status(&s, "1")?;
    for now in [STATUS_NOW, STATUS_NOW + 1, STATUS_NOW + 4_000] {
        let reply = fed_status(&s, &status_feed(["1"; 6], now))?;
        assert_eq!(reply.status, 200, "{}", reply.body);
        assert_eq!(reply.header("x-aac-view"), Some("status@1"));
        let (want, etag) = status_envelope(&s, now)?;
        assert_eq!(reply.body, want, "now {now}");
        assert_eq!(reply.header("etag"), Some(etag.as_str()), "now {now}");
        // The clock-only members move with `now`; the rest is its generations'.
        assert!(reply.body.contains("\"ageSeconds\""), "{}", reply.body);
    }
    // The same second and generations: the ETag is the identity's, whether or not it was kept.
    let a = fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?;
    let b = fed_status(&s, &status_feed(["1"; 6], STATUS_NOW + 2))?;
    assert_eq!(a.header("etag"), b.header("etag"));
    Ok(())
}

#[test]
fn status_declines_a_missing_stale_or_unproven_generation() -> Result<()> {
    let s = Service::start(&[])?;
    let named = |reply: Reply| {
        (
            reply.status,
            reply.header("x-aac-decline").map(str::to_owned),
        )
    };
    let reason = |word: &str| (503, Some(word.to_owned()));
    // Nothing pushed to this boot.
    assert_eq!(
        named(fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?),
        reason("stale")
    );
    push_status(&s, "1")?;
    assert_eq!(
        fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?.status,
        200
    );
    // A push during a request: the request names 1, the service now holds 2 of the reports.
    assert_eq!(
        push(&s, "status-reports", "2", STATUS_REPORTS, "{}")?.json()?["generation"],
        json!("2")
    );
    assert_eq!(
        named(fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?),
        reason("stale")
    );
    // A late push of 1 is not taken; the reply names 2, and the request naming 2 is served.
    assert_eq!(
        push(&s, "status-reports", "1", STATUS_REPORTS, "{}")?.json()?["generation"],
        json!("2")
    );
    let ids = ["2", "1", "1", "1", "1", "1"];
    assert_eq!(fed_status(&s, &status_feed(ids, STATUS_NOW))?.status, 200);
    // A clock `toISOString` cannot write (before 1970): the request's own date fact cannot be made.
    assert_eq!(
        named(fed_status(&s, &status_feed(ids, -1))?),
        reason("date")
    );
    // Facts that miss a word the composer ranks: the view cannot be proved (`date`), TS answers.
    let thin = status_facts(&[STATUS_REPORTS], None)?.to_string();
    push(&s, "status-facts", "3", STATUS_CONTEXT, &thin)?;
    let ids = ["2", "1", "1", "1", "1", "3"];
    assert_eq!(
        named(fed_status(&s, &status_feed(ids, STATUS_NOW))?),
        reason("date")
    );
    // A generation that does not round-trip as Node writes it, or holds an Object.prototype name, is held declined.
    for (generation, data) in [
        ("4", r#"{"reports":1.0}"#),
        ("5", r#"{"reports":{"constructor":[]}}"#),
    ] {
        assert_eq!(
            push(&s, "status-reports", generation, data, "{}")?.status,
            200
        );
        let status = s
            .get("/internal/feed/status-reports", &front_door())?
            .json()?;
        assert_eq!(status["declined"], json!("shape"), "{data}");
        let ids = [generation, "1", "1", "1", "1", "3"];
        assert_eq!(
            named(fed_status(&s, &status_feed(ids, STATUS_NOW))?),
            reason("shape")
        );
    }
    // Status sources take numeric generations and known names only.
    assert_eq!(
        push(&s, "status-reports", "g6", STATUS_REPORTS, "{}")?.status,
        503
    );
    assert_eq!(push(&s, "status-other", "6", "{}", "{}")?.status, 503);
    // An unreadable feed header is not the fed read.
    assert_eq!(named(fed_status(&s, "{\"now\":1}")?), reason("shape"));
    // Sessions only: the front door's own principal never reads Status.
    let mut stream = UnixStream::connect(&s.socket)?;
    write!(
        stream,
        "GET /api/status HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {}\r\nx-aac-feed: {}\r\n\r\n",
        front_door(),
        status_feed(["1"; 6], STATUS_NOW)
    )?;
    let mut raw = String::new();
    stream.read_to_string(&mut raw)?;
    assert_eq!(parse(&raw)?.status, 401);
    Ok(())
}

#[test]
fn a_restarted_service_holds_no_status_feed_until_the_front_door_pushes_again() -> Result<()> {
    let s = Service::start(&[])?;
    push_status(&s, "1")?;
    assert_eq!(
        fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?.status,
        200
    );
    drop(s);
    let s = Service::start(&[])?;
    assert_eq!(
        push(&s, "status-reports", "1", STATUS_REPORTS, "{}")?.status,
        200
    );
    let reply = fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?;
    assert_eq!(
        (reply.status, reply.header("x-aac-decline")),
        (503, Some("stale"))
    );
    push_status(&s, "1")?;
    assert_eq!(
        fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?.status,
        200
    );
    Ok(())
}

/// A later generation is a different key. The kept view of the earlier one is not served for it,
/// and naming the earlier id after it was replaced declines `stale` instead of those bytes.
#[test]
fn a_new_status_generation_is_not_served_from_the_previous_view() -> Result<()> {
    let s = Service::start(&[])?;
    push_status(&s, "1")?;
    let first = fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?;
    assert_eq!(first.status, 200, "{}", first.body);
    assert!(first.body.contains("Fixture task"), "{}", first.body);
    let collected = STATUS_COLLECTED.replacen("Fixture task", "Other task", 1);
    let parts = [
        STATUS_REPORTS,
        "{}",
        "{}",
        collected.as_str(),
        STATUS_ANSWERS,
        STATUS_CONTEXT,
    ];
    let facts = status_facts(&parts, None)?.to_string();
    assert_eq!(
        push(&s, "status-collected", "2", &collected, "{}")?.status,
        200
    );
    assert_eq!(
        push(&s, "status-facts", "2", STATUS_CONTEXT, &facts)?.status,
        200
    );
    let ids = ["1", "1", "1", "2", "1", "2"];
    let second = fed_status(&s, &status_feed(ids, STATUS_NOW))?;
    assert_eq!(second.status, 200, "{}", second.body);
    assert!(second.body.contains("Other task"), "{}", second.body);
    assert!(
        !second.body.contains("Fixture task"),
        "the earlier generation's bytes: {}",
        second.body
    );
    assert_ne!(second.body, first.body);
    assert_ne!(second.header("etag"), first.header("etag"));
    // The same generations again, one millisecond later: the kept view, with this instant filled in.
    let again = fed_status(&s, &status_feed(ids, STATUS_NOW + 1))?;
    assert_eq!(again.status, 200, "{}", again.body);
    assert!(again.body.contains("Other task"), "{}", again.body);
    assert_ne!(again.body, second.body, "generatedAt moves");
    assert_eq!(
        again.header("etag"),
        second.header("etag"),
        "the identity does not include generatedAt"
    );
    // The replaced id is not answered from the cache.
    assert_eq!(
        fed_status(&s, &status_feed(["1"; 6], STATUS_NOW))?.header("x-aac-decline"),
        Some("stale")
    );
    Ok(())
}
