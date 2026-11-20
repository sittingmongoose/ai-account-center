//! Kimi Code rows in OMP session logs are attributed to `kimi-code`, not OMP.
//! Synthetic homes only; never the real `~/.omp`.
use aac_usage::discover::Env;
use aac_usage::protocol::{self, Request};
use aac_usage::read::kimi;
use serde_json::{Value, json};
use std::fs;
use std::path::Path;

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
    .unwrap()
}

fn write_session(path: &Path, lines: &[Value]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let body = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n") + "\n";
    fs::write(path, body).unwrap();
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

#[test]
fn kimi_models_are_kimi_code_usage() {
    assert!(kimi::is_kimi_code_usage(Some("kimi-k3"), Some("moonshotai")));
    assert!(kimi::is_kimi_code_usage(Some("k3"), Some("kimi")));
    assert!(kimi::is_kimi_code_usage(Some("moonshotai/kimi-for-coding"), None));
    assert!(!kimi::is_kimi_code_usage(Some("qwen3-max"), Some("alibaba")));
    assert!(!kimi::is_kimi_code_usage(Some("moonshot-v1-8k"), Some("moonshotai")));
}

#[test]
fn omp_kimi_rows_are_tagged_kimi_code_and_qwen_stays_qwen() {
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    write_session(
        &h.join(".omp/agent/sessions/2026-10-01T15-00-00_kimi-hist.jsonl"),
        &[
            assistant("2026-10-01T15:05:00Z", "kimi-k3", "moonshotai", 40, 10),
            assistant("2026-10-01T15:06:00Z", "qwen3-max", "alibaba", 80, 20),
        ],
    );
    write_session(
        &h.join(".omp/agent/sessions/2026-11-18T12-00-00_kimi-recent.jsonl"),
        &[assistant("2026-11-18T12:04:00Z", "k3", "kimi", 100, 25)],
    );
    let answer = protocol::scan(&request(), h, &env(h));
    let rows = answer["rows"].as_array().expect("rows");
    let kimi: Vec<&Value> = rows
        .iter()
        .filter(|row| row["p"].as_str() == Some("kimi-code"))
        .collect();
    let qwen: Vec<&Value> = rows
        .iter()
        .filter(|row| row["m"].as_str() == Some("qwen3-max"))
        .collect();
    assert_eq!(kimi.len(), 2, "both Kimi model rows retagged: {rows:?}");
    assert!(
        kimi.iter().all(|row| row["k"] == "omp"),
        "kind stays omp so RANGE_TOOLS still sees the cells"
    );
    assert_eq!(kimi.iter().map(|row| row["i"].as_u64().unwrap_or(0)).sum::<u64>(), 140);
    assert_eq!(qwen.len(), 1);
    assert_eq!(qwen[0]["p"], "alibaba");
    assert_ne!(qwen[0]["p"], "kimi-code");
}
