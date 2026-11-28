//! Sign in again and provider preparation for Lane G's existing interactive session.
use super::{
    NativeOwners, lifecycle_antigravity as antigravity, lifecycle_codex as codex,
    lifecycle_jobs::Jobs,
    lifecycle_maintenance,
    lifecycle_registry::{antigravity_preflight, codex_cli},
    recheck,
};
use crate::{
    account_reads::public_account_id,
    account_usage::{
        producer,
        sources::{self, Credential, Source},
    },
    agent_transport::{Agents, Selector},
    public_auth::{
        self, DashboardAccess, DashboardSignInJob, Method, Reply, Request, error, lifecycle, now,
        response, storage,
    },
    signin_callbacks,
};
use aac_core::signin::ErrorCode;
use serde_json::{Map, Value, json};
use std::{path::PathBuf, sync::Arc};

#[derive(Clone)]
pub(super) enum Flow {
    Codex {
        name: String,
        add: bool,
        cli: PathBuf,
    },
    Muse {
        // Boxed: the registry entry is far larger than the other variants.
        source: Box<Source>,
        alias: String,
        home_id: Option<String>,
    },
    Antigravity {
        name: String,
        add: bool,
        stage: Option<Arc<antigravity::Stage>>,
    },
}

impl Flow {
    pub(super) fn initial(&self, id: String) -> Option<DashboardSignInJob> {
        let (provider, mode, account_id, name, platform) = match self {
            Self::Codex { name, add, .. } => (
                "codex",
                if *add { "add" } else { "signin-again" },
                (!*add).then(|| format!("codex:{name}")),
                name.clone(),
                "ubuntu",
            ),
            Self::Muse { source, .. } => (
                "muse",
                "signin-again",
                Some(source.id.clone()),
                source.id.clone(),
                "mac",
            ),
            Self::Antigravity { name, add, .. } => (
                "antigravity",
                if *add { "add" } else { "signin-again" },
                (!*add).then(|| format!("antigravity:profile:{name}")),
                name.clone(),
                "ubuntu",
            ),
        };
        let now = now();
        let stamp = storage::iso(now)?;
        Some(DashboardSignInJob {
            id,
            provider: provider.into(),
            kind: if matches!(self, Self::Antigravity { .. }) {
                "supervised-cli"
            } else {
                "device-code"
            }
            .into(),
            mode: mode.into(),
            account_id,
            profile_name: Some(name),
            platform: platform.into(),
            state: "starting".into(),
            verification: None,
            result: None,
            error: None,
            started_at: stamp.clone(),
            updated_at: stamp,
            expires_at: storage::iso(now + 900_000)?,
        })
    }

    pub(super) async fn prepare(
        &mut self,
        agents: &Arc<Agents>,
        id: &str,
    ) -> Result<Value, ErrorCode> {
        match self {
            Self::Codex { cli, .. } => {
                let cli = cli.clone();
                let root = agents.root().to_owned();
                let id = id.to_owned();
                tokio::task::spawn_blocking(move || codex::prepare(&root, &id, &cli))
                    .await
                    .map_err(|_| ErrorCode::WriteFailed)?
            }
            Self::Muse { alias, home_id, .. } => {
                let command = signin_callbacks::muse_commands(home_id.as_deref())
                    .and_then(|commands| commands["remoteLogin"].as_str().map(str::to_owned))
                    .ok_or(ErrorCode::WriteFailed)?;
                let command = agents
                    .interactive_command(
                        &Selector::usage("mac", Some(alias)),
                        &["/bin/sh".into(), "-c".into(), command.into()],
                    )
                    .await
                    .ok_or(ErrorCode::ToolMissing)?;
                let file = command.file.to_str().ok_or(ErrorCode::ToolMissing)?;
                let argv = command
                    .args
                    .iter()
                    .map(|arg| arg.to_str())
                    .collect::<Option<Vec<_>>>()
                    .ok_or(ErrorCode::WriteFailed)?;
                let env: Map<_, _> = command
                    .env
                    .into_iter()
                    .filter_map(|(key, value)| {
                        Some((
                            key.into_string().ok()?,
                            Value::String(value.into_string().ok()?),
                        ))
                    })
                    .collect();
                Ok(json!({"file":file,"args":argv,"env":env,"pty":false}))
            }
            Self::Antigravity { name, stage, .. } => {
                let root = agents.root().to_owned();
                let id = id.to_owned();
                let name = name.clone();
                let (prepared, command) = tokio::task::spawn_blocking(move || {
                    antigravity::Stage::prepare(root, &id, &name)
                })
                .await
                .map_err(|_| ErrorCode::WriteFailed)??;
                *stage = Some(Arc::new(prepared));
                Ok(command)
            }
        }
    }

    pub(super) fn origins(&self) -> &'static [&'static str] {
        match self {
            Self::Codex { .. } => &["https://auth.openai.com"],
            Self::Muse { .. } => &[
                "https://dev.meta.ai",
                "https://auth.meta.com",
                "https://www.meta.ai",
            ],
            Self::Antigravity { .. } => &[antigravity::ORIGIN],
        }
    }

    pub(super) async fn install(
        &self,
        native: &NativeOwners,
        jobs: Arc<Jobs>,
        id: &str,
    ) -> Result<Value, ErrorCode> {
        if jobs.stopped(id) {
            return Err(ErrorCode::Timeout);
        }
        let agents = &native.agents;
        match self {
            Self::Codex { name, add, .. } => {
                let name = name.clone();
                let add = *add;
                let root = agents.root().to_owned();
                let id = id.to_owned();
                tokio::task::spawn_blocking(move||{
                    let stamp=storage::iso(now()).ok_or(ErrorCode::WriteFailed)?;
                    let identity=codex::install(&root,&id,&name,add,&stamp,||jobs.stopped(&id))?;
                    Ok(json!({"accountId":format!("codex:{name}"),"email":identity.email,"plan":identity.plan}))
                }).await.map_err(|_|ErrorCode::WriteFailed)?
            }
            Self::Muse {
                source,
                alias,
                home_id,
            } => {
                // A changed source must never receive the active job's completion.
                let expected = producer::recheck::lookup(agents.root().to_owned(), &source.id)
                    .await
                    .ok()
                    .flatten()
                    .filter(|resolved| {
                        super::lifecycle_keys::key_binding(&resolved.source, source)
                    });
                if expected.is_none() {
                    return Err(ErrorCode::WriteFailed);
                }
                let computer = Selector::usage("mac", Some(alias));
                let email = agents
                    .call_value(
                        "muse-login-identity",
                        computer,
                        json!({"homeId":home_id}),
                        58_000,
                        60_000,
                        4096,
                    )
                    .await
                    .and_then(|reply| reply["email"].as_str().map(str::to_owned))
                    .ok_or(ErrorCode::WriteFailed)?;
                let email = email.trim();
                if jobs.stopped(id) || !signin_callbacks::muse_email_valid(email) {
                    return Err(ErrorCode::WriteFailed);
                }
                Ok(json!({"accountId":source.id,"email":email,"plan":null}))
            }
            Self::Antigravity { name, add, stage } => {
                let stage = stage.clone().ok_or(ErrorCode::WriteFailed)?;
                let name = name.clone();
                let add = *add;
                let native = native.clone();
                let id = id.to_owned();
                tokio::task::spawn_blocking(move || {
                    let identity =
                        antigravity::install(&native, &stage, &name, add, || jobs.stopped(&id))?;
                    Ok(json!({"accountId":format!("antigravity:profile:{name}"),
                        "email":identity.email,"plan":identity.plan}))
                })
                .await
                .map_err(|_| ErrorCode::WriteFailed)?
            }
        }
    }

    pub(super) async fn cleanup(self, root: PathBuf, id: String) {
        match self {
            Self::Codex { .. } => {
                let _ = tokio::task::spawn_blocking(move || codex::cleanup(&root, &id)).await;
            }
            Self::Antigravity { stage, .. } => {
                let _ = tokio::task::spawn_blocking(move || drop(stage)).await;
            }
            Self::Muse { .. } => {}
        }
    }
}

pub(super) fn route<'a>(method: &Method, path: &'a str) -> Option<&'a str> {
    if !matches!(*method, Method::POST | Method::OPTIONS) {
        return None;
    }
    let id = path
        .strip_prefix("/api/accounts/")?
        .strip_suffix("/signin-again")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

pub(super) async fn handle(native: &NativeOwners, raw_id: &str, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return lifecycle::options("POST", "4");
    }
    let secure = request
        .extensions()
        .get::<DashboardAccess>()
        .is_some_and(|access| access.secure_transport);
    let body = match lifecycle::mutation_body(request).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    if !body.is_empty() {
        return lifecycle::invalid_action_body();
    }
    let Some(id) = public_auth::decode_component(raw_id).filter(|id| public_account_id(&json!(id)))
    else {
        return error(400, "invalid_account", "That account id is not valid.");
    };
    let root = native.agents.root().to_owned();
    if !sources::is_additional_account_id(&id) {
        if id.starts_with("plan-opencode-go-console-") {
            return guide("browser-extension", "mac");
        }
        let known = {
            let root = root.clone();
            let id = id.clone();
            tokio::task::spawn_blocking(move || recheck::other_account_exists(&root, &id))
                .await
                .unwrap_or_default()
        };
        if !known {
            return error(404, "unknown_account", "That account was not found.");
        }
        if id.starts_with("claude:") {
            return guide("open-app", json!(["mac", "windows"]));
        }
        if let Some(name) = id.strip_prefix("codex:") {
            if !secure {
                return terminal("ubuntu", "codex login --device-auth");
            }
            return codex_start(native, name, false).await;
        }
        if let Some(name) = id.strip_prefix("antigravity:profile:") {
            return antigravity_start(native, name, false, secure).await;
        }
        return not_implemented();
    }
    let source = match producer::recheck::lookup(root, &id).await {
        Ok(Some(row)) => row.source,
        Ok(None) => return error(404, "unknown_account", "That account was not found."),
        Err(_) => {
            return error(
                500,
                "registry_unavailable",
                "Account list could not be read safely.",
            );
        }
    };
    match source.provider.as_str() {
        "cursor" => guide(
            "open-app",
            if source.platform == "ubuntu" {
                json!([])
            } else {
                json!([source.platform])
            },
        ),
        "qwen" => guide(
            "browser-extension",
            if source.platform == "mac" {
                "mac"
            } else {
                "windows"
            },
        ),
        "muse" => {
            if !secure {
                return terminal("mac", "muse login");
            }
            if std::env::var("CCS_MUSE_SIGNIN").as_deref() != Ok("on") {
                return not_implemented();
            }
            let Some(alias) = source.ssh_host.clone().filter(|_| source.platform == "mac") else {
                return not_configured();
            };
            let home_id = match &source.credential {
                Credential::Discover => None,
                Credential::ConfigHome { home_id } => Some(home_id.clone()),
                _ => return not_configured(),
            };
            start(
                native,
                Flow::Muse {
                    source: Box::new(source),
                    alias,
                    home_id,
                },
            )
        }
        provider if aac_providers::provider(provider).is_some_and(|p| p.key_store) => {
            error(409, "use_replace_key", "Use Replace key for this account.")
        }
        _ => not_implemented(),
    }
}

pub(super) async fn add_codex(native: &NativeOwners, body: &Map<String, Value>) -> Reply {
    let Some(name) = body
        .get("profileName")
        .and_then(Value::as_str)
        .filter(|name| {
            body.len() == 2 && aac_files::codex_registry::profile_name_error(name).is_ok()
        })
    else {
        return lifecycle::invalid_action_body();
    };
    codex_start(native, name, true).await
}

pub(super) async fn add_antigravity(
    native: &NativeOwners,
    body: &Map<String, Value>,
    secure: bool,
) -> Reply {
    let Some(name) = body
        .get("profileName")
        .and_then(Value::as_str)
        .filter(|name| body.len() == 2 && crate::antigravity_runtime::profile_id_ok(name))
    else {
        return lifecycle::invalid_action_body();
    };
    antigravity_start(native, name, true, secure).await
}

async fn antigravity_start(native: &NativeOwners, name: &str, add: bool, secure: bool) -> Reply {
    let name = name.to_owned();
    let root = native.agents.root().to_owned();
    let gate = tokio::task::spawn_blocking(move || {
        let unavailable = || error(500, "registry_unavailable", "Account list could not be read safely.");
        let (_document, parsed) = antigravity::current(&root).map_err(|_| unavailable())?;
        if add && parsed.as_ref().is_some_and(|parsed| parsed.profile(&name).is_some()) {
            return Err(error(409, "id_in_use", "That name is already used by another account."));
        }
        if add && parsed.as_ref().is_some_and(|parsed| parsed.profiles.len() >= 16) {
            return Err(error(409,"too_many_accounts","This provider already has the most accounts allowed."));
        }
        let fallback = format!("ai-account-center antigravity signin {name}");
        let home = std::env::home_dir();
        let preflight = secure.then(|| antigravity_preflight(home.as_deref())).flatten();
        if add && !secure {
            return Err(terminal("ubuntu", &fallback));
        }
        if preflight == Some("tool_missing") {
            return Err(error(409, "tool_missing", "The sign-in tool is not installed on the dashboard computer."));
        }
        let race = || {
            let lock = aac_files::read::entry_exists(&root.join("antigravity-profiles/.transaction-lock"))
                .map_err(|_| unavailable())?;
            if lock {
                return Err(error(409,"activation_running","Another change to these saved logins is running. Wait for it to finish."));
            }
            if lifecycle_maintenance::antigravity_signin_running(&root, &name) {
                return Err(error(409,"signin_running","A sign-in for this account is running."));
            }
            Ok(())
        };
        if !add { race()?; }
        if !secure {
            return Err(terminal("ubuntu", &fallback));
        }
        if preflight.is_some() {
            return Err(super::no_store(response(409, json!({
                "error":"Sign in to this provider from a terminal on Ubuntu with the command shown.",
                "code":"preflight_failed","fallback":{"kind":"terminal","host":"ubuntu","command":fallback}
            }))));
        }
        if add { race()?; }
        Ok(Flow::Antigravity { name, add, stage: None })
    }).await;
    match gate {
        Ok(Ok(flow)) => start(native, flow),
        Ok(Err(reply)) => reply,
        Err(_) => error(
            500,
            "registry_unavailable",
            "Account list could not be read safely.",
        ),
    }
}

async fn codex_start(native: &NativeOwners, name: &str, add: bool) -> Reply {
    let root = native.agents.root().to_owned();
    let name = name.to_owned();
    let result = tokio::task::spawn_blocking(move || {
        let cli = codex_cli().ok_or((
            "tool_missing",
            "The sign-in tool is not installed on the dashboard computer.",
        ))?;
        if add {
            let registry = codex::registry(&root)
                .map_err(|_| ("write_failed", "The change could not be saved safely."))?;
            if registry.profiles.len() >= 16 {
                return Err((
                    "too_many_accounts",
                    "This provider already has the most accounts allowed.",
                ));
            }
            if codex::name_in_use(&root, &name, &registry) {
                return Err(("id_in_use", "That name is already used by another account."));
            }
        }
        Ok(Flow::Codex { name, add, cli })
    })
    .await;
    match result {
        Ok(Ok(flow)) => start(native, flow),
        Ok(Err(("write_failed", message))) => error(500, "write_failed", message),
        Ok(Err((code, message))) => error(409, code, message),
        Err(_) => error(500, "write_failed", "The change could not be saved safely."),
    }
}

fn guide(kind: &str, target: impl serde::Serialize) -> Reply {
    let guide = if kind == "open-app" {
        json!({"kind":kind,"platforms":target})
    } else {
        json!({"kind":kind,"platform":target})
    };
    super::no_store(response(200, json!({"guide":guide})))
}

fn terminal(host: &str, command: &str) -> Reply {
    super::no_store(response(
        403,
        json!({
            "error":"Use the secure dashboard address (HTTPS or a tunnel) for keys and sign-in codes.",
            "code":"secure_transport_required","fallback":{"kind":"terminal","host":host,"command":command}
        }),
    ))
}

fn not_implemented() -> Reply {
    error(
        409,
        "not_implemented",
        "This action is not available on this server yet.",
    )
}
fn not_configured() -> Reply {
    error(
        409,
        "not_configured",
        "That app has no launcher on that computer.",
    )
}

fn start(native: &NativeOwners, flow: Flow) -> Reply {
    let Some(jobs) = &native.lifecycle_jobs else {
        return not_implemented();
    };
    jobs.clone().start(native.clone(), flow)
}
