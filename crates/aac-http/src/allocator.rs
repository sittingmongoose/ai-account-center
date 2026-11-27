//! The service binaries' global allocator, its startup tuning and post-pass reclamation.
//!
//! All three shipped service binaries (`ai-account-center`, `ccs`, `aac-http`) install
//! [`SELECTED`] as their `#[global_allocator]` and call [`tune`] first in `main()`.
//!
//! **Linux (MEM7 lane 1):** tuned jemalloc (`tikv-jemallocator`, prefixed symbols) with
//! [`JEMALLOC_CONF`]: two arenas, dirty pages purged after 1 s, no muzzy stage, and a
//! background purging thread. Release 38 on tuned mimalloc v3 held `~220-245 MiB` of freed
//! pages (`[anon:mimalloc]` RSS + swap) above a `~85-110 MiB` live heap: `mi_collect` reaches
//! only the calling thread, full pages are skipped, and hundreds of short-lived blocking
//! threads abandon pages pinned by long-lived small objects. The MEM6 twin on jemalloc held a
//! flat trough. C libraries (`zstd-sys`, SQLite) stay on glibc malloc, tuned by `mallopt`.
//!
//! **One switch back:** drop `"linux-jemalloc"` from `default` in `crates/aac-http/Cargo.toml`;
//! Linux then runs the tuned mimalloc below, exactly as release 38 did.
//!
//! **macOS and Windows:** mimalloc, tuned in code. Without tuning, mimalloc v3 eagerly commits
//! arenas (`arena_eager_commit = 2` on overcommit OSes), delays purging freed pages
//! (`purge_delay = 1000`), and retains abandoned/full small pages across short-lived blocking
//! threads (`page_reclaim_on_free = 0`, `page_full_retain = 2`, `generic_collect = 10000`).
//! [`tune_mimalloc`] sets them through `libmimalloc-sys`'s exported `mi_option_set`.
//!
//! On Linux glibc, C libraries such as `zstd-sys` bypass Rust's global allocator and leave
//! multi-MiB `ZSTD_CCtx` buffers trapped in glibc arenas unless `M_MMAP_THRESHOLD` and
//! `M_ARENA_MAX` are tuned; [`tune`] does that under either allocator. [`reclaim_on_drop`] and
//! [`reclaim_after_pass`] run after large transient passes: `mi_collect(true)` under mimalloc,
//! a thread-cache flush under jemalloc, and `malloc_trim(0)` on Linux GNU.

/// Whether this build's service binaries run on jemalloc (Linux with `linux-jemalloc`).
pub const JEMALLOC: bool = cfg!(all(target_os = "linux", feature = "linux-jemalloc"));

/// The service binaries' global allocator type.
#[cfg(all(target_os = "linux", feature = "linux-jemalloc"))]
pub type Selected = tikv_jemallocator::Jemalloc;
/// The service binaries' global allocator type.
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
pub type Selected = mimalloc::MiMalloc;

/// The service binaries' global allocator: `#[global_allocator] static GLOBAL: Selected = SELECTED;`
#[cfg(all(target_os = "linux", feature = "linux-jemalloc"))]
pub const SELECTED: Selected = tikv_jemallocator::Jemalloc;
/// The service binaries' global allocator: `#[global_allocator] static GLOBAL: Selected = SELECTED;`
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
pub const SELECTED: Selected = mimalloc::MiMalloc;

/// The tuned jemalloc options (MEM6 twin `W2_J2` / `W3_J1`), compiled in as `malloc_conf`.
/// `_RJEM_MALLOC_CONF` in the environment still overrides them for diagnosis.
pub const JEMALLOC_CONF: &core::ffi::CStr =
    c"narenas:2,dirty_decay_ms:1000,muzzy_decay_ms:0,background_thread:true";
/// Tuned `opt.narenas`.
pub const TUNED_JEMALLOC_NARENAS: u32 = 2;
/// Tuned `opt.dirty_decay_ms`.
pub const TUNED_JEMALLOC_DIRTY_DECAY_MS: isize = 1000;
/// Tuned `opt.muzzy_decay_ms`.
pub const TUNED_JEMALLOC_MUZZY_DECAY_MS: isize = 0;

/// Environment variable that makes [`tune`] print [`report`] on stderr (diagnosis and the
/// allocator integration test).
pub const REPORT_ENV: &str = "AAC_ALLOCATOR_REPORT";

#[cfg(all(target_os = "linux", feature = "linux-jemalloc"))]
mod jemalloc {
    use core::ffi::{CStr, c_char, c_void};

    /// A C string pointer jemalloc reads as its `malloc_conf` global.
    #[repr(transparent)]
    pub struct Conf(*const c_char);

    // SAFETY: the pointer refers to an immutable `'static` C string literal.
    #[allow(unsafe_code)]
    unsafe impl Sync for Conf {}

    /// jemalloc's weak `je_malloc_conf` (prefixed `_rjem_`), overridden with the tuned options.
    /// jemalloc reads it at its first allocation, before `main`.
    #[allow(unsafe_code)]
    #[unsafe(export_name = "_rjem_malloc_conf")]
    pub static MALLOC_CONF: Conf = Conf(super::JEMALLOC_CONF.as_ptr());

    /// Read one fixed-size `mallctl` value.
    #[allow(unsafe_code)]
    pub fn read<T: Copy>(name: &CStr, mut value: T) -> Option<T> {
        let mut len = core::mem::size_of::<T>();
        // SAFETY: `name` is NUL-terminated; `value`/`len` describe a writable `T` of the type
        // the named control returns (checked by the returned length).
        let rc = unsafe {
            tikv_jemalloc_sys::mallctl(
                name.as_ptr(),
                (&raw mut value).cast::<c_void>(),
                &raw mut len,
                core::ptr::null_mut(),
                0,
            )
        };
        (rc == 0 && len == core::mem::size_of::<T>()).then_some(value)
    }

    /// Run one `mallctl` command that takes and returns nothing.
    #[allow(unsafe_code)]
    pub fn run(name: &CStr) -> bool {
        // SAFETY: `name` is NUL-terminated; the control reads and writes no value.
        unsafe {
            tikv_jemalloc_sys::mallctl(
                name.as_ptr(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                core::ptr::null_mut(),
                0,
            ) == 0
        }
    }

    /// jemalloc's version string.
    #[allow(unsafe_code)]
    pub fn version() -> Option<String> {
        let ptr = read::<*const c_char>(c"version", core::ptr::null())?;
        if ptr.is_null() {
            return None;
        }
        // SAFETY: `version` returns a pointer to jemalloc's static NUL-terminated string.
        Some(
            unsafe { CStr::from_ptr(ptr) }
                .to_string_lossy()
                .into_owned(),
        )
    }
}

/// The running allocator's own answer to "which allocator, with which options", as one
/// ASCII line. Under jemalloc it reads `mallctl` (`version`, `opt.narenas`,
/// `opt.dirty_decay_ms`, `opt.muzzy_decay_ms`, `background_thread`) and checks that a 4 MiB
/// Rust allocation moves this thread's `thread.allocated` counter (`rust_heap=yes`).
#[cfg(all(target_os = "linux", feature = "linux-jemalloc"))]
#[must_use]
pub fn report() -> String {
    let before = jemalloc::read::<u64>(c"thread.allocated", 0);
    let probe: Vec<u8> = Vec::with_capacity(4 << 20);
    let probe = core::hint::black_box(probe);
    let after = jemalloc::read::<u64>(c"thread.allocated", 0);
    drop(probe);
    let rust_heap = matches!((before, after), (Some(b), Some(a)) if a.saturating_sub(b) >= 4 << 20);
    let show = |value: Option<String>| value.unwrap_or_else(|| "unreadable".to_owned());
    format!(
        "aac-allocator: jemalloc version={} narenas={} dirty_decay_ms={} muzzy_decay_ms={} background_thread={} rust_heap={}",
        show(jemalloc::version()),
        show(jemalloc::read::<u32>(c"opt.narenas", 0).map(|v| v.to_string())),
        show(jemalloc::read::<isize>(c"opt.dirty_decay_ms", 0).map(|v| v.to_string())),
        show(jemalloc::read::<isize>(c"opt.muzzy_decay_ms", 0).map(|v| v.to_string())),
        show(jemalloc::read::<bool>(c"background_thread", false).map(|v| v.to_string())),
        if rust_heap { "yes" } else { "no" },
    )
}

/// The running allocator's own answer to "which allocator, with which options", as one
/// ASCII line (mimalloc: the tuned option values `mi_option_get` reports).
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
#[must_use]
pub fn report() -> String {
    let o = current_options();
    format!(
        "aac-allocator: mimalloc purge_delay={} arena_eager_commit={} page_reclaim_on_free={} generic_collect={}",
        o.purge_delay, o.arena_eager_commit, o.page_reclaim_on_free, o.generic_collect
    )
}

/// Apply the selected allocator's startup tuning; the first call in each binary's `main()`,
/// before any worker thread starts. With `AAC_ALLOCATOR_REPORT=1`, prints [`report`] on stderr.
#[allow(unsafe_code)]
pub fn tune() {
    #[cfg(all(target_os = "linux", feature = "linux-jemalloc"))]
    {
        // Keep the `malloc_conf` definition linked into every binary that calls this.
        core::hint::black_box(&jemalloc::MALLOC_CONF);
        // SAFETY: PR_SET_THP_DISABLE takes no pointers; `mallopt` configures glibc's C
        // allocator before worker threads create additional arenas.
        unsafe {
            let _ = libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0);
            #[cfg(target_env = "gnu")]
            {
                let _ = libc::mallopt(libc::M_MMAP_THRESHOLD, TUNED_GLIBC_MMAP_THRESHOLD_BYTES);
                let _ = libc::mallopt(libc::M_ARENA_MAX, TUNED_GLIBC_ARENA_MAX);
            }
        }
    }
    #[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
    tune_mimalloc();
    if std::env::var_os(REPORT_ENV).is_some_and(|value| value == "1") {
        eprintln!("{}", report());
    }
}

#[cfg(test)]
use core::cell::Cell;
use core::ffi::{c_int, c_long};

// Keep an explicit reference to `mimalloc` so library and test targets always link the
// static `libmimalloc` archive that provides `mi_option_*` and `mi_collect`
// (via `libmimalloc-sys`).
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const _: () = {
    let _ = core::mem::size_of::<mimalloc::MiMalloc>();
};

// Exact `mi_option_t` discriminants from `libmimalloc-sys 0.1.49`
// (`c_src/mimalloc/v3/include/mimalloc.h`, the default v3 build of `mimalloc = "0.1"`).
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_VERBOSE: c_int = 2;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_EAGER_COMMIT: c_int = 3;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_ARENA_EAGER_COMMIT: c_int = 4;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_PURGE_DECOMMITS: c_int = 5;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_PURGE_DELAY: c_int = 15;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_GENERIC_COLLECT: c_int = 34;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_PAGE_RECLAIM_ON_FREE: c_int = 35;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_PAGE_FULL_RETAIN: c_int = 36;
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
const MI_OPTION_ALLOW_THP: c_int = 43;

/// Tuned `purge_delay` (ms): purge freed pages back to the OS immediately.
pub const TUNED_PURGE_DELAY_MS: c_long = 0;
/// Tuned `eager_commit` / `arena_eager_commit`: disable eager commit (`0`).
pub const TUNED_EAGER_COMMIT: c_long = 0;
/// Tuned `purge_decommits`: decommit purged pages (`MADV_DONTNEED` / `MEM_DECOMMIT`, `1`).
pub const TUNED_PURGE_DECOMMITS: c_long = 1;
/// Tuned `generic_collect`: collect thread heaps every `500` generic allocation calls
/// (down from `10000`) so transient pass allocations are retired promptly.
pub const TUNED_GENERIC_COLLECT: c_long = 500;
/// Tuned `page_reclaim_on_free`: reclaim abandoned small/medium pages across thread heaps
/// on free (`1`, up from `0`) so exiting blocking threads do not strand pages.
pub const TUNED_PAGE_RECLAIM_ON_FREE: c_long = 1;
/// Tuned `page_full_retain`: retain `0` full small pages per size class (down from `2`).
pub const TUNED_PAGE_FULL_RETAIN: c_long = 0;
/// Tuned `allow_thp`: no transparent huge pages (`0`). mimalloc marks its 1 GiB arena
/// `MADV_HUGEPAGE`, so each touched 2 MiB stretch becomes one resident huge page, and a 4 KiB
/// purge inside it cannot hand memory back (live r32: 76-108 MiB `AnonHugePages`).
pub const TUNED_ALLOW_THP: c_long = 0;
/// Tuned Linux glibc `M_MMAP_THRESHOLD` (`128 KiB`): pins the threshold so C buffers such as
/// `zstd-sys` `ZSTD_CCtx` (1.3-2.5 MiB) stay on `mmap`/`munmap` instead of heap arenas.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub const TUNED_GLIBC_MMAP_THRESHOLD_BYTES: c_int = 128 * 1024;
/// Tuned Linux glibc `M_ARENA_MAX` (`2`): bounds per-thread C allocator arenas.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
pub const TUNED_GLIBC_ARENA_MAX: c_int = 2;

#[cfg(test)]
thread_local! {
    static THREAD_RECLAIM_PASSES: Cell<u64> = const { Cell::new(0) };
}

#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
#[allow(unsafe_code)]
unsafe extern "C" {
    fn mi_option_get(option: c_int) -> c_long;
    fn mi_option_set(option: c_int, value: c_long);
    fn mi_option_is_enabled(option: c_int) -> bool;
    fn mi_options_print();
    fn mi_collect(force: bool);
}

/// Apply the tuned mimalloc (and Linux glibc) options at process startup ([`tune`] calls it
/// when mimalloc is the selected allocator).
///
/// Must be called at the very start of `main()` before spawning worker threads
/// (`mi_option_set` is documented as non-thread-safe). When `MIMALLOC_VERBOSE=1` is
/// active, prints mimalloc's live option table after applying the overrides so
/// diagnostics and integration tests see the active runtime values.
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
#[allow(unsafe_code)]
pub fn tune_mimalloc() {
    // SAFETY: `libmimalloc-sys` compiles `static.c` (including `options.c`) into the
    // static `mimalloc` archive linked by `mimalloc = "0.1"`. All option indices are
    // valid `mi_option_t` values (`< _mi_option_last`) and are set at process entry
    // before multi-threaded work begins. On Linux GNU, `mallopt` configures glibc's
    // C allocator before worker threads create additional arenas.
    unsafe {
        mi_option_set(MI_OPTION_EAGER_COMMIT, TUNED_EAGER_COMMIT);
        mi_option_set(MI_OPTION_ARENA_EAGER_COMMIT, TUNED_EAGER_COMMIT);
        mi_option_set(MI_OPTION_PURGE_DECOMMITS, TUNED_PURGE_DECOMMITS);
        mi_option_set(MI_OPTION_PURGE_DELAY, TUNED_PURGE_DELAY_MS);
        mi_option_set(MI_OPTION_GENERIC_COLLECT, TUNED_GENERIC_COLLECT);
        mi_option_set(MI_OPTION_PAGE_RECLAIM_ON_FREE, TUNED_PAGE_RECLAIM_ON_FREE);
        mi_option_set(MI_OPTION_PAGE_FULL_RETAIN, TUNED_PAGE_FULL_RETAIN);
        // The option stops mimalloc's later `MADV_HUGEPAGE` calls; the arena reserved before
        // `main` already carries the flag, so also disable THP for the whole process (what
        // mimalloc itself does when `MIMALLOC_ALLOW_THP=0` is set in the environment).
        mi_option_set(MI_OPTION_ALLOW_THP, TUNED_ALLOW_THP);
        #[cfg(target_os = "linux")]
        {
            let _ = libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0);
        }
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        {
            let _ = libc::mallopt(libc::M_MMAP_THRESHOLD, TUNED_GLIBC_MMAP_THRESHOLD_BYTES);
            let _ = libc::mallopt(libc::M_ARENA_MAX, TUNED_GLIBC_ARENA_MAX);
        }
        if mi_option_is_enabled(MI_OPTION_VERBOSE) {
            mi_options_print();
        }
    }
}

/// After a large pass: under mimalloc, collect the calling thread's heap; under jemalloc,
/// flush the calling thread's cache back to its arena (the background thread purges freed
/// pages within `dirty_decay_ms`, for every thread's arena). Then trim glibc C-allocator
/// arenas (`malloc_trim(0)`) on Linux GNU.
///
/// mimalloc: `mi_collect(true)` collects only the calling thread's `theap` (`theap.c`:
/// "cannot really collect process wide, just a theap") — there is no public API to collect
/// other threads' heaps without a new crate. The arena purge it triggers *is*
/// process-wide (`_mi_arenas_collect(force_purge=true, visit_all=true)`), and
/// cross-thread abandoned-page reclaim happens on the free path
/// (`page_reclaim_on_free=1`, `generic_collect=500`). Starving new abandonments via
/// the bounded, 10s-keep-alive Tokio blocking pool (MEM6: 233 TIDs/30min) is the
/// lever Lane 1a ships; a broadcast collect cannot reach `aac-store` or `reqwest`
/// theaps through the public `mi_collect` surface.
#[allow(unsafe_code)]
pub fn reclaim_after_pass() {
    #[cfg(all(target_os = "linux", feature = "linux-jemalloc"))]
    let _ = jemalloc::run(c"thread.tcache.flush");
    // SAFETY: `mi_collect` is thread-safe (collects the calling thread's theap and purges
    // arenas process-wide); glibc `malloc_trim(0)` is thread-safe and returns unused heap
    // pages to the OS.
    unsafe {
        #[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
        mi_collect(true);
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        {
            let _ = libc::malloc_trim(0);
        }
    }
    #[cfg(test)]
    THREAD_RECLAIM_PASSES.with(|count| count.set(count.get().saturating_add(1)));
}

#[cfg(test)]
pub(crate) fn thread_reclaim_passes() -> u64 {
    THREAD_RECLAIM_PASSES.with(Cell::get)
}

/// Scope guard that invokes [`reclaim_after_pass`] on drop, after all later locals in the
/// enclosing pass have dropped their allocations and locks.
#[derive(Debug)]
pub struct ReclaimGuard(());

impl Drop for ReclaimGuard {
    fn drop(&mut self) {
        reclaim_after_pass();
    }
}

/// Return a guard that reclaims idle allocator pages when the enclosing pass exits.
#[must_use]
pub fn reclaim_on_drop() -> ReclaimGuard {
    ReclaimGuard(())
}

/// Snapshot of the tuned mimalloc runtime option values reported by `mi_option_get`.
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TunedOptions {
    /// Legacy segment eager-commit setting (`mi_option_eager_commit` / `deprecated_eager_commit`).
    pub eager_commit: c_long,
    /// Arena eager-commit setting (`mi_option_arena_eager_commit`).
    pub arena_eager_commit: c_long,
    /// Whether purges decommit pages (`mi_option_purge_decommits`).
    pub purge_decommits: c_long,
    /// Delay in milliseconds before purging freed pages (`mi_option_purge_delay`).
    pub purge_delay: c_long,
    /// Generic allocation interval between thread-heap collections (`mi_option_generic_collect`).
    pub generic_collect: c_long,
    /// Cross-thread abandoned-page reclaim mode (`mi_option_page_reclaim_on_free`).
    pub page_reclaim_on_free: c_long,
    /// Retained full small pages per size class (`mi_option_page_full_retain`).
    pub page_full_retain: c_long,
    /// Whether transparent huge pages are allowed (`mi_option_allow_thp`).
    pub allow_thp: c_long,
}

/// Read back the current runtime values of the tuned mimalloc options.
#[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
#[allow(unsafe_code)]
pub fn current_options() -> TunedOptions {
    // SAFETY: `mi_option_get` reads static option descriptors from the linked mimalloc archive.
    unsafe {
        TunedOptions {
            eager_commit: mi_option_get(MI_OPTION_EAGER_COMMIT),
            arena_eager_commit: mi_option_get(MI_OPTION_ARENA_EAGER_COMMIT),
            purge_decommits: mi_option_get(MI_OPTION_PURGE_DECOMMITS),
            purge_delay: mi_option_get(MI_OPTION_PURGE_DELAY),
            generic_collect: mi_option_get(MI_OPTION_GENERIC_COLLECT),
            page_reclaim_on_free: mi_option_get(MI_OPTION_PAGE_RECLAIM_ON_FREE),
            page_full_retain: mi_option_get(MI_OPTION_PAGE_FULL_RETAIN),
            allow_thp: mi_option_get(MI_OPTION_ALLOW_THP),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The compiled-in jemalloc options are the ones the running allocator reports.
    #[cfg(all(target_os = "linux", feature = "linux-jemalloc"))]
    #[test]
    fn jemalloc_runs_with_the_tuned_options() {
        tune();
        assert_eq!(
            jemalloc::read::<u32>(c"opt.narenas", 0),
            Some(TUNED_JEMALLOC_NARENAS)
        );
        assert_eq!(
            jemalloc::read::<isize>(c"opt.dirty_decay_ms", 0),
            Some(TUNED_JEMALLOC_DIRTY_DECAY_MS)
        );
        assert_eq!(
            jemalloc::read::<isize>(c"opt.muzzy_decay_ms", 0),
            Some(TUNED_JEMALLOC_MUZZY_DECAY_MS)
        );
        assert_eq!(
            jemalloc::read::<bool>(c"opt.background_thread", false),
            Some(true)
        );
        assert_eq!(
            jemalloc::read::<bool>(c"background_thread", false),
            Some(true)
        );
        assert!(jemalloc::version().is_some_and(|v| v.starts_with("5.")));
        assert!(jemalloc::run(c"thread.tcache.flush"));
    }

    #[cfg(not(all(target_os = "linux", feature = "linux-jemalloc")))]
    #[test]
    fn tune_mimalloc_sets_runtime_options() {
        tune_mimalloc();
        assert_eq!(
            current_options(),
            TunedOptions {
                eager_commit: TUNED_EAGER_COMMIT,
                arena_eager_commit: TUNED_EAGER_COMMIT,
                purge_decommits: TUNED_PURGE_DECOMMITS,
                purge_delay: TUNED_PURGE_DELAY_MS,
                generic_collect: TUNED_GENERIC_COLLECT,
                page_reclaim_on_free: TUNED_PAGE_RECLAIM_ON_FREE,
                page_full_retain: TUNED_PAGE_FULL_RETAIN,
                allow_thp: TUNED_ALLOW_THP,
            }
        );
    }

    /// The process runs without transparent huge pages after tuning (Linux).
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(unsafe_code)]
    fn tune_disables_transparent_huge_pages() {
        tune();
        // SAFETY: PR_GET_THP_DISABLE takes no pointers.
        let disabled = unsafe { libc::prctl(libc::PR_GET_THP_DISABLE, 0, 0, 0, 0) };
        assert_eq!(disabled, 1);
    }

    #[test]
    fn reclaim_on_drop_runs_after_pass_locals_drop() {
        let before = thread_reclaim_passes();
        let mut observed_at_inner_drop = None;
        struct Probe<'a>(&'a mut Option<u64>);
        impl Drop for Probe<'_> {
            fn drop(&mut self) {
                *self.0 = Some(thread_reclaim_passes());
            }
        }
        {
            let _reclaim = reclaim_on_drop();
            let _pass_local = Probe(&mut observed_at_inner_drop);
            assert_eq!(thread_reclaim_passes(), before);
        }
        assert_eq!(
            observed_at_inner_drop,
            Some(before),
            "pass locals must drop before post-pass reclaim runs"
        );
        assert_eq!(thread_reclaim_passes(), before + 1);
    }
}
