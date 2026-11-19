//! The app-update job routes on the public listener: status, start and cancel.
//!
//! Admission is the retired TypeScript route's, in order: a browser session, the dashboard
//! origin and JSON content type for the two writes, an empty query and empty JSON object, and
//! `no-store` on every answer. The job owner (Lane H) runs beside the front door on Unix and
//! Windows; the M0 fixture owner replaces it only in a `parity-fixtures` build.

use super::{DashboardAccess, DashboardEvents, DashboardPrincipal, Reply};
use crate::app_update_routes::{BoundJobs, LaneHJobs, Operation};
use crate::app_updates::AppUpdates;
use axum::{extract::Request, http::Method};
use serde_json::{Map, Value};
use std::{path::PathBuf, sync::Arc};

/// One boot's job owner.
#[derive(Clone)]
pub(super) struct Owner {
    jobs: Arc<BoundJobs>,
    #[cfg(unix)]
    host: Option<Arc<super::owner_host::OwnerHost>>,
}

impl Owner {
    /// The job owner for this boot. `events` publishes job progress to the dashboard.
    pub(super) fn start(
        root: PathBuf,
        events: DashboardEvents,
        agents: Arc<crate::agent_transport::Agents>,
    ) -> Self {
        #[cfg(feature = "parity-fixtures")]
        if std::env::var("AAC_RUST_CUT_APP_UPDATES_FIXTURE").as_deref() == Ok("1") {
            return Self {
                jobs: Arc::new(BoundJobs::new(Arc::new(crate::app_update_routes::M0Jobs))),
            };
        }
        let owner = AppUpdates::with_events(
            root,
            agents,
            Some(Box::new(move |event| {
                let _ = events.publish(event);
            })),
        );
        Self {
            jobs: Arc::new(BoundJobs::new(Arc::new(LaneHJobs(owner)))),
            #[cfg(unix)]
            host: None,
        }
    }

    /// The owner for one boot that answers through the shared host.
    #[cfg(unix)]
    pub(super) fn start_with_host(
        root: PathBuf,
        events: DashboardEvents,
        agents: Arc<crate::agent_transport::Agents>,
        host: Arc<super::owner_host::OwnerHost>,
    ) -> Self {
        Self {
            host: Some(host),
            ..Self::start(root, events, agents)
        }
    }

    /// The three exact routes, including the trailing-slash spelling the TypeScript router
    /// accepts and HEAD over GET for the status read.
    pub(super) fn owns(method: &Method, path: &str) -> bool {
        operation(method, path).is_some()
    }

    /// Serve one owned request. The caller already checked [`Self::owns`].
    pub(super) async fn handle(&self, request: Request) -> Reply {
        let path = request.uri().path().trim_end_matches('/');
        let Some(operation) = operation(request.method(), path) else {
            return super::plain(404, "API endpoint was not found.");
        };
        let Some(access) = request.extensions().get::<DashboardAccess>().cloned() else {
            return super::service_error();
        };
        no_store(self.answer(&access, operation, request).await)
    }

    async fn answer(
        &self,
        access: &DashboardAccess,
        operation: Operation,
        request: Request,
    ) -> Reply {
        if !matches!(access.principal, DashboardPrincipal::Browser { .. }) {
            return super::plain(401, "Authentication required");
        }
        let requested = request.uri().clone();
        if !matches!(operation, Operation::Status) {
            if !access.origin_present || !access.same_origin {
                return super::plain(403, "App updates require the dashboard origin.");
            }
            if !json_content(&request) {
                return super::plain(415, "App updates require application/json.");
            }
            if request.uri().query().is_some_and(|q| !q.is_empty())
                || !object_body(request, 100 * 1024)
                    .await
                    .is_some_and(|body| body.is_empty())
            {
                return super::plain(400, "Provide an empty JSON object.");
            }
        }
        #[cfg(unix)]
        {
            let path = requested.path().to_owned();
            let method = match operation {
                Operation::Status => "GET",
                Operation::Start | Operation::Cancel => "POST",
            };
            let body = matches!(operation, Operation::Status).then_some(&b"{}"[..]);
            if let Some(host) = self.host.as_ref()
                && let Some(reply) = host
                    .request(
                        method,
                        &path,
                        &access.host_principal(),
                        &[("content-type", "application/json")],
                        body,
                    )
                    .await
                && (reply.status != 503 || reply.headers.get("x-aac-decline").is_none())
            {
                let Ok(value) = serde_json::from_slice::<Value>(&reply.body) else {
                    return super::plain(502, "The service reply could not be read.");
                };
                return super::response(reply.status, value);
            }
        }
        match self.jobs.call(operation).await {
            Ok(reply) => super::response(reply.status, reply.body),
            Err(_) => match operation {
                Operation::Status => {
                    super::plain(503, "App update status could not be read safely.")
                }
                Operation::Start => super::plain(500, "App updates could not start safely."),
                Operation::Cancel => {
                    super::plain(503, "App update cancellation could not be confirmed.")
                }
            },
        }
    }
}

fn operation(method: &Method, path: &str) -> Option<Operation> {
    let path = path.trim_end_matches('/');
    let method = if *method == Method::HEAD {
        &Method::GET
    } else {
        method
    };
    Operation::from_route(method, path)
}

fn json_content(request: &Request) -> bool {
    super::header(request.headers(), "content-type")
        .and_then(|value| value.split(';').next())
        .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
}

async fn object_body(request: Request, limit: usize) -> Option<Map<String, Value>> {
    use axum::http::header::{CONTENT_LENGTH, TRANSFER_ENCODING};
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|size| size.to_str().ok());
    if declared
        .and_then(|size| size.parse::<usize>().ok())
        .is_some_and(|size| size > limit)
    {
        return None;
    }
    let has_body = declared.is_some() || request.headers().contains_key(TRANSFER_ENCODING);
    let bytes = axum::body::to_bytes(request.into_body(), limit)
        .await
        .ok()?;
    if bytes.is_empty() {
        return has_body.then(Map::new);
    }
    serde_json::from_slice(&bytes).ok()
}

fn no_store(mut reply: Reply) -> Reply {
    super::set_header(&mut reply, "cache-control", "no-store");
    reply
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use axum::http::Method;

    #[test]
    fn the_three_routes_match_head_and_trailing_slashes() {
        assert_eq!(
            operation(&Method::GET, "/api/app-updates/status"),
            Some(Operation::Status)
        );
        assert_eq!(
            operation(&Method::HEAD, "/api/app-updates/status/"),
            Some(Operation::Status)
        );
        assert_eq!(
            operation(&Method::POST, "/api/app-updates/start"),
            Some(Operation::Start)
        );
        assert_eq!(
            operation(&Method::POST, "/api/app-updates/cancel"),
            Some(Operation::Cancel)
        );
        assert!(operation(&Method::GET, "/api/app-updates/start").is_none());
        assert!(operation(&Method::PUT, "/api/app-updates/status").is_none());
    }
}
