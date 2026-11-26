//! The Analytics route group served by the native front door: the capture setting on both
//! platforms (its file and admission are platform-free) and the composed analytics answer.
//! The admission order, the error bodies and the JSON encoding preserve the original route's
//! (`account-analytics-routes.ts`), so the recorded M0 contracts stay byte-equal: a browser or bar
//! session only, the dashboard origin for the analytics read and for a capture write, the exact
//! query keys, and the capture body `{"cwd": <boolean>}`.
use super::{DashboardAccess, DashboardPrincipal, Reply};
use aac_core::Cause;
use axum::{
    extract::Request,
    http::{Method, header},
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// One request to the owner host (the running private service, or the in-process one a
/// worker-less boot starts): its [`super::owner_host::ServiceReply`], or `None` when no owner answers.
pub(super) type Hop = Arc<dyn Fn(String, String, Vec<(String, String)>) -> HopFuture + Send + Sync>;

/// The hop's future: the service reply, shared with the route's own `Send` requirement.
pub(super) type HopFuture =
    Pin<Box<dyn std::future::Future<Output = Option<super::owner_host::ServiceReply>> + Send>>;

/// The capture setting's file, from the one private-file registry (`analytics-capture`).
const CAPTURE: &str = "analytics-capture";
/// The query keys the Analytics read admits, in the route's order.
const QUERY_KEYS: [&str; 8] = [
    "platform", "range", "provider", "account", "refresh", "tz", "from", "to",
];
const PRESETS: [&str; 7] = ["24h", "7d", "30d", "90d", "month", "all", "custom"];
const MAX_CUSTOM_DAYS: i64 = 36_525;

const INVALID_QUERY: &str =
    "Select a valid platform, time range, provider, account, and refresh flag.";
use crate::analytics_route::INVALID_RANGE;
const INVALID_TZ: &str = "Select a valid IANA time zone.";
const INVALID_PROVIDER: &str = "Select a provider that this server reports.";

/// The paths this owner answers (GET also covers HEAD).
pub(super) fn owns(method: &Method, path: &str) -> bool {
    if path.contains("/api/accounts/analytics") {
        let path = path.strip_suffix('/').unwrap_or(path);
        let method = if *method == Method::HEAD {
            Some(Method::GET)
        } else {
            Some(method.clone())
        };
        return match path {
            "/api/accounts/analytics" => method.as_ref() == Some(&Method::GET),
            "/api/accounts/analytics/capture" => {
                method.as_ref() == Some(&Method::GET) || method.as_ref() == Some(&Method::PUT)
            }
            _ => false,
        };
    }
    false
}

fn no_store(mut reply: Reply) -> Reply {
    super::set_header(&mut reply, "cache-control", "no-store");
    reply
}

fn headless(reply: Reply, head: bool) -> Reply {
    if !head {
        return reply;
    }
    let mut reply = reply;
    *reply.body_mut() = axum::body::Body::empty();
    reply
}

fn refused(status: u16, code: &str, message: &str) -> Reply {
    no_store(super::error(status, code, message))
}

fn unavailable() -> Reply {
    let mut reply = refused(
        503,
        "analytics_unavailable",
        "Account analytics could not be read safely. Try again.",
    );
    super::set_header(&mut reply, "retry-after", "1");
    reply
}

/// The request's admitted principal, or the reply the TypeScript route gives it.
fn session(access: &DashboardAccess) -> Result<(), Reply> {
    match &access.principal {
        DashboardPrincipal::Anonymous => {
            Err(refused(401, "auth_required", "Authentication required"))
        }
        DashboardPrincipal::Device { .. } => Err(refused(
            403,
            "device_scope",
            "This action needs a signed-in dashboard session.",
        )),
        DashboardPrincipal::Browser { .. } => Ok(()),
    }
}

/// `{cwd: <boolean>}` from the file. Missing or unreadable settings keep the default on.
fn read_capture(root: &Path) -> Value {
    json!({ "cwd": crate::analytics_route::capture_cwd(root) })
}

/// Keep the registered writer lease through cache cleanup and receipt construction. Persist
/// before clearing so a failed cleanup can be retried while capture is disabled.
fn save_capture(root: &Path, cwd: bool) -> Result<Value, ()> {
    let rows = aac_files::registry::rows().map_err(|_| ())?;
    let owners = super::native_settings::owners();
    let started = Instant::now();
    let file = loop {
        match owners.claim(rows, CAPTURE, root, None) {
            Ok(file) => break file,
            Err(error)
                if error.cause == Cause::Busy && started.elapsed() < Duration::from_secs(2) =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(_) => return Err(()),
        }
    };
    let mut bytes = serde_json::to_vec(&json!({ "cwd": cwd })).map_err(|_| ())?;
    bytes.push(b'\n');
    file.replace_settings(&bytes).map_err(|_| ())?;
    let receipt = if cwd {
        json!({ "cwd": true })
    } else {
        let (mut folders, caches) = clear_remote_cwds(root)?;
        if let Some(engine) =
            crate::analytics_storage::host(root).and_then(|host| host.analytics_engine())
        {
            folders += aac_store::analytics_cache::clear_remote_folders(&engine).map_err(|_| ())?;
        }
        json!({ "cwd": false, "cleared": { "folders": folders, "caches": caches } })
    };
    drop(file);
    Ok(receipt)
}

/// `clearAnalyticsRemoteCwds`: every host's remote cache loses its saved working folders, and a
/// cache the loader would discard is removed. Returns `(folders, caches)`.
fn clear_remote_cwds(root: &Path) -> Result<(u64, u64), ()> {
    const REMOTE_CACHE_CAP: u64 = 32 * 1024 * 1024;
    let directory = root.join("cache").join("analytics-remote-v1");
    let computers = match serde_json::from_str::<Value>(include_str!(
        "../../../../web-dashboard/public/computers.json"
    )) {
        Ok(Value::Object(map)) => map.keys().cloned().collect::<Vec<_>>(),
        _ => return Err(()),
    };
    let mut folders = 0u64;
    let mut caches = 0u64;
    for host in computers {
        let file = directory.join(format!("{host}.json"));
        let meta = match std::fs::symlink_metadata(&file) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        if !meta.file_type().is_file() {
            continue;
        }
        use aac_files::read::{Ancestors, Read, ReadPolicy};

        let policy = ReadPolicy::capped(REMOTE_CACHE_CAP).ancestors(Ancestors::NoLinkBelow(root));
        let value = match aac_files::read::read(&file, &policy) {
            Read::Bytes(bytes, _) => serde_json::from_slice::<Value>(&bytes).ok(),
            Read::Absent => continue,
            Read::TooLarge | Read::Unreadable(_) => None,
            _ => return Err(()),
        };
        let Some(mut value) = value.filter(Value::is_object) else {
            match std::fs::remove_file(&file) {
                Ok(()) => caches += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(_) => return Err(()),
            }
            continue;
        };
        let Some(sessions) = value.get_mut("sessions").and_then(Value::as_object_mut) else {
            continue;
        };
        let mut changed = 0u64;
        for evidence in sessions.values_mut() {
            let Some(item) = evidence.as_object_mut() else {
                continue;
            };
            if item.get("cwd").is_some_and(|cwd| !cwd.is_null()) {
                item.insert("cwd".into(), Value::Null);
                changed += 1;
            }
        }
        if changed == 0 {
            continue;
        }
        let body = aac_views::js_json::write(&value);
        aac_files::share::write_atomically(
            &directory,
            &format!("{host}.json"),
            &String::from_utf8_lossy(&body),
            0o600,
        )
        .map_err(|_| ())?;
        folders += changed;
    }
    Ok((folders, caches))
}

/// One query key's single string value; a repeated key is not a string, as Express's array is not.
fn query_pairs(query: Option<&str>) -> Option<Vec<(String, String)>> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (key, value) in url::form_urlencoded::parse(query.unwrap_or("").as_bytes()) {
        let key = key.into_owned();
        if pairs.iter().any(|(seen, _)| *seen == key) {
            return None;
        }
        pairs.push((key, value.into_owned()));
    }
    Some(pairs)
}

fn get<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// `isCalendarDate`: a real `YYYY-MM-DD` day, 1970 or later.
fn calendar_date(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 10 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !bytes
        .iter()
        .enumerate()
        .all(|(index, byte)| matches!(index, 4 | 7) || byte.is_ascii_digit())
    {
        return None;
    }
    let year: i64 = value.get(..4)?.parse().ok()?;
    let month: i64 = value.get(5..7)?.parse().ok()?;
    let day: i64 = value.get(8..10)?.parse().ok()?;
    if year < 1970 || !(1..=12).contains(&month) {
        return None;
    }
    let days = aac_num::civil::days_in_month(year, month);
    if day < 1 || day > days {
        return None;
    }
    Some(aac_num::civil::days_from_civil(year, month, day))
}

/// The route's query guard: `Some(path_and_query)` when admitted, else the 400 reply.
fn admitted_analytics(raw: Option<&str>, providers: &[&str]) -> Result<Option<String>, Reply> {
    let Some(pairs) = query_pairs(raw) else {
        return Err(refused(400, "invalid_query", INVALID_QUERY));
    };
    if pairs
        .iter()
        .any(|(key, _)| !QUERY_KEYS.contains(&key.as_str()))
    {
        return Err(refused(400, "invalid_query", INVALID_QUERY));
    }
    let platform = get(&pairs, "platform").unwrap_or("mac");
    let range = get(&pairs, "range").unwrap_or("7d");
    let provider = get(&pairs, "provider").unwrap_or("all");
    let account = get(&pairs, "account").unwrap_or("all");
    let refresh = get(&pairs, "refresh");
    let account_ok = !account.is_empty()
        && account.len() <= 320
        && account.as_bytes()[0].is_ascii_alphanumeric()
        && account.bytes().all(|b| {
            b.is_ascii_alphanumeric() || matches!(b, b':' | b'@' | b'.' | b'_' | b'+' | b'-')
        });
    if !matches!(platform, "mac" | "windows")
        || !account_ok
        || !matches!(refresh, None | Some("true" | "false"))
    {
        return Err(refused(400, "invalid_query", INVALID_QUERY));
    }
    if !PRESETS.contains(&range) {
        return Err(refused(400, "invalid_range", INVALID_RANGE));
    }
    if provider != "all" && provider != "other" && !providers.contains(&provider) {
        return Err(refused(400, "invalid_provider", INVALID_PROVIDER));
    }
    // validateAccountAnalyticsRangeShape: the clock-free parts of the range.
    if let Some(tz) = get(&pairs, "tz")
        && crate::analytics_route::approved_time_zone(tz).is_none()
    {
        return Err(refused(400, "invalid_tz", INVALID_TZ));
    }
    let (from, to) = (get(&pairs, "from"), get(&pairs, "to"));
    if range == "custom" {
        let (Some(from), Some(to)) = (from.and_then(calendar_date), to.and_then(calendar_date))
        else {
            return Err(refused(400, "invalid_range", INVALID_RANGE));
        };
        if from > to || to - from + 1 > MAX_CUSTOM_DAYS {
            return Err(refused(400, "invalid_range", INVALID_RANGE));
        }
    } else if from.is_some() || to.is_some() {
        return Err(refused(400, "invalid_range", INVALID_RANGE));
    }
    Ok(raw.map(str::to_owned).filter(|query| !query.is_empty()))
}

/// Whether the path is the capture setting (the rest of the group is the composed read).
pub(super) fn is_capture(path: &str) -> bool {
    path.trim_end_matches('/')
        .ends_with("/api/accounts/analytics/capture")
}

/// The capture setting: always answered here, on both platforms.
pub(super) async fn capture(
    request: Request,
    root: PathBuf,
    writes: Arc<tokio::sync::Mutex<()>>,
) -> Reply {
    let head = request.method() == Method::HEAD;
    let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
        return super::service_error();
    };
    if let Err(reply) = session(&access) {
        return headless(reply, head);
    }
    let raw_query = request.uri().query().map(str::to_owned);
    if raw_query.is_some_and(|query| !query.is_empty()) {
        return headless(
            refused(400, "invalid_query", "Unexpected capture settings query."),
            head,
        );
    }
    if matches!(*request.method(), Method::GET | Method::HEAD) {
        return headless(no_store(super::response(200, read_capture(&root))), head);
    }
    if !access.origin_present || !access.same_origin {
        return headless(
            refused(
                403,
                "origin_required",
                "Capture settings require the dashboard origin.",
            ),
            head,
        );
    }
    let is_json = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"));
    if !is_json {
        return headless(
            no_store(super::response(
                415,
                json!({"error":"Capture settings require application/json."}),
            )),
            head,
        );
    }
    let body = axum::body::to_bytes(request.into_body(), 4096).await;
    let Ok(body) = body else {
        return headless(
            refused(
                400,
                "invalid_body",
                r#"Send {"cwd": true} or {"cwd": false}."#,
            ),
            head,
        );
    };
    let Some(cwd) = exact_cwd(&body) else {
        return headless(
            refused(
                400,
                "invalid_body",
                r#"Send {"cwd": true} or {"cwd": false}."#,
            ),
            head,
        );
    };
    let write = writes.lock_owned().await;
    let saved = tokio::task::spawn_blocking(move || {
        let _write = write;
        save_capture(&root, cwd)
    })
    .await;
    let reply = match saved {
        Ok(Ok(receipt)) => no_store(super::response(200, receipt)),
        _ => refused(500, "save_failed", "Capture settings could not be saved."),
    };
    headless(reply, head)
}

/// The composed read, including the public receipt when the native owner is unavailable.
#[allow(clippy::too_many_arguments)]
pub(super) async fn analytics(
    access: Option<DashboardAccess>,
    raw_query: Option<String>,
    head: bool,
    cross_site: bool,
    accept_gzip: bool,
    if_none_match: Option<String>,
    no_cache: bool,
    hop: Option<Hop>,
) -> Reply {
    let Some(access) = access else {
        return super::service_error();
    };
    headless(
        composed(
            access,
            raw_query.as_deref(),
            cross_site,
            head,
            accept_gzip,
            if_none_match.as_deref(),
            no_cache,
            hop,
        )
        .await,
        head,
    )
}

/// The exact one-field body `{"cwd": <boolean>}`. Like `JSON.parse`, a repeated key keeps the
/// last value; any other member or a non-object is refused.
fn exact_cwd(body: &[u8]) -> Option<bool> {
    let map = serde_json::from_slice::<serde_json::Map<String, Value>>(body).ok()?;
    (map.len() == 1)
        .then(|| map.get("cwd").and_then(Value::as_bool))
        .flatten()
}

#[allow(clippy::too_many_arguments)]
async fn composed(
    access: super::DashboardAccess,
    raw_query: Option<&str>,
    cross_site: bool,
    head: bool,
    accept_gzip: bool,
    if_none_match: Option<&str>,
    no_cache: bool,
    hop: Option<Hop>,
) -> Reply {
    if let Err(reply) = session(&access) {
        return reply;
    }
    if !access.same_origin || cross_site {
        return refused(
            403,
            "origin_required",
            "Account analytics requires the dashboard origin.",
        );
    }
    let providers: Vec<&str> = aac_providers::analytics_provider_table()
        .into_iter()
        .map(|(id, _, _)| id)
        .collect();
    let query = match admitted_analytics(raw_query, &providers) {
        Ok(query) => query,
        Err(reply) => return reply,
    };
    let principal = json!({
        "kind":"session",
        "id": access.session_key(),
        "role":"admin",
    })
    .to_string();
    let uri = match query.as_deref() {
        Some(query) => format!("/api/accounts/analytics?{query}"),
        None => "/api/accounts/analytics".to_owned(),
    };
    let force_refresh =
        url::form_urlencoded::parse(query.as_deref().unwrap_or_default().as_bytes())
            .any(|(key, value)| key == "refresh" && value == "true");
    let reload = force_refresh || no_cache;
    let mut hop_headers: Vec<(String, String)> = Vec::new();
    if !head && accept_gzip {
        hop_headers.push((
            crate::compress::ACCEPT_GZIP_HEADER.to_owned(),
            "1".to_owned(),
        ));
    }
    if !reload && let Some(inm) = if_none_match {
        hop_headers.push(("if-none-match".to_owned(), inm.to_owned()));
    }
    if no_cache {
        hop_headers.push(("cache-control".to_owned(), "no-cache".to_owned()));
    }
    if let Some(hop) = hop
        && let Some(reply) = hop(uri, principal, hop_headers).await
    {
        if reply.status == 503 {
            return unavailable();
        }
        if reply.status == 304 {
            let mut out = super::empty(304);
            if let Some(etag) = reply
                .headers
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok())
            {
                super::set_header(&mut out, "etag", etag);
            }
            if let Some(vary) = reply
                .headers
                .get(header::VARY)
                .and_then(|v| v.to_str().ok())
            {
                super::set_header(&mut out, "vary", vary);
            }
            return no_store(out);
        }
        let gzip = reply
            .headers
            .get(header::CONTENT_ENCODING)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|enc| enc.eq_ignore_ascii_case("gzip"));
        let raw_len = reply
            .headers
            .get(crate::compress::RAW_LENGTH_HEADER)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or_else(|| reply.body.len());
        let etag = reply
            .headers
            .get(header::ETAG)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
            .unwrap_or_else(|| crate::compress::weak_etag_sha1(&reply.body));
        if reply.status == 200 && !reload && crate::compress::not_modified(if_none_match, &etag) {
            let mut out = super::empty(304);
            super::set_header(&mut out, "etag", &etag);
            if raw_len >= crate::compress::MIN_COMPRESS_BYTES {
                crate::compress::append_vary(out.headers_mut(), "Accept-Encoding");
            }
            return no_store(out);
        }
        let mut out = super::json_bytes_tagged(reply.status, reply.body, raw_len, gzip, &etag);
        if raw_len >= crate::compress::MIN_COMPRESS_BYTES
            || reply.headers.contains_key(header::VARY)
        {
            crate::compress::append_vary(out.headers_mut(), "Accept-Encoding");
        }
        return no_store(out);
    }
    unavailable()
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROVIDERS: [&str; 3] = ["claude", "codex", "zai"];

    fn access() -> DashboardAccess {
        DashboardAccess {
            auth_enabled: true,
            principal: DashboardPrincipal::Browser {
                username: "fixture".into(),
            },
            secure_transport: true,
            same_origin: true,
            origin_present: true,
            local_access: true,
            session_id: Some("fixture".into()),
            credential_transport: "direct-loopback",
            direct_loopback: true,
            lan_proxy: false,
            client_address: None,
        }
    }

    async fn assert_retryable_unavailable(hop: Option<Hop>) {
        let reply = composed(
            access(),
            Some("range=all"),
            false,
            false,
            false,
            None,
            false,
            hop,
        )
        .await;
        assert_eq!(reply.status(), axum::http::StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(reply.headers()["cache-control"], "no-store");
        assert_eq!(reply.headers()["retry-after"], "1");
        let bytes = axum::body::to_bytes(reply.into_body(), 4096)
            .await
            .expect("unavailable body");
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).expect("unavailable json"),
            json!({
                "error": "Account analytics could not be read safely. Try again.",
                "code": "analytics_unavailable"
            })
        );
    }

    #[tokio::test]
    async fn an_owner_decline_is_a_retryable_public_unavailable_reply() {
        let hop: Hop = Arc::new(|_, _, _| {
            Box::pin(async {
                Some(super::super::owner_host::ServiceReply {
                    status: 503,
                    headers: axum::http::HeaderMap::new(),
                    body: axum::body::Bytes::from_static(
                        br#"{"error":"TypeScript read fallback required"}"#,
                    ),
                })
            })
        });
        assert_retryable_unavailable(Some(hop)).await;
    }

    #[tokio::test]
    async fn a_missing_owner_is_a_retryable_public_unavailable_reply() {
        assert_retryable_unavailable(None).await;
    }

    #[test]
    fn the_guard_admits_the_route_defaults() {
        assert!(matches!(admitted_analytics(None, &PROVIDERS), Ok(None)));
        assert_eq!(
            admitted_analytics(Some("range=7d&tz=UTC"), &PROVIDERS).ok(),
            Some(Some("range=7d&tz=UTC".to_owned()))
        );
        assert_eq!(
            admitted_analytics(
                Some("range=custom&from=2026-11-01&to=2026-11-19"),
                &PROVIDERS
            )
            .ok(),
            Some(Some(
                "range=custom&from=2026-11-01&to=2026-11-19".to_owned()
            ))
        );
    }

    #[test]
    fn the_guard_refuses_each_named_error() {
        for query in [
            "fixture=invalid",
            "range=7d&range=30d",
            "platform=linux",
            "account=-bad",
            "refresh=yes",
        ] {
            assert!(admitted_analytics(Some(query), &PROVIDERS).is_err());
        }
        for query in [
            "range=8d",
            "range=custom",
            "range=custom&from=2026-11-02&to=2026-11-01",
        ] {
            assert!(
                admitted_analytics(Some(query), &PROVIDERS).is_err(),
                "{query}"
            );
        }
        assert!(admitted_analytics(Some("range=7d&from=2026-11-01"), &PROVIDERS).is_err());
        assert!(admitted_analytics(Some("tz=+05:00"), &PROVIDERS).is_err());
        assert!(admitted_analytics(Some("provider=retired"), &PROVIDERS).is_err());
        assert!(admitted_analytics(Some("provider=other"), &PROVIDERS).is_ok());
        assert!(admitted_analytics(Some("provider=all"), &PROVIDERS).is_ok());
    }

    #[test]
    fn the_capture_body_is_exactly_one_boolean() {
        assert_eq!(exact_cwd(br#"{"cwd":true}"#), Some(true));
        assert_eq!(exact_cwd(br#"{"cwd":false}"#), Some(false));
        for body in [
            &br#"{}"#[..],
            br#"{"cwd":1}"#,
            br#"{"cwd":true,"x":1}"#,
            br#"[]"#,
        ] {
            assert_eq!(exact_cwd(body), None, "{}", String::from_utf8_lossy(body));
        }
    }

    #[test]
    fn capture_round_trips_and_turning_off_clears_remote_folders() {
        let dir = tempfile::tempdir().expect("temp");
        let root = dir.path();
        assert_eq!(read_capture(root), json!({ "cwd": true }));
        save_capture(root, false).expect("write");
        assert_eq!(read_capture(root), json!({ "cwd": false }));
        let caches = root.join("cache/analytics-remote-v1");
        std::fs::create_dir_all(&caches).expect("caches");
        std::fs::write(
            caches.join("mac.json"),
            br#"{"sessions":{"s1":{"cwd":"/x"},"s2":{"cwd":null}}}"#,
        )
        .expect("cache");
        assert_eq!(clear_remote_cwds(root), Ok((1, 0)));
        let saved: Value =
            serde_json::from_slice(&std::fs::read(caches.join("mac.json")).expect("read"))
                .expect("json");
        assert_eq!(saved["sessions"]["s1"]["cwd"], Value::Null);
        // A cache the loader would discard is removed, never left holding folders.
        std::fs::write(caches.join("windows.json"), b"not json").expect("cache");
        assert_eq!(clear_remote_cwds(root), Ok((0, 1)));
        assert!(!caches.join("windows.json").exists());
    }

    #[test]
    fn calendar_dates_are_real_days_from_1970() {
        assert!(calendar_date("2026-11-19").is_some());
        assert!(calendar_date("2024-02-29").is_some());
        for value in [
            "1969-12-31",
            "2026-02-29",
            "2026-13-01",
            "2026-1-01",
            "2026-11-32",
        ] {
            assert!(calendar_date(value).is_none(), "{value}");
        }
    }
}
