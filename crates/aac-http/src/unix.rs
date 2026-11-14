use aac_api::{AccountRefreshSettings, VerifiedPrincipal};
use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::Deserialize;
use serde_json::json;
use std::{
    fs, io,
    os::unix::{
        fs::{MetadataExt, PermissionsExt},
        net::UnixListener as StdListener,
    },
    path::PathBuf,
    sync::Arc,
};
use subtle::ConstantTimeEq;
use tokio::net::UnixListener;

const SECRET_HEADER: &str = "x-aac-boot-secret";
const PRINCIPAL_HEADER: &str = "x-aac-principal";
const ROUTES: &str = include_str!("../../../src/web-server/rust-service-routes.json");

/// Per-service-boot configuration, delivered through a private stdin pipe, never argv.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Boot {
    /// Absolute AAC private directory, explicitly supplied by TS (no home discovery).
    pub ccs_dir: PathBuf,
    /// Random private boot directory's socket, under `ccs_dir/run`.
    pub socket_path: PathBuf,
    /// 32 random bytes encoded as lowercase hex; kept in memory for this service boot.
    pub secret: String,
    /// Public random correlation id, distinct from the secret.
    pub boot_id: String,
}

#[derive(Clone)]
struct AppState(Arc<Boot>);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Route {
    method: String,
    path: String,
    owner: String,
}

fn failure() -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        "invalid private service boundary",
    )
}

fn private_dir(path: &std::path::Path) -> io::Result<()> {
    let meta = fs::symlink_metadata(path)?;
    if !meta.is_dir() || meta.permissions().mode() & 0o777 != 0o700 {
        return Err(failure());
    }
    Ok(())
}

impl Boot {
    /// Parse only the exact object envelope supplied by TS; arrays are never positional fields.
    pub fn from_json(bytes: &[u8]) -> serde_json::Result<Self> {
        crate::json_object::from_slice(bytes, &["ccsDir", "socketPath", "secret", "bootId"])
    }

    fn validate(&self) -> io::Result<()> {
        if !self.ccs_dir.is_absolute()
            || fs::canonicalize(&self.ccs_dir)? != self.ccs_dir
            || self.secret.len() != 64
            || !self
                .secret
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.boot_id.len() != 24
            || !self.boot_id.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(failure());
        }
        let run = self.ccs_dir.join("run");
        let boot = run.join(format!("aac-http-{}", self.boot_id));
        if self.socket_path != boot.join("http.sock") {
            return Err(failure());
        }
        private_dir(&run)?;
        private_dir(&boot)?;
        if fs::symlink_metadata(&run)?.uid() != fs::metadata(&self.ccs_dir)?.uid()
            || fs::symlink_metadata(&boot)?.uid() != fs::metadata(&self.ccs_dir)?.uid()
        {
            return Err(failure());
        }
        Ok(())
    }
}

fn one_header<'a>(headers: &'a HeaderMap, key: &str) -> Option<&'a str> {
    let mut values = headers.get_all(key).iter();
    let first = values.next()?.to_str().ok()?;
    if values.next().is_some() {
        return None;
    }
    Some(first)
}

async fn boundary(State(state): State<AppState>, mut req: Request, next: Next) -> Response {
    let principal = (|| {
        let secret = one_header(req.headers(), SECRET_HEADER)?;
        if secret.len() != 64 || !bool::from(secret.as_bytes().ct_eq(state.0.secret.as_bytes())) {
            return None;
        }
        let encoded = one_header(req.headers(), PRINCIPAL_HEADER)?;
        if encoded.len() > 512 {
            return None;
        }
        let p: VerifiedPrincipal =
            crate::json_object::from_slice(encoded.as_bytes(), &["kind", "id", "role"]).ok()?;
        p.valid().then_some(p)
    })();
    let Some(principal) = principal else {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Authentication required"})),
        )
            .into_response();
    };
    req.extensions_mut().insert(principal);
    let mut response = next.run(req).await;
    if let Ok(value) = HeaderValue::from_str(&state.0.boot_id) {
        response.headers_mut().insert("x-aac-service-boot", value);
    }
    response
}

async fn route(State(state): State<AppState>, req: Request) -> Response {
    let Some(principal) = req.extensions().get::<VerifiedPrincipal>() else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    match req.uri().path() {
        "/api/health" => Json(aac_diag::health()).into_response(),
        "/api/accounts/settings" | "/api/diag" if !principal.dashboard_session() => (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":"Authentication required"})),
        )
            .into_response(),
        "/api/diag" => Json(aac_diag::diagnostics()).into_response(),
        "/api/accounts/settings" => {
            if req.uri().query().is_some_and(|q| !q.is_empty()) {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(json!({"error":"Unexpected settings query."})),
                )
                    .into_response();
            }
            let root = state.0.ccs_dir.clone();
            match tokio::task::spawn_blocking(move || read_settings(&root)).await {
                Ok(Some(value)) => Json(value).into_response(),
                Ok(None) | Err(_) => (
                    StatusCode::SERVICE_UNAVAILABLE,
                    Json(json!({"error":"TypeScript settings fallback required"})),
                )
                    .into_response(),
            }
        }
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

fn read_settings(root: &std::path::Path) -> Option<AccountRefreshSettings> {
    let bytes = aac_files::read_bounded(&root.join("account-refresh-settings.json"), 4096).ok()?;
    // Require the raw object and exact unique keys before the strict integer DTO parse.
    // Original bytes are retained: no Value normalization or positional struct decoding.
    // These small integers are exact in JS; every other input delegates to the TS owner.
    let value: AccountRefreshSettings =
        crate::json_object::from_slice(&bytes, &["refreshIntervalSeconds"]).ok()?;
    (30..=3600)
        .contains(&value.refresh_interval_seconds)
        .then_some(value)
}

struct SocketGuard {
    path: PathBuf,
    inode: u64,
}
impl Drop for SocketGuard {
    fn drop(&mut self) {
        if fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.inode) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Bind only an AF_UNIX socket, set 0600 before accepting, and stop on the supplied signal.
/// Existing paths are never unlinked; ownership of the created socket is tracked by inode.
pub async fn serve(
    boot: Boot,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> io::Result<()> {
    boot.validate()?;
    let listener = StdListener::bind(&boot.socket_path)?;
    let _guard = SocketGuard {
        path: boot.socket_path.clone(),
        inode: fs::symlink_metadata(&boot.socket_path)?.ino(),
    };
    fs::set_permissions(&boot.socket_path, fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let listener = UnixListener::from_std(listener)?;
    let routes: Vec<Box<serde_json::value::RawValue>> =
        serde_json::from_str(ROUTES).map_err(|_| failure())?;
    let state = AppState(Arc::new(boot));
    let mut router = Router::new();
    for raw in routes {
        let entry: Route =
            crate::json_object::from_slice(raw.get().as_bytes(), &["method", "path", "owner"])
                .map_err(|_| failure())?;
        if entry.owner == "rust" {
            if entry.method != "GET"
                || !matches!(
                    entry.path.as_str(),
                    "/api/health" | "/api/diag" | "/api/accounts/settings"
                )
            {
                return Err(failure());
            }
            router = router.route(&entry.path, get(route));
        } else if entry.owner != "ts-fallback" {
            return Err(failure());
        }
    }
    // Global layer protects even unknown routes and unsupported methods.
    let router = router
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state);
    tracing::info!(target: "aac_http", "private service ready");
    axum::serve(listener, router)
        .with_graceful_shutdown(shutdown)
        .await
}

#[cfg(test)]
mod dto_tests {
    use super::*;
    #[test]
    fn boot_principal_and_route_dtos_cannot_use_sequence_as_struct() {
        let boot = br#"{"ccsDir":"/tmp/fixture","socketPath":"/tmp/fixture/socket","secret":"fixture","bootId":"fixture"}"#;
        assert!(Boot::from_json(boot).is_ok());
        for raw in [
            r#"["/tmp/fixture","/tmp/fixture/socket","fixture","fixture"]"#,
            "null",
            r#"{"ccsDir":"fixture","socketPath":"fixture","secret":"fixture","bootId":"fixture","extra":0}"#,
            r#"{"ccsDir":"fixture","socketPath":"fixture","secret":"fixture","bootId":"fixture","bootId":"other"}"#,
        ] {
            assert!(Boot::from_json(raw.as_bytes()).is_err());
        }
        for raw in [
            r#"["session","fixture-admin","admin"]"#,
            "null",
            r#"{"kind":"session","id":"fixture-admin","role":"admin","extra":0}"#,
            r#"{"kind":"session","id":"fixture-admin","role":"admin","role":"admin"}"#,
        ] {
            assert!(
                crate::json_object::from_slice::<VerifiedPrincipal>(
                    raw.as_bytes(),
                    &["kind", "id", "role"]
                )
                .is_err()
            );
        }
        for raw in [
            r#"["GET","/api/health","rust"]"#,
            "null",
            r#"{"method":"GET","path":"/api/health","owner":"rust","extra":0}"#,
            r#"{"method":"GET","path":"/api/health","owner":"rust","owner":"rust"}"#,
        ] {
            assert!(
                crate::json_object::from_slice::<Route>(
                    raw.as_bytes(),
                    &["method", "path", "owner"]
                )
                .is_err()
            );
        }
    }
}
