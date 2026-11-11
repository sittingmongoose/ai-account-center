//! Profile-bound, short-lived Codex app-server transport. No HTTPS consume guess.
//! Protocol evidence: local t3code-ref generated V2 schema (creditId is mandatory here).
use crate::reset::{Grant, Identity, Result, Snapshot};
use base64::Engine;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{BufRead, BufReader, Read, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::{Duration, Instant},
};
fn hash(s: &[u8]) -> String {
    Sha256::digest(s).iter().map(|byte| format!("{byte:02x}")).collect()
}
struct Rpc {
    child: Child,
    rx: mpsc::Receiver<Vec<u8>>,
    deadline: Instant,
    next: u64,
}
impl Drop for Rpc {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Rpc {
    fn start(home: &str, binary: &Path) -> Result<Self> {
        let mut child = Command::new(binary)
            .arg("app-server")
            .env("CODEX_HOME", home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|_| "app-server-unavailable")?;
        let out = child.stdout.take().ok_or("app-server-unavailable")?;
        let (tx, rx) = mpsc::sync_channel(4);
        std::thread::spawn(move || {
            let mut reader = BufReader::new(out);
            loop {
                let mut buf = vec![];
                let result = reader.by_ref().take(2 * 1024 * 1024 + 1).read_until(b'\n', &mut buf);
                if result.is_err() || buf.is_empty() || buf.len() > 2 * 1024 * 1024 {
                    break;
                }
                if tx.send(buf).is_err() {
                    break;
                }
            }
        });
        let mut rpc = Self {
            child,
            rx,
            deadline: Instant::now() + Duration::from_secs(20),
            next: 0,
        };
        rpc.call("initialize",json!({"clientInfo":{"name":"ai_account_center","version":"8.10.0"},"capabilities":{"experimentalApi":true}}))?;
        rpc.send(json!({"method":"initialized"}))?;
        Ok(rpc)
    }
    fn send(&mut self, v: Value) -> Result<()> {
        let input = self.child.stdin.as_mut().ok_or("app-server-unavailable")?;
        writeln!(input, "{v}").map_err(|_| "app-server-unavailable")?;
        input.flush().map_err(|_| "app-server-unavailable")
    }
    fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next += 1;
        let id = self.next;
        self.send(json!({"id":id,"method":method,"params":params}))?;
        let mut bytes = 0;
        loop {
            let left = self
                .deadline
                .checked_duration_since(Instant::now())
                .ok_or("app-server-timeout")?;
            let line = self.rx.recv_timeout(left).map_err(|_| "app-server-timeout")?;
            bytes += line.len();
            if bytes > 2 * 1024 * 1024 {
                return Err("app-server-limit");
            }
            let v: Value = serde_json::from_slice(&line).map_err(|_| "app-server-shape")?;
            if v["id"] == id {
                if !v["error"].is_null() {
                    return Err("app-server-error");
                }
                return Ok(v["result"].clone());
            }
        }
    }
}
fn auth(home: &str) -> Result<(String, String, String)> {
    let path = Path::new(home).join("auth.json");
    let meta = std::fs::symlink_metadata(&path).map_err(|_| "signed-out")?;
    if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > 1024 * 1024 {
        return Err("unsafe-profile");
    }
    let raw = std::fs::read(path).map_err(|_| "signed-out")?;
    let v: Value = serde_json::from_slice(&raw).map_err(|_| "signed-out")?;
    let token = v["tokens"]["id_token"].as_str().ok_or("signed-out")?;
    let body = token.split('.').nth(1).ok_or("signed-out")?;
    let body = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(body)
        .map_err(|_| "signed-out")?;
    let claims: Value = serde_json::from_slice(&body).map_err(|_| "signed-out")?;
    let account = claims["https://api.openai.com/auth"]["chatgpt_account_id"]
        .as_str()
        .ok_or("signed-out")?;
    if v["tokens"]["account_id"].as_str() != Some(account) {
        return Err("identity-changed");
    }
    let email = claims["email"].as_str().ok_or("signed-out")?;
    Ok((account.to_owned(), email.to_owned(), hash(&raw)))
}
/// Production caller supplies a manifest-resolved exact local home; never a request path.
pub fn codex(home: &str, profile: &str, host: &str, write: Option<&Value>) -> Result<Value> {
    codex_with_binary(home, profile, host, write, Path::new("codex"))
}

/// Offline fixtures supply their own executable; the Node ABI never accepts one.
pub fn codex_with_binary(home: &str, profile: &str, host: &str, write: Option<&Value>, binary: &Path) -> Result<Value> {
    let (account, email, binding) = auth(home)?;
    let mut rpc = Rpc::start(home, binary)?;
    let login = rpc.call("account/read", json!({"refreshToken":false}))?;
    if login["account"]["type"] != "chatgpt" || login["account"]["email"].as_str() != Some(email.as_str()) {
        return Err("identity-changed");
    }
    let usage = rpc.call("account/rateLimits/read", json!({"excludeResetCreditDetails":false}))?;
    if usage["accountId"].as_str() != Some(account.as_str()) {
        return Err("identity-unconfirmed");
    }
    let credits = usage["rateLimitResetCredits"]["credits"]
        .as_array()
        .ok_or("exact-grant-unavailable")?;
    let limits = if usage["rateLimitsByLimitId"]["codex"].is_object() {
        &usage["rateLimitsByLimitId"]["codex"]
    } else {
        &usage["rateLimits"]
    };
    if !limits["limitId"].is_null() && limits["limitId"] != "codex" {
        return Err("inventory-unavailable");
    }
    let available = usage["rateLimitResetCredits"]["availableCount"]
        .as_u64()
        .ok_or("inventory-unavailable")?;
    if credits.iter().filter(|g| g["status"] == "available").count() as u64 != available {
        return Err("inventory-incomplete");
    }
    let mut windows = BTreeMap::new();
    for k in ["primary", "secondary"] {
        if let Some(n) = limits[k]["usedPercent"].as_f64() {
            windows.insert(k.to_owned(), n);
        }
    }
    let mut grants = vec![];
    if credits.len() > 256 {
        return Err("app-server-limit");
    }
    for g in credits {
        if !matches!(g["status"].as_str(), Some("available" | "redeemed" | "redeeming")) || !g["resetType"].is_string()
        {
            return Err("inventory-unavailable");
        }
        if !g["expiresAt"].is_null() && g["expiresAt"].as_i64().is_none() {
            return Err("shape");
        }
        let id = g["id"]
            .as_str()
            .filter(|s| !s.is_empty())
            .ok_or("exact-grant-unavailable")?;
        let clears = windows
            .iter()
            .filter(|(_, v)| **v >= 100.0)
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>();
        let known = g["resetType"] == "codexRateLimits";
        grants.push(Grant {
            id: Some(id.to_owned()),
            kind: if known { "banked" } else { "unknown" }.into(),
            words: if known {
                None
            } else {
                g["resetType"].as_str().and_then(crate::reset_inventory::provider_words)
            },
            clears,
            left: if g["status"] == "available" { 1 } else { 0 },
            expires_at_ms: g["expiresAt"].as_i64().and_then(|n| n.checked_mul(1000)),
            usable: g["status"] == "available",
        });
    }
    let identity = Identity {
        provider: "codex".into(),
        account,
        organization: String::new(),
        profile: profile.into(),
        host: host.into(),
    };
    let state = hash(
        serde_json::to_string(&json!([&binding, &identity, &grants]))
            .map_err(|_| "shape")?
            .as_bytes(),
    );
    if let Some(w) = write {
        if w["identity"] != json!(identity)
            || w["sourceBinding"] != state
            || w["method"] != "account/rateLimitResetCredit/consume"
        {
            return Err("identity-changed");
        }
        let body = &w["body"];
        let id = body["creditId"].as_str().ok_or("exact-grant-unavailable")?;
        if body.as_object().is_none_or(|m| m.len() != 2)
            || body["idempotencyKey"]
                .as_str()
                .is_none_or(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
            || !grants
                .iter()
                .any(|g| g.id.as_deref() == Some(id) && g.left == 1 && g.usable)
        {
            return Err("grant-changed");
        }
        if auth(home)?.2 != binding {
            return Err("identity-changed");
        }
        return rpc.call("account/rateLimitResetCredit/consume", body.clone());
    }
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "clock")?
        .as_millis() as i64;
    Ok(json!(Snapshot {
        identity,
        observed_at_ms: now,
        grants,
        windows,
        executor_ready: true,
        source_binding: state,
        account_label: email
    }))
}

/// Private adapter ABI; all paths are server-resolved registry paths.
pub fn dispatch(args: &str) -> Result<Value> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Input {
        home: String,
        expected_account: String,
        profile: String,
        host: String,
        write: Option<Value>,
    }
    let a: Input = serde_json::from_str(args).map_err(|_| "shape")?;
    if a.expected_account.is_empty() {
        return Err("identity-unconfirmed");
    }
    if let Some(w) = &a.write
        && w["identity"]["account"] != a.expected_account
    {
        return Err("identity-changed");
    }
    let result = codex(&a.home, &a.profile, &a.host, a.write.as_ref())?;
    if a.write.is_none() && result["identity"]["account"] != a.expected_account {
        return Err("identity-changed");
    }
    Ok(result)
}
