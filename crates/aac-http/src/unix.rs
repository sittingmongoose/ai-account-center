use crate::{
    app_update_routes::{AppUpdateJobs, BoundJobs, Operation as AppUpdateOperation},
    feed::Feeds,
    identity::{self, Identity},
    limits::Limits,
    table::{Handler, Row},
};
use aac_api::{AccountRefreshSettings, VerifiedPrincipal};
use aac_files::registry::{self, Owners};
use aac_views::decline::Decline;
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{MethodFilter, MethodRouter, on},
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs, io,
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener as StdListener,
    },
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use subtle::ConstantTimeEq;
use tokio::net::UnixListener;

const SECRET_HEADER: &str = "x-aac-boot-secret";

/// G16c: the display zone's facts the front door sends with an owned Status request.
#[derive(Deserialize)]
struct ZoneHeader {
    #[serde(rename = "timeZone")]
    time_zone: String,
    from: i64,
    to: i64,
    offsets: Vec<[i64; 2]>,
}

impl ZoneHeader {
    fn facts(&self) -> aac_status::produce::facts::ZoneFacts {
        aac_status::produce::facts::ZoneFacts {
            from: self.from,
            to: self.to,
            offsets: self.offsets.clone(),
        }
    }
}
const PRINCIPAL_HEADER: &str = "x-aac-principal";
/// The named reason of a private 503 (L1).
const DECLINE_HEADER: &str = "x-aac-decline";
/// Request inputs are capped at 4 MiB.
const INPUT_CAP: usize = 4 * 1024 * 1024;

/// Per-service-boot configuration, delivered through a private stdin pipe, never argv.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Boot {
    /// Absolute AAC private directory, explicitly supplied by TS (no home discovery).
    pub ccs_dir: PathBuf,
    /// Random private boot directory's socket: under `ccs_dir/run`, or, when that path would be
    /// too long for a Unix socket, under a short private folder bound to `ccs_dir` (L1).
    pub socket_path: PathBuf,
    /// 32 random bytes encoded as lowercase hex; kept in memory for this service boot.
    pub secret: String,
    /// Public random correlation id, distinct from the secret.
    pub boot_id: String,
    /// Each private-file registry row's writer as TS resolved it at start (L2,
    /// `config/private-files.json`); absent means every row's own `owner`.
    #[serde(default)]
    pub files: Option<BTreeMap<String, String>>,
    /// R3: the store host's config as TS resolved it at start; absent (or null) keeps the
    /// store in the TS process.
    #[serde(default)]
    pub store: Option<crate::store_host::StoreBoot>,
    /// R4: the owned readers' config as TS resolved it at start; absent keeps the share
    /// readers and the Scoreboard owner in the TS process.
    #[serde(default)]
    pub owned: Option<OwnedBoot>,
    /// G10: the renewal timer's homes as TS resolved them at start; absent (or null) keeps
    /// the saved-login renewal timer in the TS process.
    #[serde(default)]
    pub renewal: Option<RenewalBoot>,
    /// The private service owns lifecycle when it already owns the collection generation.
    #[serde(default)]
    pub native_lifecycle: bool,
    /// Legacy boot field retained for compatibility with the previous supervisor.
    #[serde(default)]
    pub native_usage_snapshot: bool,
    /// The public host started this private service without a worker.
    #[serde(default)]
    embedded_dashboard: bool,
}

pub use crate::owners::OwnedBoot;

/// G10: the saved-login renewal timer's boot config (the homes it reads).
#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RenewalBoot {
    /// The shared native Codex login home (`~/.codex`), absolute.
    pub codex_home: PathBuf,
    /// The home folder scanned for `.codex*` homes, absolute.
    pub home_dir: PathBuf,
}

struct Shared {
    boot: Boot,
    limits: Limits,
    owners: Owners,
    cache_writes: Arc<tokio::sync::Mutex<()>>,
    identity: Identity,
    feeds: Feeds,
    approvals: Arc<crate::account_approvals::Approvals>,
    lifecycle: Option<crate::public_auth::native_owners::NativeOwners>,
    /// M5 lane K: the claims file's writer lease, held from first use to the end of the boot.
    reset_claims: crate::reset_claims::ResetClaims,
    app_update_jobs: Option<BoundJobs>,
    /// G17: the Home feed producer (the TS producer's work, in process).
    producer: Arc<crate::home_producer::HomeProducer>,
    account_usage: crate::account_usage::AccountUsageCache,
    native_usage: bool,
    native_recheck: Option<NativeRecheck>,
    agents: Arc<crate::agent_transport::Agents>,
    collection: Arc<crate::collection::Collection>,
    cursor: Arc<crate::account_usage::cursor::Reader>,
    /// R3: the hosted store, when the boot carries its config.
    store: Option<Arc<crate::store_host::StoreHost>>,
    /// R4: the owned Status readers, when the boot carries their config.
    status_owned: Option<Arc<crate::status_owned::StatusOwned>>,
    /// R4: the owned Scoreboard, when the boot carries its config.
    scoreboard_owned: Option<Arc<crate::scoreboard_owned::ScoreboardOwned>>,
    /// G18: the aac-usage share snapshot publisher, fed by the front door's samples.
    usage: Arc<crate::usage_snapshot::UsageSnapshot>,
    /// G13: the Claude Open job owner, when the boot binds it.
    claude_open: Option<Arc<crate::claude_open::open::ClaudeOpen>>,
    /// G16b: the answer store's writer, with the owned Status readers.
    answers_owned: Option<Arc<crate::answers_owned::AnswersOwned>>,
    /// G16c: the Status feed's producers, with the owned Status readers.
    status_produce: Option<Arc<crate::status_produce::Produce>>,
    /// Singleflight for fed Status composition (`x-aac-feed`).
    status_compose: crate::status_fed::StatusCompose,
    /// R5/A2: whether the Analytics activity pass is in flight (the read's `refreshing`).
    analytics_live: Arc<crate::analytics_live::Live>,
    /// Bounded 4-entry generation-keyed Analytics response cache (eliminates per-poll recompute).
    analytics_cache: Arc<std::sync::Mutex<std::collections::VecDeque<AnalyticsCacheEntry>>>,
}

#[derive(Clone, PartialEq, Eq)]
struct AnalyticsCacheKey {
    query: String,
    write_seq: u64,
    refreshing: bool,
    mac_gen: Option<String>,
    win_gen: Option<String>,
}

struct AnalyticsCacheEntry {
    key: AnalyticsCacheKey,
    at_ms: i64,
    status: u16,
    bytes: Vec<u8>,
}

#[derive(Clone)]
struct AppState(Arc<Shared>);

struct NativeRecheck {
    producer: Arc<crate::account_usage::producer::Producer>,
    companions: Arc<dyn Fn() -> crate::account_usage::producer::CompanionInputs + Send + Sync>,
    published: Arc<dyn Fn(&crate::account_usage::AccountUsageCache) + Send + Sync>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RecheckInput {
    id: String,
}

fn failure() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "invalid private service boundary",
    )
}

pub(crate) fn private_dir(path: &Path, uid: u32) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o777 != 0o700 || meta.uid() != uid {
        return Err(failure());
    }
    Ok(())
}

/// The first 12 hex digits of SHA-256 of the canonical private root: the short socket folder's
/// name, so a folder made for one root is never used for another.
pub fn short_name(ccs_dir: &Path) -> String {
    Sha256::digest(ccs_dir.as_os_str().as_bytes())
        .iter()
        .take(6)
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn name_of(path: &Path) -> Option<&str> {
    path.file_name()?.to_str()
}

impl Boot {
    /// Parse only the exact object envelope supplied by TS; arrays are never positional fields.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        crate::json_object::from_slice_with_optional(
            bytes,
            &["ccsDir", "socketPath", "secret", "bootId"],
            &[
                "files",
                "store",
                "owned",
                "renewal",
                "nativeLifecycle",
                "nativeUsageSnapshot",
                "embeddedDashboard",
            ],
        )
    }

    /// Each registry row's writer for this boot. A map that names another row set, or gives a
    /// row to the service that does not move with a route group, refuses the boot: TS and the
    /// service must agree on every file's single writer.
    pub fn owners(&self) -> io::Result<Owners> {
        let rows = registry::rows().map_err(|_| failure())?;
        match &self.files {
            Some(files) => Owners::from_boot(rows, files).map_err(|_| failure()),
            None => Ok(Owners::defaults(rows)),
        }
    }

    /// The socket lives in `aac-http-<bootId>/http.sock` under either `<ccsDir>/run` or a short
    /// private folder: `aac-<hash12>` (in `$XDG_RUNTIME_DIR` or a configured run folder) or
    /// `<hash12>` inside a private `aac-<uid>` folder (in `/tmp`). Every folder on the way is a
    /// real 0700 folder of the root's owner; the hash binds the short folder to this root.
    fn validate(&self) -> io::Result<()> {
        if !self.ccs_dir.is_absolute()
            || fs::canonicalize(&self.ccs_dir)? != self.ccs_dir
            || self.secret.len() != 64
            || !self
                .secret
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.boot_id.len() != 24
            || !self.boot_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(failure());
        }
        self.owners()?;
        // R3: the hosted store is this root's store folder, never a path from anywhere
        // else; its job folders are absolute when named.
        if let Some(store) = &self.store
            && (store.binding_identity != aac_store::build_identity()
                || store.binding_identity["commit"] == "unknown"
                || store.dir != self.ccs_dir.join("store")
                || store
                    .snapshot_directory
                    .as_ref()
                    .is_some_and(|d| !d.is_absolute())
                || store
                    .share_backup_dir
                    .as_ref()
                    .is_some_and(|d| !d.is_absolute()))
        {
            return Err(failure());
        }
        // The share readers run independently of the optional history store.
        if let Some(owned) = &self.owned
            && (!owned.reports_root.is_absolute() || !owned.registry_dir.is_absolute())
        {
            return Err(failure());
        }
        // G10: the renewal timer reads the homes TS resolved; both must be absolute.
        if let Some(renewal) = &self.renewal
            && (!renewal.codex_home.is_absolute() || !renewal.home_dir.is_absolute())
        {
            return Err(failure());
        }
        let uid = fs::metadata(&self.ccs_dir)?.uid();
        let run = self.ccs_dir.join("run");
        private_dir(&run, uid)?;
        let boot_name = format!("aac-http-{}", self.boot_id);
        let boot = self.socket_path.parent().ok_or_else(failure)?;
        if name_of(&self.socket_path) != Some("http.sock") || name_of(boot) != Some(&boot_name) {
            return Err(failure());
        }
        let parent = boot.parent().ok_or_else(failure)?;
        if parent != run {
            let hash = short_name(&self.ccs_dir);
            let folder = name_of(parent).ok_or_else(failure)?;
            if folder == format!("aac-{hash}") {
                private_dir(parent, uid)?;
            } else if folder == hash {
                let owner = parent.parent().ok_or_else(failure)?;
                if name_of(owner) != Some(&format!("aac-{uid}")) {
                    return Err(failure());
                }
                private_dir(owner, uid)?;
                private_dir(parent, uid)?;
            } else {
                return Err(failure());
            }
        }
        private_dir(boot, uid)
    }
}

fn one_header<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    let mut values = headers.get_all(key).iter();
    let first = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(first)
}

fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"Authentication required"})),
    )
        .into_response()
}

async fn boundary(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let principal = (|| {
        let secret = one_header(req.headers(), SECRET_HEADER)?;
        if secret.len() != 64
            || !bool::from(secret.as_bytes().ct_eq(state.0.boot.secret.as_bytes()))
        {
            return None;
        }
        let encoded = one_header(req.headers(), PRINCIPAL_HEADER)?;
        if encoded.len() > 512 {
            return None;
        }
        let p: VerifiedPrincipal =
            crate::json_object::from_slice(encoded.as_bytes(), &["kind", "id", "role"]).ok()?;
        p.valid().then_some(p)
    })();
    let Some(principal) = principal else {
        return unauthorized();
    };
    req.extensions_mut().insert(principal);
    let mut response = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&state.0.boot.boot_id) {
        response.headers_mut().insert("x-aac-service-boot", value);
    }
    response
}

/// The private 503 with its named reason. The bodies are the M3 ones.
fn decline(handler: Handler, reason: Decline) -> Response {
    let error = match handler {
        Handler::Settings => "TypeScript settings fallback required",
        Handler::WriteSettings => "Usage refresh settings could not be saved.",
        Handler::WriteVisibility => "Account visibility could not be saved.",
        Handler::WritePreferences => "Dashboard preferences could not be saved.",
        _ => "TypeScript read fallback required",
    };
    let mut response = (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({ "error": error })),
    )
        .into_response();
    response
        .headers_mut()
        .insert(DECLINE_HEADER, HeaderValue::from_static(reason.as_str()));
    response
}

/// R3: a store call's error reply, the binding's `<code>: <message>` in an `error` field.
/// Not a decline: TS's store handle turns it back into the rejection the NAPI call gave.
fn store_error(error: &str) -> Response {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(json!({ "error": error })),
    )
        .into_response()
}

/// One row's endpoint: the table's roles, then its handler under its budget.
async fn endpoint(state: AppState, row: Arc<Row>, req: Request) -> Response {
    let Some(principal) = req.extensions().get::<VerifiedPrincipal>().cloned() else {
        return unauthorized();
    };
    if !row.admits(&principal) {
        return unauthorized();
    }
    if row.budget_ms == 0 {
        return handle(state, row, req).await;
    }
    let handler = row.handler;
    match tokio::time::timeout(
        Duration::from_millis(row.budget_ms),
        handle(state, row, req),
    )
    .await
    {
        Ok(response) => response,
        Err(_) => decline(handler, Decline::Deadline),
    }
}

async fn handle(state: AppState, row: Arc<Row>, req: Request) -> Response {
    let shared = &state.0;
    match row.handler {
        Handler::AppUpdates => {
            let Some(operation) = AppUpdateOperation::from_route(&row.method, &row.path) else {
                return decline(row.handler, Decline::Unsupported);
            };
            if !matches!(operation, AppUpdateOperation::Status) {
                let query = req.uri().query().is_some_and(|query| !query.is_empty());
                let body = to_bytes(req.into_body(), 1024).await;
                if query || !body.is_ok_and(|body| {
                    crate::json_object::from_slice::<serde_json::Map<String, serde_json::Value>>(
                        &body,
                        &[],
                    )
                    .is_ok()
                }) {
                    return (
                        StatusCode::BAD_REQUEST,
                        Json(json!({"error": "Provide an empty JSON object."})),
                    )
                        .into_response();
                }
            }
            let Some(jobs) = shared.app_update_jobs.as_ref() else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("x-aac-decline", "unsupported")],
                    Json(json!({"error": "App update jobs are unbound"})),
                )
                    .into_response();
            };
            match jobs.dispatch(operation).await {
                Ok(response) => response,
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::Approvals => {
            if req.uri().query().is_some() {
                return decline(row.handler, Decline::Shape);
            }
            let Ok(bytes) = to_bytes(req.into_body(), 4096).await else {
                return decline(row.handler, Decline::Shape);
            };
            match shared.approvals.apply(&bytes) {
                Some(value) => Json(value).into_response(),
                None => decline(row.handler, Decline::Shape),
            }
        }
        Handler::Lifecycle => {
            use crate::public_auth::lifecycle_host::{self, PACKET_CAP};
            if req.uri().query().is_some() {
                return decline(row.handler, Decline::Shape);
            }
            let Some(owner) = &shared.lifecycle else {
                return decline(row.handler, Decline::Unsupported);
            };
            let Some(principal) = req.extensions().get::<VerifiedPrincipal>().cloned() else {
                return unauthorized();
            };
            let Ok(bytes) = to_bytes(req.into_body(), PACKET_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            lifecycle_host::receive(
                owner,
                &principal,
                &bytes,
                row.path == lifecycle_host::COLLECTION_ENDPOINT,
            )
            .await
        }
        Handler::ResetClaims => {
            if req.uri().query().is_some() {
                return decline(row.handler, Decline::Shape);
            }
            let Ok(owners) = shared.boot.owners() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let Ok(bytes) = to_bytes(req.into_body(), crate::reset_claims::BODY_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            let state = state.clone();
            match shared
                .limits
                .blocking_for(row.pool, crate::limits::RESET_WAIT, move || {
                    state.0.reset_claims.apply(&root, &owners, &bytes)
                })
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::CursorAccount => {
            use crate::account_usage::cursor::{REQUEST_CAP, ROW};
            if shared.owners.owner(ROW) != Some(registry::Role::RustService) {
                return decline(row.handler, Decline::Unsupported);
            }
            let Ok(Ok(bytes)) = tokio::time::timeout(
                Duration::from_secs(2),
                to_bytes(req.into_body(), REQUEST_CAP),
            )
            .await
            else {
                return decline(row.handler, Decline::Shape);
            };
            match shared
                .cursor
                .answer(shared.boot.ccs_dir.clone(), shared.owners.clone(), &bytes)
                .await
            {
                Ok(value) => Json(value.as_ref()).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::ProviderCache => {
            if shared.owners.owner(crate::collection_cache::OBSERVATIONS)
                != Some(registry::Role::RustService)
            {
                return decline(row.handler, Decline::Unsupported);
            }
            let Ok(bytes) = to_bytes(req.into_body(), 4096).await else {
                return decline(row.handler, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    crate::account_usage::provider_caches::answer(&root, &bytes)
                })
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::CollectionSources => {
            use crate::account_usage::sources::{self, Sources};
            if shared.owners.owner(crate::collection_cache::OBSERVATIONS)
                != Some(registry::Role::RustService)
            {
                return decline(row.handler, Decline::Unsupported);
            }
            let root = shared.boot.ccs_dir.clone();
            match shared
                .limits
                .blocking(row.pool, move || sources::read_registry(&root))
                .await
            {
                Ok(Sources::Legacy) => Json(json!({"state":"absent"})).into_response(),
                Ok(Sources::V2(accounts)) => Json(json!({
                    "state":"ok","registry":{"version": 2,"accounts":accounts},
                }))
                .into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::CollectionTick => {
            if shared.owners.owner(crate::collection_cache::OBSERVATIONS)
                != Some(registry::Role::RustService)
            {
                return decline(row.handler, Decline::Unsupported);
            }
            let body =
                tokio::time::timeout(Duration::from_secs(2), to_bytes(req.into_body(), 64)).await;
            let Ok(Ok(bytes)) = body else {
                return decline(row.handler, Decline::Shape);
            };
            match shared.collection.next_tick(&bytes).await {
                Ok(()) => Json(json!({"due": true})).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::CollectionCache => {
            let Some((kind, key)) = req
                .uri()
                .path()
                .strip_prefix("/internal/collection-cache/")
                .and_then(|p| p.split_once('/'))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
            else {
                return decline(row.handler, Decline::Shape);
            };
            let owners = shared.owners.clone();
            let Ok(Ok(bytes)) = tokio::time::timeout(
                Duration::from_secs(2),
                to_bytes(req.into_body(), 256 * 1024),
            )
            .await
            else {
                return decline(row.handler, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            let writer = shared.cache_writes.clone().lock_owned().await;
            let saved = shared
                .limits
                .blocking(row.pool, move || {
                    let _writer = writer;
                    Ok(crate::collection_cache::write(&owners, &root, &kind, &key, &bytes).is_ok())
                })
                .await
                .unwrap_or(false);
            Json(json!({"saved": saved})).into_response()
        }
        Handler::Collect => {
            if shared.owners.owner(crate::collection_cache::OBSERVATIONS)
                != Some(registry::Role::RustService)
            {
                return decline(row.handler, Decline::Unsupported);
            }
            let verb = req
                .uri()
                .path()
                .strip_prefix("/internal/collect/")
                .unwrap_or("")
                .to_owned();
            let body = tokio::time::timeout(
                Duration::from_secs(2),
                to_bytes(req.into_body(), aac_protocol::MAX_FRAME_BYTES + 4096),
            )
            .await;
            let Ok(Ok(bytes)) = body else {
                return decline(row.handler, Decline::Shape);
            };
            shared
                .collection
                .read(shared.agents.clone(), &verb, &bytes)
                .await
        }
        Handler::Agent | Handler::AgentControl => {
            let path = req.uri().path().to_owned();
            let cap = if row.handler == Handler::AgentControl {
                65536
            } else {
                aac_protocol::MAX_SCAN_FRAME_BYTES + 4096
            };
            let bytes =
                match tokio::time::timeout(Duration::from_secs(2), to_bytes(req.into_body(), cap))
                    .await
                {
                    Ok(Ok(bytes)) => bytes,
                    _ => return decline(row.handler, Decline::Shape),
                };
            let name = path.strip_prefix("/internal/agent/").unwrap_or("");
            if row.handler == Handler::AgentControl {
                shared
                    .agents
                    .control(name.strip_suffix("/control").unwrap_or(""), &bytes)
                    .await
            } else {
                shared.agents.call(name, &bytes).await
            }
        }
        Handler::SignInHealth => {
            let Ok(Ok(bytes)) =
                tokio::time::timeout(Duration::from_secs(2), to_bytes(req.into_body(), 4096)).await
            else {
                return decline(row.handler, Decline::Shape);
            };
            match crate::signins::answer(&bytes) {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::SignInPoll => {
            let Ok(Ok(bytes)) =
                tokio::time::timeout(Duration::from_secs(2), to_bytes(req.into_body(), 4096)).await
            else {
                return decline(row.handler, Decline::Shape);
            };
            let hosts = match crate::signins::poll::parse(&bytes) {
                Ok(hosts) => hosts,
                Err(reason) => return decline(row.handler, reason),
            };
            match crate::signins::poll::read(&shared.agents, &shared.boot.ccs_dir, hosts).await {
                Ok(batch) => Json(json!({
                    "machines": batch.machines, "referenceHosts": batch.reference_hosts,
                }))
                .into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        // R3: the store host's method contract, the NAPI binding's over the socket. No
        // deadline (the table's budget is 0): backup and maintain take as long as they
        // took in process. Errors keep the binding's `<code>: <message>` shape.
        Handler::Store => {
            let Some(host) = &shared.store else {
                return store_error("service_down: the store host is not enabled on this boot");
            };
            let path = req.uri().path().to_owned();
            let method = path.strip_prefix("/internal/store/").unwrap_or_default();
            if method.is_empty()
                || method.len() > 32
                || !method.bytes().all(|b| b.is_ascii_alphanumeric())
            {
                return store_error("invalid: unknown method");
            }
            let Ok(bytes) = to_bytes(req.into_body(), crate::store_host::STORE_INPUT_CAP).await
            else {
                return store_error("invalid: store request too large");
            };
            let Ok(args) = String::from_utf8(bytes.to_vec()) else {
                return store_error("invalid: store request is not text");
            };
            match host.call(method.to_owned(), args).await {
                Ok(text) => (
                    StatusCode::OK,
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    text,
                )
                    .into_response(),
                Err(error) => store_error(&error),
            }
        }
        // R3: a body without the writer snapshot gets store status from the hosted Engine
        // in process. A snapshot body stays the pre-R3 writer-validated read (store off).
        Handler::StoreStatus => {
            let query = req.uri().query().is_some();
            let Ok(bytes) = to_bytes(req.into_body(), INPUT_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            let snapshot_shaped = crate::json_object::from_slice::<
                serde_json::Map<String, serde_json::Value>,
            >(&bytes, &["snapshot"])
            .is_ok();
            if snapshot_shaped {
                let path = "/api/store/status".to_owned();
                let root = shared.boot.ccs_dir.clone();
                return match shared
                    .limits
                    .blocking(row.pool, move || {
                        crate::read_routes::answer(
                            Handler::StoreStatus,
                            &path,
                            query,
                            &root,
                            &bytes,
                            None,
                        )
                    })
                    .await
                {
                    Ok(value) => Json(value).into_response(),
                    Err(reason) => decline(row.handler, reason),
                };
            }
            let Some(host) = shared.store.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            match shared
                .limits
                .blocking(row.pool, move || host.status_value())
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::Health => {
            let mut response = Json(aac_diag::health()).into_response();
            response
                .headers_mut()
                .insert(identity::HEADER, shared.identity.header());
            if shared.lifecycle.is_some() {
                response
                    .headers_mut()
                    .insert(identity::LIFECYCLE_HEADER, HeaderValue::from_static("1"));
            }
            response
        }
        Handler::Diag => Json(aac_diag::diagnostics()).into_response(),
        Handler::Settings => {
            if req.uri().query().is_some_and(|q| !q.is_empty()) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"Unexpected settings query."})),
                )
                    .into_response();
            }
            let root = shared.boot.ccs_dir.clone();
            match shared
                .limits
                .blocking(row.pool, move || read_settings(&root))
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::WriteSettings => {
            let capture = row.path == "/api/accounts/analytics/capture";
            if req.uri().query().is_some_and(|q| !q.is_empty()) {
                let body = if capture {
                    json!({"error":"Unexpected capture settings query.", "code":"invalid_query"})
                } else {
                    json!({"error":"Unexpected settings query."})
                };
                return (StatusCode::BAD_REQUEST, Json(body)).into_response();
            }
            let owners = match shared.boot.owners() {
                Ok(owners) => owners,
                Err(_) => return decline(row.handler, Decline::Unsupported),
            };
            let Ok(bytes) = to_bytes(req.into_body(), 4096).await else {
                return invalid_settings(capture);
            };
            let value = if capture {
                crate::json_object::from_slice::<serde_json::Value>(&bytes, &["cwd"])
                    .ok()
                    .filter(|capture| capture["cwd"].is_boolean())
            } else {
                parse_settings(&bytes).ok().map(|settings| json!(settings))
            };
            let Some(value) = value else {
                return invalid_settings(capture);
            };
            let root = shared.boot.ccs_dir.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    let rows = registry::rows().map_err(|_| Decline::Internal)?;
                    let store = if capture {
                        "analytics-capture"
                    } else {
                        "account-refresh-settings"
                    };
                    let file = owners
                        .claim(rows, store, &root, None)
                        .map_err(|_| Decline::Unsupported)?;
                    let mut bytes = serde_json::to_vec(&value).map_err(|_| Decline::Internal)?;
                    bytes.push(b'\n');
                    file.replace_settings(&bytes)
                        .map_err(|_| Decline::Internal)?;
                    if !capture {
                        file.clean_settings_temporaries();
                    }
                    Ok(value)
                })
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::WritePreferences => {
            if req.uri().query().is_some_and(|query| !query.is_empty()) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"Unexpected preferences query."})),
                )
                    .into_response();
            }
            let patch = req.method() == axum::http::Method::PATCH;
            let Ok(bytes) = to_bytes(req.into_body(), crate::preferences_write::INPUT_CAP).await
            else {
                return decline(row.handler, Decline::Unsupported);
            };
            let Ok(owners) = shared.boot.owners() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let root = shared.boot.ccs_dir.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    crate::preferences_write::save(&root, &owners, patch, &bytes)
                })
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::WriteVisibility => {
            let patch = req.method() == axum::http::Method::PATCH;
            if req.uri().query().is_some() {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({
                        "error": "This request does not take a query string.",
                        "code": "unexpected_query",
                    })),
                )
                    .into_response();
            }
            let Ok(owners) = shared.boot.owners() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let Ok(bytes) = to_bytes(req.into_body(), 9 * 1024).await else {
                return decline(row.handler, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    crate::visibility_write::save(&root, &owners, patch, &bytes)
                })
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::Feed | Handler::FeedStatus => {
            let source = req
                .uri()
                .path()
                .strip_prefix("/internal/feed/")
                .unwrap_or_default()
                .to_owned();
            if row.handler == Handler::FeedStatus {
                return match shared.feeds.status(&source) {
                    Ok(value) => Json(value).into_response(),
                    Err(reason) => decline(row.handler, reason),
                };
            }
            if shared.native_usage && source.starts_with(crate::feed::HOME_DASHBOARD) {
                return decline(row.handler, Decline::Unsupported);
            }
            let Ok(bytes) = to_bytes(req.into_body(), INPUT_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            let feeds = shared.feeds.clone();
            match shared
                .limits
                .blocking(row.pool, move || feeds.put(&source, &bytes))
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::Dashboard => {
            if shared.owners.owner(crate::collection_cache::OBSERVATIONS)
                != Some(registry::Role::RustService)
            {
                return decline(row.handler, Decline::Unsupported);
            }
            if shared.native_usage && !req.headers().contains_key(crate::home_fed::FEED_HEADER) {
                let headers = req.headers().clone();
                let uri = req.uri().clone();
                let Ok(_) = to_bytes(req.into_body(), 0).await else {
                    return decline(row.handler, Decline::Shape);
                };
                let cache = shared.account_usage.clone();
                return match shared
                    .limits
                    .blocking(row.pool, move || {
                        crate::dashboard_fed::latest(&cache, &uri, &headers)
                    })
                    .await
                {
                    Ok(reply) => reply,
                    Err(reason) => decline(row.handler, reason),
                };
            }
            let feed = one_header(req.headers(), crate::home_fed::FEED_HEADER).map(str::to_owned);
            let query = req.uri().query().is_some();
            let Ok(body) = to_bytes(req.into_body(), 0).await else {
                return decline(row.handler, Decline::Shape);
            };
            let (Some(feed), false, true) = (feed, query, body.is_empty()) else {
                return decline(row.handler, Decline::Shape);
            };
            let feeds = shared.feeds.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    crate::dashboard_fed::answer(&feeds, &feed)
                })
                .await
            {
                Ok((bytes, identity)) => crate::view_reply::respond_bytes(
                    crate::dashboard_fed::VIEW,
                    crate::dashboard_fed::VERSION,
                    bytes,
                    &identity,
                ),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::CollectionRecheck => {
            let Some(native) = &shared.native_recheck else {
                return decline(row.handler, Decline::Unsupported);
            };
            if req.uri().query().is_some() {
                return decline(row.handler, Decline::Shape);
            }
            let Ok(bytes) = to_bytes(req.into_body(), 1024).await else {
                return decline(row.handler, Decline::Shape);
            };
            let Ok(input) = crate::json_object::from_slice::<RecheckInput>(&bytes, &["id"]) else {
                return decline(row.handler, Decline::Shape);
            };
            if input.id.is_empty() || input.id.len() > 512 {
                return decline(row.handler, Decline::Shape);
            }
            match native
                .producer
                .recheck(&shared.account_usage, &(native.companions)(), &input.id)
                .await
            {
                Ok(Some(value)) => {
                    (native.published)(&shared.account_usage);
                    Json(value).into_response()
                }
                Ok(None) => (
                    StatusCode::NOT_FOUND,
                    Json(json!({"code": "unknown_account"})),
                )
                    .into_response(),
                Err(_) => store_error("recheck_failed: the account could not be checked safely"),
            }
        }
        // G17: the Home feed producer — the raw Home inputs in, the held generations named. The transitional
        // socket form of the producer; lane I's collection feeds it in process once it owns the dashboard read.
        Handler::HomeInputs => {
            let Ok(bytes) = to_bytes(req.into_body(), INPUT_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            let Ok(inputs) = serde_json::from_slice::<crate::home_producer::Inputs>(&bytes) else {
                return decline(row.handler, Decline::Shape);
            };
            let feeds = shared.feeds.clone();
            let producer = shared.producer.clone();
            match shared
                .limits
                .blocking(row.pool, move || producer.produce(&feeds, &inputs))
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        // R1: Home from the fed generations (no body; the ids, clock and context in headers). Without
        // `x-aac-feed` it is D7's envelope read below, which TS's own handler still uses.
        Handler::Home if req.headers().contains_key(crate::home_fed::FEED_HEADER) => {
            let feed = one_header(req.headers(), crate::home_fed::FEED_HEADER).map(str::to_owned);
            let contexts = req
                .headers()
                .get_all(crate::home_fed::CONTEXT_HEADER)
                .iter()
                .count();
            let page =
                one_header(req.headers(), crate::home_fed::CONTEXT_HEADER).map(str::to_owned);
            let query = req.uri().query().is_some();
            let Ok(body) = to_bytes(req.into_body(), 0).await else {
                return decline(row.handler, Decline::Shape);
            };
            let (Some(feed), false, true, true) = (feed, query, body.is_empty(), contexts <= 1)
            else {
                return decline(row.handler, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            let feeds = shared.feeds.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    crate::home_fed::answer(&feeds, &root, &feed, page.as_deref())
                })
                .await
            {
                Ok(answer) => crate::view_reply::respond_bytes(
                    aac_views::home::fed::VIEW,
                    aac_views::home::fed::VERSION,
                    answer.bytes,
                    &answer.identity,
                ),
                Err(reason) => decline(row.handler, reason),
            }
        }
        // G16c: Status from this service's own producers (no body; the clock and the display
        // zone's offsets in `x-aac-now`/`x-aac-zone`). Without a zone it declines and the front
        // door answers with its own producers.
        Handler::Status if req.headers().contains_key("x-aac-owned") => {
            let Some(produce) = shared.status_produce.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let now =
                one_header(req.headers(), "x-aac-now").and_then(|text| text.parse::<u64>().ok());
            let zone = one_header(req.headers(), "x-aac-zone")
                .and_then(|text| serde_json::from_str::<ZoneHeader>(text).ok());
            let query = req.uri().query().is_some();
            let Ok(body) = to_bytes(req.into_body(), 0).await else {
                return decline(row.handler, Decline::Shape);
            };
            let (Some(now), Some(zone), false, true) = (now, zone, query, body.is_empty()) else {
                return decline(row.handler, Decline::Shape);
            };
            match produce
                .view_async(Some(&shared.limits), now, &zone.time_zone, &zone.facts())
                .await
            {
                Ok((bytes, etag)) => crate::view_reply::respond_tagged(
                    crate::status_fed::VIEW,
                    crate::status_fed::VERSION,
                    bytes,
                    &etag,
                ),
                Err(reason) => decline(row.handler, reason),
            }
        }
        // R2: Status from the fed generations, the same way (no body; the ids and the clock in `x-aac-feed`).
        // Without it, the envelope or snapshot read below.
        Handler::Status if req.headers().contains_key(crate::home_fed::FEED_HEADER) => {
            let feed = one_header(req.headers(), crate::home_fed::FEED_HEADER).map(str::to_owned);
            let query = req.uri().query().is_some();
            let Ok(body) = to_bytes(req.into_body(), 0).await else {
                return decline(row.handler, Decline::Shape);
            };
            let (Some(feed), false, true) = (feed, query, body.is_empty()) else {
                return decline(row.handler, Decline::Shape);
            };
            match shared
                .status_compose
                .answer_fed(&shared.feeds, &shared.limits, &feed)
                .await
            {
                Ok((bytes, etag)) => crate::view_reply::respond_tagged(
                    crate::status_fed::VIEW,
                    crate::status_fed::VERSION,
                    bytes,
                    &etag,
                ),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::Events => {
            let mut response = shared.feeds.stream().into_response();
            response.headers_mut().insert(
                axum::http::header::CONTENT_TYPE,
                HeaderValue::from_static("application/x-ndjson"),
            );
            response
        }
        // R4: Scoreboard from the owned store and catalog (no body; the filters, the clock
        // and the facts generation in `x-aac-feed`). Without it, the envelope read below.
        Handler::Scoreboard if req.headers().contains_key(crate::home_fed::FEED_HEADER) => {
            let Some(owner) = shared.scoreboard_owned.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let feed = one_header(req.headers(), crate::home_fed::FEED_HEADER).map(str::to_owned);
            let query = req.uri().query().is_some();
            let Ok(body) = to_bytes(req.into_body(), 0).await else {
                return decline(row.handler, Decline::Shape);
            };
            let (Some(feed), false, true) = (feed, query, body.is_empty()) else {
                return decline(row.handler, Decline::Shape);
            };
            // Its own task: a compose that outlives this request's budget still finishes and
            // fills the owner's view cache, so the next request for the filter is a cache hit
            // (today's handler answers this one).
            let answering = tokio::spawn(async move { owner.answer(&feed).await });
            match answering.await {
                Ok(Ok((bytes, etag))) => {
                    crate::view_reply::respond_tagged("scoreboard", 1, bytes, &etag)
                }
                Ok(Err(reason)) => decline(row.handler, reason),
                Err(_) => decline(row.handler, Decline::Internal),
            }
        }
        // R4: the owned Status snapshots the front door forwards as the fed generations
        // (and its fallback composer reads when the service declines).
        Handler::StatusInputs => {
            let Some(owner) = shared.status_owned.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            match shared
                .limits
                .blocking(row.pool, move || Ok(owner.inputs()))
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        // The page's answer history from the owned feed: the journal and its verified share
        // copies, newest first, in the route's projection.
        Handler::StatusAnswersRead => {
            let Some(limit) = crate::status_answers_route::read_limit(req.uri().query()) else {
                return decline(row.handler, Decline::Shape);
            };
            let Some(produce) = shared.status_produce.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let Ok(body) = to_bytes(req.into_body(), 0).await else {
                return decline(row.handler, Decline::Shape);
            };
            if !body.is_empty() {
                return decline(row.handler, Decline::Shape);
            }
            match shared
                .limits
                .blocking(row.pool, move || Ok(produce.answers_view(limit)))
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        // The route's answer set: the same record path as the forwarded send, with the route
        // body (`answers`, `sendId`) and the browser's device word read here.
        Handler::StatusAnswersPost => {
            let Some(owner) = shared.answers_owned.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            if req.uri().query().is_some() {
                return decline(row.handler, Decline::Shape);
            }
            let device =
                crate::status_answers_route::device_of(one_header(req.headers(), "user-agent"));
            let Ok(bytes) = to_bytes(req.into_body(), crate::answers_owned::SEND_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                return decline(row.handler, Decline::Shape);
            };
            let Ok(body) = crate::status_answers_route::submit_body(&value, device) else {
                return decline(row.handler, Decline::Shape);
            };
            match owner.submit(body.to_string().as_bytes()).await {
                Ok(value) => {
                    if let Some(produce) = &shared.status_produce {
                        produce.mark_answers();
                    }
                    Json(value).into_response()
                }
                Err("shape") => decline(row.handler, Decline::Shape),
                Err(_) => store_error("answers_failed: the answer store could not finish"),
            }
        }
        // The history store's reads: the binding's own methods over the hosted Engine.
        Handler::UsageHistory | Handler::UsageAt | Handler::UsageBurn => {
            let handler = row.handler;
            let raw = req.uri().query().map(str::to_owned);
            let valid = match handler {
                Handler::UsageHistory => {
                    crate::usage_routes::valid_history(raw.as_deref(), aac_store::engine::now_ms())
                }
                Handler::UsageAt => crate::usage_routes::valid_at(raw.as_deref()),
                _ => crate::usage_routes::valid_burn(raw.as_deref()),
            };
            if !valid {
                return decline(handler, Decline::Shape);
            }
            let Some(host) = shared
                .store
                .as_ref()
                .filter(|host| host.analytics_engine().is_some())
            else {
                return decline(row.handler, Decline::Unsupported);
            };
            let answer = match handler {
                Handler::UsageHistory => {
                    crate::usage_routes::history(host, raw.as_deref(), aac_store::engine::now_ms())
                        .await
                }
                Handler::UsageAt => {
                    crate::usage_routes::at(host, raw.as_deref(), aac_store::engine::now_ms()).await
                }
                _ => {
                    crate::usage_routes::burn(host, raw.as_deref(), aac_store::engine::now_ms())
                        .await
                }
            };
            match answer {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(handler, reason),
            }
        }
        // G16b: one forwarded send, recorded by the service's answer store.
        Handler::StatusAnswers => {
            let Some(owner) = shared.answers_owned.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let query = req.uri().query().is_some();
            let Ok(bytes) = to_bytes(req.into_body(), crate::answers_owned::SEND_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            if query {
                return decline(row.handler, Decline::Shape);
            }
            match owner.submit(&bytes).await {
                Ok(value) => {
                    if let Some(produce) = &shared.status_produce {
                        produce.mark_answers();
                    }
                    Json(value).into_response()
                }
                Err("shape") => decline(row.handler, Decline::Shape),
                // Started and failed: not a decline (the front door must not record it again).
                Err(_) => store_error("answers_failed: the answer store could not finish"),
            }
        }
        // G16c: the dashboard's usage sample, built by the Analytics owner and held for the feed.
        Handler::StatusUsage => {
            let Some(produce) = shared.status_produce.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let query = req.uri().query().is_some();
            let Ok(bytes) = to_bytes(req.into_body(), 4 * 1024 * 1024).await else {
                return decline(row.handler, Decline::Shape);
            };
            if query {
                return decline(row.handler, Decline::Shape);
            }
            let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
                return decline(row.handler, Decline::Shape);
            };
            let usage = if value.is_null() {
                None
            } else {
                match crate::status_produce::UsageInput::parse(&value) {
                    Some(usage) => Some(usage),
                    None => return decline(row.handler, Decline::Shape),
                }
            };
            produce.set_usage(usage);
            Json(json!({"ok": true})).into_response()
        }
        // R4: the owned Scoreboard input the front door computes the facts over.
        Handler::ScoreboardInputs => {
            let Some(owner) = shared.scoreboard_owned.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            match owner.inputs().await {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        // G18: one pushed analytics sample; accept spawns the parse and publish onto its own
        // blocking task and replies at once, so neither holds a read permit.
        Handler::UsageSample => {
            let Ok(bytes) =
                to_bytes(req.into_body(), crate::usage_snapshot::USAGE_SAMPLE_CAP).await
            else {
                return decline(row.handler, Decline::Shape);
            };
            match shared.usage.accept(bytes) {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(row.handler, reason),
            }
        }
        Handler::ClaudeOpen => {
            let Some(open) = shared.claude_open.as_ref() else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("x-aac-decline", "unsupported")],
                    Json(json!({"error": "Claude Open jobs are unbound"})),
                )
                    .into_response();
            };
            if req.uri().query().is_some_and(|query| !query.is_empty()) {
                return decline(row.handler, Decline::Shape);
            }
            let Ok(bytes) = to_bytes(req.into_body(), 4096).await else {
                return decline(row.handler, Decline::Shape);
            };
            let Ok(map) = crate::json_object::from_slice::<
                serde_json::Map<String, serde_json::Value>,
            >(&bytes, &["id", "platform", "respondAsync"]) else {
                return decline(row.handler, Decline::Shape);
            };
            let (Some(id), Some(platform), Some(respond_async)) = (
                map.get("id").and_then(serde_json::Value::as_str),
                map.get("platform").and_then(serde_json::Value::as_str),
                map.get("respondAsync").and_then(serde_json::Value::as_bool),
            ) else {
                return decline(row.handler, Decline::Shape);
            };
            if !crate::claude_open::manifest::profile_id_pattern(id)
                || !matches!(platform, "mac" | "windows")
            {
                return decline(row.handler, Decline::Shape);
            }
            let reply = open.open(id, platform, respond_async).await;
            (
                StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(reply.body),
            )
                .into_response()
        }
        Handler::ClaudeHistoryCleanup => {
            let Some(open) = shared.claude_open.as_ref() else {
                return (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [("x-aac-decline", "unsupported")],
                    Json(json!({"error": "Claude Open jobs are unbound"})),
                )
                    .into_response();
            };
            if req.uri().query().is_some_and(|query| !query.is_empty()) {
                return decline(row.handler, Decline::Shape);
            }
            let Ok(bytes) = to_bytes(req.into_body(), 4096).await else {
                return decline(row.handler, Decline::Shape);
            };
            if crate::json_object::from_slice::<serde_json::Map<String, serde_json::Value>>(
                &bytes,
                &[],
            )
            .is_err()
            {
                return decline(row.handler, Decline::Shape);
            }
            let reply = open.cleanup().await;
            (
                StatusCode::from_u16(reply.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(reply.body),
            )
                .into_response()
        }
        // R5/A2: the owned Analytics read, composed from the service's own store, feed and
        // writer state. The front door admitted the session and the query; the object arrives
        // with no body.
        Handler::Analytics => {
            let query = req.uri().query().map(str::to_owned);
            let Ok(body) = to_bytes(req.into_body(), INPUT_CAP).await else {
                return decline(row.handler, Decline::Shape);
            };
            // A composed envelope (the retained worker's bridge, and the parity table) keeps the
            // proven part-3 / owned composition; an empty body is the native read's own inputs.
            if !body.is_empty() {
                let path = "/api/accounts/analytics".to_owned();
                let root = shared.boot.ccs_dir.clone();
                let store = shared.store.clone();
                return match shared
                    .limits
                    .blocking(row.pool, move || {
                        if let Some(answer) =
                            crate::account_reads::owned_analytics(store.as_ref(), &body)
                        {
                            return answer;
                        }
                        crate::read_routes::answer(
                            Handler::Accounts,
                            &path,
                            query.is_some(),
                            &root,
                            &body,
                            None,
                        )
                    })
                    .await
                {
                    Ok(value) => Json(value).into_response(),
                    Err(reason) => decline(row.handler, reason),
                };
            }
            let Some(store) = shared.store.clone() else {
                return decline(row.handler, Decline::Unsupported);
            };
            let root = shared.boot.ccs_dir.clone();
            let cursor_root = root.clone();
            let compose_query = query.clone();
            // The same fed dashboard generation Accounts and Home render, as the read's input.
            let cache = shared.account_usage.clone();
            let feeds = shared.feeds.clone();
            let live = Arc::clone(&shared.analytics_live);
            let force_refresh =
                url::form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
                    .any(|(key, value)| key == "refresh" && value == "true");
            let now_check = aac_store::engine::now_ms() as i64;
            let cache_key = if !force_refresh {
                store.analytics_engine().map(|engine| AnalyticsCacheKey {
                    query: query.clone().unwrap_or_default(),
                    write_seq: engine.write_seq(),
                    refreshing: live.refreshing(),
                    mac_gen: cache.generation(crate::account_usage::Desktop::Mac, false),
                    win_gen: cache.generation(crate::account_usage::Desktop::Windows, false),
                })
            } else {
                None
            };
            if let Some(key) = &cache_key
                && let Ok(guard) = shared.analytics_cache.lock()
                && let Some(hit) = guard
                    .iter()
                    .find(|e| e.key == *key && now_check.saturating_sub(e.at_ms) < 30_000)
            {
                return (
                    StatusCode::from_u16(hit.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    hit.bytes.clone(),
                )
                    .into_response();
            }
            let result = shared
                .limits
                .blocking(row.pool, move || {
                    store.wait_ready();
                    let engine = store.analytics_engine().ok_or(Decline::Internal)?;
                    let now = aac_store::engine::now_ms() as i64;
                    let snapshot = analytics_snapshot(&cache, &feeds, now);
                    let answer = crate::analytics_route::compose(
                        Arc::clone(&engine),
                        &root,
                        snapshot.as_ref(),
                        &live,
                        compose_query.as_deref(),
                        now,
                    );
                    Ok((answer, engine, now))
                })
                .await;
            let result = match result {
                Ok((mut answer, engine, now)) => {
                    if shared.owners.owner(crate::account_usage::cursor::ROW)
                        == Some(registry::Role::RustService)
                        && let Ok(value) = &mut answer
                        && let Some(activity) = value["activity"].as_object_mut()
                        && let Some(status) = Arc::clone(&shared.cursor)
                            .analytics(
                                cursor_root,
                                shared.owners.clone(),
                                engine,
                                now,
                                force_refresh,
                            )
                            .await
                    {
                        activity.insert("cursorAccount".into(), status);
                    }
                    let encode_query = query.clone();
                    shared
                        .limits
                        .blocking(row.pool, move || {
                            crate::analytics_route::encode(answer, encode_query.as_deref())
                        })
                        .await
                }
                Err(reason) => Err(reason),
            };
            match result {
                Ok((status, bytes)) => {
                    if let Some(key) = cache_key
                        && status == 200
                        && let Ok(mut guard) = shared.analytics_cache.lock()
                    {
                        guard.retain(|e| e.key != key);
                        guard.push_back(AnalyticsCacheEntry {
                            key,
                            at_ms: now_check,
                            status,
                            bytes: bytes.clone(),
                        });
                        while guard.len() > 4 {
                            guard.pop_front();
                        }
                    }
                    (
                        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                        [(axum::http::header::CONTENT_TYPE, "application/json")],
                        bytes,
                    )
                        .into_response()
                }
                Err(reason) => decline(row.handler, reason),
            }
        }
        read => {
            let path = req.uri().path().to_owned();
            let query = req.uri().query().is_some();
            let Ok(bytes) = to_bytes(req.into_body(), INPUT_CAP).await else {
                return decline(read, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            let profiles_path = path.clone();
            let profiles_root = root.clone();
            let usage_cache = shared.account_usage.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    crate::read_routes::answer(
                        read,
                        &path,
                        query,
                        &root,
                        &bytes,
                        Some(&usage_cache),
                    )
                })
                .await
            {
                Ok(value) => {
                    // G13: a bound Open owner adds each Open's progress to the manifest rows,
                    // exactly as the TS composition adds its own store's progress.
                    let value = if read == Handler::Profiles
                        && profiles_path == "/api/claude/desktop-profiles"
                    {
                        match shared.claude_open.as_ref() {
                            Some(open) => crate::claude_open::open::with_open_operations(
                                value,
                                open,
                                &profiles_root,
                            ),
                            None => value,
                        }
                    } else {
                        value
                    };
                    Json(value).into_response()
                }
                Err(reason) => decline(read, reason),
            }
        }
    }
}

pub(crate) fn read_settings(root: &Path) -> Result<AccountRefreshSettings, Decline> {
    let (file, policy) =
        crate::registered::file(root, "account-refresh-settings").ok_or(Decline::Unsupported)?;
    let aac_files::read::Read::Bytes(bytes, _) = aac_files::read::read(&file, &policy) else {
        return Err(Decline::Unsupported);
    };
    parse_settings(&bytes)
}

fn invalid_settings(capture: bool) -> Response {
    let body = if capture {
        json!({"error": r#"Send {"cwd": true} or {"cwd": false}."#, "code": "invalid_body"})
    } else {
        json!({"error":"Choose a whole number of seconds from 30 to 3600."})
    };
    (StatusCode::BAD_REQUEST, Json(body)).into_response()
}

fn parse_settings(bytes: &[u8]) -> Result<AccountRefreshSettings, Decline> {
    // Require the raw object and exact unique keys before the strict integer DTO parse.
    // Original bytes are retained: no Value normalization or positional struct decoding.
    // These small integers are exact in JS; every other input delegates to the TS owner.
    let value: AccountRefreshSettings =
        crate::json_object::from_slice(bytes, &["refreshIntervalSeconds"])
            .map_err(|_| Decline::Shape)?;
    if (30..=3600).contains(&value.refresh_interval_seconds) {
        Ok(value)
    } else {
        Err(Decline::Number)
    }
}

struct SocketGuard {
    path: PathBuf,
    inode: u64,
    /// The boot folder, then a short socket folder: removed when empty, after our socket (L1),
    /// so a front door that died without cleaning up leaves nothing behind once the service ends.
    folders: Vec<PathBuf>,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
            for folder in &self.folders {
                // Only an empty folder goes; another boot of the same root keeps the short one.
                let _ = fs::remove_dir(folder);
            }
        }
    }
}

/// Bind only an AF_UNIX socket, set 0600 before accepting, and stop on the supplied signal.
/// Existing paths are never unlinked; ownership of the created socket is tracked by inode.
/// Dispatch comes from the one route table; a row without a known handler refuses startup.
pub async fn serve(
    boot: Boot,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    serve_inner(
        boot,
        shutdown,
        None,
        false,
        Default::default(),
        None,
        configured_claude_open_package(),
    )
    .await
}

/// Serve the private routes with an app-update job owner supplied by the service binary.
pub async fn serve_with_app_update_jobs(
    boot: Boot,
    shutdown: impl Future<Output = ()> + Send + 'static,
    jobs: Arc<dyn AppUpdateJobs>,
) -> io::Result<()> {
    serve_inner(
        boot,
        shutdown,
        Some(jobs),
        false,
        Default::default(),
        None,
        None,
    )
    .await
}

/// Same private protocol, with the native app-update owner using this boot's agent pool.
pub async fn serve_composed(
    boot: Boot,
    shutdown: impl Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    serve_inner(
        boot,
        shutdown,
        None,
        true,
        Default::default(),
        None,
        configured_claude_open_package(),
    )
    .await
}

/// Run the private service with a shared dashboard cache.
pub async fn serve_with_account_usage_cache(
    boot: Boot,
    shutdown: impl Future<Output = ()> + Send + 'static,
    cache: crate::account_usage::AccountUsageCache,
) -> io::Result<()> {
    serve_inner(boot, shutdown, None, false, cache, None, None).await
}

/// Native owner callbacks share the dashboard with the public listener, hub and store.
pub struct NativeAccountUsage {
    /// Read the current facts published by the adjacent Rust owners.
    pub companions: Arc<dyn Fn() -> crate::account_usage::producer::CompanionInputs + Send + Sync>,
    /// Observe each completed cache generation, for example in the usage hub.
    pub published: Arc<dyn Fn(&crate::account_usage::AccountUsageCache) + Send + Sync>,
}

impl Default for NativeAccountUsage {
    fn default() -> Self {
        Self {
            companions: Arc::new(crate::account_usage::producer::CompanionInputs::default),
            published: Arc::new(|_| {}),
        }
    }
}

/// Start the private binary with the native producer.
/// Boot account collection with the private server's L5 pool and the public listener's cache.
pub async fn serve_with_native_account_usage(
    boot: Boot,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    cache: crate::account_usage::AccountUsageCache,
    native: NativeAccountUsage,
) -> io::Result<()> {
    let app_updates = std::env::var("AAC_RUST_NATIVE_APP_UPDATES").as_deref() == Ok("1");
    serve_inner(
        boot,
        shutdown,
        None,
        app_updates,
        cache,
        Some(native),
        configured_claude_open_package(),
    )
    .await
}

/// Serve with the G13 Claude Open job owner bound: the package root supplies the pinned
/// reference helpers, and the owner shares the service's own L5 pool.
pub async fn serve_with_claude_open(
    boot: Boot,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    package_root: PathBuf,
) -> io::Result<()> {
    serve_inner(
        boot,
        shutdown,
        None,
        false,
        Default::default(),
        None,
        Some(package_root),
    )
    .await
}

fn configured_claude_open_package() -> Option<PathBuf> {
    (std::env::var("AAC_RUST_GROUP_CLAUDE_OPEN").as_deref() == Ok("on"))
        .then(crate::agent_transport::package_root)
        .flatten()
}

async fn serve_inner(
    boot: Boot,
    shutdown: impl Future<Output = ()> + Send + 'static,
    app_update_jobs: Option<Arc<dyn AppUpdateJobs>>,
    native_app_updates: bool,
    cache: crate::account_usage::AccountUsageCache,
    native: Option<NativeAccountUsage>,
    claude_open_package: Option<PathBuf>,
) -> io::Result<()> {
    boot.validate()?;
    let owners = boot.owners()?;
    if boot.native_usage_snapshot && !boot.native_lifecycle {
        return Err(failure());
    }
    if boot.native_lifecycle
        && (native.is_none()
            || owners.owner("account-registry") != Some(registry::Role::RustService)
            || owners.owner("opencode-wallet-source") != Some(registry::Role::RustService))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native lifecycle requires its collection and registry owners",
        ));
    }
    if native.is_some()
        && owners.owner(crate::collection_cache::OBSERVATIONS) != Some(registry::Role::RustService)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native collection requires its registry owner",
        ));
    }
    let rows = crate::table::rows(crate::table::ROUTES)?;
    let _collection_lease = native
        .as_ref()
        .map(|_| aac_files::OwnedStore::claim_collection(&boot.ccs_dir).map_err(io::Error::other))
        .transpose()?;
    let identity = Identity::current()?;
    let listener = StdListener::bind(&boot.socket_path)?;
    let mut folders = Vec::new();
    if let Some(folder) = boot.socket_path.parent() {
        folders.push(folder.to_path_buf());
        if let Some(short) = folder.parent().filter(|p| *p != boot.ccs_dir.join("run")) {
            folders.push(short.to_path_buf());
        }
    }
    let _guard = SocketGuard {
        path: boot.socket_path.clone(),
        inode: fs::symlink_metadata(&boot.socket_path)?.ino(),
        folders,
    };
    fs::set_permissions(&boot.socket_path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    let cut_boot = boot.embedded_dashboard || native.is_some();
    let agents = Arc::new(crate::agent_transport::Agents::with_default(
        boot.ccs_dir.clone(),
        cut_boot,
    ));
    let feeds = cache.feeds.clone();
    let app_update_jobs = app_update_jobs.or_else(|| {
        native_app_updates.then(|| {
            let jobs = crate::app_updates::AppUpdates::with_pool(
                boot.ccs_dir.clone(),
                agents.clone(),
                Some(feeds.clone()),
            );
            Arc::new(crate::app_update_routes::LaneHJobs(jobs)) as Arc<dyn AppUpdateJobs>
        })
    });
    let collection = Arc::new(crate::collection::Collection::default());
    // One construction for the owners, shared with the public host's in-process path.
    let crate::owners::Owners {
        store,
        status_owned,
        status_produce,
        answers_owned,
        scoreboard_owned,
    } = crate::owners::Owners::start(
        crate::owners::Config {
            ccs_dir: boot.ccs_dir.clone(),
            store: boot.store.clone(),
            owned: boot.owned.clone(),
            files: boot.files.clone(),
        },
        feeds.clone(),
    )?;
    let owned_reports_root = boot.owned.as_ref().map(|owned| owned.reports_root.clone());
    let renewal = boot.renewal.clone();
    let ccs_dir = boot.ccs_dir.clone();
    let cursor = Arc::<crate::account_usage::cursor::Reader>::default();
    let producer = native.as_ref().map(|_| {
        Arc::new(crate::account_usage::producer::Producer::shared(
            boot.ccs_dir.clone(),
            owners.clone(),
            agents.clone(),
            collection.clone(),
            cursor.clone(),
        ))
    });
    let native_recheck = producer
        .as_ref()
        .zip(native.as_ref())
        .map(|(producer, native)| NativeRecheck {
            producer: Arc::clone(producer),
            companions: Arc::clone(&native.companions),
            published: Arc::clone(&native.published),
        });
    let package_root = crate::agent_transport::package_root();
    let usage = Arc::new(crate::usage_snapshot::UsageSnapshot::new(
        boot.ccs_dir.clone(),
        store.clone(),
    ));
    let claude_open = claude_open_package.map(|package| {
        let remote = Arc::new(crate::claude_open::remote::AgentRemote::new(
            Arc::clone(&agents),
            package,
        ));
        let ccs_dir = boot.ccs_dir.clone();
        Arc::new(crate::claude_open::open::ClaudeOpen::new(
            ccs_dir.clone(),
            remote,
            Box::new(move || crate::claude_open::open::snapshot_cleanup_auto(&ccs_dir)),
        ))
    });
    let approvals = Arc::new(crate::account_approvals::Approvals::default());
    let lifecycle = producer
        .as_ref()
        .zip(native.as_ref())
        .filter(|_| boot.native_lifecycle)
        .map(|(producer, native)| {
            crate::public_auth::native_owners::NativeOwners::for_private_lifecycle(
                agents.clone(),
                owners.clone(),
                cache.clone(),
                Arc::clone(producer),
                approvals.clone(),
                feeds.clone(),
                Arc::clone(&native.published),
            )
        });
    let (lifecycle, lifecycle_jobs) = lifecycle.unzip();
    let state = AppState(Arc::new(Shared {
        owners,
        cache_writes: Arc::default(),
        boot,
        limits: Limits::new(),
        identity,
        feeds,
        approvals,
        lifecycle,
        reset_claims: crate::reset_claims::ResetClaims::default(),
        app_update_jobs: app_update_jobs.map(BoundJobs::new),
        producer: Arc::new(crate::home_producer::HomeProducer::default()),
        account_usage: cache.clone(),
        native_usage: native.is_some(),
        native_recheck,
        agents: agents.clone(),
        collection: collection.clone(),
        cursor,
        store: store.clone(),
        status_owned: status_owned.clone(),
        scoreboard_owned: scoreboard_owned.clone(),
        usage,
        claude_open,
        answers_owned: answers_owned.clone(),
        status_produce: status_produce.clone(),
        status_compose: crate::status_fed::StatusCompose::default(),
        analytics_live: Arc::new(crate::analytics_live::Live::default()),
        analytics_cache: Arc::default(),
    }));
    // One method router per path, so a path with a GET and a PUT row is registered once.
    let mut paths: Vec<(String, MethodRouter<AppState>)> = Vec::new();
    for row in rows {
        let row = Arc::new(row);
        let filter = if row.method == axum::http::Method::POST {
            MethodFilter::POST
        } else if row.method == axum::http::Method::PUT {
            MethodFilter::PUT
        } else if row.method == axum::http::Method::PATCH {
            MethodFilter::PATCH
        } else {
            MethodFilter::GET
        };
        let path = row.path.clone();
        let handler = move |State(state): State<AppState>, req: Request| {
            let row = row.clone();
            async move { endpoint(state, row, req).await }
        };
        match paths.iter_mut().find(|(p, _)| *p == path) {
            Some((_, router)) => *router = std::mem::take(router).on(filter, handler),
            None => paths.push((path, on(filter, handler))),
        }
    }
    let mut router = Router::new();
    for (path, method_router) in paths {
        router = router.route(&path, method_router);
    }
    // Global layer protects even unknown routes and unsupported methods.
    // G14: the Analytics writers read the fed inputs and the boot's root beside the service.
    let writers_feeds = state.0.feeds.clone();
    let writers_ccs = state.0.boot.ccs_dir.clone();
    let writers_live = Arc::clone(&state.0.analytics_live);
    let writers_cache = state.0.account_usage.clone();
    let router = router
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state.clone());
    tracing::info!(target: "aac_http", "private service ready");
    // G20: only a build that is actually serving becomes the one installers must keep.
    let running_build = package_root
        .as_deref()
        .and_then(aac_files::deployed_build::read_packaged_build);
    let home = std::env::var("HOME").map(PathBuf::from).unwrap_or_default();
    let record_now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    let unrecorded = aac_files::deployed_build::record_running_build(
        &ccs_dir,
        running_build.as_ref(),
        std::env::var("CCS_DIR").ok().as_deref(),
        std::env::var("CCS_HOME").ok().as_deref(),
        &home,
        record_now,
    );
    if unrecorded > 0 {
        tracing::warn!(target: "aac_http", event = "web.deployed_build.unrecorded", "Running build could not be recorded");
    }
    // R3: the hosted store opens beside the HTTP service (its calls wait for the open),
    // its jobs run on TS's cadence, and the engine closes cleanly when the service ends.
    let jobs_stop = Arc::new(AtomicBool::new(false));
    let (jobs_stop_tx, jobs_stop_rx) = tokio::sync::watch::channel(false);
    // G10: the hosted renewal timer runs on the TS cadence (first cycle 3-5 minutes,
    // then 6 hours +/- 30 minutes, sooner for a backoff or busy retry). The TS service
    // defers while this boot owns it; the stop flag ends the loop when the service ends.
    let renewal_task = renewal.map(|renewal| {
        let service = aac_renewal::service::CodexProfileRenewalService::new(
            aac_renewal::service::CodexProfileRenewalServiceDeps {
                ccs_dir: ccs_dir.clone(),
                renewal: aac_renewal::renewal::CodexProfileRenewalOptions {
                    env: aac_renewal::planner::CodexRenewalEnvironment {
                        ccs_dir,
                        codex_home: Some(renewal.codex_home),
                        home_dir: Some(renewal.home_dir),
                        ..Default::default()
                    },
                    ..Default::default()
                },
                ..Default::default()
            },
        );
        tokio::spawn(async move { service.run_until(jobs_stop_rx).await })
    });
    if let Some(host) = &store {
        let opener = Arc::clone(host);
        tokio::task::spawn_blocking(move || opener.open_at_boot());
        let jobs = Arc::clone(host);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(jobs.jobs(stop));
        // G14: the quota sampler and activity scheduler run when the front door feeds them on.
        let writers_host = Arc::clone(host);
        let writers_stop = Arc::clone(&jobs_stop);
        let writers_agents = Arc::clone(&agents);
        tokio::spawn(async move {
            crate::analytics_writers::analytics_jobs(
                writers_host,
                move |now| analytics_snapshot(&writers_cache, &writers_feeds, now),
                writers_agents,
                writers_ccs,
                writers_live,
                writers_stop,
            )
            .await
        });
    }
    // R4: the owned readers' jobs, and the catalog seed before the first ingest pass.
    if let Some(root) = owned_reports_root {
        crate::status_owned::initialize_reports(root).await;
    }
    if let Some(owner) = &status_produce {
        // Warm the initial sources for the display zone before the first read arrives
        // and before background answer retries can hold the answers engine.
        owner.warm().await;
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::status_produce::Produce::jobs(jobs, stop).await });
    }
    if let Some(owner) = &status_owned {
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        let produce = status_produce.clone();
        tokio::spawn(async move { crate::status_owned::jobs(jobs, produce, stop).await });
    }
    if let Some(owner) = &answers_owned {
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::answers_owned::jobs(jobs, stop).await });
    }
    if let Some(owner) = &scoreboard_owned {
        owner.seed().await;
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::scoreboard_owned::ingest_jobs(jobs, stop, true).await });
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::scoreboard_owned::catalog_jobs(jobs, stop, true).await });
    }
    let cancel = aac_transport::CancellationToken::new();
    let lifecycle_owner = state.0.lifecycle.clone();
    let share = lifecycle_owner.as_ref().map(|_| {
        (
            Arc::clone(&state.0.usage),
            aac_renewal::claude::ClaudeSignInRenewalReader::new(
                state.0.boot.ccs_dir.clone(),
                Default::default(),
            ),
        )
    });
    let task = producer.zip(native).map(|(producer, native)| {
        let stop = cancel.clone();
        let root = state.0.boot.ccs_dir.clone();
        let samples = std::sync::Mutex::new(crate::status_produce::UsageSamples::default());
        tokio::spawn(async move {
            producer
                .run(
                    cache,
                    stop,
                    |refresh| {
                        let facts = (native.companions)();
                        let owner = lifecycle_owner.clone();
                        async move {
                            match owner {
                                Some(owner) => owner.private_companions(facts, refresh).await,
                                None => facts,
                            }
                        }
                    },
                    |cache| {
                        (native.published)(&cache);
                        if let Some(produce) = &status_produce
                            && let Some(usage) = samples
                                .lock()
                                .unwrap_or_else(|poison| poison.into_inner())
                                .project(&root, &cache)
                        {
                            produce.set_usage(Some(usage));
                        }
                        let owner = lifecycle_owner.as_ref();
                        let share = share.as_ref();
                        async move {
                            if let (Some(owner), Some((writer, signins))) = (owner, share)
                                && let Some(payload) =
                                    owner.private_share_sample(&cache, signins).await
                            {
                                writer.accept_native(payload);
                            }
                        }
                    },
                )
                .await;
        })
    });
    let stopping = cancel.clone();
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            stopping.cancel();
            collection.stop();
        })
        .await;
    cancel.cancel();
    if let Some(task) = task {
        let _ = task.await;
    }
    if let Some(jobs) = lifecycle_jobs {
        jobs.stop().await;
    }
    jobs_stop.store(true, Ordering::Release);
    let _ = jobs_stop_tx.send(true);
    if let Some(owner) = answers_owned {
        let _ = tokio::task::spawn_blocking(move || owner.close()).await;
    }
    if let Some(host) = store {
        let _ = tokio::task::spawn_blocking(move || host.close_engine()).await;
    }
    if let Some(task) = renewal_task {
        let _ = task.await;
    }
    served
}

fn analytics_snapshot(
    cache: &crate::account_usage::AccountUsageCache,
    feeds: &crate::feed::Feeds,
    now: i64,
) -> Option<serde_json::Value> {
    crate::analytics_route::snapshot(cache, now).or_else(|| {
        let generation = feeds.latest("analytics-dashboard").ok()?;
        serde_json::from_str(generation.data.get()).ok()
    })
}

#[cfg(test)]
mod dto_tests {
    use super::*;
    #[test]
    fn store_binding_identity_is_checked_before_any_open() -> Result<(), Box<dyn std::error::Error>>
    {
        let root = tempfile::tempdir()?;
        let root = fs::canonicalize(root.path())?;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
        let run = root.join("run");
        fs::create_dir(&run)?;
        fs::set_permissions(&run, fs::Permissions::from_mode(0o700))?;
        let folder = run.join("aac-http-abcdefabcdefabcdefabcdef");
        fs::create_dir(&folder)?;
        fs::set_permissions(&folder, fs::Permissions::from_mode(0o700))?;
        let mut raw = serde_json::json!({
            "ccsDir": root, "socketPath": folder.join("http.sock"),
            "secret": "c".repeat(64), "bootId": "abcdefabcdefabcdefabcdef",
            "store": {"dir": root.join("store"), "retentionDays": 0,
                      "bindingIdentity": aac_store::build_identity()},
        });
        assert!(
            Boot::from_json(&serde_json::to_vec(&raw)?)?
                .validate()
                .is_ok()
        );
        for field in ["commit", "storeSchema", "bindingVersion"] {
            let original = raw["store"]["bindingIdentity"][field].clone();
            raw["store"]["bindingIdentity"][field] = serde_json::json!("mismatch");
            assert!(
                Boot::from_json(&serde_json::to_vec(&raw)?)?
                    .validate()
                    .is_err()
            );
            assert!(!root.join("store").exists());
            raw["store"]["bindingIdentity"][field] = original;
        }
        Ok(())
    }

    #[test]
    fn short_name_matches_the_front_door() {
        // tests/unit/web-server/rust-service-health.test.ts: shortName('/home/fixture/.ccs')
        assert_eq!(short_name(Path::new("/home/fixture/.ccs")), "0a2271d2066d");
    }
    #[test]
    fn boot_principal_and_route_dtos_cannot_use_sequence_as_struct() {
        let boot =
            br#"{"ccsDir":"/tmp/fixture","socketPath":"/tmp/fixture/socket","secret":"fixture","bootId":"fixture"}"#;
        assert!(Boot::from_json(boot).is_ok());
        for raw in [
            r#"["/tmp/fixture","/tmp/fixture/socket","fixture","fixture"]"#,
            "null",
            r#"{"ccsDir":"fixture","socketPath":"fixture","secret":"fixture","bootId":"fixture","extra":0}"#,
            r#"{"ccsDir":"fixture","socketPath":"fixture","secret":"fixture","bootId":"fixture","bootId":"other"}"#,
        ] {
            assert!(Boot::from_json(raw.as_bytes()).is_err());
        }
        for raw in [
            r#"["session","fixture-admin","admin"]"#,
            "null",
            r#"{"kind":"session","id":"fixture-admin","role":"admin","extra":0}"#,
            r#"{"kind":"session","id":"fixture-admin","role":"admin","role":"admin"}"#,
        ] {
            assert!(
                crate::json_object::from_slice::<VerifiedPrincipal>(
                    raw.as_bytes(),
                    &["kind", "id", "role"]
                )
                .is_err()
            );
        }
        let tail = r#"{"method":"*","path":"*","owner":"ts-fallback"}"#;
        for raw in [
            r#"["GET","/api/health","rust","core",["public"],400,"health"]"#,
            "null",
            r#"{"method":"GET","path":"/api/health","owner":"rust","group":"core","roles":["public"],"budgetMs":400,"handler":"health","extra":0}"#,
            r#"{"method":"GET","path":"/api/health","owner":"rust","owner":"rust","group":"core","roles":["public"],"budgetMs":400,"handler":"health"}"#,
        ] {
            assert!(crate::table::rows(&format!("[{raw},{tail}]")).is_err());
        }
    }
}
