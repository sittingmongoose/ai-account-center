//! HTTP adapter for the dashboard's app-update job owner.
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

/// HTTP-compatible job operations supplied by the app-update service at boot.
///
/// Each call returns an owned dashboard job snapshot. Start and cancel acknowledge
/// promptly. The job owner handles execution, durable state, and exclusive ownership.
pub trait AppUpdateJobs: Send + Sync {
    /// Snapshot the current or most recent job, if any.
    fn status(&self) -> Option<Value>;
    /// Start a new job or describe why the owner could not start one.
    fn start(&self) -> Result<Value, AppUpdateStartError>;
    /// Request cancellation of the current job and report its state.
    fn cancel(&self) -> AppUpdateCancel;
}

/// Failure from the guarded start operation.
pub enum AppUpdateStartError {
    /// A running job already owns start. Include the current job if known.
    Busy(Option<Value>),
    /// The owner could not safely start a job.
    Failed,
}

/// Acknowledgement from the guarded cancel operation.
pub enum AppUpdateCancel {
    /// No running job. Include the most recent job if known.
    Idle(Option<Value>),
    /// The owner is cancelling this job.
    Cancelling(Value),
    /// Another process owns cancellation.
    NotOwner(Option<Value>),
}

#[derive(Clone, Copy)]
pub(crate) enum Operation {
    Status,
    Start,
    Cancel,
}

impl Operation {
    /// The only method/path pairs allowed in the private route table.
    pub(crate) fn from_route(method: &axum::http::Method, path: &str) -> Option<Self> {
        use axum::http::Method;
        match (method, path) {
            (&Method::GET, "/api/app-updates/status") => Some(Self::Status),
            (&Method::POST, "/api/app-updates/start") => Some(Self::Start),
            (&Method::POST, "/api/app-updates/cancel") => Some(Self::Cancel),
            _ => None,
        }
    }

    pub(crate) fn invoke(self, jobs: &dyn AppUpdateJobs) -> Response {
        let (status, body) = match self {
            Self::Status => (StatusCode::OK, json!({ "job": jobs.status() })),
            Self::Start => match jobs.start() {
                Ok(job) => (StatusCode::ACCEPTED, json!({ "job": job })),
                Err(AppUpdateStartError::Busy(job)) => (
                    StatusCode::CONFLICT,
                    json!({ "error": "An app update is already running.", "job": job }),
                ),
                Err(AppUpdateStartError::Failed) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": "App updates could not start safely." }),
                ),
            },
            Self::Cancel => match jobs.cancel() {
                AppUpdateCancel::Idle(job) => {
                    (StatusCode::OK, json!({ "job": job, "cancelling": false }))
                }
                AppUpdateCancel::Cancelling(job) => (
                    StatusCode::ACCEPTED,
                    json!({ "job": job, "cancelling": true }),
                ),
                AppUpdateCancel::NotOwner(job) => (
                    StatusCode::CONFLICT,
                    json!({
                        "error": "This update is run by another dashboard process, so it cannot be cancelled here.",
                        "code": "not_owner", "job": job, "cancelling": false,
                    }),
                ),
            },
        };
        (status, Json(body)).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;

    struct Fixture {
        job: Value,
        outcome: String,
    }

    impl AppUpdateJobs for Fixture {
        fn status(&self) -> Option<Value> {
            (self.outcome != "idle").then(|| self.job.clone())
        }

        fn start(&self) -> Result<Value, AppUpdateStartError> {
            match self.outcome.as_str() {
                "failed" => Err(AppUpdateStartError::Failed),
                "busy" => Err(AppUpdateStartError::Busy(self.status())),
                _ => Ok(self.job.clone()),
            }
        }

        fn cancel(&self) -> AppUpdateCancel {
            match self.outcome.as_str() {
                "not_owner" => AppUpdateCancel::NotOwner(self.status()),
                "cancelling" => {
                    let mut job = self.job.clone();
                    job["cancelRequested"] = json!(true);
                    AppUpdateCancel::Cancelling(job)
                }
                _ => AppUpdateCancel::Idle(None),
            }
        }
    }

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
        while !socket.exists() && !service.is_finished() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
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
            let reply = op.invoke(&jobs);
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
