//! M3 part 3 account reads over the real private socket: the dashboard-session gate, file
//! reads that answer only proven forms (503 otherwise), no writes, and the two compositions.
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

const ROUTES: [&str; 4] = [
    "/api/accounts/visibility",
    "/api/accounts/preferences",
    "/api/accounts/registry",
    "/api/accounts/analytics",
];

#[test]
fn account_reads_need_a_dashboard_session() -> Result<()> {
    let s = Service::start()?;
    for path in ROUTES {
        for (kind, role) in [("session", "public"), ("device", "device"), ("hub", "hub")] {
            // No body: these routes answer 401 before reading it, and a body written under
            // load can surface as a broken pipe instead of the refusal (d7-home-T2).
            let (status, _) = s.call(path, &principal(kind, role), None)?;
            assert_eq!(status, 401, "{path} {kind}/{role}");
        }
        let (status, _) = s.call(&format!("{path}?x=1"), &principal("session", "admin"), None)?;
        assert_eq!(status, 503, "{path}: a query is never a proven read");
    }
    Ok(())
}

#[test]
fn visibility_answers_proven_files_and_never_writes() -> Result<()> {
    let s = Service::start()?;
    let (status, body) = s.admin("/api/accounts/visibility", None)?;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        r#"{"visibility":{"hiddenProviders":[],"hiddenAccountIds":[],"trayHiddenProviders":[],"trayHiddenAccountIds":[]},"folderWarnings":[]}"#
    );
    s.write(
        "account-visibility.json",
        r#"{"version":1,"hiddenProviders":["muse","gone","muse"],"hiddenAccountIds":["codex:work","codex:work","plan-opencode-go-console-mac-0123456789ab"]}"#,
        0o600,
    )?;
    let before = s.listing()?;
    let (status, body) = s.admin("/api/accounts/visibility", None)?;
    assert_eq!(status, 200);
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value["visibility"]["hiddenProviders"], json!(["muse"]));
    assert_eq!(
        value["visibility"]["hiddenAccountIds"],
        json!(["codex:work", "plan-opencode-go-console-mac-0123456789ab"])
    );
    fs::set_permissions(s.dir(), fs::Permissions::from_mode(0o770))?;
    let (_, body) = s.admin("/api/accounts/visibility", None)?;
    assert!(
        body.ends_with(r#""folderWarnings":["group-writable"]}"#),
        "{body}"
    );
    fs::set_permissions(s.dir(), fs::Permissions::from_mode(0o700))?;
    // A duplicate key reads as JSON.parse reads it: the last value at the first position.
    s.write(
        "account-visibility.json",
        r#"{"version":1,"hiddenProviders":["zai"],"hiddenAccountIds":[],"hiddenProviders":["qwen"]}"#,
        0o600,
    )?;
    let (status, body) = s.admin("/api/accounts/visibility", None)?;
    assert_eq!(status, 200);
    assert!(
        body.starts_with(r#"{"visibility":{"hiddenProviders":["qwen"],"#),
        "{body}"
    );
    for (text, mode) in [
        (
            r#"{"version":1,"hiddenProviders":[],"hiddenAccountIds":[]}"#,
            0o644,
        ),
        (
            r#"{"version":1,"hiddenProviders":[],"hiddenAccountIds":[],"x":1}"#,
            0o600,
        ),
        (
            r#"{"version":"1","hiddenProviders":[],"hiddenAccountIds":[]}"#,
            0o600,
        ),
        ("\u{feff}{}", 0o600),
    ] {
        s.write("account-visibility.json", text, mode)?;
        assert_eq!(s.admin("/api/accounts/visibility", None)?.0, 503, "{text}");
    }
    assert_eq!(s.listing()?.len(), before.len(), "no file was created");
    Ok(())
}

#[test]
fn preferences_answer_admitted_zones_and_defaults_only() -> Result<()> {
    let s = Service::start()?;
    let (status, body) = s.admin("/api/accounts/preferences", None)?;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        r#"{"timeZone":"America/New_York","snapshotCleanup":{"auto":true},"usageLogSources":[]}"#
    );
    let saved = r#"{"usageLogSources":[{"path":"/x","host":"nas1","tool":"muse","id":"m"}],"snapshotCleanup":{"auto":false},"timeZone":"Europe/Paris"}"#;
    s.write("dashboard-preferences.json", saved, 0o600)?;
    let (status, body) = s.admin("/api/accounts/preferences", None)?;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        r#"{"timeZone":"Europe/Paris","snapshotCleanup":{"auto":false},"usageLogSources":[{"path":"/x","host":"nas1","tool":"muse","id":"m"}]}"#
    );
    // The engine resolves this IANA name (an alias of Asia/Calcutta), so the native reader
    // answers it and keeps the spelling the file holds.
    let kolkata =
        r#"{"timeZone":"Asia/Kolkata","snapshotCleanup":{"auto":true},"usageLogSources":[]}"#;
    s.write("dashboard-preferences.json", kolkata, 0o600)?;
    let (status, body) = s.admin("/api/accounts/preferences", None)?;
    assert_eq!(status, 200, "{kolkata}");
    assert_eq!(body, kolkata);
    for text in [
        r#"{"timeZone":"UTC","snapshotCleanup":{"auto":true}"#,
        r#"{"timeZone":"Mars/Olympus_Mons","snapshotCleanup":{"auto":true},"usageLogSources":[]}"#,
        r#"{"timeZone":"UTC","snapshotCleanup":{"auto":true},"usageLogSources":[{"id":"j","tool":"jsonl","host":"mac","path":"/j","fieldMapping":{"timestamp":"t","model":"m"}}]}"#,
    ] {
        s.write("dashboard-preferences.json", text, 0o600)?;
        assert_eq!(s.admin("/api/accounts/preferences", None)?.0, 503, "{text}");
    }
    Ok(())
}

#[test]
fn compositions_answer_valid_inputs_and_refuse_the_rest() -> Result<()> {
    let s = Service::start()?;
    let registry = json!({
        "providers": [{"id":"zai","capabilities":{"replaceKey":true}}],
        "rows": [
            {"kind":"entry","row":{"id":"zai:acct:00000001","provider":"zai","email":null,"platform":"windows"},
             "entry":{"id":"zai:acct:00000001","provider":"zai","platform":"windows","sshHost":null,"label":null},
             "credentialKind":"aac-key","key":{"last4":"x7Qa","fingerprint":"sha256:00","storedOn":"windows"},
             "museSignIn":null,"running":false},
            {"kind":"other","row":{"id":"other-acct-0001","provider":"zai","label":"W","email":null,"platform":"mac"},"running":true}
        ],
        "jobs": [], "trash": []
    });
    let (status, body) = s.admin("/api/accounts/registry", Some(&registry.to_string()))?;
    assert_eq!(status, 200, "{body}");
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(value["accounts"][0]["label"], json!("zai"));
    assert_eq!(value["accounts"][0]["actions"]["replaceKey"], json!(true));
    assert_eq!(value["accounts"][1]["removeRefusal"], Value::Null);
    for bad in [
        "not json",
        "{}",
        r#"{"providers":[],"rows":[{"kind":"nope","row":{}}],"jobs":[],"trash":[]}"#,
        r#"{"providers":[],"rows":[],"rows":[],"jobs":[],"trash":[]}"#,
    ] {
        assert_eq!(
            s.admin("/api/accounts/registry", Some(bad))?.0,
            503,
            "{bad}"
        );
    }
    let range = json!({"preset":"7d","from":1_794_085_800_000_i64,"to":1_794_690_600_000_i64,"tz":"UTC","bucketMinutes":60});
    let data = json!({
        "query":{"platform":"mac","range":"7d","provider":"all","account":"all","tz":"UTC"},
        "now":1_794_690_600_000_i64,"retainedFrom":1_792_012_200_000_i64,"bounds":null,
        "oldestQuota":null,"coverageOldestHourAt":null,"provisional":range,
        "todayMidnight":1_794_614_400_000_i64,
        "expected":{"availableFrom":1_794_614_400_000_i64,"range":range},
        "tz":"UTC","resolution":null,"available":true,"stored":false,"history":null,
        "dashboard":{"accounts":[]},"providerTable":[{"id":"codex","label":"Codex","order":1}],
        "providersWithActivity":["codex"]
    });
    // The shared presentation envelope: ISO words only (no clock/day), one ranked id.
    let presentation = json!({
        "dates": {
            "1794690600000": {"iso":"2026-11-14T21:10:00.000Z"},
            "1794085800000": {"iso":"2026-11-07T21:10:00.000Z"},
            "1794614400000": {"iso":"2026-11-14T00:00:00.000Z"}
        },
        "epochs": {},
        "order": {"codex": 0}
    });
    let analytics = json!({"data": data, "presentation": presentation});
    let (status, body) = s.admin("/api/accounts/analytics", Some(&analytics.to_string()))?;
    assert_eq!(status, 200, "{body}");
    let value: Value = serde_json::from_str(&body)?;
    assert_eq!(
        value["range"]["availableFrom"],
        json!("2026-11-14T00:00:00.000Z")
    );
    assert_eq!(value["providers"][0]["hasActivity"], json!(true));
    let mut wrong = analytics.clone();
    wrong["data"]["expected"]["availableFrom"] = json!(0);
    assert_eq!(
        s.admin("/api/accounts/analytics", Some(&wrong.to_string()))?
            .0,
        503,
        "a range TS did not compute is refused"
    );
    let mut unproven = analytics.clone();
    unproven["presentation"]["dates"] = json!({});
    assert_eq!(
        s.admin("/api/accounts/analytics", Some(&unproven.to_string()))?
            .0,
        503,
        "an ISO word without its engine fact is refused"
    );
    Ok(())
}
