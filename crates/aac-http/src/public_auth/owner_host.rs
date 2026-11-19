//! The public front door's one owner host.
//!
//! A worker-removal route group needs the owners that already exist (readers, store, journal,
//! feed producers). There is exactly one owner per fact:
//!
//! - When a private service is running, its descriptor (`run/aac-http.json`) is the endpoint and
//!   every request goes over that socket. Nothing is started here.
//! - When no endpoint exists (the worker-less cut boot, and Windows once the service compiles
//!   there), this host starts the *same* private service in-process with the boot the
//!   supervisor resolves, and writes the same descriptor. The service's own jobs, store and
//!   readers run once; no group copies a collector, a cache or a journal.
//!
//! A group registers with [`OwnerHost::request`] (and its own admission/encoding), never with a
//! second copy of an owner. `AAC_RUST_CUT_NATIVE_ROUTES=off` makes the host absent, so the cut's
//! shipped worker keeps answering.

#[cfg(unix)]
use super::service_link::{ServiceLink, ServiceReply};
use super::{HubCache, native_owners};
use crate::public_auth::DashboardEvents;
#[cfg(unix)]
use serde_json::Value as Json;
#[cfg(unix)]
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Set to `on` to let this boot answer its groups itself (the cutover switch).
pub(super) const SWITCH: &str = "AAC_RUST_CUT_NATIVE_ROUTES";

pub(super) struct OwnerHost {
    root: PathBuf,
    /// Start the in-process service for a worker-less cut boot (`dashboard` with no backend).
    /// A composition fixture has a backend and its own supervisor: never a second service there.
    embed: bool,
    #[cfg(unix)]
    link: Mutex<Option<ServiceLink>>,
    #[cfg(unix)]
    embedded: tokio::sync::Mutex<Option<Embedded>>,
    /// The in-process public owners this boot started (Accounts collection, Re-check,
    /// App Updates, settings/writes). One set, whichever platform; the groups ask it directly.
    public: Mutex<Option<PublicOwners>>,
    /// Route groups registered on this host, mounted on the public API router before the guard.
    routes: Mutex<Vec<(&'static str, axum::Router)>>,
}

struct PublicOwners {
    native: native_owners::NativeOwners,
    jobs: native_owners::Jobs,
}

#[cfg(unix)]
struct Embedded {
    stop: tokio::sync::oneshot::Sender<()>,
    task: tokio::task::JoinHandle<()>,
    descriptor: PathBuf,
}

/// Whether this boot answers its groups itself.
pub(super) fn enabled() -> bool {
    std::env::var(SWITCH).as_deref() == Ok("on")
}

#[cfg(unix)]
fn random_hex<const N: usize>() -> Option<String> {
    super::storage::random::<N>()
        .ok()
        .map(|bytes| super::storage::hex(&bytes))
}

impl OwnerHost {
    pub(super) fn new(root: PathBuf, embed: bool) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            root,
            embed,
            #[cfg(unix)]
            link: Mutex::new(None),
            #[cfg(unix)]
            embedded: tokio::sync::Mutex::new(None),
            public: Mutex::new(None),
            routes: Mutex::new(Vec::new()),
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Register one named in-process route group. Its router is merged into the public API
    /// router before the dashboard guard, so its handlers see `Extension<DashboardAccess>`
    /// (the verified principal and the origin/transport facts) exactly as `edges` does; the
    /// group keeps its own admission sentences and encoding.
    #[allow(dead_code)] // registered by the groups that land after the host
    pub(super) fn register(&self, name: &'static str, routes: axum::Router) {
        let mut held = self.routes.lock().unwrap_or_else(|p| p.into_inner());
        if !held.iter().any(|(held, _)| *held == name) {
            held.push((name, routes));
        }
    }

    /// The registered groups, merged onto the API router.
    pub(super) fn mount(&self, router: axum::Router) -> axum::Router {
        let held = self.routes.lock().unwrap_or_else(|p| p.into_inner());
        let mut router = router;
        for (_, routes) in held.iter() {
            router = router.merge(routes.clone());
        }
        router
    }

    /// Start the platform's public owners (the same `NativeOwners` the direct call site used).
    /// The host is then the one place that owns them and their jobs.
    pub(super) fn start_public(
        &self,
        groups: native_owners::Groups,
        hub: HubCache,
        events: DashboardEvents,
        worker_directory: Option<PathBuf>,
    ) -> std::io::Result<()> {
        let (native, jobs) = native_owners::NativeOwners::start(
            self.root.clone(),
            groups,
            hub,
            events,
            worker_directory,
        )?;
        *self.public.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(PublicOwners { native, jobs });
        Ok(())
    }

    /// The public owners this boot started, for the routes that answer from them.
    pub(super) fn public_owners(&self) -> Option<native_owners::NativeOwners> {
        self.public
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
            .map(|owners| owners.native.clone())
    }

    /// Stop the public owners' jobs (their collectors) at shutdown.
    pub(super) async fn stop_public(&self) {
        let owners = self.public.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(owners) = owners {
            owners.jobs.stop().await;
        }
    }

    #[cfg(unix)]
    fn cached(&self) -> Option<ServiceLink> {
        self.link.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }

    #[cfg(unix)]
    fn remember(&self, link: ServiceLink) -> ServiceLink {
        *self.link.lock().unwrap_or_else(|p| p.into_inner()) = Some(link.clone());
        link
    }

    /// The service's endpoint: a running one's descriptor, else the in-process service this
    /// host starts for a worker-less boot. Windows has no private service yet: no link there.
    #[cfg(unix)]
    async fn link(&self) -> Option<ServiceLink> {
        if let Some(link) = self.cached() {
            return Some(link);
        }
        if let Some(link) = ServiceLink::load(&self.root) {
            return Some(self.remember(link));
        }
        if !self.embed {
            // A supervisor-owned service may still be starting (the public listener binds
            // first). Wait briefly for its descriptor instead of declining the first request.
            for _ in 0..30 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if let Some(link) = ServiceLink::load(&self.root) {
                    return Some(self.remember(link));
                }
            }
            return None;
        }
        let mut embedded = self.embedded.lock().await;
        if embedded.is_none() {
            *embedded = self.start_embedded().await;
        }
        let link = ServiceLink::load(&self.root)?;
        Some(self.remember(link))
    }

    /// Whether a private service (running or started here) answers this boot.
    pub(super) async fn ready(&self) -> bool {
        #[cfg(unix)]
        {
            self.link().await.is_some()
        }
        #[cfg(not(unix))]
        {
            false
        }
    }

    /// One request a group already admitted: the service's reply, or `None` when no owner is
    /// reachable (the group answers its own decline). A failed hop drops the cached endpoint so
    /// the next request picks up a restarted service's descriptor.
    #[cfg(unix)]
    pub(super) async fn request(
        &self,
        method: &str,
        uri: &str,
        principal: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Option<ServiceReply> {
        let link = self.link().await?;
        let reply = link.request(method, uri, principal, headers, body).await;
        if reply.is_none() {
            *self.link.lock().unwrap_or_else(|p| p.into_inner()) = None;
        }
        reply
    }

    /// Start the private service before the first request, so a cold boot never spends the
    /// first request's budget on the service's own start.
    pub(super) async fn warm(&self) {
        #[cfg(unix)]
        {
            let _ = self.link().await;
        }
    }

    /// Stop the service this boot started, if any (a running one belongs to its own supervisor).
    pub(super) async fn stop(&self) {
        #[cfg(unix)]
        let embedded = self.embedded.lock().await.take();
        #[cfg(unix)]
        if let Some(embedded) = embedded {
            let _ = embedded.stop.send(());
            let _ = embedded.task.await;
            let _ = std::fs::remove_file(&embedded.descriptor);
        }
    }

    /// The boot the supervisor resolves for a plain cut: the store under `<ccs>/store` and the
    /// owned readers on the configured share roots. Only an explicitly configured boot starts
    /// here: without named share roots this would read the real share.
    #[cfg(unix)]
    fn boot_config(&self) -> Option<Json> {
        if std::env::var("AAC_RUST_STORE").as_deref() == Ok("off") {
            return None;
        }
        let on = |env: &str| std::env::var(env).as_deref() != Ok("off");
        let reports = std::env::var("AAC_AGENT_REPORTS_DIR").ok()?;
        let registry = std::env::var("AAC_PM_MAIL_REGISTRY_DIR").ok()?;
        let ccs = self.root.canonicalize().ok()?;
        let boot_id = random_hex::<12>()?;
        let secret = random_hex::<32>()?;
        let socket = ccs
            .join("run")
            .join(format!("aac-http-{boot_id}"))
            .join("http.sock");
        Some(json!({
            "ccsDir": ccs,
            "socketPath": socket,
            "secret": secret,
            "bootId": boot_id,
            "store": {
                "dir": ccs.join("store"),
                "retentionDays": 0,
                "bindingIdentity": aac_store::build_identity(),
                "snapshotDirectory": null,
                "shareBackupDir": null,
                "shareBackupOff": true,
            },
            "owned": {
                "reportsRoot": aac_status::share::agent_reports_root(Some(&reports)),
                "registryDir": aac_status::share::pm_mail_registry_dir(Some(&registry)),
                "status": on("AAC_RUST_GROUP_STATUS"),
                "scoreboard": on("AAC_RUST_GROUP_SCOREBOARD"),
            },
        }))
    }

    #[cfg(unix)]
    async fn start_embedded(&self) -> Option<Embedded> {
        let config = self.boot_config()?;
        let socket = PathBuf::from(config.get("socketPath")?.as_str()?);
        let ccs = PathBuf::from(config.get("ccsDir")?.as_str()?);
        let secret = config.get("secret")?.as_str()?.to_owned();
        let boot_id = config.get("bootId")?.as_str()?.to_owned();
        let boot = crate::unix::Boot::from_json(config.to_string().as_bytes()).ok()?;
        let descriptor = ccs.join("run").join("aac-http.json");
        let (stop, stopped) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            if let Err(error) = crate::unix::serve(boot, async {
                let _ = stopped.await;
            })
            .await
            {
                tracing::warn!(%error, "the in-process private service stopped");
            }
        });
        // The service binds before it serves; the socket appearing is its readiness signal.
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
        while !socket.exists() {
            if task.is_finished() || tokio::time::Instant::now() >= deadline {
                task.abort();
                return None;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
        if write_descriptor(&descriptor, &ccs, &socket, &secret, &boot_id).is_err() {
            let _ = stop.send(());
            let _ = task.await;
            return None;
        }
        Some(Embedded {
            stop,
            task,
            descriptor,
        })
    }
}

/// The supervisor's descriptor (`run/aac-http.json`), written the way the front door reads it:
/// private 0600, replaced atomically, holding the boot's own id and secret.
#[cfg(unix)]
pub(super) fn write_descriptor(
    path: &Path,
    ccs: &Path,
    socket: &Path,
    secret: &str,
    boot_id: &str,
) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let body = json!({
        "ccsDir": ccs,
        "socketPath": socket,
        "secret": secret,
        "bootId": boot_id,
    })
    .to_string();
    let temporary = path.with_extension("json.tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)?;
    file.write_all(body.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, path)
}

#[cfg(all(test, unix))]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::Value;

    /// The descriptor round-trips through the same checks the link applies.
    #[test]
    fn the_written_descriptor_is_the_one_the_link_reads() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("ccs");
        let run = root.join("run");
        let boot = run.join(format!("aac-http-{}", "c".repeat(24)));
        for path in [&root, &run, &boot] {
            std::fs::create_dir_all(path).unwrap();
            std::fs::set_permissions(
                path,
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .unwrap();
        }
        let socket = boot.join("http.sock");
        std::fs::write(&socket, b"").unwrap();
        std::fs::set_permissions(
            &socket,
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o600),
        )
        .unwrap();
        let ccs = root.canonicalize().unwrap();
        let descriptor = run.join("aac-http.json");
        write_descriptor(&descriptor, &ccs, &socket, &"b".repeat(64), &"c".repeat(24)).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&descriptor).unwrap()).unwrap();
        assert_eq!(written["ccsDir"], ccs.to_str().unwrap());
        assert_eq!(written["socketPath"], socket.to_str().unwrap());
        let mode = std::os::unix::fs::PermissionsExt::mode(
            &std::fs::metadata(&descriptor).unwrap().permissions(),
        );
        assert_eq!(mode & 0o777, 0o600);
        // The socket here is a regular file, so the link's socket check refuses it: the test
        // proves the descriptor, not a live link.
        assert!(ServiceLink::load(&ccs).is_none());
    }
}
