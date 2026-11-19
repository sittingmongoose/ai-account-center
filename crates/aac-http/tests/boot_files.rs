//! L2: the boot JSON carries each private-file registry row's writer as TS resolved it at start.
//! The service starts only when that map names exactly the registry's rows and gives the service
//! no row that does not move with a route group. Real binary, real socket, temporary data only.
//! Failed against the base (12b4a400c): a boot with `files` never started.
#![cfg(unix)]
#![allow(clippy::unwrap_used)]
use serde_json::{Map, Value, json};
use std::{
    fs,
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::Path,
    process::{Command, Stdio},
    time::{Duration, Instant},
};

const SECRET: &str = "dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd";
const BOOT_ID: &str = "0123456789abcdef01234567";

fn private(path: &Path) {
    fs::create_dir_all(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

/// Start the service with `files` (or none) and report whether it answered health.
fn starts(files: Option<Value>) -> bool {
    let dir = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(dir.path()).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let boot = root.join("run").join(format!("aac-http-{BOOT_ID}"));
    private(&root.join("run"));
    private(&boot);
    let socket = boot.join("http.sock");
    let mut child = Command::new(env!("CARGO_BIN_EXE_aac-http"))
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env_clear()
        .spawn()
        .unwrap();
    let mut line = json!({"ccsDir":root,"socketPath":socket,"secret":SECRET,"bootId":BOOT_ID});
    if let Some(files) = files {
        line["files"] = files;
    }
    writeln!(child.stdin.as_mut().unwrap(), "{line}").unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let healthy = loop {
        if child.try_wait().unwrap().is_some() {
            break false;
        }
        if socket.exists() && health(&socket) {
            break true;
        }
        if Instant::now() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let _ = child.kill();
    let _ = child.wait();
    healthy
}

fn health(socket: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let principal = json!({"kind":"session","id":"public-health","role":"public"});
    let _ = write!(
        stream,
        "GET /api/health HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {SECRET}\r\nx-aac-principal: {principal}\r\n\r\n"
    );
    let mut raw = String::new();
    let _ = stream.read_to_string(&mut raw);
    raw.starts_with("HTTP/1.1 200")
}

fn owners() -> Map<String, Value> {
    let registry: Value =
        serde_json::from_str(include_str!("../../../config/private-files.json")).unwrap();
    registry["rows"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| (row["id"].as_str().unwrap().to_owned(), row["owner"].clone()))
        .collect()
}

#[test]
fn the_service_starts_only_with_the_registrys_owners() {
    // Without the map (a test harness), and with TS's resolved map: every row's own owner.
    assert!(starts(None));
    assert!(starts(Some(Value::Object(owners()))));
    let mut settings = owners();
    settings.insert("account-refresh-settings".into(), json!("rust-service"));
    assert!(starts(Some(Value::Object(settings))));
    // A row missing, an unknown row, a row given to the service that moves with no group, or
    // an owner word the registry does not know: the boot is refused.
    let mut missing = owners();
    missing.remove("account-keys");
    let mut unknown = owners();
    unknown.insert("zz-unknown".into(), json!("ts"));
    let mut moved = owners();
    moved.insert("dashboard-preferences".into(), json!("rust-service"));
    let mut word = owners();
    word.insert("account-visibility".into(), json!("python"));
    for (name, files) in [
        ("missing", missing),
        ("unknown", unknown),
        ("moved", moved),
        ("word", word),
    ] {
        assert!(!starts(Some(Value::Object(files))), "{name}");
    }
    assert!(!starts(Some(json!(["ts"]))));
}
