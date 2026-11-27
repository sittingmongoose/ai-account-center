//! The shipped service binaries must run on their selected global allocator with our tuned
//! memory-return options applied at startup.
//!
//! Linux (MEM7 lane 1): tuned jemalloc. Release 38 on tuned mimalloc v3 kept ~220-245 MiB of
//! freed pages resident or swapped above a ~85-110 MiB live heap; the MEM6 twin on jemalloc
//! (two arenas, 1 s dirty decay, no muzzy stage, background purging) held a flat trough.
//! macOS / Windows (and Linux without `linux-jemalloc`): tuned mimalloc. Under glibc malloc,
//! freed compose transients stayed resident as multi-GB of arena RSS (service-memory lane,
//! measured on the 54c363fa7 build), and under untuned mimalloc, eager arena commit and
//! 1000 ms purge delay held ~100+ MiB of avoidable RSS.
//!
//! A byte scan for an allocator's name would pass on any string that names it, so this asks
//! the running allocator itself: jemalloc through `mallctl` (`AAC_ALLOCATOR_REPORT=1` prints
//! its version, options and whether a Rust allocation moved its counters), mimalloc through
//! `MIMALLOC_VERBOSE=1` (its process start line and post-tuning option table).

use std::collections::BTreeMap;
use std::process::Command;

/// Whether this build selects jemalloc (Linux with the default `linux-jemalloc` feature).
const JEMALLOC: bool = cfg!(all(target_os = "linux", feature = "linux-jemalloc"));

/// mimalloc's own start line (`_mi_verbose_message("process init: ...")`).
const STARTED: &str = "mimalloc: process init: 0x";

const BINARIES: [(&str, &str, &str); 3] = [
    (
        "ai-account-center",
        env!("CARGO_BIN_EXE_ai-account-center"),
        "--help",
    ),
    ("ccs", env!("CARGO_BIN_EXE_ccs"), "--help"),
    ("aac-http", env!("CARGO_BIN_EXE_aac-http"), "--build-commit"),
];

fn run_verbose(path: &str, flag: &str, home: &std::path::Path) -> Result<String, std::io::Error> {
    let mut command = Command::new(path);
    command
        .arg(flag)
        .env_clear()
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("CCS_HOME", home.join(".ccs"))
        .env("MIMALLOC_VERBOSE", "1")
        .env("AAC_ALLOCATOR_REPORT", "1");
    for kept in ["PATH", "SystemRoot"] {
        if let Some(value) = std::env::var_os(kept) {
            command.env(kept, value);
        }
    }
    let output = command.output()?;
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "{path} {flag} failed: {:?}\n{stderr}",
        output.status
    );
    Ok(stderr)
}

/// Parse mimalloc's `option '<name>': <value>` lines, keeping the last value reported
/// for each option (the post-`tune_mimalloc()` `mi_options_print()` output).
fn parse_final_options(stderr: &str) -> BTreeMap<String, i64> {
    let mut options = BTreeMap::new();
    for line in stderr.lines() {
        let Some(rest) = line.trim().strip_prefix("option '") else {
            continue;
        };
        let Some((name, tail)) = rest.split_once("':") else {
            continue;
        };
        let Some(token) = tail.split_whitespace().next() else {
            continue;
        };
        if let Ok(value) = token.parse::<i64>() {
            options.insert(name.to_owned(), value);
        }
    }
    options
}

/// The `key=value` fields of the binary's `aac-allocator: <name> ...` report line.
fn parse_report(stderr: &str) -> Option<(String, BTreeMap<String, String>)> {
    let line = stderr
        .lines()
        .find_map(|line| line.trim().strip_prefix("aac-allocator: "))?;
    let mut words = line.split_whitespace();
    let name = words.next()?.to_owned();
    let fields = words
        .filter_map(|word| word.split_once('='))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect();
    Some((name, fields))
}

#[test]
fn service_binaries_run_on_tuned_jemalloc_on_linux() -> Result<(), Box<dyn std::error::Error>> {
    if !JEMALLOC {
        return Ok(());
    }
    let home = tempfile::tempdir()?;
    for (name, path, flag) in BINARIES {
        let stderr = run_verbose(path, flag, home.path())?;
        let Some((allocator, fields)) = parse_report(&stderr) else {
            panic!("{name} printed no allocator report:\n{stderr}");
        };
        assert_eq!(allocator, "jemalloc", "{name}:\n{stderr}");
        let field = |key: &str| fields.get(key).map(String::as_str);
        assert!(
            field("version").is_some_and(|v| v.starts_with("5.")),
            "{name} did not report a jemalloc 5 version:\n{stderr}"
        );
        // A 4 MiB Rust allocation moved jemalloc's own per-thread counter: Rust's global
        // allocator is jemalloc, not just linked.
        assert_eq!(field("rust_heap"), Some("yes"), "{name}:\n{stderr}");
        assert_eq!(field("narenas"), Some("2"), "{name}:\n{stderr}");
        assert_eq!(field("dirty_decay_ms"), Some("1000"), "{name}:\n{stderr}");
        assert_eq!(field("muzzy_decay_ms"), Some("0"), "{name}:\n{stderr}");
        assert_eq!(
            field("background_thread"),
            Some("true"),
            "{name}:\n{stderr}"
        );
        // mimalloc may still print its start line in debug builds: the `aac-agent` library
        // (agent closure, unchanged) keeps its own `tune_mimalloc`, which links mimalloc
        // without making it the allocator; `rust_heap=yes` above is the allocator check.
    }
    Ok(())
}

#[test]
fn service_binaries_run_on_mimalloc() -> Result<(), Box<dyn std::error::Error>> {
    if JEMALLOC {
        return Ok(());
    }
    let home = tempfile::tempdir()?;
    for (name, path, flag) in BINARIES {
        let stderr = run_verbose(path, flag, home.path())?;
        assert!(
            // Linux prints a process-init banner; the Mac prints only the option table.
            stderr.contains(STARTED) || stderr.contains("option 'purge_delay'"),
            "{name} did not start on mimalloc (no {STARTED:?} on stderr):\n{stderr}"
        );
    }
    Ok(())
}

#[test]
fn service_binaries_apply_tuned_mimalloc_options_at_runtime()
-> Result<(), Box<dyn std::error::Error>> {
    if JEMALLOC {
        return Ok(());
    }
    let home = tempfile::tempdir()?;
    for (name, path, flag) in BINARIES {
        let stderr = run_verbose(path, flag, home.path())?;
        let options = parse_final_options(&stderr);
        assert_eq!(
            options.get("arena_eager_commit").copied(),
            Some(0),
            "{name} did not disable arena_eager_commit at runtime:\n{stderr}"
        );
        assert_eq!(
            options.get("purge_decommits").copied(),
            Some(1),
            "{name} did not enable purge_decommits at runtime:\n{stderr}"
        );
        assert_eq!(
            options.get("purge_delay").copied(),
            Some(0),
            "{name} did not set purge_delay=0 at runtime:\n{stderr}"
        );
        // v3 reports `deprecated_eager_commit`, v2 reports `eager_commit`.
        let eager = options
            .get("deprecated_eager_commit")
            .or_else(|| options.get("eager_commit"))
            .copied();
        assert_eq!(
            eager,
            Some(0),
            "{name} did not disable eager_commit at runtime:\n{stderr}"
        );
        assert_eq!(
            options.get("allow_thp").copied(),
            Some(0),
            "{name} did not disable allow_thp at runtime:\n{stderr}"
        );
        assert_eq!(
            options.get("page_reclaim_on_free").copied(),
            Some(1),
            "{name} did not set page_reclaim_on_free=1 at runtime:\n{stderr}"
        );
        assert_eq!(
            options.get("page_full_retain").copied(),
            Some(0),
            "{name} did not set page_full_retain=0 at runtime:\n{stderr}"
        );
        assert_eq!(
            options.get("generic_collect").copied(),
            Some(500),
            "{name} did not set generic_collect=500 at runtime:\n{stderr}"
        );
    }
    Ok(())
}

/// On Linux glibc, `zstd-sys` allocates 1.3-2.5 MiB `ZSTD_CCtx` buffers through C `malloc`,
/// bypassing Rust's global allocator (jemalloc's symbols are prefixed, so C stays on glibc).
/// Without `mallopt(M_MMAP_THRESHOLD, 128 KiB)`, freeing the first 2 MiB buffer raises glibc's dynamic mmap threshold so the second 2 MiB
/// allocation lands in a 64 MiB glibc heap arena instead of `mmap`.
#[cfg(all(target_os = "linux", target_env = "gnu"))]
#[test]
#[allow(unsafe_code)]
fn tune_keeps_glibc_large_buffers_on_mmap() {
    aac_http::allocator::tune();
    const TWO_MIB: usize = 2 * 1024 * 1024;
    // SAFETY: `libc::malloc`, `write_bytes`, `libc::free`, and `libc::mallinfo2` are called
    // with valid sizes and live non-null pointers returned by `libc::malloc`.
    unsafe {
        let first = libc::malloc(TWO_MIB);
        assert!(!first.is_null(), "first 2 MiB libc::malloc failed");
        core::ptr::write_bytes(first.cast::<u8>(), 0x5a, TWO_MIB);
        libc::free(first);

        let before = libc::mallinfo2();
        let second = libc::malloc(TWO_MIB);
        assert!(!second.is_null(), "second 2 MiB libc::malloc failed");
        core::ptr::write_bytes(second.cast::<u8>(), 0xa5, TWO_MIB);
        let during = libc::mallinfo2();
        libc::free(second);
        let after = libc::mallinfo2();

        assert!(
            during.hblkhd >= before.hblkhd + TWO_MIB,
            "second 2 MiB C allocation bypassed mmap into a glibc arena (hblkhd before={}, during={})",
            before.hblkhd,
            during.hblkhd
        );
        assert!(
            after.hblkhd <= before.hblkhd,
            "freed 2 MiB C allocation was not unmapped (hblkhd before={}, after={})",
            before.hblkhd,
            after.hblkhd
        );
    }
}
