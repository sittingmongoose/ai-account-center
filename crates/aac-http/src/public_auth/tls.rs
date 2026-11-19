use super::{DashboardAuth, PublicPeer};
use aac_files::read::{Mode, Read, ReadPolicy, read};
use axum::{
    extract::{ConnectInfo, Request},
    middleware::Next,
    response::Response,
};
use axum_server::{
    Server,
    tls_rustls::{RustlsAcceptor, RustlsConfig},
};
use serde_json::Value;
use std::{
    net::{IpAddr, SocketAddr},
    path::PathBuf,
};

fn tls_path(raw: &str) -> Option<PathBuf> {
    // As in the TS TLS config, a home-relative configured path is intentional.
    if let Some(relative) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
        let home = std::env::home_dir()?;
        Some(home.join(relative))
    } else {
        (!raw.is_empty()).then(|| PathBuf::from(raw))
    }
}

fn configured(auth: &DashboardAuth, ip: IpAddr) -> Option<(SocketAddr, Vec<u8>, Vec<u8>)> {
    let doc: Value = auth.0.runtime.lock().ok()?.files.config().ok()?;
    let listener = &doc["dashboard_tls"]["https_listener"];
    if listener["enabled"] != true {
        return None;
    }
    let port: u16 = listener
        .get("port")
        .filter(|value| !value.is_null())
        .map_or(Some(3443), |value| u16::try_from(value.as_u64()?).ok())?;
    if port == 0 {
        return None;
    }
    let cert_path = tls_path(listener["cert_path"].as_str()?)?;
    let key_path = tls_path(listener["key_path"].as_str()?)?;
    let Read::Bytes(cert, _) = read(&cert_path, &ReadPolicy::capped(4 * 1024 * 1024)) else {
        return None;
    };
    let Read::Bytes(key, _) = read(&key_path, &ReadPolicy::capped(262_144).mode(Mode::Private))
    else {
        return None;
    };
    Some((SocketAddr::new(ip, port), cert, key))
}

pub(super) async fn listener(
    auth: &DashboardAuth,
    ip: IpAddr,
) -> Option<(SocketAddr, Server<SocketAddr, RustlsAcceptor>)> {
    let (address, cert, key) = configured(auth, ip)?;
    // Select ring explicitly so another workspace consumer may enable a different provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let config = RustlsConfig::from_pem(cert, key).await.ok()?;
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .ok()?
        .into_std()
        .ok()?;
    let server = axum_server::from_tcp_rustls(listener, config).ok()?;
    tracing::info!(port = address.port(), "auth.tls.listening");
    Some((address, server))
}

pub(super) async fn mark_encrypted(mut request: Request, next: Next) -> Response {
    if let Some(ConnectInfo(address)) = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .copied()
    {
        request
            .extensions_mut()
            .insert(PublicPeer { address, tls: true });
    }
    next.run(request).await
}
