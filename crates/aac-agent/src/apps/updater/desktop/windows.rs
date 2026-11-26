//! Windows MSIX desktop packages (Python `windows_package`, `update_windows`,
//! `windows_capture`, `windows_replace` and helpers). A running app is closed,
//! updated and reopened as each instance was started; one that cannot be reopened
//! exactly is never closed (`quit_first`).
use super::{
    RESERVE, current, desktop_budget, download_desktop, download_json, failed, quit_first, result,
    version_tuple, xml,
};
use crate::apps::updater::{
    AppRow, Context, Installed, Platform, Probe, Process, Status, UpdateFailure,
    common::version_text,
    processes::{self, desktop_data_folder, desktop_profile, family, same_process},
    shared::{self, Env, ps_quote},
};
use serde_json::{Value, json};
use std::{collections::HashMap, path::Path, path::PathBuf, time::Duration};

/// The Store feed the Codex app itself reads to learn that a newer Store build exists.
pub(super) const CODEX_STORE_FEED: &str =
    "https://persistent.oaistatic.com/codex-app-prod/windows-store-update.json";
pub(super) const CODEX_STORE_ID: &str = "9PLM9XGG6VKS";
/// OpenAI's release feed names each version's own Store-signed MSIX per architecture
/// (winget cannot install it: its msstore source never matches the installed package).
pub(super) const CODEX_RELEASE_FEED: &str =
    "https://persistent.oaistatic.com/codex-app-prod/windows-release.json";
pub(super) const CODEX_RELEASE_PREFIX: &str =
    "https://persistent.oaistatic.com/codex-app-prod/releases/";
/// Closing a running app needs this much time left for the close (15 s), the install
/// (about four minutes for the 976 MB Codex MSIX) and the reopen (45 s).
const WINDOWS_REPLACE_SECONDS: u64 = 300;
/// A relaunched MSIX instance cold-starts slower than T3; still bounded.
const WINDOWS_REOPEN_SECONDS: u64 = 45;
/// Reading only an MSIX's manifest over HTTP ranges takes well under a second.
const REMOTE_MANIFEST_SECONDS: u64 = 20;

/// Python `WINDOWS`: (package identity, executable, AppId of its Application element).
pub(super) fn package_names(app_id: &str) -> (&'static str, &'static str, &'static str) {
    if app_id == "codex-desktop" {
        ("OpenAI.Codex", "app/ChatGPT.exe", "App")
    } else {
        ("Claude", "app/Claude.exe", "Claude")
    }
}

fn unknown_package(cx: &Context, app_id: &'static str, path: PathBuf, probe: Probe) -> Installed {
    let mut install = Installed::new(app_id, cx.platform, path, None, "msix");
    install.probe = probe;
    install
}

/// Only the exact `<Name>_<13-character publisher id>` may become an AUMID.
pub(super) fn exact_family(identity: &str, family: Option<&str>) -> Option<String> {
    let rest = family?.strip_prefix(identity)?.strip_prefix('_')?;
    (rest.len() == 13
        && rest
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit()))
    .then(|| family.unwrap_or_default().to_owned())
}

/// Python `windows_package(app_id)`: the registered package and its family name
/// (`None` family when it was not exactly that shape). `None` when not installed.
pub(super) async fn package(
    cx: &Context,
    app_id: &'static str,
    quiet: bool,
) -> Option<(Installed, Option<String>)> {
    let (identity, executable, _) = package_names(app_id);
    // Sort-Object keeps the query a single object when two versions coexist mid-staging.
    let script = format!(
        "$ErrorActionPreference='Stop'; $p=Get-AppxPackage -Name {} | Sort-Object {{[Version]$_.Version}} -Descending | Select-Object -First 1; if($p){{@{{name=$p.Name;version=$p.Version.ToString();publisher=$p.Publisher;root=$p.InstallLocation;family=$p.PackageFamilyName}}|ConvertTo-Json -Compress}}",
        ps_quote(identity)
    );
    let raw = match shared::powershell(cx, &script, Duration::from_secs(30), None, quiet).await {
        Ok(raw) => raw,
        // A query that ran out of time says nothing about the package.
        Err(failure) => {
            let probe = if failure.code == "timeout" {
                Probe::Timeout
            } else {
                Probe::Failed
            };
            return Some((unknown_package(cx, app_id, PathBuf::new(), probe), None));
        }
    };
    if raw.trim().is_empty() {
        return None;
    }
    let Ok(Value::Object(value)) = serde_json::from_str::<Value>(raw.trim()) else {
        return Some((
            unknown_package(cx, app_id, PathBuf::new(), Probe::Failed),
            None,
        ));
    };
    let Some(root) = value
        .get("root")
        .and_then(Value::as_str)
        .filter(|_| value.get("name").and_then(Value::as_str) == Some(identity))
    else {
        return Some((
            unknown_package(cx, app_id, PathBuf::new(), Probe::Failed),
            None,
        ));
    };
    let root = PathBuf::from(root);
    let target = root.join(executable);
    if !target.is_file() {
        return Some((unknown_package(cx, app_id, target, Probe::Failed), None));
    }
    let family = exact_family(identity, value.get("family").and_then(Value::as_str));
    let version = value
        .get("version")
        .and_then(Value::as_str)
        .and_then(version_text);
    let publisher = value
        .get("publisher")
        .and_then(Value::as_str)
        .filter(|publisher| !publisher.is_empty())
        .map(str::to_owned);
    let mut install = Installed::new(app_id, cx.platform, target, version, "msix");
    install.probe = if install.version.is_some() && publisher.is_some() {
        Probe::Ok
    } else {
        Probe::Failed
    };
    install.identity = Some(identity.to_owned());
    install.publisher = publisher;
    install.package_root = Some(root);
    Some((install, family))
}

/// One member of a local ZIP (ZIP64 included), at most `cap` bytes; `Ok(None)` when
/// absent. Stand-in until W1's `shared::zip_member` (same contract) lands.
pub(super) fn local_zip_member(
    path: &Path,
    member: &str,
    cap: u64,
) -> Result<Option<Vec<u8>>, UpdateFailure> {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let bad = UpdateFailure::failed;
    let mut file = std::fs::File::open(path).map_err(|_| bad())?;
    let at = |file: &mut std::fs::File, offset: u64, len: u64| -> Result<Vec<u8>, UpdateFailure> {
        file.seek(SeekFrom::Start(offset)).map_err(|_| bad())?;
        let mut data = vec![0; usize::try_from(len).map_err(|_| bad())?];
        file.read_exact(&mut data).map_err(|_| bad())?;
        Ok(data)
    };
    let le = |data: &[u8], offset: usize, width: usize| -> Result<u64, UpdateFailure> {
        let bytes = data.get(offset..offset + width).ok_or_else(bad)?;
        Ok(bytes
            .iter()
            .rev()
            .fold(0u64, |value, byte| (value << 8) | u64::from(*byte)))
    };
    let length = file.metadata().map_err(|_| bad())?.len();
    let tail_size = length.min(65_536 + 22);
    let tail = at(&mut file, length - tail_size, tail_size)?;
    let end = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|offset| tail[*offset..].starts_with(&[0x50, 0x4b, 0x05, 0x06]))
        .ok_or_else(bad)?;
    let (mut count, mut size, mut start) = (
        le(&tail, end + 10, 2)?,
        le(&tail, end + 12, 4)?,
        le(&tail, end + 16, 4)?,
    );
    if count == 0xffff || size == 0xffff_ffff || start == 0xffff_ffff {
        let locator = end.checked_sub(20).ok_or_else(bad)?;
        if !tail[locator..].starts_with(&[0x50, 0x4b, 0x06, 0x07]) {
            return Err(bad());
        }
        let record = at(&mut file, le(&tail, locator + 8, 8)?, 56)?;
        if !record.starts_with(&[0x50, 0x4b, 0x06, 0x06]) {
            return Err(bad());
        }
        (count, size, start) = (
            le(&record, 32, 8)?,
            le(&record, 40, 8)?,
            le(&record, 48, 8)?,
        );
    }
    if count > 100_000
        || size > 64 * 1024 * 1024
        || start.checked_add(size).ok_or_else(bad)? > length
    {
        return Err(bad());
    }
    let directory = at(&mut file, start, size)?;
    let mut offset = 0;
    for _ in 0..count {
        if !directory
            .get(offset..)
            .ok_or_else(bad)?
            .starts_with(&[0x50, 0x4b, 0x01, 0x02])
        {
            return Err(bad());
        }
        let method = le(&directory, offset + 10, 2)?;
        let mut compressed = le(&directory, offset + 20, 4)?;
        let mut uncompressed = le(&directory, offset + 24, 4)?;
        let name_len = usize::try_from(le(&directory, offset + 28, 2)?).map_err(|_| bad())?;
        let extra_len = usize::try_from(le(&directory, offset + 30, 2)?).map_err(|_| bad())?;
        let comment_len = usize::try_from(le(&directory, offset + 32, 2)?).map_err(|_| bad())?;
        let mut local = le(&directory, offset + 42, 4)?;
        let name = directory
            .get(offset + 46..offset + 46 + name_len)
            .ok_or_else(bad)?;
        let extra = directory
            .get(offset + 46 + name_len..offset + 46 + name_len + extra_len)
            .ok_or_else(bad)?;
        let mut cursor = 0;
        while cursor + 4 <= extra.len() {
            let id = le(extra, cursor, 2)?;
            let len = usize::try_from(le(extra, cursor + 2, 2)?).map_err(|_| bad())?;
            if id == 1 {
                let body = extra.get(cursor + 4..cursor + 4 + len).ok_or_else(bad)?;
                let mut field = 0;
                for value in [&mut uncompressed, &mut compressed, &mut local] {
                    if *value == 0xffff_ffff {
                        *value = le(body, field, 8)?;
                        field += 8;
                    }
                }
            }
            cursor += 4 + len;
        }
        offset += 46 + name_len + extra_len + comment_len;
        if name != member.as_bytes() {
            continue;
        }
        if compressed > cap || uncompressed > cap {
            return Err(bad());
        }
        let header = at(&mut file, local, 30)?;
        if !header.starts_with(&[0x50, 0x4b, 0x03, 0x04]) {
            return Err(bad());
        }
        let skip = le(&header, 26, 2)? + le(&header, 28, 2)?;
        let data = at(&mut file, local + 30 + skip, compressed)?;
        return match method {
            0 => Ok(Some(data)),
            8 => {
                let mut out = Vec::new();
                flate2::read::DeflateDecoder::new(&data[..])
                    .take(cap)
                    .read_to_end(&mut out)
                    .map_err(|_| bad())?;
                Ok(Some(out))
            }
            _ => Err(bad()),
        };
    }
    Ok(None)
}

/// The manifest `Identity` of an MSIX's bytes (Python `msix_info`); `signature_failed`
/// when it is missing, oversized or malformed.
pub(super) fn manifest_identity(
    manifest: Option<&[u8]>,
) -> Result<HashMap<String, String>, UpdateFailure> {
    let refused = || UpdateFailure::new("signature_failed");
    let manifest = manifest.ok_or_else(refused)?;
    if manifest.len() > 1024 * 1024 {
        return Err(refused());
    }
    let text = std::str::from_utf8(manifest).map_err(|_| refused())?;
    xml::appx_identity(text).ok_or_else(refused)
}

/// Python `msix_info(path)` for a downloaded package.
fn msix_info(package: &Path) -> Result<HashMap<String, String>, UpdateFailure> {
    let manifest = local_zip_member(package, "AppxManifest.xml", 1024 * 1024)
        .map_err(|_| UpdateFailure::new("signature_failed"))?;
    manifest_identity(manifest.as_deref())
}

/// Python `remote_msix_identity`: the published package's Identity from its manifest
/// alone (HTTP ranges); `None` when unavailable. It only decides whether a newer
/// version exists: installing still verifies the whole package.
async fn remote_msix_identity(
    cx: &Context,
    url: &str,
    timeout: Duration,
) -> Option<HashMap<String, String>> {
    let manifest = shared::remote_zip_member(cx, url, "AppxManifest.xml", timeout)
        .await
        .ok()?;
    manifest_identity(manifest.as_deref()).ok()
}

/// Python `add_appx_package`: install the verified MSIX. The wrapper always exits 0
/// and reports on stdout, which stays private: a rejection for running apps is
/// `quit_first`, any other deployment failure `update_failed`.
async fn add_appx_package(cx: &Context, package: &Path, timeout: u64) -> Result<(), UpdateFailure> {
    // Windows can retain a package resource or start an app-owned process after the
    // close scan. The target-only shutdown closes that race without stopping other
    // packages that merely share a framework dependency.
    let script = format!(
        "$ErrorActionPreference='Stop'; try {{ Add-AppxPackage -ForceTargetApplicationShutdown -Path {}; Write-Output 'CCS_ADDAPPX_OK' }} catch {{ Write-Output ('CCS_ADDAPPX_FAILED: ' + $_.Exception.ToString()) }}",
        ps_quote(&package.to_string_lossy())
    );
    let text = shared::powershell(cx, &script, Duration::from_secs(timeout), None, false).await?;
    if text.split_whitespace().any(|word| word == "CCS_ADDAPPX_OK") {
        return Ok(());
    }
    let lowered = text.to_lowercase();
    if lowered.contains("ccs_addappx_failed")
        && (lowered.contains("0x80073d02") || lowered.contains("need to be closed"))
    {
        return Err(UpdateFailure::new("quit_first"));
    }
    Err(UpdateFailure::failed())
}

/// Python `codex_store_version`: the Store build the Codex app itself offers, or `None`
/// when the feed is unreadable or names another package.
pub(super) async fn codex_store_version(cx: &Context, install: &Installed) -> Option<String> {
    let value = download_json(
        cx,
        CODEX_STORE_FEED,
        "windows-store-update.json",
        65_536,
        30,
    )
    .await
    .ok()?;
    if value.get("packageIdentity").and_then(Value::as_str) != install.identity.as_deref()
        || install.identity.is_none()
        || value.get("storeProductId").and_then(Value::as_str) != Some(CODEX_STORE_ID)
    {
        return None;
    }
    value
        .get("buildVersion")
        .and_then(Value::as_str)
        .and_then(version_text)
}

fn four_part(version: &str) -> bool {
    let parts: Vec<&str> = version.split('.').collect();
    parts.len() == 4
        && parts
            .iter()
            .all(|part| (1..=9).contains(&part.len()) && part.bytes().all(|b| b.is_ascii_digit()))
}

/// Python `codex_release_package(architecture)`: (version, package URL) from OpenAI's
/// release feed, or `None`. Only a URL directly in that version's own release folder counts.
pub(super) fn release_package(value: &Value, architecture: &str) -> Option<(String, String)> {
    let schema = value.get("schemaVersion")?;
    if schema.as_f64() != Some(1.0) {
        return None;
    }
    let packages = value.get("packages")?.as_array()?;
    let version = value.get("packageVersion")?.as_str()?;
    if !four_part(version) {
        return None;
    }
    let folder = format!("{CODEX_RELEASE_PREFIX}{version}/");
    for package in packages.iter().filter_map(Value::as_object) {
        let Some(url) = package.get("downloadUrl").and_then(Value::as_str) else {
            continue;
        };
        let named = match package.get("architecture").and_then(Value::as_str) {
            Some("x86_64") => Some("x64"),
            other => other,
        };
        if named == Some(architecture)
            && let Some(name) = url.strip_prefix(&folder)
            && let Some(stem) = name.strip_suffix(".msix")
            && !stem.is_empty()
            && stem
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        {
            return Some((version.to_owned(), url.to_owned()));
        }
    }
    None
}

async fn codex_release_package(cx: &Context, architecture: &str) -> Option<(String, String)> {
    let value = download_json(cx, CODEX_RELEASE_FEED, "windows-release.json", 65_536, 30)
        .await
        .ok()?;
    release_package(&value, architecture)
}

/// Python `process_package(item)`: the package full name of a running process, or
/// `None` when it has no package identity (read per instance: a WindowsApps executable
/// started by path can be packaged too).
#[cfg(windows)]
#[allow(unsafe_code, reason = "GetPackageFullName is a plain Win32 call")]
fn process_package(item: &Process) -> Result<Option<String>, UpdateFailure> {
    use windows_sys::Win32::{
        Foundation::{CloseHandle, HANDLE},
        System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION},
    };
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn GetPackageFullName(process: HANDLE, length: *mut u32, name: *mut u16) -> i32;
    }
    const APPMODEL_ERROR_NO_PACKAGE: i32 = 15700;
    let context = || UpdateFailure::new("restart_context");
    // SAFETY: plain Win32 calls on a handle this function owns and closes.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, item.pid);
        if handle.is_null() {
            return Err(context());
        }
        // PACKAGE_FULL_NAME_MAX_LENGTH and its terminator.
        let mut length: u32 = 128;
        let mut name = vec![0u16; 128];
        let status = GetPackageFullName(handle, &mut length, name.as_mut_ptr());
        CloseHandle(handle);
        match status {
            0 => {
                let end = name
                    .iter()
                    .position(|unit| *unit == 0)
                    .unwrap_or(name.len());
                Ok(Some(String::from_utf16_lossy(&name[..end])))
            }
            APPMODEL_ERROR_NO_PACKAGE => Ok(None),
            _ => Err(context()),
        }
    }
}

#[cfg(not(windows))]
fn process_package(_item: &Process) -> Result<Option<String>, UpdateFailure> {
    Err(UpdateFailure::new("restart_context"))
}

fn windows_explorer() -> PathBuf {
    std::env::var_os("WINDIR")
        .filter(|value| !value.is_empty())
        .map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from)
        .join("explorer.exe")
}

/// How one closed instance reopens (Python's plan tuple).
#[derive(Clone, Debug)]
pub(super) struct Plan {
    item: Process,
    /// `<PackageFamilyName>!<AppId>`: the Start menu's reopen of a packaged instance.
    aumid: Option<String>,
    arguments: Vec<String>,
    /// The captured environment a direct reopen replays.
    launch: Option<Env>,
}

/// Python `windows_capture`: the running main instances and how to reopen each
/// exactly as it was started. Read-only. An instance in another session, of another
/// package, one whose AUMID cannot be built and one it cannot inspect are
/// `quit_first`: nothing may close for it.
async fn capture(
    cx: &Context,
    install: &Installed,
) -> Result<(Vec<Process>, Vec<Plan>), UpdateFailure> {
    let captured = async {
        let contexts = processes::main_contexts(cx, install).await?;
        if contexts.is_empty() {
            return Ok((contexts, Vec::new()));
        }
        let (identity, _, application) = package_names(install.app_id);
        let session =
            processes::own_session().ok_or_else(|| UpdateFailure::new("restart_context"))?;
        let mut plans = Vec::new();
        let mut family_name: Option<Option<String>> = None;
        for original in &contexts {
            let mut item = original.clone();
            if item.session != Some(session) {
                return Err(UpdateFailure::new("quit_first"));
            }
            let name = process_package(&item)?;
            // Another package's instance: its start cannot be reproduced.
            if let Some(name) = &name
                && !name.starts_with(&format!("{identity}_"))
            {
                return Err(UpdateFailure::new("quit_first"));
            }
            let (_, relative) = desktop_data_folder(&item, Platform::Windows);
            if relative {
                // A relative data directory only reopens from the folder the app ran
                // in, so that folder is read before the arguments are planned.
                let (cwd, env) = processes::windows_context(cx, item.pid).await?;
                item.cwd = Some(cwd);
                item.env = env;
            }
            let (arguments, reproducible) = desktop_profile(&item, Platform::Windows);
            if !reproducible {
                return Err(UpdateFailure::new("quit_first"));
            }
            if !arguments.is_empty() || name.is_none() {
                // By path: the launcher's start for a profile, or a plain start of an
                // unpackaged app. Its environment is part of that start.
                let launch = if relative {
                    item.env.clone()
                } else {
                    processes::windows_context(cx, item.pid).await?.1
                };
                plans.push(Plan {
                    item,
                    aumid: None,
                    arguments,
                    launch: Some(launch),
                });
                continue;
            }
            if family_name.is_none() {
                family_name = Some(
                    package(cx, install.app_id, true)
                        .await
                        .filter(|(found, _)| found.path == install.path)
                        .and_then(|(_, family)| family),
                );
            }
            match family_name.as_ref().and_then(Option::as_deref) {
                Some(family) if windows_explorer().is_file() => plans.push(Plan {
                    item,
                    aumid: Some(format!("{family}!{application}")),
                    arguments: Vec::new(),
                    launch: None,
                }),
                _ => return Err(UpdateFailure::new("quit_first")),
            }
        }
        Ok((contexts, plans))
    };
    captured.await.map_err(|failure| {
        if failure.code == "restart_context" {
            UpdateFailure::new("quit_first")
        } else {
            failure
        }
    })
}

fn spawn_detached(mut command: std::process::Command) -> Result<(), UpdateFailure> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP
        command.creation_flags(0x0000_0008 | 0x0000_0200);
    }
    command
        .spawn()
        .map(drop)
        .map_err(|_| UpdateFailure::new("restart_failed"))
}

/// Python `reopen_windows`: start each planned instance from this install, then wait
/// for its main processes to show.
async fn reopen(cx: &Context, install: &Installed, plans: &[Plan]) -> Result<u32, UpdateFailure> {
    for plan in plans {
        if let Some(aumid) = &plan.aumid {
            // explorer.exe exits 1 even after it opened the app: only the scan decides.
            let mut command = std::process::Command::new(windows_explorer());
            command.arg(format!("shell:AppsFolder\\{aumid}"));
            spawn_detached(command)?;
        } else {
            // The same direct start as AAC's Claude launcher: current exe, its folder,
            // one data directory and the captured environment.
            let mut command = std::process::Command::new(&install.path);
            command
                .args(&plan.arguments)
                .env_clear()
                .envs(shared::captured_launch_environment(
                    plan.launch.as_deref().unwrap_or_default(),
                ));
            if let Some(folder) = install.path.parent() {
                command.current_dir(folder);
            }
            spawn_detached(command)?;
        }
    }
    if plans.is_empty() {
        return Ok(0);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(WINDOWS_REOPEN_SECONDS);
    loop {
        let table = processes::scan(cx).await?;
        let existing = family(&cx.host, install, &table, true);
        let pids: std::collections::HashSet<u32> = existing.iter().map(|item| item.pid).collect();
        if existing
            .iter()
            .filter(|item| !pids.contains(&item.ppid))
            .count()
            >= plans.len()
        {
            return Ok(u32::try_from(plans.len()).unwrap_or(u32::MAX));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(UpdateFailure::new("restart_failed"));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Python `require_replace_time`: never close an app the remaining time cannot
/// install and reopen.
fn require_replace_time(cx: &Context) -> Result<(), UpdateFailure> {
    if cx.remaining() < Duration::from_secs(RESERVE + WINDOWS_REPLACE_SECONDS) {
        return Err(UpdateFailure::new("timeout"));
    }
    Ok(())
}

/// Python `windows_replace`: install a newer package; a running app is closed first
/// and reopened as each instance was started. Instances are captured right before
/// closing and every reopen is planned before anything closes. A failed close or
/// install reopens what was closed from the package registered now; a failed reopen
/// after a good install is `restart_failed`.
async fn replace(cx: &Context, install: &Installed, package_file: &Path, after: &str) -> AppRow {
    let before = install.version.as_deref();
    let (contexts, plans) = match capture(cx, install).await {
        Ok(captured) => captured,
        Err(failure) if failure.code == "quit_first" => return quit_first(install),
        Err(failure) => return failed(install, failure.code),
    };
    let mut forced = 0;
    if !contexts.is_empty() {
        match processes::terminate_desktops(cx, install, &contexts, Duration::from_secs(15), || {
            require_replace_time(cx)
        })
        .await
        {
            Ok(count) => forced = count,
            Err(failure) => {
                // Never leave a closed instance closed: reopen what already ended
                // from the untouched package.
                let live = processes::live_contexts(cx, &contexts).await;
                let closed: Vec<Plan> = plans
                    .iter()
                    .filter(|plan| !live.iter().any(|item| same_process(item, &plan.item)))
                    .cloned()
                    .collect();
                let _ = reopen(cx, install, &closed).await;
                return failed(install, failure.code);
            }
        }
    }
    let mut refreshed: Option<Installed> = None;
    let deployed = async {
        let seconds = desktop_budget(cx, 600, RESERVE)?;
        add_appx_package(cx, package_file, seconds).await?;
        refreshed = package(cx, install.app_id, false)
            .await
            .map(|(found, _)| found);
        match &refreshed {
            Some(found)
                if !found.path.as_os_str().is_empty()
                    && found.version.as_deref() == Some(after) =>
            {
                Ok(())
            }
            _ => Err(UpdateFailure::new("version_unknown")),
        }
    }
    .await;
    let refreshed = refreshed.filter(|found| !found.path.as_os_str().is_empty());
    if let Err(failure) = deployed {
        // Reopen from whatever package is registered now: the old one when the install failed.
        let reopened = reopen(cx, refreshed.as_ref().unwrap_or(install), &plans)
            .await
            .unwrap_or(0);
        // A real deployment hold, not a preflight that closed nothing.
        let (status, code) = if failure.code == "quit_first" {
            (Status::ActionRequired, "package_in_use")
        } else {
            (Status::Failed, failure.code)
        };
        let mut row = result(install, status, before, before, Some(code), true, reopened);
        row.forced_stops = Some(forced);
        return row;
    }
    let refreshed = refreshed.expect("checked above");
    let restarted = match reopen(cx, &refreshed, &plans).await {
        Ok(restarted) => restarted,
        Err(_) => {
            return result(
                install,
                Status::RestartFailed,
                before,
                refreshed.version.as_deref(),
                Some("restart_failed"),
                true,
                0,
            );
        }
    };
    let mut row = result(
        install,
        Status::Updated,
        before,
        refreshed.version.as_deref(),
        (!plans.is_empty()).then_some("desktop_reopened"),
        true,
        restarted,
    );
    row.forced_stops = Some(forced);
    if !plans.is_empty() {
        row.restart_targets = Some(vec![json!({"kind": "desktop"})]);
    }
    row
}

/// The package architecture this computer installs (Python reads `PROCESSOR_ARCHITECTURE`).
fn architecture() -> &'static str {
    if std::env::var("PROCESSOR_ARCHITECTURE")
        .is_ok_and(|value| value.eq_ignore_ascii_case("ARM64"))
    {
        "arm64"
    } else {
        "x64"
    }
}

/// Python `update_windows`: the published manifest alone (HTTP ranges) says whether a
/// newer version exists, so a current app reports current, and a running app that
/// could not be reopened reports `quit_first`, before any download. The download and
/// its verification finish while the app keeps running; [`replace`] then closes,
/// installs and reopens.
pub(super) async fn update(cx: &Context, install: &Installed) -> AppRow {
    let before = install.version.as_deref();
    let architecture = architecture();
    let codex = install.app_id == "codex-desktop";
    let mut url = if codex {
        format!("https://persistent.oaistatic.com/codex-app-prod/ChatGPT-{architecture}.msix")
    } else {
        format!("https://claude.ai/api/desktop/win32/{architecture}/msix/latest/redirect")
    };
    let published = |info: &HashMap<String, String>| -> Option<String> {
        let arch = info
            .get("ProcessorArchitecture")
            .map(|value| value.to_lowercase())
            .unwrap_or_default();
        (info.get("Name").map(String::as_str) == install.identity.as_deref()
            && install.identity.is_some()
            && info.get("Publisher").map(String::as_str) == install.publisher.as_deref()
            && install.publisher.is_some()
            && (arch == architecture || arch == "neutral"))
            .then(|| info.get("Version").and_then(|value| version_text(value)))
            .flatten()
    };
    let unknown = || {
        result(
            install,
            Status::Unknown,
            before,
            before,
            Some("readiness_unknown"),
            false,
            0,
        )
    };
    // The version the package must name: only Codex's release feed pins one.
    let mut expected = None;
    if codex {
        let store = codex_store_version(cx, install).await;
        let release = codex_release_package(cx, architecture).await;
        match (&release, &store) {
            (Some((version, package_url)), _)
                if version_tuple(Some(version)) > version_tuple(before) =>
            {
                // The newer version's own Store-signed package: downloaded while Codex
                // runs, then closed, installed and reopened like any MSIX.
                expected = Some(version.clone());
                url = package_url.clone();
            }
            (None, None) => return unknown(),
            // The release feed could not establish the actionable version: that is
            // not evidence the app is current.
            (None, Some(store)) if version_tuple(Some(store)) > version_tuple(before) => {
                return unknown();
            }
            // The release feed answered and names nothing newer: an ahead Store
            // build is rollout telemetry, not an action for this computer. Or neither
            // feed names a newer build: the app says the same.
            _ => return current(install),
        }
    }
    let checked = async {
        let timeout = desktop_budget(cx, REMOTE_MANIFEST_SECONDS, RESERVE)?;
        let remote = remote_msix_identity(cx, &url, Duration::from_secs(timeout)).await;
        let available = remote.as_ref().and_then(published);
        if let (Some(expected), Some(available)) = (&expected, &available)
            && available != expected
        {
            // A package that contradicts its own release feed is never installed or called current.
            return Ok(Some(failed(install, "package_mismatch")));
        }
        if let Some(available) = &available
            && version_tuple(Some(available)) <= version_tuple(before)
        {
            return Ok(Some(current(install)));
        }
        // An instance that cannot be reopened answers before minutes of download.
        capture(cx, install).await?;
        Ok::<_, UpdateFailure>(None)
    }
    .await;
    match checked {
        Ok(Some(row)) => return row,
        Ok(None) => {}
        Err(failure) if failure.code == "quit_first" => return quit_first(install),
        Err(failure) => return failed(install, failure.code),
    }
    let temporary = match shared::private_temporary(cx) {
        Ok(temporary) => temporary,
        Err(failure) => return failed(install, failure.code),
    };
    let package_file = temporary.path().join("update.msix");
    let fetched = async {
        cx.phase("downloading");
        download_desktop(cx, &url, &package_file, RESERVE + WINDOWS_REPLACE_SECONDS).await?;
        cx.phase("updating");
        let after = published(&msix_info(&package_file)?)
            .ok_or_else(|| UpdateFailure::new("signature_failed"))?;
        if expected.as_ref().is_some_and(|expected| *expected != after) {
            return Err(UpdateFailure::new("package_mismatch"));
        }
        Ok(after)
    }
    .await;
    let after = match fetched {
        Ok(after) if version_tuple(Some(&after)) <= version_tuple(before) => {
            return current(install);
        }
        Ok(after) => after,
        Err(failure) if failure.code == "download_blocked" => {
            return result(
                install,
                Status::ActionRequired,
                before,
                before,
                Some("check_in_app"),
                false,
                0,
            );
        }
        Err(failure) => return failed(install, failure.code),
    };
    // Add-AppxPackage verifies the Store/publisher signature and upgrades the same
    // per-user package, preserving LocalState.
    if let Err(failure) = desktop_budget(cx, 1, RESERVE).and_then(|_| require_replace_time(cx)) {
        return failed(install, failure.code);
    }
    replace(cx, install, &package_file, &after).await
}
