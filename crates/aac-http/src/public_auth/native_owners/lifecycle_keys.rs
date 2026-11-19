//! Add and replace credentials through the registry lease and the existing collector.
use super::{
    NativeOwners,
    lifecycle_label::{claim_registry, entry_view, local_key_info, save_registry},
    recheck,
};
use crate::{
    account_reads::public_account_id,
    account_usage::{
        producer,
        sources::{self, Credential, Source},
    },
    agent_transport::{MutationReply, Selector},
    key_lifecycle::{self, Effect, Workflow},
    public_auth::{
        self, DashboardAccess, Method, Reply, Request, error, lifecycle, now, response, storage,
    },
};
use aac_files::{OwnedStore, read, registry};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Map, Value, json};
use std::path::Path;

#[derive(Clone, Copy)]
pub(super) enum Action {
    Add,
    Replace,
}

pub(super) fn route<'a>(method: &Method, path: &'a str) -> Option<(Action, &'a str)> {
    let tail = path.strip_prefix("/api/accounts/")?;
    match method {
        &Method::POST | &Method::OPTIONS if tail == "add" => Some((Action::Add, "")),
        &Method::PUT | &Method::OPTIONS => {
            let id = tail.strip_suffix("/key")?;
            (!id.is_empty() && !id.contains('/')).then_some((Action::Replace, id))
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum Failure {
    InvalidBody,
    InvalidAccount,
    UnknownAccount,
    NotOwned,
    NotConfigured,
    NotImplemented,
    Registry,
    Write,
    KeyStore,
    Duplicate,
    Rejected,
    TooMany,
    SingleAccount,
}

impl Failure {
    fn reply(self) -> Reply {
        use Failure::*;
        match self {
            InvalidBody => lifecycle::invalid_action_body(),
            InvalidAccount => error(400, "invalid_account", "That account id is not valid."),
            UnknownAccount => error(404, "unknown_account", "That account was not found."),
            NotOwned => error(
                409,
                "not_aac_owned",
                "This account reads a key saved by another app. Add a key here instead.",
            ),
            NotConfigured => error(
                409,
                "not_configured",
                "That app has no launcher on that computer.",
            ),
            NotImplemented => error(
                409,
                "not_implemented",
                "This action is not available on this server yet.",
            ),
            Registry => error(
                500,
                "registry_unavailable",
                "Account list could not be read safely.",
            ),
            Write => error(500, "write_failed", "The change could not be saved safely."),
            KeyStore => error(
                500,
                "key_store_unavailable",
                "The key could not be stored safely.",
            ),
            Duplicate => error(
                409,
                "duplicate_key",
                "That key is already saved for another account.",
            ),
            Rejected => error(
                422,
                "key_rejected",
                "The provider rejected that key. Nothing was saved.",
            ),
            TooMany => error(
                409,
                "too_many_accounts",
                "This provider already has the most accounts allowed.",
            ),
            SingleAccount => error(
                409,
                "single_account_provider",
                "This provider supports one account in this version.",
            ),
        }
    }
}

fn secure_error() -> Reply {
    error(
        403,
        "secure_transport_required",
        "Use the secure dashboard address (HTTPS or a tunnel) for keys and sign-in codes.",
    )
}

pub(super) async fn handle(native: &NativeOwners, op: Action, id: &str, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return match op {
            Action::Add => lifecycle::options("POST", "4"),
            Action::Replace => lifecycle::options("PUT", "3"),
        };
    }
    let secure = request
        .extensions()
        .get::<DashboardAccess>()
        .is_some_and(|access| access.secure_transport);
    let body = match lifecycle::mutation_body(request).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    match op {
        Action::Add => admit_add(native, secure, body).await,
        Action::Replace => {
            if !secure {
                return secure_error();
            }
            let Some(id) =
                public_auth::decode_component(id).filter(|id| public_account_id(&json!(id)))
            else {
                return Failure::InvalidAccount.reply();
            };
            let root = native.agents.root().to_owned();
            if !sources::is_additional_account_id(&id) {
                let known =
                    tokio::task::spawn_blocking(move || recheck::other_account_exists(&root, &id))
                        .await
                        .unwrap_or_default();
                return if known {
                    Failure::NotOwned
                } else {
                    Failure::UnknownAccount
                }
                .reply();
            }
            let source = match producer::recheck::lookup(root, &id).await {
                Ok(Some(resolved))
                    if matches!(resolved.source.credential, Credential::AacKey { .. }) =>
                {
                    resolved.source
                }
                Ok(Some(_)) => return Failure::NotOwned.reply(),
                Ok(None) => return Failure::UnknownAccount.reply(),
                Err(_) => return Failure::Registry.reply(),
            };
            let Some(secret) = body
                .get("key")
                .and_then(Value::as_str)
                .filter(|key| body.len() == 1 && valid_key(key))
            else {
                return Failure::InvalidBody.reply();
            };
            if !KeyStore::configured(&source) {
                return Failure::NotConfigured.reply();
            }
            let secret = secret.to_owned();
            let native = native.clone();
            match tokio::spawn(async move {
                native
                    .queues
                    .run(&format!("key:{}", source.provider), async {
                        let receipt = replace(&native, &source, &secret).await?;
                        native.changed();
                        Ok::<_, Failure>(receipt)
                    })
                    .await
                    .map_err(|_| Failure::Write)?
            })
            .await
            {
                Ok(Ok(body)) => super::no_store(response(200, body)),
                Ok(Err(error)) => error.reply(),
                Err(_) => Failure::Write.reply(),
            }
        }
    }
}

async fn admit_add(native: &NativeOwners, secure: bool, body: Map<String, Value>) -> Reply {
    let Some(provider) = body
        .get("provider")
        .and_then(Value::as_str)
        .and_then(aac_providers::provider)
        .filter(|row| row.dashboard)
    else {
        return Failure::InvalidBody.reply();
    };
    let name = provider.id.as_str();
    if name == "antigravity" {
        return super::lifecycle_login::add_antigravity(native, &body, secure).await;
    }
    if (provider.key_store || matches!(name, "codex" | "muse")) && !secure {
        if name == "codex" {
            return super::no_store(response(
                403,
                json!({
                    "error":"Use the secure dashboard address (HTTPS or a tunnel) for keys and sign-in codes.",
                    "code":"secure_transport_required",
                    "fallback":{"kind":"terminal","host":"ubuntu","command":"codex login --device-auth"}
                }),
            ));
        }
        return secure_error();
    }
    if name == "codex" {
        return super::lifecycle_login::add_codex(native, &body).await;
    }
    if name == "claude" {
        return super::lifecycle_trash::add_claude(native, &body).await;
    }
    if provider.key_store {
        let Some(secret) = body
            .get("key")
            .and_then(Value::as_str)
            .filter(|key| valid_key(key))
        else {
            return Failure::InvalidBody.reply();
        };
        let label = match body.get("label") {
            None | Some(Value::Null) => None,
            Some(Value::String(label)) if sources::valid_label(label) => Some(label.clone()),
            _ => return Failure::InvalidBody.reply(),
        };
        if body
            .keys()
            .any(|key| !matches!(key.as_str(), "provider" | "key" | "label"))
        {
            return Failure::InvalidBody.reply();
        }
        let secret = secret.to_owned();
        let native = native.clone();
        return created(
            tokio::spawn(async move {
                native
                    .queues
                    .run(&format!("key:{name}"), async {
                        let receipt = add(&native, name, &secret, label).await?;
                        native.changed();
                        Ok(receipt)
                    })
                    .await
                    .map_err(|_| Failure::Write)?
            })
            .await,
        );
    }
    if matches!(name, "cursor" | "qwen") {
        if body.len() != 1 {
            return Failure::InvalidBody.reply();
        }
        let native = native.clone();
        return created(
            tokio::task::spawn_blocking(move || {
                let root = native.agents.root();
                let (writer, mut entries) =
                    claim_registry(root, &native.owners).map_err(|_| Failure::Write)?;
                if entries.iter().any(|entry| entry.provider == name) {
                    return Err(Failure::SingleAccount);
                }
                if entries.len() >= 64 {
                    return Err(Failure::TooMany);
                }
                let (manifest, _) = sources::legacy(Some(root));
                let mut entry = manifest
                    .into_iter()
                    .find(|entry| entry.provider == name)
                    .ok_or(Failure::Write)?;
                entry.created_at = Some(second_stamp()?);
                let mut view = entry_view(&entry, None, false, None).ok_or(Failure::Write)?;
                view["lifecycle"] = json!({"state":"pending_sign_in","jobId":null});
                entries.push(entry);
                save_registry(&writer, &entries).map_err(|_| Failure::Write)?;
                native.changed();
                Ok(json!({"account":view}))
            })
            .await,
        );
    }
    Failure::NotImplemented.reply()
}

fn created(answer: Result<Result<Value, Failure>, tokio::task::JoinError>) -> Reply {
    match answer {
        Ok(Ok(body)) => super::no_store(response(201, body)),
        Ok(Err(error)) => error.reply(),
        Err(_) => Failure::Write.reply(),
    }
}

fn valid_key(secret: &str) -> bool {
    (8..=512).contains(&secret.len()) && secret.bytes().all(|b| (0x21..=0x7e).contains(&b))
}

fn fingerprint(key: &str) -> String {
    let digest = storage::sha(key);
    format!("sha256:{}", &digest[..16])
}

fn second_stamp() -> Result<String, Failure> {
    let now = now();
    storage::iso(now - now.rem_euclid(1000))
        .map(|stamp| stamp.replace(".000Z", "Z"))
        .ok_or(Failure::Write)
}

fn random_key() -> Result<String, Failure> {
    storage::random::<4>()
        .map(|bytes| storage::hex(&bytes))
        .map_err(|_| Failure::KeyStore)
}

pub(super) fn key_id(source: &Source) -> Option<&str> {
    match &source.credential {
        Credential::AacKey { key_id } => Some(key_id),
        _ => None,
    }
}

pub(super) fn key_binding(source: &Source, reviewed: &Source) -> bool {
    source.id == reviewed.id
        && source.provider == reviewed.provider
        && source.platform == reviewed.platform
        && source.ssh_host == reviewed.ssh_host
        && source.credential == reviewed.credential
}

fn registry_entries(root: &Path) -> Result<Vec<Source>, Failure> {
    match sources::read_registry(root).map_err(|_| Failure::Registry)? {
        sources::Sources::V2(entries) => Ok(entries),
        sources::Sources::Legacy => {
            let (entries, valid) = sources::legacy(Some(root));
            valid.then_some(entries).ok_or(Failure::Registry)
        }
    }
}

fn key_receipt(secret: &str, platform: &str) -> Value {
    json!({"last4":&secret[secret.len()-4..],"fingerprint":fingerprint(secret),"storedOn":platform})
}

pub(super) struct KeyStore<'a> {
    pub(super) native: &'a NativeOwners,
    pub(super) source: &'a Source,
}

impl KeyStore<'_> {
    pub(super) fn configured(source: &Source) -> bool {
        matches!(
            (source.platform.as_str(), source.ssh_host.as_deref()),
            ("ubuntu", None) | ("mac" | "windows", Some(_))
        )
    }

    fn local_writer(&self, id: &str) -> Result<OwnedStore, Failure> {
        self.native
            .owners
            .claim(
                registry::rows().map_err(|_| Failure::KeyStore)?,
                "account-keys",
                self.native.agents.root(),
                Some(&format!("{}-{id}.json", self.source.provider)),
            )
            .map_err(|_| Failure::KeyStore)
    }

    fn duplicate(&self, secret: &str, exclude: Option<&str>) -> Result<bool, Failure> {
        if self.source.ssh_host.is_some() {
            return Ok(false); // The reference remote store publishes no fingerprint inventory.
        }
        let root = self.native.agents.root();
        let directory = root.join("account-usage/keys");
        let Ok(entries) = read::entries(&directory, 2048) else {
            return Ok(false);
        };
        let prefix = format!("{}-", self.source.provider);
        let digest = fingerprint(secret);
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(read::Listed::TooMany) => return Err(Failure::KeyStore),
                Err(read::Listed::Entry(_)) => continue,
            };
            let name = entry.file_name();
            let Some(id) = name
                .to_str()
                .and_then(|name| name.strip_prefix(&prefix))
                .and_then(|name| name.strip_suffix(".json"))
                .filter(|id| {
                    id.len() == 8
                        && id
                            .bytes()
                            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
                })
            else {
                continue;
            };
            if exclude == Some(id) {
                continue;
            }
            if local_key_info(root, self.source, id)
                .is_some_and(|value| value["fingerprint"] == digest)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    async fn put(&self, id: &str, secret: &str) -> Result<(), Failure> {
        if self.source.ssh_host.is_none() {
            let record = json!({
                "version":1,"provider":self.source.provider,"keyId":id,"secret":secret,
                "fingerprint":fingerprint(secret),"last4":&secret[secret.len()-4..],"createdAt":second_stamp()?
            });
            let mut bytes = record.to_string().into_bytes();
            bytes.push(b'\n');
            return self
                .local_writer(id)?
                .replace_settings(&bytes)
                .map_err(|_| Failure::KeyStore);
        }
        let reply = self.remote("put", id, Some(secret)).await?;
        if reply["ok"] != true
            || reply["fingerprint"] != fingerprint(secret)
            || reply["last4"] != secret[secret.len() - 4..]
            || reply.as_object().is_none_or(|body| body.len() != 3)
        {
            return Err(Failure::KeyStore);
        }
        Ok(())
    }

    pub(super) async fn delete(&self, id: &str) -> Result<(), Failure> {
        if self.source.ssh_host.is_none() {
            return self
                .local_writer(id)?
                .remove_settings()
                .map_err(|_| Failure::KeyStore);
        }
        self.remote("delete", id, None).await.map(|_| ())
    }

    async fn remote(&self, action: &str, id: &str, secret: Option<&str>) -> Result<Value, Failure> {
        let alias = self.source.ssh_host.as_deref().ok_or(Failure::KeyStore)?;
        let selector = Selector::usage(&self.source.platform, Some(alias));
        let mut args = json!({"action":action,"provider":self.source.provider,"keyId":id});
        if let Some(secret) = secret {
            args["secret"] = json!(secret);
        }
        match self
            .native
            .agents
            .call_mutation("key-store", &selector, args, 20_000)
            .await
        {
            MutationReply::Result(value) if action != "delete" => return Ok(value),
            MutationReply::Result(value)
                if value["ok"] == true && value.as_object().is_some_and(|row| row.len() == 1) =>
            {
                return Ok(Value::Null);
            }
            MutationReply::NotDispatched => {}
            MutationReply::Result(_) | MutationReply::Uncertain => return Err(Failure::KeyStore),
        }
        let argv = remote_argv(&self.source.platform, action, &self.source.provider, id);
        let result = self
            .native
            .agents
            .reference(
                selector,
                argv,
                secret.unwrap_or_default().as_bytes().to_vec(),
                60_000,
                4096,
            )
            .await
            .map_err(|_| Failure::KeyStore)?;
        if result.code != Some(0) {
            return Err(Failure::KeyStore);
        }
        if action == "delete" {
            return Ok(Value::Null);
        }
        serde_json::from_slice(&result.stdout).map_err(|_| Failure::KeyStore)
    }
}

fn remote_argv(platform: &str, action: &str, provider: &str, id: &str) -> Vec<std::ffi::OsString> {
    if platform == "windows" {
        let script = format!(
            "$ErrorActionPreference = 'Stop'; $env:PYTHONIOENCODING = 'utf-8'; $env:PYTHONUTF8 = '1'; $helper = [IO.Path]::Combine($HOME, '.ccs', 'account-usage', 'key_store.py'); $input | & python.exe $helper {action} --provider '{provider}' --key-id '{id}'; exit $LASTEXITCODE"
        );
        let bytes: Vec<_> = script.encode_utf16().flat_map(u16::to_le_bytes).collect();
        vec![
            "powershell.exe".into(),
            "-NoProfile".into(),
            "-NonInteractive".into(),
            "-EncodedCommand".into(),
            STANDARD.encode(bytes).into(),
        ]
    } else {
        let command = format!(
            r#"/usr/bin/python3 "$HOME/.ccs/account-usage/key_store.py" {action} --provider '{provider}' --key-id '{id}'"#
        );
        vec!["/bin/sh".into(), "-c".into(), command.into()]
    }
}

async fn probe(native: &NativeOwners, source: &Source) -> Value {
    if let Some(producer) = native
        .lifecycle_usage
        .as_ref()
        .and_then(|usage| usage.producer.as_ref())
    {
        return producer.probe_key(source, now()).await;
    }
    Value::Null
}

fn checked_status(reading: &Value) -> &'static str {
    match reading["status"].as_str() {
        Some("ok" | "cached") => "ok",
        _ => "unverified",
    }
}

async fn add(
    native: &NativeOwners,
    provider: &str,
    secret: &str,
    label: Option<String>,
) -> Result<Value, Failure> {
    let root = native.agents.root();
    let entries = registry_entries(root)?;
    let slot = loop {
        let slot = random_key()?;
        if !entries
            .iter()
            .any(|entry| entry.provider == provider && key_id(entry) == Some(&slot))
        {
            break slot;
        }
    };
    let source = Source {
        id: format!("{provider}:acct:{slot}"),
        provider: provider.into(),
        platform: "ubuntu".into(),
        ssh_host: None,
        label,
        credential: Credential::AacKey {
            key_id: slot.clone(),
        },
        created_at: Some(second_stamp()?),
        created_by: Some("dashboard".into()),
    };
    let store = KeyStore {
        native,
        source: &source,
    };
    if store.duplicate(secret, None)? {
        return Err(Failure::Duplicate);
    }
    let reading = std::sync::Mutex::new(Value::Null);
    let accepted = key_lifecycle::run(Workflow::Add, |step| {
        let (store, slot, source, reading) = (&store, &slot, &source, &reading);
        async move {
            match step {
                Effect::Store => store.put(slot, secret).await?,
                Effect::Publish => {
                    let (writer, mut entries) =
                        claim_registry(root, &native.owners).map_err(|_| Failure::Write)?;
                    if entries.len() >= 64
                        || entries
                            .iter()
                            .filter(|entry| entry.provider == provider)
                            .count()
                            >= 16
                    {
                        return Err(Failure::TooMany);
                    }
                    entries.push(source.clone());
                    save_registry(&writer, &entries).map_err(|_| Failure::Write)?;
                }
                Effect::Probe => {
                    let sampled = probe(native, source).await;
                    let accepted = sampled["status"] != "needs_sign_in";
                    *reading.lock().map_err(|_| Failure::Write)? = sampled;
                    return Ok(accepted);
                }
                Effect::Unpublish => {
                    let (writer, mut entries) =
                        claim_registry(root, &native.owners).map_err(|_| Failure::Write)?;
                    entries.retain(|entry| !key_binding(entry, source));
                    save_registry(&writer, &entries).map_err(|_| Failure::Write)?;
                }
                Effect::Discard => store.delete(slot).await?,
            }
            Ok(true)
        }
    })
    .await?;
    if !accepted {
        return Err(Failure::Rejected);
    }
    let reading = reading.into_inner().map_err(|_| Failure::Write)?;
    let info = key_receipt(secret, "ubuntu");
    let email = reading["email"].as_str();
    let account = entry_view(&source, Some(info), true, email).ok_or(Failure::Write)?;
    Ok(json!({"account":account,"check":checked_status(&reading)}))
}

async fn replace(native: &NativeOwners, source: &Source, secret: &str) -> Result<Value, Failure> {
    let root = native.agents.root();
    let existing = registry_entries(root)?;
    if !existing.iter().any(|entry| key_binding(entry, source)) {
        return Err(Failure::UnknownAccount);
    }
    let id = key_id(source).ok_or(Failure::NotOwned)?;
    let store = KeyStore { native, source };
    if store.duplicate(secret, Some(id))? {
        return Err(Failure::Duplicate);
    }
    let slot = loop {
        let slot = random_key()?;
        if !existing
            .iter()
            .any(|entry| entry.provider == source.provider && key_id(entry) == Some(&slot))
            && local_key_info(root, source, &slot).is_none()
        {
            break slot;
        }
    };
    let trial = Source {
        credential: Credential::AacKey {
            key_id: slot.clone(),
        },
        ..source.clone()
    };
    let reading = std::sync::Mutex::new(Value::Null);
    let accepted = key_lifecycle::run(Workflow::Replace, |step| {
        let (store, slot, trial, reading) = (&store, &slot, &trial, &reading);
        async move {
            match step {
                Effect::Store => store.put(slot, secret).await?,
                Effect::Probe => {
                    let sampled = probe(native, trial).await;
                    let accepted = sampled["status"] != "needs_sign_in";
                    *reading.lock().map_err(|_| Failure::Write)? = sampled;
                    return Ok(accepted);
                }
                Effect::Publish => {
                    let (_writer, entries) =
                        claim_registry(root, &native.owners).map_err(|_| Failure::Write)?;
                    if !entries.iter().any(|entry| key_binding(entry, source)) {
                        return Err(Failure::UnknownAccount);
                    }
                    store.put(id, secret).await?;
                }
                Effect::Discard => store.delete(slot).await?,
                Effect::Unpublish => return Err(Failure::Write),
            }
            Ok(true)
        }
    })
    .await?;
    if !accepted {
        return Err(Failure::Rejected);
    }
    let reading = reading.into_inner().map_err(|_| Failure::Write)?;
    let info = key_receipt(secret, &source.platform);
    let account = entry_view(source, Some(info), true, None).ok_or(Failure::Write)?;
    Ok(json!({"account":account,"check":checked_status(&reading)}))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::public_auth::{DashboardEvents, HubCache};

    #[tokio::test]
    async fn lifecycle_key_replacement_keeps_one_registered_credential_and_echoes_no_secret() {
        let fixture = tempfile::tempdir().unwrap();
        let root = fixture.path().canonicalize().unwrap();
        let (native, jobs) = NativeOwners::start(
            root.clone(),
            super::super::Groups {
                writes: true,
                ..Default::default()
            },
            HubCache::default(),
            DashboardEvents(tokio::sync::broadcast::channel(4).0),
            None,
            true,
        )
        .unwrap();
        let provider = "zai";
        let old = "fixture-key-before";
        let new = "fixture-key-after";
        let baseline = registry_entries(&root).unwrap();
        let added = add(&native, provider, old, Some("Work".into()))
            .await
            .unwrap();
        assert_eq!(added["check"], "unverified");
        assert!(!added.to_string().contains(old));
        let entries = registry_entries(&root).unwrap();
        assert_eq!(entries.len(), baseline.len() + 1);
        assert!(entries.iter().zip(&baseline).all(|(migrated, prior)| {
            key_binding(migrated, prior)
                && migrated.label == prior.label
                && migrated.created_by.as_deref() == Some("migration")
                && migrated.created_at.is_some()
        }));
        let source = entries.last().unwrap();
        assert_eq!(added["account"]["id"], source.id);
        assert!(matches!(
            add(&native, provider, old, None).await,
            Err(Failure::Duplicate)
        ));
        let replaced = replace(&native, source, new).await.unwrap();
        assert_eq!(replaced["account"]["id"], source.id);
        assert_eq!(
            replaced["account"]["credential"]["fingerprint"],
            fingerprint(new)
        );
        assert!(!replaced.to_string().contains(new));
        assert_eq!(registry_entries(&root).unwrap().len(), baseline.len() + 1);
        let files = std::fs::read_dir(root.join("account-usage/keys"))
            .unwrap()
            .filter_map(Result::ok)
            .filter(|file| file.path().extension().is_some_and(|ext| ext == "json"))
            .count();
        assert_eq!(files, 1);
        jobs.stop().await;
    }
}
