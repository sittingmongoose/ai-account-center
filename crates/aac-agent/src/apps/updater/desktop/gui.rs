//! Short-lived, fixed LaunchAgents in the owner's GUI bootstrap session (Python
//! `app_update_macos.py`). An SSH process, even through `launchctl asuser`, can keep
//! its SSH security session; launching through `gui/<uid>` gives Keychain clients
//! the login session. No credential files or Keychain settings are read or changed.
use super::xml;
use crate::apps::updater::{
    CommandSpec, Context, UpdateFailure,
    common::{CommandOutput, NO_AUTO_UPDATE},
    processes, shared,
};
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    path::Path,
    time::{Duration, Instant},
};

/// The fixed prompt that warms Muse's sign-in in the GUI session.
pub(crate) const MUSE_WARM_PROMPT: &str = "Reply with the word: ok";

/// The only two package-owned operations a GUI LaunchAgent may run: no supplied argv or path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum GuiAction {
    MuseWarm,
    CursorUpdate,
}

impl GuiAction {
    fn word(self) -> &'static str {
        match self {
            Self::MuseWarm => "muse-warm",
            Self::CursorUpdate => "cursor-update",
        }
    }

    fn shown(self) -> &'static str {
        match self {
            Self::MuseWarm => "muse exec (Mac GUI)",
            Self::CursorUpdate => "cursor-agent update (Mac GUI)",
        }
    }

    fn argv(self, home: &Path) -> Vec<String> {
        match self {
            Self::MuseWarm => vec![
                home.join(".local/bin/muse").to_string_lossy().into_owned(),
                "exec".into(),
                "--contributor-mode".into(),
                "off".into(),
                MUSE_WARM_PROMPT.into(),
            ],
            Self::CursorUpdate => vec![
                home.join(".local/bin/cursor-agent")
                    .to_string_lossy()
                    .into_owned(),
                "update".into(),
            ],
        }
    }
}

fn uid() -> u32 {
    #[cfg(unix)]
    {
        nix::unistd::getuid().as_raw()
    }
    #[cfg(not(unix))]
    {
        0
    }
}

/// Python `_tail(path)`: the last bytes of one of the agent's private output files.
/// Over the limit the tail is kept from its first whole line: the end holds the app's
/// own refusal words, and a cut first line could lose a credential line's marker.
pub(super) fn tail(path: &Path, limit: u64) -> String {
    use std::io::{Read as _, Seek as _, SeekFrom};
    let Ok(mut file) = std::fs::File::open(path) else {
        return String::new();
    };
    let Ok(size) = file.seek(SeekFrom::End(0)) else {
        return String::new();
    };
    let start = size.saturating_sub(limit);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut data = Vec::new();
    if file.take(limit).read_to_end(&mut data).is_err() {
        return String::new();
    }
    if start > 0 {
        data = match data.iter().position(|byte| *byte == b'\n') {
            Some(cut) => data[cut + 1..].to_vec(),
            None => Vec::new(),
        };
    }
    String::from_utf8_lossy(&data).into_owned()
}

/// The first `<prefix><value>` line of `launchctl print`, with leading whitespace allowed.
fn field<'a>(output: &'a str, prefix: &str) -> Option<&'a str> {
    output.lines().find_map(|line| {
        line.trim_start_matches([' ', '\t', '\x0b', '\x0c', '\r'])
            .strip_prefix(prefix)
    })
}

/// (`state`, `last exit code`) of a `launchctl print <service>` answer.
pub(super) fn service_state(output: &str) -> (Option<String>, Option<i64>) {
    let state = field(output, "state = ")
        .filter(|value| !value.is_empty())
        .map(|value| value.trim().to_owned());
    let exited = field(output, "last exit code = ").and_then(|value| {
        let digits: String = value.chars().take_while(char::is_ascii_digit).collect();
        digits.parse().ok()
    });
    (state, exited)
}

/// The LaunchAgent property list (Python's `plistlib.dumps` dict).
pub(super) fn agent_plist(label: &str, argv: &[String], folder: &Path) -> String {
    let environment: Map<String, Value> = NO_AUTO_UPDATE
        .iter()
        .map(|(key, value)| ((*key).to_owned(), Value::from(*value)))
        .collect();
    xml::plist_xml(&json!({
        "Label": label, "ProgramArguments": argv,
        "WorkingDirectory": folder.to_string_lossy(), "RunAtLoad": true,
        "ProcessType": "Interactive", "KeepAlive": false,
        "AbandonProcessGroup": false, "ExitTimeOut": 1,
        "StandardOutPath": folder.join("stdout").to_string_lossy(),
        "StandardErrorPath": folder.join("stderr").to_string_lossy(),
        "EnvironmentVariables": environment,
    }))
}

fn launchctl(args: &[&str]) -> Vec<OsString> {
    std::iter::once("/bin/launchctl")
        .chain(args.iter().copied())
        .map(OsString::from)
        .collect()
}

/// Python `gui_command(action, timeout)`: run one fixed package operation as a
/// LaunchAgent in this user's GUI session and wait for it. A logged-out GUI is
/// `mac_gui_unavailable`; a locked Keychain `mac_keychain_locked`; another non-zero
/// exit `update_failed` with the app's own words kept in memory for classification.
pub(crate) async fn mac_gui_command(
    cx: &Context,
    action: GuiAction,
    timeout: Duration,
) -> Result<(), UpdateFailure> {
    let deadline = Instant::now() + timeout;
    let argv = action.argv(&cx.host.home);
    // launchctl calls are plumbing: the row records the app's own command instead.
    let run = async |args: &[&str],
                     maximum: u64,
                     capture: bool,
                     errors: bool|
           -> Result<CommandOutput, UpdateFailure> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(UpdateFailure::new("timeout"));
        }
        let mut spec = CommandSpec::new(launchctl(args))
            .timeout(remaining.min(Duration::from_secs(maximum)))
            .quiet();
        spec.capture = capture;
        spec.merge_errors = errors;
        cx.run(&spec).await
    };
    let domain = format!("gui/{}", uid());
    if run(&["print", &domain], 5, false, false).await.is_err() {
        return Err(UpdateFailure::new("mac_gui_unavailable"));
    }
    let label = format!(
        "com.ccs.app-updates.{}.{}",
        action.word(),
        processes::unique_suffix()
    );
    let service = format!("{domain}/{label}");
    let temporary = shared::private_temporary(cx)?;
    let folder = temporary.path();
    let plist = folder.join("agent.plist");
    std::fs::write(&plist, agent_plist(&label, &argv, folder))
        .map_err(|_| UpdateFailure::failed())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&plist, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| UpdateFailure::failed())?;
    }
    let plist_text = plist.to_string_lossy().into_owned();
    let outcome = async {
        if run(&["bootstrap", &domain, &plist_text], 5, false, false)
            .await
            .is_err()
        {
            return Err(UpdateFailure::new("mac_gui_unavailable"));
        }
        loop {
            let output = match run(&["print", &service], 5, true, false).await {
                Ok(output) => output.stdout,
                Err(error) => {
                    if error.code == "timeout" {
                        return Err(error);
                    }
                    // A vanished service alone is not evidence of logout: probe the
                    // GUI domain to tell session loss apart.
                    if let Err(domain_error) = run(&["print", &domain], 5, false, true).await {
                        if domain_error.code == "timeout" {
                            return Err(domain_error);
                        }
                        if domain_error
                            .output
                            .to_lowercase()
                            .contains("could not find domain")
                        {
                            return Err(UpdateFailure::new("mac_gui_unavailable"));
                        }
                    }
                    return Err(error);
                }
            };
            let (state, exited) = service_state(&output);
            if let (Some(state), Some(code)) = (state, exited)
                && matches!(state.as_str(), "not running" | "exited")
            {
                let shown: Vec<OsString> = argv.iter().map(OsString::from).collect();
                if code == 0 {
                    cx.record(|record| record.ran(&shown, Some(action.shown()), Some(0), None));
                    return Ok(());
                }
                // The app's own words and exit code, so a refusal (Cursor's 401, a
                // locked Keychain) is classified as on the other computers.
                let text = [
                    tail(&folder.join("stdout"), 65_536),
                    tail(&folder.join("stderr"), 65_536),
                ]
                .into_iter()
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
                cx.record(|record| record.ran(&shown, Some(action.shown()), Some(code), None));
                let lowered = text.to_lowercase();
                let word = if lowered.contains("keychain")
                    || lowered.contains("errsecinteractionnotallowed")
                {
                    "mac_keychain_locked"
                } else {
                    "update_failed"
                };
                let mut failure = UpdateFailure::new(word);
                failure.output = text;
                failure.exit_code = Some(code);
                return Err(failure);
            }
            let pause = deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_millis(200));
            tokio::time::sleep(pause).await;
        }
    }
    .await;
    // launchd owns the process group: bootout stops it and its children (a timed-out
    // Muse warm too) before the private folder goes. Also after an ambiguous bootstrap.
    let bootout = CommandSpec::new(launchctl(&["bootout", &service]))
        .timeout(Duration::from_secs(10))
        .quiet();
    if cx.run(&bootout).await.is_err() {
        // An exited or removed service has nothing left to stop. One still registered
        // means cleanup failed, and the row must say so.
        let print = CommandSpec::new(launchctl(&["print", &service]))
            .timeout(Duration::from_secs(5))
            .quiet();
        if cx.run(&print).await.is_ok() {
            return Err(UpdateFailure::new("mac_gui_cleanup_failed"));
        }
    }
    drop(temporary);
    outcome
}
