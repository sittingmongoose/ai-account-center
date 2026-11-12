//! Private credential-host capture and read. Capture/ticket commit precedes provider I/O.
use crate::reset::{Result, Snapshot, Store};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
const MAX_BYTES: u64 = 2 * 1024 * 1024;

/// Constructed only from the package's existing verified helper transport, never browser input.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HelperCommand {
    pub executable: String,
    pub args: Vec<String>,
    pub payload: Value,
}
struct OwnedChild(Child);
impl Drop for OwnedChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn helper(command: &HelperCommand, request: Value, timeout: Duration) -> Result<Value> {
    if command.executable.is_empty() || command.executable.len() > 4096 || command.args.len() > 32 {
        return Err("credential-host-unavailable");
    }
    let mut payload = command.payload.clone();
    payload["request"] = request;
    let input = serde_json::to_vec(&payload).map_err(|_| "shape")?;
    if input.len() as u64 > MAX_BYTES {
        return Err("credential-host-unavailable");
    }
    let deadline = Instant::now() + timeout;
    let mut child = OwnedChild(
        Command::new(&command.executable)
            .args(&command.args)
            .env("PYTHONIOENCODING", "utf-8")
            .env("PYTHONUTF8", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "credential-host-unavailable")?,
    );
    let mut stdin = child.0.stdin.take().ok_or("credential-host-unavailable")?;
    // A host that never consumes stdin cannot hold the capture transaction indefinitely.
    std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let stdout = child.0.stdout.take().ok_or("credential-host-unavailable")?;
    let (tx, rx) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let mut bytes = vec![];
        let result = stdout.take(MAX_BYTES + 1).read_to_end(&mut bytes);
        let _ = tx.send(result.map(|_| bytes));
    });
    let bytes = rx
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .map_err(|_| "credential-host-unavailable")?
        .map_err(|_| "credential-host-unavailable")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("credential-host-unavailable");
    }
    loop {
        if let Some(status) = child.0.try_wait().map_err(|_| "credential-host-unavailable")? {
            if !status.success() {
                return Err("credential-host-unavailable");
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err("credential-host-unavailable");
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    serde_json::from_slice(&bytes).map_err(|_| "credential-host-unavailable")
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Capture {
    account: String,
    organization: String,
    saved_login_binding: String,
    observed_at_ms: i64,
}

pub fn read(db_path: &Path, account: &str, command: &HelperCommand) -> Result<Value> {
    let request = command.payload["request"].clone();
    if !request.is_object() {
        return Err("shape");
    }
    let mut store = Store::open(db_path)?;
    let (capture, ticket) = store.capture_read(account, || {
        let mut request = request.clone();
        request["action"] = json!("capture");
        request.as_object_mut().ok_or("shape")?.remove("expectedSavedBinding");
        request
            .as_object_mut()
            .ok_or("shape")?
            .remove("expectedSavedOrganization");
        let captured: Capture = serde_json::from_value(helper(command, request, Duration::from_secs(4))?)
            .map_err(|_| "credential-host-unavailable")?;
        if captured.account.is_empty()
            || captured.account.len() > 256
            || captured.organization.is_empty()
            || captured.organization.len() > 256
            || captured.saved_login_binding.len() != 64
            || !captured.saved_login_binding.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("identity-unconfirmed");
        }
        Ok(captured)
    })?;
    // The helper pins its second process to the captured login before any OAuth request,
    // then rechecks it after usage and immediately before returning its snapshot.
    let mut read_request = request.clone();
    read_request["action"] = json!("read");
    read_request["expectedSavedBinding"] = json!(capture.saved_login_binding);
    read_request["expectedSavedOrganization"] = json!(capture.organization);
    let mut raw = helper(command, read_request, Duration::from_secs(12))?;
    if raw["savedLoginBinding"] != capture.saved_login_binding
        || raw["identity"]["account"] != capture.account
        || raw["identity"]["organization"] != capture.organization
        || raw["identity"]["provider"] != "claude"
        || raw["identity"]["profile"] != request["profile"]
        || raw["identity"]["host"] != request["host"]
    {
        return Err("stale-inventory");
    }
    raw.as_object_mut().ok_or("shape")?.remove("savedLoginBinding");
    let mut snapshot: Snapshot =
        serde_json::from_value(crate::reset_inventory::normalize_claude(&raw.to_string())?).map_err(|_| "shape")?;
    snapshot.read_ticket = Some(ticket);
    snapshot.observed_at_ms = capture.observed_at_ms;
    Ok(json!(snapshot))
}

pub fn dispatch(args: &str) -> Result<Value> {
    #[derive(Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Input {
        db_path: String,
        route_account: String,
        command: HelperCommand,
    }
    let input: Input = serde_json::from_str(args).map_err(|_| "shape")?;
    read(Path::new(&input.db_path), &input.route_account, &input.command)
}
