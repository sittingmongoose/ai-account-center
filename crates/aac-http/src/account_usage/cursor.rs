//! Local Cursor account reader and its ten-minute saved observation.
use super::provider_caches::{CURSOR_ACCOUNT_FILE, cursor_account};
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
    ffi::{OsStr, OsString},
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
        let (binary, os, billing) = tokio::task::spawn_blocking(local_reader)
            .await
            .map_err(|_| Decline::Internal)??;
        let binary = binary.ok_or(Decline::Unsupported)?;
        let args: &[&str] = match self {
            Self::KeyStatus => &["key", "status"],
            Self::Quota => &["quota"],
        };
        Ok(execute(&root, binary, os, billing, args, &[]).await)
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

/// `cursorBillingEnabled()` in cursor-account-reader.ts, for the native dashboard that runs
/// without Node: an explicit `0` or `1` wins, any other value or `NODE_ENV=test` is off, and
/// otherwise only an installed package (`read_packaged_build`) reads Cursor's billing items.
/// Tests, browser kits and source checkouts therefore never call Cursor.
fn billing_enabled(explicit: Option<&OsStr>, node_env: Option<&OsStr>, package: &Path) -> bool {
    if let Some(value) = explicit {
        return value == "1";
    }
    if node_env.is_some_and(|value| value == "test") {
        return false;
    }
    aac_files::deployed_build::read_packaged_build(package).is_some()
}

fn local_reader() -> Result<(Option<PathBuf>, OperatingSystem, bool), Decline> {
    let package = crate::agent_transport::package_root().ok_or(Decline::Unsupported)?;
    let (target, os) = target().ok_or(Decline::Unsupported)?;
    let billing = billing_enabled(
        std::env::var_os("AAC_CURSOR_BILLING_ENABLED").as_deref(),
        std::env::var_os("NODE_ENV").as_deref(),
        &package,
    );
    Ok((executable(&package, target), os, billing))
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
    billing: bool,
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
    let mut options = CallOptions::new(Duration::from_secs(120), CAP);
    options.local_cursor_billing = Some(billing);
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
    /// Keep Analytics responsive while a signed-in account walk is still running.
    pub(crate) async fn analytics(
        self: Arc<Self>,
        root: PathBuf,
        owners: Owners,
        engine: Arc<aac_store::engine::Engine>,
        now: i64,
        force: bool,
    ) -> Option<Value> {
        let since = now.saturating_sub(31 * 86_400_000).max(0);
        let request = Request {
            since_ms: since,
            until_ms: now,
            force,
        };
        let read_root = root.clone();
        let task = tokio::spawn(async move { self.read(read_root, owners, request).await });
        let observation = match tokio::time::timeout(Duration::from_millis(200), task).await {
            Ok(Ok(Ok(value))) => value,
            _ => {
                let cached_root = root.clone();
                Arc::new(
                    tokio::task::spawn_blocking(move || cursor_account(&cached_root))
                        .await
                        .ok()?
                        .ok()??,
                )
            }
        };
        let summary = analytics_status(&observation);
        let rows = observation["rows"].as_array()?;
        if rows.is_empty() {
            return Some(summary);
        }
        let (since, until) = super::provider_caches::cursor_coverage(&observation)
            .and_then(|(from, through)| {
                let from = parse_date(from).ok().flatten()?;
                let through = parse_date(through).ok().flatten()?;
                Some((since.max(from), now.min(through.saturating_sub(1))))
            })
            .unwrap_or((since, now));
        let dedupe = async {
            let (binary, os, billing, local) = tokio::task::spawn_blocking(move || {
                let (binary, os, billing) = local_reader().ok()?;
                Some((
                    binary?,
                    os,
                    billing,
                    local_cursor_cells(&engine, since, until)?,
                ))
            })
            .await
            .ok()??;
            let input = json!({"account":rows, "local":local}).to_string();
            let answer = execute(&root, binary, os, billing, &["dedupe"], input.as_bytes()).await;
            let mut summary = summary;
            let totals = answer["summary"].as_object()?;
            for field in [
                "accountTokens",
                "matchedTokens",
                "outsideTokens",
                "localUnmatchedTokens",
            ] {
                summary[field] = json!(totals.get(field)?.as_u64()?);
            }
            if rows
                .iter()
                .any(|row| row["uncosted"].as_u64().unwrap_or(0) > 0)
            {
                summary["costPartial"] = json!(true);
            }
            Some(summary)
        };
        tokio::time::timeout(Duration::from_secs(2), dedupe)
            .await
            .ok()
            .flatten()
            .or_else(|| {
                let mut unavailable = analytics_status(&observation);
                unavailable["state"] = json!("unavailable");
                unavailable["cause"] = json!("dedupe_failed");
                Some(unavailable)
            })
    }

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
        if let Some((collected, existing)) = &*memory
            && (*collected >= started || (!request.force && fresh(existing, now)))
        {
            return Ok(existing.clone());
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
            let (binary, os, billing) = local_reader()?;
            let store = owners
                .claim(
                    registry::rows().map_err(|_| Decline::Internal)?,
                    ROW,
                    &read_root,
                    None,
                )
                .map_err(|_| Decline::Busy)?;
            Ok((previous, Some((binary, os, billing, store, request))))
        })
        .await
        .map_err(|_| Decline::Internal)??;
        let Some((binary, os, billing, store, request)) = launch else {
            return previous.map(Arc::new).ok_or(Decline::Internal);
        };
        let current = match binary {
            Some(binary) => {
                let input =
                    json!({"sinceMs":request.since_ms, "untilMs":request.until_ms}).to_string();
                execute(&root, binary, os, billing, &["read"], input.as_bytes()).await
            }
            None => json!({"state":"no_reader","cause":"reader_failed"}),
        };
        let value = Arc::new(project(&current, previous.as_ref(), now)?);
        let save = value.clone();
        let _ = tokio::task::spawn_blocking(move || save_read(&root, store, &save)).await;
        *memory = Some((Instant::now(), value.clone()));
        Ok(value)
    }
}

fn analytics_status(observation: &Value) -> Value {
    let mut status = json!({
        "state": observation["state"], "checkedAt": observation["checkedAt"],
        "readAt": observation["readAt"], "unmeasured": observation["unmeasured"],
        "accountTokens": 0, "matchedTokens": 0, "outsideTokens": 0, "localUnmatchedTokens": 0,
    });
    for field in [
        "eventCounts",
        "coverageFrom",
        "coverageUntil",
        "unrecognizedModels",
        "cause",
    ] {
        if let Some(value) = observation.get(field) {
            status[field] = value.clone();
        }
    }
    if let Some(last) = observation["key"]["last4"].as_str() {
        status["last4"] = json!(last);
    }
    if matches!(
        observation["cause"].as_str(),
        Some("signin_needed" | "events_not_visible" | "events_unverified")
    ) {
        let hostname = aac_files::process::hostname().unwrap_or_default();
        let computer = crate::reset_source_route::local_computer(&hostname);
        status["computer"] = aac_status::computers::registry()[computer]["label"].clone();
    }
    status
}

fn local_cursor_cells(
    engine: &aac_store::engine::Engine,
    since: i64,
    until: i64,
) -> Option<Vec<Value>> {
    if until < since {
        return Some(Vec::new());
    }
    let range = aac_store::activity::range(
        engine,
        &aac_store::activity::RangeQuery {
            from_ms: since.try_into().ok()?,
            to_ms: until.try_into().ok()?,
            session_sample: Some(0),
            ..Default::default()
        },
    )
    .ok()?;
    let tools = range["tools"].as_array()?;
    let Some(cursor) = tools.iter().position(|tool| tool == "cursor") else {
        return Some(Vec::new());
    };
    let models = range["models"].as_array()?;
    let rows = range["rows"].as_array()?;
    Some(
        rows.iter()
            .filter(|row| row[2].as_u64() == Some(cursor as u64))
            .filter_map(|row| {
                let time = aac_num::ecma::to_iso_string(row[0].as_i64()?)?;
                let h = format!("{} {}:00", time.get(..10)?, time.get(11..13)?);
                let model = row[4].as_u64()?;
                Some(json!({"h":h,"m":models.get(model as usize)?.as_str()?,
            "i":row[5],"o":row[6],"cw":row[7],"cr":row[8]}))
            })
            .collect(),
    )
}

fn save_read(root: &Path, store: OwnedStore, value: &Value) {
    if let Ok(bytes) = serde_json::to_vec(value)
        && crate::analytics_storage::write(root, CURSOR_ACCOUNT_FILE, &bytes).is_none()
    {
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
    let sign_in_lost = matches!(state, "no_key" | "key_rejected")
        && super::provider_caches::cursor_coverage(previous).is_some();
    let state = if sign_in_lost { "unavailable" } else { state };
    let coverage = super::provider_caches::cursor_coverage(current);
    let key = if state == "no_key" || coverage.is_some() {
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
        if let Some((from, until)) = coverage {
            answer["coverageFrom"] = json!(from);
            answer["coverageUntil"] = json!(until);
        }
        if let Some(counts) = super::provider_caches::cursor_event_counts(current) {
            answer["eventCounts"] = counts;
        }
        if let Some(count) = super::provider_caches::cursor_unrecognized(current) {
            answer["unrecognizedModels"] = json!(count);
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
    if let Some((from, until)) = super::provider_caches::cursor_coverage(old) {
        result["coverageFrom"] = json!(from);
        result["coverageUntil"] = json!(until);
    }
    if let Some(counts) = super::provider_caches::cursor_event_counts(old) {
        result["eventCounts"] = counts;
    }
    if let Some(count) = super::provider_caches::cursor_unrecognized(old) {
        result["unrecognizedModels"] = json!(count);
    }
    if sign_in_lost {
        result["cause"] = json!("signin_needed");
    } else if state == "unavailable" {
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

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn billing_coverage_and_rows_survive_sign_in_loss() {
        let now = 1_794_067_200_000;
        let fresh = json!({"state":"read","readAt":now,"coverageFrom":"2026-11-07T00:00:00.000Z",
            "coverageUntil":"2026-11-07T16:00:00.000Z","rows":[{"h":"2026-11-07 08:00",
                "m":"fixture-model","i":1,"o":2,"cr":3,"cw":4,"n":1,"cents":0,"uncosted":1}]});
        let previous = project(&fresh, None, now).unwrap();
        for current in [
            json!({"state":"unavailable","cause":"signin_needed"}),
            json!({"state":"no_key"}),
        ] {
            let failed = project(&current, Some(&previous), now + 60_000).unwrap();
            assert_eq!(failed["state"], "unavailable");
            assert_eq!(failed["cause"], "signin_needed");
            for field in ["readAt", "rows", "coverageFrom", "coverageUntil"] {
                assert_eq!(failed[field], previous[field]);
            }
        }
    }

    fn package(installed: bool) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("dist")).unwrap();
        let info = json!({"version":1,"commit":"a".repeat(40),"branch":"main","dirty":false});
        std::fs::write(dir.path().join("dist/build-info.json"), info.to_string()).unwrap();
        if !installed {
            std::fs::create_dir_all(dir.path().join("src")).unwrap();
            std::fs::write(dir.path().join("src/ccs.ts"), "").unwrap();
        }
        dir
    }

    #[test]
    fn billing_switch_is_on_for_an_installed_package_only() {
        let installed = package(true);
        let checkout = package(false);
        let (one, zero, other) = (OsStr::new("1"), OsStr::new("0"), OsStr::new("yes"));
        let test = OsStr::new("test");
        assert!(billing_enabled(None, None, installed.path()));
        assert!(billing_enabled(
            None,
            Some(OsStr::new("production")),
            installed.path()
        ));
        assert!(!billing_enabled(None, None, checkout.path()));
        assert!(!billing_enabled(None, Some(test), installed.path()));
        assert!(!billing_enabled(Some(zero), None, installed.path()));
        assert!(!billing_enabled(Some(other), None, installed.path()));
        assert!(billing_enabled(Some(one), None, checkout.path()));
        assert!(billing_enabled(Some(one), Some(test), checkout.path()));
        // A package without build-info.json is not an installed build.
        let bare = tempfile::tempdir().unwrap();
        assert!(!billing_enabled(None, None, bare.path()));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_reader_child_receives_the_switch() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let fake = root.path().join("fake-reader");
        std::fs::write(
            &fake,
            "#!/bin/sh\nprintf '{\"enabled\":\"%s\"}\\n' \"${AAC_CURSOR_BILLING_ENABLED-unset}\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
        for (billing, seen) in [(true, "1"), (false, "0")] {
            let answer = execute(
                root.path(),
                fake.clone(),
                OperatingSystem::Linux,
                billing,
                &["read"],
                b"",
            )
            .await;
            assert_eq!(answer["enabled"], seen);
        }
    }

    #[test]
    fn unrecognized_models_and_read_in_progress_reach_the_status() {
        let now = 1_794_067_200_000;
        let fresh = json!({"state":"read","readAt":now,"coverageFrom":"2026-11-07T00:00:00.000Z",
            "coverageUntil":"2026-11-07T16:00:00.000Z","unrecognizedModels":2,"rows":[]});
        let previous = project(&fresh, None, now).unwrap();
        assert_eq!(previous["unrecognizedModels"], 2);
        assert_eq!(analytics_status(&previous)["unrecognizedModels"], 2);
        let busy = json!({"state":"unavailable","cause":"read_in_progress"});
        let kept = project(&busy, Some(&previous), now + 60_000).unwrap();
        assert_eq!(kept["cause"], "read_in_progress");
        assert_eq!(kept["unrecognizedModels"], 2);
        assert_eq!(kept["readAt"], previous["readAt"]);
        let status = analytics_status(&kept);
        assert_eq!(status["cause"], "read_in_progress");
        assert_eq!(status["unrecognizedModels"], 2);
        let none = project(
            &json!({"state":"read","readAt":now,"unrecognizedModels":0,"rows":[]}),
            None,
            now,
        )
        .unwrap();
        assert!(none.get("unrecognizedModels").is_none());
    }
}
