//! Review and remove additional accounts using the shared one-shot approval store.
use super::{
    NativeOwners, lifecycle_antigravity as antigravity,
    lifecycle_codex::{self as codex, RemoveError},
    lifecycle_keys::{KeyStore, key_binding, key_id},
    lifecycle_label::{claim_registry, save_registry},
};
use crate::{
    account_reads::public_account_id,
    account_usage::{
        producer, provider_caches,
        sources::{self, Source},
    },
    key_lifecycle::{self, Effect, Workflow},
    public_auth::{
        self, DashboardAccess, Method, Reply, Request, error, lifecycle, response, storage,
    },
};
use aac_files::{read, registry};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};

pub(super) fn route<'a>(method: &Method, path: &'a str) -> Option<&'a str> {
    if !matches!(*method, Method::POST | Method::OPTIONS) {
        return None;
    }
    let id = path
        .strip_prefix("/api/accounts/")?
        .strip_suffix("/remove")?;
    (!id.is_empty() && !id.contains('/')).then_some(id)
}

#[derive(Clone, Copy, Debug)]
enum Refusal {
    Missing,
    Stale,
    Running,
    Failed,
}
impl Refusal {
    fn reply(self) -> Reply {
        match self {
            Self::Missing => error(404, "unknown_account", "That account was not found."),
            Self::Stale => error(
                409,
                "confirmation_stale",
                "The account changed. Review it again.",
            ),
            Self::Running => error(
                409,
                "signin_running",
                "A sign-in for this account is running.",
            ),
            Self::Failed => error(
                500,
                "remove_failed",
                "The account could not be removed safely. Nothing was changed.",
            ),
        }
    }
}

pub(super) async fn handle(native: &NativeOwners, raw_id: &str, request: Request) -> Reply {
    if request.method() == Method::OPTIONS {
        return lifecycle::options("POST", "4");
    }
    let session = request
        .extensions()
        .get::<DashboardAccess>()
        .and_then(|access| access.session_id.as_deref())
        .unwrap_or_default()
        .to_owned();
    let body = match lifecycle::mutation_body(request).await {
        Ok(body) => body,
        Err(reply) => return reply,
    };
    let offered = body
        .get("confirmationToken")
        .and_then(Value::as_str)
        .filter(|token| {
            token.len() == 43
                && token
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
        });
    if !body.is_empty()
        && (offered.is_none()
            || body
                .keys()
                .any(|key| !matches!(key.as_str(), "confirmationToken" | "confirm"))
            || body.get("confirm").is_some_and(|value| {
                value
                    .as_str()
                    .is_none_or(|s| s.encode_utf16().count() > 320)
            }))
    {
        return lifecycle::invalid_action_body();
    }
    let Some(id) = public_auth::decode_component(raw_id).filter(|id| public_account_id(&json!(id)))
    else {
        return error(400, "invalid_account", "That account id is not valid.");
    };
    if let Some(profile) = id.strip_prefix("claude:") {
        return super::lifecycle_trash::remove_claude(
            native,
            profile,
            &session,
            offered,
            body.get("confirm").and_then(Value::as_str),
        )
        .await;
    }
    let root = native.agents.root().to_owned();
    if id.starts_with("plan-opencode-go-console-") {
        return wallet(native, &id, &session, offered).await;
    }
    if let Some(profile) = id
        .strip_prefix("codex:")
        .map(SavedProfile::Codex)
        .or_else(|| {
            id.strip_prefix("antigravity:profile:")
                .map(SavedProfile::Antigravity)
        })
    {
        return remove_saved(native, &id, profile, &session, offered).await;
    }
    if !sources::is_additional_account_id(&id) {
        return missing(offered);
    }
    let source = match producer::recheck::lookup(root, &id).await {
        Ok(Some(row)) => row.source,
        Ok(None) => return missing(offered),
        Err(_) => {
            return error(
                500,
                "registry_unavailable",
                "Account list could not be read safely.",
            );
        }
    };
    if running(native, &id) {
        return Refusal::Running.reply();
    }
    let fingerprint=storage::sha(&json!({
        "id":source.id,"platform":source.platform,"sshHost":source.ssh_host,"credential":source.credential
    }).to_string());
    let (_, computer) = producer::source_host(&source.platform, source.ssh_host.as_deref());
    let keyed = format!("The stored key is deleted from {computer}.");
    let discovered = format!("Its sign-in on {computer} is not changed.");
    let effects: &[&str] = if key_id(&source).is_some() {
        &[&keyed]
    } else {
        &[
            "The account is no longer read by the dashboard.",
            &discovered,
        ]
    };
    if let Some(reply) = approve(
        native,
        Review {
            subject: &id,
            session: &session,
            fingerprint: &fingerprint,
            token: offered,
            effects,
        },
    ) {
        return reply;
    }
    let native = native.clone();
    match tokio::spawn(async move {
        native
            .queues
            .run(&format!("key:{}", source.provider), async {
                if running(&native, &source.id) {
                    return Err(Refusal::Running);
                }
                remove(&native, &source).await?;
                native.changed();
                Ok(())
            })
            .await
            .map_err(|_| Refusal::Failed)?
    })
    .await
    {
        Ok(Ok(())) => removed_reply(),
        Ok(Err(refusal)) => refusal.reply(),
        Err(_) => Refusal::Failed.reply(),
    }
}

fn missing(token: Option<&str>) -> Reply {
    if token.is_some() {
        Refusal::Stale
    } else {
        Refusal::Missing
    }
    .reply()
}

#[derive(Clone, Copy)]
enum SavedProfile<'a> {
    Codex(&'a str),
    Antigravity(&'a str),
}

async fn remove_saved(
    native: &NativeOwners,
    id: &str,
    profile: SavedProfile<'_>,
    session: &str,
    token: Option<&str>,
) -> Reply {
    let (is_codex, name, effects) = match profile {
        SavedProfile::Codex(name) => (
            true,
            name,
            [
                "The saved Codex login of this profile is deleted from Ubuntu.",
                "The live Codex login and the other profiles are not changed.",
            ],
        ),
        SavedProfile::Antigravity(name) => (
            false,
            name,
            [
                "The saved Antigravity login of this profile is deleted from Ubuntu.",
                "The live Antigravity login, its history and the other profiles are not changed.",
            ],
        ),
    };
    let name = name.to_owned();
    let (owner, reviewed_name, subject) = (native.clone(), name.clone(), id.to_owned());
    let fingerprint = match tokio::task::spawn_blocking(move || {
        if is_codex {
            codex::review(
                owner.agents.root(),
                &reviewed_name,
                running(&owner, &subject),
            )
        } else {
            antigravity::review(&owner, &reviewed_name)
        }
    })
    .await
    {
        Ok(Ok(fingerprint)) => fingerprint,
        Ok(Err(reason)) => return saved_error(reason, token.is_some()),
        Err(_) => return Refusal::Failed.reply(),
    };
    if let Some(reply) = approve(
        native,
        Review {
            subject: id,
            session,
            fingerprint: &fingerprint,
            token,
            effects: &effects,
        },
    ) {
        return reply;
    }
    if running(native, id) {
        return Refusal::Running.reply();
    }
    let owner = native.clone();
    match tokio::task::spawn_blocking(move || {
        if is_codex {
            codex::remove(owner.agents.root(), &name, &fingerprint)
        } else {
            antigravity::remove(&owner, &name, &fingerprint)
        }
    })
    .await
    {
        Ok(Ok(())) => {
            native.changed();
            removed_reply()
        }
        Ok(Err(reason)) => saved_error(reason, true),
        Err(_) => Refusal::Failed.reply(),
    }
}

fn removed_reply() -> Reply {
    super::no_store(response(
        200,
        json!({"removed":true,"trashId":null,"purgeAfter":null}),
    ))
}

async fn wallet(native: &NativeOwners, id: &str, session: &str, token: Option<&str>) -> Reply {
    if running(native, id) {
        return Refusal::Running.reply();
    }
    let owner = native.clone();
    let target = id.to_owned();
    let session = session.to_owned();
    let token = token.map(str::to_owned);
    match tokio::task::spawn_blocking(move || {
        let native = owner;
        let id = target;
        let root = native.agents.root();
        let (writer, mut entries) =
            claim_registry(root, &native.owners).map_err(|_| Refusal::Failed)?;
        let rows = registry::rows().map_err(|_| Refusal::Failed)?;
        let source = native
            .owners
            .claim(rows, "opencode-wallet-source", root, None)
            .map_err(|_| Refusal::Failed)?;
        let position = entries.iter().position(|entry| entry.id == id);
        let entry = position.and_then(|i| entries.get(i));
        let absent = if token.is_some() {
            Refusal::Stale
        } else {
            Refusal::Missing
        };
        let _pointer = native
            .owners
            .claim(rows, provider_caches::WALLET_ACCOUNT, root, None)
            .map_err(|_| Refusal::Failed)?;
        let binding = provider_caches::wallet_source(root)
            .map_err(|_| Refusal::Failed)?
            .ok_or(absent)?;
        let binding_hash = aac_core::sha256_hex(aac_views::js_json::write(&binding));
        let account = provider_caches::wallet_account(root, &binding_hash)
            .map_err(|_| Refusal::Failed)?
            .filter(|account| *account == id)
            .ok_or(absent)?;
        let policy = read::ReadPolicy::capped(4096)
            .owner(read::Owner::Mine)
            .mode(read::Mode::Private)
            .ancestors(read::Ancestors::NoLinkBelow(root));
        let read::Read::Bytes(source_bytes, _) = read::read(source.path(), &policy) else {
            return Err(Refusal::Failed);
        };
        let source_version = aac_core::sha256_hex(source_bytes);
        let fingerprint =
            storage::sha(&json!([account, binding_hash, source_version, entry]).to_string());
        if let Some(reply) = approve(
            &native,
            Review {
                subject: &id,
                session: &session,
                fingerprint: &fingerprint,
                token: token.as_deref(),
                effects: &[
                    "The console wallet is no longer read by the dashboard.",
                    "Its sign-in in the browser is not changed.",
                ],
            },
        ) {
            return Ok(reply);
        }
        if running(&native, &id) {
            return Err(Refusal::Running);
        }
        let removed = position.map(|i| entries.remove(i));
        if removed.is_some() {
            save_registry(&writer, &entries).map_err(|_| Refusal::Failed)?;
        }
        if source.remove_settings().is_err() {
            if let (Some(i), Some(entry)) = (position, removed) {
                entries.insert(i, entry);
                let _ = save_registry(&writer, &entries);
            }
            return Err(Refusal::Failed);
        }
        native.changed();
        Ok(removed_reply())
    })
    .await
    {
        Ok(Ok(reply)) => reply,
        Ok(Err(refusal)) => refusal.reply(),
        Err(_) => Refusal::Failed.reply(),
    }
}

struct Review<'a> {
    subject: &'a str,
    session: &'a str,
    fingerprint: &'a str,
    token: Option<&'a str>,
    effects: &'a [&'a str],
}

fn approve(native: &NativeOwners, review: Review<'_>) -> Option<Reply> {
    let Some(approvals) = &native.trash else {
        return Some(Refusal::Failed.reply());
    };
    if let Some(token) = review.token {
        return approvals
            .approval(
                "consume",
                "remove",
                review.subject,
                review.session,
                review.fingerprint,
                token,
            )
            .is_none_or(|receipt| receipt["consumed"] != true)
            .then(|| Refusal::Stale.reply());
    }
    let Ok(bytes) = storage::random::<32>() else {
        return Some(Refusal::Failed.reply());
    };
    let token = URL_SAFE_NO_PAD.encode(bytes);
    let expires = approvals
        .approval(
            "issue",
            "remove",
            review.subject,
            review.session,
            review.fingerprint,
            &token,
        )
        .and_then(|receipt| receipt["expiresAtMs"].as_i64())
        .and_then(storage::iso);
    Some(match expires {
        Some(expires) => super::no_store(response(
            200,
            json!({
                "confirmation":{"token":token,"expiresAt":expires,"effects":review.effects}
            }),
        )),
        None => Refusal::Failed.reply(),
    })
}

fn saved_error(reason: RemoveError, commit: bool) -> Reply {
    let (code, message) = match reason {
        RemoveError::Missing if !commit => return Refusal::Missing.reply(),
        RemoveError::Missing | RemoveError::Stale => return Refusal::Stale.reply(),
        RemoveError::Failed => return Refusal::Failed.reply(),
        RemoveError::Running => return Refusal::Running.reply(),
        RemoveError::Active => (
            "account_active",
            "This is the live login on its computer, so it cannot be removed.",
        ),
        RemoveError::Default => (
            "account_default",
            "This is the default account. Make another account the default first.",
        ),
        RemoveError::Last => ("last_account", "This is the last account of this provider."),
        RemoveError::Busy => (
            "activation_running",
            "Another change to these saved logins is running. Wait for it to finish.",
        ),
    };
    error(409, code, message)
}

pub(super) fn running(native: &NativeOwners, id: &str) -> bool {
    native.lifecycle_jobs.as_ref().is_some_and(|jobs| {
        jobs.running(false)
            .iter()
            .any(|job| job.account_id.as_deref() == Some(id))
    })
}

async fn remove(native: &NativeOwners, source: &Source) -> Result<(), Refusal> {
    let key = key_id(source);
    if key.is_some() && !KeyStore::configured(source) {
        return Err(Refusal::Failed);
    }
    let store = KeyStore { native, source };
    let root = native.agents.root();
    let removed = std::sync::Mutex::new(None);
    key_lifecycle::run(Workflow::Remove, |step| {
        let (store, removed) = (&store, &removed);
        async move {
            match step {
                Effect::Unpublish => {
                    let (writer, mut entries) =
                        claim_registry(root, &native.owners).map_err(|_| Refusal::Failed)?;
                    let i = entries
                        .iter()
                        .position(|entry| key_binding(entry, source))
                        .ok_or(Refusal::Stale)?;
                    *removed.lock().map_err(|_| Refusal::Failed)? = Some((i, entries.remove(i)));
                    save_registry(&writer, &entries).map_err(|_| Refusal::Failed)?;
                }
                Effect::Discard => {
                    if let Some(key) = key {
                        store.delete(key).await.map_err(|_| Refusal::Failed)?;
                    }
                }
                Effect::Publish => {
                    let (writer, mut entries) =
                        claim_registry(root, &native.owners).map_err(|_| Refusal::Failed)?;
                    if !entries.iter().any(|entry| entry.id == source.id) {
                        let (i, entry) = removed
                            .lock()
                            .map_err(|_| Refusal::Failed)?
                            .take()
                            .ok_or(Refusal::Failed)?;
                        entries.insert(i.min(entries.len()), entry);
                        save_registry(&writer, &entries).map_err(|_| Refusal::Failed)?;
                    }
                }
                Effect::Store | Effect::Probe => return Err(Refusal::Failed),
            }
            Ok(true)
        }
    })
    .await?;
    Ok(())
}
