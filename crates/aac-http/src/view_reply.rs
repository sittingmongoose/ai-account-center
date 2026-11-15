//! L1 reply floor: for a route group Rust owns, the service sends the final bytes, the `ETag`
//! and `x-aac-view: <name>@<version>`, and the front door writes them without parsing. The ETag
//! follows `revalidated-view.ts`: SHA-256 of the view's JSON with the clock-only keys
//! (`generatedAt`, `ageSeconds`, `cacheAgeSeconds`) left out at any depth, written `W/"<hex>"`.
//! No route group is owned yet in L1 (R1 is the first); this is the shared piece they use.
use axum::{
    http::{HeaderValue, StatusCode, header},
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

/// `W/"<sha256 of the identity JSON>"`, the revalidated-view rule.
pub fn etag(view: &Value) -> String {
    let identity = without_clock(view).to_string();
    let digest = Sha256::digest(identity.as_bytes());
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("W/\"{hex}\"")
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
