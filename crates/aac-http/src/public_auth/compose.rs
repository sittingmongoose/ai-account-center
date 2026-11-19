//! The authenticated, loopback-only hop to the retained TypeScript route adapters.
use super::{DashboardAccess, DashboardPrincipal, Reply, error};
use axum::{
    body::Body,
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, Uri},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde_json::json;
use std::{io, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{net::TcpStream, sync::Semaphore};

const PROOF: &str = "x-aac-compose-proof";
const ACCESS: &str = "x-aac-compose-access";

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
            .filter(|value| super::storage::is_hex(value, 64))
            .and_then(|value| HeaderValue::from_str(&value).ok())
            .ok_or_else(invalid)?;
        Ok(Some(Self {
            address,
            proof,
            slots: Arc::new(Semaphore::new(32)),
        }))
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
        clean(request.headers_mut());
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
