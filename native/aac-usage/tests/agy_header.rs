//! FIX-ROUND-BIG 10, on its own (its own process, so no other test's memory counts): Antigravity's
//! write-ahead-log check reads a database's 20-byte header, never the database. Linux only: peak
//! memory is read from /proc.
#![cfg(target_os = "linux")]

use aac_usage::protocol;
use serde_json::json;
use std::fs;

/// Wonder M3, Sol L5, Astra L6: a 600 MB database (sparse) is checked by its 20-byte header. Peak
/// memory stays far below its size. b09066667 read it whole into memory first.
#[test]
fn the_antigravity_header_check_reads_twenty_bytes() {
    let peak = || -> u64 {
        let status = fs::read_to_string("/proc/self/status").unwrap();
        let line = status.lines().find(|l| l.starts_with("VmHWM:")).unwrap();
        line.split_whitespace().nth(1).unwrap().parse::<u64>().unwrap()
    };
    let home = tempfile::tempdir().unwrap();
    let h = home.path();
    let db = h.join(".gemini/antigravity/conversations/big.db");
    fs::create_dir_all(db.parent().unwrap()).unwrap();
    let mut header = b"SQLite format 3\x00".to_vec();
    header.extend_from_slice(&[0x10, 0x00, 2, 2]);
    fs::write(&db, &header).unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&db)
        .unwrap()
        .set_len(600 * 1024 * 1024)
        .unwrap();
    let before = peak();
    let ask = protocol::parse_request(
        json!({"kinds": ["antigravity"], "minDateMs": 1_780_000_000_000u64, "deadlineMs": 60_000})
            .to_string()
            .as_bytes(),
    )
    .unwrap();
    let env = aac_usage::discover::Env::from([("HOME".to_string(), h.to_string_lossy().into_owned())]);
    let _ = protocol::scan(&ask, h, &env);
    let grown_kb = peak().saturating_sub(before);
    assert!(grown_kb < 200 * 1024, "peak memory grew {grown_kb} kB");
}
