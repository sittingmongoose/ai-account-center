//! Review and remove additional accounts using the shared one-shot approval store.
use super::{
    NativeOwners, lifecycle_antigravity as antigravity,
    lifecycle_codex::{self as codex, RemoveError},
    lifecycle_keys::{KeyStore, key_binding, key_id},
    lifecycle_label::{claim_registry_async, save_registry},
};
use crate::{
    account_reads::public_account_id,
    account_usage::{
        producer,
        sources::{self, Credential, Source},
    },
    key_lifecycle::{self, Effect, Workflow},
    public_auth::{
        self, DashboardAccess, Method, Reply, Request, error, lifecycle, response, storage,
    },
};
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

/// What removing this entry actually does (the review's effects). A named key's removal deletes
/// that one stored key; the OpenCode Go discover entry deletes no key, it stops reading every
/// stored key no account names (N2). Other entries keep the generic review.
fn remove_effects(source: &Source, computer: &str) -> Vec<String> {
    if key_id(source).is_some() {
        let mut effects = vec![format!("The stored key is deleted from {computer}.")];
        if source.provider == "opencode-go" {
            effects.push("Its pasted wallet session, if any, is deleted.".to_owned());
        }
        return effects;
    }
    if source.provider == "opencode-go" && matches!(source.credential, Credential::Discover) {
        return vec![
            "It stops reading every stored key no account names.".to_owned(),
            "No stored key is deleted.".to_owned(),
        ];
    }
    vec![
        "The account is no longer read by the dashboard.".to_owned(),
        format!("Its sign-in on {computer} is not changed."),
    ]
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
    let effects = remove_effects(&source, &computer);
    let effects: Vec<&str> = effects.iter().map(String::as_str).collect();
    if let Some(reply) = approve(
        native,
        Review {
            subject: &id,
            session: &session,
            fingerprint: &fingerprint,
            token: offered,
            effects: &effects,
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
    // Keep the same registry lease for the mutation and its compensation.
    let (writer, entries) = claim_registry_async(root, &native.owners)
        .await
        .map_err(|_| Refusal::Failed)?;
    let mut remaining = entries.clone();
    let position = remaining
        .iter()
        .position(|entry| key_binding(entry, source))
        .ok_or(Refusal::Stale)?;
    remaining.remove(position);
    key_lifecycle::run(Workflow::Remove, |step| {
        let (store, writer, entries, remaining) = (&store, &writer, &entries, &remaining);
        async move {
            match step {
                Effect::Unpublish => {
                    save_registry(writer, remaining).map_err(|_| Refusal::Failed)?
                }
                Effect::Discard => {
                    if let Some(key) = key {
                        store.delete(key).await.map_err(|_| Refusal::Failed)?;
                    }
                }
                Effect::Publish => save_registry(writer, entries).map_err(|_| Refusal::Failed)?,
                Effect::Store | Effect::Probe => return Err(Refusal::Failed),
            }
            Ok(true)
        }
    })
    .await?;
    // A removed account takes its pasted console session with it, only once the removal has
    // committed: a refused or compensated removal keeps the account and its session. Discover
    // entries keep the pinned rows' sessions: those belong to rows that still exist.
    if source.provider == "opencode-go" && key.is_some() {
        forget_wallet_session(native, &source.id);
    }
    Ok(())
}

/// Delete one removed account's pasted wallet session. The account is already gone, so a
/// failed delete does not turn the removal into a failure; it is logged plainly.
fn forget_wallet_session(native: &NativeOwners, account: &str) {
    use aac_files::web_session::{Surface, delete_for_account_with_owners};
    if delete_for_account_with_owners(
        &native.owners,
        native.agents.root(),
        Surface::OpencodeWallet,
        account,
    )
    .is_err()
    {
        tracing::warn!(
            account,
            "The removed account's wallet session could not be deleted"
        );
    }
    if let Some(usage) = &native.usage {
        usage
            .producer
            .clear_web_session_cache(Surface::OpencodeWallet, Some(account));
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::{NativeOwners, Refusal, remove, remove_effects};
    use crate::account_usage::sources::{self, Credential, Source};
    use aac_files::web_session::{self, PasteOptions, Surface};
    use std::path::Path;

    async fn start_native(root: &Path) -> (NativeOwners, super::super::Jobs) {
        use crate::public_auth::{DashboardEvents, HubCache};
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

    fn named_key(platform: &str, ssh_host: Option<&str>) -> Source {
        Source {
            id: "opencode-go:acct:52f3251b".to_owned(),
            provider: "opencode-go".to_owned(),
            platform: platform.to_owned(),
            ssh_host: ssh_host.map(str::to_owned),
            label: Some("Work key".to_owned()),
            email: None,
            credential: Credential::AacKey {
                key_id: "52f3251b".to_owned(),
            },
            created_at: Some("2026-11-20T00:00:00.000Z".to_owned()),
            created_by: Some("dashboard".to_owned()),
        }
    }

    fn save_wallet(native: &NativeOwners, root: &Path, account: &str) {
        let mut parsed = web_session::parse_pasted(
            Surface::OpencodeWallet,
            "auth=fixture-wallet-cookie",
            &PasteOptions {
                workspace_id: Some("wrk_fixture123".into()),
                ..Default::default()
            },
            "2026-11-26T12:00:00Z",
        )
        .unwrap();
        parsed.meta.account_id = Some(account.to_owned());
        web_session::save_with_owners(&native.owners, root, Surface::OpencodeWallet, &parsed)
            .unwrap();
    }

    fn wallet_kept(root: &Path, account: &str) -> bool {
        web_session::load_for_account(root, Surface::OpencodeWallet, account)
            .unwrap()
            .is_some()
    }

    fn listed(root: &Path, id: &str) -> bool {
        match sources::read_registry(root).unwrap() {
            sources::Sources::V2(rows) => rows.iter().any(|row| row.id == id),
            sources::Sources::Legacy => false,
        }
    }

    /// W1: a removal that does not commit (the key delete fails and the registry is put back,
    /// or the entry changed since the review) keeps the account's wallet session; a removal
    /// that commits deletes it.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_removal_keeps_the_wallet_session_and_a_committed_one_deletes_it() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().canonicalize().unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).unwrap();
        }
        let (native, jobs) = start_native(&root).await;
        let account = "opencode-go:acct:52f3251b";

        // The key lives on a Mac no agent answers for: Discard fails, Publish puts it back.
        let remote = named_key("mac", Some("fixture-mac"));
        let (writer, _) = super::claim_registry_async(&root, &native.owners)
            .await
            .unwrap();
        super::save_registry(&writer, std::slice::from_ref(&remote)).unwrap();
        drop(writer);
        save_wallet(&native, &root, account);
        assert!(matches!(
            remove(&native, &remote).await,
            Err(Refusal::Failed)
        ));
        assert!(
            listed(&root, account),
            "the compensated removal keeps the account"
        );
        assert!(wallet_kept(&root, account), "and its wallet session");

        // The entry changed since the review: the removal is refused, the session stays.
        let reviewed = named_key("ubuntu", None);
        assert!(matches!(
            remove(&native, &reviewed).await,
            Err(Refusal::Stale)
        ));
        assert!(wallet_kept(&root, account));

        // A committed removal takes the session with it.
        let (writer, _) = super::claim_registry_async(&root, &native.owners)
            .await
            .unwrap();
        super::save_registry(&writer, std::slice::from_ref(&reviewed)).unwrap();
        drop(writer);
        remove(&native, &reviewed).await.unwrap();
        assert!(!listed(&root, account));
        assert!(!wallet_kept(&root, account));
        jobs.stop().await;
    }

    fn source(provider: &str, credential: Credential) -> Source {
        Source {
            id: format!("{provider}:usage"),
            provider: provider.to_owned(),
            platform: "ubuntu".to_owned(),
            ssh_host: None,
            label: None,
            email: None,
            credential,
            created_at: None,
            created_by: None,
        }
    }

    #[test]
    fn the_discover_entry_review_stops_reading_unnamed_keys_and_deletes_none() {
        // N2: removing the OpenCode Go discover entry deletes no key; it stops reading every
        // stored key no account names.
        let discover = source("opencode-go", Credential::Discover);
        assert_eq!(
            remove_effects(&discover, "Ubuntu"),
            [
                "It stops reading every stored key no account names.",
                "No stored key is deleted.",
            ]
        );
        // A named key's own removal still deletes that one stored key.
        let named = source(
            "opencode-go",
            Credential::AacKey {
                key_id: "52f3251b".to_owned(),
            },
        );
        assert_eq!(
            remove_effects(&named, "Ubuntu"),
            [
                "The stored key is deleted from Ubuntu.",
                "Its pasted wallet session, if any, is deleted.",
            ]
        );
        // A discover entry of a provider that does not expand stored keys keeps the generic review.
        let zai = source("zai", Credential::Discover);
        assert_eq!(
            remove_effects(&zai, "Ubuntu"),
            [
                "The account is no longer read by the dashboard.",
                "Its sign-in on Ubuntu is not changed.",
            ]
        );
    }
}
