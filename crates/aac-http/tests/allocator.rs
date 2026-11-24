//! The shipped service binaries must run on mimalloc as their global allocator:
//! under glibc malloc, freed compose transients stayed resident as multi-GB of
//! arena RSS (service-memory lane, measured on the 54c363fa7 build).
//!
//! A byte scan for the word "mimalloc" would pass on any string that names it, so
//! this asks the running allocator itself: with `MIMALLOC_VERBOSE=1`, mimalloc
//! reports its process start on stderr. A binary whose `#[global_allocator]` is
//! gone does not link mimalloc at all and prints nothing.

use std::process::Command;

/// mimalloc's own start line (`_mi_verbose_message("process init: ...")`).
const STARTED: &str = "mimalloc: process init: 0x";

#[test]
fn service_binaries_run_on_mimalloc() -> Result<(), Box<dyn std::error::Error>> {
    let home = tempfile::tempdir()?;
    for (name, path, flag) in [
        (
            "ai-account-center",
            env!("CARGO_BIN_EXE_ai-account-center"),
            "--help",
        ),
        ("ccs", env!("CARGO_BIN_EXE_ccs"), "--help"),
        ("aac-http", env!("CARGO_BIN_EXE_aac-http"), "--build-commit"),
    ] {
        let mut command = Command::new(path);
        command
            .arg(flag)
            .env_clear()
            .env("HOME", home.path())
            .env("USERPROFILE", home.path())
            .env("CCS_HOME", home.path().join(".ccs"))
            .env("MIMALLOC_VERBOSE", "1");
        for kept in ["PATH", "SystemRoot"] {
            if let Some(value) = std::env::var_os(kept) {
                command.env(kept, value);
            }
        }
        let output = command.output()?;
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{name} {flag} failed: {:?}\n{stderr}",
            output.status
        );
        assert!(
            stderr.contains(STARTED),
            "{name} did not start on mimalloc (no {STARTED:?} on stderr):\n{stderr}"
        );
    }
    Ok(())
}
