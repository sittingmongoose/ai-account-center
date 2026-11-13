//! Agent-report runs kept forever (Scoreboard v2 DESIGN.md sections 4 to 6): ingest from a
//! synthetic share in a temporary folder, dedupe by run id + content hash, idempotent passes,
//! backfill in passes with no cap, no pruning, rebuild from the ledger, catalog re-keying and
//! the query filters. Synthetic data only; never the real share or home.

mod common;

use aac_store::engine::Engine;
use aac_store::ops;
use aac_store::runs;
use common::temp_dir;
use redb::{ReadableDatabase, ReadableTable};
use serde_json::{Value as Json, json};
use std::fs;
use std::path::{Path, PathBuf};

const DAY: u64 = 86_400_000;
/// 2026-11-01T00:00:00Z.
const T0: u64 = 1_793_491_200_000;

fn open(dir: &Path) -> Engine {
    ops::open_engine(dir.to_str().unwrap(), None, None).unwrap()
}

fn call(engine: &Engine, method: &str, args: Json) -> Json {
    ops::call(engine, method, &args.to_string()).unwrap_or_else(|e| panic!("{method}: {e}"))
}

fn iso(ms: u64) -> String {
    let secs = ms / 1000;
    let days = (secs / 86_400) as i64;
    let (y, m, d) = aac_store::engine::civil_from_days(days);
    let rem = secs % 86_400;
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn models_json() -> String {
    json!({ "schema": "catalog-models@1", "models": [
        { "model": "anthropic/claude-opus-5-5", "label": "Opus 5.5", "family": "Claude", "runtimes": ["claude-code"], "efforts": ["high", "xhigh", "max"] },
        { "model": "openai/gpt-6.1-sol", "label": "Sol 6.1", "family": "GPT-6", "runtimes": ["codex"], "efforts": ["medium", "high", "max"] },
        { "model": "alibaba/qwen3.8-max", "label": "Qwen 3.8 Max", "family": "Qwen", "runtimes": [], "efforts": [] },
        { "model": "google/gemini-4-argon", "label": "Gemini 4 Argon", "family": "Gemini", "runtimes": ["antigravity"], "efforts": ["high"] }
    ]})
    .to_string()
}

fn aliases_json(extra: &[(&str, &str)]) -> String {
    let mut aliases = serde_json::Map::new();
    for (k, v) in [
        ("gpt-6.1-sol", "openai/gpt-6.1-sol"),
        ("claude-opus-5-5", "anthropic/claude-opus-5-5"),
        ("qwen3.8-max", "alibaba/qwen3.8-max"),
    ]
    .iter()
    .chain(extra.iter())
    {
        aliases.insert(k.to_string(), json!(v));
    }
    json!({ "schema": "agent-reports/model-aliases@1", "effortSuffixes": ["-low", "-medium", "-high", "-xhigh", "-max"], "aliases": aliases })
        .to_string()
}

/// A synthetic share root with a catalog.
struct Share {
    root: PathBuf,
}

impl Share {
    fn new(name: &str) -> Share {
        let root = temp_dir(name);
        let catalog = root.join("v1").join("catalog");
        fs::create_dir_all(&catalog).unwrap();
        fs::write(catalog.join("models.json"), models_json()).unwrap();
        fs::write(catalog.join("model-aliases.json"), aliases_json(&[])).unwrap();
        Share { root }
    }

    fn set_aliases(&self, extra: &[(&str, &str)]) {
        fs::write(self.root.join("v1/catalog/model-aliases.json"), aliases_json(extra)).unwrap();
    }

    fn thread(&self, writer: &str, t3: &str) {
        let dir = self.root.join("v1/writers").join(writer);
        fs::create_dir_all(&dir).unwrap();
        let doc = json!({ "schema": "agent-reports/thread@1", "writer": writer, "seq": 1, "writtenAt": "2026-11-01T00:00:00Z",
            "agent": { "name": "x", "harness": "codex", "machine": writer.split('.').next().unwrap(), "t3Thread": t3 } });
        fs::write(dir.join("thread.json"), doc.to_string()).unwrap();
    }

    /// Writes one run file and returns its relative path.
    #[allow(clippy::too_many_arguments)]
    fn run(
        &self,
        writer: &str,
        local: &str,
        seq: u64,
        ended: u64,
        model: &str,
        effort: Option<&str>,
        harness: Option<&str>,
        task_type: Option<&str>,
    ) -> String {
        let day = &iso(ended)[..10];
        let dir = self.root.join("v1/writers").join(writer).join("runs").join(day);
        fs::create_dir_all(&dir).unwrap();
        let id = format!("{writer}--{local}");
        let doc = json!({
            "schema": "agent-reports/run@1", "id": id, "writer": writer, "seq": seq,
            "writtenAt": iso(ended), "source": "observed", "project": "proj", "stream": null,
            "task": { "id": format!("node:delegated-task:command%3A{local}"), "type": task_type, "title": format!("Task {local}") },
            "model": { "id": model, "provider": "openai", "harness": harness, "effort": effort },
            "startedAt": iso(ended - 60_000), "endedAt": iso(ended), "wallSeconds": 60, "outcome": "done",
            "cost": null, "scope": {}, "review": null, "flags": [], "evidence": []
        });
        let text = serde_json::to_string_pretty(&doc).unwrap();
        fs::write(dir.join(format!("{id}.json")), text).unwrap();
        format!("writers/{writer}/runs/{day}/{id}.json")
    }
}

/// One ingest pass as the TypeScript service makes it (its secret filter accepts all here).
fn pass(engine: &Engine, share: &Share, now: u64, budget: usize, full: bool) -> (Json, Json) {
    let scan = call(
        engine,
        "runsScan",
        json!({ "root": share.root, "budget": budget, "full": full }),
    );
    let record = call(
        engine,
        "runsRecord",
        json!({
            "files": scan["files"], "refused": scan["refused"], "threads": scan["threads"],
            "catalog": scan["catalog"], "nowMs": now,
        }),
    );
    (scan, record)
}

fn query(engine: &Engine, args: Json) -> Vec<Json> {
    call(engine, "runsQuery", args)["runs"].as_array().unwrap().clone()
}

fn ledger_last(engine: &Engine) -> u64 {
    call(engine, "status", json!({}))["logs"]["ledger"]["lastSequence"]
        .as_u64()
        .unwrap_or(0)
}

/// Every row of the run tables, for equality checks.
fn dump(engine: &Engine) -> Vec<(String, Vec<u8>, Vec<u8>)> {
    engine
        .with_db(|db| {
            let read = db.begin_read()?;
            let mut out = Vec::new();
            for (name, def) in [
                ("run_dict", runs::RUN_DICT),
                ("run_head", runs::RUN_HEAD),
                ("run_rev", runs::RUN_REV),
                ("run_payload", runs::RUN_PAYLOAD),
                ("run_file", runs::RUN_FILE),
                ("run_catalog", runs::RUN_CATALOG),
            ] {
                for item in read.open_table(def)?.iter()? {
                    let (k, v) = item?;
                    out.push((name.to_string(), k.value().to_vec(), v.value().to_vec()));
                }
            }
            for item in read.open_table(runs::RUN_BY_TIME)?.iter()? {
                let (k, _) = item?;
                out.push(("run_by_time".into(), k.value().to_vec(), Vec::new()));
            }
            Ok(out)
        })
        .unwrap()
}

#[test]
fn a_pass_stores_every_run_with_ids_identity_and_the_original_text() {
    let share = Share::new("runs-basic");
    share.thread("vm.lead", "thread:delegated-task:command%3Amcp%3Aabc");
    let path = share.run(
        "vm.lead",
        "r1",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("X-High"),
        Some("claude-code"),
        Some("review"),
    );
    share.run("vm.lead", "r2", 1, T0 + 2 * DAY, "qwen3.8-max", None, Some("omp"), None);
    share.run(
        "windows.solo",
        "r3",
        1,
        T0 + 3 * DAY,
        "google/gemini-4-argon-high",
        None,
        None,
        Some("build"),
    );
    let store = temp_dir("runs-basic-store");
    let engine = open(&store);
    let (scan, record) = pass(&engine, &share, T0 + 10 * DAY, 100, false);
    assert_eq!(scan["files"].as_array().unwrap().len(), 3);
    assert_eq!(record["recorded"], json!(3));
    let all = query(&engine, json!({}));
    assert_eq!(all.len(), 3);
    let sol = &all[0];
    assert_eq!(sol["file"], json!(path));
    assert_eq!(sol["doc"]["model"]["id"], json!("openai/gpt-6.1-sol"));
    assert_eq!(sol["doc"]["model"]["effort"], json!("xhigh"));
    // Sol cannot run in Claude Code: corrected to its own harness, and the written one kept.
    assert_eq!(sol["identity"]["harness"], json!("codex"));
    assert_eq!(sol["identity"]["harnessSource"], json!("corrected"));
    assert_eq!(sol["identity"]["harnessRaw"], json!("claude-code"));
    assert_eq!(sol["identity"]["effortRaw"], json!("X-High"));
    assert_eq!(sol["identity"]["modelFamily"], json!("openai"));
    assert_eq!(sol["ids"]["computer"], json!("ubuntu"));
    assert_eq!(sol["ids"]["machine"], json!("vm"));
    assert_eq!(
        sol["ids"]["t3ThreadId"]["value"],
        json!("thread:delegated-task:command%3Amcp%3Aabc")
    );
    assert_eq!(sol["ids"]["t3ThreadId"]["confidence"], json!("inferred"));
    assert_eq!(
        sol["ids"]["t3TaskId"]["value"],
        json!("node:delegated-task:command%3Ar1")
    );
    assert_eq!(
        sol["ids"]["projectId"],
        json!({ "source": "agent-report", "id": "proj" })
    );
    assert_eq!(sol["ids"]["accountKey"], Json::Null);
    // The -max name is not split.
    let qwen = &all[1];
    assert_eq!(qwen["doc"]["model"]["id"], json!("alibaba/qwen3.8-max"));
    assert_eq!(qwen["doc"]["model"]["effort"], Json::Null);
    let gemini = &all[2];
    assert_eq!(gemini["doc"]["model"]["id"], json!("google/gemini-4-argon"));
    assert_eq!(gemini["doc"]["model"]["effort"], json!("high"));
    assert_eq!(gemini["identity"]["harness"], json!("antigravity"));
    assert_eq!(gemini["ids"]["computer"], json!("windows"));
    // The original document comes back byte for byte.
    let original = call(&engine, "runOriginal", json!({ "runId": "vm.lead--r1" }));
    assert_eq!(
        original.as_str().unwrap(),
        fs::read_to_string(share.root.join("v1").join(&path)).unwrap()
    );
    let status = call(&engine, "runsStatus", json!({}));
    assert_eq!(status["runs"], json!(3));
    assert!(status["catalogHash"].as_str().unwrap().len() == 64);
}

#[test]
fn ingest_is_idempotent_and_dedupes_by_run_id_and_content() {
    let share = Share::new("runs-idem");
    share.run(
        "vm.lead",
        "r1",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    let store = temp_dir("runs-idem-store");
    let engine = open(&store);
    pass(&engine, &share, T0 + 10 * DAY, 100, false);
    let after_first = ledger_last(&engine);
    let tables = dump(&engine);
    // Same files again, full pass: nothing read changes, nothing appended.
    let (scan, record) = pass(&engine, &share, T0 + 11 * DAY, 100, true);
    assert_eq!(
        scan["files"].as_array().unwrap().len(),
        0,
        "unchanged files are not read again"
    );
    assert_eq!(record["recorded"], json!(0));
    assert_eq!(ledger_last(&engine), after_first);
    assert_eq!(dump(&engine), tables);
    // The same bytes handed in twice (a crash between scan and record, a repeated call).
    let text = fs::read_to_string(
        share
            .root
            .join("v1/writers/vm.lead/runs")
            .join(&iso(T0 + DAY)[..10])
            .join("vm.lead--r1.json"),
    )
    .unwrap();
    let path = format!("writers/vm.lead/runs/{}/vm.lead--r1.json", &iso(T0 + DAY)[..10]);
    let twice = call(
        &engine,
        "runsRecord",
        json!({ "files": [
        { "path": path, "text": text, "size": text.len(), "mtimeMs": 1 },
        { "path": path, "text": text, "size": text.len(), "mtimeMs": 1 },
    ], "nowMs": T0 + 12 * DAY }),
    );
    assert_eq!(twice["recorded"], json!(0));
    assert_eq!(twice["known"], json!(2));
    assert_eq!(ledger_last(&engine), after_first);
    // The same content under a second path (a copy) is one revision, and its path is remembered.
    let copy = format!("writers/vm.lead/runs/{}/vm.lead--r1.json", &iso(T0 + 2 * DAY)[..10]);
    let copied = call(
        &engine,
        "runsRecord",
        json!({ "files": [{ "path": copy, "text": text, "size": text.len(), "mtimeMs": 2 }], "nowMs": T0 + 12 * DAY }),
    );
    assert_eq!(copied["recorded"], json!(0));
    assert_eq!(call(&engine, "runsStatus", json!({}))["revisions"], json!(1));
    assert_eq!(call(&engine, "runsStatus", json!({}))["files"], json!(2));
}

#[test]
fn a_newer_seq_wins_across_day_folders_and_every_revision_is_kept() {
    let share = Share::new("runs-seq");
    share.run(
        "vm.lead",
        "r1",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("high"),
        Some("codex"),
        Some("build"),
    );
    let store = temp_dir("runs-seq-store");
    let engine = open(&store);
    pass(&engine, &share, T0 + 10 * DAY, 100, false);
    // A correction with a higher seq, written into the next day's folder.
    share.run(
        "vm.lead",
        "r1",
        2,
        T0 + 2 * DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    pass(&engine, &share, T0 + 11 * DAY, 100, false);
    let all = query(&engine, json!({}));
    assert_eq!(all.len(), 1);
    assert_eq!(all[0]["doc"]["seq"], json!(2));
    assert_eq!(all[0]["doc"]["model"]["effort"], json!("max"));
    let history = call(&engine, "runHistory", json!({ "runId": "vm.lead--r1" }));
    assert_eq!(history.as_array().unwrap().len(), 2);
    // The older copy read again later never takes the head back.
    let old = history[0]["sha256"].as_str().unwrap().to_string();
    let original = call(&engine, "runOriginal", json!({ "runId": "vm.lead--r1", "sha256": old }));
    assert!(original.as_str().unwrap().contains("\"high\""));
    pass(&engine, &share, T0 + 12 * DAY, 100, true);
    assert_eq!(query(&engine, json!({}))[0]["doc"]["seq"], json!(2));
    // Only the current revision is in the time index.
    assert_eq!(query(&engine, json!({ "toMs": T0 + DAY + 1 })).len(), 0);
}

#[test]
fn backfill_reads_everything_in_passes_with_no_cap() {
    let share = Share::new("runs-backfill");
    let n = 2_000u64;
    for i in 0..n {
        let writer = format!("vm.w{}", i % 37);
        share.run(
            &writer,
            &format!("r{i}"),
            1,
            T0 + (i % 40) * DAY + i,
            "claude-opus-5-5",
            Some("xhigh"),
            Some("claude-code"),
            Some("build"),
        );
    }
    let store = temp_dir("runs-backfill-store");
    let engine = open(&store);
    let mut passes = 0;
    loop {
        let (scan, record) = pass(&engine, &share, T0 + 50 * DAY, 300, false);
        passes += 1;
        assert!(record["recorded"].as_u64().unwrap() <= 300);
        if !scan["more"].as_bool().unwrap() {
            break;
        }
        assert!(passes < 20, "backfill must finish");
    }
    assert!(passes >= 7, "a budget of 300 needs at least 7 passes for 2,000 files");
    let status = call(&engine, "runsStatus", json!({}));
    assert_eq!(status["runs"], json!(n));
    let mut seen = 0;
    let mut cursor = Json::Null;
    loop {
        let page = call(&engine, "runsQuery", json!({ "limit": 450, "after": cursor }));
        seen += page["runs"].as_array().unwrap().len();
        cursor = page["next"].clone();
        if cursor.is_null() {
            break;
        }
    }
    assert_eq!(seen as u64, n);
    // Full payload blocks are compressed.
    assert!(status["payloadBlocks"]["compressed"].as_u64().unwrap() >= 60);
}

#[test]
fn runs_are_kept_forever_and_a_rebuild_gives_the_same_tables() {
    let share = Share::new("runs-forever");
    for i in 0..70u64 {
        share.run(
            "vm.lead",
            &format!("r{i}"),
            1,
            T0 + i * 3_600_000,
            "gpt-6.1-sol",
            Some("max"),
            Some("codex"),
            Some("review"),
        );
    }
    let store = temp_dir("runs-forever-store");
    let tables;
    {
        let engine = open(&store);
        pass(&engine, &share, T0 + DAY, 1000, false);
        // Ten years later, with pruning, retirement and compaction.
        call(
            &engine,
            "maintain",
            json!({ "nowMs": T0 + 3650 * DAY, "compact": true }),
        );
        assert_eq!(call(&engine, "runsStatus", json!({}))["runs"], json!(70));
        assert_eq!(query(&engine, json!({})).len(), 70);
        tables = dump(&engine);
        engine.close().unwrap();
    }
    // A lost redb is rebuilt from the ledger, row for row.
    fs::remove_file(store.join("aac.redb")).unwrap();
    let engine = open(&store);
    assert_eq!(dump(&engine), tables);
}

#[test]
fn a_new_alias_rekeys_only_the_affected_runs_and_keeps_the_earlier_identity() {
    let share = Share::new("runs-rekey");
    share.run(
        "vm.lead",
        "r1",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    share.run(
        "vm.lead",
        "r2",
        1,
        T0 + 2 * DAY,
        "sol-preview",
        Some("high"),
        Some("codex"),
        Some("build"),
    );
    let store = temp_dir("runs-rekey-store");
    let engine = open(&store);
    let (_, first) = pass(&engine, &share, T0 + 10 * DAY, 100, false);
    let audit = call(&engine, "runsAudit", json!({}));
    assert_eq!(audit["unmapped"], json!([{ "modelRaw": "sol-preview", "runs": 1 }]));
    share.set_aliases(&[("sol-preview", "openai/gpt-6.1-sol")]);
    let (_, second) = pass(&engine, &share, T0 + 11 * DAY, 100, false);
    assert_eq!(second["catalogChanged"], json!(true));
    assert_eq!(second["rekeyed"], json!(1));
    assert_ne!(first["catalogHash"], second["catalogHash"]);
    let all = query(&engine, json!({}));
    assert_eq!(all[1]["doc"]["model"]["id"], json!("openai/gpt-6.1-sol"));
    assert_eq!(all[1]["identity"]["modelCatalogHash"], second["catalogHash"]);
    // The first run's identity was computed under the old catalog and did not change.
    assert_eq!(all[0]["identity"]["modelCatalogHash"], first["catalogHash"]);
    // A damaged catalog write keeps the last good version: nothing is re-keyed.
    fs::write(share.root.join("v1/catalog/model-aliases.json"), "{ broken").unwrap();
    let (_, third) = pass(&engine, &share, T0 + 12 * DAY, 100, false);
    assert_eq!(third["catalogChanged"], json!(false));
    assert_eq!(third["rekeyed"], json!(0));
    // The re-keying survives a rebuild from the ledger.
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(store.join("aac.redb")).unwrap();
    let engine = open(&store);
    assert_eq!(
        query(&engine, json!({}))[1]["doc"]["model"]["id"],
        json!("openai/gpt-6.1-sol")
    );
}

#[test]
fn queries_filter_by_date_source_and_task_type() {
    let share = Share::new("runs-filter");
    share.run(
        "vm.lead",
        "a",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    share.run(
        "vm.lead",
        "b",
        1,
        T0 + 5 * DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("review"),
    );
    share.run(
        "vm.lead",
        "c",
        1,
        T0 + 9 * DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("review"),
    );
    // A benchmark run.
    let path = share.run(
        "vm.lead",
        "d",
        1,
        T0 + 9 * DAY + 1,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("review"),
    );
    let file = share.root.join("v1").join(&path);
    let text = fs::read_to_string(&file)
        .unwrap()
        .replace("\"observed\"", "\"benchmark\"");
    fs::write(&file, text).unwrap();
    let store = temp_dir("runs-filter-store");
    let engine = open(&store);
    pass(&engine, &share, T0 + 20 * DAY, 100, false);
    let ids = |args: Json| -> Vec<String> {
        query(&engine, args)
            .iter()
            .map(|r| r["doc"]["id"].as_str().unwrap().to_string())
            .collect()
    };
    assert_eq!(
        ids(json!({ "fromMs": T0 + 2 * DAY, "toMs": T0 + 8 * DAY })),
        vec!["vm.lead--b"]
    );
    assert_eq!(
        ids(json!({ "taskType": "review", "source": "observed" })),
        vec!["vm.lead--b", "vm.lead--c"]
    );
    assert_eq!(ids(json!({ "source": "benchmark" })), vec!["vm.lead--d"]);
    assert_eq!(ids(json!({ "taskType": "no-such-type" })), Vec::<String>::new());
    assert_eq!(
        ids(json!({ "fromMs": T0 + 30 * DAY, "toMs": T0 })),
        Vec::<String>::new()
    );
    assert_eq!(
        ids(json!({ "reviewsOnly": true })),
        Vec::<String>::new(),
        "no run here has a review block"
    );
}

#[test]
fn refused_files_are_recorded_once_and_named() {
    let share = Share::new("runs-refused");
    share.run(
        "vm.lead",
        "ok",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        None,
        Some("codex"),
        Some("build"),
    );
    let dir = share.root.join("v1/writers/vm.lead/runs").join(&iso(T0 + DAY)[..10]);
    fs::write(dir.join("vm.lead--bad.json"), "{\"schema\":\"agent-reports/run@1\"}").unwrap();
    fs::write(dir.join("vm.lead--big.json"), "x".repeat(40 * 1024)).unwrap();
    let store = temp_dir("runs-refused-store");
    let engine = open(&store);
    let (_, record) = pass(&engine, &share, T0 + 10 * DAY, 100, false);
    assert_eq!(record["recorded"], json!(1));
    assert_eq!(record["refused"], json!(2));
    let refused = call(&engine, "runsRefused", json!({}));
    let reasons: Vec<&str> = refused
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["reason"].as_str().unwrap())
        .collect();
    assert!(reasons.contains(&"larger than the 32 KB cap"));
    assert!(reasons.contains(&"writer does not match its folder"));
    let before = ledger_last(&engine);
    let (scan, again) = pass(&engine, &share, T0 + 11 * DAY, 100, true);
    assert_eq!(
        scan["files"].as_array().unwrap().len(),
        0,
        "a refused file is not read again until it changes"
    );
    assert_eq!(again["refused"], json!(0));
    assert_eq!(ledger_last(&engine), before);
}

#[test]
fn identity_table_orders_families_and_efforts() {
    let share = Share::new("runs-identity");
    let store = temp_dir("runs-identity-store");
    let engine = open(&store);
    pass(&engine, &share, T0, 100, false);
    let table = call(&engine, "runsIdentity", json!({}));
    let families: Vec<&str> = table["families"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["id"].as_str().unwrap())
        .collect();
    assert_eq!(families, vec!["anthropic", "openai", "google", "qwen"]);
    assert_eq!(table["models"]["openai/gpt-6.1-sol"]["family"], json!("openai"));
    assert_eq!(table["efforts"][5], json!("xhigh"));
}

// ---------------------------------------------------------------------------------------------
// Review 1 regressions (H1, M1, M2). Synthetic values only.

/// A synthetic credential shape (never a real one): the reviewer's witness.
const WITNESS: &str = "sk-ant-api03-abcdefghijklmnopqrstuvwxyz0123";

/// Whether `needle` occurs in any file of the store folder, compressed segments decoded.
fn store_holds(dir: &Path, needle: &[u8]) -> Vec<String> {
    let mut hits = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(at) = stack.pop() {
        for entry in fs::read_dir(&at).unwrap().flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let mut bytes = fs::read(&path).unwrap();
            if path.extension().is_some_and(|e| e == "zst") {
                bytes = zstd::stream::decode_all(bytes.as_slice()).unwrap();
            }
            if bytes.windows(needle.len()).any(|w| w == needle) {
                hits.push(path.display().to_string());
            }
        }
    }
    hits
}

#[test]
fn a_refused_file_keeps_no_name_and_no_credential_reaches_the_db_or_the_ledger() {
    let share = Share::new("runs-h1");
    let day = &iso(T0 + DAY)[..10];
    let dir = share.root.join("v1/writers/vm.lead/runs").join(day);
    fs::create_dir_all(&dir).unwrap();
    // The witness as a file name and run id; and as text inside a document with a plain name.
    let mut doc = json!({
        "schema": "agent-reports/run@1", "id": format!("vm.lead--{WITNESS}"), "writer": "vm.lead", "seq": 1,
        "writtenAt": iso(T0 + DAY), "source": "observed",
        "task": { "id": "t", "type": "build", "title": "plain" },
        "model": { "id": "gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + DAY), "outcome": "done"
    });
    fs::write(dir.join(format!("{WITNESS}.json")), doc.to_string()).unwrap();
    doc["id"] = json!("vm.lead--plain-1");
    doc["task"]["title"] = json!(format!("uses {WITNESS}"));
    fs::write(dir.join("vm.lead--plain-1.json"), doc.to_string()).unwrap();
    share.run(
        "vm.lead",
        "ok-1",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    let store = temp_dir("runs-h1-store");
    {
        let engine = open(&store);
        let (_, record) = pass(&engine, &share, T0 + 10 * DAY, 100, true);
        assert_eq!(
            (record["recorded"].as_u64(), record["refused"].as_u64()),
            (Some(1), Some(2))
        );
        let refused = call(&engine, "runsRefused", json!({}));
        assert!(!refused.to_string().contains(WITNESS), "{refused}");
        let rows = refused.as_array().unwrap();
        assert!(
            rows.iter()
                .all(|r| r["writer"] == json!("vm.lead") && r["pathHash"].as_str().unwrap().len() == 32)
        );
        let reasons: Vec<&str> = rows.iter().map(|r| r["reason"].as_str().unwrap()).collect();
        assert!(reasons.contains(&"refused: its file name looks like a credential"));
        assert!(reasons.contains(&"refused: it contains what looks like a credential"));
        // A second pass reads neither again and adds nothing.
        let (scan, again) = pass(&engine, &share, T0 + 11 * DAY, 100, true);
        assert_eq!(scan["files"].as_array().unwrap().len(), 0);
        assert_eq!(again["refused"], json!(0));
        engine.close().unwrap();
    }
    assert_eq!(
        store_holds(&store, WITNESS.as_bytes()),
        Vec::<String>::new(),
        "after close"
    );
    // Reopened, then rebuilt from the ledger: still nowhere.
    {
        let engine = open(&store);
        assert!(!call(&engine, "runsRefused", json!({})).to_string().contains(WITNESS));
        engine.close().unwrap();
    }
    fs::remove_file(store.join("aac.redb")).unwrap();
    {
        let engine = open(&store);
        assert_eq!(call(&engine, "runsRefused", json!({})).as_array().unwrap().len(), 2);
        engine.close().unwrap();
    }
    assert_eq!(
        store_holds(&store, WITNESS.as_bytes()),
        Vec::<String>::new(),
        "after a rebuild"
    );
    // The witness file is still in the share copy: the check above is not vacuous.
    assert!(!store_holds(&share.root, WITNESS.as_bytes()).is_empty());
}

#[test]
fn a_sequence_past_u32_keeps_the_highest_correction() {
    let share = Share::new("runs-m1");
    let store = temp_dir("runs-m1-store");
    let big = 1u64 << 32;
    let engine = open(&store);
    // The higher sequence first, then a lower one above 2^32, same writtenAt.
    share.run(
        "vm.lead",
        "r1",
        big + 1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    pass(&engine, &share, T0 + 10 * DAY, 100, false);
    let path = share.run(
        "vm.lead",
        "r1",
        big,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    let file = share.root.join("v1").join(&path);
    let text = fs::read_to_string(&file).unwrap().replace("\"done\"", "\"failed\"");
    fs::write(&file, text).unwrap();
    pass(&engine, &share, T0 + 11 * DAY, 100, true);
    let winner = |engine: &Engine| {
        let all = query(engine, json!({}));
        assert_eq!(all.len(), 1);
        (
            all[0]["doc"]["seq"].as_u64().unwrap(),
            all[0]["doc"]["outcome"].as_str().unwrap().to_string(),
        )
    };
    assert_eq!(winner(&engine), (big + 1, "done".to_string()));
    let history = call(&engine, "runHistory", json!({ "runId": "vm.lead--r1" }));
    let seqs: Vec<u64> = history
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h["seq"].as_u64().unwrap())
        .collect();
    assert_eq!(seqs, vec![big + 1, big]);
    engine.close().unwrap();
    drop(engine);
    let engine = open(&store);
    assert_eq!(winner(&engine), (big + 1, "done".to_string()), "after reopen");
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(store.join("aac.redb")).unwrap();
    let engine = open(&store);
    assert_eq!(winner(&engine), (big + 1, "done".to_string()), "after a rebuild");
}

#[test]
fn a_copy_at_a_greater_path_keeps_the_typescript_winner() {
    let share = Share::new("runs-m2");
    let store = temp_dir("runs-m2-store");
    let engine = open(&store);
    let write = |ended: u64, outcome: &str| -> String {
        // Same run id, seq and writtenAt in every copy: only the path breaks the tie.
        let path = share.run(
            "vm.lead",
            "r1",
            1,
            ended,
            "gpt-6.1-sol",
            Some("max"),
            Some("codex"),
            Some("build"),
        );
        let file = share.root.join("v1").join(&path);
        let doc = fs::read_to_string(&file)
            .unwrap()
            .replace(&iso(ended), &iso(T0 + DAY))
            .replace("\"done\"", &format!("\"{outcome}\""));
        fs::write(&file, doc).unwrap();
        path
    };
    // Nov 01: A (done). Nov 03: the same A. Nov 02: B (failed).
    let first = write(T0 + DAY, "done");
    pass(&engine, &share, T0 + 10 * DAY, 100, false);
    // The same bytes under Nov 03.
    let later = first.replace(&iso(T0 + DAY)[..10], &iso(T0 + 3 * DAY)[..10]);
    let later_file = share.root.join("v1").join(&later);
    fs::create_dir_all(later_file.parent().unwrap()).unwrap();
    fs::copy(share.root.join("v1").join(&first), &later_file).unwrap();
    let (_, copy) = pass(&engine, &share, T0 + 11 * DAY, 100, false);
    assert_eq!((copy["recorded"].as_u64(), copy["known"].as_u64()), (Some(0), Some(1)));
    let middle = write(T0 + 2 * DAY, "failed");
    pass(&engine, &share, T0 + 12 * DAY, 100, false);
    assert!(first < middle && middle < later);
    let winner = |engine: &Engine| {
        let all = query(engine, json!({}));
        assert_eq!(all.len(), 1);
        (
            all[0]["file"].as_str().unwrap().to_string(),
            all[0]["doc"]["outcome"].as_str().unwrap().to_string(),
        )
    };
    // The highest seq, then writtenAt, then the greatest path: A, seen at Nov 03.
    assert_eq!(winner(&engine), (later.clone(), "done".to_string()));
    assert_eq!(call(&engine, "runsStatus", json!({}))["revisions"], json!(2));
    engine.close().unwrap();
    drop(engine);
    fs::remove_file(store.join("aac.redb")).unwrap();
    let engine = open(&store);
    assert_eq!(winner(&engine), (later.clone(), "done".to_string()), "after a rebuild");
    // A fresh store reading all three files at once agrees.
    let fresh = temp_dir("runs-m2-fresh");
    let other = open(&fresh);
    pass(&other, &share, T0 + 13 * DAY, 100, true);
    assert_eq!(winner(&other), (later, "done".to_string()), "fresh ingest");
}
