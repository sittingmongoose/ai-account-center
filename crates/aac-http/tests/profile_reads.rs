//! M3 part 4 profile reads over the real private socket: the device-scope and dashboard-session
//! gates, the Claude desktop manifest read through the capped no-follow, nonblocking
//! descriptor (503 for anything it cannot prove, a FIFO included, and never a write), the
//! hardened part 3 file reads, and the five compositions over TS-read inputs.
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

fn principal(kind: &str, role: &str) -> String {
    json!({"kind":kind,"id":if role == "public" {"public-health"} else {"fixture-admin"},"role":role}).to_string()
}

impl Service {
    fn start() -> Result<Self> {
        let root = tempfile::tempdir()?;
        fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700))?;
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
        let service = Self {
            child,
            root,
            socket,
        };
        // The socket exists before chmod; wait until it answers.
        let deadline = Instant::now() + Duration::from_secs(5);
        while service
            .call("/api/health", &principal("session", "public"), None)
            .map_or(true, |(status, _)| status != 200)
        {
            if Instant::now() >= deadline {
                return Err("service did not answer".into());
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        Ok(service)
    }

    fn dir(&self) -> &Path {
        self.root.path()
    }

    fn call(&self, path: &str, principal: &str, body: Option<&str>) -> Result<(u16, String)> {
        let mut stream = UnixStream::connect(&self.socket)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        write!(
            stream,
            "GET {path} HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {}\r\nx-aac-principal: {principal}\r\n",
            "b".repeat(64)
        )?;
        if let Some(body) = body {
            write!(
                stream,
                "content-type: application/json\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )?;
        } else {
            write!(stream, "\r\n")?;
        }
        let mut result = String::new();
        stream.read_to_string(&mut result)?;
        let status: u16 = result.get(9..12).ok_or("no status")?.parse()?;
        let body = result.split_once("\r\n\r\n").ok_or("no body")?.1.to_owned();
        Ok((status, body))
    }

    fn admin(&self, path: &str, body: Option<&str>) -> Result<(u16, String)> {
        self.call(path, &principal("session", "admin"), body)
    }

    fn write(&self, name: &str, text: &str, mode: u32) -> Result<()> {
        let file = self.dir().join(name);
        fs::write(&file, text)?;
        fs::set_permissions(&file, fs::Permissions::from_mode(mode))?;
        Ok(())
    }

    fn listing(&self) -> Result<Vec<(String, u64, u32)>> {
        let mut entries = Vec::new();
        for entry in fs::read_dir(self.dir())? {
            let entry = entry?;
            let meta = entry.metadata()?;
            entries.push((
                entry.file_name().to_string_lossy().into_owned(),
                meta.len(),
                meta.permissions().mode(),
            ));
        }
        entries.sort();
        Ok(entries)
    }
}

const DEVICE_ROUTES: [&str; 2] = ["/api/claude/desktop-profiles", "/api/antigravity/profiles"];
const SESSION_ROUTES: [&str; 4] = [
    "/api/claude/desktop-profiles/usage",
    "/api/codex/profiles",
    "/api/codex/profiles/quotas",
    "/api/antigravity/profiles/quotas",
];

fn fifo(path: &Path) -> Result<()> {
    let status = Command::new("mkfifo").arg(path).status()?;
    if !status.success() {
        return Err("mkfifo failed".into());
    }
    Ok(())
}

#[test]
fn profile_reads_keep_the_ts_scopes() -> Result<()> {
    let s = Service::start()?;
    for path in DEVICE_ROUTES.iter().chain(SESSION_ROUTES.iter()) {
        for (kind, role) in [("session", "public"), ("hub", "hub")] {
            let (status, _) = s.call(path, &principal(kind, role), Some("{}"))?;
            assert_eq!(status, 401, "{path} {kind}/{role}");
        }
        let (status, _) = s.call(
            &format!("{path}?refresh=true"),
            &principal("session", "admin"),
            None,
        )?;
        assert_eq!(status, 503, "{path}: a query is never a proven read");
    }
    // A paired device reaches only the two tray routes, as the TS /api guard's device scope.
    for path in SESSION_ROUTES {
        assert_eq!(
            s.call(path, &principal("device", "device"), Some("{}"))?.0,
            401,
            "{path}"
        );
    }
    let (status, body) = s.call(DEVICE_ROUTES[0], &principal("device", "device"), None)?;
    assert_eq!((status, body.as_str()), (200, r#"{"profiles":[]}"#));
    assert_eq!(
        s.call(DEVICE_ROUTES[1], &principal("device", "device"), Some("{}"))?
            .0,
        503
    );
    Ok(())
}

#[test]
fn the_desktop_manifest_is_read_through_the_capped_descriptor_only() -> Result<()> {
    let s = Service::start()?;
    let path = "/api/claude/desktop-profiles";
    let manifest = r#"{"version":1,"profiles":[{"id":"work","email":"w@example.com","mac":{"launcherName":"Claude Wörk","launcherPath":"/A.app","sshHost":"mac"},"windows":{"launcherName":"W"}}]}"#;
    s.write("claude-desktop-profiles.json", manifest, 0o644)?;
    let before = s.listing()?;
    let (status, body) = s.admin(path, None)?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"profiles":[{"id":"work","email":"w@example.com","mac":{"launcherName":"Claude Wörk","launcherPath":"/A.app","canOpen":true},"windows":{"launcherName":"W","canOpen":false,"launchUri":"ccs-claude://launch/work"}}]}"#
    );
    assert_eq!(s.listing()?, before, "a read never writes");
    // TS follows a symlink and may accept a larger multi-byte manifest: both are TS's.
    let file = s.dir().join("claude-desktop-profiles.json");
    fs::remove_file(&file)?;
    s.write("target.json", manifest, 0o600)?;
    std::os::unix::fs::symlink(s.dir().join("target.json"), &file)?;
    assert_eq!(s.admin(path, None)?.0, 503, "symlink");
    fs::remove_file(&file)?;
    s.write(
        "claude-desktop-profiles.json",
        &format!(
            "{{\"version\":1,\"profiles\":[],\"pad\":\"{}\"}}",
            "x".repeat(1 << 20)
        ),
        0o600,
    )?;
    assert_eq!(s.admin(path, None)?.0, 503, "over the cap");
    fs::remove_file(&file)?;
    fs::create_dir(&file)?;
    assert_eq!(s.admin(path, None)?.0, 503, "folder");
    fs::remove_dir(&file)?;
    // A FIFO is opened without blocking and refused on its descriptor, never read.
    fifo(&file)?;
    let started = Instant::now();
    assert_eq!(s.admin(path, None)?.0, 503, "fifo");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the FIFO open blocked"
    );
    for bad in [
        r#"{"version":1,"profiles":[{"email":"x"}]}"#,
        "\u{feff}{}",
        "{",
    ] {
        fs::remove_file(&file)?;
        s.write("claude-desktop-profiles.json", bad, 0o600)?;
        assert_eq!(s.admin(path, None)?.0, 503, "{bad}");
    }
    Ok(())
}

#[test]
fn part_three_files_use_the_same_descriptor_checks() -> Result<()> {
    let s = Service::start()?;
    let saved =
        r#"{"timeZone":"Europe/Paris","snapshotCleanup":{"auto":false},"usageLogSources":[]}"#;
    for (name, route) in [
        ("account-visibility.json", "/api/accounts/visibility"),
        ("dashboard-preferences.json", "/api/accounts/preferences"),
    ] {
        fifo(&s.dir().join(name))?;
        let started = Instant::now();
        assert_eq!(s.admin(route, None)?.0, 503, "{name} as a FIFO");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{name}: the FIFO open blocked"
        );
        fs::remove_file(s.dir().join(name))?;
    }
    s.write("dashboard-preferences.json", saved, 0o600)?;
    let companion = s.dir().join("dashboard-preferences-extra-sources.json");
    fifo(&companion)?;
    assert_eq!(
        s.admin("/api/accounts/preferences", None)?.0,
        503,
        "a FIFO companion"
    );
    fs::remove_file(&companion)?;
    s.write(
        "dashboard-preferences-extra-sources.json",
        &format!(
            "{{\"usageLogSources\":[],\"pad\":\"{}\"}}",
            "x".repeat(70_000)
        ),
        0o600,
    )?;
    let (status, body) = s.admin("/api/accounts/preferences", None)?;
    assert_eq!(
        (status, body.as_str()),
        (200, saved),
        "an oversized companion keeps the primary"
    );
    Ok(())
}

#[test]
fn profile_compositions_answer_valid_inputs_and_refuse_the_rest() -> Result<()> {
    let s = Service::start()?;
    let summary = json!({
        "instancesDir": "/h/.ccs/codex-instances",
        "env": {"codexHome": null, "profile": " work "},
        "registry": {"default": null, "names": ["work"]},
        "profiles": [{"name":"work","codexHome":"/h/.ccs/codex-instances/work","email":null,"plan":null,"accountId":null,"lastUsed":null,"authValid":false}],
        "activated": null
    });
    let (status, body) = s.admin("/api/codex/profiles", Some(&summary.to_string()))?;
    assert_eq!(status, 200, "{body}");
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(
        value["active"],
        json!({"name":"work","source":"env","codexHome":"/h/.ccs/codex-instances/work"})
    );
    let mut stale = summary.clone();
    stale["env"]["profile"] = json!("gone");
    assert_eq!(
        s.admin("/api/codex/profiles", Some(&stale.to_string()))?.0,
        503,
        "TS logs a stale profile"
    );

    let quotas = json!({
        "data": {
            "profiles": [{"name":"work","authValid":true}],
            "rows": [{"profile":"work","quotaStatus":"ok","fetchedAt":"2026-11-15T09:00:00Z",
                      "quotaWindows":[{"key":"five_hour","label":"5h","usedPercent":25,"resetAt":"2020-01-01T00:00:00Z"}]}],
            "renewals": [], "now": 1_794_744_000_000_i64
        },
        "presentation": {"dates": {}, "epochs": {"2020-01-01T00:00:00Z": 1_577_836_800_000_i64, "2026-11-15T09:00:00Z": 1_794_733_200_000_i64}, "order": {}}
    });
    let (status, body) = s.admin("/api/codex/profiles/quotas", Some(&quotas.to_string()))?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"profiles":[{"profileName":"work","status":"available","windows":[{"key":"five_hour","label":"5-hour limit","usedPercent":25,"resetsAt":"2020-01-01T00:00:00Z"}],"fetchedAt":"2026-11-15T09:00:00Z"}]}"#,
        "a reading taken after its reset is current"
    );
    let mut legacy = quotas.clone();
    legacy["data"]["rows"][0]["quotaWindows"][0]["resetAt"] = json!("Jan 1 2020");
    assert_eq!(
        s.admin("/api/codex/profiles/quotas", Some(&legacy.to_string()))?
            .0,
        503,
        "a legacy date parse is TS's"
    );

    let inventory = json!({
        "data": {"schemaVersion":1,"hostId":"ubuntu","profiles":[{"id":"a","email":" a@example.com ","plan":"sk-x","hostId":"ubuntu","available":true,"verifiedAt":"2026-11-15T13:30:00+05:30"}]},
        "presentation": {"dates": {"1794729600000": {"iso":"2026-11-15T08:00:00.000Z"}}, "epochs": {"2026-11-15T13:30:00+05:30": 1_794_729_600_000_i64}, "order": {}}
    });
    let (status, body) = s.admin("/api/antigravity/profiles", Some(&inventory.to_string()))?;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        body,
        r#"{"schemaVersion":1,"hostId":"ubuntu","profiles":[{"id":"a","email":"a@example.com","plan":null,"hostId":"ubuntu","available":true,"selected":false,"verifiedAt":"2026-11-15T08:00:00.000Z"}]}"#
    );
    let mut missing = inventory.clone();
    missing["presentation"]["dates"] = json!({});
    assert_eq!(
        s.admin("/api/antigravity/profiles", Some(&missing.to_string()))?
            .0,
        503,
        "an ISO word without its fact"
    );

    let usage = json!({"platform":"mac","fetchedAt":"F","profiles":[{"email":"a@example.com","read":"read","history":"{\"version\":2,\"samples\":[{\"t\":0,\"u\":{\"fh\":1}}]}"}]});
    let (status, body) = s.admin(
        "/api/claude/desktop-profiles/usage",
        Some(&usage.to_string()),
    )?;
    assert_eq!(status, 200, "{body}");
    assert!(
        body.contains(r#""sampledAt":"1970-01-01T00:00:00.000Z","utilization":{"fiveHour":1}"#),
        "{body}"
    );

    for (path, bad) in [
        ("/api/antigravity/profiles/quotas", "[]"),
        (
            "/api/antigravity/profiles/quotas",
            r#"{"data":{},"presentation":{"dates":{},"epochs":{},"order":{}},"x":1}"#,
        ),
        ("/api/codex/profiles", r#"{"instancesDir":"/x"}"#),
        (
            "/api/claude/desktop-profiles/usage",
            r#"{"platform":"mac","fetchedAt":"F","profiles":[],"profiles":[]}"#,
        ),
    ] {
        assert_eq!(s.admin(path, Some(bad))?.0, 503, "{path} {bad}");
    }
    Ok(())
}
