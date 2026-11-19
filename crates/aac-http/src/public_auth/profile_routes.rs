//! The profile reads on the public listener, without the compatibility worker.
//!
//! These are the seven routes the worker used to serve for Claude desktop profiles, Codex saved
//! profiles and the Antigravity runtime. Every owner already existed for the private read path
//! (`profile_reads`, `claude_open`, `antigravity_host`); this module adds the public admission
//! the TypeScript routes owned (session/device scope, origin, content type, query and body
//! rules), the exact response bytes, and the reads TS used to perform before handing the
//! composition over (the desktop usage history text and the Codex registry/login facts).

use super::{DashboardAccess, DashboardPrincipal, Reply};
use crate::agent_transport::{Agents, Selector};
use crate::claude_open::manifest::list_profiles;
use crate::claude_open::open::{ClaudeOpen, OpenReply, with_open_operations};
use aac_renewal::jwt::{
    CodexActivationIdentity, decode_codex_activation_identity, decode_id_token,
    has_structurally_valid_id_token, matches_codex_activation_identity,
};
use aac_renewal::registry::{CodexRegistryProfile, read_codex_profile_registry};
use aac_status::read_context::Envelope;
use aac_transport::CallOptions;
use axum::{
    extract::Request,
    http::{HeaderMap, Method, Uri},
};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
use serde_json::{Map, Value, json};
use std::{
    ffi::OsString,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

/// The read-only accounts answer for the Codex quota rows, supplied by the boot's native
/// collection where that group runs. Empty when no collection owner exists (Windows).
pub(super) type RowsSource = Arc<dyn Fn() -> Vec<Value> + Send + Sync>;

const MISSING_HISTORY: &str = "CCS_DESKTOP_USAGE_MISSING";
const MAX_HISTORY_BYTES: u64 = 1024 * 1024;
const SSH_TIMEOUT_MS: u64 = 8_000;

/// The mac history read, byte for byte the reference transport's fixed script.
const MAC_HISTORY: &str = r#"if [ ! -f "$1" ]; then printf '%s' CCS_DESKTOP_USAGE_MISSING; exit 0; fi
history_size=$(/usr/bin/stat -f '%z' "$1") || exit 1
test "$history_size" -le 1048576 || exit 1
/bin/cat "$1"
"#;

/// One boot's profile owner.
#[derive(Clone)]
pub(super) struct Profiles {
    root: PathBuf,
    open: Arc<ClaudeOpen>,
    agents: Arc<Agents>,
    rows: RowsSource,
    /// The one owner host when this boot answers its groups through it.
    #[cfg(unix)]
    host: Option<Arc<super::owner_host::OwnerHost>>,
}

/// One profile's history read: the reference's three states.
enum HistoryRead {
    Unconfigured,
    Failed,
    Read(Option<String>),
}

impl Profiles {
    /// The owner for one boot root. Construction spawns and reads nothing.
    pub(super) fn start(root: PathBuf, rows: RowsSource, agents: Arc<Agents>) -> Self {
        let package = crate::agent_transport::package_root().unwrap_or_else(|| root.clone());
        let remote = Arc::new(crate::claude_open::remote::AgentRemote::new(
            Arc::clone(&agents),
            package,
        ));
        let scope = root.clone();
        let auto = root.clone();
        let open = Arc::new(ClaudeOpen::new(
            scope,
            remote,
            Box::new(move || crate::claude_open::open::snapshot_cleanup_auto(&auto)),
        ));
        Self {
            root,
            open,
            agents,
            rows,
            #[cfg(unix)]
            host: None,
        }
    }

    /// The owner for one boot that answers through the shared host.
    #[cfg(unix)]
    pub(super) fn start_with_host(
        root: PathBuf,
        rows: RowsSource,
        agents: Arc<Agents>,
        host: Arc<super::owner_host::OwnerHost>,
    ) -> Self {
        Self {
            host: Some(host),
            ..Self::start(root, rows, agents)
        }
    }

    /// Ask the one owner host; `None` is no owner or an explicit service decline (answer the
    /// route yourself).
    #[cfg(unix)]
    async fn host_request(
        &self,
        method: &str,
        uri: &str,
        principal: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Option<super::service_link::ServiceReply> {
        let host = self.host.as_ref()?;
        let reply = host.request(method, uri, principal, headers, body).await?;
        (reply.status != 503 || reply.headers.get("x-aac-decline").is_none()).then_some(reply)
    }

    /// Relay one service JSON reply with the public route's own ETag and cache headers.
    #[cfg(unix)]
    fn relay(reply: super::service_link::ServiceReply, no_store: bool) -> Reply {
        let Ok(value) = serde_json::from_slice::<Value>(&reply.body) else {
            return super::plain(502, "The service reply could not be read.");
        };
        let mut out = super::response(reply.status, value);
        if no_store {
            super::set_header(&mut out, "cache-control", "no-store");
        }
        out
    }

    /// Whether this owner answers the request. Dynamic Open ids are decoded later; the shape
    /// check here is the router's own (one nonempty segment, no slash).
    pub(super) fn owns(method: &Method, path: &str) -> bool {
        let path = path.trim_end_matches('/');
        let get = method == Method::GET || method == Method::HEAD;
        if get
            && matches!(
                path,
                "/api/claude/desktop-profiles"
                    | "/api/claude/desktop-profiles/usage"
                    | "/api/codex/profiles"
                    | "/api/codex/profiles/quotas"
                    | "/api/antigravity/profiles"
                    | "/api/antigravity/profiles/quotas"
            )
        {
            return true;
        }
        method == Method::POST
            && (path == "/api/claude/history-snapshots/cleanup" || open_id(path).is_some())
    }

    /// Serve one owned request. The caller already checked [`Self::owns`].
    pub(super) async fn handle(&self, request: Request) -> Reply {
        let path = request.uri().path().trim_end_matches('/').to_owned();
        let uri = request.uri().clone();
        let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
            return super::service_error();
        };
        match (request.method().clone(), path.as_str()) {
            (Method::GET | Method::HEAD, "/api/claude/desktop-profiles") => {
                self.desktop_profiles(&access).await
            }
            (Method::GET | Method::HEAD, "/api/claude/desktop-profiles/usage") => {
                self.desktop_usage(&access, &uri).await
            }
            (Method::POST, "/api/claude/history-snapshots/cleanup") => {
                self.cleanup(&access, request).await
            }
            (Method::POST, _) => self.open(&access, request).await,
            (Method::GET | Method::HEAD, "/api/codex/profiles") => {
                self.codex_profiles(&access).await
            }
            (Method::GET | Method::HEAD, "/api/codex/profiles/quotas") => {
                self.codex_quotas(&access).await
            }
            (Method::GET | Method::HEAD, "/api/antigravity/profiles") => {
                self.antigravity_profiles(&access, &uri).await
            }
            (Method::GET | Method::HEAD, "/api/antigravity/profiles/quotas") => {
                self.antigravity_quotas(&access, &uri).await
            }
            _ => super::plain(404, "API endpoint was not found."),
        }
    }

    async fn desktop_profiles(&self, access: &DashboardAccess) -> Reply {
        if let Some(reply) = session_guard(access) {
            return reply;
        }
        #[cfg(unix)]
        if let Some(reply) = self
            .host_request(
                "GET",
                "/api/claude/desktop-profiles",
                &access.host_principal(),
                &[],
                None,
            )
            .await
        {
            return Self::relay(reply, false);
        }
        let root = self.root.clone();
        let open = Arc::clone(&self.open);
        let value = tokio::task::spawn_blocking(move || {
            crate::profile_reads::desktop_profiles(&root)
                .map(|value| with_open_operations(value, &open, &root))
        })
        .await
        .ok()
        .flatten();
        match value {
            Some(value) => super::response(200, value),
            None => super::plain(500, "Claude desktop profiles could not be read safely."),
        }
    }

    async fn desktop_usage(&self, access: &DashboardAccess, uri: &Uri) -> Reply {
        if let Some(reply) = session_guard(access) {
            return reply;
        }
        let Some(platform) = usage_platform(uri.query()) else {
            return super::plain(400, "Select a valid Claude desktop platform.");
        };
        #[cfg(unix)]
        if let Some(reply) = self
            .host_request("GET", &uri.to_string(), &access.host_principal(), &[], None)
            .await
        {
            return Self::relay(reply, false);
        }
        let profiles = match list_profiles(&self.root) {
            Ok(profiles) => profiles,
            Err(()) => {
                return super::plain(500, "Claude desktop usage could not be read safely.");
            }
        };
        let Some(fetched_at) = aac_num::ecma::to_iso_string(super::now()) else {
            return super::plain(500, "Claude desktop usage could not be read safely.");
        };
        let mut rows = Vec::with_capacity(profiles.len());
        for profile in &profiles {
            let launcher = match platform {
                "mac" => profile.mac.as_ref(),
                _ => profile.windows.as_ref(),
            };
            let read = match launcher {
                Some(launcher)
                    if launcher.ssh_host.is_some()
                        && launcher.profile_path.as_deref().is_some() =>
                {
                    match safe_path(
                        launcher.profile_path.as_deref().unwrap_or_default(),
                        platform,
                    ) {
                        Some(path) => {
                            self.read_history(
                                platform,
                                launcher.ssh_host.as_deref().unwrap_or_default(),
                                path,
                            )
                            .await
                        }
                        None => HistoryRead::Failed,
                    }
                }
                _ => HistoryRead::Unconfigured,
            };
            let mut row = Map::new();
            if let Some(id) = &profile.id {
                row.insert("id".into(), json!(id));
            }
            row.insert("email".into(), json!(profile.email));
            match read {
                HistoryRead::Unconfigured => {
                    row.insert("read".into(), json!("unconfigured"));
                }
                HistoryRead::Failed => {
                    row.insert("read".into(), json!("failed"));
                }
                HistoryRead::Read(history) => {
                    row.insert("read".into(), json!("read"));
                    row.insert("history".into(), json!(history));
                }
            }
            rows.push(Value::Object(row));
        }
        let input = json!({"platform": platform, "fetchedAt": fetched_at, "profiles": rows});
        match aac_views::claude_desktop::usage(&input) {
            Ok(value) => super::response(200, value),
            Err(_) => super::plain(500, "Claude desktop usage could not be read safely."),
        }
    }

    async fn read_history(
        &self,
        platform: &str,
        ssh_host: &str,
        profile_path: &str,
    ) -> HistoryRead {
        let options = CallOptions::new(
            Duration::from_millis(SSH_TIMEOUT_MS),
            MAX_HISTORY_BYTES as usize + 4096,
        );
        let selector = Selector::usage(platform, Some(ssh_host));
        let Some(_permit) = self.agents.permit() else {
            return HistoryRead::Failed;
        };
        let Some(client) = self.agents.client(&selector, options.deadline).await else {
            return HistoryRead::Failed;
        };
        let argv: Vec<OsString> = if platform == "mac" {
            [
                "/bin/sh",
                "-c",
                MAC_HISTORY,
                "aac-desktop-history",
                profile_path,
            ]
            .into_iter()
            .map(OsString::from)
            .collect()
        } else {
            let script = windows_history_script(profile_path);
            let encoded = BASE64.encode(utf16le(&script));
            vec![
                OsString::from("powershell.exe"),
                OsString::from("-NoProfile"),
                OsString::from("-NonInteractive"),
                OsString::from("-EncodedCommand"),
                OsString::from(encoded),
            ]
        };
        let Ok(output) = client.computer().execute(&argv, &[], &options).await else {
            return HistoryRead::Failed;
        };
        if output.code != Some(0) {
            return HistoryRead::Failed;
        }
        let Ok(text) = String::from_utf8(output.stdout) else {
            return HistoryRead::Failed;
        };
        if text.trim() == MISSING_HISTORY {
            HistoryRead::Read(None)
        } else {
            HistoryRead::Read(Some(text))
        }
    }

    async fn open(&self, access: &DashboardAccess, request: Request) -> Reply {
        if let Some(reply) = session_guard(access) {
            return reply;
        }
        if access.origin_present && !access.same_origin {
            return super::plain(403, "Claude account launch requires the dashboard origin.");
        }
        let path = request.uri().path().trim_end_matches('/').to_owned();
        let respond_async = prefers_respond_async(request.headers());
        let Some(raw_id) = open_id(&path) else {
            return super::plain(404, "API endpoint was not found.");
        };
        let Some(id) = super::decode_component(&raw_id) else {
            return super::plain(
                400,
                "Select a configured Claude desktop account and platform.",
            );
        };
        if !crate::claude_open::manifest::profile_id_pattern(&id) {
            return super::plain(
                400,
                "Select a configured Claude desktop account and platform.",
            );
        }
        if !json_content(&request) {
            return super::plain(415, "Claude account launch requires application/json.");
        }
        let Some(body) = object_body(request, 100 * 1024).await else {
            return super::plain(
                400,
                "Select a configured Claude desktop account and platform.",
            );
        };
        let platform = match (body.len(), body.get("platform").and_then(Value::as_str)) {
            (1, Some("mac")) => "mac",
            (1, Some("windows")) => "windows",
            _ => {
                return super::plain(
                    400,
                    "Select a configured Claude desktop account and platform.",
                );
            }
        };
        open_reply(
            self.open.open(&id, platform, respond_async).await,
            respond_async,
        )
    }

    async fn cleanup(&self, access: &DashboardAccess, request: Request) -> Reply {
        if let Some(reply) = session_guard(access) {
            return reply;
        }
        if access.origin_present && !access.same_origin {
            return super::plain(
                403,
                "History snapshot cleanup requires the dashboard origin.",
            );
        }
        if !json_content(&request) {
            return super::plain(415, "History snapshot cleanup requires application/json.");
        }
        if !object_body(request, 100 * 1024)
            .await
            .is_some_and(|body| body.is_empty())
        {
            return super::plain(400, "History snapshot cleanup takes no fields.");
        }
        #[cfg(unix)]
        {
            let service = r#"{"kind":"session","id":"front-door","role":"service"}"#;
            if let Some(reply) = self
                .host_request(
                    "POST",
                    "/internal/claude-history-cleanup",
                    service,
                    &[("content-type", "application/json")],
                    Some(b"{}"),
                )
                .await
            {
                return Self::relay(reply, false);
            }
        }
        open_reply(self.open.cleanup().await, false)
    }

    async fn codex_profiles(&self, access: &DashboardAccess) -> Reply {
        if let Some(reply) = codex_session_guard(access) {
            return reply;
        }
        #[cfg(unix)]
        if let Some(reply) = self
            .host_request(
                "GET",
                "/api/codex/profiles",
                &access.host_principal(),
                &[],
                None,
            )
            .await
        {
            return Self::relay(reply, false);
        }
        match codex_facts(&self.root)
            .and_then(|facts| aac_views::codex_profiles::summary(&facts).ok())
        {
            Some(value) => super::response(200, value),
            None => super::plain(500, "Codex profiles could not be read."),
        }
    }

    async fn codex_quotas(&self, access: &DashboardAccess) -> Reply {
        if let Some(reply) = codex_session_guard(access) {
            return reply;
        }
        #[cfg(unix)]
        if let Some(reply) = self
            .host_request(
                "GET",
                "/api/codex/profiles/quotas",
                &access.host_principal(),
                &[],
                None,
            )
            .await
        {
            return Self::relay(reply, false);
        }
        let root = self.root.clone();
        let rows = Arc::clone(&self.rows);
        let now = super::now();
        let value =
            tokio::task::spawn_blocking(move || codex_quota_answer(&root, rows.as_ref(), now))
                .await
                .ok()
                .flatten();
        match value {
            Some(value) => super::response(200, value),
            None => super::plain(500, "Codex profile usage could not be read safely."),
        }
    }

    async fn antigravity_profiles(&self, access: &DashboardAccess, uri: &Uri) -> Reply {
        if let Some(reply) = session_guard(access) {
            return reply;
        }
        if uri.query().is_some_and(|query| !query.is_empty()) {
            return no_store(super::plain(400, "Unexpected inventory query."));
        }
        #[cfg(unix)]
        if let Some(reply) = self
            .host_request("GET", &uri.to_string(), &access.host_principal(), &[], None)
            .await
        {
            return no_store(Self::relay(reply, true));
        }
        no_store(
            crate::profile_reads::antigravity_inventory(&self.root)
                .map(|value| super::response(200, value))
                .unwrap_or_else(|| {
                    super::plain(500, "Antigravity accounts could not be read safely.")
                }),
        )
    }

    async fn antigravity_quotas(&self, access: &DashboardAccess, uri: &Uri) -> Reply {
        if let Some(reply) = session_guard(access) {
            return reply;
        }
        let Some(refresh) = refresh_query(uri.query()) else {
            return no_store(super::plain(400, "Select a valid refresh value."));
        };
        #[cfg(unix)]
        if let Some(reply) = self
            .host_request("GET", &uri.to_string(), &access.host_principal(), &[], None)
            .await
        {
            return no_store(Self::relay(reply, true));
        }
        no_store(
            crate::profile_reads::antigravity_quotas(&self.root, refresh, super::now())
                .map(|value| super::response(200, value))
                .unwrap_or_else(|| {
                    super::plain(500, "Antigravity usage could not be read safely.")
                }),
        )
    }
}

fn open_reply(reply: OpenReply, respond_async: bool) -> Reply {
    let mut reply = super::response(reply.status, reply.body);
    if respond_async && reply.status().as_u16() == 202 {
        super::set_header(&mut reply, "preference-applied", "respond-async");
    }
    reply
}

/// `prefersRespondAsync`: a bounded `Prefer` header naming `respond-async` in any list member.
fn prefers_respond_async(headers: &HeaderMap) -> bool {
    headers
        .get_all("prefer")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter(|value| value.len() <= 1024)
        .flat_map(|value| value.split(','))
        .any(|preference| {
            preference
                .split(';')
                .next()
                .is_some_and(|word| word.trim().eq_ignore_ascii_case("respond-async"))
        })
}

/// `requireDashboardSession`: a signed-in browser, a paired device in the route's scope, or
/// localhost when the dashboard's sign-in is disabled.
fn session_guard(access: &DashboardAccess) -> Option<Reply> {
    if !access.auth_enabled {
        return (!access.local_access).then(|| {
            super::plain(
                403,
                "This endpoint requires localhost access when dashboard auth is disabled.",
            )
        });
    }
    matches!(access.principal, DashboardPrincipal::Anonymous)
        .then(|| super::plain(401, "Authentication required"))
}

fn codex_session_guard(access: &DashboardAccess) -> Option<Reply> {
    if !access.auth_enabled && !access.local_access {
        return Some(super::plain(
            403,
            "Codex auth profiles endpoint requires localhost access when dashboard auth is disabled.",
        ));
    }
    session_guard(access)
}

fn no_store(mut reply: Reply) -> Reply {
    super::set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn json_content(request: &Request) -> bool {
    super::header(request.headers(), "content-type")
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

async fn object_body(request: Request, limit: usize) -> Option<Map<String, Value>> {
    use axum::http::header::CONTENT_LENGTH;
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|size| size.to_str().ok());
    if declared
        .and_then(|size| size.parse::<usize>().ok())
        .is_some_and(|size| size > limit)
    {
        return None;
    }
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .ok()?;
    if bytes.is_empty() {
        // `express.json()` reads a bodyless JSON request as `{}`.
        return Some(Map::new());
    }
    serde_json::from_slice(&bytes).ok()
}

/// `/api/claude/desktop-profiles/<id>/open`, one safe segment, undecoded.
fn open_id(path: &str) -> Option<String> {
    let rest = path
        .strip_prefix("/api/claude/desktop-profiles/")?
        .strip_suffix("/open")?;
    (!rest.is_empty() && !rest.contains('/')).then(|| rest.to_owned())
}

/// The platform query the TypeScript route reads: one `platform` value, extra keys ignored.
fn usage_platform(query: Option<&str>) -> Option<&'static str> {
    let pairs: Vec<(String, String)> =
        url::form_urlencoded::parse(query.unwrap_or_default().as_bytes())
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
    let mut values = pairs
        .iter()
        .filter(|(key, _)| key == "platform")
        .map(|(_, value)| value.as_str());
    match (values.next(), values.next()) {
        (Some("mac"), None) => Some("mac"),
        (Some("windows"), None) => Some("windows"),
        _ => None,
    }
}

/// `refresh=true|false`, no other query key.
fn refresh_query(query: Option<&str>) -> Option<bool> {
    let mut refresh = None;
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or_default().as_bytes()) {
        if key != "refresh" || refresh.is_some() {
            return None;
        }
        refresh = Some(match value.as_ref() {
            "true" => true,
            "false" => false,
            _ => return None,
        });
    }
    Some(refresh.unwrap_or(false))
}

fn safe_path<'a>(value: &'a str, platform: &str) -> Option<&'a str> {
    if value.is_empty()
        || value.len() > 4096
        || value.bytes().any(|byte| byte < 0x20 || byte == 0x7f)
    {
        return None;
    }
    let absolute = if platform == "mac" {
        value.starts_with('/')
    } else {
        let bytes = value.as_bytes();
        value.starts_with("\\\\")
            || value.starts_with("//")
            || value.starts_with('/')
            || value.starts_with('\\')
            || (bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'\\' | b'/'))
    };
    absolute.then_some(value)
}

fn windows_history_script(profile_path: &str) -> String {
    let history = profile_path.replace('\'', "''");
    format!(
        "$ErrorActionPreference = 'Stop'; $history = '{history}'; if (-not [IO.File]::Exists($history)) {{ [Console]::Write('{MISSING_HISTORY}'); exit 0 }}; if ((Get-Item -LiteralPath $history).Length -gt {MAX_HISTORY_BYTES}) {{ exit 1 }}; [Console]::Write([IO.File]::ReadAllText($history))"
    )
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// The Codex summary's owner reads. `None` is the fixed failure the route answers as 500.
fn codex_facts(root: &Path) -> Option<Value> {
    let (default, records) = read_codex_profile_registry(root).ok()?;
    let instances = root.join("codex-instances");
    let instances_text = path_text(&instances);
    let mut profiles = Vec::with_capacity(records.len());
    for record in &records {
        let home = instances.join(&record.name);
        let identity = read_auth(&home.join("auth.json"));
        profiles.push(json!({
            "name": record.name,
            "codexHome": path_text(&home),
            "email": identity.email,
            "plan": identity.plan,
            "accountId": identity.account,
            "lastUsed": record.last_used,
            "authValid": identity.valid,
        }));
    }
    let live_home = home_dir().join(".codex");
    let live_auth = live_home.join("auth.json");
    let live_identity = read_auth(&live_auth);
    let activated = match &live_identity.email {
        Some(email) => json!({
            "name": resolve_activated_name(&live_auth, &records, &instances),
            "email": email,
            "plan": live_identity.plan,
            "codexHome": path_text(&live_home),
        }),
        None => Value::Null,
    };
    let names: Vec<&str> = records.iter().map(|record| record.name.as_str()).collect();
    // TS ignores an unregistered `$CCS_CODEX_PROFILE` (one warning) and answers no active
    // profile; passing null keeps that answer without the warning.
    let (codex_home, profile_env) = process_env();
    let profile = profile_env.filter(|profile| names.contains(&profile.as_str()));
    Some(json!({
        "instancesDir": instances_text,
        "env": {"codexHome": codex_home, "profile": profile},
        "registry": {"default": default, "names": names},
        "profiles": profiles,
        "activated": activated,
    }))
}

struct AuthIdentity {
    email: Option<String>,
    plan: Option<String>,
    account: Option<String>,
    valid: bool,
}

/// `decodeAccountIdentity` + the structural-token check, over the bounded read.
fn read_auth(file: &Path) -> AuthIdentity {
    let empty = AuthIdentity {
        email: None,
        plan: None,
        account: None,
        valid: false,
    };
    let aac_renewal::family::AuthBytes::Bytes(bytes) = aac_renewal::family::read_auth_bytes(file)
    else {
        return empty;
    };
    let Some(parsed) = aac_renewal::family::parse_auth_json(&bytes) else {
        return empty;
    };
    let Some(token) = parsed
        .get("tokens")
        .and_then(|tokens| tokens.get("id_token"))
        .and_then(Value::as_str)
        .filter(|token| !token.is_empty())
    else {
        return empty;
    };
    let identity = decode_id_token(token);
    let valid = identity.email.is_some()
        || identity.plan_type.is_some()
        || identity.account_id.is_some()
        || has_structurally_valid_id_token(token);
    AuthIdentity {
        email: identity.email,
        plan: identity.plan_type,
        account: identity.account_id,
        valid,
    }
}

fn activation_binding(file: &Path) -> Option<CodexActivationIdentity> {
    let aac_renewal::family::AuthBytes::Bytes(bytes) = aac_renewal::family::read_auth_bytes(file)
    else {
        return None;
    };
    let parsed = aac_renewal::family::parse_auth_json(&bytes)?;
    let tokens = parsed.get("tokens")?;
    let token = tokens.get("id_token").and_then(Value::as_str)?;
    decode_codex_activation_identity(token, tokens.get("account_id").unwrap_or(&Value::Null))
}

fn resolve_activated_name(
    live_auth: &Path,
    records: &[CodexRegistryProfile],
    instances: &Path,
) -> Option<String> {
    let live = activation_binding(live_auth)?;
    let mut best: Option<&CodexRegistryProfile> = None;
    for record in records {
        let Some(saved) = activation_binding(&instances.join(&record.name).join("auth.json"))
        else {
            continue;
        };
        if !matches_codex_activation_identity(&live, &saved) {
            continue;
        }
        let better = best.is_none_or(|current| {
            record.last_used.as_deref().unwrap_or_default()
                > current.last_used.as_deref().unwrap_or_default()
        });
        if better {
            best = Some(record);
        }
    }
    best.map(|record| record.name.clone())
}

fn process_env() -> (Option<String>, Option<String>) {
    (
        std::env::var("CODEX_HOME").ok(),
        std::env::var("CCS_CODEX_PROFILE").ok(),
    )
}

fn home_dir() -> PathBuf {
    #[cfg(windows)]
    let home = std::env::var_os("USERPROFILE").or_else(|| std::env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    home.map(PathBuf::from).unwrap_or_default()
}

fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The quota answer from the native owners: the registry's profiles, the collector rows the
/// Accounts dashboard already holds, and the renewal status file — composed by the existing
/// Codex quota view, with the ISO facts that view reads.
fn codex_quota_answer(root: &Path, rows: &dyn Fn() -> Vec<Value>, now: i64) -> Option<Value> {
    let (_, records) = read_codex_profile_registry(root).ok()?;
    let profiles: Vec<Value> = records
        .iter()
        .map(|record| {
            let identity = read_auth(
                &root
                    .join("codex-instances")
                    .join(&record.name)
                    .join("auth.json"),
            );
            json!({"name": record.name, "authValid": identity.valid})
        })
        .collect();
    if profiles.is_empty() {
        return aac_views::codex_profiles::quotas(empty_envelope(json!({
            "profiles": [],
            "rows": [],
            "renewals": [],
            "now": now as f64,
        })))
        .ok();
    }
    let dashboard_rows = rows();
    let mut quota_rows = Vec::new();
    let mut epochs = Map::new();
    for profile in &profiles {
        let name = profile["name"].as_str().unwrap_or_default();
        let Some(row) = dashboard_rows.iter().find(|row| {
            row.get("capabilities")
                .and_then(|caps| caps.get("codexProfile"))
                .and_then(Value::as_str)
                == Some(name)
        }) else {
            continue;
        };
        let status = row["status"].as_str().unwrap_or("unavailable");
        let (quota_status, needs_reauth) = match status {
            "ok" | "cached" => ("ok", false),
            "needs_sign_in" | "reauth_required" => ("error", true),
            _ => ("error", false),
        };
        for field in ["fetchedAt", "sampledAt"] {
            if let Some(text) = row[field].as_str() {
                remember_instant(&mut epochs, text);
            }
        }
        let mut windows = Vec::new();
        for window in row["windows"].as_array().into_iter().flatten() {
            let key = window["key"].as_str().unwrap_or_default();
            let label = match key {
                "five_hour" => "5-hour limit",
                "seven_day" => "Weekly limit",
                _ => window["label"].as_str().unwrap_or("Usage"),
            };
            let reset = window["resetAt"].as_str();
            if let Some(text) = reset {
                remember_instant(&mut epochs, text);
            }
            windows.push(json!({
                "key": key,
                "label": label,
                "usedPercent": window["usedPercent"],
                "resetAt": reset,
            }));
        }
        quota_rows.push(json!({
            "profile": name,
            "quotaStatus": quota_status,
            "needsReauth": needs_reauth,
            "fetchedAt": row["fetchedAt"],
            "staleAsOf": row["sampledAt"],
            "quotaWindows": windows,
        }));
    }
    let renewals = renewal_entries(root, now);
    let data = json!({
        "profiles": profiles,
        "rows": quota_rows,
        "renewals": renewals,
        "now": now as f64,
    });
    let envelope = serde_json::from_value(json!({
        "data": data,
        "presentation": {"dates": {}, "epochs": epochs, "order": {}},
    }))
    .ok()?;
    aac_views::codex_profiles::quotas(envelope).ok()
}

fn empty_envelope(data: Value) -> Envelope {
    serde_json::from_value(json!({
        "data": data,
        "presentation": {"dates": {}, "epochs": {}, "order": {}},
    }))
    .expect("empty presentation facts parse")
}

fn remember_instant(epochs: &mut Map<String, Value>, text: &str) {
    if let Some(ms) = aac_num::ecma::parse_instant(text) {
        epochs.insert(text.to_owned(), json!(ms));
    }
}

/// The renewal notes for the profiles the dashboard shows, read from the status store only.
fn renewal_entries(root: &Path, now: i64) -> Vec<Value> {
    let options = aac_renewal::renewal::CodexProfileRenewalOptions {
        env: aac_renewal::planner::CodexRenewalEnvironment {
            ccs_dir: root.to_path_buf(),
            codex_home: Some(home_dir().join(".codex")),
            home_dir: Some(home_dir()),
            ..Default::default()
        },
        now: Arc::new(move || now),
        ..Default::default()
    };
    match aac_renewal::renewal::get_codex_profile_renewal_status(&options) {
        Ok(status) => status
            .profiles
            .iter()
            .map(|profile| {
                json!([
                    profile.name,
                    {"state": profile.state.word(), "message": profile.message}
                ])
            })
            .collect(),
        Err(()) => Vec::new(),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_route_shapes_match_the_retired_router() {
        assert!(Profiles::owns(
            &Method::GET,
            "/api/claude/desktop-profiles/"
        ));
        assert!(Profiles::owns(&Method::GET, "/api/antigravity/profiles"));
        assert!(Profiles::owns(
            &Method::POST,
            "/api/claude/desktop-profiles/work/open"
        ));
        assert!(!Profiles::owns(
            &Method::POST,
            "/api/claude/desktop-profiles/open"
        ));
        assert!(!Profiles::owns(&Method::GET, "/api/codex/profiles/x"));
        assert_eq!(
            open_id("/api/claude/desktop-profiles/work/open").as_deref(),
            Some("work")
        );
        assert_eq!(open_id("/api/claude/desktop-profiles/open"), None);
    }

    #[test]
    fn queries_keep_the_typescript_rules() {
        assert_eq!(usage_platform(Some("platform=mac")), Some("mac"));
        assert_eq!(
            usage_platform(Some("platform=windows&other=1")),
            Some("windows")
        );
        assert_eq!(usage_platform(Some("platform=linux")), None);
        assert_eq!(usage_platform(Some("platform=mac&platform=mac")), None);
        assert_eq!(refresh_query(Some("refresh=true")), Some(true));
        assert_eq!(refresh_query(Some("")), Some(false));
        assert_eq!(refresh_query(Some("refresh=yes")), None);
        assert_eq!(refresh_query(Some("refresh=true&x=1")), None);
    }

    #[test]
    fn the_history_script_is_the_reference_text() {
        assert!(windows_history_script(r"C:\P").contains("$history = 'C:\\P'"));
        assert!(windows_history_script("C:'P").contains("C:''P"));
        assert_eq!(utf16le("a"), vec![97, 0]);
    }

    #[tokio::test]
    async fn remote_codex_profile_reads_keep_the_localhost_guard_when_auth_is_disabled() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().to_owned();
        let agents = Arc::new(crate::agent_transport::Agents::with_default(
            root.clone(),
            false,
        ));
        let profiles = Profiles::start(root, Arc::new(Vec::new), agents);
        let access = DashboardAccess {
            auth_enabled: false,
            principal: DashboardPrincipal::Anonymous,
            secure_transport: false,
            same_origin: true,
            origin_present: false,
            local_access: false,
            session_id: None,
            credential_transport: "test",
            direct_loopback: false,
            lan_proxy: false,
            client_address: None,
        };
        for path in ["/api/codex/profiles", "/api/codex/profiles/quotas"] {
            let request = Request::builder()
                .uri(path)
                .extension(access.clone())
                .body(axum::body::Body::empty())
                .unwrap();
            let response = profiles.handle(request).await;
            assert_eq!(response.status(), 403, "{path}");
            let bytes = axum::body::to_bytes(response.into_body(), 4096)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                json!({
                    "error": "Codex auth profiles endpoint requires localhost access when dashboard auth is disabled."
                }),
                "{path}"
            );
        }
    }

    #[tokio::test]
    async fn history_cleanup_checks_origin_and_json_and_reports_a_profile_listing_failure() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().to_owned();
        let manifest = root.join("claude-desktop-profiles.json");
        std::fs::write(&manifest, b"{").unwrap();
        let agents = Arc::new(crate::agent_transport::Agents::with_default(
            root.clone(),
            false,
        ));
        let profiles = Profiles::start(root, Arc::new(Vec::new), agents);
        let access = DashboardAccess {
            auth_enabled: true,
            principal: DashboardPrincipal::Browser {
                username: "fixture-admin".into(),
            },
            secure_transport: true,
            same_origin: true,
            origin_present: true,
            local_access: true,
            session_id: None,
            credential_transport: "test",
            direct_loopback: true,
            lan_proxy: false,
            client_address: None,
        };
        let request = |mime| {
            Request::builder()
                .method(Method::POST)
                .uri("/api/claude/history-snapshots/cleanup")
                .header("content-type", mime)
                .body(axum::body::Body::from("{}"))
                .unwrap()
        };
        let mut foreign = access.clone();
        foreign.same_origin = false;
        let response = profiles
            .cleanup(&foreign, request("application/json"))
            .await;
        assert_eq!(response.status(), 403);
        let response = profiles.cleanup(&access, request("text/plain")).await;
        assert_eq!(response.status(), 415);
        let response = profiles.cleanup(&access, request("application/json")).await;
        assert_eq!(response.status(), 500);
        let bytes = axum::body::to_bytes(response.into_body(), 4096)
            .await
            .unwrap();
        let body: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            body["error"],
            "History snapshots could not be cleaned safely."
        );
        assert_eq!(std::fs::read(manifest).unwrap(), b"{");
    }
}
