//! Read-only cut routes handed directly to the local Rust collection service.
use super::{DashboardAccess, DashboardPrincipal, Reply, storage};
use aac_files::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use axum::{
    body::{Body, to_bytes},
    extract::Request,
    http::{HeaderMap, Method, StatusCode, Uri, header, uri::PathAndQuery},
};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use serde_json::json;
use std::{
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::{net::UnixStream, sync::Semaphore};

const DASHBOARD: &str = "/api/accounts/dashboard";
const RESPONSE_CAP: usize = 8 * 1024 * 1024;

pub(super) struct DashboardRequest {
    uri: PathAndQuery,
    principal: String,
    secure: bool,
    head: bool,
    conditions: HeaderMap,
}

impl DashboardRequest {
    pub(super) fn authorized(public: &Request) -> Option<Self> {
        if !matches!(public.uri().path(), DASHBOARD | "/api/accounts/dashboard/")
            || !matches!(*public.method(), Method::GET | Method::HEAD)
            || public.headers().contains_key(header::TRANSFER_ENCODING)
            || public
                .headers()
                .get(header::CONTENT_LENGTH)
                .is_some_and(|value| value != "0")
        {
            return None;
        }
        let access = public.extensions().get::<DashboardAccess>()?;
        let principal = match &access.principal {
            DashboardPrincipal::Browser { .. } => {
                let id = storage::sha(access.session_id.as_deref()?);
                json!({"kind":"session","id":id,"role":"admin"})
            }
            DashboardPrincipal::Device { id, .. } => {
                json!({"kind":"device","id":id,"role":"device"})
            }
            DashboardPrincipal::Anonymous => return None,
        };
        let mut conditions = HeaderMap::new();
        for key in [header::CACHE_CONTROL, header::IF_NONE_MATCH] {
            for value in public.headers().get_all(&key) {
                conditions.append(&key, value.clone());
            }
        }
        let uri = match public.uri().query() {
            Some(query) => format!("{DASHBOARD}?{query}").parse().ok()?,
            None => PathAndQuery::from_static(DASHBOARD),
        };
        Some(Self {
            uri,
            principal: principal.to_string(),
            secure: access.secure_transport,
            head: public.method() == Method::HEAD,
            conditions,
        })
    }
}

#[derive(Clone)]
pub(super) struct NativeReads {
    root: PathBuf,
    permits: Arc<Semaphore>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionEndpoint {
    ccs_dir: PathBuf,
    socket_path: PathBuf,
    secret: String,
    boot_id: String,
}

impl CollectionEndpoint {
    fn load(root: &Path) -> Option<Self> {
        let root = root.canonicalize().ok()?;
        let policy = ReadPolicy::capped(8192)
            .ancestors(Ancestors::NoLinkBelow(&root))
            .mode(Mode::Private)
            .owner(Owner::Mine);
        let Read::Bytes(bytes, _) = aac_files::read::read(&root.join("run/aac-http.json"), &policy)
        else {
            return None;
        };
        let endpoint: Self = serde_json::from_slice(&bytes).ok()?;
        let parent = endpoint.socket_path.parent()?;
        let parent_mode = std::fs::symlink_metadata(parent).ok()?;
        let metadata = std::fs::symlink_metadata(&endpoint.socket_path).ok()?;
        let uid = std::fs::metadata(&root).ok()?.uid();
        (endpoint.ccs_dir == root
            && endpoint.socket_path.is_absolute()
            && endpoint.socket_path.file_name()?.to_str()? == "http.sock"
            && parent.file_name()?.to_str()? == format!("aac-http-{}", endpoint.boot_id)
            && storage::is_hex(&endpoint.secret, 64)
            && storage::is_hex(&endpoint.boot_id, 24)
            && parent_mode.is_dir()
            && parent_mode.uid() == uid
            && parent_mode.mode() & 0o777 == 0o700
            && metadata.file_type().is_socket()
            && metadata.uid() == uid
            && metadata.mode() & 0o777 == 0o600)
            .then_some(endpoint)
    }
}

impl NativeReads {
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            root,
            permits: Arc::new(Semaphore::new(12)),
        }
    }

    pub(super) async fn settings(&self, signed_in: bool, uri: Uri, head: bool) -> Reply {
        let mut reply = if !signed_in {
            super::plain(401, "Authentication required")
        } else if uri.query().is_some_and(|query| {
            url::form_urlencoded::parse(query.as_bytes()).any(|(key, _)| !key.is_empty())
        }) {
            super::plain(400, "Unexpected settings query.")
        } else {
            let root = self.root.clone();
            let pref = tokio::task::spawn_blocking(move || {
                crate::unix::read_settings(&root)
                    .ok()
                    .map(|settings| json!(settings))
            })
            .await
            .ok()
            .flatten();
            super::response(
                200,
                pref.unwrap_or_else(|| json!({"refreshIntervalSeconds":60})),
            )
        };
        super::set_header(&mut reply, "cache-control", "no-store");
        if head {
            *reply.body_mut() = Body::empty();
        }
        reply
    }

    pub(super) async fn try_dashboard(&self, input: DashboardRequest) -> Option<Reply> {
        let _permit = self.permits.try_acquire().ok()?;
        let root = self.root.clone();
        let endpoint = tokio::task::spawn_blocking(move || CollectionEndpoint::load(&root))
            .await
            .ok()
            .flatten()?;
        let mut outgoing = Request::builder()
            .uri(Uri::from(input.uri))
            .header("host", "localhost")
            .header("x-aac-boot-secret", endpoint.secret)
            .header("x-aac-principal", input.principal)
            .header(
                crate::dashboard_fed::SECURE_HEADER,
                if input.secure { "1" } else { "0" },
            )
            .body(Body::empty())
            .ok()?;
        outgoing.headers_mut().extend(input.conditions);
        tokio::time::timeout(Duration::from_millis(450), async {
            let stream = UnixStream::connect(endpoint.socket_path).await.ok()?;
            let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.ok()?;
            let driver = tokio::spawn(connection);
            let incoming = sender.send_request(outgoing).await.ok()?;
            let (mut parts, body) = incoming.into_parts();
            if parts.headers.get("x-aac-service-boot")?.to_str().ok()? != endpoint.boot_id
                || !matches!(
                    parts.status,
                    StatusCode::OK | StatusCode::NOT_MODIFIED | StatusCode::BAD_REQUEST
                )
            {
                driver.abort();
                return None;
            }
            let bytes = to_bytes(Body::new(body), RESPONSE_CAP).await.ok()?;
            driver.abort();
            let incoming = std::mem::take(&mut parts.headers);
            for name in [
                header::CONTENT_TYPE,
                header::CONTENT_LENGTH,
                header::CACHE_CONTROL,
                header::ETAG,
            ] {
                if let Some(value) = incoming.get(&name) {
                    parts.headers.insert(name, value.clone());
                }
            }
            Some(Reply::from_parts(
                parts,
                if input.head {
                    Body::empty()
                } else {
                    Body::from(bytes)
                },
            ))
        })
        .await
        .ok()
        .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{Router, routing::get};
    use std::{
        io::Write,
        os::unix::fs::{OpenOptionsExt, PermissionsExt},
    };

    #[tokio::test]
    async fn native_accounts_proxies_only_the_verified_principal_and_public_conditionals() {
        let root = tempfile::tempdir_in("/tmp").unwrap();
        let run = root.path().join("run");
        let boot_id = "b".repeat(24);
        let dir = run.join(format!("aac-http-{boot_id}"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
        let socket = dir.join("http.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        std::fs::set_permissions(&socket, std::fs::Permissions::from_mode(0o600)).unwrap();
        let secret = "c".repeat(64);
        let descriptor = json!({
            "ccsDir":root.path().canonicalize().unwrap(),
            "socketPath":socket,
            "secret":secret,
            "bootId":boot_id
        });
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(run.join("aac-http.json"))
            .unwrap();
        file.write_all(descriptor.to_string().as_bytes()).unwrap();
        let app = Router::new().route(
            DASHBOARD,
            get(move |request: Request| async move {
                assert_eq!(request.uri().query(), Some("platform=mac"));
                assert_eq!(
                    request.headers()["x-aac-boot-secret"].to_str().unwrap(),
                    secret
                );
                assert_eq!(request.headers()["x-aac-dashboard-secure"], "1");
                assert!(!request.headers().contains_key("cookie"));
                assert_eq!(request.headers()[header::IF_NONE_MATCH], "W/\"fixture\"");
                assert_eq!(
                    serde_json::from_str::<serde_json::Value>(
                        request.headers()["x-aac-principal"].to_str().unwrap()
                    )
                    .unwrap(),
                    json!({"kind":"device","id":"dev_fixture","role":"device"})
                );
                let mut reply = super::super::response(200, json!({"accounts":[]}));
                super::super::set_header(&mut reply, "x-aac-service-boot", &boot_id);
                reply
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut public = Request::builder()
            .uri("/api/accounts/dashboard?platform=mac")
            .header("cookie", "unused-fixture-cookie")
            .header("x-aac-boot-secret", "ignored")
            .header(header::IF_NONE_MATCH, "W/\"fixture\"")
            .body(Body::empty())
            .unwrap();
        assert!(DashboardRequest::authorized(&public).is_none());
        public.extensions_mut().insert(DashboardAccess {
            auth_enabled: true,
            principal: DashboardPrincipal::Device {
                id: "dev_fixture".into(),
                platform: "mac".into(),
            },
            secure_transport: true,
            same_origin: true,
            origin_present: true,
            local_access: true,
            session_id: None,
            credential_transport: "plain",
            direct_loopback: true,
            lan_proxy: false,
            client_address: None,
        });
        let native = NativeReads::new(root.path().to_path_buf());
        let reply = native
            .try_dashboard(DashboardRequest::authorized(&public).unwrap())
            .await
            .unwrap();
        assert_eq!(reply.status(), StatusCode::OK);
        assert!(!reply.headers().contains_key("x-aac-service-boot"));
        assert_eq!(
            to_bytes(reply.into_body(), 128).await.unwrap().as_ref(),
            br#"{"accounts":[]}"#
        );
        server.abort();
    }
}
