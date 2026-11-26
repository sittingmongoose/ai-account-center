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
    } else if args.is_empty() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .max_blocking_threads(8)
            .enable_all()
            .build();
        match runtime {
            Ok(runtime) => runtime
                .block_on(aac_agent::run(
                    BufReader::new(tokio::io::stdin()),
                    tokio::io::stdout(),
                ))
                .unwrap_or(1),
            Err(_) => 1,
        }
    } else {
        2
    };
    std::process::exit(code);
}
