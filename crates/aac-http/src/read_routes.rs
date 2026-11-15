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
use std::path::PathBuf;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    snapshot: Value,
}

/// `root` is the service's own canonical boot root (never a path from the TS input).
pub async fn answer(req: Request, root: PathBuf) -> Response {
    let path = req.uri().path().to_owned();
    let query = req.uri().query().is_some();
    let bytes = match to_bytes(req.into_body(), 4 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(_) => return fallback(),
    };
    let result = tokio::task::spawn_blocking(move || {
        // D7: Home's view from the page's context and the three answers it reads, in the envelope.
        if path == "/api/views/home" {
            return if query {
                None
            } else {
                let envelope =
                    crate::json_object::from_slice(&bytes, &["data", "presentation"]).ok()?;
                aac_views::home::compose(envelope).ok()
            };
        }
        if path.starts_with("/api/accounts/") {
            // TS refused any query before forwarding; anything else is not the read it proved.
            return if query {
                None
            } else {
                crate::account_reads::answer(&path, &root, &bytes)
            };
        }
        if ["/api/claude/", "/api/codex/", "/api/antigravity/"]
            .iter()
            .any(|prefix| path.starts_with(prefix))
        {
            // TS consumed its query (platform, refresh) before forwarding the bare route.
            return if query {
                None
            } else {
                crate::profile_reads::answer(&path, &root, &bytes)
            };
        }
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
