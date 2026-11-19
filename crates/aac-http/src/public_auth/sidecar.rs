//! A private compatibility worker supervised by the native dashboard.
use super::{DashboardEvents, compose::Backend, storage};
use serde::Deserialize;
use std::{
    io,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    process::Stdio,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

pub(super) struct Sidecar {
    child: Child,
    input: Option<ChildStdin>,
    output: Option<BufReader<ChildStdout>>,
    pub backend: Backend,
    pub cache: PathBuf,
}

#[derive(Deserialize)]
struct Ready {
    sidecar: u8,
    port: u16,
    cache: PathBuf,
}

fn invalid() -> io::Error {
    io::Error::other("the private dashboard sidecar did not become ready")
}

pub(super) fn package_root() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors()
        .skip(1)
        .find(|root| root.join("package.json").is_file() && root.join("dist").is_dir())
        .map(Path::to_path_buf)
}

impl Sidecar {
    pub async fn start(root: &Path, public: SocketAddr) -> io::Result<Option<Self>> {
        let explicit = std::env::var_os("AAC_TS_SIDECAR");
        if explicit.as_deref() == Some(std::ffi::OsStr::new("off")) {
            return Ok(None);
        }
        let script = explicit
            .map(PathBuf::from)
            .or_else(|| package_root().map(|path| path.join("dist/web-server/sidecar.js")))
            .filter(|file| file.is_absolute() && file.is_file())
            .ok_or_else(|| io::Error::other("the packaged dashboard sidecar is missing"))?;
        let proof = storage::hex(&storage::random::<32>().map_err(|_| invalid())?);
        let exe = std::env::current_exe()?;
        let binary = exe.with_file_name(if cfg!(windows) {
            "aac-http.exe"
        } else {
            "aac-http"
        });
        let mut child = Command::new("node")
            .arg(script)
            .env("CCS_DIR", root)
            .env("AAC_RUST_CUT_WORKER", "1")
            .env("AAC_RUST_COMPOSE", "1")
            .env("AAC_RUST_SERVICE", "on")
            .env("AAC_RUST_SERVICE_BINARY", binary)
            .env("AAC_NO_LOCAL_RUST_BUILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()?;
        let mut input = child.stdin.take().ok_or_else(invalid)?;
        input.write_all(format!("{proof}\n").as_bytes()).await?;
        let mut output = BufReader::new(child.stdout.take().ok_or_else(invalid)?);
        let mut line = Vec::new();
        let ready = tokio::time::timeout(Duration::from_secs(15), async {
            for _ in 0..64 {
                line.clear();
                let read = (&mut output)
                    .take(65537)
                    .read_until(b'\n', &mut line)
                    .await?;
                if read == 0 || read > 65536 {
                    return Err(invalid());
                }
                if let Ok(ready) = serde_json::from_slice::<Ready>(&line) {
                    return Ok(ready);
                }
            }
            Err(invalid())
        })
        .await
        .map_err(|_| invalid())??;
        let address = SocketAddr::from((Ipv4Addr::LOCALHOST, ready.port));
        if ready.sidecar != 1 || address == public || !ready.cache.is_absolute() {
            return Err(invalid());
        }
        Ok(Some(Self {
            child,
            input: Some(input),
            output: Some(output),
            backend: Backend::new(address, &proof)?,
            cache: ready.cache,
        }))
    }

    pub fn attach(&mut self, events: DashboardEvents) -> Option<tokio::task::JoinHandle<()>> {
        let output = self.output.take()?;
        Some(tokio::spawn(async move {
            if events.read_feed(output).await.is_err() {
                tracing::warn!("dashboard.sidecar_event_feed.closed");
            }
        }))
    }

    pub async fn stop(mut self) {
        drop(self.input.take());
        if tokio::time::timeout(Duration::from_secs(4), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}
