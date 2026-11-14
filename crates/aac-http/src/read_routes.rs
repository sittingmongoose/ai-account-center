//! Read-route candidates. Inputs come only from guarded TS handlers over the existing source
//! owners. Supported raw inputs compose here; unsupported shapes request logged TS fallback.
use axum::{
    Json,
    body::to_bytes,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    snapshot: Value,
}

pub async fn answer(req: Request) -> Response {
    let path = req.uri().path().to_owned();
    let bytes = match to_bytes(req.into_body(), 4 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return fallback(),
    };
    let result = tokio::task::spawn_blocking(move || {
        if path.starts_with("/api/status/files/") || path == "/api/status/deliverable" {
            return crate::read_files::answer(&path, &bytes);
        }
        if (path == "/api/scoreboard" || path == "/api/status")
            && serde_json::from_slice::<Value>(&bytes)
                .ok()?
                .get("data")
                .is_some()
        {
            let envelope =
                crate::json_object::from_slice(&bytes, &["data", "presentation"]).ok()?;
            return if path == "/api/status" {
                aac_status::read_status::compose(envelope)
            } else {
                aac_status::read_scoreboard::compose(envelope)
            };
        }
        let input: Snapshot = crate::json_object::from_slice(&bytes, &["snapshot"]).ok()?;
        match path.as_str() {
            "/api/store/status" => aac_store::read_status::from_writer(&input.snapshot),
            "/api/status" => aac_status::http_views::status(&input.snapshot),
            "/api/scoreboard" => aac_status::http_views::scoreboard(&input.snapshot),
            _ => None,
        }
    })
    .await;
    match result {
        Ok(Some(value)) => Json(value).into_response(),
        _ => fallback(),
    }
}

fn fallback() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error":"TypeScript read fallback required"})),
    )
        .into_response()
}
