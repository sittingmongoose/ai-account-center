//! CLI entrypoint for `aac-xtask`.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = match aac_xtask::find_repo_root() {
        Ok(root) => root,
        Err(err) => {
            eprintln!("[X] {err}");
            return ExitCode::from(1);
        }
    };
    match aac_xtask::dispatch(&root, &args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            if let Some(exit) = err.downcast_ref::<aac_xtask::SilentExit>() {
                let code = u8::try_from(exit.0).unwrap_or(1);
                return ExitCode::from(code);
            }
            eprintln!("[X] {err}");
            ExitCode::from(1)
        }
    }
}
