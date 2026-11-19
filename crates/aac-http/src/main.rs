//! Unix-only skeleton service; its boot secret arrives on stdin, never argv or logs.
#[cfg(unix)]
fn main() {
    // Two async workers; at most 20 blocking threads: the 8 read, 4 store and 4 fed-Status
    // permits, plus spare. The permits, not this cap, are the bound the routes rely on.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(20)
        .enable_all()
        .build();
    let Ok(runtime) = runtime else {
        eprintln!("aac-http: runtime failed");
        std::process::exit(1);
    };
    runtime.block_on(run());
}

#[cfg(unix)]
async fn run() {
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
    // G13: the claude-open group binds the Open job owner; the package root supplies the
    // pinned reference helpers. Absent either, the internal routes decline and TS answers.
    let package_root = std::env::var_os("AAC_AGENT_PACKAGE_ROOT").map(std::path::PathBuf::from);
    let open_on = std::env::var("AAC_RUST_GROUP_CLAUDE_OPEN").is_ok_and(|value| value == "on");
    let served = match (open_on, package_root) {
        (true, Some(package)) => aac_http::serve_with_claude_open(boot, shutdown, package).await,
        _ => aac_http::serve(boot, shutdown).await,
    };
    if served.is_err() {
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
