//! AI Account Center development, build, release, and performance tasks.

pub mod build;
pub mod bundle;
pub mod file_policy;
pub mod fixture;
pub mod guards;
pub mod lineage;
pub mod perf;
pub mod release;
pub mod ui;

use std::error::Error;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

/// Exit with a specific status code without printing an extra `[X]` error line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SilentExit(pub i32);

impl fmt::Display for SilentExit {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "exited with status {}", self.0)
    }
}

impl Error for SilentExit {}

/// Locate the repository workspace root from the current directory or `CARGO_MANIFEST_DIR`.
pub fn find_repo_root() -> Result<PathBuf, Box<dyn Error>> {
    if let Ok(cwd) = std::env::current_dir() {
        for ancestor in cwd.ancestors() {
            if is_workspace_root(ancestor) {
                return Ok(ancestor.to_path_buf());
            }
        }
    }
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest_dir.parent().and_then(Path::parent)
        && is_workspace_root(root)
    {
        return Ok(root.to_path_buf());
    }
    Err("could not locate AI Account Center workspace root".into())
}

fn is_workspace_root(dir: &Path) -> bool {
    let Ok(text) = fs::read_to_string(dir.join("Cargo.toml")) else {
        return false;
    };
    text.contains("[workspace]")
        && dir
            .join("config")
            .join("tables")
            .join("providers.json")
            .is_file()
}

/// Dispatch an `aac-xtask` CLI command against `root`.
pub fn dispatch(root: &Path, args: &[String]) -> Result<(), Box<dyn Error>> {
    let Some((cmd, rest)) = args.split_first() else {
        print_usage();
        return Err(Box::new(SilentExit(2)));
    };
    match cmd.as_str() {
        "-h" | "--help" | "help" => {
            print_usage();
            Ok(())
        }
        "build" => build::run(root, rest),
        "ui-validate" => ui::run(root, rest),
        "perf" => perf::run(root, rest),
        "release" => release::run(root, rest),
        "check" => guards::run(root, rest),
        "fixture" => fixture::run(rest),
        other => Err(format!("Unknown xtask command: {other}").into()),
    }
}

fn print_usage() {
    println!(
        "Usage: aac-xtask <command> [args...]\n\nCommands:\n  build         Build dashboard UI and native artifacts\n  ui-validate   Validate built Leptos UI artifacts\n  release       Verify, stage, and rehearse release packages\n  check         Check personal details, line endings, docs, and inventory\n  perf          Run memory budget benchmark or generate synthetic fixture\n  fixture       Generate Status or Scoreboard reports"
    );
}
