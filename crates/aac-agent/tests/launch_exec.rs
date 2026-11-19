//! M4a launch execution parity: the verb hands the reference argv and environment to the
//! child and keeps the reference's route/result/error contract. A compiled recorder stands
//! in for systemd-run (fixture seam); the direct route spawns a harmless infrastructure
//! command. No real app is launched.
#![allow(clippy::unwrap_used)]
use serde_json::{Value, json};
use std::io::Write;
use std::process::{Command, Stdio};

fn call(binary: &str, args: Value) -> (Option<i32>, Value) {
    let request = json!({"protocol": 1, "verb": "launch", "id": "launch-exec", "deadlineMs": 20000, "args": args});
    let mut child = Command::new(binary)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all((request.to_string() + "\n").as_bytes())
        .unwrap();
    drop(child.stdin.take());
    let output = child.wait_with_output().unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let event: Value = serde_json::from_str(text.trim()).unwrap();
    (output.status.code(), event)
}

fn result_value(event: &Value) -> Value {
    assert_eq!(event["type"], "result", "{event}");
    event["value"].clone()
}

/// Compile a tiny recorder that writes its argv and environment as JSON to a fixed path
/// and exits with a fixed code: the fake systemd-run for the fixture seam.
fn compile_recorder(
    dir: &std::path::Path,
    out: &std::path::Path,
    exit_code: i32,
) -> std::path::PathBuf {
    let source = dir.join("recorder.rs");
    let exe = dir.join(if cfg!(windows) {
        "recorder.exe"
    } else {
        "recorder"
    });
    let out_literal = out
        .to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    std::fs::write(
        &source,
        format!(
            r#"
fn esc(s: &str) -> String {{
    let mut out = String::from("\"");
    for c in s.chars() {{
        match c {{
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{{:04x}}", c as u32)),
            c => out.push(c),
        }}
    }}
    out.push('"');
    out
}}
fn main() {{
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env: Vec<(String, String)> = std::env::vars().collect();
    let mut text = String::from("{{\"args\":[");
    for (i, a) in args.iter().enumerate() {{
        if i > 0 {{ text.push(','); }}
        text.push_str(&esc(a));
    }}
    text.push_str("],\"env\":[");
    for (i, (k, v)) in env.iter().enumerate() {{
        if i > 0 {{ text.push(','); }}
        text.push_str(&format!("[{{}},{{}}]", esc(k), esc(v)));
    }}
    text.push_str("],\"cwd\":");
    text.push_str(&esc(&std::env::current_dir().unwrap().to_string_lossy()));
    text.push_str("}}");
    std::fs::write("{out_literal}", text).unwrap();
    std::process::exit({exit_code});
}}
"#
        ),
    )
    .unwrap();
    let status = Command::new("rustc")
        .args(["--edition", "2021", "-O"])
        .arg(&source)
        .arg("-o")
        .arg(&exe)
        .status()
        .unwrap();
    assert!(status.success(), "recorder compile failed");
    exe
}

#[test]
fn direct_route_spawns_detached_with_the_spec_environment_and_cwd() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("direct.json");
    // A compiled marker, not an app: it records the environment and cwd it was given.
    // (A detached PowerShell exits 0 without writing under DETACHED_PROCESS, so the
    // fixture must not depend on a console.)
    let exe = compile_recorder(dir.path(), &out, 0);
    let (code, event) = call(
        env!("CARGO_BIN_EXE_aac-agent"),
        json!({"app": "codex-desktop", "exe": exe, "args": [], "cwd": dir.path(), "env": {"AAC_TEST_KEY": "fixture-value"}}),
    );
    assert_eq!(code, Some(0), "{event}");
    let value = result_value(&event);
    assert_eq!(value["route"], "direct");
    assert!(value["pid"].as_u64().is_some_and(|pid| pid > 0), "{value}");
    // The route is detached: the agent answers at spawn, the child writes when it runs.
    let mut recorded = None;
    for _ in 0..100 {
        if let Ok(content) = std::fs::read_to_string(&out) {
            recorded = serde_json::from_str::<Value>(&content).ok();
            if recorded.is_some() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let recorded = recorded.expect("the detached child wrote its marker");
    // Full replacement: the child sees exactly the spec's environment, nothing inherited.
    assert_eq!(recorded["env"], json!([["AAC_TEST_KEY", "fixture-value"]]));
    assert_eq!(recorded["args"], json!([]));
    let cwd = recorded["cwd"].as_str().unwrap();
    // current_dir and canonicalize may differ only by the verbatim \\?\ prefix.
    let plain = |p: &str| p.strip_prefix(r"\\?\").unwrap_or(p).to_string();
    assert_eq!(
        plain(cwd),
        plain(&std::fs::canonicalize(dir.path()).unwrap().to_string_lossy())
    );
}

#[test]
fn direct_route_reports_a_spawn_failure() {
    let dir = tempfile::tempdir().unwrap();
    let (code, event) = call(
        env!("CARGO_BIN_EXE_aac-agent"),
        json!({"app": "app", "exe": dir.path().join("missing-exe"), "args": []}),
    );
    assert_eq!(code, Some(1));
    assert_eq!(event["type"], "failure");
    assert_eq!(event["failure"]["cause"], "refused");
}

#[test]
fn unknown_arguments_delegate_to_the_ts_launcher() {
    let (code, event) = call(
        env!("CARGO_BIN_EXE_aac-agent"),
        json!({"app": "app", "exe": "/bin/true", "args": [], "extra": 1}),
    );
    assert_eq!(code, Some(0));
    assert_eq!(result_value(&event), json!({"fallback": "python"}));
}

#[cfg(feature = "parity-fixtures")]
#[test]
fn systemd_route_hands_the_reference_argv_and_environment_to_the_child() {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("recorded.json");
    let recorder = compile_recorder(dir.path(), &out, 0);
    let (code, event) = call(
        env!("CARGO_BIN_EXE_aac-agent"),
        json!({
            "app": "codex-app-server", "exe": "/usr/bin/codex", "args": ["app-server"],
            "cwd": "/tmp/work", "logFile": "/tmp/app-server.log",
            "env": {"HOME": "/home/u", "OPENAI_API_KEY": "sk-fixture-secret", "LD_PRELOAD": "/p.so"},
            "platform": "linux", "systemdRunPath": recorder, "pid": 4242, "suffix": "deadbeef"
        }),
    );
    assert_eq!(code, Some(0), "{event}");
    let value = result_value(&event);
    assert_eq!(value["route"], "systemd-service");
    assert_eq!(value["unit"], "aac-launch-codex-app-server-4242-deadbeef");
    let recorded: Value = serde_json::from_str(&std::fs::read_to_string(&out).unwrap()).unwrap();
    // The exact argv the reference builds for this spec (no value ever on the argv).
    let argv = recorded["args"].as_array().unwrap();
    let argv: Vec<&str> = argv.iter().map(|w| w.as_str().unwrap()).collect();
    let trampoline = concat!(
        "if [ \"$${AAC_LOADER_LD_PRELOAD+x}\" ]; then export LD_PRELOAD=\"$$AAC_LOADER_LD_PRELOAD\"; unset AAC_LOADER_LD_PRELOAD; fi; ",
        "if [ \"$${AAC_LOADER_LD_LIBRARY_PATH+x}\" ]; then export LD_LIBRARY_PATH=\"$$AAC_LOADER_LD_LIBRARY_PATH\"; unset AAC_LOADER_LD_LIBRARY_PATH; fi; ",
        "exec \"$$@\""
    );
    assert_eq!(
        argv,
        vec![
            "--user",
            "--unit=aac-launch-codex-app-server-4242-deadbeef",
            "--collect",
            "--property=KillMode=process",
            "--working-directory=/tmp/work",
            "--setenv=HOME",
            "--setenv=OPENAI_API_KEY",
            "--setenv=AAC_LOADER_LD_PRELOAD",
            "--property=StandardOutput=append:/tmp/app-server.log",
            "--property=StandardError=append:/tmp/app-server.log",
            "--",
            "/bin/sh",
            "-c",
            trampoline,
            "sh",
            "/usr/bin/codex",
            "app-server",
        ]
    );
    // The child's environment carries the values, the loader setting renamed.
    let env: Vec<(String, String)> = recorded["env"]
        .as_array()
        .unwrap()
        .iter()
        .map(|pair| {
            (
                pair[0].as_str().unwrap().to_string(),
                pair[1].as_str().unwrap().to_string(),
            )
        })
        .collect();
    for (key, want) in [
        ("HOME", "/home/u"),
        ("OPENAI_API_KEY", "sk-fixture-secret"),
        ("AAC_LOADER_LD_PRELOAD", "/p.so"),
    ] {
        assert_eq!(
            env.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str()),
            Some(want),
            "{key}"
        );
    }
    assert!(!env.iter().any(|(k, _)| k == "LD_PRELOAD"));
}

#[cfg(feature = "parity-fixtures")]
#[test]
fn systemd_route_fails_closed_on_missing_runner_and_nonzero_exit() {
    let dir = tempfile::tempdir().unwrap();
    let (code, event) = call(
        env!("CARGO_BIN_EXE_aac-agent"),
        json!({"app": "app", "exe": "/bin/true", "args": [], "platform": "linux", "systemdRunPath": null}),
    );
    assert_eq!(code, Some(1));
    assert_eq!(event["failure"]["cause"], "refused");
    assert!(
        event["failure"]["detail"]
            .as_str()
            .unwrap()
            .contains("systemd-run is unavailable")
    );

    let out = dir.path().join("recorded.json");
    let recorder = compile_recorder(dir.path(), &out, 1);
    let (code, event) = call(
        env!("CARGO_BIN_EXE_aac-agent"),
        json!({"app": "app", "exe": "/bin/true", "args": [], "platform": "linux", "systemdRunPath": recorder, "pid": 7, "suffix": "s"}),
    );
    assert_eq!(code, Some(1));
    assert_eq!(event["failure"]["cause"], "refused");
    assert!(
        event["failure"]["detail"]
            .as_str()
            .unwrap()
            .contains("systemd-run exited with code 1")
    );
}
