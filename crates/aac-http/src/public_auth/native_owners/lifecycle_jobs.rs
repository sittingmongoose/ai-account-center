//! Public sign-in job polling and controls for the worker-free boot.
use super::NativeOwners;
use crate::{
    agent_transport::{Agents, Selector},
    public_auth::{
        DashboardAccess, DashboardEvent, DashboardSignInJob, Method, Reply, Request, error,
        lifecycle, now, response, storage,
    },
};
use aac_core::signin::{ErrorCode, restarted_placeholder, valid_code, valid_job_id};
use indexmap::IndexMap;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

const RETENTION_MS: i64 = 600_000;
const ISSUED_LIMIT: usize = 512;

#[derive(Clone, Copy)]
pub(super) enum Action {
    Get,
    Cancel,
    Code,
}

pub(super) fn route<'a>(method: &Method, path: &'a str) -> Option<(Action, &'a str)> {
    let tail = path.strip_prefix("/api/accounts/signin-jobs/")?;
    let (id, suffix) = tail.split_once('/').unwrap_or((tail, ""));
    if id.is_empty() {
        return None;
    }
    match (method, suffix) {
        (&Method::GET | &Method::HEAD | &Method::OPTIONS, "") => Some((Action::Get, id)),
        (&Method::POST | &Method::OPTIONS, "cancel") => Some((Action::Cancel, id)),
        (&Method::POST | &Method::OPTIONS, "code") => Some((Action::Code, id)),
        _ => None,
    }
}

struct Record {
    job: DashboardSignInJob,
    code_sent: bool,
    finished_at: Option<i64>,
}

#[derive(Default)]
pub(super) struct Jobs(Mutex<IndexMap<String, Option<Record>>>);

impl Jobs {
    pub(super) fn running(&self, secure: bool) -> Vec<DashboardSignInJob> {
        let Ok(jobs) = self.0.lock() else {
            return Vec::new();
        };
        jobs.values()
            .filter_map(|record| {
                let record = record.as_ref()?;
                if record.finished_at.is_some() {
                    return None;
                }
                let mut job = record.job.clone();
                if !secure {
                    job.redact();
                }
                Some(job)
            })
            .collect()
    }

    pub(super) fn observe(&self, job: DashboardSignInJob) {
        if !valid_job_id(&job.id) {
            return;
        }
        let Ok(mut jobs) = self.0.lock() else { return };
        let finish =
            finished(&job.state).then(|| storage::timestamp(&job.updated_at).unwrap_or_else(now));
        let id = job.id.clone();
        let code_sent = jobs
            .get(&id)
            .and_then(Option::as_ref)
            .is_some_and(|record| record.code_sent);
        jobs.insert(
            id,
            Some(Record {
                job,
                code_sent,
                finished_at: finish,
            }),
        );
        while jobs.len() > ISSUED_LIMIT {
            jobs.shift_remove_index(0);
        }
    }

    fn live(&self, id: &str) -> Option<Option<DashboardSignInJob>> {
        let mut jobs = self.0.lock().ok()?;
        let record = jobs.get_mut(id)?;
        if record
            .as_ref()
            .and_then(|r| r.finished_at)
            .is_some_and(|time| now() - time >= RETENTION_MS)
        {
            *record = None;
        }
        Some(record.as_ref().map(|r| r.job.clone()))
    }

    pub(super) async fn handle(
        self: Arc<Self>,
        native: &NativeOwners,
        action: Action,
        raw_id: &str,
        request: Request,
    ) -> Reply {
        if request.method() == Method::OPTIONS {
            return match action {
                Action::Get => lifecycle::options("GET, HEAD", "9"),
                _ => lifecycle::options("POST", "4"),
            };
        }
        let secure = request
            .extensions()
            .get::<DashboardAccess>()
            .is_some_and(|access| access.secure_transport);
        let body = if matches!(action, Action::Get) {
            if let Err(reply) = lifecycle::admit(&request) {
                return reply;
            }
            if request.uri().query().is_some() {
                return lifecycle::unexpected_query();
            }
            None
        } else {
            match lifecycle::mutation_body(request).await {
                Ok(body) => Some(body),
                Err(reply) => return reply,
            }
        };
        let Some(id) = crate::public_auth::decode_component(raw_id).filter(|id| valid_job_id(id))
        else {
            return unknown();
        };
        match action {
            Action::Get => match self.live(&id) {
                Some(Some(job)) => answer(job, secure),
                Some(None) => unknown(),
                None => {
                    let reply = journal(&native.agents, json!({"action":"get","jobId":id})).await;
                    if let Some(job) = reply.as_ref().and_then(|reply| reply.get("job")) {
                        if job.is_null() {
                            return unknown();
                        }
                        if let Some(job) = recovered_job(job, &id) {
                            return answer(job, secure);
                        }
                    }
                    super::no_store(response(
                        200,
                        restarted_placeholder(&id, storage::iso(now())),
                    ))
                }
            },
            Action::Cancel => {
                if body.is_none_or(|body| !body.is_empty()) {
                    return lifecycle::invalid_action_body();
                }
                self.cancel(native, id, secure)
            }
            Action::Code => {
                if !secure {
                    return error(
                        403,
                        "secure_transport_required",
                        "Use the secure dashboard address (HTTPS or a tunnel) for keys and sign-in codes.",
                    );
                }
                let code = body
                    .filter(|body| body.len() == 1)
                    .and_then(|body| body.get("code").and_then(Value::as_str).map(str::to_owned))
                    .filter(|code| valid_code(code));
                let Some(code) = code else {
                    return lifecycle::invalid_action_body();
                };
                self.code(native, id, code)
            }
        }
    }

    fn cancel(&self, native: &NativeOwners, id: String, secure: bool) -> Reply {
        let Ok(mut jobs) = self.0.lock() else {
            return unknown();
        };
        let Some(record) = jobs.get_mut(&id).and_then(Option::as_mut) else {
            return unknown();
        };
        if finished(&record.job.state) {
            return answer(record.job.clone(), secure);
        }
        let agents = native.agents.clone();
        tokio::spawn(async move {
            if !control(&agents, &id, json!({"cancel":true}), "cancelled").await {
                let reply = journal(&agents, json!({"action":"cancel","jobId":id})).await;
                if reply.is_some_and(|reply| reply["job"]["state"] == "verifying") {
                    let _ = journal(&agents,json!({
                        "action":"settle","jobId":id,"state":"cancelled","result":null,"error":null
                    })).await;
                }
            }
        });
        if record.job.state != "verifying" {
            record.job.state = "cancelled".into();
            record.job.verification = None;
            record.finished_at = Some(now());
            if let Some(at) = storage::iso(now()) {
                record.job.updated_at = at;
            }
            native.events.publish(DashboardEvent::SigninJob {
                job: Box::new(record.job.clone()),
            });
        }
        answer(record.job.clone(), secure)
    }

    fn code(self: Arc<Self>, native: &NativeOwners, id: String, code: String) -> Reply {
        let job = {
            let Ok(mut jobs) = self.0.lock() else {
                return unknown();
            };
            let Some(record) = jobs.get_mut(&id).and_then(Option::as_mut) else {
                return unknown();
            };
            if record.job.kind != "supervised-cli"
                || record.job.state != "awaiting_code"
                || record.code_sent
            {
                return error(
                    409,
                    "code_not_expected",
                    "This sign-in is not waiting for a code.",
                );
            }
            record.code_sent = true;
            record.job.state = "verifying".into();
            if let Some(at) = storage::iso(now()) {
                record.job.updated_at = at;
            }
            record.job.clone()
        };
        native.events.publish(DashboardEvent::SigninJob {
            job: Box::new(job.clone()),
        });
        let agents = native.agents.clone();
        let events = native.events.clone();
        tokio::spawn(async move {
            if control(&agents, &id, json!({"input":code}), "accepted").await {
                return;
            }
            let Ok(mut jobs) = self.0.lock() else { return };
            let Some(record) = jobs.get_mut(&id).and_then(Option::as_mut) else {
                return;
            };
            if record.job.state != "verifying" {
                return;
            }
            record.job.state = "failed".into();
            record.job.verification = None;
            record.finished_at = Some(now());
            if let Some(at) = storage::iso(now()) {
                record.job.updated_at = at;
            }
            let code = ErrorCode::WriteFailed;
            record.job.error = Some(crate::public_auth::SignInError {
                code: code.word().into(),
                message: code.message().into(),
            });
            events.publish(DashboardEvent::SigninJob {
                job: Box::new(record.job.clone()),
            });
        });
        answer(job, true)
    }
}

fn finished(state: &str) -> bool {
    matches!(state, "succeeded" | "failed" | "expired" | "cancelled")
}

fn unknown() -> Reply {
    error(
        404,
        "unknown_job",
        "That sign-in has ended and is no longer listed.",
    )
}

fn answer(mut job: DashboardSignInJob, secure: bool) -> Reply {
    if !secure {
        job.redact();
    }
    super::no_store(response(200, json!(job)))
}

async fn journal(agents: &Agents, mut args: Value) -> Option<Value> {
    args["ccs"] = json!(agents.root());
    agents
        .call_value(
            "signin-session",
            Selector::usage("ubuntu", None),
            args,
            5000,
            7000,
            32768,
        )
        .await
}

async fn control(agents: &Agents, id: &str, input: Value, field: &str) -> bool {
    let reply = agents.control(id, input.to_string().as_bytes()).await;
    let Ok(bytes) = axum::body::to_bytes(reply.into_body(), 4096).await else {
        return false;
    };
    serde_json::from_slice::<Value>(&bytes).is_ok_and(|reply| reply[field] == true)
}

fn recovered_job(value: &Value, id: &str) -> Option<DashboardSignInJob> {
    let mut job: DashboardSignInJob = serde_json::from_value(value.clone()).ok()?;
    if job.id != id
        || !matches!(job.provider.as_str(), "codex" | "muse" | "antigravity")
        || !matches!(job.kind.as_str(), "device-code" | "supervised-cli")
        || !matches!(job.mode.as_str(), "add" | "signin-again")
        || !matches!(job.platform.as_str(), "ubuntu" | "mac")
        || !(finished(&job.state)
            || matches!(
                job.state.as_str(),
                "starting" | "waiting" | "awaiting_code" | "verifying"
            ))
        || [&job.started_at, &job.updated_at, &job.expires_at]
            .iter()
            .any(|s| storage::timestamp(s).is_none())
    {
        return None;
    }
    job.redact();
    if let Some(error) = &mut job.error {
        let code = ErrorCode::from_word(&error.code)?;
        error.message = code.message().into();
    }
    Some(job)
}
