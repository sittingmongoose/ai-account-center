//! Heap held by a live-shaped scanner checkpoint: typed rows against JSON-object rows.
//!
//! A busy computer's checkpoint (`live/scanner/ubuntu`, about 38 MB) carries about 100k hour and
//! session rows. The Analytics writer held them as `serde_json::Value` objects for the whole
//! pass. This binary counts the bytes each form keeps resident and the peak while it is read,
//! with a per-thread counting allocator so parallel tests cannot disturb the numbers.
use aac_store::collect::{self, Pricing, PricingResolution, Rates};
use aac_store::scan_row::{Interner, ScanRow, with_interner};
use serde_json::{Value, json};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

struct Counting;

thread_local! {
    static LIVE: Cell<isize> = const { Cell::new(0) };
    static PEAK: Cell<isize> = const { Cell::new(0) };
}

fn add(bytes: isize) {
    let _ = LIVE.try_with(|live| {
        let now = live.get() + bytes;
        live.set(now);
        let _ = PEAK.try_with(|peak| peak.set(peak.get().max(now)));
    });
}

#[allow(unsafe_code)]
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        add(layout.size() as isize);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        add(-(layout.size() as isize));
        // SAFETY: the pointer came from `alloc` with this layout.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        add(new_size as isize - layout.size() as isize);
        // SAFETY: forwarded unchanged to the system allocator.
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static COUNTING: Counting = Counting;

/// (retained bytes, peak bytes above the start) of building `make`'s value on this thread.
fn measure<T>(make: impl FnOnce() -> T) -> (T, isize, isize) {
    let start = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(start));
    let value = make();
    let retained = LIVE.with(Cell::get) - start;
    let peak = PEAK.with(Cell::get) - start;
    (value, retained, peak)
}

/// A live-shaped checkpoint's rows: 2,000 files over 31 days of hours, 12 models, 4 tools.
fn checkpoint() -> String {
    let kinds = ["claude", "codex", "omp", "muse"];
    let models = [
        "claude-opus-4-5",
        "claude-sonnet-4-5",
        "gpt-5.5",
        "gpt-6.1-sol",
        "grok-4.7",
        "kimi-k3",
        "glm-5.4",
        "qwen3-coder",
        "gemini-3-pro",
        "devin-swe-2.1",
        "muse-code",
        "deepseek-v4",
    ];
    let mut rows = Vec::new();
    let mut srows = Vec::new();
    for index in 0..60_000usize {
        let file = format!(
            "-home-user-projects-repo-{:04}/session-{:08x}.jsonl",
            index % 2000,
            index % 2000
        );
        let day = 1 + (index / 24) % 28;
        let hour = format!("2026-11-{day:02} {:02}", index % 24);
        rows.push(json!({
            "k": kinds[index % 4], "f": file, "m": models[index % 12], "p": "anthropic",
            "h": hour, "i": index * 7, "o": index, "cr": index * 30, "cw": 12, "c": 0.0, "n": 3,
        }));
    }
    for index in 0..40_000usize {
        let file = format!(
            "-home-user-projects-repo-{:04}/session-{:08x}.jsonl",
            index % 2000,
            index % 2000
        );
        srows.push(json!({
            "k": kinds[index % 4], "f": file, "s": format!("{:016x}", index % 9000),
            "m": models[index % 12], "p": "anthropic", "a": 1_795_000_000_000u64 + index as u64,
            "z": 1_795_000_900_000u64 + index as u64, "i": index * 7, "o": index,
            "cr": index * 30, "cw": 12, "c": 0.0, "n": 3,
        }));
    }
    json!({"rows": rows, "srows": srows}).to_string()
}

#[derive(serde::Deserialize)]
struct AsValues {
    rows: Vec<Value>,
    srows: Vec<Value>,
}

#[derive(serde::Deserialize)]
struct AsTyped {
    rows: Vec<ScanRow>,
    srows: Vec<ScanRow>,
}

struct Flat;
impl Pricing for Flat {
    fn resolve(&self, _model: &str, _provider: Option<&str>) -> PricingResolution {
        PricingResolution {
            rates: Rates {
                input: 3.0,
                output: 15.0,
                cache_creation: 3.75,
                cache_read: 0.3,
            },
            source: "models.dev".into(),
        }
    }
}

#[test]
fn typed_rows_hold_a_fraction_of_the_json_object_rows() {
    let text = checkpoint();
    let (values, value_bytes, value_peak) = measure(|| serde_json::from_str::<AsValues>(&text).expect("value rows"));
    let ((typed, _), typed_bytes, typed_peak) = measure(|| {
        with_interner(Interner::default(), || {
            serde_json::from_str::<AsTyped>(&text).expect("typed rows")
        })
    });
    assert_eq!(
        values.rows.len() + values.srows.len(),
        typed.rows.len() + typed.srows.len()
    );
    println!(
        "checkpoint {:.1} MiB text, {} rows: Value rows hold {:.1} MiB (peak {:.1}); typed rows \
         hold {:.1} MiB (peak {:.1})",
        text.len() as f64 / 1048576.0,
        typed.rows.len() + typed.srows.len(),
        value_bytes as f64 / 1048576.0,
        value_peak as f64 / 1048576.0,
        typed_bytes as f64 / 1048576.0,
        typed_peak as f64 / 1048576.0,
    );
    assert!(
        typed_bytes * 4 < value_bytes,
        "typed {typed_bytes} B should be under a quarter of {value_bytes} B"
    );

    // One tool's aggregation over borrowed rows: no copy of the cache is made.
    let (worker, _, worker_peak) = measure(|| {
        let rows: Vec<&ScanRow> = typed.rows.iter().filter(|r| &*r.kind == "codex").collect();
        let srows: Vec<&ScanRow> = typed.srows.iter().filter(|r| &*r.kind == "codex").collect();
        collect::worker_from_rows(&rows, &srows, "ubuntu", 1_796_000_000_000, &Flat)
    });
    let (_, _, value_worker_peak) = measure(|| {
        let kind = |rows: &[Value]| -> Vec<Value> { rows.iter().filter(|r| r["k"] == "codex").cloned().collect() };
        let (rows, srows) = (kind(&values.rows), kind(&values.srows));
        collect::worker_from_scan(&rows, &srows, "codex", "ubuntu", 1_796_000_000_000, &Flat)
    });
    println!(
        "codex worker: peak {:.1} MiB over borrowed typed rows, {:.1} MiB over cloned Value rows",
        worker_peak as f64 / 1048576.0,
        value_worker_peak as f64 / 1048576.0,
    );
    assert!(worker["cells"].as_array().is_some_and(|cells| !cells.is_empty()));
    assert!(worker_peak < value_worker_peak);
}
