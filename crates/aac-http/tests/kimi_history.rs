//! End-to-end: OMP JSONL fixtures scanned by aac-usage, loaded once as Kimi Code history.
//! Temporary homes only; never the real `~/.omp` or `~/.ccs`.
use aac_store::activity;
use aac_store::kimi_history::{self, Unpriced};
use aac_usage::discover::Env;
use aac_usage::protocol::{self, Request};
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

const NOW: i64 = 1_795_132_800_000;
const OLD: &str = "2026-10-01T15:05:00Z";
const RECENT: &str = "2026-11-18T12:04:00Z";

fn env(home: &Path) -> Env {
    Env::from([
        ("HOME".to_string(), home.to_string_lossy().into_owned()),
        ("PATH".to_string(), String::new()),
    ])
}

fn request() -> Request {
    protocol::parse_request(
        json!({
            "kinds": ["omp"],
            "minDateMs": 946_684_800_000u64,
            "deadlineMs": 60_000
        })
        .to_string()
        .as_bytes(),
    )
    .expect("scan request")
}

fn write_session(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    let body = lines
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(path, body).expect("write session");
}

fn assistant(timestamp: &str, model: &str, provider: &str, input: u64, output: u64) -> Value {
    json!({
        "type": "message",
        "timestamp": timestamp,
        "message": {
            "role": "assistant",
            "model": model,
            "provider": provider,
            "usage": {"input": input, "output": output, "cacheRead": 0, "cacheWrite": 0}
        }
    })
}

fn dump(engine: &aac_store::engine::Engine) -> Vec<String> {
    activity::dump_cells(engine)
        .expect("dump")
        .into_iter()
        .map(|(c, t, e)| {
            format!(
                "{c} {t} {} {} {} {}",
                e.r,
                e.m,
                e.i + e.o + e.cw + e.cr,
                e.h
            )
        })
        .collect()
}

#[test]
fn omp_jsonl_kimi_history_loads_once_without_double_counting_qwen() {
    let home = tempfile::tempdir().expect("fixture home");
    let h = home.path();
    write_session(
        &h.join(".omp/agent/sessions/2026-10-01T15-00-00_kimi-hist.jsonl"),
        &[
            assistant(OLD, "kimi-k3", "moonshotai", 40, 10),
            assistant(OLD, "qwen3-max", "alibaba", 80, 20),
        ],
    );
    write_session(
        &h.join(".omp/agent/sessions/2026-11-18T12-00-00_kimi-recent.jsonl"),
        &[assistant(RECENT, "kimi-for-coding", "kimi", 100, 25)],
    );
    let scan = protocol::scan(&request(), h, &env(h));
    assert!(valid_omp_scan(&scan), "scanner answer: {}", scan);

    let store = tempfile::tempdir().expect("store");
    let engine = aac_store::ops::open_engine(store.path().to_str().expect("path"), None, None)
        .expect("open engine");
    let first = kimi_history::ingest_kimi_from_omp_scan(&engine, "ubuntu", &scan, NOW, &Unpriced)
        .expect("first backfill");
    assert!(
        first.put >= 1,
        "Kimi hours are stored: put={} refused={}",
        first.put,
        first.refused.len()
    );
    kimi_history::mark_done(&engine, "ubuntu", NOW).expect("stamp");

    let cells = dump(&engine);
    assert!(
        cells
            .iter()
            .any(|row| row.contains("kimi-code") && row.contains("kimi-k3")),
        "old Kimi attributed to kimi-code: {cells:?}"
    );
    assert!(
        cells
            .iter()
            .any(|row| row.contains("kimi-code") && row.contains("kimi-for-coding")),
        "recent Kimi attributed to kimi-code: {cells:?}"
    );
    assert!(
        !cells
            .iter()
            .any(|row| row.contains("qwen3-max") && row.contains("kimi-code")),
        "Qwen is not attributed to Kimi: {cells:?}"
    );
    let kimi_tokens: u64 = activity::dump_cells(&engine)
        .expect("dump")
        .into_iter()
        .filter(|(_, _, e)| e.r == "kimi-code")
        .map(|(_, _, e)| e.i + e.o + e.cw + e.cr)
        .sum();
    assert_eq!(kimi_tokens, 175, "40+10 and 100+25, Qwen left out");

    let again = kimi_history::ingest_kimi_from_omp_scan(&engine, "ubuntu", &scan, NOW, &Unpriced)
        .expect("second backfill");
    assert_eq!(again.put, 0);
    assert_eq!(again.accepted, 0);
    let kimi_again: u64 = activity::dump_cells(&engine)
        .expect("dump")
        .into_iter()
        .filter(|(_, _, e)| e.r == "kimi-code")
        .map(|(_, _, e)| e.i + e.o + e.cw + e.cr)
        .sum();
    assert_eq!(kimi_again, 175);
}

fn valid_omp_scan(answer: &Value) -> bool {
    answer["version"].as_u64() == Some(2)
        && answer["reader"]["name"] == "aac-usage-scan"
        && answer["kinds"]
            .as_object()
            .is_some_and(|kinds| kinds.contains_key("omp"))
}
