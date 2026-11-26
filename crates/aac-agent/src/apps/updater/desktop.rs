//! W2: the Codex and Claude desktop apps (Python `app_update_desktop.py`), plus two
//! helpers other lanes call: the Mac GUI LaunchAgent runner [`mac_gui_command`]
//! (`app_update_macos.py`, for Cursor and Muse) and the confirmed Codex CLI relaunch
//! [`confirmed_codex_restart`] (`app_update_confirmed_codex.py`).
//!
//! Every package is publisher-verified before anything changes. A running Mac app
//! is never quit (its update waits: `quit_first`); a running Windows app is closed,
//! updated and reopened as each instance was started; Ubuntu goes through apt.
use super::{
    AppRow, AppUpdater, BoxFuture, Context, Detection, Installed, Platform, Status, UpdateFailure,
    processes,
};
use std::{path::Path, time::Duration};

mod bridge;
mod confirmed;
mod gui;
mod linux;
mod mac;
mod windows;
mod xml;

#[cfg(test)]
mod tests;

#[allow(unused_imports, reason = "called by the Codex CLI updater (W6)")]
pub(super) use bridge::{BridgeMode, codex_bridge};
#[allow(unused_imports, reason = "called by the Codex CLI updater (W6)")]
pub(super) use confirmed::confirmed_codex_restart;
#[allow(
    unused_imports,
    reason = "called by the Cursor (W4) and Muse (W6) updaters"
)]
pub(super) use gui::{GuiAction, MUSE_WARM_PROMPT, mac_gui_command};

/// Desktop packages are hundreds of megabytes (the Codex MSIX is over 900 MB) and
/// their host links need minutes. Still bounded, still verified.
pub(super) const DESKTOP_DOWNLOAD_MAXIMUM: u64 = 2 * 1024 * 1024 * 1024;
pub(super) const DESKTOP_DOWNLOAD_TIMEOUT: u64 = 900;
const DOWNLOAD_BLOCKED_RETRIES: u32 = 3;
/// Seconds every desktop step keeps in reserve before the app's deadline.
const RESERVE: u64 = 90;

/// One desktop app's updater. Stateless: per-run state lives in the [`Context`].
pub(super) struct Desktop(&'static str);

pub(super) static CODEX_DESKTOP: Desktop = Desktop("codex-desktop");
pub(super) static CLAUDE_DESKTOP: Desktop = Desktop("claude-desktop");

impl AppUpdater for Desktop {
    fn app_id(&self) -> &'static str {
        self.0
    }

    fn detect<'a>(&'a self, cx: &'a Context) -> BoxFuture<'a, Detection> {
        Box::pin(async move {
            match detect_desktop(cx, self.0).await {
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
        Box::pin(async move { Ok(update_desktop(cx, install).await) })
    }
}

/// Python `detect_desktop(app_id, platform)`.
pub(super) async fn detect_desktop(cx: &Context, app_id: &'static str) -> Option<Installed> {
    match cx.platform {
        Platform::Mac => mac::detect(cx, app_id).await,
        Platform::Windows => windows::package(cx, app_id, false)
            .await
            .map(|(found, _)| found),
        Platform::Ubuntu => linux::detect(cx, app_id).await,
    }
}

/// Python `update_desktop(install, deadline, phase)`.
pub(super) async fn update_desktop(cx: &Context, install: &Installed) -> AppRow {
    if install.platform == Platform::Ubuntu {
        return linux::update(cx, install).await;
    }
    // Older builds quit the app, installed, then relaunched it, and left this marker
    // when the relaunch failed. Nothing reads it now (Windows reopens in the same
    // run), so it is obsolete: drop it and run the normal check.
    let _ = std::fs::remove_file(restart_marker(cx, install.app_id));
    match install.platform {
        Platform::Mac => mac::update(cx, install).await,
        _ => windows::update(cx, install).await,
    }
}

/// Python `restart_marker(install)`.
pub(super) fn restart_marker(cx: &Context, app_id: &str) -> std::path::PathBuf {
    cx.state.join(format!("{app_id}-pending-restart.json"))
}

/// Python `result(app_id, platform, status, before, after, manager, code, attempted, restarted)`.
pub(super) fn result(
    install: &Installed,
    status: Status,
    before: Option<&str>,
    after: Option<&str>,
    code: Option<&str>,
    attempted: bool,
    restarted: u32,
) -> AppRow {
    let mut row = AppRow::new(install.app_id, install.platform, status);
    row.previous_version = before.map(str::to_owned);
    row.version = after.map(str::to_owned);
    row.manager = Some(install.manager.to_owned());
    row.message_code = code.map(str::to_owned);
    row.update_attempted = attempted;
    row.restarted_processes = restarted;
    row
}

/// A `failed` row that keeps the installed version (the common failure shape).
fn failed(install: &Installed, code: &str) -> AppRow {
    let version = install.version.as_deref();
    result(
        install,
        Status::Failed,
        version,
        version,
        Some(code),
        false,
        0,
    )
}

/// Python `quit_first(install, before)`.
fn quit_first(install: &Installed) -> AppRow {
    let version = install.version.as_deref();
    result(
        install,
        Status::ActionRequired,
        version,
        version,
        Some("quit_first"),
        false,
        0,
    )
}

fn current(install: &Installed) -> AppRow {
    let version = install.version.as_deref();
    result(install, Status::Current, version, version, None, false, 0)
}

/// Python `desktop_budget(deadline, maximum, reserve)`: whole seconds left after the
/// reserve, at most `maximum`; `timeout` when nothing is left.
fn desktop_budget(cx: &Context, maximum: u64, reserve: u64) -> Result<u64, UpdateFailure> {
    let remaining = cx.remaining().as_secs();
    if remaining <= reserve {
        return Err(UpdateFailure::new("timeout"));
    }
    Ok(maximum.min(remaining - reserve))
}

/// Python `download_desktop`: a package download that tolerates flaky bot-challenge
/// refusals. Every refused attempt is `download_blocked` (callers say `check_in_app`).
async fn download_desktop(
    cx: &Context,
    url: &str,
    destination: &Path,
    reserve: u64,
) -> Result<(), UpdateFailure> {
    let mut attempt = 0;
    loop {
        let timeout = desktop_budget(cx, DESKTOP_DOWNLOAD_TIMEOUT, reserve)?;
        match cx
            .system
            .download(
                url,
                destination,
                DESKTOP_DOWNLOAD_MAXIMUM,
                Duration::from_secs(timeout),
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(failure)
                if failure.code == "download_blocked" && attempt + 1 < DOWNLOAD_BLOCKED_RETRIES =>
            {
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
            Err(failure) => return Err(failure),
        }
    }
}

/// Python `desktop_running(install)`: true while any process of the app runs.
/// Read-only: never asks it to quit.
async fn desktop_running(cx: &Context, install: &Installed) -> Result<bool, UpdateFailure> {
    let table = processes::scan(cx).await?;
    Ok(!processes::family(&cx.host, install, &table, true).is_empty())
}

/// A small JSON document fetched through [`super::System::download`] into a private folder.
async fn download_json(
    cx: &Context,
    url: &str,
    name: &str,
    maximum: u64,
    timeout: u64,
) -> Result<serde_json::Value, UpdateFailure> {
    let temporary = super::shared::private_temporary(cx)?;
    let target = temporary.path().join(name);
    cx.system
        .download(url, &target, maximum, Duration::from_secs(timeout))
        .await?;
    let text = std::fs::read_to_string(&target).map_err(|_| UpdateFailure::failed())?;
    serde_json::from_str(&text).map_err(|_| UpdateFailure::failed())
}

fn version_tuple(value: Option<&str>) -> Vec<u64> {
    super::common::version_tuple(value.unwrap_or_default())
}
