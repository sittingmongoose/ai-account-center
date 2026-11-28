//! Heap the Analytics activity pass's models.dev refresh step takes on a live-sized cache.
//!
//! The pass runs the refresh about every two minutes only to keep the cache file fresh; it
//! drops the answer. A fresh cache used to be re-read and re-parsed into a full JSON tree (and
//! normalized into a second copy) on every pass: about 150 MiB for a 6.8 MB cache, held for a
//! fraction of a second, which was the dashboard's periodic memory spike. A per-thread counting
//! allocator measures the step's peak so parallel tests cannot disturb the numbers.
use aac_views::pricing::{Fetched, RegistryCache, RegistryRefresh, cache_path};
use serde_json::{Map, Value, json};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::path::Path;

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

/// The peak bytes above the start while `run` runs on this thread.
fn peak<T>(run: impl FnOnce() -> T) -> (T, isize) {
    let start = LIVE.with(Cell::get);
    PEAK.with(|peak| peak.set(start));
    let value = run();
    (value, PEAK.with(Cell::get) - start)
}

const MIB: isize = 1 << 20;
const NOW: f64 = 1_800_000_000_000.0;

/// A live-shaped models.dev cache (the real one is about 6.8 MB): 120 providers of 120 models,
/// each with the catalog fields the live registry carries.
fn write_cache(dir: &Path, fetched_at: f64) -> usize {
    let mut providers = Map::new();
    for p in 0..120 {
        let mut models = Map::new();
        for m in 0..120 {
            let id = format!("model-{p}-{m}");
            models.insert(
                id.clone(),
                json!({
                    "id": id, "name": format!("Model {p} {m}"),
                    "description": "Popular open workhorse for chat, coding and self-hosting",
                    "family": "family", "attachment": false, "reasoning": true,
                    "reasoning_options": [{"type": "effort", "values": ["low", "medium", "high"]}],
                    "tool_call": true, "temperature": true, "knowledge": "2024-06",
                    "release_date": "2025-10-29", "last_updated": "2025-10-29",
                    "modalities": {"input": ["text"], "output": ["text"]},
                    "open_weights": true, "limit": {"context": 131072, "output": 4096},
                    "cost": {"input": 0.15, "output": 0.6},
                }),
            );
        }
        let key = format!("provider-{p}");
        providers.insert(
            key.clone(),
            json!({"id": key, "env": ["KEY"], "name": key, "models": models}),
        );
    }
    let bytes = serde_json::to_vec(&json!({
        "version": 1, "fetchedAt": fetched_at, "providers": Value::Object(providers),
    }))
    .expect("fixture serializes");
    std::fs::write(cache_path(dir), &bytes).expect("fixture written");
    bytes.len()
}

fn no_fetch() -> Result<Fetched, ()> {
    panic!("a fresh cache never fetches")
}

#[test]
fn the_pass_refresh_of_a_fresh_cache_does_not_reparse_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    let size = write_cache(dir.path(), NOW - 60_000.0);
    assert!(
        size > 6 * 1024 * 1024,
        "the fixture is live-sized: {size} bytes"
    );
    let refresh = RegistryRefresh::new(dir.path());
    // The resident memo the resolver and route already hold.
    assert!(
        RegistryCache::new(cache_path(dir.path()))
            .read(false, NOW)
            .is_some()
    );
    assert!(refresh.ensure(false, NOW, no_fetch));
    let (usable, bytes) = peak(|| refresh.ensure(false, NOW, no_fetch));
    assert!(usable);
    assert!(
        bytes < MIB,
        "a fresh-cache pass refresh peaked at {} MiB",
        bytes / MIB
    );
}

#[test]
fn the_pass_refresh_of_a_stale_cache_with_a_failed_fetch_does_not_reparse_it() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_cache(dir.path(), NOW - 2.0 * aac_views::pricing::CACHE_TTL_MS);
    let refresh = RegistryRefresh::new(dir.path());
    assert!(refresh.ensure(false, NOW, || Err(())));
    let before = std::fs::read(cache_path(dir.path())).expect("cache file");
    let (usable, bytes) = peak(|| refresh.ensure(false, NOW, || Err(())));
    assert!(usable, "the stale cache still answers");
    assert!(
        bytes < MIB,
        "a stale-cache pass refresh peaked at {} MiB",
        bytes / MIB
    );
    assert_eq!(
        std::fs::read(cache_path(dir.path())).expect("cache file"),
        before
    );
}

#[test]
fn the_pass_refresh_keeps_the_refresh_outcomes() {
    let dir = tempfile::tempdir().expect("temp dir");
    let refresh = RegistryRefresh::new(dir.path());
    // No cache and a failed fetch: nothing usable, nothing written.
    assert!(!refresh.ensure(false, NOW, || Err(())));
    assert!(!cache_path(dir.path()).exists());
    // A fetched registry is written exactly as the payload-returning refresh writes it.
    let body = br#"{"p":{"id":" P ","models":{"m":{"cost":{"input":1,"output":2}}}}}"#.to_vec();
    let fetched = |body: &Vec<u8>| {
        let body = body.clone();
        move || Ok(Fetched { ok: true, body })
    };
    assert!(refresh.ensure(true, NOW, fetched(&body)));
    let ensured = std::fs::read(cache_path(dir.path())).expect("cache file");
    let other = tempfile::tempdir().expect("temp dir");
    let returned = RegistryRefresh::new(other.path()).start(true, NOW, fetched(&body));
    assert!(returned.is_some());
    assert_eq!(
        ensured,
        std::fs::read(cache_path(other.path())).expect("cache file")
    );
    // A non-ok answer keeps the fresh cache it already has.
    assert!(refresh.ensure(true, NOW, || Ok(Fetched {
        ok: false,
        body: Vec::new()
    })));
    assert_eq!(
        ensured,
        std::fs::read(cache_path(dir.path())).expect("cache file")
    );
}
