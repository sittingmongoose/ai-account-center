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

use super::{DashboardAccess, DashboardPrincipal, Reply, service_link::ServiceReply, set_header};
use axum::{
    body::to_bytes,
    extract::Request,
    http::{Method, StatusCode},
    response::IntoResponse,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value as Json, json};
use std::path::Path;

/// This group's admission and encoding, over the one owner host.
pub(super) struct Routes {
    host: std::sync::Arc<super::owner_host::OwnerHost>,
}

impl Routes {
    pub(super) fn new(host: std::sync::Arc<super::owner_host::OwnerHost>) -> Self {
        Self { host }
    }

    /// Whether this boot answers `method path` itself: the switch is on and the pattern is one
    /// of the worker-removal routes.
    pub(super) fn owns(&self, method: &Method, path: &str) -> bool {
        super::owner_host::enabled() && owns_route(method, path)
    }

    fn root(&self) -> &Path {
        self.host.root()
    }

    async fn hosted(&self) -> bool {
        self.host.ready().await
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

/// `MAX_STATUS_BODY_BYTES`, the answer set's own limit.
const ANSWER_BODY_CAP: usize = 64 * 1024;
/// The file envelope's largest base64 body (`rust-file-response.ts`).
const FILE_REPLY_CAP: usize = 1_400_000;

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
        return true;
    }
    if !matches!(*method, Method::GET | Method::HEAD) {
        return false;
    }
    path == "/api/status"
        || path == "/api/status/answers"
        || path == "/api/scoreboard"
        || path == "/api/store/status"
        || path == "/api/usage/history"
        || path == "/api/usage/at"
        || path == "/api/usage/burn"
        || media(path).is_some()
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
    origin_present: bool,
    same_origin: bool,
    content_type: Option<String>,
    content_length: Option<usize>,
    range: Option<String>,
    if_none_match: Option<String>,
    sec_fetch_site: Option<String>,
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
            origin_present: access.origin_present,
            same_origin: access.same_origin,
            content_type: text("content-type"),
            content_length: text("content-length").and_then(|value| value.parse().ok()),
            range: text("range"),
            if_none_match: text("if-none-match"),
            sec_fetch_site: text("sec-fetch-site"),
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
            return Some(no_store(super::plain(401, "Authentication required")));
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

fn unavailable_status() -> Reply {
    no_store(json_reply(
        200,
        &json!({"state": "unavailable", "reason": "missing"}),
    ))
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
        Some(reply) if declined(&reply) => store_unavailable(),
        Some(reply) if reply.status == 200 => match json_of(&reply) {
            Some(value) => no_store(json_reply(200, &value)),
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
    let raw = raw.as_deref().filter(|query| !query.is_empty());
    let facts = Facts::of(access, &req);
    match path.as_str() {
        "/api/store/status" => store_status(routes, access, &facts).await,
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
            store_route(routes, access, &path, raw).await
        }
        "/api/status/answers" if req.method() == Method::POST => {
            answers_post(routes, access, req, &facts, raw).await
        }
        "/api/status/answers" => answers_read(routes, access, raw).await,
        "/api/status" => status(routes, access, raw, &facts).await,
        "/api/scoreboard" => scoreboard(routes, access, raw, &facts).await,
        "/api/status/deliverable" => deliverable(routes, access, req, &facts, raw).await,
        _ => match media(&path) {
            Some((writer, relative)) => {
                card_media(routes, access, &facts, writer, relative, raw).await
            }
            None => super::decline(),
        },
    }
}

/// `GET /api/store/status`: the hosted store's writer status, or the fallback's honest
/// "unavailable" (never an invented one).
async fn store_status(routes: &Routes, access: &DashboardAccess, facts: &Facts) -> Reply {
    if let Some(reply) = store_read(access, facts) {
        return reply;
    }
    let Some(principal) = access.service_principal() else {
        return unavailable_status();
    };
    if !routes.hosted().await {
        return unavailable_status();
    }
    match routes
        .request("GET", "/api/store/status", &principal, &[], None)
        .await
    {
        Some(reply) if declined(&reply) => unavailable_status(),
        Some(reply) if reply.status == 200 => match json_of(&reply) {
            Some(value)
                if matches!(
                    value.get("state").and_then(Json::as_str),
                    Some("ready" | "degraded" | "unavailable")
                ) =>
            {
                no_store(json_reply(200, &value))
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
            query
                .split('&')
                .all(|part| part == "limit" || part.starts_with("limit="))
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
            &[("content-type", content_type.as_str())],
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
    let preferences = crate::account_reads::preferences(root)?;
    let name = preferences
        .get("timeZone")
        .and_then(Json::as_str)
        .unwrap_or("America/New_York")
        .to_owned();
    let now = i64::try_from(now_ms()).ok()?;
    let facts = crate::zone_facts::server_zone_facts(&name, now)?;
    Some((name, facts))
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
        return super::error(400, "unexpected_query", UNEXPECTED_QUERY_STATUS);
    }
    let Some(principal) = access.service_principal() else {
        return super::decline();
    };
    if !routes.hosted().await {
        return super::decline();
    }
    let Some((time_zone, offsets)) = status_zone(routes.root()) else {
        return super::decline();
    };
    let now = now_ms().to_string();
    let zone = json!({
        "timeZone": time_zone,
        "from": offsets.from,
        "to": offsets.to,
        "offsets": offsets.offsets,
    })
    .to_string();
    let headers = [
        ("x-aac-owned", "1"),
        ("x-aac-now", now.as_str()),
        ("x-aac-zone", zone.as_str()),
    ];
    match routes
        .request("GET", "/api/status", &principal, &headers, None)
        .await
    {
        Some(reply) if reply.status == 200 => fed_view(&reply, facts.if_none_match.as_deref()),
        _ => super::decline(),
    }
}

/// The service principal internal routes admit (`VerifiedPrincipal::valid`).
const SERVICE: &str = r#"{"kind":"session","id":"front-door","role":"service"}"#;
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

/// The Scoreboard's presentation facts: the service's unfiltered input, the envelope facts over
/// it in the display zone, and the held generation id. `None` declines to the route's 503.
async fn scoreboard_facts(
    routes: &Routes,
    time_zone: &str,
    now: u64,
) -> Option<(u64, String, String)> {
    let reply = routes
        .request("GET", "/internal/scoreboard/inputs", SERVICE, &[], None)
        .await?;
    if reply.status != 200 {
        return None;
    }
    let input = json_of(&reply)?;
    let generation = input.get("generation")?.as_u64()?;
    let catalog_hash = input.get("catalogHash")?.as_str()?.to_owned();
    let catalog = input.get("catalog")?.clone();
    let runs = input.get("runs")?.clone();
    let docs = input.get("docs")?.clone();
    if catalog.is_null() || runs.is_null() || docs.is_null() {
        return None;
    }
    let task_types: Vec<Json> = catalog
        .get("taskTypes")
        .and_then(Json::as_array)
        .map(|types| {
            types
                .iter()
                .map(|kind| {
                    json!({
                        "id": kind.get("id").cloned().unwrap_or(Json::Null),
                        "label": kind.get("label").cloned().unwrap_or(Json::Null),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let data = json!({
        "now": now,
        "timeZone": time_zone,
        "root": "",
        "catalog": catalog,
        "runs": runs,
        "joinReviews": [],
        "seedFailures": input.get("seedFailures").cloned().unwrap_or_else(|| json!([])),
        "docs": docs,
        "finish": {
            "filters": {
                "from": Json::Null,
                "to": Json::Null,
                "source": "all",
                "taskType": Json::Null,
                "available": true,
                "taskTypes": task_types,
            },
            "identity": input.get("identity").cloned().unwrap_or(Json::Null),
            "history": {"store": "on", "runs": Json::Null, "importing": false},
        },
    });
    let offsets = crate::zone_facts::server_zone_facts(time_zone, i64::try_from(now).ok()?)?;
    let facts = aac_status::produce::facts::Facts::of(&[&data], Some(&offsets.zone));
    let data_text = json!({ "timeZone": time_zone }).to_string();
    let facts_text = facts.to_text();
    let short: String = catalog_hash.chars().take(12).collect();
    let facts_id = format!("{generation}:{short}:{now}");
    let body = format!(
        "{{\"generation\":{},\"data\":{},\"facts\":{}}}",
        Json::String(facts_id.clone()),
        data_text,
        facts_text
    );
    let reply = routes
        .request(
            "PUT",
            "/internal/feed/scoreboard-facts",
            SERVICE,
            &[("content-type", "application/json")],
            Some(body.as_bytes()),
        )
        .await?;
    if reply.status != 200 {
        return None;
    }
    let held = json_of(&reply)?.get("generation")?.as_str()?.to_owned();
    Some((generation, catalog_hash, held))
}

/// `GET /api/scoreboard`: the filters, the display zone's facts for the service's unfiltered
/// input, and the owned answer (bytes, ETag, view).
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
        return super::decline();
    };
    if !routes.hosted().await {
        return super::decline();
    }
    let Some((time_zone, offsets)) = status_zone(routes.root()) else {
        return super::decline();
    };
    let now = now_ms();
    let Some((clock, day)) = offsets.zone.words(i64::try_from(now).unwrap_or(0)) else {
        return super::decline();
    };
    let from_ms = match query.from.as_deref() {
        Some(day) => match crate::zone_facts::local_midnight(day, &time_zone) {
            Some(ms) => Json::from(ms),
            None => return super::decline(),
        },
        None => Json::Null,
    };
    let to_ms = match query.to.as_deref() {
        Some(day) => {
            let Some(next) = crate::zone_facts::next_day(day) else {
                return super::decline();
            };
            match crate::zone_facts::local_midnight(&next, &time_zone) {
                Some(ms) => Json::from(ms - 1000),
                None => return super::decline(),
            }
        }
        None => Json::Null,
    };
    // The service may ingest or rescan between the facts fetch and the answer; its `stale`
    // decline means fresh facts, exactly as today's handler refetches on the next request.
    // A cold boot's first seed can take a moment: retry briefly, then answer honestly.
    for attempt in 0..6 {
        let Some((generation, catalog_hash, held)) =
            scoreboard_facts(routes, &time_zone, now).await
        else {
            return super::decline();
        };
        let feed = json!({
            "now": now,
            "clock": clock.clone(),
            "day": day.clone(),
            "timeZone": time_zone.clone(),
            "from": query.from.clone(),
            "to": query.to.clone(),
            "source": query.source.clone(),
            "taskType": query.task_type.clone(),
            "fromMs": from_ms.clone(),
            "toMs": to_ms.clone(),
            "generation": generation,
            "catalogHash": catalog_hash,
            "facts": held,
        })
        .to_string();
        match routes
            .request(
                "GET",
                "/api/scoreboard",
                &principal,
                &[("x-aac-feed", feed.as_str())],
                None,
            )
            .await
        {
            Some(reply) if reply.status == 200 => {
                return fed_view(&reply, facts.if_none_match.as_deref());
            }
            Some(reply) if declined(&reply) => continue,
            _ => return super::decline(),
        }
    }
    super::decline()
}

/// One fed view reply: the service's own bytes, ETag and `x-aac-view`, written with the
/// revalidated-view headers and the `If-None-Match` 304 (`rust-view-reply.ts`).
fn fed_view(reply: &ServiceReply, if_none_match: Option<&str>) -> Reply {
    let header = |name: &str| {
        reply
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned)
    };
    let (Some(etag), Some(view)) = (header("etag"), header("x-aac-view")) else {
        return super::decline();
    };
    if !matches!(
        view.as_str(),
        "status@1" | "scoreboard@1" | "home@3" | "dashboard@1"
    ) || !etag.starts_with("W/\"")
        || etag.len() != 67
    {
        return super::decline();
    }
    let mut out = if not_modified(if_none_match, &etag) {
        super::empty(304)
    } else {
        super::json_bytes(200, reply.body.clone())
    };
    set_header(&mut out, "cache-control", "private, no-cache");
    set_header(&mut out, "etag", &etag);
    set_header(&mut out, "x-aac-view", &view);
    set_header(&mut out, "vary", "Cookie");
    out
}

/// `If-None-Match` against the reply's ETag, with the route's 304 (no body).
fn not_modified(validators: Option<&str>, etag: &str) -> bool {
    let Some(validators) = validators else {
        return false;
    };
    validators.split(',').any(|tag| {
        let value = tag.trim();
        value == "*" || value.replace("W/", "") == etag.replace("W/", "")
    })
}

/// The reports root the media route reads (`AgentReportsReader`'s rule).
fn reports_root() -> String {
    let env = std::env::var("AAC_AGENT_REPORTS_DIR").ok();
    aac_status::share::agent_reports_root(env.as_deref())
        .to_string_lossy()
        .into_owned()
}

/// `GET|HEAD /api/status/files/<writer>/<path>`: the file envelope from the service, checked and
/// written with the route's own headers (`rust-file-response.ts`).
async fn card_media(
    routes: &Routes,
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
    let Some(principal) = access.service_principal() else {
        return super::service_error();
    };
    if !routes.hosted().await {
        return super::service_error();
    }
    let input = json!({
        "kind": "media",
        "root": reports_root(),
        "writer": writer,
        "path": relative,
        "range": facts.range.as_deref(),
        "head": facts.head,
    });
    let body = input.to_string();
    match routes
        .request(
            "GET",
            "/api/status/files/{*path}",
            &principal,
            &[("content-type", "application/json")],
            Some(body.as_bytes()),
        )
        .await
    {
        Some(reply) => file_reply(&reply, facts.head, true),
        None => super::service_error(),
    }
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
    routes: &Routes,
    access: &DashboardAccess,
    req: Request,
    facts: &Facts,
    raw: Option<&str>,
) -> Reply {
    if let Some(refusal) = browser_only(access) {
        return share_headers(refusal);
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
    let Some(principal) = access.service_principal() else {
        return share_headers(super::service_error());
    };
    if !routes.hosted().await {
        return share_headers(super::service_error());
    }
    let input = json!({
        "kind": "deliverable",
        "root": machine_share_root(),
        "writer": null,
        "path": relative,
        "range": facts.range.as_deref(),
        "head": facts.head,
    });
    let body = input.to_string();
    match routes
        .request(
            "GET",
            "/api/status/deliverable",
            &principal,
            &[("content-type", "application/json")],
            Some(body.as_bytes()),
        )
        .await
    {
        Some(reply) => share_headers(file_reply(&reply, facts.head, false)),
        None => share_headers(super::service_error()),
    }
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

fn machine_share_root() -> &'static str {
    if cfg!(target_os = "windows") {
        r"\\TRUENAS\Cursor"
    } else if cfg!(target_os = "macos") {
        "/Volumes/Cursor"
    } else {
        "/mnt/Cursor"
    }
}

const FILE_HEADERS: [&str; 8] = [
    "content-type",
    "content-length",
    "content-range",
    "accept-ranges",
    "cache-control",
    "content-disposition",
    "x-content-type-options",
    "content-security-policy",
];
const FILE_TYPES: [&str; 11] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/svg+xml",
    "video/mp4",
    "video/webm",
    "application/pdf",
    "application/json",
    "text/html; charset=utf-8",
    "text/plain; charset=utf-8",
];

/// One server reply with these bytes and no derived headers (the file envelope owns its own).
fn raw_reply(status: u16, body: Vec<u8>) -> Reply {
    (
        StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        body,
    )
        .into_response()
}

/// The service's file envelope, checked and written (`rust-file-response.ts`): only these
/// header names, the route's own refusal sentences, and a body whose length matches.
fn file_reply(reply: &ServiceReply, head: bool, media: bool) -> Reply {
    let Some(value) = json_of(&reply) else {
        return super::service_error();
    };
    let Some(status) = value.get("status").and_then(Json::as_u64) else {
        return super::service_error();
    };
    let Ok(status) = u16::try_from(status) else {
        return super::service_error();
    };
    let Some(headers) = value.get("headers").and_then(Json::as_object) else {
        return super::service_error();
    };
    if !matches!(status, 200 | 206 | 400 | 404 | 413 | 415 | 416) {
        return super::service_error();
    }
    let mut out_headers: Vec<(&'static str, String)> = Vec::new();
    for (key, header_value) in headers {
        let Some(known) = FILE_HEADERS.iter().find(|held| **held == key.as_str()) else {
            return super::service_error();
        };
        let Some(text) = header_value.as_str() else {
            return super::service_error();
        };
        if text.len() > 256 || text.contains(['\r', '\n', '\0']) {
            return super::service_error();
        }
        match *known {
            "content-type" if !FILE_TYPES.contains(&text) => return super::service_error(),
            "content-security-policy" if media && text != "default-src 'none'; sandbox" => {
                return super::service_error();
            }
            "x-content-type-options" if text != "nosniff" => return super::service_error(),
            "cache-control" if media && text != "private, max-age=300" => {
                return super::service_error();
            }
            "content-disposition" if text != "attachment" => return super::service_error(),
            "accept-ranges" if text != "bytes" => return super::service_error(),
            _ => {}
        }
        out_headers.push((*known, text.to_owned()));
    }
    if let Some(range) = headers.get("content-range").and_then(Json::as_str)
        && !valid_content_range(range)
    {
        return super::service_error();
    }
    let mut out = if status == 200 || status == 206 {
        let Some(bytes) = value.get("bytes").and_then(Json::as_str) else {
            return super::service_error();
        };
        let kind = headers.get("content-type").and_then(Json::as_str);
        if value.get("json").is_some_and(|json| !json.is_null())
            || bytes.len() > FILE_REPLY_CAP
            || !kind.is_some_and(|kind| FILE_TYPES.contains(&kind))
        {
            return super::service_error();
        }
        if media
            && (!matches!(
                kind,
                Some("image/png" | "image/jpeg" | "image/webp" | "video/mp4" | "video/webm")
            ) || headers.get("x-content-type-options").and_then(Json::as_str)
                != Some("nosniff")
                || headers
                    .get("content-security-policy")
                    .and_then(Json::as_str)
                    != Some("default-src 'none'; sandbox")
                || headers.get("accept-ranges").and_then(Json::as_str) != Some("bytes")
                || headers.get("cache-control").and_then(Json::as_str)
                    != Some("private, max-age=300"))
        {
            return super::service_error();
        }
        if !media
            && kind == Some("image/svg+xml")
            && headers.get("content-disposition").and_then(Json::as_str) != Some("attachment")
        {
            return super::service_error();
        }
        if status == 206 && !headers.contains_key("content-range") {
            return super::service_error();
        }
        let decoded = match STANDARD.decode(bytes) {
            Ok(decoded) if STANDARD.encode(&decoded) == bytes => decoded,
            _ => return super::service_error(),
        };
        let length = headers
            .get("content-length")
            .and_then(Json::as_str)
            .and_then(|text| text.parse::<usize>().ok())
            .filter(|length| *length <= 100 * 1024 * 1024);
        let Some(length) = length else {
            return super::service_error();
        };
        if (head && !decoded.is_empty()) || (!head && length != decoded.len()) {
            return super::service_error();
        }
        raw_reply(status, if head { Vec::new() } else { decoded })
    } else {
        let Some(json_body) = value.get("json").filter(|json| json.is_object()) else {
            return super::service_error();
        };
        let code = json_body.get("code").and_then(Json::as_str).unwrap_or("");
        let error = json_body.get("error").and_then(Json::as_str).unwrap_or("");
        let (expected_status, sentence) = match (media, code) {
            (true, "not_found") => (404, "This file is not available."),
            (true, "too_large") => (
                413,
                "This file is larger than its limit (images 2 MB, videos 50 MB).",
            ),
            (true, "not_media") => (
                415,
                "This file is not a PNG, JPEG or WebP image or an MP4 or WebM video.",
            ),
            (true, "range_not_satisfiable") => (416, "The requested range is outside this file."),
            (false, "invalid_path") => (400, "Use a share-relative file path."),
            (false, "not_found") => (404, "This file is not available."),
            (false, "too_large") => (413, "This file exceeds the 100 MB limit."),
            (false, "unsupported_type") => (415, "This file type is not supported."),
            (false, "invalid_range") => (416, "The requested range is outside this file."),
            _ => return super::service_error(),
        };
        if status != expected_status || error != sentence {
            return super::service_error();
        }
        let body = if media {
            json!({"error": sentence, "code": code})
        } else {
            json!({"code": code, "error": sentence})
        };
        super::json_bytes(status, body.to_string().into_bytes())
    };
    for (name, value) in out_headers {
        set_header(&mut out, name, &value);
    }
    out
}

fn valid_content_range(text: &str) -> bool {
    let Some(rest) = text.strip_prefix("bytes ") else {
        return false;
    };
    let Some((range, size)) = rest.split_once('/') else {
        return false;
    };
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    if !digits(size) {
        return false;
    }
    range == "*"
        || range
            .split_once('-')
            .is_some_and(|(start, end)| digits(start) && digits(end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_group_owns_exactly_its_routes() {
        assert!(owns_route(&Method::GET, "/api/status"));
        assert!(owns_route(&Method::HEAD, "/api/status"));
        assert!(owns_route(&Method::GET, "/api/status/answers"));
        assert!(owns_route(&Method::POST, "/api/status/answers"));
        assert!(owns_route(&Method::GET, "/api/store/status"));
        for path in ["/api/usage/history", "/api/usage/at", "/api/usage/burn"] {
            assert!(owns_route(&Method::GET, path), "{path}");
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
        assert!(!owns_route(&Method::GET, "/api/scoreboard"));
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
