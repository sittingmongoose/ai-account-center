//! R4: the local agent's peak memory for one live-shaped scan pass (Linux, run on purpose):
//!
//! ```text
//! cargo build --release -p aac-agent
//! AAC_AGENT_BENCH_BIN=$CARGO_TARGET_DIR/release/aac-agent \
//!   cargo test -p aac-agent --test scan_memory -- --ignored --nocapture --test-threads=1
//! ```
//!
//! The home is invented: a busy computer's Claude Code projects (thousands of settled
//! transcripts, a few active ones with large assistant lines), scanned the way the dashboard
//! does it: a cold first pass, then the settle pass the dashboard sends every ten minutes
//! (settled prints packed, the next settle slot, new lines on the active transcripts). Sizes
//! follow the VM's live counts (MEM4: 3,371 Claude transcripts, the agent's peak 66-70 MiB).
//! `AAC_R4_*` variables change the shape; `AAC_R4_MAX_MIB` turns the printed peak into a check.
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]

use serde_json::{Map, Value, json};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DAY_MS: i64 = 86_400_000;

fn knob(name: &str, default: usize) -> usize {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        ms.rem_euclid(1000)
    )
}

/// One assistant response with usage, carrying `body` bytes of tool input like a real
/// transcript line (an edited file, a long answer).
fn claude_line(session: &str, n: usize, at_ms: i64, body: usize) -> String {
    let text = "x".repeat(body);
    format!(
        "{{\"type\":\"assistant\",\"timestamp\":\"{ts}\",\"sessionId\":\"{session}\",\"cwd\":\"/work/project\",\"requestId\":\"req_{session}_{n}\",\"message\":{{\"id\":\"msg_{session}_{n}\",\"type\":\"message\",\"role\":\"assistant\",\"model\":\"claude-opus-5-5\",\"content\":[{{\"type\":\"tool_use\",\"id\":\"toolu_{n}\",\"name\":\"Write\",\"input\":{{\"file_path\":\"/work/project/f{n}.rs\",\"content\":\"{text}\"}}}}],\"usage\":{{\"input_tokens\":{i},\"output_tokens\":{o},\"cache_read_input_tokens\":{c},\"cache_creation_input_tokens\":12}}}}}}\n",
        ts = iso(at_ms),
        i = 10 + n % 900,
        o = 5 + n % 400,
        c = 1000 + n % 50_000,
    )
}

fn user_line(session: &str, at_ms: i64, body: usize) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"{}\",\"sessionId\":\"{session}\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"tool_result\",\"content\":\"{}\"}}]}}}}\n",
        iso(at_ms),
        "y".repeat(body)
    )
}

fn set_mtime(path: &Path, ms: i64) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(UNIX_EPOCH + Duration::from_millis(ms as u64))
        .unwrap();
}

struct Shape {
    settled: usize,
    settled_lines: usize,
    active: usize,
    active_lines: usize,
    line_bytes: usize,
    big_line_mib: usize,
    output_bytes: usize,
}

fn session_id(i: usize) -> String {
    format!("{i:08x}-1111-4222-8333-{:012x}", i * 7919)
}

/// Writes the home; returns the active transcripts (appended to before the settle pass).
fn build_home(home: &Path, shape: &Shape, now: i64) -> Vec<(PathBuf, String)> {
    let projects = home.join(".claude/projects");
    let old = now - 5 * DAY_MS;
    for i in 0..shape.settled {
        let dir = projects.join(format!("-work-project-{}", i % 40));
        std::fs::create_dir_all(&dir).unwrap();
        let session = session_id(i);
        let path = dir.join(format!("{session}.jsonl"));
        let mut text = String::new();
        for n in 0..shape.settled_lines {
            text.push_str(&user_line(
                &session,
                old + n as i64 * 1000,
                shape.output_bytes,
            ));
            text.push_str(&claude_line(
                &session,
                n,
                old + n as i64 * 1000 + 500,
                shape.line_bytes,
            ));
        }
        std::fs::write(&path, text).unwrap();
        set_mtime(&path, old + shape.settled_lines as i64 * 1000 + 1000);
    }
    let mut active = Vec::new();
    for i in 0..shape.active {
        let dir = projects.join("-work-active");
        std::fs::create_dir_all(&dir).unwrap();
        let session = session_id(1_000_000 + i);
        let path = dir.join(format!("{session}.jsonl"));
        let mut file = std::fs::File::create(&path).unwrap();
        let start = now - 3_600_000;
        for n in 0..shape.active_lines {
            let body = if n == shape.active_lines / 2 {
                shape.big_line_mib << 20
            } else {
                shape.line_bytes
            };
            file.write_all(
                user_line(&session, start + n as i64 * 100, shape.output_bytes).as_bytes(),
            )
            .unwrap();
            file.write_all(claude_line(&session, n, start + n as i64 * 100 + 50, body).as_bytes())
                .unwrap();
        }
        active.push((path, session));
    }
    active
}

/// Runs the agent once on `frame` under `/usr/bin/time` (a clean fork origin, as `tests/agent.rs`
/// explains) and returns its answer and its peak resident memory in KiB.
fn run_agent(binary: &Path, home: &Path, frame: &[u8]) -> (Value, u64) {
    let metric = home.join("agent-maxrss.txt");
    let mut child = std::process::Command::new("/usr/bin/time")
        .arg("-o")
        .arg(&metric)
        .arg("-f")
        .arg("%M")
        .arg("--")
        .arg(binary)
        .env_clear()
        .env("HOME", home)
        .env("CCS_HOME", home.join(".ccs"))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("aac-agent under /usr/bin/time");
    let mut stdin = child.stdin.take().unwrap();
    let payload = frame.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&payload).unwrap());
    let mut stdout = Vec::new();
    child
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut stdout)
        .unwrap();
    let mut stderr = String::new();
    child
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    writer.join().unwrap();
    assert!(
        child.wait().unwrap().success(),
        "aac-agent failed: {stderr}"
    );
    let kib = std::fs::read_to_string(&metric)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let event: Value = serde_json::from_slice(&stdout).expect("one result event");
    assert_eq!(event["type"], "result", "{event}");
    (event["value"].clone(), kib)
}

fn frame(args: Value) -> Vec<u8> {
    let mut bytes = serde_json::to_vec(&json!({
        "protocol": 1, "verb": "scan-usage", "id": "r4-scan-memory", "deadlineMs": 110_000,
        "args": args,
    }))
    .unwrap();
    bytes.push(b'\n');
    bytes
}

#[test]
#[ignore = "benchmark: run on purpose with a release aac-agent (see the module comment)"]
fn a_live_shaped_claude_settle_pass_peak() {
    let shape = Shape {
        settled: knob("AAC_R4_SETTLED", 3_300),
        settled_lines: knob("AAC_R4_SETTLED_LINES", 12),
        active: knob("AAC_R4_ACTIVE", 12),
        active_lines: knob("AAC_R4_ACTIVE_LINES", 400),
        line_bytes: knob("AAC_R4_LINE_BYTES", 2_000),
        big_line_mib: knob("AAC_R4_BIG_LINE_MIB", 4),
        output_bytes: knob("AAC_R4_OUTPUT_BYTES", 4_000),
    };
    let binary = std::env::var_os("AAC_AGENT_BENCH_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_aac-agent")));
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ccs")).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let active = build_home(home.path(), &shape, now);
    let cutoff = now - DAY_MS;
    let slot = now as u64 / aac_usage::collector::SETTLED_CHECK_MS;
    let base = |prints: Value, store: Value| {
        json!({"kinds": ["claude"], "minDateMs": now - 31 * DAY_MS, "cwd": true, "deadlineMs": 100_000,
               "fingerprints": {"_store": store, "claude": prints}})
    };

    // Pass 1: cold (no checkpoint), as after an install.
    let store = json!({"settledBefore": {"claude": cutoff}, "settledSlot": slot});
    let (cold, cold_kib) = run_agent(&binary, home.path(), &frame(base(json!({}), store)));
    assert_eq!(
        cold["kinds"]["claude"]["state"], "ok",
        "{}",
        cold["kinds"]["claude"]
    );
    let prints = cold["kinds"]["claude"]["fingerprints"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    drop(cold);

    // Pass 2: the ten-minute settle pass, the dashboard's shape: settled prints packed, the rest
    // inline, the next slot (every settled file's metadata is checked again), new lines written.
    let (mut settled, mut open) = (Map::new(), Map::new());
    for (key, print) in prints {
        if aac_usage::collector::settled_print(&print, cutoff) {
            settled.insert(key, print);
        } else {
            open.insert(key, print);
        }
    }
    let settled_count = settled.len();
    let packed = aac_usage::protocol::pack_settled(&settled).expect("settled prints pack");
    drop(settled);
    for (path, session) in &active {
        let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
        for n in 0..40 {
            let at = now + n as i64 * 100;
            file.write_all(user_line(session, at, shape.output_bytes).as_bytes())
                .unwrap();
            file.write_all(claude_line(session, 10_000 + n, at + 50, shape.line_bytes).as_bytes())
                .unwrap();
        }
    }
    let store = json!({"settledBefore": {"claude": cutoff}, "settledSlot": slot + 1,
                       "settledPacked": {"claude": packed}});
    let settle_frame = frame(base(Value::Object(open), store));
    let (settle, settle_kib) = run_agent(&binary, home.path(), &settle_frame);
    let read = &settle["kinds"]["claude"];
    assert_eq!(read["state"], "ok", "{read}");
    assert_eq!(read["settledPacked"], true);
    let answer_bytes = serde_json::to_vec(&settle).unwrap().len();
    // Pass 3: the next steady pass (same slot), nothing new.
    let (_, steady_kib) = run_agent(&binary, home.path(), &settle_frame);
    let mib = |kib: u64| kib as f64 / 1024.0;
    eprintln!(
        "R4 claude scan peak (VmHWM): cold {:.1} MiB, settle {:.1} MiB, steady {:.1} MiB; \
         {settled_count} settled prints, settle frame {:.2} MiB, settle answer {:.2} MiB",
        mib(cold_kib),
        mib(settle_kib),
        mib(steady_kib),
        settle_frame.len() as f64 / 1048576.0,
        answer_bytes as f64 / 1048576.0,
    );
    if let Some(max) = std::env::var("AAC_R4_MAX_MIB")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
    {
        assert!(
            mib(settle_kib) <= max,
            "settle pass peak {:.1} MiB > {max} MiB",
            mib(settle_kib)
        );
        assert!(
            mib(steady_kib) <= max,
            "steady pass peak {:.1} MiB > {max} MiB",
            mib(steady_kib)
        );
    }
}

fn varint(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = (value & 0x7f) as u8;
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn pb_int(field: u64, value: u64, out: &mut Vec<u8>) {
    varint(field * 8, out);
    varint(value, out);
}

fn pb_bytes(field: u64, value: &[u8], out: &mut Vec<u8>) {
    varint(field * 8 + 2, out);
    varint(value.len() as u64, out);
    out.extend_from_slice(value);
}

/// One Antigravity step's metadata: a usage record (tokens, ids), the model, a timestamp, and
/// `pad` bytes of the other step detail the reader skips.
fn agy_step(i: u64, at_s: u64, pad: usize) -> Vec<u8> {
    let mut usage = Vec::new();
    pb_int(1, 1035, &mut usage);
    for (field, value) in [
        (2, 900 + i % 300),
        (3, 40 + i % 70),
        (4, 5000 + i % 9000),
        (5, 12),
    ] {
        pb_int(field, value, &mut usage);
    }
    pb_bytes(11, format!("resp-{i:08}").as_bytes(), &mut usage);
    let mut model = Vec::new();
    pb_int(1, 1035, &mut model);
    pb_bytes(12, b"Claude Sonnet 4.6", &mut model);
    let mut stamp = Vec::new();
    pb_int(1, at_s, &mut stamp);
    let mut step = Vec::new();
    pb_bytes(9, &usage, &mut step);
    pb_bytes(24, &model, &mut step);
    pb_bytes(8, &stamp, &mut step);
    pb_bytes(40, &vec![b'p'; pad], &mut step);
    step
}

#[test]
#[ignore = "benchmark: run on purpose with a release aac-agent (see the module comment)"]
fn a_live_shaped_antigravity_pass_peak() {
    // The VM's conversation databases (MEM4 follow-up, 2026-11-27): ten, the largest 27, 18,
    // 17, 11 and 10.5 MiB. Each step row carries its metadata blob and its conversation text.
    let sizes_mib: Vec<usize> = std::env::var("AAC_R4_AGY_MIB")
        .ok()
        .map(|v| v.split(',').filter_map(|s| s.parse().ok()).collect())
        .unwrap_or_else(|| vec![27, 18, 17, 11, 10, 4, 1, 1, 1, 1]);
    let pad = knob("AAC_R4_AGY_META_BYTES", 3_000);
    let content = knob("AAC_R4_AGY_TEXT_BYTES", 3_000);
    let binary = std::env::var_os("AAC_AGENT_BENCH_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_aac-agent")));
    let home = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(home.path().join(".ccs")).unwrap();
    let folder = home.path().join(".gemini/antigravity/conversations");
    std::fs::create_dir_all(&folder).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut steps_total = 0u64;
    for (n, mib) in sizes_mib.iter().enumerate() {
        let path = folder.join(format!("{}.db", session_id(n)));
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode = DELETE; BEGIN; CREATE TABLE steps (idx INTEGER, metadata BLOB, content TEXT)")
            .unwrap();
        let rows = (*mib << 20) / (pad + content + 64);
        for i in 0..rows as u64 {
            let at = (now / 1000) as u64 - 3_600 + i % 3_000;
            connection
                .execute(
                    "INSERT INTO steps VALUES (?1, ?2, ?3)",
                    rusqlite::params![
                        i as i64,
                        agy_step(steps_total + i, at, pad),
                        "c".repeat(content)
                    ],
                )
                .unwrap();
        }
        steps_total += rows as u64;
        connection.execute_batch("COMMIT").unwrap();
    }
    let args = json!({"kinds": ["antigravity"], "minDateMs": now - 31 * DAY_MS, "deadlineMs": 100_000,
                      "fingerprints": {"_store": {"settledSlot": now as u64 / aac_usage::collector::SETTLED_CHECK_MS}}});
    let (answer, kib) = run_agent(&binary, home.path(), &frame(args));
    let read = &answer["kinds"]["antigravity"];
    assert_eq!(read["state"], "ok", "{read}");
    let rows = answer["rows"].as_array().map_or(0, Vec::len);
    eprintln!(
        "R4 antigravity scan peak (VmHWM): {:.1} MiB; {} databases, {steps_total} steps, {rows} rows",
        kib as f64 / 1024.0,
        sizes_mib.len()
    );
    if let Some(max) = std::env::var("AAC_R4_MAX_MIB")
        .ok()
        .and_then(|v| v.parse::<f64>().ok())
    {
        assert!(
            kib as f64 / 1024.0 <= max,
            "antigravity pass peak {:.1} MiB > {max} MiB",
            kib as f64 / 1024.0
        );
    }
}
