//! macOS over SSH: the login Keychain belongs to the user's GUI security session, so one
//! bounded reset call runs this same agent inside launchd `gui/<uid>`
//! (`_mac_run_in_gui_session`). Only the non-secret request and reply pass through a private
//! 0700 folder; credentials never touch disk. The job is booted out and the folder removed.
use serde_json::Value;
#[cfg(target_os = "macos")]
use serde_json::json;

/// Set in the GUI child so it never bridges again.
pub(crate) const GUI_ENV: &str = "AAC_MAC_GUI_HELPER";
/// The child's argument: `--claude-reset-gui <reply path>`, request on stdin.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) const GUI_ARG: &str = "--claude-reset-gui";

pub(crate) fn needed() -> bool {
    cfg!(target_os = "macos")
        && std::env::var_os(GUI_ENV).is_none_or(|v| v != "1")
        && !super::keychain::unlocked()
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn run(_: &Value, _: &mut bool) -> Option<Value> {
    None
}

#[cfg(target_os = "macos")]
fn xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
pub(crate) fn run(request: &Value, started: &mut bool) -> Option<Value> {
    use std::{
        os::unix::fs::PermissionsExt,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };
    let exe = std::env::current_exe().ok()?;
    let exe = exe.to_str()?.to_owned();
    // SAFETY: getuid has no failure mode.
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let quiet = |cmd: &mut Command| {
        cmd.stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
    };
    let mut probe = Command::new("/bin/launchctl");
    probe.args(["print", &domain]);
    quiet(&mut probe);
    if !probe.output().ok()?.status.success() {
        return None;
    }
    let is_claim = request["action"] == "claim";
    let failed = || {
        if is_claim {
            json!({"unavailable":true,"outcome":"ambiguous"})
        } else {
            json!({"unavailable":true})
        }
    };
    let budget = if request["action"] == "capture" {
        3.0
    } else {
        10.5
    };
    let deadline = Instant::now() + Duration::from_secs_f64(budget);
    let mut nonce = [0u8; 16];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut nonce).ok()?;
    let label = format!(
        "com.ccs.reset-claims.{}",
        nonce.iter().map(|b| format!("{b:02x}")).collect::<String>()
    );
    let service = format!("{domain}/{label}");
    let dir = tempfile::Builder::new()
        .prefix("aac-reset-gui-")
        .tempdir()
        .ok()?;
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).ok()?;
    let write = |name: &str, bytes: &[u8]| -> Option<std::path::PathBuf> {
        let path = dir.path().join(name);
        std::fs::write(&path, bytes).ok()?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).ok()?;
        Some(path)
    };
    let request_path = write(
        "request.json",
        serde_json::to_string(request).ok()?.as_bytes(),
    )?;
    let out_path = dir.path().join("result.json");
    let mut env = vec![(GUI_ENV.to_owned(), "1".to_owned())];
    for name in ["HOME", "USER", "LOGNAME", "LANG"] {
        if let Ok(v) = std::env::var(name)
            && !v.is_empty()
        {
            env.push((name.into(), v));
        }
    }
    let strings = |items: &[&str]| {
        items
            .iter()
            .map(|s| format!("<string>{}</string>", xml(s)))
            .collect::<String>()
    };
    let env_xml: String = env
        .iter()
        .map(|(k, v)| format!("<key>{}</key><string>{}</string>", xml(k), xml(v)))
        .collect();
    let dir_text = dir.path().to_str()?.to_owned();
    let plist = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>Label</key><string>{label}</string>
<key>ProgramArguments</key><array>{args}</array>
<key>WorkingDirectory</key><string>{wd}</string>
<key>RunAtLoad</key><true/>
<key>ProcessType</key><string>Interactive</string>
<key>KeepAlive</key><false/>
<key>AbandonProcessGroup</key><false/>
<key>ExitTimeOut</key><integer>1</integer>
<key>StandardInPath</key><string>{stdin}</string>
<key>StandardOutPath</key><string>/dev/null</string>
<key>StandardErrorPath</key><string>/dev/null</string>
<key>EnvironmentVariables</key><dict>{env_xml}</dict>
</dict></plist>
"#,
        args = strings(&[&exe, GUI_ARG, out_path.to_str()?]),
        wd = xml(&dir_text),
        stdin = xml(request_path.to_str()?),
    );
    let plist_path = write("agent.plist", plist.as_bytes())?;
    // Once bootstrap runs for a claim, the child may reach the provider POST.
    if is_claim {
        *started = true;
    }
    let result = (|| {
        let mut boot = Command::new("/bin/launchctl");
        boot.args(["bootstrap", &domain, plist_path.to_str()?]);
        quiet(&mut boot);
        if !boot.output().ok()?.status.success() {
            return Some(failed());
        }
        while Instant::now() < deadline && !out_path.is_file() {
            let mut poll = Command::new("/bin/launchctl");
            poll.args(["print", &service]);
            quiet(&mut poll);
            let Ok(output) = poll.output() else {
                break;
            };
            if !output.status.success() {
                break;
            }
            let text = String::from_utf8_lossy(&output.stdout);
            let state = text
                .lines()
                .find_map(|l| l.trim_start().strip_prefix("state = "))
                .map(str::trim);
            let exited = text
                .lines()
                .any(|l| l.trim_start().starts_with("last exit code = "));
            if exited && matches!(state, Some("not running" | "exited")) {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let bytes = std::fs::read(&out_path).ok()?;
        (bytes.len() <= super::desktop::MAX_BYTES)
            .then(|| serde_json::from_slice::<Value>(&bytes).ok())
            .flatten()
            .filter(Value::is_object)
    })()
    .unwrap_or_else(failed);
    let mut out = Command::new("/bin/launchctl");
    out.args(["bootout", &service]);
    quiet(&mut out);
    let _ = out.output();
    drop(dir);
    Some(result)
}

/// The GUI child: read the request, answer into `path` atomically (0600).
pub(crate) async fn child(path: &std::path::Path) -> i32 {
    use tokio::io::AsyncReadExt;
    let mut bytes = vec![];
    if tokio::io::stdin()
        .take(super::desktop::MAX_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .await
        .is_err()
    {
        return 1;
    }
    let Ok(request) = serde_json::from_slice::<Value>(&bytes) else {
        return 1;
    };
    let answer = super::host::reset_host(&request).await;
    let tmp = path.with_extension("json.tmp");
    let Ok(text) = serde_json::to_vec(&answer) else {
        return 1;
    };
    if std::fs::write(&tmp, text).is_err() {
        return 1;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, path).is_err() {
        return 1;
    }
    0
}
