//! Private Unix service and the explicitly enabled public TCP front door.
#[cfg(unix)]
mod registered_write;

fn main() {
    #[cfg(unix)]
    match std::env::args_os()
        .nth(1)
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
    {
        Some("--file-lease") => std::process::exit(file_lease()),
        Some("--registered-write") => std::process::exit(registered_write::commit()),
        _ => {}
    }
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
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--public")) {
        let _ = tracing_subscriber::fmt()
            .json()
            .with_writer(std::io::stderr)
            .try_init();
        if let Err(error) = runtime.block_on(aac_http::public_auth::run_public()) {
            eprintln!("aac-http: {error}");
            std::process::exit(1);
        }
        // The optional event feed uses a blocking stdin reader.
        std::process::exit(0);
    }
    #[cfg(unix)]
    runtime.block_on(run());
    #[cfg(not(unix))]
    {
        eprintln!("aac-http: Unix sockets required for the private service");
        std::process::exit(1);
    }
}

#[cfg(unix)]
fn file_lease() -> i32 {
    use std::fs::{File, TryLockError};
    use std::os::fd::AsFd;
    let Ok(fd) = std::io::stdin().as_fd().try_clone_to_owned() else {
        return 74;
    };
    let file = File::from(fd);
    if !file.metadata().is_ok_and(|metadata| metadata.is_file()) {
        return 74;
    }
    match file.try_lock() {
        Ok(()) => 0,
        Err(TryLockError::WouldBlock) => 73,
        Err(TryLockError::Error(_)) => 74,
    }
    // The parent keeps its copy of the open file description.
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
    if aac_http::serve(boot, shutdown).await.is_err() {
        eprintln!("aac-http: private service failed");
        std::process::exit(1);
    }
    // Tokio stdin uses a blocking reader; do not wait for it after a termination signal.
    std::process::exit(0);
}
