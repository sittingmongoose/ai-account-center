//! L1 reply floor: for a route group Rust owns, the service sends the final bytes, the `ETag`
//! and `x-aac-view: <name>@<version>`, and the front door writes them without parsing. The ETag
//! follows `revalidated-view.ts`: SHA-256 of the view's JSON with the clock-only keys
//! (`generatedAt`, `ageSeconds`, `cacheAgeSeconds`) left out at any depth, written `W/"<hex>"`.
//! R1 (Home) is the first route group that uses it ([`respond_bytes`]).
use crate::compress::RAW_LENGTH_HEADER;
use axum::{
    body::Bytes,
    http::{HeaderName, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// The clock-only keys the identity leaves out, as `sendRevalidatedView` does.
const CLOCK_KEYS: [&str; 3] = ["generatedAt", "ageSeconds", "cacheAgeSeconds"];

fn without_clock(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .filter(|(key, _)| !CLOCK_KEYS.contains(&key.as_str()))
                .map(|(key, value)| (key.clone(), without_clock(value)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(without_clock).collect()),
        other => other.clone(),
    }
}

/// Lowercase hex of a SHA-256 digest: one String, no per-byte allocation (the ETag is written per request).
pub(crate) fn hex_digest(digest: &[u8; 32]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";
    let mut out = [0u8; 64];
    for (i, b) in digest.iter().enumerate() {
        out[2 * i] = HEX[(b >> 4) as usize];
        out[2 * i + 1] = HEX[(b & 0xf) as usize];
    }
    out.into_iter().map(char::from).collect()
}

/// `W/"<sha256 of the identity JSON>"`, the revalidated-view rule.
pub fn etag(view: &Value) -> String {
    let identity = without_clock(view).to_string();
    let digest: [u8; 32] = Sha256::digest(identity.as_bytes()).into();
    format!("W/\"{}\"", hex_digest(&digest))
}

/// One pre-encoded view answer from a service or local owner: either `304 Not Modified`
/// (short-circuited before splicing or copying) or a `200 OK` payload carrying shared `Bytes`
/// (raw or pre-compressed gzip) plus its uncompressed length and `ETag`.
pub(crate) enum EncodedView {
    NotModified {
        etag: String,
    },
    Payload {
        bytes: Bytes,
        raw_len: usize,
        gzip: bool,
        etag: String,
    },
}

/// The response a view route sends: the serialized bytes, its ETag and its view version.
/// `name` and `version` are fixed words of the route (`home`, 3).
pub fn respond(name: &'static str, version: u32, view: &Value) -> Response {
    let bytes = view.to_string();
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(&etag(view)) {
        headers.insert(header::ETAG, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("{name}@{version}")) {
        headers.insert("x-aac-view", value);
    }
    response
}

/// R1: the response of a view whose bytes the service wrote itself (`aac_views::js_json`, exactly as Node writes
/// them), with the ETag of its revalidated-view identity text (the same bytes without the clock-only members).
pub fn respond_bytes(
    name: &'static str,
    version: u32,
    bytes: Vec<u8>,
    identity: &[u8],
) -> Response {
    let digest: [u8; 32] = Sha256::digest(identity).into();
    respond_tagged(
        name,
        version,
        bytes,
        &format!("W/\"{}\"", hex_digest(&digest)),
    )
}

/// R2: [`respond_bytes`] with the ETag already known (Status keeps the last one per generations and second, and
/// reuses it only for the same identity bytes).
pub fn respond_tagged(name: &'static str, version: u32, bytes: Vec<u8>, etag: &str) -> Response {
    let mut response = (StatusCode::OK, bytes).into_response();
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    if let Ok(value) = HeaderValue::from_str(etag) {
        headers.insert(header::ETAG, value);
    }
    if let Ok(value) = HeaderValue::from_str(&format!("{name}@{version}")) {
        headers.insert("x-aac-view", value);
    }
    response
}

/// Respond from an [`EncodedView`]: `304 Not Modified` with empty body or `200 OK` with shared
/// `Bytes` (pre-compressed gzip or raw JSON) and its view/ETag headers.
pub(crate) fn respond_encoded(name: &'static str, version: u32, view: EncodedView) -> Response {
    match view {
        EncodedView::NotModified { etag } => {
            let mut response = StatusCode::NOT_MODIFIED.into_response();
            let headers = response.headers_mut();
            if let Ok(value) = HeaderValue::from_str(&etag) {
                headers.insert(header::ETAG, value);
            }
            if let Ok(value) = HeaderValue::from_str(&format!("{name}@{version}")) {
                headers.insert("x-aac-view", value);
            }
            response
        }
        EncodedView::Payload {
            bytes,
            raw_len,
            gzip,
            etag,
        } => {
            let mut response = (StatusCode::OK, bytes).into_response();
            let headers = response.headers_mut();
            headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/json; charset=utf-8"),
            );
            if let Ok(value) = HeaderValue::from_str(&etag) {
                headers.insert(header::ETAG, value);
            }
            if let Ok(value) = HeaderValue::from_str(&format!("{name}@{version}")) {
                headers.insert("x-aac-view", value);
            }
            if gzip {
                headers.insert(header::CONTENT_ENCODING, HeaderValue::from_static("gzip"));
                if let Ok(value) = HeaderValue::from_str(&raw_len.to_string()) {
                    headers.insert(HeaderName::from_static(RAW_LENGTH_HEADER), value);
                }
            }
            response
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn the_etag_leaves_out_clock_keys_at_any_depth_and_keeps_order() {
        let a = json!({"b":1,"generatedAt":"x","rows":[{"ageSeconds":3,"v":"é"}]});
        let b = json!({"b":1,"generatedAt":"y","rows":[{"ageSeconds":9,"v":"é"}]});
        assert_eq!(etag(&a), etag(&b));
        assert_ne!(etag(&a), etag(&json!({"rows":[{"v":"é"}],"b":1})));
        // Node: createHash('sha256').update(JSON.stringify({b:1,rows:[{v:'é'}]})).digest('hex')
        assert_eq!(
            etag(&a),
            "W/\"29e5acf9fc8e2d7335f721cb272c1dd7b2b750a44e189a682d940167075d2180\""
        );
    }
}
