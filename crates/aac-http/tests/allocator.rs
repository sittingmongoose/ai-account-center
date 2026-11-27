//! The shipped service binaries must run on mimalloc as their global allocator
//! with our tuned memory-return options applied in code at startup:
//! under glibc malloc, freed compose transients stayed resident as multi-GB of
//! arena RSS (service-memory lane, measured on the 54c363fa7 build), and under
//! untuned mimalloc, eager arena commit and 1000 ms purge delay held ~100+ MiB
//! of avoidable RSS.
//!
//! A byte scan for the word "mimalloc" would pass on any string that names it, so
//! this asks the running allocator itself: with `MIMALLOC_VERBOSE=1`, mimalloc
//! reports its process start on stderr, and `tune_mimalloc()` prints mimalloc's
//! own post-tuning option table (`mi_options_print`).

use std::collections::BTreeMap;
use std::process::Command;

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
        .env("MIMALLOC_VERBOSE", "1");
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

#[test]
fn service_binaries_run_on_mimalloc() -> Result<(), Box<dyn std::error::Error>> {
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
    }
    Ok(())
}
