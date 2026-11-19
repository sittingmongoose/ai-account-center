//! G13 Open orchestrator fixtures: the route flow, coalescing, the managed copy through the
//! fixed-helper boundary, the durable hold, the launcher fallbacks and the cleanup totals,
//! all over a fake remote and disposable 0700 roots. No real app, host or provider is touched.
use super::js::{sha, stringify};
use super::manifest::profile_id_pattern;
use super::open::{ClaudeOpen, Remote, SshError, SshOutput};
use super::sync::NATIVE;
use super::sync_service::HelperCall;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::os::unix::fs::PermissionsExt;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

// --- the fake remote -------------------------------------------------------------------------
#[derive(Default)]
struct FakeState {
    host_open_results: VecDeque<Option<Value>>,
    host_open_calls: Vec<Value>,
    ssh_results: VecDeque<Result<SshOutput, SshError>>,
    ssh_calls: Vec<(Vec<String>, Option<Vec<u8>>)>,
    helper_responses: HashMap<String, VecDeque<Result<Vec<u8>, ()>>>,
    helper_calls: Vec<Value>,
    host_open_delay_ms: u64,
}

struct FakeRemote(Mutex<FakeState>);

impl FakeRemote {
    fn new() -> Self {
        Self(Mutex::new(FakeState::default()))
    }
    fn state(&self) -> std::sync::MutexGuard<'_, FakeState> {
        self.0.lock().expect("fake lock")
    }
    fn queue_helper(&self, mode: &str, responses: Vec<Result<Vec<u8>, ()>>) {
        self.state()
            .helper_responses
            .insert(mode.to_string(), responses.into_iter().collect());
    }
}

impl HelperCall for FakeRemote {
    fn call<'a>(
        &'a self,
        _ssh_host: &'a str,
        _platform: &'a str,
        _profile_id: &'a str,
        request: Value,
    ) -> Pin<Box<dyn Future<Output = Result<Vec<u8>, ()>> + Send + 'a>> {
        Box::pin(async move {
            let mode = request["mode"].as_str().unwrap_or_default().to_string();
            let mut state = self.state();
            state.helper_calls.push(request);
            state
                .helper_responses
                .get_mut(&mode)
                .and_then(|queue| queue.pop_front())
                .unwrap_or(Err(()))
        })
    }
}

impl Remote for FakeRemote {
    fn host_open<'a>(
        &'a self,
        _platform: &'a str,
        _ssh_host: &'a str,
        args: Value,
    ) -> Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>> {
        Box::pin(async move {
            let delay = {
                let mut state = self.state();
                state.host_open_calls.push(args);
                state.host_open_delay_ms
            };
            if delay > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(delay)).await;
            }
            let mut state = self.state();
            state
                .host_open_results
                .pop_front()
                .unwrap_or(Some(json!({"ok": true})))
        })
    }

    fn ssh_exec<'a>(
        &'a self,
        _platform: &'a str,
        _ssh_host: &'a str,
        argv: Vec<String>,
        stdin: Option<Vec<u8>>,
        _timeout_ms: u64,
        _max_bytes: usize,
    ) -> Pin<Box<dyn Future<Output = Result<SshOutput, SshError>> + Send + 'a>> {
        Box::pin(async move {
            let mut state = self.state();
            state.ssh_calls.push((argv, stdin));
            state
                .ssh_results
                .pop_front()
                .unwrap_or(Ok(SshOutput { code: Some(0), stdout: Vec::new() }))
        })
    }
}

// --- fixture roots and the synthetic binding set ----------------------------------------------
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = std::fs::canonicalize(dir.path()).expect("canonical");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).expect("chmod");
        std::mem::forget(dir);
        Self(path)
    }
    fn path(&self) -> &Path {
        &self.0
    }
    fn write_manifest(&self, profiles: Value) {
        std::fs::write(
            self.0.join("claude-desktop-profiles.json"),
            serde_json::to_vec(&json!({"version": 1, "profiles": profiles})).expect("json"),
        )
        .expect("write");
        std::fs::set_permissions(
            self.0.join("claude-desktop-profiles.json"),
            std::fs::Permissions::from_mode(0o600),
        )
        .expect("chmod");
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const ACCOUNT: &str = "00000000-0000-4000-8000-000000000421";
const ORG: &str = "00000000-0000-4000-8000-000000000821";
const HOSTNAME: &str = "synthetic-host-A";
const USERNAME: &str = "synthetic-user-A";
const PORT: u64 = 2265;
const MAC_ALIAS: &str = "synthetic-mac-alias-A";
const WIN_ALIAS: &str = "synthetic-windows-alias-A";
const PROJECT: &str = "/mnt/Cursor/PuppetMaster";
const ROOT_TRANSCRIPT: &str = "/synthetic-user/.claude/projects";

fn policy() -> Value {
    json!({
        "version": 1, "enabled": true, "sourcePlatform": "windows",
        "identity": {"accountUuid": ACCOUNT, "organizationUuid": ORG},
        "project": {"cwd": PROJECT, "originCwd": PROJECT, "transcriptRoot": ROOT_TRANSCRIPT},
        "ssh": {
            "mac": {"alias": MAC_ALIAS, "hostname": HOSTNAME, "username": USERNAME, "port": PORT},
            "windows": {"alias": WIN_ALIAS, "hostname": HOSTNAME, "username": USERNAME, "port": PORT}
        }
    })
}

fn gmail_entry(history_sync: Option<Value>) -> Value {
    let mut entry = json!({
        "id": "gmail", "email": "gmail@example.test",
        "mac": {"launcherName": "gmail", "launcherPath": "/Applications/Claude (gmail).app",
                 "profilePath": "/Users/u/Library/Application Support/Claude-gmail", "sshHost": MAC_ALIAS},
        "windows": {"launcherName": "gmail", "profilePath": "C:/Users/u/AppData/Roaming/Claude-gmail",
                     "sshHost": WIN_ALIAS}
    });
    if let Some(sync) = history_sync {
        entry["historySync"] = sync;
    }
    entry
}

fn uuid(n: u64) -> String {
    format!("00000000-0000-4000-8000-{n:012x}")
}

fn record_value(ordinal: u64, platform: &str) -> Value {
    let desktop = uuid(ordinal * 2 - 1);
    let cli = uuid(ordinal * 2);
    let alias = if platform == "mac" { MAC_ALIAS } else { WIN_ALIAS };
    json!({
        "sessionId": format!("local_{desktop}"), "cliSessionId": cli,
        "title": "SYNTHETIC_PRIVATE_HISTORY_TITLE",
        "cwd": PROJECT, "originCwd": PROJECT,
        "sshConfig": {"sshHost": alias},
        "sshRemoteTranscriptPath": format!("{ROOT_TRANSCRIPT}/-synthetic-project-A/{cli}.jsonl"),
        "createdAt": 1, "lastActivityAt": 2, "isArchived": false
    })
}

fn wire_row(value: &Value) -> Value {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let bytes = stringify(value).into_bytes();
    json!({
        "name": format!("{}.json", value["sessionId"].as_str().expect("sessionId")),
        "base64": STANDARD.encode(&bytes),
        "sha256": sha(&bytes)
    })
}

fn snapshot_json(platform: &str, rows: &[Value]) -> Vec<u8> {
    let wire: Vec<Value> = rows.iter().map(wire_row).collect();
    let mut pairs: Vec<Value> = wire
        .iter()
        .map(|row| json!([row["name"], row["sha256"]]))
        .collect();
    pairs.sort_by(|a, b| a[0].as_str().cmp(&b[0].as_str()));
    let revision = sha(stringify(&Value::Array(pairs)).as_bytes());
    let native = NATIVE.iter().find(|(name, _, _)| *name == platform).expect("native");
    let snapshot = json!({
        "profileId": "gmail", "platform": platform,
        "identity": {"accountSha256": sha(ACCOUNT.as_bytes()), "orgSha256": sha(ORG.as_bytes())},
        "endpoint": {
            "hostnameSha256": sha(HOSTNAME.as_bytes()),
            "usernameSha256": sha(USERNAME.as_bytes()),
            "portSha256": sha(PORT.to_string().as_bytes())
        },
        "records": wire, "snapshotStable": true, "revision": revision,
        "nativeGuard": {"version": native.1, "managerSha256": native.2,
                          "warmGuardVerified": true, "autoResumeGuardVerified": true},
        "noPendingInput": true, "noScheduledWork": true, "protectedSnapshotStable": true,
        "protectedSnapshot": {"files": [{"relative": "config.json", "sha256": sha(b"synthetic-protected")}]}
    });
    stringify(&snapshot).into_bytes()
}

fn ok_bytes(value: Value) -> Result<Vec<u8>, ()> {
    Ok(stringify(&value).into_bytes())
}

fn owner(root: &Root, remote: Arc<FakeRemote>) -> Arc<ClaudeOpen> {
    Arc::new(ClaudeOpen::new(
        root.path().to_path_buf(),
        remote,
        Box::new(|| true),
    ))
}


// --- the fixtures ------------------------------------------------------------------------------
#[tokio::test]
async fn unknown_profile_is_404() {
    let root = Root::new();
    root.write_manifest(json!([]));
    let remote = Arc::new(FakeRemote::new());
    let open = owner(&root, Arc::clone(&remote));
    assert_eq!(open.open("gmail", "mac", false).await.status, 404);
    assert_eq!(
        open.open("gmail", "mac", false).await.body,
        json!({"error": "Claude desktop profile was not found."})
    );
    assert!(remote.state().host_open_calls.is_empty());
}

#[tokio::test]
async fn unsafe_id_is_404_before_any_read() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(None)]));
    let open = owner(&root, Arc::new(FakeRemote::new()));
    assert!(!profile_id_pattern("../outside"));
    assert_eq!(open.open("../outside", "mac", false).await.status, 404);
}

#[tokio::test]
async fn unconfigured_platform_is_409() {
    let root = Root::new();
    let mut entry = gmail_entry(None);
    entry["mac"]["launcherPath"] = json!("relative.app");
    root.write_manifest(json!([entry]));
    let open = owner(&root, Arc::new(FakeRemote::new()));
    let result = open.open("gmail", "mac", false).await;
    assert_eq!(result.status, 409);
    assert_eq!(
        result.body,
        json!({"error": "Claude desktop launcher is not configured for this platform."})
    );
}

#[tokio::test]
async fn ordinary_open_uses_the_launcher_verb() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(None)]));
    let remote = Arc::new(FakeRemote::new());
    let open = owner(&root, Arc::clone(&remote));
    let result = open.open("gmail", "mac", false).await;
    assert_eq!(result.status, 200);
    assert_eq!(result.body, json!({"opened": true, "id": "gmail", "platform": "mac"}));
    let state = remote.state();
    assert_eq!(state.host_open_calls.len(), 1);
    assert_eq!(state.host_open_calls[0]["op"], json!("open"));
    assert_eq!(state.host_open_calls[0]["launcher"]["launcherPath"], json!("/Applications/Claude (gmail).app"));
    assert!(state.helper_calls.is_empty());
    assert!(state.ssh_calls.is_empty());
}

#[tokio::test]
async fn ordinary_open_falls_back_to_the_fixed_ssh_command() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(None)]));
    let remote = Arc::new(FakeRemote::new());
    remote.state().host_open_results.push_back(None);
    let open = owner(&root, Arc::clone(&remote));
    assert_eq!(open.open("gmail", "mac", false).await.status, 200);
    let state = remote.state();
    assert_eq!(state.ssh_calls.len(), 1);
    let (argv, stdin) = &state.ssh_calls[0];
    assert_eq!(argv[0], "/bin/sh");
    assert_eq!(argv[1], "-c");
    assert!(argv[2].starts_with("test -d '/Applications/Claude (gmail).app' || exit 1"));
    assert!(argv[2].ends_with("exec /usr/bin/open '/Applications/Claude (gmail).app'"));
    assert!(stdin.is_none());
}

#[tokio::test]
async fn transport_failures_map_to_502_and_504() {
    for (error, status) in [(SshError::Failed, 502u16), (SshError::Timeout, 504)] {
        let root = Root::new();
        root.write_manifest(json!([gmail_entry(None)]));
        let remote = Arc::new(FakeRemote::new());
        {
            let mut state = remote.state();
            state.host_open_results.push_back(None);
            state.ssh_results.push_back(Err(error));
        }
        let open = owner(&root, Arc::clone(&remote));
        let result = open.open("gmail", "mac", false).await;
        assert_eq!(result.status, status);
    }
}

#[tokio::test]
async fn durable_hold_refuses_before_any_work() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(Some(policy()))]));
    let mut marker = super::markers::PendingMarker::arm(root.path(), "gmail", "mac").expect("arm");
    marker.release();
    let remote = Arc::new(FakeRemote::new());
    let open = owner(&root, Arc::clone(&remote));
    let result = open.open("gmail", "mac", false).await;
    assert_eq!(result.status, 409);
    assert_eq!(result.body["code"], json!("history_unconfirmed"));
    assert!(remote.state().helper_calls.is_empty());
    assert!(remote.state().host_open_calls.is_empty());
}

#[tokio::test]
async fn managed_open_copies_then_launches_with_progress() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(Some(policy()))]));
    let remote = Arc::new(FakeRemote::new());
    let source = record_value(1, "windows");
    // The core's call order: closed, collect(target), collect(source), verify, collect(source),
    // then per batch: closed, protected, append, collect(target). Retention runs after a copy.
    remote.queue_helper("closed-check", vec![ok_bytes(json!({"closed": true})), ok_bytes(json!({"closed": true}))]);
    remote.queue_helper(
        "collect",
        vec![
            Ok(snapshot_json("mac", &[])),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
            Ok(snapshot_json("mac", std::slice::from_ref(&source))),
        ],
    );
    remote.queue_helper("verify-transcripts", vec![ok_bytes(json!({"verified": true}))]);
    remote.queue_helper("protected-check", vec![ok_bytes(json!({"unchanged": true}))]);
    remote.queue_helper(
        "append",
        vec![ok_bytes(json!({"status": "created_metadata", "createdCount": 1, "protectedBytesUnchanged": true, "writerQuiescent": true}))],
    );
    remote.queue_helper(
        "snapshot-cleanup",
        vec![ok_bytes(json!({"kept": ["a"], "deleted": [], "skipped": []}))],
    );
    let open = owner(&root, Arc::clone(&remote));
    let accepted = open.open("gmail", "mac", true).await;
    assert_eq!(accepted.status, 202);
    assert_eq!(accepted.body["id"], json!("gmail"));
    assert_eq!(accepted.body["state"], json!("checking"));
    let operation_id = accepted.body["operationId"].as_str().expect("id").to_string();
    let scope = root.path().to_string_lossy().to_string();
    let settled = open.operations().settled(&scope, "gmail", "mac").expect("settled");
    assert_eq!(settled.await, Ok(()));
    let operation = open.operation_for(&scope, "gmail").expect("operation");
    assert_eq!(operation.id, operation_id);
    assert_eq!(operation.state, "opened");
    assert_eq!(operation.confirmed_count, Some(1));
    assert_eq!(operation.total_count, Some(1));
    assert!(operation.message.is_none());
    let state = remote.state();
    assert_eq!(state.host_open_calls.len(), 1);
    assert!(state.helper_calls.iter().any(|call| call["mode"] == json!("append")));
    assert!(state.helper_calls.iter().any(|call| call["mode"] == json!("snapshot-cleanup")));
    // One finished marker, no hold.
    assert!(!super::markers::open_held(root.path(), "gmail", "mac"));
    let marker_root = root.path().join("claude-history-pending");
    assert_eq!(std::fs::read_dir(&marker_root).expect("dir").count(), 1);
}

#[tokio::test]
async fn managed_copy_alone_synchronizes_one_record() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(Some(policy()))]));
    let remote = Arc::new(FakeRemote::new());
    let source = record_value(1, "windows");
    remote.queue_helper("closed-check", vec![ok_bytes(json!({"closed": true})), ok_bytes(json!({"closed": true}))]);
    remote.queue_helper(
        "collect",
        vec![
            Ok(snapshot_json("mac", &[])),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
            Ok(snapshot_json("mac", std::slice::from_ref(&source))),
        ],
    );
    remote.queue_helper("verify-transcripts", vec![ok_bytes(json!({"verified": true}))]);
    remote.queue_helper("protected-check", vec![ok_bytes(json!({"unchanged": true}))]);
    remote.queue_helper(
        "append",
        vec![ok_bytes(json!({"status": "created_metadata", "createdCount": 1, "protectedBytesUnchanged": true, "writerQuiescent": true}))],
    );
    remote.queue_helper(
        "snapshot-cleanup",
        vec![ok_bytes(json!({"kept": ["a"], "deleted": [], "skipped": []}))],
    );
    let profile = super::manifest::list_profiles(root.path())
        .expect("manifest")
        .into_iter()
        .next()
        .expect("one profile");
    let result = super::sync_service::synchronize_before_open(
        root.path(),
        &profile,
        "mac",
        remote.as_ref(),
        &super::sync_service::NoObserver,
        true,
    )
    .await;
    let modes: Vec<String> = remote
        .state()
        .helper_calls
        .iter()
        .map(|call| call["mode"].as_str().unwrap_or("?").to_string())
        .collect();
    assert_eq!(
        result["status"],
        json!("synchronized"),
        "copy result: {result}; helper calls: {modes:?}"
    );
    assert_eq!(result["createdCount"], json!(1), "copy result: {result}");
    assert!(!super::markers::open_held(root.path(), "gmail", "mac"));
}

#[tokio::test]
async fn lost_append_receipt_holds_open_as_blocked_uncertain() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(Some(policy()))]));
    let remote = Arc::new(FakeRemote::new());
    let source = record_value(1, "windows");
    remote.queue_helper("closed-check", vec![ok_bytes(json!({"closed": true})), ok_bytes(json!({"closed": true}))]);
    remote.queue_helper(
        "collect",
        vec![
            Ok(snapshot_json("mac", &[])),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
        ],
    );
    remote.queue_helper("verify-transcripts", vec![ok_bytes(json!({"verified": true}))]);
    remote.queue_helper("protected-check", vec![ok_bytes(json!({"unchanged": true}))]);
    remote.queue_helper("append", vec![Err(())]);
    let open = owner(&root, Arc::clone(&remote));
    let result = open.open("gmail", "mac", false).await;
    assert_eq!(result.status, 409);
    assert_eq!(result.body["code"], json!("history_unconfirmed"));
    let scope = root.path().to_string_lossy().to_string();
    let operation = open.operation_for(&scope, "gmail").expect("operation");
    assert_eq!(operation.state, "blocked_uncertain");
    assert_eq!(
        operation.message.as_deref(),
        Some(super::operations::CLAUDE_HISTORY_UNCONFIRMED_MESSAGE)
    );
    assert!(super::markers::open_held(root.path(), "gmail", "mac"));
    assert!(remote.state().host_open_calls.is_empty());
}
#[tokio::test]
async fn concurrent_clicks_join_one_open() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(None)]));
    let remote = Arc::new(FakeRemote::new());
    remote.state().host_open_delay_ms = 50;
    let open = owner(&root, Arc::clone(&remote));
    let (first, second) = tokio::join!(open.open("gmail", "mac", false), open.open("gmail", "mac", false));
    assert_eq!(first.status, 200);
    assert_eq!(second.status, 200);
    assert_eq!(remote.state().host_open_calls.len(), 1);
}

#[tokio::test]
async fn repeated_click_within_a_second_debounces() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(None)]));
    let remote = Arc::new(FakeRemote::new());
    let open = owner(&root, Arc::clone(&remote));
    assert_eq!(open.open("gmail", "mac", false).await.status, 200);
    assert_eq!(open.open("gmail", "mac", false).await.status, 200);
    assert_eq!(remote.state().host_open_calls.len(), 1);
}

#[tokio::test]
async fn windows_open_rebuilds_the_account_list() {
    let root = Root::new();
    let mut store = gmail_entry(None);
    store["id"] = json!("store");
    store["email"] = json!("store@example.test");
    store["windows"]["isDefault"] = json!(true);
    root.write_manifest(json!([store, gmail_entry(None)]));
    let remote = Arc::new(FakeRemote::new());
    let open = owner(&root, Arc::clone(&remote));
    assert_eq!(open.open("store", "windows", false).await.status, 200);
    let state = remote.state();
    assert_eq!(state.host_open_calls.len(), 1);
    assert_eq!(state.host_open_calls[0]["profileId"], json!("store"));
    assert_eq!(state.host_open_calls[0]["accountList"], json!(["store", "gmail"]));
}

#[tokio::test]
async fn cleanup_totals_every_profile_and_platform() {
    let root = Root::new();
    let mut store = gmail_entry(Some(policy()));
    store["id"] = json!("store");
    store["email"] = json!("store@example.test");
    store["windows"]["isDefault"] = json!(true);
    // store has no Mac launcher: one target; gmail has two.
    store.as_object_mut().expect("obj").remove("mac");
    root.write_manifest(json!([store, gmail_entry(Some(policy()))]));
    let remote = Arc::new(FakeRemote::new());
    remote.queue_helper(
        "snapshot-cleanup",
        vec![
            ok_bytes(json!({"kept": ["a", "b", "c"], "deleted": ["d"], "skipped": []})),
            ok_bytes(json!({"kept": [], "deleted": [], "skipped": ["e"]})),
            Err(()),
        ],
    );
    let open = owner(&root, Arc::clone(&remote));
    let result = open.cleanup().await;
    assert_eq!(result.status, 200);
    assert_eq!(
        result.body,
        json!({"profiles": 2, "targets": 3, "kept": 3, "deleted": 1, "skipped": 1, "failed": 1})
    );
}

#[tokio::test]
async fn policy_change_stops_the_copy_but_open_proceeds() {
    let root = Root::new();
    root.write_manifest(json!([gmail_entry(Some(policy()))]));
    let remote = Arc::new(FakeRemote::new());
    let source = record_value(1, "windows");
    remote.queue_helper("closed-check", vec![ok_bytes(json!({"closed": true})), ok_bytes(json!({"closed": true}))]);
    remote.queue_helper(
        "collect",
        vec![
            Ok(snapshot_json("mac", &[])),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
            Ok(snapshot_json("windows", std::slice::from_ref(&source))),
        ],
    );
    remote.queue_helper("verify-transcripts", vec![ok_bytes(json!({"verified": true}))]);
    remote.queue_helper("protected-check", vec![ok_bytes(json!({"unchanged": true}))]);
    // The policy disappears before the first append: the copy stops cleanly, Open proceeds.
    let open = owner(&root, Arc::clone(&remote));
    let root_path = root.path().to_path_buf();
    let manifest_path = root_path.join("claude-desktop-profiles.json");
    let mut entry = gmail_entry(None);
    entry.as_object_mut().expect("obj").remove("historySync");
    std::fs::write(
        &manifest_path,
        serde_json::to_vec(&json!({"version": 1, "profiles": [entry]})).expect("json"),
    )
    .expect("write");
    let result = open.open("gmail", "mac", false).await;
    assert_eq!(result.status, 200);
    let state = remote.state();
    assert_eq!(state.host_open_calls.len(), 1);
    assert!(!state.helper_calls.iter().any(|call| call["mode"] == json!("append")));
    assert!(!super::markers::open_held(root.path(), "gmail", "mac"));
}
