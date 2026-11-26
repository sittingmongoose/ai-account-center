//! Public lifecycle routes on the authenticated Rust dashboard.
use super::{DashboardAccess, DashboardPrincipal, Reply, error, response};
use axum::{extract::Request, http::Method, response::IntoResponse};
use serde_json::json;
use std::path::Path;

#[derive(Clone, Copy)]
pub(super) enum Route {
    SigninCommand,
    Trash,
}

impl Route {
    pub(super) fn of(method: &Method, path: &str) -> Option<Self> {
        let path = path.strip_suffix('/').unwrap_or(path);
        if !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
            return None;
        }
        match path {
            "/api/accounts/signin-command" => Some(Self::SigninCommand),
            "/api/accounts/trash" => Some(Self::Trash),
            _ => None,
        }
    }
}

pub(super) fn admit(request: &Request) -> Result<&DashboardAccess, Reply> {
    let access = request
        .extensions()
        .get::<DashboardAccess>()
        .ok_or_else(super::service_error)?;
    match access.principal {
        DashboardPrincipal::Anonymous => {
            Err(error(401, "auth_required", "Authentication required"))
        }
        DashboardPrincipal::Device { .. } => Err(super::routes::device_scope()),
        DashboardPrincipal::Browser { .. } => Ok(access),
    }
}

pub(super) async fn mutation_body(
    request: Request,
) -> Result<serde_json::Map<String, serde_json::Value>, Reply> {
    mutation_body_capped(request, BODY_CAP).await
}

pub(super) async fn mutation_body_capped(
    request: Request,
    cap: usize,
) -> Result<serde_json::Map<String, serde_json::Value>, Reply> {
    mutation_admission_capped(&request, cap)?;
    // The front door already imposed Express's parser cap. Lifecycle also checks
    // the serialized object, including when the request was sent in chunks.
    let bytes = axum::body::to_bytes(request.into_body(), 100 * 1024)
        .await
        .map_err(|_| super::invalid_body())?;
    if bytes.is_empty() {
        return Ok(serde_json::Map::new());
    }
    let body: serde_json::Map<String, serde_json::Value> =
        serde_json::from_slice(&bytes).map_err(|_| super::invalid_body())?;
    if serde_json::to_vec(&body).map_or(true, |bytes| bytes.len() > cap) {
        return Err(super::invalid_body());
    }
    Ok(body)
}

const BODY_CAP: usize = 8 * 1024;
pub(super) const WEB_SESSION_BODY_CAP: usize = 48 * 1024;

pub(super) fn mutation_admission(request: &Request) -> Result<(), Reply> {
    mutation_admission_capped(request, BODY_CAP)
}

pub(super) fn mutation_admission_capped(request: &Request, cap: usize) -> Result<(), Reply> {
    use axum::http::header::{CONTENT_LENGTH, TRANSFER_ENCODING};

    let access = admit(request)?;
    if request.uri().query().is_some() {
        return Err(unexpected_query());
    }
    if !access.origin_present || !access.same_origin {
        return Err(error(
            403,
            "origin_required",
            "This change requires the dashboard origin.",
        ));
    }
    let declared = request.headers().get(CONTENT_LENGTH);
    if (declared.is_none() && !request.headers().contains_key(TRANSFER_ENCODING))
        || !super::native_owners::json_content(request)
    {
        return Err(error(
            415,
            "json_required",
            "This change requires application/json.",
        ));
    }
    if declared
        .and_then(|length| length.to_str().ok()?.parse::<usize>().ok())
        .is_some_and(|length| length > cap)
    {
        return Err(super::invalid_body());
    }
    Ok(())
}

fn signin_command(query: &str) -> Reply {
    let mut provider = None;
    let mut profile = None;
    for (key, value) in url::form_urlencoded::parse(query.as_bytes()) {
        match key.as_ref() {
            "provider" if provider.is_none() => provider = Some(value),
            "profile" if profile.is_none() => profile = Some(value),
            _ => return invalid_action_body(),
        }
    }
    let Some(profile) = profile.filter(|name| crate::antigravity_runtime::profile_id_ok(name))
    else {
        return invalid_action_body();
    };
    if provider.as_deref() != Some("antigravity") {
        return invalid_action_body();
    }
    response(
        200,
        json!({"host":"ubuntu","command":format!("ai-account-center antigravity signin {profile}")}),
    )
}

pub(super) fn invalid_action_body() -> Reply {
    error(400, "invalid_body", "The request could not be completed.")
}

pub(super) fn options(allow: &'static str, length: &'static str) -> Reply {
    (
        [
            (axum::http::header::ALLOW, allow),
            (axum::http::header::CONTENT_TYPE, "text/plain"),
            (axum::http::header::CONTENT_LENGTH, length),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        allow,
    )
        .into_response()
}

pub(super) fn unexpected_query() -> Reply {
    error(
        400,
        "unexpected_query",
        "This request does not take a query string.",
    )
}

pub(super) async fn handle(root: &Path, route: Route, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return options("GET, HEAD", "9");
    }
    if let Err(reply) = admit(&request) {
        return reply;
    }
    let query = request.uri().query();
    if query.is_some() && matches!(route, Route::Trash) {
        return unexpected_query();
    }
    let mut reply = match route {
        Route::SigninCommand => signin_command(query.unwrap_or_default()),
        Route::Trash => {
            let root = root.to_path_buf();
            let entries =
                tokio::task::spawn_blocking(move || crate::claude_account_store::list_trash(&root))
                    .await
                    .ok()
                    .flatten();
            match entries {
                Some(entries) => response(200, json!({"entries":entries})),
                None => error(
                    500,
                    "registry_unavailable",
                    "Account list could not be read safely.",
                ),
            }
        }
    };
    super::set_header(&mut reply, "cache-control", "no-store");
    reply
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use axum::body::{Body, to_bytes};

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

    async fn request(root: &Path, uri: &str, status: u16, expected: &str) {
        let mut request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        request.extensions_mut().insert(access());
        let route = Route::of(request.method(), request.uri().path()).unwrap();
        let reply = handle(root, route, request).await;
        assert_eq!(reply.status(), status);
        assert_eq!(reply.headers()["cache-control"], "no-store");
        assert_eq!(
            reply.headers()["content-type"],
            "application/json; charset=utf-8"
        );
        assert_eq!(
            reply.headers()["content-length"].to_str().unwrap(),
            expected.len().to_string()
        );
        assert_eq!(
            to_bytes(reply.into_body(), 2048).await.unwrap().as_ref(),
            expected.as_bytes()
        );
    }

    #[tokio::test]
    async fn command_uses_only_the_two_validated_query_fields() {
        let root = tempfile::tempdir().unwrap();
        request(
            root.path(),
            "/api/accounts/signin-command?provider=antigravity&profile=a%2Db",
            200,
            r#"{"host":"ubuntu","command":"ai-account-center antigravity signin a-b"}"#,
        )
        .await;
        for query in [
            "",
            "?provider=antigravity&profile=a&profile=b",
            "?provider=antigravity&profile=a%3Bcommand",
            "?provider=antigravity&profile=a&extra=b",
        ] {
            request(
                root.path(),
                &format!("/api/accounts/signin-command{query}"),
                400,
                r#"{"error":"The request could not be completed.","code":"invalid_body"}"#,
            )
            .await;
        }
    }

    #[tokio::test]
    async fn trash_serializes_only_the_public_fields_and_refuses_a_malformed_store() {
        let root = tempfile::tempdir().unwrap();
        let route = "/api/accounts/trash";
        request(root.path(), route, 200, r#"{"entries":[]}"#).await;
        let directory = root.path().join("accounts");
        std::fs::create_dir(&directory).unwrap();
        let row = json!({
            "trashId":"tr_0123456789abcdef","provider":"claude","accountId":"claude:work",
            "label":"Work","source":"inventory","hosts":{"mac":{"trashName":"work-20261119T000000Z",
            "launcher":{"launcherName":"Example","profilePath":"/fixture/profile","sshHost":"test-mac"}}},
            "entry":{"email":"example@example.test","privatePath":"/fixture/profile"},
            "trashedAt":"2026-11-19T00:00:00Z","purgeAfter":"2026-12-19T00:00:00.000Z",
            "state":"deleting"
        });
        let file = directory.join("trash.json");
        std::fs::write(&file, json!({"version":1,"entries":[row]}).to_string()).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        request(root.path(),route,200,
            r#"{"entries":[{"trashId":"tr_0123456789abcdef","provider":"claude","label":"Work","trashedAt":"2026-11-19T00:00:00Z","purgeAfter":"2026-12-19T00:00:00.000Z","state":"deleting"}]}"#).await;
        request(
            root.path(),
            "/api/accounts/trash?refresh=true",
            400,
            r#"{"error":"This request does not take a query string.","code":"unexpected_query"}"#,
        )
        .await;
        std::fs::write(file, "{").unwrap();
        request(
            root.path(),
            route,
            500,
            r#"{"error":"Account list could not be read safely.","code":"registry_unavailable"}"#,
        )
        .await;
    }
}
