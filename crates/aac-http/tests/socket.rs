//! Real child-process checks of the private boundary; all data is temporary.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    error::Error,
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
type Result<T> = std::result::Result<T, Box<dyn Error>>;

struct Service {
    child: Child,
    root: tempfile::TempDir,
    socket: PathBuf,
}
impl Drop for Service {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Service {
    fn start() -> Result<Self> {
        let root = tempfile::tempdir()?;
        let run = root.path().join("run");
        let directory = run.join(format!("aac-http-{}", "a".repeat(24)));
        fs::create_dir_all(&directory)?;
        for p in [&run, &directory] {
            fs::set_permissions(p, fs::Permissions::from_mode(0o700))?;
        }
        let socket = directory.join("http.sock");
        let mut child = Command::new(env!("CARGO_BIN_EXE_aac-http"))
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        let boot = json!({"ccsDir":fs::canonicalize(root.path())?,"socketPath":socket,"secret":"b".repeat(64),"bootId":"a".repeat(24)});
        writeln!(child.stdin.as_mut().ok_or("stdin missing")?, "{boot}")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() {
            if child.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err("service failed to start".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // Socket metadata can exist briefly before chmod, but it cannot be accepted before chmod.
        let service = Self {
            child,
            root,
            socket,
        };
        assert!(
            service
                .request(
                    "/api/health",
                    Some(&"b".repeat(64)),
                    Some(&principal("session", "public")),
                    "GET"
                )?
                .starts_with("HTTP/1.1 200")
        );
        Ok(service)
    }
    fn request(
        &self,
        path: &str,
        secret: Option<&str>,
        principal: Option<&str>,
        method: &str,
    ) -> Result<String> {
        self.request_input(path, secret, principal, method, None)
    }
    fn request_input(
        &self,
        path: &str,
        secret: Option<&str>,
        principal: Option<&str>,
        method: &str,
        input: Option<&Value>,
    ) -> Result<String> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(2)))?;
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\nHost: private\r\nConnection: close\r\n"
        )?;
        if let Some(s) = secret {
            write!(stream, "x-aac-boot-secret: {s}\r\n")?;
        }
        if let Some(p) = principal {
            write!(stream, "x-aac-principal: {p}\r\n")?;
        }
        if let Some(input) = input {
            let body = serde_json::to_vec(input)?;
            write!(
                stream,
                "content-type: application/json\r\ncontent-length: {}\r\n\r\n",
                body.len()
            )?;
            stream.write_all(&body)?;
        } else {
            write!(stream, "\r\n")?;
        }
        let mut result = String::new();
        stream.read_to_string(&mut result)?;
        Ok(result)
    }
    fn body(&self, path: &str) -> Result<Value> {
        let result = self.request(
            path,
            Some(&"b".repeat(64)),
            Some(&principal("session", "admin")),
            "GET",
        )?;
        assert!(result.starts_with("HTTP/1.1 200"), "{result}");
        Ok(serde_json::from_str(
            result.split_once("\r\n\r\n").ok_or("no body")?.1,
        )?)
    }
}
fn principal(kind: &str, role: &str) -> String {
    json!({"kind":kind,"id":if role == "public" {"public-health"} else {"fixture-admin"},"role":role}).to_string()
}

#[test]
fn unsigned_socket_requests_are_401_and_permissions_are_private() -> Result<()> {
    let s = Service::start()?;
    assert_eq!(fs::metadata(&s.socket)?.permissions().mode() & 0o777, 0o600);
    for path in [
        "/api/health",
        "/api/accounts/settings",
        "/api/diag",
        "/api/views/home",
        "/missing",
    ] {
        for (secret, p) in [
            (None, None),
            (Some("wrong".into()), Some(principal("session", "admin"))),
            (Some("b".repeat(64)), None),
            (None, Some(principal("session", "admin"))),
            (Some("b".repeat(64)), Some("{}".into())),
            (
                Some("b".repeat(64)),
                Some(r#"["session","fixture-admin","admin"]"#.into()),
            ),
            (Some("b".repeat(64)), Some("null".into())),
            (
                Some("b".repeat(64)),
                Some(r#"{"kind":{"session":null},"id":"fixture-admin","role":"admin"}"#.into()),
            ),
            (
                Some("b".repeat(64)),
                Some(r#"{"kind":"session","id":"fixture-admin","role":{"admin":null}}"#.into()),
            ),
            (
                Some("b".repeat(64)),
                Some(
                    r#"{"kind":{"session":null},"id":"fixture-admin","role":{"admin":null}}"#
                        .into(),
                ),
            ),
            (
                Some("b".repeat(64)),
                Some(r#"{"kind":"session","id":"fixture-admin","role":"admin","extra":0}"#.into()),
            ),
            (
                Some("b".repeat(64)),
                Some(
                    r#"{"kind":"session","id":"fixture-admin","role":"admin","role":"admin"}"#
                        .into(),
                ),
            ),
            (Some("b".repeat(64)), Some(principal("hub", "admin"))),
        ] {
            let r = s.request(path, secret.as_deref(), p.as_deref(), "GET")?;
            assert!(r.starts_with("HTTP/1.1 401"), "{path}: {r}");
        }
    }
    for (kind, role) in [("session", "public"), ("device", "device"), ("hub", "hub")] {
        let p = principal(kind, role);
        assert!(
            s.request("/api/health", Some(&"b".repeat(64)), Some(&p), "GET")?
                .starts_with("HTTP/1.1 200")
        );
        for path in ["/api/accounts/settings", "/api/diag"] {
            assert!(
                s.request(path, Some(&"b".repeat(64)), Some(&p), "GET")?
                    .starts_with("HTTP/1.1 401")
            );
        }
    }
    let duplicate = format!(
        "{}\r\nx-aac-principal: {}",
        principal("session", "public"),
        principal("session", "admin")
    );
    assert!(
        s.request(
            "/api/health",
            Some(&"b".repeat(64)),
            Some(&duplicate),
            "GET"
        )?
        .starts_with("HTTP/1.1 401")
    );
    assert!(
        s.request(
            "/api/health",
            Some(&"b".repeat(64)),
            Some(&principal("session", "admin")),
            "POST"
        )?
        .starts_with("HTTP/1.1 405")
    );
    Ok(())
}

#[test]
fn settings_table_matches_proven_ts_values_or_falls_back_without_writes() -> Result<()> {
    let s = Service::start()?;
    let file = s.root.path().join("account-refresh-settings.json");
    let rows: Vec<Value> = serde_json::from_str(include_str!(
        "../../../tests/spec/settings-numeric-forms.json"
    ))?;
    let expect_fallback = || -> Result<()> {
        let reply = s.request(
            "/api/accounts/settings",
            Some(&"b".repeat(64)),
            Some(&principal("session", "admin")),
            "GET",
        )?;
        assert!(reply.starts_with("HTTP/1.1 503"), "{reply}");
        Ok(())
    };
    for row in rows {
        let bytes = row["file"].as_str().ok_or("missing file bytes")?;
        fs::write(&file, bytes)?;
        if row["owner"] == "rust" {
            assert_eq!(
                s.body("/api/accounts/settings")?,
                json!({"refreshIntervalSeconds":row["ts"]}),
                "{}",
                row["name"]
            );
        } else {
            expect_fallback()?;
        }
        assert_eq!(fs::read_to_string(&file)?, bytes);
    }
    fs::write(&file, " ".repeat(4097))?;
    expect_fallback()?;
    fs::remove_file(&file)?;
    let target = s.root.path().join("do-not-read.json");
    fs::write(&target, "{\"refreshIntervalSeconds\":99}")?;
    std::os::unix::fs::symlink(&target, &file)?;
    expect_fallback()?;
    fs::remove_file(&file)?;
    expect_fallback()?;
    let diag = s.body("/api/diag")?;
    assert_eq!(diag["store"]["status"], "unavailable");
    assert_eq!(diag["scheduler"]["lastSuccessAt"], Value::Null);
    assert_eq!(diag["jobs"], json!([]));
    Ok(())
}

#[cfg(target_os = "linux")]
#[test]
fn process_opens_no_tcp_socket() -> Result<()> {
    let s = Service::start()?;
    let mut inodes = Vec::new();
    for entry in fs::read_dir(format!("/proc/{}/fd", s.child.id()))? {
        if let Ok(target) = fs::read_link(entry?.path()) {
            let text = target.to_string_lossy();
            if let Some(id) = text
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
            {
                inodes.push(id.to_owned());
            }
        }
    }
    assert!(!inodes.is_empty(), "must observe the Unix listener socket");
    for table in ["tcp", "tcp6"] {
        for line in fs::read_to_string(format!("/proc/{}/net/{table}", s.child.id()))?
            .lines()
            .skip(1)
        {
            let columns: Vec<_> = line.split_whitespace().collect();
            assert!(
                !columns
                    .get(9)
                    .is_some_and(|id| inodes.iter().any(|i| i == id)),
                "Rust opened TCP: {line}"
            );
        }
    }
    Ok(())
}

#[test]
fn read_routes_use_the_existing_writer_snapshot_without_opening_another_writer() -> Result<()> {
    let service = Service::start()?;
    let directory = service.root.path().join("store");
    let writer =
        aac_store::ops::open_engine(directory.to_str().ok_or("path")?, Some(8 << 20), None)
            .map_err(|e| e.to_string())?;
    let snapshot = aac_store::ops::call(&writer, "status", "{}").map_err(|e| e.to_string())?;
    let before = fs::read(directory.join("aac.redb"))?;
    let result = service.request_input(
        "/api/store/status",
        Some(&"b".repeat(64)),
        Some(&principal("session", "admin")),
        "GET",
        Some(&json!({"snapshot": snapshot})),
    )?;
    assert!(result.starts_with("HTTP/1.1 200"), "{result}");
    assert_eq!(
        serde_json::from_str::<Value>(result.split("\r\n\r\n").nth(1).ok_or("body")?.trim())?,
        snapshot
    );
    assert_eq!(fs::read(directory.join("aac.redb"))?, before);
    // A real competing writer is still refused: the service did not replace/release the owner.
    assert!(aac_store::ops::open_engine(directory.to_str().ok_or("path")?, None, None).is_err());
    let unsupported = service.request_input(
        "/api/store/status",
        Some(&"b".repeat(64)),
        Some(&principal("session", "admin")),
        "GET",
        Some(&json!({"snapshot":{"state":"ready","fixture":true}})),
    )?;
    assert!(unsupported.starts_with("HTTP/1.1 503"), "{unsupported}");
    writer.close().map_err(|e| e.to_string())?;
    Ok(())
}

#[test]
fn bounded_file_reads_are_confined_and_never_follow_a_writer_root_symlink() -> Result<()> {
    let service = Service::start()?;
    let root = service.root.path().join("machine");
    fs::create_dir_all(root.join("share"))?;
    fs::write(root.join("share/test.txt"), b"fixture")?;
    let input = json!({"root":root,"writer":null,"path":"share/test.txt","start":1,"length":3,"head":false});
    let request = |input: &Value| {
        service.request_input(
            "/api/status/deliverable",
            Some(&"b".repeat(64)),
            Some(&principal("session", "admin")),
            "GET",
            Some(input),
        )
    };
    let result = request(&input)?;
    assert!(result.starts_with("HTTP/1.1 200"), "{result}");
    assert_eq!(
        serde_json::from_str::<Value>(result.split("\r\n\r\n").nth(1).ok_or("body")?.trim())?,
        json!({"size":7,"bytes":"aXh0"})
    );
    for path in [
        "share/../private.txt",
        "share/.hidden",
        "share/test.txt%00",
        "/share/test.txt",
    ] {
        let mut invalid = input.clone();
        invalid["path"] = json!(path);
        assert!(request(&invalid)?.starts_with("HTTP/1.1 503"));
    }
    let outside = service.root.path().join("outside");
    fs::create_dir(&outside)?;
    fs::write(outside.join("test.txt"), b"outside")?;
    fs::rename(root.join("share"), root.join("saved"))?;
    std::os::unix::fs::symlink(&outside, root.join("share"))?;
    assert!(request(&input)?.starts_with("HTTP/1.1 503"));
    Ok(())
}

/// D7: the front door's envelope for one case of tests/spec/views/home/cases.json, and its JS view.
fn home_case(name: &str) -> Result<(Value, Value)> {
    let table: Value =
        serde_json::from_str(include_str!("../../../tests/spec/views/home/cases.json"))?;
    let case = table["cases"]
        .as_array()
        .ok_or("no cases")?
        .iter()
        .find(|c| c["name"] == name)
        .ok_or("no such case")?;
    let envelope = json!({
        "data": {
            "ctx": case["ctx"],
            "dashboard": case["dashboard"],
            "profiles": case["profiles"],
            "antigravity": case["antigravity"],
        },
        "presentation": case["presentation"],
    });
    let expected = match case["expected"].as_str() {
        Some(text) => serde_json::from_str(text)?,
        None => Value::Null,
    };
    Ok((envelope, expected))
}

#[test]
fn home_view_is_the_js_view_or_the_shared_read_fallback() -> Result<()> {
    let s = Service::start()?;
    let secret = "b".repeat(64);
    let session = principal("session", "admin");
    let home = |path: &str, principal: Option<&str>, method: &str, input: Option<&Value>| {
        s.request_input(path, Some(&secret), principal, method, input)
    };
    let split = |reply: &str| -> Result<(String, String)> {
        let (head, body) = reply.split_once("\r\n\r\n").ok_or("no body")?;
        Ok((
            head.lines().next().unwrap_or("").to_owned(),
            body.to_owned(),
        ))
    };
    // The JS view, value for value (the front door re-serializes it, so JSON number spelling is its own).
    for name in [
        "view-model#1",
        "zone/Pacific/Chatham#1",
        "probe/narrow",
        "probe/safari",
        "edge/hidden",
        "edge/number-control",
    ] {
        let (envelope, expected) = home_case(name)?;
        let (status, got) = split(&home(
            "/api/views/home",
            Some(&session),
            "GET",
            Some(&envelope),
        )?)?;
        assert_eq!(status, "HTTP/1.1 200 OK", "{name}");
        assert_eq!(serde_json::from_str::<Value>(&got)?, expected, "{name}");
    }
    // Refusals and inputs the front door never sends take the read routes' one fallback.
    let (envelope, _) = home_case("view-model#1")?;
    let mut extra = envelope.clone();
    extra["data"]["more"] = json!(1);
    let mut unranked = envelope.clone();
    unranked["presentation"]["dates"] = json!({});
    let mut no_ctx = envelope.clone();
    no_ctx["data"]["ctx"] = json!({});
    let mut refused = vec![
        extra,
        unranked,
        no_ctx,
        json!([]),
        json!({}),
        json!({"data": envelope["data"]}),
    ];
    for name in ["decline/facts", "decline/locale", "decline/open-progress"] {
        refused.push(home_case(name)?.0);
    }
    // Big review M4/L4: amounts outside the proved number domain (the reviewers' tie witnesses and
    // both neighbouring doubles) are never served; the front door builds the page's own view.
    for label in ["1e14", "2^47", "2^51"] {
        for i in 0..3 {
            refused.push(home_case(&format!("decline/number-{label}#{i}"))?.0);
        }
    }
    for input in &refused {
        let (status, got) = split(&home(
            "/api/views/home",
            Some(&session),
            "GET",
            Some(input),
        )?)?;
        assert_eq!(status, "HTTP/1.1 503 Service Unavailable", "{input:.80}");
        assert_eq!(
            serde_json::from_str::<Value>(&got)?,
            json!({"error":"TypeScript read fallback required"})
        );
    }
    assert!(home("/api/views/home", Some(&session), "GET", None)?.starts_with("HTTP/1.1 503"));
    assert!(
        home(
            "/api/views/home?refresh=true",
            Some(&session),
            "GET",
            Some(&envelope)
        )?
        .starts_with("HTTP/1.1 503"),
        "a query is not the read the front door proved"
    );
    // Only a verified dashboard session; GET only. These answer before any body is read, so none is sent (the
    // service may close while a body is still being written).
    for p in [
        principal("device", "device"),
        principal("hub", "hub"),
        principal("session", "public"),
    ] {
        assert!(
            home("/api/views/home", Some(&p), "GET", None)?.starts_with("HTTP/1.1 401"),
            "{p}"
        );
    }
    assert!(home("/api/views/home", None, "GET", None)?.starts_with("HTTP/1.1 401"));
    assert!(home("/api/views/home", Some(&session), "POST", None)?.starts_with("HTTP/1.1 405"));
    Ok(())
}
