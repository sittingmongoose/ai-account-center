//! Registry label edits using the shared source reader, lease and registry view.
use super::{NativeOwners, recheck};
use crate::{
    account_reads::public_account_id,
    account_usage::{producer, sources},
    lifecycle_policy::{Edit, edit_registry},
    public_auth::{DashboardAccess, DashboardEvent, Reply, error, lifecycle, response},
};
use aac_files::{
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

fn account_segment(id: &str) -> bool {
    id.contains(':') || id.starts_with("plan-opencode-go-console-")
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
    let label = match body.get("label").filter(|_| body.len() == 1) {
        Some(Value::Null) => None,
        Some(Value::String(label)) if sources::valid_label(label) => Some(label.to_owned()),
        _ => return lifecycle::invalid_action_body(),
    };
    if !public_account_id(&json!(id)) {
        return error(400, "invalid_account", "That account id is not valid.");
    }

    let root = native.agents.root().to_owned();
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
        let source = commit(&root, &native.owners, &id, label)?;
        if let Some(usage) = &native.usage {
            usage.cache.request_refresh();
        }
        native.events.publish(DashboardEvent::AccountsChanged {});
        account_view(&root, &source, secure).ok_or(SaveError::Write)
    })
    .await;
    match saved {
        Ok(Ok(account)) => super::no_store(response(200, json!({"account":account}))),
        Ok(Err(SaveError::Missing)) => unknown_account(),
        _ => error(500, "write_failed", "The change could not be saved safely."),
    }
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
    label: Option<String>,
) -> Result<sources::Source, SaveError> {
    let (writer, accounts) = claim_registry(root, owners)?;
    let edited = edit_registry(
        json!({"version":2,"accounts":accounts}),
        Edit::Label {
            id: id.to_owned(),
            label,
        },
    )
    .ok_or(SaveError::Write)?;
    if edited["refusal"] == "missing" {
        return Err(SaveError::Missing);
    }
    let mut bytes = serde_json::to_vec_pretty(&edited["registry"]).map_err(|_| SaveError::Write)?;
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

pub(super) fn claim_registry(
    root: &Path,
    owners: &Owners,
) -> Result<(aac_files::OwnedStore, Vec<sources::Source>), SaveError> {
    let rows = registry::rows().map_err(|_| SaveError::Write)?;
    let writer = owners
        .claim(rows, "account-registry", root, None)
        .map_err(|_| SaveError::Write)?;
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
        let renamed = commit(&root, &owners, "zai:usage", Some("旅行".into())).unwrap();
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
        let guard = owners.claim(rows, "account-registry", &root, None).unwrap();
        assert_eq!(
            commit(&root, &owners, "zai:usage", None),
            Err(SaveError::Write)
        );
        drop(guard);
        let renamed = commit(&root, &owners, "zai:usage", None).unwrap();
        assert_eq!(renamed.label, None);
        assert_eq!(
            commit(&root, &owners, "zai:acct:00000001", None),
            Err(SaveError::Missing)
        );
    }
}
