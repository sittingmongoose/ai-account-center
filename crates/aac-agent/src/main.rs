//! Stdio entry point for the host agent; stdout is protocol data only.
use tokio::io::{AsyncWriteExt, BufReader};

#[tokio::main]
async fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let code = if args.len() == 1 && args[0] == "--version" {
        match serde_json::to_vec(&aac_agent::handshake()) {
            Ok(mut bytes) => {
                bytes.push(b'\n');
                let mut output = tokio::io::stdout();
                match output.write_all(&bytes).await {
                    Ok(()) => {
                        if output.flush().await.is_ok() {
                            0
                        } else {
                            1
                        }
                    }
                    Err(_) => 1,
                }
            }
            Err(_) => 1,
        }
    } else if args.len() == 1 && args[0] == "--claude-reset" {
        aac_agent::claude_reset_stdio().await
    } else if args.len() == 2 && args[0] == "--claude-reset-gui" {
        aac_agent::claude_reset_gui(std::path::Path::new(&args[1])).await
    } else if args.is_empty() {
        aac_agent::run(BufReader::new(tokio::io::stdin()), tokio::io::stdout())
            .await
            .unwrap_or(1)
    } else {
        2
    };
    std::process::exit(code);
}
