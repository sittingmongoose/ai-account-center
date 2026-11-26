//! Runtime tuning for the host agent's global `mimalloc` allocator (R4).
//!
//! `aac-agent` uses `mimalloc::MiMalloc` as its `#[global_allocator]` so transient
//! scan allocations are purged back to the OS immediately instead of remaining pinned
//! as RSS in allocator arenas.

use core::ffi::{c_int, c_long};

const _: () = {
    let _ = core::mem::size_of::<mimalloc::MiMalloc>();
};

// Exact `mi_option_t` discriminants from `libmimalloc-sys 0.1.49`.
const MI_OPTION_VERBOSE: c_int = 2;
const MI_OPTION_EAGER_COMMIT: c_int = 3;
const MI_OPTION_ARENA_EAGER_COMMIT: c_int = 4;
const MI_OPTION_PURGE_DECOMMITS: c_int = 5;
const MI_OPTION_PURGE_DELAY: c_int = 15;

/// Tuned `purge_delay` (ms): purge freed pages back to the OS immediately.
pub const TUNED_PURGE_DELAY_MS: c_long = 0;
/// Tuned `eager_commit` / `arena_eager_commit`: disable eager commit (`0`).
pub const TUNED_EAGER_COMMIT: c_long = 0;
/// Tuned `purge_decommits`: decommit purged pages (`1`).
pub const TUNED_PURGE_DECOMMITS: c_long = 1;

#[allow(unsafe_code)]
unsafe extern "C" {
    fn mi_option_get(option: c_int) -> c_long;
    fn mi_option_set(option: c_int, value: c_long);
    fn mi_option_is_enabled(option: c_int) -> bool;
    fn mi_options_print();
}

/// Apply the tuned mimalloc options at process startup before spawning any worker threads.
#[allow(unsafe_code)]
pub fn tune_mimalloc() {
    // SAFETY: `libmimalloc-sys` compiles `static.c` (including `options.c`) into the
    // static `mimalloc` archive linked by `mimalloc = "0.1"`. All four option indices
    // are valid `mi_option_t` values set at process entry before worker threads start.
    unsafe {
        mi_option_set(MI_OPTION_EAGER_COMMIT, TUNED_EAGER_COMMIT);
        mi_option_set(MI_OPTION_ARENA_EAGER_COMMIT, TUNED_EAGER_COMMIT);
        mi_option_set(MI_OPTION_PURGE_DECOMMITS, TUNED_PURGE_DECOMMITS);
        mi_option_set(MI_OPTION_PURGE_DELAY, TUNED_PURGE_DELAY_MS);
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
        }
    }
}
