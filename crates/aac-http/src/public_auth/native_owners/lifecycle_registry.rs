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
    let Some(usage) = &native.usage else {
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
        if row["provider"].as_str() == Some("opencode-go")
            && let Some(parent_id) = crate::account_usage::producer::recheck::discovered_key_id(id)
            && entries.iter().any(|source| {
                source.id == parent_id
                    && source.provider == "opencode-go"
                    && matches!(source.credential, sources::Credential::Discover)
            })
        {
            return json!({"kind":"stored-key","row":row});
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
    let finder = if cfg!(windows) { "where.exe" } else { "which" };
    let paths = crate::antigravity_probes::bounded_run(finder, &["codex"], 5000)?;
    let mut paths = paths.lines().map(PathBuf::from);
    #[cfg(windows)]
    {
        let command = paths.clone().find(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| {
                    ["exe", "cmd", "bat", "ps1"]
                        .iter()
                        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
                })
        });
        if let Some(path) = command {
            if path
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("ps1"))
            {
                for extension in ["cmd", "bat", "exe"] {
                    let wrapper = path.with_extension(extension);
                    if read::stat(&wrapper).is_ok_and(|stat| stat.file) {
                        return Some(wrapper);
                    }
                }
            }
            return read::stat(&path).ok()?.file.then_some(path);
        }
    }
    let path = paths.next()?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::account_usage::{Desktop, dashboard};
    use std::fs;
    use std::io::Write;
    use std::process::{Command, Stdio};

    fn set_mode(path: &Path, mode: u32) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("fixture mode");
        }
        #[cfg(not(unix))]
        let _ = (path, mode);
    }

    fn opencode_row(id: &str, label: &str) -> Value {
        json!({
            "id": id,
            "provider": "opencode-go",
            "providerLabel": "OpenCode Go",
            "label": label,
            "email": null,
            "plan": "Go",
            "platform": "ubuntu",
            "source": "Verified quota on Ubuntu",
            "status": "ok",
            "message": null,
            "fetchedAt": "2026-11-21T17:00:00.000Z",
            "sampledAt": "2026-11-21T17:00:00.000Z",
            "isActive": false,
            "windows": [{"key":"weekly","label":"Weekly","usedPercent":33.0,"remainingPercent":67.0,"resetAt":null,"windowMinutes":null,"used":null,"limit":null,"unit":null}],
            "capabilities": {"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]}
        })
    }

    /// Astra Medium 2: the native registry response gives the unnamed OpenCode Go digest row
    /// an explicit stored-key descriptor with per-key Re-check (Replace and Remove disabled),
    /// with no bare discover parent row, and the real Accounts view enables Re-check on it
    /// with no destructive action enabled.
    #[test]
    fn native_registry_gives_unnamed_stored_key_recheck_through_accounts_view() {
        let dir = tempfile::tempdir().expect("fixture root");
        let root = fs::canonicalize(dir.path()).expect("canonical root");
        set_mode(&root, 0o700);
        let reg_file = root.join("account-usage-accounts.json");
        fs::write(
            &reg_file,
            serde_json::to_vec(&json!({
                "version": 2,
                "accounts": [
                    {
                        "id": "opencode-go:usage", "provider": "opencode-go", "platform": "ubuntu",
                        "sshHost": null, "label": null, "credential": {"kind": "discover"},
                        "createdAt": null, "createdBy": "migration"
                    },
                    {
                        "id": "opencode-go:acct:52f3251b", "provider": "opencode-go", "platform": "ubuntu",
                        "sshHost": null, "label": "Account 2",
                        "credential": {"kind": "aac-key", "keyId": "52f3251b"},
                        "createdAt": null, "createdBy": "dashboard"
                    },
                    {
                        "id": "opencode-go:acct:2e034ec2", "provider": "opencode-go", "platform": "ubuntu",
                        "sshHost": null, "label": "Account 3",
                        "credential": {"kind": "aac-key", "keyId": "2e034ec2"},
                        "createdAt": null, "createdBy": "dashboard"
                    }
                ]
            }))
            .expect("registry"),
        )
        .expect("write registry");
        set_mode(&reg_file, 0o600);

        let visibility = json!({
            "hiddenProviders": [],
            "hiddenAccountIds": [],
            "trayHiddenProviders": [],
            "trayHiddenAccountIds": []
        });
        let lifecycle = dashboard::Lifecycle {
            mounted: true,
            ..Default::default()
        };
        let sample = dashboard::build(
            Desktop::Mac,
            true,
            dashboard::Input {
                rows: vec![
                    opencode_row("opencode-go:usage:cccccccc", "Key cccccccc"),
                    opencode_row("opencode-go:acct:52f3251b", "Account 2"),
                    opencode_row("opencode-go:acct:2e034ec2", "Account 3"),
                ],
                visibility: &visibility,
                visibility_available: true,
                refresh_seconds: 60,
                lifecycle: &lifecycle,
                signins: None,
                server: None,
            },
        )
        .expect("dashboard sample");

        let response = listing(&root, sample.dashboard.clone(), Vec::new()).expect("listing");
        let accounts = response["accounts"].as_array().expect("accounts");
        let ids: Vec<&str> = accounts.iter().filter_map(|a| a["id"].as_str()).collect();
        assert_eq!(
            ids,
            [
                "opencode-go:usage:cccccccc",
                "opencode-go:acct:52f3251b",
                "opencode-go:acct:2e034ec2",
            ]
        );
        assert!(
            !ids.contains(&"opencode-go:usage"),
            "no bare discover parent row"
        );
        let unnamed = &accounts[0];
        assert_eq!(unnamed["credential"], json!({"kind": "stored-key"}));
        assert_eq!(
            unnamed["actions"],
            json!({
                "signInAgain": false,
                "replaceKey": false,
                "remove": false,
                "open": [],
                "recheck": true,
            })
        );
        assert_eq!(unnamed["removeRefusal"], Value::Null);
        assert_eq!(accounts[1]["credential"]["kind"], "aac-key");
        assert_eq!(accounts[1]["actions"]["replaceKey"], true);
        assert_eq!(accounts[1]["actions"]["remove"], true);

        // Pass the exact native dashboard + native registry response through the real Accounts view.
        let view_module = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web-dashboard/public/accounts-view.mjs")
            .canonicalize()
            .expect("accounts-view.mjs");
        let script = format!(
            "import {{ accountsViewModel }} from {:?}; \
             import fs from 'node:fs'; \
             const input = JSON.parse(fs.readFileSync(0, 'utf8')); \
             const vm = accountsViewModel(input.dashboard, {{ now: Date.parse('2026-11-21T17:01:00.000Z'), registry: input.registry }}); \
             const provider = [...vm.colA, ...vm.colB].find(p => p.id === 'opencode-go'); \
             process.stdout.write(JSON.stringify(provider.rows));",
            view_module.display().to_string()
        );
        let mut child = Command::new("node")
            .args(["--input-type=module", "-e", &script])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn node");
        let payload = serde_json::to_vec(&json!({
            "dashboard": sample.dashboard,
            "registry": response,
        }))
        .expect("payload");
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(&payload)
            .expect("write payload");
        let output = child.wait_with_output().expect("wait node");
        assert!(
            output.status.success(),
            "accounts-view.mjs failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let rows: Vec<Value> = serde_json::from_slice(&output.stdout).expect("parse rows");
        assert_eq!(rows.len(), 3);
        let per_key = rows
            .iter()
            .find(|r| r["id"] == "opencode-go:usage:cccccccc")
            .expect("unnamed key row");
        assert_eq!(per_key["src"], "Key stored on this computer");
        let acts: Vec<(&str, &str, bool)> = per_key["actions"]
            .as_array()
            .expect("actions")
            .iter()
            .map(|a| {
                (
                    a["act"].as_str().unwrap_or(""),
                    a["value"].as_str().unwrap_or(""),
                    a["enabled"].as_bool().unwrap_or(false),
                )
            })
            .collect();
        assert_eq!(
            acts,
            [
                ("recheck", "opencode-go:usage:cccccccc", true),
                ("remove", "opencode-go:usage:cccccccc", false),
            ]
        );
        assert!(
            !per_key["actions"][1]["tip"]
                .as_str()
                .unwrap_or("")
                .contains("other stored keys")
        );
        assert!(
            !per_key["actions"]
                .as_array()
                .expect("actions")
                .iter()
                .any(|a| a["enabled"] == true && a["act"] != "recheck"),
            "no destructive action enabled on unnamed stored key"
        );
        let acct2 = rows
            .iter()
            .find(|r| r["id"] == "opencode-go:acct:52f3251b")
            .expect("Account 2");
        assert_eq!(acct2["actions"][0]["act"], "replace-key");
        assert_eq!(acct2["actions"][0]["enabled"], true);
        assert_eq!(acct2["actions"][1]["act"], "remove");
        assert_eq!(acct2["actions"][1]["enabled"], true);

        // Without a discover parent in the registry, a digest row does not get stored-key actions.
        fs::write(
            &reg_file,
            serde_json::to_vec(&json!({
                "version": 2,
                "accounts": [
                    {
                        "id": "opencode-go:acct:52f3251b", "provider": "opencode-go", "platform": "ubuntu",
                        "sshHost": null, "label": "Account 2",
                        "credential": {"kind": "aac-key", "keyId": "52f3251b"},
                        "createdAt": null, "createdBy": "dashboard"
                    }
                ]
            }))
            .expect("registry without discover"),
        )
        .expect("write registry without discover");
        set_mode(&reg_file, 0o600);
        let orphan = listing(&root, sample.dashboard, Vec::new()).expect("orphan listing");
        let orphan_row = orphan["accounts"]
            .as_array()
            .expect("accounts")
            .iter()
            .find(|a| a["id"] == "opencode-go:usage:cccccccc")
            .expect("orphan row");
        assert_eq!(orphan_row["credential"], Value::Null);
        assert_eq!(orphan_row["actions"]["recheck"], false);
    }
}
