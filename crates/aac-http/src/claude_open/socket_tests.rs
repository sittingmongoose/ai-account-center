//! G13 socket-level dispatch: the internal Open and cleanup routes over a real private
//! Unix socket — the unbound handoff, admission, the bound owner's refusals and totals, and
//! the GET profiles progress field. The L5 transport is deliberately unconfigured, so the
//! remote boundary answers as it does on a host without an agent: the orchestrator's
//! refusal mapping is what these cases prove. Disposable 0700 roots only.
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct BootFixture {
    root: PathBuf,
    socket: PathBuf,
    secret: String,
    boot_id: String,
}

impl BootFixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        std::mem::forget(dir);
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let run = root.join("run");
        std::fs::create_dir(&run).expect("run");
        std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        let boot_id = "abcdefabcdefabcdefabcdef".to_string();
        let folder = run.join(format!("aac-http-{boot_id}"));
        std::fs::create_dir(&folder).expect("boot folder");
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        Self {
            socket: folder.join("http.sock"),
            root,
            secret: "c".repeat(64),
            boot_id,
        }
    }

    fn boot(&self) -> crate::Boot {
        let raw = json!({
            "ccsDir": self.root, "socketPath": self.socket,
            "secret": self.secret, "bootId": self.boot_id
        });
        crate::Boot::from_json(serde_json::to_string(&raw).expect("json").as_bytes())
            .expect("boot parses")
    }

    fn write_manifest(&self, profiles: Value) {
        let file = self.root.join("claude-desktop-profiles.json");
        std::fs::write(
            &file,
            serde_json::to_vec(&json!({"version": 1, "profiles": profiles})).expect("json"),
        )
        .expect("write");
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).expect("chmod");
    }
}

impl Drop for BootFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

const SERVICE_PRINCIPAL: &str = r#"{"kind":"session","id":"front-door","role":"service"}"#;
const ADMIN_PRINCIPAL: &str = r#"{"kind":"session","id":"fixture-admin","role":"admin"}"#;

async fn start(
    fixture: &BootFixture,
    bound: bool,
) -> (tokio::sync::oneshot::Sender<()>, tokio::task::JoinHandle<()>) {
    let (stop, shutdown) = tokio::sync::oneshot::channel::<()>();
    let boot = fixture.boot();
    let package = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf();
    let handle = tokio::spawn(async move {
        let shutdown = async move {
            let _ = shutdown.await;
        };
        let result = if bound {
            crate::serve_with_claude_open(boot, shutdown, package).await
        } else {
            crate::serve(boot, shutdown).await
        };
        result.expect("serve");
    });
    for _ in 0..400 {
        if fixture.socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert!(fixture.socket.exists(), "socket never appeared");
    (stop, handle)
}

async fn http(
    socket: &Path,
    method: &str,
    path: &str,
    principal: &str,
    secret: &str,
    body: Option<&str>,
) -> (u16, String, Value) {
    let mut stream = tokio::net::UnixStream::connect(socket).await.expect("connect");
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: localhost\r\nx-aac-boot-secret: {secret}\r\nx-aac-principal: {principal}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.expect("write");
    let mut raw = Vec::new();
    stream.read_to_end(&mut raw).await.expect("read");
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").expect("headers");
    let status: u16 = head
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|code| code.parse().ok())
        .expect("status");
    let decline = head
        .lines()
        .find_map(|line| line.strip_prefix("x-aac-decline: "))
        .unwrap_or("")
        .to_string();
    let value = serde_json::from_str(body).unwrap_or(Value::Null);
    (status, decline, value)
}

fn gmail_row() -> Value {
    json!({
        "id": "gmail", "email": "gmail@example.test",
        "mac": {"launcherName": "gmail", "launcherPath": "/Applications/Claude (gmail).app",
                 "profilePath": "/Users/u/Library/Application Support/Claude-gmail",
                 "sshHost": "synthetic-mac"},
        "windows": {"launcherName": "gmail", "profilePath": "C:/Users/u/AppData/Roaming/Claude-gmail",
                     "sshHost": "synthetic-windows"}
    })
}

#[tokio::test]
async fn unbound_routes_decline_the_exact_handoff() {
    let fixture = BootFixture::new();
    let (stop, handle) = start(&fixture, false).await;
    let (status, decline, body) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-open",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some(r#"{"id":"gmail","platform":"mac","respondAsync":false}"#),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(decline, "unsupported");
    assert_eq!(body, json!({"error": "Claude Open jobs are unbound"}));
    let (status, decline, body) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-history-cleanup",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some("{}"),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(decline, "unsupported");
    assert_eq!(body, json!({"error": "Claude Open jobs are unbound"}));
    let _ = stop.send(());
    handle.await.expect("serve ends");
}

#[tokio::test]
async fn bound_open_admits_shape_and_answers_refusals() {
    let fixture = BootFixture::new();
    fixture.write_manifest(json!([]));
    let (stop, handle) = start(&fixture, true).await;
    // Unknown profile: the orchestrator's 404 before any remote step.
    let (status, _, body) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-open",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some(r#"{"id":"gmail","platform":"mac","respondAsync":false}"#),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(body, json!({"error": "Claude desktop profile was not found."}));
    // A query or an unknown body key never reaches the owner.
    let (status, decline, _) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-open?x=1",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some(r#"{"id":"gmail","platform":"mac","respondAsync":false}"#),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(decline, "shape");
    let (status, decline, _) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-open",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some(r#"{"id":"gmail","platform":"mac","respondAsync":false,"extra":1}"#),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(decline, "shape");
    // The internal route admits only the service principal.
    let (status, _, _) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-open",
        ADMIN_PRINCIPAL,
        &fixture.secret,
        Some(r#"{"id":"gmail","platform":"mac","respondAsync":false}"#),
    )
    .await;
    assert_eq!(status, 401);
    let _ = stop.send(());
    handle.await.expect("serve ends");
}

#[tokio::test]
async fn bound_open_without_transport_maps_the_remote_failure() {
    let fixture = BootFixture::new();
    fixture.write_manifest(json!([gmail_row()]));
    let (stop, handle) = start(&fixture, true).await;
    // No L5 configuration: the verb misses and the reference hop is unavailable; the
    // orchestrator answers the transport refusal, never a partial Open.
    let (status, _, body) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-open",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some(r#"{"id":"gmail","platform":"mac","respondAsync":false}"#),
    )
    .await;
    assert_eq!(status, 502);
    assert_eq!(body, json!({"error": "Claude desktop request failed."}));
    let _ = stop.send(());
    handle.await.expect("serve ends");
}

#[tokio::test]
async fn bound_profiles_carry_the_progress_field() {
    let fixture = BootFixture::new();
    fixture.write_manifest(json!([gmail_row()]));
    let (stop, handle) = start(&fixture, true).await;
    let (status, _, body) = http(
        &fixture.socket,
        "GET",
        "/api/claude/desktop-profiles",
        ADMIN_PRINCIPAL,
        &fixture.secret,
        None,
    )
    .await;
    assert_eq!(status, 200);
    let profiles = body["profiles"].as_array().expect("profiles");
    assert_eq!(profiles.len(), 1);
    assert_eq!(profiles[0]["id"], json!("gmail"));
    assert_eq!(profiles[0]["openOperation"], Value::Null);
    let _ = stop.send(());
    handle.await.expect("serve ends");
}

#[tokio::test]
async fn bound_cleanup_counts_failed_targets_without_a_policy() {
    let fixture = BootFixture::new();
    fixture.write_manifest(json!([gmail_row()]));
    let (stop, handle) = start(&fixture, true).await;
    let (status, _, body) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-history-cleanup",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some("{}"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(
        body,
        json!({"profiles": 1, "targets": 2, "kept": 0, "deleted": 0, "skipped": 0, "failed": 2})
    );
    // A nonempty body never reaches the owner.
    let (status, decline, _) = http(
        &fixture.socket,
        "POST",
        "/internal/claude-history-cleanup",
        SERVICE_PRINCIPAL,
        &fixture.secret,
        Some(r#"{"extra":1}"#),
    )
    .await;
    assert_eq!(status, 503);
    assert_eq!(decline, "shape");
    let _ = stop.send(());
    handle.await.expect("serve ends");
}
