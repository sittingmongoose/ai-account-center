//! Stdio entry point for the host agent; stdout is protocol data only.

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use std::io::Write;
use tokio::io::BufReader;

fn main() {
    aac_agent::allocator::tune_mimalloc();
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let code = if args.len() == 1 && args[0] == "--version" {
        match serde_json::to_vec(&aac_agent::handshake()) {
            Ok(mut bytes) => {
                bytes.push(b'\n');
                let mut output = std::io::stdout().lock();
                if output.write_all(&bytes).is_ok() && output.flush().is_ok() {
                    0
                } else {
                    1
                }
            }
            Err(_) => 1,
        }
    } else if args.len() == 1 && args[0] == "--claude-reset" {
        block_on(aac_agent::claude_reset_stdio())
    } else if args.len() == 2 && args[0] == "--claude-reset-gui" {
        block_on(aac_agent::claude_reset_gui(std::path::Path::new(&args[1])))
    } else if args.first().is_some_and(|arg| arg == "--console-probe") {
        aac_agent::console_probe(&args[1..])
    } else if let Some(code) = args
        .first()
        .and_then(|flag| aac_agent::app_update_subcommand(flag, &args[1..]))
    {
        code
    } else if args.len() == 1 && args[0] == "--app-update-task" {
        block_on(aac_agent::app_update_task())
    } else if args.first().is_some_and(|arg| arg == "--terminal-child") {
        aac_agent::terminal_child(&args[1..])
    } else if args.is_empty() {
        block_on(async {
            aac_agent::run(BufReader::new(tokio::io::stdin()), tokio::io::stdout())
                .await
                .unwrap_or(1)
        })
    } else {
        2
    };
    std::process::exit(code);
}

/// One bounded runtime per process (two workers, eight blocking threads); exit 1 if it can't start.
fn block_on(future: impl std::future::Future<Output = i32>) -> i32 {
    match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(8)
        .enable_all()
        .build()
    {
        Ok(runtime) => {
            let code = runtime.block_on(future);
            // Never drop the runtime normally: tokio's stdin reader is a blocking-pool thread
            // parked in read(0), and `Runtime::drop` waits for it until the parent closes
            // stdin. The transport keeps stdin open for cancel frames until this process
            // exits, so a normal drop holds every controlled call to its deadline (AR4b).
            runtime.shutdown_background();
            code
        }
        Err(_) => 1,
    }
}
