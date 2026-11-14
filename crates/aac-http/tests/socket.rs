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
        write!(stream, "\r\n")?;
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
