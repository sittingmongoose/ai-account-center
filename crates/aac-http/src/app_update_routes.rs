//! HTTP adapter for the dashboard's app-update job owner.
use aac_views::decline::Decline;
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::Semaphore;

/// The dashboard HTTP status and JSON body returned by a job operation.
pub struct AppUpdateReply {
    /// HTTP status, including the acknowledgement and conflict statuses.
    pub status: u16,
    /// The existing app-update wire body, with its field order.
    pub body: Value,
}

/// A single request to the async job owner.
pub type AppUpdateCall<'a> = Pin<Box<dyn Future<Output = AppUpdateReply> + Send + 'a>>;

/// Async operations supplied by the app-update job owner at boot.
///
/// The job owner manages running jobs, durable state, and exclusive ownership.
/// The route adapter owns request admission and passes the reply through.
pub trait AppUpdateJobs: Send + Sync {
    /// Snapshot the current or most recent job, if any.
    fn status(&self) -> AppUpdateCall<'_>;
    /// Acknowledge a new job or report why the owner could not start one.
    fn start(&self) -> AppUpdateCall<'_>;
    /// Acknowledge a job for the last job's failed rows, or report why it could not start.
    fn retry(&self) -> AppUpdateCall<'_>;
    /// Acknowledge cancellation or report why the owner could not cancel.
    fn cancel(&self) -> AppUpdateCall<'_>;
}

pub(crate) struct LaneHJobs(pub(crate) Arc<crate::app_updates::AppUpdates>);

impl AppUpdateJobs for LaneHJobs {
    fn status(&self) -> AppUpdateCall<'_> {
        Box::pin(async { to_route_reply(self.0.status().await) })
    }

    fn start(&self) -> AppUpdateCall<'_> {
        Box::pin(async { to_route_reply(self.0.start().await) })
    }

    fn retry(&self) -> AppUpdateCall<'_> {
        Box::pin(async { to_route_reply(self.0.retry().await) })
    }

    fn cancel(&self) -> AppUpdateCall<'_> {
        Box::pin(async { to_route_reply(self.0.cancel().await) })
    }
}

fn to_route_reply(reply: crate::app_updates::AppUpdateReply) -> AppUpdateReply {
    AppUpdateReply {
        status: reply.status,
        body: reply.body,
    }
}

pub(crate) struct BoundJobs {
    owner: Arc<dyn AppUpdateJobs>,
    slots: Arc<Semaphore>,
}

impl BoundJobs {
    pub(crate) fn new(owner: Arc<dyn AppUpdateJobs>) -> Self {
        Self {
            owner,
            slots: Arc::new(Semaphore::new(4)),
        }
    }

    pub(crate) async fn dispatch(&self, operation: Operation) -> Result<Response, Decline> {
        into_response(self.call(operation).await?).ok_or(Decline::Internal)
    }

    pub(crate) async fn call(&self, operation: Operation) -> Result<AppUpdateReply, Decline> {
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Decline::Busy)?;
        let owner = self.owner.clone();
        // Once admitted, a socket timeout can hide the outcome but cannot interrupt
        // the job owner's acknowledgment or release this request's permit early.
        tokio::spawn(async move {
            let _slot = slot;
            operation.call(owner.as_ref()).await
        })
        .await
        .map_err(|_| Decline::Internal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Operation {
    Status,
    Start,
    Retry,
    Cancel,
}

impl Operation {
    /// The only method/path pairs allowed in the private route table.
    pub(crate) fn from_route(method: &axum::http::Method, path: &str) -> Option<Self> {
        use axum::http::Method;
        match (method, path) {
            (&Method::GET, "/api/app-updates/status") => Some(Self::Status),
            (&Method::POST, "/api/app-updates/start") => Some(Self::Start),
            (&Method::POST, "/api/app-updates/retry") => Some(Self::Retry),
            (&Method::POST, "/api/app-updates/cancel") => Some(Self::Cancel),
            _ => None,
        }
    }

    async fn call(self, jobs: &dyn AppUpdateJobs) -> AppUpdateReply {
        match self {
            Self::Status => jobs.status().await,
            Self::Start => jobs.start().await,
            Self::Retry => jobs.retry().await,
            Self::Cancel => jobs.cancel().await,
        }
    }
}

fn into_response(reply: AppUpdateReply) -> Option<Response> {
    let status = StatusCode::from_u16(reply.status).ok()?;
    Some((status, Json(reply.body)).into_response())
}

/// The same inert app service used when M0 captures the TypeScript routes.
#[cfg(feature = "parity-fixtures")]
pub(crate) struct M0Jobs;

#[cfg(feature = "parity-fixtures")]
impl AppUpdateJobs for M0Jobs {
    fn status(&self) -> AppUpdateCall<'_> {
        Box::pin(async {
            AppUpdateReply {
                status: 200,
                body: serde_json::json!({"job":null}),
            }
        })
    }

    fn start(&self) -> AppUpdateCall<'_> {
        Box::pin(async {
            AppUpdateReply {
                status: 202,
                body: serde_json::json!({
                    "job":{"id":"fixture-update","state":"running","hosts":{},"results":[]}
                }),
            }
        })
    }

    fn retry(&self) -> AppUpdateCall<'_> {
        self.start()
    }

    fn cancel(&self) -> AppUpdateCall<'_> {
        self.status()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use serde_json::json;

    struct Fixture {
        job: Value,
        outcome: String,
    }

    fn reply(status: u16, body: Value) -> AppUpdateReply {
        AppUpdateReply { status, body }
    }

    impl AppUpdateJobs for Fixture {
        fn status(&self) -> AppUpdateCall<'_> {
            Box::pin(async {
                let job = (self.outcome != "idle").then_some(&self.job);
                reply(200, json!({ "job": job }))
            })
        }

        fn start(&self) -> AppUpdateCall<'_> {
            Box::pin(async {
                match self.outcome.as_str() {
                    "failed" => reply(
                        500,
                        json!({ "error": "App updates could not start safely." }),
                    ),
                    "busy" => reply(
                        409,
                        json!({ "error": "An app update is already running.", "job": self.job }),
                    ),
                    _ => reply(202, json!({ "job": self.job })),
                }
            })
        }

        fn retry(&self) -> AppUpdateCall<'_> {
            Box::pin(async {
                match self.outcome.as_str() {
                    "failed" => reply(
                        500,
                        json!({ "error": "App updates could not start safely." }),
                    ),
                    "busy" => reply(
                        409,
                        json!({ "error": "An app update is already running.", "job": self.job }),
                    ),
                    "empty" => reply(
                        409,
                        json!({ "error": "There is nothing to retry.", "job": null }),
                    ),
                    "nothing" => reply(
                        409,
                        json!({ "error": "There is nothing to retry.", "job": self.job }),
                    ),
                    _ => reply(202, json!({ "job": self.job })),
                }
            })
        }

        fn cancel(&self) -> AppUpdateCall<'_> {
            Box::pin(async {
                match self.outcome.as_str() {
                    "not_owner" => reply(
                        409,
                        json!({
                            "error": "This update is run by another dashboard process, so it cannot be cancelled here.",
                            "code": "not_owner", "job": self.job, "cancelling": false,
                        }),
                    ),
                    "cancelling" => {
                        let mut job = self.job.clone();
                        job["cancelRequested"] = json!(true);
                        reply(202, json!({ "job": job, "cancelling": true }))
                    }
                    _ => reply(200, json!({ "job": null, "cancelling": false })),
                }
            })
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_bound_job_owner_is_reached_only_after_socket_admission()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::{fs, os::unix::fs::PermissionsExt, sync::Arc, time::Duration};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let root = tempfile::tempdir()?;
        let ccs = root.path().canonicalize()?;
        let boot_id = "d".repeat(24);
        let secret = "e".repeat(64);
        let run = ccs.join("run");
        let boot_dir = run.join(format!("aac-http-{boot_id}"));
        fs::create_dir_all(&boot_dir)?;
        for directory in [&run, &boot_dir] {
            fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
        }
        let socket = boot_dir.join("http.sock");
        let boot = crate::Boot::from_json(&serde_json::to_vec(&json!({
            "ccsDir": ccs, "socketPath": socket, "secret": secret, "bootId": boot_id,
        }))?)?;
        let (stop, shutdown) = tokio::sync::oneshot::channel();
        let jobs = Arc::new(Fixture {
            job: json!({"id": "fixture-job"}),
            outcome: "running".into(),
        });
        let service = tokio::spawn(crate::serve_with_app_update_jobs(
            boot,
            async {
                let _ = shutdown.await;
            },
            jobs,
        ));
        tokio::time::timeout(Duration::from_secs(2), async {
            while !socket.exists() && !service.is_finished() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await?;
        for (key, principal, body, expected) in [
            ("wrong", "session", "{}", "401"),
            (secret.as_str(), "device", "{}", "401"),
            (secret.as_str(), "session", r#"{"host":"foreign"}"#, "400"),
            (secret.as_str(), "session", "{}", "202"),
        ] {
            let mut client = tokio::net::UnixStream::connect(&socket).await?;
            let role = if principal == "session" {
                "admin"
            } else {
                "device"
            };
            let envelope = json!({"kind": principal, "id": "fixture", "role": role});
            let request = format!(
                "POST /api/app-updates/start HTTP/1.1\r\nHost: private\r\nConnection: close\r\nx-aac-boot-secret: {key}\r\nx-aac-principal: {envelope}\r\nContent-Length: {}\r\n\r\n{body}",
                body.len(),
            );
            client.write_all(request.as_bytes()).await?;
            let mut response = String::new();
            tokio::time::timeout(Duration::from_secs(2), client.read_to_string(&mut response))
                .await??;
            assert!(
                response.starts_with(&format!("HTTP/1.1 {expected}")),
                "{response}"
            );
            if expected == "202" {
                assert!(
                    response.ends_with(r#"{"job":{"id":"fixture-job"}}"#),
                    "{response}"
                );
            }
        }
        let _ = stop.send(());
        service.await??;
        Ok(())
    }

    #[tokio::test]
    async fn http_outcomes_match_the_typescript_contract() -> Result<(), Box<dyn std::error::Error>>
    {
        let fixture: Value =
            serde_json::from_str(include_str!("../../../tests/spec/app-update-http.json"))?;
        for case in fixture["cases"].as_array().ok_or("missing cases")? {
            let word = case["op"].as_str().ok_or("missing operation")?;
            let method = if word == "status" {
                axum::http::Method::GET
            } else {
                axum::http::Method::POST
            };
            let op = Operation::from_route(&method, &format!("/api/app-updates/{word}"))
                .ok_or("unknown operation")?;
            let jobs = Fixture {
                job: fixture["job"].clone(),
                outcome: case["outcome"]
                    .as_str()
                    .ok_or("missing outcome")?
                    .to_owned(),
            };
            let reply = into_response(op.call(&jobs).await).ok_or("invalid status from fixture")?;
            assert_eq!(
                u64::from(reply.status().as_u16()),
                case["http"].as_u64().ok_or("missing status")?
            );
            let expected = serde_json::to_vec(&case["body"])?;
            assert_eq!(to_bytes(reply.into_body(), 4096).await?.as_ref(), expected);
        }
        Ok(())
    }
}
