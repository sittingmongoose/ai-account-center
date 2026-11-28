#![cfg(test)]
//! A live-sized twin of one busy computer's Analytics activity pass, with a counting allocator:
//! the heap of each step of the local scan (checkpoint load, scan request, agent call, answer,
//! apply and aggregation, checkpoint save) in exact bytes. Built only with the `memory-twin`
//! feature; run alone, single-threaded:
//!
//! `cargo test -p aac-http --features memory-twin --lib memory_twin -- --ignored --nocapture --test-threads=1`
//!
//! The checkpoint is synthetic but shaped like the live `ubuntu` one (about 40 MB: tens of
//! thousands of per-file prints, most of them settled, each with a reader `appendState`, and
//! about 100k hour and session rows); the agent's answer is what a steady pass returns: every
//! unsettled print, a confirmation per settled one, and the rows of the files that changed.
//!
//! The soak (MEM7) runs whole passes over seven live-shaped computers for `AAC_TWIN_MINUTES`
//! (default 20), each scan a real local child process through the transport that waits
//! `AAC_TWIN_WAIT_MS` (default 3000) before it answers, and samples the exact heap and RSS
//! (tuned mimalloc underneath, as the service runs) every 50 ms:
//!
//! `cargo test -p aac-http --release --features memory-twin --lib memory_twin_soak -- --ignored --nocapture --test-threads=1`
//!
//! The whole pass (spike 7) runs one pass over the seven computers without the transport, then
//! the pass's end (Claude mirror owners, reports, trim, typed contract, ingest), both the way the
//! pass runs now and the way it held every worker result to its end, and checks they ingest
//! the same typed reports. The Linux default allocator feature is left out, so the counter
//! sits on mimalloc as the other twins expect:
//!
//! `cargo test -p aac-http --no-default-features --features memory-twin --lib memory_twin_whole_pass -- --ignored --nocapture --test-threads=1`
use super::scan_answer::Answer;
use super::scan_state::{self, Cache};
use aac_store::collect;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};
use std::alloc::{GlobalAlloc, Layout};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

/// The service binaries' allocator.
static UNDER: mimalloc::MiMalloc = mimalloc::MiMalloc;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged to the service's allocator.
        let ptr = unsafe { UNDER.alloc(layout) };
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the pointer came from `alloc` with this layout.
        unsafe { UNDER.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged to the service's allocator.
        let out = unsafe { UNDER.realloc(ptr, layout, size) };
        if !out.is_null() {
            if size >= layout.size() {
                let now = CURRENT.fetch_add(size - layout.size(), Ordering::Relaxed) + size
                    - layout.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                CURRENT.fetch_sub(layout.size() - size, Ordering::Relaxed);
            }
        }
        out
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const MIB: f64 = 1024.0 * 1024.0;
const DAY: i64 = 86_400_000;
const HOST: &str = "ubuntu";

fn mib(bytes: usize) -> f64 {
    bytes as f64 / MIB
}

/// Peak heap above the level when it started, and the level left when it ended.
struct Step {
    name: &'static str,
    peak: usize,
    left: isize,
}

struct Meter {
    pass_base: usize,
    steps: Vec<Step>,
}

impl Meter {
    fn new() -> Self {
        let now = CURRENT.load(Ordering::Relaxed);
        PEAK.store(now, Ordering::Relaxed);
        Meter {
            pass_base: now,
            steps: Vec::new(),
        }
    }

    /// Run one step; its peak counts from the pass's starting level (what the process holds
    /// above its idle heap at that moment).
    fn step<T>(&mut self, name: &'static str, run: impl FnOnce() -> T) -> T {
        let start = CURRENT.load(Ordering::Relaxed);
        PEAK.store(start, Ordering::Relaxed);
        let out = run();
        let peak = PEAK.load(Ordering::Relaxed);
        let end = CURRENT.load(Ordering::Relaxed);
        self.steps.push(Step {
            name,
            peak: peak.saturating_sub(self.pass_base),
            left: end as isize - start as isize,
        });
        out
    }

    fn report(&self, title: &str) -> usize {
        let pass_peak = self.steps.iter().map(|s| s.peak).max().unwrap_or(0);
        println!("\n== {title}");
        println!("{:<44} {:>14} {:>14}", "step", "peak MiB", "held after");
        for step in &self.steps {
            println!(
                "{:<44} {:>14.1} {:>+14.1}",
                step.name,
                mib(step.peak),
                step.left as f64 / MIB
            );
        }
        println!("{:<44} {:>14.1}", "pass peak above idle", mib(pass_peak));
        pass_peak
    }
}

/// Deterministic pseudo-random hex.
fn hex(seed: u64, len: usize) -> String {
    let mut state = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
    let mut out = String::with_capacity(len);
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.push_str(&format!("{state:016x}"));
    }
    out.truncate(len);
    out
}

/// The twin's tools: the shared table's first four analytics tools, with each one's fixture
/// shape in that table order (prints, settled share in percent, hour rows per used file).
fn tools() -> [(&'static str, usize, usize, usize); 4] {
    let mut ids = aac_providers::ANALYTICS_TOOLS.iter().map(|tool| tool.id);
    [
        (ids.next().expect("first analytics tool"), 16_000, 92, 4),
        (ids.next().expect("second analytics tool"), 6_000, 90, 6),
        (ids.next().expect("third analytics tool"), 1_200, 85, 3),
        (ids.next().expect("fourth analytics tool"), 800, 85, 3),
    ]
}
/// Files changed since the last pass (re-read whole), per tool.
const CHANGED: usize = 40;

struct Twin {
    /// The pass being run (its changed files differ from the last pass's).
    pass: std::cell::Cell<u64>,
    now: i64,
    slot: u64,
    settled_before: HashMap<String, u64>,
    /// This computer's size against the busy one's.
    scale: f64,
}

impl Twin {
    fn new() -> Self {
        Self::scaled(1.0)
    }

    fn scaled(scale: f64) -> Self {
        let now = 1_795_212_000_000;
        Twin {
            scale,
            pass: std::cell::Cell::new(1),
            now,
            slot: now as u64 / aac_usage::collector::SETTLED_CHECK_MS,
            settled_before: tools()
                .into_iter()
                .map(|(kind, ..)| (kind.to_owned(), (now - 3 * DAY) as u64))
                .collect(),
        }
    }

    fn count(&self, count: usize) -> usize {
        ((count as f64 * self.scale) as usize).max(20)
    }

    fn key(kind: &str, index: usize) -> String {
        format!(
            "{kind}:/home/fixture/.{kind}/projects/-home-fixture-src-project-{}/{}.jsonl",
            index % 97,
            hex(index as u64 * 31 + kind.len() as u64, 32)
        )
    }

    fn settled(index: usize, share: usize) -> bool {
        index % 100 < share
    }

    fn print(&self, kind: &str, index: usize, settled: bool, generation: u64) -> Value {
        let seed = index as u64 * 7 + kind.len() as u64;
        let age = if settled { 5 + (index % 25) as i64 } else { 0 };
        let mtime = self.now - age * DAY - (index as i64 % 3_600_000) - generation as i64 * 1000;
        let size = 40_000 + (index as u64 * 977) % 4_000_000 + generation * 512;
        let mut print = json!({
            "pathKey": hex(seed ^ 0xabc, 64),
            "size": size,
            "mtimeMs": mtime,
            "head": hex(seed ^ 0x11, 40),
            "tail": hex(seed ^ 0x22 ^ generation, 40),
            "fileIdentity": format!("2049:{}", 1_000_000 + index),
            "newestUsageMs": mtime - 60_000,
            "usageClock": true,
            "appendState": {
                "model": "claude-opus-4-fixture",
                "sessionId": hex(seed ^ 0x33, 36),
                "lastTimestamp": mtime - 60_000,
                "pending": "",
                "seen": [hex(seed ^ 0x44, 24), hex(seed ^ 0x55, 24)],
                "carry": {"inputTokens": index % 5000, "outputTokens": index % 700,
                          "cwd": format!("/home/fixture/src/project-{}", index % 97)},
            },
        });
        if kind == "claude" {
            print["claudeName"] = json!(hex(seed ^ 0x66, 64));
            print["claudeMirror"] = json!(index.is_multiple_of(11));
        }
        print
    }

    fn rows(&self, kind: &str, index: usize, per_file: usize, generation: u64) -> Vec<Value> {
        let key = Self::key(kind, index);
        let start = self.now - (index as i64 % 28) * DAY;
        (0..per_file)
            .map(|hour| {
                let at = start - hour as i64 * 3_600_000;
                let hour = aac_num::ecma::to_iso_string(at).unwrap_or_default();
                json!({"k":kind,"f":key,"m":format!("model-{}", index % 6),
                    "h": format!("{} {}", &hour[..10], &hour[11..13]),
                    "i": 1000 + index as u64 + generation, "o": 200, "cr": 5000, "cw": 300,
                    "c": 0, "n": 3})
            })
            .collect()
    }

    fn session(&self, kind: &str, index: usize, generation: u64) -> Value {
        let at = self.now - (index as i64 % 28) * DAY;
        json!({"k":kind,"f":Self::key(kind, index),"s":hex(index as u64 ^ 0x77, 36),
            "m":format!("model-{}", index % 6),"a": at - 7_200_000, "z": at,
            "i": 1000 + index as u64 + generation, "o": 200, "cr": 5000, "cw": 300, "c": 0, "n": 3})
    }

    /// The stored checkpoint (as one revision 1 document).
    fn checkpoint(&self) -> Value {
        let mut fingerprints = Map::new();
        let mut rows = Vec::new();
        let mut srows = Vec::new();
        let mut states = Map::new();
        for (kind, count, share, per_file) in tools() {
            let count = self.count(count);
            let mut prints = Map::new();
            for index in 0..count {
                prints.insert(
                    Self::key(kind, index),
                    self.print(kind, index, Self::settled(index, share), 0),
                );
                if index.is_multiple_of(2) {
                    rows.extend(self.rows(kind, index, per_file, 0));
                    srows.push(self.session(kind, index, 0));
                }
            }
            fingerprints.insert(kind.to_owned(), Value::Object(prints));
            states.insert(kind.to_owned(), json!({"state":"ok"}));
        }
        json!({"_nativeRevision":1,"fingerprints":fingerprints,"rows":rows,"srows":srows,
            "states":states})
    }

    /// The agent's answer for `kinds`: every unsettled print (the first CHANGED of them re-read
    /// with new rows), a confirmation per settled print, and identity evidence.
    fn answer(&self, kinds: &[String], pass: u64) -> Value {
        let mut out_kinds = Map::new();
        let mut rows = Vec::new();
        let mut srows = Vec::new();
        let mut discovery = Map::new();
        let mut sessions = Map::new();
        for (kind, count, share, per_file) in tools() {
            if !kinds.iter().any(|k| k == kind) {
                continue;
            }
            let count = self.count(count);
            let mut prints = Map::new();
            let mut changed = 0;
            for index in 0..count {
                let key = Self::key(kind, index);
                if Self::settled(index, share) {
                    prints.insert(
                        key,
                        json!({"settledConfirmed":true,"settledCheckedSlot":self.slot}),
                    );
                    continue;
                }
                let generation = if changed < CHANGED { pass } else { 0 };
                if generation > 0 {
                    changed += 1;
                    rows.extend(self.rows(kind, index, per_file, generation));
                    srows.push(self.session(kind, index, generation));
                }
                prints.insert(key, self.print(kind, index, false, generation));
            }
            out_kinds.insert(
                kind.to_owned(),
                json!({"state":"ok","settledPacked":true,"fingerprints":prints,"replacedFiles":[]}),
            );
            discovery.insert(
                kind.to_owned(),
                json!({"freshness":{"state":"current","newestDataMs":self.now - 60_000,
                    "newestActivityMs":self.now - 60_000,"activityComplete":true},
                    "counts":{"filesFound":count,"filesRead":CHANGED},
                    "roots":[{"path":format!("/home/fixture/.{kind}"),"origin":"default"}]}),
            );
            for index in 0..count / 4 {
                sessions.insert(
                    hex(index as u64 ^ 0x77, 36),
                    json!({"cwd":format!("/home/fixture/src/project-{}", index % 97),
                        "model":"fixture","kind":kind}),
                );
            }
        }
        json!({"version":2,"truncated":false,"discoveryTruncated":false,"kinds":out_kinds,
            "rows":rows,"srows":srows,"discovery":{"kinds":discovery},
            "reader":{"name":"aac-usage-scan","version":"fixture"},"sessions":sessions})
    }
}

/// The agent's terminal line for `value`.
fn result_line(id: &str, value: &Value) -> Vec<u8> {
    let mut line = format!(
        r#"{{"type":"result","protocol":1,"id":{},"value":"#,
        json!(id)
    )
    .into_bytes();
    serde_json::to_writer(&mut line, value).expect("answer");
    line.extend_from_slice(b"}\n");
    line
}

fn store(dir: &std::path::Path) -> aac_store::engine::Engine {
    aac_store::ops::open_engine(&dir.join("store").to_string_lossy(), None, None)
        .expect("twin store")
}

/// One activity pass of the busy computer, as `activity_pass` runs it.
fn pass(
    engine: &aac_store::engine::Engine,
    twin: &Twin,
    lines: &HashMap<String, Vec<u8>>,
) -> usize {
    let pricing = super::ResolverPricing::new(std::path::Path::new("/nonexistent-twin"));
    let kinds: Vec<String> = tools().into_iter().map(|(k, ..)| k.to_owned()).collect();
    let cutoff = twin.now - 31 * DAY;
    let mut meter = Meter::new();
    let mut cache = meter.step("load checkpoint", || {
        Cache::load(engine, HOST).expect("load")
    });
    let requests = meter.step("size the scan requests", || {
        let args = super::scan_request(&kinds, cutoff, json!({}), false, None);
        scan_state::Requests::new(
            args,
            &kinds,
            &cache.fingerprints,
            &twin.settled_before,
            twin.now,
        )
    });
    let mut remote = Vec::new();
    let mut states = Vec::new();
    let mut claude_state = None;
    for batch in requests.batches.clone() {
        let args = meter
            .step("build one scan request", || {
                requests.args(&batch, &cache.fingerprints)
            })
            .expect("request fits");
        let packed: Vec<bool> = batch
            .iter()
            .map(|kind| args.packed.contains(kind))
            .collect();
        let parked = meter.step("set the checkpoint aside", || cache.park());
        let bytes = meter.step("agent call: frame, envelope, answer bytes", || {
            // call_controlled_args: the arguments are checked and written as they are held,
            // inside a small envelope; then the agent's output.
            let inner: BTreeMap<String, &RawValue> =
                serde_json::from_str(args.text.get()).expect("arguments");
            drop(inner);
            let input = aac_transport::Stdin::default()
                .part(std::sync::Arc::new(
                    br#"{"protocol":1,"verb":"scan-usage","id":"twin","deadlineMs":120000,"args":"#
                        .to_vec(),
                ))
                .part(args.text.clone())
                .part(std::sync::Arc::new(b"}\n"));
            let line = &lines[&format!("{}:{}", twin.pass.get(), batch.join(","))];
            // The streaming buffer and the collected output hold the terminal line.
            let buffer = line.clone();
            let mut stdout = line.clone();
            drop(buffer);
            let fields: BTreeMap<String, &RawValue> =
                serde_json::from_slice(&stdout[..stdout.len() - 1]).expect("event");
            let value = fields["value"].get();
            let start = value.as_ptr() as usize - stdout.as_ptr() as usize;
            let end = start + value.len();
            drop(fields);
            stdout.truncate(end);
            stdout.drain(..start);
            drop(input);
            stdout
        });
        drop(args);
        cache = meter.step("read the checkpoint back", || {
            parked.restore(engine, HOST).expect("restore")
        });
        let mut answer = meter.step("read the answer", || {
            Answer::parse(&bytes, &mut cache.strings).expect("answer")
        });
        drop(bytes);
        meter.step("apply and aggregate", || {
            assert!(super::valid_scan_answer(&answer, &batch));
            for (kind, packed) in batch.iter().zip(packed) {
                let source = super::scanned_kind(
                    &mut answer,
                    kind,
                    HOST,
                    (twin.now, cutoff),
                    (packed, false, false),
                    &mut cache,
                );
                assert_eq!(source["state"], "ok", "{kind}: {source}");
                if kind == "claude" {
                    claude_state = Some(states.len());
                } else {
                    remote.push(collect::worker_from_rows(
                        &scan_state::of_kind(&cache.rows, kind),
                        &scan_state::of_kind(&cache.srows, kind),
                        HOST,
                        twin.now,
                        &pricing,
                    ));
                }
                states.push(source);
            }
        });
        drop(answer);
    }
    let written = meter.step("save checkpoint", || {
        cache.save(engine, HOST).expect("save")
    });
    println!(
        "checkpoint written: {} keys, {:.2} MiB",
        written.keys,
        mib(written.bytes)
    );
    let part = meter.step("Claude part out of the cache", || {
        claude_state.map(|_| cache.take_kind("claude"))
    });
    drop(part);
    drop(remote);
    meter.step("drop the cache", || drop(cache));
    meter.report("pass (typed prints and answer, checkpoint in parts)")
}

#[test]
#[ignore = "memory twin: run alone with --features memory-twin --ignored --test-threads=1"]
fn memory_twin_busy_computer_pass() {
    let dir = tempfile::tempdir().expect("twin dir");
    let engine = store(dir.path());
    let twin = Twin::new();
    let checkpoint = serde_json::to_vec(&twin.checkpoint()).expect("checkpoint");
    println!("checkpoint document: {:.1} MiB", mib(checkpoint.len()));
    aac_store::analytics_cache::put(&engine, &format!("live/scanner/{HOST}"), &checkpoint)
        .expect("seed checkpoint");
    drop(checkpoint);
    // The answers the agent gives each request of this pass, prepared before the meter starts.
    let kinds: Vec<String> = tools().into_iter().map(|(k, ..)| k.to_owned()).collect();
    let mut lines = HashMap::new();
    {
        let cache = Cache::load(&engine, HOST).expect("load");
        let args = super::scan_request(&kinds, twin.now - 31 * DAY, json!({}), false, None);
        for (batch, args) in scan_state::requests(
            args,
            &kinds,
            &cache.fingerprints,
            &twin.settled_before,
            twin.now,
        ) {
            let args = args.expect("request fits");
            println!("request {:?}: {:.2} MiB", batch, mib(args.text.get().len()));
            for pass in 1..=3 {
                let line = result_line("twin", &twin.answer(&batch, pass));
                if pass == 1 {
                    println!("answer  {:?}: {:.2} MiB", batch, mib(line.len()));
                }
                lines.insert(format!("{pass}:{}", batch.join(",")), line);
            }
        }
    }
    let idle = CURRENT.load(Ordering::Relaxed);
    println!("idle heap before passes: {:.1} MiB", mib(idle));
    // The first pass migrates the document; the steady passes after it are what live repeats.
    let first = pass(&engine, &twin, &lines);
    twin.pass.set(2);
    let steady = pass(&engine, &twin, &lines);
    twin.pass.set(3);
    let again = pass(&engine, &twin, &lines);
    println!(
        "\nRESULT first {:.1} MiB, steady {:.1} MiB, steady again {:.1} MiB",
        mib(first),
        mib(steady),
        mib(again)
    );
}

/// The soak's computers: (name, size against the busy one), shaped like live's seven
/// checkpoints (31.6, 11.2, 8.0, 6.3, 8.6, 7.2 and 4.3 MiB).
const SOAK_HOSTS: [(&str, f64); 7] = [
    ("ubuntu", 1.0),
    ("nas1", 0.354),
    ("1070ti", 0.254),
    ("odroid", 0.2),
    ("mac", 0.272),
    ("windows", 0.227),
    ("alexis", 0.136),
];
/// Answers prepared per computer: the soak cycles through them.
const SOAK_ANSWERS: u64 = 3;

static IN_CALL: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn env_u64(name: &str, default: u64) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

fn rss_bytes() -> usize {
    std::fs::read_to_string("/proc/self/statm")
        .ok()
        .and_then(|text| text.split_whitespace().nth(1)?.parse::<usize>().ok())
        .map_or(0, |pages| pages * 4096)
}

fn local_computer(dir: &std::path::Path) -> aac_transport::Computer {
    let config = aac_config::ComputerConfig {
        id: aac_core::ComputerId::new("twin").expect("id"),
        label: "Twin".into(),
        os: aac_config::OperatingSystem::Linux,
        transport: aac_config::TransportConfig::Local,
        roles: Vec::new(),
        metadata: Default::default(),
    };
    let policy = aac_transport::SshPolicy::new(dir.join("known_hosts"), dir.join("run"));
    aac_transport::Computer::new(config, policy).expect("local computer")
}

/// One scan call as the dashboard makes it: the frame on a child's stdin through the transport,
/// the child's wait, and the answer's text carved out of its output.
async fn soak_call(
    computer: &aac_transport::Computer,
    args: std::sync::Arc<crate::agent_transport::ArgsText>,
    answer: &std::path::Path,
    wait_ms: u64,
) -> Vec<u8> {
    let argv: Vec<std::ffi::OsString> = vec![
        "/bin/sh".into(),
        "-c".into(),
        format!(
            "cat >/dev/null; sleep {}; cat \"$0\"",
            wait_ms as f64 / 1000.0
        )
        .into(),
        answer.into(),
    ];
    // call_controlled_args: the envelope around the arguments as they are held. The pass keeps
    // its share for the fixed scanner's fallback while the agent's call runs.
    let input = aac_transport::Stdin::default()
        .part(std::sync::Arc::new(
            br#"{"protocol":1,"verb":"scan-usage","id":"twin","deadlineMs":120000,"args":"#
                .to_vec(),
        ))
        .part(args.clone())
        .part(std::sync::Arc::new(b"}\n"));
    let options = aac_transport::CallOptions::new(
        std::time::Duration::from_millis(wait_ms + 60_000),
        64 * 1024 * 1024,
    );
    IN_CALL.store(true, Ordering::Relaxed);
    let output = computer
        .execute_stdin(&argv, input, &options)
        .await
        .expect("twin scan");
    IN_CALL.store(false, Ordering::Relaxed);
    drop(args);
    let mut stdout = output.stdout;
    let (start, end) = {
        let fields: BTreeMap<String, &RawValue> =
            serde_json::from_slice(&stdout[..stdout.len() - 1]).expect("event");
        let value = fields["value"].get();
        let start = value.as_ptr() as usize - stdout.as_ptr() as usize;
        (start, start + value.len())
    };
    stdout.truncate(end);
    stdout.drain(..start);
    stdout
}

/// One pass over every soak computer, as `activity_pass` runs it (the workers and Claude parts
/// are held to the pass's end).
async fn soak_pass(
    engine: &aac_store::engine::Engine,
    computer: &aac_transport::Computer,
    twins: &[(&str, Twin)],
    answers: &std::path::Path,
    round: u64,
    wait_ms: u64,
) -> usize {
    let pricing = super::ResolverPricing::new(std::path::Path::new("/nonexistent-twin"));
    let kinds: Vec<String> = tools().into_iter().map(|(k, ..)| k.to_owned()).collect();
    let mut remote = Vec::new();
    let mut claude = Vec::new();
    let mut calls = 0;
    for (host, twin) in twins {
        let cutoff = twin.now - 31 * DAY;
        let mut cache = Cache::load(engine, host).expect("load");
        let args = super::scan_request(&kinds, cutoff, json!({}), false, None);
        let requests = scan_state::Requests::new(
            args,
            &kinds,
            &cache.fingerprints,
            &twin.settled_before,
            twin.now,
        );
        let mut claude_state = false;
        for batch in requests.batches.clone() {
            let args = requests
                .args(&batch, &cache.fingerprints)
                .expect("request fits");
            let packed: Vec<bool> = batch
                .iter()
                .map(|kind| args.packed.contains(kind))
                .collect();
            let answer = answers.join(format!("{host}-{round}-{}.json", batch.join(",")));
            let parked = cache.park();
            let bytes = soak_call(computer, args.text, &answer, wait_ms).await;
            cache = parked.restore(engine, host).expect("restore");
            calls += 1;
            let mut answer = Answer::parse(&bytes, &mut cache.strings).expect("answer");
            drop(bytes);
            assert!(super::valid_scan_answer(&answer, &batch));
            for (kind, packed) in batch.iter().zip(packed) {
                let source = super::scanned_kind(
                    &mut answer,
                    kind,
                    host,
                    (twin.now, cutoff),
                    (packed, false, false),
                    &mut cache,
                );
                assert_eq!(source["state"], "ok", "{host} {kind}: {source}");
                if kind == "claude" {
                    claude_state = true;
                } else {
                    remote.push(collect::worker_from_rows(
                        &scan_state::of_kind(&cache.rows, kind),
                        &scan_state::of_kind(&cache.srows, kind),
                        host,
                        twin.now,
                        &pricing,
                    ));
                }
            }
        }
        cache.save(engine, host).expect("save");
        if claude_state {
            claude.push(cache.take_kind("claude"));
        }
    }
    drop(claude);
    drop(remote);
    calls
}

fn percentile(sorted: &[usize], share: f64) -> usize {
    sorted
        .get(((sorted.len() as f64 - 1.0) * share).round() as usize)
        .copied()
        .unwrap_or(0)
}

fn summary(label: &str, values: &mut [usize]) -> String {
    values.sort_unstable();
    format!(
        "{label}: min {:.1} / median {:.1} / p90 {:.1} / max {:.1} MiB",
        mib(percentile(values, 0.0)),
        mib(percentile(values, 0.5)),
        mib(percentile(values, 0.9)),
        mib(percentile(values, 1.0)),
    )
}

#[test]
#[ignore = "memory twin soak: run alone with --features memory-twin --ignored --test-threads=1"]
fn memory_twin_soak() {
    crate::allocator::tune_mimalloc();
    let minutes = env_u64("AAC_TWIN_MINUTES", 20);
    let wait_ms = env_u64("AAC_TWIN_WAIT_MS", 3000);
    let period = std::time::Duration::from_secs(env_u64("AAC_TWIN_PERIOD_S", 90));
    let dir = tempfile::tempdir().expect("twin dir");
    let engine = store(dir.path());
    let answers = dir.path().join("answers");
    std::fs::create_dir_all(&answers).expect("answers");
    let kinds: Vec<String> = tools().into_iter().map(|(k, ..)| k.to_owned()).collect();
    let twins: Vec<(&str, Twin)> = SOAK_HOSTS
        .iter()
        .map(|(host, scale)| (*host, Twin::scaled(*scale)))
        .collect();
    for (host, twin) in &twins {
        let checkpoint = serde_json::to_vec(&twin.checkpoint()).expect("checkpoint");
        println!("{host}: checkpoint {:.1} MiB", mib(checkpoint.len()));
        aac_store::analytics_cache::put(&engine, &format!("live/scanner/{host}"), &checkpoint)
            .expect("seed checkpoint");
        drop(checkpoint);
        let cache = Cache::load(&engine, host).expect("load");
        let args = super::scan_request(&kinds, twin.now - 31 * DAY, json!({}), false, None);
        for (batch, args) in scan_state::requests(
            args,
            &kinds,
            &cache.fingerprints,
            &twin.settled_before,
            twin.now,
        ) {
            let args = args.expect("request fits");
            println!(
                "  request {:?}: {:.2} MiB",
                batch,
                mib(args.text.get().len())
            );
            for round in 0..SOAK_ANSWERS {
                let line = result_line("twin", &twin.answer(&batch, round + 1));
                std::fs::write(
                    answers.join(format!("{host}-{round}-{}.json", batch.join(","))),
                    line,
                )
                .expect("answer file");
            }
        }
        drop(cache);
        // The document migrates to parts on the first save, as live's did long ago.
        let mut cache = Cache::load(&engine, host).expect("load");
        cache.save(&engine, host).expect("migrate");
    }
    crate::allocator::reclaim_after_pass();
    let computer = local_computer(dir.path());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let started = std::time::Instant::now();
    let sampler = {
        let stop = std::sync::Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut samples = Vec::new();
            while !stop.load(Ordering::Relaxed) {
                samples.push((
                    started.elapsed().as_secs_f64(),
                    CURRENT.load(Ordering::Relaxed),
                    rss_bytes(),
                    IN_CALL.load(Ordering::Relaxed),
                ));
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            samples
        })
    };
    let until = started + std::time::Duration::from_secs(minutes * 60);
    let mut passes = 0;
    let mut calls = 0;
    let mut durations = Vec::new();
    while std::time::Instant::now() < until {
        let begun = std::time::Instant::now();
        let round = passes % SOAK_ANSWERS;
        calls += runtime.block_on(soak_pass(
            &engine, &computer, &twins, &answers, round, wait_ms,
        ));
        crate::allocator::reclaim_after_pass();
        passes += 1;
        durations.push(begun.elapsed().as_secs_f64());
        if let Some(rest) = period.checked_sub(begun.elapsed()) {
            std::thread::sleep(
                rest.min(until.saturating_duration_since(std::time::Instant::now())),
            );
        }
    }
    stop.store(true, Ordering::Relaxed);
    let samples = sampler.join().expect("sampler");
    if let Ok(path) = std::env::var("AAC_TWIN_OUT") {
        let mut csv = String::from("seconds,heap_bytes,rss_bytes,in_call\n");
        for (at, heap, rss, call) in &samples {
            csv.push_str(&format!("{at:.2},{heap},{rss},{}\n", u8::from(*call)));
        }
        std::fs::write(path, csv).expect("samples");
    }
    let steady: Vec<_> = samples.iter().filter(|(at, ..)| *at >= 20.0).collect();
    let mut heap: Vec<usize> = steady.iter().map(|(_, heap, ..)| *heap).collect();
    let mut rss: Vec<usize> = steady.iter().map(|(_, _, rss, _)| *rss).collect();
    let mut in_call: Vec<usize> = steady
        .iter()
        .filter(|(.., call)| *call)
        .map(|(_, heap, ..)| *heap)
        .collect();
    println!(
        "\nSOAK {minutes} min, {passes} passes, {calls} calls, wait {wait_ms} ms, pass {:.0}-{:.0} s",
        durations.iter().copied().fold(f64::MAX, f64::min),
        durations.iter().copied().fold(0.0, f64::max)
    );
    println!("{}", summary("heap", &mut heap));
    println!("{}", summary("heap during calls", &mut in_call));
    println!("{}", summary("rss", &mut rss));
    // Drift: each five minutes' trough.
    let windows = (minutes * 60).div_ceil(300);
    let troughs: Vec<String> = (0..windows)
        .filter_map(|window| {
            let (from, to) = (window as f64 * 300.0, (window + 1) as f64 * 300.0);
            let part: Vec<_> = samples
                .iter()
                .filter(|(at, ..)| *at >= from.max(20.0) && *at < to)
                .collect();
            let heap = part.iter().map(|(_, heap, ..)| *heap).min()?;
            let rss = part.iter().map(|(_, _, rss, _)| *rss).min()?;
            Some(format!("{:.1}/{:.1}", mib(heap), mib(rss)))
        })
        .collect();
    println!("5-minute troughs heap/rss MiB: {}", troughs.join("  "));
}

/// One whole pass over the seven soak computers without the transport, then the pass's end as
/// `activity_pass` runs it: Claude mirror owners, the Claude workers, the reports, the trim, the
/// typed contract and the ingest. `shaped` builds each computer's reports as soon as it is read
/// (the pass since spike 7); otherwise every worker result is held to the end, as before.
/// Returns the pass peak above its start and a digest of the typed reports it ingested.
fn whole_pass(
    engine: &aac_store::engine::Engine,
    twins: &[(&str, Twin)],
    lines: &HashMap<String, Vec<u8>>,
    ingestor: &mut aac_store::report::Ingestor,
    round: u64,
    shaped: bool,
) -> (usize, Vec<u8>) {
    use std::collections::HashSet;
    let pricing = super::ResolverPricing::new(std::path::Path::new("/nonexistent-twin"));
    let kinds: Vec<String> = tools().into_iter().map(|(k, ..)| k.to_owned()).collect();
    let now = twins[0].1.now;
    let cutoff = now - 31 * DAY;
    let scanner_owned: HashSet<String> = kinds.iter().cloned().collect();
    let local_complete = HashSet::new();
    let frame = super::ReportFrame {
        window_from_ms: cutoff,
        now,
        local_complete: &local_complete,
        scanner_owned: &scanner_owned,
    };
    let mut held = super::Shaped::default();
    let mut meter = Meter::new();
    let mut remote: Vec<Value> = Vec::new();
    let mut claude = Vec::new();
    let mut parked = Vec::new();
    let mut states: Vec<Value> = Vec::new();
    for (host, twin) in twins {
        meter.step("one computer (load, apply, save, shape)", || {
            let start_state = states.len();
            let mut cache = Cache::load(engine, host).expect("load");
            let args = super::scan_request(&kinds, cutoff, json!({}), false, None);
            let requests = scan_state::Requests::new(
                args,
                &kinds,
                &cache.fingerprints,
                &twin.settled_before,
                twin.now,
            );
            let mut claude_state = None;
            for batch in requests.batches.clone() {
                let args = requests
                    .args(&batch, &cache.fingerprints)
                    .expect("request fits");
                let packed: Vec<bool> = batch
                    .iter()
                    .map(|kind| args.packed.contains(kind))
                    .collect();
                drop(args);
                let line = &lines[&format!("{host}:{round}:{}", batch.join(","))];
                let mut answer = Answer::parse(line, &mut cache.strings).expect("answer");
                assert!(super::valid_scan_answer(&answer, &batch));
                for (kind, packed) in batch.iter().zip(packed) {
                    let source = super::scanned_kind(
                        &mut answer,
                        kind,
                        host,
                        (twin.now, cutoff),
                        (packed, false, false),
                        &mut cache,
                    );
                    assert_eq!(source["state"], "ok", "{host} {kind}: {source}");
                    if kind == "claude" {
                        claude_state = Some(states.len());
                    } else {
                        let worker = collect::worker_from_rows(
                            &scan_state::of_kind(&cache.rows, kind),
                            &scan_state::of_kind(&cache.srows, kind),
                            host,
                            twin.now,
                            &pricing,
                        );
                        remote.push(json!({ "tool": kind, "host": host, "data": worker }));
                    }
                    states.push(source);
                }
            }
            cache.save(engine, host).expect("save");
            if let Some(state_index) = claude_state {
                let (rows, sessions, identities) = cache.take_kind("claude");
                let part = super::claude_mirrors::Part {
                    host,
                    state_index,
                    rows,
                    sessions,
                    identities,
                };
                if shaped {
                    parked.push(part.park());
                } else {
                    claude.push(part);
                }
            }
            drop(cache);
            if shaped {
                let early = super::Shaped::computer_sources(&states, start_state, &scanner_owned);
                let built = super::Shaped::build(
                    &frame,
                    &states,
                    early,
                    HashMap::new(),
                    std::mem::take(&mut remote),
                );
                held.keep(ingestor, built);
            }
        });
    }
    let (typed, remember) = if shaped {
        meter.step("Claude: parts back, owners, reports", || {
            super::shape_claude(
                engine,
                std::mem::take(&mut parked),
                &mut states,
                &pricing,
                &frame,
                &mut held,
                ingestor,
            );
            for source in &states {
                assert_ne!(source["cause"], "store_unavailable");
            }
            let rest = held.rest_sources(&states);
            let built = super::Shaped::build(&frame, &states, rest, HashMap::new(), Vec::new());
            held.keep(ingestor, built);
            std::mem::take(&mut held).finish().expect("contract")
        })
    } else {
        meter.step("Claude mirror owners (prepare_live)", || {
            super::claude_contributions::prepare_live(
                engine,
                &mut claude,
                &mut states,
                now,
                &pricing,
            )
            .expect("prepare live");
        });
        meter.step("Claude workers", || {
            for part in claude.drain(..) {
                let worker = part.worker(now, &pricing);
                remote.push(json!({ "tool": "claude", "host": part.host, "data": worker }));
            }
        });
        let reports = meter.step("build the reports", || {
            aac_store::report::build_activity_reports_owned(aac_store::report::OwnedReportArgs {
                local: HashMap::new(),
                remote: std::mem::take(&mut remote),
                states: &states,
                window_from_ms: cutoff,
                now,
                local_complete: &local_complete,
                scanner_owned: &scanner_owned,
            })
        });
        let (trimmed, remember) = meter.step("trim against the last pass", || {
            let out = ingestor.trim(&reports);
            drop(reports);
            out
        });
        let typed = meter.step("typed contract reports", || {
            let out = aac_store::report::contract_reports(&trimmed).expect("contract");
            drop(trimmed);
            out
        });
        (typed, remember)
    };
    let digest = serde_json::to_vec(&typed).expect("typed reports");
    meter.step("ingest", || {
        let result = aac_store::activity::ingest(engine, &typed).expect("ingest");
        let refused: HashSet<usize> = result
            .refused
            .iter()
            .filter_map(|r| r["index"].as_u64().map(|i| i as usize))
            .collect();
        ingestor.accept(remember, &refused);
        drop(typed);
    });
    drop(states);
    let title = if shaped {
        "whole pass, each computer's reports shaped as it is read"
    } else {
        "whole pass, every worker result held to the end (before)"
    };
    (meter.report(title), digest)
}

/// A seeded store, its computers, and every answer of every round by `host:round:batch`.
type WholePassSetup = (
    aac_store::engine::Engine,
    Vec<(&'static str, Twin)>,
    HashMap<String, Vec<u8>>,
);

/// The seven live-shaped computers seeded into a fresh store, and every answer of four rounds.
fn whole_pass_setup(dir: &std::path::Path) -> WholePassSetup {
    let engine = store(dir);
    let kinds: Vec<String> = tools().into_iter().map(|(k, ..)| k.to_owned()).collect();
    let twins: Vec<(&str, Twin)> = SOAK_HOSTS
        .iter()
        .map(|(host, scale)| (*host, Twin::scaled(*scale)))
        .collect();
    let mut lines = HashMap::new();
    for (host, twin) in &twins {
        let checkpoint = serde_json::to_vec(&twin.checkpoint()).expect("checkpoint");
        aac_store::analytics_cache::put(&engine, &format!("live/scanner/{host}"), &checkpoint)
            .expect("seed checkpoint");
        drop(checkpoint);
        let cache = Cache::load(&engine, host).expect("load");
        let args = super::scan_request(&kinds, twin.now - 31 * DAY, json!({}), false, None);
        for (batch, args) in scan_state::requests(
            args,
            &kinds,
            &cache.fingerprints,
            &twin.settled_before,
            twin.now,
        ) {
            args.expect("request fits");
            for round in 1..=WHOLE_PASS_ROUNDS {
                let line = result_line("twin", &twin.answer(&batch, round));
                let fields: BTreeMap<String, &RawValue> =
                    serde_json::from_slice(&line[..line.len() - 1]).expect("event");
                let value = fields["value"].get().as_bytes().to_vec();
                lines.insert(format!("{host}:{round}:{}", batch.join(",")), value);
            }
        }
        drop(cache);
        let mut cache = Cache::load(&engine, host).expect("load");
        cache.save(&engine, host).expect("migrate");
    }
    (engine, twins, lines)
}

const WHOLE_PASS_ROUNDS: u64 = 4;
/// The steady pass's ceiling above its start (live-shaped seven computers). Holding every
/// worker result to the pass's end peaked at about 94 MiB; the busy computer's own checkpoint
/// and apply (about 62 MiB) now set the peak.
const WHOLE_PASS_CEILING_MIB: f64 = 70.0;

/// The whole pass both ways, over two stores seeded alike: the same typed reports are ingested
/// each round, and the per-computer shaping's peak stays low. The first round sends every cell
/// (a fresh ingestor, as after a restart); the later rounds are the steady passes live repeats.
#[test]
#[ignore = "memory twin: run alone with --features memory-twin --ignored --test-threads=1"]
fn memory_twin_whole_pass() {
    let mut peaks = Vec::new();
    let mut digests = Vec::new();
    for shaped in [false, true] {
        let dir = tempfile::tempdir().expect("twin dir");
        let (engine, twins, lines) = whole_pass_setup(dir.path());
        let mut ingestor = aac_store::report::Ingestor::new();
        let mut run = Vec::new();
        let mut sent = Vec::new();
        for round in 1..=WHOLE_PASS_ROUNDS {
            let (peak, digest) = whole_pass(&engine, &twins, &lines, &mut ingestor, round, shaped);
            run.push(peak);
            sent.push(digest);
        }
        peaks.push(run);
        digests.push(sent);
    }
    let line = |run: &[usize]| {
        run.iter()
            .map(|p| format!("{:.1}", mib(*p)))
            .collect::<Vec<_>>()
            .join(" / ")
    };
    println!("\nRESULT whole pass peaks MiB, before: {}", line(&peaks[0]));
    println!("RESULT whole pass peaks MiB, after:  {}", line(&peaks[1]));
    for (round, (before, after)) in digests[0].iter().zip(&digests[1]).enumerate() {
        println!(
            "round {}: typed reports {:.2} MiB, identical: {}",
            round + 1,
            mib(before.len()),
            before == after
        );
        assert!(
            before == after,
            "round {}: the typed reports differ",
            round + 1
        );
    }
    for (round, peak) in peaks[1].iter().enumerate().skip(1) {
        assert!(
            mib(*peak) < WHOLE_PASS_CEILING_MIB,
            "steady round {}: pass peak {:.1} MiB, ceiling {WHOLE_PASS_CEILING_MIB} MiB",
            round + 1,
            mib(*peak)
        );
    }
}
