//! T3 nightly desktop, bundled server and standalone runtime updates (Python
//! `app_update_t3.py`), and the local-only T3 install client that every app
//! talking to T3's server uses (Python `app_update_t3_rpc.py`). W5 owns this file.
//!
//! Nothing here restarts the server that hosts these agents. On Ubuntu the
//! runtime is installed and a detached worker (`aac-agent --t3-restart`)
//! replaces the service once the update job is over. A session issued from T3
//! lives in memory only and is revoked on every exit path.
#![allow(unsafe_code)]

mod rpc;
#[cfg(test)]
mod tests;

// Frozen cross-lane exports consumed by W4 (`antigravity.rs`) and W6 (`runner.rs`/`update.rs`).
#[allow(unused_imports)]
pub(super) use rpc::{
    Endpoint, ISSUER_FLAG, Origin, READY_SECONDS, Session, Subscription, discover, session_signals,
    wait_ready,
};

use super::{
    AppRow, AppUpdater, BoxFuture, CommandSpec, Context, Detection, FailureCause, Installed,
    Platform, Probe, Shared, Status, UpdateFailure,
    common::{self, Native},
    processes::{self, Process},
    shared,
};
use serde_json::{Map, Value, json};
use sha2::Digest as _;
use std::{
    collections::HashSet,
    ffi::OsString,
    future::Future,
    path::{Path, PathBuf},
    sync::{Arc, LazyLock},
    time::Duration,
};
use tokio::{io::AsyncReadExt as _, time::Instant};

const RELEASES: &str = "https://api.github.com/repos/pingdotgg/t3code/releases?per_page=20";
const RELEASE_BASE: &str = "https://github.com/pingdotgg/t3code/releases/download/";
const MAC_ID: &str = "com.t3tools.t3code";
const MAC_TEAM: &str = "ARK85ZXQ4Z";
const WINDOWS_NAME: &str = "T3 Code (Nightly).exe";
const MAC_NAME: &str = "T3 Code (Nightly).app";
const WINDOWS_PUBLISHER: &str = "T3 Tools Inc";
const MAX_PACKAGE: u64 = 800 * 1024 * 1024;
const PRUNE_LIMIT: usize = 20;
const COMPONENT_KEYS: [&str; 5] = [
    "utc",
    "cursorAgent",
    "acpUpdater",
    "prunedRuntimes",
    "daemonReload",
];
/// A verified install gets this long to come back: a restarted server migrates its database
/// before it listens (a large install's 35 GB state took about 11 minutes for one added index).
const UBUNTU_READY_SECONDS: u64 = 45 * 60;
const SERVICE: &str = "t3code.service";
const HEALTH_URL: &str = "http://127.0.0.1:3773/";
const PENDING_MARKER: &str = "t3-code-pending-restart.json";
const COMPONENTS_RECORD: &str = "t3-components.json";

/// `aac-agent --t3-restart ...`: the detached T3 restart worker (Python `app_update_t3.main`).
pub(super) const RESTART_FLAG: &str = "--t3-restart";

static VERSION: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\A[0-9]+\.[0-9]+\.[0-9]+-nightly\.[0-9]{8}\.[0-9]+\z")
        .expect("T3 release pattern")
});
static AGENT_JOB: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\A[0-9a-f]{32}\z").expect("agent job id"));

/// Python `app_update_t3.exact_version`: a full nightly version (`X.Y.Z-nightly.YYYYMMDD.N`,
/// at most 64 bytes), else `None`.
pub(super) fn exact_version(value: &str) -> Option<&str> {
    (value.len() <= 64 && VERSION.is_match(value)).then_some(value)
}

fn version_of(value: &Value) -> Option<&str> {
    value.as_str().and_then(exact_version)
}

/// Python `version_key`: every decimal run in order.
fn version_key(value: &str) -> Vec<u64> {
    value
        .split(|ch: char| !ch.is_ascii_digit())
        .filter_map(|part| part.parse().ok())
        .collect()
}

fn newer(left: &str, right: &str) -> bool {
    version_key(left) > version_key(right)
}

/// Python `max(values, key)`: first of the equal largest.
fn first_max<T>(items: &[T], key: impl Fn(&T) -> Vec<u64>) -> Option<&T> {
    items
        .iter()
        .reduce(|best, item| if key(item) > key(best) { item } else { best })
}

/// Python's truth: empty, zero, `false` and `null` are false.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

/// How a step failed: a named [`UpdateFailure`], or a fault (any other exception in
/// Python, which `update_t3` reports as `failed` / `update_failed` without a cause).
#[derive(Debug)]
struct Fault(UpdateFailure, bool);

impl Fault {
    fn generic() -> Self {
        Self(UpdateFailure::failed(), false)
    }

    fn is_named(&self) -> bool {
        self.1
    }
}

impl From<UpdateFailure> for Fault {
    fn from(failure: UpdateFailure) -> Self {
        Self(failure, true)
    }
}

impl From<std::io::Error> for Fault {
    fn from(_: std::io::Error) -> Self {
        // Error text may name private paths: like Python, nothing of it is kept.
        Self::generic()
    }
}

/// Run blocking file work off the request's thread; desktop bundle trees are large.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> std::io::Result<T> + Send + 'static,
) -> Result<T, Fault> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| Fault::generic())?
        .map_err(Fault::from)
}

/// `shutil.copytree` with its defaults: a folder and everything below it, links followed.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir(to)?;
    for entry in std::fs::read_dir(from)?.flatten() {
        let target = to.join(entry.file_name());
        if entry.metadata().is_ok_and(|info| info.is_dir()) {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// `shutil.move` of a folder: rename, or copy+delete across filesystems.
fn move_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::rename(from, to) {
        Ok(()) => Ok(()),
        Err(error) if is_cross_device(&error) => {
            copy_tree(from, to)?;
            std::fs::remove_dir_all(from)
        }
        Err(error) => Err(error),
    }
}

fn is_cross_device(error: &std::io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        // ERROR_NOT_SAME_DEVICE
        error.raw_os_error() == Some(17)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = error;
        false
    }
}

/// `os.kill(pid, 0)`: `Some(alive)`, or `None` on an unexpected OS error.
fn pid_alive(pid: u32) -> Option<bool> {
    #[cfg(unix)]
    {
        let pid = i32::try_from(pid).ok()?;
        // SAFETY: signal 0 only probes process existence; no signal is delivered.
        if unsafe { libc::kill(pid, 0) } == 0 {
            return Some(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Some(false),
            Some(libc::EPERM) => Some(true),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
        None
    }
}

/// Python `_pid_alive`: unexpected errors count as not alive.
fn pid_exists(pid: u32) -> bool {
    pid_alive(pid).unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Detection (Python `detect_t3`, `runtime_install`, `windows_bundle_version`,
// `mac_bundle_version`).
// ---------------------------------------------------------------------------

/// Python `runtime_install`: the standalone runtime binary an update would act on.
fn runtime_install(home: &Path) -> Option<PathBuf> {
    let versions = home.join(".t3/runtime/versions");
    let launcher = home.join(".local/bin/t3");
    if launcher.is_file()
        && crate::apps::real(&launcher).parent().and_then(Path::parent)
            == Some(crate::apps::real(&versions).as_path())
    {
        return Some(launcher);
    }
    let candidates: Vec<PathBuf> = std::fs::read_dir(&versions)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(exact_version)
                .is_some()
                && path.join("t3").is_file()
        })
        .map(|path| path.join("t3"))
        .collect();
    first_max(&candidates, |path| {
        version_key(
            path.parent()
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or_default(),
        )
    })
    .cloned()
}

/// Python `windows_bundle_version`: the nightly the ASAR's root `package.json` names (a PE
/// ProductVersion loses the nightly suffix). Only the small root file is read; nothing executes.
fn windows_bundle_version(executable: &Path) -> Option<String> {
    let package = shared::asar_package(
        &executable.parent()?.join("resources/app.asar"),
        2 * 1024 * 1024,
    )?;
    (package.get("name").and_then(Value::as_str) == Some("t3code"))
        .then(|| {
            package
                .get("version")
                .and_then(version_of)
                .map(str::to_owned)
        })
        .flatten()
}

/// Decode standard XML character entities inside a plist string/key element.
fn xml_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(pos) = rest.find('&') {
        out.push_str(&rest[..pos]);
        rest = &rest[pos + 1..];
        let Some(semi) = rest.find(';') else {
            out.push('&');
            break;
        };
        let entity = &rest[..semi];
        rest = &rest[semi + 1..];
        match entity {
            "amp" => out.push('&'),
            "lt" => out.push('<'),
            "gt" => out.push('>'),
            "quot" => out.push('"'),
            "apos" => out.push('\''),
            code if code.starts_with("#x") || code.starts_with("#X") => {
                if let Some(ch) = u32::from_str_radix(&code[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
                {
                    out.push(ch);
                }
            }
            code if code.starts_with('#') => {
                if let Some(ch) = code[1..].parse::<u32>().ok().and_then(char::from_u32) {
                    out.push(ch);
                }
            }
            other => {
                out.push('&');
                out.push_str(other);
                out.push(';');
            }
        }
    }
    out.push_str(rest);
    out
}

/// Extract top-level `<key>...</key><string>...</string>` pairs from an XML property list.
/// Nested `<dict>` and `<array>` values are skipped so only root keys match.
fn parse_xml_plist_strings(text: &str) -> Option<std::collections::HashMap<String, String>> {
    let mut map = std::collections::HashMap::new();
    let start = text.find("<dict")?;
    let mut pos = text[start..].find('>')? + start + 1;
    let bytes = text.as_bytes();
    let mut depth: usize = 1;
    let mut pending_key: Option<String> = None;

    while pos < bytes.len() && depth > 0 {
        let Some(lt_rel) = text[pos..].find('<') else {
            break;
        };
        pos += lt_rel;
        if text[pos..].starts_with("<!--") {
            let end = text[pos + 4..].find("-->")?;
            pos += 4 + end + 3;
            continue;
        }
        let gt_rel = text[pos..].find('>')?;
        let tag = text[pos + 1..pos + gt_rel].trim();
        let next = pos + gt_rel + 1;
        let self_closing = tag.ends_with('/');
        let closing = tag.starts_with('/');
        let name = tag
            .trim_start_matches('/')
            .trim_end_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or_default();

        if closing {
            if matches!(name, "dict" | "array") {
                depth = depth.saturating_sub(1);
            }
            pos = next;
            continue;
        }
        if matches!(name, "dict" | "array") {
            if depth == 1 {
                pending_key = None;
            }
            if !self_closing {
                depth += 1;
            }
            pos = next;
            continue;
        }
        if depth == 1 {
            if name == "key" && !self_closing {
                let end = text[next..].find("</key>")?;
                pending_key = Some(xml_unescape(&text[next..next + end]));
                pos = next + end + "</key>".len();
                continue;
            }
            if let Some(key) = pending_key.take() {
                if name == "string" {
                    if self_closing {
                        map.insert(key, String::new());
                        pos = next;
                        continue;
                    }
                    let end = text[next..].find("</string>")?;
                    map.insert(key, xml_unescape(&text[next..next + end]));
                    pos = next + end + "</string>".len();
                    continue;
                }
                if !self_closing && let Some(end_tag) = text[next..].find(&format!("</{name}>")) {
                    pos = next + end_tag + name.len() + 3;
                    continue;
                }
            }
        }
        pos = next;
    }
    Some(map)
}

/// Python `mac_bundle_version`: the nightly `Contents/Info.plist` names for this bundle.
/// Parses XML plists in-process across all OSes, falling back to `plutil` only for binary plists.
async fn mac_bundle_version(cx: &Context, bundle: &Path) -> Option<String> {
    let plist = bundle.join("Contents/Info.plist");
    let raw = std::fs::read(&plist).ok()?;
    if raw.len() > 512 * 1024 {
        return None;
    }
    if !raw.starts_with(b"bplist00") {
        let text = std::str::from_utf8(&raw).ok()?;
        let entries = parse_xml_plist_strings(text)?;
        return (entries.get("CFBundleIdentifier").map(String::as_str) == Some(MAC_ID))
            .then(|| {
                entries
                    .get("CFBundleShortVersionString")
                    .and_then(|value| exact_version(value))
                    .map(str::to_owned)
            })
            .flatten();
    }
    let spec = CommandSpec::new([
        cx.host.system("/usr/bin/plutil").into_os_string(),
        "-convert".into(),
        "json".into(),
        "-o".into(),
        "-".into(),
        "--".into(),
        plist.into_os_string(),
    ])
    .timeout(Duration::from_secs(20))
    .capture()
    .quiet();
    let info: Value = serde_json::from_str(&cx.run(&spec).await.ok()?.stdout).ok()?;
    (info["CFBundleIdentifier"] == MAC_ID)
        .then(|| version_of(&info["CFBundleShortVersionString"]).map(str::to_owned))
        .flatten()
}

fn probe_version(probe: &shared::Probed) -> Option<String> {
    match probe {
        shared::Probed::Version(text) => text.as_deref().and_then(exact_version).map(str::to_owned),
        _ => None,
    }
}

/// Python `cli_probe`'s second result as the contract's probe: a timeout is `Probe::Timeout`;
/// another failure leaves the version unknown (`Probe::Ok` with `version = None`).
fn probe_state(probe: &shared::Probed) -> Probe {
    match probe {
        shared::Probed::Timeout => Probe::Timeout,
        _ => Probe::Ok,
    }
}

/// Python `detect_t3`: read-only; never installs, stops or writes anything.
async fn detect_t3(cx: &Context) -> Option<Installed> {
    let runtime = if cx.platform == Platform::Windows {
        None
    } else {
        runtime_install(&cx.host.home)
    };
    let probed = match &runtime {
        Some(runtime) => Some((
            shared::cli_probe(cx, runtime, &["--version"]).await,
            runtime.clone(),
        )),
        None => None,
    };
    let runtime_version = probed.as_ref().and_then(|(probe, _)| probe_version(probe));
    let probe = probed
        .as_ref()
        .map(|(probe, _)| probe_state(probe))
        .unwrap_or(Probe::Ok);
    let desktop = match cx.platform {
        Platform::Mac => {
            let bundle = cx.host.system(processes::T3_MAC_BUNDLE);
            if !bundle.is_dir() {
                None
            } else {
                let mut install = Installed::new(
                    "t3-code",
                    Platform::Mac,
                    bundle.clone(),
                    None,
                    "official-download",
                );
                install.identity = Some(MAC_ID.into());
                install.publisher = Some(MAC_TEAM.into());
                install.package_root = Some(bundle.clone());
                Some((install, bundle))
            }
        }
        Platform::Windows => {
            let executable = cx.host.local.join("Programs/t3code").join(WINDOWS_NAME);
            if !executable.is_file() {
                None
            } else {
                let mut install = Installed::new(
                    "t3-code",
                    Platform::Windows,
                    executable.clone(),
                    None,
                    "official-download",
                );
                install.publisher = Some(WINDOWS_PUBLISHER.into());
                install.package_root = executable.parent().map(Path::to_path_buf);
                Some((install, executable))
            }
        }
        Platform::Ubuntu => None,
    };
    if let Some((mut install, path)) = desktop {
        install.version = if cx.platform == Platform::Mac {
            mac_bundle_version(cx, &path).await
        } else {
            windows_bundle_version(&path)
        };
        install.probe = probe;
        return Some(install);
    }
    let (_, runtime) = probed?;
    let mut install = Installed::new("t3-code", cx.platform, runtime, runtime_version, "native");
    install.probe = probe;
    Some(install)
}

// ---------------------------------------------------------------------------
// Release lookup and verified package download.
// ---------------------------------------------------------------------------

/// Python `latest_release`: the newest full nightly of the first release page that has one.
async fn latest_release(cx: &Context) -> Result<String, UpdateFailure> {
    let deadline = Instant::now() + Duration::from_secs(30);
    for page in 1..=5 {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(UpdateFailure::new("timeout"));
        }
        let url = if page == 1 {
            RELEASES.to_owned()
        } else {
            format!("{RELEASES}&page={page}")
        };
        let raw = cx
            .system
            .fetch(
                &url,
                &url,
                2 * 1024 * 1024,
                left.min(Duration::from_secs(20)),
            )
            .await?;
        if Instant::now() >= deadline {
            return Err(UpdateFailure::new("timeout"));
        }
        let value: Value = serde_json::from_slice(&raw).map_err(|_| UpdateFailure::failed())?;
        let (rows, empty) = match &value {
            Value::Array(rows) => (rows.iter().collect::<Vec<_>>(), rows.is_empty()),
            Value::Object(fields) => (Vec::new(), fields.is_empty()),
            _ => return Err(UpdateFailure::failed()),
        };
        let mut versions = Vec::new();
        for row in rows {
            let Some(row) = row.as_object() else {
                continue;
            };
            if truthy(row.get("draft").unwrap_or(&Value::Null)) {
                continue;
            }
            if let Some(version) = row
                .get("tag_name")
                .and_then(Value::as_str)
                .and_then(|tag| tag.strip_prefix('v'))
                && exact_version(version).is_some()
            {
                versions.push(version);
            }
        }
        if let Some(newest) = first_max(&versions, |version| version_key(version)) {
            return Ok((*newest).to_owned());
        }
        if empty {
            break;
        }
    }
    Err(UpdateFailure::failed())
}

/// Python `verified_download`: the release's manifest pins this exact asset's SHA-512.
async fn verified_download(
    cx: &Context,
    version: &str,
    temporary: &Path,
) -> Result<PathBuf, Fault> {
    let (asset, manifest) = match cx.platform {
        Platform::Mac if matches!(cx.host.arch, "arm64" | "aarch64") => {
            (format!("T3-Code-{version}-arm64.zip"), "nightly-mac.yml")
        }
        Platform::Windows if matches!(cx.host.arch, "amd64" | "x86_64") => {
            (format!("T3-Code-{version}-x64.exe"), "nightly.yml")
        }
        _ => return Err(UpdateFailure::new("unsupported").into()),
    };
    let base = format!("{RELEASE_BASE}v{version}/");
    let (metadata, package) = (temporary.join(manifest), temporary.join(&asset));
    cx.phase("downloading");
    cx.system
        .download(
            &format!("{base}{manifest}"),
            &metadata,
            65_536,
            Duration::from_secs(30),
        )
        .await?;
    let text = tokio::fs::read_to_string(&metadata).await?;
    let (digest, _) = shared::manifest_asset(&text, version, &asset)?;
    cx.system
        .download(
            &format!("{base}{asset}"),
            &package,
            MAX_PACKAGE,
            Duration::from_secs(shared::budget(cx, 600)?),
        )
        .await?;
    let mut file = tokio::fs::File::open(&package).await?;
    let mut hasher = sha2::Sha512::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    if hasher.finalize().as_slice() != digest.as_slice() {
        return Err(UpdateFailure::new("signature_failed").into());
    }
    cx.phase("updating");
    Ok(package)
}

// ---------------------------------------------------------------------------
// Runtime update and Mac/Windows desktop swaps.
// ---------------------------------------------------------------------------

/// Python `update_runtime`: install beside the running version without `--yes` so the
/// server hosting these agents is never restarted synchronously.
async fn update_runtime(cx: &Context, runtime: &Path, version: &str) -> Result<(), Fault> {
    let timeout = Duration::from_secs(shared::budget(cx, 300)?);
    if cx.platform == Platform::Ubuntu {
        common::write_private_json(&cx.state.join(PENDING_MARKER), &json!({"version": version}))?;
    }
    let mut argv = vec![runtime.as_os_str().to_owned()];
    argv.extend(["update", version, "--channel", "nightly"].map(OsString::from));
    cx.run(&CommandSpec::new(argv).timeout(timeout)).await?;
    let replacement = cx
        .host
        .home
        .join(".t3/runtime/versions")
        .join(version)
        .join("t3");
    let actual = match shared::cli_probe(cx, &replacement, &["--version"]).await {
        shared::Probed::Version(text) => text,
        _ => None,
    };
    if actual.as_deref() != Some(version) {
        return Err(UpdateFailure::new("version_unknown").into());
    }
    Ok(())
}

/// Python `verify_mac`: codesign deep/strict, designated requirement, Gatekeeper, and bundle version.
async fn verify_mac(cx: &Context, bundle: &Path, version: &str) -> Result<(), Fault> {
    shared::verify_mac_signature(cx, bundle, MAC_ID, MAC_TEAM)
        .await
        .map_err(|_| UpdateFailure::new("signature_failed"))?;
    if mac_bundle_version(cx, bundle).await.as_deref() != Some(version) {
        return Err(UpdateFailure::new("signature_failed").into());
    }
    Ok(())
}

/// Python `verify_windows`: Authenticode Valid and signed by T3 Tools Inc.
async fn verify_windows(cx: &Context, package: &Path) -> Result<(), Fault> {
    let script = format!(
        "$ErrorActionPreference='Stop'; $s=Get-AuthenticodeSignature -LiteralPath {}; if($s.Status -ne 'Valid' -or -not $s.SignerCertificate -or $s.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false) -ne '{WINDOWS_PUBLISHER}'){{exit 1}}",
        shared::ps_quote(&package.to_string_lossy())
    );
    shared::powershell(cx, &script, Duration::from_secs(60), None, false)
        .await
        .map_err(|_| UpdateFailure::new("signature_failed"))?;
    Ok(())
}

/// One loopback HTTP GET answered 200 (Python `urlopen`). Never uses a proxy.
async fn http_ok(url: &str, timeout: Duration) -> bool {
    let Ok(client) = reqwest::Client::builder().no_proxy().build() else {
        return false;
    };
    match client.get(url).timeout(timeout).send().await {
        Ok(response) => response.status() == reqwest::StatusCode::OK,
        Err(_) => false,
    }
}

/// Python `service_state`: `(ActiveState, NRestarts)` of one user unit, or `None`.
async fn service_state(cx: &Context, service: &str) -> Option<(String, Option<u64>)> {
    let spec = CommandSpec::new([
        cx.host.system("/usr/bin/systemctl").into_os_string(),
        "--user".into(),
        "show".into(),
        "--property=ActiveState,NRestarts".into(),
        service.into(),
    ])
    .timeout(Duration::from_secs(10))
    .capture();
    let text = cx.run(&spec).await.ok()?.stdout;
    let mut active = String::new();
    let mut restarts = None;
    for line in text.lines() {
        if let Some((key, value)) = line.split_once('=') {
            match key {
                "ActiveState" => active = value.to_owned(),
                "NRestarts"
                    if !value.is_empty() && value.bytes().all(|byte| byte.is_ascii_digit()) =>
                {
                    restarts = value.parse().ok();
                }
                _ => {}
            }
        }
    }
    Some((active, restarts))
}

/// Python `health_budget`: verified installs get a real migration window floored at 30s, capped at 600s.
fn health_budget(cx: &Context) -> u64 {
    cx.remaining().as_secs().clamp(30, 600)
}

/// Python `desktop_family_alive`: whether any family process still runs; unknown counts as alive.
async fn desktop_family_alive(cx: &Context, install: &Installed) -> bool {
    match processes::scan(cx).await {
        Ok(table) => !processes::family(&cx.host, install, &table, true).is_empty(),
        Err(_) => true,
    }
}

/// Core of Python `health_check`: injectable clock/sleep so unit tests exercise 11-minute
/// migrations and 30-second bounds without wall-clock delays.
#[expect(clippy::too_many_arguments, reason = "testable clock/probe seam")]
async fn health_check_core<A, AF, P, PF, S, SF>(
    cx: &Context,
    seconds: u64,
    service: Option<&str>,
    check_alive: bool,
    mut alive: A,
    mut probe: P,
    mut now_ms: impl FnMut() -> u64,
    mut sleep: S,
) -> Result<(), UpdateFailure>
where
    A: FnMut() -> AF,
    AF: Future<Output = bool>,
    P: FnMut(Duration) -> PF,
    PF: Future<Output = bool>,
    S: FnMut(Duration) -> SF,
    SF: Future<Output = ()>,
{
    let start = now_ms();
    let deadline = start.saturating_add(seconds.saturating_mul(1000));
    let first = match service {
        Some(service) => service_state(cx, service).await,
        None => None,
    };
    let mut checked = now_ms();
    while now_ms() < deadline {
        if probe(Duration::from_secs(2)).await {
            return Ok(());
        }
        if let Some(service) = service
            && now_ms().saturating_sub(checked) >= 5_000
        {
            checked = now_ms();
            if let Some((active, restarts)) = service_state(cx, service).await {
                let restarted_again = match (first.as_ref().and_then(|row| row.1), restarts) {
                    (Some(before), Some(after)) => after > before,
                    _ => false,
                };
                if matches!(active.as_str(), "failed" | "inactive") || restarted_again {
                    return Err(UpdateFailure::new("restart_failed"));
                }
            }
        }
        if check_alive && now_ms().saturating_sub(checked) >= 15_000 {
            checked = now_ms();
            if !alive().await {
                return Err(UpdateFailure::new("restart_failed"));
            }
        }
        sleep(if service.is_some() {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(300)
        })
        .await;
    }
    Err(UpdateFailure::new("restart_failed"))
}

async fn health_check<P, PF>(
    cx: &Context,
    seconds: u64,
    service: Option<&str>,
    probe: P,
) -> Result<(), UpdateFailure>
where
    P: FnMut(Duration) -> PF,
    PF: Future<Output = bool>,
{
    let origin = Instant::now();
    health_check_core(
        cx,
        seconds,
        service,
        false,
        || std::future::ready(true),
        probe,
        || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX),
        tokio::time::sleep,
    )
    .await
}

async fn health_check_alive<A, AF, P, PF>(
    cx: &Context,
    seconds: u64,
    service: Option<&str>,
    alive: A,
    probe: P,
) -> Result<(), UpdateFailure>
where
    A: FnMut() -> AF,
    AF: Future<Output = bool>,
    P: FnMut(Duration) -> PF,
    PF: Future<Output = bool>,
{
    let origin = Instant::now();
    health_check_core(
        cx,
        seconds,
        service,
        true,
        alive,
        probe,
        || u64::try_from(origin.elapsed().as_millis()).unwrap_or(u64::MAX),
        tokio::time::sleep,
    )
    .await
}

/// `shutil.rmtree(path, ignore_errors=True)`.
async fn remove_tree(path: &Path) {
    let path = path.to_path_buf();
    let _ = tokio::task::spawn_blocking(move || std::fs::remove_dir_all(path)).await;
}

async fn close_desktops(
    cx: &Context,
    install: &Installed,
    contexts: &[Process],
) -> Result<u32, UpdateFailure> {
    if contexts.is_empty() {
        let table = processes::scan(cx).await?;
        if processes::family(&cx.host, install, &table, true).is_empty() {
            return Ok(0);
        }
    }
    processes::terminate_desktops(cx, install, contexts, Duration::from_secs(15), || Ok(())).await
}

fn count(contexts: &[Process]) -> u32 {
    u32::try_from(contexts.len()).unwrap_or(u32::MAX)
}

/// Python `update_mac`: swap beside the installed bundle, reopen captured app, roll back on failure.
async fn update_mac<P, PF>(
    cx: &Context,
    install: &Installed,
    version: &str,
    temporary: &Path,
    health: P,
) -> Result<(u32, u32), Fault>
where
    P: FnMut(Duration) -> PF,
    PF: Future<Output = bool>,
{
    let package = verified_download(cx, version, temporary).await?;
    shared::check_mac_archive(&package)?;
    let ditto = cx.host.system("/usr/bin/ditto");
    let extracted = temporary.join("extracted");
    cx.run(
        &CommandSpec::new([
            ditto.clone().into_os_string(),
            "-x".into(),
            "-k".into(),
            package.as_os_str().to_owned(),
            extracted.as_os_str().to_owned(),
        ])
        .timeout(Duration::from_secs(shared::budget(cx, 120)?)),
    )
    .await?;
    let staged = extracted.join(MAC_NAME);
    verify_mac(cx, &staged, version).await?;
    let backup = install
        .path
        .with_file_name(format!(".aac-t3-rollback-{}.app", rpc::random_hex(16)));
    let adjacent = install
        .path
        .with_file_name(format!(".aac-t3-stage-{}.app", rpc::random_hex(16)));
    let counts = mac_swap(
        cx, install, version, &ditto, &staged, &backup, &adjacent, health,
    )
    .await;
    remove_tree(&adjacent).await;
    if counts.is_ok() && backup.exists() {
        remove_tree(&backup).await;
    }
    counts
}

#[expect(clippy::too_many_arguments, reason = "staged Mac bundle swap")]
async fn mac_swap<P, PF>(
    cx: &Context,
    install: &Installed,
    version: &str,
    ditto: &Path,
    staged: &Path,
    backup: &Path,
    adjacent: &Path,
    health: P,
) -> Result<(u32, u32), Fault>
where
    P: FnMut(Duration) -> PF,
    PF: Future<Output = bool>,
{
    cx.run(
        &CommandSpec::new([
            ditto.as_os_str().to_owned(),
            staged.as_os_str().to_owned(),
            adjacent.as_os_str().to_owned(),
        ])
        .timeout(Duration::from_secs(shared::budget(cx, 120)?)),
    )
    .await?;
    verify_mac(cx, adjacent, version).await?;
    let contexts = processes::main_contexts(cx, install).await?;
    let forced = close_desktops(cx, install, &contexts).await?;
    let mut moved = false;
    let outcome = async {
        std::fs::rename(&install.path, backup)?;
        moved = true;
        std::fs::rename(adjacent, &install.path)?;
        verify_mac(cx, &install.path, version).await?;
        if !contexts.is_empty() {
            processes::restart_desktops(cx, install, &contexts).await?;
        } else {
            let mut spec = CommandSpec::new([
                cx.host.system("/usr/bin/open").into_os_string(),
                "-a".into(),
                install.path.as_os_str().to_owned(),
            ])
            .timeout(Duration::from_secs(15))
            .no_stderr();
            spec.env_mode = common::EnvMode::Exact;
            spec.env = shared::gui_launch_environment(&[])
                .into_iter()
                .map(|(key, value)| (key.into(), value.into()))
                .collect();
            cx.run(&spec).await?;
        }
        health_check(cx, health_budget(cx), None, health).await?;
        Ok(())
    }
    .await;
    let Err(fault) = outcome else {
        return Ok((count(&contexts), forced));
    };
    let rollback: Result<(), Fault> = async {
        if moved {
            close_desktops(cx, install, &processes::main_contexts(cx, install).await?).await?;
            // Ignore errors if `adjacent -> install.path` failed before creating `install.path`.
            remove_tree(&install.path).await;
            std::fs::rename(backup, &install.path)?;
        }
        Ok(())
    }
    .await;
    if processes::live_contexts(cx, &contexts).await.is_empty() {
        let _ = processes::restart_desktops(cx, install, &contexts).await;
    }
    rollback?;
    Err(fault)
}

/// Python `update_windows`: run verified silent NSIS installer; restore previous bundle on failure.
async fn update_windows<P, PF>(
    cx: &Context,
    install: &Installed,
    version: &str,
    temporary: &Path,
    health: P,
) -> Result<(u32, u32), Fault>
where
    P: FnMut(Duration) -> PF,
    PF: Future<Output = bool>,
{
    let package = verified_download(cx, version, temporary).await?;
    verify_windows(cx, &package).await?;
    let contexts = processes::main_contexts(cx, install).await?;
    let backup = temporary.join("rollback");
    let root = install.package_root.clone().unwrap_or_default();
    let copy = backup.clone();
    blocking(move || copy_tree(&root, &copy)).await?;
    let mut installed = false;
    let outcome = async {
        let forced = close_desktops(cx, install, &contexts).await?;
        shared::nsis_silent(cx, &package, shared::budget(cx, 180)?).await?;
        installed = true;
        if windows_bundle_version(&install.path).as_deref() != Some(version) {
            return Err::<u32, Fault>(UpdateFailure::new("version_unknown").into());
        }
        if !contexts.is_empty() {
            processes::restart_desktops(cx, install, &contexts).await?;
        } else {
            shared::powershell(
                cx,
                &format!(
                    "Start-Process -FilePath {}",
                    shared::ps_quote(&install.path.to_string_lossy())
                ),
                Duration::from_secs(15),
                None,
                false,
            )
            .await?;
        }
        health_check_alive(
            cx,
            health_budget(cx),
            None,
            || desktop_family_alive(cx, install),
            health,
        )
        .await?;
        Ok(forced)
    }
    .await;
    match outcome {
        Ok(forced) => Ok((count(&contexts), forced)),
        Err(fault) => windows_rollback(cx, install, &contexts, &backup, installed, fault).await,
    }
}

async fn windows_rollback(
    cx: &Context,
    install: &Installed,
    contexts: &[Process],
    backup: &Path,
    installed: bool,
    fault: Fault,
) -> Result<(u32, u32), Fault> {
    let restored: Result<(), Fault> = async {
        close_desktops(cx, install, &processes::main_contexts(cx, install).await?).await?;
        let root = install.package_root.clone().unwrap_or_default();
        blocking(move || std::fs::remove_dir_all(root)).await?;
        let (from, to) = (
            backup.to_path_buf(),
            install.package_root.clone().unwrap_or_default(),
        );
        blocking(move || copy_tree(&from, &to)).await?;
        Ok(())
    }
    .await;
    if restored.is_err() {
        let recovery = cx
            .state
            .join(format!("t3-code-rollback-{}", rpc::random_hex(16)));
        common::private_dir(&cx.state)?;
        let from = backup.to_path_buf();
        blocking(move || move_tree(&from, &recovery)).await?;
        return Err(UpdateFailure::new(if installed {
            "restart_failed"
        } else {
            "update_failed"
        })
        .into());
    }
    let _ = processes::restart_desktops(cx, install, contexts).await;
    Err(fault)
}

// ---------------------------------------------------------------------------
// One T3 row (Python `update_t3`) and `AppUpdater` implementation.
// ---------------------------------------------------------------------------

/// The lane's updater. W1's registry names it `t3::T3`.
pub(super) static T3: T3Updater = T3Updater;

/// `AppUpdater` implementation for `"t3-code"`.
pub(super) struct T3Updater;

/// Dispatch the two helper subcommands owned by W5 (`--t3-restart`, `--t3-issuer`).
pub(super) fn subcommand(flag: &str, args: &[OsString]) -> Option<i32> {
    match flag {
        RESTART_FLAG => Some(restart_worker(args)),
        ISSUER_FLAG => Some(rpc::issuer_bootstrap(args)),
        _ => None,
    }
}

impl AppUpdater for T3Updater {
    fn app_id(&self) -> &'static str {
        "t3-code"
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            match detect_t3(cx).await {
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
        Box::pin(async move { Ok(update_t3_at(cx, install, Path::new("/proc")).await) })
    }
}

struct Attempt {
    attempted: bool,
    after: Option<String>,
}

async fn update_t3_at(cx: &Context, install: &Installed, proc: &Path) -> AppRow {
    update_t3_with(
        cx,
        install,
        proc,
        |version| async move { rpc::desktop_offered(cx, &version).await },
        |version| async move { update_running_desktop(cx, install, &version).await },
        |timeout| http_ok(HEALTH_URL, timeout),
    )
    .await
}

async fn update_t3_with<O, OF, S, SF, H, HF>(
    cx: &Context,
    install: &Installed,
    proc: &Path,
    offered: O,
    self_update: S,
    health: H,
) -> AppRow
where
    O: FnOnce(String) -> OF,
    OF: Future<Output = bool>,
    S: FnOnce(String) -> SF,
    SF: Future<Output = Result<String, Fault>>,
    H: FnMut(Duration) -> HF,
    HF: Future<Output = bool>,
{
    let mut attempt = Attempt {
        attempted: false,
        after: install.version.clone(),
    };
    match run_update(
        cx,
        install,
        &mut attempt,
        proc,
        offered,
        self_update,
        health,
    )
    .await
    {
        Ok(row) => row,
        Err(fault) => {
            let failure = &fault.0;
            if fault.is_named()
                && let Some(cause) = failure.cause
            {
                cx.record(|record| record.blame(cause, failure.reason));
            }
            if attempt.attempted && is_desktop(install) {
                attempt.after = installed_desktop_version(cx, install).await;
            }
            attempt.failure_row(install, &fault)
        }
    }
}

fn is_desktop(install: &Installed) -> bool {
    install.manager == "official-download"
}

async fn run_update<O, OF, S, SF, H, HF>(
    cx: &Context,
    install: &Installed,
    attempt: &mut Attempt,
    proc: &Path,
    offered: O,
    self_update: S,
    health: H,
) -> Result<AppRow, Fault>
where
    O: FnOnce(String) -> OF,
    OF: Future<Output = bool>,
    S: FnOnce(String) -> SF,
    SF: Future<Output = Result<String, Fault>>,
    H: FnMut(Duration) -> HF,
    HF: Future<Output = bool>,
{
    let desktop = is_desktop(install);
    let ubuntu = install.platform == Platform::Ubuntu;
    let Some(before) = install.version.clone() else {
        return Err(UpdateFailure::new("version_unknown").into());
    };
    let (runtime, runtime_version) = if !desktop {
        // Standalone runtime install: `install.path` and `install.version` already carry it.
        (Some(install.path.clone()), Some(before.clone()))
    } else if install.platform == Platform::Mac {
        let rt = runtime_install(&cx.host.home);
        let rt_version = match &rt {
            Some(path) => probe_version(&shared::cli_probe(cx, path, &["--version"]).await),
            None => None,
        };
        (rt, rt_version)
    } else {
        (None, None)
    };
    if runtime.is_some() && runtime_version.is_none() {
        return Err(UpdateFailure::new("version_unknown").into());
    }
    let version = latest_release(cx).await?;
    let desktop_needed = desktop && newer(&version, &before);
    let runtime_needed = runtime_version
        .as_deref()
        .is_some_and(|current| newer(&version, current));
    let pending = cx.state.join(PENDING_MARKER);
    if !desktop_needed && !runtime_needed {
        if ubuntu && pending.exists() {
            if running_runtime_version_at(cx, proc).await == runtime_version {
                std::fs::remove_file(&pending)?;
                ubuntu_companions(cx, false, reload_unit(cx).await, proc);
                return Ok(AppRow::of(
                    install,
                    Status::Current,
                    Some(&before),
                    Some(&before),
                    "current",
                ));
            }
            let mut row = AppRow::of(
                install,
                Status::Updated,
                Some(&before),
                Some(&before),
                "t3_restart_scheduled",
            );
            ubuntu_companions(cx, true, false, proc);
            row.restart_targets = Some(vec![schedule_restart(cx, &before).await?]);
            return Ok(row);
        }
        if ubuntu {
            ubuntu_companions(cx, false, reload_unit(cx).await, proc);
        }
        return Ok(AppRow::of(
            install,
            Status::Current,
            Some(&before),
            Some(&before),
            "current",
        ));
    }
    let mut after = before.clone();
    let (mut restarted, mut forced) = (0, 0);
    let mut daemon_reload = false;
    {
        let temporary = shared::private_temporary(cx)?;
        if let (true, Some(runtime)) = (runtime_needed, runtime.as_deref()) {
            attempt.attempted = true;
            update_runtime(cx, runtime, &version).await?;
            if ubuntu {
                daemon_reload = reload_unit(cx).await;
            }
            if !desktop {
                after = version.clone();
                attempt.after = Some(after.clone());
            }
        }
        if desktop_needed {
            attempt.attempted = true;
            if offered(before.clone()).await {
                after = self_update(version.clone()).await?;
                attempt.after = Some(after.clone());
                restarted = 1;
            } else {
                (restarted, forced) = match install.platform {
                    Platform::Mac => {
                        update_mac(cx, install, &version, temporary.path(), health).await?
                    }
                    _ => update_windows(cx, install, &version, temporary.path(), health).await?,
                };
                after = version.clone();
                attempt.after = Some(after.clone());
            }
        }
    }
    let code = if ubuntu {
        "t3_restart_scheduled"
    } else {
        "t3_updated"
    };
    let mut row = AppRow::of(install, Status::Updated, Some(&before), Some(&after), code)
        .attempted(restarted);
    row.forced_stops = Some(forced);
    row.restart_targets = Some(if ubuntu {
        ubuntu_companions(cx, true, daemon_reload, proc);
        vec![schedule_restart(cx, &after).await?]
    } else if desktop_needed {
        vec![json!({"kind": "desktop"})]
    } else {
        Vec::new()
    });
    Ok(row)
}

impl Attempt {
    /// Python `update_t3`'s two `except` branches.
    fn failure_row(self, install: &Installed, fault: &Fault) -> AppRow {
        let failure = &fault.0;
        let changed = self
            .after
            .as_deref()
            .is_some_and(|after| !after.is_empty() && Some(after) != install.version.as_deref());
        let (status, code) = if !fault.is_named() {
            (Status::Failed, "update_failed")
        } else {
            let status = if changed || failure.code == "restart_failed" {
                Status::RestartFailed
            } else {
                Status::Failed
            };
            let code = if failure.code == "download_blocked" {
                "update_failed"
            } else {
                failure.code
            };
            (status, code)
        };
        let mut row = AppRow::of(
            install,
            status,
            install.version.as_deref(),
            self.after.as_deref(),
            code,
        );
        row.update_attempted = self.attempted;
        row
    }
}

async fn installed_desktop_version(cx: &Context, install: &Installed) -> Option<String> {
    if install.platform == Platform::Mac {
        mac_bundle_version(cx, &install.path).await
    } else {
        windows_bundle_version(&install.path)
    }
}

async fn update_running_desktop(
    cx: &Context,
    install: &Installed,
    version: &str,
) -> Result<String, Fault> {
    let before = install.version.clone().unwrap_or_default();
    let accept = |value: &str| exact_version(value).is_some_and(|v| newer(v, &before));
    let installed = rpc::desktop_self_update(cx, version, accept, health_budget(cx)).await?;
    if installed_desktop_version(cx, install).await.as_deref() != Some(installed.as_str()) {
        return Err(UpdateFailure::new("restart_failed")
            .with_cause(FailureCause::Unknown, Some("t3_self_update_version"))
            .into());
    }
    Ok(installed)
}

async fn schedule_restart(cx: &Context, version: &str) -> Result<Value, Fault> {
    common::write_private_json(&cx.state.join(PENDING_MARKER), &json!({"version": version}))?;
    let agent_job = std::env::var("AAC_UPDATE_AGENT_JOB")
        .ok()
        .filter(|job| AGENT_JOB.is_match(job));
    let dashboard = std::env::var("AAC_UPDATE_DASHBOARD_JOB").ok().as_deref() == Some("1");
    let unit = format!("aac-t3-restart-{}", &rpc::random_hex(16)[..12]);
    let mut argv = vec![
        cx.host.system("/usr/bin/systemd-run").into_os_string(),
        "--user".into(),
        "--collect".into(),
        format!("--unit={unit}").into(),
        "--property=RuntimeMaxSec=5400".into(),
        helper_binary(),
        RESTART_FLAG.into(),
        "--restart-service".into(),
        std::process::id().to_string().into(),
        "--state-dir".into(),
        cx.state.as_os_str().to_owned(),
    ];
    match agent_job {
        Some(job) => argv.extend(["--agent-job".into(), job.into()]),
        None if dashboard => argv.push("--dashboard-job".into()),
        None => {}
    }
    cx.run(&CommandSpec::new(argv).timeout(Duration::from_secs(15)))
        .await?;
    Ok(json!({"kind": "systemd", "service": SERVICE, "delaySeconds": 30}))
}

fn helper_binary() -> OsString {
    std::env::current_exe()
        .map(PathBuf::into_os_string)
        .unwrap_or_else(|_| "aac-agent".into())
}

async fn reload_unit(cx: &Context) -> bool {
    let systemctl = cx.host.system("/usr/bin/systemctl").into_os_string();
    let query = CommandSpec::new([
        systemctl.clone(),
        "--user".into(),
        "show".into(),
        "--property=NeedDaemonReload".into(),
        "--value".into(),
        SERVICE.into(),
    ])
    .timeout(Duration::from_secs(10))
    .capture();
    if !cx
        .run(&query)
        .await
        .is_ok_and(|output| output.stdout.trim() == "yes")
    {
        return false;
    }
    cx.run(
        &CommandSpec::new([systemctl, "--user".into(), "daemon-reload".into()])
            .timeout(Duration::from_secs(30)),
    )
    .await
    .is_ok()
}

async fn start_acp_updater(cx: &Context) -> &'static str {
    if !shared::acp_unit_present(&cx.host) {
        return "absent";
    }
    let ran = cx
        .run(
            &CommandSpec::new([
                cx.host.system("/usr/bin/systemctl").into_os_string(),
                "--user".into(),
                "start".into(),
                "--no-block".into(),
                shared::ACP_UNIT.into(),
            ])
            .timeout(Duration::from_secs(15)),
        )
        .await;
    if ran.is_err() {
        return "failed";
    }
    "started"
}

fn utc_now() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let seconds = i64::try_from(seconds).unwrap_or_default();
    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|now| now.format("%Y-%m-%dT%H:%M:%SZ").to_string())
        .unwrap_or_default()
}

/// Python `record_components`: always writes every key in `COMPONENT_KEYS` (missing keys are `null`).
fn record_components(root: &Path, values: &[(&str, Value)]) -> Option<Value> {
    let path = root.join(COMPONENTS_RECORD);
    let previous = crate::apps::read_json(&path, 64 * 1024)
        .and_then(|value| match value {
            Value::Object(map) => Some(map),
            _ => None,
        })
        .unwrap_or_default();
    let mut record = Map::new();
    for key in COMPONENT_KEYS {
        record.insert(
            key.into(),
            previous.get(key).cloned().unwrap_or(Value::Null),
        );
    }
    for (key, value) in values {
        record.insert((*key).into(), value.clone());
    }
    record.insert("utc".into(), Value::String(utc_now()));
    common::write_private_json(&path, &Value::Object(record.clone())).ok()?;
    Some(Value::Object(record))
}

fn ubuntu_companions(cx: &Context, restarting: bool, daemon_reload: bool, proc: &Path) {
    let present = shared::acp_unit_present(&cx.host);
    let acp = match (restarting, present) {
        (true, true) => "pending",
        (true, false) => "absent",
        (false, true) => "rows",
        (false, false) => "absent",
    };
    let pruned = if restarting {
        Vec::new()
    } else {
        prune_runtimes(&cx.host.home, proc)
    };
    record_components(
        &cx.state,
        &[
            ("cursorAgent", json!({"status": "separate_row"})),
            ("acpUpdater", json!(acp)),
            ("prunedRuntimes", json!(pruned)),
            ("daemonReload", json!(daemon_reload)),
        ],
    );
}

async fn finish_ubuntu_restart(cx: &Context, daemon_reload: bool, proc: &Path) -> String {
    let pruned = prune_runtimes(&cx.host.home, proc);
    let acp = start_acp_updater(cx).await;
    record_components(
        &cx.state,
        &[
            ("acpUpdater", json!(acp)),
            ("prunedRuntimes", json!(pruned)),
            ("daemonReload", json!(daemon_reload)),
        ],
    );
    let pruned_text = if pruned.is_empty() {
        "none".to_owned()
    } else {
        pruned.join(", ")
    };
    let line = format!(
        "T3 companions: ACP updater {acp}; pruned {pruned_text}; daemon-reload {}",
        if daemon_reload { "yes" } else { "no" }
    );
    println!("{line}");
    line
}

fn marker_version(marker: &Path) -> Option<String> {
    let value = crate::apps::read_json(marker, 64 * 1024)?;
    version_of(value.get("version")?).map(str::to_owned)
}

// ---------------------------------------------------------------------------
// Running runtime inspection & pruning (Python `running_runtime_version`, `prune_runtimes`).
// ---------------------------------------------------------------------------

fn runtime_under(path: &Path, versions: &Path) -> Option<String> {
    let parts: Vec<_> = path.strip_prefix(versions).ok()?.components().collect();
    (parts.len() > 1).then(|| parts[0].as_os_str().to_string_lossy().into_owned())
}

fn exe_target(link: &Path) -> PathBuf {
    let text = link.to_string_lossy();
    Path::new(text.strip_suffix(" (deleted)").unwrap_or(&text)).to_path_buf()
}

fn second(argv: &[String]) -> Option<&str> {
    argv.get(1).map(String::as_str)
}

fn runtime_of(pid: &str, versions: &Path, proc: &Path) -> Option<(String, Vec<String>)> {
    let executable = exe_target(&std::fs::read_link(proc.join(pid).join("exe")).ok()?);
    let folder = executable.parent()?;
    if executable.file_name()?.to_str()? != "t3"
        || folder.parent()? != versions
        || exact_version(folder.file_name()?.to_str()?).is_none()
    {
        return None;
    }
    let argv = std::fs::read(proc.join(pid).join("cmdline"))
        .map(|raw| {
            raw[..raw.len().min(4096)]
                .split(|byte| *byte == 0)
                .map(|item| String::from_utf8_lossy(item).into_owned())
                .collect()
        })
        .unwrap_or_default();
    Some((folder.file_name()?.to_str()?.to_owned(), argv))
}

fn is_pid_name(name: &str) -> bool {
    !name.is_empty() && name.bytes().all(|byte| byte.is_ascii_digit())
}

fn child_pids(pid: &str, proc: &Path) -> Vec<String> {
    let mut children: Vec<String> = Vec::new();
    let mut tasks: Vec<PathBuf> = std::fs::read_dir(proc.join(pid).join("task"))
        .map(|tasks| tasks.flatten().map(|entry| entry.path()).collect())
        .unwrap_or_default();
    tasks.sort();
    for task in tasks.into_iter().take(64) {
        if let Ok(text) = std::fs::read_to_string(task.join("children"))
            && text.is_ascii()
        {
            children.extend(text.split_whitespace().map(str::to_owned));
        }
    }
    children.truncate(256);
    children.retain(|child| is_pid_name(child));
    children
}

async fn running_runtime_version_at(cx: &Context, proc: &Path) -> Option<String> {
    let spec = CommandSpec::new([
        cx.host.system("/usr/bin/systemctl").into_os_string(),
        "--user".into(),
        "show".into(),
        "--property=MainPID".into(),
        "--value".into(),
        SERVICE.into(),
    ])
    .timeout(Duration::from_secs(10))
    .capture();
    let output = cx.run(&spec).await.ok()?;
    let pid = output.stdout.trim().to_owned();
    if !is_pid_name(&pid) || pid.parse::<u64>().ok()? <= 1 {
        return None;
    }
    let versions = crate::apps::real(&cx.host.home.join(".t3/runtime/versions"));
    let (version, argv) = runtime_of(&pid, &versions, proc)?;
    if second(&argv) != Some("__service-launcher") {
        return exact_version(&version).map(str::to_owned);
    }
    let servers: HashSet<String> = child_pids(&pid, proc)
        .iter()
        .filter_map(|child| runtime_of(child, &versions, proc))
        .filter(|(_, argv)| second(argv) == Some("serve"))
        .map(|(version, _)| version)
        .collect();
    (servers.len() == 1).then(|| servers.into_iter().next().unwrap_or_default())
}

/// Python `running_runtime_versions`: `None` when `proc` cannot be scanned (never delete
/// runtimes when process visibility is lost).
fn running_runtime_versions(versions: &Path, proc: &Path) -> Option<HashSet<String>> {
    let pids: Vec<String> = std::fs::read_dir(proc)
        .ok()?
        .flatten()
        .filter_map(|entry| entry.file_name().into_string().ok())
        .filter(|name| is_pid_name(name))
        .collect();
    let named = regex::Regex::new(&format!(
        "{}/([^/\\x00 \\t\\r\\n\\x0b\\x0c]+)",
        regex::escape(&versions.to_string_lossy())
    ))
    .ok()?;
    let mut kept = HashSet::new();
    for pid in pids {
        let executed = std::fs::read_link(proc.join(&pid).join("exe"))
            .ok()
            .and_then(|link| runtime_under(&exe_target(&link), versions));
        kept.extend(executed);
        let Ok(raw) = std::fs::read(proc.join(&pid).join("cmdline")) else {
            continue;
        };
        let text = String::from_utf8_lossy(&raw[..raw.len().min(65_536)]);
        kept.extend(
            named
                .captures_iter(&text)
                .filter_map(|found| exact_version(&found[1]).map(str::to_owned)),
        );
    }
    Some(kept)
}

fn active_runtime_version(home: &Path) -> Option<String> {
    let state = crate::apps::read_json(&home.join(".t3/runtime/service-state.json"), 64 * 1024)?;
    version_of(state.get("activeVersion")?).map(str::to_owned)
}

fn real_directory(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|info| info.is_dir())
}

fn prune_runtimes(home: &Path, proc: &Path) -> Vec<String> {
    let versions = crate::apps::real(&home.join(".t3/runtime/versions"));
    let Some(active) = active_runtime_version(home) else {
        return Vec::new();
    };
    let Some(running) = running_runtime_versions(&versions, proc) else {
        return Vec::new();
    };
    let mut keep: HashSet<String> = running;
    keep.insert(active.clone());
    keep.extend(runtime_under(
        &crate::apps::real(&home.join(".local/bin/t3")),
        &versions,
    ));
    let mut older: Vec<String> = std::fs::read_dir(&versions)
        .map(|entries| {
            entries
                .flatten()
                .filter_map(|entry| entry.file_name().into_string().ok())
                .filter(|name| {
                    exact_version(name).is_some()
                        && version_key(name) < version_key(&active)
                        && real_directory(&versions.join(name))
                })
                .collect()
        })
        .unwrap_or_default();
    older.sort_by_key(|name| version_key(name));
    if let Some(rollback) = older.last() {
        keep.insert(rollback.clone());
    }
    let mut deleted = Vec::new();
    for name in older
        .into_iter()
        .filter(|name| !keep.contains(name))
        .take(PRUNE_LIMIT)
    {
        if std::fs::remove_dir_all(versions.join(&name)).is_ok() {
            deleted.push(name);
        }
    }
    deleted
}

// ---------------------------------------------------------------------------
// Detached T3 restart worker (Python `deferred_restart`, `app_update_t3.main`).
// ---------------------------------------------------------------------------

fn job_finished(root: &Path, dashboard: bool) -> bool {
    let raw = match std::fs::read(root.join("dashboard-job.json")) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return !dashboard,
        Err(_) => return false,
    };
    if raw.len() > 1024 * 1024 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        return false;
    };
    let Some(job) = value.get("job").and_then(Value::as_object) else {
        return false;
    };
    let done = |host: &Value| {
        host.as_object()
            .is_some_and(|host| host.get("state") == Some(&Value::String("done".into())))
    };
    let ended = matches!(
        job.get("state").and_then(Value::as_str),
        Some("completed" | "failed")
    );
    let hosts = job.get("hosts");
    if dashboard {
        return match hosts {
            Some(Value::Object(hosts)) => hosts.get("ubuntu").is_some_and(done),
            Some(Value::Null) | None => ended,
            Some(_) => false,
        };
    }
    ended
        && match hosts {
            None | Some(Value::Null) => true,
            Some(Value::Object(hosts)) => !hosts.is_empty() && hosts.values().all(done),
            _ => false,
        }
}

fn agent_job_finished(root: &Path, job: &str) -> bool {
    use std::io::Read as _;
    let Ok(stream) = std::fs::File::open(root.join("agent-journal.json")) else {
        return false;
    };
    let mut raw = Vec::new();
    let mut limited = stream.take(65_537);
    if limited.read_to_end(&mut raw).is_err() || raw.len() > 65_536 {
        return false;
    }
    let Ok(value) = serde_json::from_slice::<Value>(&raw) else {
        return false;
    };
    let labels: HashSet<&str> = aac_providers::app_inventory().collect();
    value.get("job").and_then(Value::as_str) == Some(job)
        && value.get("platform").and_then(Value::as_str) == Some("ubuntu")
        && value.get("running") == Some(&Value::Array(Vec::new()))
        && value
            .get("results")
            .and_then(Value::as_object)
            .is_some_and(|results| {
                results.len() == labels.len()
                    && results.keys().all(|key| labels.contains(key.as_str()))
            })
}

fn restart_lock_owner_alive(path: &Path, identity: &dyn Fn(u32) -> Option<String>) -> bool {
    let fresh = || {
        std::fs::metadata(path)
            .ok()
            .and_then(|info| info.modified().ok())
            .is_some_and(|modified| {
                std::time::SystemTime::now()
                    .duration_since(modified)
                    .is_ok_and(|age| age < Duration::from_secs(300))
            })
    };
    let parsed = (|| -> Option<(u32, Option<String>)> {
        let value = crate::apps::read_json(path, 64 * 1024)?;
        let fields = value.as_object()?;
        let pid = fields.get("pid")?;
        if pid.is_boolean() {
            return None;
        }
        let pid = u32::try_from(pid.as_u64().filter(|pid| *pid >= 1)?).ok()?;
        Some((
            pid,
            fields
                .get("identity")
                .and_then(Value::as_str)
                .map(str::to_owned),
        ))
    })();
    match parsed {
        Some((pid, Some(recorded))) => match identity(pid) {
            Some(current) => current == recorded && pid_exists(pid),
            None => pid_exists(pid),
        },
        Some((pid, None)) => pid_exists(pid),
        None => fresh(),
    }
}

fn same_file(first: &std::fs::Metadata, second: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        (first.dev(), first.ino()) == (second.dev(), second.ino())
    }
    #[cfg(not(unix))]
    {
        first
            .modified()
            .ok()
            .zip(second.modified().ok())
            .is_some_and(|(a, b)| a == b)
    }
}

fn claim_restart_lock(path: &Path, alive: &dyn Fn(&Path) -> bool) -> Option<std::fs::File> {
    let create = || -> std::io::Result<std::fs::File> {
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        options.open(path)
    };
    loop {
        match create() {
            Ok(file) => return Some(file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return None,
        }
        let Ok(before) = std::fs::metadata(path) else {
            continue;
        };
        if alive(path) {
            return None;
        }
        let after = match std::fs::metadata(path) {
            Ok(after) => after,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
        };
        if !same_file(&before, &after) {
            return None;
        }
        match std::fs::remove_file(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return None,
            Ok(()) => {}
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Turn {
    Retry,
    Wait,
    Done,
}

impl From<bool> for Turn {
    fn from(restarted: bool) -> Self {
        if restarted { Self::Done } else { Self::Wait }
    }
}

async fn restart_service<H, HF>(
    cx: &Context,
    marker: &Path,
    claimed: std::fs::File,
    finished: &dyn Fn() -> bool,
    proc: &Path,
    health: H,
) -> Result<bool, Fault>
where
    H: FnOnce(u64, &str) -> HF,
    HF: Future<Output = Result<(), UpdateFailure>>,
{
    {
        let mut claimed = claimed;
        let identity = shared::process_identity(std::process::id());
        let record = json!({"pid": std::process::id(), "identity": identity}).to_string();
        std::io::Write::write_all(&mut claimed, record.as_bytes())?;
    }
    if !(marker.exists() && finished()) {
        return Ok(false);
    }
    let Some(expected) = marker_version(marker) else {
        return Err(UpdateFailure::new("restart_failed").into());
    };
    let daemon_reload = reload_unit(cx).await;
    cx.run(
        &CommandSpec::new([
            cx.host.system("/usr/bin/systemctl").into_os_string(),
            "--user".into(),
            "restart".into(),
            SERVICE.into(),
        ])
        .timeout(Duration::from_secs(60)),
    )
    .await?;
    health(UBUNTU_READY_SECONDS, SERVICE).await?;
    if running_runtime_version_at(cx, proc).await.as_deref() != Some(expected.as_str()) {
        return Err(UpdateFailure::new("restart_failed").into());
    }
    match std::fs::remove_file(marker) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => return Err(error.into()),
        _ => {}
    }
    let _ = finish_ubuntu_restart(cx, daemon_reload, proc).await;
    Ok(true)
}

#[expect(clippy::too_many_arguments, reason = "testable clock/health/pid seam")]
async fn deferred_restart_core<H, HF, S, SF>(
    cx: &Context,
    parent: u32,
    root: &Path,
    dashboard: bool,
    agent_job: Option<&str>,
    proc: &Path,
    mut parent_check: impl FnMut(u32) -> Option<bool>,
    mut health: H,
    mut now_secs: impl FnMut() -> u64,
    mut sleep: S,
) -> Result<(), UpdateFailure>
where
    H: FnMut(u64, &str) -> HF,
    HF: Future<Output = Result<(), UpdateFailure>>,
    S: FnMut(Duration) -> SF,
    SF: Future<Output = ()>,
{
    let lock_file = root.join("dashboard-update.lock");
    let dashboard = dashboard || agent_job.is_some();
    let finished = move || match agent_job {
        Some(job) => agent_job_finished(root, job),
        None => job_finished(root, dashboard),
    };
    let worker_lock = if dashboard {
        root.join("t3-restart.lock")
    } else {
        lock_file.clone()
    };
    let marker = root.join(PENDING_MARKER);
    let deadline = now_secs().saturating_add(35 * 60);
    let mut quiet_since: Option<u64> = None;
    while now_secs() < deadline {
        if !marker.exists() {
            return Ok(());
        }
        let Some(parent_alive) = parent_check(parent) else {
            return Err(UpdateFailure::failed());
        };
        let turn: Result<Turn, Fault> = async {
            let Ok(_held) = shared::execution_lock(
                &cx.host.home.join(".ccs/app-updates"),
                "helper-update.lock",
            ) else {
                quiet_since = None;
                return Ok(Turn::Wait);
            };
            if parent_alive || (lock_file.exists() && !dashboard) || !finished() {
                quiet_since = None;
                return Ok(Turn::Wait);
            }
            let now = now_secs();
            let Some(since) = quiet_since else {
                quiet_since = Some(now);
                return Ok(Turn::Wait);
            };
            if now.saturating_sub(since) < 30 {
                return Ok(Turn::Wait);
            }
            let claimed = claim_restart_lock(&worker_lock, &|path| {
                restart_lock_owner_alive(path, &shared::process_identity)
            });
            let Some(claimed) = claimed else {
                quiet_since = None;
                return Ok(Turn::Retry);
            };
            let restarted =
                restart_service(cx, &marker, claimed, &finished, proc, |seconds, svc| {
                    health(seconds, svc)
                })
                .await;
            let _ = std::fs::remove_file(&worker_lock);
            restarted.map(Turn::from)
        }
        .await;
        match turn {
            Ok(Turn::Retry) => continue,
            Ok(Turn::Wait) => {}
            Ok(Turn::Done) => return Ok(()),
            Err(fault) => return Err(fault.0),
        }
        sleep(Duration::from_secs(1)).await;
    }
    Err(UpdateFailure::new("restart_failed"))
}

async fn deferred_restart_at(
    cx: &Context,
    parent: u32,
    root: &Path,
    dashboard: bool,
    agent_job: Option<&str>,
    proc: &Path,
) -> Result<(), UpdateFailure> {
    let origin = Instant::now();
    deferred_restart_core(
        cx,
        parent,
        root,
        dashboard,
        agent_job,
        proc,
        pid_alive,
        |seconds, service| {
            let service = service.to_owned();
            async move {
                health_check(cx, seconds, Some(&service), |timeout| {
                    http_ok(HEALTH_URL, timeout)
                })
                .await
            }
        },
        || origin.elapsed().as_secs(),
        tokio::time::sleep,
    )
    .await
}

struct WorkerArguments {
    parent: u32,
    root: PathBuf,
    dashboard: bool,
    agent_job: Option<String>,
}

impl WorkerArguments {
    fn parse(args: &[OsString]) -> Option<Self> {
        let (mut parent, mut root, mut agent_job) = (None, None, None);
        let mut dashboard = false;
        let mut words = args.iter();
        while let Some(word) = words.next() {
            match word.to_str()? {
                "--restart-service" => {
                    parent = Some(words.next()?.to_str()?.trim().parse::<i64>().ok()?);
                }
                "--state-dir" => root = Some(PathBuf::from(words.next()?)),
                "--dashboard-job" => dashboard = true,
                "--agent-job" => agent_job = Some(words.next()?.to_str()?.to_owned()),
                _ => return None,
            }
        }
        let parent = u32::try_from(parent?).ok().filter(|pid| *pid > 1)?;
        let agent_job = agent_job.filter(|job| !job.is_empty());
        if agent_job
            .as_deref()
            .is_some_and(|job| !AGENT_JOB.is_match(job))
        {
            return None;
        }
        let root = match root {
            Some(root) => root,
            None => std::env::home_dir()?.join(".ccs/app-updates"),
        };
        root.is_absolute().then_some(Self {
            parent,
            root,
            dashboard,
            agent_job,
        })
    }
}

fn worker_context(root: &Path) -> Option<Context> {
    let host = Arc::new(crate::apps::Host::native(Platform::Ubuntu)?);
    Some(Context::new(
        Platform::Ubuntu,
        host,
        Arc::new(Native),
        Arc::new(Shared::default()),
        Instant::now() + Duration::from_secs(35 * 60),
        root.join("t3-restart-stage"),
        root.to_path_buf(),
        None,
        || false,
        |_| {},
    ))
}

fn restart_worker(args: &[OsString]) -> i32 {
    if !cfg!(target_os = "linux") {
        return 2;
    }
    let Some(options) = WorkerArguments::parse(args) else {
        return 2;
    };
    let worker = std::thread::Builder::new()
        .name("t3-restart".into())
        .spawn(move || {
            let Some(cx) = worker_context(&options.root) else {
                return 1;
            };
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            else {
                return 1;
            };
            match runtime.block_on(deferred_restart_at(
                &cx,
                options.parent,
                &options.root,
                options.dashboard,
                options.agent_job.as_deref(),
                Path::new("/proc"),
            )) {
                Ok(()) => 0,
                Err(_) => 1,
            }
        });
    match worker {
        Ok(worker) => worker.join().unwrap_or(1),
        Err(_) => 1,
    }
}
