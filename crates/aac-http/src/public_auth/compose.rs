//! The authenticated, loopback-only hop to the retained TypeScript route adapters.
use super::{DashboardAccess, DashboardPrincipal, Reply, error, request_log::RequestId};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, Method, Uri},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde_json::json;
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpStream, sync::Semaphore};

const PROOF: &str = "x-aac-compose-proof";
const ACCESS: &str = "x-aac-compose-access";

// Only the cut uses this list. The composition fixture remains a reference for
// the complete legacy router, including its handling of unknown methods.
const CUT_ROUTES: &[(&str, &[&str])] = &[
    ("/api/accounts/dashboard", &["GET"]),
    ("/api/accounts/settings", &["GET", "PUT"]),
    ("/api/accounts/preferences", &["GET", "PUT", "PATCH"]),
    ("/api/accounts/visibility", &["GET", "PUT", "PATCH"]),
    ("/api/accounts/analytics", &["GET"]),
    ("/api/accounts/analytics/capture", &["GET", "PUT"]),
    ("/api/accounts/registry", &["GET"]),
    ("/api/accounts/add", &["POST"]),
    ("/api/accounts/signin-command", &["GET"]),
    ("/api/accounts/signin-jobs/:id", &["GET"]),
    ("/api/accounts/signin-jobs/:id/cancel", &["POST"]),
    ("/api/accounts/signin-jobs/:id/code", &["POST"]),
    ("/api/accounts/trash", &["GET"]),
    ("/api/accounts/trash/:id/purge", &["POST"]),
    ("/api/accounts/trash/:id/restore", &["POST"]),
    ("/api/accounts/:id", &["PATCH"]),
    ("/api/accounts/:id/key", &["PUT"]),
    ("/api/accounts/:id/open", &["POST"]),
    ("/api/accounts/:id/recheck", &["POST"]),
    ("/api/accounts/:id/remove", &["POST"]),
    ("/api/accounts/:id/signin-again", &["POST"]),
    ("/api/accounts/:id/reset-claims", &["GET", "POST"]),
    ("/api/accounts/:id/reset-claims/check", &["POST"]),
    ("/api/accounts/:id/reset-claims/prepare", &["POST"]),
    ("/api/antigravity/profiles", &["GET"]),
    ("/api/antigravity/profiles/quotas", &["GET"]),
    ("/api/app-updates/status", &["GET"]),
    ("/api/app-updates/start", &["POST"]),
    ("/api/app-updates/cancel", &["POST"]),
    ("/api/claude/desktop-profiles", &["GET"]),
    ("/api/claude/desktop-profiles/usage", &["GET"]),
    ("/api/claude/desktop-profiles/:id/open", &["POST"]),
    ("/api/claude/history-snapshots/cleanup", &["POST"]),
    ("/api/codex/profiles", &["GET"]),
    ("/api/codex/profiles/quotas", &["GET"]),
    ("/api/diag", &["GET"]),
    ("/api/scoreboard", &["GET"]),
    ("/api/status", &["GET"]),
    ("/api/status/answers", &["GET", "POST"]),
    (
        "/api/status/deliverable",
        &["GET", "POST", "PUT", "PATCH", "DELETE"],
    ),
    ("/api/status/files/:owner/*relative", &["GET"]),
    ("/api/store/status", &["GET"]),
    ("/api/usage/history", &["GET"]),
    ("/api/usage/at", &["GET"]),
    ("/api/usage/burn", &["GET"]),
    ("/api/views/home", &["GET"]),
];

pub(super) fn cut_route(method: &Method, path: &str) -> bool {
    let method = if method == Method::HEAD {
        Method::GET.as_str()
    } else {
        method.as_str()
    };
    let path = path.strip_suffix('/').unwrap_or(path);
    CUT_ROUTES.iter().any(|(pattern, methods)| {
        if method != Method::OPTIONS.as_str() && !methods.contains(&method) {
            return false;
        }
        let mut segments = path.split('/');
        for part in pattern.split('/') {
            let Some(segment) = segments.next() else {
                return false;
            };
            if part.starts_with('*') {
                return !segment.is_empty() || segments.next().is_some_and(|next| !next.is_empty());
            }
            if (part.starts_with(':') && segment.is_empty())
                || (!part.starts_with(':') && part != segment)
            {
                return false;
            }
        }
        segments.next().is_none()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_forwards_only_the_adapter_contract() {
        assert!(cut_route(&Method::HEAD, "/api/accounts/dashboard"));
        assert!(cut_route(&Method::PATCH, "/api/accounts/preferences"));
        assert!(cut_route(
            &Method::POST,
            "/api/accounts/example/reset-claims/prepare"
        ));
        assert!(cut_route(
            &Method::GET,
            "/api/status/files/example/nested/file.md"
        ));
        assert!(!cut_route(&Method::GET, "/api/status/files/example"));
        assert!(!cut_route(&Method::POST, "/api/accounts/dashboard"));
        assert!(!cut_route(&Method::POST, "/api/accounts//recheck"));
        assert!(!cut_route(&Method::GET, "/api/accounts/unknown"));
        assert!(!cut_route(&Method::GET, "/api/auth/session"));
        assert!(!cut_route(&Method::GET, "/internal/collection/recheck"));
    }
}

#[derive(Clone)]
pub(super) struct Backend {
    address: SocketAddr,
    proof: HeaderValue,
    slots: Arc<Semaphore>,
}

impl Backend {
    pub(super) fn from_env(public: SocketAddr) -> io::Result<Option<Self>> {
        let upstream = std::env::var_os("AAC_RUST_COMPOSE_UPSTREAM");
        let secret = std::env::var_os("AAC_RUST_COMPOSE_PROOF");
        if upstream.is_none() && secret.is_none() {
            return Ok(None);
        }
        let invalid = || io::Error::other("invalid composition backend");
        let address: SocketAddr = upstream
            .and_then(|value| value.into_string().ok())
            .and_then(|value| value.parse().ok())
            .filter(|address: &SocketAddr| {
                address.ip().is_loopback() && address.port() != 0 && *address != public
            })
            .ok_or_else(invalid)?;
        let proof = secret
            .and_then(|value| value.into_string().ok())
            .ok_or_else(invalid)?;
        Self::new(address, &proof).map(Some)
    }

    pub(super) fn new(address: SocketAddr, proof: &str) -> io::Result<Self> {
        if !address.ip().is_loopback() || address.port() == 0 || !super::storage::is_hex(proof, 64)
        {
            return Err(io::Error::other("invalid composition backend"));
        }
        let proof = HeaderValue::from_str(proof)
            .map_err(|_| io::Error::other("invalid composition backend"))?;
        Ok(Self {
            address,
            proof,
            slots: Arc::new(Semaphore::new(32)),
        })
    }

    pub(super) async fn forward(self, mut request: Request) -> Reply {
        let Some(access) = request.extensions().get::<DashboardAccess>() else {
            return unavailable();
        };
        let principal = match &access.principal {
            DashboardPrincipal::Anonymous => json!({"kind":"anonymous"}),
            DashboardPrincipal::Browser { username } => {
                json!({"kind":"session","username":username,"id":access.session_id})
            }
            DashboardPrincipal::Device { id, platform } => {
                json!({"kind":"device","id":id,"platform":platform})
            }
        };
        let context = json!({
            "principal": principal,
            "secure": access.secure_transport,
            "transport": access.credential_transport,
            "local": access.local_access,
            "directLoopback": access.direct_loopback,
            "lanProxy": access.lan_proxy,
            "client": access.client_address,
        });
        let Ok(context) = HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(context.to_string()))
        else {
            return unavailable();
        };
        let Some(uri) = request
            .uri()
            .path_and_query()
            .and_then(|path| path.as_str().parse::<Uri>().ok())
        else {
            return unavailable();
        };
        *request.uri_mut() = uri;
        let request_id = request.extensions().get::<RequestId>().cloned();
        clean(request.headers_mut());
        request.headers_mut().remove("x-ccs-request-id");
        if let Some(RequestId(value)) = request_id {
            request.headers_mut().insert("x-ccs-request-id", value);
        }
        request.headers_mut().remove("cookie");
        request.headers_mut().remove("authorization");
        request.headers_mut().insert(PROOF, self.proof);
        request.headers_mut().insert(ACCESS, context);
        let Ok(permit) = self.slots.try_acquire_owned() else {
            return unavailable();
        };
        let reply = tokio::time::timeout(Duration::from_secs(45), async {
            let stream = TcpStream::connect(self.address).await.ok()?;
            let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.ok()?;
            let task = tokio::spawn(async move {
                let _permit = permit;
                let _ = connection.await;
            });
            let response = sender.send_request(request).await.ok()?;
            // No reuse across requests. The HTTP driver drops the idle connection when the
            // sender is dropped; a streamed response keeps it alive until its body ends.
            drop(task);
            Some(response)
        })
        .await
        .ok()
        .flatten();
        let Some(response) = reply else {
            return unavailable();
        };
        let (mut parts, body) = response.into_parts();
        clean(&mut parts.headers);
        // Rust is the sole public session writer.
        parts.headers.remove("set-cookie");
        Reply::from_parts(parts, Body::new(body))
    }
}

fn clean(headers: &mut HeaderMap) {
    let named: Vec<HeaderName> = headers
        .get_all("connection")
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .filter_map(|name| HeaderName::from_bytes(name.trim().as_bytes()).ok())
        .collect();
    for name in named {
        headers.remove(name);
    }
    for name in [
        "connection",
        "keep-alive",
        "proxy-authenticate",
        "proxy-authorization",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
        PROOF,
        ACCESS,
    ] {
        headers.remove(name);
    }
}

fn unavailable() -> Reply {
    error(
        503,
        "fallback_unavailable",
        "The dashboard route backend is unavailable.",
    )
}
