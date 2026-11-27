//! Performance memory-budget runner and synthetic fixture generator (lane X3).
//!
//! Ports `scripts/perf/memory-budget.mjs` and `scripts/perf/memory-fixture.mjs`
//! into `aac-xtask perf`, linking `aac-store` directly in Rust instead of `.node`.

pub mod base_fixtures;
pub mod budget;
pub mod fixture;
pub mod http_client;
pub mod proc_mem;

use std::error::Error;
use std::path::Path;

/// Entrypoint for `aac-xtask perf`.
///
/// Dispatches:
/// - `aac-xtask perf memory-fixture <out-dir> [...]` (alias `fixture`)
/// - `aac-xtask perf memory-budget [...]` (alias `budget`, also the default when flags or no subcommand are given)
pub fn run(root: &Path, args: &[String]) -> Result<(), Box<dyn Error>> {
    match args.first().map(String::as_str) {
        Some("memory-fixture" | "fixture") => fixture::run_cli(root, &args[1..]),
        Some("memory-budget" | "budget") => budget::run_cli(root, &args[1..]),
        Some("-h" | "--help" | "help") => {
            println!(
                "Usage:\n  aac-xtask perf [memory-budget] [--duration 300] [--steady-after 120] [--sample-interval 2] [--seed 20261125] [--config config/memory-budget.json] [--out <file.json>] [--fixture-dir <dir>] [--keep] [--no-check] [--json]\n  aac-xtask perf memory-fixture <out-dir> [--seed 20261125] [--now <epochMs>] [--runs 7500] [--writers 2000] [--registry 4500] [--days 60] [--accounts 19] [--skip-store]"
            );
            Ok(())
        }
        _ => budget::run_cli(root, args),
    }
}
