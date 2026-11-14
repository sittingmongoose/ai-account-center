//! Unix-only skeleton service; its boot secret arrives on stdin, never argv or logs.
#[cfg(unix)]
#[tokio::main(worker_threads = 2)]
async fn main() {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    let mut input = BufReader::new(tokio::io::stdin()).take(8193);
    let mut line = String::new();
    let boot = match tokio::time::timeout(
        std::time::Duration::from_secs(5),
        input.read_line(&mut line),
    )
    .await
    {
        Ok(Ok(n)) if n > 0 && n <= 8192 => aac_http::Boot::from_json(line.as_bytes()).ok(),
        _ => None,
    };
    let Some(boot) = boot else {
        eprintln!("aac-http: invalid boot input");
        std::process::exit(1);
    };
    line.clear();
    // Parent death closes the pipe. TS also sends SIGTERM on normal cleanup.
    let shutdown = async move {
        let mut byte = [0u8; 1];
        let signal = async {
            if let Ok(mut signal) =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            {
                signal.recv().await;
            }
        };
        tokio::select! { _ = input.read(&mut byte) => {}, _ = signal => {}, _ = tokio::signal::ctrl_c() => {} }
    };
    if aac_http::serve(boot, shutdown).await.is_err() {
        eprintln!("aac-http: private service failed");
        std::process::exit(1);
    }
    // Tokio stdin uses a blocking reader; do not wait for it after a termination signal.
    std::process::exit(0);
}

#[cfg(not(unix))]
fn main() {
    eprintln!("aac-http: Unix sockets required; TypeScript fallback remains active");
    std::process::exit(1);
}
