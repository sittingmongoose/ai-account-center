//! Peak memory of a long replay in a process holding only the store (docs/store.md 4.11).
//! Windows only (reads the process's peak working set); run with
//! cargo test --release --no-default-features --test replay_memory -- --ignored --nocapture
#[cfg(windows)]
mod common;

#[cfg(windows)]
use serde_json::json;

#[cfg(windows)]
#[test]
#[ignore]
fn replay_memory() {
    let days: i64 = std::env::var("DAYS").ok().and_then(|v| v.parse().ok()).unwrap_or(10);
    let dir = common::temp_dir("memory");
    let engine = aac_store::ops::open_engine(dir.to_str().unwrap(), None, None).unwrap();
    let start: i64 = 1_785_542_400_000;
    let min = 60_000i64;
    for minute in 0..days * 1440 {
        let t = start + minute * min;
        let mut batch = Vec::new();
        for i in 0..16i64 {
            let five = (minute % 300) / (3 + i % 4);
            let week = (minute % 10080) / (80 + i * 3);
            let w = |key: &str, used: i64, reset: i64| {
                json!({ "identity": format!("[\"{key}\",null,null]"), "key": key, "label": key, "used_pct": used as f64,
                        "remaining_pct": (100 - used) as f64, "reset_at_ms": reset, "window_minutes": 300.0 })
            };
            batch.push(json!({
                "account": { "identity": format!("{i:064x}"), "provider": "claude", "account_id": format!("claude:a{i}"), "plan": "max",
                             "platform": "mac", "source": "Claude quota on Mac", "status": "ok", "is_active": i == 0 },
                "sampled_at_ms": t, "observed_at_ms": t,
                "windows": [w("five_hour", five.min(100), start + (minute / 300 + 1) * 300 * min),
                            w("seven_day", week.min(100), start + (minute / 10080 + 1) * 10080 * min)],
            }));
        }
        aac_store::ops::call(&engine, "record", &json!({ "observations": batch }).to_string()).unwrap();
        if minute % 1440 == 1439 {
            aac_store::ops::call(&engine, "maintain", &json!({ "nowMs": t + 3 * 3_600_000 }).to_string()).unwrap();
        }
    }
    engine.close().unwrap();
    drop(engine);
    let pid = std::process::id();
    let out = std::process::Command::new("powershell")
        .args([
            "-NoProfile",
            "-Command",
            &format!("(Get-Process -Id {pid}).PeakWorkingSet64"),
        ])
        .output()
        .unwrap();
    let peak: u64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap_or(0);
    println!("days {days}: peak working set {} MiB", peak / 1_048_576);
    std::fs::remove_dir_all(dir).unwrap();
}
