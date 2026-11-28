//! Write-only HTTP routes for pasted web sessions:
//! - `GET /api/accounts/web-session` (metadata summaries only; never secrets)
//! - `PUT /api/accounts/web-session/{surface}` (verify-before-commit; never echoes secret)
//! - `DELETE /api/accounts/web-session/{surface}` (removes stored session file)

use super::{
    NativeOwners,
    lifecycle_label::{claim_registry, save_registry},
};
use crate::{
    account_usage::sources,
    public_auth::{
        DashboardAccess, Method, Reply, Request, error, lifecycle, now, response, storage,
    },
};
use aac_agent::web_session::{
    ReqwestTransport, host_platform,
    muse::{self, MuseBinding, MuseError},
    opencode::{self, OpenCodeError},
    qwen::{self, QwenError},
};
use aac_files::web_session::{
    self, PasteOptions, SessionError, StoredSession, Surface, parse_pasted,
};
use serde_json::{Map, Value, json};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Route {
    List,
    Put(Surface),
    Delete(Surface),
    OptionsSurface,
}

pub(super) fn route(method: &Method, path: &str) -> Option<Route> {
    let path = path.trim_end_matches('/');
    if path == "/api/accounts/web-session" {
        return matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
            .then_some(Route::List);
    }
    let tail = path.strip_prefix("/api/accounts/web-session/")?;
    if tail.is_empty() || tail.contains('/') {
        return None;
    }
    if *method == Method::OPTIONS {
        return Some(Route::OptionsSurface);
    }
    let surface = Surface::parse(tail)?;
    match *method {
        Method::PUT => Some(Route::Put(surface)),
        Method::DELETE => Some(Route::Delete(surface)),
        _ => None,
    }
}

fn secure_error() -> Reply {
    error(
        403,
        "secure_transport_required",
        "Use the secure dashboard address (HTTPS or a tunnel) for pasted sessions.",
    )
}

fn second_stamp() -> Option<String> {
    let ts = now();
    storage::iso(ts - ts.rem_euclid(1000)).map(|s| s.replace(".000Z", "Z"))
}

pub(super) async fn handle(native: &NativeOwners, op: Route, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return match op {
            Route::List => lifecycle::options("GET, HEAD", "9"),
            Route::OptionsSurface | Route::Put(_) | Route::Delete(_) => {
                lifecycle::options("PUT, DELETE", "11")
            }
        };
    }
    match op {
        Route::List => handle_list(native, request).await,
        Route::Put(surface) => handle_put(native, surface, request, None).await,
        Route::Delete(surface) => handle_delete(native, surface, request).await,
        Route::OptionsSurface => lifecycle::options("PUT, DELETE", "11"),
    }
}

async fn handle_list(native: &NativeOwners, request: Request) -> Reply {
    if let Err(reply) = lifecycle::admit(&request) {
        return super::no_store(reply);
    }
    if request.uri().query().is_some() {
        return super::no_store(lifecycle::unexpected_query());
    }
    let root = native.agents.root().to_owned();
    let result = tokio::task::spawn_blocking(move || web_session::summaries(&root)).await;
    let Ok(summaries) = result else {
        return super::no_store(error(
            500,
            "session_store_unavailable",
            "Saved web sessions could not be read safely.",
        ));
    };
    let mut sessions = Map::new();
    for (surface, meta) in summaries {
        sessions.insert(
            surface.to_owned(),
            meta.map_or(Value::Null, |m| m.public_summary()),
        );
    }
    super::no_store(response(200, json!({ "sessions": sessions })))
}

async fn handle_put(
    native: &NativeOwners,
    surface: Surface,
    request: Request,
    fixture_endpoint: Option<String>,
) -> Reply {
    let secure = request
        .extensions()
        .get::<DashboardAccess>()
        .is_some_and(|access| access.secure_transport);
    let body = match lifecycle::mutation_body_capped(request, lifecycle::WEB_SESSION_BODY_CAP).await
    {
        Ok(body) => body,
        Err(reply) => return super::no_store(reply),
    };
    if !secure {
        return super::no_store(secure_error());
    }
    let Some(cookie) = body.get("cookie").and_then(Value::as_str) else {
        return super::no_store(lifecycle::invalid_action_body());
    };
    let mut opts = PasteOptions::default();
    let mut gateway_cookie: Option<String> = None;
    let mut wallet_account: Option<String> = None;
    for (key, value) in &body {
        match (surface, key.as_str()) {
            (_, "cookie") => {}
            (Surface::OpencodeWallet, "accountId") => match value {
                Value::String(s) => wallet_account = Some(s.clone()),
                _ => return super::no_store(lifecycle::invalid_action_body()),
            },
            (Surface::Qwen, "gatewayCookie") => match value {
                Value::Null => {}
                Value::String(s) => gateway_cookie = Some(s.clone()),
                _ => return super::no_store(lifecycle::invalid_action_body()),
            },
            (Surface::Qwen, "region") => match value {
                Value::Null => {}
                Value::String(s) => opts.region = Some(s.clone()),
                _ => return super::no_store(lifecycle::invalid_action_body()),
            },
            (Surface::OpencodeWallet, "workspaceId") => match value {
                Value::Null => {}
                Value::String(s) => opts.workspace_id = Some(s.clone()),
                _ => return super::no_store(lifecycle::invalid_action_body()),
            },
            (Surface::Muse, "teamId") => match value {
                Value::Null => {}
                Value::String(s) => opts.team_id = Some(s.clone()),
                _ => return super::no_store(lifecycle::invalid_action_body()),
            },
            (Surface::Muse, "email") => match value {
                Value::Null => {}
                Value::String(s) => opts.email = Some(s.clone()),
                _ => return super::no_store(lifecycle::invalid_action_body()),
            },
            (Surface::Muse, "plan") => match value {
                Value::Null => {}
                Value::String(s) => opts.plan = Some(s.clone()),
                _ => return super::no_store(lifecycle::invalid_action_body()),
            },
            _ => return super::no_store(lifecycle::invalid_action_body()),
        }
    }

    let Some(saved_at) = second_stamp() else {
        return super::no_store(error(
            500,
            "write_failed",
            "The session could not be saved safely.",
        ));
    };
    let raw_input = if let Some(gw) = gateway_cookie {
        json!({ "cookie": cookie, "gatewayCookie": gw }).to_string()
    } else {
        cookie.to_owned()
    };
    let mut parsed = match parse_pasted(surface, &raw_input, &opts, &saved_at) {
        Ok(parsed) => parsed,
        Err(err) => return super::no_store(paste_error_reply(err)),
    };
    let wallet_account = match surface {
        Surface::OpencodeWallet => match wallet_account {
            Some(account) if web_session::valid_wallet_account_id(&account) => {
                parsed.meta.account_id = Some(account.clone());
                Some(account)
            }
            _ => return super::no_store(lifecycle::invalid_action_body()),
        },
        Surface::Qwen | Surface::Muse => None,
    };

    let Ok(transport) = ReqwestTransport::with_endpoint(Duration::from_secs(15), fixture_endpoint)
    else {
        return super::no_store(error(
            500,
            "session_store_unavailable",
            "Could not initialize HTTPS client for session verification.",
        ));
    };
    let now_ms = now();

    let platform = host_platform();
    let sample_row =
        match verify_before_commit(&transport, surface, &mut parsed, &opts, now_ms, platform).await
        {
            Ok(sample) => sample,
            Err(reply) => return super::no_store(reply),
        };

    let meta_summary = parsed.meta.public_summary();
    let native_clone = native.clone();
    let commit = native
        .queues
        .run(
            &format!("key:web-session-{}", surface.as_str()),
            async move {
                tokio::task::spawn_blocking(move || {
                    let root = native_clone.agents.root();
                    web_session::save_with_owners(&native_clone.owners, root, surface, &parsed)?;
                    if matches!(surface, Surface::Qwen | Surface::Muse) {
                        let provider = match surface {
                            Surface::Qwen => "qwen",
                            Surface::Muse => "muse",
                            Surface::OpencodeWallet => unreachable!(),
                        };
                        let _ = ensure_v2_provider_entry(
                            root,
                            &native_clone.owners,
                            provider,
                            &saved_at,
                        );
                    }
                    Ok::<_, SessionError>(())
                })
                .await
            },
        )
        .await;

    if !matches!(commit, Ok(Ok(Ok(())))) {
        return super::no_store(error(
            500,
            "session_store_unavailable",
            "The session could not be stored safely. Nothing was saved.",
        ));
    }

    if let Some(usage) = &native.usage {
        usage
            .producer
            .clear_web_session_cache(surface, wallet_account.as_deref());
    }
    native.changed();
    super::no_store(response(
        200,
        json!({
            "ok": true,
            "check": "ok",
            "session": meta_summary,
            "account": sample_row,
        }),
    ))
}

async fn verify_before_commit(
    transport: &ReqwestTransport,
    surface: Surface,
    parsed: &mut StoredSession,
    opts: &PasteOptions,
    now_ms: i64,
    platform: &str,
) -> Result<Value, Reply> {
    match surface {
        Surface::Qwen => match qwen::collect_stored(transport, parsed, now_ms).await {
            Ok(reading) => {
                parsed.meta.region = Some(reading.region.clone());
                Ok(qwen::to_account_row(
                    &reading,
                    platform,
                    "Pasted Qwen console session",
                ))
            }
            Err(QwenError::NeedsSignIn(_)) => Err(error(
                422,
                "session_rejected",
                "Qwen rejected that session cookie. Nothing was saved.",
            )),
            Err(QwenError::Unavailable(_)) => Err(error(
                422,
                "invalid_session",
                "The pasted Qwen session is invalid or uses an unsupported region. Nothing was saved.",
            )),
            Err(QwenError::RateLimited {
                retry_after_seconds,
                ..
            }) => Err(rate_limited_reply(retry_after_seconds)),
            Err(QwenError::ProviderError(_)) => Err(error(
                502,
                "provider_unreachable",
                "Qwen could not be reached to verify the session. Nothing was saved.",
            )),
        },
        Surface::OpencodeWallet => {
            let Some(cookies) = parsed.secret.cookies() else {
                return Err(error(
                    422,
                    "invalid_session",
                    "Paste a Cookie header containing auth or __Host-console_session.",
                ));
            };
            match opencode::collect(transport, cookies, opts.workspace_id.as_deref(), now_ms).await
            {
                Ok(reading) => {
                    parsed.meta.workspace_id = Some(reading.workspace_id.clone());
                    Ok(opencode::to_account_row(&reading, platform))
                }
                Err(OpenCodeError::NeedsSignIn) => Err(error(
                    422,
                    "session_rejected",
                    "OpenCode rejected that console session cookie. Nothing was saved.",
                )),
                Err(OpenCodeError::ChooseWorkspace(workspaces)) => {
                    let list: Vec<Value> = workspaces
                        .into_iter()
                        .map(|w| json!({ "id": w.id, "name": w.name }))
                        .collect();
                    Err(response(
                        409,
                        json!({
                            "error": "Multiple OpenCode workspaces belong to this session. Choose one to save.",
                            "code": "choose_workspace",
                            "workspaces": list,
                        }),
                    ))
                }
                Err(OpenCodeError::WorkspaceMismatch) => Err(error(
                    422,
                    "workspace_mismatch",
                    "That workspace does not belong to the pasted OpenCode session. Nothing was saved.",
                )),
                Err(OpenCodeError::WalletNotPrepaid) => Err(error(
                    422,
                    "wallet_not_prepaid",
                    "That OpenCode workspace does not have a prepaid Zen wallet. Nothing was saved.",
                )),
                Err(OpenCodeError::RateLimited {
                    retry_after_seconds,
                }) => Err(rate_limited_reply(retry_after_seconds.unwrap_or(60.0))),
                Err(
                    OpenCodeError::InvalidResponse
                    | OpenCodeError::ProviderError
                    | OpenCodeError::NetworkError,
                ) => Err(error(
                    502,
                    "provider_unreachable",
                    "OpenCode console could not be reached to verify the session. Nothing was saved.",
                )),
            }
        }
        Surface::Muse => {
            let Some(cookies) = parsed.secret.cookies() else {
                return Err(error(
                    422,
                    "invalid_session",
                    "Paste a Cookie header containing llama_dev_sess or llm_sess.",
                ));
            };
            let local_email = if opts.email.is_none() {
                std::env::home_dir()
                    .as_deref()
                    .and_then(muse::read_local_cli_email)
            } else {
                None
            };
            let expected_email = opts.email.as_deref().or(local_email.as_deref());
            let binding = MuseBinding {
                expected_email,
                expected_plan: opts.plan.as_deref(),
                requested_team: opts.team_id.as_deref(),
                bound_team: None,
                bound_user_hash: None,
            };
            match muse::collect(transport, cookies, &binding, now_ms).await {
                Ok(reading) => {
                    parsed.meta.email = Some(reading.email.clone());
                    parsed.meta.plan = Some(reading.plan.clone());
                    parsed.meta.team_id = Some(reading.team_id.clone());
                    parsed.meta.user_hash = reading.user_hash.clone();
                    Ok(muse::to_account_row(&reading, platform))
                }
                Err(MuseError::NeedsSignIn) => Err(error(
                    422,
                    "session_rejected",
                    "Muse Code rejected that portal session cookie. Nothing was saved.",
                )),
                Err(MuseError::ChooseTeam(teams)) => {
                    let list: Vec<Value> = teams
                        .into_iter()
                        .map(|t| json!({ "id": t.id, "name": t.name }))
                        .collect();
                    Err(response(
                        409,
                        json!({
                            "error": "Multiple Muse Code teams belong to this session. Choose one to save.",
                            "code": "choose_team",
                            "teams": list,
                        }),
                    ))
                }
                Err(MuseError::TeamMismatch) => Err(error(
                    422,
                    "team_mismatch",
                    "That team does not belong to the pasted Muse Code session. Nothing was saved.",
                )),
                Err(MuseError::AccountMismatch) => Err(error(
                    422,
                    "account_mismatch",
                    "That portal session belongs to a different Muse Code account. Nothing was saved.",
                )),
                Err(MuseError::PlanMismatch) => Err(error(
                    422,
                    "plan_mismatch",
                    "The portal team's plan does not match the expected Muse Code tier. Nothing was saved.",
                )),
                Err(MuseError::IdentityUnavailable) => Err(error(
                    422,
                    "identity_unavailable",
                    "The Muse Code portal did not return an account identity for that session. Nothing was saved.",
                )),
                Err(MuseError::RateLimited {
                    retry_after_seconds,
                }) => Err(rate_limited_reply(retry_after_seconds)),
                Err(
                    MuseError::InvalidResponse | MuseError::ProviderError | MuseError::NetworkError,
                ) => Err(error(
                    502,
                    "provider_unreachable",
                    "Muse Code portal could not be reached to verify the session. Nothing was saved.",
                )),
            }
        }
    }
}

async fn handle_delete(native: &NativeOwners, surface: Surface, request: Request) -> Reply {
    let secure = request
        .extensions()
        .get::<DashboardAccess>()
        .is_some_and(|access| access.secure_transport);
    let body = match lifecycle::mutation_body(request).await {
        Ok(body) => body,
        Err(reply) => return super::no_store(reply),
    };
    if !secure {
        return super::no_store(secure_error());
    }
    // One account's wallet session (`{"accountId": "..."}`), the legacy global
    // session (empty body), or nothing else.
    let wallet_account = match surface {
        Surface::OpencodeWallet => match body.get("accountId") {
            Some(Value::String(s)) if body.len() == 1 => {
                if web_session::valid_wallet_account_id(s) {
                    Some(s.clone())
                } else {
                    return super::no_store(lifecycle::invalid_action_body());
                }
            }
            None if body.is_empty() => None,
            _ => return super::no_store(lifecycle::invalid_action_body()),
        },
        Surface::Qwen | Surface::Muse => {
            if !body.is_empty() {
                return super::no_store(lifecycle::invalid_action_body());
            }
            None
        }
    };
    let native_clone = native.clone();
    let queued_account = wallet_account.clone();
    let deleted = native
        .queues
        .run(
            &format!("key:web-session-{}", surface.as_str()),
            async move {
                let account = queued_account;
                tokio::task::spawn_blocking(move || {
                    let root = native_clone.agents.root();
                    let existed = match account.as_deref() {
                        Some(account) => web_session::summary_for_account(root, surface, account)
                            .ok()
                            .flatten()
                            .is_some(),
                        None => web_session::summary(root, surface).ok().flatten().is_some(),
                    };
                    match account.as_deref() {
                        Some(account) => web_session::delete_for_account_with_owners(
                            &native_clone.owners,
                            root,
                            surface,
                            account,
                        )?,
                        None => {
                            web_session::delete_with_owners(&native_clone.owners, root, surface)?
                        }
                    }
                    Ok::<_, SessionError>(existed)
                })
                .await
            },
        )
        .await;
    match deleted {
        Ok(Ok(Ok(removed))) => {
            if let Some(usage) = &native.usage {
                usage
                    .producer
                    .clear_web_session_cache(surface, wallet_account.as_deref());
            }
            native.changed();
            let mut out = json!({
                "ok": true,
                "deleted": removed,
                "surface": surface.as_str(),
            });
            if let Some(account) = wallet_account.as_deref() {
                out["accountId"] = json!(account);
            }
            super::no_store(response(200, out))
        }
        _ => super::no_store(error(
            500,
            "session_store_unavailable",
            "The saved web session could not be removed safely.",
        )),
    }
}

fn rate_limited_reply(seconds: f64) -> Reply {
    let secs = seconds.ceil().max(1.0) as i64;
    let mut reply = response(
        429,
        json!({
            "error": "The provider rate limited the session check. Try again later.",
            "code": "rate_limited",
            "retryAfterSeconds": secs,
        }),
    );
    crate::public_auth::set_header(&mut reply, "retry-after", &secs.to_string());
    reply
}

fn paste_error_reply(err: SessionError) -> Reply {
    let msg = match err {
        SessionError::InvalidInput => "Paste a valid single-line Cookie header for this service.",
        SessionError::MissingRequiredCookie => {
            "That paste does not contain the required session cookie for this service."
        }
        SessionError::DisallowedToken => {
            "That looks like a CLI or API token, not a browser portal session cookie."
        }
        SessionError::InvalidMetadata => {
            "One of the region, workspace, team, or account fields is not valid."
        }
        SessionError::StorageUnavailable | SessionError::UnsupportedEncoding => {
            "The session could not be validated safely."
        }
    };
    error(422, "invalid_session", msg)
}

fn ensure_v2_provider_entry(
    root: &std::path::Path,
    owners: &aac_files::registry::Owners,
    provider: &str,
    saved_at: &str,
) -> Result<(), ()> {
    let Ok(sources::Sources::V2(_)) = sources::read_registry(root) else {
        return Ok(());
    };
    let (writer, mut entries) = claim_registry(root, owners).map_err(|_| ())?;
    if entries.iter().any(|entry| entry.provider == provider) || entries.len() >= 64 {
        return Ok(());
    }
    let (manifest, _) = sources::legacy(Some(root));
    let Some(mut entry) = manifest.into_iter().find(|e| e.provider == provider) else {
        return Ok(());
    };
    entry.created_at = Some(saved_at.to_owned());
    entries.push(entry);
    save_registry(&writer, &entries).map_err(|_| ())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::public_auth::{DashboardEvents, DashboardPrincipal, HubCache};
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::header,
        routing::any,
    };
    use std::sync::Arc;

    fn browser_access(secure: bool, same_origin: bool) -> DashboardAccess {
        DashboardAccess {
            auth_enabled: true,
            principal: DashboardPrincipal::Browser {
                username: "fixture".into(),
            },
            secure_transport: secure,
            same_origin,
            origin_present: same_origin,
            local_access: true,
            session_id: Some("sess-fixture".into()),
            credential_transport: if secure { "https" } else { "insecure" },
            direct_loopback: true,
            lan_proxy: false,
            client_address: None,
        }
    }

    fn device_access() -> DashboardAccess {
        DashboardAccess {
            principal: DashboardPrincipal::Device {
                id: "dev-1".into(),
                platform: "mac".into(),
            },
            ..browser_access(true, true)
        }
    }

    fn json_request(method: Method, uri: &str, body: Value, access: DashboardAccess) -> Request {
        let bytes = body.to_string().into_bytes();
        let mut req = Request::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::CONTENT_LENGTH, bytes.len().to_string())
            .body(Body::from(bytes))
            .unwrap();
        req.extensions_mut().insert(access);
        req
    }

    async fn spawn_upstream(
        handler: impl Fn(String) -> (u16, String) + Send + Sync + 'static,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let handler = Arc::new(handler);
        let app = Router::new().fallback(any(move |req: Request| {
            let handler = handler.clone();
            async move {
                let url = req
                    .headers()
                    .get("x-aac-fixture-url")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("")
                    .to_owned();
                let (status, body) = handler(url);
                axum::http::Response::builder()
                    .status(status)
                    .header("content-type", "application/json")
                    .body(Body::from(body))
                    .unwrap()
            }
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        (format!("http://{addr}"), task)
    }

    async fn start_native(root: &std::path::Path) -> (NativeOwners, super::super::Jobs) {
        NativeOwners::start(
            root.to_path_buf(),
            super::super::Groups {
                writes: true,
                lifecycle: true,
                ..Default::default()
            },
            HubCache::default(),
            DashboardEvents(tokio::sync::broadcast::channel(4).0),
            Default::default(),
            std::sync::Arc::new(crate::agent_transport::Agents::with_default(
                root.to_path_buf(),
                true,
            )),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn web_session_routes_enforce_secure_origin_device_and_verify_before_commit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (native, jobs) = start_native(&root).await;

        let secret = "auth=super-secret-fixture-cookie-999";

        // 1. Insecure transport refused with 403
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret }),
            browser_access(false, true),
        );
        let reply = handle_put(&native, Surface::OpencodeWallet, req, None).await;
        assert_eq!(reply.status(), 403);
        assert_eq!(reply.headers()["cache-control"], "no-store");

        // 2. Bad origin refused with 403
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret }),
            browser_access(true, false),
        );
        let reply = handle_put(&native, Surface::OpencodeWallet, req, None).await;
        assert_eq!(reply.status(), 403);

        // 3. Device token refused with 403
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret }),
            device_access(),
        );
        let reply = handle_put(&native, Surface::OpencodeWallet, req, None).await;
        assert_eq!(reply.status(), 403);

        // 4. Muse dca: token refused before network, nothing saved
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/muse",
            json!({ "cookie": "llama_dev_sess=dca:wrong-cli-token" }),
            browser_access(true, true),
        );
        let reply = handle_put(&native, Surface::Muse, req, None).await;
        assert_eq!(reply.status(), 422);
        assert!(web_session::load(&root, Surface::Muse).unwrap().is_none());

        // 5. An OpenCode wallet paste names its account, or nothing is saved.
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret }),
            browser_access(true, true),
        );
        let reply = handle_put(&native, Surface::OpencodeWallet, req, None).await;
        assert_eq!(reply.status(), 400);
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret, "accountId": "opencode-go:../escape" }),
            browser_access(true, true),
        );
        let reply = handle_put(&native, Surface::OpencodeWallet, req, None).await;
        assert_eq!(reply.status(), 400);
        assert!(
            web_session::load(&root, Surface::OpencodeWallet)
                .unwrap()
                .is_none()
        );

        // 6. Upstream 401 -> 422 session_rejected, nothing saved, never echoes secret
        let (bad_endpoint, bad_srv) = spawn_upstream(|_| (401, "{}".into())).await;
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret, "accountId": "opencode-go:usage" }),
            browser_access(true, true),
        );
        let reply = handle_put(&native, Surface::OpencodeWallet, req, Some(bad_endpoint)).await;
        assert_eq!(reply.status(), 422);
        let text =
            String::from_utf8(to_bytes(reply.into_body(), 8192).await.unwrap().to_vec()).unwrap();
        assert!(text.contains("session_rejected"));
        assert!(!text.contains("super-secret-fixture-cookie-999"));
        assert!(
            web_session::load_for_account(&root, Surface::OpencodeWallet, "opencode-go:usage")
                .unwrap()
                .is_none()
        );
        bad_srv.abort();

        // 7. Multi-workspace -> 409 choose_workspace, nothing saved
        let (multi_endpoint, multi_srv) = spawn_upstream(|url| {
            if url.ends_with("/orgs") {
                return (
                    200,
                    r#"[{"id":"wrk_a","name":"Personal"},{"id":"wrk_b","name":"Team"}]"#.into(),
                );
            }
            if url.ends_with("/billing/status") {
                return (
                    200,
                    r#"{"mode":"pay-as-you-go","billingMode":"prepaid","balanceMicroCents":"500000000"}"#.into(),
                );
            }
            (200, "{}".into())
        })
        .await;
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret, "accountId": "opencode-go:usage" }),
            browser_access(true, true),
        );
        let reply = handle_put(
            &native,
            Surface::OpencodeWallet,
            req,
            Some(multi_endpoint.clone()),
        )
        .await;
        assert_eq!(reply.status(), 409);
        let body: Value =
            serde_json::from_slice(&to_bytes(reply.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(body["code"], "choose_workspace");
        assert_eq!(body["workspaces"].as_array().unwrap().len(), 2);
        assert!(
            web_session::load_for_account(&root, Surface::OpencodeWallet, "opencode-go:usage")
                .unwrap()
                .is_none()
        );

        // 8. Verified PUT with workspaceId and accountId -> 200, saved per account
        let req = json_request(
            Method::PUT,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "cookie": secret, "workspaceId": "wrk_b", "accountId": "opencode-go:usage" }),
            browser_access(true, true),
        );
        let reply = handle_put(&native, Surface::OpencodeWallet, req, Some(multi_endpoint)).await;
        assert_eq!(reply.status(), 200);
        assert_eq!(reply.headers()["cache-control"], "no-store");
        let text =
            String::from_utf8(to_bytes(reply.into_body(), 16384).await.unwrap().to_vec()).unwrap();
        assert!(!text.contains("super-secret-fixture-cookie-999"));
        let body: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(body["session"]["workspaceId"], "wrk_b");
        assert_eq!(body["session"]["accountId"], "opencode-go:usage");
        assert_eq!(body["account"]["windows"][0]["remaining"], 5.0);
        assert!(
            web_session::load_for_account(&root, Surface::OpencodeWallet, "opencode-go:usage")
                .unwrap()
                .is_some()
        );
        assert!(
            web_session::load(&root, Surface::OpencodeWallet)
                .unwrap()
                .is_none()
        );
        multi_srv.abort();

        // 9. GET returns metadata summaries only, zero secret; the per-account
        // wallet session is not the legacy global slot.
        let mut get_req = Request::builder()
            .method(Method::GET)
            .uri("/api/accounts/web-session")
            .body(Body::empty())
            .unwrap();
        get_req.extensions_mut().insert(browser_access(true, true));
        let reply = handle_list(&native, get_req).await;
        assert_eq!(reply.status(), 200);
        assert_eq!(reply.headers()["cache-control"], "no-store");
        let text =
            String::from_utf8(to_bytes(reply.into_body(), 16384).await.unwrap().to_vec()).unwrap();
        assert!(!text.contains("super-secret-fixture-cookie-999"));
        let body: Value = serde_json::from_str(&text).unwrap();
        assert!(body["sessions"]["opencode-wallet"].is_null());
        assert!(body["sessions"]["qwen"].is_null());

        // 10. DELETE removes one account's session and nothing else
        let del_req = json_request(
            Method::DELETE,
            "/api/accounts/web-session/opencode-wallet",
            json!({ "accountId": "opencode-go:usage" }),
            browser_access(true, true),
        );
        let reply = handle_delete(&native, Surface::OpencodeWallet, del_req).await;
        assert_eq!(reply.status(), 200);
        let body: Value =
            serde_json::from_slice(&to_bytes(reply.into_body(), 8192).await.unwrap()).unwrap();
        assert_eq!(body["deleted"], true);
        assert_eq!(body["accountId"], "opencode-go:usage");
        assert!(
            web_session::load_for_account(&root, Surface::OpencodeWallet, "opencode-go:usage")
                .unwrap()
                .is_none()
        );

        jobs.stop().await;
    }
}
