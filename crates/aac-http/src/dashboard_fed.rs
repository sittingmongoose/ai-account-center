//! G2: the Accounts and tray response from the dashboard generation shared with Home.
use crate::account_usage::AccountUsageCache;
use crate::account_usage::{prepared, source};
use crate::feed::Feeds;
use aac_views::decline::Decline;
use axum::{
    extract::Query,
    http::{HeaderMap, HeaderValue, StatusCode, Uri, header},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

pub(crate) const VIEW: &str = "accounts-dashboard";
pub(crate) const VERSION: u32 = 1;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    platform: String,
    secure: bool,
    now: i64,
    updated_at: String,
    dashboard: String,
}

pub(crate) fn answer(feeds: &Feeds, header: &str) -> Result<(Vec<u8>, Vec<u8>), Decline> {
    let request: Request = crate::json_object::from_slice(
        header.as_bytes(),
        &["platform", "secure", "now", "updatedAt", "dashboard"],
    )
    .map_err(|_| Decline::Shape)?;
    if !["mac", "windows"].contains(&request.platform.as_str()) {
        return Err(Decline::Shape);
    }
    let source = source(&request.platform, request.secure);
    let generation = feeds.current(&source, &request.dashboard)?;
    let dashboard = prepared(&generation)?;
    let mut identity = Vec::with_capacity(64 + header.len());
    identity.extend_from_slice(env!("AAC_BUILD_COMMIT").as_bytes());
    identity.extend_from_slice(&generation.digest);
    identity.extend_from_slice(header.as_bytes());
    Ok((dashboard.render(&request.updated_at, request.now), identity))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DashboardQuery {
    platform: Option<String>,
    refresh: Option<String>,
}

pub(crate) fn invalid_selection() -> Response {
    crate::public_auth::plain(400, "Select a valid desktop platform and refresh value.")
}

pub(crate) fn selection(uri: &Uri) -> Option<(&'static str, bool)> {
    let Query(query) = Query::<DashboardQuery>::try_from_uri(uri).ok()?;
    let platform = match query.platform.as_deref().unwrap_or("mac") {
        "mac" => "mac",
        "windows" => "windows",
        _ => return None,
    };
    let refresh = match query.refresh.as_deref().unwrap_or("false") {
        "false" => false,
        "true" => true,
        _ => return None,
    };
    Some((platform, refresh))
}

fn invalid() -> Response {
    (
        StatusCode::BAD_REQUEST,
        axum::Json(serde_json::json!({
            "error":"Select a valid desktop platform and refresh value."
        })),
    )
        .into_response()
}

// The authenticated front door sets this private header after checking TLS.
pub(crate) const SECURE_HEADER: &str = "x-aac-dashboard-secure";

pub(crate) fn latest(
    cache: &AccountUsageCache,
    uri: &Uri,
    headers: &HeaderMap,
) -> Result<Response, Decline> {
    let mut scopes = headers.get_all(SECURE_HEADER).iter();
    let secure = match (scopes.next().map(HeaderValue::to_str), scopes.next()) {
        (None | Some(Ok("0")), None) => false,
        (Some(Ok("1")), None) => true,
        _ => return Ok(invalid()),
    };
    latest_with(cache, uri, headers, secure, false, false)
}

/// Serve the same generation to a caller whose transport was already authenticated.
pub(crate) fn latest_for_access(
    cache: &AccountUsageCache,
    uri: &Uri,
    headers: &HeaderMap,
    secure: bool,
    head: bool,
) -> Result<Response, Decline> {
    latest_with(cache, uri, headers, secure, true, head)
}

fn latest_with(
    cache: &AccountUsageCache,
    uri: &Uri,
    headers: &HeaderMap,
    secure: bool,
    public: bool,
    head: bool,
) -> Result<Response, Decline> {
    let Some((platform, refresh)) = selection(uri) else {
        return Ok(if public {
            invalid_selection()
        } else {
            invalid()
        });
    };
    if refresh {
        cache.request_refresh();
    }
    let generation = cache.feeds.latest(&source(platform, secure))?;
    let updated = generation
        .id
        .parse::<i64>()
        .ok()
        .and_then(|id| aac_num::ecma::to_iso_string(id / 1000))
        .ok_or(Decline::Date)?;
    let now = aac_core::UtcMs::now().map_err(|_| Decline::Date)?.get() as i64;
    let dashboard = prepared(&generation)?;
    let bytes = dashboard.render(&updated, now);
    let reload = refresh || crate::compress::has_no_cache(headers);
    let if_none_match = headers
        .get(header::IF_NONE_MATCH)
        .and_then(|value| value.to_str().ok());
    if public {
        let raw_len = bytes.len();
        let etag = crate::compress::weak_etag_sha1(&bytes);
        let mut response = if !reload && crate::compress::not_modified(if_none_match, &etag) {
            let mut out = StatusCode::NOT_MODIFIED.into_response();
            if let Ok(val) = HeaderValue::from_str(&etag) {
                out.headers_mut().insert(header::ETAG, val);
            }
            if raw_len >= crate::compress::MIN_COMPRESS_BYTES {
                crate::compress::append_vary(out.headers_mut(), "Accept-Encoding");
            }
            out
        } else if !head
            && crate::compress::accepts_gzip(headers)
            && let Some(gz) = crate::compress::compress_for_tag(Some(&etag), &bytes)
        {
            let mut out = crate::public_auth::json_bytes_tagged(200, gz, raw_len, true, &etag);
            crate::compress::append_vary(out.headers_mut(), "Accept-Encoding");
            out
        } else {
            let mut out = crate::public_auth::json_bytes_tagged(
                200,
                axum::body::Bytes::from(bytes),
                raw_len,
                false,
                &etag,
            );
            if raw_len >= crate::compress::MIN_COMPRESS_BYTES {
                crate::compress::append_vary(out.headers_mut(), "Accept-Encoding");
            }
            out
        };
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return Ok(response);
    }
    let identity = Sha256::digest(&bytes);
    let mut response = crate::view_reply::respond_bytes(VIEW, VERSION, bytes, &identity);
    if !reload
        && let Some(current) = response
            .headers()
            .get(header::ETAG)
            .and_then(|etag| etag.to_str().ok())
        && crate::compress::not_modified(if_none_match, current)
    {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        response.headers_mut().remove(header::CONTENT_TYPE);
        response.headers_mut().remove(header::CONTENT_LENGTH);
        *response.body_mut() = axum::body::Body::empty();
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(response)
}
