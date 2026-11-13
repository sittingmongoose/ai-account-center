//! Measures run ingest on a copy of an agent-reports share (never the live share):
//! `cargo run --release --no-default-features --example runs_measure -- <share copy> <empty store dir> [projections.jsonl]`.
//! Prints pass times, sizes and the audit; optionally writes every stored run's projection for a
//! parity check against the TypeScript reader. The secret filter is TypeScript's; this tool
//! accepts every text, so use it only on copies.

use aac_store::ops;
use serde_json::{Value as Json, json};
use std::io::Write;
use std::time::Instant;

fn call(engine: &aac_store::engine::Engine, method: &str, args: Json) -> Json {
    ops::call(engine, method, &args.to_string()).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (share, store) = (&args[1], &args[2]);
    let engine = ops::open_engine(store, None, None).unwrap();
    let now = aac_store::engine::now_ms();
    let mut pass = 0;
    let started = Instant::now();
    loop {
        pass += 1;
        let t = Instant::now();
        let scan = call(&engine, "runsScan", json!({ "root": share, "full": pass == 1 }));
        let scanned = t.elapsed();
        let t = Instant::now();
        let record = call(
            &engine,
            "runsRecord",
            json!({ "files": scan["files"], "refused": scan["refused"], "threads": scan["threads"], "catalog": scan["catalog"], "nowMs": now }),
        );
        println!(
            "pass {pass}: listed {} unchanged {} read {} in {:?}; recorded {} known {} refused {} in {:?}",
            scan["listed"],
            scan["unchanged"],
            scan["files"].as_array().unwrap().len(),
            scanned,
            record["recorded"],
            record["known"],
            record["refused"],
            t.elapsed()
        );
        if !scan["more"].as_bool().unwrap() {
            break;
        }
    }
    println!("backfill {:?}", started.elapsed());
    let t = Instant::now();
    let scan = call(&engine, "runsScan", json!({ "root": share, "full": true }));
    println!(
        "idle full pass: listed {} read {} in {:?}",
        scan["listed"],
        scan["files"].as_array().unwrap().len(),
        t.elapsed()
    );
    let t = Instant::now();
    let scan = call(&engine, "runsScan", json!({ "root": share }));
    println!("idle quick pass: listed {} folders {} in {:?}", scan["listed"], scan["folders"], t.elapsed());
    let t = Instant::now();
    let mut runs = Vec::new();
    let mut cursor = Json::Null;
    loop {
        let page = call(&engine, "runsQuery", json!({ "after": cursor }));
        runs.extend(page["runs"].as_array().unwrap().iter().cloned());
        cursor = page["next"].clone();
        if cursor.is_null() {
            break;
        }
    }
    println!("query all {} runs in {:?}", runs.len(), t.elapsed());
    println!("status {}", call(&engine, "runsStatus", json!({})));
    let status = call(&engine, "status", json!({}));
    println!("redb bytes {} stats {}", status["redbBytes"], status["redbStats"]);
    println!("ledger {}", status["logs"]["ledger"]);
    println!("refused {}", call(&engine, "runsRefused", json!({})));
    println!(
        "audit {}",
        serde_json::to_string_pretty(&call(&engine, "runsAudit", json!({}))).unwrap()
    );
    if let Some(out) = args.get(3) {
        let mut file = std::fs::File::create(out).unwrap();
        for run in &runs {
            // The projection with the written model fields restored, for the parity check.
            let text = call(&engine, "runOriginal", json!({ "runId": run["doc"]["id"] }));
            let original: Json = serde_json::from_str(text.as_str().unwrap()).unwrap();
            let mut doc = run["doc"].clone();
            doc["model"] = json!({
                "id": original["model"]["id"], "provider": original["model"]["provider"],
                "harness": original["model"]["harness"], "effort": original["model"]["effort"],
                "account": doc["model"]["account"],
            });
            writeln!(file, "{}", json!({ "file": run["file"], "doc": doc })).unwrap();
        }
    }
    engine.close().unwrap();
}
