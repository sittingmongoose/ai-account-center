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
//! second copy of an owner. The cut always uses this host for migrated routes.

#[cfg(unix)]
use super::service_link::ServiceLink;
use super::{HubCache, native_owners};
use crate::public_auth::DashboardEvents;
use axum::http::HeaderMap;
#[cfg(unix)]
use serde_json::Value as Json;
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use std::time::Duration;
use std::{
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Set to `on` to let this boot answer its groups itself (the cutover switch).
pub(super) const SWITCH: &str = "AAC_RUST_CUT_NATIVE_ROUTES";

/// One service reply, as it crossed the socket (or an in-process owner's answer).
pub(super) struct ServiceReply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

pub(super) struct OwnerHost {
    root: PathBuf,
    /// Start the in-process service for a worker-less cut boot (`dashboard` with no backend).
    /// A composition fixture has a backend and its own supervisor: never a second service there.
    embed: bool,
    #[cfg(unix)]
    link: Mutex<Option<ServiceLink>>,
    #[cfg(all(test, unix))]
    descriptor_lookups: std::sync::atomic::AtomicUsize,
    #[cfg(unix)]
    embedded: tokio::sync::Mutex<Option<Embedded>>,
    /// The in-process public owners this boot started (Accounts collection, Re-check,
    /// App Updates, settings/writes). One set, whichever platform; the groups ask it directly.
    public: Mutex<Option<PublicOwners>>,
    usage_samples: Mutex<crate::status_produce::UsageSamples>,
    /// Route groups registered on this host, mounted on the public API router before the guard.
    routes: Mutex<Vec<(&'static str, axum::Router)>>,
    /// The in-process owners for a platform without the private service (Windows).
    #[cfg(not(unix))]
    local: tokio::sync::Mutex<Option<std::sync::Arc<crate::owners::Owners>>>,
    #[cfg(not(unix))]
    local_stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    #[cfg(not(unix))]
    analytics_live: std::sync::Arc<crate::analytics_live::Live>,
    #[cfg(not(unix))]
    analytics_reads: crate::limits::Limits,
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
            #[cfg(all(test, unix))]
            descriptor_lookups: std::sync::atomic::AtomicUsize::new(0),
            #[cfg(unix)]
            embedded: tokio::sync::Mutex::new(None),
            public: Mutex::new(None),
            usage_samples: Mutex::default(),
            routes: Mutex::new(Vec::new()),
            #[cfg(not(unix))]
            local: tokio::sync::Mutex::new(None),
            #[cfg(not(unix))]
            local_stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            #[cfg(not(unix))]
            analytics_live: std::sync::Arc::default(),
            #[cfg(not(unix))]
            analytics_reads: crate::limits::Limits::new(),
        })
    }

    pub(super) fn root(&self) -> &Path {
        &self.root
    }

    /// Register one named in-process route group. Its router is merged into the public API
    /// router before the dashboard guard, so its handlers see `Extension<DashboardAccess>`
    /// (the verified principal and the origin/transport facts) exactly as `edges` does; the
    /// group keeps its own admission sentences and encoding.
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

    /// The in-process owners this platform runs instead of a private service. Windows has no
    /// private service: its Status/Scoreboard/store/answers owners run here, once, with their
    /// jobs. Unix answers through the service (or its in-process start), so this is `None`.
    pub(super) async fn local(&self) -> Option<std::sync::Arc<crate::owners::Owners>> {
        #[cfg(unix)]
        {
            None
        }
        #[cfg(not(unix))]
        {
            let mut held = self.local.lock().await;
            if self.local_stop.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            if let Some(owners) = held.as_ref() {
                return Some(owners.clone());
            }
            let ccs = self.root.canonicalize().ok()?;
            let (reports, registry_dir) = share_roots(
                std::env::var("AAC_AGENT_REPORTS_DIR").ok().as_deref(),
                std::env::var("AAC_PM_MAIL_REGISTRY_DIR").ok().as_deref(),
            );
            let config = crate::owners::Config {
                ccs_dir: ccs.clone(),
                store: crate::store_host::StoreBoot::for_public(&ccs),
                owned: Some(crate::owners::OwnedBoot {
                    reports_root: reports.clone(),
                    registry_dir,
                    status: true,
                    scoreboard: true,
                }),
                files: None,
            };
            let owners = std::sync::Arc::new(
                crate::owners::Owners::start(config, crate::feed::Feeds::new()).ok()?,
            );
            // The hosted store opens once, before any route reads it (the service does this in
            // its boot); a request must never wait behind an unopened Engine.
            if let Some(store) = owners.store.clone() {
                let opener = store.clone();
                let _ = tokio::task::spawn_blocking(move || opener.open_at_boot()).await;
                tokio::spawn(store.jobs(self.local_stop.clone()));
            }
            crate::status_owned::initialize_reports(reports).await;
            if let Some(owner) = &owners.scoreboard_owned {
                // The first request sees a catalog and runs from the same prepared generation.
                owner.seed().await;
                owner.prepare_local().await;
            }
            if let Some(owner) = &owners.status_owned {
                let owner = owner.clone();
                let produce = owners.status_produce.clone();
                let stop = self.local_stop.clone();
                tokio::spawn(async move { crate::status_owned::jobs(owner, produce, stop).await });
            }
            if let Some(produce) = owners.status_produce.clone() {
                let stop = self.local_stop.clone();
                tokio::spawn(produce.jobs(stop));
            }
            if let Some(owner) = &owners.scoreboard_owned {
                let catalog = owner.clone();
                let stop = self.local_stop.clone();
                tokio::spawn(async move {
                    crate::scoreboard_owned::catalog_jobs(catalog, stop, false).await
                });
                let ingest = owner.clone();
                let stop = self.local_stop.clone();
                tokio::spawn(async move {
                    crate::scoreboard_owned::ingest_jobs(ingest, stop, false).await
                });
            }
            if let Some(answers) = &owners.answers_owned {
                let answers = answers.clone();
                let stop = self.local_stop.clone();
                tokio::spawn(async move { crate::answers_owned::jobs(answers, stop).await });
            }
            *held = Some(owners.clone());
            Some(owners)
        }
    }

    /// Start the platform's public owners (the same `NativeOwners` the direct call site used).
    /// The host is then the one place that owns them and their jobs.
    pub(super) fn start_public(
        self: &std::sync::Arc<Self>,
        groups: native_owners::Groups,
        hub: HubCache,
        events: DashboardEvents,
    ) -> std::io::Result<()> {
        let (native, jobs) = native_owners::NativeOwners::start(
            self.root.clone(),
            groups,
            hub,
            events,
            std::sync::Arc::downgrade(self),
        )?;
        *self.public.lock().unwrap_or_else(|p| p.into_inner()) =
            Some(PublicOwners { native, jobs });
        Ok(())
    }

    pub(super) async fn record_status_sample(
        &self,
        cache: &crate::account_usage::AccountUsageCache,
    ) {
        // The full historical replay exercises the account collector separately
        // while its Status golden represents a page with no account sample.
        #[cfg(feature = "parity-fixtures")]
        if std::env::var("AAC_RUST_CUT_FIXTURE_NO_STATUS_USAGE").as_deref() == Ok("1") {
            return;
        }
        let usage = self
            .usage_samples
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .project(&self.root, cache);
        let Some(usage) = usage else {
            return;
        };
        #[cfg(not(unix))]
        if let Some(produce) = self
            .local()
            .await
            .and_then(|owner| owner.status_produce.clone())
        {
            produce.set_usage(Some(usage));
        }
        #[cfg(unix)]
        if let Ok(bytes) = serde_json::to_vec(&usage) {
            let principal = r#"{"kind":"session","id":"front-door","role":"service"}"#;
            self.request(
                "PUT",
                "/internal/status/usage",
                principal,
                &[("content-type", "application/json")],
                Some(&bytes),
            )
            .await;
        }
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
    fn published_link(&self) -> Option<ServiceLink> {
        let mut held = self.link.lock().unwrap_or_else(|p| p.into_inner());
        if held.is_none() {
            #[cfg(test)]
            self.descriptor_lookups
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            *held = ServiceLink::load(&self.root);
        }
        held.clone()
    }

    #[cfg(unix)]
    fn forget_link(&self, failed: &ServiceLink) {
        let mut held = self.link.lock().unwrap_or_else(|p| p.into_inner());
        if held.as_ref() == Some(failed) {
            *held = None;
        }
    }

    /// The service's endpoint: a running one's descriptor, else the in-process service this
    /// host starts for a worker-less boot. Windows has no private service yet: no link there.
    #[cfg(unix)]
    async fn link(&self) -> Option<ServiceLink> {
        if let Some(link) = self.published_link() {
            return Some(link);
        }
        if !self.embed {
            // A supervisor-owned service may still be starting (the public listener binds
            // first). Wait briefly for its descriptor instead of declining the first request.
            for _ in 0..100 {
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                if let Some(link) = self.published_link() {
                    return Some(link);
                }
            }
            return None;
        }
        let mut embedded = self.embedded.lock().await;
        if embedded.as_ref().is_none_or(|boot| boot.task.is_finished()) {
            *embedded = self.start_embedded().await;
        }
        self.published_link()
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
    pub(super) async fn request(
        &self,
        method: &str,
        uri: &str,
        principal: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Option<ServiceReply> {
        #[cfg(unix)]
        {
            let link = self.link().await?;
            let reply = link.request(method, uri, principal, headers, body).await;
            if reply.is_none() {
                self.forget_link(&link);
            }
            reply
        }
        #[cfg(not(unix))]
        {
            let _ = (method, uri, principal, headers, body);
            None
        }
    }

    pub(super) async fn analytics(&self, uri: &str, principal: &str) -> Option<(u16, Vec<u8>)> {
        #[cfg(unix)]
        {
            self.request("GET", uri, principal, &[], None)
                .await
                .map(|reply| (reply.status, reply.body))
        }
        #[cfg(not(unix))]
        {
            let _ = principal;
            let engine = self.local().await?.store.as_ref()?.analytics_engine()?;
            let query = uri.split_once('?').map(|(_, query)| query.to_owned());
            let root = self.root.clone();
            let live = self.analytics_live.clone();
            let cache = self.public_owners().and_then(|owner| owner.usage_cache());
            self.analytics_reads
                .blocking(crate::table::Pool::Reads, move || {
                    let now = super::now();
                    let snapshot =
                        cache.and_then(|cache| crate::analytics_route::snapshot(&cache, now));
                    let answer = crate::analytics_route::compose(
                        engine,
                        &root,
                        snapshot.as_ref(),
                        &live,
                        query.as_deref(),
                        now,
                    );
                    crate::analytics_route::encode(answer, query.as_deref())
                })
                .await
                .ok()
        }
    }

    #[cfg(unix)]
    async fn status_event_link(&self) -> Option<ServiceLink> {
        if self.embed {
            self.link().await
        } else {
            self.published_link()
        }
    }

    /// Forward the Status producer's hints without another producer or a worker subscriber.
    pub(super) async fn status_events(&self, events: DashboardEvents) {
        #[cfg(unix)]
        {
            let mut delay = Duration::from_secs(1);
            loop {
                let started = tokio::time::Instant::now();
                if let Some(link) = self.status_event_link().await {
                    link.follow_status(&events).await;
                    self.forget_link(&link);
                }
                if started.elapsed() >= Duration::from_secs(30) {
                    delay = Duration::from_secs(1);
                }
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(30));
            }
        }
        #[cfg(not(unix))]
        if let Some(owner) = self.local().await {
            let _ = events.read_status_feed(owner.feeds.stream()).await;
        }
    }

    #[cfg(windows)]
    pub(super) async fn start_analytics_jobs(&self) {
        let Some((store, (agents, cache))) = self
            .local()
            .await
            .and_then(|owners| owners.store.clone())
            .zip(self.public_owners().map(|owners| owners.profile_inputs()))
        else {
            return;
        };
        tokio::spawn(crate::analytics_writers::analytics_jobs(
            store,
            move |now| crate::analytics_route::snapshot(cache.as_ref()?, now),
            agents,
            self.root.clone(),
            self.analytics_live.clone(),
            self.local_stop.clone(),
        ));
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
        #[cfg(not(unix))]
        {
            self.local_stop
                .store(true, std::sync::atomic::Ordering::Release);
            let local = self.local.lock().await.take();
            if let Some(store) = local.and_then(|owners| owners.store.clone()) {
                let _ = tokio::task::spawn_blocking(move || store.close_engine()).await;
            }
        }
    }

    /// The boot the supervisor resolves for a plain cut: the store under `<ccs>/store` and the
    /// owned readers on the share roots. The readers resolve their own defaults when no root is
    /// named (the worker's rule).
    #[cfg(unix)]
    fn boot_config(&self) -> Option<Json> {
        let reports = std::env::var("AAC_AGENT_REPORTS_DIR").ok();
        let registry = std::env::var("AAC_PM_MAIL_REGISTRY_DIR").ok();
        self.boot_config_from(reports.as_deref(), registry.as_deref())
    }

    /// M7: the store opens from its own configuration. Two optional share-directory overrides
    /// never decide whether a private local root has a store; each reader takes its default.
    #[cfg(unix)]
    fn boot_config_from(&self, reports: Option<&str>, registry: Option<&str>) -> Option<Json> {
        let (reports_root, registry_dir) = share_roots(reports, registry);
        let ccs = self.root.canonicalize().ok()?;
        let store = crate::store_host::StoreBoot::for_public(&ccs);
        let boot_id = random_hex::<12>()?;
        let secret = random_hex::<32>()?;
        let socket = socket_layout(&ccs, &boot_id)?;
        Some(json!({
            "ccsDir": ccs,
            "socketPath": socket,
            "secret": secret,
            "bootId": boot_id,
            "embeddedDashboard": true,
            "store": store,
            "owned": {
                "reportsRoot": reports_root.to_string_lossy(),
                "registryDir": registry_dir.to_string_lossy(),
                "status": true,
                "scoreboard": true,
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
        // Prepare this boot's socket directory exactly once, with exclusive leaf creation.
        if !prepare_run_dir(&ccs, &socket) {
            return None;
        }
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

/// The supervisor's socket layout, including a private short path on deeply nested homes.
#[cfg(unix)]
#[allow(unsafe_code)]
fn socket_layout(ccs: &Path, boot_id: &str) -> Option<PathBuf> {
    let boot = format!("aac-http-{boot_id}");
    let fits = |path: &Path| path.as_os_str().as_encoded_bytes().len() <= 100;
    let classic = ccs.join("run").join(&boot).join("http.sock");
    if fits(&classic) {
        return Some(classic);
    }
    let hash = crate::unix::short_name(ccs);
    for name in ["AAC_RUST_RUN_DIR", "XDG_RUNTIME_DIR"] {
        if let Some(root) = std::env::var_os(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
        {
            let path = root
                .join(format!("aac-{hash}"))
                .join(&boot)
                .join("http.sock");
            if fits(&path) {
                return Some(path);
            }
        }
    }
    // SAFETY: geteuid has no preconditions and touches no memory.
    let uid = unsafe { libc::geteuid() };
    Some(
        Path::new("/tmp")
            .join(format!("aac-{uid}"))
            .join(hash)
            .join(boot)
            .join("http.sock"),
    )
}

/// Check every directory controlled by the service and create its boot leaf exclusively.
#[cfg(unix)]
#[allow(unsafe_code)]
fn prepare_run_dir(ccs: &Path, socket: &Path) -> bool {
    use std::os::unix::fs::DirBuilderExt;
    // SAFETY: geteuid has no preconditions and touches no memory.
    let euid = unsafe { libc::geteuid() };
    let run = ccs.join("run");
    let Some(leaf) = socket.parent() else {
        return false;
    };
    let Some(parent) = leaf.parent() else {
        return false;
    };
    let mut folders = vec![run.as_path()];
    if parent != run {
        if parent
            .file_name()
            .is_some_and(|name| name == crate::unix::short_name(ccs).as_str())
            && let Some(owner) = parent.parent()
        {
            folders.push(owner);
        }
        folders.push(parent);
    }
    for folder in folders {
        let created = std::fs::DirBuilder::new().mode(0o700).create(folder);
        if created.is_err_and(|error| error.kind() != std::io::ErrorKind::AlreadyExists)
            || !std::fs::symlink_metadata(folder).is_ok_and(|meta| owned_0700(&meta, euid))
        {
            return false;
        }
    }
    std::fs::DirBuilder::new().mode(0o700).create(leaf).is_ok()
}

/// A directory this user owns at exactly `0700`. Other modes are refused, never changed.
#[cfg(unix)]
fn owned_0700(meta: &std::fs::Metadata, euid: u32) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.uid() == euid && meta.mode() & 0o777 == 0o700
}

/// The readers' roots for a boot that names none: each resolves the worker's default (the
/// shared folders) rather than keeping the store from opening (M7).
fn share_roots(reports: Option<&str>, registry: Option<&str>) -> (PathBuf, PathBuf) {
    (
        aac_status::share::agent_reports_root(reports),
        aac_status::share::pm_mail_registry_dir(registry),
    )
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
    #![allow(unsafe_code, clippy::unwrap_used)]
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn status_event_lookup_returns_promptly_when_no_service_publishes() {
        let root = tempfile::tempdir().unwrap();
        let host = OwnerHost::new(root.path().to_path_buf(), false);
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(10),
            host.status_event_link(),
        )
        .await;
        assert!(
            matches!(result, Ok(None)),
            "the event loop must own the retry cadence"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn status_events_back_off_when_no_service_publishes() {
        let root = tempfile::tempdir().unwrap();
        let host = OwnerHost::new(root.path().to_path_buf(), false);
        let events = DashboardEvents(tokio::sync::broadcast::channel(1).0);
        assert!(
            tokio::time::timeout(Duration::from_secs(60), host.status_events(events))
                .await
                .is_err()
        );
        // The descriptor was probed at 0, 1, 3, 7, 15 and 31 seconds.
        assert_eq!(
            host.descriptor_lookups
                .load(std::sync::atomic::Ordering::Relaxed),
            6
        );
    }
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

    /// M7: an empty private local root with neither share override opens the store. The share
    /// directories are optional readers' roots; each resolves the worker's default.
    #[tokio::test]
    async fn an_empty_private_root_opens_its_store_without_share_overrides() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("ccs");
        std::fs::create_dir_all(root.join("run")).unwrap();
        for path in [&root, &root.join("run")] {
            std::fs::set_permissions(
                path,
                <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
            )
            .unwrap();
        }
        let host = OwnerHost::new(root.clone(), false);
        let boot = host
            .boot_config_from(None, None)
            .expect("a boot without share overrides");
        assert_eq!(boot["owned"]["reportsRoot"], "/mnt/Cursor/agent-reports");
        assert_eq!(boot["owned"]["registryDir"], "/mnt/Cursor/Inbox/registry");
        let (reports, registry) = share_roots(None, None);
        assert_eq!(reports, PathBuf::from("/mnt/Cursor/agent-reports"));
        assert_eq!(registry, PathBuf::from("/mnt/Cursor/Inbox/registry"));
        let ccs = root.canonicalize().unwrap();
        let owners = crate::owners::Owners::start(
            crate::owners::Config {
                ccs_dir: ccs.clone(),
                store: Some(crate::store_host::StoreBoot {
                    binding_identity: aac_store::build_identity(),
                    dir: ccs.join("store"),
                    retention_days: 0,
                    cache_bytes: None,
                    activity_retention_days: None,
                    snapshot_directory: None,
                    share_backup_dir: None,
                    share_backup_off: true,
                }),
                owned: Some(crate::owners::OwnedBoot {
                    reports_root: reports,
                    registry_dir: registry,
                    status: true,
                    scoreboard: true,
                }),
                files: None,
            },
            crate::feed::Feeds::new(),
        )
        .unwrap();
        let store = owners
            .store
            .clone()
            .expect("the local store is constructed");
        tokio::task::spawn_blocking(move || store.open_at_boot())
            .await
            .unwrap();
        let status = owners
            .store
            .as_ref()
            .unwrap()
            .status_value()
            .expect("the opened store answers its status");
        assert!(status.get("openedAt").is_some(), "{status}");
    }

    /// M1: a fresh boot makes its run folder and leaf 0700; a leaf that already exists is
    /// never reused (it belongs to another boot).
    #[test]
    fn a_fresh_boot_makes_its_own_run_folder_and_leaf() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        std::fs::create_dir(&ccs).unwrap();
        std::fs::set_permissions(&ccs, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = ccs
            .join("run")
            .join(format!("aac-http-{}", "c".repeat(24)))
            .join("http.sock");
        assert!(prepare_run_dir(&ccs, &socket));
        let run = std::fs::symlink_metadata(ccs.join("run")).unwrap();
        let leaf = std::fs::symlink_metadata(socket.parent().unwrap()).unwrap();
        for meta in [&run, &leaf] {
            assert!(meta.is_dir());
            assert_eq!(meta.mode() & 0o777, 0o700);
            // SAFETY: geteuid has no preconditions.
            assert_eq!(meta.uid(), unsafe { libc::geteuid() });
        }
        assert!(
            !prepare_run_dir(&ccs, &socket),
            "an existing leaf is refused"
        );
    }

    /// M1: a `run` symlink is refused before anything is created or changed at its target.
    #[test]
    fn a_run_symlink_is_refused_and_its_target_is_untouched() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let target = scratch.path().join("target");
        for path in [&ccs, &target] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&target, ccs.join("run")).unwrap();
        let socket = ccs
            .join("run")
            .join(format!("aac-http-{}", "c".repeat(24)))
            .join("http.sock");
        assert!(!prepare_run_dir(&ccs, &socket));
        let meta = std::fs::symlink_metadata(&target).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
        assert!(
            std::fs::symlink_metadata(ccs.join("run"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    /// M1: a `run` regular file is refused and left exactly as it was.
    #[test]
    fn a_run_file_is_refused_and_kept() {
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        std::fs::create_dir(&ccs).unwrap();
        std::fs::write(ccs.join("run"), b"not a folder").unwrap();
        let socket = ccs
            .join("run")
            .join(format!("aac-http-{}", "c".repeat(24)))
            .join("http.sock");
        assert!(!prepare_run_dir(&ccs, &socket));
        assert_eq!(std::fs::read(ccs.join("run")).unwrap(), b"not a folder");
    }

    /// M1: an existing run folder this boot did not make 0700 is refused, never chmodded.
    #[test]
    fn an_existing_run_folder_is_never_chmodded() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        std::fs::create_dir(&ccs).unwrap();
        std::fs::create_dir(ccs.join("run")).unwrap();
        std::fs::set_permissions(ccs.join("run"), std::fs::Permissions::from_mode(0o755)).unwrap();
        let socket = ccs
            .join("run")
            .join(format!("aac-http-{}", "c".repeat(24)))
            .join("http.sock");
        assert!(!prepare_run_dir(&ccs, &socket));
        let meta = std::fs::symlink_metadata(ccs.join("run")).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert!(!socket.parent().unwrap().exists());
    }

    /// M1: the owner rule is exact: our uid at 0700, nothing else.
    #[test]
    fn only_this_users_own_0700_folder_is_usable() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let folder = scratch.path().join("run");
        std::fs::create_dir(&folder).unwrap();
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700)).unwrap();
        let meta = std::fs::symlink_metadata(&folder).unwrap();
        // SAFETY: geteuid has no preconditions.
        let euid = unsafe { libc::geteuid() };
        assert!(owned_0700(&meta, euid));
        assert!(!owned_0700(&meta, euid.wrapping_add(1)));
        std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o750)).unwrap();
        let meta = std::fs::symlink_metadata(&folder).unwrap();
        assert!(!owned_0700(&meta, euid));
    }

    /// M1, through the boot itself: a `run` symlink refuses to serve and its target is
    /// untouched (the pre-fix boot created a leaf there and changed the target to 0700).
    #[tokio::test]
    async fn the_embedded_boot_refuses_a_run_symlink_without_touching_its_target() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = tempfile::tempdir().unwrap();
        let ccs = scratch.path().join("ccs");
        let target = scratch.path().join("target");
        for path in [&ccs, &target] {
            std::fs::create_dir(path).unwrap();
        }
        std::fs::set_permissions(&ccs, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink(&target, ccs.join("run")).unwrap();
        let host = OwnerHost::new(ccs.clone(), true);
        assert!(
            host.start_embedded().await.is_none(),
            "a symlinked run refuses the boot"
        );
        let meta = std::fs::symlink_metadata(&target).unwrap();
        assert_eq!(meta.permissions().mode() & 0o777, 0o755);
        assert_eq!(std::fs::read_dir(&target).unwrap().count(), 0);
    }
}

/// M2: the local owner honours the explicit store switch, as the Unix boot does.
#[cfg(all(test, not(unix)))]
mod windows_tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn the_windows_owner_preserves_store_cache_retention_and_share_backup() {
        const FIXTURE: &str = "AAC_TEST_WINDOWS_STORE_ROOT";
        if let Some(root) = std::env::var_os(FIXTURE).map(PathBuf::from) {
            let host = OwnerHost::new(root.clone(), true);
            let owners = host.local().await.expect("Windows owners");
            let store = owners.store.as_ref().expect("Windows store");
            let boot = store.boot_for_test();
            assert_eq!(boot.cache_bytes, Some(48 * 1024 * 1024));
            assert_eq!(boot.activity_retention_days, Some(400));
            assert_eq!(
                boot.snapshot_directory.as_deref(),
                Some(root.join("snapshot").as_path())
            );
            assert!(!boot.share_backup_off);
            let hour = 3_600_000;
            let sampled = (aac_store::engine::now_ms() as i64 / hour - 48) * hour;
            let at = aac_num::ecma::to_iso_string(sampled).expect("sample timestamp");
            let event = serde_json::json!({
                "now": sampled,
                "accounts": [{
                    "id": "codex:fixture", "provider": "codex", "email": null,
                    "status": "ok", "platform": "windows", "source": "Fixture",
                    "isActive": true, "sampledAt": at, "windows": [{
                        "key": "five_hour", "label": "Five hours", "usedPercent": 27,
                        "windowMinutes": 300
                    }]
                }]
            });
            store
                .call("recordDashboard".into(), event.to_string())
                .await
                .expect("seed Store reading");
            let backup = root.join("share/ai-account-center/store-backups");
            tokio::time::advance(std::time::Duration::from_secs(181)).await;
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                let answer = store
                    .call("hostJobs".into(), "{}".into())
                    .await
                    .expect("job state");
                let state: serde_json::Value = serde_json::from_str(&answer).expect("job JSON");
                if state["jobs"]["passes"]
                    .as_u64()
                    .is_some_and(|passes| passes > 0)
                {
                    assert!(state["jobs"]["lastError"].is_null(), "{state}");
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the owner never ran Store jobs"
                );
                tokio::task::spawn_blocking(|| {
                    std::thread::sleep(std::time::Duration::from_millis(20))
                })
                .await
                .expect("wait for job");
            }
            let query = serde_json::json!({"windowKey": "five_hour", "resolution": "hour",
                "fromMs": sampled, "toMs": sampled + hour - 1});
            let answer = store
                .call("seriesPoints".into(), query.to_string())
                .await
                .expect("hourly rollup");
            let rollup: serde_json::Value = serde_json::from_str(&answer).expect("rollup JSON");
            assert!(
                rollup["series"][0]["points"]
                    .as_array()
                    .is_some_and(|points| !points.is_empty()),
                "{rollup}"
            );
            assert!(
                backup.read_dir().unwrap().next().is_some(),
                "a backup reached the fixture share"
            );
            host.stop().await;
            return;
        }
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path();
        std::fs::create_dir_all(root.join("share/ai-account-center")).unwrap();
        std::fs::create_dir(root.join("snapshot")).unwrap();
        std::fs::write(
            root.join("agent-usage-snapshot.json"),
            serde_json::json!({"version": 1, "directory": root.join("snapshot")}).to_string(),
        )
        .unwrap();
        let child = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "public_auth::owner_host::windows_tests::the_windows_owner_preserves_store_cache_retention_and_share_backup", "--nocapture"])
            .env(FIXTURE, root)
            .env("CCS_HOME", root)
            .env("AAC_AGENT_REPORTS_DIR", root.join("reports"))
            .env("AAC_PM_MAIL_REGISTRY_DIR", root.join("registry"))
            .env("AAC_T3_STATE_DB", root.join("pins.sqlite"))
            .env("AAC_STATUS_DOORBELL", "off")
            .env("AAC_STORE_CACHE_MB", "48")
            .env("AAC_ANALYTICS_RETENTION_DAYS", "400")
            .env_remove("AAC_STORE")
            .env_remove("AAC_RUST_STORE")
            .env_remove("AAC_STORE_SHARE_BACKUP_DIR")
            .output().unwrap();
        assert!(
            child.status.success() && String::from_utf8_lossy(&child.stdout).contains("1 passed"),
            "{}\n{}",
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr)
        );
    }

    #[tokio::test]
    async fn the_local_owner_opens_no_store_with_the_switch_off() {
        let scratch = tempfile::tempdir().unwrap();
        let root = scratch.path().join("ccs");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("config.yaml"), "rustService:\n  store: 'off'\n").unwrap();
        let host = OwnerHost::new(root.clone(), true);
        let owners = host
            .local()
            .await
            .expect("the owners still start with the store off");
        assert!(
            owners.store.is_none(),
            "the store is not opened with the switch off"
        );
        assert!(
            !root.join("store").exists(),
            "the switch off creates no store folder"
        );
        host.stop().await;
        assert!(host.local().await.is_none());
    }
}
