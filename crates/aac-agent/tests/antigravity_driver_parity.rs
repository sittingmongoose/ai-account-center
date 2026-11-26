//! RED/GREEN parity tests for `aac-agent` Antigravity sign-in PTY driver and `"antigravity"` verb.
#![allow(clippy::unwrap_used)]

use aac_agent::signin_session::antigravity_driver::{
    DriverSpec, extract_url, menu_ready, strip_controls, url_origin,
};
use aac_agent::signin_session::process::{Command, ProcessEvent};
use serde_json::json;
use std::collections::BTreeMap;
use std::fs;
use std::path::Path;
use tokio::io::BufReader;

const ORIGIN: &[u8] = b"https://accounts.google.com";
const EXPECTED_URL: &[u8] =
    b"https://accounts.google.com/o/oauth2/auth?client_id=test&scope=email&state=xyz";

#[test]
fn extract_url_matches_osc8_and_wrapped_plain_text_on_every_prefix() {
    assert_eq!(url_origin(EXPECTED_URL), ORIGIN);
    assert_eq!(
        url_origin(b"https://accounts.google.com.evil.test/o/oauth2/auth"),
        b"https://accounts.google.com.evil.test"
    );

    // OSC 8 link
    let osc = format!(
        "\x1b]8;;{}\x07Sign in\x1b]8;;\x07",
        std::str::from_utf8(EXPECTED_URL).unwrap()
    );
    assert_eq!(
        extract_url(osc.as_bytes(), ORIGIN).as_deref(),
        Some(EXPECTED_URL)
    );

    // Plain wrapped text across 3 lines, followed by prompt line.
    // Must return None on every prefix before the trailing non-URL ended line,
    // and return the full joined URL once the ended line arrives.
    let wrapped = concat!(
        "\x1b[36mVisit:\x1b[0m https://accounts.google.com/o/oauth2/\n",
        "auth?client_id=test&scope=email\n",
        "&state=xyz\n",
        "\n",
        "Enter authorization code:\n",
    )
    .as_bytes();
    let complete_at = wrapped.windows(4).position(|w| w == b"xyz\n").unwrap() + 4 + 1; // right after the blank ended line "\n"
    for prefix_len in 0..complete_at {
        assert_eq!(
            extract_url(&wrapped[..prefix_len], ORIGIN),
            None,
            "emitted partial url at prefix {prefix_len}"
        );
    }
    assert_eq!(
        extract_url(&wrapped[..complete_at], ORIGIN).as_deref(),
        Some(EXPECTED_URL)
    );

    // Lookalike host rejected even when starting with allowed origin
    let evil = b"https://accounts.google.com.evil.test/o/oauth2/auth\n\nEnter code:\n";
    assert_eq!(extract_url(evil, ORIGIN), None);

    // Menu detection after control stripping
    let menu = strip_controls(b"\x1b[1mSelect login method\x1b[0m\r\n > 1. Google OAuth\r\n");
    assert!(menu_ready(&menu));
}

#[cfg(unix)]
#[tokio::test]
async fn pty_driver_selects_menu_emits_url_forwards_code_and_stops_on_complete_token() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let token_path = dir.path().join("antigravity-oauth-token");
    let fake_cli = dir.path().join("fake-agy.sh");
    let script = format!(
        r#"#!/bin/sh
printf '\033[1mSelect login method\033[0m\r\n > 1. Google OAuth\r\n'
IFS= read -r choice
printf 'Open this link:\r\nhttps://accounts.google.com/o/oauth2/\r\nauth?client_id=test&scope=email\r\n&state=xyz\r\n\r\nPaste code:\r\n'
IFS= read -r code
if [ "$code" = "fixture-code-42" ]; then
  umask 077
  printf '{{"auth_method":"consumer","token":{{"access_token":"acc-1","refresh_token":"ref-1","token_type":"Bearer","expiry":"2026-11-26T22:00:00Z"}}}}' > "{}"
fi
sleep 30
"#,
        token_path.display()
    );
    fs::write(&fake_cli, script).unwrap();
    fs::set_permissions(&fake_cli, fs::Permissions::from_mode(0o700)).unwrap();

    let mut env = BTreeMap::new();
    env.insert("PATH".into(), "/usr/bin:/bin".into());
    let cmd = Command {
        file: fake_cli.to_string_lossy().into_owned(),
        args: vec![],
        env,
        pty: false,
        antigravity_driver: None,
    };
    let spec = DriverSpec {
        token_path: token_path.clone(),
        origin: "https://accounts.google.com".into(),
    };
    let mut handle = aac_agent::signin_session::antigravity_driver::spawn(&cmd, &spec);
    let mut saw_url = String::new();
    let mut exit_code = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    while tokio::time::Instant::now() < deadline {
        match tokio::time::timeout(std::time::Duration::from_secs(5), handle.events.recv()).await {
            Ok(Some(ProcessEvent::Data(text))) => {
                saw_url.push_str(&text);
                if saw_url.contains("https://accounts.google.com/o/oauth2/auth?client_id=test&scope=email&state=xyz\n") {
                    assert!(handle.write("fixture-code-42\n"));
                }
            }
            Ok(Some(ProcessEvent::Exit(code, _))) => {
                exit_code = code;
                break;
            }
            _ => break,
        }
    }
    assert_eq!(
        saw_url,
        "https://accounts.google.com/o/oauth2/auth?client_id=test&scope=email&state=xyz\n"
    );
    assert_eq!(exit_code, Some(0));
}

#[tokio::test]
async fn antigravity_agent_verb_runs_runtime_health() {
    let dir = tempfile::tempdir().unwrap();
    fs::create_dir_all(dir.path().join("venv")).unwrap();
    fs::write(
        dir.path().join("venv/pyvenv.cfg"),
        "version_info = 3.13.5\n",
    )
    .unwrap();
    let req = json!({
        "protocol": 1,
        "verb": "antigravity",
        "id": "ag-health-1",
        "deadlineMs": 5000,
        "args": {
            "action": "runtime-health",
            "bundle": Path::new(dir.path()).to_string_lossy(),
            "python": "3.14",
        }
    });
    let input = format!("{req}\n");
    let mut out = Vec::new();
    let code = aac_agent::run(BufReader::new(input.as_bytes()), &mut out)
        .await
        .unwrap();
    assert_eq!(code, 0, "out={}", String::from_utf8_lossy(&out));
    let event: serde_json::Value = serde_json::from_slice(&out).unwrap();
    assert_eq!(event["type"], "result");
    assert_eq!(event["value"]["ok"], false);
    assert_eq!(event["value"]["reason"], "missing-python-module");
    assert_eq!(event["value"]["builtFor"], "3.13");
    assert_eq!(event["value"]["python"], "3.14");
}
