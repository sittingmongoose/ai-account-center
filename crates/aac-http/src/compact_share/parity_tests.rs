#![cfg(test)]
#![allow(clippy::unwrap_used)]

//! Byte-for-byte parity and performance verification for MEM7 Lane 4a compact share readers,
//! `CompactReadersCheckWriter`, and direct compact Status composition (`status_compose`).

use super::catalog::CompactCatalogReader;
use super::check::CompactReadersCheckWriter;
use super::quota::CompactQuotaReader;
use super::registry::{CompactRegistryReader, CompactRegistrySnapshot};
use super::reports::{CompactAgentReportsReader, CompactAgentReportsState};
use super::status_docs::CompactStatusDocsReader;
use crate::status_compose::{
    FactsSource, OwnedReportsView, RegistryInput, ReportsInput, collate_fast, compose_compact,
    spend_by_stream_compact,
};
use aac_files::read::Stat;
use aac_files::registry::{self, Owners};
use aac_status::answers_engine::{HistoryEntry, StepState, closing_card_versions};
use aac_status::produce::answers_feed::AnswersFeed;
use aac_status::produce::facts::ZoneFacts;
use aac_status::produce::sources::{self, Readings};
use aac_status::produce::usage::UsageInput;
use aac_status::read_context::{Date, Presentation};
use aac_status::read_status::{self, ReportView};
use aac_status::share::check::{ReadersCheckWriter, RunRow};
use aac_status::share::reports::{AgentReportsState, Limits, SeenReport};
use aac_status::share::status_docs::STATUS_DOCS_LIMITS;
use aac_status::share::{Dirent, NodeFs, Problem, ReadError, ShareFs, js_text};
use aac_views::js_json::Writer;
use serde::Deserialize;
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Deserialize)]
struct Node {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    mtime: Option<f64>,
    #[serde(default)]
    ino: Option<u64>,
    #[serde(default)]
    ctime: Option<f64>,
    #[serde(default)]
    size: Option<u64>,
    #[serde(default)]
    target: Option<String>,
}

#[derive(Deserialize)]
struct StepLimits {
    writers: Option<usize>,
    cards: Option<usize>,
    bytes: Option<u64>,
}

#[derive(Deserialize)]
struct DocsLimits {
    writers: Option<usize>,
    retained: Option<usize>,
}

#[derive(Deserialize)]
struct Expect {
    reports: String,
    #[serde(rename = "reportsChanged")]
    reports_changed: bool,
    quota: String,
    #[serde(rename = "quotaChanged")]
    quota_changed: bool,
    registry: Json,
    docs: Option<String>,
    #[serde(rename = "docsChanged")]
    docs_changed: Option<bool>,
    catalog: String,
}

#[derive(Deserialize)]
struct Step {
    name: String,
    now: u64,
    tree: Vec<(String, Node)>,
    errors: Json,
    limits: Option<StepLimits>,
    #[serde(rename = "docsLimits")]
    docs_limits: Option<DocsLimits>,
    expect: Expect,
}

#[derive(Deserialize)]
struct Corpus {
    root: String,
    #[serde(rename = "registryDir")]
    registry_dir: String,
    steps: Vec<Step>,
}

struct MemFs {
    tree: indexmap::IndexMap<String, Node>,
    errors: Json,
}

fn injected(errors: &Json, path: &str, op: &str) -> Option<io::Error> {
    let code = errors.get(path)?.get(op)?.as_str()?;
    Some(match code {
        "ENOENT" => io::Error::from_raw_os_error(2),
        "ENOTDIR" => io::Error::from_raw_os_error(20),
        "ESTALE" => io::Error::from_raw_os_error(116),
        "EACCES" => io::Error::from_raw_os_error(13),
        other => io::Error::other(other.to_string()),
    })
}

impl MemFs {
    fn node(&self, path: &str) -> Option<&Node> {
        self.tree.get(path)
    }

    fn resolve(&self, path: &str) -> Option<&Node> {
        let node = self.node(path)?;
        if node.kind == "link" {
            return self.resolve(node.target.as_deref()?);
        }
        Some(node)
    }
}

impl ShareFs for MemFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<Dirent>> {
        let dir = dir.to_string_lossy().replace('\\', "/");
        if let Some(error) = injected(&self.errors, &dir, "list") {
            return Err(error);
        }
        let node = self
            .node(&dir)
            .ok_or_else(|| io::Error::from_raw_os_error(2))?;
        if node.kind != "dir" {
            return Err(io::Error::from_raw_os_error(20));
        }
        let prefix = format!("{dir}/");
        let mut out = Vec::new();
        for (key, node) in &self.tree {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            if rest.contains('/') {
                continue;
            }
            out.push(Dirent {
                name: rest.to_string(),
                is_dir: node.kind == "dir",
                is_file: node.kind == "file",
            });
        }
        Ok(out)
    }

    fn stat(&self, file: &Path) -> io::Result<Stat> {
        let file = file.to_string_lossy().replace('\\', "/");
        if let Some(error) = injected(&self.errors, &file, "stat") {
            return Err(error);
        }
        let node = self
            .resolve(&file)
            .ok_or_else(|| io::Error::from_raw_os_error(2))?;
        Ok(Stat {
            size: node
                .size
                .unwrap_or_else(|| node.content.as_deref().unwrap_or_default().len() as u64),
            mtime_ms: node.mtime.unwrap_or(0.0),
            ctime_ms: node.ctime.or(node.mtime).unwrap_or(0.0),
            device: 1,
            inode: node.ino.unwrap_or(0),
            file: node.kind == "file",
            dir: node.kind == "dir",
        })
    }

    fn read(&self, file: &Path, cap: usize) -> Result<Vec<u8>, ReadError> {
        let file = file.to_string_lossy().replace('\\', "/");
        if let Some(error) = injected(&self.errors, &file, "read") {
            return Err(ReadError::Io(error));
        }
        let node = self
            .node(&file)
            .ok_or_else(|| ReadError::Io(io::Error::from_raw_os_error(2)))?;
        match node.kind.as_str() {
            "file" => {
                let bytes = node.content.as_deref().unwrap_or_default().as_bytes();
                Ok(bytes[..bytes.len().min(cap + 1)].to_vec())
            }
            "link" => Err(ReadError::Io(io::Error::other("ELOOP"))),
            _ => Err(ReadError::Io(io::Error::other("ENOTREG"))),
        }
    }
}

fn same(step: &str, member: &str, want: &str, got: String) {
    if want != got {
        let at = want
            .bytes()
            .zip(got.bytes())
            .position(|(a, b)| a != b)
            .unwrap_or(want.len().min(got.len()));
        let from = at.saturating_sub(100);
        panic!(
            "{step}: {member} differs at byte {at}\nwant: ...{}\ngot:  ...{}",
            &want[from..want.len().min(at + 140)],
            &got[from..got.len().min(at + 140)]
        );
    }
}

/// Replays every step of `tests/spec/share/cases.json` against all five compact share readers,
/// proving byte-for-byte parity with the TypeScript corpus and `aac_status::share::*`.
#[test]
fn compact_readers_match_typescript_corpus_step_for_step() {
    let file = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/spec/share/cases.json");
    let text = std::fs::read_to_string(&file).expect("share parity corpus");
    let corpus: Corpus = serde_json::from_str(&text).expect("corpus parses");
    let root = PathBuf::from(&corpus.root);
    let regdir = PathBuf::from(&corpus.registry_dir);

    let mut reports = CompactAgentReportsReader::new(root.clone(), Limits::default());
    let mut quota = CompactQuotaReader::new(root.clone());
    let mut registry = CompactRegistryReader::new(regdir);
    let mut docs = CompactStatusDocsReader::new(root.clone(), STATUS_DOCS_LIMITS);
    let mut catalog = CompactCatalogReader::new(root.clone());

    for step in &corpus.steps {
        let fs = MemFs {
            tree: step.tree.iter().cloned().collect(),
            errors: step.errors.clone(),
        };
        let mut limited_reports;
        let reports_reader = if let Some(limits) = &step.limits {
            let base = Limits::default();
            limited_reports = CompactAgentReportsReader::new(
                root.clone(),
                Limits {
                    writers: limits.writers.unwrap_or(base.writers),
                    cards: limits.cards.unwrap_or(base.cards),
                    bytes: limits.bytes.unwrap_or(base.bytes),
                },
            );
            &mut limited_reports
        } else {
            &mut reports
        };
        let mut limited_docs;
        let docs_reader = if let Some(limits) = &step.docs_limits {
            let base = STATUS_DOCS_LIMITS;
            limited_docs = CompactStatusDocsReader::new(
                root.clone(),
                aac_status::share::status_docs::Limits {
                    writers: limits.writers.unwrap_or(base.writers),
                    retained: limits.retained.unwrap_or(base.retained),
                },
            );
            &mut limited_docs
        } else {
            &mut docs
        };

        let reports_changed = reports_reader.scan(&fs, step.now);
        let quota_changed = quota.scan(&fs, step.now);
        let docs_changed = docs_reader.scan(&fs, step.now);
        let registry_answer = match registry.scan_changes(&fs, step.now) {
            Ok(changed) => {
                let snap = registry.snapshot(step.now);
                let threads: Vec<Json> = snap
                    .data
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|row| row.to_json())
                    .collect();
                let problems: Vec<Json> = snap
                    .problems
                    .as_deref()
                    .unwrap_or(&[])
                    .iter()
                    .map(|p| p.to_json())
                    .collect();
                json!({
                    "threads": threads,
                    "problems": problems,
                    "changed": changed,
                })
            }
            Err(message) => json!({"error": message}),
        };
        let _catalog_changed = catalog.scan(&fs, step.now);

        let name = &step.name;
        same(
            name,
            "reports",
            &step.expect.reports,
            js_text(&reports_reader.snapshot().state_json(None)),
        );
        same(
            name,
            "quota",
            &step.expect.quota,
            js_text(&quota.state_json()),
        );
        assert_eq!(
            reports_changed, step.expect.reports_changed,
            "{name}: reports changed flag"
        );
        assert_eq!(
            quota_changed, step.expect.quota_changed,
            "{name}: quota changed flag"
        );
        same(
            name,
            "registry",
            &js_text(&step.expect.registry),
            js_text(&registry_answer),
        );
        same(
            name,
            "catalog",
            &step.expect.catalog,
            js_text(&catalog.snapshot_json()),
        );
        if let Some(want) = &step.expect.docs {
            assert_eq!(
                Some(docs_changed),
                step.expect.docs_changed,
                "{name}: docs changed flag"
            );
            same(
                name,
                "docs",
                want,
                js_text(&docs_reader.snapshot().to_state_json()),
            );
        }
    }
}

fn rust_owns_readers_check() -> Owners {
    let source = include_str!("../../../../config/private-files.json").replace(
        r#""id": "readers-check", "root": "share", "path": "v1/readers/aac/*.json", "class": "state", "maxBytes": 1048576, "mode": "0644", "owner": "ts", "agent": false, "movesWith": null"#,
        r#""id": "readers-check", "root": "share", "path": "v1/readers/aac/*.json", "class": "state", "maxBytes": 1048576, "mode": "0644", "owner": "ts", "agent": false, "movesWith": "status""#,
    );
    let rows = registry::parse(&source).unwrap();
    let files: BTreeMap<String, String> = rows
        .iter()
        .map(|row| {
            let word = if row.id == "readers-check" {
                "rust-service"
            } else {
                row.owner.word()
            };
            (row.id.clone(), word.to_owned())
        })
        .collect();
    Owners::from_boot(&rows, &files).unwrap()
}

fn read_check_files(root: &Path) -> BTreeMap<String, String> {
    let check = root.join("v1").join("readers").join("aac");
    let mut got = BTreeMap::new();
    for entry in std::fs::read_dir(&check).unwrap().flatten() {
        let name = entry.file_name().into_string().unwrap();
        if name.ends_with(".json") {
            got.insert(name, std::fs::read_to_string(entry.path()).unwrap());
        }
    }
    got
}

/// Replays `tests/spec/readers-check-vectors.json` against `CompactReadersCheckWriter`,
/// proving byte-identical files and dirty-only flushing.
#[test]
fn compact_readers_check_files_match_ts_vectors_and_dirty_flush() {
    let v: Json = serde_json::from_str(include_str!(
        "../../../../tests/spec/readers-check-vectors.json"
    ))
    .unwrap();
    let now = v["now"].as_u64().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    for (rel, content) in v["share"].as_object().unwrap() {
        let file = root.join(rel);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, content.as_str().unwrap()).unwrap();
    }
    let mut reports = CompactAgentReportsReader::new(root.clone(), Limits::default());
    reports.scan(&NodeFs, now);
    let docs = CompactStatusDocsReader::new(root.clone(), STATUS_DOCS_LIMITS);
    let pins: Vec<(String, String)> = v["pins"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            (
                p[0].as_str().unwrap().to_owned(),
                p[1].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let runs: Vec<RunRow> = v["runs"]["runs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| RunRow {
            file: r["file"].as_str().unwrap().into(),
            seq: r["doc"]["seq"].as_u64().unwrap(),
            sha256: r["sha256"].as_str().unwrap().into(),
            first_seen_at: r["firstSeenAt"].as_u64().unwrap(),
        })
        .collect();
    let problems: Vec<Problem> = v["runs"]["problems"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| Problem {
            file: p["file"].as_str().unwrap().into(),
            reason: p["reason"].as_str().unwrap().into(),
            since: p["since"].as_u64().unwrap(),
        })
        .collect();

    let owners = rust_owns_readers_check();
    let mut writer = CompactReadersCheckWriter::new(root.clone());
    writer.record_status_scan(&reports.snapshot(), &docs.snapshot(), &pins);
    writer.record_runs(&runs, &problems);
    writer.flush(&owners, now);

    let first = read_check_files(&root);
    let want: BTreeMap<String, String> = v["check"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, val)| {
            (
                k.strip_prefix("v1/readers/aac/").unwrap().to_owned(),
                val.as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        first, want,
        "compact check files must match TS vectors byte-for-byte"
    );

    // Quiet flush rewrites nothing (including writtenAt).
    writer.record_status_scan(&reports.snapshot(), &docs.snapshot(), &pins);
    writer.record_runs(&runs, &problems);
    writer.flush(&owners, now + 60_000);
    assert_eq!(
        read_check_files(&root),
        first,
        "unchanged rows rewrite nothing"
    );

    // Mutate only vm.lead: only vm.lead.json changes.
    let mut more: Vec<RunRow> = runs
        .iter()
        .map(|r| RunRow {
            file: r.file.clone(),
            seq: r.seq,
            sha256: r.sha256.clone(),
            first_seen_at: r.first_seen_at,
        })
        .collect();
    more.push(RunRow {
        file: "writers/vm.lead/runs/2026-11-07/r2.json".to_string(),
        seq: 2,
        sha256: "0".repeat(64),
        first_seen_at: now,
    });
    writer.record_runs(&more, &problems);
    writer.flush(&owners, now + 120_000);
    let third = read_check_files(&root);
    assert_eq!(third["mac.worker.json"], first["mac.worker.json"]);
    assert_ne!(third["vm.lead.json"], first["vm.lead.json"]);
    assert!(third["vm.lead.json"].contains("runs/2026-11-07/r2.json"));
}

#[test]
fn collate_fast_matches_aac_num_collate_on_all_edge_cases() {
    let words = [
        "",
        " ",
        "_",
        "-",
        "~",
        "0",
        "9",
        "a",
        "A",
        "a ",
        "A ",
        "ab",
        "aB",
        "Ab",
        "AB",
        "a_1",
        "a-1",
        "a-b",
        "vm.lead",
        "vm.Lead",
        "vm.lead-1",
        "vm.lead_1",
        "Claude and GPT models \u{b7} 5-hour \u{b7} In T3",
        "Claude and GPT models \u{b7} Weekly \u{b7} In T3",
        "é",
        "e",
        "café",
        "cafe",
        "模型",
        "model",
        "a\tb",
        "a\nb",
        "🚀",
    ];
    for a in words {
        for b in words {
            assert_eq!(
                collate_fast(a, b),
                aac_num::intl::collate(a, b),
                "collate_fast({a:?}, {b:?})"
            );
        }
    }
}

fn history_entry(value: &Json) -> HistoryEntry {
    let state = |v: &Json| StepState {
        state: v["state"].as_str().unwrap_or("").to_owned(),
        at: v["at"].as_str().map(str::to_owned),
        detail: v["detail"].as_str().map(str::to_owned),
        attempts: v["attempts"].as_u64().unwrap_or(0),
    };
    HistoryEntry {
        id: value["id"].as_str().unwrap_or("").to_owned(),
        send_id: value["sendId"].as_str().map(str::to_owned),
        shared_only: value["sharedOnly"] == true,
        answer: value["answer"].clone(),
        card: value["card"].clone(),
        share: state(&value["share"]),
        doorbell: state(&value["doorbell"]),
    }
}

fn reports_state_from_wire(wire: &Json) -> AgentReportsState {
    let rows = |key: &str| -> Vec<Arc<SeenReport>> {
        wire[key]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|r| {
                Arc::new(SeenReport {
                    file: r["file"].as_str().unwrap_or_default().to_owned(),
                    writer: r["writer"].as_str().unwrap_or_default().to_owned(),
                    doc: Arc::new(r["doc"].clone()),
                    sha256: r["sha256"].as_str().unwrap_or_default().to_owned(),
                    first_seen_at: r["firstSeenAt"].as_u64().unwrap_or(0),
                    written_at_ms: r["writtenAtMs"].as_i64().unwrap_or(0),
                    problem: r["problem"].as_object().map(|p| Problem {
                        file: p["file"].as_str().unwrap_or_default().to_owned(),
                        reason: p["reason"].as_str().unwrap_or_default().to_owned(),
                        since: p["since"].as_u64().unwrap_or(0),
                    }),
                })
            })
            .collect()
    };
    let problems = wire["problems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .map(|p| Problem {
            file: p["file"].as_str().unwrap_or_default().to_owned(),
            reason: p["reason"].as_str().unwrap_or_default().to_owned(),
            since: p["since"].as_u64().unwrap_or(0),
        })
        .collect();
    AgentReportsState {
        root: wire["root"].as_str().unwrap_or_default().to_owned(),
        state: match wire["state"].as_str().unwrap_or("missing") {
            "ok" => "ok",
            "error" => "error",
            _ => "missing",
        },
        scanned_at: wire["scannedAt"].as_u64(),
        complete: wire["complete"] == true,
        threads: rows("threads"),
        tasks: rows("tasks"),
        cards: rows("cards"),
        problems,
        boards: wire["boards"].as_object().cloned().unwrap_or_default(),
    }
}

fn render_view(view: &Json) -> (Vec<u8>, Vec<u8>) {
    let mut writer = Writer::with_capacity(256 * 1024);
    writer.value(view);
    writer.finish()
}

/// Proves byte-for-byte identical `/api/status` JSON and ETag identity between `compose_compact`
/// (with `OwnedZone` and with `Wire` `Presentation`) and the legacy `aac_status::read_status`
/// oracle on `tests/spec/status-produce-vectors.json`.
#[test]
fn compact_status_compose_matches_legacy_oracle_byte_for_byte_on_produce_vectors() {
    let vectors: Json = serde_json::from_str(include_str!(
        "../../../../tests/spec/status-produce-vectors.json"
    ))
    .unwrap();
    let input = &vectors["input"];
    let zone_json = &vectors["zone"];
    let zone = ZoneFacts {
        from: zone_json["from"].as_i64().unwrap(),
        to: zone_json["to"].as_i64().unwrap(),
        offsets: zone_json["offsets"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| [row[0].as_i64().unwrap(), row[1].as_i64().unwrap()])
            .collect(),
    }
    .zone()
    .unwrap();
    let now = vectors["now"].as_i64().unwrap();
    let tz = vectors["timeZone"].as_str().unwrap();
    let offset = zone.offset_minutes(now).unwrap();
    let (clock, day) = zone.words(now).unwrap();

    let answers = AnswersFeed {
        answers: input["answers"]
            .as_array()
            .unwrap()
            .iter()
            .map(history_entry)
            .collect(),
        sends: Vec::new(),
        problem: input["answerHistoryProblem"].as_str().map(str::to_owned),
    };
    let usage = (!input["usage"].is_null()).then(|| UsageInput::parse(&input["usage"]).unwrap());
    let closed = closing_card_versions(&answers.answers);

    // Complete `machines` with any host IDs from current `config/tables/hosts.json`
    // (such as `odroid`, `1070ti`) that postdate the fixture, and also verify matching
    // `Err(Unproven::Input)` when they are absent.
    let mut full_machines = input["machines"].clone();
    if let (Some(map), Some(sample)) = (
        full_machines.as_object_mut(),
        input["machines"].get("mac").cloned(),
    ) {
        let ids: Vec<String> = vectors["sources"]["status-facts"]["data"]
            .as_str()
            .and_then(|s| serde_json::from_str::<Json>(s).ok())
            .and_then(|d| d["machineIds"].as_array().cloned())
            .unwrap_or_default()
            .into_iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect();
        for id in ids {
            map.entry(id).or_insert_with(|| sample.clone());
        }
    }

    // 1. Legacy oracle via `ReportView` + `sources::build` + `read_status::compose_with_reports`.
    let legacy_state = Arc::new(reports_state_from_wire(&input["reports"]));
    let legacy_report_view = ReportView::new(Arc::clone(&legacy_state), &closed, None);
    let legacy_sources = sources::build(&Readings {
        reports: sources::Reports::Native(&legacy_report_view),
        quota: &input["quota"],
        pinned: &input["pinned"],
        registry: &input["registry"],
        machines: &full_machines,
        browsers: &input["browsers"],
        simlock: &input["simlock"],
        usage: usage.as_ref(),
        answers: &answers,
        time_zone: tz,
        now,
        offset_minutes: offset,
        zone: &zone,
    })
    .unwrap();
    let legacy_quota: Json = serde_json::from_str(&legacy_sources.quota).unwrap();
    let legacy_pinned: Json = serde_json::from_str(&legacy_sources.pinned).unwrap();
    let legacy_collected: Json = serde_json::from_str(&legacy_sources.collected).unwrap();
    let legacy_answers: Json = serde_json::from_str(&legacy_sources.answers).unwrap();
    let legacy_facts_data: Json = serde_json::from_str(&legacy_sources.facts_data).unwrap();
    let legacy_pres: Arc<Presentation> =
        Arc::new(serde_json::from_str(&legacy_sources.facts).unwrap());
    let now_val = Json::from(now);
    let mut legacy_parts: Vec<(&str, &Json)> = vec![("now", &now_val)];
    for obj in [
        &legacy_quota,
        &legacy_pinned,
        &legacy_collected,
        &legacy_answers,
        &legacy_facts_data,
    ] {
        for (k, v) in obj.as_object().unwrap() {
            legacy_parts.push((k.as_str(), v));
        }
    }
    let (legacy_view, _) = read_status::compose_with_reports(
        legacy_parts,
        Some(&legacy_report_view),
        Arc::clone(&legacy_pres),
        (now, Date::of_now(now, clock.clone(), day.clone()).unwrap()),
        [None, None],
    )
    .unwrap();
    let (legacy_bytes, legacy_identity) = render_view(&legacy_view);

    // 2. Compact path: `CompactAgentReportsState` + `CompactRegistrySnapshot` + `OwnedZone`.
    let compact_reports = Arc::new(CompactAgentReportsState::from_agent_reports_state(
        &legacy_state,
    ));
    let owned_reports = OwnedReportsView::new(Arc::clone(&compact_reports), &closed);
    let compact_registry = CompactRegistrySnapshot::from_json(&input["registry"]);
    let compact_spend_panel = spend_by_stream_compact(
        &input["quota"],
        &compact_reports.tasks,
        &zone,
        now,
        offset,
        usage
            .as_ref()
            .map(UsageInput::account_order)
            .unwrap_or_default(),
    )
    .unwrap();
    assert_eq!(
        js_text(&compact_spend_panel),
        js_text(&legacy_quota["spendByStream"]),
        "spendByStream panel must be byte-identical"
    );

    let compact_quota = json!({
        "quota": input["quota"],
        "spendByStream": compact_spend_panel,
    });
    let mut compact_parts: Vec<(&str, &Json)> = vec![
        ("now", &now_val),
        ("quota", &compact_quota["quota"]),
        ("spendByStream", &compact_quota["spendByStream"]),
        ("pinned", &legacy_pinned["pinned"]),
        ("machines", &legacy_collected["machines"]),
        ("browsers", &legacy_collected["browsers"]),
        ("simlock", &legacy_collected["simlock"]),
        ("usage", &legacy_collected["usage"]),
        ("answers", &legacy_answers["answers"]),
        ("sends", &legacy_answers["sends"]),
        (
            "answerHistoryProblem",
            &legacy_answers["answerHistoryProblem"],
        ),
    ];
    for (k, v) in legacy_facts_data.as_object().unwrap() {
        compact_parts.push((k.as_str(), v));
    }

    let (compact_view_zone, _) = compose_compact(
        compact_parts.clone(),
        ReportsInput::Compact(&owned_reports),
        RegistryInput::Compact(&compact_registry),
        &FactsSource::OwnedZone(zone.clone()),
        now,
        clock.clone(),
        day.clone(),
        [None, None],
    )
    .unwrap();
    let (compact_bytes_zone, compact_id_zone) = render_view(&compact_view_zone);
    assert_eq!(
        String::from_utf8_lossy(&compact_bytes_zone),
        String::from_utf8_lossy(&legacy_bytes),
        "OwnedZone compact Status bytes must equal legacy oracle byte-for-byte"
    );
    assert_eq!(compact_id_zone, legacy_identity, "ETag identity must match");

    // 3. Compact path with `FactsSource::Wire(legacy_pres)` (mixed mode).
    let (compact_view_wire, _) = compose_compact(
        compact_parts,
        ReportsInput::Compact(&owned_reports),
        RegistryInput::Compact(&compact_registry),
        &FactsSource::Wire(legacy_pres),
        now,
        clock,
        day,
        [None, None],
    )
    .unwrap();
    let (compact_bytes_wire, compact_id_wire) = render_view(&compact_view_wire);
    assert_eq!(compact_bytes_wire, legacy_bytes);
    assert_eq!(compact_id_wire, legacy_identity);
}

/// Live-shaped scale parity + timing test: 1,000 kept writers, 200 task boards (10 items each),
/// 400 cards (10 open, 390 withdrawn), 5,000 pm-mail registry rows, 1,000 status docs, 2,000 runs.
/// Verifies byte-for-byte identical Status JSON + check files AND cold compose within timing budget.
#[test]
fn live_scale_compact_status_and_check_are_byte_identical_and_fast() {
    let now: i64 = 1_795_000_000_000;
    let now_u64 = now as u64;
    let zone = ZoneFacts {
        from: now - 30 * 86_400_000,
        to: now + 30 * 86_400_000,
        offsets: vec![[now - 30 * 86_400_000, -300]],
    }
    .zone()
    .unwrap();
    let (clock, day) = zone.words(now).unwrap();
    let recent_iso = aac_num::ecma::to_iso_string(now - 120_000).unwrap();
    let old_iso = aac_num::ecma::to_iso_string(now - 90_000_000).unwrap();

    let mut threads = Vec::with_capacity(1_000);
    let mut tasks = Vec::with_capacity(200);
    let mut cards = Vec::with_capacity(400);
    let mut registry_rows = Vec::with_capacity(5_000);

    for idx in 0..1_000usize {
        let machine = ["vm", "mac", "windows", "nas1"][idx % 4];
        let writer = format!("{machine}.worker-{idx:04}");
        let state = ["working", "waiting", "blocked", "idle", "done"][idx % 5];
        let seen_ms = if idx < 800 {
            now - (idx as i64 * 1_000)
        } else {
            now - 100_000_000
        };
        let doc = json!({
            "writer": writer,
            "seq": (idx as u64) + 1,
            "writtenAt": if idx < 800 { &recent_iso } else { &old_iso },
            "ttlSeconds": if idx % 3 == 0 { Json::Null } else { json!(3600) },
            "agent": {
                "name": format!("worker-{idx:04}"),
                "harness": "claude-code",
                "model": "claude-opus-4-7",
                "effort": "high",
                "account": "acct-1",
                "machine": machine,
                "t3Thread": format!("t3-{idx:04}"),
                "parent": null
            },
            "project": format!("proj-{}", idx % 12),
            "stream": format!("stream-{}", idx % 30),
            "state": state,
            "doing": format!("Working on task {idx}"),
            "blockedOn": if state == "blocked" {
                json!({
                    "kind": if idx % 10 == 0 { "owner" } else { "review" },
                    "what": format!("Card Q{idx}: waiting"),
                    "since": &recent_iso,
                    "short": null,
                    "until": null,
                    "card": if idx % 10 == 0 { json!(format!("Q{idx}")) } else { Json::Null }
                })
            } else {
                Json::Null
            },
            "progress": {"done": idx % 10, "total": 10, "unit": "tasks"},
            "links": if idx % 20 == 0 {
                json!([{"label": "Board", "path": format!("/share/boards/b-{idx}.md")}])
            } else {
                json!([])
            }
        });
        let sha = format!("{idx:064x}");
        threads.push(Arc::new(SeenReport {
            file: format!("writers/{writer}/thread.json"),
            writer: writer.clone(),
            doc: Arc::new(doc),
            sha256: sha,
            first_seen_at: seen_ms as u64,
            written_at_ms: seen_ms,
            problem: None,
        }));
    }

    for idx in 0..200usize {
        let writer = format!("vm.lead-{idx:03}");
        let item_states = ["todo", "working", "review", "blocked", "landed", "done"];
        let items: Vec<Json> = (0..10usize)
            .map(|j| {
                let state = item_states[j % 6];
                json!({
                    "id": format!("T-{idx}-{j}"),
                    "title": format!("Item {j} on stream {idx}"),
                    "state": state,
                    "owner": format!("worker-{}", j % 5),
                    "branch": format!("feat/t-{idx}-{j}"),
                    "head": format!("{:07x}", idx * 100 + j),
                    "reviews": [],
                    "landed": null,
                    "blockedOn": null,
                    "note": null,
                    "why": null,
                    "updatedAt": &recent_iso
                })
            })
            .collect();
        let doc = json!({
            "writer": writer,
            "seq": 5,
            "writtenAt": &recent_iso,
            "project": format!("proj-{}", idx % 12),
            "projectLabel": format!("Project {}", idx % 12),
            "stream": format!("stream-{idx}"),
            "streamLabel": format!("Stream {idx}"),
            "owner": format!("lead-{idx:03}"),
            "goal": format!("Deliver stream {idx}"),
            "board": format!("/share/boards/tasks-{idx}.md"),
            "items": items
        });
        tasks.push(Arc::new(SeenReport {
            file: format!("writers/{writer}/tasks.json"),
            writer: writer.clone(),
            doc: Arc::new(doc),
            sha256: format!("{:064x}", 10_000 + idx),
            first_seen_at: (now - 60_000) as u64,
            written_at_ms: now - 60_000,
            problem: None,
        }));
    }

    for idx in 0..400usize {
        let writer = format!("vm.worker-{:04}", idx % 100);
        let id = format!("{writer}--Q{idx}");
        let is_open = idx < 10;
        let urgency = ["now", "today", "this-week", "whenever"][idx % 4];
        let doc = json!({
            "id": id,
            "writer": writer,
            "seq": 1,
            "writtenAt": &recent_iso,
            "project": format!("proj-{}", idx % 12),
            "stream": format!("stream-{}", idx % 30),
            "askedBy": "worker",
            "state": if is_open { "open" } else { "withdrawn" },
            "label": format!("Q{idx}"),
            "title": format!("Question {idx}"),
            "now": "Context text for card.",
            "nowAsOf": null,
            "oldAnswer": null,
            "whatChanged": null,
            "options": [
                {"key": "A", "label": "Yes", "gets": "Fast", "costs": null, "media": [], "sections": [], "stance": null},
                {"key": "B", "label": "No", "gets": null, "costs": "Slow", "media": [], "sections": [], "stance": null}
            ],
            "recommendation": null,
            "multiple": false,
            "urgency": urgency,
            "blocks": null,
            "sections": [],
            "batch": null,
            "kind": "question",
            "askedAt": &recent_iso
        });
        cards.push(Arc::new(SeenReport {
            file: format!("writers/{writer}/cards/{id}.json"),
            writer,
            doc: Arc::new(doc),
            sha256: format!("{:064x}", 20_000 + idx),
            first_seen_at: (now - 30_000) as u64,
            written_at_ms: now - 30_000,
            problem: None,
        }));
    }

    for idx in 0..5_000usize {
        let recent = idx < 250;
        let machine = ["vm", "mac", "windows", "nas1"][idx % 4];
        registry_rows.push(json!({
            "name": format!("reg-thread-{idx:05}"),
            "kind": "agent",
            "machine": machine,
            "about": format!("Agent {idx} doing work"),
            "lastSeen": if recent { &recent_iso } else { &old_iso },
            "readAt": now_u64 - 5_000,
            "problem": null
        }));
    }

    let registry_json = json!({
        "data": registry_rows,
        "asOf": now_u64 - 5_000,
        "error": null,
        "problems": []
    });
    let mut boards = serde_json::Map::new();
    for idx in 0..200usize {
        boards.insert(
            format!("/share/boards/tasks-{idx}.md"),
            json!((now - 45_000) as f64 + 0.25),
        );
    }

    let legacy_reports_state = Arc::new(AgentReportsState {
        root: "/share/reports".to_owned(),
        state: "ok",
        scanned_at: Some(now_u64 - 2_000),
        complete: true,
        threads,
        tasks,
        cards,
        problems: Vec::new(),
        boards,
    });
    let compact_reports_state = Arc::new(CompactAgentReportsState::from_agent_reports_state(
        &legacy_reports_state,
    ));
    let compact_registry = CompactRegistrySnapshot::from_json(&registry_json);
    let closed: HashSet<String> = HashSet::new();

    let machines: serde_json::Map<String, Json> = aac_status::computers::machine_ids()
        .into_iter()
        .map(|id| {
            (
                id.to_owned(),
                json!({"data": null, "asOf": null, "error": null}),
            )
        })
        .collect();
    let machines_json = Json::Object(machines);
    let browsers_json = json!({"data": null, "asOf": null, "error": null});
    let simlock_json = json!({"data": null, "asOf": null, "error": null});
    let pinned_json = json!({"unavailable": "not read"});
    let quota_json = Json::Null;
    let answers = AnswersFeed {
        answers: Vec::new(),
        sends: Vec::new(),
        problem: None,
    };

    // Legacy oracle
    let legacy_rv = ReportView::new(Arc::clone(&legacy_reports_state), &closed, None);
    let legacy_sources = sources::build(&Readings {
        reports: sources::Reports::Native(&legacy_rv),
        quota: &quota_json,
        pinned: &pinned_json,
        registry: &registry_json,
        machines: &machines_json,
        browsers: &browsers_json,
        simlock: &simlock_json,
        usage: None,
        answers: &answers,
        time_zone: "America/New_York",
        now,
        offset_minutes: -300,
        zone: &zone,
    })
    .unwrap();
    let legacy_quota: Json = serde_json::from_str(&legacy_sources.quota).unwrap();
    let legacy_pinned: Json = serde_json::from_str(&legacy_sources.pinned).unwrap();
    let legacy_collected: Json = serde_json::from_str(&legacy_sources.collected).unwrap();
    let legacy_answers: Json = serde_json::from_str(&legacy_sources.answers).unwrap();
    let legacy_facts_data: Json = serde_json::from_str(&legacy_sources.facts_data).unwrap();
    let legacy_pres: Arc<Presentation> =
        Arc::new(serde_json::from_str(&legacy_sources.facts).unwrap());
    let now_val = Json::from(now);
    let mut legacy_parts: Vec<(&str, &Json)> = vec![("now", &now_val)];
    for obj in [
        &legacy_quota,
        &legacy_pinned,
        &legacy_collected,
        &legacy_answers,
        &legacy_facts_data,
    ] {
        for (k, v) in obj.as_object().unwrap() {
            legacy_parts.push((k.as_str(), v));
        }
    }

    let t0 = std::time::Instant::now();
    let (legacy_view, _) = read_status::compose_with_reports(
        legacy_parts,
        Some(&legacy_rv),
        Arc::clone(&legacy_pres),
        (now, Date::of_now(now, clock.clone(), day.clone()).unwrap()),
        [None, None],
    )
    .unwrap();
    let legacy_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let (legacy_bytes, legacy_id) = render_view(&legacy_view);

    // Compact path
    let owned_reports = OwnedReportsView::new(Arc::clone(&compact_reports_state), &closed);
    let mut compact_parts: Vec<(&str, &Json)> = vec![
        ("now", &now_val),
        ("quota", &legacy_quota["quota"]),
        ("spendByStream", &legacy_quota["spendByStream"]),
        ("pinned", &legacy_pinned["pinned"]),
        ("machines", &legacy_collected["machines"]),
        ("browsers", &legacy_collected["browsers"]),
        ("simlock", &legacy_collected["simlock"]),
        ("usage", &legacy_collected["usage"]),
        ("answers", &legacy_answers["answers"]),
        ("sends", &legacy_answers["sends"]),
        (
            "answerHistoryProblem",
            &legacy_answers["answerHistoryProblem"],
        ),
    ];
    for (k, v) in legacy_facts_data.as_object().unwrap() {
        compact_parts.push((k.as_str(), v));
    }

    let t1 = std::time::Instant::now();
    let (compact_view, _) = compose_compact(
        compact_parts,
        ReportsInput::Compact(&owned_reports),
        RegistryInput::Compact(&compact_registry),
        &FactsSource::OwnedZone(zone.clone()),
        now,
        clock,
        day,
        [None, None],
    )
    .unwrap();
    let compact_ms = t1.elapsed().as_secs_f64() * 1000.0;
    let (compact_bytes, compact_id) = render_view(&compact_view);

    assert_eq!(
        compact_bytes, legacy_bytes,
        "live-scale Status JSON must be byte-identical"
    );
    assert_eq!(compact_id, legacy_id, "live-scale ETag identity must match");
    eprintln!(
        "live-scale Status compose: legacy={legacy_ms:.2}ms compact={compact_ms:.2}ms bytes={}",
        compact_bytes.len()
    );

    // Also prove CompactReadersCheckWriter matches ReadersCheckWriter on multi-writer scan + runs.
    let dir_a = tempfile::tempdir().unwrap();
    let dir_b = tempfile::tempdir().unwrap();
    let root_a = dir_a.path().canonicalize().unwrap();
    let root_b = dir_b.path().canonicalize().unwrap();
    let owners = rust_owns_readers_check();
    let empty_legacy_docs = aac_status::share::status_docs::StatusDocsState {
        state: "ok",
        scanned_at: Some(now_u64),
        docs: Vec::new(),
        problems: Vec::new(),
    };
    let empty_compact_docs = super::status_docs::CompactStatusDocsState::empty();
    let pins = vec![("t3-0000".to_owned(), "Pinned Zero".to_owned())];
    let sample_runs: Vec<RunRow> = (0..50usize)
        .map(|i| RunRow {
            file: format!("writers/vm.worker-{:04}/runs/2026-11-26/r-{i}.json", i % 20),
            seq: (i + 1) as u64,
            sha256: format!("{:064x}", 50_000 + i),
            first_seen_at: now_u64,
        })
        .collect();
    let mut legacy_check = ReadersCheckWriter::new(root_a.clone());
    let mut compact_check = CompactReadersCheckWriter::new(root_b.clone());
    // Use first 40 writers for fast filesystem flush comparison.
    let mut small_legacy = (*legacy_reports_state).clone();
    small_legacy.threads.truncate(40);
    small_legacy.tasks.truncate(10);
    small_legacy.cards.truncate(20);
    let small_compact = CompactAgentReportsState::from_agent_reports_state(&small_legacy);
    legacy_check.record_status_scan(&small_legacy, &empty_legacy_docs, &pins);
    legacy_check.record_runs(&sample_runs, &[]);
    legacy_check.flush(&owners, now_u64);

    compact_check.record_status_scan(&small_compact, &empty_compact_docs, &pins);
    compact_check.record_runs(&sample_runs, &[]);
    compact_check.flush(&owners, now_u64);

    assert_eq!(
        read_check_files(&root_b),
        read_check_files(&root_a),
        "CompactReadersCheckWriter files must match ReadersCheckWriter byte-for-byte"
    );
}
