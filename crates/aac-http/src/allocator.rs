//! Runtime tuning for the process global `mimalloc` allocator.
//!
//! All three shipped service binaries (`ai-account-center`, `ccs`, `aac-http`) install
//! `mimalloc::MiMalloc` as their `#[global_allocator]`. Without tuning, mimalloc v3
//! eagerly commits arenas (`arena_eager_commit = 2` on overcommit OSes like Linux) and
//! delays purging freed pages by `1000 ms` (`purge_delay = 1000`), leaving hundreds of
//! MiB of transient compose/analytics allocations resident.
//!
//! Calling [`tune_mimalloc`] as the first step of `main()` configures mimalloc in code
//! via `libmimalloc-sys`'s exported `mi_option_set` C API so the live service needs no
//! environment variables and `Cargo.toml` / `Cargo.lock` stay unchanged.

use core::ffi::{c_int, c_long};

// Keep an explicit reference to `mimalloc` so library and test targets always link the
// static `libmimalloc` archive that provides `mi_option_*` (via `libmimalloc-sys`).
const _: () = {
    let _ = core::mem::size_of::<mimalloc::MiMalloc>();
};

// Exact `mi_option_t` discriminants from `libmimalloc-sys 0.1.49` (`c_src/mimalloc/v3/include/mimalloc.h`
// and identical in `v2/include/mimalloc.h`).
const MI_OPTION_VERBOSE: c_int = 2;
const MI_OPTION_EAGER_COMMIT: c_int = 3;
const MI_OPTION_ARENA_EAGER_COMMIT: c_int = 4;
const MI_OPTION_PURGE_DECOMMITS: c_int = 5;
const MI_OPTION_PURGE_DELAY: c_int = 15;
const MI_OPTION_ALLOW_THP: c_int = 43;

/// Tuned `purge_delay` (ms): purge freed pages back to the OS immediately.
pub const TUNED_PURGE_DELAY_MS: c_long = 0;
/// Tuned `eager_commit` / `arena_eager_commit`: disable eager commit (`0`).
pub const TUNED_EAGER_COMMIT: c_long = 0;
/// Tuned `purge_decommits`: decommit purged pages (`MADV_DONTNEED` / `MEM_DECOMMIT`, `1`).
pub const TUNED_PURGE_DECOMMITS: c_long = 1;
/// Tuned `allow_thp`: no transparent huge pages (`0`). mimalloc marks its 1 GiB arena
/// `MADV_HUGEPAGE`, so each touched 2 MiB stretch becomes one resident huge page, and a 4 KiB
/// purge inside it cannot hand memory back (live r32: 76-108 MiB `AnonHugePages`).
pub const TUNED_ALLOW_THP: c_long = 0;

#[allow(unsafe_code)]
unsafe extern "C" {
    fn mi_option_get(option: c_int) -> c_long;
    fn mi_option_set(option: c_int, value: c_long);
    fn mi_option_is_enabled(option: c_int) -> bool;
    fn mi_options_print();
}

/// Apply the tuned mimalloc options at process startup.
///
/// Must be called at the very start of `main()` before spawning worker threads
/// (`mi_option_set` is documented as non-thread-safe). When `MIMALLOC_VERBOSE=1` is
/// active, prints mimalloc's live option table after applying the overrides so
/// diagnostics and integration tests see the active runtime values.
#[allow(unsafe_code)]
pub fn tune_mimalloc() {
    // SAFETY: `libmimalloc-sys` compiles `static.c` (including `options.c`) into the
    // static `mimalloc` archive linked by `mimalloc = "0.1"`. All four option indices
    // are valid `mi_option_t` values (`< _mi_option_last`) and are set at process entry
    // before multi-threaded work begins.
    unsafe {
        mi_option_set(MI_OPTION_EAGER_COMMIT, TUNED_EAGER_COMMIT);
        mi_option_set(MI_OPTION_ARENA_EAGER_COMMIT, TUNED_EAGER_COMMIT);
        mi_option_set(MI_OPTION_PURGE_DECOMMITS, TUNED_PURGE_DECOMMITS);
        mi_option_set(MI_OPTION_PURGE_DELAY, TUNED_PURGE_DELAY_MS);
        // The option stops mimalloc's later `MADV_HUGEPAGE` calls; the arena reserved before
        // `main` already carries the flag, so also disable THP for the whole process (what
        // mimalloc itself does when `MIMALLOC_ALLOW_THP=0` is set in the environment).
        mi_option_set(MI_OPTION_ALLOW_THP, TUNED_ALLOW_THP);
        #[cfg(target_os = "linux")]
        {
            let _ = libc::prctl(libc::PR_SET_THP_DISABLE, 1, 0, 0, 0);
        }
        if mi_option_is_enabled(MI_OPTION_VERBOSE) {
            mi_options_print();
        }
    }
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
}
