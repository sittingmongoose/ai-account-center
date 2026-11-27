//! Runtime tuning and post-pass reclamation for the process allocators.
//!
//! All three shipped service binaries (`ai-account-center`, `ccs`, `aac-http`) install
//! `mimalloc::MiMalloc` as their `#[global_allocator]`. Without tuning, mimalloc v3
//! eagerly commits arenas (`arena_eager_commit = 2` on overcommit OSes like Linux),
//! delays purging freed pages (`purge_delay = 1000`), and retains abandoned/full small
//! pages across short-lived blocking threads (`page_reclaim_on_free = 0`,
//! `page_full_retain = 2`, `generic_collect = 10000`). On Linux glibc, C libraries such
//! as `zstd-sys` also bypass Rust's global allocator and leave multi-MiB `ZSTD_CCtx`
//! buffers trapped in glibc arenas unless `M_MMAP_THRESHOLD` and `M_ARENA_MAX` are tuned.
//!
//! Calling [`tune_mimalloc`] as the first step of `main()` configures mimalloc in code
//! via `libmimalloc-sys`'s exported `mi_option_set` C API (and glibc `mallopt` on Linux
//! GNU) so the live service needs no environment variables and `Cargo.toml` / `Cargo.lock`
//! stay unchanged. Calling [`reclaim_on_drop`] or [`reclaim_after_pass`] after large
//! transient passes runs `mi_collect(true)` and (on Linux GNU) `malloc_trim(0)`.

#[cfg(test)]
use core::cell::Cell;
use core::ffi::{c_int, c_long};

// Keep an explicit reference to `mimalloc` so library and test targets always link the
// static `libmimalloc` archive that provides `mi_option_*` and `mi_collect`
// (via `libmimalloc-sys`).
const _: () = {
    let _ = core::mem::size_of::<mimalloc::MiMalloc>();
};

// Exact `mi_option_t` discriminants from `libmimalloc-sys 0.1.49`
// (`c_src/mimalloc/v3/include/mimalloc.h`, the default v3 build of `mimalloc = "0.1"`).
const MI_OPTION_VERBOSE: c_int = 2;
const MI_OPTION_EAGER_COMMIT: c_int = 3;
const MI_OPTION_ARENA_EAGER_COMMIT: c_int = 4;
const MI_OPTION_PURGE_DECOMMITS: c_int = 5;
const MI_OPTION_PURGE_DELAY: c_int = 15;
const MI_OPTION_GENERIC_COLLECT: c_int = 34;
const MI_OPTION_PAGE_RECLAIM_ON_FREE: c_int = 35;
const MI_OPTION_PAGE_FULL_RETAIN: c_int = 36;
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

#[allow(unsafe_code)]
unsafe extern "C" {
    fn mi_option_get(option: c_int) -> c_long;
    fn mi_option_set(option: c_int, value: c_long);
    fn mi_option_is_enabled(option: c_int) -> bool;
    fn mi_options_print();
    fn mi_collect(force: bool);
}

/// Apply the tuned mimalloc (and Linux glibc) options at process startup.
///
/// Must be called at the very start of `main()` before spawning worker threads
/// (`mi_option_set` is documented as non-thread-safe). When `MIMALLOC_VERBOSE=1` is
/// active, prints mimalloc's live option table after applying the overrides so
/// diagnostics and integration tests see the active runtime values.
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

/// Force mimalloc collection (`mi_collect(true)`) on the calling thread plus process-wide
/// arena purging, and trim glibc C-allocator arenas (`malloc_trim(0)`) on Linux GNU.
#[allow(unsafe_code)]
pub fn reclaim_after_pass() {
    // SAFETY: `mi_collect` is thread-safe (collects the calling thread's theap and purges
    // arenas process-wide); glibc `malloc_trim(0)` is thread-safe and returns unused heap
    // pages to the OS.
    unsafe {
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
    fn tune_mimalloc_disables_transparent_huge_pages() {
        tune_mimalloc();
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
