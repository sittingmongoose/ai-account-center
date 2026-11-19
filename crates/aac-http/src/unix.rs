use crate::{
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

struct Shared {
    boot: Boot,
    limits: Limits,
    identity: Identity,
    feeds: Feeds,
    agents: Arc<crate::agent_transport::Agents>,
    /// R3: the hosted store, when the boot carries its config.
    store: Option<Arc<crate::store_host::StoreHost>>,
    /// R4: the owned Status readers, when the boot carries their config.
    status_owned: Option<Arc<crate::status_owned::StatusOwned>>,
    /// R4: the owned Scoreboard, when the boot carries its config.
    scoreboard_owned: Option<Arc<crate::scoreboard_owned::ScoreboardOwned>>,
    /// G16b: the answer store's writer, with the owned Status readers.
    answers_owned: Option<Arc<crate::answers_owned::AnswersOwned>>,
    /// G16c: the Status feed's producers, with the owned Status readers.
    status_produce: Option<Arc<crate::status_produce::Produce>>,
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
        const BASE: [&str; 4] = ["ccsDir", "socketPath", "secret", "bootId"];
        let mut last = None;
        for extra in [
            vec![],
            vec!["files"],
            vec!["store"],
            vec!["files", "store"],
            vec!["owned"],
            vec!["files", "owned"],
            vec!["store", "owned"],
            vec!["files", "store", "owned"],
        ] {
            let keys: Vec<&str> = BASE.iter().copied().chain(extra).collect();
            match crate::json_object::from_slice(bytes, &keys) {
                Ok(boot) => return Ok(boot),
                Err(error) => last = Some(error),
            }
        }
        Err(last.unwrap_or_else(|| {
            serde_json::Error::io(io::Error::new(io::ErrorKind::InvalidData, "invalid boot"))
        }))
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
    let error = if handler == Handler::Settings {
        "TypeScript settings fallback required"
    } else {
        "TypeScript read fallback required"
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
            let now = one_header(req.headers(), "x-aac-now").and_then(|text| text.parse::<u64>().ok());
            let zone = one_header(req.headers(), "x-aac-zone")
                .and_then(|text| serde_json::from_str::<ZoneHeader>(text).ok());
            let query = req.uri().query().is_some();
            let Ok(body) = to_bytes(req.into_body(), 0).await else {
                return decline(row.handler, Decline::Shape);
            };
            let (Some(now), Some(zone), false, true) = (now, zone, query, body.is_empty()) else {
                return decline(row.handler, Decline::Shape);
            };
            match shared
                .limits
                .blocking_status(move || produce.view(now, &zone.time_zone, &zone.facts()))
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
        read => {
            let path = req.uri().path().to_owned();
            let query = req.uri().query().is_some();
            let Ok(bytes) = to_bytes(req.into_body(), INPUT_CAP).await else {
                return decline(read, Decline::Shape);
            };
            let root = shared.boot.ccs_dir.clone();
            match shared
                .limits
                .blocking(row.pool, move || {
                    crate::read_routes::answer(read, &path, query, &root, &bytes)
                })
                .await
            {
                Ok(value) => Json(value).into_response(),
                Err(reason) => decline(read, reason),
            }
        }
    }
}

fn read_settings(root: &Path) -> Result<AccountRefreshSettings, Decline> {
    let (file, policy) =
        crate::registered::file(root, "account-refresh-settings").ok_or(Decline::Unsupported)?;
    let aac_files::read::Read::Bytes(bytes, _) = aac_files::read::read(&file, &policy) else {
        return Err(Decline::Unsupported);
    };
    // Require the raw object and exact unique keys before the strict integer DTO parse.
    // Original bytes are retained: no Value normalization or positional struct decoding.
    // These small integers are exact in JS; every other input delegates to the TS owner.
    let value: AccountRefreshSettings =
        crate::json_object::from_slice(&bytes, &["refreshIntervalSeconds"])
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
    boot.validate()?;
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
    let store = boot.store.clone().map(crate::store_host::StoreHost::new);
    let feeds = Feeds::new();
    // R4: the owned readers run under the store host, with the boot's share roots and the
    // registry writers TS resolved at start.
    let owners = boot.owners()?;
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
    // G16b: the answer store moves with the owned Status group.
    let answers_owned = match (&boot.owned, &store) {
        (Some(owned), Some(_)) if owned.status => Some(crate::answers_owned::AnswersOwned::new(
            boot.ccs_dir.clone(),
            owned.reports_root.clone(),
        )),
        _ => None,
    };
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
    // G16c: the producers of the fed Status generations (the owned Status group).
    let status_produce = match (&boot.owned, &answers_owned) {
        (Some(owned), Some(answers)) if owned.status => Some(crate::status_produce::Produce::new(
            boot.ccs_dir.clone(),
            owned.reports_root.clone(),
            feeds.clone(),
            Arc::clone(answers),
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
    let state = AppState(Arc::new(Shared {
        boot,
        limits: Limits::new(),
        identity,
        feeds,
        agents,
        store: store.clone(),
        status_owned: status_owned.clone(),
        scoreboard_owned: scoreboard_owned.clone(),
        answers_owned: answers_owned.clone(),
        status_produce: status_produce.clone(),
    }));
    // One method router per path, so a path with a GET and a PUT row is registered once.
    let mut paths: Vec<(String, MethodRouter<AppState>)> = Vec::new();
    for row in rows {
        let row = Arc::new(row);
        let filter = if row.method == axum::http::Method::POST {
            MethodFilter::POST
        } else if row.method == axum::http::Method::PUT {
            MethodFilter::PUT
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
        .with_state(state);
    tracing::info!(target: "aac_http", "private service ready");
    // R3: the hosted store opens beside the HTTP service (its calls wait for the open),
    // its jobs run on TS's cadence, and the engine closes cleanly when the service ends.
    let jobs_stop = Arc::new(AtomicBool::new(false));
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
        let produce = status_produce.clone();
        tokio::spawn(async move { crate::status_owned::jobs(jobs, produce, stop).await });
    }
    if let Some(owner) = &status_produce {
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::status_produce::Produce::jobs(jobs, stop).await });
    }
    if let Some(owner) = &answers_owned {
        let jobs = Arc::clone(owner);
        let stop = Arc::clone(&jobs_stop);
        tokio::spawn(async move { crate::answers_owned::jobs(jobs, stop).await });
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
    let served = axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await;
    jobs_stop.store(true, Ordering::Release);
    if let Some(owner) = answers_owned {
        let _ = tokio::task::spawn_blocking(move || owner.close()).await;
    }
    if let Some(host) = store {
        let _ = tokio::task::spawn_blocking(move || host.close_engine()).await;
    }
    served
}

/// R3: the store jobs loop — the first pass minutes after start, then every 15 minutes,
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
