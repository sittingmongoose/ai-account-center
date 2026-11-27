//! Parity and end-to-end integration tests for `aac-xtask perf`.

use aac_xtask::perf::budget::{
    PollRoute, RouteRecord, RunOptions, assert_no_symlinks, assert_resolved_inside,
    build_isolated_env, check_built_artifacts, default_poll_plan, evaluate_route_failures,
    evaluate_sample_failures, evaluate_thresholds, exit_status_info, format_human_summary,
    load_threshold_config, run_memory_budget, summarize_routes, summarize_samples,
    validate_run_options, validate_threshold_config,
};
use aac_xtask::perf::fixture::{
    FixtureOptions, Rng, build_legacy_source_accounts, build_synthetic_accounts,
    generate_memory_fixture,
};
use aac_xtask::perf::http_client::{HttpClient, decode_content_encoding};
use aac_xtask::perf::proc_mem::{
    MemorySample, ProcStat, collect_descendants, parse_proc_stat, parse_proc_status_rss,
    parse_smaps_rollup,
};
use serde_json::{Value as Json, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn walk_files(dir: &Path) -> Vec<String> {
    fn visit(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in fs::read_dir(dir).expect("read_dir") {
            let entry = entry.expect("dir entry");
            let path = entry.path();
            if path.is_dir() {
                visit(&path, base, out);
            } else {
                let rel = path
                    .strip_prefix(base)
                    .expect("strip_prefix")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    visit(dir, dir, &mut out);
    out.sort();
    out
}

fn make_sample(
    t_sec: f64,
    server_rss_kib: u64,
    children_rss_kib: u64,
    server_pss_kib: u64,
) -> MemorySample {
    MemorySample {
        t_sec,
        server_rss_kib,
        server_pss_kib: Some(server_pss_kib),
        server_anon_kib: Some(server_pss_kib),
        server_file_kib: Some(0),
        server_swap_kib: 0,
        pss_available: true,
        server_pss_available: true,
        threads: 8,
        children_count: usize::from(children_rss_kib > 0),
        children_rss_kib,
        children_pss_kib: Some(children_rss_kib),
        total_rss_kib: server_rss_kib + children_rss_kib,
        total_pss_kib: Some(server_pss_kib + children_rss_kib),
    }
}

fn contains_blocked_personal_detail(text: &str) -> bool {
    const BLOCKED_WORDS: [&str; 5] = [
        "0fbcceaae1977bab1d7be30549247fecd78a75b3574d015eb097bcce71fd9f08",
        "237e1cae99b6c84038bd60299fc641a6859e603f3a7f7729042a8a5786d5bfe7",
        "27d300fe53b3b94f115cfd63be02d868bcb8f755e56893709418084c1bfab1cd",
        "8137bce2aa40deb8ef397346bb798ac40b16f98138b1e9a0205860653e7980ba",
        "49449cc1bb8fdaff47febe7ad8762087e7feaccbc5321935f39204ba1c496db8",
    ];
    for raw in text.split(|c: char| !c.is_ascii_alphanumeric()) {
        if raw.is_empty() {
            continue;
        }
        let lower = raw.to_ascii_lowercase();
        let hash = aac_core::sha256_hex(&lower);
        if BLOCKED_WORDS.contains(&hash.as_str()) {
            return true;
        }
    }
    false
}

fn write_fake_dashboard_py(
    dir: &Path,
    filename: &str,
    fail_after_first: bool,
    truncate_path: &str,
    startup_memory_mb: usize,
    analytics_mode: &str,
) -> PathBuf {
    let script_path = dir.join(filename);
    let source = format!(
        r#"import gzip
import http.server
import json
import os
import signal
import sys
import time
import urllib.parse

args = sys.argv[1:]
port = int(args[args.index('--port') + 1]) if '--port' in args else 0
fail_after_first = {fail_after_first}
truncate_path = {truncate_path:?}
analytics_mode = {analytics_mode:?}
startup_mb = {startup_mb}
hits = {{}}

class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.1'
    def log_message(self, fmt, *a):
        pass
    def do_GET(self):
        path = self.path
        if truncate_path and path == truncate_path:
            self.wfile.write(b'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 1024\r\n\r\n{{')
            self.wfile.flush()
            time.sleep(0.05)
            os._exit(17)
        if path == '/api/health':
            body = b'{{"ok":true}}'
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if path == '/api/auth/setup':
            body = b'{{"configured":false}}'
            self.send_response(200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        count = hits.get(path, 0) + 1
        hits[path] = count
        if fail_after_first and count > 1:
            body = b'{{"error":"simulated_late_failure"}}'
            self.send_response(500)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        if path.startswith('/api/accounts/analytics?'):
            qs = urllib.parse.parse_qs(urllib.parse.urlparse(path).query)
            preset = qs.get('range', [''])[0]
            payload = {{
                'stale': analytics_mode == 'unavailable',
                'range': {{'preset': preset}},
                'history': {{'status': analytics_mode, 'sampleCount': 19 if analytics_mode == 'ok' else 0}},
                'activity': {{'status': 'ok', 'byDay': [{{'date': '2026-11-25'}}]}},
            }}
            body = gzip.compress(json.dumps(payload).encode('utf-8'))
            self.send_response(503 if analytics_mode == 'http-error' else 200)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Content-Encoding', 'gzip')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        body = json.dumps({{'ok': True, 'hit': count}}).encode('utf-8')
        self.send_response(200)
        self.send_header('Content-Type', 'application/json')
        self.send_header('Content-Length', str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get('Content-Length', '0'))
        if length > 0:
            self.rfile.read(length)
        if self.path == '/api/auth/setup':
            body = b'{{"ok":true}}'
            self.send_response(201)
            self.send_header('Content-Type', 'application/json')
            self.send_header('Set-Cookie', 'aac_session=fake-token; HttpOnly; Path=/')
            self.send_header('Content-Length', str(len(body)))
            self.end_headers()
            self.wfile.write(body)
            return
        self.send_response(404)
        self.send_header('Content-Length', '0')
        self.end_headers()

if startup_mb > 0:
    import ctypes
    buf = bytearray(startup_mb * 1024 * 1024)
    for i in range(0, len(buf), 4096):
        buf[i] = 1
    time.sleep(0.8)
    if buf[0] != 1:
        os._exit(18)
    del buf
    try:
        ctypes.CDLL('libc.so.6').malloc_trim(0)
    except Exception:
        pass

server = http.server.ThreadingHTTPServer(('127.0.0.1', port), Handler)
def handle_term(_sig, _frame):
    os._exit(0)
signal.signal(signal.SIGTERM, handle_term)
server.serve_forever()
"#,
        fail_after_first = if fail_after_first { "True" } else { "False" },
        truncate_path = truncate_path,
        analytics_mode = analytics_mode,
        startup_mb = startup_memory_mb,
    );
    fs::write(&script_path, source).expect("write fake dashboard");
    script_path
}

#[test]
fn fixture_is_byte_identical_to_js_oracle_and_has_no_personal_details() {
    let tmp_a = tempfile::tempdir().expect("tempdir a");
    let tmp_b = tempfile::tempdir().expect("tempdir b");
    let fixed_now = 1_795_608_000_000_i64; // 2026-11-25T12:00:00Z
    let opts = FixtureOptions {
        seed: 424_242,
        now: Some(fixed_now),
        writers: 16,
        run_writers: 4,
        runs: 110,
        registry_entries: 15,
        days: 3,
        accounts: 9,
        quota_samples_per_day: 2,
        activity_sessions: 8,
        pricing_providers: 4,
        pricing_models_per_provider: 3,
        skip_store: true,
    };

    let manifest_a = generate_memory_fixture(&repo_root(), tmp_a.path(), &opts).expect("gen a");
    let manifest_b = generate_memory_fixture(&repo_root(), tmp_b.path(), &opts).expect("gen b");

    assert_eq!(manifest_a.counts, manifest_b.counts);
    assert_eq!(manifest_a.counts.writers, 16);
    assert_eq!(manifest_a.counts.run_writers, 4);
    assert_eq!(manifest_a.counts.runs, 110);
    assert_eq!(manifest_a.counts.registry_entries, 15);
    assert_eq!(manifest_a.counts.providers, 3);
    assert_eq!(manifest_a.counts.accounts, 9);
    assert_eq!(manifest_a.counts.legacy_source_accounts, 7);
    assert_eq!(manifest_a.counts.days, 3);
    assert_eq!(manifest_a.counts.quota_observations, 96);
    assert_eq!(manifest_a.counts.activity_events, 568);
    assert_eq!(manifest_a.counts.activity_sessions, 8);
    assert_eq!(manifest_a.counts.pricing_providers, 7);
    assert_eq!(manifest_a.counts.pricing_models, 25);

    let files_a: Vec<String> = walk_files(tmp_a.path())
        .into_iter()
        .filter(|f| f != "fixture-manifest.json")
        .collect();
    let files_b: Vec<String> = walk_files(tmp_b.path())
        .into_iter()
        .filter(|f| f != "fixture-manifest.json")
        .collect();
    assert_eq!(files_a, files_b);
    assert_eq!(files_a.len(), 193);

    let root_a_str = tmp_a.path().to_string_lossy().to_string();
    let root_b_str = tmp_b.path().to_string_lossy().to_string();
    let mut hashes = serde_json::Map::new();
    for rel in &files_a {
        let bytes_a = fs::read(tmp_a.path().join(rel)).expect("read a");
        let bytes_b = fs::read(tmp_b.path().join(rel)).expect("read b");
        let norm_a = if rel.ends_with(".json")
            || rel.ends_with(".jsonl")
            || rel.ends_with(".md")
            || rel.ends_with(".pub")
            || rel.ends_with(".pem")
        {
            let text_a = String::from_utf8_lossy(&bytes_a).replace(&root_a_str, "<ROOT>");
            let text_b = String::from_utf8_lossy(&bytes_b).replace(&root_b_str, "<ROOT>");
            assert_eq!(text_a, text_b, "determinism mismatch in {rel}");
            if rel.ends_with(".json") || rel.ends_with(".jsonl") {
                assert!(
                    !contains_blocked_personal_detail(&text_a),
                    "personal detail in {rel}"
                );
            }
            text_a.into_bytes()
        } else {
            assert_eq!(bytes_a, bytes_b, "binary mismatch in {rel}");
            bytes_a
        };
        hashes.insert(rel.clone(), Json::String(aac_core::sha256_hex(&norm_a)));
    }

    // Verify exact individual key oracle file digests, which do not follow the Scoreboard
    // catalog, from `node scripts/perf/memory-fixture.mjs` at origin/main bc96aecf5:
    assert_eq!(
        hashes["home/.ccs/status/answer-key.pem"].as_str(),
        Some("c3e6a7f3d4a367bef9dd68a4541d5cf2579aad93b495fd8fc4d38f94dbdfda3b")
    );
    assert_eq!(
        hashes["home/.ccs/status/answers.jsonl"].as_str(),
        Some("2a75cd5523918c56674cae75016a8beb954ab8c6fbfc4cf424add3f5cd371729")
    );
    assert_eq!(
        hashes["home/.ccs/models-dev-registry-cache.json"].as_str(),
        Some("9f8c0bedb1da3c00027343fb7f65d573eec52b82e4bf666b5d36458f3412db2a")
    );
    assert_eq!(
        hashes["agent-reports/v1/quota/report.json"].as_str(),
        Some("c568f4721e206eb478f8dca1eb9baaaefe6e5db69acb0ff33bf418eba5de208e")
    );
    assert_eq!(
        hashes["usage-logs/quota-history/quota-day-001.json"].as_str(),
        Some("8cc8fe67a9ff85922173b38021d16ada46c27030de3a775e823244be6a221056")
    );
    assert_eq!(
        hashes["usage-logs/activity-claude.jsonl"].as_str(),
        Some("ce22a4ea066e84d70da50244d59de7a015aa8af744d1a47b6e1e50f28f9c41ce")
    );
    // The fixture follows the live Scoreboard catalog (docs/agent-reports/catalog), which
    // changes with every model pin, so a frozen whole-fixture digest goes stale on each catalog
    // change. Compare every file against the retained JS oracle on the same tree instead. The
    // oracle's command line has no pricing flags, so call its exported generator directly.
    let tmp_js = tempfile::tempdir().expect("tempdir js");
    let out = Command::new("node")
        .args([
            "--input-type=module",
            "-e",
            "import { generateMemoryFixture } from './scripts/perf/memory-fixture.mjs';
const m = await generateMemoryFixture(process.argv[1], {
  seed: 424242, now: 1795608000000, writers: 16, runWriters: 4, runs: 110, registryEntries: 15,
  days: 3, accounts: 9, quotaSamplesPerDay: 2, activitySessions: 8,
  pricingProviders: 4, pricingModelsPerProvider: 3, skipStore: true,
});
console.log(JSON.stringify(m.counts));",
        ])
        .arg(tmp_js.path())
        .current_dir(repo_root())
        .output()
        .expect("node oracle");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let js_files: Vec<String> = walk_files(tmp_js.path())
        .into_iter()
        .filter(|f| f != "fixture-manifest.json")
        .collect();
    assert_eq!(js_files, files_a, "file list differs from the JS oracle");
    let js_root = tmp_js.path().to_string_lossy().to_string();
    for rel in &files_a {
        let js = fs::read(tmp_js.path().join(rel)).expect("read js");
        let rust = fs::read(tmp_a.path().join(rel)).expect("read rust");
        let is_text = [".json", ".jsonl", ".md", ".pub", ".pem"]
            .iter()
            .any(|ext| rel.ends_with(ext));
        if is_text {
            let js = String::from_utf8_lossy(&js).replace(&js_root, "<ROOT>");
            let rust = String::from_utf8_lossy(&rust).replace(&root_a_str, "<ROOT>");
            assert_eq!(js, rust, "JS oracle mismatch in {rel}");
        } else {
            assert_eq!(js, rust, "JS oracle byte mismatch in {rel}");
        }
    }
    let counts: Json = serde_json::from_slice(&out.stdout).expect("oracle counts");
    for (key, value) in [
        ("writers", manifest_a.counts.writers),
        ("runs", manifest_a.counts.runs),
        ("registryEntries", manifest_a.counts.registry_entries),
        ("providers", manifest_a.counts.providers),
        ("accounts", manifest_a.counts.accounts),
        (
            "legacySourceAccounts",
            manifest_a.counts.legacy_source_accounts,
        ),
        ("days", manifest_a.counts.days),
        ("quotaObservations", manifest_a.counts.quota_observations),
        ("activityEvents", manifest_a.counts.activity_events),
        ("activitySessions", manifest_a.counts.activity_sessions),
        ("pricingProviders", manifest_a.counts.pricing_providers),
        ("pricingModels", manifest_a.counts.pricing_models),
    ] {
        assert_eq!(counts[key], json!(value), "JS oracle count {key}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let key_mode = |dir: &Path| {
            fs::metadata(dir.join("home/.ccs/status/answer-key.pem"))
                .expect("answer key")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(key_mode(tmp_a.path()), 0o600);
        assert_eq!(key_mode(tmp_js.path()), 0o600);
    }
}

#[test]
fn fixture_migrates_directly_into_aac_store_without_node_binding() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let opts = FixtureOptions {
        seed: 424_242,
        now: Some(1_795_608_000_000),
        writers: 16,
        run_writers: 4,
        runs: 110,
        registry_entries: 15,
        days: 3,
        accounts: 9,
        quota_samples_per_day: 2,
        activity_sessions: 8,
        pricing_providers: 4,
        pricing_models_per_provider: 3,
        skip_store: false,
    };
    let manifest = generate_memory_fixture(&repo_root(), tmp.path(), &opts).expect("gen + store");
    let store = manifest.store.expect("store stats");
    assert!(store["quotaSampled"].as_u64().unwrap_or(0) > 0);
    assert_eq!(store["quotaRejected"].as_u64(), Some(0));
    assert!(store["activityReports"].as_u64().unwrap_or(0) > 0);
    assert!(store["activityCellsPut"].as_u64().unwrap_or(0) > 0);
    assert!(store["runsRecorded"].as_u64().unwrap_or(0) > 0);
    assert!(store["redbBytes"].as_u64().unwrap_or(0) > 0);
    assert!(tmp.path().join("home/.ccs/store/aac.redb").is_file());
}

#[test]
fn distributes_19_synthetic_accounts_legacy_rows_and_rng_matching_oracle() {
    let accounts = build_synthetic_accounts(19);
    assert_eq!(accounts.len(), 19);
    let providers: BTreeSet<&str> = accounts.iter().map(|a| a.provider.as_str()).collect();
    assert_eq!(providers.len(), 3);
    let identities: BTreeSet<&str> = accounts.iter().map(|a| a.identity.as_str()).collect();
    assert_eq!(identities.len(), 19);
    assert!(accounts.iter().all(|a| {
        a.email
            .as_deref()
            .is_some_and(|e| e.ends_with("@example.invalid"))
    }));

    let table: Json = serde_json::from_str(
        &fs::read_to_string(repo_root().join("config/tables/providers.json")).expect("providers"),
    )
    .expect("json");
    let additional: Vec<String> = table["additionalProviders"]
        .as_array()
        .expect("array")
        .iter()
        .map(|v| v.as_str().expect("str").to_string())
        .collect();
    let legacy = build_legacy_source_accounts(&repo_root()).expect("legacy accounts");
    let ids: Vec<String> = legacy.iter().map(|a| a.id.clone()).collect();
    let expected_ids: Vec<String> = additional.iter().map(|p| format!("{p}:usage")).collect();
    assert_eq!(ids, expected_ids);
    assert!(legacy.iter().all(|a| a.platform == "ubuntu"));
    for account in &legacy {
        let expected = aac_core::sha256_hex(
            serde_json::to_string(&(account.provider.as_str(), account.id.as_str(), Json::Null))
                .expect("json"),
        );
        assert_eq!(account.identity, expected);
    }

    let mut a = Rng::new(99);
    let mut b = Rng::new(99);
    let mut c = Rng::new(100);
    let seq_a: Vec<f64> = (0..10).map(|_| a.next_f64()).collect();
    let seq_b: Vec<f64> = (0..10).map(|_| b.next_f64()).collect();
    let seq_c: Vec<f64> = (0..10).map(|_| c.next_f64()).collect();
    assert_eq!(seq_a, seq_b);
    assert_ne!(seq_a, seq_c);
    assert_eq!(seq_a[0], 0.2604658124037087);
    assert_eq!(seq_a[9], 0.6709415460936725);
}

#[test]
fn parses_smaps_rollup_stat_and_descendants() {
    let rollup = parse_smaps_rollup(
        "55b8a000-7ffd0000 ---p 00000000 00:00 0 [rollup]\n\
         Rss:              256000 kB\n\
         Pss:              245760 kB\n\
         Pss_Anon:         220160 kB\n\
         Pss_File:          25600 kB\n\
         Swap:               1024 kB\n",
    );
    assert_eq!(rollup.rss_kib, 256_000);
    assert_eq!(rollup.pss_kib, Some(245_760));
    assert_eq!(rollup.pss_anon_kib, Some(220_160));
    assert_eq!(rollup.pss_file_kib, Some(25_600));
    assert_eq!(rollup.swap_kib, 1024);
    assert!(rollup.pss_available);

    let stat = parse_proc_stat(
        "1234 (ai-account (1)) S 42 1234 1234 0 -1 4194304 1 0 0 0 10 20 0 0 20 0 14 0 100 0 0",
    );
    assert_eq!(
        stat,
        Some(ProcStat {
            ppid: 42,
            threads: 14
        })
    );

    let parents = BTreeMap::from([(100, 1), (101, 100), (102, 100), (103, 101), (200, 1)]);
    let mut kids = collect_descendants(100, &parents);
    kids.sort();
    assert_eq!(kids, vec![101, 102, 103]);
}

#[test]
fn computes_steady_median_peaks_and_evaluates_thresholds() {
    let samples = vec![
        make_sample(0.0, 100 * 1024, 0, 90 * 1024),
        make_sample(60.0, 400 * 1024, 50 * 1024, 380 * 1024),
        make_sample(120.0, 200 * 1024, 10 * 1024, 190 * 1024),
        make_sample(180.0, 210 * 1024, 0, 200 * 1024),
        make_sample(240.0, 220 * 1024, 0, 210 * 1024),
    ];
    let summary = summarize_samples(&samples, 120.0, false).expect("summary");
    assert_eq!(summary.steady_median_rss_mib, 210.0);
    assert_eq!(summary.steady_median_pss_mib, Some(200.0));
    assert_eq!(summary.peak_rss_mib, 400.0);
    assert_eq!(summary.peak_pss_mib, Some(380.0));
    assert_eq!(summary.children_peak_rss_mib, 50.0);
    assert_eq!(summary.total_peak_rss_mib, 450.0);

    let pass = evaluate_thresholds(
        &summary,
        &json!({ "limits": { "steadyRssMiB": 220, "peakRssMiB": 410, "childrenPeakRssMiB": 60 } }),
    )
    .expect("pass");
    assert!(pass.ok);
    assert!(pass.violations.is_empty());

    let fail = evaluate_thresholds(
        &summary,
        &json!({ "limits": { "steadyRssMiB": 200, "peakRssMiB": 350, "childrenPeakRssMiB": 60 } }),
    )
    .expect("fail");
    assert!(!fail.ok);
    let metrics: Vec<&str> = fail.violations.iter().map(|v| v.metric.as_str()).collect();
    assert_eq!(metrics, vec!["steadyRssMiB", "peakRssMiB"]);
    assert_eq!(fail.violations[0].delta_mib, 10.0);
    assert_eq!(fail.violations[1].delta_mib, 50.0);
}

#[test]
fn rejects_misspelled_missing_non_finite_and_unknown_limit_keys() {
    let summary = summarize_samples(
        &[
            make_sample(0.0, 700 * 1024, 0, 700 * 1024),
            make_sample(120.0, 700 * 1024, 0, 700 * 1024),
            make_sample(240.0, 900 * 1024, 80 * 1024, 900 * 1024),
        ],
        120.0,
        false,
    )
    .expect("summary");

    let misspelled = json!({
        "schemaVersion": 1,
        "limits": { "steadyRssMib": 320, "peakRssMib": 651, "childrenPeakRssMib": 16 }
    });
    assert!(
        validate_threshold_config(&misspelled, false, "test")
            .expect_err("unknown")
            .to_string()
            .to_lowercase()
            .contains("unknown limit key")
    );
    assert!(
        evaluate_thresholds(&summary, &misspelled)
            .expect_err("unknown")
            .to_string()
            .to_lowercase()
            .contains("unknown limit key")
    );

    let extra = json!({
        "schemaVersion": 1,
        "limits": { "steadyRssMiB": 320, "peakRssMiB": 651, "childrenPeakRssMiB": 16, "extraLimit": 5 }
    });
    assert!(
        validate_threshold_config(&extra, false, "test")
            .expect_err("extra")
            .to_string()
            .to_lowercase()
            .contains("unknown limit key")
    );

    let missing = json!({
        "schemaVersion": 1,
        "limits": { "steadyRssMiB": 320, "peakRssMiB": 651 }
    });
    assert!(
        validate_threshold_config(&missing, false, "test")
            .expect_err("missing")
            .to_string()
            .contains("missing required limit \"childrenPeakRssMiB\"")
    );

    for bad in [json!(null), json!(-1), json!("320")] {
        let cfg = json!({
            "schemaVersion": 1,
            "limits": { "steadyRssMiB": bad, "peakRssMiB": 651, "childrenPeakRssMiB": 16 }
        });
        assert!(
            validate_threshold_config(&cfg, false, "test")
                .expect_err("bad val")
                .to_string()
                .contains("finite non-negative number")
        );
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let bad_path = tmp.path().join("bad-budget.json");
    fs::write(
        &bad_path,
        serde_json::to_string_pretty(&misspelled).expect("json"),
    )
    .expect("write");
    assert!(
        load_threshold_config(&bad_path)
            .expect_err("load bad")
            .to_string()
            .to_lowercase()
            .contains("unknown limit key")
    );

    // CLI fails immediately on bad config before touching --fixture-dir
    let cli_res = Command::new(env!("CARGO_BIN_EXE_aac-xtask"))
        .args([
            "perf",
            "--config",
            &bad_path.to_string_lossy(),
            "--fixture-dir",
            &tmp.path().join("does-not-exist").to_string_lossy(),
        ])
        .output()
        .expect("run cli");
    assert!(!cli_res.status.success());
    assert!(
        String::from_utf8_lossy(&cli_res.stderr)
            .to_lowercase()
            .contains("unknown limit key")
    );
}

#[test]
fn compares_unrounded_values_so_4_kib_over_fails_while_exact_passes() {
    let config =
        load_threshold_config(&repo_root().join("config/memory-budget.json")).expect("cfg");
    assert_eq!(config["schemaVersion"].as_u64(), Some(1));
    assert!(config["limits"]["steadyRssMiB"].as_f64().unwrap_or(0.0) > 0.0);
    assert!(
        config["limits"]["peakRssMiB"].as_f64().unwrap_or(0.0)
            >= config["limits"]["steadyRssMiB"].as_f64().unwrap_or(0.0)
    );

    let exact = summarize_samples(
        &[
            make_sample(0.0, 200 * 1024, 0, 200 * 1024),
            make_sample(60.0, 651 * 1024, 16 * 1024, 651 * 1024),
            make_sample(120.0, 320 * 1024, 0, 320 * 1024),
            make_sample(180.0, 320 * 1024, 0, 320 * 1024),
            make_sample(240.0, 320 * 1024, 0, 320 * 1024),
        ],
        120.0,
        false,
    )
    .expect("exact");
    let exact_res = evaluate_thresholds(&exact, &config).expect("eval");
    assert!(exact_res.ok);

    let over = summarize_samples(
        &[
            make_sample(0.0, 200 * 1024, 0, 200 * 1024),
            make_sample(60.0, 651 * 1024 + 4, 16 * 1024 + 4, 651 * 1024 + 4),
            make_sample(120.0, 320 * 1024 + 4, 0, 320 * 1024 + 4),
            make_sample(180.0, 320 * 1024 + 4, 0, 320 * 1024 + 4),
            make_sample(240.0, 320 * 1024 + 4, 0, 320 * 1024 + 4),
        ],
        120.0,
        false,
    )
    .expect("over");
    assert_eq!(over.peak_rss_mib, 651.0);
    assert_eq!(over.children_peak_rss_mib, 16.0);
    assert_eq!(over.steady_median_rss_mib, 320.0);

    let over_res = evaluate_thresholds(&over, &config).expect("eval over");
    assert!(!over_res.ok);
    let metrics: Vec<&str> = over_res
        .violations
        .iter()
        .map(|v| v.metric.as_str())
        .collect();
    assert_eq!(
        metrics,
        vec!["steadyRssMiB", "peakRssMiB", "childrenPeakRssMiB"]
    );
    for v in &over_res.violations {
        assert_eq!(v.delta_kib, 4.0);
        assert!(v.delta_mib > 0.0);
        assert!(v.measured_mib > v.limit_mib);
    }
}

#[test]
fn refuses_steady_fallback_and_insufficient_steady_samples_in_check_mode() {
    let early = vec![
        make_sample(0.0, 120 * 1024, 0, 120 * 1024),
        make_sample(2.0, 130 * 1024, 0, 130 * 1024),
    ];
    assert!(
        summarize_samples(&early, 120.0, false)
            .expect_err("no steady")
            .to_string()
            .contains("steady window")
    );

    let fallback = summarize_samples(&early, 120.0, true).expect("fallback");
    assert_eq!(fallback.steady_sample_count, 0);
    assert!(fallback.steady_used_fallback);
    let config =
        load_threshold_config(&repo_root().join("config/memory-budget.json")).expect("cfg");
    assert!(
        evaluate_thresholds(&fallback, &config)
            .expect_err("fallback threshold")
            .to_string()
            .contains("steady-window samples")
    );
    let sample_failures = evaluate_sample_failures(&fallback, 300.0, 120.0, 2.0);
    assert!(sample_failures.len() >= 2);

    let short = summarize_samples(
        &[
            make_sample(0.0, 100 * 1024, 0, 100 * 1024),
            make_sample(120.0, 200 * 1024, 0, 200 * 1024),
            make_sample(122.0, 205 * 1024, 0, 205 * 1024),
            make_sample(124.0, 210 * 1024, 0, 210 * 1024),
        ],
        120.0,
        false,
    )
    .expect("short");
    let short_failures = evaluate_sample_failures(&short, 300.0, 120.0, 2.0);
    assert!(
        short_failures
            .iter()
            .any(|m| m.contains("Insufficient steady-window samples"))
    );
    assert!(short_failures.iter().any(|m| m.contains("ended too early")));
}

#[test]
fn validates_run_options_cli_zero_duration_and_route_failures_including_last_poll() {
    let plan = default_poll_plan();
    let mut opts = RunOptions::default_for(&repo_root());
    opts.duration_sec = 0.0;
    assert!(
        validate_run_options(&opts, &plan)
            .expect_err("0 dur")
            .to_string()
            .contains("durationSec")
    );
    opts.check = false;
    assert!(
        validate_run_options(&opts, &plan)
            .expect_err("0 dur nc")
            .to_string()
            .contains("durationSec")
    );
    opts.duration_sec = 10.0;
    opts.check = true;
    assert!(
        validate_run_options(&opts, &plan)
            .expect_err("steady")
            .to_string()
            .contains("steadyAfterSec")
    );
    opts.check = false;
    assert!(validate_run_options(&opts, &plan).is_ok());

    let zero_cli = Command::new(env!("CARGO_BIN_EXE_aac-xtask"))
        .args(["perf", "--duration", "0"])
        .output()
        .expect("run zero cli");
    assert!(!zero_cli.status.success());
    assert!(String::from_utf8_lossy(&zero_cli.stderr).contains("durationSec"));

    let zero_routes = summarize_routes(&[], &plan);
    assert_eq!(
        evaluate_route_failures(&zero_routes, 300.0, &plan, true).len(),
        plan.len()
    );

    let mut late_records = Vec::new();
    for route in &plan {
        let mut idx = 0;
        let mut t = route.offset_sec;
        while t < 300.0 {
            late_records.push(RouteRecord {
                t_sec: t,
                route: route.name.clone(),
                status: if idx == 0 { 200 } else { 500 },
                bytes: if idx == 0 { 1024 } else { 0 },
                encoding: String::new(),
                ms: 5,
                error: None,
            });
            idx += 1;
            t += route.period_sec;
        }
    }
    let late_failures =
        evaluate_route_failures(&summarize_routes(&late_records, &plan), 300.0, &plan, true);
    assert_eq!(late_failures.len(), 6);
    assert!(late_failures.iter().all(|m| m.contains("polls failed")));

    let mut healthy = Vec::new();
    for route in &plan {
        let mut t = route.offset_sec;
        while t < 300.0 {
            healthy.push(RouteRecord {
                t_sec: t,
                route: route.name.clone(),
                status: 200,
                bytes: 1024,
                encoding: String::new(),
                ms: 5,
                error: None,
            });
            t += route.period_sec;
        }
    }
    assert!(
        evaluate_route_failures(&summarize_routes(&healthy, &plan), 300.0, &plan, true).is_empty()
    );

    for missing in &plan {
        let duration = 600.0;
        let mut recs = Vec::new();
        for route in &plan {
            let mut t = route.offset_sec;
            let mut route_recs = Vec::new();
            while t < duration {
                route_recs.push(RouteRecord {
                    t_sec: t,
                    route: route.name.clone(),
                    status: 200,
                    bytes: 1024,
                    encoding: String::new(),
                    ms: 5,
                    error: None,
                });
                t += route.period_sec;
            }
            if route.name == missing.name {
                route_recs.pop();
            }
            recs.extend(route_recs);
        }
        let failures =
            evaluate_route_failures(&summarize_routes(&recs, &plan), duration, &plan, true);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains(&format!("{}: only ", missing.name)));
    }
}

#[test]
fn detects_sigkilled_child_isolates_env_and_drops_node_artifact_check() {
    #[cfg(unix)]
    {
        let mut child = Command::new("sleep")
            .arg("60")
            .spawn()
            .expect("spawn sleep");
        child.kill().expect("kill");
        let status = child.wait().expect("wait");
        let info = exit_status_info(status);
        assert_eq!(info.exit_code, None);
        assert_eq!(info.signal_code.as_deref(), Some("SIGKILL"));
        assert_eq!(info.reason, "signal=SIGKILL");
    }

    let env = build_isolated_env(
        Path::new("/tmp/aac-test-home"),
        Path::new("/tmp/aac-test-home/.ccs"),
        Path::new("/tmp/aac-test-reports"),
        Path::new("/tmp/aac-test-reports/pm-mail-registry"),
        Path::new("/tmp/aac-test-stubs"),
        Path::new("/tmp/aac-test-xdg"),
    );
    for key in [
        "HOME",
        "CCS_HOME",
        "USERPROFILE",
        "CODEX_HOME",
        "CLAUDE_CONFIG_DIR",
        "XDG_CONFIG_HOME",
    ] {
        assert!(env.get(key).expect(key).starts_with("/tmp/aac-test-home"));
    }
    assert_eq!(
        env.get("AAC_STATUS_DOORBELL").map(String::as_str),
        Some("off")
    );
    assert_eq!(
        env.get("AAC_AGENT_TRANSPORT").map(String::as_str),
        Some("off")
    );
    assert_eq!(
        env.get("AAC_STORE_SHARE_BACKUP_DIR").map(String::as_str),
        Some("off")
    );

    let by_name: BTreeMap<String, f64> = default_poll_plan()
        .into_iter()
        .map(|r| (r.name, r.period_sec))
        .collect();
    assert_eq!(
        by_name,
        BTreeMap::from([
            ("accounts".into(), 60.0),
            ("analytics30d".into(), 300.0),
            ("analytics7d".into(), 60.0),
            ("home-mac".into(), 60.0),
            ("home-win".into(), 60.0),
            ("scoreboard".into(), 30.0),
            ("status".into(), 10.0),
        ])
    );

    // check_built_artifacts requires native binary + built UI, NOT dist/native/aac-store.*.node
    let tmp = tempfile::tempdir().expect("tempdir");
    let bin = tmp.path().join("dist/native/ai-account-center");
    let ui = tmp.path().join("dist/ui/app/index.html");
    fs::create_dir_all(bin.parent().expect("parent")).expect("mkdir");
    fs::create_dir_all(ui.parent().expect("parent")).expect("mkdir");
    fs::write(&bin, b"bin").expect("write bin");
    fs::write(&ui, b"<html/>").expect("write ui");
    assert_eq!(
        check_built_artifacts(tmp.path()).expect("no node required"),
        bin
    );
}

#[test]
fn tool_batch_1126_guards_pss_interval_symlinks_sampling_note_and_http_decoders() {
    // 1. RSS fallback marks PSS unavailable
    let fallback = parse_proc_status_rss("VmRSS: 78156 kB\nVmSwap: 0 kB\n");
    assert_eq!(fallback.rss_kib, 78_156);
    assert!(!fallback.pss_available);
    assert_eq!(fallback.pss_kib, None);

    // 2. Per-metric PSS coverage
    let sample = MemorySample {
        t_sec: 130.0,
        server_rss_kib: 1000,
        server_pss_kib: None,
        server_anon_kib: None,
        server_file_kib: None,
        server_swap_kib: 0,
        pss_available: false,
        server_pss_available: false,
        threads: 1,
        children_count: 1,
        children_rss_kib: 200,
        children_pss_kib: None,
        total_rss_kib: 1200,
        total_pss_kib: None,
    };
    let mem = summarize_samples(std::slice::from_ref(&sample), 120.0, false).expect("mem");
    let rss_cfg = json!({ "schemaVersion": 1, "limits": { "steadyRssMiB": 100, "peakRssMiB": 100, "childrenPeakRssMiB": 100 } });
    let srv_cfg = json!({ "schemaVersion": 1, "limits": { "steadyRssMiB": 100, "peakRssMiB": 100, "childrenPeakRssMiB": 100, "steadyPssMiB": 50 } });
    let kid_cfg = json!({ "schemaVersion": 1, "limits": { "steadyRssMiB": 100, "peakRssMiB": 100, "childrenPeakRssMiB": 100, "childrenPeakPssMiB": 50 } });
    assert!(!mem.pss_available);
    assert_eq!(mem.steady_median_pss_kib, None);
    assert!(
        evaluate_thresholds(&mem, &srv_cfg)
            .expect_err("no pss")
            .to_string()
            .contains("PSS coverage")
    );
    assert!(evaluate_thresholds(&mem, &rss_cfg).expect("rss ok").ok);

    let mut srv_measured = sample;
    srv_measured.server_pss_kib = Some(900);
    srv_measured.server_pss_available = true;
    let mem2 = summarize_samples(&[srv_measured], 120.0, false).expect("mem2");
    assert!(!mem2.pss_available);
    assert!(evaluate_thresholds(&mem2, &srv_cfg).expect("srv pss ok").ok);
    assert_eq!(mem2.total_peak_pss_kib, None);
    assert!(
        evaluate_thresholds(&mem2, &kid_cfg)
            .expect_err("kid pss")
            .to_string()
            .contains("PSS coverage")
    );

    // 3. Sub-200ms sample interval rejected in both check and measure-only modes
    let plan = default_poll_plan();
    for (interval, check, should_ok) in [
        (0.05, true, false),
        (0.19, true, false),
        (0.2, true, true),
        (0.05, false, false),
    ] {
        let mut opts = RunOptions::default_for(&repo_root());
        opts.duration_sec = 10.0;
        opts.steady_after_sec = 5.0;
        opts.sample_interval_sec = interval;
        opts.check = check;
        let res = validate_run_options(&opts, &plan);
        assert_eq!(res.is_ok(), should_ok, "interval={interval} check={check}");
        if !should_ok {
            assert!(res.expect_err("err").to_string().contains(">= 0.2"));
        }
    }

    // 4. Symlink and FIFO rejection
    #[cfg(unix)]
    {
        let root = tempfile::tempdir().expect("tempdir");
        let fixture = root.path().join("fixture");
        let outside = root.path().join("outside");
        let home = fixture.join("home");
        fs::create_dir_all(&home).expect("mkdir");
        fs::create_dir_all(&outside).expect("mkdir");
        let fifo = root.path().join("pipe");
        assert!(
            Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("mkfifo")
                .success()
        );
        assert!(
            assert_no_symlinks(&fifo)
                .expect_err("fifo")
                .to_string()
                .contains("unexpected file type")
        );
        let linked = home.join(".ccs");
        std::os::unix::fs::symlink(&outside, &linked).expect("symlink");
        assert!(
            assert_no_symlinks(&fixture)
                .expect_err("symlink")
                .to_string()
                .contains("symlink")
        );
        assert!(
            assert_resolved_inside(&fixture, &linked)
                .expect_err("escape")
                .to_string()
                .contains("escapes the fixture")
        );
        assert!(assert_resolved_inside(&fixture, &home).is_ok());
        assert!(
            generate_memory_fixture(&repo_root(), &linked, &FixtureOptions::default())
                .expect_err("gen symlink")
                .to_string()
                .contains("symlink")
        );

        let mut sym_opts = RunOptions::default_for(&repo_root());
        sym_opts.duration_sec = 1.0;
        sym_opts.steady_after_sec = 0.0;
        sym_opts.sample_interval_sec = 0.2;
        sym_opts.check = false;
        sym_opts.fixture_dir = Some(fixture);
        sym_opts.launcher_path = Some(PathBuf::from("/nonexistent-must-not-start"));
        assert!(
            run_memory_budget(&repo_root(), &sym_opts)
                .expect_err("symlink fixture")
                .to_string()
                .contains("symlink")
        );
    }

    // 5. Sampling note
    assert!(mem.sampling_note.contains("between samples"));
    assert!(mem.sampling_note.contains("missed"));

    // 6. Content-Encoding decoders + chunked HTTP client
    use flate2::Compression;
    use flate2::write::{GzEncoder, ZlibEncoder};
    let raw = br#"{"hello":"world"}"#;
    assert_eq!(decode_content_encoding("", raw).expect("raw"), raw);
    assert_eq!(decode_content_encoding("identity", raw).expect("id"), raw);
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(raw).expect("gz");
    assert_eq!(
        decode_content_encoding("gzip", &gz.finish().expect("fin")).expect("dec gz"),
        raw
    );
    let mut zl = ZlibEncoder::new(Vec::new(), Compression::default());
    zl.write_all(raw).expect("zl");
    assert_eq!(
        decode_content_encoding("deflate", &zl.finish().expect("fin")).expect("dec zl"),
        raw
    );
    assert!(decode_content_encoding("zstd-unsupported", raw).is_err());

    // Chunked + keep-alive HTTP client check
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let srv = thread::spawn(move || {
        let (mut sock, _) = listener.accept().expect("accept");
        let mut buf = [0_u8; 1024];
        let _ = sock.read(&mut buf);
        sock.write_all(b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n")
            .expect("write chunked");
        let _ = sock.read(&mut buf);
        sock.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .expect("write second");
    });
    let abort = AtomicBool::new(false);
    let mut client = HttpClient::new(port, true);
    let r1 = client
        .request("GET", "/chunked", &[], None, Duration::from_secs(5), &abort)
        .expect("r1");
    assert_eq!(r1.status, 200);
    assert_eq!(r1.body, b"hello world");
    let r2 = client
        .request("GET", "/second", &[], None, Duration::from_secs(5), &abort)
        .expect("r2");
    assert_eq!(r2.status, 200);
    assert_eq!(r2.body, b"ok");
    srv.join().expect("srv");
}

#[cfg(target_os = "linux")]
#[test]
fn e2e_runner_truncated_startup_analytics_sigkill_flaky_and_healthy() {
    let tmp = tempfile::tempdir().expect("tempdir");

    // 1. Truncated response during loopback sign-in fails
    let trunc_script =
        write_fake_dashboard_py(tmp.path(), "trunc.py", false, "/api/auth/setup", 0, "ok");
    let mut trunc_opts = RunOptions::default_for(&repo_root());
    trunc_opts.duration_sec = 1.0;
    trunc_opts.steady_after_sec = 0.0;
    trunc_opts.sample_interval_sec = 0.2;
    trunc_opts.check = false;
    trunc_opts.launcher_path = Some(PathBuf::from("python3"));
    trunc_opts.launcher_args = Some(vec![
        trunc_script.to_string_lossy().into(),
        "--port".into(),
        "{port}".into(),
    ]);
    let trunc_err = run_memory_budget(&repo_root(), &trunc_opts)
        .expect_err("truncated must fail")
        .to_string();
    assert!(
        trunc_err.contains("/api/auth/setup") || trunc_err.contains("during loopback sign-in"),
        "got: {trunc_err}"
    );

    // 2. Startup peak allocation included in peak RSS and excluded from steady RSS
    let startup_script = write_fake_dashboard_py(tmp.path(), "startup.py", false, "", 160, "ok");
    let tight_cfg = tmp.path().join("tight.json");
    fs::write(&tight_cfg, r#"{"schemaVersion":1,"limits":{"steadyRssMiB":128,"peakRssMiB":128,"childrenPeakRssMiB":16}}"#).expect("write");
    let mut startup_opts = RunOptions::default_for(&repo_root());
    startup_opts.duration_sec = 1.0;
    startup_opts.steady_after_sec = 0.0;
    startup_opts.sample_interval_sec = 0.2;
    startup_opts.check = true;
    startup_opts.config_path = tight_cfg;
    startup_opts.poll_plan = Some(vec![PollRoute {
        name: "status".into(),
        path: "/api/status".into(),
        period_sec: 2.0,
        offset_sec: 0.0,
    }]);
    startup_opts.launcher_path = Some(PathBuf::from("python3"));
    startup_opts.launcher_args = Some(vec![
        startup_script.to_string_lossy().into(),
        "--port".into(),
        "{port}".into(),
    ]);
    let startup_summary = run_memory_budget(&repo_root(), &startup_opts).expect("startup run");
    assert!(startup_summary.memory.steady_median_rss_mib < 128.0);
    assert!(startup_summary.memory.peak_rss_mib > 160.0);
    let vio: Vec<&str> = startup_summary
        .threshold
        .as_ref()
        .expect("thresh")
        .violations
        .iter()
        .map(|v| v.metric.as_str())
        .collect();
    assert_eq!(vio, vec!["peakRssMiB"]);
    assert!(!startup_summary.ok);

    // 3. Analytics compressed reply verification
    let analytics_route = default_poll_plan()
        .into_iter()
        .find(|r| r.name == "analytics30d")
        .expect("a30");
    for (mode, expected_ok) in [("ok", true), ("http-error", false), ("unavailable", false)] {
        let s = write_fake_dashboard_py(tmp.path(), &format!("an_{mode}.py"), false, "", 0, mode);
        let mut opts = RunOptions::default_for(&repo_root());
        opts.duration_sec = 0.3;
        opts.steady_after_sec = 0.0;
        opts.sample_interval_sec = 0.2;
        opts.check = false;
        opts.poll_plan = Some(vec![PollRoute {
            offset_sec: 0.0,
            ..analytics_route.clone()
        }]);
        opts.launcher_path = Some(PathBuf::from("python3"));
        opts.launcher_args = Some(vec![
            s.to_string_lossy().into(),
            "--port".into(),
            "{port}".into(),
        ]);
        let sum = run_memory_budget(&repo_root(), &opts).expect("analytics run");
        assert_eq!(sum.ok, expected_ok, "mode={mode}");
        let row = sum.routes.get("analytics30d").expect("route");
        assert_eq!(row.calls, 1);
        assert_eq!(row.ok, usize::from(expected_ok));
        assert_eq!(row.failed, usize::from(!expected_ok));
    }

    // 4. SIGKILLed child, flaky late poll, and healthy pass
    let generous_cfg = tmp.path().join("generous.json");
    fs::write(&generous_cfg, r#"{"schemaVersion":1,"limits":{"steadyRssMiB":2048,"peakRssMiB":2048,"childrenPeakRssMiB":512}}"#).expect("write");
    let fast_plan = vec![
        PollRoute {
            name: "status".into(),
            path: "/api/status".into(),
            period_sec: 0.4,
            offset_sec: 0.0,
        },
        PollRoute {
            name: "accounts".into(),
            path: "/api/accounts/dashboard".into(),
            period_sec: 0.5,
            offset_sec: 0.1,
        },
    ];
    let ok_script = write_fake_dashboard_py(tmp.path(), "ok.py", false, "", 0, "ok");

    let mut kill_opts = RunOptions::default_for(&repo_root());
    kill_opts.duration_sec = 4.0;
    kill_opts.steady_after_sec = 1.5;
    kill_opts.sample_interval_sec = 0.25;
    kill_opts.seed = 1;
    kill_opts.config_path = generous_cfg.clone();
    kill_opts.check = true;
    kill_opts.poll_plan = Some(fast_plan.clone());
    kill_opts.launcher_path = Some(PathBuf::from("python3"));
    kill_opts.launcher_args = Some(vec![
        ok_script.to_string_lossy().into(),
        "--port".into(),
        "{port}".into(),
    ]);
    kill_opts.kill_after_ms = Some(350);
    let kill_err = run_memory_budget(&repo_root(), &kill_opts)
        .expect_err("sigkill")
        .to_string();
    assert!(kill_err.contains("signal=SIGKILL"), "got: {kill_err}");

    let flaky_script = write_fake_dashboard_py(tmp.path(), "flaky.py", true, "", 0, "ok");
    let mut flaky_opts = RunOptions::default_for(&repo_root());
    flaky_opts.duration_sec = 1.8;
    flaky_opts.steady_after_sec = 0.5;
    flaky_opts.sample_interval_sec = 0.2;
    flaky_opts.seed = 1;
    flaky_opts.config_path = generous_cfg.clone();
    flaky_opts.check = true;
    flaky_opts.poll_plan = Some(fast_plan.clone());
    flaky_opts.launcher_path = Some(PathBuf::from("python3"));
    flaky_opts.launcher_args = Some(vec![
        flaky_script.to_string_lossy().into(),
        "--port".into(),
        "{port}".into(),
    ]);
    let flaky_sum = run_memory_budget(&repo_root(), &flaky_opts).expect("flaky");
    assert!(!flaky_sum.ok);
    assert!(flaky_sum.route_failures.len() >= 2);
    assert!(
        flaky_sum
            .route_failures
            .iter()
            .all(|m| m.contains("polls failed"))
    );

    let mut ok_opts = flaky_opts;
    ok_opts.launcher_args = Some(vec![
        ok_script.to_string_lossy().into(),
        "--port".into(),
        "{port}".into(),
    ]);
    let ok_sum = run_memory_budget(&repo_root(), &ok_opts).expect("healthy");
    assert!(ok_sum.ok);
    assert!(ok_sum.route_failures.is_empty());
    assert!(ok_sum.sample_failures.is_empty());
    assert!(!ok_sum.memory.steady_used_fallback);
    assert!(ok_sum.memory.steady_sample_count >= 3);
    assert!(ok_sum.threshold.as_ref().expect("thresh").ok);
    let human = format_human_summary(&ok_sum);
    assert!(human.contains("[OK] Memory within all configured limits:"));
    assert!(human.contains("A helper that starts and exits between samples may be missed."));
    let serialized = serde_json::to_value(&ok_sum).expect("serialize summary");
    assert!(serialized["memory"]["steadyMedianRssKiB"].is_number());
    assert!(serialized["memory"]["steadyMedianRssMiB"].is_number());
    assert!(serialized["memory"]["peakRssKiB"].is_number());
    assert!(serialized["memory"]["peakRssMiB"].is_number());
    assert!(serialized["memory"]["childrenPeakRssKiB"].is_number());
    assert!(serialized["threshold"]["results"][0]["measuredMiB"].is_number());
    assert!(serialized["threshold"]["results"][0]["limitKiB"].is_number());
}

#[test]
fn cli_memory_fixture_generates_and_validates_flags() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = Command::new(env!("CARGO_BIN_EXE_aac-xtask"))
        .args(["perf", "memory-fixture"])
        .output()
        .expect("missing out-dir");
    assert_eq!(missing.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&missing.stderr).contains("Usage:"));

    let out_dir = tmp.path().join("cli-fix");
    let generated = Command::new(env!("CARGO_BIN_EXE_aac-xtask"))
        .args([
            "perf",
            "memory-fixture",
            &out_dir.to_string_lossy(),
            "--seed",
            "424242",
            "--now",
            "1795608000000",
            "--writers",
            "16",
            "--run-writers",
            "4",
            "--runs",
            "110",
            "--registry",
            "15",
            "--days",
            "3",
            "--accounts",
            "9",
            "--quota-per-day",
            "2",
            "--sessions",
            "8",
            "--pricing-providers",
            "4",
            "--pricing-models",
            "3",
            "--skip-store",
        ])
        .output()
        .expect("gen cli");
    assert!(
        generated.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&generated.stderr)
    );
    let stdout = String::from_utf8_lossy(&generated.stdout);
    assert!(stdout.contains("[OK] Generated synthetic fixture"));
    assert!(stdout.contains("writers=16 runs=110 registry=15"));
    assert!(stdout.contains("store: skipped (--skip-store)"));
    assert!(out_dir.join("fixture-manifest.json").is_file());
}
