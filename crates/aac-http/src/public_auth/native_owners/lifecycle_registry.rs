//! Project the account registry from the shared native collection generation.
use super::{NativeOwners, lifecycle_jobs::Jobs, lifecycle_label::local_key_info};
use crate::{
    account_usage::{AccountUsageCache, Desktop, sources},
    claude_open::manifest,
    public_auth::{Method, Reply, Request, error, lifecycle, now, set_header, storage},
};
use aac_files::{codex_identity, codex_registry, read};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub(super) fn route(method: &Method, path: &str) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        && path == "/api/accounts/registry"
}

pub(super) async fn handle(native: &NativeOwners, jobs: &Jobs, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return lifecycle::options("GET, HEAD", "9");
    }
    let secure = match lifecycle::admit(&request) {
        Ok(access) => access.secure_transport,
        Err(reply) => return reply,
    };
    if request.uri().query().is_some() {
        return lifecycle::unexpected_query();
    }
    let Some(usage) = &native.lifecycle_usage else {
        return unavailable();
    };
    let Some(dashboard) = dashboard(&usage.cache, secure).await else {
        return unavailable();
    };
    let root = native.agents.root().to_owned();
    let jobs = jobs.running(secure);
    let result = tokio::task::spawn_blocking(move || listing(&root, dashboard, jobs)).await;
    let Ok(Some(body)) = result else {
        return unavailable();
    };
    let bytes = aac_views::js_json::write(&body);
    let compressible = bytes.len() >= 1024;
    let mut reply = super::no_store(crate::public_auth::json_bytes(200, bytes));
    if compressible {
        set_header(&mut reply, "vary", "Accept-Encoding");
    }
    reply
}

fn unavailable() -> Reply {
    error(
        500,
        "internal_error",
        "The request could not be completed safely.",
    )
}

async fn dashboard(cache: &AccountUsageCache, secure: bool) -> Option<Value> {
    let render = || {
        let at = now();
        let bytes = cache
            .render(Desktop::Mac, secure, &storage::iso(at)?, at)
            .ok()?;
        serde_json::from_slice(&bytes).ok()
    };
    if let Some(dashboard) = render() {
        return Some(dashboard);
    }
    cache.request_refresh();
    if cache
        .await_generation(Desktop::Mac, secure, Duration::from_secs(4))
        .await
    {
        render()
    } else {
        None
    }
}

fn listing(
    root: &Path,
    mut dashboard: Value,
    jobs: Vec<crate::public_auth::DashboardSignInJob>,
) -> Option<Value> {
    let entries = match sources::read_registry(root) {
        Ok(sources::Sources::V2(entries)) => entries,
        Ok(sources::Sources::Legacy) => {
            let (entries, valid) = sources::legacy(Some(root));
            if valid { entries } else { Vec::new() }
        }
        Err(_) => Vec::new(),
    };
    let home = std::env::home_dir();
    let antigravity = antigravity_preflight(home.as_deref());
    let codex = codex_cli().is_none().then_some("tool_missing");
    let providers = dashboard["providers"].as_array_mut()?;
    apply_preflight(providers, "codex", codex);
    apply_preflight(providers, "antigravity", antigravity);
    let providers = dashboard["providers"].as_array()?;
    let running = |id: &str| jobs.iter().any(|job| job.account_id.as_deref() == Some(id));
    let provider = |id: &Value| providers.iter().find(|entry| entry["id"] == *id);
    let claude_inventory = manifest::list_profiles(root).unwrap_or_default();
    let claude_pending = manifest::read_pending_profiles(root).unwrap_or_default();
    let codex = codex_registry::read(&root.join("codex-profiles.yaml"));
    let antigravity_state = crate::antigravity_runtime::read_state(root);

    let rows = dashboard["accounts"].as_array()?.iter().map(|row| {
        let id = row["id"].as_str().unwrap_or_default();
        let capabilities = &row["capabilities"];
        let provider = provider(&row["provider"]);
        let running = running(id);
        match row["provider"].as_str() {
            Some("codex") if capabilities["codexProfile"].is_string() => {
                let name = capabilities["codexProfile"].as_str().unwrap_or_default();
                let refusal = codex_refusal(root, home.as_deref(), codex.as_ref(), name, row["isActive"] == true, running);
                let available = provider.is_some_and(|p| p["signIn"]["available"] == true);
                return json!({"kind":"codex","row":row,"refusal":refusal,"available":available});
            }
            Some("claude") => {
                let name = capabilities["claudeProfileId"].as_str();
                let profile = claude_inventory.iter().find(|item| item.id.as_deref() == name && name.is_some()).map(|item| {
                    let protected = [item.mac.as_ref(),item.windows.as_ref()].into_iter().flatten().any(|host|
                        host.is_default == Some(true) || manifest::is_default_data_folder(host.profile_path.as_deref()));
                    json!({"isDefault":protected,"email":item.email})
                }).or_else(|| {
                    let pending = claude_pending.iter().find(|item| Some(item.id.as_str()) == name)?;
                    let protected = [&pending.mac,&pending.windows].into_iter().any(|host|
                        manifest::is_default_data_folder(host.profile_path.as_deref()));
                    Some(json!({"isDefault":protected,"email":null}))
                });
                let allowed = provider.is_some_and(|p| p["capabilities"]["remove"] == true);
                return json!({"kind":"claude","row":row,"claudePlatforms":capabilities["claudePlatforms"],
                    "profile":profile,"removeAllowed":allowed});
            }
            Some("antigravity") if capabilities["antigravityProfileId"].is_string() => {
                let profile = capabilities["antigravityProfileId"].as_str().unwrap_or_default();
                let refusal = if read::stat(&root.join("antigravity-profiles/.transaction-lock")).is_ok() {
                    Some("activation_running")
                } else if row["isActive"] == true || antigravity_state.as_ref().is_some_and(|state| state.active_for(profile).is_some()) {
                    Some("account_active")
                } else if running || read::stat(&root.join("antigravity-signin").join(format!("{profile}.running"))).is_ok() {
                    Some("signin_running")
                } else { None };
                let reason = provider.map(|p| p["signIn"]["unavailableReason"].clone());
                return json!({"kind":"antigravity","row":row,"refusal":refusal,
                    "reason":reason,"removeAllowed":true});
            }
            _ => {}
        }
        if let Some(source) = entries.iter().find(|source| source.id == id) {
            let key = match &source.credential {
                sources::Credential::AacKey {key_id} => local_key_info(root,source,key_id),
                _ => None,
            };
            let entry = serde_json::to_value(source).unwrap_or(Value::Null);
            let muse = provider.is_some_and(|p| p["signIn"]["available"] == true);
            return json!({"kind":"entry","row":row,"entry":entry,"credentialKind":entry["credential"]["kind"],
                "key":key,"museSignIn":muse,"running":running});
        }
        json!({"kind":"other","row":row,"running":running,"walletRemove":id.starts_with("plan-opencode-go-console-")})
    }).collect::<Vec<_>>();
    let trash = crate::claude_account_store::list_trash(root).unwrap_or_default();
    aac_views::registry::compose(&json!({
        "providers":dashboard["providers"], "rows":rows, "jobs":jobs, "trash":trash
    }))
    .ok()
}

fn apply_preflight(providers: &mut [Value], id: &str, reason: Option<&str>) {
    if let Some(reason) = reason
        && let Some(provider) = providers.iter_mut().find(|entry| entry["id"] == id)
    {
        provider["signIn"]["unavailableReason"] = json!(reason);
        provider["signIn"]["available"] = json!(false);
        provider["capabilities"]["add"] = json!(false);
        provider["capabilities"]["signInAgain"] = json!(false);
    }
}

pub(super) fn codex_cli() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("CCS_CODEX_PATH").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        let path = if let Ok(relative) = path.strip_prefix("~") {
            std::env::home_dir()?.join(relative)
        } else {
            path
        };
        return read::stat(&path).ok()?.file.then_some(path);
    }
    let path = crate::antigravity_probes::bounded_run("which", &["codex"], 5000)?;
    let path = PathBuf::from(path.lines().next()?);
    read::stat(&path).ok()?.file.then_some(path)
}

pub(super) fn antigravity_preflight(home: Option<&Path>) -> Option<&'static str> {
    use crate::antigravity_probes::{bounded_run, current_uid, native_binary_problem_public};
    if !cfg!(target_os = "linux") {
        return Some("preflight_failed");
    }
    if home.is_none_or(|home| native_binary_problem_public(home, current_uid())) {
        return Some("tool_missing");
    }
    let tools = ["/usr/bin/bwrap", "/usr/bin/dbus-run-session"];
    if tools
        .iter()
        .any(|tool| !read::stat(Path::new(tool)).is_ok_and(|stat| stat.file))
    {
        return Some("preflight_failed");
    }
    bounded_run(
        tools[0],
        &[
            "--die-with-parent",
            "--unshare-user",
            "--unshare-pid",
            "--unshare-ipc",
            "--unshare-uts",
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/tmp",
            "--proc",
            "/proc",
            "--dev",
            "/dev",
            "/bin/true",
        ],
        10_000,
    )
    .is_none()
    .then_some("preflight_failed")
}

fn codex_refusal(
    root: &Path,
    home: Option<&Path>,
    registry: Option<&codex_registry::Registry>,
    name: &str,
    is_active: bool,
    running: bool,
) -> Option<&'static str> {
    let home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| home.map(|home| home.join(".codex")));
    if is_active
        || home.as_ref().is_some_and(|home| {
            codex_identity::saved_login_is_live(
                &home.join("auth.json"),
                &root.join("codex-instances").join(name).join("auth.json"),
            )
        })
    {
        return Some("account_active");
    }
    let Some(registry) = registry else {
        return Some("unavailable");
    };
    if registry.default.as_deref() == Some(name) && registry.profiles.len() > 1 {
        return Some("account_default");
    }
    if registry.profiles.len() <= 1 {
        return Some("last_account");
    }
    if home.is_some_and(|home| {
        read::stat(&home.join(".ccs-activation.lock"))
            .is_ok_and(|stat| now() as f64 - stat.mtime_ms <= 120_000.0)
    }) {
        return Some("activation_running");
    }
    running.then_some("signin_running")
}
