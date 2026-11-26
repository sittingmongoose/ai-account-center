//! Performance memory-budget and synthetic fixture tasks (owned by lane X3).

use std::error::Error;
use std::path::Path;

/// Entrypoint for `aac-xtask perf`.
pub fn run(_root: &Path, _args: &[String]) -> Result<(), Box<dyn Error>> {
    Err("aac-xtask perf is not implemented yet".into())
}
