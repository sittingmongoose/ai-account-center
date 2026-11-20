//! A private compatibility worker supervised by the native dashboard.
use super::{DashboardEvents, collection_feed::CollectionFeed, compose::Backend, storage};
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
    pub collection: Option<CollectionFeed>,
    _cache_directory: tempfile::TempDir,
    #[cfg(unix)]
    _collection_lease: Option<aac_files::OwnedStore>,
}

#[derive(Deserialize)]
struct Ready {
    sidecar: u8,
    port: u16,
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
    pub async fn start(
        root: &Path,
        public: SocketAddr,
        native_collection: bool,
    ) -> io::Result<Option<Self>> {
        let explicit = std::env::var_os("AAC_TS_SIDECAR");
        if explicit.as_deref() == Some(std::ffi::OsStr::new("off")) {
            return Ok(None);
        }
        let script = explicit
            .map(PathBuf::from)
            .or_else(|| package_root().map(|path| path.join("dist/web-server/sidecar.js")))
            .filter(|file| file.is_absolute() && file.is_file())
            .ok_or_else(|| io::Error::other("the packaged dashboard sidecar is missing"))?;
        let worker_collection = !native_collection;
        // Reserve the same root lease before enabling the worker sampler.
        #[cfg(unix)]
        let collection_lease = worker_collection
            .then(|| aac_files::OwnedStore::claim_collection(root).map_err(io::Error::other))
            .transpose()?;
        let proof = storage::hex(&storage::random::<32>().map_err(|_| invalid())?);
        let collection = if native_collection {
            Some(CollectionFeed::start(&proof).await?)
        } else {
            None
        };
        let mut cache_builder = tempfile::Builder::new();
        cache_builder.prefix("aac-sidecar-");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            cache_builder.permissions(std::fs::Permissions::from_mode(0o700));
        }
        let cache_directory = cache_builder.tempdir()?;
        let cache = cache_directory.path().join("hub.json");
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
            .env("AAC_CUT_HUB_DIRECTORY", cache_directory.path())
            .env("AAC_CUT_PUBLIC_OWNERS", "1")
            .env("AAC_CUT_PUBLIC_LIFECYCLE", "1")
            // Public boot already selected collection. The sidecar may attach to a
            // service, but cannot launch a new sampler if its own store attach fails.
            .env("AAC_CUT_PRIVATE_LIFECYCLE", "0")
            .env(
                "AAC_CUT_PUBLIC_COLLECTION",
                if native_collection { "1" } else { "0" },
            )
            .env(
                "AAC_CUT_WORKER_COLLECTION",
                if worker_collection { "1" } else { "0" },
            )
            .env("AAC_RUST_COMPOSE", "1")
            .env("AAC_RUST_SERVICE", "on")
            .env("AAC_RUST_SERVICE_BINARY", binary)
            .env("AAC_NO_LOCAL_RUST_BUILD", "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .env(
                "AAC_CUT_COLLECTION_FEED",
                collection
                    .as_ref()
                    .map(CollectionFeed::address)
                    .unwrap_or_default(),
            )
            .env("AAC_CUT_COLLECTION_PROOF", &proof)
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
        if ready.sidecar != 1 || address == public || !cache.is_absolute() {
            return Err(invalid());
        }
        Ok(Some(Self {
            child,
            input: Some(input),
            output: Some(output),
            backend: Backend::new(address, &proof, worker_collection)?,
            cache,
            collection,
            _cache_directory: cache_directory,
            #[cfg(unix)]
            _collection_lease: collection_lease,
        }))
    }

    pub fn attach(
        &mut self,
        events: DashboardEvents,
        shutdown: axum_server::Handle<SocketAddr>,
        lost: std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let output = self.output.take()?;
        Some(tokio::spawn(async move {
            if events.read_feed(output).await.is_err() {
                tracing::warn!("dashboard.sidecar_event_feed.closed");
            }
            lost.store(true, std::sync::atomic::Ordering::Relaxed);
            shutdown.graceful_shutdown(Some(Duration::from_secs(3)));
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
