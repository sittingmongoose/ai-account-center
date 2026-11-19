//! Browser admission and HTTP presentation for the existing reset ledger and sources.
use super::{
    DashboardAccess, DashboardEvents, DashboardPrincipal, Method, Next, Reply, Request, State,
    decode_component, empty, header, middleware, now, response, set_header, storage,
};
use crate::{reset_claims, reset_source_route};
use aac_files::registry;
use axum::Router;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};
use tokio::sync::Semaphore;

#[derive(Clone, Copy)]
enum Route {
    Inventory,
    Prepare,
    Claim,
    Check,
    Options(&'static str),
}

fn route<'a>(method: &Method, path: &'a str) -> Option<(Route, &'a str)> {
    let path = path.strip_suffix('/').unwrap_or(path);
    let mut parts = path.strip_prefix("/api/accounts/")?.split('/');
    let account = parts.next().filter(|part| !part.is_empty())?;
    if parts.next()? != "reset-claims" {
        return None;
    }
    let tail = parts.next();
    if parts.next().is_some() {
        return None;
    }
    let route = match (method, tail) {
        (&Method::GET | &Method::HEAD, None) => Route::Inventory,
        (&Method::POST, None) => Route::Claim,
        (&Method::POST, Some("prepare")) => Route::Prepare,
        (&Method::POST, Some("check")) => Route::Check,
        (&Method::OPTIONS, None) => Route::Options("GET, HEAD, POST"),
        (&Method::OPTIONS, Some("prepare" | "check")) => Route::Options("POST"),
        _ => return None,
    };
    Some((route, account))
}

pub(super) fn mount(
    router: Router,
    root: PathBuf,
    events: DashboardEvents,
    log: Arc<aac_log::Log>,
) -> Router {
    let owner = Arc::new(ResetRoutes {
        root,
        claims: reset_claims::ResetClaims::default(),
        events,
        log,
        slots: Arc::new(Semaphore::new(4)),
    });
    router.layer(middleware::from_fn_with_state(owner, handle))
}

struct ResetRoutes {
    root: PathBuf,
    claims: reset_claims::ResetClaims,
    events: DashboardEvents,
    log: Arc<aac_log::Log>,
    slots: Arc<Semaphore>,
}

fn code(status: u16, code: &str) -> Reply {
    response(status, json!({ "code": code }))
}

fn valid_keys(body: &Value, fields: &[&str]) -> bool {
    body.as_object().is_some_and(|body| {
        body.len() == fields.len()
            && fields.iter().all(|key| {
                body.get(*key).and_then(Value::as_str).is_some_and(|value| {
                    value.len() == 64
                        && value
                            .bytes()
                            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
                })
            })
    })
}

async fn handle(State(owner): State<Arc<ResetRoutes>>, request: Request, next: Next) -> Reply {
    let Some((route, account)) = route(request.method(), request.uri().path()) else {
        return next.run(request).await;
    };
    let mut reply = admitted(owner, route, account.to_owned(), request).await;
    set_header(&mut reply, "cache-control", "no-store");
    reply
}

async fn admitted(
    owner: Arc<ResetRoutes>,
    route: Route,
    account: String,
    request: Request,
) -> Reply {
    let Some(access) = request
        .extensions()
        .get::<DashboardAccess>()
        .filter(|access| matches!(access.principal, DashboardPrincipal::Browser { .. }))
    else {
        return code(403, "browser-session-required");
    };
    if request.uri().query().is_some() {
        return code(400, "unexpected-query");
    }
    let caller = access.session_id.as_ref().map_or_else(
        || "local".to_owned(),
        |id| format!("session:{}", storage::hex(&Sha256::digest(id))),
    );
    let origin = access.origin_present && access.same_origin;
    let (parts, body) = request.into_parts();
    let body = if parts.method == Method::GET {
        Value::Null
    } else {
        if !origin {
            return code(403, "origin-required");
        }
        if !header(&parts.headers, "content-type").is_some_and(|content| {
            content
                .split(';')
                .next()
                .is_some_and(|media| media.trim().eq_ignore_ascii_case("application/json"))
        }) {
            return code(415, "json-required");
        }
        let fields: &[&str] = if parts.uri.path().ends_with("/prepare") {
            &["grantKey"]
        } else if parts.uri.path().ends_with("/check") {
            &["attemptKey"]
        } else {
            &["grantKey", "confirmationToken"]
        };
        // The front door already applied the shared Express JSON limit before authentication.
        match super::parse_body(body, false).await {
            Ok((value, bytes)) if !bytes.is_empty() && valid_keys(&value, fields) => value,
            Ok(_) => return code(400, "invalid-body"),
            Err(reply) => return reply,
        }
    };
    let Some(account) = decode_component(&account) else {
        return super::plain(400, "The request could not be processed.");
    };
    if let Route::Options(allow) = route {
        let mut reply = axum::response::IntoResponse::into_response(allow);
        set_header(&mut reply, "allow", allow);
        set_header(&mut reply, "content-type", "text/plain; charset=utf-8");
        return reply;
    }
    let Ok(Ok(permit)) =
        tokio::time::timeout(Duration::from_secs(5), owner.slots.clone().acquire_owned()).await
    else {
        return unavailable(route);
    };
    let reply = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        owner.execute(route, &account, &caller, &body)
    })
    .await
    .unwrap_or_else(|_| unavailable(route));
    if matches!(parts.method, Method::GET | Method::HEAD)
        && reply.status().is_success()
        && let Some(etag) = header(reply.headers(), "etag")
        && super::static_ui::fresh(&parts.headers, etag, None)
    {
        let mut cached = empty(304);
        set_header(&mut cached, "etag", etag);
        return cached;
    }
    reply
}

type Result<T> = std::result::Result<T, &'static str>;

fn ledger(db: &Path, op: &str, account: &str, mut input: Value) -> Result<Value> {
    input["op"] = json!(op);
    input["account"] = json!(account);
    input["nowMs"] = json!(now());
    let entropy = storage::random::<32>().map_err(|_| "entropy")?;
    input["entropy"] = json!(storage::hex(&entropy));
    aac_status::reset::service_call(db, input.to_string().as_bytes()).map(reset_claims::sorted)
}

fn safe_reason(code: &str) -> Option<&str> {
    matches!(
        code,
        "check-again-required"
            | "claim-token-outstanding"
            | "confirmation-invalid"
            | "cooldown"
            | "grant-changed"
            | "identity-changed"
            | "identity-unconfirmed"
            | "rate-limited"
            | "stale-inventory"
            | "provider-read-only"
            | "unknown-grant-kind"
            | "exact-grant-unavailable"
            | "unsupported-grant-id"
            | "credential-host-unavailable"
            | "grant-unavailable"
            | "redeeming"
            | "refill-windows-unconfirmed"
            | "signed-out"
    )
    .then_some(code)
}

fn refusal(reason: Option<&str>, sent: bool, evidence: Value) -> Reply {
    response(
        409,
        json!({
            "code": "reset-state-unavailable",
            "action": "check-again",
            "reason": reason,
            "sent": sent,
            "evidence": evidence,
        }),
    )
}

fn unavailable(route: Route) -> Reply {
    if matches!(route, Route::Inventory) {
        response(
            503,
            json!({"status":"unavailable","code":"reset-inventory-unavailable"}),
        )
    } else {
        refusal(None, false, Value::Null)
    }
}

impl ResetRoutes {
    fn path(&self) -> Result<PathBuf> {
        let rows = registry::rows().map_err(|_| "storage")?;
        let owners = reset_claims::public_owners(rows).map_err(|_| "storage")?;
        self.claims.path(&self.root, &owners).map_err(|_| "storage")
    }

    fn execute(&self, route: Route, account: &str, caller: &str, body: &Value) -> Reply {
        let result = self.path();
        let source =
            reset_source_route::SourceContext::from_env(&self.root, super::sidecar::package_root())
                .with_log(self.log.clone());
        if matches!(route, Route::Inventory) {
            let inventory =
                result.and_then(|db| match reset_source_route::read(&source, &db, account) {
                    Ok(snapshot) => ledger(&db, "inventory", account, json!({"snapshot":snapshot})),
                    Err(error) => {
                        let mut recovery = ledger(&db, "pending", account, json!({}))?;
                        let reason = recovery["reason"]
                            .as_str()
                            .or_else(|| safe_reason(error))
                            .unwrap_or("reset-inventory-unavailable")
                            .to_owned();
                        recovery["status"] = json!("unavailable");
                        recovery["reason"] = json!(reason);
                        recovery["grants"] = json!([]);
                        Ok(recovery)
                    }
                });
            return inventory.map_or_else(|_| unavailable(route), |value| response(200, value));
        }
        let mut sent = false;
        let reply = result
            .as_ref()
            .map_err(|error| *error)
            .and_then(|db| self.write(route, db, &source, account, caller, body, &mut sent));
        let reply = match reply {
            Ok(value) => response(200, value),
            Err(error) => {
                let evidence = result
                    .ok()
                    .and_then(|db| ledger(&db, "pending", account, json!({})).ok())
                    .map_or(Value::Null, |mut recovery| recovery["evidence"].take());
                refusal(safe_reason(error), sent, evidence)
            }
        };
        self.events.reset_claims_changed();
        reply
    }

    #[allow(clippy::too_many_arguments)]
    fn write(
        &self,
        route: Route,
        db: &Path,
        source: &reset_source_route::SourceContext,
        account: &str,
        caller: &str,
        body: &Value,
        sent: &mut bool,
    ) -> Result<Value> {
        let key = &body["grantKey"];
        match route {
            Route::Prepare => ledger(db, "prepareGate", account, json!({"key":key}))?,
            Route::Claim => ledger(db, "claimGate", account, json!({"key":key}))?,
            _ => Value::Null,
        };
        let snapshot = reset_source_route::read(source, db, account)?;
        match route {
            Route::Prepare => ledger(
                db,
                "prepare",
                account,
                json!({"key":key,"caller":caller,"snapshot":snapshot}),
            ),
            Route::Check => ledger(
                db,
                "check",
                account,
                json!({"key":body["attemptKey"],"snapshot":snapshot}),
            ),
            Route::Claim => {
                let write = ledger(
                    db,
                    "begin",
                    account,
                    json!({
                        "key":key,"caller":caller,"token":body["confirmationToken"],"snapshot":snapshot
                    }),
                )?;
                self.events.reset_claims_changed();
                // Begin committed the pending attempt and consumed the approval. This blocking
                // operation runs to settlement even if the browser disconnects.
                *sent = true;
                let mut result = reset_source_route::send(source, &write)
                    .unwrap_or_else(|_| json!({"outcome":"ambiguous"}));
                if result["outcome"] == "not-sent" {
                    if account.starts_with("claude:") {
                        *sent = false;
                    } else {
                        result = json!({"outcome":"ambiguous"});
                    }
                }
                let after = if *sent {
                    reset_source_route::read(source, db, account).unwrap_or(Value::Null)
                } else {
                    Value::Null
                };
                ledger(
                    db,
                    "finish",
                    account,
                    json!({
                        "key":write["attemptKey"],"outcome":result["outcome"],
                        "retryAfterMs":result["retryAfterMs"],"snapshot":after
                    }),
                )
            }
            _ => Err("shape"),
        }
    }
}
