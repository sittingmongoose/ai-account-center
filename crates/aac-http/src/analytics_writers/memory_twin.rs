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
use super::scan_answer::Answer;
use super::scan_state::{self, Cache};
use aac_store::collect;
use serde_json::value::RawValue;
use serde_json::{Map, Value, json};
use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};

struct Counting;

static CURRENT: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let ptr = unsafe { System.alloc(layout) };
        if !ptr.is_null() {
            let now = CURRENT.fetch_add(layout.size(), Ordering::Relaxed) + layout.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        ptr
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the pointer came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) };
        CURRENT.fetch_sub(layout.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let out = unsafe { System.realloc(ptr, layout, size) };
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

/// The twin's tools: (kind, prints, settled share in percent, hour rows per used file).
const TOOLS: [(&str, usize, usize, usize); 4] = [
    ("claude", 16_000, 92, 4),
    ("codex", 6_000, 90, 6),
    ("omp", 1_200, 85, 3),
    ("muse", 800, 85, 3),
];
/// Files changed since the last pass (re-read whole), per tool.
const CHANGED: usize = 40;

struct Twin {
    /// The pass being run (its changed files differ from the last pass's).
    pass: std::cell::Cell<u64>,
    now: i64,
    slot: u64,
    settled_before: HashMap<String, u64>,
}

impl Twin {
    fn new() -> Self {
        let now = 1_795_212_000_000;
        Twin {
            pass: std::cell::Cell::new(1),
            now,
            slot: now as u64 / aac_usage::collector::SETTLED_CHECK_MS,
            settled_before: TOOLS
                .iter()
                .map(|(kind, ..)| ((*kind).to_owned(), (now - 3 * DAY) as u64))
                .collect(),
        }
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
        for (kind, count, share, per_file) in TOOLS {
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
        for (kind, count, share, per_file) in TOOLS {
            if !kinds.iter().any(|k| k == kind) {
                continue;
            }
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
    let kinds: Vec<String> = TOOLS.iter().map(|(k, ..)| (*k).to_owned()).collect();
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
        let bytes = meter.step("agent call: frame, envelope, answer bytes", || {
            // scan_usage: the frame, then the transport's checks and the agent's output.
            let mut wire =
                br#"{"protocol":1,"verb":"scan-usage","id":"twin","deadlineMs":120000,"args":"#
                    .to_vec();
            wire.extend_from_slice(args.text.get().as_bytes());
            wire.push(b'}');
            let envelope: BTreeMap<String, &RawValue> =
                serde_json::from_slice(&wire).expect("envelope");
            let inner: BTreeMap<String, &RawValue> =
                serde_json::from_str(envelope["args"].get()).expect("arguments");
            drop(inner);
            let mut input = wire.clone();
            input.push(b'\n');
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
            drop(wire);
            stdout
        });
        drop(args);
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
    let kinds: Vec<String> = TOOLS.iter().map(|(k, ..)| (*k).to_owned()).collect();
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
