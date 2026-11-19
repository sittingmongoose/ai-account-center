//! Local Cursor account reader and its ten-minute saved observation.
use super::provider_caches::cursor_account;
use aac_config::{ComputerConfig, OperatingSystem, TransportConfig};
use aac_core::{ComputerId, UtcMs, sha256_hex};
use aac_files::{
    OwnedStore,
    registry::{self, Owners},
};
use aac_transport::{CallOptions, Computer, SshPolicy};
use aac_views::{decline::Decline, js::parse_date};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{sync::Mutex, time::Instant};

pub(crate) const ROW: &str = "cursor-account-observation";
const CAP: usize = 32 * 1024 * 1024;
const TTL: i64 = 600_000;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Request {
    since_ms: i64,
    until_ms: i64,
    force: bool,
}

impl Request {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Decline> {
        let request: Self = crate::json_object::from_slice(bytes, &["sinceMs", "untilMs", "force"])
            .map_err(|_| Decline::Shape)?;
        if !(0..=request.until_ms).contains(&request.since_ms)
            || aac_num::ecma::to_iso_string(request.until_ms).is_none()
        {
            return Err(Decline::Shape);
        }
        Ok(request)
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Command {
    KeyStatus,
    Quota,
}

impl Command {
    pub(crate) fn parse(bytes: &[u8]) -> Result<Self, Decline> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Envelope {
            command: Command,
        }
        crate::json_object::from_slice::<Envelope>(bytes, &["command"])
            .map(|envelope| envelope.command)
            .map_err(|_| Decline::Shape)
    }

    /// The credential stays with the packaged reader. Only the fixed status and quota
    /// subcommands are exposed to the private service.
    async fn answer(self, root: PathBuf) -> Result<Value, Decline> {
        let (binary, os) = tokio::task::spawn_blocking(local_reader)
            .await
            .map_err(|_| Decline::Internal)??;
        let binary = binary.ok_or(Decline::Unsupported)?;
        let args: &[&str] = match self {
            Self::KeyStatus => &["key", "status"],
            Self::Quota => &["quota"],
        };
        Ok(execute(&root, binary, os, args, &[]).await)
    }
}

type Observation = Arc<Value>;

#[derive(Default)]
pub(crate) struct Reader {
    latest: Mutex<Option<(Instant, Observation)>>,
    commands: Mutex<()>,
}

fn target() -> Option<(&'static str, OperatingSystem)> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some(("x86_64-unknown-linux-gnu", OperatingSystem::Linux)),
        ("macos", "aarch64") => Some(("aarch64-apple-darwin", OperatingSystem::Mac)),
        ("macos", "x86_64") => Some(("x86_64-apple-darwin", OperatingSystem::Mac)),
        _ => None,
    }
}

fn executable(package: &Path, target: &str) -> Option<PathBuf> {
    let dir = package.join("dist/native/aac-cursor-account");
    let manifest: Value = serde_json::from_slice(
        &aac_files::read_bounded(&dir.join("manifest.json"), 64 * 1024).ok()?,
    )
    .ok()?;
    let entry = &manifest["targets"][target];
    let file = format!("{target}/aac-cursor-account");
    if manifest["name"] != "aac-cursor-account" || entry["file"] != file {
        return None;
    }
    let size = entry["size"]
        .as_u64()
        .filter(|size| (1..=64 * 1024 * 1024).contains(size))?;
    let path = dir.join(file);
    let bytes = aac_files::read_bounded(&path, size as usize).ok()?;
    (bytes.len() == size as usize && entry["sha256"] == sha256_hex(bytes)).then_some(path)
}

fn local_reader() -> Result<(Option<PathBuf>, OperatingSystem), Decline> {
    let package = crate::agent_transport::package_root().ok_or(Decline::Unsupported)?;
    let (target, os) = target().ok_or(Decline::Unsupported)?;
    Ok((executable(&package, target), os))
}

fn fresh(previous: &Value, now: i64) -> bool {
    previous["checkedAt"]
        .as_str()
        .and_then(|stamp| parse_date(stamp).ok().flatten())
        .is_some_and(|checked| (0..TTL).contains(&now.saturating_sub(checked)))
}

async fn execute(
    root: &Path,
    binary: PathBuf,
    os: OperatingSystem,
    command: &[&str],
    input: &[u8],
) -> Value {
    let failed = || json!({"state":"unavailable","cause":"reader_failed"});
    let Ok(id) = ComputerId::new("cursor-local") else {
        return failed();
    };
    let Ok(computer) = Computer::new(
        ComputerConfig {
            id,
            label: "Local Cursor account".into(),
            os,
            transport: TransportConfig::Local,
            roles: vec![],
            metadata: Default::default(),
        },
        SshPolicy::new(root.join("unused-known-hosts"), root.join("run")),
    ) else {
        return failed();
    };
    let argv: Vec<_> = std::iter::once(binary.into_os_string())
        .chain(command.iter().copied().map(OsString::from))
        .chain([OsString::from("--ccs-dir"), root.as_os_str().to_owned()])
        .collect();
    let options = CallOptions::new(Duration::from_secs(120), CAP);
    let Ok(result) = computer.execute(&argv, input, &options).await else {
        return failed();
    };
    std::str::from_utf8(&result.stdout)
        .ok()
        .and_then(|output| output.trim().lines().next_back())
        .and_then(|line| serde_json::from_str::<Value>(line).ok())
        .filter(Value::is_object)
        .unwrap_or_else(failed)
}

impl Reader {
    pub(crate) async fn command(&self, command: Command, root: PathBuf) -> Result<Value, Decline> {
        let _permit = self.commands.lock().await;
        command.answer(root).await
    }

    pub(crate) async fn read(
        &self,
        root: PathBuf,
        owners: Owners,
        request: Request,
    ) -> Result<Observation, Decline> {
        let started = Instant::now();
        let mut memory = self.latest.lock().await;
        let now = UtcMs::now().map_err(|_| Decline::Date)?.get() as i64;
        if let Some((collected, existing)) = &*memory {
            if *collected >= started || (!request.force && fresh(existing, now)) {
                return Ok(existing.clone());
            }
        }
        let read_root = root.clone();
        let last = memory.as_ref().map(|(_, value)| value.clone());
        let (previous, launch) = tokio::task::spawn_blocking(move || {
            let previous = match last {
                Some(value) => Some((*value).clone()),
                None => cursor_account(&read_root)?,
            };
            if !request.force && previous.as_ref().is_some_and(|row| fresh(row, now)) {
                return Ok::<_, Decline>((previous, None));
            }
            let (binary, os) = local_reader()?;
            let store = owners
                .claim(
                    registry::rows().map_err(|_| Decline::Internal)?,
                    ROW,
                    &read_root,
                    None,
                )
                .map_err(|_| Decline::Busy)?;
            Ok((previous, Some((binary, os, store, request))))
        })
        .await
        .map_err(|_| Decline::Internal)??;
        let Some((binary, os, store, request)) = launch else {
            return previous.map(Arc::new).ok_or(Decline::Internal);
        };
        let current = match binary {
            Some(binary) => {
                let input =
                    json!({"sinceMs":request.since_ms, "untilMs":request.until_ms}).to_string();
                execute(&root, binary, os, &["read"], input.as_bytes()).await
            }
            None => json!({"state":"no_reader","cause":"reader_failed"}),
        };
        let value = Arc::new(project(&current, previous.as_ref(), now)?);
        let save = value.clone();
        let _ = tokio::task::spawn_blocking(move || save_read(store, &save)).await;
        *memory = Some((Instant::now(), value.clone()));
        Ok(value)
    }
}

fn save_read(store: OwnedStore, value: &Value) {
    if let Ok(bytes) = serde_json::to_vec(value) {
        let _ = store.write_private_cache(&bytes);
    }
}

fn key(value: &Value) -> Option<Value> {
    let last = value["last4"].as_str()?;
    let fingerprint = value["fingerprint"].as_str()?;
    let start = last.char_indices().rev().nth(3).map_or(0, |(i, _)| i);
    Some(json!({"last4": &last[start..], "fingerprint":fingerprint}))
}

fn project(current: &Value, previous: Option<&Value>, now: i64) -> Result<Value, Decline> {
    let checked = aac_num::ecma::to_iso_string(now).ok_or(Decline::Date)?;
    let state = super::provider_caches::cursor_state(&current["state"]).unwrap_or("unavailable");
    let previous = previous.unwrap_or(&Value::Null);
    let key = if state == "no_key" {
        Value::Null
    } else {
        key(&current["key"]).unwrap_or_else(|| previous["key"].clone())
    };

    if state == "read" {
        let read_at = current["readAt"]
            .as_f64()
            .and_then(|ms| aac_num::ecma::to_iso_string(ms.trunc() as i64))
            .unwrap_or_else(|| checked.clone());
        let mut answer = json!({
            "state":state, "checkedAt":checked, "readAt":read_at, "account":current["account"].as_str(),
            "key":key, "rows":super::provider_caches::cursor_rows(&current["rows"]),
            "events":super::provider_caches::counter(&current["events"]).unwrap_or(0),
            "unmeasured":super::provider_caches::counter(&current["unmeasured"]).unwrap_or(0),
        });
        if let Some(counts) = super::provider_caches::cursor_event_counts(current) {
            answer["eventCounts"] = counts;
        }
        return Ok(answer);
    }
    let keep = state == "unavailable" && previous["readAt"].as_str().is_some_and(|s| !s.is_empty());
    let old = if keep { previous } else { &Value::Null };
    let mut result = json!({
        "state":state, "checkedAt":checked, "readAt":old["readAt"], "account":old["account"],
        "key":key, "rows":old["rows"].as_array().map(Vec::as_slice).unwrap_or_default(),
        "events":old["events"].as_u64().unwrap_or(0), "unmeasured":old["unmeasured"].as_u64().unwrap_or(0),
    });
    if let Some(counts) = super::provider_caches::cursor_event_counts(old) {
        result["eventCounts"] = counts;
    }
    if state == "unavailable" {
        result["cause"] = json!(
            current["cause"]
                .as_str()
                .filter(|cause| {
                    (1..=32).contains(&cause.len())
                        && cause.bytes().all(|b| b.is_ascii_lowercase() || b == b'_')
                })
                .unwrap_or("reader_failed")
        );
    }
    Ok(result)
}
