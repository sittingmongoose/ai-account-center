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

/// SB3 (2026-11-23 A): a writer's `runs` folder that cannot be listed hides every date folder
/// under it, so the scan names the folder (the owner keeps its first time and the composer marks
/// the verdicts that used a retained review under it). A writer that never wrote a run has no
/// `runs` folder: ordinary absence, not named.
#[cfg(unix)]
#[test]
fn an_unreadable_writer_runs_folder_is_named_and_a_missing_one_is_not() {
    use std::os::unix::fs::PermissionsExt;
    let share = Share::new("runs-sb3-folder");
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
    // A writer folder with a thread and no `runs` folder.
    share.thread("mac.reviews", "thread:delegated-task:command%3Areview%3A1");
    let store = temp_dir("runs-sb3-folder-store");
    let engine = open(&store);
    let named = |scan: &Json| -> Vec<String> {
        scan["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| p["file"].as_str().unwrap().to_string())
            .collect()
    };
    let (scan, _) = pass(&engine, &share, T0 + 10 * DAY, 100, true);
    assert!(
        named(&scan).is_empty(),
        "a writer with no runs folder is not a problem: {}",
        scan["problems"]
    );
    let runs = share.root.join("v1/writers/vm.lead/runs");
    let original = fs::metadata(&runs).unwrap().permissions();
    fs::set_permissions(&runs, fs::Permissions::from_mode(0o000)).unwrap();
    let (scan, _) = pass(&engine, &share, T0 + 11 * DAY, 100, true);
    fs::set_permissions(&runs, original).unwrap();
    assert_eq!(
        named(&scan),
        vec!["writers/vm.lead/runs".to_string()],
        "the unlistable folder is named: {}",
        scan["problems"]
    );
    assert_eq!(scan["problems"][0]["reason"], json!("folder could not be listed"));
    // The retained run still reads from the store; the folder problem names why nothing new came.
    assert_eq!(query(&engine, json!({})).len(), 1);
    let (scan, _) = pass(&engine, &share, T0 + 12 * DAY, 100, true);
    assert_eq!(scan["state"], json!("ok"));
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
                .all(|r| r.get("writer").is_none() && r["pathHash"].as_str().unwrap().len() == 32)
        );
        let reasons: Vec<&str> = rows.iter().map(|r| r["reason"].as_str().unwrap()).collect();
        assert!(reasons.contains(&"refused: its file name looks like a credential"));
        assert!(reasons.contains(&"refused: it contains what looks like a credential"));
        // A second pass reads neither again and adds nothing.
        let (scan, again) = pass(&engine, &share, T0 + 11 * DAY, 100, true);
        assert_eq!(scan["files"].as_array().unwrap().len(), 0);
        assert_eq!(again["refused"], json!(0));
        // Sealed and compressed before the bytes are read.
        call(&engine, "maintain", json!({ "nowMs": T0 + 12 * DAY }));
        engine.close().unwrap();
    }
    assert_eq!(
        store_holds(&store, WITNESS.as_bytes()),
        Vec::<String>::new(),
        "after sealing and close"
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

#[test]
fn a_refusal_reason_never_quotes_the_writer_folder() {
    let share = Share::new("runs-reason");
    let day = &iso(T0 + DAY)[..10];
    let dir = share.root.join("v1/writers/vm.lead/runs").join(day);
    fs::create_dir_all(&dir).unwrap();
    // An id naming another writer: the id rule's reason quotes the folder's writer.
    let doc = json!({
        "schema": "agent-reports/run@1", "id": "vm.other--x", "writer": "vm.lead", "seq": 1,
        "writtenAt": iso(T0 + DAY), "source": "observed",
        "task": { "id": "t", "type": "build", "title": "plain" },
        "model": { "id": "gpt-6.1-sol" }, "endedAt": iso(T0 + DAY), "outcome": "done"
    });
    fs::write(dir.join("vm.lead--x.json"), doc.to_string()).unwrap();
    let store = temp_dir("runs-reason-store");
    let engine = open(&store);
    pass(&engine, &share, T0 + 10 * DAY, 100, true);
    let refused = call(&engine, "runsRefused", json!({}));
    assert_eq!(
        refused[0]["reason"],
        json!("id does not name its writer: it must start with \"<writer>--\"")
    );
    assert!(refused[0].get("writer").is_none());
    call(&engine, "maintain", json!({ "nowMs": T0 + 12 * DAY }));
    engine.close().unwrap();
    // Nothing of the refused file's writer or name was kept.
    assert!(store_holds(&store, b"vm.lead").is_empty());
}

// ---------------------------------------------------------------------------------------------
// Recheck 1 regressions (H2): every check is the shared secret policy, in Rust as in TypeScript.
// Here Rust stands alone: the pass hands it every text, as if the TypeScript filter were absent.

/// The reviewer's three witnesses (synthetic values only).
const WRITER_KEY: &str = "sk-abcdefghijklmnopqrstuvwx";
const USERINFO: &str = "synthetic-password123";
const BASIC: &str = "c3ludGhldGljOnBhc3N3b3JkMTIz";

#[test]
fn the_shared_policy_keeps_the_recheck_witnesses_out_of_the_db_and_the_ledger() {
    let share = Share::new("runs-h2");
    let writer_key = format!("vm.{WRITER_KEY}");
    // 1. A clean run under a writer folder named like a key, and that writer's thread.
    share.run(
        &writer_key,
        "a",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    share.thread(&writer_key, "thread-plain-0");
    // 2. URL userinfo on a local host in a task title, under a plain writer.
    let path = share.run(
        "vm.synthetic",
        "url",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    let file = share.root.join("v1").join(&path);
    let mut doc: Json = serde_json::from_str(&fs::read_to_string(&file).unwrap()).unwrap();
    doc["task"]["title"] = json!(format!("https://agent:{USERINFO}@localhost/path"));
    fs::write(&file, doc.to_string()).unwrap();
    // 3. A Basic credential as the thread value of that writer, whose other run is clean.
    share.thread("vm.synthetic", &format!("Basic {BASIC}"));
    share.run(
        "vm.synthetic",
        "clean",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    // Control: a plain thread value is still taken, so the thread check is not vacuous.
    share.thread("vm.plain", "thread-plain-1");
    share.run(
        "vm.plain",
        "clean",
        1,
        T0 + DAY,
        "gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    let needles = [WRITER_KEY, USERINFO, BASIC];
    let store = temp_dir("runs-h2-store");
    {
        let engine = open(&store);
        let (scan, record) = pass(&engine, &share, T0 + 10 * DAY, 100, true);
        // The scan handed every text over: Rust's own check refused them, not the scan.
        assert_eq!(scan["files"].as_array().unwrap().len(), 4);
        assert_eq!(scan["threads"].as_object().unwrap().len(), 3);
        assert_eq!(
            (record["recorded"].as_u64(), record["refused"].as_u64()),
            (Some(2), Some(2)),
            "{record}"
        );
        let runs = query(&engine, json!({}));
        let thread_of = |writer: &str| {
            runs.iter()
                .find(|r| r["doc"]["writer"] == json!(writer))
                .map(|r| r["ids"]["t3ThreadId"].clone())
                .unwrap()
        };
        assert_eq!(thread_of("vm.synthetic"), Json::Null);
        assert_eq!(thread_of("vm.plain")["value"], json!("thread-plain-1"));
        let refused = call(&engine, "runsRefused", json!({}));
        let reasons: Vec<&str> = refused
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["reason"].as_str().unwrap())
            .collect();
        assert!(
            reasons.contains(&"refused: its file name looks like a credential"),
            "{refused}"
        );
        assert!(
            reasons.contains(&"refused: it contains what looks like a credential"),
            "{refused}"
        );
        for needle in needles {
            assert!(!json!([runs, refused]).to_string().contains(needle), "{needle}");
        }
        // Sealed and compressed before the bytes are read.
        call(&engine, "maintain", json!({ "nowMs": T0 + 12 * DAY }));
        engine.close().unwrap();
    }
    for needle in needles {
        assert_eq!(
            store_holds(&store, needle.as_bytes()),
            Vec::<String>::new(),
            "{needle} after sealing"
        );
    }
    {
        let engine = open(&store);
        assert_eq!(query(&engine, json!({})).len(), 2, "reopened");
        engine.close().unwrap();
    }
    fs::remove_file(store.join("aac.redb")).unwrap();
    {
        let engine = open(&store);
        let runs = query(&engine, json!({}));
        assert_eq!(runs.len(), 2, "replayed from the ledger");
        assert_eq!(call(&engine, "runsRefused", json!({})).as_array().unwrap().len(), 2);
        engine.close().unwrap();
    }
    for needle in needles {
        assert_eq!(
            store_holds(&store, needle.as_bytes()),
            Vec::<String>::new(),
            "{needle} after a rebuild"
        );
        // Each witness is in the share copy: the checks above are not vacuous.
        assert!(
            !store_holds(&share.root, needle.as_bytes()).is_empty(),
            "{needle} in the share"
        );
    }
}

#[test]
fn a_catalog_text_the_policy_refuses_is_never_stored() {
    let share = Share::new("runs-h2-catalog");
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
    let store = temp_dir("runs-h2-catalog-store");
    let engine = open(&store);
    pass(&engine, &share, T0 + 10 * DAY, 100, true);
    let before = call(&engine, "runsStatus", json!({}))["catalogHash"].clone();
    // An alias text naming a credential: the last stored catalog stays current.
    let alias = format!("https://agent:{USERINFO}@localhost/model");
    share.set_aliases(&[(alias.as_str(), "openai/gpt-6.1-sol")]);
    let (_, record) = pass(&engine, &share, T0 + 11 * DAY, 100, true);
    assert_eq!(record["catalogChanged"], json!(false), "{record}");
    assert_eq!(call(&engine, "runsStatus", json!({}))["catalogHash"], before);
    call(&engine, "maintain", json!({ "nowMs": T0 + 12 * DAY }));
    engine.close().unwrap();
    assert_eq!(store_holds(&store, USERINFO.as_bytes()), Vec::<String>::new());
}

#[test]
fn correction_fixture_resolves_before_native_query_filters() {
    let fixture: Json = serde_json::from_str(include_str!("../../../tests/fixtures/run-corrections.json")).unwrap();
    let share = Share::new("run-corrections-share");
    for doc in fixture["runs"].as_array().unwrap() {
        let dir = share
            .root
            .join("v1/writers")
            .join(doc["writer"].as_str().unwrap())
            .join("runs/2026-11-06");
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join(format!("{}.json", doc["id"].as_str().unwrap())),
            doc.to_string(),
        )
        .unwrap();
    }
    let db = temp_dir("run-corrections-store");
    let engine = open(&db);
    pass(&engine, &share, T0 + 10 * DAY, 1000, true);
    let mut ids: Vec<_> = query(&engine, json!({}))
        .iter()
        .map(|r| r["doc"]["id"].clone())
        .collect();
    ids.sort_by(|a, b| a.as_str().cmp(&b.as_str()));
    assert_eq!(json!(ids), fixture["ids"]);
    assert_eq!(call(&engine, "runsStatus", json!({}))["runs"], 7);
    assert_eq!(call(&engine, "runsAudit", json!({}))["runs"], 7);
    let page = call(&engine, "runsQuery", json!({"taskType":"build","limit":2}));
    assert_eq!(page["runs"].as_array().unwrap().len(), 2);
    assert_eq!(page["problems"].as_array().unwrap().len(), 5);
    let next = call(
        &engine,
        "runsQuery",
        json!({"taskType":"build","limit":2,"after":page["next"]}),
    );
    assert_eq!(next["runs"].as_array().unwrap().len(), 2);
    let before = query(&engine, json!({"toMs": T0 + 5 * DAY}));
    assert!(before.is_empty());
    let reviews = query(&engine, json!({"reviewsOnly":true}));
    assert_eq!(reviews.len(), 2);
    assert!(
        reviews
            .iter()
            .any(|r| r["doc"]["review"]["of"] == "vm.corrections--build-new")
    );
    let original = share.run(
        "vm.corrections",
        "filter-old",
        1,
        T0 + DAY,
        "openai/gpt-6.1-sol",
        Some("high"),
        Some("codex"),
        Some("build"),
    );
    let original = share.root.join("v1").join(original);
    let mut corrected: Json = serde_json::from_str(&fs::read_to_string(&original).unwrap()).unwrap();
    corrected["id"] = json!("vm.corrections--filter-new");
    corrected["corrects"] = json!("vm.corrections--filter-old");
    corrected["endedAt"] = json!(iso(T0 + 7 * DAY));
    corrected["writtenAt"] = json!(iso(T0 + 7 * DAY));
    corrected["task"]["type"] = json!("fix");
    fs::write(
        original.with_file_name("vm.corrections--filter-new.json"),
        corrected.to_string(),
    )
    .unwrap();
    pass(&engine, &share, T0 + 11 * DAY, 1000, true);
    assert!(
        query(&engine, json!({"toMs":T0 + 2 * DAY})).is_empty(),
        "a correction outside the date filter suppresses its original"
    );
    assert!(
        !query(&engine, json!({"taskType":"build"}))
            .iter()
            .any(|r| r["doc"]["id"] == "vm.corrections--filter-old")
    );
    assert_eq!(query(&engine, json!({"taskType":"fix"})).len(), 1);
    let receipts = call(&engine, "runsQuery", json!({"includeReceipts":true}));
    assert!(
        receipts["accepted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["doc"]["id"] == "vm.corrections--void")
    );
    drop(engine);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

#[test]
fn correction_history_resolves_once_across_five_pages_and_invalidates() {
    let share = Share::new("runcorr-cache-share");
    for i in 0..20_001 {
        share.run(
            "vm.probe",
            &format!("page-{i:05}"),
            1,
            T0 + DAY,
            "gpt-6.1-sol",
            Some("high"),
            Some("codex"),
            Some("build"),
        );
    }
    let first = share
        .root
        .join("v1/writers/vm.probe/runs/2026-11-02/vm.probe--page-00000.json");
    let mut correction: Json = serde_json::from_str(&fs::read_to_string(&first).unwrap()).unwrap();
    correction["id"] = json!("vm.probe--replacement");
    correction["corrects"] = json!("vm.probe--page-00000");
    fs::write(
        first.with_file_name("vm.probe--replacement.json"),
        correction.to_string(),
    )
    .unwrap();
    let db = temp_dir("runcorr-cache-store");
    let engine = std::sync::Arc::new(open(&db));
    loop {
        let (scan, _) = pass(&engine, &share, T0 + 3 * DAY, 30_000, true);
        if scan["more"] != true {
            break;
        }
    }
    assert_eq!(runs::correction_resolution_count(&engine), 0);
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let engine = std::sync::Arc::clone(&engine);
            std::thread::spawn(move || call(&engine, "runsQuery", json!({"limit":1})))
        })
        .collect();
    for reader in readers {
        assert_eq!(reader.join().unwrap()["runs"].as_array().unwrap().len(), 1);
    }
    assert_eq!(
        runs::correction_resolution_count(&engine),
        1,
        "concurrent cold queries share one resolution"
    );
    let mut after = Json::Null;
    let mut ids = std::collections::HashSet::new();
    let mut pages = 0;
    loop {
        let page = call(&engine, "runsQuery", json!({"after":after}));
        pages += 1;
        for row in page["runs"].as_array().unwrap() {
            assert!(ids.insert(row["doc"]["id"].as_str().unwrap().to_owned()));
        }
        after = page["next"].clone();
        if after.is_null() {
            break;
        }
    }
    assert_eq!(pages, 5);
    assert_eq!(ids.len(), 20_001);
    assert!(!ids.contains("vm.probe--page-00000"));
    assert!(ids.contains("vm.probe--replacement"));
    assert_eq!(
        runs::correction_resolution_count(&engine),
        1,
        "one resolution for all five pages"
    );
    call(&engine, "runsStatus", json!({}));
    call(&engine, "runsAudit", json!({}));
    query(&engine, json!({"taskType":"review"}));
    assert_eq!(
        runs::correction_resolution_count(&engine),
        1,
        "status, audit and filters reuse the same snapshot"
    );
    share.set_aliases(&[("probe-alias", "openai/gpt-6.1-sol")]);
    pass(&engine, &share, T0 + 4 * DAY, 30_000, true);
    query(&engine, json!({"limit":1}));
    assert_eq!(
        runs::correction_resolution_count(&engine),
        2,
        "catalog identity invalidates"
    );
    share.run(
        "vm.probe",
        "added",
        2,
        T0 + 5 * DAY,
        "gpt-6.1-sol",
        Some("high"),
        Some("codex"),
        Some("build"),
    );
    pass(&engine, &share, T0 + 6 * DAY, 30_000, true);
    query(&engine, json!({"limit":1}));
    assert_eq!(
        runs::correction_resolution_count(&engine),
        3,
        "new run generation invalidates"
    );
    engine.close().unwrap();
    assert!(
        runs::query(&engine, &runs::QueryArgs::default()).is_err(),
        "closed engine cannot serve a stale snapshot"
    );
    drop(engine);
    let reopened = open(&db);
    assert_eq!(runs::correction_resolution_count(&reopened), 0);
    query(&reopened, json!({"limit":1}));
    assert_eq!(
        runs::correction_resolution_count(&reopened),
        1,
        "reopen owns a fresh cache"
    );
    drop(reopened);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

#[test]
fn native_store_keeps_nanosecond_newer_sol_correction() {
    let fixture: Json =
        serde_json::from_str(include_str!("../../../tests/fixtures/run-corrections-precision.json")).unwrap();
    let share = Share::new("runcorr-precision-share");
    let dir = share.root.join("v1/writers/vm.probe/runs/2026-11-06");
    fs::create_dir_all(&dir).unwrap();
    for doc in fixture["runs"].as_array().unwrap() {
        fs::write(
            dir.join(format!("{}.json", doc["id"].as_str().unwrap())),
            doc.to_string(),
        )
        .unwrap();
    }
    let db = temp_dir("runcorr-precision-store");
    let engine = open(&db);
    pass(&engine, &share, T0 + 10 * DAY, 1000, true);
    let rows = query(&engine, json!({}));
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["doc"]["id"], "vm.probe--a-newer");
    assert_eq!(rows[0]["doc"]["model"]["id"], "openai/gpt-6.1-sol");
    assert_eq!(rows[0]["doc"]["writtenAt"], "2026-11-06T16:00:00.0002Z");
    drop(engine);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// One correction of an existing run: a new file with `corrects` and a later seq.
#[allow(clippy::too_many_arguments)]
fn write_correction(
    share: &Share,
    writer: &str,
    local: &str,
    target: &str,
    ended: u64,
    model: &str,
    effort: &str,
    harness: &str,
) {
    let rel = share.run(
        writer,
        local,
        2,
        ended,
        model,
        Some(effort),
        Some(harness),
        Some("build"),
    );
    let path = share.root.join("v1").join(rel);
    let mut doc: Json = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    doc["corrects"] = json!(format!("{writer}--{target}"));
    fs::write(&path, serde_json::to_string_pretty(&doc).unwrap()).unwrap();
}

#[test]
fn every_page_names_its_snapshot_and_an_ingest_between_pages_changes_it() {
    let share = Share::new("runcorr-page-key");
    for i in 0..4u64 {
        share.run(
            "vm.probe",
            &format!("b{i}"),
            1,
            T0 + i * 1000,
            "openai/gpt-6.1-sol",
            Some("max"),
            Some("codex"),
            Some("build"),
        );
    }
    write_correction(
        &share,
        "vm.probe",
        "b3-fix",
        "b3",
        T0 + 3 * 1000,
        "openai/gpt-6.1-sol",
        "max",
        "codex",
    );
    let db = temp_dir("runcorr-page-key-store");
    let engine = open(&db);
    pass(&engine, &share, T0 + DAY, 1000, true);
    let ids = |page: &Json| -> Vec<String> {
        page["runs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["doc"]["id"].as_str().unwrap().to_owned())
            .collect()
    };
    let first = call(&engine, "runsQuery", json!({"limit":2}));
    assert_eq!(ids(&first), vec!["vm.probe--b0", "vm.probe--b1"]);
    assert_eq!(first["generation"], json!(runs::generation(&engine).unwrap()));
    assert_eq!(
        first["catalogHash"],
        call(&engine, "runsStatus", json!({}))["catalogHash"]
    );
    // The recheck's witness: an ingest commits between two pages of one multi-page read.
    write_correction(
        &share,
        "vm.probe",
        "b0-fix",
        "b0",
        T0 + 5 * 1000,
        "anthropic/claude-opus-5-5",
        "high",
        "claude-code",
    );
    pass(&engine, &share, T0 + 2 * DAY, 1000, true);
    let second = call(&engine, "runsQuery", json!({"limit":2,"after":first["next"]}));
    assert!(
        second["generation"].as_u64().unwrap() > first["generation"].as_u64().unwrap(),
        "the later page names the new snapshot, not the cursor's"
    );
    let third = call(&engine, "runsQuery", json!({"limit":2,"after":second["next"]}));
    assert_eq!(third["generation"], second["generation"]);
    // Without a restart the consumer mixes snapshots: b0 counts twice (Sol and Opus).
    let mut mixed = ids(&first);
    mixed.extend(ids(&second));
    mixed.extend(ids(&third));
    assert!(
        mixed.contains(&"vm.probe--b0".to_owned()) && mixed.contains(&"vm.probe--b0-fix".to_owned()),
        "the race this key prevents: {mixed:?}"
    );
    // With the key, the reader restarts on the change and serves exactly one fresh snapshot.
    let fresh = call(&engine, "runsQuery", json!({}));
    assert_eq!(
        ids(&fresh),
        vec!["vm.probe--b1", "vm.probe--b2", "vm.probe--b3-fix", "vm.probe--b0-fix"]
    );
    let mut restarted: Vec<String> = Vec::new();
    let mut after = Json::Null;
    let mut key = Json::Null;
    loop {
        let page = call(&engine, "runsQuery", json!({"limit":2,"after":after}));
        let page_key = json!([page["generation"], page["catalogHash"]]);
        if after.is_null() {
            key = page_key;
        } else if page_key != key {
            restarted.clear();
            after = Json::Null;
            continue;
        }
        restarted.extend(ids(&page));
        if page["next"].is_null() {
            break;
        }
        after = page["next"].clone();
    }
    assert_eq!(restarted, ids(&fresh));
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

#[test]
fn uncorrected_pages_name_their_snapshot_too() {
    let share = Share::new("runcorr-raw-page-key");
    for i in 0..3u64 {
        share.run(
            "vm.probe",
            &format!("r{i}"),
            1,
            T0 + i * 1000,
            "openai/gpt-6.1-sol",
            Some("max"),
            Some("codex"),
            Some("build"),
        );
    }
    let db = temp_dir("runcorr-raw-page-key-store");
    let engine = open(&db);
    pass(&engine, &share, T0 + DAY, 1000, true);
    let first = call(&engine, "runsQuery", json!({"limit":2}));
    assert_eq!(first["correctionsResolved"], Json::Null, "the raw indexed path");
    assert_eq!(first["generation"], json!(runs::generation(&engine).unwrap()));
    assert_eq!(
        first["catalogHash"],
        call(&engine, "runsStatus", json!({}))["catalogHash"]
    );
    share.run(
        "vm.probe",
        "r3",
        1,
        T0 + 3 * 1000,
        "openai/gpt-6.1-sol",
        Some("max"),
        Some("codex"),
        Some("build"),
    );
    pass(&engine, &share, T0 + 2 * DAY, 1000, true);
    let second = call(&engine, "runsQuery", json!({"limit":2,"after":first["next"]}));
    assert!(
        second["generation"].as_u64().unwrap() > first["generation"].as_u64().unwrap(),
        "a run recorded between pages moves the raw path's key too"
    );
    // A catalog-only write moves the key without a new run.
    share.set_aliases(&[("raw-alias", "openai/gpt-6.1-sol")]);
    pass(&engine, &share, T0 + 3 * DAY, 1000, true);
    let third = call(&engine, "runsQuery", json!({"limit":2}));
    assert_ne!(third["catalogHash"], first["catalogHash"]);
    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

#[test]
fn scoreboard_aggregates_update_atomically_and_migrate_from_existing_runs() {
    use aac_store::scoreboard_agg::{self, ScoreboardFilter};

    let share = Share::new("sb-agg-lifecycle");
    let day = &iso(T0 + 2 * DAY)[..10];
    let dir = share.root.join("v1/writers/vm.agg/runs").join(day);
    fs::create_dir_all(&dir).unwrap();

    // 1. Review arrives BEFORE its target (simulating newest-days-first backfill).
    let rev_early = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.agg--rev-1",
        "writer": "vm.agg",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 60_000),
        "source": "observed",
        "task": { "id": "t-rev-1", "type": "review", "title": "Review target-1" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY + 60_000),
        "outcome": "done",
        "review": {
            "of": "vm.agg--target-1",
            "verdict": "pass",
            "findings": { "high": 0, "medium": 0, "low": 0 },
            "overturned": false,
            "disputed": false
        }
    });
    fs::write(dir.join("vm.agg--rev-1.json"), rev_early.to_string()).unwrap();

    let db = temp_dir("sb-agg-lifecycle-store");
    let engine = open(&db);
    pass(&engine, &share, T0 + 3 * DAY, 1000, true);

    // 2. Now target-1 arrives (plus a correction fixture with supersedes + void + refused).
    let target = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.agg--target-1",
        "writer": "vm.agg",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY),
        "source": "observed",
        "task": { "id": "t-1", "type": "build", "title": "Build 1", "size": { "tasks": 1 } },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY),
        "wallSeconds": 42,
        "outcome": "done",
        "cost": { "credits": 2.5, "usd": 0.4 }
    });
    fs::write(dir.join("vm.agg--target-1.json"), target.to_string()).unwrap();
    // Correction chain: target-2 -> target-2-fix (retains review on target-2), and target-3 -> void.
    let target2 = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.agg--target-2",
        "writer": "vm.agg",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 1000),
        "source": "observed",
        "task": { "id": "t-2", "type": "build", "title": "Build 2" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY + 1000),
        "outcome": "failed"
    });
    let target2_fix = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.agg--target-2-fix",
        "writer": "vm.agg",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 2000),
        "source": "observed",
        "corrects": "vm.agg--target-2",
        "task": { "id": "t-2", "type": "build", "title": "Build 2 fixed" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY + 2000),
        "outcome": "done"
    });
    let target3 = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.agg--target-3",
        "writer": "vm.agg",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 3000),
        "source": "observed",
        "task": { "id": "t-3", "type": "build", "title": "Build 3" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY + 3000),
        "outcome": "done"
    });
    let target3_void = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.agg--target-3-void",
        "writer": "vm.agg",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 4000),
        "source": "observed",
        "corrects": "vm.agg--target-3",
        "void": { "reason": "duplicate" },
        "task": { "id": "t-3", "type": "build", "title": "Void 3" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY + 4000),
        "outcome": "done"
    });
    let bad_corr = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.agg--bad-corr",
        "writer": "vm.agg",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 5000),
        "source": "observed",
        "corrects": "vm.agg--missing-target",
        "task": { "id": "t-bad", "type": "build", "title": "Missing target" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY + 5000),
        "outcome": "done"
    });
    for (name, doc) in [
        ("vm.agg--target-2.json", target2),
        ("vm.agg--target-2-fix.json", target2_fix),
        ("vm.agg--target-3.json", target3),
        ("vm.agg--target-3-void.json", target3_void),
        ("vm.agg--bad-corr.json", bad_corr),
    ] {
        fs::write(dir.join(name), doc.to_string()).unwrap();
    }
    pass(&engine, &share, T0 + 4 * DAY, 1000, true);

    // Effective runs: rev-1, target-1, target-2-fix (3 total).
    // `runsStatus` and `read_for_scoreboard` must NOT trigger `resolved_history`.
    assert_eq!(call(&engine, "runsStatus", json!({}))["runs"], json!(3));
    let before = scoreboard_agg::read_for_scoreboard(
        &engine,
        &ScoreboardFilter::default(),
        &[],
        &std::collections::HashMap::new(),
    )
    .unwrap();
    assert_eq!(
        runs::correction_resolution_count(&engine),
        0,
        "status + aggregate read must never build ResolvedHistory"
    );
    assert!(before.used_precomputed_buckets);
    assert_eq!(before.effective_runs_total, 3);
    assert_eq!(before.correction_problems.len(), 1);
    assert_eq!(before.correction_problems[0].reason, "corrects names a missing run");
    // Build bucket has target-1 (done, joined with rev-1!) + target-2-fix (done) = 2 runs, 1 joined review.
    let build_bucket = before
        .buckets
        .iter()
        .find(|(k, _)| k.task_type == "build")
        .expect("build bucket");
    assert_eq!(build_bucket.1.n, 2);
    assert_eq!(build_bucket.1.outcome_done, 2);
    assert_eq!(build_bucket.1.joined_reviews.len(), 1);
    assert_eq!(build_bucket.1.joined_reviews[0].review_id, "vm.agg--rev-1");

    // 3. Simulate a pre-migration v3 store: wipe all `run_sb_*` tables + meta keys, close, reopen.
    engine
        .with_db(|db_ref| {
            let txn = aac_store::engine::begin(db_ref)?;
            scoreboard_agg::delete_tables(&txn)?;
            scoreboard_agg::create_tables(&txn)?;
            {
                let mut meta = txn.open_table(runs::RUN_META)?;
                meta.remove("sb_agg_version")?;
                meta.remove("sb_agg_seq")?;
                meta.remove("sb_agg_effective_count")?;
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
    engine.close().unwrap();
    drop(engine);

    // Reopen triggers one-time migration from existing `run_head`/`run_payload`.
    let migrated = open(&db);
    let after = scoreboard_agg::read_for_scoreboard(
        &migrated,
        &ScoreboardFilter::default(),
        &[],
        &std::collections::HashMap::new(),
    )
    .unwrap();
    assert_eq!(after.effective_runs_total, before.effective_runs_total);
    assert_eq!(
        after.buckets, before.buckets,
        "migrated buckets must match incremental buckets"
    );
    assert_eq!(after.correction_problems.len(), before.correction_problems.len());

    // Confirm metadata version/seq stamped and a second reopen does not re-migrate.
    let seq_after = migrated
        .with_db(|db_ref| {
            let read = db_ref.begin_read()?;
            let meta = read.open_table(runs::RUN_META)?;
            let ver = meta
                .get("sb_agg_version")?
                .map(|v| String::from_utf8_lossy(v.value()).into_owned());
            let seq = meta
                .get("sb_agg_seq")?
                .map(|v| String::from_utf8_lossy(v.value()).into_owned());
            Ok((ver, seq))
        })
        .unwrap();
    let want_ver = scoreboard_agg::SB_AGG_VERSION.to_string();
    assert_eq!(seq_after.0.as_deref(), Some(want_ver.as_str()));
    assert_eq!(
        seq_after.1.as_deref().and_then(|s| s.parse::<u64>().ok()),
        Some(runs::generation(&migrated).unwrap())
    );
    migrated.close().unwrap();
    drop(migrated);

    // Second reopen preserves identical aggregates.
    let reopened = open(&db);
    let after2 = scoreboard_agg::read_for_scoreboard(
        &reopened,
        &ScoreboardFilter::default(),
        &[],
        &std::collections::HashMap::new(),
    )
    .unwrap();
    assert_eq!(after2.buckets, before.buckets);
    reopened.close().unwrap();
    drop(reopened);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// Regression for H1: removing a correction, retargeting a correction, or retargeting a review off
/// an active correction chain via higher-sequence revisions must keep persisted aggregates and
/// effective totals identical to full resolution (including after reopen).
#[test]
fn h1_removing_or_retargeting_correction_or_review_keeps_aggregate_in_sync() {
    use aac_store::scoreboard_agg::{self, ScoreboardFilter};

    let share = Share::new("sb-agg-h1");
    let day = &iso(T0 + 2 * DAY)[..10];
    let dir = share.root.join("v1/writers/vm.h1/runs").join(day);
    fs::create_dir_all(&dir).unwrap();
    let db = temp_dir("sb-agg-h1-store");
    let engine = open(&db);

    let make_build = |id: &str, seq: u64, ts_off: u64, corrects: Option<&str>| {
        let mut doc = json!({
            "schema": "agent-reports/run@1",
            "id": id,
            "writer": "vm.h1",
            "seq": seq,
            "writtenAt": iso(T0 + 2 * DAY + ts_off),
            "source": "observed",
            "task": { "id": format!("t-{id}"), "type": "build", "title": format!("Build {id}") },
            "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
            "endedAt": iso(T0 + 2 * DAY + ts_off),
            "outcome": "done"
        });
        if let Some(target) = corrects {
            doc["corrects"] = json!(target);
        }
        doc
    };

    let assert_matches_full = |eng: &Engine, label: &str| {
        let resolved = runs::resolved_history_snapshot(eng).unwrap();
        let agg = scoreboard_agg::read_for_scoreboard(
            eng,
            &ScoreboardFilter::default(),
            &[],
            &std::collections::HashMap::new(),
        )
        .unwrap();
        let expected_eff = resolved.facts().len() as u64;
        assert_eq!(agg.effective_runs_total, expected_eff, "{label}: effective_runs_total");
        let bucket_n: u64 = agg.buckets.iter().map(|(_, b)| b.n).sum();
        assert_eq!(bucket_n, expected_eff, "{label}: sum of bucket n");
        let resolved_build_n = resolved
            .facts()
            .iter()
            .filter(|r| r.task_type.as_deref() == Some("build"))
            .count() as u64;
        let agg_build_n: u64 = agg
            .buckets
            .iter()
            .filter(|(k, _)| k.task_type == "build")
            .map(|(_, b)| b.n)
            .sum();
        assert_eq!(agg_build_n, resolved_build_n, "{label}: build bucket n");
    };

    // 1a. Ingest build A, then same-writer build B with `corrects: A`. Effective count == 1.
    fs::write(dir.join("a.json"), make_build("vm.h1--a", 1, 1000, None).to_string()).unwrap();
    fs::write(
        dir.join("b.json"),
        make_build("vm.h1--b", 1, 2000, Some("vm.h1--a")).to_string(),
    )
    .unwrap();
    pass(&engine, &share, T0 + 3 * DAY, 1000, true);
    assert_matches_full(&engine, "after B corrects A");
    assert_eq!(call(&engine, "runsStatus", json!({}))["runs"], json!(1));

    // Ingest higher-seq revision of B with NO `corrects`: both A and B must now be effective!
    fs::write(dir.join("b.json"), make_build("vm.h1--b", 2, 3000, None).to_string()).unwrap();
    pass(&engine, &share, T0 + 3 * DAY + 5000, 1000, true);
    assert_matches_full(&engine, "after removing corrects from B");
    assert_eq!(call(&engine, "runsStatus", json!({}))["runs"], json!(2));

    // 1b. Add build C and retarget B (seq 3) to `corrects: C`: A and B effective, C superseded.
    fs::write(dir.join("c.json"), make_build("vm.h1--c", 1, 4000, None).to_string()).unwrap();
    fs::write(
        dir.join("b.json"),
        make_build("vm.h1--b", 3, 5000, Some("vm.h1--c")).to_string(),
    )
    .unwrap();
    pass(&engine, &share, T0 + 3 * DAY + 10_000, 1000, true);
    assert_matches_full(&engine, "after retargeting B to correct C");
    assert_eq!(call(&engine, "runsStatus", json!({}))["runs"], json!(2));

    // 1c. Add build D and review R initially reviewing C (in active chain B -> C), then revise R
    // to review unrelated effective target D instead.
    fs::write(dir.join("d.json"), make_build("vm.h1--d", 1, 6000, None).to_string()).unwrap();
    let make_rev = |seq: u64, ts_off: u64, target: &str| {
        json!({
            "schema": "agent-reports/run@1",
            "id": "vm.h1--rev",
            "writer": "vm.h1",
            "seq": seq,
            "writtenAt": iso(T0 + 2 * DAY + ts_off),
            "source": "observed",
            "task": { "id": "t-rev", "type": "review", "title": "Review" },
            "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
            "endedAt": iso(T0 + 2 * DAY + ts_off),
            "outcome": "done",
            "review": {
                "of": target,
                "verdict": "pass",
                "findings": { "high": 0, "medium": 0, "low": 0 },
                "overturned": false,
                "disputed": false
            }
        })
    };
    fs::write(dir.join("rev.json"), make_rev(1, 7000, "vm.h1--c").to_string()).unwrap();
    pass(&engine, &share, T0 + 3 * DAY + 15_000, 1000, true);
    assert_matches_full(&engine, "after review targeting C in correction chain");

    fs::write(dir.join("rev.json"), make_rev(2, 8000, "vm.h1--d").to_string()).unwrap();
    pass(&engine, &share, T0 + 3 * DAY + 20_000, 1000, true);
    assert_matches_full(&engine, "after retargeting review off correction chain to D");
    let agg_after = scoreboard_agg::read_for_scoreboard(
        &engine,
        &ScoreboardFilter::default(),
        &[],
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let build_bucket = agg_after
        .buckets
        .iter()
        .find(|(k, _)| k.task_type == "build")
        .expect("build bucket");
    assert_eq!(build_bucket.1.joined_reviews.len(), 1);
    assert_eq!(build_bucket.1.joined_reviews[0].target_id, "vm.h1--d");
    assert_eq!(build_bucket.1.joined_reviews[0].review_id, "vm.h1--rev");

    // Reopen must preserve the exact same verified totals.
    engine.close().unwrap();
    drop(engine);
    let reopened = open(&db);
    assert_matches_full(&reopened, "after reopen");
    reopened.close().unwrap();
    drop(reopened);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// Regression for H2: replay after a rollback to an older binary with a pending ledger tail must
/// never certify an incomplete aggregate; pre-serve trust check must also catch tampering.
#[test]
fn h2_replay_after_rollback_with_pending_ledger_tail_rebuilds_aggregates() {
    use aac_store::scoreboard_agg::{self, ScoreboardFilter};
    use redb::ReadableTable;

    let share = Share::new("sb-agg-h2");
    let day = &iso(T0 + 2 * DAY)[..10];
    let dir = share.root.join("v1/writers/vm.h2/runs").join(day);
    fs::create_dir_all(&dir).unwrap();
    let db = temp_dir("sb-agg-h2-store");

    let write_run = |name: &str, id: &str, ts_off: u64| {
        let doc = json!({
            "schema": "agent-reports/run@1",
            "id": id,
            "writer": "vm.h2",
            "seq": 1,
            "writtenAt": iso(T0 + 2 * DAY + ts_off),
            "source": "observed",
            "task": { "id": format!("t-{id}"), "type": "build", "title": format!("Build {id}") },
            "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
            "endedAt": iso(T0 + 2 * DAY + ts_off),
            "outcome": "done"
        });
        fs::write(dir.join(name), doc.to_string()).unwrap();
    };

    // 1. Open store with landed binary and ingest A (initializes aggregates + trust stamp).
    let engine = open(&db);
    write_run("a.json", "vm.h2--a", 1000);
    pass(&engine, &share, T0 + 3 * DAY, 1000, true);

    // Capture the post-A aggregate snapshot (tables + `sb_agg_*` meta) to simulate an older
    // binary that does not update `run_sb_*` or `sb_agg_*` when committing B.
    let saved_after_a = engine
        .with_db(|db_ref| {
            let read = db_ref.begin_read()?;
            let dump_bytes =
                |def: redb::TableDefinition<&[u8], &[u8]>| -> aac_store::error::Result<Vec<(Vec<u8>, Vec<u8>)>> {
                    let t = read.open_table(def)?;
                    t.iter()?
                        .map(|i| {
                            let (k, v) = i?;
                            Ok((k.value().to_vec(), v.value().to_vec()))
                        })
                        .collect()
                };
            let dump_idx = |def: redb::TableDefinition<&[u8], ()>| -> aac_store::error::Result<Vec<Vec<u8>>> {
                let t = read.open_table(def)?;
                t.iter()?.map(|i| Ok(i?.0.value().to_vec())).collect()
            };
            let meta_tbl = read.open_table(runs::RUN_META)?;
            let mut sb_meta = Vec::new();
            for item in meta_tbl.iter()? {
                let (k, v) = item?;
                if k.value().starts_with("sb_agg_") {
                    sb_meta.push((k.value().to_string(), v.value().to_vec()));
                }
            }
            Ok((
                dump_bytes(scoreboard_agg::RUN_SB_FACT)?,
                dump_bytes(scoreboard_agg::RUN_SB_BUCKET)?,
                dump_bytes(scoreboard_agg::RUN_SB_CORRECTION_REFUSED)?,
                dump_idx(scoreboard_agg::RUN_SB_REVIEWS_BY_TARGET)?,
                dump_idx(scoreboard_agg::RUN_SB_CORRECTIONS)?,
                dump_idx(scoreboard_agg::RUN_SB_PATH_RUNS)?,
                dump_idx(scoreboard_agg::RUN_SB_BY_WRITER_DAY)?,
                sb_meta,
            ))
        })
        .unwrap();

    // 2. Simulate rollback to old binary: ingest B so primary tables + ledger advance to B, then
    // rewind ONLY `run_sb_*` and `sb_agg_*` back to post-A (exactly what old binary leaves).
    write_run("b.json", "vm.h2--b", 2000);
    pass(&engine, &share, T0 + 3 * DAY + 5000, 1000, true);
    engine
        .with_db(|db_ref| {
            let txn = aac_store::engine::begin(db_ref)?;
            scoreboard_agg::delete_tables(&txn)?;
            scoreboard_agg::create_tables(&txn)?;
            {
                let mut t = txn.open_table(scoreboard_agg::RUN_SB_FACT)?;
                for (k, v) in &saved_after_a.0 {
                    t.insert(k.as_slice(), v.as_slice())?;
                }
            }
            {
                let mut t = txn.open_table(scoreboard_agg::RUN_SB_BUCKET)?;
                for (k, v) in &saved_after_a.1 {
                    t.insert(k.as_slice(), v.as_slice())?;
                }
            }
            {
                let mut t = txn.open_table(scoreboard_agg::RUN_SB_CORRECTION_REFUSED)?;
                for (k, v) in &saved_after_a.2 {
                    t.insert(k.as_slice(), v.as_slice())?;
                }
            }
            {
                let mut t = txn.open_table(scoreboard_agg::RUN_SB_REVIEWS_BY_TARGET)?;
                for k in &saved_after_a.3 {
                    t.insert(k.as_slice(), ())?;
                }
            }
            {
                let mut t = txn.open_table(scoreboard_agg::RUN_SB_CORRECTIONS)?;
                for k in &saved_after_a.4 {
                    t.insert(k.as_slice(), ())?;
                }
            }
            {
                let mut t = txn.open_table(scoreboard_agg::RUN_SB_PATH_RUNS)?;
                for k in &saved_after_a.5 {
                    t.insert(k.as_slice(), ())?;
                }
            }
            {
                let mut t = txn.open_table(scoreboard_agg::RUN_SB_BY_WRITER_DAY)?;
                for k in &saved_after_a.6 {
                    t.insert(k.as_slice(), ())?;
                }
            }
            {
                let mut meta = txn.open_table(runs::RUN_META)?;
                let old_keys: Vec<String> = meta
                    .iter()?
                    .map(|i| Ok(i?.0.value().to_string()))
                    .collect::<aac_store::error::Result<_>>()?;
                for k in old_keys {
                    if k.starts_with("sb_agg_") {
                        meta.remove(k.as_str())?;
                    }
                }
                for (k, v) in &saved_after_a.7 {
                    meta.insert(k.as_str(), v.as_slice())?;
                }
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
    engine.close().unwrap();
    drop(engine);

    // Snapshot the post-B (lagging-aggregate) redb file before appending C to the ledger.
    let saved_redb = db.join("aac.redb.post-b-old-bin");
    fs::copy(db.join("aac.redb"), &saved_redb).unwrap();

    // 3. Ingest C so C is durably written to the ledger, then restore the post-B redb file so C
    // sits in the pending ledger tail (crash after ledger sync, before redb commit).
    let engine2 = open(&db);
    write_run("c.json", "vm.h2--c", 3000);
    pass(&engine2, &share, T0 + 3 * DAY + 10_000, 1000, true);
    engine2.close().unwrap();
    drop(engine2);
    fs::rename(&saved_redb, db.join("aac.redb")).unwrap();

    // 4. Upgrade / reopen: replay applies C from ledger tail, then `finish_replay` verifies the
    // trust stamp and MUST rebuild all three runs (A, B, and C), never certifying a missing B!
    let upgraded = open(&db);
    let agg = scoreboard_agg::read_for_scoreboard(
        &upgraded,
        &ScoreboardFilter::default(),
        &[],
        &std::collections::HashMap::new(),
    )
    .unwrap();
    let resolved = runs::resolved_history_snapshot(&upgraded).unwrap();
    assert_eq!(resolved.facts().len(), 3, "primary history has A, B, C");
    assert_eq!(
        agg.effective_runs_total, 3,
        "rebuilt aggregate after rollback + replay tail must include A, B, and C"
    );
    let build_n: u64 = agg.buckets.iter().map(|(_, b)| b.n).sum();
    assert_eq!(build_n, 3, "bucket count must include B, not omit it");

    // 5. Pre-serve trust check: tamper `sb_agg_head_digest` and wipe buckets without reopening;
    // `read_for_scoreboard` must detect the mismatch before serving and rebuild cleanly.
    upgraded
        .with_db(|db_ref| {
            let txn = aac_store::engine::begin(db_ref)?;
            txn.open_table(scoreboard_agg::RUN_SB_BUCKET)?;
            txn.delete_table(scoreboard_agg::RUN_SB_BUCKET)?;
            txn.open_table(scoreboard_agg::RUN_SB_BUCKET)?;
            txn.open_table(runs::RUN_META)?.insert(
                "sb_agg_head_digest",
                b"0000000000000000000000000000000000000000000000000000000000000000".as_slice(),
            )?;
            txn.commit()?;
            Ok(())
        })
        .unwrap();
    let repaired = scoreboard_agg::read_for_scoreboard(
        &upgraded,
        &ScoreboardFilter::default(),
        &[],
        &std::collections::HashMap::new(),
    )
    .unwrap();
    assert_eq!(repaired.effective_runs_total, 3);
    let repaired_n: u64 = repaired.buckets.iter().map(|(_, b)| b.n).sum();
    assert_eq!(repaired_n, 3, "pre-serve trust mismatch must trigger rebuild");

    // 6. Legacy v1 -> v2 upgrade path: seed legacy v1 `run_sb_path_hash` (&[u8] value) and
    // `run_sb_bucket_members` tables with `sb_agg_version = "1"` and reopen; upgrade must drop
    // legacy v1 tables cleanly without TableTypeMismatch and rebuild v2 aggregates.
    upgraded
        .with_db(|db_ref| {
            const LEGACY_PATH: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("run_sb_path_hash");
            const LEGACY_MEMBERS: redb::TableDefinition<&[u8], ()> =
                redb::TableDefinition::new("run_sb_bucket_members");
            let txn = aac_store::engine::begin(db_ref)?;
            txn.open_table(LEGACY_PATH)?
                .insert([0u8; 16].as_slice(), [0u8; 4].as_slice())?;
            txn.open_table(LEGACY_MEMBERS)?.insert([0u8; 8].as_slice(), ())?;
            txn.open_table(runs::RUN_META)?
                .insert("sb_agg_version", b"1".as_slice())?;
            txn.commit()?;
            Ok(())
        })
        .unwrap();
    upgraded.close().unwrap();
    drop(upgraded);
    let migrated_v2 = open(&db);
    let agg_v2 = scoreboard_agg::read_for_scoreboard(
        &migrated_v2,
        &ScoreboardFilter::default(),
        &[],
        &std::collections::HashMap::new(),
    )
    .unwrap();
    assert_eq!(agg_v2.effective_runs_total, 3);
    assert_eq!(agg_v2.buckets.iter().map(|(_, b)| b.n).sum::<u64>(), 3);
    migrated_v2.close().unwrap();
    drop(migrated_v2);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// Property-style test: ingest a deterministic pseudo-random sequence of runs, revisions,
/// corrections (add/remove/retarget/void/refused), reviews (in-order/out-of-order/retargeted),
/// same-path replacements, path refusals, and catalog rekeys, asserting aggregate totals match
/// the full `ResolvedHistory` resolver after EVERY step.
#[test]
fn property_random_sequence_of_runs_corrections_reviews_refusals_matches_full_resolver() {
    use aac_store::scoreboard_agg::{self, ScoreboardFilter};
    use std::collections::{BTreeMap, BTreeSet};

    struct Rng(u64);
    impl Rng {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x
        }
        fn pick(&mut self, n: usize) -> usize {
            (self.next() as usize) % n
        }
    }

    let mut rng = Rng(0x5eed_cafe_beef_1234);
    let share = Share::new("sb-agg-prop");
    let writers = ["vm.p1", "vm.p2"];
    let models = ["openai/gpt-6.1-sol", "anthropic/claude-opus-5-5", "raw-alias"];
    let task_types = ["build", "test", "fix"];
    let day = &iso(T0 + 2 * DAY)[..10];
    for w in writers {
        fs::create_dir_all(share.root.join("v1/writers").join(w).join("runs").join(day)).unwrap();
    }

    let db = temp_dir("sb-agg-prop-store");
    let mut engine = open(&db);

    #[derive(Clone)]
    struct KnownRun {
        id: String,
        writer: String,
        path: String,
        seq: u64,
        ended_ms: u64,
        is_review: bool,
    }
    let mut known: Vec<KnownRun> = Vec::new();
    let mut next_id = 0usize;
    let mut clock = T0 + 3 * DAY;

    let assert_agg_equals_resolver = |eng: &Engine, step: usize| {
        let resolved = runs::resolved_history_snapshot(eng).unwrap();
        let all_rows = resolved.facts();
        let filters = [
            ("all", ScoreboardFilter::default()),
            (
                "observed",
                ScoreboardFilter {
                    source_benchmark: Some(false),
                    ..Default::default()
                },
            ),
            (
                "benchmark",
                ScoreboardFilter {
                    source_benchmark: Some(true),
                    ..Default::default()
                },
            ),
            (
                "build",
                ScoreboardFilter {
                    task_type: Some("build".to_string()),
                    ..Default::default()
                },
            ),
            (
                "review",
                ScoreboardFilter {
                    task_type: Some("review".to_string()),
                    ..Default::default()
                },
            ),
            (
                "narrow-window",
                ScoreboardFilter {
                    from_ms: Some(T0 + 2 * DAY + 100_000),
                    to_ms: Some(T0 + 2 * DAY + 500_000),
                    ..Default::default()
                },
            ),
        ];

        for (fname, filter) in &filters {
            let agg = scoreboard_agg::read_for_scoreboard(eng, filter, &[], &std::collections::HashMap::new()).unwrap();
            assert_eq!(
                agg.effective_runs_total,
                all_rows.len() as u64,
                "step {step} ({fname}): effective_runs_total"
            );

            // Filter resolved rows identically.
            let matched: Vec<_> = all_rows
                .iter()
                .filter(|r| {
                    filter.from_ms.is_none_or(|f| r.cursor.0 >= f)
                        && filter.to_ms.is_none_or(|t| r.cursor.0 <= t)
                        && filter.source_benchmark.is_none_or(|b| r.source.is_benchmark() == b)
                        && filter
                            .task_type
                            .as_deref()
                            .is_none_or(|tt| r.task_type.as_deref() == Some(tt))
                })
                .collect();

            let agg_n: u64 = agg.buckets.iter().map(|(_, b)| b.n).sum();
            assert_eq!(agg_n, matched.len() as u64, "step {step} ({fname}): filtered n");

            let res_done: u64 = matched.iter().filter(|r| &*r.outcome == "done").count() as u64;
            let agg_done: u64 = agg.buckets.iter().map(|(_, b)| b.outcome_done).sum();
            assert_eq!(agg_done, res_done, "step {step} ({fname}): done count");

            // Verify cost credits sum in exact file-derived `(cursor_ms, run_id)` order.
            let mut matched_sorted: Vec<&runs::RunFact> = matched.iter().map(|r| r.as_ref()).collect();
            matched_sorted.sort_by(|a, b| a.cursor.0.cmp(&b.cursor.0).then_with(|| a.id.cmp(&b.id)));
            let matched_docs = runs::load_run_docs(eng, &matched_sorted).unwrap();
            let mut res_credits = 0.0f64;
            let mut res_credit_n = 0u64;
            for doc_opt in &matched_docs {
                if let Some(c) = doc_opt.as_ref().and_then(|d| d["cost"]["credits"].as_f64()) {
                    res_credits += c;
                    res_credit_n += 1;
                }
            }
            let mut agg_samples: Vec<_> = agg.buckets.iter().flat_map(|(_, b)| b.cost_samples.iter()).collect();
            agg_samples.sort_by(|a, b| a.cursor_ms.cmp(&b.cursor_ms).then_with(|| a.run_id.cmp(&b.run_id)));
            let mut agg_credits = 0.0f64;
            let mut agg_credit_n = 0u64;
            for s in agg_samples {
                if let Some(c) = s.cost.credits {
                    agg_credits += c;
                    agg_credit_n += 1;
                }
            }
            assert_eq!(agg_credit_n, res_credit_n, "step {step} ({fname}): credit n");
            assert_eq!(
                agg_credits.to_bits(),
                res_credits.to_bits(),
                "step {step} ({fname}): IEEE-754 credit sum mismatch ({agg_credits} vs {res_credits})"
            );

            // Verify joined reviews targeting matched runs match resolved effective reviews.
            let matched_ids: BTreeSet<&str> = matched.iter().map(|r| r.id.as_ref()).collect();
            let mut res_joins: BTreeMap<(String, String), usize> = BTreeMap::new();
            for r in all_rows {
                if let Some(target) = r.review().map(|rev| rev.of.as_ref())
                    && matched_ids.contains(target)
                {
                    *res_joins.entry((target.to_string(), r.id.to_string())).or_insert(0) += 1;
                }
            }
            let mut agg_joins: BTreeMap<(String, String), usize> = BTreeMap::new();
            for (_, b) in &agg.buckets {
                for jr in &b.joined_reviews {
                    *agg_joins
                        .entry((jr.target_id.clone(), jr.review_id.clone()))
                        .or_insert(0) += 1;
                }
            }
            assert_eq!(agg_joins, res_joins, "step {step} ({fname}): joined reviews");

            // Verify correction problems match `resolved.problems()`.
            let res_probs: BTreeSet<(String, String)> = resolved
                .problems()
                .iter()
                .map(|p| {
                    (
                        p["file"].as_str().unwrap_or_default().to_string(),
                        p["reason"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect();
            let agg_probs: BTreeSet<(String, String)> = agg
                .correction_problems
                .iter()
                .map(|p| (p.file.clone(), p.reason.clone()))
                .collect();
            assert_eq!(agg_probs, res_probs, "step {step} ({fname}): correction problems");

            // Verify every refused file with a retained effective run in `all_rows` resolves its path.
            let retained_by_hash: BTreeMap<String, String> = all_rows
                .iter()
                .map(|r| {
                    use sha2::{Digest, Sha256};
                    let d = Sha256::digest(format!("aac-run-path-v1:{}", r.file).as_bytes());
                    let h: String = d.iter().take(16).map(|b| format!("{b:02x}")).collect();
                    (h, r.file.to_string())
                })
                .collect();
            for rf in &agg.refused {
                assert_eq!(
                    rf.retained_file.as_deref(),
                    retained_by_hash.get(&rf.path_hash).map(String::as_str),
                    "step {step} ({fname}): refused retained_file for {}",
                    rf.path_hash
                );
            }
        }
    };

    for step in 0..70usize {
        clock += 2000;
        let op = if known.len() < 4 { 0 } else { rng.pick(10) };
        match op {
            // 0..3: brand-new plain run (random out-of-order endedAt, optional cost)
            0..=3 => {
                let w = writers[rng.pick(writers.len())];
                let id = format!("{w}--run-{next_id}");
                next_id += 1;
                let ended_ms = T0 + 2 * DAY + (rng.pick(800) as u64) * 1000;
                let source = if rng.pick(5) == 0 { "benchmark" } else { "observed" };
                let tt = task_types[rng.pick(task_types.len())];
                let model = models[rng.pick(models.len())];
                let outcome = if rng.pick(4) == 0 { "failed" } else { "done" };
                let path = format!("v1/writers/{w}/runs/{day}/{id}.json");
                let mut doc = json!({
                    "schema": "agent-reports/run@1",
                    "id": id,
                    "writer": w,
                    "seq": 1,
                    "writtenAt": iso(ended_ms),
                    "source": source,
                    "task": { "id": format!("t-{next_id}"), "type": tt, "title": "Prop", "size": { "tasks": 1 } },
                    "model": { "id": model, "provider": "openai", "harness": "codex", "effort": "high" },
                    "endedAt": iso(ended_ms),
                    "outcome": outcome,
                    "cost": { "credits": ((rng.pick(500) + 1) as f64) * 0.00015 }
                });
                if source == "benchmark" {
                    doc["benchmark"] = json!("bench-a");
                }
                fs::write(share.root.join(&path), doc.to_string()).unwrap();
                known.push(KnownRun {
                    id,
                    writer: w.to_string(),
                    path,
                    seq: 1,
                    ended_ms,
                    is_review: false,
                });
            }
            // 4: brand-new review targeting a known run OR a not-yet-arrived future run
            4 => {
                let w = writers[rng.pick(writers.len())];
                let id = format!("{w}--rev-{next_id}");
                next_id += 1;
                let target_id = if rng.pick(4) == 0 {
                    format!("{w}--run-{}", next_id + 1)
                } else {
                    known[rng.pick(known.len())].id.clone()
                };
                let ended_ms = T0 + 2 * DAY + (rng.pick(800) as u64) * 1000 + 500;
                let path = format!("v1/writers/{w}/runs/{day}/{id}.json");
                let doc = json!({
                    "schema": "agent-reports/run@1",
                    "id": id,
                    "writer": w,
                    "seq": 1,
                    "writtenAt": iso(ended_ms),
                    "source": "observed",
                    "task": { "id": format!("tr-{next_id}"), "type": "review", "title": "Rev" },
                    "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
                    "endedAt": iso(ended_ms),
                    "outcome": "done",
                    "review": {
                        "of": target_id,
                        "verdict": if rng.pick(3) == 0 { "fail" } else { "pass" },
                        "findings": { "high": 0, "medium": 0, "low": 0 },
                        "overturned": rng.pick(4) == 0,
                        "disputed": rng.pick(5) == 0
                    }
                });
                fs::write(share.root.join(&path), doc.to_string()).unwrap();
                known.push(KnownRun {
                    id,
                    writer: w.to_string(),
                    path,
                    seq: 1,
                    ended_ms,
                    is_review: true,
                });
            }
            // 5: brand-new correction (valid same-writer, cross-writer refused, void, or missing target)
            5 => {
                let target = known[rng.pick(known.len())].clone();
                let w = if rng.pick(4) == 0 {
                    writers[rng.pick(writers.len())].to_string()
                } else {
                    target.writer.clone()
                };
                let id = format!("{w}--corr-{next_id}");
                next_id += 1;
                let corr_target = if rng.pick(5) == 0 {
                    format!("{w}--missing-{next_id}")
                } else {
                    target.id.clone()
                };
                let ended_ms = target.ended_ms + 1000;
                let path = format!("v1/writers/{w}/runs/{day}/{id}.json");
                let mut doc = json!({
                    "schema": "agent-reports/run@1",
                    "id": id,
                    "writer": w,
                    "seq": 1,
                    "writtenAt": iso(ended_ms),
                    "source": "observed",
                    "corrects": corr_target,
                    "task": { "id": format!("tc-{next_id}"), "type": "build", "title": "Corr", "size": { "tasks": 1 } },
                    "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
                    "endedAt": iso(ended_ms),
                    "outcome": "done",
                    "cost": { "credits": 0.01 }
                });
                if rng.pick(3) == 0 {
                    doc["void"] = json!({ "reason": "duplicate" });
                }
                fs::write(share.root.join(&path), doc.to_string()).unwrap();
                known.push(KnownRun {
                    id,
                    writer: w,
                    path,
                    seq: 1,
                    ended_ms,
                    is_review: false,
                });
            }
            // 6: higher-seq revision of an existing run (toggle/retarget `corrects` or `review.of`)
            6 => {
                let idx = rng.pick(known.len());
                let mut entry = known[idx].clone();
                entry.seq += 1;
                let new_ended = entry.ended_ms + 500;
                let mut doc = json!({
                    "schema": "agent-reports/run@1",
                    "id": entry.id,
                    "writer": entry.writer,
                    "seq": entry.seq,
                    "writtenAt": iso(new_ended),
                    "source": "observed",
                    "task": {
                        "id": format!("trev-{}", entry.seq),
                        "type": if entry.is_review { "review" } else { "build" },
                        "title": "Revised",
                        "size": { "tasks": 1 }
                    },
                    "model": { "id": "anthropic/claude-opus-5-5", "provider": "anthropic", "harness": "claude-code", "effort": "high" },
                    "endedAt": iso(new_ended),
                    "outcome": "done",
                    "cost": { "credits": 0.002 }
                });
                if entry.is_review {
                    let new_target = known[rng.pick(known.len())].id.clone();
                    doc["review"] = json!({
                        "of": new_target,
                        "verdict": "pass",
                        "findings": { "high": 0, "medium": 0, "low": 0 },
                        "overturned": false,
                        "disputed": false
                    });
                } else if rng.pick(2) == 0 {
                    let cand = known[rng.pick(known.len())].clone();
                    if cand.id != entry.id && cand.writer == entry.writer {
                        doc["corrects"] = json!(cand.id);
                    }
                }
                fs::write(share.root.join(&entry.path), doc.to_string()).unwrap();
                entry.ended_ms = new_ended;
                known[idx] = entry;
            }
            // 7: overwrite an EXISTING run's file path with a DIFFERENT run ID (H4 trigger!)
            7 => {
                let victim = known[rng.pick(known.len())].clone();
                let id = format!("{}--samepath-{next_id}", victim.writer);
                next_id += 1;
                let ended_ms = T0 + 2 * DAY + (rng.pick(800) as u64) * 1000;
                let tt = task_types[rng.pick(task_types.len())];
                let doc = json!({
                    "schema": "agent-reports/run@1",
                    "id": id,
                    "writer": victim.writer,
                    "seq": 1,
                    "writtenAt": iso(ended_ms),
                    "source": "observed",
                    "task": { "id": format!("tsp-{next_id}"), "type": tt, "title": "Same path", "size": { "tasks": 1 } },
                    "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
                    "endedAt": iso(ended_ms),
                    "outcome": "done"
                });
                fs::write(share.root.join(&victim.path), doc.to_string()).unwrap();
                known.push(KnownRun {
                    id,
                    writer: victim.writer,
                    path: victim.path,
                    seq: 1,
                    ended_ms,
                    is_review: false,
                });
            }
            // 8: corrupt/refuse either a known path OR a brand-new unknown path
            8 => {
                if rng.pick(2) == 0 {
                    let victim = &known[rng.pick(known.len())];
                    fs::write(share.root.join(&victim.path), "{not-valid-json").unwrap();
                } else {
                    let w = writers[rng.pick(writers.len())];
                    let path = format!("v1/writers/{w}/runs/{day}/bad-{next_id}.json");
                    next_id += 1;
                    fs::write(share.root.join(&path), "{bad").unwrap();
                }
            }
            // 9: catalog alias change (rekey!)
            _ => {
                let target = if step % 2 == 0 {
                    "openai/gpt-6.1-sol"
                } else {
                    "anthropic/claude-opus-5-5"
                };
                share.set_aliases(&[("raw-alias", target)]);
            }
        }

        pass(&engine, &share, clock, 1000, true);
        assert_agg_equals_resolver(&engine, step);

        if step == 35 {
            engine.close().unwrap();
            drop(engine);
            engine = open(&db);
            assert_agg_equals_resolver(&engine, step);
        }
    }

    engine.close().unwrap();
    drop(engine);
    let final_eng = open(&db);
    assert_agg_equals_resolver(&final_eng, 999);
    final_eng.close().unwrap();
    drop(final_eng);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// Phase 4, step 3: `ResolvedHistory` holds compact typed `RunFact` rows (~0.5 KB each, zero
/// `doc_json`), while full run documents stay in redb `RUN_PAYLOAD` and load on demand.
#[test]
fn run_facts_are_typed_compact_and_load_documents_on_demand() {
    use std::sync::Arc;

    assert!(
        std::mem::size_of::<runs::RunFact>() <= 384,
        "RunFact inline size must stay compact (got {} bytes)",
        std::mem::size_of::<runs::RunFact>()
    );

    let share = Share::new("run-facts-typed");
    let day = &iso(T0 + 2 * DAY)[..10];
    let dir = share.root.join("v1/writers/vm.facts/runs").join(day);
    fs::create_dir_all(&dir).unwrap();

    // Build a large (~8 KB) prose-heavy run document so retaining doc text would blow past 0.5 KB.
    let big_detail = "detailed-finding-prose-".repeat(35);
    let big_evidence: Vec<Json> = (0..12)
        .map(|i| {
            json!({
                "label": format!("artifact-{i}-{}", "x".repeat(80)),
                "path": format!("/mnt/Cursor/share/example/artifact-{i}-{}.md", "y".repeat(120))
            })
        })
        .collect();
    let orig = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.facts--target-1",
        "writer": "vm.facts",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 1_000),
        "source": "observed",
        "project": "aac",
        "stream": "PERF",
        "task": {
            "id": "task-1",
            "type": "build",
            "title": "Heavy prose build run",
            "size": { "records": 120, "files": 7, "items": 3, "words": 4200, "tasks": 2 }
        },
        "model": { "id": "gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
        "process": { "id": "phase4", "version": "1.2" },
        "startedAt": iso(T0 + 2 * DAY),
        "endedAt": iso(T0 + 2 * DAY + 1_000),
        "wallSeconds": 42.5,
        "outcome": "failed",
        "imported": { "from": "legacy.jsonl", "at": iso(T0 + 2 * DAY + 500) },
        "flags": [{ "kind": "shortcut", "detail": big_detail, "evidence": "/tmp/e.txt", "reportedBy": "vm.rev" }],
        "evidence": big_evidence
    });
    let fix = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.facts--target-1-fix",
        "writer": "vm.facts",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 2_000),
        "source": "observed",
        "corrects": "vm.facts--target-1",
        "project": "aac",
        "stream": "PERF",
        "task": {
            "id": "task-1",
            "type": "build",
            "title": "Heavy prose build run fixed",
            "size": { "records": 120, "files": 7, "items": 3, "words": 4200, "tasks": 2 }
        },
        "model": { "id": "gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
        "process": { "id": "phase4", "version": "1.2" },
        "startedAt": iso(T0 + 2 * DAY),
        "endedAt": iso(T0 + 2 * DAY + 2_000),
        "wallSeconds": 42.5,
        "outcome": "done",
        "imported": { "from": "legacy.jsonl", "at": iso(T0 + 2 * DAY + 500) },
        "flags": [{ "kind": "shortcut", "detail": big_detail, "evidence": "/tmp/e.txt", "reportedBy": "vm.rev" }],
        "evidence": big_evidence
    });
    let rev = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.facts--rev-1",
        "writer": "vm.facts",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 3_000),
        "source": "benchmark",
        "benchmark": "bench-suite",
        "task": { "id": "task-rev-1", "type": "review", "title": "Review target-1" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "max" },
        "endedAt": iso(T0 + 2 * DAY + 3_000),
        "outcome": "done",
        "review": {
            "of": "vm.facts--target-1",
            "verdict": "pass",
            "findings": { "high": 0, "medium": 1, "low": 2 },
            "overturned": true,
            "disputed": false
        }
    });
    fs::write(dir.join("vm.facts--target-1.json"), orig.to_string()).unwrap();
    fs::write(dir.join("vm.facts--target-1-fix.json"), fix.to_string()).unwrap();
    fs::write(dir.join("vm.facts--rev-1.json"), rev.to_string()).unwrap();

    let db = temp_dir("run-facts-typed-store");
    let engine = open(&db);
    pass(&engine, &share, T0 + 3 * DAY, 1000, true);

    let snap = runs::resolved_history_snapshot(&engine).unwrap();
    assert_eq!(
        snap.facts().len(),
        2,
        "target-1 is superseded; target-1-fix and rev-1 survive"
    );
    assert_eq!(snap.accepted_facts().len(), 3, "all 3 non-refused runs are accepted");

    let fix_fact: &Arc<runs::RunFact> = snap
        .facts()
        .iter()
        .find(|f| &*f.id == "vm.facts--target-1-fix")
        .expect("fix fact");
    assert!(
        fix_fact.estimated_bytes() <= 640,
        "prose-heavy run fact must stay ~0.5 KB, not hold doc text (got {} bytes)",
        fix_fact.estimated_bytes()
    );
    assert_eq!(&*fix_fact.writer, "vm.facts");
    assert_eq!(fix_fact.model_id.as_deref(), Some("openai/gpt-6.1-sol"));
    assert_eq!(fix_fact.model_raw.as_deref(), Some("gpt-6.1-sol"));
    assert_eq!(fix_fact.effort.as_deref(), Some("high"));
    assert_eq!(fix_fact.harness.as_deref(), Some("codex"));
    assert_eq!(fix_fact.task_type.as_deref(), Some("build"));
    assert_eq!(fix_fact.task_size(), [Some(120), Some(7), Some(3), Some(4200), Some(2)]);
    assert_eq!(&*fix_fact.outcome, "done");
    assert_eq!(fix_fact.started_at_ms(), Some(T0 + 2 * DAY));
    assert_eq!(fix_fact.ended_at_ms, T0 + 2 * DAY + 2_000);
    assert_eq!(fix_fact.wall_seconds(), Some(42.5));
    assert_eq!(fix_fact.project.as_deref(), Some("aac"));
    assert_eq!(fix_fact.stream.as_deref(), Some("PERF"));
    assert_eq!(fix_fact.process(), Some(("phase4", Some("1.2"))));
    assert_eq!(fix_fact.corrects(), Some("vm.facts--target-1"));
    assert!(fix_fact.imported());
    assert!(!fix_fact.has_void());
    assert!(!fix_fact.has_review());

    let rev_fact = snap
        .facts()
        .iter()
        .find(|f| &*f.id == "vm.facts--rev-1")
        .expect("review fact");
    assert_eq!(rev_fact.source, runs::RunSource::Benchmark);
    assert_eq!(rev_fact.benchmark(), Some("bench-suite"));
    let link = rev_fact.review().expect("review link");
    assert_eq!(&*link.orig_of, "vm.facts--target-1");
    assert_eq!(&*link.of, "vm.facts--target-1-fix", "retargeted through correction");
    assert_eq!(&*link.verdict, "pass");
    assert!(link.overturned);
    assert!(!link.disputed);

    // Load full documents on demand from redb RUN_PAYLOAD.
    let loaded_fix = runs::load_run_doc(&engine, fix_fact).unwrap().expect("fix doc");
    assert_eq!(loaded_fix["flags"][0]["detail"], json!(big_detail));
    assert_eq!(loaded_fix["evidence"].as_array().unwrap().len(), 12);
    assert_eq!(loaded_fix["model"]["id"], json!("openai/gpt-6.1-sol"));
    assert_eq!(loaded_fix["model"]["provider"], Json::Null);

    let batch_refs: Vec<&runs::RunFact> = snap.facts().iter().map(Arc::as_ref).collect();
    let batch_docs = runs::load_run_docs(&engine, &batch_refs).unwrap();
    assert_eq!(batch_docs.len(), 2);
    let rev_pos = snap.facts().iter().position(|f| &*f.id == "vm.facts--rev-1").unwrap();
    let rev_doc = batch_docs[rev_pos].as_ref().expect("rev doc");
    assert_eq!(rev_doc["review"]["of"], json!("vm.facts--target-1-fix"));

    // A newer revision later recorded must NOT disturb on-demand loading of the older snapshot RunFact.
    let mut fix_v2 = fix.clone();
    fix_v2["seq"] = json!(2);
    fix_v2["task"]["title"] = json!("Revised v2 title");
    fs::write(dir.join("vm.facts--target-1-fix.json"), fix_v2.to_string()).unwrap();
    pass(&engine, &share, T0 + 4 * DAY, 1000, true);
    let old_doc_still = runs::load_run_doc(&engine, fix_fact).unwrap().expect("old rev doc");
    assert_eq!(old_doc_still["task"]["title"], json!("Heavy prose build run fixed"));
    let snap2 = runs::resolved_history_snapshot(&engine).unwrap();
    let new_fix_fact = snap2
        .facts()
        .iter()
        .find(|f| &*f.id == "vm.facts--target-1-fix")
        .unwrap();
    let new_doc = runs::load_run_doc(&engine, new_fix_fact).unwrap().expect("new rev doc");
    assert_eq!(new_doc["task"]["title"], json!("Revised v2 title"));

    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// Phase 4, step 3: generations share unchanged `Arc<RunFact>` instances with zero whole-table
/// copies and zero payload decompression for unchanged runs (and zero payload reads on cold open
/// when `RUN_SB_FACT` is verified).
#[test]
fn run_facts_share_arcs_across_generations_and_avoid_whole_table_reads() {
    use aac_store::scoreboard_agg;
    use std::collections::HashMap;
    use std::sync::Arc;

    let share = Share::new("run-facts-share");
    let day = &iso(T0 + 2 * DAY)[..10];
    let dir = share.root.join("v1/writers/vm.share/runs").join(day);
    fs::create_dir_all(&dir).unwrap();

    for i in 0..40u64 {
        share.run(
            "vm.share",
            &format!("plain-{i:03}"),
            1,
            T0 + 2 * DAY + i * 1_000,
            if i == 0 { "custom-alias" } else { "openai/gpt-6.1-sol" },
            Some("high"),
            Some("codex"),
            Some("build"),
        );
    }
    let fix = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.share--plain-001-fix",
        "writer": "vm.share",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 50_000),
        "source": "observed",
        "corrects": "vm.share--plain-001",
        "task": { "id": "t-fix", "type": "build", "title": "Fix 1" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
        "endedAt": iso(T0 + 2 * DAY + 50_000),
        "outcome": "done"
    });
    let rev = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.share--rev-001",
        "writer": "vm.share",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 60_000),
        "source": "observed",
        "task": { "id": "t-rev", "type": "review", "title": "Rev 1" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
        "endedAt": iso(T0 + 2 * DAY + 60_000),
        "outcome": "done",
        "review": {
            "of": "vm.share--plain-001",
            "verdict": "pass",
            "findings": { "high": 0, "medium": 0, "low": 0 },
            "overturned": false,
            "disputed": false
        }
    });
    fs::write(dir.join("vm.share--plain-001-fix.json"), fix.to_string()).unwrap();
    fs::write(dir.join("vm.share--rev-001.json"), rev.to_string()).unwrap();

    let db = temp_dir("run-facts-share-store");
    let engine = open(&db);
    pass(&engine, &share, T0 + 3 * DAY, 1000, true);

    // 1. Cold build from verified `RUN_SB_FACT` reads ZERO payload blocks.
    let gen1 = runs::resolved_history_snapshot(&engine).unwrap();
    assert_eq!(
        runs::history_payload_read_count(&engine),
        0,
        "cold build with verified RUN_SB_FACT must not decompress RUN_PAYLOAD"
    );
    assert_eq!(gen1.facts().len(), 41);
    assert_eq!(gen1.accepted_facts().len(), 42);

    // Every non-retargeted effective run in `facts()` shares the EXACT same `Arc<RunFact>` with `accepted_facts()`.
    let acc1_by_id: HashMap<&str, &Arc<runs::RunFact>> = gen1.accepted_facts().iter().map(|f| (&*f.id, f)).collect();
    for fact in gen1.facts() {
        if fact.review().is_none_or(|r| r.of == r.orig_of) {
            assert!(
                Arc::ptr_eq(fact, acc1_by_id[&*fact.id]),
                "effective run {} must share Arc between facts() and accepted_facts()",
                fact.id
            );
        }
    }
    // Interned categorical strings share Arc pointers across runs.
    assert!(Arc::ptr_eq(&gen1.facts()[2].writer, &gen1.facts()[3].writer));
    assert!(Arc::ptr_eq(
        gen1.facts()[2].model_id.as_ref().unwrap(),
        gen1.facts()[3].model_id.as_ref().unwrap()
    ));

    // 2. Append 1 brand-new run -> generation moves; every unchanged run keeps the SAME Arc pointer
    //    and ONLY the 1 new run reads RUN_PAYLOAD.
    share.run(
        "vm.share",
        "plain-040",
        1,
        T0 + 2 * DAY + 70_000,
        "openai/gpt-6.1-sol",
        Some("high"),
        Some("codex"),
        Some("build"),
    );
    pass(&engine, &share, T0 + 4 * DAY, 1000, true);
    let gen2 = runs::resolved_history_snapshot(&engine).unwrap();
    assert!(gen2.generation() > gen1.generation());
    assert_eq!(gen2.facts().len(), 42);
    assert_eq!(
        runs::history_payload_read_count(&engine),
        1,
        "adding 1 run must decode at most 1 payload doc, not whole table"
    );
    let g1_by_id: HashMap<&str, &Arc<runs::RunFact>> = gen1.facts().iter().map(|f| (&*f.id, f)).collect();
    let g2_by_id: HashMap<&str, &Arc<runs::RunFact>> = gen2.facts().iter().map(|f| (&*f.id, f)).collect();
    for (id, f1) in &g1_by_id {
        let f2 = g2_by_id[id];
        assert!(
            Arc::ptr_eq(f1, f2),
            "unchanged run {id} must share the exact same Arc<RunFact> across generations"
        );
    }

    // 3. Catalog alias change rekeys ONLY plain-000; unchanged runs still share Arcs, and even
    //    the rekeyed run updates identity from Head+Dict with ZERO additional payload reads!
    share.set_aliases(&[("custom-alias", "openai/gpt-6.1-sol")]);
    pass(&engine, &share, T0 + 5 * DAY, 1000, true);
    let gen3 = runs::resolved_history_snapshot(&engine).unwrap();
    assert_eq!(
        runs::history_payload_read_count(&engine),
        1,
        "catalog rekey must update identity from Head+Dict without decompressing RUN_PAYLOAD"
    );
    let g3_by_id: HashMap<&str, &Arc<runs::RunFact>> = gen3.facts().iter().map(|f| (&*f.id, f)).collect();
    assert_eq!(
        g3_by_id["vm.share--plain-000"].model_id.as_deref(),
        Some("openai/gpt-6.1-sol")
    );
    assert!(!Arc::ptr_eq(
        g2_by_id["vm.share--plain-000"],
        g3_by_id["vm.share--plain-000"]
    ));
    for (id, f2) in &g2_by_id {
        if *id == "vm.share--plain-000" {
            continue;
        }
        assert!(
            Arc::ptr_eq(f2, g3_by_id[id]),
            "non-rekeyed run {id} must keep sharing Arc<RunFact> across catalog change"
        );
    }

    // Keep gen1, gen2, gen3 alive together and verify runsQuery matches exact full docs.
    let q = call(&engine, "runsQuery", json!({ "includeReceipts": true }));
    assert_eq!(q["runs"].as_array().unwrap().len(), 42);
    assert_eq!(q["accepted"].as_array().unwrap().len(), 43);
    drop((gen1, gen2, gen3));

    // 4. Fallback when `RUN_SB_FACT` is wiped/untrusted on a cold cache (AFTER reopen, so
    // `finish_replay` cannot silently rebuild `run_sb_*` before the snapshot runs) must decode
    // from `RUN_PAYLOAD` (positive payload_read_count delta) and match trusted facts identically.
    let trusted_snapshot = runs::resolved_history_snapshot(&engine).unwrap();
    let trusted_facts: Vec<runs::RunFact> = trusted_snapshot.facts().iter().map(|f| (**f).clone()).collect();
    let trusted_accepted: Vec<runs::RunFact> = trusted_snapshot
        .accepted_facts()
        .iter()
        .map(|f| (**f).clone())
        .collect();
    drop(trusted_snapshot);
    engine.close().unwrap();
    drop(engine);

    let reopened = open(&db);
    // Wipe `run_sb_*` AFTER reopen while `HistoryCache` is still completely cold.
    reopened
        .with_db(|db_ref| {
            let txn = aac_store::engine::begin(db_ref)?;
            scoreboard_agg::delete_tables(&txn)?;
            scoreboard_agg::create_tables(&txn)?;
            {
                let mut meta = txn.open_table(runs::RUN_META)?;
                meta.remove("sb_agg_version")?;
            }
            txn.commit()?;
            Ok(())
        })
        .unwrap();
    assert_eq!(runs::history_payload_read_count(&reopened), 0);
    let gen_fallback = runs::resolved_history_snapshot(&reopened).unwrap();
    assert_eq!(
        runs::history_payload_read_count(&reopened),
        43,
        "cold untrusted fallback must decode all 43 heads from RUN_PAYLOAD (M4)"
    );
    assert_eq!(gen_fallback.facts().len(), 42);
    assert_eq!(gen_fallback.accepted_facts().len(), 43);
    let fallback_facts: Vec<runs::RunFact> = gen_fallback.facts().iter().map(|f| (**f).clone()).collect();
    let fallback_accepted: Vec<runs::RunFact> = gen_fallback.accepted_facts().iter().map(|f| (**f).clone()).collect();
    assert_eq!(
        fallback_facts, trusted_facts,
        "cold payload fallback facts must match trusted RUN_SB_FACT facts"
    );
    assert_eq!(
        fallback_accepted, trusted_accepted,
        "cold payload fallback accepted receipts must match trusted path"
    );
    let rev_fb = gen_fallback
        .facts()
        .iter()
        .find(|f| &*f.id == "vm.share--rev-001")
        .unwrap();
    assert_eq!(&*rev_fb.review().unwrap().of, "vm.share--plain-001-fix");

    reopened.close().unwrap();
    drop(reopened);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// Deep review Astra H3: an upgraded release-1 ledger containing `"corrects": null` must still
/// project cleanly (not drop to zero runs), and any unprojected head must NEVER be acknowledged
/// as accepted by `accepted_receipts` without disclosing a projection problem.
#[test]
fn deep_h6_upgraded_store_handles_legacy_null_corrects_and_never_acks_unprojected_heads() {
    use aac_store::engine::LogId;
    use aac_store::event::Event;
    use aac_store::msgpack;
    use aac_store::scoreboard_agg;

    let make_recorded_event = |run_id: &str, path: &str, doc_text: &str, ended_ms: u64| -> Event {
        let sha = runs::sha256(doc_text.as_bytes());
        let sha_hex: String = sha.iter().map(|b| format!("{b:02x}")).collect();
        let payload = json!({
            "eventId": format!("agent-report:run:{run_id}:{sha_hex}"),
            "path": path,
            "size": doc_text.len() as u64,
            "mtimeMs": ended_ms,
            "sha256": sha_hex,
            "ingestedAtMs": ended_ms + 1_000,
            "doc": doc_text,
            "ids": {
                "runId": run_id,
                "writer": "vm.upgrade",
                "machine": "vm",
                "computer": "ubuntu",
                "taskId": "task-up",
                "taskType": "build",
                "t3ThreadId": null,
                "t3ThreadSource": null,
                "t3TaskId": null,
                "projectId": "aac",
                "streamId": null,
                "provider": "openai",
                "accountRaw": null,
                "eventAtMs": ended_ms,
                "observedAtMs": ended_ms,
                "startedAtMs": null,
                "wallMs": 60000,
                "seq": 1,
                "outcome": "done",
                "source": "observed",
                "benchmark": null,
                "reviewVerdict": null,
                "overturned": false,
                "disputed": false,
                "imported": false,
                "unknownModel": false
            },
            "identity": {
                "modelRaw": "openai/gpt-6.1-sol",
                "modelId": "openai/gpt-6.1-sol",
                "modelSource": "canonical",
                "modelFamily": "openai",
                "effortRaw": "high",
                "effort": "high",
                "effortSource": "written",
                "effortConflict": false,
                "harnessRaw": "codex",
                "harness": "codex",
                "harnessSource": "written",
                "modelCatalogHash": "0".repeat(64)
            }
        });
        let mut e = Event::new("run.recorded", ended_ms + 1_000, msgpack::from_json(&payload).unwrap());
        e.idempotency_key = Some(format!("agent-report:run:{run_id}:{sha_hex}"));
        e
    };

    // Part A: pre-a23d1f47c stored run with extra `"corrects": null`.
    let db_a = temp_dir("deep-h6-legacy-null-corrects");
    {
        let eng = open(&db_a);
        let legacy_doc = json!({
            "schema": "agent-reports/run@1",
            "id": "vm.upgrade--legacy-1",
            "writer": "vm.upgrade",
            "seq": 1,
            "writtenAt": iso(T0 + DAY),
            "source": "observed",
            "project": "aac",
            "task": { "id": "t-1", "type": "build", "title": "Legacy run" },
            "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
            "endedAt": iso(T0 + DAY),
            "wallSeconds": 60,
            "outcome": "done",
            "corrects": null
        })
        .to_string();
        let ev = make_recorded_event(
            "vm.upgrade--legacy-1",
            "writers/vm.upgrade/runs/2026-11-02/vm.upgrade--legacy-1.json",
            &legacy_doc,
            T0 + DAY,
        );
        eng.commit(LogId::Ledger, |batch| {
            batch.push(ev)?;
            Ok(())
        })
        .unwrap();
        eng.close().unwrap();
    }
    fs::remove_file(db_a.join("aac.redb")).unwrap();
    {
        let upgraded = open(&db_a);
        let sb = scoreboard_agg::read_for_scoreboard(
            &upgraded,
            &scoreboard_agg::ScoreboardFilter::default(),
            &[],
            &std::collections::HashMap::new(),
        )
        .unwrap();
        assert_eq!(
            sb.effective_runs_total, 1,
            "upgraded pre-a23d1f47c run with corrects:null must stay projected, not drop to 0"
        );
        let hist = runs::resolved_history_snapshot(&upgraded).unwrap();
        assert_eq!(hist.facts().len(), 1);
        assert_eq!(hist.skipped(), 0);
        let (_, receipts) = scoreboard_agg::accepted_receipts(&upgraded).unwrap();
        assert_eq!(receipts.len(), 1);
        upgraded.close().unwrap();
    }
    fs::remove_dir_all(db_a).unwrap();

    // Part B: a stored head whose payload genuinely cannot parse must NOT be acknowledged as
    // accepted AND must disclose a projection problem on the aggregate read.
    let db_b = temp_dir("deep-h6-unprojected-head");
    {
        let eng = open(&db_b);
        let unparseable_doc = json!({
            "schema": "agent-reports/run@1",
            "id": "vm.upgrade--broken-1",
            "writer": "vm.upgrade",
            "seq": 1,
            "writtenAt": iso(T0 + DAY),
            "source": "observed",
            "task": { "id": "t-b", "type": "build", "title": "Broken outcome type" },
            "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
            "endedAt": iso(T0 + DAY),
            "outcome": 12345
        })
        .to_string();
        let ev = make_recorded_event(
            "vm.upgrade--broken-1",
            "writers/vm.upgrade/runs/2026-11-02/vm.upgrade--broken-1.json",
            &unparseable_doc,
            T0 + DAY,
        );
        eng.commit(LogId::Ledger, |batch| {
            batch.push(ev)?;
            Ok(())
        })
        .unwrap();
        eng.close().unwrap();
    }
    fs::remove_file(db_b.join("aac.redb")).unwrap();
    {
        let upgraded = open(&db_b);
        let sb = scoreboard_agg::read_for_scoreboard(
            &upgraded,
            &scoreboard_agg::ScoreboardFilter::default(),
            &[],
            &std::collections::HashMap::new(),
        )
        .unwrap();
        assert_eq!(sb.effective_runs_total, 0);
        assert!(
            !sb.correction_problems.is_empty(),
            "an unprojected head must disclose a projection problem instead of staying silent"
        );
        let (_, receipts) = scoreboard_agg::accepted_receipts(&upgraded).unwrap();
        assert!(
            receipts.is_empty(),
            "an unprojected head must NEVER be acknowledged as accepted: {receipts:?}"
        );
        upgraded.close().unwrap();
    }
    fs::remove_dir_all(db_b).unwrap();
}

/// Deep review Astra H7: a revision committed between `resolved_history` row selection and
/// metadata loading in `runsQuery` must never return a torn page mixing seq-1 doc with seq-2 ids.
#[test]
fn deep_h8_corrected_history_query_never_mixes_revisions_across_transactions() {
    use std::sync::Arc;

    let share = Share::new("deep-h8-torn-page");
    let day = &iso(T0 + 2 * DAY)[..10];
    let dir = share.root.join("v1/writers/vm.race/runs").join(day);
    fs::create_dir_all(&dir).unwrap();

    // Unrelated valid correction so `has_corrections` is true and `runsQuery` uses resolved history.
    share.run(
        "vm.race",
        "c-orig",
        1,
        T0 + 2 * DAY + 1_000,
        "openai/gpt-6.1-sol",
        Some("high"),
        Some("codex"),
        Some("build"),
    );
    let c_fix = json!({
        "schema": "agent-reports/run@1",
        "id": "vm.race--c-fix",
        "writer": "vm.race",
        "seq": 1,
        "writtenAt": iso(T0 + 2 * DAY + 2_000),
        "source": "observed",
        "corrects": "vm.race--c-orig",
        "task": { "id": "t-c", "type": "build", "title": "Fix" },
        "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
        "endedAt": iso(T0 + 2 * DAY + 2_000),
        "outcome": "done"
    });
    fs::write(dir.join("vm.race--c-fix.json"), c_fix.to_string()).unwrap();

    // Target run at seq=1 with task.type = "build".
    share.run(
        "vm.race",
        "target",
        1,
        T0 + 2 * DAY + 5_000,
        "openai/gpt-6.1-sol",
        Some("high"),
        Some("codex"),
        Some("build"),
    );

    let db = temp_dir("deep-h8-store");
    let engine = Arc::new(open(&db));
    pass(&engine, &share, T0 + 3 * DAY, 1000, true);
    let gen_before = runs::generation(&engine).unwrap();

    // Between row selection and metadata read in `runsQuery`, commit seq=2 of `vm.race--target`
    // with `task.type = "fix"`.
    let eng_hook = Arc::clone(&engine);
    let share_root = share.root.clone();
    engine.set_before_query_meta_read_hook(Box::new(move || {
        let rev2 = json!({
            "schema": "agent-reports/run@1",
            "id": "vm.race--target",
            "writer": "vm.race",
            "seq": 2,
            "writtenAt": iso(T0 + 2 * DAY + 6_000),
            "source": "observed",
            "project": "proj",
            "task": { "id": "node:delegated-task:command%3Atarget", "type": "fix", "title": "Task target seq2" },
            "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
            "startedAt": iso(T0 + 2 * DAY + 5_000 - 60_000),
            "endedAt": iso(T0 + 2 * DAY + 5_000),
            "wallSeconds": 60,
            "outcome": "done"
        });
        let day_dir = share_root
            .join("v1/writers/vm.race/runs")
            .join(&iso(T0 + 2 * DAY)[..10]);
        fs::write(day_dir.join("vm.race--target.json"), rev2.to_string()).unwrap();
        let scan = call(
            &eng_hook,
            "runsScan",
            json!({ "root": share_root, "budget": 1000, "full": true }),
        );
        call(
            &eng_hook,
            "runsRecord",
            json!({
                "files": scan["files"],
                "refused": scan["refused"],
                "threads": scan["threads"],
                "catalog": scan["catalog"],
                "nowMs": T0 + 4 * DAY
            }),
        );
    }));

    let page = call(&engine, "runsQuery", json!({}));
    let target_row = page["runs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["doc"]["id"].as_str() == Some("vm.race--target"))
        .expect("target row");
    assert_eq!(
        target_row["doc"]["task"]["type"], target_row["ids"]["taskType"],
        "doc.task.type and ids.taskType must come from the same revision (page gen={}, before={gen_before}): {target_row}",
        page["generation"]
    );
    assert_eq!(
        target_row["doc"]["seq"],
        json!(2),
        "retried page must reflect the committed seq=2 revision coherently"
    );
    assert!(
        page["generation"].as_u64().unwrap() > gen_before,
        "retried page must label itself with the newer generation"
    );

    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}

/// Deep review Sol #7 / Astra M1: revising a run repeatedly with distinct interned strings
/// (`process.version`) must release unreferenced strings from `HistoryCache::intern` instead of
/// leaking them for the lifetime of the engine.
#[test]
fn deep_m1_history_interner_evicts_strings_from_retired_revisions() {
    let share = Share::new("deep-m1-intern");
    let day = &iso(T0 + 2 * DAY)[..10];
    let dir = share.root.join("v1/writers/vm.intern/runs").join(day);
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("vm.intern--r1.json");

    let db = temp_dir("deep-m1-store");
    let engine = open(&db);

    let mut base_pool_len = 0usize;
    for seq in 1..=120u64 {
        let doc = json!({
            "schema": "agent-reports/run@1",
            "id": "vm.intern--r1",
            "writer": "vm.intern",
            "seq": seq,
            "writtenAt": iso(T0 + 2 * DAY + seq * 1_000),
            "source": "observed",
            "task": { "id": "t-m1", "type": "scout", "title": "Intern test" },
            "model": { "id": "openai/gpt-6.1-sol", "provider": "openai", "harness": "codex", "effort": "high" },
            "process": { "id": "scout-proc", "version": format!("ver-{seq:04}") },
            "endedAt": iso(T0 + 2 * DAY),
            "outcome": "done"
        });
        fs::write(&path, doc.to_string()).unwrap();
        pass(&engine, &share, T0 + 3 * DAY + seq * 1_000, 1000, true);
        let snap = runs::resolved_history_snapshot(&engine).unwrap();
        assert_eq!(snap.facts().len(), 1);
        drop(snap);
        if seq == 2 {
            base_pool_len = runs::history_intern_pool_len(&engine);
        }
    }
    let final_pool_len = runs::history_intern_pool_len(&engine);
    assert!(
        final_pool_len <= base_pool_len + 2,
        "retired process.version strings leaked in HistoryCache::intern: final={final_pool_len}, base={base_pool_len}"
    );

    engine.close().unwrap();
    drop(engine);
    fs::remove_dir_all(db).unwrap();
    fs::remove_dir_all(share.root).unwrap();
}
