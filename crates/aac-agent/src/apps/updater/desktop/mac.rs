//! Mac desktop packages (Python `detect_desktop` mac, `update_mac` and helpers).
//! A running app is never quit, closed, restarted or killed: its update waits
//! until the user quits it, and that is reported (`quit_first`) before any
//! download whenever the newer version is already known.
use super::{
    RESERVE, current, desktop_budget, desktop_running, download_desktop, download_json, failed,
    quit_first, result, version_tuple, xml,
};
use crate::apps::updater::{
    AppRow, CommandSpec, Context, Fingerprint, Installed, Probe, Status, UpdateFailure,
    common::{version_text, write_private_json},
    processes, shared,
};
use serde_json::{Map, Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

/// Claude's darwin packages live on the publisher's plain-CDN release feed; the old
/// claude.ai redirect answers 403 to every non-browser client.
pub(super) const CLAUDE_DARWIN_FEED: &str =
    "https://downloads.claude.ai/releases/darwin/universal/RELEASES.json";
pub(super) const CLAUDE_DARWIN_PREFIX: &str = "https://downloads.claude.ai/releases/darwin/";
pub(super) const CODEX_DMG: &str = "https://persistent.oaistatic.com/codex-app-prod/ChatGPT.dmg";

/// Python `MAC`: (bundle name, bundle identifier, team).
pub(super) fn bundle(app_id: &str) -> (&'static str, &'static str, &'static str) {
    if app_id == "codex-desktop" {
        ("ChatGPT.app", "com.openai.codex", "2DC432GLL2")
    } else {
        ("Claude.app", "com.anthropic.claudefordesktop", "Q6L2SF6YDW")
    }
}

/// Python `bundle_info(path)`: the bundle's `Info.plist`, or an empty map.
/// XML lists are read here; a binary list goes through the system reader.
pub(super) async fn bundle_info(cx: &Context, path: &Path) -> Map<String, Value> {
    let plist = path.join("Contents/Info.plist");
    let aac_files::read::Read::Bytes(bytes, _) = aac_files::read::read(
        &plist,
        &aac_files::read::ReadPolicy::capped(4 * 1024 * 1024),
    ) else {
        return Map::new();
    };
    let value = if bytes.starts_with(b"bplist") {
        let spec = CommandSpec::new([
            std::ffi::OsString::from("/usr/bin/plutil"),
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
        cx.run(&spec)
            .await
            .ok()
            .and_then(|output| serde_json::from_str(&output.stdout).ok())
    } else {
        std::str::from_utf8(&bytes).ok().and_then(xml::plist)
    };
    match value {
        Some(Value::Object(fields)) => fields,
        _ => Map::new(),
    }
}

fn text<'a>(info: &'a Map<String, Value>, key: &str) -> Option<&'a str> {
    info.get(key).and_then(Value::as_str)
}

fn short_version(info: &Map<String, Value>) -> Option<String> {
    text(info, "CFBundleShortVersionString").and_then(version_text)
}

/// Python `detect_desktop` on the Mac.
pub(super) async fn detect(cx: &Context, app_id: &'static str) -> Option<Installed> {
    let (name, identity, _) = bundle(app_id);
    for path in [
        cx.host.system("/Applications").join(name),
        cx.host.home.join("Applications").join(name),
    ] {
        let info = bundle_info(cx, &path).await;
        let matches = text(&info, "CFBundleIdentifier") == Some(identity);
        let version = short_version(&info);
        if path.exists() && (!matches || version.is_none()) {
            let mut install = Installed::new(app_id, cx.platform, path, None, "official-download");
            install.probe = Probe::Failed;
            return Some(install);
        }
        if matches {
            let mut install = Installed::new(
                app_id,
                cx.platform,
                path.clone(),
                version,
                "official-download",
            );
            install.identity = Some(identity.to_owned());
            install.package_root = Some(path);
            return Some(install);
        }
    }
    None
}

/// Python `verify_mac(app, app_id)`: the bundle's identity and version, a deep strict
/// code signature, and the publisher's team. Any failure is `signature_failed`.
pub(super) async fn verify(
    cx: &Context,
    app: &Path,
    app_id: &str,
) -> Result<Map<String, Value>, UpdateFailure> {
    let refused = || UpdateFailure::new("signature_failed");
    let (_, identity, team) = bundle(app_id);
    let info = bundle_info(cx, app).await;
    if text(&info, "CFBundleIdentifier") != Some(identity) || short_version(&info).is_none() {
        return Err(refused());
    }
    let app = app.as_os_str().to_owned();
    cx.run(
        &CommandSpec::new([
            "/usr/bin/codesign".into(),
            "--verify".into(),
            "--deep".into(),
            "--strict".into(),
            app.clone(),
        ])
        .timeout(Duration::from_secs(60)),
    )
    .await
    .map_err(|_| refused())?;
    // codesign writes its metadata to stderr; this fixed verification reads it
    // locally and never exposes the text through any result.
    let metadata = cx
        .run(
            &CommandSpec::new([
                "/usr/bin/codesign".into(),
                "-dv".into(),
                "--verbose=4".into(),
                app,
            ])
            .timeout(Duration::from_secs(15))
            .capture()
            .errors()
            .quiet(),
        )
        .await
        .map_err(|_| refused())?
        .stdout;
    let wanted = format!("TeamIdentifier={team}");
    if metadata.len() > 65_536 || !metadata.lines().any(|line| line == wanted) {
        return Err(refused());
    }
    Ok(info)
}

/// The `*.app` bundles in `folder` whose identifier is `identity`.
async fn apps_in(cx: &Context, folder: &Path, identity: Option<&str>) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return found;
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "app"))
        .collect();
    paths.sort();
    for path in paths {
        if identity.is_some()
            && text(&bundle_info(cx, &path).await, "CFBundleIdentifier") == identity
        {
            found.push(path);
        }
    }
    found
}

/// Python `dmg_candidates`: mount the fixed publisher DMG; (matching apps, device to detach).
async fn dmg_candidates(
    cx: &Context,
    install: &Installed,
    temporary: &Path,
) -> Result<(Vec<PathBuf>, Option<String>), UpdateFailure> {
    let dmg = temporary.join("update.dmg");
    download_desktop(cx, CODEX_DMG, &dmg, RESERVE).await?;
    let mounted = cx
        .run(
            &CommandSpec::new([
                "/usr/bin/hdiutil".into(),
                "attach".into(),
                "-readonly".into(),
                "-nobrowse".into(),
                "-plist".into(),
                dmg.into_os_string(),
            ])
            .timeout(Duration::from_secs(60))
            .capture(),
        )
        .await?
        .stdout;
    let Some(Value::Object(mounted)) = xml::plist(&mounted) else {
        return Err(UpdateFailure::failed());
    };
    let entities: Vec<&Map<String, Value>> = mounted
        .get("system-entities")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_object)
                .filter(|entry| {
                    entry
                        .get("mount-point")
                        .and_then(Value::as_str)
                        .is_some_and(|point| !point.is_empty())
                })
                .collect()
        })
        .unwrap_or_default();
    let attached = entities
        .first()
        .and_then(|entry| entry.get("dev-entry"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut candidates = Vec::new();
    for entry in &entities {
        let volume = PathBuf::from(entry["mount-point"].as_str().unwrap_or_default());
        candidates.extend(apps_in(cx, &volume, install.identity.as_deref()).await);
    }
    Ok((candidates, attached))
}

/// Python `claude_release_url`: Claude's own release feed names the newer package,
/// or `None` when the installed app is current. Checking the small feed first keeps
/// a current app from downloading hundreds of megabytes and lets a running app
/// report `quit_first` before any download.
async fn claude_release_url(
    cx: &Context,
    install: &Installed,
) -> Result<Option<String>, UpdateFailure> {
    let timeout = desktop_budget(cx, 60, RESERVE)?;
    let value = download_json(cx, CLAUDE_DARWIN_FEED, "RELEASES.json", 256 * 1024, timeout).await?;
    let parsed = value
        .get("currentRelease")
        .and_then(Value::as_str)
        .and_then(version_text);
    let releases = value.get("releases").and_then(Value::as_array);
    let (Some(parsed), Some(releases)) = (parsed, releases) else {
        return Err(UpdateFailure::failed());
    };
    if version_tuple(Some(&parsed)) <= version_tuple(install.version.as_deref()) {
        return Ok(None);
    }
    for entry in releases {
        let Some(target) = entry.get("updateTo").and_then(Value::as_object) else {
            continue;
        };
        // Python `str(target.get("version") or "")`.
        let named = match target.get("version") {
            Some(Value::String(text)) => text.clone(),
            Some(Value::Number(number)) if number.as_f64() != Some(0.0) => number.to_string(),
            _ => String::new(),
        };
        if version_text(&named).as_deref() != Some(parsed.as_str()) {
            continue;
        }
        if let Some(url) = target.get("url").and_then(Value::as_str)
            && url.starts_with(CLAUDE_DARWIN_PREFIX)
        {
            return Ok(Some(url.to_owned()));
        }
    }
    Err(UpdateFailure::failed())
}

/// Python `claude_zip_candidates`: the package, expanded with ditto (it keeps
/// symlinks, resources and the code signature intact).
async fn claude_zip_candidates(
    cx: &Context,
    install: &Installed,
    temporary: &Path,
    url: &str,
) -> Result<Vec<PathBuf>, UpdateFailure> {
    let package = temporary.join("update.zip");
    download_desktop(cx, url, &package, RESERVE).await?;
    let extracted = temporary.join("extracted");
    std::fs::create_dir(&extracted).map_err(|_| UpdateFailure::failed())?;
    let seconds = desktop_budget(cx, 600, RESERVE)?;
    cx.run(
        &CommandSpec::new([
            "/usr/bin/ditto".into(),
            "-x".into(),
            "-k".into(),
            package.into_os_string(),
            extracted.clone().into_os_string(),
        ])
        .timeout(Duration::from_secs(seconds)),
    )
    .await?;
    Ok(apps_in(cx, &extracted, install.identity.as_deref()).await)
}

/// Python `package_memory(install)`: the version of the last verified Mac DMG, keyed
/// by its HEAD fingerprint.
pub(super) fn package_memory(cx: &Context, app_id: &str) -> PathBuf {
    cx.state.join(format!("{app_id}-mac-package.json"))
}

/// Python `remembered_dmg_version`: the verified version of this exact published DMG,
/// or `None` when it was never verified here. The Codex DMG has no version feed;
/// without this every check of a running Codex would download 750 MB.
pub(super) fn remembered_dmg_version(
    cx: &Context,
    install: &Installed,
    fingerprint: Option<&Fingerprint>,
) -> Option<String> {
    let fingerprint = fingerprint?;
    let value = crate::apps::read_json(&package_memory(cx, install.app_id), 64 * 1024)?;
    if value.get("url").and_then(Value::as_str) != Some(fingerprint.url.as_str())
        || value.get("size").and_then(Value::as_u64) != Some(fingerprint.size)
        || value.get("validator").and_then(Value::as_str) != Some(fingerprint.validator.as_str())
    {
        return None;
    }
    value
        .get("version")
        .and_then(Value::as_str)
        .and_then(version_text)
}

fn remember_dmg_version(
    cx: &Context,
    install: &Installed,
    fingerprint: Option<&Fingerprint>,
    version: &str,
) {
    if let Some(fingerprint) = fingerprint {
        let _ = write_private_json(
            &package_memory(cx, install.app_id),
            &json!({
                "url": fingerprint.url, "size": fingerprint.size,
                "validator": fingerprint.validator, "version": version,
            }),
        );
    }
}

/// What one Mac update left behind for its cleanup (Python's `finally`).
#[derive(Default)]
struct Swap {
    attached: Option<String>,
    stage: Option<PathBuf>,
    backup: Option<PathBuf>,
    installed: bool,
    /// The version now installed (Python `install.version` after the swap).
    version: Option<String>,
}

#[allow(
    unsafe_code,
    reason = "access(2) has no safe wrapper with the enabled nix features"
)]
fn writable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        let Ok(name) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
            return false;
        };
        // SAFETY: a valid NUL-terminated path; access() only reads it.
        unsafe { libc::access(name.as_ptr(), libc::W_OK) == 0 }
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).is_ok_and(|info| !info.permissions().readonly())
    }
}

/// Python `shutil.copytree(source, target, symlinks=True)`.
fn copy_tree(source: &Path, target: &Path) -> std::io::Result<()> {
    std::fs::create_dir(target)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = target.join(entry.file_name());
        if kind.is_symlink() {
            #[cfg(unix)]
            std::os::unix::fs::symlink(std::fs::read_link(entry.path())?, &to)?;
            #[cfg(not(unix))]
            return Err(std::io::Error::other("symlink"));
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)?;
        }
    }
    std::fs::set_permissions(target, std::fs::metadata(source)?.permissions())
}

/// Python `update_mac`: update only while the app is not running; never quit it.
/// A running app with a newer version reports `quit_first` within seconds: Claude
/// knows the newer version from its small release feed, Codex from the remembered
/// fingerprint of its last verified DMG. Only when the version is unknown does a
/// running Codex wait for the download (shown live) before it can say so.
pub(super) async fn update(cx: &Context, install: &Installed) -> AppRow {
    let running = match desktop_running(cx, install).await {
        Ok(running) => running,
        Err(failure) => return failed(install, failure.code),
    };
    let temporary = match shared::private_temporary(cx) {
        Ok(temporary) => temporary,
        Err(failure) => return failed(install, failure.code),
    };
    let mut swap = Swap {
        version: install.version.clone(),
        ..Swap::default()
    };
    let outcome = flow(cx, install, running, temporary.path(), &mut swap).await;
    if let Some(device) = &swap.attached {
        let _ = cx
            .run(
                &CommandSpec::new(["/usr/bin/hdiutil", "detach", device.as_str()])
                    .timeout(Duration::from_secs(30)),
            )
            .await;
    }
    if let Some(stage) = &swap.stage
        && stage.exists()
    {
        let _ = std::fs::remove_dir_all(stage);
    }
    if swap.installed
        && let Some(backup) = &swap.backup
        && backup.exists()
    {
        let _ = std::fs::remove_dir_all(backup);
    }
    match outcome {
        Ok(row) => row,
        Err(failure) if failure.code == "download_blocked" => {
            let version = install.version.as_deref();
            result(
                install,
                Status::ActionRequired,
                version,
                version,
                Some("check_in_app"),
                false,
                0,
            )
        }
        Err(failure) => result(
            install,
            Status::Failed,
            install.version.as_deref(),
            swap.version.as_deref(),
            Some(failure.code),
            swap.installed,
            0,
        ),
    }
}

async fn flow(
    cx: &Context,
    install: &Installed,
    running: bool,
    temporary: &Path,
    swap: &mut Swap,
) -> Result<AppRow, UpdateFailure> {
    let before = install.version.as_deref();
    let codex = install.app_id != "claude-desktop";
    let mut fingerprint = None;
    let candidates = if codex {
        fingerprint = cx
            .system
            .fingerprint(CODEX_DMG, Duration::from_secs(15))
            .await;
        let known = remembered_dmg_version(cx, install, fingerprint.as_ref());
        if let Some(known) = &known
            && version_tuple(Some(known)) <= version_tuple(before)
        {
            return Ok(current(install));
        }
        if known.is_some() && running {
            return Ok(quit_first(install));
        }
        cx.phase("downloading");
        let (candidates, attached) = dmg_candidates(cx, install, temporary).await?;
        swap.attached = attached;
        candidates
    } else {
        let Some(url) = claude_release_url(cx, install).await? else {
            return Ok(current(install));
        };
        if running {
            return Ok(quit_first(install));
        }
        cx.phase("downloading");
        claude_zip_candidates(cx, install, temporary, &url).await?
    };
    cx.phase("updating");
    let [candidate] = candidates.as_slice() else {
        return Err(UpdateFailure::new("signature_failed"));
    };
    let info = verify(cx, candidate, install.app_id).await?;
    let after = short_version(&info).ok_or_else(|| UpdateFailure::new("signature_failed"))?;
    if codex {
        remember_dmg_version(cx, install, fingerprint.as_ref(), &after);
    }
    if version_tuple(Some(&after)) <= version_tuple(before) {
        return Ok(current(install));
    }
    if running || desktop_running(cx, install).await? {
        return Ok(quit_first(install));
    }
    let parent = install
        .path
        .parent()
        .ok_or_else(UpdateFailure::failed)?
        .to_path_buf();
    if !writable(&parent) {
        return Err(UpdateFailure::new("unsupported"));
    }
    let stage = parent.join(format!(
        ".CCS-update-stage-{}.app",
        processes::unique_suffix()
    ));
    let backup = parent.join(format!(
        ".CCS-update-backup-{}.app",
        processes::unique_suffix()
    ));
    swap.stage = Some(stage.clone());
    swap.backup = Some(backup.clone());
    copy_tree(candidate, &stage).map_err(|_| UpdateFailure::failed())?;
    verify(cx, &stage, install.app_id).await?;
    // The app may have been opened while the package downloaded: look again right
    // before the swap and never replace it under a user.
    if desktop_running(cx, install).await? {
        return Ok(quit_first(install));
    }
    desktop_budget(cx, 1, RESERVE)?;
    std::fs::rename(&install.path, &backup).map_err(|_| UpdateFailure::failed())?;
    if std::fs::rename(&stage, &install.path).is_err() {
        let _ = std::fs::rename(&backup, &install.path);
        return Err(UpdateFailure::failed());
    }
    swap.installed = true;
    swap.version = Some(after.clone());
    if let Err(failure) = verify(cx, &install.path, install.app_id).await {
        let _ = std::fs::rename(&install.path, &stage);
        let _ = std::fs::rename(&backup, &install.path);
        swap.version = install.version.clone();
        swap.installed = false;
        return Err(failure);
    }
    let mut row = result(
        install,
        Status::Updated,
        before,
        Some(&after),
        None,
        true,
        0,
    );
    row.forced_stops = Some(0);
    Ok(row)
}
