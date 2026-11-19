//! The public front door's authenticated hop to the private service.
//!
//! The front door owns public admission and HTTP encoding; the private service owns the
//! readers, the producers, the store and the answer journal. A route the service answers is
//! asked over this one loopback connection with the boot secret and the verified principal,
//! exactly as the TypeScript adapter's transport did, so there is still one owner of every
//! collector and one writer of every file.
//!
//! The endpoint is the service's own `run/aac-http.json` (written at boot): its boot id, its
//! 32 random bytes, and a socket in a private folder. All of them are checked here; a missing
//! or moved endpoint is no answer (the caller decides the route's own 503).

use super::storage;
use crate::identity;
use aac_files::read::{Ancestors, Mode, Owner, Read, ReadPolicy};
use axum::body::{Body, to_bytes};
use axum::http::{HeaderMap, Request};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;
use serde::Deserialize;
use std::{
    io,
    os::unix::fs::{FileTypeExt, MetadataExt},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::net::UnixStream;

const ENDPOINT_HEADER: &str = "x-aac-service-boot";
/// The private transport's reply cap: the largest route answer (a media chunk or a view).
const REPLY_CAP: usize = 16 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_secs(10);

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CollectionEndpoint {
    ccs_dir: PathBuf,
    socket_path: PathBuf,
    secret: String,
    boot_id: String,
    #[serde(default)]
    native_lifecycle: bool,
}

/// One service reply, as it crossed the socket.
pub(super) struct ServiceReply {
    pub status: u16,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
}

#[derive(Clone)]
pub(super) struct ServiceLink {
    socket: PathBuf,
    secret: String,
    boot_id: String,
    native_lifecycle: bool,
}

impl ServiceLink {
    /// `run/aac-http.json` under the boot root, checked exactly as the store publisher does:
    /// the file is private, the socket is the named boot folder's own socket with the private
    /// owner and mode, and the tokens are the shapes the service writes.
    pub(super) fn load(root: &Path) -> Option<Self> {
        let root = root.canonicalize().ok()?;
        let policy = ReadPolicy::capped(8192)
            .ancestors(Ancestors::NoLinkBelow(&root))
            .mode(Mode::Private)
            .owner(Owner::Mine);
        let Read::Bytes(bytes, _) = aac_files::read::read(&root.join("run/aac-http.json"), &policy)
        else {
            return None;
        };
        let endpoint: CollectionEndpoint = serde_json::from_slice(&bytes).ok()?;
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
            .then_some(Self {
                socket: endpoint.socket_path,
                secret: endpoint.secret,
                boot_id: endpoint.boot_id,
                native_lifecycle: endpoint.native_lifecycle,
            })
    }

    /// Select a ready, compatible boot once. A live incompatible listener may still own
    /// collection, including a pre-lease build. Its supervisor must stop it before startup.
    pub(super) async fn acquire_lifecycle(root: &Path) -> io::Result<Option<Self>> {
        let Some(link) = Self::load(root).filter(|link| link.native_lifecycle) else {
            return Ok(None);
        };
        let principal = r#"{"kind":"session","id":"public-health","role":"public"}"#;
        if let Some(health) = link
            .request("GET", "/api/health", principal, &[], None)
            .await
        {
            let binary = std::env::current_exe()?.with_file_name("aac-http");
            let expected =
                tokio::task::spawn_blocking(move || identity::Identity::for_binary(&binary))
                    .await
                    .map_err(io::Error::other)??;
            if health.status == 200
                && health.headers.get(identity::HEADER) == Some(&expected.header())
                && health
                    .headers
                    .get(identity::LIFECYCLE_HEADER)
                    .is_some_and(|value| value == "1")
            {
                return Ok(Some(link));
            }
        }
        match tokio::time::timeout(DEADLINE, UnixStream::connect(&link.socket)).await {
            Ok(Err(error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
                ) =>
            {
                Ok(None)
            }
            _ => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "an incompatible private lifecycle service must stop before collection can start",
            )),
        }
    }

    /// One request the front door has already admitted. `principal` is the verified identity
    /// the service's table adjudicates; the service strips both headers before dispatch.
    pub(super) async fn request(
        &self,
        method: &str,
        uri: &str,
        principal: &str,
        headers: &[(&str, &str)],
        body: Option<&[u8]>,
    ) -> Option<ServiceReply> {
        let mut builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "localhost")
            .header("x-aac-boot-secret", &self.secret)
            .header("x-aac-principal", principal);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let body = body.unwrap_or_default().to_vec();
        let request = builder.body(Body::from(body)).ok()?;
        let deadline = if matches!(
            uri,
            super::lifecycle_host::ENDPOINT | super::lifecycle_host::COLLECTION_ENDPOINT
        ) {
            Duration::from_secs(150)
        } else {
            DEADLINE
        };
        tokio::time::timeout(deadline, async {
            let stream = UnixStream::connect(&self.socket).await.ok()?;
            let (mut sender, connection) = http1::handshake(TokioIo::new(stream)).await.ok()?;
            // The connection future drives the reply body; dropping it would stall the read.
            let drive = tokio::spawn(async move {
                let _ = connection.await;
            });
            let reply = match sender.send_request(request).await {
                Ok(reply) => reply,
                Err(_) => {
                    drive.abort();
                    return None;
                }
            };
            let boot = reply
                .headers()
                .get(ENDPOINT_HEADER)
                .and_then(|value| value.to_str().ok());
            if boot != Some(self.boot_id.as_str()) {
                drive.abort();
                return None;
            }
            let status = reply.status().as_u16();
            let headers = reply.headers().clone();
            let body = to_bytes(Body::new(reply.into_body()), REPLY_CAP).await.ok();
            drive.abort();
            let body = body?;
            Some(ServiceReply {
                status,
                headers,
                body: body.to_vec(),
            })
        })
        .await
        .ok()
        .flatten()
    }
}
