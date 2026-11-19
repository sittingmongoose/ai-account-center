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
}

/// R4 (DESIGN-M3-COMPLETE 1.2/1.4): which owners this boot runs, and the share roots they
/// read. TS resolves the roots and the group switches once at start; the store host must be
/// on for either owner (the runs tables and the report captures go through its lock).
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OwnedBoot {
    /// The agent-reports root (`AAC_AGENT_REPORTS_DIR`, else the shared folder).
    pub reports_root: PathBuf,
    /// The pm-mail registry folder (`AAC_PM_MAIL_REGISTRY_DIR`, else the shared inbox).
    pub registry_dir: PathBuf,
    /// The owned Status readers (the status group on, under the store host).
    pub status: bool,
    /// The owned Scoreboard (the scoreboard group on, under the store host).
    pub scoreboard: bool,
}

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
    approvals: crate::account_approvals::Approvals,
    /// M5 lane K: the claims file's writer lease, held from first use to the end of the boot.
    reset_claims: crate::reset_claims::ResetClaims,
    app_update_jobs: Option<BoundJobs>,
    /// G17: the Home feed producer (the TS producer's work, in process).
    producer: Arc<crate::home_producer::HomeProducer>,
    native_usage: bool,
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
}

#[derive(Clone)]
struct AppState(Arc<Shared>);

fn failure() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "invalid private service boundary",
    )
}

fn private_dir(path: &Path, uid: u32) -> io::Result<()> {
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
            &["files", "store", "owned", "renewal"],
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
        // R4: the owned readers read absolute share roots, and only under the store host
        // (the runs tables and the report captures go through its single-writer lock).
        if let Some(owned) = &self.owned
            && (!owned.reports_root.is_absolute()
                || !owned.registry_dir.is_absolute()
                || ((owned.status || owned.scoreboard) && self.store.is_none()))
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
            use crate::account_usage::cursor::{Command, ROW, Request};
            if shared.owners.owner(ROW) != Some(registry::Role::RustService) {
                return decline(row.handler, Decline::Unsupported);
            }
            let Ok(Ok(bytes)) =
                tokio::time::timeout(Duration::from_secs(2), to_bytes(req.into_body(), 4096)).await
            else {
                return decline(row.handler, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            if let Ok(command) = Command::parse(&bytes) {
                return match tokio::time::timeout(
                    Duration::from_secs(123),
                    shared.cursor.command(command, root),
                )
                .await
                {
                    Ok(Ok(value)) => Json(value).into_response(),
                    Ok(Err(reason)) => decline(row.handler, reason),
                    Err(_) => decline(row.handler, Decline::Internal),
                };
            }
            let request = match Request::parse(&bytes) {
                Ok(request) => request,
                Err(reason) => return decline(row.handler, reason),
            };
            let owners = shared.owners.clone();
            let reader = shared.cursor.clone();
            let task = tokio::spawn(async move { reader.read(root, owners, request).await });
            match tokio::time::timeout(Duration::from_secs(123), task).await {
                Ok(Ok(Ok(value))) => Json(value.as_ref()).into_response(),
                Ok(Ok(Err(reason))) => decline(row.handler, reason),
                _ => decline(row.handler, Decline::Internal),
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
            let feeds = shared.feeds.clone();
            match shared
                .limits
                .blocking_status(move || crate::status_fed::answer(&feeds, &feed))
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
            match owner.answer(&feed).await {
                Ok((bytes, etag)) => {
                    crate::view_reply::respond_tagged("scoreboard", 1, bytes, &etag)
                }
                Err(reason) => decline(row.handler, reason),
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
        read => {
            let path = req.uri().path().to_owned();
            let query = req.uri().query().is_some();
            let Ok(bytes) = to_bytes(req.into_body(), INPUT_CAP).await else {
                return decline(read, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            let store = shared.store.clone();
            let profiles_path = path.clone();
            let profiles_root = root.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    // R5/A2: the owned Analytics envelope reads the hosted store itself.
                    if path == "/api/accounts/analytics"
                        && let Some(answer) =
                            crate::account_reads::owned_analytics(store.as_ref(), &bytes)
                    {
                        return answer;
                    }
                    crate::read_routes::answer(read, &path, query, &root, &bytes)
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

/// Boot account collection with the private server's L5 pool and the public listener's cache.
pub async fn serve_with_native_account_usage(
    boot: Boot,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    cache: crate::account_usage::AccountUsageCache,
    native: NativeAccountUsage,
) -> io::Result<()> {
    serve_inner(boot, shutdown, None, false, cache, Some(native), None).await
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
    if native.is_some()
        && owners.owner(crate::collection_cache::OBSERVATIONS) != Some(registry::Role::RustService)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "native collection requires its registry owner",
        ));
    }
    let rows = crate::table::rows(crate::table::ROUTES)?;
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
    let agents = Arc::new(crate::agent_transport::Agents::new(boot.ccs_dir.clone()));
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
    let store = boot.store.clone().map(crate::store_host::StoreHost::new);
    let collection = Arc::new(crate::collection::Collection::default());
    // R4: the owned readers run under the store host, with the boot's share roots and the
    // registry writers TS resolved at start.
    // One readers-check writer for both owners (one file per writer carries the status
    // scan's rows and the runs rows), and one flush in flight.
    let check = Arc::new(std::sync::Mutex::new(
        aac_status::share::check::ReadersCheckWriter::new(
            boot.owned
                .as_ref()
                .map(|owned| owned.reports_root.clone())
                .unwrap_or_default(),
        ),
    ));
    let check_flushing = Arc::new(AtomicBool::new(false));
    let status_owned = match (&boot.owned, &store) {
        (Some(owned), Some(host)) if owned.status => Some(crate::status_owned::StatusOwned::new(
            owned.reports_root.clone(),
            owned.registry_dir.clone(),
            Arc::clone(host),
            feeds.clone(),
            owners.clone(),
            Arc::clone(&check),
            Arc::clone(&check_flushing),
        )),
        _ => None,
    };
    let scoreboard_owned = match (&boot.owned, &store) {
        (Some(owned), Some(host)) if owned.scoreboard => {
            Some(crate::scoreboard_owned::ScoreboardOwned::new(
                owned.reports_root.clone(),
                Arc::clone(host),
                feeds.clone(),
                Arc::clone(&check),
                owners.clone(),
                Arc::clone(&check_flushing),
            ))
        }
        _ => None,
    };
    let owned_reports_root = boot.owned.as_ref().map(|owned| owned.reports_root.clone());
    let renewal = boot.renewal.clone();
    let ccs_dir = boot.ccs_dir.clone();
    let cursor = Arc::<crate::account_usage::cursor::Reader>::default();
    let producer = native.as_ref().map(|_| {
        crate::account_usage::producer::Producer::shared(
            boot.ccs_dir.clone(),
            owners.clone(),
            agents.clone(),
            collection.clone(),
            cursor.clone(),
        )
    });
    let package_root = crate::agent_transport::package_root();
    let usage = Arc::new(crate::usage_snapshot::UsageSnapshot::new(
        boot.ccs_dir.clone(),
        package_root.clone().unwrap_or_else(|| boot.ccs_dir.clone()),
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
    let state = AppState(Arc::new(Shared {
        owners,
        cache_writes: Arc::default(),
        boot,
        limits: Limits::new(),
        identity,
        feeds,
        approvals: crate::account_approvals::Approvals::default(),
        reset_claims: crate::reset_claims::ResetClaims::default(),
        app_update_jobs: app_update_jobs.map(BoundJobs::new),
        producer: Arc::new(crate::home_producer::HomeProducer::default()),
        native_usage: native.is_some(),
        agents,
        collection: collection.clone(),
        cursor,
        store: store.clone(),
        status_owned: status_owned.clone(),
        scoreboard_owned: scoreboard_owned.clone(),
        usage,
        claude_open,
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
        tokio::spawn(async move { store_jobs(jobs, stop).await });
    }
    // R4: the owned readers' jobs, and the catalog seed before the first ingest pass.
    if let Some(owner) = &status_owned {
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::status_owned::jobs(jobs, stop).await });
    }
    if let Some(owner) = &scoreboard_owned {
        let seeder = Arc::clone(owner);
        let root = owned_reports_root.clone().expect("scoreboard owner");
        let _ = tokio::task::spawn_blocking(move || {
            let mut failures = Vec::new();
            aac_status::share::seed::seed_catalog(
                &root,
                &|name| aac_status::share::seed::embedded_seed(name).map(|bytes| bytes.to_vec()),
                &mut failures,
            );
            seeder.set_seed_failures(
                failures
                    .iter()
                    .map(|failure| json!({"file": failure.file, "code": failure.code}))
                    .collect(),
            );
        })
        .await;
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::scoreboard_owned::ingest_jobs(jobs, stop).await });
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::scoreboard_owned::catalog_jobs(jobs, stop).await });
    }
    let cancel = aac_transport::CancellationToken::new();
    let task = producer.zip(native).map(|(producer, native)| {
        let stop = cancel.clone();
        let store = store.clone();
        tokio::spawn(async move {
            producer
                .run(
                    cache,
                    stop,
                    || (native.companions)(),
                    |cache| {
                        (native.published)(&cache);
                        let store = store.clone();
                        async move {
                            if let Some(store) = store {
                                let _ = tokio::task::spawn_blocking(move || {
                                    store.record_native_account_usage(&cache)
                                })
                                .await;
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
    jobs_stop.store(true, Ordering::Release);
    let _ = jobs_stop_tx.send(true);
    if let Some(host) = store {
        let _ = tokio::task::spawn_blocking(move || host.close_engine()).await;
    }
    if let Some(task) = renewal_task {
        let _ = task.await;
    }
    served
}

/// R3: the store jobs loop â€” the first pass minutes after start, then every 15 minutes,
/// as TS's maintenance loop ran. Each pass is one blocking task; the flag ends the loop
/// when the service ends.
async fn store_jobs(host: Arc<crate::store_host::StoreHost>, stop: Arc<AtomicBool>) {
    tokio::time::sleep(Duration::from_millis(crate::store_host::FIRST_PASS_MS)).await;
    loop {
        if stop.load(Ordering::Acquire) {
            return;
        }
        let pass = Arc::clone(&host);
        let _ =
            tokio::task::spawn_blocking(move || pass.job_pass(aac_store::engine::now_ms())).await;
        tokio::time::sleep(Duration::from_millis(crate::store_host::MAINTAIN_EVERY_MS)).await;
    }
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
