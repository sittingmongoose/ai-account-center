//! The worker-removal routes the Rust front door answers itself, without the TypeScript
//! adapters: the Status view, its answer history and answer sets, card media, deliverables,
//! the store status and the history store's reads.
//!
//! The front door owns public admission (session, origin, device scope, JSON, body and query
//! limits), the guards and the HTTP encoding. The owners stay where they are: every route is
//! asked of the private service through [`super::owner_host`] with the verified principal, so
//! the share readers, the answer journal, the store host and its collectors keep one owner and
//! one writer. A boot without the matching owned group answers the honest decline
//! ([`super::decline`]), never a second implementation of the owner.
//!
//! Encodings follow the TypeScript routes byte for byte: the fixed `{error, code}` sentences,
//! `cache-control`, `vary`, the feed views' ETag and 304, and the file envelope's validated
//! headers and lengths.

use super::{DashboardAccess, DashboardPrincipal, Reply, owner_host::ServiceReply, set_header};
use aac_views::decline::Decline;
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{Method, StatusCode},
    response::IntoResponse,
};
use serde_json::{Value as Json, json};
use std::path::Path;
use tokio::io::AsyncReadExt;

/// This group's admission and encoding, over the one owner host.
pub(super) struct Routes {
    host: std::sync::Arc<super::owner_host::OwnerHost>,
    cut: bool,
    legacy_switch: bool,
}

impl Routes {
    pub(super) fn new(
        host: std::sync::Arc<super::owner_host::OwnerHost>,
        cut: bool,
        legacy_switch: bool,
    ) -> Self {
        Self {
            host,
            cut,
            legacy_switch,
        }
    }

    /// The migrated groups use their Rust owners on cut boots.
    pub(super) fn owns(&self, method: &Method, path: &str) -> bool {
        (self.cut || self.legacy_switch) && owns_route(method, path)
    }

    fn root(&self) -> &Path {
        self.host.root()
    }

    async fn hosted(&self) -> bool {
        self.host.ready().await
    }

    /// The in-process owners on a platform without the private service (Windows).
    async fn local(&self) -> Option<std::sync::Arc<crate::owners::Owners>> {
        self.host.local().await
    }

    async fn request(
        &self,
        method: &str,
        uri: &str,
        principal: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Option<ServiceReply> {
        self.host
            .request(method, uri, principal, headers, body)
            .await
    }
}

/// The reply's JSON body, when it is one; the service's routes answer JSON.
fn json_of(reply: &ServiceReply) -> Option<Json> {
    serde_json::from_slice(&reply.body).ok()
}

/// `true` when the service named a decline (`x-aac-decline`): no owner for this route.
fn declined(reply: &ServiceReply) -> bool {
    reply.status == StatusCode::SERVICE_UNAVAILABLE.as_u16()
        && reply.headers.contains_key("x-aac-decline")
}

/// The named reason of a decline (`stale`, `unsupported`, ...).
fn decline_reason(reply: &ServiceReply) -> Option<&str> {
    reply
        .headers
        .get("x-aac-decline")
        .and_then(|value| value.to_str().ok())
}

/// A named decline: the M0 contract's answer, with the reason in the log (the TypeScript path
/// answered from the same read; a worker-less boot has nobody else).
fn decline_because(reason: &'static str) -> Reply {
    tracing::warn!(reason, "native group route declined");
    // The CLI dashboard has no tracing subscriber; its stderr is the operator's log.
    eprintln!("aac-http: native group route declined ({reason})");
    super::decline()
}

/// `MAX_STATUS_BODY_BYTES`, the answer set's own limit.
const ANSWER_BODY_CAP: usize = 64 * 1024;
const INVALID_BODY: (u16, &str, &str) = (
    400,
    "invalid_body",
    "The request body is not valid for this action.",
);
const UNEXPECTED_QUERY_STATUS: &str = "This request does not take a query string.";
const ORIGIN_REQUIRED: &str = "This change requires the dashboard origin.";
const JSON_REQUIRED: &str = "This change requires application/json.";
const BODY_TOO_LARGE: &str = "The request body is too large.";

/// `true` for every path this module answers (the caller then never forwards the request).
fn owns_route(method: &Method, path: &str) -> bool {
    let path = path.strip_suffix('/').unwrap_or(path);
    if path == "/api/status/deliverable" {
        return matches!(
            *method,
            Method::GET
                | Method::HEAD
                | Method::POST
                | Method::PUT
                | Method::PATCH
                | Method::DELETE
                | Method::OPTIONS
        );
    }
    if path == "/api/status/answers" && method == Method::POST {
        return true;
    }
    if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return false;
    }
    path == "/api/status"
        || path == "/api/status/answers"
        || path == "/api/scoreboard"
        || store_path(path)
        || media(path).is_some()
}

fn store_path(path: &str) -> bool {
    matches!(
        path,
        "/api/store/status" | "/api/usage/history" | "/api/usage/at" | "/api/usage/burn"
    )
}

/// `(writer, relative path)` of a card-media route, with the reference's path shapes.
fn media(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("/api/status/files/")?;
    let (writer, relative) = rest.split_once('/')?;
    (!writer.is_empty() && !relative.is_empty() && !writer.contains('/'))
        .then_some((writer, relative))
}

/// The few request facts the routes read, taken before the request is consumed.
struct Facts {
    head: bool,
    accept_gzip: bool,
    origin_present: bool,
    same_origin: bool,
    content_type: Option<String>,
    content_length: Option<usize>,
    range: Option<String>,
    if_none_match: Option<String>,
    sec_fetch_site: Option<String>,
    user_agent: Option<String>,
}

impl Facts {
    fn of(access: &DashboardAccess, req: &Request) -> Self {
        let text = |name: &str| {
            req.headers()
                .get(name)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned)
        };
        Self {
            head: req.method() == Method::HEAD,
            accept_gzip: crate::compress::accepts_gzip(req.headers()),
            origin_present: access.origin_present,
            same_origin: access.same_origin,
            content_type: text("content-type"),
            content_length: text("content-length").and_then(|value| value.parse().ok()),
            range: text("range"),
            if_none_match: text("if-none-match"),
            sec_fetch_site: text("sec-fetch-site"),
            user_agent: text("user-agent"),
        }
    }
}

fn no_store(mut reply: Reply) -> Reply {
    set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn json_reply(status: u16, value: &Json) -> Reply {
    super::json_bytes(status, value.to_string().into_bytes())
}

/// The Status tab's routes: a signed-in browser only (`requireBrowserSession`).
fn browser_only(access: &DashboardAccess) -> Option<Reply> {
    match &access.principal {
        DashboardPrincipal::Browser { .. } => None,
        DashboardPrincipal::Device { .. } => Some(no_store(super::error(
            403,
            "device_scope",
            "This page needs a signed-in dashboard session.",
        ))),
        DashboardPrincipal::Anonymous if access.auth_enabled => Some(no_store(super::error(
            401,
            "auth_required",
            "Authentication required",
        ))),
        DashboardPrincipal::Anonymous => Some(no_store(super::error(
            403,
            "signin_required",
            "These tabs need dashboard sign-in turned on.",
        ))),
    }
}

/// The history store's reads: a session from the dashboard origin, or a paired device token.
fn store_read(access: &DashboardAccess, facts: &Facts) -> Option<Reply> {
    match &access.principal {
        DashboardPrincipal::Anonymous => {
            return Some(no_store(super::error(
                401,
                "auth_required",
                "Authentication required",
            )));
        }
        DashboardPrincipal::Browser { .. } => {
            let cross_site = facts
                .sec_fetch_site
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case("cross-site"));
            if !facts.same_origin || cross_site {
                return Some(no_store(super::error(
                    403,
                    "origin_required",
                    "The history store requires the dashboard origin.",
                )));
            }
        }
        DashboardPrincipal::Device { .. } => {}
    }
    None
}

fn invalid_query() -> Reply {
    no_store(super::error(
        400,
        "invalid_query",
        "The query is not valid for this route.",
    ))
}

fn store_unavailable() -> Reply {
    no_store(super::error(
        503,
        "store_unavailable",
        "The history store is not available on this computer.",
    ))
}

fn store_failed() -> Reply {
    no_store(super::error(
        503,
        "store_failed",
        "The history store could not answer.",
    ))
}

fn store_json(value: &Json) -> Reply {
    no_store(super::json_bytes(200, aac_views::js_json::write(value)))
}

fn unavailable_status(root: &Path) -> Reply {
    let reason = if crate::store_host::disabled_for_public(root) {
        "disabled"
    } else {
        "missing"
    };
    store_json(&json!({"state": "unavailable", "reason": reason}))
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// The history store's answer for one read the front door admitted: the body exactly, or the
/// route's `store_unavailable` when the service hosts no store.
async fn store_route(
    routes: &Routes,
    access: &DashboardAccess,
    path: &str,
    raw: Option<&str>,
) -> Reply {
    if crate::store_host::disabled() {
        return store_unavailable();
    }
    if let Some(owners) = routes.local().await {
        let Some(store) = owners.store.as_ref() else {
            return store_unavailable();
        };
        // Still opening is a retryable failure, never "no store on this computer".
        match store.usage_read_ready().await {
            Ok(()) => {}
            Err(Decline::Busy) => return store_failed(),
            Err(_) => return store_unavailable(),
        }
        let now = now_ms();
        let answer = match path {
            "/api/usage/history" => crate::usage_routes::history(store, raw, now).await,
            "/api/usage/at" => crate::usage_routes::at(store, raw, now).await,
            _ => crate::usage_routes::burn(store, raw, now).await,
        };
        return match answer {
            Ok(value) => store_json(&value),
            Err(_) => store_failed(),
        };
    }
    let Some(principal) = access.service_principal() else {
        return store_unavailable();
    };
    if !routes.hosted().await {
        return store_unavailable();
    }
    let uri = match raw {
        Some(raw) => format!("{path}?{raw}"),
        None => path.to_owned(),
    };
    match routes.request("GET", &uri, &principal, &[], None).await {
        Some(reply) if decline_reason(&reply) == Some("unsupported") => store_unavailable(),
        Some(reply) if reply.status == 200 => match json_of(&reply) {
            Some(value) => store_json(&value),
            None => store_failed(),
        },
        Some(_) => store_failed(),
        None => store_unavailable(),
    }
}

pub(super) async fn handle(routes: &Routes, access: &DashboardAccess, req: Request) -> Reply {
    let path = req.uri().path().to_owned();
    let path = path.strip_suffix('/').unwrap_or(&path).to_owned();
    let raw = req.uri().query().map(str::to_owned);
    let raw = raw.as_deref();
    let facts = Facts::of(access, &req);
    if req.method() == Method::OPTIONS && path != "/api/status/deliverable" {
        if store_path(&path) {
            return super::lifecycle::options("GET, HEAD", "9");
        }
        return browser_only(access).unwrap_or_else(|| {
            no_store(if path == "/api/status/answers" {
                super::lifecycle::options("GET, HEAD, POST", "15")
            } else {
                super::lifecycle::options("GET, HEAD", "9")
            })
        });
    }
    match path.as_str() {
        "/api/store/status" => {
            if let Some(reply) = store_read(access, &facts) {
                return reply;
            }
            if raw.is_some_and(|query| query.split('&').any(|pair| !pair.is_empty())) {
                return invalid_query();
            }
            super::revalidate_json(store_status(routes, access).await, req.headers())
        }
        "/api/usage/history" | "/api/usage/at" | "/api/usage/burn" => {
            if let Some(reply) = store_read(access, &facts) {
                return reply;
            }
            let valid = match path.as_str() {
                "/api/usage/history" => crate::usage_routes::valid_history(raw, now_ms()),
                "/api/usage/at" => crate::usage_routes::valid_at(raw),
                _ => crate::usage_routes::valid_burn(raw),
            };
            if !valid {
                return invalid_query();
            }
            super::revalidate_json(store_route(routes, access, &path, raw).await, req.headers())
        }
        "/api/status/answers" if req.method() == Method::POST => {
            answers_post(routes, access, req, &facts, raw).await
        }
        "/api/status/answers" => answers_read(routes, access, raw).await,
        "/api/status" => status(routes, access, raw, &facts).await,
        "/api/scoreboard" => scoreboard(routes, access, raw, &facts).await,
        "/api/status/deliverable" => deliverable(access, req, &facts, raw).await,
        _ => match media(&path) {
            Some((writer, relative)) => card_media(access, &facts, writer, relative, raw).await,
            None => super::decline(),
        },
    }
}

/// The service declined busy: the store is still opening. That is a retryable failure, never
/// "unavailable/missing", which would say there is no store.
fn store_still_opening(reply: &ServiceReply) -> bool {
    declined(reply) && decline_reason(reply) == Some("busy")
}

/// `GET /api/store/status`: the hosted store's writer status, or the fallback's honest
/// "unavailable" (never an invented one).
async fn store_status(routes: &Routes, access: &DashboardAccess) -> Reply {
    let unavailable = || unavailable_status(routes.root());
    if crate::store_host::disabled() {
        return unavailable();
    }
    if let Some(owners) = routes.local().await {
        let Some(store) = owners.store.clone() else {
            return unavailable();
        };
        return match tokio::task::spawn_blocking(move || store.status_value()).await {
            Ok(Ok(value)) => store_json(&value),
            Ok(Err(Decline::Unsupported)) => unavailable(),
            _ => store_failed(),
        };
    }
    let Some(principal) = access.service_principal() else {
        return unavailable();
    };
    if !routes.hosted().await {
        return unavailable();
    }
    let reply = routes
        .request("GET", "/api/store/status", &principal, &[], None)
        .await;
    if reply.as_ref().is_some_and(store_still_opening) {
        return store_failed();
    }
    match reply {
        Some(reply) if declined(&reply) => unavailable(),
        Some(reply) if reply.status == 200 => match json_of(&reply) {
            Some(value)
                if matches!(
                    value.get("state").and_then(Json::as_str),
                    Some("ready" | "degraded" | "unavailable")
                ) =>
            {
                store_json(&value)
            }
            _ => store_failed(),
        },
        Some(_) => store_failed(),
        None => store_failed(),
    }
}

/// `GET /api/status/answers`.
async fn answers_read(routes: &Routes, access: &DashboardAccess, raw: Option<&str>) -> Reply {
    const UNAVAILABLE: &str = "The answer history can't be read right now.";
    if let Some(reply) = browser_only(access) {
        return reply;
    }
    let Some(limit) = crate::status_answers_route::read_limit(raw) else {
        let only_limit = raw.is_some_and(|query| {
            let mut pairs = url::form_urlencoded::parse(query.as_bytes());
            pairs.next().is_some_and(|(name, _)| name == "limit") && pairs.next().is_none()
        });
        return no_store(if only_limit {
            super::error(
                400,
                "invalid_limit",
                "limit must be a whole number from 1 to 500.",
            )
        } else {
            super::error(400, "unexpected_query", "Only limit is accepted here.")
        });
    };
    if let Some(owners) = routes.local().await {
        let Some(produce) = owners.status_produce.as_ref() else {
            return no_store(super::error(503, "answers_unavailable", UNAVAILABLE));
        };
        return no_store(json_reply(200, &produce.answers_view(limit)));
    }
    let Some(principal) = access.service_principal() else {
        return no_store(super::error(503, "answers_unavailable", UNAVAILABLE));
    };
    if !routes.hosted().await {
        return no_store(super::error(503, "answers_unavailable", UNAVAILABLE));
    }
    let uri = format!("/api/status/answers?limit={limit}");
    match routes.request("GET", &uri, &principal, &[], None).await {
        Some(reply) if reply.status == 200 => match json_of(&reply) {
            Some(value) if value.get("answers").is_some() => no_store(json_reply(200, &value)),
            _ => super::decline(),
        },
        Some(reply) if declined(&reply) => super::decline(),
        _ => no_store(super::error(503, "answers_unavailable", UNAVAILABLE)),
    }
}

/// `POST /api/status/answers`: the route's guards, then the owner's own record path.
async fn answers_post(
    routes: &Routes,
    access: &DashboardAccess,
    req: Request,
    facts: &Facts,
    raw: Option<&str>,
) -> Reply {
    if let Some(reply) = browser_only(access) {
        return reply;
    }
    if raw.is_some() {
        return no_store(super::error(
            400,
            "unexpected_query",
            UNEXPECTED_QUERY_STATUS,
        ));
    }
    if !facts.origin_present || !facts.same_origin {
        return no_store(super::error(403, "origin_required", ORIGIN_REQUIRED));
    }
    let json_content = facts
        .content_type
        .as_deref()
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
    if !json_content {
        return no_store(super::error(415, "json_required", JSON_REQUIRED));
    }
    if facts
        .content_length
        .is_some_and(|length| length > ANSWER_BODY_CAP)
    {
        return no_store(super::error(413, "body_too_large", BODY_TOO_LARGE));
    }
    let content_type = facts.content_type.clone().unwrap_or_default();
    let body = match to_bytes(req.into_body(), ANSWER_BODY_CAP).await {
        Ok(body) => body,
        Err(_) => return no_store(super::error(413, "body_too_large", BODY_TOO_LARGE)),
    };
    let invalid = || no_store(super::error(INVALID_BODY.0, INVALID_BODY.1, INVALID_BODY.2));
    let Ok(value) = serde_json::from_slice::<Json>(&body) else {
        return invalid();
    };
    if !crate::status_answers_route::valid_send_body(&value) {
        return invalid();
    }
    if let Some(owners) = routes.local().await {
        let Some(answer_store) = owners.answers_owned.as_ref() else {
            return super::decline();
        };
        let device = crate::status_answers_route::device_of(facts.user_agent.as_deref());
        let Ok(submit) = crate::status_answers_route::submit_body(&value, device) else {
            return invalid();
        };
        return match answer_store.submit(submit.to_string().as_bytes()).await {
            Ok(reply) => {
                if let Some(produce) = &owners.status_produce {
                    produce.mark_answers();
                }
                no_store(json_reply(200, &reply))
            }
            Err("shape") => invalid(),
            Err(_) => no_store(super::error(
                500,
                "answers_failed",
                "The answers could not be finished. Some may have been recorded; reload before sending again.",
            )),
        };
    }
    let Some(principal) = access.service_principal() else {
        return super::decline();
    };
    if !routes.hosted().await {
        return super::decline();
    }
    match routes
        .request(
            "POST",
            "/api/status/answers",
            &principal,
            &[
                ("content-type", content_type.as_str()),
                (
                    "user-agent",
                    facts.user_agent.as_deref().unwrap_or_default(),
                ),
            ],
            Some(&body),
        )
        .await
    {
        Some(reply) if reply.status == 200 => match json_of(&reply) {
            Some(value) if value.get("results").is_some() => no_store(json_reply(200, &value)),
            _ => super::decline(),
        },
        Some(reply) if declined(&reply) => super::decline(),
        _ => no_store(super::error(
            500,
            "answers_failed",
            "The answers could not be finished. Some may have been recorded; reload before sending again.",
        )),
    }
}

/// The display zone the Status producers format in: the saved preference, else the reference's
/// default. A zone the database does not hold declines (the reference's `Intl` throw).
fn status_zone(root: &Path) -> Option<(String, crate::zone_facts::ZoneOffsets)> {
    // The preferences reader answers the saved zone; an absent file is the reference's default
    // (the TypeScript reader falls back the same way), never a missing zone.
    let now = i64::try_from(now_ms()).ok()?;
    crate::zone_facts::status_zone(root, now)
}

/// `GET /api/status`: the service's own fed generations, with this request's clock and the
/// display zone's offsets (the one fact the service cannot reproduce from its files).
async fn status(
    routes: &Routes,
    access: &DashboardAccess,
    raw: Option<&str>,
    facts: &Facts,
) -> Reply {
    if let Some(reply) = browser_only(access) {
        return reply;
    }
    if raw.is_some() {
        return no_store(super::error(
            400,
            "unexpected_query",
            UNEXPECTED_QUERY_STATUS,
        ));
    }
    let Some((time_zone, offsets)) = status_zone(routes.root()) else {
        return decline_because("zone");
    };
    if let Some(owners) = routes.local().await {
        let Some(produce) = owners.status_produce.as_ref() else {
            return decline_because("status");
        };
        let zone = aac_status::produce::facts::ZoneFacts {
            from: offsets.from,
            to: offsets.to,
            offsets: offsets.offsets.clone(),
        };
        let want_gzip = !facts.head && facts.accept_gzip;
        return match produce
            .view_async_encoded(
                None,
                now_ms(),
                &time_zone,
                &zone,
                want_gzip,
                facts.if_none_match.as_deref(),
            )
            .await
        {
            Ok(view) => fed_encoded(view, "status@1"),
            Err(reason) => {
                eprintln!("aac-http: local Status view declined ({reason:?})");
                decline_because("status")
            }
        };
    }
    let now = now_ms().to_string();
    let zone = json!({
        "timeZone": time_zone,
        "from": offsets.from,
        "to": offsets.to,
        "offsets": offsets.offsets,
    })
    .to_string();
    let mut headers: Vec<(&str, &str)> = vec![
        ("x-aac-owned", "1"),
        ("x-aac-now", now.as_str()),
        ("x-aac-zone", zone.as_str()),
    ];
    if !facts.head && facts.accept_gzip {
        headers.push((crate::compress::ACCEPT_GZIP_HEADER, "1"));
    }
    if let Some(inm) = facts.if_none_match.as_deref() {
        headers.push(("if-none-match", inm));
    }
    let Some(principal) = access.service_principal() else {
        return decline_because("principal");
    };
    if !routes.hosted().await {
        return decline_because("service");
    }
    // The first owned Status request can beat the service's first report scan; retry a cold
    // `unsupported`/`stale`/`busy` for a moment instead of declining to nobody.
    for attempt in 0..6 {
        match routes
            .request("GET", "/api/status", &principal, &headers, None)
            .await
        {
            Some(reply) if reply.status == 200 || reply.status == 304 => {
                return fed_view(reply, facts.if_none_match.as_deref());
            }
            Some(reply) if declined(&reply) => {
                let reason = decline_reason(&reply).unwrap_or("");
                tracing::warn!(reason, "the owned Status read declined");
                eprintln!("aac-http: owned Status read declined ({reason})");
                if !matches!(reason, "unsupported" | "stale" | "busy" | "deadline") {
                    return decline_because("status");
                }
                if attempt < 5 {
                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                    continue;
                }
                return decline_because("status");
            }
            _ => return decline_because("status"),
        }
    }
    decline_because("status")
}

struct ScoreboardQuery {
    from: Option<String>,
    to: Option<String>,
    source: String,
    task_type: Option<String>,
}

const FILTER_KEYS: [&str; 4] = ["from", "to", "source", "taskType"];
const FILTER_UNEXPECTED: &str = "This request takes only from, to, source and taskType.";
const FILTER_ONCE: &str = "Each filter may be given once.";
const FILTER_DAY: &str = "Dates are days written YYYY-MM-DD.";
const FILTER_ORDER: &str = "The first day is after the last day.";
const FILTER_SOURCE: &str = "Source is all, real or benchmark.";
const FILTER_TASK: &str = "The task type is not a task type id.";

fn query_decode(value: &str) -> Option<String> {
    percent_decode(&value.replace('+', " "))
}

/// The route's own filter contract (`parseScoreboardQuery`).
fn scoreboard_query(raw: Option<&str>) -> Result<ScoreboardQuery, (&'static str, &'static str)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for part in raw
        .unwrap_or_default()
        .split('&')
        .filter(|part| !part.is_empty())
    {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        let key = query_decode(key).ok_or(("unexpected_query", FILTER_UNEXPECTED))?;
        if !FILTER_KEYS.contains(&key.as_str()) {
            return Err(("unexpected_query", FILTER_UNEXPECTED));
        }
        if pairs.iter().any(|(held, _)| *held == key) {
            return Err(("invalid_filter", FILTER_ONCE));
        }
        let value = query_decode(value).ok_or(("invalid_filter", FILTER_DAY))?;
        pairs.push((key, value));
    }
    let get = |key: &str| {
        pairs
            .iter()
            .find(|(held, _)| held == key)
            .map(|(_, value)| value.clone())
    };
    let from = get("from");
    let to = get("to");
    for day in [from.as_deref(), to.as_deref()].into_iter().flatten() {
        if !crate::zone_facts::is_calendar_date(day) {
            return Err(("invalid_filter", FILTER_DAY));
        }
    }
    if let (Some(first), Some(last)) = (from.as_deref(), to.as_deref())
        && first > last
    {
        return Err(("invalid_filter", FILTER_ORDER));
    }
    let source = get("source").unwrap_or_else(|| "all".to_owned());
    if !matches!(source.as_str(), "all" | "real" | "benchmark") {
        return Err(("invalid_filter", FILTER_SOURCE));
    }
    let task_type = get("taskType");
    if let Some(task) = task_type.as_deref() {
        let valid = (1..=64).contains(&task.len())
            && task.as_bytes()[0].is_ascii_alphanumeric()
            && task
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte));
        if !valid {
            return Err(("invalid_filter", FILTER_TASK));
        }
    }
    Ok(ScoreboardQuery {
        from,
        to,
        source,
        task_type,
    })
}

/// `GET /api/scoreboard`: the filters, the snapshot the service holds (its facts stay in
/// the service), and the owned answer (bytes, ETag, view).
async fn scoreboard(
    routes: &Routes,
    access: &DashboardAccess,
    raw: Option<&str>,
    facts: &Facts,
) -> Reply {
    if let Some(reply) = browser_only(access) {
        return reply;
    }
    let query = match scoreboard_query(raw) {
        Ok(query) => query,
        Err((code, error)) => return no_store(super::error(400, code, error)),
    };
    let Some(principal) = access.service_principal() else {
        return decline_because("principal");
    };
    let local = routes
        .local()
        .await
        .and_then(|owners| owners.scoreboard_owned.clone());
    if local.is_none() && !routes.hosted().await {
        return decline_because("service");
    }
    let Some((time_zone, offsets)) = status_zone(routes.root()) else {
        return decline_because("zone");
    };
    let now = now_ms();
    let Some((clock, day)) = offsets.zone.words(i64::try_from(now).unwrap_or(0)) else {
        return decline_because("words");
    };
    let from_ms = match query.from.as_deref() {
        Some(day) => match crate::zone_facts::local_midnight(day, &time_zone) {
            Some(ms) => Json::from(ms),
            None => return decline_because("from"),
        },
        None => Json::Null,
    };
    let to_ms = match query.to.as_deref() {
        Some(day) => {
            let Some(next) = crate::zone_facts::next_day(day) else {
                return decline_because("to");
            };
            match crate::zone_facts::local_midnight(&next, &time_zone) {
                Some(ms) => Json::from(ms - 1000),
                None => return decline_because("to"),
            }
        }
        None => Json::Null,
    };
    // The feed names no snapshot: the service answers for the snapshot it holds and keeps
    // its facts there, so the inputs body (the whole run set, past the service link's
    // reply cap at real size) never crosses to this door. `stale` means the snapshot
    // moved under the compose (retry reads the new one); `busy` and `deadline` mean a
    // compose is still running and filling the owner's view cache. A cold boot's first
    // seed, or a store that is off or lock-held while the share scan starts, can take a
    // moment: retry briefly, then answer honestly.
    let feed = json!({
        "now": now,
        "clock": clock,
        "day": day,
        "timeZone": time_zone,
        "from": query.from,
        "to": query.to,
        "source": query.source,
        "taskType": query.task_type,
        "fromMs": from_ms,
        "toMs": to_ms,
    })
    .to_string();
    let want_gzip = !facts.head && facts.accept_gzip;
    let mut headers: Vec<(&str, &str)> = vec![("x-aac-feed", feed.as_str())];
    if want_gzip {
        headers.push((crate::compress::ACCEPT_GZIP_HEADER, "1"));
    }
    if let Some(inm) = facts.if_none_match.as_deref() {
        headers.push(("if-none-match", inm));
    }
    for attempt in 0..6 {
        let reason = if let Some(owner) = local.as_deref() {
            match owner
                .answer_encoded(&feed, want_gzip, facts.if_none_match.as_deref())
                .await
            {
                Ok(view) => return fed_encoded(view, "scoreboard@1"),
                Err(reason) => Some(reason),
            }
        } else {
            match routes
                .request("GET", "/api/scoreboard", &principal, &headers, None)
                .await
            {
                Some(reply) if reply.status == 200 || reply.status == 304 => {
                    return fed_view(reply, facts.if_none_match.as_deref());
                }
                Some(reply) if declined(&reply) => Decline::ALL
                    .into_iter()
                    .find(|reason| Some(reason.as_str()) == decline_reason(&reply)),
                Some(reply) => {
                    eprintln!(
                        "aac-http: scoreboard answer replied {} without a decline",
                        reply.status
                    );
                    return decline_because("answer");
                }
                None => {
                    eprintln!("aac-http: scoreboard answer got no reply from the service");
                    return decline_because("answer");
                }
            }
        };
        match reason {
            Some(Decline::Stale | Decline::Busy | Decline::Deadline) if attempt < 5 => {
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            Some(retryable @ (Decline::Stale | Decline::Busy | Decline::Deadline)) => {
                return decline_because(retryable.as_str());
            }
            Some(Decline::Unsupported) => return decline_because("unsupported"),
            Some(Decline::Shape) => return decline_because("shape"),
            Some(other) => return decline_because(other.as_str()),
            None => return decline_because("declined"),
        }
    }
    decline_because("answer")
}

fn valid_fed_meta(view: &str, etag: &str) -> bool {
    matches!(view, "status@1" | "scoreboard@1" | "home@3" | "dashboard@1")
        && etag.starts_with("W/\"")
        && etag.ends_with('"')
        && etag.len() >= 40
        && etag[3..etag.len() - 1]
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
}

fn fed_encoded(view: crate::view_reply::EncodedView, view_name: &str) -> Reply {
    match view {
        crate::view_reply::EncodedView::NotModified { etag } => {
            if !valid_fed_meta(view_name, &etag) {
                eprintln!("aac-http: fed view refused: {view_name} {etag}");
                return super::decline();
            }
            let mut out = super::empty(304);
            set_header(&mut out, "cache-control", "private, no-cache");
            set_header(&mut out, "etag", &etag);
            set_header(&mut out, "x-aac-view", view_name);
            set_header(&mut out, "vary", "Cookie, Accept-Encoding");
            out
        }
        crate::view_reply::EncodedView::Payload {
            bytes,
            raw_len,
            gzip,
            etag,
        } => fed_shared_bytes(bytes, raw_len, gzip, &etag, view_name),
    }
}

fn fed_shared_bytes(
    body: axum::body::Bytes,
    raw_len: usize,
    gzip: bool,
    etag: &str,
    view: &str,
) -> Reply {
    if !valid_fed_meta(view, etag) {
        eprintln!("aac-http: fed view refused: {view} {etag}");
        return super::decline();
    }
    let body_len = body.len();
    let mut out = (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )],
        body,
    )
        .into_response();
    set_header(&mut out, "content-length", &body_len.to_string());
    if gzip {
        set_header(&mut out, "content-encoding", "gzip");
        set_header(
            &mut out,
            crate::compress::RAW_LENGTH_HEADER,
            &raw_len.to_string(),
        );
    }
    set_header(&mut out, "cache-control", "private, no-cache");
    set_header(&mut out, "etag", etag);
    set_header(&mut out, "x-aac-view", view);
    if raw_len >= crate::compress::MIN_COMPRESS_BYTES {
        set_header(&mut out, "vary", "Cookie, Accept-Encoding");
    } else {
        set_header(&mut out, "vary", "Cookie");
    }
    out
}

/// One fed view reply: consumes the service's `ServiceReply` directly with zero body copy,
/// preserving pre-compressed gzip `Bytes`, ETag, `x-aac-view`, and `304 Not Modified`.
fn fed_view(reply: ServiceReply, if_none_match: Option<&str>) -> Reply {
    let header = |name: &str| {
        reply
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let (Some(etag), Some(view)) = (header("etag"), header("x-aac-view")) else {
        eprintln!("aac-http: fed view missing its etag or view header");
        return super::decline();
    };
    if !valid_fed_meta(&view, &etag) {
        eprintln!("aac-http: fed view refused: {view} {etag}");
        return super::decline();
    }
    let gzip = header("content-encoding").is_some_and(|enc| enc.eq_ignore_ascii_case("gzip"));
    let raw_len = header(crate::compress::RAW_LENGTH_HEADER)
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or_else(|| reply.body.len());
    if reply.status == 304 || crate::compress::not_modified(if_none_match, &etag) {
        let mut out = super::empty(304);
        set_header(&mut out, "cache-control", "private, no-cache");
        set_header(&mut out, "etag", &etag);
        set_header(&mut out, "x-aac-view", &view);
        if reply.status == 304 || raw_len >= crate::compress::MIN_COMPRESS_BYTES {
            set_header(&mut out, "vary", "Cookie, Accept-Encoding");
        } else {
            set_header(&mut out, "vary", "Cookie");
        }
        return out;
    }
    fed_shared_bytes(reply.body, raw_len, gzip, &etag, &view)
}

/// A fed view whose bytes and ETag a local owner wrote (e.g. Home with page context).
pub(super) fn fed_bytes(
    body: Vec<u8>,
    etag: &str,
    view: &str,
    if_none_match: Option<&str>,
    accept_gzip: bool,
    head: bool,
) -> Reply {
    if !valid_fed_meta(view, etag) {
        eprintln!("aac-http: fed view refused: {view} {etag}");
        return super::decline();
    }
    let raw_len = body.len();
    if crate::compress::not_modified(if_none_match, etag) {
        let mut out = super::empty(304);
        set_header(&mut out, "cache-control", "private, no-cache");
        set_header(&mut out, "etag", etag);
        set_header(&mut out, "x-aac-view", view);
        if raw_len >= crate::compress::MIN_COMPRESS_BYTES {
            set_header(&mut out, "vary", "Cookie, Accept-Encoding");
        } else {
            set_header(&mut out, "vary", "Cookie");
        }
        return out;
    }
    if !head
        && accept_gzip
        && let Some(gz) = crate::compress::compress_for_tag(Some(etag), &body)
    {
        return fed_shared_bytes(gz, raw_len, true, etag, view);
    }
    fed_shared_bytes(axum::body::Bytes::from(body), raw_len, false, etag, view)
}

/// The reports root the media route reads (`AgentReportsReader`'s rule).
fn reports_root() -> String {
    let env = std::env::var("AAC_AGENT_REPORTS_DIR").ok();
    aac_status::share::agent_reports_root(env.as_deref())
        .to_string_lossy()
        .into_owned()
}

/// `GET|HEAD /api/status/files/<writer>/<path>`: the admitted card media and its range headers.
async fn card_media(
    access: &DashboardAccess,
    facts: &Facts,
    writer: &str,
    relative: &str,
    raw: Option<&str>,
) -> Reply {
    if let Some(reply) = browser_only(access) {
        return reply;
    }
    if raw.is_some() {
        return no_store(super::error(
            400,
            "unexpected_query",
            UNEXPECTED_QUERY_STATUS,
        ));
    }
    let (Some(writer), Some(relative)) = (
        super::decode_component(writer),
        super::decode_component(relative),
    ) else {
        return no_store(super::error(
            404,
            "not_found",
            "This file is not available.",
        ));
    };
    let input = json!({
        "kind": "media",
        "root": reports_root(),
        "writer": writer,
        "path": relative,
        "range": facts.range.as_deref(),
        "head": facts.head,
    });
    read_file("/api/status/files/{*path}", input).await
}

/// The share route's own headers, on every reply including refusals.
fn share_headers(mut reply: Reply) -> Reply {
    set_header(&mut reply, "cache-control", "no-store");
    set_header(&mut reply, "x-content-type-options", "nosniff");
    set_header(
        &mut reply,
        "content-security-policy",
        "sandbox; default-src 'none'; script-src 'none'; connect-src 'none'; style-src 'unsafe-inline'; img-src data:; base-uri 'none'; form-action 'none'; frame-ancestors 'self'",
    );
    set_header(&mut reply, "referrer-policy", "no-referrer");
    set_header(&mut reply, "cross-origin-resource-policy", "same-origin");
    reply
}

/// `GET|HEAD /api/status/deliverable?path=...`: the share route's own refusals, then one file.
async fn deliverable(
    access: &DashboardAccess,
    req: Request,
    facts: &Facts,
    raw: Option<&str>,
) -> Reply {
    if let Some(refusal) = browser_only(access) {
        return refusal;
    }
    if !facts.same_origin {
        return share_headers(super::error(
            403,
            "wrong_origin",
            "Use the dashboard origin.",
        ));
    }
    if req.method() != Method::GET && req.method() != Method::HEAD {
        let mut refusal = super::error(405, "read_only", "Only GET and HEAD are supported.");
        set_header(&mut refusal, "allow", "GET, HEAD");
        return share_headers(refusal);
    }
    let Some(relative) = deliverable_path(raw) else {
        return share_headers(super::error(
            400,
            "invalid_path",
            "Use a share-relative file path.",
        ));
    };
    let input = json!({
        "kind": "deliverable",
        "root": aac_status::share::machine_share_root(),
        "writer": null,
        "path": relative,
        "range": facts.range.as_deref(),
        "head": facts.head,
    });
    share_headers(read_file("/api/status/deliverable", input).await)
}

/// The share route's query: exactly `path`, a safe relative path under one allowed base.
fn deliverable_path(raw: Option<&str>) -> Option<String> {
    let raw = raw?;
    let mut pairs = raw.split('&');
    let (key, value) = pairs.next()?.split_once('=')?;
    if key != "path" || pairs.next().is_some() {
        return None;
    }
    let decoded = percent_decode(value)?;
    let safe = !decoded.is_empty()
        && decoded.encode_utf16().count() <= 2048
        && !decoded
            .bytes()
            .any(|byte| byte == b'\\' || byte == b':' || byte == b'%' || byte < 32 || byte == 127)
        && decoded
            .split('/')
            .all(|part| !part.is_empty() && !part.starts_with('.'));
    if !safe {
        return None;
    }
    let parts: Vec<&str> = decoded.split('/').collect();
    let allowed = (parts[0] == "share" && parts.len() > 1)
        || (parts.len() > 5
            && parts[..3] == ["agent-reports", "v1", "writers"]
            && (1..=128).contains(&parts[3].len())
            && parts[3].as_bytes()[0].is_ascii_alphanumeric()
            && parts[3]
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            && parts[4] == "media");
    allowed.then_some(decoded)
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let hi = char::from(*bytes.get(i + 1)?).to_digit(16)? as u8;
                let lo = char::from(*bytes.get(i + 2)?).to_digit(16)? as u8;
                out.push(hi * 16 + lo);
                i += 3;
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

/// Serve the checked descriptor directly, including ranges larger than the private JSON cap.
async fn read_file(route: &'static str, input: Json) -> Reply {
    let body = input.to_string();
    let Ok(Some((prepared, file))) =
        tokio::task::spawn_blocking(move || crate::read_files::prepare(route, body.as_bytes()))
            .await
    else {
        return super::service_error();
    };
    let Some(status) = prepared["status"]
        .as_u64()
        .and_then(|status| u16::try_from(status).ok())
    else {
        return super::service_error();
    };
    let mut reply = if prepared["json"].is_object() {
        no_store(json_reply(status, &prepared["json"]))
    } else {
        let Ok(status) = StatusCode::from_u16(status) else {
            return super::service_error();
        };
        let body = if let Some((file, length)) = file {
            let reader = tokio::fs::File::from_std(file).take(length);
            Body::from_stream(tokio_util::io::ReaderStream::new(reader))
        } else {
            Body::empty()
        };
        (status, body).into_response()
    };
    if let Some(headers) = prepared["headers"].as_object() {
        for (name, value) in headers {
            if let (Ok(name), Some(value)) =
                (axum::http::HeaderName::try_from(name), value.as_str())
                && let Ok(value) = axum::http::HeaderValue::from_str(value)
            {
                reply.headers_mut().insert(name, value);
            }
        }
    }
    reply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", windows))]
    #[tokio::test]
    async fn media_and_deliverables_stream_beyond_the_private_envelope_cap() -> std::io::Result<()>
    {
        for (route, kind, writer, relative, on_disk) in [
            (
                "/api/status/files/{*path}",
                "media",
                Some("vm.fixture"),
                "image.png",
                "v1/writers/vm.fixture/image.png",
            ),
            (
                "/api/status/deliverable",
                "deliverable",
                None,
                "share/folder/video.mp4",
                "share/folder/video.mp4",
            ),
        ] {
            let root = tempfile::tempdir()?;
            let file = root.path().join(on_disk);
            std::fs::create_dir_all(file.parent().expect("fixture parent"))?;
            let mut bytes = vec![42u8; 1_500_000];
            bytes[..8].copy_from_slice(&[137, 80, 78, 71, 13, 10, 26, 10]);
            std::fs::write(file, &bytes)?;
            let mut input = json!({"root": root.path(), "kind": kind, "writer": writer,
                "path": relative, "head": false, "range": "bytes=1-1200000"});
            let response = read_file(route, input.clone()).await;
            assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
            assert_eq!(response.headers()["content-length"], "1200000");
            assert_eq!(
                response.headers()["content-range"],
                "bytes 1-1200000/1500000"
            );
            let streamed = to_bytes(response.into_body(), 1_200_000)
                .await
                .expect("range");
            assert_eq!(&streamed[..], &bytes[1..1_200_001]);
            input["head"] = Json::Bool(true);
            let head = read_file(route, input).await;
            assert_eq!(head.headers()["content-length"], "1200000");
            assert!(
                to_bytes(head.into_body(), 0)
                    .await
                    .expect("empty HEAD")
                    .is_empty()
            );
        }
        Ok(())
    }

    #[test]
    fn a_store_still_opening_is_a_retryable_failure_not_a_missing_store() {
        let reply = |reason: &'static str| {
            let mut headers = axum::http::HeaderMap::new();
            headers.insert(
                "x-aac-decline",
                axum::http::HeaderValue::from_static(reason),
            );
            ServiceReply {
                status: 503,
                headers,
                body: axum::body::Bytes::new(),
            }
        };
        assert!(store_still_opening(&reply("busy")));
        assert!(!store_still_opening(&reply("unsupported")));
        assert_eq!(store_failed().status(), 503);
    }

    #[test]
    fn the_group_owns_exactly_its_routes() {
        assert!(owns_route(&Method::GET, "/api/status"));
        assert!(owns_route(&Method::HEAD, "/api/status"));
        assert!(owns_route(&Method::GET, "/api/status/answers"));
        assert!(owns_route(&Method::POST, "/api/status/answers"));
        let host = super::super::owner_host::OwnerHost::new(Default::default(), false);
        let cut = Routes::new(host.clone(), true, false);
        let optional = Routes::new(host, false, true);
        for path in [
            "/api/store/status",
            "/api/usage/history",
            "/api/usage/at",
            "/api/usage/burn",
        ] {
            assert!(cut.owns(&Method::GET, path), "{path}");
            assert!(optional.owns(&Method::GET, path), "{path}");
        }
        assert!(owns_route(
            &Method::GET,
            "/api/status/files/vm.lead/cards/Q1/one.png"
        ));
        assert!(!owns_route(&Method::GET, "/api/status/files/vm.lead"));
        assert!(!owns_route(
            &Method::POST,
            "/api/status/files/vm.lead/a.png"
        ));
        assert!(owns_route(&Method::GET, "/api/scoreboard"));
        assert!(!owns_route(&Method::GET, "/api/views/home"));
    }

    #[test]
    fn the_deliverable_path_is_the_share_routes_contract() {
        assert_eq!(
            deliverable_path(Some("path=share%2Fa%20b%2Fc.html")).as_deref(),
            Some("share/a b/c.html")
        );
        assert_eq!(
            deliverable_path(Some(
                "path=agent-reports%2Fv1%2Fwriters%2Fvm.lead%2Fmedia%2Fclip.mp4"
            ))
            .as_deref(),
            Some("agent-reports/v1/writers/vm.lead/media/clip.mp4")
        );
        for bad in [
            None,
            Some(""),
            Some("path=share"),
            Some("path=share%2F..%2Fetc%2Fpasswd"),
            Some("path=share%2F.hidden%2Fx"),
            Some("path=share%2F%00"),
            Some("path=other%2Fx.html"),
            Some("path=share%2Fa&extra=1"),
            Some("Path=share%2Fa"),
        ] {
            assert_eq!(deliverable_path(bad), None, "{bad:?}");
        }
    }
}
