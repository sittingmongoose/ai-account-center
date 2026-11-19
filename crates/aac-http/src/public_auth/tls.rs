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

fn configured(listener: &Value, ip: IpAddr) -> Option<(SocketAddr, Vec<u8>, Vec<u8>)> {
    let port: u16 = listener
        .get("port")
        .filter(|value| !value.is_null())
        .map_or(Some(3443), |value| u16::try_from(value.as_u64()?).ok())?;
    if port == 0 {
        return None;
    }
    // Certificate renewal commonly updates a stable symlink to a versioned public file.
    let cert_path = tls_path(listener["cert_path"].as_str()?)?
        .canonicalize()
        .ok()?;
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
    let doc = auth.0.runtime.lock().ok()?.files.config().ok()?;
    let settings = &doc["dashboard_tls"]["https_listener"];
    if settings["enabled"] != true {
        return None;
    }
    let Some((address, cert, key)) = configured(settings, ip) else {
        tracing::warn!("auth.tls.listener_off");
        return None;
    };
    // Select ring explicitly so another workspace consumer may enable a different provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    let Ok(config) = RustlsConfig::from_pem(cert, key).await else {
        tracing::warn!("auth.tls.listener_off");
        return None;
    };
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

#[cfg(all(test, unix))]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn reads_linked_public_certificate_but_refuses_linked_or_shared_key() {
        let home = tempfile::tempdir().unwrap();
        let cert = home.path().join("cert-1.pem");
        let stable_cert = home.path().join("cert.pem");
        let key = home.path().join("key.pem");
        std::fs::write(&cert, b"fixture certificate").unwrap();
        std::fs::write(&key, b"fixture private key").unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();
        symlink("cert-1.pem", &stable_cert).unwrap();
        let mut settings = json!({"enabled":true,"cert_path":stable_cert,"key_path":key});
        let ip = IpAddr::from([127, 0, 0, 1]);
        let (_, cert_bytes, key_bytes) = configured(&settings, ip).unwrap();
        assert_eq!(cert_bytes, b"fixture certificate");
        assert_eq!(key_bytes, b"fixture private key");

        let linked_key = home.path().join("linked-key.pem");
        symlink("key.pem", &linked_key).unwrap();
        settings["key_path"] = json!(linked_key);
        assert!(configured(&settings, ip).is_none());
        settings["key_path"] = json!(key);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(configured(&settings, ip).is_none());
    }
}
