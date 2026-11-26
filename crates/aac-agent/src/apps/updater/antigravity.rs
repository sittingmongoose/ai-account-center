//! T3's managed Antigravity ACP runtime (Python `app_update_antigravity.py`
//! and `app_update_antigravity_release.py`): read-only discovery of the
//! runtime T3 activated, and one guarded install through the local T3 server.
//!
//! AAC never opens, replaces or repairs `active.json`. T3's running server
//! owns release selection, validation, publication and activation through
//! `provider.install.start`; this follows the target T3 offers and the
//! operation T3 returns. Nothing here pins a release: T3 installs Google's
//! newest native CLI, and AAC reads the same channel only to know what is
//! newest when T3 gave no usable offer.
use super::{
    AppRow, AppUpdater, BoxFuture, Context, Detection, FailureCause, Host, Installed, Platform,
    Probe, Status, UpdateFailure, common, shared,
};
use aac_files::read::{Read, ReadPolicy};
use regex::Regex;
use serde_json::{Value, json};
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::LazyLock,
    time::Duration,
};
use tokio::time::Instant;

const APP: &str = "antigravity-acp";
const MANAGER: &str = "official-download";
const RECORD: &str = ".install-complete.json";
/// Python `app_update_antigravity_release.CLI_MANIFEST_BASE`.
const CLI_MANIFEST_BASE: &str =
    "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests/";
/// Python `NATIVE_FROM`: T3 refuses anything older; the CLI channel starts at 1.5.0.
const NATIVE_FROM: [u64; 3] = [1, 5, 0];
/// Python `SIGNIN_CARRIES_TO_NATIVE`: hosts where moving from the 1.3/1.4
/// runtime pair to the native CLI is known to keep the sign-in.
const SIGNIN_CARRIES_TO_NATIVE: [Platform; 1] = [Platform::Windows];
/// How long a subscription may stay quiet while T3's background release check runs.
const OFFER_SETTLE: Duration = Duration::from_secs(15);
const MANIFEST_SECONDS: Duration = Duration::from_secs(10);
const MANIFEST_LIMIT: u64 = 16 * 1024;
/// A T3 server this run did not replace gets one short readiness window.
const QUIET_READY_SECONDS: Duration = Duration::from_secs(15);
/// Python `t3_rpc.READY_SECONDS`: a server replaced in this run gets time to come up.
const READY_SECONDS: Duration = Duration::from_secs(180);
/// T3 reports success once its operation ends; its pointer is re-read a few times.
const VERIFY_ATTEMPTS: u32 = 3;
const VERIFY_PAUSE: Duration = Duration::from_millis(500);
/// Python `update_antigravity_acp`: the install's own bound.
const INSTALL_SECONDS: u64 = 900;
const SETTINGS_LIMIT: u64 = 4 * 1024 * 1024;
const RECORD_LIMIT: u64 = 8192;
/// Python `follow_up_antigravity_acp`'s bounds.
const FOLLOW_UP_SECONDS: Duration = Duration::from_secs(20 * 60);
const FOLLOW_UP_POLL: Duration = Duration::from_secs(10);
const FOLLOW_UP_LOCK: Duration = Duration::from_secs(2);

static RELEASE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[a-f0-9]{64}\z").expect("release id"));
static SEMVER: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+\z").expect("semver"));
/// T3 accepts older non-semver records (`agy_acp_server_1.1.1`): bounded plain words only.
static STATE_VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\A[A-Za-z0-9][A-Za-z0-9._+-]{0,63}\z").expect("state version"));
static SIGNIN_WORDS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)sign[- ]?in|log[- ]?in|logged in|not authenticated|unauthenticated|re-?authenticate",
    )
    .expect("sign-in words")
});
const PHASES: [&str; 7] = [
    "idle",
    "downloading",
    "extracting",
    "verifying",
    "succeeded",
    "failed",
    "cancelled",
];

pub(super) static ACP: Acp = Acp;

pub(super) struct Acp;

impl AppUpdater for Acp {
    fn app_id(&self) -> &'static str {
        APP
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            match find(&cx.host, cx.platform) {
                Some(install) => Detection::Found(Box::new(install)),
                None => Detection::Absent,
            }
        })
    }

    fn update<'a>(
        &'a self,
        cx: &'a Context,
        install: &'a Installed,
    ) -> BoxFuture<'a, Result<AppRow, UpdateFailure>> {
        Box::pin(async move { Ok(update_with(cx, install, None, &Local).await) })
    }
}

/// Python `result(..., attempted)`.
fn row(
    install: &Installed,
    status: Status,
    before: Option<&str>,
    after: Option<&str>,
    code: &str,
    attempted: bool,
) -> AppRow {
    let row = AppRow::of(install, status, before, after, code);
    if attempted { row.attempted(0) } else { row }
}

fn newer(left: &str, right: &str) -> bool {
    common::version_tuple(left) > common::version_tuple(right)
}

/// Python `version_tuple(version) >= NATIVE_FROM`.
fn native(version: &str) -> bool {
    common::version_tuple(version) >= NATIVE_FROM.to_vec()
}

fn protocol() -> UpdateFailure {
    UpdateFailure::new("t3_install_failed:protocol")
}

fn unavailable(reason: &'static str) -> UpdateFailure {
    UpdateFailure::new("t3_install_failed:provider_unavailable")
        .with_cause(FailureCause::Unknown, Some(reason))
}

// --- discovery (read-only) ----------------------------------------------------------------------

/// Python `platform_key`: the folder T3 keeps this computer's managed runtime in.
fn platform_key(platform: Platform, arch: &str) -> Option<String> {
    let arch = match arch {
        "arm64" | "aarch64" => "arm64",
        "x86_64" | "amd64" => "x64",
        _ => return None,
    };
    let system = match platform {
        Platform::Ubuntu => "linux",
        Platform::Mac => "darwin",
        Platform::Windows => "win32",
    };
    Some(format!("{system}-{arch}"))
}

fn managed_root(host: &Host, platform: Platform) -> Option<PathBuf> {
    Some(
        host.home
            .join(".t3/tools/antigravity-acp")
            .join(platform_key(platform, host.arch)?),
    )
}

/// Python `cli_target`: the CLI channel's manifest name for this computer.
fn cli_target(host: &Host, platform: Platform) -> Option<String> {
    let arch = match host.arch {
        "arm64" | "aarch64" => "arm64",
        "x86_64" | "amd64" => "amd64",
        _ => return None,
    };
    let system = match platform {
        Platform::Ubuntu => "linux",
        Platform::Mac => "darwin",
        Platform::Windows => "windows",
    };
    let libc = if arch == "arm64" { "aarch64" } else { "x86_64" };
    let musl = platform == Platform::Ubuntu
        && host.system(&format!("/lib/libc.musl-{libc}.so.1")).exists();
    Some(format!(
        "{system}_{arch}{}",
        if musl { "_musl" } else { "" }
    ))
}

/// Python `read_json`: a small regular file that is not a link, holding an object.
fn read_object(path: &Path, limit: u64) -> Option<serde_json::Map<String, Value>> {
    match aac_files::read::read(path, &ReadPolicy::capped(limit)) {
        Read::Bytes(bytes, _) => match serde_json::from_slice::<Value>(&bytes) {
            Ok(Value::Object(fields)) => Some(fields),
            _ => None,
        },
        _ => None,
    }
}

/// Python `custom_executable_instances`: the instance ids whose T3 settings name a
/// custom Antigravity executable. T3 refuses to install its managed runtime through
/// such an instance. Only ids are read, never other settings; an unreadable file
/// names none (T3's own refusal then decides).
fn custom_executable_instances(host: &Host) -> HashSet<String> {
    let mut custom = HashSet::new();
    let path = host.home.join(".t3/userdata/settings.json");
    let Some(mut fields) = read_object(&path, SETTINGS_LIMIT) else {
        return custom;
    };
    let Some(Value::Object(instances)) = fields.remove("providerInstances") else {
        return custom;
    };
    for (key, value) in instances {
        let binary = value["config"]["binaryPath"].as_str();
        if value["config"].is_object() && binary.is_some_and(|path| !path.trim().is_empty()) {
            custom.insert(key);
        }
    }
    custom
}

fn linked(path: &Path) -> bool {
    std::fs::symlink_metadata(path)
        .is_ok_and(|info| info.file_type().is_symlink() || common::is_junction(&info))
}

/// Python `_artifact`: the record names `name`, and that file is an executable of the recorded size.
fn artifact(directory: &Path, item: &Value, name: &str, windows: bool) -> bool {
    if item["name"] != name {
        return false;
    }
    let Some(size) = item["bytes"]
        .as_u64()
        .filter(|size| (1..=9_007_199_254_740_991).contains(size))
    else {
        return false;
    };
    let path = directory.join(name);
    let Ok(meta) = std::fs::symlink_metadata(&path) else {
        return false;
    };
    !linked(&path)
        && meta.is_file()
        && meta.len() == size
        && (windows || crate::apps::executable(&path))
}

/// Python `completed`: a finished T3 install record, in either shape T3 writes:
/// the 1.3/1.4 runtime pair, or the native 1.5+ CLI with `"harness": null`
/// (T3 up to 0.1.11) or no harness key at all (T3 0.1.12). A record with no
/// harness must name the native CLI; only a harness object makes it the old pair.
fn completed(platform: Platform, root: &Path, release_id: &str) -> Option<(String, &'static str)> {
    if !RELEASE_ID.is_match(release_id) {
        return None;
    }
    let directory = root.join("versions").join(release_id);
    if linked(&directory) {
        return None;
    }
    let record = read_object(&directory.join(RECORD), RECORD_LIMIT)?;
    let version = record.get("version").filter(|version| {
        record.get("releaseId").and_then(Value::as_str) == Some(release_id)
            && version
                .as_str()
                .is_some_and(|version| STATE_VERSION.is_match(version))
    })?;
    let windows = platform == Platform::Windows;
    let kind = if record.get("harness").is_none_or(Value::is_null) {
        let cli = if windows { "agy.exe" } else { "antigravity" };
        if !artifact(&directory, &record["executable"], cli, windows) {
            return None;
        }
        "native"
    } else {
        let (server, harness) = if windows {
            ("agy_acp_server.exe", "localharness_external.exe")
        } else {
            ("agy_acp_server.par", "localharness_external")
        };
        if !artifact(&directory, &record["executable"], server, windows)
            || !artifact(&directory, &record["harness"], harness, windows)
        {
            return None;
        }
        "harness"
    };
    Some((version.as_str()?.to_owned(), kind))
}

/// Python `detect_antigravity_acp`.
fn find(host: &Host, platform: Platform) -> Option<Installed> {
    let root = managed_root(host, platform)?;
    let mut install = Installed::new(APP, platform, root.clone(), None, MANAGER);
    let active = root.join("active.json");
    let found = match std::fs::symlink_metadata(&active) {
        // Python's read_json raises for a linked, oversized or unreadable pointer,
        // one that is not an object, or a missing release id: the runtime is then unknown.
        Ok(_) => match read_object(&active, RECORD_LIMIT) {
            Some(pointer) => pointer
                .get("releaseId")
                .and_then(Value::as_str)
                .and_then(|release_id| completed(platform, &root, release_id)),
            None => None,
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return None,
        Err(_) => None,
    };
    match found {
        Some((version, kind)) => {
            install.version = Some(version);
            install.runtime_kind = Some(kind);
            install.owner = Some(json!({"manager": "t3", "runtime": kind}));
        }
        None => {
            install.probe = Probe::Failed;
            install.probe_reason = Some("runtime_record_invalid");
        }
    }
    Some(install)
}

// --- the install --------------------------------------------------------------------------------

/// Python `newest_cli_version`: the CLI channel's newest version for this computer,
/// or `None` when it could not be read.
async fn newest_cli_version(cx: &Context, deadline: Instant) -> Option<String> {
    let target = cli_target(&cx.host, cx.platform)?;
    let end = deadline.min(Instant::now() + MANIFEST_SECONDS);
    if end <= Instant::now() {
        return None;
    }
    let url = format!("{CLI_MANIFEST_BASE}{target}.json");
    let raw = cx
        .system
        .fetch(
            &url,
            CLI_MANIFEST_BASE,
            MANIFEST_LIMIT,
            end - Instant::now(),
        )
        .await
        .ok()?;
    let value: Value = serde_json::from_slice(&raw).ok()?;
    value
        .get("version")?
        .as_str()
        .filter(|version| SEMVER.is_match(version) && native(version))
        .map(str::to_owned)
}

/// Python `checked_state`: one ProviderInstallState. `version` is T3's offer and
/// may be null until its release check ends.
fn checked_state(value: &Value) -> Result<&Value, UpdateFailure> {
    let Some(fields) = value.as_object() else {
        return Err(protocol());
    };
    if fields.get("driver").and_then(Value::as_str) != Some("antigravity") {
        return Err(protocol());
    }
    if !fields
        .get("phase")
        .and_then(Value::as_str)
        .is_some_and(|phase| PHASES.contains(&phase))
    {
        return Err(protocol());
    }
    for key in ["version", "installedVersion"] {
        if let Some(version) = fields.get(key)
            && !version.is_null()
            && !version
                .as_str()
                .is_some_and(|version| STATE_VERSION.is_match(version))
        {
            return Err(protocol());
        }
    }
    Ok(value)
}

/// What the updater does through T3's local install client (Python
/// `InstallRpc` in `app_update_t3_rpc.py`; lane W5 ports it in `t3.rs`).
pub(super) trait T3Session: Send {
    /// Python `InstallRpc.call`: one request on this session.
    fn call<'a>(
        &'a mut self,
        method: &'a str,
        payload: Value,
    ) -> BoxFuture<'a, Result<Value, UpdateFailure>>;

    /// Python `InstallRpc.subscribe`: every state of `instance`'s subscription
    /// until the stream ends, or until `take` says stop (the subscription is then
    /// interrupted). `take`'s own error ends the read with it.
    fn states<'a>(
        &'a mut self,
        instance: &'a str,
        take: &'a mut (dyn FnMut(Value) -> Result<bool, UpdateFailure> + Send),
    ) -> BoxFuture<'a, Result<(), UpdateFailure>>;

    /// Python `read_offer`'s read: a subscription on the current connection that
    /// gives up once it stays quiet for `settle` (the connection is then dropped)
    /// or once `take` says stop (it is then interrupted).
    fn offer<'a>(
        &'a mut self,
        instance: &'a str,
        settle: Duration,
        take: &'a mut (dyn FnMut(Value) -> Result<bool, UpdateFailure> + Send),
    ) -> BoxFuture<'a, Result<(), UpdateFailure>>;

    /// Python `InstallRpc.close`: end the session (its bearer is revoked).
    fn close<'a>(self: Box<Self>) -> BoxFuture<'a, Result<(), UpdateFailure>>;
}

/// How the updater reaches the local T3 server (Python `wait_ready`, `discover`
/// and `InstallRpc` in `app_update_t3_rpc.py`; lane W5's `t3.rs` is [`Local`]'s backing).
pub(super) trait T3Server: Send + Sync {
    /// Python `t3_rpc.wait_ready`.
    fn ready<'a>(
        &'a self,
        cx: &'a Context,
        expected: Option<&'a str>,
        seconds: Duration,
    ) -> BoxFuture<'a, Result<(), UpdateFailure>>;

    /// Python `t3_rpc.discover` and one `InstallRpc` session.
    fn open<'a>(
        &'a self,
        cx: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn T3Session + 'a>, UpdateFailure>>;
}

/// Python `read_offer`: T3's current offer, the latest subscription state,
/// waiting a bounded while for a non-null version. Subscribing starts T3's
/// background release check, so the first state can precede it.
async fn read_offer(
    client: &mut dyn T3Session,
    instance: &str,
) -> Result<Option<Value>, UpdateFailure> {
    let mut latest = None;
    client
        .offer(instance, OFFER_SETTLE, &mut |value| {
            let state = checked_state(&value)?;
            latest = Some(value.clone());
            Ok(state
                .get("version")
                .and_then(Value::as_str)
                .is_some_and(|version| !version.is_empty())
                && state
                    .get("phase")
                    .and_then(Value::as_str)
                    .is_some_and(|phase| {
                        matches!(phase, "idle" | "succeeded" | "failed" | "cancelled")
                    }))
        })
        .await?;
    Ok(latest)
}

/// Python `refresh_models`: ask T3 to rebuild every enabled Antigravity
/// instance's model list after an install. A refusal whose words (its error tag
/// and detail, never the instance name) mention sign-in, or an instance that now
/// reads unauthenticated, means the new runtime cannot use the stored sign-in.
/// Returns the worst of all instances: (`signin` | `failed` | `ok`, word).
async fn refresh_models(
    client: &mut dyn T3Session,
    instances: &[String],
) -> (&'static str, Option<&'static str>) {
    let (mut signin, mut failed) = (None, None);
    for instance in instances {
        let reply = client
            .call(
                "server.refreshProviders",
                json!({"instanceId": instance, "refreshModels": true}),
            )
            .await;
        let reply = match reply {
            Ok(reply) => reply,
            Err(error) => {
                let output = error.output.as_str();
                if SIGNIN_WORDS.is_match(output) {
                    signin = signin.or(Some("refresh_signin"));
                } else if output.to_lowercase().contains("timed out") {
                    failed = failed.or(Some("refresh_timed_out"));
                } else {
                    failed = failed.or(Some("refresh_failed"));
                }
                continue;
            }
        };
        for provider in reply
            .get("providers")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|provider| provider.is_object() && provider["instanceId"] == *instance)
        {
            if provider["auth"].is_object() && provider["auth"]["status"] == "unauthenticated" {
                signin = signin.or(Some("unauthenticated"));
            }
        }
    }
    match (signin, failed) {
        (Some(word), _) => ("signin", Some(word)),
        (None, Some(word)) => ("failed", Some(word)),
        (None, None) => ("ok", None),
    }
}

/// Python `restart_pending`: whether T3 changed in this run without its new
/// server running yet (the Linux deferred restart).
fn restart_pending(t3_row: Option<&Value>) -> bool {
    let Some(row) = t3_row else {
        return false;
    };
    if !row.is_object() || row["status"] == "current" {
        return false;
    }
    if let Some(targets) = row["restartTargets"].as_array()
        && targets
            .iter()
            .any(|target| target.is_object() && target["kind"] == "systemd")
    {
        return true;
    }
    row["platform"] == "ubuntu" && row["updateAttempted"] == true
}

/// Python `replaced_version`: the server version T3's own row put in place in
/// this run (Mac and Windows reopen it at once).
fn replaced_version(t3_row: Option<&Value>) -> Option<&str> {
    let row = t3_row?;
    let desktop = row["restartTargets"].as_array().is_some_and(|targets| {
        targets
            .iter()
            .any(|target| target.is_object() && target["kind"] == "desktop")
    });
    (row.is_object() && row["status"] == "updated" && desktop)
        .then(|| row["version"].as_str())
        .flatten()
}

/// Python `verified_release`: the runtime T3 activated, read from its own
/// pointer and record on disk (never T3's provider snapshot): at least `target`
/// and never below `floor`. Re-read a bounded few times; `None` if not.
async fn verified_release(cx: &Context, target: &str, floor: &str) -> Option<Installed> {
    for attempt in 1..=VERIFY_ATTEMPTS {
        let found = find(&cx.host, cx.platform);
        if let Some(version) = found
            .as_ref()
            .filter(|found| found.probe == Probe::Ok)
            .and_then(|found| found.version.as_deref())
            && !newer(target, version)
            && !newer(floor, version)
        {
            return found;
        }
        if attempt < VERIFY_ATTEMPTS {
            tokio::time::sleep(VERIFY_PAUSE).await;
        }
    }
    None
}

/// Python `update_antigravity_acp`: one guarded install through T3. `expected`
/// names the server version that must answer (the follow-up after a Linux
/// computer's deferred T3 restart); otherwise the T3 row decides.
async fn update_with(
    cx: &Context,
    install: &Installed,
    expected: Option<&str>,
    server: &dyn T3Server,
) -> AppRow {
    let before = install.version.clone();
    let mut attempted = false;
    cx.record(|record| {
        record.owner = Some(
            install
                .owner
                .clone()
                .unwrap_or_else(|| json!({"manager": "t3"})),
        )
    });
    let deadline = cx
        .deadline
        .min(Instant::now() + Duration::from_secs(INSTALL_SECONDS));
    let same = before.as_deref();
    if managed_root(&cx.host, cx.platform).is_none() {
        return row(install, Status::Failed, same, same, "unsupported", false);
    }
    if restart_pending(cx.t3_row.as_ref()) {
        // The running server is the one this run replaced: its offer is the previous server's.
        cx.record(|record| {
            record.blame(
                FailureCause::T3RestartPending,
                Some("deferred_server_restart"),
            )
        });
        return row(
            install,
            Status::ActionRequired,
            same,
            same,
            "t3_restart_pending",
            false,
        );
    }
    let outcome = through(cx, install, expected, deadline, server, &mut attempted).await;
    match outcome {
        Ok(row) => row,
        Err(error) => {
            let after = find(&cx.host, cx.platform).and_then(|found| found.version);
            if error.cause.is_some() || error.reason.is_some() {
                cx.record(|record| {
                    record.blame(error.cause.unwrap_or(FailureCause::Unknown), error.reason)
                });
            }
            let status = if matches!(
                error.code,
                "t3_unreachable" | "t3_auth_failed" | "t3_absent" | "t3_not_ready"
            ) {
                Status::ActionRequired
            } else {
                Status::Failed
            };
            row(
                install,
                status,
                same,
                after.as_deref(),
                error.code,
                attempted,
            )
        }
    }
}

async fn through(
    cx: &Context,
    install: &Installed,
    expected: Option<&str>,
    deadline: Instant,
    server: &dyn T3Server,
    attempted: &mut bool,
) -> Result<AppRow, UpdateFailure> {
    // A server replaced in this run gets time to come up; one left alone either answers or does not.
    cx.phase("t3_ready");
    let replaced = expected
        .filter(|expected| !expected.is_empty())
        .or_else(|| replaced_version(cx.t3_row.as_ref()));
    let seconds = if replaced.is_some() {
        READY_SECONDS
    } else {
        QUIET_READY_SECONDS
    };
    server.ready(cx, replaced, seconds).await?;
    cx.phase("discover");
    let mut client = server.open(cx).await?;
    let outcome = offer_and_install(cx, client.as_mut(), install, deadline, attempted).await;
    // Python's `with closing(...)`: a session that cannot be closed (its bearer
    // stays issued) replaces whatever the install itself said.
    client.close().await?;
    outcome
}

async fn offer_and_install(
    cx: &Context,
    client: &mut dyn T3Session,
    install: &Installed,
    deadline: Instant,
    attempted: &mut bool,
) -> Result<AppRow, UpdateFailure> {
    cx.phase("offer");
    let config = client.call("server.getConfig", json!({})).await?;
    if !config.is_object() {
        return Err(protocol());
    }
    let items: Vec<&Value> = match config.get("providers") {
        None | Some(Value::Null) => Vec::new(),
        Some(Value::Array(items)) => items.iter().collect(),
        Some(Value::String(_) | Value::Object(_)) => Vec::new(),
        Some(_) => return Err(protocol()),
    };
    let mut installable: Vec<&Value> = Vec::new();
    // T3 refreshes models after an install only for the instance it ran on, and only when enabled.
    let mut enabled: Vec<String> = Vec::new();
    for provider in items {
        if !provider.is_object() || provider["driver"] != "antigravity" {
            continue;
        }
        let can_install = match provider.get("setup") {
            None | Some(Value::Null) => false,
            Some(Value::Object(setup)) => setup.get("canInstall") == Some(&Value::Bool(true)),
            // Python's other falsy values read as an empty setup.
            Some(Value::Array(items)) if items.is_empty() => false,
            Some(Value::String(text)) if text.is_empty() => false,
            Some(Value::Number(number)) if number.as_f64() == Some(0.0) => false,
            Some(Value::Bool(false)) => false,
            Some(_) => return Err(protocol()),
        };
        if !can_install {
            continue;
        }
        installable.push(provider);
        if provider.get("enabled") == Some(&Value::Bool(true))
            && let Some(id) = provider.get("instanceId").and_then(Value::as_str)
        {
            enabled.push(id.to_owned());
        }
    }
    if enabled.is_empty() {
        return Err(unavailable(if installable.is_empty() {
            "no_installable_instance"
        } else {
            "no_enabled_instance"
        }));
    }
    // Install through an instance T3 manages (enabled first); refresh every enabled one after.
    let custom = custom_executable_instances(&cx.host);
    let managed: Vec<&str> = installable
        .iter()
        .filter_map(|provider| provider.get("instanceId").and_then(Value::as_str))
        .filter(|id| !custom.contains(*id))
        .collect();
    let instance = enabled
        .iter()
        .map(String::as_str)
        .find(|id| managed.contains(id))
        .or_else(|| managed.first().copied());
    let Some(instance) = instance else {
        return Err(unavailable("no_managed_instance"));
    };
    let offer = read_offer(client, instance).await?;
    let offered = offer
        .as_ref()
        .and_then(|offer| offer.get("version"))
        .and_then(Value::as_str)
        .filter(|version| SEMVER.is_match(version));
    let current = find(&cx.host, cx.platform);
    let Some(current) = current.filter(|current| current.probe == Probe::Ok) else {
        return Err(UpdateFailure::new("t3_install_failed:readiness_unknown")
            .with_cause(FailureCause::Unknown, Some("runtime_record_invalid")));
    };
    let Some(current_version) = current.version.clone() else {
        return Err(protocol());
    };
    let guarded =
        current.runtime_kind == Some("harness") && !SIGNIN_CARRIES_TO_NATIVE.contains(&cx.platform);
    // Google's channel is read only when T3 gave no usable offer, or when this
    // runtime pair could be replaced by the native CLI: T3's start re-reads
    // that channel itself.
    let newest = if offered.is_none() || guarded {
        newest_cli_version(cx, deadline).await
    } else {
        None
    };
    let known: Vec<&str> = [offered, newest.as_deref()].into_iter().flatten().collect();
    let target = known
        .iter()
        .copied()
        .max_by_key(|version| common::version_tuple(version));
    let target_word = if target.is_some() && target == offered {
        "t3-offer"
    } else {
        "cli-manifest"
    };
    cx.record(|record| {
        if let Some(owner) = record.owner.as_mut().and_then(Value::as_object_mut) {
            owner.insert("target".into(), Value::String(target_word.into()));
        }
    });
    if guarded {
        let native: Vec<&str> = known
            .iter()
            .copied()
            .filter(|version| native(version))
            .collect();
        // With no target at all nothing can start either: that is "offer unknown" below, not a sign-in.
        if !native.is_empty()
            || (newest.is_none() && target.is_some_and(|target| newer(target, &current_version)))
        {
            // Whatever the offer says, start may install the native CLI, which cannot read the
            // sign-in this runtime pair stored here (unless T3 carries the sign-in over).
            cx.record(|record| {
                record.blame(
                    FailureCause::SigninNeeded,
                    Some(if native.is_empty() {
                        "migration_unchecked"
                    } else {
                        "token_storage_change"
                    }),
                )
            });
            let mut held = row(
                install,
                Status::ActionRequired,
                install.version.as_deref(),
                Some(current_version.as_str()),
                "antigravity_acp_signin_hold",
                false,
            );
            if let Some(version) = native
                .iter()
                .copied()
                .max_by_key(|version| common::version_tuple(version))
                .or(target)
            {
                held.held_version = Some(version.to_owned());
            }
            return Ok(held);
        }
    }
    let Some(target) = target else {
        cx.record(|record| record.blame(FailureCause::Unknown, Some("no_offer")));
        return Ok(row(
            install,
            Status::Unknown,
            install.version.as_deref(),
            Some(current_version.as_str()),
            "antigravity_acp_offer_unknown",
            false,
        ));
    };
    // T3 would reinstall the same CLI: never call install for it.
    if !newer(target, &current_version) {
        return Ok(row(
            install,
            Status::Current,
            install.version.as_deref(),
            Some(current_version.as_str()),
            "current",
            false,
        ));
    }
    cx.phase("install");
    *attempted = true;
    let started = client
        .call("provider.install.start", json!({"instanceId": instance}))
        .await?;
    let state = checked_state(&started)?;
    let Some(operation) = state
        .get("operationId")
        .and_then(Value::as_str)
        .filter(|operation| !operation.is_empty())
    else {
        return Err(UpdateFailure::new("t3_install_failed:protocol")
            .with_cause(FailureCause::Unknown, Some("no_operation")));
    };
    // T3 re-reads the release at start; follow what this operation installs.
    let target = state
        .get("version")
        .and_then(Value::as_str)
        .filter(|version| SEMVER.is_match(version))
        .unwrap_or(target);
    let mut last = None;
    client
        .states(instance, &mut |value| {
            let state = checked_state(&value)?;
            if state.get("operationId").and_then(Value::as_str) != Some(operation) {
                return Err(UpdateFailure::new("t3_install_state_unknown")
                    .with_cause(FailureCause::Unknown, Some("operation_changed")));
            }
            let done = state
                .get("phase")
                .and_then(Value::as_str)
                .is_some_and(|phase| matches!(phase, "succeeded" | "failed" | "cancelled"));
            if done {
                last = Some(value.clone());
            }
            Ok(done)
        })
        .await?;
    let Some(state) = last else {
        // The subscription ended without a final state (Python's StopIteration).
        return Err(protocol());
    };
    if state["phase"] != "succeeded" {
        // Server messages can contain local paths or credentials.
        // Emit only a fixed reason, never arbitrary diagnostic text.
        return Err(UpdateFailure::new(if state["phase"] == "cancelled" {
            "t3_install_failed:cancelled"
        } else {
            "t3_install_failed:installer"
        }));
    }
    cx.phase("verify");
    // T3 must have activated at least what its operation named, and never gone backwards.
    let Some(found) = verified_release(cx, target, &current_version).await else {
        // T3 said it finished: have it re-read its runtime anyway, so its own provider
        // status shows what is on disk instead of the version before the install.
        refresh_models(client, &enabled).await;
        return Err(UpdateFailure::new("t3_install_failed:verification"));
    };
    cx.record(|record| {
        let target = record
            .owner
            .as_ref()
            .and_then(|owner| owner.get("target"))
            .cloned();
        let mut owner = found.owner.clone().unwrap_or_else(|| json!({}));
        if let Some(fields) = owner.as_object_mut() {
            fields.insert("target".into(), target.unwrap_or(Value::Null));
        }
        record.owner = Some(owner);
    });
    let Some(version) = found.version.clone() else {
        return Err(protocol());
    };
    if !newer(&version, &current_version) {
        // T3 only re-checked the installed runtime (a withdrawn release, its own manifest
        // read failed, a rollback): nothing new runs, so this is not an update.
        cx.record(|record| record.note("reverified"));
        return Ok(row(
            install,
            Status::Current,
            install.version.as_deref(),
            Some(version.as_str()),
            "current",
            true,
        ));
    }
    cx.phase("refresh_models");
    let (outcome, reason) = refresh_models(client, &enabled).await;
    let before = install.version.as_deref();
    match outcome {
        "signin" => {
            cx.record(|record| record.blame(FailureCause::SigninNeeded, reason));
            Ok(row(
                install,
                Status::ActionRequired,
                before,
                Some(version.as_str()),
                "antigravity_acp_signin_needed",
                true,
            ))
        }
        "failed" => {
            cx.record(|record| record.note(reason.unwrap_or("refresh_failed")));
            Ok(row(
                install,
                Status::Updated,
                before,
                Some(version.as_str()),
                "antigravity_acp_models_stale",
                true,
            ))
        }
        _ => Ok(row(
            install,
            Status::Updated,
            before,
            Some(version.as_str()),
            "antigravity_acp_updated",
            true,
        )),
    }
}

/// Python `follow_up_antigravity_acp`: Antigravity ACP against the T3 server this
/// run installed, after a Linux computer's deferred restart. The coordinator
/// runs it once the computer's part is done. `expected` names the T3 version
/// that must answer (`None`: whichever T3 answers). It waits, bounded, for that
/// server, then takes the helper lock and runs the same guarded install a run
/// does. AAC never restarts T3 or stops a T3-owned process here.
///
/// The context's deadline must leave room for the wait and the install: the
/// 36-minute follow-up worker budget does.
pub(super) async fn follow_up(cx: &Context, expected: Option<&str>) -> AppRow {
    follow_up_with(cx, expected, &Local).await
}

async fn follow_up_with(cx: &Context, expected: Option<&str>, server: &dyn T3Server) -> AppRow {
    let platform = cx.platform;
    let end = Instant::now()
        .checked_add(FOLLOW_UP_SECONDS)
        .unwrap_or(cx.deadline)
        .min(cx.deadline);
    let skipped = || AppRow::new(APP, platform, Status::Skipped).code("skipped_cancelled");
    let finish = |row: AppRow| {
        let mut row = row;
        if row.diagnostics.is_none() {
            let value = row.to_value();
            row.diagnostics = Some(cx.record(|record| record.export(&value)));
        }
        row
    };
    let Some(install) = find(&cx.host, platform) else {
        return finish(AppRow::new(APP, platform, Status::NotInstalled));
    };
    cx.record(|record| record.owner = install.owner.clone());
    if install.probe != Probe::Ok {
        cx.record(|record| {
            record.blame(
                FailureCause::Unknown,
                Some(install.probe_reason.unwrap_or("probe_failed")),
            )
        });
        return finish(AppRow::new(APP, platform, Status::Unknown).code("readiness_unknown"));
    }
    let before = install.version.clone();
    let before = before.as_deref();
    cx.phase("checking");
    cx.phase("t3_restart");
    loop {
        if cx.cancelled() {
            return finish(skipped());
        }
        let remaining = end.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            cx.record(|record| record.blame(FailureCause::T3NotReady, Some("restart_not_seen")));
            return finish(row(
                &install,
                Status::ActionRequired,
                before,
                before,
                "t3_not_ready",
                false,
            ));
        }
        match server
            .ready(cx, expected, FOLLOW_UP_POLL.min(remaining))
            .await
        {
            Ok(()) => break,
            Err(error) if error.code == "t3_not_ready" => {}
            Err(error) => {
                cx.record(|record| {
                    record.blame(
                        error.cause.unwrap_or(FailureCause::T3Absent),
                        Some(error.reason.unwrap_or("no_t3_install")),
                    )
                });
                return finish(row(
                    &install,
                    Status::ActionRequired,
                    before,
                    before,
                    error.code,
                    false,
                ));
            }
        }
    }
    loop {
        if cx.cancelled() {
            return finish(skipped());
        }
        match shared::execution_lock(&cx.state, "helper-update.lock") {
            Ok(_lock) => {
                let current = find(&cx.host, platform).unwrap_or_else(|| install.clone());
                if cx.cancelled() {
                    return finish(skipped());
                }
                cx.phase("updating");
                // The install keeps its own bound (15 minutes) from here, past the wait's.
                return finish(update_with(cx, &current, expected, server).await);
            }
            // The restart worker holds the lock while it restarts T3.
            Err(error) if error.code == "busy" => {}
            Err(error) => {
                return finish(row(
                    &install,
                    Status::Failed,
                    before,
                    before,
                    error.code,
                    false,
                ));
            }
        }
        if Instant::now() >= end {
            return finish(row(
                &install,
                Status::ActionRequired,
                before,
                before,
                "busy",
                false,
            ));
        }
        tokio::time::sleep(FOLLOW_UP_LOCK).await;
    }
}

// ---------------------------------------------------------------------------
// TEMPORARY (lane W4): the production binding is lane W5's `t3.rs`. Until its
// bundle lands the local client answers honestly that it is not there, so a
// row reads `action_required` / `t3_not_ready` and nothing is installed.
// ---------------------------------------------------------------------------

struct Local;

impl T3Server for Local {
    fn ready<'a>(
        &'a self,
        _cx: &'a Context,
        _expected: Option<&'a str>,
        _seconds: Duration,
    ) -> BoxFuture<'a, Result<(), UpdateFailure>> {
        Box::pin(async {
            Err(UpdateFailure::new("t3_not_ready")
                .with_cause(FailureCause::T3NotReady, Some("t3_rpc_not_ported")))
        })
    }

    fn open<'a>(
        &'a self,
        _cx: &'a Context,
    ) -> BoxFuture<'a, Result<Box<dyn T3Session + 'a>, UpdateFailure>> {
        Box::pin(async {
            Err(UpdateFailure::new("t3_not_ready")
                .with_cause(FailureCause::T3NotReady, Some("t3_rpc_not_ported")))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apps::updater::tests::{FakeSystem, context as base_context};
    use crate::apps::updater::{
        CommandOutput, CommandSpec, Diagnostics, Downloaded, Fingerprint, ProcessTable, System,
        attempt,
    };
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
    };

    const TOKEN: &str = "private-fixture-bearer-never-output";
    const SIGNIN: &str = "Antigravity model refresh timed out. Try again or check Google sign-in.";

    /// `FakeSystem` plus the one manifest `newest_cli_version` reads.
    struct Fixture {
        system: Arc<FakeSystem>,
        manifest: Mutex<Option<Vec<u8>>>,
        fetched: Mutex<Vec<(String, u64)>>,
    }

    impl Fixture {
        fn new(manifest: Option<Value>) -> Arc<Self> {
            Arc::new(Self {
                system: FakeSystem::answering(|_| panic!("the ACP updater runs no command")),
                manifest: Mutex::new(
                    manifest.map(|value| serde_json::to_vec(&value).expect("manifest")),
                ),
                fetched: Mutex::new(Vec::new()),
            })
        }
    }

    impl System for Fixture {
        fn run<'a>(
            &'a self,
            spec: &'a CommandSpec,
            diagnostics: &'a Mutex<Diagnostics>,
        ) -> BoxFuture<'a, Result<CommandOutput, UpdateFailure>> {
            self.system.run(spec, diagnostics)
        }

        fn processes(
            &self,
            platform: Platform,
        ) -> BoxFuture<'_, Result<ProcessTable, UpdateFailure>> {
            self.system.processes(platform)
        }

        fn download<'a>(
            &'a self,
            url: &'a str,
            destination: &'a Path,
            maximum: u64,
            timeout: Duration,
        ) -> BoxFuture<'a, Result<Downloaded, UpdateFailure>> {
            self.system.download(url, destination, maximum, timeout)
        }

        fn fetch<'a>(
            &'a self,
            url: &'a str,
            prefix: &'a str,
            maximum: u64,
            timeout: Duration,
        ) -> BoxFuture<'a, Result<Vec<u8>, UpdateFailure>> {
            Box::pin(async move {
                assert_eq!((prefix, maximum), (CLI_MANIFEST_BASE, MANIFEST_LIMIT));
                assert!(timeout <= MANIFEST_SECONDS);
                self.fetched
                    .lock()
                    .expect("fetched")
                    .push((url.to_owned(), maximum));
                self.manifest
                    .lock()
                    .expect("manifest")
                    .clone()
                    .ok_or_else(UpdateFailure::failed)
            })
        }

        fn fingerprint<'a>(
            &'a self,
            url: &'a str,
            timeout: Duration,
        ) -> BoxFuture<'a, Option<Fingerprint>> {
            self.system.fingerprint(url, timeout)
        }
    }

    fn context(
        root: &tempfile::TempDir,
        platform: Platform,
        system: Arc<dyn System>,
        cancelled: bool,
    ) -> Context {
        base_context(root.path(), platform, system, cancelled)
    }

    #[cfg(unix)]
    fn mode(path: &Path, bits: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(bits)).expect("mode");
    }

    #[cfg(not(unix))]
    fn mode(_: &Path, _: u32) {}

    /// Puts a T3 0.1.12 release folder back before its temporary root goes away.
    struct Writable(PathBuf);

    impl Drop for Writable {
        fn drop(&mut self) {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o700));
            }
        }
    }

    fn key(platform: Platform) -> &'static str {
        match platform {
            Platform::Ubuntu => "linux-x64",
            Platform::Mac => "darwin-x64",
            Platform::Windows => "win32-x64",
        }
    }

    fn managed(root: &Path, platform: Platform) -> PathBuf {
        root.join("home/.t3/tools/antigravity-acp")
            .join(key(platform))
    }

    /// Python `AcpTests.install_record`; `shape` `Some("0.1.12")` writes T3
    /// 0.1.12's layout (a read-only release folder, a 0400 record, 0555 executables).
    fn install_record(
        root: &Path,
        platform: Platform,
        version: &str,
        native: bool,
        release: &str,
        shape: Option<&str>,
    ) -> PathBuf {
        let directory = managed(root, platform).join("versions").join(release);
        std::fs::create_dir_all(&directory).expect("release folder");
        let windows = platform == Platform::Windows;
        let names: Vec<(&str, String)> = if native {
            vec![(
                "executable",
                if windows { "agy.exe" } else { "antigravity" }.to_owned(),
            )]
        } else {
            vec![
                (
                    "executable",
                    format!("agy_acp_server{}", if windows { ".exe" } else { ".par" }),
                ),
                (
                    "harness",
                    format!("localharness_external{}", if windows { ".exe" } else { "" }),
                ),
            ]
        };
        let mut record = json!({"releaseId": release, "version": version});
        for (kind, name) in &names {
            std::fs::write(directory.join(name), b"bin").expect("artifact");
            mode(&directory.join(name), 0o755);
            record[kind] = json!({"name": name, "bytes": 3});
        }
        if native && shape != Some("0.1.12") {
            record["harness"] = Value::Null;
        }
        common::write_private_json(&directory.join(RECORD), &record).expect("record");
        if shape == Some("0.1.12") {
            mode(&directory.join(RECORD), 0o400);
            for (_, name) in &names {
                mode(&directory.join(name), 0o555);
            }
            mode(&directory, 0o500);
        }
        directory
    }

    /// Python `AcpTests.runtime`: the runtime T3 has active.
    fn runtime(
        root: &Path,
        platform: Platform,
        version: &str,
        native: bool,
        shape: Option<&str>,
    ) -> PathBuf {
        let directory = install_record(root, platform, version, native, &"a".repeat(64), shape);
        pointer(root, platform, &"a".repeat(64));
        directory
    }

    fn pointer(root: &Path, platform: Platform, release: &str) {
        common::write_private_json(
            &managed(root, platform).join("active.json"),
            &json!({"releaseId": release}),
        )
        .expect("active");
    }

    fn active(root: &Path, platform: Platform) -> Vec<u8> {
        std::fs::read(managed(root, platform).join("active.json")).expect("active")
    }

    fn settings(root: &Path, instances: Value) {
        let path = root.join("home/.t3/userdata/settings.json");
        std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
        std::fs::write(
            path,
            serde_json::to_vec(&json!({"providerInstances": instances})).expect("json"),
        )
        .expect("settings");
    }

    fn providers() -> Value {
        json!([
            {"driver": "codex", "instanceId": "wrong", "enabled": true, "setup": {"canInstall": true}},
            {"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": true, "setup": {"canInstall": true}},
            {"driver": "antigravity", "instanceId": "antigravity", "enabled": false, "setup": {"canInstall": true}},
            {"driver": "antigravity", "instanceId": "agy-second-id", "enabled": true, "setup": {"canInstall": true}},
        ])
    }

    /// What the fake T3 answers (Python `AcpTests.fake_t3`).
    #[derive(Clone)]
    struct Script {
        providers: Value,
        install_on: &'static str,
        /// The offer: the idle state's version (`None` until T3's release check ends).
        offer: Option<&'static str>,
        /// What the fake T3 activates.
        installed: &'static str,
        phase: &'static str,
        failure: Option<&'static str>,
        first_null: bool,
        refresh: Option<&'static str>,
        activate_late: bool,
        activate_shape: Option<&'static str>,
        operation: &'static str,
        ready: Option<UpdateFailure>,
        revoke_fails: bool,
        /// The version T3's start reply names (it re-reads the release at start).
        start_version: Option<&'static str>,
    }

    impl Default for Script {
        fn default() -> Self {
            Self {
                providers: providers(),
                install_on: "agy-custom-id",
                offer: Some("1.5.8"),
                installed: "1.5.8",
                phase: "succeeded",
                failure: None,
                first_null: false,
                refresh: None,
                activate_late: false,
                activate_shape: None,
                operation: "op",
                ready: None,
                revoke_fails: false,
                start_version: None,
            }
        }
    }

    fn set(mut value: Value, key: &str, item: impl Into<Value>) -> Value {
        value[key] = item.into();
        value
    }

    fn idle(script: &Script, installed: &str) -> Value {
        json!({
            "driver": "antigravity", "operationId": null, "phase": "idle", "downloadedBytes": 0,
            "totalBytes": null, "version": script.offer, "installedVersion": installed,
            "canRemove": true, "message": null,
        })
    }

    struct Inner {
        script: Script,
        root: PathBuf,
        platform: Platform,
        installed: String,
        calls: Mutex<Vec<(String, Value)>>,
        ready: Mutex<Vec<(Option<String>, Duration)>>,
        started: AtomicUsize,
        restore: Mutex<Vec<Writable>>,
    }

    #[derive(Clone)]
    struct T3(Arc<Inner>);

    impl T3 {
        fn new(root: &Path, platform: Platform, installed: &str, script: Script) -> Self {
            Self(Arc::new(Inner {
                script,
                root: root.to_path_buf(),
                platform,
                installed: installed.to_owned(),
                calls: Mutex::new(Vec::new()),
                ready: Mutex::new(Vec::new()),
                started: AtomicUsize::new(0),
                restore: Mutex::new(Vec::new()),
            }))
        }

        fn calls(&self) -> Vec<(String, Value)> {
            self.0.calls.lock().expect("calls").clone()
        }

        /// The instances each method was asked about, in order.
        fn asked(&self, method: &str) -> Vec<String> {
            self.calls()
                .iter()
                .filter(|(name, _)| name == method)
                .map(|(_, payload)| {
                    payload["instanceId"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned()
                })
                .collect()
        }

        fn started(&self) -> usize {
            self.0.started.load(Ordering::SeqCst)
        }

        fn windows(&self) -> Vec<(Option<String>, Duration)> {
            self.0.ready.lock().expect("ready").clone()
        }

        /// What T3 does when its operation succeeds: activate `installed`.
        fn activate(&self) {
            let script = &self.0.script;
            let release = "b".repeat(64);
            let directory = install_record(
                &self.0.root,
                self.0.platform,
                script.installed,
                native(script.installed),
                &release,
                script.activate_shape,
            );
            if script.activate_shape.is_some() {
                self.0
                    .restore
                    .lock()
                    .expect("restore")
                    .push(Writable(directory));
            }
            pointer(&self.0.root, self.0.platform, &release);
        }
    }

    struct Session {
        t3: T3,
        started: bool,
    }

    impl T3Session for Session {
        fn call<'a>(
            &'a mut self,
            method: &'a str,
            payload: Value,
        ) -> BoxFuture<'a, Result<Value, UpdateFailure>> {
            Box::pin(async move {
                let inner = &self.t3.0;
                let script = inner.script.clone();
                inner
                    .calls
                    .lock()
                    .expect("calls")
                    .push((method.to_owned(), payload.clone()));
                // The CLI failed (it did not answer with an empty reply): not a lost reply.
                if script.failure == Some("auth") {
                    return Err(UpdateFailure::new("t3_session_issue_unknown")
                        .with_cause(FailureCause::Unknown, Some("issuer_failed")));
                }
                match method {
                    "server.getConfig" => Ok(json!({"providers": script.providers})),
                    "server.refreshProviders" => {
                        assert_eq!(payload["refreshModels"], true);
                        match script.refresh {
                            Some(refresh) => Err(UpdateFailure {
                                output: format!("ProviderSetupError\n{refresh}"),
                                ..UpdateFailure::new("t3_install_failed:rpc")
                            }),
                            None => Ok(json!({"providers": [{
                                "instanceId": payload["instanceId"], "driver": "antigravity",
                                "auth": {"status": "authenticated"},
                            }]})),
                        }
                    }
                    "provider.install.start" => {
                        assert_eq!(payload, json!({"instanceId": script.install_on}));
                        inner.started.fetch_add(1, Ordering::SeqCst);
                        self.started = true;
                        if script.failure == Some("ambiguous") {
                            return Err(UpdateFailure::new("t3_install_state_unknown"));
                        }
                        let started = set(
                            set(idle(&script, &inner.installed), "operationId", "op"),
                            "phase",
                            "downloading",
                        );
                        Ok(match script.start_version {
                            Some(version) => set(started, "version", version),
                            None => started,
                        })
                    }
                    other => panic!("unexpected request {other}"),
                }
            })
        }

        fn states<'a>(
            &'a mut self,
            _instance: &'a str,
            take: &'a mut (dyn FnMut(Value) -> Result<bool, UpdateFailure> + Send),
        ) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async move {
                let inner = &self.t3.0;
                let script = inner.script.clone();
                if !self.started {
                    return take(idle(&script, &inner.installed)).map(|_| ());
                }
                match script.failure {
                    Some("timeout") => Err(UpdateFailure::new("t3_install_timeout")),
                    Some("denied") => Err(UpdateFailure::new("t3_auth_failed")),
                    _ => {
                        if script.phase == "succeeded" && !script.activate_late {
                            self.t3.activate();
                        }
                        take(set(
                            set(
                                set(
                                    idle(&script, &inner.installed),
                                    "operationId",
                                    script.operation,
                                ),
                                "phase",
                                script.phase,
                            ),
                            "message",
                            TOKEN,
                        ))
                        .map(|_| ())
                    }
                }
            })
        }

        fn offer<'a>(
            &'a mut self,
            _instance: &'a str,
            _settle: Duration,
            take: &'a mut (dyn FnMut(Value) -> Result<bool, UpdateFailure> + Send),
        ) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async move {
                let inner = &self.t3.0;
                let script = inner.script.clone();
                if !self.started && script.first_null {
                    take(set(idle(&script, &inner.installed), "version", Value::Null))?;
                }
                take(idle(&script, &inner.installed)).map(|_| ())
            })
        }

        fn close<'a>(self: Box<Self>) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async move {
                if self.t3.0.script.revoke_fails {
                    Err(UpdateFailure::new("t3_session_revoke_failed"))
                } else {
                    Ok(())
                }
            })
        }
    }

    impl T3Server for T3 {
        fn ready<'a>(
            &'a self,
            _cx: &'a Context,
            expected: Option<&'a str>,
            seconds: Duration,
        ) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async move {
                self.0
                    .ready
                    .lock()
                    .expect("ready")
                    .push((expected.map(str::to_owned), seconds));
                match &self.0.script.ready {
                    Some(failure) => Err(failure.clone()),
                    None => Ok(()),
                }
            })
        }

        fn open<'a>(
            &'a self,
            _cx: &'a Context,
        ) -> BoxFuture<'a, Result<Box<dyn T3Session + 'a>, UpdateFailure>> {
            Box::pin(async move {
                if self.0.script.failure == Some("unreachable") {
                    return Err(UpdateFailure::new("t3_unreachable"));
                }
                Ok(Box::new(Session {
                    t3: self.clone(),
                    started: false,
                }) as Box<dyn T3Session + 'a>)
            })
        }
    }

    /// A T3 server that never answers: each poll waits its window, then says so.
    struct Never;

    impl T3Server for Never {
        fn ready<'a>(
            &'a self,
            _cx: &'a Context,
            _expected: Option<&'a str>,
            seconds: Duration,
        ) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async move {
                tokio::time::sleep(seconds).await;
                Err(UpdateFailure::new("t3_not_ready")
                    .with_cause(FailureCause::T3NotReady, Some("connection_refused")))
            })
        }

        fn open<'a>(
            &'a self,
            _cx: &'a Context,
        ) -> BoxFuture<'a, Result<Box<dyn T3Session + 'a>, UpdateFailure>> {
            Box::pin(async { Err(UpdateFailure::new("t3_not_ready")) })
        }
    }

    fn finish(mut row: AppRow, cx: &Context) -> Value {
        if row.diagnostics.is_none() {
            let value = row.to_value();
            row.diagnostics = Some(cx.record(|record| record.export(&value)));
        }
        row.to_value()
    }

    /// The follow-up's row.
    async fn follow(cx: &Context, expected: Option<&str>, t3: &dyn T3Server) -> Value {
        follow_up_with(cx, expected, t3).await.to_value()
    }

    /// One row, the way `attempt` runs it: owner, phases and diagnostics.
    async fn run(cx: &Context, t3: &T3) -> Value {
        let install = find(&cx.host, cx.platform).expect("found");
        cx.record(|record| record.owner = install.owner.clone());
        cx.phase("checking");
        cx.phase("updating");
        finish(update_with(cx, &install, None, t3).await, cx)
    }

    /// `(status, messageCode, previousVersion, version, updateAttempted)`.
    fn outcome(row: &Value) -> (Option<&str>, Option<&str>, Option<&str>, Option<&str>, bool) {
        (
            row["status"].as_str(),
            row["messageCode"].as_str(),
            row["previousVersion"].as_str(),
            row["version"].as_str(),
            row["updateAttempted"] == true,
        )
    }

    fn at<'a>(row: &'a Value, path: &[&str]) -> Option<&'a str> {
        let mut value = row;
        for key in path {
            value = &value[*key];
        }
        value.as_str()
    }

    // --- record shapes (lead fix 3) ---------------------------------------------------------------

    #[test]
    fn detects_both_record_shapes_t3_writes() {
        // AcpTests.test_detects_both_record_shapes_t3_writes
        for (platform, version, native) in [
            (Platform::Ubuntu, "1.4.5", false),
            (Platform::Ubuntu, "1.5.8", true),
            (Platform::Windows, "1.5.8", true),
            (Platform::Windows, "1.3.0", false),
            (Platform::Mac, "1.5.8", true),
        ] {
            let root = tempfile::tempdir().expect("root");
            let cx = context(&root, platform, Fixture::new(None), false);
            runtime(root.path(), platform, version, native, None);
            let found = find(&cx.host, platform).expect("found");
            let kind = if native { "native" } else { "harness" };
            assert_eq!(
                (found.version.as_deref(), found.probe, found.runtime_kind),
                (Some(version), Probe::Ok, Some(kind)),
                "{platform:?}"
            );
            assert_eq!(found.owner, Some(json!({"manager": "t3", "runtime": kind})));
            assert_eq!(
                (found.manager, found.path),
                (MANAGER, managed(root.path(), platform))
            );
        }
    }

    #[test]
    fn a_native_record_with_the_wrong_executable_or_size_is_unknown() {
        // AcpTests.test_a_native_record_with_the_wrong_executable_or_size_is_unknown
        let root = tempfile::tempdir().expect("root");
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let directory = runtime(root.path(), Platform::Ubuntu, "1.5.8", true, None);
        let record = read_object(&directory.join(RECORD), RECORD_LIMIT).expect("record");
        for executable in [
            json!({"name": "agy.exe", "bytes": 3}),
            json!({"name": "antigravity", "bytes": 4}),
            json!({"name": "antigravity", "bytes": "3"}),
        ] {
            let mut next = record.clone();
            next.insert("executable".into(), executable.clone());
            common::write_private_json(&directory.join(RECORD), &Value::Object(next))
                .expect("record");
            let found = find(&cx.host, Platform::Ubuntu).expect("found");
            assert_eq!(
                (found.probe, found.probe_reason, found.runtime_kind),
                (Probe::Failed, Some("runtime_record_invalid"), None),
                "{executable}"
            );
        }
    }

    /// Captured read-only from the Mac on 2026-11-20 after T3 0.1.12
    /// (0.1.12-nightly.20261120.4994) installed 1.5.13.
    #[test]
    fn detects_t3_0112s_native_record_that_has_no_harness_key() {
        // AcpTests.test_detects_t3_0112s_native_record_that_has_no_harness_key
        for (platform, name) in [
            (Platform::Mac, "antigravity"),
            (Platform::Windows, "agy.exe"),
            (Platform::Ubuntu, "antigravity"),
        ] {
            let root = tempfile::tempdir().expect("root");
            let cx = context(&root, platform, Fixture::new(None), false);
            let directory = runtime(root.path(), platform, "1.5.13", true, Some("0.1.12"));
            let _writable = Writable(directory);
            let directory = managed(root.path(), platform)
                .join("versions")
                .join("a".repeat(64));
            let record = read_object(&directory.join(RECORD), RECORD_LIMIT).expect("record");
            assert!(!record.contains_key("harness"));
            assert_eq!(record["executable"]["name"], name);
            let found = find(&cx.host, platform).expect("found");
            assert_eq!(
                (found.version.as_deref(), found.probe, found.runtime_kind),
                (Some("1.5.13"), Probe::Ok, Some("native")),
                "{platform:?}"
            );
            if platform == Platform::Mac {
                let keys: Vec<&String> = record.keys().collect();
                assert_eq!(keys, ["releaseId", "version", "executable"]);
                assert_eq!(record["executable"], json!({"name": name, "bytes": 3}));
            }
        }
    }

    #[test]
    fn a_record_without_a_harness_key_must_still_name_the_native_cli() {
        // AcpTests.test_a_record_without_a_harness_key_must_still_name_the_native_cli
        let root = tempfile::tempdir().expect("root");
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let directory = runtime(
            root.path(),
            Platform::Ubuntu,
            "1.5.13",
            true,
            Some("0.1.12"),
        );
        let _writable = Writable(directory.clone());
        // The old pair's executable with no harness is neither shape T3 writes: still unknown, never native.
        mode(&directory, 0o700);
        mode(&directory.join(RECORD), 0o600);
        std::fs::write(directory.join("agy_acp_server.par"), b"bin").expect("artifact");
        mode(&directory.join("agy_acp_server.par"), 0o755);
        common::write_private_json(
            &directory.join(RECORD),
            &json!({
                "releaseId": directory.file_name(),
                "version": "1.4.5",
                "executable": {"name": "agy_acp_server.par", "bytes": 3},
            }),
        )
        .expect("record");
        let found = find(&cx.host, Platform::Ubuntu).expect("found");
        assert_eq!((found.probe, found.runtime_kind), (Probe::Failed, None));
    }

    #[cfg(unix)]
    #[test]
    fn linked_records_and_pointers_are_unknown() {
        // Python read_json / completed: a link is never followed.
        let root = tempfile::tempdir().expect("root");
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let directory = runtime(root.path(), Platform::Ubuntu, "1.5.8", true, None);
        let active = managed(root.path(), Platform::Ubuntu).join("active.json");
        let elsewhere = root.path().join("elsewhere.json");
        std::fs::rename(&active, &elsewhere).expect("move the pointer");
        std::os::unix::fs::symlink(&elsewhere, &active).expect("link");
        assert_eq!(
            find(&cx.host, Platform::Ubuntu).expect("found").probe,
            Probe::Failed
        );
        std::fs::remove_file(&active).expect("unlink");
        std::fs::rename(&elsewhere, &active).expect("restore the pointer");
        let record = directory.join(RECORD);
        std::fs::rename(&record, &elsewhere).expect("move the record");
        std::os::unix::fs::symlink(&elsewhere, &record).expect("link");
        assert_eq!(
            find(&cx.host, Platform::Ubuntu).expect("found").probe,
            Probe::Failed
        );
    }

    #[tokio::test]
    async fn an_unknown_pointer_is_unknown_and_absence_does_not_install() {
        // AcpTests.test_unknown_pointer_is_unknown_and_absence_does_not_install
        let root = tempfile::tempdir().expect("root");
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        assert!(find(&cx.host, Platform::Ubuntu).is_none());
        assert_eq!(
            attempt(&ACP, &cx).await.to_value()["status"],
            "not_installed"
        );
        runtime(root.path(), Platform::Ubuntu, "1.4.5", false, None);
        pointer(root.path(), Platform::Ubuntu, "../outside");
        let found = find(&cx.host, Platform::Ubuntu).expect("found");
        assert_eq!(
            (found.probe, found.probe_reason),
            (Probe::Failed, Some("runtime_record_invalid"))
        );
        let row = attempt(&ACP, &cx).await.to_value();
        assert_eq!(
            (
                at(&row, &["status"]),
                at(&row, &["messageCode"]),
                at(&row, &["diagnostics", "reason"]),
            ),
            (
                Some("unknown"),
                Some("readiness_unknown"),
                Some("runtime_record_invalid")
            )
        );
    }

    #[test]
    fn unreadable_t3_settings_name_no_custom_executable() {
        // AcpTests.test_unreadable_t3_settings_name_no_custom_executable
        let root = tempfile::tempdir().expect("root");
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let path = root.path().join("home/.t3/userdata/settings.json");
        std::fs::create_dir_all(path.parent().expect("folder")).expect("folder");
        for text in [
            "{",
            "[]",
            r#"{"providerInstances": []}"#,
            r#"{"providerInstances": {"a": {"config": "x"}}}"#,
        ] {
            std::fs::write(&path, text).expect("settings");
            assert!(custom_executable_instances(&cx.host).is_empty(), "{text}");
        }
        std::fs::remove_file(&path).expect("remove");
        assert!(custom_executable_instances(&cx.host).is_empty());
        settings(
            root.path(),
            json!({
                "agy-custom-id": {"driver": "antigravity", "enabled": true,
                                  "config": {"authMethod": "oauth-personal", "binaryPath": " C:\\w\\agy.cmd "}},
                "agy-second-id": {"config": {"binaryPath": "  "}},
                "other": {"driver": "codex", "config": {"binaryPath": 7}},
                "last": ["not", "an", "object"],
            }),
        );
        assert_eq!(
            custom_executable_instances(&cx.host)
                .into_iter()
                .collect::<Vec<_>>(),
            ["agy-custom-id"]
        );
    }

    // --- target and instance (lead fixes 1 and 2) -----------------------------------------------

    #[tokio::test]
    async fn installs_t3s_offer_on_an_enabled_instance_and_refreshes_every_enabled_one() {
        // AcpTests.test_installs_t3s_offer_on_an_enabled_instance_and_refreshes_every_enabled_one
        let root = tempfile::tempdir().expect("root");
        let old = runtime(root.path(), Platform::Windows, "1.3.0", false, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.3.0",
            Script {
                providers: json!([
                    {"driver": "codex", "instanceId": "wrong", "enabled": true, "setup": {"canInstall": true}},
                    {"driver": "antigravity", "instanceId": "antigravity", "enabled": false, "setup": {"canInstall": true}},
                    {"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": true, "setup": {"canInstall": true}},
                    {"driver": "antigravity", "instanceId": "agy-second-id", "enabled": true, "setup": {"canInstall": true}},
                ]),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("updated"),
                Some("antigravity_acp_updated"),
                Some("1.3.0"),
                Some("1.5.8"),
                true
            )
        );
        // T3 was not replaced in this run: one short readiness window, no version expectation.
        assert_eq!(t3.windows(), vec![(None, QUIET_READY_SECONDS)]);
        // Never the disabled default instance T3 lists first: T3 would skip its model refresh.
        assert!(
            t3.calls()
                .iter()
                .all(|(_, payload)| payload["instanceId"] != "antigravity")
        );
        assert_eq!(
            t3.asked("server.refreshProviders"),
            ["agy-custom-id", "agy-second-id"]
        );
        assert_eq!(t3.started(), 1);
        assert_eq!(
            find(&cx.host, Platform::Windows)
                .expect("found")
                .runtime_kind,
            Some("native")
        );
        assert!(old.is_dir());
        assert_eq!(
            at(&row, &["diagnostics", "owner", "target"]),
            Some("t3-offer")
        );
        assert_eq!(at(&row, &["diagnostics", "cause"]), Some("none"));
        assert!(!row.to_string().contains(TOKEN));
    }

    #[tokio::test]
    async fn a_null_offer_waits_for_t3s_check_and_falls_back_to_the_cli_manifest() {
        // AcpTests.test_a_null_offer_waits_for_t3s_check_and_falls_back_to_the_cli_manifest
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.7", true, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.7",
            Script {
                offer: None,
                installed: "1.5.8",
                first_null: true,
                ..Script::default()
            },
        );
        let system = Fixture::new(Some(
            json!({"version": "1.5.8", "url": "https://example.invalid/cli"}),
        ));
        let cx = context(&root, Platform::Windows, system.clone(), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            (
                at(&row, &["status"]),
                at(&row, &["version"]),
                at(&row, &["diagnostics", "owner", "target"]),
            ),
            (Some("updated"), Some("1.5.8"), Some("cli-manifest"))
        );
        assert_eq!(
            system.fetched.lock().expect("fetched")[0].0,
            format!("{CLI_MANIFEST_BASE}windows_amd64.json")
        );

        // Without a channel answer either, nothing can be known and nothing starts.
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.7", true, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.7",
            Script {
                offer: None,
                first_null: true,
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("unknown"),
                Some("antigravity_acp_offer_unknown"),
                Some("1.5.7"),
                Some("1.5.7"),
                false
            )
        );
        assert_eq!(at(&row, &["diagnostics", "reason"]), Some("no_offer"));
        assert_eq!(t3.started(), 0);
    }

    #[tokio::test]
    async fn current_or_newer_never_calls_install() {
        // AcpTests.test_current_or_newer_never_calls_install_because_t3_would_reinstall_the_same_cli
        for (installed, offer) in [("1.5.8", "1.5.8"), ("1.5.9", "1.5.8")] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), Platform::Windows, installed, true, None);
            let before = active(root.path(), Platform::Windows);
            let t3 = T3::new(
                root.path(),
                Platform::Windows,
                installed,
                Script {
                    offer: Some(offer),
                    ..Script::default()
                },
            );
            let cx = context(&root, Platform::Windows, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            assert_eq!(
                outcome(&row),
                (
                    Some("current"),
                    Some("current"),
                    Some(installed),
                    Some(installed),
                    false
                )
            );
            assert_eq!(t3.started(), 0);
            assert_eq!(active(root.path(), Platform::Windows), before);
        }
    }

    #[tokio::test]
    async fn installs_through_a_managed_instance_when_the_enabled_ones_run_a_custom_executable() {
        // AcpTests.test_installs_through_a_managed_instance_when_the_enabled_ones_run_a_custom_executable
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.7", true, None);
        let wrapper = json!({"driver": "antigravity", "enabled": true,
                             "config": {"authMethod": "oauth-personal", "binaryPath": "C:\\w\\agy.cmd"}});
        settings(
            root.path(),
            json!({
                "agy-custom-id": wrapper.clone(), "agy-second-id": wrapper,
                "other": {"driver": "codex", "config": {"binaryPath": " "}},
            }),
        );
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.7",
            Script {
                install_on: "antigravity",
                providers: json!([
                    {"driver": "antigravity", "instanceId": "antigravity", "enabled": false, "setup": {"canInstall": true}},
                    {"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": true, "setup": {"canInstall": true}},
                    {"driver": "antigravity", "instanceId": "agy-second-id", "enabled": true, "setup": {"canInstall": true}},
                ]),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            (at(&row, &["status"]), at(&row, &["version"])),
            (Some("updated"), Some("1.5.8"))
        );
        assert_eq!(t3.asked("provider.install.start"), ["antigravity"]);
        assert_eq!(
            t3.asked("server.refreshProviders"),
            ["agy-custom-id", "agy-second-id"]
        );
    }

    #[tokio::test]
    async fn an_enabled_managed_instance_still_comes_before_the_disabled_default() {
        // AcpTests.test_an_enabled_managed_instance_still_comes_before_the_disabled_default
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.7", true, None);
        settings(
            root.path(),
            json!({
                "agy-custom-id": {"config": {"binaryPath": "/w/antigravity"}},
                "agy-second-id": {"config": {"binaryPath": ""}},
            }),
        );
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.7",
            Script {
                install_on: "agy-second-id",
                providers: json!([
                    {"driver": "antigravity", "instanceId": "antigravity", "enabled": false, "setup": {"canInstall": true}},
                    {"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": true, "setup": {"canInstall": true}},
                    {"driver": "antigravity", "instanceId": "agy-second-id", "enabled": true, "setup": {"canInstall": true}},
                ]),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        assert_eq!(run(&cx, &t3).await["status"], "updated");
        assert_eq!(t3.started(), 1);
    }

    #[tokio::test]
    async fn no_managed_or_enabled_instance_fails_before_any_install() {
        // AcpTests.test_every_installable_instance_on_a_custom_executable_fails_before_any_install
        // AcpTests.test_no_enabled_instance_fails_before_any_install
        for (providers, reason) in [
            (
                json!([{"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": true, "setup": {"canInstall": true}}]),
                "no_managed_instance",
            ),
            (
                json!([{"driver": "antigravity", "instanceId": "antigravity", "enabled": false, "setup": {"canInstall": true}}]),
                "no_enabled_instance",
            ),
            (
                json!([{"driver": "antigravity", "instanceId": "antigravity", "enabled": true, "setup": {"canInstall": false}}]),
                "no_installable_instance",
            ),
        ] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), Platform::Windows, "1.3.0", false, None);
            settings(
                root.path(),
                json!({"agy-custom-id": {"config": {"binaryPath": "/w/antigravity"}}}),
            );
            let t3 = T3::new(
                root.path(),
                Platform::Windows,
                "1.3.0",
                Script {
                    providers,
                    ..Script::default()
                },
            );
            let cx = context(&root, Platform::Windows, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            assert_eq!(
                (
                    at(&row, &["status"]),
                    at(&row, &["messageCode"]),
                    at(&row, &["diagnostics", "reason"]),
                ),
                (
                    Some("failed"),
                    Some("t3_install_failed:provider_unavailable"),
                    Some(reason)
                ),
                "{reason}"
            );
            assert_eq!(t3.started(), 0, "{reason}");
        }
    }

    #[tokio::test]
    async fn an_answer_t3_cannot_give_is_a_protocol_failure() {
        // Python: a non-iterable provider list or a truthy non-dict setup raises inside the install.
        for providers in [
            json!(5),
            json!([{"driver": "antigravity", "instanceId": "agy-custom-id", "enabled": true, "setup": "yes"}]),
        ] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), Platform::Windows, "1.5.7", true, None);
            let t3 = T3::new(
                root.path(),
                Platform::Windows,
                "1.5.7",
                Script {
                    providers: providers.clone(),
                    ..Script::default()
                },
            );
            let cx = context(&root, Platform::Windows, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            assert_eq!(
                (at(&row, &["status"]), at(&row, &["messageCode"]),),
                (Some("failed"), Some("t3_install_failed:protocol")),
                "{providers}"
            );
            assert_eq!(t3.started(), 0);
        }
    }

    // --- sign-in (lead fix 4) ---------------------------------------------------------------------

    fn notes(row: &Value) -> Vec<String> {
        row["diagnostics"]["notes"]
            .as_array()
            .map(|notes| {
                notes
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn linux_and_mac_hold_the_move_to_the_native_cli_and_windows_makes_it() {
        // AcpTests.test_linux_and_mac_hold_the_move_to_the_native_cli_and_windows_makes_it
        for platform in [Platform::Ubuntu, Platform::Mac, Platform::Windows] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), platform, "1.4.5", false, None);
            let before = active(root.path(), platform);
            let t3 = T3::new(root.path(), platform, "1.4.5", Script::default());
            let cx = context(&root, platform, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            if platform == Platform::Windows {
                assert_eq!(
                    (at(&row, &["status"]), at(&row, &["version"])),
                    (Some("updated"), Some("1.5.8"))
                );
                continue;
            }
            assert_eq!(
                outcome(&row),
                (
                    Some("action_required"),
                    Some("antigravity_acp_signin_hold"),
                    Some("1.4.5"),
                    Some("1.4.5"),
                    false
                ),
                "{platform:?}"
            );
            assert_eq!(row["heldVersion"], "1.5.8", "{platform:?}");
            assert_eq!(
                (
                    at(&row, &["diagnostics", "cause"]),
                    at(&row, &["diagnostics", "reason"]),
                ),
                (Some("signin_needed"), Some("token_storage_change")),
                "{platform:?}"
            );
            assert_eq!(t3.started(), 0, "{platform:?}");
            assert_eq!(active(root.path(), platform), before);
        }
    }

    #[tokio::test]
    async fn only_a_native_channel_version_is_an_answer() {
        // Python newest_cli_version: T3's parseAntigravityCliRelease refuses anything older
        // than 1.5.0, so a channel below it is no answer (the runtime pair is then held).
        for (platform, target) in [
            (Platform::Ubuntu, "linux_amd64"),
            (Platform::Mac, "darwin_amd64"),
            (Platform::Windows, "windows_amd64"),
        ] {
            for (manifest, answer) in [
                (json!({"version": "1.4.7"}), None),
                (json!({"version": "1.5.9"}), Some("1.5.9")),
                (json!({"version": "1.6"}), None),
                (json!({"version": "1.5.9-beta.1"}), None),
                (json!({"version": 7}), None),
                (json!("1.5.9"), None),
                (Value::Null, None),
            ] {
                let root = tempfile::tempdir().expect("root");
                let system = Fixture::new(Some(manifest.clone()));
                let cx = context(&root, platform, system.clone(), false);
                assert_eq!(
                    newest_cli_version(&cx, cx.deadline).await,
                    answer.map(str::to_owned),
                    "{platform:?} {manifest}"
                );
                assert_eq!(
                    system.fetched.lock().expect("fetched")[0].0,
                    format!("{CLI_MANIFEST_BASE}{target}.json")
                );
            }
        }
        // No channel answer, or no time left to read one.
        let root = tempfile::tempdir().expect("root");
        let system = Fixture::new(None);
        let cx = context(&root, Platform::Ubuntu, system.clone(), false);
        assert_eq!(newest_cli_version(&cx, cx.deadline).await, None);
        assert_eq!(system.fetched.lock().expect("fetched").len(), 1);
        let mut cx = context(&root, Platform::Ubuntu, system.clone(), false);
        cx.deadline = Instant::now();
        assert_eq!(newest_cli_version(&cx, cx.deadline).await, None);
        assert_eq!(system.fetched.lock().expect("fetched").len(), 1);
    }

    #[tokio::test]
    async fn an_older_offer_with_no_channel_answer_is_held_as_unchecked() {
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Ubuntu, "1.4.5", false, None);
        let before = active(root.path(), Platform::Ubuntu);
        let t3 = T3::new(
            root.path(),
            Platform::Ubuntu,
            "1.4.5",
            Script {
                offer: Some("1.4.9"),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("action_required"),
                Some("antigravity_acp_signin_hold"),
                Some("1.4.5"),
                Some("1.4.5"),
                false
            )
        );
        assert_eq!(row["heldVersion"], "1.4.9");
        assert_eq!(
            (
                at(&row, &["diagnostics", "cause"]),
                at(&row, &["diagnostics", "reason"]),
            ),
            (Some("signin_needed"), Some("migration_unchecked"))
        );
        assert_eq!(t3.started(), 0);
        assert_eq!(active(root.path(), Platform::Ubuntu), before);
    }

    #[tokio::test]
    async fn a_refresh_that_asks_for_sign_in_reports_sign_in_needed_not_updated() {
        // AcpTests.test_a_refresh_that_asks_for_sign_in_reports_sign_in_needed_not_updated
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Ubuntu, "1.5.7", true, None);
        let t3 = T3::new(
            root.path(),
            Platform::Ubuntu,
            "1.5.7",
            Script {
                refresh: Some(SIGNIN),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("action_required"),
                Some("antigravity_acp_signin_needed"),
                Some("1.5.7"),
                Some("1.5.8"),
                true
            )
        );
        assert_eq!(at(&row, &["diagnostics", "cause"]), Some("signin_needed"));
        assert_eq!(at(&row, &["diagnostics", "reason"]), Some("refresh_signin"));
        assert!(!row.to_string().contains("Google"));
    }

    #[tokio::test]
    async fn a_refresh_that_fails_for_another_reason_keeps_updated_and_says_models_are_stale() {
        // AcpTests.test_a_refresh_that_fails_for_another_reason_keeps_updated_and_says_models_are_stale
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.7", true, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.7",
            Script {
                refresh: Some("The provider instance configuration is invalid."),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("updated"),
                Some("antigravity_acp_models_stale"),
                Some("1.5.7"),
                Some("1.5.8"),
                true
            )
        );
        assert_eq!(notes(&row), ["refresh_failed"]);
    }

    // --- a session with scripted answers, for the model refresh ----------------------------------

    struct Answers {
        queue: Mutex<std::collections::VecDeque<Result<Value, UpdateFailure>>>,
        asked: Mutex<Vec<(String, Value)>>,
    }

    impl Answers {
        fn new(answers: Vec<Result<Value, UpdateFailure>>) -> Self {
            Self {
                queue: Mutex::new(answers.into()),
                asked: Mutex::new(Vec::new()),
            }
        }
    }

    impl T3Session for Answers {
        fn call<'a>(
            &'a mut self,
            method: &'a str,
            payload: Value,
        ) -> BoxFuture<'a, Result<Value, UpdateFailure>> {
            Box::pin(async move {
                self.asked
                    .lock()
                    .expect("asked")
                    .push((method.to_owned(), payload));
                self.queue
                    .lock()
                    .expect("queue")
                    .pop_front()
                    .expect("an answer is scripted")
            })
        }

        fn states<'a>(
            &'a mut self,
            _instance: &'a str,
            _take: &'a mut (dyn FnMut(Value) -> Result<bool, UpdateFailure> + Send),
        ) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async { panic!("no subscription here") })
        }

        fn offer<'a>(
            &'a mut self,
            _instance: &'a str,
            _settle: Duration,
            _take: &'a mut (dyn FnMut(Value) -> Result<bool, UpdateFailure> + Send),
        ) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async { panic!("no offer here") })
        }

        fn close<'a>(self: Box<Self>) -> BoxFuture<'a, Result<(), UpdateFailure>> {
            Box::pin(async { Ok(()) })
        }
    }

    #[tokio::test]
    async fn a_refresh_continues_past_a_sign_in_and_ignores_instance_names() {
        // AlsoFixed.test_model_refresh_continues_past_a_sign_in_and_ignores_instance_names
        let refused = |detail: &str| {
            Err(UpdateFailure {
                output: format!("ProviderSetupError\n{detail}"),
                ..UpdateFailure::new("t3_install_failed:rpc")
            })
        };
        let instances = || vec!["first".to_owned(), "second".to_owned()];
        let mut session = Answers::new(vec![refused(SIGNIN), Ok(json!({"providers": []}))]);
        assert_eq!(
            refresh_models(&mut session, &instances()).await,
            ("signin", Some("refresh_signin"))
        );
        let asked = session.asked.into_inner().expect("asked");
        assert_eq!(
            asked
                .iter()
                .map(|(method, payload)| (
                    method.as_str(),
                    payload["instanceId"].as_str(),
                    payload["refreshModels"].clone()
                ))
                .collect::<Vec<_>>(),
            [
                ("server.refreshProviders", Some("first"), true.into()),
                ("server.refreshProviders", Some("second"), true.into()),
            ],
            "every instance is asked even after one needs a sign-in"
        );
        // Another instance's answer never decides, and a clean refresh is ok.
        let mut session = Answers::new(vec![
            Ok(json!({"providers": [
                {"instanceId": "second", "auth": {"status": "unauthenticated"}},
            ]})),
            Ok(json!({"providers": [
                {"instanceId": "first", "driver": "antigravity", "auth": {"status": "authenticated"}},
            ]})),
        ]);
        assert_eq!(
            refresh_models(&mut session, &instances()).await,
            ("ok", None)
        );
        // An instance that now reads unauthenticated means the stored sign-in cannot be used.
        let mut session = Answers::new(vec![
            Ok(json!({"providers": [
                {"instanceId": "first", "auth": {"status": "unauthenticated"}},
            ]})),
            Ok(json!({"providers": []})),
        ]);
        assert_eq!(
            refresh_models(&mut session, &instances()).await,
            ("signin", Some("unauthenticated"))
        );
        for (code, output, word) in [
            (
                "t3_install_failed:rpc",
                "The provider instance configuration is invalid.",
                "refresh_failed",
            ),
            ("t3_install_timeout", "", "refresh_failed"),
            (
                "t3_install_failed:rpc",
                "Operation timed out",
                "refresh_timed_out",
            ),
        ] {
            let mut session = Answers::new(vec![
                Err(UpdateFailure {
                    output: output.into(),
                    ..UpdateFailure::new(code)
                }),
                Ok(json!({"providers": []})),
            ]);
            assert_eq!(
                refresh_models(&mut session, &instances()).await,
                ("failed", Some(word)),
                "{code} {output}"
            );
        }
    }

    // --- ordering against T3 ----------------------------------------------------------------------

    #[tokio::test]
    async fn a_t3_restart_still_pending_never_uses_the_previous_servers_offer() {
        // AcpTests.test_a_t3_restart_still_pending_never_uses_the_previous_servers_offer
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Ubuntu, "1.5.7", true, None);
        let t3 = T3::new(root.path(), Platform::Ubuntu, "1.5.7", Script::default());
        let mut cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        cx.t3_row = Some(json!({
            "appId": "t3-code", "platform": "ubuntu", "status": "updated", "updateAttempted": true,
            "restartTargets": [{"kind": "systemd", "service": "t3code.service", "delaySeconds": 30}],
        }));
        let row = run(&cx, &t3).await;
        assert_eq!(
            (
                at(&row, &["status"]),
                at(&row, &["messageCode"]),
                at(&row, &["diagnostics", "cause"]),
            ),
            (
                Some("action_required"),
                Some("t3_restart_pending"),
                Some("t3_restart_pending")
            )
        );
        assert!(!outcome(&row).4);
        assert!(t3.windows().is_empty() && t3.calls().is_empty());
    }

    #[tokio::test]
    async fn a_replaced_desktop_server_is_awaited_by_its_new_version() {
        // AcpTests.test_a_replaced_desktop_server_is_awaited_by_its_new_version
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.7", true, None);
        let t3 = T3::new(root.path(), Platform::Windows, "1.5.7", Script::default());
        let mut cx = context(&root, Platform::Windows, Fixture::new(None), false);
        cx.t3_row = Some(json!({
            "appId": "t3-code", "platform": "windows", "status": "updated",
            "version": "0.1.10-nightly.20261113.4616", "updateAttempted": true,
            "restartTargets": [{"kind": "desktop"}],
        }));
        assert_eq!(run(&cx, &t3).await["status"], "updated");
        assert_eq!(
            t3.windows(),
            vec![(
                Some("0.1.10-nightly.20261113.4616".to_owned()),
                READY_SECONDS
            )]
        );
    }

    #[tokio::test]
    async fn a_server_that_is_absent_or_not_ready_is_action_required() {
        // Python: t3_unreachable, t3_auth_failed, t3_absent and t3_not_ready need the user.
        for (failure, code) in [
            (
                UpdateFailure::new("t3_absent")
                    .with_cause(FailureCause::T3Absent, Some("no_t3_install")),
                "t3_absent",
            ),
            (
                UpdateFailure::new("t3_not_ready")
                    .with_cause(FailureCause::T3NotReady, Some("previous_server")),
                "t3_not_ready",
            ),
        ] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), Platform::Mac, "1.5.7", true, None);
            let t3 = T3::new(
                root.path(),
                Platform::Mac,
                "1.5.7",
                Script {
                    ready: Some(failure.clone()),
                    ..Script::default()
                },
            );
            let cx = context(&root, Platform::Mac, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            assert_eq!(
                (
                    at(&row, &["status"]),
                    at(&row, &["messageCode"]),
                    at(&row, &["diagnostics", "cause"]),
                    at(&row, &["diagnostics", "reason"])
                ),
                (
                    Some("action_required"),
                    Some(code),
                    Some(failure.cause.map(FailureCause::as_str).unwrap_or_default()),
                    failure.reason
                ),
                "{code}"
            );
            assert!(t3.calls().is_empty());
        }
    }

    // --- the follow-up after a Linux computer's deferred T3 restart -------------------------------

    #[tokio::test]
    async fn a_follow_up_reports_absent_failed_and_cancelled_runs_without_touching_t3() {
        let root = tempfile::tempdir().expect("root");
        let t3 = T3::new(root.path(), Platform::Ubuntu, "", Script::default());
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        assert_eq!(follow(&cx, None, &t3).await["status"], "not_installed");
        pointer(root.path(), Platform::Ubuntu, "../outside");
        let row = follow(&cx, None, &t3).await;
        assert_eq!(
            (
                at(&row, &["status"]),
                at(&row, &["messageCode"]),
                at(&row, &["diagnostics", "reason"]),
            ),
            (
                Some("unknown"),
                Some("readiness_unknown"),
                Some("runtime_record_invalid")
            )
        );
        runtime(root.path(), Platform::Ubuntu, "1.5.7", true, None);
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), true);
        let row = follow(&cx, None, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("skipped"),
                Some("skipped_cancelled"),
                None,
                None,
                false
            )
        );
        assert!(t3.windows().is_empty() && t3.calls().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_follow_up_whose_server_never_answers_says_so() {
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Ubuntu, "1.5.7", true, None);
        let absent = T3::new(
            root.path(),
            Platform::Ubuntu,
            "1.5.7",
            Script {
                ready: Some(
                    UpdateFailure::new("t3_absent")
                        .with_cause(FailureCause::T3Absent, Some("no_t3_install")),
                ),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let row = follow(&cx, None, &absent).await;
        assert_eq!(
            outcome(&row),
            (
                Some("action_required"),
                Some("t3_absent"),
                Some("1.5.7"),
                Some("1.5.7"),
                false
            )
        );
        assert_eq!(
            (
                at(&row, &["diagnostics", "cause"]),
                at(&row, &["diagnostics", "reason"]),
            ),
            (Some("t3_absent"), Some("no_t3_install"))
        );
        assert!(absent.calls().is_empty());

        // Not ready until the window closes: the row says so instead of waiting forever.
        let row = follow(&cx, Some("0.1.12-nightly.20261120.4994"), &Never).await;
        assert_eq!(
            outcome(&row),
            (
                Some("action_required"),
                Some("t3_not_ready"),
                Some("1.5.7"),
                Some("1.5.7"),
                false
            )
        );
        assert_eq!(
            (
                at(&row, &["diagnostics", "cause"]),
                at(&row, &["diagnostics", "reason"]),
            ),
            (Some("t3_not_ready"), Some("restart_not_seen"))
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_follow_up_waits_for_the_helper_lock_then_runs_the_same_guarded_install() {
        // AcpTests.test_the_follow_up_after_a_linux_restart_waits_for_the_new_server_by_version
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Ubuntu, "1.5.12", true, None);
        let script = Script {
            offer: Some("1.5.13"),
            installed: "1.5.13",
            ..Script::default()
        };
        let t3 = T3::new(root.path(), Platform::Ubuntu, "1.5.12", script.clone());
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let held = shared::execution_lock(&cx.state, "helper-update.lock").expect("held");
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            drop(held);
        });
        let expected = Some("0.1.12-nightly.20261120.4994");
        let row = follow(&cx, expected, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("updated"),
                Some("antigravity_acp_updated"),
                Some("1.5.12"),
                Some("1.5.13"),
                true
            )
        );
        assert_eq!(
            t3.windows(),
            vec![
                (expected.map(str::to_owned), FOLLOW_UP_POLL),
                (expected.map(str::to_owned), READY_SECONDS)
            ]
        );
        assert_eq!(t3.started(), 1);
        assert_eq!(
            at(&row, &["diagnostics", "owner", "target"]),
            Some("t3-offer")
        );
        // Python's export keeps the first eight phases of the whole follow-up.
        let phases: Vec<&String> = row["diagnostics"]["phases"]
            .as_object()
            .expect("phases")
            .keys()
            .collect();
        assert_eq!(
            phases,
            [
                "checking",
                "t3_restart",
                "updating",
                "t3_ready",
                "discover",
                "offer",
                "install",
                "verify",
            ]
        );

        // A lock nobody frees ends as busy once the window closes, and nothing starts.
        let _held = shared::execution_lock(&cx.state, "helper-update.lock").expect("held again");
        let t3 = T3::new(root.path(), Platform::Ubuntu, "1.5.13", script);
        let row = follow(&cx, None, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("action_required"),
                Some("busy"),
                Some("1.5.13"),
                Some("1.5.13"),
                false
            )
        );
        assert_eq!(t3.windows(), vec![(None, FOLLOW_UP_POLL)]);
        assert!(t3.calls().is_empty());
    }

    // --- session and operation safety -------------------------------------------------------------

    #[tokio::test]
    async fn only_the_operation_this_run_started_decides_the_row() {
        // AcpTests.test_reconnected_subscription_checks_original_operation_and_never_restarts
        for (operation, code) in [
            ("op", "antigravity_acp_updated"),
            ("different", "t3_install_state_unknown"),
        ] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), Platform::Windows, "1.3.0", false, None);
            let t3 = T3::new(
                root.path(),
                Platform::Windows,
                "1.3.0",
                Script {
                    operation,
                    ..Script::default()
                },
            );
            let cx = context(&root, Platform::Windows, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            assert_eq!(at(&row, &["messageCode"]), Some(code), "{operation}");
            if operation == "different" {
                assert_eq!(
                    at(&row, &["diagnostics", "reason"]),
                    Some("operation_changed")
                );
            }
            // Never start twice, whatever a reconnected subscription shows.
            assert_eq!(t3.started(), 1);
        }
    }

    #[tokio::test]
    async fn failures_never_write_the_pointer_or_leak_the_token() {
        // AcpTests.test_failures_never_write_pointer_or_leak_token_and_always_revoke
        for (failure, phase, code) in [
            (Some("ambiguous"), "succeeded", "t3_install_state_unknown"),
            (None, "failed", "t3_install_failed:installer"),
            (None, "cancelled", "t3_install_failed:cancelled"),
            (Some("timeout"), "succeeded", "t3_install_timeout"),
            (Some("unreachable"), "succeeded", "t3_unreachable"),
            (Some("denied"), "succeeded", "t3_auth_failed"),
        ] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), Platform::Windows, "1.3.0", false, None);
            let before = active(root.path(), Platform::Windows);
            let t3 = T3::new(
                root.path(),
                Platform::Windows,
                "1.3.0",
                Script {
                    phase,
                    failure,
                    ..Script::default()
                },
            );
            let cx = context(&root, Platform::Windows, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            let status = if matches!(failure, Some("unreachable" | "denied")) {
                "action_required"
            } else {
                "failed"
            };
            assert_eq!(
                (at(&row, &["status"]), at(&row, &["messageCode"])),
                (Some(status), Some(code)),
                "{failure:?} {phase}"
            );
            assert_eq!(row["version"], "1.3.0", "{failure:?} {phase}");
            assert_eq!(active(root.path(), Platform::Windows), before);
            assert!(!row.to_string().contains(TOKEN));
        }
    }

    #[tokio::test]
    async fn a_failed_revoke_is_reported_in_the_row_without_the_token() {
        // AcpTests.test_failed_revoke_is_reported_in_the_row_without_token
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.3.0", false, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.3.0",
            Script {
                revoke_fails: true,
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        // The install itself succeeded: the runtime on disk is the new one.
        assert_eq!(
            outcome(&row),
            (
                Some("failed"),
                Some("t3_session_revoke_failed"),
                Some("1.3.0"),
                Some("1.5.8"),
                true
            )
        );
        assert!(!row.to_string().contains(TOKEN));
    }

    #[tokio::test]
    async fn an_issue_failure_without_a_listed_session_is_issue_unknown_and_never_installs() {
        // AcpTests.test_an_issue_failure_without_a_listed_session_is_issue_unknown_and_never_installs
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.3.0", false, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.3.0",
            Script {
                failure: Some("auth"),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        // The CLI failed (it did not answer with an empty reply): not a lost reply.
        assert_eq!(
            (
                at(&row, &["status"]),
                at(&row, &["messageCode"]),
                at(&row, &["diagnostics", "cause"]),
            ),
            (
                Some("failed"),
                Some("t3_session_issue_unknown"),
                Some("unknown")
            )
        );
        assert_eq!(t3.started(), 0);
    }

    #[tokio::test]
    async fn false_success_is_rejected() {
        // AcpTests.test_false_success_is_rejected
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.3.0", false, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.3.0",
            Script {
                offer: Some("1.5.8"),
                installed: "1.4.5",
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            (at(&row, &["status"]), at(&row, &["messageCode"])),
            (Some("failed"), Some("t3_install_failed:verification"))
        );
        assert_eq!(row["version"], "1.4.5");
        assert!(!row.to_string().contains(TOKEN));
    }

    #[tokio::test]
    async fn a_failed_verification_still_asks_t3_to_refresh_every_enabled_instance() {
        // AcpTests.test_a_failed_verification_still_asks_t3_to_refresh_every_enabled_instance
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.12", true, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.12",
            Script {
                offer: Some("1.5.13"),
                installed: "1.5.12",
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            (at(&row, &["status"]), at(&row, &["messageCode"])),
            (Some("failed"), Some("t3_install_failed:verification"))
        );
        assert_eq!(
            t3.asked("server.refreshProviders"),
            ["agy-custom-id", "agy-second-id"]
        );
    }

    #[tokio::test]
    async fn a_recheck_that_installed_nothing_new_is_current() {
        // Python: T3 re-reads the release at start, and the operation then only re-checked the
        // installed runtime (a withdrawn release, its own manifest read failed, a rollback).
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.12", true, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.12",
            Script {
                offer: Some("1.5.13"),
                installed: "1.5.12",
                start_version: Some("1.5.12"),
                ..Script::default()
            },
        );
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            outcome(&row),
            (
                Some("current"),
                Some("current"),
                Some("1.5.12"),
                Some("1.5.12"),
                true
            )
        );
        assert_eq!(notes(&row), ["reverified"]);
        assert_eq!(t3.asked("server.refreshProviders"), Vec::<String>::new());
    }

    #[tokio::test]
    async fn verification_rereads_a_release_t3_publishes_just_after_it_reports_success() {
        // AcpTests.test_verification_rereads_a_release_t3_publishes_just_after_it_reports_success
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.12", true, None);
        let t3 = T3::new(
            root.path(),
            Platform::Windows,
            "1.5.12",
            Script {
                offer: Some("1.5.13"),
                installed: "1.5.13",
                activate_late: true,
                ..Script::default()
            },
        );
        let late = t3.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(100)).await;
            late.activate();
        });
        let cx = context(&root, Platform::Windows, Fixture::new(None), false);
        let row = run(&cx, &t3).await;
        assert_eq!(
            (at(&row, &["status"]), at(&row, &["version"])),
            (Some("updated"), Some("1.5.13"))
        );
    }

    #[tokio::test]
    async fn an_install_t3_0112_writes_verifies_as_updated() {
        // AcpTests.test_an_install_t3_0112_writes_verifies_as_updated
        for platform in [Platform::Mac, Platform::Windows] {
            let root = tempfile::tempdir().expect("root");
            runtime(root.path(), platform, "1.5.12", true, None);
            let t3 = T3::new(
                root.path(),
                platform,
                "1.5.12",
                Script {
                    offer: Some("1.5.13"),
                    installed: "1.5.13",
                    activate_shape: Some("0.1.12"),
                    ..Script::default()
                },
            );
            let cx = context(&root, platform, Fixture::new(None), false);
            let row = run(&cx, &t3).await;
            assert_eq!(
                outcome(&row),
                (
                    Some("updated"),
                    Some("antigravity_acp_updated"),
                    Some("1.5.12"),
                    Some("1.5.13"),
                    true
                ),
                "{platform:?}"
            );
            assert_eq!(t3.started(), 1);
            assert_eq!(
                at(&row, &["diagnostics", "owner", "runtime"]),
                Some("native")
            );
        }
    }

    #[tokio::test]
    async fn the_channel_is_read_only_when_it_can_decide_the_target() {
        // Python: Google's channel is read only when T3 gave no usable offer, or when this
        // runtime pair could be replaced by the native CLI.
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Windows, "1.5.7", true, None);
        let t3 = T3::new(root.path(), Platform::Windows, "1.5.7", Script::default());
        let system = Fixture::new(Some(json!({"version": "9.9.9"})));
        let cx = context(&root, Platform::Windows, system.clone(), false);
        let row = run(&cx, &t3).await;
        assert_eq!(at(&row, &["version"]), Some("1.5.8"));
        assert!(
            system.fetched.lock().expect("fetched").is_empty(),
            "T3's offer decides: the channel is not read"
        );
    }

    #[tokio::test]
    async fn without_a_t3_client_the_row_says_so_and_nothing_is_installed() {
        // The binding lane W5's `t3.rs` backs: until it lands the local client is not
        // there, and the row says so instead of guessing.
        let root = tempfile::tempdir().expect("root");
        runtime(root.path(), Platform::Ubuntu, "1.5.7", true, None);
        let before = active(root.path(), Platform::Ubuntu);
        let cx = context(&root, Platform::Ubuntu, Fixture::new(None), false);
        let row = attempt(&ACP, &cx).await.to_value();
        assert_eq!(
            (
                at(&row, &["status"]),
                at(&row, &["messageCode"]),
                at(&row, &["diagnostics", "cause"]),
            ),
            (
                Some("action_required"),
                Some("t3_not_ready"),
                Some("t3_not_ready")
            )
        );
        assert_eq!(active(root.path(), Platform::Ubuntu), before);
    }
}
