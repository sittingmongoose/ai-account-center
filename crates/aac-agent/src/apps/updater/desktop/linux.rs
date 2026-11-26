//! Ubuntu desktop packages through apt (Python `detect_desktop` ubuntu,
//! `apt_package_sources`, `update_linux`).
use super::{failed, result, version_tuple};
use crate::apps::updater::{
    AppRow, CommandSpec, Context, Installed, Probe, Process, Status, UpdateFailure,
    common::version_text,
    processes::{self, same_process},
    shared,
};
use std::time::Duration;

/// (apt package, executable) of each desktop app.
fn package(app_id: &str) -> (&'static str, &'static str) {
    if app_id == "codex-desktop" {
        ("chatgpt", "/usr/lib/chatgpt/ChatGPT")
    } else {
        ("claude-desktop", "/usr/lib/claude-desktop/claude-desktop")
    }
}

/// The only origin a Codex Desktop candidate may come from.
const CODEX_APT_ORIGIN: &str = "https://persistent.oaistatic.com/codex-app-prod/linux/deb ";

/// Python `detect_desktop` on Ubuntu.
pub(super) async fn detect(cx: &Context, app_id: &'static str) -> Option<Installed> {
    let (name, executable) = package(app_id);
    let path = cx.host.system(executable);
    if !path.is_file() {
        return None;
    }
    let spec = CommandSpec::new(["/usr/bin/dpkg-query", "-W", "-f=${Version}", name])
        .timeout(Duration::from_secs(20))
        .capture();
    let (version, probe, root) = match cx.run(&spec).await {
        Ok(output) => {
            let version = version_text(&output.stdout);
            let probe = if version.is_some() {
                Probe::Ok
            } else {
                Probe::Failed
            };
            (
                version,
                probe,
                path.parent().map(std::path::Path::to_path_buf),
            )
        }
        Err(failure) if failure.code == "timeout" => (None, Probe::Timeout, None),
        Err(_) => (None, Probe::Failed, None),
    };
    let mut install = Installed::new(app_id, cx.platform, path, version, "apt");
    install.identity = Some(name.to_owned());
    install.probe = probe;
    install.package_root = root;
    Some(install)
}

/// Python `apt_package_sources(policy)`: the package sources in `apt-cache policy`'s
/// version table, minus the installed status file.
pub(super) fn apt_package_sources(policy: &str) -> Vec<String> {
    policy
        .lines()
        .filter_map(|line| {
            // A source row reads "<priority> <url or path>"; a version row starts with the version.
            let fields: Vec<&str> = line.split_whitespace().collect();
            (fields.len() > 1
                && fields[0].bytes().all(|byte| byte.is_ascii_digit())
                && (fields[1].contains('/') || fields[1].contains(':'))
                && fields[1] != "/var/lib/dpkg/status")
                .then(|| fields[1].to_owned())
        })
        .collect()
}

/// The `Candidate:` version of `apt-cache policy`.
fn candidate(policy: &str) -> Option<String> {
    policy
        .lines()
        .find(|line| line.trim().starts_with("Candidate:"))
        .and_then(|line| line.split_once(':'))
        .and_then(|(_, rest)| version_text(rest))
}

/// Python's trusted-origin check over `apt-cache madison chatgpt`.
pub(super) fn trusted_codex_candidate(madison: &str, candidate: &str) -> bool {
    madison.lines().any(|line| {
        let parts: Vec<&str> = line.split('|').map(str::trim).collect();
        parts.len() == 3
            && version_text(parts[1]).as_deref() == Some(candidate)
            && parts[2].starts_with(CODEX_APT_ORIGIN)
    })
}

fn sudo(args: &[&str]) -> CommandSpec {
    let mut argv = vec!["/usr/bin/sudo", "-n"];
    argv.extend_from_slice(args);
    CommandSpec::new(argv)
}

/// Python `update_linux`: refresh package metadata, then upgrade this one already
/// installed, signed-repository package; never a full system upgrade.
pub(super) async fn update(cx: &Context, install: &Installed) -> AppRow {
    let mut contexts = Vec::new();
    match flow(cx, install, &mut contexts).await {
        Ok(row) => row,
        Err(failure) => {
            if !contexts.is_empty() {
                let live = processes::live_contexts(cx, &contexts).await;
                let closed: Vec<Process> = contexts
                    .iter()
                    .filter(|item| !live.iter().any(|other| same_process(item, other)))
                    .cloned()
                    .collect();
                let _ = processes::restart_desktops(cx, install, &closed).await;
            }
            failed(install, failure.code)
        }
    }
}

async fn flow(
    cx: &Context,
    install: &Installed,
    contexts: &mut Vec<Process>,
) -> Result<AppRow, UpdateFailure> {
    let before = install.version.as_deref();
    let identity = install.identity.as_deref().unwrap_or_default();
    cx.run(&sudo(&["/usr/bin/apt-get", "update", "-qq"]).timeout(Duration::from_secs(180)))
        .await?;
    let policy = cx
        .run(
            &CommandSpec::new(["/usr/bin/apt-cache", "policy", identity])
                .timeout(Duration::from_secs(15))
                .capture(),
        )
        .await?
        .stdout;
    let candidate = candidate(&policy).ok_or_else(|| UpdateFailure::new("version_unknown"))?;
    // An Ubuntu upgrade turns third-party sources off. Without one apt only knows the
    // installed copy, so "current" would be a false answer.
    if apt_package_sources(&policy).is_empty() {
        return Ok(result(
            install,
            Status::ActionRequired,
            before,
            before,
            Some("source_disabled"),
            false,
            0,
        ));
    }
    let codex = install.app_id == "codex-desktop";
    // Codex Desktop's own record: no other app that reaches here keeps one.
    let retry_restart = codex && {
        let marker = shared::pending_marker(&super::restart_marker(cx, install.app_id));
        shared::marker_owes_restart(&marker) || shared::marker_applies_to(&marker, before)
    };
    let not_newer = version_tuple(Some(&candidate)) <= version_tuple(before);
    if not_newer && !retry_restart {
        return Ok(result(
            install,
            Status::Current,
            before,
            before,
            None,
            false,
            0,
        ));
    }
    if codex {
        let madison = cx
            .run(
                &CommandSpec::new(["/usr/bin/apt-cache", "madison", "chatgpt"])
                    .timeout(Duration::from_secs(15))
                    .capture(),
            )
            .await?
            .stdout;
        if !trusted_codex_candidate(&madison, &candidate) {
            return Err(UpdateFailure::new("signature_failed"));
        }
        // Same bounded bridge budget as the Codex CLI (lock, idle wait, apt).
        let seconds = cx.remaining().as_secs().clamp(30, 420);
        let restart_only = retry_restart && not_newer;
        let expected = if restart_only {
            before.unwrap_or_default()
        } else {
            candidate.as_str()
        };
        return Ok(super::bridge::codex_bridge(
            cx,
            super::bridge::BridgeMode::Desktop,
            Some(expected),
            restart_only,
            seconds,
        )
        .await);
    }
    *contexts = processes::main_contexts(cx, install).await?;
    let forced =
        processes::terminate_desktops(cx, install, contexts, Duration::from_secs(15), || Ok(()))
            .await?;
    let mut upgrade = sudo(&[
        "/usr/bin/apt-get",
        "install",
        "--only-upgrade",
        "-y",
        identity,
    ])
    .timeout(Duration::from_secs(180));
    upgrade = upgrade.env("DEBIAN_FRONTEND", "noninteractive");
    cx.run(&upgrade).await?;
    let refreshed = detect(cx, install.app_id)
        .await
        .filter(|found| found.version.as_deref() != before)
        .ok_or_else(|| UpdateFailure::new("version_unknown"))?;
    let after = refreshed.version.as_deref();
    let restarted = match processes::restart_desktops(cx, &refreshed, contexts).await {
        Ok(restarted) => restarted,
        Err(_) => {
            return Ok(result(
                install,
                Status::RestartFailed,
                before,
                after,
                Some("restart_failed"),
                true,
                0,
            ));
        }
    };
    let mut row = result(
        install,
        Status::Updated,
        before,
        after,
        None,
        true,
        restarted,
    );
    row.forced_stops = Some(forced);
    Ok(row)
}
