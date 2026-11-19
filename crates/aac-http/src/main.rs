//! Unix-only skeleton service; its boot secret arrives on stdin, never argv or logs.
#[cfg(unix)]
mod lifecycle_policy;
#[cfg(unix)]
mod provider_write;
#[cfg(unix)]
mod registered_write;

#[cfg(unix)]
fn main() {
    match std::env::args_os()
        .nth(1)
        .as_deref()
        .and_then(std::ffi::OsStr::to_str)
    {
        Some("--file-lease") => std::process::exit(file_lease()),
        Some("--sweep-orphan-keys") => std::process::exit(registered_write::sweep_orphan_keys()),
        Some("--registered-write") => std::process::exit(registered_write::commit()),
        Some("--provider-write") => std::process::exit(provider_write::commit()),
        Some("--codex-staging") => std::process::exit(provider_write::staging(false)),
        Some("--antigravity-staging") => std::process::exit(provider_write::staging(true)),
        Some("--claude-workflow") => std::process::exit(provider_workflow(false)),
        Some("--key-workflow") => std::process::exit(provider_workflow(true)),
        Some("--lifecycle-maintenance") => std::process::exit(lifecycle_maintenance()),
        Some("--lifecycle-queue") => std::process::exit(lifecycle_queue()),
        Some("--lifecycle-plan") => std::process::exit(lifecycle_policy::run()),
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
    runtime.block_on(run());
}

#[cfg(unix)]
fn provider_workflow(key: bool) -> i32 {
    let Some(mode) = std::env::args().nth(2) else {
        return 74;
    };
    let runtime = tokio::runtime::Builder::new_current_thread().build();
    let result = runtime.and_then(|runtime| {
        if key {
            runtime.block_on(aac_http::key_lifecycle::compatibility_pipe(&mode))
        } else {
            runtime.block_on(aac_http::claude_lifecycle::compatibility_pipe(&mode))
        }
    });
    if result.is_ok() { 0 } else { 74 }
}

#[cfg(unix)]
fn lifecycle_maintenance() -> i32 {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build();
    let result = runtime.and_then(|runtime| {
        runtime.block_on(aac_http::lifecycle_maintenance::compatibility_pipe())
    });
    if result.is_ok() { 0 } else { 74 }
}

#[cfg(unix)]
fn lifecycle_queue() -> i32 {
    use std::{fs::File, os::fd::AsFd, path::Path};
    let mut args = std::env::args_os().skip(2);
    let (Some(root), Some(scope), None) = (args.next(), args.next(), args.next()) else {
        return 74;
    };
    let Some(scope) = scope.to_str() else {
        return 74;
    };
    let Ok(fd) = std::io::stdin().as_fd().try_clone_to_owned() else {
        return 74;
    };
    match aac_files::lifecycle_queue::lock_inherited(Path::new(&root), scope, &File::from(fd)) {
        Ok(()) => 0,
        Err(_) => 74,
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
    if !file
        .metadata()
        .is_ok_and(|metadata| metadata.is_file() || metadata.is_dir())
    {
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

#[cfg(not(unix))]
fn main() {
    eprintln!("aac-http: Unix sockets required; TypeScript fallback remains active");
    std::process::exit(1);
}
