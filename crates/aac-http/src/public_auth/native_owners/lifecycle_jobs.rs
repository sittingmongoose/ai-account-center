//! Public sign-in job polling and controls for the worker-free boot.
use super::{NativeOwners, lifecycle_login::Flow};
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
    installing: bool,
    stop_requested: bool,
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
        let previous = jobs.get(&id).and_then(Option::as_ref);
        let code_sent = previous.is_some_and(|record| record.code_sent);
        let installing = finish.is_none() && previous.is_some_and(|record| record.installing);
        let stop_requested = previous.is_some_and(|record| record.stop_requested);
        if previous.is_some_and(|record| record.finished_at.is_some()) && finish.is_none() {
            return;
        }
        jobs.insert(
            id,
            Some(Record {
                job,
                code_sent,
                installing,
                stop_requested,
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
        record.stop_requested = true;
        if record.installing {
            return answer(record.job.clone(), secure);
        }
        let agents = native.agents.clone();
        tokio::spawn(async move {
            if !control(&agents, &id, json!({"cancel":true}), "cancelled").await {
                let _ = journal(&agents, json!({"action":"cancel","jobId":id})).await;
            }
        });
        record.job.state = "cancelled".into();
        record.job.verification = None;
        record.finished_at = Some(now());
        if let Some(at) = storage::iso(now()) {
            record.job.updated_at = at;
        }
        native.events.publish(DashboardEvent::SigninJob {
            job: Box::new(record.job.clone()),
        });
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

impl Jobs {
    fn begin_install(&self, id: &str) -> bool {
        let Ok(mut jobs) = self.0.lock() else {
            return false;
        };
        let Some(record) = jobs.get_mut(id).and_then(Option::as_mut) else {
            return false;
        };
        if stopped(record) {
            return false;
        }
        record.installing = true;
        true
    }

    pub(super) fn stopped(&self, id: &str) -> bool {
        self.0
            .lock()
            .ok()
            .is_none_or(|jobs| jobs.get(id).and_then(Option::as_ref).is_none_or(stopped))
    }

    pub(super) fn start(self: Arc<Self>, native: NativeOwners, flow: Flow) -> Reply {
        let Ok(random) = storage::random::<8>() else {
            return start_error();
        };
        let id = format!("job_{}", storage::hex(&random));
        let Some(job) = flow.initial(id.clone()) else {
            return start_error();
        };
        {
            let Ok(mut jobs) = self.0.lock() else {
                return start_error();
            };
            let running = jobs
                .values()
                .filter_map(Option::as_ref)
                .filter(|job| job.finished_at.is_none());
            if let Some(record) = running
                .clone()
                .find(|record| record.job.provider == job.provider)
            {
                return super::no_store(response(
                    409,
                    json!({
                        "error":"A sign-in for this provider is already running.","code":"job_running",
                        "jobId":record.job.id
                    }),
                ));
            }
            if running.count() >= 3 {
                return error(
                    409,
                    "too_many_jobs",
                    "Too many sign-ins are running. Finish one first.",
                );
            }
            while jobs.len() >= ISSUED_LIMIT {
                let Some(index) = jobs
                    .values()
                    .position(|row| row.as_ref().is_none_or(|row| row.finished_at.is_some()))
                else {
                    return start_error();
                };
                jobs.shift_remove_index(index);
            }
            jobs.insert(
                id,
                Some(Record {
                    job: job.clone(),
                    code_sent: false,
                    installing: false,
                    stop_requested: false,
                    finished_at: None,
                }),
            );
        }
        let initial = job.clone();
        native.events.publish(DashboardEvent::SigninJob {
            job: Box::new(job.clone()),
        });
        tokio::spawn(async move {
            self.run_flow(&native, flow, job).await;
        });
        super::no_store(response(202, json!({"job":initial})))
    }

    fn publish(&self, native: &NativeOwners, job: DashboardSignInJob) {
        self.observe(job.clone());
        native
            .events
            .publish(DashboardEvent::SigninJob { job: Box::new(job) });
    }

    async fn run_flow(
        self: Arc<Self>,
        native: &NativeOwners,
        mut flow: Flow,
        initial: DashboardSignInJob,
    ) {
        let id = initial.id.clone();
        let root = native.agents.root().to_owned();
        let command = flow.prepare(&native.agents, &id).await;
        let (mut job, install) = match command {
            Ok(command) if !self.stopped(&id) => {
                self.dispatch(native, &flow, initial, command).await
            }
            Ok(_) => (failure(initial, ErrorCode::Timeout), false),
            Err(err) => (failure(initial, err), false),
        };
        if install && self.begin_install(&id) {
            self.publish(native, job.clone());
            job = match flow.install(native, self.clone(), &id).await {
                Ok(result) => {
                    job.result = serde_json::from_value(result).ok();
                    job.state = "succeeded".into();
                    job.verification = None;
                    job
                }
                Err(err) => failure(job, err),
            };
        }
        let cancelled = self
            .0
            .lock()
            .ok()
            .and_then(|jobs| {
                jobs.get(&id)
                    .and_then(Option::as_ref)
                    .map(|record| record.stop_requested)
            })
            .unwrap_or(false);
        if job.state != "succeeded" && cancelled {
            job.state = "cancelled".into();
            job.error = None;
            job.verification = None;
        } else if install && job.state == "verifying" {
            job = failure(job, ErrorCode::Timeout);
        }
        if let Some(at) = storage::iso(now()) {
            job.updated_at = at;
        }
        if install {
            let recovery = job
                .result
                .as_ref()
                .map(|result| json!({"accountId":result.account_id,"email":null,"plan":null}));
            let _ = journal(
                &native.agents,
                json!({
                    "action":"settle","jobId":id,"state":job.state,"result":recovery,
                    "error":job.error.as_ref().map(|err|err.code.as_str())
                }),
            )
            .await;
        }
        flow.cleanup(root, id).await;
        let succeeded = job.state == "succeeded";
        self.publish(native, job);
        if succeeded {
            native.changed();
        }
    }

    async fn dispatch(
        &self,
        native: &NativeOwners,
        flow: &Flow,
        initial: DashboardSignInJob,
        command: Value,
    ) -> (DashboardSignInJob, bool) {
        use futures_util::StreamExt;
        let request = json!({
            "computer":{"platform":"ubuntu","sshHost":null},
            "args":{"jobId":initial.id,"frontDoorPid":std::process::id(),"provider":initial.provider,
                "kind":initial.kind,"mode":initial.mode,"accountId":initial.account_id,
                "profileName":initial.profile_name,"platform":initial.platform,
                "allowedOrigins":flow.origins(),"timeoutMs":900000,"command":command},
            "deadlineMs":960000,"timeoutMs":965000,"maxBytes":1048576,"callId":initial.id
        });
        let reply = native
            .agents
            .call("lifecycle-signin", request.to_string().as_bytes())
            .await;
        let mut stream = reply.into_body().into_data_stream();
        let mut count = 0;
        while let Some(Ok(frame)) = stream.next().await {
            count += frame.len();
            if count > 1_048_576 {
                break;
            }
            let Ok(frame) = serde_json::from_slice::<Value>(&frame) else {
                break;
            };
            if frame["protocol"] != 1 || frame["id"] != initial.id {
                break;
            }
            match frame["type"].as_str() {
                Some("progress") => {
                    if self.stopped(&initial.id) {
                        continue;
                    }
                    if let Some(job) = incoming(&frame["data"], &initial)
                        && matches!(job.state.as_str(), "waiting" | "awaiting_code")
                    {
                        if self
                            .live(&job.id)
                            .flatten()
                            .is_some_and(|current| current.state == "verifying")
                        {
                            continue;
                        }
                        self.publish(native, job);
                    }
                }
                Some("result") => {
                    if let Some(terminal) = terminal(&frame["value"], &initial) {
                        return terminal;
                    }
                    break;
                }
                _ => break,
            }
        }
        (failure(initial, ErrorCode::WriteFailed), false)
    }
}

fn start_error() -> Reply {
    error(500, "write_failed", "The change could not be saved safely.")
}

fn incoming(frame: &Value, expected: &DashboardSignInJob) -> Option<DashboardSignInJob> {
    let mut job: DashboardSignInJob = serde_json::from_value(frame["job"].clone()).ok()?;
    if job.id != expected.id
        || job.provider != expected.provider
        || job.kind != expected.kind
        || job.mode != expected.mode
        || job.account_id != expected.account_id
        || job.profile_name != expected.profile_name
        || job.platform != expected.platform
    {
        return None;
    }
    // The launch reply timestamp remains the route's authoritative start and expiry.
    job.started_at = expected.started_at.clone();
    job.expires_at = expected.expires_at.clone();
    if let Some(verification) = &mut job.verification {
        verification.expires_at = Some(expected.expires_at.clone());
    }
    Some(job)
}

fn stopped(record: &Record) -> bool {
    record.stop_requested
        || record.finished_at.is_some()
        || storage::timestamp(&record.job.expires_at).is_none_or(|end| end <= now())
}

fn terminal(frame: &Value, expected: &DashboardSignInJob) -> Option<(DashboardSignInJob, bool)> {
    let install = match frame.get("installPending") {
        Some(value) => value.as_bool()?,
        None => false,
    };
    let job = incoming(frame, expected)?;
    let admitted = if install {
        job.state == "verifying"
    } else {
        finished(&job.state)
    };
    admitted.then_some((job, install))
}
fn failure(mut job: DashboardSignInJob, code: ErrorCode) -> DashboardSignInJob {
    job.state = if code == ErrorCode::Timeout {
        "expired"
    } else {
        "failed"
    }
    .into();
    job.verification = None;
    job.result = None;
    job.error = Some(crate::public_auth::SignInError {
        code: code.word().into(),
        message: code.message().into(),
    });
    job
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn lifecycle_signin_uses_the_agent_job_envelope_and_rejects_a_different_target() {
        let initial = Flow::Codex {
            name: "saved".into(),
            add: false,
            cli: "fixture-codex".into(),
        }
        .initial("job_0123456789abcdef".into())
        .unwrap();
        let mut progress = initial.clone();
        progress.state = "waiting".into();
        assert_eq!(
            incoming(&json!({"job":progress}), &initial).unwrap().state,
            "waiting"
        );
        assert!(terminal(&json!({"job":progress,"installPending":true}), &initial).is_none());
        let mut done = progress;
        done.state = "verifying".into();
        let (verified, installs) =
            terminal(&json!({"job":done,"installPending":true}), &initial).unwrap();
        assert!(installs);
        assert_eq!(verified.account_id, initial.account_id);
        done.account_id = Some("codex:other".into());
        assert!(terminal(&json!({"job":done,"installPending":true}), &initial).is_none());
    }
}
