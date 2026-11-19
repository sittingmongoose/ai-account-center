//! Carry admitted dashboard requests to the private lifecycle and collection owner.
use super::{Reply, Request, lifecycle, native_owners::NativeOwners, owner_host::OwnerHost};
use axum::{Router, routing::any};
use std::sync::{Arc, Weak};

pub(crate) const ENDPOINT: &str = "/internal/accounts/lifecycle";
pub(crate) const COLLECTION_ENDPOINT: &str = "/internal/accounts/collection";
#[cfg(unix)]
pub(crate) use unix::{PACKET_CAP, receive};

#[derive(Clone)]
pub(super) struct Routes {
    host: Weak<OwnerHost>,
    private: bool,
}

impl Routes {
    pub(super) fn new(host: &Arc<OwnerHost>, private: bool) -> Self {
        Self {
            host: Arc::downgrade(host),
            private,
        }
    }

    pub(super) fn router(&self) -> Router {
        let mut router = Router::new();
        for path in [
            "/api/accounts/registry",
            "/api/accounts/signin-command",
            "/api/accounts/signin-jobs/{id}",
            "/api/accounts/signin-jobs/{id}/cancel",
            "/api/accounts/signin-jobs/{id}/code",
            "/api/accounts/trash",
            "/api/accounts/trash/{id}/purge",
            "/api/accounts/trash/{id}/restore",
            "/api/accounts/add",
            "/api/accounts/{id}/open",
            "/api/accounts/{id}/remove",
            "/api/accounts/{id}/signin-again",
            "/api/accounts/{id}/key",
        ] {
            let routes = self.clone();
            router = router.route(
                path,
                any(move |request: Request| async move { routes.handle(request).await }),
            );
        }
        router
    }

    pub(super) fn owns(&self, request: &Request) -> bool {
        #[cfg(unix)]
        if self.private && NativeOwners::owns_collection(request.method(), request.uri().path()) {
            return true;
        }
        NativeOwners::owns_lifecycle(request.method(), request.uri().path())
    }

    pub(super) async fn handle(&self, request: Request) -> Reply {
        if !self.owns(&request) {
            return super::plain(404, "API endpoint was not found.");
        }
        let Some(host) = self.host.upgrade() else {
            return super::service_error();
        };
        if self.private {
            #[cfg(unix)]
            return unix::public(&host, request).await;
            #[cfg(not(unix))]
            return super::service_error();
        }
        if let Some(route) = lifecycle::Route::of(request.method(), request.uri().path()) {
            return lifecycle::handle(host.root(), route, request).await;
        }
        let Some(owner) = host.public_owners() else {
            return super::service_error();
        };
        owner.handle(request).await
    }
}

#[cfg(unix)]
mod unix {
    use super::super::{
        DashboardAccess, DashboardPrincipal, Method, Reply, Request, lifecycle,
        native_owners::{NativeOwners, no_store, recheck},
    };
    use super::{COLLECTION_ENDPOINT, ENDPOINT};
    use aac_api::{PrincipalKind, PrincipalRole, VerifiedPrincipal};
    use axum::{
        body::{Body, to_bytes},
        http::{HeaderMap, HeaderName, HeaderValue, Uri, header},
        response::IntoResponse,
    };
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde::{Deserialize, Serialize};
    use serde_json::json;

    const INPUT_CAP: usize = 100 * 1024;
    pub(crate) const PACKET_CAP: usize = (INPUT_CAP / 3 + 1) * 4 + 16 * 1024;

    #[derive(Deserialize, Serialize)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    struct Packet {
        method: String,
        target: String,
        content_type: Option<String>,
        content_length: Option<String>,
        transfer_encoding: bool,
        body: String,
        secure: bool,
        local: bool,
        origin_present: bool,
        same_origin: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        if_none_match: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        cache_control: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        device_platform: Option<String>,
    }

    pub(in crate::public_auth) async fn public(
        host: &super::super::owner_host::OwnerHost,
        request: Request,
    ) -> Reply {
        if request.method() == Method::OPTIONS {
            return options(request.uri().path());
        }
        let collection = NativeOwners::owns_collection(request.method(), request.uri().path());
        let dashboard = collection
            && NativeOwners::dashboard_route(
                request.method(),
                request.uri().path().trim_end_matches('/'),
            );
        let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
            return super::super::service_error();
        };
        if collection
            && !dashboard
            && let Err(reply) = recheck::admission(&access, &request)
        {
            return no_store(reply);
        }
        if !collection && let Err(reply) = lifecycle::admit(&request) {
            return reply;
        }
        if !collection
            && !matches!(*request.method(), Method::GET | Method::HEAD)
            && let Err(reply) = lifecycle::mutation_admission(&request)
        {
            return reply;
        }
        let (principal, device_platform) = match &access.principal {
            DashboardPrincipal::Browser { .. } => {
                let Some(session) = &access.session_id else {
                    return super::super::service_error();
                };
                (json!({"kind":"session","id":session,"role":"admin"}), None)
            }
            DashboardPrincipal::Device { id, platform } if dashboard => (
                json!({"kind":"device","id":id,"role":"device"}),
                Some(platform.clone()),
            ),
            _ => return no_store(super::super::plain(401, "Authentication required")),
        };
        let (parts, body) = request.into_parts();
        let bytes = if dashboard {
            axum::body::Bytes::new()
        } else {
            let Ok(bytes) = to_bytes(body, INPUT_CAP).await else {
                return super::super::invalid_body();
            };
            bytes
        };
        let (if_none_match, cache_control) = if dashboard {
            (
                header_values(&parts.headers, header::IF_NONE_MATCH),
                header_values(&parts.headers, header::CACHE_CONTROL),
            )
        } else {
            Default::default()
        };
        let packet = Packet {
            method: parts.method.to_string(),
            target: parts
                .uri
                .path_and_query()
                .map_or("/", |target| target.as_str())
                .to_owned(),
            content_type: parts
                .headers
                .get(header::CONTENT_TYPE)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            content_length: parts
                .headers
                .get(header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            transfer_encoding: parts.headers.contains_key(header::TRANSFER_ENCODING),
            body: STANDARD.encode(bytes),
            secure: access.secure_transport,
            local: access.local_access,
            origin_present: access.origin_present,
            same_origin: access.same_origin,
            if_none_match,
            cache_control,
            device_platform,
        };
        let Ok(bytes) = serde_json::to_vec(&packet) else {
            return super::super::service_error();
        };
        let endpoint = if collection {
            COLLECTION_ENDPOINT
        } else {
            ENDPOINT
        };
        let principal = principal.to_string();
        let Some(answer) = host
            .request(
                "POST",
                endpoint,
                &principal,
                &[("content-type", "application/json")],
                Some(&bytes),
            )
            .await
        else {
            return super::super::decline();
        };
        if answer.status == 503 && answer.headers.contains_key("x-aac-decline") {
            return super::super::decline();
        }
        let mut reply = (
            axum::http::StatusCode::from_u16(answer.status)
                .unwrap_or(axum::http::StatusCode::INTERNAL_SERVER_ERROR),
            answer.body,
        )
            .into_response();
        reply.headers_mut().clear();
        for name in [
            header::ALLOW,
            header::CACHE_CONTROL,
            header::CONTENT_LENGTH,
            header::CONTENT_TYPE,
            header::ETAG,
            header::RETRY_AFTER,
            header::VARY,
            header::X_CONTENT_TYPE_OPTIONS,
        ] {
            if let Some(value) = answer.headers.get(&name) {
                reply.headers_mut().insert(name, value.clone());
            }
        }
        reply
    }

    fn header_values(headers: &HeaderMap, name: HeaderName) -> Vec<String> {
        headers
            .get_all(name)
            .iter()
            .filter_map(|value| value.to_str().ok().map(str::to_owned))
            .collect()
    }

    fn options(path: &str) -> Reply {
        let path = path.trim_end_matches('/');
        if NativeOwners::owns_lifecycle(&Method::GET, path) {
            return lifecycle::options("GET, HEAD", "9");
        }
        if NativeOwners::owns_lifecycle(&Method::PUT, path) {
            return lifecycle::options("PUT", "3");
        }
        if NativeOwners::owns_lifecycle(&Method::PATCH, path) {
            return lifecycle::options("PATCH", "5");
        }
        lifecycle::options("POST", "4")
    }

    pub(crate) async fn receive(
        native: &NativeOwners,
        principal: &VerifiedPrincipal,
        bytes: &[u8],
        collection: bool,
    ) -> Reply {
        let browser = principal.dashboard_session() && principal.kind == PrincipalKind::Session;
        let device = collection && principal.role == PrincipalRole::Device;
        if !principal.valid() || !(browser || device) {
            return super::super::error(401, "auth_required", "Authentication required");
        }
        let Ok(packet) = serde_json::from_slice::<Packet>(bytes) else {
            return super::super::invalid_body();
        };
        let Some(request) = decode(packet, principal, collection) else {
            return super::super::invalid_body();
        };
        if collection {
            return native.handle(request).await;
        }
        let root = native.root();
        if let Some(route) = lifecycle::Route::of(request.method(), request.uri().path()) {
            return lifecycle::handle(root, route, request).await;
        }
        native.handle(request).await
    }

    fn decode(packet: Packet, principal: &VerifiedPrincipal, collection: bool) -> Option<Request> {
        let method = Method::from_bytes(packet.method.as_bytes()).ok()?;
        let uri = packet.target.parse::<Uri>().ok()?;
        if uri.scheme().is_some() || uri.authority().is_some() {
            return None;
        }
        let owned = if collection {
            NativeOwners::owns_collection(&method, uri.path())
        } else {
            NativeOwners::owns_lifecycle(&method, uri.path())
        };
        if !owned {
            return None;
        }
        let (identity, session_id) = if principal.role == PrincipalRole::Device {
            if !NativeOwners::dashboard_route(&method, uri.path().trim_end_matches('/')) {
                return None;
            }
            let platform = packet
                .device_platform
                .filter(|value| matches!(value.as_str(), "mac" | "windows"))?;
            (
                DashboardPrincipal::Device {
                    id: principal.id.clone(),
                    platform,
                },
                None,
            )
        } else {
            (
                DashboardPrincipal::Browser {
                    username: String::new(),
                },
                Some(principal.id.clone()),
            )
        };
        let bytes = STANDARD.decode(packet.body).ok()?;
        if bytes.len() > INPUT_CAP {
            return None;
        }
        let mut builder = axum::http::Request::builder()
            .method(method)
            .uri(uri)
            .extension(DashboardAccess {
                auth_enabled: true,
                principal: identity,
                secure_transport: packet.secure,
                origin_present: packet.origin_present,
                same_origin: packet.same_origin,
                local_access: packet.local,
                session_id,
                credential_transport: if packet.secure { "https" } else { "insecure" },
                direct_loopback: packet.local,
                lan_proxy: false,
                client_address: None,
            });
        if let Some(content_len) = packet.content_length {
            builder = builder.header(header::CONTENT_LENGTH, content_len);
        }
        if packet.transfer_encoding {
            builder = builder.header(
                header::TRANSFER_ENCODING,
                HeaderValue::from_static("chunked"),
            );
        }
        if let Some(content_type) = packet.content_type {
            builder = builder.header(header::CONTENT_TYPE, content_type);
        }
        for value in packet.if_none_match {
            builder = builder.header(header::IF_NONE_MATCH, value);
        }
        for value in packet.cache_control {
            builder = builder.header(header::CACHE_CONTROL, value);
        }
        builder.body(Body::from(bytes)).ok()
    }
}
