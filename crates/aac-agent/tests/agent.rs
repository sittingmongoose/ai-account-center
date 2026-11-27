//! The built binary is exercised over the same bounded transport used for a remote agent.
#![allow(clippy::unwrap_used)]
use aac_config::{ComputerConfig, OperatingSystem, TransportConfig};
use aac_core::{Cause, ComputerId, sha256_hex};
use aac_protocol::{Event, Request};
use aac_transport::*;
use serde_json::json;
use std::time::Duration;

fn computer() -> Computer {
    Computer::new(
        ComputerConfig {
            id: ComputerId::new("local-fixture").unwrap(),
            label: "Fixture".into(),
            os: OperatingSystem::Linux,
            transport: TransportConfig::Local,
            roles: Vec::new(),
            metadata: Default::default(),
        },
        SshPolicy::new("/unused/known_hosts".into(), "/unused/run".into()),
    )
    .unwrap()
}

#[tokio::test]
async fn built_agent_hash_handshake_and_health_work_through_local_transport() {
    let binary = std::path::PathBuf::from(env!("CARGO_BIN_EXE_aac-agent"));
    let hash = sha256_hex(std::fs::read(&binary).unwrap());
    let artifact = AgentArtifact::load(binary, hash, aac_agent::handshake()).unwrap();
    let client = AgentClient::new(computer(), artifact);
    let request = Request {
        protocol: 1,
        verb: "health".into(),
        id: "health-fixture".into(),
        deadline_ms: 1000,
        args: json!({}),
    };
    let output = client
        .call(&request, &CallOptions::new(Duration::from_secs(5), 4096))
        .await
        .unwrap();
    assert!(
        matches!(output.last(), Some(Event::Result { value, .. }) if value["status"] == "ok" && value["observedAt"].as_u64().is_some())
    );
}

#[tokio::test]
async fn protocol_failures_are_terminal_and_never_dispatch_unknown_verbs() {
    for (payload, cause) in [
        ("{bad}\n", Cause::Damaged),
        (
            "{\"protocol\":2,\"verb\":\"health\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{}}\n",
            Cause::AgentMismatch,
        ),
        (
            "{\"protocol\":1,\"verb\":\"scan-usage\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{}}\n",
            Cause::Refused,
        ),
        (
            "{\"protocol\":1,\"verb\":\"health\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{\"path\":\"/unused\"}}\n",
            Cause::Refused,
        ),
    ] {
        let computer = computer();
        let output = computer
            .execute(
                &[env!("CARGO_BIN_EXE_aac-agent").into()],
                payload.as_bytes(),
                &CallOptions::new(Duration::from_secs(5), 4096),
            )
            .await
            .unwrap();
        assert_eq!(output.code, Some(1));
        let event: Event = serde_json::from_slice(&output.stdout).unwrap();
        assert!(matches!(event, Event::Failure { failure, .. } if failure.cause == cause));
        assert_eq!(output.stdout.iter().filter(|c| **c == b'\n').count(), 1);
    }
}

#[tokio::test]
async fn cancellation_on_open_stdin_is_a_classified_terminal_failure() {
    let input = b"{\"protocol\":1,\"verb\":\"health\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{}}\n{\"cancel\":true}\n";
    let mut output = Vec::new();
    assert_eq!(
        aac_agent::run(tokio::io::BufReader::new(&input[..]), &mut output)
            .await
            .unwrap(),
        1
    );
    assert!(
        matches!(serde_json::from_slice::<Event>(&output).unwrap(), Event::Failure { failure, .. } if failure.cause == Cause::Cancelled)
    );
}

#[tokio::test]
async fn frame_caps_and_truncated_frames_are_enforced() {
    let bytes = b"12345\n";
    let mut reader = tokio::io::BufReader::new(&bytes[..]);
    assert_eq!(
        aac_agent::read_line(&mut reader, 5)
            .await
            .unwrap_err()
            .cause,
        Cause::OutputTooLarge
    );
    let mut reader = tokio::io::BufReader::new(&b"truncated"[..]);
    assert_eq!(
        aac_agent::read_line(&mut reader, 100)
            .await
            .unwrap_err()
            .cause,
        Cause::Damaged
    );
}

#[tokio::test(start_paused = true)]
async fn idle_stdin_has_a_bounded_initial_read_deadline() {
    let (input, _keep_open) = tokio::io::duplex(128);
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(input), &mut output)
        .await
        .unwrap();
    assert_eq!(code, 1);
    assert!(
        matches!(serde_json::from_slice::<Event>(&output).unwrap(), Event::Failure { failure, .. } if failure.cause == Cause::Timeout)
    );
}

#[tokio::test]
async fn signin_fixture_is_read_only_and_reports_existing_expiry() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude");
    std::fs::create_dir(&dir).unwrap();
    let credentials = br#"{"claudeAiOauth":{"accessToken":"fixture-access","refreshToken":"fixture-refresh","expiresAt":1800007200123,"refreshTokenExpiresAt":1800108000123}}"#;
    std::fs::write(dir.join(".credentials.json"), credentials).unwrap();
    let payload = json!({"protocol":1,"verb":"signin-expiry","id":"expiry","deadlineMs":1000,"args":{"home":home.path(),"nowMs":1800000000123_i64}}).to_string()+"\n";
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output)
        .await
        .unwrap();
    assert_eq!(code, 0);
    let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(event["value"]["profiles"][0]["state"], "ok");
    assert_eq!(
        std::fs::read(dir.join(".credentials.json")).unwrap(),
        credentials
    );
    assert!(
        !String::from_utf8(output)
            .unwrap()
            .contains("fixture-access")
    );
}

/// AR5b (LIVE r34): T3 Code's settings name instances in non-ASCII and Claude Code's
/// credentials may carry floats. signin-expiry has no helper behind it, so neither may
/// decline the verb.
#[tokio::test]
async fn signin_expiry_reads_t3_settings_and_credentials_with_non_ascii_and_floats() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path().join(".claude");
    std::fs::create_dir(&dir).unwrap();
    std::fs::write(
        dir.join(".credentials.json"),
        "{\"claudeAiOauth\":{\"accessToken\":\"fixture-access\",\"refreshToken\":\"fixture-refresh\",\"expiresAt\":1800007200123,\"refreshTokenExpiresAt\":1800108000123,\"ratio\":0.5,\"label\":\"caf\u{e9}\"}}",
    )
    .unwrap();
    std::fs::create_dir_all(home.path().join(".t3/userdata")).unwrap();
    std::fs::write(
        home.path().join(".t3/userdata/settings.json"),
        "{\"ratio\": 0.5, \"providerInstances\": {\"claude_me\": {\"driver\": \"claudeAgent\", \"displayName\": \"Claude \u{b7} me\"}}}",
    )
    .unwrap();
    let payload = json!({"protocol":1,"verb":"signin-expiry","id":"expiry","deadlineMs":1000,"args":{"home":home.path(),"nowMs":1800000000123_i64}}).to_string()+"\n";
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output)
        .await
        .unwrap();
    assert_eq!(code, 0);
    let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert!(event["value"].get("fallback").is_none(), "{event}");
    assert_eq!(event["value"]["profiles"][0]["state"], "ok", "{event}");
    assert!(!event.to_string().contains("fixture-access"));
}

/// An unreadable settings.json leaf is Python's `read_failed()`: discovery is incomplete,
/// the inventory still answers. Only a denied ancestor chain (or a platform without a
/// search-capable descriptor) may decline the whole verb (qwen lane 2 recheck L1).
#[cfg(unix)]
#[tokio::test]
async fn an_unreadable_settings_leaf_is_incomplete_discovery_not_a_verb_decline() {
    use std::os::unix::fs::PermissionsExt;
    if aac_files::path::uid() == 0 {
        return; // Root bypasses the fixture's permission control.
    }
    let home = tempfile::tempdir().unwrap();
    let userdata = home.path().join(".t3/userdata");
    std::fs::create_dir_all(&userdata).unwrap();
    let settings = userdata.join("settings.json");
    std::fs::write(&settings, json!({"providerInstances":{}}).to_string()).unwrap();
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o000)).unwrap();
    let payload = json!({"protocol":1,"verb":"signin-expiry","id":"leaf","deadlineMs":5000,"args":{"home":home.path(),"nowMs":1800000000123_i64}}).to_string()+"\n";
    let mut output = Vec::new();
    let code = aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output).await;
    // Restore before asserting so the private fixture can always be removed.
    std::fs::set_permissions(&settings, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(code.unwrap(), 0);
    let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(event["type"], "result");
    let value = &event["value"];
    if aac_files::read::search_capable() {
        assert_ne!(
            value,
            &json!({"fallback":"python"}),
            "the leaf alone never declines the verb"
        );
        let profiles = value["profiles"].as_array().expect("an inventory answers");
        assert!(
            profiles
                .iter()
                .any(|p| p["name"] == "saved-profiles" && p["state"] == "unavailable"),
            "read_failed() marks discovery incomplete: {value}"
        );
    } else {
        // Without O_PATH/O_SEARCH the chain open needs read access, so a leaf denial may
        // be the chain's: production intentionally declines the whole verb there.
        assert_eq!(
            value,
            &json!({"fallback":"python"}),
            "a non-search platform declines the ambiguous denial"
        );
    }
}

#[cfg(not(feature = "parity-fixtures"))]
#[tokio::test]
async fn production_verbs_refuse_fixture_injection() {
    for (verb, args) in [
        ("signin-expiry", json!({"keychain":{}})),
        ("inventory", json!({"probe":"cores=1","platform":"linux"})),
        (
            "scan-usage",
            json!({"kinds":["codex"],"minDateMs":0,"_scanFixture":{"home":"/unused","env":{}}}),
        ),
    ] {
        let payload = json!({"protocol":1,"verb":verb,"id":"fixture","deadlineMs":1000,"args":args})
            .to_string() + "\n";
        let mut output = Vec::new();
        assert_eq!(
            aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output)
                .await
                .unwrap(),
            1
        );
        let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(event["failure"]["cause"], "refused");
    }
}

#[cfg(feature = "parity-fixtures")]
#[tokio::test]
async fn scan_uses_the_shared_library_without_changing_its_reply_bytes() {
    let home = tempfile::tempdir().unwrap();
    let args = json!({"kinds":["codex","claude","cursor","devin"],"minDateMs":0,"presence":true,"cwd":false});
    let parsed = aac_usage::protocol::parse_request(&serde_json::to_vec(&args).unwrap()).unwrap();
    let env = aac_usage::discover::env_from(std::iter::empty::<(String, String)>());
    let expected =
        serde_json::to_string(&aac_usage::protocol::scan(&parsed, home.path(), &env)).unwrap();
    let mut fixture_args = args;
    fixture_args["_scanFixture"] = json!({"home":home.path(),"env":{}});
    let input = json!({"protocol":1,"verb":"scan-usage","id":"scan-fixture","deadlineMs":5000,"args":fixture_args}).to_string() + "\n";
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_aac-agent"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut child| {
            use std::io::Write;
            child.stdin.take().unwrap().write_all(input.as_bytes())?;
            child.wait_with_output()
        })
        .unwrap();
    assert!(output.status.success());
    let bytes = String::from_utf8(output.stdout).unwrap();
    assert_eq!(
        bytes,
        format!(
            "{{\"type\":\"result\",\"protocol\":1,\"id\":\"scan-fixture\",\"value\":{expected}}}\n"
        )
    );
}

#[test]
fn aac_agent_runs_on_tuned_mimalloc() {
    let home = tempfile::tempdir().unwrap();
    let mut command = std::process::Command::new(env!("CARGO_BIN_EXE_aac-agent"));
    command
        .arg("--version")
        .env_clear()
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env("CCS_HOME", home.path().join(".ccs"))
        .env("MIMALLOC_VERBOSE", "1");
    for kept in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(kept) {
            command.env(kept, value);
        }
    }
    let output = command.output().unwrap();
    assert!(output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        // Linux prints a process-init banner; the Mac prints only the option table.
        stderr.contains("mimalloc: process init: 0x") || stderr.contains("option 'purge_delay'"),
        "aac-agent did not start on mimalloc:\n{stderr}"
    );
    let mut options = std::collections::BTreeMap::new();
    for line in stderr.lines() {
        let Some(rest) = line.trim().strip_prefix("option '") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once("':") else {
            continue;
        };
        let Some(token) = tail.split_whitespace().next() else {
            continue;
        };
        if let Ok(value) = token.parse::<i64>() {
            options.insert(name.to_owned(), value);
        }
    }
    assert_eq!(
        options.get("arena_eager_commit").copied(),
        Some(0),
        "{stderr}"
    );
    assert_eq!(options.get("purge_decommits").copied(), Some(1), "{stderr}");
    assert_eq!(options.get("purge_delay").copied(), Some(0), "{stderr}");
    let eager = options
        .get("deprecated_eager_commit")
        .or_else(|| options.get("eager_commit"))
        .copied();
    assert_eq!(eager, Some(0), "{stderr}");
}

#[cfg(target_os = "linux")]
fn run_and_measure_max_rss_kib(
    binary: &std::path::Path,
    home: &std::path::Path,
    frame: &[u8],
) -> (bool, Vec<u8>, Vec<u8>, u64) {
    use std::io::{Read, Write};

    // Linux `wait4` inherits the parent's `mm->hiwater_rss` across `fork()+execve()`, so if an
    // earlier parallel test in this harness read the 34 MiB debug binary into memory, a direct
    // `Command::spawn` + `wait4` reports the parent harness's RSS instead of `aac-agent`'s own
    // `VmHWM`. Launching through a fresh tiny wrapper (`/usr/bin/time` ~1.5 MiB or `python3`
    // `os.wait4` ~12 MiB) gives a clean `fork` origin and measures `aac-agent`'s exact `VmHWM`.
    let metric_file = home.join("agent-maxrss.txt");
    let mut cmd = if std::path::Path::new("/usr/bin/time").is_file() {
        let mut c = std::process::Command::new("/usr/bin/time");
        c.arg("-o")
            .arg(&metric_file)
            .arg("-f")
            .arg("%M")
            .arg("--")
            .arg(binary);
        c
    } else {
        let mut c = std::process::Command::new("python3");
        c.arg("-c")
            .arg(
                "import os,sys
                 p=os.fork()
                 if p==0: os.execv(sys.argv[2],[sys.argv[2]])
                 _,st,ru=os.wait4(p,0)
                 open(sys.argv[1],'w').write(str(ru.ru_maxrss))
                 sys.exit(os.waitstatus_to_exitcode(st))",
            )
            .arg(&metric_file)
            .arg(binary);
        c
    };
    let mut child = cmd
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CCS_HOME", home.join(".ccs"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let payload = frame.to_vec();
    let write_thread = std::thread::spawn(move || {
        stdin.write_all(&payload).unwrap();
    });
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_end(&mut stderr)
        .unwrap();
    write_thread.join().unwrap();
    let status = child.wait().unwrap();
    let max_rss_kib = std::fs::read_to_string(&metric_file)
        .expect("max RSS metric file")
        .trim()
        .parse::<u64>()
        .expect("valid max RSS KiB");
    (status.success(), stdout, stderr, max_rss_kib)
}

#[cfg(target_os = "linux")]
#[test]
fn scan_usage_live_shaped_settled_packed_stays_under_50_mib_vmhwm() {
    use std::io::{Read, Write};
    use std::os::unix::fs::MetadataExt;
    use std::time::{Duration, UNIX_EPOCH};

    let home = tempfile::tempdir().unwrap();
    let codex_dir = home.path().join(".codex/sessions/2026/11/14");
    std::fs::create_dir_all(&codex_dir).unwrap();
    let settled_file =
        codex_dir.join("rollout-2026-11-14T12-00-00-3e43c7f5-744b-4790-9d0b-06042ecf0a20.jsonl");
    let settled_body = concat!(
        "{\"type\":\"session_meta\",\"payload\":{\"id\":\"settled-sess\",\"model_provider\":\"openai\"}}\n",
        "{\"type\":\"turn_context\",\"payload\":{\"model\":\"gpt-6.1-sol\"}}\n",
        "{\"timestamp\":\"2026-11-14T12:00:00Z\",\"type\":\"event_msg\",\"payload\":{\"type\":\"token_count\",\"info\":{\"total_token_usage\":{\"input_tokens\":100,\"output_tokens\":10}}}}\n"
    );
    std::fs::write(&settled_file, settled_body).unwrap();
    let old_ms: i64 = 1_794_657_600_000;
    std::fs::File::options()
        .write(true)
        .open(&settled_file)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(old_ms as u64))
        .unwrap();
    let meta = std::fs::metadata(&settled_file).unwrap();
    let settled_path_key = aac_usage::util::filekey("codex", &settled_file);
    let settled_key = aac_usage::util::filekey_text("codex", "settled-sess");
    let settled_fp = aac_usage::util::fingerprint(&settled_file).unwrap();

    // Stream-generate a live-shaped ~38.6 MiB uncompressed settledPacked JSON payload (45,001
    // prints) without inflating the parent test process's RSS before spawning `aac-agent`.
    struct SettledJsonGen {
        first: String,
        idx: u32,
        total: u32,
        old_ms: i64,
        buf: Vec<u8>,
        pos: usize,
        done: bool,
    }
    impl Read for SettledJsonGen {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            while self.pos >= self.buf.len() {
                if self.done {
                    return Ok(0);
                }
                self.buf.clear();
                self.pos = 0;
                if !self.first.is_empty() {
                    self.buf.extend_from_slice(self.first.as_bytes());
                    self.first.clear();
                } else if self.idx < self.total {
                    let i = self.idx;
                    self.idx += 1;
                    let mix = |mut x: u64| -> u64 {
                        x = x.wrapping_add(0x9e37_79b9_7f4a_7c15);
                        x = (x ^ (x >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
                        x = (x ^ (x >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
                        x ^ (x >> 31)
                    };
                    let b = u64::from(i).wrapping_mul(16);
                    let (k0, k1, k2, k3) = (mix(b), mix(b + 1), mix(b + 2), mix(b + 3));
                    let (h0, h1, h2, h3) = (mix(b + 4), mix(b + 5), mix(b + 6), mix(b + 7));
                    let (t0, t1, t2, t3) = (mix(b + 8), mix(b + 9), mix(b + 10), mix(b + 11));
                    let (p0, p1, p2, p3) = (mix(b + 12), mix(b + 13), mix(b + 14), mix(b + 15));
                    let _ = write!(
                        self.buf,
                        ",\"{k0:016x}{k1:016x}{k2:016x}{k3:016x}\":{{\"newestUsageMs\":{ms},\"size\":{sz},\"mtimeMs\":{ms},\"head\":\"{h0:016x}{h1:016x}{h2:016x}{h3:016x}\",\"tail\":\"{t0:016x}{t1:016x}{t2:016x}{t3:016x}\",\"pathKey\":\"{p0:016x}{p1:016x}{p2:016x}{p3:016x}\",\"fileIdentity\":\"fd00:{i:x}\",\"settledCheckedSlot\":2991000,\"appendState\":{{\"head\":\"{h0:016x}{h1:016x}{h2:016x}{h3:016x}\",\"tail\":\"{t0:016x}{t1:016x}{t2:016x}{t3:016x}\",\"parser\":{{\"model\":\"gpt-6.1-sol\",\"provider\":\"openai\",\"previous\":[{inp}.0,{out}.0,{cr}.0,0.0],\"sessionDigest\":\"{k0:016x}\"}}}}}}",
                        ms = self.old_ms,
                        sz = 4096u64 + u64::from(i),
                        inp = 10_000 + (i % 50_000),
                        out = 500 + (i % 4_000),
                        cr = 4_000 + (i % 20_000),
                    );
                } else {
                    self.buf.push(b'}');
                    self.done = true;
                }
            }
            let n = out.len().min(self.buf.len() - self.pos);
            out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
            self.pos += n;
            Ok(n)
        }
    }

    let first_entry = format!(
        "{{\"{settled_key}\":{{\"newestUsageMs\":{old_ms},\"size\":{size},\"mtimeMs\":{mtime},\"head\":\"{head}\",\"tail\":\"{tail}\",\"pathKey\":\"{settled_path_key}\",\"fileIdentity\":\"{dev:x}:{ino:x}\",\"settledCheckedSlot\":2991000,\"appendState\":{{\"head\":\"{head}\",\"tail\":\"{tail}\",\"parser\":{{\"model\":\"gpt-6.1-sol\",\"provider\":\"openai\",\"previous\":[100.0,10.0,0.0,0.0],\"sessionDigest\":\"0123456789abcdef\"}}}}}}}}",
        size = meta.len(),
        mtime = aac_usage::util::mtime_ms(&meta),
        head = settled_fp.head,
        tail = settled_fp.tail,
        dev = meta.dev(),
        ino = meta.ino(),
    );
    // Drop the trailing `}` so subsequent generated entries append cleanly.
    let first_prefix = first_entry[..first_entry.len() - 1].to_owned();
    let packed = aac_usage::protocol::pack_settled_from_reader(SettledJsonGen {
        first: first_prefix,
        idx: 0,
        total: 35_000,
        old_ms,
        buf: Vec::with_capacity(1024),
        pos: 0,
        done: false,
    })
    .expect("pack settled fixture");

    let frame = json!({
        "protocol": 1,
        "verb": "scan-usage",
        "id": "r4-vmhwm-fixture",
        "deadlineMs": 30_000,
        "args": {
            "kinds": ["codex"],
            "minDateMs": 0,
            "cwd": false,
            "fingerprints": {
                "_store": {
                    "settledBefore": {"codex": old_ms + 86_400_000},
                    "settledSlot": 2_991_000u64,
                    "settledPacked": {"codex": packed}
                },
                "codex": {}
            }
        }
    })
    .to_string()
        + "\n";
    drop(packed);

    let frame_mib = frame.len() as f64 / (1024.0 * 1024.0);
    let binary = std::env::var_os("AAC_AGENT_BENCH_BIN")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(env!("CARGO_BIN_EXE_aac-agent")));
    let (ok, stdout, stderr, max_rss_kib) =
        run_and_measure_max_rss_kib(&binary, home.path(), frame.as_bytes());
    assert!(
        ok,
        "aac-agent failed (frame={frame_mib:.2} MiB) stderr={}",
        String::from_utf8_lossy(&stderr)
    );
    let event: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
    assert_eq!(event["type"], "result");
    assert_eq!(event["value"]["kinds"]["codex"]["state"], "ok");
    assert_eq!(event["value"]["kinds"]["codex"]["settledPacked"], true);
    assert_eq!(
        event["value"]["kinds"]["codex"]["fingerprints"][&settled_key]["settledConfirmed"],
        true
    );
    let max_rss_mib = max_rss_kib as f64 / 1024.0;
    eprintln!(
        "aac-agent live-shaped settledPacked VmHWM: {max_rss_mib:.2} MiB ({max_rss_kib} KiB), wire frame: {frame_mib:.2} MiB"
    );
    assert!(
        max_rss_kib <= 50 * 1024,
        "aac-agent VmHWM exceeded 50 MiB budget: {max_rss_mib:.2} MiB ({max_rss_kib} KiB)"
    );
}

#[tokio::test]
async fn large_history_frames_do_not_loosen_other_verb_limits() {
    for (verb, expected) in [
        ("claude-history", Cause::Refused),
        ("quota-read", Cause::OutputTooLarge),
    ] {
        let mut bytes=serde_json::to_vec(&json!({"protocol":1,"id":"history-budget","verb":verb,"deadlineMs":1000,"args":{"mode":"collect","padding":"x".repeat(aac_protocol::MAX_FRAME_BYTES)}})).expect("large synthetic envelope");
        bytes.push(b'\n');
        let mut output = Vec::new();
        assert_eq!(
            aac_agent::run(tokio::io::BufReader::new(bytes.as_slice()), &mut output)
                .await
                .expect("bounded agent"),
            1
        );
        match serde_json::from_slice::<Event>(&output).expect("failure event") {
            Event::Failure { failure, .. } => assert_eq!(failure.cause, expected, "{verb}"),
            event => panic!("unexpected event: {event:?}"),
        }
    }
}

#[tokio::test]
async fn a_failover_frame_that_fails_the_strict_input_check_is_refused_honestly() {
    // Non-ASCII text, a non-canonical number spelling and a duplicate key each fail the raw
    // check. The answer keeps the action's shape and names the reason; it never asks Python.
    for (action, args) in [
        (
            "collect",
            r#"{"version":1,"action":"collect","provider":"claude","homeDir":"/home/café"}"#,
        ),
        (
            "inventory",
            r#"{"version":1.0,"action":"inventory","provider":"codex"}"#,
        ),
        (
            "inventory",
            r#"{"version":1,"action":"inventory","provider":"claude","provider":"claude"}"#,
        ),
    ] {
        let payload = format!(
            r#"{{"protocol":1,"verb":"failover-inventory","id":"strict","deadlineMs":5000,"args":{args}}}"#
        ) + "\n";
        let mut output = Vec::new();
        let code = aac_agent::run(tokio::io::BufReader::new(payload.as_bytes()), &mut output)
            .await
            .unwrap();
        assert_eq!(code, 0);
        let event: serde_json::Value = serde_json::from_slice(&output).unwrap();
        let value = &event["value"];
        assert_ne!(value, &json!({"fallback":"python"}), "{args}");
        assert_eq!(value["action"], action, "{value}");
        assert_eq!(value["reason"], "request-not-proven", "{value}");
        if action == "collect" {
            assert_eq!(value["failureCode"], "unavailable");
            assert_eq!(value["sample"], serde_json::Value::Null);
        } else {
            assert_eq!(value["failed"], true);
            assert_eq!(value["candidates"], json!([]));
        }
    }
}

/// W6: T3's helper subcommands dispatch before agent framing (`aac-agent --t3-issuer`
/// with no parent and CLI exits 125, as W5's issuer bootstrap answers; any other
/// unknown argument would exit 2).
#[test]
fn t3_helper_subcommands_run_before_agent_framing() {
    let status = std::process::Command::new(env!("CARGO_BIN_EXE_aac-agent"))
        .arg("--t3-issuer")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .expect("agent runs");
    assert_eq!(status.code(), Some(125));
}

/// The agent exits after its terminal frame even while the parent keeps stdin open, as the
/// controlled transport does (it holds stdin for cancel frames until the child exits). Release
/// 31 dropped the runtime before `process::exit`, and the drop waited on the blocking stdin
/// read, so every controlled call ran to its deadline and failed as a timeout.
#[test]
fn the_agent_exits_after_its_terminal_frame_with_stdin_still_open() {
    use std::io::{BufRead, Write};
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_aac-agent"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"{\"protocol\":1,\"verb\":\"health\",\"id\":\"fixture\",\"deadlineMs\":1000,\"args\":{}}\n")
        .unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(
        matches!(
            serde_json::from_str::<Event>(&line).unwrap(),
            Event::Result { .. }
        ),
        "{line}"
    );
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if started.elapsed() > Duration::from_secs(3) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    drop(stdin);
    assert!(
        status.is_some(),
        "aac-agent stayed alive after its terminal frame while stdin was open"
    );
}
