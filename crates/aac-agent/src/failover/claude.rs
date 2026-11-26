//! U1's Claude adapter on U2's failover dispatcher (`failover_core.py` Claude rows): Claude
//! Code logins on every platform, the default Keychain login on macOS, and the Claude Desktop
//! profiles on Windows. Collection verifies the live profile names the saved account and
//! organization before usage is read, and never publishes a token or a provider body.
use super::{Adapter, Context, *};
use crate::claude_usage::{
    desktop,
    http::{self, Client, HttpError},
    normalize,
};
use std::time::Duration;

const SAFE_NAME: &str = r"\A[A-Za-z0-9][A-Za-z0-9_-]{0,63}\z";
const MAX_SOURCES: usize = 64;

pub(crate) struct Claude;
impl Adapter for Claude {
    fn inventory(&mut self, c: &Context) -> Result<Value, ()> {
        let result = proof::scope(|| inventory(c));
        if result == proof::fallback() {
            Err(())
        } else {
            Ok(result)
        }
    }
    async fn collect(&mut self, candidate: &Value, c: &Context) -> Value {
        collect(candidate, c).await
    }
}

fn uuid_lower(v: &Value) -> Option<String> {
    v.as_str()
        .filter(|s| desktop::is_uuid(s))
        .map(str::to_lowercase)
}
fn number_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9_007_199_254_740_992.0 {
        json!(n as i64)
    } else {
        json!(n)
    }
}

/// Python's `_candidate("claude", ...)`.
#[allow(clippy::too_many_arguments)]
fn candidate(
    storage: &Path,
    selectors: Vec<String>,
    binding: Value,
    revision: Value,
    email: Value,
    plan: Option<String>,
    private: Value,
) -> Value {
    let mut item = json!({"locator":digest(&json!(["claude", storage.to_string_lossy().replace('\\', "/")])),
        "selectors":selectors,"identity":{"kind":"claude-account-org-v1","key":digest(&json!(["claude", binding]))},
        "revision":digest(&revision),"email":email,"plan":plan});
    if let (Some(obj), Some(extra)) = (item.as_object_mut(), private.as_object()) {
        obj.extend(extra.clone());
    }
    item
}

/// The default Claude Code login in the macOS Keychain (`_mac_default_keychain`).
fn mac_keychain(home: &Path) -> Vec<Value> {
    if !cfg!(target_os = "macos") {
        return vec![];
    }
    let root = home.join(".claude");
    let metadata = read(&home.join(".claude.json"), CORE_CAP, true).unwrap_or(Value::Null);
    let account = &metadata["oauthAccount"];
    let (Some(account_id), Some(org_id), Some(address)) = (
        uuid(&account["accountUuid"]),
        uuid(&account["organizationUuid"]),
        email(&account["emailAddress"], false),
    ) else {
        return vec![];
    };
    let Ok(raw) = crate::claude_usage::keychain::read("Claude Code-credentials", "", false) else {
        return vec![];
    };
    let Some(credentials) = proof::json(&raw, desktop::MAX_BYTES, false) else {
        return vec![];
    };
    let oauth = &credentials["claudeAiOauth"];
    let Some(token) = secret(&oauth["accessToken"]) else {
        return vec![];
    };
    vec![candidate(
        &root.join("keychain-default"),
        vec!["discover".into(), "claude-profile:default".into()],
        json!([account_id, org_id]),
        json!([oauth, account_id, org_id, address]),
        json!(address),
        desktop_text(&oauth["subscriptionType"], 64),
        json!({"_token":token,"_account":account_id,"_organization":org_id,"_mode":"cli",
            "_expires":oauth["expiresAt"].as_f64().filter(|n| n.is_finite()).map(number_value)}),
    )]
}

#[derive(Default)]
struct Desktop {
    rows: Vec<Value>,
    sign_in_needed: Vec<String>,
    incomplete: Vec<String>,
}
fn path_key(p: &Path) -> String {
    let text = files::lexical(p).to_string_lossy().replace('/', "\\");
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}
fn regular(path: &Path) -> bool {
    desktop::regular_file(path)
}
/// `_configured_windows_profile_root`: a local folder, no link or share, files optional.
fn configured_root(root: &Path, require_files: bool) -> Result<Option<PathBuf>, ()> {
    let text = root.to_string_lossy();
    if !root.is_absolute() || text.starts_with("\\\\") || text.starts_with("//") {
        return Err(());
    }
    match desktop::canonical_profile_directory(root) {
        Err(_) => Err(()),
        Ok(p) if p == root && std::fs::symlink_metadata(root).is_err() => Ok(None),
        Ok(p) => {
            if require_files {
                for name in ["config.json", "Local State"] {
                    match std::fs::symlink_metadata(root.join(name)) {
                        Ok(_) if regular(&root.join(name)) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                        _ => return Err(()),
                    }
                }
            }
            Ok(Some(p))
        }
    }
}
/// `_claude_desktop_candidates`: Windows Desktop profiles from discovery and the manifest.
fn desktop_candidates(home: &Path, ccs: &Path, claude_profile: Option<&Value>) -> Desktop {
    let mut out = Desktop::default();
    if !cfg!(windows) {
        return out;
    }
    let families = ["claude-profile:*", "claude-desktop-profile:*"];
    let manifest = read(&ccs.join("claude-desktop-profiles.json"), CORE_CAP, true);
    let manifest = manifest.unwrap_or(Value::Null);
    let mut profiles: Vec<Value> = if manifest["version"] == 1 {
        manifest["profiles"].as_array().cloned().unwrap_or_default()
    } else {
        vec![]
    };
    if let Some(selected) = claude_profile {
        let mut windows =
            json!({"isDefault": selected.get("isDefault").cloned().unwrap_or(json!(false))});
        if let Some(path) = selected.get("profilePath") {
            windows["profilePath"] = path.clone();
        }
        let entry = json!({"id":selected["id"],"email":selected["email"],"windows":windows});
        profiles.retain(|p| p.get("id") != Some(&selected["id"]));
        profiles.insert(0, entry);
    }
    // (root, selectors, address, identifier), in Python's dict order.
    let mut roots: Vec<(PathBuf, Vec<String>, Option<String>, String)> = vec![];
    let put = |roots: &mut Vec<(PathBuf, Vec<String>, Option<String>, String)>,
               entry: (PathBuf, Vec<String>, Option<String>, String)| {
        let key = path_key(&entry.0);
        if let Some(slot) = roots.iter_mut().find(|r| path_key(&r.0) == key) {
            *slot = entry;
        } else {
            roots.push(entry);
        }
    };
    let defaults: Vec<String> = [
        "discover",
        "claude-profile:default",
        "claude-desktop-profile:default",
    ]
    .map(String::from)
    .to_vec();
    for root in [
        desktop::windows_profile_path("default", home, true),
        home.join("AppData/Roaming/Claude"),
    ] {
        put(&mut roots, (root, defaults.clone(), None, "default".into()));
    }
    for root in children(&home.join("AppData/Roaming"), true, 1024) {
        let name = root
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if let Some(id) = name
            .strip_prefix("Claude-")
            .filter(|id| matches(SAFE_NAME, id))
        {
            let id = id.to_owned();
            put(
                &mut roots,
                (
                    root,
                    vec![
                        format!("claude-profile:{id}"),
                        format!("claude-desktop-profile:{id}"),
                    ],
                    None,
                    id,
                ),
            );
        }
    }
    let mut configured: Vec<String> = vec![];
    let mut assigned: Vec<String> = vec![];
    for profile in profiles.iter().take(MAX_SOURCES) {
        let (Some(id), Some(address), Some(launcher)) = (
            profile["id"].as_str().filter(|id| matches(SAFE_NAME, id)),
            email(&profile["email"], false),
            profile["windows"].as_object().filter(|o| !o.is_empty()),
        ) else {
            continue;
        };
        let is_default = launcher.get("isDefault") == Some(&json!(true));
        let supplied = launcher.get("profilePath");
        let mut root = match supplied {
            None => desktop::windows_profile_path(id, home, is_default),
            Some(Value::String(s)) if s.chars().count() <= 4096 && Path::new(s).is_absolute() => {
                desktop::resolve_windows_profile_root(Path::new(s), home)
            }
            Some(_) => continue,
        };
        root = files::lexical(&root);
        let mut bound = vec![
            format!("claude-profile:{id}"),
            format!("claude-desktop-profile:{id}"),
        ];
        let named_default = root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().to_lowercase() == "claude");
        if is_default || named_default {
            bound.extend([
                "claude-profile:default".into(),
                "claude-desktop-profile:default".into(),
            ]);
        }
        if supplied.is_some() {
            match configured_root(&root, false) {
                Err(()) => out.incomplete.extend(bound.clone()),
                Ok(Some(canonical)) => {
                    let moved = desktop::resolve_windows_profile_root(&canonical, home);
                    root = if moved != canonical {
                        match configured_root(&moved, false) {
                            Ok(Some(current)) => current,
                            Err(()) => {
                                out.incomplete.extend(bound.clone());
                                moved
                            }
                            Ok(None) => moved,
                        }
                    } else {
                        moved
                    };
                }
                Ok(None) => {}
            }
        }
        let key = path_key(&root);
        if assigned.contains(&key) {
            continue;
        }
        assigned.push(key.clone());
        if supplied.is_some() {
            configured.push(key.clone());
        }
        let native = roots
            .iter()
            .find(|r| path_key(&r.0) == key)
            .map(|r| r.1.clone());
        let mut selectors = native.unwrap_or_default();
        selectors.extend([
            format!("claude-profile:{id}"),
            format!("claude-desktop-profile:{id}"),
        ]);
        let named_default = root
            .file_name()
            .is_some_and(|n| n.to_string_lossy().to_lowercase() == "claude");
        if is_default || named_default {
            selectors.extend([
                "claude-profile:default".into(),
                "claude-desktop-profile:default".into(),
            ]);
        }
        selectors.sort();
        selectors.dedup();
        put(&mut roots, (root, selectors, Some(address), id.to_owned()));
    }
    let _ = families;
    for (root, selectors, address, id) in roots.iter().take(MAX_SOURCES) {
        desktop_root(
            home,
            root,
            selectors,
            address.as_deref(),
            id,
            &configured,
            &mut out,
        );
    }
    for (root, selectors, _, _) in roots.iter().take(MAX_SOURCES) {
        if path_key(&desktop::resolve_windows_profile_root(root, home)) != path_key(root) {
            let key = path_key(root);
            out.rows.retain(|r| {
                r["_profile_root"].as_str().map(|p| path_key(Path::new(p))) != Some(key.clone())
            });
            out.sign_in_needed.retain(|s| !selectors.contains(s));
        }
    }
    out
}
fn desktop_root(
    home: &Path,
    root: &Path,
    selectors: &[String],
    address: Option<&str>,
    id: &str,
    configured: &[String],
    out: &mut Desktop,
) {
    if path_key(&desktop::resolve_windows_profile_root(root, home)) != path_key(root) {
        return;
    }
    let root = if configured.contains(&path_key(root)) {
        match configured_root(root, true) {
            Ok(Some(r)) => r,
            Ok(None) => return,
            Err(()) => {
                out.incomplete.extend(selectors.iter().cloned());
                return;
            }
        }
    } else {
        if !regular(&root.join("config.json")) || !regular(&root.join("Local State")) {
            return;
        }
        root.to_path_buf()
    };
    let needed = |out: &mut Desktop| {
        out.sign_in_needed
            .extend(selectors.iter().filter(|s| *s != "discover").cloned())
    };
    match desktop::decrypt_cache(&root) {
        Ok((account, cache)) => {
            let entries = desktop::eligible_entries(&account, &cache, true, None, 0.0);
            if path_key(&desktop::resolve_windows_profile_root(&root, home)) != path_key(&root) {
                return;
            }
            if entries.is_empty() {
                needed(out);
            }
            for entry in entries {
                let (Some(account_id), Some(org_id)) = (
                    uuid_lower(&json!(account)),
                    uuid_lower(&json!(entry.identities[2])),
                ) else {
                    continue;
                };
                let identities = json!(entry.identities);
                let expires = number_value(entry.expires);
                out.rows.push(candidate(
                    &root.join(format!("oauth-{}", digest(&identities))),
                    selectors.to_vec(),
                    json!([account_id, org_id]),
                    json!([entry.token, identities, address, expires]),
                    json!(address),
                    None,
                    json!({"_token":entry.token,"_account":account_id,"_organization":org_id,"_profile":id,
                        "_mode":"desktop","_expires":expires,"_profile_root":root.to_string_lossy()}),
                ));
            }
        }
        Err(e) if e.status == "needs_sign_in" => needed(out),
        Err(_) => out.incomplete.extend(selectors.iter().cloned()),
    }
}

fn inventory(c: &Context) -> Value {
    if !proof::ascii(&c.home.to_string_lossy()) || !proof::ascii(&c.ccs.to_string_lossy()) {
        return proof::fallback();
    }
    let desktop = desktop_candidates(&c.home, &c.ccs, c.claude_profile.as_ref());
    let mut rows: Vec<Value> = core_candidates("claude", &c.home, &c.ccs)
        .into_iter()
        .map(|mut row| {
            row["_mode"] = json!("cli");
            row
        })
        .collect();
    rows.extend(mac_keychain(&c.home));
    rows.extend(desktop.rows);
    let mut unique: Vec<Value> = vec![];
    for row in rows {
        if let Some(old) = unique.iter_mut().find(|v| v["locator"] == row["locator"]) {
            let mut selectors: Vec<String> = old["selectors"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(row["selectors"].as_array().into_iter().flatten())
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            selectors.sort();
            selectors.dedup();
            old["selectors"] = json!(selectors);
        } else if unique.len() < MAX_SOURCES {
            unique.push(row);
        }
    }
    let mut needed = desktop.sign_in_needed;
    needed.sort();
    needed.dedup();
    let mut incomplete = desktop.incomplete;
    incomplete.sort();
    incomplete.dedup();
    let mut out = json!({"candidates":unique});
    if !needed.is_empty() {
        out["signInNeededSelectors"] = json!(needed);
    }
    if !incomplete.is_empty() {
        out["incompleteSelectors"] = json!(incomplete);
    }
    out
}

/// `failover_core.Client` statuses for Claude.
fn failure_code(e: HttpError) -> (&'static str, Option<i64>) {
    match e.status {
        _ if e.invalid => ("invalid_response", None),
        Some(429) => ("rate_limited", e.retry_after),
        Some(401) => ("needs_sign_in", None),
        _ => ("unavailable", None),
    }
}
struct Failure(&'static str, Option<i64>);

async fn collect(candidate: &Value, c: &Context) -> Value {
    let mut observed: Vec<Value> = vec![];
    let result = collect_inner(candidate, c, &mut observed).await;
    let mut out = match result {
        Ok(Collected { sample, hold: None }) => json!({"sample":sample,"failureCode":null}),
        // Python's cooldown: the sample stands, and the provider's 429 still holds the account.
        Ok(Collected {
            sample,
            hold: Some(retry),
        }) => {
            let mut v = json!({"sample":sample,"failureCode":"rate_limited"});
            if let Some(r) = retry {
                v["retryAfterSeconds"] = json!(r);
            }
            v
        }
        Err(Failure(code, retry)) => {
            let mut v = json!({"sample":null,"failureCode":code});
            if let Some(r) = retry {
                v["retryAfterSeconds"] = json!(r);
            }
            v
        }
    };
    if !observed.is_empty() {
        out["claudeHttp"] = json!(observed);
    }
    out
}
async fn get(
    client: &Client,
    route: &str,
    stage: &str,
    token: &str,
    observed: &mut Vec<Value>,
) -> Result<Value, Failure> {
    let result = client.get(route, token).await;
    let (status, retry) = match &result {
        Ok(_) => (Some(200), None),
        Err(e) => (e.status, e.retry_after),
    };
    if observed.len() < 5 {
        observed.push(json!({"stage":stage,"status":status,"retryAfterSeconds":retry}));
    }
    result.map_err(|e| {
        let (code, retry) = failure_code(e);
        Failure(code, retry)
    })
}
async fn collect_inner(
    candidate: &Value,
    c: &Context,
    observed: &mut Vec<Value>,
) -> Result<Collected, Failure> {
    if c.provider != "claude" {
        return Err(Failure("unavailable", None));
    }
    let home = &c.home;
    let root = candidate["_profile_root"].as_str().map(PathBuf::from);
    let verify_root = || match &root {
        Some(r) if path_key(&desktop::resolve_windows_profile_root(r, home)) != path_key(r) => {
            Err(Failure("source_changed", None))
        }
        _ => Ok(()),
    };
    let now = crate::claude_usage::now_ms() as f64;
    if candidate["_expires"]
        .as_f64()
        .is_some_and(|e| e <= now + 30000.0)
    {
        return Err(Failure("needs_sign_in", None));
    }
    verify_root()?;
    let token = candidate["_token"].as_str().unwrap_or("");
    let client = Client::new(Duration::from_secs(12), Duration::from_secs(5))
        .ok_or(Failure("unavailable", None))?;
    let profile = get(&client, http::PROFILE, "oauth_profile", token, observed).await?;
    let (account, org) = (&profile["account"], &profile["organization"]);
    let account = if account.is_object() {
        account.clone()
    } else {
        json!({})
    };
    let org = if org.is_object() {
        org.clone()
    } else {
        json!({})
    };
    let live_email = email(&account["email"], false);
    if live_email.is_none()
        || (!candidate["email"].is_null() && live_email.as_deref() != candidate["email"].as_str())
        || uuid_lower(&account["uuid"]).as_deref() != candidate["_account"].as_str()
        || uuid_lower(&org["uuid"]).as_deref() != candidate["_organization"].as_str()
    {
        return Err(Failure("identity_mismatch", None));
    }
    verify_root()?;
    let usage = get(&client, http::USAGE, "oauth_usage", token, observed).await?;
    verify_root()?;
    let mut windows = normalize::normalize_windows(&usage);
    windows.extend(normalize::normalize_reset_credits(
        &usage["cedar_ember"],
        now / 1000.0,
    ));
    windows.extend(normalize::normalize_session_reset_allowance(
        &usage["juniper_tide"],
    ));
    if windows.is_empty() {
        return Err(Failure("invalid_response", None));
    }
    let plan = normalize::claude_plan(&account, &org, &candidate["plan"]);
    let plan_class = normalize::claude_plan_class(&account, &org, plan.as_deref());
    // The web-only extras come from the same OAuth sign-in: the usage answer carries the reset
    // grants and the weekly session reset, and the prepaid balance has its own OAuth route. An
    // unread balance stays unavailable, never zero; it never fails the quota it rides on.
    let reset = if normalize::has_reset_grants(&windows) {
        "ok"
    } else {
        "unavailable"
    };
    let organization = candidate["_organization"].as_str().unwrap_or("");
    let (prepaid, hold) = prepaid_windows(&client, organization, token, observed).await;
    let prepaid_status = if prepaid.is_some() {
        "ok"
    } else {
        "unavailable"
    };
    for window in prepaid.unwrap_or_default() {
        if !windows.iter().any(|w| w["key"] == window["key"]) {
            windows.push(window);
        }
    }
    verify_root()?;
    let stamp = normalize::iso_ms(crate::claude_usage::now_ms());
    let platform_label = match c.platform {
        "ubuntu" => "Ubuntu",
        "mac" => "Mac",
        _ => "Windows",
    };
    let mut sample = json!({"id":format!("native:claude:{}", c.platform),"provider":"claude",
        "providerLabel":"Claude","label":"Claude","email":live_email,"plan":plan,"platform":c.platform,
        "source":format!("Claude verified quota on {platform_label}"),"status":"ok","message":null,
        "fetchedAt":stamp,"sampledAt":stamp,"isActive":false,"windows":windows,
        "capabilities":{"codexProfile":null,"claudeProfileId":null,"claudePlatforms":[]}});
    if let Some(class) = plan_class {
        sample["planClass"] = json!(class);
    }
    sample["optionalExtras"] = json!({"resetCredits":reset,"prepaidBalance":prepaid_status});
    // Ties retained extras to this sign-in and its stored revision (`optionalExtrasContext`).
    sample["optionalExtrasContext"] = json!(aac_core::sha256_hex(
        json!([
            "claude-optional-oauth-v1",
            candidate["identity"],
            candidate["revision"]
        ])
        .to_string()
        .as_bytes()
    ));
    Ok(Collected { sample, hold })
}
/// `optional_web_windows`' balance half over OAuth: `Some` windows once the balance was read
/// (possibly none, as Python), and the provider's hold when it answered 429.
async fn prepaid_windows(
    client: &Client,
    organization: &str,
    token: &str,
    observed: &mut Vec<Value>,
) -> (Option<Vec<Value>>, Option<Option<i64>>) {
    let result = client.prepaid(organization, token).await;
    let (status, retry) = match &result {
        Ok(_) => (Some(200), None),
        Err(e) => (e.status, e.retry_after),
    };
    if observed.len() < 5 {
        observed.push(json!({"stage":"optional_web","status":status,"retryAfterSeconds":retry}));
    }
    match result {
        Ok(value) => (Some(normalize::normalize_prepaid(&value)), None),
        Err(e) if e.status == Some(429) => (None, Some(e.retry_after)),
        Err(_) => (None, None),
    }
}
/// A verified sample, plus the provider's hold when an optional read was rate limited.
struct Collected {
    sample: Value,
    hold: Option<Option<i64>>,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn write(path: &Path, value: Value) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
    }
    fn fixture() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        write(
            &home.join(".claude.json"),
            json!({"oauthAccount":{"accountUuid":"11111111-1111-1111-1111-111111111111",
                "organizationUuid":"33333333-3333-3333-3333-333333333333","emailAddress":"user@example.test"}}),
        );
        write(
            &home.join(".claude/.credentials.json"),
            json!({"claudeAiOauth":{"accessToken":"synthetic-token","expiresAt":1,"subscriptionType":"max"}}),
        );
        dir
    }

    #[test]
    fn claude_inventory_is_native_and_never_publishes_the_token() {
        let dir = fixture();
        let home = dir.path().to_string_lossy().to_string();
        let out =
            run(&json!({"version":1,"action":"inventory","provider":"claude","homeDir":home}))
                .expect("inventory");
        assert_ne!(out, proof::fallback());
        let candidates = out["candidates"].as_array().expect("candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0]["email"], "user@example.test");
        assert!(!out.to_string().contains("synthetic-token"));
    }

    #[test]
    fn expired_cli_login_needs_sign_in_without_provider_io() {
        let dir = fixture();
        let home = dir.path().to_string_lossy().to_string();
        let inventory =
            run(&json!({"version":1,"action":"inventory","provider":"claude","homeDir":home}))
                .expect("inventory");
        let wanted = inventory["candidates"][0].clone();
        let out = run(&json!({"version":1,"action":"collect","provider":"claude","homeDir":home,"candidate":wanted}))
            .expect("collect");
        assert_ne!(out, proof::fallback());
        assert_eq!(out["failureCode"], "needs_sign_in");
        assert!(
            out.get("claudeHttp").is_none(),
            "no provider request was made"
        );
    }

    /// A local server answering each request in order with its status line
    /// (`429 Too Many Requests\r\nRetry-After: 7`); returns its origin and the requests.
    fn server_with(
        responses: Vec<(&'static str, String)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            // Give up after a few seconds so a run that never connects cannot hang the test.
            listener.set_nonblocking(true).unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            let mut seen = vec![];
            for (status, body) in responses {
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(_) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(_) => return seen,
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut input = vec![0; 8192];
                let size = stream.read(&mut input).unwrap();
                seen.push(String::from_utf8_lossy(&input[..size]).into_owned());
                let reply = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(reply.as_bytes()).unwrap();
            }
            seen
        });
        (format!("http://{addr}"), worker)
    }
    fn live_fixture() -> tempfile::TempDir {
        let dir = fixture();
        write(
            &dir.path().join(".claude/.credentials.json"),
            json!({"claudeAiOauth":{"accessToken":"synthetic-token","expiresAt":9_000_000_000_000_i64,"subscriptionType":"pro"}}),
        );
        dir
    }
    fn collect_against(home: &Path, profile: Value) -> (Value, Vec<String>) {
        let prepaid = json!({"amount":2500,"currency":"USD","next_expires_at":null});
        collect_with(home, profile, ("200 OK", prepaid.to_string()))
    }
    /// One collect: profile, usage (with the weekly session reset), then the prepaid answer.
    fn collect_with(
        home: &Path,
        profile: Value,
        prepaid: (&'static str, String),
    ) -> (Value, Vec<String>) {
        let usage = json!({"five_hour":{"utilization":12,"resets_at":"2026-11-26T20:00:00Z"},
            "cedar_ember":{"eligible":true,"grants":[{"resets_left":1,"resets_total":1,"paused":false}]},
            "juniper_tide":{"eligible":true,"available":true,"resets_per_week":1,"next_available_at":null}});
        let (origin, worker) = server_with(vec![
            ("200 OK", profile.to_string()),
            ("200 OK", usage.to_string()),
            prepaid,
        ]);
        crate::claude_usage::http::TEST_ORIGIN.with(|o| *o.borrow_mut() = Some(origin));
        let home = home.to_string_lossy().to_string();
        let inventory =
            run(&json!({"version":1,"action":"inventory","provider":"claude","homeDir":home}))
                .unwrap();
        let wanted = inventory["candidates"][0].clone();
        let out = run(&json!({"version":1,"action":"collect","provider":"claude","homeDir":home,"candidate":wanted})).unwrap();
        crate::claude_usage::http::TEST_ORIGIN.with(|o| *o.borrow_mut() = None);
        (out, worker.join().unwrap())
    }

    #[test]
    fn verified_profile_collects_native_quota_and_plan_class() {
        let dir = live_fixture();
        let profile = json!({"account":{"uuid":"11111111-1111-1111-1111-111111111111","email":"user@example.test","has_claude_max":true},
            "organization":{"uuid":"33333333-3333-3333-3333-333333333333","rate_limit_tier":"default_claude_max_20x"}});
        let (out, requests) = collect_against(dir.path(), profile);
        assert_eq!(out["failureCode"], Value::Null, "{out}");
        let sample = &out["sample"];
        let keys: Vec<&str> = sample["windows"]
            .as_array()
            .unwrap()
            .iter()
            .map(|w| w["key"].as_str().unwrap())
            .collect();
        assert_eq!(
            keys,
            [
                "five_hour",
                "reset_credits_available",
                "reset_credit_available_grant_1",
                "session_reset_allowance",
                "prepaid_balance"
            ]
        );
        let prepaid = &sample["windows"].as_array().unwrap()[4];
        assert_eq!(prepaid["remaining"], json!(25.0), "{prepaid}");
        assert_eq!(prepaid["unit"], "USD", "{prepaid}");
        assert_eq!(sample["planClass"], "max");
        assert_eq!(sample["optionalExtras"]["resetCredits"], "ok");
        assert_eq!(sample["optionalExtras"]["prepaidBalance"], "ok");
        assert!(
            sample["optionalExtrasContext"]
                .as_str()
                .is_some_and(|c| c.len() == 64)
        );
        let stages: Vec<_> = out["claudeHttp"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["stage"].as_str().unwrap())
            .collect();
        assert_eq!(stages, ["oauth_profile", "oauth_usage", "optional_web"]);
        assert!(!out.to_string().contains("synthetic-token"));
        assert!(requests[0].starts_with("GET /api/oauth/profile "));
        assert!(requests[1].starts_with("GET /api/oauth/usage?cedar_ember=1&at_wall=1 "));
        assert!(requests[2].starts_with(
            "GET /api/oauth/organizations/33333333-3333-3333-3333-333333333333/prepaid/credits "
        ));
        assert!(
            requests[2]
                .to_ascii_lowercase()
                .contains("x-organization-uuid: 33333333-3333-3333-3333-333333333333")
        );
        assert!(
            requests[0].contains("authorization: Bearer synthetic-token")
                || requests[0].contains("Authorization: Bearer synthetic-token")
        );
    }

    fn max_profile() -> Value {
        json!({"account":{"uuid":"11111111-1111-1111-1111-111111111111","email":"user@example.test","has_claude_max":true},
            "organization":{"uuid":"33333333-3333-3333-3333-333333333333","rate_limit_tier":"default_claude_max_20x"}})
    }

    #[test]
    fn an_unread_prepaid_balance_stays_unavailable_and_keeps_the_quota() {
        let dir = live_fixture();
        let (out, _) = collect_with(
            dir.path(),
            max_profile(),
            ("500 Internal Server Error", "{}".into()),
        );
        assert_eq!(out["failureCode"], Value::Null, "{out}");
        let sample = &out["sample"];
        assert_eq!(sample["optionalExtras"]["prepaidBalance"], "unavailable");
        assert!(
            !sample["windows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["key"] == "prepaid_balance"),
            "never a zero balance"
        );
        assert!(
            sample["windows"]
                .as_array()
                .unwrap()
                .iter()
                .any(|w| w["key"] == "five_hour")
        );
    }

    #[test]
    fn a_rate_limited_prepaid_read_holds_the_account_but_keeps_the_sample() {
        let dir = live_fixture();
        let (out, _) = collect_with(
            dir.path(),
            max_profile(),
            ("429 Too Many Requests\r\nRetry-After: 7", "{}".into()),
        );
        assert_eq!(out["failureCode"], "rate_limited", "{out}");
        assert_eq!(out["retryAfterSeconds"], 7);
        assert_eq!(
            out["sample"]["optionalExtras"]["prepaidBalance"],
            "unavailable"
        );
        assert_eq!(out["sample"]["optionalExtras"]["resetCredits"], "ok");
    }

    #[test]
    fn another_organization_is_an_identity_mismatch_before_usage() {
        let dir = live_fixture();
        let profile = json!({"account":{"uuid":"11111111-1111-1111-1111-111111111111","email":"user@example.test"},
            "organization":{"uuid":"44444444-4444-4444-4444-444444444444"}});
        let (out, _) = collect_against(dir.path(), profile);
        assert_eq!(out["failureCode"], "identity_mismatch");
        assert_eq!(
            out["claudeHttp"].as_array().unwrap().len(),
            1,
            "usage is never read"
        );
    }
}
