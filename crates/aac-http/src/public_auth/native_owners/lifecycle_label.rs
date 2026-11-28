//! Registry label edits using the shared source reader, lease and registry view.
use super::{NativeOwners, recheck};
use crate::{
    account_reads::public_account_id,
    account_usage::{producer, sources},
    lifecycle_policy::{Edit, edit_registry},
    public_auth::{DashboardAccess, Reply, error, lifecycle, response},
};
use aac_files::{
    codex_registry::{self, Field},
    profile_writes::CodexLease,
    read::{self, Ancestors, Mode, Read, ReadPolicy},
    registry::{self, Owners},
};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn route(method: &axum::http::Method, path: &str) -> Option<String> {
    use axum::http::Method;
    if !matches!(*method, Method::PATCH | Method::OPTIONS) {
        return None;
    }
    let raw = path.strip_prefix("/api/accounts/")?;
    if raw.is_empty() || raw.contains('/') || matches!(raw, "preferences" | "visibility") {
        return None;
    }
    let id = super::super::decode_component(raw)?;
    (*method == Method::PATCH || account_segment(&id)).then_some(id)
}

/// Every renameable account id has a provider prefix. The retired console wallet rows
/// (`plan-opencode-go-console-*`) are no longer produced, so they are not accepted here.
fn account_segment(id: &str) -> bool {
    id.contains(':')
}

pub(super) async fn handle(
    native: &NativeOwners,
    id: &str,
    request: axum::extract::Request,
) -> Reply {
    if request.method() == axum::http::Method::OPTIONS {
        return lifecycle::options("PATCH", "5");
    }
    if !account_segment(id) {
        return super::super::plain(404, "API endpoint was not found.");
    }
    let secure = request
        .extensions()
        .get::<DashboardAccess>()
        .is_some_and(|access| access.secure_transport);
    let body = match lifecycle::mutation_body(request).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let change = match route_change(&body) {
        Ok(change) => change,
        Err(reply) => return reply,
    };
    if !public_account_id(&json!(id)) {
        return error(400, "invalid_account", "That account id is not valid.");
    }
    let provider_named = ["claude:", "codex:", "antigravity:profile:"]
        .iter()
        .any(|prefix| id.starts_with(prefix));
    // Claude, Codex and Antigravity report the account's email themselves: only the name is
    // the owner's there, and a body without a name changes nothing.
    let label = match (&change, provider_named) {
        (Change { email: Some(_), .. }, true) => {
            return error(
                400,
                "email_from_provider",
                "This account's email comes from its provider, so it cannot be edited here.",
            );
        }
        (Change { label: None, .. }, true) => return lifecycle::invalid_action_body(),
        (Change { label, .. }, _) => label.clone().flatten(),
    };

    let root = native.agents.root().to_owned();
    if let Some(profile) = id.strip_prefix("claude:") {
        if profile.is_empty() || profile.contains(':') {
            return error(400, "invalid_account", "That account id is not valid.");
        }
        let profile = profile.to_owned();
        let label = label.clone();
        let native = native.clone();
        let root = root.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let account = commit_claude(&root, &native.owners, &profile, label)?;
            native.changed();
            Ok::<_, SaveError>(account)
        })
        .await;
        return match saved {
            Ok(Ok(account)) => super::no_store(response(200, json!({"account": account}))),
            Ok(Err(SaveError::Missing)) => unknown_account(),
            _ => error(500, "write_failed", "The change could not be saved safely."),
        };
    }
    if let Some(name) = id.strip_prefix("codex:") {
        if name.is_empty() || name.contains(':') {
            return error(400, "invalid_account", "That account id is not valid.");
        }
        let name = name.to_owned();
        let label = label.clone();
        let native = native.clone();
        let root = root.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let account = commit_codex(&root, &name, label)?;
            native.changed();
            Ok::<_, SaveError>(account)
        })
        .await;
        return match saved {
            Ok(Ok(account)) => super::no_store(response(200, json!({"account": account}))),
            Ok(Err(SaveError::Missing)) => unknown_account(),
            _ => error(500, "write_failed", "The change could not be saved safely."),
        };
    }
    if let Some(name) = id.strip_prefix("antigravity:profile:") {
        use super::lifecycle_codex::RemoveError;
        let name = name.to_owned();
        let label = label.clone();
        let native = native.clone();
        let root = root.clone();
        let saved = tokio::task::spawn_blocking(move || {
            let account = super::lifecycle_antigravity::rename(&root, &name, label)?;
            native.changed();
            Ok::<_, RemoveError>(account)
        })
        .await;
        return match saved {
            Ok(Ok(account)) => super::no_store(response(200, json!({"account": account}))),
            Ok(Err(RemoveError::Missing)) => unknown_account(),
            Ok(Err(RemoveError::Busy)) => error(
                409,
                "activation_running",
                "Another change to these saved logins is running. Wait for it to finish.",
            ),
            _ => error(500, "write_failed", "The change could not be saved safely."),
        };
    }
    if !sources::is_additional_account_id(id) {
        let id = id.to_owned();
        let known = tokio::task::spawn_blocking(move || recheck::other_account_exists(&root, &id))
            .await
            .unwrap_or_default();
        return if known {
            error(
                409,
                "not_implemented",
                "This action is not available on this server yet.",
            )
        } else {
            unknown_account()
        };
    }
    match producer::recheck::lookup(root.clone(), id).await {
        Ok(Some(_)) => {}
        Ok(None) => return unknown_account(),
        Err(_) => {
            return error(
                500,
                "registry_unavailable",
                "Account list could not be read safely.",
            );
        }
    }
    let native = native.clone();
    let id = id.to_owned();
    let saved = tokio::task::spawn_blocking(move || {
        let source = commit(&root, &native.owners, &id, &change)?;
        native.changed();
        account_view(&root, &source, secure).ok_or(SaveError::Write)
    })
    .await;
    match saved {
        Ok(Ok(account)) => super::no_store(response(200, json!({"account":account}))),
        Ok(Err(SaveError::Missing)) => unknown_account(),
        _ => error(500, "write_failed", "The change could not be saved safely."),
    }
}

/// The name as every kind stores it: trimmed like the Accounts field, then the shared label
/// rule. A name the Codex registry cannot carry exactly (a non-breaking space, U+FFFE/U+FFFF)
/// is refused for every kind, before anything is written, so one name never renames some
/// kinds and fails on others.
fn route_label(label: &str) -> Result<String, Reply> {
    let label = label.trim_matches(crate::claude_account_store::is_js_space);
    if !sources::valid_label(label) {
        return Err(lifecycle::invalid_action_body());
    }
    if codex_registry::scalar(label).is_none() {
        return Err(error(
            400,
            "invalid_label",
            "That name has a character that cannot be saved, such as a non-breaking space. Use ordinary spaces.",
        ));
    }
    Ok(label.to_owned())
}

/// What one PATCH changes: `label` and/or `email`, each `None` when the body leaves it out
/// and `Some(None)` when it clears it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Change {
    label: Option<Option<String>>,
    email: Option<Option<String>>,
}

fn route_change(body: &serde_json::Map<String, Value>) -> Result<Change, Reply> {
    if body.is_empty() || body.keys().any(|key| key != "label" && key != "email") {
        return Err(lifecycle::invalid_action_body());
    }
    let mut change = Change::default();
    match body.get("label") {
        None => {}
        Some(Value::Null) => change.label = Some(None),
        Some(Value::String(label)) => change.label = Some(Some(route_label(label)?)),
        Some(_) => return Err(lifecycle::invalid_action_body()),
    }
    match body.get("email") {
        None => {}
        Some(Value::Null) => change.email = Some(None),
        Some(Value::String(email)) => change.email = Some(route_email(email)?),
        Some(_) => return Err(lifecycle::invalid_action_body()),
    }
    Ok(change)
}

/// The owner's display email, trimmed like the Accounts field; empty clears it.
fn route_email(email: &str) -> Result<Option<String>, Reply> {
    let email = email.trim_matches(crate::claude_account_store::is_js_space);
    if email.is_empty() {
        return Ok(None);
    }
    if !sources::valid_owner_email(email) {
        return Err(error(
            400,
            "invalid_email",
            "That email does not look like name@example.com.",
        ));
    }
    Ok(Some(email.to_owned()))
}

fn unknown_account() -> Reply {
    error(404, "unknown_account", "That account was not found.")
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum SaveError {
    Missing,
    Write,
}

fn commit(
    root: &Path,
    owners: &Owners,
    id: &str,
    change: &Change,
) -> Result<sources::Source, SaveError> {
    let (writer, accounts) = claim_registry(root, owners)?;
    let mut registry = json!({"version":2,"accounts":accounts});
    let edits = [
        change.label.clone().map(|label| Edit::Label {
            id: id.to_owned(),
            label,
        }),
        change.email.clone().map(|email| Edit::Email {
            id: id.to_owned(),
            email,
        }),
    ];
    for edit in edits.into_iter().flatten() {
        let edited = edit_registry(registry, edit).ok_or(SaveError::Write)?;
        if edited["refusal"] == "missing" {
            return Err(SaveError::Missing);
        }
        registry = edited["registry"].clone();
    }
    let mut bytes = serde_json::to_vec_pretty(&registry).map_err(|_| SaveError::Write)?;
    bytes.push(b'\n');
    let source = sources::parse_registry(&bytes)
        .map_err(|_| SaveError::Write)?
        .into_iter()
        .find(|source| source.id == id)
        .ok_or(SaveError::Write)?;
    writer
        .replace_settings(&bytes)
        .map_err(|_| SaveError::Write)?;
    Ok(source)
}

/// One Claude profile document edit for the label route: the pending store and the
/// inventory share the `label` key (the pending row already surfaces it; the live row
/// prefers it too), and the profile id never changes.
fn claude_document(root: &Path, store: &str) -> Result<Value, SaveError> {
    let row = registry::row(store).map_err(|_| SaveError::Write)?;
    let file = root.join(row.file(None).map_err(|_| SaveError::Write)?);
    let bytes = if store == "claude-pending" {
        match read::private_store(&file, row.max_bytes) {
            Read::Absent => return Ok(json!({"version":1,"profiles":[]})),
            Read::Bytes(bytes, _) => bytes,
            _ => return Err(SaveError::Write),
        }
    } else {
        match read::read(&file, &ReadPolicy::capped(row.max_bytes)) {
            Read::Absent => return Ok(json!({"version":1,"profiles":[]})),
            Read::Bytes(bytes, _) => bytes,
            _ => return Err(SaveError::Write),
        }
    };
    crate::claude_open::js::parse(&bytes).ok_or(SaveError::Write)
}

fn claude_save(
    writer: &aac_files::OwnedStore,
    store: &str,
    document: &Value,
) -> Result<(), SaveError> {
    if store == "claude-pending" && crate::claude_open::manifest::parse_pending(document).is_none()
    {
        return Err(SaveError::Write);
    }
    if store != "claude-pending" && crate::claude_open::manifest::manifest_rows(document).is_none()
    {
        return Err(SaveError::Write);
    }
    let mut bytes = serde_json::to_vec_pretty(document).map_err(|_| SaveError::Write)?;
    bytes.push(b'\n');
    writer
        .replace_settings(&bytes)
        .map_err(|_| SaveError::Write)
}

/// Rename a Claude profile's display label in whichever store carries it.
fn commit_claude(
    root: &Path,
    owners: &Owners,
    profile: &str,
    label: Option<String>,
) -> Result<Value, SaveError> {
    let rows = registry::rows().map_err(|_| SaveError::Write)?;
    let claim = |store: &str| {
        owners
            .claim(rows, store, root, None)
            .map_err(|_| SaveError::Write)
    };
    // Hold both stores like the add flow does, so a sign-in landing between the
    // two writes cannot lose the label.
    let pending_writer = claim("claude-pending")?;
    let inventory_writer = claim("claude-desktop-profiles")?;
    let mut missing = true;
    let mut shown = profile.to_owned();
    for (store, writer) in [
        ("claude-pending", &pending_writer),
        ("claude-desktop-profiles", &inventory_writer),
    ] {
        let mut document = claude_document(root, store)?;
        let changed = document["profiles"]
            .as_array_mut()
            .map(|profiles| {
                profiles
                    .iter_mut()
                    .find(|entry| entry["id"].as_str() == Some(profile))
                    .map(|entry| {
                        entry["label"] = match &label {
                            Some(name) => json!(name),
                            None => Value::Null,
                        };
                        true
                    })
                    .unwrap_or(false)
            })
            .unwrap_or(false);
        if changed {
            missing = false;
            shown = label.clone().unwrap_or_else(|| profile.to_owned());
            claude_save(writer, store, &document)?;
        }
    }
    if missing {
        return Err(SaveError::Missing);
    }
    Ok(json!({"id": format!("claude:{profile}"), "provider": "claude", "label": shown}))
}

/// Rename a Codex profile's display label in its registry metadata; the profile
/// name (the account id) never changes.
fn commit_codex(root: &Path, name: &str, label: Option<String>) -> Result<Value, SaveError> {
    let lease = CodexLease::registry(root).map_err(|_| SaveError::Write)?;
    let mut current =
        codex_registry::read(&root.join("codex-profiles.yaml")).ok_or(SaveError::Write)?;
    if !current.has(name) {
        return Err(SaveError::Missing);
    }
    let field = match &label {
        Some(text) => Field::Str(text.clone()),
        None => Field::Null,
    };
    current
        .update(name, vec![("label".to_owned(), field)])
        .map_err(|_| SaveError::Write)?;
    let bytes = codex_registry::emit(&current).ok_or(SaveError::Write)?;
    // Write only a registry every reader accepts back: a name the YAML subset cannot carry
    // exactly would otherwise leave every Codex profile unreadable.
    if codex_registry::parse(bytes.as_bytes())
        .is_none_or(|parsed| parsed.get(name) != current.get(name))
    {
        return Err(SaveError::Write);
    }
    lease
        .writer()
        .codex_metadata(root, bytes.as_bytes())
        .map_err(|_| SaveError::Write)?;
    Ok(
        json!({"id": format!("codex:{name}"), "provider": "codex", "label": label.unwrap_or_else(|| name.to_owned())}),
    )
}

pub(super) fn claim_registry(
    root: &Path,
    owners: &Owners,
) -> Result<(aac_files::OwnedStore, Vec<sources::Source>), SaveError> {
    let rows = registry::rows().map_err(|_| SaveError::Write)?;
    let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
    let writer = loop {
        match owners.claim(rows, "account-registry", root, None) {
            Ok(writer) => break writer,
            Err(error)
                if error.cause == aac_core::Cause::Busy && std::time::Instant::now() < until =>
            {
                std::thread::sleep(std::time::Duration::from_millis(15));
            }
            Err(_) => return Err(SaveError::Write),
        }
    };
    // Re-resolve under the same registered lock the compatibility writer takes.
    let accounts = match sources::read_registry(root).map_err(|_| SaveError::Write)? {
        sources::Sources::V2(accounts) => accounts,
        sources::Sources::Legacy => {
            let (mut accounts, valid) = sources::legacy(Some(root));
            if !valid {
                return Err(SaveError::Write);
            }
            let now = super::super::storage::iso(super::super::now()).ok_or(SaveError::Write)?;
            for entry in &mut accounts {
                entry.created_by = Some("migration".into());
                entry.created_at = Some(now.clone());
            }
            accounts
        }
    };
    Ok((writer, accounts))
}

pub(super) async fn claim_registry_async(
    root: &Path,
    owners: &Owners,
) -> Result<(aac_files::OwnedStore, Vec<sources::Source>), SaveError> {
    let root = root.to_owned();
    let owners = owners.clone();
    tokio::task::spawn_blocking(move || claim_registry(&root, &owners))
        .await
        .map_err(|_| SaveError::Write)?
}

pub(super) fn save_registry(
    writer: &aac_files::OwnedStore,
    entries: &[sources::Source],
) -> Result<(), SaveError> {
    let mut bytes = serde_json::to_vec_pretty(&json!({"version":2,"accounts":entries}))
        .map_err(|_| SaveError::Write)?;
    bytes.push(b'\n');
    sources::parse_registry(&bytes).map_err(|_| SaveError::Write)?;
    writer
        .replace_settings(&bytes)
        .map_err(|_| SaveError::Write)
}

fn account_view(root: &Path, source: &sources::Source, secure: bool) -> Option<Value> {
    let key = match &source.credential {
        sources::Credential::AacKey { key_id } => local_key_info(root, source, key_id),
        _ => None,
    };
    let replace_key =
        secure && aac_providers::provider(&source.provider).is_some_and(|row| row.key_store);
    entry_view(source, key, replace_key, None)
}

pub(super) fn entry_view(
    source: &sources::Source,
    key: Option<Value>,
    replace_key: bool,
    email: Option<&str>,
) -> Option<Value> {
    let entry = serde_json::to_value(source).ok()?;
    let credential_kind = entry["credential"]["kind"].as_str()?;
    let view = aac_views::registry::compose(&json!({
        "providers":[{"id":source.provider,"capabilities":{"replaceKey":replace_key}}],
        "rows":[{"kind":"entry","row":{"email":email},"entry":entry,"credentialKind":credential_kind,
            "key":key,"museSignIn":false,"running":false}],
        "trash":[],"jobs":[]
    }))
    .ok()?;
    view["accounts"].as_array()?.first().cloned()
}

pub(super) fn local_key_info(root: &Path, source: &sources::Source, id: &str) -> Option<Value> {
    if source.platform != "ubuntu" || source.ssh_host.is_some() {
        return None;
    }
    let row = registry::row("account-keys").ok()?;
    let path = root.join(
        row.file(Some(&format!("{}-{id}.json", source.provider)))
            .ok()?,
    );
    if !path.parent().is_some_and(read::private_directory) {
        return None;
    }
    let policy = ReadPolicy::capped(row.max_bytes)
        .mode(Mode::Private)
        .ancestors(Ancestors::NoLinkBelow(root));
    let Read::Bytes(bytes, _) = read::read(&path, &policy) else {
        return None;
    };
    let key: Value = serde_json::from_slice(&bytes).ok()?;
    let secret = key["secret"].as_str()?;
    if key["version"] != 1
        || key["provider"] != source.provider
        || key["keyId"] != id
        || !(8..=512).contains(&secret.len())
        || !secret.bytes().all(|c| (0x21..=0x7e).contains(&c))
    {
        return None;
    }
    let digest = crate::antigravity_runtime::sha256_hex(secret.as_bytes());
    let fingerprint = format!("sha256:{}", &digest[..16]);
    if key
        .get("fingerprint")
        .is_some_and(|value| value != &fingerprint)
    {
        return None;
    }
    Some(
        json!({"fingerprint":fingerprint,"last4":&secret[secret.len()-4..],"storedOn":source.platform}),
    )
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::public_auth::{DashboardAccess, DashboardPrincipal};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn the_route_no_longer_takes_retired_console_wallet_ids() {
        use axum::http::Method;
        let console = "/api/accounts/plan-opencode-go-console-web-0123456789ab";
        assert_eq!(route(&Method::OPTIONS, console), None);
        assert_eq!(
            route(&Method::OPTIONS, "/api/accounts/codex%3Atwo").as_deref(),
            Some("codex:two")
        );
        assert!(!account_segment(
            "plan-opencode-go-console-mac-0123456789ab"
        ));
        assert!(account_segment("claude:work"));
    }

    #[test]
    fn label_migrates_the_legacy_registry_and_changes_no_other_entry() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let rows = registry::rows().unwrap();
        let files = rows
            .iter()
            .map(|row| {
                let owner = if row.id == "account-registry" {
                    registry::Role::RustService
                } else {
                    row.owner
                };
                (row.id.clone(), owner.word().to_owned())
            })
            .collect();
        let owners = Owners::from_boot(rows, &files).unwrap();
        let renamed = commit(
            &root,
            &owners,
            "zai:usage",
            &Change {
                label: Some(Some("旅行".into())),
                email: None,
            },
        )
        .unwrap();
        assert_eq!(renamed.label.as_deref(), Some("旅行"));
        let sources::Sources::V2(accounts) = sources::read_registry(&root).unwrap() else {
            panic!("expected migrated registry");
        };
        assert_eq!(accounts.len(), 7);
        assert!(
            accounts
                .iter()
                .all(|entry| entry.created_by.as_deref() == Some("migration"))
        );
        assert!(
            accounts
                .iter()
                .filter(|entry| entry.id != "zai:usage")
                .all(|entry| entry.label.is_none())
        );
        let account = account_view(&root, &renamed, true).unwrap();
        assert_eq!(
            account.to_string(),
            r#"{"id":"zai:usage","provider":"zai","label":"旅行","email":null,"platform":"ubuntu","credential":{"kind":"discover"},"hidden":false,"trayHidden":false,"lifecycle":{"state":"ready","jobId":null},"actions":{"signInAgain":false,"replaceKey":false,"remove":true,"open":[],"recheck":true},"removeRefusal":null}"#
        );
        let clear = Change {
            label: Some(None),
            email: None,
        };
        let guard = owners.claim(rows, "account-registry", &root, None).unwrap();
        assert_eq!(
            commit(&root, &owners, "zai:usage", &clear),
            Err(SaveError::Write)
        );
        drop(guard);
        let renamed = commit(&root, &owners, "zai:usage", &clear).unwrap();
        assert_eq!(renamed.label, None);
        assert_eq!(
            commit(&root, &owners, "zai:acct:00000001", &clear),
            Err(SaveError::Missing)
        );
    }

    fn owners() -> Owners {
        let rows = registry::rows().unwrap();
        let files = rows
            .iter()
            .map(|row| {
                let owner = if row.moves_with.as_deref() == Some("writes") {
                    registry::Role::RustService
                } else {
                    row.owner
                };
                (row.id.clone(), owner.word().to_owned())
            })
            .collect();
        Owners::from_boot(rows, &files).unwrap()
    }

    fn store_file(root: &Path, store: &str) -> std::path::PathBuf {
        let row = registry::row(store).unwrap();
        root.join(row.file(None).unwrap())
    }

    fn write_store(root: &Path, store: &str, document: &Value) {
        let file = store_file(root, store);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        std::fs::write(&file, serde_json::to_string(document).unwrap()).unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[test]
    fn claude_label_edits_the_pending_store_and_keeps_the_id() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let owners = owners();
        let launcher = json!({"launcherName":"Claude Work","launcherPath":"/Applications/Claude Work.app","profilePath":"/x/Claude-work","sshHost":"mac-1"});
        let launcher_win = launcher.clone();
        write_store(
            &root,
            "claude-pending",
            &json!({"version":1,"profiles":[{
                "id":"work","label":"Old name","mac":launcher,"windows":launcher_win,
                "createdAt":"2026-11-20T00:00:00.000Z","expectedEmail":"a@example.test"
            }]}),
        );
        let account = commit_claude(&root, &owners, "work", Some("New name".into())).unwrap();
        assert_eq!(
            account,
            json!({"id":"claude:work","provider":"claude","label":"New name"})
        );
        let doc = claude_document(&root, "claude-pending").unwrap();
        assert_eq!(doc["profiles"][0]["label"], json!("New name"));
        assert_eq!(doc["profiles"][0]["id"], json!("work"));
        assert!(
            crate::claude_open::manifest::parse_pending(&doc).is_some(),
            "the edited pending store still parses"
        );
        // Clearing the label restores the profile id as the display name.
        let account = commit_claude(&root, &owners, "work", None).unwrap();
        assert_eq!(account["label"], json!("work"));
        let doc = claude_document(&root, "claude-pending").unwrap();
        assert_eq!(doc["profiles"][0]["label"], Value::Null);
        // A profile in neither store is unknown, and writes nothing.
        assert_eq!(
            commit_claude(&root, &owners, "ghost", Some("Nope".into())),
            Err(SaveError::Missing)
        );
    }

    #[test]
    fn claude_label_edits_the_inventory_when_the_profile_is_live() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let owners = owners();
        write_store(
            &root,
            "claude-desktop-profiles",
            &json!({"version":1,"profiles":[{
                "id":"work","label":"Old name","email":"a@example.test",
                "mac":{"launcherName":"Claude Work","launcherPath":"/Applications/Claude Work.app","profilePath":"/x/Claude-work","sshHost":"mac-1","isDefault":false}
            }]}),
        );
        let account = commit_claude(&root, &owners, "work", Some("New name".into())).unwrap();
        assert_eq!(account["id"], json!("claude:work"));
        let doc = claude_document(&root, "claude-desktop-profiles").unwrap();
        assert_eq!(doc["profiles"][0]["label"], json!("New name"));
        // The pending store is absent, so nothing is written there.
        assert_eq!(
            commit_claude(&root, &owners, "ghost", Some("Nope".into())),
            Err(SaveError::Missing)
        );
    }

    #[test]
    fn codex_label_edits_the_registry_meta_and_round_trips() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let file = root.join("codex-profiles.yaml");
        let fixture = "version: '1.0'\ndefault: null\nprofiles:\n  work:\n    type: codex\n    created: '2026-11-19T00:00:00.000Z'\n    last_used: null\n    email: a@b.c\n";
        std::fs::write(&file, fixture).unwrap();
        let before = codex_registry::read(&file).unwrap();
        let account = commit_codex(&root, "work", Some("My Work".into())).unwrap();
        assert_eq!(
            account,
            json!({"id":"codex:work","provider":"codex","label":"My Work"})
        );
        let after = codex_registry::read(&file).unwrap();
        assert_eq!(after.list(), before.list());
        let meta = after.get("work").unwrap();
        let label = meta.iter().find(|(key, _)| key == "label").map(|(_, v)| v);
        assert_eq!(label, Some(&Field::Str("My Work".into())));
        // Clearing removes the display name; the profile name is unchanged.
        let account = commit_codex(&root, "work", None).unwrap();
        assert_eq!(account["label"], json!("work"));
        let cleared = codex_registry::read(&file).unwrap();
        assert!(cleared.has("work"));
        assert!(
            cleared
                .get("work")
                .unwrap()
                .iter()
                .any(|(key, v)| key == "label" && *v == Field::Null)
        );
        assert_eq!(
            commit_codex(&root, "ghost", Some("Nope".into())),
            Err(SaveError::Missing)
        );
    }

    #[test]
    fn codex_label_with_yaml_look_alikes_still_reads_back_exactly() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        let file = root.join("codex-profiles.yaml");
        let fixture = "version: '1.0'\ndefault: null\nprofiles:\n  work:\n    type: codex\n    created: '2026-11-19T00:00:00.000Z'\n    last_used: null\n";
        std::fs::write(&file, fixture).unwrap();
        for name in [
            "null",
            "yes",
            "0123",
            "1.5",
            "Work: main",
            "it's #1",
            "- dash",
            "  padded",
            "\"quoted\"",
            "Ünïcode ✓",
            "a@b.c",
            "[list]",
            "{map}",
            "~",
        ] {
            let account = commit_codex(&root, "work", Some(name.into())).unwrap();
            assert_eq!(account["label"], json!(name));
            let meta = codex_registry::read(&file)
                .unwrap()
                .get("work")
                .unwrap()
                .clone();
            assert!(
                meta.iter()
                    .any(|(key, value)| key == "label" && *value == Field::Str(name.into())),
                "{name}"
            );
        }
    }

    fn patch(
        id: &str,
        body: &str,
        who: DashboardPrincipal,
        origin: bool,
    ) -> axum::extract::Request {
        let mut request = axum::http::Request::builder()
            .method(axum::http::Method::PATCH)
            .uri(format!("/api/accounts/{}", id.replace(':', "%3A")))
            .header("content-type", "application/json")
            .header("content-length", body.len())
            .body(axum::body::Body::from(body.to_owned()))
            .unwrap();
        request.extensions_mut().insert(DashboardAccess {
            auth_enabled: true,
            principal: who,
            secure_transport: true,
            same_origin: true,
            origin_present: origin,
            local_access: true,
            session_id: Some("fixture-session".into()),
            credential_transport: "direct-loopback",
            direct_loopback: true,
            lan_proxy: false,
            client_address: None,
        });
        request
    }

    async fn answer(native: &NativeOwners, id: &str, body: &str) -> (u16, Value) {
        let browser = DashboardPrincipal::Browser {
            username: "fixture".into(),
        };
        reply_json(handle(native, id, patch(id, body, browser, true)).await).await
    }

    async fn reply_json(reply: Reply) -> (u16, Value) {
        let status = reply.status().as_u16();
        let bytes = axum::body::to_bytes(reply.into_body(), 1 << 20)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_route_renames_claude_codex_and_antigravity_and_keeps_its_checks() {
        use crate::antigravity_runtime::{self as runtime, LiveIdentity};
        use crate::public_auth::{DashboardEvents, HubCache};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        // Claude: a pending profile.
        let launcher = json!({"launcherName":"Claude Work","launcherPath":"/Applications/Claude Work.app","profilePath":"/x/Claude-work","sshHost":"mac-1"});
        write_store(
            &root,
            "claude-pending",
            &json!({"version":1,"profiles":[{"id":"work","label":"Old","mac":launcher,"windows":launcher,
                "createdAt":"2026-11-20T00:00:00.000Z","expectedEmail":null}]}),
        );
        // Codex: one saved profile.
        let codex = root.join("codex-profiles.yaml");
        std::fs::write(&codex, "version: '1.0'\ndefault: null\nprofiles:\n  two:\n    type: codex\n    created: '2026-11-19T00:00:00.000Z'\n    last_used: null\n").unwrap();
        // Antigravity: one saved profile with its sign-in.
        let credential = b"fixture-credential-a";
        let fingerprint = runtime::sha256_hex(credential);
        let identity = json!({"email":"alpha@example.com","subject":"sub-alpha","plan":"Google AI Pro",
            "verifiedAt":"2026-10-01T16:40:00.000Z","source":"provider-userinfo"});
        let key = LiveIdentity::from_value(&identity).unwrap().identity_key();
        let registry = json!({"version":1,"revision":1,"active":null,"transaction":null,"profiles":[{
            "id":"alpha","email":"alpha@example.com","identityKey":key,"plan":"Google AI Pro",
            "credential":{"fingerprint":fingerprint,"format":"antigravity-consumer-json","identity":identity,
                "savedAt":"2026-10-01T16:40:00.000Z"}}]});
        let place = root.join("antigravity-profiles/registry-000000000001.json");
        let saved = root.join(format!(
            "antigravity-instances/alpha/ubuntu/credential-{fingerprint}.bin"
        ));
        for (file, bytes) in [
            (&place, registry.to_string().into_bytes()),
            (&saved, credential.to_vec()),
        ] {
            let parent = file.parent().unwrap();
            std::fs::create_dir_all(parent).unwrap();
            #[cfg(unix)]
            {
                std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700)).unwrap();
                std::fs::set_permissions(
                    parent.parent().unwrap(),
                    std::fs::Permissions::from_mode(0o700),
                )
                .unwrap();
            }
            std::fs::write(file, bytes).unwrap();
            #[cfg(unix)]
            std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
        }
        let (native, _jobs) = NativeOwners::start(
            root.clone(),
            super::super::Groups {
                writes: true,
                lifecycle: true,
                ..Default::default()
            },
            HubCache::default(),
            DashboardEvents(tokio::sync::broadcast::channel(4).0),
            Default::default(),
            std::sync::Arc::new(crate::agent_transport::Agents::with_default(
                root.clone(),
                true,
            )),
        )
        .unwrap();

        let (status, body) = answer(&native, "claude:work", r#"{"label":"Work Claude"}"#).await;
        assert_eq!(
            (status, &body["account"]["label"]),
            (200, &json!("Work Claude"))
        );
        let pending = claude_document(&root, "claude-pending").unwrap();
        assert_eq!(pending["profiles"][0]["label"], "Work Claude");
        assert_eq!(pending["profiles"][0]["id"], "work");

        let (status, body) = answer(&native, "codex:two", r#"{"label":"Second account"}"#).await;
        assert_eq!((status, &body["account"]["id"]), (200, &json!("codex:two")));
        let meta = codex_registry::read(&codex)
            .unwrap()
            .get("two")
            .unwrap()
            .clone();
        assert!(
            meta.iter()
                .any(|(k, v)| k == "label" && *v == Field::Str("Second account".into()))
        );

        let (status, body) = answer(
            &native,
            "antigravity:profile:alpha",
            r#"{"label":"Work Gemini"}"#,
        )
        .await;
        assert_eq!(
            (status, &body["account"]["label"]),
            (200, &json!("Work Gemini"))
        );
        let state = runtime::read_state(&root).unwrap();
        assert_eq!(
            state.profile("alpha").unwrap().label.as_deref(),
            Some("Work Gemini")
        );
        assert_eq!(std::fs::read(&saved).unwrap(), credential);

        // Unknown accounts, bad names, and the session and origin checks still refuse.
        for id in ["claude:ghost", "codex:ghost", "antigravity:profile:ghost"] {
            let (status, body) = answer(&native, id, r#"{"label":"Nope"}"#).await;
            assert_eq!(
                (status, body["code"].as_str()),
                (404, Some("unknown_account")),
                "{id}"
            );
        }
        for body in [
            r#"{"label":""}"#,
            r#"{"label":"   "}"#,
            r#"{"label":"bad\u0007"}"#,
            r#"{"name":"x"}"#,
        ] {
            let (status, reply) = answer(&native, "codex:two", body).await;
            assert_eq!(
                (status, reply["code"].as_str()),
                (400, Some("invalid_body")),
                "{body}"
            );
        }
        let long = format!(r#"{{"label":"{}"}}"#, "x".repeat(49));
        assert_eq!(answer(&native, "codex:two", &long).await.0, 400);
        let browser = DashboardPrincipal::Browser {
            username: "fixture".into(),
        };
        let (status, reply) = reply_json(
            handle(
                &native,
                "codex:two",
                patch("codex:two", r#"{"label":"X"}"#, browser, false),
            )
            .await,
        )
        .await;
        assert_eq!(
            (status, reply["code"].as_str()),
            (403, Some("origin_required"))
        );
        let (status, reply) = reply_json(
            handle(
                &native,
                "codex:two",
                patch(
                    "codex:two",
                    r#"{"label":"X"}"#,
                    DashboardPrincipal::Anonymous,
                    true,
                ),
            )
            .await,
        )
        .await;
        assert_eq!(
            (status, reply["code"].as_str()),
            (401, Some("auth_required"))
        );
        let meta = codex_registry::read(&codex)
            .unwrap()
            .get("two")
            .unwrap()
            .clone();
        assert!(
            meta.iter()
                .any(|(k, v)| k == "label" && *v == Field::Str("Second account".into())),
            "a refused request changes nothing"
        );

        // R4: a name the Codex registry cannot carry is refused with the same clear 400 for
        // every kind, and nothing is written.
        for id in ["claude:work", "codex:two", "antigravity:profile:alpha"] {
            let (status, reply) = answer(&native, id, r#"{"label":"Work\u00a0PC"}"#).await;
            assert_eq!(
                (status, reply["code"].as_str()),
                (400, Some("invalid_label")),
                "{id}"
            );
        }
        assert_eq!(
            claude_document(&root, "claude-pending").unwrap()["profiles"][0]["label"],
            "Work Claude"
        );
        assert_eq!(
            runtime::read_state(&root)
                .unwrap()
                .profile("alpha")
                .unwrap()
                .label
                .as_deref(),
            Some("Work Gemini")
        );

        // R3: the route trims like the Accounts field, and a name of spaces only (any JS
        // space, a non-breaking one too) is refused.
        let (status, body) = answer(&native, "codex:two", r#"{"label":"  Work  "}"#).await;
        assert_eq!((status, &body["account"]["label"]), (200, &json!("Work")));
        let meta = codex_registry::read(&codex)
            .unwrap()
            .get("two")
            .unwrap()
            .clone();
        assert!(
            meta.iter()
                .any(|(k, v)| k == "label" && *v == Field::Str("Work".into()))
        );
        let (status, body) = answer(&native, "claude:work", r#"{"label":"\tWork Claude "}"#).await;
        assert_eq!(
            (status, &body["account"]["label"]),
            (200, &json!("Work Claude"))
        );
        let (status, reply) = answer(&native, "codex:two", r#"{"label":" \u00a0 "}"#).await;
        assert_eq!(
            (status, reply["code"].as_str()),
            (400, Some("invalid_body"))
        );
    }

    /// The owner's display email: stored beside the label for registry accounts only (a
    /// provider that reports the email never takes one), checked for a plain email shape,
    /// cleared by null or an empty field, and never touching the label or the account id.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_route_keeps_an_owner_email_for_registry_accounts_only() {
        use crate::public_auth::{DashboardEvents, HubCache};
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        #[cfg(unix)]
        std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        let (native, _jobs) = NativeOwners::start(
            root.clone(),
            super::super::Groups {
                writes: true,
                lifecycle: true,
                ..Default::default()
            },
            HubCache::default(),
            DashboardEvents(tokio::sync::broadcast::channel(4).0),
            Default::default(),
            std::sync::Arc::new(crate::agent_transport::Agents::with_default(
                root.clone(),
                true,
            )),
        )
        .unwrap();
        let entry = |id: &str| {
            let sources::Sources::V2(accounts) = sources::read_registry(&root).unwrap() else {
                panic!("expected a v2 registry");
            };
            accounts.into_iter().find(|source| source.id == id).unwrap()
        };
        let stored = |id: &str| {
            let bytes = std::fs::read(root.join("account-usage-accounts.json")).unwrap();
            let registry: Value = serde_json::from_slice(&bytes).unwrap();
            registry["accounts"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["id"] == id)
                .cloned()
                .unwrap()
        };

        // An email alone: saved (trimmed), the label and the id untouched.
        let (status, _) = answer(&native, "zai:usage", r#"{"email":"  me@example.com "}"#).await;
        assert_eq!(status, 200);
        let zai = entry("zai:usage");
        assert_eq!(
            (zai.email.as_deref(), zai.label.as_deref()),
            (Some("me@example.com"), None)
        );
        // A name alone keeps the email; both at once change both.
        assert_eq!(
            answer(&native, "zai:usage", r#"{"label":"Work Z"}"#)
                .await
                .0,
            200
        );
        let zai = entry("zai:usage");
        assert_eq!(
            (zai.email.as_deref(), zai.label.as_deref()),
            (Some("me@example.com"), Some("Work Z"))
        );
        assert_eq!(
            answer(
                &native,
                "cursor:usage",
                r#"{"label":"Team Cursor","email":"team@example.org"}"#
            )
            .await
            .0,
            200
        );
        let cursor = entry("cursor:usage");
        assert_eq!(
            (cursor.email.as_deref(), cursor.label.as_deref()),
            (Some("team@example.org"), Some("Team Cursor"))
        );
        // A bad shape is refused in plain words and changes nothing.
        for bad in [
            "not-an-email",
            "a@b",
            "@example.com",
            "two words@example.com",
            "a@b@example.com",
            "me@example.com\u{7}",
        ] {
            let body = json!({ "email": bad }).to_string();
            let (status, reply) = answer(&native, "zai:usage", &body).await;
            assert_eq!(
                (status, reply["code"].as_str()),
                (400, Some("invalid_email")),
                "{bad}"
            );
        }
        let too_long = json!({ "email": format!("{}@example.com", "a".repeat(250)) }).to_string();
        assert_eq!(answer(&native, "zai:usage", &too_long).await.0, 400);
        assert_eq!(entry("zai:usage").email.as_deref(), Some("me@example.com"));
        // Clearing: an empty field and null both remove the key from the file.
        assert_eq!(
            answer(&native, "zai:usage", r#"{"email":"   "}"#).await.0,
            200
        );
        assert!(stored("zai:usage").get("email").is_none());
        assert_eq!(entry("zai:usage").label.as_deref(), Some("Work Z"));
        assert_eq!(
            answer(&native, "cursor:usage", r#"{"email":null}"#).await.0,
            200
        );
        assert!(stored("cursor:usage").get("email").is_none());
        assert_eq!(entry("cursor:usage").label.as_deref(), Some("Team Cursor"));
        // Bodies with other keys or other types are refused.
        for body in [
            r#"{"email":5}"#,
            r#"{"email":"a@b.co","name":"x"}"#,
            r#"{}"#,
        ] {
            let (status, reply) = answer(&native, "zai:usage", body).await;
            assert_eq!(
                (status, reply["code"].as_str()),
                (400, Some("invalid_body")),
                "{body}"
            );
        }
        // Claude, Codex and Antigravity report their own email: refused before any lookup.
        for id in ["claude:work", "codex:two", "antigravity:profile:alpha"] {
            for body in [
                r#"{"email":"me@example.com"}"#,
                r#"{"label":"X","email":null}"#,
            ] {
                let (status, reply) = answer(&native, id, body).await;
                assert_eq!(
                    (status, reply["code"].as_str()),
                    (400, Some("email_from_provider")),
                    "{id} {body}"
                );
            }
        }
        // Unknown registry ids, the session and the origin checks still refuse.
        let (status, reply) = answer(&native, "zai:acct:0badf00d", r#"{"email":"a@b.co"}"#).await;
        assert_eq!(
            (status, reply["code"].as_str()),
            (404, Some("unknown_account"))
        );
        let browser = DashboardPrincipal::Browser {
            username: "fixture".into(),
        };
        let body = r#"{"email":"x@example.com"}"#;
        let (status, _) = reply_json(
            handle(
                &native,
                "zai:usage",
                patch("zai:usage", body, browser, false),
            )
            .await,
        )
        .await;
        assert_eq!(status, 403);
        let (status, _) = reply_json(
            handle(
                &native,
                "zai:usage",
                patch("zai:usage", body, DashboardPrincipal::Anonymous, true),
            )
            .await,
        )
        .await;
        assert_eq!(status, 401);
        assert!(stored("zai:usage").get("email").is_none());
    }
}
