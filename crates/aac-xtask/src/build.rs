//! Build tasks (owned by lane X1).

use std::error::Error;
use std::path::Path;

/// Entrypoint for `aac-xtask build`.
pub fn run(_root: &Path, _args: &[String]) -> Result<(), Box<dyn Error>> {
    Err("aac-xtask build is not implemented in this branch".into())
}
